"""
Unit tests verifying remediation of Plan 02, Plan 03, and Plan 05.
"""

import pytest
import numpy as np
import kyrax

def test_plan02_numeric_grid_fastlane(tmp_path):
    out_file = str(tmp_path / "numeric_grid.xlsx")
    arr = np.array([[1.0, 2.5], [3.0, 4.25]], dtype=np.float64)
    
    # 1. Grid key works
    kyrax.write_excel_turbo(out_file, [{"name": "Data", "grid": arr}])
    assert (tmp_path / "numeric_grid.xlsx").exists()

def test_plan02_numeric_grid_mutual_exclusivity(tmp_path):
    out_file = str(tmp_path / "mutual_excl.xlsx")
    arr = np.array([[1.0, 2.0]], dtype=np.float64)
    
    # 2. Combining grid with columns or rows raises ValueError
    with pytest.raises(ValueError, match="mutually exclusive"):
        kyrax.write_excel_turbo(out_file, [{"name": "Data", "grid": arr, "columns": [1.0, 2.0]}])
