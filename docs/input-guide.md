# Input guide

This guide describes the input surface of the current checkout. It focuses on
the inputs that are documented and exercised here; it is not an exhaustive
catalog of every nested writer field or every API member. See
[Limitations](limitations.md) for known gaps.

## Supported inputs

| Entry point | Accepts | Returns |
|---|---|---|
| `kyrax.read_excel(source)` | Path or workbook bytes. The calamine-backed reader is exercised with XLSX, XLS, and ODS fixtures. | `ExcelReader` |
| `kyrax.read_excel_turbo(path, password=None)` | Path to an XLSX/XLSM workbook. `password` opens an encrypted workbook when the build supports it. | `TurboReader` |
| `kyrax.load_workbook(filename, ...)` | Path, `bytes`, `bytearray`, `memoryview`, or a binary file-like object. | Editable workbook by default; read-only workbook with `read_only=True`; turbo reader with `edit_mode=False`. |
| `kyrax.write_excel_turbo(path, sheets, ...)` | Path and a list of sheet dictionaries. | `None` |
| `kyrax.write_excel_turbo_bytes(sheets, ...)` | A list of sheet dictionaries. | XLSX `bytes` |
| `kyrax.write_excel_turbo_stream(path, sheets, ...)` | Path and a list of sheet dictionaries; plain row generators are consumed once. | `None` |
| `kyrax.edit_excel(path, data_only=False)` | Path to an existing XLSX/XLSM workbook. | `EditableWorkbook` |
| `kyrax.edit_excel_bytes(data, data_only=False)` | Workbook bytes. | `EditableWorkbook` |

`read_excel_turbo` does not accept bytes; use `read_excel` or `load_workbook`
for in-memory input. `read_excel` does not accept file-like objects; use
`load_workbook` for those.

## Coordinate conventions

| Input | Convention |
|---|---|
| Editable `ws.cell(row, column)` | 1-based row and column. |
| Editable A1 references (`"B2"`, `"A1:C3"`) | 1-based, as in Excel. |
| Editable `ws[3]` row lookup | 1-based row; returns a tuple of cells. Current source supports it; older builds may raise `TypeError`. |
| Editable `ws["A1:B2"]` block | Returns a tuple of row tuples of cells. |
| Turbo sheet `rows` / `rows_iter` | The first item begins at A1. Headers are ordinary rows. |
| Turbo `formulas` map `(row, col)` or list `row`, `col` | Zero-based; A1 is `(0, 0)`. |
| Turbo `cell_styles` map or list `row`, `col` | Zero-based in current source; A1 is `(0, 0)`. |
| Turbo `row_dims` `row` | 1-based. `row_idx` is the zero-based alternative. |
| Turbo `col_dims` `min`, `max`, `col` | 1-based column numbers. |
| Excel ranges, merges, references, `freeze_panes` | A1 notation, e.g. `"A1:B3"`, `"B2"`. |
| Standard reader `header_row`, sheet index, `use_columns` integer indices | Zero-based. |
| Standard reader `skip_rows` list/callable | Zero-based relative to data rows after the header. |
| Read-only `iter_rows` `min_row`/`max_row`/`min_col`/`max_col` | 1-based. |
| `read_excel_turbo_cell(path, sheet_idx, row, column)` | 1-based row and column. |

Excel's grid limit is 1,048,576 rows by 16,384 columns. The writers validate
many coordinates against that limit, but not every nested field is exhaustively
checked.

## Create and edit workbooks

```python
import kyrax

wb = kyrax.Workbook()
ws = wb.active
ws.title = "Sales"
ws.append(["Item", "Price"])
ws.append(["Apple", 2.5])
ws.cell(row=3, column=1, value="Pear")
ws["B3"] = 3
ws["C2"] = "=B2*2"
ws.freeze_panes = "B2"
wb.save("sales.xlsx")
```

`Workbook()` is an alias for `EditableWorkbook`. `load_workbook` defaults to
editable mode; `edit_excel` and `edit_excel_bytes` are the direct entry points.
`save` accepts a path or a writable binary file-like object. Saving does not
calculate formulas.

Editable sheets support cell assignment, `append`, `cell`, `set_cell`,
`iter_rows`, `iter_cols`, `rows`, `columns`, `values`, `merge_cells`,
`unmerge_cells`, `insert_rows`, `delete_rows`, `insert_cols`, `delete_cols`,
`move_range`, `freeze_panes`, `tab_color`, `auto_filter.ref`,
`column_dimensions`, `row_dimensions`, `print_area`, `print_title_rows`,
`print_title_cols`, `sheet_state`, `protection`, `page_setup`,
`data_validations`, `conditional_formatting`, `add_data_validation`,
`add_table`, `add_chart`, and `add_image`.

