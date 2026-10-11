"""Pin the write-path conversion of every Python scalar type to a cell.

Exact builtin types take a fast path in the Rust writer; subclasses, numpy-ish
scalars, Decimal, IntEnum etc. take the generic path. Both must produce the
same cells, so this test freezes the observable behaviour (value, type and
number format after a kyrax round-trip) for both writers.
"""

from __future__ import annotations

import datetime as dt
import enum
import math
import warnings
from decimal import Decimal

import kyrax
import pytest


class MyInt(int):
    pass


class MyStr(str):
    pass


class MyDT(dt.datetime):
    pass


class Color(enum.IntEnum):
    RED = 7


LONG = "x" * 40000

# (label, python value, expected read-back value, expected number_format)
CASES = [
    ("bool_true", True, True, None),
    ("bool_false", False, False, None),
    ("int", 5, 5.0, None),
    ("int_zero", 0, 0.0, None),
    ("int_neg_zero", -0, 0.0, None),
    ("int_neg", -7, -7.0, None),
    ("int_2_53", 2**53, 2.0**53, None),
    ("int_i64_max", 2**63 - 1, float(2**63 - 1), None),
    ("int_2_63", 2**63, float(2**63), None),
    ("int_2_80", 2**80, float(2**80), None),
    ("float", 1.5, 1.5, None),
    ("float_neg_zero", -0.0, -0.0, None),
    ("float_nan", float("nan"), float("nan"), None),
    ("float_inf", float("inf"), float("inf"), None),
    ("float_ninf", float("-inf"), float("-inf"), None),
    ("str", "hello", "hello", None),
    ("str_empty", "", None, None),
    ("err_div0", "#DIV/0!", "#DIV/0!", None),
    ("err_na", "#N/A", "#N/A", None),
    ("hash_not_error", "#foo", "#foo", None),
    ("str_long", LONG, "x" * 32767, None),
    (
        "datetime",
        dt.datetime(2020, 1, 2, 3, 4, 5, 600000),
        dt.datetime(2020, 1, 2, 3, 4, 5, 600000),
        "m/d/yy h:mm",
    ),
    ("datetime_midnight", dt.datetime(2020, 1, 2), dt.datetime(2020, 1, 2), "mm-dd-yy"),
    ("date", dt.date(2020, 1, 2), dt.datetime(2020, 1, 2), "mm-dd-yy"),
    ("time", dt.time(3, 4, 5, 600000), dt.time(3, 4, 5, 600000), "hh:mm:ss"),
    (
        "timedelta",
        dt.timedelta(days=1, seconds=5, microseconds=500000),
        dt.timedelta(days=1, seconds=5, microseconds=500000),
        "[h]:mm:ss",
    ),
    ("int_subclass", MyInt(3), 3.0, None),
    ("str_subclass", MyStr("s"), "s", None),
    ("datetime_subclass", MyDT(2021, 2, 3, 4, 5, 6), dt.datetime(2021, 2, 3, 4, 5, 6), "m/d/yy h:mm"),
    ("decimal", Decimal("1.5"), 1.5, None),
    ("intenum", Color.RED, 7.0, None),
]


def _same(actual, expected):
    if isinstance(expected, float) and math.isnan(expected):
        return isinstance(actual, float) and math.isnan(actual)
    if isinstance(expected, float) and expected == 0.0:
        return (
            isinstance(actual, float)
            and actual == 0.0
            and math.copysign(1.0, actual) == math.copysign(1.0, expected)
        )
    return type(actual) is type(expected) and actual == expected


@pytest.mark.parametrize("writer", ["write_excel_turbo", "write_excel_turbo_stream"])
def test_write_cell_types_roundtrip(tmp_path, writer):
    path = str(tmp_path / "types.xlsx")
    rows = [[v] for _, v, _, _ in CASES]
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        getattr(kyrax, writer)(path, [{"name": "S", "rows": rows}])
    truncation = [
        w for w in caught if issubclass(w.category, UserWarning) and "32767" in str(w.message)
    ]
    assert len(truncation) == 1
    assert "40000 characters" in str(truncation[0].message)

    ws = kyrax.load_workbook(path).active
    for i, (label, _, expected, fmt) in enumerate(CASES, start=1):
        cell = ws.cell(i, 1)
        assert _same(cell.value, expected), (
            f"{label}: got {cell.value!r} ({type(cell.value).__name__}), "
            f"want {expected!r} ({type(expected).__name__})"
        )
        assert cell.number_format == fmt, f"{label}: number_format {cell.number_format!r}"


def test_error_literal_is_not_truncated_or_altered(tmp_path):
    path = str(tmp_path / "err.xlsx")
    kyrax.write_excel_turbo(path, [{"name": "S", "rows": [["#DIV/0!", "#N/A"]]}])
    row = next(kyrax.load_workbook(path).active.iter_rows(values_only=True))
    assert row == ("#DIV/0!", "#N/A")
