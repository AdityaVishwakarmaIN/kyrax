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
Master Opus (alignment)
 ├─ Orchestrator Opus A (planning, management) ── Sonnet (high) implementors
 └─ Orchestrator Opus B (planning, management) ── Sonnet (high) implementors
```

- **Master Opus** (this session): after the baseline scoreboard exists, decides
  the split between teams A and B by where kyrax loses most. Each team owns
  separate files and areas; no overlap. Reviews both team plans before work
  starts, checks goal and valve alignment after EVERY merge (benchmark gate
  plus goal check), and does a final check before the push. Settles any dispute
  between orchestrators. Only Master merges into the work branch and pushes.
- **Orchestrator Opus** (two): owns one team's area. Writes its task files
  before any implementation, reads each Sonnet implementor's plan and must
  approve it ("go") before that job starts, reviews results, reports to Master.
- **Sonnet (high) implementors**: unlimited, parallel, one git worktree each.
  Write the plan to `_work/<id>.md`, ask the orchestrator for approval, then
  implement, test and report back.
- **Early finish:** a team with no worthwhile jobs left stops and closes. It
  does not take the other team's work and spends nothing more.
- **Fallback:** if a subagent cannot spawn subagents in this runtime, Master
  spawns the Sonnet implementors on the orchestrators' behalf. The approval
  chain stays identical: Sonnet → its orchestrator → Master.

## Rules
0. **Preflight (first 5 min, cloud):** confirm the Rust toolchain builds kyrax,
   `maturin` works, and the competitor packages install. If anything is blocked,
   stop and report; do not burn the time budget.
1. **Branch:** from the branch this session started on, create
   `ultimate/benchmark-domination`. All changes go there. Push that branch once,
   after completion. Never push `main`, tags or PRs. Never publish to PyPI.
2. **Planning files:** before any work, Master writes `_work/MAIN.md` (plan,
   split, gates). Each orchestrator writes `_work/<team>.md`, and each subtask
   gets `_work/<id>.md` (scope, files, success metric) BEFORE implementation.
   All are temporary, never committed, and deleted at completion.
3. **Approval gate:** no task or subtask is implemented until its orchestrator
   Opus has checked the plan against this goal and valve and said "go".
4. **Cloud first:** do everything that runs on Linux in the cloud. Anything that
   needs the user's device (real Excel open-check, laptop-class timings) is not
   a blocker; list it as a handoff item.
5. **Measure first:** build the harness in unshipped `bench/`. It is the only
   place competitors (openpyxl, XlsxWriter, calamine, pandas) may be imported;
   never in the package, its tests, or its dependencies. Baseline every task,
   then attack the biggest losses first.
6. **Architecture law:** all feature logic lives in Rust; Python stays a thin
   binding. openpyxl is never imported or reused by kyrax itself.
7. **No regressions:** no new runtime dependencies, no new test failures, no API
   breaks, no file corruption, no speed or RSS regressions beyond noise.
8. **Hygiene:** never delete repo files (archive instead). Never overclaim:
   every "faster than" needs a same-harness number, and measured results stay
   separate from estimates.
9. **Finish:** delete all `_work/` files and agent worktrees/branches, confirm
   the tree is clean, push the branch, then give a plain report, decision
   first: before/after table, remaining losses, handoff items, commit hash.
