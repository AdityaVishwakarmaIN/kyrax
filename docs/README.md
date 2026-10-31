# Kyrax documentation

Kyrax is a Python library for reading, writing, and editing Excel workbooks,
backed by a Rust core. This documentation describes the current checkout and
links to runnable examples.

## Start here

| Guide | What it covers |
|---|---|
| [Quickstart](quickstart.md) | Self-contained create, edit, read, write, CSV/JSON, formula, and validation workflows. |
| [Input guide](input-guide.md) | Supported inputs, coordinate conventions, nested writer schemas, validation, streaming, grid/Arrow, pandas, CSV/JSON, formulas, encryption, and errors. |
| [API reference](api-reference.md) | Public API by area, signatures, return types, exceptions, and links to the type stub. |
| [Limitations](limitations.md) | Verified limitations, deferred findings, and explicit non-claims. |
| [Migration from openpyxl](migration-from-openpyxl.md) | Familiar API, differences, read-only/data-only behavior, error cells, and unsupported options. |
| [Coverage](coverage.md) | Capability matrix mapping inputs to docs, examples, test evidence, and known limitations. |
| [Examples](../examples/README.md) | Runnable scripts for each major workflow. |

## Support scope

- **Python only.** There is no `kyrax` console script, no `kyrax.__main__`, and
  `python -m kyrax` fails. Use the Python API from a script or notebook.
- **Python 3.10+.** The package metadata requires Python 3.10 or newer.
- **Primary format: XLSX/XLSM.** The turbo reader, turbo writer, and editable
  workbook API target XLSX/XLSM. The standard `read_excel` reader is
  calamine-backed and is exercised in tests with XLSX, legacy XLS, and ODS
  fixtures; other calamine-supported formats are not documented here.
- **Optional integrations.** `kyrax[pandas]` installs pandas and PyArrow;
  `kyrax[pyarrow]` installs PyArrow; `kyrax[polars]` installs Polars. The core
  reader/writer and editable API do not require pandas, PyArrow, or Polars.
- **No Excel application required.** Kyrax writes OOXML directly. Microsoft
  Excel and LibreOffice interoperability has not been verified; openpyxl is used
  as an independent reader in selected tests, but openpyxl acceptance is not
  proof of Excel behavior.
- **Rust log bridging is lazy.** `import kyrax` does not import Python
  `logging`; the bridge initializes on the first Rust log record. Set
  `KYRAX_LOG=1` to initialize it eagerly during module import.

## Documentation status

This documentation was written against the current checkout and its installed
1.0.8 build. The audit folders in the parent workspace were used as source
material, but findings were rechecked against the current code where possible.
The docs do not claim exhaustive coverage of every nested writer schema, every
API member, every pandas option, or every Excel feature. See
[Limitations](limitations.md) and [Coverage](coverage.md) for the boundaries.

The type stub at [`python/kyrax/_kyrax.pyi`](../python/kyrax/_kyrax.pyi) is the
source-level type reference. It parses and has no duplicate top-level names in
this checkout, but it is not a runtime completeness test and may lag the runtime
surface. Treat the signatures in the [API reference](api-reference.md) as
authoritative for the documented surface.
