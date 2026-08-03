// functions/math.rs — the math function family. Owned exclusively by the math
// family agent; no other agent edits this file.
//
// Registry contract: implement `register` below and keep this exact signature.
// Do NOT edit functions/mod.rs — the `mod math;` declaration and the
// `math::register(&mut r)` call site in `build()` are already final.
// See functions/mod.rs for the worked ABS template.
use super::{FuncArg, FuncCtx, FuncSpec, Registry};
use crate::turbo::calc::coerce::{coerce_number, coerce_text, compare, compare_eq};
use crate::turbo::calc::value::{ArrayValue, CalcError, CalcValue};
use std::cmp::Ordering;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

fn ok_num(n: f64) -> Result<CalcValue, CalcError> {
    if n.is_finite() {
        Ok(CalcValue::Number(n))
    } else {
        Err(CalcError::Num)
    }
}

fn sum(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let mut total = 0.0;
    for arg in args {
        match arg.value(ctx)? {
            CalcValue::Array(a) => {
                for v in a.iter() {
                    match v {
                        CalcValue::Number(n) => total += n,
                        CalcValue::Error(e) => return Err(*e),
                        _ => {}
                    }
                }
            }
            v => total += coerce_number(&v)?,
        }
    }
    ok_num(total)
}

fn product(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let mut prod = 1.0;
    let mut any = false;
    for arg in args {
        match arg.value(ctx)? {
            CalcValue::Array(a) => {
                for v in a.iter() {
                    match v {
                        CalcValue::Number(n) => {
                            prod *= n;
                            any = true;
                        }
                        CalcValue::Error(e) => return Err(*e),
                        _ => {}
                    }
                }
            }
            v => {
                prod *= coerce_number(&v)?;
                any = true;
            }
        }
    }
    if any {
        ok_num(prod)
    } else {
        Ok(CalcValue::Number(0.0))
    }
}

fn average(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let mut total = 0.0;
    let mut count = 0usize;
    for arg in args {
        match arg.value(ctx)? {
            CalcValue::Array(a) => {
                for v in a.iter() {
                    match v {
                        CalcValue::Number(n) => {
                            total += n;
                            count += 1;
                        }
                        CalcValue::Error(e) => return Err(*e),
                        _ => {}
                    }
                }
            }
            v => {
                total += coerce_number(&v)?;
                count += 1;
            }
        }
    }
    if count == 0 {
        Err(CalcError::Div0)
    } else {
        ok_num(total / count as f64)
    }
}

fn count(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let mut n = 0usize;
    for arg in args {
        match arg.value(ctx)? {
            CalcValue::Array(a) => {
                for v in a.iter() {
                    match v {
                        CalcValue::Number(_) => n += 1,
                        CalcValue::Error(e) => return Err(*e),
                        _ => {}
                    }
                }
            }
            v => {
                coerce_number(&v)?;
                n += 1;
            }
        }
    }
    Ok(CalcValue::Number(n as f64))
}

fn counta(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let mut n = 0usize;
    for arg in args {
        match arg.value(ctx)? {
            CalcValue::Array(a) => {
                for v in a.iter() {
                    if !v.is_blank() {
                        n += 1;
                    }
                }
            }
            v => {
                if !v.is_blank() {
                    n += 1;
                }
            }
        }
    }
    Ok(CalcValue::Number(n as f64))
}

fn min(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let mut best: Option<f64> = None;
    for arg in args {
        match arg.value(ctx)? {
            CalcValue::Array(a) => {
                for v in a.iter() {
                    match v {
                        CalcValue::Number(n) => best = Some(best.map_or(*n, |b| b.min(*n))),
                        CalcValue::Error(e) => return Err(*e),
                        _ => {}
                    }
                }
            }
            v => {
                let n = coerce_number(&v)?;
                best = Some(best.map_or(n, |b| b.min(n)));
            }
        }
    }
    Ok(CalcValue::Number(best.unwrap_or(0.0)))
}

fn max(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let mut best: Option<f64> = None;
    for arg in args {
        match arg.value(ctx)? {
            CalcValue::Array(a) => {
                for v in a.iter() {
                    match v {
                        CalcValue::Number(n) => best = Some(best.map_or(*n, |b| b.max(*n))),
                        CalcValue::Error(e) => return Err(*e),
                        _ => {}
                    }
                }
            }
            v => {
                let n = coerce_number(&v)?;
                best = Some(best.map_or(n, |b| b.max(n)));
            }
        }
    }
    Ok(CalcValue::Number(best.unwrap_or(0.0)))
}

fn abs(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    ok_num(n.abs())
}

fn int(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    ok_num(n.floor())
}

fn mod_(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    let d = coerce_number(&args[1].value(ctx)?)?;
    if d == 0.0 {
        return Err(CalcError::Div0);
    }
    let mut r = n % d;
    if r != 0.0 && (r < 0.0) != (d < 0.0) {
        r += d;
    }
    ok_num(r)
}

fn power(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let b = coerce_number(&args[0].value(ctx)?)?;
    let e = coerce_number(&args[1].value(ctx)?)?;
    if b == 0.0 && e < 0.0 {
        return Err(CalcError::Div0);
    }
    if b < 0.0 && e.fract() != 0.0 {
        return Err(CalcError::Num);
    }
    ok_num(b.powf(e))
}

fn sqrt(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    if n < 0.0 {
        return Err(CalcError::Num);
    }
    ok_num(n.sqrt())
}

enum RoundMode {
    HalfAway,
    AwayFromZero,
    TowardZero,
}

fn apply_mode(scaled: f64, mode: RoundMode) -> f64 {
    match mode {
        RoundMode::HalfAway => scaled.round(),
        RoundMode::AwayFromZero => {
            if scaled < 0.0 {
                scaled.floor()
            } else {
                scaled.ceil()
            }
        }
        RoundMode::TowardZero => {
            if scaled < 0.0 {
                scaled.ceil()
            } else {
                scaled.floor()
            }
        }
    }
}

fn round_by(n: f64, digits: i32, mode: RoundMode) -> f64 {
    if digits >= 0 {
        let factor = 10f64.powi(digits);
        if !factor.is_finite() {
            return n;
        }
        let scaled = n * factor;
        if !scaled.is_finite() {
            return n;
        }
        apply_mode(scaled, mode) / factor
    } else {
        let factor = 10f64.powi(-digits);
        if factor.is_infinite() {
            return 0.0;
        }
        let scaled = n / factor;
        apply_mode(scaled, mode) * factor
    }
}

fn round(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    let digits = coerce_number(&args[1].value(ctx)?)?.trunc() as i32;
    ok_num(round_by(n, digits, RoundMode::HalfAway))
}

fn roundup(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    let digits = coerce_number(&args[1].value(ctx)?)?.trunc() as i32;
    ok_num(round_by(n, digits, RoundMode::AwayFromZero))
}

fn rounddown(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    let digits = coerce_number(&args[1].value(ctx)?)?.trunc() as i32;
    ok_num(round_by(n, digits, RoundMode::TowardZero))
}

#[derive(Clone, Copy, Debug)]
enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

struct Criterion {
    op: Op,
    operand: CalcValue,
}

fn parse_criteria(v: &CalcValue) -> Result<Criterion, CalcError> {
    let s = coerce_text(v)?;
    let s = s.trim();
    let (op, rest) = if let Some(r) = s.strip_prefix("<=") {
        (Op::Le, r.trim())
    } else if let Some(r) = s.strip_prefix(">=") {
        (Op::Ge, r.trim())
    } else if let Some(r) = s.strip_prefix("<>") {
        (Op::Ne, r.trim())
    } else if let Some(r) = s.strip_prefix('<') {
        (Op::Lt, r.trim())
    } else if let Some(r) = s.strip_prefix('>') {
        (Op::Gt, r.trim())
    } else if let Some(r) = s.strip_prefix('=') {
        (Op::Eq, r.trim())
    } else {
        (Op::Eq, s)
    };
    let operand = match rest.parse::<f64>() {
        Ok(n) if n.is_finite() => CalcValue::Number(n),
        _ => CalcValue::text(rest),
    };
    Ok(Criterion { op, operand })
}

fn criteria_match(v: &CalcValue, crit: &Criterion) -> Result<bool, CalcError> {
    if let CalcValue::Error(e) = v {
        return Err(*e);
    }
    match &crit.operand {
        CalcValue::Text(t) if t.is_empty() => match crit.op {
            Op::Eq => return Ok(v.is_blank() || coerce_text(v)?.is_empty()),
            Op::Ne => return Ok(!v.is_blank() && !coerce_text(v)?.is_empty()),
            _ => {}
        },
        _ => {}
    }
    let wildcards = matches!(&crit.operand, CalcValue::Text(_));
    match crit.op {
        Op::Eq => compare_eq(v, &crit.operand, wildcards),
        Op::Ne => compare_eq(v, &crit.operand, wildcards).map(|eq| !eq),
        Op::Lt => compare(v, &crit.operand).map(|o| o == Ordering::Less),
        Op::Le => compare(v, &crit.operand).map(|o| o != Ordering::Greater),
        Op::Gt => compare(v, &crit.operand).map(|o| o == Ordering::Greater),
        Op::Ge => compare(v, &crit.operand).map(|o| o != Ordering::Less),
    }
}

fn range_array(ctx: &FuncCtx, arg: &FuncArg) -> Result<ArrayValue, CalcError> {
    match arg.value(ctx)? {
        CalcValue::Array(a) => Ok((*a).clone()),
        v => Ok(ArrayValue::new(1, 1, vec![v])),
    }
}

fn addable_number(v: &CalcValue) -> Result<Option<f64>, CalcError> {
    match v {
        CalcValue::Number(n) => Ok(Some(*n)),
        CalcValue::Text(t) => match t.trim().parse::<f64>() {
            Ok(n) if n.is_finite() => Ok(Some(n)),
            _ => Ok(None),
        },
        CalcValue::Error(e) => Err(*e),
        _ => Ok(None),
    }
}

fn sumif(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let range = range_array(ctx, &args[0])?;
    let crit = parse_criteria(&args[1].value(ctx)?)?;
    let sum_range = if args.len() == 3 {
        range_array(ctx, &args[2])?
    } else {
        range.clone()
    };
    if range.shape() != sum_range.shape() {
        return Err(CalcError::Value);
    }
    let mut total = 0.0;
    for (v, sv) in range.iter().zip(sum_range.iter()) {
        if criteria_match(v, &crit)? {
            if let Some(n) = addable_number(sv)? {
                total += n;
            }
        }
    }
    ok_num(total)
}

fn countif(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let range = range_array(ctx, &args[0])?;
    let crit = parse_criteria(&args[1].value(ctx)?)?;
    let mut n = 0usize;
    for v in range.iter() {
        if criteria_match(v, &crit)? {
            n += 1;
        }
    }
    Ok(CalcValue::Number(n as f64))
}

fn averageif(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let range = range_array(ctx, &args[0])?;
    let crit = parse_criteria(&args[1].value(ctx)?)?;
    let avg_range = if args.len() == 3 {
        range_array(ctx, &args[2])?
    } else {
        range.clone()
    };
    if range.shape() != avg_range.shape() {
        return Err(CalcError::Value);
    }
    let mut total = 0.0;
    let mut count = 0usize;
    for (v, av) in range.iter().zip(avg_range.iter()) {
        if criteria_match(v, &crit)? {
            if let Some(n) = addable_number(av)? {
                total += n;
                count += 1;
            }
        }
    }
    if count == 0 {
        Err(CalcError::Div0)
    } else {
        ok_num(total / count as f64)
    }
}

fn ifs_scan(
    ctx: &FuncCtx,
    args: &[FuncArg],
) -> Result<(ArrayValue, Vec<(ArrayValue, Criterion)>), CalcError> {
    let sum = range_array(ctx, &args[0])?;
    let n_pairs = args.len() - 1;
    if n_pairs % 2 != 0 {
        return Err(CalcError::Value);
    }
    let mut pairs = Vec::new();
    for i in (1..args.len()).step_by(2) {
        let cr = range_array(ctx, &args[i])?;
        if cr.shape() != sum.shape() {
            return Err(CalcError::Value);
        }
        let crit = parse_criteria(&args[i + 1].value(ctx)?)?;
        pairs.push((cr, crit));
    }
    Ok((sum, pairs))
}

fn sumifs(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let (sum, pairs) = ifs_scan(ctx, args)?;
    let len = sum.data.len();
    let mut total = 0.0;
    for i in 0..len {
        let mut matched = true;
        for (cr, crit) in &pairs {
            if !criteria_match(&cr.data[i], crit)? {
                matched = false;
                break;
            }
        }
        if matched {
            if let Some(n) = addable_number(&sum.data[i])? {
                total += n;
            }
        }
    }
    ok_num(total)
}

fn countifs(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    if args.len() % 2 != 0 {
        return Err(CalcError::Value);
    }
    let mut ranges = Vec::new();
    let mut crits = Vec::new();
    for i in (0..args.len()).step_by(2) {
        ranges.push(range_array(ctx, &args[i])?);
        crits.push(parse_criteria(&args[i + 1].value(ctx)?)?);
    }
    let shape = ranges[0].shape();
    for cr in &ranges {
        if cr.shape() != shape {
            return Err(CalcError::Value);
        }
    }
    let len = ranges[0].data.len();
    let mut n = 0usize;
    for i in 0..len {
        let mut matched = true;
        for (cr, crit) in ranges.iter().zip(&crits) {
            if !criteria_match(&cr.data[i], crit)? {
                matched = false;
                break;
            }
        }
        if matched {
            n += 1;
        }
    }
    Ok(CalcValue::Number(n as f64))
}

