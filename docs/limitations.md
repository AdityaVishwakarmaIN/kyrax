# Limitations

This page separates limitations verified in the current checkout from deferred
findings and explicit non-claims. "Verified" means the behavior was observed in
this checkout with the installed 1.0.8 build, a focused test, or source
inspection. It does not mean the behavior is desirable or permanent.

## Verified limitations

### Interface and interoperability

- **Python only.** There is no `kyrax` console script, no `kyrax.__main__`, and
  `python -m kyrax` fails with `No module named kyrax.__main__`. Use a Python
  script or notebook.
- **Excel interoperability is not verified.** Tests use openpyxl as an
  independent reader in selected cases. openpyxl acceptance or rejection is not
  proof of Microsoft Excel or LibreOffice behavior. No Excel application,
  LibreOffice, or Excel COM oracle was used for this documentation.
- **The type stub is not a complete reference.** `python/kyrax/_kyrax.pyi`
  parses and has no duplicate top-level names in this checkout, but it is not a
  runtime completeness test and may lag the runtime surface. Runtime signatures
  in the [API reference](api-reference.md) are authoritative for the documented
  surface.

### pandas

- **Error cells are strings.** pandas reads a real error cell such as `#DIV/0!`
  as the same string as a literal `#DIV/0!` cell. Use the turbo
  `cell_errors()` API when the distinction matters.
- **The pandas writer supports `mode="w"` only.** Append mode raises
  `NotImplementedError`. The writer buffers cells and is not a streaming writer.
- **pandas option coverage is not exhaustive.** Current tests cover `header`,
  `skiprows`, `usecols`, `converters`, `na_values`, `dtype`, `na_filter`,
  `freeze_panes`, and pandas 3.0+ `autofilter`; `BytesIO` input also works in
  this checkout. Other pandas versions and options may behave differently.

### Structural edits and metadata

- **Existing comments block structural edits.** Inserting or deleting rows or
  columns, or moving a range, on a loaded sheet with existing comments is
  refused with `InvalidParametersError`.
- **Existing table column-count changes are refused.** Structural edits that
  would change an existing table's column count are refused rather than
  corrupting the table metadata.
- **Structural edits after adding a pending table are refused.** Add the table
  after structural edits, or save and reload first.
- **Chart and image anchors do not follow structural edits.** A chart or image
  anchored at `D2` remains at `D2` after `insert_rows(1)`; the data moves but
  the drawing anchor does not.
- **Some metadata getters only reflect pending edits.** For example,
  `ws.tab_color` returns the pending value before save but returns `None` after
  reload even though the saved file contains the tab color (openpyxl readback
  shows it). Verify saved metadata with an independent reader when it matters.
- **`wb.properties` is read-only.** Assigning `wb.properties.title` raises
  `AttributeError` in the current build.

### Turbo writer nested inputs

- **Nested schemas are not exhaustively documented or validated.** The core
  schema is documented in the [input guide](input-guide.md), but many nested
  fields and combinations are only source-grounded. Unknown top-level sheet keys
  now raise `ValueError`, but nested typos or unsupported combinations may still
  be ignored or weakly validated.
- **External links are weakly validated.** `external_links` accepts strings and
  dictionaries with a `target`; an integer item is accepted but produces no
  link. Validate targets yourself.
- **Unknown workbook properties are ignored.** `props` accepts known keys such
  as `title`, `creator`, `description`, `subject`, `last_modified_by`,
  `company`, and `custom`; unknown keys are silently ignored.
- **Duplicate named styles collapse.** Supplying two named styles with the same
  name is accepted, but the later duplicate is not preserved as a separate
  style.
- **Table header-only references are rejected.** A turbo table reference must
  include a header row and at least one data row. Table names are validated
  against a subset of Excel's rules: start with a letter, `_`, or `\`; remaining
  characters alphanumeric, `_`, or `.`; not a cell reference; at most 255
  characters. Names that Excel might accept outside this subset are rejected.
- **Table column names are not checked against header cells.** The writer
  validates the column count but does not verify that supplied `columns` names
  match the header-row cell text.
- **Arrow structs with a `value` field are unwrapped.** A struct column
  `struct<value: int64>` writes the inner value; a struct without `value` raises
  `TypeError`. This can silently reinterpret structured data.
- **`Fraction` and custom `__float__` values bypass exactness checks.** The
  writer checks Python `int`, `Decimal`, and `__index__` values, but an arbitrary
  object with `__float__` (including `fractions.Fraction`) is converted to a
  float without an exactness check.
- **JSON underflow is silent.** JSON `1e-400` becomes `0.0`. JSON numbers
  outside the finite f64 range are rejected, but underflow is not.
- **Formula context accepts `Decimal` and custom `__float__` values.** The
  formula context checks Python `int` exactness and rejects non-finite values,
  but `Decimal("0.1")` and custom `__float__` values are accepted through float
  conversion without the cell writer's exactness check.
