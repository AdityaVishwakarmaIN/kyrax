"""
Compare read performance with kyrax, xlrd and different openpyxl options
"""

import pytest

from .readers import kyrax_read, pyxl_read, xlrd_read


@pytest.fixture
def plain_data_xls():
    return "./python/tests/benchmarks/fixtures/plain_data.xls"


@pytest.fixture
def plain_data_xlsx():
    return "./python/tests/benchmarks/fixtures/plain_data.xlsx"


@pytest.fixture
def formula_xlsx():
    return "./python/tests/benchmarks/fixtures/formulas.xlsx"


@pytest.mark.benchmark(group="xlsx")
def test_pyxl(benchmark, plain_data_xlsx):
    benchmark(pyxl_read, plain_data_xlsx)


@pytest.mark.benchmark(group="xls")
def test_xlrd(benchmark, plain_data_xls):
    benchmark(xlrd_read, plain_data_xls)


@pytest.mark.benchmark(group="xls")
def test_kyrax_xls(benchmark, plain_data_xls):
    benchmark(kyrax_read, plain_data_xls)


@pytest.mark.benchmark(group="xlsx")
def test_kyrax_xlsx(benchmark, plain_data_xlsx):
    benchmark(kyrax_read, plain_data_xlsx)


@pytest.mark.benchmark(group="xlsx")
def test_pyxl_with_formulas(benchmark, formula_xlsx):
    benchmark(pyxl_read, formula_xlsx)


@pytest.mark.benchmark(group="xlsx")
def test_kyrax_with_formulas(benchmark, formula_xlsx):
    benchmark(kyrax_read, formula_xlsx)
