from __future__ import annotations

import typing
from collections.abc import Callable
from typing import TYPE_CHECKING, Literal

if TYPE_CHECKING:
    import pyarrow as pa

DType = Literal["null", "int", "float", "string", "boolean", "datetime", "date", "duration"]
DTypeMap = dict[str | int, DType]
ColumnNameFrom = Literal["provided", "looked_up", "generated"]
DTypeFrom = Literal["provided_for_all", "provided_by_index", "provided_by_name", "guessed"]
SheetVisible = Literal["visible", "hidden", "veryhidden"]

class ColumnInfoNoDtype:
    def __init__(
        self,
        *,
        name: str,
        index: int,
        absolute_index: int,
        column_name_from: ColumnNameFrom,
    ) -> None: ...
    @property
    def name(self) -> str: ...
    @property
    def index(self) -> int: ...
    @property
    def absolute_index(self) -> int: ...
    @property
    def column_name_from(self) -> ColumnNameFrom: ...

class ColumnInfo:
    def __init__(
        self,
        *,
        name: str,
        index: int,
        absolute_index: int,
        column_name_from: ColumnNameFrom,
        dtype: DType,
        dtype_from: DTypeFrom,
    ) -> None: ...
    @property
    def name(self) -> str: ...
    @property
    def index(self) -> int: ...
    @property
    def absolute_index(self) -> int: ...
    @property
    def dtype(self) -> DType: ...
    @property
    def column_name_from(self) -> ColumnNameFrom: ...
    @property
    def dtype_from(self) -> DTypeFrom: ...

class DefinedName:
    def __init__(
        self,
        *,
        name: str,
        formula: str,
    ) -> None: ...
    @property
    def name(self) -> str: ...
    @property
    def formula(self) -> str: ...

class CellError:
    @property
    def position(self) -> tuple[int, int]: ...
    @property
    def row_offset(self) -> int: ...
    @property
    def offset_position(self) -> tuple[int, int]: ...
    @property
    def detail(self) -> str: ...
    def __repr__(self) -> str: ...

class CellErrors:
    @property
    def errors(self) -> list[CellError]: ...
    def __repr__(self) -> str: ...

class _ExcelSheet:
    @property
    def name(self) -> str:
        """The name of the sheet"""
    @property
    def width(self) -> int:
        """The sheet's width"""
    @property
    def height(self) -> int:
        """The sheet's height"""
    @property
    def total_height(self) -> int:
        """The sheet's total height"""
    @property
    def offset(self) -> int:
        """The sheet's offset before data starts"""
    @property
    def selected_columns(self) -> list[ColumnInfo]:
        """The sheet's selected columns"""
    def available_columns(self) -> list[ColumnInfo]:
        """The columns available for the given sheet"""
    @property
    def specified_dtypes(self) -> DTypeMap | None:
        """The dtypes specified for the sheet"""
    @property
    def visible(self) -> SheetVisible:
        """The visibility of the sheet"""
    def to_arrow(self) -> pa.RecordBatch:
        """Converts the sheet to a pyarrow `RecordBatch`

        Requires the `pyarrow` extra to be installed.
        """
    def to_arrow_with_errors(self) -> tuple[pa.RecordBatch, CellErrors]:
        """Converts the sheet to a pyarrow `RecordBatch` with error information.

        Stores the positions of any values that cannot be parsed as the specified type and were
        therefore converted to None.

        Requires the `pyarrow` extra to be installed.
        """
    def __arrow_c_schema__(self) -> object:
        """Export the schema as an `ArrowSchema` `PyCapsule`.

        https://arrow.apache.org/docs/format/CDataInterface/PyCapsuleInterface.html#arrowschema-export

        The Arrow PyCapsule Interface enables zero-copy data exchange with
        Arrow-compatible libraries without requiring PyArrow as a dependency.
        """
    def __arrow_c_array__(self, requested_schema: object = None) -> tuple[object, object]:
        """Export the schema and data as a pair of `ArrowSchema` and `ArrowArray` `PyCapsules`.

        The optional `requested_schema` parameter allows for potential schema conversion.

        https://arrow.apache.org/docs/format/CDataInterface/PyCapsuleInterface.html#arrowarray-export

        The Arrow PyCapsule Interface enables zero-copy data exchange with
        Arrow-compatible libraries without requiring PyArrow as a dependency.
        """

