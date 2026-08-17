// calc/eval.rs — the formula interpreter (wave 3): a tree-walking evaluator
// over `ast::Expr`. Pure and side-effect free: the grid is read only through
// `FuncCtx`/`CellResolver`, so this same interpreter serves the read path, the
// write path, and the overlay.
//
// Guardrails, in order of importance:
//   * Errors are never guessed. A node the resolver cannot combine (`Colon`,
//     `Union`, `Lambda`, `LambdaParam`) returns `#REF!` rather than a number.
//   * Arrays broadcast Excel-style for binary/unary operators; out-of-range
//     cells become `#N/A`, and the result is capped at 4M cells (`#VALUE!`).
//   * Recursion is depth-bounded (256) so pathological formulas error instead
//     of blowing the stack. No input-derived data is ever `unwrap`-ed.

use crate::turbo::calc::ast::{BinaryOp, Expr, SuffixOp, UnaryOp};
use crate::turbo::calc::coerce::{coerce_number, coerce_text, compare, compare_eq};
use crate::turbo::calc::functions::{FuncArg, FuncCtx, registry};
use crate::turbo::calc::value::{ArrayValue, CalcError, CalcValue};
use std::cmp::Ordering;
use std::sync::Arc;

/// Maximum nesting depth before evaluation gives up with `#VALUE!`.
const MAX_DEPTH: usize = 256;

/// Broadcast/shape cap: results larger than this are `#VALUE!`, never allocated.
const MAX_CELLS: u64 = 4_000_000;

/// Evaluate one formula AST node against `ctx`. See the module docs for the
/// full semantics; this is the only public entry point.
pub fn eval(expr: &Expr, ctx: &FuncCtx) -> Result<CalcValue, CalcError> {
    eval_depth(expr, ctx, 0)
}

fn eval_depth(expr: &Expr, ctx: &FuncCtx, depth: usize) -> Result<CalcValue, CalcError> {
    if depth > MAX_DEPTH {
        return Err(CalcError::Value);
    }
    match expr {
        Expr::Value(v) => Ok(v.clone()),
        Expr::Null => Ok(CalcValue::Blank),
        Expr::Ref(r) => ctx.resolve(r),
        Expr::Unary(op, inner) => {
            let v = eval_depth(inner, ctx, depth + 1)?;
            apply_unary(*op, v, ctx)
        }
        Expr::Suffix(SuffixOp::Percent, inner) => {
            let v = eval_depth(inner, ctx, depth + 1)?;
            percent(v)
        }
        Expr::Binary(op, l, r) => {
            let lv = eval_depth(l, ctx, depth + 1)?;
            let rv = eval_depth(r, ctx, depth + 1)?;
            apply_binary(*op, lv, rv)
        }
        Expr::Formula(children) => {
            // A formula node with more than one child has no single value
            // (spec §4); exactly one child evaluates through.
            if children.len() == 1 {
                eval_depth(&children[0], ctx, depth + 1)
            } else {
                Err(CalcError::Value)
            }
        }
        Expr::Function { name, args } => call_function(name, args, ctx, depth),
        Expr::Colon(_, _) | Expr::Union(_) | Expr::Lambda { .. } | Expr::LambdaParam(_) => {
            Err(CalcError::Ref)
        }
    }
}

// -- binary operators --------------------------------------------------------

fn apply_binary(op: BinaryOp, l: CalcValue, r: CalcValue) -> Result<CalcValue, CalcError> {
    // Error operands propagate immediately, left side first.
    if let CalcValue::Error(e) = l {
        return Err(e);
    }
    if let CalcValue::Error(e) = r {
        return Err(e);
    }
    if l.is_array() || r.is_array() {
        broadcast_binary(op, l, r)
    } else {
        scalar_binary(op, l, r)
    }
}

