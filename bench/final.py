"""Build the before/after scoreboard (bench/results/final.{json,md}).

    python bench/final.py BASELINE AFTER [--rerun PRECHANGE_RERUN ...]

BASELINE: the idle-machine baseline (all libraries, pre-change build).
AFTER:    kyrax rows of the final tip (run.py --reuse BASELINE).
--rerun:  kyrax rows of the pre-change build re-measured in the final session;
          they replace the matching BASELINE kyrax rows as "before" (the original
          value is kept in the table for transparency).
Every number is MEASURED by bench/run.py; this script only joins them.
"""

from __future__ import annotations

import argparse
import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
NOT_COMP = {"(data only)"}


def load(p):
    with open(p) as f:
        return json.load(f)


def key(r):
    return (r["task"], r["size"], r["lib"])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("baseline")
    ap.add_argument("after")
    ap.add_argument("--rerun", nargs="*", default=[])
    ap.add_argument("--out", default=os.path.join(HERE, "results", "final"))
    a = ap.parse_args()
    base, after = load(a.baseline), load(a.after)
    reruns = [load(p) for p in a.rerun]
    b_rows = {key(r): r for r in base["rows"]}
    rr_rows = {key(r): r for d in reruns for r in d["rows"] if r["lib"].startswith("kyrax")}
    rr_comp = {key(r): r for d in reruns for r in d["rows"]
               if not r["lib"].startswith("kyrax") and r["lib"] not in NOT_COMP}
    comp = {}
    for r in base["rows"]:
        if not r["lib"].startswith("kyrax") and r["lib"] not in NOT_COMP:
            comp.setdefault((r["task"], r["size"]), []).append(rr_comp.get(key(r), r))
    order = [key(r) for r in base["rows"]]
    a_rows = {key(r): r for r in after["rows"] if r["lib"].startswith("kyrax")}
    out_rows = []
    for k in order:
        if not k[2].startswith("kyrax") or k not in a_rows:
            continue
        orig = b_rows[k]
        before = rr_rows.get(k, orig)
        aft = a_rows[k]
        cs = comp.get(k[:2], [])
        best_cpu = min(cs, key=lambda r: r["cpu_task_s"]["median"]) if cs else None
        best_rss = min(cs, key=lambda r: r["rss_mb"]["median"]) if cs else None
        row = {
            "task": k[0], "size": k[1], "rows": orig["rows"], "lib": k[2],
            "before_source": "pre-change build re-measured in final session" if k in rr_rows
            else "baseline (idle, 23be925)",
            "baseline_original": {"cpu": orig["cpu_task_s"], "rss": orig["rss_mb"]},
            "before": {"cpu": before["cpu_task_s"], "rss": before["rss_mb"]},
            "after": {"cpu": aft["cpu_task_s"], "rss": aft["rss_mb"]},
            "verify_after": aft.get("verify", "n/a"),
            "status": "MEASURED",
        }
        if best_cpu:
            row["best_cpu"] = {"lib": best_cpu["lib"], "cpu": best_cpu["cpu_task_s"]["median"]}
            row["best_rss"] = {"lib": best_rss["lib"], "rss": best_rss["rss_mb"]["median"]}
        out_rows.append(row)
    data = {"baseline_meta": base["meta"], "after_meta": after["meta"],
            "rerun_meta": [d["meta"] for d in reruns], "rows": out_rows,
            "competitor_reruns": [
                {"task": k[0], "size": k[1], "lib": k[2],
                 "orig_cpu": b_rows[k]["cpu_task_s"]["median"], "cpu": r["cpu_task_s"]["median"],
                 "n": r["cpu_task_s"]["n"], "orig_rss": b_rows[k]["rss_mb"]["median"],
                 "rss": r["rss_mb"]["median"]}
                for k, r in rr_comp.items() if k in b_rows]}
    with open(a.out + ".json", "w") as f:
        json.dump(data, f, indent=1)
    with open(a.out + ".md", "w") as f:
        f.write(render(data))
    print(a.out + ".md")


