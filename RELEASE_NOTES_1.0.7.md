# kyrax 1.0.7

Release date: 2026-08-18

## Highlights

- Fixes the main CI pipeline (was red under Rust 1.97.1).
- Pins `ethnum 1.5.3` so the Polars feature builds again with the current
  toolchain.
- Makes `direct_eval` hermetic: it now reads the repository-local fixture
  `tests_formulas/fail_rows.csv` instead of an external file absent from
  clean CI checkouts.
- Resolves the strict-Clippy backlog (`cargo clippy --tests -- -D warnings`
  passes with zero errors): 1088 `assert_eq` ambiguity compile errors, 182
  disallowed-macro lints (tests now use `pretty_assertions`), and 47
  production style lints. All changes are behavior-preserving.
- Completes the `_TurboSheet` / `_TurboReader` type stubs and remaining
  extension exports in `_kyrax.pyi`, clearing ~44 static type errors.
- Sets `fail-fast: false` on the CI test matrix so one red job no longer
  cancels the other 27.

## Validation

- `cargo clippy --tests -- -D warnings`: 0 errors.
- `cargo check --no-default-features --features polars`: passes.
- `cargo test --lib`: 1277 passed, 0 failed.
- `cargo test --test direct_eval`: passes (pinned MATCH=3, MISMATCH=2,
  EVAL_ERROR=2).
- `ruff check`, `ruff format --check`, `ty check`: all pass.
- Known pre-existing issue, unchanged by this release: the machine-specific
  memory-ratio test `tests/streaming_memory.rs` fails on local Windows in
  both debug and release (documented release-only in its header); it is not
  exercised by the release wheel build.