//! Row and column insert/delete via one forward byte splice (PERF_EXPERIMENTS.md E2, M2).
//!
//! Rows above the insertion point stay inside an untouched byte run and are
//! never parsed — UNLESS they carry a formula, because a formula may reference
//! the shifted region even when the row that holds it does not move. Formula
//! bodies are the one part of a cell that must be rewritten everywhere: a
//! formula's text is passed through `refshift::shift_refs` on the shifted axis
//! so references inside it move with the grid. Rows at or below the shift point
//! get their `r=` attribute, their cells' `r=` attributes, and any
//! shared-formula `ref=` attribute rewritten, and their formula bodies shifted.
//! Nothing but the current row is ever held. Cached `<v>` text is opaque and
//! passes through untouched: the writer always emits calcPr
//! fullCalcOnLoad="1", so Excel recomputes every formula on open and the stale
//! cache can never leak into a saved workbook.
//!
//! The column splice is the same shape on the column axis. Because cells are
//! ordered by column within a row, every row is walked; a row holding no cell
//! at or after the shift column is copied byte-for-byte unless it carries a
//! formula. Cell refs are rewritten with variable-width column letters (Z ->
//! AA), the `<cols>` min/max spans shift, split or vanish, each affected row's
//! `spans` attribute is dropped (Excel recomputes it), and shared-formula
//! `ref=` column ranges follow the cells.
//!
//! Refusal contract: `None` means the operation is refused rather than
//! performed incorrectly. Refusal cases are a missing/empty `<sheetData>`, an
//! implicit-numbered row (row splice) or cell (column splice) at or below the
//! shift point, an insert that would push an existing row past row 1048576 or
//! an existing cell/span past column 16384, a shared-formula `ref=` or the
//! `<dimension>` element that would leave the grid, and a delete that would
//! remove a shared formula's `si` master while any dependent carrying the same
//! `si` survives.

use std::borrow::Cow;

use ahash::{AHashMap, AHashSet};

use crate::turbo::formula::translate_body;
use crate::turbo::overlay::{extract_xml_attr, find_element};
use crate::turbo::refshift::{Axis, shift_refs};
use crate::turbo::write::xml::{col_letters, write_u32};

const MAX_ROW: u64 = 1_048_576;
const MAX_COL: u32 = 16_384;

/// Shift rows in one worksheet part per openpyxl semantics.
///
/// `at` is the 1-based first affected row. `delta > 0` inserts `delta` blank
/// rows at `at..at+delta-1`, shifting existing rows `>= at` down by `delta`.
/// `delta < 0` removes rows `at..at-1-delta`, shifting rows below up.
///
/// Returns `Cow::Borrowed` when nothing changes (empty `delta`, or the whole
/// grid lies above the shift point), `Cow::Owned` with the spliced bytes
/// otherwise, and `None` when the operation is refused (see module doc).
pub fn shift_rows<'a>(xml: &'a [u8], at: u32, delta: i64) -> Option<Cow<'a, [u8]>> {
    if at == 0 {
        return None;
    }
    if delta == 0 {
        return Some(Cow::Borrowed(xml));
    }

    let sd_start = find_element(xml, b"sheetData", 0)?;
    let gt = sd_start + memchr::memchr(b'>', &xml[sd_start..])?;
    let self_closing = gt > sd_start && xml[gt - 1] == b'/';
    if self_closing {
        return None;
    }
    let body_start = gt + 1;
    let close = body_start + memchr::memmem::find(&xml[body_start..], b"</sheetData>")?;
    let body = &xml[body_start..close];
    find_element(body, b"row", 0)?;

    if delta < 0
        && memchr::memmem::find(body, b"t=\"shared\"").is_some()
        && would_orphan_shared(body, at, (-delta) as u64)
    {
        return None;
    }

    match splice_body(body, at, delta)? {
        Cow::Borrowed(_) => Some(Cow::Borrowed(xml)),
        Cow::Owned(sb) => {
            let header = update_dimension(&xml[..body_start], at, delta);
            let mut out = Vec::with_capacity(xml.len() + sb.len() + 16);
            match header {
                HeaderEdit::Unchanged => out.extend_from_slice(&xml[..body_start]),
                HeaderEdit::Changed(h) => out.extend_from_slice(&h),
                HeaderEdit::Refused => return None,
            }
            out.extend_from_slice(&sb);
            out.extend_from_slice(&xml[close..]);
            Some(Cow::Owned(out))
        }
    }
}

/// Insert `count` blank rows at `at` (openpyxl `insert_rows` semantics).
pub fn insert_rows<'a>(xml: &'a [u8], at: u32, count: u32) -> Option<Cow<'a, [u8]>> {
    shift_rows(xml, at, count as i64)
}

/// Delete `count` rows starting at `at` (openpyxl `delete_rows` semantics).
pub fn delete_rows<'a>(xml: &'a [u8], at: u32, count: u32) -> Option<Cow<'a, [u8]>> {
    shift_rows(xml, at, -(count as i64))
}

/// Shift columns in one worksheet part per openpyxl semantics.
///
/// `at` is the 1-based first affected column. `delta > 0` inserts `delta`
/// blank columns at `at..at+delta-1`, shifting existing columns `>= at` right
/// by `delta`. `delta < 0` removes columns `at..at-1-delta`, shifting columns
/// to the right left.
///
/// Returns `Cow::Borrowed` when nothing changes (empty `delta`, or the whole
/// grid lies left of the shift point), `Cow::Owned` with the spliced bytes
/// otherwise, and `None` when the operation is refused (see module doc).
pub fn shift_cols<'a>(xml: &'a [u8], at: u32, delta: i64) -> Option<Cow<'a, [u8]>> {
    if at == 0 {
        return None;
    }
    if delta == 0 {
        return Some(Cow::Borrowed(xml));
    }

    let sd_start = find_element(xml, b"sheetData", 0)?;
    let gt = sd_start + memchr::memchr(b'>', &xml[sd_start..])?;
    let self_closing = gt > sd_start && xml[gt - 1] == b'/';
    if self_closing {
        return None;
    }
    let body_start = gt + 1;
    let close = body_start + memchr::memmem::find(&xml[body_start..], b"</sheetData>")?;
    let body = &xml[body_start..close];
    find_element(body, b"row", 0)?;

    if delta < 0
        && memchr::memmem::find(body, b"t=\"shared\"").is_some()
        && would_orphan_shared_cols(body, at, (-delta) as u64)
    {
        return None;
    }

    let body_edit = match splice_body_cols(body, at, delta)? {
        Cow::Borrowed(_) => None,
        Cow::Owned(sb) => Some(sb),
    };
    let header_edit = update_header_cols(&xml[..body_start], at, delta);

    match (header_edit, body_edit) {
        (HeaderEdit::Refused, _) => None,
        (HeaderEdit::Unchanged, None) => Some(Cow::Borrowed(xml)),
        (HeaderEdit::Unchanged, Some(sb)) => {
            let mut out = Vec::with_capacity(xml.len() + sb.len() + 16);
            out.extend_from_slice(&xml[..body_start]);
            out.extend_from_slice(&sb);
            out.extend_from_slice(&xml[close..]);
            Some(Cow::Owned(out))
        }
        (HeaderEdit::Changed(h), body_edit) => {
            let extra = body_edit.as_ref().map(|b| b.len()).unwrap_or(0);
            let mut out = Vec::with_capacity(xml.len() + extra + 16);
            out.extend_from_slice(&h);
            match body_edit {
                Some(sb) => out.extend_from_slice(&sb),
                None => out.extend_from_slice(body),
            }
            out.extend_from_slice(&xml[close..]);
            Some(Cow::Owned(out))
        }
    }
}

/// Insert `count` blank columns at `at` (openpyxl `insert_cols` semantics).
pub fn insert_cols<'a>(xml: &'a [u8], at: u32, count: u32) -> Option<Cow<'a, [u8]>> {
    shift_cols(xml, at, count as i64)
}

/// Delete `count` columns starting at `at` (openpyxl `delete_cols` semantics).
pub fn delete_cols<'a>(xml: &'a [u8], at: u32, count: u32) -> Option<Cow<'a, [u8]>> {
    shift_cols(xml, at, -(count as i64))
}

/// Map a 1-based row index across a shift. `Some` carries the new index, `None`
/// means the index lies inside a delete band and is gone.
///
/// `pub(crate)` because the dependent-feature fixup pass (`turbo/fixup.rs`) must
/// use the exact same mapping as the splice — an off-by-one between the two
/// silently misaligns every merged range and every defined name in the file.
pub(crate) fn shifted_row(r: u32, at: u32, delta: i64) -> Option<u32> {
    let r = r as i64;
    let at = at as i64;
    if delta > 0 {
        Some(if r >= at {
            (r + delta) as u32
        } else {
            r as u32
        })
    } else {
        let count = -delta;
        if r < at {
            Some(r as u32)
        } else if r >= at + count {
            Some((r - count) as u32)
        } else {
            None
        }
    }
}

fn splice_body<'a>(body: &'a [u8], at: u32, delta: i64) -> Option<Cow<'a, [u8]>> {
    let mut out: Vec<u8> = Vec::new();
    let mut pos = 0usize;
    let mut seq_row = 0u32;
    let mut prefix_written = false;
    let mut last_end = 0usize;
    let at64 = at as u64;

    while let Some(row_start) = find_element(body, b"row", pos) {
        let gt_rel = memchr::memchr(b'>', &body[row_start..])?;
        let tag_end = row_start + gt_rel;
        let row_tag = &body[row_start..=tag_end];
        let r_attr = extract_xml_attr(row_tag, b"r");
        let has_r = r_attr.is_some();
        let row_idx = r_attr
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(seq_row + 1);
        seq_row = row_idx;
        let row_self_closing = tag_end > row_start && body[tag_end - 1] == b'/';
        let row_end = if row_self_closing {
            tag_end + 1
        } else {
            match memchr::memmem::find(&body[tag_end + 1..], b"</row>") {
                Some(rc) => tag_end + 1 + rc + 6,
                None => body.len(),
            }
        };

        let row_idx64 = row_idx as u64;
        if row_idx64 < at64 {
            // Fast path: rows above the insertion point stay in an untouched
            // byte run and are never parsed. The one deliberate exception is a
            // row that carries a formula: its body may reference the shifted
            // region even though the row itself does not move, so it must be
            // visited too. That is the unavoidable cost of correctness, and
            // only formula-bearing rows pay it. A cheap scan for the two bytes
            // "<f" keeps every other row in the untouched run exactly as today.
            // Do NOT advance `last_end` here. It marks how far the output has
            // been filled, and this row is deliberately NOT being written: it
            // belongs to the untouched run that the next flush copies wholesale
            // via `body[last_end..row_start]`. Advancing it would step over
            // these bytes and drop the row from the output entirely — silent
            // data loss, visible only when a formula-free row sits between two
            // formula rows above the insertion point.
            if !contains_formula(&body[row_start..row_end]) {
                pos = row_end;
                continue;
            }
            if !prefix_written {
                out.extend_from_slice(&body[0..row_start]);
                prefix_written = true;
            } else {
                out.extend_from_slice(&body[last_end..row_start]);
            }
            rewrite_row_formulas(&mut out, &body[row_start..row_end], at, delta, Axis::Row)?;
            last_end = row_end;
            pos = row_end;
            continue;
        }
        if !has_r {
            return None;
        }

        if !prefix_written {
            out.extend_from_slice(&body[0..row_start]);
            prefix_written = true;
        } else {
            out.extend_from_slice(&body[last_end..row_start]);
        }

        if delta > 0 {
            let new_idx = row_idx64.checked_add(delta as u64)?;
            if new_idx > MAX_ROW {
                return None;
            }
            rewrite_row(
                &mut out,
                &body[row_start..row_end],
                at,
                delta,
                new_idx as u32,
            )?;
        } else {
            let count = (-delta) as u64;
            if row_idx64 >= at64 + count {
                let new_idx = (row_idx64 - count) as u32;
                rewrite_row(&mut out, &body[row_start..row_end], at, delta, new_idx)?;
            }
        }
        last_end = row_end;
        pos = row_end;
    }

    if !prefix_written {
        return Some(Cow::Borrowed(body));
    }
    out.extend_from_slice(&body[last_end..]);
    Some(Cow::Owned(out))
}

fn rewrite_row(out: &mut Vec<u8>, row: &[u8], at: u32, delta: i64, new_idx: u32) -> Option<()> {
    let gt_rel = memchr::memchr(b'>', row).unwrap_or(row.len().saturating_sub(1));
    let tag_end = gt_rel;
    let tag = &row[..=tag_end];
    let self_closing = tag_end > 0 && row[tag_end - 1] == b'/';
    if let Some((vs, ve)) = attr_value_span(tag, b"r") {
        out.extend_from_slice(&tag[..vs]);
        write_u32(out, new_idx);
        out.extend_from_slice(&tag[ve..]);
    } else {
        out.extend_from_slice(tag);
    }
    if self_closing {
        return Some(());
    }
    let close_at = if row.ends_with(b"</row>") {
        row.len() - 6
    } else {
        row.len()
    };
    let body = &row[tag_end + 1..close_at];
    let mut pos = 0;
    while let Some(cs) = find_element(body, b"c", pos) {
        out.extend_from_slice(&body[pos..cs]);
        let Some(cgt_rel) = memchr::memchr(b'>', &body[cs..]) else {
            break;
        };
        let ctag_end = cs + cgt_rel;
        let cself = ctag_end > cs && body[ctag_end - 1] == b'/';
        let cend = if cself {
            ctag_end + 1
        } else {
            match memchr::memmem::find(&body[ctag_end + 1..], b"</c>") {
                Some(ce) => ctag_end + 1 + ce + 4,
                None => body.len(),
            }
        };
        rewrite_cell(out, &body[cs..cend], at, delta, new_idx)?;
        pos = cend;
    }
    out.extend_from_slice(&body[pos..]);
    out.extend_from_slice(&row[close_at..]);
    Some(())
}

fn rewrite_cell(
    out: &mut Vec<u8>,
    cell: &[u8],
    at: u32,
    delta: i64,
    parent_new_idx: u32,
) -> Option<()> {
    let gt_rel = memchr::memchr(b'>', cell).unwrap_or(cell.len().saturating_sub(1));
    let tag_end = gt_rel;
    let tag = &cell[..=tag_end];
    let self_closing = tag_end > 0 && cell[tag_end - 1] == b'/';
    if let Some((vs, ve)) = attr_value_span(tag, b"r") {
        let val = &tag[vs..ve];
        let mut prefix_end = val.len();
        let mut new_row = parent_new_idx;
        if let Some((ds, de)) = trailing_digits_span(val) {
            if let Some(old) = std::str::from_utf8(&val[ds..de])
                .ok()
                .and_then(|s| s.parse::<u32>().ok())
            {
                new_row = shifted_row(old, at, delta).unwrap_or(parent_new_idx);
                prefix_end = ds;
            }
        }
        out.extend_from_slice(&tag[..vs]);
        out.extend_from_slice(&val[..prefix_end]);
        write_u32(out, new_row);
        out.extend_from_slice(&tag[ve..]);
    } else {
        out.extend_from_slice(tag);
    }
    if self_closing {
        return Some(());
    }
    let close_at = if cell.ends_with(b"</c>") {
        cell.len() - 4
    } else {
        cell.len()
    };
    let body = &cell[tag_end + 1..close_at];
    rewrite_cell_f_body(out, body, at, delta, Axis::Row)?;
    out.extend_from_slice(&cell[close_at..]);
    Some(())
}

/// Rewrite every `<f>` element in one cell's body: shift the shared-formula
/// `ref=` attribute (its existing path) and shift the formula BODY TEXT through
/// `refshift::shift_refs`. The two shift exactly once each, on their own paths.
/// Cells without an `<f>` are copied byte-for-byte. Returns `None` when a shared
/// `ref=` would leave the grid — the whole operation must refuse.
fn rewrite_cell_f_body(
    out: &mut Vec<u8>,
    body: &[u8],
    at: u32,
    delta: i64,
    axis: Axis,
) -> Option<()> {
    if let Some(fs) = find_element(body, b"f", 0) {
        out.extend_from_slice(&body[..fs]);
        let fend = match memchr::memchr(b'>', &body[fs..]) {
            Some(gt_rel) if body[fs + gt_rel - 1] == b'/' => fs + gt_rel + 1,
            _ => match memchr::memmem::find(&body[fs..], b"</f>") {
                Some(off) => fs + off + 4,
                None => body.len(),
            },
        };
        rewrite_f_element(out, &body[fs..fend], at, delta, axis)?;
        out.extend_from_slice(&body[fend..]);
    } else {
        out.extend_from_slice(body);
    }
    Some(())
}

/// Rewrite one `<f>...</f>` element: the `ref=` attribute via `rewrite_f_tag`
/// /`rewrite_f_tag_cols`, then the body text through `refshift::shift_refs`.
/// A self-closing `<f .../>` has no body, and an empty `<f></f>` body (a
/// shared-formula dependent carrying only `si`) is left alone. Returns `None`
/// when a shared `ref=` would leave the grid — the whole operation must refuse.
fn rewrite_f_element(
    out: &mut Vec<u8>,
    fspan: &[u8],
    at: u32,
    delta: i64,
    axis: Axis,
) -> Option<()> {
    let Some(gt_rel) = memchr::memchr(b'>', fspan) else {
        out.extend_from_slice(fspan);
        return Some(());
    };
    let tag = &fspan[..=gt_rel];
    let self_closing = gt_rel > 0 && fspan[gt_rel - 1] == b'/';
    match axis {
        Axis::Row => rewrite_f_tag(out, tag, at, delta)?,
        Axis::Col => rewrite_f_tag_cols(out, tag, at, delta)?,
    }
    if self_closing {
        return Some(());
    }
    let after = &fspan[gt_rel + 1..];
    let close_len = usize::from(after.ends_with(b"</f>")) * 4;
    let body = &after[..after.len() - close_len];
    if !body.is_empty() {
        if let Ok(s) = std::str::from_utf8(body) {
            match shift_refs(s, axis, at, delta) {
                Cow::Borrowed(_) => out.extend_from_slice(body),
                Cow::Owned(shifted) => out.extend_from_slice(shifted.as_bytes()),
            }
        } else {
            out.extend_from_slice(body);
        }
    }
    if close_len > 0 {
        out.extend_from_slice(&after[after.len() - close_len..]);
    }
    Some(())
}

/// Rewrite only the formula bodies inside a row, leaving the row's `r=` and its
/// cells' `r=` attributes untouched. Used for rows above the shift point that
/// carry a formula: the row does not move, only its formulas' references do.
/// Returns `None` when a shared `ref=` would leave the grid — the whole
/// operation must refuse even though the row itself does not move.
fn rewrite_row_formulas(
    out: &mut Vec<u8>,
    row: &[u8],
    at: u32,
    delta: i64,
    axis: Axis,
) -> Option<()> {
    let gt_rel = memchr::memchr(b'>', row).unwrap_or(row.len().saturating_sub(1));
    let tag_end = gt_rel;
    let self_closing = tag_end > 0 && row[tag_end - 1] == b'/';
    out.extend_from_slice(&row[..=tag_end]);
    if self_closing {
        return Some(());
    }
    let close_at = if row.ends_with(b"</row>") {
        row.len() - 6
    } else {
        row.len()
    };
    let body = &row[tag_end + 1..close_at];
    let mut pos = 0;
    while let Some(cs) = find_element(body, b"c", pos) {
        out.extend_from_slice(&body[pos..cs]);
        let Some(cgt_rel) = memchr::memchr(b'>', &body[cs..]) else {
            break;
        };
        let ctag_end = cs + cgt_rel;
        let cself = ctag_end > cs && body[ctag_end - 1] == b'/';
        let cend = if cself {
            ctag_end + 1
        } else {
            match memchr::memmem::find(&body[ctag_end + 1..], b"</c>") {
                Some(ce) => ctag_end + 1 + ce + 4,
                None => body.len(),
            }
        };
        rewrite_cell_formulas(out, &body[cs..cend], at, delta, axis)?;
        pos = cend;
    }
    out.extend_from_slice(&body[pos..]);
    out.extend_from_slice(&row[close_at..]);
    Some(())
}

/// Copy a cell's tag unchanged and shift only the formula bodies inside it.
fn rewrite_cell_formulas(
    out: &mut Vec<u8>,
    cell: &[u8],
    at: u32,
    delta: i64,
    axis: Axis,
) -> Option<()> {
    let gt_rel = memchr::memchr(b'>', cell).unwrap_or(cell.len().saturating_sub(1));
    let tag_end = gt_rel;
    let self_closing = tag_end > 0 && cell[tag_end - 1] == b'/';
    out.extend_from_slice(&cell[..=tag_end]);
    if self_closing {
        return Some(());
    }
    let close_at = if cell.ends_with(b"</c>") {
        cell.len() - 4
    } else {
        cell.len()
    };
    let body = &cell[tag_end + 1..close_at];
    rewrite_cell_f_body(out, body, at, delta, axis)?;
    out.extend_from_slice(&cell[close_at..]);
    Some(())
}

