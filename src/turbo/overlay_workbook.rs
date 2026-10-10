//! Workbook-level state for the editable overlay (`load_workbook` / `Workbook`):
//! defined names (incl. sheet-scoped `_xlnm.Print_Area` / `_xlnm.Print_Titles`),
//! sheet visibility (`<sheet state=...>`) and read-only core document properties.
//!
//! Lazily loaded from the source `xl/workbook.xml` on first access and applied
//! to the workbook XML at save time. Untouched `<definedName>` elements are
//! re-emitted byte-for-byte (only `localSheetId` is re-pointed when the sheet
//! order changed); nothing is rewritten unless the user changed something.

use ahash::AHashMap;

use crate::turbo::error::{TurboError, TurboResult};
use crate::turbo::overlay::{WorkbookOverlay, inflate_entry};
use crate::turbo::structural::find_attr;
use crate::turbo::write::xml::escape_text;

/// One `<definedName>`. `scope` is the owning sheet NAME (resolved from
/// `localSheetId` at load, re-resolved to an index at save) or `None` = global.
#[derive(Debug, Clone)]
pub struct WbName {
    pub name: String,
    pub scope: Option<String>,
    pub value: String,
    /// Verbatim source element (untouched names round-trip byte-for-byte).
    pub raw: Option<String>,
    /// `localSheetId` as found in `raw` (scoped names whose sheet vanished keep it).
    pub raw_local: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct WorkbookLevel {
    pub names: Vec<WbName>,
    pub names_dirty: bool,
    /// Sheet name -> `hidden` / `veryHidden` (visible sheets are absent).
    pub states: AHashMap<String, String>,
    pub states_dirty: bool,
}

pub const PRINT_AREA: &str = "_xlnm.Print_Area";
pub const PRINT_TITLES: &str = "_xlnm.Print_Titles";

fn decode(raw: &[u8]) -> String {
    let mut scratch = Vec::new();
    String::from_utf8_lossy(crate::turbo::decode::decode_bytes(raw, &mut scratch)).into_owned()
}

fn esc_attr(s: &str) -> String {
    escape_text(s).replace('"', "&quot;")
}

/// Byte range of the `<sheets>` children and each `<sheet .../>` tag inside.
fn sheet_tags(xml: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let Some(so) = xml.find("<sheets") else { return out };
    let se = xml[so..].find("</sheets>").map(|p| so + p).unwrap_or(xml.len());
    let mut i = so + "<sheets".len();
    while let Some(rel) = xml[i..se.max(i)].find("<sheet") {
        let s = i + rel;
        let after = xml.as_bytes().get(s + 6).copied().unwrap_or(b'>');
        let Some(e_rel) = xml[s..].find('>') else { break };
        let e = s + e_rel + 1;
        if after == b' ' || after == b'/' || after == b'>' || after == b'\t' || after == b'\n' {
            out.push((s, e));
        }
        i = e;
    }
    out
}

/// Sheet names (decoded) and states in `<sheets>` order.
pub fn parse_sheets(xml: &str) -> Vec<(String, Option<String>)> {
    sheet_tags(xml)
        .into_iter()
        .map(|(s, e)| {
            let tag = xml[s..e].as_bytes();
            let name = find_attr(tag, b"name").map(decode).unwrap_or_default();
            let state = find_attr(tag, b"state").map(decode);
            (name, state)
        })
        .collect()
}

/// `(start, end)` of the whole `<definedNames>` element, if any.
fn defined_names_span(xml: &str) -> Option<(usize, usize)> {
    let s = xml.find("<definedNames")?;
    let gt = s + xml[s..].find('>')?;
    if xml.as_bytes()[gt - 1] == b'/' {
        return Some((s, gt + 1));
    }
    let close = "</definedNames>";
    let e = gt + xml[gt..].find(close)? + close.len();
    Some((s, e))
}

/// Parse every `<definedName>` (raw element, name, localSheetId, decoded value).
pub fn parse_defined_names(xml: &str) -> Vec<(String, String, Option<u32>, String)> {
    let mut out = Vec::new();
    let Some((ds, de)) = defined_names_span(xml) else { return out };
    let block = &xml[ds..de];
    let mut i = "<definedNames".len();
    while let Some(rel) = block[i..].find("<definedName") {
        let s = i + rel;
        if block[s..].starts_with("<definedNames") {
            i = s + 1;
            continue;
        }
        let Some(gt_rel) = block[s..].find('>') else { break };
        let gt = s + gt_rel;
        let tag = block[s..gt].as_bytes();
        let name = find_attr(tag, b"name").map(decode).unwrap_or_default();
        let local = find_attr(tag, b"localSheetId")
            .and_then(|v| std::str::from_utf8(v).ok()?.parse::<u32>().ok());
        let (value, end) = if block.as_bytes()[gt - 1] == b'/' {
            (String::new(), gt + 1)
        } else {
            let close = "</definedName>";
            let Some(c_rel) = block[gt..].find(close) else { break };
            let c = gt + c_rel;
            (decode(block[gt + 1..c].as_bytes()), c + close.len())
        };
        out.push((block[s..end].to_string(), name, local, value));
        i = end;
    }
    out
}

/// Build the workbook-level model from the original workbook XML. `renames`
/// (old, new) are replayed so a lazily-loaded model sees current sheet names.
pub fn load_level(xml: &str, renames: &[(String, String)]) -> WorkbookLevel {
    let rename = |n: &str| -> String {
        let mut cur = n.to_string();
        for (old, new) in renames {
            if &cur == old {
                cur = new.clone();
            }
        }
        cur
    };
    let sheets = parse_sheets(xml);
    let mut lvl = WorkbookLevel::default();
    for (n, st) in &sheets {
        if let Some(st) = st {
            if st != "visible" {
                lvl.states.insert(rename(n), st.clone());
            }
        }
    }
    for (raw, name, local, value) in parse_defined_names(xml) {
        let orig = local.and_then(|i| sheets.get(i as usize)).map(|(n, _)| n.clone());
        let scope = orig.as_deref().map(rename);
        let mut n = WbName { name, scope, value, raw: Some(raw), raw_local: local };
        // Sheet renamed before this lazy load: re-point its print area / titles.
        if let (Some(o), Some(s)) = (&orig, &n.scope) {
            let print = n.name.eq_ignore_ascii_case(PRINT_AREA) || n.name.eq_ignore_ascii_case(PRINT_TITLES);
            if print && o != s {
                n.value = qualify_refs(s, &n.value);
                n.raw = None;
                lvl.names_dirty = true;
            }
        }
        lvl.names.push(n);
    }
    lvl
}

/// Set (or remove when `val` is None) attribute `attr` inside the start tag
/// `tag` (`<x a="1" .../>`). Returns the new tag text.
fn set_tag_attr(tag: &str, attr: &str, val: Option<&str>) -> String {
    let needle = format!(" {attr}=\"");
    if let Some(p) = tag.find(&needle) {
        let vs = p + needle.len();
        let ve = vs + tag[vs..].find('"').unwrap_or(0);
        match val {
            Some(v) => format!("{}{}{}", &tag[..vs], esc_attr(v), &tag[ve..]),
            None => format!("{}{}", &tag[..p], &tag[ve + 1..]),
        }
    } else if let Some(v) = val {
        let end = if tag.ends_with("/>") { tag.len() - 2 } else { tag.len() - 1 };
        format!("{} {attr}=\"{}\"{}", &tag[..end], esc_attr(v), &tag[end..])
    } else {
        tag.to_string()
    }
}

/// Apply sheet visibility states to every `<sheet>` tag; repoint
/// `workbookView@activeTab` to the first visible sheet when it would land on a
/// hidden one (Excel cannot show a hidden active sheet).
pub fn apply_states(xml: &str, states: &AHashMap<String, String>) -> String {
    let tags = sheet_tags(xml);
    let mut out = String::with_capacity(xml.len() + 32);
    let mut pos = 0;
    let mut visible: Vec<bool> = Vec::new();
    for (s, e) in tags {
        let tag = &xml[s..e];
        let name = find_attr(tag.as_bytes(), b"name").map(decode).unwrap_or_default();
        let st = states.get(&name).map(|s| s.as_str());
        visible.push(st.is_none());
        out.push_str(&xml[pos..s]);
        out.push_str(&set_tag_attr(tag, "state", st));
        pos = e;
    }
    out.push_str(&xml[pos..]);
    let Some(first_visible) = visible.iter().position(|v| *v) else { return out };
    if let Some(s) = out.find("<workbookView") {
        if let Some(gt) = out[s..].find('>') {
            let tag = out[s..s + gt + 1].to_string();
            let active = find_attr(tag.as_bytes(), b"activeTab")
                .and_then(|v| std::str::from_utf8(v).ok()?.parse::<usize>().ok())
                .unwrap_or(0);
            if !visible.get(active).copied().unwrap_or(true) {
                let fixed = set_tag_attr(&tag, "activeTab", Some(&first_visible.to_string()));
                out.replace_range(s..s + gt + 1, &fixed);
            }
        }
    }
    out
}

/// Replace (or insert / remove) the `<definedNames>` block with `names`,
/// resolving each scoped name's `localSheetId` against the `<sheets>` order of
/// `xml`. Names scoped to a sheet that no longer exists are dropped.
pub fn apply_names(xml: &str, names: &[WbName]) -> String {
    let order: Vec<String> = parse_sheets(xml).into_iter().map(|(n, _)| n).collect();
    let mut block = String::new();
    for n in names {
        let local = match &n.scope {
            Some(sheet) => match order.iter().position(|o| o == sheet) {
                Some(i) => Some(i as u32),
                None => continue,
            },
            None => None,
        };
        if let Some(raw) = &n.raw {
            if local == n.raw_local {
                block.push_str(raw);
            } else {
                let gt = raw.find('>').unwrap_or(raw.len() - 1);
                let start = &raw[..gt + 1];
                let fixed = set_tag_attr(start, "localSheetId", local.map(|l| l.to_string()).as_deref());
                block.push_str(&fixed);
                block.push_str(&raw[gt + 1..]);
            }
            continue;
        }
        block.push_str(&format!("<definedName name=\"{}\"", esc_attr(&n.name)));
        if let Some(l) = local {
            block.push_str(&format!(" localSheetId=\"{l}\""));
        }
        block.push_str(&format!(">{}</definedName>", escape_text(&n.value)));
    }
    let new_block = if block.is_empty() {
        String::new()
    } else {
        format!("<definedNames>{block}</definedNames>")
    };
    let mut out = xml.to_string();
    if let Some((s, e)) = defined_names_span(xml) {
        out.replace_range(s..e, &new_block);
        return out;
    }
    if new_block.is_empty() {
        return out;
    }
    // Schema order: sheets, functionGroups?, externalReferences?, definedNames.
    let at = ["</externalReferences>", "</functionGroups>", "</sheets>"]
        .iter()
        .find_map(|t| out.find(t).map(|p| p + t.len()));
    if let Some(at) = at {
        out.insert_str(at, &new_block);
    }
    out
}

impl WorkbookOverlay {
    /// Lazily load the workbook-level model from the source workbook.xml.
    pub fn wb_level(&mut self) -> TurboResult<&mut WorkbookLevel> {
        if self.wb_level.is_none() {
            let xml = inflate_entry(&self.archive_map, "xl/workbook.xml")?
                .ok_or_else(|| TurboError::Format("xl/workbook.xml not found in archive".into()))?;
            let lvl = load_level(&String::from_utf8_lossy(&xml), &self.renamed_sheets);
            self.wb_level = Some(lvl);
        }
        Ok(self.wb_level.as_mut().expect("loaded above"))
    }

