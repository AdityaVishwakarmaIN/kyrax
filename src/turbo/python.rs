//! PyO3 bindings for the turbo fast-path reader.

use std::sync::{Arc, OnceLock};

use arrow_array::{
    Array, ArrayRef, RecordBatch, UInt32Array,
    builder::{StringBuilder, StringDictionaryBuilder, UInt32Builder},
    types::Int32Type,
};
use pyo3::{
    Bound, PyAny, PyResult, Python, exceptions::PyValueError, prelude::*,
    types::{PyDict, PyList},
};

use super::{
    a1, list_sheet_names, range_a1, read_workbook_turbo_sheet, ActivePane, Alignment,
    AutoFilterMeta,
    Border, CKind, CellError, CellRange, CfRuleRec, ChartAnchor, ChartMeta, Color, ColDim,
    DataValidationRec, DefinedName, Features, Fill, Font, FormulaColumn,
    HeaderFooterMeta, Hyperlink, LinkTarget, NameKind, NamedStyleRec, PageMarginsMeta,
    PageSetupMeta, Pane, PaneState, Person, PivotTableMeta, PrintOptionsMeta, Protection, RowDim,
    Scope, SheetComments, SheetFormat, SheetKind, SheetProtectionMeta, SheetState, SheetViewMeta,
    Side, StyleTable, Table, ThreadedComment, TurboError, VbaProject, WorkbookProps,
};
use crate::error::{KyraxError, KyraxErrorKind, py_errors::IntoPyResult};

// ---------------------------------------------------------------------------
// Feature flag parsing
// ---------------------------------------------------------------------------

fn parse_features(features: Option<&Bound<'_, PyAny>>) -> PyResult<Features> {
    let Some(obj) = features else {
        return Ok(Features::VALUES);
    };
    if let Ok(s) = obj.extract::<String>() {
        return match s.as_str() {
            "values" => Ok(Features::VALUES),
            "all" => Ok(Features::ALL),
            other => Err(PyValueError::new_err(format!(
                "unknown features string {other:?}; expected \"values\" or \"all\""
            ))),
        };
    }
    if let Ok(list) = obj.extract::<Vec<String>>() {
        let mut f = Features::VALUES;
        for name in &list {
            f |= match name.as_str() {
                "values" => Features::VALUES,
                "styles" => Features::STYLES,
                "formulas" => Features::FORMULAS,
                "merges" => Features::MERGES,
                "defined_names" => Features::DEFINED_NAMES,
                "tables" => Features::TABLES,
                "hyperlinks" => Features::HYPERLINKS,
                "comments" => Features::COMMENTS,
                "sheet_meta" => Features::SHEET_META,
                "page_setup" => Features::PAGE_SETUP,
                "workbook_meta" => Features::WORKBOOK_META,
                "validations" => Features::VALIDATIONS,
                "cond_format" => Features::COND_FORMAT,
                "charts" => Features::CHARTS,
                "pivots" => Features::PIVOTS,
                "vba" => Features::VBA,
                "all" => Features::ALL,
                other => {
                    return Err(PyValueError::new_err(format!(
                        "unknown feature {other:?}; expected one of \
                         styles|formulas|merges|defined_names|tables|hyperlinks|comments|\
                         sheet_meta|page_setup|workbook_meta|validations|cond_format|\
                         charts|pivots|vba"
                    )));
                }
            };
        }
        return Ok(f);
    }
    Err(PyValueError::new_err(
        "features must be \"all\", \"values\", or a list of feature names",
    ))
}

fn turbo_err_to_py(err: TurboError) -> PyErr {
    let fe: KyraxError = KyraxErrorKind::Internal(err.to_string()).into();
    fe.into()
}

fn arrow_err(msg: impl ToString) -> KyraxError {
    KyraxErrorKind::ArrowError(msg.to_string()).into()
}

// ---------------------------------------------------------------------------
// Color / style helpers → Python dicts
// ---------------------------------------------------------------------------

fn color_to_dict<'py>(py: Python<'py>, c: &Color) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    match c.kind {
        CKind::None => {
            d.set_item("kind", "none")?;
        }
        CKind::Auto => {
            d.set_item("kind", "auto")?;
        }
        CKind::Rgb => {
            d.set_item("kind", "rgb")?;
            d.set_item("argb", format!("{:08X}", c.val))?;
        }
        CKind::Indexed => {
            d.set_item("kind", "indexed")?;
            d.set_item("indexed", c.val)?;
        }
        CKind::Theme => {
            d.set_item("kind", "theme")?;
            d.set_item("theme", c.val)?;
            d.set_item("tint", c.tint as f64)?;
        }
    }
    Ok(d)
}

fn font_to_dict<'py>(py: Python<'py>, font: &Font) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("name", &font.name)?;
    d.set_item("size", font.sz as f64)?;
    d.set_item("bold", font.bold)?;
    d.set_item("italic", font.italic)?;
    d.set_item(
        "underline",
        font.underline
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or("none"),
    )?;
    d.set_item("color", color_to_dict(py, &font.color)?)?;
    Ok(d)
}

fn fill_to_dict<'py>(py: Python<'py>, fill: &Fill) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("pattern", &fill.pattern)?;
    d.set_item("fg", color_to_dict(py, &fill.fg)?)?;
    d.set_item("bg", color_to_dict(py, &fill.bg)?)?;
    Ok(d)
}

fn side_to_dict<'py>(py: Python<'py>, side: &Side) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    match &side.style {
        Some(s) => {
            d.set_item("style", s)?;
            d.set_item("color", color_to_dict(py, &side.color)?)?;
        }
        None => {
            d.set_item("style", py.None())?;
            d.set_item("color", py.None())?;
        }
    }
    Ok(d)
}

fn border_to_dict<'py>(py: Python<'py>, b: &Border) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("left", side_to_dict(py, &b.left)?)?;
    d.set_item("right", side_to_dict(py, &b.right)?)?;
    d.set_item("top", side_to_dict(py, &b.top)?)?;
    d.set_item("bottom", side_to_dict(py, &b.bottom)?)?;
    d.set_item("diagonal", side_to_dict(py, &b.diagonal)?)?;
    d.set_item("diagonal_up", b.diagonal_up)?;
    d.set_item("diagonal_down", b.diagonal_down)?;
    d.set_item("outline", b.outline)?;
    Ok(d)
}

fn alignment_to_dict<'py>(py: Python<'py>, a: &Alignment) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("horizontal", a.horizontal.as_deref())?;
    d.set_item("vertical", a.vertical.as_deref())?;
    d.set_item("text_rotation", a.text_rotation)?;
    d.set_item("wrap_text", a.wrap_text)?;
    d.set_item("shrink_to_fit", a.shrink_to_fit)?;
    d.set_item("indent", a.indent)?;
    d.set_item("relative_indent", a.relative_indent)?;
    d.set_item("justify_last_line", a.justify_last_line)?;
    d.set_item("reading_order", a.reading_order)?;
    Ok(d)
}

fn protection_to_dict<'py>(py: Python<'py>, p: &Protection) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("locked", p.locked)?;
    d.set_item("hidden", p.hidden)?;
    Ok(d)
}

