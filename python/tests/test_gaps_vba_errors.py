import io
import tempfile
import zipfile

import pytest

from kyrax import write_excel_turbo

DATA = [{"name": "Sheet1", "columns": [[1, 2]]}]


def test_vba_archive_path_missing_vba_project_bin():
    src_io = io.BytesIO()
    with zipfile.ZipFile(src_io, "w") as z:
        z.writestr("[Content_Types].xml", b"<Types/>")

    with tempfile.NamedTemporaryFile(suffix=".xlsm", delete=False) as f_src:
        f_src.write(src_io.getvalue())
        src_path = f_src.name
    with tempfile.NamedTemporaryFile(suffix=".xlsm", delete=False) as f_out:
        out_path = f_out.name

    with pytest.raises(ValueError, match="has no xl/vbaProject.bin") as excinfo:
        write_excel_turbo(out_path, DATA, vba_archive_path=src_path)
    assert src_path in str(excinfo.value)


def test_vba_archive_path_non_zip_file():
    with tempfile.NamedTemporaryFile(suffix=".txt", delete=False) as f_src:
        f_src.write(b"this is not a zip archive")
        src_path = f_src.name
    with tempfile.NamedTemporaryFile(suffix=".xlsm", delete=False) as f_out:
        out_path = f_out.name

    with pytest.raises(ValueError, match="is not a valid zip") as excinfo:
        write_excel_turbo(out_path, DATA, vba_archive_path=src_path)
    assert src_path in str(excinfo.value)