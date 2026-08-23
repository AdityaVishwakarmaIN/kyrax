//! Sparse overlay table for edit_excel / load_workbook (Plan 01).

use ahash::{AHashMap, AHashSet};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crate::turbo::error::{TurboError, TurboResult};
use crate::turbo::fixup::{
    fixup_pivot_cache_xml, fixup_pivot_table_xml, fixup_sheet_xml, fixup_table_part_xml,
    fixup_workbook_xml, pivot_cache_source_ref, set_pivot_cache_refresh_on_load,
};
use crate::turbo::mutate::{
    attr_value_span, delete_cols, delete_rows, insert_cols, insert_rows, move_range,
};
use crate::turbo::refshift::Axis;
use crate::turbo::structural::{
    RelKind, parse_rels, parse_workbook_pivot_caches, resolve_zip_path,
};
use crate::turbo::write::model::{
    CachedValue, Cell, CellValue, FormulaKind, Row, Sheet, SstBuilder,
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
    /// Row/column insert-delete operations recorded in user order. Applied at
    /// save time, each as mutate-splice then fixup, BEFORE cell edits.
    pub ops: Vec<SheetOp>,
    pub is_dirty: bool,
}

impl SheetOverlay {
    /// Insert `amount` blank rows at 1-based `at` (openpyxl semantics).
    pub fn insert_rows(&mut self, at: u32, amount: u32) {
        if amount == 0 {
            return;
        }
        self.ops.push(SheetOp::InsertRows { at, amount });
        self.is_dirty = true;
    }

    /// Delete `amount` rows starting at 1-based `at` (openpyxl semantics).
    pub fn delete_rows(&mut self, at: u32, amount: u32) {
        if amount == 0 {
            return;
        }
        self.ops.push(SheetOp::DeleteRows { at, amount });
        self.is_dirty = true;
    }

    /// Insert `amount` blank columns at 1-based `at` (openpyxl semantics).
    pub fn insert_cols(&mut self, at: u32, amount: u32) {
        if amount == 0 {
            return;
        }
        self.ops.push(SheetOp::InsertCols { at, amount });
        self.is_dirty = true;
    }

    /// Delete `amount` columns starting at 1-based `at` (openpyxl semantics).
    pub fn delete_cols(&mut self, at: u32, amount: u32) {
        if amount == 0 {
            return;
        }
        self.ops.push(SheetOp::DeleteCols { at, amount });
        self.is_dirty = true;
    }

    /// Relocate the rectangle `(r1,c1)..(r2,c2)` (1-based, inclusive) by `rows`,
    /// `cols` (signed). Destination cells are overwritten and vacated source
    /// cells become empty; nothing else on the sheet shifts. `translate` shifts
    /// formula bodies inside the moved range (openpyxl `move_range` semantics).
    #[allow(clippy::too_many_arguments)]
    pub fn move_range(
        &mut self,
        r1: u32,
        c1: u32,
        r2: u32,
        c2: u32,
        rows: i64,
        cols: i64,
        translate: bool,
    ) {
        if rows == 0 && cols == 0 {
            return;
        }
        self.ops.push(SheetOp::MoveRange {
            r1,
            c1,
            r2,
            c2,
            rows,
            cols,
            translate,
        });
        self.is_dirty = true;
    }
}

/// One recorded row/column operation on an editable sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SheetOp {
    InsertRows {
        at: u32,
        amount: u32,
    },
    DeleteRows {
        at: u32,
        amount: u32,
    },
    InsertCols {
        at: u32,
        amount: u32,
    },
    DeleteCols {
        at: u32,
        amount: u32,
    },
    MoveRange {
        r1: u32,
        c1: u32,
        r2: u32,
        c2: u32,
        rows: i64,
        cols: i64,
        translate: bool,
    },
}

impl SheetOp {
    /// The grid axis the operation shifts. Only grid mutations have a single
    /// axis; `MoveRange` moves on both axes and its single-axis helpers are
    /// never consulted (the save loop applies it directly).
    pub fn axis(&self) -> Axis {
        match self {
            SheetOp::InsertRows { .. } | SheetOp::DeleteRows { .. } => Axis::Row,
            SheetOp::InsertCols { .. } | SheetOp::DeleteCols { .. } => Axis::Col,
            SheetOp::MoveRange { .. } => Axis::Row,
        }
    }

    /// The 1-based first affected index. `MoveRange` returns 0; its single-axis
    /// helpers are never consulted.
    pub fn at(&self) -> u32 {
        match self {
            SheetOp::InsertRows { at, .. }
            | SheetOp::DeleteRows { at, .. }
            | SheetOp::InsertCols { at, .. }
            | SheetOp::DeleteCols { at, .. } => *at,
            SheetOp::MoveRange { .. } => 0,
        }
    }

    /// Signed shift for the mutate / fixup passes (positive inserts, negative deletes).
    pub fn delta(&self) -> i64 {
        match self {
            SheetOp::InsertRows { amount, .. } | SheetOp::InsertCols { amount, .. } => {
                *amount as i64
            }
            SheetOp::DeleteRows { amount, .. } | SheetOp::DeleteCols { amount, .. } => {
                -(*amount as i64)
            }
            SheetOp::MoveRange { .. } => 0,
        }
    }

    /// Human-readable description for refusal errors.
    pub fn human(&self) -> String {
        match self {
            SheetOp::InsertRows { at, amount } => {
                format!("insert {amount} row(s) at row {at}")
            }
            SheetOp::DeleteRows { at, amount } => {
                format!("delete {amount} row(s) starting at row {at}")
            }
            SheetOp::InsertCols { at, amount } => {
                format!("insert {amount} column(s) at column {at}")
            }
            SheetOp::DeleteCols { at, amount } => {
                format!("delete {amount} column(s) starting at column {at}")
            }
            SheetOp::MoveRange {
                r1,
                c1,
                r2,
                c2,
                rows,
                cols,
                ..
            } => {
                format!("move range {r1}:{c1}-{r2}:{c2} by rows={rows} cols={cols}")
            }
        }
    }
}

pub struct StructureRewriteResult {
    pub workbook_xml: Vec<u8>,
    pub wb_rels_xml: Option<Vec<u8>>,
    pub content_types_xml: Option<Vec<u8>>,
    pub new_parts: AHashMap<String, String>,
    pub deleted_entry_paths: AHashSet<String>,
}

fn esc_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

fn extract_xml_attr_str(tag: &str, attr_name: &str) -> Option<String> {    let needle = format!("{attr_name}=\"");
    if let Some(pos) = tag.find(&needle) {
        let val_start = pos + needle.len();
        if let Some(end_quote) = tag[val_start..].find('"') {
            return Some(tag[val_start..val_start + end_quote].to_string());
        }
    }
    let needle_sq = format!("{attr_name}='");
    if let Some(pos) = tag.find(&needle_sq) {
        let val_start = pos + needle_sq.len();
        if let Some(end_quote) = tag[val_start..].find('\'') {
            return Some(tag[val_start..val_start + end_quote].to_string());
        }
    }
    None
}

/// Copy `xml` verbatim except dropping `<tag ...>` elements for which `drop`
/// returns true. Non-element bytes (declaration, whitespace, containers) are
/// preserved byte-for-byte. Returns None when nothing was dropped.
fn filter_empty_elements<F: Fn(&str) -> bool>(xml: &str, tag: &str, drop: F) -> Option<String> {
    let open = format!("<{}", tag);
    let mut out = String::with_capacity(xml.len());
    let mut pos = 0usize;
    let mut dropped = false;
    while let Some(rel) = xml[pos..].find(&open) {
        let s = pos + rel;
        // Element boundary: next '>'. Attr values escape '>' as "&gt;".
        let e_rel = xml[s..].find('>')?;
        let e = s + e_rel + 1;
        if drop(&xml[s + open.len()..e - 1]) {
            dropped = true;
            out.push_str(&xml[pos..s]);
        } else {
            out.push_str(&xml[pos..e]);
        }
        pos = e;
    }
    if !dropped {
        return None;
    }
    out.push_str(&xml[pos..]);
    Some(out)
}

pub struct WorkbookOverlay {
    pub archive_map: ArchiveMap,
    pub sheet_overlays: AHashMap<String, SheetOverlay>,
    pub new_sheets: Vec<Sheet>,
    pub deleted_sheets: AHashSet<String>,
    pub renamed_sheets: Vec<(String, String)>,
    pub structure_dirty: bool,
    pub hydrated: AHashMap<String, Arc<Sheet>>,
}

impl WorkbookOverlay {
    pub fn new(archive_map: ArchiveMap) -> Self {
        Self {
            archive_map,
            sheet_overlays: AHashMap::default(),
            new_sheets: Vec::new(),
            deleted_sheets: AHashSet::default(),
            renamed_sheets: Vec::new(),
            structure_dirty: false,
            hydrated: AHashMap::default(),
        }
    }

    pub fn new_blank() -> TurboResult<Self> {
        let wb = crate::turbo::write::model::Workbook::new();
        let bytes = crate::turbo::write::writer::write_workbook_bytes(&wb)
            .map_err(|e| TurboError::Format(format!("failed to serialize blank workbook: {e}")))?;
        let archive_map = ArchiveMap::parse(Arc::new(bytes))?;
        Ok(Self::new(archive_map))
    }

