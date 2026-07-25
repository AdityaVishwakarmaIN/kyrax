//! Turbo fast-path XLSX reader: memchr scan, rayon chunks, libdeflater, Arrow columns.
//!
//! Port of the verified `_reference/struct_proto` prototype into a library module.
//! Enable with the `__arrow` feature (or any feature that enables `__arrow`).

mod decode;
mod error;
mod formula;
mod meta;
mod scan;
mod structural;
mod styles;
mod zipmin;

pub mod overlay;

/// Turbo WRITE path (silo A core). Additive; does not alter the read path.
pub mod write;

#[cfg(feature = "python")]
pub mod python;

pub use error::{TurboError, TurboResult};
pub use formula::translate_body;
pub use meta::{
    ActivePane, AppProps, AutoFilterMeta, CfRuleRec, CfVo, ColDim, ColorScaleParams, CoreProps,
    DataBarParams, DataValidationRec, FilterColumnMeta, HeaderFooterMeta, IconSetParams,
    PageMarginsMeta, PageSetupMeta, Pane, PaneState, PrintOptionsMeta, RowDim, SheetFormat,
    SheetKind, SheetProtectionMeta, SheetState, SheetViewMeta, WorkbookProps,
};
pub use overlay::{SheetOverlay, WorkbookOverlay};
pub use scan::{CellError, FormulaColumn, FormulaKind, FormulaRecord};
pub use structural::{
    AnchorCell, CellRange, ChartAnchor, ChartMeta, ChartType, Comment, DefinedName, Hyperlink,
    LinkTarget, NameKind, Person, PivotCacheMeta, PivotDataField, PivotTableMeta, Scope,
    SeriesMeta, SheetComments, Table, TableColumn, TableStyle, ThreadedComment, VbaProject, a1,
    range_a1,
};
pub use styles::{
    Alignment, Border, CKind, Color, Dxf, DxfFont, Fill, Font, NamedStyleRec, Protection, Resolved,
    Side, StyleTable, Xf,
};
pub use zipmin::{ArchiveMap, ZipEntryMeta};

use arrow_array::{ArrayRef, UInt32Array};
use scan::{ScanFeat, parse_parallel, parse_shared_strings, sheet_data_region};
use structural::{RelKind, parse_rels, parse_workbook, resolve_zip_path};
use styles::parse_style_table;
use zipmin::{inflate, read_entry};

// ----------------------------------------------------------------------------
// Feature flags (plain bitflags, no extra deps)
// ----------------------------------------------------------------------------

/// Bitflags controlling which turbo features are extracted.
/// `VALUES` is always implied by [`read_workbook_turbo`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Features(pub u32);

impl Features {
    /// Typed value columns (always on).
    pub const VALUES: Features = Features(1 << 0);
    /// Per-cell style index (`@s`) columns + `styles.xml` StyleTable
    /// (includes full borders, alignment, protection, named styles).
    pub const STYLES: Features = Features(1 << 1);
    /// Sparse formula records + shared-formula translation.
    pub const FORMULAS: Features = Features(1 << 2);
    /// Merged cell ranges from the sheet tail.
    pub const MERGES: Features = Features(1 << 3);
    /// Workbook defined names.
    pub const DEFINED_NAMES: Features = Features(1 << 4);
    /// Excel tables (`tableParts` + table XML).
    pub const TABLES: Features = Features(1 << 5);
    /// Hyperlinks (tail + rels).
    pub const HYPERLINKS: Features = Features(1 << 6);
    /// Comments part (authors + records) **and** threaded comments + persons.
    pub const COMMENTS: Features = Features(1 << 7);
    /// Sheet structure meta: row/col dims, autofilter, panes, protection, sheetPr/state.
    pub const SHEET_META: Features = Features(1 << 8);
    /// Page setup / margins / header-footer / print options.
    pub const PAGE_SETUP: Features = Features(1 << 9);
    /// Workbook props (core.xml, app.xml, workbookPr, calcPr). `date1904` always parsed.
    pub const WORKBOOK_META: Features = Features(1 << 10);
    /// Data validations (sheet tail).
    pub const VALIDATIONS: Features = Features(1 << 11);
    /// Conditional formatting rules + dxfs.
    pub const COND_FORMAT: Features = Features(1 << 12);
    /// Chart structured metadata + chartsheet chart discovery.
    /// (Bit 13 — SPEC suggested `1<<8` but that bit is occupied by SHEET_META.)
    pub const CHARTS: Features = Features(1 << 13);
    /// Pivot table + cache definition metadata.
    pub const PIVOTS: Features = Features(1 << 14);
    /// VBA presence + raw `vbaProject.bin` bytes.
    pub const VBA: Features = Features(1 << 15);