## Turbo writer sheet dictionaries

Each entry in `sheets` is a dictionary. The following are the core keys used in
this documentation. Unknown top-level sheet keys raise `ValueError`, and
conflicting data sources (`rows` with `columns`, for example) are rejected.

| Key | Meaning |
|---|---|
| `name` | Sheet name. Defaults to `Sheet`; supply a valid name explicitly. |
| `rows` / `rows_iter` | Iterable of row iterables. The first row starts at A1 and may contain headers. |
| `columns` / `data` / `table` | Column-oriented data. Each inner sequence is a column and includes its header as the first value. Use one data source per sheet. |
| `grid` | NumPy 2-D C-contiguous `float32`/`float64` array. Single-sheet only; see [Grid and Arrow](#grid-and-arrow). |
| `formulas` | Formula map or list. Coordinates are zero-based. |
| `cell_styles` / `styles` | Style map or list. Coordinates are zero-based. |
| `style_palette` | List of style dictionaries applied cyclically across existing cells. |
| `row_dims` / `col_dims` | Row and column dimensions. |
| `merges` | List of A1 range strings. |
| `hyperlinks` | List of dictionaries with `ref` and optional `target`, `location`, `display`. |
| `comments` | List of dictionaries with `ref`, `text`, optional `author`, `height`, `width`. |
| `tables` | List of table dictionaries. |
| `data_validations` / `validations` / `dv` | List of data-validation dictionaries. |
| `conditional_formatting` / `cf` | List of conditional-formatting dictionaries. |
| `auto_filter` | Range string or dictionary with `ref` and optional `columns`. |
| `protection` / `sheet_protection` | Boolean or dictionary with `sheet`, `password`, `already_hashed`. |
| `scenarios` | List of dictionaries with `name` and optional `cells`. |
| `print_options`, `page_margins`, `page_setup`, `header_footer` | Print settings. |
| `print_area`, `print_titles`, `row_breaks`, `col_breaks`, `tab_color` | Layout and print metadata. |
| `freeze_panes` / `freeze_cell` | Cell reference string. |
| `state` / `visibility` | `visible`, `hidden`, or `veryHidden`. At least one sheet must remain visible. |
| `charts` | List of chart dictionaries. |
| `images` | List of image dictionaries with `data`/`bytes` or `path`, and optional `anchor`. |
| `pivots` | List of pivot-table dictionaries. |
| `named_styles` | Per-sheet named styles; the workbook keyword also accepts a list. |

The nested schemas below are the selected, source-grounded shapes used in this
documentation. They are not a complete specification of every field or
combination. Validate important output with an independent reader.

### Rows, columns, and formulas

```python
import kyrax

sheets = [
    {
        "name": "Data",
        "rows": [["A", "B", "Sum"], [2, 3, None]],
        "formulas": {(1, 2): {"text": "A2+B2", "cached": 5}},
    }
]
kyrax.write_excel_turbo("formula.xlsx", sheets)
```

`formulas` accepts a dictionary keyed by zero-based `(row, col)` pairs. The value
is a formula string or a dictionary with `text` (or `formula`), optional
`cached`, `kind`, and `ref`. It also accepts a list of dictionaries with
zero-based `row`, `col`, and `text`/`formula`. A missing `cached` field or
`cached=None` means no supplied cached value; numeric zero is retained as a
value. `emit_cached_values=False` suppresses supplied caches. `recalculate=True`
computes formulas the engine supports and leaves unsupported formulas for the
spreadsheet application.

### Styles and dimensions

```python
import kyrax

sheets = [
    {
        "name": "Styled",
        "rows": [["Heading", "Value"], ["A", 1]],
        "cell_styles": {
            (0, 0): {"font": {"bold": True, "color": "0070C0"}},
            (0, 1): {"fill": {"pattern_type": "solid", "fg_color": "FFFFE0"}},
        },
        "row_dims": [{"row": 1, "height": 24}],
        "col_dims": [{"min": 1, "width": 18}],
    }
]
kyrax.write_excel_turbo("styled.xlsx", sheets)
```

Style dictionaries may include `font`, `fill`, `border`, `alignment`,
`protection`, `num_fmt`/`number_format`, `named_style`, `quote_prefix`, and
`pivot_button`. Colors accept an RGB string or a dictionary with `rgb`, `theme`,
`indexed`, and `tint`. The exact accepted value domains and aliases are not
exhaustively documented here; see the source parser and tests before relying on
unusual combinations.

### Structure, validation, and tables

```python
import kyrax

sheets = [
    {
        "name": "Data",
        "rows": [["Name", "Score"], ["Apple", 3], ["Pear", 5]],
        "freeze_panes": "A2",
        "auto_filter": "A1:B3",
        "merges": ["D1:E1"],
        "data_validations": [
            {
                "type": "whole",
                "operator": "between",
                "formula1": "1",
                "formula2": "10",
                "sqref": "B2:B3",
            }
        ],
        "tables": [
            {
                "display_name": "Scores",
                "ref": "A1:B3",
                "columns": ["Name", "Score"],
            }
        ],
    }
]
kyrax.write_excel_turbo("structured.xlsx", sheets)
```

Data-validation `type`, `operator`, `errorStyle`, `imeMode`, and `sqref` are
validated. Table names are validated against a subset of Excel's rules, table
references must include a header row and at least one data row, column counts
must match the reference width, and table names must be unique. The writer does
not verify that supplied `columns` names match the header-row cell text.

### Charts, images, and pivots

```python
import kyrax

sheets = [
    {
        "name": "Data",
        "rows": [["Region", "Amount"], ["East", 10], ["West", 20]],
        "charts": [
            {
                "type": "bar",
                "title": "Amounts",
                "series": [{"val_ref": "Data!$B$2:$B$3", "cat_ref": "Data!$A$2:$A$3"}],
                "anchor": "D2",
            }
        ],
        "pivots": [
            {
                "name": "PivotTable1",
                "source_range": "A1:B3",
                "rows": ["Region"],
                "data": [{"field": "Amount", "agg": "sum"}],
                "target_cell": "F3",
            }
        ],
    }
]
kyrax.write_excel_turbo("chart_pivot.xlsx", sheets)
```

Chart types include bar/column, line, pie, scatter, and area. Axis titles are
accepted but not rendered. Image input detects PNG, JPEG, and GIF from their
signatures; PNG chunk framing is checked, but image data is not decoded or fully
validated. Adding a chart or image to a loaded sheet that already contains
drawings is refused.

## Validation rules

The current source validates the following inputs before writing:

- **Sheet names:** non-empty, at most 31 characters, no `: \ / ? * [ ]` or
  control characters, no leading/trailing apostrophe, not `History`, and
  case-insensitively unique.
- **Table names:** start with a letter, `_`, or `\`; remaining characters are
  alphanumeric, `_`, or `.`; not a cell reference; at most 255 characters; unique
  case-insensitively. Table references need a header row and at least one data
  row, and column counts must match the reference width.
- **Numbers:** Python `int` and `decimal.Decimal` values must be exactly
  representable as a double; non-finite values are rejected. NumPy integer
  scalars are not covered by the same exactness check in every path.
- **Dates and times:** timezone-aware `datetime`/`time` values are rejected;
  convert explicitly first.
- **Text:** illegal XML control characters are rejected. Strings longer than
  32,767 characters are truncated with a `UserWarning`.
- **Formulas:** zero-based coordinates must be inside the Excel grid; cached
  values follow the `cached`/`emit_cached_values` rules above.
- **Data validation:** `type`, `operator`, `errorStyle`, `imeMode`, and `sqref`
  are checked.
- **Workbook metadata:** `active_tab` must be inside the sheet count; at least
  one sheet must be visible; defined-name `local_sheet_id` values must be in
  range.
- **Streaming rows:** each row must be a sequence of cell values; bare strings,
  bytes, and mappings are rejected.

## Streaming

### Write streaming

```python
import kyrax

def rows():
    yield ["Index", "Value"]
    for index in range(100_000):
        yield [index, index * 2]

kyrax.write_excel_turbo_stream(
    "large.xlsx",
    [{"name": "Data", "rows": rows()}],
    string_mode="inline",
)
```

Plain lazy rows are consumed once. Inline strings use bounded row/chunk cell
memory; `string_mode="sst"` retains unique strings. Feature-rich sheets and
unsupported option combinations may use the eager cell model before streaming
their XML. Lazy output omits the optional worksheet dimension. A sibling
temporary archive preserves the destination if a row generator fails late.
Zip64 records are emitted when the archive crosses the 4 GB / 65,535-entry
limits.

### Read streaming

```python
import kyrax

stream = kyrax.read_excel_turbo_iter("large.xlsx", sheet_idx=0, chunk_size=10_000)
try:
    for batch in stream:
        print(batch.num_rows)
finally:
    stream.close()

rows = kyrax.read_excel_turbo_iter_rows(
    "large.xlsx", sheet_idx=0, min_row=1, max_row=100, data_only=True
)
try:
    for row in rows:
        print(row)
finally:
    rows.close()

value = kyrax.read_excel_turbo_cell("large.xlsx", 0, row=1, column=1)
```

`read_excel_turbo_iter` yields PyArrow `RecordBatch` chunks and requires PyArrow.
`read_excel_turbo_iter_rows` yields Python value tuples and does not require
PyArrow. Row one is ordinary worksheet data. `data_only=True` selects cached
formula results. `read_excel_turbo_cell` reads one 1-based cell. `close_streams`
closes a list of streaming readers in one call.

`load_workbook(read_only=True)` wraps the row stream:

```python
import kyrax

with kyrax.load_workbook("large.xlsx", read_only=True, data_only=True) as wb:
    ws = wb.active
    for row in ws.iter_rows(values_only=True):
        print(row)
```

Read-only `iter_rows` supports `values_only=True` only. Cell proxies are not
available in read-only mode. Read-only mode loads shared strings, styles, and
formula anchors into memory; worksheet headers and individual rows are limited
to 16 MiB each.

## Grid and Arrow

### NumPy grid fast path

```python
import numpy as np
import kyrax

grid = np.ascontiguousarray([[1.0, 2.5], [3.0, 4.0]], dtype=np.float64)
kyrax.write_excel_turbo("grid.xlsx", [{"name": "Data", "grid": grid}])
```

The grid fast path requires a single sheet, a 2-D C-contiguous `float32` or
`float64` array, and native byte order. It rejects infinity, masked arrays, and
every metadata key except `name`, `grid`, `visibility`, and `state`. NaN becomes
a blank cell. Use `rows` or `columns` when you need formatting or structure.

### Arrow tables

```python
import pyarrow as pa
import kyrax

table = pa.table({"name": ["Apple", "Pear"], "count": [2, 3]})
kyrax.write_excel_turbo("arrow.xlsx", [{"name": "Data", "table": table}])
```

The `table` key accepts an Arrow Table or RecordBatch-like object. Primitive
Float64, Float32, Int64, Int32, Boolean, String, and LargeString columns are
converted directly; other types fall back to Python values. Int64 values must be
exactly representable as a double, and non-finite floats are rejected. A struct
column with a `value` field is unwrapped to that value; a struct without `value`
raises `TypeError`. This can silently reinterpret structured data, so convert
unsupported Arrow types explicitly.

## pandas

```python
import pandas as pd
import kyrax

frame = pd.DataFrame({"item": ["Apple"], "price": [2.5]})
frame.to_excel("pandas.xlsx", engine="kyrax", index=False)

read_back = pd.read_excel("pandas.xlsx", engine="kyrax")
```

The pandas writer buffers cells and uses the turbo bytes writer; it is not a
streaming writer. It supports `mode="w"` only. Freeze panes are supported. The
current source forwards pandas 3.0+ `autofilter`; older builds may ignore it.
Error cells are surfaced as error-code strings, so pandas cannot distinguish a
real error cell from a literal string with the same text. Use the turbo
`cell_errors()` API when that distinction matters.

The pandas reader delegates parsing to pandas and uses kyrax for worksheet
rows. Current tests cover `header`, `skiprows`, `usecols`, `converters`,
`na_values`, `dtype`, `na_filter`, and `freeze_panes`; `BytesIO` input also
works in this checkout. Other pandas options and versions are not exhaustively
verified.

`ExcelSheet.to_pandas()` converts through PyArrow and therefore requires the
`pyarrow` extra. `ExcelSheet.to_polars()` requires Polars.

## CSV and JSON

All functions live in `kyrax.io`.

| Function | Purpose |
|---|---|
| `csv_to_sheet(csv_path, xlsx_out, sheet_name="Sheet1", ...)` | Import a CSV file into a new single-sheet workbook. |
| `csv_bytes_to_sheet(csv_bytes, xlsx_out, ...)` | Import in-memory CSV bytes. |
| `sheet_to_csv(path, sheet, out_path, ...)` | Export one worksheet to a CSV file. |
| `sheet_to_csv_bytes(path, sheet, ...)` | Export one worksheet to CSV bytes. |
| `json_to_sheet(json_path, xlsx_out, sheet_name="Sheet1", ...)` | Import JSON/NDJSON into a new workbook. |
| `json_bytes_to_sheet(json_bytes, xlsx_out, ...)` | Import in-memory JSON/NDJSON bytes. |
| `sheet_to_json(path, sheet, out_path, ...)` | Export one worksheet to JSON/NDJSON. |
| `sheet_to_json_bytes(path, sheet, ...)` | Export one worksheet to JSON/NDJSON bytes. |

CSV import accepts a leading UTF-8 BOM and CRLF, LF, or bare CR line endings.
`infer_types=False` is the default, so values such as `"007"` stay text. Invalid
UTF-8 is rejected. JSON import supports `records`, `columns`, and `ndjson`
shapes. Heterogeneous record keys form a first-seen union; nested objects and
arrays become their raw JSON text; integers beyond 2^53 are kept as digit
strings; non-finite numbers are rejected. Trailing content and trailing commas
are rejected. `has_header` is accepted for API symmetry, but JSON import always
writes a header row because a grid has no unlabelled state.

CSV dates use Excel format tokens; JSON dates use strftime tokens, with an empty
format meaning ISO 8601. JSON `1e-400` underflows to `0.0`; this policy is not
configurable.

## Formulas

```python
from kyrax import formulas

print(formulas.evaluate("=SUM(1,2)"))
print(formulas.evaluate("A1*2", {"A1": 21}))
print(formulas.evaluate("=1/0"))
print(formulas.dependencies("=SUM(A1:B2)+C5"))
print(len(formulas.list_functions()))
```

`evaluate` accepts a formula with or without a leading `=`. The optional context
maps A1 cell references to values; missing cells read as blank. Results are
floats for numbers, strings for text and error codes, booleans, `None` for
blank, and nested lists for array results. Parse errors raise `ValueError`;
unknown functions return an Excel error such as `#NAME?`.

`dependencies` returns sorted, deduplicated static A1 references. Sheet names
are quoted when Excel requires it. Dynamic references such as `INDIRECT` and
`OFFSET` cannot be enumerated statically and are omitted.

`recalculate(sheets)` returns XLSX bytes. It accepts the same sheet-dictionary
schema as `write_excel_turbo` and writes computed cached values for formulas the
engine can evaluate. Unsupported, external, or circular formulas are left
uncomputed and the workbook keeps `fullCalcOnLoad="1"` so a spreadsheet
application can fill them on open.

The formula context accepts `Decimal` and arbitrary `__float__` values without
the exactness check used by the cell writer. Convert such values explicitly if
exactness matters.

## Encryption

```python
import kyrax

print(kyrax.is_encrypted("maybe_encrypted.xlsx"))

reader = kyrax.read_excel_turbo("encrypted.xlsx", password="secret")
```

`is_encrypted` detects an ECMA-376 encrypted workbook without a password and
never raises. `read_excel_turbo(path, password=...)` opens an encrypted workbook
when the build includes encryption support; a plain workbook ignores the
password. `encryption_info(path)` reports the scheme and algorithms without a
password, but requires a build with the `encryption` feature and raises
`KyraxError` otherwise. `load_workbook` has no password parameter.

## Error handling

Kyrax exceptions derive from `KyraxError`:

| Exception | Meaning |
|---|---|
| `KyraxError` | Base class for all kyrax errors. |
| `UnsupportedColumnTypeCombinationError` | A column contains an unsupported type combination. |
| `CannotRetrieveCellDataError` | Data for a given cell cannot be retrieved. |
| `CalamineCellError` | Calamine reported an error about cell content. |
| `CalamineError` | Generic calamine error. |
| `SheetNotFoundError` | Sheet was not found. |
| `ColumnNotFoundError` | Column was not found. |
| `ArrowError` | Generic Arrow error. |
| `InvalidParametersError` | Provided parameters are invalid. |

Built-in exceptions are also used: `ValueError` for invalid values and parse
errors, `TypeError` for unsupported argument types, `KeyError` for a missing
sheet in an editable workbook, `IndexError` for an out-of-range sheet index,
`NotImplementedError` for unsupported read-only cell proxies, and `ImportError`
for missing optional dependencies.

`validate_excel(path)` returns a report and does not raise for a bad input. The
report contains `valid`, `errors`, `warnings`, `infos`, and `findings`; each
finding has `code`, `severity`, `part`, `location`, `message`, and `repairable`.
`repair_excel(path, out_path, severity="warning")` writes a separate output only
for repairable findings and reports every action. The source file is never
modified.

## Next steps

- [API reference](api-reference.md) for signatures and return types.
- [Limitations](limitations.md) for verified gaps.
- [Migration from openpyxl](migration-from-openpyxl.md) for familiar API
  differences.
