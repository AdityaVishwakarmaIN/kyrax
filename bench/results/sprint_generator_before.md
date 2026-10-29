# kyrax benchmark scoreboard (full)

- Date: 2026-10-28 18:19 UTC · commit `2bd9754` · Python 3.12.14 · 3 CPUs · Linux-6.18.55-x86_64-with-glibc2.41
- Versions: openpyxl 3.1.5, XlsxWriter 3.2.9, python-calamine 0.9.0, pandas 3.1.0, pyarrow 26.0.0, numpy 2.5.4
- Harness runtime: 41.7 s
- All rows MEASURED on this machine with this harness (no estimates).
- `CPU-s` = user+sys of the timed section (library import + task, all threads), median.
  `noise` = (max-min)/median over the repeats. `RSS` = worker peak RSS (VmHWM).
  `ratio` = kyrax median / best competitor median (>1 = kyrax worse).

| task | size | lib | CPU-s | CPU noise | proc CPU-s | peak RSS MB | RSS min-max | wall-s | n | ratio CPU (vs) | ratio RSS (vs) | verify | status |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|---|---|
| write_stream | small (1,000) | (data only) | 0.003 | 59% | 0.029 | 9 | 9-9 | 0.003 | 3 |  |  | n/a | MEASURED |
| write_stream | small (1,000) | **kyrax-stream** | 0.025 | 22% | 0.060 | 20 | 20-20 | 0.025 | 3 | 0.32 (xlsxwriter-cm) | 1.35 (xlsxwriter-cm) | ok (reopens, all values) | MEASURED |
| write_stream | small (1,000) | xlsxwriter-cm | 0.078 | 10% | 0.109 | 15 | 15-15 | 0.078 | 3 |  |  | ok (reopens, all values) | MEASURED |
| write_stream | small (1,000) | openpyxl-wo | 0.370 | 19% | 0.419 | 34 | 34-35 | 0.209 | 3 |  |  | ok (reopens, all values) | MEASURED |
| write_stream | medium (100,000) | (data only) | 0.254 | 19% | 0.282 | 9 | 9-9 | 0.256 | 3 |  |  | n/a | MEASURED |
| write_stream | medium (100,000) | **kyrax-stream** | 0.770 | 23% | 0.825 | 224 | 224-224 | 0.771 | 3 | 0.20 (xlsxwriter-cm) | 15.02 (xlsxwriter-cm) | ok (reopens, all values) | MEASURED |
| write_stream | medium (100,000) | xlsxwriter-cm | 3.928 | 14% | 3.961 | 15 | 15-15 | 3.932 | 3 |  |  | ok (reopens, all values) | MEASURED |
| write_stream | medium (100,000) | openpyxl-wo | 7.412 | 10% | 7.454 | 34 | 34-34 | 7.223 | 3 |  |  | ok (reopens, all values) | MEASURED |
