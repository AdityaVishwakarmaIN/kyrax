"""Shared typed-read corpus definitions and lossless value encoding.

Regenerate expectations with the independent reader in
``bench/generate_readonly_oracle.py``. The saved corpus comes from openpyxl
3.1.5, not from the Kyrax implementation under test.
"""

from __future__ import annotations

import hashlib
import json

FIXTURES = [
    "fixture-single-sheet.xlsx",
    "fixture-multi-dtypes-columns.xlsx",
    "fixture-multi-sheet.xlsx",
    "sheet-null-strings.xlsx",
    "sheet-null-strings-empty.xlsx",
    "sheet-with-na.xlsx",
    "null-column.xlsx",
    "no-header.xlsx",
    "decimal-numbers.xlsx",
    "single-sheet-skip-rows-durations.xlsx",
    "fixture-type-errors.xlsx",
    "fixture-single-sheet-with-types.xlsx",
    "div0.xlsx",
    "empty.xlsx",
    "sheet-and-table-with-offset.xlsx",
    "fixture-changing-header-location.xlsx",
    "fixture-single-sheet-duplicated-columns.xlsx",
]
WINDOWS = [
    {},
    {"min_row": 2},
    {"min_row": 3, "max_row": 5},
    {"max_row": 1},
    {"max_row": 3},
    {"min_col": 2},
    {"min_col": 2, "max_col": 3},
    {"max_col": 1},
    {"min_row": 2, "max_row": 4, "min_col": 2, "max_col": 4},
    {"min_row": 50, "max_row": 60},
    {"min_col": 40},
]
CELLS = [(1, 1), (1, 2), (2, 1), (2, 2), (3, 3), (5, 2), (4, 7), (1, 99), (99, 1), (200, 200)]


def enc(v):
    """Type-tagged, lossless encoding of a cell value."""
    if v is None:
        return ["n"]
    t = type(v).__name__
    if t == "float":
        return ["f", v.hex()]
    if t in ("str", "bool", "int"):
        return [t[0], v]
    return ["?", t, repr(v)]


BIG = "infer-dtypes-fallback.xlsx"  # ~140k rows: several 10k-row batches
BIG_WINDOWS = [
    {},
    {"min_row": 9995, "max_row": 10005},
    {"min_row": 19999, "max_row": 20002},
    {"min_row": 140000, "min_col": 2},
]


def digest(rows):
    blob = json.dumps(rows, separators=(",", ":")).encode()
    return [len(rows), hashlib.sha256(blob).hexdigest()]


def build(path, *, load_workbook):
    out = {}
    wb = load_workbook(str(path / BIG), read_only=True)
    ws = wb.active
    out["__big__"] = [
        digest([[enc(c) for c in r] for r in ws.iter_rows(values_only=True, **w)])
        for w in BIG_WINDOWS
    ]
    wb.close()
    for fx in FIXTURES:
        wb = load_workbook(str(path / fx), read_only=True)
        entry = {}
        for name in wb.sheetnames[:2]:
            ws = wb[name]
            wins = []
            for w in WINDOWS:
                wins.append([[enc(c) for c in r] for r in ws.iter_rows(values_only=True, **w)])
            cells = [enc(ws.cell(r, c).value) for r, c in CELLS]
            entry[name] = {"windows": wins, "cells": cells}
        wb.close()
        out[fx] = entry
    return out


if __name__ == "__main__":
    raise SystemExit("Use bench/generate_readonly_oracle.py with the benchmark environment")
