# Examples

Each script is self-contained and writes its output files to the current
directory. Run them from a scratch directory so the generated workbooks, CSV,
and JSON files do not clutter the repository.

```bash
python examples/quickstart.py
```

Kyrax is a Python-only interface; there is no CLI.

| Example | What it shows | Optional dependencies |
|---|---|---|
| [`quickstart.py`](quickstart.py) | Create, edit, save, and read a workbook with the familiar API and the standard reader. | None |
| [`read_write.py`](read_write.py) | Turbo write from rows, bytes, and a row generator; standard, turbo, and read-only reads. | None |
| [`pandas_workflow.py`](pandas_workflow.py) | pandas `engine="kyrax"` writer/reader and `ExcelSheet.to_pandas()`. | pandas, PyArrow |
| [`interchange.py`](interchange.py) | CSV/JSON import and export with `kyrax.io`. | None |
| [`formulas.py`](formulas.py) | Evaluate formulas, inspect dependencies, and recalculate a workbook. | None |
| [`validate_repair.py`](validate_repair.py) | Validate a workbook, create a repairable defect, and repair it. | None |
| [`familiar_api.py`](familiar_api.py) | openpyxl-style cells, styles, merges, tables, validation, conditional formatting, charts, defined names, and sheet controls. | None |

## Outputs

The scripts create files such as `quickstart_output.xlsx`,
`read_write_rows.xlsx`, `pandas_output.xlsx`, `interchange_csv.xlsx`,
`formulas_recalculated.xlsx`, `validate_repaired.xlsx`, and
`familiar_api.xlsx` in the working directory. They are safe to delete.

## Running the examples as tests

The repository test `python/tests/test_examples.py` runs every `examples/*.py`
script in a temporary working directory. Optional-dependency examples print a
skip message and exit successfully when their dependency is missing.

## See also

- [Documentation](../docs/README.md)
- [Quickstart](../docs/quickstart.md)
- [Input guide](../docs/input-guide.md)
- [Limitations](../docs/limitations.md)
