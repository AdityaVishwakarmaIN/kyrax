"""Focused re-bench for Task 5: formulas/comments turbo all + selective.

Best-of-3 after one warm-up, same mechanics as bench_final.py.
"""
from __future__ import annotations

import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "python"))
sys.path.insert(0, str(ROOT / "scripts"))

from bench_final import best_of, materialize_all, path_for, turbo_selective  # noqa: E402


def main() -> None:
    cells = [
        ("formulas", "all", lambda: materialize_all(path_for("formulas"))),
        ("formulas", "selective", lambda: turbo_selective(path_for("formulas"), ["formulas"])),
        ("comments", "all", lambda: materialize_all(path_for("comments"))),
        ("comments", "selective", lambda: turbo_selective(path_for("comments"), ["comments"])),
    ]
    for name, kind, fn in cells:
        t = best_of(fn)
        print(f"{name} turbo {kind}: {t:.6f}s")


if __name__ == "__main__":
    main()
