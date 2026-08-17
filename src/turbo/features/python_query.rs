//! PyO3 bindings for the Phase 3 feature queries: workbook diff and threaded
//! comments. Rust owns every decision (part-tiered diff, entry-name-pass
//! detection, deterministic writers); this file only marshals paths, dicts,
//! and bytes across the FFI boundary.
//!
//! WIRING: the coordinator applies these; this file ships no `cfg` of its own.
//! WIRING:
//! WIRING: 1. src/turbo/features/mod.rs — add:
//! WIRING:        #[cfg(feature = "python")]
//! WIRING:        pub mod python_query;
//! WIRING:
//! WIRING: 2. src/lib.rs, inside the _kyrax pymodule (after the C2 validate block):
//! WIRING:        // Phase 3 feature queries: diff + threaded comments
//! WIRING:        {
//! WIRING:            use crate::turbo::features::python_query::{
//! WIRING:                py_diff_parts, py_diff_workbooks, py_read_threaded_comments,
//! WIRING:                py_write_threaded_comments,
//! WIRING:            };
//! WIRING:            m.add_function(wrap_pyfunction!(py_diff_parts, m)?)?;
//! WIRING:            m.add_function(wrap_pyfunction!(py_diff_workbooks, m)?)?;
//! WIRING:            m.add_function(wrap_pyfunction!(py_read_threaded_comments, m)?)?;
//! WIRING:            m.add_function(wrap_pyfunction!(py_write_threaded_comments, m)?)?;
//! WIRING:        }

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};

use crate::turbo::error::TurboError;
use crate::turbo::features::diff::{ChangeKind, diff_parts, diff_workbooks};
use crate::turbo::features::threaded_comments::{
    Person, ThreadedComment, parse_persons, parse_threaded_comments, write_persons,
    write_threaded_comments,
};

fn turbo_err_to_py(err: TurboError) -> PyErr {
    let fe: crate::error::KyraxError =
        crate::error::KyraxErrorKind::Internal(err.to_string()).into();
    fe.into()
}

/// Lowercase Python-facing name of a [`ChangeKind`] variant.
fn change_kind_to_str(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Added => "added",
        ChangeKind::Removed => "removed",
        ChangeKind::ValueChanged => "value_changed",
        ChangeKind::FormulaChanged => "formula_changed",
        ChangeKind::TypeChanged => "type_changed",
    }
}

fn opt_str(d: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<String>> {
    d.get_item(key)?.map(|v| v.extract::<String>()).transpose()
}

/// A required dict key: present and a string, or a ValueError naming the key.
fn req_str(d: &Bound<'_, PyDict>, key: &str) -> PyResult<String> {
    let v = d
        .get_item(key)?
        .ok_or_else(|| PyValueError::new_err(format!("missing required key '{key}'")))?;
    v.extract::<String>()
        .map_err(|_| PyValueError::new_err(format!("key '{key}' must be a string")))
}

fn comment_to_dict<'py>(py: Python<'py>, c: &ThreadedComment) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("cell", &c.cell)?;
    d.set_item("text", &c.text)?;
    d.set_item("author_id", &c.author_id)?;
    match &c.created {
        Some(t) => d.set_item("created", t)?,
        None => d.set_item("created", py.None())?,
    }
    d.set_item("id", &c.id)?;
    match &c.parent_id {
        Some(p) => d.set_item("parent_id", p)?,
        None => d.set_item("parent_id", py.None())?,
    }
    Ok(d)
}

fn person_to_dict<'py>(py: Python<'py>, p: &Person) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("id", &p.id)?;
    d.set_item("display_name", &p.display_name)?;
    Ok(d)
}

fn parse_comment_obj(obj: &Bound<'_, PyAny>) -> PyResult<ThreadedComment> {
    let d = obj
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("each threaded comment must be a dict"))?;
    Ok(ThreadedComment {
        cell: req_str(d, "cell")?,
        text: req_str(d, "text")?,
        author_id: req_str(d, "author_id")?,
        id: req_str(d, "id")?,
        created: opt_str(d, "created")?,
        parent_id: opt_str(d, "parent_id")?,
    })
}

