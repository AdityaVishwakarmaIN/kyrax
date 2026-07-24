"""Integration coherence for the expanded turbo reader (MERGE M3).

(a) features=\"all\" turns on every flag — accessors non-None (empty OK).
(b) Flag + accessor naming is snake_case, no synonyms.
(c) Flag off → None; flag on + missing part → empty (no panics).
(d) Selective single-flag loads on gap fixtures.
(e) One-read guarantee: all requested surfaces from one load_sheet.
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "python"))

from kyrax import read_excel_turbo  # noqa: E402

TESTDATA = ROOT / "testdata"

# Canonical feature flag names (must match Rust parse_features + __init__ docs).
ALL_FLAGS = [
    "styles",
    "formulas",
    "merges",
    "defined_names",
    "tables",
    "hyperlinks",
    "comments",
    "sheet_meta",
    "page_setup",
    "workbook_meta",
    "validations",
    "cond_format",
    "charts",
    "pivots",
    "vba",
]

# Fixtures rich enough to exercise most surfaces under features=all.
GAP_FIXTURES = {
    "gap_richmeta.xlsx": "rich cell meta",
    "gap_sheetmeta.xlsx": "sheet/workbook meta",
    "charts.xlsx": "charts + chartsheet",
    "pivot.xlsx": "pivots",
    "threaded.xlsx": "threaded comments",
    "vba.xlsm": "VBA",
    "structured.xlsx": "merges/tables/hyperlinks/names",
    "formulas.xlsx": "formulas",
    "comments.xlsx": "legacy comments",
    "styled.xlsx": "styles",
}

# Single-flag → fixture: non_none accessors + nonempty accessors (subset).
# (flag, fixture, sheet_idx_or_name, non_none_checks, nonempty_checks)
SELECTIVE_CASES = [
    ("styles", "gap_richmeta.xlsx", 0, ["style_table", "style_indices", "named_styles"], ["style_table", "named_styles"]),
    ("formulas", "formulas.xlsx", 0, ["formulas"], ["formulas"]),
    ("merges", "structured.xlsx", 0, ["merges"], ["merges"]),
    ("defined_names", "structured.xlsx", 0, ["defined_names"], ["defined_names"]),
    ("tables", "structured.xlsx", 0, ["tables"], ["tables"]),
    ("hyperlinks", "structured.xlsx", 0, ["hyperlinks"], ["hyperlinks"]),
    ("comments", "comments.xlsx", 0, ["comments", "comment_authors", "threaded_comments"], ["comments", "comment_authors"]),
    ("comments", "threaded.xlsx", 0, ["comments", "threaded_comments", "persons"], ["comments", "threaded_comments", "persons"]),
    ("sheet_meta", "gap_sheetmeta.xlsx", "MetaMain", ["row_dimensions", "column_dimensions", "sheet_view", "protection"], ["row_dimensions", "column_dimensions", "sheet_view", "protection"]),
    ("page_setup", "gap_sheetmeta.xlsx", "MetaMain", ["page_setup", "page_margins", "print_options", "header_footer"], ["page_setup", "page_margins"]),
    ("workbook_meta", "gap_sheetmeta.xlsx", 0, ["workbook_props"], ["workbook_props"]),
    ("validations", "gap_richmeta.xlsx", 0, ["data_validations"], ["data_validations"]),
    ("cond_format", "gap_richmeta.xlsx", 0, ["conditional_formatting"], ["conditional_formatting"]),
    ("charts", "charts.xlsx", "Sales", ["charts"], ["charts"]),
    ("pivots", "pivot.xlsx", 0, ["pivots"], ["pivots"]),
    ("vba", "vba.xlsm", 0, ["vba_project"], ["vba_project"]),
]

# Sheet-level accessors gated by a feature flag (None when flag off).
SHEET_GATED = [
    ("styles", ["style_indices", "style_table", "named_styles"]),
    ("formulas", ["formulas"]),
    ("merges", ["merges"]),
    ("tables", ["tables"]),
    ("hyperlinks", ["hyperlinks"]),
    ("comments", ["comments", "comment_authors", "threaded_comments"]),
    ("sheet_meta", ["row_dimensions", "column_dimensions", "sheet_format", "sheet_view", "protection"]),
    ("page_setup", ["page_setup", "page_margins", "print_options", "header_footer"]),
    ("validations", ["data_validations"]),
    ("cond_format", ["conditional_formatting"]),
    ("charts", ["charts"]),
    ("pivots", ["pivots"]),
]

# Reader-level accessors gated by a feature flag.
READER_GATED = [
    ("defined_names", ["defined_names"]),
    ("tables", ["tables"]),  # reader.tables()
    ("workbook_meta", ["workbook_props"]),
    ("comments", ["persons"]),
    ("vba", ["vba_project"]),
]


def _call_sheet(sheet, name: str):
    attr = getattr(sheet, name)
    return attr() if callable(attr) else attr


def _call_reader(reader, name: str):
    attr = getattr(reader, name)
    return attr() if callable(attr) else attr


def _is_empty(val) -> bool:
    if val is None:
        return True
    if hasattr(val, "num_rows"):
        return val.num_rows == 0
    if isinstance(val, (list, dict, tuple, str, bytes)):
        return len(val) == 0
    return False


# ---------------------------------------------------------------------------
# (a) features=\"all\" turns on every flag — accessors non-None
# ---------------------------------------------------------------------------


def test_all_flags_list_matches_docs():
    """Canonical flag set is snake_case and matches the public docstring set."""
    for f in ALL_FLAGS:
        assert f == f.lower()
        assert " " not in f
        assert f.replace("_", "").isalnum()
    # No synonym pairs for the same concept
    assert "conditional_formatting" not in ALL_FLAGS  # flag is cond_format
    assert "data_validations" not in ALL_FLAGS  # flag is validations
    assert "sheet_metadata" not in ALL_FLAGS
    assert "workbook_props" not in ALL_FLAGS  # flag is workbook_meta


def test_features_all_every_accessor_non_none_on_rich_fixtures():
    """features='all' enables every flag; gated accessors return non-None containers.

    Presence-optional fields (auto_filter, freeze_panes, code_name, tab_color,
    vba_project when absent) may still be None when the file lacks that part.
    """
    # Use structured + gap fixtures so each surface is exercised somewhere.
    paths = [
        TESTDATA / "structured.xlsx",
        TESTDATA / "gap_richmeta.xlsx",
        TESTDATA / "gap_sheetmeta.xlsx",
        TESTDATA / "formulas.xlsx",
        TESTDATA / "comments.xlsx",
        TESTDATA / "charts.xlsx",
        TESTDATA / "pivot.xlsx",
        TESTDATA / "threaded.xlsx",
        TESTDATA / "vba.xlsm",
        TESTDATA / "mixed.xlsx",
    ]
    # Accumulators: at least one fixture must yield non-None for each gated accessor
    seen_sheet: dict[str, bool] = {}
    seen_reader: dict[str, bool] = {}

    for path in paths:
        assert path.exists(), path
        reader = read_excel_turbo(str(path))
        # pick first worksheet if present else first sheet
        sheet = None
        for name in reader.sheet_names:
            sh = reader.load_sheet(name, features="all")
            if sh.sheet_kind == "worksheet" or sheet is None:
                sheet = sh
            if sh.sheet_kind == "worksheet":
                break
        assert sheet is not None

        for _flag, names in SHEET_GATED:
            for n in names:
                val = _call_sheet(sheet, n)
                # With features=all these must not be None (empty OK).
                assert val is not None, f"{path.name}: {n} is None under features=all"
                seen_sheet[n] = True

        for n in ("defined_names", "workbook_props", "persons"):
            val = _call_reader(reader, n)
            assert val is not None, f"{path.name}: reader.{n} is None under features=all"
            seen_reader[n] = True

        # tables on reader
        assert reader.tables() is not None
        # vba: has_vba is bool; vba_project may be None when absent
        assert isinstance(reader.has_vba, bool)
        if path.name == "vba.xlsm":
            assert reader.has_vba is True
            assert reader.vba_project() is not None
        else:
            # requested, but file may lack the part
            assert reader.has_vba is False
            assert reader.vba_project() is None

        # always-on surfaces
        _ = sheet.to_arrow()
        _ = sheet.cell_errors()
        assert sheet.name
        assert sheet.sheet_state in ("visible", "hidden", "veryHidden")
        assert sheet.sheet_kind in ("worksheet", "chartsheet")

    for _flag, names in SHEET_GATED:
        for n in names:
            assert seen_sheet.get(n), f"accessor {n} never seen non-None"


def test_features_all_includes_every_flag_via_selective_union():
    """Explicit list of all flags equals features='all' for gated surfaces."""
    path = TESTDATA / "gap_richmeta.xlsx"
    r_all = read_excel_turbo(str(path))
    s_all = r_all.load_sheet(0, features="all")
    r_list = read_excel_turbo(str(path))
    s_list = r_list.load_sheet(0, features=list(ALL_FLAGS))

    assert (s_all.style_table() is not None) == (s_list.style_table() is not None)
    assert (s_all.data_validations() is not None) == (s_list.data_validations() is not None)
    assert (s_all.conditional_formatting() is not None) == (
        s_list.conditional_formatting() is not None
    )
    assert len(s_all.data_validations() or []) == len(s_list.data_validations() or [])
    assert len(s_all.conditional_formatting() or []) == len(
        s_list.conditional_formatting() or []
    )
    assert (r_all.workbook_props() is not None) == (r_list.workbook_props() is not None)


# ---------------------------------------------------------------------------
# (c) flag off → None; no panics when part missing
# ---------------------------------------------------------------------------


def test_values_only_all_gated_none():
    reader = read_excel_turbo(str(TESTDATA / "gap_richmeta.xlsx"))
    sheet = reader.load_sheet(0, features="values")
    for _flag, names in SHEET_GATED:
        for n in names:
            assert _call_sheet(sheet, n) is None, f"{n} should be None under values-only"
    assert reader.defined_names() is None
    assert reader.tables() is None
    assert reader.workbook_props() is None
    assert reader.persons() is None
    assert reader.has_vba is False
    assert reader.vba_project() is None
    # always available
    rb = sheet.to_arrow()
    assert rb.num_rows == sheet.nrows
    errs = sheet.cell_errors()
    assert errs is not None and errs.num_rows >= 0


def test_flag_off_returns_none_no_throw():
    path = TESTDATA / "gap_sheetmeta.xlsx"
    reader = read_excel_turbo(str(path))
    # only sheet_meta — page_setup / validations / charts etc must be None
    sheet = reader.load_sheet("MetaMain", features=["sheet_meta"])
    assert sheet.row_dimensions() is not None
    assert sheet.page_setup() is None
    assert sheet.data_validations() is None
    assert sheet.charts() is None
    assert sheet.pivots() is None
    assert sheet.formulas() is None
    assert sheet.style_table() is None
    assert reader.workbook_props() is None


def test_missing_part_returns_empty_not_raise():
    """File lacks charts/pivots/comments → empty containers when flags on."""
    reader = read_excel_turbo(str(TESTDATA / "mixed.xlsx"))
    sheet = reader.load_sheet(0, features="all")
    assert sheet.charts() == []
    assert sheet.pivots() == []
    assert sheet.comments() is not None and sheet.comments().num_rows == 0
    assert sheet.threaded_comments() == []
    assert sheet.formulas() is not None and sheet.formulas().num_rows == 0
    assert sheet.merges() == []
    assert sheet.hyperlinks() == []
    assert sheet.data_validations() == []
    assert sheet.conditional_formatting() == []
    assert reader.persons() == []
    assert reader.has_vba is False
    assert reader.vba_project() is None


# ---------------------------------------------------------------------------
# (d) selective single-flag loads
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "flag,fixture,idx,non_none,nonempty",
    SELECTIVE_CASES,
    ids=[f"{c[0]}@{c[1]}" for c in SELECTIVE_CASES],
)
def test_selective_single_flag(flag, fixture, idx, non_none, nonempty):
    path = TESTDATA / fixture
    reader = read_excel_turbo(str(path))
    sheet = reader.load_sheet(idx, features=[flag])

    def resolve(name: str):
        if name in ("defined_names", "workbook_props", "persons", "vba_project"):
            return _call_reader(reader, name)
        if name == "tables" and flag == "tables":
            val = sheet.tables()
            return val if val is not None else reader.tables()
        return _call_sheet(sheet, name)

    # Requested flag surfaces present
    for name in non_none:
        val = resolve(name)
        assert val is not None, f"{flag}: {name} is None"
    for name in nonempty:
        val = resolve(name)
        assert not _is_empty(val), f"{flag}: {name} unexpectedly empty on {fixture}"

    # Everything else gated must be None
    for f, names in SHEET_GATED:
        if f == flag:
            continue
        for n in names:
            # COND_FORMAT may parse styles.xml for dxfs → style_table / named_styles surface
            if flag == "cond_format" and n in ("style_table", "style_indices", "named_styles"):
                continue
            val = _call_sheet(sheet, n)
            assert val is None, f"flag={flag}: {n} should be None (owned by {f}), got {type(val)}"

    for f, names in READER_GATED:
        if f == flag:
            continue
        for n in names:
            val = _call_reader(reader, n)
            if n == "vba_project":
                assert reader.has_vba is False
                assert val is None
            else:
                assert val is None, f"flag={flag}: reader.{n} should be None"


# ---------------------------------------------------------------------------
# (e) ONE-READ GUARANTEE
# ---------------------------------------------------------------------------


def test_one_read_gap_richmeta():
    """values+styles+validations+cf+named_styles in ONE load_sheet."""
    reader = read_excel_turbo(str(TESTDATA / "gap_richmeta.xlsx"))
    sheet = reader.load_sheet(0, features="all")

    rb = sheet.to_arrow()
    assert rb.num_rows == sheet.nrows > 0
    assert sheet.style_table() is not None and len(sheet.style_table()) > 0
    assert sheet.style_indices() is not None
    assert sheet.named_styles() is not None and len(sheet.named_styles()) >= 1
    assert sheet.data_validations() is not None and len(sheet.data_validations()) >= 1
    assert sheet.conditional_formatting() is not None and len(sheet.conditional_formatting()) >= 1
    # formulas/merges/etc empty but non-None
    assert sheet.formulas() is not None
    assert sheet.merges() is not None
    assert sheet.charts() is not None
    assert sheet.pivots() is not None
    assert sheet.comments() is not None
    assert sheet.threaded_comments() is not None
    assert reader.defined_names() is not None
    assert reader.workbook_props() is not None
    assert reader.persons() is not None


def test_one_read_gap_sheetmeta():
    """dims+views+protection+pagesetup+workbook props in ONE load_sheet."""
    reader = read_excel_turbo(str(TESTDATA / "gap_sheetmeta.xlsx"))
    sheet = reader.load_sheet("MetaMain", features="all")

    _ = sheet.to_arrow()
    assert sheet.row_dimensions() is not None
    assert sheet.column_dimensions() is not None
    assert sheet.sheet_format() is not None
    assert sheet.sheet_view() is not None
    assert sheet.protection() is not None
    assert sheet.auto_filter() is not None
    assert sheet.page_setup() is not None
    assert sheet.page_margins() is not None
    assert sheet.print_options() is not None
    assert sheet.header_footer() is not None
    assert reader.workbook_props() is not None
    assert reader.date1904 is False
    # structural empties still non-None
    assert sheet.charts() is not None
    assert sheet.data_validations() is not None


def test_one_read_structured_and_parts():
    """values+styles+formulas+merges+names+tables+hyperlinks+comments(+threaded)
    +validations+cf+dims+pagesetup+charts/pivots-if-present — one call per sheet.
    """
    # structured: merges/names/tables/hyperlinks
    r = read_excel_turbo(str(TESTDATA / "structured.xlsx"))
    s = r.load_sheet(0, features="all")
    _ = s.to_arrow()
    assert s.style_table() is not None
    assert s.formulas() is not None
    assert s.merges() is not None and len(s.merges()) > 0
    assert s.hyperlinks() is not None and len(s.hyperlinks()) > 0
    assert s.tables() is not None and len(s.tables()) > 0
    assert r.defined_names() is not None and len(r.defined_names()) > 0
    assert s.comments() is not None
    assert s.threaded_comments() is not None
    assert s.data_validations() is not None
    assert s.conditional_formatting() is not None
    assert s.row_dimensions() is not None
    assert s.page_setup() is not None
    assert s.charts() is not None
    assert s.pivots() is not None

    # charts
    r2 = read_excel_turbo(str(TESTDATA / "charts.xlsx"))
    for name in r2.sheet_names:
        sh = r2.load_sheet(name, features="all")
        _ = sh.to_arrow()
        assert sh.charts() is not None
    sales = r2.load_sheet("Sales", features="all")
    assert len(sales.charts()) >= 1

    # pivots
    r3 = read_excel_turbo(str(TESTDATA / "pivot.xlsx"))
    sp = r3.load_sheet(0, features="all")
    assert sp.pivots() is not None and len(sp.pivots()) >= 1

    # threaded + legacy
    r4 = read_excel_turbo(str(TESTDATA / "threaded.xlsx"))
    st = r4.load_sheet(0, features="all")
    assert st.comments() is not None and st.comments().num_rows >= 1
    assert st.threaded_comments() is not None and len(st.threaded_comments()) >= 1
    assert r4.persons() is not None and len(r4.persons()) >= 1

    # vba
    r5 = read_excel_turbo(str(TESTDATA / "vba.xlsm"))
    _ = r5.load_sheet(0, features="all")
    assert r5.has_vba is True
    assert r5.vba_project() is not None and len(r5.vba_project()) > 0


def test_one_read_no_second_pass_needed():
    """Touching every accessor after a single load_sheet does not require reload."""
    reader = read_excel_turbo(str(TESTDATA / "gap_richmeta.xlsx"))
    sheet = reader.load_sheet(0, features="all")
    # First pass materialize
    bundle = {
        "arrow": sheet.to_arrow(),
        "errors": sheet.cell_errors(),
        "styles": sheet.style_table(),
        "si": sheet.style_indices(),
        "named": sheet.named_styles(),
        "formulas": sheet.formulas(),
        "merges": sheet.merges(),
        "hyperlinks": sheet.hyperlinks(),
        "comments": sheet.comments(),
        "threaded": sheet.threaded_comments(),
        "charts": sheet.charts(),
        "pivots": sheet.pivots(),
        "tables": sheet.tables(),
        "row_dims": sheet.row_dimensions(),
        "col_dims": sheet.column_dimensions(),
        "sheet_format": sheet.sheet_format(),
        "auto_filter": sheet.auto_filter(),
        "sheet_view": sheet.sheet_view(),
        "protection": sheet.protection(),
        "page_setup": sheet.page_setup(),
        "page_margins": sheet.page_margins(),
        "print_options": sheet.print_options(),
        "header_footer": sheet.header_footer(),
        "validations": sheet.data_validations(),
        "cf": sheet.conditional_formatting(),
        "names": reader.defined_names(),
        "wb_tables": reader.tables(),
        "wb_props": reader.workbook_props(),
        "persons": reader.persons(),
        "vba": reader.vba_project(),
    }
    # Second pass: same objects still available (idempotent accessors)
    assert sheet.style_table() is not None
    assert sheet.data_validations() == bundle["validations"]
    assert sheet.conditional_formatting() == bundle["cf"]
    assert reader.workbook_props() is not None
