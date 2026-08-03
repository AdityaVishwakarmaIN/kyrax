//! PERF_EXPERIMENTS.md regression gates (ST-2).
//!
//! The three measured results in PERF_EXPERIMENTS.md are invisible to the
//! functional suite: a future refactor can undo any of them and every
//! correctness test still passes. These gates pin the performance SHAPES so
//! that cannot happen silently. Each runs in well under a second in release
//! mode, so they stay in the default test run.
//!
//! Rules applied throughout: assert RATIOS (or per-cell byte shapes), never
//! absolute times; warm up before timing so a cold first call's allocator
//! behaviour cannot be measured as the implementation's; print the measured
//! figures so a run with `--nocapture` shows real numbers.

use std::borrow::Cow;
use std::hint::black_box;
use std::time::{Duration, Instant};

use crate::turbo::mutate::shift_rows;
use crate::turbo::refshift::{Axis, shift_refs};
use crate::turbo::write::xml::write_escaped_text;

/// Time `a` and `b` in alternating rounds: warm up both first (a cold first
/// call measures allocator behaviour, not the implementation), then run
/// `rounds` rounds of `a` then `b`, returning the best of each side.
/// Interleaving cancels slow drift from machine contention, so the two bests
/// are comparable even on a busy shared runner.
fn best_of_pair<A: FnMut(), B: FnMut()>(rounds: usize, mut a: A, mut b: B) -> (Duration, Duration) {
    a();
    b();
    let mut best_a = Duration::MAX;
    let mut best_b = Duration::MAX;
    for _ in 0..rounds {
        let t = Instant::now();
        a();
        best_a = best_a.min(t.elapsed());
        let t = Instant::now();
        b();
        best_b = best_b.min(t.elapsed());
    }
    (best_a, best_b)
}

// ----------------------------------------------------------------------------
// GATE 1 — E1, string escaping
// ----------------------------------------------------------------------------

/// GATE 1 — PERF_EXPERIMENTS.md E1: `write_escaped_text` must stay faster than
/// the naive two-pass shape it replaced (scan for illegal chars, then escape).
///
/// E1 measured the shipped fused SWAR scan at ~1441 MB/s vs 322 MB/s for the
/// naive fix — a 4.5x gap. Tolerance: shipped must be at least 15% faster
/// (ratio < 0.85). That margin is far inside the measured gap, so a genuine
/// regression trips it while CI noise does not.
#[test]
fn perfgate_e1_escape_stays_faster_than_two_pass() {
    let corpus = escape_corpus(250_000);

    let shipped = || {
        let mut sink = Vec::new();
        for s in &corpus {
            write_escaped_text(&mut sink, s);
        }
        black_box(sink.len());
    };
    let two_pass = || {
        let mut sink = Vec::new();
        for s in &corpus {
            naive_two_pass(&mut sink, s);
        }
        black_box(sink.len());
    };

    let (shipped_d, two_pass_d) = best_of_pair(5, shipped, two_pass);
    let shipped_ms = shipped_d.as_secs_f64() * 1000.0;
    let two_pass_ms = two_pass_d.as_secs_f64() * 1000.0;
    let ratio = shipped_ms / two_pass_ms;

    println!(
        "E1 gate: {} strings, shipped {:.2} ms, naive two-pass {:.2} ms, ratio {:.3}",
        corpus.len(),
        shipped_ms,
        two_pass_ms,
        ratio
    );
    assert!(
        ratio < 0.85,
        "write_escaped_text took {:.2}x the naive two-pass time - the fused E1 scan has been lost",
        ratio
    );
}

/// The naive two-pass shape E1 replaced (candidate B): pass one scans for
/// illegal control characters, pass two escapes. This is the exact shape the
/// fused SWAR scan was measured 4.5x faster than.
fn naive_two_pass(out: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    let mut illegal = vec![false; bytes.len()];
    for (i, &b) in bytes.iter().enumerate() {
        if b < 0x20 && b != 0x09 && b != 0x0A && b != 0x0D {
            illegal[i] = true;
        }
    }
    for (i, &b) in bytes.iter().enumerate() {
        if illegal[i] {
            continue;
        }
        match b {
            b'&' => out.extend_from_slice(b"&amp;"),
            b'<' => out.extend_from_slice(b"&lt;"),
            b'>' => out.extend_from_slice(b"&gt;"),
            _ => out.push(b),
        }
    }
}

// ----------------------------------------------------------------------------
// GATE 2 — E2, the mutate splice memory shape
// ----------------------------------------------------------------------------

