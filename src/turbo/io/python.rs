//! PyO3 bindings for the turbo format-interchange layer: CSV and JSON
//! import/export (src/turbo/io/csv.rs and json.rs).
//!
//! Rust owns every rule (RFC 4180 quoting, number fidelity, date rendering,
//! streaming parsers); this file only marshals paths, bytes and option strings
//! across the FFI boundary, exactly like the Phase 3 `python_*` modules.
//!
//! Approach B (the agreed plan): path functions for the streaming disk-to-disk
//! case and bytes variants for the in-process case, mirroring the
//! `write_excel_turbo` / `write_excel_turbo_bytes` pair. Every call runs under
//! `py.detach` so the GIL is released for the I/O and parse. A missing sheet on
//! export surfaces as the typed `SheetNotFoundError` (the same resolution the
//! turbo reader uses); everything else converts through `turbo_err_to_py`.
//!
//! WIRING (the coordinator applies these; this file ships no `cfg` of its own):
//! WIRING:
//! WIRING: 1. src/turbo/io/mod.rs — add:
//! WIRING:        #[cfg(feature = "python")]
//! WIRING:        pub mod python;
//! WIRING:
//! WIRING: 2. src/lib.rs, inside the _kyrax pymodule (after the Phase 3
//! WIRING:    features block):
//! WIRING:        // turbo io: csv + json interchange
//! WIRING:        {
//! WIRING:            use crate::turbo::io::python::{
//! WIRING:                py_csv_bytes_to_sheet, py_csv_to_sheet, py_json_bytes_to_sheet,
//! WIRING:                py_json_to_sheet, py_sheet_to_csv, py_sheet_to_csv_bytes,
//! WIRING:                py_sheet_to_json, py_sheet_to_json_bytes,
//! WIRING:            };
//! WIRING:            m.add_function(wrap_pyfunction!(py_sheet_to_csv, m)?)?;
//! WIRING:            m.add_function(wrap_pyfunction!(py_sheet_to_csv_bytes, m)?)?;
//! WIRING:            m.add_function(wrap_pyfunction!(py_csv_to_sheet, m)?)?;
//! WIRING:            m.add_function(wrap_pyfunction!(py_csv_bytes_to_sheet, m)?)?;
//! WIRING:            m.add_function(wrap_pyfunction!(py_sheet_to_json, m)?)?;
//! WIRING:            m.add_function(wrap_pyfunction!(py_sheet_to_json_bytes, m)?)?;
//! WIRING:            m.add_function(wrap_pyfunction!(py_json_to_sheet, m)?)?;
//! WIRING:            m.add_function(wrap_pyfunction!(py_json_bytes_to_sheet, m)?)?;
//! WIRING:        }

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyBytes;

use crate::error::py_errors::IntoPyResult;
use crate::error::{KyraxError, KyraxErrorKind};
use crate::turbo::error::TurboError;
use crate::turbo::io::csv::{CsvOptions, csv_bytes_to_sheet, csv_to_sheet, sheet_to_csv};
use crate::turbo::io::json::{
    JsonOptions, JsonShape, json_to_sheet, json_to_sheet_from, sheet_to_json,
};

fn turbo_err_to_py(err: TurboError) -> PyErr {
    let fe: KyraxError = KyraxErrorKind::Internal(err.to_string()).into();
    fe.into()
}

// ---------------------------------------------------------------------------
// Option parsing
// ---------------------------------------------------------------------------

fn parse_byte(s: &str, what: &str) -> PyResult<u8> {
    match s.as_bytes() {
        [b] => Ok(*b),
        _ => Err(PyValueError::new_err(format!(
            "{what} must be a single ASCII character; got {s:?}"
        ))),
    }
}

fn parse_csv_options(
    delimiter: &str,
    quote: &str,
    has_header: bool,
    infer_types: bool,
    date_format: &str,
) -> PyResult<CsvOptions> {
    Ok(CsvOptions {
        delimiter: parse_byte(delimiter, "delimiter")?,
        quote: parse_byte(quote, "quote")?,
        has_header,
        infer_types,
        date_format: date_format.to_owned(),
    })
}

fn parse_csv_import_options(
    delimiter: &str,
    quote: &str,
    has_header: bool,
    infer_types: bool,
) -> PyResult<CsvOptions> {
    Ok(CsvOptions {
        delimiter: parse_byte(delimiter, "delimiter")?,
        quote: parse_byte(quote, "quote")?,
        has_header,
        infer_types,
        date_format: String::new(),
    })
}

fn parse_shape(s: &str) -> PyResult<JsonShape> {
    match s {
        "records" => Ok(JsonShape::Records),
        "columns" => Ok(JsonShape::Columns),
        "ndjson" => Ok(JsonShape::Ndjson),
        other => Err(PyValueError::new_err(format!(
            "shape must be 'records' | 'columns' | 'ndjson'; got {other:?}"
        ))),
    }
}

