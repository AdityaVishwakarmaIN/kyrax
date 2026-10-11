# GOAL — Make kyrax the last Excel package anyone needs

## Objective
Within 2–3 hours of wall-clock time, move kyrax (`kyrax-main/kyrax`) along its current trajectory to the point where it handles 99% of real Excel tasks: full read, write and edit of `.xlsx`/`.xlsm`, read-only `.xls`/`.xlsb`/`.ods`. Nobody should need openpyxl, xlsxwriter, xlrd or pandas' Excel engines next to it. Speed of completion is the top priority. A close second is never breaking what already works.

## Step 0: feasibility verdict, before any code (≤10 min)
Read `kyrax-main/CLAUDE.md`, `kyrax-main/architecture_snapshot.txt` and the repo. Then post a short verdict:
- **Feasible:** the ordered work list, the parallel split, and a time plan that fits 2–3 hours. Start immediately; do not wait for approval.
- **Not feasible in 2–3 hours:** say so plainly, before any implementation. State what can be reached, what can't, and why. Then stop and wait.

## Hard rules
1. **Branching:** branch from `cleanup/minimize-repo` (commit `a7a42d6`) into a new branch `ultimate/excel-engine`. Never commit to or rewrite any other branch.
2. **Publishing:** nothing goes to PyPI. No tags, no `maturin publish`, no release workflow triggers. At the end, push the branch to `origin` only. No pull request.
3. **Architecture law (CLAUDE.md):** all feature logic lives in Rust; Python stays a thin binding. openpyxl is never imported or used as a dependency.
4. **Balanced, no regressions:** keep these in balance. A gain in one area must not cost another. For example, a faster read must not slow down write, and vice versa.
   - **Dependencies:** add no new runtime dependency unless it is truly unavoidable. Dev and test tools are fine.
   - **Speed and memory:** take a benchmark baseline first and re-check at the end. No regression on any existing read, write or edit path.
   - **API:** every existing public Python call keeps working with the same behavior.
5. **Don't break the existing implementation:** the current tests must stay green after every merge into the branch. If a change breaks something and can't be fixed fast, revert it and move on.
6. **Skip work that's too hard or niche:** if an item would take too much of the timebox or matters to very few users, skip it and list it in the final report. Don't half-build it.

## Allowed tools
- Install whatever the build or tests need: `winget`, GNU `make`, Rust and Python dev tools.
- Use the locally installed Microsoft Excel through COM as a validation oracle. It checks that files open with no repair prompt and that formula results match Excel.
- Run many parallel subagents. The orchestrator plans, splits, merges and verifies. Subagents implement and test in non-overlapping areas, such as one owner per module or file family.

## Direction: progress the current trajectory, don't reinvent it
Work in impact order, using `architecture_snapshot.txt` as the starting backlog:
1. **Familiar door:** `load_workbook()` and the openpyxl-style API cover everyday workflows end to end, with the turbo engine behind it (A1). Charts, images, tables, conditional formatting, data validation, comments, defined names and print setup should be authorable and editable through that one API.
2. **Fidelity gaps:** B1–B6 (styles, named styles, gradient fills, table styles, `copy_worksheet` merges, `date1904`).
3. **Scale:** constant-memory streaming write (A2) and the memory invariants (C2).
4. **Interchange:** Parquet I/O (A4), only if it adds no heavy dependency. Fix the pandas engine registration order (A6).
5. **Formula engine:** widen and verify function coverage against Excel.
6. **Tier-3 authoring (A5):** create slicers, sparklines and similar from scratch, only where cheap.

Before each wave, theorize briefly what is missing for the 99% target and what the next highest-value step is. Then execute.

## Testing: maximize coverage
- Start by recording baseline counts: tests passing, tests failing, coverage.
- Bring back the archived tests where it is cheap. Their fixtures are in `graveyard/kyrax-dev/kyrax-junk/tests/fixtures`, and the 14 test files are in `graveyard/kyrax-dev/repo-cleanup-2026-10-11/python/tests/`.
- Every new feature gets Rust and Python tests. Round-trip tests check read → edit → write → re-read, plus Excel COM spot checks.
- Keep the tests fast, not exhaustive. Prove each claim with one solid test rather than many slow ones.

## Finish (last ~20 min)
Run the full test suite, confirm the no-regression benchmark checks, commit, and push `ultimate/excel-engine`. Then report in plain words:
- what is now covered and what share of everyday Excel tasks that reaches;
- what was skipped and why;
- measured numbers versus baseline, with measured results kept clearly separate from estimates;
- the commit hash.

Never overclaim. State failures plainly.
