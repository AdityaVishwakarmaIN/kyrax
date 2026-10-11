"""Read-back fidelity of the familiar (openpyxl-style) doorway + pandas engine."""

from __future__ import annotations

import datetime as dt
import os
import subprocess
import sys
import textwrap
import zipfile

import pytest

import kyrax


def _roundtrip(tmp_path, build, name="rb.xlsx", **load_kw):
    p = str(tmp_path / name)
    wb = kyrax.Workbook()
    build(wb.active)
    wb.save(p)
    return p, kyrax.load_workbook(p, **load_kw).active


D = dt.date(2024, 1, 2)
DT = dt.datetime(2024, 1, 2, 3, 4, 5)
T = dt.time(3, 4, 5)


@pytest.mark.parametrize("how", ["append", "setitem", "cell"])
def test_dates_get_default_format_and_read_back(tmp_path, how):
    def build(ws):
        if how == "append":
            ws.append([D, DT, T])
        elif how == "setitem":
            ws["A1"], ws["B1"], ws["C1"] = D, DT, T
        else:
            for c, v in enumerate((D, DT, T), 1):
                ws.cell(row=1, column=c, value=v)

    p, ws = _roundtrip(tmp_path, build)
    assert ws["A1"].value == dt.datetime(2024, 1, 2)
    assert ws["B1"].value == DT
    assert ws["C1"].value == T
    assert [ws.cell(1, c).number_format for c in (1, 2, 3)] == [
        "yyyy-mm-dd", "yyyy-mm-dd h:mm:ss", "h:mm:ss",
    ]
    assert list(ws.iter_rows(values_only=True))[0] == (dt.datetime(2024, 1, 2), DT, T)
    # Every custom numFmtId used by an xf is declared (no Excel repair prompt).
    styles = zipfile.ZipFile(p).read("xl/styles.xml").decode()
    assert 'formatCode="yyyy-mm-dd"' in styles
    assert 'formatCode="yyyy-mm-dd h:mm:ss"' in styles


def test_explicit_format_is_kept_and_plain_numbers_stay_numbers(tmp_path):
    def build(ws):
        ws["A1"].number_format = "dd/mm/yy"
        ws["A1"] = D
        ws["B1"] = 45293.5  # no date format -> stays a number

    _, ws = _roundtrip(tmp_path, build)
    assert ws["A1"].number_format == "dd/mm/yy"
    assert ws["A1"].value == dt.datetime(2024, 1, 2)
    assert ws["B1"].value == 45293.5


def test_links_and_comments_valid_xml_and_kept_on_edit(tmp_path):
    # Regression: `r:` prefix was undeclared (Excel refused the file),
    # legacyDrawing sat before pageMargins, and edits dropped old links/notes.
    import xml.dom.minidom

    from kyrax import Comment

    p0, p1 = str(tmp_path / "h0.xlsx"), str(tmp_path / "h1.xlsx")
    wb = kyrax.Workbook()
    ws = wb.active
    ws["A1"].hyperlink = "https://example.com"
    ws["B1"].comment = Comment("hello", "me")
    wb.save(p0)
    wb = kyrax.load_workbook(p0)
    wb.active["A2"].hyperlink = "https://example.org"
    wb.active["B2"].comment = Comment("second", "me")
    wb.save(p1)

    with zipfile.ZipFile(p1) as z:
        sheet = z.read("xl/worksheets/sheet1.xml").decode()
        assert sum("comments" in n and n.endswith(".xml") for n in z.namelist()) == 1
    xml.dom.minidom.parseString(sheet)  # namespace-well-formed
    assert sheet.index("<pageMargins") < sheet.index("<legacyDrawing")
    ws = kyrax.load_workbook(p1).active
    assert ws["A1"].hyperlink == "https://example.com"
    assert ws["A2"].hyperlink == "https://example.org"
    assert (ws["B1"].comment, ws["B2"].comment) == ("hello", "second")


