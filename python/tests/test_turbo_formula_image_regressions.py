import base64
from xml.etree import ElementTree as ET
from zipfile import ZipFile

import kyrax
import openpyxl
import pytest

NS = {"x": "http://schemas.openxmlformats.org/spreadsheetml/2006/main"}
PNG = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNgYGBgAAAABQABeqhXUAAAAABJRU5ErkJggg=="
)


def sheet_xml(path):
    with ZipFile(path) as archive:
        return ET.fromstring(archive.read("xl/worksheets/sheet1.xml"))


@pytest.mark.parametrize("as_list", [False, True])
@pytest.mark.parametrize("cache", [None, 0])
def test_formula_cache_none_and_zero(tmp_path, as_list, cache):
    path = tmp_path / "formula.xlsx"
    formula = {"text": "1+2", "cached": cache}
    formulas = [dict(formula, row=0, col=0)] if as_list else {(0, 0): formula}
    kyrax.write_excel_turbo(path, [{"name": "S", "formulas": formulas}])
    cell = sheet_xml(path).find(".//x:c", NS)
    value = cell.find("x:v", NS)
    if cache is None:
        assert value is None or value.text is None
    else:
        assert value is not None and float(value.text) == 0
    assert openpyxl.load_workbook(path, data_only=True).active["A1"].value == cache


@pytest.mark.parametrize("as_list", [False, True])
@pytest.mark.parametrize("row,col,field", [(1048576, 0, "row"), (0, 16384, "col")])
def test_formula_outside_grid_rejected(tmp_path, as_list, row, col, field):
    formulas = [{"row": row, "col": col, "text": "1"}] if as_list else {(row, col): "1"}
    with pytest.raises(ValueError, match=field):
        kyrax.write_excel_turbo(tmp_path / "bad.xlsx", [{"name": "S", "formulas": formulas}])


@pytest.mark.parametrize("row,col,cell", [(1048575, 0, "A1048576"), (0, 16383, "XFD1")])
def test_formula_grid_boundary_accepted(tmp_path, row, col, cell):
    path = tmp_path / "boundary.xlsx"
    kyrax.write_excel_turbo(path, [{"name": "S", "formulas": {(row, col): "1"}}])
    assert sheet_xml(path).find(".//x:c", NS).attrib["r"] == cell


def test_image_only_sheet_is_valid_xml_and_loadable(tmp_path):
    path = tmp_path / "image.xlsx"
    kyrax.write_excel_turbo(path, [{"name": "S", "rows": [["a"]], "images": [{"data": PNG}]}])
    sheet_xml(path)
    assert len(openpyxl.load_workbook(path).active._images) == 1


@pytest.mark.parametrize("cut", [8, 20, len(PNG) - 1], ids=["signature", "header", "missing-iend"])
def test_truncated_png_rejected(tmp_path, cut):
    with pytest.raises(ValueError, match="truncated PNG"):
        kyrax.write_excel_turbo(
            tmp_path / "bad.xlsx",
            [{"name": "S", "rows": [["a"]], "images": [{"data": PNG[:cut]}]}],
        )