fn parse_json_options(shape: &str, has_header: bool, date_format: &str) -> PyResult<JsonOptions> {
    Ok(JsonOptions {
        shape: parse_shape(shape)?,
        has_header,
        date_format: date_format.to_owned(),
    })
}

/// Resolve a sheet name to its workbook index, surfacing a typed
/// `SheetNotFoundError` on a miss (the turbo reader's own resolution).
fn resolve_sheet_index(py: Python<'_>, path: &str, sheet: &str) -> PyResult<usize> {
    let path = path.to_owned();
    let names = py
        .detach(|| crate::turbo::list_sheet_names(&path))
        .map_err(turbo_err_to_py)?;
    names
        .iter()
        .position(|n| n == sheet)
        .ok_or_else(|| {
            KyraxErrorKind::SheetNotFound(crate::types::IdxOrName::Name(sheet.to_owned())).into()
        })
        .into_pyresult()
}

// ---------------------------------------------------------------------------
// CSV export
// ---------------------------------------------------------------------------

/// Stream one worksheet to a CSV file (RFC 4180, CRLF).
///
/// Numbers in date-formatted cells render per `date_format` (Excel token
/// syntax, default `yyyy-mm-dd hh:mm:ss`); formula cells emit their cached
/// value; empty-string cells emit `""` (distinct from blank fields). Raises
/// `SheetNotFoundError` for a missing sheet, `ValueError` for an invalid
/// delimiter/quote, and `KyraxError` for I/O or format failures.
#[pyfunction(name = "sheet_to_csv")]
#[allow(clippy::too_many_arguments)]
pub fn py_sheet_to_csv(
    py: Python<'_>,
    path: &str,
    sheet: &str,
    out_path: &str,
    delimiter: &str,
    quote: &str,
    has_header: bool,
    infer_types: bool,
    date_format: &str,
) -> PyResult<()> {
    let opts = parse_csv_options(delimiter, quote, has_header, infer_types, date_format)?;
    resolve_sheet_index(py, path, sheet)?;
    let (path, sheet, out_path) = (path.to_owned(), sheet.to_owned(), out_path.to_owned());
    py.detach(move || {
        let out = std::fs::File::create(&out_path)?;
        sheet_to_csv(&path, &sheet, out, &opts)
    })
    .map_err(turbo_err_to_py)
}

/// Export one worksheet to CSV text, returned as bytes.
///
/// Same options as `sheet_to_csv`; the document is fully materialised in
/// memory (the `write_excel_turbo_bytes` analogue), so callers never touch
/// disk.
#[pyfunction(name = "sheet_to_csv_bytes")]
#[allow(clippy::too_many_arguments)]
pub fn py_sheet_to_csv_bytes<'py>(
    py: Python<'py>,
    path: &str,
    sheet: &str,
    delimiter: &str,
    quote: &str,
    has_header: bool,
    infer_types: bool,
    date_format: &str,
) -> PyResult<Bound<'py, PyBytes>> {
    let opts = parse_csv_options(delimiter, quote, has_header, infer_types, date_format)?;
    resolve_sheet_index(py, path, sheet)?;
    let (path, sheet) = (path.to_owned(), sheet.to_owned());
    let mut buf: Vec<u8> = Vec::new();
    py.detach(|| sheet_to_csv(&path, &sheet, &mut buf, &opts))
        .map_err(turbo_err_to_py)?;
    Ok(PyBytes::new(py, &buf))
}

// ---------------------------------------------------------------------------
// CSV import
// ---------------------------------------------------------------------------

/// Parse a CSV file and write a new single-sheet workbook.
///
/// Every record maps to a sheet row (nothing is dropped); with `infer_types`
/// only leading-zero-safe, precision-safe numerics are promoted to numbers.
#[pyfunction(name = "csv_to_sheet")]
#[allow(clippy::too_many_arguments)]
pub fn py_csv_to_sheet(
    py: Python<'_>,
    csv_path: &str,
    xlsx_out: &str,
    sheet_name: &str,
    delimiter: &str,
    quote: &str,
    has_header: bool,
    infer_types: bool,
) -> PyResult<()> {
    let opts = parse_csv_import_options(delimiter, quote, has_header, infer_types)?;
    let (csv_path, xlsx_out, sheet_name) = (
        csv_path.to_owned(),
        xlsx_out.to_owned(),
        sheet_name.to_owned(),
    );
    py.detach(move || csv_to_sheet(&csv_path, &xlsx_out, &sheet_name, &opts))
        .map_err(turbo_err_to_py)
}