def render(d):
    am, bm = d["after_meta"], d["baseline_meta"]
    L = [
        "# kyrax cost-per-task: before / after",
        "",
        f"- Before: commit `{bm['git']}` build, idle machine, {bm['date_utc']} (bench/results/baseline.md).",
        f"- After: commit `{am['git']}`, {am['date_utc']}, {am.get('cpu_model', '?')}; kyrax rows "
        "re-measured; competitor rows reused from the baseline run unless re-measured (see below).",
    ]
    for m in d["rerun_meta"]:
        L.append(f"- Re-measured before-rows: commit `{m['git']}` build, {m['date_utc']} "
                 "(same session as After, back-to-back). Rows marked † use it as Before; re-measured "
                 "competitor rows (if any) replace the baseline competitor in the ratios.")
    L += [
        "- CPU-s = user+sys of library import + task (all threads), median; RSS = peak (VmHWM).",
        "  `[min-max]` = noise band over repeats. Ratio = kyrax / best competitor (>1 = kyrax worse).",
        "- Every number below is MEASURED with bench/run.py on this machine. No estimates in this table.",
        "",
        "| task | size | lib | CPU before | CPU after | Δ CPU | RSS before | RSS after | Δ RSS | CPU ratio before → after (vs) | RSS ratio before → after (vs) | verify |",
        "|---|---|---|---:|---:|---:|---:|---:|---:|---|---|---|",
    ]
    for r in d["rows"]:
        b, a = r["before"], r["after"]
        bc, ac, br, ar = b["cpu"]["median"], a["cpu"]["median"], b["rss"]["median"], a["rss"]["median"]
        dag = " †" if r["before_source"].startswith("pre-change") else ""
        def band(x):
            return f"[{x['min']:.3f}-{x['max']:.3f}]"
        if "best_cpu" in r:
            bcpu, brss = r["best_cpu"], r["best_rss"]
            rc = f"{bc / bcpu['cpu']:.2f} → **{ac / bcpu['cpu']:.2f}** ({bcpu['lib']})"
            rr = f"{br / brss['rss']:.2f} → **{ar / brss['rss']:.2f}** ({brss['lib']})"
        else:
            rc = rr = "no competitor"
        L.append(
            f"| {r['task']} | {r['size']} ({r['rows']:,}) | {r['lib']}{dag} | {bc:.3f} {band(b['cpu'])} | "
            f"{ac:.3f} {band(a['cpu'])} | {100 * (ac - bc) / bc:+.0f}% | {br:.0f} | {ar:.0f} | "
            f"{100 * (ar - br) / br:+.0f}% | {rc} | {rr} | {r['verify_after']} |"
        )
    if d.get("competitor_reruns"):
        L += ["", "## Re-measured competitor rows (used for ratios)", "",
              "| task | size | lib | original baseline CPU-s | re-measured CPU-s | n | original RSS | re-measured RSS |",
              "|---|---|---|---:|---:|---:|---:|---:|"]
        for c in d["competitor_reruns"]:
            L.append(f"| {c['task']} | {c['size']} | {c['lib']} | {c['orig_cpu']:.3f} | {c['cpu']:.3f} | "
                     f"{c['n']} | {c['orig_rss']:.0f} | {c['rss']:.0f} |")
    rer = [r for r in d["rows"] if r["before_source"].startswith("pre-change")]
    if rer:
        L += ["", "## † Re-measured baseline rows", "",
              "| task | size | lib | original baseline CPU-s | re-measured pre-change CPU-s | original RSS | re-measured RSS |",
              "|---|---|---|---:|---:|---:|---:|"]
        for r in rer:
            o, b = r["baseline_original"], r["before"]
            L.append(f"| {r['task']} | {r['size']} | {r['lib']} | {o['cpu']['median']:.3f} | "
                     f"{b['cpu']['median']:.3f} | {o['rss']['median']:.0f} | {b['rss']['median']:.0f} |")
    L += ["", "## Caveats", "",
          "- read_values: kyrax read_only returns dates as serial floats, booleans as 'TRUE'/'FALSE' "
          "strings and ints as floats; calamine/openpyxl return typed values (see baseline.md). "
          "Any read_values comparison carries this caveat.",
          "- Competitor medians come from the idle baseline run; kyrax After rows from the final run.",
          ""]
    return "\n".join(L)


if __name__ == "__main__":
    main()