fn style_table_to_list<'py>(py: Python<'py>, st: &StyleTable) -> PyResult<Bound<'py, PyList>> {
    let mut items = Vec::with_capacity(st.xfs.len());
    for i in 0..st.xfs.len() {
        let r = st.resolve(i as u32);
        let d = PyDict::new(py);
        d.set_item("number_format", &r.number_format)?;
        d.set_item("is_date", r.is_date)?;
        d.set_item("font", font_to_dict(py, &r.font)?)?;
        d.set_item("fill", fill_to_dict(py, &r.fill)?)?;
        d.set_item("border_id", r.border_id)?;
        d.set_item("border", border_to_dict(py, &r.border)?)?;
        d.set_item("alignment", alignment_to_dict(py, &r.alignment)?)?;
        d.set_item("protection", protection_to_dict(py, &r.protection)?)?;
        d.set_item("name", r.style_name.as_deref())?;
        items.push(d);
    }
    PyList::new(py, items)
}

fn named_styles_to_list<'py>(py: Python<'py>, styles: &[NamedStyleRec]) -> PyResult<Bound<'py, PyList>> {
    let mut items = Vec::with_capacity(styles.len());
    for ns in styles {
        let d = PyDict::new(py);
        d.set_item("name", &ns.name)?;
        d.set_item("xf_id", ns.xf_id)?;
        d.set_item("builtin_id", ns.builtin_id)?;
        d.set_item("hidden", ns.hidden)?;
        items.push(d);
    }
    PyList::new(py, items)
}

fn defined_name_to_dict<'py>(py: Python<'py>, dn: &DefinedName) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("name", &dn.name)?;
    match &dn.scope {
        Scope::Global => d.set_item("scope", py.None())?,
        Scope::Sheet(idx) => d.set_item("scope", *idx)?,
    }
    d.set_item("value", &dn.value)?;
    d.set_item("reserved", dn.reserved.as_ref().map(|s| s.as_str()))?;
    d.set_item("hidden", dn.hidden)?;
    d.set_item("external", dn.external)?;
    let kind = match dn.kind {
        NameKind::Range => "range",
        NameKind::Constant => "constant",
        NameKind::Formula => "formula",
    };
    d.set_item("kind", kind)?;
    Ok(d)
}

fn table_to_dict<'py>(py: Python<'py>, t: &Table) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("name", &t.name)?;
    d.set_item("display_name", &t.display_name)?;
    d.set_item("ref", range_a1(&t.ref_))?;
    d.set_item("header_row_count", t.header_row_count)?;
    d.set_item("totals_row_count", t.totals_row_count)?;
    let mut cols = Vec::with_capacity(t.columns.len());
    for c in &t.columns {
        let cd = PyDict::new(py);
        cd.set_item("name", &c.name)?;
        if let Some(ref f) = c.totals_fn {
            cd.set_item("totals_row_function", f)?;
        }
        if let Some(ref l) = c.totals_label {
            cd.set_item("totals_row_label", l)?;
        }
        cols.push(cd);
    }
    d.set_item("columns", PyList::new(py, cols)?)?;
    if let Some(ref style) = t.style {
        d.set_item("style_name", &style.name)?;
    }
    Ok(d)
}

fn hyperlink_to_dict<'py>(py: Python<'py>, h: &Hyperlink) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("ref", range_a1(&h.ref_))?;
    match &h.target {
        LinkTarget::External(url) => {
            d.set_item("kind", "external")?;
            d.set_item("target", url)?;
        }
        LinkTarget::Internal => {
            d.set_item("kind", "internal")?;
        }
    }
    if let Some(ref loc) = h.location {
        d.set_item("location", loc)?;
    }
    if let Some(ref disp) = h.display {
        d.set_item("display", disp)?;
    }
    if let Some(ref tip) = h.tooltip {
        d.set_item("tooltip", tip)?;
    }
    Ok(d)
}

/// Convert a Rust `RecordBatch` to a pyarrow object (same path as `to_arrow`).
#[cfg(feature = "pyarrow")]
fn record_batch_to_py<'py>(py: Python<'py>, rb: RecordBatch) -> PyResult<Bound<'py, PyAny>> {
    use arrow_pyarrow::ToPyArrow;
    use pyo3::IntoPyObjectExt;

    rb.to_pyarrow(py)
        .map_err(|err| arrow_err(err.to_string()))
        .into_pyresult()
        .and_then(|obj| obj.into_bound_py_any(py))
}

/// Sparse formulas → RecordBatch: row, col, kind, text, ref.
///
/// Single-pass materialize (no separate `records()` + `materialize_all()`).
/// Translation is eager into the utf8 builder; the win vs dict-per-cell is
/// avoiding PyO3 marshalling of hundreds of thousands of Python dicts.
fn formulas_to_batch(f: &FormulaColumn) -> Result<RecordBatch, KyraxError> {
    let rows = f.materialize_export_rows();
    let n = rows.len();

    let mut row_b = UInt32Builder::with_capacity(n);
    let mut col_b = UInt32Builder::with_capacity(n);
    let mut kind_b = StringDictionaryBuilder::<Int32Type>::new();
    // Estimate ~16 bytes average formula text; ref is sparse (array only).
    let mut text_b = StringBuilder::with_capacity(n, n.saturating_mul(16));
    let mut ref_b = StringBuilder::with_capacity(n, n.saturating_mul(4));

    for (row, col, kind, text, ref_a1) in &rows {
        row_b.append_value(*row);
        col_b.append_value(*col);
        kind_b.append_value(*kind);
        text_b.append_value(text);
        match ref_a1 {
            Some(r) => ref_b.append_value(r),
            None => ref_b.append_null(),
        }
    }

    let row_arr: ArrayRef = Arc::new(row_b.finish());
    let col_arr: ArrayRef = Arc::new(col_b.finish());
    let kind_arr: ArrayRef = Arc::new(kind_b.finish());
    let text_arr: ArrayRef = Arc::new(text_b.finish());
    let ref_arr: ArrayRef = Arc::new(ref_b.finish());

    RecordBatch::try_from_iter_with_nullable([
        ("row", row_arr, false),
        ("col", col_arr, false),
        ("kind", kind_arr, false),
        ("text", text_arr, false),
        ("ref", ref_arr, true),
    ])
    .map_err(arrow_err)
}

/// Comments → RecordBatch: ref (A1), author (dict-encoded utf8), text.
fn comments_to_batch(sc: &SheetComments) -> Result<RecordBatch, KyraxError> {
    let n = sc.comments.len();
    let mut ref_b = StringBuilder::with_capacity(n, n.saturating_mul(6));
    let mut author_b = StringDictionaryBuilder::<Int32Type>::new();
    let mut text_b = StringBuilder::with_capacity(n, n.saturating_mul(32));

    for c in &sc.comments {
        ref_b.append_value(a1(c.row, c.col));
        let author = sc
            .authors
            .get(c.author_id as usize)
            .map(|s| s.as_str())
            .unwrap_or("");
        author_b.append_value(author);
        text_b.append_value(&c.text);
    }

    let ref_arr: ArrayRef = Arc::new(ref_b.finish());
    let author_arr: ArrayRef = Arc::new(author_b.finish());
    let text_arr: ArrayRef = Arc::new(text_b.finish());

    RecordBatch::try_from_iter_with_nullable([
        ("ref", ref_arr, false),
        ("author", author_arr, false),
        ("text", text_arr, false),
    ])
    .map_err(arrow_err)
}

