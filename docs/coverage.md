# Coverage

This matrix maps the documented capabilities to the relevant guide, runnable
example, selected test evidence, and known limitations. Test evidence is
selected, not exhaustive, and does not establish Microsoft Excel
interoperability.

| Capability | Documentation | Example | Selected test evidence | Known limitations |
|---|---|---|---|---|
| Create, edit, and save a workbook | [Quickstart](quickstart.md), [Input guide](input-guide.md), [Migration](migration-from-openpyxl.md) | [`quickstart.py`](../examples/quickstart.py), [`familiar_api.py`](../examples/familiar_api.py) | `python/tests/test_familiar_workbook.py`, `test_familiar_readback.py`, `test_input_usability.py`, `test_examples.py` | Structural metadata refusals; `wb.properties` read-only; chart/image anchors do not follow structural edits. |
| Turbo write from rows, columns, or bytes | [Input guide](input-guide.md#turbo-writer-sheet-dictionaries) | [`read_write.py`](../examples/read_write.py) | `python/tests/test_write_rowstream.py`, `test_write_row_iterators.py`, `test_write_sheet_validation.py` | Nested schemas are not exhaustively documented; feature-rich streaming may use an eager fallback. |
| Streaming write | [Input guide](input-guide.md#streaming) | [`read_write.py`](../examples/read_write.py) | `python/tests/test_write_rowstream.py`, `test_write_row_iterators.py` | Lazy output may omit dimensions; feature-rich sheets may fall back to the eager model. |
| NumPy grid fast path | [Input guide](input-guide.md#grid-and-arrow) | — | `python/tests/test_arrow_grid_numeric_regressions.py` | Single sheet, C-contiguous float32/float64, native byte order, no metadata, no infinity, masked arrays rejected. |
| Arrow table input | [Input guide](input-guide.md#grid-and-arrow) | — | `python/tests/test_arrow_grid_numeric_regressions.py` | Structs with `value` are unwrapped; unsupported types raise `TypeError`; non-finite floats rejected. |
| Standard sheet/table reading | [Input guide](input-guide.md#readers) | [`read_write.py`](../examples/read_write.py) | `python/tests/test_kyrax.py`, `test_dtypes.py`, `test_column_selection.py`, `test_eagerness.py`, `test_tables.py`, `test_errors.py` | `read_excel` accepts path/bytes only; dtype inference can widen to string with a warning. |
| Turbo feature reading | [Input guide](input-guide.md#readers) | [`read_write.py`](../examples/read_write.py) | `python/tests/test_turbo_formula_image_regressions.py`, `test_familiar_drawings.py`, `test_tables.py` | `header_row` only 0/None; accessors return `None` when a feature was not requested. |
| Read-only streaming | [Input guide](input-guide.md#streaming) | [`read_write.py`](../examples/read_write.py) | `python/tests/test_readonly_rows_native.py`, `test_typed_readonly_adversarial.py` | `values_only=True` only; 16 MiB header/row limits; dimensions may be unknown. |
| pandas reader/writer | [Input guide](input-guide.md#pandas) | [`pandas_workflow.py`](../examples/pandas_workflow.py) | `python/tests/test_pandas_reader_options.py`, `test_pandas_freeze_panes.py`, `test_pandas_autofilter.py` | Error cells are strings; writer supports `mode="w"` only; option coverage is not exhaustive. |
| CSV/JSON interchange | [Input guide](input-guide.md#csv-and-json) | [`interchange.py`](../examples/interchange.py) | `python/tests/test_confirmed_investigation_regressions.py` (selected), `test_examples.py` | JSON underflow is silent; `has_header` is a no-op for JSON import; nested schemas are raw JSON text. |
| Formula evaluation and recalculation | [Input guide](input-guide.md#formulas) | [`formulas.py`](../examples/formulas.py) | `python/tests/test_formula_oracle.py`, `test_continuation_formula_context.py`, `test_confirmed_investigation_regressions.py` | Function coverage is not exhaustive; dependencies are static; context accepts `Decimal`/custom `__float__` without exactness checks. |
| Validate and repair | [Input guide](input-guide.md#error-handling) | [`validate_repair.py`](../examples/validate_repair.py) | `python/tests/test_import_cost.py` (smoke), `test_examples.py` | Only repairable findings are fixed; Excel behavior is not verified. |
| Encryption detection | [Input guide](input-guide.md#encryption) | — | `python/tests/test_import_cost.py` (`is_encrypted` on plain files) | `encryption_info` requires the encryption feature; no encrypted fixture was tested. |
| Styles, validation, conditional formatting, tables | [Input guide](input-guide.md#structure-validation-and-tables) | [`familiar_api.py`](../examples/familiar_api.py) | `python/tests/test_familiar_cf_tables.py`, `test_editable_style_validation_roundtrip.py`, `test_structural_metadata_audit_regressions.py` | Nested fields and combinations are not exhaustive; table column names are not checked against header text. |
| Charts and images | [Input guide](input-guide.md#charts-images-and-pivots) | [`familiar_api.py`](../examples/familiar_api.py) (chart) | `python/tests/test_familiar_drawings.py`, `test_images.py`, `test_turbo_formula_image_regressions.py` | Axis titles not rendered; existing drawings block new drawings; image validation is partial. |
| Structural edits | [Migration](migration-from-openpyxl.md#structural-edits), [Limitations](limitations.md#structural-edits-and-metadata) | [`familiar_api.py`](../examples/familiar_api.py) | `python/tests/test_editable_structure_pending.py`, `test_structural_existing_metadata.py`, `test_pending_table_structure.py`, `test_structural_metadata_audit_regressions.py` | Existing comments/table column-count changes/pending tables refuse; drawing anchors do not follow. |
| Python-only interface | [Documentation home](README.md#support-scope), [Limitations](limitations.md#interface-and-interoperability) | — | `pyproject.toml` has no console script; no `kyrax.__main__`; `python -m kyrax` fails. | No CLI. |

## What the tests do not establish

- Microsoft Excel or LibreOffice interoperability.
- Exhaustive nested turbo schema coverage.
- Every pandas version or option combination.
- Every formula function or calculation edge case.
- Every structural metadata combination.
- Performance or memory characteristics.
- Static typing correctness of `python/kyrax/_kyrax.pyi`.

See [Limitations](limitations.md) for the detailed list.
