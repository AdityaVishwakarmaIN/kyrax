"""Convert between XLSX, CSV, and JSON with ``kyrax.io``.

Run from any directory; the example writes ``interchange_*`` files there.
"""

from kyrax.io import (
    csv_bytes_to_sheet,
    csv_to_sheet,
    json_bytes_to_sheet,
    json_to_sheet,
    sheet_to_csv,
    sheet_to_csv_bytes,
    sheet_to_json,
    sheet_to_json_bytes,
)

csv_bytes_to_sheet(
    b"name,count\nApple,2\nPear,3\n",
    "interchange_csv.xlsx",
    infer_types=True,
)
json_bytes_to_sheet(
    b'[{"name":"Apple","count":2},{"name":"Pear","count":3}]',
    "interchange_json.xlsx",
)

print("CSV bytes:")
print(sheet_to_csv_bytes("interchange_csv.xlsx", "Sheet1").decode())
print("JSON bytes:")
print(sheet_to_json_bytes("interchange_json.xlsx", "Sheet1", shape="records").decode())

sheet_to_csv("interchange_csv.xlsx", "Sheet1", "interchange_export.csv")
sheet_to_json(
    "interchange_json.xlsx",
    "Sheet1",
    "interchange_export.json",
    shape="records",
)

csv_to_sheet(
    "interchange_export.csv",
    "interchange_csv_roundtrip.xlsx",
    infer_types=True,
)
json_to_sheet("interchange_export.json", "interchange_json_roundtrip.xlsx")

print("Wrote CSV, JSON, and round-trip workbooks.")