fn averageifs(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let (avg, pairs) = ifs_scan(ctx, args)?;
    let len = avg.data.len();
    let mut total = 0.0;
    let mut count = 0usize;
    for i in 0..len {
        let mut matched = true;
        for (cr, crit) in &pairs {
            if !criteria_match(&cr.data[i], crit)? {
                matched = false;
                break;
            }
        }
        if matched {
            if let Some(n) = addable_number(&avg.data[i])? {
                total += n;
                count += 1;
            }
        }
    }
    if count == 0 {
        Err(CalcError::Div0)
    } else {
        ok_num(total / count as f64)
    }
}

// -- group 6: trigonometry and logarithms ------------------------------------

/// Unary math over a coerced number, wrapped through `ok_num` so a non-finite
/// result is #NUM! rather than a NaN/Inf value.
macro_rules! unary_math {
    ($fname:ident, $op:expr) => {
        fn $fname(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
            let n = coerce_number(&args[0].value(ctx)?)?;
            ok_num($op(n))
        }
    };
}

unary_math!(exp, |n: f64| n.exp());
unary_math!(sin, |n: f64| n.sin());
unary_math!(cos, |n: f64| n.cos());
unary_math!(tan, |n: f64| n.tan());
unary_math!(sinh, |n: f64| n.sinh());
unary_math!(cosh, |n: f64| n.cosh());
unary_math!(tanh, |n: f64| n.tanh());
unary_math!(atan, |n: f64| n.atan());
unary_math!(degrees, |n: f64| n.to_degrees());
unary_math!(radians, |n: f64| n.to_radians());

fn pi(_ctx: &FuncCtx, _args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    Ok(CalcValue::Number(std::f64::consts::PI))
}

fn ln(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    if n <= 0.0 {
        return Err(CalcError::Num);
    }
    ok_num(n.ln())
}

fn log10(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    if n <= 0.0 {
        return Err(CalcError::Num);
    }
    ok_num(n.log10())
}

fn log(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    if n <= 0.0 {
        return Err(CalcError::Num);
    }
    let base = if args.len() == 2 {
        let b = coerce_number(&args[1].value(ctx)?)?;
        if b <= 0.0 {
            return Err(CalcError::Num);
        }
        if b == 1.0 {
            return Err(CalcError::Div0);
        }
        b
    } else {
        10.0
    };
    ok_num(n.ln() / base.ln())
}

fn asin(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    if !(-1.0..=1.0).contains(&n) {
        return Err(CalcError::Num);
    }
    ok_num(n.asin())
}

fn acos(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    if !(-1.0..=1.0).contains(&n) {
        return Err(CalcError::Num);
    }
    ok_num(n.acos())
}

fn atan2(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    // Excel's ATAN2(x_num, y_num) is the arctangent of y_num/x_num, so the
    // y argument leads.
    let x = coerce_number(&args[0].value(ctx)?)?;
    let y = coerce_number(&args[1].value(ctx)?)?;
    ok_num(y.atan2(x))
}

fn acot(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    let v = if n == 0.0 {
        std::f64::consts::FRAC_PI_2
    } else if n > 0.0 {
        (1.0 / n).atan()
    } else {
        (1.0 / n).atan() + std::f64::consts::PI
    };
    ok_num(v)
}

fn csc(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    let s = n.sin();
    if s == 0.0 {
        return Err(CalcError::Div0);
    }
    ok_num(1.0 / s)
}

fn sec(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    let c = n.cos();
    if c == 0.0 {
        return Err(CalcError::Div0);
    }
    ok_num(1.0 / c)
}

fn cot(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    let t = n.tan();
    if t == 0.0 {
        return Err(CalcError::Div0);
    }
    ok_num(1.0 / t)
}

fn csch(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    let s = n.sinh();
    if s == 0.0 {
        return Err(CalcError::Div0);
    }
    ok_num(1.0 / s)
}

fn sech(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    ok_num(1.0 / n.cosh())
}

fn coth(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    let t = n.tanh();
    if t == 0.0 {
        return Err(CalcError::Div0);
    }
    ok_num(1.0 / t)
}

/// asinh(x) = ln(x + sqrt(x^2 + 1)).
fn asinh(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    ok_num((n + (n * n + 1.0).sqrt()).ln())
}

/// acosh(x) = 2 ln(sqrt((x+1)/2) + sqrt((x-1)/2)); the split-square-root form
/// stays accurate for x close to 1 where (x^2 - 1) would cancel.
fn acosh(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    if n < 1.0 {
        return Err(CalcError::Num);
    }
    ok_num(2.0 * (((n + 1.0) / 2.0).sqrt() + ((n - 1.0) / 2.0).sqrt()).ln())
}

fn atanh(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    if !(-1.0..1.0).contains(&n) {
        return Err(CalcError::Num);
    }
    ok_num(0.5 * ((1.0 + n) / (1.0 - n)).ln())
}

/// acoth(x) = 0.5 ln((x+1)/(x-1)), defined for |x| > 1.
fn acoth(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    if n.abs() <= 1.0 {
        return Err(CalcError::Num);
    }
    ok_num(0.5 * ((n + 1.0) / (n - 1.0)).ln())
}

// -- group 7: rounding and sign ----------------------------------------------

/// Legacy CEILING: rounds away from zero to the nearest multiple of
/// `significance`; a positive number with negative significance is #NUM!. Zero
/// in either argument is 0 (Microsoft's documented behavior).
fn ceiling_legacy(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    let s = coerce_number(&args[1].value(ctx)?)?;
    if n == 0.0 || s == 0.0 {
        return Ok(CalcValue::Number(0.0));
    }
    if n > 0.0 && s < 0.0 {
        return Err(CalcError::Num);
    }
    let q = n / s;
    let r = if q >= 0.0 { q.ceil() } else { q.floor() };
    ok_num(s * r)
}

/// Legacy FLOOR. Modern Excel (2010+) rounds negative numbers with a positive
/// significance away from zero — FLOOR(-5.4,1) is -6 — and only errors with
/// #NUM! when the number is positive and the significance negative.
fn floor_legacy(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    let s = coerce_number(&args[1].value(ctx)?)?;
    if n == 0.0 {
        return Ok(CalcValue::Number(0.0));
    }
    if s == 0.0 {
        return Err(CalcError::Div0);
    }
    if n > 0.0 && s < 0.0 {
        return Err(CalcError::Num);
    }
    ok_num(s * (n / s).floor())
}

/// CEILING.MATH(number, [significance=1], [mode=0]). Default rounds up (toward
/// +inf) whatever the sign; a non-zero mode makes negative numbers round
/// toward zero. Significance 0 is 0.
fn ceiling_math(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    let s = if args.len() > 1 {
        coerce_number(&args[1].value(ctx)?)?
    } else {
        1.0
    };
    let mode = if args.len() > 2 {
        coerce_number(&args[2].value(ctx)?)?
    } else {
        0.0
    };
    if n == 0.0 || s == 0.0 {
        return Ok(CalcValue::Number(0.0));
    }
    let q = n / s;
    let r = if mode == 0.0 || n >= 0.0 {
        q.ceil()
    } else if q >= 0.0 {
        q.floor()
    } else {
        q.ceil()
    };
    ok_num(s * r)
}

/// CEILING.PRECISE(number, [significance=1]): rounds up toward +inf; the sign
/// of `significance` is irrelevant (Excel takes its absolute value).
fn ceiling_precise(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    let s = if args.len() > 1 {
        coerce_number(&args[1].value(ctx)?)?.abs()
    } else {
        1.0
    };
    if n == 0.0 || s == 0.0 {
        return Ok(CalcValue::Number(0.0));
    }
    ok_num(s * (n / s).ceil())
}

/// FLOOR.MATH(number, [significance=1], [mode=0]). Default rounds down (toward
/// -inf); a non-zero mode makes negative numbers round toward zero.
fn floor_math(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    let s = if args.len() > 1 {
        coerce_number(&args[1].value(ctx)?)?
    } else {
        1.0
    };
    let mode = if args.len() > 2 {
        coerce_number(&args[2].value(ctx)?)?
    } else {
        0.0
    };
    if n == 0.0 || s == 0.0 {
        return Ok(CalcValue::Number(0.0));
    }
    let q = n / s;
    let r = if mode == 0.0 || n >= 0.0 {
        q.floor()
    } else if q >= 0.0 {
        q.floor()
    } else {
        q.ceil()
    };
    ok_num(s * r)
}

/// FLOOR.PRECISE(number, [significance=1]): rounds down toward -inf; sign of
/// `significance` is irrelevant.
fn floor_precise(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    let s = if args.len() > 1 {
        coerce_number(&args[1].value(ctx)?)?.abs()
    } else {
        1.0
    };
    if n == 0.0 || s == 0.0 {
        return Ok(CalcValue::Number(0.0));
    }
    ok_num(s * (n / s).floor())
}

/// MROUND: rounds half away from zero to the nearest multiple; opposite-sign
/// arguments are #NUM! and a zero multiple is #DIV/0!.
fn mround(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    let m = coerce_number(&args[1].value(ctx)?)?;
    if n == 0.0 {
        return Ok(CalcValue::Number(0.0));
    }
    if m == 0.0 {
        return Err(CalcError::Div0);
    }
    if (n > 0.0) != (m > 0.0) {
        return Err(CalcError::Num);
    }
    ok_num((n / m).round() * m)
}

fn trunc(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    let digits = if args.len() > 1 {
        coerce_number(&args[1].value(ctx)?)?.trunc() as i32
    } else {
        0
    };
    ok_num(round_by(n, digits, RoundMode::TowardZero))
}

fn even(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    if n == 0.0 {
        return Ok(CalcValue::Number(0.0));
    }
    let v = if n > 0.0 {
        let c = n.ceil();
        if c % 2.0 == 0.0 { c } else { c + 1.0 }
    } else {
        let f = n.floor();
        if f % 2.0 == 0.0 { f } else { f - 1.0 }
    };
    ok_num(v)
}

fn odd(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    if n == 0.0 {
        return Ok(CalcValue::Number(1.0));
    }
    let v = if n > 0.0 {
        let c = n.ceil();
        if c % 2.0 == 1.0 { c } else { c + 1.0 }
    } else {
        let f = n.floor();
        if f % 2.0 == -1.0 { f } else { f - 1.0 }
    };
    ok_num(v)
}

fn sign(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    Ok(CalcValue::Number(if n > 0.0 {
        1.0
    } else if n < 0.0 {
        -1.0
    } else {
        0.0
    }))
}

fn quotient(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    let d = coerce_number(&args[1].value(ctx)?)?;
    if d == 0.0 {
        return Err(CalcError::Div0);
    }
    ok_num((n / d).trunc())
}

// -- group 8: combinatorics ---------------------------------------------------

fn fact(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?.trunc();
    if n < 0.0 || n > 170.0 {
        return Err(CalcError::Num);
    }
    let mut r = 1.0;
    let mut i = 2.0;
    while i <= n {
        r *= i;
        i += 1.0;
    }
    ok_num(r)
}

fn factdouble(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?.trunc();
    if n < 0.0 {
        return Err(CalcError::Num);
    }
    let mut r = 1.0;
    let mut i = if n % 2.0 == 0.0 { 2.0 } else { 1.0 };
    while i <= n {
        r *= i;
        i += 2.0;
    }
    ok_num(r)
}

/// Binomial coefficient; `k == 0` is 1 and `k > n` is 0 (used by COMBINA whose
/// first argument may legitimately sit below k).
fn comb(n: f64, k: f64) -> f64 {
    if k == 0.0 {
        return 1.0;
    }
    if k > n {
        return 0.0;
    }
    let k = k.min(n - k);
    let mut r = 1.0;
    let mut i = 0.0;
    while i < k {
        r *= (n - i) / (k - i);
        i += 1.0;
    }
    r
}

fn combin(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?.trunc();
    let k = coerce_number(&args[1].value(ctx)?)?.trunc();
    if n < 0.0 || k < 0.0 || k > n {
        return Err(CalcError::Num);
    }
    ok_num(comb(n, k))
}

fn combina(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?.trunc();
    let k = coerce_number(&args[1].value(ctx)?)?.trunc();
    if n < 0.0 || k < 0.0 {
        return Err(CalcError::Num);
    }
    ok_num(comb(n + k - 1.0, k))
}

fn permut(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?.trunc();
    let k = coerce_number(&args[1].value(ctx)?)?.trunc();
    if n < 0.0 || k < 0.0 || k > n {
        return Err(CalcError::Num);
    }
    let mut r = 1.0;
    let mut i = 0;
    while (i as f64) < k {
        r *= n - i as f64;
        i += 1;
    }
    ok_num(r)
}

fn permutationa(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?.trunc();
    let k = coerce_number(&args[1].value(ctx)?)?.trunc();
    if n < 0.0 || k < 0.0 {
        return Err(CalcError::Num);
    }
    ok_num(n.powf(k))
}