    /// Keep scoped names / states keyed by the new sheet name (rename hook).
    pub fn wb_level_on_rename(&mut self, old: &str, new: &str) {
        if let Some(l) = self.wb_level.as_mut() {
            for n in &mut l.names {
                if n.scope.as_deref() == Some(old) {
                    n.scope = Some(new.to_string());
                    // Print area / titles always point at their own sheet.
                    if n.name.eq_ignore_ascii_case(PRINT_AREA) || n.name.eq_ignore_ascii_case(PRINT_TITLES) {
                        n.value = qualify_refs(new, &n.value);
                        n.raw = None;
                        l.names_dirty = true;
                    }
                }
            }
            if let Some(st) = l.states.remove(old) {
                l.states.insert(new.to_string(), st);
            }
        }
    }

    /// Sheet-scoped name value (e.g. Print_Area) for `sheet`.
    pub fn local_name(&mut self, sheet: &str, name: &str) -> TurboResult<Option<String>> {
        let l = self.wb_level()?;
        Ok(l.names
            .iter()
            .find(|n| n.scope.as_deref() == Some(sheet) && n.name.eq_ignore_ascii_case(name))
            .map(|n| n.value.clone()))
    }

    /// Set / remove (None) a sheet-scoped name.
    pub fn set_local_name(&mut self, sheet: &str, name: &str, value: Option<String>) -> TurboResult<()> {
        let l = self.wb_level()?;
        let pos = l
            .names
            .iter()
            .position(|n| n.scope.as_deref() == Some(sheet) && n.name.eq_ignore_ascii_case(name));
        match (pos, value) {
            (Some(i), Some(v)) => {
                l.names[i].value = v;
                l.names[i].raw = None;
            }
            (Some(i), None) => {
                l.names.remove(i);
            }
            (None, Some(v)) => l.names.push(WbName {
                name: name.to_string(),
                scope: Some(sheet.to_string()),
                value: v,
                raw: None,
                raw_local: None,
            }),
            (None, None) => return Ok(()),
        }
        l.names_dirty = true;
        Ok(())
    }

