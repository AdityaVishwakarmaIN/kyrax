//! Workbook / sheet / cell model for the turbo write path (silo A + B + C).

use ahash::AHashMap;
use std::sync::Arc;

use crate::turbo::calc::spill::SpillRegion;
use crate::turbo::meta::AutoFilterMeta;

use super::cf_dv::{ConditionalFormatting, DataValidation};
use super::charts::{Anchor, Chart, ChartsheetSpec};
use super::pivot::{PivotAgg, PivotDataField, PivotField, PivotTableSpec};
use super::rich_text::RichText;
use super::style_engine::StyleDesc;

/// String emission strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringMode {
    /// Match openpyxl: `t="inlineStr"` + embedded text.
    InlineStr,
    /// OOXML sharedStrings.xml + `t="s"` indices.
    SharedStrings,
    /// Choose SST when unique/total ratio is below [`AUTO_SST_THRESHOLD`].
    Auto,
}

/// Default unique/total ratio below which Auto picks SharedStrings.
pub const AUTO_SST_THRESHOLD: f64 = 0.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "visible" => Some(SheetState::Visible),
            "hidden" => Some(SheetState::Hidden),
            "veryHidden" | "veryhidden" => Some(SheetState::VeryHidden),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum FormulaKind {
    Normal,
    Array {
        ref_: String,
    },
    DataTable {
        ref_: String,
        dt2d: bool,
        dtr: bool,
        r1: Option<String>,
        r2: Option<String>,
        del1: bool,
        del2: bool,
        ca: bool,
    },
}

/// Cached formula result (enhancement over openpyxl write path).
#[derive(Debug, Clone, PartialEq)]
pub enum CachedValue {
    Number(f64),
    Bool(bool),
    Error(String),
    Str(String),
}

#[derive(Debug, Clone)]
pub enum CellValue {
    /// Skip emission when no style (ledger 23).
    Empty,
    Number(f64),
    Bool(bool),
    Error(String),
    Str(String),
    /// Excel serial day number (Windows 1900 system by default).
    DateSerial(f64),
    /// Time fraction of day serial [0.0, 1.0)
    Time(f64),
    /// Duration in days (e.g. for [h]:mm:ss)
    Duration(f64),
    /// Multi-run rich text (`t="inlineStr"` with `<r>` children).
    Rich(RichText),
    Formula {
        text: String,
        kind: FormulaKind,
        cached: Option<CachedValue>,
    },
}

