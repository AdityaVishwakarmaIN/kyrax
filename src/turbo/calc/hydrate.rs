// calc/hydrate.rs — the hydration orchestrator (plan §4, wave 3 B3).
//
// One pass over a workbook: collect every formula cell, parse it, order the
// cells by dependency, evaluate in that order, and write each result back into
// `CellValue::Formula.cached` so the writer can emit `<v>`.
//
// The correctness rule that shapes this file: **never emit a wrong number.**
// Three things can make a result untrustworthy — a formula we cannot parse, a
// dependency we cannot resolve statically, and a cycle. All three are handled
// the same way: the cell is registered in the graph as unresolved, `deps`
// excludes it *and every cell that transitively reads it* from the evaluation
// order, and those cells keep whatever cache the file already had. The caller
// sees them in `CalcReport::fallback` and must set `calcPr fullCalcOnLoad="1"`
// so Excel fills them on open.
//
// Evaluation reads through `SheetData`, which already holds each formula
// cell's prior cached value, so a cell computed earlier in the order is
// visible to its dependents via `update_computed`.

use crate::turbo::calc::ast::Expr;
use crate::turbo::calc::deps::{CellKey, DependencyGraph};
use crate::turbo::calc::eval::eval;
use crate::turbo::calc::functions::FuncCtx;
use crate::turbo::calc::parser::parse_formula;
use crate::turbo::calc::sheetdata::SheetData;
use crate::turbo::calc::value::CalcValue;
use crate::turbo::calc::{CalcOptions, CalcReport};
use crate::turbo::write::model::{CellValue, Workbook};
use std::collections::HashMap;

/// Evaluate `wb`'s formula cells and fill their `cached` slots.
///
/// Cells that already carry a cache are left alone unless
/// `options.force_recalc` is set; they still participate in ordering, so their
/// dependents read the file's own value rather than a blank.
/// Walk every formula cell into a dependency graph, without evaluating any of
/// them.
///
/// Split out of [`hydrate_workbook`] so the graph can be built on its own:
/// `crate::turbo::calc::dependency_graph_for_path` hands it to
/// [`crate::turbo::features::provenance`], which answers "what does this cell
/// read?" and "what breaks if I change it?" — questions that need the graph
/// and none of the arithmetic.
///
/// Deliberately shared rather than duplicated. A second, near-identical walk
/// would drift, and a provenance answer that disagreed with what the evaluator
/// actually did would be worse than no provenance at all.
///
/// Returns the graph, the parsed expressions, which cells arrived already
/// cached, and the formula count.
pub(super) fn collect_graph(
    wb: &Workbook,
    data: &SheetData,
) -> (
    DependencyGraph,
    HashMap<CellKey, Expr>,
    HashMap<CellKey, bool>,
    usize,
) {
    let mut graph = DependencyGraph::new();
    let mut exprs: HashMap<CellKey, Expr> = HashMap::new();
    let mut has_cache: HashMap<CellKey, bool> = HashMap::new();
    let mut total = 0usize;

    for (s, sheet) in wb.sheets.iter().enumerate() {
        let sheet_idx = s as u32;
        for row in &sheet.rows {
            let r = row.row.saturating_sub(1);
            for cell in &row.cells {
                let CellValue::Formula { text, cached, .. } = &cell.value else {
                    continue;
                };
                let Ok(c) = u16::try_from(cell.col.saturating_sub(1)) else {
                    continue; // beyond column XFD; cannot hold a value
                };
                let key = CellKey::new(sheet_idx, r, c);
                total += 1;
                has_cache.insert(key, cached.is_some());
                match parse_formula(text) {
                    Ok(expr) => {
                        // An `Unresolved` error still inserts the node; the
                        // cell is simply routed to fallback by `topological`.
                        let _ = graph.add_formula(key, &expr, data);
                        exprs.insert(key, expr);
                    }
                    Err(_) => {
                        let _ = graph.add_unparseable(key);
                    }
                }
            }
        }
    }

    (graph, exprs, has_cache, total)
}

/// Build only the dependency graph for `wb` — no evaluation, no write-back.
pub(super) fn graph_only(wb: &Workbook) -> DependencyGraph {
    let data = SheetData::build(wb);
    collect_graph(wb, &data).0
}

