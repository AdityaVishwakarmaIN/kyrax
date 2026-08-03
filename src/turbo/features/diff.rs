//! diff.rs — workbook diff for kyrax.
//!
//! openpyxl cannot diff workbooks; kyrax can, because our writer is
//! deterministic — identical input produces byte-identical output, so a
//! structural diff is tractable for us and for no one else. The feature is the
//! tiering:
//!
//!   LEVEL 1 — part level: compare the two zips entry-by-entry using the
//!   CRC-32 already stored in each central-directory record. Zero inflates.
//!   (`diff_parts`)
//!
//!   LEVEL 2 — sheet/cell level: only the sheet parts LEVEL 1 flagged are
//!   inflated, then diffed cell by cell. Both sides are sorted by row then
//!   column in every real file, so cells are merged with a MERGE JOIN (O(1)
//!   memory), never a HashMap of every cell on both sides.
//!
//!   LEVEL 3 — semantic (styles, defined names, merged ranges): reserved for a
//!   later phase; the tiered structure leaves the seam.
//!
//! Everywhere `a` is BEFORE and `b` is AFTER: a part or cell present only in
//! `a` was REMOVED, present only in `b` was ADDED.
//!
//! Shared-string cells (`t="s"`) compare by their stored index text, not the
//! resolved string — resolving would mean inflating sharedStrings.xml, which
//! the tiering forbids unless LEVEL 1 flags it. Inline strings compare by
//! their first `<t>` run. Malformed sheet XML returns
//! [`TurboError::Format`]; nothing here panics on hostile input.

use crate::turbo::error::{TurboError, TurboResult};
use crate::turbo::structural::{a1, find_attr, parse_rels, resolve_zip_path};
use ahash::AHashMap;

