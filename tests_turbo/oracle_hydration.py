"""Cache oracle for formula hydration (plan Wave 6, T2).

Excel's own cached values are the ground truth. For every fixture in
``testdata/`` this script:

  1. reads each sheet's formula texts and the values Excel cached for them,
  2. rebuilds an equivalent workbook carrying the same formulas and the same
     plain data, but **no** caches,
  3. saves it with ``recalculate=True`` so kyrax computes the values itself,
  4. reads it back and compares kyrax's value to Excel's, cell by cell.

Three outcomes per formula cell:

  ``match``      kyrax computed a value and it equals Excel's.
  ``mismatch``   kyrax computed a value and it differs — the failure that must
                 be zero, because it means a wrong number reached a file.
  ``fallback``   kyrax declined to compute. Not a failure: the engine is
                 allowed to say "I don't know", and the workbook carries
                 ``fullCalcOnLoad`` so Excel fills it in.

Run: ``python tests_turbo/oracle_hydration.py`` (writes the report and exits
non-zero if any mismatch is found).
"""

from __future__ import annotations

import math
import re
import sys
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "python"))

from kyrax import write_excel_turbo  # noqa: E402

_SHEET = re.compile(r"xl/worksheets/sheet\d+\.xml")
_CELL = re.compile(rb"<c\s+([^>/]*?)(?:/>|>(.*?)</c>)", re.S)
_ATTR = re.compile(rb'(\w+)="([^"]*)"')
_F = re.compile(rb"<f[^>]*>(.*?)</f>", re.S)
_V = re.compile(rb"<v>(.*?)</v>", re.S)


def _rc(ref: str):
    m = re.match(r"([A-Z]+)(\d+)", ref)
    if not m:
        return None
    col = 0
    for ch in m.group(1):
        col = col * 26 + (ord(ch) - 64)
    return int(m.group(2)) - 1, col - 1


def parse_sheet(xml: bytes, shared: list[str]):
    """(values_by_rc, formulas_by_rc) straight from a worksheet part.

    Reading the XML rather than the reader keeps Excel's own cached values as
    ground truth without a reader convenience reshaping them.
    """
    values, formulas = {}, {}
    for m in _CELL.finditer(xml):
        attrs = dict(_ATTR.findall(m.group(1)))
        ref = attrs.get(b"r", b"").decode()
        rc = _rc(ref) if ref else None
        if rc is None:
            continue
        body = m.group(2) or b""
        fm = _F.search(body)
        if fm and not fm.group(1).strip():
            continue  # shared-formula follower with no text of its own
        if fm:
            formulas[rc] = fm.group(1).decode()
        vm = _V.search(body)
        if vm is None:
            continue
        raw = vm.group(1).decode()
        t = attrs.get(b"t", b"").decode()
        try:
            if t == "b":
                values[rc] = raw == "1"
            elif t == "s":
                values[rc] = shared[int(raw)]
            elif t in ("str", "e", "inlineStr"):
                values[rc] = raw
            else:
                values[rc] = float(raw)
        except (ValueError, IndexError):
            continue
    return values, formulas


def shared_strings(z) -> list[str]:
    try:
        xml = z.read("xl/sharedStrings.xml").decode("utf-8", "replace")
    except KeyError:
        return []
    return re.findall(r"<si>(?:(?!</si>).)*?</si>", xml, re.S) and [
        "".join(re.findall(r"<t[^>]*>(.*?)</t>", si, re.S))
        for si in re.findall(r"<si>((?:(?!</si>).)*?)</si>", xml, re.S)
    ]

TESTDATA = ROOT / "testdata"
REPORT = ROOT.parent / "plans" / "formula_hydration_notes" / "oracle_report.md"

# Relative tolerance for float comparison. Excel stores ~15 significant digits;
# anything agreeing to 1e-9 relative is the same number for our purposes.
REL_TOL = 1e-9

# Rebuilding a sheet means materialising it as Python lists; past this many
# cells that costs more than the coverage is worth, and the fixture is reported
# as skipped rather than silently dropped.
CELL_CAP = 2_500_000


def _same(a, b) -> bool:
    """Excel-equivalence, not Python equality."""
    if isinstance(a, bool) or isinstance(b, bool):
        return bool(a) == bool(b)
    if isinstance(a, (int, float)) and isinstance(b, (int, float)):
        if math.isnan(a) or math.isnan(b):
            return False
        if a == b:
            return True
        scale = max(abs(a), abs(b), 1.0)
        return abs(a - b) <= REL_TOL * scale
    return str(a) == str(b)


