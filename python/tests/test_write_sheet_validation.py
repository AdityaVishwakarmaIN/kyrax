import io
import zipfile

import openpyxl
import pytest
import kyrax


def write(entry, tmp_path, sheet):
    if entry == 'bytes':
        return kyrax.write_excel_turbo_bytes([sheet])
    path = tmp_path / 'test.xlsx'
    getattr(kyrax, 'write_excel_turbo' + ('_stream' if entry == 'stream' else ''))(path, [sheet])
    return path.read_bytes()


@pytest.mark.parametrize('entry', ['path', 'bytes', 'stream'])
@pytest.mark.parametrize('sheet', [
    {'rowz': [[1]]},
    {'rows': [[1]], 'columns': [[2]]},
    {'rows_iter': iter([[1]]), 'data': [[2]]},
    {'rows': [[1]], 'table': [[2]]},
    {'rows': [[1]], 'rows_iter': [[2]]},
    {'columns': [[1]], 'data': [[2]]},
    {'row_dims': [{'row': 0}]},
    {'row_dims': [{'row': 1048577}]},
    {'row_dims': [{'row_idx': 1048576}]},
    {'col_dims': [{'col': 0}]},
    {'col_dims': [{'min': 1, 'max': 16385}]},
    {'col_dims': [{'min': 2, 'max': 1}]},
    {'cell_styles': {(1048576, 0): {}}},
    {'cell_styles': [{'row': 0, 'col': 16384, 'style': {}}]},
])
def test_reject_invalid_sheet(entry, sheet, tmp_path):
    with pytest.raises(ValueError):
        write(entry, tmp_path, sheet)


@pytest.mark.parametrize('entry', ['path', 'bytes', 'stream'])
def test_styles_use_zero_based_coordinates_and_sorted_dimension_rows(entry, tmp_path):
    style = {'font': {'bold': True}}
    sheet = {'rows': [['a', 'b'], ['c', 'd']], 'row_dims': [{'row': 5}, {'row': 3}],
             'col_dims': [{'min': 1, 'max': 2, 'width': 20}],
             'cell_styles': [{'row': 1, 'col': 1, 'style': style}, {'row': 4, 'col': 0, 'style': style}]}
    wb = openpyxl.load_workbook(io.BytesIO(write(entry, tmp_path, sheet)))
    assert wb.active['B2'].font.bold
    assert wb.active['A5'].font.bold
    assert not wb.active['A1'].font.bold
    sheet['cell_styles'] = {(1, 1): style}
    wb = openpyxl.load_workbook(io.BytesIO(write(entry, tmp_path, sheet)))
    assert wb.active['B2'].font.bold


def test_last_dimension_coordinates(tmp_path):
    data = write('bytes', tmp_path, {'row_dims': [{'row_idx': 1048575, 'height': 20}], 'col_dims': [{'col': 16384}]})
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        xml = archive.read('xl/worksheets/sheet1.xml')
    assert b'r="1048576"' in xml
    assert b'min="16384"' in xml


@pytest.mark.parametrize('entry', ['path', 'bytes', 'stream'])
@pytest.mark.parametrize('alias', ['table', 'rows_iter'])
def test_grid_rejects_all_data_aliases(entry, alias, tmp_path):
    np = pytest.importorskip('numpy')
    with pytest.raises(ValueError, match='mutually exclusive'):
        write(entry, tmp_path, {'grid': np.array([[1.0]]), alias: [[2]]})
