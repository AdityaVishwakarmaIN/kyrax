from __future__ import annotations

import typing
from collections.abc import Callable
from typing import TYPE_CHECKING, Literal, TypeAlias

if TYPE_CHECKING:
    import pandas as pd
    import polars as pl
    import pyarrow as pa

from os.path import expanduser
from pathlib import Path

try:
    import importlib.util

    importlib.util.find_spec("pyarrow")
    _PYARROW_AVAILABLE = True
except ImportError:
    _PYARROW_AVAILABLE = False

from ._kyrax import (
    ArrowError,
    CalamineCellError,
    CalamineError,
    CannotRetrieveCellDataError,
    CellError,
    CellErrors,
    ColumnInfo,
    ColumnInfoNoDtype,
    ColumnNotFoundError,
    DefinedName,
    InvalidParametersError,
    KyraxError,
    SheetNotFoundError,
    UnsupportedColumnTypeCombinationError,
    __version__,
    _ExcelReader,
    _ExcelSheet,
    _ExcelTable,
    _TurboReader,
    _TurboSheet,
)
from ._kyrax import is_encrypted as _is_encrypted
from ._kyrax import read_excel as _read_excel
from ._kyrax import read_excel_turbo as _read_excel_turbo
from ._kyrax import repair_excel as _repair_excel
from ._kyrax import validate_excel as _validate_excel
from ._kyrax import write_excel_turbo as _write_excel_turbo
from ._kyrax import write_excel_turbo_bytes as _write_excel_turbo_bytes
from ._kyrax import write_excel_turbo_stream as _write_excel_turbo_stream

try:
    from ._kyrax import encryption_info as _encryption_info
except ImportError:
    _encryption_info = None  # type: ignore[assignment]  # ty: ignore[invalid-assignment]
try:
    from ._kyrax import EditableSheet, EditableWorkbook, edit_excel
except ImportError:
    edit_excel = None  # type: ignore[assignment]  # ty: ignore[invalid-assignment]
    EditableWorkbook = None  # type: ignore[misc, assignment]  # ty: ignore[invalid-assignment]
    EditableSheet = None  # type: ignore[misc, assignment]  # ty: ignore[invalid-assignment]


def validate_excel(path: Path | str) -> dict:
    """Validate a workbook and return a structured report — never raises for a
    bad input.

    :param path: Path to an ``.xlsx``/``.xlsm`` file (or anything else).
    :return: ``{"valid", "errors", "warnings", "infos", "findings"}`` where each
        finding is ``{"code", "severity", "part", "location", "message",
        "repairable"}``. The ``code`` is a stable string (``"encrypted_workbook"``,
        ``"legacy_biff"``, ``"not_ooxml_package"``, ``"corrupt_zip"``,
        ``"dangling_rel"``, ``"overlapping_merge"``, ...) so callers can branch
        on it without string matching the message.
    """
    if isinstance(path, Path):
        path = expanduser(str(path))
    return _validate_excel(str(path))


def repair_excel(
    path: Path | str,
    out_path: Path | str,
    *,
    severity: Literal["error", "warning", "info"] = "warning",
) -> dict:
    """Repair a workbook conservatively into ``out_path``.

    Only repairable findings at ``severity`` or above are fixed, and every
    change is reported. The source file is never modified. Encrypted / legacy /
    non-spreadsheet / unreadable inputs write nothing.

    :return: ``{"wrote_output", "report", "actions"}`` where each action is
        ``{"code", "severity", "part", "description", "before", "after"}``.
    """
    if isinstance(path, Path):
        path = expanduser(str(path))
    if isinstance(out_path, Path):
        out_path = expanduser(str(out_path))
    return _repair_excel(str(path), str(out_path), severity=severity)


def load_workbook(filename: str | Path, edit_mode: bool = False):
    """Load an Excel workbook.

    With ``edit_mode=False`` (default) this returns an :class:`ExcelReader` for
    reading. With ``edit_mode=True`` it returns an :class:`EditableWorkbook`
    (backed by :func:`edit_excel`) that records byte-preserving edits and
    applies them on :meth:`EditableWorkbook.save`:

    - ``wb[sheet].set_cell(row, col, value)`` — 1-based cell edit
    - ``wb[sheet].set_cell_style(row, col, ...)`` — 1-based cell style
    - ``wb[sheet].insert_rows(idx, amount=1)`` / ``insert_cols(...)`` — insert
      blank rows/columns at 1-based ``idx``, shifting the grid down/right
    - ``wb[sheet].delete_rows(idx, amount=1)`` / ``delete_cols(...)`` — delete
      rows/columns starting at 1-based ``idx``, shifting the grid up/left
    - ``wb[sheet].move_range(range_string, rows=0, cols=0, translate=False)`` —
      relocate a rectangular block by ``rows``/``cols`` without shifting the
      rest of the sheet; ``translate=True`` also shifts formula references
      inside the moved range

    Shifts are applied before cell edits (an edit coordinate is final while a
    shift moves the grid under it). ``save`` is all-or-nothing: a refusal
    (implicit-numbered row/cell at or below the shift point, a grid limit
    exceeded, a shared-formula master orphaned, or a table header row deleted)
    raises ``InvalidParametersError`` and leaves the destination untouched.
    """
    if edit_mode:
        if edit_excel is None:
            raise NotImplementedError("edit_excel is not available in this build")
        return edit_excel(str(filename))
    return read_excel(filename)


DType = Literal["null", "int", "float", "string", "boolean", "datetime", "date", "duration"]
DTypeMap: TypeAlias = "dict[str | int, DType]"
ColumnNameFrom: TypeAlias = Literal["provided", "looked_up", "generated"]
DTypeFrom: TypeAlias = Literal[
    "provided_for_all", "provided_by_index", "provided_by_name", "guessed"
]
SheetVisible: TypeAlias = Literal["visible", "hidden", "veryhidden"]