class _ExcelTable:
    @property
    def name(self) -> str:
        """The name of the table"""
    @property
    def sheet_name(self) -> str:
        """The name of the sheet this table belongs to"""
    @property
    def width(self) -> int:
        """The table's width"""
    @property
    def height(self) -> int:
        """The table's height"""
    @property
    def total_height(self) -> int:
        """The table's total height"""
    @property
    def offset(self) -> int:
        """The table's offset before data starts"""
    @property
    def selected_columns(self) -> list[ColumnInfo]:
        """The table's selected columns"""
    def available_columns(self) -> list[ColumnInfo]:
        """The columns available for the given table"""
    @property
    def specified_dtypes(self) -> DTypeMap | None:
        """The dtypes specified for the table"""
    def to_arrow(self) -> pa.RecordBatch:
        """Converts the table to a pyarrow `RecordBatch`

        Requires the `pyarrow` extra to be installed.
        """
    def __arrow_c_schema__(self) -> object:
        """Export the schema as an `ArrowSchema` `PyCapsule`.

        https://arrow.apache.org/docs/format/CDataInterface/PyCapsuleInterface.html#arrowschema-export

        The Arrow PyCapsule Interface enables zero-copy data exchange with
        Arrow-compatible libraries without requiring PyArrow as a dependency.
        """

    def __arrow_c_array__(self, requested_schema: object = None) -> tuple[object, object]:
        """Export the schema and data as a pair of `ArrowSchema` and `ArrowArray` `PyCapsules`.

        The optional `requested_schema` parameter allows for potential schema conversion.

        https://arrow.apache.org/docs/format/CDataInterface/PyCapsuleInterface.html#arrowarray-export

        The Arrow PyCapsule Interface enables zero-copy data exchange with
        Arrow-compatible libraries without requiring PyArrow as a dependency.
        """