// ----------------------------------------------------------------------------
// Data model
// ----------------------------------------------------------------------------
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Removed,
    ValueChanged,
    FormulaChanged,
    TypeChanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartChange {
    pub name: String,
    pub kind: ChangeKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellChange {
    pub sheet: String,
    pub cell: String,
    pub kind: ChangeKind,
    pub before: Option<String>,
    pub after: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkbookDiff {
    pub parts: Vec<PartChange>,
    pub cells: Vec<CellChange>,
    pub identical: bool,
}

// ----------------------------------------------------------------------------
// LEVEL 1 — part diff, CRC-only, zero inflates.
// ----------------------------------------------------------------------------
pub fn diff_parts(a: &[u8], b: &[u8]) -> TurboResult<Vec<PartChange>> {
    let (entries_a, errors_a) = crate::turbo::zipmin::list_entries(a)?;
    if let Some(e) = errors_a.first() {
        return Err(TurboError::Format(format!(
            "corrupt central directory in a: {e}"
        )));
    }
    let (entries_b, errors_b) = crate::turbo::zipmin::list_entries(b)?;
    if let Some(e) = errors_b.first() {
        return Err(TurboError::Format(format!(
            "corrupt central directory in b: {e}"
        )));
    }

    let mut crc_a: AHashMap<String, u32> = AHashMap::with_capacity(entries_a.len());
    for e in &entries_a {
        crc_a.insert(e.name.clone(), e.crc32);
    }
    let mut crc_b: AHashMap<String, u32> = AHashMap::with_capacity(entries_b.len());
    for e in &entries_b {
        crc_b.insert(e.name.clone(), e.crc32);
    }

    let mut parts: Vec<PartChange> = Vec::new();
    for (name, crc) in &crc_a {
        match crc_b.get(name) {
            Some(&bc) if bc == *crc => {}
            Some(_) => parts.push(PartChange {
                name: name.clone(),
                kind: ChangeKind::ValueChanged,
            }),
            None => parts.push(PartChange {
                name: name.clone(),
                kind: ChangeKind::Removed,
            }),
        }
    }
    for name in crc_b.keys() {
        if !crc_a.contains_key(name) {
            parts.push(PartChange {
                name: name.clone(),
                kind: ChangeKind::Added,
            });
        }
    }
    parts.sort_by(|x, y| x.name.cmp(&y.name));
    Ok(parts)
}

// ----------------------------------------------------------------------------
// LEVEL 2 — cell diff over only the parts LEVEL 1 flagged.
// ----------------------------------------------------------------------------
pub fn diff_workbooks(a: &[u8], b: &[u8]) -> TurboResult<WorkbookDiff> {
    let parts = diff_parts(a, b)?;
    let mut cells: Vec<CellChange> = Vec::new();
    let mut identical = parts.is_empty();

    let changed: Vec<&PartChange> = parts.iter().filter(|p| is_sheet_part(&p.name)).collect();

    if !changed.is_empty() {
        let wb_a = crate::turbo::zipmin::read_entry(a, "xl/workbook.xml")?;
        let rels_a = crate::turbo::zipmin::read_entry(a, "xl/_rels/workbook.xml.rels")?;
        let name_map = build_sheet_name_map(wb_a.as_deref(), rels_a.as_deref());

        let mut order: Vec<usize> = (0..changed.len()).collect();
        order.sort_by(|&i, &j| changed[i].name.cmp(&changed[j].name));

        for &idx in &order {
            let pc = changed[idx];
            let sheet = name_map
                .get(&pc.name)
                .cloned()
                .unwrap_or_else(|| derive_sheet_name(&pc.name));
            let xa = crate::turbo::zipmin::read_entry(a, &pc.name)?;
            let xb = crate::turbo::zipmin::read_entry(b, &pc.name)?;
            match (xa, xb) {
                (Some(a_xml), Some(b_xml)) => {
                    let mut cs = diff_sheet_cells(&a_xml, &b_xml, &sheet)?;
                    cells.append(&mut cs);
                }
                (Some(a_xml), None) => {
                    let mut cs = diff_sheet_present(&a_xml, &sheet, ChangeKind::Removed)?;
                    cells.append(&mut cs);
                }
                (None, Some(b_xml)) => {
                    let mut cs = diff_sheet_present(&b_xml, &sheet, ChangeKind::Added)?;
                    cells.append(&mut cs);
                }
                (None, None) => {}
            }
        }
    }

    identical = identical && cells.is_empty();
    Ok(WorkbookDiff {
        parts,
        cells,
        identical,
    })
}

/// Diff two sheet-XML payloads. Both sides must carry a `<sheetData>` block
/// (self-closing is allowed); anything else malformed returns `Err`, never
/// panics. `sheet` is the display name stamped onto every change.
pub fn diff_sheet_cells(a_xml: &[u8], b_xml: &[u8], sheet: &str) -> TurboResult<Vec<CellChange>> {
    let (rs_a, re_a) = crate::turbo::scan::sheet_data_region(a_xml)?;
    let (rs_b, re_b) = crate::turbo::scan::sheet_data_region(b_xml)?;
    let a_region = &a_xml[rs_a..re_a];
    let b_region = &b_xml[rs_b..re_b];

    let mut out: Vec<CellChange> = Vec::new();
    let mut scratch_a: Vec<u8> = Vec::new();
    let mut scratch_b: Vec<u8> = Vec::new();

    let mut ia = 0usize;
    let mut ib = 0usize;
    loop {
        let ra = next_row(a_region, ia)?;
        let rb = next_row(b_region, ib)?;
        match (ra, rb) {
            (None, None) => break,
            (Some(a), Some(b)) => {
                if a.row < b.row {
                    emit_row(&a, sheet, ChangeKind::Removed, &mut out, &mut scratch_a)?;
                    ia = a.next;
                } else if b.row < a.row {
                    emit_row(&b, sheet, ChangeKind::Added, &mut out, &mut scratch_a)?;
                    ib = b.next;
                } else {
                    merge_row(&a, &b, sheet, &mut out, &mut scratch_a, &mut scratch_b)?;
                    ia = a.next;
                    ib = b.next;
                }
            }
            (Some(a), None) => {
                emit_row(&a, sheet, ChangeKind::Removed, &mut out, &mut scratch_a)?;
                ia = a.next;
            }
            (None, Some(b)) => {
                emit_row(&b, sheet, ChangeKind::Added, &mut out, &mut scratch_a)?;
                ib = b.next;
            }
        }
    }
    Ok(out)
}

// ----------------------------------------------------------------------------
// Internal: sheet part selection and name resolution.
// ----------------------------------------------------------------------------
fn is_sheet_part(name: &str) -> bool {
    name.starts_with("xl/worksheets/sheet") && name.ends_with(".xml")
}

/// Resolve `xl/worksheets/sheetN.xml` -> display name (`Sheet1`) from
/// workbook.xml + its rels. Only reached when at least one sheet part changed.
fn build_sheet_name_map(
    wb_xml: Option<&[u8]>,
    rels_xml: Option<&[u8]>,
) -> AHashMap<String, String> {
    let mut rid_to_name: AHashMap<String, String> = AHashMap::default();
    if let Some(xml) = wb_xml {
        let mut i = 0usize;
        while let Some(o) = memchr::memmem::find(&xml[i..], b"<sheet ") {
            let s = i + o;
            let gt = s + memchr::memchr(b'>', &xml[s..]).unwrap_or(xml.len() - s);
            let tag = &xml[s..gt];
            if let (Some(n), Some(r)) = (find_attr(tag, b"name"), find_attr(tag, b"r:id")) {
                rid_to_name.insert(
                    String::from_utf8_lossy(r).into_owned(),
                    String::from_utf8_lossy(n).into_owned(),
                );
            }
            i = gt + 1;
        }
    }

    let mut out: AHashMap<String, String> = AHashMap::default();
    if let Some(rels) = rels_xml {
        let rels = parse_rels(rels);
        for (rid, name) in rid_to_name {
            if let Some(rel) = rels.get(&rid) {
                out.insert(resolve_zip_path("xl/", &rel.target), name);
            }
        }
    }
    out
}

fn derive_sheet_name(part: &str) -> String {
    let stem = part
        .trim_start_matches("xl/worksheets/")
        .trim_end_matches(".xml");
    match stem.strip_prefix("sheet") {
        Some(idx) => format!("Sheet{idx}"),
        None => stem.to_string(),
    }
}

// ----------------------------------------------------------------------------
// Row / cell scanning (namespace-agnostic like the rest of turbo).
// ----------------------------------------------------------------------------
#[derive(Clone, Copy)]
struct RowInfo<'a> {
    row: u32, // 0-based absolute row number
    body: &'a [u8],
    next: usize,
}

#[derive(Clone, Copy)]
struct CellInfo<'a> {
    col: u32, // 0-based column number
    ctype: Option<&'a [u8]>,
    f: Option<&'a [u8]>, // raw formula text
    v: Option<&'a [u8]>, // raw cached value text
}

