//! fixup.rs — dependent-feature fixups for row and column insert/delete (T1-1d).
//!
//! `mutate.rs` splices `<sheetData>`, the `<dimension>` ref, and shared-formula
//! `ref=` attributes. Everything else in the sheet that names a coordinate goes
//! stale: mergeCells, autoFilter, hyperlinks, dataValidations (sqref +
//! formula1/2), conditionalFormatting (sqref + cfRule formulas),
//! rowBreaks/colBreaks ids, and sheetView pane/selection coordinates — plus, in
//! OTHER parts, table refs (`xl/tables/*.xml`) and workbook defined names
//! (`xl/workbook.xml`).
//!
//! This module is a pure, byte-preserving metadata pass. It edits only the sheet
//! header (before `<sheetData>`) and tail (after `</sheetData>`), never the grid,
//! so it is O(features) per sheet, not O(cells): peak RSS stays the splice's
//! O(output) and the extra CPU is a few memcpys against a 10M-cell splice. It is
//! designed to run AFTER the splice, on the spliced bytes.
//!
//! The mappings are `mutate::shifted_row` / `mutate::shifted_col`, promoted to
//! `pub(crate)` so the splice and the fixup can never diverge — the load-bearing
//! invariant. On the column axis the shared `shifted_col` from `mutate.rs` is
//! preferred over any local mirror (the column splice owns the `<cols>` element
//! itself, so this pass never touches it).
//!
//! `$` markers are treated as COSMETIC in ranges and defined-name values: Excel
//! adjusts those structurally on insert/delete (a print area `$A$1:$D$6` shrinks
//! to `$A$1:$D$4` when rows 2-3 are deleted). This is why structural ranges are
//! NOT routed through `refshift::shift_refs` (which correctly pins `$`-absolute
//! rows in *cell formulas*); formula text (DV/CF) still goes through `refshift`.
//!
//! Refusal contract (all-or-nothing): `None` means refuse the whole operation.
//! Only table parts refuse — a delete that removes a table's header row (row
//! axis) or empties a table (column axis) is corruption. Everything else is
//! silently adjusted (dropped, shrunk, or clamped) per the element rules, because
//! the user would not notice the loss: the deleted rows/cols held the affected
//! data anyway.

use std::borrow::Cow;

use crate::turbo::decode::decode_bytes;
use crate::turbo::formula::letters_to_index;
use crate::turbo::mutate::{attr_value_span, shifted_col, shifted_row};
use crate::turbo::overlay::find_element;
use crate::turbo::refshift::{Axis, shift_refs};
use crate::turbo::write::xml::col_letters;

/// How a range reacts when its leading edge (first row/col) falls inside the
/// delete band.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BandPolicy {
    /// Leading edge in band drops the whole range (merge anchor, hyperlink
    /// anchor, autoFilter header row, validation cell gone). Whole range in
    /// band also drops.
    Drop,
    /// Leading edge in band clamps to the band start (`at`); the range survives
    /// shifted (a merge trimmed of a column). Whole range in band drops.
    Clamp,
    /// View-state clamping: leading edge clamps to `at`, and a fully-deleted
    /// range clamps to the band edge (`at-1`) so a selection never vanishes.
    ClampView,
}

/// A parsed A1 cell reference (1-based row/col). The `$` flags are cosmetic.
#[derive(Clone, Copy, PartialEq, Eq)]
struct CellRef {
    row: u32,
    col: u32,
    abs_row: bool,
    abs_col: bool,
}

/// One endpoint of an A1 range: a cell, a whole row (`$1`), or a whole column
/// (`$A`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum A1Part {
    Cell(CellRef),
    WholeRow(u32, bool),
    WholeCol(u32, bool),
}

/// Result of shifting a single A1 range.
enum RangeShift {
    Unchanged,
    Dropped,
    Changed(Vec<u8>),
}

/// Result of editing one XML element.
enum ElemEdit {
    Keep,
    Drop,
    Replace(Vec<u8>),
}

/// Outcome of shifting one mergeCell element.
enum MergeOutcome {
    Keep,
    Replace(Vec<u8>),
    Drop,
}

/// Shift the index that lies on `axis` (1-based). Both mappings come from the
/// splice (`mutate::shifted_row` / `mutate::shifted_col`), so the fixup and the
/// splice can never diverge.
#[inline]
fn shift_index(axis: Axis, idx: u32, at: u32, delta: i64) -> Option<u32> {
    match axis {
        Axis::Row => shifted_row(idx, at, delta),
        Axis::Col => shifted_col(idx, at, delta),
    }
}

// ----------------------------------------------------------------------------
// Public entry points
// ----------------------------------------------------------------------------

/// Fix up every coordinate a sheet part names outside `<sheetData>`.
///
/// Returns `Cow::Borrowed` when nothing changes, `Cow::Owned` with the fixed
/// bytes otherwise, and `None` for an invalid shift point (`at == 0`). This pass
/// never refuses: all of its elements are safe to adjust silently. Table parts
/// and workbook defined names are separate parts and use their own entry points.
pub fn fixup_sheet_xml<'a>(
    xml: &'a [u8],
    axis: Axis,
    at: u32,
    delta: i64,
) -> Option<Cow<'a, [u8]>> {
    if delta == 0 {
        return Some(Cow::Borrowed(xml));
    }
    if at == 0 {
        return None;
    }

    // Split at the sheetData block so the metadata passes never copy the grid.
    let sd = find_element(xml, b"sheetData", 0)?;
    let sd_close = memchr::memmem::find(&xml[sd..], b"</sheetData>").map(|p| sd + p)?;
    let sd_close_end = sd_close + b"</sheetData>".len();
    let header = &xml[..sd];
    let tail = &xml[sd_close_end..];

    let mut changed = false;

    let mut header_out = header.to_vec();
    if let Some(n) = fixup_sheet_view(header, axis, at, delta) {
        header_out = n;
        changed = true;
    }

    let mut tail_out: Cow<'_, [u8]> = Cow::Borrowed(tail);
    apply(&mut tail_out, |b| fixup_merge_cells(b, axis, at, delta));
    apply(&mut tail_out, |b| fixup_hyperlinks(b, axis, at, delta));
    apply(&mut tail_out, |b| fixup_auto_filter(b, axis, at, delta));
    apply(&mut tail_out, |b| {
        fixup_data_validations(b, axis, at, delta)
    });
    apply(&mut tail_out, |b| {
        fixup_conditional_formatting(b, axis, at, delta)
    });
    apply(&mut tail_out, |b| fixup_breaks(b, axis, at, delta));
    if let Cow::Owned(_) = &tail_out {
        changed = true;
    }

    if !changed {
        return Some(Cow::Borrowed(xml));
    }

    let mut out = Vec::with_capacity(header_out.len() + tail_out.len() + (sd_close_end - sd) + 16);
    out.extend_from_slice(&header_out);
    out.extend_from_slice(&xml[sd..sd_close_end]);
    out.extend_from_slice(&tail_out);
    Some(Cow::Owned(out))
}

/// Fix up one table part (`xl/tables/tableN.xml`).
///
/// Shifts the table `ref` (and any `totalsRowRef` attribute and inner
/// `<autoFilter ref>`), and zeroes `totalsRowCount` when the delete removes the
/// totals row. Refuses — returns `None` — when the operation would corrupt the
/// table: deleting the header row (row axis) or deleting every column (column
/// axis).
pub fn fixup_table_part_xml<'a>(
    xml: &'a [u8],
    axis: Axis,
    at: u32,
    delta: i64,
) -> Option<Cow<'a, [u8]>> {
    if delta == 0 {
        return Some(Cow::Borrowed(xml));
    }
    if at == 0 {
        return None;
    }

    let s = find_element(xml, b"table", 0)?;
    let gt_rel = memchr::memchr(b'>', &xml[s..])?;
    let gt = s + gt_rel;
    let tag = &xml[s..gt];
    let (vs, ve) = attr_value_span(tag, b"ref")?;
    let val = &tag[vs..ve];
    let parts = parse_a1_range(val)?;
    if parts.len() != 2 {
        return Some(Cow::Borrowed(xml));
    }

    // Refusal pre-checks (all-or-nothing: refuse before anything is edited).
    match axis {
        Axis::Row => {
            let r0 = parts.iter().filter_map(part_row).min()?;
            if shift_index(Axis::Row, r0, at, delta).is_none() {
                return None; // header row deleted -> corrupt table
            }
        }
        Axis::Col => {
            let c0 = parts.iter().filter_map(part_col).min()?;
            let c1 = parts.iter().filter_map(part_col).max()?;
            if shift_index(Axis::Col, c0, at, delta).is_none()
                && shift_index(Axis::Col, c1, at, delta).is_none()
            {
                return None; // every column deleted -> empty table
            }
        }
    }

    // Totals row (the bottom of `ref` when totalsRowCount > 0) removed?
    let totals: u32 = std::str::from_utf8(
        attr_value_span(tag, b"totalsRowCount")
            .map(|(vs, ve)| &tag[vs..ve])
            .unwrap_or(b"0"),
    )
    .ok()
    .and_then(|s| s.trim().parse().ok())
    .unwrap_or(0);
    let mut set_totals_zero = false;
    if axis == Axis::Row && totals > 0 {
        if let Some(r1) = parts.iter().filter_map(part_row).max() {
            if shift_index(Axis::Row, r1, at, delta).is_none() {
                set_totals_zero = true;
            }
        }
    }

    // Shift the ref (Clamp: refusal pre-checked, so it can only shrink or grow).
    let ref_edit = match shift_a1_range(val, axis, at, delta, BandPolicy::Clamp) {
        RangeShift::Changed(r) => Some(r),
        RangeShift::Unchanged => None,
        RangeShift::Dropped => return None,
    };

    // Rebuild the open tag (ref, totalsRowCount, defensive totalsRowRef).
    let mut new_tag = tag.to_vec();
    let mut changed = false;
    if let Some(r) = ref_edit {
        let mut t = Vec::with_capacity(new_tag.len() + r.len());
        t.extend_from_slice(&new_tag[..vs]);
        t.extend_from_slice(&r);
        t.extend_from_slice(&new_tag[ve..]);
        new_tag = t;
        changed = true;
    }
    if set_totals_zero {
        if let Some((vs, ve)) = attr_value_span(&new_tag, b"totalsRowCount") {
            if &new_tag[vs..ve] != b"0" {
                let mut t = Vec::with_capacity(new_tag.len() + 1);
                t.extend_from_slice(&new_tag[..vs]);
                t.push(b'0');
                t.extend_from_slice(&new_tag[ve..]);
                new_tag = t;
                changed = true;
            }
        }
    }
    if let Some((vs, ve)) = attr_value_span(&new_tag, b"totalsRowRef") {
        if let RangeShift::Changed(r) =
            shift_a1_range(&new_tag[vs..ve], axis, at, delta, BandPolicy::Clamp)
        {
            let mut t = Vec::with_capacity(new_tag.len() + r.len());
            t.extend_from_slice(&new_tag[..vs]);
            t.extend_from_slice(&r);
            t.extend_from_slice(&new_tag[ve..]);
            new_tag = t;
            changed = true;
        }
    }

    // Rebuild the open tag (ref, totalsRowCount, defensive totalsRowRef), then
    // run the inner <autoFilter ref> on the body regardless of whether the open
    // tag changed, so a table whose ref is static but whose autoFilter moves is
    // still fixed.
    let new_open_len = new_tag.len();
    let mut out = Vec::with_capacity(xml.len() + new_open_len + 16);
    out.extend_from_slice(&xml[..s]);
    out.extend_from_slice(&new_tag);
    out.extend_from_slice(&xml[gt..]);

    let body_from = s + new_open_len + 1;
    if let Some(nb) = fixup_table_inner_auto_filter(&out[body_from..], axis, at, delta) {
        out.truncate(body_from);
        out.extend_from_slice(&nb);
        changed = true;
    }

    if changed {
        Some(Cow::Owned(out))
    } else {
        Some(Cow::Borrowed(xml))
    }
}