    /// Global defined names (name, value) in document order.
    pub fn global_names(&mut self) -> TurboResult<Vec<(String, String)>> {
        Ok(self
            .wb_level()?
            .names
            .iter()
            .filter(|n| n.scope.is_none())
            .map(|n| (n.name.clone(), n.value.clone()))
            .collect())
    }

    pub fn set_global_name(&mut self, name: &str, value: Option<String>) -> TurboResult<bool> {
        let l = self.wb_level()?;
        let pos = l.names.iter().position(|n| n.scope.is_none() && n.name == name);
        match (pos, value) {
            (Some(i), Some(v)) => {
                l.names[i].value = v;
                l.names[i].raw = None;
            }
            (Some(i), None) => {
                l.names.remove(i);
            }
            (None, Some(v)) => l.names.push(WbName {
                name: name.to_string(),
                scope: None,
                value: v,
                raw: None,
                raw_local: None,
            }),
            (None, None) => return Ok(false),
        }
        l.names_dirty = true;
        Ok(true)
    }

    pub fn sheet_state(&mut self, sheet: &str) -> TurboResult<String> {
        Ok(self
            .wb_level()?
            .states
            .get(sheet)
            .cloned()
            .unwrap_or_else(|| "visible".to_string()))
    }

    /// Set a sheet's visibility. Refuses to hide the last visible sheet
    /// (Excel repairs a workbook with no visible sheet).
    pub fn set_sheet_state(&mut self, sheet: &str, state: &str) -> TurboResult<()> {
        let state = match state.to_ascii_lowercase().as_str() {
            "visible" => "visible",
            "hidden" => "hidden",
            "veryhidden" => "veryHidden",
            _ => {
                return Err(TurboError::Format(format!(
                    "sheet_state must be 'visible', 'hidden' or 'veryHidden', got '{state}'"
                )));
            }
        };
        let names = self.sheet_names();
        let l = self.wb_level()?;
        if state == "visible" {
            l.states.remove(sheet);
        } else {
            let others_visible = names
                .iter()
                .any(|n| n != sheet && !l.states.contains_key(n));
            if !others_visible {
                return Err(TurboError::Format(
                    "at least one sheet must stay visible".into(),
                ));
            }
            l.states.insert(sheet.to_string(), state.to_string());
        }
        l.states_dirty = true;
        Ok(())
    }