    pub const ALL: Features = Features(
        Self::VALUES.0
            | Self::STYLES.0
            | Self::FORMULAS.0
            | Self::MERGES.0
            | Self::DEFINED_NAMES.0
            | Self::TABLES.0
            | Self::HYPERLINKS.0
            | Self::COMMENTS.0
            | Self::SHEET_META.0
            | Self::PAGE_SETUP.0
            | Self::WORKBOOK_META.0
            | Self::VALIDATIONS.0
            | Self::COND_FORMAT.0
            | Self::CHARTS.0
            | Self::PIVOTS.0
            | Self::VBA.0,
    );

    #[inline]
    pub const fn contains(self, other: Features) -> bool {
        (self.0 & other.0) == other.0
    }

    #[inline]
    pub const fn union(self, other: Features) -> Features {
        Features(self.0 | other.0)
    }

    #[inline]
    pub const fn intersection(self, other: Features) -> Features {
        Features(self.0 & other.0)
    }
}

impl std::ops::BitOr for Features {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl std::ops::BitOrAssign for Features {
    fn bitor_assign(&mut self, rhs: Self) {
        *self = self.union(rhs);
    }
}

// ----------------------------------------------------------------------------
// Public result types
// ----------------------------------------------------------------------------

/// One worksheet's turbo payload.
pub struct TurboSheet {
    pub name: String,
    /// Header row (sheet row 1) as column names.
    pub column_names: Vec<String>,
    /// Typed value columns as Arrow arrays (Float64 with nulls, or Dictionary&lt;Int32,Utf8&gt;).
    pub columns: Vec<ArrayRef>,
    /// Data row count (header excluded).
    pub nrows: usize,
    /// Column count.
    pub ncols: usize,
    /// Per-value-column style xf indices (`@s`), gated by [`Features::STYLES`].
    pub style_indices: Option<Vec<UInt32Array>>,
    /// Sparse formulas, gated by [`Features::FORMULAS`].
    pub formulas: Option<FormulaColumn>,
    /// Sparse typed error caches (`t="e"`); always collected on the value path.
    pub cell_errors: Vec<scan::CellError>,
    /// Merged ranges, gated by [`Features::MERGES`].
    pub merges: Option<Vec<CellRange>>,
    /// Tables on this sheet, gated by [`Features::TABLES`].
    pub tables: Option<Vec<Table>>,
    /// Hyperlinks, gated by [`Features::HYPERLINKS`].
    pub hyperlinks: Option<Vec<Hyperlink>>,
    /// Legacy comments, gated by [`Features::COMMENTS`].
    pub comments: Option<SheetComments>,
    /// Threaded comments (Office 2018+), gated by [`Features::COMMENTS`].
    pub threaded_comments: Option<Vec<ThreadedComment>>,
    /// Charts on this sheet, gated by [`Features::CHARTS`].
    pub charts: Option<Vec<ChartMeta>>,
    /// Pivot tables on this sheet, gated by [`Features::PIVOTS`].
    pub pivots: Option<Vec<PivotTableMeta>>,

    // --- Stream A ---
    pub sheet_state: SheetState,
    pub sheet_kind: SheetKind,
    pub row_dimensions: Option<Vec<RowDim>>,
    pub column_dimensions: Option<Vec<ColDim>>,
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

