// calc/mod.rs — formula calculation engine: hydrate formula cells with computed values.
//
// All core logic lives in Rust and stays language-agnostic (no pyo3 here; the
// module is the eval target for the read path, the write path, and the overlay).
// Every formula cell carries both its text and a cached result; when the file
// supplies none, the engine computes one so the writer can emit `<v>`.
//
// Wave 1 = the type contract only: `CalcValue`, `CalcError`, the AST, coercion.
// `hydrate_workbook` is the wave-3 signature placeholder.

mod ast;
mod coerce;
pub mod deps;
mod eval;
pub mod functions;
mod hydrate;
mod lexer;
mod parser;
mod refs;
mod sheetdata;
/// Dynamic-array spill-region ownership. Public because the spill map is the
/// contract between the evaluator (which commits regions), the dependency graph
/// (which reads them to invalidate), and the dynamic-array functions.
pub mod spill;
mod value;

// Test-only: the shared harness and the per-class formula matrices. Each
// `tests_class_*` file is owned by exactly one author; the harness is shared
// so a class test contains cases and nothing else.
#[cfg(test)]
mod testkit;
#[cfg(test)]
mod tests_class_logical_lookup;
#[cfg(test)]
mod tests_class_math_info;
#[cfg(test)]
mod tests_class_text_datetime;

use crate::turbo::error::{TurboError, TurboResult};

pub use ast::{
    BinaryOp, CellRef, ColumnRef, Expr, RangeRef, RefCore, RefExpr, RowRef, SuffixOp, TableColRef,
    TableRef, TableSection, UnaryOp,
};
pub use coerce::{
    blank_as, classify_text, coerce_number, coerce_text, compare, compare_eq, is_real_num,
    number_to_general, wildcard_match,
};
pub use parser::{ParseError, parse_formula};
pub use value::{ArrayValue, CalcError, CalcValue};

/// Options controlling one hydration pass.
#[derive(Debug, Clone, Copy)]
pub struct CalcOptions {
    /// Excel 1904 date system (affects date-serial arithmetic only).
    pub date1904: bool,
    /// Recompute even when a formula cell already carries a cached value.
    pub force_recalc: bool,
    /// Circular-reference iteration cap (0 = none).
    pub max_iterations: u32,
}

impl Default for CalcOptions {
    fn default() -> Self {
        Self {
            date1904: false,
            force_recalc: false,
            max_iterations: 0,
        }
    }
}

/// Result counters for one hydration pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CalcReport {
    /// Formula cells seen on the grid.
    pub total_formulas: usize,
    /// Cells with a fresh result written to `cached`.
    pub computed: usize,
    /// Cells left uncomputed (unsupported function, external ref, internal-only
    /// error). The caller must set `fullCalcOnLoad="1"` so Excel fills them on
    /// open — never emit a fabricated number.
    pub fallback: usize,
    /// Computed results that ARE error values (legal `t="e"` caches).
    pub error_cells: usize,
    /// Circular references detected.
    pub cycles: usize,
}

/// Evaluate `wb`'s formula cells against its value grid and fill each
/// `CellValue::Formula.cached` slot. See `hydrate.rs` for the pass itself.
///
/// When the returned report has a non-zero `fallback` or `cycles`, the caller
/// MUST set `calcPr fullCalcOnLoad="1"` so Excel recomputes the cells kyrax
/// deliberately left alone.
pub fn hydrate_workbook(
    wb: &mut crate::turbo::write::model::Workbook,
    options: &CalcOptions,
) -> CalcReport {
    hydrate::hydrate_workbook(wb, options)
}

/// Build the dependency graph for the workbook at `path`, without evaluating
/// anything.
///
/// This exists because the graph was previously unreachable: `hydrate_workbook`
/// builds one internally and throws it away, and the only [`RefResolver`] impl
/// lives in a private module. [`crate::turbo::features::provenance`] needs a
/// graph to answer "what does this cell read?" and "what breaks if I change
/// it?" — so this is the accessor that makes provenance callable at all.
///
/// Cost: inflates each worksheet part once and parses every formula. That is
/// unavoidable — a dependency graph *is* the parse of every formula — but note
/// it is deliberately **not** the full hydration pass: no evaluation, no
/// write-back, no `SheetData` mutation. Asking what a workbook depends on
/// should not cost what asking for its values costs.
///
/// [`RefResolver`]: deps::RefResolver
pub fn dependency_graph_for_path(path: &str) -> TurboResult<deps::DependencyGraph> {
    use crate::turbo::overlay::hydrate_sheet_from_xml;
    use crate::turbo::write::model::Workbook;
    use crate::turbo::zipmin::{ArchiveMap, inflate_entry};
    use std::sync::Arc;

    let bytes = std::fs::read(path).map_err(TurboError::Io)?;
    let map = ArchiveMap::parse(Arc::new(bytes))?;

    // `CellKey.sheet` is an index into `wb.sheets`, so the order this Vec is
    // built in *is* the API contract — a caller passing sheet_index 1 has to
    // get the same sheet every run. Zip entry order is not guaranteed to be
    // sheet order, so sort by the numeric suffix of the part name
    // (`sheet2.xml` -> 2), which is the convention Excel writes and the only
    // ordering available without re-parsing workbook.xml's `<sheet>` list.
    let mut parts: Vec<&String> = map
        .entry_order
        .iter()
        .filter(|n| n.starts_with("xl/worksheets/sheet") && n.ends_with(".xml"))
        .collect();
    parts.sort_by_key(|n| {
        n.trim_start_matches("xl/worksheets/sheet")
            .trim_end_matches(".xml")
            .parse::<u32>()
            .unwrap_or(u32::MAX)
    });

    let mut wb = Workbook::default();
    for entry_name in parts {
        let Some(meta) = map.entries.get(entry_name) else {
            continue;
        };
        let xml = inflate_entry(&map.source_bytes, meta)?;
        let mut sheet = hydrate_sheet_from_xml(&xml, &map.shared_strings)?;
        sheet.name = map
            .sheet_name_map
            .iter()
            .find(|(_, target)| *target == entry_name)
            .map(|(name, _)| name.clone())
            .unwrap_or_default();
        wb.sheets.push(sheet);
    }

    Ok(hydrate::graph_only(&wb))
}
