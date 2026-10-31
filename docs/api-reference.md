# API reference

This page documents the public surface used in this documentation. Signatures
are taken from the current runtime and source. The type stub at
[`python/kyrax/_kyrax.pyi`](../python/kyrax/_kyrax.pyi) is the source-level type
reference. It parses and has no duplicate top-level names in this checkout, but
it is not a runtime completeness test and may lag the runtime surface. Treat the
signatures below as authoritative for the documented surface, and see
[Limitations](limitations.md).

## Top-level functions

```python
read_excel(source: Path | str | bytes) -> ExcelReader
read_excel_turbo(path: Path | str, password: str | None = None) -> TurboReader
write_excel_turbo(path: Path | str, sheets: list[dict], *, ...) -> None
write_excel_turbo_bytes(sheets: list[dict], *, ...) -> bytes
write_excel_turbo_stream(path: Path | str, sheets: list[dict], *, ...) -> None
load_workbook(filename, read_only=False, data_only=False, edit_mode=None, **kwargs)
edit_excel(path: Path | str, data_only: bool = False) -> EditableWorkbook
edit_excel_bytes(data: bytes, data_only: bool = False) -> EditableWorkbook
validate_excel(path: Path | str) -> dict
repair_excel(path: Path | str, out_path: Path | str, *, severity="warning") -> dict
is_encrypted(path: Path | str) -> bool
encryption_info(path: Path | str) -> dict
```

| Function | Returns | Notes |
|---|---|---|
| `read_excel` | `ExcelReader` | Path or bytes. Calamine-backed; exercised with XLSX, XLS, and ODS fixtures. |
| `read_excel_turbo` | `TurboReader` | Path only. `password` opens an encrypted workbook when supported. |
| `write_excel_turbo` | `None` | Writes an XLSX to a path. |
| `write_excel_turbo_bytes` | `bytes` | Returns XLSX bytes. |
| `write_excel_turbo_stream` | `None` | Streams an XLSX to a path; plain row generators are consumed once. |
| `load_workbook` | Editable workbook, `ReadOnlyWorkbook`, or `TurboReader` | Default editable; `read_only=True` streams; `edit_mode=False` returns a turbo reader. |
| `edit_excel` / `edit_excel_bytes` | `EditableWorkbook` | Direct byte-preserving edit entry points. |
| `validate_excel` | `dict` | Never raises for a bad input; returns a structured report. |
| `repair_excel` | `dict` | Writes a separate output for repairable findings; never modifies the source. |
| `is_encrypted` | `bool` | Detects an ECMA-376 encrypted workbook without a password. |
| `encryption_info` | `dict` | Requires a build with the `encryption` feature; otherwise raises `KyraxError`. |

`write_excel_turbo`, `write_excel_turbo_bytes`, and `write_excel_turbo_stream`
share these keyword options:

```python
string_mode: Literal["inline", "sst", "auto"] = "inline"
emit_cached_values: bool = True
date1904: bool = False
date_iso: bool = False
features: list[str] | str | None = None
active_tab: int = 0
named_styles: list[dict] | None = None
props: dict | None = None
defined_names: list[dict] | None = None
chartsheets: list[dict] | None = None
lock_structure: bool = False
external_links: list | None = None
creator: str | None = None
macro_enabled: bool = False
recalculate: bool = False
vba_archive_path: Path | str | None = None
```

