from __future__ import annotations

import kyrax
import pytest

from .utils import path_for_fixture


def test_cell_error_repr() -> None:
    excel_reader = kyrax.read_excel(path_for_fixture("fixture-type-errors.xlsx"))
    _, cell_errors = excel_reader.load_sheet(0, dtypes={"Column": "int"}).to_arrow_with_errors()
    assert cell_errors is not None
    assert (
        repr(cell_errors.errors[0])
        == """CellError(position=(2, 0), offset_position=(1, 0), row_offset=1, detail="Expected int but got 'String(\\"foo\\")'")"""  # noqa: E501
    )


def test_read_excel_bad_type() -> None:
    expected_message = "source must be a string or bytes"
    with pytest.raises(kyrax.InvalidParametersError, match=expected_message):
        kyrax.read_excel(42)  # type: ignore[arg-type] # ty: ignore[invalid-argument-type]


def test_does_not_exist() -> None:
    expected_message = """calamine error: Cannot detect file format
Context:
    0: Could not open workbook at path_does_not_exist.nope
    1: could not load excel file at path_does_not_exist.nope"""

    with pytest.raises(kyrax.CalamineError, match=expected_message) as exc_info:
        kyrax.read_excel("path_does_not_exist.nope")

    assert exc_info.value.__doc__ == "Generic calamine error"

    # Should also work with the base error type
    with pytest.raises(kyrax.KyraxError, match=expected_message):
        kyrax.read_excel("path_does_not_exist.nope")


def test_sheet_idx_not_found_error() -> None:
    excel_reader = kyrax.read_excel(path_for_fixture("fixture-single-sheet.xlsx"))
    expected_message = """sheet at index 42 not found
Context:
    0: Sheet index 42 is out of range. File has 1 sheets."""

    with pytest.raises(kyrax.SheetNotFoundError, match=expected_message) as exc_info:
        excel_reader.load_sheet(42)

    assert exc_info.value.__doc__ == "Sheet was not found"

    # Should also work with the base error type
    with pytest.raises(kyrax.KyraxError, match=expected_message):
        excel_reader.load_sheet(42)


def test_sheet_name_not_found_error() -> None:
    excel_reader = kyrax.read_excel(path_for_fixture("fixture-single-sheet.xlsx"))
    expected_message = """sheet with name "idontexist" not found
Context:
    0: Sheet "idontexist" not found in file. Available sheets: "January"."""

    with pytest.raises(kyrax.SheetNotFoundError, match=expected_message) as exc_info:
        excel_reader.load_sheet("idontexist")

    assert exc_info.value.__doc__ == "Sheet was not found"


@pytest.mark.parametrize(
    "exc_class, expected_docstring",
    [
        (kyrax.KyraxError, "The base class for all kyrax errors"),
        (
            kyrax.UnsupportedColumnTypeCombinationError,
            "Column contains an unsupported type combination",
        ),
        (kyrax.CannotRetrieveCellDataError, "Data for a given cell cannot be retrieved"),
        (
            kyrax.CalamineCellError,
            "calamine returned an error regarding the content of the cell",
        ),
        (kyrax.CalamineError, "Generic calamine error"),
        (kyrax.ColumnNotFoundError, "Column was not found"),
        (kyrax.SheetNotFoundError, "Sheet was not found"),
        (kyrax.ArrowError, "Generic arrow error"),
        (kyrax.InvalidParametersError, "Provided parameters are invalid"),
    ],
)
def test_docstrings(exc_class: type[Exception], expected_docstring: str) -> None:
    assert exc_class.__doc__ == expected_docstring


def test_schema_sample_rows_must_be_nonzero() -> None:
    excel_reader = kyrax.read_excel(path_for_fixture("fixture-single-sheet.xlsx"))

    with pytest.raises(
        kyrax.InvalidParametersError,
        match="schema_sample_rows cannot be 0, as it would prevent dtype inferring",
    ):
        excel_reader.load_sheet(0, schema_sample_rows=0)

    with pytest.raises(
        kyrax.InvalidParametersError,
        match="schema_sample_rows cannot be 0, as it would prevent dtype inferring",
    ):
        excel_reader.load_table("my-table", schema_sample_rows=0)