/// Advance to the next `<row>` in a sheetData region. A row whose open tag has
/// no `>` or whose `</row>` never appears is a `Format` error, never a panic.
fn next_row<'a>(xml: &'a [u8], mut i: usize) -> TurboResult<Option<RowInfo<'a>>> {
    while i < xml.len() {
        let Some(o) = memchr::memmem::find(&xml[i..], b"<row") else {
            break;
        };
        let s = i + o;
        match xml.get(s + 4).copied() {
            Some(b' ') | Some(b'>') | Some(b'/') | Some(b'\n') | Some(b'\r') | Some(b'\t') => {}
            _ => {
                i = s + 4;
                continue;
            }
        }
        let gt = s + memchr::memchr(b'>', &xml[s..])
            .ok_or_else(|| TurboError::Format("unclosed <row open tag in sheet XML".into()))?;
        let self_close = xml.get(gt.saturating_sub(1)) == Some(&b'/');
        let tag = &xml[s + 1..gt];
        let row = match find_attr(tag, b"r") {
            Some(v) => crate::turbo::decode::atoi(v)
                .map(|r| r.saturating_sub(1))
                .unwrap_or(0),
            None => 0,
        };
        let body_start = gt + 1;
        let (body, next) = if self_close {
            (&xml[body_start..body_start], body_start)
        } else {
            let rel = memchr::memmem::find(&xml[body_start..], b"</row>")
                .ok_or_else(|| TurboError::Format("unclosed </row> in sheet XML".into()))?;
            (&xml[body_start..body_start + rel], body_start + rel + 6)
        };
        return Ok(Some(RowInfo { row, body, next }));
    }
    Ok(None)
}

