//! S2 — type-promotion and mixed-column fuzz harness.
//!
//! P0 in PERF_EXPERIMENTS_PHASE2.md was a quadratic in the mixed-type column
//! path: `Dict::strings()` rebuilt the whole intern pool once per string cell
//! inside the `Column::Mixed` finalizer. A 50k-row sheet with numbers and
//! strings interleaved in one column took 118 s; an equivalent homogeneous
//! sheet took 0.065 s. 694 tests missed it because every fixture was
//! homogeneous — a column of numbers or a column of strings, never both.
//!
//! This harness makes that entire class of bug — the wrong-answer half AND the
//! pathological-performance half — impossible to reintroduce:
//!
//! D1  A generator producing sheets with adversarial type distributions:
//!     every pairing of numeric / inline string / shared string / boolean /
//!     error / date / empty / formula-with-cached-value interleaved within a
//!     single column; deliberate promotion ORDER (numeric-then-string,
//!     string-then-numeric, alternating); a type appearing once in 10,000 rows
//!     at the start / middle / end; entirely-null columns and only-the-final-
//!     row-has-a-value columns; very wide and very tall shapes; duplicate vs
//!     entirely-distinct strings (the bug scaled with DISTINCT count).
//!
//! D2  Correctness invariants over every generated sheet:
//!      * every value written is read back with its correct type and value —
//!        no cell silently becomes null;
//!      * COLUMN LENGTH always equals the row count (the bug that was fixed
//!        alongside the quadratic appended NEITHER a value NOR a null on an
//!        out-of-range index, silently shortening a column and misaligning the
//!        batch);
//!      * mixed columns stringify numbers exactly like the numeric path
//!        formats them (ryu round-trip);
//!      * reading with different `Features` flags never changes a cell's value.
//!
//! D3  A performance invariant, asserting SCALING not absolute time: the same
//!     mixed shape at N and 2N rows must keep the time ratio under 3.0 (a
//!     quadratic read measures ~4.0x per doubling), and mixed must stay within
//!     a small multiple of homogeneous at the same size (before the fix that
//!     multiple was 1,820x). Measured figures are printed so a failure is
//!     diagnosable.
//!
//! The default run stays under ~30 s. The exhaustive sweep is behind `#[ignore]`:
//!
//!   cargo test --release --features __arrow --test type_fuzz type_fuzz_exhaustive -- --ignored
//!
//! The reader (`nextexcel/src/turbo/scan.rs`) is intentionally OUT OF BOUNDS:
//! this file only writes xlsx via the turbo WRITE path and reads them back via
//! the turbo READ path. A real defect must be reported, not patched here.

#![cfg(feature = "__arrow")]

use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use arrow_array::types::Int32Type;
use arrow_array::{Array as _, ArrayRef, DictionaryArray, Float64Array, StringArray};
use kyrax::turbo::write::{
    CachedValue, Cell, CellValue, FormulaKind, Row, StringMode, Workbook, date_to_serial,
    save_workbook,
};
use kyrax::turbo::{Features, read_workbook_turbo};
use pretty_assertions::assert_eq;

// ---------------------------------------------------------------------------
// Fixed-seed PRNG (xorshift64, deterministic — a failure reproduces).
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: u64) -> u32 {
        (self.next() % n) as u32
    }
    fn chance(&mut self, pct: u32) -> bool {
        self.below(100) < pct
    }
}

// ---------------------------------------------------------------------------
// Value generation
// ---------------------------------------------------------------------------

/// Strings chosen to stress the reader: XML specials, leading/trailing spaces
/// (xml:space), non-ASCII, ref-like text, empty.
const SPECIAL_STRINGS: &[&str] = &[
    "",
    "a",
    "A1",
    "XFD1048576",
    "=SUM(A1:A2)",
    "&<>\"'",
    "café 東京",
    "  leading-space",
    "trailing-space  ",
    "N/A",
    "plain text",
    "héllo wörld",
    "with\t tab",
    "quoted \"inner\" text",
    "0.5 looks numeric but is text",
];

const ERR_CODES: &[&str] = &["#DIV/0!", "#VALUE!", "#REF!", "#NAME?", "#N/A", "#NULL!"];

const EDGE_NUMS: &[f64] = &[
    0.0,
    -0.0,
    1.0,
    -1.0,
    0.1,
    0.1 + 0.2,
    1e-9,
    1e20,
    123_456.789,
    std::f64::consts::PI,
    2.5e-7,
    9_999_999_999.0,
];

fn gen_num(rng: &mut Rng) -> f64 {
    if rng.chance(30) {
        return EDGE_NUMS[rng.below(EDGE_NUMS.len() as u64) as usize];
    }
    match rng.below(6) {
        0 => rng.below(1_000_000) as f64,
        1 => (rng.below(1_000_000) as f64) / 100.0,
        2 => -((rng.below(1_000_000) as f64) + 0.5),
        3 => (rng.below(10_000_000) as f64) * 1e4,
        4 => (rng.below(1_000_000_000) as f64) * 1e-4,
        _ => rng.below(2_000_000_000) as f64,
    }
}

fn gen_str(rng: &mut Rng) -> String {
    SPECIAL_STRINGS[rng.below(SPECIAL_STRINGS.len() as u64) as usize].to_string()
}

fn gen_err(rng: &mut Rng) -> String {
    ERR_CODES[rng.below(ERR_CODES.len() as u64) as usize].to_string()
}

