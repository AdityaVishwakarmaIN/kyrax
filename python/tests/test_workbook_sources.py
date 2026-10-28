"""Source normalization and lifecycle for the familiar workbook API."""

from __future__ import annotations

import datetime as dt
import io
from pathlib import Path

import kyrax
import pytest


@pytest.fixture
def workbook_bytes():
    wb = kyrax.Workbook()
    wb.active.append(["name", "count"])
    wb.active.append(["item", 7])
    out = io.BytesIO()
    wb.save(out)
    return out.getvalue()


@pytest.mark.parametrize("read_only", [False, True])
@pytest.mark.parametrize("kind", ["bytes", "bytearray", "memoryview", "stream"])
def test_binary_sources(workbook_bytes, read_only, kind):
    factories = {
        "bytes": bytes, "bytearray": bytearray, "memoryview": memoryview, "stream": io.BytesIO
    }
    source = factories[kind](workbook_bytes)
    wb = kyrax.load_workbook(source, read_only=read_only)
    assert list(wb.active.iter_rows(values_only=True)) == [("name", "count"), ("item", 7)]
    staged = Path(wb._path) if read_only else None
    if staged is not None:
        assert staged.exists()
    wb.close()
    wb.close()
    if staged is not None:
        assert not staged.exists()
    if kind == "stream":
        assert not source.closed


def test_stream_staging_uses_bounded_reads_and_current_position(workbook_bytes):
    class BoundedStream(io.BytesIO):
        def read(self, size=-1):
            assert 0 < size <= 1024 * 1024
            # Exercise legal short reads, not just one large buffer.
            return super().read(min(size, 137))

    source = BoundedStream(b"prefix" + workbook_bytes)
    source.seek(6)
    with kyrax.load_workbook(source, read_only=True) as wb:
        assert wb.active.cell(2, 1).value == "item"
    assert not source.closed


def test_failed_staging_and_open_remove_temporary_files(workbook_bytes, monkeypatch):
    import tempfile

    original = tempfile.NamedTemporaryFile
    staged_paths = []

    def track(*args, **kwargs):
        handle = original(*args, **kwargs)
        staged_paths.append(Path(handle.name))
        return handle

    monkeypatch.setattr(tempfile, "NamedTemporaryFile", track)
    with pytest.raises(TypeError, match="binary file-like"):
        kyrax.load_workbook(io.StringIO("not binary"), read_only=True)
    with pytest.raises(Exception):
        kyrax.load_workbook(b"not a workbook", read_only=True)
    assert len(staged_paths) == 2
    assert all(not path.exists() for path in staged_paths)


@pytest.mark.parametrize("read_only", [False, True])
def test_unsupported_arguments_fail_before_consuming_source(workbook_bytes, read_only):
    source = io.BytesIO(workbook_bytes)
    with pytest.raises(TypeError, match="unsupported arguments.*unknown"):
        kyrax.load_workbook(source, read_only=read_only, unknown=True)
    assert source.tell() == 0


@pytest.mark.parametrize("source", [123, object(), io.StringIO("text")])
@pytest.mark.parametrize("read_only", [False, True])
def test_invalid_sources_raise_type_error(source, read_only):
    with pytest.raises(TypeError, match="source|binary"):
        kyrax.load_workbook(source, read_only=read_only)


@pytest.mark.parametrize("data_only", [False, True])
def test_read_only_formula_selection_for_rows_and_cells(tmp_path, data_only):
    path = str(tmp_path / "formula.xlsx")
    kyrax.write_excel_turbo(path, [{
        "name": "S", "rows": [[1, 2, None], [3, 4, None]],
        "formulas": {
            (0, 2): {"text": "SUM(A1:B1)", "cached": 3},
            (1, 2): {"text": "SUM(A2:B2)"},
        },
    }])
    expected = [(1, 2, 3 if data_only else "=SUM(A1:B1)"),
                (3, 4, None if data_only else "=SUM(A2:B2)")]
    with kyrax.load_workbook(path, read_only=True, data_only=data_only) as wb:
        assert list(wb.active.values) == expected
        assert wb.active.cell(1, 3).value == expected[0][2]
        assert wb.active.cell(2, 3).value == expected[1][2]


def test_read_only_values_preserve_per_cell_types(tmp_path):
    path = str(tmp_path / "typed.xlsx")
    wb = kyrax.Workbook()
    values = [42, True, "007", dt.date(2024, 1, 2), dt.time(3, 4, 5),
              dt.timedelta(days=2, seconds=3), "#DIV/0!"]
    for value in values:
        wb.active.append([value])
    wb.save(path)
    expected = [42, True, "007", dt.datetime(2024, 1, 2), dt.time(3, 4, 5),
                dt.timedelta(days=2, seconds=3), "#DIV/0!"]
    with kyrax.load_workbook(path, read_only=True) as reader:
        actual = [row[0] for row in reader.active.values]
        assert actual == expected
        assert isinstance(actual[0], (int, float)) and not isinstance(actual[0], bool)
        assert [type(value) for value in actual[1:]] == [type(value) for value in expected[1:]]
