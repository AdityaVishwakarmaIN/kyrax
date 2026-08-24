import io
import tempfile
import zipfile
from pathlib import Path
import pytest
import kyrax
from kyrax import (
    load_workbook,
    write_excel_turbo,
    write_excel_turbo_bytes,
    ReadOnlyWorkbook,
    ReadOnlyWorksheet,
)

try:
    import pyarrow as pa
    PYARROW_AVAILABLE = True
except ImportError:
    PYARROW_AVAILABLE = False


def test_item_38_to_arrow_with_errors_contract():
    """Item 38: to_arrow_with_errors must return tuple[pa.RecordBatch, CellErrors] unconditionally."""
    if not PYARROW_AVAILABLE:
        pytest.skip("pyarrow not available")

    # Generate small sheet ("columns" must be columnar arrays; row 1 = header)
    data = [{"name": "Sheet1", "rows": [["A"], [1], [2], [3]]}]
    xlsx_bytes = write_excel_turbo_bytes(data)
    with tempfile.NamedTemporaryFile(suffix=".xlsx", delete=False) as f:
        f.write(xlsx_bytes)
        f_path = f.name

    reader = kyrax.read_excel_turbo(f_path)
    sheet = reader.load_sheet("Sheet1")
    rb, errors = sheet.to_arrow_with_errors()

    assert rb is not None
    assert rb.num_rows == 3
    assert errors is not None
    assert hasattr(errors, "errors")
    assert len(errors.errors) == 0


def test_item_13a_sheet_controls_getters_setters():
    """Item 13a: page_setup, data_validations, protection properties on PyEditableSheet."""
    with tempfile.NamedTemporaryFile(suffix=".xlsx", delete=False) as f:
        f_path = f.name

    # Create blank workbook (edit_excel on a 0-byte file would fail archive parse)
    wb = kyrax.EditableWorkbook()

    ws = wb.active

    # 1. page_setup
    ws.page_setup = {"orientation": "landscape", "paper_size": 9, "scale": 85}
    ps = ws.page_setup
    assert ps is not None
    assert ps.get("orientation") == "landscape"
    assert ps.get("paper_size") == 9
    assert ps.get("scale") == 85

    # 2. data_validations
    ws.data_validations = [
        {
            "type": "list",
            "formula1": '"Option1,Option2"',
            "sqref": "A1:A10",
            "allow_blank": True,
        }
    ]
    dvs = ws.data_validations
    assert len(dvs) == 1
    assert dvs[0]["type"] == "list"
    assert dvs[0]["formula1"] == '"Option1,Option2"'
    assert dvs[0]["sqref"] == "A1:A10"

    # 3. protection
    ws.protection = {"sheet": True, "password": "secret"}
    prot = ws.protection
    assert prot is not None
    assert prot.get("sheet") is True
    assert prot.get("password") == "secret"

    # Save and verify no corruption / ECMA ordering
    wb.save(f_path)
    with open(f_path, "rb") as f:
        saved_bytes = f.read()

    z = zipfile.ZipFile(io.BytesIO(saved_bytes))
    assert "xl/worksheets/sheet1.xml" in z.namelist()
    xml_content = z.read("xl/worksheets/sheet1.xml").decode("utf-8")
    assert "dataValidations" in xml_content
    assert "pageSetup" in xml_content
    assert "sheetProtection" in xml_content


def test_item_28_vba_preservation_wiring():
    """Item 28: vba_archive_path preserves VBA parts and sets macro_enabled."""
    # Create mock source xlsm with vbaProject.bin
    src_io = io.BytesIO()
    with zipfile.ZipFile(src_io, "w") as z_src:
        z_src.writestr("xl/vbaProject.bin", b"VBA_BINARY_BLOB_MOCK")
        z_src.writestr("xl/vbaProjectSignature.bin", b"VBA_SIGNATURE_MOCK")
        z_src.writestr("[Content_Types].xml", b"<Types/>")

    with tempfile.NamedTemporaryFile(suffix=".xlsm", delete=False) as f_src:
        f_src.write(src_io.getvalue())
        src_path = f_src.name

    with tempfile.NamedTemporaryFile(suffix=".xlsm", delete=False) as f_out:
        out_path = f_out.name

    data = [{"name": "Sheet1", "columns": [[10, 20]]}]
    write_excel_turbo(out_path, data, vba_archive_path=src_path)

    # Inspect generated xlsm
    with open(out_path, "rb") as f:
        out_bytes = f.read()

    z_out = zipfile.ZipFile(io.BytesIO(out_bytes))
    assert "xl/vbaProject.bin" in z_out.namelist()
    assert z_out.read("xl/vbaProject.bin") == b"VBA_BINARY_BLOB_MOCK"
    assert "xl/vbaProjectSignature.bin" in z_out.namelist()
    assert z_out.read("xl/vbaProjectSignature.bin") == b"VBA_SIGNATURE_MOCK"

    ct_xml = z_out.read("[Content_Types].xml").decode("utf-8")
    assert "application/vnd.ms-excel.sheet.macroEnabled.main+xml" in ct_xml
    assert "application/vnd.ms-office.vbaProject" in ct_xml

    rels_xml = z_out.read("xl/_rels/workbook.xml.rels").decode("utf-8")
    assert "vbaProject.bin" in rels_xml


