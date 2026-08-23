"""Pandas ExcelReader and ExcelWriter engine plugin for kyrax.

Zero feature logic in Python: delegates read path to _TurboReader -> Arrow -> pandas
and write path to write_excel_turbo.
"""

from __future__ import annotations

import os
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    import pandas as pd

from . import _TurboReader, read_excel_turbo, write_excel_turbo, write_excel_turbo_bytes

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

    def parse(
        self,
        sheet_name: str | int | list[int | str] | None = 0,
        header: int | list[int] | None = 0,
        names: list[str] | None = None,
        index_col: int | list[int] | None = None,
        usecols: Any = None,
        skiprows: Any = None,
        nrows: int | None = None,
        **kwargs: Any,
    ) -> Any:
        if sheet_name is None:
            names_list = self.sheet_names
            return {
                name: self.parse(
                    sheet_name=name,
                    header=header,
                    names=names,
                    index_col=index_col,
                    usecols=usecols,
                    skiprows=skiprows,
                    nrows=nrows,
                    **kwargs,
                )
                for name in names_list
            }

        if isinstance(sheet_name, list):
            return {
                name: self.parse(
                    sheet_name=name,
                    header=header,
                    names=names,
                    index_col=index_col,
                    usecols=usecols,
                    skiprows=skiprows,
                    nrows=nrows,
                    **kwargs,
                )
                for name in sheet_name
            }

        reader = getattr(self, "book", None)
        if reader is None and self._path is not None:
            reader = read_excel_turbo(self._path)

        if reader is None:
            raise ValueError("No valid workbook reader available")

        if header is None:
            header_row = None
        elif isinstance(header, int):
            header_row = header
        else:
            raise NotImplementedError("multi-row headers unsupported")

        if isinstance(skiprows, int):
            header_row = None
        elif skiprows is not None:
            raise NotImplementedError("callable/list skiprows unsupported")

        sheet = reader.load_sheet(sheet_name, features="values", header_row=header_row)
        arrow_table = sheet.to_arrow()
        df = arrow_table.to_pandas()

        if isinstance(skiprows, int) and skiprows > 0:
            if header is not None:
                if len(df) > skiprows:
                    df.columns = df.iloc[skiprows - 1].tolist()
                    df = df.iloc[skiprows:].reset_index(drop=True)
                else:
                    df = df.iloc[skiprows:].reset_index(drop=True)
            else:
                df = df.iloc[skiprows:].reset_index(drop=True)

        if header is None and (skiprows is None or not isinstance(skiprows, int)):
            import pandas as pd
            df.columns = pd.RangeIndex(len(df.columns))

        if usecols is not None:
            if callable(usecols):
                cols_to_keep = [c for c in df.columns if usecols(c)]
                df = df[cols_to_keep]
            elif isinstance(usecols, (list, tuple)):
                if all(isinstance(c, int) for c in usecols):
                    df = df.iloc[:, list(usecols)]
                else:
                    df = df[[c for c in usecols if c in df.columns]]
            elif isinstance(usecols, str):
                from .utils import column_index_from_string
                selected_indices = []
                for part in usecols.split(","):
                    part = part.strip()
                    if ":" in part:
                        start_col, end_col = part.split(":", 1)
                        c_start = column_index_from_string(start_col.strip()) - 1
                        c_end = column_index_from_string(end_col.strip()) - 1
                        selected_indices.extend(range(c_start, c_end + 1))
                    else:
                        selected_indices.append(column_index_from_string(part) - 1)
                valid_indices = [i for i in selected_indices if 0 <= i < len(df.columns)]
                df = df.iloc[:, valid_indices]

        if names is not None:
            df.columns = names

        if nrows is not None:
            df = df.iloc[:nrows]

        if index_col is not None:
            if isinstance(index_col, int):
                col_name = df.columns[index_col]
                df = df.set_index(col_name)
            elif isinstance(index_col, (list, tuple)):
                col_names = [df.columns[i] for i in index_col]
                df = df.set_index(col_names)

        return df

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
            sheets_data.append({"name": name, "rows": rows})

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

