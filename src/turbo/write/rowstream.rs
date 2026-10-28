//! Direct row streaming for plain `rows` sheets (B2).
//!
//! The common write is "a list of plain row lists". For that shape we skip the
//! `Workbook` cell model entirely: Python row -> XML chunk -> zip, with O(1) row
//! of model memory. `try_write_path` / `try_write_bytes` return "not handled"
//! (`false` / `None`) for anything outside that shape *before* touching a sink,
//! and the caller then runs today's path unchanged. Output parts are
//! byte-identical to the model path (only the part order differs: styles.xml is
//! written after the worksheets because date xfs are discovered while streaming).
//!
//! GIL: held only while reading Python row objects. Every flush of the chunk
//! buffer into the zip writer and every tail part (sheet trailer + deflate,
//! styles, sst, workbook, rels, content types, zip finish) runs detached.
//! Consequence: another Python thread may mutate a row list between the
//! pre-pass and the streaming pass; that cannot cause unsafety, only a
//! dimension/content mismatch (the write itself is not atomic w.r.t. threads).

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Cursor, Seek, Write};

use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::ffi;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};

use super::model::*;
use super::python::{extract_wrapper_style, parse_string_mode, py_to_cell_value_flagged, write_err_to_py};
use super::structural::{root_rels_xml, write_app_props, write_core_props};
use super::style_engine::StyleEngine;
use super::writer::{
    PartCounters, SstAccess, THEME_XML, resolve_cell_style, stream_add_entry, write_content_types,
    write_row, write_sheet_head, write_sheet_head_unknown_dimension, write_sheet_tail, write_sst, write_workbook_rels,
    write_workbook_xml,
};
use super::media::MediaInterner;
use super::xml::dimension_ref;
use super::zip::StreamingZipWriter;

/// Flush the XML chunk buffer to the zip writer once it reaches this size.
const FLUSH_AT: usize = 64 * 1024;
const MAX_ROWS: usize = 1_048_576;
const MAX_COLS: usize = 16_384;

/// The entry-point kwargs the fast path needs.
pub(crate) struct Kw<'a> {
    string_mode: &'a str,
    emit_cached_values: bool,
    date1904: bool,
    date_iso: bool,
    active_tab: u32,
    creator: Option<&'a str>,
    /// All the kwargs the fast path cannot honour are at their defaults.
    plain: bool,
}

impl<'a> Kw<'a> {
    /// `plain`: no feature/named_styles/props/defined_names/chartsheets/
    /// external_links/vba kwarg and no lock_structure/macro_enabled/recalculate.
    pub(crate) fn new(
        string_mode: &'a str,
        emit_cached_values: bool,
        date1904: bool,
        date_iso: bool,
        active_tab: u32,
        creator: Option<&'a str>,
        plain: bool,
    ) -> Self {
        Self {
            string_mode,
            emit_cached_values,
            date1904,
            date_iso,
            active_tab,
            creator,
            plain,
        }
    }
}

/// Exact type pointers of the cell types the fast path accepts.
struct Types {
    bool_: *mut ffi::PyTypeObject,
    int_: *mut ffi::PyTypeObject,
    float_: *mut ffi::PyTypeObject,
    str_: *mut ffi::PyTypeObject,
    datetime: *mut ffi::PyTypeObject,
    date: *mut ffi::PyTypeObject,
}

impl Types {
    fn new(py: Python<'_>) -> PyResult<Self> {
        let dt = py.import("datetime")?;
        Ok(Self {
            bool_: py.get_type::<PyBool>().as_type_ptr(),
            int_: py.get_type::<PyInt>().as_type_ptr(),
            float_: py.get_type::<PyFloat>().as_type_ptr(),
            str_: py.get_type::<PyString>().as_type_ptr(),
            datetime: dt.getattr("datetime")?.cast_into::<pyo3::types::PyType>()?.as_type_ptr(),
            date: dt.getattr("date")?.cast_into::<pyo3::types::PyType>()?.as_type_ptr(),
        })
    }

