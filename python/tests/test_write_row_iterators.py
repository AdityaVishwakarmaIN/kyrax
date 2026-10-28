"""One-shot row sources stream without retaining an entire workbook model."""

from __future__ import annotations

import datetime as dt
import itertools
import warnings
import zipfile
from pathlib import Path

import kyrax
import pytest


def _parts(path: Path) -> dict[str, bytes]:
    with zipfile.ZipFile(path) as archive:
        return {name: archive.read(name) for name in archive.namelist()}


def _without_dimension(xml: bytes) -> bytes:
    start = xml.find(b"<dimension ")
    if start < 0:
        return xml
    end = xml.index(b"/>", start) + 2
    return xml[:start] + xml[end:]


@pytest.mark.parametrize("row_key", ["rows", "rows_iter"])
@pytest.mark.parametrize("string_mode", ["inline", "sst"])
@pytest.mark.parametrize("date_options", [{}, {"date1904": True}, {"date_iso": True}])
def test_generator_matches_model_parts(tmp_path, row_key, string_mode, date_options):
    rows = [
        ["id", "kind", "date", "number", "empty", "flag"],
        [1, "text <&>\t\n中", dt.date(2024, 2, 29), 2.5, None, True],
        [2, "", dt.datetime(2020, 1, 1, 12, 30), -(2**63), None, False],
        [],
        [None, {"value": 3, "style": {"bold": True}}, dt.time(1, 2, 3)],
        [dt.timedelta(days=2, seconds=5), {"rich": True, "runs": [{"text": "a"}]}],
        ["#N/A", "=literal", float("inf"), float("nan")],
    ]
    expected = tmp_path / "model.xlsx"
    actual = tmp_path / "generator.xlsx"
    # A per-sheet key outside the streaming subset forces the established model
    # route for an independent conversion/emission comparison.
    kyrax.write_excel_turbo_stream(
        expected,
        [{"name": "Data", "rows": rows, "freeze_panes": "A1"}],
        string_mode=string_mode,
        **date_options,
    )
    consumed = []

    def source():
        for index, row in enumerate(rows):
            consumed.append(index)
            yield (cell for cell in row)

    kyrax.write_excel_turbo_stream(
        actual,
        [{"name": "Data", row_key: source()}],
        string_mode=string_mode,
        **date_options,
    )
    assert consumed == list(range(len(rows)))
    got, want = _parts(actual), _parts(expected)
    assert set(got) == set(want)
    for name in want:
        if name.startswith("xl/worksheets/"):
            assert b"<dimension " not in got[name]
            assert got[name] == _without_dimension(want[name]), name
        else:
            assert got[name] == want[name], name


@pytest.mark.parametrize("existing", [False, True])
@pytest.mark.parametrize("failure", ["generator", "value", "columns", "warning"])
def test_late_failure_preserves_destination_and_cleans_temp(tmp_path, existing, failure):
    target = tmp_path / "out.xlsx"
    original = b"existing workbook bytes"
    if existing:
        target.write_bytes(original)

    def rows():
        # Force multiple flushes before failing; failure cleanup must work after
        # archive output has already reached the filesystem.
        for index in range(5000):
            yield [index, "some data " * 20]
        if failure == "generator":
            raise RuntimeError("source failed")
        if failure == "value":
            yield [object()]
        elif failure == "columns":
            yield [None] * 16385
        elif failure == "warning":
            yield ["x" * 32768]

    with warnings.catch_warnings():
        warnings.simplefilter("error", UserWarning)
        with pytest.raises((RuntimeError, TypeError, ValueError, UserWarning)):
            kyrax.write_excel_turbo_stream(target, [{"name": "S", "rows": rows()}])
    if existing:
        assert target.read_bytes() == original
    else:
        assert not target.exists()
    assert list(tmp_path.glob("*.tmp")) == []
    assert list(tmp_path.glob(".*.tmp")) == []


def test_generator_replaces_existing_workbook(tmp_path):
    target = tmp_path / "out.xlsx"
    target.write_bytes(b"old")
    kyrax.write_excel_turbo_stream(target, [{"name": "S", "rows": iter([[7], [8]])}])
    workbook = kyrax.load_workbook(target)
    assert list(workbook.active.values) == [(7,), (8,)]
    workbook.close()
    assert list(tmp_path.glob(".*.tmp")) == []


def test_unsupported_sheet_options_fall_back_without_consuming_twice(tmp_path):
    target = tmp_path / "features.xlsx"
    rows = iter([[1, 2], [3, 4]])
    kyrax.write_excel_turbo_stream(target, [{"name": "S", "rows": rows, "freeze_panes": "B2"}])
    workbook = kyrax.load_workbook(target)
    assert list(workbook.active.values) == [(1, 2), (3, 4)]
    assert workbook.active.freeze_panes == "B2"
    workbook.close()


def test_unknown_dimensions_empty_and_multiple_sheets(tmp_path):
    target = tmp_path / "many.xlsx"
    kyrax.write_excel_turbo_stream(
        target,
        [
            {"name": "Empty", "rows": iter(())},
            {"name": "Sparse", "rows": iter([[], [None, 4], [None, None, 5]])},
            {"name": "List", "rows": [[True, "x"]]},
        ],
        active_tab=1,
    )
    with zipfile.ZipFile(target) as archive:
        assert all(
            b"<dimension " not in archive.read(f"xl/worksheets/sheet{index}.xml")
            for index in (1, 2, 3)
        )
    workbook = kyrax.load_workbook(target)
    assert workbook.sheetnames == ["Empty", "Sparse", "List"]
    assert workbook.active.title == "Sparse"
    assert workbook["Sparse"].cell(2, 2).value == 4
    assert workbook["List"].cell(1, 1).value is True
    workbook.close()


def test_generator_row_limit_is_enforced_without_retaining_rows(tmp_path):
    target = tmp_path / "too_many.xlsx"
    with pytest.raises(ValueError, match="1048576"):
        kyrax.write_excel_turbo_stream(
            target, [{"name": "S", "rows": itertools.repeat((), 1_048_577)}]
        )
    assert not target.exists()
    assert list(tmp_path.glob(".*.tmp")) == []