/// Validate an Excel worksheet name according to ECMA-376 and Excel UI rules.
#[allow(dead_code)]
pub fn validate_sheet_name(
    name: &str,
    existing: &[String],
) -> Result<(), crate::turbo::error::TurboError> {
    if name.is_empty() {
        return Err(crate::turbo::error::TurboError::Format(
            "sheet name cannot be empty".into(),
        ));
    }
    let char_len = name.chars().count();
    if char_len > 31 {
        return Err(crate::turbo::error::TurboError::Format(format!(
            "sheet name \"{name}\" exceeds maximum length of 31 characters (got {char_len})"
        )));
    }
    const INVALID_CHARS: [char; 7] = [':', '\\', '/', '?', '*', '[', ']'];
    for c in name.chars() {
        if INVALID_CHARS.contains(&c) {
            return Err(crate::turbo::error::TurboError::Format(format!(
                "sheet name \"{name}\" contains invalid character '{c}'"
            )));
        }
    }
    if name.starts_with('\'') || name.ends_with('\'') {
        return Err(crate::turbo::error::TurboError::Format(format!(
            "sheet name \"{name}\" cannot start or end with an apostrophe"
        )));
    }
    if name.eq_ignore_ascii_case("History") {
        return Err(crate::turbo::error::TurboError::Format(
            "sheet name \"History\" is reserved by Excel".into(),
        ));
    }
    for ex in existing {
        if name.eq_ignore_ascii_case(ex) {
            return Err(crate::turbo::error::TurboError::Format(format!(
                "sheet name \"{name}\" already exists (case-insensitive duplicate of \"{ex}\")"
            )));
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct Cell {
    /// 1-based column index.
    pub col: u32,
    pub value: CellValue,
    /// Resolved cellXf index (`s` attribute). Omit when None or 0.
    pub style: Option<u32>,
    /// Pending style descriptor (resolved into `style` at write time).
    /// Boxed so unstyled cells stay small (pay-for-what-you-use).
    pub style_desc: Option<Box<StyleDesc>>,
}

impl Cell {
    pub fn new(col: u32, value: CellValue) -> Self {
        Self {
            col,
            value,
            style: None,
            style_desc: None,
        }
    }

    pub fn with_style_desc(mut self, desc: StyleDesc) -> Self {
        self.style_desc = Some(Box::new(desc));
        self
    }
}

#[derive(Debug, Clone)]
pub struct Row {
    /// 1-based row index.
    pub row: u32,
    pub cells: Vec<Cell>,
    pub height: Option<f64>,
    pub hidden: bool,
    pub style: Option<u32>,
    pub custom_height: bool,
}

impl Row {
    pub fn new(row: u32) -> Self {
        Self {
            row,
            cells: Vec::new(),
            height: None,
            hidden: false,
            style: None,
            custom_height: false,
        }
    }

    pub fn with_cell(mut self, col: u32, value: CellValue) -> Self {
        self.cells.push(Cell::new(col, value));
        self
    }
}

#[derive(Debug, Clone)]
pub struct ColDim {
    pub min: u32,
    pub max: u32,
    pub width: Option<f64>,
    pub hidden: bool,
    pub style: Option<u32>,
    pub best_fit: bool,
    pub custom_width: bool,
    pub outline_level: u8,
}

/// Sheet view freeze pane (A1 top-left of unfrozen region), silo A basics.
#[derive(Debug, Clone, Default)]
pub struct SheetViewOpts {
    /// e.g. `"B2"` freezes rows above and cols left of B2.
    pub freeze_cell: Option<String>,
}

// ── Silo C structural model ──────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct DocProps {
    pub title: Option<String>,
    pub creator: Option<String>,
    pub description: Option<String>,
    pub subject: Option<String>,
    pub last_modified_by: Option<String>,
    pub company: Option<String>,
    pub created: Option<String>,
    pub modified: Option<String>,
    pub custom: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DefinedName {
    pub name: String,
    pub value: String,
    pub local_sheet_id: Option<u32>,
    pub hidden: bool,
}

#[derive(Debug, Clone)]
pub struct ExternalLink {
    pub target: String,
}

#[derive(Debug, Clone)]
pub struct SheetProtection {
    pub sheet: bool,
    pub password: Option<String>,
    pub already_hashed: bool,
}

#[derive(Debug, Clone)]
pub struct Scenario {
    pub name: String,
    pub cells: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct Hyperlink {
    pub ref_: String,
    pub target: Option<String>,
    pub location: Option<String>,
    pub display: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PrintOptions {
    pub horizontal_centered: bool,
    pub vertical_centered: bool,
    pub headings: bool,
    pub grid_lines: bool,
}

#[derive(Debug, Clone)]
pub struct PageMargins {
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
    pub header: f64,
    pub footer: f64,
}

impl Default for PageMargins {
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

#[derive(Debug, Clone, Default)]
pub struct PageSetup {
    pub orientation: Option<String>,
    pub paper_size: Option<i32>,
    pub fit_to_page: bool,
    pub fit_to_width: Option<i32>,
    pub fit_to_height: Option<i32>,
    pub scale: Option<i32>,
}

#[derive(Debug, Clone, Default)]
pub struct HeaderFooter {
    pub odd_header_center: Option<String>,
    pub odd_header_left: Option<String>,
    pub odd_header_right: Option<String>,
    pub odd_footer_center: Option<String>,
    pub odd_footer_left: Option<String>,
    pub odd_footer_right: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TableDef {
    pub display_name: String,
    pub ref_: String,
    pub columns: Vec<String>,
    pub style_name: Option<String>,
    pub show_row_stripes: bool,
}

#[derive(Debug, Clone)]
pub struct Comment {
    pub ref_: String,
    pub author: String,
    pub text: String,
    pub height: u32,
    pub width: u32,
}

impl Default for Comment {
    fn default() -> Self {
        Self {
            ref_: String::new(),
            author: "Author".into(),
            text: String::new(),
            height: 79,
            width: 144,
        }
    }
}

/// Image file format, detected from magic bytes (never from a file extension).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Gif,
}

impl ImageFormat {
    /// ZIP part extension for the media file (`xl/media/imageN.{ext}`).
    pub fn extension(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpeg",
            ImageFormat::Gif => "gif",
        }
    }

    /// `[Content_Types].xml` Default content type for the extension.
    pub fn content_type(self) -> &'static str {
        match self {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::Gif => "image/gif",
        }
    }
}

/// Detect the image format from leading magic bytes. The extension is never
/// trusted; PNG/JPEG/GIF signatures are definitive.
pub fn detect_image_format(bytes: &[u8]) -> Option<ImageFormat> {
    if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some(ImageFormat::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(ImageFormat::Jpeg)
    } else if bytes.starts_with(b"GIF8") {
        Some(ImageFormat::Gif)
    } else {
        None
    }
}

/// An image placed on a worksheet. `bytes` are shared via `Arc` so dedup
/// across sheets is cheap; the media part index is resolved by the
/// [`super::media::MediaInterner`] at write time.
#[derive(Debug, Clone)]
pub struct Image {
    pub bytes: Arc<[u8]>,
    pub format: ImageFormat,
    pub anchor: Anchor,
}

#[derive(Debug, Clone)]
pub struct Sheet {
    pub name: String,
    pub state: SheetState,
    pub rows: Vec<Row>,
    pub cols: Vec<ColDim>,
    pub default_row_height: f64,
    pub base_col_width: u32,
    pub default_col_width: Option<f64>,
    /// If set, used as dimension ref; else computed from cells.
    pub dimension: Option<String>,
    pub code_name: Option<String>,
    pub view: SheetViewOpts,
    /// Conditional formatting ranges (CF dxfs registered before styles.xml).
    pub conditional_formatting: Vec<ConditionalFormatting>,
    /// Data validations.
    pub data_validations: Vec<DataValidation>,
    /// Pending row style descriptors (row_num 1-based → StyleDesc).
    pub row_style_descs: Vec<(u32, StyleDesc)>,
    /// Pending col style descriptors (index into cols).
    pub col_style_descs: Vec<(usize, StyleDesc)>,
    /// Dates / rich / styles / CF / DV present — set by builders (no O(n) rescan).
    pub needs_style_work: bool,
    // Silo C structural
    pub tab_color_rgb: Option<String>,
    pub protection: Option<SheetProtection>,
    pub scenarios: Vec<Scenario>,
    /// AutoFilter ref + filter columns. Reuses the read-side structure
    /// (`crate::turbo::meta::AutoFilterMeta`) so a read-modify-write round trip
    /// moves the exact model the reader parsed, nothing invented on the write side.
    /// The reader only parses value filters today (`<filters>` + `<filter val>`);
    /// customFilters / top10 / dynamicFilter / colorFilter / iconFilter / sortState
    /// have no read model yet, so they are a known remaining read/write gap.
    pub auto_filter: Option<AutoFilterMeta>,
    pub merges: Vec<String>,
    pub hyperlinks: Vec<Hyperlink>,
    pub print_options: Option<PrintOptions>,
    pub page_margins: Option<PageMargins>,
    pub page_setup: Option<PageSetup>,
    pub header_footer: Option<HeaderFooter>,
    pub row_breaks: Vec<u32>,
    pub col_breaks: Vec<u32>,
    pub tables: Vec<TableDef>,
    pub comments: Vec<Comment>,
    pub charts: Vec<Chart>,
    pub images: Vec<Image>,
    pub print_area: Option<String>,
    pub print_titles: Option<String>,
    /// Pivot tables authored on this sheet (Task B5b).
    pub pivots: Vec<PivotTableSpec>,
    /// Active dynamic array spill regions persisted on this sheet.
    pub spills: Vec<SpillRegion>,
}

impl Sheet {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            state: SheetState::Visible,
            rows: Vec::new(),
            cols: Vec::new(),
            default_row_height: 15.0,
            base_col_width: 8,
            default_col_width: None,
            dimension: None,
            code_name: None,
            view: SheetViewOpts::default(),
            conditional_formatting: Vec::new(),
            data_validations: Vec::new(),
            row_style_descs: Vec::new(),
            col_style_descs: Vec::new(),
            needs_style_work: false,
            tab_color_rgb: None,
            protection: None,
            scenarios: Vec::new(),
            auto_filter: None,
            merges: Vec::new(),
            hyperlinks: Vec::new(),
            print_options: None,
            page_margins: None,
            page_setup: None,
            header_footer: None,
            row_breaks: Vec::new(),
            col_breaks: Vec::new(),
            tables: Vec::new(),
            comments: Vec::new(),
            charts: Vec::new(),
            images: Vec::new(),
            print_area: None,
            print_titles: None,
            pivots: Vec::new(),
            spills: Vec::new(),
        }
    }

    pub fn bounds(&self) -> Option<(u32, u32, u32, u32)> {
        let mut min_r = u32::MAX;
        let mut max_r = 0u32;
        let mut min_c = u32::MAX;
        let mut max_c = 0u32;
        let mut any = false;
        for row in &self.rows {
            if row.cells.is_empty() && row.height.is_none() && row.style.is_none() && !row.hidden {
                continue;
            }
            any = true;
            min_r = min_r.min(row.row);
            max_r = max_r.max(row.row);
            for c in &row.cells {
                min_c = min_c.min(c.col);
                max_c = max_c.max(c.col);
            }
            if row.cells.is_empty() {
                min_c = min_c.min(1);
                max_c = max_c.max(1);
            }
        }
        if !any {
            None
        } else {
            if min_c == u32::MAX {
                min_c = 1;
                max_c = 1;
            }
            Some((min_r, min_c, max_r, max_c))
        }
    }
}

/// Write-side feature flags (pay-for-what-you-use; structural families W3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WriteFeatures(pub u32);

impl WriteFeatures {
    /// Typed cell values (always on for write).
    pub const VALUES: WriteFeatures = WriteFeatures(1 << 0);
    /// Formula cells + optional cached values.
    pub const FORMULAS: WriteFeatures = WriteFeatures(1 << 1);
    /// Row/col dimensions + sheet views/freeze basics.
    pub const DIMS: WriteFeatures = WriteFeatures(1 << 2);
    /// Cell style indices + styles.xml StyleEngine (W2).
    pub const STYLES: WriteFeatures = WriteFeatures(1 << 3);
    /// Merges + sheet protection / scenarios / autoFilter / print stack.
    pub const MERGES: WriteFeatures = WriteFeatures(1 << 4);
    /// Hyperlinks (+ sheet rels).
    pub const HYPERLINKS: WriteFeatures = WriteFeatures(1 << 5);
    /// Comments + VML + legacyDrawing.
    pub const COMMENTS: WriteFeatures = WriteFeatures(1 << 6);
    /// Tables as parts + tableParts.
    pub const TABLES: WriteFeatures = WriteFeatures(1 << 7);
    /// Defined names incl. Print_Area / Print_Titles / _FilterDatabase.
    pub const DEFINED_NAMES: WriteFeatures = WriteFeatures(1 << 8);
    /// Conditional formatting / data validations (W2).
    pub const CF_DV: WriteFeatures = WriteFeatures(1 << 9);
    /// Charts / drawings / chartsheets.
    pub const CHARTS: WriteFeatures = WriteFeatures(1 << 10);
    /// Full core/app/custom props + workbook protection.
    pub const PROPS: WriteFeatures = WriteFeatures(1 << 11);
    /// Images (media parts + drawing pic rels).
    pub const IMAGES: WriteFeatures = WriteFeatures(1 << 12);
    /// Pivot tables (cache definition, records, table part + rels, workbook wiring).
    pub const PIVOTS: WriteFeatures = WriteFeatures(1 << 13);

    pub const CORE: WriteFeatures = WriteFeatures(Self::VALUES.0 | Self::FORMULAS.0 | Self::DIMS.0);

    /// CORE + styles + CF/DV (W2 default when styles supplied).
    pub const WITH_STYLES: WriteFeatures =
        WriteFeatures(Self::CORE.0 | Self::STYLES.0 | Self::CF_DV.0);

    /// All write families (W3 `features="all"`).
    pub const ALL: WriteFeatures = WriteFeatures(
        Self::CORE.0
            | Self::STYLES.0
            | Self::CF_DV.0
            | Self::MERGES.0
            | Self::HYPERLINKS.0
            | Self::COMMENTS.0
            | Self::TABLES.0
            | Self::DEFINED_NAMES.0
            | Self::CHARTS.0
            | Self::PROPS.0
            | Self::IMAGES.0
            | Self::PIVOTS.0,
    );

    #[inline]
    pub const fn contains(self, other: WriteFeatures) -> bool {
        (self.0 & other.0) == other.0
    }

    #[inline]
    pub const fn union(self, other: WriteFeatures) -> WriteFeatures {
        WriteFeatures(self.0 | other.0)
    }
}

#[derive(Debug, Clone)]
pub struct WriteOptions {
    pub string_mode: StringMode,
    /// When true, emit `<v>` for formula cells that carry a cache.
    pub emit_cached_values: bool,
    pub date1904: bool,
    pub date_iso: bool,
    pub features: WriteFeatures,
    /// Auto SST unique/total threshold (only used when string_mode == Auto).
    pub auto_sst_threshold: f64,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self {
            string_mode: StringMode::InlineStr,
            emit_cached_values: true,
            date1904: false,
            date_iso: false,
            features: WriteFeatures::CORE,
            auto_sst_threshold: AUTO_SST_THRESHOLD,
        }
    }
}

/// Named style registration before write (→ cellStyleXfs + cellStyles).
#[derive(Debug, Clone)]
pub struct NamedStyleInput {
    pub name: String,
    pub desc: StyleDesc,
    pub builtin_id: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct Workbook {
    pub sheets: Vec<Sheet>,
    pub options: WriteOptions,
    pub active_tab: u32,
    pub creator: String,
    /// When set, build sheetData from columnar numeric data (fast path).
    pub numeric_columns: Option<NumericGrid>,
    /// User named styles (Normal is always bootstrapped by StyleEngine).
    pub named_styles: Vec<NamedStyleInput>,
    /// Set when styles/CF/DV/dates need StyleEngine resolve (avoids O(n) scan).
    pub style_work: bool,
    /// Document properties (F011–F013).
    pub props: DocProps,
    /// Workbook structure lock (F017).
    pub lock_structure: bool,
    /// Explicit defined names (F022); auto Print_*/_FilterDatabase added on write.
    pub defined_names: Vec<DefinedName>,
    /// External workbook links (F021/F100 thin stub).
    pub external_links: Vec<ExternalLink>,
    /// Chartsheets (F099).
    pub chartsheets: Vec<ChartsheetSpec>,
    /// When true, workbook content-type is macro-enabled (`.xlsm`).
    pub macro_enabled: bool,
    /// Optional path to a source `.xlsm` for VBA preserve (F101). Deferred if unset.
    pub vba_archive_path: Option<String>,
}

/// Dense numeric grid: `values[row * ncols + col]`, 0-based.
#[derive(Debug, Clone)]
pub struct NumericGrid {
    pub sheet_name: String,
    pub nrows: u32,
    pub ncols: u32,
    pub values: Arc<Vec<f64>>,
}

impl Workbook {
    pub fn new() -> Self {
        Self {
            sheets: vec![Sheet::new("Sheet")],
            options: WriteOptions::default(),
            active_tab: 0,
            creator: "kyrax".into(),
            numeric_columns: None,
            named_styles: Vec::new(),
            style_work: false,
            props: DocProps::default(),
            lock_structure: false,
            defined_names: Vec::new(),
            external_links: Vec::new(),
            chartsheets: Vec::new(),
            macro_enabled: false,
            vba_archive_path: None,
        }
    }

    pub fn with_sheet(name: impl Into<String>) -> Self {
        Self {
            sheets: vec![Sheet::new(name)],
            options: WriteOptions::default(),
            active_tab: 0,
            creator: "kyrax".into(),
            numeric_columns: None,
            named_styles: Vec::new(),
            style_work: false,
            props: DocProps::default(),
            lock_structure: false,
            defined_names: Vec::new(),
            external_links: Vec::new(),
            chartsheets: Vec::new(),
            macro_enabled: false,
            vba_archive_path: None,
        }
    }

    /// Auto-detect structural feature flags from content (pay-for-what-you-use).
    pub fn auto_enable_structural_features(&mut self) {
        let mut f = self.options.features;
        if self.props.title.is_some()
            || self.props.creator.is_some()
            || !self.props.custom.is_empty()
            || self.lock_structure
        {
            f = f.union(WriteFeatures::PROPS);
        }
        if !self.defined_names.is_empty()
            || self.sheets.iter().any(|s| {
                s.print_area.is_some() || s.print_titles.is_some() || s.auto_filter.is_some()
            })
        {
            f = f.union(WriteFeatures::DEFINED_NAMES);
        }
        if !self.external_links.is_empty() {
            f = f.union(WriteFeatures::PROPS); // ext refs ship with package extras
        }
        if !self.chartsheets.is_empty() {
            f = f.union(WriteFeatures::CHARTS);
        }
        for sh in &self.sheets {
            if !sh.merges.is_empty()
                || sh.protection.is_some()
                || !sh.scenarios.is_empty()
                || sh.auto_filter.is_some()
                || sh.print_options.is_some()
                || sh.page_margins.is_some()
                || sh.page_setup.is_some()
                || sh.header_footer.is_some()
                || !sh.row_breaks.is_empty()
                || !sh.col_breaks.is_empty()
                || sh.tab_color_rgb.is_some()
            {
                f = f.union(WriteFeatures::MERGES);
            }
            if !sh.hyperlinks.is_empty() {
                f = f.union(WriteFeatures::HYPERLINKS);
            }
            if !sh.comments.is_empty() {
                f = f.union(WriteFeatures::COMMENTS);
            }
            if !sh.tables.is_empty() {
                f = f.union(WriteFeatures::TABLES);
            }
            if !sh.charts.is_empty() {
                f = f.union(WriteFeatures::CHARTS);
            }
            if !sh.images.is_empty() {
                f = f.union(WriteFeatures::IMAGES);
            }
            if !sh.pivots.is_empty() {
                f = f.union(WriteFeatures::PIVOTS);
            }
        }
        self.options.features = f;
    }

    /// True when any styles / CF / DV / rich text / date serials need StyleEngine.
    /// Uses `style_work` flag when set (Python path); otherwise structural checks only
    /// (no O(cells) scan — callers that inject DateSerial/styles must set `style_work`).
    pub fn needs_style_engine(&self) -> bool {
        if self.style_work || !self.named_styles.is_empty() {
            return true;
        }
        if self.options.features.contains(WriteFeatures::STYLES)
            || self.options.features.contains(WriteFeatures::CF_DV)
        {
            return true;
        }
        for sheet in &self.sheets {
            if sheet.needs_style_work
                || !sheet.conditional_formatting.is_empty()
                || !sheet.data_validations.is_empty()
                || !sheet.row_style_descs.is_empty()
                || !sheet.col_style_descs.is_empty()
            {
                return true;
            }
        }
        false
    }

    /// Resolve Auto → InlineStr or SharedStrings based on string stats.
    pub fn resolve_string_mode(&self) -> StringMode {
        match self.options.string_mode {
            StringMode::Auto => {
                let (unique, total) = string_stats(self);
                if total > 0 && (unique as f64) / (total as f64) < self.options.auto_sst_threshold {
                    StringMode::SharedStrings
                } else {
                    StringMode::InlineStr
                }
            }
            other => other,
        }
    }

    /// Author a pivot table on `sheet` sourcing `source_range`, with the given
    /// row/column axis fields and data fields aggregated onto `target_cell`.
    ///
    /// This is the Rust entry point for the write half of the pivot engine; it
    /// validates the layout against the sheet's header row before anything is
    /// stored, so a typo fails fast instead of producing a silently skipped
    /// part at save time.
    pub fn add_pivot_table(
        &mut self,
        sheet: usize,
        source_range: &str,
        rows: &[PivotField],
        cols: &[PivotField],
        data: &[(PivotField, PivotAgg)],
        target_cell: &str,
    ) -> Result<(), String> {
        let n = self.sheets.len();
        let sh = self.sheets.get_mut(sheet).ok_or_else(|| {
            format!("add_pivot_table: sheet index {sheet} out of range ({n} sheets)")
        })?;
        let spec = PivotTableSpec {
            name: String::new(),
            source_range: source_range.to_string(),
            rows: rows.to_vec(),
            cols: cols.to_vec(),
            data: data
                .iter()
                .map(|(f, a)| PivotDataField {
                    field: f.clone(),
                    agg: *a,
                })
                .collect(),
            target_cell: target_cell.to_string(),
        };
        spec.validate(sh)?;
        sh.pivots.push(spec);
        Ok(())
    }
}

impl Default for Workbook {
    fn default() -> Self {
        Self::new()
    }
}

fn string_stats(wb: &Workbook) -> (usize, usize) {
    let mut map = AHashMap::new();
    let mut total = 0usize;
    for sheet in &wb.sheets {
        for row in &sheet.rows {
            for cell in &row.cells {
                if let CellValue::Str(s) = &cell.value {
                    total += 1;
                    map.entry(s.as_str()).or_insert(());
                }
            }
        }
    }
    (map.len(), total)
}

/// Shared string table builder.
pub struct SstBuilder {
    map: AHashMap<String, u32>,
    strings: Vec<String>,
    total_refs: u32,
}

impl SstBuilder {
    pub fn new() -> Self {
        Self {
            map: AHashMap::new(),
            strings: Vec::new(),
            total_refs: 0,
        }
    }

    pub fn intern(&mut self, s: &str) -> u32 {
        self.total_refs += 1;
        if let Some(&idx) = self.map.get(s) {
            return idx;
        }
        let idx = self.strings.len() as u32;
        self.map.insert(s.to_string(), idx);
        self.strings.push(s.to_string());
        idx
    }

    /// Read-only index after a serial pre-build (parallel sheet emission).
    pub fn lookup(&self, s: &str) -> u32 {
        *self
            .map
            .get(s)
            .expect("SST lookup: string missing from pre-built table")
    }

    pub fn len(&self) -> usize {
        self.strings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }

    pub fn strings(&self) -> &[String] {
        &self.strings
    }

    pub fn total_refs(&self) -> u32 {
        self.total_refs
    }
}

impl Default for SstBuilder {
    fn default() -> Self {
        Self::new()
    }
}
