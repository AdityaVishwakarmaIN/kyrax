// functions/database.rs — the database function family (DSUM, DAVERAGE, ...).
// Owned exclusively by the database family agent; no other agent edits this file.
//
// Registry contract: implement `register` below and keep this exact signature.
// Do NOT edit functions/mod.rs — the `mod database;` declaration and the
// `database::register(&mut r)` call site in `build()` are already final.
// See functions/mod.rs for the worked ABS template.
//
// These twelve functions share one shape: D<AGG>(database, field, criteria).
// `database` is a range whose FIRST ROW is column headers; `criteria` is a
// separate range, also with a header row, where each row is a set of AND
// conditions and multiple rows are OR-ed together. `field` selects the column
// to aggregate, either by header name (case-insensitive) or by 1-based index.
//
// Criteria semantics, matching Excel exactly:
//   * A bare text criterion is a PREFIX match ("Ap" matches "Apple"); the exact
//     form `="Apple"` matches only that text.
//   * `*` / `?` are wildcards in text criteria, `~` escapes a literal one.
//   * Comparison operators `>`, `<`, `>=`, `<=`, `<>`, `=` before a number make
//     a numeric comparison; a numeric-looking STRING is never coerced to a
//     number (numeric criteria only meet Number cells, text criteria only Text).
//   * An EMPTY criteria cell means "no condition on this column"; a criteria
//     header naming a column the database does not have matches nothing.
//   * Text comparison is case-insensitive.
//
// All three arguments are RANGES, so these functions read `FuncArg::Reference`
// via `FuncCtx` exactly as COUNTIF/SUMIF in math.rs do; the criteria grid is
// compiled ONCE into a predicate before the record scan, never re-parsed per
// row.
use super::{FuncArg, FuncCtx, FuncSpec, Registry};
use crate::turbo::calc::coerce::{coerce_text, wildcard_match};
use crate::turbo::calc::value::{ArrayValue, CalcError, CalcValue};
use std::cmp::Ordering;

fn ok_num(n: f64) -> Result<CalcValue, CalcError> {
    if n.is_finite() {
        Ok(CalcValue::Number(n))
    } else {
        Err(CalcError::Num)
    }
}

/// One argument as a dense array; scalars become a 1x1 array so a single-cell
/// database/criteria still has a header row.
fn range_array(ctx: &FuncCtx, arg: &FuncArg) -> Result<ArrayValue, CalcError> {
    match arg.value(ctx)? {
        CalcValue::Array(a) => Ok((*a).clone()),
        v => Ok(ArrayValue::new(1, 1, vec![v])),
    }
}

// -- criteria parsing (once per call, then reused for every record) ----------

#[derive(Clone, Copy, Debug)]
enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// One compiled condition on one database column.
#[derive(Clone, Debug)]
enum Criterion {
    /// Numeric comparison; only matches Number cells.
    Num { op: Op, n: f64 },
    /// Text values starting with the pattern (`*` / `?` wildcards honoured).
    Prefix { pattern: String },
    /// Exact text match (case-insensitive), from `="..."`.
    Exact { text: String },
    /// `<>text` (unquoted): text values NOT starting with the pattern.
    NotPrefix { pattern: String },
    /// `<>"text"`: text values not exactly equal.
    NotExact { text: String },
    /// Ordering comparison against text (`>abc`, `<abc`, ...).
    TextCmp { op: Op, text: String },
    /// `""` — matches blank or empty text.
    IsEmpty,
    /// `<>` — matches anything non-blank, non-empty-text.
    NotEmpty,
    /// Equals a boolean.
    BoolVal(bool),
    /// Degenerate operand (`>=` with nothing after it): never matches.
    Never,
}