/// Fix up workbook defined names whose leading sheet qualifier names
/// `sheet_name` (matched case-insensitively, per Excel). External names
/// (`[1]...`), constants, and names of other sheets are left alone. Returns
/// `Cow::Borrowed` when no defined name references `sheet_name`.
///
/// This is a different part than the spliced sheet; the caller must apply both
/// in one save, or neither (all-or-nothing).
pub fn fixup_workbook_xml<'a>(
    xml: &'a [u8],
    sheet_name: &str,
    axis: Axis,
    at: u32,
    delta: i64,
) -> Cow<'a, [u8]> {
    if delta == 0 {
        return Cow::Borrowed(xml);
    }
    let Some(ds) = find_element(xml, b"definedNames", 0) else {
        return Cow::Borrowed(xml);
    };
    let Some(dgt_rel) = memchr::memchr(b'>', &xml[ds..]) else {
        return Cow::Borrowed(xml);
    };
    let dgt = ds + dgt_rel;
    if dgt > ds && xml[dgt - 1] == b'/' {
        return Cow::Borrowed(xml);
    }
    let close_tag = b"</definedNames>";
    let Some(dc_rel) = memchr::memmem::find(&xml[dgt..], close_tag) else {
        return Cow::Borrowed(xml);
    };
    let dc = dgt + dc_rel;
    let block = &xml[dgt + 1..dc];
    let dn_close = b"</definedName>";

    let mut new_block: Vec<u8> = Vec::new();
    let mut pos = 0usize;
    let mut changed = false;
    let mut started = false;
    let mut last = 0usize;
    loop {
        let Some(srel) = find_element(&block[pos..], b"definedName", 0) else {
            break;
        };
        let s = pos + srel;
        let Some(gt_rel) = memchr::memchr(b'>', &block[s..]) else {
            break;
        };
        let gt = s + gt_rel;
        if gt > s && block[gt - 1] == b'/' {
            pos = gt + 1;
            continue;
        }
        let Some(c_rel) = memchr::memmem::find(&block[gt..], dn_close) else {
            break;
        };
        let c = gt + c_rel;
        let end = c + dn_close.len();

        if !started {
            new_block.extend_from_slice(&block[..s]);
            started = true;
        } else {
            new_block.extend_from_slice(&block[last..s]);
        }
        let value = &block[gt + 1..c];
        match shift_defined_name_value(value, sheet_name, axis, at, delta) {
            None => new_block.extend_from_slice(&block[s..end]),
            Some(new_val) => {
                new_block.extend_from_slice(&block[s..=gt]);
                new_block.extend_from_slice(&new_val);
                new_block.extend_from_slice(&block[c..end]);
                changed = true;
            }
        }
        last = end;
        pos = end;
    }

    if !started || !changed {
        return Cow::Borrowed(xml);
    }
    new_block.extend_from_slice(&block[last..]);

    let mut out = Vec::with_capacity(xml.len() + 16);
    out.extend_from_slice(&xml[..=dgt]);
    out.extend_from_slice(&new_block);
    out.extend_from_slice(&xml[dc..]);
    Cow::Owned(out)
}

// ----------------------------------------------------------------------------
// Sheet header: sheetView / pane / selection
// ----------------------------------------------------------------------------

fn fixup_sheet_view(header: &[u8], axis: Axis, at: u32, delta: i64) -> Option<Vec<u8>> {
    let mut buf = header.to_vec();
    let mut changed = false;
    if let Some(n) = rewrite_elements(&buf, b"sheetView", &mut |e| {
        rewrite_attr(e, b"topLeftCell", |v| shift_view_ref(v, axis, at, delta))
    }) {
        buf = n;
        changed = true;
    }
    if let Some(n) = rewrite_elements(&buf, b"pane", &mut |e| {
        rewrite_attr(e, b"topLeftCell", |v| shift_view_ref(v, axis, at, delta))
    }) {
        buf = n;
        changed = true;
    }
    if let Some(n) = rewrite_elements(&buf, b"selection", &mut |e| {
        rewrite_selection(e, axis, at, delta)
    }) {
        buf = n;
        changed = true;
    }
    if changed { Some(buf) } else { None }
}

/// A selection element carries both `activeCell` and `sqref`; rewrite both in
/// one pass so their byte offsets stay consistent.
fn rewrite_selection(elem: &[u8], axis: Axis, at: u32, delta: i64) -> ElemEdit {
    let tag_end = memchr::memchr(b'>', elem).unwrap_or(elem.len());
    let mut new_tag = elem[..=tag_end].to_vec();
    let mut changed = false;
    for attr in [&b"activeCell"[..], &b"sqref"[..]] {
        let Some((vs, ve)) = attr_value_span(&new_tag, attr) else {
            continue;
        };
        let val = new_tag[vs..ve].to_vec();
        if let Some(shifted) = shift_view_ref(&val, axis, at, delta) {
            if shifted != val {
                let mut t = Vec::with_capacity(new_tag.len() + shifted.len());
                t.extend_from_slice(&new_tag[..vs]);
                t.extend_from_slice(&shifted);
                t.extend_from_slice(&new_tag[ve..]);
                new_tag = t;
                changed = true;
            }
        }
    }
    if !changed {
        ElemEdit::Keep
    } else {
        let mut out = Vec::with_capacity(new_tag.len() + elem.len());
        out.extend_from_slice(&new_tag);
        out.extend_from_slice(&elem[tag_end + 1..]);
        ElemEdit::Replace(out)
    }
}

// ----------------------------------------------------------------------------
// Sheet tail: mergeCells
// ----------------------------------------------------------------------------

fn fixup_merge_cells(tail: &[u8], axis: Axis, at: u32, delta: i64) -> Option<Vec<u8>> {
    let Some(s) = find_element(tail, b"mergeCells", 0) else {
        return None;
    };
    let Some(gt_rel) = memchr::memchr(b'>', &tail[s..]) else {
        return None;
    };
    let gt = s + gt_rel;
    if gt > s && tail[gt - 1] == b'/' {
        return None; // <mergeCells/> empty
    }
    let close_tag = b"</mergeCells>";
    let Some(close_rel) = memchr::memmem::find(&tail[gt..], close_tag) else {
        return None;
    };
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
        match shift_merge_ref(mcell, axis, at, delta) {
            MergeOutcome::Keep => survivors.push(mcell.to_vec()),
            MergeOutcome::Replace(n) => {
                survivors.push(n);
                changed = true;
            }
            MergeOutcome::Drop => {
                changed = true;
            }
        }
    }

    if survivors.is_empty() {
        if changed {
            // Drop the whole mergeCells block.
            let mut out = Vec::with_capacity(tail.len());
            out.extend_from_slice(&tail[..s]);
            out.extend_from_slice(&tail[close + close_tag.len()..]);
            Some(out)
        } else {
            None
        }
    } else {
        // Rebuild the open tag with a corrected count (Excel ignores it, but a
        // stale count is a lie we don't want to ship).
        let mut new_open = tail[s..=gt].to_vec();
        let mut block_changed = changed;
        if let Some((vs, ve)) = attr_value_span(&new_open, b"count") {
            let count = survivors.len().to_string();
            if &new_open[vs..ve] != count.as_bytes() {
                let mut t = Vec::with_capacity(new_open.len());
                t.extend_from_slice(&new_open[..vs]);
                t.extend_from_slice(count.as_bytes());
                t.extend_from_slice(&new_open[ve..]);
                new_open = t;
                block_changed = true;
            }
        }
        if !block_changed {
            return None;
        }
        let mut new_block = Vec::with_capacity(block.len() + 8);
        new_block.extend_from_slice(&new_open);
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
}

fn shift_merge_ref(mcell: &[u8], axis: Axis, at: u32, delta: i64) -> MergeOutcome {
    let tag_end = memchr::memchr(b'>', mcell).unwrap_or(mcell.len());
    let tag = &mcell[..=tag_end];
    let Some((vs, ve)) = attr_value_span(tag, b"ref") else {
        return MergeOutcome::Keep;
    };
    let val = &tag[vs..ve];
    let policy = match axis {
        // Both axes trim rather than drop. A merge is a rectangle; deleting its
        // first row leaves rows 3..n, which shift up into a still-valid range.
        // Nothing in OOXML ties a merge to the row that happened to hold the
        // value, and treating the two axes differently has no basis in the
        // format. Dropping is also the unrecoverable direction: a user who
        // wanted the merge gone can remove it, one who lost it cannot get it
        // back. A merge wholly inside the band is still dropped.
        // NOTE: not yet confirmed against real Excel — see TIER01_PLAN.md.
        Axis::Row => BandPolicy::Clamp,
        Axis::Col => BandPolicy::Clamp,
    };
    match shift_a1_range(val, axis, at, delta, policy) {
        RangeShift::Unchanged => MergeOutcome::Keep,
        RangeShift::Dropped => MergeOutcome::Drop,
        RangeShift::Changed(new_val) => {
            // A merge reduced to a single cell is invalid in Excel: remove it.
            if is_single_cell_text(&new_val) {
                return MergeOutcome::Drop;
            }
            let mut out = Vec::with_capacity(mcell.len() + new_val.len());
            out.extend_from_slice(&mcell[..vs]);
            out.extend_from_slice(&new_val);
            out.extend_from_slice(&mcell[ve..]);
            MergeOutcome::Replace(out)
        }
    }
}

// ----------------------------------------------------------------------------
// Sheet tail: hyperlinks, autoFilter
// ----------------------------------------------------------------------------

fn fixup_hyperlinks(tail: &[u8], axis: Axis, at: u32, delta: i64) -> Option<Vec<u8>> {
    rewrite_elements(tail, b"hyperlink", &mut |e| {
        rewrite_range_attr(e, b"ref", axis, at, delta, BandPolicy::Drop)
    })
}

fn fixup_auto_filter(tail: &[u8], axis: Axis, at: u32, delta: i64) -> Option<Vec<u8>> {
    let policy = match axis {
        Axis::Row => BandPolicy::Drop, // header row deleted -> filter dropped (Excel does too)
        Axis::Col => BandPolicy::Clamp,
    };
    rewrite_elements(tail, b"autoFilter", &mut |e| {
        rewrite_range_attr(e, b"ref", axis, at, delta, policy)
    })
}

// ----------------------------------------------------------------------------
// Sheet tail: dataValidations, conditionalFormatting
// ----------------------------------------------------------------------------