    // --- Stream B (sheet-level) ---
    pub data_validations: Option<Vec<DataValidationRec>>,
    pub cf_rules: Option<Vec<CfRuleRec>>,
}

/// Workbook-level turbo result.
pub struct TurboWorkbook {
    pub sheets: Vec<TurboSheet>,
    /// Workbook-level defined names (when [`Features::DEFINED_NAMES`]).
    pub defined_names: Option<Vec<DefinedName>>,
    /// Parsed `styles.xml` (when [`Features::STYLES`] or [`Features::COND_FORMAT`]).
    pub style_table: Option<StyleTable>,
    /// Workbook props (when [`Features::WORKBOOK_META`]); `date1904` always filled.
    pub workbook_props: Option<WorkbookProps>,
    /// Always parsed from workbookPr (date serial epoch flag; serials not rewritten).
    pub date1904: bool,
    /// Persons part (threaded comments authors), gated by [`Features::COMMENTS`].
    pub persons: Option<Vec<Person>>,
    /// VBA project blob, gated by [`Features::VBA`].
    pub vba: Option<VbaProject>,
}

// ----------------------------------------------------------------------------
// Entry point
// ----------------------------------------------------------------------------

/// List worksheet names from `xl/workbook.xml` without parsing sheet data.
pub fn list_sheet_names(path: &str) -> TurboResult<Vec<String>> {
    let zip = std::fs::read(path)?;
    let wb_xml = read_entry(&zip, "xl/workbook.xml")?
        .ok_or_else(|| TurboError::MissingPart("xl/workbook.xml".into()))?;
    let (sheet_metas, _) = parse_workbook(&wb_xml);
    Ok(sheet_metas.into_iter().map(|m| m.name).collect())
}

/// Read an XLSX workbook with the turbo fast path.
///
/// Feature flags select optional work; [`Features::VALUES`] is always performed.
/// Inflates and scans **every** worksheet. Prefer
/// [`read_workbook_turbo_sheet`] when only one sheet is needed.
pub fn read_workbook_turbo(path: &str, features: Features) -> TurboResult<TurboWorkbook> {
    read_workbook_turbo_filtered(path, features, None)
}

/// Like [`read_workbook_turbo`], but inflate+scan only the sheet at `sheet_idx`
/// (0-based workbook order). Shared parts (workbook.xml sheet list, sharedStrings,
/// styles, defined names, workbook-level sidecars) still load when features request them.
///
/// Returned [`TurboWorkbook::sheets`] contains a single entry for the selected sheet.
pub fn read_workbook_turbo_sheet(
    path: &str,
    features: Features,
    sheet_idx: usize,
) -> TurboResult<TurboWorkbook> {
    read_workbook_turbo_filtered(path, features, Some(sheet_idx))
}

/// Internal entry: `only_sheet = None` parses all sheets; `Some(i)` parses only sheet `i`.
fn read_workbook_turbo_filtered(
    path: &str,
    features: Features,
    only_sheet: Option<usize>,
) -> TurboResult<TurboWorkbook> {
    let features = features.union(Features::VALUES);
    let zip = std::fs::read(path)?;

    // Shared strings (optional part)
    let shared = match read_entry(&zip, "xl/sharedStrings.xml")? {
        Some(sx) => Some(parse_shared_strings(&sx)),
        None => None,
    };

    // Styles: STYLES needs full table; COND_FORMAT needs dxfs (same parse is fine)
    let need_styles =
        features.contains(Features::STYLES) || features.contains(Features::COND_FORMAT);
    let style_table = if need_styles {
        match read_entry(&zip, "xl/styles.xml")? {
            Some(sx) => Some(parse_style_table(&sx)),
            None => Some(parse_style_table(b"")),
        }
    } else {
        None
    };

    // Workbook.xml: sheet order + optional defined names + always date1904
    let wb_xml = read_entry(&zip, "xl/workbook.xml")?
        .ok_or_else(|| TurboError::MissingPart("xl/workbook.xml".into()))?;
    let (mut sheet_metas, defined_names_all) = parse_workbook(&wb_xml);
    let (date1904, wb_code_name, full_calc, calc_id) = meta::parse_workbook_pr(&wb_xml);
    let defined_names = if features.contains(Features::DEFINED_NAMES) {
        Some(defined_names_all)
    } else {
        None
    };

    // Workbook rels → sheet path by r:id + chartsheet detection
    let wb_rels = match read_entry(&zip, "xl/_rels/workbook.xml.rels")? {
        Some(rx) => parse_rels(&rx),
        None => Default::default(),
    };
    for m in &mut sheet_metas {
        if let Some(rid) = &m.rid {
            if let Some(rel) = wb_rels.get(rid) {
                if rel.kind == RelKind::Chartsheet {
                    m.kind = SheetKind::Chartsheet;
                }
            }
        }
    }

    // Workbook props (A7)
    let workbook_props = if features.contains(Features::WORKBOOK_META) {
        let core = match read_entry(&zip, "docProps/core.xml")? {
            Some(cx) => meta::parse_core_props(&cx),
            None => meta::CoreProps::default(),
        };
        let app = match read_entry(&zip, "docProps/app.xml")? {
            Some(ax) => meta::parse_app_props(&ax),
            None => meta::AppProps::default(),
        };
        Some(WorkbookProps {
            date1904,
            code_name: wb_code_name,
            full_calc_on_load: full_calc,
            calc_id,
            core,
            app,
        })
    } else {
        None
    };

    let scan_feat = ScanFeat {
        styles: features.contains(Features::STYLES),
        formulas: features.contains(Features::FORMULAS),
        row_meta: features.contains(Features::SHEET_META),
    };
    let nthreads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);

