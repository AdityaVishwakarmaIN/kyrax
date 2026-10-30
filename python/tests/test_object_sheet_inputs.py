import io
from types import SimpleNamespace
from zipfile import ZipFile
import xml.etree.ElementTree as ET

import pytest
import kyrax


def test_object_sheet_rows_and_merges():
    sheet = SimpleNamespace(name="S", rows=[["x"]], merges=["A1:B1"])
    data = kyrax.write_excel_turbo_bytes([sheet])
    with ZipFile(io.BytesIO(data)) as archive:
        root = ET.fromstring(archive.read("xl/worksheets/sheet1.xml"))
    ns = {"x": "http://schemas.openxmlformats.org/spreadsheetml/2006/main"}
    assert root.find(".//x:sheetData/x:row/x:c[@r='A1']", ns) is not None
    assert root.find(".//x:mergeCell[@ref='A1:B1']", ns) is not None


def test_object_sheet_ignores_unrelated_attributes():
    sheet = SimpleNamespace(name="S", rows=[["x"]], metadata="unrelated")
    assert kyrax.write_excel_turbo_bytes([sheet])


@pytest.mark.parametrize("sheet", [
    SimpleNamespace(name="S", columns=None, data=[[42]]),
    SimpleNamespace(name="S", rows=None, rows_iter=[[42]]),
])
def test_object_sheet_none_alias_does_not_hide_data(sheet):
    openpyxl = pytest.importorskip("openpyxl")
    workbook = openpyxl.load_workbook(io.BytesIO(kyrax.write_excel_turbo_bytes([sheet])))
    assert workbook.active["A1"].value == 42


def test_object_sheet_named_styles():
    sheet = SimpleNamespace(name="S", named_styles=[{"name": "CustomStyle"}])
    data = kyrax.write_excel_turbo_bytes([sheet])
    with ZipFile(io.BytesIO(data)) as archive:
        assert b'CustomStyle' in archive.read("xl/styles.xml")


def test_object_sheet_grid():
    np = pytest.importorskip("numpy")
    openpyxl = pytest.importorskip("openpyxl")
    sheet = SimpleNamespace(name="S", grid=np.array([[42.0]]))
    workbook = openpyxl.load_workbook(io.BytesIO(kyrax.write_excel_turbo_bytes([sheet])))
    assert workbook.active["A1"].value == 42
