"""Use the openpyxl-style workbook API.

Run from any directory; the example writes ``familiar_api.xlsx`` there.
No optional dependencies are required.
"""

from pathlib import Path

import kyrax
from kyrax.chart import BarChart, Reference
from kyrax.formatting import CellIsRule
from kyrax.styles import Alignment, Border, Font, PatternFill, Side
from kyrax.worksheet import DataValidation, Table

OUTPUT = Path("familiar_api.xlsx")

wb = kyrax.Workbook()
ws = wb.active
ws.title = "Sales"

ws["A1":"C1"] = [["Item", "Price", "In stock"]]
ws["A2":"C3"] = [["Apple", 2.5, True], ["Pear", 3.0, False]]
ws["A4"] = "Total"
ws["B4"] = "=SUM(B2:B3)"

ws["A1"].font = Font(name="Calibri", size=14, bold=True, color="0070C0")
ws["A1"].fill = PatternFill(fill_type="solid", start_color="FFFFE0")
thin = Side(style="thin", color="000000")
ws["A1"].border = Border(left=thin, right=thin, top=thin, bottom=thin)
ws["A1"].alignment = Alignment(horizontal="center", vertical="center")

ws.merge_cells("A1:C1")
ws.freeze_panes = "A2"
ws.tab_color = "0070C0"
ws.column_dimensions["A"].width = 18
ws.row_dimensions[1].height = 24

ws["B2"].hyperlink = "https://example.com"
ws["C2"].comment = kyrax.Comment("First row", "Author")

ws.insert_rows(2)
ws["A2"] = "Inserted row"
ws.delete_rows(2)

ws.add_data_validation(DataValidation(type="whole", formula1="1", sqref="B2:B3"))
ws.add_table(Table(displayName="SalesTable", ref="A1:C3"))
ws.conditional_formatting.add(
    "B2:B3",
    CellIsRule(
        operator=">",
        formula=["2"],
        fill=PatternFill(fill_type="solid", start_color="FFFF00"),
    ),
)

chart = BarChart()
chart.title = "Prices"
chart.add_data(
    Reference(ws, min_col=2, min_row=1, max_row=3),
    titles_from_data=True,
)
chart.set_categories(Reference(ws, min_col=1, min_row=2, max_row=3))
ws.add_chart(chart, "E2")

notes = wb.create_sheet("Notes")
notes["A1"] = "Created with the familiar workbook API"
notes.auto_filter.ref = "A1:A2"
copied = wb.copy_worksheet("Notes")
copied.title = "Notes copy"
wb.move_sheet("Notes copy", offset=-1)

wb.defined_names["SalesTotal"] = "Sales!$B$4"
wb.save(OUTPUT)

loaded = kyrax.load_workbook(OUTPUT)
sheet = loaded["Sales"]
print("A1:", sheet["A1"].value)
print("Row 3:", [cell.value for cell in sheet[3]])
print("Freeze panes:", sheet.freeze_panes)
print("Merged ranges:", [str(r) for r in sheet.merged_cells])
print("Defined names:", list(loaded.defined_names))
print("Sheets:", loaded.sheetnames)
loaded.close()
print("Wrote", OUTPUT)
