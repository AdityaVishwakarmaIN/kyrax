//! PyO3 bindings for the Phase 3 feature inventories: slicers, rich data,
//! Power Query, digital signatures, form controls and external links. Rust
//! owns every decision — the entry-name-pass detection contract (E7), the
//! per-part parsers, and the signatures *drop* list — and this file only
//! marshals a path into zip bytes and those bytes into plain Python dicts and
//! lists. No pyclasses: dicts keep the binding thin and keep every field name
//! a Rust-side decision.
//!
//! Part-level parsers that take an already-inflated part are not bound at all
//! (Python has no way to produce a part), and neither is `resolve_reference`,
//! whose `&[ExternalBook]` input is a Rust-only type. The six modules are
//! reached through their zip-level functions instead.
//!
//! WIRING: the coordinator applies these; this file ships no `cfg` of its own.
//! WIRING:
//! WIRING: 1. src/turbo/features/mod.rs — add:
//! WIRING:        #[cfg(feature = "python")]
//! WIRING:        pub mod python_inventory;
//! WIRING:
//! WIRING: 2. src/lib.rs, inside the _kyrax pymodule (after the C2 validate block):
//! WIRING:        // Phase 3 feature inventories
//! WIRING:        {
//! WIRING:            use crate::turbo::features::python_inventory::{
//! WIRING:                py_control_parts, py_external_links, py_feature_parts, py_is_signed,
//! WIRING:                py_power_query_inventory, py_rich_data_parts, py_signature_info,
//! WIRING:                py_slicer_inventory,
//! WIRING:            };
//! WIRING:            m.add_function(wrap_pyfunction!(py_slicer_inventory, m)?)?;
//! WIRING:            m.add_function(wrap_pyfunction!(py_rich_data_parts, m)?)?;
//! WIRING:            m.add_function(wrap_pyfunction!(py_power_query_inventory, m)?)?;
//! WIRING:            m.add_function(wrap_pyfunction!(py_is_signed, m)?)?;
//! WIRING:            m.add_function(wrap_pyfunction!(py_signature_info, m)?)?;
//! WIRING:            m.add_function(wrap_pyfunction!(py_control_parts, m)?)?;
//! WIRING:            m.add_function(wrap_pyfunction!(py_external_links, m)?)?;
//! WIRING:            m.add_function(wrap_pyfunction!(py_feature_parts, m)?)?;
//! WIRING:        }
//! WIRING:
//! WIRING: (Optional, only if the coordinator wants the `feature_parts`
//! WIRING:  external_links entry driven entirely by Rust: add an
//! WIRING:  `external_link_part_names` fn to external_links.rs. Until then the
//! WIRING:  helper here re-lists `xl/externalLinks/*` — same prefix the Rust
//! WIRING:  module uses internally.)

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use crate::turbo::error::TurboError;
use crate::turbo::features::controls::control_part_names;
use crate::turbo::features::external_links::{CachedCell, ExternalBook, load_external_books};
use crate::turbo::features::power_query::{
    Connection, PowerQueryInventory, inventory_power_query, power_query_part_names,
};
use crate::turbo::features::rich_values::rich_data_part_names;
use crate::turbo::features::signatures::{
    SignatureInfo, detect_signatures, is_signed, signature_part_names,
};
use crate::turbo::features::slicers::{
    SlicerInventory, SlicerRef, inventory_slicers, slicer_part_names,
};

fn turbo_err_to_py(err: TurboError) -> PyErr {
    let fe: crate::error::KyraxError =
        crate::error::KyraxErrorKind::Internal(err.to_string()).into();
    fe.into()
}

/// Read a workbook file, releasing the GIL for the I/O and the whole inventory
/// pass, and surface any failure (missing file, not a zip, corrupt entry) as a
/// Python exception rather than a panic.
fn read_workbook_bytes<'py>(py: Python<'py>, path: &str) -> PyResult<Vec<u8>> {
    py.detach(|| std::fs::read(path).map_err(TurboError::from))
        .map_err(turbo_err_to_py)
}

