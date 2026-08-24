from __future__ import annotations

import io
import datetime
from pathlib import Path
import pytest
import pandas as pd
import polars as pl
import pyarrow as pa

import kyrax
import kyrax.utils as ku
from kyrax import Workbook, load_workbook, edit_excel, Cell, SheetStream, read_excel_turbo_iter


def test_coordinate_helpers():
    assert ku.get_column_letter(1) == "A"
    assert ku.get_column_letter(26) == "Z"
    assert ku.get_column_letter(27) == "AA"
    assert ku.get_column_letter(703) == "AAA"
    assert ku.get_column_letter(16384) == "XFD"

    assert ku.column_index_from_string("A") == 1
    assert ku.column_index_from_string("Z") == 26
    assert ku.column_index_from_string("AA") == 27
    assert ku.column_index_from_string("AAA") == 703
    assert ku.column_index_from_string("XFD") == 16384

    assert ku.coordinate_to_tuple("A1") == (1, 1)
    assert ku.coordinate_to_tuple("C10") == (10, 3)
    assert ku.coordinate_to_tuple("XFD1048576") == (1048576, 16384)

    assert ku.range_boundaries("A1:C10") == (1, 1, 3, 10)
    assert ku.range_boundaries("B2") == (2, 2, 2, 2)

    assert ku.quote_sheetname("Simple") == "Simple"
    assert ku.quote_sheetname("With Space") == "'With Space'"
    assert ku.quote_sheetname("Sheet's") == "'Sheet''s'"


def test_editable_workbook_lifecycle(tmp_path: Path):
    # Blank workbook creation
    wb = Workbook()
    assert wb.sheetnames == ["Sheet"]
    ws = wb.active
    assert ws.title == "Sheet"

    # Sheet naming validation
    with pytest.raises(Exception):
        ws.title = "Invalid[Name]"
    with pytest.raises(Exception):
        ws.title = "A" * 32
    with pytest.raises(Exception):
        ws.title = ""

    ws.title = "DataSheet"
    assert wb.sheetnames == ["DataSheet"]

    # Append rows
    ws.append(["ID", "Name", "Score", "Active"])
    ws.append([1, "Alice", 95.5, True])
    ws.append([2, "Bob", 88.0, False])

    assert ws.min_row == 1
    assert ws.max_row == 3
    assert ws.min_column == 1
    assert ws.max_column == 4
    assert ws.dimensions == "A1:D3"

    # PyCell lazy proxy access
    c = ws.cell(row=2, column=2)
    assert isinstance(c, Cell)
    assert c.row == 2
    assert c.column == 2
    assert c.coordinate == "B2"
    assert c.value == "Alice"

    # Cell modification
    c.value = "Alicia"
    assert ws.cell(row=2, column=2).value == "Alicia"

    # Offset
    c_offset = c.offset(row=1, column=1)
    assert c_offset.coordinate == "C3"
    assert c_offset.value == 88.0

    # Create and remove sheet
    ws2 = wb.create_sheet(title="Summary")
    assert wb.sheetnames == ["DataSheet", "Summary"]
    wb.remove("Summary")
    assert wb.sheetnames == ["DataSheet"]

    # Save to file path
    out_file = str(tmp_path / "test_lifecycle.xlsx")
    wb.save(out_file)

    # Save to file-like object (BytesIO)
    bio = io.BytesIO()
    wb.save(bio)
    assert bio.tell() > 0

    # Load back with load_workbook
    wb_loaded = load_workbook(out_file)
    ws_loaded = wb_loaded["DataSheet"]
    assert ws_loaded.cell(row=2, column=2).value == "Alicia"


def test_pandas_ecosystem_integration(tmp_path: Path):
    import kyrax.pandas
    out_file = str(tmp_path / "pandas_test.xlsx")

    df = pd.DataFrame({
        "IntCol": [1, 2, 3],
        "FloatCol": [1.5, 2.5, 3.5],
        "StrCol": ["alpha", "beta", "gamma"],
        "BoolCol": [True, False, True],
    })

    # Test DataFrame.to_excel with engine='kyrax'
    df.to_excel(out_file, engine="kyrax", index=False)

    # Test pd.read_excel with engine='kyrax'
    df_read = pd.read_excel(out_file, engine="kyrax")
    assert len(df_read) == 3
    assert list(df_read.columns) == ["IntCol", "FloatCol", "StrCol", "BoolCol"]
    assert df_read["IntCol"].tolist() == [1, 2, 3]
    assert df_read["StrCol"].tolist() == ["alpha", "beta", "gamma"]


