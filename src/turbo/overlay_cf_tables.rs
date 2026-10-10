//! Editable-path (overlay) authoring of conditional formatting, data
//! validations and tables (`ws.conditional_formatting.add`,
//! `ws.add_data_validation`, `ws.add_table`).
//!
//! No new XML generators: CF blocks come from `ConditionalFormatting::emit`,
//! dxfs from `StyleEngine` / `DxfDesc::emit`, table parts from `write_table`,
//! DV blocks from the existing `splice_sheet_controls` path. This module only
//! allocates ids/parts/rels and splices the results into the saved package.

use ahash::AHashMap;

use crate::turbo::error::{TurboError, TurboResult};
use crate::turbo::overlay::{
    WorkbookOverlay, extract_xml_attr, find_element, inflate_entry, sheet_table_parts,
    splice_tail_control,
};
use crate::turbo::scan::parse_ref_range_strict;
use crate::turbo::write::{
    CellValue, ConditionalFormatting, DataValidation, RichRun, StyleEngine, TableDef, write_table,
};
use crate::turbo::zipmin::ArchiveMap;

const TABLE_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/table";
const TABLE_CT: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.table+xml";
const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const EMPTY_RELS: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\n</Relationships>";

/// Sentinel tag that never occurs: makes `splice_tail_control` a pure
/// "insert after the last predecessor" (it would otherwise replace an
/// existing element of the same name, and CF blocks / tableParts must append).
const NO_TAG: &[u8] = b"kyraxNeverPresent";

/// ECMA-376 CT_Worksheet elements that precede `<conditionalFormatting>`
/// (including itself, so new blocks follow existing ones).
const CF_PREDECESSORS: &[&[u8]] = &[
    b"conditionalFormatting",
    b"phoneticPr",
    b"mergeCells",
    b"customSheetViews",
    b"dataConsolidate",
    b"sortState",
    b"autoFilter",
    b"scenarios",
    b"protectedRanges",
    b"sheetProtection",
    b"sheetCalcPr",
    b"sheetData",
];

/// Every CT_Worksheet element that precedes `<tableParts>` (only `extLst`
/// follows it).
const TABLE_PARTS_PREDECESSORS: &[&[u8]] = &[
    b"mc:AlternateContent",
    b"legacyDrawingHF",
    b"legacyDrawing",
    b"drawing",
    b"picture",
    b"oleObjects",
    b"controls",
    b"webPublishItems",
    b"ignoredErrors",
    b"smartTags",
    b"cellWatches",
    b"customProperties",
    b"colBreaks",
    b"rowBreaks",
    b"headerFooter",
    b"pageSetup",
    b"pageMargins",
    b"printOptions",
    b"hyperlinks",
    b"dataValidations",
    b"conditionalFormatting",
    b"phoneticPr",
    b"mergeCells",
    b"customSheetViews",
    b"dataConsolidate",
    b"sortState",
    b"autoFilter",
    b"scenarios",
    b"protectedRanges",
    b"sheetProtection",
    b"sheetCalcPr",
    b"sheetData",
];

/// One `<table>` part already present in the source package.
struct ExistingTable {
    part: String,
    id: u32,
    name: String,
    ref_: String,
}

fn parse_table_root(part: &str, xml: &[u8]) -> Option<ExistingTable> {
    let s = find_element(xml, b"table", 0)?;
    let gt = s + memchr::memchr(b'>', &xml[s..])?;
    let tag = &xml[s..=gt];
    Some(ExistingTable {
        part: part.to_string(),
        id: extract_xml_attr(tag, b"id")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        name: extract_xml_attr(tag, b"displayName")
            .or_else(|| extract_xml_attr(tag, b"name"))
            .unwrap_or_default(),
        ref_: extract_xml_attr(tag, b"ref").unwrap_or_default(),
    })
}

fn is_table_part(entry: &str) -> bool {
    entry.starts_with("xl/tables/") && entry.ends_with(".xml") && !entry.contains("/_rels/")
}

/// Every table part in the package (all sheets).
fn scan_existing_tables(map: &ArchiveMap) -> TurboResult<Vec<ExistingTable>> {
    let mut out = Vec::new();
    for entry in &map.entry_order {
        if !is_table_part(entry) {
            continue;
        }
        if let Some(xml) = inflate_entry(map, entry)? {
            if let Some(t) = parse_table_root(entry, &xml) {
                out.push(t);
            }
        }
    }
    Ok(out)
}