    let need_rels = features.contains(Features::HYPERLINKS)
        || features.contains(Features::TABLES)
        || features.contains(Features::COMMENTS)
        || features.contains(Features::CHARTS)
        || features.contains(Features::PIVOTS);

    // Stream C workbook-level: persons, VBA, pivot caches
    let persons = if features.contains(Features::COMMENTS) {
        let persons_rel = wb_rels.values().find(|r| r.kind == RelKind::Person);
        match persons_rel {
            Some(rel) => {
                let path = resolve_zip_path("xl/", &rel.target);
                match read_entry(&zip, &path)? {
                    Some(px) => Some(structural::parse_persons(&px)),
                    None => Some(Vec::new()),
                }
            }
            None => Some(Vec::new()),
        }
    } else {
        None
    };

    let vba = if features.contains(Features::VBA) {
        let mut v = VbaProject::default();
        if let Some(rel) = wb_rels.values().find(|r| r.kind == RelKind::VbaProject) {
            let path = resolve_zip_path("xl/", &rel.target);
            v.present = true;
            v.part = Some(path.clone());
            v.bytes = read_entry(&zip, &path)?;
        } else if let Some(bytes) = read_entry(&zip, "xl/vbaProject.bin")? {
            // content-types only / conventional path fallback
            v.present = true;
            v.part = Some("xl/vbaProject.bin".into());
            v.bytes = Some(bytes);
        }
        Some(v)
    } else {
        None
    };

    let pivot_cache_paths = if features.contains(Features::PIVOTS) {
        structural::parse_workbook_pivot_caches(&wb_xml, &wb_rels)
    } else {
        Default::default()
    };
    // Pre-parse cache definitions once
    let mut pivot_caches: std::collections::HashMap<u32, structural::PivotCacheMeta> =
        std::collections::HashMap::new();
    if features.contains(Features::PIVOTS) {
        for (cid, path) in &pivot_cache_paths {
            if let Some(cx) = read_entry(&zip, path)? {
                pivot_caches.insert(*cid, structural::parse_pivot_cache(&cx, path.clone()));
            }
        }
    }

    if let Some(i) = only_sheet {
        if i >= sheet_metas.len() {
            return Err(TurboError::Format(format!(
                "sheet index {i} out of range ({} sheets)",
                sheet_metas.len()
            )));
        }
    }