    pub fn sheet_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        for n in &self.archive_map.sheet_names {
            if !self.deleted_sheets.contains(n) {
                names.push(n.clone());
            }
        }
        for s in &self.new_sheets {
            if !self.deleted_sheets.contains(&s.name) && !names.contains(&s.name) {
                names.push(s.name.clone());
            }
        }
        names
    }

    pub fn rename_sheet(&mut self, old_name: &str, new_name: &str) -> TurboResult<()> {
        if old_name == new_name {
            return Ok(());
        }
        self.structure_dirty = true;
        self.renamed_sheets.push((old_name.to_string(), new_name.to_string()));
        if let Some(target) = self.archive_map.sheet_name_map.remove(old_name) {
            self.archive_map.sheet_name_map.insert(new_name.to_string(), target);
        }
        for n in &mut self.archive_map.sheet_names {
            if n == old_name {
                *n = new_name.to_string();
            }
        }
        if let Some(so) = self.sheet_overlays.remove(old_name) {
            self.sheet_overlays.insert(new_name.to_string(), so);
        }
        for s in &mut self.new_sheets {
            if s.name == old_name {
                s.name = new_name.to_string();
            }
        }
        if let Some(h) = self.hydrated.remove(old_name) {
            self.hydrated.insert(new_name.to_string(), h);
        }
        Ok(())
    }

    pub fn create_sheet(&mut self, title: &str, index: Option<usize>) -> TurboResult<()> {
        self.structure_dirty = true;
        let mut sheet = Sheet::new(title);
        sheet.name = title.to_string();
        if let Some(idx) = index {
            let clamped = idx.min(self.new_sheets.len());
            self.new_sheets.insert(clamped, sheet);
        } else {
            self.new_sheets.push(sheet);
        }
        self.deleted_sheets.remove(title);
        Ok(())
    }

    pub fn delete_sheet(&mut self, name: &str) -> TurboResult<()> {
        self.structure_dirty = true;
        self.deleted_sheets.insert(name.to_string());
        self.new_sheets.retain(|s| s.name != name);
        self.hydrated.remove(name);
        Ok(())
    }

    pub fn copy_worksheet(&mut self, src_name: &str, target_title: &str) -> TurboResult<()> {
        let hydrated = self.hydrated_sheet(src_name)?;
        let mut new_sheet = match hydrated {
            Some(src) => (*src).clone(),
            None => Sheet::new(target_title),
        };
        new_sheet.name = target_title.to_string();
        self.create_sheet(target_title, None)?;

        if let Some(src_ov) = self.sheet_overlays.get(src_name).cloned() {
            let target_ov = self.sheet_overlays.entry(target_title.to_string()).or_default();
            target_ov.modified_cells = src_ov.modified_cells;
            target_ov.modified_styles = src_ov.modified_styles;
            target_ov.ops = src_ov.ops;
            target_ov.is_dirty = true;
        }
        self.hydrated.insert(target_title.to_string(), Arc::new(new_sheet));
        Ok(())
    }

    pub fn hydrated_sheet(&mut self, sheet_name: &str) -> TurboResult<Option<Arc<Sheet>>> {
        if let Some(s) = self.hydrated.get(sheet_name) {
            return Ok(Some(Arc::clone(s)));
        }
        if let Some(s) = self.new_sheets.iter().find(|s| s.name == sheet_name) {
            let arc = Arc::new(s.clone());
            self.hydrated.insert(sheet_name.to_string(), Arc::clone(&arc));
            return Ok(Some(arc));
        }
        let Some(target) = self.archive_map.sheet_name_map.get(sheet_name) else {
            return Ok(None);
        };
        let Some(xml) = inflate_entry(&self.archive_map, target)? else {
            return Ok(None);
        };
        let sheet = hydrate_sheet_from_xml(&xml, &self.archive_map.shared_strings)?;
        let arc = Arc::new(sheet);
        self.hydrated.insert(sheet_name.to_string(), Arc::clone(&arc));
        Ok(Some(arc))
    }

    pub fn rewrite_workbook_structure(&mut self) -> TurboResult<StructureRewriteResult> {
        let mut new_parts: AHashMap<String, String> = AHashMap::default();
        let mut deleted_entry_paths: AHashSet<String> = AHashSet::default();

        for deleted_name in &self.deleted_sheets {
            if let Some(target) = self.archive_map.sheet_name_map.get(deleted_name) {
                deleted_entry_paths.insert(target.clone());
                if let Some(stem) = target.strip_prefix("xl/worksheets/") {
                    deleted_entry_paths.insert(format!("xl/worksheets/_rels/{stem}.rels"));
                }
            }
        }

        let wb_raw = inflate_entry(&self.archive_map, "xl/workbook.xml")?
            .ok_or_else(|| TurboError::Format("xl/workbook.xml not found in archive".into()))?;
        let wb_str = String::from_utf8_lossy(&wb_raw);

        let mut existing_sheets: Vec<(String, u32, String)> = Vec::new();
        let mut max_sheet_id = 0u32;
        let mut max_r_id_num = 0u32;

        if let Some(sheets_start) = wb_str.find("<sheets") {
            if let Some(open_tag_end) = wb_str[sheets_start..].find('>') {
                let open_tag_end = sheets_start + open_tag_end;
                if let Some(sheets_close) = wb_str[open_tag_end..].find("</sheets>") {
                    let sheets_close = open_tag_end + sheets_close;
                    let inner = &wb_str[open_tag_end + 1..sheets_close];
                    for chunk in inner.split("<sheet") {
                        if chunk.trim().is_empty() {
                            continue;
                        }
                        let tag = if let Some(end) = chunk.find("/>").or_else(|| chunk.find('>')) {
                            &chunk[..end]
                        } else {
                            chunk
                        };
                        let name = extract_xml_attr_str(tag, "name");
                        let sheet_id_str = extract_xml_attr_str(tag, "sheetId");
                        let rid = extract_xml_attr_str(tag, "r:id").or_else(|| extract_xml_attr_str(tag, "id"));
                        if let (Some(n), Some(s_id), Some(r)) = (name, sheet_id_str, rid) {
                            let sid: u32 = s_id.parse().unwrap_or(1);
                            if sid > max_sheet_id {
                                max_sheet_id = sid;
                            }
                            if let Some(num_str) = r.strip_prefix("rId") {
                                if let Ok(num) = num_str.parse::<u32>() {
                                    if num > max_r_id_num {
                                        max_r_id_num = num;
                                    }
                                }
                            }
                            existing_sheets.push((n, sid, r));
                        }
                    }
                }
            }
        }

        // Fresh relationship ids must not collide with NON-sheet relationships
        // (theme/styles/sharedStrings often hold higher rIds than the sheets),
        // so take the maximum across the whole workbook rels part.
        if let Some(rels_raw) = inflate_entry(&self.archive_map, "xl/_rels/workbook.xml.rels")? {
            let rels_str = String::from_utf8_lossy(&rels_raw);
            let mut sp = 0usize;
            while let Some(rel) = rels_str[sp..].find("Id=\"rId") {
                let s = sp + rel + 7;
                let digits = rels_str[s..].find('"').unwrap_or(0);
                if digits > 0 {
                    if let Ok(n) = rels_str[s..s + digits].parse::<u32>() {
                        if n > max_r_id_num {
                            max_r_id_num = n;
                        }
                    }
                }
                sp = s;
            }
        }

        let final_names = self.sheet_names();
        let mut final_sheet_entries: Vec<(String, u32, String)> = Vec::new();
        let mut assigned_parts_count = 1u32;

        for name in &final_names {
            let existing = existing_sheets.iter().find(|(n, _, _)| {
                n == name || self.renamed_sheets.iter().any(|(old, new)| old == n && new == name)
            });

            if let Some((_, sid, rid)) = existing {
                final_sheet_entries.push((name.clone(), *sid, rid.clone()));
            } else {
                max_sheet_id += 1;
                max_r_id_num += 1;
                let sid = max_sheet_id;
                let rid = format!("rId{}", max_r_id_num);
                let part_path = loop {
                    let path = format!("xl/worksheets/sheet{assigned_parts_count}.xml");
                    assigned_parts_count += 1;
                    if !self.archive_map.entries.contains_key(&path) && !new_parts.values().any(|p| p == &path) {
                        break path;
                    }
                };
                new_parts.insert(name.clone(), part_path.clone());
                self.archive_map.sheet_name_map.insert(name.clone(), part_path);
                final_sheet_entries.push((name.clone(), sid, rid));
            }
        }

        let mut rewritten_wb = wb_str.to_string();
        if let Some(sheets_start) = rewritten_wb.find("<sheets") {
            if let Some(open_tag_end) = rewritten_wb[sheets_start..].find('>') {
                let open_tag_end = sheets_start + open_tag_end;
                if let Some(sheets_close) = rewritten_wb[open_tag_end..].find("</sheets>") {
                    let sheets_close = open_tag_end + sheets_close;
                    let mut inner = String::new();
                    for (n, sid, rid) in &final_sheet_entries {
                        inner.push_str(&format!(
                            "<sheet name=\"{}\" sheetId=\"{}\" r:id=\"{}\"/>",
                            esc_attr(n),
                            sid,
                            rid
                        ));
                    }
                    rewritten_wb.replace_range(open_tag_end + 1..sheets_close, &inner);
                }
            }
        }

        let wb_rels_xml = if let Some(rels_raw) = inflate_entry(&self.archive_map, "xl/_rels/workbook.xml.rels")? {
            let rels_str = String::from_utf8_lossy(&rels_raw);
            let deleted_rids: Vec<String> = existing_sheets
                .iter()
                .filter(|(n, _, _)| self.deleted_sheets.contains(n))
                .map(|(_, _, r)| r.clone())
                .collect();
            let mut additions = String::new();
            for (name, part_path) in &new_parts {
                if let Some((_, _, rid)) = final_sheet_entries.iter().find(|(n, _, _)| n == name) {
                    let rel_target = part_path.strip_prefix("xl/").unwrap_or(part_path);
                    additions.push_str(&format!(
                        "<Relationship Id=\"{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"{}\"/>",
                        rid,
                        rel_target
                    ));
                }
            }
            let filtered = filter_empty_elements(&rels_str, "Relationship", |attrs| {
                if let Some(t) = extract_xml_attr_str(attrs, "Target") {
                    let resolved = if t.starts_with("worksheets/") {
                        format!("xl/{t}")
                    } else if t.starts_with("/xl/") {
                        t[1..].to_string()
                    } else {
                        t.clone()
                    };
                    if deleted_entry_paths.contains(&resolved) {
                        return true;
                    }
                }
                if let Some(i) = extract_xml_attr_str(attrs, "Id") {
                    if deleted_rids.iter().any(|r| *r == i) {
                        return true;
                    }
                }
                false
            });
            if filtered.is_none() && additions.is_empty() {
                None
            } else {
                let mut rewritten_rels = filtered.unwrap_or_else(|| rels_str.to_string());
                if let Some(ins) = rewritten_rels.rfind("</Relationships>") {
                    rewritten_rels.insert_str(ins, &additions);
                }
                Some(rewritten_rels.into_bytes())
            }
        } else {
            None
        };

        let content_types_xml = if let Some(ct_raw) = inflate_entry(&self.archive_map, "[Content_Types].xml")? {
            let ct_str = String::from_utf8_lossy(&ct_raw);
            let mut additions = String::new();
            for part_path in new_parts.values() {
                additions.push_str(&format!(
                    "<Override PartName=\"/{}\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>",
                    part_path
                ));
            }
            let filtered = filter_empty_elements(&ct_str, "Override", |attrs| {
                if let Some(p) = extract_xml_attr_str(attrs, "PartName") {
                    let clean_p = p.strip_prefix('/').unwrap_or(&p);
                    return deleted_entry_paths.contains(clean_p);
                }
                false
            });
            if filtered.is_none() && additions.is_empty() {
                None
            } else {
                let mut rewritten_ct = filtered.unwrap_or_else(|| ct_str.to_string());
                if let Some(ins) = rewritten_ct.rfind("</Types>") {
                    rewritten_ct.insert_str(ins, &additions);
                }
                Some(rewritten_ct.into_bytes())
            }
        } else {
            None
        };

        Ok(StructureRewriteResult {
            workbook_xml: rewritten_wb.into_bytes(),
            wb_rels_xml,
            content_types_xml,
            new_parts,
            deleted_entry_paths,
        })
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

    /// Record an insert of `amount` blank rows at 1-based `at` on `sheet_name`.
    pub fn insert_rows(&mut self, sheet_name: &str, at: u32, amount: u32) {
        self.sheet_overlays
            .entry(sheet_name.to_string())
            .or_default()
            .insert_rows(at, amount);
    }

    /// Record a delete of `amount` rows starting at 1-based `at` on `sheet_name`.
    pub fn delete_rows(&mut self, sheet_name: &str, at: u32, amount: u32) {
        self.sheet_overlays
            .entry(sheet_name.to_string())
            .or_default()
            .delete_rows(at, amount);
    }

    /// Record an insert of `amount` blank columns at 1-based `at` on `sheet_name`.
    pub fn insert_cols(&mut self, sheet_name: &str, at: u32, amount: u32) {
        self.sheet_overlays
            .entry(sheet_name.to_string())
            .or_default()
            .insert_cols(at, amount);
    }

    /// Record a delete of `amount` columns starting at 1-based `at` on `sheet_name`.
    pub fn delete_cols(&mut self, sheet_name: &str, at: u32, amount: u32) {
        self.sheet_overlays
            .entry(sheet_name.to_string())
            .or_default()
            .delete_cols(at, amount);
    }

    /// Record a relocation of `(r1,c1)..(r2,c2)` (1-based, inclusive) by `rows`,
    /// `cols` (signed) on `sheet_name` (openpyxl `move_range` semantics).
    #[allow(clippy::too_many_arguments)]
    pub fn move_range(
        &mut self,
        sheet_name: &str,
        r1: u32,
        c1: u32,
        r2: u32,
        c2: u32,
        rows: i64,
        cols: i64,
        translate: bool,
    ) {
        self.sheet_overlays
            .entry(sheet_name.to_string())
            .or_default()
            .move_range(r1, c1, r2, c2, rows, cols, translate);
    }

    pub fn save(&mut self) -> TurboResult<Vec<u8>> {
        let mut rewritten_parts: AHashMap<String, Vec<u8>> = AHashMap::default();
        let mut deleted_entry_paths: AHashSet<String> = AHashSet::default();
        let mut assigned_new_parts: AHashMap<String, String> = AHashMap::default();

        if self.structure_dirty
            || !self.deleted_sheets.is_empty()
            || !self.new_sheets.is_empty()
            || !self.renamed_sheets.is_empty()
        {
            let res = self.rewrite_workbook_structure()?;
            rewritten_parts.insert("xl/workbook.xml".to_string(), res.workbook_xml);
            if let Some(wb_rels) = res.wb_rels_xml {
                rewritten_parts.insert("xl/_rels/workbook.xml.rels".to_string(), wb_rels);
            }
            if let Some(ct) = res.content_types_xml {
                rewritten_parts.insert("[Content_Types].xml".to_string(), ct);
            }
            deleted_entry_paths = res.deleted_entry_paths;
            assigned_new_parts = res.new_parts;
        }

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
                    let p = assigned_new_parts.get(sheet_name).cloned().unwrap_or_else(|| {
                        format!("xl/worksheets/{sheet_name}.xml")
                    });
                    modified_entry_paths.insert(p);
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

        let mut rendered: AHashMap<String, Vec<u8>> = AHashMap::default();
        let mut table_edit: AHashMap<String, Vec<u8>> = AHashMap::default();
        let mut pivot_table_edit: AHashMap<String, Vec<u8>> = AHashMap::default();
        let mut pivot_cache_edit: AHashMap<String, Vec<u8>> = AHashMap::default();
        let mut pivot_cache_map: Option<std::collections::HashMap<u32, String>> = None;
        let mut workbook_bytes: Option<Vec<u8>> = rewritten_parts.remove("xl/workbook.xml");
        let mut workbook_modified = workbook_bytes.is_some();

        let formula_affected = self
            .sheet_overlays
            .values()
            .any(|o| o.is_dirty && (!o.modified_cells.is_empty() || !o.ops.is_empty()));

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
                        assigned_new_parts.get(sheet_name).cloned().unwrap_or_else(|| {
                            format!("xl/worksheets/{sheet_name}.xml")
                        })
                    }
                }
            };

            let original_xml = inflate_entry(&self.archive_map, &entry_name)?;

            let pivot_parts =
                sheet_pivot_parts(&self.archive_map, &entry_name, &mut pivot_cache_map)?;

            let mut sheet_xml: Option<Vec<u8>> = original_xml;
            if !overlay.ops.is_empty() {
                let Some(mut buf) = sheet_xml.take() else {
                    return Err(TurboError::Refused(format!(
                        "cannot {} in sheet '{}': the sheet has no source XML to splice",
                        overlay.ops[0].human(),
                        sheet_name
                    )));
                };
                let table_parts = sheet_table_parts(&self.archive_map, &entry_name, &buf);

                for op in &overlay.ops {
                    let mutated =
                        apply_mutate(&buf, op).ok_or_else(|| refuse_sheet_op(sheet_name, op))?;
                    buf = mutated.into_owned();

                    if matches!(op, SheetOp::MoveRange { .. }) {
                        continue;
                    }

                    for tp in &table_parts {
                        let cur: Cow<'_, [u8]> = match table_edit.get(tp) {
                            Some(prev) => Cow::Borrowed(prev.as_slice()),
                            None => match inflate_entry(&self.archive_map, tp)? {
                                Some(tx) => Cow::Owned(tx),
                                None => continue,
                            },
                        };
                        match fixup_table_part_xml(cur.as_ref(), op.axis(), op.at(), op.delta()) {
                            Some(Cow::Owned(o)) => {
                                table_edit.insert(tp.clone(), o);
                            }
                            Some(Cow::Borrowed(_)) => {}
                            None => return Err(refuse_table_op(sheet_name, tp, op)),
                        }
                    }

                    for (pt_part, cache_part) in &pivot_parts {
                        let cur: Cow<'_, [u8]> = match pivot_table_edit.get(pt_part) {
                            Some(prev) => Cow::Borrowed(prev.as_slice()),
                            None => match inflate_entry(&self.archive_map, pt_part)? {
                                Some(px) => Cow::Owned(px),
                                None => continue,
                            },
                        };
                        if let Some(Cow::Owned(o)) =
                            fixup_pivot_table_xml(cur.as_ref(), op.axis(), op.at(), op.delta())
                        {
                            pivot_table_edit.insert(pt_part.clone(), o);
                        }
                        if let Some(cp) = cache_part {
                            let curc: Cow<'_, [u8]> = match pivot_cache_edit.get(cp) {
                                Some(prev) => Cow::Borrowed(prev.as_slice()),
                                None => match inflate_entry(&self.archive_map, cp)? {
                                    Some(cx) => Cow::Owned(cx),
                                    None => continue,
                                },
                            };
                            if let Some(Cow::Owned(o)) = fixup_pivot_cache_xml(
                                curc.as_ref(),
                                sheet_name,
                                op.axis(),
                                op.at(),
                                op.delta(),
                            ) {
                                pivot_cache_edit.insert(cp.clone(), o);
                            }
                        }
                    }

                    if workbook_bytes.is_none() {
                        workbook_bytes = inflate_entry(&self.archive_map, "xl/workbook.xml")?;
                    }
                    if let Some(wx) = workbook_bytes.as_mut() {
                        if let Cow::Owned(o) =
                            fixup_workbook_xml(wx, sheet_name, op.axis(), op.at(), op.delta())
                        {
                            *wx = o;
                            workbook_modified = true;
                        }
                    }

                    let fixed = fixup_sheet_xml(&buf, op.axis(), op.at(), op.delta())
                        .ok_or_else(|| refuse_sheet_op(sheet_name, op))?;
                    buf = fixed.into_owned();
                }
                sheet_xml = Some(buf);
            }

            // Pivot-cache staleness (edit / move_range gaps): a cell edit or a
            // moved range that touches a cache's source range makes its
            // materialised records stale, so the cache is tagged refreshOnLoad
            // and Excel rebuilds it on open.
            if !overlay.modified_cells.is_empty()
                || overlay
                    .ops
                    .iter()
                    .any(|o| matches!(o, SheetOp::MoveRange { .. }))
            {
                apply_pivot_staleness(
                    &self.archive_map,
                    sheet_name,
                    &pivot_parts,
                    &overlay.modified_cells,
                    &overlay.ops,
                    &mut pivot_cache_edit,
                )?;
            }

            // Cell edits are applied AFTER shifts (final coordinates).
            let resolved = sheet_resolved_styles.get(sheet_name);
            if let Some(xml) = sheet_xml {
                if let Some(spliced) = splice_sheet_xml(&xml, overlay, resolved) {
                    rendered.insert(entry_name.clone(), spliced);
                    continue;
                }
            }

            // Fallback (brand-new sheet / unparseable part): synthesize from scratch.
            let mut sheet = Sheet::new(sheet_name);
            sheet.name = sheet_name.clone();

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
                    Ok(c_idx) => {
                        r.cells[c_idx].value = val.clone();
                        if let Some(desc) = overlay.modified_styles.get(&(row, col)) {
                            r.cells[c_idx].style_desc = Some(Box::new(desc.clone()));
                        }
                    }
                    Err(c_idx) => {
                        let mut cell = Cell::new(col, val.clone());
                        if let Some(desc) = overlay.modified_styles.get(&(row, col)) {
                            cell.style_desc = Some(Box::new(desc.clone()));
                        }
                        r.cells.insert(c_idx, cell);
                    }
                }
            }

            let mut sst = SstBuilder::new();
            let xml = write_worksheet(&sheet, false, false, &mut sst);
            rendered.insert(entry_name, xml);
        }

        for new_sheet in &self.new_sheets {
            let part_path = assigned_new_parts.get(&new_sheet.name).cloned().unwrap_or_else(|| {
                format!("xl/worksheets/{}.xml", new_sheet.name)
            });
            if !rendered.contains_key(&part_path) {
                let mut sst = SstBuilder::new();
                let xml = write_worksheet(new_sheet, false, false, &mut sst);
                rendered.insert(part_path, xml);
            }
        }

        if formula_affected {
            if workbook_bytes.is_none() {
                workbook_bytes = inflate_entry(&self.archive_map, "xl/workbook.xml")?;
            }
            if let Some(wx) = workbook_bytes.as_mut() {
                if let Cow::Owned(o) = ensure_full_calc_on_load(wx)? {
                    *wx = o;
                    workbook_modified = true;
                }
            }

            // Strip cached <v> from already-rendered dirty sheets too.
            let rendered_names: Vec<String> = rendered.keys().cloned().collect();
            for name in rendered_names {
                if let Some(stripped) = strip_formula_cached_values(&rendered[&name]) {
                    rendered.insert(name, stripped);
                }
            }
            for (name, entry_name) in &self.archive_map.sheet_name_map {
                let _ = name;
                if rendered.contains_key(entry_name) {
                    continue;
                }
                if let Some(orig) = inflate_entry(&self.archive_map, entry_name)? {
                    if let Some(stripped) = strip_formula_cached_values(&orig) {
                        rendered.insert(entry_name.clone(), stripped);
                    }
                }
            }
        }

        let content_changed = !rendered.is_empty()
            || !table_edit.is_empty()
            || !pivot_table_edit.is_empty()
            || !pivot_cache_edit.is_empty()
            || workbook_modified
            || styles_modified
            || !self.deleted_sheets.is_empty()
            || !self.new_sheets.is_empty()
            || !rewritten_parts.is_empty();
        let signature_removal = if content_changed {
            strip_signature_metadata(&self.archive_map)?
        } else {
            None
        };

        let mut zip = ZipWriter::new();

        for entry_name in &self.archive_map.entry_order {
            if deleted_entry_paths.contains(entry_name)
                || rewritten_parts.contains_key(entry_name)
                || modified_entry_paths.contains(entry_name)
                || rendered.contains_key(entry_name)
                || table_edit.contains_key(entry_name)
                || pivot_table_edit.contains_key(entry_name)
                || pivot_cache_edit.contains_key(entry_name)
                || (entry_name == "xl/workbook.xml" && workbook_modified)
                || matches!(
                    &signature_removal,
                    Some(sr) if sr.drop.iter().any(|d| d == entry_name)
                )
                || (signature_removal.is_some() && entry_name == "_rels/.rels")
                || (signature_removal.is_some() && entry_name == "[Content_Types].xml")
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
                        method: entry.compression_method,
                        crc32: entry.crc32,
                        uncomp_size: entry.uncompressed_size,
                        data: payload,
                    });
                }
            }
        }

        for (name, bytes) in &rendered {
            zip.add(name, bytes);
        }
        for (name, bytes) in &table_edit {
            zip.add(name, bytes);
        }
        for (name, bytes) in &pivot_table_edit {
            zip.add(name, bytes);
        }
        for (name, bytes) in &pivot_cache_edit {
            zip.add(name, bytes);
        }
        for (name, bytes) in &rewritten_parts {
            zip.add(name, bytes);
        }
        if workbook_modified {
            if let Some(wb) = &workbook_bytes {
                zip.add("xl/workbook.xml", wb);
            }
        }

        if let Some(sr) = &signature_removal {
            zip.add("_rels/.rels", &sr.rels);
            zip.add("[Content_Types].xml", &sr.content_types);
        }

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
// Row/column mutation save-path helpers
// ---------------------------------------------------------------------------

