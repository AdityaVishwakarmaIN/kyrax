"""T1-2b image cross-check against an INDEPENDENT reader (openpyxl).

These tests guard against symmetric write/read bugs: kyrax writes a workbook,
openpyxl (``openpyxl.reader.drawings.find_images`` — the stated bar) must see the
right number of images at the right anchors; and on a workbook written by
openpyxl, kyrax's reader must agree with openpyxl's own reader.
"""

from __future__ import annotations

import io
import zipfile

import kyrax
import openpyxl
from openpyxl.drawing.image import Image as OpxImage
from openpyxl.drawing.spreadsheet_drawing import (
    AbsoluteAnchor,
    AnchorMarker,
    OneCellAnchor,
    TwoCellAnchor,
)
from openpyxl.drawing.xdr import XDRPoint2D, XDRPositiveSize2D
from openpyxl.reader.drawings import find_images


def _img_bytes(fmt: str) -> bytes:
    from PIL import Image as PILImage

    buf = io.BytesIO()
    PILImage.new("RGB", (1, 1), (255, 0, 0)).save(buf, format=fmt)
    return buf.getvalue()


PNG = _img_bytes("PNG")
JPEG = _img_bytes("JPEG")
EMU_PER_CM = 360000.0


def _opx_anchor(anchor):
    """Normalise an openpyxl anchor object to a comparable tuple."""
    if isinstance(anchor, OneCellAnchor):
        f = anchor._from
        return ("oneCell", (f.col, f.colOff, f.row, f.rowOff), (anchor.ext.cx, anchor.ext.cy))
    if isinstance(anchor, TwoCellAnchor):
        f, t = anchor._from, anchor.to
        return (
            "twoCell",
            (f.col, f.colOff, f.row, f.rowOff),
            (t.col, t.colOff, t.row, t.rowOff),
            anchor.editAs,
        )
    if isinstance(anchor, AbsoluteAnchor):
        return (
            "absolute",
            (anchor.pos.x, anchor.pos.y),
            (anchor.ext.cx, anchor.ext.cy),
        )
    raise AssertionError(f"unexpected anchor {type(anchor).__name__}")


def _kyrax_anchor(d: dict):
    a = d["anchor"]
    kind = a["kind"]
    if kind == "oneCell":
        f = a["from"]
        return ("oneCell", (f["col"], f["col_off"], f["row"], f["row_off"]), (a["cx"], a["cy"]))
    if kind == "twoCell":
        f, t = a["from"], a["to"]
        return (
            "twoCell",
            (f["col"], f["col_off"], f["row"], f["row_off"]),
            (t["col"], t["col_off"], t["row"], t["row_off"]),
            a.get("edit_as"),
        )
    if kind == "absolute":
        return ("absolute", (a["x"], a["y"]), (a["cx"], a["cy"]))
    raise AssertionError(kind)


def _kyrax_workbook_bytes() -> bytes:
    """One workbook with one image of each anchor kind, written by kyrax."""
    return kyrax.write_excel_turbo_bytes(
        [
            {
                "name": "Data",
                "images": [
                    {
                        "data": PNG,
                        "anchor": {
                            "type": "oneCell",
                            "cell": "B2",
                            "col_off": 76200,
                            "row_off": 50800,
                            "width": 4.0,
                            "height": 3.0,
                        },
                    },
                    {
                        "data": JPEG,
                        "anchor": {
                            "type": "twoCell",
                            "from": "C3",
                            "from_off": [1000, 2000],
                            "to": "F6",
                            "to_off": [3000, 4000],
                            "edit_as": "oneCell",
                        },
                    },
                    {
                        "data": PNG,
                        "anchor": {
                            "type": "absolute",
                            "x": 1000000,
                            "y": 2000000,
                            "cx": 3000000,
                            "cy": 4000000,
                        },
                    },
                ],
            },
        ]
    )


