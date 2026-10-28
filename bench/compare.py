"""Gate check: compare a new results JSON against the baseline.

    python bench/compare.py bench/results/baseline.json bench/results/quick.json

A kyrax row REGRESSES when its new median is above the baseline max AND worse
than the baseline median by more than max(baseline noise, --floor %). The same
rule applies to CPU-s and peak RSS. Exit code 1 if any row regresses.
"""

from __future__ import annotations

import argparse
import json
import sys


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("baseline")
    ap.add_argument("new")
    ap.add_argument("--floor", type=float, default=5.0, help="min regression %% (default 5)")
    ap.add_argument("--abs-cpu", type=float, default=0.01, help="ignore CPU deltas below this many s")
    ap.add_argument("--abs-rss", type=float, default=2.0, help="ignore RSS deltas below this many MB")
    ap.add_argument("--all-libs", action="store_true", help="also compare competitor rows")
    a = ap.parse_args()
    base = {(r["task"], r["size"], r["lib"]): r for r in json.load(open(a.baseline))["rows"]}
    new = json.load(open(a.new))["rows"]
    bad = 0
    print(f"{'task':<11} {'size':<6} {'lib':<14} {'metric':<6} {'base':>9} {'new':>9} {'delta':>8} {'noise':>6} {'gnoise':>6}  verdict")
    for r in new:
        key = (r["task"], r["size"], r["lib"])
        # New workloads also need correctness checks, even without a baseline.
        if r.get("verify", "n/a").startswith("FAIL"):
            print(f"{key[0]:<11} {key[1]:<6} {key[2]:<14} verify FAILED: {r['verify']}")
            bad += 1
        if key not in base or (not a.all_libs and not r["lib"].startswith("kyrax")):
            continue
        b = base[key]
        for metric, field in (("cpu", "cpu_task_s"), ("rss", "rss_mb")):
            bm, bmax, nm = b[field]["median"], b[field]["max"], r[field]["median"]
            noise = b[field]["noise_pct"]
            delta = 100.0 * (nm - bm) / bm if bm else 0.0
            absf = a.abs_cpu if metric == "cpu" else a.abs_rss
            if nm > bmax and delta > max(noise, a.floor) and nm - bm > absf:
                verdict = "REGRESSION"
                bad += 1
            elif delta < -max(noise, a.floor) and r[field]["max"] < b[field]["min"]:
                verdict = "improved"
            else:
                verdict = "within noise"
            gnoise = r[field]["noise_pct"]
            wider = "  [gate noise wider than baseline]" if gnoise > noise + 1.0 else ""
            print(f"{key[0]:<11} {key[1]:<6} {key[2]:<14} {metric:<6} {bm:>9.3f} {nm:>9.3f} "
                  f"{delta:>+7.1f}% {noise:>5.0f}% {gnoise:>5.0f}%  {verdict}{wider}")
    print(f"\n{bad} regression(s)")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