/// Inflate one ZIP part by entry path. `Ok(None)` when the part is absent or
/// cannot be inflated (the callers decide whether that is an error).
fn inflate_entry(map: &ArchiveMap, name: &str) -> TurboResult<Option<Vec<u8>>> {
    let Some(entry) = map.entries.get(name) else {
        return Ok(None);
    };
    let start = entry.data_offset as usize;
    let end = start + (entry.compressed_size as usize);
    if end > map.source_bytes.len() {
        return Ok(None);
    }
    crate::turbo::zipmin::inflate(
        entry.compression_method,
        &map.source_bytes[start..end],
        entry.uncompressed_size as usize,
    )
    .map(Some)
}

/// Everything needed to invalidate a stale digital signature: the ZIP members
/// to drop and the byte-preserving rewrites of the two package-level metadata
/// parts that referenced them.
struct SignatureRemoval {
    drop: Vec<String>,
    rels: Vec<u8>,
    content_types: Vec<u8>,
}

/// When the workbook carries a digital signature, build the drop list and the
/// rewrites of `_rels/.rels` and `[Content_Types].xml` that invalidate it.
/// `Ok(None)` when the archive is unsigned (the common case costs one entry-name
/// listing, no inflate). A signed archive that is missing or has malformed
/// metadata it would have to rewrite refuses with `Format` â€” this is called
/// before any ZIP emission, so a bad signed package can never produce a partial
/// archive.
fn strip_signature_metadata(map: &ArchiveMap) -> TurboResult<Option<SignatureRemoval>> {
    let drop = crate::turbo::features::signatures::signature_part_names(&map.source_bytes)?;
    if drop.is_empty() {
        return Ok(None);
    }
    let rels_xml = inflate_entry(map, "_rels/.rels")?
        .ok_or_else(|| TurboError::Format("signed workbook is missing _rels/.rels".into()))?;
    let rels = crate::turbo::features::signatures::strip_signature_rels(&rels_xml)?;
    let ct_xml = inflate_entry(map, "[Content_Types].xml")?.ok_or_else(|| {
        TurboError::Format("signed workbook is missing [Content_Types].xml".into())
    })?;
    let content_types = crate::turbo::features::signatures::strip_signature_content_types(&ct_xml)?;
    Ok(Some(SignatureRemoval {
        drop,
        rels,
        content_types,
    }))
}