/// Cheap guard used to keep the splice's untouched byte run: true only when the
/// row's byte span contains the two bytes `<f`.
#[inline]
fn contains_formula(row: &[u8]) -> bool {
    memchr::memmem::find(row, b"<f").is_some()
}

/// Three-way outcome of shifting a shared-formula `ref=` attribute. The row and
/// column paths share it so the two axes cannot drift apart again.
enum RefShift {
    /// Nothing changed, or the value could not be parsed. The caller emits the
    /// original tag.
    Unchanged,
    /// The ref shifted in-grid and must be rewritten.
    Changed(Vec<u8>),
    /// The shifted ref would leave the grid (or invert). The caller refuses the
    /// whole operation: `None` on the public entry points, never a stale ref.
    OutOfGrid,
}

fn rewrite_f_tag(out: &mut Vec<u8>, ftag: &[u8], at: u32, delta: i64) -> Option<()> {
    if memchr::memmem::find(ftag, b"t=\"shared\"").is_none() {
        out.extend_from_slice(ftag);
        return Some(());
    }
    let Some(rp) = memchr::memmem::find(ftag, b"ref=\"") else {
        out.extend_from_slice(ftag);
        return Some(());
    };
    let vs = rp + 5;
    let Some(vl) = memchr::memchr(b'"', &ftag[vs..]) else {
        out.extend_from_slice(ftag);
        return Some(());
    };
    let ve = vs + vl;
    match shift_ref_value(&ftag[vs..ve], at, delta) {
        RefShift::Unchanged => {
            out.extend_from_slice(ftag);
            Some(())
        }
        RefShift::Changed(new_val) => {
            out.extend_from_slice(&ftag[..vs]);
            out.extend_from_slice(&new_val);
            out.extend_from_slice(&ftag[ve..]);
            Some(())
        }
        RefShift::OutOfGrid => None,
    }
}

fn shift_ref_value(val: &[u8], at: u32, delta: i64) -> RefShift {
    let Some(s) = std::str::from_utf8(val).ok() else {
        return RefShift::Unchanged;
    };
    let parts: Vec<&str> = s.split(':').map(str::trim).collect();
    if parts.is_empty() || parts.len() > 2 {
        return RefShift::Unchanged;
    }
    let mut spans = Vec::with_capacity(parts.len());
    let mut mapped = Vec::with_capacity(parts.len());
    for ep in &parts {
        let Some((ds, de)) = trailing_digits_span(ep.as_bytes()) else {
            return RefShift::Unchanged;
        };
        let Some(old) = ep[ds..de].parse::<u32>().ok() else {
            return RefShift::Unchanged;
        };
        spans.push((ds, de));
        mapped.push(shifted_row(old, at, delta));
    }
    let new_rows: Vec<u32> = match (parts.len(), mapped[0], mapped.get(1).copied().flatten()) {
        (1, Some(r), _) => vec![r],
        (1, None, _) => return RefShift::Unchanged,
        (_, Some(a), Some(b)) => vec![a, b],
        (_, None, Some(b)) => vec![at, b],
        (_, Some(a), None) => vec![a, at.saturating_sub(1)],
        (_, None, None) => return RefShift::Unchanged,
    };
    // Both of these make the ref impossible to represent in-grid. The whole
    // operation must refuse (`OutOfGrid`), not emit the original stale tag:
    // a stale ref no longer covers its own master and breaks round-trip
    // identity on the paired delete.
    if new_rows.len() == 2 && new_rows[1] < new_rows[0] {
        return RefShift::OutOfGrid;
    }
    if new_rows.iter().any(|&r| r == 0 || u64::from(r) > MAX_ROW) {
        return RefShift::OutOfGrid;
    }
    let mut changed = false;
    let mut new = Vec::with_capacity(val.len() + 8);
    for (i, ep) in parts.iter().enumerate() {
        if i > 0 {
            new.push(b':');
        }
        let (ds, de) = spans[i];
        new.extend_from_slice(&ep.as_bytes()[..ds]);
        let Some(old) = ep[ds..de].parse::<u32>().ok() else {
            return RefShift::Unchanged;
        };
        if old != new_rows[i] {
            changed = true;
        }
        write_u32(&mut new, new_rows[i]);
    }
    if changed {
        RefShift::Changed(new)
    } else {
        RefShift::Unchanged
    }
}

/// Column index after the shift; `None` means the column is removed by a delete.
///
/// `pub(crate)` because the dependent-feature fixup pass (`turbo/fixup.rs`) must
/// use the exact same mapping as the splice — an off-by-one between the two
/// silently misaligns every merged range and every defined name in the file.
pub(crate) fn shifted_col(c: u32, at: u32, delta: i64) -> Option<u32> {
    let c = c as i64;
    let at = at as i64;
    if delta > 0 {
        if c < at {
            Some(c as u32)
        } else {
            Some((c + delta) as u32)
        }
    } else {
        let count = -delta;
        if c < at {
            Some(c as u32)
        } else if c >= at + count {
            Some((c - count) as u32)
        } else {
            None
        }
    }
}

fn splice_body_cols<'a>(body: &'a [u8], at: u32, delta: i64) -> Option<Cow<'a, [u8]>> {
    let mut out: Vec<u8> = Vec::new();
    let mut pos = 0usize;
    let mut prefix_written = false;
    let mut last_end = 0usize;
    let mut any_changed = false;

    while let Some(row_start) = find_element(body, b"row", pos) {
        let gt_rel = memchr::memchr(b'>', &body[row_start..])?;
        let tag_end = row_start + gt_rel;
        let row_self_closing = tag_end > row_start && body[tag_end - 1] == b'/';
        let row_end = if row_self_closing {
            tag_end + 1
        } else {
            match memchr::memmem::find(&body[tag_end + 1..], b"</row>") {
                Some(rc) => tag_end + 1 + rc + 6,
                None => body.len(),
            }
        };

        if !prefix_written {
            out.extend_from_slice(&body[0..row_start]);
            prefix_written = true;
        } else {
            out.extend_from_slice(&body[last_end..row_start]);
        }

        if row_self_closing {
            out.extend_from_slice(&body[row_start..row_end]);
        } else if rewrite_row_cols(&mut out, &body[row_start..row_end], at, delta)? {
            any_changed = true;
        }
        last_end = row_end;
        pos = row_end;
    }

    if !prefix_written || !any_changed {
        return Some(Cow::Borrowed(body));
    }
    out.extend_from_slice(&body[last_end..]);
    Some(Cow::Owned(out))
}

/// Rewrite one row's cells for a column shift. Returns `false` when the row
/// holds no cell at or after the shift column (copied byte-for-byte), `true`
/// when it was rewritten, and `None` on refusal.
fn rewrite_row_cols(out: &mut Vec<u8>, row: &[u8], at: u32, delta: i64) -> Option<bool> {
    let gt_rel = memchr::memchr(b'>', row)?;
    let tag_end = gt_rel;
    let tag = &row[..=tag_end];
    let body_start = tag_end + 1;
    let close_at = if row.ends_with(b"</row>") {
        row.len() - 6
    } else {
        row.len()
    };
    let body = &row[body_start..close_at];

    #[allow(dead_code)] // `has_r` documents the parsed shape; the splice keys off offsets.
    struct CellRun {
        start: usize,
        end: usize,
        col: u32,
        has_r: bool,
    }
    let mut cells: Vec<CellRun> = Vec::new();
    let mut seq_col = 0u32;
    let mut affected = false;
    let mut cpos = 0usize;
    while let Some(cs) = find_element(body, b"c", cpos) {
        let cgt_rel = memchr::memchr(b'>', &body[cs..])?;
        let ctag_end = cs + cgt_rel;
        let cself = ctag_end > cs && body[ctag_end - 1] == b'/';
        let cend = if cself {
            ctag_end + 1
        } else {
            match memchr::memmem::find(&body[ctag_end + 1..], b"</c>") {
                Some(ce) => ctag_end + 1 + ce + 4,
                None => body.len(),
            }
        };
        let ctag = &body[cs..=ctag_end];
        let has_r = memchr::memmem::find(ctag, b"r=\"").is_some();
        let col = if let Some(v) = extract_xml_attr(ctag, b"r") {
            col_from_ref_bytes(v.as_bytes()).unwrap_or(seq_col + 1)
        } else {
            seq_col + 1
        };
        seq_col = col;
        if col >= at {
            affected = true;
            if !has_r {
                return None;
            }
        }
        cells.push(CellRun {
            start: cs,
            end: cend,
            col,
            has_r,
        });
        cpos = cend;
    }

    if !affected {
        // No cell moves, but a formula inside the row may still reference the
        // shifted columns. Rewrite only the formula bodies in that case; a row
        // with neither is copied byte-for-byte (fast path).
        if contains_formula(row) {
            rewrite_row_formulas(out, row, at, delta, Axis::Col)?;
            return Some(true);
        }
        out.extend_from_slice(row);
        return Some(false);
    }

    // The spans attribute is a cached column range; Excel recomputes it on
    // load, so drop it here rather than track it through the shift.
    match drop_attr(tag, b"spans") {
        Some((s, e)) => {
            out.extend_from_slice(&tag[..s]);
            out.extend_from_slice(&tag[e..]);
        }
        None => out.extend_from_slice(tag),
    }

    let mut last = 0usize;
    for run in &cells {
        out.extend_from_slice(&body[last..run.start]);
        let shifted = shifted_col(run.col, at, delta);
        if delta > 0 {
            let new_col = shifted?;
            if new_col > MAX_COL {
                return None;
            }
            rewrite_cell_cols(out, &body[run.start..run.end], new_col, at, delta)?;
        } else {
            if let Some(new_col) = shifted {
                rewrite_cell_cols(out, &body[run.start..run.end], new_col, at, delta)?;
            }
        }
        last = run.end;
    }
    out.extend_from_slice(&body[last..]);
    out.extend_from_slice(&row[close_at..]);
    Some(true)
}

fn rewrite_cell_cols(
    out: &mut Vec<u8>,
    cell: &[u8],
    new_col: u32,
    at: u32,
    delta: i64,
) -> Option<()> {
    let gt_rel = memchr::memchr(b'>', cell).unwrap_or(cell.len().saturating_sub(1));
    let tag_end = gt_rel;
    let tag = &cell[..=tag_end];
    let self_closing = tag_end > 0 && cell[tag_end - 1] == b'/';
    if let Some((vs, ve)) = attr_value_span(tag, b"r") {
        let val = &tag[vs..ve];
        let mut ls = 0;
        while ls < val.len() && val[ls].is_ascii_alphabetic() {
            ls += 1;
        }
        out.extend_from_slice(&tag[..vs]);
        let mut buf = [0u8; 4];
        out.extend_from_slice(col_letters(new_col, &mut buf));
        out.extend_from_slice(&val[ls..]);
        out.extend_from_slice(&tag[ve..]);
    } else {
        out.extend_from_slice(tag);
    }
    if self_closing {
        return Some(());
    }
    let close_at = if cell.ends_with(b"</c>") {
        cell.len() - 4
    } else {
        cell.len()
    };
    let body = &cell[tag_end + 1..close_at];
    rewrite_cell_f_body(out, body, at, delta, Axis::Col)?;
    out.extend_from_slice(&cell[close_at..]);
    Some(())
}

fn rewrite_f_tag_cols(out: &mut Vec<u8>, ftag: &[u8], at: u32, delta: i64) -> Option<()> {
    if memchr::memmem::find(ftag, b"t=\"shared\"").is_none() {
        out.extend_from_slice(ftag);
        return Some(());
    }
    let Some(rp) = memchr::memmem::find(ftag, b"ref=\"") else {
        out.extend_from_slice(ftag);
        return Some(());
    };
    let vs = rp + 5;
    let Some(vl) = memchr::memchr(b'"', &ftag[vs..]) else {
        out.extend_from_slice(ftag);
        return Some(());
    };
    let ve = vs + vl;
    match shift_ref_value_cols(&ftag[vs..ve], at, delta) {
        RefShift::Unchanged => {
            out.extend_from_slice(ftag);
            Some(())
        }
        RefShift::Changed(new_val) => {
            out.extend_from_slice(&ftag[..vs]);
            out.extend_from_slice(&new_val);
            out.extend_from_slice(&ftag[ve..]);
            Some(())
        }
        RefShift::OutOfGrid => None,
    }
}

fn shift_ref_value_cols(val: &[u8], at: u32, delta: i64) -> RefShift {
    let Some(s) = std::str::from_utf8(val).ok() else {
        return RefShift::Unchanged;
    };
    let parts: Vec<&str> = s.split(':').map(str::trim).collect();
    if parts.is_empty() || parts.len() > 2 {
        return RefShift::Unchanged;
    }
    let mut spans = Vec::with_capacity(parts.len());
    let mut mapped = Vec::with_capacity(parts.len());
    for ep in &parts {
        let Some((ds, de)) = col_letters_span(ep.as_bytes()) else {
            return RefShift::Unchanged;
        };
        let Some(old) = letters_to_col(&ep.as_bytes()[ds..de]) else {
            return RefShift::Unchanged;
        };
        spans.push((ds, de));
        mapped.push(shifted_col(old, at, delta));
    }
    let new_cols: Vec<u32> = match (parts.len(), mapped[0], mapped.get(1).copied().flatten()) {
        (1, Some(c), _) => vec![c],
        (1, None, _) => return RefShift::Unchanged,
        (_, Some(a), Some(b)) => vec![a, b],
        (_, None, Some(b)) => vec![at, b],
        (_, Some(a), None) => vec![a, at.saturating_sub(1)],
        (_, None, None) => return RefShift::Unchanged,
    };
    if new_cols.len() == 2 && new_cols[1] < new_cols[0] {
        return RefShift::OutOfGrid;
    }
    if new_cols.iter().any(|&c| c == 0 || c > MAX_COL) {
        return RefShift::OutOfGrid;
    }
    let mut changed = false;
    let mut new = Vec::with_capacity(val.len() + 8);
    for (i, ep) in parts.iter().enumerate() {
        if i > 0 {
            new.push(b':');
        }
        let (ds, de) = spans[i];
        let Some(old) = letters_to_col(&ep.as_bytes()[ds..de]) else {
            return RefShift::Unchanged;
        };
        if old != new_cols[i] {
            changed = true;
        }
        let mut buf = [0u8; 4];
        new.extend_from_slice(col_letters(new_cols[i], &mut buf));
        new.extend_from_slice(&ep.as_bytes()[de..]);
    }
    if changed {
        RefShift::Changed(new)
    } else {
        RefShift::Unchanged
    }
}

enum HeaderEdit {
    Unchanged,
    Refused,
    Changed(Vec<u8>),
}

enum ColsSpanEdit {
    None,
    Refused,
    Replaced {
        start: usize,
        end: usize,
        bytes: Vec<u8>,
    },
}

/// Three-way outcome of shifting the `<dimension>` element. Mirrors `RefShift`:
/// a declared dimension that would leave the grid refuses rather than clamping,
/// so the column and row axes and the ref/dimension paths all agree.
enum DimEdit {
    /// Never constructed today, and the handler at the call site is therefore
    /// unreachable — both are kept on purpose.
    ///
    /// An earlier pass made an out-of-range `<dimension>` refuse the whole
    /// edit. That was wrong: `<dimension>` is advisory and Excel recomputes it,
    /// so a workbook declaring `A1:A1048576` with data in row 5 was refusing a
    /// legal one-row insert. The decision was reverted to clamping on both
    /// axes. Deleting this variant would also delete the refusal path, and the
    /// next person to want a refusal would rebuild it without finding out why
    /// it was removed.
    #[allow(dead_code)]
    Refused,
    None,
    Replaced {
        start: usize,
        end: usize,
        bytes: Vec<u8>,
    },
}

fn update_header_cols(header: &[u8], at: u32, delta: i64) -> HeaderEdit {
    let cols = rewrite_cols(header, at, delta);
    if let ColsSpanEdit::Refused = cols {
        return HeaderEdit::Refused;
    }
    let dim = dimension_edit_cols(header, at, delta);
    if let DimEdit::Refused = dim {
        return HeaderEdit::Refused;
    }
    let mut edits: Vec<(usize, usize, Vec<u8>)> = Vec::new();
    if let DimEdit::Replaced { start, end, bytes } = dim {
        edits.push((start, end, bytes));
    }
    if let ColsSpanEdit::Replaced { start, end, bytes } = cols {
        edits.push((start, end, bytes));
    }
    if edits.is_empty() {
        return HeaderEdit::Unchanged;
    }
    edits.sort_by_key(|(s, _, _)| *s);
    let mut out = Vec::with_capacity(header.len() + 32);
    let mut pos = 0usize;
    for (s, e, repl) in edits {
        out.extend_from_slice(&header[pos..s]);
        out.extend_from_slice(&repl);
        pos = e;
    }
    out.extend_from_slice(&header[pos..]);
    HeaderEdit::Changed(out)
}

fn dimension_edit_cols(header: &[u8], at: u32, delta: i64) -> DimEdit {
    let Some(dim_start) = find_element(header, b"dimension", 0) else {
        return DimEdit::None;
    };
    let Some(gt) = memchr::memchr(b'>', &header[dim_start..]) else {
        return DimEdit::None;
    };
    let gt = dim_start + gt;
    let tag = &header[dim_start..=gt];
    let Some(rp) = memchr::memmem::find(tag, b"ref=\"") else {
        return DimEdit::None;
    };
    let vs = rp + 5;
    let Some(vl) = memchr::memchr(b'"', &tag[vs..]) else {
        return DimEdit::None;
    };
    let ve = vs + vl;
    let val = &tag[vs..ve];
    let Some(val_str) = std::str::from_utf8(val).ok() else {
        return DimEdit::None;
    };
    let parts: Vec<&str> = val_str.split(':').map(str::trim).collect();
    if parts.is_empty() || parts.len() > 2 {
        return DimEdit::None;
    }
    let mut spans = Vec::with_capacity(parts.len());
    let mut mapped = Vec::with_capacity(parts.len());
    for ep in &parts {
        let Some((ds, de)) = col_letters_span(ep.as_bytes()) else {
            return DimEdit::None;
        };
        let Some(old) = letters_to_col(&ep.as_bytes()[ds..de]) else {
            return DimEdit::None;
        };
        spans.push((ds, de));
        mapped.push(shifted_col(old, at, delta));
    }
    let (ns, ne, empty_all) = match (parts.len(), mapped[0], mapped.get(1).copied().flatten()) {
        (1, Some(c), _) => (c, c, false),
        (1, None, _) => (1, 1, true),
        (_, Some(a), Some(b)) => (a.min(b), a.max(b), false),
        (_, None, Some(b)) => (at, b, false),
        (_, Some(a), None) => (a, at.saturating_sub(1), false),
        (_, None, None) => (1, 1, true),
    };
    // CLAMP, mirroring `update_dimension` on the row axis — see the full
    // reasoning there. `<dimension>` is an advisory bounding box that writers
    // routinely over-declare and Excel recomputes on load, so refusing an
    // otherwise-valid operation because the DECLARED right edge would pass XFD
    // is a false refusal. The column splice has already refused if real cell
    // content would leave the grid, so only empty declared space is clamped.
    //
    // Kept symmetric with the row axis on purpose: divergence between these two
    // paths has already produced two separate bugs in this file.
    let ns = ns.min(MAX_COL);
    let ne = ne.min(MAX_COL);
    let new_val: Vec<u8> = if empty_all || ns == 0 || ne < ns {
        b"A1".to_vec()
    } else {
        let new_cols = [ns, ne];
        let mut v = Vec::with_capacity(val.len() + 8);
        for (i, ep) in parts.iter().enumerate() {
            if i > 0 {
                v.push(b':');
            }
            let (_, de) = spans[i];
            let mut buf = [0u8; 4];
            v.extend_from_slice(col_letters(new_cols[i], &mut buf));
            v.extend_from_slice(&ep.as_bytes()[de..]);
        }
        v
    };
    if new_val == val {
        return DimEdit::None;
    }
    let abs_vs = dim_start + vs;
    let abs_ve = dim_start + ve;
    DimEdit::Replaced {
        start: abs_vs,
        end: abs_ve,
        bytes: new_val,
    }
}

