//! Read-back fidelity for the editable (openpyxl-style) doorway.
//!
//! - number-format driven typing: a numeric cell whose format is a date/time
//!   format reads back as a date/time (openpyxl semantics), and writing a
//!   date-like value onto a cell with no explicit format applies openpyxl's
//!   default display format;
//! - the workbook epoch (`date1904`) for both directions;
//! - comments and hyperlinks already stored in the source file.
//!
//! Everything here is parsed lazily from the (immutable) source archive and
//! cached on the overlay, so a getter costs one parse per sheet per session.
// Most entry points are only reached from the Python binding.
#![cfg_attr(not(feature = "python"), allow(dead_code))]

use std::sync::Arc;

use ahash::AHashMap;

use super::model::{CellValue, Sheet};
use super::style_engine::StyleDesc;
use crate::turbo::error::TurboResult;
use crate::turbo::overlay::{WorkbookOverlay, inflate_entry};
use crate::turbo::structural::{self, LinkTarget, RelKind};
use crate::turbo::styles::{StyleTable, is_date_format, is_timedelta_format};
use crate::turbo::zipmin::ArchiveMap;

/// Days between the 1900 (1899-12-30) and 1904 (1904-01-01) epochs.
const EPOCH_1904_SHIFT: f64 = 1462.0;
/// Cap on cells one `<hyperlink ref="A1:Z9">` range expands to.
const MAX_LINK_CELLS: u64 = 16_384;

/// How a numeric cell value reads back, decided by its number format.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NumKind {
    Plain,
    Date,
    Timedelta,
}

pub fn classify_format(fmt: &str) -> NumKind {
    if is_timedelta_format(fmt) {
        NumKind::Timedelta
    } else if is_date_format(fmt) {
        NumKind::Date
    } else {
        NumKind::Plain
    }
}

/// A hyperlink stored in the source file (`target` is the external URL/path).
#[derive(Clone, Debug, Default)]
pub struct ExistingLink {
    pub target: Option<String>,
    pub location: Option<String>,
    pub display: Option<String>,
    pub tooltip: Option<String>,
}

/// Comments and hyperlinks of one source sheet, keyed by 1-based (row, col).
#[derive(Debug, Default)]
pub struct SheetExtras {
    pub comments: AHashMap<(u32, u32), (String, String)>,
    pub hyperlinks: AHashMap<(u32, u32), ExistingLink>,
}

/// Lazily parsed source-file facts, cached for the overlay's lifetime.
#[derive(Default)]
pub struct ReadbackCache {
    styles: Option<Option<Arc<StyleTable>>>,
    date1904: Option<bool>,
    /// Keyed by sheet part path (stable across renames).
    sheets: AHashMap<String, Arc<SheetExtras>>,
}

/// The workbook's `date1904` flag (false for a blank or unreadable workbook).
pub fn date1904(ov: &mut WorkbookOverlay) -> bool {
    if let Some(v) = ov.readback.date1904 {
        return v;
    }
    let v = inflate_entry(&ov.archive_map, "xl/workbook.xml")
        .ok()
        .flatten()
        .map(|x| crate::turbo::meta::parse_workbook_pr(&x).0)
        .unwrap_or(false);
    ov.readback.date1904 = Some(v);
    v
}

fn style_table(ov: &mut WorkbookOverlay) -> Option<Arc<StyleTable>> {
    if let Some(st) = &ov.readback.styles {
        return st.clone();
    }
    let st = inflate_entry(&ov.archive_map, "xl/styles.xml")
        .ok()
        .flatten()
        .map(|x| Arc::new(crate::turbo::styles::parse_style_table(&x)));
    ov.readback.styles = Some(st.clone());
    st
}

/// `s=` style index of the source cell at (row, col), if any.
pub fn source_style(sheet: Option<&Sheet>, row: u32, col: u32) -> Option<u32> {
    let sheet = sheet?;
    let r = &sheet.rows[sheet.rows.binary_search_by(|r| r.row.cmp(&row)).ok()?];
    r.cells[r.cells.binary_search_by(|c| c.col.cmp(&col)).ok()?].style
}

