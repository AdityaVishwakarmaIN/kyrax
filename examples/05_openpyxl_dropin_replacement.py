"""05_openpyxl_dropin_replacement.py: Drop-in openpyxl compatibility demonstration."""

from pathlib import Path
import kyrax
from kyrax.styles import Font, PatternFill, Side, Border, Alignment, Protection

def main():
    out_path = Path("examples_output_05.xlsx")

    # 1. Create workbook & manipulate sheets
    wb = kyrax.Workbook()
    ws = wb.active
    ws.title = "Summary"

    ws2 = wb.create_sheet(title="Details")
    ws2["A1"] = "Transaction Detail"

    # 2. 2D Range Assignment
    ws["A1":"C2"] = [
        ["Product", "Units", "Revenue"],
        ["Alpha", 100, 2500.0],
    ]

    # 3. Cell Styling
    ws["A1"].font = Font(name="Segoe UI", size=12, bold=True, color="004B87")
    ws["A1"].fill = PatternFill(fill_type="solid", start_color="E6F2FF")
    ws["A1"].alignment = Alignment(horizontal="center", vertical="center")

    thin = Side(style="thin", color="B0C4DE")
    ws["A1"].border = Border(left=thin, right=thin, top=thin, bottom=thin)
    ws["A1"].protection = Protection(locked=True, hidden=False)

    # 4. Merging and Sheet Controls
    ws.merge_cells("A10:D10")
    ws["A10"] = "Confidential - Internal Use Only"
    ws.freeze_panes = "A2"
    ws.tab_color = "004B87"

    wb.save(str(out_path))
    print(f"Saved drop-in openpyxl compatible workbook to {out_path}")

    # 5. Read back via read_only=True
    ro_wb = kyrax.load_workbook(str(out_path), read_only=True)
    print(f"Sheet names: {ro_wb.sheetnames}")
    ro_ws = ro_wb["Summary"]
    for row in ro_ws.iter_rows(values_only=True):
        print(f"Row: {row}")

    if out_path.exists():
        out_path.unlink()

if __name__ == "__main__":
    main()
