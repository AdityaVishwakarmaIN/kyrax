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

class CellError:
    """Represents an error encountered while parsing a single cell value."""
    @property
    def position(self) -> tuple[int, int]:
        """(row, col) coordinates in the raw sheet grid (including header rows)."""
    @property
    def offset_position(self) -> tuple[int, int]:
        """(row, col) coordinates in the exported RecordBatch (header stripped)."""
    @property
    def row_offset(self) -> int:
        """Row offset subtracted from raw row to get RecordBatch row."""
    @property
    def detail(self) -> str:
        """Error message or Excel error code."""

class CellErrors:
    """Collection of cell parsing errors."""
    @property
    def errors(self) -> list[CellError]:
        """List of all cell errors."""

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
    vba_archive_path: str | None = None,
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

def write_excel_turbo_stream(
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
    vba_archive_path: str | None = None,
) -> None:
    """Write an XLSX streaming to disk (openpyxl ``write_only`` analogue);
    same options and sheet-dict schema as :func:`write_excel_turbo`."""

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
    vba_archive_path: str | None = None,
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
    def __repr__(self) -> str: ...

class _TurboSheet:
    """One worksheet loaded via the turbo fast path.

    Values come from :meth:`to_arrow`; the other methods expose the features
    requested through ``features=`` on :meth:`_TurboReader.load_sheet`
    (``None`` when a feature was not requested).
    """

    @property
    def name(self) -> str: ...
    @property
    def nrows(self) -> int: ...
    @property
    def ncols(self) -> int: ...
    @property
    def column_names(self) -> list[str]: ...
    def to_arrow(self) -> pa.RecordBatch:
        """Values as a pyarrow `RecordBatch` (dictionary-encoded strings OK).

        Cached formula results are present in value columns (both-not-XOR with
        formula text). Typed cell errors surface via :meth:`cell_errors`.

        Requires the `pyarrow` extra to be installed.
        """

    def to_arrow_with_errors(self) -> tuple[pa.RecordBatch, CellErrors]:
        """Values and errors as (values: RecordBatch, errors: CellErrors).
        Requires the `pyarrow` extra to be installed.
        """

    def style_indices(self) -> list[list[int]] | None:
        """Per-column uint32 xf indices (length=ncols, each list length=nrows).

        Returns `None` if styles were not requested.
        """

    def style_table(self) -> list[dict] | None:
        """Resolved cellXfs (one dict per xf). `None` if styles not loaded."""

    def formulas(self) -> pa.RecordBatch | None:
        """Sparse formulas with shared text translated as a `RecordBatch`.

        Columns: ``row``, ``col`` (uint32, 0-based data indices, header
        excluded), ``kind`` (plain|shared|array|dataTable), ``text`` (utf8,
        shared translated), ``ref`` (utf8, null unless array). `None` if not
        requested. Requires the `pyarrow` extra.
        """

    def cell_errors(self) -> pa.RecordBatch:
        """Sparse typed error caches from ``t="e"`` cells as a `RecordBatch`.

        Columns: ``row``, ``col`` (uint32, 0-based data indices, header
        excluded), ``code`` (utf8). Always available (empty batch when the
        sheet has no error cells). Requires the `pyarrow` extra.
        """

    def merges(self) -> list[str] | None:
        """Merged ranges as A1 strings (``"A1:B2"``). `None` if not requested."""

    def hyperlinks(self) -> list[dict] | None:
        """Hyperlinks as dicts. `None` if not requested."""

    def comments(self) -> pa.RecordBatch | None:
        """Comments as a `RecordBatch` (ref, author, text). `None` if not
        requested. Requires the `pyarrow` extra.
        """

    def comment_authors(self) -> list[str] | None:
        """Comment author list (when comments were requested); else `None`."""

    @property
    def legacy_is_mirror(self) -> bool:
        """True when legacy comments are Excel mirrors of threaded comments."""

    def threaded_comments(self) -> list[dict] | None:
        """Threaded comments (Office 2018+). `None` if comments not requested."""

    def charts(self) -> list[dict] | None:
        """Chart metadata on this sheet. `None` if charts not requested."""

    def images(self) -> list[dict] | None:
        """Images on this sheet (``data`` bytes + ``anchor`` dict).
        `None` if images not requested.
        """

    def pivots(self) -> list[dict] | None:
        """Pivot table metadata on this sheet. `None` if pivots not requested."""

    def tables(self) -> list[dict] | None:
        """Tables on this sheet. `None` if tables not requested."""

    @property
    def sheet_state(self) -> str:
        """Sheet visibility: ``visible`` | ``hidden`` | ``veryHidden``."""

    @property
    def sheet_kind(self) -> str:
        """``worksheet`` or ``chartsheet``."""

    def row_dimensions(self) -> dict | None:
        """Explicitly-set row dimensions ``{row: {height, hidden, ...}}`` (1-based keys)."""

    def column_dimensions(self) -> list[dict] | None:
        """Column dimension records (min/max/width/hidden/...)."""

    def sheet_format(self) -> dict | None:
        """Default row/col format props from ``sheetFormatPr``."""

    def auto_filter(self) -> dict | None:
        """AutoFilter ref + filter columns, or `None`."""

    def sheet_view(self) -> dict | None:
        """Active sheetView props + optional pane."""

    def freeze_panes(self) -> str | None:
        """Freeze panes top-left cell A1 (when frozen), else `None`."""

    def protection(self) -> dict | None:
        """Sheet protection flags + password hash fields."""

    def page_setup(self) -> dict | None:
        """Print page setup (orientation, paper size, scale, fit...)."""

    def page_margins(self) -> dict | None:
        """Page margins in inches."""

    def print_options(self) -> dict | None:
        """Print options (centered, headings, grid lines)."""

    def header_footer(self) -> dict | None:
        """Header/footer raw strings (unescaped, with ``&L``/``&C``/``&R``)."""

    @property
    def code_name(self) -> str | None:
        """VBA sheet code name from ``sheetPr``."""

    @property
    def tab_color(self) -> str | None:
        """Tab color (rgb hex or ``theme:N``)."""

    def data_validations(self) -> list[dict] | None:
        """Data validation records (sheet-level). `None` if not requested."""

    def conditional_formatting(self) -> list[dict] | None:
        """Flat CF rule list with resolved dxf when available."""

    def named_styles(self) -> list[dict] | None:
        """Named styles from styles.xml (when styles loaded)."""

    def __repr__(self) -> str: ...