/// Number-format class of (row, col): an in-session style wins (it replaces
/// the cell's xf on save), else the source cell's xf.
pub fn num_kind(
    ov: &mut WorkbookOverlay,
    sheet: &str,
    row: u32,
    col: u32,
    src_style: Option<u32>,
) -> NumKind {
    if let Some(desc) = ov
        .sheet_overlays
        .get(sheet)
        .and_then(|so| so.modified_styles.get(&(row, col)))
    {
        return desc.num_fmt.as_deref().map_or(NumKind::Plain, classify_format);
    }
    let Some(s) = src_style else {
        return NumKind::Plain;
    };
    let Some(st) = style_table(ov) else {
        return NumKind::Plain;
    };
    if st.xf_is_timedelta.get(s as usize).copied().unwrap_or(false) {
        NumKind::Timedelta
    } else if st.is_date(s) {
        NumKind::Date
    } else {
        NumKind::Plain
    }
}

/// Shift a 1900-system date serial into the workbook's epoch.
pub fn rebase_epoch(v: CellValue, date1904: bool) -> CellValue {
    match v {
        CellValue::DateSerial(n) if date1904 => CellValue::DateSerial(n - EPOCH_1904_SHIFT),
        other => other,
    }
}

/// openpyxl's write-time rule: a date-like value written to a cell without an
/// explicit number format gets `fmt`. An explicit format is never overridden;
/// a source cell's font/fill/border are kept.
pub fn apply_default_format(ov: &mut WorkbookOverlay, sheet: &str, row: u32, col: u32, fmt: &str) {
    if let Some(desc) = ov
        .sheet_overlays
        .get_mut(sheet)
        .and_then(|so| so.modified_styles.get_mut(&(row, col)))
    {
        if desc.num_fmt.is_none() {
            desc.num_fmt = Some(fmt.to_string());
        }
        return;
    }
    let hydrated = ov.hydrated_sheet(sheet).ok().flatten();
    let mut desc = StyleDesc::default();
    if let Some(s) = source_style(hydrated.as_deref(), row, col) {
        if let Some(st) = style_table(ov) {
            if st.numfmt_code(s) != "General" {
                return;
            }
        }
        let _ = ov.ensure_base_styles(sheet);
        if let Some(base) = ov.base_styles.get(sheet).and_then(|m| m.get(&(row, col))) {
            desc = base.clone();
        }
    }
    desc.num_fmt = Some(fmt.to_string());
    ov.set_cell_style(sheet, row, col, desc);
}

/// Comments/hyperlinks stored in the source file for `sheet` (None for a
/// sheet created in this session).
pub fn sheet_extras(ov: &mut WorkbookOverlay, sheet: &str) -> Option<Arc<SheetExtras>> {
    let target = ov.archive_map.sheet_name_map.get(sheet)?.clone();
    if let Some(x) = ov.readback.sheets.get(&target) {
        return Some(Arc::clone(x));
    }
    let x = Arc::new(load_extras(&ov.archive_map, &target).unwrap_or_default());
    ov.readback.sheets.insert(target, Arc::clone(&x));
    Some(x)
}

fn load_extras(map: &ArchiveMap, target: &str) -> TurboResult<SheetExtras> {
    let mut out = SheetExtras::default();
    let (dir, file) = target.rsplit_once('/').unwrap_or(("", target));
    let rels = match inflate_entry(map, &format!("{dir}/_rels/{file}.rels"))? {
        Some(rx) => structural::parse_rels(&rx),
        None => Default::default(),
    };
    let base_dir = format!("{dir}/");
    if let Some(rel) = rels.values().find(|r| r.kind == RelKind::Comments) {
        let path = structural::resolve_zip_path(&base_dir, &rel.target);
        if let Some(cx) = inflate_entry(map, &path)? {
            let sc = structural::parse_comments(&cx);
            for c in sc.comments {
                let author = sc
                    .authors
                    .get(c.author_id as usize)
                    .cloned()
                    .unwrap_or_default();
                out.comments.insert((c.row + 1, c.col + 1), (c.text, author));
            }
        }
    }
    if let Some(xml) = inflate_entry(map, target)? {
        let tail = memchr::memmem::find(&xml, b"</sheetData>").map_or(&xml[..], |p| &xml[p..]);
        for h in structural::scan_hyperlinks(tail, &rels) {
            let link = ExistingLink {
                target: match h.target {
                    LinkTarget::External(t) => Some(t),
                    LinkTarget::Internal => None,
                },
                location: h.location,
                display: h.display,
                tooltip: h.tooltip,
            };
            // openpyxl binds a range link to every cell of the range.
            let r = h.ref_;
            let mut n = 0u64;
            'cells: for row in r.r0..=r.r1.max(r.r0) {
                for col in r.c0..=r.c1.max(r.c0) {
                    n += 1;
                    if n > MAX_LINK_CELLS {
                        break 'cells;
                    }
                    out.hyperlinks.insert((row + 1, col + 1), link.clone());
                }
            }
        }
    }
    Ok(out)
}

