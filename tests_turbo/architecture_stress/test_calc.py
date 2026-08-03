"""A4 Wave-1 architecture-stress tests: formula hydration/cache safety.

Harness-independent baseline for the A4 campaign. These tests are
self-contained: they drive the real write path (``write_excel_turbo`` with
``recalculate=True``) and assert against the saved XLSX artifact (raw XML),
exactly like ``tests_turbo/test_formula_hydration.py``.

Contract under test: a formula cell is preserved as text AND carries a computed
value; a formula kyrax cannot compute exactly stays UNCACHED and requests
``fullCalcOnLoad="1"`` - never a fabricated or retained old cache.

Deferred to A4.md (not asserted here): spill/dynamic-array regions, LAMBDA
non-scalar output, shared ``t="shared"`` formulas, refshift/overlay
invalidation legs (A3), hostile limits (A6), Excel COM oracles (A4/A6).
"""

from __future__ import annotations

import re
import sys
import zipfile
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "python"))

from kyrax import write_excel_turbo  # noqa: E402

_CELL = re.compile(rb"<c\s+([^>/]*?)(?:/>|>(.*?)</c>)", re.S)
_ATTR = re.compile(rb'(\w+)="([^"]*)"')
_F = re.compile(rb"<f[^>]*>(.*?)</f>", re.S)
_V = re.compile(rb"<v>(.*?)</v>", re.S)
_WB = re.compile(r"xl/workbook.xml")


def _read(path: Path) -> tuple[dict, dict]:
    """Return ``(values, formulas)`` keyed by 0-based (row, col) from raw XML."""
    with zipfile.ZipFile(path) as z:
        name = next(n for n in z.namelist() if re.match(r"xl/worksheets/sheet\d+\.xml", n))
        xml = z.read(name)
    values: dict = {}
    formulas: dict = {}
    for m in _CELL.finditer(xml):
        attrs = dict(_ATTR.findall(m.group(1)))
        ref = attrs.get(b"r", b"").decode()
        if not ref:
            continue
        rc = _rc(ref)
        body = m.group(2) or b""
        fm = _F.search(body)
        if fm:
            formulas[rc] = fm.group(1).decode()
        vm = _V.search(body)
        if not vm:
            continue
        raw = vm.group(1).decode()
        t = attrs.get(b"t", b"").decode()
        if t == "b":
            values[rc] = raw == "1"
        elif t in ("str", "e", "inlineStr", "s"):
            values[rc] = raw
        else:
            values[rc] = float(raw)
    return values, formulas


def _rc(ref: str) -> tuple[int, int]:
    m = re.match(r"([A-Z]+)(\d+)", ref)
    col = 0
    for ch in m.group(1):
        col = col * 26 + (ord(ch) - 64)
    return int(m.group(2)) - 1, col - 1


def _full_calc_on_load(path: Path) -> bool:
    with zipfile.ZipFile(path) as z:
        name = next(n for n in z.namelist() if _WB.search(n))
        xml = z.read(name).decode("utf-8")
    return 'fullCalcOnLoad="1"' in xml


def _sheet(formulas: dict, rows: list[list] | None = None) -> list[dict]:
    return [{"name": "Sheet1", "rows": rows or [], "formulas": formulas}]


def _write(path: Path, formulas: dict, rows: list[list] | None = None, recalculate: bool = True):
    write_excel_turbo(str(path), _sheet(formulas, rows), recalculate=recalculate)


# ---------------------------------------------------------------------------
# A4-HYD-01: cache safety - computed cells carry a value, fallbacks never do.
# ---------------------------------------------------------------------------


def test_recalculate_writes_value_alongside_formula(tmp_path):
    path = tmp_path / "hydrated.xlsx"
    _write(path, {(0, 0): "=1+2", (0, 1): "=A1*10", (0, 2): '="a"&"b"'}, recalculate=True)
    values, formulas = _read(path)
    assert formulas[(0, 0)].lstrip("=") == "1+2"
    assert values.get((0, 0)) == 3
    assert values.get((0, 1)) == 30
    assert values.get((0, 2)) == "ab"


