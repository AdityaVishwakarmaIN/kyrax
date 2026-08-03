// calc/testkit.rs — shared harness for the per-class formula test matrices.
//
// Test-only. Its whole point is that a class test says what Excel does and
// nothing else: no resolver boilerplate, no FuncCtx construction, no manual
// `FuncArg` building. Every helper here drives the **real** path —
// `parse_formula` then `eval` — so a test exercises the lexer, parser,
// reference resolution, coercion, the registry and the function body together.
// Calling a function pointer directly would prove far less.
//
// Error reporting is deliberately lossless: a formula that fails to parse is
// distinguishable from one that evaluates to an Excel error, because "kyrax
// could not read this" and "Excel says #VALUE!" are different outcomes and a
// test must be able to tell them apart.

#![cfg(test)]

use std::collections::HashMap;

use crate::turbo::calc::ast::RefCore;
use crate::turbo::calc::eval::eval;
use crate::turbo::calc::functions::{CellResolver, FuncCtx};
use crate::turbo::calc::parser::parse_formula;
use crate::turbo::calc::refs::{RefParse, parse_a1};
use crate::turbo::calc::value::{ArrayValue, CalcError, CalcValue};

/// What a formula produced.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Evaluated to a value (which may itself be an Excel error value).
    Value(CalcValue),
    /// Evaluation returned an Excel error.
    Err(CalcError),
    /// The text could not be parsed at all — the cell would take the
    /// hydration fallback path, not receive an error value.
    ParseError,
}

/// A single-sheet grid, addressed in A1 notation.
pub struct Grid {
    cells: HashMap<(u32, u32), CalcValue>,
    date1904: bool,
}

impl CellResolver for Grid {
    fn cell(&self, _sheet: u32, row: u32, col: u32) -> Option<CalcValue> {
        self.cells.get(&(row, col)).cloned()
    }

    fn sheet_index(&self, name: &str) -> Option<u32> {
        // One sheet, so `Sheet1!A1` resolves and anything else is #REF! —
        // which is what a test asserting on a bad sheet name wants.
        if name.eq_ignore_ascii_case("Sheet1") {
            Some(0)
        } else {
            None
        }
    }
}

impl Grid {
    pub fn empty() -> Self {
        Grid {
            cells: HashMap::new(),
            date1904: false,
        }
    }

    /// 1904 date system, for the date-class tests.
    pub fn with_1904(mut self) -> Self {
        self.date1904 = true;
        self
    }

    /// Place one value at an A1 address. Panics on a bad address — that is a
    /// bug in the test, not a condition under test.
    pub fn set(mut self, a1: &str, v: CalcValue) -> Self {
        let (row, col) = a1_to_rc(a1);
        self.cells.insert((row, col), v);
        self
    }

    pub fn set_num(self, a1: &str, n: f64) -> Self {
        self.set(a1, CalcValue::Number(n))
    }

    pub fn set_text(self, a1: &str, s: &str) -> Self {
        self.set(a1, CalcValue::text(s))
    }

    pub fn set_bool(self, a1: &str, b: bool) -> Self {
        self.set(a1, CalcValue::Bool(b))
    }

    /// Fill a column downward from `a1`, e.g. `col("A1", &[1.0, 2.0, 3.0])`.
    pub fn col(mut self, a1: &str, values: &[f64]) -> Self {
        let (row, col) = a1_to_rc(a1);
        for (i, v) in values.iter().enumerate() {
            self.cells
                .insert((row + i as u32, col), CalcValue::Number(*v));
        }
        self
    }

    /// Fill a row rightward from `a1`.
    pub fn row(mut self, a1: &str, values: &[f64]) -> Self {
        let (row, col) = a1_to_rc(a1);
        for (i, v) in values.iter().enumerate() {
            self.cells
                .insert((row, col + i as u32), CalcValue::Number(*v));
        }
        self
    }

    /// Evaluate `formula` as if it lived in cell `at`.
    pub fn at(&self, at: &str, formula: &str) -> Outcome {
        let (row, col) = a1_to_rc(at);
        let expr = match parse_formula(formula) {
            Ok(e) => e,
            Err(_) => return Outcome::ParseError,
        };
        let ctx = FuncCtx {
            date1904: self.date1904,
            sheet: 0,
            row,
            col,
            resolver: self,
        };
        match eval(&expr, &ctx) {
            Ok(v) => Outcome::Value(v),
            Err(e) => Outcome::Err(e),
        }
    }

    /// Evaluate `formula` in a far-away cell (Z100), so it never collides with
    /// the data the test placed.
    pub fn calc(&self, formula: &str) -> Outcome {
        self.at("Z100", formula)
    }