fn rewrite_cols(header: &[u8], at: u32, delta: i64) -> ColsSpanEdit {
    let cs = match find_element(header, b"cols", 0) {
        Some(c) => c,
        None => return ColsSpanEdit::None,
    };
    let gt = match memchr::memchr(b'>', &header[cs..]) {
        Some(g) => cs + g,
        None => return ColsSpanEdit::None,
    };
    if gt > cs && header[gt - 1] == b'/' {
        return ColsSpanEdit::None;
    }
    let ce = match memchr::memmem::find(&header[gt + 1..], b"</cols>") {
        Some(o) => gt + 1 + o,
        None => return ColsSpanEdit::None,
    };
    let inner = &header[gt + 1..ce];

    let mut out: Vec<u8> = Vec::with_capacity(inner.len() + 32);
    let mut pos = 0usize;
    let mut changed = false;
    while let Some(sp) = find_element(inner, b"col", pos) {
        let st = match memchr::memchr(b'>', &inner[sp..]) {
            Some(s) => sp + s,
            None => return ColsSpanEdit::Refused,
        };
        let self_closing = st > sp && inner[st - 1] == b'/';
        let end = if self_closing {
            st + 1
        } else {
            match memchr::memmem::find(&inner[st + 1..], b"</col>") {
                Some(rc) => st + 1 + rc + 6,
                None => inner.len(),
            }
        };
        let tag = &inner[sp..=st];
        let m0 = match extract_xml_attr(tag, b"min").and_then(|s| s.parse::<u32>().ok()) {
            Some(m) => m,
            None => return ColsSpanEdit::Refused,
        };
        let m1 = match extract_xml_attr(tag, b"max").and_then(|s| s.parse::<u32>().ok()) {
            Some(m) => m,
            None => return ColsSpanEdit::Refused,
        };

        out.extend_from_slice(&inner[pos..sp]);

        if delta > 0 {
            if m1 < at {
                out.extend_from_slice(&inner[sp..end]);
            } else if m0 >= at {
                let n0 = m0 + delta as u32;
                let n1 = m1 + delta as u32;
                if n1 > MAX_COL {
                    return ColsSpanEdit::Refused;
                }
                rewrite_col_tag(&mut out, &inner[sp..end], n0, n1);
                changed = true;
            } else {
                let n1 = m1 + delta as u32;
                if n1 > MAX_COL {
                    return ColsSpanEdit::Refused;
                }
                rewrite_col_tag(&mut out, &inner[sp..end], m0, n1);
                changed = true;
            }
        } else {
            let count = (-delta) as u32;
            let de = at + count;
            if m1 < at {
                out.extend_from_slice(&inner[sp..end]);
            } else if m0 >= de {
                rewrite_col_tag(&mut out, &inner[sp..end], m0 - count, m1 - count);
                changed = true;
            } else if m0 >= at && m1 < de {
                // Span falls entirely inside the deleted run.
                changed = true;
            } else if m0 < at && m1 < de {
                rewrite_col_tag(&mut out, &inner[sp..end], m0, at - 1);
                changed = true;
            } else if m0 >= at && m1 >= de {
                rewrite_col_tag(&mut out, &inner[sp..end], at, m1 - count);
                changed = true;
            } else {
                // Span straddles the deleted run: split into two.
                rewrite_col_tag(&mut out, &inner[sp..end], m0, at - 1);
                rewrite_col_tag(&mut out, &inner[sp..end], at, m1 - count);
                changed = true;
            }
        }
        pos = end;
    }
    out.extend_from_slice(&inner[pos..]);
    if !changed {
        return ColsSpanEdit::None;
    }
    ColsSpanEdit::Replaced {
        start: gt + 1,
        end: ce,
        bytes: out,
    }
}

fn would_orphan_shared_cols(body: &[u8], at: u32, count: u64) -> bool {
    let end = at as u64 + count;
    for (_si, group) in scan_shared_groups_cols(body) {
        if group.masters.is_empty() {
            continue;
        }
        let all_masters_deleted = group
            .masters
            .iter()
            .all(|&c| (c as u64) >= at as u64 && (c as u64) < end);
        if !all_masters_deleted {
            continue;
        }
        let any_member_survives = group
            .members
            .iter()
            .any(|&c| (c as u64) < at as u64 || (c as u64) >= end);
        if any_member_survives {
            return true;
        }
    }
    false
}

fn scan_shared_groups_cols(body: &[u8]) -> AHashMap<u32, SharedGroup> {
    let mut map: AHashMap<u32, SharedGroup> = AHashMap::new();
    let mut pos = 0usize;
    while let Some(row_start) = find_element(body, b"row", pos) {
        let Some(gt_rel) = memchr::memchr(b'>', &body[row_start..]) else {
            break;
        };
        let tag_end = row_start + gt_rel;
        let row_self_closing = tag_end > row_start && body[tag_end - 1] == b'/';
        let row_end = if row_self_closing {
            tag_end + 1
        } else {
            match memchr::memmem::find(&body[tag_end + 1..], b"</row>") {
                Some(rc) => tag_end + 1 + rc + 6,
                None => body.len(),
            }
        };
        if !row_self_closing {
            let mut cpos = tag_end + 1;
            while cpos < row_end {
                let Some(cs) = find_element(body, b"c", cpos) else {
                    break;
                };
                if cs >= row_end {
                    break;
                }
                let Some(cgt) = memchr::memchr(b'>', &body[cs..]) else {
                    break;
                };
                let ctag_end = cs + cgt;
                let cself = ctag_end > cs && body[ctag_end - 1] == b'/';
                let cend = if cself {
                    ctag_end + 1
                } else {
                    match memchr::memmem::find(&body[ctag_end + 1..], b"</c>") {
                        Some(ce) => ctag_end + 1 + ce + 4,
                        None => ctag_end + 1,
                    }
                };
                let cell_end = cend.min(row_end);
                if let Some(fs) = find_element(&body[cs..cell_end], b"f", 0) {
                    let Some(fgt) = memchr::memchr(b'>', &body[cs..cell_end][fs..]) else {
                        cpos = cend;
                        continue;
                    };
                    let ftag = &body[cs..cell_end][fs..fs + fgt + 1];
                    if memchr::memmem::find(ftag, b"t=\"shared\"").is_some() {
                        let si = extract_xml_attr(ftag, b"si")
                            .and_then(|s| s.parse::<u32>().ok())
                            .unwrap_or(0);
                        let has_ref = memchr::memmem::find(ftag, b"ref=\"").is_some();
                        let col = extract_xml_attr(&body[cs..=ctag_end], b"r")
                            .and_then(|s| col_from_ref_bytes(s.as_bytes()))
                            .unwrap_or(0);
                        let group = map.entry(si).or_insert(SharedGroup {
                            masters: Vec::new(),
                            members: Vec::new(),
                        });
                        group.members.push(col);
                        if has_ref {
                            group.masters.push(col);
                        }
                    }
                }
                cpos = cend;
            }
        }
        pos = row_end;
    }
    map
}

fn update_dimension(header: &[u8], at: u32, delta: i64) -> HeaderEdit {
    let Some(dim_start) = find_element(header, b"dimension", 0) else {
        return HeaderEdit::Unchanged;
    };
    let Some(gt) = memchr::memchr(b'>', &header[dim_start..]) else {
        return HeaderEdit::Unchanged;
    };
    let gt = dim_start + gt;
    let tag = &header[dim_start..=gt];
    let Some(rp) = memchr::memmem::find(tag, b"ref=\"") else {
        return HeaderEdit::Unchanged;
    };
    let vs = rp + 5;
    let Some(vl) = memchr::memchr(b'"', &tag[vs..]) else {
        return HeaderEdit::Unchanged;
    };
    let ve = vs + vl;
    let val = &tag[vs..ve];
    let Some(val_str) = std::str::from_utf8(val).ok() else {
        return HeaderEdit::Unchanged;
    };
    let parts: Vec<&str> = val_str.split(':').map(str::trim).collect();
    if parts.is_empty() || parts.len() > 2 {
        return HeaderEdit::Unchanged;
    }
    let mut spans = Vec::with_capacity(parts.len());
    let mut mapped = Vec::with_capacity(parts.len());
    for ep in &parts {
        let Some((ds, de)) = trailing_digits_span(ep.as_bytes()) else {
            return HeaderEdit::Unchanged;
        };
        let Some(old) = ep[ds..de].parse::<u32>().ok() else {
            return HeaderEdit::Unchanged;
        };
        spans.push((ds, de));
        mapped.push(shifted_row(old, at, delta));
    }
    let (ns, ne, empty_all) = match (parts.len(), mapped[0], mapped.get(1).copied().flatten()) {
        (1, Some(r), _) => (r, r, false),
        (1, None, _) => (1, 1, true),
        (_, Some(a), Some(b)) => (a.min(b), a.max(b), false),
        (_, None, Some(b)) => (at, b, false),
        (_, Some(a), None) => (a, at.saturating_sub(1), false),
        (_, None, None) => (1, 1, true),
    };
    // CLAMP, do not refuse — and this is deliberately different from how a
    // shared `ref=` is treated a few hundred lines up.
    //
    // `<dimension>` is an ADVISORY bounding box, not a coordinate that owns
    // data. Writers routinely over-declare it: emitting `A1:A1048576` for a
    // sheet holding ten rows is common, and Excel recomputes the value on load
    // regardless of what we write.
    //
    // Refusing here was measured to break real files. A sheet declaring the
    // full grid height with content at row 5 refused a one-row insert at row 2,
    // even though the content lands at row 6 — nowhere near the boundary. That
    // is a false refusal on a legitimate workbook, which is worse than an
    // imprecise advisory value.
    //
    // Clamping is SAFE here because of ordering: the splice has already refused
    // if any real `<row>` would pass MAX_ROW (see the guard in `splice_body`).
    // So by the time we get here, all actual content is known to fit, and the
    // only thing being clamped is a declared bound over empty space.
    //
    // The cost is that insert-then-delete is not byte-identical when the
    // dimension was pinned at the grid edge: it clamps on the way out and
    // un-clamps one lower on the way back. The ST-1 harness carves this case
    // out explicitly rather than treating it as a defect.
    let ns = ns.min(MAX_ROW as u32);
    let ne = ne.min(MAX_ROW as u32);
    let new_val: Vec<u8> = if empty_all || ns == 0 || ne < ns {
        b"A1".to_vec()
    } else {
        let new_rows = [ns, ne];
        let mut v = Vec::with_capacity(val.len() + 8);
        for (i, ep) in parts.iter().enumerate() {
            if i > 0 {
                v.push(b':');
            }
            let (ds, _) = spans[i];
            v.extend_from_slice(&ep.as_bytes()[..ds]);
            write_u32(&mut v, new_rows[i]);
        }
        v
    };
    if new_val == val {
        return HeaderEdit::Unchanged;
    }
    let abs_vs = dim_start + vs;
    let abs_ve = dim_start + ve;
    let mut out = Vec::with_capacity(header.len() + 8);
    out.extend_from_slice(&header[..abs_vs]);
    out.extend_from_slice(&new_val);
    out.extend_from_slice(&header[abs_ve..]);
    HeaderEdit::Changed(out)
}

fn would_orphan_shared(body: &[u8], at: u32, count: u64) -> bool {
    let end = at as u64 + count;
    for (_si, group) in scan_shared_groups(body) {
        if group.masters.is_empty() {
            continue;
        }
        let all_masters_deleted = group
            .masters
            .iter()
            .all(|&r| (r as u64) >= at as u64 && (r as u64) < end);
        if !all_masters_deleted {
            continue;
        }
        let any_member_survives = group
            .members
            .iter()
            .any(|&r| (r as u64) < at as u64 || (r as u64) >= end);
        if any_member_survives {
            return true;
        }
    }
    false
}

struct SharedGroup {
    masters: Vec<u32>,
    members: Vec<u32>,
}

fn scan_shared_groups(body: &[u8]) -> AHashMap<u32, SharedGroup> {
    let mut map: AHashMap<u32, SharedGroup> = AHashMap::new();
    let mut pos = 0usize;
    let mut seq_row = 0u32;
    while let Some(row_start) = find_element(body, b"row", pos) {
        let Some(gt_rel) = memchr::memchr(b'>', &body[row_start..]) else {
            break;
        };
        let tag_end = row_start + gt_rel;
        let row_tag = &body[row_start..=tag_end];
        let row_idx = extract_xml_attr(row_tag, b"r")
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(seq_row + 1);
        seq_row = row_idx;
        let row_self_closing = tag_end > row_start && body[tag_end - 1] == b'/';
        let row_end = if row_self_closing {
            tag_end + 1
        } else {
            match memchr::memmem::find(&body[tag_end + 1..], b"</row>") {
                Some(rc) => tag_end + 1 + rc + 6,
                None => body.len(),
            }
        };
        if !row_self_closing {
            let mut cpos = tag_end + 1;
            while cpos < row_end {
                let Some(cs) = find_element(body, b"c", cpos) else {
                    break;
                };
                if cs >= row_end {
                    break;
                }
                let Some(cgt) = memchr::memchr(b'>', &body[cs..]) else {
                    break;
                };
                let ctag_end = cs + cgt;
                let cself = ctag_end > cs && body[ctag_end - 1] == b'/';
                let cend = if cself {
                    ctag_end + 1
                } else {
                    match memchr::memmem::find(&body[ctag_end + 1..], b"</c>") {
                        Some(ce) => ctag_end + 1 + ce + 4,
                        None => ctag_end + 1,
                    }
                };
                let cell_end = cend.min(row_end);
                if let Some(fs) = find_element(&body[cs..cell_end], b"f", 0) {
                    let Some(fgt) = memchr::memchr(b'>', &body[cs..cell_end][fs..]) else {
                        cpos = cend;
                        continue;
                    };
                    let ftag = &body[cs..cell_end][fs..fs + fgt + 1];
                    if memchr::memmem::find(ftag, b"t=\"shared\"").is_some() {
                        let si = extract_xml_attr(ftag, b"si")
                            .and_then(|s| s.parse::<u32>().ok())
                            .unwrap_or(0);
                        let has_ref = memchr::memmem::find(ftag, b"ref=\"").is_some();
                        let group = map.entry(si).or_insert(SharedGroup {
                            masters: Vec::new(),
                            members: Vec::new(),
                        });
                        group.members.push(row_idx);
                        if has_ref {
                            group.masters.push(row_idx);
                        }
                    }
                }
                cpos = cend;
            }
        }
        pos = row_end;
    }
    map
}

fn trailing_digits_span(s: &[u8]) -> Option<(usize, usize)> {
    let mut e = s.len();
    while e > 0 && s[e - 1].is_ascii_digit() {
        e -= 1;
    }
    if e == s.len() {
        None
    } else {
        Some((e, s.len()))
    }
}

/// Byte span of ` attr="VALUE"` inside a tag body. `pub(crate)` for the fixup pass.
pub(crate) fn attr_value_span(tag: &[u8], attr: &[u8]) -> Option<(usize, usize)> {
    let mut search = Vec::with_capacity(attr.len() + 2);
    search.extend_from_slice(attr);
    search.extend_from_slice(b"=\"");
    let o = memchr::memmem::find(tag, &search)?;
    let vs = o + search.len();
    let q = memchr::memchr(b'"', &tag[vs..])?;
    Some((vs, vs + q))
}

/// Byte span of the leading column letters of an A1 ref (`"B12"` → `(0, 1)`),
/// tolerating a leading `$` for absolute refs.
fn col_letters_span(s: &[u8]) -> Option<(usize, usize)> {
    let mut start = 0usize;
    while start < s.len() && s[start] == b'$' {
        start += 1;
    }
    let mut e = start;
    while e < s.len() && s[e].is_ascii_alphabetic() {
        e += 1;
    }
    if e == start { None } else { Some((start, e)) }
}

/// Column letters → 1-based column index (`"AA"` → 27).
fn letters_to_col(letters: &[u8]) -> Option<u32> {
    let mut idx = 0u32;
    for &b in letters {
        if !b.is_ascii_alphabetic() {
            return None;
        }
        let val = (b.to_ascii_uppercase() - b'A' + 1) as u32;
        idx = idx.checked_mul(26)?.checked_add(val)?;
    }
    if idx == 0 || idx > MAX_COL {
        None
    } else {
        Some(idx)
    }
}

/// Leading column letters of a cell ref (`"B12"` → 2, 1-based).
fn col_from_ref_bytes(bytes: &[u8]) -> Option<u32> {
    let (_, de) = col_letters_span(bytes)?;
    letters_to_col(&bytes[..de])
}

/// Remove `attr="value"` plus the space before it from an opening tag.
/// Returns the byte span to delete, or `None` when the attribute is absent.
fn drop_attr(tag: &[u8], attr: &[u8]) -> Option<(usize, usize)> {
    let mut needle = Vec::with_capacity(attr.len() + 2);
    needle.extend_from_slice(attr);
    needle.extend_from_slice(b"=\"");
    let mut p = 0usize;
    while let Some(off) = memchr::memmem::find(&tag[p..], &needle) {
        let s = p + off;
        let preceded = s == 0 || tag[s - 1].is_ascii_whitespace() || tag[s - 1] == b'<';
        if preceded {
            let vs = s + needle.len();
            let q = memchr::memchr(b'"', &tag[vs..])?;
            let end = vs + q + 1;
            let start = if s > 0 && tag[s - 1].is_ascii_whitespace() {
                s - 1
            } else {
                s
            };
            return Some((start, end));
        }
        p = s + needle.len();
    }
    None
}

/// Rewrite the `min`/`max` attributes of a `<col>` tag, preserving every other
/// attribute and the tag's self-closing form.
fn rewrite_col_tag(out: &mut Vec<u8>, tag: &[u8], n0: u32, n1: u32) {
    let mut pieces = tag.to_vec();
    if let Some((s, e)) = drop_attr(&pieces, b"min") {
        pieces.drain(s..e);
    }
    if let Some((s, e)) = drop_attr(&pieces, b"max") {
        pieces.drain(s..e);
    }
    let mut attr = Vec::with_capacity(24);
    attr.push(b' ');
    attr.extend_from_slice(b"min=\"");
    write_u32(&mut attr, n0);
    attr.extend_from_slice(b"\" max=\"");
    write_u32(&mut attr, n1);
    attr.push(b'"');
    pieces.splice(4..4, attr);
    out.extend_from_slice(&pieces);
}

// ----------------------------------------------------------------------------
// move_range — relocate a rectangular block without shifting the rest of the grid.
// ----------------------------------------------------------------------------
//
// Unlike insert/delete, moving a range relocates content: every cell in the
// source rectangle is re-emitted at `+ (rows, cols)`, the vacated source cells
// become empty, and destination cells are overwritten (an empty source position
// clears the destination position too — the destination is a full imprint of the
// source, matching openpyxl). Everything else on the sheet is untouched.
//
// The whole move is computed against the ORIGINAL grid: source cells are
// buffered up front, so an overlapping move never reads a cell it has already
// overwritten. Only cells inside `source ∪ destination` are held; rows outside
// the band flow through as untouched byte runs, so moving a small range in a
// large sheet costs O(band), not O(sheet).
//
// Refusal contract: `None` means the move is refused rather than performed
// incorrectly. Refusal cases are a missing/empty `<sheetData>`, an
// implicit-numbered row or cell whose position lies in the source or
// destination rectangle, a destination corner that leaves the grid
// (all-or-nothing: nothing is written), and a shared-formula `ref=` that would
// leave the grid.

/// Relocate the rectangle `(r1,c1)..(r2,c2)` (1-based, inclusive) by `rows`,
/// `cols` (signed), per openpyxl `ws.move_range` semantics.
///
/// `translate` controls whether formula bodies INSIDE the moved range are
/// translated by the same offset (`true`, via `formula::translate_body` — the
/// same translator openpyxl uses) or left byte-for-byte (`false`, the default).
///
/// Returns `Cow::Borrowed` when nothing changes (`rows == 0 && cols == 0`, or no
/// cell or anchor actually moves), `Cow::Owned` with the relocated bytes
/// otherwise, and `None` when the move is refused (see module note).
#[allow(clippy::too_many_arguments)]
pub fn move_range<'a>(
    xml: &'a [u8],
    r1: u32,
    c1: u32,
    r2: u32,
    c2: u32,
    rows: i64,
    cols: i64,
    translate: bool,
) -> Option<Cow<'a, [u8]>> {
    let (r1, r2) = if r1 <= r2 { (r1, r2) } else { (r2, r1) };
    let (c1, c2) = if c1 <= c2 { (c1, c2) } else { (c2, c1) };
    if r1 == 0 || c1 == 0 || r2 as u64 > MAX_ROW || c2 > MAX_COL {
        return None;
    }
    if rows == 0 && cols == 0 {
        return Some(Cow::Borrowed(xml));
    }
    let dr1 = (r1 as i64).checked_add(rows)?;
    let dr2 = (r2 as i64).checked_add(rows)?;
    let dc1 = (c1 as i64).checked_add(cols)?;
    let dc2 = (c2 as i64).checked_add(cols)?;
    if dr1 < 1 || dc1 < 1 || dr2 > MAX_ROW as i64 || dc2 > MAX_COL as i64 {
        return None;
    }
    let (dr1, dc1, dr2, dc2) = (dr1 as u32, dc1 as u32, dr2 as u32, dc2 as u32);

    let sd_start = find_element(xml, b"sheetData", 0)?;
    let gt = sd_start + memchr::memchr(b'>', &xml[sd_start..])?;
    let self_closing = gt > sd_start && xml[gt - 1] == b'/';
    if self_closing {
        return None;
    }
    let body_start = gt + 1;
    let close = body_start + memchr::memmem::find(&xml[body_start..], b"</sheetData>")?;
    let body = &xml[body_start..close];
    find_element(body, b"row", 0)?;

    let rband_min = r1.min(dr1);
    let rband_max = r2.max(dr2);
    let moved = prescan_move_body(
        body, r1, c1, r2, c2, dr1, dc1, dr2, dc2, rband_min, rband_max,
    )?;

    let body_edit = splice_move_body(
        body, &moved, r1, c1, r2, c2, dr1, dc1, dr2, dc2, rows, cols, translate,
    )?;
    let header_edit = update_dimension_move(&xml[..body_start], dr1, dc1, dr2, dc2);

    let header_out: Cow<'_, [u8]> = match header_edit {
        HeaderEdit::Unchanged => Cow::Borrowed(&xml[..body_start]),
        HeaderEdit::Changed(h) => Cow::Owned(h),
        HeaderEdit::Refused => return None,
    };

    let tail = &xml[close..];
    let mut tail_out: Cow<'_, [u8]> = Cow::Borrowed(tail);
    if let Some(nt) = move_tail_fixups(tail, r1, c1, r2, c2, rows, cols) {
        tail_out = Cow::Owned(nt);
    }

    let body_changed = matches!(body_edit, Cow::Owned(_));
    let header_changed = matches!(header_out, Cow::Owned(_));
    let tail_changed = matches!(tail_out, Cow::Owned(_));
    if !body_changed && !header_changed && !tail_changed {
        return Some(Cow::Borrowed(xml));
    }

    let mut out = Vec::with_capacity(xml.len() + 32);
    out.extend_from_slice(&header_out);
    out.extend_from_slice(&body_edit);
    out.extend_from_slice(&tail_out);
    Some(Cow::Owned(out))
}