    /// Save hook: apply states / defined names to the outgoing workbook XML.
    /// `structure_rewritten` = the `<sheets>` block was regenerated (which
    /// drops `state` attributes), so states are re-applied from the model.
    pub fn apply_wb_level(
        &mut self,
        structure_rewritten: bool,
        wb: &mut Option<Vec<u8>>,
        wb_modified: &mut bool,
    ) -> TurboResult<()> {
        if structure_rewritten {
            self.wb_level()?;
        }
        let Some(l) = self.wb_level.as_ref() else { return Ok(()) };
        let do_states = l.states_dirty || (structure_rewritten && !l.states.is_empty());
        if !do_states && !l.names_dirty {
            return Ok(());
        }
        if wb.is_none() {
            *wb = inflate_entry(&self.archive_map, "xl/workbook.xml")?;
        }
        let Some(bytes) = wb.as_mut() else { return Ok(()) };
        let mut xml = String::from_utf8_lossy(bytes).into_owned();
        if do_states {
            xml = apply_states(&xml, &l.states);
        }
        if l.names_dirty {
            xml = apply_names(&xml, &l.names);
        }
        *bytes = xml.into_bytes();
        *wb_modified = true;
        Ok(())
    }

    /// Core document properties read from `docProps/core.xml`
    /// (tag -> decoded text). Read-only.
    pub fn core_properties(&self) -> TurboResult<Vec<(&'static str, Option<String>)>> {
        let xml = inflate_entry(&self.archive_map, "docProps/core.xml")?.unwrap_or_default();
        let xml = String::from_utf8_lossy(&xml);
        let get = |tag: &str| -> Option<String> {
            let s = xml.find(&format!("<{tag}"))?;
            let gt = s + xml[s..].find('>')?;
            if xml.as_bytes()[gt - 1] == b'/' {
                return Some(String::new());
            }
            let e = gt + xml[gt..].find(&format!("</{tag}>"))?;
            Some(decode(xml[gt + 1..e].as_bytes()))
        };
        Ok(vec![
            ("title", get("dc:title")),
            ("creator", get("dc:creator")),
            ("subject", get("dc:subject")),
            ("description", get("dc:description")),
            ("keywords", get("cp:keywords")),
            ("lastModifiedBy", get("cp:lastModifiedBy")),
            ("category", get("cp:category")),
            ("created", get("dcterms:created")),
            ("modified", get("dcterms:modified")),
        ])
    }
}

/// `$`-absolute form of an A1 ref / row span / column span: `A1:C3` ->
/// `$A$1:$C$3`, `1:2` -> `$1:$2`, `A:B` -> `$A:$B`.
pub fn abs_ref(r: &str) -> String {
    r.split(':')
        .map(|tok| {
            let t: String = tok.chars().filter(|c| *c != '$').collect();
            let split = t.find(|c: char| c.is_ascii_digit()).unwrap_or(t.len());
            let (letters, digits) = t.split_at(split);
            let mut o = String::new();
            if !letters.is_empty() {
                o.push('$');
                o.push_str(&letters.to_ascii_uppercase());
            }
            if !digits.is_empty() {
                o.push('$');
                o.push_str(digits);
            }
            o
        })
        .collect::<Vec<_>>()
        .join(":")
}

/// `'Sheet'!$A$1:$C$3[,'Sheet'!...]` for a comma-separated list of refs.
pub fn qualify_refs(sheet: &str, refs: &str) -> String {
    let q = format!("'{}'", sheet.replace('\'', "''"));
    refs.split(',')
        .map(|r| r.trim())
        .filter(|r| !r.is_empty())
        .map(|r| {
            let bare = r.rsplit_once('!').map(|(_, b)| b).unwrap_or(r);
            format!("{q}!{}", abs_ref(bare))
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Is an (unqualified, `$`-free) ref a whole-row span (`1:3`)?
fn is_row_span(r: &str) -> bool {
    !r.is_empty() && r.chars().all(|c| c.is_ascii_digit() || c == ':')
}

fn is_col_span(r: &str) -> bool {
    !r.is_empty() && r.chars().all(|c| c.is_ascii_alphabetic() || c == ':')
}

/// Split a Print_Titles value into (rows, cols), each unqualified and `$`-free
/// (`'S'!$A:$B,'S'!$1:$1` -> (Some("1:1"), Some("A:B"))).
pub fn split_print_titles(value: &str) -> (Option<String>, Option<String>) {
    let mut rows = None;
    let mut cols = None;
    for part in value.split(',') {
        let bare: String = part
            .rsplit_once('!')
            .map(|(_, b)| b)
            .unwrap_or(part)
            .chars()
            .filter(|c| *c != '$')
            .collect();
        if is_row_span(&bare) {
            rows = Some(bare);
        } else if is_col_span(&bare) {
            cols = Some(bare);
        }
    }
    (rows, cols)
}

impl WorkbookOverlay {
    pub fn print_titles(&mut self, sheet: &str) -> TurboResult<(Option<String>, Option<String>)> {
        Ok(self
            .local_name(sheet, PRINT_TITLES)?
            .map(|v| split_print_titles(&v))
            .unwrap_or((None, None)))
    }

    /// Update one half (rows when `rows`, else cols) of Print_Titles.
    pub fn set_print_titles(&mut self, sheet: &str, rows: bool, val: Option<&str>) -> TurboResult<()> {
        let (mut r, mut c) = self.print_titles(sheet)?;
        let v = val.map(|s| s.to_string()).filter(|s| !s.trim().is_empty());
        if let Some(s) = &v {
            let bare: String = s.chars().filter(|c| *c != '$').collect();
            let ok = if rows { is_row_span(&bare) } else { is_col_span(&bare) };
            if !ok {
                return Err(TurboError::Format(format!(
                    "invalid print title {} '{s}'",
                    if rows { "rows (expected like '1:2')" } else { "columns (expected like 'A:B')" }
                )));
            }
        }
        if rows {
            r = v;
        } else {
            c = v;
        }
        let parts: Vec<String> = [c, r].into_iter().flatten().collect();
        let value = if parts.is_empty() {
            None
        } else {
            Some(qualify_refs(sheet, &parts.join(",")))
        };
        self.set_local_name(sheet, PRINT_TITLES, value)
    }
}

impl WorkbookOverlay {
    pub fn print_area(&mut self, sheet: &str) -> TurboResult<Option<String>> {
        self.local_name(sheet, PRINT_AREA)
    }

    /// `refs` = comma-separated A1 ranges (optionally sheet-qualified); None clears.
    pub fn set_print_area(&mut self, sheet: &str, refs: Option<&str>) -> TurboResult<()> {
        let value = match refs.map(str::trim).filter(|s| !s.is_empty()) {
            None => None,
            Some(r) => {
                let ok = r.split(',').all(|p| {
                    let bare = p.trim().rsplit_once('!').map(|(_, b)| b).unwrap_or(p.trim());
                    !bare.is_empty() && bare.chars().all(|c| c.is_ascii_alphanumeric() || c == '$' || c == ':')
                });
                if !ok {
                    return Err(TurboError::Format(format!("invalid print area '{r}'")));
                }
                Some(qualify_refs(sheet, r))
            }
        };
        self.set_local_name(sheet, PRINT_AREA, value)
    }
}

/// PyO3 surface: `wb.defined_names`, `wb.properties` and helpers the
/// EditableSheet / EditableWorkbook bindings delegate to.
#[cfg(feature = "python")]
pub mod py {
    use super::*;
    use pyo3::exceptions::{PyKeyError, PyTypeError, PyValueError};
    use pyo3::prelude::*;
    use std::sync::{Arc, Mutex};

    pub type Shared = Arc<Mutex<WorkbookOverlay>>;

    pub fn to_py_err(e: TurboError) -> PyErr {
        PyValueError::new_err(e.to_string())
    }

    pub fn with<T>(ov: &Shared, f: impl FnOnce(&mut WorkbookOverlay) -> TurboResult<T>) -> PyResult<T> {
        let mut g = ov.lock().map_err(|e| PyValueError::new_err(e.to_string()))?;
        f(&mut g).map_err(to_py_err)
    }

    /// `str` or iterable of `str` -> comma-joined refs (None stays None).
    pub fn refs_arg(val: Option<&Bound<'_, PyAny>>) -> PyResult<Option<String>> {
        let Some(v) = val else { return Ok(None) };
        if v.is_none() {
            return Ok(None);
        }
        if let Ok(s) = v.extract::<String>() {
            return Ok(Some(s));
        }
        let mut parts = Vec::new();
        for item in v.try_iter()? {
            parts.push(item?.extract::<String>()?);
        }
        Ok(Some(parts.join(",")))
    }

    /// One defined name as seen from Python (openpyxl `DefinedName` shape).
    #[pyclass(name = "WorkbookDefinedName", skip_from_py_object)]
    #[derive(Clone)]
    pub struct PyWbDefinedName {
        #[pyo3(get)]
        pub name: String,
        #[pyo3(get)]
        pub value: String,
        #[pyo3(get, name = "localSheetId")]
        pub local_sheet_id: Option<u32>,
    }

    #[pymethods]
    impl PyWbDefinedName {
        #[new]
        #[pyo3(signature = (name, attr_text = None, value = None))]
        fn new(name: String, attr_text: Option<String>, value: Option<String>) -> Self {
            Self { name, value: attr_text.or(value).unwrap_or_default(), local_sheet_id: None }
        }
        #[getter]
        fn attr_text(&self) -> String {
            self.value.clone()
        }
        fn __repr__(&self) -> String {
            format!("<DefinedName name={:?} attr_text={:?}>", self.name, self.value)
        }
    }

    /// Dict-like view over the workbook's GLOBAL defined names.
    #[pyclass(name = "DefinedNameDict")]
    pub struct PyDefinedNameDict {
        pub overlay: Shared,
    }

    fn value_arg(v: &Bound<'_, PyAny>) -> PyResult<String> {
        if let Ok(s) = v.extract::<String>() {
            return Ok(s);
        }
        for attr in ["attr_text", "value", "formula"] {
            if let Ok(a) = v.getattr(attr) {
                if let Ok(s) = a.extract::<String>() {
                    return Ok(s);
                }
            }
        }
        Err(PyTypeError::new_err("defined name value must be a str or an object with .attr_text / .value"))
    }

    impl PyDefinedNameDict {
        fn lookup(&self, name: &str) -> PyResult<Option<PyWbDefinedName>> {
            let names = with(&self.overlay, |ov| ov.global_names())?;
            Ok(names
                .iter()
                .find(|(n, _)| n == name)
                .or_else(|| names.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)))
                .map(|(n, v)| PyWbDefinedName { name: n.clone(), value: v.clone(), local_sheet_id: None }))
        }
    }

    #[pymethods]
    impl PyDefinedNameDict {
        fn __getitem__(&self, name: &str) -> PyResult<PyWbDefinedName> {
            self.lookup(name)?.ok_or_else(|| PyKeyError::new_err(name.to_string()))
        }
        fn __setitem__(&self, name: &str, value: &Bound<'_, PyAny>) -> PyResult<()> {
            let v = value_arg(value)?;
            if name.trim().is_empty() {
                return Err(PyValueError::new_err("defined name must not be empty"));
            }
            with(&self.overlay, |ov| ov.set_global_name(name, Some(v))).map(|_| ())
        }
        fn __delitem__(&self, name: &str) -> PyResult<()> {
            let real = self.lookup(name)?.ok_or_else(|| PyKeyError::new_err(name.to_string()))?;
            with(&self.overlay, |ov| ov.set_global_name(&real.name, None)).map(|_| ())
        }
        fn __contains__(&self, name: &str) -> PyResult<bool> {
            Ok(self.lookup(name)?.is_some())
        }
        fn __len__(&self) -> PyResult<usize> {
            Ok(with(&self.overlay, |ov| ov.global_names())?.len())
        }
        fn __iter__<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
            let keys = self.keys()?;
            Ok(pyo3::types::PyList::new(py, keys)?.try_iter()?.into_any())
        }
        fn keys(&self) -> PyResult<Vec<String>> {
            Ok(with(&self.overlay, |ov| ov.global_names())?.into_iter().map(|(n, _)| n).collect())
        }
        fn values(&self) -> PyResult<Vec<PyWbDefinedName>> {
            Ok(with(&self.overlay, |ov| ov.global_names())?
                .into_iter()
                .map(|(name, value)| PyWbDefinedName { name, value, local_sheet_id: None })
                .collect())
        }
        fn items(&self) -> PyResult<Vec<(String, PyWbDefinedName)>> {
            Ok(self.values()?.into_iter().map(|d| (d.name.clone(), d)).collect())
        }
        #[pyo3(signature = (name, default = None))]
        fn get<'py>(&self, py: Python<'py>, name: &str, default: Option<Bound<'py, PyAny>>) -> PyResult<Bound<'py, PyAny>> {
            match self.lookup(name)? {
                Some(d) => Ok(Bound::new(py, d)?.into_any()),
                None => Ok(default.unwrap_or_else(|| py.None().into_bound(py))),
            }
        }
        /// openpyxl 3.1 `wb.defined_names.add(DefinedName(...))`.
        fn add(&self, defn: &Bound<'_, PyAny>) -> PyResult<()> {
            let name: String = defn.getattr("name")?.extract()?;
            self.__setitem__(&name, defn)
        }
        fn __repr__(&self) -> PyResult<String> {
            Ok(format!("<DefinedNameDict {:?}>", self.keys()?))
        }
    }

    /// Read-only core document properties (`docProps/core.xml`).
    #[pyclass(name = "DocumentProperties")]
    pub struct PyDocumentProperties {
        #[pyo3(get)]
        title: Option<String>,
        #[pyo3(get)]
        creator: Option<String>,
        #[pyo3(get)]
        subject: Option<String>,
        #[pyo3(get)]
        description: Option<String>,
        #[pyo3(get)]
        keywords: Option<String>,
        #[pyo3(get, name = "lastModifiedBy")]
        last_modified_by: Option<String>,
        #[pyo3(get)]
        category: Option<String>,
        #[pyo3(get)]
        created: Option<Py<PyAny>>,
        #[pyo3(get)]
        modified: Option<Py<PyAny>>,
    }

    /// W3CDTF -> naive `datetime` (UTC), falling back to the raw string.
    fn w3c_datetime(py: Python<'_>, s: Option<String>) -> PyResult<Option<Py<PyAny>>> {
        let Some(s) = s else { return Ok(None) };
        let trimmed = s.trim().trim_end_matches('Z');
        let dt = py.import("datetime")?.getattr("datetime")?;
        Ok(Some(match dt.call_method1("fromisoformat", (trimmed,)) {
            Ok(v) => v.unbind(),
            Err(_) => s.into_pyobject(py)?.into_any().unbind(),
        }))
    }

    pub fn document_properties(py: Python<'_>, ov: &Shared) -> PyResult<PyDocumentProperties> {
        let props = with(ov, |ov| ov.core_properties())?;
        let mut m: AHashMap<&str, Option<String>> = props.into_iter().collect();
        let mut take = |k: &str| m.remove(k).flatten();
        Ok(PyDocumentProperties {
            title: take("title"),
            creator: take("creator"),
            subject: take("subject"),
            description: take("description"),
            keywords: take("keywords"),
            last_modified_by: take("lastModifiedBy"),
            category: take("category"),
            created: w3c_datetime(py, take("created"))?,
            modified: w3c_datetime(py, take("modified"))?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WB: &str = r#"<workbook><bookViews><workbookView activeTab="0"/></bookViews><sheets><sheet name="A" sheetId="1" r:id="rId1"/><sheet name="B&amp;C" sheetId="2" state="hidden" r:id="rId2"/></sheets><definedNames><definedName name="X" comment="keep">A!$A$1</definedName><definedName name="_xlnm.Print_Area" localSheetId="1">'B&amp;C'!$A$1:$B$2</definedName></definedNames><calcPr/></workbook>"#;

    #[test]
    fn load_and_roundtrip_untouched() {
        let l = load_level(WB, &[]);
        assert_eq!(l.states.get("B&C").map(|s| s.as_str()), Some("hidden"));
        assert_eq!(l.names.len(), 2);
        assert_eq!(l.names[1].scope.as_deref(), Some("B&C"));
        assert_eq!(l.names[1].value, "'B&C'!$A$1:$B$2");
        assert_eq!(apply_names(WB, &l.names), WB);
        assert_eq!(apply_states(WB, &l.states), WB);
    }

    #[test]
    fn add_names_and_states() {
        let mut l = load_level(WB, &[]);
        l.names.push(WbName {
            name: "_xlnm.Print_Area".into(),
            scope: Some("A".into()),
            value: qualify_refs("A", "a1:c3"),
            raw: None,
            raw_local: None,
        });
        let out = apply_names(WB, &l.names);
        assert!(out.contains(r#"<definedName name="_xlnm.Print_Area" localSheetId="0">'A'!$A$1:$C$3</definedName>"#));
        l.states.remove("B&C");
        l.states.insert("A".into(), "veryHidden".into());
        let out = apply_states(&out, &l.states);
        assert!(out.contains(r#"<sheet name="A" sheetId="1" r:id="rId1" state="veryHidden"/>"#));
        assert!(out.contains(r#"<sheet name="B&amp;C" sheetId="2" r:id="rId2"/>"#));
        assert!(out.contains(r#"activeTab="1""#));
        let none = apply_names("<workbook><sheets/></workbook>", &[]);
        assert_eq!(none, "<workbook><sheets/></workbook>");
        let ins = apply_names(
            "<workbook><sheets><sheet name=\"S\"/></sheets><calcPr/></workbook>",
            &[WbName { name: "N".into(), scope: None, value: "S!$A$1".into(), raw: None, raw_local: None }],
        );
        assert!(ins.contains("</sheets><definedNames><definedName name=\"N\">S!$A$1</definedName></definedNames><calcPr/>"));
    }

    #[test]
    fn print_titles_helpers() {
        assert_eq!(abs_ref("A1:c3"), "$A$1:$C$3");
        assert_eq!(abs_ref("1:2"), "$1:$2");
        assert_eq!(abs_ref("A:B"), "$A:$B");
        assert_eq!(
            split_print_titles("'S'!$A:$B,'S'!$1:$1"),
            (Some("1:1".into()), Some("A:B".into()))
        );
    }
}
