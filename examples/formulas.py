"""Evaluate formulas, inspect dependencies, and recalculate a workbook.

Run from any directory; the example writes ``formulas_recalculated.xlsx``
there. No optional dependencies are required.
"""

from kyrax import formulas

print("SUM:", formulas.evaluate("=SUM(1,2)"))
print("Context:", formulas.evaluate("A1*2", {"A1": 21}))
print("Error value:", formulas.evaluate("=1/0"))
print("Dependencies:", formulas.dependencies("=SUM(A1:B2)+C5"))
print("Registered functions:", len(formulas.list_functions()))

sheets = [
    {
        "name": "Calc",
        "rows": [["A", "B", "Sum"], [2, 3, None]],
        "formulas": {(1, 2): "A2+B2"},
    }
]
data = formulas.recalculate(sheets)
with open("formulas_recalculated.xlsx", "wb") as output:
    output.write(data)

print("Wrote formulas_recalculated.xlsx", len(data), "bytes")
