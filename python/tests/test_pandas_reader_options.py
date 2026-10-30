import pandas as pd
import pytest

import kyrax
import kyrax.pandas  # noqa: F401 - register the engine


@pytest.fixture
def workbook(tmp_path):
    path = tmp_path / "input.xlsx"
    kyrax.write_excel_turbo(path, [{"name": "S", "rows": [
        ["preamble", "ignore"], ["id", "value"], ["001", "NA"], ["002", "hello"]
    ]}])
    return path


def test_pandas_reader_options(workbook):
    df = pd.read_excel(workbook, engine="kyrax", header=1, dtype={"id": str},
                       converters={"value": lambda value: "X" + value})
    assert df["id"].tolist() == ["001", "002"]
    assert df["value"].tolist() == ["XNA", "Xhello"]
    df = pd.read_excel(workbook, engine="kyrax", header=1, na_values=["hello"])
    assert df["value"].isna().all()
    df = pd.read_excel(workbook, engine="kyrax", header=1, na_filter=False)
    assert df["value"].tolist() == ["NA", "hello"]


@pytest.mark.parametrize("header, columns, first", [
    (0, ["id", "value"], 1),
    (1, ["001", "NA"], 2),
    (None, [0, 1], "id"),
])
def test_pandas_skiprows_and_header(workbook, header, columns, first):
    df = pd.read_excel(workbook, engine="kyrax", skiprows=1, header=header)
    assert df.columns.tolist() == columns
    assert df.iloc[0, 0] == first


def test_pandas_usecols_rejects_missing_columns(workbook):
    with pytest.raises(ValueError, match="missing"):
        pd.read_excel(workbook, engine="kyrax", header=1, usecols=["id", "missing"])
    with pytest.raises(pd.errors.ParserError, match="out-of-bounds"):
        pd.read_excel(workbook, engine="kyrax", header=1, usecols="A:D")


def test_pandas_reader_preserves_cell_types(tmp_path):
    path = tmp_path / "types.xlsx"
    kyrax.write_excel_turbo(path, [{"name": "S", "rows": [
        ["number", "flag"], [12, True], [34, False]
    ]}])
    df = pd.read_excel(path, engine="kyrax")
    assert df["number"].tolist() == [12, 34]
    assert df["flag"].tolist() == [True, False]


def test_pandas_ignores_formatted_empty_cells(tmp_path):
    openpyxl = pytest.importorskip("openpyxl")
    path = tmp_path / "formatted_empty.xlsx"
    workbook = openpyxl.Workbook()
    sheet = workbook.active
    sheet.append(["first", "second"])
    sheet.append([1, 2])
    sheet["E5"].number_format = "0.00"
    workbook.save(path)

    expected = pd.read_excel(path, engine="openpyxl")
    actual = pd.read_excel(path, engine="kyrax")
    pd.testing.assert_frame_equal(actual, expected)
