"""Pandas ExcelReader and ExcelWriter engine plugin for kyrax.

Delegates worksheet reading to kyrax and parsing to pandas, and writing to
write_excel_turbo.
"""

from __future__ import annotations

import os
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    import pandas as pd

from . import (
    _TurboReader, read_excel_turbo, read_excel_turbo_iter_rows,
    write_excel_turbo, write_excel_turbo_bytes,
)

try:
    from pandas.io.excel._base import BaseExcelReader, ExcelWriter
except ImportError:
    class BaseExcelReader:  # type: ignore[no-redef]
        """Fallback base when pandas is not installed."""
        def __init__(self, filepath_or_buffer: Any, storage_options: Any = None) -> None:
            self.handles = None
            self.book = None

    class ExcelWriter:  # type: ignore[no-redef]
        """Fallback base when pandas is not installed."""
        def __init__(
            self,
            path: Any,
            engine: Any = None,
            date_format: Any = None,
            datetime_format: Any = None,
            mode: str = "w",
            storage_options: Any = None,
            if_sheet_exists: Any = None,
            engine_kwargs: Any = None,
            **kwargs: Any,
        ) -> None:
            self._path = path


class KyraxExcelReader(BaseExcelReader):
    """Pandas ExcelReader engine powered by kyrax Rust core."""

    @property
    def _workbook_class(self) -> type:
        return _TurboReader

    def __init__(
        self,
        filepath_or_buffer: Any,
        storage_options: Any = None,
        engine_kwargs: Any = None,
    ) -> None:
        self.engine_kwargs = engine_kwargs or {}
        self._path: str | None = None
        if isinstance(filepath_or_buffer, (str, os.PathLike)):
            self._path = str(filepath_or_buffer)
        elif hasattr(filepath_or_buffer, "name") and isinstance(filepath_or_buffer.name, str):
            self._path = filepath_or_buffer.name

        super().__init__(filepath_or_buffer, storage_options=storage_options, engine_kwargs=engine_kwargs)

    def load_workbook(self, filepath_or_buffer: Any, engine_kwargs: Any = None) -> Any:
        path = None
        if isinstance(filepath_or_buffer, (str, os.PathLike)):
            path = str(filepath_or_buffer)
        elif hasattr(filepath_or_buffer, "name") and isinstance(filepath_or_buffer.name, str):
            path = filepath_or_buffer.name
        elif self._path is not None:
            path = self._path

        if path is not None:
            return read_excel_turbo(path)
        raise ValueError("kyrax engine requires a seekable file path")

    @property
    def sheet_names(self) -> list[str]:
        if hasattr(self, "book") and self.book is not None:
            return self.book.sheet_names
        if self._path is not None:
            return read_excel_turbo(self._path).sheet_names
        return []

    def get_sheet_by_name(self, name: str) -> int:
        return self.sheet_names.index(name)

    def get_sheet_by_index(self, index: int) -> int:
        if index < 0 or index >= len(self.sheet_names):
            raise ValueError(f"Worksheet index {index} is invalid")
        return index

    def get_sheet_data(self, sheet: int, file_rows_needed: int | None = None) -> list[list[Any]]:
        if self._path is None:
            raise ValueError("kyrax engine requires a file path for pandas parsing")
        rows = read_excel_turbo_iter_rows(
            self._path, sheet_idx=sheet, max_row=file_rows_needed, data_only=True
        )
        try:
            data = []
            for row in rows:
                values = list(row)
                while values and (values[-1] is None or values[-1] == ""):
                    values.pop()
                data.append(values)
            while data and not data[-1]:
                data.pop()
            return data
        finally:
            rows.close()

    def close(self) -> None:
        pass


class KyraxExcelWriter(ExcelWriter):
    """Pandas ExcelWriter engine powered by kyrax Rust core."""

    _engine = "kyrax"
    _supported_extensions = (".xlsx", ".xlsm")

    def __init__(
        self,
        path: Any,
        engine: Any = None,
        date_format: Any = None,
        datetime_format: Any = None,
        mode: str = "w",
        storage_options: Any = None,
        if_sheet_exists: Any = None,
        engine_kwargs: Any = None,
        **kwargs: Any,
    ) -> None:
        if mode != "w":
            raise NotImplementedError("kyrax engine supports mode='w' only")
        self._path = path
        super().__init__(
            path,
            engine=engine,
            date_format=date_format,
            datetime_format=datetime_format,
            mode=mode,
            storage_options=storage_options,
            if_sheet_exists=if_sheet_exists,
            engine_kwargs=engine_kwargs,
        )
        self._sheets: dict[str, list[list[Any]]] = {}
        self._freeze_panes: dict[str, str] = {}

    def _write_cells(
        self,
        cells: Any,
        sheet_name: str | None = None,
        startrow: int = 0,
        startcol: int = 0,
        freeze_panes: tuple[int, int] | None = None,
        autofilter_range: str | None = None,
    ) -> None:
        sheet = sheet_name or "Sheet1"
        if freeze_panes is not None:
            row, col = freeze_panes
            if not (0 <= row < 1048576 and 0 <= col < 16384):
                raise ValueError("freeze_panes must be within the Excel worksheet grid")
            from .utils import get_column_letter

            self._freeze_panes[sheet] = f"{get_column_letter(col + 1)}{row + 1}"
        if sheet not in self._sheets:
            self._sheets[sheet] = []

        grid = self._sheets[sheet]
        for cell in cells:
            r = cell.row + startrow
            c = cell.col + startcol
            while len(grid) <= r:
                grid.append([])
            row_list = grid[r]
            while len(row_list) <= c:
                row_list.append(None)
            val = getattr(cell, "value", getattr(cell, "val", None))
            row_list[c] = val

    write_cells = _write_cells

    def _save(self) -> None:
        if not self._sheets:
            return
        sheets_data = []
        for name, rows in self._sheets.items():
            sheet_data = {"name": name, "rows": rows}
            if name in self._freeze_panes:
                sheet_data["freeze_panes"] = self._freeze_panes[name]
            sheets_data.append(sheet_data)

        data = write_excel_turbo_bytes(sheets_data)

        handles = getattr(self, "_handles", None)
        target = getattr(handles, "handle", None) if handles is not None else getattr(self, "_path", None)
        if hasattr(target, "write"):
            target.write(data)
        elif isinstance(target, (str, os.PathLike)):
            with open(str(target), "wb") as f:
                f.write(data)

    def close(self) -> None:
        self._save()
        handles = getattr(self, "_handles", None)
        if handles is not None and hasattr(handles, "close"):
            handles.close()


def register() -> None:
    """Register kyrax as an Excel reader/writer engine in pandas."""
    try:
        import pandas.io.excel._base as pd_base
        import pandas.io.excel._util as pd_util
        if hasattr(pd_base, "ExcelFile") and hasattr(pd_base.ExcelFile, "_engines"):
            pd_base.ExcelFile._engines["kyrax"] = KyraxExcelReader
        if hasattr(pd_util, "_writers"):
            pd_util._writers["kyrax"] = KyraxExcelWriter
    except Exception:
        pass


register()

