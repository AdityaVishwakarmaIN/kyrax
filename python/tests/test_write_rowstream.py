"""Direct row streaming (rowstream) must be byte-identical to the model path.

A sheet whose ``rows`` is a plain list-of-lists takes the fast path. Passing the
same rows through the non-streaming bytes entry point with an iterator forces
the model reference; every zip part must decompress to identical bytes. Lazy
streaming itself may omit the optional dimension element.
"""

from __future__ import annotations

import datetime as dt
import decimal
import io
import os
import warnings
import zipfile
from pathlib import Path

import kyrax
import pytest

BIG = 2**70
LONG = "x" * 40000

ROWS_ALL = [
    [1, -2, 0, 2**53, 2**62, -(2**62)],
    [1.5, -0.0, 1e300, 1e-300, -1.25, 3.141592653589793],
    [True, False, None, "", " lead", "trail "],
    ["#N/A", "#DIV/0!", "=SUM(A1:A2)", "+1", "-1", "@x"],
    ["a<b>&\"c'", "tab\there", "new\nline", "unié中\U0001f600", "ctl", "dup"],
    [
        dt.datetime(2024, 2, 29, 13, 45, 59),
        dt.datetime(2024, 1, 1),
        dt.date(2024, 3, 1),
        dt.datetime(1899, 12, 30, 1),
    ],
    [
        dt.datetime(2024, 1, 1, 0, 0, 0, 500000),
        dt.date(1900, 1, 1),
        dt.date(1900, 3, 1),
        dt.date(2999, 12, 31),
    ],
    [],
    [None, None],
    [None, None, "lead-empty-cols", None],
    ["ragged"],
    (1, "tuple", 2.5, None, dt.date(2020, 2, 2)),
    ["dup", "dup", "dup", "unié中\U0001f600"],
    [None] * 5 + [7],
    [],
]


def _model(sheets):
    """Same sheets, rows swapped for iterators -> forces the model path."""
    return [{**s, "rows": iter(s["rows"])} if "rows" in s else dict(s) for s in sheets]


def _parts(data: bytes) -> dict[str, bytes]:
    with zipfile.ZipFile(io.BytesIO(data)) as zf:
        return {n: zf.read(n) for n in zf.namelist()}


def _write(entry: str, tmp_path: Path, sheets, **kw) -> bytes:
    if entry == "bytes":
        return kyrax.write_excel_turbo_bytes(sheets, **kw)
    p = tmp_path / f"{entry}.xlsx"
    getattr(kyrax, f"write_excel_turbo{'_stream' if entry == 'stream' else ''}")(
        str(p), sheets, **kw
    )
    return p.read_bytes()


def _assert_identical(entry, tmp_path, sheets, **kw):
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        fast = _parts(_write(entry, tmp_path, sheets, **kw))
        slow = _parts(_write("bytes", tmp_path, _model(sheets), **kw))
    assert set(fast) == set(slow)
    for name in slow:
        assert fast[name] == slow[name], name
    return fast


ENTRIES = ["path", "stream", "bytes"]


@pytest.mark.parametrize("entry", ENTRIES)
@pytest.mark.parametrize("string_mode", ["inline", "sst"])
@pytest.mark.parametrize(
    "opts",
    [
        {},
        {"date_iso": True},
        {"date1904": True},
        {"emit_cached_values": False},
        {"active_tab": 1, "creator": "me"},
    ],
    ids=["default", "date_iso", "date1904", "nocache", "tab_creator"],
)
def test_identical_parts(entry, string_mode, opts, tmp_path):
    sheets = [
        {"name": "Data", "rows": ROWS_ALL},
        {
            "name": "Hidden & <x>",
            "rows": [["h", 1], [None, dt.date(2021, 5, 5)]],
            "visibility": "hidden",
        },
        {"name": "Empty", "rows": []},
        {"name": "Very", "rows": [[1]], "state": "veryHidden"},
        {"rows": [[None]]},
    ]
    _assert_identical(entry, tmp_path, sheets, string_mode=string_mode, **opts)


