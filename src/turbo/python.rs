//! PyO3 bindings for the turbo fast-path reader.

use std::sync::{Arc, OnceLock};

use arrow_array::{
    Array, ArrayRef, RecordBatch, UInt32Array,
    builder::{StringBuilder, StringDictionaryBuilder, UInt32Builder},
    types::Int32Type,
};
use pyo3::{
    Bound, PyAny, PyResult, Python,
    exceptions::PyValueError,
    prelude::*,
    types::{PyDict, PyList, PyString, PyTuple},
};

use super::{
    ActivePane, Alignment, AutoFilterMeta, Border, CKind, CellError, CellRange, CfRuleRec,
    ChartAnchor, ChartMeta, ColDim, Color, DataValidationRec, DefinedName, Features, Fill, Font,
    FormulaColumn, HeaderFooterMeta, Hyperlink, ImageMeta, LinkTarget, NameKind, NamedStyleRec,
    PageMarginsMeta, PageSetupMeta, Pane, PaneState, Person, PivotTableMeta, PrintOptionsMeta,
    Protection, ReadImageAnchor, ReadImageMarker, RowDim, Scope, SheetComments, SheetFormat,
    SheetKind, SheetProtectionMeta, SheetState, SheetViewMeta, Side, StyleTable, Table,
    ThreadedComment, TurboError, VbaProject, WorkbookProps, a1,
    list_sheet_names_and_active_tab_with_password, range_a1,
    read_workbook_turbo_sheet_with_options,
};
use crate::error::{KyraxError, KyraxErrorKind, py_errors::IntoPyResult};
use crate::turbo::error::TurboResult;

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
                "images" => Features::IMAGES,
                "pivots" => Features::PIVOTS,
                "vba" => Features::VBA,
                "all" => Features::ALL,
                other => {
                    return Err(PyValueError::new_err(format!(
                        "unknown feature {other:?}; expected one of \
                         styles|formulas|merges|defined_names|tables|hyperlinks|comments|\
                         sheet_meta|page_setup|workbook_meta|validations|cond_format|\
                         charts|images|pivots|vba"
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
    d.set_item("strike", font.strike)?;
    d.set_item("underline", font.underline.as_deref().unwrap_or("none"))?;
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

fn named_styles_to_list<'py>(
    py: Python<'py>,
    styles: &[NamedStyleRec],
) -> PyResult<Bound<'py, PyList>> {
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
    d.set_item("reserved", dn.reserved.as_deref())?;
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
#[pyclass(name = "_TurboSheet", module = "kyrax._kyrax", skip_from_py_object)]
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
    /// Number of rows above the data region (1 when a header row is present,
    /// else 0). Re-bases turbo's 0-based data indices to raw-grid positions
    /// for `CellError.position` / `row_offset`.
    header_offset: usize,
    merges: Option<Vec<CellRange>>,
    tables: Option<Vec<Table>>,
    hyperlinks: Option<Vec<Hyperlink>>,
    comments: Option<SheetComments>,
    threaded_comments: Option<Vec<ThreadedComment>>,
    charts: Option<Vec<ChartMeta>>,
    images: Option<Vec<ImageMeta>>,
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
        header_offset: usize,
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
            header_offset,
            merges: sheet.merges,
            tables: sheet.tables,
            hyperlinks: sheet.hyperlinks,
            comments: sheet.comments,
            threaded_comments: sheet.threaded_comments,
            charts: sheet.charts,
            images: sheet.images,
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

impl Clone for PyTurboSheet {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            column_names: self.column_names.clone(),
            columns: self.columns.clone(),
            nrows: self.nrows,
            ncols: self.ncols,
            style_indices: self.style_indices.clone(),
            style_table: self.style_table.clone(),
            formulas: self.formulas.clone(),
            formulas_batch: match self.formulas_batch.get() {
                Some(b) => {
                    let cell = OnceLock::new();
                    let _ = cell.set(b.clone());
                    cell
                }
                None => OnceLock::new(),
            },
            cell_errors: self.cell_errors.clone(),
            header_offset: self.header_offset,
            merges: self.merges.clone(),
            tables: self.tables.clone(),
            hyperlinks: self.hyperlinks.clone(),
            comments: self.comments.clone(),
            threaded_comments: self.threaded_comments.clone(),
            charts: self.charts.clone(),
            images: self.images.clone(),
            pivots: self.pivots.clone(),
            sheet_state: self.sheet_state,
            sheet_kind: self.sheet_kind,
            row_dimensions: self.row_dimensions.clone(),
            column_dimensions: self.column_dimensions.clone(),
            sheet_format: self.sheet_format.clone(),
            auto_filter: self.auto_filter.clone(),
            sheet_view: self.sheet_view.clone(),
            protection: self.protection.clone(),
            page_setup: self.page_setup.clone(),
            page_margins: self.page_margins.clone(),
            print_options: self.print_options.clone(),
            header_footer: self.header_footer.clone(),
            code_name: self.code_name.clone(),
            tab_color: self.tab_color.clone(),
            data_validations: self.data_validations.clone(),
            cf_rules: self.cf_rules.clone(),
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
                        let nullable = arr.null_count() > 0 || arr.is_empty();
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

    /// Values as a pyarrow `RecordBatch` accompanied by `CellErrors` structure.
    #[cfg(feature = "pyarrow")]
    fn to_arrow_with_errors<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        use pyo3::IntoPyObjectExt;
        let rb = self.to_arrow(py)?;
        let errors = crate::types::excelsheet::CellErrors {
            errors: self
                .cell_errors
                .iter()
                .map(|e| crate::types::excelsheet::CellError {
                    position: (e.row as usize + self.header_offset, e.col as usize),
                    row_offset: self.header_offset,
                    detail: e.code.clone(),
                })
                .collect(),
        };
        (rb, errors).into_bound_py_any(py)
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
                        .map(|i| if arr.is_null(i) { 0 } else { arr.value(i) })
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
                let rb = py.detach(|| formulas_to_batch(f)).into_pyresult()?;
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
                let rb = py.detach(|| comments_to_batch(sc)).into_pyresult()?;
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

    /// Images on this sheet (bytes + anchor); None if images not requested.
    fn images<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyList>>> {
        match &self.images {
            Some(imgs) => {
                let mut items = Vec::with_capacity(imgs.len());
                for im in imgs {
                    items.push(image_to_dict(py, im)?);
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
    d.set_item("top_left_cell", p.top_left_cell.map(|(r, c)| a1_cell(r, c)))?;
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
        fd.set_item("strike", f.strike)?;
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
const MAX_TURBO_CACHE_CELLS: usize = 20_000_000;

type TurboCacheKey = (usize, u64, Option<usize>);

#[derive(Default)]
struct TurboReaderCache {
    map: ahash::AHashMap<TurboCacheKey, (PyTurboSheet, usize)>,
    order: std::collections::VecDeque<TurboCacheKey>,
    total_cells: usize,
}

impl TurboReaderCache {
    fn get(&self, key: &TurboCacheKey) -> Option<PyTurboSheet> {
        self.map.get(key).map(|(s, _)| s.clone())
    }

    fn insert(&mut self, key: TurboCacheKey, sheet: PyTurboSheet, cells: usize) {
        if cells > MAX_TURBO_CACHE_CELLS {
            return;
        }
        if let Some((_, old_cells)) = self.map.remove(&key) {
            self.total_cells = self.total_cells.saturating_sub(old_cells);
            self.order.retain(|k| k != &key);
        }
        while self.total_cells + cells > MAX_TURBO_CACHE_CELLS {
            if let Some(oldest_key) = self.order.pop_front() {
                if let Some((_, c)) = self.map.remove(&oldest_key) {
                    self.total_cells = self.total_cells.saturating_sub(c);
                }
            } else {
                break;
            }
        }
        self.map.insert(key, (sheet, cells));
        self.order.push_back(key);
        self.total_cells += cells;
    }
}

/// A reader for an open workbook that parses sidecars selectively per load.
///
/// Multi-sheet workbooks pay one selective read per `load_sheet` call because
/// each call may pass different feature bits, and sheet extraction consumes the
/// per-sheet Arrow columns. Documented residual cost for multi-sheet workflows.
/// Unrequested features are not computed (wired to Rust `Features` bitflags).
#[pyclass(name = "_TurboReader", module = "kyrax._kyrax")]
pub struct PyTurboReader {
    path: String,
    /// Password for an encrypted workbook (None = not supplied / not encrypted).
    password: Option<String>,
    sheet_names: Vec<String>,
    defined_names: Option<Vec<DefinedName>>,
    tables: Option<Vec<Table>>,
    workbook_props: Option<WorkbookProps>,
    date1904: bool,
    active_tab: u32,
    persons: Option<Vec<Person>>,
    vba: Option<VbaProject>,
    cached_sheets: std::sync::Mutex<TurboReaderCache>,
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
    /// `header_row`: row index of the header (default: 0). `None` means no header (row 1 is data).
    #[pyo3(signature = (idx_or_name, *, features = None, header_row = Some(0)))]
    fn load_sheet(
        &mut self,
        py: Python<'_>,
        idx_or_name: &Bound<'_, PyAny>,
        features: Option<&Bound<'_, PyAny>>,
        header_row: Option<usize>,
    ) -> PyResult<PyTurboSheet> {
        if let Some(hr) = header_row {
            if hr != 0 {
                return Err(PyValueError::new_err(
                    "only header_row=0 or None is supported",
                ));
            }
        }
        let feat = parse_features(features)?;
        // Resolve before I/O so the selective path inflates only this sheet.
        let sheet_idx = resolve_sheet_index(idx_or_name, &self.sheet_names)?;
        let cache_key = (sheet_idx, feat.0 as u64, header_row);

        if let Ok(guard) = self.cached_sheets.lock() {
            if let Some(cached) = guard.get(&cache_key) {
                return Ok(cached);
            }
        }

        let path = self.path.clone();
        let password = self.password.clone();
        let wb = py
            .detach(|| {
                read_workbook_turbo_sheet_with_options(
                    &path,
                    feat,
                    sheet_idx,
                    password.as_deref(),
                    header_row,
                )
            })
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

        let result =
            PyTurboSheet::from_parts(sheet, style_table, usize::from(header_row.is_some()));
        let cell_count = result.nrows * result.ncols;
        if let Ok(mut guard) = self.cached_sheets.lock() {
            guard.insert(cache_key, result.clone(), cell_count);
        }
        Ok(result)
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

    /// 0-based index of the active tab; 0 when absent.
    #[getter]
    fn active_tab(&self) -> u32 {
        self.active_tab
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

fn image_marker_to_dict<'py>(
    py: Python<'py>,
    m: &ReadImageMarker,
    d: &Bound<'py, PyDict>,
    key: &str,
) -> PyResult<()> {
    let md = PyDict::new(py);
    md.set_item("col", m.col)?;
    md.set_item("col_off", m.col_off)?;
    md.set_item("row", m.row)?;
    md.set_item("row_off", m.row_off)?;
    d.set_item(key, md)?;
    Ok(())
}

fn image_to_dict<'py>(py: Python<'py>, im: &ImageMeta) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("sheet", im.sheet)?;
    d.set_item("part", &im.part)?;
    d.set_item("data", pyo3::types::PyBytes::new(py, &im.bytes))?;
    let anchor = PyDict::new(py);
    anchor.set_item("kind", im.anchor.kind_str())?;
    match &im.anchor {
        ReadImageAnchor::Absolute { x, y, cx, cy } => {
            anchor.set_item("x", x)?;
            anchor.set_item("y", y)?;
            anchor.set_item("cx", cx)?;
            anchor.set_item("cy", cy)?;
        }
        ReadImageAnchor::OneCell { from, cx, cy } => {
            image_marker_to_dict(py, from, &anchor, "from")?;
            anchor.set_item("cx", cx)?;
            anchor.set_item("cy", cy)?;
        }
        ReadImageAnchor::TwoCell { from, to, edit_as } => {
            image_marker_to_dict(py, from, &anchor, "from")?;
            image_marker_to_dict(py, to, &anchor, "to")?;
            anchor.set_item("edit_as", edit_as.as_deref())?;
        }
    }
    d.set_item("anchor", anchor)?;
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
    d.set_item("active_tab", wp.active_tab)?;
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
                KyraxErrorKind::SheetNotFound(crate::types::IdxOrName::Idx(idx as usize)).into(),
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

pub fn sniff_magic_bytes(path: &str) -> TurboResult<()> {
    let mut file = std::fs::File::open(path)?;
    use std::io::Read;
    let mut magic = [0u8; 8];
    let n = file.read(&mut magic)?;
    if n >= 4 && &magic[..4] == b"PK\x03\x04" {
        return Ok(());
    }
    if n >= 8 && &magic[..8] == b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1" {
        return Err(TurboError::Refused(
            "legacy binary Excel (.xls) format is not supported by turbo engine; use read_excel() for legacy BIFF formats".into(),
        ));
    }
    if n < 4 {
        return Err(TurboError::Format(
            "file is too short to be a valid XLSX archive".into(),
        ));
    }
    Err(TurboError::Format(format!(
        "not a valid OOXML/ZIP archive: unexpected header {:02x?}",
        &magic[..n.min(4)]
    )))
}

#[pyclass(name = "SheetStream", module = "kyrax._kyrax")]
pub struct PySheetStream {
    inner: std::sync::Mutex<Option<crate::turbo::stream::SheetStream>>,
    pending: std::sync::Mutex<Option<(arrow_array::RecordBatch, usize)>>,
    chunk_size: usize,
    opts: crate::turbo::stream::StreamOptions,
}

#[pymethods]
impl PySheetStream {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    /// Header row (sheet row 1) captured by the stream's schema pre-pass.
    /// Exposed so the Python read_only doorway can yield full grid rows
    /// (openpyxl parity) while the engine itself streams data rows only.
    #[getter]
    fn column_names(&self) -> PyResult<Vec<String>> {
        if let Ok(guard) = self.inner.lock() {
            if let Some(s) = guard.as_ref() {
                return Ok(s.column_names().to_vec());
            }
        }
        Ok(Vec::new())
    }

    fn __next__<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        // First check pending sliced batch
        if let Ok(mut pending_guard) = self.pending.lock() {
            if let Some((batch, offset)) = pending_guard.take() {
                let remaining = batch.num_rows() - offset;
                let take_len = remaining.min(self.chunk_size);
                let sub = batch.slice(offset, take_len);
                let next_offset = offset + take_len;
                if next_offset < batch.num_rows() {
                    *pending_guard = Some((batch, next_offset));
                }
                #[cfg(feature = "pyarrow")]
                {
                    let py_batch = record_batch_to_py(py, sub)?;
                    return Ok(Some(py_batch));
                }
                #[cfg(not(feature = "pyarrow"))]
                {
                    return Err(PyValueError::new_err("pyarrow feature is not enabled"));
                }
            }
        }

        let mut guard = self
            .inner
            .lock()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let stream = match guard.as_mut() {
            Some(s) => s,
            None => return Ok(None),
        };
        let opts = self.opts.clone();
        let maybe_batch = py
            .detach(|| stream.next_batch(&opts))
            .map_err(turbo_err_to_py)?;
        match maybe_batch {
            Some(batch) => {
                let num_rows = batch.num_rows();
                if self.chunk_size > 0 && num_rows > self.chunk_size {
                    let sub = batch.slice(0, self.chunk_size);
                    if let Ok(mut pending_guard) = self.pending.lock() {
                        *pending_guard = Some((batch, self.chunk_size));
                    }
                    #[cfg(feature = "pyarrow")]
                    {
                        let py_batch = record_batch_to_py(py, sub)?;
                        Ok(Some(py_batch))
                    }
                    #[cfg(not(feature = "pyarrow"))]
                    {
                        Err(PyValueError::new_err("pyarrow feature is not enabled"))
                    }
                } else {
                    #[cfg(feature = "pyarrow")]
                    {
                        let py_batch = record_batch_to_py(py, batch)?;
                        Ok(Some(py_batch))
                    }
                    #[cfg(not(feature = "pyarrow"))]
                    {
                        Err(PyValueError::new_err("pyarrow feature is not enabled"))
                    }
                }
            }
            None => {
                *guard = None;
                Ok(None)
            }
        }
    }

    fn close(&self) -> PyResult<()> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        *guard = None;
        if let Ok(mut pending_guard) = self.pending.lock() {
            *pending_guard = None;
        }
        Ok(())
    }

    #[getter]
    fn closed(&self) -> bool {
        if let Ok(guard) = self.inner.lock() {
            guard.is_none()
        } else {
            true
        }
    }
}

#[pyfunction(name = "read_excel_turbo_iter")]
#[pyo3(signature = (path, sheet_idx = 0, chunk_size = 10000))]
pub fn py_read_excel_turbo_iter(
    py: Python<'_>,
    path: &str,
    sheet_idx: usize,
    chunk_size: usize,
) -> PyResult<PySheetStream> {
    sniff_magic_bytes(path).map_err(turbo_err_to_py)?;
    let path_buf = path.to_string();
    let opts = crate::turbo::stream::StreamOptions {
        batch_rows: chunk_size,
        ..Default::default()
    };
    let stream_opts = opts.clone();
    let stream = py
        .detach(|| crate::turbo::stream::SheetStream::open(&path_buf, sheet_idx, stream_opts))
        .map_err(turbo_err_to_py)?;
    Ok(PySheetStream {
        inner: std::sync::Mutex::new(Some(stream)),
        pending: std::sync::Mutex::new(None),
        chunk_size,
        opts,
    })
}

// ---------------------------------------------------------------------------
// Read-only streaming row shaping / cell lookup / lifecycle (Rust-side)
// ---------------------------------------------------------------------------
//
// The read_only doorway performs all grid-semantics shaping (header row 1
// re-injection, min/max row/col selection, values_only tuples) and cell lookup
// here in Rust so the Python layer stays a thin, loop-free API doorway.

#[pyclass(name = "ReadOnlyRows", module = "kyrax._kyrax")]
pub struct PyReadOnlyRowIter {
    inner: Option<crate::turbo::stream::SheetStream>,
    opts: crate::turbo::stream::StreamOptions,
    // 1-based grid bounds (min_row/min_col clamped to >= 1).
    min_row: usize,
    max_row: Option<usize>,
    min_col: usize,
    max_col: Option<usize>,
    header: Vec<String>,
    header_done: bool,
    data_row: usize,
    finished: bool,
    // Pending batch state: pyarrow lists for the selected columns, plus the
    // row window within the batch that is still to be yielded. `pending_staged`
    // distinguishes "a batch is staged" from "no columns were selected".
    pending_staged: bool,
    pending_arrays: Vec<Py<PyAny>>,
    pending_sub_start: usize,
    pending_sub_end: usize,
    pending_row: usize,
}

impl PyReadOnlyRowIter {
    /// Mirror the Python `_select` helper for the header row: `lo = min_col-1`,
    /// `hi = min(max_col, n)` (or `n` when max_col is None), empty when lo>=hi.
    fn header_hi(&self) -> usize {
        match self.max_col {
            Some(b) => b.min(self.header.len()),
            None => self.header.len(),
        }
    }

    /// Data-columns upper bound: `min(max_col, num_cols)` when max_col is a
    /// positive value, else `num_cols` (mirrors Python's `c2_bound or num_cols`).
    fn data_c2(&self, num_cols: usize) -> usize {
        match self.max_col {
            Some(b) if b > 0 => b.min(num_cols),
            _ => num_cols,
        }
    }

    fn build_header<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let lo = self.min_col.saturating_sub(1);
        let hi = self.header_hi();
        let mut items: Vec<Bound<'py, PyAny>> = Vec::new();
        if lo < hi {
            for i in lo..hi.min(self.header.len()) {
                items.push(PyString::new(py, &self.header[i]).into_any());
            }
        }
        Ok(PyTuple::new(py, items)?.into_any())
    }

    fn build_row<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let r = self.pending_row;
        let mut items: Vec<Bound<'py, PyAny>> = Vec::with_capacity(self.pending_arrays.len());
        for arr in &self.pending_arrays {
            items.push(arr.bind(py).get_item(r)?);
        }
        Ok(PyTuple::new(py, items)?.into_any())
    }

    /// Select the columns and the row window for a batch and stage them for
    /// `build_row`. Mirrors the Python batch shaping exactly.
    fn prepare_batch<'py>(
        &mut self,
        py: Python<'py>,
        batch: &RecordBatch,
        batch_start: usize,
        num: usize,
    ) -> PyResult<()> {
        #[cfg(feature = "pyarrow")]
        {
            use arrow_pyarrow::ToPyArrow;
            let num_cols = batch.num_columns();
            let c1 = self.min_col.saturating_sub(1);
            let c2 = self.data_c2(num_cols);
            let sub_start = self.min_row.saturating_sub(batch_start).min(num);
            let sub_end = match self.max_row {
                Some(m) => (m.saturating_sub(batch_start) + 1).min(num),
                None => num,
            };
            let mut arrays: Vec<Py<PyAny>> = Vec::new();
            if sub_start < sub_end {
                for c in c1..c2 {
                    if c < num_cols {
                        let arr = batch.column(c).clone();
                        let py_arr = arr
                            .to_data()
                            .to_pyarrow(py)
                            .map_err(|e| arrow_err(e.to_string()))?;
                        // to_pylist converts Arrow scalars to native Python
                        // values (matching the old batch.to_pydict() path).
                        let py_list = py_arr
                            .call_method0("to_pylist")
                            .map_err(|e| arrow_err(e.to_string()))?;
                        arrays.push(py_list.unbind());
                    }
                }
            }
            self.pending_arrays = arrays;
            self.pending_sub_start = sub_start;
            self.pending_sub_end = sub_end;
            self.pending_row = sub_start;
            self.pending_staged = true;
            Ok(())
        }
        #[cfg(not(feature = "pyarrow"))]
        {
            let _ = (py, batch, batch_start, num);
            Err(PyValueError::new_err("pyarrow feature is not enabled"))
        }
    }
}

#[pymethods]
impl PyReadOnlyRowIter {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__<'py>(&mut self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        // 0. A closed iterator yields no further rows (close() clears `inner`).
        if self.inner.is_none() {
            return Ok(None);
        }

        let mut stream = match self.inner.take() {
            Some(s) => s,
            None => return Ok(None),
        };
        let opts = self.opts.clone();

        loop {
            // 1. Yield a staged row (an empty tuple when no columns matched).
            if self.pending_staged {
                if self.pending_row < self.pending_sub_end {
                    let tup = self.build_row(py)?;
                    self.pending_row += 1;
                    self.inner = Some(stream);
                    return Ok(Some(tup));
                }
                self.pending_staged = false;
                self.pending_arrays = Vec::new();
            }

            // 2. Exhausted?
            if self.finished {
                self.inner = Some(stream);
                return Ok(None);
            }

            // 3. Pull the next batch.
            let batch = match py
                .detach(|| stream.next_batch(&opts))
                .map_err(turbo_err_to_py)?
            {
                Some(b) => b,
                None => {
                    self.finished = true;
                    self.inner = Some(stream);
                    return Ok(None);
                }
            };

            if self.header.is_empty() {
                self.header = stream.column_names().to_vec();
            }

            let num = batch.num_rows();
            let batch_start = self.data_row;
            let batch_end = self.data_row.saturating_add(num).saturating_sub(1);
            self.data_row = self.data_row.saturating_add(num);

            // 4. Grid row 1 is the header: re-inject it once, before the first
            //    data batch, mirroring Python's openpyxl-parity semantics.
            if !self.header_done {
                self.header_done = true;
                let in_bounds = self.min_row <= 1 && self.max_row.map_or(true, |m| m >= 1);
                if in_bounds && (!self.header.is_empty() || num > 0) {
                    let hdr = self.build_header(py)?;
                    let done = self.max_row.map_or(false, |m| batch_start > m);
                    if done {
                        self.finished = true;
                    } else if batch_end >= self.min_row {
                        self.prepare_batch(py, &batch, batch_start, num)?;
                    }
                    self.inner = Some(stream);
                    return Ok(Some(hdr));
                }
            }

            // 5. Skip batches outside the requested data-row window.
            let done = self.max_row.map_or(false, |m| batch_start > m);
            if done {
                self.finished = true;
                continue;
            }
            if batch_end < self.min_row {
                continue;
            }

            // 6. Stage the data-row window for this batch; loop to yield rows.
            self.prepare_batch(py, &batch, batch_start, num)?;
        }
    }

    fn close(&mut self) -> PyResult<()> {
        self.inner = None;
        Ok(())
    }

    #[getter]
    fn closed(&self) -> bool {
        self.inner.is_none()
    }
}

/// Open a streaming read of a worksheet with grid-semantics row/column shaping
/// applied in Rust. Yields Python tuples (values_only semantics), including the
/// header row (grid row 1) for openpyxl parity.
#[pyfunction(name = "read_excel_turbo_iter_rows")]
#[pyo3(signature = (
    path,
    sheet_idx = 0,
    min_row = None,
    max_row = None,
    min_col = None,
    max_col = None,
    chunk_size = 10000
))]
pub fn py_read_excel_turbo_iter_rows(
    py: Python<'_>,
    path: &str,
    sheet_idx: usize,
    min_row: Option<usize>,
    max_row: Option<usize>,
    min_col: Option<usize>,
    max_col: Option<usize>,
    chunk_size: usize,
) -> PyResult<PyReadOnlyRowIter> {
    sniff_magic_bytes(path).map_err(turbo_err_to_py)?;
    let path_buf = path.to_string();
    let opts = crate::turbo::stream::StreamOptions {
        batch_rows: chunk_size,
        ..Default::default()
    };
    let stream = py
        .detach(|| crate::turbo::stream::SheetStream::open(&path_buf, sheet_idx, opts.clone()))
        .map_err(turbo_err_to_py)?;
    Ok(PyReadOnlyRowIter {
        inner: Some(stream),
        opts,
        min_row: min_row.unwrap_or(1).max(1),
        max_row,
        min_col: min_col.unwrap_or(1).max(1),
        max_col,
        header: Vec::new(),
        header_done: false,
        data_row: 2,
        finished: false,
        pending_staged: false,
        pending_arrays: Vec::new(),
        pending_sub_start: 0,
        pending_sub_end: 0,
        pending_row: 0,
    })
}