/// Ensure `xl/workbook.xml` declares `calcPr fullCalcOnLoad="1"` so Excel
/// recomputes every formula on load instead of presenting a stale cached `<v>`
/// as current (the splice never recomputes formula results). Returns
/// `Cow::Borrowed` when the property is already `"1"` (byte-identical no-op, so
/// openpyxl-authored sources â€” which default to `"1"` â€” keep every member
/// byte-identical), and a spliced owned buffer otherwise. A malformed workbook
/// refuses with `Format` before any output is emitted.
fn ensure_full_calc_on_load(xml: &[u8]) -> TurboResult<Cow<'_, [u8]>> {
    const ATTR: &[u8] = b"fullCalcOnLoad";
    // Locate the <calcPr open tag.
    let Some(o) = memchr::memmem::find(xml, b"<calcPr") else {
        return Ok(Cow::Owned(insert_calc_pr(xml)?));
    };
    let Some(gt_rel) = memchr::memchr(b'>', &xml[o..]) else {
        return Err(TurboError::Format(
            "unterminated <calcPr> tag in xl/workbook.xml".into(),
        ));
    };
    let gt = o + gt_rel;
    let tag = &xml[o..gt];
    match crate::turbo::structural::find_attr(tag, ATTR) {
        Some(b"1") => Ok(Cow::Borrowed(xml)),
        Some(_) => {
            // Replace the existing value in place with "1" (never a duplicate
            // attribute; preserves every unrelated byte).
            let (vs, ve) = attr_value_span(tag, ATTR).ok_or_else(|| {
                TurboError::Format("malformed fullCalcOnLoad in xl/workbook.xml".into())
            })?;
            let mut out = Vec::with_capacity(xml.len() + 1);
            out.extend_from_slice(&xml[..o + vs]);
            out.extend_from_slice(b"1");
            out.extend_from_slice(&xml[o + ve..]);
            Ok(Cow::Owned(out))
        }
        None => {
            // Insert the attribute before the tag's closing '>' (handles both
            // `<calcPr ...>` and self-closing `<calcPr .../>`).
            let mut out = Vec::with_capacity(xml.len() + b" fullCalcOnLoad=\"1\"".len());
            out.extend_from_slice(&xml[..gt]);
            out.extend_from_slice(b" fullCalcOnLoad=\"1\"");
            out.extend_from_slice(&xml[gt..]);
            Ok(Cow::Owned(out))
        }
    }
}

/// Insert a fresh `<calcPr fullCalcOnLoad="1"/>` into workbook.xml at a
/// schema-legal position: right after `</definedNames>`, else before the first
/// of `<oleSize`, `<customWorkbookViews`, `<pivotCaches`, `<smartTagPr`,
/// `<extLst`, else immediately before `</workbook>`. Returns `Format` when the
/// workbook has no `</workbook>` (malformed).
fn insert_calc_pr(xml: &[u8]) -> TurboResult<Vec<u8>> {
    const TAG: &[u8] = b"<calcPr fullCalcOnLoad=\"1\"/>";
    const AFTER: &[u8] = b"</definedNames>";
    let at = if let Some(p) = memchr::memmem::find(xml, AFTER) {
        p + AFTER.len()
    } else {
        let mut at = None;
        for n in [
            b"<oleSize".as_slice(),
            b"<customWorkbookViews".as_slice(),
            b"<pivotCaches".as_slice(),
            b"<smartTagPr".as_slice(),
            b"<extLst".as_slice(),
        ] {
            if let Some(p) = memchr::memmem::find(xml, n) {
                at = Some(p);
                break;
            }
        }
        match at {
            Some(p) => p,
            None => memchr::memmem::find(xml, b"</workbook>").ok_or_else(|| {
                TurboError::Format("xl/workbook.xml is missing </workbook>".into())
            })?,
        }
    };
    let mut out = Vec::with_capacity(xml.len() + TAG.len());
    out.extend_from_slice(&xml[..at]);
    out.extend_from_slice(TAG);
    out.extend_from_slice(&xml[at..]);
    Ok(out)
}