def test_conditional_formatting_rules(tmp_path: Path):
    out_file = str(tmp_path / "cf_rules_test.xlsx")

    sheets = [{
        "name": "CF_Sheet",
        "rows": [
            ["Score", "Metric", "Status"],
            [10, 50, "PASS"],
            [20, 75, "FAIL"],
            [30, 90, "PASS"],
            [40, 100, "PENDING"],
        ],
        "conditional_formatting": [
            {
                "sqref": "A2:A5",
                "rules": [
                    {
                        "type": "colorScale",
                        "priority": 1,
                        "cfvos": [{"type": "min"}, {"type": "max"}],
                        "colors": [{"rgb": "F8696B"}, {"rgb": "63BE7B"}],
                    },
                    {
                        "type": "dataBar",
                        "priority": 2,
                        "color": {"rgb": "638EC6"},
                        "show_value": True,
                        "min_length": 10,
                        "max_length": 90,
                    },
                    {
                        "type": "top10",
                        "priority": 3,
                        "rank": 2,
                        "percent": False,
                        "bottom": False,
                        "dxf": {"font": {"bold": True, "color": {"rgb": "006100"}}},
                    },
                    {
                        "type": "aboveAverage",
                        "priority": 4,
                        "above_average": True,
                        "dxf": {"font": {"italic": True}},
                    },
                    {
                        "type": "duplicateValues",
                        "priority": 5,
                        "dxf": {"font": {"strike": True}},
                    },
                    {
                        "type": "uniqueValues",
                        "priority": 6,
                        "dxf": {"font": {"underline": "single"}},
                    },
                ],
            },
            {
                "sqref": "C2:C5",
                "rules": [
                    {
                        "type": "containsText",
                        "priority": 7,
                        "text": "PASS",
                        "dxf": {"font": {"color": {"rgb": "008000"}}},
                    },
                    {
                        "type": "beginsWith",
                        "priority": 8,
                        "text": "P",
                        "dxf": {"font": {"bold": True}},
                    },
                    {
                        "type": "endsWith",
                        "priority": 9,
                        "text": "G",
                        "dxf": {"font": {"italic": True}},
                    },
                    {
                        "type": "containsBlanks",
                        "priority": 10,
                        "dxf": {"font": {"color": {"rgb": "FF0000"}}},
                    },
                    {
                        "type": "timePeriod",
                        "priority": 11,
                        "time_period": "today",
                        "dxf": {"font": {"bold": True}},
                    },
                ],
            }
        ]
    }]

    kyrax.write_excel_turbo(out_file, sheets)

    # Validate file integrity with read_excel_turbo
    reader = kyrax.read_excel_turbo(out_file)
    assert reader.sheet_names == ["CF_Sheet"]
    sheet = reader.load_sheet("CF_Sheet")
    assert sheet.nrows == 4
    assert sheet.ncols == 3


def test_persistent_handle_cache_and_streaming_write(tmp_path: Path):
    out_file = str(tmp_path / "streaming_write_test.xlsx")

    # Streaming write via rows_iter generator
    def row_generator():
        yield ["Index", "Data", "Flag"]
        for i in range(100):
            yield [i, f"Row_{i}", i % 2 == 0]

    sheets = [{
        "name": "StreamSheet",
        "rows_iter": row_generator(),
    }]
    kyrax.write_excel_turbo(out_file, sheets)

    # Persistent handle LRU cache test
    reader = kyrax.read_excel_turbo(out_file)
    s1 = reader.load_sheet("StreamSheet")
    s2 = reader.load_sheet("StreamSheet")
    # Verify cached sheet equality
    assert s1.nrows == s2.nrows == 100
    assert s1.ncols == s2.ncols == 3

    # Streaming read via read_excel_turbo_iter
    stream = read_excel_turbo_iter(out_file, sheet_idx=0, chunk_size=25)
    batches = list(stream)
    assert len(batches) == 4
    total_rows = sum(b.num_rows for b in batches)
    assert total_rows == 100


def test_magic_byte_sniffing(tmp_path: Path):
    corrupt_file = str(tmp_path / "corrupt.xlsx")
    with open(corrupt_file, "wb") as f:
        f.write(b"NOT_A_ZIP_FILE")

    with pytest.raises(Exception) as exc_info:
        kyrax.read_excel_turbo(corrupt_file)
    assert "magic bytes" in str(exc_info.value).lower() or "zip" in str(exc_info.value).lower() or "format" in str(exc_info.value).lower()