- **Grid fast path is narrow.** It requires a single sheet, a 2-D C-contiguous
  `float32`/`float64` NumPy array, native byte order, and no metadata keys except
  `name`, `grid`, `visibility`, and `state`. It rejects infinity and masked
  arrays; NaN becomes a blank cell.
- **Image validation is partial.** PNG, JPEG, and GIF are detected by magic
  bytes. PNG chunk framing is checked, but image data is not decoded or fully
  validated. Supply valid images.
- **Chart axis titles are not rendered.** Assigning an axis title emits a
  `UserWarning`; the title is not written to the chart.
- **Existing drawings block new drawings.** Adding a chart or image to a loaded
  sheet that already contains drawings is refused.

### Readers and streaming

- **Turbo reader header rows are limited.** `read_excel_turbo(...).load_sheet`
  supports only `header_row=0` and `header_row=None`; other values raise
  `ValueError`.
- **Input types differ by reader.** `read_excel` accepts a path or bytes;
  `read_excel_turbo` accepts a path only; `load_workbook` accepts a path, bytes,
  `bytearray`, `memoryview`, or a binary file-like object.
- **Read-only mode is values-only forward streaming.** `iter_rows` supports
  `values_only=True` only; cell proxies are not available. Read-only mode loads
  shared strings, styles, and formula anchors into memory; worksheet headers and
  individual rows are limited to 16 MiB each.
- **Lazy streamed output may omit dimensions.** `write_excel_turbo_stream` with
  lazy rows omits the optional worksheet dimension, so some readers may not know
  the maximum row/column counts until scanning.
- **Feature-rich streaming can fall back to an eager model.** The streaming
  writer is not guaranteed to stay streaming for every feature combination.

### Encryption

- **`encryption_info` is feature-gated.** The base build raises `KyraxError`
  ("requires a kyrax build with the `encryption` feature"). `is_encrypted` works
  without a password and never raises.
- **`load_workbook` has no password parameter.** Use
  `read_excel_turbo(path, password=...)` for encrypted workbooks when the build
  supports decryption.

### Compatibility notes

- **`ws[3]` row lookup is supported in current source.** It returns a tuple of
  cells. Older builds may raise `TypeError: sheet key must be a coordinate
  string or slice`; `ws.iter_rows(min_row=3, max_row=3)` is the portable
  fallback.
- **`read_excel` is not the openpyxl-compatible entry point.** Use
  `load_workbook` for the familiar workbook API.

## Deferred findings

These items come from the audit material or were not exhaustively rechecked in
this checkout. They are not claims that the behavior is broken; they are areas
where the evidence is incomplete.

- **Full nested turbo schema coverage.** Charts, conditional formatting, data
  validation, styles, scenarios, print settings, pivots, and their combinations
  are not exhaustively tested. The source accepts more fields than this
  documentation describes.
- **Structural metadata combinations.** Pivot locations, drawing anchors, view
  state, freeze panes, row heights, and other metadata after every
  insert/delete/move sequence are not exhaustively verified. Chart and image
  anchors are verified not to follow row inserts; other combinations may differ.
- **Excel application behavior.** Table header/name validity, external-link
  targets, duplicate named styles, overlapping merges, and all-hidden workbooks
  were not opened in Microsoft Excel or LibreOffice.
- **Encrypted workbook fixtures.** No encrypted fixture was tested in this
  checkout; encryption behavior is documented from the API and source.
- **Polars-specific paths.** Polars is optional and not exercised by the core
  examples. `to_polars()` and `pl.DataFrame(sheet)` require Polars.
- **Performance and memory.** No benchmark or memory measurement is part of this
  documentation. The streaming writer's memory characteristics depend on string
  mode, styles, and feature fallback.
- **Static typing.** The stub is not validated by mypy/pyright in this
  documentation, and it may omit native members or lag runtime behavior.
- **Numeric and coercion policies.** Arrow struct unwrapping, `Fraction`/custom
  `__float__` conversion, JSON underflow, and formula-context `Decimal` handling
  may be intentional policy choices. They are documented as observed behavior,
  not as defects.

## Assumptions and non-claims

- openpyxl is treated as a useful independent reader, not an Excel oracle.
- The installed 1.0.8 build is assumed to match the current source for the
  documented behavior, except where a compatibility note says otherwise.
- XLSX/XLSM is the primary documented format. XLS and ODS are mentioned only
  because the standard reader is exercised with those fixtures.
- Optional dependencies are assumed to be installed through the documented
  extras when an example uses them.
- This documentation does not claim exhaustive API coverage, exhaustive nested
  schema coverage, full openpyxl compatibility, or Microsoft Excel
  interoperability.

## How to check your output

For important workbooks:

1. Validate the file with `kyrax.validate_excel(path)`.
2. Read it back with an independent reader such as openpyxl.
3. For error cells, use the turbo `cell_errors()` API rather than relying on
   pandas strings.
4. For structural edits, save and reload before adding tables or drawings, and
   verify metadata that matters.
5. For exact identifiers, write strings rather than large numbers.
