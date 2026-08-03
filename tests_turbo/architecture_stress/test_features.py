"""Lane A2 - OOXML authoring / feature completeness (Wave 1 baseline).

Exclusive owner: A2. Write access: this file only (plus tests/archstress_features.rs
and plans/architecture_stress/NOTES/A2.md). All source, shared harness (A6 common.py /
fixtures.py), and other lanes' files are excluded.

Wave-1 scope: bounded author -> disk -> read -> no-op edit_excel save -> unrelated-cell
edit -> read lifecycle with exact member-delta expectations, plus focused probes for
chart / pivot / image dedup / DV / legacy comment / simple filter / style. Everything is
self-contained (local temp dirs, no network, no Excel COM). A6 fixture and COM
capabilities are skipped with a named dependency when absent.

Coverage map (see NOTES/A2.md):
  A2-CHART-01 (probe)  test_chart_part_presence + determinism
  A2-PIVOT-01 (probe)  test_pivot_roundtrip
  A2-IMAGE-01 (probe)  test_image_dedup_one_part, test_distinct_images_never_alias
  A2-DV-01    (probe)  test_dv_part_presence
  A2-CMT-01a  (legacy) test_legacy_comment_roundtrip
  A2-FMT-01   (probe)  test_style_preserved_through_overlay
  A2-FILT-01a (simple) test_simple_filter_preserved_through_overlay,
                       test_auto_filter_authoring_schema
  A2-FILT-01b (complex, skipped KNOWN-GAP)
                       test_complex_filter_modes_skipped_dependency
  A2-CMT-01c public authoring gap: test_known_gap_no_public_threaded_authoring
  CMT-01d / COM        test_*_skipped_dependency
  P06 baseline half    test_noop_save_member_preservation,
                       test_unrelated_edit_member_delta
  A6-DET feed          test_deterministic_bytes
"""

from __future__ import annotations

import base64
import hashlib
import io
import zipfile

import pytest

import kyrax

OPX = pytest.importorskip("openpyxl")

# ---------------------------------------------------------------------------
# Local helpers (no shared harness, no fixtures.py)
# ---------------------------------------------------------------------------

# A small, well-known valid 1x1 PNG (magic: \x89PNG). Used for image dedup probes.
_PNG = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="
)
# Distinct bytes with a JPEG magic header; never decoded, only packaged.
_JPEG = b"\xff\xd8\xff\xe0\x00\x10JFIF\x00\x01" + bytes(32)


def _feature_sheet(name: str = "Data") -> dict:
    """A representative authored sheet exercising several shipped writers."""
    return {
        "name": name,
        "columns": [
            ["Region", "East", "West", "East", "West"],
            ["Product", "Widget", "Gadget", "Widget", "Gadget"],
            ["Amount", 100, 150, 200, 50],
        ],
        "merges": ["F1:G1"],
        "comments": [{"ref": "A1", "author": "Ada", "text": "Hello"}],
        "data_validations": [
            {
                "type": "whole",
                "operator": "between",
                "formula1": "1",
                "formula2": "10",
                "sqref": "D1:D5",
            }
        ],
        "charts": [
            {
                "type": "col",
                "title": "Sales",
                "anchor": "E2",
                "series": [
                    {
                        "title_literal": "Amount",
                        "cat_ref": "'Data'!$A$2:$A$5",
                        "val_ref": "'Data'!$C$2:$C$5",
                    }
                ],
            }
        ],
        "images": [{"data": _PNG, "anchor": "H2"}],
        "pivots": [
            {
                "name": "PivotTable1",
                "source_range": "A1:C5",
                "rows": ["Region"],
                "cols": ["Product"],
                "data": [{"field": "Amount", "agg": "sum"}],
                "target_cell": "E10",
            }
        ],
    }


def _authored_bytes(sheets: list[dict]) -> bytes:
    return kyrax.write_excel_turbo_bytes(sheets, features="all")


def _write(path, data: bytes) -> None:
    with open(path, "wb") as fh:
        fh.write(data)


def _member_hashes(data: bytes) -> dict[str, str]:
    with zipfile.ZipFile(io.BytesIO(data)) as zf:
        return {n: hashlib.sha256(zf.read(n)).hexdigest() for n in sorted(zf.namelist())}


def _member_names(data: bytes) -> list[str]:
    with zipfile.ZipFile(io.BytesIO(data)) as zf:
        return sorted(zf.namelist())


def _media_parts(data: bytes) -> list[str]:
    return [n for n in _member_names(data) if n.startswith("xl/media/")]


def _worksheet_part(data: bytes) -> str:
    """The single actual worksheet part (never an _rels sibling)."""
    names = [
        n
        for n in _member_names(data)
        if n.startswith("xl/worksheets/")
        and not n.startswith("xl/worksheets/_rels/")
    ]
    assert len(names) == 1, names
    return names[0]