def read_excel_turbo_iter(
    path: str,
    sheet_idx: int = 0,
    chunk_size: int = 10000,
) -> SheetStream:
    """Stream batches of rows from a worksheet as pyarrow RecordBatches."""

class SheetStream:
    """Iterator yielding RecordBatch chunks incrementally from an XLSX sheet."""
    def __iter__(self) -> SheetStream: ...
    def __next__(self) -> pa.RecordBatch: ...
    def close(self) -> None: ...
    @property
    def closed(self) -> bool: ...

class Font:
    def __init__(
        self,
        name: str | None = None,
        size: float | None = None,
        sz: float | None = None,
        bold: bool | None = None,
        b: bool | None = None,
        italic: bool | None = None,
        i: bool | None = None,
        strike: bool | None = None,
        underline: str | None = None,
        u: str | None = None,
        color: str | dict | None = None,
    ) -> None: ...
    name: str | None
    size: float | None
    sz: float | None
    bold: bool | None
    b: bool | None
    italic: bool | None
    i: bool | None
    strike: bool | None
    underline: str | None
    u: str | None
    color: str | None

class PatternFill:
    def __init__(
        self,
        fill_type: str | None = None,
        patternType: str | None = None,
        start_color: str | dict | None = None,
        end_color: str | dict | None = None,
        fgColor: str | dict | None = None,
        bgColor: str | dict | None = None,
    ) -> None: ...
    fill_type: str | None
    patternType: str | None
    start_color: str | None
    end_color: str | None
    fgColor: str | None
    bgColor: str | None

class Side:
    def __init__(
        self,
        style: str | None = None,
        border_style: str | None = None,
        color: str | dict | None = None,
    ) -> None: ...
    style: str | None
    border_style: str | None
    color: str | None