fn fixup_data_validations(tail: &[u8], axis: Axis, at: u32, delta: i64) -> Option<Vec<u8>> {
    rewrite_elements(tail, b"dataValidation", &mut |elem| {
        let tag_end = memchr::memchr(b'>', elem).unwrap_or(elem.len());
        let tag = &elem[..=tag_end];

        let mut new_elem = elem.to_vec();
        let mut changed = false;
        let mut drop_element = false;

        // sqref is a space-separated list of ranges.
        if let Some((vs, ve)) = attr_value_span(tag, b"sqref") {
            let val = &tag[vs..ve];
            match shift_multi_ref(val, axis, at, delta, BandPolicy::Drop, true) {
                Some(shifted) if shifted.is_empty() => drop_element = true,
                Some(shifted) if shifted != val => {
                    let mut t = Vec::with_capacity(new_elem.len() + shifted.len());
                    t.extend_from_slice(&new_elem[..vs]);
                    t.extend_from_slice(&shifted);
                    t.extend_from_slice(&new_elem[ve..]);
                    new_elem = t;
                    changed = true;
                }
                _ => {}
            }
        }
        if drop_element {
            return ElemEdit::Drop;
        }

        // formula1 / formula2 are child-element formula TEXT.
        for fname in [b"formula1", b"formula2"] {
            if let Some(n) = shift_child_formula(&new_elem, fname, axis, at, delta) {
                new_elem = n;
                changed = true;
            }
        }
        if changed {
            ElemEdit::Replace(new_elem)
        } else {
            ElemEdit::Keep
        }
    })
}

fn fixup_conditional_formatting(tail: &[u8], axis: Axis, at: u32, delta: i64) -> Option<Vec<u8>> {
    rewrite_elements(tail, b"conditionalFormatting", &mut |elem| {
        let tag_end = memchr::memchr(b'>', elem).unwrap_or(elem.len());
        let tag = &elem[..=tag_end];
        let mut out = elem.to_vec();
        let mut changed = false;

        // sqref on the block's open tag; empty -> drop the whole block.
        if let Some((vs, ve)) = attr_value_span(tag, b"sqref") {
            let val = &tag[vs..ve];
            match shift_multi_ref(val, axis, at, delta, BandPolicy::Drop, true) {
                Some(shifted) if shifted.is_empty() => return ElemEdit::Drop,
                Some(shifted) if shifted != val => {
                    let mut t = Vec::with_capacity(out.len() + shifted.len());
                    t.extend_from_slice(&out[..vs]);
                    t.extend_from_slice(&shifted);
                    t.extend_from_slice(&out[ve..]);
                    out = t;
                    changed = true;
                }
                _ => {}
            }
        }

        // cfRule <formula> bodies: shift text via refshift. Collect spans on the
        // (possibly re-sized) buffer first, then edit back-to-front so offsets
        // stay valid.
        let new_tag_end = memchr::memchr(b'>', &out).unwrap_or(out.len());
        let mut spans = Vec::new();
        let mut pos = new_tag_end + 1;
        while pos < out.len() {
            let Some(fs) = find_element(&out[pos..], b"formula", 0) else {
                break;
            };
            let fs = pos + fs;
            let Some(gt_rel) = memchr::memchr(b'>', &out[fs..]) else {
                break;
            };
            let fgt = fs + gt_rel;
            if fgt > fs && out[fgt - 1] == b'/' {
                pos = fgt + 1;
                continue;
            }
            let Some(close_rel) = memchr::memmem::find(&out[fgt..], b"</formula>") else {
                break;
            };
            let fclose = fgt + close_rel;
            spans.push((fgt + 1, fclose));
            pos = fclose + b"</formula>".len();
        }
        for (ts, te) in spans.into_iter().rev() {
            if let Ok(s) = std::str::from_utf8(&out[ts..te]) {
                if let Cow::Owned(ns) = shift_refs(s, axis, at, delta) {
                    let mut t = Vec::with_capacity(out.len() + ns.len());
                    t.extend_from_slice(&out[..ts]);
                    t.extend_from_slice(ns.as_bytes());
                    t.extend_from_slice(&out[te..]);
                    out = t;
                    changed = true;
                }
            }
        }

        if changed {
            ElemEdit::Replace(out)
        } else {
            ElemEdit::Keep
        }
    })
}

/// Shift one `<formula1>` / `<formula2>` child's TEXT via refshift.
fn shift_child_formula(
    elem: &[u8],
    name: &[u8],
    axis: Axis,
    at: u32,
    delta: i64,
) -> Option<Vec<u8>> {
    let open = format!("<{}", String::from_utf8_lossy(name));
    let close = format!("</{}>", String::from_utf8_lossy(name));
    let o = memchr::memmem::find(elem, open.as_bytes())?;
    if !matches!(
        elem.get(o + open.len()),
        Some(&b'>') | Some(&b' ') | Some(&b'/')
    ) {
        return None;
    }
    let gt = o + memchr::memchr(b'>', &elem[o..])?;
    if gt > o && elem[gt - 1] == b'/' {
        return None;
    }
    let close_at = memchr::memmem::find(&elem[gt..], close.as_bytes())? + gt;
    let text = &elem[gt + 1..close_at];
    let s = std::str::from_utf8(text).ok()?;
    match shift_refs(s, axis, at, delta) {
        Cow::Borrowed(_) => None,
        Cow::Owned(ns) => {
            let mut out = Vec::with_capacity(elem.len() + ns.len());
            out.extend_from_slice(&elem[..=gt]);
            out.extend_from_slice(ns.as_bytes());
            out.extend_from_slice(&elem[close_at..]);
            Some(out)
        }
    }
}

// ----------------------------------------------------------------------------
// Sheet tail: rowBreaks / colBreaks (each on its own axis)
// ----------------------------------------------------------------------------

fn fixup_breaks(tail: &[u8], axis: Axis, at: u32, delta: i64) -> Option<Vec<u8>> {
    let name: &[u8] = match axis {
        Axis::Row => b"rowBreaks",
        Axis::Col => b"colBreaks",
    };
    let Some(s) = find_element(tail, name, 0) else {
        return None;
    };
    let Some(gt_rel) = memchr::memchr(b'>', &tail[s..]) else {
        return None;
    };
    let gt = s + gt_rel;
    if gt > s && tail[gt - 1] == b'/' {
        return None;
    }
    let close_tag = closing_tag(name);
    let Some(close_rel) = memchr::memmem::find(&tail[gt..], &close_tag) else {
        return None;
    };
    let close = gt + close_rel;
    let block = &tail[gt + 1..close];

    let mut survivors: Vec<Vec<u8>> = Vec::new();
    let mut changed = false;
    let mut pos = 0usize;
    while let Some(bs) = find_element(block, b"brk", pos) {
        let bte_rel = memchr::memchr(b'>', &block[bs..])?;
        let bte = bs + bte_rel;
        let self_close = bte > bs && block[bte - 1] == b'/';
        let bend = if self_close {
            bte + 1
        } else {
            let cr = memchr::memmem::find(&block[bte..], b"</brk>")?;
            bte + cr + b"</brk>".len()
        };
        let brk = &block[bs..bend];
        pos = bend;

        let tag = &block[bs..bte];
        match attr_value_span(tag, b"id") {
            Some((vs, ve)) => {
                let id: u32 = std::str::from_utf8(&tag[vs..ve])
                    .ok()?
                    .trim()
                    .parse()
                    .ok()?;
                match shift_index(axis, id, at, delta) {
                    None => {
                        changed = true; // break row/col deleted
                    }
                    Some(nid) if nid != id => {
                        let mut o = Vec::with_capacity(brk.len() + 8);
                        o.extend_from_slice(&block[bs..bs + vs]);
                        let mut ib = itoa::Buffer::new();
                        o.extend_from_slice(ib.format(nid).as_bytes());
                        o.extend_from_slice(&block[bs + ve..bend]);
                        survivors.push(o);
                        changed = true;
                    }
                    Some(_) => survivors.push(brk.to_vec()),
                }
            }
            None => survivors.push(brk.to_vec()),
        }
    }

    if !changed {
        return None;
    }
    if survivors.is_empty() {
        let mut out = Vec::with_capacity(tail.len());
        out.extend_from_slice(&tail[..s]);
        out.extend_from_slice(&tail[close + close_tag.len()..]);
        return Some(out);
    }

    // Rebuild open tag with corrected count / manualBreakCount.
    let mut new_open = tail[s..=gt].to_vec();
    for attr in [&b"count"[..], &b"manualBreakCount"[..]] {
        let Some((vs, ve)) = attr_value_span(&new_open, attr) else {
            continue;
        };
        let n = if attr == b"count" {
            survivors.len()
        } else {
            survivors
                .iter()
                .filter(|b| memchr::memmem::find(b, b"man=\"1\"").is_some())
                .count()
        };
        let ns = n.to_string();
        if &new_open[vs..ve] != ns.as_bytes() {
            let mut t = Vec::with_capacity(new_open.len());
            t.extend_from_slice(&new_open[..vs]);
            t.extend_from_slice(ns.as_bytes());
            t.extend_from_slice(&new_open[ve..]);
            new_open = t;
        }
    }

    let mut out = Vec::with_capacity(tail.len() + 8);
    out.extend_from_slice(&tail[..s]);
    out.extend_from_slice(&new_open);
    for surv in &survivors {
        out.extend_from_slice(surv);
    }
    out.extend_from_slice(&tail[close..]);
    Some(out)
}

// ----------------------------------------------------------------------------
// Table inner autoFilter
// ----------------------------------------------------------------------------

fn fixup_table_inner_auto_filter(body: &[u8], axis: Axis, at: u32, delta: i64) -> Option<Vec<u8>> {
    let Some(s) = find_element(body, b"autoFilter", 0) else {
        return None;
    };
    let gt_rel = memchr::memchr(b'>', &body[s..])?;
    let gt = s + gt_rel;
    let self_close = gt > s && body[gt - 1] == b'/';
    let end = if self_close {
        gt + 1
    } else {
        let cr = memchr::memmem::find(&body[gt..], b"</autoFilter>")?;
        gt + cr + b"</autoFilter>".len()
    };
    let elem = &body[s..end];
    match rewrite_range_attr(elem, b"ref", axis, at, delta, BandPolicy::Clamp) {
        ElemEdit::Keep => None,
        ElemEdit::Drop => {
            let mut out = Vec::with_capacity(body.len());
            out.extend_from_slice(&body[..s]);
            out.extend_from_slice(&body[end..]);
            Some(out)
        }
        ElemEdit::Replace(r) => {
            let mut out = Vec::with_capacity(body.len() + r.len());
            out.extend_from_slice(&body[..s]);
            out.extend_from_slice(&r);
            out.extend_from_slice(&body[end..]);
            Some(out)
        }
    }
}

// ----------------------------------------------------------------------------
// Workbook defined names
// ----------------------------------------------------------------------------

fn shift_defined_name_value(
    value: &[u8],
    sheet_name: &str,
    axis: Axis,
    at: u32,
    delta: i64,
) -> Option<Vec<u8>> {
    if value.first() == Some(&b'[') {
        return None; // external reference [1]...
    }
    let (bang, qualifier) = extract_qualifier(value)?;
    let mut scratch = Vec::new();
    let decoded = decode_bytes(qualifier, &mut scratch);
    if !decoded.eq_ignore_ascii_case(sheet_name.as_bytes()) {
        return None; // some other sheet
    }
    let rest = &value[bang + 1..];
    let shifted = shift_name_value(rest, axis, at, delta)?;
    let mut out = Vec::with_capacity(value.len() + shifted.len());
    out.extend_from_slice(&value[..=bang]);
    out.extend_from_slice(&shifted);
    Some(out)
}

