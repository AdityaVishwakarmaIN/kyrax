// functions/information.rs — the information function family (plan §5 Tier 1).
//
// Registry contract: `register` below, same signature as every other family.
// `functions/mod.rs` already declares `mod information;` and calls
// `information::register(&mut r)` — this file never edits it.
//
// Two groups live here:
//   * the `IS*` predicates, `NA`, `N`, `TYPE` and `ERROR.TYPE`, which inspect a
//     value's kind rather than its content, and therefore must NOT propagate an
//     error argument — seeing the error IS their job;
//   * `ROW` / `COLUMN` / `ROWS` / `COLUMNS`, which read the raw reference via
//     `FuncArg::as_reference` instead of materializing it, so `ROWS(A:A)`
//     costs nothing.

use super::{FuncArg, FuncCtx, FuncSpec, MAX_COLS, MAX_ROWS, Registry};
use crate::turbo::calc::ast::{CellRef, RefCore, RefExpr};
use crate::turbo::calc::coerce::{coerce_number, coerce_text};
use crate::turbo::calc::refs::cell_to_a1;
use crate::turbo::calc::value::{CalcError, CalcValue};

/// The value an inspection function sees. An argument that failed to evaluate
/// arrives as `CalcValue::Error` (the eval loop converts it), so this never
/// short-circuits — that is exactly what `ISERROR` needs.
fn seen(ctx: &FuncCtx, arg: &FuncArg) -> CalcValue {
    match arg.value(ctx) {
        Ok(v) => v,
        Err(e) => CalcValue::Error(e),
    }
}

/// A 1x1 array behaves as its single element for the predicates.
fn unwrap_single(v: CalcValue) -> CalcValue {
    match &v {
        CalcValue::Array(a) if a.rows == 1 && a.cols == 1 => a.get(0, 0).clone(),
        _ => v,
    }
}

fn f_isblank(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let v = unwrap_single(seen(ctx, &args[0]));
    Ok(CalcValue::Bool(matches!(v, CalcValue::Blank)))
}

fn f_isnumber(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let v = unwrap_single(seen(ctx, &args[0]));
    Ok(CalcValue::Bool(matches!(v, CalcValue::Number(_))))
}

fn f_istext(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let v = unwrap_single(seen(ctx, &args[0]));
    Ok(CalcValue::Bool(matches!(v, CalcValue::Text(_))))
}

fn f_isnontext(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let v = unwrap_single(seen(ctx, &args[0]));
    Ok(CalcValue::Bool(!matches!(v, CalcValue::Text(_))))
}

fn f_islogical(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let v = unwrap_single(seen(ctx, &args[0]));
    Ok(CalcValue::Bool(matches!(v, CalcValue::Bool(_))))
}

fn f_iserror(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let v = unwrap_single(seen(ctx, &args[0]));
    Ok(CalcValue::Bool(v.is_error()))
}

/// `ISERR` is `ISERROR` minus `#N/A`.
fn f_iserr(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let v = unwrap_single(seen(ctx, &args[0]));
    Ok(CalcValue::Bool(
        matches!(v.error(), Some(e) if e != CalcError::Na),
    ))
}

fn f_isna(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let v = unwrap_single(seen(ctx, &args[0]));
    Ok(CalcValue::Bool(v.error() == Some(CalcError::Na)))
}

/// True only for a reference the engine can actually address. A defined name,
/// table or external reference is reported as not-a-reference rather than
/// claimed and then failed at resolve time.
fn f_isref(_ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let is_ref = matches!(
        args[0].as_reference(),
        Some(RefExpr::Local(_)) | Some(RefExpr::Sheet { .. })
    );
    Ok(CalcValue::Bool(is_ref))
}

fn parity(ctx: &FuncCtx, args: &[FuncArg], want_odd: bool) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    // Excel truncates toward zero before testing parity.
    let t = n.trunc();
    if !t.is_finite() || t.abs() >= 9.007_199_254_740_992e15 {
        return Err(CalcError::Num);
    }
    let odd = (t as i64).rem_euclid(2) == 1;
    Ok(CalcValue::Bool(odd == want_odd))
}