/// Typed error caches → RecordBatch: row, col, code.
fn cell_errors_to_batch(errs: &[CellError]) -> Result<RecordBatch, KyraxError> {
    let n = errs.len();
    let mut row_b = UInt32Builder::with_capacity(n);
    let mut col_b = UInt32Builder::with_capacity(n);
    let mut code_b = StringBuilder::with_capacity(n, n.saturating_mul(8));

    for e in errs {
        row_b.append_value(e.row);
        col_b.append_value(e.col);
        code_b.append_value(&e.code);
    }

    let row_arr: ArrayRef = Arc::new(row_b.finish());
    let col_arr: ArrayRef = Arc::new(col_b.finish());
    let code_arr: ArrayRef = Arc::new(code_b.finish());

    RecordBatch::try_from_iter_with_nullable([
        ("row", row_arr, false),
        ("col", col_arr, false),
        ("code", code_arr, false),
    ])
    .map_err(arrow_err)
}

// ---------------------------------------------------------------------------
// TurboSheet
// ---------------------------------------------------------------------------

/// One worksheet loaded via the turbo path.
///
/// Shape notes:
/// - `style_indices()` returns a **list of per-column** pyarrow `uint32` arrays
///   (one array per value column, length = `nrows`), or `None` if styles were
///   not requested.
/// - `merges()` returns A1 range strings (`"A1:B2"`).
/// - Formula `row`/`col` are **0-based data indices** (header excluded).
/// - Cached formula results live in the value columns from `to_arrow()`
///   (both-not-XOR). Typed cell errors (`t="e"`) are also listed sparsely via
///   `cell_errors()`; pure-error columns may still show the code string in
///   values, while errors mixed into numeric columns surface as null there.
#[pyclass(name = "_TurboSheet", module = "kyrax._kyrax")]
pub struct PyTurboSheet {
    name: String,
    column_names: Vec<String>,
    columns: Vec<ArrayRef>,
    nrows: usize,
    ncols: usize,
    style_indices: Option<Vec<UInt32Array>>,
    style_table: Option<StyleTable>,
    formulas: Option<FormulaColumn>,
    /// Cached Arrow export of formulas (materialize once per sheet load).
    formulas_batch: OnceLock<RecordBatch>,
    cell_errors: Vec<CellError>,
    merges: Option<Vec<CellRange>>,
    tables: Option<Vec<Table>>,
    hyperlinks: Option<Vec<Hyperlink>>,
    comments: Option<SheetComments>,
    threaded_comments: Option<Vec<ThreadedComment>>,
    charts: Option<Vec<ChartMeta>>,
    pivots: Option<Vec<PivotTableMeta>>,
    // Stream A
    sheet_state: SheetState,
    sheet_kind: SheetKind,
    row_dimensions: Option<Vec<RowDim>>,
    column_dimensions: Option<Vec<ColDim>>,
    sheet_format: Option<SheetFormat>,
    auto_filter: Option<AutoFilterMeta>,
    sheet_view: Option<SheetViewMeta>,
    protection: Option<SheetProtectionMeta>,
    page_setup: Option<PageSetupMeta>,
    page_margins: Option<PageMarginsMeta>,
    print_options: Option<PrintOptionsMeta>,
    header_footer: Option<HeaderFooterMeta>,
    code_name: Option<String>,
    tab_color: Option<String>,
    // Stream B
    data_validations: Option<Vec<DataValidationRec>>,
    cf_rules: Option<Vec<CfRuleRec>>,
}

impl PyTurboSheet {
    fn from_parts(
        sheet: super::TurboSheet,
        style_table: Option<StyleTable>,
    ) -> Self {
        Self {
            name: sheet.name,
            column_names: sheet.column_names,
            columns: sheet.columns,
            nrows: sheet.nrows,
            ncols: sheet.ncols,
            style_indices: sheet.style_indices,
            style_table,
            formulas: sheet.formulas,
            formulas_batch: OnceLock::new(),
            cell_errors: sheet.cell_errors,
            merges: sheet.merges,
            tables: sheet.tables,
            hyperlinks: sheet.hyperlinks,
            comments: sheet.comments,
            threaded_comments: sheet.threaded_comments,
            charts: sheet.charts,
            pivots: sheet.pivots,
            sheet_state: sheet.sheet_state,
            sheet_kind: sheet.sheet_kind,
            row_dimensions: sheet.row_dimensions,
            column_dimensions: sheet.column_dimensions,
            sheet_format: sheet.sheet_format,
            auto_filter: sheet.auto_filter,
            sheet_view: sheet.sheet_view,
            protection: sheet.protection,
            page_setup: sheet.page_setup,
            page_margins: sheet.page_margins,
            print_options: sheet.print_options,
            header_footer: sheet.header_footer,
            code_name: sheet.code_name,
            tab_color: sheet.tab_color,
            data_validations: sheet.data_validations,
            cf_rules: sheet.cf_rules,
        }
    }
}

#[pymethods]
impl PyTurboSheet {
    #[getter]
    fn name(&self) -> &str {
        &self.name
    }

    #[getter]
    fn nrows(&self) -> usize {
        self.nrows
    }

    #[getter]
    fn ncols(&self) -> usize {
        self.ncols
    }

    #[getter]
    fn column_names(&self) -> Vec<String> {
        self.column_names.clone()
    }

    /// Values as a pyarrow `RecordBatch`. Strings may be dictionary-encoded.
    ///
    /// Formula cells contribute their cached calculated `<v>` values here
    /// (both-not-XOR with the formula text from `formulas()`).
    #[cfg(feature = "pyarrow")]
    fn to_arrow<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        use arrow_array::RecordBatchOptions;
        use arrow_pyarrow::ToPyArrow;
        use arrow_schema::Schema;
        use pyo3::IntoPyObjectExt;
        use std::sync::Arc;

        let names = self.column_names.clone();
        let cols = self.columns.clone();
        let nrows = self.nrows;
        let rb = py
            .detach(|| {
                // Empty sheet (0 columns): RecordBatch needs an explicit row count.
                if cols.is_empty() {
                    return RecordBatch::try_new_with_options(
                        Arc::new(Schema::empty()),
                        vec![],
                        &RecordBatchOptions::new().with_row_count(Some(nrows)),
                    )
                    .map_err(arrow_err);
                }
                RecordBatch::try_from_iter_with_nullable(names.into_iter().zip(cols).map(
                    |(name, arr)| {
                        let nullable = arr.null_count() > 0 || arr.len() == 0;
                        (name, arr, nullable)
                    },
                ))
                .map_err(arrow_err)
            })
            .into_pyresult()?;