/// Advance the cell cursor one cell. Cells without an `r` attribute take the
/// next sequential column, matching the guaranteed row/column sort order.
fn pull<'a>(
    body: &'a [u8],
    pos: &mut usize,
    next_col: &mut u32,
) -> TurboResult<Option<CellInfo<'a>>> {
    loop {
        let Some(o) = memchr::memmem::find(&body[*pos..], b"<c") else {
            return Ok(None);
        };
        let s = *pos + o;
        match body.get(s + 2).copied() {
            Some(b' ') | Some(b'>') | Some(b'/') | Some(b'\n') | Some(b'\r') | Some(b'\t') => {}
            _ => {
                *pos = s + 2;
                continue;
            }
        }
        let gt = s + memchr::memchr(b'>', &body[s..])
            .ok_or_else(|| TurboError::Format("unclosed <c open tag in sheet XML".into()))?;
        let self_close = body.get(gt.saturating_sub(1)) == Some(&b'/');
        let tag = &body[s + 1..gt];
        let ctype = find_attr(tag, b"t");
        let col = match find_attr(tag, b"r") {
            Some(r) => cell_col(r).unwrap_or(*next_col),
            None => *next_col,
        };
        *next_col = col + 1;

        let (content, next) = if self_close {
            (b"" as &[u8], gt + 1)
        } else {
            let rel = memchr::memmem::find(&body[gt + 1..], b"</c>")
                .ok_or_else(|| TurboError::Format("unclosed </c> in sheet XML".into()))?;
            (&body[gt + 1..gt + 1 + rel], gt + 1 + rel + 4)
        };
        let f = formula_text(content);
        let v = if ctype == Some(b"inlineStr") {
            inline_text(content)
        } else {
            v_text(content)
        };
        *pos = next;
        return Ok(Some(CellInfo { col, ctype, f, v }));
    }
}

