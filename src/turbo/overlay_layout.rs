//! Sheet layout edits for the editable overlay: column widths / hidden /
//! outline level (`<cols>`), row heights / hidden (`<row ht hidden>`), plus
//! file read-back helpers for merges, freeze panes and autoFilter.
//!
//! Overrides live in [`SheetLayout`] on the `SheetOverlay` and are applied to
//! the final sheet XML at save by [`apply_layout`] (both the splice path and
//! the brand-new-sheet fallback path).

use std::collections::BTreeMap;

use crate::turbo::error::TurboResult;
use crate::turbo::overlay::{WorkbookOverlay, find_element, inflate_entry, splice_tail_control};
use crate::turbo::structural::{CellRange, range_a1};
use crate::turbo::write::xml::write_f64;

/// One column-range override (`min` is the map key, `max` inclusive).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ColLayout {
    pub max: u32,
    pub width: Option<f64>,
    pub hidden: bool,
    pub outline_level: u8,
    pub style: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RowLayout {
    pub height: Option<f64>,
    pub hidden: bool,
}

#[derive(Debug, Clone, Default)]
pub struct SheetLayout {
    /// Non-overlapping column ranges keyed by 1-based `min`.
    pub cols: BTreeMap<u32, ColLayout>,
    /// 1-based row -> layout.
    pub rows: BTreeMap<u32, RowLayout>,
}

impl SheetLayout {
    pub fn is_empty(&self) -> bool {
        self.cols.is_empty() && self.rows.is_empty()
    }

    /// Insert `l` over `[min, l.max]`, trimming any overlapping ranges.
    pub fn insert_cols(&mut self, min: u32, l: ColLayout) {
        let max = l.max;
        let overlapping: Vec<(u32, ColLayout)> = self
            .cols
            .range(..=max)
            .filter(|(_, e)| e.max >= min)
            .map(|(k, e)| (*k, *e))
            .collect();
        for (k, e) in overlapping {
            self.cols.remove(&k);
            if k < min {
                self.cols.insert(k, ColLayout { max: min - 1, ..e });
            }
            if e.max > max {
                self.cols.insert(max + 1, ColLayout { max: e.max, ..e });
            }
        }
        self.cols.insert(min, l);
    }

    fn col(&self, col: u32) -> Option<ColLayout> {
        self.cols
            .range(..=col)
            .next_back()
            .filter(|(_, l)| l.max >= col)
            .map(|(_, l)| *l)
    }
}

// ---------------------------------------------------------------------------
// Tiny tag attribute model (values kept raw / already escaped).
// ---------------------------------------------------------------------------

type Attrs = Vec<(String, String)>;

fn parse_tag(tag: &str) -> (String, Attrs, bool) {
    let t = tag.trim();
    let self_closing = t.ends_with("/>");
    let body = t.trim_start_matches('<').trim_end_matches('>');
    let body = body.strip_suffix('/').unwrap_or(body);
    let name_end = body.find(|c: char| c.is_whitespace()).unwrap_or(body.len());
    let name = body[..name_end].to_string();
    let mut attrs = Vec::new();
    let mut rest = &body[name_end..];
    loop {
        rest = rest.trim_start();
        let Some(eq) = rest.find('=') else { break };
        let key = rest[..eq].trim().to_string();
        let after = rest[eq + 1..].trim_start();
        let Some(q) = after.chars().next() else { break };
        if q != '"' && q != '\'' {
            break;
        }
        let Some(end) = after[1..].find(q) else { break };
        attrs.push((key, after[1..1 + end].to_string()));
        rest = &after[end + 2..];
    }
    (name, attrs, self_closing)
}

fn get_attr<'a>(attrs: &'a Attrs, key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn set_attr(attrs: &mut Attrs, key: &str, val: Option<String>) {
    match (attrs.iter().position(|(k, _)| k == key), val) {
        (Some(i), Some(v)) => attrs[i].1 = v,
        (Some(i), None) => {
            attrs.remove(i);
        }
        (None, Some(v)) => attrs.push((key.to_string(), v)),
        (None, None) => {}
    }
}

