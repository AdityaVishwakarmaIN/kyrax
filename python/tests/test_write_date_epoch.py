"""Python calendar values must use the epoch declared in workbook.xml."""

from __future__ import annotations

import datetime as dt
import io
import xml.etree.ElementTree as ET
import zipfile

import kyrax
import pytest

NS = {"s": "http://schemas.openxmlformats.org/spreadsheetml/2006/main"}


def _write(entry, tmp_path, rows, **options):
    sheets = [{"name": "Dates", **rows}]
    if entry == "bytes":
        return kyrax.write_excel_turbo_bytes(sheets, **options)
    path = tmp_path / "dates.xlsx"
    writer = kyrax.write_excel_turbo_stream if entry == "stream" else kyrax.write_excel_turbo
    writer(path, sheets, **options)
    return path.read_bytes()


@pytest.mark.parametrize("entry", ["path", "stream", "bytes"])
@pytest.mark.parametrize("source", ["rows", "iterator", "columns"])
@pytest.mark.parametrize("date1904", [False, True])
@pytest.mark.parametrize("date_iso", [False, True])
def test_calendar_values_use_workbook_epoch(entry, source, date1904, date_iso, tmp_path):
    values = [
        dt.date(2024, 1, 2),
        dt.datetime(2024, 1, 2, 12, 30, 0),
        dt.date(1900, 1, 1),
        dt.date(1900, 2, 28),
        dt.date(1900, 3, 1),
        dt.date(1904, 1, 1),
    ]
    if source == "columns":
        rows = {"columns": [[value] for value in values]}
    elif source == "iterator":
        rows = {"rows": iter([values])}
    else:
        rows = {"rows": [values]}
    data = _write(entry, tmp_path, rows, date1904=date1904, date_iso=date_iso)
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        root = ET.fromstring(archive.read("xl/worksheets/sheet1.xml"))
        props = ET.fromstring(archive.read("xl/workbook.xml")).find("s:workbookPr", NS)
        assert (props is not None and props.get("date1904") in ("1", "true")) == date1904
    cells = root.findall("s:sheetData/s:row/s:c", NS)
    assert len(cells) == len(values)
    for cell, value in zip(cells, values):
        text = cell.find("s:v", NS).text
        if date_iso:
            assert cell.get("t") == "d"
            expected = value.isoformat()
            # The date-only ISO form represents a midnight datetime equally.
            assert text == expected
        else:
            assert cell.get("t") is None
            epoch = dt.datetime(1904, 1, 1) if date1904 else dt.datetime(1899, 12, 30)
            date = (
                value if isinstance(value, dt.datetime) else dt.datetime.combine(value, dt.time())
            )
            expected = (date - epoch).total_seconds() / 86400
            if not date1904 and 0 < expected <= 60:
                expected -= 1  # Excel's fictitious 1900-02-29
            assert float(text) == pytest.approx(expected, rel=0, abs=1e-9)


@pytest.mark.parametrize("entry", ["path", "stream", "bytes"])
@pytest.mark.parametrize("date1904", [False, True])
def test_numeric_time_and_duration_values_are_not_epoch_shifted(entry, date1904, tmp_path):
    data = _write(
        entry,
        tmp_path,
        {"rows": [[45293.0, dt.time(12), dt.timedelta(days=2, hours=12)]]},
        date1904=date1904,
    )
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        root = ET.fromstring(archive.read("xl/worksheets/sheet1.xml"))
    assert [float(value.text) for value in root.findall("s:sheetData/s:row/s:c/s:v", NS)] == [
        45293.0,
        0.5,
        2.5,
    ]


def test_editable_1904_date_is_shifted_once(tmp_path):
    path = tmp_path / "editable.xlsx"
    kyrax.write_excel_turbo(path, [{"name": "Dates", "rows": [["date"]]}], date1904=True)
    wb = kyrax.load_workbook(path)
    wb.active.append([dt.date(2024, 1, 2)])
    wb.active["B2"] = dt.datetime(2024, 1, 2, 12)
    wb.save(path)
    with zipfile.ZipFile(path) as archive:
        root = ET.fromstring(archive.read("xl/worksheets/sheet1.xml"))
    cells = {c.get("r"): c for c in root.findall("s:sheetData/s:row/s:c", NS)}
    assert float(cells["A2"].find("s:v", NS).text) == 43831.0
    assert float(cells["B2"].find("s:v", NS).text) == 43831.5