@pytest.mark.parametrize("entry", ENTRIES)
def test_single_sheet_variants(entry, tmp_path):
    for rows in (
        [[1, 2], [3, 4]],
        [],
        [[]],
        [[None, None], [None]],
        [[], [], [None, 5]],
        [[None] * 3 + ["x"], ["y"] + [None] * 4],
        [[dt.date(2020, 1, 1)]],
    ):
        _assert_identical(entry, tmp_path, [{"name": "S", "rows": rows}])
    _assert_identical(entry, tmp_path, [{"rows": [[1]]}])  # default name


@pytest.mark.parametrize("entry", ENTRIES)
def test_long_string_warns_and_truncates(entry, tmp_path):
    sheets = [{"name": "S", "rows": [[LONG, "ok"]]}]
    with pytest.warns(UserWarning, match="32767"):
        fast = _parts(_write(entry, tmp_path, sheets))
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        slow = _parts(_write("bytes", tmp_path, _model(sheets)))
    assert fast == slow


@pytest.mark.parametrize("entry", ENTRIES)
def test_many_rows_cross_flush_boundary(entry, tmp_path):
    rows = [[i, f"s{i % 50}", i * 0.5, dt.date(2020, 1, 1 + i % 28)] for i in range(5000)]
    _assert_identical(entry, tmp_path, [{"name": "Big", "rows": rows}], string_mode="sst")
    _assert_identical(entry, tmp_path, [{"name": "Big", "rows": rows}])


def test_values_reopen_equal(tmp_path):
    rows = [
        ["id", "name", "when", "ok", "x"],
        [1, "alpha", dt.datetime(2024, 5, 6, 7, 8, 9), True, 1.25],
        [2, None, dt.date(2024, 5, 7), False, None],
        [3, "gamma", None, True, 3.5],
    ]
    fast = tmp_path / "fast.xlsx"
    slow = tmp_path / "slow.xlsx"
    kyrax.write_excel_turbo(str(fast), [{"name": "S", "rows": rows}], string_mode="sst")
    kyrax.write_excel_turbo(str(slow), _model([{"name": "S", "rows": rows}]), string_mode="sst")
    for kind in ("fast", "slow"):
        wb = kyrax.load_workbook(str(tmp_path / f"{kind}.xlsx"))
        got = [list(r) for r in wb["S"].iter_rows(values_only=True)]
        assert got[1] == [1, "alpha", dt.datetime(2024, 5, 6, 7, 8, 9), True, 1.25]
        assert got[2][2] == dt.datetime(2024, 5, 7)
        assert got[2][1] is None and got[3][2] is None
    rd_fast = kyrax.read_excel_turbo(str(fast)).load_sheet(0).to_arrow().to_pylist()
    rd_slow = kyrax.read_excel_turbo(str(slow)).load_sheet(0).to_arrow().to_pylist()
    assert rd_fast == rd_slow


# ---------------------------------------------------------------- ineligible


class _Sub(int):
    pass


INELIGIBLE = {
    "dict_cell": [[{"value": 1, "style": {"bold": True}}]],
    "rich": [[{"rich": True, "runs": [{"text": "a"}]}]],
    "time": [[dt.time(1, 2, 3)]],
    "timedelta": [[dt.timedelta(days=1, seconds=5)]],
    "decimal": [[decimal.Decimal("1.5")]],
    "int_subclass": [[_Sub(3)]],
    "str_in_row": ["ab", "cd"],
    "generator": None,
}


