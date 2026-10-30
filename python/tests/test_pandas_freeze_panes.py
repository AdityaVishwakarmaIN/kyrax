import io

import openpyxl
import pandas as pd
from kyrax.pandas import register


def test_pandas_freeze_panes_round_trip():
    register()
    output = io.BytesIO()
    pd.DataFrame({"value": [1]}).to_excel(output, engine="kyrax", freeze_panes=(1, 2))
    output.seek(0)
    assert openpyxl.load_workbook(output).active.freeze_panes == "C2"