def test_new_style_on_file_with_several_existing_styles(tmp_path):
    # Regression: new xf indices must be shifted past the source's cellXfs.
    from kyrax.styles import Font

    p0, p1 = str(tmp_path / "s0.xlsx"), str(tmp_path / "s1.xlsx")
    wb = kyrax.Workbook()
    wb.active["A1"] = "x"
    wb.active["B1"] = D
    wb.save(p0)
    wb = kyrax.load_workbook(p0)
    wb.active["A1"].font = Font(bold=True)
    wb.save(p1)
    ws = kyrax.load_workbook(p1).active
    assert ws["A1"].font.bold is True
    assert ws["A1"].number_format == "General"
    assert ws["B1"].value == dt.datetime(2024, 1, 2)


def test_existing_comment_and_hyperlink_read_back(tmp_path):
    def build(ws):
        ws["A2"].comment = kyrax.Comment("hi", "me")
        ws["C3"].hyperlink = "https://x.example"

    _, ws = _roundtrip(tmp_path, build)
    c = ws["A2"].comment
    assert (c.text, c.author) == ("hi", "me")
    assert c == "hi"
    h = ws["C3"].hyperlink
    assert h.target == "https://x.example"
    assert str(h) == "https://x.example" and h == "https://x.example"
    assert ws["B2"].comment is None and ws["B2"].hyperlink is None


def test_data_only_never_returns_formula_text(tmp_path):
    p, ws = _roundtrip(tmp_path, lambda ws: ws.__setitem__("A1", "=SUM(B1:B2)"))
    assert ws["A1"].value == "=SUM(B1:B2)"
    wb = kyrax.load_workbook(p, data_only=True)
    assert wb.active["A1"].value is None
    assert list(wb.active.iter_rows(values_only=True))[0][0] is None


def test_data_only_returns_cached_value(tmp_path):
    p = str(tmp_path / "cached.xlsx")
    kyrax.write_excel_turbo(p, [{
        "name": "S",
        "rows": [[1, 2, None]],
        "formulas": {(0, 2): {"text": "SUM(A1:B1)", "cached": 3}},
    }])
    assert kyrax.load_workbook(p, data_only=True).active["C1"].value == 3
    assert kyrax.load_workbook(p).active["C1"].value == "=SUM(A1:B1)"


def test_date1904_append_uses_1904_epoch(tmp_path):
    p = str(tmp_path / "d1904.xlsx")
    kyrax.write_excel_turbo(p, [{"name": "S", "rows": [["x"]]}], date1904=True)
    wb = kyrax.load_workbook(p)
    wb.active.append([D])
    wb.save(p)
    assert kyrax.load_workbook(p).active["A2"].value == dt.datetime(2024, 1, 2)
    xml = zipfile.ZipFile(p).read("xl/worksheets/sheet1.xml").decode()
    assert "<v>43831" in xml  # 45293 - 1462


@pytest.mark.parametrize("order", ["kyrax_first", "pandas_first"])
def test_pandas_engine_any_import_order(tmp_path, order):
    pytest.importorskip("pandas")
    pytest.importorskip("pyarrow")
    imports = "import kyrax, pandas as pd" if order == "kyrax_first" else "import pandas as pd, kyrax"
    code = textwrap.dedent(f"""
        {imports}
        p = {str(tmp_path / 'pd.xlsx')!r}
        pd.DataFrame({{"a": [1, 2], "b": ["x", "y"]}}).to_excel(p, engine="kyrax", index=False)
        df = pd.read_excel(p, engine="kyrax")
        assert list(df.columns) == ["a", "b"], df
        assert df["b"].tolist() == ["x", "y"], df
        assert df["a"].tolist() == [1, 2], df
        print("OK")
    """)
    env = dict(os.environ)
    env["PYTHONPATH"] = os.pathsep.join(
        [os.path.dirname(os.path.dirname(kyrax.__file__)), env.get("PYTHONPATH", "")]
    )
    r = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True, env=env, timeout=120)
    assert r.returncode == 0 and "OK" in r.stdout, r.stderr