class ExcelSheet:
    """A class representing a single sheet in an Excel File"""

    def __init__(self, sheet: _ExcelSheet) -> None:
        self._sheet = sheet

    @property
    def name(self) -> str:
        """The name of the sheet"""
        return self._sheet.name

    @property
    def width(self) -> int:
        """The sheet's width"""
        return self._sheet.width

    @property
    def height(self) -> int:
        """The sheet's height, with `skip_rows` and `nrows` applied"""
        return self._sheet.height

    @property
    def total_height(self) -> int:
        """The sheet's total height"""
        return self._sheet.total_height

    @property
    def selected_columns(self) -> list[ColumnInfo]:
        """The sheet's selected columns"""
        return self._sheet.selected_columns

    def available_columns(self) -> list[ColumnInfo]:
        """The columns available for the given sheet"""
        return self._sheet.available_columns()

    @property
    def specified_dtypes(self) -> DTypeMap | None:
        """The dtypes specified for the sheet"""
        return self._sheet.specified_dtypes

    @property
    def visible(self) -> SheetVisible:
        """The visibility of the sheet"""
        return self._sheet.visible

    def to_arrow(self) -> pa.RecordBatch:
        """Converts the sheet to a pyarrow `RecordBatch`

        Requires the `pyarrow` extra to be installed.
        """
        if not _PYARROW_AVAILABLE:
            raise ImportError(
                "pyarrow is required for to_arrow(). Install with: pip install 'kyrax[pyarrow]'"
            )
        return self._sheet.to_arrow()

    def to_arrow_with_errors(self) -> tuple[pa.RecordBatch, CellErrors | None]:
        """Converts the sheet to a pyarrow `RecordBatch` with error information.

        Stores the positions of any values that cannot be parsed as the specified type and were
        therefore converted to None.

        Requires the `pyarrow` extra to be installed.
        """
        if not _PYARROW_AVAILABLE:
            raise ImportError(
                "pyarrow is required for to_arrow_with_errors(). Install with: pip install 'kyrax[pyarrow]'"  # noqa: E501
            )
        rb, cell_errors = self._sheet.to_arrow_with_errors()
        if not cell_errors.errors:
            return (rb, None)
        return (rb, cell_errors)

    def to_pandas(self) -> pd.DataFrame:
        """Converts the sheet to a Pandas `DataFrame`.

        Requires the `pandas` extra to be installed.
        """
        # Note: pandas PyCapsule interface requires __dataframe__ or __arrow_c_stream__
        # which we don't implement. Using pyarrow conversion for now.
        # (see https://pandas.pydata.org/docs/reference/api/pandas.api.interchange.from_dataframe.html)
        return self.to_arrow().to_pandas()

    def to_polars(self) -> pl.DataFrame:
        """Converts the sheet to a Polars `DataFrame`.

        Uses the Arrow PyCapsule Interface for zero-copy data exchange.
        Requires the `polars` extra to be installed.
        """
        import polars as pl

        return pl.DataFrame(self)

    def __arrow_c_schema__(self) -> object:
        """Export the schema as an `ArrowSchema` `PyCapsule`.

        https://arrow.apache.org/docs/format/CDataInterface/PyCapsuleInterface.html#arrowschema-export

        The Arrow PyCapsule Interface enables zero-copy data exchange with
        Arrow-compatible libraries without requiring PyArrow as a dependency.
        """
        return self._sheet.__arrow_c_schema__()

    def __arrow_c_array__(self, requested_schema: object | None = None) -> tuple[object, object]:
        """Export the schema and data as a pair of `ArrowSchema` and `ArrowArray` `PyCapsules`.

        The optional `requested_schema` parameter allows for potential schema conversion.

        https://arrow.apache.org/docs/format/CDataInterface/PyCapsuleInterface.html#arrowarray-export

        The Arrow PyCapsule Interface enables zero-copy data exchange with
        Arrow-compatible libraries without requiring PyArrow as a dependency.
        """
        return self._sheet.__arrow_c_array__(requested_schema)

    def __repr__(self) -> str:
        return self._sheet.__repr__()


class ExcelTable:
    """A class representing a single table in an Excel file"""

    def __init__(self, table: _ExcelTable) -> None:
        self._table = table

    @property
    def name(self) -> str:
        """The name of the table"""
        return self._table.name

    @property
    def sheet_name(self) -> str:
        """The name of the sheet this table belongs to"""
        return self._table.sheet_name

    @property
    def width(self) -> int:
        """The table's width"""
        return self._table.width

    @property
    def height(self) -> int:
        """The table's height"""
        return self._table.height

    @property
    def total_height(self) -> int:
        """The table's total height"""
        return self._table.total_height

    @property
    def offset(self) -> int:
        """The table's offset before data starts"""
        return self._table.offset

    @property
    def selected_columns(self) -> list[ColumnInfo]:
        """The table's selected columns"""
        return self._table.selected_columns

    def available_columns(self) -> list[ColumnInfo]:
        """The columns available for the given table"""
        return self._table.available_columns()

    @property
    def specified_dtypes(self) -> DTypeMap | None:
        """The dtypes specified for the table"""
        return self._table.specified_dtypes

    def to_arrow(self) -> pa.RecordBatch:
        """Converts the table to a pyarrow `RecordBatch`

        Requires the `pyarrow` extra to be installed.
        """
        if not _PYARROW_AVAILABLE:
            raise ImportError(
                "pyarrow is required for to_arrow(). Install with: pip install 'kyrax[pyarrow]'"
            )
        return self._table.to_arrow()

    def to_pandas(self) -> pd.DataFrame:
        """Converts the table to a Pandas `DataFrame`.

        Requires the `pandas` extra to be installed.
        """
        # Note: pandas PyCapsule interface requires __dataframe__ or __arrow_c_stream__
        # which we don't implement. Using pyarrow conversion for now.
        # (see https://pandas.pydata.org/docs/reference/api/pandas.api.interchange.from_dataframe.html)
        return self.to_arrow().to_pandas()

    def to_polars(self) -> pl.DataFrame:
        """Converts the table to a Polars `DataFrame`.

        Uses the Arrow PyCapsule Interface for zero-copy data exchange.
        Requires the `polars` extra to be installed.
        """
        import polars as pl

        return pl.DataFrame(self)

    def __arrow_c_schema__(self) -> object:
        """Export the schema as an `ArrowSchema` `PyCapsule`.

        https://arrow.apache.org/docs/format/CDataInterface/PyCapsuleInterface.html#arrowschema-export

        The Arrow PyCapsule Interface enables zero-copy data exchange with
        Arrow-compatible libraries without requiring PyArrow as a dependency.
        """
        return self._table.__arrow_c_schema__()

    def __arrow_c_array__(self, requested_schema: object | None = None) -> tuple[object, object]:
        """Export the schema and data as a pair of `ArrowSchema` and `ArrowArray` `PyCapsules`.

        The optional `requested_schema` parameter allows for potential schema conversion.

        https://arrow.apache.org/docs/format/CDataInterface/PyCapsuleInterface.html#arrowarray-export

        The Arrow PyCapsule Interface enables zero-copy data exchange with
        Arrow-compatible libraries without requiring PyArrow as a dependency.
        """
        return self._table.__arrow_c_array__(requested_schema)


