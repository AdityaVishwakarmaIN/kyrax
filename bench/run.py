"""kyrax cost-per-task benchmark driver.

Every measurement runs in a fresh subprocess (bench/worker.py); the parent
reaps it with os.wait4() to get whole-process CPU (user+sys, all threads) and
peak RSS (ru_maxrss). Results: median of N repeats + min/max noise band.

    python bench/run.py                  # full matrix -> bench/results/latest.*
    python bench/run.py --quick          # merge-gate subset (kyrax rows, small+medium)
    python bench/run.py --out baseline   # write bench/results/baseline.*

See bench/README.md for the environment (competitors live in a scratch venv).
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import platform
import statistics
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
sys.path.insert(0, HERE)
from fixtures import NCOLS, SIZES, ensure_fixture  # noqa: E402
from worker import EDIT_CELLS, TASKS  # noqa: E402

# (task, size) -> libraries measured. openpyxl/pandas-openpyxl/xlsxwriter-normal
# are skipped at 1M rows: one run takes minutes and they are not the best
# competitor there (calamine / xlsxwriter constant_memory are).
MATRIX = {
    ("import", "small"): ["kyrax", "calamine", "openpyxl", "xlsxwriter", "pandas"],
    ("read_values", "small"): ["kyrax", "calamine", "openpyxl-ro"],
    ("read_values", "medium"): ["kyrax", "calamine", "openpyxl-ro"],
    ("read_values", "large"): ["kyrax", "calamine"],
    ("read_df", "small"): ["kyrax", "pandas-calamine", "pandas-openpyxl"],
    ("read_df", "medium"): ["kyrax", "pandas-calamine", "pandas-openpyxl"],
    ("read_df", "large"): ["kyrax", "pandas-calamine"],
    ("write", "small"): list(TASKS["write"]),
    ("write", "medium"): list(TASKS["write"]),
    ("write", "large"): ["(data only)", "kyrax", "kyrax-stream", "xlsxwriter-cm"],
    ("edit", "small"): ["kyrax", "openpyxl"],
    ("edit", "medium"): ["kyrax", "openpyxl"],
    ("edit", "large"): ["kyrax"],  # openpyxl full load of 1M rows: minutes per run
}
REPEATS = {"small": 5, "medium": 3, "large": 3}
QUICK_SIZES = ("small", "medium")
NOT_COMPETITORS = {"(data only)"}


def is_kyrax(lib: str) -> bool:
    return lib.startswith("kyrax")


def measure(python, task, lib, size, fixture, workdir, env):
    out = os.path.join(workdir, f"out_{task}_{lib.strip('()').replace(' ', '_')}.xlsx")
    res = os.path.join(workdir, "result.json")
    if os.path.exists(res):
        os.remove(res)
    w0 = time.perf_counter()
    p = subprocess.Popen(
        [python, os.path.join(HERE, "worker.py"), task, lib, size, fixture, out, res],
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
    )
    # wait4 (not communicate/wait) so the child's own rusage is ours to read.
    err_chunks = []
    _, status, ru = os.wait4(p.pid, 0)
    wall = time.perf_counter() - w0
    p.returncode = os.waitstatus_to_exitcode(status)
    if p.stderr:
        err_chunks.append(p.stderr.read().decode(errors="replace"))
        p.stderr.close()
    if p.returncode != 0 or not os.path.exists(res):
        raise RuntimeError(f"{task}/{lib}/{size} failed rc={p.returncode}:\n{''.join(err_chunks)[-3000:]}")
    with open(res) as f:
        r = json.load(f)
    r.update(
        cpu_total=ru.ru_utime + ru.ru_stime,
        # rss_mb comes from the worker's VmHWM; wait4's ru_maxrss is kept only as
        # a cross-check (Linux folds the parent's RSS at exec into it).
        maxrss_wait4_mb=ru.ru_maxrss / 1024.0,
        wall_total=wall,
        out=out,
    )
    return r


def verify(task, lib, size, fixture, out, nrows):
    """Reopen the produced file with python-calamine; check shape and values."""
    if task in ("import", "read_values", "read_df") or lib in NOT_COMPETITORS:
        return "n/a"
    from python_calamine import CalamineWorkbook

    got = CalamineWorkbook.from_path(out).get_sheet_by_index(0).to_python()
    if task == "write":
        from fixtures import make_rows

        exp = make_rows(nrows)
        if len(got) != len(exp):
            return f"FAIL rows {len(got)} != {len(exp)}"
        idxs = range(len(exp)) if nrows <= 100_000 else (0, 1, len(exp) // 2, len(exp) - 1)
        for idx in idxs:
            g, e = [_norm(x) for x in got[idx]], [_norm(x) for x in exp[idx]]
            if g != e:
                return f"FAIL row {idx}: {g!r} != {e!r}"
        return "ok (reopens, all values)" if nrows <= 100_000 else "ok (reopens, spot rows)"
    # edit: full losslessness on values: every cell equals the source except edits.
    src = CalamineWorkbook.from_path(fixture).get_sheet_by_index(0).to_python()
    k = min(EDIT_CELLS, nrows)
    if len(got) != len(src):
        return f"FAIL rows {len(got)} != {len(src)}"
    bad = 0
    for i, (g, s) in enumerate(zip(got, src)):
        r = i + 1
        exp = list(s) + [""] * (len(g) - len(s))
        if 2 <= r <= k + 1:
            exp[1] = r * 0.5
            exp[NCOLS] = f"edited-{r}"
        if list(g) != exp:
            bad += 1
    return "ok (all values lossless)" if bad == 0 else f"FAIL {bad} rows differ"


def _norm(x):
    """Normalize reader representation: numbers -> float, midnight datetime -> date."""
    if isinstance(x, bool) or x is None:
        return x
    if isinstance(x, (int, float)):
        return float(x)
    if isinstance(x, dt.datetime) and x.time() == dt.time(0):
        return x.date()
    return x


def summarize(vals):
    vals = sorted(vals)
    med = statistics.median(vals)
    return {"median": med, "min": vals[0], "max": vals[-1], "n": len(vals),
            "noise_pct": (100.0 * (vals[-1] - vals[0]) / med) if med else 0.0}


def run(args):
    python = args.python
    env = dict(os.environ)
    env["PYTHONPATH"] = os.path.join(REPO, "python") + (
        os.pathsep + env["PYTHONPATH"] if env.get("PYTHONPATH") else ""
    )
    env.setdefault("PYTHONHASHSEED", "0")
    cache = args.cache
    sizes = [s for s in ("small", "medium", "large") if s in args.sizes]
    if args.quick:
        sizes = [s for s in sizes if s in QUICK_SIZES]
    t_start = time.perf_counter()
    for s in sizes:
        ensure_fixture(cache, s)
    rows = []
    reuse = {}
    if args.reuse:
        with open(args.reuse) as f:
            reuse = {(r["task"], r["size"], r["lib"]): r for r in json.load(f)["rows"]}
    with tempfile.TemporaryDirectory(prefix="work-", dir=cache) as workdir:
        for (task, size), libs in MATRIX.items():
            if size not in sizes or (args.tasks and task not in args.tasks):
                continue
            if args.quick or args.kyrax_only or reuse:
                libs = [lib for lib in libs if is_kyrax(lib)]
            if reuse:
                rows.extend(dict(r, status="MEASURED (reused from " + os.path.basename(args.reuse) + ")")
                            for (t2, s2, l2), r in reuse.items()
                            if t2 == task and s2 == size and not is_kyrax(l2))
            if args.libs:
                libs = [lib for lib in libs if lib in args.libs]
            if not libs:
                continue
            fixture = os.path.join(cache, f"fixture_v1_{size}.xlsx")
            n = args.repeats or REPEATS[size]
            samples = {lib: [] for lib in libs}
            verdicts = {}
            for rep in range(n):  # round-robin across libs to spread drift
                for lib in libs:
                    r = measure(python, task, lib, size, fixture, workdir, env)
                    samples[lib].append(r)
                    if rep == 0:
                        verdicts[lib] = verify(task, lib, size, fixture, r["out"], SIZES[size])
                    print(f"  {task:<11} {size:<6} {lib:<16} rep{rep} cpu_task={r['cpu_task']:.3f}s "
                          f"rss={r['rss_mb']:.0f}MB", file=sys.stderr, flush=True)
            for lib in libs:
                ss = samples[lib]
                rows.append({
                    "task": task, "size": size, "rows": SIZES[size], "lib": lib,
                    "status": "MEASURED",
                    "cpu_task_s": summarize([x["cpu_task"] for x in ss]),
                    "cpu_total_s": summarize([x["cpu_total"] for x in ss]),
                    "rss_mb": summarize([x["rss_mb"] for x in ss]),
                    "wall_task_s": summarize([x["wall_task"] for x in ss]),
                    "shape": ss[0]["shape"],
                    "verify": verdicts.get(lib, "n/a"),
                })
    _ratios(rows)
    meta = {
        "date_utc": dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%d %H:%M UTC"),
        "git": _git(),
        "python": sys.version.split()[0],
        "platform": platform.platform(),
        "cpus": os.cpu_count(),
        "mode": ("quick" if args.quick else "full") + (" (competitors reused)" if args.reuse else ""),
        "harness_runtime_s": round(time.perf_counter() - t_start, 1),
        "versions": _versions(python, env),
    }
    return {"meta": meta, "rows": rows}


def _ratios(rows):
    groups = {}
    for r in rows:
        groups.setdefault((r["task"], r["size"]), []).append(r)
    for grp in groups.values():
        comp = [r for r in grp if not is_kyrax(r["lib"]) and r["lib"] not in NOT_COMPETITORS]
        if not comp:
            continue
        best_cpu = min(comp, key=lambda r: r["cpu_task_s"]["median"])
        best_rss = min(comp, key=lambda r: r["rss_mb"]["median"])
        for r in grp:
            if is_kyrax(r["lib"]):
                r["vs_best_cpu"] = {"lib": best_cpu["lib"],
                                    "ratio": r["cpu_task_s"]["median"] / best_cpu["cpu_task_s"]["median"]}
                r["vs_best_rss"] = {"lib": best_rss["lib"],
                                    "ratio": r["rss_mb"]["median"] / best_rss["rss_mb"]["median"]}


def _git():
    try:
        h = subprocess.check_output(["git", "-C", REPO, "rev-parse", "--short", "HEAD"], text=True).strip()
        dirty = subprocess.call(["git", "-C", REPO, "diff", "--quiet", "--", "src", "python"]) != 0
        return h + ("+dirty" if dirty else "")
    except Exception:
        return "unknown"


def _versions(python, env):
    code = (
        "import importlib.metadata as m, json\n"
        "out = {}\n"
        "for p in ['openpyxl','XlsxWriter','python-calamine','pandas','pyarrow','numpy']:\n"
        "    try: out[p] = m.version(p)\n"
        "    except Exception: out[p] = None\n"
        "import kyrax, os; out['kyrax'] = os.path.dirname(kyrax.__file__)\n"
        "print(json.dumps(out))\n"
    )
    try:
        return json.loads(subprocess.check_output([python, "-c", code], env=env, text=True))
    except Exception as e:  # pragma: no cover
        return {"error": str(e)}


def fmt_md(data):
    m = data["meta"]
    lines = [
        f"# kyrax benchmark scoreboard ({m['mode']})",
        "",
        f"- Date: {m['date_utc']} · commit `{m['git']}` · Python {m['python']} · "
        f"{m['cpus']} CPUs · {m['platform']}",
        f"- Versions: " + ", ".join(f"{k} {v}" for k, v in m["versions"].items() if k != "kyrax"),
        f"- Harness runtime: {m['harness_runtime_s']} s",
        "- All rows MEASURED on this machine with this harness (no estimates).",
        "- `CPU-s` = user+sys of the timed section (library import + task, all threads), median.",
        "  `noise` = (max-min)/median over the repeats. `RSS` = worker peak RSS (VmHWM).",
        "  `ratio` = kyrax median / best competitor median (>1 = kyrax worse).",
        "",
        "| task | size | lib | CPU-s | CPU noise | proc CPU-s | peak RSS MB | RSS min-max | wall-s | n | ratio CPU (vs) | ratio RSS (vs) | verify | status |",
        "|---|---|---|---:|---:|---:|---:|---:|---:|---:|---|---|---|---|",
    ]
    for r in data["rows"]:
        c, t, rs, w = r["cpu_task_s"], r["cpu_total_s"], r["rss_mb"], r["wall_task_s"]
        rc = f"{r['vs_best_cpu']['ratio']:.2f} ({r['vs_best_cpu']['lib']})" if "vs_best_cpu" in r else ""
        rr = f"{r['vs_best_rss']['ratio']:.2f} ({r['vs_best_rss']['lib']})" if "vs_best_rss" in r else ""
        lib = f"**{r['lib']}**" if is_kyrax(r["lib"]) else r["lib"]
        lines.append(
            f"| {r['task']} | {r['size']} ({r['rows']:,}) | {lib} | {c['median']:.3f} | "
            f"{c['noise_pct']:.0f}% | {t['median']:.3f} | {rs['median']:.0f} | "
            f"{rs['min']:.0f}-{rs['max']:.0f} | {w['median']:.3f} | {c['n']} | {rc} | {rr} | "
            f"{r['verify']} | {r['status']} |"
        )
    return "\n".join(lines) + "\n"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--python", default=os.environ.get("BENCH_PYTHON", sys.executable),
                    help="interpreter of the competitor venv (default: $BENCH_PYTHON or this one)")
    ap.add_argument("--cache", default=os.path.join(HERE, ".cache"), help="fixture cache dir")
    ap.add_argument("--quick", action="store_true", help="kyrax rows only, small+medium")
    ap.add_argument("--kyrax-only", action="store_true", help="kyrax rows only, all sizes")
    ap.add_argument("--sizes", nargs="+", default=list(SIZES), choices=list(SIZES))
    ap.add_argument("--tasks", nargs="+", choices=list(TASKS))
    ap.add_argument("--libs", nargs="+")
    ap.add_argument("--reuse", metavar="JSON",
                    help="copy competitor rows from a previous results file; measure kyrax only")
    ap.add_argument("--repeats", type=int, help="override repeats per size")
    ap.add_argument("--out", default=None, help="results basename (default: latest / quick)")
    ap.add_argument("--render", metavar="JSON", help="only re-render the markdown of a results file")
    args = ap.parse_args()
    if args.render:
        with open(args.render) as f:
            data = json.load(f)
        order = {k: i for i, k in enumerate(MATRIX)}
        data["rows"].sort(key=lambda r: order.get((r["task"], r["size"]), 99))
        md_path = os.path.splitext(args.render)[0] + ".md"
        with open(md_path, "w") as f:
            f.write(fmt_md(data))
        print(md_path)
        return
    data = run(args)
    name = args.out or ("quick" if args.quick else "latest")
    os.makedirs(os.path.join(HERE, "results"), exist_ok=True)
    with open(os.path.join(HERE, "results", f"{name}.json"), "w") as f:
        json.dump(data, f, indent=1)
    md = fmt_md(data)
    with open(os.path.join(HERE, "results", f"{name}.md"), "w") as f:
        f.write(md)
    print(md)


if __name__ == "__main__":
    main()
