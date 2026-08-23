# Lane J — Final Gate Checklist (from FORMULA_GREEN_PLAN.txt §4.1)

Every check below must pass with **no exceptions** before the engine leaves
`engine-formula/`. This file is the gate walk list; each item names the
command that proves it.

| # | Check | Proof |
|---|-------|-------|
| 1 | **488/488 Excel-standard functions present** (30 missing → 0) | `from kyrax import formulas; len(formulas.list_functions())` — must equal the 488-entry standard list |
| 2 | **0 Excel-refereed failures** in matrix_v2 | `python tests_formulas/run_round2.py` → `formula-validation/round2/matrix_v2_draft.csv`; every `CONFIRMED-EXCEL` row's `verdict` = `PASS` |
| 3 | **0 array-write-path rows** | matrix_v2 draft: no row flagged `array-write-path` remains unhandled |
| 4 | **tests_formulas 100% green** | `python -m pytest tests_formulas -q` — 0 failures (Lane J's `xfail` pending rows may remain `xfail`, never fail) |
| 5 | **cargo test 100% green** | `cargo test --test direct_eval` and every lane's integration test from `kyrax/` |
| 6 | **No repair prompt (COM-verified)** | Excel COM opens every output family (`write_excel_turbo` × all feature sets); `Workbook.EnableAutoRecover` / open-check reports no repair dialog |
| 7 | **Determinism** | `python -m pytest tests_formulas/test_determinism.py -q` — identical non-volatile workbook ⇒ byte-identical output; volatile set (`NOW`/`TODAY`/`RAND`) excluded |
| 8 | **`from kyrax import formulas` works per doctests** | `python -m pytest --doctest-modules python/kyrax/formulas.py -q` — 3 doctests pass |

## Lane J regression gate (this lane)

- `tests_formulas/test_contract.py`
  - 99 `CONFIRMED-EXCEL` rows — engine must equal the Excel-measured
    `excel_value`; rows still waiting on a lane's fix are `xfail(strict=False)`
    with reason `pending lane X` (self-healing: the fix flips them to real pass).
  - 15 `ORACLE-WRONG` rows — **anti-regression**, hard-asserted: kyrax must
    KEEP its current Excel-matching value (e.g. `SUMPRODUCT({1,"2",3},{1,2,3})=10`).
- `tests_formulas/test_determinism.py` — byte-determinism for the
  non-volatile set; volatile set excluded.

## Final status (2026-08-16)

- 488/488 public standard names are registered and unique.
- `matrix_v2.csv`: 511 rows, 0 `FAIL`.
- Rust: 1264 passed, 0 failed, 3 ignored; Lane H: 4 passed.
- Python formulas: 132 passed, 1 expected-xfail; turbo: 239 passed, 18 skipped.
- Excel COM: 7/7 family workbooks opened with normal load, no repair mode.
- Determinism: 2 passed; doctests: 3 passed.
- Evidence: `formula-validation/reports/FINAL_REPORT_v2.md`.
