//! Sparse overlay table for edit_excel / load_workbook (Plan 01).

use ahash::{AHashMap, AHashSet};
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::turbo::error::{TurboError, TurboResult};
use crate::turbo::write::model::{
    CachedValue, Cell, CellValue, Row, Sheet, SstBuilder, Workbook, WriteOptions,
};
use crate::turbo::write::style_engine::{StyleDesc, StyleEngine};
use crate::turbo::write::writer::write_worksheet;
use crate::turbo::write::xml::{
    dimension_ref, needs_preserve, push, push_str, truncate_str, write_coord, write_escaped_text,
    write_f64, write_u32,
};
use crate::turbo::write::zip::{PrecompressedPart, ZipWriter};
use crate::turbo::zipmin::ArchiveMap;

#[derive(Debug, Clone, Default)]
pub struct SheetOverlay {
    pub modified_cells: AHashMap<(u32, u32), CellValue>,
    pub modified_styles: AHashMap<(u32, u32), StyleDesc>,
    pub is_dirty: bool,
}

pub struct WorkbookOverlay {
    pub archive_map: ArchiveMap,
    pub sheet_overlays: AHashMap<String, SheetOverlay>,
    pub new_sheets: Vec<Sheet>,
    pub deleted_sheets: AHashSet<String>,
}

impl WorkbookOverlay {
    pub fn new(archive_map: ArchiveMap) -> Self {
        Self {
            archive_map,
            sheet_overlays: AHashMap::default(),
            new_sheets: Vec::new(),
            deleted_sheets: AHashSet::default(),
        }
    }

    pub fn set_cell(&mut self, sheet_name: &str, row: u32, col: u32, val: CellValue) {
        let overlay = self
            .sheet_overlays
            .entry(sheet_name.to_string())
            .or_default();
        overlay.modified_cells.insert((row, col), val);
        overlay.is_dirty = true;
    }

    pub fn set_cell_style(&mut self, sheet_name: &str, row: u32, col: u32, desc: StyleDesc) {
        let overlay = self
            .sheet_overlays
            .entry(sheet_name.to_string())
            .or_default();
        overlay.modified_styles.insert((row, col), desc);
        overlay.is_dirty = true;
    }

