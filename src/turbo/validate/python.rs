//! PyO3 bindings for C2 validate & repair. Rust owns all logic; this is a thin
//! dict marshalling layer (the hard architectural rule from CLAUDE.md).

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use super::{Finding, RepairOptions, Severity, ValidateReport, repair_workbook, validate_workbook};

fn turbo_err_to_py(err: crate::turbo::error::TurboError) -> PyErr {
    let fe: crate::error::KyraxError =
        crate::error::KyraxErrorKind::Internal(err.to_string()).into();
    fe.into()
}

fn finding_to_dict<'py>(py: Python<'py>, f: &Finding) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("code", f.code.as_str())?;
    d.set_item("severity", f.severity.as_str())?;
    d.set_item("part", &f.part)?;
    match &f.location {
        Some(l) => d.set_item("location", l)?,
        None => d.set_item("location", py.None())?,
    }
    d.set_item("message", &f.message)?;
    d.set_item("repairable", f.repairable)?;
    Ok(d)
}

fn report_to_dict<'py>(py: Python<'py>, report: &ValidateReport) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("valid", report.is_clean())?;
    d.set_item("errors", report.errors)?;
    d.set_item("warnings", report.warnings)?;
    d.set_item("infos", report.infos)?;
    let mut items = Vec::with_capacity(report.findings.len());
    for f in &report.findings {
        items.push(finding_to_dict(py, f)?);
    }
    d.set_item("findings", PyList::new(py, items)?)?;
    Ok(d)
}

fn action_to_dict<'py>(
    py: Python<'py>,
    code: &'static str,
    severity: &'static str,
    part: &str,
    description: &str,
    before: &str,
    after: &str,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("code", code)?;
    d.set_item("severity", severity)?;
    d.set_item("part", part)?;
    d.set_item("description", description)?;
    d.set_item("before", before)?;
    d.set_item("after", after)?;
    Ok(d)
}

/// Validate an Excel file and return a structured report.
///
/// Never raises for a bad input: a missing file, an encrypted workbook, a
/// legacy .xls, a non-zip, or a valid zip that is not OOXML all come back as
/// `findings` with distinct `code`s. Returns `{valid, errors, warnings, infos,
/// findings}` where each finding is `{code, severity, part, location, message,
/// repairable}`.
#[pyfunction(name = "validate_excel")]
pub fn py_validate_excel<'py>(py: Python<'py>, path: &str) -> PyResult<Bound<'py, PyDict>> {
    let report = py
        .detach(|| validate_workbook(path))
        .map_err(turbo_err_to_py)?;
    report_to_dict(py, &report)
}

/// Repair an Excel file conservatively, writing a corrected copy to `out_path`.
///
/// `severity` (default `"warning"`) opts into repairs for findings at that
/// severity or above: `"error"` repairs only errors, `"info"` repairs
/// everything repairable. The source file is never modified. Returns
/// `{wrote_output, report, actions}` where each action is
/// `{code, severity, part, description, before, after}`. A non-package input
/// (encrypted / legacy / not a spreadsheet / unreadable) writes nothing.
#[pyfunction(name = "repair_excel")]
#[pyo3(signature = (path, out_path, severity = "warning"))]
pub fn py_repair_excel<'py>(
    py: Python<'py>,
    path: &str,
    out_path: &str,
    severity: &str,
) -> PyResult<Bound<'py, PyDict>> {
    let opts = match severity {
        "error" => RepairOptions {
            max_severity: Severity::Error,
            ..Default::default()
        },
        "warning" => RepairOptions::default(),
        "info" => RepairOptions {
            max_severity: Severity::Info,
            ..Default::default()
        },
        other => {
            return Err(PyValueError::new_err(format!(
                "severity must be 'error' | 'warning' | 'info', got {other:?}"
            )));
        }
    };
    let (report, actions, wrote) = py
        .detach(|| repair_workbook(path, out_path, &opts))
        .map_err(turbo_err_to_py)?;

    let d = PyDict::new(py);
    d.set_item("wrote_output", wrote)?;
    d.set_item("report", report_to_dict(py, &report)?)?;
    let mut items = Vec::with_capacity(actions.len());
    for a in &actions {
        items.push(action_to_dict(
            py,
            a.code.as_str(),
            a.severity.as_str(),
            &a.part,
            &a.description,
            &a.before,
            &a.after,
        )?);
    }
    d.set_item("actions", PyList::new(py, items)?)?;
    Ok(d)
}
