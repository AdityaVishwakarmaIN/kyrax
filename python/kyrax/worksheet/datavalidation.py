"""openpyxl-compatible ``DataValidation`` holder.

Converts to the data-validation dict the Rust engine parses. Use with
``ws.add_data_validation(dv)``; ``dv.add("E1:E5")`` may be called before or
after that (later calls update the sheet's stored copy).
"""

from __future__ import annotations

from typing import Any


class DataValidation:
    """openpyxl ``DataValidation``."""

    _SYNC_FIELDS = frozenset(
        {
            "type",
            "formula1",
            "formula2",
            "showErrorMessage",
            "showInputMessage",
            "showDropDown",
            "allowBlank",
            "promptTitle",
            "errorStyle",
            "error",
            "prompt",
            "errorTitle",
            "imeMode",
            "operator",
        }
    )

    def __setattr__(self, name: str, value: Any) -> None:
        object.__setattr__(self, name, value)
        if name in self._SYNC_FIELDS and getattr(self, "_kx_bound", None):
            self._kx_sync()

    def __init__(
        self,
        type: str | None = None,
        formula1: str | None = None,
        formula2: str | None = None,
        showErrorMessage: bool = False,
        showInputMessage: bool = False,
        showDropDown: bool = False,
        allowBlank: bool = False,
        sqref: Any = (),
        promptTitle: str | None = None,
        errorStyle: str | None = None,
        error: str | None = None,
        prompt: str | None = None,
        errorTitle: str | None = None,
        imeMode: str | None = None,
        operator: str | None = None,
        allow_blank: bool | None = None,
    ) -> None:
        self.type = type
        self.formula1 = formula1
        self.formula2 = formula2
        self.showErrorMessage = showErrorMessage
        self.showInputMessage = showInputMessage
        self.showDropDown = showDropDown
        self.allowBlank = allowBlank if allow_blank is None else allow_blank
        self.promptTitle = promptTitle
        self.errorStyle = errorStyle
        self.error = error
        self.prompt = prompt
        self.errorTitle = errorTitle
        self.imeMode = imeMode
        self.operator = operator
        self._ranges: list[str] = []
        if isinstance(sqref, str):
            self._ranges = sqref.split()
        elif sqref:
            self._ranges = [str(r) for r in sqref]
        self._kx_bound: list[tuple[Any, int]] = []

    @property
    def sqref(self) -> str:
        return " ".join(self._ranges)

    @sqref.setter
    def sqref(self, value: str) -> None:
        self._ranges = str(value).split()
        self._kx_sync()

    @property
    def allow_blank(self) -> bool:
        return self.allowBlank

    @allow_blank.setter
    def allow_blank(self, value: bool) -> None:
        self.allowBlank = value

    def add(self, cell: Any) -> None:
        """Add a cell or range (``"E1"``, ``"E1:E5"``, or a cell object)."""
        coord = getattr(cell, "coordinate", cell)
        self._ranges.append(str(coord))
        self._kx_sync()

    def _kx_sync(self) -> None:
        for ws, idx in self._kx_bound:
            ws._kx_set_data_validation(idx, self._kx_spec())

    def _kx_spec(self) -> dict[str, Any]:
        spec: dict[str, Any] = {
            "sqref": self.sqref,
            "allow_blank": bool(self.allowBlank),
            "show_error_message": bool(self.showErrorMessage),
            "show_input_message": bool(self.showInputMessage),
            "show_drop_down": bool(self.showDropDown),
        }
        for key, val in (
            ("type", self.type),
            ("formula1", self.formula1),
            ("formula2", self.formula2),
            ("operator", self.operator),
            ("error", self.error),
            ("errorTitle", self.errorTitle),
            ("prompt", self.prompt),
            ("promptTitle", self.promptTitle),
            ("errorStyle", self.errorStyle),
            ("imeMode", self.imeMode),
        ):
            if val is not None:
                spec[key] = str(val)
        return spec

    def __repr__(self) -> str:
        return f"<DataValidation type={self.type!r} sqref={self.sqref!r}>"


__all__ = ["DataValidation"]