fn split_operator(s: &str) -> (Option<Op>, &str) {
    if let Some(r) = s.strip_prefix("<=") {
        (Some(Op::Le), r)
    } else if let Some(r) = s.strip_prefix(">=") {
        (Some(Op::Ge), r)
    } else if let Some(r) = s.strip_prefix("<>") {
        (Some(Op::Ne), r)
    } else if let Some(r) = s.strip_prefix('<') {
        (Some(Op::Lt), r)
    } else if let Some(r) = s.strip_prefix('>') {
        (Some(Op::Gt), r)
    } else if let Some(r) = s.strip_prefix('=') {
        (Some(Op::Eq), r)
    } else {
        (None, s)
    }
}

fn parse_criterion(v: &CalcValue) -> Result<Criterion, CalcError> {
    match v {
        CalcValue::Number(n) => Ok(Criterion::Num { op: Op::Eq, n: *n }),
        CalcValue::Bool(b) => Ok(Criterion::BoolVal(*b)),
        CalcValue::Error(e) => Err(*e),
        CalcValue::Blank => Ok(Criterion::Never),
        CalcValue::Text(t) => parse_text_criterion(t),
        CalcValue::Array(_) => Err(CalcError::Value),
    }
}

fn parse_text_criterion(s: &str) -> Result<Criterion, CalcError> {
    let s = s.trim();
    // `="text"` — the exact-match form (quotes make it literal, not a prefix).
    if let Some(inner) = s.strip_prefix("=\"") {
        if let Some(text) = inner.strip_suffix('"') {
            return Ok(Criterion::Exact {
                text: text.to_string(),
            });
        }
    }
    if let Some(inner) = s.strip_prefix("<>\"") {
        if let Some(text) = inner.strip_suffix('"') {
            return Ok(Criterion::NotExact {
                text: text.to_string(),
            });
        }
    }
    let (op, rest) = split_operator(s);
    let rest = rest.trim();
    match op {
        None => {
            if s.is_empty() {
                Ok(Criterion::IsEmpty)
            } else {
                Ok(Criterion::Prefix {
                    pattern: s.to_string(),
                })
            }
        }
        Some(op) => {
            if rest.is_empty() {
                return Ok(match op {
                    Op::Eq => Criterion::IsEmpty,
                    Op::Ne => Criterion::NotEmpty,
                    _ => Criterion::Never,
                });
            }
            if let Ok(n) = rest.parse::<f64>() {
                if n.is_finite() {
                    return Ok(Criterion::Num { op, n });
                }
            }
            // A text operand stays TEXT: `=5` coerces to a number above, but a
            // criterion like `>abc` compares as text and `=Ap` is a prefix.
            Ok(match op {
                Op::Eq => Criterion::Prefix {
                    pattern: rest.to_string(),
                },
                Op::Ne => Criterion::NotPrefix {
                    pattern: rest.to_string(),
                },
                Op::Lt | Op::Le | Op::Gt | Op::Ge => Criterion::TextCmp {
                    op,
                    text: rest.to_string(),
                },
            })
        }
    }
}

/// A bare text criterion is a prefix match, so anchor the pattern at the start
/// of the value with a trailing `*` (this also keeps `Ap?` needing two chars).
fn prefix_pattern(pattern: &str) -> String {
    format!("{pattern}*")
}

fn order_result(op: Op, ord: Ordering, eq: bool) -> bool {
    match op {
        Op::Eq => eq,
        Op::Ne => !eq,
        Op::Lt => ord == Ordering::Less,
        Op::Le => ord != Ordering::Greater,
        Op::Gt => ord == Ordering::Greater,
        Op::Ge => ord != Ordering::Less,
    }
}

