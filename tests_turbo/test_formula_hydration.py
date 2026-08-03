"""Formula hydration: a saved formula cell carries its text AND a value.

This is the openpyxl comparison the architecture note makes — ``data_only``
forces a choice between the formula and its cached value, and kyrax refuses to
make that choice. So every test here reads *both* surfaces back from the same
file and asserts they agree.

The other half of the contract matters just as much: when kyrax cannot compute a
formula exactly it must leave the cell alone rather than invent a number. The
fallback tests below assert the *absence* of a value, never a fabricated one.
"""

from __future__ import annotations

import re
import sys
import zipfile
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "python"))

from kyrax import write_excel_turbo  # noqa: E402

_CELL = re.compile(rb"<c\s+([^>/]*?)(?:/>|>(.*?)</c>)", re.S)
_ATTR = re.compile(rb'(\w+)="([^"]*)"')
_F = re.compile(rb"<f[^>]*>(.*?)</f>", re.S)
_V = re.compile(rb"<v>(.*?)</v>", re.S)


def _sheet(formulas: dict, rows: list[list] | None = None) -> list[dict]:
    return [{"name": "Sheet1", "rows": rows or [], "formulas": formulas}]


def _rc(ref: str) -> tuple[int, int]:
    """``A1`` -> ``(0, 0)``."""
    m = re.match(r"([A-Z]+)(\d+)", ref)
    letters, digits = m.group(1), m.group(2)
    col = 0
    for ch in letters:
        col = col * 26 + (ord(ch) - 64)
    return int(digits) - 1, col - 1


def sheet_xml(path: Path) -> str:
    with zipfile.ZipFile(path) as z:
        name = next(
            n for n in z.namelist() if re.match(r"xl/worksheets/sheet\d+\.xml", n)
        )
        return z.read(name).decode("utf-8")


def _read(path: Path):
    """Return ``(values_by_rc, formulas_by_rc)`` straight from the saved XML.

    Parsing the artifact rather than going back through the reader keeps this
    honest: it asserts what Excel will actually open, and cannot be fooled by a
    reader convenience (its arrow view treats row 1 as a header row, which
    would silently hide the very cells under test).
    """
    xml = sheet_xml(path).encode("utf-8")
    values: dict[tuple[int, int], object] = {}
    formulas: dict[tuple[int, int], str] = {}
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


def test_recalculate_writes_formula_and_value_together(tmp_path):
    """The core promise: read the file back and get both, from one save."""
    path = tmp_path / "hydrated.xlsx"
    write_excel_turbo(
        str(path),
        _sheet(
            {(0, 0): "=1+2", (0, 1): "=A1*10", (0, 2): '="a"&"b"'},
        ),
        recalculate=True,
    )
    values, formulas = _read(path)

    # the formula text survives
    assert formulas[(0, 0)].lstrip("=") == "1+2"
    assert formulas[(0, 1)].lstrip("=") == "A1*10"
    # ...and a value was computed for it
    assert values.get((0, 0)) == 3
    assert values.get((0, 1)) == 30
    assert values.get((0, 2)) == "ab"


def test_dependency_order_is_respected(tmp_path):
    """B1 reads A1, C1 reads B1 — written out of order on purpose."""
    path = tmp_path / "chain.xlsx"
    write_excel_turbo(
        str(path),
        _sheet({(0, 2): "=B1+1", (0, 1): "=A1+1", (0, 0): "=1"}),
        recalculate=True,
    )
    values, _ = _read(path)
    assert values.get((0, 0)) == 1
    assert values.get((0, 1)) == 2
    assert values.get((0, 2)) == 3


def test_values_feed_formulas(tmp_path):
    """Plain data cells are visible to the formulas that read them."""
    path = tmp_path / "withdata.xlsx"
    write_excel_turbo(
        str(path),
        _sheet({(2, 0): "=SUM(A1:A2)"}, rows=[[10], [32]]),
        recalculate=True,
    )
    values, _ = _read(path)
    assert values.get((2, 0)) == 42


def test_without_recalculate_nothing_is_computed(tmp_path):
    """Hydration is opt-in; the default write path must not change."""
    path = tmp_path / "plain.xlsx"
    write_excel_turbo(str(path), _sheet({(0, 0): "=1+2"}))
    values, formulas = _read(path)
    assert formulas[(0, 0)].lstrip("=") == "1+2"
    assert (0, 0) not in values


# ---------------------------------------------------------------------------
# The other half of the contract: never emit a wrong number.
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "formula",
    [
        "=NOSUCHFUNCTION(1,2)",          # unsupported function
        "=[Book2.xlsx]Sheet1!A1",        # external reference
        "=1+",                           # unparseable
    ],
)
def test_uncomputable_formulas_never_get_a_number(tmp_path, formula):
    path = tmp_path / "fallback.xlsx"
    write_excel_turbo(str(path), _sheet({(0, 0): formula}), recalculate=True)
    values, formulas = _read(path)
    # the formula itself is preserved
    assert (0, 0) in formulas
    # ...and whatever happened, it is not a fabricated number
    assert not isinstance(values.get((0, 0)), (int, float)) or isinstance(
        values.get((0, 0)), bool
    ), f"{formula} produced the number {values.get((0, 0))!r}"


def test_circular_reference_is_left_uncomputed(tmp_path):
    path = tmp_path / "cycle.xlsx"
    write_excel_turbo(
        str(path),
        _sheet({(0, 0): "=B1+1", (0, 1): "=A1+1"}),
        recalculate=True,
    )
    values, formulas = _read(path)
    assert (0, 0) in formulas and (0, 1) in formulas
    assert (0, 0) not in values, "a cycle must not produce a value"
    assert (0, 1) not in values


def test_a_cell_reading_an_uncomputable_cell_is_also_left_alone(tmp_path):
    """The poisoning rule: B1 must not be computed from a blank A1."""
    path = tmp_path / "poison.xlsx"
    write_excel_turbo(
        str(path),
        _sheet({(0, 0): "=1+", (0, 1): "=A1*2"}),
        recalculate=True,
    )
    values, _ = _read(path)
    assert (0, 1) not in values, "B1 was computed from an uncomputable A1"


def test_full_calc_on_load_is_set(tmp_path):
    """Excel must be told to finish the job for anything kyrax skipped."""
    path = tmp_path / "flag.xlsx"
    write_excel_turbo(
        str(path), _sheet({(0, 0): "=NOSUCHFUNCTION(1)"}), recalculate=True
    )
    with zipfile.ZipFile(path) as z:
        wb_xml = z.read("xl/workbook.xml").decode("utf-8")
    assert 'fullCalcOnLoad="1"' in wb_xml


def test_computed_string_and_bool_carry_a_result_type(tmp_path):
    """A non-numeric cache needs ``t=`` or Excel reads it as a number."""
    path = tmp_path / "typed.xlsx"
    write_excel_turbo(
        str(path),
        _sheet({(0, 0): '="x"&"y"', (0, 1): "=1=1"}),
        recalculate=True,
    )
    xml = sheet_xml(path)
    assert 't="str"' in xml, "computed string cache is untyped"
    assert 't="b"' in xml, "computed boolean cache is untyped"