fn scalar_binary(op: BinaryOp, l: CalcValue, r: CalcValue) -> Result<CalcValue, CalcError> {
    match op {
        BinaryOp::Add => numeric_binary(l, r, |a, b| a + b),
        BinaryOp::Sub => numeric_binary(l, r, |a, b| a - b),
        BinaryOp::Mul => numeric_binary(l, r, |a, b| a * b),
        BinaryOp::Div => {
            let a = coerce_number(&l)?;
            let b = coerce_number(&r)?;
            if b == 0.0 {
                return Err(CalcError::Div0);
            }
            finite_result(a / b)
        }
        BinaryOp::Pow => {
            let a = coerce_number(&l)?;
            let b = coerce_number(&r)?;
            finite_result(a.powf(b))
        }
        BinaryOp::Concat => {
            let a = coerce_text(&l)?;
            let b = coerce_text(&r)?;
            Ok(CalcValue::text(a + &b))
        }
        BinaryOp::Eq => compare_eq(&l, &r, false).map(CalcValue::Bool),
        BinaryOp::Ne => compare_eq(&l, &r, false).map(|b| CalcValue::Bool(!b)),
        BinaryOp::Gt => compare(&l, &r).map(|o| CalcValue::Bool(o == Ordering::Greater)),
        BinaryOp::Ge => compare(&l, &r).map(|o| CalcValue::Bool(o != Ordering::Less)),
        BinaryOp::Lt => compare(&l, &r).map(|o| CalcValue::Bool(o == Ordering::Less)),
        BinaryOp::Le => compare(&l, &r).map(|o| CalcValue::Bool(o != Ordering::Greater)),
    }
}

fn numeric_binary(
    l: CalcValue,
    r: CalcValue,
    f: impl Fn(f64, f64) -> f64,
) -> Result<CalcValue, CalcError> {
    let a = coerce_number(&l)?;
    let b = coerce_number(&r)?;
    finite_result(f(a, b))
}

fn finite_result(n: f64) -> Result<CalcValue, CalcError> {
    if n.is_finite() {
        Ok(CalcValue::Number(n))
    } else {
        Err(CalcError::Num)
    }
}

// -- array broadcasting ------------------------------------------------------

fn broadcast_binary(op: BinaryOp, l: CalcValue, r: CalcValue) -> Result<CalcValue, CalcError> {
    let a = match l {
        CalcValue::Array(a) => a,
        other => Arc::new(ArrayValue::new(1, 1, vec![other])),
    };
    let b = match r {
        CalcValue::Array(a) => a,
        other => Arc::new(ArrayValue::new(1, 1, vec![other])),
    };
    let rows = a.rows.max(b.rows);
    let cols = a.cols.max(b.cols);
    if rows as u64 * cols as u64 > MAX_CELLS {
        return Err(CalcError::Value);
    }
    let mut data = Vec::with_capacity((rows * cols) as usize);
    for i in 0..rows {
        for j in 0..cols {
            let elem = match (
                broadcast_index(&a, rows, cols, i, j),
                broadcast_index(&b, rows, cols, i, j),
            ) {
                (Some(x), Some(y)) => match scalar_binary(op, x, y) {
                    Ok(v) => v,
                    Err(e) => CalcValue::Error(e),
                },
                _ => CalcValue::Error(CalcError::Na),
            };
            data.push(elem);
        }
    }
    Ok(CalcValue::array(ArrayValue::new(rows, cols, data)))
}

/// Pick the operand cell for result cell `(i, j)` of a broadcast to `rows x
/// cols`. A dimension of 1 broadcasts across the whole axis; a dimension equal
/// to the target pairs element-wise; anything else is out of range → `None`.
fn broadcast_index(a: &ArrayValue, rows: u32, cols: u32, i: u32, j: u32) -> Option<CalcValue> {
    let ri = if a.rows == 1 {
        0
    } else if a.rows == rows {
        i
    } else {
        return None;
    };
    let ci = if a.cols == 1 {
        0
    } else if a.cols == cols {
        j
    } else {
        return None;
    };
    Some(a.get(ri, ci).clone())
}

// -- unary operators and suffix ---------------------------------------------

