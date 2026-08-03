//! ST-1 — randomised invariant stress harness for the mutate byte-splice.
//!
//! `turbo/mutate.rs` shifts rows/columns by splicing worksheet XML in one forward
//! pass. Example-based tests only prove the examples; this file states properties
//! that must hold for EVERY input, then hunts them with a fixed-seed PRNG so any
//! failure reproduces exactly and prints its seed, the input, and the operation.
//!
//! The reference row/column mapping below is the documented spec (never the
//! implementation's own helpers), so an implementation bug cannot make these
//! checks pass by agreeing with itself.
//!
//! Invariants (strongest first):
//!   I1 round-trip identity    insert(at,n)+delete(at,n) == input, byte for byte
//!   I2 cell conservation      insert adds/loses nothing; delete removes exactly
//!                             the cells inside the deleted band
//!   I3 grid bounds            every coordinate after any op is 1..=1048576 /
//!                             1..=16384 (rows, cells, dimension, shared refs)
//!   I4 monotonic ordering     rows strictly increasing; cells in a row strictly
//!                             increasing by column
//!   I5 well-formedness        cheap structural check: tags balance, quotes close
//!   I6 refusal is total       None writes nothing; delta==0 never refuses
//!   I7 ref shift + opacity     formula bodies equal the independent ref-shift
//!                             spec (refshift::shift_refs semantics) and inline
//!                             string text is byte-identical; the shared `ref=`
//!                             attribute is the only grid coordinate the splice
//!                             owns outside the bodies
//!
//! Findings pinned by deterministic probes:
//!   `probe_row_splice_drops_gap_rows_between_formulas` — the row splice's
//!   above-shift fast path used to drop non-formula rows between two formula
//!   rows (real data loss on a plain insert). Fixed in mutate.rs; the probe now
//!   guards the regression.
//!   `probe_stale_ref_guard_breaks_round_trip` — a shared-formula ref that
//!   cannot be shifted in-grid now makes the whole operation refuse (None), so
//!   no stale ref can reach the output. The `<dimension>` clamp is the same
//!   class of problem and still trips round-trip identity, so that half of the
//!   probe is expected RED until the clamp is changed to refuse too.
//! The random I1/I2/I7 row streams are green; any future violation prints its
//! seed on failure.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt::Write as _;

use kyrax::turbo::mutate::{delete_cols, delete_rows, insert_cols, insert_rows};

const MAX_ROW: u32 = 1_048_576;
const MAX_COL: u32 = 16_384;

// ---------------------------------------------------------------------------
// PRNG — xorshift64, fixed seed. The seed is part of every failure report.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: u64) -> u32 {
        (self.next() % n) as u32
    }
    fn chance(&mut self, pct: u32) -> bool {
        self.below(100) < pct
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len() as u64) as usize]
    }
    fn pick_u32(&mut self, xs: &[u32]) -> u32 {
        xs[self.below(xs.len() as u64) as usize]
    }
}

// ---------------------------------------------------------------------------
// Reference coordinate mapping — the independent spec.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Axis {
    Row,
    Col,
}

impl Axis {
    fn name(&self) -> &'static str {
        match self {
            Axis::Row => "rows",
            Axis::Col => "cols",
        }
    }
}

fn col_letters_str(c: u32, out: &mut Vec<u8>) {
    let mut tmp = [0u8; 4];
    let mut n = 0;
    let mut c = c;
    while c > 0 {
        let m = (c - 1) % 26;
        tmp[n] = (b'A' + m as u8) as u8;
        c = (c - 1) / 26;
        n += 1;
    }
    for i in (0..n).rev() {
        out.push(tmp[i]);
    }
}

fn letters_to_col(s: &[u8]) -> Option<u32> {
    let mut idx = 0u32;
    for &b in s {
        if !b.is_ascii_alphabetic() {
            return None;
        }
        let v = (b.to_ascii_uppercase() - b'A' + 1) as u32;
        idx = idx.checked_mul(26)?.checked_add(v)?;
    }
    if idx == 0 || idx > MAX_COL {
        None
    } else {
        Some(idx)
    }
}

/// `"B12"` -> `(row 12, col 2)`.
fn parse_cell_ref(v: &[u8]) -> Option<(u32, u32)> {
    let mut e = v.len();
    while e > 0 && v[e - 1].is_ascii_digit() {
        e -= 1;
    }
    if e == 0 {
        return None;
    }
    let col = letters_to_col(&v[..e])?;
    let row: u32 = std::str::from_utf8(&v[e..]).ok()?.parse().ok()?;
    Some((row, col))
}

/// `"B2:D4"` -> normalized (r1,c1,r2,c2) rectangle.
fn parse_ref(v: &[u8]) -> Option<(u32, u32, u32, u32)> {
    let s = std::str::from_utf8(v).ok()?;
    let mut parts = s.split(':');
    let a = parts.next()?.trim();
    let b = parts.next().unwrap_or(a).trim();
    let (r1, c1) = parse_cell_ref(a.as_bytes())?;
    let (r2, c2) = parse_cell_ref(b.as_bytes())?;
    Some((r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)))
}

fn parse_u32(b: &[u8]) -> Option<u32> {
    std::str::from_utf8(b).ok()?.parse().ok()
}

// ---------------------------------------------------------------------------
// Sheet model + serializer. The generator builds a model; the serializer is the
// only source of sheet bytes, so everything is byte-exact and round-trippable.
// ---------------------------------------------------------------------------

#[derive(Clone)]
enum CellKind {
    Value(u64),
    SelfClosing,
    Formula(String, u64),
    InlineString(String),
    SharedMaster {
        si: u32,
        body: String,
        r1: u32,
        r2: u32,
        c1: u32,
        c2: u32,
    },
    SharedDep {
        si: u32,
    },
}

#[derive(Clone)]
struct CellModel {
    row: u32,
    col: u32,
    kind: CellKind,
}

#[derive(Clone)]
struct RowModel {
    idx: u32,
    self_closing: bool,
    cells: Vec<CellModel>,
}

#[derive(Clone)]
struct SheetModel {
    rows: Vec<RowModel>,
    /// (r1, c1, r2, c2); None = omit the dimension element.
    dimension: Option<(u32, u32, u32, u32)>,
    cols: Vec<(u32, u32)>,
    merge_cells: Vec<String>,
    spans: bool,
}

fn shared_ref_str(r1: u32, c1: u32, r2: u32, c2: u32) -> String {
    let mut a = Vec::new();
    col_letters_str(c1, &mut a);
    let mut b = Vec::new();
    col_letters_str(c2, &mut b);
    format!(
        "{}{}:{}{}",
        String::from_utf8(a).unwrap(),
        r1,
        String::from_utf8(b).unwrap(),
        r2
    )
}

fn serialize(m: &SheetModel) -> Vec<u8> {
    let mut out = String::from("<?xml version=\"1.0\"?><worksheet xmlns=\"x\">");
    match m.dimension {
        Some((r1, c1, r2, c2)) => {
            out.push_str("<dimension ref=\"");
            let mut buf = Vec::new();
            col_letters_str(c1, &mut buf);
            out.push_str(&String::from_utf8(buf).unwrap());
            out.push_str(&r1.to_string());
            if (r2, c2) != (r1, c1) {
                out.push(':');
                let mut buf = Vec::new();
                col_letters_str(c2, &mut buf);
                out.push_str(&String::from_utf8(buf).unwrap());
                out.push_str(&r2.to_string());
            }
            out.push_str("\"/>");
        }
        None => {}
    }
    if !m.cols.is_empty() {
        out.push_str("<cols>");
        for (a, b) in &m.cols {
            let _ = write!(out, "<col min=\"{a}\" max=\"{b}\" width=\"8.5\"/>");
        }
        out.push_str("</cols>");
    }
    if !m.merge_cells.is_empty() {
        let _ = write!(out, "<mergeCells count=\"{}\">", m.merge_cells.len());
        for mc in &m.merge_cells {
            let _ = write!(out, "<mergeCell ref=\"{mc}\"/>");
        }
        out.push_str("</mergeCells>");
    }
    out.push_str("<sheetData>");
    for row in &m.rows {
        if row.self_closing {
            let _ = write!(out, "<row r=\"{}\"/>", row.idx);
            continue;
        }
        if row.cells.is_empty() {
            let _ = write!(out, "<row r=\"{}\"></row>", row.idx);
            continue;
        }
        if m.spans {
            let minc = row.cells.iter().map(|c| c.col).min().unwrap();
            let maxc = row.cells.iter().map(|c| c.col).max().unwrap();
            let _ = write!(out, "<row r=\"{}\" spans=\"{minc}:{maxc}\">", row.idx);
        } else {
            let _ = write!(out, "<row r=\"{}\">", row.idx);
        }
        for cell in &row.cells {
            serialize_cell(&mut out, cell);
        }
        out.push_str("</row>");
    }
    out.push_str("</sheetData></worksheet>");
    out.into_bytes()
}

