# kyrax benchmark scoreboard (full)

- Date: 2026-10-11 12:29 UTC · commit `2067acf` · Python 3.13.16 · 4 CPUs · Linux-6.18.44-fc-v114-x86_64-with-glibc2.39
- Versions: openpyxl 3.1.5, XlsxWriter 3.2.9, python-calamine 0.8.3, pandas 3.0.6, pyarrow 23.0.0, numpy 2.5.4
- Harness runtime: 40.9 s
- All rows MEASURED on this machine with this harness (no estimates).
- `CPU-s` = user+sys of the timed section (library import + task, all threads), median.
  `noise` = (max-min)/median over the repeats. `RSS` = worker peak RSS (VmHWM).
  `ratio` = kyrax median / best competitor median (>1 = kyrax worse).

| task | size | lib | CPU-s | CPU noise | proc CPU-s | peak RSS MB | RSS min-max | wall-s | n | ratio CPU (vs) | ratio RSS (vs) | verify | status |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|---|---|
| write | small (1,000) | xlsxwriter | 0.092 | 19% | 0.137 | 22 | 22-22 | 0.098 | 5 |  |  | ok (reopens, all values) | MEASURED |
| write | small (1,000) | xlsxwriter-cm | 0.108 | 49% | 0.155 | 21 | 21-21 | 0.111 | 5 |  |  | ok (reopens, all values) | MEASURED |
| write | medium (100,000) | xlsxwriter | 5.122 | 14% | 5.741 | 157 | 157-157 | 5.205 | 3 |  |  | ok (reopens, all values) | MEASURED |
| write | medium (100,000) | xlsxwriter-cm | 5.585 | 4% | 6.015 | 56 | 56-56 | 5.699 | 3 |  |  | ok (reopens, all values) | MEASURED |
