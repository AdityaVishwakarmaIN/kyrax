"""Familiar (openpyxl-style) chart + image authoring on the editable API."""

from __future__ import annotations

import struct
import zipfile
import zlib

import pytest

import kyrax
from kyrax.chart import BarChart, LineChart, PieChart, Reference, ScatterChart, Series
from kyrax.drawing.image import Image


def _png(w: int, h: int) -> bytes:
    def chunk(tag: bytes, data: bytes) -> bytes:
        body = tag + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    raw = b"".join(b"\x00" + b"\xff\x00\x00" * w for _ in range(h))
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw))
        + chunk(b"IEND", b"")
    )


def _fill(ws) -> None:
    ws.append(["Month", "Sales", "Cost"])
    for i, (s, c) in enumerate([(10, 4), (20, 8), (15, 9), (30, 12)], start=1):
        ws.append([f"M{i}", s, c])


def _parts(path) -> dict[str, str]:
    with zipfile.ZipFile(path) as z:
        return {n: z.read(n).decode("utf-8", "replace") for n in z.namelist()}


def test_new_workbook_chart_and_image_round_trip(tmp_path):
    png = tmp_path / "logo.png"
    png.write_bytes(_png(40, 20))
    wb = kyrax.Workbook()
    ws = wb.active
    _fill(ws)

    chart = BarChart()
    chart.title = "Sales"
    chart.style = 10
    chart.x_axis.title = "Month"
    chart.add_data(Reference(ws, min_col=2, min_row=1, max_col=3, max_row=5), titles_from_data=True)
    chart.set_categories(Reference(ws, min_col=1, min_row=2, max_row=5))
    ws.add_chart(chart, "E2")

    img = Image(str(png))
    assert (img.width, img.height) == (40, 20)
    ws.add_image(img, "E20")

    out = tmp_path / "new.xlsx"
    wb.save(str(out))
    parts = _parts(out)
    q = f"'{ws.title}'"
    c = parts["xl/charts/chart1.xml"]
    assert "<barChart>" in c and '<barDir val="col"/>' in c and ">Sales<" in c
    assert f"<f>{q}!$B$2:$B$5</f>" in c and f"<f>{q}!$C$1</f>" in c
    assert f"<f>{q}!$A$2:$A$5</f>" in c
    assert "xl/media/image1.png" in parts
    d = parts["xl/drawings/drawing1.xml"]
    assert "<graphicFrame>" in d and "<pic>" in d
    # 40x20 px at 9525 EMU/px.
    assert 'cx="381000" cy="190500"' in d
    sheet_rels = next(v for k, v in parts.items() if k.startswith("xl/worksheets/_rels/"))
    assert 'Target="/xl/drawings/drawing1.xml"' in sheet_rels
    sheet_xml = next(v for k, v in parts.items() if k.startswith("xl/worksheets/sheet"))
    assert "<drawing r:id=" in sheet_xml and "xmlns:r=" in sheet_xml
    ct = parts["[Content_Types].xml"]
    assert 'PartName="/xl/charts/chart1.xml"' in ct and 'PartName="/xl/drawings/drawing1.xml"' in ct
    assert 'Extension="png"' in ct

    # Reload with kyrax: data intact, chart + image visible to the reader.
    assert kyrax.load_workbook(str(out)).active["B3"].value == 20
    sheet = kyrax.read_excel_turbo(str(out)).load_sheet(0, features=["images", "charts"])
    assert len(sheet.images()) == 1
    assert len(sheet.charts()) == 1


def test_existing_file_numbering_and_existing_drawing_refused(tmp_path):
    src = tmp_path / "src.xlsx"
    wb = kyrax.Workbook()
    _fill(wb.active)
    wb.create_sheet("Two")
    _fill(wb["Two"])
    first = BarChart()
    first.add_data(Reference(wb.active, min_col=2, min_row=2, max_row=5))
    wb.active.add_chart(first, "E2")
    wb.save(str(src))

    wb = kyrax.load_workbook(str(src))
    with pytest.raises(NotImplementedError):
        wb.active.add_chart(LineChart(), "H2")
    ws = wb["Two"]
    line = LineChart()
    line.add_data(Reference(ws, min_col=2, min_row=1, max_row=5), titles_from_data=True)
    pie = PieChart()
    pie.series.append(Series(Reference(ws, min_col=3, min_row=2, max_row=5), title="Cost"))
    pie.set_categories(Reference(ws, min_col=1, min_row=2, max_row=5))
    sc = ScatterChart()
    sc.series.append(
        Series(Reference(ws, min_col=3, min_row=1, max_row=5),
               xvalues=Reference(ws, min_col=2, min_row=2, max_row=5), title_from_data=True)
    )
    for ch, cell in ((line, "E2"), (pie, "E20"), (sc, "M2")):
        ws.add_chart(ch, cell)
    out = tmp_path / "edited.xlsx"
    wb.save(str(out))

    parts = _parts(out)
    assert {"xl/charts/chart1.xml", "xl/charts/chart2.xml", "xl/charts/chart3.xml",
            "xl/charts/chart4.xml", "xl/drawings/drawing1.xml", "xl/drawings/drawing2.xml"} <= set(parts)
    assert "<lineChart>" in parts["xl/charts/chart2.xml"]
    assert "<pieChart>" in parts["xl/charts/chart3.xml"]
    sc_xml = parts["xl/charts/chart4.xml"]
    assert "<xVal><numRef><f>'Two'!$B$2:$B$5</f>" in sc_xml
    assert "<yVal><numRef><f>'Two'!$C$2:$C$5</f>" in sc_xml
    assert parts["[Content_Types].xml"].count('PartName="/xl/drawings/drawing2.xml"') == 1
    assert len(kyrax.read_excel_turbo(str(out)).load_sheet(1, features=["charts"]).charts()) == 3
    # The original chart is untouched.
    assert len(kyrax.read_excel_turbo(str(out)).load_sheet(0, features=["charts"]).charts()) == 1
