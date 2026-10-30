import pytest
from kyrax import formulas


def test_formula_context_rejects_lossy_integers_and_nonfinite_numbers():
    for value in (9007199254740993, float('nan'), float('inf'), -float('inf')):
        with pytest.raises(ValueError):
            formulas.evaluate('=A1', {'A1': value})
    assert formulas.evaluate('=A1', {'A1': 42}) == 42
    assert formulas.evaluate('=A1', {'A1': 9007199254740992}) == 9007199254740992