fn parse_person_obj(obj: &Bound<'_, PyAny>) -> PyResult<Person> {
    let d = obj
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("each person must be a dict"))?;
    Ok(Person {
        id: req_str(d, "id")?,
        display_name: req_str(d, "display_name")?,
    })
}

/// Compare the part lists of two workbook files from their zip central
/// directories.
///
/// Zero inflates: only the CRC-32 already stored in each central-directory
/// record is compared. `a` is BEFORE, `b` is AFTER; a part present only in `a`
/// is "removed", only in `b` is "added". Returns a list of `{name, kind}` where
/// `kind` is "added", "removed", or "value_changed". Raises on a missing file
/// or a corrupt archive; never panics.
#[pyfunction(name = "diff_parts")]
pub fn py_diff_parts<'py>(
    py: Python<'py>,
    a_path: &str,
    b_path: &str,
) -> PyResult<Bound<'py, PyList>> {
    let parts = py
        .detach(|| {
            let a = std::fs::read(a_path).map_err(TurboError::from)?;
            let b = std::fs::read(b_path).map_err(TurboError::from)?;
            diff_parts(&a, &b)
        })
        .map_err(turbo_err_to_py)?;
    let mut items = Vec::with_capacity(parts.len());
    for p in &parts {
        let d = PyDict::new(py);
        d.set_item("name", &p.name)?;
        d.set_item("kind", change_kind_to_str(p.kind))?;
        items.push(d);
    }
    PyList::new(py, items)
}

/// Diff two workbook files at part and cell level.
///
/// `a` is BEFORE, `b` is AFTER. A part or cell present only in `a` is
/// "removed"; present only in `b` is "added" — getting this backwards is the
/// easiest way to make a diff API lie. Returns `{identical, parts, cells}`
/// where each part is `{name, kind}` and each cell is `{sheet, cell, kind,
/// before, after}`; `before`/`after` are the value strings on the `a`/`b` side
/// (or None when the value is absent there). `kind` is one of "added",
/// "removed", "value_changed", "formula_changed", "type_changed". Only sheet
/// parts flagged by the part diff are inflated. Raises on a missing file or a
/// corrupt archive; never panics.
#[pyfunction(name = "diff_workbooks")]
pub fn py_diff_workbooks<'py>(
    py: Python<'py>,
    a_path: &str,
    b_path: &str,
) -> PyResult<Bound<'py, PyDict>> {
    let diff = py
        .detach(|| {
            let a = std::fs::read(a_path).map_err(TurboError::from)?;
            let b = std::fs::read(b_path).map_err(TurboError::from)?;
            diff_workbooks(&a, &b)
        })
        .map_err(turbo_err_to_py)?;

    let d = PyDict::new(py);
    d.set_item("identical", diff.identical)?;
    let mut parts = Vec::with_capacity(diff.parts.len());
    for p in &diff.parts {
        let pd = PyDict::new(py);
        pd.set_item("name", &p.name)?;
        pd.set_item("kind", change_kind_to_str(p.kind))?;
        parts.push(pd);
    }
    d.set_item("parts", PyList::new(py, parts)?)?;
    let mut cells = Vec::with_capacity(diff.cells.len());
    for c in &diff.cells {
        let cd = PyDict::new(py);
        cd.set_item("sheet", &c.sheet)?;
        cd.set_item("cell", &c.cell)?;
        cd.set_item("kind", change_kind_to_str(c.kind))?;
        match &c.before {
            Some(b) => cd.set_item("before", b)?,
            None => cd.set_item("before", py.None())?,
        }
        match &c.after {
            Some(a) => cd.set_item("after", a)?,
            None => cd.set_item("after", py.None())?,
        }
        cells.push(cd);
    }
    d.set_item("cells", PyList::new(py, cells)?)?;
    Ok(d)
}