// ---------------------------------------------------------------------------
// Slicers / timelines
// ---------------------------------------------------------------------------

fn slicer_to_dict<'py>(py: Python<'py>, s: &SlicerRef) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("name", s.name.as_str())?;
    d.set_item("cache", s.cache.as_str())?;
    match &s.caption {
        Some(c) => d.set_item("caption", c.as_str())?,
        None => d.set_item("caption", py.None())?,
    }
    match s.column_count {
        Some(n) => d.set_item("column_count", n)?,
        None => d.set_item("column_count", py.None())?,
    }
    Ok(d)
}

fn slicer_inventory_to_dict<'py>(
    py: Python<'py>,
    inv: &SlicerInventory,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);

    let mut slicers = Vec::with_capacity(inv.slicers.len());
    for s in &inv.slicers {
        slicers.push(slicer_to_dict(py, s)?);
    }
    d.set_item("slicers", PyList::new(py, slicers)?)?;

    let mut timelines = Vec::with_capacity(inv.timelines.len());
    for t in &inv.timelines {
        let td = PyDict::new(py);
        td.set_item("name", t.name.as_str())?;
        td.set_item("cache", t.cache.as_str())?;
        match &t.caption {
            Some(c) => td.set_item("caption", c.as_str())?,
            None => td.set_item("caption", py.None())?,
        }
        timelines.push(td);
    }
    d.set_item("timelines", PyList::new(py, timelines)?)?;

    let mut caches = Vec::with_capacity(inv.slicer_caches.len());
    for c in &inv.slicer_caches {
        let cd = PyDict::new(py);
        cd.set_item("name", c.name.as_str())?;
        match &c.source_name {
            Some(s) => cd.set_item("source_name", s.as_str())?,
            None => cd.set_item("source_name", py.None())?,
        }
        cd.set_item(
            "pivot_tables",
            PyList::new(py, c.pivot_tables.iter().map(String::as_str))?,
        )?;
        caches.push(cd);
    }
    d.set_item("slicer_caches", PyList::new(py, caches)?)?;

    d.set_item(
        "timeline_cache_names",
        PyList::new(py, inv.timeline_cache_names.iter().map(String::as_str))?,
    )?;
    d.set_item(
        "sheet_slicer_refs",
        PyList::new(py, inv.sheet_slicer_refs.iter().map(String::as_str))?,
    )?;
    Ok(d)
}

/// Inventory the slicer and timeline parts of a workbook file.
///
/// Returns `{slicers, timelines, slicer_caches, timeline_cache_names,
/// sheet_slicer_refs}`; each slicer is `{name, cache, caption, column_count}`,
/// each timeline `{name, cache, caption}`, each slicer cache `{name,
/// source_name, pivot_tables}`. Detection costs one entry-name pass and only
/// the parts that exist are inflated. Raises on a missing file or a corrupt
/// archive; never panics.
#[pyfunction(name = "slicer_inventory")]
pub fn py_slicer_inventory<'py>(py: Python<'py>, path: &str) -> PyResult<Bound<'py, PyDict>> {
    let zip = read_workbook_bytes(py, path)?;
    let inv = py
        .detach(move || inventory_slicers(&zip))
        .map_err(turbo_err_to_py)?;
    slicer_inventory_to_dict(py, &inv)
}

// ---------------------------------------------------------------------------
// Rich data
// ---------------------------------------------------------------------------

