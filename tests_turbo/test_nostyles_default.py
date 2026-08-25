"""Absent-style default: an XLSX without xl/styles.xml.

With styles requested, turbo must expose a valid default style_table resolving
the implicit Excel style index 0. With styles not requested, the table must stay
None (selective loading — no manufactured/parsed styles).
"""

from __future__ import annotations

import sys
import zipfile
from io import BytesIO
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "python"))

from kyrax import read_excel_turbo  # noqa: E402


CONTENT_TYPES = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>"""

ROOT_RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"""

WORKBOOK = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
 xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets>
    <sheet name="NoStyles" sheetId="1" r:id="rId1"/>
  </sheets>
</workbook>"""

WB_RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"""

SHEET = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1">
      <c r="A1" t="inlineStr"><is><t>h1</t></is></c>
      <c r="B1" t="inlineStr"><is><t>h2</t></is></c>
    </row>
    <row r="2">
      <c r="A2" t="n"><v>1</v></c>
      <c r="B2" t="n"><v>2</v></c>
    </row>
    <row r="3">
      <c r="A3" t="n"><v>3</v></c>
      <c r="C3" t="n"><v>9</v></c>
    </row>
  </sheetData>
</worksheet>"""


@pytest.fixture
def nostyles_path(tmp_path: Path) -> Path:
    path = tmp_path / "nostyles_default.xlsx"
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("[Content_Types].xml", CONTENT_TYPES)
        z.writestr("_rels/.rels", ROOT_RELS)
        z.writestr("xl/workbook.xml", WORKBOOK)
        z.writestr("xl/_rels/workbook.xml.rels", WB_RELS)
        z.writestr("xl/worksheets/sheet1.xml", SHEET)
        # deliberately no xl/styles.xml
    return path


def test_nostyles_styles_requested_gets_default(nostyles_path: Path) -> None:
    sheet = read_excel_turbo(str(nostyles_path)).load_sheet(0, features="all")

    # Style indices present, all resolving to the implicit default index 0.
    si = sheet.style_indices()
    assert si is not None
    for c in range(sheet.ncols):
        for r in range(sheet.nrows):
            assert si[c][r] == 0

    # A valid default style_table resolves index 0 (was None before the fix).
    st = sheet.style_table()
    assert st is not None, "styles requested but absent styles.xml must yield a default table"
    assert st[0]["number_format"] == "General"
    assert st[0]["font"]["name"] == "Calibri"
    assert st[0]["fill"]["pattern"] == "none"


def test_nostyles_styles_not_requested_stays_none(nostyles_path: Path) -> None:
    # features=["values"] requests no styles -> no manufactured/parsed table.
    sheet = read_excel_turbo(str(nostyles_path)).load_sheet(0, features=["values"])
    assert sheet.style_table() is None
    assert sheet.style_indices() is None

    # Values are still readable.
    rb = sheet.to_arrow()
    assert sheet.column_names[0] == "h1"
    assert rb.column(0)[0].as_py() == 1.0