/// Merge the engine's custom number formats (`codes[i]` has engine id
/// `164 + i`) into an existing styles.xml. Codes already present reuse the
/// source id; new ones get fresh ids above the source's highest. Returns the
/// patched XML and `map[i]` = final id for engine id `164 + i`. On XML it can't
/// patch, returns the input unchanged with an identity map.
pub fn splice_num_fmts(xml: &[u8], codes: &[String]) -> (Vec<u8>, Vec<i32>) {
    use crate::turbo::overlay::extract_xml_attr;
    let identity = || (0..codes.len() as i32).map(|i| i + 164).collect::<Vec<_>>();
    if codes.is_empty() {
        return (xml.to_vec(), Vec::new());
    }
    // Existing custom formats: decoded code -> id.
    let mut existing: AHashMap<String, i32> = AHashMap::new();
    let mut max_id = 163;
    let mut count = 0usize;
    let mut pos = 0;
    while let Some(off) = memchr::memmem::find(&xml[pos..], b"<numFmt ") {
        let start = pos + off;
        let Some(gt) = memchr::memchr(b'>', &xml[start..]) else { break };
        let tag = &xml[start..start + gt + 1];
        if let (Some(id), Some(code)) = (
            extract_xml_attr(tag, b"numFmtId").and_then(|s| s.parse::<i32>().ok()),
            extract_xml_attr(tag, b"formatCode"),
        ) {
            let mut scratch = Vec::new();
            let dec = crate::turbo::decode::decode_bytes(code.as_bytes(), &mut scratch);
            existing.entry(String::from_utf8_lossy(dec).into_owned()).or_insert(id);
            max_id = max_id.max(id);
            count += 1;
        }
        pos = start + gt + 1;
    }
    let mut map = Vec::with_capacity(codes.len());
    let mut added = Vec::new();
    let mut n_added = 0usize;
    for code in codes {
        if let Some(&id) = existing.get(code.as_str()) {
            map.push(id);
        } else {
            max_id += 1;
            n_added += 1;
            existing.insert(code.clone(), max_id);
            map.push(max_id);
            added.extend_from_slice(b"<numFmt numFmtId=\"");
            added.extend_from_slice(max_id.to_string().as_bytes());
            added.extend_from_slice(b"\" formatCode=\"");
            for ch in code.chars() {
                match ch {
                    '&' => added.extend_from_slice(b"&amp;"),
                    '<' => added.extend_from_slice(b"&lt;"),
                    '>' => added.extend_from_slice(b"&gt;"),
                    '"' => added.extend_from_slice(b"&quot;"),
                    c => {
                        let mut b = [0u8; 4];
                        added.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
                    }
                }
            }
            added.extend_from_slice(b"\"/>");
        }
    }
    if added.is_empty() {
        return (xml.to_vec(), map);
    }
    let new_count = count + n_added;
    let open = |n: usize| format!("<numFmts count=\"{n}\">").into_bytes();
    let mut out = Vec::with_capacity(xml.len() + added.len() + 32);
    if let Some(s) = memchr::memmem::find(xml, b"<numFmts") {
        let Some(gt) = memchr::memchr(b'>', &xml[s..]).map(|g| s + g) else {
            return (xml.to_vec(), identity());
        };
        out.extend_from_slice(&xml[..s]);
        out.extend_from_slice(&open(new_count));
        if xml[gt - 1] == b'/' {
            out.extend_from_slice(&added);
            out.extend_from_slice(b"</numFmts>");
            out.extend_from_slice(&xml[gt + 1..]);
        } else {
            let Some(c) = memchr::memmem::find(&xml[gt..], b"</numFmts>").map(|c| gt + c) else {
                return (xml.to_vec(), identity());
            };
            out.extend_from_slice(&xml[gt + 1..c]);
            out.extend_from_slice(&added);
            out.extend_from_slice(&xml[c..]);
        }
    } else {
        let Some(s) = memchr::memmem::find(xml, b"<styleSheet") else {
            return (xml.to_vec(), identity());
        };
        let Some(gt) = memchr::memchr(b'>', &xml[s..]).map(|g| s + g) else {
            return (xml.to_vec(), identity());
        };
        out.extend_from_slice(&xml[..=gt]);
        out.extend_from_slice(&open(new_count));
        out.extend_from_slice(&added);
        out.extend_from_slice(b"</numFmts>");
        out.extend_from_slice(&xml[gt + 1..]);
    }
    (out, map)
}