/// Extract the leading `'Sheet'!` or `Sheet!` qualifier. Returns the byte index
/// of `!` and the raw qualifier bytes (excluding the `!`).
fn extract_qualifier(value: &[u8]) -> Option<(usize, &[u8])> {
    if value.get(0) == Some(&b'\'') {
        let mut i = 1;
        while i < value.len() {
            if value[i] == b'\'' {
                if value.get(i + 1) == Some(&b'\'') {
                    i += 2;
                    continue;
                }
                break;
            }
            i += 1;
        }
        if i >= value.len() {
            return None;
        }
        if value.get(i + 1) != Some(&b'!') {
            return None;
        }
        Some((i + 1, &value[1..i]))
    } else {
        let bang = memchr::memchr(b'!', value)?;
        if bang == 0 {
            return None;
        }
        Some((bang, &value[..bang]))
    }
}

/// Shift the value AFTER the sheet qualifier. Pure ranges shift structurally
/// (`$` cosmetic, whole-delete becomes `#REF!`); anything else is formula text
/// routed through `refshift`.
fn shift_name_value(val: &[u8], axis: Axis, at: u32, delta: i64) -> Option<Vec<u8>> {
    let core = if val.first() == Some(&b'=') {
        &val[1..]
    } else {
        val
    };
    if parse_a1_range(core).is_some() {
        return match shift_a1_range(core, axis, at, delta, BandPolicy::Clamp) {
            RangeShift::Changed(r) => Some(r),
            RangeShift::Unchanged => None,
            RangeShift::Dropped => Some(b"#REF!".to_vec()),
        };
    }
    let text = std::str::from_utf8(core).ok()?;
    match shift_refs(text, axis, at, delta) {
        Cow::Borrowed(_) => None,
        Cow::Owned(s) => Some(s.into_bytes()),
    }
}

// ----------------------------------------------------------------------------
// Pivot parts: pivotCacheDefinition + pivotTableDefinition
// ----------------------------------------------------------------------------

/// Fix up one pivot cache definition part (`xl/pivotCache/pivotCacheDefinitionN.xml`).
///
/// A pivot cache carries a `<worksheetSource>` naming the sheet and range it was
/// built from. When rows/columns are inserted or deleted in that sheet, the
/// range must shift exactly like every other reference this pass owns (merges,
/// tables, defined names…), or the pivot silently reads the wrong cells after a
/// row insert — the data-loss bug this pass exists to prevent.
///
/// The cache also holds a MATERIALISED copy of the source data (`pivotCacheRecords`
/// and `sharedItems`, including per-field counts and min/max). A shift means
/// that copy is STALE: the root is therefore tagged `refreshOnLoad="1"` so
/// Excel rebuilds records and shared items from the source on open. A stale
/// cache silently showing old numbers is the class of wrong-answer bug this
/// project treats as unacceptable, so the tag is never dropped once a mutation
/// has touched the source. When the source range does NOT move the cache
/// content is untouched and the part stays byte-identical.
///
/// Returns `None` for an invalid shift point (`at == 0`); `Cow::Borrowed` when
/// this cache does not source from `sheet_name`, has no worksheet source, or has
/// nothing to shift.
pub fn fixup_pivot_cache_xml<'a>(
    xml: &'a [u8],
    sheet_name: &str,
    axis: Axis,
    at: u32,
    delta: i64,
) -> Option<Cow<'a, [u8]>> {
    if delta == 0 {
        return Some(Cow::Borrowed(xml));
    }
    if at == 0 {
        return None;
    }
    let ws = find_element(xml, b"worksheetSource", 0)?;
    let ws_gt = ws + memchr::memchr(b'>', &xml[ws..])?;
    let ws_tag = &xml[ws..=ws_gt];

    // Only caches whose source sheet is the one being mutated shift; every
    // other cache in the workbook is untouched. The `sheet` qualifier matches
    // case-insensitively, like Excel, and is quoted when it contains spaces.
    let (ss, se) = attr_value_span(ws_tag, b"sheet")?;
    let mut scratch = Vec::new();
    let decoded = decode_sheet_name(&ws_tag[ss..se], &mut scratch);
    if !decoded.eq_ignore_ascii_case(sheet_name.as_bytes()) {
        return Some(Cow::Borrowed(xml));
    }

    // Shift the source range (Clamp: a partially-deleted range shrinks, a
    // wholly-deleted range becomes #REF!, mirroring defined-name behaviour).
    let mut edits: Vec<(usize, usize, Vec<u8>)> = Vec::new();
    if let Some((rs, re)) = attr_value_span(ws_tag, b"ref") {
        let abs_rs = ws + rs;
        let abs_re = ws + re;
        match shift_a1_range(&ws_tag[rs..re], axis, at, delta, BandPolicy::Clamp) {
            RangeShift::Unchanged => {}
            RangeShift::Dropped => edits.push((abs_rs, abs_re, b"#REF!".to_vec())),
            RangeShift::Changed(r) => edits.push((abs_rs, abs_re, r)),
        }
    }
    if edits.is_empty() {
        // The source range did not move, so the cached records are still exact.
        return Some(Cow::Borrowed(xml));
    }

    // The source range moved, so the materialised cache is stale: tag the root
    // so Excel refreshes records + sharedItems on open.
    let root = find_element(xml, b"pivotCacheDefinition", 0)?;
    let root_gt = root + memchr::memchr(b'>', &xml[root..])?;
    if let Some((vs, ve, bytes)) = set_refresh_on_load_span(&xml[root..=root_gt]) {
        edits.push((root + vs, root + ve, bytes));
    }

    edits.sort_by_key(|(s, _, _)| *s);
    let mut out = Vec::with_capacity(xml.len() + edits.len() * 16);
    let mut pos = 0usize;
    for (s, e, bytes) in edits {
        out.extend_from_slice(&xml[pos..s]);
        out.extend_from_slice(&bytes);
        pos = e;
    }
    out.extend_from_slice(&xml[pos..]);
    Some(Cow::Owned(out))
}

/// Fix up one pivot table part (`xl/pivotTables/pivotTableN.xml`).
///
/// A pivot table's own `<location ref>` points at where the pivot renders on
/// its host sheet; when that sheet is mutated, the location must shift with the
/// grid like every other anchor. The relative offsets (`firstHeaderRow`,
/// `firstDataRow`, `firstDataCol`) are offsets within the pivot and stay as-is.
///
/// Returns `None` for an invalid shift point (`at == 0`); `Cow::Borrowed` when
/// the part has no `<location>` to shift.
pub fn fixup_pivot_table_xml<'a>(
    xml: &'a [u8],
    axis: Axis,
    at: u32,
    delta: i64,
) -> Option<Cow<'a, [u8]>> {
    if delta == 0 {
        return Some(Cow::Borrowed(xml));
    }
    if at == 0 {
        return None;
    }
    let loc = find_element(xml, b"location", 0)?;
    let loc_gt = loc + memchr::memchr(b'>', &xml[loc..])?;
    let loc_tag = &xml[loc..=loc_gt];
    let (rs, re) = attr_value_span(loc_tag, b"ref")?;
    let new_val = match shift_a1_range(&loc_tag[rs..re], axis, at, delta, BandPolicy::Clamp) {
        RangeShift::Unchanged => return Some(Cow::Borrowed(xml)),
        RangeShift::Dropped => b"#REF!".to_vec(),
        RangeShift::Changed(r) => r,
    };
    let abs_rs = loc + rs;
    let abs_re = loc + re;
    let mut out = Vec::with_capacity(xml.len() + new_val.len() + 8);
    out.extend_from_slice(&xml[..abs_rs]);
    out.extend_from_slice(&new_val);
    out.extend_from_slice(&xml[abs_re..]);
    Some(Cow::Owned(out))
}

/// Set `refreshOnLoad="1"` on a pivot cache definition's root tag. Returns
/// `None` when it is already set (byte-identical) or the part has no root.
///
/// `pub(crate)` for the edit/move_range staleness pass in `overlay.rs`.
pub(crate) fn set_pivot_cache_refresh_on_load(xml: &[u8]) -> Option<Vec<u8>> {
    let root = find_element(xml, b"pivotCacheDefinition", 0)?;
    let root_gt = root + memchr::memchr(b'>', &xml[root..])?;
    let (vs, ve, bytes) = set_refresh_on_load_span(&xml[root..=root_gt])?;
    let abs_vs = root + vs;
    let abs_ve = root + ve;
    let mut out = Vec::with_capacity(xml.len() + bytes.len());
    out.extend_from_slice(&xml[..abs_vs]);
    out.extend_from_slice(&bytes);
    out.extend_from_slice(&xml[abs_ve..]);
    Some(out)
}

/// Parse the source range of a pivot cache definition: the decoded `sheet`
/// attribute and the 1-based inclusive rectangle `(r0, c0, r1, c1)` of `ref`.
/// `None` when the cache has no plain worksheet source range.
///
/// `pub(crate)` for the edit/move_range staleness pass in `overlay.rs`.
pub(crate) fn pivot_cache_source_ref(xml: &[u8]) -> Option<(String, (u32, u32, u32, u32))> {
    let ws = find_element(xml, b"worksheetSource", 0)?;
    let ws_gt = ws + memchr::memchr(b'>', &xml[ws..])?;
    let ws_tag = &xml[ws..=ws_gt];
    let (ss, se) = attr_value_span(ws_tag, b"sheet")?;
    let (rs, re) = attr_value_span(ws_tag, b"ref")?;
    let mut scratch = Vec::new();
    let sheet = String::from_utf8_lossy(decode_sheet_name(&ws_tag[ss..se], &mut scratch).as_ref())
        .into_owned();
    let rect = parse_a1_rect(&ws_tag[rs..re])?;
    Some((sheet, rect))
}

/// Decode a `worksheetSource sheet` attribute and normalize it the way Excel
/// names a sheet: XML entities decoded, and a leading/trailing `'` pair (Excel
/// quotes sheet names that contain spaces) stripped, with doubled `''` inside
/// unescaped to a literal apostrophe.
fn decode_sheet_name<'a>(raw: &'a [u8], scratch: &'a mut Vec<u8>) -> Cow<'a, [u8]> {
    let decoded = decode_bytes(raw, scratch);
    if decoded.len() < 2 || decoded[0] != b'\'' || decoded[decoded.len() - 1] != b'\'' {
        return Cow::Borrowed(decoded);
    }
    let inner = &decoded[1..decoded.len() - 1];
    if memchr::memchr(b'\'', inner).is_none() {
        return Cow::Borrowed(inner);
    }
    let mut out = Vec::with_capacity(inner.len());
    let mut i = 0;
    while i < inner.len() {
        if inner[i] == b'\'' && inner.get(i + 1) == Some(&b'\'') {
            out.push(b'\'');
            i += 2;
        } else {
            out.push(inner[i]);
            i += 1;
        }
    }
    Cow::Owned(out)
}