fn apply_unary(op: UnaryOp, v: CalcValue, ctx: &FuncCtx) -> Result<CalcValue, CalcError> {
    match op {
        UnaryOp::ImplicitIntersect => intersect_or_pass(v, ctx),
        UnaryOp::Plus | UnaryOp::Minus => {
            let negate = op == UnaryOp::Minus;
            match v {
                CalcValue::Array(a) => {
                    let rows = a.rows;
                    let cols = a.cols;
                    if rows as u64 * cols as u64 > MAX_CELLS {
                        return Err(CalcError::Value);
                    }
                    let mut data = Vec::with_capacity((rows * cols) as usize);
                    for e in a.iter() {
                        match coerce_number(e) {
                            Ok(n) => data.push(CalcValue::Number(if negate { -n } else { n })),
                            Err(err) => data.push(CalcValue::Error(err)),
                        }
                    }
                    Ok(CalcValue::array(ArrayValue::new(rows, cols, data)))
                }
                other => {
                    coerce_number(&other).map(|n| CalcValue::Number(if negate { -n } else { n }))
                }
            }
        }
    }
}

fn intersect_or_pass(v: CalcValue, ctx: &FuncCtx) -> Result<CalcValue, CalcError> {
    match v {
        CalcValue::Array(a) => implicit_intersect(&a, ctx.row, ctx.col),
        other => Ok(other),
    }
}

/// The single value on the formula's own row or column, per Excel implicit
/// intersection. A row vector picks the formula column, a column vector the
/// formula row, and a rectangle the (row, col) cell; no such cell → `#VALUE!`.
fn implicit_intersect(a: &ArrayValue, row: u32, col: u32) -> Result<CalcValue, CalcError> {
    let (r, c) = match (a.rows, a.cols) {
        (1, 1) => (0, 0),
        (1, _) => (0, col),
        (_, 1) => (row, 0),
        _ => (row, col),
    };
    if r < a.rows && c < a.cols {
        Ok(a.get(r, c).clone())
    } else {
        Err(CalcError::Value)
    }
}

fn percent(v: CalcValue) -> Result<CalcValue, CalcError> {
    Ok(CalcValue::Number(coerce_number(&v)? / 100.0))
}

// -- function calls ----------------------------------------------------------

fn call_function(
    name: &str,
    args: &[Expr],
    ctx: &FuncCtx,
    depth: usize,
) -> Result<CalcValue, CalcError> {
    let spec = registry().get(name).ok_or(CalcError::Name)?;
    spec.validate(args.len())?;
    let mut cargs: Vec<FuncArg> = Vec::with_capacity(args.len());
    let lazy_args = [
        "LAMBDA",
        "LET",
        "MAP",
        "REDUCE",
        "SCAN",
        "BYROW",
        "BYCOL",
        "MAKEARRAY",
        "ISOMITTED",
    ]
    .iter()
    .any(|candidate| name.eq_ignore_ascii_case(candidate));
    for arg in args {
        if lazy_args {
            cargs.push(FuncArg::Expr(Box::new(arg.clone())));
            continue;
        }
        match arg {
            // A bare reference/range stays unevaluated so the function can
            // consume it raw (or surface the reference error itself).
            Expr::Ref(r) => cargs.push(FuncArg::Reference(r.clone())),
            // Everything else is evaluated eagerly; an error is passed through
            // as a value so IFERROR / IFNA / ISERROR can inspect it.
            _ => {
                let v = eval_depth(arg, ctx, depth + 1).unwrap_or_else(CalcValue::Error);
                cargs.push(FuncArg::Value(v));
            }
        }
    }
    if !spec.array_aware {
        // Scalarize arrays by implicit intersection before a non-array-aware
        // function sees them; failure to intersect is #VALUE!.
        for arg in cargs.iter_mut() {
            if let FuncArg::Value(v) = arg {
                if v.is_array() {
                    *v = scalarize_value(v.clone(), ctx)?;
                }
            }
        }
    }
    (spec.func)(ctx, &cargs)
}

