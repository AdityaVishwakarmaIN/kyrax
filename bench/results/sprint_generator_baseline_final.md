# kyrax benchmark scoreboard (full)

- Date: 2026-10-30 05:05 UTC · commit `fc0fcb07b2447288ab48e909b65e671c20f1725d` · Python 3.12.14 · 3 CPUs · Linux-6.18.57-x86_64-with-glibc2.41
- Versions: openpyxl 3.1.5, XlsxWriter 3.2.9, python-calamine 0.9.0, pandas 3.1.0, pyarrow 26.0.0, numpy 2.5.4
- Harness runtime: 3.2 s
- All rows MEASURED on this machine with this harness (no estimates).
- `CPU-s` = user+sys of the timed section (library import + task, all threads), median.
  `noise` = (max-min)/median over the repeats. `RSS` = worker peak RSS (VmHWM).
  `ratio` = kyrax median / best competitor median (>1 = kyrax worse).

| task | size | lib | CPU-s | CPU noise | proc CPU-s | peak RSS MB | RSS min-max | wall-s | output bytes | n | ratio CPU (vs) | ratio RSS (vs) | verify | status |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---|---|---|---|
| write_stream | medium (100,000) | **kyrax-stream-inline** | 0.696 | 5% | 0.736 | 224 | 223-224 | 0.697 | 4,755,043 | 3 |  |  | ok (reopens, all values) | MEASURED |