/// Does a database cell satisfy a compiled criterion? Type-sensitive: numeric
/// criteria meet only numbers, text criteria only text. An error cell never
/// satisfies any criterion (the row simply does not match).
fn criterion_matches(v: &CalcValue, c: &Criterion) -> Result<bool, CalcError> {
    if v.is_error() {
        return Ok(false);
    }
    match c {
        Criterion::Num { op, n } => match v {
            CalcValue::Number(x) => {
                let ord = x.partial_cmp(n).unwrap_or(Ordering::Equal);
                Ok(order_result(*op, ord, ord == Ordering::Equal))
            }
            _ => Ok(false),
        },
        Criterion::Prefix { pattern } => match v {
            CalcValue::Text(t) => Ok(wildcard_match(&prefix_pattern(pattern), t)),
            _ => Ok(false),
        },
        Criterion::Exact { text } => match v {
            CalcValue::Text(t) => Ok(t.eq_ignore_ascii_case(text)),
            _ => Ok(false),
        },
        Criterion::NotPrefix { pattern } => match v {
            CalcValue::Text(t) => Ok(!wildcard_match(&prefix_pattern(pattern), t)),
            _ => Ok(false),
        },
        Criterion::NotExact { text } => match v {
            CalcValue::Text(t) => Ok(!t.eq_ignore_ascii_case(text)),
            _ => Ok(false),
        },
        Criterion::TextCmp { op, text } => match v {
            CalcValue::Text(t) => {
                let ord = t.to_ascii_lowercase().cmp(&text.to_ascii_lowercase());
                Ok(order_result(*op, ord, ord == Ordering::Equal))
            }
            _ => Ok(false),
        },
        Criterion::IsEmpty => Ok(v.is_blank() || matches!(v, CalcValue::Text(t) if t.is_empty())),
        Criterion::NotEmpty => {
            Ok(!(v.is_blank() || matches!(v, CalcValue::Text(t) if t.is_empty())))
        }
        Criterion::BoolVal(b) => Ok(matches!(v, CalcValue::Bool(x) if *x == *b)),
        Criterion::Never => Ok(false),
    }
}

// -- criteria compilation -----------------------------------------------------

/// One criteria condition row, pre-mapped onto database column indices.
enum RowPred {
    /// AND of (database_column, criterion). An empty vector matches everything
    /// (an all-blank criteria row is "no filter"), which is the Excel gotcha.
    Conds(Vec<(usize, Criterion)>),
    /// A condition named a column the database does not have: never matches.
    Dead,
}

fn header_text(v: &CalcValue) -> Option<String> {
    coerce_text(v).ok()
}

fn header_eq(db_header: &CalcValue, name: &str) -> bool {
    header_text(db_header).is_some_and(|h| h.eq_ignore_ascii_case(name))
}

/// Compile the criteria grid once: header names map to database columns, then
/// each condition row becomes a predicate. Errors in a criteria cell propagate.
fn compile_criteria(db: &ArrayValue, crit: &ArrayValue) -> Result<Vec<RowPred>, CalcError> {
    let db_cols = db.cols as usize;
    let mut rows = Vec::new();
    for r in 1..crit.rows {
        let mut dead = false;
        let mut conds = Vec::new();
        for c in 0..crit.cols {
            let header = crit.get(0, c);
            if header.is_blank() {
                continue; // a blank header is no condition on this column
            }
            let Some(hname) = header_text(header) else {
                continue;
            };
            if hname.is_empty() {
                continue;
            }
            let Some(dc) = (0..db_cols).find(|&dc| header_eq(db.get(0, dc as u32), &hname)) else {
                dead = true; // criteria header absent from the database
                continue;
            };
            let cell = crit.get(r, c);
            if cell.is_blank() {
                continue; // an empty criteria cell is "no condition", not "equals blank"
            }
            conds.push((dc, parse_criterion(cell)?));
        }
        rows.push(if dead {
            RowPred::Dead
        } else {
            RowPred::Conds(conds)
        });
    }
    Ok(rows)
}

/// Does one database record satisfy the OR-of-ANDs criteria predicate?
fn row_matches(db: &ArrayValue, db_row: u32, preds: &[RowPred]) -> Result<bool, CalcError> {
    if preds.is_empty() {
        return Ok(false); // no criteria rows: nothing matches
    }
    for pred in preds {
        let ok = match pred {
            RowPred::Dead => continue,
            RowPred::Conds(conds) => {
                let mut all = true;
                for (dc, cr) in conds {
                    if !criterion_matches(db.get(db_row, *dc as u32), cr)? {
                        all = false;
                        break;
                    }
                }
                all
            }
        };
        if ok {
            return Ok(true);
        }
    }
    Ok(false)
}