# Members that may legitimately change after an unrelated cell edit: only the
# edited worksheet (exactly xl/worksheets/sheet1.xml) plus the optional SST /
# styles / calcChain entries. No property parts, no other worksheets.
_DELTA_ALLOW = (
    "xl/worksheets/sheet1.xml",
    "xl/sharedStrings.xml",
    "xl/styles.xml",
    "xl/calcChain.xml",
)
# No-op save allows no changed payload parts: every member byte-identical.
_NOOP_ALLOW = ()


def _assert_member_delta(m1: dict[str, str], m2: dict[str, str], allow: tuple) -> None:
    assert set(m1) == set(m2), "member set changed"
    for name, h1 in m1.items():
        if name.startswith(allow):
            continue
        assert h1 == m2[name], f"unexpected change in {name}"


# ---------------------------------------------------------------------------
# Author -> disk -> read -> no-op save -> unrelated edit -> read lifecycle
# ---------------------------------------------------------------------------


def test_author_read_roundtrip(tmp_path) -> None:
    path = tmp_path / "gen1.xlsx"
    _write(path, _authored_bytes([_feature_sheet()]))

    wb = OPX.load_workbook(str(path))
    assert wb.sheetnames == ["Data"]
    assert wb["Data"]["B2"].value == "Widget"
    assert wb["Data"]["C3"].value == 150
    assert wb["Data"]["A1"].comment is not None

    reader = kyrax.read_excel_turbo(str(path))
    sheet = reader.load_sheet(0, features=["pivots"])
    pivots = sheet.pivots()
    assert pivots is not None and len(pivots) == 1


def test_noop_save_member_preservation(tmp_path) -> None:
    gen1 = tmp_path / "gen1.xlsx"
    gen2 = tmp_path / "gen2.xlsx"
    _write(gen1, _authored_bytes([_feature_sheet()]))
    m1 = _member_hashes(open(gen1, "rb").read())

    wb = kyrax.edit_excel(str(gen1))
    wb.save(str(gen2))
    m2 = _member_hashes(open(gen2, "rb").read())

    _assert_member_delta(m1, m2, _NOOP_ALLOW)


def test_unrelated_edit_member_delta(tmp_path) -> None:
    """P06 baseline half: unrelated cell edit must not touch feature parts."""
    gen1 = tmp_path / "gen1.xlsx"
    gen2 = tmp_path / "gen2.xlsx"
    gen3 = tmp_path / "gen3.xlsx"
    _write(gen1, _authored_bytes([_feature_sheet()]))
    m1 = _member_hashes(open(gen1, "rb").read())

    wb = kyrax.edit_excel(str(gen1))
    wb.save(str(gen2))
    wb2 = kyrax.edit_excel(str(gen2))
    wb2["Data"].set_cell(50, 20, 12345.0)
    wb2.save(str(gen3))
    m3 = _member_hashes(open(gen3, "rb").read())

    _assert_member_delta(m1, m3, _DELTA_ALLOW)
    for expected in (
        "xl/charts/chart1.xml",
        "xl/drawings/drawing1.xml",
        "xl/pivotTables/pivotTable1.xml",
        "xl/pivotCache/pivotCacheDefinition1.xml",
        "xl/comments/comment1.xml",
        "xl/media/image1.png",
    ):
        assert expected in m3, f"feature part {expected} dropped after edit"


def test_deterministic_bytes() -> None:
    a = _authored_bytes([_feature_sheet()])
    b = _authored_bytes([_feature_sheet()])
    assert a == b


# ---------------------------------------------------------------------------
# Focused probes (shipped writer seams)
# ---------------------------------------------------------------------------


def test_chart_part_presence(tmp_path) -> None:
    path = tmp_path / "chart.xlsx"
    _write(path, _authored_bytes([_feature_sheet()]))
    names = _member_names(open(path, "rb").read())
    assert "xl/charts/chart1.xml" in names
    assert any(n.startswith("xl/drawings/") for n in names)


def test_pivot_roundtrip(tmp_path) -> None:
    path = tmp_path / "pivot.xlsx"
    _write(path, _authored_bytes([_feature_sheet()]))
    reader = kyrax.read_excel_turbo(str(path))
    sheet = reader.load_sheet(0, features=["pivots"])
    p = sheet.pivots()[0]
    assert p["name"] == "PivotTable1"
    assert p["row_fields"] == ["Region"]
    assert p["col_fields"] == ["Product"]
    assert p["cache_source"]["ref"] == "A1:C5"


def test_image_dedup_one_part(tmp_path) -> None:
    sheets = [_feature_sheet(), _feature_sheet("Data2"), _feature_sheet("Data3")]
    path = tmp_path / "dedup.xlsx"
    _write(path, _authored_bytes(sheets))
    media = _media_parts(open(path, "rb").read())
    assert [m for m in media if m.endswith(".png")] == ["xl/media/image1.png"]