class ExcelReader:
    """A class representing an open Excel file and allowing to read its sheets"""

    def __init__(self, reader: _ExcelReader) -> None:
        self._reader = reader

    @property
    def sheet_names(self) -> list[str]:
        """The list of sheet names"""
        return self._reader.sheet_names

    @typing.overload
    def load_sheet(
        self,
        idx_or_name: int | str,
        *,
        header_row: int | None = 0,
        column_names: list[str] | None = None,
        skip_rows: int | list[int] | Callable[[int], bool] | None = None,
        n_rows: int | None = None,
        schema_sample_rows: int | None = 1_000,
        dtype_coercion: Literal["coerce", "strict"] = "coerce",
        use_columns: list[str]
        | list[int]
        | str
        | Callable[[ColumnInfoNoDtype], bool]
        | None = None,
        dtypes: DType | DTypeMap | None = None,
        eager: Literal[False] = ...,
        skip_whitespace_tail_rows: bool = False,
        whitespace_as_null: bool = False,
    ) -> ExcelSheet: ...

    @typing.overload
    def load_sheet(
        self,
        idx_or_name: int | str,
        *,
        header_row: int | None = 0,
        column_names: list[str] | None = None,
        skip_rows: int | list[int] | Callable[[int], bool] | None = None,
        n_rows: int | None = None,
        schema_sample_rows: int | None = 1_000,
        dtype_coercion: Literal["coerce", "strict"] = "coerce",
        use_columns: list[str]
        | list[int]
        | str
        | Callable[[ColumnInfoNoDtype], bool]
        | None = None,
        dtypes: DType | DTypeMap | None = None,
        eager: Literal[True] = ...,
        skip_whitespace_tail_rows: bool = False,
        whitespace_as_null: bool = False,
    ) -> pa.RecordBatch: ...

    def load_sheet(
        self,
        idx_or_name: int | str,
        *,
        header_row: int | None = 0,
        column_names: list[str] | None = None,
        skip_rows: int | list[int] | Callable[[int], bool] | None = None,
        n_rows: int | None = None,
        schema_sample_rows: int | None = 1_000,
        dtype_coercion: Literal["coerce", "strict"] = "coerce",
        use_columns: list[str]
        | list[int]
        | str
        | Callable[[ColumnInfoNoDtype], bool]
        | None = None,
        dtypes: DType | DTypeMap | None = None,
        eager: bool = False,
        skip_whitespace_tail_rows: bool = False,
        whitespace_as_null: bool = False,
    ) -> ExcelSheet | pa.RecordBatch:
        """Loads a sheet by index or name.

        No-silent-data-loss read contract: When a column's inferred dtype (guessed from
        `schema_sample_rows`, default 1000) disagrees with values later in the column,
        kyrax widens the column to string and emits a `UserWarning` naming the column
        instead of silently nulling those cells (openpyxl never loses a cell value, so
        silent nulling was the one behaviour with no openpyxl analogue). Under
        `dtype_coercion='strict'`, an error is raised instead. Passing
        `schema_sample_rows=None` samples the full column. An explicit `dtypes`
        argument is never second-guessed.

        :param idx_or_name: The index (starting at 0) or the name of the sheet to load.
        :param header_row: The index of the row containing the column labels, default index is 0.
                           If `None`, the sheet does not have any column labels.
                           Any rows before the `header_row` will be automatically skipped.
        :param column_names: Overrides headers found in the document.
                             If `column_names` is used, `header_row` will be ignored.
        :param n_rows: Specifies how many rows should be loaded.
                       If `None`, all rows are loaded
        :param skip_rows: Specifies which rows should be skipped after the `header_row`.
                          Any rows before the `header_row` are automatically skipped.
                          It means row indices are relative to data rows, not the sheet!
                          Can be one of:
                          - `int`: Skip this many rows after the header row
                          - `list[int]`: Skip specific row indices (0-based relative to data rows)
                          - `Callable[[int], bool]`: Function that receives row index (0-based
                          relative to data rows) and returns True to skip the row
                          - `None`: If `header_row` is None, skips empty rows at beginning
        :param schema_sample_rows: Specifies how many rows should be used to determine
                                   the dtype of a column. Cannot be 0. A specific dtype can be
                                   enforced for some or all columns through the `dtypes` parameter.
                                   If `None`, all rows will be used.
        :param dtype_coercion: Specifies how type coercion should behave. `coerce` (the default)
                               will try to coerce different dtypes in a column to the same one,
                               whereas `strict` will raise an error in case a column contains
                               several dtypes. Note that this only applies to columns whose dtype
                               is guessed, i.e. not specified via `dtypes`.
        :param use_columns: Specifies the columns to use. Can either be:
                            - `None` to select all columns
                            - A list of strings and ints, the column names and/or indices
                              (starting at 0)
                            - A string, a comma separated list of Excel column letters and column
                              ranges (e.g. `"A:E"` or `"A,C,E:F"`, which would result in
                              `A,B,C,D,E` and `A,C,E,F`). Also supports open-ended ranges
                              (e.g. `"B:"` to select all columns from B onwards) and from-beginning
                              ranges (e.g. `":C"` to select columns from A to C). These can be
                              combined for "except" patterns (e.g. `":C,E:"` to select everything
                              except column D)
                            - A callable, a function that takes a column and returns a boolean
                              indicating whether the column should be used
        :param dtypes: An optional dtype (for all columns)
                       or dict of dtypes with keys as column indices or names.
        :param eager: Specifies whether the sheet should be loaded eagerly.
                      `False` (default) will load the sheet lazily using the `PyCapsule` interface,
                      whereas `True` will load it eagerly via `pyarrow`.

                      Eager loading requires the `pyarrow` extra to be installed.
        :param skip_whitespace_tail_rows: Skip rows at the end of the sheet
                                          containing only whitespace and null values.
        :param whitespace_as_null: Consider cells containing only whitespace as null values.
        """
        sheet_or_rb = self._reader.load_sheet(
            idx_or_name=idx_or_name,
            header_row=header_row,
            column_names=column_names,
            skip_rows=skip_rows,
            n_rows=n_rows,
            schema_sample_rows=schema_sample_rows,
            dtype_coercion=dtype_coercion,
            use_columns=use_columns,
            dtypes=dtypes,
            eager=eager,
            skip_whitespace_tail_rows=skip_whitespace_tail_rows,
            whitespace_as_null=whitespace_as_null,
        )
        return sheet_or_rb if eager else ExcelSheet(sheet_or_rb)

    def table_names(self, sheet_name: str | None = None) -> list[str]:
        """The list of table names.

        Will return an empty list if no tables are found.

        :param sheet_name: If given, will limit the list to the given sheet, will be faster
        too.
        """
        return self._reader.table_names(sheet_name)

    def defined_names(self) -> list[DefinedName]:
        """The list of defined names (named ranges) in the workbook.

        Returns a list of DefinedName objects with 'name' and 'formula' attributes.
        The formula is a string representation of the range or expression.

        Will return an empty list if no defined names are found.
        """
        return self._reader.defined_names()

    @typing.overload
    def load_table(
        self,
        name: str,
        *,
        header_row: int | None = None,
        column_names: list[str] | None = None,
        skip_rows: int | None = None,
        n_rows: int | None = None,
        schema_sample_rows: int | None = 1_000,
        dtype_coercion: Literal["coerce", "strict"] = "coerce",
        use_columns: list[str]
        | list[int]
        | str
        | Callable[[ColumnInfoNoDtype], bool]
        | None = None,
        dtypes: DType | DTypeMap | None = None,
        eager: Literal[False] = ...,
        skip_whitespace_tail_rows: bool = False,
        whitespace_as_null: bool = False,
    ) -> ExcelTable: ...

    @typing.overload
    def load_table(
        self,
        name: str,
        *,
        header_row: int | None = None,
        column_names: list[str] | None = None,
        skip_rows: int | None = None,
        n_rows: int | None = None,
        schema_sample_rows: int | None = 1_000,
        dtype_coercion: Literal["coerce", "strict"] = "coerce",
        use_columns: list[str]
        | list[int]
        | str
        | Callable[[ColumnInfoNoDtype], bool]
        | None = None,
        dtypes: DType | DTypeMap | None = None,
        eager: Literal[True] = ...,
        skip_whitespace_tail_rows: bool = False,
        whitespace_as_null: bool = False,
    ) -> pa.RecordBatch: ...

    def load_table(
        self,
        name: str,
        *,
        header_row: int | None = None,
        column_names: list[str] | None = None,
        skip_rows: int | None = None,
        n_rows: int | None = None,
        schema_sample_rows: int | None = 1_000,
        dtype_coercion: Literal["coerce", "strict"] = "coerce",
        use_columns: list[str]
        | list[int]
        | str
        | Callable[[ColumnInfoNoDtype], bool]
        | None = None,
        dtypes: DType | DTypeMap | None = None,
        eager: bool = False,
        skip_whitespace_tail_rows: bool = False,
        whitespace_as_null: bool = False,
    ) -> ExcelTable | pa.RecordBatch:
        """Loads a table by name.

        No-silent-data-loss read contract: When a column's inferred dtype (guessed from
        `schema_sample_rows`, default 1000) disagrees with values later in the column,
        kyrax widens the column to string and emits a `UserWarning` naming the column
        instead of silently nulling those cells (openpyxl never loses a cell value, so
        silent nulling was the one behaviour with no openpyxl analogue). Under
        `dtype_coercion='strict'`, an error is raised instead. Passing
        `schema_sample_rows=None` samples the full column. An explicit `dtypes`
        argument is never second-guessed.

        :param name: The name of the table to load.
        :param header_row: The index of the row containing the column labels.
                           If `None`, the table's column names will be used.
                           Any rows before the `header_row` will be automatically skipped.
        :param column_names: Overrides headers found in the document.
                             If `column_names` is used, `header_row` will be ignored.
        :param n_rows: Specifies how many rows should be loaded.
                       If `None`, all rows are loaded
        :param skip_rows: Specifies how many rows should be skipped after the `header_row`.
                          Any rows before the `header_row` are automatically skipped.
                          If `header_row` is `None`, it skips the number of rows from the
                          start of the sheet.
        :param schema_sample_rows: Specifies how many rows should be used to determine
                                   the dtype of a column. Cannot be 0. A specific dtype can be
                                   enforced for some or all columns through the `dtypes` parameter.
                                   If `None`, all rows will be used.
        :param dtype_coercion: Specifies how type coercion should behave. `coerce` (the default)
                               will try to coerce different dtypes in a column to the same one,
                               whereas `strict` will raise an error in case a column contains
                               several dtypes. Note that this only applies to columns whose dtype
                               is guessed, i.e. not specified via `dtypes`.
        :param use_columns: Specifies the columns to use. Can either be:
                            - `None` to select all columns
                            - A list of strings and ints, the column names and/or indices
                              (starting at 0)
                            - A string, a comma separated list of Excel column letters and column
                              ranges (e.g. `"A:E"` or `"A,C,E:F"`, which would result in
                              `A,B,C,D,E` and `A,C,E,F`). Also supports open-ended ranges
                              (e.g. `"B:"` to select all columns from B onwards) and from-beginning
                              ranges (e.g. `":C"` to select columns from A to C). These can be
                              combined for "except" patterns (e.g. `":C,E:"` to select everything
                              except column D)
                            - A callable, a function that takes a column and returns a boolean
                              indicating whether the column should be used
        :param dtypes: An optional dtype (for all columns)
                       or dict of dtypes with keys as column indices or names.
        :param eager: Specifies whether the table should be loaded eagerly.
                      `False` (default) will load the table lazily using the `PyCapsule` interface,
                      whereas `True` will load it eagerly via `pyarrow`.

                      Eager loading requires the `pyarrow` extra to be installed.
        :param skip_whitespace_tail_rows: Skip rows at the end of the table
                                          containing only whitespace and null values.
        :param whitespace_as_null: Consider cells containing only whitespace as null values.
        """
        if eager:
            return self._reader.load_table(
                name=name,
                header_row=header_row,
                column_names=column_names,
                skip_rows=skip_rows,
                n_rows=n_rows,
                schema_sample_rows=schema_sample_rows,
                dtype_coercion=dtype_coercion,
                use_columns=use_columns,
                dtypes=dtypes,
                eager=True,
                skip_whitespace_tail_rows=skip_whitespace_tail_rows,
                whitespace_as_null=whitespace_as_null,
            )
        else:
            return ExcelTable(
                self._reader.load_table(
                    name=name,
                    header_row=header_row,
                    column_names=column_names,
                    skip_rows=skip_rows,
                    n_rows=n_rows,
                    schema_sample_rows=schema_sample_rows,
                    dtype_coercion=dtype_coercion,
                    use_columns=use_columns,
                    dtypes=dtypes,
                    eager=False,
                    skip_whitespace_tail_rows=skip_whitespace_tail_rows,
                    whitespace_as_null=whitespace_as_null,
                )
            )

    def load_sheet_eager(
        self,
        idx_or_name: int | str,
        *,
        header_row: int | None = 0,
        column_names: list[str] | None = None,
        skip_rows: int | list[int] | Callable[[int], bool] | None = None,
        n_rows: int | None = None,
        schema_sample_rows: int | None = 1_000,
        dtype_coercion: Literal["coerce", "strict"] = "coerce",
        use_columns: list[str] | list[int] | str | None = None,
        dtypes: DType | DTypeMap | None = None,
    ) -> pa.RecordBatch:
        """Loads a sheet eagerly by index or name.

        For xlsx files, this will be faster and more memory-efficient, as it will use
        `worksheet_range_ref` under the hood, which returns borrowed types.

        Refer to `load_sheet` for parameter documentation

        Requires the `pyarrow` extra to be installed.
        """
        return self._reader.load_sheet(
            idx_or_name=idx_or_name,
            header_row=header_row,
            column_names=column_names,
            skip_rows=skip_rows,
            n_rows=n_rows,
            schema_sample_rows=schema_sample_rows,
            dtype_coercion=dtype_coercion,
            use_columns=use_columns,
            dtypes=dtypes,
            eager=True,
        )

    def load_sheet_by_name(
        self,
        name: str,
        *,
        header_row: int | None = 0,
        column_names: list[str] | None = None,
        skip_rows: int | None = None,
        n_rows: int | None = None,
        schema_sample_rows: int | None = 1_000,
        dtype_coercion: Literal["coerce", "strict"] = "coerce",
        use_columns: list[str]
        | list[int]
        | str
        | Callable[[ColumnInfoNoDtype], bool]
        | None = None,
        dtypes: DType | DTypeMap | None = None,
    ) -> ExcelSheet:
        """Loads a sheet by name.

        Refer to `load_sheet` for parameter documentation
        """
        return self.load_sheet(
            name,
            header_row=header_row,
            column_names=column_names,
            skip_rows=skip_rows,
            n_rows=n_rows,
            schema_sample_rows=schema_sample_rows,
            dtype_coercion=dtype_coercion,
            use_columns=use_columns,
            dtypes=dtypes,
        )

    def load_sheet_by_idx(
        self,
        idx: int,
        *,
        header_row: int | None = 0,
        column_names: list[str] | None = None,
        skip_rows: int | None = None,
        n_rows: int | None = None,
        schema_sample_rows: int | None = 1_000,
        dtype_coercion: Literal["coerce", "strict"] = "coerce",
        use_columns: list[str]
        | list[int]
        | str
        | Callable[[ColumnInfoNoDtype], bool]
        | None = None,
        dtypes: DType | DTypeMap | None = None,
    ) -> ExcelSheet:
        """Loads a sheet by index.

        Refer to `load_sheet` for parameter documentation
        """
        return self.load_sheet(
            idx,
            header_row=header_row,
            column_names=column_names,
            skip_rows=skip_rows,
            n_rows=n_rows,
            schema_sample_rows=schema_sample_rows,
            dtype_coercion=dtype_coercion,
            use_columns=use_columns,
            dtypes=dtypes,
        )

    def __repr__(self) -> str:
        return self._reader.__repr__()