fn emit_tag(out: &mut Vec<u8>, name: &str, attrs: &Attrs, self_closing: bool) {
    out.push(b'<');
    out.extend_from_slice(name.as_bytes());
    for (k, v) in attrs {
        out.push(b' ');
        out.extend_from_slice(k.as_bytes());
        out.extend_from_slice(b"=\"");
        out.extend_from_slice(v.as_bytes());
        out.push(b'"');
    }
    out.extend_from_slice(if self_closing { b"/>" } else { b">" });
}

fn fmt_f64(v: f64) -> String {
    let mut b = Vec::new();
    write_f64(&mut b, v);
    String::from_utf8(b).unwrap_or_default()
}

fn is_true(v: Option<&str>) -> bool {
    matches!(v, Some("1") | Some("true"))
}

// ---------------------------------------------------------------------------
// Scanning existing sheet XML.
// ---------------------------------------------------------------------------

struct FileCol {
    min: u32,
    max: u32,
    attrs: Attrs,
}

fn scan_cols(xml: &[u8]) -> Vec<FileCol> {
    let header_end = find_element(xml, b"sheetData", 0).unwrap_or(xml.len());
    let header = &xml[..header_end];
    let Some(cs) = find_element(header, b"cols", 0) else {
        return Vec::new();
    };
    let end = memchr::memmem::find(&header[cs..], b"</cols>").map_or(header.len(), |p| cs + p);
    let region = &header[..end];
    let mut out = Vec::new();
    let mut pos = cs + 5;
    while let Some(p) = find_element(region, b"col", pos) {
        let Some(gt) = memchr::memchr(b'>', &region[p..]).map(|g| p + g) else {
            break;
        };
        let (_, mut attrs, _) = parse_tag(&String::from_utf8_lossy(&region[p..=gt]));
        let min = get_attr(&attrs, "min")
            .and_then(|v| v.parse().ok())
            .unwrap_or(1);
        let max = get_attr(&attrs, "max")
            .and_then(|v| v.parse().ok())
            .unwrap_or(min);
        attrs.retain(|(k, _)| k != "min" && k != "max");
        if min <= max {
            out.push(FileCol { min, max, attrs });
        }
        pos = gt + 1;
    }
    out
}

fn col_layout_from(attrs: &Attrs, max: u32) -> ColLayout {
    ColLayout {
        max,
        width: get_attr(attrs, "width").and_then(|v| v.parse().ok()),
        hidden: is_true(get_attr(attrs, "hidden")),
        outline_level: get_attr(attrs, "outlineLevel")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        style: get_attr(attrs, "style").and_then(|v| v.parse().ok()),
    }
}

fn row_layout_from(attrs: &Attrs) -> RowLayout {
    RowLayout {
        height: get_attr(attrs, "ht").and_then(|v| v.parse().ok()),
        hidden: is_true(get_attr(attrs, "hidden")),
    }
}

/// Rows carrying `ht` / `hidden`. `only` stops at that row.
fn scan_rows(xml: &[u8], only: Option<u32>) -> Vec<(u32, RowLayout)> {
    let mut out = Vec::new();
    let Some(sd) = find_element(xml, b"sheetData", 0) else {
        return out;
    };
    let close = memchr::memmem::find(&xml[sd..], b"</sheetData>").map_or(xml.len(), |p| sd + p);
    let body = &xml[..close];
    let mut pos = sd + 10;
    let mut seq = 0u32;
    while let Some(rs) = find_element(body, b"row", pos) {
        let Some(gt) = memchr::memchr(b'>', &body[rs..]).map(|g| rs + g) else {
            break;
        };
        let (_, attrs, _) = parse_tag(&String::from_utf8_lossy(&body[rs..=gt]));
        let r = get_attr(&attrs, "r")
            .and_then(|v| v.parse().ok())
            .unwrap_or(seq + 1);
        seq = r;
        pos = gt + 1;
        if let Some(t) = only {
            if r < t {
                continue;
            }
            if r > t {
                break;
            }
        }
        let l = row_layout_from(&attrs);
        if l != RowLayout::default() {
            out.push((r, l));
        }
        if only.is_some() {
            break;
        }
    }
    out
}

fn sheet_source_xml(ov: &WorkbookOverlay, sheet: &str) -> Option<Vec<u8>> {
    let target = ov.archive_map.sheet_name_map.get(sheet)?;
    inflate_entry(&ov.archive_map, target).ok().flatten()
}