fn f_iseven(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    parity(ctx, args, false)
}

fn f_isodd(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    parity(ctx, args, true)
}

/// `NA()` yields the `#N/A` **value**, not an evaluation failure, so a caller
/// like `IFNA` can catch it.
fn f_na(_ctx: &FuncCtx, _args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    Ok(CalcValue::err(CalcError::Na))
}

/// `N`: numbers pass through, TRUE is 1, everything non-numeric is 0, an error
/// stays an error.
fn f_n(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    Ok(match unwrap_single(seen(ctx, &args[0])) {
        CalcValue::Number(n) => CalcValue::Number(n),
        CalcValue::Bool(b) => CalcValue::Number(if b { 1.0 } else { 0.0 }),
        CalcValue::Error(e) => CalcValue::Error(e),
        _ => CalcValue::Number(0.0),
    })
}

/// Excel's `TYPE` codes: 1 number, 2 text, 4 logical, 16 error, 64 array. A
/// blank cell types as a number, matching Excel.
fn f_type(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let code = match seen(ctx, &args[0]) {
        CalcValue::Number(_) | CalcValue::Blank => 1.0,
        CalcValue::Text(_) => 2.0,
        CalcValue::Bool(_) => 4.0,
        CalcValue::Error(_) => 16.0,
        CalcValue::Array(_) => 64.0,
    };
    Ok(CalcValue::Number(code))
}

/// `ERROR.TYPE`: the documented 1-7 codes; anything that is not one of those
/// errors (including a non-error value) is `#N/A`.
fn f_error_type(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let Some(e) = unwrap_single(seen(ctx, &args[0])).error() else {
        return Ok(CalcValue::err(CalcError::Na));
    };
    let code = match e {
        CalcError::Null => 1.0,
        CalcError::Div0 => 2.0,
        CalcError::Value => 3.0,
        CalcError::Ref => 4.0,
        CalcError::Name => 5.0,
        CalcError::Num => 6.0,
        CalcError::Na => 7.0,
        _ => return Ok(CalcValue::err(CalcError::Na)),
    };
    Ok(CalcValue::Number(code))
}

// -- ROW / COLUMN / ROWS / COLUMNS -------------------------------------------

/// The cartesian part of an addressable reference, or `None` when the argument
/// is a name/table/external/3-D reference this layer cannot resolve to
/// coordinates.
fn core_of(arg: &FuncArg) -> Option<&RefCore> {
    match arg.as_reference()? {
        RefExpr::Local(c) => Some(c),
        RefExpr::Sheet { inner, .. } => Some(inner),
        _ => None,
    }
}

/// `(first_row, first_col, rows, cols)` — 0-based origin, sizes in cells.
/// Whole rows and columns report the full Excel grid extent, which is what
/// `ROWS(A:A)` and `COLUMNS(1:1)` return.
fn extent(core: &RefCore) -> (u32, u32, u32, u32) {
    match core {
        RefCore::Cell(c) => (c.row, u32::from(c.col), 1, 1),
        RefCore::Range(r) => (
            r.start.row,
            u32::from(r.start.col),
            r.end.row - r.start.row + 1,
            u32::from(r.end.col - r.start.col) + 1,
        ),
        RefCore::Row(r) => (r.start, 0, r.end - r.start + 1, u32::from(MAX_COLS)),
        RefCore::Column(c) => (
            0,
            u32::from(c.start),
            MAX_ROWS,
            u32::from(c.end - c.start) + 1,
        ),
    }
}

/// `ROW()` is the formula's own row; `ROW(ref)` is the reference's top row.
fn f_row(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    if args.is_empty() {
        return Ok(CalcValue::Number(f64::from(ctx.row) + 1.0));
    }
    let core = core_of(&args[0]).ok_or(CalcError::Ref)?;
    Ok(CalcValue::Number(f64::from(extent(core).0) + 1.0))
}

fn f_column(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    if args.is_empty() {
        return Ok(CalcValue::Number(f64::from(ctx.col) + 1.0));
    }
    let core = core_of(&args[0]).ok_or(CalcError::Ref)?;
    Ok(CalcValue::Number(f64::from(extent(core).1) + 1.0))
}