        rb.to_pyarrow(py)
            .map_err(|err| arrow_err(err.to_string()))
            .into_pyresult()
            .and_then(|obj| obj.into_bound_py_any(py))
    }

    /// Per-column style xf indices, or None if styles not requested.
    ///
    /// Shape: `list[list[int]]` with length `ncols`; each inner list has length
    /// `nrows` (matches Rust `Vec<UInt32Array>` layout).
    fn style_indices(&self) -> Option<Vec<Vec<u32>>> {
        self.style_indices.as_ref().map(|cols| {
            cols.iter()
                .map(|arr| {
                    (0..arr.len())
                        .map(|i| {
                            if arr.is_null(i) {
                                0
                            } else {
                                arr.value(i)
                            }
                        })
                        .collect()
                })
                .collect()
        })
    }

    /// Resolved cellXfs as a list of dicts (one per xf), or None if styles not loaded.
    fn style_table<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyList>>> {
        match &self.style_table {
            Some(st) => Ok(Some(style_table_to_list(py, st)?)),
            None => Ok(None),
        }
    }

    /// Sparse formulas as a pyarrow `RecordBatch`; None if not requested.
    ///
    /// Columns: `row` (uint32), `col` (uint32), `kind` (dict utf8:
    /// plain|shared|array|dataTable), `text` (utf8, shared translated),
    /// `ref` (utf8, null unless array). row/col are 0-based data indices
    /// (header excluded).
    #[cfg(feature = "pyarrow")]
    fn formulas<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        match &self.formulas {
            Some(f) => {
                // Cache the RecordBatch: materialize/translate once per load_sheet.
                if let Some(rb) = self.formulas_batch.get() {
                    return Ok(Some(record_batch_to_py(py, rb.clone())?));
                }
                let rb = py
                    .detach(|| formulas_to_batch(f))
                    .into_pyresult()?;
                let _ = self.formulas_batch.set(rb.clone());
                Ok(Some(record_batch_to_py(py, rb)?))
            }
            None => Ok(None),
        }
    }

    /// Sparse typed error caches (`t="e"`) as a pyarrow `RecordBatch`.
    ///
    /// Columns: `row`, `col` (uint32, 0-based data indices), `code` (utf8).
    /// Always collected on the value path (empty batch when no error cells).
    #[cfg(feature = "pyarrow")]
    fn cell_errors<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let rb = py
            .detach(|| cell_errors_to_batch(&self.cell_errors))
            .into_pyresult()?;
        record_batch_to_py(py, rb)
    }

    /// Merged ranges as A1 strings (`"A1:B2"`); None if merges not requested.
    fn merges(&self) -> Option<Vec<String>> {
        self.merges
            .as_ref()
            .map(|m| m.iter().map(range_a1).collect())
    }

    /// Hyperlinks as dicts; None if not requested.
    fn hyperlinks<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyList>>> {
        match &self.hyperlinks {
            Some(hs) => {
                let mut items = Vec::with_capacity(hs.len());
                for h in hs {
                    items.push(hyperlink_to_dict(py, h)?);
                }
                Ok(Some(PyList::new(py, items)?))
            }
            None => Ok(None),
        }
    }

    /// Comments as a pyarrow `RecordBatch`; None if not requested.
    ///
    /// Columns: `ref` (utf8 A1), `author` (dict utf8), `text` (utf8).
    #[cfg(feature = "pyarrow")]
    fn comments<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        match &self.comments {
            Some(sc) => {
                let rb = py
                    .detach(|| comments_to_batch(sc))
                    .into_pyresult()?;
                Ok(Some(record_batch_to_py(py, rb)?))
            }
            None => Ok(None),
        }
    }

    /// Comment author list (when comments were requested); else None.
    fn comment_authors(&self) -> Option<Vec<String>> {
        self.comments.as_ref().map(|c| c.authors.clone())
    }

    /// True when legacy comments are Excel mirrors of threaded comments on this sheet.
    #[getter]
    fn legacy_is_mirror(&self) -> bool {
        self.comments
            .as_ref()
            .map(|c| c.legacy_is_mirror)
            .unwrap_or(false)
    }

    /// Threaded comments (Office 2018+); None if comments not requested.
    fn threaded_comments<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyList>>> {
        match &self.threaded_comments {
            Some(ts) => {
                let mut items = Vec::with_capacity(ts.len());
                for t in ts {
                    items.push(threaded_comment_to_dict(py, t)?);
                }
                Ok(Some(PyList::new(py, items)?))
            }
            None => Ok(None),
        }
    }

    /// Chart metadata on this sheet; None if charts not requested.
    fn charts<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyList>>> {
        match &self.charts {
            Some(cs) => {
                let mut items = Vec::with_capacity(cs.len());
                for c in cs {
                    items.push(chart_to_dict(py, c)?);
                }
                Ok(Some(PyList::new(py, items)?))
            }
            None => Ok(None),
        }
    }

    /// Pivot table metadata on this sheet; None if pivots not requested.
    fn pivots<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyList>>> {
        match &self.pivots {
            Some(ps) => {
                let mut items = Vec::with_capacity(ps.len());
                for p in ps {
                    items.push(pivot_to_dict(py, p)?);
                }
                Ok(Some(PyList::new(py, items)?))
            }
            None => Ok(None),
        }
    }

    /// Tables on this sheet; None if tables not requested.
    fn tables<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyList>>> {
        match &self.tables {
            Some(ts) => {
                let mut items = Vec::with_capacity(ts.len());
                for t in ts {
                    items.push(table_to_dict(py, t)?);
                }
                Ok(Some(PyList::new(py, items)?))
            }
            None => Ok(None),
        }
    }

    /// Sheet visibility: `"visible"` | `"hidden"` | `"veryHidden"`.
    #[getter]
    fn sheet_state(&self) -> &'static str {
        self.sheet_state.as_str()
    }

    /// `"worksheet"` or `"chartsheet"`.
    #[getter]
    fn sheet_kind(&self) -> &'static str {
        self.sheet_kind.as_str()
    }

    /// Explicitly-set row dimensions as `{row_index: {height, hidden, ...}}` (1-based keys).
    fn row_dimensions<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
        match &self.row_dimensions {
            Some(rows) => {
                let d = PyDict::new(py);
                for rd in rows {
                    let rd_d = PyDict::new(py);
                    rd_d.set_item("height", rd.height)?;
                    rd_d.set_item("hidden", rd.hidden)?;
                    rd_d.set_item("outline_level", rd.outline_level)?;
                    rd_d.set_item("collapsed", rd.collapsed)?;
                    rd_d.set_item("style", rd.style)?;
                    d.set_item(rd.row, rd_d)?;
                }
                Ok(Some(d))
            }
            None => Ok(None),
        }
    }

    /// Column dimensions as list of dicts with min/max/width/hidden/...
    fn column_dimensions<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyList>>> {
        match &self.column_dimensions {
            Some(cols) => {
                let mut items = Vec::with_capacity(cols.len());
                for cd in cols {
                    let d = PyDict::new(py);
                    d.set_item("min", cd.min)?;
                    d.set_item("max", cd.max)?;
                    d.set_item("width", cd.width)?;
                    d.set_item("hidden", cd.hidden)?;
                    d.set_item("best_fit", cd.best_fit)?;
                    d.set_item("outline_level", cd.outline_level)?;
                    d.set_item("collapsed", cd.collapsed)?;
                    d.set_item("style", cd.style)?;
                    items.push(d);
                }
                Ok(Some(PyList::new(py, items)?))
            }
            None => Ok(None),
        }
    }

    fn sheet_format<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
        match &self.sheet_format {
            Some(sf) => {
                let d = PyDict::new(py);
                d.set_item("base_col_width", sf.base_col_width)?;
                d.set_item("default_col_width", sf.default_col_width)?;
                d.set_item("default_row_height", sf.default_row_height)?;
                d.set_item("custom_height", sf.custom_height)?;
                d.set_item("zero_height", sf.zero_height)?;
                d.set_item("outline_level_row", sf.outline_level_row)?;
                d.set_item("outline_level_col", sf.outline_level_col)?;
                Ok(Some(d))
            }
            None => Ok(None),
        }
    }

    fn auto_filter<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
        match &self.auto_filter {
            Some(af) => {
                let d = PyDict::new(py);
                d.set_item("ref", range_a1(&af.ref_))?;
                let mut cols = Vec::new();
                for c in &af.columns {
                    let cd = PyDict::new(py);
                    cd.set_item("col_id", c.col_id)?;
                    cd.set_item("hidden_button", c.hidden_button)?;
                    cd.set_item("show_button", c.show_button)?;
                    cd.set_item("values", c.values.clone())?;
                    cd.set_item("blank", c.blank)?;
                    cols.push(cd);
                }
                d.set_item("columns", PyList::new(py, cols)?)?;
                Ok(Some(d))
            }
            None => Ok(None),
        }
    }

    fn sheet_view<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
        match &self.sheet_view {
            Some(sv) => Ok(Some(sheet_view_to_dict(py, sv)?)),
            None => Ok(None),
        }
    }

    fn freeze_panes(&self) -> Option<String> {
        self.sheet_view.as_ref().and_then(|sv| {
            sv.pane.as_ref().and_then(|p| {
                if matches!(p.state, PaneState::Frozen | PaneState::FrozenSplit) {
                    p.top_left_cell.map(|(r, c)| a1(r, c))
                } else {
                    None
                }
            })
        })
    }

    fn protection<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
        match &self.protection {
            Some(p) => Ok(Some(protection_meta_to_dict(py, p)?)),
            None => Ok(None),
        }
    }

    fn page_setup<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
        match &self.page_setup {
            Some(ps) => {
                let d = PyDict::new(py);
                d.set_item("orientation", ps.orientation.as_deref())?;
                d.set_item("paper_size", ps.paper_size)?;
                d.set_item("scale", ps.scale)?;
                d.set_item("fit_to_width", ps.fit_to_width)?;
                d.set_item("fit_to_height", ps.fit_to_height)?;
                d.set_item("fit_to_page", ps.fit_to_page)?;
                d.set_item("first_page_number", ps.first_page_number)?;
                d.set_item("page_order", ps.page_order.as_deref())?;
                d.set_item("black_and_white", ps.black_and_white)?;
                d.set_item("draft", ps.draft)?;
                Ok(Some(d))
            }
            None => Ok(None),
        }
    }

    fn page_margins<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
        match &self.page_margins {
            Some(pm) => {
                let d = PyDict::new(py);
                d.set_item("left", pm.left)?;
                d.set_item("right", pm.right)?;
                d.set_item("top", pm.top)?;
                d.set_item("bottom", pm.bottom)?;
                d.set_item("header", pm.header)?;
                d.set_item("footer", pm.footer)?;
                Ok(Some(d))
            }
            None => Ok(None),
        }
    }

    fn print_options<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
        match &self.print_options {
            Some(po) => {
                let d = PyDict::new(py);
                d.set_item("horizontal_centered", po.horizontal_centered)?;
                d.set_item("vertical_centered", po.vertical_centered)?;
                d.set_item("headings", po.headings)?;
                d.set_item("grid_lines", po.grid_lines)?;
                d.set_item("grid_lines_set", po.grid_lines_set)?;
                Ok(Some(d))
            }
            None => Ok(None),
        }
    }

    fn header_footer<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
        match &self.header_footer {
            Some(hf) => {
                let d = PyDict::new(py);
                d.set_item("different_odd_even", hf.different_odd_even)?;
                d.set_item("different_first", hf.different_first)?;
                d.set_item("scale_with_doc", hf.scale_with_doc)?;
                d.set_item("align_with_margins", hf.align_with_margins)?;
                d.set_item("odd_header", hf.odd_header.as_deref())?;
                d.set_item("odd_footer", hf.odd_footer.as_deref())?;
                d.set_item("even_header", hf.even_header.as_deref())?;
                d.set_item("even_footer", hf.even_footer.as_deref())?;
                d.set_item("first_header", hf.first_header.as_deref())?;
                d.set_item("first_footer", hf.first_footer.as_deref())?;
                Ok(Some(d))
            }
            None => Ok(None),
        }
    }

    #[getter]
    fn code_name(&self) -> Option<&str> {
        self.code_name.as_deref()
    }

    #[getter]
    fn tab_color(&self) -> Option<&str> {
        self.tab_color.as_deref()
    }

    fn data_validations<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyList>>> {
        match &self.data_validations {
            Some(dvs) => {
                let mut items = Vec::with_capacity(dvs.len());
                for dv in dvs {
                    items.push(data_validation_to_dict(py, dv)?);
                }
                Ok(Some(PyList::new(py, items)?))
            }
            None => Ok(None),
        }
    }

    fn conditional_formatting<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyList>>> {
        match &self.cf_rules {
            Some(rules) => {
                let dxfs = self.style_table.as_ref().map(|st| st.dxfs.as_slice());
                let mut items = Vec::with_capacity(rules.len());
                for r in rules {
                    items.push(cf_rule_to_dict(py, r, dxfs)?);
                }
                Ok(Some(PyList::new(py, items)?))
            }
            None => Ok(None),
        }
    }

    /// Named styles from styles.xml (when styles loaded).
    fn named_styles<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyList>>> {
        match &self.style_table {
            Some(st) => Ok(Some(named_styles_to_list(py, &st.named_styles)?)),
            None => Ok(None),
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "TurboSheet<name={:?}, nrows={}, ncols={}>",
            self.name, self.nrows, self.ncols
        )
    }
}