    /// True for a non-None cell of an accepted exact type whose conversion
    /// cannot fail or warn mid-stream: ints must fit i64 (bigger ones take the
    /// model path's f64 fallback, which can raise), strings must be within
    /// Excel's 32767-char limit (longer ones warn, and warnings may be errors).
    /// So a mid-write error on the fast path can only be an I/O error.
    #[inline]
    fn accepts(&self, o: &Bound<'_, PyAny>) -> bool {
        let t = o.get_type_ptr();
        if t == self.str_ {
            // len() is the code-point count, O(1) on str.
            return o.len().is_ok_and(|n| n <= 32767);
        }
        if t == self.int_ {
            return o.extract::<i64>().is_ok();
        }
        t == self.float_ || t == self.bool_ || t == self.datetime || t == self.date
    }
}

/// Walk an exact `list` / `tuple`. The callback returns `Ok(false)` to stop;
/// the result is `Ok(false)` when stopped early or when `seq` is neither.
fn seq_for_each<'py, F>(seq: &Bound<'py, PyAny>, mut f: F) -> PyResult<bool>
where
    F: FnMut(usize, Bound<'py, PyAny>) -> PyResult<bool>,
{
    if let Ok(l) = seq.cast_exact::<PyList>() {
        for (i, x) in l.iter().enumerate() {
            if !f(i, x)? {
                return Ok(false);
            }
        }
        Ok(true)
    } else if let Ok(t) = seq.cast_exact::<PyTuple>() {
        for (i, x) in t.iter().enumerate() {
            if !f(i, x)? {
                return Ok(false);
            }
        }
        Ok(true)
    } else {
        Ok(false)
    }
}

struct SheetPlan<'py> {
    name: String,
    state: SheetState,
    rows: Option<Bound<'py, PyAny>>,
    dimension: Option<String>,
}

struct Plan<'py> {
    sheets: Vec<SheetPlan<'py>>,
    use_sst: bool,
    iterable_rows: bool,
}

/// Read-only validation + dimension of one sheet's rows. `None` = ineligible.
/// Mirrors the model path: row index is the position over ALL rows, a row is
/// present iff it has a non-None cell.
fn prepass(types: &Types, rows: &Bound<'_, PyAny>) -> PyResult<Option<String>> {
    let (mut r0, mut r1) = (usize::MAX, 0usize);
    let (mut c0, mut c1) = (usize::MAX, 0usize);
    let ok = seq_for_each(rows, |ri, row| {
        if ri >= MAX_ROWS {
            return Ok(false);
        }
        let (mut first, mut last) = (0usize, 0usize);
        let row_ok = seq_for_each(&row, |ci, c| {
            if ci >= MAX_COLS {
                return Ok(false);
            }
            if !c.is_none() {
                if !types.accepts(&c) {
                    return Ok(false);
                }
                if first == 0 {
                    first = ci + 1;
                }
                last = ci + 1;
            }
            Ok(true)
        })?;
        if !row_ok {
            return Ok(false);
        }
        if first > 0 {
            r0 = r0.min(ri + 1);
            r1 = r1.max(ri + 1);
            c0 = c0.min(first);
            c1 = c1.max(last);
        }
        Ok(true)
    })?;
    if !ok {
        return Ok(None);
    }
    Ok(Some(if r1 == 0 {
        "A1".to_string()
    } else {
        dimension_ref(r0 as u32, c0 as u32, r1 as u32, c1 as u32)
    }))
}

