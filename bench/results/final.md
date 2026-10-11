# kyrax cost-per-task: before / after

- Original baseline: commit `ea9b33a` source (= 6db1236 source), 2026-10-11 08:52 UTC (bench/results/baseline.md).
- After: commit `2067acf`, 2026-10-11 12:28 UTC, Intel(R) Xeon(R) Processor @ 2.80GHz; kyrax rows re-measured; competitor rows reused from the baseline run unless re-measured (see below).
- Before (re-measured): commit `6db1236` build, 2026-10-11 12:25 UTC, same session as After, back-to-back. Rows marked † use it as Before (original baseline kept below).
- Competitor re-measure (2026-10-11 12:28 UTC, same session): replaces the baseline competitor median in the ratios (table below).
- Competitor re-measure (2026-10-11 12:29 UTC, same session): replaces the baseline competitor median in the ratios (table below).
- Competitor re-measure (2026-10-11 12:29 UTC, same session): replaces the baseline competitor median in the ratios (table below).
- Competitor re-measure (2026-10-11 12:30 UTC, same session): replaces the baseline competitor median in the ratios (table below).
- Competitor re-measure (2026-10-11 12:32 UTC, same session): replaces the baseline competitor median in the ratios (table below).
- CPU-s = user+sys of library import + task (all threads), median; RSS = peak (VmHWM).
  `[min-max]` = noise band over repeats. Ratio = kyrax / best competitor (>1 = kyrax worse).
- Every number below is MEASURED with bench/run.py on this machine. No estimates in this table.

| task | size | lib | CPU before | CPU after | Δ CPU | RSS before | RSS after | Δ RSS | CPU ratio before → after (vs) | RSS ratio before → after (vs) | verify |
|---|---|---|---:|---:|---:|---:|---:|---:|---|---|---|
| import | small (1,000) | kyrax † | 0.030 [0.028-0.043] | 0.015 [0.013-0.019] | -48% | 20 | 18 | -11% | 24.22 → **12.59** (calamine) | 1.63 → **1.45** (calamine) | n/a |
| read_values | small (1,000) | kyrax † | 0.371 [0.307-0.482] | 0.027 [0.022-0.030] | -93% | 63 | 23 | -64% | 49.64 → **3.62** (calamine) | 4.69 → **1.69** (calamine) | n/a |
| read_values | medium (100,000) | kyrax † | 2.032 [1.946-2.044] | 0.754 [0.720-0.774] | -63% | 121 | 76 | -38% | 3.27 → **1.21** (calamine) | 1.45 → **0.90** (openpyxl-ro) | n/a |
| read_values | large (1,000,000) | kyrax † | 16.003 [15.823-16.550] | 6.544 [6.507-6.854] | -59% | 517 | 430 | -17% | 2.53 → **1.03** (calamine) | 0.71 → **0.59** (calamine) | n/a |
| read_df | small (1,000) | kyrax † | 0.637 [0.633-0.669] | 0.655 [0.629-0.696] | +3% | 107 | 106 | -0% | 0.97 → **0.99** (pandas-calamine) | 1.06 → **1.06** (pandas-calamine) | n/a |
| read_df | medium (100,000) | kyrax † | 1.268 [1.261-1.342] | 1.148 [1.116-1.200] | -9% | 148 | 148 | -0% | 0.72 → **0.65** (pandas-calamine) | 0.85 → **0.85** (pandas-openpyxl) | n/a |
| read_df | large (1,000,000) | kyrax † | 5.655 [5.586-6.190] | 6.169 [6.030-6.202] | +9% | 619 | 616 | -0% | 0.43 → **0.47** (pandas-calamine) | 0.47 → **0.47** (pandas-calamine) | n/a |
| edit | large (1,000,000) | kyrax † | 7.250 [7.122-7.954] | 6.778 [6.453-7.117] | -6% | 959 | 957 | -0% | no competitor | no competitor | ok (all values lossless) |
| write | small (1,000) | kyrax † | 0.040 [0.040-0.042] | 0.020 [0.019-0.027] | -50% | 27 | 22 | -20% | 0.43 → **0.21** (xlsxwriter) | 1.30 → **1.05** (xlsxwriter-cm) | ok (reopens, all values) |
| write | small (1,000) | kyrax-stream † | 0.042 [0.040-0.059] | 0.019 [0.018-0.020] | -53% | 27 | 22 | -19% | 0.45 → **0.21** (xlsxwriter) | 1.30 → **1.05** (xlsxwriter-cm) | ok (reopens, all values) |
| write | medium (100,000) | kyrax † | 1.134 [1.091-1.444] | 0.529 [0.527-0.565] | -53% | 404 | 127 | -69% | 0.22 → **0.10** (xlsxwriter) | 7.26 → **2.28** (xlsxwriter-cm) | ok (reopens, all values) |
| write | medium (100,000) | kyrax-stream † | 1.340 [1.319-1.405] | 0.542 [0.537-0.568] | -60% | 400 | 127 | -68% | 0.26 → **0.11** (xlsxwriter) | 7.18 → **2.27** (xlsxwriter-cm) | ok (reopens, all values) |
| write | large (1,000,000) | kyrax † | 25.227 [19.318-25.869] | 5.422 [5.124-5.795] | -79% | 3807 | 1104 | -71% | 0.44 → **0.10** (xlsxwriter-cm) | 10.22 → **2.96** (xlsxwriter-cm) | ok (reopens, spot rows) |
| write | large (1,000,000) | kyrax-stream † | 23.950 [15.660-25.798] | 5.653 [5.551-5.780] | -76% | 3833 | 1104 | -71% | 0.42 → **0.10** (xlsxwriter-cm) | 10.29 → **2.96** (xlsxwriter-cm) | ok (reopens, spot rows) |
| edit | small (1,000) | kyrax † | 0.048 [0.040-0.058] | 0.023 [0.022-0.034] | -52% | 25 | 23 | -6% | 0.08 → **0.04** (openpyxl) | 0.59 → **0.55** (openpyxl) | ok (all values lossless) |
| edit | medium (100,000) | kyrax † | 0.875 [0.726-0.880] | 0.732 [0.691-0.887] | -16% | 114 | 112 | -2% | 0.05 → **0.04** (openpyxl) | 0.26 → **0.25** (openpyxl) | ok (all values lossless) |

