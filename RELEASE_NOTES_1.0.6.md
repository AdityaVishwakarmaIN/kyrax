# kyrax 1.0.6

Release date: 2026-08-17

## Highlights

- Completes the 488-function Excel-standard formula contract.
- Adds `from kyrax import formulas` with Rust-backed evaluation, inventory,
  dependency, and workbook recalculation APIs.
- Persists dynamic-array results through the XLSX write path.
- Expands Excel behavior across engineering, financial, statistical, math,
  date/time, text, lookup, information, logical, and web functions.
- Preserves editable formula kinds, shared-formula translation, `_xlfn`
  prefixes, and typed cached values through load and save.
- Applies Excel's top-left scalar selection to IS-family array arguments.
- Keeps Python as a thin facade; formula behavior remains in Rust.

## Validation

- 488/488 public standard functions registered.
- 0 Excel-refereed failures in the final 511-row matrix.
- 7/7 generated formula-family workbooks opened normally through Excel COM.
- Formula Python tests: 133 passed, 1 expected failure.
- Turbo Python tests: 265 passed, 18 skipped.
- Rust release core: 1279 passed. An unrelated temporary `debug_inflate`
  probe panicked and is not part of the release test suite.
- Non-volatile workbook output is byte-deterministic.