@pytest.mark.parametrize("name", INELIGIBLE)
@pytest.mark.parametrize("entry", ENTRIES)
def test_ineligible_inputs_unchanged(name, entry, tmp_path):
    def build():
        if name == "generator":
            return [{"name": "S", "rows": (r for r in [[1, 2], [3]])}]
        return [{"name": "S", "rows": list(INELIGIBLE[name])}]

    try:
        got = _parts(_write(entry, tmp_path, build()))
    except Exception as e:  # same error as the iterator spelling
        with pytest.raises(type(e)):
            _write("bytes", tmp_path, _model(build()))
        return
    ref = _parts(_write("bytes", tmp_path, _model(build())))
    if entry == "stream" and name == "generator":
        # A one-shot source now takes the streaming path. Only the optional
        # worksheet bounds differ; all cell, type, style and metadata XML must
        # remain identical to the independent model route.
        for part in ref:
            if part.startswith("xl/worksheets/"):
                assert b"<dimension " not in got[part]
                xml = ref[part]
                start = xml.index(b"<dimension ")
                end = xml.index(b"/>", start) + 2
                ref[part] = xml[:start] + xml[end:]
    assert got == ref


@pytest.mark.parametrize("entry", ENTRIES)
def test_ineligible_kwargs_unchanged(entry, tmp_path):
    rows = [[1, "a", "a", dt.date(2020, 1, 1)]]
    base = [{"name": "S", "rows": rows}]
    for kw in (
        {"features": "all"},
        {"string_mode": "auto"},
        {"props": {"title": "t"}},
        {"defined_names": [{"name": "N", "value": "S!$A$1"}]},
        {"lock_structure": True},
        {"external_links": ["file.xlsx"]},
    ):
        got = _parts(_write(entry, tmp_path, base, **kw))
        ref = _parts(_write("bytes", tmp_path, _model(base), **kw))
        assert got == ref, kw
    # extra sheet keys also fall through to the model path
    extra = [{"name": "S", "rows": rows, "freeze_panes": "A2", "merges": ["A1:B1"]}]
    assert _parts(_write(entry, tmp_path, extra)) == _parts(
        _write("bytes", tmp_path, _model(extra))
    )


@pytest.mark.parametrize("entry", ENTRIES)
def test_illegal_control_character_rejected(entry, tmp_path):
    with pytest.raises(ValueError, match="control character"):
        _write(entry, tmp_path, [{"name": "S", "rows": [["a\x01b"]]}])


@pytest.mark.parametrize("entry", ENTRIES)
def test_errors_still_raised(entry, tmp_path):
    with pytest.raises(Exception):
        _write(entry, tmp_path, [{"name": "S", "rows": [[1]], "visibility": "bogus"}])
    with pytest.raises(Exception):
        _write(entry, tmp_path, [])
    with pytest.raises(Exception):
        _write(entry, tmp_path, [{"name": "S", "rows": [[1] * 16385]}])
    with pytest.raises(Exception):
        _write(
            entry, tmp_path, [{"name": "S", "rows": [[1]], "state": "visible"}], string_mode="nope"
        )


# --------------------------------------------------------------- error path


@pytest.mark.parametrize("entry", ["path", "stream"])
def test_midwrite_error_removes_new_file(entry, tmp_path):
    p = tmp_path / "new.xlsx"
    rows = [[1, 2]] * 10 + [
        [BIG * BIG * BIG * 10**400]
    ]  # exact int too large for f64 -> mid-write error
    fn = kyrax.write_excel_turbo_stream if entry == "stream" else kyrax.write_excel_turbo
    with pytest.raises(Exception):
        fn(str(p), [{"name": "S", "rows": rows}])
    assert not p.exists()


@pytest.mark.parametrize("entry", ["path", "stream"])
def test_midwrite_error_keeps_preexisting_file(entry, tmp_path):
    p = tmp_path / "old.xlsx"
    p.write_bytes(b"user data")
    rows = [[1, 2]] * 10 + [[BIG * BIG * BIG * 10**400]]
    fn = kyrax.write_excel_turbo_stream if entry == "stream" else kyrax.write_excel_turbo
    with pytest.raises(Exception):
        fn(str(p), [{"name": "S", "rows": rows}])
    assert p.exists()  # never deleted