/// List the rich-data pass-through parts of a workbook file.
///
/// Returns every `xl/richData/*` entry plus `xl/metadata.xml` when present —
/// the parts the byte-preserving edit path must carry through untouched. A
/// workbook with no rich data returns an empty list. (`parse_value_metadata`,
/// which maps a cell's `vm` index into `xl/metadata.xml`, is not bound: it
/// takes an already-inflated part, which Python has no way to produce.) Raises
/// on a missing file or a corrupt archive; never panics.
#[pyfunction(name = "rich_data_parts")]
pub fn py_rich_data_parts<'py>(py: Python<'py>, path: &str) -> PyResult<Bound<'py, PyList>> {
    let zip = read_workbook_bytes(py, path)?;
    let parts = py
        .detach(move || rich_data_part_names(&zip))
        .map_err(turbo_err_to_py)?;
    PyList::new(py, parts.iter().map(String::as_str))
}

// ---------------------------------------------------------------------------
// Power Query / data model
// ---------------------------------------------------------------------------

fn connection_to_dict<'py>(py: Python<'py>, c: &Connection) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("id", c.id.as_str())?;
    d.set_item("name", c.name.as_str())?;
    match &c.kind {
        Some(k) => d.set_item("kind", k.as_str())?,
        None => d.set_item("kind", py.None())?,
    }
    match &c.command {
        Some(cmd) => d.set_item("command", cmd.as_str())?,
        None => d.set_item("command", py.None())?,
    }
    d.set_item("is_power_query", c.is_power_query)?;
    Ok(d)
}

fn pq_inventory_to_dict<'py>(
    py: Python<'py>,
    inv: &PowerQueryInventory,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    let mut conns = Vec::with_capacity(inv.connections.len());
    for c in &inv.connections {
        conns.push(connection_to_dict(py, c)?);
    }
    d.set_item("connections", PyList::new(py, conns)?)?;
    d.set_item("has_data_mashup", inv.has_data_mashup)?;
    d.set_item(
        "custom_xml_parts",
        PyList::new(py, inv.custom_xml_parts.iter().map(String::as_str))?,
    )?;
    d.set_item(
        "query_table_parts",
        PyList::new(py, inv.query_table_parts.iter().map(String::as_str))?,
    )?;
    d.set_item(
        "model_parts",
        PyList::new(py, inv.model_parts.iter().map(String::as_str))?,
    )?;
    Ok(d)
}

/// Inventory the Power Query and data-model parts of a workbook file.
///
/// Returns `{connections, has_data_mashup, custom_xml_parts,
/// query_table_parts, model_parts}`; each connection is `{id, name, kind,
/// command, is_power_query}`. `is_power_query` is true when the `dbPr`
/// connection string names `Microsoft.Mashup`. One entry-name pass; only
/// `xl/connections.xml` and `customXml/item*.xml` are inflated, and only when
/// present. Raises on a missing file or a corrupt archive; never panics.
#[pyfunction(name = "power_query_inventory")]
pub fn py_power_query_inventory<'py>(py: Python<'py>, path: &str) -> PyResult<Bound<'py, PyDict>> {
    let zip = read_workbook_bytes(py, path)?;
    let inv = py
        .detach(move || inventory_power_query(&zip))
        .map_err(turbo_err_to_py)?;
    pq_inventory_to_dict(py, &inv)
}

// ---------------------------------------------------------------------------
// Digital signatures
// ---------------------------------------------------------------------------

/// Is the workbook digitally signed?
///
/// Cheap entry-name-only check: signature parts live under `_xmlsignatures/`,
/// so presence is decided with no inflate at all. Raises on a missing file or
/// a corrupt archive; never panics.
#[pyfunction(name = "is_signed_workbook")]
pub fn py_is_signed(py: Python<'_>, path: &str) -> PyResult<bool> {
    let zip = read_workbook_bytes(py, path)?;
    py.detach(move || is_signed(&zip)).map_err(turbo_err_to_py)
}

fn signature_to_dict<'py>(py: Python<'py>, s: &SignatureInfo) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("part_name", s.part_name.as_str())?;
    match &s.signed_at {
        Some(t) => d.set_item("signed_at", t.as_str())?,
        None => d.set_item("signed_at", py.None())?,
    }
    match &s.signer_hint {
        Some(h) => d.set_item("signer_hint", h.as_str())?,
        None => d.set_item("signer_hint", py.None())?,
    }
    Ok(d)
}

