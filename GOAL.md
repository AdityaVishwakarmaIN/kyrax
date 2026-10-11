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

## Rules
0. **Preflight (first 5 min, cloud):** confirm the Rust toolchain builds kyrax,
   `maturin` works, and the competitor packages install. If anything is blocked,
   stop and report; do not burn the time budget.
1. **Branch:** from the branch this session started on, create
   `ultimate/benchmark-domination`. All changes go there. Push that branch once,
   after completion. Never push `main`, tags or PRs. Never publish to PyPI.
2. **Planning files:** before any work, the main agent writes `_work/MAIN.md`
   (plan, workstreams, gates). Each subtask gets `_work/<id>.md` (scope, files,
   success metric) BEFORE implementation. All are temporary, never committed,
   and deleted at completion.
3. **Sanity gate:** before implementing any task or subtask, a Sonnet (high)
   subagent checks the next action plan against this goal and valve. Implement
   only after it passes; revise the plan if it does not.
4. **Parallelism:** unlimited Sonnet (high) subagents, one per independent
   workstream, each in its own git worktree off the working branch. The main
   agent merges them one at a time and runs the benchmark gate after each merge.
5. **Cloud first:** do everything that runs on Linux in the cloud. Anything that
   needs the user's device (real Excel open-check, laptop-class timings) is not
   a blocker; list it as a handoff item.
6. **Measure first:** build the harness in unshipped `bench/`. It is the only
   place competitors (openpyxl, XlsxWriter, calamine, pandas) may be imported;
   never in the package, its tests, or its dependencies. Baseline every task,
   then attack the biggest losses first.
7. **Architecture law:** all feature logic lives in Rust; Python stays a thin
   binding. openpyxl is never imported or reused by kyrax itself.
8. **No regressions:** no new runtime dependencies, no new test failures, no API
   breaks, no file corruption, no speed or RSS regressions beyond noise.
9. **Hygiene:** never delete repo files (archive instead). Never overclaim:
   every "faster than" needs a same-harness number, and measured results stay
   separate from estimates.
10. **Finish:** delete all `_work/` files and agent worktrees/branches, confirm
    the tree is clean, push the branch, then give a plain report, decision
    first: before/after table, remaining losses, handoff items, commit hash.
