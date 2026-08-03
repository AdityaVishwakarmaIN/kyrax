mod data;
mod error;
#[cfg(feature = "__arrow")]
pub mod turbo;
mod types;
mod utils;

use std::fmt::Display;

/// Global allocator: reduces Windows heap fragmentation on repeated large
/// write alloc/free cycles (cell model + XML buffers + deflate).
///
/// Windows-only: the system allocators on Linux/macOS don't exhibit the same
/// fragmentation, and pulling mimalloc into the Linux aarch64 cross-build
/// breaks on the manylinux2014 toolchain (mimalloc's C build rejects an
/// unknown `-Wdate-time` flag under its ancient GCC).
#[cfg(all(windows, not(feature = "count_alloc")))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[cfg(feature = "python")]
use error::py_errors;
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use types::excelsheet::{CellError, CellErrors};

pub use data::{KyraxColumn, KyraxSeries};
use error::ErrorContext;
pub use error::{KyraxError, KyraxErrorKind, KyraxResult};
pub use types::{
    ColumnInfo, ColumnNameFrom, DType, DTypeCoercion, DTypeFrom, DTypes, DefinedName, ExcelReader,
    ExcelSheet, ExcelTable, IdxOrName, LoadSheetOrTableOptions, SelectedColumns, SheetVisible,
    SkipRows,
};

/// Reads an excel file and returns an object allowing to access its sheets, tables, and a bit of metadata.
/// This is a wrapper around `ExcelReader::try_from_path`.
pub fn read_excel<S: AsRef<str> + Display>(path: S) -> KyraxResult<ExcelReader> {
    ExcelReader::try_from_path(path.as_ref())
        .with_context(|| format!("could not load excel file at {path}"))
}

#[cfg(feature = "python")]
/// Reads an excel file and returns an object allowing to access its sheets, tables, and a bit of metadata
#[pyfunction(name = "read_excel")]
fn py_read_excel<'py>(source: &Bound<'_, PyAny>, py: Python<'py>) -> PyResult<ExcelReader> {
    use py_errors::IntoPyResult;

    if let Ok(path) = source.extract::<String>() {
        py.detach(|| ExcelReader::try_from_path(&path))
            .with_context(|| format!("could not load excel file at {path}"))
            .into_pyresult()
    } else if let Ok(bytes) = source.extract::<&[u8]>() {
        py.detach(|| ExcelReader::try_from(bytes))
            .with_context(|| "could not load excel file for those bytes")
            .into_pyresult()
    } else {
        Err(py_errors::InvalidParametersError::new_err(
            "source must be a string or bytes",
        ))
    }
}

// Taken from pydantic-core:
// https://github.com/pydantic/pydantic-core/blob/main/src/lib.rs#L24
#[cfg(feature = "python")]
fn get_python_version() -> String {
    let version = env!("CARGO_PKG_VERSION").to_string();
    // cargo uses "1.0-alpha1" etc. while python uses "1.0.0a1", this is not full compatibility,
    // but it's good enough for now
    // see https://docs.rs/semver/1.0.9/semver/struct.Version.html#method.parse for rust spec
    // see https://peps.python.org/pep-0440/ for python spec
    // it seems the dot after "alpha/beta" e.g. "-alpha.1" is not necessary, hence why this works
    version.replace("-alpha", "a").replace("-beta", "b")
}

