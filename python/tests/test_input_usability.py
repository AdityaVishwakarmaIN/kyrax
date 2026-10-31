"""Input-usability regressions: table refs, row lookup, and runtime help."""

from __future__ import annotations

import kyrax
import pytest
from kyrax.worksheet.table import Table


def test_turbo_table_ref_requires_a_data_row():
    with pytest.raises(ValueError, match="header row and at least one data row"):
        kyrax.write_excel_turbo_bytes(
            [
                {
                    "name": "S",
                    "rows": [["A", "B"]],
                    "tables": [{"name": "T", "ref": "A1:B1", "columns": ["A", "B"]}],
                }
            ]
        )


def test_editable_table_ref_requires_a_data_row():
    ws = kyrax.Workbook().active
    ws.append(["A", "B"])
    with pytest.raises(ValueError, match="header row and at least one data row"):
        ws.add_table(Table(displayName="T", ref="A1:B1"))


def test_ws_row_lookup_and_a1_regressions():
    ws = kyrax.Workbook().active
    ws["A1"] = "h1"
    ws["B1"] = "h2"
    ws["A2"] = 1
    ws["B2"] = 2
    ws["A3"] = 3
    ws["B3"] = 4

    row = ws[3]
    assert isinstance(row, tuple)
    assert [c.coordinate for c in row] == ["A3", "B3"]
    assert [c.value for c in row] == [3, 4]

    cell = ws["A1"]
    assert cell.coordinate == "A1"
    assert cell.value == "h1"

    block = ws["A1:B2"]
    assert isinstance(block, tuple) and len(block) == 2
    assert [[c.coordinate for c in r] for r in block] == [["A1", "B1"], ["A2", "B2"]]
    assert [[c.value for c in r] for r in block] == [["h1", "h2"], [1, 2]]


def test_ws_row_lookup_rejects_invalid_rows():
    ws = kyrax.Workbook().active
    ws["A1"] = 1
    for bad_row in (0, -1, 1_048_577):
        with pytest.raises(ValueError, match="out of grid"):
            ws[bad_row]


def test_editable_methods_have_runtime_help():
    for method in (
        kyrax.EditableSheet.cell,
        kyrax.EditableSheet.append,
        kyrax.EditableSheet.iter_rows,
        kyrax.EditableSheet.merge_cells,
        kyrax.EditableSheet.insert_rows,
        kyrax.EditableSheet.delete_rows,
        kyrax.EditableSheet.insert_cols,
        kyrax.EditableSheet.delete_cols,
        kyrax.EditableSheet.move_range,
        kyrax.EditableWorkbook.create_sheet,
    ):
        assert method.__doc__ and method.__doc__.strip()