    pub fn save(&self) -> TurboResult<Vec<u8>> {
        let mut zip = ZipWriter::new();

        // Build set of modified ZIP entry paths and resolve style descriptors
        let mut modified_entry_paths = AHashSet::default();
        let mut style_engine = StyleEngine::new();
        let mut sheet_resolved_styles: AHashMap<String, AHashMap<(u32, u32), u32>> =
            AHashMap::default();
        let mut styles_modified = false;

        for (sheet_name, overlay) in &self.sheet_overlays {
            if overlay.is_dirty {
                if let Some(target) = self.archive_map.sheet_name_map.get(sheet_name) {
                    modified_entry_paths.insert(target.clone());
                } else if sheet_name.starts_with("xl/") {
                    modified_entry_paths.insert(sheet_name.clone());
                } else {
                    modified_entry_paths.insert(format!("xl/worksheets/{sheet_name}.xml"));
                }
            }

            if !overlay.modified_styles.is_empty() {
                styles_modified = true;
                let mut resolved = AHashMap::default();
                for (&(r, c), desc) in &overlay.modified_styles {
                    let xf_idx = style_engine.resolve(desc);
                    resolved.insert((r, c), xf_idx);
                }
                sheet_resolved_styles.insert(sheet_name.clone(), resolved);
            }
        }

        if styles_modified {
            modified_entry_paths.insert("xl/styles.xml".to_string());
        }

        // Copy all untouched parts from ArchiveMap verbatim
        for entry_name in &self.archive_map.entry_order {
            if self.deleted_sheets.contains(entry_name) || modified_entry_paths.contains(entry_name)
            {
                continue;
            }

            if let Some(entry) = self.archive_map.entries.get(entry_name) {
                let start = entry.data_offset as usize;
                let end = start + (entry.compressed_size as usize);
                if end <= self.archive_map.source_bytes.len() {
                    let payload = self.archive_map.source_bytes[start..end].to_vec();
                    zip.add_precompressed(PrecompressedPart {
                        name: entry.name.clone(),
                        method: entry.compression_method, // CRITICAL: Preserves STORE (0) vs DEFLATE (8)
                        crc32: entry.crc32,
                        uncomp_size: entry.uncompressed_size,
                        data: payload,
                    });
                }
            }
        }

        // Render modified dirty sheets
        for (sheet_name, overlay) in &self.sheet_overlays {
            if !overlay.is_dirty {
                continue;
            }

            let entry_name = match self.archive_map.sheet_name_map.get(sheet_name) {
                Some(target) => target.clone(),
                None => {
                    if sheet_name.starts_with("xl/") {
                        sheet_name.clone()
                    } else {
                        format!("xl/worksheets/{sheet_name}.xml")
                    }
                }
            };

            // Preferred path: byte-preserving splice of the ORIGINAL sheet XML.
            // Only <sheetData> is rewritten; every other top-level child of
            // <worksheet> (cols, mergeCells, sheetViews/freeze panes, autoFilter,
            // hyperlinks, dataValidations, conditionalFormatting, sheetPr,
            // sheetFormatPr, pageMargins, pageSetup, tableParts, ...) and every
            // row attribute (ht/customHeight/spans/s) survives byte-for-byte.
            let original_xml = self.archive_map.entries.get(&entry_name).and_then(|entry| {
                let start = entry.data_offset as usize;
                let end = start + (entry.compressed_size as usize);
                if end > self.archive_map.source_bytes.len() {
                    return None;
                }
                crate::turbo::zipmin::inflate(
                    entry.compression_method,
                    &self.archive_map.source_bytes[start..end],
                    entry.uncompressed_size as usize,
                )
                .ok()
            });

            let resolved = sheet_resolved_styles.get(sheet_name);
            if let Some(xml) = original_xml {
                if let Some(spliced) = splice_sheet_xml(&xml, overlay, resolved) {
                    zip.add(&entry_name, &spliced);
                    continue;
                }
            }

            // Fallback (brand-new sheet / unparseable part): synthesize from scratch.
            let mut sheet = Sheet::new(sheet_name);
            sheet.name = sheet_name.clone();

            // Apply overlay cell modifications onto hydrated Sheet
            for (&(row, col), val) in &overlay.modified_cells {
                let r_idx = match sheet.rows.binary_search_by(|r| r.row.cmp(&row)) {
                    Ok(i) => i,
                    Err(i) => {
                        sheet.rows.insert(i, Row::new(row));
                        i
                    }
                };
                let r = &mut sheet.rows[r_idx];
                match r.cells.binary_search_by(|c| c.col.cmp(&col)) {
                    Ok(c_idx) => r.cells[c_idx].value = val.clone(),
                    Err(c_idx) => r.cells.insert(c_idx, Cell::new(col, val.clone())),
                }
            }

            let mut sst = SstBuilder::new();
            let xml = write_worksheet(&sheet, false, false, &mut sst);
            zip.add(&entry_name, &xml);
        }

        // Append-only splice of xl/styles.xml: insert new font/fill/border/xf
        // records at the end of each pool, bump count= attributes, and leave
        // every pre-existing record byte-identical. This ensures existing s=
        // indices on pass-through cells remain valid.
        if styles_modified {
            let original_styles = self
                .archive_map
                .entries
                .get("xl/styles.xml")
                .and_then(|entry| {
                    let start = entry.data_offset as usize;
                    let end = start + (entry.compressed_size as usize);
                    if end > self.archive_map.source_bytes.len() {
                        return None;
                    }
                    crate::turbo::zipmin::inflate(
                        entry.compression_method,
                        &self.archive_map.source_bytes[start..end],
                        entry.uncompressed_size as usize,
                    )
                    .ok()
                });

            if let Some(orig_xml) = original_styles {
                // Parse existing pool counts from the original XML.
                let orig_font_count = parse_pool_count(&orig_xml, b"fonts");
                let orig_fill_count = parse_pool_count(&orig_xml, b"fills");
                let orig_border_count = parse_pool_count(&orig_xml, b"borders");
                let orig_xf_count = parse_pool_count(&orig_xml, b"cellXfs");

                // StyleEngine bootstraps: 1 font, 2 fills, 1 border, 1 xf.
                // New records are those beyond the bootstrap baseline.
                let boot_fonts: u32 = 1;
                let boot_fills: u32 = 2;
                let boot_borders: u32 = 1;
                let boot_xfs: u32 = 1;

                // Offset the resolved style indices by delta between original
                // pool sizes and bootstrap sizes.
                let font_offset = orig_font_count.saturating_sub(boot_fonts);
                let fill_offset = orig_fill_count.saturating_sub(boot_fills);
                let border_offset = orig_border_count.saturating_sub(boot_borders);
                let xf_offset = orig_xf_count.saturating_sub(boot_xfs);

                // Rewrite resolved xf indices with offset applied.
                for resolved in sheet_resolved_styles.values_mut() {
                    for xf_idx in resolved.values_mut() {
                        *xf_idx += xf_offset;
                    }
                }

                // Emit new records (skip bootstrap items).
                let mut new_font_xml = Vec::new();
                for f in style_engine.fonts().iter().skip(boot_fonts as usize) {
                    f.emit(&mut new_font_xml);
                }
                let mut new_fill_xml = Vec::new();
                for f in style_engine.fills().iter().skip(boot_fills as usize) {
                    f.emit(&mut new_fill_xml);
                }
                let mut new_border_xml = Vec::new();
                for b in style_engine.borders().iter().skip(boot_borders as usize) {
                    b.emit(&mut new_border_xml);
                }
                let mut new_xf_xml = Vec::new();
                for st in style_engine.cell_xfs().iter().skip(boot_xfs as usize) {
                    emit_xf_with_offsets(
                        &mut new_xf_xml,
                        st,
                        font_offset,
                        fill_offset,
                        border_offset,
                    );
                }

                let new_font_count = style_engine.font_count() as u32 - boot_fonts;
                let new_fill_count = style_engine.fill_count() as u32 - boot_fills;
                let new_border_count = style_engine.border_count() as u32 - boot_borders;
                let new_xf_count = style_engine.cell_xf_count() as u32 - boot_xfs;

                let spliced = splice_styles_xml_pools(
                    &orig_xml,
                    &new_font_xml,
                    new_font_count,
                    orig_font_count,
                    &new_fill_xml,
                    new_fill_count,
                    orig_fill_count,
                    &new_border_xml,
                    new_border_count,
                    orig_border_count,
                    &new_xf_xml,
                    new_xf_count,
                    orig_xf_count,
                );
                zip.add("xl/styles.xml", &spliced);
            } else {
                // No existing styles.xml (shouldn't happen for valid xlsx).
                // Fallback: emit fresh styles.xml from engine.
                let styles_xml = style_engine.emit_styles_xml();
                zip.add("xl/styles.xml", &styles_xml);
            }
        }

        zip.finish().map_err(TurboError::Io)
    }
}

