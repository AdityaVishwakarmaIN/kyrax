"""Regenerate typed scalar-read expectations using independent openpyxl.

Run with the benchmark environment, never the package's runtime environment:
    python bench/generate_readonly_oracle.py [output.json]
The committed corpus covers raw grid values, formulas, sparse ranges and types.
"""

from __future__ import annotations

import importlib.util
import json
import sys
from pathlib import Path

import openpyxl

ROOT = Path(__file__).resolve().parents[1]
HELPER = ROOT / "python/tests/data/_gen_readonly_rows_expected.py"
spec = importlib.util.spec_from_file_location("readonly_expectations", HELPER)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
output = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("/workspace/readonly_typed_oracle.json")
output.write_text(
    json.dumps(
        module.build(ROOT / "tests/fixtures", load_workbook=openpyxl.load_workbook),
        separators=(",", ":"),
    )
)
print(output)