/// One cell captured from the source rectangle, ready to be re-emitted at its
/// translated position.
struct MovedCell {
    row: u32,
    col: u32,
    /// The raw `<c>...</c>` bytes; `r=` and formula bodies are rewritten at
    /// emission time.
    bytes: Vec<u8>,
}

/// Scan the body once: buffer every cell inside the source rectangle and refuse
/// on an implicit-numbered row/cell whose position lies in `source ∪ destination`.
#[allow(clippy::too_many_arguments)]
fn prescan_move_body(
    body: &[u8],
    r1: u32,
    c1: u32,
    r2: u32,
    c2: u32,
    dr1: u32,
    dc1: u32,
    dr2: u32,
    dc2: u32,
    rband_min: u32,
    rband_max: u32,
) -> Option<Vec<MovedCell>> {
    let mut moved: Vec<MovedCell> = Vec::new();
    let mut pos = 0usize;
    let mut seq_row = 0u32;
    while let Some(row_start) = find_element(body, b"row", pos) {
        let gt_rel = memchr::memchr(b'>', &body[row_start..])?;
        let tag_end = row_start + gt_rel;
        let row_tag = &body[row_start..=tag_end];
        let r_attr = extract_xml_attr(row_tag, b"r");
        let row_idx = r_attr
            .as_ref()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(seq_row + 1);
        seq_row = row_idx;
        let row_self_closing = tag_end > row_start && body[tag_end - 1] == b'/';
        let row_end = if row_self_closing {
            tag_end + 1
        } else {
            match memchr::memmem::find(&body[tag_end + 1..], b"</row>") {
                Some(rc) => tag_end + 1 + rc + 6,
                None => body.len(),
            }
        };

        if row_idx >= rband_min && row_idx <= rband_max {
            let in_src_rows = row_idx >= r1 && row_idx <= r2;
            let in_dest_rows = row_idx >= dr1 && row_idx <= dr2;
            if r_attr.is_none() && (in_src_rows || in_dest_rows) {
                return None;
            }
            if !row_self_closing {
                let row_inner = &body[tag_end + 1..row_end];
                let mut seq_col = 0u32;
                let mut cpos = 0usize;
                while let Some(cs) = find_element(row_inner, b"c", cpos) {
                    let cgt_rel = memchr::memchr(b'>', &row_inner[cs..])?;
                    let ctag_end = cs + cgt_rel;
                    let cself = ctag_end > cs && row_inner[ctag_end - 1] == b'/';
                    let cend = if cself {
                        ctag_end + 1
                    } else {
                        match memchr::memmem::find(&row_inner[ctag_end + 1..], b"</c>") {
                            Some(ce) => ctag_end + 1 + ce + 4,
                            None => row_inner.len(),
                        }
                    };
                    let ctag = &row_inner[cs..=ctag_end];
                    let c_attr = extract_xml_attr(ctag, b"r");
                    let col = c_attr
                        .as_ref()
                        .and_then(|s| col_from_ref_bytes(s.as_bytes()))
                        .unwrap_or(seq_col + 1);
                    seq_col = col;
                    let in_src = in_src_rows && col >= c1 && col <= c2;
                    let in_dest = in_dest_rows && col >= dc1 && col <= dc2;
                    if c_attr.is_none() && (in_src || in_dest) {
                        return None;
                    }
                    if in_src {
                        moved.push(MovedCell {
                            row: row_idx,
                            col,
                            bytes: row_inner[cs..cend].to_vec(),
                        });
                    }
                    cpos = cend;
                }
            }
        }
        pos = row_end;
    }
    Some(moved)
}

/// One forward pass over the body: rows outside `source ∪ destination` stay in
/// untouched byte runs; rows inside the band are rebuilt, and moved cells land
/// at their translated positions (in column order within each row). Returns
/// `Cow::Borrowed` when nothing changed.
#[allow(clippy::too_many_arguments)]
fn splice_move_body<'a>(
    body: &'a [u8],
    moved: &[MovedCell],
    r1: u32,
    c1: u32,
    r2: u32,
    c2: u32,
    dr1: u32,
    dc1: u32,
    dr2: u32,
    dc2: u32,
    rows: i64,
    cols: i64,
    translate: bool,
) -> Option<Cow<'a, [u8]>> {
    let mut out: Vec<u8> = Vec::new();
    let mut pos = 0usize;
    let mut seq_row = 0u32;
    let mut prefix_written = false;
    let mut last_end = 0usize;
    let mut any_change = false;
    // Destination rows already emitted into an existing `<row>` in the body.
    // Moved cells whose destination row is NOT in this set get brand-new rows
    // after the loop (their destination fell in a gap or past the last row).
    let mut emitted_dest_rows: AHashSet<u32> = AHashSet::new();

    while let Some(row_start) = find_element(body, b"row", pos) {
        let gt_rel = memchr::memchr(b'>', &body[row_start..])?;
        let tag_end = row_start + gt_rel;
        let row_tag = &body[row_start..=tag_end];
        let row_idx = extract_xml_attr(row_tag, b"r")
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(seq_row + 1);
        seq_row = row_idx;
        let row_self_closing = tag_end > row_start && body[tag_end - 1] == b'/';
        let row_end = if row_self_closing {
            tag_end + 1
        } else {
            match memchr::memmem::find(&body[tag_end + 1..], b"</row>") {
                Some(rc) => tag_end + 1 + rc + 6,
                None => body.len(),
            }
        };

        let is_src_row = row_idx >= r1 && row_idx <= r2;
        let is_dest_row = row_idx >= dr1 && row_idx <= dr2;
        if !is_src_row && !is_dest_row {
            pos = row_end;
            continue;
        }

        if !prefix_written {
            out.extend_from_slice(&body[0..row_start]);
            prefix_written = true;
        } else {
            out.extend_from_slice(&body[last_end..row_start]);
        }

        // Moved cells destined for this row (source row == row_idx - rows).
        // `moved` is sorted by source row, so for a fixed destination row every
        // match comes from a single source row and stays in column order.
        let here: Vec<&MovedCell> = if is_dest_row {
            let src_row = (row_idx as i64).wrapping_sub(rows);
            let mut v: Vec<&MovedCell> = Vec::new();
            for m in moved {
                if (m.row as i64) == src_row {
                    v.push(m);
                } else if (m.row as i64) > src_row {
                    break;
                }
            }
            emitted_dest_rows.insert(row_idx);
            v
        } else {
            Vec::new()
        };

        let changed = rebuild_row(
            &mut out,
            &body[row_start..row_end],
            row_idx,
            &here,
            rows,
            cols,
            translate,
            r1,
            c1,
            r2,
            c2,
            dr1,
            dc1,
            dr2,
            dc2,
        )?;
        if changed {
            any_change = true;
        }
        last_end = row_end;
        pos = row_end;
    }

    // Any moved cell whose destination row does not exist in the body (the
    // destination fell in a gap, or past the last row) becomes a brand-new row.
    if emitted_dest_rows.len() < moved.len() {
        let new_rows: Vec<&MovedCell> = moved
            .iter()
            .filter(|m| !emitted_dest_rows.contains(&((m.row as i64 + rows) as u32)))
            .collect();
        if !new_rows.is_empty() {
            if !prefix_written {
                out.extend_from_slice(&body[..body.len()]);
                prefix_written = true;
            } else {
                out.extend_from_slice(&body[last_end..]);
                last_end = body.len();
            }
            let mut k = 0usize;
            while k < new_rows.len() {
                let dest_row = (new_rows[k].row as i64 + rows) as u32;
                out.extend_from_slice(b"<row r=\"");
                write_u32(&mut out, dest_row);
                out.extend_from_slice(b"\">");
                while k < new_rows.len() && (new_rows[k].row as i64 + rows) as u32 == dest_row {
                    let dest_col = (new_rows[k].col as i64 + cols) as u32;
                    emit_moved_cell(
                        &mut out,
                        new_rows[k],
                        dest_row,
                        dest_col,
                        translate,
                        rows,
                        cols,
                    )?;
                    k += 1;
                }
                out.extend_from_slice(b"</row>");
            }
            any_change = true;
        }
    }

    if !prefix_written {
        return Some(Cow::Borrowed(body));
    }
    out.extend_from_slice(&body[last_end..]);
    if any_change {
        Some(Cow::Owned(out))
    } else {
        Some(Cow::Borrowed(body))
    }
}

/// Rebuild one row that lies in `source ∪ destination`: vacate source cells,
/// drop overwritten destination cells, and splice in the moved cells that land
/// here, keeping the row in strict column order. Returns `true` when the row's
/// bytes actually changed.
#[allow(clippy::too_many_arguments)]
fn rebuild_row(
    out: &mut Vec<u8>,
    row: &[u8],
    row_idx: u32,
    here: &[&MovedCell],
    rows: i64,
    cols: i64,
    translate: bool,
    r1: u32,
    c1: u32,
    r2: u32,
    c2: u32,
    dr1: u32,
    dc1: u32,
    dr2: u32,
    dc2: u32,
) -> Option<bool> {
    let gt_rel = memchr::memchr(b'>', row)?;
    let tag_end = gt_rel;
    let tag = &row[..=tag_end];
    let self_closing = tag_end > 0 && row[tag_end - 1] == b'/';
    let close_at = if row.ends_with(b"</row>") {
        row.len() - 6
    } else {
        row.len()
    };
    let body = &row[tag_end + 1..close_at];

    let src_rows = row_idx >= r1 && row_idx <= r2;
    let dest_rows = row_idx >= dr1 && row_idx <= dr2;

    // Cheap pre-check: does any cell in this row lie in the source or the
    // destination rectangle? Only then do the row's bytes change (and only then
    // may the cached `spans` attribute be dropped).
    let mut has_cell_change = false;
    if !self_closing {
        let mut cpos = 0usize;
        let mut seq_col = 0u32;
        while let Some(cs) = find_element(body, b"c", cpos) {
            let Some(cgt_rel) = memchr::memchr(b'>', &body[cs..]) else {
                break;
            };
            let ctag_end = cs + cgt_rel;
            let cself = ctag_end > cs && body[ctag_end - 1] == b'/';
            let cend = if cself {
                ctag_end + 1
            } else {
                match memchr::memmem::find(&body[ctag_end + 1..], b"</c>") {
                    Some(ce) => ctag_end + 1 + ce + 4,
                    None => body.len(),
                }
            };
            let col = extract_xml_attr(&body[cs..=ctag_end], b"r")
                .and_then(|s| col_from_ref_bytes(s.as_bytes()))
                .unwrap_or(seq_col + 1);
            seq_col = col;
            if (src_rows && col >= c1 && col <= c2) || (dest_rows && col >= dc1 && col <= dc2) {
                has_cell_change = true;
                break;
            }
            cpos = cend;
        }
    }

    if !has_cell_change && here.is_empty() {
        out.extend_from_slice(row);
        return Some(false);
    }

    // Open tag, dropping the cached `spans` attribute (Excel recomputes it).
    let mut open = tag.to_vec();
    if let Some((vs, ve)) = drop_attr(&open, b"spans") {
        open.drain(vs..ve);
    }
    if self_closing {
        // Expand `<row .../>` to `<row ...>` so moved cells can land here.
        open.pop();
    }
    out.extend_from_slice(&open);

    let mut cpos = 0usize;
    let mut hi = 0usize;
    let mut seq_col = 0u32;
    while let Some(cs) = find_element(body, b"c", cpos) {
        let Some(cgt_rel) = memchr::memchr(b'>', &body[cs..]) else {
            break;
        };
        let ctag_end = cs + cgt_rel;
        let cself = ctag_end > cs && body[ctag_end - 1] == b'/';
        let cend = if cself {
            ctag_end + 1
        } else {
            match memchr::memmem::find(&body[ctag_end + 1..], b"</c>") {
                Some(ce) => ctag_end + 1 + ce + 4,
                None => body.len(),
            }
        };
        let col = extract_xml_attr(&body[cs..=ctag_end], b"r")
            .and_then(|s| col_from_ref_bytes(s.as_bytes()))
            .unwrap_or(seq_col + 1);
        seq_col = col;

        // Emit any moved cells whose translated column sorts before this cell.
        while hi < here.len() {
            let dc = (here[hi].col as i64 + cols) as u32;
            if dc >= col {
                break;
            }
            emit_moved_cell(out, here[hi], row_idx, dc, translate, rows, cols)?;
            hi += 1;
        }

        let in_src = src_rows && col >= c1 && col <= c2;
        let in_dest = dest_rows && col >= dc1 && col <= dc2;
        if in_src || in_dest {
            // Vacated (source) or overwritten (destination): drop the original cell.
            out.extend_from_slice(&body[cpos..cs]);
            cpos = cend;
            continue;
        }
        out.extend_from_slice(&body[cpos..cend]);
        cpos = cend;
    }

    // Any remaining moved cells sort after every original cell in this row.
    while hi < here.len() {
        let dc = (here[hi].col as i64 + cols) as u32;
        emit_moved_cell(out, here[hi], row_idx, dc, translate, rows, cols)?;
        hi += 1;
    }

    if self_closing {
        out.extend_from_slice(b"</row>");
    } else {
        out.extend_from_slice(&body[cpos..]);
        out.extend_from_slice(&row[close_at..]);
    }
    Some(true)
}

/// Re-emit a moved cell at `(dest_row, dest_col)`: rewrite its `r=` attribute,
/// and when `translate` is set, translate its formula body and shared `ref=`.
fn emit_moved_cell(
    out: &mut Vec<u8>,
    mc: &MovedCell,
    dest_row: u32,
    dest_col: u32,
    translate: bool,
    rows: i64,
    cols: i64,
) -> Option<()> {
    let bytes = &mc.bytes;
    let gt_rel = memchr::memchr(b'>', bytes)?;
    let tag_end = gt_rel;
    let tag = &bytes[..=tag_end];
    let self_closing = tag_end > 0 && bytes[tag_end - 1] == b'/';
    if let Some((vs, ve)) = attr_value_span(tag, b"r") {
        out.extend_from_slice(&tag[..vs]);
        let mut buf = [0u8; 4];
        out.extend_from_slice(col_letters(dest_col, &mut buf));
        write_u32(out, dest_row);
        out.extend_from_slice(&tag[ve..]);
    } else {
        out.extend_from_slice(tag);
    }
    if self_closing {
        return Some(());
    }
    let close_at = if bytes.ends_with(b"</c>") {
        bytes.len() - 4
    } else {
        bytes.len()
    };
    let cell_body = &bytes[tag_end + 1..close_at];
    if translate {
        rewrite_cell_f_body_move(out, cell_body, rows, cols)?;
    } else {
        out.extend_from_slice(cell_body);
    }
    out.extend_from_slice(&bytes[close_at..]);
    Some(())
}

/// Rewrite one cell's `<f>` element for a translated move: the shared `ref=`
/// attribute follows the offset and the body text goes through
/// `formula::translate_body` (the same translator openpyxl uses for
/// `move_range(..., translate=True)`). Returns `None` when a shared `ref=`
/// would leave the grid — the whole move must refuse.
fn rewrite_cell_f_body_move(out: &mut Vec<u8>, body: &[u8], rows: i64, cols: i64) -> Option<()> {
    if let Some(fs) = find_element(body, b"f", 0) {
        out.extend_from_slice(&body[..fs]);
        let fend = match memchr::memchr(b'>', &body[fs..]) {
            Some(gt_rel) if body[fs + gt_rel - 1] == b'/' => fs + gt_rel + 1,
            _ => match memchr::memmem::find(&body[fs..], b"</f>") {
                Some(off) => fs + off + 4,
                None => body.len(),
            },
        };
        rewrite_f_element_move(out, &body[fs..fend], rows, cols)?;
        out.extend_from_slice(&body[fend..]);
    } else {
        out.extend_from_slice(body);
    }
    Some(())
}

fn rewrite_f_element_move(out: &mut Vec<u8>, fspan: &[u8], rows: i64, cols: i64) -> Option<()> {
    let Some(gt_rel) = memchr::memchr(b'>', fspan) else {
        out.extend_from_slice(fspan);
        return Some(());
    };
    let tag = &fspan[..=gt_rel];
    let self_closing = gt_rel > 0 && fspan[gt_rel - 1] == b'/';
    if memchr::memmem::find(tag, b"t=\"shared\"").is_some() {
        if let Some(rp) = memchr::memmem::find(tag, b"ref=\"") {
            let vs = rp + 5;
            if let Some(vl) = memchr::memchr(b'"', &tag[vs..]) {
                let ve = vs + vl;
                match translate_shared_ref(&tag[vs..ve], rows, cols) {
                    MoveRefShift::OutOfGrid => return None,
                    MoveRefShift::Unchanged => out.extend_from_slice(tag),
                    MoveRefShift::Changed(new_val) => {
                        out.extend_from_slice(&tag[..vs]);
                        out.extend_from_slice(&new_val);
                        out.extend_from_slice(&tag[ve..]);
                    }
                }
            } else {
                out.extend_from_slice(tag);
            }
        } else {
            out.extend_from_slice(tag);
        }
    } else {
        out.extend_from_slice(tag);
    }
    if self_closing {
        return Some(());
    }
    let after = &fspan[gt_rel + 1..];
    let close_len = usize::from(after.ends_with(b"</f>")) * 4;
    let body = &after[..after.len() - close_len];
    if !body.is_empty() {
        if let Ok(s) = std::str::from_utf8(body) {
            let translated = translate_body(s, rows as i32, cols as i32);
            out.extend_from_slice(translated.as_bytes());
        } else {
            out.extend_from_slice(body);
        }
    }
    if close_len > 0 {
        out.extend_from_slice(&after[after.len() - close_len..]);
    }
    Some(())
}

enum MoveRefShift {
    Unchanged,
    Changed(Vec<u8>),
    OutOfGrid,
}

/// Translate a shared-formula `ref=` value ("A1" or "A1:B3") by `(rows, cols)`.
/// `OutOfGrid` when any endpoint would leave the grid.
fn translate_shared_ref(val: &[u8], rows: i64, cols: i64) -> MoveRefShift {
    let mut new = Vec::with_capacity(val.len() + 8);
    let mut changed = false;
    let mut start = 0usize;
    let mut parts = 0usize;
    for i in 0..=val.len() {
        if i == val.len() || val[i] == b':' {
            if parts > 0 {
                new.push(b':');
            }
            match shift_ref_endpoint(&val[start..i], rows, cols, &mut new) {
                Ok(c) => changed |= c,
                Err(()) => return MoveRefShift::OutOfGrid,
            }
            parts += 1;
            start = i + 1;
        }
    }
    if parts == 0 || parts > 2 {
        return MoveRefShift::Unchanged;
    }
    if changed {
        MoveRefShift::Changed(new)
    } else {
        MoveRefShift::Unchanged
    }
}

/// Shift one endpoint of a shared `ref=` (tolerating `$` markers) by `(rows,
/// cols)`, appending the rebuilt bytes. `Err(())` when the endpoint leaves the
/// grid. Returns whether anything changed.
fn shift_ref_endpoint(ep: &[u8], rows: i64, cols: i64, out: &mut Vec<u8>) -> Result<bool, ()> {
    let (ds, _de) = trailing_digits_span(ep).ok_or(())?;
    let row: u32 = std::str::from_utf8(&ep[ds..])
        .ok()
        .ok_or(())?
        .parse()
        .ok()
        .ok_or(())?;
    let (cs, ce) = col_letters_span(&ep[..ds]).ok_or(())?;
    let col = letters_to_col(&ep[cs..ce]).ok_or(())?;
    let nrow = row as i64 + rows;
    let ncol = col as i64 + cols;
    if nrow < 1 || nrow > MAX_ROW as i64 || ncol < 1 || ncol > MAX_COL as i64 {
        return Err(());
    }
    let mut changed = false;
    out.extend_from_slice(&ep[..cs]);
    let mut buf = [0u8; 4];
    let letters = col_letters(ncol as u32, &mut buf);
    if letters != &ep[cs..ce] {
        changed = true;
    }
    out.extend_from_slice(letters);
    out.extend_from_slice(&ep[ce..ds]);
    if nrow as u32 != row {
        changed = true;
    }
    write_u32(out, nrow as u32);
    Ok(changed)
}