// -- field resolution ---------------------------------------------------------

/// The aggregation column: a 1-based index or a header name (case-insensitive),
/// or `None` when DCOUNT/DCOUNTA omit the field to count matching records.
/// An unmatched name or an out-of-range index is `#VALUE!`.
fn resolve_field(
    ctx: &FuncCtx,
    args: &[FuncArg],
    db: &ArrayValue,
    allow_omit: bool,
) -> Result<Option<usize>, CalcError> {
    match args[1].value(ctx)? {
        CalcValue::Number(n) => {
            let idx = n.trunc();
            if !(1.0..=(db.cols as f64)).contains(&idx) {
                return Err(CalcError::Value);
            }
            Ok(Some(idx as usize - 1))
        }
        CalcValue::Text(name) => {
            for dc in 0..db.cols as usize {
                if header_eq(db.get(0, dc as u32), &name) {
                    return Ok(Some(dc));
                }
            }
            Err(CalcError::Value)
        }
        CalcValue::Blank => {
            if allow_omit {
                Ok(None)
            } else {
                Err(CalcError::Value)
            }
        }
        CalcValue::Error(e) => Err(e),
        _ => Err(CalcError::Value),
    }
}

// -- the shared engine ---------------------------------------------------------

#[derive(Clone, Copy)]
enum Agg {
    Sum,
    Average,
    Count,
    CountA,
    Max,
    Min,
    Product,
    Get,
    StDevS,
    StDevP,
    VarS,
    VarP,
}

/// Welford's online moments — numerically stable variance, never
/// sum-of-squares minus square-of-sum.
struct Welford {
    n: u64,
    mean: f64,
    m2: f64,
}

impl Welford {
    fn new() -> Self {
        Welford {
            n: 0,
            mean: 0.0,
            m2: 0.0,
        }
    }
    fn push(&mut self, x: f64) {
        self.n += 1;
        let delta = x - self.mean;
        self.mean += delta / self.n as f64;
        self.m2 += delta * (x - self.mean);
    }
    fn var_sample(&self) -> f64 {
        self.m2 / (self.n - 1) as f64
    }
    fn var_pop(&self) -> f64 {
        self.m2 / self.n as f64
    }
}

