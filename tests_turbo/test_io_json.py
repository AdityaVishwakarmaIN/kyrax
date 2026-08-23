"""Turbo io JSON tests: sheet <-> json/ndjson export/import (path + bytes)."""

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


def test_sheet_to_json_records(tmp_path: Path):
    path = _make(tmp_path / "src.xlsx", [["Region", "East", "West"], ["Amount", 100, 200]])
    text = kio.sheet_to_json_bytes(str(path), "Data", shape="records")
    assert text == b'[{"Region":"East","Amount":100},{"Region":"West","Amount":200}]'

    out = tmp_path / "out.json"
    kio.sheet_to_json(str(path), "Data", str(out), shape="records")
    assert out.read_bytes() == text


def test_sheet_to_json_columns(tmp_path: Path):
    path = _make(tmp_path / "src.xlsx", [["Region", "East", "West"], ["Amount", 100, 200]])
    text = kio.sheet_to_json_bytes(str(path), "Data", shape="columns")
    assert text == b'{"Region":["East","West"],"Amount":[100,200]}'


def test_sheet_to_json_ndjson(tmp_path: Path):
    path = _make(tmp_path / "src.xlsx", [["Region", "East", "West"], ["Amount", 100, 200]])
    text = kio.sheet_to_json_bytes(str(path), "Data", shape="ndjson")
    assert text == b'{"Region":"East","Amount":100}\n{"Region":"West","Amount":200}\n'


def test_sheet_to_json_null_vs_empty_string(tmp_path: Path):
    # Excel empty cell -> JSON null; Excel empty-string cell -> JSON "".
    path = _make(tmp_path / "src.xlsx", [["A", "x", "y"], ["B", "", None]])
    text = kio.sheet_to_json_bytes(str(path), "Data", shape="records")
    assert text == b'[{"A":"x","B":""},{"A":"y","B":null}]'


def test_sheet_to_json_big_int_is_string(tmp_path: Path):
    # An integral value beyond 2^53 emits as a string (fidelity, never a
    # silent lossy double).
    path = _make(tmp_path / "src.xlsx", [["Big", 1e300]])
    text = kio.sheet_to_json_bytes(str(path), "Data", shape="records")
    assert text == b'[{"Big":"1e300"}]'


def test_sheet_to_json_dates_iso_and_strftime(tmp_path: Path):
    path = tmp_path / "src.xlsx"
    wb = openpyxl.Workbook()
    ws = wb.active
    ws.title = "Data"
    ws["A1"] = "When"
    ws["A2"] = datetime(2021, 3, 4, 5, 6, 7)
    wb.save(path)
    assert (
        kio.sheet_to_json_bytes(str(path), "Data", shape="records")
        == b'[{"When":"2021-03-04T05:06:07"}]'
    )
    assert (
        kio.sheet_to_json_bytes(str(path), "Data", shape="records", date_format="%d/%m/%Y")
        == b'[{"When":"04/03/2021"}]'
    )


def test_sheet_to_json_no_header_positional_keys(tmp_path: Path):
    # has_header=False: keys are positional and the header row becomes the
    # first data record.
    path = _make(tmp_path / "src.xlsx", [["Region", "East"], ["Amount", 100]])
    text = kio.sheet_to_json_bytes(str(path), "Data", shape="records", has_header=False)
    assert text == b'[{"1":"Region","2":"Amount"},{"1":"East","2":100}]'


def test_sheet_to_json_bad_shape(tmp_path: Path):
    path = _make(tmp_path / "src.xlsx", [["A", "a"]])
    with pytest.raises(ValueError):
        kio.sheet_to_json_bytes(str(path), "Data", shape="xml")


# ---------------------------------------------------------------------------
# Import
# ---------------------------------------------------------------------------


def test_json_to_sheet_records_heterogeneous_keys(tmp_path: Path):
    json_path = tmp_path / "in.json"
    json_path.write_text(
        '[{"Region":"East","Amount":100},{"Amount":200,"Region":"West","Note":null}]'
    )
    out = tmp_path / "out.xlsx"
    kio.json_to_sheet(str(json_path), str(out), "S")
    ws = _load(out)["S"]
    assert ws["A1"].value == "Region"
    assert ws["B1"].value == "Amount"
    assert ws["C1"].value == "Note"
    assert ws["A2"].value == "East"
    assert ws["B2"].value == 100
    assert ws["C2"].value is None
    assert ws["A3"].value == "West"
    assert ws["B3"].value == 200


def test_json_bytes_to_sheet_matches_path(tmp_path: Path):
    data = b'[{"a":1,"b":2}]'
    out_a = tmp_path / "out_a.xlsx"
    out_b = tmp_path / "out_b.xlsx"
    in_path = tmp_path / "in.json"
    in_path.write_bytes(data)
    kio.json_bytes_to_sheet(data, str(out_a), "S")
    kio.json_to_sheet(str(in_path), str(out_b), "S")
    assert out_a.read_bytes() == out_b.read_bytes()


def test_json_to_sheet_big_int_kept_as_string(tmp_path: Path):
    json_path = tmp_path / "in.json"
    json_path.write_text('[{"id":12345678901234567890,"v":1}]')
    out = tmp_path / "out.xlsx"
    kio.json_to_sheet(str(json_path), str(out), "S")
    ws = _load(out)["S"]
    assert ws["A2"].value == "12345678901234567890"
    assert ws["B2"].value == 1


def test_json_to_sheet_nested_is_raw_text(tmp_path: Path):
    json_path = tmp_path / "in.json"
    json_path.write_text('[{"a":1,"nested":{"x":[1,2]}}]')
    out = tmp_path / "out.xlsx"
    kio.json_to_sheet(str(json_path), str(out), "S")
    ws = _load(out)["S"]
    assert ws["A2"].value == 1
    assert ws["B2"].value == '{"x":[1,2]}'


def test_json_to_sheet_ndjson(tmp_path: Path):
    json_path = tmp_path / "in.ndjson"
    json_path.write_text('{"a":1,"b":2}\n{"a":3}\n')
    out = tmp_path / "out.xlsx"
    kio.json_to_sheet(str(json_path), str(out), "S", shape="ndjson")
    ws = _load(out)["S"]
    assert ws["A1"].value == "a"
    assert ws["B1"].value == "b"
    assert ws["A2"].value == 1
    assert ws["B2"].value == 2
    assert ws["A3"].value == 3
    assert ws["B3"].value is None


def test_json_to_sheet_columns_shape(tmp_path: Path):
    json_path = tmp_path / "in.json"
    json_path.write_text('{"a":[1,2],"b":[true,"x"]}')
    out = tmp_path / "out.xlsx"
    kio.json_to_sheet(str(json_path), str(out), "S", shape="columns")
    ws = _load(out)["S"]
    assert ws["A1"].value == "a"
    assert ws["B1"].value == "b"
    assert ws["A2"].value == 1
    assert ws["B2"].value is True
    assert ws["A3"].value == 2
    assert ws["B3"].value == "x"


def test_json_to_sheet_truncated_raises(tmp_path: Path):
    out = tmp_path / "out.xlsx"
    with pytest.raises(kyrax.KyraxError):
        kio.json_bytes_to_sheet(b'[{"a":1}', str(out), "S")
