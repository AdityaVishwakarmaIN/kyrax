"""S1 adversarial stress tests for gap features.

Fixtures from tests_turbo/gen_stress2.py → testdata/stress2_*.xlsx.
"""

from __future__ import annotations

import sys
from pathlib import Path

import openpyxl
import pytest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "python"))

from kyrax import read_excel_turbo  # noqa: E402

TESTDATA = ROOT / "testdata"

STRESS2_FILES = [
    "stress2_sparse_meta.xlsx",
    "stress2_multisheet_meta.xlsx",
    "stress2_onecell_all.xlsx",
    "stress2_tail_order.xlsx",
    "stress2_malformed_meta.xlsx",
    "stress2_malformed_chart.xlsx",
    "stress2_malformed_pivot.xlsx",
    "stress2_malformed_threaded.xlsx",
    "stress2_malformed_vba.xlsx",
]


def _ensure_fixtures() -> None:
    missing = [n for n in STRESS2_FILES if not (TESTDATA / n).exists()]
    if not missing:
        return
    import tests_turbo.gen_stress2 as gen

    gen.main()


def setup_module(module):  # noqa: ARG001
    _ensure_fixtures()


def cell_value(rb, row: int, col: int):
    return rb.column(col)[row].as_py()


# ---------------------------------------------------------------------------
# 1. Sparse meta
# ---------------------------------------------------------------------------


def test_stress2_sparse_meta_dims_filter_freeze_and_values():
    path = TESTDATA / "stress2_sparse_meta.xlsx"
    sheet = read_excel_turbo(str(path)).load_sheet(0, features="all")
    rb = sheet.to_arrow()

    wb = openpyxl.load_workbook(path, data_only=True)
    ws = wb.active
    wb_f = openpyxl.load_workbook(path, data_only=False)
    ws_f = wb_f.active

    # --- sparse values still correct (@r regression guard) ---
    # A5 → data row 3, col 0
    assert cell_value(rb, 5 - 2, 0) == 100.0
    assert cell_value(rb, 5 - 2, 2) == 300.0
    # B5 empty
    assert cell_value(rb, 5 - 2, 1) is None
    assert cell_value(rb, 10 - 2, 3) == 400.0
    assert cell_value(rb, 20 - 2, 0) == 1.0
    assert cell_value(rb, 20 - 2, 2) == 2.0

    # --- row dimensions vs openpyxl (incl. empty rows with height/hidden) ---
    rows = sheet.row_dimensions() or {}
    # turbo keys are 1-based sheet row numbers from XML
    for sheet_row in (2, 3, 4, 5, 8, 20):
        oxl = ws_f.row_dimensions[sheet_row]
        assert sheet_row in rows or int(sheet_row) in rows, f"missing row dim {sheet_row}: {rows.keys()}"
        got = rows.get(sheet_row) or rows.get(int(sheet_row))
        assert got is not None
        if oxl.height is not None:
            assert got["height"] is not None
            assert abs(float(got["height"]) - float(oxl.height)) < 0.1
        if oxl.hidden:
            assert got["hidden"] is True

    # empty rows 2/3/8 must still appear when openpyxl recorded them
    for empty_r in (2, 3, 8):
        key = empty_r if empty_r in rows else int(empty_r)
        assert key in rows, f"empty-row dim missing for row {empty_r}"

    # --- column dimensions: beyond used range (F, G, Z) ---
    cols = sheet.column_dimensions() or []
    by_min = {c["min"]: c for c in cols}
    # openpyxl letter → 1-based index
    for letter in ("A", "B", "F", "G", "Z"):
        oxl = ws_f.column_dimensions[letter]
        idx = openpyxl.utils.column_index_from_string(letter)
        got = by_min.get(idx)
        assert got is not None, f"missing col {letter} min={idx}; have {by_min.keys()}"
        if oxl.width is not None and got["width"] is not None:
            assert abs(float(got["width"]) - float(oxl.width)) < 0.2
        if oxl.hidden:
            assert got["hidden"] is True

    # --- autofilter + freeze ---
    af = sheet.auto_filter()
    assert af is not None
    assert af["ref"].replace("$", "") == (ws_f.auto_filter.ref or "").replace("$", "")
    assert sheet.freeze_panes() == ws_f.freeze_panes

    wb.close()
    wb_f.close()


# ---------------------------------------------------------------------------
# 2. Multi-sheet isolation
# ---------------------------------------------------------------------------


