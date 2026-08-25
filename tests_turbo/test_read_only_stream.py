"""Read-only streaming doorway: grid semantics moved into Rust.

Covers the public read_only surface preserved by the architecture fix:
iter_rows bounds/header/values_only, cell lookup, repeated iteration, close,
workbook lifecycle, and error contracts.
"""

import pytest

import kyrax


def _author(path, *, sheets=None):
    kyrax.write_excel_turbo(
        str(path),
        sheets
        or [
            {
                "name": "S1",
                "rows": [
                    ["h1", "h2", "h3"],
                    [1.0, 2.0, 3.0],
                    [4.0, 5.0, 6.0],
                    [7.0, 8.0, 9.0],
                ],
            },
            {"name": "S2", "rows": [["x", "y"], [10.0, 20.0]]},
        ],
    )


def _open(path):
    return kyrax.load_workbook(str(path), read_only=True)


def test_read_only_grid_full_iteration(tmp_path):
    path = tmp_path / "a.xlsx"
    _author(path)
    wb = _open(path)
    ws = wb["S1"]
    rows = list(ws.iter_rows(values_only=True))
    # Grid semantics: row 1 is the header row and IS yielded.
    assert rows == [
        ("h1", "h2", "h3"),
        (1.0, 2.0, 3.0),
        (4.0, 5.0, 6.0),
        (7.0, 8.0, 9.0),
    ]
    wb.close()


def test_read_only_values_property(tmp_path):
    path = tmp_path / "a.xlsx"
    _author(path)
    wb = _open(path)
    ws = wb["S1"]
    assert list(ws.values) == [
        ("h1", "h2", "h3"),
        (1.0, 2.0, 3.0),
        (4.0, 5.0, 6.0),
        (7.0, 8.0, 9.0),
    ]
    wb.close()


def test_read_only_row_bounds(tmp_path):
    path = tmp_path / "a.xlsx"
    _author(path)
    wb = _open(path)
    ws = wb["S1"]
    assert list(ws.iter_rows(min_row=2, max_row=3, values_only=True)) == [
        (1.0, 2.0, 3.0),
        (4.0, 5.0, 6.0),
    ]
    assert list(ws.iter_rows(min_row=4, values_only=True)) == [
        (7.0, 8.0, 9.0)
    ]
    assert list(ws.iter_rows(max_row=1, values_only=True)) == [("h1", "h2", "h3")]
    wb.close()


def test_read_only_column_bounds(tmp_path):
    path = tmp_path / "a.xlsx"
    _author(path)
    wb = _open(path)
    ws = wb["S1"]
    assert list(ws.iter_rows(min_col=2, max_col=2, values_only=True)) == [
        ("h2",),
        (2.0,),
        (5.0,),
        (8.0,),
    ]
    assert list(ws.iter_rows(min_col=2, values_only=True)) == [
        ("h2", "h3"),
        (2.0, 3.0),
        (5.0, 6.0),
        (8.0, 9.0),
    ]
    # Header excluded when row 1 is outside the requested window.
    assert list(ws.iter_rows(min_row=2, min_col=3, max_col=3, values_only=True)) == [
        (3.0,),
        (6.0,),
        (9.0,),
    ]
    wb.close()


def test_read_only_row_and_col_combined(tmp_path):
    path = tmp_path / "a.xlsx"
    _author(path)
    wb = _open(path)
    ws = wb["S1"]
    assert list(
        ws.iter_rows(min_row=2, max_row=3, min_col=2, max_col=2, values_only=True)
    ) == [(2.0,), (5.0,)]
    # Out-of-range window yields nothing.
    assert list(ws.iter_rows(min_row=99, values_only=True)) == []
    assert list(ws.iter_rows(min_col=99, values_only=True)) == [(), (), (), ()]
    wb.close()


def test_read_only_cell_lookup(tmp_path):
    path = tmp_path / "a.xlsx"
    _author(path)
    wb = _open(path)
    ws = wb["S1"]
    assert ws.cell(1, 1).value == "h1"
    assert ws.cell(1, 3).value == "h3"
    assert ws.cell(2, 2).value == 2.0
    assert ws.cell(3, 2).value == 5.0
    assert ws.cell(4, 3).value == 9.0
    assert ws.cell(4, 1).value == 7.0
    assert ws.cell(1, 1).row == 1
    assert ws.cell(1, 1).column == 1
    # Out of range -> None (openpyxl ReadOnlyWorksheet parity).
    assert ws.cell(99, 1).value is None
    assert ws.cell(2, 99).value is None
    wb.close()


def test_read_only_cell_header_row(tmp_path):
    path = tmp_path / "a.xlsx"
    _author(path)
    wb = _open(path)
    ws = wb["S1"]
    assert ws.cell(1, 2).value == "h2"


def test_read_only_values_only_mandatory(tmp_path):
    path = tmp_path / "a.xlsx"
    _author(path)
    wb = _open(path)
    ws = wb["S1"]
    with pytest.raises(NotImplementedError):
        list(ws.iter_rows())


def test_read_only_repeated_iteration(tmp_path):
    path = tmp_path / "a.xlsx"
    _author(path)
    wb = _open(path)
    ws = wb["S1"]
    first = list(ws.iter_rows(values_only=True))
    second = list(ws.iter_rows(values_only=True))
    assert first == second
    wb.close()


def test_read_only_sheet_close_mid_iteration(tmp_path):
    path = tmp_path / "a.xlsx"
    _author(path)
    wb = _open(path)
    ws = wb["S1"]
    it = ws.iter_rows(values_only=True)
    first = next(it)
    assert first == ("h1", "h2", "h3")
    ws.close()
    with pytest.raises(StopIteration):
        next(it)
    with pytest.raises(ValueError, match="closed"):
        list(ws.iter_rows(values_only=True))
    wb.close()


def test_read_only_workbook_close_mid_iteration(tmp_path):
    path = tmp_path / "a.xlsx"
    _author(path)
    wb = _open(path)
    ws = wb["S1"]
    it = ws.iter_rows(values_only=True)
    assert next(it) == ("h1", "h2", "h3")
    wb.close()
    with pytest.raises(StopIteration):
        next(it)
    with pytest.raises(ValueError, match="closed"):
        list(ws.iter_rows(values_only=True))
    with pytest.raises(ValueError, match="closed"):
        wb.sheetnames
    # Closing twice is a no-op.
    wb.close()


def test_read_only_context_manager(tmp_path):
    path = tmp_path / "a.xlsx"
    _author(path)
    with _open(path) as wb:
        ws = wb["S1"]
        assert list(ws.iter_rows(values_only=True))[0] == ("h1", "h2", "h3")
    with pytest.raises(ValueError, match="closed"):
        wb.sheetnames


def test_read_only_workbook_sheet_access(tmp_path):
    path = tmp_path / "a.xlsx"
    _author(path)
    wb = _open(path)
    assert wb.sheetnames == ["S1", "S2"]
    assert wb["S2"].cell(1, 1).value == "x"
    assert wb["S2"].cell(2, 2).value == 20.0
    with pytest.raises(KeyError):
        wb["Nope"]
    wb.close()