def read_excel(source: Path | str | bytes) -> ExcelReader:
    """Opens and loads an excel file.

    No-silent-data-loss read contract: When a column's inferred dtype (guessed from
    `schema_sample_rows`, default 1000) disagrees with values later in the column,
    kyrax widens the column to string and emits a `UserWarning` naming the column
    instead of silently nulling those cells (openpyxl never loses a cell value, so
    silent nulling was the one behaviour with no openpyxl analogue). Under
    `dtype_coercion='strict'`, an error is raised instead. Passing
    `schema_sample_rows=None` samples the full column. An explicit `dtypes`
    argument is never second-guessed.

    :param source: The path to a file or its content as bytes
    """
    if isinstance(source, str | Path):
        source = expanduser(source)
    return ExcelReader(_read_excel(source))


class TurboSheet:
    """A worksheet loaded via the turbo fast path."""

    def __init__(self, sheet: _TurboSheet) -> None:
        self._sheet = sheet

    @property
    def name(self) -> str:
        return self._sheet.name

    @property
    def nrows(self) -> int:
        return self._sheet.nrows

    @property
    def ncols(self) -> int:
        return self._sheet.ncols

    @property
    def column_names(self) -> list[str]:
        return self._sheet.column_names

    def to_arrow(self) -> pa.RecordBatch:
        """Values as a pyarrow RecordBatch (dictionary-encoded strings OK).

        Cached formula results are present in value columns (both-not-XOR with
        formula text). Typed cell errors (`t=\"e\"`) are listed via
        :meth:`cell_errors`; pure-error columns may still show the error code
        string in values, while errors mixed into numeric columns are null.
        """
        if not _PYARROW_AVAILABLE:
            raise ImportError(
                "pyarrow is required for to_arrow(). Install with: pip install 'kyrax[pyarrow]'"
            )
        return self._sheet.to_arrow()

    def cell_errors(self) -> pa.RecordBatch:
        """Sparse typed error caches from ``t=\"e\"`` cells as a RecordBatch.

        Columns: ``row``, ``col`` (uint32, 0-based data indices, header
        excluded), ``code`` (utf8, e.g. ``\"#DIV/0!\"``). Always available
        (empty batch when the sheet has no error cells).
        """
        if not _PYARROW_AVAILABLE:
            raise ImportError(
                "pyarrow is required for cell_errors(). Install with: pip install 'kyrax[pyarrow]'"
            )
        return self._sheet.cell_errors()

    def style_indices(self) -> list[list[int]] | None:
        """Per-column uint32 xf indices (list of lists: length=ncols, each len=nrows).

        Returns None if styles were not requested.
        """
        return self._sheet.style_indices()

    def style_table(self) -> list[dict] | None:
        """Resolved cellXfs (one dict per xf). None if styles not requested."""
        return self._sheet.style_table()

    def formulas(self) -> pa.RecordBatch | None:
        """Sparse formulas with shared text translated as a RecordBatch.

        Columns: ``row``, ``col`` (uint32, 0-based data indices, header
        excluded), ``kind`` (plain|shared|array|dataTable), ``text`` (utf8,
        shared translated), ``ref`` (utf8, null unless array). None if not
        requested.
        """
        if not _PYARROW_AVAILABLE:
            raise ImportError(
                "pyarrow is required for formulas(). Install with: pip install 'kyrax[pyarrow]'"
            )
        return self._sheet.formulas()

    def merges(self) -> list[str] | None:
        """Merged ranges as A1 strings (\"A1:B2\"). None if not requested."""
        return self._sheet.merges()

    def hyperlinks(self) -> list[dict] | None:
        """Hyperlinks as dicts. None if not requested."""
        return self._sheet.hyperlinks()

    def comments(self) -> pa.RecordBatch | None:
        """Comments as a RecordBatch: ref (A1), author, text. None if not requested."""
        if not _PYARROW_AVAILABLE:
            raise ImportError(
                "pyarrow is required for comments(). Install with: pip install 'kyrax[pyarrow]'"
            )
        return self._sheet.comments()

    def comment_authors(self) -> list[str] | None:
        """Author table for comments when comments were requested."""
        return self._sheet.comment_authors()

    @property
    def legacy_is_mirror(self) -> bool:
        """True when legacy comments are Excel mirrors of threaded comments on this sheet."""
        return self._sheet.legacy_is_mirror

    def threaded_comments(self) -> list[dict] | None:
        """Threaded comments (Office 2018+). None if comments not requested."""
        return self._sheet.threaded_comments()

    def charts(self) -> list[dict] | None:
        """Chart structured metadata on this sheet. None if charts not requested."""
        return self._sheet.charts()

    def images(self) -> list[dict] | None:
        """Images on this sheet (``data`` bytes + ``anchor`` dict).

        None if ``images`` was not requested.
        """
        return self._sheet.images()

    def pivots(self) -> list[dict] | None:
        """Pivot table metadata on this sheet. None if pivots not requested."""
        return self._sheet.pivots()

    def tables(self) -> list[dict] | None:
        """Tables on this sheet. None if not requested."""
        return self._sheet.tables()

    # --- Stream A: sheet / workbook metadata ---

    @property
    def sheet_state(self) -> str:
        """Sheet visibility: ``visible`` | ``hidden`` | ``veryHidden``."""
        return self._sheet.sheet_state

    @property
    def sheet_kind(self) -> str:
        """``worksheet`` or ``chartsheet``."""
        return self._sheet.sheet_kind

    def row_dimensions(self) -> dict | None:
        """Explicitly-set row dimensions ``{row: {height, hidden, ...}}`` (1-based keys)."""
        return self._sheet.row_dimensions()

    def column_dimensions(self) -> list[dict] | None:
        """Column dimension records (min/max/width/hidden/...)."""
        return self._sheet.column_dimensions()

    def sheet_format(self) -> dict | None:
        """Default row/col format props from ``sheetFormatPr``."""
        return self._sheet.sheet_format()

    def auto_filter(self) -> dict | None:
        """AutoFilter ref + filter columns, or None."""
        return self._sheet.auto_filter()

    def sheet_view(self) -> dict | None:
        """Active sheetView props + optional pane."""
        return self._sheet.sheet_view()

    def freeze_panes(self) -> str | None:
        """Freeze panes top-left cell A1 (when frozen), else None."""
        return self._sheet.freeze_panes()

    def protection(self) -> dict | None:
        """Sheet protection flags + password hash fields."""
        return self._sheet.protection()

    def page_setup(self) -> dict | None:
        """Print page setup (orientation, paper size, scale, fit...)."""
        return self._sheet.page_setup()

    def page_margins(self) -> dict | None:
        """Page margins in inches."""
        return self._sheet.page_margins()

    def print_options(self) -> dict | None:
        """Print options (centered, headings, grid lines)."""
        return self._sheet.print_options()

    def header_footer(self) -> dict | None:
        """Header/footer raw strings (unescaped, with ``&L``/``&C``/``&R``)."""
        return self._sheet.header_footer()

    @property
    def code_name(self) -> str | None:
        """VBA sheet code name from ``sheetPr``."""
        return self._sheet.code_name

    @property
    def tab_color(self) -> str | None:
        """Tab color (rgb hex or ``theme:N``)."""
        return self._sheet.tab_color

    # --- Stream B: rich cell metadata ---

    def data_validations(self) -> list[dict] | None:
        """Data validation records (sheet-level). None if not requested."""
        return self._sheet.data_validations()

    def conditional_formatting(self) -> list[dict] | None:
        """Flat CF rule list with resolved dxf when available."""
        return self._sheet.conditional_formatting()

    def named_styles(self) -> list[dict] | None:
        """Named styles from styles.xml (when styles loaded)."""
        return self._sheet.named_styles()

    def __repr__(self) -> str:
        return self._sheet.__repr__()