fn scalarize_value(v: CalcValue, ctx: &FuncCtx) -> Result<CalcValue, CalcError> {
    match v {
        CalcValue::Array(a) => implicit_intersect(&a, ctx.row, ctx.col),
        other => Ok(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turbo::calc::functions::CellResolver;
    use crate::turbo::calc::parse_formula;
    use pretty_assertions::assert_eq;

    /// Fixed 3x3 grid on sheet 0:
    ///   A1=1 B1=2 C1=3
    ///   A2=4 B2=5 C2=6
    ///   A3=7 B3=8 C3=9
    /// Anything outside is blank; only sheet "S1" resolves.
    struct Grid;

    impl CellResolver for Grid {
        fn cell(&self, sheet: u32, row: u32, col: u32) -> Option<CalcValue> {
            if sheet == 0 && row < 3 && col < 3 {
                Some(CalcValue::Number((row * 3 + col + 1) as f64))
            } else {
                None
            }
        }

        fn sheet_index(&self, name: &str) -> Option<u32> {
            match name {
                "S1" => Some(0),
                "S2" => Some(1),
                _ => None,
            }
        }
    }

    fn ctx<'a>(resolver: &'a dyn CellResolver) -> FuncCtx<'a> {
        FuncCtx {
            date1904: false,
            sheet: 0,
            row: 0,
            col: 0,
            resolver,
        }
    }

    fn eval_text(s: &str) -> Result<CalcValue, CalcError> {
        let expr = parse_formula(s).unwrap_or_else(|e| panic!("parse {s:?} failed: {e:?}"));
        let grid = Grid;
        let c = ctx(&grid);
        eval(&expr, &c)
    }

    fn num(n: f64) -> CalcValue {
        CalcValue::Number(n)
    }

    fn err(e: CalcError) -> CalcValue {
        CalcValue::Error(e)
    }

    fn arr(rows: u32, cols: u32, data: Vec<CalcValue>) -> CalcValue {
        CalcValue::array(ArrayValue::new(rows, cols, data))
    }

    #[test]
    fn arithmetic_precedence_results() {
        assert_eq!(eval_text("=1+2*3"), Ok(num(7.0)));
        assert_eq!(eval_text("=(1+2)*3"), Ok(num(9.0)));
        assert_eq!(eval_text("=2^3^2"), Ok(num(64.0)));
        assert_eq!(eval_text("=-2^2"), Ok(num(4.0)));
        assert_eq!(eval_text("=10-2*3"), Ok(num(4.0)));
        // `&` binds looser than `+`: (2+3)&4 = "54"
        assert_eq!(eval_text("=2+3&4"), Ok(CalcValue::text("54")));
    }

    #[test]
    fn division_by_zero() {
        assert_eq!(eval_text("=1/0"), Err(CalcError::Div0));
        assert_eq!(eval_text("=1/(2-2)"), Err(CalcError::Div0));
        assert_eq!(eval_text("=A1/0"), Err(CalcError::Div0));
        // non-finite powers are #NUM!, not an overflow
        assert_eq!(eval_text("=10^400"), Err(CalcError::Num));
    }

    #[test]
    fn string_concat() {
        assert_eq!(eval_text("=\"a\"&\"b\""), Ok(CalcValue::text("ab")));
        assert_eq!(eval_text("=1&2"), Ok(CalcValue::text("12")));
        assert_eq!(eval_text("=A1&B1"), Ok(CalcValue::text("12")));
        assert_eq!(eval_text("=TRUE&\"\""), Ok(CalcValue::text("TRUE")));
    }

    #[test]
    fn comparisons_across_types() {
        assert_eq!(eval_text("=1<2"), Ok(CalcValue::Bool(true)));
        assert_eq!(eval_text("=1=\"1\""), Ok(CalcValue::Bool(false)));
        assert_eq!(eval_text("=1<\"a\""), Ok(CalcValue::Bool(true)));
        assert_eq!(eval_text("=TRUE>1"), Ok(CalcValue::Bool(true)));
        assert_eq!(eval_text("=1>=1"), Ok(CalcValue::Bool(true)));
        assert_eq!(eval_text("=1<>2"), Ok(CalcValue::Bool(true)));
        assert_eq!(eval_text("=\"a\"=\"A\""), Ok(CalcValue::Bool(true)));
        assert_eq!(eval_text("=A1<B1"), Ok(CalcValue::Bool(true)));
    }

    #[test]
    fn percent_and_unary_minus() {
        assert_eq!(eval_text("=50%"), Ok(num(0.5)));
        assert_eq!(eval_text("=50%%"), Ok(num(0.005)));
        assert_eq!(eval_text("=A1%"), Ok(num(0.01)));
        assert_eq!(eval_text("=-5"), Ok(num(-5.0)));
        assert_eq!(eval_text("=+5"), Ok(num(5.0)));
        assert_eq!(eval_text("=-(1+2)"), Ok(num(-3.0)));
    }

    #[test]
    fn error_operands_propagate_through_operators() {
        assert_eq!(eval_text("=1/0+5"), Err(CalcError::Div0));
        assert_eq!(eval_text("=5+1/0"), Err(CalcError::Div0));
        assert_eq!(eval_text("=#N/A*2"), Err(CalcError::Na));
        assert_eq!(eval_text("=2^#VALUE!"), Err(CalcError::Value));
        assert_eq!(eval_text("=1+\"abc\""), Err(CalcError::Value));
        assert_eq!(eval_text("=\"abc\"-1"), Err(CalcError::Value));
        // a bare error literal evaluates to the error *value*, not an Err
        assert_eq!(eval_text("=#DIV/0!"), Ok(err(CalcError::Div0)));
    }

    #[test]
    fn unknown_function_is_name_error() {
        assert_eq!(eval_text("=NXKFN12345(1)"), Err(CalcError::Name));
        assert_eq!(eval_text("=BOGUSFUNC()"), Err(CalcError::Name));
    }

    #[test]
    fn sum_over_range_reference() {
        assert_eq!(eval_text("=SUM(A1:B2)"), Ok(num(12.0)));
        assert_eq!(eval_text("=SUM(A1:C3)"), Ok(num(45.0)));
        assert_eq!(eval_text("=SUM(A1,B2,C3)"), Ok(num(15.0)));
        assert_eq!(eval_text("=SUM()"), Ok(num(0.0)));
    }

    #[test]
    fn if_chooses_a_branch() {
        assert_eq!(
            eval_text("=IF(1>0,\"yes\",\"no\")"),
            Ok(CalcValue::text("yes"))
        );
        assert_eq!(
            eval_text("=IF(1<0,\"yes\",\"no\")"),
            Ok(CalcValue::text("no"))
        );
        assert_eq!(eval_text("=IF(TRUE,42,0)"), Ok(num(42.0)));
        assert_eq!(
            eval_text("=IF(0,\"yes\",\"no\")"),
            Ok(CalcValue::text("no"))
        );
    }

    #[test]
    fn error_values_reach_error_handling_functions() {
        assert_eq!(
            eval_text("=IFERROR(1/0,\"err\")"),
            Ok(CalcValue::text("err"))
        );
        assert_eq!(eval_text("=IFERROR(2*3,0)"), Ok(num(6.0)));
        assert_eq!(eval_text("=IFNA(#N/A,7)"), Ok(num(7.0)));
        // IFNA only catches #N/A; other errors pass through untouched.
        assert_eq!(eval_text("=IFNA(#DIV/0!,7)"), Ok(err(CalcError::Div0)));
    }

    #[test]
    fn array_literal_arithmetic_broadcasts() {
        assert_eq!(
            eval_text("={1,2}+{3;4}"),
            Ok(arr(2, 2, vec![num(4.0), num(5.0), num(5.0), num(6.0)]))
        );
        assert_eq!(
            eval_text("={1,2,3}*2"),
            Ok(arr(1, 3, vec![num(2.0), num(4.0), num(6.0)]))
        );
        assert_eq!(
            eval_text("=2*{1,2,3}"),
            Ok(arr(1, 3, vec![num(2.0), num(4.0), num(6.0)]))
        );
        assert_eq!(
            eval_text("={1;2}+{3;4}"),
            Ok(arr(2, 1, vec![num(4.0), num(6.0)]))
        );
        assert_eq!(
            eval_text("={1,2}-{3,4}"),
            Ok(arr(1, 2, vec![num(-2.0), num(-2.0)]))
        );
        // shapes that cannot pair element-wise yield the #N/A error per cell
        assert_eq!(
            eval_text("={1,2}+{3,4,5}"),
            Ok(arr(
                1,
                3,
                vec![err(CalcError::Na), err(CalcError::Na), err(CalcError::Na)]
            ))
        );
    }

    #[test]
    fn implicit_intersection_uses_own_row_and_column() {
        assert_eq!(eval_text("=@A1:A3"), Ok(num(1.0)));
        assert_eq!(eval_text("=@A1:C1"), Ok(num(1.0)));
        // `@` forces the intersection on a rectangle: cell (row 0, col 0) → A1.
        assert_eq!(eval_text("=@A1:B2"), Ok(num(1.0)));
    }

    #[test]
    fn non_array_aware_function_on_a_range_value() {
        // A bare range argument is passed unevaluated as a `Reference` (SUM
        // needs the raw range), so the loop never scalarizes it. ABS resolves
        // it to a dense array and coerce_number rejects arrays → #VALUE!.
        assert_eq!(eval_text("=ABS(A1:B2)"), Err(CalcError::Value));
        // Scalarize the range first with `@` and ABS is happy again.
        assert_eq!(eval_text("=ABS(@A1:B2)"), Ok(num(1.0)));
    }

    #[test]
    fn implicit_intersection_out_of_range_is_value_error() {
        let grid = Grid;
        let c = FuncCtx {
            date1904: false,
            sheet: 0,
            row: 5,
            col: 5,
            resolver: &grid,
        };
        assert_eq!(
            eval(&parse_formula("=@A1:A3").unwrap(), &c),
            Err(CalcError::Value)
        );
        assert_eq!(
            eval(&parse_formula("=ABS(A1:C1)").unwrap(), &c),
            Err(CalcError::Value)
        );
    }

    #[test]
    fn blank_cells_coerce_in_arithmetic_and_concat() {
        assert_eq!(eval_text("=A4+1"), Ok(num(1.0)));
        assert_eq!(eval_text("=B4&\"x\""), Ok(CalcValue::text("x")));
    }

    #[test]
    fn colon_union_and_lambda_refuse_to_guess() {
        let grid = Grid;
        let c = ctx(&grid);
        assert_eq!(
            eval(&parse_formula("=my_name:other_name").unwrap(), &c),
            Err(CalcError::Ref)
        );
        assert_eq!(
            eval(&parse_formula("=(A1,B2)").unwrap(), &c),
            Err(CalcError::Ref)
        );
        let lam = Expr::Lambda {
            params: vec!["x".into()],
            body: Box::new(Expr::LambdaParam(0)),
        };
        assert_eq!(eval(&lam, &c), Err(CalcError::Ref));
        let union = Expr::Union(vec![
            parse_formula("A1").unwrap(),
            parse_formula("B2").unwrap(),
        ]);
        assert_eq!(eval(&union, &c), Err(CalcError::Ref));
    }

    #[test]
    fn formula_root_rules() {
        let grid = Grid;
        let c = ctx(&grid);
        let multi = Expr::Formula(vec![
            parse_formula("1").unwrap(),
            parse_formula("2").unwrap(),
        ]);
        assert_eq!(eval(&multi, &c), Err(CalcError::Value));
        let single = Expr::Formula(vec![parse_formula("1+2").unwrap()]);
        assert_eq!(eval(&single, &c), Ok(num(3.0)));
    }

    #[test]
    fn recursion_depth_is_bounded() {
        let grid = Grid;
        let c = ctx(&grid);
        let mut shallow = parse_formula("1").unwrap();
        let mut deep = parse_formula("1").unwrap();
        for _ in 0..10 {
            shallow = Expr::Binary(
                BinaryOp::Add,
                Box::new(shallow),
                Box::new(parse_formula("1").unwrap()),
            );
        }
        for _ in 0..300 {
            deep = Expr::Binary(
                BinaryOp::Add,
                Box::new(deep),
                Box::new(parse_formula("1").unwrap()),
            );
        }
        assert_eq!(eval(&shallow, &c), Ok(num(11.0)));
        assert_eq!(eval(&deep, &c), Err(CalcError::Value));
    }
}
