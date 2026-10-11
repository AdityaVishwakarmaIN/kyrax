# kyrax benchmark scoreboard (full)

- Date: 2026-10-11 08:52 UTC · commit `ea9b33a` · Python 3.13.16 · 4 CPUs · Linux-6.18.44-fc-v114-x86_64-with-glibc2.39
- Versions: openpyxl 3.1.5, XlsxWriter 3.2.9, python-calamine 0.8.3, pandas 3.0.6, pyarrow 23.0.0, numpy 2.5.4
- Harness runtime: 828.5 s
- All rows MEASURED on this machine with this harness (no estimates).
- `CPU-s` = user+sys of the timed section (library import + task, all threads), median.
  `noise` = (max-min)/median over the repeats. `RSS` = worker peak RSS (VmHWM).
  `ratio` = kyrax median / best competitor median (>1 = kyrax worse).

| task | size | lib | CPU-s | CPU noise | proc CPU-s | peak RSS MB | RSS min-max | wall-s | n | ratio CPU (vs) | ratio RSS (vs) | verify | status |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|---|---|
| import | small (1,000) | **kyrax** | 0.030 | 14% | 0.069 | 20 | 20-20 | 0.030 | 5 | 29.41 (calamine) | 1.61 (calamine) | n/a | MEASURED |
| import | small (1,000) | calamine | 0.001 | 19% | 0.031 | 12 | 12-13 | 0.001 | 5 |  |  | n/a | MEASURED |
| import | small (1,000) | openpyxl | 0.439 | 11% | 0.499 | 40 | 40-40 | 0.166 | 5 |  |  | n/a | MEASURED |
| import | small (1,000) | xlsxwriter | 0.039 | 12% | 0.073 | 20 | 20-20 | 0.039 | 5 |  |  | n/a | MEASURED |
| import | small (1,000) | pandas | 0.634 | 6% | 0.776 | 100 | 100-100 | 0.356 | 5 |  |  | n/a | MEASURED |
| read_values | small (1,000) | **kyrax** | 0.360 | 22% | 0.481 | 67 | 67-67 | 0.139 | 5 | 48.72 (calamine) | 4.97 (calamine) | n/a | MEASURED |
| read_values | small (1,000) | calamine | 0.007 | 46% | 0.041 | 13 | 13-14 | 0.007 | 5 |  |  | n/a | MEASURED |
| read_values | small (1,000) | openpyxl-ro | 0.489 | 6% | 0.551 | 41 | 41-41 | 0.209 | 5 |  |  | n/a | MEASURED |
| read_values | medium (100,000) | **kyrax** | 1.916 | 3% | 1.974 | 125 | 125-125 | 1.641 | 3 | 3.01 (calamine) | 1.49 (openpyxl-ro) | n/a | MEASURED |
| read_values | medium (100,000) | calamine | 0.636 | 2% | 0.671 | 84 | 84-84 | 0.644 | 3 |  |  | n/a | MEASURED |
| read_values | medium (100,000) | openpyxl-ro | 4.577 | 2% | 4.659 | 84 | 83-84 | 4.338 | 3 |  |  | n/a | MEASURED |
| read_values | large (1,000,000) | **kyrax** | 16.071 | 3% | 16.141 | 520 | 520-520 | 15.938 | 3 | 2.45 (calamine) | 0.71 (calamine) | n/a | MEASURED |
| read_values | large (1,000,000) | calamine | 6.550 | 4% | 6.588 | 728 | 728-728 | 6.592 | 3 |  |  | n/a | MEASURED |
| read_df | small (1,000) | **kyrax** | 0.656 | 8% | 0.791 | 112 | 112-112 | 0.375 | 5 | 0.99 (pandas-calamine) | 1.06 (pandas-calamine) | n/a | MEASURED |
| read_df | small (1,000) | pandas-calamine | 0.661 | 9% | 0.821 | 106 | 106-106 | 0.379 | 5 |  |  | n/a | MEASURED |
| read_df | small (1,000) | pandas-openpyxl | 0.779 | 8% | 0.938 | 111 | 111-111 | 0.511 | 5 |  |  | n/a | MEASURED |
| read_df | medium (100,000) | **kyrax** | 1.165 | 3% | 1.298 | 154 | 154-154 | 0.888 | 3 | 0.69 (pandas-calamine) | 0.89 (pandas-openpyxl) | n/a | MEASURED |
| read_df | medium (100,000) | pandas-calamine | 1.700 | 7% | 1.913 | 227 | 227-227 | 1.426 | 3 |  |  | n/a | MEASURED |
| read_df | medium (100,000) | pandas-openpyxl | 5.986 | 6% | 6.251 | 174 | 173-174 | 5.753 | 3 |  |  | n/a | MEASURED |
| read_df | large (1,000,000) | **kyrax** | 5.621 | 1% | 5.782 | 619 | 619-619 | 5.374 | 3 | 0.42 (pandas-calamine) | 0.47 (pandas-calamine) | n/a | MEASURED |
| read_df | large (1,000,000) | pandas-calamine | 13.236 | 4% | 13.945 | 1319 | 1319-1319 | 13.061 | 3 |  |  | n/a | MEASURED |
| write | small (1,000) | (data only) | 0.000 | 22% | 0.035 | 12 | 12-12 | 0.000 | 5 |  |  | n/a | MEASURED |
| write | small (1,000) | **kyrax** | 0.041 | 44% | 0.082 | 27 | 27-27 | 0.041 | 5 | 0.44 (xlsxwriter) | 1.30 (xlsxwriter-cm) | ok (reopens, all values) | MEASURED |
| write | small (1,000) | **kyrax-stream** | 0.045 | 46% | 0.089 | 27 | 27-27 | 0.046 | 5 | 0.49 (xlsxwriter) | 1.30 (xlsxwriter-cm) | ok (reopens, all values) | MEASURED |
| write | small (1,000) | xlsxwriter | 0.092 | 15% | 0.140 | 22 | 22-22 | 0.097 | 5 |  |  | ok (reopens, all values) | MEASURED |
| write | small (1,000) | xlsxwriter-cm | 0.097 | 13% | 0.140 | 21 | 21-21 | 0.100 | 5 |  |  | ok (reopens, all values) | MEASURED |
| write | small (1,000) | openpyxl | 0.538 | 7% | 0.613 | 44 | 44-44 | 0.257 | 5 |  |  | ok (reopens, all values) | MEASURED |
| write | small (1,000) | openpyxl-wo | 0.553 | 19% | 0.620 | 41 | 41-41 | 0.273 | 5 |  |  | ok (reopens, all values) | MEASURED |
| write | medium (100,000) | (data only) | 0.000 | 28% | 0.442 | 47 | 47-47 | 0.000 | 3 |  |  | n/a | MEASURED |
| write | medium (100,000) | **kyrax** | 1.076 | 6% | 1.528 | 404 | 404-404 | 1.089 | 3 | 0.22 (xlsxwriter) | 7.25 (xlsxwriter-cm) | ok (reopens, all values) | MEASURED |
| write | medium (100,000) | **kyrax-stream** | 1.060 | 4% | 1.510 | 400 | 399-400 | 1.061 | 3 | 0.22 (xlsxwriter) | 7.17 (xlsxwriter-cm) | ok (reopens, all values) | MEASURED |
| write | medium (100,000) | xlsxwriter | 4.910 | 1% | 5.579 | 157 | 157-157 | 4.998 | 3 |  |  | ok (reopens, all values) | MEASURED |
| write | medium (100,000) | xlsxwriter-cm | 5.370 | 2% | 5.786 | 56 | 56-56 | 5.486 | 3 |  |  | ok (reopens, all values) | MEASURED |
| write | medium (100,000) | openpyxl | 11.567 | 3% | 12.414 | 343 | 343-343 | 11.422 | 3 |  |  | ok (reopens, all values) | MEASURED |
| write | medium (100,000) | openpyxl-wo | 11.084 | 6% | 11.540 | 76 | 76-76 | 10.948 | 3 |  |  | ok (reopens, all values) | MEASURED |
| write | large (1,000,000) | (data only) | 0.000 | 20% | 4.341 | 364 | 364-364 | 0.000 | 3 |  |  | n/a | MEASURED |
| write | large (1,000,000) | **kyrax** | 26.090 | 5% | 30.492 | 3807 | 3807-3807 | 26.368 | 3 | 0.48 (xlsxwriter-cm) | 10.22 (xlsxwriter-cm) | ok (reopens, spot rows) | MEASURED |
| write | large (1,000,000) | **kyrax-stream** | 23.874 | 15% | 28.429 | 3833 | 3832-3833 | 24.094 | 3 | 0.44 (xlsxwriter-cm) | 10.28 (xlsxwriter-cm) | ok (reopens, spot rows) | MEASURED |
| write | large (1,000,000) | xlsxwriter-cm | 54.348 | 1% | 58.864 | 373 | 373-373 | 55.398 | 3 |  |  | ok (reopens, spot rows) | MEASURED |
| edit | small (1,000) | **kyrax** | 0.044 | 28% | 0.084 | 25 | 25-26 | 0.045 | 5 | 0.07 (openpyxl) | 0.56 (openpyxl) | ok (all values lossless) | MEASURED |
| edit | small (1,000) | openpyxl | 0.610 | 9% | 0.680 | 45 | 45-45 | 0.333 | 5 |  |  | ok (all values lossless) | MEASURED |
| edit | medium (100,000) | **kyrax** | 0.727 | 8% | 0.777 | 114 | 114-114 | 0.735 | 3 | 0.04 (openpyxl) | 0.26 (openpyxl) | ok (all values lossless) | MEASURED |
| edit | medium (100,000) | openpyxl | 17.343 | 2% | 18.252 | 445 | 445-445 | 17.247 | 3 |  |  | ok (all values lossless) | MEASURED |
| edit | large (1,000,000) | **kyrax** | 6.702 | 6% | 6.768 | 959 | 959-959 | 6.782 | 3 |  |  | ok (all values lossless) | MEASURED |

