import tempfile
from pathlib import Path
import kyrax

def test_mixed_column_preservation():
    with tempfile.TemporaryDirectory() as tmpdir:
        output_path = Path(tmpdir) / "mixed_col.xlsx"

        # Create workbook with mixed types in Column 1: Float then String
        sheets_data = [{
            "name": "Sheet1",
            "columns": [
                [10.5, "PENDING", 20.0, "COMPLETED"],
            ]
        }]
        kyrax.write_excel_turbo(str(output_path), sheets_data)
        assert output_path.exists()

        # Read back via kyrax.read_excel
        reader = kyrax.read_excel(str(output_path))
        sheet = reader.load_sheet(0, header_row=None)
        batch = sheet.to_arrow()
        # Ensure rows are read cleanly without silent data loss
        assert len(batch) == 4
