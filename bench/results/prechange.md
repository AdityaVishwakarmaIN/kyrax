# kyrax benchmark scoreboard (full)

- Date: 2026-10-11 12:25 UTC · commit `6db1236` · Python 3.13.16 · 4 CPUs · Linux-6.18.44-fc-v114-x86_64-with-glibc2.39
- Versions: openpyxl 3.1.5, XlsxWriter 3.2.9, python-calamine 0.8.3, pandas 3.0.6, pyarrow 23.0.0, numpy 2.5.4
- Harness runtime: 328.8 s
- All rows MEASURED on this machine with this harness (no estimates).
- `CPU-s` = user+sys of the timed section (library import + task, all threads), median.
  `noise` = (max-min)/median over the repeats. `RSS` = worker peak RSS (VmHWM).
  `ratio` = kyrax median / best competitor median (>1 = kyrax worse).

| task | size | lib | CPU-s | CPU noise | proc CPU-s | peak RSS MB | RSS min-max | wall-s | n | ratio CPU (vs) | ratio RSS (vs) | verify | status |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|---|---|
| import | small (1,000) | **kyrax** | 0.030 | 50% | 0.071 | 20 | 20-20 | 0.030 | 5 |  |  | n/a | MEASURED |
| read_values | small (1,000) | **kyrax** | 0.371 | 47% | 0.471 | 63 | 63-63 | 0.144 | 5 |  |  | n/a | MEASURED |
| read_values | medium (100,000) | **kyrax** | 2.032 | 5% | 2.091 | 121 | 121-125 | 1.765 | 3 |  |  | n/a | MEASURED |
| read_values | large (1,000,000) | **kyrax** | 16.003 | 5% | 16.067 | 517 | 516-517 | 15.889 | 3 |  |  | n/a | MEASURED |
| read_df | small (1,000) | **kyrax** | 0.637 | 6% | 0.770 | 107 | 106-107 | 0.364 | 5 |  |  | n/a | MEASURED |
| read_df | medium (100,000) | **kyrax** | 1.268 | 6% | 1.419 | 148 | 148-148 | 1.017 | 3 |  |  | n/a | MEASURED |
| read_df | large (1,000,000) | **kyrax** | 5.655 | 11% | 5.794 | 619 | 619-619 | 5.430 | 3 |  |  | n/a | MEASURED |
| write | small (1,000) | **kyrax** | 0.040 | 6% | 0.082 | 27 | 27-27 | 0.041 | 5 |  |  | ok (reopens, all values) | MEASURED |
| write | small (1,000) | **kyrax-stream** | 0.042 | 44% | 0.082 | 27 | 27-27 | 0.043 | 5 |  |  | ok (reopens, all values) | MEASURED |
| write | medium (100,000) | **kyrax** | 1.134 | 31% | 1.596 | 404 | 404-404 | 1.152 | 3 |  |  | ok (reopens, all values) | MEASURED |
| write | medium (100,000) | **kyrax-stream** | 1.340 | 6% | 2.028 | 400 | 400-400 | 1.356 | 3 |  |  | ok (reopens, all values) | MEASURED |
| write | large (1,000,000) | **kyrax** | 25.227 | 26% | 29.924 | 3807 | 3807-3807 | 25.508 | 3 |  |  | ok (reopens, spot rows) | MEASURED |
| write | large (1,000,000) | **kyrax-stream** | 23.950 | 42% | 29.043 | 3833 | 3833-3833 | 24.237 | 3 |  |  | ok (reopens, spot rows) | MEASURED |
| edit | small (1,000) | **kyrax** | 0.048 | 38% | 0.097 | 25 | 25-25 | 0.049 | 5 |  |  | ok (all values lossless) | MEASURED |
| edit | medium (100,000) | **kyrax** | 0.875 | 18% | 0.921 | 114 | 114-114 | 0.881 | 3 |  |  | ok (all values lossless) | MEASURED |
| edit | large (1,000,000) | **kyrax** | 7.250 | 11% | 7.332 | 959 | 959-959 | 7.421 | 3 |  |  | ok (all values lossless) | MEASURED |