fn ranges_overlap(a: &str, b: &str) -> bool {
    match (
        parse_ref_range_strict(a.as_bytes()),
        parse_ref_range_strict(b.as_bytes()),
    ) {
        (Some((ar1, ac1, ar2, ac2)), Some((br1, bc1, br2, bc2))) => {
            ar1 <= br2 && br1 <= ar2 && ac1 <= bc2 && bc1 <= ac2
        }
        _ => false,
    }
}

/// Excel's table-name rules (subset): starts with a letter, `_` or `\`; the
/// rest letters, digits, `_` or `.`; no spaces; not a cell reference.
fn validate_table_name(name: &str) -> TurboResult<()> {
    let mut chars = name.chars();
    let ok_first = matches!(chars.next(), Some(c) if c.is_alphabetic() || c == '_' || c == '\\');
    let ok_rest = chars.all(|c| c.is_alphanumeric() || c == '_' || c == '.');
    let looks_like_ref = parse_ref_range_strict(name.as_bytes()).is_some();
    if !ok_first || !ok_rest || looks_like_ref || name.chars().count() > 255 {
        return Err(TurboError::Format(format!(
            "invalid table name {name:?}: use letters, digits, '_' or '.', starting with a letter or '_'"
        )));
    }
    Ok(())
}

/// Text Excel would show for a header cell; `None` for a blank cell.
fn header_text(v: &CellValue) -> Option<String> {
    let s = match v {
        CellValue::Empty => return None,
        CellValue::Str(s) => s.clone(),
        CellValue::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        CellValue::Error(e) => e.clone(),
        CellValue::Number(n)
        | CellValue::DateSerial(n)
        | CellValue::Time(n)
        | CellValue::Duration(n) => {
            if n.fract() == 0.0 && n.abs() < 1e15 {
                format!("{}", *n as i64)
            } else {
                format!("{n}")
            }
        }
        CellValue::Rich(rt) => rt
            .runs
            .iter()
            .map(|r| match r {
                RichRun::Text(t) => t.as_str(),
                RichRun::Block { text, .. } => text.as_str(),
            })
            .collect(),
        CellValue::Formula { .. } => return None,
    };
    if s.is_empty() { None } else { Some(s) }
}

impl WorkbookOverlay {
    /// `(displayName, ref)` of every table on `sheet`: existing parts first,
    /// then the ones added in this session.
    pub fn sheet_tables(&self, sheet: &str) -> TurboResult<Vec<(String, String)>> {
        let mut out = Vec::new();
        if let Some(entry) = self.archive_map.sheet_name_map.get(sheet) {
            if let Some(xml) = inflate_entry(&self.archive_map, entry)? {
                for part in sheet_table_parts(&self.archive_map, entry, &xml) {
                    if let Some(tx) = inflate_entry(&self.archive_map, &part)? {
                        if let Some(t) = parse_table_root(&part, &tx) {
                            out.push((t.name, t.ref_));
                        }
                    }
                }
            }
        }
        if let Some(so) = self.sheet_overlays.get(sheet) {
            for t in &so.tables_new {
                out.push((t.display_name.clone(), t.ref_.clone()));
            }
        }
        Ok(out)
    }

    /// Queue a new table on `sheet`. Header names are resolved from the
    /// header-row cells at save time (`prepare_tables`).
    pub fn add_table(&mut self, sheet: &str, t: TableDef) -> TurboResult<()> {
        validate_table_name(&t.display_name)?;
        let Some((r1, _, r2, _)) = parse_ref_range_strict(t.ref_.as_bytes()) else {
            return Err(TurboError::Format(format!(
                "invalid table ref {:?}",
                t.ref_
            )));
        };
        if r2 <= r1 {
            return Err(TurboError::Format(format!(
                "table ref {:?} needs a header row and at least one data row",
                t.ref_
            )));
        }
        let lower = t.display_name.to_lowercase();
        let taken = scan_existing_tables(&self.archive_map)?
            .iter()
            .any(|e| e.name.to_lowercase() == lower)
            || self
                .sheet_overlays
                .values()
                .flat_map(|so| so.tables_new.iter())
                .any(|n| n.display_name.to_lowercase() == lower);
        if taken {
            return Err(TurboError::Format(format!(
                "a table named {:?} already exists in this workbook",
                t.display_name
            )));
        }
        for (name, r) in self.sheet_tables(sheet)? {
            if ranges_overlap(&r, &t.ref_) {
                return Err(TurboError::Format(format!(
                    "table ref {:?} overlaps table {name:?} ({r})",
                    t.ref_
                )));
            }
        }
        let so = self.sheet_overlays.entry(sheet.to_string()).or_default();
        so.tables_new.push(t);
        so.is_dirty = true;
        Ok(())
    }

