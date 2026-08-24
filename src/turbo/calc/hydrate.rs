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
use crate::turbo::calc::functions::{FuncCtx, MAX_COLS, MAX_ROWS};
use crate::turbo::calc::parser::parse_formula;
use crate::turbo::calc::sheetdata::SheetData;
use crate::turbo::calc::spill::SpillRegion;
use crate::turbo::calc::value::{ArrayValue, CalcError, CalcValue};
use crate::turbo::calc::{CalcOptions, CalcReport};
use crate::turbo::write::model::{CachedValue, Cell, CellValue, Row, Sheet, Workbook};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

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

    // 4. Evaluate in dependency order. An array result spills: the anchor
    //    keeps its formula (cached with the array's first element) and every
    //    other element is written as a plain value cell at its own position —
    //    Excel's own no-rich-metadata representation of a spilled dynamic
    //    array. A 1x1 array is scalarized to its single element, which is what
    //    Excel itself stores for a function that happened to return a block of
    //    one.
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
        // A multi-cell array is a pending spill, persisted after its rectangle
        // has been checked against the whole sheet at once.
        let mut spill: Option<Arc<ArrayValue>> = None;
        // An error is a legitimate Excel result as long as the code is one
        // Excel can store; internal-only codes mean "we could not compute
        // this", which is a fallback, not a value.
        let stored = match value {
            Ok(CalcValue::Array(arr)) if arr.rows == 0 || arr.cols == 0 => None,
            Ok(CalcValue::Array(arr)) if arr.rows > 1 || arr.cols > 1 => {
                spill = Some(arr);
                None
            }
            Ok(CalcValue::Array(arr)) => scalar_cache(arr.get(0, 0)),
            Ok(CalcValue::Blank) => None,
            Ok(CalcValue::Error(e)) if e.is_internal() => None,
            Ok(v) => Some(v),
            Err(e) if e.cacheable() => Some(CalcValue::Error(e)),
            Err(_) => None,
        };
        if let Some(arr) = spill {
            match persist_spill(&mut data, wb, key, &arr) {
                PersistOutcome::Computed => {
                    report.computed += 1;
                    if arr.get(0, 0).is_error() {
                        report.error_cells += 1;
                    }
                }
                PersistOutcome::BlockedError => {
                    report.error_cells += 1;
                }
                PersistOutcome::Fallback => {
                    report.fallback += 1;
                }
            }
            continue;
        }
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
            None => {
                if options.force_recalc {
                    if let Some(sheet) = wb.sheets.get_mut(key.sheet as usize) {
                        let r = key.row + 1;
                        let c = u32::from(key.col) + 1;
                        if let Some(row) = sheet.rows.iter_mut().find(|row| row.row == r) {
                            if let Some(cell) = row.cells.iter_mut().find(|cell| cell.col == c) {
                                if let CellValue::Formula { cached, .. } = &mut cell.value {
                                    *cached = None;
                                }
                            }
                        }
                    }
                }
                report.fallback += 1;
            }
        }
    }

    // 5. Commit. `write_back` only fills slots whose value is a cacheable
    //    scalar, so nothing fabricated can reach the XML.
    data.write_back(wb);
    report
}

/// The cacheable scalar view of an array element: `None` for blank and
/// internal-only codes, which have no legal `<v>`.
fn scalar_cache(v: &CalcValue) -> Option<CalcValue> {
    match v {
        CalcValue::Blank => None,
        CalcValue::Error(e) if e.is_internal() => None,
        other => Some(other.clone()),
    }
}

enum PersistOutcome {
    Computed,
    BlockedError,
    Fallback,
}

fn cell_matches_calc_value(cv: &CellValue, val: &CalcValue) -> bool {
    match (cv, val) {
        (CellValue::Number(n1), CalcValue::Number(n2)) => (n1 - n2).abs() < f64::EPSILON,
        (CellValue::Str(s1), CalcValue::Text(s2)) => s1 == s2.as_ref(),
        (CellValue::Bool(b1), CalcValue::Bool(b2)) => b1 == b2,
        (CellValue::Error(e1), CalcValue::Error(e2)) => e1 == e2.code(),
        _ => false,
    }
}