/// Detect every digital-signature part and report best-effort metadata.
///
/// Returns a list of `{part_name, signed_at, signer_hint}`. Fast path: a
/// workbook with no `_xmlsignatures/` entry costs one name pass and returns an
/// empty list — nothing is inflated. A corrupt signature part is skipped
/// best-effort, not fatal. Raises on a missing file or a corrupt archive;
/// never panics.
#[pyfunction(name = "signature_info")]
pub fn py_signature_info<'py>(py: Python<'py>, path: &str) -> PyResult<Bound<'py, PyList>> {
    let zip = read_workbook_bytes(py, path)?;
    let sigs = py
        .detach(move || detect_signatures(&zip))
        .map_err(turbo_err_to_py)?;
    let mut items = Vec::with_capacity(sigs.len());
    for s in &sigs {
        items.push(signature_to_dict(py, s)?);
    }
    PyList::new(py, items)
}

// ---------------------------------------------------------------------------
// Form / ActiveX / OLE controls
// ---------------------------------------------------------------------------

/// List the form-control, ActiveX and embedded-OLE parts of a workbook file.
///
/// Returns every entry under `xl/ctrlProps/*`, `xl/activeX/*` and
/// `xl/embeddings/*` for the byte-preserving edit path to carry through
/// untouched. One entry-name pass, no inflation. Raises on a missing file or a
/// corrupt archive; never panics.
#[pyfunction(name = "control_parts")]
pub fn py_control_parts<'py>(py: Python<'py>, path: &str) -> PyResult<Bound<'py, PyList>> {
    let zip = read_workbook_bytes(py, path)?;
    let parts = py
        .detach(move || control_part_names(&zip))
        .map_err(turbo_err_to_py)?;
    PyList::new(py, parts.iter().map(String::as_str))
}

// ---------------------------------------------------------------------------
// External links
// ---------------------------------------------------------------------------

fn cached_cell_to_dict<'py>(py: Python<'py>, c: &CachedCell) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("sheet_id", c.sheet_id)?;
    d.set_item("cell", c.cell.as_str())?;
    d.set_item("value", c.value.as_str())?;
    match &c.kind {
        Some(k) => d.set_item("kind", k.as_str())?,
        None => d.set_item("kind", py.None())?,
    }
    Ok(d)
}

fn book_to_dict<'py>(py: Python<'py>, b: &ExternalBook) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("index", b.index)?;
    match &b.target {
        Some(t) => d.set_item("target", t.as_str())?,
        None => d.set_item("target", py.None())?,
    }
    d.set_item(
        "sheet_names",
        PyList::new(py, b.sheet_names.iter().map(String::as_str))?,
    )?;
    let mut names = Vec::with_capacity(b.defined_names.len());
    for (name, refers_to) in &b.defined_names {
        let nd = PyDict::new(py);
        nd.set_item("name", name.as_str())?;
        nd.set_item("refers_to", refers_to.as_str())?;
        names.push(nd);
    }
    d.set_item("defined_names", PyList::new(py, names)?)?;
    let mut cached = Vec::with_capacity(b.cached.len());
    for c in &b.cached {
        cached.push(cached_cell_to_dict(py, c)?);
    }
    d.set_item("cached", PyList::new(py, cached)?)?;
    Ok(d)
}