class TurboReader:
    """Workbook handle for the turbo fast path."""

    def __init__(self, reader: _TurboReader) -> None:
        self._reader = reader

    @property
    def sheet_names(self) -> list[str]:
        return self._reader.sheet_names

    def load_sheet(
        self,
        idx_or_name: int | str,
        *,
        features: list[str] | str = "values",
    ) -> TurboSheet:
        """Load a sheet with selective feature extraction.

        :param idx_or_name: Sheet index (0-based) or name.
        :param features: ``\"values\"`` (default), ``\"all\"``, or a list from
            {styles, formulas, merges, defined_names, tables, hyperlinks, comments,
            sheet_meta, page_setup, workbook_meta, validations, cond_format,
            charts, images, pivots, vba}.
            Values are always included; unrequested features are not computed.
            ``comments`` also loads threaded comments + persons.
        """
        return TurboSheet(self._reader.load_sheet(idx_or_name, features=features))

    def defined_names(self) -> list[dict] | None:
        """Workbook defined names from the last load that requested them."""
        return self._reader.defined_names()

    def tables(self) -> list[dict] | None:
        """All tables from the last load that requested tables."""
        return self._reader.tables()

    @property
    def date1904(self) -> bool:
        """1904 date system flag (updated on last ``load_sheet``). Serials not rewritten."""
        return self._reader.date1904

    def workbook_props(self) -> dict | None:
        """Workbook props (core/app/workbookPr) from last load that requested them."""
        return self._reader.workbook_props()

    def persons(self) -> list[dict] | None:
        """Threaded-comment persons from last load that requested comments/``all``."""
        return self._reader.persons()

    @property
    def has_vba(self) -> bool:
        """Whether a VBA project is present (after a load with ``vba`` / ``all``)."""
        return self._reader.has_vba

    def vba_project(self) -> bytes | None:
        """Raw ``vbaProject.bin`` bytes; None if VBA not requested or absent."""
        return self._reader.vba_project()

    def __repr__(self) -> str:
        return self._reader.__repr__()


