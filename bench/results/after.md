# kyrax benchmark scoreboard (full (competitors reused))

- Date: 2026-10-11 12:28 UTC · commit `2067acf` · Python 3.13.16 · 4 CPUs · Linux-6.18.44-fc-v114-x86_64-with-glibc2.39
- Versions: openpyxl 3.1.5, XlsxWriter 3.2.9, python-calamine 0.8.3, pandas 3.0.6, pyarrow 23.0.0, numpy 2.5.4
- Harness runtime: 180.2 s
- All rows MEASURED on this machine with this harness (no estimates).
- `CPU-s` = user+sys of the timed section (library import + task, all threads), median.
  `noise` = (max-min)/median over the repeats. `RSS` = worker peak RSS (VmHWM).
  `ratio` = kyrax median / best competitor median (>1 = kyrax worse).

| task | size | lib | CPU-s | CPU noise | proc CPU-s | peak RSS MB | RSS min-max | wall-s | n | ratio CPU (vs) | ratio RSS (vs) | verify | status |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|---|---|
| import | small (1,000) | calamine | 0.001 | 19% | 0.031 | 12 | 12-13 | 0.001 | 5 |  |  | n/a | MEASURED (reused from baseline.json) |
| import | small (1,000) | openpyxl | 0.439 | 11% | 0.499 | 40 | 40-40 | 0.166 | 5 |  |  | n/a | MEASURED (reused from baseline.json) |
| import | small (1,000) | xlsxwriter | 0.039 | 12% | 0.073 | 20 | 20-20 | 0.039 | 5 |  |  | n/a | MEASURED (reused from baseline.json) |
| import | small (1,000) | pandas | 0.634 | 6% | 0.776 | 100 | 100-100 | 0.356 | 5 |  |  | n/a | MEASURED (reused from baseline.json) |
| import | small (1,000) | **kyrax** | 0.015 | 37% | 0.054 | 18 | 18-18 | 0.018 | 5 | 15.30 (calamine) | 1.45 (calamine) | n/a | MEASURED |
| read_values | small (1,000) | calamine | 0.007 | 46% | 0.041 | 13 | 13-14 | 0.007 | 5 |  |  | n/a | MEASURED (reused from baseline.json) |
| read_values | small (1,000) | openpyxl-ro | 0.489 | 6% | 0.551 | 41 | 41-41 | 0.209 | 5 |  |  | n/a | MEASURED (reused from baseline.json) |
| read_values | small (1,000) | **kyrax** | 0.027 | 33% | 0.071 | 23 | 23-23 | 0.030 | 5 | 3.66 (calamine) | 1.69 (calamine) | n/a | MEASURED |
| read_values | medium (100,000) | calamine | 0.636 | 2% | 0.671 | 84 | 84-84 | 0.644 | 3 |  |  | n/a | MEASURED (reused from baseline.json) |
| read_values | medium (100,000) | openpyxl-ro | 4.577 | 2% | 4.659 | 84 | 83-84 | 4.338 | 3 |  |  | n/a | MEASURED (reused from baseline.json) |
| read_values | medium (100,000) | **kyrax** | 0.754 | 7% | 0.798 | 76 | 76-76 | 0.767 | 3 | 1.19 (calamine) | 0.90 (openpyxl-ro) | n/a | MEASURED |
| read_values | large (1,000,000) | calamine | 6.550 | 4% | 6.588 | 728 | 728-728 | 6.592 | 3 |  |  | n/a | MEASURED (reused from baseline.json) |
| read_values | large (1,000,000) | **kyrax** | 6.544 | 5% | 6.579 | 430 | 430-430 | 6.598 | 3 | 1.00 (calamine) | 0.59 (calamine) | n/a | MEASURED |
| read_df | small (1,000) | pandas-calamine | 0.661 | 9% | 0.821 | 106 | 106-106 | 0.379 | 5 |  |  | n/a | MEASURED (reused from baseline.json) |
| read_df | small (1,000) | pandas-openpyxl | 0.779 | 8% | 0.938 | 111 | 111-111 | 0.511 | 5 |  |  | n/a | MEASURED (reused from baseline.json) |
| read_df | small (1,000) | **kyrax** | 0.655 | 10% | 0.771 | 106 | 106-107 | 0.372 | 5 | 0.99 (pandas-calamine) | 1.00 (pandas-calamine) | n/a | MEASURED |
| read_df | medium (100,000) | pandas-calamine | 1.700 | 7% | 1.913 | 227 | 227-227 | 1.426 | 3 |  |  | n/a | MEASURED (reused from baseline.json) |
| read_df | medium (100,000) | pandas-openpyxl | 5.986 | 6% | 6.251 | 174 | 173-174 | 5.753 | 3 |  |  | n/a | MEASURED (reused from baseline.json) |
| read_df | medium (100,000) | **kyrax** | 1.148 | 7% | 1.279 | 148 | 148-148 | 0.866 | 3 | 0.68 (pandas-calamine) | 0.85 (pandas-openpyxl) | n/a | MEASURED |
| read_df | large (1,000,000) | pandas-calamine | 13.236 | 4% | 13.945 | 1319 | 1319-1319 | 13.061 | 3 |  |  | n/a | MEASURED (reused from baseline.json) |
| read_df | large (1,000,000) | **kyrax** | 6.169 | 3% | 6.307 | 616 | 616-617 | 5.927 | 3 | 0.47 (pandas-calamine) | 0.47 (pandas-calamine) | n/a | MEASURED |
| write | small (1,000) | (data only) | 0.000 | 22% | 0.035 | 12 | 12-12 | 0.000 | 5 |  |  | n/a | MEASURED (reused from baseline.json) |
| write | small (1,000) | xlsxwriter | 0.092 | 15% | 0.140 | 22 | 22-22 | 0.097 | 5 |  |  | ok (reopens, all values) | MEASURED (reused from baseline.json) |
| write | small (1,000) | xlsxwriter-cm | 0.097 | 13% | 0.140 | 21 | 21-21 | 0.100 | 5 |  |  | ok (reopens, all values) | MEASURED (reused from baseline.json) |
| write | small (1,000) | openpyxl | 0.538 | 7% | 0.613 | 44 | 44-44 | 0.257 | 5 |  |  | ok (reopens, all values) | MEASURED (reused from baseline.json) |
| write | small (1,000) | openpyxl-wo | 0.553 | 19% | 0.620 | 41 | 41-41 | 0.273 | 5 |  |  | ok (reopens, all values) | MEASURED (reused from baseline.json) |
| write | small (1,000) | **kyrax** | 0.020 | 41% | 0.062 | 22 | 22-22 | 0.021 | 5 | 0.21 (xlsxwriter) | 1.04 (xlsxwriter-cm) | ok (reopens, all values) | MEASURED |
| write | small (1,000) | **kyrax-stream** | 0.019 | 10% | 0.056 | 22 | 22-22 | 0.020 | 5 | 0.21 (xlsxwriter) | 1.05 (xlsxwriter-cm) | ok (reopens, all values) | MEASURED |
| write | medium (100,000) | (data only) | 0.000 | 28% | 0.442 | 47 | 47-47 | 0.000 | 3 |  |  | n/a | MEASURED (reused from baseline.json) |
| write | medium (100,000) | xlsxwriter | 4.910 | 1% | 5.579 | 157 | 157-157 | 4.998 | 3 |  |  | ok (reopens, all values) | MEASURED (reused from baseline.json) |
| write | medium (100,000) | xlsxwriter-cm | 5.370 | 2% | 5.786 | 56 | 56-56 | 5.486 | 3 |  |  | ok (reopens, all values) | MEASURED (reused from baseline.json) |
| write | medium (100,000) | openpyxl | 11.567 | 3% | 12.414 | 343 | 343-343 | 11.422 | 3 |  |  | ok (reopens, all values) | MEASURED (reused from baseline.json) |
| write | medium (100,000) | openpyxl-wo | 11.084 | 6% | 11.540 | 76 | 76-76 | 10.948 | 3 |  |  | ok (reopens, all values) | MEASURED (reused from baseline.json) |
| write | medium (100,000) | **kyrax** | 0.529 | 7% | 0.942 | 127 | 127-127 | 0.533 | 3 | 0.11 (xlsxwriter) | 2.27 (xlsxwriter-cm) | ok (reopens, all values) | MEASURED |
| write | medium (100,000) | **kyrax-stream** | 0.542 | 6% | 0.992 | 127 | 127-127 | 0.548 | 3 | 0.11 (xlsxwriter) | 2.27 (xlsxwriter-cm) | ok (reopens, all values) | MEASURED |
| write | large (1,000,000) | (data only) | 0.000 | 20% | 4.341 | 364 | 364-364 | 0.000 | 3 |  |  | n/a | MEASURED (reused from baseline.json) |
| write | large (1,000,000) | xlsxwriter-cm | 54.348 | 1% | 58.864 | 373 | 373-373 | 55.398 | 3 |  |  | ok (reopens, spot rows) | MEASURED (reused from baseline.json) |
| write | large (1,000,000) | **kyrax** | 5.422 | 12% | 10.059 | 1104 | 1104-1104 | 5.507 | 3 | 0.10 (xlsxwriter-cm) | 2.96 (xlsxwriter-cm) | ok (reopens, spot rows) | MEASURED |
| write | large (1,000,000) | **kyrax-stream** | 5.653 | 4% | 9.803 | 1104 | 1104-1104 | 5.720 | 3 | 0.10 (xlsxwriter-cm) | 2.96 (xlsxwriter-cm) | ok (reopens, spot rows) | MEASURED |
| edit | small (1,000) | openpyxl | 0.610 | 9% | 0.680 | 45 | 45-45 | 0.333 | 5 |  |  | ok (all values lossless) | MEASURED (reused from baseline.json) |
| edit | small (1,000) | **kyrax** | 0.023 | 53% | 0.061 | 23 | 23-24 | 0.024 | 5 | 0.04 (openpyxl) | 0.52 (openpyxl) | ok (all values lossless) | MEASURED |
| edit | medium (100,000) | openpyxl | 17.343 | 2% | 18.252 | 445 | 445-445 | 17.247 | 3 |  |  | ok (all values lossless) | MEASURED (reused from baseline.json) |
| edit | medium (100,000) | **kyrax** | 0.732 | 27% | 0.786 | 112 | 112-112 | 0.754 | 3 | 0.04 (openpyxl) | 0.25 (openpyxl) | ok (all values lossless) | MEASURED |
| edit | large (1,000,000) | **kyrax** | 6.778 | 10% | 6.844 | 957 | 957-957 | 6.903 | 3 |  |  | ok (all values lossless) | MEASURED |