fn a1_cell(row: u32, col: u32) -> String {
    a1(row, col)
}

fn sheet_view_to_dict<'py>(py: Python<'py>, sv: &SheetViewMeta) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("show_grid_lines", sv.show_grid_lines)?;
    d.set_item("zoom_scale", sv.zoom_scale)?;
    d.set_item("tab_selected", sv.tab_selected)?;
    d.set_item(
        "top_left_cell",
        sv.top_left_cell.map(|(r, c)| a1_cell(r, c)),
    )?;
    d.set_item("workbook_view_id", sv.workbook_view_id)?;
    d.set_item("show_formulas", sv.show_formulas)?;
    d.set_item("show_row_col_headers", sv.show_row_col_headers)?;
    d.set_item("show_zeros", sv.show_zeros)?;
    d.set_item("right_to_left", sv.right_to_left)?;
    if let Some(ref pane) = sv.pane {
        d.set_item("pane", pane_to_dict(py, pane)?)?;
        if matches!(pane.state, PaneState::Frozen | PaneState::FrozenSplit) {
            d.set_item(
                "freeze_panes",
                pane.top_left_cell.map(|(r, c)| a1_cell(r, c)),
            )?;
        } else {
            d.set_item("freeze_panes", py.None())?;
        }
    } else {
        d.set_item("pane", py.None())?;
        d.set_item("freeze_panes", py.None())?;
    }
    Ok(d)
}

