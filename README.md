# `kyrax`

A fast Excel file reader and writer for Python and Rust.

Kyrax reads, writes, and edits Excel workbooks from Python. The core is Rust;
the public interface is Python-only. There is no CLI, no console script, and
`python -m kyrax` does not work.

## Documentation

- [Documentation home](docs/README.md)
- [Quickstart](docs/quickstart.md)
- [Input guide](docs/input-guide.md)
- [API reference](docs/api-reference.md)
- [Limitations](docs/limitations.md)
- [Migration from openpyxl](docs/migration-from-openpyxl.md)
- [Coverage](docs/coverage.md)
- [Runnable examples](examples/README.md)
- Python API types: [`python/kyrax/_kyrax.pyi`](./python/kyrax/_kyrax.pyi)
- Rust: `cargo doc --open -p kyrax`

## Stability

The Python API is mostly stable, and we avoid breaking changes as much as
possible. The free-threaded build is still considered experimental. The Rust
crate is still experimental, and breaking changes are to be expected.

## Installation

```bash
# Lightweight installation (no PyArrow dependency)
pip install kyrax

# With Polars support only (no PyArrow needed)
pip install kyrax[polars]

# With Pandas support (includes PyArrow)
pip install kyrax[pandas]

# With PyArrow support
pip install kyrax[pyarrow]

# With all integrations
pip install kyrax[pandas,polars]
```

## Quick start

The snippets below build on each other: they create the files they read and
write their output to the current directory.

### Create, edit, and read a workbook

```python
import kyrax

wb = kyrax.Workbook()
ws = wb.active
ws.title = "Sales"
ws.append(["Item", "Price"])
ws.append(["Apple", 2.5])
ws["A3"] = "Pear"
ws["B3"] = 3
ws["C2"] = "=B2*2"
ws.freeze_panes = "A2"
wb.save("sales.xlsx")

loaded = kyrax.load_workbook("sales.xlsx")
print(loaded["Sales"]["A2"].value)  # Apple
print(loaded["Sales"]["C2"].value)  # =B2*2
loaded.close()
```

Saving does not calculate formulas. Use `data_only=True` to read cached formula
results, or `recalculate=True` with the turbo writer to compute supported
formulas while writing.

### Read tabular data

```python
import kyrax

reader = kyrax.read_excel("sales.xlsx")
print(reader.sheet_names)  # ['Sales']
sheet = reader.load_sheet("Sales", use_columns="A:B", n_rows=10)
print(sheet.height, sheet.width)
print([column.name for column in sheet.selected_columns])
```

`read_excel` accepts a path or bytes and is calamine-backed; tests exercise
XLSX, legacy XLS, and ODS fixtures. The turbo reader, turbo writer, and editable
API target XLSX/XLSM.

### Write with the turbo writer

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

