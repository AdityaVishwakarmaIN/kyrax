# kyrax benchmark scoreboard (full (competitors reused))

- Date: 2026-10-29 04:09 UTC · commit `fe07016` · Python 3.12.14 · 3 CPUs · Linux-6.18.55-x86_64-with-glibc2.41
- Versions: openpyxl 3.1.5, XlsxWriter 3.2.9, python-calamine 0.9.0, pandas 3.1.0, pyarrow 26.0.0, numpy 2.5.4
- Harness runtime: 161.0 s
- All rows MEASURED on this machine with this harness (no estimates).
- `CPU-s` = user+sys of the timed section (library import + task, all threads), median.
  `noise` = (max-min)/median over the repeats. `RSS` = worker peak RSS (VmHWM).
  `ratio` = kyrax median / best competitor median (>1 = kyrax worse).

| task | size | lib | CPU-s | CPU noise | proc CPU-s | peak RSS MB | RSS min-max | wall-s | n | ratio CPU (vs) | ratio RSS (vs) | verify | status |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|---|---|
| import | small (1,000) | calamine | 0.001 | 54% | 0.025 | 10 | 10-10 | 0.001 | 3 |  |  | n/a | MEASURED (reused from sprint_baseline.json) |
| import | small (1,000) | openpyxl | 0.295 | 5% | 0.334 | 34 | 34-34 | 0.110 | 3 |  |  | n/a | MEASURED (reused from sprint_baseline.json) |
| import | small (1,000) | xlsxwriter | 0.027 | 155% | 0.052 | 14 | 14-14 | 0.027 | 3 |  |  | n/a | MEASURED (reused from sprint_baseline.json) |
| import | small (1,000) | pandas | 0.465 | 4% | 0.560 | 112 | 112-112 | 0.316 | 3 |  |  | n/a | MEASURED (reused from sprint_baseline.json) |
| import | small (1,000) | **kyrax** | 0.010 | 7% | 0.031 | 16 | 16-16 | 0.010 | 3 | 11.92 (calamine) | 1.54 (calamine) | n/a | MEASURED |
| read_values | small (1,000) | calamine | 0.007 | 345% | 0.033 | 11 | 11-11 | 0.007 | 3 |  |  | ok (all values; bool/date/numeric types) | MEASURED (reused from sprint_baseline.json) |
| read_values | small (1,000) | openpyxl-ro | 0.344 | 4% | 0.391 | 35 | 35-35 | 0.138 | 3 |  |  | ok (all values; bool/date/numeric types) | MEASURED (reused from sprint_baseline.json) |
| read_values | small (1,000) | **kyrax** | 0.013 | 20% | 0.042 | 20 | 19-20 | 0.014 | 3 | 1.99 (calamine) | 1.74 (calamine) | ok (all values; bool/date/numeric types) | MEASURED |
| read_values | medium (100,000) | calamine | 0.493 | 13% | 1.068 | 82 | 82-82 | 0.494 | 3 |  |  | ok (all values; bool/date/numeric types) | MEASURED (reused from sprint_baseline.json) |
| read_values | medium (100,000) | openpyxl-ro | 3.685 | 7% | 4.342 | 78 | 77-78 | 3.512 | 3 |  |  | ok (all values; bool/date/numeric types) | MEASURED (reused from sprint_baseline.json) |
| read_values | medium (100,000) | **kyrax** | 0.361 | 8% | 0.906 | 58 | 57-58 | 0.361 | 3 | 0.73 (calamine) | 0.74 (openpyxl-ro) | ok (all values; bool/date/numeric types) | MEASURED |
| read_values | large (1,000,000) | calamine | 5.067 | 26% | 12.147 | 726 | 726-726 | 5.083 | 3 |  |  | ok (all values; bool/date/numeric types) | MEASURED (reused from sprint_baseline.json) |
| read_values | large (1,000,000) | **kyrax** | 3.429 | 6% | 8.401 | 375 | 375-375 | 3.435 | 3 | 0.68 (calamine) | 0.52 (calamine) | ok (all values; bool/date/numeric types) | MEASURED |
| read_df | small (1,000) | pandas-calamine | 0.562 | 24% | 0.718 | 122 | 122-122 | 0.410 | 3 |  |  | ok (all values; bool/date/numeric types) | MEASURED (reused from sprint_baseline.json) |
| read_df | small (1,000) | pandas-openpyxl | 0.667 | 18% | 0.877 | 126 | 126-126 | 0.547 | 3 |  |  | ok (all values; bool/date/numeric types) | MEASURED (reused from sprint_baseline.json) |
| read_df | small (1,000) | **kyrax** | 0.499 | 9% | 0.577 | 110 | 110-110 | 0.323 | 3 | 0.89 (pandas-calamine) | 0.90 (pandas-calamine) | ok (all values; bool/date/numeric types) | MEASURED |
| read_df | medium (100,000) | pandas-calamine | 1.422 | 18% | 2.532 | 250 | 249-250 | 1.293 | 3 |  |  | ok (all values; bool/date/numeric types) | MEASURED (reused from sprint_baseline.json) |
| read_df | medium (100,000) | pandas-openpyxl | 6.199 | 15% | 7.291 | 193 | 193-193 | 6.058 | 3 |  |  | ok (all values; bool/date/numeric types) | MEASURED (reused from sprint_baseline.json) |
| read_df | medium (100,000) | **kyrax** | 0.739 | 5% | 1.530 | 153 | 153-153 | 0.564 | 3 | 0.52 (pandas-calamine) | 0.79 (pandas-openpyxl) | ok (all values; bool/date/numeric types) | MEASURED |
| read_df | large (1,000,000) | pandas-calamine | 10.004 | 4% | 19.209 | 1344 | 1344-1344 | 9.830 | 3 |  |  | ok (all values; bool/date/numeric types) | MEASURED (reused from sprint_baseline.json) |
| read_df | large (1,000,000) | **kyrax** | 3.794 | 12% | 11.794 | 615 | 614-615 | 3.593 | 3 | 0.38 (pandas-calamine) | 0.46 (pandas-calamine) | ok (all values; bool/date/numeric types) | MEASURED |
| write | small (1,000) | (data only) | 0.000 | 14% | 0.034 | 9 | 9-9 | 0.000 | 3 |  |  | n/a | MEASURED (reused from sprint_baseline.json) |
| write | small (1,000) | xlsxwriter | 0.074 | 45% | 0.113 | 16 | 16-16 | 0.074 | 3 |  |  | ok (reopens, all values) | MEASURED (reused from sprint_baseline.json) |
| write | small (1,000) | xlsxwriter-cm | 0.080 | 34% | 0.115 | 15 | 15-15 | 0.080 | 3 |  |  | ok (reopens, all values) | MEASURED (reused from sprint_baseline.json) |
| write | small (1,000) | openpyxl | 0.382 | 11% | 0.435 | 37 | 37-38 | 0.209 | 3 |  |  | ok (reopens, all values) | MEASURED (reused from sprint_baseline.json) |
| write | small (1,000) | openpyxl-wo | 0.380 | 2% | 0.440 | 34 | 34-35 | 0.220 | 3 |  |  | ok (reopens, all values) | MEASURED (reused from sprint_baseline.json) |
| write | small (1,000) | **kyrax** | 0.013 | 11% | 0.040 | 19 | 19-20 | 0.013 | 3 | 0.18 (xlsxwriter) | 1.26 (xlsxwriter-cm) | ok (reopens, all values) | MEASURED |
| write | small (1,000) | **kyrax-stream** | 0.014 | 10% | 0.040 | 19 | 18-19 | 0.014 | 3 | 0.19 (xlsxwriter) | 1.26 (xlsxwriter-cm) | ok (reopens, all values) | MEASURED |
| write | medium (100,000) | (data only) | 0.000 | 7% | 0.343 | 44 | 44-44 | 0.000 | 3 |  |  | n/a | MEASURED (reused from sprint_baseline.json) |
| write | medium (100,000) | xlsxwriter | 4.596 | 14% | 5.112 | 151 | 151-151 | 4.603 | 3 |  |  | ok (reopens, all values) | MEASURED (reused from sprint_baseline.json) |
| write | medium (100,000) | xlsxwriter-cm | 4.249 | 17% | 4.603 | 50 | 50-50 | 4.250 | 3 |  |  | ok (reopens, all values) | MEASURED (reused from sprint_baseline.json) |
| write | medium (100,000) | openpyxl | 9.564 | 18% | 10.287 | 337 | 337-337 | 9.463 | 3 |  |  | ok (reopens, all values) | MEASURED (reused from sprint_baseline.json) |
| write | medium (100,000) | openpyxl-wo | 7.982 | 6% | 8.382 | 69 | 69-70 | 7.824 | 3 |  |  | ok (reopens, all values) | MEASURED (reused from sprint_baseline.json) |
| write | medium (100,000) | **kyrax** | 0.431 | 10% | 0.718 | 124 | 124-125 | 0.431 | 3 | 0.10 (xlsxwriter-cm) | 2.48 (xlsxwriter-cm) | ok (reopens, all values) | MEASURED |
| write | medium (100,000) | **kyrax-stream** | 0.448 | 11% | 0.747 | 124 | 124-124 | 0.448 | 3 | 0.11 (xlsxwriter-cm) | 2.48 (xlsxwriter-cm) | ok (reopens, all values) | MEASURED |
| write | large (1,000,000) | (data only) | 0.000 | 43% | 3.803 | 361 | 361-361 | 0.000 | 3 |  |  | n/a | MEASURED (reused from sprint_baseline.json) |
| write | large (1,000,000) | xlsxwriter-cm | 38.057 | 5% | 41.431 | 367 | 367-367 | 38.096 | 3 |  |  | ok (reopens, spot rows) | MEASURED (reused from sprint_baseline.json) |
| write | large (1,000,000) | **kyrax** | 4.156 | 4% | 7.334 | 1101 | 1101-1102 | 4.161 | 3 | 0.11 (xlsxwriter-cm) | 3.00 (xlsxwriter-cm) | ok (reopens, spot rows) | MEASURED |
| write | large (1,000,000) | **kyrax-stream** | 4.098 | 5% | 7.159 | 1102 | 1101-1102 | 4.098 | 3 | 0.11 (xlsxwriter-cm) | 3.00 (xlsxwriter-cm) | ok (reopens, spot rows) | MEASURED |
| edit | small (1,000) | openpyxl | 0.425 | 19% | 0.481 | 39 | 39-39 | 0.235 | 3 |  |  | ok (all values lossless) | MEASURED (reused from sprint_baseline.json) |
| edit | small (1,000) | **kyrax** | 0.018 | 9% | 0.043 | 21 | 20-21 | 0.018 | 3 | 0.04 (openpyxl) | 0.53 (openpyxl) | ok (all values lossless) | MEASURED |
| edit | medium (100,000) | openpyxl | 14.979 | 18% | 16.192 | 438 | 438-439 | 14.825 | 3 |  |  | ok (all values lossless) | MEASURED (reused from sprint_baseline.json) |
| edit | medium (100,000) | **kyrax** | 0.484 | 1% | 0.510 | 109 | 109-110 | 0.484 | 3 | 0.03 (openpyxl) | 0.25 (openpyxl) | ok (all values lossless) | MEASURED |
| edit | large (1,000,000) | **kyrax** | 4.762 | 3% | 4.812 | 954 | 954-954 | 4.763 | 3 |  |  | ok (all values lossless) | MEASURED |
