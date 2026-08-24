"""Unit test verifying that example workflows and openpyxl drop-in features execute properly."""

import sys
import runpy
from pathlib import Path
import pytest
import kyrax
from kyrax.styles import Font, PatternFill, Side, Border, Alignment, Protection, Comment

EXAMPLES_DIR = Path(__file__).resolve().parent.parent.parent / "examples"
EXAMPLE_FILES = sorted(p.name for p in EXAMPLES_DIR.glob("*.py")) or ["__none__"]

@pytest.mark.parametrize("example_file", EXAMPLE_FILES)
def test_example_scripts_execution(tmp_path, monkeypatch, example_file):
    script_path = EXAMPLES_DIR / example_file
    if not script_path.exists():
        pytest.skip(f"Example script {example_file} not found")
    monkeypatch.chdir(tmp_path)
    runpy.run_path(str(script_path), run_name="__main__")


def test_openpyxl_dropin_full_workflow(tmp_path):
    out_path = tmp_path / "test_dropin.xlsx"

    wb = kyrax.Workbook()
    ws = wb.active
    ws.title = "Main"

    # Slicing & cell assignment
    ws["A1":"C2"] = [
        ["Header1", "Header2", "Header3"],
        [10, 20.5, "Text"],
    ]

    assert ws["A1"].value == "Header1"
    assert ws["B2"].value == 20.5
    assert ws["C2"].value == "Text"

    # Styling
    f = Font(name="Arial", size=12, bold=True, strike=True, color="FF0000")
    ws["A1"].font = f
    assert ws["A1"].font.bold is True
    assert ws["A1"].font.strike is True

    fill = PatternFill(fill_type="solid", start_color="FFFF00")
    ws["A1"].fill = fill
    assert ws["A1"].fill.fill_type == "solid"

    thin = Side(style="thin", color="000000")
    ws["A1"].border = Border(left=thin, right=thin, top=thin, bottom=thin)

    align = Alignment(horizontal="center", vertical="center", wrap_text=True)
    ws["A1"].alignment = align
    assert ws["A1"].alignment.wrap_text is True

    prot = Protection(locked=True, hidden=False)
    ws["A1"].protection = prot
    assert ws["A1"].protection.locked is True

    # Hyperlink and comment
    ws["B2"].hyperlink = "https://example.com"
    assert ws["B2"].hyperlink == "https://example.com"

    ws["C2"].comment = Comment("Test comment", "Author")
    assert ws["C2"].comment == "Test comment"

    # Sheet controls
    ws.freeze_panes = "A2"
    assert ws.freeze_panes == "A2"

    ws.tab_color = "0070C0"
    assert ws.tab_color == "0070C0"

    # Merging
    ws.merge_cells("A10:C12")
    with pytest.raises(Exception):
        ws.merge_cells("B11:D15") # overlap rejection

    ws.unmerge_cells("A10:C12")

    # Save and reload in read_only mode
    wb.save(str(out_path))

    ro_wb = kyrax.load_workbook(str(out_path), read_only=True)
    assert "Main" in ro_wb.sheetnames
    ro_ws = ro_wb["Main"]
    rows = list(ro_ws.iter_rows(values_only=True))
    assert len(rows) >= 2
    assert rows[0][0] == "Header1"


def test_data_only_doorway(tmp_path):
    wb = kyrax.Workbook()
    ws = wb.active
    ws["A1"] = 10
    ws["A2"] = 20
    ws["A3"] = "=SUM(A1:A2)"
    p = tmp_path / "formula.xlsx"
    wb.save(str(p))

    # edit_excel data_only
    ewb = kyrax.edit_excel(str(p), data_only=True)
    assert ewb.active["A1"].value == 10