class Border:
    def __init__(
        self,
        left: Side | None = None,
        right: Side | None = None,
        top: Side | None = None,
        bottom: Side | None = None,
        diagonal: Side | None = None,
        diagonal_up: bool = False,
        diagonal_down: bool = False,
        diagonalUp: bool | None = None,
        diagonalDown: bool | None = None,
        outline: bool = True,
    ) -> None: ...
    left: Side | None
    right: Side | None
    top: Side | None
    bottom: Side | None
    diagonal: Side | None
    diagonal_up: bool
    diagonal_down: bool
    outline: bool

class Alignment:
    def __init__(
        self,
        horizontal: str | None = None,
        vertical: str | None = None,
        text_rotation: int = 0,
        textRotation: int | None = None,
        wrap_text: bool | None = None,
        wrapText: bool | None = None,
        shrink_to_fit: bool | None = None,
        shrinkToFit: bool | None = None,
        indent: int = 0,
        relative_indent: int = 0,
        relativeIndent: int | None = None,
        justify_last_line: bool | None = None,
        justifyLastLine: bool | None = None,
        reading_order: int = 0,
        readingOrder: int | None = None,
    ) -> None: ...
    horizontal: str | None
    vertical: str | None
    text_rotation: int
    wrap_text: bool | None
    shrink_to_fit: bool | None
    indent: int

class Protection:
    def __init__(self, locked: bool = True, hidden: bool = False) -> None: ...
    locked: bool
    hidden: bool

class Comment:
    def __init__(self, text: str = "", author: str = "") -> None: ...
    text: str
    author: str
    content: str

class Cell:
    """A lazy proxy cell handle on an :class:`EditableSheet`."""
    @property
    def row(self) -> int: ...
    @property
    def column(self) -> int: ...
    @property
    def coordinate(self) -> str: ...
    @property
    def value(self) -> object: ...
    @value.setter
    def value(self, val: object) -> None: ...
    @property
    def font(self) -> Font: ...
    @font.setter
    def font(self, font: Font | dict) -> None: ...
    @property
    def fill(self) -> PatternFill: ...
    @fill.setter
    def fill(self, fill: PatternFill | dict) -> None: ...
    @property
    def border(self) -> Border: ...
    @border.setter
    def border(self, border: Border | dict) -> None: ...
    @property
    def alignment(self) -> Alignment: ...
    @alignment.setter
    def alignment(self, alignment: Alignment | dict) -> None: ...
    @property
    def protection(self) -> Protection: ...
    @protection.setter
    def protection(self, protection: Protection | dict) -> None: ...
    @property
    def style(self) -> str | None: ...
    @style.setter
    def style(self, name: str | None) -> None: ...
    @property
    def number_format(self) -> str | None: ...
    @number_format.setter
    def number_format(self, fmt: str | None) -> None: ...
    @property
    def hyperlink(self) -> str | None: ...
    @hyperlink.setter
    def hyperlink(self, link: str | None) -> None: ...
    @property
    def comment(self) -> str | None: ...
    @comment.setter
    def comment(self, comment: str | Comment | None) -> None: ...
    def offset(self, row: int = 0, column: int = 0) -> Cell: ...

