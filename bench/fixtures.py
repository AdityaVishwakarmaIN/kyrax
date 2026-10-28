"""Deterministic benchmark data and fixture generation.

The same row generator feeds both the READ/EDIT fixtures (written once with
XlsxWriter, shared-strings table, like an Excel-saved file) and the WRITE
tasks (rows built in-process before the timed section).
"""

from __future__ import annotations

import datetime as _dt
import os
import random

SIZES = {"small": 1_000, "medium": 100_000, "large": 1_000_000}
HEADER = ["id", "amount", "category", "label", "date", "flag", "qty", "price"]
NCOLS = len(HEADER)
_CATS = [f"cat-{i:02d}" for i in range(50)]
_EPOCH = _dt.datetime(2020, 1, 1)
SEED = 20261011
FIXTURE_VERSION = 1


def iter_rows(n: int):
    """Yield header + n deterministic rows without retaining the data set."""
    rnd = random.Random(SEED)
    yield list(HEADER)
    cats = _CATS
    for i in range(n):
        yield [
                i + 1,
                round(rnd.random() * 10_000, 2),
                cats[rnd.randrange(50)],
                f"item-{rnd.randrange(n * 4 + 1)}",
                _EPOCH + _dt.timedelta(days=rnd.randrange(3650)),
                rnd.random() < 0.5,
                rnd.randrange(1000),
                round(rnd.random() * 100, 3),
            ]


def make_rows(n: int) -> list[list]:
    """Header + n data rows, 8 mixed-type columns (int, float, str, date, bool)."""
    return list(iter_rows(n))


def fixture_path(cache_dir: str, size: str) -> str:
    return os.path.join(cache_dir, f"fixture_v{FIXTURE_VERSION}_{size}.xlsx")


def ensure_fixture(cache_dir: str, size: str) -> str:
    """Create the READ/EDIT fixture for ``size`` once (needs xlsxwriter)."""
    os.makedirs(cache_dir, exist_ok=True)
    path = fixture_path(cache_dir, size)
    if os.path.exists(path):
        return path
    import xlsxwriter

    tmp = path + ".tmp.xlsx"
    rows = make_rows(SIZES[size])
    wb = xlsxwriter.Workbook(tmp, {"default_date_format": "yyyy-mm-dd"})
    ws = wb.add_worksheet("Sheet1")
    for r, row in enumerate(rows):
        ws.write_row(r, 0, row)
    wb.close()
    os.replace(tmp, path)
    return path
