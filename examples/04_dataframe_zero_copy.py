"""04_dataframe_zero_copy.py: Zero-copy DataFrame ingestion via Arrow PyCapsule Interface."""

from pathlib import Path
import kyrax

def main():
    out_path = Path("examples_output_04.xlsx")

    # Create test workbook
    wb = kyrax.Workbook()
    ws = wb.active
    ws.title = "Analytics"
    ws.append(["Region", "Sales", "ProfitMargin"])
    ws.append(["North", 120000.0, 0.18])
    ws.append(["South", 85000.0, 0.22])
    ws.append(["East", 145000.0, 0.15])
    ws.append(["West", 98000.0, 0.19])
    wb.save(str(out_path))

    # 1. Read sheet
    reader = kyrax.read_excel(str(out_path))
    sheet = reader.load_sheet("Analytics")

    # 2. Ingest into Polars if available
    try:
        import polars as pl
        df_pl = pl.DataFrame(sheet)
        print("Loaded Polars DataFrame via PyCapsule zero-copy:")
        print(df_pl)
    except ImportError:
        print("Polars not installed; skipping Polars demo")

    # 3. Ingest into PyArrow / Pandas if available
    try:
        import pyarrow as pa
        batch = sheet.to_arrow()
        print(f"Loaded PyArrow RecordBatch: {batch.num_rows} rows x {batch.num_columns} cols")
    except ImportError:
        print("PyArrow not installed; skipping PyArrow demo")

    if out_path.exists():
        out_path.unlink()

if __name__ == "__main__":
    main()