#[cfg(feature = "python")]
#[pymodule(gil_used = false)]
fn _kyrax(m: &Bound<'_, PyModule>) -> PyResult<()> {
    use crate::types::excelsheet::column_info::{ColumnInfo, ColumnInfoNoDtype};

    pyo3_log::init();

    let py = m.py();
    m.add_function(wrap_pyfunction!(py_read_excel, m)?)?;
    m.add_class::<ColumnInfo>()?;
    m.add_class::<ColumnInfoNoDtype>()?;
    m.add_class::<DefinedName>()?;
    m.add_class::<CellError>()?;
    m.add_class::<CellErrors>()?;
    m.add_class::<ExcelSheet>()?;
    m.add_class::<ExcelReader>()?;
    m.add_class::<ExcelTable>()?;

    // turbo fast-path (read)
    {
        use crate::turbo::python::{
            PyTurboReader, PyTurboSheet, py_is_encrypted, py_read_excel_turbo,
        };
        m.add_function(wrap_pyfunction!(py_read_excel_turbo, m)?)?;
        m.add_function(wrap_pyfunction!(py_is_encrypted, m)?)?;
        m.add_class::<PyTurboReader>()?;
        m.add_class::<PyTurboSheet>()?;
    }

    // C1c encrypted-workbook metadata (requires the `encryption` feature)
    #[cfg(feature = "encryption")]
    {
        use crate::turbo::python::py_encryption_info;
        m.add_function(wrap_pyfunction!(py_encryption_info, m)?)?;
    }

    // turbo write path (W1 silo A core)
    {
        use crate::turbo::write::python::{
            PyEditableSheet, PyEditableWorkbook, py_edit_excel, py_write_excel_turbo,
            py_write_excel_turbo_bytes, py_write_excel_turbo_stream,
        };
        m.add_function(wrap_pyfunction!(py_write_excel_turbo, m)?)?;
        m.add_function(wrap_pyfunction!(py_write_excel_turbo_stream, m)?)?;
        m.add_function(wrap_pyfunction!(py_write_excel_turbo_bytes, m)?)?;
        m.add_function(wrap_pyfunction!(py_edit_excel, m)?)?;
        m.add_class::<PyEditableWorkbook>()?;
        m.add_class::<PyEditableSheet>()?;
    }

    // C2 validate & repair
    {
        use crate::turbo::validate::python::{py_repair_excel, py_validate_excel};
        m.add_function(wrap_pyfunction!(py_validate_excel, m)?)?;
        m.add_function(wrap_pyfunction!(py_repair_excel, m)?)?;
    }

    // Phase 3 features: the Tier 3 MEDIUM/LOW capabilities neither kyrax nor
    // openpyxl held before. Reachable from Python because an engine capability
    // nobody can call does not count as shipped.
    {
        use crate::turbo::features::python_inventory::{
            py_control_parts, py_external_links, py_feature_parts, py_is_signed,
            py_power_query_inventory, py_rich_data_parts, py_signature_info, py_slicer_inventory,
        };
        m.add_function(wrap_pyfunction!(py_slicer_inventory, m)?)?;
        m.add_function(wrap_pyfunction!(py_rich_data_parts, m)?)?;
        m.add_function(wrap_pyfunction!(py_power_query_inventory, m)?)?;
        m.add_function(wrap_pyfunction!(py_is_signed, m)?)?;
        m.add_function(wrap_pyfunction!(py_signature_info, m)?)?;
        m.add_function(wrap_pyfunction!(py_control_parts, m)?)?;
        m.add_function(wrap_pyfunction!(py_external_links, m)?)?;
        m.add_function(wrap_pyfunction!(py_feature_parts, m)?)?;

        use crate::turbo::features::python_query::{
            py_diff_parts, py_diff_workbooks, py_read_threaded_comments, py_write_threaded_comments,
        };
        m.add_function(wrap_pyfunction!(py_diff_parts, m)?)?;
        m.add_function(wrap_pyfunction!(py_diff_workbooks, m)?)?;
        m.add_function(wrap_pyfunction!(py_read_threaded_comments, m)?)?;
        m.add_function(wrap_pyfunction!(py_write_threaded_comments, m)?)?;

        use crate::turbo::features::python_sparkline::{
            py_dependency_query, py_read_sparklines, py_splice_sparklines,
        };
        m.add_function(wrap_pyfunction!(py_read_sparklines, m)?)?;
        m.add_function(wrap_pyfunction!(py_splice_sparklines, m)?)?;
        m.add_function(wrap_pyfunction!(py_dependency_query, m)?)?;
    }

    m.add("__version__", get_python_version())?;

    // errors
    [
        ("KyraxError", py.get_type::<py_errors::KyraxError>()),
        (
            "UnsupportedColumnTypeCombinationError",
            py.get_type::<py_errors::UnsupportedColumnTypeCombinationError>(),
        ),
        (
            "CannotRetrieveCellDataError",
            py.get_type::<py_errors::CannotRetrieveCellDataError>(),
        ),
        (
            "CalamineCellError",
            py.get_type::<py_errors::CalamineCellError>(),
        ),
        ("CalamineError", py.get_type::<py_errors::CalamineError>()),
        (
            "SheetNotFoundError",
            py.get_type::<py_errors::SheetNotFoundError>(),
        ),
        (
            "ColumnNotFoundError",
            py.get_type::<py_errors::ColumnNotFoundError>(),
        ),
        ("ArrowError", py.get_type::<py_errors::ArrowError>()),
        (
            "InvalidParametersError",
            py.get_type::<py_errors::InvalidParametersError>(),
        ),
    ]
    .into_iter()
    .try_for_each(|(exc_name, exc_type)| m.add(exc_name, exc_type))
}