// ---------------------------------------------------------------------------
// Overlay read / write API (used by the Python binding).
// ---------------------------------------------------------------------------

pub fn get_col(ov: &WorkbookOverlay, sheet: &str, col: u32) -> ColLayout {
    if let Some(l) = ov
        .sheet_overlays
        .get(sheet)
        .and_then(|so| so.layout.col(col))
    {
        return ColLayout { max: col, ..l };
    }
    if let Some(xml) = sheet_source_xml(ov, sheet) {
        if let Some(fc) = scan_cols(&xml)
            .into_iter()
            .find(|f| f.min <= col && col <= f.max)
        {
            return col_layout_from(&fc.attrs, col);
        }
    }
    ColLayout {
        max: col,
        ..Default::default()
    }
}

pub fn update_col(ov: &mut WorkbookOverlay, sheet: &str, col: u32, f: impl FnOnce(&mut ColLayout)) {
    let mut l = get_col(ov, sheet, col);
    f(&mut l);
    l.max = col;
    let so = ov.sheet_overlays.entry(sheet.to_string()).or_default();
    so.layout.insert_cols(col, l);
    so.is_dirty = true;
}

pub fn get_row(ov: &WorkbookOverlay, sheet: &str, row: u32) -> RowLayout {
    if let Some(l) = ov
        .sheet_overlays
        .get(sheet)
        .and_then(|so| so.layout.rows.get(&row))
    {
        return *l;
    }
    sheet_source_xml(ov, sheet)
        .and_then(|xml| scan_rows(&xml, Some(row)).into_iter().next())
        .map(|(_, l)| l)
        .unwrap_or_default()
}

pub fn update_row(ov: &mut WorkbookOverlay, sheet: &str, row: u32, f: impl FnOnce(&mut RowLayout)) {
    let mut l = get_row(ov, sheet, row);
    f(&mut l);
    let so = ov.sheet_overlays.entry(sheet.to_string()).or_default();
    so.layout.rows.insert(row, l);
    so.is_dirty = true;
}

/// Current merged ranges (file + overlay), 1-based `(r1, c1, r2, c2)`.
pub fn merged_ranges(
    ov: &mut WorkbookOverlay,
    sheet: &str,
) -> TurboResult<Vec<(u32, u32, u32, u32)>> {
    ov.ensure_merged_cache(sheet)?;
    Ok(ov
        .sheet_overlays
        .get(sheet)
        .map(|so| so.merged_bounds.clone())
        .unwrap_or_default())
}

pub fn range_ref(r1: u32, c1: u32, r2: u32, c2: u32) -> String {
    range_a1(&CellRange {
        r0: r1 - 1,
        c0: c1 - 1,
        r1: r2 - 1,
        c1: c2 - 1,
    })
}

/// `freeze_panes` read-back: overlay value, else the file's frozen pane.
pub fn freeze_panes(ov: &WorkbookOverlay, sheet: &str) -> Option<String> {
    if let Some(fp) = ov
        .sheet_overlays
        .get(sheet)
        .and_then(|so| so.freeze_panes.clone())
    {
        return Some(fp);
    }
    let xml = sheet_source_xml(ov, sheet)?;
    let header_end = find_element(&xml, b"sheetData", 0).unwrap_or(xml.len());
    let (_, _, view, _, _, _) = crate::turbo::meta::scan_sheet_header(&xml[..header_end]);
    let pane = view?.pane?;
    if pane.state == crate::turbo::meta::PaneState::Split {
        return None;
    }
    let (r, c) = pane.top_left_cell.unwrap_or((
        pane.y_split.unwrap_or(0.0) as u32,
        pane.x_split.unwrap_or(0.0) as u32,
    ));
    Some(range_ref(r + 1, c + 1, r + 1, c + 1))
}

/// `auto_filter` read-back: overlay value, else the file's `<autoFilter ref>`.
pub fn auto_filter(ov: &WorkbookOverlay, sheet: &str) -> Option<String> {
    if let Some(af) = ov
        .sheet_overlays
        .get(sheet)
        .and_then(|so| so.auto_filter.clone())
    {
        return Some(af);
    }
    let xml = sheet_source_xml(ov, sheet)?;
    let tail_start = memchr::memmem::find(&xml, b"</sheetData>").unwrap_or(0);
    crate::turbo::meta::scan_auto_filter(&xml[tail_start..]).map(|af| range_a1(&af.ref_))
}