#[cfg(feature = "python")]
mod py {
    use super::NumKind;
    use pyo3::prelude::*;

    /// openpyxl's `from_excel`: a serial in a date-formatted cell becomes a
    /// `datetime` (`time` for 0 <= serial < 1), in a `[h]`-style cell a
    /// `timedelta`. Returns None (caller keeps the float) when Python cannot
    /// represent it.
    pub fn serial_to_py<'py>(
        py: Python<'py>,
        serial: f64,
        kind: NumKind,
        date1904: bool,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        if kind == NumKind::Plain || !serial.is_finite() {
            return Ok(None);
        }
        let dt = py.import("datetime")?;
        let timedelta = dt.getattr("timedelta")?;
        let made = if kind == NumKind::Timedelta {
            let ms = (serial * 86_400_000.0).round();
            timedelta.call1((0, 0, 0, ms))
        } else {
            let mut day = serial.floor();
            let ms = ((serial - day) * 86_400_000.0).round() as i64;
            if (0.0..1.0).contains(&serial) && ms < 86_400_000 {
                let us = ms * 1000;
                let secs = us / 1_000_000;
                return Ok(Some(dt.getattr("time")?.call1((
                    secs / 3600,
                    (secs / 60) % 60,
                    secs % 60,
                    us % 1_000_000,
                ))?));
            }
            // Excel's phantom 1900-02-29: serials below 60 sit one day later.
            if !date1904 && serial > 0.0 && serial < 60.0 {
                day += 1.0;
            }
            let epoch = if date1904 {
                dt.getattr("datetime")?.call1((1904, 1, 1))?
            } else {
                dt.getattr("datetime")?.call1((1899, 12, 30))?
            };
            timedelta
                .call1((day as i64, 0, 0, ms))
                .and_then(|d| epoch.add(d))
        };
        Ok(made.ok())
    }
}

#[cfg(feature = "python")]
pub use py::serial_to_py;
#[cfg(feature = "python")]
pub use py_link::PyHyperlink;

#[cfg(feature = "python")]
mod py_link {
    use super::ExistingLink;
    use pyo3::prelude::*;

    /// `cell.hyperlink` (openpyxl shape). `str()` is the target and it
    /// compares equal to that string, so `cell.hyperlink == "https://..."`
    /// keeps working.
    #[pyclass(name = "Hyperlink", module = "kyrax")]
    #[derive(Clone, Debug, Default)]
    pub struct PyHyperlink {
        #[pyo3(get, name = "ref")]
        pub ref_: String,
        #[pyo3(get)]
        pub target: Option<String>,
        #[pyo3(get)]
        pub location: Option<String>,
        #[pyo3(get)]
        pub display: Option<String>,
        #[pyo3(get)]
        pub tooltip: Option<String>,
    }

    impl PyHyperlink {
        pub fn from_target(ref_: String, target: String) -> Self {
            Self { ref_, target: Some(target), ..Default::default() }
        }

        pub fn from_existing(ref_: String, l: &ExistingLink) -> Self {
            Self {
                ref_,
                target: l.target.clone(),
                location: l.location.clone(),
                display: l.display.clone(),
                tooltip: l.tooltip.clone(),
            }
        }

        /// The link as one string: external target, else in-workbook location.
        pub fn text(&self) -> String {
            self.target
                .clone()
                .or_else(|| self.location.clone())
                .unwrap_or_default()
        }
    }

    #[pymethods]
    impl PyHyperlink {
        fn __str__(&self) -> String {
            self.text()
        }

        fn __repr__(&self) -> String {
            let opt = |v: &Option<String>| v.as_ref().map_or("None".to_string(), |s| format!("{s:?}"));
            format!(
                "Hyperlink(ref={:?}, target={}, location={})",
                self.ref_,
                opt(&self.target),
                opt(&self.location)
            )
        }

        fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
            if let Ok(s) = other.extract::<String>() {
                return s == self.text();
            }
            other
                .cast::<PyHyperlink>()
                .map(|h| {
                    let h = h.borrow();
                    h.target == self.target && h.location == self.location
                })
                .unwrap_or(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_formats() {
        assert_eq!(classify_format("General"), NumKind::Plain);
        assert_eq!(classify_format("0.00"), NumKind::Plain);
        assert_eq!(classify_format("yyyy-mm-dd"), NumKind::Date);
        assert_eq!(classify_format("yyyy-mm-dd h:mm:ss"), NumKind::Date);
        assert_eq!(classify_format("h:mm:ss"), NumKind::Date);
        assert_eq!(classify_format("[hh]:mm:ss"), NumKind::Timedelta);
    }

    #[test]
    fn splice_num_fmts_reuses_and_appends() {
        let src = br#"<styleSheet xmlns="x"><numFmts count="1"><numFmt numFmtId="164" formatCode="0.000"/></numFmts><fonts/></styleSheet>"#;
        let codes = vec!["yyyy-mm-dd".to_string(), "0.000".to_string()];
        let (out, map) = splice_num_fmts(src, &codes);
        let s = String::from_utf8(out).unwrap();
        assert_eq!(map, vec![165, 164]);
        assert!(s.contains(r#"<numFmts count="2">"#), "{s}");
        assert!(s.contains(r#"<numFmt numFmtId="165" formatCode="yyyy-mm-dd"/></numFmts>"#));
        // Missing / empty containers are created in schema position.
        let (out, map) = splice_num_fmts(br#"<styleSheet a="1"><fonts/></styleSheet>"#, &codes[..1]);
        assert_eq!(map, vec![164]);
        assert!(String::from_utf8(out).unwrap().starts_with(
            r#"<styleSheet a="1"><numFmts count="1"><numFmt numFmtId="164" formatCode="yyyy-mm-dd"/></numFmts><fonts/>"#
        ));
        let (out, _) = splice_num_fmts(br#"<styleSheet><numFmts count="0"/><fonts/></styleSheet>"#, &codes[..1]);
        assert!(String::from_utf8(out).unwrap().contains(r#"<numFmts count="1"><numFmt numFmtId="164""#));
    }

    #[test]
    fn rebase_only_dates_in_1904() {
        assert!(matches!(
            rebase_epoch(CellValue::DateSerial(45293.0), true),
            CellValue::DateSerial(n) if n == 45293.0 - 1462.0
        ));
        assert!(matches!(
            rebase_epoch(CellValue::DateSerial(45293.0), false),
            CellValue::DateSerial(n) if n == 45293.0
        ));
        assert!(matches!(rebase_epoch(CellValue::Time(0.5), true), CellValue::Time(n) if n == 0.5));
    }

    #[test]
    fn existing_comments_and_links_read_back() {
        let mut wb = crate::turbo::write::model::Workbook::new();
        let mut sheet = Sheet::new("S");
        sheet.comments.push(crate::turbo::write::model::Comment {
            ref_: "A2".into(),
            text: "hi".into(),
            author: "me".into(),
            ..Default::default()
        });
        sheet.hyperlinks.push(crate::turbo::write::model::Hyperlink {
            ref_: "C3".into(),
            target: Some("https://x.example".into()),
            location: None,
            display: None,
        });
        wb.sheets.push(sheet);
        let bytes = crate::turbo::write::writer::write_workbook_bytes(&wb).unwrap();
        let mut ov = WorkbookOverlay::new(ArchiveMap::parse(Arc::new(bytes)).unwrap());
        let x = sheet_extras(&mut ov, "S").unwrap();
        assert_eq!(x.comments.get(&(2, 1)).map(|c| c.0.as_str()), Some("hi"));
        assert_eq!(x.comments.get(&(2, 1)).map(|c| c.1.as_str()), Some("me"));
        assert!(x.hyperlinks.get(&(1, 1)).is_none());
        let l = x.hyperlinks.get(&(3, 3)).unwrap();
        assert_eq!(l.target.as_deref(), Some("https://x.example"));
        assert!(!date1904(&mut ov));
    }
}