class _ExcelReader:
    """A class representing an open Excel file and allowing to read its sheets"""

    @typing.overload
    def load_sheet(
        self,
        idx_or_name: str | int,
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
    ) -> _ExcelSheet: ...
    @typing.overload
    def load_sheet(
        self,
        idx_or_name: str | int,
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
    @typing.overload
    def load_sheet(
        self,
        idx_or_name: str | int,
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
    ) -> pa.RecordBatch: ...
    @typing.overload
    def load_table(
        self,
        name: str,
        *,
        header_row: int | None = None,
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
    ) -> _ExcelTable: ...
    @typing.overload
    def load_table(
        self,
        name: str,
        *,
        header_row: int | None = None,
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
    @property
    def sheet_names(self) -> list[str]: ...
    def table_names(self, sheet_name: str | None = None) -> list[str]: ...
    def defined_names(self) -> list[DefinedName]: ...

def read_excel(source: str | bytes) -> _ExcelReader:
    """Reads an excel file and returns an ExcelReader"""

def read_excel_turbo(path: str, password: str | None = None) -> _TurboReader:
    """Open an XLSX for turbo reading; `password` opens an encrypted workbook"""

def write_excel_turbo(
    path: str,
    sheets: list,
    *,
    string_mode: str = "inline",
    emit_cached_values: bool = True,
    date1904: bool = False,
    features: list[str] | str | None = None,
    active_tab: int = 0,
    named_styles: list | None = None,
    props: dict | None = None,
    defined_names: list | None = None,
    chartsheets: list | None = None,
    lock_structure: bool = False,
    external_links: list | None = None,
    creator: str | None = None,
    macro_enabled: bool = False,
    recalculate: bool = False,
) -> None:
    """Write an XLSX via the turbo write path (core + styles + structural).

    Each sheet dict may carry a ``pivots`` list to author pivot tables::

        {"name": "PivotTable1", "source_range": "A1:C5",
         "rows": ["Region"], "cols": ["Product"],
         "data": [{"field": "Amount", "agg": "sum"}], "target_cell": "E3"}

    ``rows``/``cols`` fields are header names or 0-based column indices;
    ``data`` entries pair a field with ``sum``/``count``/``average``/``max``/
    ``min``/``product``/``stdDev``/``stdDevp``/``var``/``varp``.
    """

def is_encrypted(path: str) -> bool:
    """Detect an ECMA-376 encrypted workbook without a password"""

def encryption_info(path: str) -> dict:
    """Report an encrypted workbook's scheme, algorithm and spin count (no password)"""

def write_excel_turbo_bytes(
    sheets: list,
    *,
    string_mode: str = "inline",
    emit_cached_values: bool = True,
    date1904: bool = False,
    features: list[str] | str | None = None,
    active_tab: int = 0,
    named_styles: list | None = None,
    props: dict | None = None,
    defined_names: list | None = None,
    chartsheets: list | None = None,
    lock_structure: bool = False,
    external_links: list | None = None,
    creator: str | None = None,
    macro_enabled: bool = False,
    recalculate: bool = False,
) -> bytes:
    """Write an XLSX and return bytes (sheet dicts support the same ``pivots``
    key as :func:`write_excel_turbo`)."""

class _TurboReader:
    @property
    def sheet_names(self) -> list[str]: ...
    def load_sheet(
        self, idx_or_name: int | str, *, features: list[str] | str | None = None
    ) -> _TurboSheet: ...
    def defined_names(self) -> list[dict] | None: ...
    def tables(self) -> list[dict] | None: ...
    @property
    def date1904(self) -> bool: ...
    def workbook_props(self) -> dict | None: ...
    def persons(self) -> list[dict] | None: ...
    @property
    def has_vba(self) -> bool: ...
    def vba_project(self) -> bytes | None: ...

class _TurboSheet: ...

class EditableSheet:
    """A live sheet handle on an :class:`EditableWorkbook`.

    All indices are 1-BASED, matching openpyxl and Excel. ``insert_rows(2)``
    puts a new blank row AT row 2 and pushes every existing row at or below
    row 2 down. Operations are recorded and applied at ``save()`` time.

    ``ws["A1"]`` reads the effective value at cell A1: a direct edit (via
    ``set_cell`` or a range assignment) shadows the original workbook XML
    immediately; otherwise the original cell value is returned. Empty cells
    read as ``None``. Numbers and dates read as ``float``, booleans as
    ``bool``, strings/errors as ``str``, formulas as their formula text with
    one leading ``=``, and rich text as its flattened text.

    ``ws["A1:B2"]`` reads a rectangular range as a row-major
    ``list[list[scalar]]``; ``ws["A1:B2"] = [[..], [..]]`` writes one where the
    value must be a 2D list/tuple of exactly the range's dimensions (converted
    before any edit is recorded, so a bad value leaves no partial writes).

    Queued ``insert_rows``/``delete_rows``/``insert_cols``/``delete_cols``/
    ``move_range`` operations materialize at ``save()`` time and are not
    reflected by reads until then.
    """

    def __getitem__(self, key: str) -> object | list[list[object]]:
        """Read a cell (``"A1"`` → scalar) or a range (``"A1:B2"`` → 2D list).

        Raises ``ValueError`` when ``key`` is not a valid A1 cell or range
        (malformed syntax, zero, or out of the 1..1_048_576 row / A..XFD
        column grid).
        """
    def __setitem__(self, key: str, value: object) -> None:
        """Set a cell (``"A1"``) or a rectangular range (``"A1:B2"``).

        Range values must be 2D lists/tuples of the exact range dimensions;
        otherwise ``TypeError``. Invalid ``key`` raises ``ValueError``. All
        values are validated and converted before any edit is recorded, so a
        bad element never leaves a partial write.
        """
    def set_cell(self, row: int, col: int, value: object) -> None:
        """Set the value of cell ``(row, col)`` (1-based).

        Raises ``ValueError`` when ``row``/``col`` is zero or out of the
        1..1_048_576 row / 1..16_384 column grid.
        """
    def set_cell_style(
        self,
        row: int,
        col: int,
        *,
        font: dict | None = None,
        fill: dict | None = None,
        border: dict | None = None,
        num_fmt: str | None = None,
    ) -> None:
        """Set a style on cell ``(row, col)`` (1-based).

        Raises ``ValueError`` when ``row``/``col`` is zero or out of grid.
        """
    def insert_rows(self, idx: int, amount: int = 1) -> None:
        """Insert ``amount`` blank rows at 1-based ``idx``.

        Rows at or below ``idx`` shift down. Raises ``InvalidParametersError``
        when ``idx < 1``, and at ``save()`` time when the operation would
        corrupt the sheet (an implicit-numbered row/cell at or below the shift
        point, a grid limit of 1,048,576 rows would be exceeded, or a
        shared-formula master would be orphaned).
        """
    def delete_rows(self, idx: int, amount: int = 1) -> None:
        """Delete ``amount`` rows starting at 1-based ``idx``.

        Rows below shift up. Raises ``InvalidParametersError`` when ``idx < 1``,
        and at ``save()`` time when the operation would corrupt the sheet (an
        implicit-numbered row/cell at or below the shift point, or a shared
        formula's master would be removed while a dependent survives).
        """
    def insert_cols(self, idx: int, amount: int = 1) -> None:
        """Insert ``amount`` blank columns at 1-based ``idx``.

        Columns at or right of ``idx`` shift right. Raises ``InvalidParametersError``
        when ``idx < 1``, and at ``save()`` time when the operation would corrupt
        the sheet (an implicit-numbered cell at or right of the shift point, or
        the 16,384-column grid limit would be exceeded).
        """
    def delete_cols(self, idx: int, amount: int = 1) -> None:
        """Delete ``amount`` columns starting at 1-based ``idx``.

        Columns to the right shift left. Raises ``InvalidParametersError`` when
        ``idx < 1``, and at ``save()`` time when the operation would corrupt the
        sheet (an implicit-numbered cell at or right of the shift point).
        """
    def move_range(
        self,
        range_string: str,
        rows: int = 0,
        cols: int = 0,
        translate: bool = False,
    ) -> None:
        """Move a range of cells by ``rows`` and ``cols`` (positive is
        down/right, negative is up/left).

        Every cell in the range is relocated; the vacated source cells become
        empty and destination cells are overwritten. Nothing else on the sheet
        shifts. With ``translate=True`` the formulas *inside* the moved range
        have their references translated by the same offset (openpyxl
        ``move_range`` semantics; default ``False`` leaves them alone).

        Merged ranges, hyperlinks, data validations and conditional formatting
        anchors fully contained in the moved range follow it; anchors that
        straddle the boundary stay put. Formulas *outside* the range that point
        into it are **not** rewritten.

        Raises ``ValueError`` immediately when ``range_string`` is malformed or
        out of the 1..1_048_576 row / A..XFD column grid, and
        ``InvalidParametersError`` at ``save()`` time when the move would push
        any cell off the grid (1,048,576 rows / 16,384 columns), an
        implicit-numbered row/cell lies inside the moved region, or a
        shared-formula ``ref=`` would leave the grid — nothing is written.
        """

class EditableWorkbook:
    """A byte-preserving edit handle over an existing XLSX (from ``edit_excel``).

    Cell edits, styles, and row/column insert-delete operations are recorded
    against a sparse overlay and applied together at ``save()``. Row/column
    shifts are applied BEFORE cell edits, so an edit coordinate is final while
    a shift moves the grid under it.
    """

    def __getitem__(self, sheet_name: str) -> EditableSheet:
        """Return a live handle to the named sheet."""
    def save(self, path: str) -> None:
        """Write the edited workbook to ``path``.

        ALL-OR-NOTHING: if any recorded operation is refused (a table header
        row would be deleted, a shared-formula master orphaned, an
        implicit-numbered row/cell hit, or a grid limit exceeded), an
        ``InvalidParametersError`` is raised and NOTHING is written — the
        destination file is left untouched.
        """

def edit_excel(path: str) -> EditableWorkbook:
    """Open an existing XLSX for byte-preserving edits.

    Prefer the friendlier :func:`load_workbook` (``edit_mode=True``) wrapper.
    """

def validate_excel(path: str) -> dict:
    """Validate a workbook; returns a report dict, never raises for bad input."""

def repair_excel(path: str, out_path: str, severity: str = "warning") -> dict:
    """Repair a workbook into out_path; returns {wrote_output, report, actions}."""

# --- standalone formula API (backing `kyrax.formulas`) ---

def evaluate(formula: str, context: dict[str, object] | None = None) -> object:
    """Evaluate one formula against an optional A1->value cell context.

    Returns float / str / bool / error-code str, None for blank, or a nested
    list for an array result. The GIL is released during evaluation.
    """

def list_functions() -> list[tuple[str, str]]:
    """Every registered worksheet function as (name, category) pairs."""

def dependencies(formula: str) -> list[str]:
    """The cell references the formula reads, as sorted A1 strings."""

def recalculate(sheets: list) -> bytes:
    """Recalculate a workbook (same sheet-dict schema as write_excel_turbo)
    and return it as XLSX bytes with computed formula caches."""

__version__: str

# Exceptions
class KyraxError(Exception): ...
class UnsupportedColumnTypeCombinationError(KyraxError): ...
class CannotRetrieveCellDataError(KyraxError): ...
class CalamineCellError(KyraxError): ...
class CalamineError(KyraxError): ...
class SheetNotFoundError(KyraxError): ...
class ColumnNotFoundError(KyraxError): ...
class ArrowError(KyraxError): ...
class InvalidParametersError(KyraxError): ...
