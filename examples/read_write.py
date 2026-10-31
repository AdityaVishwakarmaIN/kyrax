"""Write workbooks with the turbo writer and read them back.

Run from any directory; the example writes several ``read_write_*.xlsx`` files
there. No optional dependencies are required.
"""

from pathlib import Path

import kyrax

ROWS_FILE = Path("read_write_rows.xlsx")
BYTES_FILE = Path("read_write_bytes.xlsx")
STREAM_FILE = Path("read_write_stream.xlsx")

sheets = [
    {
        "name": "Sales",
        "rows": [["Item", "Price"], ["Apple", 2.5], ["Pear", 3.0]],
        "freeze_panes": "A2",
        "auto_filter": "A1:B3",
        "merges": ["D1:E1"],
    },
    {"name": "Notes", "rows": [["Note"], ["Created by read_write.py"]]},
]
kyrax.write_excel_turbo(ROWS_FILE, sheets)

data = kyrax.write_excel_turbo_bytes([{"name": "Bytes", "rows": [["x"], [1]]}])
BYTES_FILE.write_bytes(data)


def square_rows():
    yield ["Index", "Square"]
    for index in range(5):
        yield [index, index * index]


kyrax.write_excel_turbo_stream(
    STREAM_FILE,
    [{"name": "Squares", "rows": square_rows()}],
    string_mode="inline",
)

reader = kyrax.read_excel(ROWS_FILE)
print("Standard reader sheets:", reader.sheet_names)
sheet = reader.load_sheet("Sales", use_columns="A:B")
print("Data size:", sheet.height, "rows x", sheet.width, "columns")
print("Columns:", [column.name for column in sheet.selected_columns])

turbo = kyrax.read_excel_turbo(ROWS_FILE)
turbo_sheet = turbo.load_sheet("Sales", features=["values", "merges", "sheet_meta"])
print("Turbo size:", turbo_sheet.nrows, "rows x", turbo_sheet.ncols, "columns")
print("Merges:", turbo_sheet.merges())
print("Freeze panes:", turbo_sheet.freeze_panes())

print("Read-only streaming:")
with kyrax.load_workbook(STREAM_FILE, read_only=True) as workbook:
    worksheet = workbook["Squares"]
    for row in worksheet.iter_rows(values_only=True):
        print(" ", row)

print("Wrote", ROWS_FILE, BYTES_FILE, STREAM_FILE)
