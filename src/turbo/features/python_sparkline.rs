//! PyO3 bindings for sparklines and cell provenance (features/python_sparkline).
//!
//! Two Tier-3 feature areas need a live value rather than a one-shot call, so
//! they get a thin marshalling module beside the logic:
//!   * sparkline read/write (`features::sparklines`) — the one-probe sheet-part
//!     parse and the byte-preserving splice,
//!   * dependency queries (`features::provenance`) — `Provenance` borrows a
//!     `DependencyGraph` (never cloned), so it cannot cross into Python; a
//!     single function builds the graph, queries it, and returns owned tuples.
//!
//! Rust owns every rule; this file converts dicts/tuples and raises ValueError
//! for an unknown enum string. Reading the workbook is run under `py.detach`
//! so the GIL is released while the zip is on disk or a graph is being built.
//!
//! WIRING (the coordinator applies these):
//!   src/turbo/features/mod.rs:
//!     #[cfg(feature = "python")] pub mod python_sparkline;
//!   src/lib.rs, inside fn _kyrax:
//!     {
//!         use crate::turbo::features::python_sparkline::{
//!             py_dependency_query, py_read_sparklines, py_splice_sparklines,
//!         };
//!         m.add_function(wrap_pyfunction!(py_read_sparklines, m)?)?;
//!         m.add_function(wrap_pyfunction!(py_splice_sparklines, m)?)?;
//!         m.add_function(wrap_pyfunction!(py_dependency_query, m)?)?;
//!     }
//!   Accessor (being added to src/turbo/calc/mod.rs in parallel with this file;
//!   the calc hydration pass builds a graph and throws it away, so this exists):
//!     pub fn dependency_graph_for_path(
//!         path: &str,
//!     ) -> crate::turbo::error::TurboResult<crate::turbo::calc::deps::DependencyGraph>
//!     It must reuse the existing hydration entry point (calc/hydrate.rs) and
//!     return the graph that pass produces instead of discarding it.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList, PyTuple};

use super::provenance::Provenance;
use super::sparklines::{
    SparkType, Sparkline, SparklineGroup, parse_sparklines, splice_sparklines,
};
use crate::turbo::calc::deps::CellKey;
use crate::turbo::error::TurboResult;

fn turbo_err_to_py(err: crate::turbo::error::TurboError) -> PyErr {
    let fe: crate::error::KyraxError =
        crate::error::KyraxErrorKind::Internal(err.to_string()).into();
    fe.into()
}

// ---------------------------------------------------------------------------
// Sparkline groups: dict <-> SparklineGroup (one shared schema, both ways)
// ---------------------------------------------------------------------------

fn opt_str(d: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<String>> {
    d.get_item(key)?.map(|v| v.extract::<String>()).transpose()
}

fn opt_bool(d: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<bool>> {
    d.get_item(key)?.map(|v| v.extract::<bool>()).transpose()
}

fn group_to_dict<'py>(py: Python<'py>, g: &SparklineGroup) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("kind", g.kind.as_str())?;
    let mut sl = Vec::with_capacity(g.sparklines.len());
    for s in &g.sparklines {
        let sd = PyDict::new(py);
        sd.set_item("source", &s.source)?;
        sd.set_item("location", &s.location)?;
        sl.push(sd);
    }
    d.set_item("sparklines", PyList::new(py, sl)?)?;
    d.set_item("color_series", g.color_series.as_deref())?;
    d.set_item("color_negative", g.color_negative.as_deref())?;
    d.set_item("markers", g.markers)?;
    d.set_item("high", g.high)?;
    d.set_item("low", g.low)?;
    d.set_item("display_empty_as", &g.display_empty_as)?;
    Ok(d)
}

fn parse_kind(s: &str) -> PyResult<SparkType> {
    match s {
        "line" => Ok(SparkType::Line),
        "column" => Ok(SparkType::Column),
        "stacked" => Ok(SparkType::Stacked),
        other => Err(PyValueError::new_err(format!(
            "sparkline kind must be 'line' | 'column' | 'stacked'; got {other:?}"
        ))),
    }
}

fn parse_sparkline(obj: &Bound<'_, PyAny>) -> PyResult<Sparkline> {
    let d = obj
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("each sparkline must be a dict {source, location}"))?;
    Ok(Sparkline {
        source: opt_str(d, "source")?.unwrap_or_default(),
        location: opt_str(d, "location")?.unwrap_or_default(),
    })
}