def test_item_7_read_only_streaming():
    """Item 7: load_workbook(..., read_only=True) forward streaming and API."""
    data = [
        # "columns" is column-major (each inner list = one column);
        # sheet row 1 is the header row; streaming yields data rows only
        {"name": "Sheet1", "columns": [["hdr", 1, 2, 3, 4, 5]]},
        {"name": "Sheet2", "columns": [["h", 10, 20, 30]]},
    ]
    xlsx_bytes = write_excel_turbo_bytes(data)
    with tempfile.NamedTemporaryFile(suffix=".xlsx", delete=False) as f:
        f.write(xlsx_bytes)
        f_path = f.name

    wb = load_workbook(f_path, read_only=True)
    assert isinstance(wb, ReadOnlyWorkbook)
    assert wb.sheetnames == ["Sheet1", "Sheet2"]

    ws1 = wb["Sheet1"]
    assert isinstance(ws1, ReadOnlyWorksheet)
    assert ws1.title == "Sheet1"
    assert ws1.min_row == 1
    assert ws1.min_column == 1
    assert ws1.max_row is None
    assert ws1.max_column is None

    # Forward row iteration — grid semantics: row 1 is the header, like openpyxl
    rows = list(ws1.iter_rows(values_only=True))
    edit_wb = load_workbook(f_path)
    assert rows == list(edit_wb["Sheet1"].iter_rows(values_only=True))
    assert len(rows) == 6
    assert rows[0] == ("hdr",)
    assert rows[1] == (1,)
    assert rows[5] == (5,)

    # Sliced row iteration (grid coordinates)
    sliced = list(ws1.iter_rows(min_row=2, max_row=4, values_only=True))
    assert len(sliced) == 3
    assert sliced[0] == (1,)
    assert sliced[2] == (3,)

    # values property
    v_rows = list(ws1.values)
    assert len(v_rows) == 6

    # cell method (grid coordinates: sheet row 2 = first data row)
    c2 = ws1.cell(row=2, column=1)
    assert c2.value == 1
    assert c2.row == 2
    assert c2.column == 1

    # Rejection of cell proxy requests without values_only
    with pytest.raises(NotImplementedError):
        list(ws1.iter_rows(values_only=False))

    wb.close()


def test_item_7_read_only_close_blocks_every_sheet_doorway():
    xlsx_bytes = write_excel_turbo_bytes(
        [{"name": "Sheet1", "rows": [["header"], [1]]}]
    )
    with tempfile.NamedTemporaryFile(suffix=".xlsx", delete=False) as f:
        f.write(xlsx_bytes)
        f_path = f.name

    wb = load_workbook(f_path, read_only=True)
    ws = wb["Sheet1"]
    wb.close()

    with pytest.raises(ValueError, match="^workbook is closed$"):
        wb["Sheet1"]
    with pytest.raises(ValueError, match="^workbook is closed$"):
        wb.active
    with pytest.raises(ValueError, match="^workbook is closed$"):
        wb.worksheets
    with pytest.raises(ValueError, match="^workbook is closed$"):
        wb.load_sheet("Sheet1")
    with pytest.raises(ValueError, match="^workbook is closed$"):
        list(ws.iter_rows(values_only=True))
    with pytest.raises(ValueError, match="^workbook is closed$"):
        ws.values
    with pytest.raises(ValueError, match="^workbook is closed$"):
        ws.cell(row=1, column=1)
    with pytest.raises(ValueError, match="^workbook is closed$"):
        ws.max_row

    wb.close()


def test_item_7_editable_written_read_only_parity(tmp_path: Path):
    """Item 7: rows from load_workbook(read_only=True) must equal rows from
    default edit_mode for a workbook written via EditableWorkbook
    (grid semantics incl header row 1)."""
    if not PYARROW_AVAILABLE:
        pytest.skip("pyarrow not available")

    path = tmp_path / "editable_parity.xlsx"
    wb = kyrax.EditableWorkbook()
    ws = wb.active
    ws.title = "Sheet1"
    ws.append(["Name", "Score"])
    ws.append(["Alice", 1])
    ws.append(["Bob", 2])
    ws.append(["Carol", 3])
    wb.save(str(path))

    expected = [("Name", "Score"), ("Alice", 1), ("Bob", 2), ("Carol", 3)]

    edit_ws = load_workbook(str(path))["Sheet1"]
    assert list(edit_ws.iter_rows(values_only=True)) == expected

    ro_ws = load_workbook(str(path), read_only=True)["Sheet1"]
    assert list(ro_ws.iter_rows(values_only=True)) == expected


def test_item_54_probe_workflow_exists():
    """Item 54: empirical manylinux2014 wheel probe workflow is present."""
    repo_root = Path(__file__).resolve().parents[2]
    probe_yml = repo_root / ".github" / "workflows" / "wheel_probe.yml"
    assert probe_yml.exists()
    text = probe_yml.read_text(encoding="utf-8")
    assert "2014" in text  # must actually target the old runtime
    # Probe verdict gets recorded after CI runs; doc location documented in plan
    assert "auditwheel" in text or "maturin" in text