fn plan<'py>(
    py: Python<'py>,
    sheets: &Bound<'py, PyAny>,
    kw: &Kw<'_>,
    iterable_rows: bool,
) -> PyResult<Option<Plan<'py>>> {
    if !kw.plain {
        return Ok(None);
    }
    let use_sst = match parse_string_mode(kw.string_mode) {
        Ok(StringMode::InlineStr) => false,
        Ok(StringMode::SharedStrings) => true,
        _ => return Ok(None),
    };
    let Ok(list) = sheets.cast::<PyList>() else {
        return Ok(None);
    };
    if list.is_empty() {
        return Ok(None);
    }
    let types = Types::new(py)?;
    let mut out = Vec::with_capacity(list.len());
    for item in list.iter() {
        let Ok(d) = item.cast::<PyDict>() else {
            return Ok(None);
        };
        for (k, _) in d.iter() {
            let ok_key = k
                .cast::<PyString>()
                .ok()
                .and_then(|s| s.to_str().ok().map(|s| matches!(s, "name" | "rows" | "visibility" | "state" | "rows_iter")))
                .unwrap_or(false);
            if !ok_key {
                return Ok(None);
            }
        }
        let name = match d.get_item("name")? {
            None => "Sheet".to_string(),
            Some(n) => match n.cast::<PyString>().ok().and_then(|s| s.to_str().ok().map(str::to_owned)) {
                Some(s) => s,
                None => return Ok(None),
            },
        };
        let mut state = SheetState::Visible;
        // Same precedence as the model path: "visibility" first, then "state".
        if let Some(v) = d.get_item("visibility")?.or(d.get_item("state")?) {
            let Some(st) = v
                .cast::<PyString>()
                .ok()
                .and_then(|s| s.to_str().ok().and_then(SheetState::parse))
            else {
                return Ok(None);
            };
            state = st;
        }
        let source = d.get_item("rows")?.or(d.get_item("rows_iter")?);
        let (rows, dimension) = if iterable_rows {
            (source.filter(|r| !r.is_none()), None)
        } else {
            match source {
                None => (None, Some("A1".to_string())),
                Some(r) => match prepass(&types, &r)? {
                    Some(dim) => (Some(r), Some(dim)),
                    None => return Ok(None),
                },
            }
        };
        out.push(SheetPlan {
            name,
            state,
            rows,
            dimension,
        });
    }
    Ok(Some(Plan { sheets: out, use_sst, iterable_rows }))
}

