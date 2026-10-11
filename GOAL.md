# GOAL — kyrax: lowest cost per task

## Goal
Minimize CPU-seconds and peak RSS per task versus openpyxl, XlsxWriter,
calamine and pandas, on laptop and datacenter, from small to multi-million-row
workbooks, with OOXML coverage, lossless round-trips, zero-dependency core.

## Valve (realistic limits)
- Ship only if no benchmark regresses beyond noise.
- Rare features are opt-in.
- Stop when marginal gains fall below measurement variance, or at 3 h
  wall-clock, whichever comes first.

## Hierarchy

```
Master Opus (alignment, sanity check, accountable head)
 ├─ Orchestrator Opus A (plans + manages) ── Sonnet (high) implementors
 └─ Orchestrator Opus B (plans + manages) ── Sonnet (high) implementors
```

- **Master Opus** (this session) is the sanity check and the accountable head.
  Minimal plate, maximum responsibility. It does NOT write plans, code, merges
  or benchmarks. It only reads, signs off, vetoes and pushes. If things go
  south, it answers for it.
  - Authority: veto any plan, halt any team, order any merge reverted.
  - Duties (nothing else): (1) set the A/B split from the baseline scoreboard;
    (2) sign off every subtask list before any Sonnet starts; (3) sign off
    every merge result (gate numbers + goal alignment); (4) final check and the
    single push.
- **Orchestrator Opus** (two) owns one team's area and does all the planning
  and management: writes the team to-do and every subtask file, shares them
  with Master, hands approved subtasks to Sonnet, reviews results, merges its
  team's work, runs the benchmark gate, and reports numbers to Master.
- **Sonnet (high) implementors**: unlimited, parallel, one git worktree each.
  Implement exactly the subtask file they are given, test, and report to their
  orchestrator. If blocked or unclear, ask the orchestrator; never guess.
- **Early finish:** a team with no worthwhile jobs left stops and closes. It
  does not take the other team's work and spends nothing more.
- **Fallback:** if a subagent cannot spawn subagents in this runtime, Master
  spawns the Sonnet implementors on the orchestrators' behalf. The flow stays
  identical: Orchestrator plans → Master signs off → Sonnet implements.

## Flow (every subtask, no exceptions)
1. **Orchestrator Opus** writes the subtask file `_work/<team>/<id>.md`
   (scope, files, success metric, gate) and lists it in `_work/<team>.md`.
2. **Master Opus** reads it and checks alignment with this goal and valve.
   Reply: approved, or changes needed. Nothing proceeds without approval.
3. **Orchestrator** hands the approved file to a **Sonnet** implementor.
4. **Sonnet** implements in its worktree, tests, reports back.
5. **Orchestrator** reviews, merges into the work branch, runs the gate, and
   sends Master the numbers.
6. **Master** signs off, or orders a revert.

## Rules
0. **Preflight (first 5 min, cloud):** confirm the Rust toolchain builds kyrax,
   `maturin` works, and the competitor packages install. If anything is blocked,
   stop and report; do not burn the time budget.
1. **Branch:** from the branch this session started on, create
   `ultimate/benchmark-domination`. All changes go there. Push that branch once,
   after completion. Never push `main`, tags or PRs. Never publish to PyPI.
2. **Baseline first:** Orchestrator A builds the harness in unshipped `bench/`
   and records the baseline scoreboard. `bench/` is the only place competitors
   (openpyxl, XlsxWriter, calamine, pandas) may be imported; never in the
   package, its tests, or its dependencies. Master then sets the split.
3. **Separate areas:** teams A and B own separate files; no overlap.
4. **Planning files are temporary:** `_work/MAIN.md` (Master's split and gates),
   `_work/<team>.md`, `_work/<team>/<id>.md`. All are created BEFORE the work
   they describe, never committed, and deleted at completion.
5. **Cloud first:** do everything that runs on Linux in the cloud. Anything that
   needs the user's device (real Excel open-check, laptop-class timings) is not
   a blocker; list it as a handoff item.
6. **Architecture law:** all feature logic lives in Rust; Python stays a thin
   binding. openpyxl is never imported or reused by kyrax itself.
7. **No regressions:** no new runtime dependencies, no new test failures, no API
   breaks, no file corruption, no speed or RSS regressions beyond noise.
8. **Hygiene:** never delete repo files (archive instead). Never overclaim:
   every "faster than" needs a same-harness number, and measured results stay
   separate from estimates.
9. **Finish:** delete all `_work/` files and agent worktrees/branches, confirm
   the tree is clean, then Master pushes the branch and gives a plain report,
   decision first: before/after table, remaining losses, handoff items,
   commit hash.