def read_excel_turbo(path: Path | str, password: str | None = None) -> TurboReader:
    """Open an XLSX file for turbo reading.

    Only sheet names are read up front; call :meth:`TurboReader.load_sheet` for data.

    :param path: Path to an ``.xlsx`` file.
    :param password: Password for an ECMA-376 encrypted workbook; a plain
        workbook ignores it. ``None`` (the default) fails an encrypted workbook
        with a clear "password required" error.
    """
    if isinstance(path, Path):
        path = expanduser(str(path))
    elif isinstance(path, str):
        path = expanduser(path)
    return TurboReader(_read_excel_turbo(path, password))


def is_encrypted(path: Path | str) -> bool:
    """True if the file is an ECMA-376 encrypted workbook (OLE/CFB with an
    ``EncryptionInfo`` stream). Needs no password and never raises."""
    if isinstance(path, Path):
        path = expanduser(str(path))
    return _is_encrypted(str(path))


def encryption_info(path: Path | str) -> dict:
    """Report an encrypted workbook's scheme, algorithm and spin count WITHOUT
    a password: ``{"scheme", "cipher_algorithm", "hash_algorithm", "key_bits",
    "block_size", "salt_size", "spin_count", "message"}``.

    Raises ``KyraxError`` when the file is not an encrypted workbook. Requires
    a build with the ``encryption`` feature.
    """
    if isinstance(path, Path):
        path = expanduser(str(path))
    if _encryption_info is None:
        raise KyraxError("encryption_info requires a kyrax build with the `encryption` feature")
    return _encryption_info(str(path))