fn pane_to_dict<'py>(py: Python<'py>, p: &Pane) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("x_split", p.x_split)?;
    d.set_item("y_split", p.y_split)?;
    d.set_item(
        "top_left_cell",
        p.top_left_cell.map(|(r, c)| a1_cell(r, c)),
    )?;
    let active = match p.active_pane {
        ActivePane::BottomRight => "bottomRight",
        ActivePane::TopRight => "topRight",
        ActivePane::BottomLeft => "bottomLeft",
        ActivePane::TopLeft => "topLeft",
    };
    d.set_item("active_pane", active)?;
    let state = match p.state {
        PaneState::Split => "split",
        PaneState::Frozen => "frozen",
        PaneState::FrozenSplit => "frozenSplit",
    };
    d.set_item("state", state)?;
    Ok(d)
}

fn protection_meta_to_dict<'py>(
    py: Python<'py>,
    p: &SheetProtectionMeta,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("sheet", p.sheet)?;
    d.set_item("objects", p.objects)?;
    d.set_item("scenarios", p.scenarios)?;
    d.set_item("format_cells", p.format_cells)?;
    d.set_item("format_columns", p.format_columns)?;
    d.set_item("format_rows", p.format_rows)?;
    d.set_item("insert_columns", p.insert_columns)?;
    d.set_item("insert_rows", p.insert_rows)?;
    d.set_item("insert_hyperlinks", p.insert_hyperlinks)?;
    d.set_item("delete_columns", p.delete_columns)?;
    d.set_item("delete_rows", p.delete_rows)?;
    d.set_item("select_locked_cells", p.select_locked_cells)?;
    d.set_item("select_unlocked_cells", p.select_unlocked_cells)?;
    d.set_item("sort", p.sort)?;
    d.set_item("auto_filter", p.auto_filter)?;
    d.set_item("pivot_tables", p.pivot_tables)?;
    d.set_item("password", p.password.as_deref())?;
    d.set_item("algorithm_name", p.algorithm_name.as_deref())?;
    d.set_item("hash_value", p.hash_value.as_deref())?;
    d.set_item("salt_value", p.salt_value.as_deref())?;
    d.set_item("spin_count", p.spin_count)?;
    d.set_item("enabled", p.sheet)?;
    Ok(d)
}

fn data_validation_to_dict<'py>(
    py: Python<'py>,
    dv: &DataValidationRec,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("type", dv.type_.as_deref())?;
    d.set_item("operator", dv.operator.as_deref())?;
    d.set_item("allow_blank", dv.allow_blank)?;
    d.set_item("show_input_message", dv.show_input_message)?;
    d.set_item("show_error_message", dv.show_error_message)?;
    d.set_item("show_drop_down", dv.show_drop_down)?;
    d.set_item("error_style", dv.error_style.as_deref())?;
    d.set_item("sqref", &dv.sqref)?;
    d.set_item("formula1", dv.formula1.as_deref())?;
    d.set_item("formula2", dv.formula2.as_deref())?;
    d.set_item("prompt_title", dv.prompt_title.as_deref())?;
    d.set_item("prompt", dv.prompt.as_deref())?;
    d.set_item("error_title", dv.error_title.as_deref())?;
    d.set_item("error", dv.error.as_deref())?;
    Ok(d)
}

fn cf_rule_to_dict<'py>(
    py: Python<'py>,
    r: &CfRuleRec,
    dxfs: Option<&[super::Dxf]>,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("sqref", &r.sqref)?;
    d.set_item("type", &r.type_)?;
    d.set_item("priority", r.priority)?;
    d.set_item("operator", r.operator.as_deref())?;
    d.set_item("stop_if_true", r.stop_if_true)?;
    d.set_item("dxf_id", r.dxf_id)?;
    d.set_item("formulas", r.formulas.clone())?;
    d.set_item("text", r.text.as_deref())?;
    d.set_item("rank", r.rank)?;
    d.set_item("percent", r.percent)?;
    d.set_item("bottom", r.bottom)?;
    d.set_item("above_average", r.above_average)?;
    d.set_item("equal_average", r.equal_average)?;
    d.set_item("std_dev", r.std_dev)?;
    d.set_item("time_period", r.time_period.as_deref())?;
    // resolve dxf
    if let (Some(id), Some(dxfs)) = (r.dxf_id, dxfs) {
        if let Some(dxf) = dxfs.get(id as usize) {
            d.set_item("dxf", dxf_to_dict(py, dxf)?)?;
        } else {
            d.set_item("dxf", py.None())?;
        }
    } else {
        d.set_item("dxf", py.None())?;
    }
    if let Some(ref cs) = r.color_scale {
        let cd = PyDict::new(py);
        let mut cfvos = Vec::new();
        for v in &cs.cfvo {
            let vd = PyDict::new(py);
            vd.set_item("type", &v.type_)?;
            vd.set_item("val", v.val.as_deref())?;
            vd.set_item("gte", v.gte)?;
            cfvos.push(vd);
        }
        cd.set_item("cfvo", PyList::new(py, cfvos)?)?;
        let mut colors = Vec::new();
        for c in &cs.colors {
            colors.push(color_to_dict(py, c)?);
        }
        cd.set_item("colors", PyList::new(py, colors)?)?;
        d.set_item("color_scale", cd)?;
    } else {
        d.set_item("color_scale", py.None())?;
    }
    if let Some(ref db) = r.data_bar {
        let dd = PyDict::new(py);
        let mut cfvos = Vec::new();
        for v in &db.cfvo {
            let vd = PyDict::new(py);
            vd.set_item("type", &v.type_)?;
            vd.set_item("val", v.val.as_deref())?;
            vd.set_item("gte", v.gte)?;
            cfvos.push(vd);
        }
        dd.set_item("cfvo", PyList::new(py, cfvos)?)?;
        dd.set_item("color", color_to_dict(py, &db.color)?)?;
        dd.set_item("show_value", db.show_value)?;
        d.set_item("data_bar", dd)?;
    } else {
        d.set_item("data_bar", py.None())?;
    }
    if let Some(ref is) = r.icon_set {
        let id = PyDict::new(py);
        id.set_item("icon_set", is.icon_set.as_deref())?;
        id.set_item("show_value", is.show_value)?;
        id.set_item("percent", is.percent)?;
        id.set_item("reverse", is.reverse)?;
        d.set_item("icon_set", id)?;
    } else {
        d.set_item("icon_set", py.None())?;
    }
    Ok(d)
}

