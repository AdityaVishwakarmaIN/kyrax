# Partial and streaming read benchmark

Source: `fc0fcb0` · 2026-10-28T18:20:41.092578+00:00 · Python 3.12.14
Package root: `/workspace/kyrax-baseline-package`

CPU/wall include import, open, identical Python checksum/type bookkeeping, and close.
Peak RSS is captured before untimed expected-data validation. First read consumes ten
rows including the header. Calamine eagerly loads all rows in both tasks. Shared
strings may load eagerly in every reader. FAIL rows have unequal type semantics.

| task | size | library | CPU s | wall s | peak MiB | n | verification |
|---|---|---|---:|---:|---:|---:|---|
| read_first | small | kyrax | 0.0089 | 0.0089 | 26.4 | 3 | FAIL typed checksum/count |
| read_first | medium | kyrax | 0.1214 | 0.1214 | 39.6 | 3 | FAIL typed checksum/count |
| read_first | large | kyrax | 0.8241 | 0.8242 | 75.9 | 3 | FAIL typed checksum/count |
| read_stream | small | kyrax | 0.0165 | 0.0165 | 26.6 | 3 | FAIL typed checksum/count |
| read_stream | medium | kyrax | 1.1048 | 1.1049 | 41.9 | 3 | FAIL typed checksum/count |
| read_stream | large | kyrax | 11.2187 | 11.2298 | 75.9 | 3 | FAIL typed checksum/count |
