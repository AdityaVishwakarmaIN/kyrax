//! A4 Wave-1 architecture-stress tests: formula registry / semantics, date
//! systems, dependency invalidation, cycles, workbook diff, provenance.
//!
//! Harness-independent baseline. Uses ONLY public crate APIs (`kyrax::turbo::*`)
//! plus a local minimal workbook builder. `calc::testkit` is `#[cfg(test)]`
//! private and cannot be imported here, so evaluation is exercised through the
//! real `hydrate_workbook` path and dependency queries through the public
//! `deps` / `provenance` / `diff` APIs.
//!
//! Deferred (see plans/architecture_stress/NOTES/A4.md): end-to-end spilling
//! formulas, the LAMBDA family through public hydrate (KNOWN-GAP), shared
//! `t="shared"` formulas, hostile limits (A6), Excel COM oracles (A4/A6).

use kyrax::turbo::calc::deps::{CellKey, DepError, DependencyGraph, RefResolver};
use kyrax::turbo::calc::functions::registry;
use kyrax::turbo::calc::spill::{SpillMap, SpillOutcome, SpillRegion};
use kyrax::turbo::calc::{CalcOptions, hydrate_workbook, parse_formula};
use kyrax::turbo::features::diff::{ChangeKind, diff_workbooks};
use kyrax::turbo::features::provenance::Provenance;
use kyrax::turbo::write::{
    CachedValue, Cell, CellValue, FormulaKind, Row, Sheet, Workbook, write_workbook_bytes,
};

/// Local single-sheet resolver: `Sheet1` maps to sheet id 0, everything else
/// (names / tables / other sheets) resolves `Unknown` -> safe fallback, never
/// a fabricated edge.
struct SheetResolver;

impl RefResolver for SheetResolver {
    fn sheet_id(&self, name: &str) -> Option<u32> {
        if name.eq_ignore_ascii_case("Sheet1") {
            Some(0)
        } else {
            None
        }
    }
}

fn sheet1(cells: Vec<(u32, u32, CellValue)>) -> Workbook {
    let mut rows: Vec<Row> = Vec::new();
    let mut by_row: std::collections::BTreeMap<u32, Row> = Default::default();
    for (row, col, value) in cells {
        let r = by_row.entry(row).or_insert_with(|| Row::new(row));
        r.cells.push(Cell::new(col, value));
    }
    for (_, r) in by_row {
        rows.push(r);
    }
    let mut sheet = Sheet::new("Sheet1");
    sheet.rows = rows;
    let mut wb = Workbook::new();
    wb.sheets.clear();
    wb.sheets.push(sheet);
    wb
}

fn formula(text: &str) -> CellValue {
    CellValue::Formula {
        text: text.to_string(),
        kind: FormulaKind::Normal,
        cached: None,
    }
}

fn cached_number(wb: &Workbook, sheet: usize, row: usize, col: usize) -> f64 {
    match &wb.sheets[sheet].rows[row].cells[col].value {
        CellValue::Formula {
            cached: Some(CachedValue::Number(n)),
            ..
        } => *n,
        other => panic!("expected cached number at ({sheet},{row},{col}), got {other:?}"),
    }
}

fn hydrate(wb: &mut Workbook, date1904: bool) -> kyrax::turbo::CalcReport {
    hydrate_workbook(
        wb,
        &CalcOptions {
            date1904,
            force_recalc: true,
            max_iterations: 0,
        },
    )
}

// ---------------------------------------------------------------------------
// A4-REG-01 / FN-01 basics through the real registry + evaluation path.
// ---------------------------------------------------------------------------

#[test]
fn registry_lookup_is_case_insensitive_and_unknown_is_none() {
    let reg = registry();
    let upper = reg.get("SUM").expect("SUM must be registered");
    let mixed = reg.get("sum").expect("lowercase lookup must resolve");
    assert!(
        std::ptr::eq(upper, mixed),
        "case variants must map to one spec"
    );
    assert!(
        reg.get("NOSUCHFUNCTION").is_none(),
        "unknown name -> fallback"
    );
}