class EditableSheet:
    """A live sheet handle on an :class:`EditableWorkbook`."""

    @property
    def title(self) -> str: ...
    @title.setter
    def title(self, val: str) -> None: ...
    @property
    def min_row(self) -> int: ...
    @property
    def max_row(self) -> int: ...
    @property
    def min_column(self) -> int: ...
    @property
    def max_column(self) -> int: ...
    @property
    def dimensions(self) -> str: ...
    @property
    def freeze_panes(self) -> str | None: ...
    @freeze_panes.setter
    def freeze_panes(self, val: str | None) -> None: ...
    @property
    def tab_color(self) -> str | None: ...
    @tab_color.setter
    def tab_color(self, val: str | None) -> None: ...
    @property
    def auto_filter(self) -> str | None: ...
    @auto_filter.setter
    def auto_filter(self, val: str | None) -> None: ...
    @property
    def values(self) -> typing.Iterator[tuple[object, ...]]: ...
    def append(self, iterable: typing.Iterable[object]) -> None: ...
    def cell(self, row: int, column: int, value: object | None = None) -> Cell: ...
    def merge_cells(
        self,
        range_string: str | None = None,
        start_row: int | None = None,
        start_column: int | None = None,
        end_row: int | None = None,
        end_column: int | None = None,
    ) -> None: ...
    def unmerge_cells(
        self,
        range_string: str | None = None,
        start_row: int | None = None,
        start_column: int | None = None,
        end_row: int | None = None,
        end_column: int | None = None,
    ) -> None: ...
    def iter_rows(
        self,
        min_row: int | None = None,
        max_row: int | None = None,
        min_col: int | None = None,
        max_col: int | None = None,
        values_only: bool = False,
    ) -> typing.Iterator[tuple[object, ...]]: ...
    def iter_cols(
        self,
        min_row: int | None = None,
        max_row: int | None = None,
        min_col: int | None = None,
        max_col: int | None = None,
        values_only: bool = False,
    ) -> typing.Iterator[tuple[object, ...]]: ...
    def __getitem__(self, key: str | slice) -> Cell | tuple[tuple[Cell, ...], ...]: ...
    def __setitem__(self, key: str, value: object) -> None: ...
    def set_cell(self, row: int, col: int, value: object) -> None: ...
    def set_cell_style(
        self,
        row: int,
        col: int,
        *,
        font: dict | Font | None = None,
        fill: dict | PatternFill | None = None,
        border: dict | Border | None = None,
        num_fmt: str | None = None,
    ) -> None: ...
    def insert_rows(self, idx: int, amount: int = 1) -> None: ...
    def delete_rows(self, idx: int, amount: int = 1) -> None: ...
    def insert_cols(self, idx: int, amount: int = 1) -> None: ...
    def delete_cols(self, idx: int, amount: int = 1) -> None: ...
    def move_range(
        self,
        range_string: str,
        rows: int = 0,
        cols: int = 0,
        translate: bool = False,
    ) -> None: ...

class EditableWorkbook:
    """A byte-preserving edit handle over an existing XLSX (from ``edit_excel``)."""

    def __init__(self) -> None: ...
    @property
    def sheetnames(self) -> list[str]: ...
    @property
    def worksheets(self) -> list[EditableSheet]: ...
    @property
    def active(self) -> EditableSheet: ...
    @active.setter
    def active(self, sheet: EditableSheet | str | int) -> None: ...
    def create_sheet(self, title: str | None = None, index: int | None = None) -> EditableSheet: ...
    def remove(self, worksheet: EditableSheet | str) -> None: ...
    def copy_worksheet(self, from_worksheet: EditableSheet | str) -> EditableSheet: ...
    def move_sheet(self, sheet: EditableSheet | str, offset: int = 0) -> None: ...
    def __getitem__(self, sheet_name: str) -> EditableSheet: ...
    def __delitem__(self, sheet_name: str) -> None: ...
    def save(self, path_or_fileobj: str | object) -> None: ...

class SheetStream:
    """A memory-bounded streaming iterator yielding PyArrow record batches."""
    def __iter__(self) -> SheetStream: ...
    def __next__(self) -> pa.RecordBatch: ...

def read_excel_turbo_iter(path: str, sheet_idx: int = 0, chunk_size: int = 10000) -> SheetStream: ...
def get_column_letter(col: int) -> str: ...
def column_index_from_string(s: str) -> int: ...
def coordinate_to_tuple(coord: str) -> tuple[int, int]: ...
def range_boundaries(range_str: str) -> tuple[int, int, int, int]: ...
def quote_sheetname(name: str) -> str: ...
def edit_excel(path: str, data_only: bool = False) -> EditableWorkbook: ...
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

# --- Phase 3 feature inspection / query API ---

def slicer_inventory(path: str) -> dict:
    """Inventory slicer/timeline parts: ``{slicers, timelines,
    slicer_caches, timeline_cache_names, sheet_slicer_refs}``."""

