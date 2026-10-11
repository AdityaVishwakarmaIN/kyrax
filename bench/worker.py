"""One measurement in a fresh process: ``worker.py TASK LIB SIZE FIXTURE OUT RESULT``.

Timed section = library import + the task itself (interpreter start-up and
input-data construction are excluded; they are identical for every library).
The parent additionally records whole-process CPU and peak RSS via wait4().
"""

from __future__ import annotations

import json
import os
import resource
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from fixtures import NCOLS, SIZES, make_rows  # noqa: E402

EDIT_CELLS = 1000  # cells overwritten in column B + cells added in a new column I


def _vm_hwm_mb() -> float:
    """Peak RSS of this process image (VmHWM). Unlike the parent's wait4
    ru_maxrss it is not polluted by the parent's RSS at fork/exec time."""
    with open("/proc/self/status") as f:
        for line in f:
            if line.startswith("VmHWM:"):
                return int(line.split()[1]) / 1024.0
    return float("nan")


def _cpu() -> float:
    r = resource.getrusage(resource.RUSAGE_SELF)
    return r.ru_utime + r.ru_stime


# --------------------------------------------------------------------- IMPORT
def _import(mod):
    def fn(fx, out, rows):
        __import__(mod)
        return 0, 0

    return fn


# ---------------------------------------------------------------- READ values
def read_values_kyrax(fx, out, rows):
    import kyrax

    wb = kyrax.load_workbook(fx, read_only=True)
    data = list(wb.active.iter_rows(values_only=True))
    return len(data), len(data[0])


def read_values_calamine(fx, out, rows):
    from python_calamine import CalamineWorkbook

    data = CalamineWorkbook.from_path(fx).get_sheet_by_index(0).to_python()
    return len(data), len(data[0])


def read_values_openpyxl(fx, out, rows):
    import openpyxl

    wb = openpyxl.load_workbook(fx, read_only=True)
    data = list(wb.active.iter_rows(values_only=True))
    wb.close()
    return len(data), len(data[0])


# ------------------------------------------------------------- READ DataFrame
def read_df_kyrax(fx, out, rows):
    import kyrax

    df = kyrax.read_excel(fx).load_sheet(0).to_pandas()
    return len(df) + 1, len(df.columns)


def read_df_pandas_calamine(fx, out, rows):
    import pandas as pd

    df = pd.read_excel(fx, engine="calamine")
    return len(df) + 1, len(df.columns)


def read_df_pandas_openpyxl(fx, out, rows):
    import pandas as pd

    df = pd.read_excel(fx, engine="openpyxl")
    return len(df) + 1, len(df.columns)


# ---------------------------------------------------------------------- WRITE
def write_noop(fx, out, rows):
    """Floor: data construction only (shows the input-data RSS baseline)."""
    return len(rows), len(rows[0])


def write_kyrax(fx, out, rows):
    import kyrax

    kyrax.write_excel_turbo(out, [{"name": "Sheet1", "rows": rows}])
    return len(rows), len(rows[0])


def write_kyrax_stream(fx, out, rows):
    import kyrax

    kyrax.write_excel_turbo_stream(out, [{"name": "Sheet1", "rows": rows}])
    return len(rows), len(rows[0])


def _xlsxwriter(out, rows, constant_memory):
    import xlsxwriter

    wb = xlsxwriter.Workbook(
        out, {"default_date_format": "yyyy-mm-dd", "constant_memory": constant_memory}
    )
    ws = wb.add_worksheet("Sheet1")
    write_row = ws.write_row
    for r, row in enumerate(rows):
        write_row(r, 0, row)
    wb.close()
    return len(rows), len(rows[0])


def write_xlsxwriter(fx, out, rows):
    return _xlsxwriter(out, rows, False)


def write_xlsxwriter_cm(fx, out, rows):
    return _xlsxwriter(out, rows, True)


def _openpyxl_write(out, rows, write_only):
    import openpyxl

    wb = openpyxl.Workbook(write_only=write_only)
    ws = wb.create_sheet("Sheet1") if write_only else wb.active
    append = ws.append
    for row in rows:
        append(row)
    wb.save(out)
    return len(rows), len(rows[0])


def write_openpyxl(fx, out, rows):
    return _openpyxl_write(out, rows, False)


def write_openpyxl_wo(fx, out, rows):
    return _openpyxl_write(out, rows, True)


# ----------------------------------------------------------------------- EDIT
def _edits(nrows):
    k = min(EDIT_CELLS, nrows)
    return [(r, r * 0.5, f"edited-{r}") for r in range(2, k + 2)]


def edit_kyrax(fx, out, rows):
    import kyrax

    nrows = int(os.environ["BENCH_NROWS"])
    wb = kyrax.load_workbook(fx)
    ws = wb.active
    for r, num, text in _edits(nrows):
        ws.cell(row=r, column=2, value=num)
        ws.cell(row=r, column=NCOLS + 1, value=text)
    wb.save(out)
    return nrows + 1, NCOLS + 1


def edit_openpyxl(fx, out, rows):
    import openpyxl

    nrows = int(os.environ["BENCH_NROWS"])
    wb = openpyxl.load_workbook(fx)
    ws = wb.active
    for r, num, text in _edits(nrows):
        ws.cell(row=r, column=2, value=num)
        ws.cell(row=r, column=NCOLS + 1, value=text)
    wb.save(out)
    return nrows + 1, NCOLS + 1


TASKS = {
    "import": {
        "kyrax": _import("kyrax"),
        "calamine": _import("python_calamine"),
        "openpyxl": _import("openpyxl"),
        "xlsxwriter": _import("xlsxwriter"),
        "pandas": _import("pandas"),
    },
    "read_values": {
        "kyrax": read_values_kyrax,
        "calamine": read_values_calamine,
        "openpyxl-ro": read_values_openpyxl,
    },
    "read_df": {
        "kyrax": read_df_kyrax,
        "pandas-calamine": read_df_pandas_calamine,
        "pandas-openpyxl": read_df_pandas_openpyxl,
    },
    "write": {
        "(data only)": write_noop,
        "kyrax": write_kyrax,
        "kyrax-stream": write_kyrax_stream,
        "xlsxwriter": write_xlsxwriter,
        "xlsxwriter-cm": write_xlsxwriter_cm,
        "openpyxl": write_openpyxl,
        "openpyxl-wo": write_openpyxl_wo,
    },
    "edit": {
        "kyrax": edit_kyrax,
        "openpyxl": edit_openpyxl,
    },
}


def main() -> None:
    task, lib, size, fx, out, result = sys.argv[1:7]
    fn = TASKS[task][lib]
    rows = make_rows(SIZES[size]) if task == "write" else None
    os.environ["BENCH_NROWS"] = str(SIZES[size])
    c0, w0 = _cpu(), time.perf_counter()
    shape = fn(fx, out, rows)
    c1, w1 = _cpu(), time.perf_counter()
    with open(result, "w") as f:
        json.dump(
            {"cpu_task": c1 - c0, "wall_task": w1 - w0, "rss_mb": _vm_hwm_mb(),
             "shape": list(shape)},
            f,
        )


if __name__ == "__main__":
    main()