fn parse_group(obj: &Bound<'_, PyAny>) -> PyResult<SparklineGroup> {
    let d = obj
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("sparkline group must be a dict"))?;
    let kind_s: String = d
        .get_item("kind")?
        .ok_or_else(|| PyValueError::new_err("sparkline group missing 'kind'"))?
        .extract()?;
    let mut sparklines = Vec::new();
    if let Some(sl) = d.get_item("sparklines")? {
        let list = sl
            .cast::<PyList>()
            .map_err(|_| PyValueError::new_err("sparkline group 'sparklines' must be a list"))?;
        for item in list.iter() {
            sparklines.push(parse_sparkline(&item)?);
        }
    }
    Ok(SparklineGroup {
        kind: parse_kind(&kind_s)?,
        sparklines,
        color_series: opt_str(d, "color_series")?,
        color_negative: opt_str(d, "color_negative")?,
        markers: opt_bool(d, "markers")?.unwrap_or(false),
        high: opt_bool(d, "high")?.unwrap_or(false),
        low: opt_bool(d, "low")?.unwrap_or(false),
        display_empty_as: opt_str(d, "display_empty_as")?.unwrap_or_else(|| "gap".into()),
    })
}

fn parse_groups(obj: &Bound<'_, PyAny>) -> PyResult<Vec<SparklineGroup>> {
    let list = obj
        .cast::<PyList>()
        .map_err(|_| PyValueError::new_err("sparkline groups must be a list of dicts"))?;
    let mut out = Vec::with_capacity(list.len());
    for item in list.iter() {
        out.push(parse_group(&item)?);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// read_sparklines
// ---------------------------------------------------------------------------

/// Read the sparkline groups on one worksheet of an XLSX file.
///
/// `sheet_index` is 0-based workbook order and maps to the conventional
/// `xl/worksheets/sheetN.xml` part via the zip central directory. Only that
/// part is ever inflated; if it has no sparklines the sheet part gets a single
/// `memmem` probe and the call returns `[]` (absent is the common case and must
/// cost one probe, never an inflate — PERF_EXPERIMENTS_PHASE3 E7). Returns a
/// list of `{kind, sparklines, color_series, color_negative, markers, high,
/// low, display_empty_as}` dicts.
#[pyfunction(name = "read_sparklines")]
pub fn py_read_sparklines<'py>(
    py: Python<'py>,
    path: &str,
    sheet_index: usize,
) -> PyResult<Bound<'py, PyList>> {
    let path = path.to_owned();
    let groups = py
        .detach(|| read_sparklines_impl(&path, sheet_index))
        .map_err(turbo_err_to_py)?;
    let mut items = Vec::with_capacity(groups.len());
    for g in &groups {
        items.push(group_to_dict(py, g)?);
    }
    PyList::new(py, items)
}

fn read_sparklines_impl(path: &str, sheet_index: usize) -> TurboResult<Vec<SparklineGroup>> {
    let zip = std::fs::read(path)?;
    // One entry-name pass through the central directory; inflates this part
    // and nothing else. A missing part is the same answer as no sparklines.
    let part = format!("xl/worksheets/sheet{}.xml", sheet_index.saturating_add(1));
    let Some(sheet_xml) = crate::turbo::zipmin::read_entry(&zip, &part)? else {
        return Ok(Vec::new());
    };
    parse_sparklines(&sheet_xml)
}

// ---------------------------------------------------------------------------
// splice_sparklines
// ---------------------------------------------------------------------------

/// Splice sparkline groups into a worksheet part, returning the new bytes.
///
/// `sheet_xml` is the raw worksheet part and `groups` a list of the same dicts
/// `read_sparklines` produces. Every byte outside the sparkline extension is
/// preserved verbatim. Runs under the GIL deliberately: the input is borrowed
/// and the edit is a single part, so copying it to release the GIL would clone
/// exactly the large buffer the performance contract forbids.
#[pyfunction(name = "splice_sparklines")]
pub fn py_splice_sparklines<'py>(
    py: Python<'py>,
    sheet_xml: &[u8],
    groups: &Bound<'_, PyAny>,
) -> PyResult<Bound<'py, PyBytes>> {
    let groups = parse_groups(groups)?;
    let out = splice_sparklines(sheet_xml, &groups).map_err(turbo_err_to_py)?;
    Ok(PyBytes::new(py, &out))
}