/// Byte span inside a pivot-cache root open tag to edit so `refreshOnLoad="1"`
/// is set, plus the replacement bytes. Returns `None` when already set to `1`.
fn set_refresh_on_load_span(tag: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    if let Some((vs, ve)) = attr_value_span(tag, b"refreshOnLoad") {
        if &tag[vs..ve] == b"1" {
            return None;
        }
        return Some((vs, ve, b"1".to_vec()));
    }
    // Append before the closing `>` (before the `/` of a self-closing tag).
    let ins = if tag.len() >= 2 && tag[tag.len() - 2] == b'/' {
        tag.len() - 2
    } else {
        tag.len() - 1
    };
    Some((ins, ins, b" refreshOnLoad=\"1\"".to_vec()))
}

/// Parse an A1 range into a normalized 1-based inclusive rectangle
/// `(r0, c0, r1, c1)`. `None` for whole-row/whole-column or malformed refs.
fn parse_a1_rect(val: &[u8]) -> Option<(u32, u32, u32, u32)> {
    let parts = parse_a1_range(val)?;
    let mut r0 = u32::MAX;
    let mut c0 = u32::MAX;
    let mut r1 = 0u32;
    let mut c1 = 0u32;
    for p in &parts {
        r0 = r0.min(part_row(p)?);
        c0 = c0.min(part_col(p)?);
        r1 = r1.max(part_row(p)?);
        c1 = c1.max(part_col(p)?);
    }
    if r1 == 0 || c1 == 0 {
        return None;
    }
    Some((r0, c0, r1, c1))
}

// ----------------------------------------------------------------------------
// Shared range machinery
// ----------------------------------------------------------------------------

fn part_row(p: &A1Part) -> Option<u32> {
    match p {
        A1Part::Cell(c) => Some(c.row),
        A1Part::WholeRow(r, _) => Some(*r),
        A1Part::WholeCol(_, _) => None,
    }
}

fn part_col(p: &A1Part) -> Option<u32> {
    match p {
        A1Part::Cell(c) => Some(c.col),
        A1Part::WholeCol(c, _) => Some(*c),
        A1Part::WholeRow(_, _) => None,
    }
}

fn part_axis_val(p: &A1Part, axis: Axis) -> Option<u32> {
    match axis {
        Axis::Row => part_row(p),
        Axis::Col => part_col(p),
    }
}

fn set_part_axis_val(p: &mut A1Part, axis: Axis, v: u32) {
    match (p, axis) {
        (A1Part::Cell(c), Axis::Row) => c.row = v,
        (A1Part::Cell(c), Axis::Col) => c.col = v,
        (A1Part::WholeRow(r, _), Axis::Row) => *r = v,
        (A1Part::WholeCol(c, _), Axis::Col) => *c = v,
        _ => {}
    }
}

/// Parse `A1`, `$A$1`, `$1`, `$A`, and the `:`-joined forms.
fn parse_a1_range(val: &[u8]) -> Option<Vec<A1Part>> {
    let mut parts = Vec::with_capacity(2);
    let mut start = 0;
    for idx in 0..=val.len() {
        if idx == val.len() || val[idx] == b':' {
            parts.push(parse_a1_part(&val[start..idx])?);
            start = idx + 1;
        }
    }
    if parts.is_empty() || parts.len() > 2 {
        return None;
    }
    Some(parts)
}

fn parse_a1_part(s: &[u8]) -> Option<A1Part> {
    let mut i = 0;
    let has_dollar = s.get(i) == Some(&b'$');
    if has_dollar {
        i += 1;
    }
    let ls = i;
    while i < s.len() && s[i].is_ascii_alphabetic() {
        i += 1;
    }
    let le = i;
    let mut abs_row = false;
    if le > ls && s.get(i) == Some(&b'$') {
        abs_row = true;
        i += 1;
    }
    let rs = i;
    while i < s.len() && s[i].is_ascii_digit() {
        i += 1;
    }
    if i != s.len() {
        return None;
    }
    let has_letters = le > ls;
    let has_digits = i > rs;
    if has_letters && has_digits {
        Some(A1Part::Cell(CellRef {
            row: std::str::from_utf8(&s[rs..i]).ok()?.parse().ok()?,
            col: letters_to_index(&s[ls..le])?,
            abs_row,
            abs_col: has_dollar,
        }))
    } else if has_letters {
        Some(A1Part::WholeCol(letters_to_index(&s[ls..le])?, has_dollar))
    } else if has_digits {
        Some(A1Part::WholeRow(
            std::str::from_utf8(&s[rs..i]).ok()?.parse().ok()?,
            has_dollar,
        ))
    } else {
        None
    }
}

/// Shift one A1 range. `Unchanged` when the range neither moves nor is affected;
/// `Dropped` when the policy says it disappears; `Changed` with the new bytes.
fn shift_a1_range(val: &[u8], axis: Axis, at: u32, delta: i64, policy: BandPolicy) -> RangeShift {
    let Some(mut parts) = parse_a1_range(val) else {
        return RangeShift::Unchanged;
    };
    // A part spanning the whole axis (e.g. `A:A` on the row axis) can neither
    // move nor be deleted by this axis's operation.
    if parts.iter().any(|p| part_axis_val(p, axis).is_none()) {
        return RangeShift::Unchanged;
    }
    match apply_axis_shift(&mut parts, axis, at, delta, policy) {
        None => RangeShift::Dropped,
        Some(false) => RangeShift::Unchanged,
        Some(true) => {
            let rebuilt = serialize_range(&parts);
            if rebuilt == val {
                RangeShift::Unchanged
            } else {
                RangeShift::Changed(rebuilt)
            }
        }
    }
}

/// Apply the axis shift to both endpoints. `None` = the range is gone on this
/// axis; `Some(false)` = nothing moved; `Some(true)` = at least one endpoint
/// moved.
fn apply_axis_shift(
    parts: &mut [A1Part],
    axis: Axis,
    at: u32,
    delta: i64,
    policy: BandPolicy,
) -> Option<bool> {
    let n = parts.len();
    let v0 = part_axis_val(&parts[0], axis)?;
    let v1 = part_axis_val(&parts[n - 1], axis)?;
    let (lo, hi) = if v0 <= v1 { (v0, v1) } else { (v1, v0) };
    let lo_prime = shift_index(axis, lo, at, delta);
    let hi_prime = shift_index(axis, hi, at, delta);
    let edge = at.saturating_sub(1).max(1);
    let (lo_prime, hi_prime) = match (lo_prime, hi_prime) {
        (None, None) => match policy {
            BandPolicy::Drop | BandPolicy::Clamp => return None,
            BandPolicy::ClampView => (edge, edge),
        },
        (None, Some(h)) => match policy {
            BandPolicy::Drop => return None,
            BandPolicy::Clamp | BandPolicy::ClampView => (at, h),
        },
        (Some(l), None) => (l, edge),
        (Some(l), Some(h)) => (l, h),
    };
    let (lo_slot, hi_slot) = if v0 <= v1 {
        (0usize, n - 1)
    } else {
        (n - 1, 0)
    };
    let mut changed = false;
    if lo != lo_prime {
        set_part_axis_val(&mut parts[lo_slot], axis, lo_prime);
        changed = true;
    }
    if hi != hi_prime {
        set_part_axis_val(&mut parts[hi_slot], axis, hi_prime);
        changed = true;
    }
    Some(changed)
}

fn serialize_range(parts: &[A1Part]) -> Vec<u8> {
    let mut out = serialize_part(&parts[0]);
    if parts.len() == 2 {
        out.push(b':');
        out.extend_from_slice(&serialize_part(&parts[1]));
    }
    out
}

fn serialize_part(p: &A1Part) -> Vec<u8> {
    let mut out = Vec::with_capacity(8);
    match p {
        A1Part::Cell(c) => {
            if c.abs_col {
                out.push(b'$');
            }
            let mut buf = [0u8; 4];
            out.extend_from_slice(col_letters(c.col, &mut buf));
            if c.abs_row {
                out.push(b'$');
            }
            let mut ib = itoa::Buffer::new();
            out.extend_from_slice(ib.format(c.row).as_bytes());
        }
        A1Part::WholeRow(r, abs) => {
            if *abs {
                out.push(b'$');
            }
            let mut ib = itoa::Buffer::new();
            out.extend_from_slice(ib.format(*r).as_bytes());
        }
        A1Part::WholeCol(c, abs) => {
            if *abs {
                out.push(b'$');
            }
            let mut buf = [0u8; 4];
            out.extend_from_slice(col_letters(*c, &mut buf));
        }
    }
    out
}

/// True when a serialized range is a single cell (an invalid merge).
fn is_single_cell_text(t: &[u8]) -> bool {
    match parse_a1_range(t) {
        Some(parts) => {
            parts.len() == 2
                && part_row(&parts[0]).is_some()
                && part_col(&parts[0]).is_some()
                && part_row(&parts[1]).is_some()
                && part_col(&parts[1]).is_some()
                && part_row(&parts[0]) == part_row(&parts[1])
                && part_col(&parts[0]) == part_col(&parts[1])
        }
        _ => false,
    }
}