// ---------------------------------------------------------------------------
// Byte-preserving sheet XML splice
// ---------------------------------------------------------------------------

/// Rewrite only `<sheetData>` inside an existing worksheet part, passing every
/// other byte of the original XML through unchanged.
///
/// Returns `None` when the input does not look like a worksheet part (no
/// `<sheetData>`), so the caller can fall back to full re-serialization.
pub fn splice_sheet_xml(
    xml: &[u8],
    overlay: &SheetOverlay,
    resolved_styles: Option<&AHashMap<(u32, u32), u32>>,
) -> Option<Vec<u8>> {
    // Group modifications by row, columns ascending.
    static EMPTY_CELL: CellValue = CellValue::Empty;
    let mut by_row: BTreeMap<u32, BTreeMap<u32, &CellValue>> = BTreeMap::new();
    for (&(r, c), v) in &overlay.modified_cells {
        by_row.entry(r).or_default().insert(c, v);
    }
    for &(r, c) in overlay.modified_styles.keys() {
        by_row.entry(r).or_default().entry(c).or_insert(&EMPTY_CELL);
    }

    // Widen <dimension ref="..."/> first (offsets before <sheetData> only).
    let widened = widen_dimension(xml, &by_row);
    let xml: &[u8] = widened.as_deref().unwrap_or(xml);

    let sd_start = find_element(xml, b"sheetData", 0)?;
    let gt = sd_start + memchr::memchr(b'>', &xml[sd_start..])?;
    let self_closing = gt > sd_start && xml[gt - 1] == b'/';

    let mut out: Vec<u8> = Vec::with_capacity(xml.len() + 64 * by_row.len().max(1) + 64);

    if self_closing {
        out.extend_from_slice(&xml[..sd_start]);
        push(&mut out, b"<sheetData>");
        for (row, cells) in &by_row {
            emit_new_row(&mut out, *row, cells, resolved_styles);
        }
        push(&mut out, b"</sheetData>");
        out.extend_from_slice(&xml[gt + 1..]);
        return Some(out);
    }

    let body_start = gt + 1;
    let close = body_start + memchr::memmem::find(&xml[body_start..], b"</sheetData>")?;
    out.extend_from_slice(&xml[..body_start]);
    splice_sheet_data_body(
        &xml[body_start..close],
        by_row,
        &mut out,
        overlay,
        resolved_styles,
    );
    out.extend_from_slice(&xml[close..]);
    Some(out)
}

