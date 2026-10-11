# kyrax benchmark scoreboard (full)

- Date: 2026-10-11 12:30 UTC · commit `2067acf` · Python 3.13.16 · 4 CPUs · Linux-6.18.44-fc-v114-x86_64-with-glibc2.39
- Versions: openpyxl 3.1.5, XlsxWriter 3.2.9, python-calamine 0.8.3, pandas 3.0.6, pyarrow 23.0.0, numpy 2.5.4
- Harness runtime: 57.4 s
- All rows MEASURED on this machine with this harness (no estimates).
- `CPU-s` = user+sys of the timed section (library import + task, all threads), median.
  `noise` = (max-min)/median over the repeats. `RSS` = worker peak RSS (VmHWM).
  `ratio` = kyrax median / best competitor median (>1 = kyrax worse).

| task | size | lib | CPU-s | CPU noise | proc CPU-s | peak RSS MB | RSS min-max | wall-s | n | ratio CPU (vs) | ratio RSS (vs) | verify | status |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|---|---|
| edit | small (1,000) | openpyxl | 0.613 | 12% | 0.683 | 42 | 42-42 | 0.330 | 5 |  |  | ok (all values lossless) | MEASURED |
| edit | medium (100,000) | openpyxl | 16.949 | 2% | 17.868 | 442 | 442-442 | 16.890 | 3 |  |  | ok (all values lossless) | MEASURED |