fn cache_anchor_error(data: &mut SheetData, wb: &mut Workbook, key: &CellKey) {
    let (r0, c0) = (key.row, u32::from(key.col));
    if let Some(sheet) = wb.sheets.get_mut(key.sheet as usize) {
        if let Some(prev) = sheet.spills.iter().find(|s| s.anchor == (r0, c0)).copied() {
            remove_region_cells(sheet, prev, None);
        }
        sheet.spills.retain(|s| s.anchor != (r0, c0));
        let r = r0 + 1;
        let c = c0 + 1;
        if let Some(row) = sheet.rows.iter_mut().find(|row| row.row == r) {
            if let Some(cell) = row.cells.iter_mut().find(|cell| cell.col == c) {
                if let CellValue::Formula { cached, .. } = &mut cell.value {
                    *cached = Some(CachedValue::Error("#SPILL!".to_string()));
                }
            }
        }
    }
    let _ = data.update_computed(key.sheet, r0, c0, CalcValue::Error(CalcError::Spill));
}

fn remove_region_cells(
    sheet: &mut Sheet,
    old_region: SpillRegion,
    new_region: Option<SpillRegion>,
) {
    let (or0, oc0) = old_region.anchor;
    for r in or0..or0 + old_region.rows {
        for c in oc0..oc0 + old_region.cols {
            if r == or0 && c == oc0 {
                continue; // anchor stays
            }
            if let Some(nr) = new_region {
                let (nr0, nc0) = nr.anchor;
                if r >= nr0 && r < nr0 + nr.rows && c >= nc0 && c < nc0 + nr.cols {
                    continue; // inside new region, keep
                }
            }
            let row_num = r + 1;
            let col_num = c + 1;
            if let Some(row) = sheet.rows.iter_mut().find(|row| row.row == row_num) {
                row.cells.retain(|cell| cell.col != col_num);
            }
        }
    }
    sheet.rows.retain(|row| !row.cells.is_empty());
}

/// Materialize a spilled array into `wb` using Excel's no-rich-metadata model:
/// the anchor keeps its formula (its cached `<v>` comes from the first element
/// via `write_back`), and every other element becomes a plain value cell at its
/// row/col.
fn persist_spill(
    data: &mut SheetData,
    wb: &mut Workbook,
    key: &CellKey,
    arr: &Arc<ArrayValue>,
) -> PersistOutcome {
    let (rows, cols) = arr.shape();
    let (r0, c0) = (key.row, u32::from(key.col));
    if r0 + rows > MAX_ROWS || c0 + cols > MAX_COLS as u32 {
        cache_anchor_error(data, wb, key);
        return PersistOutcome::BlockedError;
    }
    let Some(sheet) = wb.sheets.get_mut(key.sheet as usize) else {
        return PersistOutcome::Fallback;
    };

    // Anchor's own previous region, if any
    let my_prev_region = sheet.spills.iter().find(|s| s.anchor == (r0, c0)).copied();

    // Obstacle rule (only a cell that is non-empty, not the anchor):
    for dr in 0..rows {
        for dc in 0..cols {
            if dr == 0 && dc == 0 {
                continue;
            }
            let r = r0 + dr;
            let c = c0 + dc;
            let row_1b = r + 1;
            let col_1b = c + 1;

            let cell_opt = sheet
                .rows
                .iter()
                .find(|row| row.row == row_1b)
                .and_then(|row| row.cells.iter().find(|cell| cell.col == col_1b));

            if let Some(cell) = cell_opt {
                if !matches!(cell.value, CellValue::Empty) {
                    // 1. Inside this anchor's previous region (sheet.spills find by anchor) -> reclaimable (own).
                    let inside_my_prev = my_prev_region.is_some_and(|pr| {
                        let (pr0, pc0) = pr.anchor;
                        r >= pr0 && r < pr0 + pr.rows && c >= pc0 && c < pc0 + pr.cols
                    });
                    if inside_my_prev {
                        continue;
                    }

                    // 2. Inside a different anchor's region -> obstacle (foreign spill).
                    let inside_other_spill = sheet.spills.iter().any(|s| {
                        s.anchor != (r0, c0)
                            && r >= s.anchor.0
                            && r < s.anchor.0 + s.rows
                            && c >= s.anchor.1
                            && c < s.anchor.1 + s.cols
                    });
                    if inside_other_spill {
                        cache_anchor_error(data, wb, key);
                        return PersistOutcome::BlockedError;
                    }

                    // 3. Value-matches computed element -> reclaimable (reload fallback).
                    let expected_val = arr.get(dr, dc);
                    if cell_matches_calc_value(&cell.value, expected_val) {
                        continue;
                    }

                    // 4. Else -> obstacle -> Blocked.
                    cache_anchor_error(data, wb, key);
                    return PersistOutcome::BlockedError;
                }
            }
        }
    }

    // Shrink by region: remove only previous-region cells outside the new region
    let new_region = SpillRegion {
        anchor: (r0, c0),
        rows,
        cols,
    };
    if let Some(prev) = my_prev_region {
        remove_region_cells(sheet, prev, Some(new_region));
    }

    // Update sheet.spills: retain out anchor's old entry and push new_region
    sheet.spills.retain(|s| s.anchor != (r0, c0));
    sheet.spills.push(new_region);

    // Anchor: give the resolver the first element so downstream formulas read it
    let _ = data.update_computed(
        key.sheet,
        key.row,
        u32::from(key.col),
        arr.get(0, 0).clone(),
    );

    // Materialize non-anchor plain cells into sheet.rows
    materialize_spill_cells(sheet, r0, c0, arr);

    // Downstream visibility: push all non-blank elements into SheetData
    for (i, v) in arr.data.iter().enumerate() {
        let dr = (i as u32) / cols;
        let dc = (i as u32) % cols;
        if dr == 0 && dc == 0 {
            continue;
        }
        if let Some(cv) = scalar_cache(v) {
            let _ = data.update_computed(key.sheet, r0 + dr, c0 + dc, cv);
        }
    }

    PersistOutcome::Computed
}

