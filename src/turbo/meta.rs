//! Sheet / workbook metadata + sheet-tail rich meta (data validations, CF rules).
//!
//! Stream A: pre-sheetData header + body row attrs + post-sheetData tail + workbook sidecars.
//! Stream B (sheet-level): dataValidations + conditionalFormatting (tail).

use super::decode::decode_bytes;
use super::structural::{CellRange, find_attr, parse_range};
use super::styles::{Color, parse_color};

// ============================================================================
// A1 — Row / column dimensions
// ============================================================================

#[derive(Clone, Debug)]
pub struct RowDim {
    pub row: u32, // 1-based
    pub height: Option<f64>,
    pub hidden: bool,
    pub outline_level: u8,
    pub collapsed: bool,
    pub style: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct ColDim {
    pub min: u32, // 1-based inclusive
    pub max: u32,
    pub width: Option<f64>,
    pub hidden: bool,
    pub best_fit: bool,
    pub outline_level: u8,
    pub collapsed: bool,
    pub style: Option<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct SheetFormat {
    pub base_col_width: Option<u32>,
    pub default_col_width: Option<f64>,
    pub default_row_height: Option<f64>,
    pub custom_height: Option<bool>,
    pub zero_height: Option<bool>,
    pub outline_level_row: Option<u8>,
    pub outline_level_col: Option<u8>,
}

// ============================================================================
// A2 — AutoFilter
// ============================================================================

#[derive(Clone, Debug)]
pub struct FilterColumnMeta {
    pub col_id: u32,
    pub hidden_button: bool,
    pub show_button: bool,
    pub values: Vec<String>,
    pub blank: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct AutoFilterMeta {
    pub ref_: CellRange,
    pub columns: Vec<FilterColumnMeta>,
}

// ============================================================================
// A3 — Panes / sheet view
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneState {
    Split,
    Frozen,
    FrozenSplit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivePane {
    BottomRight,
    TopRight,
    BottomLeft,
    TopLeft,
}

#[derive(Clone, Debug)]
pub struct Pane {
    pub x_split: Option<f64>,
    pub y_split: Option<f64>,
    pub top_left_cell: Option<(u32, u32)>, // 0-based
    pub active_pane: ActivePane,
    pub state: PaneState,
}

#[derive(Clone, Debug)]
pub struct SheetViewMeta {
    pub show_grid_lines: Option<bool>,
    pub zoom_scale: Option<u32>,
    pub tab_selected: Option<bool>,
    pub top_left_cell: Option<(u32, u32)>,
    pub workbook_view_id: u32,
    pub show_formulas: Option<bool>,
    pub show_row_col_headers: Option<bool>,
    pub show_zeros: Option<bool>,
    pub right_to_left: Option<bool>,
    pub pane: Option<Pane>,
}

// ============================================================================
// A4 — Sheet protection
// ============================================================================

#[derive(Clone, Debug)]
pub struct SheetProtectionMeta {
    pub sheet: bool,
    pub objects: bool,
    pub scenarios: bool,
    pub format_cells: bool,
    pub format_columns: bool,
    pub format_rows: bool,
    pub insert_columns: bool,
    pub insert_rows: bool,
    pub insert_hyperlinks: bool,
    pub delete_columns: bool,
    pub delete_rows: bool,
    pub select_locked_cells: bool,
    pub select_unlocked_cells: bool,
    pub sort: bool,
    pub auto_filter: bool,
    pub pivot_tables: bool,
    pub password: Option<String>,
    pub algorithm_name: Option<String>,
    pub hash_value: Option<String>,
    pub salt_value: Option<String>,
    pub spin_count: Option<u32>,
}

impl Default for SheetProtectionMeta {
    fn default() -> Self {
        // openpyxl SheetProtection constructor defaults when sheet protection object exists
        Self {
            sheet: false,
            objects: false,
            scenarios: false,
            format_cells: true,
            format_columns: true,
            format_rows: true,
            insert_columns: true,
            insert_rows: true,
            insert_hyperlinks: true,
            delete_columns: true,
            delete_rows: true,
            select_locked_cells: false,
            select_unlocked_cells: false,
            sort: true,
            auto_filter: true,
            pivot_tables: true,
            password: None,
            algorithm_name: None,
            hash_value: None,
            salt_value: None,
            spin_count: None,
        }
    }
}

// ============================================================================
// A5 — Page setup
// ============================================================================

#[derive(Clone, Debug, Default)]
pub struct PageSetupMeta {
    pub orientation: Option<String>,
    pub paper_size: Option<u32>,
    pub scale: Option<u32>,
    pub fit_to_width: Option<u32>,
    pub fit_to_height: Option<u32>,
    pub fit_to_page: Option<bool>,
    pub first_page_number: Option<u32>,
    pub page_order: Option<String>,
    pub black_and_white: Option<bool>,
    pub draft: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct PageMarginsMeta {
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
    pub header: f64,
    pub footer: f64,
}

impl Default for PageMarginsMeta {
    fn default() -> Self {
        Self {
            left: 0.75,
            right: 0.75,
            top: 1.0,
            bottom: 1.0,
            header: 0.5,
            footer: 0.5,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct PrintOptionsMeta {
    pub horizontal_centered: Option<bool>,
    pub vertical_centered: Option<bool>,
    pub headings: Option<bool>,
    pub grid_lines: Option<bool>,
    pub grid_lines_set: Option<bool>,
}

#[derive(Clone, Debug, Default)]
pub struct HeaderFooterMeta {
    pub different_odd_even: Option<bool>,
    pub different_first: Option<bool>,
    pub scale_with_doc: Option<bool>,
    pub align_with_margins: Option<bool>,
    pub odd_header: Option<String>,
    pub odd_footer: Option<String>,
    pub even_header: Option<String>,
    pub even_footer: Option<String>,
    pub first_header: Option<String>,
    pub first_footer: Option<String>,
}

// ============================================================================
// A6 — Sheet state / kind / sheetPr
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SheetState {
    Visible,
    Hidden,
    VeryHidden,
}

impl SheetState {
    pub fn as_str(self) -> &'static str {
        match self {
            SheetState::Visible => "visible",
            SheetState::Hidden => "hidden",
            SheetState::VeryHidden => "veryHidden",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SheetKind {
    Worksheet,
    Chartsheet,
}

impl SheetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SheetKind::Worksheet => "worksheet",
            SheetKind::Chartsheet => "chartsheet",
        }
    }
}

// ============================================================================
// A7 — Workbook props
// ============================================================================

#[derive(Clone, Debug, Default)]
pub struct CoreProps {
    pub title: Option<String>,
    pub creator: Option<String>,
    pub description: Option<String>,
    pub subject: Option<String>,
    pub last_modified_by: Option<String>,
    pub created: Option<String>,
    pub modified: Option<String>,
    pub category: Option<String>,
    pub keywords: Option<String>,
    pub revision: Option<String>,
    pub version: Option<String>,
    pub content_status: Option<String>,
    pub language: Option<String>,
    pub identifier: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct AppProps {
    pub application: Option<String>,
    pub app_version: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct WorkbookProps {
    pub date1904: bool,
    pub code_name: Option<String>,
    pub full_calc_on_load: Option<bool>,
    pub calc_id: Option<u32>,
    pub core: CoreProps,
    pub app: AppProps,
}

// ============================================================================
// B4 — Data validations
// ============================================================================

#[derive(Clone, Debug)]
pub struct DataValidationRec {
    pub type_: Option<String>,
    pub operator: Option<String>,
    pub allow_blank: bool,
    pub show_input_message: bool,
    pub show_error_message: bool,
    pub show_drop_down: bool,
    pub error_style: Option<String>,
    pub sqref: String,
    pub formula1: Option<String>,
    pub formula2: Option<String>,
    pub prompt_title: Option<String>,
    pub prompt: Option<String>,
    pub error_title: Option<String>,
    pub error: Option<String>,
}

// ============================================================================
// B5 — Conditional formatting rules
// ============================================================================

#[derive(Clone, Debug)]
pub struct CfVo {
    pub type_: String,
    pub val: Option<String>,
    pub gte: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct ColorScaleParams {
    pub cfvo: Vec<CfVo>,
    pub colors: Vec<Color>,
}

#[derive(Clone, Debug)]
pub struct DataBarParams {
    pub cfvo: Vec<CfVo>,
    pub color: Color,
    pub show_value: Option<bool>,
    pub min_length: Option<u32>,
    pub max_length: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct IconSetParams {
    pub icon_set: Option<String>,
    pub cfvo: Vec<CfVo>,
    pub show_value: Option<bool>,
    pub percent: Option<bool>,
    pub reverse: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct CfRuleRec {
    pub sqref: String,
    pub type_: String,
    pub priority: i32,
    pub operator: Option<String>,
    pub stop_if_true: Option<bool>,
    pub dxf_id: Option<u32>,
    pub formulas: Vec<String>,
    pub text: Option<String>,
    pub rank: Option<i32>,
    pub percent: Option<bool>,
    pub bottom: Option<bool>,
    pub above_average: Option<bool>,
    pub equal_average: Option<bool>,
    pub std_dev: Option<i32>,
    pub time_period: Option<String>,
    pub color_scale: Option<ColorScaleParams>,
    pub data_bar: Option<DataBarParams>,
    pub icon_set: Option<IconSetParams>,
}

// ============================================================================
// Aggregated sheet meta from header + tail
// ============================================================================

#[derive(Clone, Debug, Default)]
#[allow(dead_code)] // aggregate shape reserved for future single-return API
pub struct SheetMetaExtra {
    pub row_dimensions: Vec<RowDim>,
    pub column_dimensions: Vec<ColDim>,
    pub sheet_format: Option<SheetFormat>,
    pub auto_filter: Option<AutoFilterMeta>,
    pub sheet_view: Option<SheetViewMeta>,
    pub protection: Option<SheetProtectionMeta>,
    pub page_setup: Option<PageSetupMeta>,
    pub page_margins: Option<PageMarginsMeta>,
    pub print_options: Option<PrintOptionsMeta>,
    pub header_footer: Option<HeaderFooterMeta>,
    pub code_name: Option<String>,
    pub tab_color: Option<String>,
    pub data_validations: Vec<DataValidationRec>,
    pub cf_rules: Vec<CfRuleRec>,
}

// ============================================================================
// Low-level helpers
// ============================================================================

#[inline]
fn attr_bool(tag: &[u8], name: &[u8]) -> Option<bool> {
    find_attr(tag, name).map(|v| v == b"1" || v.eq_ignore_ascii_case(b"true"))
}

#[inline]
fn attr_bool_default(tag: &[u8], name: &[u8], default: bool) -> bool {
    attr_bool(tag, name).unwrap_or(default)
}

#[inline]
fn attr_u32(tag: &[u8], name: &[u8]) -> Option<u32> {
    find_attr(tag, name)
        .and_then(|v| std::str::from_utf8(v).ok())
        .and_then(|v| v.parse().ok())
}

#[inline]
fn attr_f64(tag: &[u8], name: &[u8]) -> Option<f64> {
    find_attr(tag, name)
        .and_then(|v| std::str::from_utf8(v).ok())
        .and_then(|v| v.parse().ok())
}

#[inline]
fn attr_u8(tag: &[u8], name: &[u8]) -> Option<u8> {
    find_attr(tag, name)
        .and_then(|v| std::str::from_utf8(v).ok())
        .and_then(|v| v.parse().ok())
}

fn attr_owned(tag: &[u8], name: &[u8], scratch: &mut Vec<u8>) -> Option<String> {
    find_attr(tag, name).map(|raw| String::from_utf8_lossy(decode_bytes(raw, scratch)).into_owned())
}

fn parse_a1_cell(s: &[u8]) -> Option<(u32, u32)> {
    // 0-based (row, col)
    let r = parse_range(s);
    Some((r.r0, r.c0))
}

/// Capture interesting `<row>` attrs. Returns None when only r/spans present.
pub fn parse_row_dim(row_tag: &[u8], sheet_row: Option<u32>) -> Option<RowDim> {
    // row_tag is bytes after `<row` up to `>` (may include leading space)
    let has_ht = find_attr(row_tag, b"ht").is_some();
    let has_hidden = find_attr(row_tag, b"hidden").is_some();
    let has_outline = find_attr(row_tag, b"outlineLevel").is_some();
    let has_collapsed = find_attr(row_tag, b"collapsed").is_some();
    let has_s = find_attr(row_tag, b"s").is_some();
    let has_custom_format = find_attr(row_tag, b"customFormat").is_some();
    let has_thick =
        find_attr(row_tag, b"thickBot").is_some() || find_attr(row_tag, b"thickTop").is_some();
    let has_custom_height = find_attr(row_tag, b"customHeight").is_some();
    if !(has_ht
        || has_hidden
        || has_outline
        || has_collapsed
        || has_s
        || has_custom_format
        || has_thick
        || has_custom_height)
    {
        return None;
    }
    let row = sheet_row.or_else(|| attr_u32(row_tag, b"r")).unwrap_or(1);
    Some(RowDim {
        row,
        height: attr_f64(row_tag, b"ht"),
        hidden: attr_bool_default(row_tag, b"hidden", false),
        outline_level: attr_u8(row_tag, b"outlineLevel").unwrap_or(0),
        collapsed: attr_bool_default(row_tag, b"collapsed", false),
        style: attr_u32(row_tag, b"s"),
    })
}

// ============================================================================
// Pre-sheetData header scan
// ============================================================================

/// Everything the pre-sheetData header scan extracts.
pub type SheetHeaderMeta = (
    Vec<ColDim>,
    Option<SheetFormat>,
    Option<SheetViewMeta>,
    Option<String>, // codeName
    Option<String>, // tabColor rgb/theme
    Option<bool>,   // fitToPage from pageSetUpPr
);

pub fn scan_sheet_header(header: &[u8]) -> SheetHeaderMeta {
    let mut scratch = Vec::new();
    let mut cols = Vec::new();
    let mut sheet_format = None;
    let mut sheet_view = None;
    let mut code_name = None;
    let mut tab_color = None;
    let mut fit_to_page = None;

    // sheetPr
    if let Some(o) = memchr::memmem::find(header, b"<sheetPr") {
        let te = o + memchr::memchr(b'>', &header[o..]).unwrap_or(header.len() - o);
        let tag = &header[o..te];
        code_name = attr_owned(tag, b"codeName", &mut scratch);
        // self-closing vs with children
        if header.get(te.saturating_sub(1)) != Some(&b'/') {
            let close = memchr::memmem::find(&header[te..], b"</sheetPr>")
                .map(|p| te + p)
                .unwrap_or(header.len());
            let inner = &header[te..close];
            if let Some(to) = memchr::memmem::find(inner, b"<tabColor") {
                let tte = to + memchr::memchr(b'>', &inner[to..]).unwrap_or(0);
                let ttag = &inner[to..tte];
                if let Some(rgb) = find_attr(ttag, b"rgb") {
                    tab_color = Some(String::from_utf8_lossy(rgb).into_owned());
                } else if let Some(th) = find_attr(ttag, b"theme") {
                    tab_color = Some(format!("theme:{}", String::from_utf8_lossy(th)));
                } else if let Some(idx) = find_attr(ttag, b"indexed") {
                    tab_color = Some(format!("indexed:{}", String::from_utf8_lossy(idx)));
                }
            }
            if let Some(po) = memchr::memmem::find(inner, b"<pageSetUpPr") {
                let pte = po + memchr::memchr(b'>', &inner[po..]).unwrap_or(0);
                fit_to_page = attr_bool(&inner[po..pte], b"fitToPage");
            }
        }
    }

    // sheetFormatPr
    if let Some(o) = memchr::memmem::find(header, b"<sheetFormatPr") {
        let te = o + memchr::memchr(b'>', &header[o..]).unwrap_or(header.len() - o);
        let tag = &header[o..te];
        sheet_format = Some(SheetFormat {
            base_col_width: attr_u32(tag, b"baseColWidth"),
            default_col_width: attr_f64(tag, b"defaultColWidth"),
            default_row_height: attr_f64(tag, b"defaultRowHeight"),
            custom_height: attr_bool(tag, b"customHeight"),
            zero_height: attr_bool(tag, b"zeroHeight"),
            outline_level_row: attr_u8(tag, b"outlineLevelRow"),
            outline_level_col: attr_u8(tag, b"outlineLevelCol"),
        });
    }

    // cols
    if let Some(co) = memchr::memmem::find(header, b"<cols") {
        let after = header.get(co + 5).copied().unwrap_or(b'>');
        if after == b' ' || after == b'>' || after == b'/' {
            let ce = memchr::memmem::find(&header[co..], b"</cols>")
                .map(|p| co + p)
                .unwrap_or(header.len());
            let region = &header[co..ce];
            let mut i = 0usize;
            while let Some(o) = memchr::memmem::find(&region[i..], b"<col ") {
                let start = i + o;
                let te =
                    start + memchr::memchr(b'>', &region[start..]).unwrap_or(region.len() - start);
                let tag = &region[start..te];
                let min = attr_u32(tag, b"min").unwrap_or(1);
                let max = attr_u32(tag, b"max").unwrap_or(min);
                // Malformed `min > max` — skip rather than emit inverted ranges.
                if min <= max {
                    cols.push(ColDim {
                        min,
                        max,
                        width: attr_f64(tag, b"width"),
                        hidden: attr_bool_default(tag, b"hidden", false),
                        best_fit: attr_bool_default(tag, b"bestFit", false),
                        outline_level: attr_u8(tag, b"outlineLevel").unwrap_or(0),
                        collapsed: attr_bool_default(tag, b"collapsed", false),
                        style: attr_u32(tag, b"style"),
                    });
                }
                i = te + 1;
            }
        }
    }

    // sheetViews — first sheetView only
    if let Some(so) = memchr::memmem::find(header, b"<sheetView ") {
        let te = so + memchr::memchr(b'>', &header[so..]).unwrap_or(header.len() - so);
        let tag = &header[so..te];
        let mut view = SheetViewMeta {
            show_grid_lines: attr_bool(tag, b"showGridLines"),
            zoom_scale: attr_u32(tag, b"zoomScale"),
            tab_selected: attr_bool(tag, b"tabSelected"),
            top_left_cell: find_attr(tag, b"topLeftCell").and_then(parse_a1_cell),
            workbook_view_id: attr_u32(tag, b"workbookViewId").unwrap_or(0),
            show_formulas: attr_bool(tag, b"showFormulas"),
            show_row_col_headers: attr_bool(tag, b"showRowColHeaders"),
            show_zeros: attr_bool(tag, b"showZeros"),
            right_to_left: attr_bool(tag, b"rightToLeft"),
            pane: None,
        };
        // pane child
        let view_end = if header.get(te.saturating_sub(1)) == Some(&b'/') {
            te
        } else {
            memchr::memmem::find(&header[te..], b"</sheetView>")
                .map(|p| te + p)
                .unwrap_or(header.len())
        };
        let inner = &header[te..view_end];
        if let Some(po) = memchr::memmem::find(inner, b"<pane ") {
            let pte = po + memchr::memchr(b'>', &inner[po..]).unwrap_or(0);
            let ptag = &inner[po..pte];
            let state = match find_attr(ptag, b"state") {
                Some(b"frozen") => PaneState::Frozen,
                Some(b"frozenSplit") => PaneState::FrozenSplit,
                _ => PaneState::Split,
            };
            let active = match find_attr(ptag, b"activePane") {
                Some(b"topRight") => ActivePane::TopRight,
                Some(b"bottomLeft") => ActivePane::BottomLeft,
                Some(b"topLeft") => ActivePane::TopLeft,
                _ => ActivePane::BottomRight,
            };
            view.pane = Some(Pane {
                x_split: attr_f64(ptag, b"xSplit"),
                y_split: attr_f64(ptag, b"ySplit"),
                top_left_cell: find_attr(ptag, b"topLeftCell").and_then(parse_a1_cell),
                active_pane: active,
                state,
            });
        }
        sheet_view = Some(view);
    } else if memchr::memmem::find(header, b"<sheetViews").is_some() {
        // empty / default view
        sheet_view = Some(SheetViewMeta {
            show_grid_lines: None,
            zoom_scale: None,
            tab_selected: None,
            top_left_cell: None,
            workbook_view_id: 0,
            show_formulas: None,
            show_row_col_headers: None,
            show_zeros: None,
            right_to_left: None,
            pane: None,
        });
    }

    (
        cols,
        sheet_format,
        sheet_view,
        code_name,
        tab_color,
        fit_to_page,
    )
}

// ============================================================================
// Tail scan (SHEET_META / PAGE_SETUP / VALIDATIONS / COND_FORMAT)
// ============================================================================

pub fn scan_auto_filter(tail: &[u8]) -> Option<AutoFilterMeta> {
    let o = memchr::memmem::find(tail, b"<autoFilter")?;
    let after = tail.get(o + 11).copied().unwrap_or(b'>');
    if !(after == b' ' || after == b'>' || after == b'/') {
        return None;
    }
    let te = o + memchr::memchr(b'>', &tail[o..])?;
    let tag = &tail[o..te];
    let refr = find_attr(tag, b"ref")?;
    let ref_ = parse_range(refr);
    let mut columns = Vec::new();
    if tail.get(te.saturating_sub(1)) != Some(&b'/') {
        let end = memchr::memmem::find(&tail[te..], b"</autoFilter>")
            .map(|p| te + p)
            .unwrap_or(tail.len());
        let inner = &tail[te..end];
        let mut i = 0usize;
        let mut scratch = Vec::new();
        while let Some(fo) = memchr::memmem::find(&inner[i..], b"<filterColumn ") {
            let start = i + fo;
            let fte = start + memchr::memchr(b'>', &inner[start..]).unwrap_or(inner.len() - start);
            let ftag = &inner[start..fte];
            let col_id = attr_u32(ftag, b"colId").unwrap_or(0);
            let hidden_button = attr_bool_default(ftag, b"hiddenButton", false);
            let show_button = attr_bool_default(ftag, b"showButton", true);
            let mut values = Vec::new();
            let mut blank = None;
            let fend = if inner.get(fte.saturating_sub(1)) == Some(&b'/') {
                fte
            } else {
                memchr::memmem::find(&inner[fte..], b"</filterColumn>")
                    .map(|p| fte + p)
                    .unwrap_or(inner.len())
            };
            let finner = &inner[fte..fend];
            // filters block
            if let Some(fo2) = memchr::memmem::find(finner, b"<filters") {
                let fte2 = fo2 + memchr::memchr(b'>', &finner[fo2..]).unwrap_or(0);
                let ftag2 = &finner[fo2..fte2];
                blank = attr_bool(ftag2, b"blank");
                let fend2 = if finner.get(fte2.saturating_sub(1)) == Some(&b'/') {
                    fte2
                } else {
                    memchr::memmem::find(&finner[fte2..], b"</filters>")
                        .map(|p| fte2 + p)
                        .unwrap_or(finner.len())
                };
                let mut j = fte2;
                while let Some(vo) = memchr::memmem::find(&finner[j..fend2], b"<filter ") {
                    let vs = j + vo;
                    let ve = vs + memchr::memchr(b'>', &finner[vs..]).unwrap_or(0);
                    if let Some(val) = attr_owned(&finner[vs..ve], b"val", &mut scratch) {
                        values.push(val);
                    }
                    j = ve + 1;
                }
            }
            columns.push(FilterColumnMeta {
                col_id,
                hidden_button,
                show_button,
                values,
                blank,
            });
            i = fend + 1;
        }
    }
    Some(AutoFilterMeta { ref_, columns })
}

pub fn scan_protection(tail: &[u8]) -> Option<SheetProtectionMeta> {
    let o = memchr::memmem::find(tail, b"<sheetProtection")?;
    let after = tail.get(o + 16).copied().unwrap_or(b'>');
    if !(after == b' ' || after == b'>' || after == b'/') {
        return None;
    }
    let te = o + memchr::memchr(b'>', &tail[o..])?;
    let tag = &tail[o..te];
    let mut scratch = Vec::new();
    // openpyxl defaults when attrs missing
    let mut p = SheetProtectionMeta::default();
    if let Some(v) = attr_bool(tag, b"sheet") {
        p.sheet = v;
    }
    if let Some(v) = attr_bool(tag, b"objects") {
        p.objects = v;
    }
    if let Some(v) = attr_bool(tag, b"scenarios") {
        p.scenarios = v;
    }
    if let Some(v) = attr_bool(tag, b"formatCells") {
        p.format_cells = v;
    }
    if let Some(v) = attr_bool(tag, b"formatColumns") {
        p.format_columns = v;
    }
    if let Some(v) = attr_bool(tag, b"formatRows") {
        p.format_rows = v;
    }
    if let Some(v) = attr_bool(tag, b"insertColumns") {
        p.insert_columns = v;
    }
    if let Some(v) = attr_bool(tag, b"insertRows") {
        p.insert_rows = v;
    }
    if let Some(v) = attr_bool(tag, b"insertHyperlinks") {
        p.insert_hyperlinks = v;
    }
    if let Some(v) = attr_bool(tag, b"deleteColumns") {
        p.delete_columns = v;
    }
    if let Some(v) = attr_bool(tag, b"deleteRows") {
        p.delete_rows = v;
    }
    if let Some(v) = attr_bool(tag, b"selectLockedCells") {
        p.select_locked_cells = v;
    }
    if let Some(v) = attr_bool(tag, b"selectUnlockedCells") {
        p.select_unlocked_cells = v;
    }
    if let Some(v) = attr_bool(tag, b"sort") {
        p.sort = v;
    }
    if let Some(v) = attr_bool(tag, b"autoFilter") {
        p.auto_filter = v;
    }
    if let Some(v) = attr_bool(tag, b"pivotTables") {
        p.pivot_tables = v;
    }
    p.password = attr_owned(tag, b"password", &mut scratch);
    p.algorithm_name = attr_owned(tag, b"algorithmName", &mut scratch);
    p.hash_value = attr_owned(tag, b"hashValue", &mut scratch);
    p.salt_value = attr_owned(tag, b"saltValue", &mut scratch);
    p.spin_count = attr_u32(tag, b"spinCount");
    Some(p)
}

pub fn scan_page_setup(tail: &[u8]) -> Option<PageSetupMeta> {
    let o = memchr::memmem::find(tail, b"<pageSetup")?;
    let after = tail.get(o + 10).copied().unwrap_or(b'>');
    if !(after == b' ' || after == b'>' || after == b'/') {
        return None;
    }
    let te = o + memchr::memchr(b'>', &tail[o..])?;
    let tag = &tail[o..te];
    let mut scratch = Vec::new();
    Some(PageSetupMeta {
        orientation: attr_owned(tag, b"orientation", &mut scratch),
        paper_size: attr_u32(tag, b"paperSize"),
        scale: attr_u32(tag, b"scale"),
        fit_to_width: attr_u32(tag, b"fitToWidth"),
        fit_to_height: attr_u32(tag, b"fitToHeight"),
        fit_to_page: None, // from sheetPr/pageSetUpPr
        first_page_number: attr_u32(tag, b"firstPageNumber"),
        page_order: attr_owned(tag, b"pageOrder", &mut scratch),
        black_and_white: attr_bool(tag, b"blackAndWhite"),
        draft: attr_bool(tag, b"draft"),
    })
}

pub fn scan_page_margins(tail: &[u8]) -> Option<PageMarginsMeta> {
    let o = memchr::memmem::find(tail, b"<pageMargins")?;
    let after = tail.get(o + 12).copied().unwrap_or(b'>');
    if !(after == b' ' || after == b'>' || after == b'/') {
        return None;
    }
    let te = o + memchr::memchr(b'>', &tail[o..])?;
    let tag = &tail[o..te];
    let d = PageMarginsMeta::default();
    Some(PageMarginsMeta {
        left: attr_f64(tag, b"left").unwrap_or(d.left),
        right: attr_f64(tag, b"right").unwrap_or(d.right),
        top: attr_f64(tag, b"top").unwrap_or(d.top),
        bottom: attr_f64(tag, b"bottom").unwrap_or(d.bottom),
        header: attr_f64(tag, b"header").unwrap_or(d.header),
        footer: attr_f64(tag, b"footer").unwrap_or(d.footer),
    })
}

pub fn scan_print_options(tail: &[u8]) -> Option<PrintOptionsMeta> {
    let o = memchr::memmem::find(tail, b"<printOptions")?;
    let after = tail.get(o + 13).copied().unwrap_or(b'>');
    if !(after == b' ' || after == b'>' || after == b'/') {
        return None;
    }
    let te = o + memchr::memchr(b'>', &tail[o..])?;
    let tag = &tail[o..te];
    Some(PrintOptionsMeta {
        horizontal_centered: attr_bool(tag, b"horizontalCentered"),
        vertical_centered: attr_bool(tag, b"verticalCentered"),
        headings: attr_bool(tag, b"headings"),
        grid_lines: attr_bool(tag, b"gridLines"),
        grid_lines_set: attr_bool(tag, b"gridLinesSet"),
    })
}

fn text_child(parent: &[u8], name: &str, scratch: &mut Vec<u8>) -> Option<String> {
    let open = format!("<{}", name);
    let close = format!("</{}>", name);
    let o = memchr::memmem::find(parent, open.as_bytes())?;
    let after = parent.get(o + open.len()).copied().unwrap_or(b'>');
    if !(after == b' ' || after == b'>' || after == b'/') {
        return None;
    }
    let te = o + memchr::memchr(b'>', &parent[o..])?;
    if parent.get(te.saturating_sub(1)) == Some(&b'/') {
        return Some(String::new());
    }
    let ce = memchr::memmem::find(&parent[te..], close.as_bytes()).map(|p| te + p)?;
    let raw = &parent[te + 1..ce];
    Some(String::from_utf8_lossy(decode_bytes(raw, scratch)).into_owned())
}

pub fn scan_header_footer(tail: &[u8]) -> Option<HeaderFooterMeta> {
    let o = memchr::memmem::find(tail, b"<headerFooter")?;
    let after = tail.get(o + 13).copied().unwrap_or(b'>');
    if !(after == b' ' || after == b'>' || after == b'/') {
        return None;
    }
    let te = o + memchr::memchr(b'>', &tail[o..])?;
    let tag = &tail[o..te];
    let mut hf = HeaderFooterMeta {
        different_odd_even: attr_bool(tag, b"differentOddEven"),
        different_first: attr_bool(tag, b"differentFirst"),
        scale_with_doc: attr_bool(tag, b"scaleWithDoc"),
        align_with_margins: attr_bool(tag, b"alignWithMargins"),
        ..Default::default()
    };
    if tail.get(te.saturating_sub(1)) != Some(&b'/') {
        let end = memchr::memmem::find(&tail[te..], b"</headerFooter>")
            .map(|p| te + p)
            .unwrap_or(tail.len());
        let inner = &tail[te..end];
        let mut scratch = Vec::new();
        hf.odd_header = text_child(inner, "oddHeader", &mut scratch);
        hf.odd_footer = text_child(inner, "oddFooter", &mut scratch);
        hf.even_header = text_child(inner, "evenHeader", &mut scratch);
        hf.even_footer = text_child(inner, "evenFooter", &mut scratch);
        hf.first_header = text_child(inner, "firstHeader", &mut scratch);
        hf.first_footer = text_child(inner, "firstFooter", &mut scratch);
    }
    Some(hf)
}

// ----------------------------------------------------------------------------
// Data validations (B4)
// ----------------------------------------------------------------------------

pub fn scan_data_validations(tail: &[u8]) -> Vec<DataValidationRec> {
    let mut out = Vec::new();
    let mut scratch = Vec::new();
    // only main-namespace dataValidations (not x14)
    let Some(block_o) = memchr::memmem::find(tail, b"<dataValidations") else {
        return out;
    };
    let after = tail.get(block_o + 16).copied().unwrap_or(b'>');
    if !(after == b' ' || after == b'>' || after == b'/') {
        return out;
    }
    let block_te = block_o + memchr::memchr(b'>', &tail[block_o..]).unwrap_or(0);
    if tail.get(block_te.saturating_sub(1)) == Some(&b'/') {
        return out;
    }
    let block_end = memchr::memmem::find(&tail[block_te..], b"</dataValidations>")
        .map(|p| block_te + p)
        .unwrap_or(tail.len());
    let region = &tail[block_te..block_end];
    let mut i = 0usize;
    while let Some(o) = memchr::memmem::find(&region[i..], b"<dataValidation ") {
        let start = i + o;
        let te = start + memchr::memchr(b'>', &region[start..]).unwrap_or(region.len() - start);
        let tag = &region[start..te];
        let self_closing = region.get(te.saturating_sub(1)) == Some(&b'/');
        let sqref = attr_owned(tag, b"sqref", &mut scratch).unwrap_or_default();
        let mut formula1 = None;
        let mut formula2 = None;
        let end = if self_closing {
            te
        } else {
            memchr::memmem::find(&region[te..], b"</dataValidation>")
                .map(|p| te + p)
                .unwrap_or(region.len())
        };
        // Empty sqref is malformed / inert — skip (clean degradation).
        if sqref.trim().is_empty() {
            i = end + 1;
            continue;
        }
        if !self_closing {
            let inner = &region[te..end];
            formula1 = text_child(inner, "formula1", &mut scratch);
            formula2 = text_child(inner, "formula2", &mut scratch);
        }
        out.push(DataValidationRec {
            type_: attr_owned(tag, b"type", &mut scratch),
            operator: attr_owned(tag, b"operator", &mut scratch),
            allow_blank: attr_bool_default(tag, b"allowBlank", false),
            show_input_message: attr_bool_default(tag, b"showInputMessage", false),
            show_error_message: attr_bool_default(tag, b"showErrorMessage", false),
            show_drop_down: attr_bool_default(tag, b"showDropDown", false),
            error_style: attr_owned(tag, b"errorStyle", &mut scratch),
            sqref,
            formula1,
            formula2,
            prompt_title: attr_owned(tag, b"promptTitle", &mut scratch),
            prompt: attr_owned(tag, b"prompt", &mut scratch),
            error_title: attr_owned(tag, b"errorTitle", &mut scratch),
            error: attr_owned(tag, b"error", &mut scratch),
        });
        i = end + 1;
    }
    out
}

// ----------------------------------------------------------------------------
// Conditional formatting (B5)
// ----------------------------------------------------------------------------

fn parse_cfvos(region: &[u8]) -> Vec<CfVo> {
    let mut out = Vec::new();
    let mut scratch = Vec::new();
    let mut i = 0usize;
    while let Some(o) = memchr::memmem::find(&region[i..], b"<cfvo ") {
        let start = i + o;
        let te = start + memchr::memchr(b'>', &region[start..]).unwrap_or(region.len() - start);
        let tag = &region[start..te];
        out.push(CfVo {
            type_: attr_owned(tag, b"type", &mut scratch).unwrap_or_default(),
            val: attr_owned(tag, b"val", &mut scratch),
            gte: attr_bool(tag, b"gte"),
        });
        i = te + 1;
    }
    out
}

fn parse_cf_colors(region: &[u8]) -> Vec<Color> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(o) = memchr::memmem::find(&region[i..], b"<color ") {
        let start = i + o;
        let te = start + memchr::memchr(b'>', &region[start..]).unwrap_or(region.len() - start);
        // open-tag slice without leading '<'
        let open = &region[start + 1..te];
        out.push(parse_color(open));
        i = te + 1;
    }
    // also <color/> self-closing with no space after color
    out
}

pub fn scan_conditional_formatting(tail: &[u8]) -> Vec<CfRuleRec> {
    let mut out = Vec::new();
    let mut scratch = Vec::new();
    let mut i = 0usize;
    let n = tail.len();
    while let Some(o) = memchr::memmem::find(&tail[i..n], b"<conditionalFormatting ") {
        let start = i + o;
        let te = start + memchr::memchr(b'>', &tail[start..n]).unwrap_or(n - start);
        let tag = &tail[start..te];
        let sqref = attr_owned(tag, b"sqref", &mut scratch).unwrap_or_default();
        if tail.get(te.saturating_sub(1)) == Some(&b'/') {
            i = te + 1;
            continue;
        }
        let end = memchr::memmem::find(&tail[te..n], b"</conditionalFormatting>")
            .map(|p| te + p)
            .unwrap_or(n);
        let region = &tail[te..end];
        let mut j = 0usize;
        while let Some(ro) = memchr::memmem::find(&region[j..], b"<cfRule ") {
            let rs = j + ro;
            let rte = rs + memchr::memchr(b'>', &region[rs..]).unwrap_or(region.len() - rs);
            let rtag = &region[rs..rte];
            let self_closing = region.get(rte.saturating_sub(1)) == Some(&b'/');
            let rtype = attr_owned(rtag, b"type", &mut scratch).unwrap_or_default();
            let priority = find_attr(rtag, b"priority")
                .and_then(|v| std::str::from_utf8(v).ok())
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let mut formulas = Vec::new();
            let mut color_scale = None;
            let mut data_bar = None;
            let mut icon_set = None;
            let rend = if self_closing {
                rte
            } else {
                memchr::memmem::find(&region[rte..], b"</cfRule>")
                    .map(|p| rte + p)
                    .unwrap_or(region.len())
            };
            if !self_closing {
                let inner = &region[rte..rend];
                // formulas
                let mut k = 0usize;
                while let Some(fo) = memchr::memmem::find(&inner[k..], b"<formula") {
                    let fs = k + fo;
                    let after = inner.get(fs + 8).copied().unwrap_or(b'>');
                    if !(after == b' ' || after == b'>' || after == b'/') {
                        k = fs + 8;
                        continue;
                    }
                    let fte = fs + memchr::memchr(b'>', &inner[fs..]).unwrap_or(0);
                    if inner.get(fte.saturating_sub(1)) == Some(&b'/') {
                        k = fte + 1;
                        continue;
                    }
                    let fce = memchr::memmem::find(&inner[fte..], b"</formula>")
                        .map(|p| fte + p)
                        .unwrap_or(inner.len());
                    let raw = &inner[fte + 1..fce];
                    formulas.push(
                        String::from_utf8_lossy(decode_bytes(raw, &mut scratch)).into_owned(),
                    );
                    k = fce + 10;
                }
                if let Some(cso) = memchr::memmem::find(inner, b"<colorScale") {
                    let cse = memchr::memmem::find(&inner[cso..], b"</colorScale>")
                        .map(|p| cso + p)
                        .unwrap_or(inner.len());
                    let creg = &inner[cso..cse];
                    color_scale = Some(ColorScaleParams {
                        cfvo: parse_cfvos(creg),
                        colors: parse_cf_colors(creg),
                    });
                }
                if let Some(dbo) = memchr::memmem::find(inner, b"<dataBar") {
                    let dbe = if inner[dbo..].starts_with(b"<dataBar/")
                        || memchr::memmem::find(&inner[dbo..dbo + 20.min(inner.len() - dbo)], b"/>")
                            .is_some()
                            && memchr::memmem::find(&inner[dbo..], b"</dataBar>").is_none()
                    {
                        dbo + memchr::memchr(b'>', &inner[dbo..]).unwrap_or(0) + 1
                    } else {
                        memchr::memmem::find(&inner[dbo..], b"</dataBar>")
                            .map(|p| dbo + p)
                            .unwrap_or(inner.len())
                    };
                    let dreg = &inner[dbo..dbe];
                    let dte = dbo + memchr::memchr(b'>', &inner[dbo..]).unwrap_or(0);
                    let dtag = &inner[dbo..dte];
                    let colors = parse_cf_colors(dreg);
                    data_bar = Some(DataBarParams {
                        cfvo: parse_cfvos(dreg),
                        color: colors.first().copied().unwrap_or_else(Color::default_rgb),
                        show_value: attr_bool(dtag, b"showValue"),
                        min_length: attr_u32(dtag, b"minLength"),
                        max_length: attr_u32(dtag, b"maxLength"),
                    });
                }
                if let Some(iso) = memchr::memmem::find(inner, b"<iconSet") {
                    let ise = memchr::memmem::find(&inner[iso..], b"</iconSet>")
                        .map(|p| iso + p)
                        .unwrap_or_else(|| {
                            iso + memchr::memchr(b'>', &inner[iso..]).unwrap_or(0) + 1
                        });
                    let ireg = &inner[iso..ise];
                    let ite = iso + memchr::memchr(b'>', &inner[iso..]).unwrap_or(0);
                    let itag = &inner[iso..ite];
                    icon_set = Some(IconSetParams {
                        icon_set: attr_owned(itag, b"iconSet", &mut scratch),
                        cfvo: parse_cfvos(ireg),
                        show_value: attr_bool(itag, b"showValue"),
                        percent: attr_bool(itag, b"percent"),
                        reverse: attr_bool(itag, b"reverse"),
                    });
                }
            }
            out.push(CfRuleRec {
                sqref: sqref.clone(),
                type_: rtype,
                priority,
                operator: attr_owned(rtag, b"operator", &mut scratch),
                stop_if_true: attr_bool(rtag, b"stopIfTrue"),
                dxf_id: attr_u32(rtag, b"dxfId"),
                formulas,
                text: attr_owned(rtag, b"text", &mut scratch),
                rank: find_attr(rtag, b"rank")
                    .and_then(|v| std::str::from_utf8(v).ok())
                    .and_then(|v| v.parse().ok()),
                percent: attr_bool(rtag, b"percent"),
                bottom: attr_bool(rtag, b"bottom"),
                above_average: attr_bool(rtag, b"aboveAverage"),
                equal_average: attr_bool(rtag, b"equalAverage"),
                std_dev: find_attr(rtag, b"stdDev")
                    .and_then(|v| std::str::from_utf8(v).ok())
                    .and_then(|v| v.parse().ok()),
                time_period: attr_owned(rtag, b"timePeriod", &mut scratch),
                color_scale,
                data_bar,
                icon_set,
            });
            j = rend + 1;
        }
        i = end + 1;
    }
    out
}

// ============================================================================
// Workbook props (A7)
// ============================================================================

fn local_elem_text(xml: &[u8], local: &str, scratch: &mut Vec<u8>) -> Option<String> {
    // match <local> or <ns:local>
    let needle = format!(":{}", local);
    let bare = format!("<{}", local);
    let mut pos = 0usize;
    while pos < xml.len() {
        let rel = if let Some(o) = memchr::memmem::find(&xml[pos..], bare.as_bytes()) {
            Some(o)
        } else if let Some(o) = memchr::memmem::find(&xml[pos..], needle.as_bytes()) {
            // ensure preceded by '<' or letter for ns prefix — find '<' before
            let abs = pos + o;
            let mut s = abs;
            while s > pos && xml[s] != b'<' {
                s -= 1;
            }
            if xml.get(s) == Some(&b'<') {
                Some(s - pos)
            } else {
                None
            }
        } else {
            None
        };
        let Some(o) = rel else { break };
        let start = pos + o;
        // verify tag name ends correctly
        let after_name = start
            + if xml[start + 1..].starts_with(local.as_bytes()) {
                1 + local.len()
            } else {
                // ns:local
                let colon = memchr::memchr(b':', &xml[start..]).map(|p| start + p)?;
                if !xml[colon + 1..].starts_with(local.as_bytes()) {
                    pos = start + 1;
                    continue;
                }
                colon + 1 + local.len() - start
            };
        let after = xml.get(after_name).copied().unwrap_or(b'>');
        if !(after == b' ' || after == b'>' || after == b'/') {
            pos = start + 1;
            continue;
        }
        let te = start + memchr::memchr(b'>', &xml[start..])?;
        if xml.get(te.saturating_sub(1)) == Some(&b'/') {
            return Some(String::new());
        }
        // close tag: </local> or </ns:local>
        let close_bare = format!("</{}", local);
        if let Some(ce) = memchr::memmem::find(&xml[te..], close_bare.as_bytes()) {
            let raw = &xml[te + 1..te + ce];
            return Some(String::from_utf8_lossy(decode_bytes(raw, scratch)).into_owned());
        }
        // namespaced close
        let close_ns = format!(":{}", local);
        let mut k = te;
        while let Some(co) = memchr::memmem::find(&xml[k..], close_ns.as_bytes()) {
            let abs = k + co;
            // look back for </
            if abs >= 2 && xml[abs - 2] == b'<' && xml[abs - 1] == b'/'
                || (abs >= 3 && xml[abs - 1] != b'/'/* ns prefix */)
            {
                // find '<'
                let mut s = abs;
                while s > te && xml[s] != b'<' {
                    s -= 1;
                }
                if xml.get(s) == Some(&b'<') && xml.get(s + 1) == Some(&b'/') {
                    let raw = &xml[te + 1..s];
                    return Some(String::from_utf8_lossy(decode_bytes(raw, scratch)).into_owned());
                }
            }
            k = abs + 1;
        }
        pos = start + 1;
    }
    None
}

pub fn parse_core_props(xml: &[u8]) -> CoreProps {
    let mut scratch = Vec::new();
    CoreProps {
        title: local_elem_text(xml, "title", &mut scratch),
        creator: local_elem_text(xml, "creator", &mut scratch),
        description: local_elem_text(xml, "description", &mut scratch),
        subject: local_elem_text(xml, "subject", &mut scratch),
        last_modified_by: local_elem_text(xml, "lastModifiedBy", &mut scratch),
        created: local_elem_text(xml, "created", &mut scratch),
        modified: local_elem_text(xml, "modified", &mut scratch),
        category: local_elem_text(xml, "category", &mut scratch),
        keywords: local_elem_text(xml, "keywords", &mut scratch),
        revision: local_elem_text(xml, "revision", &mut scratch),
        version: local_elem_text(xml, "version", &mut scratch),
        content_status: local_elem_text(xml, "contentStatus", &mut scratch),
        language: local_elem_text(xml, "language", &mut scratch),
        identifier: local_elem_text(xml, "identifier", &mut scratch),
    }
}

pub fn parse_app_props(xml: &[u8]) -> AppProps {
    let mut scratch = Vec::new();
    AppProps {
        application: local_elem_text(xml, "Application", &mut scratch),
        app_version: local_elem_text(xml, "AppVersion", &mut scratch),
    }
}

/// Parse date1904 + optional full workbook props from workbook.xml.
pub fn parse_workbook_pr(xml: &[u8]) -> (bool, Option<String>, Option<bool>, Option<u32>) {
    let mut date1904 = false;
    let mut code_name = None;
    let mut full_calc = None;
    let mut calc_id = None;
    let mut scratch = Vec::new();
    if let Some(o) = memchr::memmem::find(xml, b"<workbookPr") {
        let te = o + memchr::memchr(b'>', &xml[o..]).unwrap_or(0);
        let tag = &xml[o..te];
        date1904 = attr_bool_default(tag, b"date1904", false);
        code_name = attr_owned(tag, b"codeName", &mut scratch);
    }
    if let Some(o) = memchr::memmem::find(xml, b"<calcPr") {
        let te = o + memchr::memchr(b'>', &xml[o..]).unwrap_or(0);
        let tag = &xml[o..te];
        full_calc = attr_bool(tag, b"fullCalcOnLoad");
        calc_id = attr_u32(tag, b"calcId");
    }
    (date1904, code_name, full_calc, calc_id)
}
