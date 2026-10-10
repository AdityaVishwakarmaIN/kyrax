"""Workbook-level openpyxl-familiar features on the editable workbook:
load from bytes / file-like, defined names, print area / titles, sheet state,
conveniences (index, close, iteration, membership, properties)."""

from __future__ import annotations

import io
import zipfile

import pytest

import kyrax


def _roundtrip(wb) -> object:
    buf = io.BytesIO()
    wb.save(buf)
    buf.seek(0)
    return kyrax.load_workbook(buf)


def _wb_xml(wb) -> str:
    buf = io.BytesIO()
    wb.save(buf)
    with zipfile.ZipFile(io.BytesIO(buf.getvalue())) as z:
        return z.read("xl/workbook.xml").decode()


def _two_sheet_book():
    wb = kyrax.Workbook()
    wb.active.title = "Data"
    wb["Data"]["A1"] = 1
    wb.create_sheet("Other")["B2"] = "x"
    return wb


def test_load_workbook_from_bytes_and_filelike(tmp_path):
    wb = _two_sheet_book()
    p = tmp_path / "a.xlsx"
    wb.save(str(p))
    raw = p.read_bytes()

    for src in (raw, bytearray(raw), io.BytesIO(raw)):
        got = kyrax.load_workbook(src)
        assert got.sheetnames == ["Data", "Other"]
        assert got["Data"]["A1"].value == 1
    with open(p, "rb") as fh:
        got = kyrax.load_workbook(fh)
    got["Other"]["C3"] = 5
    again = _roundtrip(got)
    assert again["Other"]["C3"].value == 5
    assert again["Other"]["B2"].value == "x"


def test_defined_names_roundtrip():
    wb = _two_sheet_book()
    dn = wb.defined_names
    assert len(dn) == 0 and "MyRange" not in dn
    dn["MyRange"] = "Data!$A$1:$C$3"
    dn["Rate"] = kyrax.WorkbookDefinedName("Rate", attr_text="0.05")
    assert "MyRange" in dn and len(dn) == 2

    got = _roundtrip(wb)
    names = got.defined_names
    assert sorted(names) == ["MyRange", "Rate"]
    d = names["MyRange"]
    assert (d.name, d.value, d.attr_text) == ("MyRange", "Data!$A$1:$C$3", "Data!$A$1:$C$3")
    assert names.get("missing") is None

    del names["Rate"]
    names["MyRange"] = "Data!$B$2"
    got2 = _roundtrip(got)
    assert list(got2.defined_names) == ["MyRange"]
    assert got2.defined_names["MyRange"].value == "Data!$B$2"
    with pytest.raises(KeyError):
        got2.defined_names["Rate"]


def test_print_area_and_titles_roundtrip():
    wb = _two_sheet_book()
    ws = wb["Other"]
    assert ws.print_area is None and ws.print_title_rows is None
    ws.print_area = "A1:C3"
    ws.print_title_rows = "1:1"
    ws.print_title_cols = "A:B"
    wb.defined_names["G"] = "Other!$A$1"

    got = _roundtrip(wb)
    o = got["Other"]
    assert o.print_area == "'Other'!$A$1:$C$3"
    assert o.print_title_rows == "1:1"
    assert o.print_title_cols == "A:B"
    assert got["Data"].print_area is None
    # sheet-scoped names are not global defined names
    assert list(got.defined_names) == ["G"]
    xml = _wb_xml(got)
    assert '<definedName name="_xlnm.Print_Area" localSheetId="1">' in xml

    # clearing one title half keeps the other; clearing the area drops it
    o.print_title_cols = None
    o.print_area = None
    got2 = _roundtrip(got)
    assert got2["Other"].print_area is None
    assert got2["Other"].print_title_rows == "1:1"
    assert got2["Other"].print_title_cols is None

    with pytest.raises(ValueError):
        got2["Other"].print_title_rows = "A:B"


def test_print_area_follows_sheet_rename_and_reorder():
    wb = _two_sheet_book()
    wb["Other"].print_area = "B2:D4"
    got = _roundtrip(wb)
    got["Other"].title = "Renamed"
    got.create_sheet("First")
    got.move_sheet("First", -2)
    got2 = _roundtrip(got)
    assert got2.sheetnames[0] == "First"
    assert got2["Renamed"].print_area == "'Renamed'!$B$2:$D$4"
    idx = got2.sheetnames.index("Renamed")
    assert f'localSheetId="{idx}"' in _wb_xml(got2)


def test_sheet_state_roundtrip():
    wb = _two_sheet_book()
    wb.create_sheet("Secret")
    assert wb["Other"].sheet_state == "visible"
    wb["Other"].sheet_state = "hidden"
    wb["Secret"].sheet_state = "veryHidden"
    got = _roundtrip(wb)
    assert got["Other"].sheet_state == "hidden"
    assert got["Secret"].sheet_state == "veryHidden"
    assert got["Data"].sheet_state == "visible"

    # states survive an unrelated structural edit (new sheet) on reload
    got.create_sheet("More")
    got2 = _roundtrip(got)
    assert got2["Other"].sheet_state == "hidden"
    got2["Other"].sheet_state = "visible"
    assert _roundtrip(got2)["Other"].sheet_state == "visible"

    with pytest.raises(ValueError):
        got2["Data"].sheet_state = "bogus"
    one = kyrax.Workbook()
    with pytest.raises(ValueError):
        one.active.sheet_state = "hidden"


def test_hidden_active_sheet_repoints_active_tab():
    wb = _two_sheet_book()
    wb["Data"].sheet_state = "hidden"
    xml = _wb_xml(wb)
    assert 'activeTab="1"' in xml


def test_workbook_conveniences():
    wb = _two_sheet_book()
    assert len(wb.sheetnames) == 2 and len(wb) == 2
    assert [ws.title for ws in wb] == ["Data", "Other"]
    assert "Data" in wb and "Nope" not in wb and wb["Other"] in wb
    assert wb.index(wb["Other"]) == 1
    assert wb.get_sheet_by_name("Data").title == "Data"
    assert wb.close() is None
    props = wb.properties
    assert props.creator is not None
    assert props.created is not None
    with pytest.raises(AttributeError):
        props.title = "x"


def test_untouched_workbook_xml_is_byte_preserved():
    wb = _two_sheet_book()
    wb.defined_names["Keep"] = "Data!$A$1"
    buf = io.BytesIO()
    wb.save(buf)
    src = buf.getvalue()
    got = kyrax.load_workbook(src)
    _ = got.defined_names["Keep"].value, got["Data"].sheet_state  # read-only access
    got["Data"]["Z9"] = 1
    with zipfile.ZipFile(io.BytesIO(src)) as z:
        before = z.read("xl/workbook.xml")
    out = io.BytesIO()
    got.save(out)
    with zipfile.ZipFile(io.BytesIO(out.getvalue())) as z:
        after = z.read("xl/workbook.xml")
    assert before == after
