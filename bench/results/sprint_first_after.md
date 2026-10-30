# Partial and streaming read benchmark

Source: `fe07016` · 2026-10-29T04:11:50.646878+00:00 · Python 3.12.14
Package root: `/workspace/kyrax/python`

CPU/wall include import, open, identical Python checksum/type bookkeeping, and close.
Peak RSS is captured before untimed expected-data validation. First read consumes ten
rows including the header. Calamine eagerly loads all rows in both tasks. Shared
strings may load eagerly in every reader. FAIL rows have unequal type semantics.

| task | size | library | CPU s | wall s | peak MiB | n | verification |
|---|---|---|---:|---:|---:|---:|---|
| read_first | small | kyrax | 0.0073 | 0.0073 | 25.8 | 3 | ok (count and typed full-cell checksum) |
| read_first | small | calamine | 0.0057 | 0.0057 | 19.2 | 3 | ok (count and typed full-cell checksum) |
| read_first | medium | kyrax | 0.0266 | 0.0266 | 30.1 | 3 | ok (count and typed full-cell checksum) |
| read_first | medium | calamine | 0.5617 | 0.5618 | 91.7 | 3 | ok (count and typed full-cell checksum) |
| read_first | large | kyrax | 0.1370 | 0.1370 | 75.9 | 3 | ok (count and typed full-cell checksum) |
| read_first | large | calamine | 4.9884 | 4.9890 | 760.4 | 3 | ok (count and typed full-cell checksum) |