    /// Queue conditional-formatting ranges; rules on an already-queued range
    /// join that range's block (openpyxl groups by range the same way).
    pub fn add_conditional_formatting(&mut self, sheet: &str, cfs: Vec<ConditionalFormatting>) {
        let so = self.sheet_overlays.entry(sheet.to_string()).or_default();
        for cf in cfs {
            match so.cf_new.iter_mut().find(|c| c.sqref == cf.sqref) {
                Some(existing) => existing.rules.extend(cf.rules),
                None => so.cf_new.push(cf),
            }
        }
        so.is_dirty = true;
    }

    /// `(sqref, [rule type])` per CF range: existing ranges, then queued ones.
    pub fn conditional_formatting_summary(
        &self,
        sheet: &str,
    ) -> TurboResult<Vec<(String, Vec<String>)>> {
        let mut out: Vec<(String, Vec<String>)> = Vec::new();
        if let Some(entry) = self.archive_map.sheet_name_map.get(sheet) {
            if let Some(xml) = inflate_entry(&self.archive_map, entry)? {
                for r in crate::turbo::meta::scan_conditional_formatting(&xml) {
                    match out.iter_mut().find(|(s, _)| *s == r.sqref) {
                        Some((_, types)) => types.push(r.type_),
                        None => out.push((r.sqref, vec![r.type_])),
                    }
                }
            }
        }
        if let Some(so) = self.sheet_overlays.get(sheet) {
            for cf in &so.cf_new {
                let mut buf = Vec::new();
                cf.emit(&mut buf);
                let types = crate::turbo::meta::scan_conditional_formatting(&buf)
                    .into_iter()
                    .map(|r| r.type_)
                    .collect();
                out.push((cf.sqref.clone(), types));
            }
        }
        Ok(out)
    }

