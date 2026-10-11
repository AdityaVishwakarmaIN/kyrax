# kyrax benchmark scoreboard (full)

- Date: 2026-10-11 12:28 UTC · commit `2067acf` · Python 3.13.16 · 4 CPUs · Linux-6.18.44-fc-v114-x86_64-with-glibc2.39
- Versions: openpyxl 3.1.5, XlsxWriter 3.2.9, python-calamine 0.8.3, pandas 3.0.6, pyarrow 23.0.0, numpy 2.5.4
- Harness runtime: 22.0 s
- All rows MEASURED on this machine with this harness (no estimates).
- `CPU-s` = user+sys of the timed section (library import + task, all threads), median.
  `noise` = (max-min)/median over the repeats. `RSS` = worker peak RSS (VmHWM).
  `ratio` = kyrax median / best competitor median (>1 = kyrax worse).

| task | size | lib | CPU-s | CPU noise | proc CPU-s | peak RSS MB | RSS min-max | wall-s | n | ratio CPU (vs) | ratio RSS (vs) | verify | status |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|---|---|
| import | small (1,000) | calamine | 0.001 | 39% | 0.031 | 13 | 12-13 | 0.001 | 5 |  |  | n/a | MEASURED |
| read_values | small (1,000) | calamine | 0.007 | 15% | 0.040 | 13 | 13-14 | 0.008 | 5 |  |  | n/a | MEASURED |
| read_values | medium (100,000) | calamine | 0.622 | 1% | 0.657 | 84 | 84-84 | 0.623 | 3 |  |  | n/a | MEASURED |
| read_values | large (1,000,000) | calamine | 6.337 | 9% | 6.383 | 728 | 728-728 | 6.397 | 3 |  |  | n/a | MEASURED |
