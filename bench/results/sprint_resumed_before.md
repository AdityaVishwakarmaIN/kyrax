# kyrax benchmark scoreboard (full)

- Date: 2026-10-29 04:05 UTC · commit `fc0fcb0` · Python 3.12.14 · 3 CPUs · Linux-6.18.55-x86_64-with-glibc2.41
- Versions: openpyxl 3.1.5, XlsxWriter 3.2.9, python-calamine 0.9.0, pandas 3.1.0, pyarrow 26.0.0, numpy 2.5.4
- Harness runtime: 5.0 s
- All rows MEASURED on this machine with this harness (no estimates).
- `CPU-s` = user+sys of the timed section (library import + task, all threads), median.
  `noise` = (max-min)/median over the repeats. `RSS` = worker peak RSS (VmHWM).
  `ratio` = kyrax median / best competitor median (>1 = kyrax worse).

| task | size | lib | CPU-s | CPU noise | proc CPU-s | peak RSS MB | RSS min-max | wall-s | n | ratio CPU (vs) | ratio RSS (vs) | verify | status |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|---|---|
| read_values | medium (100,000) | **kyrax** | 0.532 | 3% | 0.575 | 72 | 72-72 | 0.537 | 3 |  |  | FAIL row 1 col 4: 44973.0 (float) != datetime.datetime(2023, 2, 16, 0, 0) (datetime) | MEASURED |
| write_stream | medium (100,000) | **kyrax-stream** | 0.723 | 7% | 0.765 | 224 | 223-224 | 0.723 | 3 |  |  | ok (reopens, all values) | MEASURED |