def test_overlay_structure_mutations(tmp_path: Path):
    wb = Workbook()
    ws1 = wb.active
    ws1.title = "First"
    ws1.append(["A", "B", "C"])
    ws1.append([1, 2, 3])

    ws2 = wb.create_sheet(title="Second")
    ws2.append(["X", "Y"])
    ws2.append([10, 20])

    ws3 = wb.create_sheet(title="Third")
    ws3.append(["Alpha", "Beta"])
    ws3.append([100, 200])

    file1 = str(tmp_path / "structure_base.xlsx")
    wb.save(file1)

    # Now load and perform multiple structure mutations: rename, delete, copy
    wb_edit = load_workbook(file1)
    assert wb_edit.sheetnames == ["First", "Second", "Third"]

    # Rename middle sheet
    wb_edit["Second"].title = "RenamedSecond"
    assert wb_edit.sheetnames == ["First", "RenamedSecond", "Third"]

    # Delete first sheet
    wb_edit.remove("First")
    assert wb_edit.sheetnames == ["RenamedSecond", "Third"]

    # Copy Third sheet
    ws_copied = wb_edit.copy_worksheet(wb_edit["Third"])
    assert ws_copied.title == "Third Copy"
    assert wb_edit.sheetnames == ["RenamedSecond", "Third", "Third Copy"]

    file2 = str(tmp_path / "structure_mutated.xlsx")
    wb_edit.save(file2)

    # Validate structure with fresh reader
    reader = kyrax.read_excel_turbo(file2)
    assert reader.sheet_names == ["RenamedSecond", "Third", "Third Copy"]
    s_copied = reader.load_sheet("Third Copy")
    assert s_copied.nrows == 1
    assert s_copied.ncols == 2


def test_sheet_generator_iter_rows_iter_cols_values(tmp_path: Path):
    wb = Workbook()
    ws = wb.active
    ws.title = "Grid"
    ws.append(["H1", "H2", "H3"])
    ws.append([1, 2, 3])
    ws.append([4, 5, 6])

    # iter_rows with values_only=True
    rows_gen = ws.iter_rows(values_only=True)
    rows_list = list(rows_gen)
    assert len(rows_list) == 3
    assert rows_list[0] == ("H1", "H2", "H3")
    assert rows_list[1] == (1, 2, 3)
    assert rows_list[2] == (4, 5, 6)

    # iter_rows with Cell objects
    cell_rows = list(ws.iter_rows(values_only=False))
    assert len(cell_rows) == 3
    assert cell_rows[1][0].value == 1
    assert cell_rows[1][0].coordinate == "A2"

    # iter_cols with values_only=True
    cols_gen = ws.iter_cols(values_only=True)
    cols_list = list(cols_gen)
    assert len(cols_list) == 3
    assert cols_list[0] == ("H1", 1, 4)
    assert cols_list[1] == ("H2", 2, 5)
    assert cols_list[2] == ("H3", 3, 6)

    # .values property
    values_list = list(ws.values)
    assert len(values_list) == 3
    assert values_list[1] == (1, 2, 3)


def test_cf_implied_formulas_and_icon_set(tmp_path: Path):
    out_file = str(tmp_path / "cf_advanced.xlsx")
    sheets = [{
        "name": "CF_Adv",
        "rows": [
            ["Text", "Val"],
            ["Hello World", 10],
            ["Foo", 20],
            ["", 30],
        ],
        "conditional_formatting": [
            {
                "sqref": "A2:A4",
                "rules": [
                    {"type": "containsText", "text": "World", "dxf": {"font": {"bold": True}}},
                    {"type": "notContainsText", "text": "Bar", "dxf": {"font": {"italic": True}}},
                    {"type": "beginsWith", "text": "Hel", "dxf": {"font": {"color": {"rgb": "0000FF"}}}},
                    {"type": "endsWith", "text": "rld", "dxf": {"font": {"strike": True}}},
                    {"type": "containsBlanks", "dxf": {"font": {"underline": "single"}}},
                    {"type": "notContainsBlanks", "dxf": {"font": {"bold": True}}},
                    {"type": "timePeriod", "time_period": "last7Days", "dxf": {"font": {"bold": True}}},
                ]
            },
            {
                "sqref": "B2:B4",
                "rules": [
                    {
                        "type": "iconSet",
                        "icon_style": "3TrafficLights1",
                        "percent": True,
                        "cfvos": [
                            {"type": "percent", "val": "0"},
                            {"type": "percent", "val": "33"},
                            {"type": "percent", "val": "67"},
                        ]
                    }
                ]
            }
        ]
    }]

    kyrax.write_excel_turbo(out_file, sheets)
    reader = kyrax.read_excel_turbo(out_file)
    assert reader.sheet_names == ["CF_Adv"]


def test_date_iso_emission(tmp_path: Path):
    out_file = str(tmp_path / "date_iso_test.xlsx")
    sheets = [{
        "name": "Dates",
        "rows": [
            ["ID", "DateCol"],
            [1, datetime.date(2024, 6, 15)],
            [2, datetime.datetime(2024, 6, 15, 14, 30, 0)],
        ]
    }]
    kyrax.write_excel_turbo(out_file, sheets, date_iso=True)

    # Validate output zip contains ISO dates
    import zipfile
    with zipfile.ZipFile(out_file, "r") as z:
        sheet_xml = z.read("xl/worksheets/sheet1.xml").decode("utf-8")
        assert 't="d"' in sheet_xml
        assert "2024-06-15" in sheet_xml