    /// Append one validation, keeping the sheet's existing ones. Returns its
    /// index for later `set_data_validation` (openpyxl lets `dv.add(range)`
    /// run after `ws.add_data_validation(dv)`).
    pub fn add_data_validation(&mut self, sheet: &str, dv: DataValidation) -> TurboResult<usize> {
        let needs_seed = self
            .sheet_overlays
            .get(sheet)
            .is_none_or(|so| so.data_validations.is_empty());
        let seed = if needs_seed {
            self.ensure_data_validations(sheet)?;
            self.data_validations_cache
                .get(sheet)
                .cloned()
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let so = self.sheet_overlays.entry(sheet.to_string()).or_default();
        if needs_seed {
            so.data_validations = seed;
        }
        so.data_validations.push(dv);
        so.is_dirty = true;
        Ok(so.data_validations.len() - 1)
    }

    /// Replace the validation at `idx` (no-op when out of range).
    pub fn set_data_validation(&mut self, sheet: &str, idx: usize, dv: DataValidation) {
        if let Some(so) = self.sheet_overlays.get_mut(sheet) {
            if let Some(slot) = so.data_validations.get_mut(idx) {
                *slot = dv;
                so.is_dirty = true;
            }
        }
    }
}

/// Save pre-pass: name each queued table's columns from its header-row cells
/// (overlay edits first, then the sheet's stored values). Blank or duplicate
/// headers get Excel's own fix (`ColumnN`, `Name2`) written back to the cell,
/// because a header cell that disagrees with its column name makes Excel
/// repair the file.
pub(crate) fn prepare_tables(ov: &mut WorkbookOverlay) -> TurboResult<()> {
    let mut names: Vec<String> = ov
        .sheet_overlays
        .iter()
        .filter(|(_, so)| !so.tables_new.is_empty())
        .map(|(n, _)| n.clone())
        .collect();
    if names.is_empty() {
        return Ok(());
    }
    names.sort();
    for name in names {
        let hydrated = ov.hydrated_sheet(&name)?;
        let stored = |r: u32, c: u32| -> Option<CellValue> {
            let sheet = hydrated.as_ref()?;
            let row = sheet.rows.iter().find(|row| row.row == r)?;
            row.cells.iter().find(|cell| cell.col == c).map(|cell| cell.value.clone())
        };
        let Some(so) = ov.sheet_overlays.get_mut(&name) else {
            continue;
        };
        let mut writes: Vec<((u32, u32), String)> = Vec::new();
        for t in so.tables_new.iter_mut() {
            let Some((r1, c1, _, c2)) = parse_ref_range_strict(t.ref_.as_bytes()) else {
                continue;
            };
            let mut cols: Vec<String> = Vec::with_capacity((c2 - c1 + 1) as usize);
            for c in c1..=c2 {
                let val = so
                    .modified_cells
                    .get(&(r1, c))
                    .cloned()
                    .or_else(|| stored(r1, c));
                let is_formula = matches!(val, Some(CellValue::Formula { .. }));
                let text = val.as_ref().and_then(header_text);
                let base = text
                    .clone()
                    .unwrap_or_else(|| format!("Column{}", c - c1 + 1));
                let mut name = base.clone();
                let mut k = 2;
                while cols.iter().any(|x| x.to_lowercase() == name.to_lowercase()) {
                    name = format!("{base}{k}");
                    k += 1;
                }
                if !is_formula && text.as_deref() != Some(name.as_str()) {
                    writes.push(((r1, c), name.clone()));
                }
                cols.push(name);
            }
            t.columns = cols;
        }
        for (pos, s) in writes {
            so.modified_cells.insert(pos, CellValue::Str(s));
        }
    }
    Ok(())
}

/// New `<dxf>` records appended to `styles.xml` for queued CF rules.
pub(crate) struct DxfAddition {
    xml: Vec<u8>,
    count: u32,
}

/// Count `<dxf>` records in a styles part.
fn count_dxfs(styles: &[u8]) -> u32 {
    let Some(s) = find_element(styles, b"dxfs", 0) else {
        return 0;
    };
    let Some(end) = memchr::memmem::find(&styles[s..], b"</dxfs>") else {
        return 0; // self-closing <dxfs/>
    };
    let body = &styles[s..s + end];
    let mut n = 0u32;
    let mut pos = 0usize;
    while let Some(p) = find_element(body, b"dxf", pos) {
        n += 1;
        pos = p + 4;
    }
    n
}

/// Append `add` to the `<dxfs>` pool of `styles` (creating it in schema order
/// — after cellStyles, before tableStyles/colors/extLst — when absent).
pub(crate) fn patch_styles_dxfs(styles: Vec<u8>, add: Option<&DxfAddition>) -> Vec<u8> {
    let Some(add) = add else {
        return styles;
    };
    let total = count_dxfs(&styles) + add.count;
    let mut block = format!("<dxfs count=\"{total}\">").into_bytes();
    let (start, end) = match find_element(&styles, b"dxfs", 0) {
        Some(s) => {
            let gt = s + memchr::memchr(b'>', &styles[s..]).unwrap_or(0);
            if styles[gt - 1] == b'/' {
                (s, gt + 1)
            } else {
                let close = memchr::memmem::find(&styles[gt..], b"</dxfs>")
                    .map(|p| gt + p)
                    .unwrap_or(gt + 1);
                block.extend_from_slice(&styles[gt + 1..close]);
                (s, close + b"</dxfs>".len())
            }
        }
        None => {
            let at = [&b"<tableStyles"[..], b"<colors", b"<extLst", b"</styleSheet>"]
                .iter()
                .find_map(|a| memchr::memmem::find(&styles, a))
                .unwrap_or(styles.len());
            (at, at)
        }
    };
    block.extend_from_slice(&add.xml);
    block.extend_from_slice(b"</dxfs>");
    let mut out = Vec::with_capacity(styles.len() + block.len());
    out.extend_from_slice(&styles[..start]);
    out.extend_from_slice(&block);
    out.extend_from_slice(&styles[end..]);
    out
}

fn ensure_r_namespace(xml: &mut Vec<u8>) {
    let Some(s) = find_element(xml, b"worksheet", 0) else {
        return;
    };
    let Some(gt) = memchr::memchr(b'>', &xml[s..]).map(|p| s + p) else {
        return;
    };
    if memchr::memmem::find(&xml[s..gt], b"xmlns:r=").is_some() {
        return;
    }
    let decl = format!(" xmlns:r=\"{REL_NS}\"");
    let at = if xml[gt - 1] == b'/' { gt - 1 } else { gt };
    let tail = xml.split_off(at);
    xml.extend_from_slice(decl.as_bytes());
    xml.extend_from_slice(&tail);
}

fn max_rel_id(rels: &str) -> u32 {
    let mut max_id = 0u32;
    let mut pos = 0usize;
    while let Some(p) = rels[pos..].find("Id=\"rId") {
        let start = pos + p + "Id=\"rId".len();
        let Some(q) = rels[start..].find('"') else {
            break;
        };
        if let Ok(n) = rels[start..start + q].parse::<u32>() {
            max_id = max_id.max(n);
        }
        pos = start + q;
    }
    max_id
}

/// Merge new `<tablePart>` refs into the sheet's `<tableParts>` (or insert
/// the element right after its last schema predecessor).
fn splice_table_parts(xml: &mut Vec<u8>, new_rids: &[String]) {
    let mut parts: Vec<Vec<u8>> = Vec::new();
    if let Some(s) = find_element(xml, b"tableParts", 0) {
        let gt = s + memchr::memchr(b'>', &xml[s..]).unwrap_or(0);
        let end = if xml[gt - 1] == b'/' {
            gt + 1
        } else {
            memchr::memmem::find(&xml[gt..], b"</tableParts>")
                .map(|p| gt + p + b"</tableParts>".len())
                .unwrap_or(gt + 1)
        };
        let mut pos = s + 1;
        while let Some(p) = find_element(&xml[..end], b"tablePart", pos) {
            let pe = p + memchr::memchr(b'>', &xml[p..end]).unwrap_or(0);
            parts.push(xml[p..=pe].to_vec());
            pos = pe + 1;
        }
        xml.drain(s..end);
    }
    for rid in new_rids {
        parts.push(format!("<tablePart r:id=\"{rid}\"/>").into_bytes());
    }
    let mut block = format!("<tableParts count=\"{}\">", parts.len()).into_bytes();
    for p in &parts {
        block.extend_from_slice(p);
    }
    block.extend_from_slice(b"</tableParts>");
    splice_tail_control(xml, NO_TAG, &block, TABLE_PARTS_PREDECESSORS, b"</worksheet>");
}

/// Save hook: splice queued CF blocks and table parts into the rendered
/// sheets, adding table parts, sheet rels and content-type overrides. Runs
/// after every other sheet splice so `<tableParts>` lands after the drawing /
/// legacyDrawing elements. Returns the dxfs `styles.xml` must gain.
pub(crate) fn apply_cf_tables(
    ov: &WorkbookOverlay,
    rendered: &mut AHashMap<String, Vec<u8>>,
    rewritten_parts: &mut AHashMap<String, Vec<u8>>,
) -> TurboResult<Option<DxfAddition>> {
    let mut names: Vec<&String> = ov
        .sheet_overlays
        .iter()
        .filter(|(n, so)| {
            (!so.cf_new.is_empty() || !so.tables_new.is_empty())
                && !ov.deleted_sheets.contains(*n)
        })
        .map(|(n, _)| n)
        .collect();
    if names.is_empty() {
        return Ok(None);
    }
    names.sort();

    let map = &ov.archive_map;
    let base_dxf = inflate_entry(map, "xl/styles.xml")?
        .map(|s| count_dxfs(&s))
        .unwrap_or(0);
    let mut dxf_engine = StyleEngine::new();

    let existing = scan_existing_tables(map)?;
    let mut next_table_id = existing.iter().map(|t| t.id).max().unwrap_or(0) + 1;
    let mut next_part_no = existing
        .iter()
        .filter_map(|t| {
            t.part
                .strip_prefix("xl/tables/table")?
                .strip_suffix(".xml")?
                .parse::<u32>()
                .ok()
        })
        .max()
        .unwrap_or(0)
        + 1;
    let mut ct_additions = String::new();

    for name in names {
        let so = &ov.sheet_overlays[name];
        let entry = map
            .sheet_name_map
            .get(name)
            .cloned()
            .unwrap_or_else(|| format!("xl/worksheets/{name}.xml"));
        let mut xml = match rendered.remove(&entry) {
            Some(x) => x,
            None => match inflate_entry(map, &entry)? {
                Some(x) => x,
                None => continue,
            },
        };

        if !so.cf_new.is_empty() {
            let mut priority = crate::turbo::meta::scan_conditional_formatting(&xml)
                .iter()
                .map(|r| r.priority.max(0) as u32)
                .max()
                .unwrap_or(0);
            let mut block = Vec::new();
            for cf in &so.cf_new {
                let mut cf = cf.clone();
                cf.register_dxfs(&mut dxf_engine);
                for rule in &mut cf.rules {
                    priority += 1;
                    rule.priority = priority;
                    if let Some(id) = rule.dxf_id.as_mut() {
                        *id += base_dxf;
                    }
                }
                cf.emit(&mut block);
            }
            splice_tail_control(&mut xml, NO_TAG, &block, CF_PREDECESSORS, b"</worksheet>");
        }

        if !so.tables_new.is_empty() {
            let stem = entry.rsplit('/').next().unwrap_or("sheet1.xml");
            let rels_path = format!("xl/worksheets/_rels/{stem}.rels");
            let mut rels = match rendered.get(&rels_path) {
                Some(r) => String::from_utf8_lossy(r).into_owned(),
                None => match inflate_entry(map, &rels_path)? {
                    Some(r) => String::from_utf8_lossy(&r).into_owned(),
                    None => EMPTY_RELS.to_string(),
                },
            };
            let mut rid = max_rel_id(&rels);
            let mut rel_add = String::new();
            let mut new_rids = Vec::new();
            for t in &so.tables_new {
                let part = format!("xl/tables/table{next_part_no}.xml");
                rendered.insert(
                    part.clone(),
                    write_table(t, next_table_id as usize).into_bytes(),
                );
                rid += 1;
                rel_add.push_str(&format!(
                    "<Relationship Id=\"rId{rid}\" Type=\"{TABLE_REL_TYPE}\" Target=\"../tables/table{next_part_no}.xml\"/>"
                ));
                ct_additions.push_str(&format!(
                    "<Override PartName=\"/{part}\" ContentType=\"{TABLE_CT}\"/>"
                ));
                new_rids.push(format!("rId{rid}"));
                next_part_no += 1;
                next_table_id += 1;
            }
            if let Some(at) = rels.rfind("</Relationships>") {
                rels.insert_str(at, &rel_add);
            }
            rendered.insert(rels_path, rels.into_bytes());
            splice_table_parts(&mut xml, &new_rids);
            ensure_r_namespace(&mut xml);
        }

        rendered.insert(entry, xml);
    }

    if !ct_additions.is_empty() {
        const CT: &str = "[Content_Types].xml";
        let ct = match rendered.remove(CT).or_else(|| rewritten_parts.remove(CT)) {
            Some(c) => Some(c),
            None => inflate_entry(map, CT)?,
        };
        if let Some(ct) = ct {
            let mut ct = String::from_utf8_lossy(&ct).into_owned();
            if let Some(at) = ct.rfind("</Types>") {
                ct.insert_str(at, &ct_additions);
            }
            rendered.insert(CT.to_string(), ct.into_bytes());
        }
    }

    let dxfs = dxf_engine.dxfs();
    if dxfs.is_empty() {
        return Ok(None);
    }
    let mut xml = Vec::new();
    for d in dxfs {
        d.emit(&mut xml);
    }
    Ok(Some(DxfAddition {
        xml,
        count: dxfs.len() as u32,
    }))
}

#[cfg(feature = "python")]
pub(crate) mod py {
    //! Thin PyO3 doorway: `ws.conditional_formatting` list object plus the
    //! spec conversion shared by the `EditableSheet` methods.

