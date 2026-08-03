"""B5b — PIVOT AUTHORING, Python binding surface.

Proves the `pivots` sheet key on `write_excel_turbo_bytes`:
  * the output loads in openpyxl without error (strong non-corruption signal);
  * the authored layout reads back through the kyrax turbo reader;
  * the output is byte-deterministic.
"""

from __future__ import annotations

import io
import zipfile

import openpyxl
import pytest

from kyrax import read_excel_turbo, write_excel_turbo_bytes

PIVOT_SHEET = {
    "name": "Data",
    "columns": [
        ["Region", "East", "East", "West", "West"],
        ["Product", "Widget", "Gadget", "Widget", "Gadget"],
        ["Amount", 100, 150, 200, 50],
    ],
    "pivots": [
        {
            "name": "PivotTable1",
            "source_range": "A1:C5",
            "rows": ["Region"],
            "cols": ["Product"],
            "data": [{"field": "Amount", "agg": "sum"}],
            "target_cell": "E3",
        }
    ],
}


def _bytes() -> bytes:
    return write_excel_turbo_bytes([PIVOT_SHEET], features="all")


def _parts(data: bytes) -> dict[str, str]:
    with zipfile.ZipFile(io.BytesIO(data)) as zf:
        return {n: zf.read(n).decode("utf-8") for n in zf.namelist()}


def test_openpyxl_loads_authored_pivot() -> None:
    wb = openpyxl.load_workbook(io.BytesIO(_bytes()))
    assert wb.sheetnames == ["Data"]


def test_reads_back_through_turbo_reader(tmp_path) -> None:
    path = tmp_path / "pivot.xlsx"
    path.write_bytes(_bytes())
    reader = read_excel_turbo(str(path))
    sheet = reader.load_sheet(0, features=["pivots"])
    pivots = sheet.pivots()
    assert pivots is not None and len(pivots) == 1
    p = pivots[0]
    assert p["name"] == "PivotTable1"
    assert p["location_ref"] == "E3:H6"
    assert p["row_fields"] == ["Region"]
    assert p["col_fields"] == ["Product"]
    assert p["data_fields"][0]["name"] == "Sum of Amount"
    assert p["data_fields"][0]["fld"] == 2
    assert p["cache_source"]["ref"] == "A1:C5"
    assert p["cache_field_names"] == ["Region", "Product", "Amount"]


def test_two_pivots_distinct_cache_ids(tmp_path) -> None:
    sheet = dict(PIVOT_SHEET)
    sheet["pivots"] = [
        dict(PIVOT_SHEET["pivots"][0]),
        {
            "name": "PivotTable2",
            "source_range": "A1:C5",
            "rows": ["Product"],
            "cols": [],
            "data": [{"field": "Amount", "agg": "count"}],
            "target_cell": "J3",
        },
    ]
    data = write_excel_turbo_bytes([sheet], features="all")
    path = tmp_path / "two.xlsx"
    path.write_bytes(data)
    reader = read_excel_turbo(str(path))
    pivots = reader.load_sheet(0, features=["pivots"]).pivots()
    assert pivots is not None and len(pivots) == 2
    cache_ids = {p["cache_id"] for p in pivots}
    assert cache_ids == {0, 1}
    by_name = {p["name"]: p for p in pivots}
    assert by_name["PivotTable1"]["row_fields"] == ["Region"]
    assert by_name["PivotTable2"]["row_fields"] == ["Product"]


def test_byte_deterministic() -> None:
    assert _bytes() == _bytes()


def test_package_wiring() -> None:
    parts = _parts(_bytes())
    for needle in [
        'pivotCache cacheId="0" r:id="rId4"',
        'refreshOnLoad="1"',
        'containsString="1"',
        'containsNumber="1" containsInteger="1" minValue="50" maxValue="200"',
        'pivotCacheRecords1.xml',
        'pivotCacheDefinition1.xml',
        'pivotTable1.xml',
    ]:
        assert any(needle in body for body in parts.values()), needle


def test_bad_field_name_raises() -> None:
    sheet = dict(PIVOT_SHEET)
    bad = dict(PIVOT_SHEET["pivots"][0])
    bad["rows"] = ["Nope"]
    sheet["pivots"] = [bad]
    with pytest.raises(Exception):
        write_excel_turbo_bytes([sheet], features="all")