    let mut sheets = Vec::with_capacity(only_sheet.map(|_| 1).unwrap_or(sheet_metas.len()));
    for (sheet_idx, meta) in sheet_metas.iter().enumerate() {
        // Selective-sheet fast path: skip inflate/scan of non-requested sheets.
        if let Some(only) = only_sheet {
            if sheet_idx != only {
                continue;
            }
        }

        let sheet_path = resolve_sheet_path(meta, sheet_idx, &wb_rels);
        let is_chartsheet = meta.kind == SheetKind::Chartsheet;
        let sheet_base_dir = if is_chartsheet {
            "xl/chartsheets/"
        } else {
            "xl/worksheets/"
        };

        // Chartsheet: empty grid; still load chart sidecars when requested
        if is_chartsheet {
            let mut sheet = empty_sheet(meta.name.clone(), meta.state, meta.kind, features);
            if features.contains(Features::CHARTS) {
                let sheet_file = sheet_path.rsplit('/').next().unwrap_or("sheet1.xml");
                let rels_path = format!("xl/chartsheets/_rels/{sheet_file}.rels");
                let rels = match read_entry(&zip, &rels_path)? {
                    Some(rx) => parse_rels(&rx),
                    None => Default::default(),
                };
                sheet.charts = Some(load_sheet_charts(
                    &zip,
                    sheet_base_dir,
                    &rels,
                    sheet_idx as u32,
                )?);
            }
            sheets.push(sheet);
            continue;
        }

        let sheet_xml = match zipmin::find_entry(&zip, &sheet_path) {
            Some((m, c, u)) => inflate(m, c, u)?,
            None => {
                return Err(TurboError::MissingPart(sheet_path));
            }
        };

        // Missing sheetData (rare): treat as empty grid
        let partial = match sheet_data_region(&sheet_xml) {
            Ok(_) => parse_parallel(&sheet_xml, nthreads, shared.as_ref(), scan_feat)?,
            Err(_) => {
                sheets.push(empty_sheet(
                    meta.name.clone(),
                    meta.state,
                    meta.kind,
                    features,
                ));
                continue;
            }
        };
        let row_dims_from_scan = partial.row_dims.clone();
        let (column_names, columns, style_indices, mut formulas, cell_errors, nrows, ncols) =
            partial.into_arrow_columns()?;
        // Flag on → always Some (empty batch when the sheet has no formulas).
        // Flag off → None (scan skips formula capture so the Option is empty).
        if features.contains(Features::FORMULAS) {
            if formulas.is_none() {
                formulas = Some(FormulaColumn::empty());
            }
        } else {
            formulas = None;
        }

        // Structural sidecars (selective)
        let (sheet_start, sheet_end) = sheet_data_region(&sheet_xml)?;
        let header_xml = &sheet_xml[..sheet_start];
        // include bytes before sheetData open tag properly
        let pre = if let Some(sd) = memchr::memmem::find(&sheet_xml, b"<sheetData") {
            &sheet_xml[..sd]
        } else {
            header_xml
        };
        let tail = &sheet_xml[sheet_end..];

        // Sheet-level rels (only if needed)
        let sheet_file = sheet_path.rsplit('/').next().unwrap_or("sheet1.xml");
        let rels = if need_rels {
            let rels_path = format!("xl/worksheets/_rels/{sheet_file}.rels");
            match read_entry(&zip, &rels_path)? {
                Some(rx) => parse_rels(&rx),
                None => Default::default(),
            }
        } else {
            Default::default()
        };

        let merges = if features.contains(Features::MERGES) {
            Some(structural::scan_merges(tail))
        } else {
            None
        };

        let hyperlinks = if features.contains(Features::HYPERLINKS) {
            Some(structural::scan_hyperlinks(tail, &rels))
        } else {
            None
        };

        let tables = if features.contains(Features::TABLES) {
            let rids = structural::scan_table_part_rids(tail);
            let base_dir = "xl/worksheets/";
            let mut tables = Vec::new();
            for rid in &rids {
                if let Some(rel) = rels.get(rid) {
                    let path = resolve_zip_path(base_dir, &rel.target);
                    if let Some(tx) = read_entry(&zip, &path)? {
                        if let Some(tab) = structural::parse_table(&tx, sheet_idx as u32) {
                            tables.push(tab);
                        }
                    }
                }
            }
            Some(tables)
        } else {
            None
        };

        let has_threaded_rel = rels.values().any(|r| r.kind == RelKind::ThreadedComment);

        let comments = if features.contains(Features::COMMENTS) {
            let base_dir = "xl/worksheets/";
            let mut sc = rels
                .values()
                .find(|r| r.kind == RelKind::Comments)
                .and_then(|r| {
                    let path = resolve_zip_path(base_dir, &r.target);
                    read_entry(&zip, &path)
                        .ok()
                        .flatten()
                        .map(|cx| structural::parse_comments(&cx))
                })
                .unwrap_or(SheetComments {
                    authors: Vec::new(),
                    comments: Vec::new(),
                    legacy_is_mirror: false,
                });
            sc.legacy_is_mirror = has_threaded_rel;
            Some(sc)
        } else {
            None
        };

        let threaded_comments = if features.contains(Features::COMMENTS) {
            let base_dir = "xl/worksheets/";
            let mut all = Vec::new();
            for r in rels.values().filter(|r| r.kind == RelKind::ThreadedComment) {
                let path = resolve_zip_path(base_dir, &r.target);
                if let Some(tx) = read_entry(&zip, &path)? {
                    all.extend(structural::parse_threaded_comments(&tx));
                }
            }
            if let Some(ref ps) = persons {
                structural::resolve_threaded_person_names(&mut all, ps);
            }
            Some(all)
        } else {
            None
        };

        let charts = if features.contains(Features::CHARTS) {
            Some(load_sheet_charts(
                &zip,
                "xl/worksheets/",
                &rels,
                sheet_idx as u32,
            )?)
        } else {
            None
        };

        let pivots = if features.contains(Features::PIVOTS) {
            let base_dir = "xl/worksheets/";
            let mut pivs = Vec::new();
            for r in rels.values().filter(|r| r.kind == RelKind::PivotTable) {
                let path = resolve_zip_path(base_dir, &r.target);
                if let Some(px) = read_entry(&zip, &path)? {
                    // Prefer cache from table's cacheId; fall back to table rel → cache def
                    let mut cache_meta = None;
                    // Peek cacheId quickly for lookup
                    if let Some(cid) = peek_pivot_cache_id(&px) {
                        cache_meta = pivot_caches.get(&cid).cloned();
                    }
                    if cache_meta.is_none() {
                        // table-level rels
                        let table_file = path.rsplit('/').next().unwrap_or("pivotTable1.xml");
                        let trels_path = format!("xl/pivotTables/_rels/{table_file}.rels");
                        if let Some(trx) = read_entry(&zip, &trels_path)? {
                            let trels = parse_rels(&trx);
                            if let Some(crel) =
                                trels.values().find(|x| x.kind == RelKind::PivotCacheDef)
                            {
                                let cpath = resolve_zip_path("xl/pivotTables/", &crel.target);
                                if let Some(cx) = read_entry(&zip, &cpath)? {
                                    cache_meta = Some(structural::parse_pivot_cache(&cx, cpath));
                                }
                            }
                        }
                    }
                    let cache = cache_meta.unwrap_or(structural::PivotCacheMeta {
                        part: String::new(),
                        source_type: String::new(),
                        worksheet_sheet: None,
                        worksheet_ref: None,
                        worksheet_name: None,
                        field_names: Vec::new(),
                    });
                    if let Some(pt) = structural::parse_pivot_table(&px, sheet_idx as u32, cache) {
                        pivs.push(pt);
                    }
                }
            }
            Some(pivs)
        } else {
            None
        };

        // Stream A header + tail
        let (mut col_dims, sheet_format, sheet_view, code_name, tab_color, fit_to_page) =
            if features.contains(Features::SHEET_META) || features.contains(Features::PAGE_SETUP) {
                meta::scan_sheet_header(pre)
            } else {
                (Vec::new(), None, None, None, None, None)
            };

        let (
            row_dimensions,
            column_dimensions,
            sheet_format,
            auto_filter,
            sheet_view,
            protection,
            code_name,
            tab_color,
        ) = if features.contains(Features::SHEET_META) {
            let auto_filter = meta::scan_auto_filter(tail);
            let protection = Some(meta::scan_protection(tail).unwrap_or_default());
            let sheet_view = sheet_view.or_else(|| {
                Some(SheetViewMeta {
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
                })
            });
            // openpyxl fills missing col width with 13 when only hidden etc. — we keep XML truth
            let _ = &mut col_dims;
            (
                Some(row_dims_from_scan),
                Some(col_dims),
                sheet_format.or_else(|| {
                    Some(SheetFormat {
                        base_col_width: Some(8),
                        default_col_width: None,
                        default_row_height: Some(15.0),
                        custom_height: None,
                        zero_height: None,
                        outline_level_row: None,
                        outline_level_col: None,
                    })
                }),
                auto_filter,
                sheet_view,
                protection,
                code_name,
                tab_color,
            )
        } else {
            (None, None, None, None, None, None, None, None)
        };

        let (page_setup, page_margins, print_options, header_footer) =
            if features.contains(Features::PAGE_SETUP) {
                let mut ps = meta::scan_page_setup(tail).unwrap_or_default();
                if ps.fit_to_page.is_none() {
                    ps.fit_to_page = fit_to_page;
                }
                // re-scan header for fit_to_page if SHEET_META was off
                let fit = if fit_to_page.is_none() {
                    meta::scan_sheet_header(pre).5
                } else {
                    fit_to_page
                };
                if ps.fit_to_page.is_none() {
                    ps.fit_to_page = fit;
                }
                (
                    Some(ps),
                    Some(meta::scan_page_margins(tail).unwrap_or_default()),
                    Some(meta::scan_print_options(tail).unwrap_or_default()),
                    Some(meta::scan_header_footer(tail).unwrap_or_default()),
                )
            } else {
                (None, None, None, None)
            };

        let data_validations = if features.contains(Features::VALIDATIONS) {
            Some(meta::scan_data_validations(tail))
        } else {
            None
        };

        let cf_rules = if features.contains(Features::COND_FORMAT) {
            Some(meta::scan_conditional_formatting(tail))
        } else {
            None
        };

        sheets.push(TurboSheet {
            name: meta.name.clone(),
            column_names,
            columns,
            nrows,
            ncols,
            style_indices,
            formulas,
            cell_errors,
            merges,
            tables,
            hyperlinks,
            comments,
            threaded_comments,
            charts,
            pivots,
            sheet_state: meta.state,
            sheet_kind: meta.kind,
            row_dimensions,
            column_dimensions,
            sheet_format,
            auto_filter,
            sheet_view,
            protection,
            page_setup,
            page_margins,
            print_options,
            header_footer,
            code_name,
            tab_color,
            data_validations,
            cf_rules,
        });
    }