/// Write the spilled values of `arr` (whose anchor is `r0, c0`, 0-based) into
/// `sheet` as plain cells. The anchor cell itself keeps its formula. Blank and
/// internal-only elements leave their cell empty. Existing rows are reused
/// (cells stay column-sorted) and new rows are appended then sorted, so the
/// emitted XML is always strictly ascending.
fn materialize_spill_cells(sheet: &mut Sheet, r0: u32, c0: u32, arr: &ArrayValue) {
    let (_, cols) = arr.shape();
    let mut new_rows: BTreeMap<u32, Vec<(u32, CellValue)>> = BTreeMap::new();
    for (i, v) in arr.data.iter().enumerate() {
        let dr = (i as u32) / cols;
        let dc = (i as u32) % cols;
        if dr == 0 && dc == 0 {
            continue; // the anchor keeps its formula
        }
        let Some(cv) = spill_element_value(v) else {
            continue; // blank / internal-only element: leave the cell empty
        };
        new_rows
            .entry(r0 + dr + 1)
            .or_default()
            .push((c0 + dc + 1, cv));
    }
    for (row_num, cells) in new_rows {
        if let Some(row) = sheet.rows.iter_mut().find(|r| r.row == row_num) {
            for (col, cv) in cells {
                match row.cells.binary_search_by(|c| c.col.cmp(&col)) {
                    Ok(pos) => {
                        row.cells[pos].value = cv;
                    }
                    Err(pos) => row.cells.insert(pos, Cell::new(col, cv)),
                }
            }
        } else {
            let mut row = Row::new(row_num);
            for (col, cv) in cells {
                row.cells.push(Cell::new(col, cv));
            }
            sheet.rows.push(row);
        }
    }
    sheet.rows.sort_by_key(|r| r.row);
}

