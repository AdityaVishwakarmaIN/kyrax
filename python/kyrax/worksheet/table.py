"""openpyxl-compatible ``Table`` / ``TableStyleInfo`` holders.

Converts to the table dict the Rust engine parses. Use with
``ws.add_table(table)``; column names come from the header-row cells at save.
"""

from __future__ import annotations

from typing import Any


class TableStyleInfo:
    """openpyxl ``TableStyleInfo``."""

    def __init__(
        self,
        name: str | None = None,
        showFirstColumn: bool | None = None,
        showLastColumn: bool | None = None,
        showRowStripes: bool | None = None,
        showColumnStripes: bool | None = None,
    ) -> None:
        self.name = name
        self.showFirstColumn = showFirstColumn
        self.showLastColumn = showLastColumn
        self.showRowStripes = showRowStripes
        self.showColumnStripes = showColumnStripes


class Table:
    """openpyxl ``Table`` (``displayName`` + ``ref`` such as ``"A1:D5"``)."""

    def __init__(
        self,
        displayName: str | None = None,
        ref: str | None = None,
        name: str | None = None,
        tableStyleInfo: TableStyleInfo | None = None,
        id: int | None = None,
        **_: Any,
    ) -> None:
        self.displayName = displayName or name
        self.name = name or displayName
        self.ref = ref
        self.tableStyleInfo = tableStyleInfo
        self.id = id  # assigned by kyrax at save; accepted for compatibility

    def _kx_spec(self) -> dict[str, Any]:
        spec: dict[str, Any] = {"display_name": self.displayName, "ref": self.ref}
        style = self.tableStyleInfo
        if style is not None and style.name:
            spec["style_name"] = style.name
            spec["show_row_stripes"] = bool(style.showRowStripes)
            spec["show_first_column"] = bool(style.showFirstColumn)
            spec["show_last_column"] = bool(style.showLastColumn)
            spec["show_column_stripes"] = bool(style.showColumnStripes)
        return spec

    def __repr__(self) -> str:
        return f"<Table displayName={self.displayName!r} ref={self.ref!r}>"


__all__ = ["Table", "TableStyleInfo"]