/// Shift every whitespace-separated A1 range in a multi-range attribute value
/// (sqref, selection). When `drop_empty`, ranges that vanish are removed and an
/// empty result is returned as `Some(Vec::new())`.
fn shift_multi_ref(
    val: &[u8],
    axis: Axis,
    at: u32,
    delta: i64,
    policy: BandPolicy,
    drop_empty: bool,
) -> Option<Vec<u8>> {
    let mut kept: Vec<Vec<u8>> = Vec::new();
    let mut changed = false;
    for token in val.split(|&b| b == b' ') {
        if token.is_empty() {
            continue;
        }
        match shift_a1_range(token, axis, at, delta, policy) {
            RangeShift::Unchanged => kept.push(token.to_vec()),
            RangeShift::Changed(r) => {
                kept.push(r);
                changed = true;
            }
            RangeShift::Dropped => {
                if !drop_empty {
                    kept.push(token.to_vec());
                } else {
                    changed = true;
                }
            }
        }
    }
    if !changed {
        return None;
    }
    if kept.is_empty() {
        return Some(Vec::new());
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

/// Shift a view-state ref (pane topLeftCell, selection activeCell/sqref): clamp
/// into the grid, never drop.
fn shift_view_ref(v: &[u8], axis: Axis, at: u32, delta: i64) -> Option<Vec<u8>> {
    shift_multi_ref(v, axis, at, delta, BandPolicy::ClampView, false)
}

// ----------------------------------------------------------------------------
// XML element rewriting
// ----------------------------------------------------------------------------

fn closing_tag(name: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(name.len() + 3);
    v.extend_from_slice(b"</");
    v.extend_from_slice(name);
    v.push(b'>');
    v
}

/// Rebuild `xml` rewriting every `<name>` element through `f`. Returns `None`
/// when nothing changed.
fn rewrite_elements(
    xml: &[u8],
    name: &[u8],
    f: &mut dyn FnMut(&[u8]) -> ElemEdit,
) -> Option<Vec<u8>> {
    let close = closing_tag(name);
    let mut out: Vec<u8> = Vec::new();
    let mut pos = 0usize;
    let mut changed = false;
    let mut started = false;
    let mut last = 0usize;
    loop {
        let Some(rel) = find_element(&xml[pos..], name, 0) else {
            break;
        };
        let s = pos + rel;
        let Some(gt_rel) = memchr::memchr(b'>', &xml[s..]) else {
            break;
        };
        let tag_end = s + gt_rel;
        let self_close = tag_end > s && xml[tag_end - 1] == b'/';
        let end = if self_close {
            tag_end + 1
        } else {
            let Some(close_rel) = memchr::memmem::find(&xml[tag_end..], &close) else {
                break;
            };
            tag_end + close_rel + close.len()
        };
        let elem = &xml[s..end];
        if !started {
            out.extend_from_slice(&xml[..s]);
            started = true;
        } else {
            out.extend_from_slice(&xml[last..s]);
        }
        match f(elem) {
            ElemEdit::Keep => out.extend_from_slice(elem),
            ElemEdit::Drop => {
                changed = true;
            }
            ElemEdit::Replace(r) => {
                out.extend_from_slice(&r);
                changed = true;
            }
        }
        last = end;
        pos = end;
    }
    if !started {
        return None;
    }
    out.extend_from_slice(&xml[last..]);
    if changed { Some(out) } else { None }
}

/// Rewrite one attribute of an element's open tag through `shift`.
fn rewrite_attr(elem: &[u8], attr: &[u8], shift: impl Fn(&[u8]) -> Option<Vec<u8>>) -> ElemEdit {
    let tag_end = memchr::memchr(b'>', elem).unwrap_or(elem.len());
    let tag = &elem[..=tag_end];
    let Some((vs, ve)) = attr_value_span(tag, attr) else {
        return ElemEdit::Keep;
    };
    match shift(&tag[vs..ve]) {
        Some(new_val) if new_val != tag[vs..ve] => {
            let mut out = Vec::with_capacity(elem.len() + new_val.len());
            out.extend_from_slice(&elem[..vs]);
            out.extend_from_slice(&new_val);
            out.extend_from_slice(&elem[ve..]);
            ElemEdit::Replace(out)
        }
        _ => ElemEdit::Keep,
    }
}

/// Rewrite a single range-valued attribute; a dropped range drops the element.
fn rewrite_range_attr(
    elem: &[u8],
    attr: &[u8],
    axis: Axis,
    at: u32,
    delta: i64,
    policy: BandPolicy,
) -> ElemEdit {
    let tag_end = memchr::memchr(b'>', elem).unwrap_or(elem.len());
    let tag = &elem[..=tag_end];
    let Some((vs, ve)) = attr_value_span(tag, attr) else {
        return ElemEdit::Keep;
    };
    match shift_a1_range(&tag[vs..ve], axis, at, delta, policy) {
        RangeShift::Unchanged => ElemEdit::Keep,
        RangeShift::Dropped => ElemEdit::Drop,
        RangeShift::Changed(new_val) => {
            let mut out = Vec::with_capacity(elem.len() + new_val.len());
            out.extend_from_slice(&elem[..vs]);
            out.extend_from_slice(&new_val);
            out.extend_from_slice(&elem[ve..]);
            ElemEdit::Replace(out)
        }
    }
}

/// Wire one chained metadata pass into a `Cow` buffer.
fn apply<'a>(cur: &mut Cow<'a, [u8]>, f: impl Fn(&[u8]) -> Option<Vec<u8>>) {
    if let Some(next) = f(cur.as_ref()) {
        *cur = Cow::Owned(next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A worksheet whose header/tail carry arbitrary metadata; sheetData is a
    /// single row so the pass has a grid to anchor on.
    fn sheet(header_extra: &str, tail: &str) -> String {
        format!(
            "<worksheet xmlns=\"s\">{header_extra}<sheetData><row r=\"1\"><c r=\"A1\"><v>1</v></c></row></sheetData>{tail}</worksheet>"
        )
    }

    fn fix_sheet(x: &str, axis: Axis, at: u32, delta: i64) -> String {
        let out = fixup_sheet_xml(x.as_bytes(), axis, at, delta)
            .expect("fixup_sheet_xml refused unexpectedly");
        String::from_utf8(out.into_owned()).unwrap()
    }

    fn fix_wb(x: &str, name: &str, axis: Axis, at: u32, delta: i64) -> String {
        let out = fixup_workbook_xml(x.as_bytes(), name, axis, at, delta);
        String::from_utf8(out.into_owned()).unwrap()
    }

    fn fix_table(x: &str, axis: Axis, at: u32, delta: i64) -> Option<String> {
        let out = fixup_table_part_xml(x.as_bytes(), axis, at, delta)?;
        Some(String::from_utf8(out.into_owned()).unwrap())
    }

    // ------------------------------------------------------------------
    // Merges (the case openpyxl gets wrong)
    // ------------------------------------------------------------------

    #[test]
    fn merge_shrinks_when_rows_deleted_inside() {
        let x = sheet(
            "",
            r#"<mergeCells count="1"><mergeCell ref="A2:A5"/></mergeCells>"#,
        );
        let out = fix_sheet(&x, Axis::Row, 3, -2);
        assert!(out.contains(r#"<mergeCell ref="A2:A3"/>"#), "{out}");
        assert!(out.contains(r#"count="1""#), "{out}");
    }

    #[test]
    fn merge_bottom_clamped_when_band_overruns_bottom() {
        // Bottom (5) falls inside the band [4,6): clamp to at-1 == 3. This is
        // the off-by-one the plan called out as most likely wrong.
        let x = sheet(
            "",
            r#"<mergeCells count="1"><mergeCell ref="A2:A5"/></mergeCells>"#,
        );
        let out = fix_sheet(&x, Axis::Row, 4, -3);
        assert!(out.contains(r#"<mergeCell ref="A2:A3"/>"#), "{out}");
    }

    #[test]
    fn merge_first_row_deleted_trims_not_drops() {
        // Deleting the first row of A2:A5 leaves rows 3..5, which shift up to
        // 2..4. The merge is a rectangle and survives as A2:A4. Dropping it
        // would silently destroy formatting the user cannot recover.
        let x = sheet(
            "",
            r#"<mergeCells count="1"><mergeCell ref="A2:A5"/></mergeCells>"#,
        );
        let out = fix_sheet(&x, Axis::Row, 2, -1);
        assert!(out.contains(r#"<mergeCell ref="A2:A4"/>"#), "{out}");
    }

    #[test]
    fn merge_wholly_inside_delete_band_is_still_dropped() {
        let x = sheet(
            "",
            r#"<mergeCells count="1"><mergeCell ref="A2:A3"/></mergeCells>"#,
        );
        let out = fix_sheet(&x, Axis::Row, 2, -2);
        assert!(!out.contains("mergeCell"), "{out}");
    }

    #[test]
    fn merge_fully_covered_removes_merge() {
        let x = sheet(
            "",
            r#"<mergeCells count="1"><mergeCell ref="A2:A5"/></mergeCells>"#,
        );
        let out = fix_sheet(&x, Axis::Row, 2, -4);
        assert!(!out.contains("mergeCell"), "{out}");
    }

    #[test]
    fn merge_collapse_to_single_cell_is_removed() {
        let x = sheet(
            "",
            r#"<mergeCells count="1"><mergeCell ref="A2:A5"/></mergeCells>"#,
        );
        let out = fix_sheet(&x, Axis::Row, 3, -3);
        assert!(!out.contains("mergeCell"), "{out}");
    }

    #[test]
    fn merge_multi_col_single_row_survives_row_ops() {
        let x = sheet(
            "",
            r#"<mergeCells count="1"><mergeCell ref="A1:D1"/></mergeCells>"#,
        );
        let out = fix_sheet(&x, Axis::Row, 5, 2);
        assert!(out.contains(r#"<mergeCell ref="A1:D1"/>"#), "{out}");
    }

    #[test]
    fn merge_below_band_shifts_down() {
        let x = sheet(
            "",
            r#"<mergeCells count="1"><mergeCell ref="A5:A6"/></mergeCells>"#,
        );
        let out = fix_sheet(&x, Axis::Row, 2, -1);
        assert!(out.contains(r#"<mergeCell ref="A4:A5"/>"#), "{out}");
    }

    #[test]
    fn merge_above_band_untouched() {
        let x = sheet(
            "",
            r#"<mergeCells count="1"><mergeCell ref="A1:A2"/></mergeCells>"#,
        );
        let out = fix_sheet(&x, Axis::Row, 5, -1);
        assert!(out.contains(r#"<mergeCell ref="A1:A2"/>"#), "{out}");
    }

    #[test]
    fn merge_grows_on_insert_inside() {
        let x = sheet(
            "",
            r#"<mergeCells count="1"><mergeCell ref="A1:A3"/></mergeCells>"#,
        );
        let out = fix_sheet(&x, Axis::Row, 2, 1);
        assert!(out.contains(r#"<mergeCell ref="A1:A4"/>"#), "{out}");
    }

    #[test]
    fn merge_trimmed_by_column_delete() {
        let x = sheet(
            "",
            r#"<mergeCells count="1"><mergeCell ref="B2:C3"/></mergeCells>"#,
        );
        let out = fix_sheet(&x, Axis::Col, 2, -1);
        assert!(out.contains(r#"<mergeCell ref="B2:B3"/>"#), "{out}");
    }

    #[test]
    fn merge_removed_when_whole_column_span_deleted() {
        let x = sheet(
            "",
            r#"<mergeCells count="1"><mergeCell ref="B2:C3"/></mergeCells>"#,
        );
        let out = fix_sheet(&x, Axis::Col, 2, -2);
        assert!(!out.contains("mergeCell"), "{out}");
    }

    #[test]
    fn all_merges_gone_drops_the_block() {
        let x = sheet(
            "",
            r#"<mergeCells count="2"><mergeCell ref="A1:A2"/><mergeCell ref="A3:A4"/></mergeCells>"#,
        );
        let out = fix_sheet(&x, Axis::Row, 1, -4);
        assert!(!out.contains("mergeCell"), "{out}");
        assert!(!out.contains("mergeCells"), "{out}");
    }

    // ------------------------------------------------------------------
    // autoFilter, hyperlinks
    // ------------------------------------------------------------------

    #[test]
    fn auto_filter_ref_shrinks() {
        let x = sheet("", r#"<autoFilter ref="A1:D6"/>"#);
        let out = fix_sheet(&x, Axis::Row, 2, -2);
        assert!(out.contains(r#"<autoFilter ref="A1:D4"/>"#), "{out}");
    }

    #[test]
    fn auto_filter_dropped_when_header_row_deleted() {
        let x = sheet("", r#"<autoFilter ref="A1:D6"/>"#);
        let out = fix_sheet(&x, Axis::Row, 1, -1);
        assert!(!out.contains("autoFilter"), "{out}");
    }

    #[test]
    fn hyperlink_anchor_deleted_drops_element() {
        let x = sheet(
            "",
            r#"<hyperlinks><hyperlink ref="A2" r:id="rId1"/></hyperlinks>"#,
        );
        let out = fix_sheet(&x, Axis::Row, 2, -1);
        assert!(!out.contains("<hyperlink "), "{out}");
    }

    #[test]
    fn hyperlink_shifted_when_above_band() {
        let x = sheet(
            "",
            r#"<hyperlinks><hyperlink ref="A2" r:id="rId1"/></hyperlinks>"#,
        );
        let out = fix_sheet(&x, Axis::Row, 1, -1);
        assert!(out.contains(r#"ref="A1""#), "{out}");
    }

    // ------------------------------------------------------------------
    // Data validations (multi-range sqref)
    // ------------------------------------------------------------------

    #[test]
    fn dv_sqref_shifts_and_formula_shifts() {
        let x = sheet(
            "",
            r#"<dataValidations count="1"><dataValidation type="list" sqref="A1:A5 C5:D8"><formula1>=$F1:$F10</formula1></dataValidation></dataValidations>"#,
        );
        let out = fix_sheet(&x, Axis::Row, 2, -2);
        assert!(out.contains(r#"sqref="A1:A3 C3:D6""#), "{out}");
        assert!(out.contains(r#"<formula1>=$F1:$F8</formula1>"#), "{out}");
    }

    #[test]
    fn dv_sqref_single_cell_inside_band_drops() {
        let x = sheet(
            "",
            r#"<dataValidations count="1"><dataValidation type="list" sqref="B2 C5:D8"><formula1>=B2</formula1></dataValidation></dataValidations>"#,
        );
        let out = fix_sheet(&x, Axis::Row, 2, -1);
        // B2 deleted from the list; C5:D8 shifts up; the whole DV keeps B-less sqref
        assert!(out.contains(r#"sqref="C4:D7""#), "{out}");
    }

    #[test]
    fn dv_dropped_when_sqref_empties() {
        let x = sheet(
            "",
            r#"<dataValidations count="1"><dataValidation type="list" sqref="B2"><formula1>=B2</formula1></dataValidation></dataValidations>"#,
        );
        let out = fix_sheet(&x, Axis::Row, 2, -1);
        assert!(!out.contains("<dataValidation "), "{out}");
    }

    #[test]
    fn dv_formula_string_literal_untouched() {
        // refshift treats "A1" inside a string literal as text.
        let x = sheet(
            "",
            r#"<dataValidations count="1"><dataValidation type="list" sqref="A1:A3"><formula1>=IF(B5="A1",C5,D5)</formula1></dataValidation></dataValidations>"#,
        );
        let out = fix_sheet(&x, Axis::Row, 2, -1);
        assert!(
            out.contains(r#"<formula1>=IF(B4="A1",C4,D4)</formula1>"#),
            "{out}"
        );
    }

    // ------------------------------------------------------------------
    // Conditional formatting
    // ------------------------------------------------------------------

    #[test]
    fn cf_sqref_shifts_and_formula_shifts() {
        let x = sheet(
            "",
            r#"<conditionalFormatting sqref="A1:A10 B3:C4"><cfRule type="expression" priority="1"><formula>AND($A1>5,B3)</formula></cfRule></conditionalFormatting>"#,
        );
        let out = fix_sheet(&x, Axis::Row, 2, -2);
        assert!(
            out.contains(r#"<conditionalFormatting sqref="A1:A8">"#),
            "{out}"
        );
        assert!(out.contains(r#"<formula>AND($A1>5,B1)</formula>"#), "{out}");
    }

    #[test]
    fn cf_block_dropped_when_sqref_empties() {
        let x = sheet(
            "",
            r#"<conditionalFormatting sqref="B2:C3"><cfRule type="expression" priority="1"><formula>AND($A1>5,B3)</formula></cfRule></conditionalFormatting>"#,
        );
        let out = fix_sheet(&x, Axis::Row, 2, -2);
        assert!(!out.contains("conditionalFormatting"), "{out}");
    }

    // ------------------------------------------------------------------
    // Breaks
    // ------------------------------------------------------------------

    #[test]
    fn row_breaks_shift_and_drop() {
        let x = sheet(
            "",
            r#"<rowBreaks count="3" manualBreakCount="2"><brk id="2" max="16383" man="0"/><brk id="5" max="16383" man="1"/><brk id="7" max="16383" man="1"/></rowBreaks>"#,
        );
        let out = fix_sheet(&x, Axis::Row, 4, -3);
        // band [4,6]: id 5 dropped; id 7 -> 4; id 2 untouched
        assert!(!out.contains(r#"id="5""#), "{out}");
        assert!(out.contains(r#"id="4""#), "{out}");
        assert!(out.contains(r#"id="2""#), "{out}");
        assert!(out.contains(r#"count="2""#), "{out}");
        assert!(out.contains(r#"manualBreakCount="1""#), "{out}");
    }

    #[test]
    fn col_breaks_moved_on_col_axis_only() {
        let x = sheet(
            "",
            r#"<colBreaks count="1" manualBreakCount="1"><brk id="7" max="1048575" man="1"/></colBreaks>"#,
        );
        let out = fix_sheet(&x, Axis::Col, 3, -2);
        assert!(out.contains(r#"id="5""#), "{out}");
        // a row op leaves col breaks alone
        let out2 = fix_sheet(&x, Axis::Row, 3, -2);
        assert!(out2.contains(r#"id="7""#), "{out2}");
    }

    // ------------------------------------------------------------------
    // Panes / selection (view state)
    // ------------------------------------------------------------------

    #[test]
    fn selection_clamps_into_band_edge() {
        let x = sheet(
            r#"<sheetViews><sheetView workbookViewId="0"><selection activeCell="B5" sqref="B5"/></sheetView></sheetViews>"#,
            "",
        );
        let out = fix_sheet(&x, Axis::Row, 3, -2);
        assert!(out.contains(r#"activeCell="B3""#), "{out}");
        assert!(out.contains(r#"sqref="B3""#), "{out}");
    }

    #[test]
    fn pane_top_left_clamps() {
        let x = sheet(
            r#"<sheetViews><sheetView workbookViewId="0"><pane xSplit="1" ySplit="1" topLeftCell="B2" activePane="bottomRight" state="frozen"/></sheetView></sheetViews>"#,
            "",
        );
        let out = fix_sheet(&x, Axis::Row, 1, -1);
        assert!(out.contains(r#"topLeftCell="B1""#), "{out}");
    }

    // ------------------------------------------------------------------
    // Defined names in workbook.xml
    // ------------------------------------------------------------------

    #[test]
    fn defined_name_print_area_shifts() {
        let wb = r#"<workbook><definedNames><definedName name="_xlnm.Print_Area" localSheetId="0">'MetaMain'!$A$1:$D$6</definedName></definedNames></workbook>"#;
        let out = fix_wb(wb, "MetaMain", Axis::Row, 2, -2);
        assert!(out.contains("'MetaMain'!$A$1:$D$4"), "{out}");
    }

    #[test]
    fn defined_name_print_titles_whole_row() {
        let wb = r#"<workbook><definedNames><definedName name="_xlnm.Print_Titles" localSheetId="0">'MetaMain'!$1:$1</definedName></definedNames></workbook>"#;
        let out = fix_wb(wb, "MetaMain", Axis::Row, 1, -1);
        assert!(out.contains("'MetaMain'!#REF!"), "{out}");
    }

    #[test]
    fn defined_name_quoted_sheet_with_space() {
        let wb = r#"<workbook><definedNames><definedName name="_xlnm.Print_Area">'My Sheet'!$A$1:$D$5</definedName></definedNames></workbook>"#;
        let out = fix_wb(wb, "My Sheet", Axis::Row, 2, -2);
        assert!(out.contains("'My Sheet'!$A$1:$D$3"), "{out}");
    }

    #[test]
    fn defined_name_other_sheet_untouched() {
        let wb = r#"<workbook><definedNames><definedName name="OtherName">'Other'!$A$1:$D$6</definedName></definedNames></workbook>"#;
        let out = fix_wb(wb, "MetaMain", Axis::Row, 2, -2);
        assert_eq!(out, wb);
    }

    #[test]
    fn defined_name_case_insensitive_match() {
        let wb = r#"<workbook><definedNames><definedName name="PA">'metaMAIN'!$A$1:$D$6</definedName></definedNames></workbook>"#;
        let out = fix_wb(wb, "MetaMain", Axis::Row, 2, -2);
        assert!(out.contains("'metaMAIN'!$A$1:$D$4"), "{out}");
    }

    #[test]
    fn defined_name_external_skipped() {
        let wb = r#"<workbook><definedNames><definedName name="X" hidden="1">[1]Sheet1!$A$1:$D$6</definedName></definedNames></workbook>"#;
        let out = fix_wb(wb, "MetaMain", Axis::Row, 2, -2);
        assert_eq!(out, wb);
    }

    #[test]
    fn defined_name_whole_area_deleted_becomes_ref() {
        let wb = r#"<workbook><definedNames><definedName name="_xlnm.Print_Area" localSheetId="0">'MetaMain'!$A$1:$D$6</definedName></definedNames></workbook>"#;
        let out = fix_wb(wb, "MetaMain", Axis::Row, 1, -6);
        assert!(out.contains("'MetaMain'!#REF!"), "{out}");
    }

    #[test]
    fn defined_name_shifts_on_column_axis() {
        let wb = r#"<workbook><definedNames><definedName name="_xlnm.Print_Area" localSheetId="0">'MetaMain'!$A$1:$D$6</definedName></definedNames></workbook>"#;
        let out = fix_wb(wb, "MetaMain", Axis::Col, 3, -2);
        assert!(out.contains("'MetaMain'!$A$1:$B$6"), "{out}");
    }

    #[test]
    fn defined_name_whole_column_ref_on_col_axis() {
        let wb = r#"<workbook><definedNames><definedName name="ColName">'MetaMain'!$A:$D</definedName></definedNames></workbook>"#;
        let out = fix_wb(wb, "MetaMain", Axis::Col, 2, -1);
        assert!(out.contains("'MetaMain'!$A:$C"), "{out}");
    }

    #[test]
    fn no_matching_names_borrows() {
        let wb = r#"<workbook><definedNames><definedName name="K">=42</definedName></definedNames></workbook>"#;
        let out = fixup_workbook_xml(wb.as_bytes(), "MetaMain", Axis::Row, 2, -2);
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    // ------------------------------------------------------------------
    // Tables
    // ------------------------------------------------------------------

    #[test]
    fn table_header_row_deleted_refuses() {
        let t = r#"<table id="1" name="Table1" displayName="Table1" ref="A1:D10" totalsRowCount="1" headerRowCount="1"><autoFilter ref="A1:D10"/></table>"#;
        assert!(fix_table(t, Axis::Row, 1, -1).is_none());
    }

    #[test]
    fn table_data_shrink_keeps_totals_row() {
        let t = r#"<table id="1" name="Table1" displayName="Table1" ref="A1:D10" totalsRowCount="1" headerRowCount="1"><autoFilter ref="A1:D10"/></table>"#;
        let out = fix_table(t, Axis::Row, 2, -2).unwrap();
        assert!(out.contains(r#"ref="A1:D8""#), "{out}");
        assert!(out.contains(r#"totalsRowCount="1""#), "{out}");
    }

    #[test]
    fn table_totals_row_deleted_zeroes_count() {
        let t = r#"<table id="1" name="Table1" displayName="Table1" ref="A1:D10" totalsRowCount="1" headerRowCount="1"><autoFilter ref="A1:D10"/></table>"#;
        let out = fix_table(t, Axis::Row, 9, -3).unwrap();
        assert!(out.contains(r#"ref="A1:D8""#), "{out}");
        assert!(out.contains(r#"totalsRowCount="0""#), "{out}");
    }

    #[test]
    fn table_inner_auto_filter_shifts() {
        let t = r#"<table id="1" name="Table1" displayName="Table1" ref="A1:D10" headerRowCount="1"><autoFilter ref="A1:D10"/></table>"#;
        let out = fix_table(t, Axis::Row, 2, -2).unwrap();
        assert!(out.contains(r#"<autoFilter ref="A1:D8"/>"#), "{out}");
    }

    #[test]
    fn table_all_columns_deleted_refuses() {
        let t =
            r#"<table id="1" name="Table1" displayName="Table1" ref="A1:D10" headerRowCount="1"/>"#;
        assert!(fix_table(t, Axis::Col, 1, -4).is_none());
        let ok = fix_table(t, Axis::Col, 3, -2).unwrap();
        assert!(ok.contains(r#"ref="A1:B10""#), "{ok}");
    }

    // ------------------------------------------------------------------
    // Composition and pass-level behaviour
    // ------------------------------------------------------------------

    #[test]
    fn no_dependent_features_borrows() {
        let x = sheet("", "");
        let out = fixup_sheet_xml(x.as_bytes(), Axis::Row, 2, -1).unwrap();
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn fixup_composes_with_the_splice() {
        use crate::turbo::mutate::shift_rows;
        let x = sheet(
            "",
            r#"<mergeCells count="1"><mergeCell ref="A2:A5"/></mergeCells>"#,
        );
        let spliced = shift_rows(x.as_bytes(), 3, -2).unwrap().into_owned();
        let out = fixup_sheet_xml(&spliced, Axis::Row, 3, -2).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(out.contains(r#"<mergeCell ref="A2:A3"/>"#), "{out}");
        // sheetData rows still shifted by the splice
        assert!(out.contains(r#"<row r="1""#), "{out}");
    }

    #[test]
    fn merge_delete_then_insert_roundtrips() {
        let x = sheet(
            "",
            r#"<mergeCells count="1"><mergeCell ref="A2:A5"/></mergeCells>"#,
        );
        let del = fixup_sheet_xml(x.as_bytes(), Axis::Row, 3, -2)
            .unwrap()
            .into_owned();
        assert!(
            String::from_utf8_lossy(&del).contains(r#"<mergeCell ref="A2:A3"/>"#),
            "delete shrank the merge"
        );
        let ins = fixup_sheet_xml(&del, Axis::Row, 3, 2).unwrap();
        let ins = String::from_utf8(ins.into_owned()).unwrap();
        assert!(ins.contains(r#"<mergeCell ref="A2:A5"/>"#), "{ins}");
    }

    #[test]
    fn combined_features_shift_in_one_pass() {
        let x = sheet(
            "",
            r#"<mergeCells count="1"><mergeCell ref="A2:A5"/></mergeCells><hyperlinks><hyperlink ref="B5" r:id="rId1"/></hyperlinks><rowBreaks count="1" manualBreakCount="1"><brk id="9" max="16383" man="1"/></rowBreaks>"#,
        );
        let out = fix_sheet(&x, Axis::Row, 3, -2);
        assert!(out.contains(r#"<mergeCell ref="A2:A3"/>"#), "{out}");
        assert!(out.contains(r#"ref="B3""#), "{out}");
        assert!(out.contains(r#"id="7""#), "{out}");
    }

    #[test]
    fn invalid_shift_point_refuses() {
        assert!(fixup_sheet_xml(sheet("", "").as_bytes(), Axis::Row, 0, -1).is_none());
    }

    // ------------------------------------------------------------------
    // Pivot cache definition (cacheSource ref) + pivot table location
    // ------------------------------------------------------------------

    fn cache_xml(refr: &str, sheet: &str) -> String {
        format!(
            r#"<pivotCacheDefinition xmlns="x" recordCount="4"><cacheSource type="worksheet"><worksheetSource sheet="{sheet}" ref="{refr}"/></cacheSource><cacheFields count="1"><cacheField name="A"><sharedItems count="1"><s v="x"/></sharedItems></cacheField></cacheFields></pivotCacheDefinition>"#
        )
    }

    fn fix_cache(x: &str, name: &str, axis: Axis, at: u32, delta: i64) -> Option<String> {
        let out = fixup_pivot_cache_xml(x.as_bytes(), name, axis, at, delta)?;
        Some(String::from_utf8(out.into_owned()).unwrap())
    }

    fn pivot_table_xml(loc: &str) -> String {
        format!(
            r#"<pivotTableDefinition xmlns="x" name="P1" cacheId="0"><location ref="{loc}" firstHeaderRow="1" firstDataRow="2" firstDataCol="1"/></pivotTableDefinition>"#
        )
    }

    #[test]
    fn pivot_cache_source_shifts_on_row_insert_inside() {
        // Row inserted at 3: A1:C5 -> A1:C6, and the moved source makes the
        // materialised cache stale -> refreshOnLoad set.
        let out =
            fix_cache(&cache_xml("A1:C5", "Data"), "Data", Axis::Row, 3, 1).expect("must fix");
        assert!(out.contains(r#"ref="A1:C6""#), "{out}");
        assert!(out.contains(r#"refreshOnLoad="1""#), "{out}");
    }

    #[test]
    fn pivot_cache_source_shifts_on_insert_above() {
        let out = fix_cache(&cache_xml("A1:C5", "Data"), "Data", Axis::Row, 1, 2).unwrap();
        assert!(out.contains(r#"ref="A3:C7""#), "{out}");
        assert!(out.contains(r#"refreshOnLoad="1""#), "{out}");
    }

    #[test]
    fn pivot_cache_source_other_sheet_untouched() {
        // The mutated sheet is "Other"; this cache sources "Data".
        let x = cache_xml("A1:C5", "Data");
        let out = fixup_pivot_cache_xml(x.as_bytes(), "Other", Axis::Row, 3, 1).unwrap();
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn pivot_cache_source_case_insensitive_sheet_match() {
        let out = fix_cache(&cache_xml("A1:C5", "data"), "DATA", Axis::Row, 3, 1).unwrap();
        assert!(out.contains(r#"ref="A1:C6""#), "{out}");
    }

    #[test]
    fn pivot_cache_source_below_band_untouched() {
        // Insert below the source range: the range neither moves nor changes,
        // so the cache stays byte-identical (no refreshOnLoad).
        let x = cache_xml("A1:C5", "Data");
        let out = fixup_pivot_cache_xml(x.as_bytes(), "Data", Axis::Row, 10, 3).unwrap();
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn pivot_cache_source_delete_inside_shrinks() {
        let out = fix_cache(&cache_xml("A1:C5", "Data"), "Data", Axis::Row, 2, -2).unwrap();
        assert!(out.contains(r#"ref="A1:C3""#), "{out}");
        assert!(out.contains(r#"refreshOnLoad="1""#), "{out}");
    }

    #[test]
    fn pivot_cache_source_whole_delete_becomes_ref() {
        let out = fix_cache(&cache_xml("A2:C5", "Data"), "Data", Axis::Row, 2, -4).unwrap();
        assert!(out.contains(r##"ref="#REF!""##), "{out}");
        assert!(out.contains(r#"refreshOnLoad="1""#), "{out}");
    }

    #[test]
    fn pivot_cache_source_shifts_on_column_axis() {
        let out = fix_cache(&cache_xml("A1:C5", "Data"), "Data", Axis::Col, 2, 1).unwrap();
        assert!(out.contains(r#"ref="A1:D5""#), "{out}");
        assert!(out.contains(r#"refreshOnLoad="1""#), "{out}");
    }

    #[test]
    fn pivot_cache_refresh_on_load_preserved_when_already_set() {
        let mut x = cache_xml("A1:C5", "Data");
        x = x.replacen(
            "<pivotCacheDefinition",
            r#"<pivotCacheDefinition refreshOnLoad="1""#,
            1,
        );
        let out = fixup_pivot_cache_xml(x.as_bytes(), "Data", Axis::Row, 3, 1).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(out.contains(r#"refreshOnLoad="1""#), "{out}");
        assert_eq!(out.matches("refreshOnLoad").count(), 1, "{out}");
    }

    #[test]
    fn pivot_cache_attr_zero_refresh_to_one() {
        let mut x = cache_xml("A1:C5", "Data");
        x = x.replacen(
            "<pivotCacheDefinition",
            r#"<pivotCacheDefinition refreshOnLoad="0""#,
            1,
        );
        let out = fixup_pivot_cache_xml(x.as_bytes(), "Data", Axis::Row, 3, 1).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(out.contains(r#"refreshOnLoad="1""#), "{out}");
    }

    #[test]
    fn pivot_table_location_shifts_on_own_sheet_row_op() {
        let x = pivot_table_xml("E3:G8");
        let out = fixup_pivot_table_xml(x.as_bytes(), Axis::Row, 1, 2).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(out.contains(r#"<location ref="E5:G10""#), "{out}");
    }

    #[test]
    fn pivot_table_location_column_axis() {
        let x = pivot_table_xml("E3:G8");
        let out = fixup_pivot_table_xml(x.as_bytes(), Axis::Col, 2, 1).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(out.contains(r#"<location ref="F3:H8""#), "{out}");
    }

    #[test]
    fn pivot_table_location_unaffected_borrows() {
        let x = pivot_table_xml("E3:G8");
        let out = fixup_pivot_table_xml(x.as_bytes(), Axis::Row, 10, 3).unwrap();
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn pivot_cache_source_ref_parses_rect() {
        let x = cache_xml("A1:C5", "Data");
        let (sheet, (r0, c0, r1, c1)) = pivot_cache_source_ref(x.as_bytes()).unwrap();
        assert_eq!(sheet, "Data");
        assert_eq!((r0, c0, r1, c1), (1, 1, 5, 3));
    }

    #[test]
    fn pivot_cache_source_ref_quoted_sheet_decodes() {
        let x = cache_xml("A1:C5", "My Sheet");
        let (sheet, _) = pivot_cache_source_ref(x.as_bytes()).unwrap();
        assert_eq!(sheet, "My Sheet");
    }

    #[test]
    fn pivot_cache_source_sheet_quoted_with_space_matches() {
        // Excel quotes sheet names with spaces: `sheet="'My Sheet'"`.
        let x = r#"<pivotCacheDefinition xmlns="x"><cacheSource type="worksheet"><worksheetSource sheet="'My Sheet'" ref="A1:C5"/></cacheSource></pivotCacheDefinition>"#;
        let out = fixup_pivot_cache_xml(x.as_bytes(), "My Sheet", Axis::Row, 3, 1).unwrap();
        let out = String::from_utf8(out.into_owned()).unwrap();
        assert!(out.contains(r#"ref="A1:C6""#), "{out}");
    }

    #[test]
    fn set_pivot_cache_refresh_on_load_is_idempotent() {
        let x = cache_xml("A1:C5", "Data");
        let once = set_pivot_cache_refresh_on_load(x.as_bytes()).unwrap();
        let twice = set_pivot_cache_refresh_on_load(&once);
        assert!(matches!(twice, None));
        assert!(String::from_utf8_lossy(&once).contains(r#"refreshOnLoad="1""#));
    }
}