/// A spill element as a write-model cell value. Cacheable scalars become typed
/// cells; blank and internal-only codes have no legal XML value and become
/// `None` (the cell is left empty).
fn spill_element_value(v: &CalcValue) -> Option<CellValue> {
    match v {
        CalcValue::Number(n) => Some(CellValue::Number(*n)),
        CalcValue::Text(s) => Some(CellValue::Str(s.to_string())),
        CalcValue::Bool(b) => Some(CellValue::Bool(*b)),
        CalcValue::Error(e) if e.cacheable() => Some(CellValue::Error(e.code().to_string())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turbo::write::model::{CachedValue, Cell, FormulaKind, Row, Sheet};
    use pretty_assertions::{assert_eq, assert_ne};

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

    #[test]
    fn probe_index_eval() {
        use crate::turbo::calc::eval::eval;
        use crate::turbo::calc::functions::FuncCtx;
        use crate::turbo::calc::parser::parse_formula;
        use crate::turbo::calc::sheetdata::SheetData;
        let mut wb = Workbook::new();
        let mut r = Row::new(6);
        r.cells = vec![Cell::new(1, CellValue::Str("Tom".into()))];
        wb.sheets[0].rows = vec![r];
        let data = SheetData::build(&wb);
        let expr = parse_formula("=INDEX(A6:B7,1,1)").unwrap();
        let ctx = FuncCtx {
            date1904: false,
            sheet: 0,
            row: 2,
            col: 2,
            resolver: &data,
        };
        let r = eval(&expr, &ctx);
        eprintln!("PROBE INDEX => {r:?}");
    }

    fn cell_value_of(wb: &Workbook, row: u32, col: u32) -> Option<&CellValue> {
        let s = &wb.sheets[0];
        let r = s.rows.iter().find(|r| r.row == row)?;
        r.cells.iter().find(|c| c.col == col).map(|c| &c.value)
    }

    #[test]
    fn a_true_spill_writes_each_value_as_a_plain_cell_and_caches_the_anchor() {
        // =SEQUENCE(3) in A1 spills A1:A3 with {1;2;3}. The anchor keeps its
        // formula (cached 1); A2 and A3 become plain Number cells.
        let mut wb = book(vec![formula_cell(1, "=SEQUENCE(3)")]);
        let report = hydrate_workbook(&mut wb, &CalcOptions::default());
        assert_eq!(report.total_formulas, 1);
        assert_eq!(report.computed, 1, "{report:?}");
        assert_eq!(report.fallback, 0, "{report:?}");
        // the anchor is cached with the array's first element...
        assert_eq!(cached_of(&wb, 1, 1), Some("Number(1.0)".to_string()));
        assert!(matches!(
            cell_value_of(&wb, 1, 1),
            Some(CellValue::Formula { .. })
        ));
        // ...and the rest of the spill are plain values at A2 / A3
        assert!(matches!(
            cell_value_of(&wb, 2, 1),
            Some(CellValue::Number(2.0))
        ));
        assert!(matches!(
            cell_value_of(&wb, 3, 1),
            Some(CellValue::Number(3.0))
        ));
    }

    #[test]
    fn a_1x1_array_is_scalarized_to_a_plain_cache() {
        let mut wb = book(vec![formula_cell(1, "=MUNIT(1)")]);
        let report = hydrate_workbook(&mut wb, &CalcOptions::default());
        assert_eq!(report.computed, 1, "{report:?}");
        assert_eq!(cached_of(&wb, 1, 1), Some("Number(1.0)".to_string()));
        // no spill cells appeared: only the anchor row exists
        assert_eq!(wb.sheets[0].rows.len(), 1);
    }

    #[test]
    fn two_dimensional_spill_materializes_row_major() {
        let mut wb = book(vec![formula_cell(1, "=SEQUENCE(2,3,10,10)")]);
        let report = hydrate_workbook(&mut wb, &CalcOptions::default());
        assert_eq!(report.total_formulas, 1);
        assert_eq!(report.computed, 1, "{report:?}");
        assert_eq!(report.error_cells, 0);
        assert_eq!(cached_of(&wb, 1, 1), Some("Number(10.0)".to_string()));

        assert!(
            matches!(cell_value_of(&wb, 1, 2), Some(CellValue::Number(n)) if (*n - 20.0).abs() < f64::EPSILON)
        );
        assert!(
            matches!(cell_value_of(&wb, 1, 3), Some(CellValue::Number(n)) if (*n - 30.0).abs() < f64::EPSILON)
        );
        assert!(
            matches!(cell_value_of(&wb, 2, 1), Some(CellValue::Number(n)) if (*n - 40.0).abs() < f64::EPSILON)
        );
        assert!(
            matches!(cell_value_of(&wb, 2, 2), Some(CellValue::Number(n)) if (*n - 50.0).abs() < f64::EPSILON)
        );
        assert!(
            matches!(cell_value_of(&wb, 2, 3), Some(CellValue::Number(n)) if (*n - 60.0).abs() < f64::EPSILON)
        );
    }

    #[test]
    fn blocked_spill_caches_spill_error_without_clobbering() {
        let mut wb = Workbook::new();
        let mut r1 = Row::new(1);
        r1.cells = vec![formula_cell(1, "=SEQUENCE(2)")];
        let mut r2 = Row::new(2);
        r2.cells = vec![num_cell(1, 99.0)];
        wb.sheets[0].rows = vec![r1, r2];
        let report = hydrate_workbook(&mut wb, &CalcOptions::default());
        assert_eq!(report.computed, 0, "{report:?}");
        assert_eq!(report.error_cells, 1, "{report:?}");
        assert_eq!(report.fallback, 0, "{report:?}");
        assert_eq!(cached_of(&wb, 1, 1), Some("Error(\"#SPILL!\")".to_string()));
        assert!(
            matches!(cell_value_of(&wb, 2, 1), Some(CellValue::Number(n)) if (*n - 99.0).abs() < f64::EPSILON)
        );
    }

    #[test]
    fn clearing_obstacle_rehydrates_and_repeat_recalc_is_idempotent() {
        let mut wb = Workbook::new();
        let mut r1 = Row::new(1);
        r1.cells = vec![formula_cell(1, "=SEQUENCE(2)")];
        let mut r2 = Row::new(2);
        r2.cells = vec![num_cell(1, 99.0)];
        wb.sheets[0].rows = vec![r1, r2];

        // Pass 1: blocked by 99.0
        let rep1 = hydrate_workbook(&mut wb, &CalcOptions::default());
        assert_eq!(rep1.error_cells, 1);
        assert_eq!(cached_of(&wb, 1, 1), Some("Error(\"#SPILL!\")".to_string()));

        // Clear obstacle at row 2 col 1
        wb.sheets[0].rows[1].cells.clear();
        let forced = CalcOptions {
            force_recalc: true,
            ..CalcOptions::default()
        };

        // Pass 2: now unblocked -> computes
        let rep2 = hydrate_workbook(&mut wb, &forced);
        assert_eq!(rep2.computed, 1);
        assert_eq!(rep2.error_cells, 0);
        assert_eq!(cached_of(&wb, 1, 1), Some("Number(1.0)".to_string()));
        assert!(
            matches!(cell_value_of(&wb, 2, 1), Some(CellValue::Number(n)) if (*n - 2.0).abs() < f64::EPSILON)
        );

        // Pass 3: repeat recalc is idempotent (self-cells are not treated as obstacles)
        let rep3 = hydrate_workbook(&mut wb, &forced);
        assert_eq!(rep3.computed, 1);
        assert_eq!(rep3.error_cells, 0);
        assert_eq!(cached_of(&wb, 1, 1), Some("Number(1.0)".to_string()));
        assert!(
            matches!(cell_value_of(&wb, 2, 1), Some(CellValue::Number(n)) if (*n - 2.0).abs() < f64::EPSILON)
        );
    }

    #[test]
    fn shrink_clears_vacated_cells() {
        let mut wb = book(vec![formula_cell(1, "=SEQUENCE(5)")]);
        let forced = CalcOptions {
            force_recalc: true,
            ..CalcOptions::default()
        };
        let rep1 = hydrate_workbook(&mut wb, &forced);
        assert_eq!(rep1.computed, 1);
        assert!(
            matches!(cell_value_of(&wb, 5, 1), Some(CellValue::Number(n)) if (*n - 5.0).abs() < f64::EPSILON)
        );
        assert_eq!(wb.sheets[0].rows.len(), 5);

        // Shrink formula from SEQUENCE(5) to SEQUENCE(2)
        if let CellValue::Formula { text, cached, .. } = &mut wb.sheets[0].rows[0].cells[0].value {
            *text = "=SEQUENCE(2)".to_string();
            *cached = None;
        }
        let rep2 = hydrate_workbook(&mut wb, &forced);
        assert_eq!(rep2.computed, 1);
        assert!(
            matches!(cell_value_of(&wb, 2, 1), Some(CellValue::Number(n)) if (*n - 2.0).abs() < f64::EPSILON)
        );
        assert!(cell_value_of(&wb, 3, 1).is_none());
        assert!(cell_value_of(&wb, 4, 1).is_none());
        assert!(cell_value_of(&wb, 5, 1).is_none());
        assert_eq!(wb.sheets[0].rows.len(), 2);
    }

    #[test]
    fn downstream_formula_reads_spill_in_same_pass() {
        // A1 = SEQUENCE(3) (spills A1:A3)
        // A4 = SUM(A1:A3)
        let mut wb = Workbook::new();
        let mut r1 = Row::new(1);
        r1.cells = vec![formula_cell(1, "=SEQUENCE(3)")];
        let mut r4 = Row::new(4);
        r4.cells = vec![formula_cell(1, "=SUM(A1:A3)")];
        wb.sheets[0].rows = vec![r1, r4];

        let report = hydrate_workbook(&mut wb, &CalcOptions::default());
        assert_eq!(report.total_formulas, 2);
        assert_eq!(report.computed, 2, "{report:?}");
        assert_eq!(report.fallback, 0, "{report:?}");
        assert_eq!(cached_of(&wb, 1, 1), Some("Number(1.0)".to_string()));
        assert_eq!(cached_of(&wb, 4, 1), Some("Number(6.0)".to_string()));
    }

    #[test]
    fn forced_recalc_overwrites_stale_with_spill_error() {
        let mut wb = Workbook::new();
        let mut r1 = Row::new(1);
        let mut f = formula_cell(1, "=SEQUENCE(2)");
        if let CellValue::Formula { cached, .. } = &mut f.value {
            *cached = Some(CachedValue::Number(42.0));
        }
        r1.cells = vec![f];
        let mut r2 = Row::new(2);
        r2.cells = vec![num_cell(1, 99.0)];
        wb.sheets[0].rows = vec![r1, r2];

        // Without force_recalc, stale cache stands
        let rep1 = hydrate_workbook(&mut wb, &CalcOptions::default());
        assert_eq!(rep1.computed, 0);
        assert_eq!(cached_of(&wb, 1, 1), Some("Number(42.0)".to_string()));

        // With force_recalc, stale cache is overwritten with #SPILL!
        let forced = CalcOptions {
            force_recalc: true,
            ..CalcOptions::default()
        };
        let rep2 = hydrate_workbook(&mut wb, &forced);
        assert_eq!(rep2.error_cells, 1);
        assert_eq!(cached_of(&wb, 1, 1), Some("Error(\"#SPILL!\")".to_string()));
    }

    #[test]
    fn recalc_after_input_change_recomputes_spill_not_spill() {
        // A1 holds 2.0, B1 = SEQUENCE(A1) spills B1:B2 with 1, 2.
        let mut wb = Workbook::new();
        let mut r1 = Row::new(1);
        r1.cells = vec![num_cell(1, 2.0), formula_cell(2, "=SEQUENCE(A1)")];
        wb.sheets[0].rows = vec![r1];

        let rep1 = hydrate_workbook(&mut wb, &CalcOptions::default());
        assert_eq!(rep1.computed, 1);
        assert_eq!(cached_of(&wb, 1, 2), Some("Number(1.0)".to_string()));
        assert!(
            matches!(cell_value_of(&wb, 2, 2), Some(CellValue::Number(n)) if (*n - 2.0).abs() < f64::EPSILON)
        );

        // Change A1 to 3.0: B1 should recompute to 3 rows (B1..B3) with values 1, 2, 3 without #SPILL!
        if let Some(row) = wb.sheets[0].rows.iter_mut().find(|r| r.row == 1) {
            if let Some(cell) = row.cells.iter_mut().find(|c| c.col == 1) {
                cell.value = CellValue::Number(3.0);
            }
        }
        let forced = CalcOptions {
            force_recalc: true,
            ..CalcOptions::default()
        };
        let rep2 = hydrate_workbook(&mut wb, &forced);
        assert_eq!(rep2.computed, 1);
        assert_eq!(rep2.error_cells, 0);
        assert_eq!(cached_of(&wb, 1, 2), Some("Number(1.0)".to_string()));
        assert!(
            matches!(cell_value_of(&wb, 2, 2), Some(CellValue::Number(n)) if (*n - 2.0).abs() < f64::EPSILON)
        );
        assert!(
            matches!(cell_value_of(&wb, 3, 2), Some(CellValue::Number(n)) if (*n - 3.0).abs() < f64::EPSILON)
        );
    }

    #[test]
    fn shrunk_spill_keeps_user_value_outside_region() {
        // A1 = SEQUENCE(4) spills A1..A4. User data at A10 = 999.0.
        let mut wb = Workbook::new();
        let mut r1 = Row::new(1);
        r1.cells = vec![formula_cell(1, "=SEQUENCE(4)")];
        let mut r10 = Row::new(10);
        r10.cells = vec![num_cell(1, 999.0)];
        wb.sheets[0].rows = vec![r1, r10];

        let rep1 = hydrate_workbook(&mut wb, &CalcOptions::default());
        assert_eq!(rep1.computed, 1);
        assert_eq!(wb.sheets[0].spills.len(), 1);
        assert_eq!(
            wb.sheets[0].spills[0],
            SpillRegion {
                anchor: (0, 0),
                rows: 4,
                cols: 1
            }
        );
        assert!(
            matches!(cell_value_of(&wb, 10, 1), Some(CellValue::Number(n)) if (*n - 999.0).abs() < f64::EPSILON)
        );

        // Shrink A1 to SEQUENCE(2)
        if let CellValue::Formula { text, cached, .. } = &mut wb.sheets[0].rows[0].cells[0].value {
            *text = "=SEQUENCE(2)".to_string();
            *cached = None;
        }
        let forced = CalcOptions {
            force_recalc: true,
            ..CalcOptions::default()
        };
        let rep2 = hydrate_workbook(&mut wb, &forced);
        assert_eq!(rep2.computed, 1);
        assert_eq!(
            wb.sheets[0].spills[0],
            SpillRegion {
                anchor: (0, 0),
                rows: 2,
                cols: 1
            }
        );
        assert!(matches!(
            cell_value_of(&wb, 1, 1),
            Some(CellValue::Formula { .. })
        ));
        assert!(
            matches!(cell_value_of(&wb, 2, 1), Some(CellValue::Number(n)) if (*n - 2.0).abs() < f64::EPSILON)
        );
        assert!(cell_value_of(&wb, 3, 1).is_none());
        assert!(cell_value_of(&wb, 4, 1).is_none());
        // User cell at row 10 must be preserved!
        assert!(
            matches!(cell_value_of(&wb, 10, 1), Some(CellValue::Number(n)) if (*n - 999.0).abs() < f64::EPSILON)
        );
    }

    #[test]
    fn blocked_spill_clears_its_previous_region() {
        // A1 = SEQUENCE(2) spills A1..A2
        let mut wb = book(vec![formula_cell(1, "=SEQUENCE(2)")]);
        let rep1 = hydrate_workbook(&mut wb, &CalcOptions::default());
        assert_eq!(rep1.computed, 1);
        assert_eq!(wb.sheets[0].spills.len(), 1);
        assert_eq!(
            wb.sheets[0].spills[0],
            SpillRegion {
                anchor: (0, 0),
                rows: 2,
                cols: 1
            }
        );
        assert!(
            matches!(cell_value_of(&wb, 2, 1), Some(CellValue::Number(n)) if (*n - 2.0).abs() < f64::EPSILON)
        );

        // User puts obstacle at A3 (row 3, col 1 = 99.0)
        let mut r3 = Row::new(3);
        r3.cells = vec![num_cell(1, 99.0)];
        wb.sheets[0].rows.push(r3);

        // Change formula to =SEQUENCE(3) which expands into A3
        if let CellValue::Formula { text, cached, .. } = &mut wb.sheets[0].rows[0].cells[0].value {
            *text = "=SEQUENCE(3)".to_string();
            *cached = None;
        }

        // Force recalc: A3 blocks the spill -> A1 becomes #SPILL!, A2 (previous neighbor) is cleared!
        let forced = CalcOptions {
            force_recalc: true,
            ..CalcOptions::default()
        };
        let rep2 = hydrate_workbook(&mut wb, &forced);
        assert_eq!(rep2.error_cells, 1);
        assert_eq!(cached_of(&wb, 1, 1), Some("Error(\"#SPILL!\")".to_string()));
        assert!(cell_value_of(&wb, 2, 1).is_none()); // previous neighbor cleared!
        assert!(
            matches!(cell_value_of(&wb, 3, 1), Some(CellValue::Number(n)) if (*n - 99.0).abs() < f64::EPSILON)
        );
        assert!(wb.sheets[0].spills.is_empty());
    }
}