fn serialize_cell(out: &mut String, cell: &CellModel) {
    let mut buf = Vec::new();
    col_letters_str(cell.col, &mut buf);
    let a1 = format!("{}{}", String::from_utf8(buf).unwrap(), cell.row);
    match &cell.kind {
        CellKind::Value(v) => {
            let _ = write!(out, "<c r=\"{a1}\"><v>{v}</v></c>");
        }
        CellKind::SelfClosing => {
            let _ = write!(out, "<c r=\"{a1}\" s=\"3\"/>");
        }
        CellKind::Formula(body, v) => {
            let _ = write!(out, "<c r=\"{a1}\"><f>{body}</f><v>{v}</v></c>");
        }
        CellKind::InlineString(t) => {
            let _ = write!(out, "<c r=\"{a1}\" t=\"inlineStr\"><is><t>{t}</t></is></c>");
        }
        CellKind::SharedMaster {
            si,
            body,
            r1,
            r2,
            c1,
            c2,
        } => {
            let refstr = shared_ref_str(*r1, *c1, *r2, *c2);
            let _ = write!(
                out,
                "<c r=\"{a1}\" t=\"str\"><f t=\"shared\" ref=\"{refstr}\" si=\"{si}\">{body}</f><v>1</v></c>"
            );
        }
        CellKind::SharedDep { si } => {
            let _ = write!(
                out,
                "<c r=\"{a1}\" t=\"str\"><f t=\"shared\" si=\"{si}\"/></c>"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Scanner — independent structural view used by every invariant and by the
// shrinker. Tolerant of our own serializer's shapes only (that is the contract
// the splice must preserve).
// ---------------------------------------------------------------------------

struct ScanCell {
    row: u32,
    col: u32,
    is_shared: bool,
    is_master: bool,
    shared_ref: Option<(u32, u32, u32, u32)>,
    /// Formula body text (empty for shared dependents that only carry `si`).
    fbody: Option<String>,
    /// Inline-string `<t>` texts. These are opaque: the splice never rewrites them.
    inline: Vec<String>,
}

struct ScanSheet {
    rows: Vec<(u32, Vec<ScanCell>)>,
    dimension: Option<(u32, u32, u32, u32)>,
    cols: Vec<(u32, u32)>,
}

/// Find the byte offset of an opening `<tag` (not `</tag`), with a valid
/// boundary after the name.
fn find_tag(bytes: &[u8], tag: &[u8], from: usize) -> Option<usize> {
    let mut i = from.max(0);
    while i + 1 < bytes.len() {
        if bytes[i] == b'<' && bytes[i + 1] != b'/' {
            let rest = &bytes[i + 1..];
            if rest.starts_with(tag) {
                let e = tag.len();
                if rest.len() == e || matches!(rest[e], b' ' | b'>' | b'/') {
                    return Some(i);
                }
            }
        }
        i += 1;
    }
    None
}

fn extract_attr<'a>(tag: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    let mut search = Vec::with_capacity(name.len() + 2);
    search.extend_from_slice(name);
    search.extend_from_slice(b"=\"");
    let p = memchr::memmem::find(tag, &search)?;
    let vs = p + search.len();
    let q = memchr::memchr(b'"', &tag[vs..])?;
    Some(&tag[vs..vs + q])
}

fn gt_offset(bytes: &[u8], from: usize) -> usize {
    memchr::memchr(b'>', &bytes[from..]).unwrap_or(bytes.len() - from)
}

/// Absolute byte offset of the `>` that closes a tag starting at `from`.
fn gt_at(bytes: &[u8], from: usize) -> usize {
    from + gt_offset(bytes, from)
}

fn scan(bytes: &[u8]) -> ScanSheet {
    let mut s = ScanSheet {
        rows: Vec::new(),
        dimension: None,
        cols: Vec::new(),
    };
    if let Some(ds) = find_tag(bytes, b"dimension", 0) {
        let gt = gt_at(bytes, ds);
        if let Some(refv) = extract_attr(&bytes[ds..=gt.min(bytes.len().saturating_sub(1))], b"ref")
        {
            s.dimension = parse_ref(refv);
        }
    }
    if let Some(cs) = find_tag(bytes, b"cols", 0) {
        let gt = gt_at(bytes, cs);
        if !(gt > cs && bytes[gt - 1] == b'/') {
            let ce = gt + 1 + memchr::memmem::find(&bytes[gt + 1..], b"</cols>").unwrap_or(0);
            let inner = &bytes[gt + 1..ce];
            let mut p = 0;
            while let Some(sp) = find_tag(inner, b"col", p) {
                let st = gt_at(inner, sp);
                let tag = &inner[sp..=st];
                let mn = extract_attr(tag, b"min").and_then(parse_u32);
                let mx = extract_attr(tag, b"max").and_then(parse_u32);
                if let (Some(mn), Some(mx)) = (mn, mx) {
                    s.cols.push((mn, mx));
                }
                p = st + 1;
            }
        }
    }
    if let Some(sd) = find_tag(bytes, b"sheetData", 0) {
        let gt = gt_at(bytes, sd);
        if !(gt > sd && bytes[gt - 1] == b'/') {
            let close =
                gt + 1 + memchr::memmem::find(&bytes[gt + 1..], b"</sheetData>").unwrap_or(0);
            let body = &bytes[gt + 1..close];
            let mut p = 0;
            while let Some(rs) = find_tag(body, b"row", p) {
                let gt2 = gt_at(body, rs);
                let rtag = &body[rs..=gt2];
                let self_closing = rtag.len() > 1 && rtag[rtag.len() - 1] == b'/';
                let idx = extract_attr(rtag, b"r").and_then(parse_u32).unwrap_or(0);
                let row_end = if self_closing {
                    gt2 + 1
                } else {
                    let rel = memchr::memmem::find(&body[gt2 + 1..], b"</row>")
                        .map(|o| gt2 + 1 + o + 6)
                        .unwrap_or(body.len());
                    rel
                };
                let mut cells = Vec::new();
                if !self_closing {
                    let mut cp = gt2 + 1;
                    while let Some(cso) = find_tag(body, b"c", cp) {
                        if cso >= row_end {
                            break;
                        }
                        let cgt = gt_at(body, cso);
                        let cself = cgt > cso && body[cgt - 1] == b'/';
                        let cend = if cself {
                            cgt + 1
                        } else {
                            let rel = memchr::memmem::find(&body[cgt + 1..], b"</c>")
                                .map(|o| cgt + 1 + o + 4)
                                .unwrap_or(row_end);
                            rel.min(row_end)
                        };
                        cells.push(scan_cell(&body[cso..cend]));
                        cp = cend;
                    }
                }
                s.rows.push((idx, cells));
                p = row_end;
            }
        }
    }
    s
}

fn scan_cell(bytes: &[u8]) -> ScanCell {
    let gt = memchr::memchr(b'>', bytes).unwrap_or(bytes.len().saturating_sub(1));
    let ctag = &bytes[..=gt];
    let self_closing = gt > 0 && ctag[gt - 1] == b'/';
    let (row, col) = extract_attr(ctag, b"r")
        .and_then(parse_cell_ref)
        .unwrap_or((0, 0));
    let mut sc = ScanCell {
        row,
        col,
        is_shared: false,
        is_master: false,
        shared_ref: None,
        fbody: None,
        inline: Vec::new(),
    };
    if self_closing {
        return sc;
    }
    if let Some(fs) = find_tag(bytes, b"f", gt + 1) {
        let fgt = gt_at(bytes, fs);
        let fself = fgt > fs && bytes[fgt - 1] == b'/';
        let ftag = &bytes[fs..=fgt];
        let shared = extract_attr(ftag, b"t")
            .map(|v| v == b"shared")
            .unwrap_or(false);
        if shared {
            sc.is_shared = true;
            if let Some(r) = extract_attr(ftag, b"ref") {
                sc.is_master = true;
                sc.shared_ref = parse_ref(r);
            }
        }
        if !fself {
            let rel = memchr::memmem::find(&bytes[fgt + 1..], b"</f>").unwrap_or(0);
            sc.fbody = Some(String::from_utf8_lossy(&bytes[fgt + 1..fgt + 1 + rel]).into_owned());
        }
    }
    if let Some(is) = find_tag(bytes, b"is", gt + 1) {
        let ie = gt_at(bytes, is);
        let iclose = ie + 1 + memchr::memmem::find(&bytes[ie + 1..], b"</is>").unwrap_or(0);
        let inner = &bytes[ie + 1..iclose];
        let mut tp = 0;
        while let Some(ts) = find_tag(inner, b"t", tp) {
            let tgt = gt_at(inner, ts);
            let rel = memchr::memmem::find(&inner[tgt + 1..], b"</t>").unwrap_or(0);
            sc.inline
                .push(String::from_utf8_lossy(&inner[tgt + 1..tgt + 1 + rel]).into_owned());
            tp = tgt + 1 + rel;
        }
    }
    sc
}

// ---------------------------------------------------------------------------
// The invariants themselves.
// ---------------------------------------------------------------------------

fn check_bounds(s: &ScanSheet) -> Result<(), String> {
    if let Some((r1, c1, r2, c2)) = s.dimension {
        for (r, c) in [(r1, c1), (r2, c2)] {
            if !(1..=MAX_ROW).contains(&r) || !(1..=MAX_COL).contains(&c) {
                return Err(format!(
                    "dimension coordinate ({r},{c}) escapes the grid (rows 1..={MAX_ROW}, cols 1..={MAX_COL})"
                ));
            }
        }
    }
    for (idx, cells) in &s.rows {
        if !(1..=MAX_ROW).contains(idx) {
            return Err(format!("row r={idx} escapes the grid"));
        }
        for c in cells {
            if !(1..=MAX_ROW).contains(&c.row) || !(1..=MAX_COL).contains(&c.col) {
                return Err(format!(
                    "cell r={}{} escapes the grid (row {}, col {})",
                    letters_of(c.col),
                    c.row,
                    c.row,
                    c.col
                ));
            }
            if let Some((r1, c1, r2, c2)) = c.shared_ref {
                if !(1..=MAX_ROW).contains(&r1)
                    || !(1..=MAX_ROW).contains(&r2)
                    || !(1..=MAX_COL).contains(&c1)
                    || !(1..=MAX_COL).contains(&c2)
                {
                    return Err(format!("shared ref {r1},{c1}-{r2},{c2} escapes the grid"));
                }
            }
        }
    }
    for (a, b) in &s.cols {
        if *a == 0 || *b < *a || *b > MAX_COL {
            return Err(format!("cols span {a}..={b} escapes the grid"));
        }
    }
    Ok(())
}

fn letters_of(c: u32) -> String {
    let mut buf = Vec::new();
    col_letters_str(c, &mut buf);
    String::from_utf8(buf).unwrap()
}

fn check_ordering(s: &ScanSheet) -> Result<(), String> {
    let mut prev_row = 0u32;
    for (idx, cells) in &s.rows {
        if *idx <= prev_row {
            return Err(format!(
                "rows not strictly increasing: {prev_row} then {idx}"
            ));
        }
        prev_row = *idx;
        let mut prev_col = 0u32;
        for c in cells {
            if c.col <= prev_col {
                return Err(format!(
                    "cells not strictly increasing by column in row {idx} ({} then {})",
                    prev_col, c.col
                ));
            }
            prev_col = c.col;
        }
    }
    Ok(())
}

/// Cheap structural well-formedness: tags balance for the element kinds we emit
/// and every attribute quote closes.
fn check_wellformed(bytes: &[u8]) -> Result<(), String> {
    // One O(n) pass: attribute quotes must close, and every tracked element tag
    // must balance. Element kinds outside the tracked set (worksheet, xml PI,
    // mergeCell/dimension/col self-closings) are ignored.
    let mut q_open = false;
    for &b in bytes {
        if b == b'"' {
            q_open = !q_open;
        }
    }
    if q_open {
        return Err("unterminated attribute quote".to_string());
    }

    let mut row = 0i64;
    let mut c = 0i64;
    let mut f = 0i64;
    let mut is = 0i64;
    let mut t = 0i64;
    let mut sheet_data = 0i64;
    let mut cols = 0i64;
    let mut col = 0i64;
    let mut dim = 0i64;
    let mut merge_cell = 0i64;

    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        // `>` that closes this tag; bounds-checked so a truncated element errors.
        let rel = memchr::memchr(b'>', &bytes[i + 1..]).ok_or("truncated element: no '>'")?;
        let gt = i + 1 + rel;
        let close = i + 1 < gt && bytes[i + 1] == b'/';
        let name_start = if close { i + 2 } else { i + 1 };
        let mut name_end = name_start;
        while name_end < gt
            && bytes[name_end] != b' '
            && bytes[name_end] != b'>'
            && bytes[name_end] != b'/'
        {
            name_end += 1;
        }
        let self_close = !close && gt > i + 1 && bytes[gt - 1] == b'/';
        let name = &bytes[name_start..name_end];
        let apply = |opens: &mut i64| {
            if close {
                *opens -= 1;
                if *opens < 0 {
                    Err(format!(
                        "closing </{}> with no matching open",
                        String::from_utf8_lossy(name)
                    ))
                } else {
                    Ok(())
                }
            } else if !self_close {
                *opens += 1;
                Ok(())
            } else {
                Ok(())
            }
        };
        let r = if name == b"row" {
            apply(&mut row)
        } else if name == b"c" {
            apply(&mut c)
        } else if name == b"f" {
            apply(&mut f)
        } else if name == b"is" {
            apply(&mut is)
        } else if name == b"t" {
            apply(&mut t)
        } else if name == b"sheetData" {
            apply(&mut sheet_data)
        } else if name == b"cols" {
            apply(&mut cols)
        } else if name == b"col" {
            apply(&mut col)
        } else if name == b"dimension" {
            apply(&mut dim)
        } else if name == b"mergeCell" {
            apply(&mut merge_cell)
        } else {
            Ok(())
        };
        r?;
        i = gt + 1;
    }

    for (n, bal) in [
        ("row", row),
        ("c", c),
        ("f", f),
        ("is", is),
        ("t", t),
        ("sheetData", sheet_data),
        ("cols", cols),
        ("col", col),
        ("dimension", dim),
        ("mergeCell", merge_cell),
    ] {
        if bal != 0 {
            return Err(format!("unbalanced <{n}>: net {bal} opens"));
        }
    }
    Ok(())
}

fn cell_count(s: &ScanSheet) -> usize {
    s.rows.iter().map(|(_, cs)| cs.len()).sum()
}

fn cells_in_band(s: &ScanSheet, axis: Axis, at: u32, count: u32) -> usize {
    s.rows
        .iter()
        .flat_map(|(_, cs)| cs.iter())
        .filter(|c| match axis {
            Axis::Row => (at..at + count).contains(&c.row),
            Axis::Col => (at..at + count).contains(&c.col),
        })
        .count()
}

fn opaque_tokens(s: &ScanSheet) -> Vec<String> {
    let mut v = Vec::new();
    for (_, cells) in &s.rows {
        for c in cells {
            v.extend(c.inline.iter().cloned());
        }
    }
    v
}

/// Reverse-map an AFTER coordinate to the BEFORE coordinate that occupied the
/// same grid position (a surviving cell after `op`). `None` only on underflow.
fn reverse_shift(axis: Axis, op: Op, at: u32, count: u32, r: u32, c: u32) -> Option<(u32, u32)> {
    match (axis, op) {
        (Axis::Row, Op::Insert) => Some((if r < at { r } else { r.checked_sub(count)? }, c)),
        (Axis::Row, Op::Delete) => Some((if r < at { r } else { r.checked_add(count)? }, c)),
        (Axis::Col, Op::Insert) => Some((r, if c < at { c } else { c.checked_sub(count)? })),
        (Axis::Col, Op::Delete) => Some((r, if c < at { c } else { c.checked_add(count)? })),
    }
}

/// Independent spec of `refshift::shift_refs` (PERF_EXPERIMENTS.md E3): a single
/// byte scan that shifts cell references on one axis. String literals and quoted
/// sheet names are opaque, `$`-absolute components are pinned, and a delete that
/// pushes an index below 1 turns the reference into `#REF!`. Written from the
/// documented spec, never from the implementation.
fn ref_shift_formula(formula: &str, axis: Axis, at: u32, delta: i64) -> String {
    let f = formula.as_bytes();
    let n = f.len();
    let mut out = String::with_capacity(n);
    let mut run = 0usize;
    let mut i = 0usize;
    while i < n {
        let c = f[i];
        if c == b'"' {
            i += skip_dquote(f, i);
            continue;
        }
        if c == b'\'' {
            i += skip_squote(f, i);
            continue;
        }
        let prev_ident =
            i > 0 && (f[i - 1].is_ascii_alphanumeric() || f[i - 1] == b'_' || f[i - 1] == b'.');
        if (c == b'$' || c.is_ascii_alphabetic()) && !prev_ident {
            let start = i;
            let mut p = i;
            let abs_col = f[p] == b'$';
            if abs_col {
                p += 1;
            }
            let cs = p;
            while p < n && f[p].is_ascii_alphabetic() {
                p += 1;
            }
            let le = p; // letters end
            let abs_row = p < n && f[p] == b'$';
            if abs_row {
                p += 1;
            }
            let rs = p;
            while p < n && f[p].is_ascii_digit() {
                p += 1;
            }
            let is_ref = (1..=3).contains(&(le - cs))
                && p > rs
                && !(p < n && f[p] == b'(')
                && !(p < n && (f[p].is_ascii_alphabetic() || f[p] == b'_'));
            if is_ref {
                let repl: Option<String> = match axis {
                    Axis::Row => {
                        if abs_row {
                            None
                        } else {
                            let row: u32 = std::str::from_utf8(&f[rs..p])
                                .unwrap_or("0")
                                .parse()
                                .unwrap_or(0);
                            if row >= at {
                                let nr = row as i64 + delta;
                                if nr < 1 {
                                    Some("#REF!".to_string())
                                } else {
                                    let mut v = String::from_utf8_lossy(&f[start..rs]).into_owned();
                                    v.push_str(&nr.to_string());
                                    Some(v)
                                }
                            } else {
                                None
                            }
                        }
                    }
                    Axis::Col => {
                        if abs_col {
                            None
                        } else if let Some(idx) = letters_to_col(&f[cs..le]) {
                            if idx < at {
                                None
                            } else {
                                let ni = idx as i64 + delta;
                                if ni < 1 {
                                    Some("#REF!".to_string())
                                } else {
                                    let mut v = letters_of(ni as u32);
                                    v.push_str(&String::from_utf8_lossy(&f[le..p]));
                                    Some(v)
                                }
                            }
                        } else {
                            None
                        }
                    }
                };
                if let Some(repl) = repl {
                    out.push_str(&String::from_utf8_lossy(&f[run..start]));
                    out.push_str(&repl);
                    run = p;
                }
                i = p;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&String::from_utf8_lossy(&f[run..]));
    out
}

fn skip_dquote(f: &[u8], i: usize) -> usize {
    let mut j = i + 1;
    while j < f.len() {
        if f[j] == b'"' {
            if j + 1 < f.len() && f[j + 1] == b'"' {
                j += 2;
                continue;
            }
            return j + 1 - i;
        }
        j += 1;
    }
    f.len() - i
}

fn skip_squote(f: &[u8], i: usize) -> usize {
    let mut j = i + 1;
    while j < f.len() {
        if f[j] == b'\'' {
            if j + 1 < f.len() && f[j + 1] == b'\'' {
                j += 2;
                continue;
            }
            return j + 1 - i;
        }
        j += 1;
    }
    f.len() - i
}

// ---------------------------------------------------------------------------
// Failure reporting. Every violation prints seed + input + op + output.
// ---------------------------------------------------------------------------

struct Case {
    seed: u64,
    iter: usize,
    axis: &'static str,
    op: &'static str,
    at: u32,
    count: u32,
}

fn fail(case: &Case, detail: &str, before: &[u8], after: Option<&[u8]>) -> ! {
    let mut msg = String::new();
    let _ = writeln!(msg, "=== INVARIANT VIOLATION ===");
    let _ = writeln!(msg, "seed:      {}", case.seed);
    let _ = writeln!(msg, "iteration: {}", case.iter);
    let _ = writeln!(msg, "axis:      {}", case.axis);
    let _ = writeln!(
        msg,
        "op:        {} at={} count={}",
        case.op, case.at, case.count
    );
    let _ = writeln!(msg, "reason:    {detail}");
    let _ = writeln!(msg, "--- input ({} bytes) ---", before.len());
    let _ = writeln!(msg, "{}", String::from_utf8_lossy(before));
    if let Some(a) = after {
        let _ = writeln!(msg, "--- output ({} bytes) ---", a.len());
        let _ = writeln!(msg, "{}", String::from_utf8_lossy(a));
    }
    let _ = writeln!(
        msg,
        "reproduce: re-run with this seed; the PRNG is deterministic"
    );
    panic!("{msg}");
}

// ---------------------------------------------------------------------------
// Operation plumbing.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Op {
    Insert,
    Delete,
}

fn apply(axis: Axis, op: Op, bytes: &[u8], at: u32, count: u32) -> Option<Cow<'_, [u8]>> {
    match (axis, op) {
        (Axis::Row, Op::Insert) => insert_rows(bytes, at, count),
        (Axis::Row, Op::Delete) => delete_rows(bytes, at, count),
        (Axis::Col, Op::Insert) => insert_cols(bytes, at, count),
        (Axis::Col, Op::Delete) => delete_cols(bytes, at, count),
    }
}

fn roundtrip_violated(m: &SheetModel, axis: Axis, at: u32, count: u32) -> bool {
    let before = serialize(m);
    let Some(mid) = apply(axis, Op::Insert, &before, at, count) else {
        return false;
    };
    let Some(back) = apply(axis, Op::Delete, &mid, at, count) else {
        return false;
    };
    back != before
}

// ---------------------------------------------------------------------------
// The sheet generator. Bias the pools so the hard cases are hit often:
// digit-length row boundaries, column-letter width boundaries, row gaps,
// self-closing and empty rows, shared formulas with a master + dependents,
// inline strings that look like cell references, and — for the bound hunts —
// dimensions and shared refs that over-extend toward the grid edge.
// ---------------------------------------------------------------------------

const ROW_BOUNDARIES: [u32; 12] = [
    1, 9, 10, 99, 100, 998, 999, 1000, 99_998, 99_999, 100_000, 1_048_576,
];
const COL_BOUNDARIES: [u32; 10] = [1, 2, 3, 5, 26, 27, 28, 52, 702, 16_384]; // A B C E Z AA AB AZ ZZ XFD

#[derive(Clone)]
struct GenOpts {
    max_row: u32,
    max_col: u32,
    shared: bool,
    overextend_dim: bool,
    overextend_ref: bool,
    cols_block: bool,
    spans: bool,
    merge_cells: bool,
}

fn gen_row(rng: &mut Rng, max: u32) -> u32 {
    let r = if rng.chance(60) {
        1 + rng.below(12) as u32
    } else if rng.chance(50) {
        rng.pick_u32(&ROW_BOUNDARIES)
    } else {
        1 + rng.below(200) as u32
    };
    r.min(max)
}

fn gen_col(rng: &mut Rng, max: u32) -> u32 {
    let c = if rng.chance(55) {
        1 + rng.below(8) as u32
    } else if rng.chance(50) {
        rng.pick_u32(&COL_BOUNDARIES)
    } else {
        1 + rng.below(60) as u32
    };
    c.min(max)
}

fn gen_formula_body(rng: &mut Rng) -> String {
    let a = 1 + rng.below(10) as u32;
    let b = 1 + rng.below(10) as u32;
    let f = rng.pick(&["SUM", "AVG", "MAX"]);
    match rng.below(3) {
        0 => format!("{f}(A{a}:C{b})"),
        1 => format!("{f}(B{a})*{b}"),
        _ => format!("=\"A1 not a ref {a}:{b}\"&B{a}"),
    }
}

fn gen_trap_text(rng: &mut Rng) -> String {
    let a = 1 + rng.below(9) as u32;
    let b = 1 + rng.below(9) as u32;
    let xs = [
        format!("A{a} looks like a ref to Z{b}"),
        format!("text {a}:{b} is not a range"),
        format!("literal XFD{a} and ZZ{b} inside"),
    ];
    xs[rng.below(xs.len() as u64) as usize].clone()
}

fn gen_cell_kind(rng: &mut Rng) -> CellKind {
    let roll = rng.below(100);
    if roll < 40 {
        CellKind::Value((1 + rng.below(1_000_000)) as u64)
    } else if roll < 55 {
        CellKind::SelfClosing
    } else if roll < 75 {
        CellKind::Formula(gen_formula_body(rng), (1 + rng.below(10_000)) as u64)
    } else {
        CellKind::InlineString(gen_trap_text(rng))
    }
}

fn gen_shared_group(
    rng: &mut Rng,
    opts: &GenOpts,
    map: &mut BTreeMap<(u32, u32), CellKind>,
    si: &mut u32,
) {
    let row_band = rng.chance(50);
    if row_band {
        let c = gen_col(rng, opts.max_col);
        let start = gen_row(rng, opts.max_row);
        let len = 2 + rng.below(3);
        let r2 = (start + len - 1).min(opts.max_row).max(start);
        let ref_r2 = if opts.overextend_ref && rng.chance(50) {
            (r2 + 1 + rng.below(3)).min(MAX_ROW)
        } else {
            r2
        };
        let s = *si;
        *si += 1;
        map.insert(
            (start, c),
            CellKind::SharedMaster {
                si: s,
                body: gen_formula_body(rng),
                r1: start,
                r2: ref_r2,
                c1: c,
                c2: c,
            },
        );
        for r in (start + 1)..=r2 {
            if r <= opts.max_row {
                map.insert((r, c), CellKind::SharedDep { si: s });
            }
        }
    } else {
        let r = gen_row(rng, opts.max_row);
        let c1 = gen_col(rng, opts.max_col);
        let len = 2 + rng.below(3);
        let c2 = (c1 + len - 1).min(opts.max_col).max(c1);
        let ref_c2 = if opts.overextend_ref && rng.chance(50) {
            (c2 + 1 + rng.below(2)).min(MAX_COL)
        } else {
            c2
        };
        let s = *si;
        *si += 1;
        map.insert(
            (r, c1),
            CellKind::SharedMaster {
                si: s,
                body: gen_formula_body(rng),
                r1: r,
                r2: r,
                c1,
                c2: ref_c2,
            },
        );
        for c in (c1 + 1)..=c2 {
            if c <= opts.max_col {
                map.insert((r, c), CellKind::SharedDep { si: s });
            }
        }
    }
}

fn gen_sheet(rng: &mut Rng, opts: &GenOpts) -> SheetModel {
    let mut map: BTreeMap<(u32, u32), CellKind> = BTreeMap::new();
    let mut si = 0u32;

    if opts.shared && rng.chance(70) {
        gen_shared_group(rng, opts, &mut map, &mut si);
        if rng.chance(25) {
            gen_shared_group(rng, opts, &mut map, &mut si);
        }
    }

    let n_isolated = rng.below(20);
    for _ in 0..n_isolated {
        let r = gen_row(rng, opts.max_row);
        let c = gen_col(rng, opts.max_col);
        map.entry((r, c)).or_insert_with(|| gen_cell_kind(rng));
    }

    let content = map.keys().cloned().collect::<Vec<_>>();
    let rows: Vec<RowModel> = {
        let mut by_row: BTreeMap<u32, Vec<CellModel>> = BTreeMap::new();
        for (r, c) in content {
            let kind = map.remove(&(r, c)).unwrap();
            by_row.entry(r).or_default().push(CellModel {
                row: r,
                col: c,
                kind,
            });
        }
        by_row
            .into_iter()
            .map(|(idx, mut cells)| {
                cells.sort_by_key(|c| c.col);
                let self_closing = rng.chance(15);
                RowModel {
                    idx,
                    self_closing,
                    cells: if self_closing { Vec::new() } else { cells },
                }
            })
            .collect()
    };

    // Recompute gaps: inject a few explicit empty placeholder rows inside gaps
    // so "rows with gaps" appear as `<row r="N"></row>` / self-closing. Bounded:
    // at most 2 placeholder rows per gap (a gap can be ~1M wide near the grid
    // edge, so never walk it).
    let mut rows = rows;
    let mut with_empties: Vec<RowModel> = Vec::new();
    let mut prev = 0u32;
    for row in rows.drain(..) {
        if row.idx - prev > 1 && rng.chance(40) {
            let fill = 1 + rng.below(2);
            for k in 1..=fill {
                let r = prev + k;
                if r >= row.idx {
                    break;
                }
                with_empties.push(RowModel {
                    idx: r,
                    self_closing: rng.chance(50),
                    cells: Vec::new(),
                });
            }
        }
        prev = row.idx;
        with_empties.push(row);
    }
    rows = with_empties;

    let r1 = rows.first().map(|r| r.idx).unwrap_or(1);
    let c1 = 1;
    let (r2, c2) = rows
        .iter()
        .flat_map(|r| r.cells.iter())
        .fold((r1, c1), |(mr, mc), c| (mr.max(c.row), mc.max(c.col)));
    let (r2, c2) = if r2 == 0 { (1, 1) } else { (r2, c2) };

    let dimension = if opts.overextend_dim && rng.chance(50) {
        let over = 1 + rng.below(500_000);
        Some((r1, c1, r2.saturating_add(over).min(MAX_ROW), c2))
    } else {
        Some((r1, c1, r2, c2))
    };

    let cols = if opts.cols_block && rng.chance(60) {
        let mut v = Vec::new();
        let mut c = 1u32;
        while c <= opts.max_col && v.len() < 6 {
            let e = (c + rng.below(6) as u32).min(opts.max_col);
            v.push((c, e));
            c = e + 1 + rng.below(2) as u32;
        }
        v
    } else {
        Vec::new()
    };

    let merge_cells = if opts.merge_cells {
        vec!["A1:B2".to_string()]
    } else {
        Vec::new()
    };

    SheetModel {
        rows,
        dimension,
        cols,
        merge_cells,
        spans: opts.spans,
    }
}

fn gen_op(rng: &mut Rng, model: &SheetModel) -> (u32, u32) {
    let content_max = model.rows.last().map(|r| r.idx).unwrap_or(1);
    let at = if rng.chance(30) {
        rng.pick_u32(&[1, 2, 3])
    } else if rng.chance(25) && content_max > 8 {
        content_max - rng.below(6) as u32
    } else {
        1 + rng.below(content_max.max(1) as u64)
    }
    .max(1);
    let count = if rng.chance(50) {
        1
    } else if rng.chance(30) {
        2 + rng.below(2) as u32
    } else {
        1 + rng.below(8) as u32
    };
    (at, count)
}

// ---------------------------------------------------------------------------
// The shrinker (plan Q2): halve the sheet, drop rows, drop cells, reduce the
// operation, re-test until 1-minimal.
// ---------------------------------------------------------------------------

fn shrink(
    mut model: SheetModel,
    at: u32,
    count: u32,
    _axis: Axis,
    failing: &dyn Fn(&SheetModel, u32, u32) -> bool,
) -> (SheetModel, u32, u32) {
    let mut at = at;
    let mut count = count;
    loop {
        let mut changed = false;
        for i in (0..model.rows.len()).rev() {
            let mut cand = model.clone();
            cand.rows.remove(i);
            if failing(&cand, at, count) {
                model = cand;
                changed = true;
                break;
            }
        }
        if changed {
            continue;
        }
        for i in 0..model.rows.len() {
            let nc = model.rows[i].cells.len();
            if nc > 1 {
                let mut cand = model.clone();
                cand.rows[i].cells.truncate(nc / 2);
                if failing(&cand, at, count) {
                    model = cand;
                    changed = true;
                    break;
                }
            }
        }
        if changed {
            continue;
        }
        if count > 1 {
            let c = count / 2;
            let c = if c < 1 { 1 } else { c };
            if failing(&model, at, c) {
                count = c;
                changed = true;
            }
        }
        if changed {
            continue;
        }
        if at > 1 {
            let cand = at / 2;
            if cand >= 1 && cand != at && failing(&model, cand, count) {
                at = cand;
                changed = true;
            }
        }
        if changed {
            continue;
        }
        break;
    }
    (model, at, count)
}

// ---------------------------------------------------------------------------
// Runners.
// ---------------------------------------------------------------------------

const SEED_ROUNDTRIP_ROWS: u64 = 0x5EED_0001;
const SEED_ROUNDTRIP_COLS: u64 = 0x5EED_0002;
const SEED_STREAM_ROWS: u64 = 0x5EED_0003;
const SEED_STREAM_COLS: u64 = 0x5EED_0004;
const SEED_BOUND_ROWS: u64 = 0x5EED_0005;
const SEED_BOUND_COLS: u64 = 0x5EED_0006;
const SEED_OPAQUE_ROWS: u64 = 0x5EED_0007;
const SEED_OPAQUE_COLS: u64 = 0x5EED_0008;

fn base_opts(axis: Axis) -> GenOpts {
    GenOpts {
        max_row: if axis == Axis::Row { 200 } else { 120 },
        max_col: if axis == Axis::Col { 120 } else { 30 },
        shared: true,
        overextend_dim: false,
        overextend_ref: false,
        cols_block: false,
        spans: false,
        merge_cells: true,
    }
}

#[test]
fn i1_round_trip_rows() {
    let mut rng = Rng::new(SEED_ROUNDTRIP_ROWS);
    let opts = base_opts(Axis::Row);
    for iter in 0..3000 {
        let model = gen_sheet(&mut rng, &opts);
        let before = serialize(&model);
        let (at, count) = gen_op(&mut rng, &model);
        let case = Case {
            seed: SEED_ROUNDTRIP_ROWS,
            iter,
            axis: "rows",
            op: "roundtrip insert+delete",
            at,
            count,
        };
        let Some(mid) = apply(Axis::Row, Op::Insert, &before, at, count) else {
            continue; // refused → I6 territory, not a round-trip candidate
        };
        let mid = mid.into_owned();
        let Some(back) = apply(Axis::Row, Op::Delete, &mid, at, count) else {
            fail(&case, "delete after insert refused", &mid, None);
        };
        let back = back.into_owned();
        if back != before {
            fail(
                &case,
                "insert_rows(at,n) + delete_rows(at,n) did not reproduce the input byte-for-byte",
                &before,
                Some(&back),
            );
        }
    }
}

#[test]
fn i1_round_trip_cols() {
    let mut rng = Rng::new(SEED_ROUNDTRIP_COLS);
    let opts = base_opts(Axis::Col);
    for iter in 0..3000 {
        let model = gen_sheet(&mut rng, &opts);
        let before = serialize(&model);
        let (at, count) = gen_op(&mut rng, &model);
        let case = Case {
            seed: SEED_ROUNDTRIP_COLS,
            iter,
            axis: "cols",
            op: "roundtrip insert+delete",
            at,
            count,
        };
        let Some(mid) = apply(Axis::Col, Op::Insert, &before, at, count) else {
            continue;
        };
        let mid = mid.into_owned();
        let Some(back) = apply(Axis::Col, Op::Delete, &mid, at, count) else {
            fail(&case, "delete after insert refused", &mid, None);
        };
        let back = back.into_owned();
        if back != before {
            fail(
                &case,
                "insert_cols(at,n) + delete_cols(at,n) did not reproduce the input byte-for-byte",
                &before,
                Some(&back),
            );
        }
    }
}

/// I2 + I4 + I5 on a random insert/delete walk, both axes.
fn run_stream(seed: u64, axis: Axis, iters: usize) {
    let mut rng = Rng::new(seed);
    let opts = base_opts(axis);
    for iter in 0..iters {
        let model = gen_sheet(&mut rng, &opts);
        let before = serialize(&model);
        let (at, count) = gen_op(&mut rng, &model);
        let op = if rng.chance(50) {
            Op::Insert
        } else {
            Op::Delete
        };
        let case = Case {
            seed,
            iter,
            axis: axis.name(),
            op: if matches!(op, Op::Insert) {
                "insert"
            } else {
                "delete"
            },
            at,
            count,
        };
        match apply(axis, op, &before, at, count) {
            None => {}
            Some(Cow::Borrowed(b)) => {
                if b != before {
                    fail(
                        &case,
                        "borrowed result must alias the unchanged input",
                        &before,
                        None,
                    );
                }
            }
            Some(Cow::Owned(after)) => {
                let sc = scan(&after);
                check_ordering(&sc).unwrap_or_else(|e| fail(&case, &e, &before, Some(&after)));
                check_wellformed(&after).unwrap_or_else(|e| fail(&case, &e, &before, Some(&after)));
                if let Err(e) = check_bounds(&sc) {
                    fail(&case, &e, &before, Some(&after));
                }
                let bcount = cell_count(&scan(&before));
                let acount = cell_count(&sc);
                match op {
                    Op::Insert => {
                        if acount != bcount {
                            fail(
                                &case,
                                &format!(
                                    "insert changed cell count: {bcount} -> {acount} (must be equal)"
                                ),
                                &before,
                                Some(&after),
                            );
                        }
                    }
                    Op::Delete => {
                        let expected =
                            bcount.saturating_sub(cells_in_band(&scan(&before), axis, at, count));
                        if acount != expected {
                            fail(
                                &case,
                                &format!(
                                    "delete removed {expected} cells (band count), output has {acount}, input had {bcount}"
                                ),
                                &before,
                                Some(&after),
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn i2_i4_i5_stream_rows() {
    run_stream(SEED_STREAM_ROWS, Axis::Row, 4000);
}

#[test]
fn i2_i4_i5_stream_cols() {
    run_stream(SEED_STREAM_COLS, Axis::Col, 4000);
}

/// I3 hunts the grid-bound guard: sheets with over-extended dimensions and
/// over-extended shared refs, plus boundary rows/cols and cols blocks. Expected
/// GREEN now that mutate refuses on both axes (coordinator regression tests
/// coordinator_shared_ref_cannot_escape_bottom_of_grid and
/// coordinator_dimension_cannot_escape_bottom_of_grid).
fn run_bound_hunt(seed: u64, axis: Axis, iters: usize) {
    let mut rng = Rng::new(seed);
    let mut opts = base_opts(axis);
    opts.max_row = MAX_ROW;
    opts.max_col = MAX_COL;
    opts.overextend_dim = true;
    opts.overextend_ref = true;
    opts.cols_block = true;
    opts.spans = true;
    for iter in 0..iters {
        let model = gen_sheet(&mut rng, &opts);
        let before = serialize(&model);
        let (at, count) = gen_op(&mut rng, &model);
        let op = if rng.chance(50) {
            Op::Insert
        } else {
            Op::Delete
        };
        let case = Case {
            seed,
            iter,
            axis: axis.name(),
            op: if matches!(op, Op::Insert) {
                "insert"
            } else {
                "delete"
            },
            at,
            count,
        };
        let Some(mid) = apply(axis, op, &before, at, count) else {
            continue;
        };
        let mid = mid.into_owned();
        let sc = scan(&mid);
        check_bounds(&sc).unwrap_or_else(|e| fail(&case, &e, &before, Some(&mid)));
        check_ordering(&sc).unwrap_or_else(|e| fail(&case, &e, &before, Some(&mid)));
        check_wellformed(&mid).unwrap_or_else(|e| fail(&case, &e, &before, Some(&mid)));
    }
}

#[test]
fn i3_bounds_hunt_rows() {
    run_bound_hunt(SEED_BOUND_ROWS, Axis::Row, 3000);
}

#[test]
fn i3_bounds_hunt_cols() {
    run_bound_hunt(SEED_BOUND_COLS, Axis::Col, 3000);
}

/// I7: inline-string text is opaque (byte-identical through any op) and formula
/// bodies equal `refshift::shift_refs` applied to the body on the shifted axis —
/// checked against the independent spec above, never the implementation. A
/// delete drops exactly the tokens/bodies of the deleted cells.
fn run_opaque(seed: u64, axis: Axis, iters: usize) {
    let mut rng = Rng::new(seed);
    let opts = base_opts(axis);
    for iter in 0..iters {
        let model = gen_sheet(&mut rng, &opts);
        let before = serialize(&model);
        let (at, count) = gen_op(&mut rng, &model);
        let case = Case {
            seed,
            iter,
            axis: axis.name(),
            op: "opaque",
            at,
            count,
        };
        for op in [Op::Insert, Op::Delete] {
            let Some(after) = apply(axis, op, &before, at, count) else {
                continue;
            };
            let after = after.into_owned();
            let sb = scan(&before);
            let sa = scan(&after);
            let delta = match op {
                Op::Insert => count as i64,
                Op::Delete => -(count as i64),
            };

            // Inline strings: identical in order; on delete, drop deleted cells'.
            let expected_inline = {
                let mut v = Vec::new();
                for (_, cells) in &sb.rows {
                    for c in cells {
                        let in_band = match axis {
                            Axis::Row => (at..at + count).contains(&c.row),
                            Axis::Col => (at..at + count).contains(&c.col),
                        };
                        if matches!(op, Op::Insert) || !in_band {
                            v.extend(c.inline.iter().cloned());
                        }
                    }
                }
                v
            };
            let got_inline = opaque_tokens(&sa);
            if expected_inline != got_inline {
                fail(
                    &case,
                    &format!(
                        "inline-string text changed: {} tokens before, {} after (is/t text is opaque)",
                        expected_inline.len(),
                        got_inline.len()
                    ),
                    &before,
                    Some(&after),
                );
            }

            // Formula bodies: must equal the ref-shift spec applied to the body.
            // The AFTER cell sits at a shifted coordinate; reverse-map it back to
            // the BEFORE cell that occupies the same logical grid position.
            for (_, cells_after) in &sa.rows {
                for c in cells_after {
                    let Some(body_after) = &c.fbody else {
                        continue;
                    };
                    let Some((br, bc)) = reverse_shift(axis, op, at, count, c.row, c.col) else {
                        continue;
                    };
                    let body_before = sb
                        .rows
                        .iter()
                        .flat_map(|(_, cs)| cs.iter())
                        .find(|b| b.row == br && b.col == bc)
                        .and_then(|b| b.fbody.clone());
                    let Some(body_before) = body_before else {
                        continue;
                    };
                    let spec = ref_shift_formula(&body_before, axis, at, delta);
                    if body_after != &spec {
                        fail(
                            &case,
                            &format!(
                                "formula body after {} at {} does not match the ref-shift spec:\n  before: {body_before}\n  spec:   {spec}\n  actual: {body_after}",
                                if matches!(op, Op::Insert) {
                                    "insert"
                                } else {
                                    "delete"
                                },
                                axis.name()
                            ),
                            &before,
                            Some(&after),
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn i7_opaque_rows() {
    run_opaque(SEED_OPAQUE_ROWS, Axis::Row, 1200);
}

#[test]
fn i7_opaque_cols() {
    run_opaque(SEED_OPAQUE_COLS, Axis::Col, 1200);
}

/// I6: refusal is total. None must write nothing (the input slice is immutable,
/// so this is structural), delta==0 never refuses and returns the input, and a
/// borrowed result is byte-identical to the input.
#[test]
fn i6_refusal_is_total() {
    // at == 0 always refuses, on both axes, both directions.
    let s = xml(
        "A1:A2",
        "<row r=\"1\"><c r=\"A1\"><v>1</v></c></row><row r=\"2\"><c r=\"A2\"><v>2</v></c></row>",
    );
    assert!(insert_rows(s.as_bytes(), 0, 1).is_none());
    assert!(delete_rows(s.as_bytes(), 0, 1).is_none());
    assert!(insert_cols(s.as_bytes(), 0, 1).is_none());
    assert!(delete_cols(s.as_bytes(), 0, 1).is_none());

    // delta == 0 is always a borrow of the unchanged input.
    for axis in [Axis::Row, Axis::Col] {
        for op in [Op::Insert, Op::Delete] {
            let r = apply(axis, op, s.as_bytes(), 2, 0).expect("zero delta must not refuse");
            match r {
                Cow::Borrowed(b) => assert_eq!(b, s.as_bytes()),
                Cow::Owned(b) => panic!("zero delta must borrow, got {b:?}"),
            }
        }
    }

    // Empty / missing sheetData refuses.
    assert!(insert_rows(b"<worksheet><sheetData/></worksheet>", 1, 1).is_none());
    assert!(insert_rows(b"<worksheet><sheetData></sheetData></worksheet>", 1, 1).is_none());
    assert!(insert_rows(b"<worksheet></worksheet>", 1, 1).is_none());

    // An implicit-numbered row at/below the shift point refuses.
    let imp = xml(
        "A1:A2",
        "<row><c r=\"A1\"><v>1</v></c></row><row r=\"2\"><c r=\"A2\"><v>2</v></c></row>",
    );
    assert!(insert_rows(imp.as_bytes(), 1, 1).is_none());
    assert!(delete_rows(imp.as_bytes(), 1, 1).is_none());

    // Insert that would push a real row past 1048576 refuses.
    let bottom = xml(
        "A1048576:A1048576",
        "<row r=\"1048576\"><c r=\"A1048576\"><v>x</v></c></row>",
    );
    assert!(insert_rows(bottom.as_bytes(), 1_048_576, 1).is_none());

    // Delete that would orphan a shared master while a dependent survives refuses.
    let orphan = xml(
        "A1:A3",
        "<row r=\"1\"><c r=\"A1\"><f t=\"shared\" ref=\"A1:A3\" si=\"0\">=A1+A2</f></c></row>\
         <row r=\"2\"><c r=\"A2\"><f t=\"shared\" si=\"0\"/></c></row>\
         <row r=\"3\"><c r=\"A3\"><f t=\"shared\" si=\"0\"/></c></row>",
    );
    assert!(delete_rows(orphan.as_bytes(), 1, 1).is_none());

    // A refused operation leaves the caller's bytes untouched.
    let saved = orphan.clone();
    let r = delete_rows(orphan.as_bytes(), 1, 1);
    assert!(r.is_none());
    assert_eq!(orphan, saved, "refused op must not mutate the input");
}

fn xml(dim: &str, rows: &str) -> String {
    format!(
        r#"<?xml version="1.0"?><worksheet xmlns="s"><dimension ref="{dim}"/><sheetData>{rows}</sheetData></worksheet>"#
    )
}

// ---------------------------------------------------------------------------
// Pinned design behaviours (endorsed scope for I1): the col splice drops the
// cached `spans` attribute and splits a straddling `<cols>` span. Both are
// deliberate; they are pinned here so they cannot silently regress either way.
// ---------------------------------------------------------------------------

#[test]
fn probe_col_splice_drops_spans() {
    let s = xml(
        "A1:C1",
        "<row r=\"1\" spans=\"1:3\"><c r=\"B1\" s=\"3\"><v>1</v></c></row>",
    );
    let out = insert_cols(s.as_bytes(), 2, 1).unwrap();
    let out = String::from_utf8(out.into_owned()).unwrap();
    assert!(
        !out.contains("spans="),
        "col splice must drop the cached spans attr (Excel recomputes it): {out}"
    );
    assert!(out.contains(r#"<c r="C1" s="3">"#), "{out}");
}

#[test]
fn probe_cols_span_straddle_splits() {
    let s = r#"<?xml version="1.0"?><worksheet xmlns="s"><dimension ref="A1:E1"/><cols><col min="1" max="5" width="8"/></cols><sheetData><row r="1"><c r="E1"><v>1</v></c></row></sheetData></worksheet>"#;
    let out = delete_cols(s.as_bytes(), 3, 1).unwrap();
    let out = String::from_utf8(out.into_owned()).unwrap();
    assert!(out.contains(r#"<col min="1" max="2" width="8"/>"#), "{out}");
    assert!(out.contains(r#"<col min="3" max="4" width="8"/>"#), "{out}");
}

/// The open coordinator question, settled on evidence: when the grid-bound guard
/// trips, must the splice refuse or emit the original (now stale) coordinate?
/// The shared-formula ref guard now REFUSES - the whole operation returns None,
/// so no stale ref can reach the output - which is the correct call, because a
/// stale coordinate breaks I1 on the paired delete. The dimension clamp is the
/// same class of problem and still trips: a declared dimension whose edge would
/// pass the grid is CLAMPED at the max, so the paired delete shrinks it and
/// round-trip identity fails. This probe pins both: the shared-ref refusal is
/// asserted green; the dimension round-trip break is expected RED until the
/// clamp is changed to refuse, matching the ref guard.
#[test]
fn probe_stale_ref_guard_breaks_round_trip() {
    // Shared-ref guard: must REFUSE, on both axes, so nothing stale is emitted.
    let shared_refs: &[(&[u8], u32, u32, Axis)] = &[
        (
            br#"<dimension ref="A1:A5"/><sheetData><row r="1048570"><c r="A1048570"><f t="shared" ref="A1048570:A1048576" si="0">=A1+A2</f><v>3</v></c></row></sheetData>"#,
            1,
            1,
            Axis::Row,
        ),
        (
            br#"<dimension ref="A1:XFD1"/><sheetData><row r="1"><c r="XEZ1"><f t="shared" ref="XEZ1:XFD1" si="0">=XEZ1</f><v>3</v></c></row></sheetData>"#,
            1,
            1,
            Axis::Col,
        ),
    ];
    for (n, (input, at, count, axis)) in shared_refs.iter().enumerate() {
        assert!(
            apply(*axis, Op::Insert, input, *at, *count).is_none(),
            "case {n} ({} axis) - a shared ref that cannot shift in-grid must refuse, not              emit a stale ref",
            axis.name()
        );
    }

    // Dimension: must SUCCEED with a clamped value. `<dimension>` is an
    // ADVISORY bounding box, not a coordinate that owns cells. Writers routinely
    // over-declare it — `A1:A1048576` for a sheet holding ten rows is common —
    // and Excel recomputes it on load.
    //
    // Refusing on it was implemented and then reverted: it made a sheet that
    // declares the full grid height reject an ordinary one-row insert, even
    // though the only real content sat at row 10. That is a false refusal on a
    // legitimate workbook, which is worse than an imprecise advisory value.
    //
    // Clamping is safe here because of ORDERING: the splice has already refused
    // if any real `<row>` would pass the grid edge, so by this point all actual
    // content is known to fit and only declared empty space is clamped.
    //
    // ACCEPTED CARVE-OUT: because the clamp is not symmetric, insert-then-delete
    // is not byte-identical for a dimension pinned at the edge — it clamps on
    // the way out and un-clamps one lower on the way back. I1 therefore does not
    // apply to this shape. That is a documented consequence of the decision, not
    // an undiscovered defect.
    let dimensions: &[(&[u8], u32, u32, Axis, &str, &str)] = &[
        (
            br#"<dimension ref="A1:A1048576"/><sheetData><row r="10"><c r="A10"><v>1</v></c></row></sheetData>"#,
            10,
            1,
            Axis::Row,
            "A1:A1048576",
            "1048577",
        ),
        (
            br#"<dimension ref="A1:XFD1"/><sheetData><row r="1"><c r="J1"><v>1</v></c></row></sheetData>"#,
            10,
            1,
            Axis::Col,
            "A1:XFD1",
            "XFE",
        ),
    ];
    for (n, (input, at, count, axis, want, forbidden)) in dimensions.iter().enumerate() {
        let mid = apply(*axis, Op::Insert, input, *at, *count).unwrap_or_else(|| {
            panic!(
                "dimension case {n} ({} axis): refused an operation whose real content stays                  well inside the grid. `<dimension>` is advisory and must clamp, not refuse.
{}",
                axis.name(),
                String::from_utf8_lossy(input)
            )
        });
        let mid = mid.into_owned();
        // I3 must still hold: no out-of-grid coordinate anywhere in the output.
        check_bounds(&scan(&mid)).expect("I3 bounds must hold after the clamp");
        let s = String::from_utf8_lossy(&mid).into_owned();
        assert!(
            s.contains(want),
            "dimension case {n} ({} axis): must clamp to {want}, got:
{s}",
            axis.name()
        );
        assert!(
            !s.contains(forbidden),
            "dimension case {n} ({} axis): emitted out-of-grid coordinate {forbidden}:
{s}",
            axis.name()
        );
    }
}

// ---------------------------------------------------------------------------
// Heavy runs — behind #[ignore] so the default suite stays under ~5 s.
// ---------------------------------------------------------------------------

/// Regression probe for the silent row-loss defect this harness found on its
/// first run. `insert_rows` dropped a row outright: rows 1, 10, 80, 200 with an
/// insert of 3 at row 132 came back as 1, 80, 203 — row 10 gone, no error.
///
/// Cause was in the formula fast path. Rows above the insertion point that carry
/// no formula are deliberately left unwritten so they stay in an untouched byte
/// run, but the code also advanced the marker tracking how much input had been
/// copied out. The next flush then started past those bytes and never emitted
/// them.
///
/// It takes three conditions at once: a formula-free row sitting BETWEEN two
/// formula rows, with all of them ABOVE the insertion point. Rows before the
/// first formula row are safe because the first flush copies from byte 0. That
/// is why a 684-test suite missed it — the formula tests are formula-dense and
/// the perf fixture is formula-free, so nothing exercised the mix.
#[test]
fn probe_row_splice_drops_gap_rows_between_formulas() {
    let input = r#"<?xml version="1.0"?><worksheet xmlns="x"><dimension ref="A1:AD200"/><sheetData><row r="1"><c r="A1"><f>SUM(A1:C10)</f><v>1</v></c></row><row r="10"><c r="A10"><v>10</v></c></row><row r="80"><c r="A80"><f>MAX(A7:C10)</f><v>80</v></c></row><row r="200"><c r="AD200"><v>200</v></c></row></sheetData></worksheet>"#;
    let mid = insert_rows(input.as_bytes(), 132, 3).expect("insert must succeed");
    let mid = String::from_utf8_lossy(&mid).into_owned();
    assert!(
        mid.contains(r#"<row r="10">"#) && mid.contains("<v>10</v>"),
        "row 10 was dropped between two formula rows:
{mid}"
    );
    assert!(
        mid.contains(r#"<row r="203">"#),
        "row 200 did not shift to 203:
{mid}"
    );
    assert!(
        mid.contains(r#"<row r="80">"#),
        "row 80 must not move:
{mid}"
    );
}

#[test]
#[ignore = "deep stress: 100k cases per axis, ~20 s"]
fn deep_round_trip_rows() {
    let mut rng = Rng::new(0xDEEF_0001);
    let opts = base_opts(Axis::Row);
    for iter in 0..100_000 {
        let model = gen_sheet(&mut rng, &opts);
        let before = serialize(&model);
        let (at, count) = gen_op(&mut rng, &model);
        let case = Case {
            seed: 0xDEEF_0001,
            iter,
            axis: "rows",
            op: "roundtrip insert+delete",
            at,
            count,
        };
        let Some(mid) = apply(Axis::Row, Op::Insert, &before, at, count) else {
            continue;
        };
        let mid = mid.into_owned();
        let Some(back) = apply(Axis::Row, Op::Delete, &mid, at, count) else {
            fail(&case, "delete after insert refused", &mid, None);
        };
        if back.into_owned() != before {
            fail(&case, "deep round-trip violated", &before, None);
        }
    }
}

#[test]
#[ignore = "deep stress: 100k cases per axis, ~20 s"]
fn deep_round_trip_cols() {
    let mut rng = Rng::new(0xDEEF_0002);
    let opts = base_opts(Axis::Col);
    for iter in 0..100_000 {
        let model = gen_sheet(&mut rng, &opts);
        let before = serialize(&model);
        let (at, count) = gen_op(&mut rng, &model);
        let case = Case {
            seed: 0xDEEF_0002,
            iter,
            axis: "cols",
            op: "roundtrip insert+delete",
            at,
            count,
        };
        let Some(mid) = apply(Axis::Col, Op::Insert, &before, at, count) else {
            continue;
        };
        let mid = mid.into_owned();
        let Some(back) = apply(Axis::Col, Op::Delete, &mid, at, count) else {
            fail(&case, "delete after insert refused", &mid, None);
        };
        if back.into_owned() != before {
            fail(&case, "deep round-trip violated", &before, None);
        }
    }
}

/// Demonstrate the shrinker (plan Q2) against the dimension-clamp failing shape:
/// it must reduce a many-row sheet to a smaller still-failing one.
#[test]
#[ignore = "demonstrates the shrinker; requires the open dimension-clamp finding"]
fn shrinker_demo_on_stale_ref_case() {
    // Deterministic violating model: a declared dimension reaching row 1048576
    // while content stops at row 10. Insert at 10 clamps the dimension in place;
    // the paired delete shrinks it, so round-trip identity fails.
    let model = SheetModel {
        rows: vec![
            RowModel {
                idx: 1,
                self_closing: false,
                cells: vec![CellModel {
                    row: 1,
                    col: 2,
                    kind: CellKind::Value(7),
                }],
            },
            RowModel {
                idx: 4,
                self_closing: false,
                cells: vec![CellModel {
                    row: 4,
                    col: 2,
                    kind: CellKind::Formula("MAX(A7:C10)".into(), 1),
                }],
            },
            RowModel {
                idx: 10,
                self_closing: false,
                cells: vec![CellModel {
                    row: 10,
                    col: 1,
                    kind: CellKind::Value(1),
                }],
            },
        ],
        dimension: Some((1, 1, 1_048_576, 2)),
        cols: Vec::new(),
        merge_cells: Vec::new(),
        spans: false,
    };
    let (at, count) = (10u32, 1u32);
    assert!(
        roundtrip_violated(&model, Axis::Row, at, count),
        "dimension-clamp shape must violate round-trip"
    );
    let original_len = serialize(&model).len();
    let (small, sat, scount) = shrink(model, at, count, Axis::Row, &|m, a, c| {
        roundtrip_violated(m, Axis::Row, a, c)
    });
    let small_len = serialize(&small).len();
    assert!(small_len <= original_len, "shrinker must not grow the case");
    eprintln!(
        "shrinker: {} bytes -> {} bytes, op at={} count={}",
        original_len, small_len, sat, scount
    );
    eprintln!(
        "shrunk sheet:\n{}",
        String::from_utf8_lossy(&serialize(&small))
    );
}
