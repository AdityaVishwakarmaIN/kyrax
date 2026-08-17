// functions/statistical.rs — the statistical function family. Owned
// exclusively by the statistical family agent; no other agent edits this file.
//
// Registry contract: implement `register` below and keep this exact signature.
// Do NOT edit functions/mod.rs — the `mod statistical;` declaration and the
// `statistical::register(&mut r)` call site in `build()` are already final.
// See functions/mod.rs for the worked ABS template.
//
// AVERAGE / COUNT / COUNTA / MIN / MAX / COUNTIF / COUNTIFS / AVERAGEIF /
// AVERAGEIFS already live in the math family (functions/math.rs), so they are
// NOT re-registered here. Criteria matching for MAXIFS/MINIFS is a private
// helper in this file (no cross-family imports).
//
// Numerical notes (accuracy is the hard part of this family):
//   * Variance/stddev use Welford's online algorithm (m2 += delta*(x-mean)),
//     never sum-of-squares minus square-of-sum, which cancels catastrophically
//     for values around 1e6 with a spread of 1.
//   * GAMMALN is the Lanczos approximation, g=7, n=9 (Pugh's coefficients, as
//     published in Numerical Recipes / Wikipedia). Relative error < ~1e-14 for
//     x >= 1 and ~1e-13 near 0.5 after reflection.
//   * NORM.S.INV is Wichura's AS241 rational approximation plus one Acklam
//     Newton refinement step. Max absolute error < 1.15e-9 over the whole
//     range; the refinement pushes it to ~1e-15 near the centre.
//   * The regularized incomplete gamma (series + continued fraction) and the
//     regularized incomplete beta (continued fraction) each converge to a
//     relative EPS of 1e-14 with an iteration cap of 10_000 — the same pair of
//     helpers backs CHISQ, T, F, BETA and the erf/erfc used by the normal CDF.
//     Reported distribution values are accurate to better than 1e-9 relative
//     for the parameter ranges in real workbooks (df < 1e6).
//   * The .RT (upper-tail) functions are computed directly from the
//     complementary incomplete gamma/beta, never as 1 - cdf, which would round
//     the answer to 0 in exactly the far tail where it matters.
//   * All inverse functions bisect a monotone CDF: invert_from_zero (widening
//     [0, hi] then 100 halvings) for the gamma-backed ones, and invert_beta
//     (100 halvings of [0,1]) for the beta-backed ones (F.INV, T.INV.2T,
//     BETA.INV). Both converge the argument to ~1e-14 relative — far past the
//     1e-9 threshold that is shippable.
//
//     Do not misread the "1e-4/1e-5" figures that Excel help-page comparisons
//     produce: those pages PRINT their worked examples to four or five
//     decimals, so agreement with them is capped by the page, not by us.
//     Measured against scipy in double precision, these inverses are accurate
//     to 1e-14..1e-16 relative — see `coordinator_accuracy` below, which pins
//     F.INV, T.INV.2T, GAMMA.INV and CHISQ.INV at a 1e-9 tolerance against
//     independently computed reference values.
//   * LINEST/LOGEST fit by modified Gram-Schmidt QR of the design matrix —
//     stable against near-collinear predictors that break normal equations —
//     and report standard errors from the diagonal of (X'X)^{-1} = (R'R)^{-1}.
//     TREND/GROWTH reuse the same fit; GROWTH fits ln(y).
use super::{FuncArg, FuncCtx, FuncSpec, Registry};
use crate::turbo::calc::coerce::{coerce_number, coerce_text, compare, compare_eq};
use crate::turbo::calc::value::{ArrayValue, CalcError, CalcValue};
use std::cmp::Ordering;

const PI: f64 = 3.14159265358979323846;
const SQRT2: f64 = 1.41421356237309504880;
const SQRT_2PI: f64 = 2.50662827463100050242;

fn ok_num(n: f64) -> Result<CalcValue, CalcError> {
    if n.is_finite() {
        Ok(CalcValue::Number(n))
    } else {
        Err(CalcError::Num)
    }
}

// -- special functions --------------------------------------------------------

/// Lanczos approximation to ln Γ(x), g=7, n=9 (Pugh's coefficients).
fn gammaln(x: f64) -> f64 {
    if x <= 0.0 {
        return f64::NAN;
    }
    if x < 0.5 {
        // reflection: ln Γ(x) = ln π − ln Γ(1−x) − ln|sin πx|
        return PI.ln() - gammaln(1.0 - x) - (PI * x).sin().abs().ln();
    }
    const P: [f64; 9] = [
        0.99999999999980993,
        676.5203681218851,
        -1259.1392167224028,
        771.32342877765313,
        -176.61502916214059,
        12.507343278686905,
        -0.13857109526572012,
        9.9843695780195716e-6,
        1.5056327351493116e-7,
    ];
    let z = x - 1.0;
    let mut a = P[0];
    for i in 1..9 {
        a += P[i] / (z + i as f64);
    }
    let t = z + 7.5;
    0.5 * (2.0 * PI).ln() + (z + 0.5) * t.ln() - t + a.ln()
}

const GAMMA_EPS: f64 = 1e-14;
const GAMMA_MAXIT: usize = 10_000;
const FPMIN: f64 = 1e-300;

/// Regularized lower incomplete gamma P(a,x) = γ(a,x)/Γ(a), series form.
fn gamma_p_series(a: f64, x: f64) -> f64 {
    let gln = gammaln(a);
    let mut ap = a;
    let mut sum = 1.0 / a;
    let mut del = sum;
    for _ in 0..GAMMA_MAXIT {
        ap += 1.0;
        del *= x / ap;
        sum += del;
        if del.abs() < sum.abs() * GAMMA_EPS {
            break;
        }
    }
    sum * (-x + a * x.ln() - gln).exp()
}

/// Regularized upper incomplete gamma Q(a,x), continued-fraction form.
fn gamma_q_cf(a: f64, x: f64) -> f64 {
    let gln = gammaln(a);
    let mut b = x + 1.0 - a;
    let mut c = 1.0 / FPMIN;
    let mut d = 1.0 / b;
    let mut h = d;
    for i in 1..=GAMMA_MAXIT {
        let an = -(i as f64) * (i as f64 - a);
        b += 2.0;
        d = an * d + b;
        if d.abs() < FPMIN {
            d = FPMIN;
        }
        c = b + an / c;
        if c.abs() < FPMIN {
            c = FPMIN;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < GAMMA_EPS {
            break;
        }
    }
    (-x + a * x.ln() - gln).exp() * h
}

/// Regularized lower incomplete gamma P(a,x).
fn gamma_p(a: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x < a + 1.0 {
        gamma_p_series(a, x)
    } else {
        1.0 - gamma_q_cf(a, x)
    }
}

/// Regularized upper incomplete gamma Q(a,x).
fn gamma_q(a: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 1.0;
    }
    if x < a + 1.0 {
        1.0 - gamma_p_series(a, x)
    } else {
        gamma_q_cf(a, x)
    }
}

/// Lentz continued fraction for the incomplete beta.
fn beta_cf(a: f64, b: f64, x: f64) -> f64 {
    let qab = a + b;
    let qap = a + 1.0;
    let qam = a - 1.0;
    let mut c = 1.0;
    let mut d = 1.0 - qab * x / qap;
    if d.abs() < FPMIN {
        d = FPMIN;
    }
    d = 1.0 / d;
    let mut h = d;
    for m in 1..=GAMMA_MAXIT {
        let m2 = 2.0 * (m as f64);
        let mut aa = (m as f64) * (b - m as f64) * x / ((qam + m2) * (a + m2));
        d = 1.0 + aa * d;
        if d.abs() < FPMIN {
            d = FPMIN;
        }
        c = 1.0 + aa / c;
        if c.abs() < FPMIN {
            c = FPMIN;
        }
        d = 1.0 / d;
        h *= d * c;
        aa = -(a + m as f64) * (qab + m as f64) * x / ((a + m2) * (qap + m2));
        d = 1.0 + aa * d;
        if d.abs() < FPMIN {
            d = FPMIN;
        }
        c = 1.0 + aa / c;
        if c.abs() < FPMIN {
            c = FPMIN;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < GAMMA_EPS {
            break;
        }
    }
    h
}

/// Regularized incomplete beta I_x(a,b), 0 <= x <= 1.
fn beta_cont(a: f64, b: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let lnbt = gammaln(a + b) - gammaln(a) - gammaln(b) + a * x.ln() + b * (1.0 - x).ln();
    let bt = lnbt.exp();
    if x < (a + 1.0) / (a + b + 2.0) {
        bt * beta_cf(a, b, x) / a
    } else {
        1.0 - bt * beta_cf(b, a, 1.0 - x) / b
    }
}

fn erfc(x: f64) -> f64 {
    if x >= 0.0 {
        gamma_q(0.5, x * x)
    } else {
        2.0 - gamma_q(0.5, x * x)
    }
}

fn norm_s_cdf(z: f64) -> f64 {
    0.5 * erfc(-z / SQRT2)
}

fn norm_s_pdf(z: f64) -> f64 {
    (-z * z / 2.0).exp() / SQRT_2PI
}

/// Standard normal quantile, Wichura's AS241 rational approximation plus one
/// Acklam Newton refinement step. Assumes 0 < p < 1 (callers validate).
fn norm_s_inv(p: f64) -> f64 {
    const A: [f64; 6] = [
        -3.969683028665376e+01,
        2.209460984245205e+02,
        -2.759285104469687e+02,
        1.383577518672690e+02,
        -3.066479806614716e+01,
        2.506628277459239e+00,
    ];
    const B: [f64; 5] = [
        -5.447609879822406e+01,
        1.615858368580409e+02,
        -1.556989798598866e+02,
        6.680131188771972e+01,
        -1.328068155288572e+01,
    ];
    const C: [f64; 6] = [
        -7.784894002430293e-03,
        -3.223964580411365e-01,
        -2.400758277161838e+00,
        -2.549732539343734e+00,
        4.374664141464968e+00,
        2.938163982698783e+00,
    ];
    const D: [f64; 4] = [
        7.784695709041462e-03,
        3.224671290700398e-01,
        2.445134137142996e+00,
        3.754408661907416e+00,
    ];
    let mut x;
    if p < 0.02425 {
        let q = (-2.0 * p.ln()).sqrt();
        x = (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0);
    } else if p <= 0.97575 {
        let q = p - 0.5;
        let r = q * q;
        x = (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0);
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        x = -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0);
    }
    // Acklam's refinement step: x ← x − u/(1 + x·u/2), u = (Φ(x)−p)·√(2π)·e^(x²/2).
    let e = norm_s_cdf(x) - p;
    let u = e * SQRT_2PI * (x * x / 2.0).exp();
    if u.is_finite() {
        x = x - u / (1.0 + x * u / 2.0);
    }
    x
}