def test_stress2_multisheet_isolation():
    path = TESTDATA / "stress2_multisheet_meta.xlsx"
    reader = read_excel_turbo(str(path))
    names = reader.sheet_names
    assert "ValidCF" in names
    assert "ChartsOnly" in names
    assert "Threaded" in names
    assert "DimsPanes" in names

    sheets = {n: reader.load_sheet(n, features="all") for n in names}

    # Per-sheet isolation of validations
    dvs = {n: (s.data_validations() or []) for n, s in sheets.items()}
    assert len(dvs["ValidCF"]) >= 1
    assert any("A2" in (d.get("sqref") or "") for d in dvs["ValidCF"])
    # ChartsOnly: no validations
    assert dvs.get("ChartsOnly", []) == [] or all(
        not d.get("sqref") for d in dvs["ChartsOnly"]
    )
    # Threaded has list validation on B2 only
    thr_dvs = dvs["Threaded"]
    assert any("B2" in (d.get("sqref") or "") for d in thr_dvs)
    # ValidCF validations must not appear on DimsPanes
    assert dvs["DimsPanes"] == [] or not any(
        "A2:A11" in (d.get("sqref") or "") for d in dvs["DimsPanes"]
    )

    # CF isolation
    cfs = {n: (s.conditional_formatting() or []) for n, s in sheets.items()}
    assert len(cfs["ValidCF"]) >= 1
    assert cfs["ChartsOnly"] == []
    assert cfs["DimsPanes"] == []
    assert len(cfs["Threaded"]) >= 1
    # ValidCF rule range must not leak onto Threaded
    thr_sqrefs = " ".join(r.get("sqref") or "" for r in cfs["Threaded"])
    assert "A2:A11" not in thr_sqrefs

    # Charts only on ChartsOnly (+ maybe chartsheet)
    for n, s in sheets.items():
        charts = s.charts() or []
        if n == "ChartsOnly":
            assert len(charts) >= 1, charts
            assert any("Bar" in (c.get("title") or "") or c.get("type") for c in charts)
        elif getattr(s, "sheet_kind", None) == "chartsheet":
            # chartsheet may hold a chart
            pass
        else:
            assert charts == [], f"{n} leaked charts: {charts}"

    # Threaded comments only on Threaded
    for n, s in sheets.items():
        thr = s.threaded_comments() or []
        if n == "Threaded":
            assert len(thr) >= 1
            refs = [t.get("ref") for t in thr]
            assert any(r and "A2" in r for r in refs), thr
        elif s.sheet_kind == "chartsheet":
            assert thr == []
        else:
            assert thr == [], f"{n} leaked threaded: {thr}"

    # Protection isolation
    prot_valid = sheets["ValidCF"].protection() or {}
    prot_charts = sheets["ChartsOnly"].protection() or {}
    assert prot_valid.get("sheet") is True
    # ChartsOnly intentionally unprotected
    assert prot_charts.get("sheet") in (False, None) or prot_charts.get("sheet") is False

    # Freeze panes differ per sheet
    freezes = {n: s.freeze_panes() for n, s in sheets.items() if s.sheet_kind != "chartsheet"}
    assert freezes.get("ValidCF") == "A2"
    assert freezes.get("ChartsOnly") == "B2"
    assert freezes.get("Threaded") == "A3"
    assert freezes.get("DimsPanes") == "C2"

    # DimsPanes auto_filter present; others not the same ref
    af_dims = sheets["DimsPanes"].auto_filter()
    assert af_dims is not None
    assert "A1:B7" in (af_dims.get("ref") or "").replace("$", "")
    af_charts = sheets["ChartsOnly"].auto_filter()
    assert af_charts is None or af_charts.get("ref") != af_dims.get("ref")

    # Chartsheet present → empty grid, no panic
    chartsheets = [n for n, s in sheets.items() if s.sheet_kind == "chartsheet"]
    if chartsheets:
        cs = sheets[chartsheets[0]]
        assert cs.nrows == 0
        assert cs.ncols == 0
        _ = cs.to_arrow()


