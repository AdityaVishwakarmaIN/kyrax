"""Editable-API authoring of conditional formatting, data validation, tables."""

from __future__ import annotations

import re
import zipfile

import pytest

import kyrax
from kyrax.formatting.rule import CellIsRule, ColorScaleRule, DataBarRule, FormulaRule
from kyrax.styles import Font, PatternFill
from kyrax.worksheet.datavalidation import DataValidation
from kyrax.worksheet.table import Table, TableStyleInfo


def _fill_data(ws) -> None:
    for c, h in enumerate(["Name", "Qty", "Price"], start=1):
        ws.cell(row=1, column=c, value=h)
    for r in range(2, 6):
        ws.cell(row=r, column=1, value=f"item{r}")
        ws.cell(row=r, column=2, value=r * 3)
        ws.cell(row=r, column=3, value=r * 1.5)


def _decorate(ws) -> None:
    red = PatternFill(start_color="FFC7CE", end_color="FFC7CE", fill_type="solid")
    ws.conditional_formatting.add("B2:B5", CellIsRule(operator=">", formula=["10"], fill=red))
    ws.conditional_formatting.add("B2:B5", FormulaRule(formula=["B2<5"], font=Font(bold=True)))
    ws.conditional_formatting.add(
        "C2:C5", ColorScaleRule(start_type="min", start_color="F8696B", end_type="max", end_color="63BE7B")
    )
    ws.conditional_formatting.add(
        "C2:C5", DataBarRule(start_type="min", end_type="max", color="638EC6")
    )
    dv = DataValidation(type="list", formula1='"Yes,No"', allow_blank=True)
    ws.add_data_validation(dv)
    dv.add("E2:E5")  # openpyxl allows adding ranges after registration
    ws.add_table(
        Table(
            displayName="Stock",
            ref="A1:C5",
            tableStyleInfo=TableStyleInfo(name="TableStyleMedium9", showRowStripes=True),
        )
    )


def _sheet_part(path, idx=1) -> str:
    with zipfile.ZipFile(path) as z:
        return z.read(f"xl/worksheets/sheet{idx}.xml").decode()


def _assert_package(path, n_tables=1) -> None:
    with zipfile.ZipFile(path) as z:
        names = z.namelist()
        assert len(names) == len(set(names)), "duplicate zip entries"
        ct = z.read("[Content_Types].xml").decode()
        styles = z.read("xl/styles.xml").decode()
        tables = [n for n in names if re.fullmatch(r"xl/tables/table\d+\.xml", n)]
        ids = [re.search(r'<table [^>]*\bid="(\d+)"', z.read(t).decode()).group(1) for t in tables]
    assert len(tables) == n_tables and len(set(ids)) == n_tables
    for t in tables:
        assert f'PartName="/{t}"' in ct
    m = re.search(r'<dxfs count="(\d+)">(.*?)</dxfs>', styles)
    assert m and int(m.group(1)) == m.group(2).count("<dxf>") >= 2


def _assert_order(xml: str) -> None:
    pos = [xml.find(t) for t in ("<conditionalFormatting", "<dataValidations", "<tableParts")]
    assert all(p > 0 for p in pos) and pos == sorted(pos), pos
    assert xml.index("</sheetData>") < pos[0]
    assert re.search(r'<cfRule type="cellIs"[^>]*dxfId="\d+"', xml)


def _assert_reloaded(path) -> None:
    ws = kyrax.load_workbook(path).active
    assert ws.tables == {"Stock": "A1:C5"}
    cf = list(ws.conditional_formatting)
    assert len(ws.conditional_formatting) == 2
    assert {c["sqref"]: [r["type"] for r in c["rules"]] for c in cf} == {
        "B2:B5": ["cellIs", "expression"],
        "C2:C5": ["colorScale", "dataBar"],
    }
    dvs = ws.data_validations
    assert [(d["sqref"], d["formula1"]) for d in dvs] == [("E2:E5", '"Yes,No"')]
    assert ws["A1"].value == "Name" and ws["B3"].value == 9


def test_new_workbook_cf_dv_table(tmp_path):
    wb = kyrax.Workbook()
    ws = wb.active
    _fill_data(ws)
    _decorate(ws)
    out = tmp_path / "new.xlsx"
    wb.save(str(out))
    _assert_package(out)
    xml = _sheet_part(out)
    _assert_order(xml)
    with zipfile.ZipFile(out) as z:
        table = z.read("xl/tables/table1.xml").decode()
    assert '<tableColumn id="1" name="Name"/>' in table and 'name="Price"' in table
    assert 'name="TableStyleMedium9"' in table
    _assert_reloaded(out)