def test_pycell_mutations_and_save_validation(tmp_path: Path):
    wb = Workbook()
    ws = wb.active
    c = ws.cell(row=1, column=1, value="Hello")
    assert c.value == "Hello"

    # Hyperlink + comment are implemented (plan items 33 / R4): set and read back
    c.hyperlink = "https://example.com"
    assert c.hyperlink == "https://example.com"
    c.comment = "A comment"
    assert c.comment == "A comment"

    # Pre-validation on save: non-seekable / invalid target
    class NonSeekableStream:
        def write(self, b):
            pass

    with pytest.raises(ValueError):
        wb.save(NonSeekableStream())

    with pytest.raises(TypeError):
        wb.save(12345)


def test_page_setup_dv_roundtrip_via_sheet_meta_oracle(tmp_path: Path):
    out_file = str(tmp_path / "page_dv_roundtrip.xlsx")
    wb = Workbook()
    ws = wb.active
    ws.page_setup = {"orientation": "landscape", "paper_size": 9, "scale": 85}
    ws.data_validations = [
        {
            "type": "list",
            "formula1": '"Option1,Option2"',
            "sqref": "A1:A10",
            "allow_blank": True,
        }
    ]
    wb.save(out_file)

    reader = kyrax.read_excel_turbo(out_file)
    ts = reader.load_sheet(0, features=["page_setup", "validations"])

    ps = ts.page_setup()
    assert ps is not None
    assert ps.get("orientation") == "landscape"
    assert ps.get("paper_size") == 9
    assert ps.get("scale") == 85

    dvs = ts.data_validations()
    assert dvs is not None
    assert len(dvs) == 1
    dv = dvs[0]
    assert dv.get("type") == "list"
    assert dv.get("formula1") == '"Option1,Option2"'
    assert dv.get("sqref") == "A1:A10"
    assert dv.get("allow_blank") is True


def test_load_workbook_backward_compatibility(tmp_path: Path):
    out_file = str(tmp_path / "compat_test.xlsx")
    wb = Workbook()
    ws = wb.active
    ws.append(["Col1", "Col2"])
    ws.append([10, 20])
    wb.save(out_file)

    # Default edit_mode: returns EditableWorkbook
    wb_edit = load_workbook(out_file)
    assert isinstance(wb_edit, Workbook)

    # read_only=True: returns TurboReader
    r_ro = load_workbook(out_file, read_only=True)
    assert hasattr(r_ro, "load_sheet")

    # edit_mode=False: returns TurboReader
    r_no_edit = load_workbook(out_file, edit_mode=False)
    assert hasattr(r_no_edit, "load_sheet")

    # Unsupported kwargs on edit_mode=True raise TypeError
    with pytest.raises(TypeError):
        load_workbook(out_file, unknown_kwarg=123)


def test_pandas_extended_options(tmp_path: Path):
    out_file = str(tmp_path / "pandas_options_test.xlsx")
    df = pd.DataFrame({
        "A": [10, 20, 30, 40],
        "B": [100, 200, 300, 400],
        "C": ["x", "y", "z", "w"],
    })
    df.to_excel(out_file, engine="kyrax", index=False)

    # KyraxExcelWriter mode != 'w' raises NotImplementedError
    with pytest.raises(NotImplementedError):
        pd.ExcelWriter(out_file, engine="kyrax", mode="a")

    # usecols test
    df_usecols = pd.read_excel(out_file, engine="kyrax", usecols=["A", "C"])
    assert list(df_usecols.columns) == ["A", "C"]
    assert len(df_usecols) == 4

    # nrows test
    df_nrows = pd.read_excel(out_file, engine="kyrax", nrows=2)
    assert len(df_nrows) == 2

    # header=None
    df_no_header = pd.read_excel(out_file, engine="kyrax", header=None)
    assert len(df_no_header) == 5  # header row + 4 data rows


def test_turboreader_header_row_validation(tmp_path: Path):
    out_file = str(tmp_path / "header_val_test.xlsx")
    wb = Workbook()
    ws = wb.active
    ws.append(["A", "B"])
    ws.append([1, 2])
    wb.save(out_file)

    reader = kyrax.read_excel_turbo(out_file)
    # header_row=0 or None succeeds
    s0 = reader.load_sheet(0, header_row=0)
    assert s0.nrows == 1
    s_none = reader.load_sheet(0, header_row=None)
    assert s_none.nrows == 2

    # header_row > 0 raises ValueError
    with pytest.raises(ValueError):
        reader.load_sheet(0, header_row=2)