/// Load every external book referenced by a workbook file.
///
/// Returns a list of `{index, target, sheet_names, defined_names, cached}`;
/// each cached cell is `{sheet_id, cell, value, kind}`. `target` is the
/// resolved `externalLinkPath` relationship, `index` comes from the filename
/// digits (`externalLink3.xml` → 3), and the cached values let a formula open
/// without the other workbook present. (`resolve_reference` is not bound: it
/// takes a Rust-only `&[ExternalBook]`, which Python has no way to construct;
/// the same lookup falls out of this dict list.) Raises on a missing file or a
/// corrupt archive; never panics.
#[pyfunction(name = "external_links")]
pub fn py_external_links<'py>(py: Python<'py>, path: &str) -> PyResult<Bound<'py, PyList>> {
    let zip = read_workbook_bytes(py, path)?;
    let books = py
        .detach(move || load_external_books(&zip))
        .map_err(turbo_err_to_py)?;
    let mut items = Vec::with_capacity(books.len());
    for b in &books {
        items.push(book_to_dict(py, b)?);
    }
    PyList::new(py, items)
}

// ---------------------------------------------------------------------------
// Cross-module part inventory
// ---------------------------------------------------------------------------

/// Entry names under `xl/externalLinks/`, sorted, for the uniform
/// category → part-names shape of [`py_feature_parts`].
///
/// external_links.rs exposes no part-names function — its inventory is the
/// parsed books, not the raw entries — so this helper re-lists the central
/// directory once (the E7 entry-name pass, no inflate) using the same prefix
/// `load_external_books` filters on internally. Pure marshalling; the
/// spreadsheet semantics stay in the Rust module that invented the prefix.
fn external_link_part_names(zip_bytes: &[u8]) -> crate::turbo::error::TurboResult<Vec<String>> {
    let (entries, _errors) = crate::turbo::zipmin::list_entries(zip_bytes)?;
    let mut out: Vec<String> = entries
        .into_iter()
        .filter(|e| e.name.starts_with("xl/externalLinks/"))
        .map(|e| e.name)
        .collect();
    out.sort();
    Ok(out)
}

/// Inventory the pass-through parts of all six feature areas in one call.
///
/// Returns one dict mapping category name → list of part names:
/// `{slicers, rich_data, power_query, signatures, controls, external_links}`.
/// The `signatures` list is a **DROP list**, not a preserve list: a signature
/// is a cryptographic binding over the bytes of the parts it covers, so once
/// those bytes change the binding is broken, and carrying a now-invalid
/// signature forward makes Excel flag the file as tampered — strictly worse
/// than an honest unsigned file. Every other category is a preserve list for
/// the byte-preserving edit path. Detection is all entry-name passes; nothing
/// is inflated. Raises on a missing file or a corrupt archive; never panics.
#[pyfunction(name = "feature_parts")]
pub fn py_feature_parts<'py>(py: Python<'py>, path: &str) -> PyResult<Bound<'py, PyDict>> {
    let zip = read_workbook_bytes(py, path)?;
    let (slicers, rich_data, power_query, signatures, controls, external_links) = py
        .detach(move || {
            let slicers = slicer_part_names(&zip)?;
            let rich_data = rich_data_part_names(&zip)?;
            let power_query = power_query_part_names(&zip)?;
            let signatures = signature_part_names(&zip)?;
            let controls = control_part_names(&zip)?;
            let external_links = external_link_part_names(&zip)?;
            Ok::<_, TurboError>((
                slicers,
                rich_data,
                power_query,
                signatures,
                controls,
                external_links,
            ))
        })
        .map_err(turbo_err_to_py)?;

    let d = PyDict::new(py);
    d.set_item(
        "slicers",
        PyList::new(py, slicers.iter().map(String::as_str))?,
    )?;
    d.set_item(
        "rich_data",
        PyList::new(py, rich_data.iter().map(String::as_str))?,
    )?;
    d.set_item(
        "power_query",
        PyList::new(py, power_query.iter().map(String::as_str))?,
    )?;
    d.set_item(
        "signatures",
        PyList::new(py, signatures.iter().map(String::as_str))?,
    )?;
    d.set_item(
        "controls",
        PyList::new(py, controls.iter().map(String::as_str))?,
    )?;
    d.set_item(
        "external_links",
        PyList::new(py, external_links.iter().map(String::as_str))?,
    )?;
    Ok(d)
}