    // Only surface style_table when STYLES requested (COND_FORMAT may have parsed it for dxfs)
    let style_table_out =
        if features.contains(Features::STYLES) || features.contains(Features::COND_FORMAT) {
            style_table
        } else {
            None
        };

    Ok(TurboWorkbook {
        sheets,
        defined_names,
        style_table: style_table_out,
        workbook_props,
        date1904,
        persons,
        vba,
    })
}

fn peek_pivot_cache_id(xml: &[u8]) -> Option<u32> {
    let start = memchr::memmem::find(xml, b"cacheId=\"")?;
    let vs = start + 9;
    let ve = vs + memchr::memchr(b'"', &xml[vs..])?;
    std::str::from_utf8(&xml[vs..ve]).ok()?.parse().ok()
}

fn load_sheet_charts(
    zip: &[u8],
    sheet_base_dir: &str,
    sheet_rels: &structural::RelMap,
    sheet_idx: u32,
) -> TurboResult<Vec<ChartMeta>> {
    let mut charts = Vec::new();
    for rel in sheet_rels.values().filter(|r| r.kind == RelKind::Drawing) {
        let drawing_path = resolve_zip_path(sheet_base_dir, &rel.target);
        let Some(dx) = read_entry(zip, &drawing_path)? else {
            continue;
        };
        let anchors = structural::parse_drawing_chart_anchors(&dx);
        let drawing_file = drawing_path.rsplit('/').next().unwrap_or("drawing1.xml");
        // drawings live under xl/drawings/
        let drels_path = format!("xl/drawings/_rels/{drawing_file}.rels");
        let drels = match read_entry(zip, &drels_path)? {
            Some(rx) => parse_rels(&rx),
            None => Default::default(),
        };
        for (rid, anchor) in anchors {
            let Some(crel) = drels.get(&rid) else {
                continue;
            };
            if crel.kind != RelKind::Chart {
                continue;
            }
            let chart_path = resolve_zip_path("xl/drawings/", &crel.target);
            let Some(cx) = read_entry(zip, &chart_path)? else {
                continue;
            };
            charts.push(structural::parse_chart(&cx, sheet_idx, chart_path, anchor));
        }
    }
    Ok(charts)
}