    use std::sync::{Arc, Mutex};

    use pyo3::exceptions::PyValueError;
    use pyo3::prelude::*;
    use pyo3::types::{PyDict, PyList};

    use crate::turbo::overlay::WorkbookOverlay;

    /// `obj._kx_spec()` when the object offers it (kyrax data-holder classes),
    /// else the object itself (a plain dict spec).
    pub(crate) fn spec_of<'py>(obj: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        if obj.hasattr("_kx_spec")? {
            obj.call_method0("_kx_spec")
        } else {
            Ok(obj.clone())
        }
    }

    fn lock(ov: &Arc<Mutex<WorkbookOverlay>>) -> PyResult<std::sync::MutexGuard<'_, WorkbookOverlay>> {
        ov.lock().map_err(|e| PyValueError::new_err(e.to_string()))
    }

    /// openpyxl `ws.conditional_formatting`: `.add(range, rule)`, `len()`,
    /// iteration over `{"sqref", "rules"}` dicts (existing + added ranges).
    #[pyclass(name = "ConditionalFormattingList", module = "kyrax._kyrax")]
    pub struct PyConditionalFormattingList {
        pub sheet_name: String,
        pub overlay: Arc<Mutex<WorkbookOverlay>>,
    }

    #[pymethods]
    impl PyConditionalFormattingList {
        #[allow(non_snake_case)] // openpyxl keyword name
        fn add(&self, range_string: &str, cfRule: &Bound<'_, PyAny>) -> PyResult<()> {
            let py = cfRule.py();
            let spec = PyDict::new(py);
            spec.set_item("sqref", range_string)?;
            spec.set_item("rules", PyList::new(py, [spec_of(cfRule)?])?)?;
            let cfs = crate::turbo::write::python::parse_conditional_formatting(
                PyList::new(py, [spec])?.as_any(),
            )?;
            lock(&self.overlay)?.add_conditional_formatting(&self.sheet_name, cfs);
            Ok(())
        }

        fn _summary<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
            let summary = lock(&self.overlay)?
                .conditional_formatting_summary(&self.sheet_name)
                .map_err(|e| PyValueError::new_err(e.to_string()))?;
            let out = PyList::empty(py);
            for (sqref, types) in summary {
                let d = PyDict::new(py);
                d.set_item("sqref", sqref)?;
                let rules = PyList::empty(py);
                for t in types {
                    let r = PyDict::new(py);
                    r.set_item("type", t)?;
                    rules.append(r)?;
                }
                d.set_item("rules", rules)?;
                out.append(d)?;
            }
            Ok(out)
        }

        fn __len__(&self, py: Python<'_>) -> PyResult<usize> {
            Ok(self._summary(py)?.len())
        }

        fn __iter__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
            self._summary(py)?.as_any().try_iter().map(|it| it.into_any())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dxfs_patch_appends_and_creates() {
        let add = DxfAddition {
            xml: b"<dxf><font><b/></font></dxf>".to_vec(),
            count: 1,
        };
        let s = b"<styleSheet><cellStyles/><dxfs count=\"1\"><dxf/></dxfs><tableStyles/></styleSheet>".to_vec();
        let out = String::from_utf8(patch_styles_dxfs(s, Some(&add))).unwrap();
        assert!(out.contains("<dxfs count=\"2\"><dxf/><dxf><font><b/></font></dxf></dxfs><tableStyles/>"));
        let s = b"<styleSheet><cellStyles/><tableStyles/></styleSheet>".to_vec();
        let out = String::from_utf8(patch_styles_dxfs(s, Some(&add))).unwrap();
        assert!(out.contains("<cellStyles/><dxfs count=\"1\"><dxf>"));
    }

    #[test]
    fn table_parts_merge_and_order() {
        let mut x = b"<worksheet><sheetData/><pageMargins/><tableParts count=\"1\"><tablePart r:id=\"rId1\"/></tableParts></worksheet>".to_vec();
        splice_table_parts(&mut x, &["rId2".to_string()]);
        let s = String::from_utf8(x).unwrap();
        assert!(s.contains("<pageMargins/><tableParts count=\"2\"><tablePart r:id=\"rId1\"/><tablePart r:id=\"rId2\"/></tableParts></worksheet>"));
        let mut x = b"<worksheet><sheetData/><drawing r:id=\"rId1\"/><extLst/></worksheet>".to_vec();
        splice_table_parts(&mut x, &["rId2".to_string()]);
        let s = String::from_utf8(x).unwrap();
        assert!(s.contains("<drawing r:id=\"rId1\"/><tableParts count=\"1\"><tablePart r:id=\"rId2\"/></tableParts><extLst/>"));
    }

    #[test]
    fn table_name_rules() {
        assert!(validate_table_name("Sales_2024").is_ok());
        assert!(validate_table_name("My Table").is_err());
        assert!(validate_table_name("A1").is_err());
        assert!(validate_table_name("1abc").is_err());
    }
}