/// Size of a reference or of an already-materialized array.
fn size(ctx: &FuncCtx, arg: &FuncArg, rows: bool) -> Result<CalcValue, CalcError> {
    if let Some(core) = core_of(arg) {
        let (_, _, r, c) = extent(core);
        return Ok(CalcValue::Number(f64::from(if rows { r } else { c })));
    }
    match arg.value(ctx)? {
        CalcValue::Array(a) => Ok(CalcValue::Number(f64::from(if rows {
            a.rows
        } else {
            a.cols
        }))),
        CalcValue::Error(e) => Err(e),
        // A scalar is a 1x1 area.
        _ => Ok(CalcValue::Number(1.0)),
    }
}

fn f_rows(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    size(ctx, &args[0], true)
}

fn f_columns(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    size(ctx, &args[0], false)
}

// -- SHEET / CELL / INFO ------------------------------------------------------

/// `SHEET([value])`: the 1-based sheet number of the formula's own sheet, of a
/// reference, or of a sheet name. A name that does not resolve is `#REF!`.
///
/// `SHEETS` is deliberately NOT registered: its no-argument form needs the
/// workbook's total sheet count, which `CellResolver` does not expose. An
/// unregistered name routes to the fullCalcOnLoad fallback, which is correct.
fn f_sheet(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    if args.is_empty() {
        return Ok(CalcValue::Number(f64::from(ctx.sheet) + 1.0));
    }
    let sheet = match args[0].as_reference() {
        Some(RefExpr::Local(_)) => Some(ctx.sheet),
        Some(RefExpr::Sheet { name, .. }) => ctx.resolver.sheet_index(name),
        Some(_) => None,
        None => {
            let name = coerce_text(&args[0].value(ctx)?)?;
            ctx.resolver.sheet_index(&name)
        }
    };
    match sheet {
        Some(s) => Ok(CalcValue::Number(f64::from(s) + 1.0)),
        None => Err(CalcError::Ref),
    }
}

/// The formula cell's own coordinates, or the top-left cell of a reference
/// argument (carrying the sheet it lives on, which differs for a `Sheet!` ref).
fn cell_target(ctx: &FuncCtx, arg: Option<&FuncArg>) -> Result<(u32, u32, u16), CalcError> {
    let Some(arg) = arg else {
        return Ok((ctx.sheet, ctx.row, ctx.col as u16));
    };
    match arg.as_reference() {
        Some(RefExpr::Local(core)) => first_cell(ctx.sheet, core),
        Some(RefExpr::Sheet { name, inner }) => {
            let idx = ctx.resolver.sheet_index(name).ok_or(CalcError::Ref)?;
            first_cell(idx, inner)
        }
        Some(_) => Err(CalcError::Ref),
        None => Err(CalcError::Value),
    }
}

fn first_cell(sheet: u32, core: &RefCore) -> Result<(u32, u32, u16), CalcError> {
    match core {
        RefCore::Cell(c) => Ok((sheet, c.row, c.col)),
        RefCore::Range(r) => Ok((sheet, r.start.row, r.start.col)),
        RefCore::Row(r) => Ok((sheet, r.start, 0)),
        RefCore::Column(c) => Ok((sheet, 0, c.start)),
    }
}

