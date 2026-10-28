"""Compare typed grid reads to openpyxl across the repository XLSX corpus.

Run in the isolated benchmark environment after rebuilding Kyrax. This is an
independent value/type oracle, not a real-Excel compatibility claim.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import kyrax
import openpyxl


def tagged(value):
    return type(value).__name__, repr(value)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    results = []
    for path in sorted((root / "tests/fixtures").glob("*.xlsx")):
        for cached in (False, True):
            entry = {"fixture": path.name, "data_only": cached}
            try:
                oracle = openpyxl.load_workbook(path, read_only=True, data_only=cached)
                expected = {
                    sheet.title: [[tagged(v) for v in row] for row in sheet.values]
                    for sheet in oracle.worksheets
                }
                oracle.close()
            except Exception as error:
                entry.update(status="oracle rejected", error=str(error))
                results.append(entry)
                continue
            try:
                with kyrax.load_workbook(path, read_only=True, data_only=cached) as workbook:
                    got = {
                        name: [[tagged(v) for v in row] for row in workbook[name].values]
                        for name in expected
                    }
                if got == expected:
                    entry.update(status="pass", rows=sum(map(len, got.values())))
                else:
                    differences = []
                    for name in expected:
                        if len(got[name]) != len(expected[name]):
                            differences.append(
                                f"{name}: row count {len(got[name])} != {len(expected[name])}"
                            )
                        for rownum, (a, b) in enumerate(zip(got[name], expected[name]), 1):
                            if a != b:
                                differences.append(f"{name}:{rownum}: {a!r} != {b!r}")
                            if len(differences) >= 5:
                                break
                    entry.update(status="FAIL", differences=differences[:5])
            except Exception as error:
                entry.update(status="FAIL", error=str(error))
            results.append(entry)
    args.out.write_text(json.dumps(results, indent=2) + "\n")
    counts = {
        state: sum(r["status"] == state for r in results)
        for state in ("pass", "FAIL", "oracle rejected")
    }
    print(json.dumps(counts))
    for entry in results:
        if entry["status"] == "FAIL":
            print(json.dumps(entry))
    return bool(counts["FAIL"])


if __name__ == "__main__":
    raise SystemExit(main())
