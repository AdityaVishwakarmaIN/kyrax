"""Generate formula-family workbooks for the Excel COM acceptance gate."""

from __future__ import annotations

from pathlib import Path

from kyrax import write_excel_turbo


ROOT = Path(__file__).resolve().parents[2]
OUTPUT = ROOT / "formula-validation" / "round2" / "com_acceptance"


FAMILIES = {
    "math_logical": {
        "rows": [[1, 2], [3, 4]],
        "formulas": {
            (0, 3): "=SUM(A1:B2)",
            (1, 3): "=IF(A1<B1,POWER(B1,3),0)",
            (2, 3): "=AGGREGATE(9,0,A1:B2)",
        },
    },
    "statistical": {
        "rows": [[1, 2], [2, 4], [3, 6], [4, 8]],
        "formulas": {
            (0, 3): "=AVERAGE(A1:A4)",
            (1, 3): "=STDEV.S(A1:A4)",
            (2, 3): "=PERCENTILE.INC(A1:A4,0.75)",
            (3, 3): "=FORECAST(5,B1:B4,A1:A4)",
        },
    },
    "financial": {
        "rows": [],
        "formulas": {
            (0, 0): "=PV(0.05/12,60,-100)",
            (1, 0): "=FV(0.05/12,60,-100,0)",
            (2, 0): "=EFFECT(0.05,12)",
        },
    },
    "engineering": {
        "rows": [],
        "formulas": {
            (0, 0): '=IMCOSH("1+2i")',
            (1, 0): "=BESSELI(1,2)",
            (2, 0): '=CONVERT(1,"m","ft")',
        },
    },
    "text_datetime": {
        "rows": [],
        "formulas": {
            (0, 0): '=TEXT(1234.5,"#,##0.00")',
            (1, 0): '=DAY("2020-01-02")',
            (2, 0): '=ARRAYTOTEXT({1,"x",TRUE})',
            (3, 0): '=NUMBERVALUE("20%%")',
        },
    },
    "lookup_information": {
        "rows": [[1, "one"], [2, "two"], [3, "three"]],
        "formulas": {
            (0, 3): "=XLOOKUP(2,A1:A3,B1:B3)",
            (1, 3): "=INDEX(B1:B3,MATCH(3,A1:A3,0))",
            (2, 3): "=AREAS(A1:B3)",
            (3, 3): "=ISFORMULA(D1)",
        },
    },
    "dynamic_spill": {
        "rows": [],
        "formulas": {
            (0, 0): "=SEQUENCE(3,2,1,1)",
            (0, 4): "=MAP({1,2,3},LAMBDA(x,x*2))",
            (4, 0): "=SCAN(0,{1,2,3},LAMBDA(a,b,a+b))",
            (4, 4): "=MAKEARRAY(2,2,LAMBDA(row,col,row+col))",
        },
    },
}


def main() -> int:
    OUTPUT.mkdir(parents=True, exist_ok=True)
    for old in OUTPUT.glob("*.xlsx"):
        old.unlink()
    for name, sheet in FAMILIES.items():
        path = OUTPUT / f"{name}.xlsx"
        write_excel_turbo(
            str(path),
            [{"name": "FormulaGate", **sheet}],
            recalculate=True,
        )
        print(path)
    print(f"generated={len(FAMILIES)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