/// `CELL(info_type, [reference])`: the info_types this engine can answer from
/// a value grid alone — `"address"`, `"col"`, `"row"` come from reference
/// geometry, `"contents"` is the cell's value (a blank reads as 0, matching
/// Excel). Every other documented type (`"type"`, `"format"`, `"width"`,
/// `"prefix"`, `"protect"`, `"filename"`, ...) needs cell metadata — number
/// formats, column widths, a formula flag — that `CellResolver` does not
/// expose, so it is `#VALUE!` rather than a guessed value. A formula cell in
/// particular would make `"type"`'s `"c"` answer undecidable, so that type is
/// not claimed either.
fn f_cell(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let key = coerce_text(&args[0].value(ctx)?)?;
    let key = key.trim().to_ascii_lowercase();
    let (sheet, row, col) = cell_target(ctx, args.get(1))?;
    match key.as_str() {
        "address" => Ok(CalcValue::text(cell_to_a1(&CellRef {
            col,
            row,
            abs_col: true,
            abs_row: true,
        }))),
        "col" => Ok(CalcValue::Number(f64::from(col) + 1.0)),
        "row" => Ok(CalcValue::Number(f64::from(row) + 1.0)),
        "contents" => {
            let v = ctx
                .resolver
                .cell(sheet, row, u32::from(col))
                .unwrap_or(CalcValue::Blank);
            Ok(match v {
                CalcValue::Blank => CalcValue::Number(0.0),
                other => other,
            })
        }
        _ => Err(CalcError::Value),
    }
}

/// `INFO(type_text)`: environment facts. A library owns no workbook file or
/// application window, so of the documented types only `"system"` has a real
/// answer here — the OS as Excel spells it, on the two platforms Excel runs
/// on. Every other type (`"directory"`, `"numfile"`, `"osversion"`, `"recalc"`,
/// `"release"`) is `#N/A`: guessing a path or version string would be worse
/// than not answering.
fn f_info(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let ty = coerce_text(&args[0].value(ctx)?)?;
    match ty.trim().to_ascii_lowercase().as_str() {
        "system" => match std::env::consts::OS {
            "windows" => Ok(CalcValue::text("Windows")),
            "macos" => Ok(CalcValue::text("Macintosh")),
            _ => Err(CalcError::Na),
        },
        _ => Err(CalcError::Na),
    }
}

// -- registry ----------------------------------------------------------------

/// One-argument inspection spec. `array_aware` so a range argument is handed
/// over whole instead of being implicitly intersected first.
const fn spec1(name: &'static str, func: super::Func) -> FuncSpec {
    FuncSpec {
        name,
        min_args: 1,
        max_args: Some(1),
        volatile: false,
        array_aware: true,
        func,
    }
}

const ISBLANK: FuncSpec = spec1("ISBLANK", f_isblank);
const ISNUMBER: FuncSpec = spec1("ISNUMBER", f_isnumber);
const ISTEXT: FuncSpec = spec1("ISTEXT", f_istext);
const ISNONTEXT: FuncSpec = spec1("ISNONTEXT", f_isnontext);
const ISLOGICAL: FuncSpec = spec1("ISLOGICAL", f_islogical);
const ISERROR: FuncSpec = spec1("ISERROR", f_iserror);
const ISERR: FuncSpec = spec1("ISERR", f_iserr);
const ISNA: FuncSpec = spec1("ISNA", f_isna);
const ISREF: FuncSpec = spec1("ISREF", f_isref);
const ISEVEN: FuncSpec = spec1("ISEVEN", f_iseven);
const ISODD: FuncSpec = spec1("ISODD", f_isodd);
const N: FuncSpec = spec1("N", f_n);
const TYPE: FuncSpec = spec1("TYPE", f_type);
const ERROR_TYPE: FuncSpec = spec1("ERROR.TYPE", f_error_type);
const ROWS: FuncSpec = spec1("ROWS", f_rows);
const COLUMNS: FuncSpec = spec1("COLUMNS", f_columns);

const NA: FuncSpec = FuncSpec {
    name: "NA",
    min_args: 0,
    max_args: Some(0),
    volatile: false,
    array_aware: false,
    func: f_na,
};

const ROW: FuncSpec = FuncSpec {
    name: "ROW",
    min_args: 0,
    max_args: Some(1),
    volatile: false,
    array_aware: true,
    func: f_row,
};

const COLUMN: FuncSpec = FuncSpec {
    name: "COLUMN",
    min_args: 0,
    max_args: Some(1),
    volatile: false,
    array_aware: true,
    func: f_column,
};

const SHEET: FuncSpec = FuncSpec {
    name: "SHEET",
    min_args: 0,
    max_args: Some(1),
    volatile: false,
    array_aware: true,
    func: f_sheet,
};

