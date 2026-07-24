"""W1 write-core round-trip tests: turbo write -> openpyxl load."""

from __future__ import annotations

import warnings
from datetime import date, datetime
from pathlib import Path

import pytest

from kyrax import write_excel_turbo, write_excel_turbo_bytes

openpyxl = pytest.importorskip("openpyxl")


def _load(path: Path, **kwargs):
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        return openpyxl.load_workbook(path, **kwargs)


def test_smoke_inline(tmp_path: Path):
    path = tmp_path / "smoke.xlsx"
    write_excel_turbo(
        str(path),
        [
            {
                "name": "Data",
                "columns": [
                    [1.5, 2.0, None],
                    ["hello", " world", "a&b"],
                    [True, False, True],
                ],
            }
        ],
        string_mode="inline",
    )
    wb = _load(path)
    ws = wb["Data"]
    assert ws["A1"].value == 1.5
    assert ws["B1"].value == "hello"
    assert ws["B2"].value == " world"
    assert ws["B3"].value == "a&b"
    assert ws["C1"].value is True
    assert ws["C2"].value is False
    assert ws["A3"].value is None


def test_sst_variant(tmp_path: Path):
    path = tmp_path / "sst.xlsx"
    write_excel_turbo(
        str(path),
        [
            {
                "name": "S",
                "columns": [["foo", "bar", "foo", "foo"]],
            }
        ],
        string_mode="sst",
    )
    wb = _load(path)
    ws = wb["S"]
    assert ws["A1"].value == "foo"
    assert ws["A2"].value == "bar"
    assert ws["A3"].value == "foo"
    # sharedStrings part should exist
    import zipfile

    with zipfile.ZipFile(path) as zf:
        assert "xl/sharedStrings.xml" in zf.namelist()


def test_mixed_1k(tmp_path: Path):
    n = 1000
    nums = list(range(n))
    strs = [f"s{i % 50}" for i in range(n)]
    bools = [i % 2 == 0 for i in range(n)]
    path = tmp_path / "mixed1k.xlsx"
    write_excel_turbo(
        str(path),
        [{"name": "M", "columns": [nums, strs, bools]}],
        string_mode="inline",
    )
    wb = _load(path)
    ws = wb["M"]
    assert ws["A1"].value == 0
    assert ws["A1000"].value == 999
    assert ws["B1"].value == "s0"
    assert ws["B51"].value == "s0"
    assert ws["C1"].value is True
    assert ws["C2"].value is False


def test_formulas_with_cache(tmp_path: Path):
    path = tmp_path / "formulas.xlsx"
    write_excel_turbo(
        str(path),
        [
            {
                "name": "F",
                "columns": [[10.0, 20.0, None]],
                "formulas": {
                    (2, 0): {"text": "=A1+A2", "cached": 30.0},
                    (0, 1): {"text": "=1+1", "cached": 2.0},
                },
            }
        ],
        emit_cached_values=True,
    )
    wb = _load(path, data_only=False)
    ws = wb["F"]
    assert ws["A1"].value == 10.0
    assert ws["A3"].value == "=A1+A2"
    assert ws["B1"].value == "=1+1"

    # data_only path: openpyxl returns cached values only if they were written
    # and the file was previously calculated by Excel in some versions;
    # with our <f>+<v> emission, openpyxl data_only should read the v.
    wb2 = openpyxl.load_workbook(path, data_only=True)
    ws2 = wb2["F"]
    # openpyxl data_only uses cached v when present
    assert ws2["A3"].value == 30.0 or ws2["A3"].value == 30
    assert ws2["B1"].value == 2.0 or ws2["B1"].value == 2


def test_multi_sheet(tmp_path: Path):
    path = tmp_path / "multi.xlsx"
    write_excel_turbo(
        str(path),
        [
            {"name": "One", "columns": [[1, 2, 3]]},
            {"name": "Two", "columns": [["a", "b"]]},
        ],
    )
    wb = _load(path)
    assert wb.sheetnames == ["One", "Two"]
    assert wb["One"]["A2"].value == 2
    assert wb["Two"]["A1"].value == "a"


def test_hidden_sheet(tmp_path: Path):
    path = tmp_path / "hidden.xlsx"
    write_excel_turbo(
        str(path),
        [
            {"name": "Vis", "columns": [[1]]},
            {"name": "Hid", "visibility": "hidden", "columns": [[2]]},
        ],
    )
    wb = _load(path)
    assert wb["Hid"].sheet_state == "hidden"
    assert wb["Hid"]["A1"].value == 2


def test_empty_sheet(tmp_path: Path):
    path = tmp_path / "empty.xlsx"
    write_excel_turbo(
        str(path),
        [{"name": "Empty", "columns": []}],
    )
    wb = _load(path)
    assert "Empty" in wb.sheetnames
    assert wb["Empty"]["A1"].value is None


def test_types_mix(tmp_path: Path):
    path = tmp_path / "types.xlsx"
    write_excel_turbo(
        str(path),
        [
            {
                "name": "T",
                "columns": [
                    [1, 2.5, None],
                    ["x", "y", "z"],
                    [True, False, None],
                    [date(2020, 1, 15), None, None],
                ],
            }
        ],
    )
    wb = _load(path)
    ws = wb["T"]
    assert ws["A1"].value == 1
    assert ws["A2"].value == 2.5
    assert ws["B1"].value == "x"
    assert ws["C1"].value is True
    # W2: date cells get date numFmt xf → openpyxl returns date/datetime (is_date)
    d1 = ws["D1"].value
    assert d1 == date(2020, 1, 15) or (
        isinstance(d1, datetime) and d1.date() == date(2020, 1, 15)
    ) or d1 == 43845 or d1 == 43845.0
    assert ws["D1"].is_date is True or isinstance(d1, (date, datetime))


def test_bytes_api():
    data = write_excel_turbo_bytes(
        [{"name": "B", "rows": [[1, "a"], [2, "b"]]}],
        string_mode="inline",
    )
    assert data[:2] == b"PK"
    # load from bytes via temp
    import io

    wb = openpyxl.load_workbook(io.BytesIO(data))
    assert wb["B"]["A1"].value == 1
    assert wb["B"]["B2"].value == "b"


def test_col_dims(tmp_path: Path):
    path = tmp_path / "dims.xlsx"
    write_excel_turbo(
        str(path),
        [
            {
                "name": "D",
                "columns": [[1, 2]],
                "col_dims": [{"min": 1, "max": 1, "width": 20.0}],
                "row_dims": [{"row": 1, "height": 30.0}],
            }
        ],
    )
    wb = _load(path)
    ws = wb["D"]
    assert ws.column_dimensions["A"].width == pytest.approx(20.0)
    assert ws.row_dimensions[1].height == pytest.approx(30.0)


def test_auto_string_mode_rep(tmp_path: Path):
    # highly repetitive -> auto should pick SST
    path = tmp_path / "auto_rep.xlsx"
    vals = ["x"] * 100
    write_excel_turbo(
        str(path),
        [{"name": "A", "columns": [vals]}],
        string_mode="auto",
    )
    import zipfile

    with zipfile.ZipFile(path) as zf:
        assert "xl/sharedStrings.xml" in zf.namelist()
    wb = _load(path)
    assert wb["A"]["A50"].value == "x"
