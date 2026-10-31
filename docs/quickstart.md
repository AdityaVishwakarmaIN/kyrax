# Quickstart

The examples on this page build on each other: they create the workbooks they
read and write their output to the current directory. No external fixtures are
required.

## Install

```bash
pip install kyrax
```

Optional integrations:

```bash
pip install "kyrax[pandas]"   # pandas + PyArrow
pip install "kyrax[pyarrow]"  # PyArrow only
pip install "kyrax[polars]"   # Polars only
```

Kyrax is a Python-only interface. There is no CLI and `python -m kyrax` does not
work.

## Create, edit, and save a workbook

```python
import kyrax

wb = kyrax.Workbook()
ws = wb.active
ws.title = "Sales"

ws.append(["Item", "Price", "In stock"])
ws.append(["Apple", 2.5, True])
ws.append(["Pear", 3.0, False])

ws["D1"] = "Total"
ws["D2"] = "=B2+B3"
ws.freeze_panes = "A2"
wb.save("quickstart.xlsx")
```

`Workbook()` creates an editable workbook with one active sheet. Cell
coordinates such as `"D2"` and `ws.cell(row, column)` are 1-based. Saving does
not calculate formulas; `"=B2+B3"` is stored as formula text.

## Read it back

```python
import kyrax

wb = kyrax.load_workbook("quickstart.xlsx")
ws = wb["Sales"]
print(ws["A2"].value)          # Apple
print(ws["D2"].value)          # =B2+B3
print([c.value for c in ws[3]])  # ['Pear', 3.0, False, None]
wb.close()
```

`load_workbook` defaults to editable mode. Use `read_only=True` for forward
streaming and `data_only=True` to read cached formula results instead of formula
text.

## Read tabular data

```python
import kyrax

reader = kyrax.read_excel("quickstart.xlsx")
print(reader.sheet_names)  # ['Sales']

sheet = reader.load_sheet("Sales", use_columns="A:B", n_rows=10)
print(sheet.height, sheet.width)
print([column.name for column in sheet.selected_columns])
```

`read_excel` returns an `ExcelReader`. `load_sheet` accepts a zero-based sheet
index or a sheet name. `header_row=0` is the default; `header_row=None` treats
the first row as data. `use_columns` accepts names, zero-based indices, Excel
range strings such as `"A:B"`, or a callable.

## Write with the turbo writer

```python
import kyrax

sheets = [
    {
        "name": "Sales",
        "rows": [["Item", "Price"], ["Apple", 2.5], ["Pear", 3.0]],
        "freeze_panes": "A2",
        "auto_filter": "A1:B3",
    }
]
kyrax.write_excel_turbo("turbo.xlsx", sheets)

data = kyrax.write_excel_turbo_bytes(sheets)
with open("turbo_bytes.xlsx", "wb") as output:
    output.write(data)
```

`rows` is an iterable of row iterables and includes any header row. Do not mix
`rows` with `columns`/`data`/`table` or `grid` in the same sheet.

## Stream a large sheet

```python
import kyrax

def rows():
    yield ["Index", "Square"]
    for index in range(1000):
        yield [index, index * index]

kyrax.write_excel_turbo_stream(
    "streamed.xlsx",
    [{"name": "Squares", "rows": rows()}],
    string_mode="inline",
)
```

The streaming writer consumes a plain row generator once. Inline strings use
bounded row/chunk cell memory; `string_mode="sst"` retains unique strings.
Feature-rich sheets may use an eager fallback. Lazy output omits the optional
worksheet dimension, so some readers may not know the row count until scanning.

## Read a large sheet without loading it all

```python
import kyrax

with kyrax.load_workbook("streamed.xlsx", read_only=True) as wb:
    ws = wb["Squares"]
    for row in ws.iter_rows(values_only=True):
        print(row)
```

Read-only mode is forward streaming. `iter_rows(values_only=True)` is the
supported row API; cell proxies are not available in read-only mode.

## pandas

```python
import pandas as pd
import kyrax

frame = pd.DataFrame({"item": ["Apple", "Pear"], "price": [2.5, 3.0]})
frame.to_excel("pandas.xlsx", engine="kyrax", index=False)

read_back = pd.read_excel("pandas.xlsx", engine="kyrax")
print(read_back)
```

The pandas writer buffers cells and supports `mode="w"` only. Error cells are
surfaced as error-code strings; use the turbo `cell_errors()` API when you need
to distinguish a real error from a literal string.

## CSV and JSON

```python
from kyrax.io import csv_bytes_to_sheet, json_bytes_to_sheet, sheet_to_csv_bytes

csv_bytes_to_sheet(
    b"name,count\nApple,2\nPear,3\n",
    "from_csv.xlsx",
    infer_types=True,
)
json_bytes_to_sheet(
    b'[{"name":"Apple","count":2}]',
    "from_json.xlsx",
)

print(sheet_to_csv_bytes("from_csv.xlsx", "Sheet1").decode())
```

`kyrax.io` also provides path-based import/export and JSON export in
`records`, `columns`, and `ndjson` shapes.

## Formulas

```python
from kyrax import formulas

print(formulas.evaluate("=SUM(1,2)"))                 # 3.0
print(formulas.evaluate("A1*2", {"A1": 21}))          # 42.0
print(formulas.evaluate("=1/0"))                      # #DIV/0!
print(formulas.dependencies("=SUM(A1:B2)+C5"))        # ['A1:B2', 'C5']
```

`formulas.recalculate(sheets)` returns XLSX bytes with computed cached values
for formulas the engine can evaluate. Unsupported, external, or circular
formulas are left uncomputed and the workbook requests full calculation on open.

## Validate and repair

```python
import kyrax

report = kyrax.validate_excel("quickstart.xlsx")
print(report["valid"])

result = kyrax.repair_excel("quickstart.xlsx", "quickstart_repaired.xlsx")
print(result["wrote_output"], result["actions"])
```

`validate_excel` returns a structured report and does not raise for a bad input.
`repair_excel` only fixes repairable findings at the requested severity and
never modifies the source file.

## Next steps

- [Input guide](input-guide.md) for the full input surface and coordinate rules.
- [API reference](api-reference.md) for signatures and return types.
- [Limitations](limitations.md) for verified gaps.
- [Examples](../examples/README.md) for complete runnable scripts.
