"""Lane J — determinism gate: identical inputs must produce identical bytes.

The engine's writer is deterministic for non-volatile workbooks: saving the
same sheets twice yields byte-identical output. Volatile functions (``NOW``,
``TODAY``, ``RAND``, ...) are EXCLUDED from the determinism set by design —
their cached values change between runs, so byte identity cannot be required
(probe_j21 logic).
"""

from __future__ import annotations

from kyrax import write_excel_turbo_bytes

_VOLATILE = ("NOW", "TODAY", "RAND", "RANDBETWEEN", "RANDARRAY")


def _has_volatile(sheets) -> bool:
    text = str(sheets)
    return any(fn in text for fn in _VOLATILE)


def _det_workbook():
    """Data + non-volatile formulas: the determinism set."""
    return [
        {
            "name": "S",
            "rows": [[1, 2], [3, 4]],
            "formulas": {
                (0, 2): "=A1+B1",
                (1, 2): "=SUM(A1:B2)",
                (0, 3): '="a"&"b"',
            },
        }
    ]


def _volatile_workbook():
    return [
        {
            "name": "S",
            "rows": [[1], [2]],
            "formulas": {(0, 1): "=RAND()", (1, 1): "=NOW()", (0, 2): "=A1+A2"},
        }
    ]


def test_deterministic_workbook_is_byte_identical():
    sheets = _det_workbook()
    assert not _has_volatile(sheets), "determinism set must exclude volatile cells"
    b1 = write_excel_turbo_bytes(sheets, recalculate=True)
    b2 = write_excel_turbo_bytes(sheets, recalculate=True)
    assert b1 == b2, "same non-volatile workbook saved twice must be byte-identical"


def test_volatile_workbook_excluded_from_determinism_set():
    sheets = _volatile_workbook()
    assert _has_volatile(sheets), "volatile workbook must be flagged as excluded"
    v1 = write_excel_turbo_bytes(sheets, recalculate=True)
    v2 = write_excel_turbo_bytes(sheets, recalculate=True)
    # RAND/NOW cached values legitimately differ between runs: byte identity
    # is NOT required for the volatile set (the gate must not assert it).
    assert isinstance(v1, bytes) and isinstance(v2, bytes)
