"""Smoke verification for kyrax turbo Python bindings.

Run from repo root:
  .venv/Scripts/python scripts/turbo_smoke.py
"""

from __future__ import annotations

import random
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "python"))

import kyrax
from kyrax import read_excel, read_excel_turbo

TESTDATA = ROOT / "testdata"

passed = 0
failed = 0


def check(name: str, cond: bool, detail: str = "") -> None:
    global passed, failed
    if cond:
        print(f"PASS  {name}" + (f" — {detail}" if detail else ""))
        passed += 1
    else:
        print(f"FAIL  {name}" + (f" — {detail}" if detail else ""))
        failed += 1


def cell_value(rb, row: int, col: int):
    """Scalar from a RecordBatch cell (handles dict-encoded strings via as_py)."""
    return rb.column(col)[row].as_py()


def main() -> int:
    print("=== turbo smoke ===")
    print(f"kyrax {kyrax.__version__}")
    print(f"testdata  {TESTDATA}")
    print()

    structured = str(TESTDATA / "structured.xlsx")

    # ------------------------------------------------------------------
    # (f) selective flags timing FIRST (before heavy formula materialization)
    # ------------------------------------------------------------------
    # Warm the OS file cache once, then time both paths.
    read_excel_turbo(structured).load_sheet(0, features=["merges"])

    def time_load(features, n: int = 3) -> float:
        samples = []
        for _ in range(n):
            t0 = time.perf_counter()
            read_excel_turbo(structured).load_sheet(0, features=features)
            samples.append(time.perf_counter() - t0)
        return min(samples)

    t_merges = time_load(["merges"])
    t_all = time_load("all")

    ss_merges = read_excel_turbo(structured).load_sheet(0, features=["merges"])
    m_only = ss_merges.merges() or []
    f_only = ss_merges.formulas()
    c_only = ss_merges.comments()
    check(
        "(f) merges-only has merges",
        len(m_only) == 20000,
        f"got {len(m_only)}",
    )
    check(
        "(f) merges-only formulas/comments empty",
        f_only is None and c_only is None,
        f"formulas={f_only!r} comments={c_only!r}",
    )
    check(
        "(f) merges-only faster than all",
        t_merges < t_all,
        f"merges={t_merges:.3f}s all={t_all:.3f}s",
    )
    print(f"      timing: features=['merges']={t_merges:.3f}s  features='all'={t_all:.3f}s")

    # ------------------------------------------------------------------
    # (a) values match classic path on mixed.xlsx
    # ------------------------------------------------------------------
    mixed = str(TESTDATA / "mixed.xlsx")
    classic = read_excel(mixed).load_sheet(0).to_arrow()
    turbo_r = read_excel_turbo(mixed)
    turbo_s = turbo_r.load_sheet(0, features="values")
    turbo_rb = turbo_s.to_arrow()

    check(
        "(a) mixed row count",
        classic.num_rows == turbo_rb.num_rows == turbo_s.nrows,
        f"classic={classic.num_rows} turbo={turbo_rb.num_rows}",
    )

    n_cols = min(classic.num_columns, turbo_rb.num_columns)
    rng = random.Random(42)
    spots = [
        (rng.randrange(classic.num_rows), rng.randrange(n_cols)) for _ in range(20)
    ]
    mismatches = []
    for r, c in spots:
        cv = cell_value(classic, r, c)
        tv = cell_value(turbo_rb, r, c)
        if isinstance(cv, float) and isinstance(tv, float):
            ok = abs(cv - tv) < 1e-9 or (cv != cv and tv != tv)  # NaN
        else:
            ok = cv == tv
        if not ok:
            mismatches.append((r, c, cv, tv))
    check(
        "(a) mixed 20-cell spot check",
        len(mismatches) == 0,
        f"mismatches={mismatches[:3]}" if mismatches else "20 cells equal",
    )

    # ------------------------------------------------------------------
    # (b) structured.xlsx features="all"
    # ------------------------------------------------------------------
    sr = read_excel_turbo(structured)
    ss = sr.load_sheet(0, features="all")

    merges = ss.merges() or []
    hlinks = ss.hyperlinks() or []
    dns = sr.defined_names() or []
    tables = ss.tables() or []

    check("(b) merges == 20000", len(merges) == 20000, f"got {len(merges)}")
    check("(b) hyperlinks == 30000", len(hlinks) == 30000, f"got {len(hlinks)}")
    check("(b) defined_names == 198", len(dns) == 198, f"got {len(dns)}")
    check("(b) tables == 20", len(tables) == 20, f"got {len(tables)}")

    # ------------------------------------------------------------------
    # (c) comments.xlsx
    # ------------------------------------------------------------------
    comments_path = str(TESTDATA / "comments.xlsx")
    cs = read_excel_turbo(comments_path).load_sheet(0, features="all")
    comments = cs.comments()
    n_comments = 0 if comments is None else comments.num_rows
    authors = cs.comment_authors() or []
    check("(c) comments == 30000", n_comments == 30000, f"got {n_comments}")
    check("(c) authors == 10", len(authors) == 10, f"got {len(authors)}")

    # ------------------------------------------------------------------
    # (d) styled.xlsx
    # ------------------------------------------------------------------
    styled = str(TESTDATA / "styled.xlsx")
    st = read_excel_turbo(styled).load_sheet(0, features=["styles"])
    style_table = st.style_table() or []
    style_idx = st.style_indices()
    check(
        "(d) style_table len == 123",
        len(style_table) == 123,
        f"got {len(style_table)}",
    )
    check(
        "(d) style_indices present",
        style_idx is not None and len(style_idx) == st.ncols,
        f"cols={None if style_idx is None else len(style_idx)} nrows0={None if not style_idx else len(style_idx[0])}",
    )

    # ------------------------------------------------------------------
    # (e) formulas.xlsx
    # ------------------------------------------------------------------
    formulas_path = str(TESTDATA / "formulas.xlsx")
    fs = read_excel_turbo(formulas_path).load_sheet(0, features=["formulas"])
    formulas = fs.formulas()
    n_formulas = 0 if formulas is None else formulas.num_rows
    check(
        "(e) formulas length > 390000",
        n_formulas > 390_000,
        f"got {n_formulas}",
    )
    # D-column shared formula at spreadsheet row 3 → data row 1, col 3 → "A3*2"
    d3_text = None
    if formulas is not None:
        fpd = formulas.to_pydict()
        for i in range(formulas.num_rows):
            if fpd["row"][i] == 1 and fpd["col"][i] == 3:
                d3_text = fpd["text"][i]
                break
    check(
        "(e) D3 shared formula text == A3*2",
        d3_text == "A3*2",
        f"got {d3_text!r}",
    )
    # cached values land in to_arrow (both-not-XOR)
    frb = fs.to_arrow()
    check(
        "(e) to_arrow has cached values",
        frb.num_rows == fs.nrows and frb.num_columns == fs.ncols,
        f"rows={frb.num_rows} cols={frb.num_columns}",
    )

    # ------------------------------------------------------------------
    print()
    print(f"=== result: {passed} PASS, {failed} FAIL ===")
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
