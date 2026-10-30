import datetime as dt
from decimal import Decimal

import kyrax
import openpyxl
import pytest
from kyrax.chart import BarChart


def write(mode, value, tmp_path):
    path = tmp_path / f"{mode}.xlsx"
    if mode == "editable":
        wb = kyrax.Workbook()
        wb.active["A1"] = value
        wb.save(path)
    elif mode == "turbo":
        kyrax.write_excel_turbo(path, [{"name": "S", "rows": [[value]]}])
    else:
        kyrax.write_excel_turbo_stream(path, [{"name": "S", "rows": [[value]]}])
    return path


@pytest.mark.parametrize("mode", ["editable", "turbo", "stream"])
@pytest.mark.parametrize(
    "value",
    [
        float("nan"),
        float("inf"),
        -float("inf"),
        2**53 + 1,
        Decimal("9007199254740993"),
        dt.datetime(2024, 1, 1, tzinfo=dt.timezone.utc),
        dt.time(12, tzinfo=dt.timezone.utc),
    ],
)
def test_reject_values_that_cannot_be_safely_written(mode, value, tmp_path):
    with pytest.raises(ValueError):
        write(mode, value, tmp_path)


@pytest.mark.parametrize("mode", ["editable", "turbo", "stream"])
def test_valid_scalars_round_trip(mode, tmp_path):
    assert openpyxl.load_workbook(write(mode, 42, tmp_path)).active["A1"].value == 42


@pytest.mark.parametrize("writer", [kyrax.write_excel_turbo, kyrax.write_excel_turbo_stream])
@pytest.mark.parametrize("names", [["Data", "data"], [""], ["bad/name"], ["bad:name"], ["x" * 32]])
def test_reject_invalid_names(writer, names, tmp_path):
    with pytest.raises(Exception, match="sheet name"):
        writer(tmp_path / "invalid.xlsx", [{"name": n, "rows": [[1]]} for n in names])


def test_axis_title_warns():
    with pytest.warns(UserWarning, match="axis titles"):
        BarChart().x_axis.title = "Month"