def test_distinct_images_never_alias(tmp_path) -> None:
    sheet = _feature_sheet()
    sheet["images"] = [
        {"data": _PNG, "anchor": "H2"},
        {"data": _JPEG, "anchor": "H4"},
    ]
    path = tmp_path / "distinct.xlsx"
    _write(path, _authored_bytes([sheet]))
    media = _media_parts(open(path, "rb").read())
    assert "xl/media/image1.png" in media
    assert "xl/media/image2.jpeg" in media


def test_dv_part_presence(tmp_path) -> None:
    path = tmp_path / "dv.xlsx"
    _write(path, _authored_bytes([_feature_sheet()]))
    raw = open(path, "rb").read()
    xml = _member_bytes(raw, _worksheet_part(raw))
    assert b"<dataValidation" in xml
    assert b'sqref="D1:D5"' in xml
    assert b'showDropDown="0"' in xml


def test_legacy_comment_roundtrip(tmp_path) -> None:
    path = tmp_path / "cmt.xlsx"
    _write(path, _authored_bytes([_feature_sheet()]))
    wb = OPX.load_workbook(str(path))
    comment = wb["Data"]["A1"].comment
    assert comment is not None
    assert comment.text == "Hello"
    assert comment.author == "Ada"


def test_style_preserved_through_overlay(tmp_path) -> None:
    """A2-FMT-01 probe: styles authored by openpyxl survive an overlay save."""
    src = tmp_path / "styled.xlsx"
    out = tmp_path / "styled_out.xlsx"
    wb = OPX.Workbook()
    ws = wb.active
    ws.title = "Data"
    ws["A1"] = 1.5
    ws["A1"].font = OPX.styles.Font(bold=True, color="FFFF0000")
    ws["A1"].number_format = "0.000"
    wb.save(str(src))
    wb.close()

    eb = kyrax.edit_excel(str(src))
    eb.save(str(out))
    out_wb = OPX.load_workbook(str(out))
    cell = out_wb["Data"]["A1"]
    assert cell.value == 1.5
    assert cell.font.bold is True
    assert cell.number_format == "0.000"


def test_simple_filter_preserved_through_overlay(tmp_path) -> None:
    """A2-FILT-01a: a simple (value-list) autofilter survives an overlay save."""
    src = tmp_path / "filtered.xlsx"
    out = tmp_path / "filtered_out.xlsx"
    wb = OPX.Workbook()
    ws = wb.active
    ws.title = "Data"
    for r in range(1, 6):
        ws.cell(row=r, column=1, value=("a", "a", "b", "b", "c")[r - 1])
    ws.auto_filter.ref = "A1:A5"
    ws.auto_filter.add_filter_column(0, ["a", "b"])
    wb.save(str(src))
    wb.close()

    eb = kyrax.edit_excel(str(src))
    eb.save(str(out))
    out_wb = OPX.load_workbook(str(out))
    assert out_wb["Data"].auto_filter.ref == "A1:A5"


# ---------------------------------------------------------------------------
# Evidence-based known-gap probes and named dependencies
# ---------------------------------------------------------------------------


def test_known_gap_no_public_threaded_authoring() -> None:
    """A2-CMT-01c: the public writer does not package threaded comments/persons.

    Passing this assertion is the KNOWN-GAP evidence: the key is ignored and no
    threaded-comment parts are emitted. There is no threaded authoring seam in
    write_excel_turbo_bytes (see NOTES/A2.md CMT-01b emitter-only coverage).
    """
    sheet = _feature_sheet()
    sheet["threaded_comments"] = [
        {"ref": "A2", "text": "reply", "person_id": "p1"}
    ]
    names = _member_names(_authored_bytes([sheet]))
    assert not [n for n in names if "threadedComment" in n]
    assert not [n for n in names if n.startswith("xl/persons")]


def test_auto_filter_authoring_schema() -> None:
    """A2-FILT-01a: author a simple value-list filter via the supported
    ``auto_filter`` sheet key (ref + columns with col_id/values/blank).
    """
    sheet = _feature_sheet()
    sheet["auto_filter"] = {
        "ref": "A1:A5",
        "columns": [{"col_id": 0, "values": ["East", "West"], "blank": False}],
    }
    raw = _authored_bytes([sheet])
    xml = _member_bytes(raw, _worksheet_part(raw))
    assert b'<autoFilter ref="A1:A5"' in xml
    assert b'<filter val="East"' in xml
    assert b'<filter val="West"' in xml


def test_complex_filter_modes_skipped_dependency() -> None:
    pytest.skip(
        "DEP: complex filter modes (custom/top10/dynamic/color/icon/sort) have no "
        "authoring schema; KNOWN-GAP, no invented filters key"
    )


def test_threaded_preservation_skipped_dependency() -> None:
    pytest.skip("DEP: A6 threaded-comment fixture (F04 part injection) is absent")


def test_excel_com_acceptance_skipped_dependency() -> None:
    pytest.skip("DEP: A6 Windows Excel COM acceptance harness is absent")


def _member_bytes(data: bytes, name: str) -> bytes:
    with zipfile.ZipFile(io.BytesIO(data)) as zf:
        return zf.read(name)