def _validate_sheets(sheets: list[dict]) -> None:
    for i, sheet in enumerate(sheets):
        cols = sheet.get("columns")
        if isinstance(cols, list):
            for j, col in enumerate(cols):
                if isinstance(col, str):
                    name = sheet.get("name", i)
                    raise TypeError(
                        f"sheet '{name}': columns[{j}] is a str ('{col}'). "
                        "The 'columns' key takes columnar DATA (a list of column arrays), "
                        "not header names. To write a header row, pass it as the first "
                        "entry of 'rows', e.g. rows=[['Region','Profit'], ...]."
                    )


def write_excel_turbo(
    path: Path | str,
    sheets: list[dict],
    *,
    string_mode: Literal["inline", "sst", "auto"] = "inline",
    emit_cached_values: bool = True,
    date1904: bool = False,
    features: list[str] | str | None = None,
    active_tab: int = 0,
    named_styles: list[dict] | None = None,
    props: dict | None = None,
    defined_names: list[dict] | None = None,
    chartsheets: list[dict] | None = None,
    lock_structure: bool = False,
    external_links: list | None = None,
    creator: str | None = None,
    macro_enabled: bool = False,
    recalculate: bool = False,
) -> None:
    """Write an XLSX workbook via the turbo write path (silo A+B+C).

    Each sheet dict supports core/styles fields (see W1/W2) plus structural:
      - ``merges``, ``hyperlinks``, ``comments``, ``tables``, ``charts``
      - ``auto_filter``, ``protection``, ``scenarios``, print stack
      - ``tab_color``, ``print_area``, ``print_titles``, row/col breaks

    Workbook kwargs: ``props``, ``defined_names``, ``chartsheets``,
    ``lock_structure``, ``external_links``, ``macro_enabled``.

    :param features: ``core`` | ``all`` | ``styles`` or list of feature names
        (``merges``, ``hyperlinks``, ``comments``, ``tables``, ``charts``,
        ``defined_names``, ``props``, …). Content auto-enables flags.
    :param recalculate: compute every formula cell in Rust and write the result
        as its cached value, so the saved file carries formula **and** value.
        Formulas kyrax cannot compute exactly (unsupported function, external
        reference, circular reference) are left uncomputed and the workbook
        keeps ``calcPr fullCalcOnLoad="1"`` so Excel fills them on open — a
        wrong number is never written.
    """
    if isinstance(path, Path):
        path = expanduser(str(path))
    elif isinstance(path, str):
        path = expanduser(path)
    _write_excel_turbo(
        path,
        sheets,
        string_mode=string_mode,
        emit_cached_values=emit_cached_values,
        date1904=date1904,
        features=features,
        active_tab=active_tab,
        named_styles=named_styles,
        props=props,
        defined_names=defined_names,
        chartsheets=chartsheets,
        lock_structure=lock_structure,
        external_links=external_links,
        creator=creator,
        macro_enabled=macro_enabled,
        recalculate=recalculate,
    )