See the [input guide](input-guide.md#turbo-writer-sheet-dictionaries) for the
sheet-dictionary schema and validation rules.

## Readers

### `ExcelReader`

```python
reader.sheet_names -> list[str]
reader.load_sheet(
    idx_or_name: int | str,
    *,
    header_row: int | None = 0,
    column_names: list[str] | None = None,
    skip_rows: int | list[int] | Callable[[int], bool] | None = None,
    n_rows: int | None = None,
    schema_sample_rows: int | None = 1000,
    dtype_coercion: Literal["coerce", "strict"] = "coerce",
    use_columns: list[str] | list[int] | str | Callable[[ColumnInfoNoDtype], bool] | None = None,
    dtypes: DType | DTypeMap | None = None,
    eager: bool = False,
    skip_whitespace_tail_rows: bool = False,
    whitespace_as_null: bool = False,
) -> ExcelSheet | pa.RecordBatch
reader.load_sheet_eager(idx_or_name, *, ...) -> pa.RecordBatch
reader.load_sheet_by_name(name, *, ...) -> ExcelSheet
reader.load_sheet_by_idx(idx, *, ...) -> ExcelSheet
reader.load_table(
    name: str,
    *,
    header_row: int | None = None,
    column_names: list[str] | None = None,
    skip_rows: int | None = None,
    n_rows: int | None = None,
    schema_sample_rows: int | None = 1000,
    dtype_coercion: Literal["coerce", "strict"] = "coerce",
    use_columns: list[str] | list[int] | str | Callable[[ColumnInfoNoDtype], bool] | None = None,
    dtypes: DType | DTypeMap | None = None,
    eager: bool = False,
    skip_whitespace_tail_rows: bool = False,
    whitespace_as_null: bool = False,
) -> ExcelTable | pa.RecordBatch
reader.table_names(sheet_name: str | None = None) -> list[str]
reader.defined_names() -> list[DefinedName]
```

`header_row` is zero-based; `None` means no header. `column_names` overrides the
header and causes `header_row` to be ignored. `skip_rows` indices are zero-based
relative to data rows after the header. `schema_sample_rows=0` raises
`InvalidParametersError`; `None` samples all rows. `dtype_coercion="strict"`
raises instead of widening a conflicting inferred column to string. `eager=True`
returns a PyArrow `RecordBatch` and requires PyArrow. `load_table` defaults to
`header_row=None` and uses the table's column names.

### `ExcelSheet`

```python
sheet.name -> str
sheet.width -> int
sheet.height -> int
sheet.total_height -> int
sheet.selected_columns -> list[ColumnInfo]
sheet.available_columns() -> list[ColumnInfo]
sheet.specified_dtypes -> DTypeMap | None
sheet.visible -> Literal["visible", "hidden", "veryhidden"]
sheet.to_arrow() -> pa.RecordBatch
sheet.to_arrow_with_errors() -> tuple[pa.RecordBatch, CellErrors]
sheet.to_pandas() -> pd.DataFrame
sheet.to_polars() -> pl.DataFrame
sheet.__arrow_c_schema__() -> object
sheet.__arrow_c_array__(requested_schema=None) -> tuple[object, object]
```

`to_arrow`, `to_arrow_with_errors`, and `to_pandas` require PyArrow.
`to_polars` requires Polars. The `__arrow_c_*` methods implement the Arrow
PyCapsule Interface for zero-copy exchange.

### `ExcelTable`

```python
table.name -> str
table.sheet_name -> str
table.width -> int
table.height -> int
table.total_height -> int
table.offset -> int
table.selected_columns -> list[ColumnInfo]
table.available_columns() -> list[ColumnInfo]
table.specified_dtypes -> DTypeMap | None
table.to_arrow() -> pa.RecordBatch
table.to_pandas() -> pd.DataFrame
table.to_polars() -> pl.DataFrame
table.__arrow_c_schema__() -> object
table.__arrow_c_array__(requested_schema=None) -> tuple[object, object]
```

### Metadata types

```python
ColumnInfo.name -> str
ColumnInfo.index -> int
ColumnInfo.absolute_index -> int
ColumnInfo.dtype -> DType
ColumnInfo.column_name_from -> ColumnNameFrom
ColumnInfo.dtype_from -> DTypeFrom

CellError.position -> tuple[int, int]
CellError.offset_position -> tuple[int, int]
CellError.row_offset -> int
CellError.detail -> str
CellErrors.errors -> list[CellError]

DefinedName.name -> str
DefinedName.formula -> str
```

`DType` is one of `"null"`, `"int"`, `"float"`, `"string"`, `"boolean"`,
`"datetime"`, `"date"`, or `"duration"`.

### `TurboReader`

```python
reader.sheet_names -> list[str]
reader.active_tab -> int
reader.date1904 -> bool
reader.load_sheet(
    idx_or_name: int | str,
    *,
    features: list[str] | str = "values",
    header_row: int | None = 0,
) -> TurboSheet
reader.defined_names() -> list[dict] | None
reader.tables() -> list[dict] | None
reader.workbook_props() -> dict | None
reader.persons() -> list[dict] | None
reader.has_vba -> bool
reader.vba_project() -> bytes | None
```

`features` is `"values"` (default), `"all"`, or a list containing `styles`,
`formulas`, `merges`, `defined_names`, `tables`, `hyperlinks`, `comments`,
`sheet_meta`, `page_setup`, `workbook_meta`, `validations`, `cond_format`,
`charts`, `images`, `pivots`, and `vba`. Values are always included. The turbo
reader supports only `header_row=0` and `header_row=None`; other values raise
`ValueError`.

### `TurboSheet`

```python
sheet.name -> str
sheet.nrows -> int
sheet.ncols -> int
sheet.column_names -> list[str]
sheet.to_arrow() -> pa.RecordBatch
sheet.to_arrow_with_errors() -> tuple[pa.RecordBatch, CellErrors]
sheet.cell_errors() -> pa.RecordBatch
sheet.style_indices() -> list[list[int]] | None
sheet.style_table() -> list[dict] | None
sheet.formulas() -> pa.RecordBatch | None
sheet.merges() -> list[str] | None
sheet.hyperlinks() -> list[dict] | None
sheet.comments() -> pa.RecordBatch | None
sheet.comment_authors() -> list[str] | None
sheet.legacy_is_mirror -> bool
sheet.threaded_comments() -> list[dict] | None
sheet.charts() -> list[dict] | None
sheet.images() -> list[dict] | None
sheet.pivots() -> list[dict] | None
sheet.tables() -> list[dict] | None
sheet.sheet_state -> str
sheet.sheet_kind -> str
sheet.row_dimensions() -> dict | None
sheet.column_dimensions() -> list[dict] | None
sheet.sheet_format() -> dict | None
sheet.auto_filter() -> dict | None
sheet.sheet_view() -> dict | None
sheet.freeze_panes() -> str | None
sheet.protection() -> dict | None
sheet.page_setup() -> dict | None
sheet.page_margins() -> dict | None
sheet.print_options() -> dict | None
sheet.header_footer() -> dict | None
sheet.code_name -> str | None
sheet.tab_color -> str | None
sheet.data_validations() -> list[dict] | None
sheet.conditional_formatting() -> list[dict] | None
sheet.named_styles() -> list[dict] | None
```

Accessors return `None` when the corresponding feature was not requested.
`to_arrow`, `to_arrow_with_errors`, `cell_errors`, `formulas`, and `comments`
require PyArrow.

### Read-only workbooks

```python
wb = load_workbook(path, read_only=True, data_only=False)
wb.sheetnames -> list[str]
wb.worksheets -> list[ReadOnlyWorksheet]
wb.active -> ReadOnlyWorksheet
wb[name_or_index] -> ReadOnlyWorksheet
wb.load_sheet(idx_or_name, **kwargs) -> TurboSheet
wb.close() -> None

ws.title -> str
ws.max_row -> int | None
ws.max_column -> int | None
ws.min_row -> int
ws.min_column -> int
ws.iter_rows(min_row=None, max_row=None, min_col=None, max_col=None, values_only=False)
ws.values -> Iterator[tuple]
ws.cell(row, column) -> _ReadOnlyCellProxy
ws.close() -> None
```

`ReadOnlyWorkbook` is a context manager. `iter_rows` supports
`values_only=True` only; other values raise `NotImplementedError`. `max_row` and
`max_column` may be `None` for lazy output that omits the worksheet dimension.
`_ReadOnlyCellProxy` exposes `value`, `row`, and `column`.

### Low-level streaming functions

```python
read_excel_turbo_iter(path, sheet_idx=0, chunk_size=10000) -> SheetStream
read_excel_turbo_iter_rows(
    path,
    sheet_idx=0,
    min_row=None,
    max_row=None,
    min_col=None,
    max_col=None,
    chunk_size=10000,
    data_only=False,
) -> ReadOnlyRows
read_excel_turbo_cell(
    path,
    sheet_idx=0,
    row=1,
    column=1,
    chunk_size=10000,
    data_only=False,
) -> object | None
close_streams(streams: list[object]) -> None
```

`SheetStream` yields PyArrow `RecordBatch` chunks. `ReadOnlyRows` yields Python
value tuples. Both expose `close()` and `closed`. Row one is ordinary worksheet
data. `data_only=True` selects cached formula results.

## Writers

The three writer functions share the options listed under
[Top-level functions](#top-level-functions). The sheet dictionary schema is
documented in the [input guide](input-guide.md#turbo-writer-sheet-dictionaries).

## Editable workbooks

### `load_workbook`, `edit_excel`, `edit_excel_bytes`

```python
load_workbook(
    filename: Path | str | bytes | bytearray | memoryview | BinaryIO,
    read_only: bool = False,
    data_only: bool = False,
    edit_mode: bool | None = None,
    **kwargs,
)
```

- Default or `edit_mode=True`: returns an `EditableWorkbook`.
- `read_only=True`: returns a `ReadOnlyWorkbook`.
- `edit_mode=False`: returns a `TurboReader`; keyword arguments are passed to
  `read_excel` and unsupported arguments raise `TypeError`.
- Unsupported keyword arguments on the editable/read-only paths raise
  `TypeError`.
- `data_only=True` returns cached formula results; missing caches are not
  calculated.

`edit_excel(path, data_only=False)` and `edit_excel_bytes(data, data_only=False)`
return an `EditableWorkbook` directly.

### `EditableWorkbook`

```python
wb = Workbook()  # alias for EditableWorkbook
wb.sheetnames -> list[str]
wb.worksheets -> list[EditableSheet]
wb.active -> EditableSheet
wb.active = sheet_or_name_or_index
wb.create_sheet(title=None, index=None) -> EditableSheet
wb.remove(worksheet_or_name) -> None
wb.copy_worksheet(from_worksheet) -> EditableSheet
wb.move_sheet(sheet_or_name, offset=0) -> None
wb[name] -> EditableSheet
del wb[name]
wb.defined_names -> DefinedNameDict
wb.properties -> DocumentProperties
wb.get_sheet_by_name(name) -> EditableSheet
wb.index(worksheet_or_name) -> int
wb.close() -> None
len(wb) -> int
name in wb -> bool
iter(wb) -> Iterator[EditableSheet]
wb.save(path_or_fileobj) -> None
```

`wb.properties` is read-only in the current build; assigning a property raises
`AttributeError`. `wb.save` accepts a path or a writable binary file-like object
and does not calculate formulas.

`DefinedNameDict` is dict-like: `wb.defined_names[name] = "Sheet!$A$1"` creates a
global name; it supports `get`, `keys`, `values`, `items`, `add`, `in`, `len`,
iteration, and deletion. `WorkbookDefinedName(name, attr_text=None, value=None)`
models an entry.

### `EditableSheet`

```python
ws.title -> str
ws.print_area -> str | None
ws.print_title_rows -> str | None
ws.print_title_cols -> str | None
ws.sheet_state -> str
ws.min_row -> int
ws.max_row -> int
ws.min_column -> int
ws.max_column -> int
ws.dimensions -> str
ws.freeze_panes -> str | None
ws.tab_color -> str | None
ws.auto_filter -> AutoFilter
ws.values -> Iterator[tuple]
ws.rows -> Iterator[tuple[Cell, ...]]
ws.columns -> Iterator[tuple[Cell, ...]]
ws.merged_cells -> MultiCellRange
ws.protection -> dict | None
ws.page_setup -> dict | None
ws.data_validations -> list[dict]
ws.column_dimensions -> DimensionHolder
ws.row_dimensions -> DimensionHolder
ws.conditional_formatting -> ConditionalFormattingList

ws.append(iterable) -> None
ws.cell(row, column, value=None) -> Cell
ws.set_cell(row, col, value) -> None
ws.set_cell_style(row, col, *, font=None, fill=None, border=None, num_fmt=None) -> None
ws.iter_rows(min_row=None, max_row=None, min_col=None, max_col=None, values_only=False)
ws.iter_cols(min_row=None, max_row=None, min_col=None, max_col=None, values_only=False)
ws.merge_cells(range_string=None, start_row=None, start_column=None, end_row=None, end_column=None) -> None
ws.unmerge_cells(range_string=None, start_row=None, start_column=None, end_row=None, end_column=None) -> None
ws.insert_rows(idx, amount=1) -> None
ws.delete_rows(idx, amount=1) -> None
ws.insert_cols(idx, amount=1) -> None
ws.delete_cols(idx, amount=1) -> None
ws.move_range(range_string, rows=0, cols=0, translate=False) -> None
ws.add_data_validation(dv) -> None
ws.add_table(table) -> None
ws.add_chart(chart, anchor=None) -> None
ws.add_image(img, anchor=None) -> None
```

`ws["A1"]` returns a `Cell`; `ws["A1:B2"]` returns a tuple of row tuples of
cells; `ws[3]` returns a tuple of cells for the 1-based row. `ws["A1"] = value`
and `ws["A1":"C1"] = [[...]]` assign values. Row and column numbers are 1-based.
`merge_cells` rejects overlapping merges. `add_table` requires a header row and
at least one data row. `add_chart`/`add_image` are refused when a loaded sheet
already contains drawings.

### `Cell`

```python
cell.row -> int
cell.column -> int
cell.coordinate -> str
cell.value -> object
cell.font -> Font
cell.fill -> PatternFill
cell.border -> Border
cell.alignment -> Alignment
cell.protection -> Protection
cell.style -> str | None
cell.number_format -> str | None
cell.hyperlink -> str | None
cell.comment -> str | None
cell.offset(row=0, column=0) -> Cell
```

Style, hyperlink, comment, and number-format properties are settable.

### Style classes

```python
Font(name=None, size=None, sz=None, bold=None, b=None, italic=None, i=None,
     strike=None, underline=None, u=None, color=None)
PatternFill(fill_type=None, patternType=None, start_color=None, end_color=None,
            fgColor=None, bgColor=None)
Side(style=None, border_style=None, color=None)
Border(left=None, right=None, top=None, bottom=None, diagonal=None,
       diagonal_up=False, diagonal_down=False, diagonalUp=None,
       diagonalDown=None, outline=True)
Alignment(horizontal=None, vertical=None, text_rotation=0, textRotation=None,
          wrap_text=None, wrapText=None, shrink_to_fit=None, shrinkToFit=None,
          indent=0, relative_indent=0, relativeIndent=None,
          justify_last_line=None, justifyLastLine=None, reading_order=0,
          readingOrder=None)
Protection(locked=True, hidden=False)
Comment(text="", author="")
```

These classes are exported from `kyrax` and `kyrax.styles`.

### Worksheet helpers

```python
DataValidation(
    type=None, formula1=None, formula2=None, showErrorMessage=False,
    showInputMessage=False, showDropDown=False, allowBlank=False, sqref=(),
    promptTitle=None, errorStyle=None, error=None, prompt=None,
    errorTitle=None, imeMode=None, operator=None, allow_blank=None,
)
dv.add(cell_or_range) -> None
dv.sqref -> str
dv.allow_blank -> bool

Table(displayName=None, ref=None, name=None, tableStyleInfo=None, id=None, **_)
TableStyleInfo(name=None, showFirstColumn=None, showLastColumn=None,
               showRowStripes=None, showColumnStripes=None)
```

`DataValidation` synchronizes attached field changes to the sheet. `Table`
accepts extra keyword arguments for openpyxl compatibility and ignores them.

### Conditional-formatting rules

```python
Rule(type, dxf=None, priority=None, stopIfTrue=None, operator=None, text=None,
     timePeriod=None, rank=None, percent=None, bottom=None, aboveAverage=None,
     equalAverage=None, stdDev=None, formula=None, colorScale=None,
     dataBar=None, iconSet=None, **_)
DifferentialStyle(font=None, fill=None, border=None, **_)
CellIsRule(operator=None, formula=None, stopIfTrue=None, font=None, border=None, fill=None)
FormulaRule(formula=None, stopIfTrue=None, font=None, border=None, fill=None)
ColorScaleRule(start_type=None, start_value=None, start_color=None,
               mid_type=None, mid_value=None, mid_color=None,
               end_type=None, end_value=None, end_color=None)
DataBarRule(start_type=None, start_value=None, end_type=None, end_value=None,
            color=None, showValue=None, minLength=None, maxLength=None)
IconSetRule(icon_style=None, type=None, values=None, showValue=None,
            percent=None, reverse=None)
```

Use `ws.conditional_formatting.add("B2:B10", rule)`. Nested rule fields are not
exhaustively documented here.

### Charts

```python
Reference(worksheet=None, min_col=None, min_row=None, max_col=None, max_row=None,
          range_string=None)
Series(values, xvalues=None, zvalues=None, title=None, title_from_data=False)
BarChart()
LineChart()
AreaChart()
PieChart()
ScatterChart()
chart.add_data(data, from_rows=False, titles_from_data=False) -> None
chart.set_categories(labels) -> None
```

Chart types support a title, style, size, grouping, and legend position. Axis
titles are accepted but not rendered. `Series(zvalues=...)` raises
`NotImplementedError`.

### Images

```python
Image(img: str | os.PathLike[str] | IO[bytes])
img.width -> int
img.height -> int
img.anchor -> str
```

`Image` reads PNG, JPEG, or GIF bytes and the intrinsic pixel size. Add it with
`ws.add_image(img, anchor)`.

## Formulas

```python
formulas.evaluate(formula: str, context: dict[str, Any] | None = None) -> Any
formulas.list_functions() -> list[tuple[str, str]]
formulas.recalculate(sheets: list[dict[str, Any]]) -> bytes
formulas.dependencies(formula: str) -> list[str]
```

`evaluate` returns floats, strings (including Excel error codes), booleans,
`None`, or nested lists. Parse errors raise `ValueError`; unknown functions
return an Excel error. `dependencies` returns static references only; dynamic
references such as `INDIRECT` are omitted. `recalculate` returns XLSX bytes and
leaves unsupported formulas uncomputed with `fullCalcOnLoad="1"`.

## CSV and JSON

```python
kyrax.io.sheet_to_csv(path, sheet, out_path, *, delimiter=",", quote='"',
                      has_header=True, infer_types=False,
                      date_format="yyyy-mm-dd hh:mm:ss") -> None
kyrax.io.sheet_to_csv_bytes(path, sheet, *, ...) -> bytes
kyrax.io.csv_to_sheet(csv_path, xlsx_out, sheet_name="Sheet1", *, ...) -> None
kyrax.io.csv_bytes_to_sheet(csv_bytes, xlsx_out, sheet_name="Sheet1", *, ...) -> None
kyrax.io.sheet_to_json(path, sheet, out_path, *, shape="records",
                       has_header=True, date_format="") -> None
kyrax.io.sheet_to_json_bytes(path, sheet, *, ...) -> bytes
kyrax.io.json_to_sheet(json_path, xlsx_out, sheet_name="Sheet1", *, ...) -> None
kyrax.io.json_bytes_to_sheet(json_bytes, xlsx_out, sheet_name="Sheet1", *, ...) -> None
```

`shape` is `"records"`, `"columns"`, or `"ndjson"`. CSV `infer_types=False` is
the default. JSON import always writes a header row; `has_header` is accepted
for API symmetry.

## Utilities

```python
kyrax.utils.get_column_letter(col: int) -> str
kyrax.utils.column_index_from_string(s: str) -> int
kyrax.utils.coordinate_to_tuple(coord: str) -> tuple[int, int]
kyrax.utils.range_boundaries(range_str: str) -> tuple[int, int, int, int]
kyrax.utils.quote_sheetname(name: str) -> str
```

These are also re-exported from the native extension.

## Exceptions

```python
KyraxError
UnsupportedColumnTypeCombinationError(KyraxError)
CannotRetrieveCellDataError(KyraxError)
CalamineCellError(KyraxError)
CalamineError(KyraxError)
SheetNotFoundError(KyraxError)
ColumnNotFoundError(KyraxError)
ArrowError(KyraxError)
InvalidParametersError(KyraxError)
```

Built-in exceptions are also used: `ValueError`, `TypeError`, `KeyError`,
`IndexError`, `NotImplementedError`, and `ImportError`. See the
[input guide](input-guide.md#error-handling) for when they are raised.
