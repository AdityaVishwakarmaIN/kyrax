"""Small independently specified OOXML cases for the typed grid reader."""

from __future__ import annotations

import datetime as dt
import zipfile

import kyrax
import pytest


def _book(tmp_path, rows, dimension="A1:B3"):
    path = tmp_path / "grid.xlsx"
    kyrax.Workbook().save(str(path))
    with zipfile.ZipFile(path) as archive:
        parts = {name: archive.read(name) for name in archive.namelist()}
    parts["xl/worksheets/sheet1.xml"] = (
        '<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">'
        + (f'<dimension ref="{dimension}"/>' if dimension else "")
        + f"<sheetData>{rows}</sheetData></worksheet>"
    ).encode()
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as archive:
        for name, contents in parts.items():
            archive.writestr(name, contents)
    return path


@pytest.mark.parametrize("dimension", [None, "A1", "A1:D20"])
def test_empty_sheet_has_no_phantom_rows(tmp_path, dimension):
    with kyrax.load_workbook(_book(tmp_path, "", dimension), read_only=True) as wb:
        assert list(wb.active.values) == []
        assert list(wb.active.iter_rows(max_row=3, values_only=True)) == []
        assert wb.active.cell(1, 1).value is None


def test_sparse_rows_and_requested_bounds_stop_at_physical_eof(tmp_path):
    rows = '<row r="3"><c r="B3"><v>7</v></c></row>'
    with kyrax.load_workbook(_book(tmp_path, rows, "A1:B20"), read_only=True) as wb:
        ws = wb.active
        assert list(ws.values) == [(None, None), (None, None), (None, 7)]
        assert list(ws.iter_rows(min_row=5, max_row=10, values_only=True)) == []
        assert list(ws.iter_rows(min_row=2, max_row=10, min_col=2, max_col=3,
                                 values_only=True)) == [(None, None), (7, None)]


def test_shared_formulas_resolve_before_requested_window(tmp_path):
    rows = (
        '<row r="1"><c r="B1"><f t="shared" si="0" ref="B1:B3">'
        'A1+$A$1</f><v>2</v></c></row>'
        '<row r="3"><c r="B3"><f t="shared" si="0"/><v>4</v></c></row>'
    )
    path = _book(tmp_path, rows)
    with kyrax.load_workbook(path, read_only=True) as wb:
        assert list(wb.active.iter_rows(min_row=3, min_col=2, values_only=True)) == [
            ("=A3+$A$1",)
        ]
        assert wb.active.cell(3, 2).value == "=A3+$A$1"
    with kyrax.load_workbook(path, read_only=True, data_only=True) as wb:
        assert wb.active.cell(3, 2).value == 4


def test_numeric_boolean_whitespace_and_text_whitespace(tmp_path):
    rows = (
        '<row r="1"><c r="A1"><v> 42 </v></c><c r="B1" t="b"><v> 1 </v></c>'
        '<c r="C1" t="str"><v> text </v></c></row>'
    )
    with kyrax.load_workbook(_book(tmp_path, rows, "A1:C1"), read_only=True) as wb:
        assert list(wb.active.values) == [(42, True, " text ")]


def test_rich_inline_text_excludes_phonetic_annotations(tmp_path):
    rows = (
        '<row r="1"><c r="A1" t="inlineStr"><is>'
        '<r><t>東京</t></r><rPh sb="0" eb="2"><t>とうきょう</t></rPh>'
        '<r><t xml:space="preserve"> &amp; tail</t></r></is></c></row>'
    )
    with kyrax.load_workbook(_book(tmp_path, rows, "A1"), read_only=True) as wb:
        assert list(wb.active.values) == [("東京 & tail",)]


def test_iso_date_time_and_datetime_cells(tmp_path):
    rows = (
        '<row r="1"><c r="A1" t="d"><v>2024-01-02</v></c>'
        '<c r="B1" t="d"><v>03:04:05.123</v></c>'
        '<c r="C1" t="d"><v>2024-01-02T03:04:05.123Z</v></c></row>'
    )
    with kyrax.load_workbook(_book(tmp_path, rows, "A1:C1"), read_only=True) as wb:
        assert list(wb.active.values) == [(
            dt.date(2024, 1, 2), dt.time(3, 4, 5, 123000),
            dt.datetime(2024, 1, 2, 3, 4, 5, 123000),
        )]