/// Run the streaming pass into `w` and return it once the zip is finished.
fn stream<'py, W: Write + Seek + Send>(
    py: Python<'py>,
    plan: &Plan<'py>,
    kw: &Kw<'_>,
    w: W,
) -> PyResult<W> {
    let mut wb = Workbook::new();
    wb.sheets = plan
        .sheets
        .iter()
        .map(|sp| {
            let mut s = Sheet::new(sp.name.clone());
            s.state = sp.state;
            s.dimension = sp.dimension.clone();
            s
        })
        .collect();
    wb.options = WriteOptions {
        string_mode: if plan.use_sst {
            StringMode::SharedStrings
        } else {
            StringMode::InlineStr
        },
        emit_cached_values: kw.emit_cached_values,
        date1904: kw.date1904,
        date_iso: kw.date_iso,
        features: WriteFeatures::CORE,
        auto_sst_threshold: AUTO_SST_THRESHOLD,
    };
    wb.active_tab = kw.active_tab;
    wb.creator = kw.creator.unwrap_or("kyrax").into();

    let (use_sst, emit_cache) = (plan.use_sst, kw.emit_cached_values);
    let (date_iso, date1904) = (kw.date_iso, kw.date1904);
    let mut zip = StreamingZipWriter::new(w);

    // Same content as save_workbook_stream for a workbook with no props.
    let titles: Vec<&str> = wb.sheets.iter().map(|s| s.name.as_str()).collect();
    let core = write_core_props(
        &DocProps {
            creator: Some(wb.creator.clone()),
            ..Default::default()
        },
        &wb.creator,
    );
    let app = write_app_props(&wb.props, &titles);
    let werr = write_err_to_py;
    stream_add_entry(&mut zip, "docProps/core.xml", core.as_bytes()).map_err(werr)?;
    stream_add_entry(&mut zip, "docProps/app.xml", app.as_bytes()).map_err(werr)?;
    stream_add_entry(&mut zip, "xl/theme/theme1.xml", THEME_XML.as_bytes()).map_err(werr)?;

    let interner = MediaInterner::new();
    let mut counters = PartCounters::default();
    let mut sst = SstBuilder::new();
    let mut eng = StyleEngine::new();
    let mut style_flag = false;
    let mut buf: Vec<u8> = Vec::with_capacity(FLUSH_AT + 4096);
    let mut row = Row::new(0);

    for (i, sp) in plan.sheets.iter().enumerate() {
        let path = format!("xl/worksheets/sheet{}.xml", i + 1);
        if plan.iterable_rows {
            write_sheet_head_unknown_dimension(&mut zip, &path, &wb.sheets[i], &mut buf).map_err(werr)?;
        } else {
            write_sheet_head(&mut zip, &path, &wb.sheets[i], &mut buf).map_err(werr)?;
        }
        buf.clear();
        if let Some(rows) = &sp.rows {
            let mut emit_row = |ri: usize, row_obj: Bound<'py, PyAny>| -> PyResult<bool> {
                if ri >= MAX_ROWS {
                    return Err(PyValueError::new_err("row count exceeds Excel limit of 1048576"));
                }
                row.row = ri as u32 + 1;
                row.cells.clear();
                let mut emit_cell = |ci: usize, c: Bound<'py, PyAny>| -> PyResult<bool> {
                    if ci >= MAX_COLS {
                        return Err(PyValueError::new_err(format!(
                            "row {} exceeds Excel's column limit of 16384", ri + 1
                        )));
                    }
                    if c.is_none() {
                        return Ok(true);
                    }
                    let v = py_to_cell_value_flagged(&c, date1904, Some(&mut style_flag))?;
                    let mut cell = Cell::new(ci as u32 + 1, v);
                    if plan.iterable_rows {
                        cell.style_desc = extract_wrapper_style(&c)?;
                        if matches!(cell.value, CellValue::Empty) && cell.style_desc.is_none() {
                            return Ok(true);
                        }
                    }
                    resolve_cell_style(&mut eng, &mut cell, &wb.options);
                    row.cells.push(cell);
                    Ok(true)
                };
                if plan.iterable_rows {
                    let cells = row_obj.try_iter().map_err(|_| {
                        PyValueError::new_err("each row must be an iterable of cell values")
                    })?;
                    for (ci, c) in cells.enumerate() {
                        emit_cell(ci, c?)?;
                    }
                } else if !seq_for_each(&row_obj, &mut emit_cell)? {
                    return Err(PyTypeError::new_err("row changed type while being written"));
                }
                write_row(
                    &mut buf,
                    &row,
                    use_sst,
                    emit_cache,
                    &mut SstAccess::Build(&mut sst),
                    date_iso,
                    date1904,
                );
                if buf.len() >= FLUSH_AT {
                    py.detach(|| zip.write_chunk(&buf)).map_err(werr)?;
                    buf.clear();
                }
                Ok(true)
            };
            if plan.iterable_rows {
                let iter = rows.try_iter().map_err(|_| {
                    PyValueError::new_err("rows / rows_iter must be an iterable of row sequences")
                })?;
                for (ri, row_obj) in iter.enumerate() {
                    emit_row(ri, row_obj?)?;
                }
            } else if !seq_for_each(rows, &mut emit_row)? {
                return Err(PyTypeError::new_err("rows changed type while being written"));
            }
        }
        // Flush the remainder, then the sheet trailer + deflate, without the GIL.
        py.detach(|| -> io::Result<()> {
            if !buf.is_empty() {
                zip.write_chunk(&buf)?;
                buf.clear();
            }
            write_sheet_tail(
                &mut zip,
                &wb.sheets[i],
                &mut counters,
                wb.options.features,
                &interner,
                &[],
                &mut buf,
            )
            .map(drop)
        })
        .map_err(werr)?;
    }

    // Mirror build_workbook_from_py: dates need StyleEngine, which turns on
    // STYLES|CF_DV; then the structural auto-detection.
    if style_flag {
        wb.options.features = wb
            .options
            .features
            .union(WriteFeatures::STYLES)
            .union(WriteFeatures::CF_DV);
    }
    wb.auto_enable_structural_features();

    py.detach(|| -> io::Result<W> {
        stream_add_entry(&mut zip, "xl/styles.xml", &eng.emit_styles_xml())?;
        let has_sst = use_sst && !sst.is_empty();
        if has_sst {
            stream_add_entry(&mut zip, "xl/sharedStrings.xml", &write_sst(&sst))?;
        }
        let ws_count = wb.sheets.len().max(1);
        let names_states: Vec<(String, SheetState)> = wb
            .sheets
            .iter()
            .map(|s| (s.name.clone(), s.state))
            .collect();
        stream_add_entry(
            &mut zip,
            "xl/workbook.xml",
            &write_workbook_xml(&wb, &names_states, &[], wb.options.features, &[], &[]),
        )?;
        stream_add_entry(
            &mut zip,
            "xl/_rels/workbook.xml.rels",
            &write_workbook_rels(ws_count, 0, 0, has_sst, false, &[], &[]),
        )?;
        stream_add_entry(&mut zip, "_rels/.rels", root_rels_xml(false).as_bytes())?;
        stream_add_entry(
            &mut zip,
            "[Content_Types].xml",
            &write_content_types(
                ws_count,
                0,
                has_sst,
                false,
                false,
                &[],
                &interner.media_defaults(),
            ),
        )?;
        let finished = zip.finish()?;
        super::zip::shrink_comp_scratch();
        Ok(finished)
    })
    .map_err(werr)
}