/// D<AGG>(database, field, criteria). Zero matching records is `#DIV/0!` for
/// DAVERAGE (and for a sample stddev/variance with under two points) and `0`
/// for DSUM/DPRODUCT/DMAX/DMIN; DGET's zero/two-match cases are its own
/// `#VALUE!` / `#NUM!` pair.
fn database_fn(
    ctx: &FuncCtx,
    args: &[FuncArg],
    agg: Agg,
    allow_omit: bool,
) -> Result<CalcValue, CalcError> {
    let db = range_array(ctx, &args[0])?;
    let (field, crit_idx) = if args.len() == 2 {
        (None, 1) // DCOUNT/DCOUNTA with the field omitted
    } else {
        (resolve_field(ctx, args, &db, allow_omit)?, 2)
    };
    let crit = range_array(ctx, &args[crit_idx])?;
    let preds = compile_criteria(&db, &crit)?;

    let mut sum = 0.0;
    let mut prod = 1.0;
    let mut num_count = 0usize;
    let mut matched = 0usize;
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut w = Welford::new();
    let mut single: Option<CalcValue> = None;

    for r in 1..db.rows {
        if !row_matches(&db, r, &preds)? {
            continue;
        }
        matched += 1;
        let Some(fc) = field else {
            continue; // counting matching records is decided below via `matched`
        };
        let v = db.get(r, fc as u32);
        match agg {
            Agg::Count => match v {
                CalcValue::Number(_) => num_count += 1,
                CalcValue::Error(e) => return Err(*e),
                _ => {}
            },
            Agg::CountA => {
                if !v.is_blank() {
                    num_count += 1;
                }
            }
            Agg::Get => {
                if single.is_none() {
                    single = Some(v.clone());
                }
            }
            _ => match v {
                CalcValue::Number(n) => {
                    let n = *n;
                    sum += n;
                    prod *= n;
                    num_count += 1;
                    min = min.min(n);
                    max = max.max(n);
                    w.push(n);
                }
                CalcValue::Error(e) => return Err(*e),
                _ => {}
            },
        }
    }
    if field.is_none() {
        num_count = matched;
    }

    match agg {
        Agg::Sum => ok_num(sum),
        Agg::Product => ok_num(if num_count > 0 { prod } else { 0.0 }),
        Agg::Average => {
            if num_count == 0 {
                Err(CalcError::Div0)
            } else {
                ok_num(sum / num_count as f64)
            }
        }
        Agg::Count | Agg::CountA => Ok(CalcValue::Number(num_count as f64)),
        Agg::Max => Ok(CalcValue::Number(if num_count > 0 { max } else { 0.0 })),
        Agg::Min => Ok(CalcValue::Number(if num_count > 0 { min } else { 0.0 })),
        Agg::Get => {
            if matched == 0 {
                Err(CalcError::Value)
            } else if matched > 1 {
                Err(CalcError::Num)
            } else {
                Ok(single.unwrap_or(CalcValue::Blank))
            }
        }
        Agg::StDevS => {
            if num_count < 2 {
                Err(CalcError::Div0)
            } else {
                ok_num(w.var_sample().sqrt())
            }
        }
        Agg::StDevP => {
            if num_count == 0 {
                Err(CalcError::Div0)
            } else {
                ok_num(w.var_pop().sqrt())
            }
        }
        Agg::VarS => {
            if num_count < 2 {
                Err(CalcError::Div0)
            } else {
                ok_num(w.var_sample())
            }
        }
        Agg::VarP => {
            if num_count == 0 {
                Err(CalcError::Div0)
            } else {
                ok_num(w.var_pop())
            }
        }
    }
}

// -- the twelve entry points ---------------------------------------------------

fn dsum(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    database_fn(ctx, args, Agg::Sum, false)
}
fn daverage(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    database_fn(ctx, args, Agg::Average, false)
}
fn dcount(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    database_fn(ctx, args, Agg::Count, true)
}
fn dcounta(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    database_fn(ctx, args, Agg::CountA, true)
}
fn dmax(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    database_fn(ctx, args, Agg::Max, false)
}
fn dmin(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    database_fn(ctx, args, Agg::Min, false)
}
fn dproduct(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    database_fn(ctx, args, Agg::Product, false)
}
fn dget(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    database_fn(ctx, args, Agg::Get, false)
}
fn dstdev(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    database_fn(ctx, args, Agg::StDevS, false)
}
fn dstdevp(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    database_fn(ctx, args, Agg::StDevP, false)
}
fn dvar(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    database_fn(ctx, args, Agg::VarS, false)
}
fn dvarp(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    database_fn(ctx, args, Agg::VarP, false)
}

const DSUM: FuncSpec = FuncSpec {
    name: "DSUM",
    min_args: 3,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: dsum,
};
const DAVERAGE: FuncSpec = FuncSpec {
    name: "DAVERAGE",
    min_args: 3,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: daverage,
};
const DCOUNT: FuncSpec = FuncSpec {
    name: "DCOUNT",
    min_args: 2,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: dcount,
};
const DCOUNTA: FuncSpec = FuncSpec {
    name: "DCOUNTA",
    min_args: 2,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: dcounta,
};
const DMAX: FuncSpec = FuncSpec {
    name: "DMAX",
    min_args: 3,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: dmax,
};
const DMIN: FuncSpec = FuncSpec {
    name: "DMIN",
    min_args: 3,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: dmin,
};
const DPRODUCT: FuncSpec = FuncSpec {
    name: "DPRODUCT",
    min_args: 3,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: dproduct,
};
const DGET: FuncSpec = FuncSpec {
    name: "DGET",
    min_args: 3,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: dget,
};
const DSTDEV: FuncSpec = FuncSpec {
    name: "DSTDEV",
    min_args: 3,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: dstdev,
};
const DSTDEVP: FuncSpec = FuncSpec {
    name: "DSTDEVP",
    min_args: 3,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: dstdevp,
};
const DVAR: FuncSpec = FuncSpec {
    name: "DVAR",
    min_args: 3,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: dvar,
};
const DVARP: FuncSpec = FuncSpec {
    name: "DVARP",
    min_args: 3,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: dvarp,
};