fn empty_sheet(name: String, state: SheetState, kind: SheetKind, features: Features) -> TurboSheet {
    TurboSheet {
        name,
        column_names: Vec::new(),
        columns: Vec::new(),
        nrows: 0,
        ncols: 0,
        style_indices: None,
        formulas: None,
        cell_errors: Vec::new(),
        merges: if features.contains(Features::MERGES) {
            Some(Vec::new())
        } else {
            None
        },
        tables: if features.contains(Features::TABLES) {
            Some(Vec::new())
        } else {
            None
        },
        hyperlinks: if features.contains(Features::HYPERLINKS) {
            Some(Vec::new())
        } else {
            None
        },
        comments: if features.contains(Features::COMMENTS) {
            Some(SheetComments {
                authors: Vec::new(),
                comments: Vec::new(),
                legacy_is_mirror: false,
            })
        } else {
            None
        },
        threaded_comments: if features.contains(Features::COMMENTS) {
            Some(Vec::new())
        } else {
            None
        },
        charts: if features.contains(Features::CHARTS) {
            Some(Vec::new())
        } else {
            None
        },
        pivots: if features.contains(Features::PIVOTS) {
            Some(Vec::new())
        } else {
            None
        },
        sheet_state: state,
        sheet_kind: kind,
        row_dimensions: if features.contains(Features::SHEET_META) {
            Some(Vec::new())
        } else {
            None
        },
        column_dimensions: if features.contains(Features::SHEET_META) {
            Some(Vec::new())
        } else {
            None
        },
        sheet_format: None,
        auto_filter: None,
        sheet_view: None,
        protection: None,
        page_setup: None,
        page_margins: None,
        print_options: None,
        header_footer: None,
        code_name: None,
        tab_color: None,
        data_validations: if features.contains(Features::VALIDATIONS) {
            Some(Vec::new())
        } else {
            None
        },
        cf_rules: if features.contains(Features::COND_FORMAT) {
            Some(Vec::new())
        } else {
            None
        },
    }
}

fn resolve_sheet_path(
    meta: &structural::SheetMeta,
    sheet_idx: usize,
    wb_rels: &structural::RelMap,
) -> String {
    if let Some(rid) = &meta.rid {
        if let Some(rel) = wb_rels.get(rid) {
            return resolve_zip_path("xl/", &rel.target);
        }
    }
    // Fallback: conventional sheetN.xml (1-based)
    format!("xl/worksheets/sheet{}.xml", sheet_idx + 1)
}
