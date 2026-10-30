# kyrax benchmark scoreboard (full)

- Date: 2026-10-29 04:10 UTC · commit `fe07016` · Python 3.12.14 · 3 CPUs · Linux-6.18.55-x86_64-with-glibc2.41
- Versions: openpyxl 3.1.5, XlsxWriter 3.2.9, python-calamine 0.9.0, pandas 3.1.0, pyarrow 26.0.0, numpy 2.5.4
- Harness runtime: 35.7 s
- All rows MEASURED on this machine with this harness (no estimates).
- `CPU-s` = user+sys of the timed section (library import + task, all threads), median.
  `noise` = (max-min)/median over the repeats. `RSS` = worker peak RSS (VmHWM).
  `ratio` = kyrax median / best competitor median (>1 = kyrax worse).

| task | size | lib | CPU-s | CPU noise | proc CPU-s | peak RSS MB | RSS min-max | wall-s | n | ratio CPU (vs) | ratio RSS (vs) | verify | status |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|---|---|
| read_values | small (1,000) | calamine | 0.007 | 9% | 0.036 | 11 | 11-11 | 0.007 | 3 |  |  | ok (all values; bool/date/numeric types) | MEASURED |
| read_values | medium (100,000) | calamine | 0.499 | 3% | 1.083 | 82 | 82-82 | 0.499 | 3 |  |  | ok (all values; bool/date/numeric types) | MEASURED |
| read_values | large (1,000,000) | calamine | 4.930 | 13% | 11.040 | 725 | 725-726 | 4.932 | 3 |  |  | ok (all values; bool/date/numeric types) | MEASURED |
