"""Turbo io CSV tests: sheet <-> csv export/import (path + bytes variants)."""

from __future__ import annotations

from datetime import datetime
from pathlib import Path

import kyrax
import pytest
from kyrax import io as kio
from kyrax import write_excel_turbo

openpyxl = pytest.importorskip("openpyxl")


def _make(path: Path, columns: list[list], name: str = "Data") -> Path:
    write_excel_turbo(str(path), [{"name": name, "columns": columns}])
    return path


def _load(path: Path):
    return openpyxl.load_workbook(path, data_only=True)


# ---------------------------------------------------------------------------
# Export
# ---------------------------------------------------------------------------


def test_sheet_to_csv_headers_and_numbers(tmp_path: Path):
    # Row 1 of the grid is the turbo reader's header slot: it becomes the first
    # CSV line; data rows follow. Integer-valued floats lose the ".0".
    path = _make(tmp_path / "src.xlsx", [["Region", "East", "West"], ["Amount", 1.5, 2.0]])
    text = kio.sheet_to_csv_bytes(str(path), "Data")
    assert text == b"Region,Amount\r\nEast,1.5\r\nWest,2\r\n"

    out = tmp_path / "out.csv"
    kio.sheet_to_csv(str(path), "Data", str(out))
    assert out.read_bytes() == text


def test_sheet_to_csv_quoting(tmp_path: Path):
    # A field is quoted iff it contains the delimiter, the quote byte, or a
    # newline; embedded quotes are doubled. Quoted-empty ("") is distinct from
    # blank.
    path = _make(
        tmp_path / "src.xlsx",
        [
            ["Name", "Ada", "Lin"],
            ["Quote", "a,b", 'he said "hi"'],
            ["Blank", "", None],
        ],
    )
    text = kio.sheet_to_csv_bytes(str(path), "Data")
    assert text == b'Name,Quote,Blank\r\nAda,"a,b",""\r\nLin,"he said ""hi""",\r\n'


def test_sheet_to_csv_has_header_does_not_change_bytes(tmp_path: Path):
    path = _make(tmp_path / "src.xlsx", [["Region", "East"], ["Amount", 1.5]])
    assert kio.sheet_to_csv_bytes(str(path), "Data", has_header=False) == (
        kio.sheet_to_csv_bytes(str(path), "Data", has_header=True)
    )


def test_sheet_to_csv_dates(tmp_path: Path):
    # Date-styled numeric cells render per the date_format pattern, never as
    # raw serials. Authored via openpyxl so the number format is date-styled.
    path = tmp_path / "src.xlsx"
    wb = openpyxl.Workbook()
    ws = wb.active
    ws.title = "Data"
    ws["A1"] = "When"
    ws["A2"] = datetime(2021, 3, 4, 5, 6, 7)
    wb.save(path)
    assert kio.sheet_to_csv_bytes(str(path), "Data") == b"When\r\n2021-03-04 05:06:07\r\n"


def test_sheet_to_csv_mixed_column_keeps_dot_zero(tmp_path: Path):
    # A column that mixes numbers and text pre-renders numbers as text, so an
    # integral value keeps its ".0" — documented in io/csv.rs.
    path = _make(tmp_path / "src.xlsx", [["H", "a", "b"], ["Mixed", 1, "x"]])
    assert kio.sheet_to_csv_bytes(str(path), "Data") == b"H,Mixed\r\na,1.0\r\nb,x\r\n"


def test_sheet_to_csv_sheet_not_found(tmp_path: Path):
    path = _make(tmp_path / "src.xlsx", [["A", "a"]])
    with pytest.raises(kyrax.SheetNotFoundError):
        kio.sheet_to_csv_bytes(str(path), "Nope")


def test_sheet_to_csv_bad_options(tmp_path: Path):
    path = _make(tmp_path / "src.xlsx", [["A", "a"]])
    with pytest.raises(ValueError):
        kio.sheet_to_csv_bytes(str(path), "Data", delimiter="::")
    with pytest.raises(ValueError):
        kio.sheet_to_csv_bytes(str(path), "Data", quote="")


# ---------------------------------------------------------------------------
# Import
# ---------------------------------------------------------------------------


def test_csv_to_sheet_roundtrip(tmp_path: Path):
    csv_path = tmp_path / "in.csv"
    csv_path.write_text("a,b\n1,2\n3,4\n")
    out = tmp_path / "out.xlsx"
    kio.csv_to_sheet(str(csv_path), str(out), "S")
    ws = _load(out)["S"]
    assert ws["A1"].value == "a"
    assert ws["B1"].value == "b"
    assert ws["A2"].value == "1"  # infer_types=False keeps everything text
    assert ws["A3"].value == "3"
    assert ws["B3"].value == "4"


def test_csv_bytes_to_sheet_matches_path(tmp_path: Path):
    data = b"a,b\n1,2\n"
    out_a = tmp_path / "out_a.xlsx"
    out_b = tmp_path / "out_b.xlsx"
    in_path = tmp_path / "in.csv"
    in_path.write_bytes(data)
    kio.csv_bytes_to_sheet(data, str(out_a), "S")
    kio.csv_to_sheet(str(in_path), str(out_b), "S")
    assert out_a.read_bytes() == out_b.read_bytes()


def test_csv_to_sheet_infer_types(tmp_path: Path):
    # Exact, documented inference: leading-zero forms and integers beyond 2^53
    # stay text even with infer_types=True.
    csv_path = tmp_path / "in.csv"
    csv_path.write_text("007,12345678901234567890,1.5,2\n")
    out = tmp_path / "out.xlsx"
    kio.csv_to_sheet(str(csv_path), str(out), "S", infer_types=True, has_header=False)
    ws = _load(out)["S"]
    assert ws["A1"].value == "007"
    assert ws["B1"].value == "12345678901234567890"
    assert ws["C1"].value == 1.5
    assert ws["D1"].value == 2


def test_csv_to_sheet_quoted_empty_vs_blank(tmp_path: Path):
    csv_path = tmp_path / "in.csv"
    csv_path.write_text('h1,h2\n"",\n')
    out = tmp_path / "out.xlsx"
    kio.csv_to_sheet(str(csv_path), str(out), "S")
    ws = _load(out)["S"]
    assert ws["A2"].value == ""  # quoted empty -> empty-string cell
    assert ws["B2"].value is None  # unquoted empty -> blank cell


def test_csv_to_sheet_blank_line_is_a_row(tmp_path: Path):
    csv_path = tmp_path / "in.csv"
    csv_path.write_text("a\n\nb\n")
    out = tmp_path / "out.xlsx"
    kio.csv_to_sheet(str(csv_path), str(out), "S")
    ws = _load(out)["S"]
    assert ws["A1"].value == "a"
    assert ws["A2"].value is None
    assert ws["A3"].value == "b"


def test_csv_to_sheet_bom_lf_and_custom_delimiter(tmp_path: Path):
    csv_path = tmp_path / "in.csv"
    csv_path.write_bytes("\ufeffa;b\n1;2\n".encode())
    out = tmp_path / "out.xlsx"
    kio.csv_to_sheet(str(csv_path), str(out), "S", delimiter=";")
    ws = _load(out)["S"]
    assert ws["A1"].value == "a"
    assert ws["B2"].value == "2"


def test_csv_to_sheet_unterminated_quote_raises(tmp_path: Path):
    out = tmp_path / "out.xlsx"
    with pytest.raises(kyrax.KyraxError):
        kio.csv_bytes_to_sheet(b'"abc', str(out), "S")
