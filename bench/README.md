# bench/ — kyrax cost-per-task benchmark harness (not shipped)

`bench/` is excluded from the wheel (`[tool.maturin] python-source = "python"`
packages only `python/kyrax`) and from the sdist (the `include` whitelist in
`Cargo.toml`; verified with `cargo package --list`). It is the only place
competitor libraries may be imported.

## Environment

Competitors (openpyxl, XlsxWriter, python-calamine, pandas) and pyarrow live in a
**separate scratch venv**, never in `pyproject.toml` / `uv.lock`:

```bash
uv venv /path/to/benchenv/.venv --python 3.13
uv pip install --python /path/to/benchenv/.venv/bin/python \
    openpyxl==3.1.5 XlsxWriter==3.2.9 python-calamine pandas pyarrow==23.0.0
```

kyrax is **not installed** into that venv. The driver puts `<repo>/python` on the
workers' `PYTHONPATH`, so they import the package exactly as the last
`uv run maturin develop --uv --release -E pyarrow,pandas,polars` left it
(`python/kyrax/_kyrax.abi3.so`, abi3 so any CPython >= 3.10 loads it).
Rebuild with `maturin develop --release` before measuring — a debug build
is meaningless here.

## Running

```bash
export BENCH_PYTHON=/path/to/benchenv/.venv/bin/python
$BENCH_PYTHON bench/run.py --out baseline     # full matrix, one-off: ~14 min (828 s)
$BENCH_PYTHON bench/run.py --quick            # merge gate: kyrax rows, small+medium, ~30 s
$BENCH_PYTHON bench/compare.py bench/results/baseline.json bench/results/quick.json
```

Other knobs: `--kyrax-only` (kyrax rows, all sizes), `--sizes`, `--tasks`,
`--libs`, `--repeats`, `--cache` (fixture dir; default `bench/.cache/`, untracked).
Fixtures are generated once, deterministically (seeded), with XlsxWriter
(shared-strings table, date number format) — 8 columns: int, float, two
string columns (50 repeated categories / mostly unique labels), date, bool,
int, float; header row + 1k / 100k / 1M rows.

## What is measured

Each measurement is a fresh `python bench/worker.py TASK LIB SIZE ...` process.

- **CPU-s** (`cpu_task_s`, the primary metric): user+sys of the worker process
  (all threads) over the timed section = library import + task. Interpreter
  start-up and input-data construction are excluded (identical for all libs).
  `cpu_total_s` is the whole-process figure from the parent's `os.wait4()`.
- **Peak RSS** (`rss_mb`): the worker's `VmHWM` at exit. (`wait4`'s
  `ru_maxrss` is also stored, but Linux folds the parent's RSS at exec into it,
  so it is unreliable once the driver itself has grown.)
- **wall** (info only).
- Median of N repeats (5 small, 3 medium/large), round-robin across libraries;
  noise band = min/max and `(max-min)/median`.
- `ratio` = kyrax median / best competitor median (>1 = kyrax worse).
- Outputs are verified by reopening with python-calamine: WRITE compares every
  value (spot rows at 1M); EDIT checks every cell equals the source except the
  edited ones (value losslessness incl. date styles).

Tasks and the kyrax API used (the fastest idiomatic path a user would pick):

| task | kyrax API | competitors |
|---|---|---|
| import | `import kyrax` | python_calamine, openpyxl, xlsxwriter, pandas |
| read_values | `load_workbook(p, read_only=True).active.iter_rows(values_only=True)` | calamine `to_python()`, openpyxl `read_only` iter_rows |
| read_df | `read_excel(p).load_sheet(0).to_pandas()` | `pd.read_excel` engine calamine / openpyxl |
| write | `write_excel_turbo(p, [{"name", "rows"}])`, `write_excel_turbo_stream` | XlsxWriter normal / constant_memory, openpyxl normal / write_only |
| edit | `load_workbook(p)` + `ws.cell(...)` x2000 + `save` | openpyxl `load_workbook` + same edits + save |

`(data only)` in WRITE is the floor (rows built, nothing written); it is not a
competitor.

## Gate rule (`compare.py`)

A kyrax row regresses when its new median exceeds the baseline max **and** is
worse than the baseline median by more than max(baseline noise %, 5 %), for
CPU-s or RSS, and by more than 10 ms CPU / 2 MB RSS absolute. Any verify
failure also fails the gate.

For an "after" table without re-measuring the (unchanged) competitors:
`$BENCH_PYTHON bench/run.py --reuse bench/results/baseline.json --out after`
(kyrax rows re-measured at all sizes, ~5 min; reused rows are labelled).

Final before/after table: `bench/final.py BASELINE AFTER --rerun PRECHANGE [COMPETITOR...]`
joins measured result files into `bench/results/final.{json,md}` (no new numbers).

## Read correctness and generator inputs (October 2026 sprint)

Fresh sprint runs validate every READ cell after the timed section. Validation
uses the deterministic source generator; booleans must be booleans, date cells
must be Python dates/datetimes (midnight dates are equivalent), strings must
remain strings, and numeric cells must have equal numeric values. Integral
floats are accepted because python-calamine exposes Excel numbers as floats;
observed column type names are recorded as `read_types` in JSON. DataFrame
reads check column names and every value using the same rules. The worker
retains the result through the end of timing, then snapshots peak RSS before
validation. These timings should be compared only with runs of this harness
version. Historical READ rows marked `n/a` were not checked by this validator.
A known type mismatch is recorded as `FAIL`; timings remain evidence of that
workload but must not support a claim of equivalent semantics.

The original WRITE matrix constructs a full input list before timing. Its RSS
therefore includes the complete input, and the name `kyrax-stream` does not by
itself establish bounded memory. To measure a producer/consumer workflow:

```bash
$BENCH_PYTHON bench/run.py --tasks write_stream --sizes small medium --out sprint_generator
```

This opt-in task feeds the same deterministic generator to Kyrax's stream API,
XlsxWriter `constant_memory`, and openpyxl `write_only`. Generation CPU is inside
the timed section for every library; no full input list is retained. A data-only
row measures generator consumption as a floor. Output verification is identical
to WRITE. Run large sizes only after checking available memory: an API accepting
an iterator can still materialize it internally.