fn dxf_to_dict<'py>(py: Python<'py>, dxf: &super::Dxf) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    if let Some(ref f) = dxf.font {
        let fd = PyDict::new(py);
        fd.set_item("name", f.name.as_deref())?;
        fd.set_item("sz", f.sz.map(|s| s as f64))?;
        fd.set_item("b", f.bold)?;
        fd.set_item("i", f.italic)?;
        fd.set_item("u", f.underline.as_deref())?;
        if let Some(ref c) = f.color {
            fd.set_item("color", color_to_dict(py, c)?)?;
        } else {
            fd.set_item("color", py.None())?;
        }
        d.set_item("font", fd)?;
    } else {
        d.set_item("font", py.None())?;
    }
    if let Some(ref fill) = dxf.fill {
        d.set_item("fill", fill_to_dict(py, fill)?)?;
    } else {
        d.set_item("fill", py.None())?;
    }
    if let Some(ref b) = dxf.border {
        d.set_item("border", border_to_dict(py, b)?)?;
    } else {
        d.set_item("border", py.None())?;
    }
    d.set_item("num_fmt", dxf.num_fmt.as_deref())?;
    if let Some(ref a) = dxf.alignment {
        d.set_item("alignment", alignment_to_dict(py, a)?)?;
    } else {
        d.set_item("alignment", py.None())?;
    }
    if let Some(ref p) = dxf.protection {
        d.set_item("protection", protection_to_dict(py, p)?)?;
    } else {
        d.set_item("protection", py.None())?;
    }
    Ok(d)
}

// ---------------------------------------------------------------------------
// TurboReader
// ---------------------------------------------------------------------------

/// Workbook handle for the turbo fast path.
///
/// `load_sheet` re-reads the zip and shared parts with the requested feature flags,
/// but inflates+scans only the selected sheet (selective-sheet fast path).
///
/// Caching the parsed `TurboWorkbook` across calls is intentionally not done:
/// each call may pass different feature bits, and sheet extraction consumes the
/// per-sheet Arrow columns. Documented residual cost for multi-sheet workflows.
/// Unrequested features are not computed (wired to Rust `Features` bitflags).
#[pyclass(name = "_TurboReader", module = "kyrax._kyrax")]
pub struct PyTurboReader {
    path: String,
    sheet_names: Vec<String>,
    defined_names: Option<Vec<DefinedName>>,
    tables: Option<Vec<Table>>,
    workbook_props: Option<WorkbookProps>,
    date1904: bool,
    persons: Option<Vec<Person>>,
    vba: Option<VbaProject>,
}

#[pymethods]
impl PyTurboReader {
    #[getter]
    fn sheet_names(&self) -> Vec<String> {
        self.sheet_names.clone()
    }

    /// Load one sheet with selective feature extraction.
    ///
    /// `features` is `"values"` (default), `"all"`, or a list of feature names.
    /// Values are always included.
    #[pyo3(signature = (idx_or_name, *, features = None))]
    fn load_sheet(
        &mut self,
        py: Python<'_>,
        idx_or_name: &Bound<'_, PyAny>,
        features: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<PyTurboSheet> {
        let feat = parse_features(features)?;
        // Resolve before I/O so the selective path inflates only this sheet.
        let sheet_idx = resolve_sheet_index(idx_or_name, &self.sheet_names)?;
        let path = self.path.clone();
        let wb = py
            .detach(|| read_workbook_turbo_sheet(&path, feat, sheet_idx))
            .map_err(turbo_err_to_py)?;

        // Cache workbook-level sidecars when requested
        if feat.contains(Features::DEFINED_NAMES) {
            self.defined_names = wb.defined_names.clone();
        } else {
            // Keep prior cache if any; do not invent empty when not requested
        }
        if feat.contains(Features::TABLES) {
            // Selective path: only tables on the loaded sheet are present.
            let mut all = Vec::new();
            for s in &wb.sheets {
                if let Some(ref t) = s.tables {
                    all.extend(t.iter().cloned());
                }
            }
            self.tables = Some(all);
        }
        self.date1904 = wb.date1904;
        if feat.contains(Features::WORKBOOK_META) {
            self.workbook_props = wb.workbook_props.clone();
        }
        if feat.contains(Features::COMMENTS) {
            self.persons = wb.persons.clone();
        }
        if feat.contains(Features::VBA) {
            self.vba = wb.vba.clone();
        }

        // Selective parse returns a single-sheet vec (the requested sheet).
        let sheet = wb
            .sheets
            .into_iter()
            .next()
            .ok_or_else(|| {
                KyraxErrorKind::SheetNotFound(crate::types::IdxOrName::Idx(sheet_idx)).into()
            })
            .into_pyresult()?;

        let style_table = if feat.contains(Features::STYLES) || feat.contains(Features::COND_FORMAT)
        {
            wb.style_table
        } else {
            None
        };

        Ok(PyTurboSheet::from_parts(sheet, style_table))
    }

    /// Workbook-level defined names from the last load that requested them.
    ///
    /// Returns `None` if `defined_names` / `"all"` has not been loaded yet.
    fn defined_names<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyList>>> {
        match &self.defined_names {
            Some(dns) => {
                let mut items = Vec::with_capacity(dns.len());
                for dn in dns {
                    items.push(defined_name_to_dict(py, dn)?);
                }
                Ok(Some(PyList::new(py, items)?))
            }
            None => Ok(None),
        }
    }

    /// All tables from the last load that requested tables (across sheets).
    fn tables<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyList>>> {
        match &self.tables {
            Some(ts) => {
                let mut items = Vec::with_capacity(ts.len());
                for t in ts {
                    items.push(table_to_dict(py, t)?);
                }
                Ok(Some(PyList::new(py, items)?))
            }
            None => Ok(None),
        }
    }

    /// `date1904` epoch flag (always updated on last `load_sheet`). Serials are not rewritten.
    #[getter]
    fn date1904(&self) -> bool {
        self.date1904
    }

    /// Workbook-level properties from the last load that requested `workbook_meta` / `"all"`.
    fn workbook_props<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyDict>>> {
        match &self.workbook_props {
            Some(wp) => Ok(Some(workbook_props_to_dict(py, wp)?)),
            None => Ok(None),
        }
    }

    /// Persons (threaded-comment authors) from last load that requested comments/`all`.
    fn persons<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyList>>> {
        match &self.persons {
            Some(ps) => {
                let mut items = Vec::with_capacity(ps.len());
                for p in ps {
                    let d = PyDict::new(py);
                    d.set_item("id", &p.id)?;
                    d.set_item("display_name", &p.display_name)?;
                    d.set_item("user_id", p.user_id.as_deref())?;
                    d.set_item("provider_id", p.provider_id.as_deref())?;
                    items.push(d);
                }
                Ok(Some(PyList::new(py, items)?))
            }
            None => Ok(None),
        }
    }

    /// Whether a VBA project part is present (after a load with `vba` / `"all"`).
    #[getter]
    fn has_vba(&self) -> bool {
        self.vba.as_ref().map(|v| v.present).unwrap_or(false)
    }

    /// Raw `vbaProject.bin` bytes; None if VBA not requested or absent.
    fn vba_project<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        match &self.vba {
            Some(v) => match &v.bytes {
                Some(b) => Ok(Some(pyo3::types::PyBytes::new(py, b).into_any())),
                None => Ok(None),
            },
            None => Ok(None),
        }
    }