/// Strip every cached `<v>` result from formula cells in a worksheet part.
///
/// A formula cell is a `<c>` whose content contains an `<f>` element; its cached
/// result (a `<v>â€¦</v>` element, or a self-closing `<v />`) is a materialised
/// value that becomes stale the moment the grid shifts. This helper excises only
/// those `<v>` elements and preserves every other byte exactly â€” formula text,
/// attributes, and non-formula cells are untouched.
///
/// Returns `None` when nothing changed (no cached value found, or no formulas),
/// so callers can leave the part byte-identical.
fn strip_formula_cached_values(xml: &[u8]) -> Option<Vec<u8>> {
    let mut removes: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    while i < xml.len() {
        // A formula cell is `<c` â€¦ `</c>` whose body contains `<f`.
        let Some(o) = memchr::memmem::find(&xml[i..], b"<c") else {
            break;
        };
        let cs = i + o;
        // Only a real cell element: `<c>` / `<c r=â€¦>` / `<c â€¦>`. A following `o`
        // means `<cols>`/`<col>` â€” skip the tag.
        match xml.get(cs + 2) {
            Some(b'>') | Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n') | Some(b'/') => {}
            _ => {
                i = cs + 2;
                continue;
            }
        }
        let Some(gt_rel) = memchr::memchr(b'>', &xml[cs..]) else {
            break;
        };
        let gt = cs + gt_rel;
        let self_close = gt > cs && xml[gt - 1] == b'/';
        if self_close {
            i = gt + 1;
            continue;
        }
        let Some(cl_rel) = memchr::memmem::find(&xml[gt + 1..], b"</c>") else {
            break;
        };
        let ce = gt + 1 + cl_rel;
        let body = &xml[gt + 1..ce];
        if memchr::memmem::find(body, b"<f").is_none() {
            i = ce;
            continue;
        }
        // This cell holds a formula: remove every `<v â€¦>â€¦</v>` and `<v â€¦/>`.
        let mut j = 0usize;
        while j < body.len() {
            let Some(vo) = memchr::memmem::find(&body[j..], b"<v") else {
                break;
            };
            let vs = j + vo;
            let Some(vgt_rel) = memchr::memchr(b'>', &body[vs..]) else {
                break;
            };
            let vgt = vs + vgt_rel;
            let v_self = vgt > vs && body[vgt - 1] == b'/';
            let ve = if v_self {
                vgt + 1
            } else {
                let Some(vcl_rel) = memchr::memmem::find(&body[vgt + 1..], b"</v>") else {
                    break;
                };
                vgt + 1 + vcl_rel + b"</v>".len()
            };
            removes.push((gt + 1 + vs, gt + 1 + ve));
            j = ve;
        }
        i = ce;
    }
    if removes.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(xml.len());
    let mut prev = 0usize;
    for (s, e) in removes {
        out.extend_from_slice(&xml[prev..s]);
        prev = e;
    }
    out.extend_from_slice(&xml[prev..]);
    Some(out)
}

/// Resolve the ZIP entry paths of every table part owned by `sheet_entry`,
/// following the sheet's `tableParts` rids through its rels part.
fn sheet_table_parts(map: &ArchiveMap, sheet_entry: &str, sheet_xml: &[u8]) -> Vec<String> {
    let tail = match memchr::memmem::find(sheet_xml, b"</sheetData>") {
        Some(p) => &sheet_xml[p + b"</sheetData>".len()..],
        None => sheet_xml,
    };
    let rids = crate::turbo::structural::scan_table_part_rids(tail);
    if rids.is_empty() {
        return Vec::new();
    }
    let base = sheet_entry.rsplit('/').next().unwrap_or("sheet1.xml");
    let rels_path = format!("xl/worksheets/_rels/{base}.rels");
    let rels = match inflate_entry(map, &rels_path) {
        Ok(Some(rx)) => crate::turbo::structural::parse_rels(&rx),
        _ => Default::default(),
    };
    let mut out = Vec::with_capacity(rids.len());
    for rid in &rids {
        if let Some(rel) = rels.get(rid) {
            out.push(crate::turbo::structural::resolve_zip_path(
                "xl/worksheets/",
                &rel.target,
            ));
        }
    }
    out
}

/// Resolve every pivot table part owned by `sheet_entry` (via the sheet's rels)
/// and, for each, the cache definition part it references (its `cacheId` â†’
/// workbook `<pivotCaches>` â†’ workbook rels â†’ part path). The workbook cache
/// map is built once and cached. `Ok(())` with an empty vec when the sheet has
/// no pivots or the workbook has no resolvable caches.
fn sheet_pivot_parts(
    map: &ArchiveMap,
    sheet_entry: &str,
    wb_cache_map: &mut Option<std::collections::HashMap<u32, String>>,
) -> TurboResult<Vec<(String, Option<String>)>> {
    let base = sheet_entry.rsplit('/').next().unwrap_or("sheet1.xml");
    let rels_path = format!("xl/worksheets/_rels/{base}.rels");
    let rels = match inflate_entry(map, &rels_path)? {
        Some(rx) => parse_rels(&rx),
        None => Default::default(),
    };
    let tables: Vec<String> = rels
        .values()
        .filter(|r| r.kind == RelKind::PivotTable)
        .map(|r| resolve_zip_path("xl/worksheets/", &r.target))
        .collect();
    if tables.is_empty() {
        return Ok(Vec::new());
    }
    if wb_cache_map.is_none() {
        *wb_cache_map = workbook_pivot_caches(map)?;
    }
    let mut out = Vec::with_capacity(tables.len());
    for pt in tables {
        let cache = match inflate_entry(map, &pt)? {
            Some(px) => peek_pivot_cache_id(&px)
                .and_then(|cid| wb_cache_map.as_ref().and_then(|m| m.get(&cid)).cloned()),
            None => None,
        };
        out.push((pt, cache));
    }
    Ok(out)
}

/// Workbook-level `cacheId â†’ cache definition part path`, from workbook.xml's
/// `<pivotCaches>` + the workbook rels. `Ok(None)` when the workbook has no
/// pivot caches (or no workbook to read).
fn workbook_pivot_caches(
    map: &ArchiveMap,
) -> TurboResult<Option<std::collections::HashMap<u32, String>>> {
    let wb_xml = match inflate_entry(map, "xl/workbook.xml")? {
        Some(x) => x,
        None => return Ok(None),
    };
    let wb_rels = match inflate_entry(map, "xl/_rels/workbook.xml.rels")? {
        Some(rx) => parse_rels(&rx),
        None => Default::default(),
    };
    Ok(Some(parse_workbook_pivot_caches(&wb_xml, &wb_rels)))
}

/// The `cacheId` a pivot table part declares.
fn peek_pivot_cache_id(xml: &[u8]) -> Option<u32> {
    let start = memchr::memmem::find(xml, b"cacheId=\"")?;
    let vs = start + 9;
    let ve = vs + memchr::memchr(b'"', &xml[vs..])?;
    std::str::from_utf8(&xml[vs..ve]).ok()?.parse().ok()
}

/// Tag every pivot cache that sources from `sheet_name` and whose source range
/// a cell edit or a moved range touches, so Excel refreshes the materialised
/// records on open. Insert/delete staleness is handled inside
/// `fixup_pivot_cache_xml`; this closes the edit + move_range gaps.
fn apply_pivot_staleness(
    map: &ArchiveMap,
    sheet_name: &str,
    pivot_parts: &[(String, Option<String>)],
    edited_cells: &AHashMap<(u32, u32), CellValue>,
    ops: &[SheetOp],
    pivot_cache_edit: &mut AHashMap<String, Vec<u8>>,
) -> TurboResult<()> {
    for (_, cache_part) in pivot_parts {
        let Some(cp) = cache_part else {
            continue;
        };
        let cur: Cow<'_, [u8]> = match pivot_cache_edit.get(cp) {
            Some(prev) => Cow::Borrowed(prev.as_slice()),
            None => match inflate_entry(map, cp)? {
                Some(cx) => Cow::Owned(cx),
                None => continue,
            },
        };
        let Some((src_sheet, (r0, c0, r1, c1))) = pivot_cache_source_ref(cur.as_ref()) else {
            continue;
        };
        if !src_sheet.eq_ignore_ascii_case(sheet_name) {
            continue;
        }
        // A cell edit inside the source range.
        let mut stale = edited_cells
            .keys()
            .any(|&(r, c)| r >= r0 && r <= r1 && c >= c0 && c <= c1);
        // A moved range whose source or destination intersects the source range.
        if !stale {
            for op in ops {
                if let SheetOp::MoveRange {
                    r1: a,
                    c1: b,
                    r2: d,
                    c2: e,
                    rows,
                    cols,
                    ..
                } = op
                {
                    let src = (*a.min(d), *b.min(e), *a.max(d), *b.max(e));
                    let dr1 = *a as i64 + *rows;
                    let dc1 = *b as i64 + *cols;
                    let dr2 = *d as i64 + *rows;
                    let dc2 = *e as i64 + *cols;
                    let dst = (
                        dr1.min(dr2) as u32,
                        dc1.min(dc2) as u32,
                        dr1.max(dr2) as u32,
                        dc1.max(dc2) as u32,
                    );
                    if rects_intersect((r0, c0, r1, c1), src)
                        || rects_intersect((r0, c0, r1, c1), dst)
                    {
                        stale = true;
                        break;
                    }
                }
            }
        }
        if stale {
            if let Some(nb) = set_pivot_cache_refresh_on_load(cur.as_ref()) {
                pivot_cache_edit.insert(cp.clone(), nb);
            }
        }
    }
    Ok(())
}

/// Do two 1-based inclusive rectangles overlap?
fn rects_intersect(a: (u32, u32, u32, u32), b: (u32, u32, u32, u32)) -> bool {
    a.0 <= b.2 && b.0 <= a.2 && a.1 <= b.3 && b.1 <= a.3
}