def test_without_recalculate_nothing_is_computed(tmp_path):
    path = tmp_path / "plain.xlsx"
    _write(path, {(0, 0): "=1+2"}, recalculate=False)
    values, formulas = _read(path)
    assert formulas[(0, 0)].lstrip("=") == "1+2"
    assert (0, 0) not in values


@pytest.mark.parametrize(
    "formula",
    [
        "=[Book2.xlsx]Sheet1!A1",  # external reference
        "=1+",  # unparseable
    ],
)
def test_uncomputable_formula_stays_uncached_and_requests_recalc(tmp_path, formula):
    path = tmp_path / "fallback.xlsx"
    _write(path, {(0, 0): formula}, recalculate=True)
    values, formulas = _read(path)
    assert (0, 0) in formulas, "formula text must be preserved"
    assert (0, 0) not in values, (
        f"{formula} must remain UNCACHED (no <v>), got {values.get((0, 0))!r}"
    )
    assert _full_calc_on_load(path), "fallback workbook must request fullCalcOnLoad"


def test_unknown_function_is_cached_name_error(tmp_path):
    # A parseable-but-unknown function is NOT an "I could not compute this"
    # fallback: it evaluates to Excel's own #NAME? error, which is a legal
    # cached `t="e"` value. The oracle is the error code, not an uncached cell.
    path = tmp_path / "name.xlsx"
    _write(path, {(0, 0): "=NOSUCHFUNCTION(1,2)"}, recalculate=True)
    values, formulas = _read(path)
    assert (0, 0) in formulas, "formula text must be preserved"
    assert values.get((0, 0)) == "#NAME?", (
        f"expected a cached #NAME? error, got {values.get((0, 0))!r}"
    )


# ---------------------------------------------------------------------------
# A4-FN-01 / A4-REG-01: function semantics through the real evaluation path.
# ---------------------------------------------------------------------------


def test_sum_over_plain_cells(tmp_path):
    path = tmp_path / "sum.xlsx"
    _write(path, {(2, 0): "=SUM(A1:A2)"}, rows=[[10], [32]], recalculate=True)
    values, _ = _read(path)
    assert values.get((2, 0)) == 42


def test_if_upper_len_basics(tmp_path):
    path = tmp_path / "text.xlsx"
    _write(
        path,
        {(0, 0): '=IF(1>0,"yes","no")', (0, 1): '=UPPER("ab")', (0, 2): '=LEN("abcd")'},
        recalculate=True,
    )
    values, _ = _read(path)
    assert values.get((0, 0)) == "yes"
    assert values.get((0, 1)) == "AB"
    assert values.get((0, 2)) == 4


# ---------------------------------------------------------------------------
# A4-DATE-01: 1900-system serial exact oracle (1904 delta deferred).
# ---------------------------------------------------------------------------


def test_date_serial_1900_matches_excel(tmp_path):
    path = tmp_path / "date.xlsx"
    _write(path, {(0, 0): "=DATE(2020,1,15)", (0, 1): "=DATE(2020,1,15)+1"}, recalculate=True)
    values, _ = _read(path)
    assert values.get((0, 0)) == 43845
    assert values.get((0, 1)) == 43846


# ---------------------------------------------------------------------------
# A4-CYCLE-01: circular references never get a value, never hang.
# ---------------------------------------------------------------------------


def test_self_cycle_is_uncached_and_requests_recalc(tmp_path):
    path = tmp_path / "selfcycle.xlsx"
    _write(path, {(0, 0): "=A1"}, recalculate=True)
    values, formulas = _read(path)
    assert (0, 0) in formulas
    assert (0, 0) not in values, "self-cyclic cell must never carry a value"
    assert _full_calc_on_load(path)