def write_excel_turbo_stream(
    path: Path | str,
    sheets: list[dict],
    *,
    string_mode: Literal["inline", "sst", "auto"] = "inline",
    emit_cached_values: bool = True,
    date1904: bool = False,
    features: list[str] | str | None = None,
    active_tab: int = 0,
    named_styles: list[dict] | None = None,
    props: dict | None = None,
    defined_names: list[dict] | None = None,
    chartsheets: list[dict] | None = None,
    lock_structure: bool = False,
    external_links: list | None = None,
    creator: str | None = None,
    macro_enabled: bool = False,
    recalculate: bool = False,
) -> None:
    """Stream an XLSX workbook straight to disk (openpyxl ``write_only`` analogue).

    Same options and sheet-dict schema as :func:`write_excel_turbo`, but sheet
    XML is deflated and flushed incrementally instead of being buffered whole,
    so writer-side memory stays bounded regardless of row count. Emits Zip64
    records when the archive crosses the 4 GB / 65535-entry limits.

    This path is deliberately serial; :func:`write_excel_turbo` remains the
    default and keeps its parallel multi-sheet compression.
    """
    if isinstance(path, Path):
        path = expanduser(str(path))
    elif isinstance(path, str):
        path = expanduser(path)
    _write_excel_turbo_stream(
        path,
        sheets,
        string_mode=string_mode,
        emit_cached_values=emit_cached_values,
        date1904=date1904,
        features=features,
        active_tab=active_tab,
        named_styles=named_styles,
        props=props,
        defined_names=defined_names,
        chartsheets=chartsheets,
        lock_structure=lock_structure,
        external_links=external_links,
        creator=creator,
        macro_enabled=macro_enabled,
        recalculate=recalculate,
    )


def write_excel_turbo_bytes(
    sheets: list[dict],
    *,
    string_mode: Literal["inline", "sst", "auto"] = "inline",
    emit_cached_values: bool = True,
    date1904: bool = False,
    features: list[str] | str | None = None,
    active_tab: int = 0,
    named_styles: list[dict] | None = None,
    props: dict | None = None,
    defined_names: list[dict] | None = None,
    chartsheets: list[dict] | None = None,
    lock_structure: bool = False,
    external_links: list | None = None,
    creator: str | None = None,
    macro_enabled: bool = False,
    recalculate: bool = False,
) -> bytes:
    """Write an XLSX workbook and return bytes (same options as write_excel_turbo)."""
    return _write_excel_turbo_bytes(
        sheets,
        string_mode=string_mode,
        emit_cached_values=emit_cached_values,
        date1904=date1904,
        features=features,
        active_tab=active_tab,
        named_styles=named_styles,
        props=props,
        defined_names=defined_names,
        chartsheets=chartsheets,
        lock_structure=lock_structure,
        external_links=external_links,
        creator=creator,
        macro_enabled=macro_enabled,
        recalculate=recalculate,
    )


__all__ = (
    # version
    "__version__",
    # standalone formula API (lazy; imported on first access)
    "formulas",
    # csv/json interchange (lazy; imported on first access)
    "io",
    # main entrypoint
    "read_excel",
    "read_excel_turbo",
    "write_excel_turbo",
    "write_excel_turbo_bytes",
    "write_excel_turbo_stream",
    # validate & repair
    "validate_excel",
    "repair_excel",
    # encrypted-workbook support
    "is_encrypted",
    "encryption_info",
    # editable round-trip API
    "load_workbook",
    "edit_excel",
    "EditableWorkbook",
    "EditableSheet",
    # Python types
    "DType",
    "DTypeMap",
    # Excel reader
    "ExcelReader",
    # Excel sheet
    "ExcelSheet",
    # Excel table
    "ExcelTable",
    # Turbo path
    "TurboReader",
    "TurboSheet",
    # Column metadata
    "DTypeFrom",
    "ColumnNameFrom",
    "ColumnInfo",
    # Defined names
    "DefinedName",
    # Parse error information
    "CellError",
    "CellErrors",
    # Exceptions
    "KyraxError",
    "CannotRetrieveCellDataError",
    "CalamineCellError",
    "CalamineError",
    "SheetNotFoundError",
    "ColumnNotFoundError",
    "ArrowError",
    "InvalidParametersError",
    "UnsupportedColumnTypeCombinationError",
)


def __getattr__(name: str):
    # Lazy submodules: `from kyrax import formulas` (and `kyrax.formulas`)
    # import on first use, so the `formulas` facade costs nothing when unused.
    # `importlib.import_module` (not `from . import formulas`) avoids the
    # parent-attribute lookup that would re-enter this hook and recurse.
    if name == "formulas":
        import importlib

        return importlib.import_module(f"{__name__}.formulas")
    if name == "io":
        import importlib

        return importlib.import_module(f"{__name__}.io")
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")
