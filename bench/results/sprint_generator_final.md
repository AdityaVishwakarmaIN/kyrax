# kyrax benchmark scoreboard (full)

- Date: 2026-10-30 05:04 UTC · commit `f13a5c8` · Python 3.12.14 · 3 CPUs · Linux-6.18.57-x86_64-with-glibc2.41
- Versions: openpyxl 3.1.5, XlsxWriter 3.2.9, python-calamine 0.9.0, pandas 3.1.0, pyarrow 26.0.0, numpy 2.5.4
- Harness runtime: 27.8 s
- All rows MEASURED on this machine with this harness (no estimates).
- `CPU-s` = user+sys of the timed section (library import + task, all threads), median.
  `noise` = (max-min)/median over the repeats. `RSS` = worker peak RSS (VmHWM).
  `ratio` = kyrax median / best competitor median (>1 = kyrax worse).

| task | size | lib | CPU-s | CPU noise | proc CPU-s | peak RSS MB | RSS min-max | wall-s | output bytes | n | ratio CPU (vs) | ratio RSS (vs) | verify | status |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---|---|---|---|
| write_stream | small (1,000) | **kyrax-stream-inline** | 0.018 | 64% | 0.051 | 19 | 18-19 | 0.018 | 74,846 | 3 |  |  | ok (reopens, all values) | MEASURED |
| write_stream | medium (100,000) | **kyrax-stream-inline** | 0.559 | 8% | 0.585 | 19 | 19-19 | 0.559 | 7,043,340 | 3 |  |  | ok (reopens, all values) | MEASURED |
| write_stream | large (1,000,000) | **kyrax-stream-inline** | 5.424 | 3% | 5.449 | 19 | 18-19 | 5.431 | 71,277,519 | 3 |  |  | ok (reopens, spot rows) | MEASURED |
