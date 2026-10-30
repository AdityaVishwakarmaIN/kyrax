import pytest
import kyrax
from kyrax.worksheet.table import Table


@pytest.mark.parametrize("operation", [
    lambda s: s.insert_cols(2),
    lambda s: s.delete_cols(2),
    lambda s: s.insert_rows(2),
    lambda s: s.delete_rows(2),
    lambda s: s.move_range("A1:C2", rows=1),
])
def test_structural_edit_after_pending_table_is_refused(tmp_path, operation):
    wb = kyrax.Workbook()
    ws = wb.active
    ws.append(["one", "two", "three"])
    ws.append([1, 2, 3])
    ws.add_table(Table(displayName="Sample", ref="A1:C2"))
    operation(ws)
    with pytest.raises(Exception, match="structural edits after adding a table"):
        wb.save(tmp_path / "out.xlsx")


def test_table_added_after_structural_edit_can_save(tmp_path):
    wb = kyrax.Workbook()
    ws = wb.active
    ws.insert_rows(1)
    ws.append(["one", "two"])
    ws.append([1, 2])
    ws.add_table(Table(displayName="Sample", ref="A1:B2"))
    wb.save(tmp_path / "out.xlsx")