/// Read every threaded-comment part and the persons author list from a
/// workbook file.
///
/// Part discovery is a zip entry-name pass (E7 contract): when the workbook has
/// no `xl/threadedComments/threadedComment*.xml` or `xl/persons/person*.xml`
/// entries, nothing is inflated and the result is two empty lists. Returns
/// `{comments, persons}` where each comment is `{cell, text, author_id,
/// created, id, parent_id}` and each person is `{id, display_name}`. Raises on
/// a missing file or a corrupt archive; never panics.
#[pyfunction(name = "read_threaded_comments")]
pub fn py_read_threaded_comments<'py>(py: Python<'py>, path: &str) -> PyResult<Bound<'py, PyDict>> {
    let (comments, persons) = py
        .detach(|| {
            let zip = std::fs::read(path).map_err(TurboError::from)?;
            let (entries, errors) = crate::turbo::zipmin::list_entries(&zip)?;
            if let Some(e) = errors.first() {
                return Err(TurboError::Format(format!(
                    "corrupt central directory: {e}"
                )));
            }
            let mut comments: Vec<ThreadedComment> = Vec::new();
            for e in &entries {
                if e.name.starts_with("xl/threadedComments/threadedComment")
                    && e.name.ends_with(".xml")
                {
                    if let Some(xml) = crate::turbo::zipmin::read_entry(&zip, &e.name)? {
                        comments.extend(parse_threaded_comments(&xml)?);
                    }
                }
            }
            let mut persons: Vec<Person> = Vec::new();
            for e in &entries {
                if e.name.starts_with("xl/persons/person") && e.name.ends_with(".xml") {
                    if let Some(xml) = crate::turbo::zipmin::read_entry(&zip, &e.name)? {
                        persons.extend(parse_persons(&xml)?);
                    }
                }
            }
            Ok((comments, persons))
        })
        .map_err(turbo_err_to_py)?;

    let d = PyDict::new(py);
    let mut c_items = Vec::with_capacity(comments.len());
    for c in &comments {
        c_items.push(comment_to_dict(py, c)?);
    }
    d.set_item("comments", PyList::new(py, c_items)?)?;
    let mut p_items = Vec::with_capacity(persons.len());
    for p in &persons {
        p_items.push(person_to_dict(py, p)?);
    }
    d.set_item("persons", PyList::new(py, p_items)?)?;
    Ok(d)
}

/// Serialize threaded comments and persons into the two XML parts.
///
/// Writes NO files — the caller decides where the parts go. Takes `comments`
/// and `persons`, two lists of dicts, and returns
/// `{threaded_comments_xml, persons_xml}` as bytes. Comment dicts marshal onto
/// the Rust model field by field: `cell`, `text`, `author_id` and `id` are
/// required (a missing one raises ValueError naming the key); `created` and
/// `parent_id` are optional. Person dicts require `id` and `display_name`.
/// Output is byte-deterministic for identical input.
#[pyfunction(name = "write_threaded_comments")]
pub fn py_write_threaded_comments<'py>(
    py: Python<'py>,
    comments: &Bound<'_, PyAny>,
    persons: &Bound<'_, PyAny>,
) -> PyResult<Bound<'py, PyDict>> {
    let comments_list = comments
        .cast::<PyList>()
        .map_err(|_| PyValueError::new_err("comments must be a list of dicts"))?;
    let persons_list = persons
        .cast::<PyList>()
        .map_err(|_| PyValueError::new_err("persons must be a list of dicts"))?;
    let mut tc = Vec::with_capacity(comments_list.len());
    for item in comments_list.iter() {
        tc.push(parse_comment_obj(&item)?);
    }
    let mut ps = Vec::with_capacity(persons_list.len());
    for item in persons_list.iter() {
        ps.push(parse_person_obj(&item)?);
    }
    let (tc_xml, ps_xml): (Vec<u8>, Vec<u8>) = py
        .detach(|| Ok::<_, TurboError>((write_threaded_comments(&tc), write_persons(&ps))))
        .map_err(turbo_err_to_py)?;
    let d = PyDict::new(py);
    d.set_item("threaded_comments_xml", PyBytes::new(py, &tc_xml))?;
    d.set_item("persons_xml", PyBytes::new(py, &ps_xml))?;
    Ok(d)
}