/// `copy_worksheet` support: copy merges and column/row layout of `src` onto `dst`.
pub fn copy_layout(ov: &mut WorkbookOverlay, src: &str, dst: &str) -> TurboResult<()> {
    let mut layout = SheetLayout::default();
    if let Some(xml) = sheet_source_xml(ov, src) {
        for fc in scan_cols(&xml) {
            layout
                .cols
                .insert(fc.min, col_layout_from(&fc.attrs, fc.max));
        }
        layout.rows.extend(scan_rows(&xml, None));
    }
    if let Some(so) = ov.sheet_overlays.get(src) {
        for (&min, &l) in &so.layout.cols {
            layout.insert_cols(min, l);
        }
        layout
            .rows
            .extend(so.layout.rows.iter().map(|(k, v)| (*k, *v)));
    }
    let merges = merged_ranges(ov, src)?;
    let dso = ov.sheet_overlays.entry(dst.to_string()).or_default();
    for (r1, c1, r2, c2) in merges {
        let s = range_ref(r1, c1, r2, c2);
        if !dso.merges.contains(&s) {
            dso.merges.push(s);
        }
    }
    dso.merged_lookup = None;
    dso.layout = layout;
    dso.is_dirty = true;
    Ok(())
}

// ---------------------------------------------------------------------------
// Save-time application.
// ---------------------------------------------------------------------------

/// Apply column / row layout overrides to a final sheet XML.
pub fn apply_layout(xml: Vec<u8>, layout: &SheetLayout) -> Vec<u8> {
    if layout.is_empty() {
        return xml;
    }
    let mut xml = if layout.rows.is_empty() {
        xml
    } else {
        apply_rows(&xml, &layout.rows).unwrap_or(xml)
    };
    if !layout.cols.is_empty() {
        apply_cols(&mut xml, &layout.cols);
    }
    xml
}

fn set_row_attrs(attrs: &mut Attrs, l: &RowLayout) {
    set_attr(attrs, "ht", l.height.map(fmt_f64));
    set_attr(attrs, "customHeight", l.height.map(|_| "1".to_string()));
    set_attr(attrs, "hidden", l.hidden.then(|| "1".to_string()));
}

fn emit_new_row(out: &mut Vec<u8>, r: u32, l: &RowLayout) {
    if *l == RowLayout::default() {
        return;
    }
    let mut attrs: Attrs = vec![("r".to_string(), r.to_string())];
    set_row_attrs(&mut attrs, l);
    emit_tag(out, "row", &attrs, true);
}

fn apply_rows(xml: &[u8], rows: &BTreeMap<u32, RowLayout>) -> Option<Vec<u8>> {
    let sd = find_element(xml, b"sheetData", 0)?;
    let gt = sd + memchr::memchr(b'>', &xml[sd..])?;
    let mut out = Vec::with_capacity(xml.len() + rows.len() * 48);
    let mut pending = rows.iter().peekable();
    if xml[gt - 1] == b'/' {
        out.extend_from_slice(&xml[..sd]);
        out.extend_from_slice(b"<sheetData>");
        for (r, l) in pending {
            emit_new_row(&mut out, *r, l);
        }
        out.extend_from_slice(b"</sheetData>");
        out.extend_from_slice(&xml[gt + 1..]);
        return Some(out);
    }
    let close = gt + 1 + memchr::memmem::find(&xml[gt + 1..], b"</sheetData>")?;
    let body = &xml[..close];
    let mut pos = gt + 1;
    out.extend_from_slice(&xml[..pos]);
    let mut seq = 0u32;
    while let Some(rs) = find_element(body, b"row", pos) {
        let rgt = rs + memchr::memchr(b'>', &body[rs..])?;
        out.extend_from_slice(&body[pos..rs]);
        let tag = String::from_utf8_lossy(&body[rs..=rgt]);
        let (name, mut attrs, sc) = parse_tag(&tag);
        let r = get_attr(&attrs, "r")
            .and_then(|v| v.parse().ok())
            .unwrap_or(seq + 1);
        seq = r;
        while let Some(&(&pr, pl)) = pending.peek() {
            if pr > r {
                break;
            }
            if pr < r {
                emit_new_row(&mut out, pr, pl);
            }
            pending.next();
        }
        match rows.get(&r) {
            Some(l) => {
                set_row_attrs(&mut attrs, l);
                emit_tag(&mut out, &name, &attrs, sc);
            }
            None => out.extend_from_slice(&body[rs..=rgt]),
        }
        pos = rgt + 1;
    }
    out.extend_from_slice(&body[pos..close]);
    for (r, l) in pending {
        emit_new_row(&mut out, *r, l);
    }
    out.extend_from_slice(&xml[close..]);
    Some(out)
}

