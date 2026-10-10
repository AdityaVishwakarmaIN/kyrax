"""openpyxl-style sheet layout on kyrax's editable worksheet: column widths,
row heights, merged_cells, rows/columns, copy_worksheet layout, auto_filter
object and freeze_panes read-back. Every claim is a save -> reload round-trip,
on both a brand-new Workbook (default + created sheet) and a loaded file."""

from __future__ import annotations

import zipfile
from pathlib import Path

import kyrax


def _reload(wb, path: Path):
    wb.save(str(path))
    return kyrax.load_workbook(str(path))


def _sheet_xml(path: Path, n: int) -> str:
    with zipfile.ZipFile(path) as z:
        return z.read(f"xl/worksheets/sheet{n}.xml").decode()


def test_column_and_row_dimensions_roundtrip(tmp_path: Path) -> None:
    wb = kyrax.Workbook()
    ws = wb.active
    ws["A1"] = "x"
    assert ws.column_dimensions["A"].width is None
    assert ws.row_dimensions[1].height is None
    ws.column_dimensions["A"].width = 30
    ws.column_dimensions["C"].hidden = True
    ws.column_dimensions["D"].outline_level = 2
    ws.row_dimensions[1].height = 40
    ws.row_dimensions[3].hidden = True
    new = wb.create_sheet("New")
    new.column_dimensions["B"].width = 12.5
    new.row_dimensions[2].height = 22

    wb2 = _reload(wb, tmp_path / "a.xlsx")
    ws2 = wb2[ws.title]
    assert ws2.column_dimensions["A"].width == 30
    assert ws2.column_dimensions["C"].hidden is True
    assert ws2.column_dimensions["D"].outline_level == 2
    assert ws2.column_dimensions["B"].width is None
    assert ws2.row_dimensions[1].height == 40
    assert ws2.row_dimensions[3].hidden is True
    assert ws2["A1"].value == "x"
    assert wb2["New"].column_dimensions["B"].width == 12.5
    assert wb2["New"].row_dimensions[2].height == 22

    # Edit the loaded file: change one, keep the others.
    ws2.column_dimensions["A"].width = 8
    ws2.column_dimensions["C"].hidden = False
    ws2.row_dimensions[1].height = None
    ws2.row_dimensions[5].height = 18
    wb3 = _reload(wb2, tmp_path / "b.xlsx")
    ws3 = wb3[ws.title]
    assert ws3.column_dimensions["A"].width == 8
    assert ws3.column_dimensions["C"].hidden is False
    assert ws3.column_dimensions["D"].outline_level == 2
    assert ws3.row_dimensions[1].height is None
    assert ws3.row_dimensions[3].hidden is True
    assert ws3.row_dimensions[5].height == 18
    xml = _sheet_xml(tmp_path / "b.xlsx", 1)
    assert xml.index("<cols>") < xml.index("<sheetData")


def test_merged_cells_roundtrip(tmp_path: Path) -> None:
    wb = kyrax.Workbook()
    ws = wb.active
    ws.merge_cells("A1:C1")
    new = wb.create_sheet("New")
    new.merge_cells("B2:C3")
    assert [str(r) for r in ws.merged_cells.ranges] == ["A1:C1"]
    assert "A1:C1" in ws.merged_cells
    assert "B1" in ws.merged_cells
    assert "D1" not in ws.merged_cells

    wb2 = _reload(wb, tmp_path / "m.xlsx")
    ws2 = wb2[ws.title]
    assert [str(r) for r in ws2.merged_cells.ranges] == ["A1:C1"]
    assert [str(r) for r in wb2["New"].merged_cells.ranges] == ["B2:C3"]
    r = ws2.merged_cells.ranges[0]
    assert (r.min_row, r.min_col, r.max_row, r.max_col) == (1, 1, 1, 3)
    assert r == "A1:C1"

    ws2.unmerge_cells("A1:C1")
    ws2.merge_cells("A5:B6")
    assert [str(r) for r in ws2.merged_cells.ranges] == ["A5:B6"]
    wb3 = _reload(wb2, tmp_path / "m2.xlsx")
    assert [str(r) for r in wb3[ws.title].merged_cells.ranges] == ["A5:B6"]
    assert "A1:C1" not in wb3[ws.title].merged_cells


def test_rows_and_columns_properties(tmp_path: Path) -> None:
    wb = kyrax.Workbook()
    ws = wb.active
    ws.append([1, 2])
    ws.append([3, 4])
    ws2 = _reload(wb, tmp_path / "r.xlsx").active
    for sheet in (ws, ws2):
        assert [[c.value for c in row] for row in sheet.rows] == [[1, 2], [3, 4]]
        assert [[c.value for c in col] for col in sheet.columns] == [[1, 3], [2, 4]]


def test_copy_worksheet_copies_layout(tmp_path: Path) -> None:
    wb = kyrax.Workbook()
    ws = wb.active
    ws["A1"] = "h"
    ws.merge_cells("A1:B1")
    ws.column_dimensions["A"].width = 25
    ws.row_dimensions[2].height = 33
    wb2 = _reload(wb, tmp_path / "c.xlsx")
    src = wb2[ws.title]
    src.column_dimensions["B"].width = 7  # overlay edit on top of the file
    cp = wb2.copy_worksheet(src)
    wb3 = _reload(wb2, tmp_path / "c2.xlsx")
    c = wb3[cp.title]
    assert c["A1"].value == "h"
    assert [str(r) for r in c.merged_cells.ranges] == ["A1:B1"]
    assert c.column_dimensions["A"].width == 25
    assert c.column_dimensions["B"].width == 7
    assert c.row_dimensions[2].height == 33


def test_auto_filter_object_and_back_compat(tmp_path: Path) -> None:
    wb = kyrax.Workbook()
    ws = wb.active
    ws.append(["a", "b", "c"])
    assert ws.auto_filter.ref is None
    assert not ws.auto_filter
    ws.auto_filter.ref = "A1:C3"
    assert ws.auto_filter == "A1:C3"
    assert str(ws.auto_filter) == "A1:C3"
    new = wb.create_sheet("New")
    new.auto_filter = "A1:B2"  # legacy str setter still accepted
    assert new.auto_filter.ref == "A1:B2"

    wb2 = _reload(wb, tmp_path / "f.xlsx")
    assert wb2[ws.title].auto_filter.ref == "A1:C3"
    assert wb2[ws.title].auto_filter == "A1:C3"
    assert wb2["New"].auto_filter.ref == "A1:B2"
    wb2[ws.title].auto_filter.ref = "A1:C9"
    wb3 = _reload(wb2, tmp_path / "f2.xlsx")
    assert wb3[ws.title].auto_filter.ref == "A1:C9"


def test_freeze_panes_read_back_after_reload(tmp_path: Path) -> None:
    wb = kyrax.Workbook()
    ws = wb.active
    ws.freeze_panes = "B2"
    wb.create_sheet("New").freeze_panes = "A3"
    wb2 = _reload(wb, tmp_path / "p.xlsx")
    assert wb2[ws.title].freeze_panes == "B2"
    assert wb2["New"].freeze_panes == "A3"
    wb2[ws.title].freeze_panes = "C1"
    wb3 = _reload(wb2, tmp_path / "p2.xlsx")
    assert wb3[ws.title].freeze_panes == "C1"