pub fn hydrate_workbook(wb: &mut Workbook, options: &CalcOptions) -> CalcReport {
    let mut report = CalcReport::default();

    // 1. Snapshot the grid. `SheetData` owns the values; `wb` is only touched
    //    again at write-back.
    let mut data = SheetData::build(wb);
    let date1904 = options.date1904 || data.date1904();

    // 2. Collect formula cells and parse them. A parse failure registers the
    //    cell as unparseable rather than dropping it, so its dependents are
    //    excluded too.
    let (mut graph, exprs, has_cache, total_formulas) = collect_graph(wb, &data);
    report.total_formulas = total_formulas;

    // 3. Order. `order` is exactly the set we may compute; `fallback` and
    //    `cycles` are reported, never computed.
    let topo = graph.topological();
    report.cycles = topo.cycles.len();
    report.fallback = topo.fallback.len();

    // 4. Evaluate in dependency order.
    for key in &topo.order {
        let Some(expr) = exprs.get(key) else {
            continue; // unparseable cells never reach `order`, but be explicit
        };
        if !options.force_recalc && has_cache.get(key).copied().unwrap_or(false) {
            continue; // the file's own cache stands
        }
        let value = {
            let ctx = FuncCtx {
                date1904,
                sheet: key.sheet,
                row: key.row,
                col: u32::from(key.col),
                resolver: &data,
            };
            eval(expr, &ctx)
        };
        // An error is a legitimate Excel result as long as the code is one
        // Excel can store; internal-only codes mean "we could not compute
        // this", which is a fallback, not a value.
        let stored = match value {
            Ok(CalcValue::Array(_)) | Ok(CalcValue::Blank) => None,
            Ok(CalcValue::Error(e)) if e.is_internal() => None,
            Ok(v) => Some(v),
            Err(e) if e.cacheable() => Some(CalcValue::Error(e)),
            Err(_) => None,
        };
        match stored {
            Some(v) => {
                if v.is_error() {
                    report.error_cells += 1;
                }
                if data.update_computed(key.sheet, key.row, u32::from(key.col), v) {
                    report.computed += 1;
                } else {
                    report.fallback += 1;
                }
            }
            None => report.fallback += 1,
        }
    }

    // 5. Commit. `write_back` only fills slots whose value is a cacheable
    //    scalar, so nothing fabricated can reach the XML.
    data.write_back(wb);
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turbo::write::model::{CachedValue, Cell, FormulaKind, Row, Sheet};

    fn num_cell(col: u32, n: f64) -> Cell {
        Cell::new(col, CellValue::Number(n))
    }

    fn formula_cell(col: u32, text: &str) -> Cell {
        Cell::new(
            col,
            CellValue::Formula {
                text: text.to_string(),
                kind: FormulaKind::Normal,
                cached: None,
            },
        )
    }

    /// One sheet, one row of cells at row 1.
    fn book(cells: Vec<Cell>) -> Workbook {
        let mut r = Row::new(1);
        r.cells = cells;
        let mut sheet = Sheet::new("Sheet1");
        sheet.rows = vec![r];
        let mut wb = Workbook::new();
        wb.sheets = vec![sheet];
        wb
    }

    fn cached_of(wb: &Workbook, row: u32, col: u32) -> Option<String> {
        let r = wb.sheets[0].rows.iter().find(|r| r.row == row)?;
        let c = r.cells.iter().find(|c| c.col == col)?;
        match &c.value {
            CellValue::Formula { cached, .. } => cached.as_ref().map(|v| format!("{v:?}")),
            _ => None,
        }
    }

    #[test]
    fn computes_a_simple_chain_in_dependency_order() {
        let mut wb = book(vec![
            num_cell(1, 2.0),
            num_cell(2, 3.0),
            formula_cell(3, "=A1+B1"),
            formula_cell(4, "=C1*2"),
        ]);
        let report = hydrate_workbook(&mut wb, &CalcOptions::default());
        assert_eq!(report.total_formulas, 2);
        // C1 must be computed before D1 for D1 to see 5.
        assert!(cached_of(&wb, 1, 3).is_some(), "{report:?}");
        assert!(cached_of(&wb, 1, 4).is_some(), "{report:?}");
    }

    #[test]
    fn an_unparseable_formula_is_fallback_and_never_cached() {
        let mut wb = book(vec![num_cell(1, 1.0), formula_cell(2, "=1+")]);
        let report = hydrate_workbook(&mut wb, &CalcOptions::default());
        assert_eq!(report.total_formulas, 1);
        assert_eq!(report.computed, 0);
        assert!(report.fallback >= 1, "{report:?}");
        assert_eq!(cached_of(&wb, 1, 2), None);
    }

    #[test]
    fn a_cell_reading_an_unparseable_cell_is_also_fallback() {
        let mut wb = book(vec![formula_cell(1, "=1+"), formula_cell(2, "=A1*2")]);
        let report = hydrate_workbook(&mut wb, &CalcOptions::default());
        assert_eq!(report.total_formulas, 2);
        assert_eq!(report.computed, 0, "{report:?}");
        assert_eq!(cached_of(&wb, 1, 2), None);
    }

    #[test]
    fn a_cycle_is_reported_and_left_uncomputed() {
        let mut wb = book(vec![formula_cell(1, "=B1+1"), formula_cell(2, "=A1+1")]);
        let report = hydrate_workbook(&mut wb, &CalcOptions::default());
        assert_eq!(report.cycles, 1, "{report:?}");
        assert_eq!(report.computed, 0);
        assert_eq!(cached_of(&wb, 1, 1), None);
    }

    #[test]
    fn an_existing_cache_is_kept_unless_force_recalc() {
        let mut wb = book(vec![num_cell(1, 2.0), formula_cell(2, "=A1+1")]);
        // seed a (deliberately stale) cache
        if let CellValue::Formula { cached, .. } = &mut wb.sheets[0].rows[0].cells[1].value {
            *cached = Some(CachedValue::Number(99.0));
        }
        let report = hydrate_workbook(&mut wb, &CalcOptions::default());
        assert_eq!(report.computed, 0, "{report:?}");
        assert_eq!(cached_of(&wb, 1, 2), Some("Number(99.0)".to_string()));

        let forced = CalcOptions {
            force_recalc: true,
            ..CalcOptions::default()
        };
        let report = hydrate_workbook(&mut wb, &forced);
        assert_eq!(report.computed, 1, "{report:?}");
        assert_eq!(cached_of(&wb, 1, 2), Some("Number(3.0)".to_string()));
    }

    #[test]
    fn an_unknown_function_never_fabricates_a_number() {
        let mut wb = book(vec![formula_cell(1, "=NOSUCHFUNC(1,2)")]);
        let report = hydrate_workbook(&mut wb, &CalcOptions::default());
        assert_eq!(report.total_formulas, 1);
        // #NAME? is a legal cached error, so it may be stored — what must
        // never happen is a numeric cache.
        assert_ne!(cached_of(&wb, 1, 1), Some("Number(0.0)".to_string()));
    }
}