fn gen_date(rng: &mut Rng) -> f64 {
    date_to_serial(
        2020 + (rng.below(10) as i32),
        1 + rng.below(12),
        1 + rng.below(28),
    )
}

// ---------------------------------------------------------------------------
// The model: write-side plan + expected read-back.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Plan {
    Null,
    Num(f64),
    Str(String),
    Bool(bool),
    Err(String),
    Date(f64),
    FormulaNum(f64),
    FormulaStr(String),
    FormulaBool(bool),
    FormulaErr(String),
    FormulaNone,
}

impl Plan {
    fn to_cellvalue(&self) -> CellValue {
        match self {
            Plan::Null => CellValue::Empty,
            Plan::Num(x) => CellValue::Number(*x),
            Plan::Str(s) => CellValue::Str(s.clone()),
            Plan::Bool(b) => CellValue::Bool(*b),
            Plan::Err(e) => CellValue::Error(e.clone()),
            Plan::Date(x) => CellValue::DateSerial(*x),
            Plan::FormulaNum(x) => CellValue::Formula {
                text: "=1+1".into(),
                kind: FormulaKind::Normal,
                cached: Some(CachedValue::Number(*x)),
            },
            Plan::FormulaStr(s) => CellValue::Formula {
                text: "=\"x\"".into(),
                kind: FormulaKind::Normal,
                cached: Some(CachedValue::Str(s.clone())),
            },
            Plan::FormulaBool(b) => CellValue::Formula {
                text: "=1=1".into(),
                kind: FormulaKind::Normal,
                cached: Some(CachedValue::Bool(*b)),
            },
            Plan::FormulaErr(e) => CellValue::Formula {
                text: "=1/0".into(),
                kind: FormulaKind::Normal,
                cached: Some(CachedValue::Error(e.clone())),
            },
            Plan::FormulaNone => CellValue::Formula {
                text: "=1+1".into(),
                kind: FormulaKind::Normal,
                cached: None,
            },
        }
    }

    fn error_code(&self) -> Option<&str> {
        match self {
            Plan::Err(e) | Plan::FormulaErr(e) => Some(e),
            _ => None,
        }
    }

    /// Does this cell make the reader push a string into its column?
    fn is_string_producing(&self) -> bool {
        matches!(
            self,
            Plan::Str(_) | Plan::Err(_) | Plan::FormulaStr(_) | Plan::FormulaErr(_)
        )
    }

    /// Does this cell make the reader push a number into its column?
    fn is_number_producing(&self) -> bool {
        matches!(
            self,
            Plan::Num(_)
                | Plan::Bool(_)
                | Plan::Date(_)
                | Plan::FormulaNum(_)
                | Plan::FormulaBool(_)
        )
    }

    fn string_value(&self) -> String {
        match self {
            Plan::Str(s) | Plan::FormulaStr(s) => s.clone(),
            Plan::Err(e) | Plan::FormulaErr(e) => e.clone(),
            _ => String::new(),
        }
    }