/// Parse `"A1"` or `"A1:B3"` (with optional `$`) into a normalized 1-based
/// rectangle `(r1,c1,r2,c2)`. `None` for whole-row/whole-column or malformed refs.
fn parse_dim_ref(v: &[u8]) -> Option<(u32, u32, u32, u32)> {
    let s = std::str::from_utf8(v).ok()?;
    let mut it = s.split(':');
    let a = parse_cell_ref_bytes(it.next()?.trim().as_bytes())?;
    match it.next() {
        Some(b) => {
            let b = parse_cell_ref_bytes(b.trim().as_bytes())?;
            Some((a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1)))
        }
        None => Some((a.0, a.1, a.0, a.1)),
    }
}

fn parse_cell_ref_bytes(v: &[u8]) -> Option<(u32, u32)> {
    let (ds, _) = trailing_digits_span(v)?;
    let row: u32 = std::str::from_utf8(&v[ds..]).ok()?.parse().ok()?;
    let (cs, ce) = col_letters_span(&v[..ds])?;
    let col = letters_to_col(&v[cs..ce])?;
    Some((row, col))
}

/// Serialize a 1-based rectangle as an A1 range; single cell collapses to one ref.
fn serialize_dim(r1: u32, c1: u32, r2: u32, c2: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity(12);
    let mut buf = [0u8; 4];
    v.extend_from_slice(col_letters(c1, &mut buf));
    write_u32(&mut v, r1);
    if r1 != r2 || c1 != c2 {
        v.push(b':');
        v.extend_from_slice(col_letters(c2, &mut buf));
        write_u32(&mut v, r2);
    }
    v
}

/// Widen `<dimension ref>` to cover the destination rectangle (advisory; Excel
/// recomputes it on load, so widening-only is always safe).
fn update_dimension_move(header: &[u8], dr1: u32, dc1: u32, dr2: u32, dc2: u32) -> HeaderEdit {
    let Some(dim_start) = find_element(header, b"dimension", 0) else {
        return HeaderEdit::Unchanged;
    };
    let Some(gt) = memchr::memchr(b'>', &header[dim_start..]) else {
        return HeaderEdit::Unchanged;
    };
    let gt = dim_start + gt;
    let tag = &header[dim_start..=gt];
    let Some(rp) = memchr::memmem::find(tag, b"ref=\"") else {
        return HeaderEdit::Unchanged;
    };
    let vs = rp + 5;
    let Some(vl) = memchr::memchr(b'"', &tag[vs..]) else {
        return HeaderEdit::Unchanged;
    };
    let ve = vs + vl;
    let val = &tag[vs..ve];
    let Some((r0, c0, r1, c1)) = parse_dim_ref(val) else {
        return HeaderEdit::Unchanged;
    };
    let nr0 = r0.min(dr1).max(1);
    let nc0 = c0.min(dc1).max(1);
    let nr1 = r1.max(dr2).min(MAX_ROW as u32);
    let nc1 = c1.max(dc2).min(MAX_COL);
    let new_val = serialize_dim(nr0, nc0, nr1, nc1);
    if new_val == val {
        return HeaderEdit::Unchanged;
    }
    let abs_vs = dim_start + vs;
    let abs_ve = dim_start + ve;
    let mut out = Vec::with_capacity(header.len() + 8);
    out.extend_from_slice(&header[..abs_vs]);
    out.extend_from_slice(&new_val);
    out.extend_from_slice(&header[abs_ve..]);
    HeaderEdit::Changed(out)
}

// ----------------------------------------------------------------------------
// move_range tail fixups — anchors anchored inside the moved block follow it.
// ----------------------------------------------------------------------------
//
// Decision (consistent across merges, hyperlinks, data validations and
// conditional formatting): an anchor range that is FULLY CONTAINED in the moved
// source rectangle is translated by the same `(rows, cols)` offset. An anchor
// that straddles the boundary is left untouched — its meaning is ambiguous, and
// keeping it is the conservative choice that never corrupts the range. No
// asymmetry between the features: they share one code path.

enum RangeMove {
    Unchanged,
    Changed(Vec<u8>),
}

/// Translate `val` by `(rows, cols)` iff it is fully contained in the moved
/// rectangle.
fn translate_contained(
    val: &[u8],
    r1: u32,
    c1: u32,
    r2: u32,
    c2: u32,
    rows: i64,
    cols: i64,
) -> RangeMove {
    let Some((a0, b0, a1, b1)) = parse_dim_ref(val) else {
        return RangeMove::Unchanged;
    };
    if a0 < r1 || b0 < c1 || a1 > r2 || b1 > c2 {
        return RangeMove::Unchanged;
    }
    let new = serialize_dim(
        (a0 as i64 + rows) as u32,
        (b0 as i64 + cols) as u32,
        (a1 as i64 + rows) as u32,
        (b1 as i64 + cols) as u32,
    );
    if new == val {
        RangeMove::Unchanged
    } else {
        RangeMove::Changed(new)
    }
}

fn closing_tag_mv(name: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(name.len() + 3);
    v.extend_from_slice(b"</");
    v.extend_from_slice(name);
    v.push(b'>');
    v
}

/// Rewrite every `<name>` element of `tail` through `f` (which returns the new
/// element bytes, or `None` to keep). Returns `None` when nothing changed.
fn rewrite_elements_mv(
    tail: &[u8],
    name: &[u8],
    f: &mut dyn FnMut(&[u8]) -> Option<Vec<u8>>,
) -> Option<Vec<u8>> {
    let close = closing_tag_mv(name);
    let mut out: Vec<u8> = Vec::new();
    let mut pos = 0usize;
    let mut changed = false;
    let mut started = false;
    let mut last = 0usize;
    while let Some(rel) = find_element(&tail[pos..], name, 0) {
        let s = pos + rel;
        let Some(gt_rel) = memchr::memchr(b'>', &tail[s..]) else {
            break;
        };
        let tag_end = s + gt_rel;
        let self_close = tag_end > s && tail[tag_end - 1] == b'/';
        let end = if self_close {
            tag_end + 1
        } else {
            let Some(close_rel) = memchr::memmem::find(&tail[tag_end..], &close) else {
                break;
            };
            tag_end + close_rel + close.len()
        };
        let elem = &tail[s..end];
        if !started {
            out.extend_from_slice(&tail[..s]);
            started = true;
        } else {
            out.extend_from_slice(&tail[last..s]);
        }
        if let Some(n) = f(elem) {
            out.extend_from_slice(&n);
            changed = true;
        } else {
            out.extend_from_slice(elem);
        }
        last = end;
        pos = end;
    }
    if !started {
        return None;
    }
    out.extend_from_slice(&tail[last..]);
    if changed { Some(out) } else { None }
}

fn apply_mv<'a>(cur: &mut Cow<'a, [u8]>, f: impl Fn(&[u8]) -> Option<Vec<u8>>) {
    if let Some(next) = f(cur.as_ref()) {
        *cur = Cow::Owned(next);
    }
}

/// Translate every anchor anchored inside the moved block.
fn move_tail_fixups(
    tail: &[u8],
    r1: u32,
    c1: u32,
    r2: u32,
    c2: u32,
    rows: i64,
    cols: i64,
) -> Option<Vec<u8>> {
    let mut cur: Cow<'_, [u8]> = Cow::Borrowed(tail);
    apply_mv(&mut cur, |b| {
        fixup_merge_cells_move(b, r1, c1, r2, c2, rows, cols)
    });
    apply_mv(&mut cur, |b| {
        fixup_hyperlinks_move(b, r1, c1, r2, c2, rows, cols)
    });
    apply_mv(&mut cur, |b| {
        fixup_data_validations_move(b, r1, c1, r2, c2, rows, cols)
    });
    apply_mv(&mut cur, |b| {
        fixup_conditional_formatting_move(b, r1, c1, r2, c2, rows, cols)
    });
    match cur {
        Cow::Owned(o) => Some(o),
        Cow::Borrowed(_) => None,
    }
}

fn fixup_merge_cells_move(
    tail: &[u8],
    r1: u32,
    c1: u32,
    r2: u32,
    c2: u32,
    rows: i64,
    cols: i64,
) -> Option<Vec<u8>> {
    let s = find_element(tail, b"mergeCells", 0)?;
    let gt_rel = memchr::memchr(b'>', &tail[s..])?;
    let gt = s + gt_rel;
    if gt > s && tail[gt - 1] == b'/' {
        return None;
    }
    let close_tag = b"</mergeCells>";
    let close_rel = memchr::memmem::find(&tail[gt..], close_tag)?;
    let close = gt + close_rel;
    let block = &tail[gt + 1..close];

    let mut survivors: Vec<Vec<u8>> = Vec::new();
    let mut changed = false;
    let mut pos = 0usize;
    while let Some(ms) = find_element(block, b"mergeCell", pos) {
        let mte_rel = memchr::memchr(b'>', &block[ms..])?;
        let mte = ms + mte_rel;
        let self_close = mte > ms && block[mte - 1] == b'/';
        let mend = if self_close {
            mte + 1
        } else {
            let cr = memchr::memmem::find(&block[mte..], b"</mergeCell>")?;
            mte + cr + b"</mergeCell>".len()
        };
        let mcell = &block[ms..mend];
        pos = mend;
        if let Some(new_bytes) = translate_merge_ref(mcell, r1, c1, r2, c2, rows, cols) {
            survivors.push(new_bytes);
            changed = true;
        } else {
            survivors.push(mcell.to_vec());
        }
    }

    if !changed {
        return None;
    }
    // Translation preserves every merge's shape, so the `count` attribute stays
    // correct; the open tag is kept verbatim.
    let mut new_block = Vec::with_capacity(block.len() + 8);
    new_block.extend_from_slice(&tail[s..=gt]);
    for surv in &survivors {
        new_block.extend_from_slice(surv);
    }
    new_block.extend_from_slice(close_tag);
    let mut out = Vec::with_capacity(tail.len() + 8);
    out.extend_from_slice(&tail[..s]);
    out.extend_from_slice(&new_block);
    out.extend_from_slice(&tail[close + close_tag.len()..]);
    Some(out)
}

fn translate_merge_ref(
    mcell: &[u8],
    r1: u32,
    c1: u32,
    r2: u32,
    c2: u32,
    rows: i64,
    cols: i64,
) -> Option<Vec<u8>> {
    let tag_end = memchr::memchr(b'>', mcell)?;
    let tag = &mcell[..=tag_end];
    let (vs, ve) = attr_value_span(tag, b"ref")?;
    let val = &tag[vs..ve];
    match translate_contained(val, r1, c1, r2, c2, rows, cols) {
        RangeMove::Changed(new_val) => {
            let mut out = Vec::with_capacity(mcell.len() + new_val.len());
            out.extend_from_slice(&mcell[..vs]);
            out.extend_from_slice(&new_val);
            out.extend_from_slice(&mcell[ve..]);
            Some(out)
        }
        RangeMove::Unchanged => None,
    }
}

fn fixup_hyperlinks_move(
    tail: &[u8],
    r1: u32,
    c1: u32,
    r2: u32,
    c2: u32,
    rows: i64,
    cols: i64,
) -> Option<Vec<u8>> {
    rewrite_elements_mv(tail, b"hyperlink", &mut |e| {
        rewrite_range_attr_move(e, r1, c1, r2, c2, rows, cols)
    })
}

fn rewrite_range_attr_move(
    elem: &[u8],
    r1: u32,
    c1: u32,
    r2: u32,
    c2: u32,
    rows: i64,
    cols: i64,
) -> Option<Vec<u8>> {
    let tag_end = memchr::memchr(b'>', elem)?;
    let tag = &elem[..=tag_end];
    let (vs, ve) = attr_value_span(tag, b"ref")?;
    let val = &tag[vs..ve];
    match translate_contained(val, r1, c1, r2, c2, rows, cols) {
        RangeMove::Changed(new_val) => {
            let mut out = Vec::with_capacity(elem.len() + new_val.len());
            out.extend_from_slice(&elem[..vs]);
            out.extend_from_slice(&new_val);
            out.extend_from_slice(&elem[ve..]);
            Some(out)
        }
        RangeMove::Unchanged => None,
    }
}

fn fixup_data_validations_move(
    tail: &[u8],
    r1: u32,
    c1: u32,
    r2: u32,
    c2: u32,
    rows: i64,
    cols: i64,
) -> Option<Vec<u8>> {
    rewrite_elements_mv(tail, b"dataValidation", &mut |elem| {
        let tag_end = memchr::memchr(b'>', elem)?;
        let tag = &elem[..=tag_end];
        let mut new_elem = elem.to_vec();
        let mut changed = false;
        if let Some((vs, ve)) = attr_value_span(tag, b"sqref") {
            let val = &tag[vs..ve];
            if let Some(shifted) = shift_sqref_move(val, r1, c1, r2, c2, rows, cols) {
                let mut t = Vec::with_capacity(new_elem.len() + shifted.len());
                t.extend_from_slice(&new_elem[..vs]);
                t.extend_from_slice(&shifted);
                t.extend_from_slice(&new_elem[ve..]);
                new_elem = t;
                changed = true;
            }
        }
        if changed { Some(new_elem) } else { None }
    })
}

fn fixup_conditional_formatting_move(
    tail: &[u8],
    r1: u32,
    c1: u32,
    r2: u32,
    c2: u32,
    rows: i64,
    cols: i64,
) -> Option<Vec<u8>> {
    rewrite_elements_mv(tail, b"conditionalFormatting", &mut |elem| {
        let tag_end = memchr::memchr(b'>', elem)?;
        let tag = &elem[..=tag_end];
        let mut out = elem.to_vec();
        let mut changed = false;
        if let Some((vs, ve)) = attr_value_span(tag, b"sqref") {
            let val = &tag[vs..ve];
            if let Some(shifted) = shift_sqref_move(val, r1, c1, r2, c2, rows, cols) {
                let mut t = Vec::with_capacity(out.len() + shifted.len());
                t.extend_from_slice(&out[..vs]);
                t.extend_from_slice(&shifted);
                t.extend_from_slice(&out[ve..]);
                out = t;
                changed = true;
            }
        }
        if changed { Some(out) } else { None }
    })
}

