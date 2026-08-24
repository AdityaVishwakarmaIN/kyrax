"""01_basic_workbook.py: Basic workbook creation, sheet operations, and cell mutation."""

from pathlib import Path
import kyrax

def main():
    # 1. Create a blank workbook
    wb = kyrax.Workbook()
    ws = wb.active
    ws.title = "SalesData"

    # 2. Append rows of data
    headers = ["Product", "Quarter", "Revenue", "InStock"]
    ws.append(headers)

    ws.append(["Widget A", "Q1", 45000.0, True])
    ws.append(["Widget B", "Q1", 28500.5, False])
    ws.append(["Widget C", "Q1", 62100.25, True])

    # 3. Access cells by coordinate
    print(f"Cell A1: {ws['A1'].value}")
    print(f"Dimensions: {ws.dimensions}")

    # 4. Save workbook
    out_path = Path("examples_output_01.xlsx")
    wb.save(str(out_path))
    print(f"Saved {out_path}")

    # 5. Clean up
    if out_path.exists():
        out_path.unlink()

if __name__ == "__main__":
    main()