/// Invert a monotone CDF by bisection over [0, hi], widening hi until it
/// brackets the target. ~100 halvings give far more than 1e-9 precision.
fn invert_from_zero(p: f64, hi0: f64, f: &dyn Fn(f64) -> f64) -> f64 {
    let mut lo = 0.0;
    let mut hi = hi0.max(1.0);
    let mut fhi = f(hi);
    let mut guard = 0;
    while fhi < p && guard < 64 {
        hi *= 2.0;
        fhi = f(hi);
        guard += 1;
    }
    for _ in 0..100 {
        let mid = 0.5 * (lo + hi);
        if f(mid) < p {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

// -- argument collection ------------------------------------------------------

/// Whether an aggregate counts TRUE/FALSE/text cells in ranges, or only numbers.
#[derive(Clone, Copy, PartialEq)]
enum Agg {
    Numbers,
    All,
}

/// Collect the numeric arguments, mirroring the math family's AVERAGE/COUNT
/// coercion contract: direct scalar arguments are coerced, array/range
/// elements are filtered by `agg`.
fn collect_numbers(ctx: &FuncCtx, args: &[FuncArg], agg: Agg) -> Result<Vec<f64>, CalcError> {
    let mut out = Vec::new();
    for arg in args {
        match arg.value(ctx)? {
            CalcValue::Array(a) => {
                for v in a.iter() {
                    match v {
                        CalcValue::Number(n) => out.push(*n),
                        CalcValue::Bool(b) if agg == Agg::All => {
                            out.push(if *b { 1.0 } else { 0.0 })
                        }
                        CalcValue::Text(_) if agg == Agg::All => out.push(0.0),
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

/// A single argument as a dense array (scalars become 1x1).
fn range_array(ctx: &FuncCtx, arg: &FuncArg) -> Result<ArrayValue, CalcError> {
    match arg.value(ctx)? {
        CalcValue::Array(a) => Ok((*a).clone()),
        v => Ok(ArrayValue::new(1, 1, vec![v])),
    }
}

/// Like [`range_array`], but a scalar *error* argument propagates immediately
/// instead of being wrapped into a 1x1 array — the hypothesis-test family must
/// surface `COVARIANCE.P(#NAME?, ...) -> #NAME?`, not a shape-driven #N/A.
fn array_arg(ctx: &FuncCtx, arg: &FuncArg) -> Result<ArrayValue, CalcError> {
    let v = arg.value(ctx)?;
    if let CalcValue::Error(e) = &v {
        return Err(*e);
    }
    Ok(match v {
        CalcValue::Array(a) => (*a).clone(),
        v => ArrayValue::new(1, 1, vec![v]),
    })
}

/// The numeric elements of an array value (text/blank ignored, errors
/// propagate); a scalar coerces to a single-element list.
fn array_numbers(v: &CalcValue) -> Result<Vec<f64>, CalcError> {
    match v {
        CalcValue::Array(a) => {
            let mut out = Vec::new();
            for x in a.iter() {
                match x {
                    CalcValue::Number(n) => out.push(*n),
                    CalcValue::Error(e) => return Err(*e),
                    _ => {}
                }
            }
            Ok(out)
        }
        v => Ok(vec![coerce_number(v)?]),
    }
}

/// Welford's online moments: numerically stable variance for values whose mean
/// dwarfs their spread (financial data). Never sum-of-squares minus mean².
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

fn sorted(mut v: Vec<f64>) -> Vec<f64> {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    v
}

// -- criteria matching (private to this file; math.rs's are not reusable) ----

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
    if let CalcValue::Text(t) = &crit.operand {
        if t.is_empty() {
            return match crit.op {
                Op::Eq => Ok(v.is_blank() || coerce_text(v)?.is_empty()),
                Op::Ne => Ok(!v.is_blank() && !coerce_text(v)?.is_empty()),
                _ => Ok(false),
            };
        }
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

/// Scan MAXIFS/MINIFS-style args: value range + (criteria_range, criterion)*.
fn ifs_scan(
    ctx: &FuncCtx,
    args: &[FuncArg],
) -> Result<(ArrayValue, Vec<(ArrayValue, Criterion)>), CalcError> {
    let values = range_array(ctx, &args[0])?;
    if (args.len() - 1) % 2 != 0 {
        return Err(CalcError::Value);
    }
    let mut pairs = Vec::new();
    let mut i = 1;
    while i + 1 < args.len() {
        let cr = range_array(ctx, &args[i])?;
        if cr.shape() != values.shape() {
            return Err(CalcError::Value);
        }
        let crit = parse_criteria(&args[i + 1].value(ctx)?)?;
        pairs.push((cr, crit));
        i += 2;
    }
    Ok((values, pairs))
}

// -- group 1: core aggregates ------------------------------------------------

fn averagea(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let v = collect_numbers(ctx, args, Agg::All)?;
    if v.is_empty() {
        return Err(CalcError::Div0);
    }
    let sum: f64 = v.iter().sum();
    ok_num(sum / v.len() as f64)
}

fn countblank(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let mut n = 0usize;
    for arg in args {
        match arg.value(ctx)? {
            CalcValue::Array(a) => {
                for v in a.iter() {
                    if v.is_blank() || matches!(v, CalcValue::Text(t) if t.is_empty()) {
                        n += 1;
                    }
                }
            }
            v => {
                // A literal error argument propagates (Excel: COUNTBLANK(#REF!)
                // -> #REF!); an error *inside a reference* is a value, not blank.
                if arg.as_reference().is_none() {
                    if let CalcValue::Error(e) = &v {
                        return Err(*e);
                    }
                }
                if v.is_blank() || matches!(&v, CalcValue::Text(t) if t.is_empty()) {
                    n += 1;
                }
            }
        }
    }
    Ok(CalcValue::Number(n as f64))
}

fn maxa(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let v = collect_numbers(ctx, args, Agg::All)?;
    Ok(CalcValue::Number(
        v.into_iter()
            .fold(None, |b: Option<f64>, x| Some(b.map_or(x, |b| b.max(x))))
            .unwrap_or(0.0),
    ))
}

fn mina(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let v = collect_numbers(ctx, args, Agg::All)?;
    Ok(CalcValue::Number(
        v.into_iter()
            .fold(None, |b: Option<f64>, x| Some(b.map_or(x, |b| b.min(x))))
            .unwrap_or(0.0),
    ))
}

fn maxifs(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let (values, pairs) = ifs_scan(ctx, args)?;
    let mut best: Option<f64> = None;
    for i in 0..values.data.len() {
        let mut matched = true;
        for (cr, crit) in &pairs {
            if !criteria_match(&cr.data[i], crit)? {
                matched = false;
                break;
            }
        }
        if matched {
            if let CalcValue::Number(n) = values.data[i] {
                best = Some(best.map_or(n, |b| b.max(n)));
            }
        }
    }
    Ok(CalcValue::Number(best.unwrap_or(0.0)))
}

fn minifs(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let (values, pairs) = ifs_scan(ctx, args)?;
    let mut best: Option<f64> = None;
    for i in 0..values.data.len() {
        let mut matched = true;
        for (cr, crit) in &pairs {
            if !criteria_match(&cr.data[i], crit)? {
                matched = false;
                break;
            }
        }
        if matched {
            if let CalcValue::Number(n) = values.data[i] {
                best = Some(best.map_or(n, |b| b.min(n)));
            }
        }
    }
    Ok(CalcValue::Number(best.unwrap_or(0.0)))
}

fn median(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let v = sorted(collect_numbers(ctx, args, Agg::Numbers)?);
    let n = v.len();
    if n == 0 {
        return Err(CalcError::Num);
    }
    if n % 2 == 1 {
        ok_num(v[n / 2])
    } else {
        ok_num(0.5 * (v[n / 2 - 1] + v[n / 2]))
    }
}

fn mode_sngl(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let v = collect_numbers(ctx, args, Agg::Numbers)?;
    match most_frequent(&v, false) {
        Some(list) if !list.is_empty() => ok_num(list[0]),
        _ => Err(CalcError::Na),
    }
}

fn mode_mult(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let v = collect_numbers(ctx, args, Agg::Numbers)?;
    let modes = most_frequent(&v, true);
    match modes {
        Some(list) if !list.is_empty() => Ok(CalcValue::array(ArrayValue::new(
            list.len() as u32,
            1,
            list.into_iter().map(CalcValue::Number).collect(),
        ))),
        _ => Err(CalcError::Na),
    }
}

/// Most frequent values. `first` selects first-appearance tie-break order;
/// `all` returns every mode (ascending value order), else just the first.
fn most_frequent(v: &[f64], all: bool) -> Option<Vec<f64>> {
    if v.is_empty() {
        return None;
    }
    let mut counts: Vec<(f64, usize)> = Vec::new();
    for &x in v {
        if let Some(slot) = counts.iter_mut().find(|(k, _)| k.to_bits() == x.to_bits()) {
            slot.1 += 1;
        } else {
            counts.push((x, 1));
        }
    }
    let max = counts.iter().map(|(_, c)| *c).max()?;
    if max < 2 {
        return None;
    }
    if !all {
        return counts
            .into_iter()
            .find(|(_, c)| *c == max)
            .map(|(x, _)| vec![x]);
    }
    let mut modes: Vec<f64> = counts
        .into_iter()
        .filter(|(_, c)| *c == max)
        .map(|(x, _)| x)
        .collect();
    modes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    Some(modes)
}

// -- group 2: spread ----------------------------------------------------------

fn stdev_sample(ctx: &FuncCtx, args: &[FuncArg], agg: Agg) -> Result<CalcValue, CalcError> {
    let v = collect_numbers(ctx, args, agg)?;
    if v.len() < 2 {
        return Err(CalcError::Div0);
    }
    let mut w = Welford::new();
    for x in &v {
        w.push(*x);
    }
    ok_num(w.var_sample().sqrt())
}

fn stdev_pop(ctx: &FuncCtx, args: &[FuncArg], agg: Agg) -> Result<CalcValue, CalcError> {
    let v = collect_numbers(ctx, args, agg)?;
    if v.is_empty() {
        return Err(CalcError::Div0);
    }
    let mut w = Welford::new();
    for x in &v {
        w.push(*x);
    }
    ok_num(w.var_pop().sqrt())
}

fn var_sample(ctx: &FuncCtx, args: &[FuncArg], agg: Agg) -> Result<CalcValue, CalcError> {
    let v = collect_numbers(ctx, args, agg)?;
    if v.len() < 2 {
        return Err(CalcError::Div0);
    }
    let mut w = Welford::new();
    for x in &v {
        w.push(*x);
    }
    ok_num(w.var_sample())
}

fn var_pop(ctx: &FuncCtx, args: &[FuncArg], agg: Agg) -> Result<CalcValue, CalcError> {
    let v = collect_numbers(ctx, args, agg)?;
    if v.is_empty() {
        return Err(CalcError::Div0);
    }
    let mut w = Welford::new();
    for x in &v {
        w.push(*x);
    }
    ok_num(w.var_pop())
}

fn stdev_s(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    stdev_sample(ctx, args, Agg::Numbers)
}
fn stdev_p(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    stdev_pop(ctx, args, Agg::Numbers)
}
fn stdeva(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    stdev_sample(ctx, args, Agg::All)
}
fn stdevpa(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    stdev_pop(ctx, args, Agg::All)
}
fn var_s(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    var_sample(ctx, args, Agg::Numbers)
}
fn var_p(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    var_pop(ctx, args, Agg::Numbers)
}
fn vara(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    var_sample(ctx, args, Agg::All)
}
fn varpa(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    var_pop(ctx, args, Agg::All)
}

fn avedev(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let v = collect_numbers(ctx, args, Agg::Numbers)?;
    let n = v.len();
    if n == 0 {
        return Err(CalcError::Div0);
    }
    let mean = v.iter().sum::<f64>() / n as f64;
    ok_num(v.iter().map(|x| (x - mean).abs()).sum::<f64>() / n as f64)
}

fn devsq(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let v = collect_numbers(ctx, args, Agg::Numbers)?;
    let n = v.len();
    if n == 0 {
        return Err(CalcError::Div0);
    }
    let mean = v.iter().sum::<f64>() / n as f64;
    ok_num(v.iter().map(|x| (x - mean) * (x - mean)).sum())
}

// -- group 3: rank and position ----------------------------------------------

fn large_small(ctx: &FuncCtx, args: &[FuncArg], largest: bool) -> Result<CalcValue, CalcError> {
    let v = sorted(collect_numbers(ctx, &args[..1], Agg::Numbers)?);
    // Excel rounds k to the nearest integer (LARGE(...,2.5) -> 3rd largest),
    // not truncates (which gave 2nd largest).
    let k = coerce_number(&args[1].value(ctx)?)?.round() as i64;
    if k <= 0 || (k as usize) > v.len() {
        return Err(CalcError::Num);
    }
    let idx = if largest {
        v.len() - k as usize
    } else {
        k as usize - 1
    };
    ok_num(v[idx])
}

fn large(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    large_small(ctx, args, true)
}
fn small(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    large_small(ctx, args, false)
}

fn rank(ctx: &FuncCtx, args: &[FuncArg], avg: bool) -> Result<CalcValue, CalcError> {
    let x = coerce_number(&args[0].value(ctx)?)?;
    let refs = collect_numbers(ctx, &args[1..2], Agg::Numbers)?;
    if !refs.iter().any(|r| r.to_bits() == x.to_bits()) {
        return Err(CalcError::Na);
    }
    let descending = if args.len() == 3 {
        coerce_number(&args[2].value(ctx)?)? == 0.0
    } else {
        true
    };
    let less = refs.iter().filter(|r| **r < x).count();
    let greater = refs.iter().filter(|r| **r > x).count();
    let equal = refs.iter().filter(|r| r.to_bits() == x.to_bits()).count();
    let base = if descending { greater } else { less };
    let rank = if avg {
        base as f64 + (equal as f64 + 1.0) / 2.0
    } else {
        (base + 1) as f64
    };
    ok_num(rank)
}

fn rank_eq(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    rank(ctx, args, false)
}
fn rank_avg(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    rank(ctx, args, true)
}

fn percentile_inc(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let v = sorted(collect_numbers(ctx, &args[..1], Agg::Numbers)?);
    let k = coerce_number(&args[1].value(ctx)?)?;
    let n = v.len();
    if n == 0 || !(0.0..=1.0).contains(&k) {
        return Err(CalcError::Num);
    }
    let pos = k * (n - 1) as f64;
    let lo = pos.floor() as usize;
    let frac = pos - pos.floor();
    if lo + 1 < n {
        ok_num(v[lo] + frac * (v[lo + 1] - v[lo]))
    } else {
        ok_num(v[lo])
    }
}

fn percentile_exc(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let v = sorted(collect_numbers(ctx, &args[..1], Agg::Numbers)?);
    let k = coerce_number(&args[1].value(ctx)?)?;
    let n = v.len();
    if n == 0 {
        return Err(CalcError::Num);
    }
    if k <= 1.0 / (n + 1) as f64 || k >= n as f64 / (n + 1) as f64 {
        return Err(CalcError::Num);
    }
    let pos = k * (n + 1) as f64;
    let lo = pos.floor() as usize - 1;
    let frac = pos - pos.floor();
    if lo + 1 < n {
        ok_num(v[lo] + frac * (v[lo + 1] - v[lo]))
    } else {
        ok_num(v[lo])
    }
}

fn quartile_inc(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let q = coerce_number(&args[1].value(ctx)?)?.trunc() as i64;
    if !(0..=4).contains(&q) {
        return Err(CalcError::Num);
    }
    percentile_inc(
        ctx,
        &[
            args[0].clone(),
            FuncArg::Value(CalcValue::Number(q as f64 / 4.0)),
        ],
    )
}

fn quartile_exc(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let q = coerce_number(&args[1].value(ctx)?)?.trunc() as i64;
    if !(1..=3).contains(&q) {
        return Err(CalcError::Num);
    }
    percentile_exc(
        ctx,
        &[
            args[0].clone(),
            FuncArg::Value(CalcValue::Number(q as f64 / 4.0)),
        ],
    )
}

fn percentrank(ctx: &FuncCtx, args: &[FuncArg], exc: bool) -> Result<CalcValue, CalcError> {
    let v = sorted(collect_numbers(ctx, &args[..1], Agg::Numbers)?);
    let x = coerce_number(&args[1].value(ctx)?)?;
    let sig = if args.len() == 3 {
        let a3 = &args[2];
        let v3 = a3.value(ctx)?;
        // Excel distinguishes an empty *cell reference* significance (→ #N/A)
        // from an omitted/trailing-comma argument (defaults to 3 digits).
        if a3.as_reference().is_some() && v3.is_blank() {
            return Err(CalcError::Na);
        }
        if v3.is_blank() {
            3
        } else {
            let s = coerce_number(&v3)?.trunc();
            if s < 1.0 {
                return Err(CalcError::Num);
            }
            s as i32
        }
    } else {
        3
    };
    let n = v.len();
    if n == 0 {
        return Err(CalcError::Num);
    }
    if x < v[0] || x > v[n - 1] {
        return Err(CalcError::Na);
    }
    let n1 = if exc { n + 1 } else { n - 1 };
    if n1 == 0 {
        return ok_num(0.0);
    }
    let mut rank = 0.0f64;
    // exact match at i -> (i + 1)/n1 for exc, i/(n2) for inc
    for i in 0..n {
        if v[i].to_bits() == x.to_bits() {
            let base = if exc { (i + 1) as f64 } else { i as f64 };
            rank = base / n1 as f64;
            let factor = 10f64.powi(sig);
            // Excel truncates to `sig` digits (0.255, not 0.256), never rounds.
            return ok_num((rank * factor).trunc() / factor);
        }
    }
    // interpolate between v[i] and v[i+1]
    for i in 0..n - 1 {
        if x > v[i] && x < v[i + 1] {
            let base = if exc { (i + 1) as f64 } else { i as f64 };
            rank = (base + (x - v[i]) / (v[i + 1] - v[i])) / n1 as f64;
            break;
        }
    }
    let factor = 10f64.powi(sig);
    ok_num((rank * factor).trunc() / factor)
}

fn percentrank_inc(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    percentrank(ctx, args, false)
}
fn percentrank_exc(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    percentrank(ctx, args, true)
}

fn trimmean(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let mut v = collect_numbers(ctx, &args[..1], Agg::Numbers)?;
    let p = coerce_number(&args[1].value(ctx)?)?;
    if !(0.0..=1.0).contains(&p) {
        return Err(CalcError::Num);
    }
    let n = v.len() as f64;
    let excluded = 2.0 * (n * p / 2.0).floor();
    let keep = v.len() as i64 - excluded as i64;
    if keep <= 0 {
        return Err(CalcError::Num);
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let drop = (excluded / 2.0) as usize;
    let slice = &v[drop..drop + keep as usize];
    ok_num(slice.iter().sum::<f64>() / slice.len() as f64)
}

// -- group 4: distributions ---------------------------------------------------

fn norm_dist(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let x = coerce_number(&args[0].value(ctx)?)?;
    let mean = coerce_number(&args[1].value(ctx)?)?;
    let sd = coerce_number(&args[2].value(ctx)?)?;
    let cum = coerce_number(&args[3].value(ctx)?)? != 0.0;
    if sd <= 0.0 {
        return Err(CalcError::Num);
    }
    let z = (x - mean) / sd;
    if cum {
        ok_num(norm_s_cdf(z))
    } else {
        ok_num(norm_s_pdf(z) / sd)
    }
}

fn norm_inv(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let p = coerce_number(&args[0].value(ctx)?)?;
    let mean = coerce_number(&args[1].value(ctx)?)?;
    let sd = coerce_number(&args[2].value(ctx)?)?;
    if p <= 0.0 || p >= 1.0 || sd <= 0.0 {
        return Err(CalcError::Num);
    }
    ok_num(mean + sd * norm_s_inv(p))
}

fn norm_s_dist(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let z = coerce_number(&args[0].value(ctx)?)?;
    let cum = coerce_number(&args[1].value(ctx)?)? != 0.0;
    if cum {
        ok_num(norm_s_cdf(z))
    } else {
        ok_num(norm_s_pdf(z))
    }
}

fn norm_s_inv_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let p = coerce_number(&args[0].value(ctx)?)?;
    if p <= 0.0 || p >= 1.0 {
        return Err(CalcError::Num);
    }
    ok_num(norm_s_inv(p))
}

fn binom_dist(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let k = coerce_number(&args[0].value(ctx)?)?.trunc();
    let n = coerce_number(&args[1].value(ctx)?)?.trunc();
    let p = coerce_number(&args[2].value(ctx)?)?;
    let cum = coerce_number(&args[3].value(ctx)?)? != 0.0;
    if k < 0.0 || k > n || p < 0.0 || p > 1.0 {
        return Err(CalcError::Num);
    }
    let k = k as i64;
    let n = n as i64;
    if cum {
        // P(X ≤ k) = I_{1−p}(n−k, k+1)
        if k >= n {
            return ok_num(1.0);
        }
        if p == 0.0 {
            return ok_num(1.0);
        }
        if p == 1.0 {
            return ok_num(if k >= n { 1.0 } else { 0.0 });
        }
        ok_num(beta_cont((n - k) as f64, (k + 1) as f64, 1.0 - p))
    } else {
        if p == 0.0 {
            return ok_num(if k == 0 { 1.0 } else { 0.0 });
        }
        if p == 1.0 {
            return ok_num(if k == n { 1.0 } else { 0.0 });
        }
        let lg =
            gammaln(n as f64 + 1.0) - gammaln(k as f64 + 1.0) - gammaln(n as f64 - k as f64 + 1.0);
        ok_num((lg + k as f64 * p.ln() + (n - k) as f64 * (1.0 - p).ln()).exp())
    }
}

fn poisson_dist(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let x = coerce_number(&args[0].value(ctx)?)?.trunc();
    let mean = coerce_number(&args[1].value(ctx)?)?;
    let cum = coerce_number(&args[2].value(ctx)?)? != 0.0;
    if x < 0.0 || mean < 0.0 {
        return Err(CalcError::Num);
    }
    let k = x as i64;
    if cum {
        // P(X ≤ k) = Q(k+1, λ)
        ok_num(gamma_q(k as f64 + 1.0, mean))
    } else {
        if mean == 0.0 {
            return ok_num(if k == 0 { 1.0 } else { 0.0 });
        }
        let lg = gammaln(k as f64 + 1.0);
        ok_num((-mean + k as f64 * mean.ln() - lg).exp())
    }
}

fn expon_dist(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let x = coerce_number(&args[0].value(ctx)?)?;
    let lambda = coerce_number(&args[1].value(ctx)?)?;
    let cum = coerce_number(&args[2].value(ctx)?)? != 0.0;
    if x < 0.0 || lambda <= 0.0 {
        return Err(CalcError::Num);
    }
    if cum {
        ok_num(1.0 - (-lambda * x).exp())
    } else {
        ok_num(lambda * (-lambda * x).exp())
    }
}

fn t_dist(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let x = coerce_number(&args[0].value(ctx)?)?;
    let df = coerce_number(&args[1].value(ctx)?)?;
    let cum = coerce_number(&args[2].value(ctx)?)? != 0.0;
    if df < 1.0 {
        return Err(CalcError::Num);
    }
    if cum {
        let u = df / (df + x * x);
        if x >= 0.0 {
            ok_num(1.0 - 0.5 * beta_cont(df / 2.0, 0.5, u))
        } else {
            ok_num(0.5 * beta_cont(df / 2.0, 0.5, u))
        }
    } else {
        let lg = gammaln((df + 1.0) / 2.0) - gammaln(df / 2.0) - 0.5 * (df * PI).ln();
        ok_num((lg - (df + 1.0) / 2.0 * (1.0 + x * x / df).ln()).exp())
    }
}

fn t_dist_2t(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let x = coerce_number(&args[0].value(ctx)?)?;
    let df = coerce_number(&args[1].value(ctx)?)?;
    if x < 0.0 || df < 1.0 {
        return Err(CalcError::Num);
    }
    let u = df / (df + x * x);
    ok_num(beta_cont(df / 2.0, 0.5, u))
}

fn t_inv(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let p = coerce_number(&args[0].value(ctx)?)?;
    let df = coerce_number(&args[1].value(ctx)?)?;
    if p <= 0.0 || p > 1.0 || df < 1.0 {
        return Err(CalcError::Num);
    }
    // The t-distribution is symmetric: use p' >= 0.5, then reflect.
    let p_hi = if p >= 0.5 { p } else { 1.0 - p };
    let f = |t: f64| -> f64 {
        let u = df / (df + t * t);
        1.0 - 0.5 * beta_cont(df / 2.0, 0.5, u)
    };
    let t = invert_from_zero(p_hi, 1.0, &f);
    ok_num(if p >= 0.5 { t } else { -t })
}

fn chisq_dist(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let x = coerce_number(&args[0].value(ctx)?)?;
    let df = coerce_number(&args[1].value(ctx)?)?;
    let cum = coerce_number(&args[2].value(ctx)?)? != 0.0;
    if x < 0.0 || df <= 0.0 {
        return Err(CalcError::Num);
    }
    if cum {
        ok_num(gamma_p(df / 2.0, x / 2.0))
    } else {
        let lg = (df / 2.0 - 1.0) * x.ln() - x / 2.0 - (df / 2.0) * 2.0f64.ln() - gammaln(df / 2.0);
        ok_num(lg.exp())
    }
}

fn chisq_inv(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let p = coerce_number(&args[0].value(ctx)?)?;
    let df = coerce_number(&args[1].value(ctx)?)?;
    if p <= 0.0 || p > 1.0 || df <= 0.0 {
        return Err(CalcError::Num);
    }
    let f = |x: f64| -> f64 { gamma_p(df / 2.0, x / 2.0) };
    let x = invert_from_zero(p, df, &f);
    ok_num(x)
}

fn f_dist(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let x = coerce_number(&args[0].value(ctx)?)?;
    let d1 = coerce_number(&args[1].value(ctx)?)?;
    let d2 = coerce_number(&args[2].value(ctx)?)?;
    let cum = coerce_number(&args[3].value(ctx)?)? != 0.0;
    if x < 0.0 || d1 <= 0.0 || d2 <= 0.0 {
        return Err(CalcError::Num);
    }
    if cum {
        let a = d1 / 2.0;
        let b = d2 / 2.0;
        ok_num(beta_cont(a, b, d1 * x / (d1 * x + d2)))
    } else {
        let a = d1 / 2.0;
        let b = d2 / 2.0;
        let lg = a * (d1 / d2).ln() + (a - 1.0) * x.ln()
            - (gammaln(a) + gammaln(b) - gammaln(a + b))
            - (a + b) * (1.0 + d1 * x / d2).ln();
        ok_num(lg.exp())
    }
}

/// Γ(x) for any x except the non-positive integers (poles → NaN). Negative
/// non-integers use the reflection formula Γ(x) = π / (sin πx · Γ(1−x));
/// Excel's GAMMA accepts them (e.g. Γ(−2.5) ≈ −0.9453).
fn gamma_val(x: f64) -> f64 {
    if x > 0.0 {
        gammaln(x).exp()
    } else if x.trunc() == x {
        // non-positive integers are poles (sin πx is ~1e-16, not exactly 0)
        f64::NAN
    } else {
        let s = (PI * x).sin();
        if s == 0.0 {
            f64::NAN
        } else {
            PI / (s * gamma_val(1.0 - x))
        }
    }
}

fn gamma_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let x = coerce_number(&args[0].value(ctx)?)?;
    ok_num(gamma_val(x))
}

fn gammaln_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let x = coerce_number(&args[0].value(ctx)?)?;
    if x <= 0.0 {
        return Err(CalcError::Num);
    }
    ok_num(gammaln(x))
}

fn beta_dist(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let x = coerce_number(&args[0].value(ctx)?)?;
    let alpha = coerce_number(&args[1].value(ctx)?)?;
    let beta = coerce_number(&args[2].value(ctx)?)?;
    let cum = coerce_number(&args[3].value(ctx)?)? != 0.0;
    let (a, b) = if args.len() >= 6 {
        (
            coerce_number(&args[4].value(ctx)?)?,
            coerce_number(&args[5].value(ctx)?)?,
        )
    } else {
        (0.0, 1.0)
    };
    if alpha <= 0.0 || beta <= 0.0 || a == b {
        return Err(CalcError::Num);
    }
    if x < a || x > b {
        return Err(CalcError::Num);
    }
    let z = (x - a) / (b - a);
    if cum {
        ok_num(beta_cont(alpha, beta, z))
    } else {
        let lg = (alpha - 1.0) * z.ln() + (beta - 1.0) * (1.0 - z).ln()
            - (gammaln(alpha) + gammaln(beta) - gammaln(alpha + beta))
            - (b - a).ln();
        ok_num(lg.exp())
    }
}

fn weibull_dist(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let x = coerce_number(&args[0].value(ctx)?)?;
    let alpha = coerce_number(&args[1].value(ctx)?)?;
    let beta = coerce_number(&args[2].value(ctx)?)?;
    let cum = coerce_number(&args[3].value(ctx)?)? != 0.0;
    if x < 0.0 || alpha <= 0.0 || beta <= 0.0 {
        return Err(CalcError::Num);
    }
    let t = (x / beta).powf(alpha);
    if cum {
        ok_num(1.0 - (-t).exp())
    } else {
        ok_num(alpha / beta * (x / beta).powf(alpha - 1.0) * (-t).exp())
    }
}

fn lognorm_dist(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let x = coerce_number(&args[0].value(ctx)?)?;
    let mean = coerce_number(&args[1].value(ctx)?)?;
    let sd = coerce_number(&args[2].value(ctx)?)?;
    let cum = coerce_number(&args[3].value(ctx)?)? != 0.0;
    if x <= 0.0 || sd <= 0.0 {
        return Err(CalcError::Num);
    }
    let z = (x.ln() - mean) / sd;
    if cum {
        ok_num(norm_s_cdf(z))
    } else {
        ok_num((-z * z / 2.0).exp() / (x * sd * SQRT_2PI))
    }
}

fn hypgeom_dist(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let s = coerce_number(&args[0].value(ctx)?)?.trunc() as i64;
    let n = coerce_number(&args[1].value(ctx)?)?.trunc() as i64;
    let k = coerce_number(&args[2].value(ctx)?)?.trunc() as i64;
    let n_pop = coerce_number(&args[3].value(ctx)?)?.trunc() as i64;
    let cum = coerce_number(&args[4].value(ctx)?)? != 0.0;
    if s < 0 || n < 0 || k < 0 || n_pop < 0 || s > n || k > n_pop || n > n_pop {
        return Err(CalcError::Num);
    }
    let pmf = |i: i64| -> f64 {
        if i < 0 || i > k || (n - i) < 0 || (n - i) > (n_pop - k) {
            return 0.0;
        }
        let lg = lcomb(k, i) + lcomb(n_pop - k, n - i) - lcomb(n_pop, n);
        lg.exp()
    };
    if cum {
        let mut total = 0.0;
        for i in 0..=s {
            total += pmf(i);
        }
        ok_num(total)
    } else {
        ok_num(pmf(s))
    }
}

fn lcomb(n: i64, k: i64) -> f64 {
    gammaln(n as f64 + 1.0) - gammaln(k as f64 + 1.0) - gammaln((n - k) as f64 + 1.0)
}

fn negbinom_dist(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let f = coerce_number(&args[0].value(ctx)?)?.trunc() as i64;
    let s = coerce_number(&args[1].value(ctx)?)?.trunc() as i64;
    let p = coerce_number(&args[2].value(ctx)?)?;
    let cum = coerce_number(&args[3].value(ctx)?)? != 0.0;
    if f < 0 || s < 0 || p < 0.0 || p > 1.0 {
        return Err(CalcError::Num);
    }
    if p == 0.0 {
        return ok_num(if cum {
            1.0
        } else if f == 0 {
            1.0
        } else {
            0.0
        });
    }
    if p == 1.0 {
        return ok_num(if cum {
            1.0
        } else if f == 0 {
            1.0
        } else {
            0.0
        });
    }
    let pmf = |i: i64| -> f64 {
        if i < 0 {
            return 0.0;
        }
        let lg = gammaln((i + s) as f64) - gammaln(i as f64 + 1.0) - gammaln(s as f64)
            + s as f64 * p.ln()
            + i as f64 * (1.0 - p).ln();
        lg.exp()
    };
    if cum {
        let mut total = 0.0;
        for i in 0..=f {
            total += pmf(i);
        }
        ok_num(total)
    } else {
        ok_num(pmf(f))
    }
}

// -- group 5: regression ------------------------------------------------------

/// Pair up two equal-shape arrays, keeping only (number, number) pairs.
fn pair_arrays(ctx: &FuncCtx, args: &[FuncArg]) -> Result<(Vec<f64>, Vec<f64>), CalcError> {
    let a1 = array_arg(ctx, &args[0])?;
    let a2 = array_arg(ctx, &args[1])?;
    if a1.shape() != a2.shape() {
        return Err(CalcError::Na);
    }
    let mut xs = Vec::new();
    let mut ys = Vec::new();
    for (x, y) in a1.iter().zip(a2.iter()) {
        match (x, y) {
            (CalcValue::Number(xn), CalcValue::Number(yn)) => {
                xs.push(*xn);
                ys.push(*yn);
            }
            (CalcValue::Error(e), _) | (_, CalcValue::Error(e)) => return Err(*e),
            _ => {}
        }
    }
    Ok((xs, ys))
}

struct LinReg {
    n: usize,
    mx: f64,
    my: f64,
    sxx: f64,
    syy: f64,
    sxy: f64,
}

fn linreg(xs: &[f64], ys: &[f64]) -> LinReg {
    let n = xs.len();
    let mx = xs.iter().sum::<f64>() / n as f64;
    let my = ys.iter().sum::<f64>() / n as f64;
    let mut sxx = 0.0;
    let mut syy = 0.0;
    let mut sxy = 0.0;
    for (x, y) in xs.iter().zip(ys.iter()) {
        let dx = x - mx;
        let dy = y - my;
        sxx += dx * dx;
        syy += dy * dy;
        sxy += dx * dy;
    }
    LinReg {
        n,
        mx,
        my,
        sxx,
        syy,
        sxy,
    }
}

fn slope_intercept(ctx: &FuncCtx, args: &[FuncArg]) -> Result<(f64, f64), CalcError> {
    // known_y's is args[0], known_x's is args[1]; x is the predictor.
    let (ys, xs) = pair_arrays(ctx, args)?;
    if xs.len() < 2 {
        return Err(CalcError::Div0);
    }
    let r = linreg(&xs, &ys);
    if r.sxx == 0.0 {
        return Err(CalcError::Div0);
    }
    let slope = r.sxy / r.sxx;
    let intercept = r.my - slope * r.mx;
    Ok((slope, intercept))
}

fn slope(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    ok_num(slope_intercept(ctx, args)?.0)
}
fn intercept(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    ok_num(slope_intercept(ctx, args)?.1)
}

fn correl(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let (xs, ys) = pair_arrays(ctx, args)?;
    if xs.len() < 2 {
        return Err(CalcError::Div0);
    }
    let r = linreg(&xs, &ys);
    if r.sxx == 0.0 || r.syy == 0.0 {
        return Err(CalcError::Div0);
    }
    ok_num(r.sxy / (r.sxx * r.syy).sqrt())
}
fn pearson(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    correl(ctx, args)
}
fn rsq(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let c = correl(ctx, args)?;
    if let CalcValue::Number(v) = c {
        ok_num(v * v)
    } else {
        Ok(c)
    }
}

fn steyx(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let (xs, ys) = pair_arrays(ctx, args)?;
    if xs.len() < 3 {
        return Err(CalcError::Div0);
    }
    let r = linreg(&xs, &ys);
    if r.sxx == 0.0 {
        return Err(CalcError::Div0);
    }
    let sse = r.syy - r.sxy * r.sxy / r.sxx;
    if sse < 0.0 {
        return Err(CalcError::Div0);
    }
    ok_num((sse / (r.n - 2) as f64).sqrt())
}

fn forecast_linear(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let x = coerce_number(&args[0].value(ctx)?)?;
    let (sl, ic) = slope_intercept(ctx, &args[1..])?;
    ok_num(sl * x + ic)
}

fn covariance(ctx: &FuncCtx, args: &[FuncArg], sample: bool) -> Result<CalcValue, CalcError> {
    let (xs, ys) = pair_arrays(ctx, args)?;
    if xs.is_empty() {
        // Referee (2026-08): no numeric pairs at all -> #VALUE! (Excel), not
        // #DIV/0!. The plan's "single pair -> 0" is wrong; a lone pair is
        // still under-determined and stays #DIV/0! below.
        return Err(CalcError::Value);
    }
    if xs.len() < 2 {
        return Err(CalcError::Div0);
    }
    let r = linreg(&xs, &ys);
    if sample {
        ok_num(r.sxy / (r.n - 1) as f64)
    } else {
        ok_num(r.sxy / r.n as f64)
    }
}

fn covar_p(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    covariance(ctx, args, false)
}
fn covar_s(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    covariance(ctx, args, true)
}

fn skew(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let v = collect_numbers(ctx, args, Agg::Numbers)?;
    let n = v.len();
    if n < 3 {
        return Err(CalcError::Div0);
    }
    let mut w = Welford::new();
    for x in &v {
        w.push(*x);
    }
    let s = w.var_sample().sqrt();
    if s == 0.0 {
        return Err(CalcError::Div0);
    }
    let m3 = v.iter().map(|x| ((x - w.mean) / s).powi(3)).sum::<f64>();
    ok_num(n as f64 / ((n - 1) as f64 * (n - 2) as f64) * m3)
}

fn kurt(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let v = collect_numbers(ctx, args, Agg::Numbers)?;
    let n = v.len();
    if n < 4 {
        return Err(CalcError::Div0);
    }
    let mut w = Welford::new();
    for x in &v {
        w.push(*x);
    }
    let s = w.var_sample().sqrt();
    if s == 0.0 {
        return Err(CalcError::Div0);
    }
    let m4 = v.iter().map(|x| ((x - w.mean) / s).powi(4)).sum::<f64>();
    let nf = n as f64;
    let a = nf * (nf + 1.0) / ((nf - 1.0) * (nf - 2.0) * (nf - 3.0)) * m4;
    let b = 3.0 * (nf - 1.0) * (nf - 1.0) / ((nf - 2.0) * (nf - 3.0));
    ok_num(a - b)
}

fn fisher(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let x = coerce_number(&args[0].value(ctx)?)?;
    if x <= -1.0 || x >= 1.0 {
        return Err(CalcError::Num);
    }
    ok_num(0.5 * ((1.0 + x) / (1.0 - x)).ln())
}

fn fisherinv(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let y = coerce_number(&args[0].value(ctx)?)?;
    // FISHERINV(y) = tanh(y) = (e^2y − 1)/(e^2y + 1). tanh saturates at ±1,
    // matching Excel for extreme arguments (FISHERINV(9999999.23658) -> 1);
    // the old exp-form overflowed to inf/inf = NaN.
    ok_num(y.tanh())
}

fn standardize(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let x = coerce_number(&args[0].value(ctx)?)?;
    let mean = coerce_number(&args[1].value(ctx)?)?;
    let sd = coerce_number(&args[2].value(ctx)?)?;
    if sd <= 0.0 {
        return Err(CalcError::Num);
    }
    ok_num((x - mean) / sd)
}

// -- group 6: means -----------------------------------------------------------

fn geomean(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let v = collect_numbers(ctx, args, Agg::Numbers)?;
    if v.is_empty() {
        return Err(CalcError::Num);
    }
    let mut s = 0.0;
    for &x in &v {
        if x <= 0.0 {
            return Err(CalcError::Num);
        }
        s += x.ln();
    }
    ok_num((s / v.len() as f64).exp())
}

fn harmean(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let v = collect_numbers(ctx, args, Agg::Numbers)?;
    if v.is_empty() {
        return Err(CalcError::Num);
    }
    let mut s = 0.0;
    for &x in &v {
        if x <= 0.0 {
            return Err(CalcError::Num);
        }
        s += 1.0 / x;
    }
    ok_num(v.len() as f64 / s)
}

// -- group 7: right tails and inverse distributions ---------------------------
//
// The .RT functions are the upper-tail probabilities hypothesis tests use. They
// are computed directly from the complementary incomplete gamma/beta (gamma_q /
// beta_cont on the far branch), never as `1 - cdf`, which would round the answer
// to 0 in exactly the far tail where it matters. The inverses bisect the CDF
// (100 halvings, far past 1e-9) — the same root-finder the existing CHISQ.INV
// and T.INV use.

/// Invert the regularized incomplete beta I_u(a,b) = p for u in [0,1] by
/// bisection. Absolute precision in u is ~1e-30 (limited only by the 1e-14
/// relative accuracy of beta_cont), which is far past shippable.
fn invert_beta(a: f64, b: f64, p: f64) -> f64 {
    let mut lo = 0.0;
    let mut hi = 1.0;
    for _ in 0..100 {
        let mid = 0.5 * (lo + hi);
        if beta_cont(a, b, mid) < p {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

fn chisq_dist_rt(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let x = coerce_number(&args[0].value(ctx)?)?;
    let df = coerce_number(&args[1].value(ctx)?)?.trunc();
    if x < 0.0 || df < 1.0 {
        return Err(CalcError::Num);
    }
    ok_num(gamma_q(df / 2.0, x / 2.0))
}

fn chisq_inv_rt(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let p = coerce_number(&args[0].value(ctx)?)?;
    let df = coerce_number(&args[1].value(ctx)?)?.trunc();
    if p <= 0.0 || p > 1.0 || df < 1.0 {
        return Err(CalcError::Num);
    }
    let f = |x: f64| gamma_p(df / 2.0, x / 2.0);
    let x = invert_from_zero(1.0 - p, df, &f);
    ok_num(x)
}

fn f_dist_rt(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let x = coerce_number(&args[0].value(ctx)?)?;
    let d1 = coerce_number(&args[1].value(ctx)?)?;
    let d2 = coerce_number(&args[2].value(ctx)?)?;
    if x < 0.0 || d1 <= 0.0 || d2 <= 0.0 {
        return Err(CalcError::Num);
    }
    ok_num(beta_cont(d2 / 2.0, d1 / 2.0, d2 / (d1 * x + d2)))
}

fn f_inv(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let p = coerce_number(&args[0].value(ctx)?)?;
    let d1 = coerce_number(&args[1].value(ctx)?)?;
    let d2 = coerce_number(&args[2].value(ctx)?)?;
    if p < 0.0 || p > 1.0 || d1 <= 0.0 || d2 <= 0.0 {
        return Err(CalcError::Num);
    }
    // Referee (2026-08): F.INV with p = 0 (blank probability) is 0, not #NUM!.
    if p == 0.0 {
        return ok_num(0.0);
    }
    let u = invert_beta(d1 / 2.0, d2 / 2.0, p);
    ok_num(d2 * u / (d1 * (1.0 - u)))
}

fn f_inv_rt(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let p = coerce_number(&args[0].value(ctx)?)?;
    let d1 = coerce_number(&args[1].value(ctx)?)?;
    let d2 = coerce_number(&args[2].value(ctx)?)?;
    if p <= 0.0 || p > 1.0 || d1 <= 0.0 || d2 <= 0.0 {
        return Err(CalcError::Num);
    }
    // Referee (2026-08): F.INV.RT(TRUE,...) = F.INV(0,...) = 0. The bisection
    // would otherwise land on a tiny-but-nonzero value.
    if p == 1.0 {
        return ok_num(0.0);
    }
    let v = invert_beta(d2 / 2.0, d1 / 2.0, p);
    ok_num(d2 * (1.0 - v) / (d1 * v))
}

fn t_dist_rt(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let x = coerce_number(&args[0].value(ctx)?)?;
    let df = coerce_number(&args[1].value(ctx)?)?.trunc();
    if x < 0.0 || df < 1.0 {
        return Err(CalcError::Num);
    }
    let u = df / (df + x * x);
    ok_num(0.5 * beta_cont(df / 2.0, 0.5, u))
}

fn t_inv_2t_val(p: f64, df: f64) -> Result<f64, CalcError> {
    if p <= 0.0 || p > 1.0 || df < 1.0 {
        return Err(CalcError::Num);
    }
    // T.DIST.2T(t, df) = I_u(df/2, 1/2) with u = df/(df+t²); invert for u, then t.
    let u = invert_beta(df / 2.0, 0.5, p);
    Ok((df * (1.0 - u) / u).sqrt())
}

fn t_inv_2t(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let p = coerce_number(&args[0].value(ctx)?)?;
    let df = coerce_number(&args[1].value(ctx)?)?.trunc();
    ok_num(t_inv_2t_val(p, df)?)
}

fn binom_inv(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?.trunc();
    let p = coerce_number(&args[1].value(ctx)?)?;
    let alpha = coerce_number(&args[2].value(ctx)?)?;
    if n < 0.0 || !(0.0..=1.0).contains(&p) || alpha <= 0.0 || alpha >= 1.0 {
        return Err(CalcError::Num);
    }
    let n = n as i64;
    // P(X ≤ k) = I_{1−p}(n−k, k+1); binary search the smallest k with CDF ≥ alpha.
    let cdf = |k: i64| beta_cont((n - k) as f64, (k + 1) as f64, 1.0 - p);
    let mut lo = 0i64;
    let mut hi = n;
    while lo < hi {
        let mid = (lo + hi) / 2;
        if cdf(mid) < alpha {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    ok_num(lo as f64)
}

fn beta_inv(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let prob = coerce_number(&args[0].value(ctx)?)?;
    let alpha = coerce_number(&args[1].value(ctx)?)?;
    let beta = coerce_number(&args[2].value(ctx)?)?;
    let (a, b) = if args.len() >= 5 {
        (
            coerce_number(&args[3].value(ctx)?)?,
            coerce_number(&args[4].value(ctx)?)?,
        )
    } else {
        (0.0, 1.0)
    };
    if prob <= 0.0 || prob > 1.0 || alpha <= 0.0 || beta <= 0.0 || a == b {
        return Err(CalcError::Num);
    }
    let z = invert_beta(alpha, beta, prob);
    ok_num(a + (b - a) * z)
}

fn lognorm_inv(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let prob = coerce_number(&args[0].value(ctx)?)?;
    let mean = coerce_number(&args[1].value(ctx)?)?;
    let sd = coerce_number(&args[2].value(ctx)?)?;
    if prob <= 0.0 || prob >= 1.0 || sd <= 0.0 {
        return Err(CalcError::Num);
    }
    ok_num((mean + sd * norm_s_inv(prob)).exp())
}

fn gamma_dist(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let x = coerce_number(&args[0].value(ctx)?)?;
    let alpha = coerce_number(&args[1].value(ctx)?)?;
    let beta = coerce_number(&args[2].value(ctx)?)?;
    let cum = coerce_number(&args[3].value(ctx)?)? != 0.0;
    if x < 0.0 || alpha <= 0.0 || beta <= 0.0 {
        return Err(CalcError::Num);
    }
    if cum {
        ok_num(gamma_p(alpha, x / beta))
    } else if x == 0.0 {
        // x^(alpha−1): 0 for alpha>1, 1/beta for alpha=1, infinite for alpha<1.
        if alpha == 1.0 {
            ok_num(1.0 / beta)
        } else if alpha > 1.0 {
            ok_num(0.0)
        } else {
            Err(CalcError::Num)
        }
    } else {
        let lg = (alpha - 1.0) * x.ln() - x / beta - alpha * beta.ln() - gammaln(alpha);
        ok_num(lg.exp())
    }
}

fn gamma_inv(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let prob = coerce_number(&args[0].value(ctx)?)?;
    let alpha = coerce_number(&args[1].value(ctx)?)?;
    let beta = coerce_number(&args[2].value(ctx)?)?;
    if prob <= 0.0 || prob >= 1.0 || alpha <= 0.0 || beta <= 0.0 {
        return Err(CalcError::Num);
    }
    let f = |t: f64| gamma_p(alpha, t);
    let t = invert_from_zero(prob, alpha, &f);
    ok_num(beta * t)
}

fn binom_dist_range(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?.trunc() as i64;
    let p = coerce_number(&args[1].value(ctx)?)?;
    let s1 = coerce_number(&args[2].value(ctx)?)?.trunc() as i64;
    let s2 = if args.len() == 4 {
        coerce_number(&args[3].value(ctx)?)?.trunc() as i64
    } else {
        s1
    };
    if n < 0 || !(0.0..=1.0).contains(&p) || s1 < 0 || s1 > n || s2 < s1 || s2 > n {
        return Err(CalcError::Num);
    }
    let pmf = |k: i64| -> f64 {
        if k < 0 || k > n {
            return 0.0;
        }
        if p == 0.0 {
            return if k == 0 { 1.0 } else { 0.0 };
        }
        if p == 1.0 {
            return if k == n { 1.0 } else { 0.0 };
        }
        let lg =
            gammaln(n as f64 + 1.0) - gammaln(k as f64 + 1.0) - gammaln(n as f64 - k as f64 + 1.0)
                + k as f64 * p.ln()
                + (n - k) as f64 * (1.0 - p).ln();
        lg.exp()
    };
    let mut total = 0.0;
    for k in s1..=s2 {
        total += pmf(k);
    }
    ok_num(total)
}

// -- group 8: hypothesis tests ------------------------------------------------

fn chisq_test(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let actual = array_arg(ctx, &args[0])?;
    let expected = array_arg(ctx, &args[1])?;
    if actual.shape() != expected.shape() {
        return Err(CalcError::Na);
    }
    // Incoming Excel error values propagate before any other rule.
    for v in actual.iter().chain(expected.iter()) {
        if let CalcValue::Error(e) = v {
            return Err(*e);
        }
    }
    // Referee (2026-08): 1x1 or empty ranges have fewer than two data points
    // -> #VALUE! (not #N/A as Univer claims, not the old #NUM!).
    if actual.data.len() < 2 || expected.data.len() < 2 {
        return Err(CalcError::Value);
    }
    let (rows, cols) = actual.shape();
    let df = ((rows as i64) - 1) * ((cols as i64) - 1);
    if df < 1 {
        return Err(CalcError::Num);
    }
    let mut stat = 0.0;
    for (o, e) in actual.iter().zip(expected.iter()) {
        let on = coerce_number(o)?;
        let en = coerce_number(e)?;
        if en < 1.0 {
            return Err(CalcError::Num);
        }
        let d = on - en;
        stat += d * d / en;
    }
    ok_num(gamma_q(df as f64 / 2.0, stat / 2.0))
}

fn f_test(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let a1 = array_numbers(&args[0].value(ctx)?)?;
    let a2 = array_numbers(&args[1].value(ctx)?)?;
    if a1.len() < 2 || a2.len() < 2 {
        return Err(CalcError::Div0);
    }
    let mut w1 = Welford::new();
    for &x in &a1 {
        w1.push(x);
    }
    let mut w2 = Welford::new();
    for &x in &a2 {
        w2.push(x);
    }
    let v1 = w1.var_sample();
    let v2 = w2.var_sample();
    if v1 == 0.0 || v2 == 0.0 {
        return Err(CalcError::Div0);
    }
    let f = v1 / v2;
    let d1 = (a1.len() - 1) as f64;
    let d2 = (a2.len() - 1) as f64;
    // Two-sided F-test: 2 * min(P(F > f_obs), P(F <= f_obs)), both tails
    // computed directly (no `1 - cdf`) for far-tail precision.
    let p = if f >= 1.0 {
        2.0 * beta_cont(d2 / 2.0, d1 / 2.0, d2 / (d1 * f + d2))
    } else {
        2.0 * beta_cont(d1 / 2.0, d2 / 2.0, d1 * f / (d1 * f + d2))
    };
    ok_num(p.min(1.0))
}

fn t_test(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let tails = coerce_number(&args[2].value(ctx)?)?.trunc();
    let ty = coerce_number(&args[3].value(ctx)?)?.trunc();
    if tails != 1.0 && tails != 2.0 {
        return Err(CalcError::Num);
    }
    if ty != 1.0 && ty != 2.0 && ty != 3.0 {
        return Err(CalcError::Num);
    }
    let a1 = array_arg(ctx, &args[0])?;
    let a2 = array_arg(ctx, &args[1])?;
    let (t, df) = if ty == 1.0 {
        // Paired: differences within the pair.
        if a1.shape() != a2.shape() {
            return Err(CalcError::Na);
        }
        let mut diffs = Vec::new();
        for (x, y) in a1.iter().zip(a2.iter()) {
            match (x, y) {
                (CalcValue::Number(a), CalcValue::Number(b)) => diffs.push(a - b),
                (CalcValue::Error(e), _) | (_, CalcValue::Error(e)) => return Err(*e),
                _ => {}
            }
        }
        if diffs.is_empty() {
            return Err(CalcError::Na);
        }
        let n = diffs.len() as f64;
        let mean = diffs.iter().sum::<f64>() / n;
        let mut m2 = 0.0;
        for &d in &diffs {
            let dd = d - mean;
            m2 += dd * dd;
        }
        let sd = (m2 / (n - 1.0)).sqrt();
        if sd == 0.0 {
            return Err(CalcError::Div0);
        }
        (mean / (sd / n.sqrt()), n - 1.0)
    } else {
        let mut w1 = Welford::new();
        let mut w2 = Welford::new();
        for v in a1.iter() {
            match v {
                CalcValue::Number(x) => w1.push(*x),
                CalcValue::Error(e) => return Err(*e),
                _ => {}
            }
        }
        for v in a2.iter() {
            match v {
                CalcValue::Number(x) => w2.push(*x),
                CalcValue::Error(e) => return Err(*e),
                _ => {}
            }
        }
        let n1 = w1.n as f64;
        let n2 = w2.n as f64;
        if n1 < 2.0 || n2 < 2.0 {
            return Err(CalcError::Div0);
        }
        let v1 = w1.var_sample();
        let v2 = w2.var_sample();
        if ty == 2.0 {
            // Two-sample, pooled equal-variance t-test.
            let sp2 = ((n1 - 1.0) * v1 + (n2 - 1.0) * v2) / (n1 + n2 - 2.0);
            if sp2 == 0.0 {
                return Err(CalcError::Div0);
            }
            let denom = sp2.sqrt() * (1.0 / n1 + 1.0 / n2).sqrt();
            if denom == 0.0 {
                return Err(CalcError::Div0);
            }
            ((w1.mean - w2.mean) / denom, n1 + n2 - 2.0)
        } else {
            // Welch: unequal variance, Welch-Satterthwaite df (NOT n1+n2-2).
            let se1 = v1 / n1;
            let se2 = v2 / n2;
            let denom = (se1 + se2).sqrt();
            if denom == 0.0 {
                return Err(CalcError::Div0);
            }
            let t = (w1.mean - w2.mean) / denom;
            let df = (se1 + se2) * (se1 + se2) / (se1 * se1 / (n1 - 1.0) + se2 * se2 / (n2 - 1.0));
            (t, df)
        }
    };
    if !df.is_finite() || df <= 0.0 {
        return Err(CalcError::Div0);
    }
    let u = df / (df + t * t);
    let twotail = beta_cont(df / 2.0, 0.5, u);
    ok_num(if tails == 2.0 { twotail } else { 0.5 * twotail })
}

fn z_test(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let vals = array_numbers(&args[0].value(ctx)?)?;
    if vals.is_empty() {
        return Err(CalcError::Na);
    }
    let x = coerce_number(&args[1].value(ctx)?)?;
    let n = vals.len() as f64;
    let mean = vals.iter().sum::<f64>() / n;
    let sigma = if args.len() == 3 {
        let s = coerce_number(&args[2].value(ctx)?)?;
        if s <= 0.0 {
            return Err(CalcError::Num);
        }
        s
    } else {
        if vals.len() < 2 {
            return Err(CalcError::Div0);
        }
        let mut w = Welford::new();
        for &v in &vals {
            w.push(v);
        }
        w.var_sample().sqrt()
    };
    let z = (mean - x) / (sigma / n.sqrt());
    ok_num(1.0 - norm_s_cdf(z))
}

// -- group 9: confidence intervals --------------------------------------------

fn confidence_norm(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let alpha = coerce_number(&args[0].value(ctx)?)?;
    let sd = coerce_number(&args[1].value(ctx)?)?;
    let size = coerce_number(&args[2].value(ctx)?)?.trunc();
    if alpha <= 0.0 || alpha >= 1.0 || sd <= 0.0 || size < 1.0 {
        return Err(CalcError::Num);
    }
    ok_num(norm_s_inv(1.0 - alpha / 2.0) * sd / size.sqrt())
}

fn confidence_t(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let alpha = coerce_number(&args[0].value(ctx)?)?;
    let sd = coerce_number(&args[1].value(ctx)?)?;
    let size = coerce_number(&args[2].value(ctx)?)?.trunc();
    if alpha <= 0.0 || alpha >= 1.0 || sd <= 0.0 || size < 1.0 {
        return Err(CalcError::Num);
    }
    let t = t_inv_2t_val(alpha, size - 1.0)?;
    ok_num(t * sd / size.sqrt())
}

// -- group 10: regression and forecasting (LINEST / LOGEST / TREND / GROWTH) --
//
// LINEST/LOGEST return full statistics arrays (5 rows) when `stats` is TRUE,
// in Excel's exact layout:
//   row 1: slopes (last predictor first) and intercept
//   row 2: standard errors of the same
//   row 3: R², SE_y
//   row 4: F, df
//   row 5: SS_reg, SS_resid
// with trailing #N/A cells in rows 3-5. With `const` FALSE there is no
// intercept column. Multiple regression (several predictor columns) is
// supported; the design matrix is fitted with modified Gram-Schmidt QR
// (numerically stable against the near-collinear predictors that break normal
// equations), and the SEs come from the diagonal of (X'X)^{-1} = (R'R)^{-1}.
// TREND/GROWTH reuse the same fit to extrapolate; GROWTH fits ln(y).

/// One observation: y with its k predictor values.
struct FitRows {
    y: Vec<f64>,
    x: Vec<Vec<f64>>,
    k: usize,
    y_was_col: bool,
}

/// Pull (y, x_1..x_k) observations out of LINEST-style arguments, honouring
/// Excel's layout rules: known_y's is one row or column; known_x's either the
/// same number of rows (one predictor per column) or, when known_y's is a row,
/// the same number of columns (one predictor per row). Rows where any cell is
/// non-numeric are dropped. When known_x's is omitted, x is 1..n.
fn fit_observations(ctx: &FuncCtx, args: &[FuncArg]) -> Result<FitRows, CalcError> {
    let y = range_array(ctx, &args[0])?;
    let (yr, yc) = y.shape();
    if yr > 1 && yc > 1 {
        return Err(CalcError::Ref);
    }
    let y_is_col = yc == 1;
    let n = if y_is_col { yr } else { yc } as usize;
    let yvals: Vec<Option<f64>> = if y_is_col {
        (0..yr).map(|r| y.get(r, 0).as_number()).collect()
    } else {
        (0..yc).map(|c| y.get(0, c).as_number()).collect()
    };
    if args.len() < 2 {
        let mut ys = Vec::new();
        let mut xs = Vec::new();
        let mut rank = 0.0f64;
        for yv in yvals {
            if let Some(v) = yv {
                rank += 1.0;
                ys.push(v);
                xs.push(vec![rank]);
            }
        }
        if ys.len() < 2 {
            return Err(CalcError::Ref);
        }
        return Ok(FitRows {
            y: ys,
            x: xs,
            k: 1,
            y_was_col: y_is_col,
        });
    }
    let xa = range_array(ctx, &args[1])?;
    let (xr0, xc0) = xa.shape();
    let xr = xr0 as usize;
    let xc = xc0 as usize;
    // known_x's either shares known_y's orientation (one predictor per column
    // when y is a column, per row when y is a row) or is a single row/column
    // vector for the single-predictor case.
    let (k, xvals): (usize, Vec<Vec<Option<f64>>>) = if y_is_col {
        if xr == n {
            (
                xc,
                (0..xc)
                    .map(|c| {
                        (0..xr)
                            .map(|r| xa.get(r as u32, c as u32).as_number())
                            .collect()
                    })
                    .collect(),
            )
        } else if xr == 1 && xc == n {
            (
                1,
                vec![(0..xc).map(|c| xa.get(0, c as u32).as_number()).collect()],
            )
        } else {
            return Err(CalcError::Ref);
        }
    } else if xc == n {
        (
            xr,
            (0..xr)
                .map(|r| {
                    (0..xc)
                        .map(|c| xa.get(r as u32, c as u32).as_number())
                        .collect()
                })
                .collect(),
        )
    } else if xc == 1 && xr == n {
        (
            1,
            vec![(0..xr).map(|r| xa.get(r as u32, 0).as_number()).collect()],
        )
    } else {
        return Err(CalcError::Ref);
    };
    let mut ys = Vec::new();
    let mut xs: Vec<Vec<f64>> = Vec::new();
    for i in 0..n {
        if let Some(yv) = yvals[i] {
            let mut row = Vec::with_capacity(k);
            let mut all = true;
            for j in 0..k {
                match xvals[j][i] {
                    Some(v) => row.push(v),
                    None => {
                        all = false;
                        break;
                    }
                }
            }
            if all {
                ys.push(yv);
                xs.push(row);
            }
        }
    }
    if ys.len() < 2 {
        return Err(CalcError::Ref);
    }
    Ok(FitRows {
        y: ys,
        x: xs,
        k,
        y_was_col: y_is_col,
    })
}

struct LinFit {
    k: usize,
    const_used: bool,
    coeffs: Vec<f64>,
    se: Vec<f64>,
    r2: f64,
    se_y: Option<f64>,
    f: Option<f64>,
    df: f64,
    ss_reg: f64,
    ss_resid: f64,
}

fn fit_least_squares(rows: &FitRows, use_const: bool) -> Result<LinFit, CalcError> {
    let n = rows.y.len();
    let k = rows.k;
    let p = k + if use_const { 1 } else { 0 };
    if n < p {
        return Err(CalcError::Ref);
    }
    // Design matrix D (n x p): predictors then, if const, a constant column.
    let mut d = vec![0.0f64; n * p];
    for i in 0..n {
        for j in 0..k {
            d[i * p + j] = rows.x[i][j];
        }
        if use_const {
            d[i * p + k] = 1.0;
        }
    }
    // Modified Gram-Schmidt QR of D.
    let mut q = vec![0.0f64; n * p];
    let mut r = vec![0.0f64; p * p];
    for j in 0..p {
        let mut v = vec![0.0f64; n];
        for i in 0..n {
            v[i] = d[i * p + j];
        }
        for i in 0..j {
            let mut dot = 0.0;
            for m in 0..n {
                dot += q[m * p + i] * v[m];
            }
            r[i * p + j] = dot;
            for m in 0..n {
                v[m] -= dot * q[m * p + i];
            }
        }
        let mut norm2 = 0.0;
        for m in 0..n {
            norm2 += v[m] * v[m];
        }
        if norm2 == 0.0 {
            return Err(CalcError::Num); // collinear predictor column
        }
        let norm = norm2.sqrt();
        r[j * p + j] = norm;
        for m in 0..n {
            q[m * p + j] = v[m] / norm;
        }
    }
    let mut qty = vec![0.0f64; p];
    for j in 0..p {
        for i in 0..n {
            qty[j] += q[i * p + j] * rows.y[i];
        }
    }
    let mut beta = vec![0.0f64; p];
    for j in (0..p).rev() {
        let mut s = qty[j];
        for m in (j + 1)..p {
            s -= r[j * p + m] * beta[m];
        }
        beta[j] = s / r[j * p + j];
    }
    // Residuals and sums of squares. With const the total SS is centred on the
    // mean of y; without, Excel's LINEST totals about zero.
    let mean_y = rows.y.iter().sum::<f64>() / n as f64;
    let mut ss_resid = 0.0;
    let mut ss_reg = 0.0;
    for i in 0..n {
        let mut yhat = 0.0;
        for j in 0..p {
            yhat += d[i * p + j] * beta[j];
        }
        let e = rows.y[i] - yhat;
        ss_resid += e * e;
        let dyh = if use_const { yhat - mean_y } else { yhat };
        ss_reg += dyh * dyh;
    }
    let ss_tot: f64 = if use_const {
        rows.y.iter().map(|y| (y - mean_y) * (y - mean_y)).sum()
    } else {
        rows.y.iter().map(|y| y * y).sum()
    };
    let df = (n - p) as f64;
    let r2 = if ss_tot == 0.0 { 0.0 } else { ss_reg / ss_tot };
    // Diagonal of (X'X)^{-1} = (R'R)^{-1} via the inverse of the triangular R.
    let mut rinv = vec![0.0f64; p * p];
    for j in 0..p {
        rinv[j * p + j] = 1.0 / r[j * p + j];
        for i in (0..j).rev() {
            let mut s = 0.0;
            for m in (i + 1)..=j {
                s += r[i * p + m] * rinv[m * p + j];
            }
            rinv[i * p + j] = -s / r[i * p + i];
        }
    }
    let mse = ss_resid / df;
    let mut se = Vec::with_capacity(p);
    for j in 0..p {
        let mut c = 0.0;
        for m in j..p {
            c += rinv[j * p + m] * rinv[j * p + m];
        }
        se.push(if df > 0.0 { (mse * c).sqrt() } else { f64::NAN });
    }
    let se_y = if df > 0.0 { Some(mse.sqrt()) } else { None };
    let f = if df > 0.0 && ss_resid > 0.0 {
        Some((ss_reg / k as f64) / mse)
    } else {
        None
    };
    Ok(LinFit {
        k,
        const_used: use_const,
        coeffs: beta,
        se,
        r2,
        se_y,
        f,
        df,
        ss_reg,
        ss_resid,
    })
}

fn linest_layout(fit: &LinFit, stats: bool) -> CalcValue {
    let p = fit.coeffs.len();
    let k = fit.k;
    let se_cell = |v: f64| {
        if v.is_nan() {
            CalcValue::err(CalcError::Div0)
        } else {
            CalcValue::Number(v)
        }
    };
    if !stats {
        let mut data = Vec::with_capacity(p);
        for j in 0..k {
            data.push(CalcValue::Number(fit.coeffs[k - 1 - j]));
        }
        if fit.const_used {
            data.push(CalcValue::Number(fit.coeffs[k]));
        }
        return CalcValue::array(ArrayValue::new(1, p as u32, data));
    }
    let mut data = vec![CalcValue::err(CalcError::Na); 5 * p];
    for j in 0..k {
        data[j] = CalcValue::Number(fit.coeffs[k - 1 - j]);
    }
    if fit.const_used {
        data[k] = CalcValue::Number(fit.coeffs[k]);
    }
    for j in 0..k {
        data[p + j] = se_cell(fit.se[k - 1 - j]);
    }
    if fit.const_used {
        data[p + k] = se_cell(fit.se[k]);
    }
    data[2 * p] = CalcValue::Number(fit.r2);
    if p > 1 {
        data[2 * p + 1] = match fit.se_y {
            Some(v) => CalcValue::Number(v),
            None => CalcValue::err(CalcError::Div0),
        };
    }
    data[3 * p] = match fit.f {
        Some(v) => CalcValue::Number(v),
        None => CalcValue::err(CalcError::Num),
    };
    if p > 1 {
        data[3 * p + 1] = CalcValue::Number(fit.df);
    }
    data[4 * p] = CalcValue::Number(fit.ss_reg);
    if p > 1 {
        data[4 * p + 1] = CalcValue::Number(fit.ss_resid);
    }
    CalcValue::array(ArrayValue::new(5, p as u32, data))
}

fn linest(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let rows = fit_observations(ctx, args)?;
    let use_const = if args.len() >= 3 {
        coerce_number(&args[2].value(ctx)?)? != 0.0
    } else {
        true
    };
    let stats = if args.len() >= 4 {
        coerce_number(&args[3].value(ctx)?)? != 0.0
    } else {
        false
    };
    let fit = fit_least_squares(&rows, use_const)?;
    Ok(linest_layout(&fit, stats))
}

fn logest(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let mut rows = fit_observations(ctx, args)?;
    let use_const = if args.len() >= 3 {
        coerce_number(&args[2].value(ctx)?)? != 0.0
    } else {
        true
    };
    let stats = if args.len() >= 4 {
        coerce_number(&args[3].value(ctx)?)? != 0.0
    } else {
        false
    };
    for v in &mut rows.y {
        if *v <= 0.0 {
            return Err(CalcError::Num);
        }
        *v = v.ln();
    }
    let fit = fit_least_squares(&rows, use_const)?;
    // Same layout as LINEST, but the intercept position holds b = e^(intercept).
    let p = fit.coeffs.len();
    let k = fit.k;
    let se_cell = |v: f64| {
        if v.is_nan() {
            CalcValue::err(CalcError::Div0)
        } else {
            CalcValue::Number(v)
        }
    };
    if !stats {
        let mut data = Vec::with_capacity(p);
        for j in 0..k {
            data.push(CalcValue::Number(fit.coeffs[k - 1 - j]));
        }
        if fit.const_used {
            data.push(CalcValue::Number(fit.coeffs[k].exp()));
        }
        return Ok(CalcValue::array(ArrayValue::new(1, p as u32, data)));
    }
    let mut data = vec![CalcValue::err(CalcError::Na); 5 * p];
    for j in 0..k {
        data[j] = CalcValue::Number(fit.coeffs[k - 1 - j]);
    }
    if fit.const_used {
        data[k] = CalcValue::Number(fit.coeffs[k].exp());
    }
    for j in 0..k {
        data[p + j] = se_cell(fit.se[k - 1 - j]);
    }
    if fit.const_used {
        data[p + k] = se_cell(fit.se[k]);
    }
    data[2 * p] = CalcValue::Number(fit.r2);
    if p > 1 {
        data[2 * p + 1] = match fit.se_y {
            Some(v) => CalcValue::Number(v),
            None => CalcValue::err(CalcError::Div0),
        };
    }
    data[3 * p] = match fit.f {
        Some(v) => CalcValue::Number(v),
        None => CalcValue::err(CalcError::Num),
    };
    if p > 1 {
        data[3 * p + 1] = CalcValue::Number(fit.df);
    }
    data[4 * p] = CalcValue::Number(fit.ss_reg);
    if p > 1 {
        data[4 * p + 1] = CalcValue::Number(fit.ss_resid);
    }
    Ok(CalcValue::array(ArrayValue::new(5, p as u32, data)))
}

fn trend_growth(ctx: &FuncCtx, args: &[FuncArg], growth: bool) -> Result<CalcValue, CalcError> {
    let mut rows = fit_observations(ctx, args)?;
    let use_const = if args.len() >= 4 {
        coerce_number(&args[3].value(ctx)?)? != 0.0
    } else {
        true
    };
    if growth {
        for v in &mut rows.y {
            if *v <= 0.0 {
                return Err(CalcError::Num);
            }
            *v = v.ln();
        }
    }
    let fit = fit_least_squares(&rows, use_const)?;
    let predict = |point: &[f64]| -> f64 {
        let mut s = 0.0;
        for j in 0..fit.k {
            s += fit.coeffs[j] * point[j];
        }
        if fit.const_used {
            s += fit.coeffs[fit.k];
        }
        s
    };
    if args.len() < 3 {
        // No new_x's: fitted values at the known x's, shaped like known_y's.
        let m = rows.y.len();
        let mut data = Vec::with_capacity(m);
        for i in 0..m {
            let mut v = predict(&rows.x[i]);
            if growth {
                v = v.exp();
            }
            data.push(CalcValue::Number(v));
        }
        let (rr, cc) = if rows.y_was_col {
            (m as u32, 1u32)
        } else {
            (1u32, m as u32)
        };
        return Ok(CalcValue::array(ArrayValue::new(rr, cc, data)));
    }
    let nx = range_array(ctx, &args[2])?;
    let (nrr0, ncc0) = nx.shape();
    let nrr = nrr0 as usize;
    let ncc = ncc0 as usize;
    let mut result_rows = Vec::new();
    if fit.k == 1 {
        if nrr == 1 {
            for c in 0..ncc {
                let xv = coerce_number(nx.get(0, c as u32))?;
                let mut v = predict(&[xv]);
                if growth {
                    v = v.exp();
                }
                result_rows.push(v);
            }
            return Ok(CalcValue::array(ArrayValue::new(
                1,
                ncc as u32,
                result_rows.into_iter().map(CalcValue::Number).collect(),
            )));
        } else if ncc == 1 {
            for r in 0..nrr {
                let xv = coerce_number(nx.get(r as u32, 0))?;
                let mut v = predict(&[xv]);
                if growth {
                    v = v.exp();
                }
                result_rows.push(v);
            }
            return Ok(CalcValue::array(ArrayValue::new(
                nrr as u32,
                1,
                result_rows.into_iter().map(CalcValue::Number).collect(),
            )));
        } else {
            return Err(CalcError::Ref);
        }
    }
    if ncc != fit.k {
        return Err(CalcError::Ref);
    }
    for r in 0..nrr {
        let mut pt = Vec::with_capacity(fit.k);
        for c in 0..ncc {
            pt.push(coerce_number(nx.get(r as u32, c as u32))?);
        }
        let mut v = predict(&pt);
        if growth {
            v = v.exp();
        }
        result_rows.push(v);
    }
    Ok(CalcValue::array(ArrayValue::new(
        nrr as u32,
        1,
        result_rows.into_iter().map(CalcValue::Number).collect(),
    )))
}

fn trend(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    trend_growth(ctx, args, false)
}

fn growth(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    trend_growth(ctx, args, true)
}

// -- group 11: distribution helpers -------------------------------------------

fn phi(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let x = coerce_number(&args[0].value(ctx)?)?;
    ok_num(norm_s_pdf(x))
}

fn gauss(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let z = coerce_number(&args[0].value(ctx)?)?;
    if z < -10.0 || z > 10.0 {
        return Err(CalcError::Num);
    }
    ok_num(norm_s_cdf(z) - 0.5)
}

fn prob(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let xa = range_array(ctx, &args[0])?;
    let pa = range_array(ctx, &args[1])?;
    if xa.shape() != pa.shape() {
        // Excel: a different number of data points -> #N/A (not #NUM!).
        return Err(CalcError::Na);
    }
    let mut xs = Vec::new();
    let mut ps = Vec::new();
    for (x, p) in xa.iter().zip(pa.iter()) {
        match (x, p) {
            (CalcValue::Number(a), CalcValue::Number(b)) => {
                xs.push(*a);
                ps.push(*b);
            }
            (CalcValue::Error(e), _) | (_, CalcValue::Error(e)) => return Err(*e),
            _ => {}
        }
    }
    let sum: f64 = ps.iter().sum();
    if (sum - 1.0).abs() > 1e-9 {
        return Err(CalcError::Num);
    }
    for &p in &ps {
        if p <= 0.0 || p > 1.0 {
            return Err(CalcError::Num);
        }
    }
    let lower = coerce_number(&args[2].value(ctx)?)?;
    let upper = if args.len() == 4 {
        Some(coerce_number(&args[3].value(ctx)?)?)
    } else {
        None
    };
    let mut total = 0.0;
    for (i, &x) in xs.iter().enumerate() {
        let in_range = match upper {
            Some(u) => x >= lower && x <= u,
            None => x.to_bits() == lower.to_bits(),
        };
        if in_range {
            total += ps[i];
        }
    }
    ok_num(total)
}

fn frequency(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let data = range_array(ctx, &args[0])?;
    let bins = range_array(ctx, &args[1])?;
    let mut dvals = Vec::new();
    for v in data.iter() {
        match v {
            CalcValue::Number(n) => dvals.push(*n),
            CalcValue::Error(e) => return Err(*e),
            _ => {}
        }
    }
    let mut bvals = Vec::new();
    for v in bins.iter() {
        match v {
            CalcValue::Number(n) => bvals.push(*n),
            CalcValue::Error(e) => return Err(*e),
            _ => {}
        }
    }
    // Each value falls into the first bin (in array order) it is <=, else the
    // overflow bucket — one element more than the number of bins.
    let nb = bvals.len();
    let mut counts = vec![0usize; nb + 1];
    for &x in &dvals {
        let mut placed = false;
        for (i, &b) in bvals.iter().enumerate() {
            if x <= b {
                counts[i] += 1;
                placed = true;
                break;
            }
        }
        if !placed {
            counts[nb] += 1;
        }
    }
    Ok(CalcValue::array(ArrayValue::new(
        (nb + 1) as u32,
        1,
        counts
            .into_iter()
            .map(|c| CalcValue::Number(c as f64))
            .collect(),
    )))
}

fn skew_p(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let v = collect_numbers(ctx, args, Agg::Numbers)?;
    let n = v.len();
    if n < 3 {
        return Err(CalcError::Div0);
    }
    let mean = v.iter().sum::<f64>() / n as f64;
    let mut m2 = 0.0;
    let mut m3 = 0.0;
    for &x in &v {
        let d = x - mean;
        m2 += d * d;
        m3 += d * d * d;
    }
    let sd = (m2 / n as f64).sqrt();
    if sd == 0.0 {
        return Err(CalcError::Div0);
    }
    ok_num((m3 / n as f64) / (sd * sd * sd))
}

// -- registration -------------------------------------------------------------

fn spec_variadic(name: &'static str, func: super::Func) -> FuncSpec {
    FuncSpec {
        name,
        min_args: 1,
        max_args: None,
        volatile: false,
        array_aware: true,
        func,
    }
}

fn spec(
    name: &'static str,
    min_args: usize,
    max_args: Option<usize>,
    func: super::Func,
) -> FuncSpec {
    FuncSpec {
        name,
        min_args,
        max_args,
        volatile: false,
        array_aware: false,
        func,
    }
}

/// Array-aware fixed-arity spec (range-taking functions that must receive
/// arrays whole: the hypothesis tests, PROB, FREQUENCY, LINEST & friends).
fn spec_arr(
    name: &'static str,
    min_args: usize,
    max_args: Option<usize>,
    func: super::Func,
) -> FuncSpec {
    FuncSpec {
        name,
        min_args,
        max_args,
        volatile: false,
        array_aware: true,
        func,
    }
}

/// Register a spec, promoting it to `'static` by leaking (registration runs
/// once on cold path; the registry itself stores `&'static FuncSpec`).
fn reg(r: &mut Registry, spec: FuncSpec) {
    r.register(Box::leak(Box::new(spec)));
}

// -- legacy compatibility wrappers -------------------------------------------
//
// The pre-2010 names either fix a cumulative flag the modern form takes as an
// argument, or route a `tails` selector to the one-tailed/two-tailed pair.
// Each wrapper clones the original arguments, appends the fixed flag, and
// delegates to the modern implementation.

fn with_cumulative(
    ctx: &FuncCtx,
    args: &[FuncArg],
    cumulative: bool,
    inner: fn(&FuncCtx, &[FuncArg]) -> Result<CalcValue, CalcError>,
) -> Result<CalcValue, CalcError> {
    let mut full = args.to_vec();
    full.push(FuncArg::Value(CalcValue::Bool(cumulative)));
    inner(ctx, &full)
}

/// BETADIST(x, α, β, [A], [B]) = BETA.DIST(x, α, β, TRUE, [A], [B]).
fn betadist_legacy(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let mut full = args.to_vec();
    full.insert(3, FuncArg::Value(CalcValue::Bool(true)));
    beta_dist(ctx, &full)
}

/// LOGNORMDIST(x, μ, σ) = LOGNORM.DIST(x, μ, σ, TRUE).
fn lognormdist_legacy(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    with_cumulative(ctx, args, true, lognorm_dist)
}

/// NEGBINOMDIST(k, f, p) = NEGBINOM.DIST(k, f, p, FALSE).
fn negbinomdist_legacy(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    with_cumulative(ctx, args, false, negbinom_dist)
}

/// HYPGEOMDIST(k, n, K, N) = HYPGEOM.DIST(k, n, K, N, FALSE).
fn hypgeomdist_legacy(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    with_cumulative(ctx, args, false, hypgeom_dist)
}

/// NORMSDIST(z) = NORM.S.DIST(z, TRUE).
fn normsdist_legacy(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    with_cumulative(ctx, args, true, norm_s_dist)
}

/// TDIST(x, df, tails): tails 1 routes to T.DIST.RT, tails 2 to T.DIST.2T.
fn tdist_legacy(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let tails = coerce_number(&args[2].value(ctx)?)?.trunc();
    let pair = [args[0].clone(), args[1].clone()];
    match tails as i64 {
        1 => t_dist_rt(ctx, &pair),
        2 => t_dist_2t(ctx, &pair),
        _ => Err(CalcError::Num),
    }
}

pub fn register(r: &mut Registry) {
    // group 1 — core aggregates
    reg(r, spec_variadic("AVERAGEA", averagea));
    reg(r, spec_variadic("COUNTBLANK", countblank));
    reg(r, spec_variadic("MAXA", maxa));
    reg(r, spec_variadic("MINA", mina));
    reg(r, spec("MAXIFS", 3, None, maxifs));
    reg(r, spec("MINIFS", 3, None, minifs));
    reg(r, spec_variadic("MEDIAN", median));
    reg(r, spec_variadic("MODE.SNGL", mode_sngl));
    reg(r, spec_variadic("MODE.MULT", mode_mult));
    // group 2 — spread
    reg(r, spec_variadic("STDEV.S", stdev_s));
    reg(r, spec_variadic("STDEV.P", stdev_p));
    reg(r, spec_variadic("STDEVA", stdeva));
    reg(r, spec_variadic("STDEVPA", stdevpa));
    reg(r, spec_variadic("VAR.S", var_s));
    reg(r, spec_variadic("VAR.P", var_p));
    reg(r, spec_variadic("VARA", vara));
    reg(r, spec_variadic("VARPA", varpa));
    reg(r, spec_variadic("AVEDEV", avedev));
    reg(r, spec_variadic("DEVSQ", devsq));
    // group 3 — rank and position
    reg(r, spec("LARGE", 2, Some(2), large));
    reg(r, spec("SMALL", 2, Some(2), small));
    reg(r, spec("RANK.EQ", 2, Some(3), rank_eq));
    reg(r, spec("RANK.AVG", 2, Some(3), rank_avg));
    reg(r, spec("PERCENTILE.INC", 2, Some(2), percentile_inc));
    reg(r, spec("PERCENTILE.EXC", 2, Some(2), percentile_exc));
    reg(r, spec("QUARTILE.INC", 2, Some(2), quartile_inc));
    reg(r, spec("QUARTILE.EXC", 2, Some(2), quartile_exc));
    reg(r, spec("PERCENTRANK.INC", 2, Some(3), percentrank_inc));
    reg(r, spec("PERCENTRANK.EXC", 2, Some(3), percentrank_exc));
    reg(r, spec("TRIMMEAN", 2, Some(2), trimmean));
    // group 4 — distributions
    reg(r, spec("NORM.DIST", 4, Some(4), norm_dist));
    reg(r, spec("NORM.INV", 3, Some(3), norm_inv));
    reg(r, spec("NORM.S.DIST", 2, Some(2), norm_s_dist));
    reg(r, spec("NORM.S.INV", 1, Some(1), norm_s_inv_fn));
    reg(r, spec("BINOM.DIST", 4, Some(4), binom_dist));
    reg(r, spec("POISSON.DIST", 3, Some(3), poisson_dist));
    reg(r, spec("EXPON.DIST", 3, Some(3), expon_dist));
    reg(r, spec("T.DIST", 3, Some(3), t_dist));
    reg(r, spec("T.DIST.2T", 2, Some(2), t_dist_2t));
    reg(r, spec("T.INV", 2, Some(2), t_inv));
    reg(r, spec("CHISQ.DIST", 3, Some(3), chisq_dist));
    reg(r, spec("CHISQ.INV", 2, Some(2), chisq_inv));
    reg(r, spec("F.DIST", 4, Some(4), f_dist));
    reg(r, spec("GAMMA", 1, Some(1), gamma_fn));
    reg(r, spec("GAMMALN", 1, Some(1), gammaln_fn));
    reg(r, spec("BETA.DIST", 4, Some(6), beta_dist));
    reg(r, spec("WEIBULL.DIST", 4, Some(4), weibull_dist));
    reg(r, spec("LOGNORM.DIST", 4, Some(4), lognorm_dist));
    reg(r, spec("HYPGEOM.DIST", 5, Some(5), hypgeom_dist));
    reg(r, spec("NEGBINOM.DIST", 4, Some(4), negbinom_dist));
    // group 5 — regression
    reg(r, spec("SLOPE", 2, Some(2), slope));
    reg(r, spec("INTERCEPT", 2, Some(2), intercept));
    reg(r, spec("CORREL", 2, Some(2), correl));
    reg(r, spec("PEARSON", 2, Some(2), pearson));
    reg(r, spec("RSQ", 2, Some(2), rsq));
    reg(r, spec("STEYX", 2, Some(2), steyx));
    reg(r, spec("FORECAST.LINEAR", 3, Some(3), forecast_linear));
    reg(r, spec("COVARIANCE.P", 2, Some(2), covar_p));
    reg(r, spec("COVARIANCE.S", 2, Some(2), covar_s));
    reg(r, spec_variadic("SKEW", skew));
    reg(r, spec_variadic("KURT", kurt));
    reg(r, spec("FISHER", 1, Some(1), fisher));
    reg(r, spec("FISHERINV", 1, Some(1), fisherinv));
    reg(r, spec("STANDARDIZE", 3, Some(3), standardize));
    // group 6 — means
    reg(r, spec_variadic("GEOMEAN", geomean));
    reg(r, spec_variadic("HARMEAN", harmean));
    // group 7 — right tails and inverse distributions
    reg(r, spec("CHISQ.DIST.RT", 2, Some(2), chisq_dist_rt));
    reg(r, spec("CHISQ.INV.RT", 2, Some(2), chisq_inv_rt));
    // compatibility aliases: CHIDIST = CHISQ.DIST.RT, CHIINV = CHISQ.INV.RT
    reg(r, spec("CHIDIST", 2, Some(2), chisq_dist_rt));
    reg(r, spec("CHIINV", 2, Some(2), chisq_inv_rt));
    reg(r, spec("F.DIST.RT", 3, Some(3), f_dist_rt));
    reg(r, spec("F.INV", 3, Some(3), f_inv));
    reg(r, spec("F.INV.RT", 3, Some(3), f_inv_rt));
    reg(r, spec("T.DIST.RT", 2, Some(2), t_dist_rt));
    reg(r, spec("T.INV.2T", 2, Some(2), t_inv_2t));
    reg(r, spec("BINOM.INV", 3, Some(3), binom_inv));
    reg(r, spec("BETA.INV", 3, Some(5), beta_inv));
    reg(r, spec("LOGNORM.INV", 3, Some(3), lognorm_inv));
    reg(r, spec("GAMMA.DIST", 4, Some(4), gamma_dist));
    reg(r, spec("GAMMA.INV", 3, Some(3), gamma_inv));
    reg(r, spec("BINOM.DIST.RANGE", 3, Some(4), binom_dist_range));
    // group 8 — hypothesis tests
    reg(r, spec_arr("CHISQ.TEST", 2, Some(2), chisq_test));
    reg(r, spec_arr("F.TEST", 2, Some(2), f_test));
    reg(r, spec_arr("T.TEST", 4, Some(4), t_test));
    reg(r, spec_arr("Z.TEST", 2, Some(3), z_test));
    // group 9 — confidence intervals
    reg(r, spec("CONFIDENCE.NORM", 3, Some(3), confidence_norm));
    reg(r, spec("CONFIDENCE.T", 3, Some(3), confidence_t));
    // group 10 — regression and forecasting
    reg(r, spec_arr("LINEST", 1, Some(4), linest));
    reg(r, spec_arr("LOGEST", 1, Some(4), logest));
    reg(r, spec_arr("TREND", 1, Some(4), trend));
    reg(r, spec_arr("GROWTH", 1, Some(4), growth));
    // group 11 — distribution helpers
    reg(r, spec("PHI", 1, Some(1), phi));
    reg(r, spec("GAUSS", 1, Some(1), gauss));
    reg(r, spec_arr("PROB", 3, Some(4), prob));
    reg(r, spec_arr("FREQUENCY", 2, Some(2), frequency));
    reg(r, spec_variadic("SKEW.P", skew_p));
    // group 12 — legacy compatibility names (Excel pre-2010). Plain aliases
    // where the legacy signature matches the modern function exactly;
    // wrappers where legacy semantics differ (fixed cumulative flag or
    // tails argument).
    reg(r, spec("BETAINV", 3, Some(5), beta_inv));
    reg(r, spec("BINOMDIST", 4, Some(4), binom_dist));
    reg(r, spec_arr("CHITEST", 2, Some(2), chisq_test));
    reg(r, spec("CONFIDENCE", 3, Some(3), confidence_norm));
    reg(r, spec("COVAR", 2, Some(2), covar_p));
    reg(r, spec("CRITBINOM", 3, Some(3), binom_inv));
    reg(r, spec("EXPONDIST", 3, Some(3), expon_dist));
    reg(r, spec_arr("FTEST", 2, Some(2), f_test));
    reg(r, spec("GAMMADIST", 4, Some(4), gamma_dist));
    reg(r, spec("GAMMAINV", 3, Some(3), gamma_inv));
    reg(r, spec("LOGINV", 3, Some(3), lognorm_inv));
    reg(r, spec_variadic("MODE", mode_sngl));
    reg(r, spec("NORMDIST", 4, Some(4), norm_dist));
    reg(r, spec("NORMINV", 3, Some(3), norm_inv));
    reg(r, spec("NORMSINV", 1, Some(1), norm_s_inv_fn));
    reg(r, spec("PERCENTILE", 2, Some(2), percentile_inc));
    reg(r, spec("PERCENTRANK", 2, Some(3), percentrank_inc));
    reg(r, spec("POISSON", 3, Some(3), poisson_dist));
    reg(r, spec("QUARTILE", 2, Some(2), quartile_inc));
    reg(r, spec("RANK", 2, Some(3), rank_eq));
    reg(r, spec_variadic("STDEV", stdev_s));
    reg(r, spec_variadic("STDEVP", stdev_p));
    reg(r, spec_arr("TTEST", 4, Some(4), t_test));
    reg(r, spec_variadic("VAR", var_s));
    reg(r, spec_variadic("VARP", var_p));
    reg(r, spec("WEIBULL", 4, Some(4), weibull_dist));
    reg(r, spec_arr("ZTEST", 2, Some(3), z_test));
    reg(r, spec("FORECAST", 3, Some(3), forecast_linear));
    reg(r, spec("GAMMALN.PRECISE", 1, Some(1), gammaln_fn));
    reg(r, spec("HYPGEOMDIST", 4, Some(4), hypgeomdist_legacy));
    // right-tail legacy forms map straight onto the right-tail moderns
    reg(r, spec("FDIST", 3, Some(3), f_dist_rt));
    reg(r, spec("FINV", 3, Some(3), f_inv_rt));
    reg(r, spec("TINV", 2, Some(2), t_inv_2t));
    // wrappers: legacy fixed-cumulative / tails semantics
    reg(r, spec("BETADIST", 3, Some(5), betadist_legacy));
    reg(r, spec("LOGNORMDIST", 3, Some(3), lognormdist_legacy));
    reg(r, spec("NEGBINOMDIST", 3, Some(3), negbinomdist_legacy));
    reg(r, spec("NORMSDIST", 1, Some(1), normsdist_legacy));
    reg(r, spec("TDIST", 3, Some(3), tdist_legacy));
}

#[cfg(test)]
mod coordinator_accuracy {
    //! Coordinator check: the family agent's own note claims the inverses agree
    //! with published values only to ~1e-4/1e-5 while ALSO claiming to be "far
    //! past the 1e-9 shippable bar". Those cannot both be true, and the brief
    //! said anything worse than 1e-9 should be left unregistered. This pins the
    //! answer against scipy (double precision), not against Excel help-page
    //! digits, which are themselves printed to only a few decimals.
    use crate::turbo::calc::testkit::*;

    fn approx(formula: &str, want: f64, tol: f64) {
        let got = num(formula);
        let rel = ((got - want) / want).abs();
        println!("{formula:<34} got {got:.15}  want {want:.15}  rel {rel:.2e}");
        assert!(
            rel < tol,
            "{formula}: rel error {rel:.2e} exceeds {tol:.0e}"
        );
    }

    #[test]
    fn coordinator_inverse_accuracy_vs_scipy() {
        approx("F.INV(0.9,6,4)", 4.009749312673945, 1e-9);
        approx("T.INV.2T(0.05,10)", 2.2281388519862744, 1e-9);
        approx("GAMMA.INV(0.5,9,2)", 17.33790236874074, 1e-9);
        approx("CHISQ.INV(0.93,1)", 3.283020286759539, 1e-9);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turbo::calc::testkit::{Grid, approx, error, num};

    #[test]
    fn averagea_counts_text_and_logicals() {
        let g = Grid::empty()
            .set_num("A1", 2.0)
            .set_bool("A2", true)
            .set_text("A3", "abc");
        approx("=AVERAGEA(2,TRUE)", 1.5, 1e-12);
        g_approx("=AVERAGEA(A1:A3)", 1.0, &g);
        assert_eq!(g.num("=AVERAGEA(A1:A2)"), 1.5);
        assert_eq!(Grid::empty().error("=AVERAGEA(A1:A3)"), CalcError::Div0);
    }

    #[test]
    fn countblank_counts_blank_and_empty_string() {
        let g = Grid::empty().set_num("A1", 1.0).set_text("A2", "");
        assert_eq!(g.num("=COUNTBLANK(A1:A3)"), 2.0);
        assert_eq!(Grid::empty().num("=COUNTBLANK(A1:A5)"), 5.0);
        let g2 = Grid::empty().set_num("A1", 0.0).set_bool("A2", false);
        assert_eq!(
            g2.num("=COUNTBLANK(A1:A3)"),
            1.0,
            "0 and FALSE are not blank"
        );
    }

    #[test]
    fn maxa_mina_include_text_as_zero() {
        let g = Grid::empty()
            .set_num("A1", 0.2)
            .set_num("A2", 0.3)
            .set_text("A3", "abc")
            .set_bool("A4", true)
            .set_num("A5", 0.1);
        assert_eq!(g.num("=MAXA(A1:A5)"), 1.0);
        assert_eq!(g.num("=MINA(A1:A5)"), 0.0);
        assert_eq!(Grid::empty().num("=MAXA(A1:A3)"), 0.0);
        assert_eq!(Grid::empty().num("=MINA(A1:A3)"), 0.0);
    }

    #[test]
    fn maxifs_minifs() {
        let g = Grid::empty()
            .col("A1", &[1.0, 2.0, 3.0, 4.0])
            .col("B1", &[10.0, 20.0, 30.0, 40.0]);
        assert_eq!(g.num("=MAXIFS(A1:A4,B1:B4,\">15\")"), 4.0);
        assert_eq!(g.num("=MINIFS(A1:A4,B1:B4,\">15\")"), 2.0);
        assert_eq!(g.num("=MAXIFS(A1:A4,B1:B4,\">100\")"), 0.0);
        assert_eq!(g.num("=MINIFS(A1:A4,B1:B4,\">100\")"), 0.0);
        assert_eq!(
            g.error("=MAXIFS(A1:A3,B1:B3,\">0\",C1:C3)"),
            CalcError::Value
        );
        assert_eq!(error("=MAXIFS(A1:A2)"), CalcError::Value);
    }

    #[test]
    fn maxifs_criteria_operators_and_wildcards() {
        let g = Grid::empty()
            .col("A1", &[1.0, 5.0, 6.0, 10.0])
            .set_text("C1", "apple")
            .set_text("C2", "apricot")
            .set_text("C3", "banana")
            .set_text("C4", "apple");
        assert_eq!(g.num("=MAXIFS(A1:A4,C1:C4,\"ap*\")"), 10.0);
        assert_eq!(g.num("=MINIFS(A1:A4,C1:C4,\"<>banana\")"), 1.0);
        assert_eq!(g.num("=MAXIFS(A1:A4,A1:A4,\">5\")"), 10.0);
    }

    #[test]
    fn median_basic_and_even() {
        assert_eq!(num("=MEDIAN(1,2,3,4,5)"), 3.0);
        assert_eq!(num("=MEDIAN(1,2,3,4,5,6)"), 3.5);
        let g = Grid::empty().col("A1", &[5.0, 2.0, 9.0, 4.0]);
        assert_eq!(g.num("=MEDIAN(A1:A4)"), 4.5);
        assert_eq!(Grid::empty().error("=MEDIAN(A1:A3)"), CalcError::Num);
        assert_eq!(error("=MEDIAN(\"abc\")"), CalcError::Value);
    }

    #[test]
    fn mode_sngl_and_mult() {
        let g = Grid::empty().col("A1", &[5.6, 4.0, 4.0, 3.0, 2.0, 4.0]);
        assert_eq!(g.num("=MODE.SNGL(A1:A6)"), 4.0);
        let arr = g.array("=MODE.MULT(A1:A6)");
        assert_eq!(arr.shape(), (1, 1));
        assert_eq!(arr.data[0], CalcValue::Number(4.0));
        let g2 = Grid::empty().col("A1", &[1.0, 2.0, 2.0, 3.0, 3.0, 4.0]);
        let arr2 = g2.array("=MODE.MULT(A1:A6)");
        assert_eq!(arr2.shape(), (2, 1));
        assert_eq!(arr2.data[0], CalcValue::Number(2.0));
        assert_eq!(arr2.data[1], CalcValue::Number(3.0));
        assert_eq!(
            Grid::empty()
                .col("A1", &[1.0, 2.0, 3.0])
                .error("=MODE.SNGL(A1:A3)"),
            CalcError::Na
        );
    }

    #[test]
    fn stdev_var_welford_stability() {
        // The catastrophic-cancellation test from the task brief: values
        // around 1e6 with a spread of 1 must give the right stdev, not 0.
        let g = Grid::empty().col("A1", &[1000000.1, 1000000.2, 1000000.3]);
        g_approx("=STDEV.S(A1:A3)", 0.1, &g);
        g_approx("=STDEV.P(A1:A3)", 0.1 * (2.0f64 / 3.0).sqrt(), &g);
        g_approx("=VAR.S(A1:A3)", 0.01, &g);
        // Excel-documented example
        let v = [
            1345.0, 1301.0, 1368.0, 1322.0, 1310.0, 1370.0, 1318.0, 1350.0, 1303.0, 1299.0,
        ];
        let g2 = Grid::empty().col("A1", &v);
        g_approx("=STDEV.S(A1:A10)", 27.4639157198, &g2);
        g_approx("=STDEV.P(A1:A10)", 26.05455814, &g2);
        g_approx("=VAR.S(A1:A10)", 754.2666666667, &g2);
        g_approx("=VAR.P(A1:A10)", 678.84, &g2);
        assert_eq!(Grid::empty().error("=STDEV.S(A1)"), CalcError::Div0);
        assert_eq!(Grid::empty().error("=STDEV.P(A1:A2)"), CalcError::Div0);
        assert_eq!(error("=STDEV.S(\"abc\")"), CalcError::Value);
    }

    #[test]
    fn stdeva_vara_count_text_bool() {
        let g = Grid::empty()
            .set_num("A1", 2.0)
            .set_num("A2", 4.0)
            .set_bool("A3", true)
            .set_text("A4", "x");
        g_approx("=STDEVA(A1:A4)", 1.7078251277, &g);
        g_approx("=VARA(A1:A4)", 2.9166666667, &g);
        g_approx("=STDEVPA(A1:A4)", 1.4790199458, &g);
        g_approx("=VARPA(A1:A4)", 2.1875, &g);
    }

    #[test]
    fn avedev_and_devsq() {
        let g = Grid::empty().col("A1", &[4.0, 5.0, 6.0, 7.0, 5.0, 4.0, 3.0]);
        g_approx("=AVEDEV(A1:A7)", 1.0204081633, &g);
        let g2 = Grid::empty().col("A1", &[4.0, 5.0, 8.0, 7.0, 11.0, 4.0, 3.0]);
        assert_eq!(g2.num("=DEVSQ(A1:A7)"), 48.0);
        assert_eq!(Grid::empty().error("=AVEDEV(A1:A3)"), CalcError::Div0);
        assert_eq!(Grid::empty().error("=DEVSQ(A1:A3)"), CalcError::Div0);
    }

    /// Grid-aware relative-tolerance check (the free `approx` uses the empty grid).
    fn g_approx(formula: &str, expected: f64, g: &Grid) {
        g_approx_tol(formula, expected, 1e-9, g);
    }

    fn g_approx_tol(formula: &str, expected: f64, rel: f64, g: &Grid) {
        let got = g.num(formula);
        let scale = expected.abs().max(1.0);
        assert!(
            (got - expected).abs() <= rel * scale,
            "{formula} -> {got}, expected {expected} (rel tol {rel})"
        );
    }

    #[test]
    fn large_small() {
        let g = Grid::empty().col("A1", &[3.0, 5.0, 3.0, 5.0, 4.0]);
        assert_eq!(g.num("=LARGE(A1:A5,2)"), 5.0);
        assert_eq!(g.num("=LARGE(A1:A5,3)"), 4.0);
        assert_eq!(g.num("=SMALL(A1:A5,1)"), 3.0);
        assert_eq!(g.num("=SMALL(A1:A5,3)"), 4.0);
        assert_eq!(g.error("=LARGE(A1:A5,0)"), CalcError::Num);
        assert_eq!(g.error("=LARGE(A1:A5,6)"), CalcError::Num);
        assert_eq!(Grid::empty().error("=LARGE(A1:A3,1)"), CalcError::Num);
    }

    #[test]
    fn rank_eq_and_avg() {
        let g = Grid::empty().col("A1", &[7.0, 3.5, 3.5, 1.0, 2.0]);
        assert_eq!(g.num("=RANK.EQ(3.5,A1:A5,0)"), 2.0);
        assert_eq!(g.num("=RANK.EQ(3.5,A1:A5,1)"), 3.0);
        assert_eq!(g.num("=RANK.EQ(7,A1:A5)"), 1.0);
        assert_eq!(g.num("=RANK.AVG(3.5,A1:A5,0)"), 2.5);
        assert_eq!(g.num("=RANK.AVG(3.5,A1:A5,1)"), 3.5);
        assert_eq!(g.error("=RANK.EQ(9,A1:A5)"), CalcError::Na);
        assert_eq!(error("=RANK.EQ(1)"), CalcError::Value, "too few arguments");
    }

    #[test]
    fn percentile_and_quartile() {
        let g = Grid::empty().col("A1", &[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(g.num("=PERCENTILE.INC(A1:A4,0.3)"), 1.9);
        assert_eq!(g.num("=PERCENTILE.EXC(A1:A4,0.3)"), 1.5);
        let q = Grid::empty().col("A1", &[1.0, 2.0, 4.0, 7.0, 8.0, 9.0, 10.0, 12.0]);
        assert_eq!(q.num("=QUARTILE.INC(A1:A8,1)"), 3.5);
        assert_eq!(q.num("=QUARTILE.EXC(A1:A8,1)"), 2.5);
        assert_eq!(q.num("=QUARTILE.INC(A1:A8,3)"), 9.25);
        assert_eq!(g.error("=PERCENTILE.INC(A1:A4,1.5)"), CalcError::Num);
        assert_eq!(g.error("=PERCENTILE.EXC(A1:A4,0)"), CalcError::Num);
        assert_eq!(q.error("=QUARTILE.EXC(A1:A8,0)"), CalcError::Num);
        assert_eq!(q.error("=QUARTILE.INC(A1:A8,5)"), CalcError::Num);
    }

    #[test]
    fn percentrank() {
        let g = Grid::empty().col("A1", &[1.0, 2.0, 3.0, 4.0]);
        g_approx("=PERCENTRANK.INC(A1:A4,2)", 0.333, &g);
        g_approx("=PERCENTRANK.EXC(A1:A4,2)", 0.4, &g);
        g_approx("=PERCENTRANK.INC(A1:A4,2.5)", 0.5, &g);
        g_approx("=PERCENTRANK.INC(A1:A4,2,4)", 0.3333, &g);
        assert_eq!(g.error("=PERCENTRANK.INC(A1:A4,0)"), CalcError::Na);
        assert_eq!(g.error("=PERCENTRANK.EXC(A1:A4,4.5)"), CalcError::Na);
        assert_eq!(g.error("=PERCENTRANK.INC(A1:A4,2,0)"), CalcError::Num);
    }

    #[test]
    fn trimmean() {
        let g = Grid::empty().col(
            "A1",
            &[
                4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0,
            ],
        );
        assert_eq!(g.num("=TRIMMEAN(A1:A12,0.1)"), 9.5);
        assert_eq!(g.num("=TRIMMEAN(A1:A12,0.2)"), 9.5);
        assert_eq!(g.error("=TRIMMEAN(A1:A12,1.1)"), CalcError::Num);
        assert_eq!(g.error("=TRIMMEAN(A1:A12,1)"), CalcError::Num);
    }

    #[test]
    fn norm_distribution() {
        approx("=NORM.S.DIST(1.333333,TRUE)", 0.908788726, 1e-9);
        approx("=NORM.S.DIST(1.333333,FALSE)", 0.1640101476, 1e-9);
        approx("=NORM.DIST(42,40,1.5,TRUE)", 0.9087887803, 1e-9);
        approx("=NORM.DIST(42,40,1.5,FALSE)", 0.1093400498, 1e-9);
        approx("=NORM.S.INV(0.908789)", 1.333335, 1e-5);
        approx("=NORM.S.INV(0.975)", 1.9599639845, 1e-9);
        approx("=NORM.INV(0.908789,40,1.5)", 42.000002, 1e-5);
        assert_eq!(error("=NORM.DIST(0,0,0,TRUE)"), CalcError::Num);
        assert_eq!(error("=NORM.INV(0,0,1)"), CalcError::Num);
        assert_eq!(error("=NORM.S.INV(1)"), CalcError::Num);
    }

    #[test]
    fn binom_dist() {
        approx("=BINOM.DIST(6,10,0.5,FALSE)", 0.205078125, 1e-12);
        approx("=BINOM.DIST(6,10,0.5,TRUE)", 0.828125, 1e-12);
        approx("=BINOM.DIST(3,5,0.25,FALSE)", 0.087890625, 1e-12);
        assert_eq!(error("=BINOM.DIST(11,10,0.5,TRUE)"), CalcError::Num);
        assert_eq!(error("=BINOM.DIST(1,10,1.5,TRUE)"), CalcError::Num);
    }

    #[test]
    fn poisson_dist() {
        approx("=POISSON.DIST(2,5,TRUE)", 0.1246520195, 1e-9);
        approx("=POISSON.DIST(2,5,FALSE)", 0.0842243375, 1e-9);
        approx("=POISSON.DIST(0,3,TRUE)", 0.0497870684, 1e-9);
        assert_eq!(error("=POISSON.DIST(-1,5,TRUE)"), CalcError::Num);
        assert_eq!(error("=POISSON.DIST(2,-1,TRUE)"), CalcError::Num);
    }

    #[test]
    fn expon_dist() {
        approx("=EXPON.DIST(0.2,10,TRUE)", 0.8646647168, 1e-9);
        approx("=EXPON.DIST(0.2,10,FALSE)", 1.353352832, 1e-9);
        assert_eq!(error("=EXPON.DIST(-1,10,TRUE)"), CalcError::Num);
        assert_eq!(error("=EXPON.DIST(1,0,TRUE)"), CalcError::Num);
    }

    #[test]
    fn t_distribution() {
        approx("=T.DIST(60,1,TRUE)", 0.9946953264, 1e-9);
        approx("=T.DIST(60,1,FALSE)", 0.0000883948587, 1e-9);
        approx("=T.DIST(1.959,60,TRUE)", 0.9726176312, 1e-9);
        approx("=T.DIST(-60,1,TRUE)", 0.0053046736, 1e-9);
        approx("=T.DIST.2T(1.959,60)", 0.0547647377, 1e-9);
        approx("=T.DIST.2T(60,1)", 0.0106093473, 1e-9);
        approx("=T.INV(0.9,60)", 1.295821, 1e-6);
        approx("=T.INV(0.1,60)", -1.295821, 1e-6);
        approx("=T.INV(0.5,10)", 0.0, 1e-12);
        assert_eq!(error("=T.DIST(1,0,TRUE)"), CalcError::Num);
        assert_eq!(error("=T.DIST.2T(-1,60)"), CalcError::Num);
        assert_eq!(error("=T.INV(0,60)"), CalcError::Num);
    }

    #[test]
    fn chisq_dist() {
        approx("=CHISQ.DIST(18.307,10,TRUE)", 0.95, 1e-4);
        approx("=CHISQ.DIST(18.307,10,FALSE)", 0.0154807845, 1e-9);
        approx("=CHISQ.DIST(3.247,7,TRUE)", 0.1387484949, 1e-9);
        approx("=CHISQ.INV(0.95,10)", 18.307038, 1e-4);
        approx("=CHISQ.INV(0.05,10)", 3.940299, 1e-6);
        approx("=CHISQ.INV(0.5,10)", 9.341818, 1e-6);
        assert_eq!(error("=CHISQ.DIST(-1,10,TRUE)"), CalcError::Num);
        assert_eq!(error("=CHISQ.DIST(1,0,TRUE)"), CalcError::Num);
        assert_eq!(error("=CHISQ.INV(0,10)"), CalcError::Num);
    }

    #[test]
    fn f_dist() {
        approx("=F.DIST(15.2069,6,4,TRUE)", 0.99, 1e-4);
        approx("=F.DIST(15.2069,6,4,FALSE)", 0.0012237917, 1e-9);
        approx("=F.DIST(1,2,3,TRUE)", 0.5352419985, 1e-9);
        assert_eq!(error("=F.DIST(-1,6,4,TRUE)"), CalcError::Num);
        assert_eq!(error("=F.DIST(1,0,4,TRUE)"), CalcError::Num);
    }

    #[test]
    fn gamma_and_gammaln() {
        approx("=GAMMALN(4.5)", 2.4537365708, 1e-9);
        approx("=GAMMALN(10)", 12.8018274801, 1e-9);
        approx("=GAMMA(2.5)", 1.3293403882, 1e-9);
        approx("=GAMMA(5)", 24.0, 1e-12);
        assert_eq!(error("=GAMMALN(0)"), CalcError::Num);
        assert_eq!(error("=GAMMA(-1)"), CalcError::Num);
    }

    #[test]
    fn beta_dist() {
        approx("=BETA.DIST(2,8,10,TRUE,1,3)", 0.6854706, 1e-6);
        approx("=BETA.DIST(2,8,10,FALSE,1,3)", 1.483764648, 1e-6);
        approx("=BETA.DIST(0.5,2,3,TRUE)", 0.6875, 1e-9);
        assert_eq!(error("=BETA.DIST(0,8,10,TRUE,1,3)"), CalcError::Num);
        assert_eq!(error("=BETA.DIST(2,0,10,TRUE)"), CalcError::Num);
    }

    #[test]
    fn weibull_dist() {
        approx("=WEIBULL.DIST(105,20,100,TRUE)", 0.9295813901, 1e-9);
        approx("=WEIBULL.DIST(105,20,100,FALSE)", 0.0355888640, 1e-9);
        approx("=WEIBULL.DIST(0,1,1,TRUE)", 0.0, 1e-12);
        assert_eq!(error("=WEIBULL.DIST(-1,20,100,TRUE)"), CalcError::Num);
        assert_eq!(error("=WEIBULL.DIST(1,0,100,TRUE)"), CalcError::Num);
    }

    #[test]
    fn lognorm_dist() {
        approx("=LOGNORM.DIST(4,3.5,1.2,TRUE)", 0.0390835557, 1e-9);
        approx("=LOGNORM.DIST(4,3.5,1.2,FALSE)", 0.0176175969, 1e-9);
        assert_eq!(error("=LOGNORM.DIST(0,3.5,1.2,TRUE)"), CalcError::Num);
        assert_eq!(error("=LOGNORM.DIST(4,3.5,0,TRUE)"), CalcError::Num);
    }

    #[test]
    fn hypgeom_dist() {
        approx("=HYPGEOM.DIST(1,4,8,20,TRUE)", 0.4654282766, 1e-9);
        approx("=HYPGEOM.DIST(1,4,8,20,FALSE)", 0.3632610939, 1e-9);
        approx("=HYPGEOM.DIST(0,4,8,20,FALSE)", 0.1021671833, 1e-9);
        assert_eq!(error("=HYPGEOM.DIST(5,4,8,20,TRUE)"), CalcError::Num);
        assert_eq!(error("=HYPGEOM.DIST(1,4,25,20,TRUE)"), CalcError::Num);
    }

    #[test]
    fn negbinom_dist() {
        approx("=NEGBINOM.DIST(10,5,0.25,TRUE)", 0.3135140585, 1e-9);
        approx("=NEGBINOM.DIST(10,5,0.25,FALSE)", 0.0550486604, 1e-9);
        assert_eq!(error("=NEGBINOM.DIST(-1,5,0.25,TRUE)"), CalcError::Num);
        assert_eq!(error("=NEGBINOM.DIST(1,5,1.5,TRUE)"), CalcError::Num);
    }

    #[test]
    fn regression_family() {
        // Excel-documented FORECAST example
        let g = Grid::empty()
            .col("A1", &[6.0, 7.0, 9.0, 15.0, 21.0])
            .col("B1", &[20.0, 28.0, 31.0, 38.0, 40.0]);
        g_approx_tol("=FORECAST.LINEAR(30,A1:A5,B1:B5)", 10.607253, 1e-6, &g);
        // Excel-documented CORREL example
        let g2 = Grid::empty()
            .col("A1", &[3.0, 2.0, 4.0, 5.0, 6.0])
            .col("B1", &[9.0, 7.0, 12.0, 15.0, 17.0]);
        g_approx("=CORREL(A1:A5,B1:B5)", 0.9970544855, &g2);
        g_approx("=PEARSON(A1:A5,B1:B5)", 0.9970544855, &g2);
        g_approx_tol("=RSQ(A1:A5,B1:B5)", 0.9941178, 1e-6, &g2);
        g_approx("=COVARIANCE.P(A1:A5,B1:B5)", 5.2, &g2);
        g_approx("=COVARIANCE.S(A1:A5,B1:B5)", 6.5, &g2);
        // PERFECT line: slope 2, intercept 0, rsq 1
        let p = Grid::empty()
            .col("A1", &[2.0, 4.0, 6.0, 8.0])
            .col("B1", &[1.0, 2.0, 3.0, 4.0]);
        g_approx("=SLOPE(A1:A4,B1:B4)", 2.0, &p);
        g_approx("=INTERCEPT(A1:A4,B1:B4)", 0.0, &p);
        g_approx("=RSQ(A1:A4,B1:B4)", 1.0, &p);
        // constant x -> zero variance -> #DIV/0!
        let flat = Grid::empty()
            .col("A1", &[1.0, 2.0, 3.0])
            .col("B1", &[5.0, 5.0, 5.0]);
        assert_eq!(flat.error("=SLOPE(A1:A3,B1:B3)"), CalcError::Div0);
        let mism = Grid::empty()
            .col("A1", &[1.0, 2.0, 3.0])
            .col("B1", &[1.0, 2.0]);
        assert_eq!(mism.error("=CORREL(A1:A3,B1:B2)"), CalcError::Na);
    }

    #[test]
    fn skew_kurt() {
        let g = Grid::empty().col("A1", &[3.0, 4.0, 5.0, 2.0, 3.0, 4.0, 5.0, 6.0, 4.0, 7.0]);
        g_approx("=SKEW(A1:A10)", 0.3595430714, &g);
        g_approx("=KURT(A1:A10)", -0.1517996372, &g);
        assert_eq!(
            Grid::empty().col("A1", &[1.0, 2.0]).error("=SKEW(A1:A2)"),
            CalcError::Div0
        );
        assert_eq!(
            Grid::empty()
                .col("A1", &[1.0, 2.0, 3.0])
                .error("=KURT(A1:A3)"),
            CalcError::Div0
        );
    }

    #[test]
    fn fisher_and_standardize() {
        approx("=FISHER(0.75)", 0.9729550745, 1e-9);
        approx("=FISHERINV(0.9729550745)", 0.75, 1e-9);
        approx("=FISHERINV(0)", 0.0, 1e-12);
        approx("=STANDARDIZE(42,40,1.5)", 1.333333, 1e-5);
        assert_eq!(error("=FISHER(1)"), CalcError::Num);
        assert_eq!(error("=FISHER(-1.5)"), CalcError::Num);
        assert_eq!(error("=STANDARDIZE(1,0,0)"), CalcError::Num);
    }

    #[test]
    fn spread_mixed_args() {
        // direct scalar coercion vs range filtering
        let g = Grid::empty()
            .set_text("A1", "x")
            .set_num("A2", 1.0)
            .set_num("A3", 2.0);
        approx("=MEDIAN(1,\"2\",3)", 2.0, 1e-12);
        g_approx("=STDEV.S(A2:A3,3)", 1.0, &g);
        // a range ignores text; a scalar text arg must coerce or error
        assert_eq!(g.error("=STDEV.S(A1,1,2)"), CalcError::Value);
        assert_eq!(g.error("=STDEV.S(\"abc\",1,2)"), CalcError::Value);
        let g2 = Grid::empty()
            .set_text("A1", "x")
            .set_num("A2", 1.0)
            .set_num("A3", 3.0);
        g_approx("=STDEV.S(A1:A3)", 1.4142135624, &g2);
    }

    // -- helpers for the added families ---------------------------------------

    fn approx_cell(v: f64, expected: f64) {
        let scale = expected.abs().max(1.0);
        assert!(
            (v - expected).abs() <= 1e-5 * scale,
            "cell value {v} != expected {expected}"
        );
    }

    fn num_at(a: &ArrayValue, r: u32, c: u32) -> f64 {
        match a.get(r, c) {
            CalcValue::Number(n) => *n,
            other => panic!("not a number at ({r},{c}): {other:?}"),
        }
    }

    #[test]
    fn geomean_and_harmean() {
        let g = Grid::empty().col("A1", &[4.0, 5.0, 8.0, 7.0, 11.0, 4.0, 3.0]);
        g_approx("=GEOMEAN(A1:A7)", 5.4769869697, &g);
        g_approx("=HARMEAN(A1:A7)", 5.0283759621, &g);
        approx("=GEOMEAN(1,2,4)", 2.0, 1e-12);
        approx("=HARMEAN(2,3,6)", 3.0, 1e-12);
        assert_eq!(error("=GEOMEAN(1,0)"), CalcError::Num);
        assert_eq!(error("=HARMEAN(1,-1)"), CalcError::Num);
        assert_eq!(Grid::empty().error("=GEOMEAN(A1:A3)"), CalcError::Num);
    }

    #[test]
    fn right_tail_distributions() {
        approx("=CHISQ.DIST.RT(18.307,10)", 0.05, 1e-4);
        approx("=CHISQ.DIST.RT(3.247,7)", 0.8612515, 1e-6);
        approx("=F.DIST.RT(15.2069,6,4)", 0.01, 1e-4);
        approx("=F.DIST.RT(1,2,3)", 0.4647580015, 1e-9);
        approx("=T.DIST.RT(1.959,60)", 0.0273823689, 1e-9);
        approx("=T.DIST.RT(0.5,1)", 0.3524163823, 1e-6);
        assert_eq!(error("=CHISQ.DIST.RT(-1,10)"), CalcError::Num);
        assert_eq!(error("=CHISQ.DIST.RT(1,0)"), CalcError::Num);
        assert_eq!(error("=F.DIST.RT(-1,6,4)"), CalcError::Num);
        assert_eq!(error("=T.DIST.RT(-1,60)"), CalcError::Num);
    }

    #[test]
    fn inverse_distributions() {
        approx("=CHISQ.INV.RT(0.05,10)", 18.307038, 1e-4);
        approx("=CHISQ.INV.RT(0.95,10)", 3.940299, 1e-6);
        approx("=F.INV(0.95,6,4)", 6.163132, 1e-4);
        approx("=F.INV(0.99,6,4)", 15.2069, 1e-4);
        approx("=F.INV.RT(0.05,6,4)", 6.163132, 1e-4);
        approx("=T.INV.2T(0.05,60)", 2.0002978, 1e-5);
        approx("=T.INV.2T(0.05,49)", 2.0095752, 1e-5);
        approx("=T.INV.2T(1,10)", 0.0, 1e-12);
        approx("=BETA.INV(0.6854706,8,10,1,3)", 2.0, 1e-6);
        approx("=BETA.INV(0.5,2,3)", 0.3857275681, 1e-6);
        approx("=BETA.INV(0.6875,2,3)", 0.5, 1e-6);
        approx("=LOGNORM.INV(0.0390835567,3.5,1.2)", 4.0, 1e-6);
        approx("=GAMMA.INV(0.068094,9,2)", 10.0, 1e-4);
        approx("=GAMMA.INV(0.5,9,2)", 17.338, 1e-2);
        approx("=BINOM.INV(6,0.5,0.75)", 4.0, 1e-12);
        approx("=BINOM.INV(10,0.3,0.5)", 3.0, 1e-12);
        assert_eq!(error("=CHISQ.INV.RT(0,10)"), CalcError::Num);
        // Referee (2026-08): F.INV with p = 0 / blank p returns 0, not #NUM!.
        assert_eq!(num("=F.INV(0,6,4)"), 0.0);
        assert_eq!(error("=F.INV.RT(1.5,6,4)"), CalcError::Num);
        assert_eq!(error("=T.INV.2T(0,60)"), CalcError::Num);
        assert_eq!(error("=BETA.INV(0,8,10)"), CalcError::Num);
        assert_eq!(error("=LOGNORM.INV(1,3.5,1.2)"), CalcError::Num);
        assert_eq!(error("=GAMMA.INV(1,9,2)"), CalcError::Num);
        assert_eq!(error("=BINOM.INV(6,0.5,0)"), CalcError::Num);
    }

    #[test]
    fn gamma_dist_fn() {
        approx("=GAMMA.DIST(10,9,2,TRUE)", 0.068094, 1e-6);
        approx("=GAMMA.DIST(10,9,2,FALSE)", 0.032639, 1e-6);
        approx("=GAMMA.DIST(2,1,2,TRUE)", 0.6321205588, 1e-9);
        approx("=GAMMA.DIST(0,1,2,FALSE)", 0.5, 1e-12);
        assert_eq!(error("=GAMMA.DIST(-1,9,2,TRUE)"), CalcError::Num);
        assert_eq!(error("=GAMMA.DIST(1,0,2,TRUE)"), CalcError::Num);
        assert_eq!(error("=GAMMA.DIST(1,9,0,TRUE)"), CalcError::Num);
    }

    #[test]
    fn binom_dist_range_fn() {
        approx("=BINOM.DIST.RANGE(10,0.5,4)", 0.205078125, 1e-12);
        approx("=BINOM.DIST.RANGE(10,0.5,3,5)", 0.568359375, 1e-12);
        approx("=BINOM.DIST.RANGE(60,0.75,48)", 0.0839, 1e-3);
        approx("=BINOM.DIST.RANGE(60,0.75,45,50)", 0.5239, 1e-3);
        assert_eq!(error("=BINOM.DIST.RANGE(10,0.5,11)"), CalcError::Num);
        assert_eq!(error("=BINOM.DIST.RANGE(10,1.5,4)"), CalcError::Num);
        assert_eq!(error("=BINOM.DIST.RANGE(10,0.5,3,2)"), CalcError::Num);
    }

    #[test]
    fn chisq_test_example() {
        // Microsoft's published example, exact published output.
        let g = Grid::empty()
            .row("A1", &[58.0, 35.0])
            .row("A2", &[11.0, 25.0])
            .row("A3", &[10.0, 23.0])
            .row("C1", &[45.35, 47.65])
            .row("C2", &[17.56, 18.44])
            .row("C3", &[16.09, 16.91]);
        g_approx_tol("=CHISQ.TEST(A1:B3,C1:D3)", 0.0003078, 1e-3, &g);
        let mism = Grid::empty()
            .row("A1", &[1.0, 2.0])
            .row("B1", &[1.0, 2.0, 3.0]);
        assert_eq!(mism.error("=CHISQ.TEST(A1:B1,B1:D1)"), CalcError::Na);
        let bad = Grid::empty().row("A1", &[1.0, 2.0]).row("B1", &[0.0, 3.0]);
        assert_eq!(bad.error("=CHISQ.TEST(A1:B1,B1:C1)"), CalcError::Num);
    }

    #[test]
    fn f_test_fn() {
        // P(F_{2,2} > x) = 1/(1+x); F_obs = 9 -> p = 2/10 = 0.2.
        let g = Grid::empty()
            .col("A1", &[0.0, 3.0, 6.0])
            .col("B1", &[0.0, 1.0, 2.0]);
        g_approx("=F.TEST(A1:A3,B1:B3)", 0.2, &g);
        let eq = Grid::empty()
            .col("A1", &[1.0, 2.0, 3.0, 4.0])
            .col("B1", &[5.0, 6.0, 7.0, 8.0]);
        g_approx("=F.TEST(A1:A4,B1:B4)", 1.0, &eq);
        let flat = Grid::empty()
            .col("A1", &[1.0, 1.0, 1.0])
            .col("B1", &[1.0, 2.0, 3.0]);
        assert_eq!(flat.error("=F.TEST(A1:A3,B1:B3)"), CalcError::Div0);
    }

    #[test]
    fn t_test_fn() {
        // Microsoft's published paired example.
        let g = Grid::empty()
            .col("A1", &[3.0, 4.0, 5.0, 8.0, 9.0, 1.0, 2.0, 4.0, 5.0])
            .col("B1", &[6.0, 19.0, 3.0, 2.0, 14.0, 4.0, 5.0, 17.0, 1.0]);
        g_approx_tol("=T.TEST(A1:A9,B1:B9,2,1)", 0.196016, 1e-3, &g);
        g_approx_tol("=T.TEST(A1:A9,B1:B9,1,1)", 0.098008, 1e-3, &g);
        // Two samples of 5 with equal variance: t = -5, df = 8, p = 0.001053
        // (the canonical R t.test output) for both pooled and Welch.
        let w = Grid::empty()
            .col("A1", &[1.0, 2.0, 3.0, 4.0, 5.0])
            .col("B1", &[6.0, 7.0, 8.0, 9.0, 10.0]);
        g_approx_tol("=T.TEST(A1:A5,B1:B5,2,2)", 0.001053, 1e-4, &w);
        g_approx_tol("=T.TEST(A1:A5,B1:B5,2,3)", 0.001053, 1e-4, &w);
        assert_eq!(w.error("=T.TEST(A1:A5,B1:B5,3,2)"), CalcError::Num);
        assert_eq!(w.error("=T.TEST(A1:A5,B1:B5,2,4)"), CalcError::Num);
        let pa = Grid::empty()
            .col("A1", &[1.0, 2.0, 3.0])
            .col("B1", &[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(pa.error("=T.TEST(A1:A3,B1:B4,2,1)"), CalcError::Na);
    }

    #[test]
    fn z_test_fn() {
        let g = Grid::empty().col("A1", &[3.0, 6.0, 7.0, 8.0, 6.0, 5.0, 4.0, 2.0, 1.0, 9.0]);
        g_approx_tol("=Z.TEST(A1:A10,4)", 0.090574, 1e-4, &g);
        g_approx_tol("=Z.TEST(A1:A10,4,1.5)", 0.0102, 1e-4, &g);
        assert_eq!(g.error("=Z.TEST(A1:A10,4,0)"), CalcError::Num);
        assert_eq!(Grid::empty().error("=Z.TEST(A1:A3,0)"), CalcError::Na);
    }

    #[test]
    fn confidence() {
        approx("=CONFIDENCE.NORM(0.05,2.5,50)", 0.692951, 1e-6);
        approx("=CONFIDENCE.T(0.05,1,50)", 0.284196, 1e-6);
        assert_eq!(error("=CONFIDENCE.NORM(0,2.5,50)"), CalcError::Num);
        assert_eq!(error("=CONFIDENCE.T(0.05,0,50)"), CalcError::Num);
        assert_eq!(error("=CONFIDENCE.NORM(0.05,2.5,0)"), CalcError::Num);
    }

    #[test]
    fn linest_single_predictor() {
        let g = Grid::empty()
            .col("A1", &[1.0, 2.0, 3.0, 5.0])
            .col("B1", &[1.0, 2.0, 3.0, 4.0]);
        // Without stats: one row [slope, intercept].
        let simple = g.array("=LINEST(A1:A4,B1:B4,TRUE,FALSE)");
        assert_eq!(simple.shape(), (1, 2));
        approx_cell(simple.data[0].as_number().unwrap(), 1.3);
        approx_cell(simple.data[1].as_number().unwrap(), -0.5);
        // Full statistics in Excel's 5-row layout (hand-computed reference).
        let full = g.array("=LINEST(A1:A4,B1:B4,TRUE,TRUE)");
        assert_eq!(full.shape(), (5, 2));
        approx_cell(num_at(&full, 0, 0), 1.3);
        approx_cell(num_at(&full, 0, 1), -0.5);
        approx_cell(num_at(&full, 1, 0), 0.173205);
        approx_cell(num_at(&full, 1, 1), 0.474342);
        approx_cell(num_at(&full, 2, 0), 0.965714);
        approx_cell(num_at(&full, 2, 1), 0.387298);
        approx_cell(num_at(&full, 3, 0), 56.3333);
        approx_cell(num_at(&full, 3, 1), 2.0);
        approx_cell(num_at(&full, 4, 0), 8.45);
        approx_cell(num_at(&full, 4, 1), 0.30);
        // const=FALSE: regression through the origin; a 5x1 stats array whose
        // rows 3-5 carry only their first statistic (no SE_y/df/SSresid column).
        let origin = g.array("=LINEST(A1:A4,B1:B4,FALSE,TRUE)");
        assert_eq!(origin.shape(), (5, 1));
        approx_cell(origin.data[0].as_number().unwrap(), 1.133333);
        approx_cell(origin.data[1].as_number().unwrap(), 0.072);
        approx_cell(origin.data[2].as_number().unwrap(), 0.988034);
        approx_cell(origin.data[3].as_number().unwrap(), 247.714);
        approx_cell(origin.data[4].as_number().unwrap(), 38.5333);
        // known_x's omitted -> x is 1..n
        let gx = Grid::empty().col("A1", &[2.0, 4.0, 6.0, 8.0]);
        let nos = gx.array("=LINEST(A1:A4)");
        assert_eq!(nos.shape(), (1, 2));
        approx_cell(nos.data[0].as_number().unwrap(), 2.0);
        approx_cell(nos.data[1].as_number().unwrap(), 0.0);
        let mism = Grid::empty()
            .col("A1", &[1.0, 2.0, 3.0])
            .col("B1", &[1.0, 2.0]);
        assert_eq!(mism.error("=LINEST(A1:A3,B1:B2)"), CalcError::Ref);
    }

    #[test]
    fn linest_multiple_predictors() {
        // Hand-computed reference: y = 0.428571 x1 + 1.428571 x2 + 2.357143.
        let g = Grid::empty()
            .col("A1", &[4.0, 8.0, 9.0, 7.0])
            .col("B1", &[1.0, 2.0, 3.0, 4.0])
            .col("C1", &[1.0, 3.0, 4.0, 2.0]);
        let full = g.array("=LINEST(A1:A4,B1:C4,TRUE,TRUE)");
        assert_eq!(full.shape(), (5, 3));
        approx_cell(num_at(&full, 0, 0), 1.428571);
        approx_cell(num_at(&full, 0, 1), 0.428571);
        approx_cell(num_at(&full, 0, 2), 2.357143);
        approx_cell(num_at(&full, 1, 0), 0.319438);
        approx_cell(num_at(&full, 1, 1), 0.319438);
        approx_cell(num_at(&full, 1, 2), 0.934050);
        approx_cell(num_at(&full, 2, 0), 0.969388);
        approx_cell(num_at(&full, 2, 1), 0.654654);
        assert!(matches!(full.get(2, 2), CalcValue::Error(CalcError::Na)));
        approx_cell(num_at(&full, 3, 0), 15.8333);
        approx_cell(num_at(&full, 3, 1), 1.0);
        assert!(matches!(full.get(3, 2), CalcValue::Error(CalcError::Na)));
        approx_cell(num_at(&full, 4, 0), 13.571428);
        approx_cell(num_at(&full, 4, 1), 0.428571);
    }

    #[test]
    fn trend_and_growth() {
        let y = [1.0, 2.0, 3.0, 5.0];
        let x = [1.0, 2.0, 3.0, 4.0];
        let g = Grid::empty().col("A1", &y).col("B1", &x);
        let fit = g.array("=TREND(A1:A4,B1:B4)");
        assert_eq!(fit.shape(), (4, 1));
        approx_cell(num_at(&fit, 0, 0), 0.8);
        approx_cell(num_at(&fit, 1, 0), 2.1);
        approx_cell(num_at(&fit, 2, 0), 3.4);
        approx_cell(num_at(&fit, 3, 0), 4.7);
        let g2 = Grid::empty()
            .col("A1", &y)
            .col("B1", &x)
            .set_num("D1", 5.0)
            .set_num("D2", 6.0);
        let ext = g2.array("=TREND(A1:A4,B1:B4,D1:D2)");
        assert_eq!(ext.shape(), (2, 1));
        approx_cell(num_at(&ext, 0, 0), 6.0);
        approx_cell(num_at(&ext, 1, 0), 7.3);
    }

    #[test]
    fn logest_and_growth() {
        // y = 5 * e^(2x) exactly: LOGEST -> m = 2, b = 5.
        let x: [f64; 4] = [1.0, 2.0, 3.0, 4.0];
        let y: Vec<f64> = x.iter().map(|&xi| 5.0 * (2.0 * xi).exp()).collect();
        let g = Grid::empty().col("A1", &y).col("B1", &x);
        let logest = g.array("=LOGEST(A1:A4,B1:B4,TRUE,FALSE)");
        assert_eq!(logest.shape(), (1, 2));
        approx_cell(logest.data[0].as_number().unwrap(), 2.0);
        approx_cell(logest.data[1].as_number().unwrap(), 5.0);
        let growth = g.array("=GROWTH(A1:A4,B1:B4)");
        assert_eq!(growth.shape(), (4, 1));
        for (i, v) in y.iter().enumerate() {
            approx_cell(num_at(&growth, i as u32, 0), *v);
        }
        let g2 = Grid::empty()
            .col("A1", &y)
            .col("B1", &x)
            .set_num("D1", 5.0)
            .set_num("D2", 6.0);
        let ext = g2.array("=GROWTH(A1:A4,B1:B4,D1:D2)");
        approx_cell(num_at(&ext, 0, 0), 5.0 * (10.0f64).exp());
        approx_cell(num_at(&ext, 1, 0), 5.0 * (12.0f64).exp());
    }

    #[test]
    fn phi_gauss_prob_frequency_skewp() {
        approx("=PHI(2)", 0.053990967, 1e-9);
        approx("=PHI(0)", 0.3989422804, 1e-9);
        approx("=GAUSS(0.5)", 0.1914624613, 1e-9);
        approx("=GAUSS(0)", 0.0, 1e-12);
        assert_eq!(error("=GAUSS(11)"), CalcError::Num);
        assert_eq!(error("=GAUSS(-11)"), CalcError::Num);
        // PROB published examples
        let g = Grid::empty()
            .col("A1", &[3.0, 4.0, 5.0, 6.0])
            .col("B1", &[0.2, 0.3, 0.4, 0.1]);
        g_approx("=PROB(A1:A4,B1:B4,4,5)", 0.7, &g);
        let g2 = Grid::empty()
            .col("A1", &[0.0, 1.0, 2.0, 3.0])
            .col("B1", &[0.2, 0.3, 0.1, 0.4]);
        g_approx("=PROB(A1:A4,B1:B4,2)", 0.1, &g2);
        let g3 = Grid::empty().col("A1", &[1.0, 2.0]).col("B1", &[0.5, 0.4]);
        assert_eq!(g3.error("=PROB(A1:A2,B1:B2,1)"), CalcError::Num);
        // FREQUENCY: one bucket more than the bins, first-bin semantics
        let f = Grid::empty()
            .col("A1", &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0])
            .col("B1", &[2.0, 4.0]);
        let fr = f.array("=FREQUENCY(A1:A6,B1:B2)");
        assert_eq!(fr.shape(), (3, 1));
        approx_cell(num_at(&fr, 0, 0), 2.0);
        approx_cell(num_at(&fr, 1, 0), 2.0);
        approx_cell(num_at(&fr, 2, 0), 2.0);
        // SKEW.P is the population skewness (docs value 0.303193 rounded).
        let s = Grid::empty().col("A1", &[3.0, 4.0, 5.0, 2.0, 3.0, 4.0, 5.0, 6.0, 4.0, 7.0]);
        g_approx_tol("=SKEW.P(A1:A10)", 0.303193, 1e-6, &s);
        assert_eq!(s.error("=SKEW.P(A1:A2)"), CalcError::Div0);
    }

    // -- Lane C round-2 referee corrections (local Excel, 2026-08) ------------

    #[test]
    fn chidist_chinv_compatibility_aliases() {
        // CHIDIST(x,df) = CHISQ.DIST.RT(x,df); CHIINV(p,df) = CHISQ.INV.RT(p,df).
        approx("=CHIDIST(3.247,7)", 0.8612515, 1e-6);
        approx("=CHIDIST(18.307,10)", 0.05, 1e-4);
        approx("=CHIINV(0.05,10)", 18.307038, 1e-4);
        approx("=CHIINV(0.95,10)", 3.940299, 1e-6);
        assert_eq!(error("=CHIDIST(1,0)"), CalcError::Num);
        assert_eq!(error("=CHIINV(0,10)"), CalcError::Num);
    }

    #[test]
    fn chitest_1x1_or_empty_ranges_are_value_error() {
        // Referee: CHISQ.TEST/CHITEST on 1x1 or empty ranges is #VALUE!, not
        // #N/A (Univer's claim) and not the old #NUM!.
        assert_eq!(
            Grid::empty().error("=CHISQ.TEST(A7:A7,A8:A8)"),
            CalcError::Value
        );
        assert_eq!(
            Grid::empty().error("=CHISQ.TEST(BZ1,CA1)"),
            CalcError::Value
        );
    }

    #[test]
    fn chisq_test_propagates_incoming_errors() {
        assert_eq!(error("=CHISQ.TEST(#NAME?,BZ1:CA2)"), CalcError::Name);
    }

    #[test]
    fn covariance_p_propagates_incoming_errors() {
        assert_eq!(error("=COVARIANCE.P(#NAME?,A9:B11)"), CalcError::Name);
        assert_eq!(error("=COVARIANCE.P(A9:B11,#REF!)"), CalcError::Ref);
    }

    #[test]
    fn covariance_p_empty_args_are_value_error() {
        // Referee: empty args -> #VALUE!; the plan's "single pair -> 0" is wrong.
        assert_eq!(error("=COVARIANCE.P(S1,T1)"), CalcError::Value);
        assert_eq!(error("=COVARIANCE.S(S1,T1)"), CalcError::Value);
    }

    #[test]
    fn t_test_propagates_incoming_errors() {
        assert_eq!(error("=T.TEST(#NAME?,A5:D5,2,1)"), CalcError::Name);
        assert_eq!(error("=T.TEST(A5:D5,#REF!,2,1)"), CalcError::Ref);
    }

    #[test]
    fn percentrank_trailing_comma_defaults_to_3_digits() {
        // Trailing-comma empty significance defaults to 3 digits; Excel
        // truncates rather than rounds (0.255, not 0.256).
        let g = Grid::empty().row("A1", &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0]);
        assert_eq!(g.num("=PERCENTRANK.INC(A1:J1,3.3,)"), 0.255);
        assert_eq!(g.num("=PERCENTRANK.EXC(A1:J1,3.3,)"), 0.3);
    }

    #[test]
    fn percentrank_empty_ref_significance_is_na() {
        // Referee: a blank *cell-reference* significance argument is #N/A —
        // distinct from the trailing-comma (omitted) case in the test above.
        assert_eq!(
            Grid::empty().error("=PERCENTRANK.EXC(EX1:FG1,3.3,EX9001)"),
            CalcError::Na
        );
        assert_eq!(
            Grid::empty().error("=PERCENTRANK.INC(FV1:GE1,3.3,FV9001)"),
            CalcError::Na
        );
    }

    #[test]
    fn large_small_round_non_integer_k() {
        // Referee: LARGE(...,2.5) rounds k up to 3 (8); the old truncation gave
        // k=2 (9).
        let g = Grid::empty().col("A1", &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0]);
        assert_eq!(g.num("=LARGE(A1:A10,2.5)"), 8.0);
        assert_eq!(g.num("=SMALL(A1:A10,2.5)"), 3.0);
    }

    #[test]
    fn f_inv_blank_p_is_zero() {
        assert_eq!(num("=F.INV(0,6,4)"), 0.0);
        assert_eq!(Grid::empty().num("=F.INV(BP9001,6,4)"), 0.0);
        assert_eq!(error("=F.INV(-0.1,6,4)"), CalcError::Num);
    }

    #[test]
    fn f_inv_rt_p_is_one_is_zero() {
        assert_eq!(num("=F.INV.RT(TRUE,6,4)"), 0.0);
        assert_eq!(num("=F.INV.RT(1,6,4)"), 0.0);
        assert_eq!(error("=F.INV.RT(0,6,4)"), CalcError::Num);
        assert_eq!(error("=F.INV.RT(1.5,6,4)"), CalcError::Num);
    }

    #[test]
    fn fisherinv_large_arguments_saturate_at_plus_minus_one() {
        approx("=FISHERINV(9999999.23658)", 1.0, 1e-9);
        approx("=FISHERINV(-9999999.23658)", -1.0, 1e-9);
    }

    #[test]
    fn gamma_accepts_numeric_strings_and_negative_non_integers() {
        // Referee: GAMMA("-2.5") -> -0.945308720482942 (numeric string accepted,
        // reflection formula handles negative non-integers).
        approx("=GAMMA(\"-2.5\")", -0.945308720482942, 1e-9);
        approx("=GAMMA(-0.5)", -3.544907701811032, 1e-9);
        // Poles at non-positive integers remain #NUM!.
        assert_eq!(error("=GAMMA(-1)"), CalcError::Num);
        assert_eq!(error("=GAMMA(0)"), CalcError::Num);
    }

    #[test]
    fn countblank_propagates_literal_error_arguments() {
        assert_eq!(error("=COUNTBLANK(#REF!)"), CalcError::Ref);
        assert_eq!(error("=COUNTBLANK(#N/A)"), CalcError::Na);
    }

    #[test]
    fn prob_mismatched_range_sizes_are_na() {
        assert_eq!(
            Grid::empty().error("=PROB(AF1:AJ1,AN1:AQ1,2)"),
            CalcError::Na
        );
    }

    #[test]
    fn legacy_excel_names_are_registered_and_route_to_modern_semantics() {
        let names = [
            "BETADIST",
            "BETAINV",
            "BINOMDIST",
            "CHITEST",
            "CONFIDENCE",
            "COVAR",
            "CRITBINOM",
            "EXPONDIST",
            "FDIST",
            "FINV",
            "FTEST",
            "GAMMADIST",
            "GAMMAINV",
            "HYPGEOMDIST",
            "LOGINV",
            "LOGNORMDIST",
            "MODE",
            "NEGBINOMDIST",
            "NORMDIST",
            "NORMINV",
            "NORMSDIST",
            "NORMSINV",
            "PERCENTILE",
            "PERCENTRANK",
            "POISSON",
            "QUARTILE",
            "RANK",
            "STDEV",
            "STDEVP",
            "TDIST",
            "TINV",
            "TTEST",
            "VAR",
            "VARP",
            "WEIBULL",
            "ZTEST",
            "FORECAST",
            "GAMMALN.PRECISE",
        ];
        for name in names {
            assert!(
                crate::turbo::calc::functions::registry()
                    .get(name)
                    .is_some(),
                "{name} is absent from the function registry"
            );
        }

        let equivalent = [
            ("=BETADIST(0.5,2,3,0,1)", "=BETA.DIST(0.5,2,3,TRUE,0,1)"),
            ("=HYPGEOMDIST(1,4,8,20)", "=HYPGEOM.DIST(1,4,8,20,FALSE)"),
            ("=LOGNORMDIST(4,3.5,1.2)", "=LOGNORM.DIST(4,3.5,1.2,TRUE)"),
            (
                "=NEGBINOMDIST(10,5,0.25)",
                "=NEGBINOM.DIST(10,5,0.25,FALSE)",
            ),
            ("=NORMSDIST(1.5)", "=NORM.S.DIST(1.5,TRUE)"),
            ("=TDIST(1.5,10,1)", "=T.DIST.RT(1.5,10)"),
            ("=TDIST(1.5,10,2)", "=T.DIST.2T(1.5,10)"),
            ("=TINV(0.05,10)", "=T.INV.2T(0.05,10)"),
            ("=FDIST(1.5,10,12)", "=F.DIST.RT(1.5,10,12)"),
            ("=FINV(0.05,10,12)", "=F.INV.RT(0.05,10,12)"),
        ];
        for (legacy, modern) in equivalent {
            approx(legacy, num(modern), 1e-12);
        }
        assert_eq!(error("=TDIST(1.5,10,3)"), CalcError::Num);
    }
}