def test_warning_as_error_removes_new_file(tmp_path):
    p = tmp_path / "w.xlsx"
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        with pytest.raises(UserWarning):
            kyrax.write_excel_turbo_stream(str(p), [{"name": "S", "rows": [[1], [LONG]]}])
    assert not os.path.exists(p)


def test_overwrites_existing_file(tmp_path):
    p = tmp_path / "o.xlsx"
    p.write_bytes(b"x" * 100000)
    kyrax.write_excel_turbo(str(p), [{"name": "S", "rows": [[1, 2]]}])
    assert zipfile.ZipFile(p).namelist()
    assert kyrax.load_workbook(str(p))["S"]["B1"].value == 2


@pytest.mark.parametrize("entry", ENTRIES)
def test_fast_path_is_taken(entry, tmp_path):
    """styles.xml is emitted after the worksheets only by the row-streaming path."""
    sheets = [{"name": "S", "rows": [[1, dt.date(2024, 1, 1)]]}]

    def order(data):
        return zipfile.ZipFile(io.BytesIO(data)).namelist()

    fast = order(_write(entry, tmp_path, sheets))
    slow = order(_write("bytes", tmp_path, _model(sheets)))
    assert fast.index("xl/styles.xml") > fast.index("xl/worksheets/sheet1.xml")
    assert slow.index("xl/styles.xml") < slow.index("xl/worksheets/sheet1.xml")


def _took_fast_path(data: bytes) -> bool:
    """The fast path writes styles.xml after the worksheets; the model path before."""
    with zipfile.ZipFile(io.BytesIO(data)) as zf:
        names = zf.namelist()
    return names.index("xl/styles.xml") > names.index("xl/worksheets/sheet1.xml")


@pytest.mark.parametrize("entry", ENTRIES)
def test_fast_path_taken_and_identical(entry, tmp_path):
    sheets = [{"name": "S", "rows": ROWS_ALL}]
    assert _took_fast_path(_write(entry, tmp_path, sheets))
    _assert_identical(entry, tmp_path, sheets)


@pytest.mark.parametrize("entry", ENTRIES)
@pytest.mark.parametrize("cell", [2**63, BIG, LONG], ids=["above-i64", "big", "long"])
def test_ineligible_large_values_use_model_path(entry, cell, tmp_path):
    """Master condition 3: ints outside i64 and > 32767-char strings are
    ineligible, so fast-path mid-write errors can only be I/O errors.
    Exactly representable big ints still write via the model path; values
    that cannot be represented exactly are rejected (see below)."""
    sheets = [{"name": "S", "rows": [[1, "a"], [cell]]}]
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        data = _write(entry, tmp_path, sheets)
    assert not _took_fast_path(data)
    ok = [{"name": "S", "rows": [[1, "a"], [2**62, "x" * 32767]]}]
    assert _took_fast_path(_write(entry, tmp_path, ok))


UNSAFE_NUMBERS = [
    pytest.param(2**63 - 1, id="i64-max-not-exact"),
    pytest.param(-(2**63) - 1, id="below-i64-not-exact"),
    pytest.param(2**53 + 1, id="above-f64-integer-range"),
    pytest.param(float("nan"), id="nan"),
    pytest.param(float("inf"), id="inf"),
    pytest.param(float("-inf"), id="-inf"),
    pytest.param(decimal.Decimal("9007199254740993"), id="decimal-not-exact"),
]


@pytest.mark.parametrize("entry", ENTRIES)
@pytest.mark.parametrize("cell", UNSAFE_NUMBERS)
def test_unsafe_numbers_rejected(entry, cell, tmp_path):
    """Non-finite and non-exactly-representable numbers must fail on every
    entry point instead of silently rounding or writing invalid XML."""
    with pytest.raises(ValueError, match="NaN or infinity|represented exactly"):
        _write(entry, tmp_path, [{"name": "S", "rows": [[1, "a"], [cell]]}])