#[test]
fn fn_sum_reads_plain_cells_through_hydration() {
    let mut wb = sheet1(vec![
        (1, 1, CellValue::Number(10.0)),
        (2, 1, CellValue::Number(32.0)),
        (3, 1, formula("=SUM(A1:A2)")),
    ]);
    let report = hydrate(&mut wb, false);
    assert_eq!(report.computed, 1, "one formula computed");
    assert_eq!(report.fallback, 0);
    assert_eq!(cached_number(&wb, 0, 2, 0), 42.0);
}

#[test]
fn fn_if_chooses_branch_and_returns_text() {
    let mut wb = sheet1(vec![
        (1, 1, formula("=IF(1>0,\"yes\",\"no\")")),
        (2, 1, formula("=IF(0>1,\"yes\",\"no\")")),
    ]);
    hydrate(&mut wb, false);
    for (row, want) in [(0usize, "yes"), (1, "no")] {
        match &wb.sheets[0].rows[row].cells[0].value {
            CellValue::Formula {
                cached: Some(CachedValue::Str(s)),
                ..
            } => assert_eq!(s, want),
            other => panic!("expected cached text {want:?}, got {other:?}"),
        }
    }
}

#[test]
fn unknown_function_is_computed_name_error_not_fallback() {
    // An unknown function name is parseable and evaluates to Excel's own
    // #NAME? error, which IS a legal cached `t="e"` value (`error_cells`), not
    // an "I could not compute this" fallback. Genuine fallback (uncached) is
    // unparseable / external / address-function refs, covered separately.
    let mut wb = sheet1(vec![
        (1, 1, formula("=NOSUCHFUNCTION(1,2)")),
        (2, 1, formula("=1+2")),
    ]);
    let report = hydrate(&mut wb, false);
    assert_eq!(
        report.computed, 2,
        "both the #NAME? error result and =1+2 are computed"
    );
    assert_eq!(
        report.error_cells, 1,
        "#NAME? is a stored error, not fallback"
    );
    assert_eq!(report.fallback, 0);
    match &wb.sheets[0].rows[0].cells[0].value {
        CellValue::Formula {
            cached: Some(CachedValue::Error(s)),
            ..
        } => assert_eq!(s, "#NAME?"),
        other => panic!("unknown function must cache #NAME?, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// A4-DATE-01 (1900 system exact oracle; 1904 delta deferred to A4.md).
// ---------------------------------------------------------------------------

#[test]
fn date_serial_1900_system_matches_excel() {
    let mut wb = sheet1(vec![
        (1, 1, formula("=DATE(2020,1,15)")),
        (2, 1, formula("=DATE(2020,1,15)+1")),
    ]);
    hydrate(&mut wb, false);
    assert_eq!(cached_number(&wb, 0, 0, 0), 43845.0);
    assert_eq!(cached_number(&wb, 0, 1, 0), 43846.0);
}

// ---------------------------------------------------------------------------
// A4-CYCLE-01 / fallback: cycles are excluded and never computed.
// ---------------------------------------------------------------------------

#[test]
fn cycle_is_detected_and_excluded_from_order() {
    let mut g = DependencyGraph::new();
    let a1 = CellKey::new(0, 0, 0);
    let b1 = CellKey::new(0, 0, 1);
    let resolver = SheetResolver;
    g.add_formula(a1, &parse_formula("=B1").unwrap(), &resolver)
        .unwrap();
    g.add_formula(b1, &parse_formula("=A1").unwrap(), &resolver)
        .unwrap();
    let topo = g.topological();
    assert!(
        !topo.cycles.is_empty(),
        "A1<->B1 must be reported as a cycle"
    );
    assert!(topo.order.is_empty(), "cyclic cells must not be computed");
    assert!(topo.fallback.contains(&a1) && topo.fallback.contains(&b1));
}

#[test]
fn unresolved_name_excludes_cell_and_dependents_from_order() {
    let mut g = DependencyGraph::new();
    let a1 = CellKey::new(0, 0, 0);
    let a2 = CellKey::new(0, 1, 0);
    let resolver = SheetResolver;
    let err = g
        .add_formula(a1, &parse_formula("=UNKNOWN_NAME+1").unwrap(), &resolver)
        .expect_err("unknown defined name must be Unresolved");
    assert!(matches!(err, DepError::Unresolved { .. }));
    g.add_formula(a2, &parse_formula("=A1+1").unwrap(), &resolver)
        .unwrap();
    let topo = g.topological();
    assert!(
        topo.order.is_empty(),
        "dependent of unresolved cell must not compute"
    );
    assert!(topo.fallback.contains(&a1) && topo.fallback.contains(&a2));
}

#[test]
fn unparseable_formula_is_fallback() {
    let mut g = DependencyGraph::new();
    let a1 = CellKey::new(0, 0, 0);
    assert!(parse_formula("=1+").is_err());
    g.add_unparseable(a1).unwrap();
    let topo = g.topological();
    assert!(topo.order.is_empty());
    assert!(topo.fallback.contains(&a1));
}

// ---------------------------------------------------------------------------
// A4-PROV-01 / PROV-02: queries over the graph the evaluator would use.
// ---------------------------------------------------------------------------

fn chain_graph() -> (DependencyGraph, CellKey, CellKey, CellKey, CellKey) {
    let resolver = SheetResolver;
    let mut g = DependencyGraph::new();
    let a1 = CellKey::new(0, 0, 0);
    let a2 = CellKey::new(0, 1, 0);
    let a3 = CellKey::new(0, 2, 0);
    let b1 = CellKey::new(0, 0, 1);
    g.add_formula(a2, &parse_formula("=A1+1").unwrap(), &resolver)
        .unwrap();
    g.add_formula(a3, &parse_formula("=A2+1").unwrap(), &resolver)
        .unwrap();
    g.add_formula(b1, &parse_formula("=A2*2").unwrap(), &resolver)
        .unwrap();
    g.add_formula(a1, &parse_formula("=10").unwrap(), &resolver)
        .unwrap();
    // `topological()` sorts the spatial index that edge materialization
    // (`partition_point`) depends on; hydration always sorts before eval, so a
    // Provenance built over an un-sorted graph would not match the live graph.
    let _ = g.topological();
    (g, a1, a2, a3, b1)
}

#[test]
fn provenance_precedents_and_dependents_match_graph() {
    let (g, a1, a2, a3, _b1) = chain_graph();
    let p = Provenance::new(&g);
    assert_eq!(p.precedents(a3), vec![a2], "A3 reads A2 directly");
    assert!(p.dependents(a1).contains(&a2), "A1 feeds A2");
    let mut deep = p.dependents_deep(a1);
    deep.sort();
    let mut want = vec![a2, a3, CellKey::new(0, 0, 1)];
    want.sort();
    assert_eq!(deep, want, "all and only transitive dependents of A1");
    assert!(p.is_leaf(CellKey::new(0, 2, 1)), "unused cell is a leaf");
}

#[test]
fn provenance_impact_of_matches_hydration_graph() {
    let (g, a1, a2, a3, b1) = chain_graph();
    let p = Provenance::new(&g);
    let mut impact = p.impact_of(&[a1]);
    impact.sort();
    let mut want = vec![a2, a3, b1];
    want.sort();
    assert_eq!(
        impact, want,
        "changing A1 recomputes exactly its dependents"
    );
}

// ---------------------------------------------------------------------------
// A4-DIFF-01 / DIFF-02: deterministic diff over real serialized workbooks.
// ---------------------------------------------------------------------------

#[test]
fn diff_identical_workbooks_is_empty_and_deterministic() {
    let mut wb = sheet1(vec![
        (1, 1, CellValue::Number(5.0)),
        (2, 1, formula("=1+2")),
    ]);
    hydrate(&mut wb, false);
    let a = write_workbook_bytes(&wb).expect("serialize a");
    let b = write_workbook_bytes(&wb).expect("serialize b");
    assert_eq!(
        a, b,
        "deterministic writer: identical input -> identical bytes"
    );
    let diff = diff_workbooks(&a, &b).expect("diff parses");
    assert!(diff.identical);
    assert!(diff.parts.is_empty());
    assert!(diff.cells.is_empty());
}

#[test]
fn diff_value_change_is_reported_not_ignored() {
    let mut wb = sheet1(vec![
        (1, 1, CellValue::Number(5.0)),
        (2, 1, formula("=A1+1")),
    ]);
    hydrate(&mut wb, false);
    let a = write_workbook_bytes(&wb).expect("serialize a");
    wb.sheets[0].rows[0].cells[0].value = CellValue::Number(8.0);
    hydrate(&mut wb, false);
    let b = write_workbook_bytes(&wb).expect("serialize b");
    let diff = diff_workbooks(&a, &b).expect("diff parses");
    assert!(!diff.identical);
    assert!(
        diff.cells
            .iter()
            .any(|c| c.kind == ChangeKind::ValueChanged || c.kind == ChangeKind::FormulaChanged),
        "changing A1 must surface a cell change, got {:?}",
        diff.cells
    );
}

/// Hostile-input case: run only under the A6 exact-PID timeout/RSS guard.
#[test]
#[ignore = "A6 exact-PID timeout/RSS guard required before running hostile malformed-ZIP case"]
fn diff_malformed_zip_returns_typed_error_not_partial_result() {
    let garbage = b"this is not a zip archive, no central directory at all";
    let err = diff_workbooks(garbage, garbage).expect_err("garbage must not diff clean");
    assert!(matches!(err, kyrax::turbo::TurboError::Format(_)));
}

// ---------------------------------------------------------------------------
// A4-SPILL-01: spill region geometry through the public SpillMap API.
//
// KNOWN-GAP seam: a top-level spilling formula (`SEQUENCE`, `FILTER`) cannot
// be asserted end-to-end through hydration because `hydrate` treats array
// results as uncacheable (`Ok(CalcValue::Array(_)) => None` -> fallback). The
// spill ENGINE itself is public (`calc::spill::SpillMap`) and is the exact
// code the eval/overlay layer drives, so these tests pin its four rules:
// never overwrite, clear-before-claim, explicit ownership, `A1#` via
// `region_of`. All cases are small and non-hostile.
// ---------------------------------------------------------------------------

fn free_cell(_r: u32, _c: u32) -> bool {
    false
}

#[test]
fn spill_commits_and_queries_ownership() {
    let mut m = SpillMap::new();
    let reg = SpillRegion {
        anchor: (2, 3),
        rows: 2,
        cols: 3,
    };
    assert_eq!(
        m.probe(&free_cell, reg.anchor, reg.rows, reg.cols),
        SpillOutcome::Ok(reg)
    );
    m.commit(reg);
    assert_eq!(m.owner_of((2, 3)), Some((2, 3)), "anchor owns itself");
    assert_eq!(m.owner_of((2, 5)), Some((2, 3)), "spilled result owned");
    assert_eq!(m.owner_of((3, 5)), Some((2, 3)));
    assert_eq!(
        m.owner_of((1, 5)),
        None,
        "outside the rectangle stays unowned"
    );
    assert_eq!(m.region_of((2, 3)), Some(&reg), "A1# resolution");
}

#[test]
fn spill_never_overwrites_occupied_content() {
    let m = SpillMap::new();
    let occupied = |r: u32, c: u32| r == 3 && c == 4;
    let out = m.probe(&occupied, (2, 3), 2, 3);
    assert!(
        matches!(out, SpillOutcome::Blocked { by: (3, 4) }),
        "blocked probe must report the first obstacle, got {out:?}"
    );
    assert_eq!(m.owner_of((3, 4)), None, "a blocked probe claims nothing");
}

#[test]
fn spill_foreign_anchor_blocks_but_own_anchor_is_transparent() {
    let mut m = SpillMap::new();
    m.commit(SpillRegion {
        anchor: (1, 1),
        rows: 2,
        cols: 2,
    });
    assert!(
        matches!(
            m.probe(&free_cell, (2, 1), 1, 1),
            SpillOutcome::Blocked { by: (2, 1) }
        ),
        "a different anchor cannot claim an already-claimed cell"
    );
    assert_eq!(
        m.probe(&free_cell, (1, 1), 2, 2),
        SpillOutcome::Ok(SpillRegion {
            anchor: (1, 1),
            rows: 2,
            cols: 2
        }),
        "the owning anchor re-probing its own region is transparent"
    );
}

#[test]
fn spill_shrink_unclaims_vacated_cells() {
    let mut m = SpillMap::new();
    m.commit(SpillRegion {
        anchor: (2, 2),
        rows: 2,
        cols: 2,
    });
    m.commit(SpillRegion {
        anchor: (2, 2),
        rows: 1,
        cols: 1,
    });
    assert_eq!(m.owner_of((2, 2)), Some((2, 2)), "kept cell stays owned");
    assert_eq!(
        m.owner_of((2, 3)),
        None,
        "vacated cell unclaimed after shrink"
    );
    assert_eq!(m.owner_of((3, 2)), None);
}

#[test]
fn spill_zero_area_commit_clears_the_region() {
    let mut m = SpillMap::new();
    m.commit(SpillRegion {
        anchor: (0, 0),
        rows: 2,
        cols: 2,
    });
    assert_eq!(m.region_of((0, 0)).map(|r| r.rows), Some(2));
    m.commit(SpillRegion {
        anchor: (0, 0),
        rows: 0,
        cols: 0,
    });
    assert_eq!(m.region_of((0, 0)), None, "zero-area commit clears");
    assert_eq!(m.owner_of((0, 1)), None);
}

#[test]
fn spill_off_grid_is_rejected() {
    let m = SpillMap::new();
    // MAX_ROWS is 1_048_576; a 2M-row rectangle cannot fit.
    assert_eq!(
        m.probe(&free_cell, (0, 0), 2_000_000, 1),
        SpillOutcome::OffGrid
    );
}

// ---------------------------------------------------------------------------
// A4-LAMBDA-01: KNOWN-GAP probes. Public `hydrate` routes every LAMBDA-family
// formula to fallback (verified: LET -> fallback 2/2, MAP/REDUCE/SCAN ->
// fallback 3/3). These are deterministic NEGATIVE capability probes that pin
// the gap and will fail the moment a real end-to-end seam computes them - they
// are never PASS claims.
// ---------------------------------------------------------------------------

fn assert_uncached(wb: &Workbook, row: usize, col: usize, formula: &str) {
    match &wb.sheets[0].rows[row].cells[col].value {
        CellValue::Formula { cached: None, .. } => {}
        other => panic!("{formula} must stay UNCACHED (KNOWN-GAP), got {other:?}"),
    }
}

#[test]
fn lambda_let_is_uncached_fallback_known_gap() {
    let mut wb = sheet1(vec![
        (1, 1, formula("=LET(x,10,x*2)")),
        (2, 1, formula("=LET(a,5,LET(b,a+1,b*2))")),
    ]);
    let report = hydrate(&mut wb, false);
    assert_eq!(
        report.fallback, 2,
        "LET cases must route to fallback (KNOWN-GAP), report {report:?}"
    );
    assert_uncached(&wb, 0, 0, "=LET(x,10,x*2)");
    assert_uncached(&wb, 1, 0, "=LET(a,5,LET(b,a+1,b*2))");
}

#[test]
fn lambda_map_reduce_scan_is_uncached_fallback_known_gap() {
    let mut wb = sheet1(vec![
        (1, 1, formula("=SUM(MAP({1,2,3},LAMBDA(a,a*2)))")),
        (2, 1, formula("=REDUCE(0,{1,2,3},LAMBDA(a,b,a+b))")),
        (3, 1, formula("=SUM(SCAN(0,{1,2,3},LAMBDA(a,b,a+b)))")),
    ]);
    let report = hydrate(&mut wb, false);
    assert_eq!(
        report.fallback, 3,
        "lambda-array consumers must route to fallback (KNOWN-GAP), report {report:?}"
    );
    assert_uncached(&wb, 0, 0, "=SUM(MAP(...))");
    assert_uncached(&wb, 1, 0, "=REDUCE(...)");
    assert_uncached(&wb, 2, 0, "=SUM(SCAN(...))");
}
