"""Standalone formula engine — ``from kyrax import formulas``.

Thin facade over the Rust calc engine (``_kyrax``): there is no formula logic
in Python (CLAUDE.md law). Everything here converts a Python argument into the
Rust binding and converts the result back.

>>> evaluate('=SUM(1,2)')
3.0
>>> evaluate('A1*2', {'A1': 21})
42.0
>>> len(list_functions()) > 400
True
"""

from __future__ import annotations

from typing import Any

from ._kyrax import (
    dependencies as _dependencies,
)
from ._kyrax import (
    evaluate as _evaluate,
)
from ._kyrax import (
    list_functions as _list_functions,
)
from ._kyrax import (
    recalculate as _recalculate,
)


def evaluate(formula: str, context: dict[str, Any] | None = None) -> Any:
    """Evaluate one formula against an optional cell context.

    :param formula: Excel formula text, with or without a leading ``=``.
    :param context: optional map of A1 cell references to values, e.g.
        ``{"A1": 5}``, used as the cell grid the formula reads. A cell that is
        not present reads as blank.
    :return: the computed value — ``float`` for numbers, ``str`` for text and
        error codes (``"#DIV/0!"``, ``"#N/A"``, ...), ``bool``, ``None`` for
        blank, and a nested ``list`` for an array result.

    >>> evaluate('=SUM(1,2)')
    3.0
    >>> evaluate('A1*2', {'A1': 21})
    42.0
    >>> evaluate('=1/0')
    '#DIV/0!'
    """
    return _evaluate(formula, context)


def list_functions() -> list[tuple[str, str]]:
    """Every registered worksheet function as ``(name, category)`` pairs.

    The list is built from the engine's function registry, so it reflects
    exactly the functions the engine can evaluate.
    """
    return _list_functions()


def recalculate(sheets: list[dict[str, Any]]) -> bytes:
    """Recalculate a workbook and return it as XLSX bytes.

    :param sheets: the same sheet-dict schema :func:`kyrax.write_excel_turbo`
        accepts (``rows`` / ``columns`` plus a ``formulas`` map).
    :return: XLSX bytes in which every formula carries its computed value as
        the cached result. Formulas kyrax cannot compute exactly (unsupported
        function, external reference, circular reference) are left uncomputed
        and the workbook keeps ``fullCalcOnLoad="1"`` so Excel fills them on
        open — a wrong number is never written.
    """
    return _recalculate(sheets)


def dependencies(formula: str) -> list[str]:
    """The cell references ``formula`` reads, as sorted, deduplicated A1
    strings.

    >>> dependencies('=SUM(A1:B2)+C5')
    ['A1:B2', 'C5']
    >>> dependencies('=XLOOKUP(A1, B:B, C2:C5)')
    ['A1', 'B:B', 'C2:C5']
    """
    return _dependencies(formula)


__all__ = ("evaluate", "list_functions", "recalculate", "dependencies")