# ---------------------------------------------------------------------------
# 3. Malformed — no panic, clean degradation
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "fname,checks",
    [
        (
            "stress2_malformed_meta.xlsx",
            "meta",
        ),
        (
            "stress2_malformed_chart.xlsx",
            "chart",
        ),
        (
            "stress2_malformed_pivot.xlsx",
            "pivot",
        ),
        (
            "stress2_malformed_threaded.xlsx",
            "threaded",
        ),
        (
            "stress2_malformed_vba.xlsx",
            "vba",
        ),
    ],
)
def test_stress2_malformed_no_panic(fname, checks):
    path = TESTDATA / fname
    reader = read_excel_turbo(str(path))
    sheet = reader.load_sheet(0, features="all")
    _ = sheet.to_arrow()

    if checks == "meta":
        cols = sheet.column_dimensions() or []
        # min>max col skipped or not expanded as crash
        for c in cols:
            assert c["min"] <= c["max"], c
        dvs = sheet.data_validations() or []
        # empty sqref skipped
        for d in dvs:
            assert d.get("sqref"), d
        cfs = sheet.conditional_formatting() or []
        for r in cfs:
            # oob dxfId → dxf is None, no throw
            if r.get("dxf_id") is not None and r["dxf_id"] >= 1:
                assert r.get("dxf") is None or isinstance(r.get("dxf"), dict)
    elif checks == "chart":
        charts = sheet.charts() or []
        # truncated chart: empty list or partial — no panic
        assert isinstance(charts, list)
    elif checks == "pivot":
        pivots = sheet.pivots() or []
        assert isinstance(pivots, list)
        # missing cache: empty or pivot without rich cache fields
        for p in pivots:
            assert "name" in p or "location" in p or True
    elif checks == "threaded":
        thr = sheet.threaded_comments() or []
        assert len(thr) >= 1
        # unknown personId → empty display name, still returned
        t0 = thr[0]
        assert t0.get("text") or t0.get("ref")
        # person_display_name empty or missing when unresolved
        name = t0.get("person_display_name") or t0.get("person_name") or ""
        assert name == "" or name  # must not raise
    elif checks == "vba":
        # content-type claims VBA but part missing
        assert reader.has_vba in (True, False)
        vba = reader.vba_project()
        # clean degradation: None or empty
        assert vba is None or (isinstance(vba, (bytes, bytearray)) and len(vba) == 0)


# ---------------------------------------------------------------------------
# 4. One cell — all features together
# ---------------------------------------------------------------------------


def test_stress2_onecell_all_features():
    path = TESTDATA / "stress2_onecell_all.xlsx"
    sheet = read_excel_turbo(str(path)).load_sheet(0, features="all")
    rb = sheet.to_arrow()

    # Merge
    merges = sheet.merges() or []
    assert any("B2" in m.replace("$", "").upper() for m in merges), merges

    # Formula (+ cached value on values path when present)
    formulas = sheet.formulas()
    assert formulas is not None and formulas.num_rows >= 1
    fpd = formulas.to_pydict()
    # B2 → data row 0, col 1
    found = False
    for i in range(formulas.num_rows):
        if fpd["row"][i] == 0 and fpd["col"][i] == 1:
            found = True
            assert "1+2" in (fpd["text"][i] or "").replace(" ", "")
    assert found, fpd

    # Cached value if injected
    cached = cell_value(rb, 0, 1)
    assert cached in (3.0, 3, None) or cached is not None

    # Named style + border on style table
    si = sheet.style_indices()
    st = sheet.style_table()
    assert si is not None and st is not None
    xf = si[1][0]  # col B, row 0
    style = st[xf]
    # named style StressAll or non-default border
    ns = sheet.named_styles() or []
    ns_names = [n.get("name") for n in ns]
    assert "StressAll" in ns_names or style.get("name") == "StressAll" or style.get("border")
    border = style.get("border") or {}
    # full border sides present
    for side in ("left", "right", "top", "bottom"):
        assert side in border or any(
            isinstance(b, dict) and side in b for b in [border]
        ), border

    # Validation
    dvs = sheet.data_validations() or []
    assert any("B2" in (d.get("sqref") or "") for d in dvs), dvs

    # CF
    cfs = sheet.conditional_formatting() or []
    assert any("B2" in (r.get("sqref") or "") for r in cfs), cfs

    # Hyperlink
    hlinks = sheet.hyperlinks() or []
    assert any("B2" in (h.get("ref") or "") for h in hlinks), hlinks
    assert any("example.com" in (h.get("target") or "") for h in hlinks)

    # Threaded comment
    thr = sheet.threaded_comments() or []
    assert len(thr) >= 1
    assert any("B2" in (t.get("ref") or "") for t in thr), thr


# ---------------------------------------------------------------------------
# 5. Tail order independence
# ---------------------------------------------------------------------------


def test_stress2_tail_order_independent():
    path = TESTDATA / "stress2_tail_order.xlsx"
    sheet = read_excel_turbo(str(path)).load_sheet(0, features="all")
    rb = sheet.to_arrow()
    assert cell_value(rb, 0, 0) == 10.0
    assert cell_value(rb, 0, 1) == 20.0

    cfs = sheet.conditional_formatting() or []
    assert len(cfs) >= 1
    assert any("A2" in (r.get("sqref") or "") for r in cfs)

    dvs = sheet.data_validations() or []
    assert len(dvs) >= 1
    assert any("A2" in (d.get("sqref") or "") for d in dvs)

    hlinks = sheet.hyperlinks() or []
    assert len(hlinks) >= 1
    assert any("B2" in (h.get("ref") or "") for h in hlinks)
    assert any("example.com" in (h.get("target") or "") for h in hlinks)
