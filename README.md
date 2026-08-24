# `kyrax`

[![Docs](https://img.shields.io/badge/docs-GitHub%20Pages-blue.svg)](https://adityamukherjee-99.github.io/kyrax/)

A fast excel file reader for Python and Rust.

Docs:
 * Python (local: `make doc-serve`)
 * Rust (local: `cargo doc --open -p kyrax`)

## Stability

The Python library is considered production-ready. The API is mostly stable, and we avoid breaking changes as much as
possible.

> ⚠️ The free-threaded build is still considered experimental

The Rust crate is still experimental, and breaking changes are to be expected.

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

## Quick Start

### Modern usage (recommended)

kyrax supports the [Arrow PyCapsule Interface](https://arrow.apache.org/docs/format/CDataInterface/PyCapsuleInterface.html) for zero-copy data exchange with libraries like Polars, without requiring pyarrow as a dependency.
Use kyrax with any Arrow-compatible library without requiring pyarrow.

```python
import kyrax

# Load an Excel file
reader = kyrax.read_excel("data.xlsx")
sheet = reader.load_sheet(0)  # Load first sheet

# Use with Polars (zero-copy, no pyarrow needed)
import polars as pl
df = pl.DataFrame(sheet)  # Direct PyCapsule interface
print(df)

# Or use the to_polars() method (also via PyCapsule)
df = sheet.to_polars()
print(df)

# Or access the raw Arrow data via PyCapsule interface
schema = sheet.__arrow_c_schema__()
array_data = sheet.__arrow_c_array__()
```

### Traditional usage (with pandas/pyarrow)

```python
import kyrax

reader = kyrax.read_excel("data.xlsx")
sheet = reader.load_sheet(0)

# Convert to pandas (requires `pandas` extra)
df = sheet.to_pandas()

# Or get pyarrow RecordBatch directly
record_batch = sheet.to_arrow()
```

### Working with tables

```python
reader = kyrax.read_excel("data.xlsx")

# List available tables
tables = reader.table_names()
print(f"Available tables: {tables}")

# Load a specific table
table = reader.load_table("MyTable")
df = pl.DataFrame(table)  # Zero-copy via PyCapsule, no pyarrow needed
```

### Turbo Read (high-performance XLSX reader)

`read_excel_turbo` provides selective feature parsing for high-throughput XLSX loading:

```python
import kyrax

# Open workbook for turbo reading
reader = kyrax.read_excel_turbo("data.xlsx")

# Selective loading: values, formulas, styles, merges, comments, etc.
sheet = reader.load_sheet("Sheet1", features=["values", "styles", "formulas"])

# Access Arrow columns, cell errors, style indices, formulas
arrow_data = sheet.to_arrow()
styles = sheet.style_indices()
formulas = sheet.formulas()
```

### Turbo Write & Streaming Export

Declarative, high-speed XLSX writing and streaming export:

```python
import kyrax
import numpy as np

# Declarative write from sheet dicts or NumPy float grid fast lane
arr = np.array([[1.0, 2.5], [3.0, 4.25]], dtype=np.float64)
kyrax.write_excel_turbo("output.xlsx", [{"name": "Data", "grid": arr}])

# Streaming write for large datasets.
# NOTE: "columns" takes columnar DATA — a list of column arrays, not header
# names. Headers go in the first entry of "rows".
kyrax.write_excel_turbo_stream(
    "large_output.xlsx",
    [{"name": "Sheet1", "columns": [[1.0, 2.0, 3.0], ["a", "b", "c"]]}]
)
```

### Openpyxl Drop-in Replacement

`kyrax` provides a drop-in replacement for openpyxl workflows with full style objects, cell mutation, merge/unmerge, sheet controls, and 10x-50x speedups:

```python
import kyrax
from kyrax.styles import Font, PatternFill, Side, Border, Alignment

# Create or load workbook
wb = kyrax.Workbook()  # or kyrax.load_workbook("financials.xlsx")
ws = wb.active
ws.title = "Summary"

# Cell assignment and 2D slicing
ws["A1"] = "Quarterly Revenue"
ws["A1":"C1"] = [["Quarterly Revenue", 10500.5, True]]

# Styling with standard openpyxl classes
ws["A1"].font = Font(name="Calibri", size=14, bold=True, color="0070C0")
ws["A1"].fill = PatternFill(fill_type="solid", start_color="FFFFE0")
ws["A1"].alignment = Alignment(horizontal="center", vertical="center")

thin = Side(style="thin", color="000000")
ws["A1"].border = Border(left=thin, right=thin, top=thin, bottom=thin)

# Merging and sheet controls
ws.merge_cells("A1:C1")
ws.freeze_panes = "A2"
ws.tab_color = "0070C0"

# Save with byte-preservation
wb.save("summary.xlsx")
```

### High-Throughput Read-Only Ingestion

```python
import kyrax

# Fast read-only iterator backed by Rust turbo engine
wb = kyrax.load_workbook("huge_dataset.xlsx", read_only=True)
ws = wb.active

for row in ws.iter_rows(values_only=True):
    pass
```

## Key Features

- **Openpyxl drop-in replacement**: `load_workbook`, `Workbook`, `Font`, `PatternFill`, `Border`, `Side`, `Alignment`, `Protection`, `Comment`, `merge_cells`, `freeze_panes`.
- **Zero-copy data exchange** via [Arrow PyCapsule Interface](https://arrow.apache.org/docs/format/CDataInterface/PyCapsuleInterface.html) into Polars and Pandas.
- **High-speed Turbo engine** - selective XLSX feature reading (`read_excel_turbo`), declarative writing (`write_excel_turbo`), and streaming (`write_excel_turbo_stream`).
- **Byte-preserving edit mode** - edit cells while keeping original untouched XML parts, macros, and styles byte-for-byte intact.
- **Standalone formula evaluation** - evaluate formulas, track dependencies, and recalculate spreadsheets with `kyrax.formulas`.
- **Validation & repair** - inspect corrupted spreadsheets and fix repairable findings with `validate_excel` and `repair_excel`.
- **High performance** - 100% Rust core with zero feature logic in Python.

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

This runs a full development cycle: formatting, building, linting, and testing

### Development Commands

Run `make help` to see all available commands, or use these common ones:

```bash
make all          # full dev cycle: format, build, lint, test
make install      # install with debug build (daily development)
make install-prod # install with release build (benchmarking)
make test         # to run the tests
make lint         # to run the linter
make format       # to format python and rust code
make doc-serve    # to serve the documentation locally
```

### Useful Resources

* [`python/kyrax/_kyrax.pyi`](./python/kyrax/_kyrax.pyi) - Python API types
* [`python/tests/`](./python/tests) - Comprehensive usage examples

## Benchmarking

For benchmarking, use `make benchmarks` which automatically builds an optimised wheel.
This is required for profiling, as dev mode builds are much slower.

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

* Use `cargo check` to verify that your rust code compiles, no need to go through `maturin` every time
* `cargo clippy` = 💖
* Careful with arrow constructors, they tend to allocate a lot
* [`mprof`](https://github.com/pythonprofilers/memory_profiler) and `time` go a long way for perf checks,
  no need to go fancy right from the start