`rows` includes any header row. Do not mix `rows` with `columns`/`data`/`table`
or `grid` in the same sheet. See the
[input guide](docs/input-guide.md#turbo-writer-sheet-dictionaries) for the sheet
dictionary schema.

### Stream rows

```python
import kyrax

def rows():
    yield ["Index", "Square"]
    for index in range(100_000):
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

### Read-only streaming

```python
import kyrax

with kyrax.load_workbook("streamed.xlsx", read_only=True) as wb:
    ws = wb.active
    for row in ws.iter_rows(values_only=True):
        print(row)
```

Read-only mode is forward streaming. `iter_rows(values_only=True)` is the
supported row API; cell proxies are not available. Read-only mode loads shared
strings, styles, and formula anchors into memory; worksheet headers and
individual rows are limited to 16 MiB each.

### pandas and Arrow

```python
import pandas as pd
import kyrax

frame = pd.DataFrame({"item": ["Apple"], "price": [2.5]})
frame.to_excel("pandas.xlsx", engine="kyrax", index=False)
print(pd.read_excel("pandas.xlsx", engine="kyrax"))

reader = kyrax.read_excel("pandas.xlsx")
sheet = reader.load_sheet(0)
print(sheet.to_pandas())  # requires the pandas/pyarrow extra
```

The pandas writer buffers cells and supports `mode="w"` only. Error cells are
surfaced as error-code strings; use the turbo `cell_errors()` API when you need
to distinguish a real error from a literal string.

### CSV, JSON, and formulas

```python
from kyrax.io import csv_bytes_to_sheet, json_bytes_to_sheet
from kyrax import formulas

csv_bytes_to_sheet(b"name,count\nApple,2\n", "from_csv.xlsx", infer_types=True)
json_bytes_to_sheet(b'[{"name":"Apple","count":2}]', "from_json.xlsx")

print(formulas.evaluate("=SUM(1,2)"))          # 3.0
print(formulas.evaluate("A1*2", {"A1": 21}))   # 42.0
print(formulas.dependencies("=SUM(A1:B2)+C5"))
```

## Feature overview

| Area | What you can do | Entry points |
|---|---|---|
| Read | Standard tabular read, selective turbo feature read, read-only streaming | `read_excel`, `read_excel_turbo`, `load_workbook(read_only=True)` |
| Write | Rows, columns, NumPy grid, bytes, streaming | `write_excel_turbo`, `write_excel_turbo_bytes`, `write_excel_turbo_stream` |
| Edit | openpyxl-style workbook, cells, styles, merges, tables, validation, conditional formatting, charts, images | `Workbook`, `load_workbook`, `edit_excel` |
| pandas | `engine="kyrax"` reader and writer | `kyrax[pandas]` |
| CSV/JSON | Import and export | `kyrax.io` |
| Formulas | Evaluate, inspect dependencies, recalculate | `kyrax.formulas` |
| Validate/repair | Structured reports and conservative repair | `validate_excel`, `repair_excel` |
| Encryption | Detect encrypted workbooks; inspect/read when built with the encryption feature | `is_encrypted`, `encryption_info`, `read_excel_turbo` |

## Key features

- **Familiar workbook API**: `load_workbook`, `Workbook`, `Font`,
  `PatternFill`, `Border`, `Side`, `Alignment`, `Protection`, `Comment`,
  `merge_cells`, `freeze_panes`, tables, data validation, conditional
  formatting, charts, and images.
- **Zero-copy data exchange** via the
  [Arrow PyCapsule Interface](https://arrow.apache.org/docs/format/CDataInterface/PyCapsuleInterface.html)
  into Polars and other Arrow-compatible libraries.
- **Turbo engine**: selective XLSX feature reading (`read_excel_turbo`),
  declarative writing (`write_excel_turbo`), and streaming
  (`write_excel_turbo_stream`).
- **Byte-preserving edit mode** for untouched package parts, macros, and styles.
- **Standalone formula evaluation** with `kyrax.formulas`.
- **Validation and repair** with `validate_excel` and `repair_excel`.
- **Rust core** with Python bindings and thin adapters.

## Limitations

Kyrax is not a complete openpyxl replacement and does not claim exhaustive
nested-schema or Microsoft Excel interoperability. Verified limitations include
pandas error cells as strings, pandas `mode="w"` only, structural metadata
refusals, external-link/property/named-style gaps, Arrow `value` struct
unwrapping, `Fraction`/custom `__float__` coercion, JSON underflow, table
header/name rules, and the absence of a CLI. See
[Limitations](docs/limitations.md) for the full list and
[Coverage](docs/coverage.md) for test evidence.

## Contributing & Development

### Prerequisites

You'll need:

1. **[Rust](https://rustup.rs/)** - Rust stable or nightly
2. **[uv](https://docs.astral.sh/uv/getting-started/installation/)** - Fast Python package manager (will install Python 3.10+ automatically)
3. **[git](https://git-scm.com/)** - For version control
4. **[make](https://www.gnu.org/software/make/)** - For running development commands

**Python Version Management:**

uv handles Python installation automatically. To use a specific Python version:

```bash
uv python install 3.13  # Install Python 3.13
uv python pin 3.13      # Pin project to Python 3.13
```

### Quick Start

```bash
# Clone the repository (or from your fork)
git clone <repository-url>
cd kyrax

# First-time setup: install dependencies, build debug version, and setup pre-commit hooks
make setup-dev
```

Verify your installation by running:

```bash
make
```

This runs a full development cycle: formatting, building, linting, and testing.

### Development Commands

Run `make help` to see all available commands, or use these common ones:

```bash
make all          # full dev cycle: format, build, lint, test
make install      # install with debug build (daily development)
make install-prod # install with release build (benchmarking)
make test         # to run the tests
make lint         # to run the linter
make format       # to format python and rust code
```

### Useful Resources

- [`python/kyrax/_kyrax.pyi`](./python/kyrax/_kyrax.pyi) - Python API types
- [`python/tests/`](./python/tests) - Comprehensive usage examples
- [`docs/`](./docs) - User documentation
- [`examples/`](./examples) - Self-contained runnable examples

## Benchmarking

For benchmarking, use `make benchmarks` which automatically builds an optimised
wheel. This is required for profiling, as dev mode builds are much slower.

### Speed benchmarks

```bash
make benchmarks
```

### Memory profiling

```bash
mprof run -T 0.01 python python/tests/benchmarks/memory.py python/tests/benchmarks/fixtures/plain_data.xls
```

## Creating a release

1. Create a PR containing a commit that only updates the version in `Cargo.toml`.
2. Once it is approved, squash and merge it into main.
3. Tag the squashed commit, and push it.
4. The `release` GitHub action will take care of the rest.

## Dev tips

- Use `cargo check` to verify that your Rust code compiles; no need to go
  through `maturin` every time.
- `cargo clippy` is useful for linting.
- Careful with Arrow constructors; they tend to allocate a lot.
- [`mprof`](https://github.com/pythonprofilers/memory_profiler) and `time` go a
  long way for perf checks; no need to go fancy right from the start.

Turbo sheet dictionaries use either `rows`/`rows_iter` or
`columns`/`data`/`table`, not both. Unknown sheet keys raise an error.
`cell_styles` coordinates are zero-based in both the `(row, col)` map and the
`{row, col, style}` list forms. If migrating a list that used one-based
coordinates, subtract one from each row and column. Row dimensions use one-based
`row` or zero-based `row_idx`; column dimensions use one-based `col` or
`min`/`max`.

Turbo image input detects PNG, JPEG, and GIF formats from their signatures. PNG
inputs also require complete chunk framing, but image data is not decoded or
fully validated. Supply valid images, since spreadsheet readers may discard
malformed data.
