# kyrax benchmark scoreboard (full)

- Date: 2026-10-28 18:18 UTC · commit `2bd9754` · Python 3.12.14 · 3 CPUs · Linux-6.18.55-x86_64-with-glibc2.41
- Versions: openpyxl 3.1.5, XlsxWriter 3.2.9, python-calamine 0.9.0, pandas 3.1.0, pyarrow 26.0.0, numpy 2.5.4
- Harness runtime: 613.7 s
- All rows MEASURED on this machine with this harness (no estimates).
- `CPU-s` = user+sys of the timed section (library import + task, all threads), median.
  `noise` = (max-min)/median over the repeats. `RSS` = worker peak RSS (VmHWM).
  `ratio` = kyrax median / best competitor median (>1 = kyrax worse).

| task | size | lib | CPU-s | CPU noise | proc CPU-s | peak RSS MB | RSS min-max | wall-s | n | ratio CPU (vs) | ratio RSS (vs) | verify | status |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|---|---|
| import | small (1,000) | **kyrax** | 0.009 | 126% | 0.032 | 16 | 15-16 | 0.009 | 3 | 11.69 (calamine) | 1.49 (calamine) | n/a | MEASURED |
| import | small (1,000) | calamine | 0.001 | 54% | 0.025 | 10 | 10-10 | 0.001 | 3 |  |  | n/a | MEASURED |
| import | small (1,000) | openpyxl | 0.295 | 5% | 0.334 | 34 | 34-34 | 0.110 | 3 |  |  | n/a | MEASURED |
| import | small (1,000) | xlsxwriter | 0.027 | 155% | 0.052 | 14 | 14-14 | 0.027 | 3 |  |  | n/a | MEASURED |
| import | small (1,000) | pandas | 0.465 | 4% | 0.560 | 112 | 112-112 | 0.316 | 3 |  |  | n/a | MEASURED |
| read_values | small (1,000) | **kyrax** | 0.015 | 15% | 0.040 | 20 | 20-20 | 0.015 | 3 | 2.24 (calamine) | 1.79 (calamine) | FAIL row 1 col 4: 44973.0 (float) != datetime.datetime(2023, 2, 16, 0, 0) (datetime) | MEASURED |
| read_values | small (1,000) | calamine | 0.007 | 345% | 0.033 | 11 | 11-11 | 0.007 | 3 |  |  | ok (all values; bool/date/numeric types) | MEASURED |
| read_values | small (1,000) | openpyxl-ro | 0.344 | 4% | 0.391 | 35 | 35-35 | 0.138 | 3 |  |  | ok (all values; bool/date/numeric types) | MEASURED |
| read_values | medium (100,000) | **kyrax** | 0.555 | 22% | 0.594 | 73 | 72-73 | 0.556 | 3 | 1.13 (calamine) | 0.94 (openpyxl-ro) | FAIL row 1 col 4: 44973.0 (float) != datetime.datetime(2023, 2, 16, 0, 0) (datetime) | MEASURED |
| read_values | medium (100,000) | calamine | 0.493 | 13% | 1.068 | 82 | 82-82 | 0.494 | 3 |  |  | ok (all values; bool/date/numeric types) | MEASURED |
| read_values | medium (100,000) | openpyxl-ro | 3.685 | 7% | 4.342 | 78 | 77-78 | 3.512 | 3 |  |  | ok (all values; bool/date/numeric types) | MEASURED |
| read_values | large (1,000,000) | **kyrax** | 5.499 | 21% | 5.662 | 434 | 432-434 | 5.502 | 3 | 1.09 (calamine) | 0.60 (calamine) | FAIL row 1 col 4: 44973.0 (float) != datetime.datetime(2023, 2, 16, 0, 0) (datetime) | MEASURED |
| read_values | large (1,000,000) | calamine | 5.067 | 26% | 12.147 | 726 | 726-726 | 5.083 | 3 |  |  | ok (all values; bool/date/numeric types) | MEASURED |
| read_df | small (1,000) | **kyrax** | 0.671 | 21% | 0.793 | 128 | 126-128 | 0.418 | 3 | 1.19 (pandas-calamine) | 1.05 (pandas-calamine) | ok (all values; bool/date/numeric types) | MEASURED |
| read_df | small (1,000) | pandas-calamine | 0.562 | 24% | 0.718 | 122 | 122-122 | 0.410 | 3 |  |  | ok (all values; bool/date/numeric types) | MEASURED |
| read_df | small (1,000) | pandas-openpyxl | 0.667 | 18% | 0.877 | 126 | 126-126 | 0.547 | 3 |  |  | ok (all values; bool/date/numeric types) | MEASURED |
| read_df | medium (100,000) | **kyrax** | 0.930 | 31% | 2.079 | 170 | 169-170 | 0.798 | 3 | 0.65 (pandas-calamine) | 0.88 (pandas-openpyxl) | ok (all values; bool/date/numeric types) | MEASURED |
| read_df | medium (100,000) | pandas-calamine | 1.422 | 18% | 2.532 | 250 | 249-250 | 1.293 | 3 |  |  | ok (all values; bool/date/numeric types) | MEASURED |
| read_df | medium (100,000) | pandas-openpyxl | 6.199 | 15% | 7.291 | 193 | 193-193 | 6.058 | 3 |  |  | ok (all values; bool/date/numeric types) | MEASURED |
| read_df | large (1,000,000) | **kyrax** | 4.606 | 13% | 14.123 | 614 | 614-615 | 4.414 | 3 | 0.46 (pandas-calamine) | 0.46 (pandas-calamine) | ok (all values; bool/date/numeric types) | MEASURED |
| read_df | large (1,000,000) | pandas-calamine | 10.004 | 4% | 19.209 | 1344 | 1344-1344 | 9.830 | 3 |  |  | ok (all values; bool/date/numeric types) | MEASURED |
| write | small (1,000) | (data only) | 0.000 | 14% | 0.034 | 9 | 9-9 | 0.000 | 3 |  |  | n/a | MEASURED |
| write | small (1,000) | **kyrax** | 0.022 | 33% | 0.059 | 19 | 19-19 | 0.022 | 3 | 0.30 (xlsxwriter) | 1.26 (xlsxwriter-cm) | ok (reopens, all values) | MEASURED |
| write | small (1,000) | **kyrax-stream** | 0.017 | 10% | 0.056 | 19 | 18-19 | 0.018 | 3 | 0.24 (xlsxwriter) | 1.25 (xlsxwriter-cm) | ok (reopens, all values) | MEASURED |
| write | small (1,000) | xlsxwriter | 0.074 | 45% | 0.113 | 16 | 16-16 | 0.074 | 3 |  |  | ok (reopens, all values) | MEASURED |
| write | small (1,000) | xlsxwriter-cm | 0.080 | 34% | 0.115 | 15 | 15-15 | 0.080 | 3 |  |  | ok (reopens, all values) | MEASURED |
| write | small (1,000) | openpyxl | 0.382 | 11% | 0.435 | 37 | 37-38 | 0.209 | 3 |  |  | ok (reopens, all values) | MEASURED |
| write | small (1,000) | openpyxl-wo | 0.380 | 2% | 0.440 | 34 | 34-35 | 0.220 | 3 |  |  | ok (reopens, all values) | MEASURED |
| write | medium (100,000) | (data only) | 0.000 | 7% | 0.343 | 44 | 44-44 | 0.000 | 3 |  |  | n/a | MEASURED |
| write | medium (100,000) | **kyrax** | 0.512 | 14% | 0.847 | 124 | 124-124 | 0.512 | 3 | 0.12 (xlsxwriter-cm) | 2.48 (xlsxwriter-cm) | ok (reopens, all values) | MEASURED |
| write | medium (100,000) | **kyrax-stream** | 0.479 | 11% | 0.782 | 124 | 124-124 | 0.479 | 3 | 0.11 (xlsxwriter-cm) | 2.48 (xlsxwriter-cm) | ok (reopens, all values) | MEASURED |
| write | medium (100,000) | xlsxwriter | 4.596 | 14% | 5.112 | 151 | 151-151 | 4.603 | 3 |  |  | ok (reopens, all values) | MEASURED |
| write | medium (100,000) | xlsxwriter-cm | 4.249 | 17% | 4.603 | 50 | 50-50 | 4.250 | 3 |  |  | ok (reopens, all values) | MEASURED |
| write | medium (100,000) | openpyxl | 9.564 | 18% | 10.287 | 337 | 337-337 | 9.463 | 3 |  |  | ok (reopens, all values) | MEASURED |
| write | medium (100,000) | openpyxl-wo | 7.982 | 6% | 8.382 | 69 | 69-70 | 7.824 | 3 |  |  | ok (reopens, all values) | MEASURED |
| write | large (1,000,000) | (data only) | 0.000 | 43% | 3.803 | 361 | 361-361 | 0.000 | 3 |  |  | n/a | MEASURED |
| write | large (1,000,000) | **kyrax** | 4.688 | 9% | 8.315 | 1101 | 1100-1102 | 4.694 | 3 | 0.12 (xlsxwriter-cm) | 3.00 (xlsxwriter-cm) | ok (reopens, spot rows) | MEASURED |
| write | large (1,000,000) | **kyrax-stream** | 4.740 | 3% | 8.195 | 1101 | 1101-1101 | 4.744 | 3 | 0.12 (xlsxwriter-cm) | 3.00 (xlsxwriter-cm) | ok (reopens, spot rows) | MEASURED |
| write | large (1,000,000) | xlsxwriter-cm | 38.057 | 5% | 41.431 | 367 | 367-367 | 38.096 | 3 |  |  | ok (reopens, spot rows) | MEASURED |
| edit | small (1,000) | **kyrax** | 0.018 | 3% | 0.043 | 21 | 21-21 | 0.018 | 3 | 0.04 (openpyxl) | 0.54 (openpyxl) | ok (all values lossless) | MEASURED |
| edit | small (1,000) | openpyxl | 0.425 | 19% | 0.481 | 39 | 39-39 | 0.235 | 3 |  |  | ok (all values lossless) | MEASURED |
| edit | medium (100,000) | **kyrax** | 0.559 | 14% | 0.589 | 110 | 110-110 | 0.559 | 3 | 0.04 (openpyxl) | 0.25 (openpyxl) | ok (all values lossless) | MEASURED |
| edit | medium (100,000) | openpyxl | 14.979 | 18% | 16.192 | 438 | 438-439 | 14.825 | 3 |  |  | ok (all values lossless) | MEASURED |
| edit | large (1,000,000) | **kyrax** | 6.151 | 22% | 6.190 | 955 | 955-955 | 6.158 | 3 |  |  | ok (all values lossless) | MEASURED |