def rich_data_parts(path: str) -> list[str]:
    """List the ``xl/richData/*`` pass-through parts (plus ``xl/metadata.xml``)."""

def power_query_inventory(path: str) -> dict:
    """Inventory Power Query / data-model parts: ``{connections,
    has_data_mashup, custom_xml_parts, query_table_parts, model_parts}``."""

def is_signed_workbook(path: str) -> bool:
    """True when the workbook has digital-signature parts."""

def signature_info(path: str) -> list[dict]:
    """Detect every digital-signature part as ``{part_name, signed_at,
    signer_hint}`` dicts."""

def control_parts(path: str) -> list[str]:
    """List the form-control / ActiveX / embedded-OLE part names."""

def external_links(path: str) -> list[dict]:
    """Load every external book: ``{index, target, sheet_names,
    defined_names, cached}`` dicts."""

def feature_parts(path: str) -> dict:
    """One-call part inventory mapping category -> list of part names:
    ``{slicers, rich_data, power_query, signatures, controls,
    external_links}``."""

def diff_parts(a_path: str, b_path: str) -> list[dict]:
    """Compare two workbooks' part lists; ``{name, kind}`` where kind is
    "added" | "removed" | "value_changed" (a is BEFORE, b is AFTER)."""

def diff_workbooks(a_path: str, b_path: str) -> dict:
    """Diff two workbooks at part and cell level: ``{identical, parts,
    cells}`` (a is BEFORE, b is AFTER)."""

def read_threaded_comments(path: str) -> dict:
    """Read threaded comments and persons: ``{comments, persons}``."""

def write_threaded_comments(comments: list, persons: list) -> dict:
    """Serialize threaded comments and persons to
    ``{threaded_comments_xml, persons_xml}`` bytes."""

def read_sparklines(path: str, sheet_index: int) -> list[dict]:
    """Read sparkline groups on one worksheet (0-based ``sheet_index``)."""

def splice_sparklines(sheet_xml: bytes, groups: list) -> bytes:
    """Splice sparkline groups into a worksheet part, returning new bytes."""

def dependency_query(
    path: str,
    cells: list[tuple[int, int, int]],
    mode: str,
) -> list[tuple[int, int, int]]:
    """Answer a dependency query over the formula graph. ``cells`` are
    ``(sheet_index, row, col)`` seeds (all 0-based); ``mode`` is one of
    "precedents" | "dependents" | "precedents_deep" | "dependents_deep" |
    "impact" | "roots"."""

# --- turbo io: csv + json interchange ---

def sheet_to_csv(
    path: str,
    sheet: str,
    out_path: str,
    delimiter: str,
    quote: str,
    has_header: bool,
    infer_types: bool,
    date_format: str,
) -> None: ...
def sheet_to_csv_bytes(
    path: str,
    sheet: str,
    delimiter: str,
    quote: str,
    has_header: bool,
    infer_types: bool,
    date_format: str,
) -> bytes: ...
def csv_to_sheet(
    csv_path: str,
    xlsx_out: str,
    sheet_name: str,
    delimiter: str,
    quote: str,
    has_header: bool,
    infer_types: bool,
) -> None: ...
def csv_bytes_to_sheet(
    csv_bytes: bytes,
    xlsx_out: str,
    sheet_name: str,
    delimiter: str,
    quote: str,
    has_header: bool,
    infer_types: bool,
) -> None: ...
def sheet_to_json(
    path: str,
    sheet: str,
    out_path: str,
    shape: str,
    has_header: bool,
    date_format: str,
) -> None: ...
def sheet_to_json_bytes(
    path: str,
    sheet: str,
    shape: str,
    has_header: bool,
    date_format: str,
) -> bytes: ...
def json_to_sheet(
    json_path: str,
    xlsx_out: str,
    sheet_name: str,
    shape: str,
    has_header: bool,
) -> None: ...
def json_bytes_to_sheet(
    json_bytes: bytes,
    xlsx_out: str,
    sheet_name: str,
    shape: str,
    has_header: bool,
) -> None: ...