fn apply_cols(xml: &mut Vec<u8>, cols: &BTreeMap<u32, ColLayout>) {
    let existing = scan_cols(xml);
    let mut entries: Vec<(u32, u32, Attrs)> = Vec::new();
    for fc in &existing {
        let mut start = fc.min;
        for (&omin, o) in cols.range(..=fc.max) {
            if o.max < start {
                continue;
            }
            if omin > start {
                entries.push((start, omin - 1, fc.attrs.clone()));
            }
            start = start.max(o.max + 1);
            if start > fc.max {
                break;
            }
        }
        if start <= fc.max {
            entries.push((start, fc.max, fc.attrs.clone()));
        }
    }
    for (&omin, o) in cols {
        let mut attrs = existing
            .iter()
            .find(|f| f.min <= omin && omin <= f.max)
            .map(|f| f.attrs.clone())
            .unwrap_or_default();
        set_attr(&mut attrs, "width", o.width.map(fmt_f64));
        set_attr(&mut attrs, "customWidth", o.width.map(|_| "1".to_string()));
        set_attr(&mut attrs, "hidden", o.hidden.then(|| "1".to_string()));
        set_attr(
            &mut attrs,
            "outlineLevel",
            (o.outline_level > 0).then(|| o.outline_level.to_string()),
        );
        if get_attr(&attrs, "style").is_none() {
            set_attr(&mut attrs, "style", o.style.map(|s| s.to_string()));
        }
        if !attrs.is_empty() {
            entries.push((omin, o.max, attrs));
        }
    }
    entries.sort_by_key(|e| e.0);
    let mut block = Vec::new();
    if !entries.is_empty() {
        block.extend_from_slice(b"<cols>");
        for (min, max, attrs) in entries {
            let mut all: Attrs = vec![
                ("min".into(), min.to_string()),
                ("max".into(), max.to_string()),
            ];
            all.extend(attrs);
            emit_tag(&mut block, "col", &all, true);
        }
        block.extend_from_slice(b"</cols>");
    }
    splice_tail_control(
        xml,
        b"cols",
        &block,
        &[b"sheetFormatPr", b"sheetViews", b"dimension", b"sheetPr"],
        b"<sheetData",
    );
}

// ---------------------------------------------------------------------------
// Python binding proxies (thin: every call goes through the functions above).
// ---------------------------------------------------------------------------

#[cfg(feature = "python")]
pub mod py {
    use std::sync::{Arc, Mutex, MutexGuard};

    use pyo3::IntoPyObjectExt;
    use pyo3::exceptions::{PyKeyError, PyValueError};
    use pyo3::prelude::*;

    use super::*;
    use crate::turbo::scan::{MAX_GRID_COLS, MAX_GRID_ROWS, parse_ref_range_strict};

    type Shared = Arc<Mutex<WorkbookOverlay>>;