// ---------------------------------------------------------------------------
// dependency_query
// ---------------------------------------------------------------------------

/// The dependency queries `Provenance` answers, by name.
#[derive(Clone, Copy)]
enum QueryMode {
    Precedents,
    Dependents,
    PrecedentsDeep,
    DependentsDeep,
    Impact,
    Roots,
}

fn parse_mode(s: &str) -> PyResult<QueryMode> {
    match s {
        "precedents" => Ok(QueryMode::Precedents),
        "dependents" => Ok(QueryMode::Dependents),
        "precedents_deep" => Ok(QueryMode::PrecedentsDeep),
        "dependents_deep" => Ok(QueryMode::DependentsDeep),
        "impact" => Ok(QueryMode::Impact),
        "roots" => Ok(QueryMode::Roots),
        other => Err(PyValueError::new_err(format!(
            "mode must be 'precedents' | 'dependents' | 'precedents_deep' | \
             'dependents_deep' | 'impact' | 'roots'; got {other:?}"
        ))),
    }
}

/// `(sheet, row, col)` seeds become graph keys; the graph's column is `u16`.
fn parse_cells(cells: Vec<(u32, u32, u32)>) -> PyResult<Vec<CellKey>> {
    let mut out = Vec::with_capacity(cells.len());
    for (sheet, row, col) in cells {
        let col = u16::try_from(col).map_err(|_| {
            PyValueError::new_err(format!(
                "column index {col} exceeds the Excel limit of 16383"
            ))
        })?;
        out.push(CellKey::new(sheet, row, col));
    }
    Ok(out)
}

/// Build the graph, query it, and return the owned keys. The graph and the
/// `Provenance` handle that borrows it both live and die inside this frame.
fn query_impl(path: &str, seeds: &[CellKey], mode: QueryMode) -> TurboResult<Vec<CellKey>> {
    let graph = crate::turbo::calc::dependency_graph_for_path(path)?;
    let prov = Provenance::new(&graph);
    let mut keys = match mode {
        QueryMode::Precedents => seeds.iter().flat_map(|&c| prov.precedents(c)).collect(),
        QueryMode::Dependents => seeds.iter().flat_map(|&c| prov.dependents(c)).collect(),
        QueryMode::PrecedentsDeep => seeds
            .iter()
            .flat_map(|&c| prov.precedents_deep(c))
            .collect(),
        QueryMode::DependentsDeep => seeds
            .iter()
            .flat_map(|&c| prov.dependents_deep(c))
            .collect(),
        QueryMode::Impact => prov.impact_of(seeds),
        // A whole-graph answer: the seeds are the already-changed inputs only
        // for the modes above; roots are the model's true inputs.
        QueryMode::Roots => prov.roots(),
    };
    // Per-seed results are already sorted; the union is re-sorted and
    // deduplicated so overlapping subtrees yield one row per cell.
    keys.sort_unstable();
    keys.dedup();
    Ok(keys)
}

/// Answer a dependency query over a workbook's formula graph.
///
/// `cells` is a list of `(sheet_index, row, col)` seeds (all 0-based) and
/// `mode` is `"precedents"`, `"dependents"`, `"precedents_deep"`,
/// `"dependents_deep"`, `"impact"`, or `"roots"` (roots ignores `cells`).
/// The graph is built from the file, queried through `Provenance`, and torn
/// down before returning, so Python never holds a borrow. Returns
/// `(sheet, row, col)` tuples sorted by `(sheet, row, col)`, deduplicated.
#[pyfunction(name = "dependency_query")]
pub fn py_dependency_query<'py>(
    py: Python<'py>,
    path: &str,
    cells: Vec<(u32, u32, u32)>,
    mode: &str,
) -> PyResult<Bound<'py, PyList>> {
    let mode = parse_mode(mode)?;
    let seeds = parse_cells(cells)?;
    let path = path.to_owned();
    let keys = py
        .detach(|| query_impl(&path, &seeds, mode))
        .map_err(turbo_err_to_py)?;
    let mut items = Vec::with_capacity(keys.len());
    for k in &keys {
        items.push(PyTuple::new(py, [k.sheet, k.row, u32::from(k.col)])?);
    }
    PyList::new(py, items)
}

// No unit tests here: these bindings take `Bound` values that need an
// initialised interpreter, which is why the crate's other python.rs files
// carry none either. `cargo check --features python` is the gate.