def test_openpyxl_reads_kyrax_written_images() -> None:
    buf = _kyrax_workbook_bytes()
    with zipfile.ZipFile(io.BytesIO(buf)) as archive:
        assert "xl/drawings/drawing1.xml" in archive.namelist()
        charts, images = find_images(archive, "xl/drawings/drawing1.xml")

    assert charts == []
    assert len(images) == 3, "openpyxl must see all three kyrax-written images"

    # openpyxl's find_images groups anchors as absoluteAnchor + oneCellAnchor +
    # twoCellAnchor, regardless of the drawing's document order. The writer
    # emits oneCell, twoCell, absolute — so openpyxl reports them absolute first.
    # 1) absolute in EMU.
    img = images[0]
    assert img.ref.getvalue() == PNG
    anchor = img.anchor
    assert isinstance(anchor, AbsoluteAnchor)
    assert (anchor.pos.x, anchor.pos.y) == (1000000, 2000000)
    assert (anchor.ext.cx, anchor.ext.cy) == (3000000, 4000000)

    # 2) oneCell at B2 with EMU offsets and cm-derived extent.
    img = images[1]
    assert img.ref.getvalue() == PNG
    anchor = img.anchor
    assert isinstance(anchor, OneCellAnchor)
    assert (anchor._from.col, anchor._from.row) == (1, 1)
    assert (anchor._from.colOff, anchor._from.rowOff) == (76200, 50800)
    assert (anchor.ext.cx, anchor.ext.cy) == (
        int(4.0 * EMU_PER_CM),
        int(3.0 * EMU_PER_CM),
    )

    # 3) twoCell from C3 to F6 with offsets and editAs.
    img = images[2]
    assert img.ref.getvalue() == JPEG
    anchor = img.anchor
    assert isinstance(anchor, TwoCellAnchor)
    assert (anchor._from.col, anchor._from.row) == (2, 2)
    assert (anchor._from.colOff, anchor._from.rowOff) == (1000, 2000)
    assert (anchor.to.col, anchor.to.row) == (5, 5)
    assert (anchor.to.colOff, anchor.to.rowOff) == (3000, 4000)
    assert anchor.editAs == "oneCell"


def test_kyrax_reader_matches_openpyxl_on_openpyxl_written_file(tmp_path) -> None:
    """The stated bar on the read side: kyrax's reader must agree with
    ``openpyxl.reader.drawings.find_images`` on the very same file."""
    wb = openpyxl.Workbook()
    ws = wb.active
    ws.title = "Data"

    anchors = [
        OneCellAnchor(
            _from=AnchorMarker(col=1, colOff=111, row=1, rowOff=222),
            ext=XDRPositiveSize2D(cx=333333, cy=444444),
        ),
        TwoCellAnchor(
            _from=AnchorMarker(col=2, colOff=333, row=2, rowOff=444),
            to=AnchorMarker(col=5, colOff=555, row=5, rowOff=666),
            editAs="oneCell",
        ),
        AbsoluteAnchor(
            pos=XDRPoint2D(x=1111, y=2222),
            ext=XDRPositiveSize2D(cx=3333, cy=4444),
        ),
    ]
    for data, anchor in zip([PNG, JPEG, PNG], anchors):
        im = OpxImage(io.BytesIO(data))
        im.anchor = anchor
        ws.add_image(im)

    path = tmp_path / "opx_images.xlsx"
    wb.save(str(path))

    # openpyxl, the stated bar.
    with zipfile.ZipFile(str(path)) as archive:
        charts, images = find_images(archive, "xl/drawings/drawing1.xml")
    assert charts == []
    assert len(images) == 3

    # kyrax reader.
    reader = kyrax.read_excel_turbo(str(path))
    sheet = reader.load_sheet(0, features=["images"])
    krx = sheet.images()
    assert krx is not None
    assert len(krx) == 3

    # openpyxl groups anchors as absoluteAnchor + oneCellAnchor + twoCellAnchor;
    # its own writer also serializes anchors in a non-insertion order. Compare
    # as multisets keyed on the normalized anchor tuple.
    opx_norm = sorted(_opx_anchor(i.anchor) for i in images)
    krx_norm = sorted(_kyrax_anchor(d) for d in krx)
    assert opx_norm == krx_norm, "kyrax reader must agree with openpyxl.find_images"

    ordered_images = sorted(images, key=lambda x: _opx_anchor(x.anchor))
    ordered_kyrax = sorted(krx, key=_kyrax_anchor)
    for i, d in zip(ordered_images, ordered_kyrax):
        assert i.ref.getvalue() == d["data"]
