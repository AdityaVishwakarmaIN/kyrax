# kyrax benchmark scoreboard (full)

- Date: 2026-10-11 12:29 UTC · commit `2067acf` · Python 3.13.16 · 4 CPUs · Linux-6.18.44-fc-v114-x86_64-with-glibc2.39
- Versions: openpyxl 3.1.5, XlsxWriter 3.2.9, python-calamine 0.8.3, pandas 3.0.6, pyarrow 23.0.0, numpy 2.5.4
- Harness runtime: 48.5 s
- All rows MEASURED on this machine with this harness (no estimates).
- `CPU-s` = user+sys of the timed section (library import + task, all threads), median.
  `noise` = (max-min)/median over the repeats. `RSS` = worker peak RSS (VmHWM).
  `ratio` = kyrax median / best competitor median (>1 = kyrax worse).

| task | size | lib | CPU-s | CPU noise | proc CPU-s | peak RSS MB | RSS min-max | wall-s | n | ratio CPU (vs) | ratio RSS (vs) | verify | status |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|---|---|
| read_df | small (1,000) | pandas-calamine | 0.659 | 6% | 0.770 | 100 | 100-101 | 0.386 | 5 |  |  | n/a | MEASURED |
| read_df | medium (100,000) | pandas-calamine | 1.765 | 4% | 1.960 | 222 | 221-222 | 1.509 | 3 |  |  | n/a | MEASURED |
| read_df | large (1,000,000) | pandas-calamine | 13.074 | 5% | 13.769 | 1313 | 1313-1313 | 12.909 | 3 |  |  | n/a | MEASURED |