/// Apply the mutate splice for one recorded op. `None` means the splice
/// refused (would corrupt the sheet rather than shift it).
fn apply_mutate<'a>(xml: &'a [u8], op: &SheetOp) -> Option<Cow<'a, [u8]>> {
    match op {
        SheetOp::InsertRows { at, amount } => insert_rows(xml, *at, *amount),
        SheetOp::DeleteRows { at, amount } => delete_rows(xml, *at, *amount),
        SheetOp::InsertCols { at, amount } => insert_cols(xml, *at, *amount),
        SheetOp::DeleteCols { at, amount } => delete_cols(xml, *at, *amount),
        SheetOp::MoveRange {
            r1,
            c1,
            r2,
            c2,
            rows,
            cols,
            translate,
        } => move_range(xml, *r1, *c1, *r2, *c2, *rows, *cols, *translate),
    }
}

/// Refusal reason for the sheet splice / sheet fixup pass. `mutate.rs` refuses
/// without saying which constraint tripped, so the message names the class of
/// causes; the operation-specific situations are documented on the Python API.
fn refuse_sheet_op(sheet_name: &str, op: &SheetOp) -> TurboError {
    match op {
        SheetOp::MoveRange { .. } => TurboError::Refused(format!(
            "cannot {} in sheet '{}': refused because it would corrupt the worksheet â€” \
             a destination corner would leave the grid (rows 1..=1,048,576, columns 1..=16,384), \
             an implicit-numbered row/cell lies inside the moved region, or a shared-formula ref= \
             would leave the grid",
            op.human(),
            sheet_name
        )),
        _ => TurboError::Refused(format!(
            "cannot {} in sheet '{}': refused because it would corrupt the worksheet â€” an \
             implicit-numbered row/cell at or below the shift point, a grid limit (1,048,576 rows \
             / 16,384 columns) would be exceeded, or a shared-formula master would be orphaned",
            op.human(),
            sheet_name
        )),
    }
}

