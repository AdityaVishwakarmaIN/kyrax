import kyrax
import pytest

from .utils import path_for_fixture


@pytest.mark.parametrize("path", ("sheet-with-defined-names.xlsx",))
def test_defined_names(path: str) -> None:
    excel_reader = kyrax.read_excel(path_for_fixture(path))
    defined_names = excel_reader.defined_names()

    expected_defined_names = [
        kyrax.DefinedName(name="AddingValues", formula="SUM(sheet1!$K$5:$K$6)"),
        kyrax.DefinedName(name="DefinedRange", formula="sheet1!$A$5:$D$7"),
        kyrax.DefinedName(name="NamedConstant", formula="3.4"),
    ]

    assert defined_names == expected_defined_names