fn cell_col(r: &[u8]) -> Option<u32> {
    let mut i = 0;
    while i < r.len() && r[i].is_ascii_alphabetic() {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    crate::turbo::formula::letters_to_index(&r[..i]).map(|c| c.saturating_sub(1))
}

fn formula_text(content: &[u8]) -> Option<&[u8]> {
    let o = memchr::memmem::find(content, b"<f")?;
    match content.get(o + 2).copied() {
        Some(b' ') | Some(b'>') => {}
        _ => return None,
    }
    let s = o;
    let gt = s + memchr::memchr(b'>', &content[s..])?;
    if content.get(gt.saturating_sub(1)) == Some(&b'/') {
        return Some(b"");
    }
    let c = memchr::memmem::find(&content[gt + 1..], b"</f>")?;
    Some(&content[gt + 1..gt + 1 + c])
}

fn v_text(content: &[u8]) -> Option<&[u8]> {
    let o = memchr::memmem::find(content, b"<v>")?;
    let c = memchr::memmem::find(&content[o + 3..], b"</v>")?;
    Some(&content[o + 3..o + 3 + c])
}

fn inline_text(content: &[u8]) -> Option<&[u8]> {
    let o = memchr::memmem::find(content, b"<t>")?;
    let c = memchr::memmem::find(&content[o + 3..], b"</t>")?;
    Some(&content[o + 3..o + 3 + c])
}

// ----------------------------------------------------------------------------
// Merge joins and change emission.
// ----------------------------------------------------------------------------
fn merge_row(
    a: &RowInfo,
    b: &RowInfo,
    sheet: &str,
    out: &mut Vec<CellChange>,
    scratch_a: &mut Vec<u8>,
    scratch_b: &mut Vec<u8>,
) -> TurboResult<()> {
    let mut pa = 0usize;
    let mut nca = 0u32;
    let mut pb = 0usize;
    let mut ncb = 0u32;
    let mut ca = pull(a.body, &mut pa, &mut nca)?;
    let mut cb = pull(b.body, &mut pb, &mut ncb)?;
    loop {
        match (ca, cb) {
            (None, None) => break,
            (Some(x), Some(y)) => {
                if x.col < y.col {
                    emit_cell(&x, a.row, sheet, ChangeKind::Removed, out, scratch_a);
                    ca = pull(a.body, &mut pa, &mut nca)?;
                } else if y.col < x.col {
                    emit_cell(&y, b.row, sheet, ChangeKind::Added, out, scratch_b);
                    cb = pull(b.body, &mut pb, &mut ncb)?;
                } else {
                    classify_cell(&x, &y, a.row, sheet, out, scratch_a, scratch_b);
                    ca = pull(a.body, &mut pa, &mut nca)?;
                    cb = pull(b.body, &mut pb, &mut ncb)?;
                }
            }
            (Some(x), None) => {
                emit_cell(&x, a.row, sheet, ChangeKind::Removed, out, scratch_a);
                ca = pull(a.body, &mut pa, &mut nca)?;
            }
            (None, Some(y)) => {
                emit_cell(&y, b.row, sheet, ChangeKind::Added, out, scratch_b);
                cb = pull(b.body, &mut pb, &mut ncb)?;
            }
        }
    }
    Ok(())
}

fn emit_row(
    ri: &RowInfo,
    sheet: &str,
    kind: ChangeKind,
    out: &mut Vec<CellChange>,
    scratch: &mut Vec<u8>,
) -> TurboResult<()> {
    let mut pos = 0usize;
    let mut nc = 0u32;
    loop {
        let Some(c) = pull(ri.body, &mut pos, &mut nc)? else {
            break;
        };
        let cell = a1(ri.row, c.col);
        let (before, after) = match kind {
            ChangeKind::Added => (None, owned(c.v, scratch)),
            ChangeKind::Removed => (owned(c.v, scratch), None),
            _ => (None, None),
        };
        out.push(CellChange {
            sheet: sheet.to_string(),
            cell,
            kind,
            before,
            after,
        });
    }
    Ok(())
}

fn emit_cell(
    c: &CellInfo,
    row: u32,
    sheet: &str,
    kind: ChangeKind,
    out: &mut Vec<CellChange>,
    scratch: &mut Vec<u8>,
) {
    let cell = a1(row, c.col);
    let (before, after) = match kind {
        ChangeKind::Added => (None, owned(c.v, scratch)),
        ChangeKind::Removed => (owned(c.v, scratch), None),
        _ => (None, None),
    };
    out.push(CellChange {
        sheet: sheet.to_string(),
        cell,
        kind,
        before,
        after,
    });
}

fn classify_cell(
    a: &CellInfo,
    b: &CellInfo,
    row: u32,
    sheet: &str,
    out: &mut Vec<CellChange>,
    scratch_a: &mut Vec<u8>,
    scratch_b: &mut Vec<u8>,
) {
    let cell = a1(row, a.col);
    if !same_decoded(a.f, b.f, scratch_a, scratch_b) {
        out.push(CellChange {
            sheet: sheet.to_string(),
            cell,
            kind: ChangeKind::FormulaChanged,
            before: owned(a.f, scratch_a),
            after: owned(b.f, scratch_b),
        });
        return;
    }
    if !same_decoded(a.v, b.v, scratch_a, scratch_b) {
        out.push(CellChange {
            sheet: sheet.to_string(),
            cell,
            kind: ChangeKind::ValueChanged,
            before: owned(a.v, scratch_a),
            after: owned(b.v, scratch_b),
        });
        return;
    }
    if a.ctype != b.ctype {
        out.push(CellChange {
            sheet: sheet.to_string(),
            cell,
            kind: ChangeKind::TypeChanged,
            before: a.ctype.map(|v| String::from_utf8_lossy(v).into_owned()),
            after: b.ctype.map(|v| String::from_utf8_lossy(v).into_owned()),
        });
    }
}

/// Compare two optional raw byte regions after XML entity decoding.
fn same_decoded(
    x: Option<&[u8]>,
    y: Option<&[u8]>,
    scratch_a: &mut Vec<u8>,
    scratch_b: &mut Vec<u8>,
) -> bool {
    match (x, y) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            let da = crate::turbo::decode::decode_bytes(a, scratch_a);
            let db = crate::turbo::decode::decode_bytes(b, scratch_b);
            da == db
        }
        _ => false,
    }
}

/// Owned, entity-decoded copy of a value slice (built only when emitting).
fn owned(raw: Option<&[u8]>, scratch: &mut Vec<u8>) -> Option<String> {
    raw.map(|r| {
        let d = crate::turbo::decode::decode_bytes(r, scratch);
        String::from_utf8_lossy(d).into_owned()
    })
}