fn splice_sheet_data_body(
    body: &[u8],
    mut by_row: BTreeMap<u32, BTreeMap<u32, &CellValue>>,
    out: &mut Vec<u8>,
    overlay: &SheetOverlay,
    resolved_styles: Option<&AHashMap<(u32, u32), u32>>,
) {
    let mut pos = 0usize;
    let mut seq_row = 0u32;

    while let Some(row_start) = find_element(body, b"row", pos) {
        out.extend_from_slice(&body[pos..row_start]);

        let Some(gt_rel) = memchr::memchr(b'>', &body[row_start..]) else {
            pos = row_start;
            break;
        };
        let tag_end = row_start + gt_rel; // index of '>'
        let row_tag = &body[row_start..=tag_end];
        let row_idx = extract_xml_attr(row_tag, b"r")
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(seq_row + 1);
        seq_row = row_idx;

        // Any brand-new rows that sort before this one.
        flush_rows_before(out, &mut by_row, row_idx, resolved_styles);

        let mut my_cells = by_row.remove(&row_idx);
        let row_self_closing = tag_end > row_start && body[tag_end - 1] == b'/';

        if row_self_closing {
            match my_cells {
                Some(cells) if !cells.is_empty() => {
                    // `<row .../>` → `<row ...>cells</row>`, attributes preserved.
                    out.extend_from_slice(&body[row_start..tag_end - 1]);
                    push(out, b">");
                    for (col, val) in &cells {
                        let style_idx = resolved_styles.and_then(|m| m.get(&(row_idx, *col)));
                        let s_str = style_idx.map(|idx| idx.to_string());
                        emit_cell(out, row_idx, *col, val, s_str.as_deref());
                    }
                    push(out, b"</row>");
                }
                _ => out.extend_from_slice(&body[row_start..=tag_end]),
            }
            pos = tag_end + 1;
            continue;
        }

        // Row open tag verbatim (ht / customHeight / spans / s / hidden intact).
        out.extend_from_slice(&body[row_start..=tag_end]);

        let (cells_end, after_row) = match memchr::memmem::find(&body[tag_end + 1..], b"</row>") {
            Some(rc) => (tag_end + 1 + rc, tag_end + 1 + rc + 6),
            None => (body.len(), body.len()),
        };

        let hay = &body[..cells_end];
        let mut cpos = tag_end + 1;
        let mut seq_col = 0u32;

        while let Some(c_start) = find_element(hay, b"c", cpos) {
            out.extend_from_slice(&hay[cpos..c_start]);

            let Some(cg) = memchr::memchr(b'>', &hay[c_start..]) else {
                cpos = c_start;
                break;
            };
            let c_tag_end = c_start + cg;
            let c_tag = &hay[c_start..=c_tag_end];
            let c_self_closing = c_tag_end > c_start && hay[c_tag_end - 1] == b'/';
            let c_end = if c_self_closing {
                c_tag_end + 1
            } else {
                match memchr::memmem::find(&hay[c_tag_end + 1..], b"</c>") {
                    Some(ce) => c_tag_end + 1 + ce + 4,
                    None => c_tag_end + 1,
                }
            };

            let col = extract_xml_attr(c_tag, b"r")
                .and_then(|r| col_from_ref_bytes(r.as_bytes()))
                .map(|c| (c as u32) + 1)
                .unwrap_or(seq_col + 1);
            seq_col = col;

            let mut replaced = false;
            if let Some(cells) = my_cells.as_mut() {
                let lower: Vec<u32> = cells.range(..col).map(|(k, _)| *k).collect();
                for k in lower {
                    if let Some(v) = cells.remove(&k) {
                        let style_idx = resolved_styles.and_then(|m| m.get(&(row_idx, k)));
                        let s_str = style_idx.map(|idx| idx.to_string());
                        emit_cell(out, row_idx, k, v, s_str.as_deref());
                    }
                }
                if let Some(v) = cells.remove(&col) {
                    let is_value_modified = overlay.modified_cells.contains_key(&(row_idx, col));
                    if is_value_modified {
                        let style_idx = resolved_styles.and_then(|m| m.get(&(row_idx, col)));
                        let s_str = style_idx.map(|idx| idx.to_string());
                        let orig_s = extract_xml_attr(c_tag, b"s");
                        let s_attr = s_str.as_deref().or(orig_s.as_deref());
                        emit_cell(out, row_idx, col, v, s_attr);
                        replaced = true;
                    } else if let Some(&new_s) =
                        resolved_styles.and_then(|m| m.get(&(row_idx, col)))
                    {
                        emit_cell_with_new_style(out, &hay[c_start..c_end], c_tag, new_s);
                        replaced = true;
                    }
                }
            }
            if !replaced {
                out.extend_from_slice(&hay[c_start..c_end]);
            }
            cpos = c_end;
        }

        if let Some(cells) = my_cells {
            for (col, val) in &cells {
                let style_idx = resolved_styles.and_then(|m| m.get(&(row_idx, *col)));
                let s_str = style_idx.map(|idx| idx.to_string());
                emit_cell(out, row_idx, *col, val, s_str.as_deref());
            }
        }

        out.extend_from_slice(&body[cpos..after_row]);
        pos = after_row;
    }

    // Rows past the last existing one, then the original trailing bytes.
    let remaining: Vec<u32> = by_row.keys().copied().collect();
    for r in remaining {
        if let Some(cells) = by_row.remove(&r) {
            emit_new_row(out, r, &cells, resolved_styles);
        }
    }
    out.extend_from_slice(&body[pos..]);
}