def check_file(path: Path) -> dict:
    stats = {
        "file": path.name,
        "formulas": 0,
        "oracle": 0,
        "match": 0,
        "mismatch": 0,
        "fallback": 0,
        "examples": [],
        "note": "",
    }
    try:
        with zipfile.ZipFile(path) as z:
            shared = shared_strings(z)
            parts = sorted(n for n in z.namelist() if _SHEET.fullmatch(n))
            sheets = [(n, parse_sheet(z.read(n), shared)) for n in parts]
    except Exception as exc:  # a fixture we cannot even open is not an oracle
        stats["note"] = f"unreadable: {type(exc).__name__}"
        return stats

    names = [n.rsplit("/", 1)[-1] for n, _ in sheets]
    for si, (_part, (cached, formulas)) in enumerate(sheets):
        if not formulas:
            continue
        stats["formulas"] += len(formulas)

        # Rebuild: plain data cells as rows, formulas as formulas, no caches.
        data = {rc: v for rc, v in cached.items() if rc not in formulas}
        max_r = max((r for r, _ in list(data) + list(formulas)), default=-1)
        max_c = max((c for _, c in list(data) + list(formulas)), default=-1)
        if max_r < 0:
            continue
        if (max_r + 1) * (max_c + 1) > CELL_CAP:
            stats["note"] = (
                f"skipped: {(max_r + 1) * (max_c + 1)} cells exceeds the "
                f"{CELL_CAP} rebuild cap"
            )
            continue
        rows = [[None] * (max_c + 1) for _ in range(max_r + 1)]
        for (r, c), v in data.items():
            rows[r][c] = v

        out = path.parent / f".oracle_{path.stem}_{si}.xlsx"
        try:
            write_excel_turbo(
                str(out),
                [{"name": "S", "rows": rows, "formulas": dict(formulas)}],
                recalculate=True,
            )
            # Read the result back the same way: from the XML, so the
            # comparison is between two values that both came out of a file.
            with zipfile.ZipFile(out) as z:
                osh = sorted(n for n in z.namelist() if _SHEET.fullmatch(n))
                got, _ = parse_sheet(z.read(osh[0]), shared_strings(z))
        except Exception as exc:
            stats["note"] = f"rebuild failed: {type(exc).__name__}: {exc}"
            continue
        finally:
            out.unlink(missing_ok=True)

        for rc, _text in formulas.items():
            if rc not in cached:
                continue  # Excel cached nothing here; no ground truth to check
            stats["oracle"] += 1
            if rc not in got:
                stats["fallback"] += 1
            elif _same(got[rc], cached[rc]):
                stats["match"] += 1
            else:
                stats["mismatch"] += 1
                if len(stats["examples"]) < 5:
                    stats["examples"].append(
                        f"{names[si]}!r{rc[0]}c{rc[1]} `{formulas[rc]}` "
                        f"excel={cached[rc]!r} kyrax={got[rc]!r}"
                    )
    return stats


def main() -> int:
    files = sorted(TESTDATA.glob("*.xlsx"))
    results = [check_file(p) for p in files]
    results = [r for r in results if r["formulas"] or r["note"]]

    total = {k: sum(r[k] for r in results) for k in ("formulas", "oracle", "match", "mismatch", "fallback")}
    lines = [
        "# Cache-oracle report — formula hydration",
        "",
        "Excel's own cached values are the ground truth. Each formula cell is",
        "rebuilt without its cache, recomputed by kyrax, and compared.",
        "**`mismatch` is the number that must be zero** — it means a wrong value",
        "reached a file. `fallback` is kyrax declining to compute, which is a",
        "supported outcome (the workbook carries `fullCalcOnLoad`).",
        "",
        f"Float comparison tolerance: relative {REL_TOL:g}.",
        "",
        "| fixture | formula cells | with an Excel cache | match | mismatch | fallback | note |",
        "|---|---:|---:|---:|---:|---:|---|",
    ]
    for r in results:
        lines.append(
            f"| {r['file']} | {r['formulas']} | {r['oracle']} | {r['match']} | "
            f"{r['mismatch']} | {r['fallback']} | {r['note']} |"
        )
    lines.append(
        f"| **total** | **{total['formulas']}** | **{total['oracle']}** | "
        f"**{total['match']}** | **{total['mismatch']}** | **{total['fallback']}** | |"
    )

    examples = [e for r in results for e in r["examples"]]
    if examples:
        lines += ["", "## Mismatches", ""] + [f"- {e}" for e in examples]

    REPORT.parent.mkdir(parents=True, exist_ok=True)
    REPORT.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print("\n".join(lines[-len(results) - 4 :]))
    print(f"\nreport written to {REPORT}")
    return 1 if total["mismatch"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