/// Walk one whole sheet (a side whose sheet part exists on only one archive).
fn diff_sheet_present(xml: &[u8], sheet: &str, kind: ChangeKind) -> TurboResult<Vec<CellChange>> {
    let (rs, re) = crate::turbo::scan::sheet_data_region(xml)?;
    let region = &xml[rs..re];
    let mut out: Vec<CellChange> = Vec::new();
    let mut scratch: Vec<u8> = Vec::new();
    let mut i = 0usize;
    loop {
        let Some(ri) = next_row(region, i)? else {
            break;
        };
        emit_row(&ri, sheet, kind, &mut out, &mut scratch)?;
        i = ri.next;
    }
    Ok(out)
}

// ----------------------------------------------------------------------------
// Tests
// ----------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    // ---- minimal STORE zip writer (no dependency, no files on disk) ----
    fn crc32(data: &[u8]) -> u32 {
        let mut table = [0u32; 256];
        for (i, t) in table.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB88320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *t = c;
        }
        let mut c = 0xFFFF_FFFFu32;
        for &b in data {
            c = table[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
        }
        c ^ 0xFFFF_FFFF
    }

    fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, payload) in entries {
            let nameb = name.as_bytes();
            let crc = crc32(payload);
            let lh_off = out.len() as u32;
            out.extend_from_slice(b"PK\x03\x04");
            out.extend_from_slice(&20u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&crc.to_le_bytes());
            out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            out.extend_from_slice(&(nameb.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(nameb);
            out.extend_from_slice(payload);

            central.extend_from_slice(b"PK\x01\x02");
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&20u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&crc.to_le_bytes());
            central.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            central.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            central.extend_from_slice(&(nameb.len() as u16).to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u16.to_le_bytes());
            central.extend_from_slice(&0u32.to_le_bytes());
            central.extend_from_slice(&lh_off.to_le_bytes());
            central.extend_from_slice(nameb);
        }
        let cd_off = out.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(b"PK\x05\x06");
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd_off.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    // ---- fixtures ----
    const WORKBOOK_XML: &[u8] = br#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#;
    const WORKBOOK_RELS: &[u8] = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#;

    const SHEET1_A: &[u8] = br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>hello</t></is></c><c r="B1"><v>42</v></c></row><row r="2"><c r="A2"><v>3.14</v></c></row></sheetData></worksheet>"#;
    const SHEET1_B: &[u8] = br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>hello</t></is></c><c r="B1"><v>43</v></c></row><row r="2"><c r="A2"><v>3.14</v></c></row></sheetData></worksheet>"#;
    const SHEET_ROWS_AB: &[u8] = br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>hello</t></is></c><c r="B1"><v>42</v></c></row><row r="3"><c r="A3" t="inlineStr"><is><t>new</t></is></c></row></sheetData></worksheet>"#;
    const FORMULA_A: &[u8] = br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><f>A2*2</f><v>8</v></c></row></sheetData></worksheet>"#;
    const FORMULA_B: &[u8] = br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1"><f>A2*3</f><v>12</v></c></row></sheetData></worksheet>"#;

    fn wb(sheet1: &[u8]) -> Vec<u8> {
        build_zip(&[
            ("xl/workbook.xml", WORKBOOK_XML),
            ("xl/_rels/workbook.xml.rels", WORKBOOK_RELS),
            ("xl/worksheets/sheet1.xml", sheet1),
        ])
    }

    #[test]
    fn diff_identical_workbooks() {
        let bytes = wb(SHEET1_A);
        let d = diff_workbooks(&bytes, &bytes).unwrap();
        assert!(d.identical);
        assert!(d.parts.is_empty());
        assert!(d.cells.is_empty());
    }

    #[test]
    fn diff_parts_identical_is_empty() {
        let a = wb(SHEET1_A);
        assert!(diff_parts(&a, &a).unwrap().is_empty());
    }

    #[test]
    fn diff_parts_added_removed_changed() {
        let a = build_zip(&[("a.txt", b"aaa"), ("b.txt", b"old")]);
        let b = build_zip(&[("b.txt", b"new"), ("c.txt", b"ccc")]);
        let parts = diff_parts(&a, &b).unwrap();
        assert_eq!(
            parts,
            vec![
                PartChange {
                    name: "a.txt".into(),
                    kind: ChangeKind::Removed
                },
                PartChange {
                    name: "b.txt".into(),
                    kind: ChangeKind::ValueChanged
                },
                PartChange {
                    name: "c.txt".into(),
                    kind: ChangeKind::Added
                },
            ]
        );
    }

    #[test]
    fn diff_one_cell_change() {
        let d = diff_workbooks(&wb(SHEET1_A), &wb(SHEET1_B)).unwrap();
        assert!(!d.identical);
        assert_eq!(d.cells.len(), 1);
        let c = &d.cells[0];
        assert_eq!(c.sheet, "Sheet1");
        assert_eq!(c.cell, "B1");
        assert_eq!(c.kind, ChangeKind::ValueChanged);
        assert_eq!(c.before.as_deref(), Some("42"));
        assert_eq!(c.after.as_deref(), Some("43"));
        assert!(d.parts.iter().any(|p| p.name == "xl/worksheets/sheet1.xml"
            && p.kind == ChangeKind::ValueChanged));
        assert!(!d.parts.iter().any(|p| p.name == "xl/workbook.xml"));
    }

    #[test]
    fn diff_added_removed_rows() {
        let d = diff_workbooks(&wb(SHEET1_A), &wb(SHEET_ROWS_AB)).unwrap();
        assert_eq!(d.cells.len(), 2);
        assert_eq!(d.cells[0].cell, "A2");
        assert_eq!(d.cells[0].kind, ChangeKind::Removed);
        assert_eq!(d.cells[0].before.as_deref(), Some("3.14"));
        assert_eq!(d.cells[0].after, None);
        assert_eq!(d.cells[1].cell, "A3");
        assert_eq!(d.cells[1].kind, ChangeKind::Added);
        assert_eq!(d.cells[1].before, None);
        assert_eq!(d.cells[1].after.as_deref(), Some("new"));
    }

    #[test]
    fn diff_formula_change() {
        let d = diff_workbooks(&wb(FORMULA_A), &wb(FORMULA_B)).unwrap();
        assert_eq!(d.cells.len(), 1);
        let c = &d.cells[0];
        assert_eq!(c.cell, "A1");
        assert_eq!(c.kind, ChangeKind::FormulaChanged);
        assert_eq!(c.before.as_deref(), Some("A2*2"));
        assert_eq!(c.after.as_deref(), Some("A2*3"));
    }

    #[test]
    fn diff_type_change() {
        let a = br#"<worksheet><sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData></worksheet>"#;
        let b = br#"<worksheet><sheetData><row r="1"><c r="A1" t="b"><v>1</v></c></row></sheetData></worksheet>"#;
        let cells = diff_sheet_cells(a, b, "S").unwrap();
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].cell, "A1");
        assert_eq!(cells[0].kind, ChangeKind::TypeChanged);
        assert_eq!(cells[0].before, None);
        assert_eq!(cells[0].after.as_deref(), Some("b"));
    }

    #[test]
    fn diff_malformed_sheet_returns_err() {
        let bad = br#"<worksheet><sheetData><row r="1"><c r="A1"><v>1</v></c></row"#;
        assert!(diff_sheet_cells(SHEET1_A, bad, "S").is_err());
        assert!(diff_workbooks(&wb(SHEET1_A), &wb(bad)).is_err());
        let no_sheetdata = b"<worksheet/>";
        assert!(diff_sheet_cells(SHEET1_A, no_sheetdata, "S").is_err());
    }

    #[test]
    fn diff_absent_sheet_parts() {
        let empty = build_zip(&[
            ("xl/workbook.xml", WORKBOOK_XML),
            ("xl/_rels/workbook.xml.rels", WORKBOOK_RELS),
        ]);
        let d = diff_workbooks(&empty, &empty).unwrap();
        assert!(d.identical);
        assert!(d.cells.is_empty());

        let d = diff_workbooks(&empty, &wb(SHEET1_A)).unwrap();
        assert!(!d.identical);
        assert!(
            d.parts
                .iter()
                .any(|p| p.name == "xl/worksheets/sheet1.xml" && p.kind == ChangeKind::Added)
        );
        assert_eq!(d.cells.len(), 3);
        assert!(
            d.cells
                .iter()
                .all(|c| c.sheet == "Sheet1" && c.kind == ChangeKind::Added)
        );
    }

    #[test]
    fn diff_sheet_identical_direct() {
        assert!(
            diff_sheet_cells(SHEET1_A, SHEET1_A, "S")
                .unwrap()
                .is_empty()
        );
    }
}
