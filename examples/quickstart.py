"""Create, edit, and read a workbook with kyrax.

Run from any directory; the example writes ``quickstart_output.xlsx`` there.
"""

from pathlib import Path

import kyrax

OUTPUT = Path("quickstart_output.xlsx")

wb = kyrax.Workbook()
ws = wb.active
ws.title = "Sales"

ws.append(["Item", "Price", "In stock"])
ws.append(["Apple", 2.5, True])
ws.append(["Pear", 3.0, False])

ws["D1"] = "Total"
ws["D2"] = "=B2+B3"
ws.freeze_panes = "A2"
wb.save(OUTPUT)

loaded = kyrax.load_workbook(OUTPUT)
sheet = loaded["Sales"]
print("A2 value:", sheet["A2"].value)
print("D2 formula:", sheet["D2"].value)
print("Rows:")
for row in sheet.iter_rows(min_row=1, max_row=3, values_only=True):
    print(" ", row)
loaded.close()

reader = kyrax.read_excel(OUTPUT)
data = reader.load_sheet("Sales")
print("Sheet size:", data.height, "rows x", data.width, "columns")
print("Columns:", [column.name for column in data.selected_columns])
print("Wrote", OUTPUT)
