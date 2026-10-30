import datetime as dt
from decimal import Decimal
import sys

import pytest
import kyrax

np = pytest.importorskip("numpy")
pa = pytest.importorskip("pyarrow")
openpyxl = pytest.importorskip("openpyxl")


def test_grid_byte_order_and_infinity(tmp_path):
    native = np.array([[1.5, np.nan]], dtype="f8")
    path = tmp_path / "native.xlsx"
    kyrax.write_excel_turbo(path, [{"name": "S", "grid": native}])
    ws = openpyxl.load_workbook(path).active
    assert ws["A1"].value == 1.5
    assert ws["B1"].value is None
    non_native = ">" if sys.byteorder == "little" else "<"
    for dtype in ("f4", "f8"):
        with pytest.raises(ValueError, match="byte order"):
            kyrax.write_excel_turbo(tmp_path / "endian.xlsx", [{"name": "S", "grid": np.array([[1.5]], dtype=non_native + dtype)}])
        for value in (float("inf"), float("-inf")):
            with pytest.raises(ValueError, match="infinity"):
                kyrax.write_excel_turbo(tmp_path / "inf.xlsx", [{"name": "S", "grid": np.array([[value]], dtype=dtype)}])


def test_arrow_recordbatch_conversion_and_validation(tmp_path):
    def write(array):
        batch = pa.RecordBatch.from_arrays([array], names=["value"])
        path = tmp_path / "arrow.xlsx"
        kyrax.write_excel_turbo(path, [{"name": "S", "columns": batch}])
        return openpyxl.load_workbook(path).active["A1"].value

    assert write(pa.array([42], type=pa.int16())) == 42
    assert write(pa.array([dt.date(2024, 1, 1)], type=pa.date32())).date() == dt.date(2024, 1, 1)
    assert write(pa.array([None, 1.5], type=pa.float64())) is None
    for array in (pa.array([2**53 + 1], type=pa.int64()),
                  pa.array([2**53 + 1], type=pa.uint64()),
                  pa.array([Decimal("1.23")], type=pa.decimal128(10, 2)),
                  pa.array([float("inf")]), pa.array([float("nan")]),
                  pa.array([dt.datetime(2024, 1, 1, tzinfo=dt.timezone.utc)], type=pa.timestamp("us", tz="UTC"))):
        with pytest.raises(ValueError):
            write(array)

@pytest.mark.parametrize("mode", ["editable", "turbo", "stream"])
@pytest.mark.parametrize("dtype", [np.int64, np.uint64])
def test_numpy_integer_exactness(tmp_path, mode, dtype):
    def write(value):
        path = tmp_path / "scalar.xlsx"
        if mode == "editable":
            wb = kyrax.Workbook()
            wb.active["A1"] = value
            wb.save(path)
        else:
            writer = kyrax.write_excel_turbo_stream if mode == "stream" else kyrax.write_excel_turbo
            writer(path, [{"name": "S", "rows": [[value]]}])
        return openpyxl.load_workbook(path).active["A1"].value

    assert write(dtype(42)) == 42
    assert write(dtype(2**53)) == 2**53
    with pytest.raises(ValueError, match="represented exactly"):
        write(dtype(2**53 + 1))