## Re-measured competitor rows (used for ratios)

| task | size | lib | original baseline CPU-s | re-measured CPU-s | n | original RSS | re-measured RSS |
|---|---|---|---:|---:|---:|---:|---:|
| import | small | calamine | 0.001 | 0.001 | 5 | 12 | 13 |
| read_values | small | calamine | 0.007 | 0.007 | 5 | 13 | 13 |
| read_values | medium | calamine | 0.636 | 0.622 | 3 | 84 | 84 |
| read_values | large | calamine | 6.550 | 6.337 | 3 | 728 | 728 |
| read_df | small | pandas-calamine | 0.661 | 0.659 | 5 | 106 | 100 |
| read_df | medium | pandas-calamine | 1.700 | 1.765 | 3 | 227 | 222 |
| read_df | large | pandas-calamine | 13.236 | 13.074 | 3 | 1319 | 1313 |
| write | small | xlsxwriter | 0.092 | 0.092 | 5 | 22 | 22 |
| write | small | xlsxwriter-cm | 0.097 | 0.108 | 5 | 21 | 21 |
| write | medium | xlsxwriter | 4.910 | 5.122 | 3 | 157 | 157 |
| write | medium | xlsxwriter-cm | 5.370 | 5.585 | 3 | 56 | 56 |
| edit | small | openpyxl | 0.610 | 0.613 | 5 | 45 | 42 |
| edit | medium | openpyxl | 17.343 | 16.949 | 3 | 445 | 442 |
| write | large | xlsxwriter-cm | 54.348 | 56.815 | 1 | 373 | 373 |

## † Re-measured baseline rows

| task | size | lib | original baseline CPU-s | re-measured pre-change CPU-s | original RSS | re-measured RSS |
|---|---|---|---:|---:|---:|---:|
| import | small | kyrax | 0.030 | 0.030 | 20 | 20 |
| read_values | small | kyrax | 0.360 | 0.371 | 67 | 63 |
| read_values | medium | kyrax | 1.916 | 2.032 | 125 | 121 |
| read_values | large | kyrax | 16.071 | 16.003 | 520 | 517 |
| read_df | small | kyrax | 0.656 | 0.637 | 112 | 107 |
| read_df | medium | kyrax | 1.165 | 1.268 | 154 | 148 |
| read_df | large | kyrax | 5.621 | 5.655 | 619 | 619 |
| edit | large | kyrax | 6.702 | 7.250 | 959 | 959 |
| write | small | kyrax | 0.041 | 0.040 | 27 | 27 |
| write | small | kyrax-stream | 0.045 | 0.042 | 27 | 27 |
| write | medium | kyrax | 1.076 | 1.134 | 404 | 404 |
| write | medium | kyrax-stream | 1.060 | 1.340 | 400 | 400 |
| write | large | kyrax | 26.090 | 25.227 | 3807 | 3807 |
| write | large | kyrax-stream | 23.874 | 23.950 | 3833 | 3833 |
| edit | small | kyrax | 0.044 | 0.048 | 25 | 25 |
| edit | medium | kyrax | 0.727 | 0.875 | 114 | 114 |

## Caveats

- read_values: kyrax read_only returns dates as serial floats, booleans as 'TRUE'/'FALSE' strings and ints as floats; calamine/openpyxl return typed values (see baseline.md). Any read_values comparison carries this caveat.
- Ratios use the same-session competitor re-measure where listed above (write 1M xlsxwriter-cm: 1 repeat); other competitor medians come from the idle morning baseline.
- The machine was restarted between the morning baseline and the final run, so Before and After are both taken from the final session.