    fn lock(ov: &Shared) -> PyResult<MutexGuard<'_, WorkbookOverlay>> {
        ov.lock().map_err(|e| PyValueError::new_err(e.to_string()))
    }

    fn col_index(key: &Bound<'_, PyAny>) -> PyResult<u32> {
        let idx = if let Ok(i) = key.extract::<u32>() {
            Some(i)
        } else {
            let s: String = key.extract()?;
            parse_ref_range_strict(format!("{}1", s.trim()).as_bytes()).map(|(_, c, _, _)| c)
        };
        idx.filter(|&c| c >= 1 && c <= MAX_GRID_COLS)
            .ok_or_else(|| PyKeyError::new_err(format!("invalid column key {key}")))
    }

    /// `ws.column_dimensions` / `ws.row_dimensions`.
    #[pyclass(name = "DimensionHolder")]
    pub struct PyDimensionHolder {
        pub sheet_name: String,
        pub overlay: Shared,
        pub rows: bool,
    }

    #[pymethods]
    impl PyDimensionHolder {
        fn __getitem__(&self, py: Python<'_>, key: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
            let sheet_name = self.sheet_name.clone();
            let overlay = Arc::clone(&self.overlay);
            if self.rows {
                let row: u32 = key.extract()?;
                if row == 0 || row > MAX_GRID_ROWS {
                    return Err(PyKeyError::new_err(format!("invalid row index {row}")));
                }
                PyRowDimension {
                    sheet_name,
                    overlay,
                    row,
                }
                .into_py_any(py)
            } else {
                let col = col_index(key)?;
                PyColumnDimension {
                    sheet_name,
                    overlay,
                    col,
                }
                .into_py_any(py)
            }
        }
    }

    #[pyclass(name = "ColumnDimension")]
    pub struct PyColumnDimension {
        sheet_name: String,
        overlay: Shared,
        col: u32,
    }

    impl PyColumnDimension {
        fn get(&self) -> PyResult<ColLayout> {
            Ok(get_col(&*lock(&self.overlay)?, &self.sheet_name, self.col))
        }
        fn set(&self, f: impl FnOnce(&mut ColLayout)) -> PyResult<()> {
            update_col(&mut *lock(&self.overlay)?, &self.sheet_name, self.col, f);
            Ok(())
        }
    }

    #[pymethods]
    impl PyColumnDimension {
        #[getter]
        fn index(&self) -> String {
            let r = range_ref(1, self.col, 1, self.col);
            r.trim_end_matches('1').to_string()
        }
        #[getter]
        fn width(&self) -> PyResult<Option<f64>> {
            Ok(self.get()?.width)
        }
        #[setter]
        fn set_width(&self, v: Option<f64>) -> PyResult<()> {
            self.set(|l| l.width = v)
        }
        #[getter]
        fn hidden(&self) -> PyResult<bool> {
            Ok(self.get()?.hidden)
        }
        #[setter]
        fn set_hidden(&self, v: bool) -> PyResult<()> {
            self.set(|l| l.hidden = v)
        }
        #[getter]
        fn outline_level(&self) -> PyResult<u8> {
            Ok(self.get()?.outline_level)
        }
        #[setter]
        fn set_outline_level(&self, v: u8) -> PyResult<()> {
            if v > 7 {
                return Err(PyValueError::new_err("outline_level must be 0..=7"));
            }
            self.set(|l| l.outline_level = v)
        }
    }

    #[pyclass(name = "RowDimension")]
    pub struct PyRowDimension {
        sheet_name: String,
        overlay: Shared,
        row: u32,
    }

    #[pymethods]
    impl PyRowDimension {
        #[getter]
        fn index(&self) -> u32 {
            self.row
        }
        #[getter]
        fn height(&self) -> PyResult<Option<f64>> {
            Ok(get_row(&*lock(&self.overlay)?, &self.sheet_name, self.row).height)
        }
        #[setter]
        fn set_height(&self, v: Option<f64>) -> PyResult<()> {
            update_row(
                &mut *lock(&self.overlay)?,
                &self.sheet_name,
                self.row,
                |l| l.height = v,
            );
            Ok(())
        }
        #[getter]
        fn hidden(&self) -> PyResult<bool> {
            Ok(get_row(&*lock(&self.overlay)?, &self.sheet_name, self.row).hidden)
        }
        #[setter]
        fn set_hidden(&self, v: bool) -> PyResult<()> {
            update_row(
                &mut *lock(&self.overlay)?,
                &self.sheet_name,
                self.row,
                |l| l.hidden = v,
            );
            Ok(())
        }
    }

    /// A merged range; `str(r)` is its A1 ref ("A1:C1").
    #[pyclass(name = "CellRange", frozen, skip_from_py_object)]
    #[derive(Clone)]
    pub struct PyCellRange {
        #[pyo3(get)]
        min_row: u32,
        #[pyo3(get)]
        min_col: u32,
        #[pyo3(get)]
        max_row: u32,
        #[pyo3(get)]
        max_col: u32,
    }

    #[pymethods]
    impl PyCellRange {
        #[getter]
        fn coord(&self) -> String {
            range_ref(self.min_row, self.min_col, self.max_row, self.max_col)
        }
        #[getter]
        fn bounds(&self) -> (u32, u32, u32, u32) {
            (self.min_col, self.min_row, self.max_col, self.max_row)
        }
        fn __str__(&self) -> String {
            self.coord()
        }
        fn __repr__(&self) -> String {
            format!("<CellRange {}>", self.coord())
        }
        fn __hash__(&self) -> u64 {
            ((self.min_row as u64) << 42)
                ^ ((self.min_col as u64) << 28)
                ^ ((self.max_row as u64) << 14)
                ^ self.max_col as u64
        }
        fn __eq__(&self, other: &Bound<'_, PyAny>) -> bool {
            parse_any_range(other)
                .is_some_and(|b| b == (self.min_row, self.min_col, self.max_row, self.max_col))
        }
    }

    fn parse_any_range(obj: &Bound<'_, PyAny>) -> Option<(u32, u32, u32, u32)> {
        if let Ok(r) = obj.cast::<PyCellRange>() {
            let r = r.get();
            return Some((r.min_row, r.min_col, r.max_row, r.max_col));
        }
        let s: String = obj.extract().ok()?;
        let (r1, c1, r2, c2) = parse_ref_range_strict(s.replace('$', "").as_bytes())?;
        Some((r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)))
    }

    /// `ws.merged_cells` snapshot (openpyxl `MultiCellRange`-like).
    #[pyclass(name = "MultiCellRange")]
    pub struct PyMergedCells {
        bounds: Vec<(u32, u32, u32, u32)>,
    }

    impl PyMergedCells {
        pub fn load(sheet_name: &str, overlay: &Shared) -> PyResult<Self> {
            let bounds = merged_ranges(&mut *lock(overlay)?, sheet_name)
                .map_err(|e| PyValueError::new_err(e.to_string()))?;
            Ok(Self { bounds })
        }
        fn items(&self) -> Vec<PyCellRange> {
            self.bounds
                .iter()
                .map(|&(r1, c1, r2, c2)| PyCellRange {
                    min_row: r1.min(r2),
                    min_col: c1.min(c2),
                    max_row: r1.max(r2),
                    max_col: c1.max(c2),
                })
                .collect()
        }
    }

    #[pymethods]
    impl PyMergedCells {
        #[getter]
        fn ranges(&self) -> Vec<PyCellRange> {
            self.items()
        }
        fn __iter__(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
            let list = pyo3::types::PyList::new(py, self.items())?;
            Ok(list.try_iter()?.into_any().unbind())
        }
        fn __len__(&self) -> usize {
            self.bounds.len()
        }
        /// True when the cell / range is inside any merged range.
        fn __contains__(&self, item: &Bound<'_, PyAny>) -> bool {
            let Some((r1, c1, r2, c2)) = parse_any_range(item) else {
                return false;
            };
            self.items()
                .iter()
                .any(|m| m.min_row <= r1 && r2 <= m.max_row && m.min_col <= c1 && c2 <= m.max_col)
        }
        fn __str__(&self) -> String {
            self.items()
                .iter()
                .map(|r| r.coord())
                .collect::<Vec<_>>()
                .join(" ")
        }
        fn __repr__(&self) -> String {
            format!("<MultiCellRange [{}]>", self.__str__())
        }
    }

    /// `ws.auto_filter` (openpyxl-style `.ref`; compares equal to its ref string).
    #[pyclass(name = "AutoFilter")]
    pub struct PyAutoFilter {
        pub sheet_name: String,
        pub overlay: Shared,
    }

    #[pymethods]
    impl PyAutoFilter {
        #[getter(r#ref)]
        fn get_ref(&self) -> PyResult<Option<String>> {
            Ok(auto_filter(&*lock(&self.overlay)?, &self.sheet_name))
        }
        #[setter(r#ref)]
        fn set_ref_value(&self, v: Option<String>) -> PyResult<()> {
            let mut ov = lock(&self.overlay)?;
            let so = ov
                .sheet_overlays
                .entry(self.sheet_name.clone())
                .or_default();
            so.auto_filter = v;
            so.is_dirty = true;
            Ok(())
        }
        fn __str__(&self) -> PyResult<String> {
            Ok(self.get_ref()?.unwrap_or_default())
        }
        fn __repr__(&self) -> PyResult<String> {
            Ok(format!("<AutoFilter ref={:?}>", self.get_ref()?))
        }
        fn __bool__(&self) -> PyResult<bool> {
            Ok(self.get_ref()?.is_some())
        }
        fn __eq__(&self, other: &Bound<'_, PyAny>) -> PyResult<bool> {
            let mine = self.get_ref()?;
            if other.is_none() {
                return Ok(mine.is_none());
            }
            if let Ok(o) = other.cast::<PyAutoFilter>() {
                return Ok(mine == o.borrow().get_ref()?);
            }
            Ok(other
                .extract::<String>()
                .ok()
                .is_some_and(|s| Some(s) == mine))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHEET: &[u8] = br#"<worksheet><dimension ref="A1"/><sheetFormatPr defaultRowHeight="15"/><cols><col min="1" max="3" width="9" style="2" customWidth="1"/></cols><sheetData><row r="2" spans="1:1"><c r="A2"><v>1</v></c></row><row r="5" ht="20" customHeight="1"/></sheetData></worksheet>"#;

    #[test]
    fn layout_cols_split_existing_range() {
        let mut lay = SheetLayout::default();
        lay.insert_cols(
            2,
            ColLayout {
                max: 2,
                width: Some(30.0),
                ..Default::default()
            },
        );
        let out = String::from_utf8(apply_layout(SHEET.to_vec(), &lay)).unwrap();
        assert!(out.contains(r#"<cols><col min="1" max="1" width="9" style="2" customWidth="1"/><col min="2" max="2" width="30.0" style="2" customWidth="1"/><col min="3" max="3" width="9" style="2" customWidth="1"/></cols>"#), "{out}");
    }

    #[test]
    fn layout_cols_inserted_when_absent() {
        let xml = br#"<worksheet><sheetFormatPr defaultRowHeight="15"/><sheetData/></worksheet>"#;
        let mut lay = SheetLayout::default();
        lay.insert_cols(
            4,
            ColLayout {
                max: 4,
                hidden: true,
                ..Default::default()
            },
        );
        let out = String::from_utf8(apply_layout(xml.to_vec(), &lay)).unwrap();
        assert!(out.contains(r#"<sheetFormatPr defaultRowHeight="15"/><cols><col min="4" max="4" hidden="1"/></cols><sheetData"#), "{out}");
    }

    #[test]
    fn layout_rows_rewrite_and_insert() {
        let mut lay = SheetLayout::default();
        lay.rows.insert(
            1,
            RowLayout {
                height: Some(40.0),
                hidden: false,
            },
        );
        lay.rows.insert(
            2,
            RowLayout {
                height: None,
                hidden: true,
            },
        );
        lay.rows.insert(5, RowLayout::default());
        lay.rows.insert(
            9,
            RowLayout {
                height: Some(12.5),
                hidden: false,
            },
        );
        let out = String::from_utf8(apply_layout(SHEET.to_vec(), &lay)).unwrap();
        assert!(out.contains(r#"<sheetData><row r="1" ht="40.0" customHeight="1"/><row r="2" spans="1:1" hidden="1"><c r="A2">"#), "{out}");
        assert!(
            out.contains(r#"<row r="5"/><row r="9" ht="12.5" customHeight="1"/></sheetData>"#),
            "{out}"
        );
        assert_eq!(
            scan_rows(out.as_bytes(), Some(9)),
            vec![(
                9,
                RowLayout {
                    height: Some(12.5),
                    hidden: false
                }
            )]
        );
    }

    #[test]
    fn layout_insert_cols_trims_overlaps() {
        let mut lay = SheetLayout::default();
        lay.insert_cols(
            1,
            ColLayout {
                max: 10,
                width: Some(5.0),
                ..Default::default()
            },
        );
        lay.insert_cols(
            4,
            ColLayout {
                max: 4,
                width: Some(8.0),
                ..Default::default()
            },
        );
        let keys: Vec<(u32, u32)> = lay.cols.iter().map(|(k, v)| (*k, v.max)).collect();
        assert_eq!(keys, vec![(1, 3), (4, 4), (5, 10)]);
        assert_eq!(lay.col(7).unwrap().width, Some(5.0));
    }
}