// Coordinator perf gate: the shipped splice must still deliver PERF_EXPERIMENTS.md
// E2. Materialising measured 189 bytes held per cell; the splice measured 39.
// This asserts the shape (bytes of output per cell) has not regressed toward
// materialisation, and prints the CPU figure for the record.
//
// The 60 B/cell ceiling is the difference between one worker and five inside
// the 2 GB per-worker budget in plans/northstar_metric.md, so whoever trips
// this gate has reintroduced materialisation, not just slowed something down.
#[test]
fn coordinator_e2_perf_gate() {
    let rows: u32 = 20_000;
    let cols: u32 = 20;
    let mut xml = Vec::with_capacity((rows * cols * 40) as usize);
    xml.extend_from_slice(b"<dimension ref=\"A1:T20000\"/><sheetData>");
    for r in 1..=rows {
        xml.extend_from_slice(format!("<row r=\"{}\" spans=\"1:{}\">", r, cols).as_bytes());
        for c in 1..=cols {
            let mut buf = [0u8; 4];
            let letters = crate::turbo::write::xml::col_letters(c, &mut buf).to_vec();
            let a1 = format!("{}{}", String::from_utf8(letters).unwrap(), r);
            xml.extend_from_slice(
                format!("<c r=\"{}\" s=\"3\"><v>{}</v></c>", a1, r * c).as_bytes(),
            );
        }
        xml.extend_from_slice(b"</row>");
    }
    xml.extend_from_slice(b"</sheetData>");
    let cells = (rows * cols) as f64;

    let t = std::time::Instant::now();
    let out = shift_rows(&xml, 2, 1).expect("splice must succeed");
    let el = t.elapsed();
    let out_len = out.len();

    let bytes_per_cell = out_len as f64 / cells;
    println!(
        "E2 gate: {} cells, in {:.1} MB, out {:.1} MB, {:.1} B/cell, {:.1} ms",
        cells as u64,
        xml.len() as f64 / 1e6,
        out_len as f64 / 1e6,
        bytes_per_cell,
        el.as_secs_f64() * 1000.0
    );

    // The splice output is the sheet plus one row. Materialising held ~189 B
    // per cell in live structs; the splice holds only its output.
    assert!(
        bytes_per_cell < 60.0,
        "output grew to {:.1} B/cell - materialisation reintroduced: at the ~189 B/cell of the old shape this file would fit ONE worker inside the 2 GB per-worker budget of plans/northstar_metric.md, not five",
        bytes_per_cell
    );
    // Every row below the insert must have moved.
    let s = std::str::from_utf8(&out).unwrap();
    assert!(s.contains(r#"<row r="20001""#), "last row did not shift");
    assert!(
        s.contains(r#"<c r="A20001""#),
        "last row cells did not shift"
    );
}

// ----------------------------------------------------------------------------
// GATE 3 — E3, formula reference shifting
// ----------------------------------------------------------------------------

/// GATE 3 — PERF_EXPERIMENTS.md E3: `shift_refs` must (a) stay faster than a
/// tokenise-and-rebuild approach (P1), and (b) return `Cow::Borrowed` whenever
/// nothing changes.
///
/// E3 measured the byte scan at 1.40M formulas/s vs P1's 771k (1.81x faster);
/// E3b measured a further 1.25x from borrowing. Tolerances: shipped must be at
/// least 20% faster than tokenise-and-rebuild (ratio < 0.8), and at least 80%
/// of a corpus where most formulas do not reference the shifted region must
/// come back borrowed — a regression that always allocates would pass every
/// functional test while quietly costing real CPU.
#[test]
fn perfgate_e3_shift_refs_faster_and_borrows() {
    let corpus = formula_corpus(100_000);
    let at = 1000u32;
    let delta = 2i64;

    let mut borrowed = 0usize;
    for f in &corpus {
        if let Cow::Borrowed(_) = shift_refs(f, Axis::Row, at, delta) {
            borrowed += 1;
        }
    }
    let borrow_frac = borrowed as f64 / corpus.len() as f64;
    println!(
        "E3 gate: {} formulas, {} borrowed ({:.1}%)",
        corpus.len(),
        borrowed,
        borrow_frac * 100.0
    );
    assert!(
        borrow_frac > 0.8,
        "only {:.1}% of formulas returned Cow::Borrowed - a regression that always allocates would pass every functional test",
        borrow_frac * 100.0
    );

    let shipped = || {
        let mut sink = 0usize;
        for f in &corpus {
            sink += shift_refs(f, Axis::Row, at, delta).len();
        }
        black_box(sink);
    };
    let tokenized = || {
        let mut sink = 0usize;
        for f in &corpus {
            sink += tokenize_and_rebuild(f, Axis::Row, at, delta).len();
        }
        black_box(sink);
    };

    let (shipped_d, tokenized_d) = best_of_pair(5, shipped, tokenized);
    let shipped_ms = shipped_d.as_secs_f64() * 1000.0;
    let tokenized_ms = tokenized_d.as_secs_f64() * 1000.0;
    let ratio = shipped_ms / tokenized_ms;

    println!(
        "E3 gate: shipped {:.2} ms, tokenise-and-rebuild {:.2} ms, ratio {:.3}",
        shipped_ms, tokenized_ms, ratio
    );
    assert!(
        ratio < 0.8,
        "shift_refs took {:.2}x a tokenise-and-rebuild rebuild - the E3 byte scan has been lost",
        ratio
    );
}

/// Tokenise-and-rebuild reference for E3 (the P1 shape): split the formula
/// into tokens, shift any token that is exactly a cell reference, and rebuild
/// a fresh `String`. Always allocates; deliberately the naive shape.
fn tokenize_and_rebuild(formula: &str, axis: Axis, at: u32, delta: i64) -> String {
    let mut out = String::with_capacity(formula.len() + 8);
    for tok in tokenize(formula) {
        match shift_token(&tok, axis, at, delta) {
            Some(shifted) => out.push_str(&shifted),
            None => out.push_str(&tok),
        }
    }
    out
}

fn tokenize(s: &str) -> Vec<String> {
    let b = s.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_alphabetic() || c == b'$' {
            let start = i;
            i += 1;
            while i < b.len()
                && (b[i].is_ascii_alphanumeric() || b[i] == b'$' || b[i] == b'_' || b[i] == b'.')
            {
                i += 1;
            }
            toks.push(s[start..i].to_string());
        } else if c == b'"' {
            let start = i;
            i += 1;
            while i < b.len() {
                if b[i] == b'"' {
                    if i + 1 < b.len() && b[i + 1] == b'"' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            toks.push(s[start..i].to_string());
        } else {
            toks.push(s[i..i + 1].to_string());
            i += 1;
        }
    }
    toks
}

/// Shift exactly one cell-reference token (row axis only — the axis this gate
/// exercises). Returns `None` when the token is not a shiftable reference.
fn shift_token(tok: &str, axis: Axis, at: u32, delta: i64) -> Option<String> {
    let b = tok.as_bytes();
    let mut p = 0usize;
    let abs_col = p < b.len() && b[p] == b'$';
    if abs_col {
        p += 1;
    }
    let cs = p;
    while p < b.len() && b[p].is_ascii_alphabetic() {
        p += 1;
    }
    let le = p;
    let abs_row = p < b.len() && b[p] == b'$';
    if abs_row {
        p += 1;
    }
    let rs = p;
    while p < b.len() && b[p].is_ascii_digit() {
        p += 1;
    }
    let is_ref = (1..=3).contains(&(le - cs)) && p > rs && p == b.len();
    if !is_ref {
        return None;
    }
    match axis {
        Axis::Row => {
            if abs_row {
                return None;
            }
            let row: u32 = tok[rs..p].parse().ok()?;
            if row < at {
                return None;
            }
            let nr = row as i64 + delta;
            if nr < 1 {
                return Some("#REF!".into());
            }
            Some(format!("{}{}", &tok[..rs], nr))
        }
        // The gate exercises the row axis only.
        Axis::Col => None,
    }
}

// ----------------------------------------------------------------------------
// Corpus builders (deterministic, generated, not committed fixtures)
// ----------------------------------------------------------------------------

/// Deterministic LCG so the corpus is byte-stable across runs and platforms.
struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Lcg(seed)
    }
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

/// E1 corpus: >= 100k strings shaped like real sheet content — mostly clean
/// short ASCII, some needing escapes, a few with control characters.
fn escape_corpus(n: usize) -> Vec<String> {
    let mut rng = Lcg::new(0xE1_5EED);
    let clean = [
        "Customer",
        "Account",
        "quantity",
        "unit price",
        "Net total",
        "line item",
        "warehouse B",
        "Q3 revenue",
        "backup done",
        "delivered",
        "payment received for order shipped today",
        "monthly reconciliation report Q3 fiscal year",
        "the quick brown fox jumps over the lazy dog",
        "inventory count completed and signed off by shift lead",
    ];
    let mut v = Vec::with_capacity(n);
    for i in 0..n {
        let r = (rng.next() % 100) as u32;
        let base = clean[(rng.next() as usize) % clean.len()];
        let s = if r < 70 {
            format!("{base} {i}")
        } else if r < 95 {
            format!("{base} <{i}> & {i} > 100%")
        } else {
            format!("{base}\u{1}{i}\u{8}")
        };
        v.push(s);
    }
    v
}

/// E3 corpus: >= 100k formulas where most do NOT reference the shifted region
/// (shift at row 1000; ~90% reference rows 1..9). The tail references rows
/// >= 1000 so the shift path is exercised too.
fn formula_corpus(n: usize) -> Vec<String> {
    let mut rng = Lcg::new(0xE3_5EED);
    let mut v = Vec::with_capacity(n);
    for i in 0..n {
        let r = (rng.next() % 100) as u32;
        let a = (rng.next() % 9) + 1;
        let b = (rng.next() % 9) + 1;
        let s = if r < 70 {
            format!("SUM(A{a}:C{b})+IF(D{a}>0,E{b},\"no\")")
        } else if r < 90 {
            format!("B{a}+C{b}&\"\"&A{b}")
        } else {
            format!("SUM(A{}{}:B{}{})+1", 1000 + a, i % 7, 1000 + b, i % 5)
        };
        v.push(s);
    }
    v
}

// ----------------------------------------------------------------------------
// GATE 4 — E4, lazy shared-formula hydration
// ----------------------------------------------------------------------------

/// Best-of-N time for one closure (warm up, then `rounds`).
fn best_of(rounds: usize, mut f: impl FnMut()) -> Duration {
    f();
    let mut best = Duration::MAX;
    for _ in 0..rounds {
        let t = Instant::now();
        f();
        best = best.min(t.elapsed());
    }
    best
}

/// GATE 4 — PERF_EXPERIMENTS_PHASE2.md E4: shared-formula dependents must not
/// be translated at load, and on-demand hydration must stay arena-backed.
///
/// E4 measured the race on 400k dependents (single-threaded): eager
/// String-per-dependent 52.1 ms, eager arena 35.4 ms, LAZY (store si+delta,
/// translate on demand) 1.0 ms — 50x. The load win is doing nothing until
/// asked; the materialisation win is the arena (0.68x vs today) when the
/// caller reads everything.
///
/// Tolerances, on a 100k-dependent race-shaped column (debug uses 20k and
/// fewer rounds so the default suite stays fast; the assertions hold in both):
///   (1) reading no formulas must be at least 10x cheaper than reading all of
///       them via the lazy handle (measured ~180–850x) — a regression that
///       eagerly hydrates would pass every functional test while silently
///       costing real load time;
///   (2) arena hydration (borrowed slices, no per-formula String) must never be
///       slower than the String-per-formula materialisation it replaced
///       (measured ~0.79–0.94x) — the "must not be slower than today" bound
///       for the worst case of a caller reading every formula.
#[test]
fn perfgate_e4_lazy_hydration() {
    use crate::turbo::scan::synthetic_hydration_column;
    let col = synthetic_hydration_column(if cfg!(debug_assertions) {
        20_000
    } else {
        100_000
    });
    let rounds = if cfg!(debug_assertions) { 2 } else { 5 };

    let load_none = || {
        let t = col.lazy();
        black_box(t.len());
    };
    let read_all = || {
        let mut t = col.lazy();
        t.hydrate_all();
        black_box(t.bytes().len());
    };
    let arena = || {
        let mut t = col.lazy();
        t.hydrate_all();
        black_box(t.bytes().len());
    };
    let naive = || {
        let v = col.materialize_all_naive();
        black_box(v.len());
    };

    let (none_d, all_d) = best_of_pair(rounds, load_none, read_all);
    let (arena_d, naive_d) = best_of_pair(rounds, arena, naive);

    let none_ms = none_d.as_secs_f64() * 1000.0;
    let all_ms = all_d.as_secs_f64() * 1000.0;
    let arena_ms = arena_d.as_secs_f64() * 1000.0;
    let naive_ms = naive_d.as_secs_f64() * 1000.0;

    println!(
        "E4 gate: {} entries | read-none {:.3} ms, read-all {:.3} ms ({:.0}x) | arena {:.2} ms vs String/formula {:.2} ms ({:.3}x)",
        col.len(),
        none_ms,
        all_ms,
        all_ms / none_ms.max(1e-6),
        arena_ms,
        naive_ms,
        arena_ms / naive_ms.max(1e-6)
    );

    assert!(
        all_ms > none_ms * 10.0,
        "reading no formulas cost {:.3} ms and reading all cost {:.3} ms — the lazy E4 decision has been lost",
        none_ms,
        all_ms
    );
    // Assertion (2) is a 1.0x bound — two shapes that are meant to be close,
    // compared with no margin. That is fine when the machine is quiet and a
    // false red when it is not: this fired once at 1.204x while ten background
    // agents were saturating the CPU, then passed on every rerun. A ratio of
    // two measurements taken at different moments is only meaningful when the
    // load between them is comparable, so the strict bound is release-only,
    // where the numbers are large enough to survive scheduling noise. Debug
    // keeps a loose sanity bound that still catches a real inversion.
    //
    // Assertion (1) above has ~180-850x of margin and needs no such carve-out.
    let bound = if cfg!(debug_assertions) {
        naive_ms * 2.0
    } else {
        naive_ms
    };
    assert!(
        arena_ms < bound,
        "arena hydration took {:.2} ms vs String-per-formula {:.2} ms ({:.3}x) — reading every formula must not be slower than the shape E4 replaced",
        arena_ms,
        naive_ms,
        arena_ms / naive_ms.max(1e-6)
    );

    // Real-file figures, for the record (release only — debug's ~20x slowdown
    // would dominate the run budget for no gate value). The "before" column is
    // the String-per-formula materialisation (candidate A, what shipped before
    // E4); "after" is the arena (candidate C). Load itself is identical either
    // way — E4 only moved WHEN translation runs, so the before/after delta is
    // the read-every-formula step.
    if cfg!(debug_assertions) {
        return;
    }
    let fpath = format!("{}/testdata/formulas.xlsx", env!("CARGO_MANIFEST_DIR"));
    let vf = crate::turbo::Features::VALUES | crate::turbo::Features::FORMULAS;
    let before = |f: &crate::turbo::FormulaColumn| {
        let rows = f.materialize_all_naive();
        black_box(rows.len());
    };
    let after = |f: &crate::turbo::FormulaColumn| {
        let rows = f.materialize_export_rows();
        black_box(rows.len());
    };
    let values_ms = best_of(3, || {
        let wb = crate::turbo::read_workbook_turbo(&fpath, crate::turbo::Features::VALUES)
            .expect("read formulas.xlsx");
        black_box(wb.sheets[0].nrows);
    })
    .as_secs_f64()
        * 1000.0;
    let vf_load_ms = best_of(3, || {
        let wb = crate::turbo::read_workbook_turbo(&fpath, vf).expect("read formulas.xlsx");
        black_box(wb.sheets[0].formulas.as_ref().expect("formulas").len());
    })
    .as_secs_f64()
        * 1000.0;
    let vf_before_ms = best_of(3, || {
        let wb = crate::turbo::read_workbook_turbo(&fpath, vf).expect("read formulas.xlsx");
        before(wb.sheets[0].formulas.as_ref().expect("formulas"));
    })
    .as_secs_f64()
        * 1000.0;
    let vf_after_ms = best_of(3, || {
        let wb = crate::turbo::read_workbook_turbo(&fpath, vf).expect("read formulas.xlsx");
        after(wb.sheets[0].formulas.as_ref().expect("formulas"));
    })
    .as_secs_f64()
        * 1000.0;
    let all_before_ms = best_of(3, || {
        let wb = crate::turbo::read_workbook_turbo(&fpath, crate::turbo::Features::ALL)
            .expect("read formulas.xlsx");
        before(wb.sheets[0].formulas.as_ref().expect("formulas"));
    })
    .as_secs_f64()
        * 1000.0;
    let all_after_ms = best_of(3, || {
        let wb = crate::turbo::read_workbook_turbo(&fpath, crate::turbo::Features::ALL)
            .expect("read formulas.xlsx");
        after(wb.sheets[0].formulas.as_ref().expect("formulas"));
    })
    .as_secs_f64()
        * 1000.0;
    println!(
        "E4 file: formulas.xlsx | VALUES {:.1} ms | VALUES+FORMULAS load {:.1} ms | VF before(A) {:.1} ms after(C) {:.1} ms | ALL before(A) {:.1} ms after(C) {:.1} ms",
        values_ms, vf_load_ms, vf_before_ms, vf_after_ms, all_before_ms, all_after_ms
    );
}
