//! Turbo WRITE path — silo A core + silo B styles + silo C structural/charts (W1–W3).
//!
//! Additive module: does not modify the shipped read path.
//!
//! Pipeline: columnar/row model → StyleEngine resolve (opt) → direct XML
//! (itoa/ryu) → optional SST → styles.xml/theme/props → structural parts
//! (tables/comments/drawings/charts) → libdeflater → ZIP.

mod cf_dv;
mod charts;
pub(crate) mod media;
pub(crate) mod model;
pub(crate) mod pivot;
mod rich_text;
mod structural;
pub(crate) mod style_engine;
pub(crate) mod writer;
pub(crate) mod xml;
pub(crate) mod zip;

#[cfg(feature = "python")]
pub mod python;

pub use cf_dv::{
    CfRule, CfRuleKind, CfVo, ConditionalFormatting, DataValidation, emit_data_validations,
};
pub use charts::{
    Anchor, Chart, ChartType, ChartsheetSpec, EMU_PER_CM, EMU_PER_INCH, EMU_PER_POINT, Series,
    cm_to_emu, write_chart_space, write_drawing,
};
pub use model::{
    AUTO_SST_THRESHOLD, CachedValue, Cell, CellValue, ColDim, Comment, DefinedName, DocProps,
    ExternalLink, FormulaKind, HeaderFooter, Hyperlink, Image, ImageFormat, NamedStyleInput,
    NumericGrid, PageMargins, PageSetup, PrintOptions, Row, Scenario, Sheet, SheetProtection,
    SheetState, SheetViewOpts, SstBuilder, StringMode, TableDef, Workbook, WriteFeatures,
    WriteOptions, detect_image_format,
};
pub use pivot::{PivotAgg, PivotDataField, PivotField, PivotTableSpec, build_pivot_parts};
pub use rich_text::{RichRun, RichText, RunFont};
pub use structural::{hash_password, write_comments, write_table};
pub use style_engine::{
    AlignDesc, BorderDesc, ColorSpec, DxfDesc, FillDesc, FontDesc, ProtDesc, SideDesc, StyleArray,
    StyleDesc, StyleEngine,
};
pub use writer::{
    date_to_serial, datetime_to_serial, save_workbook, save_workbook_stream,
    write_numeric_grid_sheet, write_workbook_bytes, write_worksheet,
};
pub use xml::{col_letters, dimension_ref, escape_text, write_coord};
pub use zip::ZipWriter;