/// Fast-path write to `path`. `Ok(false)` = not eligible, nothing was touched.
/// On a mid-write error the output file is removed only if this call created it.
pub(crate) fn try_write_path(
    py: Python<'_>,
    sheets: &Bound<'_, PyAny>,
    kw: &Kw<'_>,
    path: &str,
) -> PyResult<bool> {
    let Some(plan) = plan(py, sheets, kw, false)? else {
        return Ok(false);
    };
    let (file, created) = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(f) => (f, true),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            (File::create(path).map_err(write_err_to_py)?, false)
        }
        Err(e) => return Err(write_err_to_py(e)),
    };
    match stream(py, &plan, kw, BufWriter::with_capacity(64 * 1024, file)) {
        Ok(mut bw) => {
            bw.flush().map_err(write_err_to_py)?;
            Ok(true)
        }
        Err(e) => {
            if created {
                let _ = std::fs::remove_file(path);
            }
            Err(e)
        }
    }
}

/// Fast-path write to memory. `Ok(None)` = not eligible.
pub(crate) fn try_write_bytes(
    py: Python<'_>,
    sheets: &Bound<'_, PyAny>,
    kw: &Kw<'_>,
) -> PyResult<Option<Vec<u8>>> {
    let Some(plan) = plan(py, sheets, kw, false)? else {
        return Ok(None);
    };
    Ok(Some(stream(py, &plan, kw, Cursor::new(Vec::new()))?.into_inner()))
}

/// Single-pass row-iterable write. Eligibility is decided without consuming an
/// iterator. A sibling temporary archive protects the destination if iteration,
/// conversion, or an I/O operation fails late in the write.
pub(crate) fn try_write_iter_path(
    py: Python<'_>,
    sheets: &Bound<'_, PyAny>,
    kw: &Kw<'_>,
    path: &str,
) -> PyResult<bool> {
    let Some(plan) = plan(py, sheets, kw, true)? else {
        return Ok(false);
    };
    // Keep exact list/tuple input on the existing model fallback when it did
    // not qualify for its validated fast path. This route is for one-shot and
    // other lazy sources; no iterator is advanced by the eligibility check.
    if !plan.sheets.iter().any(|s| s.rows.as_ref().is_some_and(|r| {
        r.cast_exact::<PyList>().is_err() && r.cast_exact::<PyTuple>().is_err()
    })) {
        return Ok(false);
    }
    let target = std::path::Path::new(path);
    let existing_permissions = match std::fs::metadata(target) {
        Ok(meta) => Some(meta.permissions()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(write_err_to_py(e)),
    };
    let parent = target.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(std::path::Path::new("."));
    if target.file_name().is_none() {
        return Err(PyValueError::new_err("output path must name a file"));
    }
    static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let (temp_path, file) = loop {
        let id = NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Do not append to the destination basename: a valid long filename
        // could otherwise exceed the filesystem's per-component name limit.
        let candidate = parent.join(format!(".kyrax-{}-{id}.tmp", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&candidate) {
            Ok(file) => break (candidate, file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(write_err_to_py(e)),
        }
    };
    struct TempGuard(std::path::PathBuf);
    impl Drop for TempGuard {
        fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); }
    }
    let guard = TempGuard(temp_path);
    let mut writer = stream(py, &plan, kw, BufWriter::with_capacity(64 * 1024, file))?;
    writer.flush().map_err(write_err_to_py)?;
    drop(writer);
    if let Some(permissions) = existing_permissions {
        std::fs::set_permissions(&guard.0, permissions).map_err(write_err_to_py)?;
    }
    std::fs::rename(&guard.0, target).map_err(write_err_to_py)?;
    Ok(true)
}