/// Parse an in-memory CSV buffer and write a new single-sheet workbook.
///
/// Same semantics as `csv_to_sheet`; the CSV never touches disk.
#[pyfunction(name = "csv_bytes_to_sheet")]
#[allow(clippy::too_many_arguments)]
pub fn py_csv_bytes_to_sheet(
    py: Python<'_>,
    csv_bytes: &[u8],
    xlsx_out: &str,
    sheet_name: &str,
    delimiter: &str,
    quote: &str,
    has_header: bool,
    infer_types: bool,
) -> PyResult<()> {
    let opts = parse_csv_import_options(delimiter, quote, has_header, infer_types)?;
    let data = csv_bytes.to_vec();
    let (xlsx_out, sheet_name) = (xlsx_out.to_owned(), sheet_name.to_owned());
    py.detach(move || csv_bytes_to_sheet(&data, &xlsx_out, &sheet_name, &opts))
        .map_err(turbo_err_to_py)
}

// ---------------------------------------------------------------------------
// JSON export
// ---------------------------------------------------------------------------

/// Stream one worksheet to a JSON/NDJSON file.
///
/// `shape` is `"records"` (row-oriented array), `"columns"` (column-oriented
/// object) or `"ndjson"` (one object per line). Empty cells emit `null`,
/// empty-string cells emit `""`, integers beyond 2^53 emit as strings, and
/// date-styled cells render per `date_format` (strftime tokens; empty = ISO
/// 8601).
#[pyfunction(name = "sheet_to_json")]
pub fn py_sheet_to_json(
    py: Python<'_>,
    path: &str,
    sheet: &str,
    out_path: &str,
    shape: &str,
    has_header: bool,
    date_format: &str,
) -> PyResult<()> {
    let opts = parse_json_options(shape, has_header, date_format)?;
    resolve_sheet_index(py, path, sheet)?;
    let (path, sheet, out_path) = (path.to_owned(), sheet.to_owned(), out_path.to_owned());
    py.detach(move || {
        let out = std::fs::File::create(&out_path)?;
        sheet_to_json(&path, &sheet, out, &opts)
    })
    .map_err(turbo_err_to_py)
}

/// Export one worksheet to JSON/NDJSON text, returned as bytes.
///
/// Same options as `sheet_to_json`; the document is fully materialised in
/// memory.
#[pyfunction(name = "sheet_to_json_bytes")]
pub fn py_sheet_to_json_bytes<'py>(
    py: Python<'py>,
    path: &str,
    sheet: &str,
    shape: &str,
    has_header: bool,
    date_format: &str,
) -> PyResult<Bound<'py, PyBytes>> {
    let opts = parse_json_options(shape, has_header, date_format)?;
    resolve_sheet_index(py, path, sheet)?;
    let (path, sheet) = (path.to_owned(), sheet.to_owned());
    let mut buf: Vec<u8> = Vec::new();
    py.detach(|| sheet_to_json(&path, &sheet, &mut buf, &opts))
        .map_err(turbo_err_to_py)?;
    Ok(PyBytes::new(py, &buf))
}

// ---------------------------------------------------------------------------
// JSON import
// ---------------------------------------------------------------------------

/// Parse a JSON/NDJSON file and write a new single-sheet workbook.
///
/// `Records`/`Ndjson` accept heterogeneous keys; the first-seen union becomes
/// the columns with missing values as empty cells. Nested objects/arrays land
/// as their raw JSON text; integers beyond 2^53 are kept as their digit
/// strings.
#[pyfunction(name = "json_to_sheet")]
pub fn py_json_to_sheet(
    py: Python<'_>,
    json_path: &str,
    xlsx_out: &str,
    sheet_name: &str,
    shape: &str,
    has_header: bool,
) -> PyResult<()> {
    let opts = parse_json_options(shape, has_header, "")?;
    let (json_path, xlsx_out, sheet_name) = (
        json_path.to_owned(),
        xlsx_out.to_owned(),
        sheet_name.to_owned(),
    );
    py.detach(move || json_to_sheet(&json_path, &xlsx_out, &sheet_name, &opts))
        .map_err(turbo_err_to_py)
}

/// Parse an in-memory JSON/NDJSON buffer and write a new single-sheet workbook.
///
/// Same semantics as `json_to_sheet`; the document never touches disk.
#[pyfunction(name = "json_bytes_to_sheet")]
pub fn py_json_bytes_to_sheet(
    py: Python<'_>,
    json_bytes: &[u8],
    xlsx_out: &str,
    sheet_name: &str,
    shape: &str,
    has_header: bool,
) -> PyResult<()> {
    let opts = parse_json_options(shape, has_header, "")?;
    let data = json_bytes.to_vec();
    let (xlsx_out, sheet_name) = (xlsx_out.to_owned(), sheet_name.to_owned());
    py.detach(move || json_to_sheet_from(data.as_slice(), &xlsx_out, &sheet_name, &opts))
        .map_err(turbo_err_to_py)
}

// No unit tests here: these bindings take `Bound` values that need an
// initialised interpreter, which is why the crate's other python.rs files
// carry none either. `cargo check --features python` is the gate.