def test_existing_file_cf_dv_table(tmp_path):
    src = tmp_path / "src.xlsx"
    wb = kyrax.Workbook()
    _fill_data(wb.active)
    wb.save(str(src))
    wb = kyrax.load_workbook(str(src))
    _decorate(wb.active)
    out = tmp_path / "edited.xlsx"
    wb.save(str(out))
    _assert_package(out)
    _assert_order(_sheet_part(out))
    _assert_reloaded(out)


def test_existing_table_kept_and_names_unique(tmp_path):
    src = tmp_path / "t1.xlsx"
    wb = kyrax.Workbook()
    ws = wb.active
    _fill_data(ws)
    ws.add_table(Table(displayName="First", ref="A1:C5"))
    wb.save(str(src))

    wb = kyrax.load_workbook(str(src))
    ws = wb.active
    assert ws.tables == {"First": "A1:C5"}
    with pytest.raises(Exception, match="already exists"):
        ws.add_table(Table(displayName="first", ref="H1:I3"))
    with pytest.raises(Exception, match="overlaps"):
        ws.add_table(Table(displayName="Clash", ref="B2:D9"))
    # Blank header cells get Excel's ColumnN names written back.
    ws["H2"] = 1
    ws["I2"] = 2
    ws.add_table(Table(displayName="Second", ref="H1:I2"))
    ws["Z1"] = "edit elsewhere"
    out = tmp_path / "t2.xlsx"
    wb.save(str(out))

    with zipfile.ZipFile(out) as z:
        ct = z.read("[Content_Types].xml").decode()
        rels = z.read("xl/worksheets/_rels/sheet1.xml.rels").decode()
        t2 = z.read("xl/tables/table2.xml").decode()
    assert ct.count("spreadsheetml.table+xml") == 2
    assert rels.count("relationships/table") == 2
    assert 'name="Column1"' in t2 and 'name="Column2"' in t2
    xml = _sheet_part(out)
    assert '<tableParts count="2">' in xml and xml.count("<tablePart ") == 2
    ws = kyrax.load_workbook(str(out)).active
    assert ws.tables == {"First": "A1:C5", "Second": "H1:I2"}
    assert ws["H1"].value == "Column1"


def test_created_sheet_dv_and_table(tmp_path):
    wb = kyrax.Workbook()
    _fill_data(wb.active)
    wb.active.add_table(Table(displayName="One", ref="A1:C5"))
    ws2 = wb.create_sheet("Two")
    _fill_data(ws2)
    ws2.add_table(Table(displayName="TwoTable", ref="A1:C5"))
    ws2.add_data_validation(DataValidation(type="whole", formula1="0", sqref="B2:B5"))
    out = tmp_path / "two.xlsx"
    wb.save(str(out))
    xml = _sheet_part(out, 2)
    assert xml.index("<dataValidations") < xml.index("<tableParts")
    with zipfile.ZipFile(out) as z:
        ids = sorted(re.search(r'\bid="(\d+)"', z.read(f"xl/tables/table{i}.xml").decode()).group(1) for i in (1, 2))
    assert ids == ["1", "2"]
    ws2 = kyrax.load_workbook(str(out))["Two"]
    assert ws2.tables == {"TwoTable": "A1:C5"} and len(ws2.data_validations) == 1


def test_add_data_validation_keeps_existing(tmp_path):
    src = tmp_path / "dv.xlsx"
    wb = kyrax.Workbook()
    wb.active.add_data_validation(
        DataValidation(type="whole", operator="between", formula1="1", formula2="9", sqref="A1:A3")
    )
    wb.save(str(src))
    wb = kyrax.load_workbook(str(src))
    dv = DataValidation(type="list", formula1='"a,b"')
    dv.add("B1")
    wb.active.add_data_validation(dv)
    out = tmp_path / "dv2.xlsx"
    wb.save(str(out))
    dvs = kyrax.load_workbook(str(out)).active.data_validations
    assert [(d["sqref"], d.get("type")) for d in dvs] == [("A1:A3", "whole"), ("B1", "list")]
