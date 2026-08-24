"""02_styling_and_formatting.py: Applying fonts, fills, borders, alignment, and number formats."""

from pathlib import Path
import kyrax
from kyrax.styles import Font, PatternFill, Side, Border, Alignment

def main():
    wb = kyrax.Workbook()
    ws = wb.active
    ws.title = "StyledReport"

    # Set header
    ws["A1"] = "Executive Summary"
    ws["A1"].font = Font(name="Arial", size=16, bold=True, color="1F497D")
    ws["A1"].alignment = Alignment(horizontal="center", vertical="center")
    ws.merge_cells("A1:D1")

    # Table headers
    headers = ["Metric", "2025 Actual", "2026 Target", "Growth"]
    for col_idx, h in enumerate(headers, start=1):
        cell = ws.cell(row=3, column=col_idx, value=h)
        cell.font = Font(name="Arial", size=11, bold=True, color="FFFFFF")
        cell.fill = PatternFill(fill_type="solid", start_color="4F81BD")
        cell.alignment = Alignment(horizontal="center")

    # Border setup
    thin = Side(style="thin", color="D9D9D9")
    double = Side(style="double", color="000000")
    total_border = Border(top=thin, bottom=double)

    # Data row
    ws["A4"] = "Revenue"
    ws["B4"] = 1250000
    ws["C4"] = 1500000
    ws["D4"] = 0.20

    ws["B4"].number_format = "$#,##0"
    ws["C4"].number_format = "$#,##0"
    ws["D4"].number_format = "0.0%"

    ws["A4"].border = total_border
    ws["B4"].border = total_border
    ws["C4"].border = total_border
    ws["D4"].border = total_border

    out_path = Path("examples_output_02.xlsx")
    wb.save(str(out_path))
    print(f"Saved styled workbook: {out_path}")

    if out_path.exists():
        out_path.unlink()

if __name__ == "__main__":
    main()