    fn number_value(&self) -> f64 {
        match self {
            Plan::Num(x) | Plan::Date(x) | Plan::FormulaNum(x) => *x,
            Plan::Bool(b) | Plan::FormulaBool(b) if *b => 1.0,
            _ => 0.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum ReadCell {
    Null,
    Num(f64),
    Str(String),
}

fn plan_for_kind(kind: &str, rng: &mut Rng) -> Plan {
    match kind {
        "num" => Plan::Num(gen_num(rng)),
        "str" => Plan::Str(gen_str(rng)),
        "bool" => Plan::Bool(rng.chance(50)),
        "err" => Plan::Err(gen_err(rng)),
        "date" => Plan::Date(gen_date(rng)),
        "empty" => Plan::Null,
        "formula" => Plan::FormulaNum(gen_num(rng)),
        _ => unreachable!("unknown kind {kind}"),
    }
}

// ---------------------------------------------------------------------------
// Sheet model → workbook → file → read back → verify.
// ---------------------------------------------------------------------------

struct GenSheet {
    name: String,
    mode: StringMode,
    ncols: usize,
    /// data[row][col]; every row has exactly `ncols` plans.
    data: Vec<Vec<Plan>>,
}

fn blank_sheet(name: &str, nrows: usize, ncols: usize, mode: StringMode) -> GenSheet {
    GenSheet {
        name: name.to_string(),
        mode,
        ncols,
        data: vec![vec![Plan::Null; ncols]; nrows],
    }
}

fn build_workbook(gs: &GenSheet) -> Workbook {
    let mut wb = Workbook::with_sheet(gs.name.clone());
    wb.options.string_mode = gs.mode;
    let mut header = Row::new(1);
    for c in 0..gs.ncols {
        header
            .cells
            .push(Cell::new((c + 1) as u32, CellValue::Str(format!("c{c}"))));
    }
    wb.sheets[0].rows.push(header);
    for (r, rowcells) in gs.data.iter().enumerate() {
        let mut row = Row::new((r + 2) as u32);
        for (c, p) in rowcells.iter().enumerate() {
            if matches!(p, Plan::Null) {
                continue;
            }
            row.cells.push(Cell::new((c + 1) as u32, p.to_cellvalue()));
        }
        wb.sheets[0].rows.push(row);
    }
    wb
}

static FILE_COUNTER: AtomicUsize = AtomicUsize::new(0);
static SHEETS_VERIFIED: AtomicUsize = AtomicUsize::new(0);

fn tmp_path(tag: &str) -> String {
    let n = FILE_COUNTER.fetch_add(1, Ordering::SeqCst);
    let mut p = std::env::temp_dir();
    let safe: String = tag
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    p.push(format!(
        "kyrax_type_fuzz_{safe}_{}_{n}.xlsx",
        std::process::id()
    ));
    p.to_string_lossy().into_owned()
}

/// Bounded violation sink so a systematic misalignment never builds a
/// multi-MB failure message.
struct Sink {
    items: Vec<String>,
    cap: usize,
}

impl Sink {
    fn new() -> Self {
        Sink {
            items: Vec::new(),
            cap: 200,
        }
    }
    fn push(&mut self, s: String) {
        if self.items.len() < self.cap {
            self.items.push(s);
        }
    }
}

fn read_cell(col: &ArrayRef, i: usize) -> ReadCell {
    if col.is_null(i) {
        return ReadCell::Null;
    }
    if let Some(f) = col.as_any().downcast_ref::<Float64Array>() {
        ReadCell::Num(f.value(i))
    } else if let Some(s) = col.as_any().downcast_ref::<StringArray>() {
        ReadCell::Str(s.value(i).to_string())
    } else if let Some(d) = col.as_any().downcast_ref::<DictionaryArray<Int32Type>>() {
        let keys = d.keys();
        if keys.is_null(i) {
            ReadCell::Null
        } else {
            let k = keys.value(i);
            let vals = d
                .values()
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("dict values are strings");
            ReadCell::Str(vals.value(k as usize).to_string())
        }
    } else {
        // Unknown array type: report as null; the expected-value comparison
        // below will flag it as a violation rather than passing silently.
        ReadCell::Null
    }
}

fn verify(gs: &GenSheet, path: &str, features: Features) -> Vec<String> {
    let mut v = Sink::new();
    let wb = match read_workbook_turbo(path, features) {
        Ok(wb) => wb,
        Err(e) => {
            v.push(format!("read failed: {e}"));
            return v.items;
        }
    };
    let s = &wb.sheets[0];
    let model_rows = gs.data.len();

    // Rows where EVERY cell is empty are omitted by the write path (an
    // all-blank row has no XML element), so the file only contains rows up to
    // the last row that has at least one non-empty cell.
    let expected_nrows = gs
        .data
        .iter()
        .rposition(|row| row.iter().any(|p| !matches!(p, Plan::Null)))
        .map(|i| i + 1)
        .unwrap_or(0);

    if s.ncols != gs.ncols {
        v.push(format!("ncols {} != expected {}", s.ncols, gs.ncols));
    }
    if s.nrows != expected_nrows {
        v.push(format!(
            "nrows {} != expected data rows {expected_nrows} (model rows {model_rows})",
            s.nrows
        ));
    }

    for c in 0..gs.ncols {
        let Some(col) = s.columns.get(c) else {
            v.push(format!("missing column {c}"));
            continue;
        };
        // D2 / the alignment invariant: a column that is shorter than nrows
        // would misalign the whole record batch.
        if col.len() != s.nrows {
            v.push(format!(
                "column {c} length {} != nrows {}",
                col.len(),
                s.nrows
            ));
        }
        // Never read past the array end (a shortened column is already flagged
        // above; comparing what IS present still gives useful context).
        let check_rows = col.len().min(s.nrows).min(expected_nrows);

        // Column-wide type inference, mirroring the reader's promotion rule: a
        // single string-producing cell anywhere in the column turns EVERY number
        // in the column into its stringified form (the column becomes Mixed).
        let any_str = (0..expected_nrows).any(|r| gs.data[r][c].is_string_producing());

        for r in 0..check_rows {
            let p = &gs.data[r][c];
            let got = read_cell(col, r);
            if p.is_string_producing() {
                let expected = p.string_value();
                match &got {
                    ReadCell::Str(g) => {
                        if g != &expected {
                            v.push(format!(
                                "col {c} row {r}: expected string '{expected}', got '{g}'"
                            ));
                        }
                    }
                    ReadCell::Null => v.push(format!(
                        "col {c} row {r}: expected string '{expected}', got null"
                    )),
                    ReadCell::Num(x) => v.push(format!(
                        "col {c} row {r}: expected string '{expected}', got num {x}"
                    )),
                }
            } else if p.is_number_producing() {
                let x = p.number_value();
                if any_str {
                    // Mixed column: numbers must stringify exactly like the
                    // numeric path formats them (ryu round-trip).
                    match &got {
                        ReadCell::Str(s) => {
                            let mut buf = ryu::Buffer::new();
                            let expected_str = buf.format(x);
                            if s != expected_str {
                                v.push(format!(
                                    "col {c} row {r}: mixed string '{s}' != numeric-path formatting '{expected_str}' for value {x}"
                                ));
                            }
                        }
                        ReadCell::Num(y) => v.push(format!(
                            "col {c} row {r}: expected stringified number {x}, got numeric {y} in a mixed column"
                        )),
                        ReadCell::Null => v.push(format!(
                            "col {c} row {r}: expected stringified number {x}, got null"
                        )),
                    }
                } else {
                    match &got {
                        ReadCell::Num(y) => {
                            if x.to_bits() != y.to_bits() {
                                v.push(format!(
                                    "col {c} row {r}: numeric value {x} read back as {y}"
                                ));
                            }
                        }
                        ReadCell::Str(s) => v.push(format!(
                            "col {c} row {r}: expected num {x}, got string '{s}'"
                        )),
                        ReadCell::Null => {
                            v.push(format!("col {c} row {r}: expected num {x}, got null"))
                        }
                    }
                }
            } else {
                match &got {
                    ReadCell::Null => {}
                    ReadCell::Num(x) => {
                        v.push(format!("col {c} row {r}: expected null, got num {x}"));
                    }
                    ReadCell::Str(s) => {
                        v.push(format!("col {c} row {r}: expected null, got string '{s}'"))
                    }
                }
            }
        }
    }

    // Error caches (t="e") must be recorded sparsely, exactly once per error cell.
    let mut expected_errs: Vec<(u32, u32, String)> = Vec::new();
    for (r, row) in gs.data.iter().enumerate() {
        for (c, p) in row.iter().enumerate() {
            if let Some(code) = p.error_code() {
                expected_errs.push((r as u32, c as u32, code.to_string()));
            }
        }
    }
    let mut got_errs: Vec<(u32, u32, String)> = s
        .cell_errors
        .iter()
        .map(|e| (e.row, e.col, e.code.clone()))
        .collect();
    expected_errs.sort();
    got_errs.sort();
    if expected_errs != got_errs {
        v.push(format!(
            "cell_errors mismatch: expected {} error(s), got {}",
            expected_errs.len(),
            got_errs.len()
        ));
        for (a, b) in expected_errs.iter().zip(got_errs.iter()) {
            if a != b {
                v.push(format!("  expected error {a:?}, got {b:?}"));
                break;
            }
        }
    }

    v.items
}

fn assert_clean(tag: &str, violations: Vec<String>) {
    if !violations.is_empty() {
        let mut msg = format!("TYPE FUZZ VIOLATION [{tag}]:\n");
        for v in violations.iter().take(12) {
            let _ = writeln!(msg, "  {v}");
        }
        if violations.len() > 12 {
            let _ = writeln!(msg, "  ... and {} more", violations.len() - 12);
        }
        panic!("{msg}");
    }
}

/// Build → write → read back with the given features → assert every invariant.
fn run_case(tag: &str, gs: &GenSheet) {
    let path = tmp_path(tag);
    let wb = build_workbook(gs);
    save_workbook(&wb, &path).unwrap_or_else(|e| panic!("{tag}: write failed: {e}"));
    let violations = verify(gs, &path, Features::VALUES);
    assert_clean(tag, violations);
    SHEETS_VERIFIED.fetch_add(1, Ordering::SeqCst);
    eprintln!("  ok  {tag} ({} rows x {} cols)", gs.data.len(), gs.ncols);
}

// ---------------------------------------------------------------------------
// Column builders for the adversarial distributions.
// ---------------------------------------------------------------------------

/// Promotion order within one column, as a function of the row index.
enum Order {
    NumThenStr,
    StrThenNum,
    Alternating,
}

impl Order {
    fn plan(&self, r: usize, rng: &mut Rng) -> Plan {
        match self {
            Order::NumThenStr => {
                if r < 100 {
                    Plan::Num(gen_num(rng))
                } else {
                    Plan::Str(format!("late_str_{r}"))
                }
            }
            Order::StrThenNum => {
                if r < 100 {
                    Plan::Str(format!("early_str_{r}"))
                } else {
                    Plan::Num(gen_num(rng))
                }
            }
            Order::Alternating => {
                if r % 2 == 0 {
                    Plan::Num(gen_num(rng))
                } else {
                    Plan::Str(format!("alt_str_{r}"))
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------------

const SEED_PAIR: u64 = 0x5EE0_0001;
const SEED_ORDER: u64 = 0x5EE0_0002;
const SEED_SPARSE: u64 = 0x5EE0_0003;
const SEED_EXHAUST: u64 = 0x5EE0_0004;

/// D1: every pairing of the write-side kinds, interleaved in one column.
#[test]
fn pairwise_type_promotions_roundtrip() {
    eprintln!("[type_fuzz] pairwise type promotions");
    let kinds = ["num", "str", "bool", "err", "date", "empty", "formula"];
    let mut rng = Rng::new(SEED_PAIR);
    let mut n = 0;
    for i in 0..kinds.len() {
        for j in (i + 1)..kinds.len() {
            let mode = if n % 2 == 0 {
                StringMode::InlineStr
            } else {
                StringMode::SharedStrings
            };
            let mut gs = blank_sheet(&format!("pair_{}_{}", kinds[i], kinds[j]), 6, 2, mode);
            for r in 0..6 {
                gs.data[r][0] = if r % 2 == 0 {
                    plan_for_kind(kinds[i], &mut rng)
                } else {
                    plan_for_kind(kinds[j], &mut rng)
                };
                gs.data[r][1] = Plan::Num(gen_num(&mut rng));
            }
            run_case(&format!("pair {}/{}", kinds[i], kinds[j]), &gs);
            n += 1;
        }
    }
}

/// D1: promotion is directional — numeric-then-string, string-then-numeric,
/// and alternating are separate branches in the reader.
#[test]
fn promotion_order_directions() {
    eprintln!("[type_fuzz] promotion order directions");
    let mut rng = Rng::new(SEED_ORDER);
    let orders = [
        ("num_then_str", Order::NumThenStr),
        ("str_then_num", Order::StrThenNum),
        ("alternating", Order::Alternating),
    ];
    for (name, order) in orders {
        for (mi, mode) in [StringMode::InlineStr, StringMode::SharedStrings]
            .iter()
            .enumerate()
        {
            let mut gs = blank_sheet(&format!("order_{name}_{mi}"), 200, 2, *mode);
            for r in 0..200 {
                gs.data[r][0] = order.plan(r, &mut rng);
                gs.data[r][1] = Plan::Num(r as f64);
            }
            run_case(&format!("order {name}"), &gs);
        }
    }
}

/// D1: a single promoting cell in a 10,000-row column, at the start, the
/// middle, and the end — and the mirror case (one number among strings).
#[test]
fn sparse_promoter_in_ten_thousand() {
    eprintln!("[type_fuzz] sparse promoter in 10,000 rows");
    let mut rng = Rng::new(SEED_SPARSE);
    let mut gs = blank_sheet("sparse_10k", 10_000, 6, StringMode::InlineStr);
    for r in 0..10_000 {
        let n = gen_num(&mut rng);
        gs.data[r][0] = Plan::Num(n);
        gs.data[r][1] = Plan::Num(n);
        gs.data[r][2] = Plan::Num(n);
        gs.data[r][3] = Plan::Str("common".into());
        gs.data[r][4] = Plan::Str("common".into());
        gs.data[r][5] = Plan::Str("common".into());
    }
    // One string among numbers: start / middle / end.
    gs.data[0][0] = Plan::Str("rare_start".into());
    gs.data[5_000][1] = Plan::Str("rare_mid".into());
    gs.data[9_999][2] = Plan::Str("rare_end".into());
    // One number among strings: start / middle / end.
    gs.data[0][3] = Plan::Num(1.5);
    gs.data[5_000][4] = Plan::Num(-42.25);
    #[allow(clippy::approx_constant)]
    let end_num = 3.14;
    gs.data[9_999][5] = Plan::Num(end_num);
    run_case("sparse 10k", &gs);
}

/// D1: entirely-null columns and columns where only the final (or first) row
/// has a value.
#[test]
fn null_and_single_value_columns() {
    eprintln!("[type_fuzz] null and single-value columns");
    let mut rng = Rng::new(0x5EE0_0005);
    let mut gs = blank_sheet("null_and_single", 200, 4, StringMode::InlineStr);
    for r in 0..200 {
        gs.data[r][0] = Plan::Num(gen_num(&mut rng)); // dense number column
        gs.data[r][1] = Plan::Null; // entirely null
        gs.data[r][2] = Plan::Null; // value only in the final row
        gs.data[r][3] = Plan::Null; // value only in the first row
    }
    gs.data[199][2] = Plan::Num(777.0);
    gs.data[0][3] = Plan::Str("only_first".into());
    run_case("null/single", &gs);
}

/// D1: very wide (many columns) and very tall (many rows) shapes. The tall
/// sheet also phases a column from numbers to strings halfway, exercising the
/// per-chunk column-type promotion path across row boundaries.
#[test]
fn wide_and_tall_shapes() {
    eprintln!("[type_fuzz] wide and tall shapes");
    let mut rng = Rng::new(0x5EE0_0006);

    // Wide: 300 columns x 30 rows, type cycling across columns.
    let wide_kinds = ["num", "str", "bool", "err", "date", "empty", "formula"];
    let mut wide = blank_sheet("wide_300", 30, 300, StringMode::InlineStr);
    for r in 0..30 {
        for c in 0..300 {
            wide.data[r][c] = plan_for_kind(wide_kinds[c % wide_kinds.len()], &mut rng);
        }
    }
    run_case("wide 300x30", &wide);

    // Tall: 100,000 rows x 3 cols. Col A phases number -> string at 50k,
    // col B is entirely-distinct inline strings, col C dense numbers.
    let mut tall = blank_sheet("tall_100k", 100_000, 3, StringMode::InlineStr);
    for r in 0..100_000 {
        tall.data[r][0] = if r < 50_000 {
            Plan::Num(r as f64)
        } else {
            Plan::Str(format!("phase_str_{r}"))
        };
        tall.data[r][1] = Plan::Str(format!("distinct_inline_{r}"));
        tall.data[r][2] = Plan::Num(gen_num(&mut rng));
    }
    run_case("tall 100kx3", &tall);
}

/// D1: the intern pool behaves differently for duplicate vs entirely distinct
/// strings, and the P0 quadratic scaled with DISTINCT count.
#[test]
fn duplicate_vs_distinct_strings() {
    eprintln!("[type_fuzz] duplicate vs distinct strings");
    // Duplicate-heavy, written through a real sharedStrings table (8 distinct).
    let mut dup = blank_sheet("dup_strings", 100_000, 3, StringMode::SharedStrings);
    for r in 0..100_000 {
        let s = SPECIAL_STRINGS[r % SPECIAL_STRINGS.len()].to_string();
        dup.data[r][0] = Plan::Str(s.clone());
        dup.data[r][1] = Plan::Str(s.clone());
        dup.data[r][2] = Plan::Str(format!("dup_pool_{}", r % 8));
    }
    run_case("dup strings 100k", &dup);

    // Entirely distinct, via sharedStrings (a 100k-entry SST).
    let mut distinct = blank_sheet("distinct_strings", 100_000, 3, StringMode::SharedStrings);
    for r in 0..100_000 {
        distinct.data[r][0] = Plan::Str(format!("distinct_a_{r}"));
        distinct.data[r][1] = Plan::Str(format!("distinct_b_{r}_&_<>{r}"));
        distinct.data[r][2] = Plan::Num(r as f64);
    }
    run_case("distinct strings 100k", &distinct);
}

/// Formula-with-cached-value: all five cache types, mixed into a numeric
/// column so a promoting formula cell forces the mixed path.
#[test]
fn formula_cached_variant_roundtrip() {
    eprintln!("[type_fuzz] formula cached variants");
    let mut rng = Rng::new(0x5EE0_0007);
    for (mi, mode) in [StringMode::InlineStr, StringMode::SharedStrings]
        .iter()
        .enumerate()
    {
        let mut gs = blank_sheet(&format!("formulas_{mi}"), 40, 3, *mode);
        for r in 0..40 {
            let f = match r % 5 {
                0 => Plan::FormulaNum(gen_num(&mut rng)),
                1 => Plan::FormulaStr(gen_str(&mut rng)),
                2 => Plan::FormulaBool(r % 2 == 0),
                3 => Plan::FormulaErr(gen_err(&mut rng)),
                _ => Plan::FormulaNone,
            };
            gs.data[r][0] = f;
            gs.data[r][1] = Plan::Num(gen_num(&mut rng));
            gs.data[r][2] = Plan::Str("adjacent".into());
        }
        run_case(&format!("formulas mode {mi}"), &gs);
    }
}

/// D2: reading with different Features flags must never change a cell's value.
#[test]
fn features_flags_do_not_change_values() {
    eprintln!("[type_fuzz] features-flag invariance");
    let mut rng = Rng::new(0x5EE0_0008);
    let flagsets = [
        Features::VALUES,
        Features::VALUES | Features::STYLES | Features::FORMULAS,
        Features::ALL,
    ];
    let mut n = 0;
    for t in 0..6 {
        let mode = if t % 2 == 0 {
            StringMode::InlineStr
        } else {
            StringMode::SharedStrings
        };
        let mut gs = blank_sheet(&format!("flags_{t}"), 200, 4, mode);
        for r in 0..200 {
            gs.data[r][0] = Plan::Num(gen_num(&mut rng));
            gs.data[r][1] = Plan::Str(gen_str(&mut rng));
            gs.data[r][2] = match r % 4 {
                0 => Plan::Bool(true),
                1 => Plan::Err(gen_err(&mut rng)),
                2 => Plan::FormulaNum(gen_num(&mut rng)),
                _ => Plan::Null,
            };
            gs.data[r][3] = if r == 150 {
                Plan::Str("sparse".into())
            } else {
                Plan::Num(gen_num(&mut rng))
            };
        }
        let path = tmp_path(&format!("flags_{t}"));
        let wb = build_workbook(&gs);
        save_workbook(&wb, &path).unwrap_or_else(|e| panic!("flags write failed: {e}"));
        for f in flagsets {
            let violations = verify(&gs, &path, f);
            assert_clean(&format!("flags t={t} features={f:?}"), violations);
        }
        n += 1;
    }
    eprintln!("  flags invariance over {n} sheets");
}

// ---------------------------------------------------------------------------
// D3 — performance invariants. These assert SCALING (a ratio), not absolute
// milliseconds, so they stay portable across machines.
// ---------------------------------------------------------------------------

/// Build a mixed sheet: 3 columns, col B interleaves numbers and DISTINCT
/// strings, col C is all distinct strings, col A all numbers.
fn perf_mixed_sheet(nrows: usize) -> Workbook {
    let mut wb = Workbook::with_sheet("perf_mixed");
    let mut header = Row::new(1);
    for c in 1..=3u32 {
        header
            .cells
            .push(Cell::new(c, CellValue::Str(format!("h{c}"))));
    }
    wb.sheets[0].rows.push(header);
    for r in 0..nrows {
        let mut row = Row::new((r + 2) as u32);
        row.cells.push(Cell::new(1, CellValue::Number(r as f64)));
        row.cells.push(Cell::new(
            2,
            if r % 2 == 0 {
                CellValue::Number(r as f64)
            } else {
                CellValue::Str(format!("distinct_row_{r}_mid"))
            },
        ));
        row.cells.push(Cell::new(
            3,
            CellValue::Str(format!("distinct_row_{r}_tail")),
        ));
        wb.sheets[0].rows.push(row);
    }
    wb
}

/// Homogeneous control: same shape, all numbers.
fn perf_homog_sheet(nrows: usize) -> Workbook {
    let mut wb = Workbook::with_sheet("perf_homog");
    let mut header = Row::new(1);
    for c in 1..=3u32 {
        header
            .cells
            .push(Cell::new(c, CellValue::Str(format!("h{c}"))));
    }
    wb.sheets[0].rows.push(header);
    for r in 0..nrows {
        let mut row = Row::new((r + 2) as u32);
        row.cells.push(Cell::new(1, CellValue::Number(r as f64)));
        row.cells.push(Cell::new(2, CellValue::Number(r as f64)));
        row.cells.push(Cell::new(3, CellValue::Number(r as f64)));
        wb.sheets[0].rows.push(row);
    }
    wb
}

/// One timed read of `path`; the result is kept alive past the timer so the
/// allocation work is included and the read cannot be elided.
fn time_one(path: &str) -> Duration {
    let t = Instant::now();
    let wb = read_workbook_turbo(path, Features::VALUES).expect("timed read");
    let _sum: usize = wb
        .sheets
        .iter()
        .flat_map(|s| s.columns.iter())
        .map(|c| c.len())
        .sum();
    t.elapsed()
}

/// D3: the mixed-type read must scale linearly, and mixed must stay within a
/// small multiple of homogeneous at the same size.
///
/// The other tests in this binary write 100k-row sheets concurrently, so a
/// single file's wall time is burstily inflated. The three files are therefore
/// read back-to-back in each round, and we keep the MINIMUM RATIO across
/// rounds: any contention burst hits the files of one round together, and the
/// cleanest round sets the reported ratio. This asserts scaling (a ratio),
/// never an absolute millisecond count, so it stays portable.
#[test]
fn mixed_column_scaling_is_linear() {
    let n = 100_000usize;
    const ROUNDS: usize = 8;
    const MAX_SCALING_RATIO: f64 = 3.0; // linear is ~2.0; quadratic measured ~4.0
    const MAX_MIXED_OVER_HOMOG: f64 = 25.0; // pre-fix was 1,820x; post-fix ~2.2x

    eprintln!("[type_fuzz perf] building sheets (N={n}, 2N={})", n * 2);
    let mixed_n = tmp_path("perf_mixed_n");
    let mixed_2n = tmp_path("perf_mixed_2n");
    let homog_2n = tmp_path("perf_homog_2n");
    save_workbook(&perf_mixed_sheet(n), &mixed_n).unwrap();
    save_workbook(&perf_mixed_sheet(n * 2), &mixed_2n).unwrap();
    save_workbook(&perf_homog_sheet(n * 2), &homog_2n).unwrap();

    // Length sanity on the largest sheets (the alignment invariant).
    for (path, rows) in [(&mixed_2n, n * 2), (&homog_2n, n * 2)] {
        let wb = read_workbook_turbo(path, Features::VALUES).unwrap();
        let s = &wb.sheets[0];
        assert_eq!(s.nrows, rows, "nrows for {}", path);
        for (ci, col) in s.columns.iter().enumerate() {
            assert_eq!(col.len(), s.nrows, "column {ci} length for {}", path);
        }
    }

    // Warm the allocator / rayon pool / OS file cache for all three files.
    for p in [&mixed_n, &mixed_2n, &homog_2n] {
        let _ = read_workbook_turbo(p, Features::VALUES).expect("warmup read");
    }

    let mut best_ratio = f64::MAX;
    let mut best_multiple = f64::MAX;
    let mut best_t1 = Duration::MAX;
    let mut best_t2 = Duration::MAX;
    let mut best_t3 = Duration::MAX;
    for _ in 0..ROUNDS {
        let a = time_one(&mixed_n);
        let b = time_one(&mixed_2n);
        let c = time_one(&homog_2n);
        best_t1 = best_t1.min(a);
        best_t2 = best_t2.min(b);
        best_t3 = best_t3.min(c);
        let ratio = b.as_secs_f64() / a.as_secs_f64().max(1e-9);
        let multiple = b.as_secs_f64() / c.as_secs_f64().max(1e-9);
        if ratio < best_ratio {
            best_ratio = ratio;
        }
        if multiple < best_multiple {
            best_multiple = multiple;
        }
    }

    eprintln!(
        "\n[type_fuzz perf] mixed   {n} rows  : {:8.2} ms",
        best_t1.as_secs_f64() * 1e3
    );
    eprintln!(
        "[type_fuzz perf] mixed   {} rows: {:8.2} ms",
        n * 2,
        best_t2.as_secs_f64() * 1e3
    );
    eprintln!(
        "[type_fuzz perf] homog   {} rows: {:8.2} ms",
        n * 2,
        best_t3.as_secs_f64() * 1e3
    );
    eprintln!(
        "[type_fuzz perf] scaling ratio       : {:.2}  (assert < {MAX_SCALING_RATIO})",
        best_ratio
    );
    eprintln!(
        "[type_fuzz perf] mixed/homogeneous   : {:.2}x (assert < {MAX_MIXED_OVER_HOMOG})",
        best_multiple
    );

    assert!(
        best_ratio < MAX_SCALING_RATIO,
        "mixed read scaling is super-linear: {n} rows took {:.1} ms but {} rows took {:.1} ms (min ratio {:.2}, threshold {MAX_SCALING_RATIO}); doubling rows must not cost ~3x time",
        best_t1.as_secs_f64() * 1e3,
        n * 2,
        best_t2.as_secs_f64() * 1e3,
        best_ratio
    );
    assert!(
        best_multiple < MAX_MIXED_OVER_HOMOG,
        "mixed columns cost {:.2}x a homogeneous column at {} rows (threshold {MAX_MIXED_OVER_HOMOG}); before the P0 fix this was 1,820x",
        best_multiple,
        n * 2
    );
}

// ---------------------------------------------------------------------------
// Exhaustive sweep (ignored by default).
// ---------------------------------------------------------------------------

/// Randomized sweep over many seeds, sizes, shapes, and both string modes,
/// plus sparse-promotion hunts at 100k rows. Slower than the default run.
///
/// Run with:
///   cargo test --release --features __arrow --test type_fuzz type_fuzz_exhaustive -- --ignored
#[test]
#[ignore = "exhaustive type-fuzz sweep; run explicitly (command in the doc comment)"]
fn type_fuzz_exhaustive() {
    eprintln!("[type_fuzz] exhaustive sweep (ignored test)");
    let modes = [StringMode::InlineStr, StringMode::SharedStrings];
    let sizes = [16usize, 300, 3_000];
    let widths = [1usize, 3, 8];
    let styles = [
        "all_num",
        "all_str",
        "alt_num_str",
        "num_heavy_str_rare",
        "str_heavy_num_rare",
        "all_types",
        "mostly_empty",
        "phased",
    ];

    let mut sheets = 0usize;
    for seed in 0..120 {
        let mut s = Rng::new(SEED_EXHAUST.wrapping_add(seed as u64 * 0x9E37_79B9));
        let nrows = sizes[s.below(sizes.len() as u64) as usize];
        let ncols = widths[s.below(widths.len() as u64) as usize];
        let mode = modes[s.below(2) as u64 as usize];
        let style = styles[s.below(styles.len() as u64) as usize];
        let mut gs = blank_sheet(&format!("rand_{seed}"), nrows, ncols, mode);
        for r in 0..nrows {
            for c in 0..ncols {
                gs.data[r][c] = gen_for_style(&mut s, style, r, nrows);
            }
        }
        run_case(&format!("rand seed {seed} style {style}"), &gs);
        sheets += 1;
    }

    // Sparse hunts at 100k rows: one promoting cell at a random position.
    for seed in 0..6 {
        let mut s = Rng::new(0x5EE0_0A00 + seed);
        let mode = modes[(seed % 2) as usize];
        let mut gs = blank_sheet(&format!("hunt_{seed}"), 100_000, 2, mode);
        for r in 0..100_000 {
            gs.data[r][0] = if s.chance(80) {
                Plan::Num(gen_num(&mut s))
            } else {
                Plan::Str(format!("body_str_{}", r % 50))
            };
            gs.data[r][1] = Plan::Str("fixed".into());
        }
        let pos = [0usize, 50_000, 99_999][(seed % 3) as usize];
        gs.data[pos][1] = Plan::Num(gen_num(&mut s));
        run_case(&format!("sparse hunt {seed} at {pos}"), &gs);
        sheets += 1;
    }

    // Phased columns at scale, both directions, both modes.
    for dir in ["num_then_str", "str_then_num"] {
        for (mi, mode) in modes.iter().enumerate() {
            let mut gs = blank_sheet(&format!("phase_{dir}_{mi}"), 120_000, 3, *mode);
            for r in 0..120_000 {
                let switch = 60_000;
                let (a, b) = if dir == "num_then_str" {
                    (Plan::Num(r as f64), Plan::Str(format!("p_{r}")))
                } else {
                    (Plan::Str(format!("p_{r}")), Plan::Num(r as f64))
                };
                gs.data[r][0] = if r < switch { a } else { b };
                gs.data[r][1] = Plan::Num(r as f64);
                gs.data[r][2] = Plan::Str(format!("q_{r}"));
            }
            run_case(&format!("phase {dir} mode {mi}"), &gs);
            sheets += 1;
        }
    }

    let total = SHEETS_VERIFIED.load(Ordering::SeqCst);
    eprintln!(
        "[type_fuzz] exhaustive sweep verified {sheets} additional sheets; cumulative total {total}"
    );
}

/// Cell plan for one (row, col) under a named distribution style.
fn gen_for_style(rng: &mut Rng, style: &str, r: usize, nrows: usize) -> Plan {
    match style {
        "all_num" => Plan::Num(gen_num(rng)),
        "all_str" => Plan::Str(gen_str(rng)),
        "alt_num_str" => {
            if r % 2 == 0 {
                Plan::Num(gen_num(rng))
            } else {
                Plan::Str(gen_str(rng))
            }
        }
        "num_heavy_str_rare" => {
            if rng.chance(90) {
                Plan::Num(gen_num(rng))
            } else {
                Plan::Str(gen_str(rng))
            }
        }
        "str_heavy_num_rare" => {
            if rng.chance(90) {
                Plan::Str(gen_str(rng))
            } else {
                Plan::Num(gen_num(rng))
            }
        }
        "all_types" => match rng.below(8) {
            0 => Plan::Num(gen_num(rng)),
            1 => Plan::Str(gen_str(rng)),
            2 => Plan::Bool(rng.chance(50)),
            3 => Plan::Err(gen_err(rng)),
            4 => Plan::Date(gen_date(rng)),
            5 => Plan::Null,
            6 => Plan::FormulaNum(gen_num(rng)),
            _ => Plan::FormulaErr(gen_err(rng)),
        },
        "mostly_empty" => {
            if rng.chance(85) {
                Plan::Null
            } else if rng.chance(50) {
                Plan::Num(gen_num(rng))
            } else {
                Plan::Str(gen_str(rng))
            }
        }
        "phased" => {
            if r < nrows / 2 {
                Plan::Num(gen_num(rng))
            } else {
                Plan::Str(format!("phase_{r}"))
            }
        }
        _ => unreachable!("unknown style {style}"),
    }
}
