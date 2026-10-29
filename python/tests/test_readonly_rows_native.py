"""Typed worksheet rows checked against an independent openpyxl corpus.

The committed expectations preserve Python types, formula text, sparse rows
and explicit row/column windows. They were generated with openpyxl 3.1.5 via
``bench/generate_readonly_oracle.py``. Scalar reads need no PyArrow import.
"""

from __future__ import annotations

import datetime as dt
import hashlib
import json
import subprocess
import sys
import textwrap
from pathlib import Path

import kyrax
import pytest

HERE = Path(__file__).parent
FIXTURES = HERE.parent.parent / "tests" / "fixtures"
EXPECTED = json.loads((HERE / "data" / "readonly_rows_expected.json").read_text())

# Re-use the generator's window/cell lists and value encoder so the test and the
# recorded expectations can never drift apart.
sys.path.insert(0, str(HERE / "data"))
import _gen_readonly_rows_expected as gen  # noqa: E402

BIG_EXPECTED = EXPECTED.pop("__big__")


def _cases() -> list[tuple[str, str]]:
    return [(fx, sheet) for fx, sheets in EXPECTED.items() for sheet in sheets]


@pytest.mark.parametrize(("fixture", "sheet"), _cases())
def test_iter_rows_windows_match_independent_oracle(fixture: str, sheet: str) -> None:
    exp = EXPECTED[fixture][sheet]["windows"]
    wb = kyrax.load_workbook(str(FIXTURES / fixture), read_only=True)
    try:
        ws = wb[sheet]
        for window, want in zip(gen.WINDOWS, exp, strict=True):
            got = [[gen.enc(c) for c in r] for r in ws.iter_rows(values_only=True, **window)]
            assert got == want, f"{fixture}/{sheet} window={window}"
    finally:
        wb.close()


@pytest.mark.parametrize(("fixture", "sheet"), _cases())
def test_cell_matches_independent_oracle(fixture: str, sheet: str) -> None:
    exp = EXPECTED[fixture][sheet]["cells"]
    wb = kyrax.load_workbook(str(FIXTURES / fixture), read_only=True)
    try:
        ws = wb[sheet]
        for (r, c), want in zip(gen.CELLS, exp, strict=True):
            assert gen.enc(ws.cell(r, c).value) == want, f"{fixture}/{sheet} cell({r},{c})"
    finally:
        wb.close()


def test_multi_batch_matches_independent_oracle() -> None:
    """~140k rows span many 10k-row batches; windows straddle batch edges."""
    wb = kyrax.load_workbook(str(FIXTURES / gen.BIG), read_only=True)
    try:
        ws = wb.active
        for window, (n, sha) in zip(gen.BIG_WINDOWS, BIG_EXPECTED, strict=True):
            rows = [[gen.enc(c) for c in r] for r in ws.iter_rows(values_only=True, **window)]
            blob = json.dumps(rows, separators=(",", ":")).encode()
            assert len(rows) == n, window
            assert hashlib.sha256(blob).hexdigest() == sha, window
    finally:
        wb.close()


def test_values_are_native_python_types() -> None:
    wb = kyrax.load_workbook(str(FIXTURES / "fixture-multi-dtypes-columns.xlsx"), read_only=True)
    try:
        kinds = {type(c) for r in wb.active.iter_rows(values_only=True) for c in r}
    finally:
        wb.close()
    assert kinds <= {int, float, bool, str, dt.datetime, dt.time, dt.timedelta, type(None)}
    assert {int, bool, str, dt.datetime} <= kinds


_NO_PYARROW_SCRIPT = textwrap.dedent(
    """
    import sys
    {block}
    import kyrax
    wb = kyrax.load_workbook({path!r}, read_only=True)
    ws = wb.active
    rows = list(ws.iter_rows(values_only=True))
    assert len(rows) > 1, rows
    ws.cell(2, 1).value
    list(ws.iter_rows(values_only=True, min_row=2, max_row=3, min_col=2))
    wb.close()
    assert "pyarrow" not in sys.modules or sys.modules["pyarrow"] is None, "pyarrow imported"
    print(len(rows))
    """
)


def _run(block: str) -> subprocess.CompletedProcess[str]:
    script = _NO_PYARROW_SCRIPT.format(block=block, path=str(FIXTURES / "sheet-null-strings.xlsx"))
    return subprocess.run(
        [sys.executable, "-c", script], capture_output=True, text=True, check=False
    )


def test_iter_rows_works_with_pyarrow_blocked() -> None:
    res = _run("sys.modules['pyarrow'] = None")
    assert res.returncode == 0, res.stderr
    assert res.stdout.strip() == "11"


def test_pyarrow_never_imported() -> None:
    res = _run("")
    assert res.returncode == 0, res.stderr