pub fn register(r: &mut Registry) {
    r.register(&DSUM);
    r.register(&DAVERAGE);
    r.register(&DCOUNT);
    r.register(&DCOUNTA);
    r.register(&DMAX);
    r.register(&DMIN);
    r.register(&DPRODUCT);
    r.register(&DGET);
    r.register(&DSTDEV);
    r.register(&DSTDEVP);
    r.register(&DVAR);
    r.register(&DVARP);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turbo::calc::testkit::Grid;
    use pretty_assertions::assert_eq;

    /// The shared test database (6 records, 4 columns):
    ///   Product    Sales   Region   Qty
    ///   Apples     100     East     10
    ///   Apples     200     West     "high"
    ///   Pears      150     East     (blank)
    ///   Pears      250     West     20
    ///   Bananas    75      East     "med"
    ///   Bananas    125     West     15
    fn db_grid() -> Grid {
        Grid::empty()
            .set_text("A1", "Product")
            .set_text("B1", "Sales")
            .set_text("C1", "Region")
            .set_text("D1", "Qty")
            .set_text("A2", "Apples")
            .set_num("B2", 100.0)
            .set_text("C2", "East")
            .set_num("D2", 10.0)
            .set_text("A3", "Apples")
            .set_num("B3", 200.0)
            .set_text("C3", "West")
            .set_text("D3", "high")
            .set_text("A4", "Pears")
            .set_num("B4", 150.0)
            .set_text("C4", "East")
            .set_text("A5", "Pears")
            .set_num("B5", 250.0)
            .set_text("C5", "West")
            .set_num("D5", 20.0)
            .set_text("A6", "Bananas")
            .set_num("B6", 75.0)
            .set_text("C6", "East")
            .set_text("D6", "med")
            .set_text("A7", "Bananas")
            .set_num("B7", 125.0)
            .set_text("C7", "West")
            .set_num("D7", 15.0)
    }

    fn approx_num(g: &Grid, formula: &str, expected: f64) {
        let got = g.num(formula);
        assert!(
            (got - expected).abs() <= 1e-9 * expected.abs().max(1.0),
            "{formula} -> {got}, expected {expected}"
        );
    }

    #[test]
    fn dsum_single_and_row_by_name_and_index() {
        let g = db_grid()
            .set_text("F1", "Product")
            .set_text("G1", "Region")
            .set_text("F2", "Apples")
            .set_text("G2", "East");
        assert_eq!(g.num("=DSUM(A1:D7,2,F1:G2)"), 100.0);
        assert_eq!(g.num("=DSUM(A1:D7,\"Sales\",F1:G2)"), 100.0);
        // function names and header names are case-insensitive
        assert_eq!(g.num("=dsum(A1:D7,\"sales\",F1:G2)"), 100.0);
    }

    #[test]
    fn two_criteria_rows_or() {
        let g = db_grid()
            .set_text("F1", "Product")
            .set_text("G1", "Region")
            .set_text("F2", "Apples")
            .set_text("G2", "East")
            .set_text("F3", "Pears")
            .set_text("G3", "West");
        assert_eq!(g.num("=DSUM(A1:D7,\"Sales\",F1:G3)"), 350.0);
    }

    #[test]
    fn bare_text_criterion_is_a_prefix_match() {
        let g = db_grid().set_text("F1", "Product").set_text("F2", "App");
        assert_eq!(g.num("=DSUM(A1:D7,\"Sales\",F1:F2)"), 300.0);
    }

    #[test]
    fn quoted_equals_is_an_exact_match() {
        let g = db_grid()
            .set_text("F1", "Product")
            .set_text("F2", "=\"Apples\"");
        assert_eq!(g.num("=DSUM(A1:D7,\"Sales\",F1:F2)"), 300.0);
        let g2 = db_grid()
            .set_text("F1", "Product")
            .set_text("F2", "=\"App\"");
        assert_eq!(g2.num("=DSUM(A1:D7,\"Sales\",F1:F2)"), 0.0);
    }

    #[test]
    fn wildcards_in_text_criteria() {
        let g = db_grid().set_text("F1", "Product").set_text("F2", "Ap*");
        assert_eq!(g.num("=DSUM(A1:D7,\"Sales\",F1:F2)"), 300.0);
        let g2 = db_grid().set_text("F1", "Product").set_text("F2", "?ears");
        assert_eq!(g2.num("=DSUM(A1:D7,\"Sales\",F1:F2)"), 400.0);
    }

    #[test]
    fn empty_criteria_cell_is_ignored() {
        let g = db_grid()
            .set_text("F1", "Product")
            .set_text("G1", "Sales")
            .set_text("F2", "Apples"); // G2 is blank: no condition on Sales
        assert_eq!(g.num("=DSUM(A1:D7,\"Sales\",F1:G2)"), 300.0);
    }

    #[test]
    fn operator_criteria_numeric() {
        let g = db_grid().set_text("F1", "Sales").set_text("F2", ">100");
        assert_eq!(g.num("=DSUM(A1:D7,\"Sales\",F1:F2)"), 725.0);
        let g2 = db_grid().set_text("F1", "Sales").set_text("F2", "<100");
        assert_eq!(g2.num("=DSUM(A1:D7,\"Sales\",F1:F2)"), 75.0);
        let g3 = db_grid().set_text("F1", "Sales").set_text("F2", ">=100");
        assert_eq!(g3.num("=DSUM(A1:D7,\"Sales\",F1:F2)"), 825.0);
        let g4 = db_grid().set_text("F1", "Sales").set_text("F2", "<>100");
        assert_eq!(g4.num("=DSUM(A1:D7,\"Sales\",F1:F2)"), 800.0);
    }

    #[test]
    fn criteria_header_not_in_database_matches_nothing() {
        let g = db_grid().set_text("F1", "Missing").set_text("F2", "5");
        assert_eq!(g.num("=DSUM(A1:D7,\"Sales\",F1:F2)"), 0.0);
        assert_eq!(g.error("=DGET(A1:D7,\"Sales\",F1:F2)"), CalcError::Value);
    }

    #[test]
    fn dget_zero_and_two_matches_are_distinct_errors() {
        let one = db_grid()
            .set_text("F1", "Product")
            .set_text("G1", "Region")
            .set_text("F2", "Apples")
            .set_text("G2", "East");
        assert_eq!(one.num("=DGET(A1:D7,\"Sales\",F1:G2)"), 100.0);
        let two = db_grid().set_text("F1", "Product").set_text("F2", "Apples");
        assert_eq!(two.error("=DGET(A1:D7,\"Sales\",F1:F2)"), CalcError::Num);
        let zero = db_grid().set_text("F1", "Product").set_text("F2", "Kiwis");
        assert_eq!(zero.error("=DGET(A1:D7,\"Sales\",F1:F2)"), CalcError::Value);
    }

    #[test]
    fn dcount_vs_dcounta_on_mixed_column() {
        let g = db_grid().set_text("F1", "Region").set_text("F2", "East");
        // East Qty values: 10, (blank), "med" → DCOUNT counts only 10.
        assert_eq!(g.num("=DCOUNT(A1:D7,\"Qty\",F1:F2)"), 1.0);
        // DCOUNTA counts 10 and "med".
        assert_eq!(g.num("=DCOUNTA(A1:D7,\"Qty\",F1:F2)"), 2.0);
        // Field omitted: both count the three matching records.
        assert_eq!(g.num("=DCOUNT(A1:D7,,F1:F2)"), 3.0);
        assert_eq!(g.num("=DCOUNTA(A1:D7,,F1:F2)"), 3.0);
    }

    #[test]
    fn text_criterion_does_not_match_numbers() {
        // "10" as TEXT must not silently match the numeric 10.
        let as_text = db_grid().set_text("F1", "Qty").set_text("F2", "10");
        assert_eq!(as_text.num("=DSUM(A1:D7,\"Sales\",F1:F2)"), 0.0);
        // 10 as a NUMBER matches the numeric-10 row (Apples).
        let as_num = db_grid().set_text("F1", "Qty").set_num("F2", 10.0);
        assert_eq!(as_num.num("=DSUM(A1:D7,\"Sales\",F1:F2)"), 100.0);
    }

    #[test]
    fn aggregates_and_zero_match_behaviour() {
        let g = db_grid().set_text("F1", "Region").set_text("F2", "East");
        approx_num(&g, "=DAVERAGE(A1:D7,\"Sales\",F1:F2)", 325.0 / 3.0);
        assert_eq!(g.num("=DMAX(A1:D7,\"Sales\",F1:F2)"), 150.0);
        assert_eq!(g.num("=DMIN(A1:D7,\"Sales\",F1:F2)"), 75.0);
        assert_eq!(
            g.num("=DPRODUCT(A1:D7,\"Sales\",F1:F2)"),
            100.0 * 150.0 * 75.0
        );
        // No matching records: DSUM/DPRODUCT are 0, DAVERAGE is #DIV/0!.
        let none = db_grid().set_text("F1", "Region").set_text("F2", "South");
        assert_eq!(none.num("=DSUM(A1:D7,\"Sales\",F1:F2)"), 0.0);
        assert_eq!(none.num("=DPRODUCT(A1:D7,\"Sales\",F1:F2)"), 0.0);
        assert_eq!(
            none.error("=DAVERAGE(A1:D7,\"Sales\",F1:F2)"),
            CalcError::Div0
        );
    }

    #[test]
    fn stdev_sample_vs_population_differ() {
        let g = db_grid().set_text("F1", "Region").set_text("F2", "East");
        let m2: f64 = 2916.6666666666665;
        let s = g.num("=DSTDEV(A1:D7,\"Sales\",F1:F2)");
        let p = g.num("=DSTDEVP(A1:D7,\"Sales\",F1:F2)");
        assert!(
            (s - p).abs() > 1e-6,
            "sample and population stddev must differ"
        );
        assert!((s - (m2 / 2.0).sqrt()).abs() < 1e-9);
        assert!((p - (m2 / 3.0).sqrt()).abs() < 1e-9);
        approx_num(&g, "=DVAR(A1:D7,\"Sales\",F1:F2)", m2 / 2.0);
        approx_num(&g, "=DVARP(A1:D7,\"Sales\",F1:F2)", m2 / 3.0);
        // A single matching point: sample stddev is #DIV/0!, population is 0.
        let one = db_grid()
            .set_text("F1", "Region")
            .set_text("G1", "Product")
            .set_text("F2", "East")
            .set_text("G2", "Pears");
        assert_eq!(one.error("=DSTDEV(A1:D7,\"Sales\",F1:G2)"), CalcError::Div0);
        assert_eq!(one.num("=DSTDEVP(A1:D7,\"Sales\",F1:G2)"), 0.0);
    }

    #[test]
    fn field_name_and_index_errors() {
        let g = db_grid().set_text("F1", "Region").set_text("F2", "East");
        assert_eq!(g.error("=DSUM(A1:D7,\"Nope\",F1:F2)"), CalcError::Value);
        assert_eq!(g.error("=DSUM(A1:D7,5,F1:F2)"), CalcError::Value);
        assert_eq!(g.error("=DSUM(A1:D7,0,F1:F2)"), CalcError::Value);
    }
}
