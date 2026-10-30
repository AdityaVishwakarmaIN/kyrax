# Partial and streaming read benchmark

Source: `fe07016` · 2026-10-29T04:12:07.185273+00:00 · Python 3.12.14
Package root: `/workspace/kyrax/python`

CPU/wall include import, open, identical Python checksum/type bookkeeping, and close.
Peak RSS is captured before untimed expected-data validation. First read consumes ten
rows including the header. Calamine eagerly loads all rows in both tasks. Shared
strings may load eagerly in every reader. FAIL rows have unequal type semantics.

| task | size | library | CPU s | wall s | peak MiB | n | verification |
|---|---|---|---:|---:|---:|---:|---|
| read_stream | large | kyrax | 8.7465 | 8.7539 | 75.9 | 1 | ok (count and typed full-cell checksum) |
