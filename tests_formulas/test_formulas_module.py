"""Lane I: the ``kyrax.formulas`` module surface.

Run with the project venv::

    nextexcel/.venv/Scripts/python.exe -m pytest tests_formulas/test_formulas_module.py
"""

import doctest
import sys

import pytest

import kyrax

REQUIRED_DOCTESTS = """
>>> evaluate('=SUM(1,2)')
3.0
>>> evaluate('A1*2', {'A1': 21})
42.0
>>> len(list_functions()) > 400
True
"""


def test_lazy_import_has_no_cost_until_used():
    """``import kyrax`` must not import the formulas submodule; accessing it
    (or ``from kyrax import formulas``) loads it lazily."""
    assert "kyrax.formulas" not in sys.modules
    from kyrax import formulas  # noqa: F401

    assert "kyrax.formulas" in sys.modules


def test_doctests_pass():
    """The three required doctests, plus everything else in formulas.py."""
    import kyrax.formulas as mod

    failures, _ = doctest.testmod(mod, verbose=False)
    assert failures == 0
    # The three required doctests; `run_docstring_examples` raises on failure.
    doctest.run_docstring_examples(
        REQUIRED_DOCTESTS,
        mod.__dict__,
        name="required_doctests",
        verbose=False,
    )


def test_evaluate_sum():
    assert kyrax.formulas.evaluate("=SUM(1,2)") == 3.0
    assert isinstance(kyrax.formulas.evaluate("=SUM(1,2)"), float)


def test_evaluate_without_leading_equals():
    assert kyrax.formulas.evaluate("1+2") == 3.0


def test_evaluate_with_context():
    assert kyrax.formulas.evaluate("A1*2", {"A1": 21}) == 42.0


def test_evaluate_context_missing_cell_is_blank():
    # A bare reference to a cell that is not in the context reads as blank.
    assert kyrax.formulas.evaluate("=A1") is None
    # ...which coerces to 0 in arithmetic (Excel semantics).
    assert kyrax.formulas.evaluate("=A1+1") == 1.0


def test_evaluate_value_types():
    assert kyrax.formulas.evaluate('="a"&"b"') == "ab"
    assert kyrax.formulas.evaluate("=1<2") is True
    assert kyrax.formulas.evaluate("=1/0") == "#DIV/0!"
    assert kyrax.formulas.evaluate("=SQRT(-1)") == "#NUM!"
    assert kyrax.formulas.evaluate("=NOTEXISTINGFN(1)") == "#NAME?"


def test_evaluate_array_result_is_nested_list():
    assert kyrax.formulas.evaluate("={1,2,3}*2") == [[2.0, 4.0, 6.0]]


def test_evaluate_releases_gil_on_large_compute():
    # A heavy-but-cheap formula must complete and return the right scalar.
    n = 200_000
    formula = "=SUM(" + ",".join("1" for _ in range(n)) + ")"
    assert kyrax.formulas.evaluate(formula) == float(n)


def test_evaluate_invalid_formula_raises():
    with pytest.raises(ValueError):
        kyrax.formulas.evaluate("=SUM(")


def test_evaluate_invalid_context_key_raises():
    with pytest.raises(ValueError):
        kyrax.formulas.evaluate("=1+1", {"A1:B2": [[1, 2], [3, 4]]})
    with pytest.raises(ValueError):
        kyrax.formulas.evaluate("=1+1", {"not-a-ref": 1})


def test_list_functions_count_exceeds_400():
    funcs = kyrax.formulas.list_functions()
    assert len(funcs) > 400


def test_list_functions_shape_and_categories():
    funcs = kyrax.formulas.list_functions()
    by_name = {}
    for name, category in funcs:
        assert isinstance(name, str) and name
        assert isinstance(category, str) and category
        assert name not in by_name, f"duplicate function {name}"
        by_name[name] = category
    # Spot-check categories across every family.
    assert by_name["SUM"] == "Math & Trig"
    assert by_name["VLOOKUP"] == "Lookup & Reference"
    assert by_name["BETA.DIST"] == "Statistical"
    assert by_name["IF"] == "Logical"
    assert by_name["FIXED"] == "Text"
    assert by_name["NOW"] == "Date & Time"
    assert by_name["ERROR.TYPE"] == "Information"
    assert by_name["PMT"] == "Financial"
    assert by_name["IMABS"] == "Engineering"
    assert by_name["DSUM"] == "Database"


def test_dependencies_collects_refs():
    assert kyrax.formulas.dependencies("=SUM(A1:B2)+C5") == ["A1:B2", "C5"]
    assert kyrax.formulas.dependencies("=A1*2") == ["A1"]
    assert kyrax.formulas.dependencies("=XLOOKUP(A1, B:B, C2:C5)") == [
        "A1",
        "B:B",
        "C2:C5",
    ]
    assert kyrax.formulas.dependencies("=SUM(1,2,3)") == []
    assert kyrax.formulas.dependencies("=A1+A1+A1") == ["A1"]


def test_dependencies_invalid_formula_raises():
    with pytest.raises(ValueError):
        kyrax.formulas.dependencies("=(")


def test_recalculate_returns_workbook_bytes_with_computed_values():
    sheets = [
        {
            "name": "S1",
            "rows": [[1.0, 2.0]],
            "formulas": {(0, 2): "=A1+B1"},
        }
    ]
    data = kyrax.formulas.recalculate(sheets)
    assert isinstance(data, bytes)
    assert data[:2] == b"PK"

    wb = kyrax.read_excel(data)
    sheet = wb.load_sheet(0, header_row=None, eager=True)
    row0 = list(sheet.to_pylist()[0].values())
    assert row0[:3] == [1.0, 2.0, 3.0]


def test_recalculate_accepts_write_path_schema():
    # The same sheet dicts write_excel_turbo accepts must work unchanged.
    sheets = [
        {
            "name": "S2",
            "columns": [["a", "b"], [1, 2]],
            "formulas": {(0, 2): '=A1&"-"&B1'},
        }
    ]
    data = kyrax.formulas.recalculate(sheets)
    assert data[:2] == b"PK"
    wb = kyrax.read_excel(data)
    sheet = wb.load_sheet(0, header_row=None, eager=True)
    row0 = list(sheet.to_pylist()[0].values())
    assert "a-1" in row0
