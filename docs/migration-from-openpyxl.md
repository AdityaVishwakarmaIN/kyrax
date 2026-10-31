# Migration from openpyxl

Kyrax provides an openpyxl-style workbook API for common create, edit, and read
workflows. This guide covers the familiar parts, the differences that matter,
and the options that are not supported.

## What stays familiar

```python
import kyrax
from kyrax.styles import Font, PatternFill, Side, Border, Alignment

wb = kyrax.Workbook()
ws = wb.active
ws.title = "Summary"

ws["A1"] = "Quarterly Revenue"
ws["A1":"C1"] = [["Quarterly Revenue", 10500.5, True]]
ws["A1"].font = Font(name="Calibri", size=14, bold=True, color="0070C0")
ws["A1"].fill = PatternFill(fill_type="solid", start_color="FFFFE0")
ws["A1"].alignment = Alignment(horizontal="center", vertical="center")

thin = Side(style="thin", color="000000")
ws["A1"].border = Border(left=thin, right=thin, top=thin, bottom=thin)

ws.merge_cells("A1:C1")
ws.freeze_panes = "A2"
ws.tab_color = "0070C0"
wb.save("summary.xlsx")
```

Familiar pieces include:

- `Workbook()`, `load_workbook()`, `wb.active`, `wb.sheetnames`,
  `wb.worksheets`, `wb[name]`, `wb.create_sheet`, `wb.remove`,
  `wb.copy_worksheet`, `wb.move_sheet`, and `wb.save`.
- `ws["A1"]`, `ws["A1"].value`, `ws.cell(row, column, value)`, `ws.append`,
  `ws.iter_rows`, `ws.iter_cols`, `ws.rows`, `ws.columns`, and `ws.values`.
- `ws.merge_cells`, `ws.unmerge_cells`, `ws.freeze_panes`, `ws.tab_color`,
  `ws.auto_filter.ref`, `ws.column_dimensions["A"].width`, and
  `ws.row_dimensions[1].height`.
- `Font`, `PatternFill`, `Side`, `Border`, `Alignment`, `Protection`, and
  `Comment` from `kyrax.styles`.
- `DataValidation`, `Table`, and `TableStyleInfo` from `kyrax.worksheet`.
- Conditional-formatting rules from `kyrax.formatting`.
- Charts from `kyrax.chart` and images from `kyrax.drawing.image`.

## Differences that matter

### Row access: `ws[3]`

Current source supports `ws[3]` and returns a tuple of cells for the 1-based
row:

```python
row = ws[3]
print([cell.coordinate for cell in row])
print([cell.value for cell in row])
```

Older builds may raise `TypeError: sheet key must be a coordinate string or
slice`. The portable fallback is:

```python
row = next(ws.iter_rows(min_row=3, max_row=3))
print([cell.value for cell in row])
```

`ws["A1:B2"]` returns a tuple of row tuples of cells. `ws["A1"]` returns a
single cell.

### Read-only and data-only

`load_workbook` defaults to editable mode. The flags change the return type:

```python
editable = kyrax.load_workbook("book.xlsx")
read_only = kyrax.load_workbook("book.xlsx", read_only=True)
cached = kyrax.load_workbook("book.xlsx", data_only=True)
reader = kyrax.load_workbook("book.xlsx", edit_mode=False)
```

- `read_only=True` returns a `ReadOnlyWorkbook` backed by forward streaming.
  `iter_rows(values_only=True)` is the supported row API; cell proxies are not
  available. Use it as a context manager or call `close()`.
- `data_only=True` returns cached formula results. Missing caches are not
  calculated while reading. `data_only=False` returns formula text.
- `edit_mode=False` returns a `TurboReader`, not an editable workbook.
- `load_workbook` accepts a path, `bytes`, `bytearray`, `memoryview`, or a binary
  file-like object. Streams are read from their current position and remain
  open.

Read-only mode loads shared strings, styles, and formula anchors into memory;
worksheet headers and individual rows are limited to 16 MiB each.

### Error cells

pandas and the standard reader surface error cells as error-code strings such as
`#DIV/0!`. A real error cell and a literal string with the same text are not
distinguishable through pandas. Use the turbo API when that distinction matters:

```python
sheet = kyrax.read_excel_turbo("book.xlsx").load_sheet(0, features=["values"])
errors = sheet.cell_errors()
print(errors.to_pydict())
```

### Formula calculation

`wb.save()` does not calculate formulas. It stores formula text and any cached
values that already exist. To read cached results, use `data_only=True`. To
compute formulas while writing with the turbo writer, use `recalculate=True` or
`kyrax.formulas.recalculate(sheets)`. Formulas the engine cannot evaluate are
left uncomputed and the workbook requests full calculation on open.

### Structural edits

Structural edits work on new and loaded workbooks, but some metadata is
conservative:

- Existing comments block structural edits.
- Structural edits that change an existing table's column count are refused.
- Structural edits after adding a pending table are refused.
- Chart and image anchors do not follow row/column inserts.
- Some metadata getters only reflect pending edits; verify saved metadata with
  an independent reader.

See [Limitations](limitations.md) for the full list.

### Unsupported or limited options

- `load_workbook` rejects unsupported keyword arguments on the editable and
  read-only paths with `TypeError`.
- `read_only=True` does not support cell proxies; use `values_only=True`.
- `read_excel_turbo(...).load_sheet` supports only `header_row=0` and
  `header_row=None`.
- The pandas writer supports `mode="w"` only.
- Chart axis titles are accepted but not rendered.
- Adding a chart or image to a loaded sheet that already contains drawings is
  refused.
- `wb.properties` is read-only; assigning a property raises `AttributeError`.
- `ws[3]` is supported in current source but not in older builds.
- The type stub may lag runtime behavior; see the [API reference](api-reference.md).

### pandas

Kyrax registers a pandas Excel engine:

```python
import pandas as pd
import kyrax

pd.DataFrame({"item": ["Apple"], "price": [2.5]}).to_excel(
    "pandas.xlsx", engine="kyrax", index=False
)
frame = pd.read_excel("pandas.xlsx", engine="kyrax")
```

The writer buffers cells and supports `mode="w"` only. Error cells are strings.
Freeze panes are supported. The current source forwards pandas 3.0+ `autofilter`;
older builds may ignore it.

## Migration checklist

1. Replace `openpyxl.load_workbook` with `kyrax.load_workbook` for the familiar
   API, or `kyrax.read_excel` for tabular data.
2. Replace `openpyxl.Workbook` with `kyrax.Workbook`.
3. Import styles from `kyrax.styles`; import `DataValidation`, `Table`, and
   `TableStyleInfo` from `kyrax.worksheet`.
4. Use `ws.iter_rows(min_row=3, max_row=3)` if you need to support older builds
   that do not implement `ws[3]`.
5. Use `data_only=True` for cached formula results and `recalculate=True` when
   writing formulas that should carry computed values.
6. Use the turbo `cell_errors()` API when error cells must be distinguished from
   literal strings.
7. Save and reload before adding tables or drawings if you also need structural
   edits.
8. Validate important output with `kyrax.validate_excel` and an independent
   reader.