fn multinomial(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let vals = flatten_scalars(ctx, args)?;
    let vals: Vec<f64> = vals.into_iter().map(|x| x.trunc()).collect();
    for &x in &vals {
        if x < 0.0 {
            return Err(CalcError::Num);
        }
    }
    let mut result = 1.0;
    let mut remaining = vals.iter().sum::<f64>();
    for &x in &vals {
        result *= comb(remaining, x);
        remaining -= x;
    }
    ok_num(result)
}

fn gcd(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let vals = flatten_scalars(ctx, args)?;
    let vals: Vec<f64> = vals.into_iter().map(|x| x.trunc()).collect();
    for &x in &vals {
        if x < 0.0 {
            return Err(CalcError::Num);
        }
    }
    let mut g = 0.0;
    for x in vals {
        if g == 0.0 {
            g = x.abs();
        } else {
            g = gcd_f(g, x.abs());
        }
    }
    ok_num(g)
}

fn lcm(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let vals = flatten_scalars(ctx, args)?;
    let vals: Vec<f64> = vals.into_iter().map(|x| x.trunc()).collect();
    for &x in &vals {
        if x < 0.0 {
            return Err(CalcError::Num);
        }
    }
    let mut l = 1.0;
    for x in vals {
        if x == 0.0 {
            return Ok(CalcValue::Number(0.0));
        }
        l = l / gcd_f(l, x) * x.abs();
    }
    ok_num(l)
}

fn gcd_f(mut a: f64, mut b: f64) -> f64 {
    a = a.abs();
    b = b.abs();
    while b != 0.0 {
        let r = a % b;
        a = b;
        b = r;
    }
    a
}

/// Collect numeric arguments the way SUM does: array/range elements keep only
/// real numbers (errors propagate, everything else is ignored), direct scalar
/// arguments are coerced (bool/text/blank become numbers).
fn flatten_scalars(ctx: &FuncCtx, args: &[FuncArg]) -> Result<Vec<f64>, CalcError> {
    let mut out = Vec::new();
    for arg in args {
        match arg.value(ctx)? {
            CalcValue::Array(a) => {
                for v in a.iter() {
                    match v {
                        CalcValue::Number(n) => out.push(*n),
                        CalcValue::Error(e) => return Err(*e),
                        _ => {}
                    }
                }
            }
            v => out.push(coerce_number(&v)?),
        }
    }
    Ok(out)
}

// -- group 9: sums over ranges -----------------------------------------------

fn sumproduct(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let mut arrays: Vec<ArrayValue> = Vec::with_capacity(args.len());
    for arg in args {
        arrays.push(range_array(ctx, arg)?);
    }
    let shape = arrays[0].shape();
    for a in &arrays {
        if a.shape() != shape {
            return Err(CalcError::Value);
        }
    }
    let n = arrays[0].data.len();
    let mut total = 0.0;
    for i in 0..n {
        let mut prod = 1.0;
        for a in &arrays {
            match &a.data[i] {
                CalcValue::Number(x) => prod *= *x,
                CalcValue::Bool(b) => prod *= if *b { 1.0 } else { 0.0 },
                CalcValue::Error(e) => return Err(*e),
                _ => prod *= 0.0,
            }
        }
        total += prod;
    }
    ok_num(total)
}

fn sumsq(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let vals = flatten_scalars(ctx, args)?;
    ok_num(vals.iter().map(|x| x * x).sum())
}

fn sumx_pair(
    ctx: &FuncCtx,
    args: &[FuncArg],
    f: fn(f64, f64) -> f64,
) -> Result<CalcValue, CalcError> {
    let a = range_array(ctx, &args[0])?;
    let b = range_array(ctx, &args[1])?;
    if a.shape() != b.shape() {
        return Err(CalcError::Value);
    }
    let mut total = 0.0;
    for (x, y) in a.iter().zip(b.iter()) {
        match (x, y) {
            (CalcValue::Number(xn), CalcValue::Number(yn)) => total += f(*xn, *yn),
            (CalcValue::Error(e), _) | (_, CalcValue::Error(e)) => return Err(*e),
            _ => {}
        }
    }
    ok_num(total)
}

fn sumx2my2(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    sumx_pair(ctx, args, |x, y| x * x - y * y)
}

fn sumx2py2(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    sumx_pair(ctx, args, |x, y| x * x + y * y)
}

fn sumxmy2(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    sumx_pair(ctx, args, |x, y| (x - y) * (x - y))
}

fn seriessum(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let x = coerce_number(&args[0].value(ctx)?)?;
    let n = coerce_number(&args[1].value(ctx)?)?;
    let m = coerce_number(&args[2].value(ctx)?)?;
    let coeff = range_array(ctx, &args[3])?;
    if coeff.rows > 1 && coeff.cols > 1 {
        return Err(CalcError::Value);
    }
    let mut total = 0.0;
    for (i, c) in coeff.iter().enumerate() {
        let c = match c {
            CalcValue::Number(v) => *v,
            CalcValue::Error(e) => return Err(*e),
            _ => 0.0,
        };
        total += c * x.powf(n + i as f64 * m);
    }
    ok_num(total)
}

// -- group 10: random numbers and bases --------------------------------------

/// SplitMix64 PRNG, process-global so RAND/RANDBETWEEN differ between calls
/// (they must be flagged volatile so the result is never cached).
static RAND_SEED: AtomicU64 = AtomicU64::new(0x9E37_79B9_7F4A_7C15);