def test_two_cell_cycle_is_uncached_and_requests_recalc(tmp_path):
    path = tmp_path / "cycle.xlsx"
    _write(path, {(0, 0): "=B1", (0, 1): "=A1"}, recalculate=True)
    values, formulas = _read(path)
    assert (0, 0) in formulas and (0, 1) in formulas
    assert (0, 0) not in values and (0, 1) not in values, "A1<->B1 must stay uncached"
    assert _full_calc_on_load(path)


# ---------------------------------------------------------------------------
# A4-HYD-02: mutating an input recomputes all and only transitive dependents.
# ---------------------------------------------------------------------------


def test_mutation_invalidates_only_transitive_dependents(tmp_path):
    path1 = tmp_path / "before.xlsx"
    path2 = tmp_path / "after.xlsx"
    formulas = {(1, 0): "=A1+1", (2, 0): "=A2+1", (0, 1): "=5"}
    rows = [[1]]
    _write(path1, formulas, rows=rows, recalculate=True)
    rows2 = [[10]]
    _write(path2, formulas, rows=rows2, recalculate=True)
    v1, _ = _read(path1)
    v2, _ = _read(path2)
    # transitive dependents of A1 recomputed
    assert v2.get((1, 0)) == 11 and v1.get((1, 0)) == 2
    assert v2.get((2, 0)) == 12 and v1.get((2, 0)) == 3
    # independent cell B1 untouched by the mutation
    assert v1.get((0, 1)) == 5 and v2.get((0, 1)) == 5


# ---------------------------------------------------------------------------
# A4-DIFF-01 / P08 proxy: identical input -> byte-identical deterministic save.
# ---------------------------------------------------------------------------


def test_writer_is_deterministic_same_input_same_bytes(tmp_path):
    path1 = tmp_path / "a.xlsx"
    path2 = tmp_path / "b.xlsx"
    formulas = {(0, 0): "=1+2", (1, 0): "=A1*10", (2, 0): "=SUM(A1:A2)"}
    _write(path1, formulas, recalculate=True)
    _write(path2, formulas, recalculate=True)
    assert path1.read_bytes() == path2.read_bytes(), "deterministic writer -> diff-clean save"


# ---------------------------------------------------------------------------
# A4-LAMBDA-01: KNOWN-GAP probes (end-to-end through public hydrate).
#
# Public hydrate routes every LAMBDA-family formula to fallback in Wave-1
# (verified: LET -> fallback 2/2, MAP/REDUCE -> fallback 2/2). These are
# deterministic NEGATIVE capability probes that pin the gap and will fail the
# moment a real seam computes them - they are never PASS claims.
# ---------------------------------------------------------------------------


def test_let_is_uncached_fallback_known_gap(tmp_path):
    path = tmp_path / "let_gap.xlsx"
    _write(
        path,
        {(0, 0): "=LET(x,10,x*2)", (0, 1): "=LET(a,5,LET(b,a+1,b*2))"},
        recalculate=True,
    )
    values, formulas = _read(path)
    assert (0, 0) in formulas and (0, 1) in formulas, "formula text must be preserved"
    assert (0, 0) not in values and (0, 1) not in values, "LET must stay UNCACHED (KNOWN-GAP)"
    assert _full_calc_on_load(path)


def test_map_reduce_is_uncached_fallback_known_gap(tmp_path):
    path = tmp_path / "lambda_gap.xlsx"
    _write(
        path,
        {
            (0, 0): "=SUM(MAP({1,2,3},LAMBDA(a,a*2)))",
            (0, 1): "=REDUCE(0,{1,2,3},LAMBDA(a,b,a+b))",
        },
        recalculate=True,
    )
    values, formulas = _read(path)
    assert (0, 0) in formulas and (0, 1) in formulas, "formula text must be preserved"
    assert (0, 0) not in values and (0, 1) not in values, (
        "lambda consumers must stay UNCACHED (KNOWN-GAP)"
    )
    assert _full_calc_on_load(path)