/// Translate every fully-contained range in a space-separated sqref value.
fn shift_sqref_move(
    val: &[u8],
    r1: u32,
    c1: u32,
    r2: u32,
    c2: u32,
    rows: i64,
    cols: i64,
) -> Option<Vec<u8>> {
    let mut kept: Vec<Vec<u8>> = Vec::new();
    let mut changed = false;
    for token in val.split(|&b| b == b' ') {
        if token.is_empty() {
            continue;
        }
        match translate_contained(token, r1, c1, r2, c2, rows, cols) {
            RangeMove::Unchanged => kept.push(token.to_vec()),
            RangeMove::Changed(r) => {
                kept.push(r);
                changed = true;
            }
        }
    }
    if !changed {
        return None;
    }
    let mut out = Vec::with_capacity(val.len() + 8);
    for (i, k) in kept.iter().enumerate() {
        if i > 0 {
            out.push(b' ');
        }
        out.extend_from_slice(k);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turbo::SheetOverlay;
    use crate::turbo::overlay::splice_sheet_xml;
    use crate::turbo::write::model::CellValue;
    use pretty_assertions::assert_eq;

    fn xml(dim: &str, rows: &str) -> String {
        format!(
            r#"<?xml version="1.0"?><worksheet xmlns="s"><dimension ref="{dim}"/><sheetData>{rows}</sheetData></worksheet>"#
        )
    }

    #[test]
    fn insert_shifts_rows_cells_and_dimension() {
        let s = xml(
            "A1:D3",
            r#"<row r="1"><c r="A1"><v>a</v></c></row><row r="2"><c r="A2"><v>b</v></c></row><row r="3"><c r="A3"><v>c</v></c></row>"#,
        );
        let out = shift_rows(s.as_bytes(), 2, 2).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(out.contains(r#"<dimension ref="A1:D5"/>"#), "{out}");
        assert!(
            out.contains(r#"<row r="1"><c r="A1"><v>a</v></c></row>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<row r="4"><c r="A4"><v>b</v></c></row>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<row r="5"><c r="A5"><v>c</v></c></row>"#),
            "{out}"
        );
        assert!(!out.contains(r#"<row r="2">"#), "{out}");
        assert!(!out.contains(r#"<row r="3">"#), "{out}");
    }

    #[test]
    fn insert_at_row_one_shifts_everything() {
        let s = xml(
            "A1:B2",
            r#"<row r="1"><c r="A1"><v>1</v></c></row><row r="2"><c r="B2"><v>2</v></c></row>"#,
        );
        let out = shift_rows(s.as_bytes(), 1, 3).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(out.contains(r#"<dimension ref="A4:B5"/>"#), "{out}");
        assert!(
            out.contains(r#"<row r="4"><c r="A4"><v>1</v></c></row>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<row r="5"><c r="B5"><v>2</v></c></row>"#),
            "{out}"
        );
    }

    #[test]
    fn insert_past_last_row_borrows() {
        let s = xml(
            "A1:B2",
            r#"<row r="1"><c r="A1"><v>1</v></c></row><row r="2"><c r="A2"><v>2</v></c></row>"#,
        );
        let out = shift_rows(s.as_bytes(), 5, 3).unwrap();
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn insert_zero_borrows() {
        let s = xml(
            "A1:A2",
            r#"<row r="1"><c r="A1"><v>1</v></c></row><row r="2"><c r="A2"><v>2</v></c></row>"#,
        );
        let out = shift_rows(s.as_bytes(), 2, 0).unwrap();
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn delete_middle_shifts_rows_up() {
        let s = xml(
            "A1:D4",
            r#"<row r="1"><c r="A1"><v>1</v></c></row><row r="2"><c r="A2"><v>2</v></c></row><row r="3"><c r="A3"><v>3</v></c></row><row r="4"><c r="A4"><v>4</v></c></row>"#,
        );
        let out = delete_rows(s.as_bytes(), 2, 1).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(out.contains(r#"<dimension ref="A1:D3"/>"#), "{out}");
        assert!(
            out.contains(r#"<row r="1"><c r="A1"><v>1</v></c></row>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<row r="2"><c r="A2"><v>3</v></c></row>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<row r="3"><c r="A3"><v>4</v></c></row>"#),
            "{out}"
        );
        assert!(!out.contains("<v>2</v>"), "{out}");
        assert!(!out.contains(r#"<row r="4">"#), "{out}");
    }

    #[test]
    fn delete_past_last_row_borrows() {
        let s = xml(
            "A1:A2",
            r#"<row r="1"><c r="A1"><v>1</v></c></row><row r="2"><c r="A2"><v>2</v></c></row>"#,
        );
        let out = delete_rows(s.as_bytes(), 10, 3).unwrap();
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn delete_whole_grid_sets_dimension_a1() {
        let s = xml(
            "A1:D3",
            r#"<row r="1"><c r="A1"><v>1</v></c></row><row r="2"><c r="A2"><v>2</v></c></row><row r="3"><c r="A3"><v>3</v></c></row>"#,
        );
        let out = delete_rows(s.as_bytes(), 1, 3).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(out.contains(r#"<dimension ref="A1"/>"#), "{out}");
        assert!(out.contains(r#"<sheetData></sheetData>"#), "{out}");
    }

    #[test]
    fn delete_overlapping_bottom_shrinks_dimension() {
        let s = xml(
            "A1:D5",
            r#"<row r="1"><c r="A1"><v>1</v></c></row><row r="2"><c r="A2"><v>2</v></c></row><row r="3"><c r="A3"><v>3</v></c></row><row r="4"><c r="A4"><v>4</v></c></row><row r="5"><c r="A5"><v>5</v></c></row>"#,
        );
        let out = delete_rows(s.as_bytes(), 4, 2).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(out.contains(r#"<dimension ref="A1:D3"/>"#), "{out}");
        assert!(!out.contains(r#"<row r="4">"#), "{out}");
        assert!(!out.contains(r#"<row r="5">"#), "{out}");
    }

    #[test]
    fn row_attributes_survive_shift() {
        let s = xml(
            "A1:B2",
            r#"<row r="1" ht="15" customHeight="1" spans="1:1"><c r="A1" s="3"><v>1</v></c></row>"#,
        );
        let out = shift_rows(s.as_bytes(), 1, 1).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            out.contains(r#"<row r="2" ht="15" customHeight="1" spans="1:1">"#),
            "{out}"
        );
        assert!(out.contains(r#"<c r="A2" s="3"><v>1</v></c>"#), "{out}");
    }

    #[test]
    fn digit_length_change() {
        let s = xml(
            "A1:A10",
            r#"<row r="9"><c r="A9"><v>9</v></c></row><row r="10"><c r="A10"><v>10</v></c></row>"#,
        );
        let out = shift_rows(s.as_bytes(), 1, 1).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            out.contains(r#"<row r="10"><c r="A10"><v>9</v></c></row>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<row r="11"><c r="A11"><v>10</v></c></row>"#),
            "{out}"
        );

        let s = xml(
            "A1:A99999",
            r#"<row r="99998"><c r="A99998"><v>x</v></c></row><row r="99999"><c r="A99999"><v>y</v></c></row>"#,
        );
        let out = shift_rows(s.as_bytes(), 99998, 1).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(out.contains(r#"<row r="99999">"#), "{out}");
        assert!(out.contains(r#"<row r="100000">"#), "{out}");
    }

    #[test]
    fn self_closing_and_empty_rows_in_delete() {
        let s = xml(
            "A1:A3",
            r#"<row r="1"/><row r="2"><c r="A2"><v>2</v></c></row><row r="3"></row>"#,
        );
        let out = delete_rows(s.as_bytes(), 2, 1).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(out.contains(r#"<row r="1"/>"#), "{out}");
        assert!(out.contains(r#"<row r="2"></row>"#), "{out}");
        assert!(!out.contains("<v>2</v>"), "{out}");
    }

    #[test]
    fn empty_sheet_refuses() {
        assert!(shift_rows(b"<worksheet><sheetData/></worksheet>", 1, 1).is_none());
        assert!(shift_rows(b"<worksheet><sheetData></sheetData></worksheet>", 1, 1).is_none());
        assert!(shift_rows(b"<worksheet></worksheet>", 1, 1).is_none());
    }

    #[test]
    fn implicit_row_at_or_below_shift_point_refuses() {
        let s = xml(
            "A1:A2",
            r#"<row><c r="A1"><v>1</v></c></row><row r="2"><c r="A2"><v>2</v></c></row>"#,
        );
        assert!(shift_rows(s.as_bytes(), 1, 1).is_none());
        assert!(delete_rows(s.as_bytes(), 1, 1).is_none());
        let ok = shift_rows(s.as_bytes(), 2, 1).unwrap();
        assert!(matches!(ok, Cow::Owned(_)));
    }

    #[test]
    fn insert_past_1048576_refuses() {
        let s = xml(
            "A1048576:A1048576",
            r#"<row r="1048576"><c r="A1048576"><v>x</v></c></row>"#,
        );
        assert!(shift_rows(s.as_bytes(), 1048576, 1).is_none());
        let ok = xml(
            "A1048572:A1048572",
            r#"<row r="1048572"><c r="A1048572"><v>x</v></c></row>"#,
        );
        assert!(shift_rows(ok.as_bytes(), 1048570, 3).is_some());
    }

    #[test]
    fn formula_body_shifts_with_rows() {
        let s = xml(
            "A1:B2",
            r#"<row r="2"><c r="B2"><f>SUM(A1:A5)</f><v>15</v></c></row>"#,
        );
        let out = shift_rows(s.as_bytes(), 1, 1).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            out.contains(r#"<c r="B3"><f>SUM(A2:A6)</f><v>15</v></c>"#),
            "{out}"
        );
    }

    #[test]
    fn missing_dimension_is_skipped() {
        let s = r#"<worksheet xmlns="s"><sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData></worksheet>"#;
        let out = shift_rows(s.as_bytes(), 1, 1).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            out.contains(r#"<row r="2"><c r="A2"><v>1</v></c></row>"#),
            "{out}"
        );
        assert!(!out.contains("dimension"), "{out}");
    }

    #[test]
    fn shared_formula_ref_shifts_with_rows() {
        let s = xml(
            "A1:A3",
            r#"<row r="1"><c r="A1" t="str"><f t="shared" ref="A1:A3" si="0">=A1+A2</f><v>3</v></c></row><row r="2"><c r="A2" t="str"><f t="shared" si="0"/></c></row><row r="3"><c r="A3" t="str"><f t="shared" si="0"/></c></row>"#,
        );
        let out = insert_rows(s.as_bytes(), 1, 1).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            out.contains(
                r#"<c r="A2" t="str"><f t="shared" ref="A2:A4" si="0">=A2+A3</f><v>3</v></c>"#
            ),
            "{out}"
        );
        assert!(
            out.contains(r#"<row r="3"><c r="A3" t="str"><f t="shared" si="0"/></c></row>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<row r="4"><c r="A4" t="str"><f t="shared" si="0"/></c></row>"#),
            "{out}"
        );
    }

    #[test]
    fn shared_formula_ref_shrinks_on_delete() {
        let s = xml(
            "A3:A6",
            r#"<row r="1"><c r="A1"><v>1</v></c></row><row r="2"><c r="A2"><v>2</v></c></row><row r="3"><c r="A3" t="str"><f t="shared" ref="A3:A6" si="0">=A3+A4</f><v>3</v></c></row><row r="4"><c r="A4" t="str"><f t="shared" si="0"/></c></row><row r="5"><c r="A5" t="str"><f t="shared" si="0"/></c></row><row r="6"><c r="A6" t="str"><f t="shared" si="0"/></c></row>"#,
        );
        let out = delete_rows(s.as_bytes(), 1, 2).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            out.contains(
                r#"<c r="A1" t="str"><f t="shared" ref="A1:A4" si="0">=A1+A2</f><v>3</v></c>"#
            ),
            "{out}"
        );
        assert!(
            out.contains(r#"<row r="2"><c r="A2" t="str"><f t="shared" si="0"/></c></row>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<row r="3"><c r="A3" t="str"><f t="shared" si="0"/></c></row>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<row r="4"><c r="A4" t="str"><f t="shared" si="0"/></c></row>"#),
            "{out}"
        );
        assert!(!out.contains(r#"<row r="5">"#), "{out}");
    }

    #[test]
    fn shared_ref_of_master_above_shift_point_shrinks_with_band() {
        let s = xml(
            "A1:A5",
            r#"<row r="1"><c r="A1" t="str"><f t="shared" ref="A1:A5" si="0">=A1+A2</f><v>3</v></c></row><row r="2"><c r="A2" t="str"><f t="shared" si="0"/></c></row><row r="3"><c r="A3" t="str"><f t="shared" si="0"/></c></row><row r="4"><c r="A4" t="str"><f t="shared" si="0"/></c></row><row r="5"><c r="A5" t="str"><f t="shared" si="0"/></c></row>"#,
        );
        let out = delete_rows(s.as_bytes(), 3, 2).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            out.contains(
                r#"<c r="A1" t="str"><f t="shared" ref="A1:A3" si="0">=A1+A2</f><v>3</v></c>"#
            ),
            "{out}"
        );
        assert!(
            out.contains(r#"<row r="2"><c r="A2" t="str"><f t="shared" si="0"/></c></row>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<row r="3"><c r="A3" t="str"><f t="shared" si="0"/></c></row>"#),
            "{out}"
        );
    }

    #[test]
    fn shared_formula_ref_with_deleted_start() {
        let s = xml(
            "A2:A6",
            r#"<row r="2"><c r="A2" t="str"><f t="shared" si="0"/></c></row><row r="3"><c r="A3" t="str"><f t="shared" si="0"/></c></row><row r="4"><c r="A4" t="str"><f t="shared" ref="A2:A6" si="0">=SUM(A2:A6)</f><v>5</v></c></row><row r="5"><c r="A5" t="str"><f t="shared" si="0"/></c></row><row r="6"><c r="A6" t="str"><f t="shared" si="0"/></c></row>"#,
        );
        let out = delete_rows(s.as_bytes(), 2, 2).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            out.contains(r#"<c r="A2" t="str"><f t="shared" ref="A2:A4" si="0">=SUM(#REF!:A4)</f><v>5</v></c>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<row r="3"><c r="A3" t="str"><f t="shared" si="0"/></c></row>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<row r="4"><c r="A4" t="str"><f t="shared" si="0"/></c></row>"#),
            "{out}"
        );
    }

    #[test]
    fn delete_orphaning_shared_master_refuses() {
        let s = xml(
            "A2:A5",
            r#"<row r="1"><c r="A1"><v>1</v></c></row><row r="2"><c r="A2" t="str"><f t="shared" ref="A2:A5" si="0">=A2+A3</f><v>5</v></c></row><row r="3"><c r="A3" t="str"><f t="shared" si="0"/></c></row><row r="4"><c r="A4" t="str"><f t="shared" si="0"/></c></row><row r="5"><c r="A5" t="str"><f t="shared" si="0"/></c></row>"#,
        );
        assert!(delete_rows(s.as_bytes(), 2, 1).is_none());
        let ok = delete_rows(s.as_bytes(), 3, 1).unwrap();
        assert!(matches!(ok, Cow::Owned(_)));
        let ok = delete_rows(s.as_bytes(), 2, 4).unwrap();
        assert!(matches!(ok, Cow::Owned(_)));
    }

    #[test]
    fn shared_single_cell_ref_shifts() {
        let s = xml(
            "A1:B2",
            r#"<row r="2"><c r="B2" t="str"><f t="shared" ref="B2" si="0">=B2</f><v>1</v></c></row>"#,
        );
        let out = insert_rows(s.as_bytes(), 1, 1).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            out.contains(r#"<c r="B3" t="str"><f t="shared" ref="B3" si="0">=B3</f><v>1</v></c>"#),
            "{out}"
        );
    }

    #[test]
    fn shared_master_below_insert_shifts_ref() {
        let s = xml(
            "A5:A7",
            r#"<row r="5"><c r="A5" t="str"><f t="shared" ref="A5:A7" si="0">=A5+A6</f><v>2</v></c></row><row r="6"><c r="A6" t="str"><f t="shared" si="0"/></c></row><row r="7"><c r="A7" t="str"><f t="shared" si="0"/></c></row>"#,
        );
        let out = insert_rows(s.as_bytes(), 3, 2).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            out.contains(
                r#"<c r="A7" t="str"><f t="shared" ref="A7:A9" si="0">=A7+A8</f><v>2</v></c>"#
            ),
            "{out}"
        );
        assert!(
            out.contains(r#"<row r="8"><c r="A8" t="str"><f t="shared" si="0"/></c></row>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<row r="9"><c r="A9" t="str"><f t="shared" si="0"/></c></row>"#),
            "{out}"
        );
    }

    #[test]
    fn composition_shift_then_overlay_edit() {
        let s = xml(
            "A1:A2",
            r#"<row r="1"><c r="A1"><v>1</v></c></row><row r="2"><c r="A2"><v>2</v></c></row>"#,
        );
        let shifted = shift_rows(s.as_bytes(), 1, 1).unwrap().into_owned();
        let mut overlay = SheetOverlay::default();
        overlay
            .modified_cells
            .insert((2, 1), CellValue::Str("edited".into()));
        overlay.is_dirty = true;
        let spliced = splice_sheet_xml(&shifted, &overlay, None).unwrap();
        let spliced = String::from_utf8(spliced).unwrap();
        assert!(
            spliced
                .contains(r#"<row r="2"><c r="A2" t="inlineStr"><is><t>edited</t></is></c></row>"#),
            "{spliced}"
        );
        assert!(
            spliced.contains(r#"<row r="3"><c r="A3"><v>2</v></c></row>"#),
            "{spliced}"
        );
        assert!(!spliced.contains("<v>1</v>"), "{spliced}");
    }

    // ------------------ column splice ------------------

    fn xml_cols(dim: &str, cols: &str, rows: &str) -> String {
        format!(
            r#"<?xml version="1.0"?><worksheet xmlns="s"><dimension ref="{dim}"/>{cols}<sheetData>{rows}</sheetData></worksheet>"#
        )
    }

    #[test]
    fn insert_shifts_cells_and_dimension_cols() {
        let s = xml(
            "A1:D3",
            r#"<row r="1"><c r="A1"><v>a</v></c><c r="B1"><v>b</v></c><c r="C1"><v>c</v></c><c r="D1"><v>d</v></c></row>"#,
        );
        let out = insert_cols(s.as_bytes(), 2, 2).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(out.contains(r#"<dimension ref="A1:F3"/>"#), "{out}");
        assert!(out.contains(r#"<c r="A1"><v>a</v></c>"#), "{out}");
        assert!(out.contains(r#"<c r="D1"><v>b</v></c>"#), "{out}");
        assert!(out.contains(r#"<c r="E1"><v>c</v></c>"#), "{out}");
        assert!(out.contains(r#"<c r="F1"><v>d</v></c>"#), "{out}");
    }

    #[test]
    fn insert_col_at_one_shifts_everything() {
        let s = xml(
            "A1:B1",
            r#"<row r="1"><c r="A1"><v>1</v></c><c r="B1"><v>2</v></c></row>"#,
        );
        let out = insert_cols(s.as_bytes(), 1, 3).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(out.contains(r#"<c r="D1"><v>1</v></c>"#), "{out}");
        assert!(out.contains(r#"<c r="E1"><v>2</v></c>"#), "{out}");
        assert!(out.contains(r#"<dimension ref="D1:E1"/>"#), "{out}");
    }

    #[test]
    fn z_to_aa_width_growth() {
        let s = xml("A1:Z1", r#"<row r="1"><c r="Z1"><v>z</v></c></row>"#);
        let out = insert_cols(s.as_bytes(), 26, 1).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(out.contains(r#"<c r="AA1"><v>z</v></c>"#), "{out}");
        assert!(out.contains(r#"<dimension ref="A1:AA1"/>"#), "{out}");
    }

    #[test]
    fn aa_back_to_z_width_shrink() {
        let s = xml("A1:AA1", r#"<row r="1"><c r="AA1"><v>aa</v></c></row>"#);
        let out = delete_cols(s.as_bytes(), 26, 1).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(out.contains(r#"<c r="Z1"><v>aa</v></c>"#), "{out}");
        assert!(out.contains(r#"<dimension ref="A1:Z1"/>"#), "{out}");
    }

    #[test]
    fn delete_middle_shifts_cells_left() {
        let s = xml(
            "A1:D1",
            r#"<row r="1"><c r="A1"><v>1</v></c><c r="B1"><v>2</v></c><c r="C1"><v>3</v></c><c r="D1"><v>4</v></c></row>"#,
        );
        let out = delete_cols(s.as_bytes(), 2, 1).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(out.contains(r#"<c r="A1"><v>1</v></c>"#), "{out}");
        assert!(out.contains(r#"<c r="B1"><v>3</v></c>"#), "{out}");
        assert!(out.contains(r#"<c r="C1"><v>4</v></c>"#), "{out}");
        assert!(!out.contains("<v>2</v>"), "{out}");
        assert!(out.contains(r#"<dimension ref="A1:C1"/>"#), "{out}");
    }

    #[test]
    fn delete_past_last_col_borrows() {
        let s = xml("A1:A1", r#"<row r="1"><c r="A1"><v>1</v></c></row>"#);
        let out = delete_cols(s.as_bytes(), 10, 3).unwrap();
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn insert_past_last_col_borrows() {
        let s = xml(
            "A1:B1",
            r#"<row r="1"><c r="A1"><v>1</v></c><c r="B1"><v>2</v></c></row>"#,
        );
        let out = insert_cols(s.as_bytes(), 5, 3).unwrap();
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn insert_zero_cols_borrows() {
        let s = xml("A1:A1", r#"<row r="1"><c r="A1"><v>1</v></c></row>"#);
        let out = insert_cols(s.as_bytes(), 2, 0).unwrap();
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn delete_whole_grid_sets_dimension_a1_cols() {
        let s = xml(
            "A1:C1",
            r#"<row r="1"><c r="A1"><v>1</v></c><c r="B1"><v>2</v></c><c r="C1"><v>3</v></c></row>"#,
        );
        let out = delete_cols(s.as_bytes(), 1, 3).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(out.contains(r#"<dimension ref="A1"/>"#), "{out}");
        assert!(out.contains(r#"<row r="1"></row>"#), "{out}");
    }

    #[test]
    fn delete_overlapping_right_shrinks_dimension() {
        let s = xml(
            "A1:E1",
            r#"<row r="1"><c r="A1"><v>1</v></c><c r="B1"><v>2</v></c><c r="C1"><v>3</v></c><c r="D1"><v>4</v></c><c r="E1"><v>5</v></c></row>"#,
        );
        let out = delete_cols(s.as_bytes(), 4, 2).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(out.contains(r#"<dimension ref="A1:C1"/>"#), "{out}");
        assert!(!out.contains("<v>4</v>"), "{out}");
        assert!(!out.contains("<v>5</v>"), "{out}");
    }

    #[test]
    fn cols_span_shifts_on_insert() {
        let s = xml_cols(
            "A1:C1",
            r#"<cols><col min="1" max="3" width="9.5"/></cols>"#,
            r#"<row r="1"><c r="C1"><v>1</v></c></row>"#,
        );
        let out = insert_cols(s.as_bytes(), 2, 1).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            out.contains(r#"<col min="1" max="4" width="9.5"/>"#),
            "{out}"
        );
        assert!(out.contains(r#"<c r="D1"><v>1</v></c>"#), "{out}");
    }

    #[test]
    fn cols_span_below_insert_is_untouched() {
        let s = xml_cols(
            "A1:B1",
            r#"<cols><col min="1" max="2" width="8" hidden="1"/></cols>"#,
            r#"<row r="1"><c r="B1"><v>1</v></c></row>"#,
        );
        let out = insert_cols(s.as_bytes(), 5, 1).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            out.contains(r#"<col min="1" max="2" width="8" hidden="1"/>"#),
            "{out}"
        );
    }

    #[test]
    fn cols_span_split_by_delete() {
        let s = xml_cols(
            "A1:E1",
            r#"<cols><col min="1" max="5" width="8"/></cols>"#,
            r#"<row r="1"><c r="E1"><v>1</v></c></row>"#,
        );
        let out = delete_cols(s.as_bytes(), 3, 1).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(out.contains(r#"<col min="1" max="2" width="8"/>"#), "{out}");
        assert!(out.contains(r#"<col min="3" max="4" width="8"/>"#), "{out}");
    }

    #[test]
    fn cols_span_removed_by_delete() {
        let s = xml_cols(
            "A1:B1",
            r#"<cols><col min="2" max="3" width="8"/></cols>"#,
            r#"<row r="1"><c r="B1"><v>1</v></c></row>"#,
        );
        let out = delete_cols(s.as_bytes(), 2, 2).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(out.contains(r#"<cols></cols>"#), "{out}");
    }

    #[test]
    fn insert_past_16384_refuses() {
        let s = xml("XFD1:XFD1", r#"<row r="1"><c r="XFD1"><v>x</v></c></row>"#);
        assert!(insert_cols(s.as_bytes(), 16384, 1).is_none());
        let ok = xml("XFZ1:XFZ1", r#"<row r="1"><c r="XFZ1"><v>x</v></c></row>"#);
        assert!(insert_cols(ok.as_bytes(), 16380, 4).is_some());
        let ok = xml("A1:A1", r#"<row r="1"><c r="A1"><v>x</v></c></row>"#);
        assert!(insert_cols(ok.as_bytes(), 1, 5).is_some());
    }

    #[test]
    fn row_with_no_cell_at_or_after_insert_is_byte_identical() {
        let row1 = r#"<row r="1" spans="1:2"><c r="A1"><v>a</v></c><c r="B1"><v>b</v></c></row>"#;
        let row2 = r#"<row r="2"><c r="E2"><v>e</v></c></row>"#;
        let s = xml("A1:E2", &format!("{row1}{row2}"));
        let out = insert_cols(s.as_bytes(), 3, 2).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(out.contains(row1), "{out}");
        assert!(
            out.contains(r#"<row r="2"><c r="G2"><v>e</v></c></row>"#),
            "{out}"
        );
    }

    #[test]
    fn row_attributes_survive_col_shift() {
        let s = xml(
            "A1:B1",
            r#"<row r="1" ht="15" customHeight="1" spans="1:2"><c r="B1" s="3"><v>1</v></c></row>"#,
        );
        let out = insert_cols(s.as_bytes(), 2, 1).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            out.contains(
                r#"<row r="1" ht="15" customHeight="1"><c r="C1" s="3"><v>1</v></c></row>"#
            ),
            "{out}"
        );
    }

    #[test]
    fn shared_formula_ref_cols_shifts() {
        let s = xml(
            "B1:D1",
            r#"<row r="1"><c r="B1" t="str"><f t="shared" ref="B1:D1" si="0">=B1+C1</f><v>3</v></c><c r="C1" t="str"><f t="shared" si="0"/></c><c r="D1" t="str"><f t="shared" si="0"/></c></row>"#,
        );
        let out = insert_cols(s.as_bytes(), 2, 1).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            out.contains(
                r#"<c r="C1" t="str"><f t="shared" ref="C1:E1" si="0">=C1+D1</f><v>3</v></c>"#
            ),
            "{out}"
        );
        assert!(
            out.contains(r#"<c r="D1" t="str"><f t="shared" si="0"/></c>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<c r="E1" t="str"><f t="shared" si="0"/></c>"#),
            "{out}"
        );
    }

    #[test]
    fn shared_formula_ref_cols_shrinks_on_delete() {
        let s = xml(
            "A1:C1",
            r#"<row r="1"><c r="A1" t="str"><f t="shared" si="0"/></c><c r="B1" t="str"><f t="shared" si="0"/></c><c r="C1" t="str"><f t="shared" ref="A1:C1" si="0">=A1+B1</f><v>3</v></c></row>"#,
        );
        let out = delete_cols(s.as_bytes(), 1, 1).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            out.contains(
                r#"<c r="B1" t="str"><f t="shared" ref="A1:B1" si="0">=#REF!+A1</f><v>3</v></c>"#
            ),
            "{out}"
        );
        assert!(
            out.contains(r#"<c r="A1" t="str"><f t="shared" si="0"/></c>"#),
            "{out}"
        );
    }

    #[test]
    fn shared_single_cell_ref_cols_shifts() {
        let s = xml(
            "B2:B2",
            r#"<row r="2"><c r="B2" t="str"><f t="shared" ref="B2" si="0">=B2</f><v>1</v></c></row>"#,
        );
        let out = insert_cols(s.as_bytes(), 2, 1).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            out.contains(r#"<c r="C2" t="str"><f t="shared" ref="C2" si="0">=C2</f><v>1</v></c>"#),
            "{out}"
        );
    }

    #[test]
    fn delete_orphaning_shared_master_cols_refuses() {
        let s = xml(
            "B1:C1",
            r#"<row r="1"><c r="B1" t="str"><f t="shared" ref="B1:C1" si="0">=B1+C1</f><v>3</v></c><c r="C1" t="str"><f t="shared" si="0"/></c></row>"#,
        );
        assert!(delete_cols(s.as_bytes(), 2, 1).is_none());
        let ok = delete_cols(s.as_bytes(), 3, 1).unwrap();
        assert!(matches!(ok, Cow::Owned(_)));
        let ok = delete_cols(s.as_bytes(), 2, 2).unwrap();
        assert!(matches!(ok, Cow::Owned(_)));
    }

    #[test]
    fn empty_sheet_refuses_cols() {
        assert!(shift_cols(b"<worksheet><sheetData/></worksheet>", 1, 1).is_none());
        assert!(shift_cols(b"<worksheet><sheetData></sheetData></worksheet>", 1, 1).is_none());
        assert!(shift_cols(b"<worksheet></worksheet>", 1, 1).is_none());
    }

    #[test]
    fn implicit_cell_at_or_after_shift_column_refuses() {
        let s = xml(
            "A1:B1",
            r#"<row r="1"><c><v>1</v></c><c r="B1"><v>2</v></c></row>"#,
        );
        assert!(insert_cols(s.as_bytes(), 1, 1).is_none());
        assert!(delete_cols(s.as_bytes(), 1, 1).is_none());
        let ok = insert_cols(s.as_bytes(), 2, 1).unwrap();
        assert!(matches!(ok, Cow::Owned(_)));
    }

    #[test]
    fn formula_body_shifts_on_col_shift() {
        let s = xml(
            "A1:B1",
            r#"<row r="1"><c r="B1"><f>SUM(A1:A5)</f><v>15</v></c></row>"#,
        );
        let out = insert_cols(s.as_bytes(), 1, 1).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            out.contains(r#"<c r="C1"><f>SUM(B1:B5)</f><v>15</v></c>"#),
            "{out}"
        );
    }

    #[test]
    fn missing_dimension_is_skipped_cols() {
        let s = r#"<worksheet xmlns="s"><sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData></worksheet>"#;
        let out = insert_cols(s.as_bytes(), 1, 1).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(out.contains(r#"<c r="B1"><v>1</v></c>"#), "{out}");
        assert!(!out.contains("dimension"), "{out}");
    }

    #[test]
    fn insert_cols_empty_row_and_self_closing_cells() {
        let s = xml(
            "A1:A3",
            r#"<row r="1"/><row r="2"><c r="B2" s="3"/></row><row r="3"></row>"#,
        );
        let out = insert_cols(s.as_bytes(), 2, 1).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(out.contains(r#"<row r="1"/>"#), "{out}");
        assert!(
            out.contains(r#"<row r="2"><c r="C2" s="3"/></row>"#),
            "{out}"
        );
        assert!(out.contains(r#"<row r="3"></row>"#), "{out}");
    }

    // Coordinator regression tests for two row/column asymmetries found by the
    // ST-1 planner: the row axis was missing the grid bound guard that the
    // column axis already had. Both emitted an out-of-grid reference.
    #[test]
    fn coordinator_shared_ref_cannot_escape_bottom_of_grid() {
        let x = format!(
            r#"<dimension ref="A1:A5"/><sheetData><row r="{r}"><c r="A{r}"><f t="shared" ref="A{r}:A1048576" si="0">A1</f></c></row></sheetData>"#,
            r = 1_048_570u32
        );
        // The shifted ref would pass 1048576, so the whole operation refuses.
        // Emitting the original (stale) ref would leave it pointing off the grid
        // while its master moved, silently breaking the workbook.
        assert!(shift_rows(x.as_bytes(), 1, 1).is_none());
    }

    #[test]
    fn coordinator_dimension_cannot_escape_bottom_of_grid() {
        let x = r#"<dimension ref="A1:A1048576"/><sheetData><row r="10"><c r="A10"><v>1</v></c></row></sheetData>"#;
        // The declared dimension would shift to A1:A1048577. It must be CLAMPED
        // to the last row, and the operation must still succeed: the only real
        // content is at row 10 and lands at row 11, far inside the grid.
        //
        // Refusing here was tried and reverted. `<dimension>` is advisory and
        // routinely over-declared, so refusing broke ordinary workbooks to buy
        // round-trip byte identity on a value Excel recomputes on load. The
        // guard that actually protects data is the one on real `<row>` indices,
        // asserted by `coordinator_real_content_at_grid_edge_still_refuses`.
        let out = shift_rows(x.as_bytes(), 10, 1).expect("must not refuse on an advisory bound");
        let s = std::str::from_utf8(&out).unwrap();
        assert!(
            s.contains("A1:A1048576"),
            "dimension must clamp at the last row: {s}"
        );
        assert!(
            !s.contains("1048577"),
            "no out-of-grid coordinate may be emitted: {s}"
        );
    }

    // ST-1: the grid-bound refusal must be total and loud. A shared ref or
    // dimension that cannot be shifted in-grid makes the WHOLE operation return
    // None (never a stale coordinate), and a ref comfortably inside the grid
    // still shifts normally — no over-refusal.
    #[test]
    fn shared_ref_past_bottom_of_grid_refuses_insert() {
        let s = xml(
            "A1048570:A1048576",
            r#"<row r="1048570"><c r="A1048570" t="str"><f t="shared" ref="A1048570:A1048576" si="0">=A1+A2</f><v>3</v></c></row>"#,
        );
        // The ref would need to become A1048571:A1048577 — past MAX_ROW.
        assert!(insert_rows(s.as_bytes(), 1, 1).is_none());
        assert!(shift_rows(s.as_bytes(), 1, 1).is_none());
    }

    #[test]
    fn shared_ref_past_xfd_refuses_insert_cols() {
        let s = xml(
            "XEZ1:XFD1",
            r#"<row r="1"><c r="XEZ1" t="str"><f t="shared" ref="XEZ1:XFD1" si="0">=XEZ1</f><v>3</v></c></row>"#,
        );
        // The ref would need to become XEY1...XFX1 — past column 16384.
        assert!(insert_cols(s.as_bytes(), 1, 1).is_none());
        assert!(shift_cols(s.as_bytes(), 1, 1).is_none());
    }

    #[test]
    fn shared_ref_inside_grid_still_shifts() {
        let s = xml(
            "A1:A5",
            r#"<row r="1"><c r="A1" t="str"><f t="shared" ref="A1:A5" si="0">=A1+A2</f><v>3</v></c></row>"#,
        );
        let out = insert_rows(s.as_bytes(), 1, 2).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            out.contains(
                r#"<c r="A3" t="str"><f t="shared" ref="A3:A7" si="0">=A3+A4</f><v>3</v></c>"#
            ),
            "{out}"
        );

        let c = xml(
            "B1:F1",
            r#"<row r="1"><c r="B1" t="str"><f t="shared" ref="B1:F1" si="0">=B1+C1</f><v>3</v></c></row>"#,
        );
        let out = insert_cols(c.as_bytes(), 2, 1).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            out.contains(
                r#"<c r="C1" t="str"><f t="shared" ref="C1:G1" si="0">=C1+D1</f><v>3</v></c>"#
            ),
            "{out}"
        );
    }

    #[test]
    fn out_of_grid_refusal_leaves_no_partial_output() {
        let s = xml(
            "A1048570:A1048576",
            r#"<row r="1048570"><c r="A1048570" t="str"><f t="shared" ref="A1048570:A1048576" si="0">=A1+A2</f><v>3</v></c></row>"#,
        );
        // The caller gets None, not a half-written buffer: the refusal is
        // all-or-nothing and the input slice is untouched.
        let before = s.as_bytes();
        let saved = before.to_vec();
        assert!(insert_rows(before, 1, 1).is_none());
        assert_eq!(before, saved.as_slice());
    }

    #[test]
    fn coordinator_cell_formula_refs_must_shift() {
        // A formula ANYWHERE in the sheet that points into or below the shifted
        // band must move with the grid, including one in a row that did not
        // itself move. Otherwise the saved workbook silently computes from the
        // wrong cells.
        let x = concat!(
            r#"<dimension ref="A1:B10"/><sheetData>"#,
            r#"<row r="1"><c r="B1"><f>SUM(A5:A10)</f><v>0</v></c></row>"#,
            r#"<row r="8"><c r="B8"><f>A9*2</f><v>0</v></c></row>"#,
            r#"</sheetData>"#
        );
        let out = shift_rows(x.as_bytes(), 3, 2).expect("insert must succeed");
        let s = std::str::from_utf8(&out).unwrap();
        assert!(
            s.contains("SUM(A7:A12)"),
            "formula in an unmoved row did not shift: {s}"
        );
        assert!(
            s.contains("A11*2"),
            "formula in a moved row did not shift: {s}"
        );
    }

    #[test]
    fn coordinator_overdeclared_dimension_must_not_refuse() {
        // Regression guard against turning the advisory <dimension> into a
        // refusal. Writers over-declare it constantly; this sheet claims the
        // whole grid height but holds one row at row 5. Inserting at row 2
        // moves that content to row 6, nowhere near the boundary, so the
        // operation MUST succeed. An earlier revision refused it, which broke
        // ordinary workbooks for the sake of round-trip byte identity on a
        // value Excel recomputes anyway.
        let x = concat!(
            r#"<dimension ref="A1:A1048576"/><sheetData>"#,
            r#"<row r="5"><c r="A5"><v>1</v></c></row>"#,
            r#"</sheetData>"#
        );
        let out = shift_rows(x.as_bytes(), 2, 1)
            .expect("must not refuse: all real content stays well inside the grid");
        let s = std::str::from_utf8(&out).unwrap();
        assert!(s.contains(r#"<row r="6">"#), "content did not shift: {s}");
        // The declared edge is clamped at the grid boundary, never emitted past it.
        assert!(
            s.contains("A1:A1048576"),
            "dimension must clamp, not overflow: {s}"
        );
    }

    #[test]
    fn coordinator_overdeclared_dimension_cols_must_not_refuse() {
        // Column-axis twin of the test above. These two paths have diverged
        // twice before, each time producing a bug, so they are asserted together.
        let x = concat!(
            r#"<dimension ref="A1:XFD1"/><sheetData>"#,
            r#"<row r="1"><c r="B1"><v>1</v></c></row>"#,
            r#"</sheetData>"#
        );
        let out = shift_cols(x.as_bytes(), 1, 1)
            .expect("must not refuse: all real content stays well inside the grid");
        let s = std::str::from_utf8(&out).unwrap();
        assert!(s.contains(r#"r="C1""#), "content did not shift: {s}");
        // Both ends shift: the start moves A->B because a blank column was
        // inserted to its left, and the end clamps at XFD instead of running on
        // to a column that does not exist.
        assert!(
            s.contains("B1:XFD1"),
            "dimension must clamp, not overflow: {s}"
        );
        assert!(
            !s.contains("XFE"),
            "no out-of-grid column may be emitted: {s}"
        );
    }

    #[test]
    fn coordinator_real_content_at_grid_edge_still_refuses() {
        // The other side of the line: clamping the ADVISORY dimension must not
        // have weakened the guard on real content. A row sitting on the last
        // row of the grid cannot be pushed off it, so this must still refuse.
        let x = concat!(
            r#"<dimension ref="A1:A1048576"/><sheetData>"#,
            r#"<row r="1048576"><c r="A1048576"><v>1</v></c></row>"#,
            r#"</sheetData>"#
        );
        assert!(
            shift_rows(x.as_bytes(), 2, 1).is_none(),
            "real content at the last row must still refuse to shift off the grid"
        );
    }

    #[test]
    fn coordinator_formula_free_row_between_formula_rows_survives() {
        // Regression for silent data loss found by the ST-1 invariant harness.
        // The formula fast path skips a formula-free row above the insertion
        // point without writing it, leaving it for the next flush's untouched
        // run. An earlier version also advanced `last_end` past it, so the
        // flush stepped over the row and dropped it from the output.
        //
        // It takes THREE things at once to expose: a formula-free row that sits
        // BETWEEN two formula rows, with all of them ABOVE the insertion point.
        // Rows before the first formula row are safe (the first flush copies
        // from byte 0), which is why every earlier formula test missed this.
        let x = concat!(
            r#"<dimension ref="A1:A200"/><sheetData>"#,
            r#"<row r="1"><c r="A1"><f>SUM(A5:A10)</f><v>0</v></c></row>"#,
            r#"<row r="10"><c r="A10"><v>10</v></c></row>"#,
            r#"<row r="80"><c r="A80"><f>MAX(A7:A10)</f><v>0</v></c></row>"#,
            r#"<row r="200"><c r="A200"><v>200</v></c></row>"#,
            r#"</sheetData>"#
        );
        let out = shift_rows(x.as_bytes(), 132, 3).expect("insert must succeed");
        let s = std::str::from_utf8(&out).unwrap();
        assert!(s.contains(r#"<row r="10">"#), "row 10 was dropped: {s}");
        assert!(s.contains("<v>10</v>"), "row 10's value was dropped: {s}");
        // The rows that should move still move, and the ones above stay put.
        assert!(
            s.contains(r#"<row r="203">"#),
            "row 200 did not shift to 203: {s}"
        );
        assert!(s.contains(r#"<row r="80">"#), "row 80 must not move: {s}");
    }

    #[test]
    fn formula_in_row_above_shift_point_shifts() {
        let s = xml(
            "A1:B10",
            concat!(
                r#"<row r="1"><c r="B1"><f>SUM(A5:A10)</f><v>0</v></c></row>"#,
                r#"<row r="3"><c r="A3"><v>1</v></c></row>"#,
            ),
        );
        let out = insert_rows(s.as_bytes(), 3, 2).unwrap();
        let s = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            s.contains(r#"<row r="1"><c r="B1"><f>SUM(A7:A12)</f><v>0</v></c></row>"#),
            "row 1 must stay put while its formula shifts: {s}"
        );
        assert!(
            s.contains(r#"<row r="5"><c r="A5"><v>1</v></c></row>"#),
            "{s}"
        );
    }

    #[test]
    fn formula_in_moved_row_shifts_with_cell() {
        let s = xml(
            "A1:B4",
            concat!(
                r#"<row r="1"><c r="A1"><v>1</v></c></row>"#,
                r#"<row r="3"><c r="B3"><f>B4*2</f><v>0</v></c></row>"#,
            ),
        );
        let out = insert_rows(s.as_bytes(), 2, 2).unwrap();
        let s = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            s.contains(r#"<row r="5"><c r="B5"><f>B6*2</f><v>0</v></c></row>"#),
            "the moved cell's formula must shift with it: {s}"
        );
    }

    #[test]
    fn absolute_refs_do_not_move() {
        let s = xml(
            "A1:B2",
            r#"<row r="1"><c r="B1"><f>$A$5+A$5+$A5</f><v>0</v></c></row>"#,
        );
        let out = insert_rows(s.as_bytes(), 1, 2).unwrap();
        let s = String::from_utf8(out.into_owned()).unwrap();
        assert!(s.contains("$A$5+A$5+$A7"), "{s}");
    }

    #[test]
    fn string_literal_looking_like_reference_is_untouched() {
        let s = xml(
            "A1:B2",
            r#"<row r="1"><c r="B1"><f>IF(A1="SUM(A5:A10)",B1,0)</f><v>0</v></c></row>"#,
        );
        let out = insert_rows(s.as_bytes(), 1, 1).unwrap();
        let s = String::from_utf8(out.into_owned()).unwrap();
        assert!(s.contains(r#"IF(A2="SUM(A5:A10)",B2,0)"#), "{s}");
    }

    #[test]
    fn shared_formula_ref_and_body_shift_each_once() {
        let s = xml(
            "A1:A3",
            r#"<row r="1"><c r="A1" t="str"><f t="shared" ref="A1:A3" si="0">=A1+A2</f><v>3</v></c></row><row r="2"><c r="A2" t="str"><f t="shared" si="0"/></c></row><row r="3"><c r="A3" t="str"><f t="shared" si="0"/></c></row>"#,
        );
        let out = insert_rows(s.as_bytes(), 1, 2).unwrap();
        let s = String::from_utf8(out.into_owned()).unwrap();
        // The ref attribute shifts on its own path and the body on its own path,
        // each exactly once. A double shift of either would emit A5:A7 / A5+A6.
        assert!(
            s.contains(
                r#"<c r="A3" t="str"><f t="shared" ref="A3:A5" si="0">=A3+A4</f><v>3</v></c>"#
            ),
            "{s}"
        );
    }

    #[test]
    fn self_closing_and_empty_f_elements_do_not_panic() {
        let s = xml(
            "A1:B4",
            concat!(
                r#"<row r="1"><c r="A1"><f/></c></row>"#,
                r#"<row r="2"><c r="A2"><f></f></c></row>"#,
                r#"<row r="3"><c r="A3" t="str"><f t="shared" si="0"/><v>5</v></c></row>"#,
            ),
        );
        let out = insert_rows(s.as_bytes(), 1, 1).unwrap();
        let s = String::from_utf8(out.into_owned()).unwrap();
        assert!(s.contains(r#"<row r="2"><c r="A2"><f/></c></row>"#), "{s}");
        assert!(
            s.contains(r#"<row r="3"><c r="A3"><f></f></c></row>"#),
            "{s}"
        );
        assert!(
            s.contains(r#"<row r="4"><c r="A4" t="str"><f t="shared" si="0"/><v>5</v></c></row>"#),
            "{s}"
        );
    }

    #[test]
    fn formula_col_axis_above_and_below_shift_point() {
        let s = xml(
            "A1:D2",
            r#"<row r="1"><c r="A1"><f>SUM(C1:C3)</f><v>0</v></c><c r="D1"><f>D2*2</f><v>0</v></c></row>"#,
        );
        let out = insert_cols(s.as_bytes(), 2, 2).unwrap();
        let s = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            s.contains(r#"<c r="A1"><f>SUM(E1:E3)</f><v>0</v></c>"#),
            "an unmoved cell's formula must shift with the columns: {s}"
        );
        assert!(s.contains(r#"<c r="F1"><f>F2*2</f><v>0</v></c>"#), "{s}");
    }

    #[test]
    fn delete_destroys_ref_inside_formula_text() {
        let s = xml(
            "A1:B3",
            r#"<row r="3"><c r="B3"><f>SUM(A1:A2)</f><v>0</v></c></row>"#,
        );
        let out = delete_rows(s.as_bytes(), 1, 2).unwrap();
        let s = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            s.contains(r#"<c r="B1"><f>SUM(#REF!:#REF!)</f><v>0</v></c>"#),
            "{s}"
        );
    }

    #[test]
    fn sheet_without_formulas_is_byte_identical() {
        // The formula scan only rewrites rows carrying "<f"; a sheet with no
        // formulas must flow through the untouched byte run exactly as before.
        let row1 =
            r#"<row r="1" ht="15" customHeight="1" spans="1:2"><c r="A1" s="3"><v>1</v></c></row>"#;
        let s = xml(
            "A1:B3",
            &format!(
                "{row1}<row r=\"2\"><c r=\"A2\"><v>2</v></c></row><row r=\"3\"><c r=\"B3\"><v>3</v></c></row>"
            ),
        );
        let out = insert_rows(s.as_bytes(), 2, 1).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            out.contains(row1),
            "a formula-free row above the shift point must stay byte-identical: {out}"
        );

        // Whole grid above the shift point with no formulas: borrow, no rewrite.
        let s2 = xml("A1:B2", row1);
        assert!(matches!(
            insert_rows(s2.as_bytes(), 5, 3),
            Some(Cow::Borrowed(_))
        ));
    }

    // The E2 gate fixture is formula-free, so it proves the fast path survived
    // T1-1f but never measures the formula path. This does: same shape, but
    // every fifth cell carries a formula, so every formula-bearing row leaves
    // the untouched byte run and pays for a shift_refs call.
    #[test]
    fn coordinator_e2_perf_gate_with_formulas() {
        let rows: u32 = 20_000;
        let cols: u32 = 20;
        let mut xml = Vec::with_capacity((rows * cols * 44) as usize);
        xml.extend_from_slice(b"<dimension ref=\"A1:T20000\"/><sheetData>");
        for r in 1..=rows {
            xml.extend_from_slice(format!("<row r=\"{}\" spans=\"1:{}\">", r, cols).as_bytes());
            for c in 1..=cols {
                let mut buf = [0u8; 4];
                let letters = crate::turbo::write::xml::col_letters(c, &mut buf).to_vec();
                let a1 = format!("{}{}", String::from_utf8(letters).unwrap(), r);
                if c % 5 == 0 {
                    xml.extend_from_slice(
                        format!("<c r=\"{}\"><f>SUM(A{}:B{})</f><v>{}</v></c>", a1, r, r, r)
                            .as_bytes(),
                    );
                } else {
                    xml.extend_from_slice(
                        format!("<c r=\"{}\" s=\"3\"><v>{}</v></c>", a1, r * c).as_bytes(),
                    );
                }
            }
            xml.extend_from_slice(b"</row>");
        }
        xml.extend_from_slice(b"</sheetData>");
        let cells = (rows * cols) as f64;

        let t = std::time::Instant::now();
        let out = shift_rows(&xml, 2, 1).expect("splice must succeed");
        let el = t.elapsed();
        let bpc = out.len() as f64 / cells;
        println!(
            "E2 formula gate: {} cells (20% formulas), {:.1} B/cell, {:.1} ms",
            cells as u64,
            bpc,
            el.as_secs_f64() * 1000.0
        );
        let s = std::str::from_utf8(&out).unwrap();
        assert!(
            s.contains("SUM(A3:B3)"),
            "row 2 formula did not shift to row 3"
        );
        assert!(bpc < 70.0, "formula path grew to {bpc:.1} B/cell");
    }

    // ------------------ move_range ------------------

    fn a1c(c: u32) -> String {
        let mut buf = [0u8; 4];
        String::from_utf8(col_letters(c, &mut buf).to_vec()).unwrap()
    }

    fn cell_ref(r: u32, c: u32) -> String {
        format!("{}{}", a1c(c), r)
    }

    #[test]
    fn move_down_right_relocates_and_vacates() {
        let s = xml(
            "A1:D4",
            r#"<row r="1"><c r="A1"><v>1</v></c><c r="B1"><v>2</v></c></row><row r="2"><c r="A2"><v>3</v></c><c r="B2"><v>4</v></c></row>"#,
        );
        let out = move_range(s.as_bytes(), 1, 1, 2, 2, 1, 2, false).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        // Content lands at +1 row, +2 cols.
        assert!(out.contains(r#"<c r="C2"><v>1</v></c>"#), "{out}");
        assert!(out.contains(r#"<c r="D2"><v>2</v></c>"#), "{out}");
        assert!(out.contains(r#"<c r="C3"><v>3</v></c>"#), "{out}");
        assert!(out.contains(r#"<c r="D3"><v>4</v></c>"#), "{out}");
        // Source is vacated: no cells remain in A1:B2.
        assert!(!out.contains(r#"<c r="A1"><v>1</v></c>"#), "{out}");
        assert!(!out.contains(r#"<c r="B2"><v>4</v></c>"#), "{out}");
        // Destination is imprinted by the source (including its empty cells), so
        // the destination cells that were NOT in the source are gone.
        let s2 = xml(
            "A1:D3",
            r#"<row r="1"><c r="A1"><v>1</v></c><c r="C1"><v>x</v></c><c r="D1"><v>y</v></c></row><row r="2"><c r="A2"><v>2</v></c></row>"#,
        );
        let out = move_range(s2.as_bytes(), 1, 1, 2, 1, 0, 2, false).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        // Old destination content (C1's "x") is overwritten; source vacated.
        assert!(out.contains(r#"<c r="C1"><v>1</v></c>"#), "{out}");
        assert!(out.contains(r#"<c r="C2"><v>2</v></c>"#), "{out}");
        assert!(!out.contains(r#"<c r="A1"><v>1</v></c>"#), "{out}");
        // D1 is OUTSIDE the destination rectangle (C1:C2), so it is untouched.
        assert!(out.contains(r#"<c r="D1"><v>y</v></c>"#), "{out}");
    }

    #[test]
    fn move_up_left_relocates() {
        let s = xml(
            "A1:D4",
            r#"<row r="3"><c r="C3"><v>a</v></c><c r="D3"><v>b</v></c></row><row r="4"><c r="C4"><v>c</v></c><c r="D4"><v>d</v></c></row>"#,
        );
        let out = move_range(s.as_bytes(), 3, 3, 4, 4, -2, -2, false).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(out.contains(r#"<c r="A1"><v>a</v></c>"#), "{out}");
        assert!(out.contains(r#"<c r="B1"><v>b</v></c>"#), "{out}");
        assert!(out.contains(r#"<c r="A2"><v>c</v></c>"#), "{out}");
        assert!(out.contains(r#"<c r="B2"><v>d</v></c>"#), "{out}");
        assert!(!out.contains(r#"<c r="C3">"#), "{out}");
        assert!(!out.contains(r#"<c r="D4">"#), "{out}");
    }

    #[test]
    fn overlapping_move_down_computes_against_original_grid() {
        // S = A1:A3, D = A2:A4. Each cell must move one row down; A1 must keep
        // its ORIGINAL value at A2, never the pre-move A1 (which had been read).
        let s = xml(
            "A1:A4",
            r#"<row r="1"><c r="A1"><v>1</v></c></row><row r="2"><c r="A2"><v>2</v></c></row><row r="3"><c r="A3"><v>3</v></c></row>"#,
        );
        let out = move_range(s.as_bytes(), 1, 1, 3, 1, 1, 0, false).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            out.contains(r#"<row r="2"><c r="A2"><v>1</v></c></row>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<row r="3"><c r="A3"><v>2</v></c></row>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<row r="4"><c r="A4"><v>3</v></c></row>"#),
            "{out}"
        );
        assert!(!out.contains(r#"<row r="1"><c r="A1"><v>"#), "{out}");
    }

    #[test]
    fn overlapping_move_up_computes_against_original_grid() {
        // S = A2:A4, D = A1:A3. Moving up must read the ORIGINAL values.
        let s = xml(
            "A1:A4",
            r#"<row r="1"><c r="A1"><v>1</v></c></row><row r="2"><c r="A2"><v>2</v></c></row><row r="3"><c r="A3"><v>3</v></c></row><row r="4"><c r="A4"><v>4</v></c></row>"#,
        );
        let out = move_range(s.as_bytes(), 2, 1, 4, 1, -1, 0, false).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            out.contains(r#"<row r="1"><c r="A1"><v>2</v></c></row>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<row r="2"><c r="A2"><v>3</v></c></row>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<row r="3"><c r="A3"><v>4</v></c></row>"#),
            "{out}"
        );
        assert!(!out.contains(r#"<row r="4"><c r="A4"><v>"#), "{out}");
    }

    #[test]
    fn move_translate_true_shifts_formulas_inside_range() {
        let s = xml(
            "A1:D4",
            r#"<row r="1"><c r="B1"><f>SUM(A1:A3)</f><v>6</v></c><c r="D1"><f>B1*2</f><v>12</v></c></row>"#,
        );
        // Move B1:D1 down 1 and right 1.
        let out = move_range(s.as_bytes(), 1, 2, 1, 4, 1, 1, true).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            out.contains(r#"<c r="C2"><f>SUM(B2:B4)</f><v>6</v></c>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<c r="E2"><f>C2*2</f><v>12</v></c>"#),
            "{out}"
        );
    }

    #[test]
    fn move_translate_false_leaves_formulas_alone() {
        let s = xml(
            "A1:D4",
            r#"<row r="1"><c r="B1"><f>SUM(A1:A3)</f><v>6</v></c></row>"#,
        );
        let out = move_range(s.as_bytes(), 1, 2, 1, 2, 1, 1, false).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            out.contains(r#"<c r="C2"><f>SUM(A1:A3)</f><v>6</v></c>"#),
            "{out}"
        );
    }

    #[test]
    fn move_destination_beyond_last_row_creates_new_rows() {
        let s = xml(
            "A1:B2",
            r#"<row r="1"><c r="A1"><v>1</v></c><c r="B1"><v>2</v></c></row><row r="2"><c r="A2"><v>3</v></c></row>"#,
        );
        let out = move_range(s.as_bytes(), 1, 1, 2, 2, 5, 0, false).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            out.contains(r#"<row r="6"><c r="A6"><v>1</v></c><c r="B6"><v>2</v></c></row>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<row r="7"><c r="A7"><v>3</v></c></row>"#),
            "{out}"
        );
        assert!(!out.contains(r#"<c r="A1">"#), "{out}");
        // Dimension widened to cover the destination.
        assert!(out.contains(r#"<dimension ref="A1:B7"/>"#), "{out}");
    }

    #[test]
    fn move_out_of_grid_refuses_and_leaves_input_untouched() {
        let s = xml(
            "A1:A2",
            r#"<row r="1"><c r="A1"><v>1</v></c></row><row r="2"><c r="A2"><v>2</v></c></row>"#,
        );
        let saved = s.clone();
        // Pushing past the last row refuses.
        assert!(move_range(s.as_bytes(), 1, 1, 2, 1, 1_048_575, 0, false).is_none());
        // Pushing past XFD refuses.
        assert!(move_range(s.as_bytes(), 1, 1, 1, 1, 0, 16_384, false).is_none());
        // Moving up past row 1 refuses.
        assert!(move_range(s.as_bytes(), 1, 1, 1, 1, -1, 0, false).is_none());
        // Moving left past column A refuses.
        assert!(move_range(s.as_bytes(), 1, 1, 1, 1, 0, -1, false).is_none());
        assert_eq!(s, saved, "refused move must not mutate the input");
    }

    #[test]
    fn move_zero_offset_borrows() {
        let s = xml(
            "A1:A2",
            r#"<row r="1"><c r="A1"><v>1</v></c></row><row r="2"><c r="A2"><v>2</v></c></row>"#,
        );
        let out = move_range(s.as_bytes(), 1, 1, 2, 2, 0, 0, false).unwrap();
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(out.as_ref() as &[u8], s.as_bytes());
    }

    #[test]
    fn move_empty_sheet_refuses() {
        assert!(
            move_range(
                b"<worksheet><sheetData/></worksheet>",
                1,
                1,
                2,
                2,
                1,
                0,
                false
            )
            .is_none()
        );
        assert!(
            move_range(
                b"<worksheet><sheetData></sheetData></worksheet>",
                1,
                1,
                2,
                2,
                1,
                0,
                false
            )
            .is_none()
        );
        assert!(move_range(b"<worksheet></worksheet>", 1, 1, 2, 2, 1, 0, false).is_none());
    }

    #[test]
    fn move_implicit_row_in_band_refuses() {
        let s = xml(
            "A1:A2",
            r#"<row><c r="A1"><v>1</v></c></row><row r="2"><c r="A2"><v>2</v></c></row>"#,
        );
        assert!(move_range(s.as_bytes(), 1, 1, 2, 1, 1, 0, false).is_none());
        // A move that does not touch the implicit row succeeds.
        let ok = move_range(s.as_bytes(), 2, 1, 2, 1, 0, 1, false).unwrap();
        assert!(matches!(ok, Cow::Owned(_)));
    }

    #[test]
    fn move_implicit_cell_in_band_refuses() {
        let s = xml(
            "A1:B1",
            r#"<row r="1"><c><v>1</v></c><c r="B1"><v>2</v></c></row>"#,
        );
        assert!(move_range(s.as_bytes(), 1, 1, 1, 1, 0, 1, false).is_none());
        // Moving only the explicit cell succeeds.
        let ok = move_range(s.as_bytes(), 1, 2, 1, 2, 0, 1, false).unwrap();
        assert!(matches!(ok, Cow::Owned(_)));
    }

    #[test]
    fn move_self_closing_and_empty_rows_survive() {
        let s = xml(
            "A1:A4",
            r#"<row r="1"/><row r="2"><c r="B2" s="3"/></row><row r="3"></row><row r="4"><c r="A4"><v>1</v></c></row>"#,
        );
        let out = move_range(s.as_bytes(), 2, 2, 4, 2, 1, 0, false).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(out.contains(r#"<row r="1"/>"#), "{out}");
        // B2 (in the source range) moves to B3; its style follows.
        assert!(out.contains(r#"<row r="2"></row>"#), "{out}");
        assert!(
            out.contains(r#"<row r="3"><c r="B3" s="3"/></row>"#),
            "{out}"
        );
        // A4 is outside the source range (column A vs B), so it stays put.
        assert!(
            out.contains(r#"<row r="4"><c r="A4"><v>1</v></c></row>"#),
            "{out}"
        );
    }

    #[test]
    fn move_merged_range_inside_block_follows() {
        let s = format!(
            r#"<?xml version="1.0"?><worksheet xmlns="s"><dimension ref="A1:D5"/><sheetData>{rows}</sheetData><mergeCells count="1"><mergeCell ref="A2:A3"/></mergeCells></worksheet>"#,
            rows =
                r#"<row r="2"><c r="A2"><v>1</v></c></row><row r="3"><c r="A3"><v>2</v></c></row>"#,
        );
        let out = move_range(s.as_bytes(), 2, 1, 4, 2, 1, 2, false).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(out.contains(r#"<mergeCell ref="C3:C4"/>"#), "{out}");
    }

    #[test]
    fn move_merged_range_straddling_boundary_stays() {
        let s = format!(
            r#"<?xml version="1.0"?><worksheet xmlns="s"><dimension ref="A1:D5"/><sheetData>{rows}</sheetData><mergeCells count="1"><mergeCell ref="A1:A3"/></mergeCells></worksheet>"#,
            rows = r#"<row r="1"><c r="A1"><v>1</v></c></row><row r="2"><c r="A2"><v>2</v></c></row><row r="3"><c r="A3"><v>3</v></c></row>"#,
        );
        // The merge A1:A3 straddles the source A2:A3 (A1 is outside); it is left alone.
        let out = move_range(s.as_bytes(), 2, 1, 3, 1, 1, 0, false).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(out.contains(r#"<mergeCell ref="A1:A3"/>"#), "{out}");
    }

    #[test]
    fn move_hyperlinks_dv_cf_inside_block_follow() {
        let s = format!(
            r#"<?xml version="1.0"?><worksheet xmlns="s"><dimension ref="A1:D5"/><sheetData>{rows}</sheetData><hyperlinks><hyperlink ref="B2" r:id="rId1"/></hyperlinks><dataValidations count="1"><dataValidation type="whole" sqref="B2 B3"><formula1>1</formula1></dataValidation></dataValidations><conditionalFormatting sqref="B2:C3"><cfRule type="expression" priority="1"><formula>A1>1</formula></cfRule></conditionalFormatting></worksheet>"#,
            rows = r#"<row r="2"><c r="B2"><v>1</v></c><c r="B3"><v>2</v></c><c r="C2"><v>3</v></c><c r="C3"><v>4</v></c></row>"#,
        );
        let out = move_range(s.as_bytes(), 2, 2, 3, 3, 1, 1, false).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            out.contains(r#"<hyperlink ref="C3" r:id="rId1"/>"#),
            "{out}"
        );
        assert!(out.contains(r#"sqref="C3 C4""#), "{out}");
        assert!(
            out.contains(r#"<conditionalFormatting sqref="C3:D4">"#),
            "{out}"
        );
        // CF rule formula text is left alone (only cell formulas translate).
        assert!(out.contains(r#"<formula>A1>1</formula>"#), "{out}");
    }

    #[test]
    fn move_shared_formula_ref_and_body_translate() {
        let s = xml(
            "A1:A3",
            r#"<row r="1"><c r="A1" t="str"><f t="shared" ref="A1:A3" si="0">=A1+A2</f><v>3</v></c></row><row r="2"><c r="A2" t="str"><f t="shared" si="0"/></c></row><row r="3"><c r="A3" t="str"><f t="shared" si="0"/></c></row>"#,
        );
        let out = move_range(s.as_bytes(), 1, 1, 1, 1, 1, 0, true).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            out.contains(
                r#"<c r="A2" t="str"><f t="shared" ref="A2:A4" si="0">=A2+A3</f><v>3</v></c>"#
            ),
            "{out}"
        );
    }

    #[test]
    fn move_row_attributes_survive() {
        let s = xml(
            "A1:B2",
            r#"<row r="1" ht="15" customHeight="1" spans="1:1"><c r="A1"><v>1</v></c></row>"#,
        );
        let out = move_range(s.as_bytes(), 1, 1, 1, 1, 1, 0, false).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        // The source row keeps its attributes (spans dropped; Excel recomputes)
        // and is vacated.
        assert!(
            out.contains(r#"<row r="1" ht="15" customHeight="1"></row>"#),
            "{out}"
        );
        // The cell moved into a brand-new row; row attributes are not copied
        // (a moved cell does not drag its source row's height with it).
        assert!(
            out.contains(r#"<row r="2"><c r="A2"><v>1</v></c></row>"#),
            "{out}"
        );
    }

    #[test]
    fn move_up_into_gap_creates_missing_destination_rows() {
        // Source B2:C3 -> destination A1:B2. Row 1 does not exist in the body,
        // so the cells destined for it must become a brand-new row — an earlier
        // cursor-based implementation consumed them and silently dropped them.
        let s = xml(
            "B2:C3",
            r#"<row r="2"><c r="B2"><v>2</v></c><c r="C2"><v>3</v></c></row><row r="3"><c r="B3"><v>4</v></c></row>"#,
        );
        let out = move_range(s.as_bytes(), 2, 2, 3, 3, -1, -1, false).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(
            out.contains(r#"<row r="1"><c r="A1"><v>2</v></c><c r="B1"><v>3</v></c></row>"#),
            "{out}"
        );
        assert!(
            out.contains(r#"<row r="2"><c r="A2"><v>4</v></c></row>"#),
            "{out}"
        );
        assert!(out.contains(r#"<row r="3"></row>"#), "{out}");
    }

    #[test]
    fn move_range_small_move_in_large_sheet_stays_lean() {
        // Same shape as the E2 gate fixture: a large formula-free grid. Moving a
        // 2x2 block far away must NOT rewrite the whole sheet — the output must
        // stay within a small byte delta of the input (only the band changed).
        let rows: u32 = 20_000;
        let cols: u32 = 20;
        let mut xml = Vec::with_capacity((rows * cols * 40) as usize);
        xml.extend_from_slice(b"<dimension ref=\"A1:T20000\"/><sheetData>");
        for r in 1..=rows {
            xml.extend_from_slice(format!("<row r=\"{}\" spans=\"1:{}\">", r, cols).as_bytes());
            for c in 1..=cols {
                let mut buf = [0u8; 4];
                let letters = crate::turbo::write::xml::col_letters(c, &mut buf).to_vec();
                let a1 = format!("{}{}", String::from_utf8(letters).unwrap(), r);
                xml.extend_from_slice(
                    format!("<c r=\"{}\" s=\"3\"><v>{}</v></c>", a1, r * c).as_bytes(),
                );
            }
            xml.extend_from_slice(b"</row>");
        }
        xml.extend_from_slice(b"</sheetData>");
        let cells = (rows * cols) as f64;

        let t = std::time::Instant::now();
        let out = move_range(&xml, 2, 2, 3, 3, 10_000, 100, false).expect("move must succeed");
        let el = t.elapsed();

        let delta = (out.len() as i64 - xml.len() as i64).abs();
        println!(
            "E2 move gate: {} cells, input {:.1} MB, output {:.1} MB, delta {} B, {:.1} ms",
            cells as u64,
            xml.len() as f64 / 1e6,
            out.len() as f64 / 1e6,
            delta,
            el.as_secs_f64() * 1000.0
        );
        assert!(
            delta < 1024,
            "small move rewrote the sheet: input {} bytes, output {} bytes",
            xml.len(),
            out.len()
        );
        let bpc = out.len() as f64 / cells;
        assert!(
            bpc < 60.0,
            "output grew to {bpc:.1} B/cell — materialisation reintroduced"
        );

        let s = std::str::from_utf8(&out).unwrap();
        // The 2x2 block landed at rows 10002..10003, cols 102..103.
        let dst = cell_ref(10_002, 102);
        assert!(
            s.contains(&format!(r#"<c r="{dst}" s="3"><v>4</v></c>"#)),
            "{s}"
        );
        // Source cells vacated.
        assert!(!s.contains(r#"<c r="B2""#), "{s}");
        // A far-away row must be untouched (byte-identical content at its ref).
        let far = cell_ref(15_000, 10);
        assert!(
            s.contains(&format!(r#"<c r="{far}" s="3"><v>{}</v></c>"#, 10 * 15_000)),
            "{s}"
        );
    }
}