/// Refusal reason for a table-part fixup (header-row delete / emptied table).
fn refuse_table_op(sheet_name: &str, table_part: &str, op: &SheetOp) -> TurboError {
    TurboError::Refused(format!(
        "cannot {} in sheet '{}': refused because it would corrupt table part '{}' â€” it would \
         delete the table's header row or delete every column of the table",
        op.human(),
        sheet_name,
        table_part
    ))
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
                    // `<row .../>` â†’ `<row ...>cells</row>`, attributes preserved.
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
        CellValue::Number(n) | CellValue::DateSerial(n) | CellValue::Time(n) | CellValue::Duration(n) => {
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
        CellValue::Formula { text, kind, cached } => {
            // Declare the cached result's type before closing the cell tag, or
            // Excel reads a non-numeric `<v>` as a number. A numeric cache uses
            // the implicit default and emits no `t=`.
            match cached {
                Some(CachedValue::Bool(_)) => push(out, br#" t="b""#),
                Some(CachedValue::Error(_)) => push(out, br#" t="e""#),
                Some(CachedValue::Str(_)) => push(out, br#" t="str""#),
                Some(CachedValue::Number(_)) | None => {}
            }
            let bodytxt = text.strip_prefix('=').unwrap_or(text.as_str());
            match kind {
                FormulaKind::Normal => {
                    push(out, b"><f>");
                    write_escaped_text(out, bodytxt);
                    push(out, b"</f>");
                }
                FormulaKind::Array { ref_ } => {
                    push(out, br#"><f t="array" ref=""#);
                    write_escaped_text(out, ref_);
                    push(out, b"\">");
                    write_escaped_text(out, bodytxt);
                    push(out, b"</f>");
                }
                FormulaKind::DataTable {
                    ref_,
                    dt2d,
                    dtr,
                    r1,
                    r2,
                    del1,
                    del2,
                    ca,
                } => {
                    push(out, br#"><f t="dataTable" ref=""#);
                    write_escaped_text(out, ref_);
                    push(out, b"\"");
                    if *dt2d {
                        push(out, br#" dt2D="1""#);
                    }
                    if *dtr {
                        push(out, br#" dtr="1""#);
                    }
                    if let Some(r) = r1 {
                        push(out, br#" r1=""#);
                        write_escaped_text(out, r);
                        push(out, b"\"");
                    }
                    if let Some(r) = r2 {
                        push(out, br#" r2=""#);
                        write_escaped_text(out, r);
                        push(out, b"\"");
                    }
                    if *del1 {
                        push(out, br#" del1="1""#);
                    }
                    if *del2 {
                        push(out, br#" del2="1""#);
                    }
                    if *ca {
                        push(out, br#" ca="1""#);
                    }
                    push(out, b"/>");
                }
            }
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
pub(crate) fn find_element(hay: &[u8], name: &[u8], from: usize) -> Option<usize> {
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

/// `"B12"` â†’ `(row, col)` 1-based. `$` is tolerated.
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
    // Shared-formula groups (`<f t="shared" si=...>`): the anchor carries the
    // text, every dependent carries only `si`. Anchors are keyed by `si`; the
    // dependents are translated from their anchor by their row/col delta after
    // the main loop, matching the streaming reader (scan.rs) and openpyxl.
    let mut shared_anchors: HashMap<u32, (u32, u32, String)> = HashMap::new();
    let mut shared_deps: Vec<(u32, u32, u32)> = Vec::new();

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
                    let f_info = scan_f(cell_body);
                    let val = parse_cell_body(
                        cell_body,
                        cell_t.as_deref(),
                        shared_strings,
                        f_info.as_ref(),
                    );
                    let mut cell = Cell::new(col_idx, val);
                    cell.style = cell_s;
                    row.cells.push(cell);

                    if let Some(f) = &f_info {
                        if f.t.as_deref() == Some("shared") {
                            if let Some(si) = f.si {
                                if f.text_start < f.text_end {
                                    let mut scratch = Vec::new();
                                    let decoded = crate::turbo::decode::decode_bytes(
                                        &cell_body[f.text_start..f.text_end],
                                        &mut scratch,
                                    );
                                    let text = String::from_utf8_lossy(decoded).into_owned();
                                    shared_anchors.insert(si, (row_idx, col_idx, text));
                                } else {
                                    shared_deps.push((row_idx, col_idx, si));
                                }
                            }
                        }
                    }
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

    // Translate every shared-formula dependent from its anchor's text by the
    // (row, col) delta. An orphaned `si` (no anchor in this sheet) degrades to
    // Empty, matching the streaming reader.
    for (row, col, si) in shared_deps {
        if let Some((arow, acol, text)) = shared_anchors.get(&si) {
            let (dr, dc) = (row as i32 - *arow as i32, col as i32 - *acol as i32);
            // Mirror the anchor's leading-`=` convention: `<f>` text is usually
            // stored without `=`, but a writer may include it.
            let translated = if let Some(rest) = text.strip_prefix('=') {
                format!("={}", crate::turbo::formula::translate_body(rest, dr, dc))
            } else {
                crate::turbo::formula::translate_body(text, dr, dc)
            };
            if let Some(cell) = find_cell_mut(&mut sheet, row, col) {
                if let CellValue::Formula { text, .. } = &mut cell.value {
                    *text = translated;
                }
            }
        } else if let Some(cell) = find_cell_mut(&mut sheet, row, col) {
            cell.value = CellValue::Empty;
        }
    }

    Ok(sheet)
}

fn find_cell_mut(sheet: &mut Sheet, row: u32, col: u32) -> Option<&mut Cell> {
    let r = sheet.rows.iter_mut().find(|r| r.row == row)?;
    r.cells.iter_mut().find(|c| c.col == col)
}

/// Parsed attributes of a cell's `<f>` element. `None` (the `scan_f` return
/// value) means the cell has no formula element at all.
struct FInfo {
    /// Raw `t` attribute value (`array` / `shared` / `dataTable`), `None` for a
    /// plain `<f>`.
    t: Option<String>,
    ref_: Option<String>,
    si: Option<u32>,
    dt2d: bool,
    dtr: bool,
    r1: Option<String>,
    r2: Option<String>,
    del1: bool,
    del2: bool,
    ca: bool,
    /// Formula body text span within the cell body (`start == end` when the
    /// `<f>` element is self-closing, e.g. a shared dependent or dataTable).
    text_start: usize,
    text_end: usize,
}

/// Scan a cell body for its `<f>` element (plain, attributed, or self-closing).
/// Any byte after `<f` other than a valid element boundary is skipped, so `<f`
/// never matches inside another element's text (cell text is entity-escaped).
fn scan_f(body: &[u8]) -> Option<FInfo> {
    let mut pos = 0usize;
    while let Some(off) = memchr::memmem::find(&body[pos..], b"<f") {
        let s = pos + off;
        match body.get(s + 2) {
            Some(&b'>') | Some(&b'/') | Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n') => {
                let gt = memchr::memchr(b'>', &body[s..])? + s;
                let tag = &body[s..=gt];
                let is_self_closing = gt > s && body[gt - 1] == b'/';
                let mut info = FInfo {
                    t: None,
                    ref_: None,
                    si: None,
                    dt2d: false,
                    dtr: false,
                    r1: None,
                    r2: None,
                    del1: false,
                    del2: false,
                    ca: false,
                    text_start: gt + 1,
                    text_end: gt + 1,
                };
                if let Some(t) = extract_xml_attr(tag, b"t") {
                    info.t = Some(t);
                }
                if let Some(r) = extract_xml_attr(tag, b"ref") {
                    info.ref_ = Some(r);
                }
                if let Some(v) = extract_xml_attr(tag, b"si").and_then(|v| v.parse::<u32>().ok()) {
                    info.si = Some(v);
                }
                info.dt2d = extract_xml_attr(tag, b"dt2D").as_deref() == Some("1");
                info.dtr = extract_xml_attr(tag, b"dtr").as_deref() == Some("1");
                if let Some(r) = extract_xml_attr(tag, b"r1") {
                    info.r1 = Some(r);
                }
                if let Some(r) = extract_xml_attr(tag, b"r2") {
                    info.r2 = Some(r);
                }
                info.del1 = extract_xml_attr(tag, b"del1").as_deref() == Some("1");
                info.del2 = extract_xml_attr(tag, b"del2").as_deref() == Some("1");
                info.ca = extract_xml_attr(tag, b"ca").as_deref() == Some("1");
                if !is_self_closing {
                    if let Some(ce) = memchr::memmem::find(&body[gt + 1..], b"</f>") {
                        info.text_end = gt + 1 + ce;
                    }
                }
                return Some(info);
            }
            _ => pos = s + 2,
        }
    }
    None
}

fn parse_cell_body(
    body: &[u8],
    t_attr: Option<&str>,
    shared_strings: &crate::turbo::scan::StringArena,
    f: Option<&FInfo>,
) -> CellValue {
    if let Some(f) = f {
        // Decode XML entities: the `<f>` text is stored escaped (`A1&lt;5`),
        // and `emit_cell` re-escapes via `write_escaped_text`, so the model must
        // hold the unescaped formula or a `&`/`<`/`>` in a formula double-escapes.
        let mut scratch = Vec::new();
        let decoded =
            crate::turbo::decode::decode_bytes(&body[f.text_start..f.text_end], &mut scratch);
        let f_text = String::from_utf8_lossy(decoded).into_owned();
        let kind = match f.t.as_deref() {
            Some("array") => FormulaKind::Array {
                ref_: f.ref_.clone().unwrap_or_else(|| "A1".into()),
            },
            Some("dataTable") => FormulaKind::DataTable {
                ref_: f.ref_.clone().unwrap_or_else(|| "A1".into()),
                dt2d: f.dt2d,
                dtr: f.dtr,
                r1: f.r1.clone(),
                r2: f.r2.clone(),
                del1: f.del1,
                del2: f.del2,
                ca: f.ca,
            },
            _ => FormulaKind::Normal,
        };
        // A formula cell's `t` attribute declares the cached result's type
        // (`b`/`e`/`str`), exactly mirroring the writer's `formula_result_type`.
        // Without it, a boolean cache would read as a number and an error/str
        // cache would be dropped entirely.
        //
        // `t="e"`/`t="str"` caches hold text and must be XML-decoded (`A&amp;B`
        // â†’ `A&B`) so a later emit's `write_escaped_text` re-escapes to the
        // original bytes. `t="s"` is a shared-string index (some producers
        // write formula results that way), resolved through the SST like the
        // non-formula branch below.
        let cached = extract_tag_value(body, b"<v>", b"</v>").and_then(|v| {
            let mut scratch = Vec::new();
            match t_attr {
                Some("b") => Some(CachedValue::Bool(
                    v == "1" || v.eq_ignore_ascii_case("true"),
                )),
                Some("e") => {
                    let decoded = crate::turbo::decode::decode_bytes(v.as_bytes(), &mut scratch);
                    Some(CachedValue::Error(
                        String::from_utf8_lossy(decoded).into_owned(),
                    ))
                }
                Some("str") => {
                    let decoded = crate::turbo::decode::decode_bytes(v.as_bytes(), &mut scratch);
                    Some(CachedValue::Str(
                        String::from_utf8_lossy(decoded).into_owned(),
                    ))
                }
                Some("s") => v
                    .parse::<u32>()
                    .ok()
                    .and_then(|id| shared_strings.try_resolve(id))
                    .map(|s| CachedValue::Str(String::from_utf8_lossy(s).into_owned())),
                _ => v.parse::<f64>().map(CachedValue::Number).ok().or_else(|| {
                    let decoded = crate::turbo::decode::decode_bytes(v.as_bytes(), &mut scratch);
                    Some(CachedValue::Str(
                        String::from_utf8_lossy(decoded).into_owned(),
                    ))
                }),
            }
        });
        return CellValue::Formula {
            text: f_text,
            kind,
            cached,
        };
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

pub(crate) fn extract_xml_attr(tag: &[u8], attr: &[u8]) -> Option<String> {
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
#[allow(clippy::too_many_arguments)]
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

        // Update count="N" â†’ count="N+new_count"
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

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    // Test-only: the fixtures build a Workbook and wrap bytes in an Arc; the
    // non-test path takes both from its caller.
    use crate::turbo::write::model::StringMode;
    use crate::turbo::write::model::Workbook;
    use crate::turbo::write::writer::write_workbook_bytes;
    use std::sync::Arc;

    /// A single-sheet workbook with `A1="a"`, `A2="b"` (inline strings).
    fn source_bytes() -> Vec<u8> {
        let mut wb = Workbook::new();
        wb.options.string_mode = StringMode::InlineStr;
        let sh = &mut wb.sheets[0];
        sh.rows
            .push(Row::new(1).with_cell(1, CellValue::Str("a".into())));
        sh.rows
            .push(Row::new(2).with_cell(1, CellValue::Str("b".into())));
        write_workbook_bytes(&wb).unwrap()
    }

    /// Hydrate the `"Sheet"` sheet from a saved workbook and return the string
    /// value at (row, col) or `None` when the cell is absent/empty.
    fn cell_str(saved: &[u8], row: u32, col: u32) -> Option<String> {
        let map = ArchiveMap::parse(Arc::new(saved.to_vec())).unwrap();
        let path = map.sheet_name_map.get("Sheet").cloned()?;
        let xml = inflate_entry(&map, &path).ok()??;
        let sheet = hydrate_sheet_from_xml(&xml, &map.shared_strings).ok()?;
        let r = sheet.rows.iter().find(|r| r.row == row)?;
        let c = r.cells.iter().find(|c| c.col == col)?;
        match &c.value {
            CellValue::Str(s) => Some(s.clone()),
            _ => None,
        }
    }

    /// The op order in the save path must be shift-then-edit: a recorded
    /// `insert_rows` moves the grid, and a cell edit's coordinate is final.
    ///
    /// Start: A1="a", A2="b". Record insert at 1, then edit (2,1)="edited".
    /// Correct (shift first): A1=blank, A2="edited", A3="b".
    /// Wrong (edit first):   A1=blank, A2="a",     A3="edited".
    #[test]
    fn save_applies_shifts_before_cell_edits() {
        let map = ArchiveMap::parse(Arc::new(source_bytes())).unwrap();
        let mut ov = WorkbookOverlay::new(map);
        ov.insert_rows("Sheet", 1, 1);
        ov.set_cell("Sheet", 2, 1, CellValue::Str("edited".into()));
        let saved = ov.save().expect("save must succeed");

        assert_eq!(
            cell_str(&saved, 1, 1),
            None,
            "row 1 must be the new blank row"
        );
        assert_eq!(
            cell_str(&saved, 2, 1).as_deref(),
            Some("edited"),
            "the edit must land on the shifted grid (row 2), not be pushed down"
        );
        assert_eq!(
            cell_str(&saved, 3, 1).as_deref(),
            Some("b"),
            "the original row-2 value must shift down to row 3"
        );
    }

    // -----------------------------------------------------------------------
    // Editable-model formula import fidelity (Fix 2): kinds, typed caches,
    // shared-formula translation, `_xlfn` pass-through.
    // -----------------------------------------------------------------------

    fn hydrate(xml: &str) -> Sheet {
        hydrate_sheet_from_xml(
            xml.as_bytes(),
            &crate::turbo::scan::parse_shared_strings(b""),
        )
        .unwrap()
    }

    fn hydrate_sst(xml: &str, sst: &str) -> Sheet {
        hydrate_sheet_from_xml(
            xml.as_bytes(),
            &crate::turbo::scan::parse_shared_strings(sst.as_bytes()),
        )
        .unwrap()
    }

    fn cell_at(sheet: &Sheet, row: u32, col: u32) -> Option<&Cell> {
        let r = sheet.rows.iter().find(|r| r.row == row)?;
        r.cells.iter().find(|c| c.col == col)
    }

    #[test]
    fn hydrate_plain_formula_keeps_text_and_number_cache() {
        let sheet = hydrate(r#"<row r="1"><c r="A1"><f>SUM(A1:A2)</f><v>3</v></c></row>"#);
        match cell_at(&sheet, 1, 1).map(|c| &c.value) {
            Some(CellValue::Formula { text, kind, cached }) => {
                assert_eq!(text, "SUM(A1:A2)");
                assert!(matches!(kind, FormulaKind::Normal));
                assert_eq!(cached, &Some(CachedValue::Number(3.0)));
            }
            other => panic!("expected a plain formula cell, got {other:?}"),
        }
    }

    #[test]
    fn hydrate_preserves_bool_error_and_str_caches() {
        let sheet = hydrate(
            r#"<row r="1"><c r="A1" t="b"><f>TRUE()</f><v>1</v></c>
                <c r="B1" t="e"><f>1/0</f><v>#DIV/0!</v></c>
                <c r="C1" t="str"><f>"x"</f><v>x</v></c></row>"#,
        );
        let a = cell_at(&sheet, 1, 1).unwrap();
        let b = cell_at(&sheet, 1, 2).unwrap();
        let c = cell_at(&sheet, 1, 3).unwrap();
        let CellValue::Formula { cached, .. } = &a.value else {
            panic!("A1 not a formula: {:?}", a.value);
        };
        assert_eq!(cached, &Some(CachedValue::Bool(true)));
        let CellValue::Formula { cached, .. } = &b.value else {
            panic!("B1 not a formula: {:?}", b.value);
        };
        assert_eq!(cached, &Some(CachedValue::Error("#DIV/0!".into())));
        let CellValue::Formula { cached, .. } = &c.value else {
            panic!("C1 not a formula: {:?}", c.value);
        };
        assert_eq!(cached, &Some(CachedValue::Str("x".into())));
    }

    #[test]
    fn hydrate_preserves_array_and_data_table_kinds() {
        let sheet = hydrate(
            r#"<row r="1"><c r="A1"><f t="array" ref="A1:B2">SUM(A1:A2)</f><v>3</v></c>
                <c r="B1" t="b"><f t="dataTable" ref="B1:B2" dt2D="1" r1="C1" r2="C2" del1="1" ca="1"/><v>1</v></c></row>"#,
        );
        match cell_at(&sheet, 1, 1).map(|c| &c.value) {
            Some(CellValue::Formula { text, kind, cached }) => {
                assert_eq!(text, "SUM(A1:A2)");
                assert!(matches!(kind, FormulaKind::Array { ref_ } if ref_ == "A1:B2"));
                assert_eq!(cached, &Some(CachedValue::Number(3.0)));
            }
            other => panic!("A1 not an array formula cell: {other:?}"),
        }
        match cell_at(&sheet, 1, 2).map(|c| &c.value) {
            Some(CellValue::Formula { kind, cached, .. }) => {
                assert!(matches!(
                    kind,
                    FormulaKind::DataTable {
                        ref_, dt2d, dtr, r1, r2, del1, del2, ca
                    } if ref_ == "B1:B2"
                        && *dt2d
                        && !*dtr
                        && r1.as_deref() == Some("C1")
                        && r2.as_deref() == Some("C2")
                        && *del1
                        && !*del2
                        && *ca
                ));
                assert_eq!(cached, &Some(CachedValue::Bool(true)));
            }
            other => panic!("B1 not a dataTable formula cell: {other:?}"),
        }
    }

    #[test]
    fn hydrate_translates_shared_formula_dependents() {
        let sheet = hydrate(
            r#"<row r="1"><c r="A1"><f t="shared" ref="A1:A3" si="0">A1+A2</f><v>3</v></c></row>
               <row r="2"><c r="A2" t="str"><f t="shared" si="0"/></c></row>
               <row r="3"><c r="A3" t="str"><f t="shared" si="0"/></c></row>"#,
        );
        let CellValue::Formula { text, cached, .. } = &cell_at(&sheet, 1, 1).unwrap().value else {
            panic!("A1 not a formula");
        };
        assert_eq!(text, "A1+A2");
        assert_eq!(cached, &Some(CachedValue::Number(3.0)));
        for (row, expected) in [(2, "A2+A3"), (3, "A3+A4")] {
            let CellValue::Formula { text, .. } = &cell_at(&sheet, row, 1).unwrap().value else {
                panic!("A{row} not a translated formula");
            };
            assert_eq!(
                text, expected,
                "A{row} must carry the translated anchor text"
            );
        }
    }

    #[test]
    fn hydrate_translates_shared_anchor_with_leading_equals() {
        let sheet = hydrate(
            r#"<row r="1"><c r="A1"><f t="shared" ref="A1:A2" si="0">=A1*2</f><v>2</v></c></row>
               <row r="2"><c r="A2"><f t="shared" si="0"/></c></row>"#,
        );
        let CellValue::Formula { text, .. } = &cell_at(&sheet, 1, 1).unwrap().value else {
            panic!("A1 not a formula");
        };
        assert_eq!(text, "=A1*2");
        let CellValue::Formula { text, .. } = &cell_at(&sheet, 2, 1).unwrap().value else {
            panic!("A2 not a translated formula");
        };
        assert_eq!(text, "=A2*2");
    }

    #[test]
    fn hydrate_orphan_shared_dependent_is_empty() {
        let sheet = hydrate(r#"<row r="1"><c r="A1"><f t="shared" si="99"/></c></row>"#);
        let cell = cell_at(&sheet, 1, 1).unwrap();
        assert!(matches!(cell.value, CellValue::Empty));
    }

    #[test]
    fn hydrate_preserves_xlfn_verbatim() {
        let sheet =
            hydrate(r#"<row r="1"><c r="A1"><f>_xlfn.XLOOKUP(B1,B2:B3,B4)</f><v>5</v></c></row>"#);
        let CellValue::Formula { text, cached, .. } = &cell_at(&sheet, 1, 1).unwrap().value else {
            panic!("A1 not a formula");
        };
        assert_eq!(text, "_xlfn.XLOOKUP(B1,B2:B3,B4)");
        assert_eq!(cached, &Some(CachedValue::Number(5.0)));
    }

    #[test]
    fn hydrate_decodes_entity_in_str_cache() {
        let sheet = hydrate(r#"<row r="1"><c r="A1" t="str"><f>"x"</f><v>A&amp;B</v></c></row>"#);
        let CellValue::Formula { cached, .. } = &cell_at(&sheet, 1, 1).unwrap().value else {
            panic!("A1 not a formula");
        };
        assert_eq!(cached, &Some(CachedValue::Str("A&B".into())));
    }

    #[test]
    fn hydrate_decodes_entity_in_error_cache() {
        let sheet =
            hydrate(r#"<row r="1"><c r="A1" t="e"><f>NA()</f><v>#NAME&amp;?</v></c></row>"#);
        let CellValue::Formula { cached, .. } = &cell_at(&sheet, 1, 1).unwrap().value else {
            panic!("A1 not a formula");
        };
        assert_eq!(cached, &Some(CachedValue::Error("#NAME&?".into())));
    }

    #[test]
    fn emit_cell_re_escapes_decoded_str_cache() {
        let sheet = hydrate(r#"<row r="1"><c r="A1" t="str"><f>"x"</f><v>A&amp;B</v></c></row>"#);
        let a = cell_at(&sheet, 1, 1).unwrap();
        let mut out = Vec::new();
        emit_cell(&mut out, 1, 1, &a.value, None);
        // Decoded in the model, re-escaped on emit: `A&B` round-trips byte-for-byte.
        assert_eq!(
            String::from_utf8_lossy(&out),
            r#"<c r="A1" t="str"><f>"x"</f><v>A&amp;B</v></c>"#
        );
    }

    #[test]
    fn hydrate_resolves_formula_sst_cache_via_shared_strings() {
        let sst =
            r#"<sst count="2" uniqueCount="2"><si><t>alpha</t></si><si><t>a&amp;b</t></si></sst>"#;
        let sheet = hydrate_sst(
            r#"<row r="1"><c r="A1" t="s"><f>INDEX(B1:B2,1)</f><v>0</v></c></row>
                <row r="2"><c r="A2" t="s"><f>INDEX(B1:B2,2)</f><v>1</v></c></row>"#,
            sst,
        );
        let a = cell_at(&sheet, 1, 1).unwrap();
        let b = cell_at(&sheet, 2, 1).unwrap();
        let CellValue::Formula { cached, .. } = &a.value else {
            panic!("A1 not a formula");
        };
        assert_eq!(cached, &Some(CachedValue::Str("alpha".into())));
        let CellValue::Formula { cached, .. } = &b.value else {
            panic!("A2 not a formula");
        };
        assert_eq!(cached, &Some(CachedValue::Str("a&b".into())));
    }

    #[test]
    fn hydrate_out_of_range_sst_cache_is_dropped() {
        let sst = r#"<sst count="1" uniqueCount="1"><si><t>alpha</t></si></sst>"#;
        let sheet = hydrate_sst(
            r#"<row r="1"><c r="A1" t="s"><f>INDEX(B1:B2,1)</f><v>99</v></c></row>"#,
            sst,
        );
        let CellValue::Formula { cached, .. } = &cell_at(&sheet, 1, 1).unwrap().value else {
            panic!("A1 not a formula");
        };
        assert_eq!(cached, &None);
    }

    #[test]
    fn hydrate_decodes_formula_text_entities() {
        let sheet =
            hydrate(r#"<row r="1"><c r="A1"><f>IF(A1&lt;5,"x&amp;y",1)</f><v>1</v></c></row>"#);
        let CellValue::Formula { text, .. } = &cell_at(&sheet, 1, 1).unwrap().value else {
            panic!("A1 not a formula");
        };
        assert_eq!(text, "IF(A1<5,\"x&y\",1)");
    }

    #[test]
    fn hydrate_decodes_named_and_numeric_entities() {
        let sheet = hydrate(
            r#"<row r="1"><c r="A1"><f>&quot;a&quot;&amp;&#60;&#x3E;&apos;b&apos;</f><v>1</v></c></row>"#,
        );
        let CellValue::Formula { text, .. } = &cell_at(&sheet, 1, 1).unwrap().value else {
            panic!("A1 not a formula");
        };
        assert_eq!(text, "\"a\"&<>'b'");
    }

    #[test]
    fn hydrate_decodes_shared_anchor_entities_before_translation() {
        let sheet = hydrate(
            r#"<row r="1"><c r="A1"><f t="shared" ref="A1:A2" si="0">IF(A1&lt;5,"x&amp;y",0)</f><v>1</v></c></row>
               <row r="2"><c r="A2"><f t="shared" si="0"/></c></row>"#,
        );
        let CellValue::Formula { text, .. } = &cell_at(&sheet, 1, 1).unwrap().value else {
            panic!("A1 not a formula");
        };
        assert_eq!(text, "IF(A1<5,\"x&y\",0)");
        let CellValue::Formula { text, .. } = &cell_at(&sheet, 2, 1).unwrap().value else {
            panic!("A2 not a translated formula");
        };
        assert_eq!(text, "IF(A2<5,\"x&y\",0)");
    }

    #[test]
    fn emit_cell_re_escapes_decoded_formula_text() {
        let sheet =
            hydrate(r#"<row r="1"><c r="A1"><f>IF(A1&lt;5,"x&amp;y",1)</f><v>1</v></c></row>"#);
        let a = cell_at(&sheet, 1, 1).unwrap();
        let mut out = Vec::new();
        emit_cell(&mut out, 1, 1, &a.value, None);
        // Decoded in the model, re-escaped on emit: the saved bytes match the
        // source bytes for the `<f>` body (no double-escape, no lost entity).
        assert_eq!(
            String::from_utf8_lossy(&out),
            r#"<c r="A1"><f>IF(A1&lt;5,"x&amp;y",1)</f><v>1.0</v></c>"#
        );
    }

    #[test]
    fn emit_cell_round_trips_array_and_data_table_kinds() {
        let sheet = hydrate(
            r#"<row r="1"><c r="A1"><f t="array" ref="A1:B2">SUM(A1:A2)</f><v>3</v></c>
                <c r="B1" t="b"><f t="dataTable" ref="B1:B2" dt2D="1" r1="C1" r2="C2" del1="1" ca="1"/><v>1</v></c></row>"#,
        );
        let a = cell_at(&sheet, 1, 1).unwrap();
        let b = cell_at(&sheet, 1, 2).unwrap();
        let mut out = Vec::new();
        emit_cell(&mut out, 1, 1, &a.value, None);
        assert_eq!(
            String::from_utf8_lossy(&out),
            r#"<c r="A1"><f t="array" ref="A1:B2">SUM(A1:A2)</f><v>3.0</v></c>"#
        );
        let mut out = Vec::new();
        emit_cell(&mut out, 1, 2, &b.value, None);
        assert_eq!(
            String::from_utf8_lossy(&out),
            r#"<c r="B1" t="b"><f t="dataTable" ref="B1:B2" dt2D="1" r1="C1" r2="C2" del1="1" ca="1"/><v>1</v></c>"#
        );
    }
}
