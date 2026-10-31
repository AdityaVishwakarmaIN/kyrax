"""Use kyrax as a pandas Excel engine.

Install the optional dependencies with ``pip install "kyrax[pandas]"``.
The example writes ``pandas_output.xlsx`` in the current directory.
"""

try:
    import pandas as pd
except ImportError:
    print("pandas is not installed; install kyrax[pandas] to run this example.")
    raise SystemExit(0)

try:
    import pyarrow  # noqa: F401
except ImportError:
    print("pyarrow is not installed; install kyrax[pandas] to run this example.")
    raise SystemExit(0)

import kyrax

frame = pd.DataFrame(
    {
        "item": ["Apple", "Pear"],
        "price": [2.5, 3.0],
        "in_stock": [True, False],
    }
)

frame.to_excel(
    "pandas_output.xlsx",
    engine="kyrax",
    index=False,
    freeze_panes=(1, 0),
)

read_back = pd.read_excel("pandas_output.xlsx", engine="kyrax")
print("pandas read_excel:")
print(read_back)

sheet = kyrax.read_excel("pandas_output.xlsx").load_sheet(0)
print("ExcelSheet.to_pandas:")
print(sheet.to_pandas())
print("Wrote pandas_output.xlsx")