fn rand_u64() -> u64 {
    let mut z = RAND_SEED.fetch_add(0x9E37_79B9_7F4A_7C15, AtomicOrdering::Relaxed);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn rand_uniform() -> f64 {
    ((rand_u64() >> 11) as f64) / ((1u64 << 53) as f64)
}

fn rand(_ctx: &FuncCtx, _args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    Ok(CalcValue::Number(rand_uniform()))
}

fn randbetween(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let bottom = coerce_number(&args[0].value(ctx)?)?.round();
    let top = coerce_number(&args[1].value(ctx)?)?.round();
    if bottom > top {
        return Err(CalcError::Num);
    }
    let range = top - bottom + 1.0;
    ok_num(bottom + (rand_uniform() * range).floor())
}

/// ROMAN, ported from the ODFF/LibreOffice algorithm. The `form` argument
/// 0-4 selects how aggressively the subtractive pairs collapse:
/// ROMAN(499,0)="CDXCIX", (499,1)="LDVLIV", (499,2)="XDIX", (499,3)="VDIV",
/// (499,4)="ID" — the published Excel values.
fn roman(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?.trunc();
    let form = if args.len() > 1 {
        coerce_number(&args[1].value(ctx)?)?.trunc()
    } else {
        0.0
    };
    if !(0.0..=3999.0).contains(&n) || !(0.0..=4.0).contains(&form) {
        return Err(CalcError::Value);
    }
    const CHARS: [char; 7] = ['M', 'D', 'C', 'L', 'X', 'V', 'I'];
    const VALS: [u16; 7] = [1000, 500, 100, 50, 10, 5, 1];
    let max_index = 6usize;
    let mut val = n as u16;
    let mode = form as u16;
    let mut out = String::new();
    for i in 0..=(max_index / 2) {
        let n_index = 2 * i;
        let n_digit = val / VALS[n_index];
        if n_digit % 5 == 4 {
            let n_index2 = if n_digit == 4 {
                n_index - 1
            } else {
                n_index - 2
            };
            let mut ni = n_index;
            let mut n_steps = 0u16;
            while n_steps < mode && ni < max_index {
                n_steps += 1;
                if VALS[n_index2] - VALS[ni + 1] <= val {
                    ni += 1;
                } else {
                    n_steps = mode;
                }
            }
            out.push(CHARS[ni]);
            out.push(CHARS[n_index2]);
            val = val.wrapping_add(VALS[ni]).wrapping_sub(VALS[n_index2]);
        } else {
            if n_digit > 4 {
                out.push(CHARS[n_index - 1]);
            }
            for _ in 0..(n_digit % 5) {
                out.push(CHARS[n_index]);
            }
            val %= VALS[n_index];
        }
    }
    Ok(CalcValue::text(out))
}

fn roman_digit(c: char) -> Option<(u16, bool)> {
    match c {
        'M' => Some((1000, true)),
        'D' => Some((500, false)),
        'C' => Some((100, true)),
        'L' => Some((50, false)),
        'X' => Some((10, true)),
        'V' => Some((5, false)),
        'I' => Some((1, true)),
        _ => None,
    }
}

/// ARABIC, ported from the ODFF/LibreOffice validator (case-insensitive,
/// rejects non-canonical ordering and values above 3999 with #VALUE!).
fn arabic(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let s = coerce_text(&args[0].value(ctx)?)?;
    let s = s.trim().to_ascii_uppercase();
    if s.is_empty() {
        return Err(CalcError::Value);
    }
    let chars: Vec<char> = s.chars().collect();
    let mut value = 0u16;
    let mut valid_rest: u16 = 3999;
    let mut i = 0;
    while i < chars.len() {
        let (d1, is_dec1) = match roman_digit(chars[i]) {
            Some(v) => v,
            None => return Err(CalcError::Value),
        };
        let (d2, _is_dec2) = if i + 1 < chars.len() {
            match roman_digit(chars[i + 1]) {
                Some(v) => v,
                None => return Err(CalcError::Value),
            }
        } else {
            (0, false)
        };
        if d1 >= d2 {
            value = value.wrapping_add(d1);
            valid_rest %= d1 * (if is_dec1 { 5 } else { 2 });
            if valid_rest < d1 {
                return Err(CalcError::Value);
            }
            valid_rest -= d1;
            i += 1;
        } else if d1 * 2 != d2 {
            let diff = d2 - d1;
            value = value.wrapping_add(diff);
            if valid_rest < diff {
                return Err(CalcError::Value);
            }
            valid_rest = d1 - 1;
            i += 2;
        } else {
            return Err(CalcError::Value);
        }
    }
    Ok(CalcValue::Number(value as f64))
}

fn digit_char(d: u8) -> char {
    if d < 10 {
        (b'0' + d) as char
    } else {
        (b'A' + d - 10) as char
    }
}

fn base(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?.trunc();
    let radix = coerce_number(&args[1].value(ctx)?)?.trunc() as i64;
    if n < 0.0 || n >= 9_007_199_254_740_992.0 {
        return Err(CalcError::Num);
    }
    if !(2..=36).contains(&radix) {
        return Err(CalcError::Num);
    }
    let min_len = if args.len() > 2 {
        let m = coerce_number(&args[2].value(ctx)?)?.trunc();
        if !(0.0..=255.0).contains(&m) {
            return Err(CalcError::Num);
        }
        m as usize
    } else {
        0
    };
    let mut v = n as u64;
    let mut digits: Vec<char> = Vec::new();
    loop {
        digits.push(digit_char((v % radix as u64) as u8));
        v /= radix as u64;
        if v == 0 {
            break;
        }
    }
    digits.reverse();
    while digits.len() < min_len {
        digits.insert(0, '0');
    }
    Ok(CalcValue::text(digits.into_iter().collect::<String>()))
}

fn decimal(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let s = coerce_text(&args[0].value(ctx)?)?;
    let radix = coerce_number(&args[1].value(ctx)?)?.trunc() as i64;
    if !(2..=36).contains(&radix) {
        return Err(CalcError::Num);
    }
    let s = s.trim();
    if s.is_empty() || s.starts_with('-') || s.starts_with('+') {
        return Err(CalcError::Num);
    }
    let mut value = 0.0;
    for c in s.chars() {
        let d = match c {
            '0'..='9' => (c as i64) - ('0' as i64),
            'A'..='Z' => (c as i64) - ('A' as i64) + 10,
            'a'..='z' => (c as i64) - ('a' as i64) + 10,
            _ => return Err(CalcError::Num),
        };
        if d >= radix {
            return Err(CalcError::Num);
        }
        value = value * radix as f64 + d as f64;
        if value >= 9_007_199_254_740_992.0 {
            return Err(CalcError::Num);
        }
    }
    ok_num(value)
}

// -- group 11: SUBTOTAL and AGGREGATE ----------------------------------------

/// Welford's online moments so STDEV/VAR don't cancel on tight spreads.
fn welford(nums: &[f64]) -> (f64, f64) {
    let mut mean = 0.0;
    let mut m2 = 0.0;
    for (i, &x) in nums.iter().enumerate() {
        let delta = x - mean;
        mean += delta / (i + 1) as f64;
        m2 += delta * (x - mean);
    }
    (mean, m2)
}

fn stdev_sample(nums: &[f64]) -> f64 {
    (welford(nums).1 / (nums.len() - 1) as f64).sqrt()
}

fn stdev_pop(nums: &[f64]) -> f64 {
    (welford(nums).1 / nums.len() as f64).sqrt()
}

fn var_sample(nums: &[f64]) -> f64 {
    welford(nums).1 / (nums.len() - 1) as f64
}

fn var_pop(nums: &[f64]) -> f64 {
    welford(nums).1 / nums.len() as f64
}

/// The shared aggregate core behind SUBTOTAL and AGGREGATE. `ignore_errors`
/// skips error cells (AGGREGATE options 2/3/6/7); otherwise the first error
/// propagates. Group 3 is COUNTA (counts every non-blank cell), the rest
/// operate on numbers only.
fn aggregate_values(
    group: i64,
    values: &[CalcValue],
    ignore_errors: bool,
) -> Result<CalcValue, CalcError> {
    let mut nums: Vec<f64> = Vec::new();
    let mut non_blank = 0usize;
    for v in values {
        match v {
            CalcValue::Error(e) => {
                if !ignore_errors {
                    return Err(*e);
                }
            }
            CalcValue::Number(n) => {
                nums.push(*n);
                non_blank += 1;
            }
            CalcValue::Bool(_) => non_blank += 1,
            CalcValue::Text(t) => {
                if !t.is_empty() {
                    non_blank += 1;
                }
            }
            _ => {}
        }
    }
    match group {
        1 => {
            if nums.is_empty() {
                return Err(CalcError::Div0);
            }
            ok_num(nums.iter().sum::<f64>() / nums.len() as f64)
        }
        2 => Ok(CalcValue::Number(nums.len() as f64)),
        3 => Ok(CalcValue::Number(non_blank as f64)),
        4 => Ok(CalcValue::Number(
            nums.iter().cloned().reduce(f64::max).unwrap_or(0.0),
        )),
        5 => Ok(CalcValue::Number(
            nums.iter().cloned().reduce(f64::min).unwrap_or(0.0),
        )),
        6 => ok_num(nums.iter().fold(1.0, |a, b| a * b)),
        7 => {
            if nums.len() < 2 {
                return Err(CalcError::Div0);
            }
            ok_num(stdev_sample(&nums))
        }
        8 => {
            if nums.is_empty() {
                return Err(CalcError::Div0);
            }
            ok_num(stdev_pop(&nums))
        }
        9 => ok_num(nums.iter().sum()),
        10 => {
            if nums.len() < 2 {
                return Err(CalcError::Div0);
            }
            ok_num(var_sample(&nums))
        }
        11 => {
            if nums.is_empty() {
                return Err(CalcError::Div0);
            }
            ok_num(var_pop(&nums))
        }
        12 => {
            if nums.is_empty() {
                return Err(CalcError::Num);
            }
            let mut v = nums.clone();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
            let n = v.len();
            ok_num(if n % 2 == 1 {
                v[n / 2]
            } else {
                0.5 * (v[n / 2 - 1] + v[n / 2])
            })
        }
        13 => {
            let mut best: Option<(f64, usize)> = None;
            for &x in &nums {
                let count = nums.iter().filter(|y| y.to_bits() == x.to_bits()).count();
                if count > 1 && best.map_or(true, |(_, c)| count > c) {
                    best = Some((x, count));
                }
            }
            match best {
                Some((x, _)) => Ok(CalcValue::Number(x)),
                None => Err(CalcError::Na),
            }
        }
        _ => Err(CalcError::Value),
    }
}

fn percentile_inc(nums: &[f64], k: f64) -> Result<CalcValue, CalcError> {
    let n = nums.len();
    if n == 0 || !(0.0..=1.0).contains(&k) {
        return Err(CalcError::Num);
    }
    let pos = k * (n - 1) as f64;
    let lo = pos.floor() as usize;
    let frac = pos - lo as f64;
    ok_num(if lo + 1 < n {
        nums[lo] + frac * (nums[lo + 1] - nums[lo])
    } else {
        nums[lo]
    })
}

fn percentile_exc(nums: &[f64], k: f64) -> Result<CalcValue, CalcError> {
    let n = nums.len();
    if n == 0 {
        return Err(CalcError::Num);
    }
    if k <= 1.0 / (n + 1) as f64 || k >= n as f64 / (n + 1) as f64 {
        return Err(CalcError::Num);
    }
    let pos = k * (n + 1) as f64;
    let lo = pos.floor() as usize - 1;
    let frac = pos - pos.floor();
    ok_num(if lo + 1 < n {
        nums[lo] + frac * (nums[lo + 1] - nums[lo])
    } else {
        nums[lo]
    })
}

/// The k-taking AGGREGATE functions (LARGE/SMALL/PERCENTILE/QUARTILE).
fn aggregate_k(
    group: i64,
    values: &[CalcValue],
    k: f64,
    ignore_errors: bool,
) -> Result<CalcValue, CalcError> {
    let mut nums: Vec<f64> = Vec::new();
    for v in values {
        match v {
            CalcValue::Number(n) => nums.push(*n),
            CalcValue::Error(e) => {
                if !ignore_errors {
                    return Err(*e);
                }
            }
            _ => {}
        }
    }
    nums.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let n = nums.len();
    match group {
        14 => {
            let k = k.trunc() as i64;
            if k < 1 || k > n as i64 {
                return Err(CalcError::Num);
            }
            ok_num(nums[n - k as usize])
        }
        15 => {
            let k = k.trunc() as i64;
            if k < 1 || k > n as i64 {
                return Err(CalcError::Num);
            }
            ok_num(nums[k as usize - 1])
        }
        16 => percentile_inc(&nums, k),
        17 => {
            let q = k.trunc() as i64;
            if !(0..=4).contains(&q) {
                return Err(CalcError::Num);
            }
            percentile_inc(&nums, q as f64 / 4.0)
        }
        18 => percentile_exc(&nums, k),
        19 => {
            let q = k.trunc() as i64;
            if !(1..=3).contains(&q) {
                return Err(CalcError::Num);
            }
            percentile_exc(&nums, q as f64 / 4.0)
        }
        _ => Err(CalcError::Value),
    }
}

fn subtotal(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let mut fnum = coerce_number(&args[0].value(ctx)?)?.trunc() as i64;
    if !(1..=11).contains(&fnum) && !(101..=111).contains(&fnum) {
        return Err(CalcError::Value);
    }
    if fnum > 100 {
        fnum -= 100;
    }
    let mut values: Vec<CalcValue> = Vec::new();
    for arg in &args[1..] {
        let arr = range_array(ctx, arg)?;
        values.extend(arr.iter().cloned());
    }
    aggregate_values(fnum, &values, false)
}

fn aggregate(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let fnum = coerce_number(&args[0].value(ctx)?)?.trunc() as i64;
    let options = coerce_number(&args[1].value(ctx)?)?.trunc() as i64;
    if !(1..=19).contains(&fnum) || !(0..=7).contains(&options) {
        return Err(CalcError::Value);
    }
    let ignore_errors = matches!(options, 2 | 3 | 6 | 7);
    let k_needed = matches!(fnum, 14 | 15 | 16 | 17 | 18 | 19);
    if k_needed && args.len() < 4 {
        return Err(CalcError::Value);
    }
    let refs = if k_needed {
        &args[2..args.len() - 1]
    } else {
        &args[2..]
    };
    let mut values: Vec<CalcValue> = Vec::new();
    for arg in refs {
        let arr = range_array(ctx, arg)?;
        values.extend(arr.iter().cloned());
    }
    if k_needed {
        let k = coerce_number(&args[args.len() - 1].value(ctx)?)?;
        aggregate_k(fnum, &values, k, ignore_errors)
    } else {
        aggregate_values(fnum, &values, ignore_errors)
    }
}

// -- group 12: matrix --------------------------------------------------------

fn matrix_to_f64(a: &ArrayValue) -> Result<Vec<f64>, CalcError> {
    let mut out = Vec::with_capacity(a.data.len());
    for v in a.iter() {
        match v {
            CalcValue::Number(n) => out.push(*n),
            CalcValue::Blank => out.push(0.0),
            CalcValue::Bool(_) | CalcValue::Text(_) => return Err(CalcError::Value),
            CalcValue::Error(e) => return Err(*e),
            CalcValue::Array(_) => return Err(CalcError::Value),
        }
    }
    Ok(out)
}

/// In-place LU with partial pivoting (physical row swaps). Returns the sign of
/// the permutation, or None when the matrix is singular.
fn lu_factor(n: usize, a: &mut [f64]) -> Option<f64> {
    let mut sign = 1.0f64;
    for col in 0..n {
        let mut best = col;
        let mut bestv = a[col * n + col].abs();
        for r in (col + 1)..n {
            let v = a[r * n + col].abs();
            if v > bestv {
                bestv = v;
                best = r;
            }
        }
        if bestv == 0.0 {
            return None;
        }
        if best != col {
            for c in 0..n {
                a.swap(col * n + c, best * n + c);
            }
            sign = -sign;
        }
        let pivot = a[col * n + col];
        for r in (col + 1)..n {
            let mult = a[r * n + col] / pivot;
            a[r * n + col] = mult;
            for c in (col + 1)..n {
                a[r * n + c] -= mult * a[col * n + c];
            }
        }
    }
    Some(sign)
}

fn mmult(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let a = range_array(ctx, &args[0])?;
    let b = range_array(ctx, &args[1])?;
    if a.cols != b.rows {
        return Err(CalcError::Value);
    }
    let av = matrix_to_f64(&a)?;
    let bv = matrix_to_f64(&b)?;
    let (m, k, n) = (a.rows as usize, a.cols as usize, b.cols as usize);
    let mut out = vec![0.0; m * n];
    for i in 0..m {
        for j in 0..n {
            let mut s = 0.0;
            for t in 0..k {
                s += av[i * k + t] * bv[t * n + j];
            }
            out[i * n + j] = s;
        }
    }
    Ok(CalcValue::array(ArrayValue::new(
        a.rows,
        b.cols,
        out.into_iter().map(CalcValue::Number).collect(),
    )))
}

fn minverse(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let a = range_array(ctx, &args[0])?;
    if a.rows != a.cols {
        return Err(CalcError::Value);
    }
    let n = a.rows as usize;
    let mut lu = matrix_to_f64(&a)?;
    let mut swaps: Vec<(usize, usize)> = Vec::new();
    for col in 0..n {
        let mut best = col;
        let mut bestv = lu[col * n + col].abs();
        for r in (col + 1)..n {
            let v = lu[r * n + col].abs();
            if v > bestv {
                bestv = v;
                best = r;
            }
        }
        if bestv == 0.0 {
            return Err(CalcError::Num);
        }
        if best != col {
            for c in 0..n {
                lu.swap(col * n + c, best * n + c);
            }
            swaps.push((col, best));
        }
        let pivot = lu[col * n + col];
        for r in (col + 1)..n {
            let mult = lu[r * n + col] / pivot;
            lu[r * n + col] = mult;
            for c in (col + 1)..n {
                lu[r * n + c] -= mult * lu[col * n + c];
            }
        }
    }
    let mut inv = vec![0.0; n * n];
    for j in 0..n {
        let mut b = vec![0.0; n];
        b[j] = 1.0;
        for &(r1, r2) in &swaps {
            b.swap(r1, r2);
        }
        for r in 0..n {
            let mut s = b[r];
            for c in 0..r {
                s -= lu[r * n + c] * b[c];
            }
            b[r] = s;
        }
        for r in (0..n).rev() {
            let mut s = b[r];
            for c in (r + 1)..n {
                s -= lu[r * n + c] * inv[c * n + j];
            }
            inv[r * n + j] = s / lu[r * n + r];
        }
    }
    Ok(CalcValue::array(ArrayValue::new(
        a.rows,
        a.cols,
        inv.into_iter().map(CalcValue::Number).collect(),
    )))
}

fn mdeterm(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let a = range_array(ctx, &args[0])?;
    if a.rows != a.cols {
        return Err(CalcError::Value);
    }
    let n = a.rows as usize;
    let mut m = matrix_to_f64(&a)?;
    let sign = match lu_factor(n, &mut m) {
        Some(s) => s,
        None => return Ok(CalcValue::Number(0.0)),
    };
    let mut det = sign;
    for i in 0..n {
        det *= m[i * n + i];
    }
    ok_num(det)
}

fn transpose(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let a = range_array(ctx, &args[0])?;
    let mut data = Vec::with_capacity((a.rows * a.cols) as usize);
    for c in 0..a.cols {
        for r in 0..a.rows {
            data.push(a.get(r, c).clone());
        }
    }
    Ok(CalcValue::array(ArrayValue::new(a.cols, a.rows, data)))
}

fn munit(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?.trunc() as i64;
    if n < 1 {
        return Err(CalcError::Value);
    }
    let n = n as u32;
    if n as u64 * n as u64 > 4_000_000 {
        return Err(CalcError::Value);
    }
    let mut data = vec![CalcValue::Number(0.0); (n * n) as usize];
    for i in 0..n {
        data[(i * n + i) as usize] = CalcValue::Number(1.0);
    }
    Ok(CalcValue::array(ArrayValue::new(n, n, data)))
}

const SUM: FuncSpec = FuncSpec {
    name: "SUM",
    min_args: 0,
    max_args: None,
    volatile: false,
    array_aware: true,
    func: sum,
};

const PRODUCT: FuncSpec = FuncSpec {
    name: "PRODUCT",
    min_args: 1,
    max_args: None,
    volatile: false,
    array_aware: true,
    func: product,
};

const AVERAGE: FuncSpec = FuncSpec {
    name: "AVERAGE",
    min_args: 1,
    max_args: None,
    volatile: false,
    array_aware: true,
    func: average,
};

const COUNT: FuncSpec = FuncSpec {
    name: "COUNT",
    min_args: 1,
    max_args: None,
    volatile: false,
    array_aware: true,
    func: count,
};

const COUNTA: FuncSpec = FuncSpec {
    name: "COUNTA",
    min_args: 1,
    max_args: None,
    volatile: false,
    array_aware: true,
    func: counta,
};

const MIN: FuncSpec = FuncSpec {
    name: "MIN",
    min_args: 1,
    max_args: None,
    volatile: false,
    array_aware: true,
    func: min,
};

const MAX: FuncSpec = FuncSpec {
    name: "MAX",
    min_args: 1,
    max_args: None,
    volatile: false,
    array_aware: true,
    func: max,
};

const ABS: FuncSpec = FuncSpec {
    name: "ABS",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: abs,
};

const INT: FuncSpec = FuncSpec {
    name: "INT",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: int,
};

const MOD: FuncSpec = FuncSpec {
    name: "MOD",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: mod_,
};

const POWER: FuncSpec = FuncSpec {
    name: "POWER",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: power,
};

const SQRT: FuncSpec = FuncSpec {
    name: "SQRT",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: sqrt,
};

const ROUND: FuncSpec = FuncSpec {
    name: "ROUND",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: round,
};

const ROUNDUP: FuncSpec = FuncSpec {
    name: "ROUNDUP",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: roundup,
};

const ROUNDDOWN: FuncSpec = FuncSpec {
    name: "ROUNDDOWN",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: rounddown,
};

const SUMIF: FuncSpec = FuncSpec {
    name: "SUMIF",
    min_args: 2,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: sumif,
};

const SUMIFS: FuncSpec = FuncSpec {
    name: "SUMIFS",
    min_args: 3,
    max_args: None,
    volatile: false,
    array_aware: false,
    func: sumifs,
};

const COUNTIF: FuncSpec = FuncSpec {
    name: "COUNTIF",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: countif,
};

const COUNTIFS: FuncSpec = FuncSpec {
    name: "COUNTIFS",
    min_args: 2,
    max_args: None,
    volatile: false,
    array_aware: false,
    func: countifs,
};

const AVERAGEIF: FuncSpec = FuncSpec {
    name: "AVERAGEIF",
    min_args: 2,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: averageif,
};

const AVERAGEIFS: FuncSpec = FuncSpec {
    name: "AVERAGEIFS",
    min_args: 3,
    max_args: None,
    volatile: false,
    array_aware: false,
    func: averageifs,
};

const PI: FuncSpec = FuncSpec {
    name: "PI",
    min_args: 0,
    max_args: Some(0),
    volatile: false,
    array_aware: false,
    func: pi,
};

const EXP: FuncSpec = FuncSpec {
    name: "EXP",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: exp,
};

const LN: FuncSpec = FuncSpec {
    name: "LN",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: ln,
};

const LOG: FuncSpec = FuncSpec {
    name: "LOG",
    min_args: 1,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: log,
};

const LOG10: FuncSpec = FuncSpec {
    name: "LOG10",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: log10,
};

const SIN: FuncSpec = FuncSpec {
    name: "SIN",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: sin,
};

const COS: FuncSpec = FuncSpec {
    name: "COS",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: cos,
};

const TAN: FuncSpec = FuncSpec {
    name: "TAN",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: tan,
};

const CSC: FuncSpec = FuncSpec {
    name: "CSC",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: csc,
};

const SEC: FuncSpec = FuncSpec {
    name: "SEC",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: sec,
};

const COT: FuncSpec = FuncSpec {
    name: "COT",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: cot,
};

const ASIN: FuncSpec = FuncSpec {
    name: "ASIN",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: asin,
};

const ACOS: FuncSpec = FuncSpec {
    name: "ACOS",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: acos,
};

const ATAN: FuncSpec = FuncSpec {
    name: "ATAN",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: atan,
};

const ATAN2: FuncSpec = FuncSpec {
    name: "ATAN2",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: atan2,
};

const ACOT: FuncSpec = FuncSpec {
    name: "ACOT",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: acot,
};

const SINH: FuncSpec = FuncSpec {
    name: "SINH",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: sinh,
};

const COSH: FuncSpec = FuncSpec {
    name: "COSH",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: cosh,
};

const TANH: FuncSpec = FuncSpec {
    name: "TANH",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: tanh,
};

const CSCH: FuncSpec = FuncSpec {
    name: "CSCH",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: csch,
};

const SECH: FuncSpec = FuncSpec {
    name: "SECH",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: sech,
};

const COTH: FuncSpec = FuncSpec {
    name: "COTH",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: coth,
};

const ASINH: FuncSpec = FuncSpec {
    name: "ASINH",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: asinh,
};

const ACOSH: FuncSpec = FuncSpec {
    name: "ACOSH",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: acosh,
};

const ATANH: FuncSpec = FuncSpec {
    name: "ATANH",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: atanh,
};

const ACOTH: FuncSpec = FuncSpec {
    name: "ACOTH",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: acoth,
};

const DEGREES: FuncSpec = FuncSpec {
    name: "DEGREES",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: degrees,
};

const RADIANS: FuncSpec = FuncSpec {
    name: "RADIANS",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: radians,
};

const CEILING: FuncSpec = FuncSpec {
    name: "CEILING",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: ceiling_legacy,
};

const CEILING_MATH: FuncSpec = FuncSpec {
    name: "CEILING.MATH",
    min_args: 1,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: ceiling_math,
};

const CEILING_PRECISE: FuncSpec = FuncSpec {
    name: "CEILING.PRECISE",
    min_args: 1,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: ceiling_precise,
};

const FLOOR: FuncSpec = FuncSpec {
    name: "FLOOR",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: floor_legacy,
};

const FLOOR_MATH: FuncSpec = FuncSpec {
    name: "FLOOR.MATH",
    min_args: 1,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: floor_math,
};

const FLOOR_PRECISE: FuncSpec = FuncSpec {
    name: "FLOOR.PRECISE",
    min_args: 1,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: floor_precise,
};

const MROUND: FuncSpec = FuncSpec {
    name: "MROUND",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: mround,
};

const TRUNC: FuncSpec = FuncSpec {
    name: "TRUNC",
    min_args: 1,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: trunc,
};

const EVEN: FuncSpec = FuncSpec {
    name: "EVEN",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: even,
};

const ODD: FuncSpec = FuncSpec {
    name: "ODD",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: odd,
};

const SIGN: FuncSpec = FuncSpec {
    name: "SIGN",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: sign,
};

const QUOTIENT: FuncSpec = FuncSpec {
    name: "QUOTIENT",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: quotient,
};

const ISO_CEILING: FuncSpec = FuncSpec {
    name: "ISO.CEILING",
    min_args: 1,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: ceiling_precise,
};

const FACT: FuncSpec = FuncSpec {
    name: "FACT",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: fact,
};

const FACTDOUBLE: FuncSpec = FuncSpec {
    name: "FACTDOUBLE",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: factdouble,
};

const COMBIN: FuncSpec = FuncSpec {
    name: "COMBIN",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: combin,
};

const COMBINA: FuncSpec = FuncSpec {
    name: "COMBINA",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: combina,
};

const PERMUT: FuncSpec = FuncSpec {
    name: "PERMUT",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: permut,
};

const PERMUTATIONA: FuncSpec = FuncSpec {
    name: "PERMUTATIONA",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: permutationa,
};

const MULTINOMIAL: FuncSpec = FuncSpec {
    name: "MULTINOMIAL",
    min_args: 1,
    max_args: Some(255),
    volatile: false,
    array_aware: true,
    func: multinomial,
};

const GCD: FuncSpec = FuncSpec {
    name: "GCD",
    min_args: 1,
    max_args: Some(255),
    volatile: false,
    array_aware: true,
    func: gcd,
};

const LCM: FuncSpec = FuncSpec {
    name: "LCM",
    min_args: 1,
    max_args: Some(255),
    volatile: false,
    array_aware: true,
    func: lcm,
};

const SUMPRODUCT: FuncSpec = FuncSpec {
    name: "SUMPRODUCT",
    min_args: 1,
    max_args: Some(255),
    volatile: false,
    array_aware: true,
    func: sumproduct,
};

const SUMSQ: FuncSpec = FuncSpec {
    name: "SUMSQ",
    min_args: 1,
    max_args: Some(255),
    volatile: false,
    array_aware: true,
    func: sumsq,
};

const SUMX2MY2: FuncSpec = FuncSpec {
    name: "SUMX2MY2",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: true,
    func: sumx2my2,
};

const SUMX2PY2: FuncSpec = FuncSpec {
    name: "SUMX2PY2",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: true,
    func: sumx2py2,
};

const SUMXMY2: FuncSpec = FuncSpec {
    name: "SUMXMY2",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: true,
    func: sumxmy2,
};

const SERIESSUM: FuncSpec = FuncSpec {
    name: "SERIESSUM",
    min_args: 4,
    max_args: Some(4),
    volatile: false,
    array_aware: true,
    func: seriessum,
};

const RAND: FuncSpec = FuncSpec {
    name: "RAND",
    min_args: 0,
    max_args: Some(0),
    volatile: true,
    array_aware: false,
    func: rand,
};

const RANDBETWEEN: FuncSpec = FuncSpec {
    name: "RANDBETWEEN",
    min_args: 2,
    max_args: Some(2),
    volatile: true,
    array_aware: false,
    func: randbetween,
};

const ROMAN: FuncSpec = FuncSpec {
    name: "ROMAN",
    min_args: 1,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: roman,
};

const ARABIC: FuncSpec = FuncSpec {
    name: "ARABIC",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: arabic,
};

const BASE: FuncSpec = FuncSpec {
    name: "BASE",
    min_args: 2,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: base,
};

const DECIMAL: FuncSpec = FuncSpec {
    name: "DECIMAL",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: decimal,
};

const SUBTOTAL: FuncSpec = FuncSpec {
    name: "SUBTOTAL",
    min_args: 2,
    max_args: Some(255),
    volatile: false,
    array_aware: false,
    func: subtotal,
};

const AGGREGATE: FuncSpec = FuncSpec {
    name: "AGGREGATE",
    min_args: 3,
    max_args: Some(255),
    volatile: false,
    array_aware: false,
    func: aggregate,
};

const MMULT: FuncSpec = FuncSpec {
    name: "MMULT",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: true,
    func: mmult,
};

const MINVERSE: FuncSpec = FuncSpec {
    name: "MINVERSE",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: true,
    func: minverse,
};

const MDETERM: FuncSpec = FuncSpec {
    name: "MDETERM",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: true,
    func: mdeterm,
};

const TRANSPOSE: FuncSpec = FuncSpec {
    name: "TRANSPOSE",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: true,
    func: transpose,
};

const MUNIT: FuncSpec = FuncSpec {
    name: "MUNIT",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: true,
    func: munit,
};

pub fn register(r: &mut Registry) {
    r.register(&SUM);
    r.register(&PRODUCT);
    r.register(&AVERAGE);
    r.register(&COUNT);
    r.register(&COUNTA);
    r.register(&MIN);
    r.register(&MAX);
    r.register(&ABS);
    r.register(&INT);
    r.register(&MOD);
    r.register(&POWER);
    r.register(&SQRT);
    r.register(&ROUND);
    r.register(&ROUNDUP);
    r.register(&ROUNDDOWN);
    r.register(&SUMIF);
    r.register(&SUMIFS);
    r.register(&COUNTIF);
    r.register(&COUNTIFS);
    r.register(&AVERAGEIF);
    r.register(&AVERAGEIFS);
    // trigonometry and logarithms
    r.register(&PI);
    r.register(&EXP);
    r.register(&LN);
    r.register(&LOG);
    r.register(&LOG10);
    r.register(&SIN);
    r.register(&COS);
    r.register(&TAN);
    r.register(&CSC);
    r.register(&SEC);
    r.register(&COT);
    r.register(&ASIN);
    r.register(&ACOS);
    r.register(&ATAN);
    r.register(&ATAN2);
    r.register(&ACOT);
    r.register(&SINH);
    r.register(&COSH);
    r.register(&TANH);
    r.register(&CSCH);
    r.register(&SECH);
    r.register(&COTH);
    r.register(&ASINH);
    r.register(&ACOSH);
    r.register(&ATANH);
    r.register(&ACOTH);
    r.register(&DEGREES);
    r.register(&RADIANS);
    // rounding and sign
    r.register(&CEILING);
    r.register(&CEILING_MATH);
    r.register(&CEILING_PRECISE);
    r.register(&FLOOR);
    r.register(&FLOOR_MATH);
    r.register(&FLOOR_PRECISE);
    r.register(&MROUND);
    r.register(&TRUNC);
    r.register(&EVEN);
    r.register(&ODD);
    r.register(&SIGN);
    r.register(&QUOTIENT);
    r.register(&ISO_CEILING);
    // combinatorics
    r.register(&FACT);
    r.register(&FACTDOUBLE);
    r.register(&COMBIN);
    r.register(&COMBINA);
    r.register(&PERMUT);
    r.register(&PERMUTATIONA);
    r.register(&MULTINOMIAL);
    r.register(&GCD);
    r.register(&LCM);
    // sums over ranges
    r.register(&SUMPRODUCT);
    r.register(&SUMSQ);
    r.register(&SUMX2MY2);
    r.register(&SUMX2PY2);
    r.register(&SUMXMY2);
    r.register(&SERIESSUM);
    // random numbers and bases
    r.register(&RAND);
    r.register(&RANDBETWEEN);
    r.register(&ROMAN);
    r.register(&ARABIC);
    r.register(&BASE);
    r.register(&DECIMAL);
    // aggregation
    r.register(&SUBTOTAL);
    r.register(&AGGREGATE);
    // matrix
    r.register(&MMULT);
    r.register(&MINVERSE);
    r.register(&MDETERM);
    r.register(&TRANSPOSE);
    r.register(&MUNIT);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turbo::calc::functions::{CellResolver, Func};

    struct EmptyResolver;
    impl CellResolver for EmptyResolver {
        fn cell(&self, _sheet: u32, _row: u32, _col: u32) -> Option<CalcValue> {
            None
        }
        fn sheet_index(&self, _name: &str) -> Option<u32> {
            None
        }
    }

    fn ctx() -> FuncCtx<'static> {
        static R: EmptyResolver = EmptyResolver;
        FuncCtx {
            date1904: false,
            sheet: 0,
            row: 0,
            col: 0,
            resolver: &R,
        }
    }

    fn call(f: Func, vals: Vec<CalcValue>) -> Result<CalcValue, CalcError> {
        let c = ctx();
        let args: Vec<FuncArg> = vals.into_iter().map(FuncArg::Value).collect();
        f(&c, &args)
    }

    fn arr(rows: u32, cols: u32, data: Vec<CalcValue>) -> CalcValue {
        CalcValue::array(ArrayValue::new(rows, cols, data))
    }

    fn col(data: Vec<CalcValue>) -> CalcValue {
        arr(data.len() as u32, 1, data)
    }

    fn num(n: f64) -> CalcValue {
        CalcValue::Number(n)
    }

    fn txt(s: &str) -> CalcValue {
        CalcValue::text(s)
    }

    #[test]
    fn sum_ignores_text_blank_bool_in_arrays() {
        let v = col(vec![
            num(1.0),
            num(2.0),
            txt("5"),
            CalcValue::Blank,
            CalcValue::Bool(true),
        ]);
        assert_eq!(call(sum, vec![v]), Ok(num(3.0)));
        assert_eq!(
            call(sum, vec![CalcValue::Blank, CalcValue::Blank]),
            Ok(num(0.0))
        );
    }

    #[test]
    fn sum_coerces_scalar_bool_and_numeric_text() {
        assert_eq!(
            call(sum, vec![CalcValue::Bool(true), txt("5"), num(1.0)]),
            Ok(num(7.0))
        );
        assert_eq!(call(sum, vec![txt("abc")]), Err(CalcError::Value));
    }

    #[test]
    fn sum_propagates_errors() {
        let v = col(vec![num(1.0), CalcValue::err(CalcError::Na)]);
        assert_eq!(call(sum, vec![v]), Err(CalcError::Na));
        assert_eq!(
            call(sum, vec![CalcValue::err(CalcError::Div0)]),
            Err(CalcError::Div0)
        );
    }

    #[test]
    fn product_basic_and_empty() {
        let v = col(vec![num(2.0), num(3.0), num(4.0)]);
        assert_eq!(call(product, vec![v]), Ok(num(24.0)));
        let empty = col(vec![txt("x"), CalcValue::Blank, CalcValue::Bool(true)]);
        assert_eq!(call(product, vec![empty]), Ok(num(0.0)));
    }

    #[test]
    fn average_basic_and_div0() {
        let v = col(vec![num(1.0), num(2.0), num(3.0)]);
        assert_eq!(call(average, vec![v]), Ok(num(2.0)));
        let empty = col(vec![txt("x"), CalcValue::Blank]);
        assert_eq!(call(average, vec![empty]), Err(CalcError::Div0));
    }

    #[test]
    fn count_vs_counta() {
        let v = col(vec![
            num(1.0),
            txt("x"),
            CalcValue::Blank,
            CalcValue::Bool(true),
        ]);
        assert_eq!(call(count, vec![v.clone()]), Ok(num(1.0)));
        assert_eq!(call(counta, vec![v]), Ok(num(3.0)));
        let blanks = col(vec![CalcValue::Blank, CalcValue::Blank]);
        assert_eq!(call(counta, vec![blanks]), Ok(num(0.0)));
    }

    #[test]
    fn min_max_empty_is_zero() {
        let empty = col(vec![txt("x"), CalcValue::Blank]);
        assert_eq!(call(min, vec![empty.clone()]), Ok(num(0.0)));
        assert_eq!(call(max, vec![empty]), Ok(num(0.0)));
        assert_eq!(call(min, vec![num(3.0), num(1.0)]), Ok(num(1.0)));
        assert_eq!(call(max, vec![num(3.0), num(1.0)]), Ok(num(3.0)));
    }

    #[test]
    fn abs_and_int() {
        assert_eq!(call(abs, vec![num(-3.5)]), Ok(num(3.5)));
        assert_eq!(call(abs, vec![num(2.0)]), Ok(num(2.0)));
        assert_eq!(call(int, vec![num(1.5)]), Ok(num(1.0)));
        assert_eq!(call(int, vec![num(-1.5)]), Ok(num(-2.0)));
    }

    #[test]
    fn mod_sign_follows_divisor_and_div0() {
        assert_eq!(call(mod_, vec![num(3.0), num(2.0)]), Ok(num(1.0)));
        assert_eq!(call(mod_, vec![num(-3.0), num(2.0)]), Ok(num(1.0)));
        assert_eq!(call(mod_, vec![num(3.0), num(-2.0)]), Ok(num(-1.0)));
        assert_eq!(call(mod_, vec![num(-3.0), num(-2.0)]), Ok(num(-1.0)));
        assert_eq!(call(mod_, vec![num(3.0), num(0.0)]), Err(CalcError::Div0));
    }

    #[test]
    fn power_basic_and_errors() {
        assert_eq!(call(power, vec![num(2.0), num(3.0)]), Ok(num(8.0)));
        assert_eq!(call(power, vec![num(-8.0), num(3.0)]), Ok(num(-512.0)));
        assert_eq!(call(power, vec![num(0.0), num(-1.0)]), Err(CalcError::Div0));
        assert_eq!(call(power, vec![num(-2.0), num(0.5)]), Err(CalcError::Num));
    }

    #[test]
    fn sqrt_negative_is_num() {
        assert_eq!(call(sqrt, vec![num(9.0)]), Ok(num(3.0)));
        assert_eq!(call(sqrt, vec![num(0.0)]), Ok(num(0.0)));
        assert_eq!(call(sqrt, vec![num(-1.0)]), Err(CalcError::Num));
    }

    #[test]
    fn round_negative_digits_and_half_away() {
        assert_eq!(call(round, vec![num(155.0), num(-1.0)]), Ok(num(160.0)));
        assert_eq!(call(round, vec![num(155.0), num(-2.0)]), Ok(num(200.0)));
        assert_eq!(call(round, vec![num(2.5), num(0.0)]), Ok(num(3.0)));
        assert_eq!(call(round, vec![num(-2.5), num(0.0)]), Ok(num(-3.0)));
        assert_eq!(call(round, vec![num(2.346), num(2.0)]), Ok(num(2.35)));
    }

    #[test]
    fn roundup_rounddown() {
        assert_eq!(call(roundup, vec![num(2.11), num(1.0)]), Ok(num(2.2)));
        assert_eq!(call(rounddown, vec![num(2.19), num(1.0)]), Ok(num(2.1)));
        assert_eq!(call(roundup, vec![num(-2.11), num(1.0)]), Ok(num(-2.2)));
        assert_eq!(call(rounddown, vec![num(-2.19), num(1.0)]), Ok(num(-2.1)));
        assert_eq!(call(roundup, vec![num(155.0), num(-1.0)]), Ok(num(160.0)));
        assert_eq!(call(rounddown, vec![num(155.0), num(-1.0)]), Ok(num(150.0)));
    }

    #[test]
    fn sumif_with_operator_and_sum_range() {
        let range = col(vec![num(1.0), num(5.0), num(6.0), num(10.0)]);
        assert_eq!(call(sumif, vec![range.clone(), txt(">5")]), Ok(num(16.0)));
        let sum_range = col(vec![num(10.0), num(20.0), num(30.0), num(40.0)]);
        assert_eq!(
            call(sumif, vec![range, txt(">5"), sum_range]),
            Ok(num(70.0))
        );
    }

    #[test]
    fn sumif_shape_mismatch_is_value() {
        let range = col(vec![num(1.0), num(2.0), num(3.0)]);
        let small = col(vec![num(1.0), num(2.0)]);
        assert_eq!(
            call(sumif, vec![range, txt(">0"), small]),
            Err(CalcError::Value)
        );
    }

    #[test]
    fn countif_with_wildcard_and_text() {
        let range = col(vec![txt("apple"), txt("apricot"), txt("banana")]);
        assert_eq!(call(countif, vec![range, txt("ap*")]), Ok(num(2.0)));
        let nums = col(vec![num(1.0), num(2.0), num(3.0)]);
        assert_eq!(call(countif, vec![nums, txt(">1")]), Ok(num(2.0)));
    }

    #[test]
    fn averageif_and_no_match() {
        let range = col(vec![num(1.0), num(2.0), num(3.0)]);
        assert_eq!(
            call(averageif, vec![range.clone(), txt(">1")]),
            Ok(num(2.5))
        );
        assert_eq!(
            call(averageif, vec![range, txt(">5")]),
            Err(CalcError::Div0)
        );
    }

    #[test]
    fn sumifs_and_countifs() {
        let sum_range = col(vec![num(1.0), num(2.0), num(3.0)]);
        let crit1 = col(vec![num(10.0), num(20.0), num(30.0)]);
        assert_eq!(
            call(sumifs, vec![sum_range, crit1, txt(">15")]),
            Ok(num(5.0))
        );
        let c1 = col(vec![txt("a"), txt("b"), txt("a")]);
        let c2 = col(vec![num(1.0), num(2.0), num(3.0)]);
        assert_eq!(
            call(countifs, vec![c1, txt("a"), c2, txt(">1")]),
            Ok(num(1.0))
        );
    }

    #[test]
    fn averageifs_no_match_is_div0() {
        let avg = col(vec![num(1.0), num(2.0), num(3.0)]);
        let crit1 = col(vec![num(10.0), num(20.0), num(30.0)]);
        assert_eq!(
            call(averageifs, vec![avg, crit1, txt(">100")]),
            Err(CalcError::Div0)
        );
    }

    // -- group 6: trigonometry and logs ------------------------------------

    use crate::turbo::calc::testkit::Grid;

    fn n(f: &str) -> f64 {
        Grid::empty().num(f)
    }
    fn e(f: &str) -> CalcError {
        Grid::empty().error(f)
    }
    fn t(f: &str) -> String {
        Grid::empty().text(f)
    }
    fn ax(f: &str, expected: f64, rel: f64) {
        crate::turbo::calc::testkit::approx(f, expected, rel);
    }

    #[test]
    fn trig_basics() {
        assert_eq!(n("=PI()"), std::f64::consts::PI);
        ax("=SIN(PI()/2)", 1.0, 1e-12);
        assert_eq!(n("=COS(0)"), 1.0);
        ax("=TAN(PI()/4)", 1.0, 1e-12);
        ax("=CSC(PI()/2)", 1.0, 1e-12);
        assert_eq!(n("=SEC(0)"), 1.0);
        ax("=COT(PI()/4)", 1.0, 1e-12);
        ax("=ASIN(1)", std::f64::consts::FRAC_PI_2, 1e-15);
        ax("=ACOS(1)", 0.0, 1e-15);
        ax("=ATAN(1)", std::f64::consts::FRAC_PI_4, 1e-15);
        // ATAN2(x, y) is the arctangent of y/x: the angle of point (x, y).
        ax("=ATAN2(1,1)", std::f64::consts::FRAC_PI_4, 1e-15);
        ax("=ATAN2(1,-1)", -std::f64::consts::FRAC_PI_4, 1e-15);
        ax("=ATAN2(-1,1)", 3.0 * std::f64::consts::FRAC_PI_4, 1e-15);
        ax("=ATAN2(0,-1)", -std::f64::consts::FRAC_PI_2, 1e-15);
        ax("=ACOT(1)", std::f64::consts::FRAC_PI_4, 1e-15);
        ax("=ACOT(-1)", 3.0 * std::f64::consts::FRAC_PI_4, 1e-15);
        ax("=ACOT(0)", std::f64::consts::FRAC_PI_2, 1e-15);
        assert_eq!(n("=SINH(0)"), 0.0);
        assert_eq!(n("=COSH(0)"), 1.0);
        ax("=TANH(0.5)", 0.4621171572600098, 1e-13);
        ax("=CSCH(1)", 1.0 / 1.1752011936438014, 1e-13);
        ax("=SECH(0)", 1.0, 1e-15);
        ax("=COTH(1)", 1.0 / 0.7615941559557649, 1e-13);
    }

    #[test]
    fn log_exp_degrees_radians() {
        assert_eq!(n("=EXP(0)"), 1.0);
        ax("=EXP(1)", std::f64::consts::E, 1e-15);
        assert_eq!(n("=LN(1)"), 0.0);
        ax("=LN(EXP(3))", 3.0, 1e-12);
        assert_eq!(n("=LOG10(1000)"), 3.0);
        ax("=LOG(100,10)", 2.0, 1e-12);
        ax("=LOG(8,2)", 3.0, 1e-12);
        ax("=LOG(100)", 2.0, 1e-12);
        ax("=DEGREES(PI())", 180.0, 1e-12);
        ax("=RADIANS(180)", std::f64::consts::PI, 1e-15);
    }

    #[test]
    fn trig_domain_and_pole_errors() {
        assert_eq!(e("=LN(0)"), CalcError::Num);
        assert_eq!(e("=LN(-1)"), CalcError::Num);
        assert_eq!(e("=LOG10(0)"), CalcError::Num);
        assert_eq!(e("=LOG(10,0)"), CalcError::Num);
        assert_eq!(e("=LOG(10,1)"), CalcError::Div0);
        assert_eq!(e("=LOG(-5)"), CalcError::Num);
        assert_eq!(e("=ASIN(2)"), CalcError::Num);
        assert_eq!(e("=ASIN(-1.1)"), CalcError::Num);
        assert_eq!(e("=ACOS(2)"), CalcError::Num);
        assert_eq!(e("=ACOSH(0.5)"), CalcError::Num);
        assert_eq!(e("=ATANH(1)"), CalcError::Num);
        assert_eq!(e("=ATANH(-1)"), CalcError::Num);
        assert_eq!(e("=ATANH(2)"), CalcError::Num);
        assert_eq!(e("=ACOTH(1)"), CalcError::Num);
        assert_eq!(e("=ACOTH(0)"), CalcError::Num);
        assert_eq!(e("=ACOTH(-0.5)"), CalcError::Num);
        assert_eq!(e("=CSC(0)"), CalcError::Div0);
        assert_eq!(e("=COT(0)"), CalcError::Div0);
        assert_eq!(e("=CSCH(0)"), CalcError::Div0);
        assert_eq!(e("=COTH(0)"), CalcError::Div0);
        assert_eq!(e("=EXP(1000)"), CalcError::Num);
    }

    #[test]
    fn hyperbolic_inverse_accuracy() {
        // ln-identity derivatives; values to full double precision:
        //   ACOSH(2) = 1.3169578969248166
        //   ASINH(1) = ln(1 + sqrt 2) = 0.8813735870195430
        //   ATANH(0.5) = ACOTH(2) = 0.5493061443340549
        ax("=ACOSH(2)", 1.3169578969248166, 1e-14);
        ax("=ASINH(1)", 0.8813735870195430, 1e-14);
        ax("=ATANH(0.5)", 0.5493061443340549, 1e-14);
        ax("=ACOTH(2)", 0.5493061443340549, 1e-14);
        ax("=ACOSH(3)", 1.7627471740390860, 1e-14);
        ax("=ATANH(-0.25)", -0.2554128118829954, 1e-14);
        // near the acosh asymptote x -> 1 the split-square-root form avoids
        // cancellation, so ACOSH(1 + 1e-10) stays correct to ~1e-12 relative.
        ax("=ACOSH(1.0000000001)", 1.4142135623730951e-5, 1e-6);
        // round-trip through the identity x -> x
        ax("=TANH(ACOTH(3))", 1.0 / 3.0, 1e-12);
    }

    // -- group 7: rounding and sign ----------------------------------------

    #[test]
    fn ceiling_legacy_published_values() {
        assert_eq!(n("=CEILING(2.5,1)"), 3.0);
        assert_eq!(n("=CEILING(-2.5,-2)"), -4.0);
        assert_eq!(n("=CEILING(-2.5,2)"), -4.0);
        assert_eq!(n("=CEILING(1.5,0.1)"), 1.5);
        assert_eq!(n("=CEILING(0.234,0.01)"), 0.24);
        assert_eq!(n("=CEILING(0,5)"), 0.0);
        assert_eq!(n("=CEILING(5,0)"), 0.0);
        assert_eq!(e("=CEILING(2.5,-2)"), CalcError::Num);
    }

    #[test]
    fn floor_legacy_published_values() {
        assert_eq!(n("=FLOOR(10,3)"), 9.0);
        assert_eq!(n("=FLOOR(40,7)"), 35.0);
        assert_eq!(n("=FLOOR(320,25)"), 300.0);
        assert_eq!(n("=FLOOR(610,100)"), 600.0);
        // modern Excel (2010+): FLOOR(-5.4,1) rounds away from zero to -6 and
        // only errors when the number is positive and the significance negative.
        assert_eq!(n("=FLOOR(-5.4,1)"), -6.0);
        assert_eq!(n("=FLOOR(-5.4,-1)"), -5.0);
        assert_eq!(n("=FLOOR(-2.5,-2)"), -2.0);
        assert_eq!(e("=FLOOR(2.5,-2)"), CalcError::Num);
        assert_eq!(e("=FLOOR(5,0)"), CalcError::Div0);
        assert_eq!(n("=FLOOR(0,5)"), 0.0);
    }

    #[test]
    fn ceiling_floor_math_precise() {
        assert_eq!(n("=CEILING.MATH(4.3)"), 5.0);
        assert_eq!(n("=CEILING.MATH(-4.3)"), -4.0);
        assert_eq!(n("=CEILING.MATH(-8.1,2)"), -8.0);
        assert_eq!(n("=CEILING.MATH(-5.5,2,-1)"), -4.0);
        assert_eq!(n("=CEILING.MATH(24.3,5)"), 25.0);
        assert_eq!(n("=CEILING.MATH(6.7)"), 7.0);
        assert_eq!(n("=FLOOR.MATH(24.3,5)"), 20.0);
        assert_eq!(n("=FLOOR.MATH(-8.1,2)"), -10.0);
        assert_eq!(n("=FLOOR.MATH(-5.5,2,-1)"), -4.0);
        assert_eq!(n("=FLOOR.MATH(6.7)"), 6.0);
        assert_eq!(n("=CEILING.PRECISE(-8.1,2)"), -8.0);
        assert_eq!(n("=CEILING.PRECISE(8.1,2)"), 10.0);
        assert_eq!(n("=FLOOR.PRECISE(-8.1,2)"), -10.0);
        assert_eq!(n("=ISO.CEILING(-8.1,2)"), -8.0);
        assert_eq!(n("=ISO.CEILING(8.1,2)"), 10.0);
        assert_eq!(n("=CEILING.MATH(0,5)"), 0.0);
        assert_eq!(n("=FLOOR.MATH(5,0)"), 0.0);
    }

    #[test]
    fn mround_trunc_even_odd_sign_quotient() {
        assert_eq!(n("=MROUND(10,3)"), 9.0);
        assert_eq!(n("=MROUND(5,2)"), 6.0);
        assert_eq!(n("=MROUND(-10,-3)"), -9.0);
        assert_eq!(n("=MROUND(0,5)"), 0.0);
        assert_eq!(e("=MROUND(10,-3)"), CalcError::Num);
        assert_eq!(e("=MROUND(10,0)"), CalcError::Div0);
        ax("=MROUND(1.3,0.2)", 1.4, 1e-12);
        assert_eq!(n("=TRUNC(8.9)"), 8.0);
        assert_eq!(n("=TRUNC(-8.9)"), -8.0);
        assert_eq!(n("=TRUNC(123.456,1)"), 123.4);
        assert_eq!(n("=TRUNC(155,-1)"), 150.0);
        assert_eq!(n("=TRUNC(4.5)"), 4.0);
        assert_eq!(n("=EVEN(1.5)"), 2.0);
        assert_eq!(n("=EVEN(3)"), 4.0);
        assert_eq!(n("=EVEN(-1.5)"), -2.0);
        assert_eq!(n("=EVEN(0)"), 0.0);
        assert_eq!(n("=ODD(1.5)"), 3.0);
        assert_eq!(n("=ODD(2)"), 3.0);
        assert_eq!(n("=ODD(-1.5)"), -3.0);
        assert_eq!(n("=ODD(0)"), 1.0);
        assert_eq!(n("=SIGN(-5)"), -1.0);
        assert_eq!(n("=SIGN(0)"), 0.0);
        assert_eq!(n("=SIGN(9.2)"), 1.0);
        assert_eq!(n("=QUOTIENT(5,2)"), 2.0);
        assert_eq!(n("=QUOTIENT(-5,2)"), -2.0);
        assert_eq!(n("=QUOTIENT(4.5,2)"), 2.0);
        assert_eq!(e("=QUOTIENT(5,0)"), CalcError::Div0);
    }

    // -- group 8: combinatorics ----------------------------------------------

    #[test]
    fn combinatorics() {
        assert_eq!(n("=FACT(5)"), 120.0);
        assert_eq!(n("=FACT(0)"), 1.0);
        assert_eq!(n("=FACT(5.5)"), 120.0);
        assert_eq!(e("=FACT(-1)"), CalcError::Num);
        assert_eq!(e("=FACT(171)"), CalcError::Num);
        assert_eq!(n("=FACTDOUBLE(6)"), 48.0);
        assert_eq!(n("=FACTDOUBLE(5)"), 15.0);
        assert_eq!(n("=FACTDOUBLE(0)"), 1.0);
        assert_eq!(n("=FACTDOUBLE(10)"), 3840.0);
        assert_eq!(e("=FACTDOUBLE(-2)"), CalcError::Num);
        assert_eq!(n("=COMBIN(10,3)"), 120.0);
        assert_eq!(n("=COMBIN(8,2)"), 28.0);
        assert_eq!(n("=COMBIN(0,0)"), 1.0);
        assert_eq!(e("=COMBIN(3,4)"), CalcError::Num);
        assert_eq!(n("=COMBINA(4,3)"), 20.0);
        assert_eq!(n("=COMBINA(10,3)"), 220.0);
        assert_eq!(n("=COMBINA(4,2)"), 10.0);
        assert_eq!(n("=COMBINA(0,0)"), 1.0);
        assert_eq!(e("=COMBINA(-1,2)"), CalcError::Num);
        assert_eq!(n("=PERMUT(10,3)"), 720.0);
        assert_eq!(n("=PERMUT(3,0)"), 1.0);
        assert_eq!(e("=PERMUT(2,3)"), CalcError::Num);
        assert_eq!(n("=PERMUTATIONA(10,3)"), 1000.0);
        assert_eq!(n("=PERMUTATIONA(0,0)"), 1.0);
        assert_eq!(e("=PERMUTATIONA(-1,2)"), CalcError::Num);
        assert_eq!(n("=MULTINOMIAL(2,3,4)"), 1260.0);
        assert_eq!(n("=MULTINOMIAL(1,1,1)"), 6.0);
        assert_eq!(e("=MULTINOMIAL(-1,2)"), CalcError::Num);
        assert_eq!(n("=GCD(24,36)"), 12.0);
        assert_eq!(n("=GCD(5,0)"), 5.0);
        assert_eq!(n("=GCD(0,0)"), 0.0);
        assert_eq!(n("=GCD(7)"), 7.0);
        assert_eq!(e("=GCD(-5,2)"), CalcError::Num);
        assert_eq!(n("=LCM(24,36)"), 72.0);
        assert_eq!(n("=LCM(5,2)"), 10.0);
        assert_eq!(n("=LCM(0,5)"), 0.0);
        assert_eq!(e("=LCM(-5,2)"), CalcError::Num);
    }

    // -- group 9: sums over ranges -------------------------------------------

    #[test]
    fn sumproduct_and_range_sums() {
        assert_eq!(n("=SUMPRODUCT({1,2,3},{1,2,3})"), 14.0);
        let g = Grid::empty()
            .col("A1", &[1.0, 2.0, 3.0])
            .col("B1", &[4.0, 5.0, 6.0]);
        assert_eq!(g.num("=SUMPRODUCT(A1:A3,B1:B3)"), 32.0);
        assert_eq!(g.num("=SUMPRODUCT(A1:A3)"), 6.0);
        assert_eq!(e("=SUMPRODUCT({1,2},{3,4,5})"), CalcError::Value);
        assert_eq!(n("=SUMSQ(3,4)"), 25.0);
        assert_eq!(n("=SUMSQ(3,4,5)"), 50.0);
        let g2 = Grid::empty().col("A1", &[1.0, 2.0]);
        assert_eq!(g2.num("=SUMSQ(A1:A2)"), 5.0);
        assert_eq!(n("=SUMX2MY2({2,3},{3,5})"), -21.0);
        assert_eq!(n("=SUMX2PY2({1,2},{3,4})"), 30.0);
        assert_eq!(n("=SUMXMY2({2,3},{3,5})"), 5.0);
        assert_eq!(e("=SUMXMY2({1,2},{3,4,5})"), CalcError::Value);
        assert_eq!(n("=SERIESSUM(2,1,1,{1,2,3})"), 34.0);
        assert_eq!(n("=SERIESSUM(3,2,0,{1,2})"), 27.0);
        assert_eq!(e("=SERIESSUM(2,1,1,{1,2;3,4})"), CalcError::Value);
    }

    // -- group 10: random numbers and bases ----------------------------------

    #[test]
    fn rand_and_randbetween() {
        let r1 = n("=RAND()");
        let r2 = n("=RAND()");
        assert!((0.0..1.0).contains(&r1));
        assert!((0.0..1.0).contains(&r2));
        assert_ne!(r1, r2, "RAND must differ between calls");
        let rb = n("=RANDBETWEEN(3,5)");
        assert!((3.0..=5.0).contains(&rb));
        assert_eq!(rb, rb.trunc());
        assert_eq!(e("=RANDBETWEEN(5,3)"), CalcError::Num);
    }

    #[test]
    fn roman_and_arabic() {
        assert_eq!(t("=ROMAN(499)"), "CDXCIX");
        assert_eq!(t("=ROMAN(499,0)"), "CDXCIX");
        assert_eq!(t("=ROMAN(499,1)"), "LDVLIV");
        assert_eq!(t("=ROMAN(499,2)"), "XDIX");
        assert_eq!(t("=ROMAN(499,3)"), "VDIV");
        assert_eq!(t("=ROMAN(499,4)"), "ID");
        assert_eq!(t("=ROMAN(0)"), "");
        assert_eq!(t("=ROMAN(255)"), "CCLV");
        assert_eq!(t("=ROMAN(2013)"), "MMXIII");
        assert_eq!(t("=ROMAN(3999)"), "MMMCMXCIX");
        assert_eq!(e("=ROMAN(4000)"), CalcError::Value);
        assert_eq!(e("=ROMAN(-1)"), CalcError::Value);
        assert_eq!(e("=ROMAN(1,5)"), CalcError::Value);
        assert_eq!(n("=ARABIC(\"MCMXII\")"), 1912.0);
        assert_eq!(n("=ARABIC(\"lvii\")"), 57.0);
        assert_eq!(n("=ARABIC(\"CDXCIX\")"), 499.0);
        assert_eq!(n("=ARABIC(\"CMXCIX\")"), 999.0);
        assert_eq!(n("=ARABIC(\" MCMXII \")"), 1912.0);
        assert_eq!(e("=ARABIC(\"MMMM\")"), CalcError::Value);
        assert_eq!(e("=ARABIC(\"xyz\")"), CalcError::Value);
        assert_eq!(e("=ARABIC(\"VX\")"), CalcError::Value);
        assert_eq!(e("=ARABIC(\"\")"), CalcError::Value);
    }

    #[test]
    fn base_and_decimal() {
        assert_eq!(t("=BASE(7,2)"), "111");
        assert_eq!(t("=BASE(13,16)"), "D");
        assert_eq!(t("=BASE(13,10)"), "13");
        assert_eq!(t("=BASE(3,2,4)"), "0011");
        assert_eq!(t("=BASE(10,16,4)"), "000A");
        assert_eq!(t("=BASE(0,2)"), "0");
        assert_eq!(e("=BASE(-1,2)"), CalcError::Num);
        assert_eq!(e("=BASE(1,1)"), CalcError::Num);
        assert_eq!(e("=BASE(1,37)"), CalcError::Num);
        assert_eq!(n("=DECIMAL(\"FF\",16)"), 255.0);
        assert_eq!(n("=DECIMAL(\"1101\",2)"), 13.0);
        assert_eq!(n("=DECIMAL(\"111\",2)"), 7.0);
        assert_eq!(n("=DECIMAL(\"ff\",16)"), 255.0);
        assert_eq!(e("=DECIMAL(\"FG\",16)"), CalcError::Num);
        assert_eq!(e("=DECIMAL(\"123\",2)"), CalcError::Num);
        assert_eq!(e("=DECIMAL(\"FF\",1)"), CalcError::Num);
        assert_eq!(e("=DECIMAL(\"-5\",10)"), CalcError::Num);
        assert_eq!(e("=DECIMAL(\"5.5\",10)"), CalcError::Num);
    }

    #[test]
    fn random_functions_are_volatile() {
        use crate::turbo::calc::functions::registry;
        for name in ["RAND", "RANDBETWEEN", "RANDARRAY"] {
            let spec = registry()
                .get(name)
                .unwrap_or_else(|| panic!("{name} not registered"));
            assert!(spec.volatile, "{name} must never be cached");
        }
    }

    // -- group 11: SUBTOTAL and AGGREGATE ------------------------------------

    #[test]
    fn subtotal_selects_aggregate() {
        let g = Grid::empty().col("A1", &[1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(g.num("=SUBTOTAL(9,A1:A5)"), 15.0);
        assert_eq!(g.num("=SUBTOTAL(109,A1:A5)"), 15.0);
        assert_eq!(g.num("=SUBTOTAL(1,A1:A5)"), 3.0);
        assert_eq!(g.num("=SUBTOTAL(2,A1:A5)"), 5.0);
        assert_eq!(g.num("=SUBTOTAL(3,A1:A5)"), 5.0);
        assert_eq!(g.num("=SUBTOTAL(4,A1:A5)"), 5.0);
        assert_eq!(g.num("=SUBTOTAL(5,A1:A5)"), 1.0);
        assert_eq!(g.num("=SUBTOTAL(6,A1:A5)"), 120.0);
        assert_eq!(g.num("=SUBTOTAL(102,A1:A5)"), 5.0);
        assert_eq!(g.num("=SUBTOTAL(105,A1:A5)"), 1.0);
        // 107 = STDEV.S (sample), 108 = STDEV.P (population).
        let stdev_s = g.num("=SUBTOTAL(107,A1:A5)");
        assert!(
            (stdev_s - 2.5f64.sqrt()).abs() < 1e-12,
            "STDEV.S over 1..5 = {stdev_s}"
        );
        let stdev_p = g.num("=SUBTOTAL(108,A1:A5)");
        assert!(
            (stdev_p - std::f64::consts::SQRT_2).abs() < 1e-12,
            "STDEV.P over 1..5 = {stdev_p}"
        );
        assert_eq!(e("=SUBTOTAL(12,A1:A5)"), CalcError::Value);
        let err = Grid::empty()
            .set_num("A1", 1.0)
            .set("A2", CalcValue::err(CalcError::Div0))
            .set_num("A3", 2.0);
        assert_eq!(err.error("=SUBTOTAL(9,A1:A3)"), CalcError::Div0);
    }

    #[test]
    fn aggregate_functions() {
        let g = Grid::empty().col("A1", &[1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(g.num("=AGGREGATE(9,0,A1:A5)"), 15.0);
        assert_eq!(g.num("=AGGREGATE(1,4,A1:A5)"), 3.0);
        assert_eq!(g.num("=AGGREGATE(2,0,A1:A5)"), 5.0);
        assert_eq!(g.num("=AGGREGATE(12,0,A1:A5)"), 3.0);
        assert_eq!(g.num("=AGGREGATE(4,0,A1:A5)"), 5.0);
        assert_eq!(g.num("=AGGREGATE(5,0,A1:A5)"), 1.0);
        let m = Grid::empty().col("A1", &[1.0, 2.0, 2.0, 3.0, 3.0, 3.0]);
        assert_eq!(m.num("=AGGREGATE(13,0,A1:A6)"), 3.0);
        assert_eq!(m.num("=AGGREGATE(14,0,A1:A6,2)"), 3.0);
        assert_eq!(m.num("=AGGREGATE(15,0,A1:A6,2)"), 2.0);
        assert_eq!(m.num("=AGGREGATE(16,0,A1:A6,0.5)"), 2.5);
        assert_eq!(m.num("=AGGREGATE(17,0,A1:A6,1)"), 2.0);
        assert_eq!(m.num("=AGGREGATE(18,0,A1:A6,0.25)"), 1.75);
        assert_eq!(m.num("=AGGREGATE(19,0,A1:A6,2)"), 2.5);
        let err = Grid::empty()
            .set_num("A1", 1.0)
            .set_num("A2", 2.0)
            .set("A3", CalcValue::err(CalcError::Div0))
            .set_num("A4", 3.0);
        assert_eq!(err.error("=AGGREGATE(9,0,A1:A4)"), CalcError::Div0);
        assert_eq!(err.error("=AGGREGATE(9,4,A1:A4)"), CalcError::Div0);
        assert_eq!(err.num("=AGGREGATE(9,2,A1:A4)"), 6.0);
        assert_eq!(err.num("=AGGREGATE(9,3,A1:A4)"), 6.0);
        assert_eq!(err.num("=AGGREGATE(9,6,A1:A4)"), 6.0);
        assert_eq!(err.num("=AGGREGATE(9,7,A1:A4)"), 6.0);
        assert_eq!(e("=AGGREGATE(0,0,A1:A4)"), CalcError::Value);
        assert_eq!(e("=AGGREGATE(9,8,A1:A4)"), CalcError::Value);
        assert_eq!(e("=AGGREGATE(14,0,A1:A6)"), CalcError::Value);
    }

    // -- group 12: matrix -----------------------------------------------------

    #[test]
    fn mmult_and_mdeterm() {
        assert_eq!(n("=MDETERM({1,2;3,4})"), -2.0);
        assert_eq!(n("=MDETERM({1,0;0,1})"), 1.0);
        assert_eq!(n("=MDETERM({1,2;2,4})"), 0.0);
        assert_eq!(e("=MDETERM({1,2,3;4,5,6})"), CalcError::Value);
        let m = Grid::empty().array("=MMULT({1,2;3,4},{5,6;7,8})");
        assert_eq!(m.shape(), (2, 2));
        assert_eq!(m.get(0, 0).as_number().unwrap(), 19.0);
        assert_eq!(m.get(0, 1).as_number().unwrap(), 22.0);
        assert_eq!(m.get(1, 0).as_number().unwrap(), 43.0);
        assert_eq!(m.get(1, 1).as_number().unwrap(), 50.0);
        assert_eq!(e("=MMULT({1,2},{3,4,5})"), CalcError::Value);
    }

    #[test]
    fn minverse_accuracy_and_errors() {
        let inv = Grid::empty().array("=MINVERSE({1,2;3,4})");
        assert_eq!(inv.shape(), (2, 2));
        let (a, b, c, d) = (
            inv.get(0, 0).as_number().unwrap(),
            inv.get(0, 1).as_number().unwrap(),
            inv.get(1, 0).as_number().unwrap(),
            inv.get(1, 1).as_number().unwrap(),
        );
        assert!((a - -2.0).abs() < 1e-12, "inverse(0,0) = {a}");
        assert!((b - 1.0).abs() < 1e-12, "inverse(0,1) = {b}");
        assert!((c - 1.5).abs() < 1e-12, "inverse(1,0) = {c}");
        assert!((d - -0.5).abs() < 1e-12, "inverse(1,1) = {d}");
        assert_eq!(e("=MINVERSE({1,2;2,4})"), CalcError::Num);
        assert_eq!(e("=MINVERSE({1,2,3})"), CalcError::Value);
    }

    #[test]
    fn transpose_and_munit() {
        let tr = Grid::empty().array("=TRANSPOSE({1,2;3,4})");
        assert_eq!(tr.shape(), (2, 2));
        assert_eq!(tr.get(0, 1).as_number().unwrap(), 3.0);
        assert_eq!(tr.get(1, 0).as_number().unwrap(), 2.0);
        let id = Grid::empty().array("=MUNIT(3)");
        assert_eq!(id.shape(), (3, 3));
        assert_eq!(id.get(0, 0).as_number().unwrap(), 1.0);
        assert_eq!(id.get(0, 1).as_number().unwrap(), 0.0);
        assert_eq!(id.get(2, 2).as_number().unwrap(), 1.0);
        assert_eq!(e("=MUNIT(0)"), CalcError::Value);
    }

    #[test]
    fn every_math_gap_function_is_registered() {
        use crate::turbo::calc::functions::registry;
        let non_volatile = [
            "PI",
            "EXP",
            "LN",
            "LOG",
            "LOG10",
            "SIN",
            "COS",
            "TAN",
            "CSC",
            "SEC",
            "COT",
            "ASIN",
            "ACOS",
            "ATAN",
            "ATAN2",
            "ACOT",
            "SINH",
            "COSH",
            "TANH",
            "CSCH",
            "SECH",
            "COTH",
            "ASINH",
            "ACOSH",
            "ATANH",
            "ACOTH",
            "DEGREES",
            "RADIANS",
            "CEILING",
            "CEILING.MATH",
            "CEILING.PRECISE",
            "FLOOR",
            "FLOOR.MATH",
            "FLOOR.PRECISE",
            "MROUND",
            "TRUNC",
            "EVEN",
            "ODD",
            "SIGN",
            "QUOTIENT",
            "ISO.CEILING",
            "FACT",
            "FACTDOUBLE",
            "COMBIN",
            "COMBINA",
            "PERMUT",
            "PERMUTATIONA",
            "MULTINOMIAL",
            "GCD",
            "LCM",
            "SUMPRODUCT",
            "SUMSQ",
            "SUMX2MY2",
            "SUMX2PY2",
            "SUMXMY2",
            "SERIESSUM",
            "ROMAN",
            "ARABIC",
            "BASE",
            "DECIMAL",
            "SUBTOTAL",
            "AGGREGATE",
            "MMULT",
            "MINVERSE",
            "MDETERM",
            "TRANSPOSE",
            "MUNIT",
        ];
        for name in non_volatile {
            let spec = registry()
                .get(name)
                .unwrap_or_else(|| panic!("{name} not registered"));
            assert!(!spec.volatile, "{name} must not be volatile");
        }
        for name in [
            "MULTINOMIAL",
            "GCD",
            "LCM",
            "SUMPRODUCT",
            "SUMSQ",
            "SUMX2MY2",
            "SUMX2PY2",
            "SUMXMY2",
            "SERIESSUM",
            "MMULT",
            "MINVERSE",
            "MDETERM",
            "TRANSPOSE",
            "MUNIT",
        ] {
            assert!(
                registry().get(name).unwrap().array_aware,
                "{name} must be array-aware"
            );
        }
    }
}