// CELL and INFO are volatile in Excel (their output can change with the
// environment), so they must never be cached.
const CELL: FuncSpec = FuncSpec {
    name: "CELL",
    min_args: 1,
    max_args: Some(2),
    volatile: true,
    array_aware: true,
    func: f_cell,
};

const INFO: FuncSpec = FuncSpec {
    name: "INFO",
    min_args: 1,
    max_args: Some(1),
    volatile: true,
    array_aware: false,
    func: f_info,
};

pub fn register(r: &mut Registry) {
    r.register(&ISBLANK);
    r.register(&ISNUMBER);
    r.register(&ISTEXT);
    r.register(&ISNONTEXT);
    r.register(&ISLOGICAL);
    r.register(&ISERROR);
    r.register(&ISERR);
    r.register(&ISNA);
    r.register(&ISREF);
    r.register(&ISEVEN);
    r.register(&ISODD);
    r.register(&NA);
    r.register(&N);
    r.register(&TYPE);
    r.register(&ERROR_TYPE);
    r.register(&ROW);
    r.register(&COLUMN);
    r.register(&ROWS);
    r.register(&COLUMNS);
    r.register(&SHEET);
    r.register(&CELL);
    r.register(&INFO);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turbo::calc::ast::{CellRef, ColumnRef, RangeRef};
    use crate::turbo::calc::functions::CellResolver;
    use crate::turbo::calc::testkit::{Grid, Outcome};

    struct NoCells;
    impl CellResolver for NoCells {
        fn cell(&self, _s: u32, _r: u32, _c: u32) -> Option<CalcValue> {
            None
        }
        fn sheet_index(&self, _name: &str) -> Option<u32> {
            None
        }
    }

    fn call(spec: &FuncSpec, args: Vec<FuncArg>) -> Result<CalcValue, CalcError> {
        let resolver = NoCells;
        let ctx = FuncCtx {
            date1904: false,
            sheet: 0,
            row: 4,
            col: 2,
            resolver: &resolver,
        };
        (spec.func)(&ctx, &args)
    }

    fn v(x: CalcValue) -> FuncArg {
        FuncArg::Value(x)
    }

    fn cell(col: u16, row: u32) -> CellRef {
        CellRef {
            col,
            row,
            abs_col: false,
            abs_row: false,
        }
    }

    #[test]
    fn predicates_classify_by_kind() {
        assert_eq!(
            call(&ISBLANK, vec![v(CalcValue::Blank)]),
            Ok(CalcValue::Bool(true))
        );
        assert_eq!(
            call(&ISBLANK, vec![v(CalcValue::text(""))]),
            Ok(CalcValue::Bool(false)),
            "an empty string is not a blank cell"
        );
        assert_eq!(
            call(&ISNUMBER, vec![v(CalcValue::number(0.0))]),
            Ok(CalcValue::Bool(true))
        );
        assert_eq!(
            call(&ISNUMBER, vec![v(CalcValue::text("1"))]),
            Ok(CalcValue::Bool(false)),
            "numeric text is text"
        );
        assert_eq!(
            call(&ISTEXT, vec![v(CalcValue::text("x"))]),
            Ok(CalcValue::Bool(true))
        );
        assert_eq!(
            call(&ISNONTEXT, vec![v(CalcValue::number(1.0))]),
            Ok(CalcValue::Bool(true))
        );
        assert_eq!(
            call(&ISLOGICAL, vec![v(CalcValue::bool(false))]),
            Ok(CalcValue::Bool(true))
        );
    }

    #[test]
    fn error_predicates_split_na_from_the_rest() {
        let div0 = v(CalcValue::err(CalcError::Div0));
        let na = v(CalcValue::err(CalcError::Na));
        assert_eq!(
            call(&ISERROR, vec![div0.clone()]),
            Ok(CalcValue::Bool(true))
        );
        assert_eq!(call(&ISERROR, vec![na.clone()]), Ok(CalcValue::Bool(true)));
        assert_eq!(call(&ISERR, vec![div0.clone()]), Ok(CalcValue::Bool(true)));
        assert_eq!(
            call(&ISERR, vec![na.clone()]),
            Ok(CalcValue::Bool(false)),
            "ISERR excludes #N/A"
        );
        assert_eq!(call(&ISNA, vec![na]), Ok(CalcValue::Bool(true)));
        assert_eq!(call(&ISNA, vec![div0]), Ok(CalcValue::Bool(false)));
        assert_eq!(
            call(&ISERROR, vec![v(CalcValue::number(1.0))]),
            Ok(CalcValue::Bool(false))
        );
    }

    #[test]
    fn isref_only_accepts_addressable_references() {
        assert_eq!(
            call(
                &ISREF,
                vec![FuncArg::Reference(RefExpr::Local(RefCore::Cell(cell(
                    0, 0
                ))))]
            ),
            Ok(CalcValue::Bool(true))
        );
        assert_eq!(
            call(
                &ISREF,
                vec![FuncArg::Reference(RefExpr::Name {
                    name: "tax".into(),
                    sheet: None
                })]
            ),
            Ok(CalcValue::Bool(false))
        );
        assert_eq!(
            call(&ISREF, vec![v(CalcValue::number(1.0))]),
            Ok(CalcValue::Bool(false))
        );
    }

    #[test]
    fn parity_truncates_toward_zero() {
        assert_eq!(
            call(&ISEVEN, vec![v(CalcValue::number(2.9))]),
            Ok(CalcValue::Bool(true))
        );
        assert_eq!(
            call(&ISODD, vec![v(CalcValue::number(3.9))]),
            Ok(CalcValue::Bool(true))
        );
        assert_eq!(
            call(&ISEVEN, vec![v(CalcValue::number(-3.0))]),
            Ok(CalcValue::Bool(false))
        );
        assert_eq!(
            call(&ISODD, vec![v(CalcValue::number(-3.0))]),
            Ok(CalcValue::Bool(true))
        );
        assert_eq!(
            call(&ISEVEN, vec![v(CalcValue::text("x"))]),
            Err(CalcError::Value)
        );
    }

    #[test]
    fn na_n_type_and_error_type() {
        assert_eq!(call(&NA, vec![]), Ok(CalcValue::err(CalcError::Na)));
        assert_eq!(
            call(&N, vec![v(CalcValue::bool(true))]),
            Ok(CalcValue::Number(1.0))
        );
        assert_eq!(
            call(&N, vec![v(CalcValue::text("abc"))]),
            Ok(CalcValue::Number(0.0))
        );
        assert_eq!(
            call(&TYPE, vec![v(CalcValue::text("a"))]),
            Ok(CalcValue::Number(2.0))
        );
        assert_eq!(
            call(&TYPE, vec![v(CalcValue::bool(true))]),
            Ok(CalcValue::Number(4.0))
        );
        assert_eq!(
            call(&TYPE, vec![v(CalcValue::err(CalcError::Ref))]),
            Ok(CalcValue::Number(16.0))
        );
        assert_eq!(
            call(&ERROR_TYPE, vec![v(CalcValue::err(CalcError::Div0))]),
            Ok(CalcValue::Number(2.0))
        );
        assert_eq!(
            call(&ERROR_TYPE, vec![v(CalcValue::number(1.0))]),
            Ok(CalcValue::err(CalcError::Na)),
            "a non-error argument is #N/A, not a code"
        );
    }

    #[test]
    fn row_and_column_use_the_formula_cell_when_bare() {
        // ctx is row index 4, col index 2 -> 1-based 5 and 3
        assert_eq!(call(&ROW, vec![]), Ok(CalcValue::Number(5.0)));
        assert_eq!(call(&COLUMN, vec![]), Ok(CalcValue::Number(3.0)));
        let r = FuncArg::Reference(RefExpr::Local(RefCore::Range(RangeRef {
            start: cell(1, 9),
            end: cell(3, 19),
        })));
        assert_eq!(call(&ROW, vec![r.clone()]), Ok(CalcValue::Number(10.0)));
        assert_eq!(call(&COLUMN, vec![r]), Ok(CalcValue::Number(2.0)));
    }

    #[test]
    fn rows_and_columns_never_materialize_a_whole_column() {
        let range = FuncArg::Reference(RefExpr::Local(RefCore::Range(RangeRef {
            start: cell(0, 0),
            end: cell(2, 4),
        })));
        assert_eq!(call(&ROWS, vec![range.clone()]), Ok(CalcValue::Number(5.0)));
        assert_eq!(call(&COLUMNS, vec![range]), Ok(CalcValue::Number(3.0)));

        let whole_col = FuncArg::Reference(RefExpr::Local(RefCore::Column(ColumnRef {
            start: 0,
            end: 0,
        })));
        assert_eq!(
            call(&ROWS, vec![whole_col.clone()]),
            Ok(CalcValue::Number(f64::from(MAX_ROWS)))
        );
        assert_eq!(call(&COLUMNS, vec![whole_col]), Ok(CalcValue::Number(1.0)));

        assert_eq!(
            call(&ROWS, vec![v(CalcValue::number(1.0))]),
            Ok(CalcValue::Number(1.0))
        );
    }

    // -- SHEET / CELL / INFO (through the real parse→eval path) ---------------

    #[test]
    fn sheet_reports_sheet_numbers() {
        let g = Grid::empty();
        assert_eq!(g.num("=SHEET()"), 1.0);
        assert_eq!(g.num("=SHEET(Sheet1!A1)"), 1.0);
        assert_eq!(g.num("=SHEET(\"Sheet1\")"), 1.0);
        assert_eq!(g.error("=SHEET(\"NoSuch\")"), CalcError::Ref);
        assert_eq!(g.error("=SHEET(NoSuchSheet!A1)"), CalcError::Ref);
    }

    #[test]
    fn cell_address_row_col_and_contents() {
        let g = Grid::empty()
            .set_num("A1", 42.0)
            .set_text("B1", "hi")
            .set_num("A2", 7.0);
        assert_eq!(
            g.at("C5", "=CELL(\"row\")"),
            Outcome::Value(CalcValue::Number(5.0))
        );
        assert_eq!(
            g.at("C5", "=CELL(\"col\")"),
            Outcome::Value(CalcValue::Number(3.0))
        );
        assert_eq!(
            g.at("C5", "=CELL(\"address\")"),
            Outcome::Value(CalcValue::text("$C$5"))
        );
        assert_eq!(g.text("=CELL(\"address\", A1:B2)"), "$A$1");
        assert_eq!(g.text("=CELL(\"address\", Sheet1!A2)"), "$A$2");
        assert_eq!(g.num("=CELL(\"contents\", A1)"), 42.0);
        assert_eq!(g.text("=CELL(\"contents\", B1)"), "hi");
        assert_eq!(g.num("=CELL(\"contents\", Z99)"), 0.0);
    }

    #[test]
    fn cell_unsupported_types_are_value() {
        let g = Grid::empty();
        assert_eq!(g.error("=CELL(\"format\")"), CalcError::Value);
        assert_eq!(g.error("=CELL(\"width\")"), CalcError::Value);
        assert_eq!(g.error("=CELL(\"type\")"), CalcError::Value);
        assert_eq!(g.error("=CELL(\"filename\")"), CalcError::Value);
        assert_eq!(g.error("=CELL(\"bogus\")"), CalcError::Value);
    }

    #[test]
    fn info_system_reports_the_platform() {
        let expected = match std::env::consts::OS {
            "windows" => "Windows",
            "macos" => "Macintosh",
            _ => return, // not an Excel platform; #N/A is the honest answer
        };
        assert_eq!(Grid::empty().text("=INFO(\"system\")"), expected);
    }

    #[test]
    fn info_unsupported_types_are_na() {
        let g = Grid::empty();
        assert_eq!(g.error("=INFO(\"release\")"), CalcError::Na);
        assert_eq!(g.error("=INFO(\"directory\")"), CalcError::Na);
        assert_eq!(g.error("=INFO(\"numfile\")"), CalcError::Na);
        assert_eq!(g.error("=INFO(\"osversion\")"), CalcError::Na);
        assert_eq!(g.error("=INFO(\"bogus\")"), CalcError::Na);
    }
}