    /// The numeric result, or a panic naming the formula and what it actually
    /// produced — a failing assertion should say what happened, not just
    /// "None".
    pub fn num(&self, formula: &str) -> f64 {
        match self.calc(formula) {
            Outcome::Value(CalcValue::Number(n)) => n,
            other => panic!("{formula} -> {other:?}, expected a number"),
        }
    }

    pub fn text(&self, formula: &str) -> String {
        match self.calc(formula) {
            Outcome::Value(CalcValue::Text(t)) => t.to_string(),
            other => panic!("{formula} -> {other:?}, expected text"),
        }
    }

    pub fn boolean(&self, formula: &str) -> bool {
        match self.calc(formula) {
            Outcome::Value(CalcValue::Bool(b)) => b,
            other => panic!("{formula} -> {other:?}, expected a boolean"),
        }
    }

    /// The Excel error a formula produces, whether it came back as an error
    /// value or as an evaluation error — the two are equivalent to a user.
    pub fn error(&self, formula: &str) -> CalcError {
        match self.calc(formula) {
            Outcome::Err(e) => e,
            Outcome::Value(CalcValue::Error(e)) => e,
            other => panic!("{formula} -> {other:?}, expected an error"),
        }
    }

    /// The result as an array, for spill/array-returning functions.
    pub fn array(&self, formula: &str) -> ArrayValue {
        match self.calc(formula) {
            Outcome::Value(CalcValue::Array(a)) => (*a).clone(),
            other => panic!("{formula} -> {other:?}, expected an array"),
        }
    }
}

// -- free helpers for the common "no data needed" case -----------------------

/// Evaluate against an empty grid.
pub fn calc(formula: &str) -> Outcome {
    Grid::empty().calc(formula)
}

pub fn num(formula: &str) -> f64 {
    Grid::empty().num(formula)
}

pub fn text(formula: &str) -> String {
    Grid::empty().text(formula)
}

pub fn boolean(formula: &str) -> bool {
    Grid::empty().boolean(formula)
}

pub fn error(formula: &str) -> CalcError {
    Grid::empty().error(formula)
}

/// Assert a numeric result within a relative tolerance — for the functions
/// where exact binary equality is not the right question.
pub fn approx(formula: &str, expected: f64, rel: f64) {
    let got = num(formula);
    let scale = expected.abs().max(1.0);
    assert!(
        (got - expected).abs() <= rel * scale,
        "{formula} -> {got}, expected {expected} (rel tol {rel})"
    );
}

/// `A1` / `$B$7` -> 0-based `(row, col)`.
fn a1_to_rc(a1: &str) -> (u32, u32) {
    match parse_a1(a1) {
        RefParse::Ref(RefCore::Cell(c)) => (c.row, u32::from(c.col)),
        other => panic!("{a1:?} is not a cell address ({other:?})"),
    }
}

#[cfg(test)]
mod selftest {
    use super::*;

    #[test]
    fn the_harness_drives_the_real_pipeline() {
        // arithmetic through lexer -> parser -> eval
        assert_eq!(num("=1+2*3"), 7.0);
        // registry dispatch
        assert_eq!(num("=SUM(1,2,3)"), 6.0);
        // reference resolution against the grid
        let g = Grid::empty().col("A1", &[1.0, 2.0, 3.0]);
        assert_eq!(g.num("=SUM(A1:A3)"), 6.0);
        assert_eq!(g.num("=A2"), 2.0);
        // a missing cell is blank, not zero-by-accident
        assert_eq!(
            g.calc("=ISBLANK(B1)"),
            Outcome::Value(CalcValue::Bool(true))
        );
    }

    #[test]
    fn the_harness_distinguishes_the_three_outcomes() {
        // an Excel error value
        assert_eq!(error("=1/0"), CalcError::Div0);
        // an unknown name is an error, not a parse failure
        assert_eq!(error("=NOSUCHFUNC(1)"), CalcError::Name);
        // ...and unreadable text is a parse failure, not an error value
        assert_eq!(calc("=1+"), Outcome::ParseError);
        assert_eq!(calc("=SUM(A1"), Outcome::ParseError);
    }

    #[test]
    fn the_formula_cell_position_is_honoured() {
        let g = Grid::empty();
        assert_eq!(g.at("C5", "=ROW()"), Outcome::Value(CalcValue::Number(5.0)));
        assert_eq!(
            g.at("C5", "=COLUMN()"),
            Outcome::Value(CalcValue::Number(3.0))
        );
    }

    #[test]
    fn helpers_panic_loudly_rather_than_silently_passing() {
        let r = std::panic::catch_unwind(|| num("=\"abc\""));
        assert!(r.is_err(), "num() must not accept a text result");
    }
}