/// Look up a single grid cell value (row/column 1-based) in read_only mode.
/// Grid row 1 is the header row. Returns the cell value or `None` when the cell
/// is empty or out of range.
#[pyfunction(name = "read_excel_turbo_cell")]
#[pyo3(signature = (path, sheet_idx = 0, row = 1, column = 1, chunk_size = 10000))]
pub fn py_read_excel_turbo_cell<'py>(
    py: Python<'py>,
    path: &str,
    sheet_idx: usize,
    row: usize,
    column: usize,
    chunk_size: usize,
) -> PyResult<Bound<'py, PyAny>> {
    sniff_magic_bytes(path).map_err(turbo_err_to_py)?;
    let opts = crate::turbo::stream::StreamOptions {
        batch_rows: chunk_size,
        ..Default::default()
    };
    let mut stream = crate::turbo::stream::SheetStream::open(path, sheet_idx, opts.clone())
        .map_err(turbo_err_to_py)?;
    let col0 = column.saturating_sub(1);
    let mut header_pending = true;
    let mut data_row: usize = 2;
    loop {
        let batch = match py
            .detach(|| stream.next_batch(&opts))
            .map_err(turbo_err_to_py)?
        {
            Some(b) => b,
            None => break,
        };
        if header_pending {
            header_pending = false;
            let names = stream.column_names().to_vec();
            if row == 1 {
                return if col0 < names.len() {
                    Ok(PyString::new(py, &names[col0]).into_any())
                } else {
                    Ok(py.None().into_bound(py))
                };
            }
        }
        let num = batch.num_rows();
        let batch_start = data_row;
        let batch_end = data_row.saturating_add(num).saturating_sub(1);
        data_row = data_row.saturating_add(num);
        if row < batch_start || row > batch_end {
            continue;
        }
        let idx = row - batch_start;
        let num_cols = batch.num_columns();
        if col0 >= num_cols {
            return Ok(py.None().into_bound(py));
        }
        #[cfg(feature = "pyarrow")]
        {
            use arrow_pyarrow::ToPyArrow;
            let arr = batch.column(col0).clone();
            let py_arr = arr
                .to_data()
                .to_pyarrow(py)
                .map_err(|e| arrow_err(e.to_string()))?;
            let py_list = py_arr
                .call_method0("to_pylist")
                .map_err(|e| arrow_err(e.to_string()))?;
            return Ok(py_list.get_item(idx)?);
        }
        #[cfg(not(feature = "pyarrow"))]
        {
            let _ = idx;
            return Err(PyValueError::new_err("pyarrow feature is not enabled"));
        }
    }
    Ok(py.None().into_bound(py))
}