def edit_excel(path: str, data_only: bool = False) -> EditableWorkbook: ...

class Cell:
    @property
    def row(self) -> int: ...
    @property
    def column(self) -> int: ...
    @property
    def coordinate(self) -> str: ...
    @property
    def value(self) -> object: ...
    @value.setter
    def value(self, val: object) -> None: ...
    @property
    def font(self) -> object: ...
    @font.setter
    def font(self, val: object) -> None: ...
    @property
    def fill(self) -> object: ...
    @fill.setter
    def fill(self, val: object) -> None: ...
    @property
    def border(self) -> object: ...
    @border.setter
    def border(self, val: object) -> None: ...
    @property
    def alignment(self) -> object: ...
    @alignment.setter
    def alignment(self, val: object) -> None: ...
    @property
    def protection(self) -> object: ...
    @protection.setter
    def protection(self, val: object) -> None: ...
    @property
    def number_format(self) -> str | None: ...
    @number_format.setter
    def number_format(self, val: str | None) -> None: ...
    @property
    def hyperlink(self) -> str | None: ...
    @hyperlink.setter
    def hyperlink(self, val: str | None) -> None: ...
    @property
    def comment(self) -> str | None: ...
    @comment.setter
    def comment(self, val: object) -> None: ...

class EditableSheet:
    @property
    def title(self) -> str: ...
    @title.setter
    def title(self, val: str) -> None: ...
    @property
    def max_row(self) -> int: ...
    @property
    def max_column(self) -> int: ...
    @property
    def min_row(self) -> int: ...
    @property
    def min_column(self) -> int: ...
    @property
    def freeze_panes(self) -> str | None: ...
    @freeze_panes.setter
    def freeze_panes(self, val: str | None) -> None: ...
    @property
    def tab_color(self) -> str | None: ...
    @tab_color.setter
    def tab_color(self, val: str | None) -> None: ...
    @property
    def auto_filter(self) -> str | None: ...
    @auto_filter.setter
    def auto_filter(self, val: str | None) -> None: ...
    @property
    def protection(self) -> dict | None: ...
    @protection.setter
    def protection(self, val: dict | None) -> None: ...
    @property
    def page_setup(self) -> dict | None: ...
    @page_setup.setter
    def page_setup(self, val: dict | None) -> None: ...
    @property
    def data_validations(self) -> list[dict]: ...
    @data_validations.setter
    def data_validations(self, val: list[dict] | None) -> None: ...
    def cell(self, row: int, column: int, value: object = None) -> Cell: ...
    def merge_cells(self, range_string: str | None = None, start_row: int | None = None, start_column: int | None = None, end_row: int | None = None, end_column: int | None = None) -> None: ...
    def unmerge_cells(self, range_string: str | None = None, start_row: int | None = None, start_column: int | None = None, end_row: int | None = None, end_column: int | None = None) -> None: ...
    def insert_rows(self, idx: int, amount: int = 1) -> None: ...
    def delete_rows(self, idx: int, amount: int = 1) -> None: ...
    def insert_cols(self, idx: int, amount: int = 1) -> None: ...
    def delete_cols(self, idx: int, amount: int = 1) -> None: ...
    def move_range(self, cell_range: str, rows: int = 0, cols: int = 0, translate: bool = False) -> None: ...
    def append(self, row: list | tuple) -> None: ...

class EditableWorkbook:
    def __init__(self) -> None: ...
    @property
    def sheetnames(self) -> list[str]: ...
    @property
    def worksheets(self) -> list[EditableSheet]: ...
    @property
    def active(self) -> EditableSheet: ...
    @active.setter
    def active(self, val: str | int | EditableSheet) -> None: ...
    def create_sheet(self, title: str | None = None, index: int | None = None) -> EditableSheet: ...
    def remove(self, worksheet: str | EditableSheet) -> None: ...
    def copy_worksheet(self, from_worksheet: str | EditableSheet) -> EditableSheet: ...
    def move_sheet(self, sheet: str | EditableSheet, offset: int = 0) -> None: ...
    def save(self, target: object) -> None: ...

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