## Value semantics caveat (read_values)

The read_values libraries do not return identical Python values for the same file (fixture
row 2 shown): kyrax read_only `iter_rows` -> `(1.0, 4918.52, 'cat-31', 'item-1384', 44973.0,
'TRUE', 366.0, 47.994)` (dates as serial floats, booleans as "TRUE"/"FALSE" strings, ints as
floats); python-calamine -> `[1.0, ..., datetime.date(2023, 2, 16), True, 366.0, ...]`;
openpyxl -> `(1, ..., datetime.datetime(2023, 2, 16, 0, 0), True, 366, ...)`. kyrax therefore
does less conversion work in this row; any "faster than" claim on read_values must carry this
caveat.

## Test-suite baseline (commit ea9b33a, before any optimization)

| suite | command | result |
|---|---|---|
| Rust | `cargo test --release --no-fail-fast` | 1307 passed, **10 failed** (pre-existing), 1 ignored |
| Python | `uv run pytest` (default config, `python/tests`) | 484 passed, 1 skipped, 0 failed |

The 10 Rust failures are pre-existing and environmental: 8 `turbo::features::stress::*`,
`turbo::write::writer::tests::auto_filter_roundtrip_survives` and
`turbo::perfgate::perfgate_e4_lazy_hydration` all read `<repo>/testdata/`, which is
not in the repository. Gate rule: no failures beyond these 10.

## Harness runtime

- full (`bench/run.py --out baseline`): 828 s (all libs, 3 sizes)
- `--quick` (kyrax rows, small+medium): ~30 s
- `--reuse bench/results/baseline.json` (kyrax rows, all sizes): ~5 min (est. from kyrax row times)