/// Close a list of streaming readers/iterators without a Python-level loop.
/// Used by the read_only workbook lifecycle to close every open stream.
#[pyfunction(name = "close_streams")]
pub fn py_close_streams(streams: Vec<Bound<'_, PyAny>>) -> PyResult<()> {
    for s in &streams {
        s.call_method0("close")?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Open an XLSX file for turbo reading (sheet names only; data on `load_sheet`).
///
/// `password` opens an ECMA-376 encrypted workbook; a plain workbook ignores it.
#[pyfunction(name = "read_excel_turbo")]
#[pyo3(signature = (path, password = None))]
pub fn py_read_excel_turbo(path: &str, password: Option<String>) -> PyResult<PyTurboReader> {
    sniff_magic_bytes(path).map_err(turbo_err_to_py)?;
    let (sheet_names, active_tab) =
        list_sheet_names_and_active_tab_with_password(path, password.as_deref())
            .map_err(turbo_err_to_py)?;
    Ok(PyTurboReader {
        path: path.to_owned(),
        password,
        sheet_names,
        defined_names: None,
        tables: None,
        workbook_props: None,
        date1904: false,
        active_tab,
        persons: None,
        vba: None,
        cached_sheets: std::sync::Mutex::new(TurboReaderCache::default()),
    })
}

/// Detect an ECMA-376 encrypted workbook (OLE/CFB with an `EncryptionInfo`
/// stream) without a password. Never raises for an unreadable file.
#[pyfunction(name = "is_encrypted")]
pub fn py_is_encrypted(path: &str) -> PyResult<bool> {
    let data = std::fs::read(path).map_err(|e| turbo_err_to_py(TurboError::Io(e)))?;
    Ok(crate::turbo::crypto::is_encrypted(&data))
}

/// Report an encrypted workbook's scheme, algorithm and spin count WITHOUT a
/// password. Raises when the file is not an encrypted workbook.
#[cfg(feature = "encryption")]
#[pyfunction(name = "encryption_info")]
pub fn py_encryption_info<'py>(py: Python<'py>, path: &str) -> PyResult<Bound<'py, PyAny>> {
    let data = py
        .detach(|| std::fs::read(path))
        .map_err(|e: std::io::Error| turbo_err_to_py(TurboError::Io(e)))?;
    let meta = crate::turbo::crypto::encryption_info(&data)
        .map_err(|e| turbo_err_to_py(TurboError::Format(e.to_string())))?;
    let d = PyDict::new(py);
    d.set_item("scheme", meta.scheme)?;
    d.set_item("cipher_algorithm", &meta.cipher_algorithm)?;
    d.set_item("hash_algorithm", &meta.hash_algorithm)?;
    d.set_item("key_bits", meta.key_bits)?;
    d.set_item("block_size", meta.block_size)?;
    d.set_item("salt_size", meta.salt_size)?;
    d.set_item("spin_count", meta.spin_count)?;
    match &meta.message {
        Some(m) => d.set_item("message", m)?,
        None => d.set_item("message", py.None())?,
    }
    Ok(d.into_any())
}