fn flush_rows_before(
    out: &mut Vec<u8>,
    by_row: &mut BTreeMap<u32, BTreeMap<u32, &CellValue>>,
    before: u32,
    resolved_styles: Option<&AHashMap<(u32, u32), u32>>,
) {
    let keys: Vec<u32> = by_row.range(..before).map(|(k, _)| *k).collect();
    for k in keys {
        if let Some(cells) = by_row.remove(&k) {
            emit_new_row(out, k, &cells, resolved_styles);
        }
    }
}

fn emit_new_row(
    out: &mut Vec<u8>,
    row: u32,
    cells: &BTreeMap<u32, &CellValue>,
    resolved_styles: Option<&AHashMap<(u32, u32), u32>>,
) {
    push(out, br#"<row r=""#);
    write_u32(out, row);
    push(out, br#"">"#);
    for (col, val) in cells {
        let style_idx = resolved_styles.and_then(|m| m.get(&(row, *col)));
        let s_str = style_idx.map(|idx| idx.to_string());
        emit_cell(out, row, *col, val, s_str.as_deref());
    }
    push(out, b"</row>");
}

fn emit_cell_with_new_style(out: &mut Vec<u8>, full_c: &[u8], c_tag: &[u8], new_s: u32) {
    if let Some(orig_s) = extract_xml_attr(c_tag, b"s") {
        let old_pattern = format!(r#"s="{}"#, orig_s);
        let new_pattern = format!(r#"s="{}"#, new_s);
        let s_str = String::from_utf8_lossy(full_c);
        let updated = s_str.replace(&old_pattern, &new_pattern);
        push_str(out, &updated);
    } else {
        push_str(out, r#"<c s=""#);
        write_u32(out, new_s);
        push(out, b"\"");
        out.extend_from_slice(&full_c[2..]);
    }
}

/// Emit one `<c>` element. `s_attr` is the raw `s=` value taken from the cell
/// being replaced (kept so pass-through styles.xml indices stay valid).
fn emit_cell(out: &mut Vec<u8>, row: u32, col: u32, val: &CellValue, s_attr: Option<&str>) {
    push(out, br#"<c r=""#);
    write_coord(out, row, col);
    push(out, b"\"");
    if let Some(s) = s_attr {
        push(out, br#" s=""#);
        push_str(out, s);
        push(out, b"\"");
    }

    match val {
        CellValue::Empty => {
            push(out, b"/>");
        }
        CellValue::Number(n) | CellValue::DateSerial(n) => {
            push(out, b"><v>");
            write_f64(out, *n);
            push(out, b"</v></c>");
        }
        CellValue::Bool(b) => {
            push(out, br#" t="b"><v>"#);
            push(out, if *b { b"1" } else { b"0" });
            push(out, b"</v></c>");
        }
        CellValue::Error(e) => {
            push(out, br#" t="e"><v>"#);
            write_escaped_text(out, e);
            push(out, b"</v></c>");
        }
        CellValue::Str(s) => {
            // Inline strings: no sharedStrings.xml dependency (the original SST
            // part is passed through untouched, so it must not be extended).
            let t = truncate_str(s);
            push(out, br#" t="inlineStr"><is><t"#);
            if needs_preserve(t) {
                push(out, br#" xml:space="preserve""#);
            }
            push(out, b">");
            write_escaped_text(out, t);
            push(out, b"</t></is></c>");
        }
        CellValue::Rich(rt) => {
            push(out, br#" t="inlineStr">"#);
            rt.emit_is(out);
            push(out, b"</c>");
        }
        CellValue::Formula { text, cached, .. } => {
            push(out, b"><f>");
            let bodytxt = text.strip_prefix('=').unwrap_or(text.as_str());
            write_escaped_text(out, bodytxt);
            push(out, b"</f>");
            match cached {
                Some(CachedValue::Number(n)) => {
                    push(out, b"<v>");
                    write_f64(out, *n);
                    push(out, b"</v>");
                }
                Some(CachedValue::Bool(b)) => {
                    push(out, b"<v>");
                    push(out, if *b { b"1" } else { b"0" });
                    push(out, b"</v>");
                }
                Some(CachedValue::Error(e)) | Some(CachedValue::Str(e)) => {
                    push(out, b"<v>");
                    write_escaped_text(out, e);
                    push(out, b"</v>");
                }
                None => {}
            }
            push(out, b"</c>");
        }
    }
}

/// Find `<name` at an element boundary (next byte is space / `>` / `/`).
fn find_element(hay: &[u8], name: &[u8], from: usize) -> Option<usize> {
    if from >= hay.len() {
        return None;
    }
    let mut needle = Vec::with_capacity(name.len() + 1);
    needle.push(b'<');
    needle.extend_from_slice(name);
    let mut p = from;
    while let Some(off) = memchr::memmem::find(&hay[p..], &needle) {
        let s = p + off;
        match hay.get(s + needle.len()) {
            Some(&b)
                if b == b' '
                    || b == b'>'
                    || b == b'/'
                    || b == b'\t'
                    || b == b'\r'
                    || b == b'\n' =>
            {
                return Some(s);
            }
            Some(_) => p = s + needle.len(),
            None => return None,
        }
    }
    None
}

/// Expand `<dimension ref="..."/>` to cover newly written cells. Returns `None`
/// when nothing needs to change.
fn widen_dimension(
    xml: &[u8],
    by_row: &BTreeMap<u32, BTreeMap<u32, &CellValue>>,
) -> Option<Vec<u8>> {
    if by_row.is_empty() {
        return None;
    }
    let mut nr0 = u32::MAX;
    let mut nc0 = u32::MAX;
    let mut nr1 = 0u32;
    let mut nc1 = 0u32;
    for (r, cells) in by_row {
        for c in cells.keys() {
            nr0 = nr0.min(*r);
            nr1 = nr1.max(*r);
            nc0 = nc0.min(*c);
            nc1 = nc1.max(*c);
        }
    }
    if nr1 == 0 || nc1 == 0 {
        return None;
    }

    let dim_start = find_element(xml, b"dimension", 0)?;
    let gt = dim_start + memchr::memchr(b'>', &xml[dim_start..])?;
    let tag = &xml[dim_start..=gt];
    let ref_pos = memchr::memmem::find(tag, b"ref=\"")? + 5;
    let ref_len = memchr::memchr(b'"', &tag[ref_pos..])?;
    let ref_str = std::str::from_utf8(&tag[ref_pos..ref_pos + ref_len]).ok()?;

    let (mut r0, mut c0, mut r1, mut c1) = parse_dimension_ref(ref_str)?;
    let orig = (r0, c0, r1, c1);
    r0 = r0.min(nr0);
    c0 = c0.min(nc0);
    r1 = r1.max(nr1);
    c1 = c1.max(nc1);
    if (r0, c0, r1, c1) == orig {
        return None;
    }

    let new_ref = dimension_ref(r0, c0, r1, c1);
    let abs_val_start = dim_start + ref_pos;
    let abs_val_end = abs_val_start + ref_len;
    let mut out = Vec::with_capacity(xml.len() + new_ref.len());
    out.extend_from_slice(&xml[..abs_val_start]);
    out.extend_from_slice(new_ref.as_bytes());
    out.extend_from_slice(&xml[abs_val_end..]);
    Some(out)
}

fn parse_dimension_ref(s: &str) -> Option<(u32, u32, u32, u32)> {
    let mut it = s.split(':');
    let a = parse_a1_ref(it.next()?)?;
    match it.next() {
        Some(b) => {
            let b = parse_a1_ref(b)?;
            Some((a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1)))
        }
        None => Some((a.0, a.1, a.0, a.1)),
    }
}

/// `"B12"` → `(row, col)` 1-based. `$` is tolerated.
fn parse_a1_ref(s: &str) -> Option<(u32, u32)> {
    let bytes: Vec<u8> = s.bytes().filter(|&b| b != b'$').collect();
    let col = col_from_ref_bytes(&bytes)? as u32 + 1;
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        i += 1;
    }
    let row: u32 = std::str::from_utf8(&bytes[i..]).ok()?.parse().ok()?;
    if row == 0 { None } else { Some((row, col)) }
}

pub fn hydrate_sheet_from_xml(
    xml: &[u8],
    shared_strings: &crate::turbo::scan::StringArena,
) -> TurboResult<Sheet> {
    let mut sheet = Sheet::new("");
    let end = xml.len();
    let mut pos = 0;

    while pos < end {
        if let Some(ro) = memchr::memmem::find(&xml[pos..end], b"<row ") {
            let r_start = pos + ro;
            let Some(gt) = memchr::memchr(b'>', &xml[r_start..end]) else {
                break;
            };
            let row_tag = &xml[r_start..r_start + gt + 1];

            let row_idx = if let Some(r_str) = extract_xml_attr(row_tag, b"r") {
                r_str.parse::<u32>().unwrap_or(0)
            } else {
                (sheet.rows.len() as u32) + 1
            };

            let row_close = match memchr::memmem::find(&xml[r_start..end], b"</row>") {
                Some(rc) => r_start + rc + 6,
                None => r_start + gt + 1,
            };

            let mut row = Row::new(row_idx);
            let mut c_pos = r_start + gt + 1;

            while c_pos < row_close {
                if let Some(co) = memchr::memmem::find(&xml[c_pos..row_close], b"<c ") {
                    let c_start = c_pos + co;
                    let Some(c_gt) = memchr::memchr(b'>', &xml[c_start..row_close]) else {
                        break;
                    };
                    let c_tag = &xml[c_start..c_start + c_gt + 1];

                    let is_self_closing = c_tag.ends_with(b"/>");
                    let c_end = if is_self_closing {
                        c_start + c_gt + 1
                    } else {
                        match memchr::memmem::find(&xml[c_start..row_close], b"</c>") {
                            Some(ce) => c_start + ce + 4,
                            None => c_start + c_gt + 1,
                        }
                    };

                    c_pos = c_end;

                    let cell_ref = extract_xml_attr(c_tag, b"r");
                    let cell_t = extract_xml_attr(c_tag, b"t");
                    let cell_s = extract_xml_attr(c_tag, b"s").and_then(|s| s.parse::<u32>().ok());

                    let col_idx = if let Some(r_ref) = &cell_ref {
                        col_from_ref_bytes(r_ref.as_bytes())
                            .map(|c| (c + 1) as u32)
                            .unwrap_or((row.cells.len() as u32) + 1)
                    } else {
                        (row.cells.len() as u32) + 1
                    };

                    if is_self_closing {
                        let mut cell = Cell::new(col_idx, CellValue::Empty);
                        cell.style = cell_s;
                        row.cells.push(cell);
                        continue;
                    }

                    let cell_body = &xml[c_start..c_end];
                    let val = parse_cell_body(cell_body, cell_t.as_deref(), shared_strings);
                    let mut cell = Cell::new(col_idx, val);
                    cell.style = cell_s;
                    row.cells.push(cell);
                } else {
                    break;
                }
            }

            if !row.cells.is_empty() {
                sheet.rows.push(row);
            }
            pos = row_close;
        } else {
            break;
        }
    }

    Ok(sheet)
}

fn parse_cell_body(
    body: &[u8],
    t_attr: Option<&str>,
    shared_strings: &crate::turbo::scan::StringArena,
) -> CellValue {
    if let Some(fo) = memchr::memmem::find(body, b"<f>") {
        if let Some(fc) = memchr::memmem::find(body, b"</f>") {
            let f_text = String::from_utf8_lossy(&body[fo + 3..fc]).into_owned();
            let v_text = extract_tag_value(body, b"<v>", b"</v>");
            let cached = v_text
                .and_then(|v| v.parse::<f64>().ok())
                .map(crate::turbo::write::model::CachedValue::Number);
            return CellValue::Formula {
                text: f_text,
                kind: crate::turbo::write::model::FormulaKind::Normal,
                cached,
            };
        }
    }

    let v_text = extract_tag_value(body, b"<v>", b"</v>");

    match t_attr {
        Some("s") => {
            if let Some(v_str) = v_text {
                if let Ok(id) = v_str.parse::<u32>() {
                    if let Some(s_bytes) = shared_strings.try_resolve(id) {
                        return CellValue::Str(String::from_utf8_lossy(s_bytes).into_owned());
                    }
                }
            }
            CellValue::Empty
        }
        Some("inlineStr") => {
            if let Some(t_text) = extract_tag_value(body, b"<t>", b"</t>") {
                CellValue::Str(t_text)
            } else {
                CellValue::Empty
            }
        }
        Some("b") => {
            if let Some(v_str) = v_text {
                CellValue::Bool(v_str == "1" || v_str.eq_ignore_ascii_case("true"))
            } else {
                CellValue::Empty
            }
        }
        Some("e") => {
            if let Some(v_str) = v_text {
                CellValue::Error(v_str)
            } else {
                CellValue::Empty
            }
        }
        Some("str") => {
            if let Some(v_str) = v_text {
                CellValue::Str(v_str)
            } else {
                CellValue::Empty
            }
        }
        _ => {
            if let Some(v_str) = v_text {
                if let Ok(num) = v_str.parse::<f64>() {
                    CellValue::Number(num)
                } else {
                    CellValue::Str(v_str)
                }
            } else {
                CellValue::Empty
            }
        }
    }
}

fn extract_tag_value(xml: &[u8], open_tag: &[u8], close_tag: &[u8]) -> Option<String> {
    let o = memchr::memmem::find(xml, open_tag)?;
    let c = memchr::memmem::find(xml, close_tag)?;
    if c > o + open_tag.len() {
        Some(String::from_utf8_lossy(&xml[o + open_tag.len()..c]).into_owned())
    } else {
        None
    }
}

fn extract_xml_attr(tag: &[u8], attr: &[u8]) -> Option<String> {
    let mut search = Vec::with_capacity(attr.len() + 2);
    search.extend_from_slice(attr);
    search.extend_from_slice(b"=\"");
    let o = memchr::memmem::find(tag, &search)?;
    let val_start = o + search.len();
    let q = memchr::memchr(b'"', &tag[val_start..])?;
    let val_bytes = &tag[val_start..val_start + q];
    Some(String::from_utf8_lossy(val_bytes).into_owned())
}

fn col_from_ref_bytes(bytes: &[u8]) -> Option<usize> {
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    let mut idx = 0usize;
    for &b in &bytes[..i] {
        let val = (b.to_ascii_uppercase() - b'A' + 1) as usize;
        idx = idx * 26 + val;
    }
    if idx == 0 { None } else { Some(idx - 1) }
}

// ---------------------------------------------------------------------------
// Append-only styles.xml splice helpers
// ---------------------------------------------------------------------------

use crate::turbo::write::style_engine::StyleArray;

/// Parse `count="N"` from a pool element like `<fonts count="3">`.
fn parse_pool_count(xml: &[u8], pool_tag: &[u8]) -> u32 {
    // Find `<pool_tag` then extract count="..." attribute.
    if let Some(pos) = find_element(xml, pool_tag, 0) {
        if let Some(gt) = memchr::memchr(b'>', &xml[pos..]) {
            let tag = &xml[pos..pos + gt + 1];
            if let Some(count_str) = extract_xml_attr(tag, b"count") {
                return count_str.parse::<u32>().unwrap_or(0);
            }
        }
    }
    0
}

/// Emit an `<xf>` element with font/fill/border ids offset by the given deltas.
fn emit_xf_with_offsets(
    out: &mut Vec<u8>,
    st: &StyleArray,
    font_offset: u32,
    fill_offset: u32,
    border_offset: u32,
) {
    push_str(out, "<xf numFmtId=\"");
    write_u32(out, st.num_fmt_id as u32);
    push_str(out, "\" fontId=\"");
    write_u32(out, st.font_id as u32 + font_offset);
    push_str(out, "\" fillId=\"");
    write_u32(out, st.fill_id as u32 + fill_offset);
    push_str(out, "\" borderId=\"");
    write_u32(out, st.border_id as u32 + border_offset);
    push(out, b"\"");
    if st.alignment_id != 0 {
        push_str(out, " applyAlignment=\"1\"");
    }
    if st.protection_id != 0 {
        push_str(out, " applyProtection=\"1\"");
    }
    push_str(out, " />");
}

/// Splice new records into each pool of an existing styles.xml.
/// For each pool (fonts, fills, borders, cellXfs):
///   1. Find `</pool_tag>` closing tag
///   2. Insert new records before it
///   3. Update `count="N"` to `count="N+new_count"`
fn splice_styles_xml_pools(
    xml: &[u8],
    new_fonts: &[u8],
    new_font_count: u32,
    orig_font_count: u32,
    new_fills: &[u8],
    new_fill_count: u32,
    orig_fill_count: u32,
    new_borders: &[u8],
    new_border_count: u32,
    orig_border_count: u32,
    new_xfs: &[u8],
    new_xf_count: u32,
    orig_xf_count: u32,
) -> Vec<u8> {
    let mut result = xml.to_vec();

    // Process in reverse order so earlier splices don't invalidate later positions.
    let pools: &[(&[u8], &[u8], u32, u32)] = &[
        (b"cellXfs", new_xfs, new_xf_count, orig_xf_count),
        (b"borders", new_borders, new_border_count, orig_border_count),
        (b"fills", new_fills, new_fill_count, orig_fill_count),
        (b"fonts", new_fonts, new_font_count, orig_font_count),
    ];

    for &(tag, new_records, new_count, orig_count) in pools {
        if new_count == 0 {
            continue;
        }

        // Build closing tag: </fonts>, </fills>, etc.
        let mut close_tag = Vec::with_capacity(tag.len() + 3);
        close_tag.extend_from_slice(b"</");
        close_tag.extend_from_slice(tag);
        close_tag.push(b'>');

        if let Some(close_pos) = memchr::memmem::find(&result, &close_tag) {
            // Insert new records before closing tag.
            let mut updated = Vec::with_capacity(result.len() + new_records.len());
            updated.extend_from_slice(&result[..close_pos]);
            updated.extend_from_slice(new_records);
            updated.extend_from_slice(&result[close_pos..]);
            result = updated;
        }

        // Update count="N" → count="N+new_count"
        let new_total = orig_count + new_count;
        let old_count_str = format!("count=\"{}\"", orig_count);
        let new_count_str = format!("count=\"{}\"", new_total);

        // Find the count attr specifically within the opening tag of this pool.
        if let Some(tag_pos) = find_element(&result, tag, 0) {
            if let Some(gt) = memchr::memchr(b'>', &result[tag_pos..]) {
                let tag_range = &result[tag_pos..tag_pos + gt + 1];
                if let Some(count_offset) =
                    memchr::memmem::find(tag_range, old_count_str.as_bytes())
                {
                    let abs_start = tag_pos + count_offset;
                    let abs_end = abs_start + old_count_str.len();
                    let mut updated = Vec::with_capacity(
                        result.len() + new_count_str.len() - old_count_str.len(),
                    );
                    updated.extend_from_slice(&result[..abs_start]);
                    updated.extend_from_slice(new_count_str.as_bytes());
                    updated.extend_from_slice(&result[abs_end..]);
                    result = updated;
                }
            }
        }
    }

    result
}