    fn __repr__(&self) -> String {
        format!("TurboReader<path={:?}>", self.path)
    }
}

fn threaded_comment_to_dict<'py>(
    py: Python<'py>,
    t: &ThreadedComment,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("ref", &t.ref_raw)?;
    d.set_item("id", &t.id)?;
    d.set_item("person_id", &t.person_id)?;
    d.set_item("person_display_name", &t.person_display_name)?;
    d.set_item("parent_id", t.parent_id.as_deref())?;
    d.set_item("done", t.done)?;
    d.set_item("text", &t.text)?;
    d.set_item("datetime", t.datetime.as_deref())?;
    d.set_item("row", t.ref_cell.0)?;
    d.set_item("col", t.ref_cell.1)?;
    Ok(d)
}

fn chart_to_dict<'py>(py: Python<'py>, c: &ChartMeta) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("sheet", c.sheet)?;
    d.set_item("part", &c.part)?;
    let types: Vec<&str> = c.chart_types.iter().map(|t| t.as_str()).collect();
    d.set_item("chart_types", types)?;
    d.set_item(
        "type",
        c.chart_types.first().map(|t| t.as_str()).unwrap_or("other"),
    )?;
    d.set_item("title", c.title.as_deref())?;
    d.set_item("x_axis_title", c.x_axis_title.as_deref())?;
    d.set_item("y_axis_title", c.y_axis_title.as_deref())?;

    let anchor = PyDict::new(py);
    anchor.set_item("kind", c.anchor.kind_str())?;
    match &c.anchor {
        ChartAnchor::OneCell { from } => {
            let f = PyDict::new(py);
            f.set_item("col", from.col)?;
            f.set_item("row", from.row)?;
            anchor.set_item("from", f)?;
        }
        ChartAnchor::TwoCell { from, to } => {
            let f = PyDict::new(py);
            f.set_item("col", from.col)?;
            f.set_item("row", from.row)?;
            anchor.set_item("from", f)?;
            let t = PyDict::new(py);
            t.set_item("col", to.col)?;
            t.set_item("row", to.row)?;
            anchor.set_item("to", t)?;
        }
        ChartAnchor::Absolute | ChartAnchor::Unknown => {}
    }
    d.set_item("anchor", anchor)?;

    let mut series = Vec::with_capacity(c.series.len());
    for s in &c.series {
        let sd = PyDict::new(py);
        sd.set_item("title_ref", s.title_ref.as_deref())?;
        sd.set_item("title_cache", s.title_cache.clone())?;
        sd.set_item("categories_ref", s.categories_ref.as_deref())?;
        sd.set_item("categories_cache", s.categories_cache.clone())?;
        sd.set_item("values_ref", s.values_ref.as_deref())?;
        sd.set_item("values_cache", s.values_cache.clone())?;
        series.push(sd);
    }
    d.set_item("series", series)?;
    Ok(d)
}

fn pivot_to_dict<'py>(py: Python<'py>, p: &PivotTableMeta) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("sheet", p.sheet)?;
    d.set_item("name", &p.name)?;
    d.set_item("location_ref", &p.location_ref)?;
    d.set_item("cache_id", p.cache_id)?;
    d.set_item("row_fields", p.row_fields.clone())?;
    d.set_item("col_fields", p.col_fields.clone())?;
    let mut dfs = Vec::with_capacity(p.data_fields.len());
    for df in &p.data_fields {
        let dd = PyDict::new(py);
        dd.set_item("name", &df.name)?;
        dd.set_item("fld", df.field_index)?;
        dfs.push(dd);
    }
    d.set_item("data_fields", dfs)?;
    d.set_item("cache_field_names", p.cache.field_names.clone())?;
    let src = PyDict::new(py);
    src.set_item("type", &p.cache.source_type)?;
    src.set_item("sheet", p.cache.worksheet_sheet.as_deref())?;
    src.set_item("ref", p.cache.worksheet_ref.as_deref())?;
    src.set_item("name", p.cache.worksheet_name.as_deref())?;
    d.set_item("cache_source", src)?;
    d.set_item("cache_part", &p.cache.part)?;
    Ok(d)
}

fn workbook_props_to_dict<'py>(
    py: Python<'py>,
    wp: &WorkbookProps,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("date1904", wp.date1904)?;
    d.set_item("code_name", wp.code_name.as_deref())?;
    d.set_item("full_calc_on_load", wp.full_calc_on_load)?;
    d.set_item("calc_id", wp.calc_id)?;
    let core = PyDict::new(py);
    core.set_item("title", wp.core.title.as_deref())?;
    core.set_item("creator", wp.core.creator.as_deref())?;
    core.set_item("description", wp.core.description.as_deref())?;
    core.set_item("subject", wp.core.subject.as_deref())?;
    core.set_item("last_modified_by", wp.core.last_modified_by.as_deref())?;
    core.set_item("created", wp.core.created.as_deref())?;
    core.set_item("modified", wp.core.modified.as_deref())?;
    core.set_item("category", wp.core.category.as_deref())?;
    core.set_item("keywords", wp.core.keywords.as_deref())?;
    core.set_item("revision", wp.core.revision.as_deref())?;
    core.set_item("version", wp.core.version.as_deref())?;
    d.set_item("core", core)?;
    let app = PyDict::new(py);
    app.set_item("application", wp.app.application.as_deref())?;
    app.set_item("app_version", wp.app.app_version.as_deref())?;
    d.set_item("app", app)?;
    Ok(d)
}

fn resolve_sheet_index(idx_or_name: &Bound<'_, PyAny>, names: &[String]) -> PyResult<usize> {
    if let Ok(idx) = idx_or_name.extract::<isize>() {
        let n = names.len() as isize;
        let resolved = if idx < 0 { n + idx } else { idx };
        if resolved < 0 || resolved as usize >= names.len() {
            return Err(
                KyraxErrorKind::SheetNotFound(crate::types::IdxOrName::Idx(idx as usize))
                    .into(),
            )
            .into_pyresult();
        }
        return Ok(resolved as usize);
    }
    if let Ok(name) = idx_or_name.extract::<String>() {
        return names
            .iter()
            .position(|n| n == &name)
            .ok_or_else(|| {
                KyraxErrorKind::SheetNotFound(crate::types::IdxOrName::Name(name)).into()
            })
            .into_pyresult();
    }
    Err(PyValueError::new_err(
        "idx_or_name must be an int sheet index or str sheet name",
    ))
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Open an XLSX file for turbo reading (sheet names only; data on `load_sheet`).
#[pyfunction(name = "read_excel_turbo")]
pub fn py_read_excel_turbo(path: &str) -> PyResult<PyTurboReader> {
    let sheet_names = list_sheet_names(path).map_err(turbo_err_to_py)?;
    Ok(PyTurboReader {
        path: path.to_owned(),
        sheet_names,
        defined_names: None,
        tables: None,
        workbook_props: None,
        date1904: false,
        persons: None,
        vba: None,
    })
}

