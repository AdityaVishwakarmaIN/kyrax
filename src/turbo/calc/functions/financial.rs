// functions/financial.rs — the financial function family. Owned exclusively
// by the financial family agent; no other agent edits this file.
//
// Registry contract: implement `register` below and keep this exact signature.
// Do NOT edit functions/mod.rs — the `mod financial;` declaration and the
// `financial::register(&mut r)` call site in `build()` are already final.
// See functions/mod.rs for the worked ABS template.
//
// Conventions that keep this family honest:
//   * Sign: cash outflows are negative and inflows positive, exactly like
//     Excel. PMT for a loan returns a negative number.
//   * `type` (0 = end of period, 1 = beginning) affects PV, FV, PMT, IPMT,
//     PPMT, NPER, CUMIPMT, CUMPRINC, RATE; anything else is #NUM!.
//   * `basis` (0 = US 30/360, 1 = actual/actual, 2 = actual/360,
//     3 = actual/365, 4 = European 30/360) is implemented once in
//     `day_count` / `year_fraction` and every securities function routes
//     through it, including the fiddly US end-of-February rules.
//   * Date arguments are Excel serial numbers, including the 1900 leap-year
//     bug (serial 60 = the phantom 1900-02-29).
//   * RATE, IRR, XIRR, YIELD have no closed form: Newton-Raphson starting
//     from 0.1 (or the caller's guess), 100 iterations, relative tolerance
//     1e-9. Non-convergence is #NUM!, never a non-converged value. IRR/XIRR
//     require at least one positive and one negative cash flow.
use super::{FuncArg, FuncCtx, FuncSpec, Registry};
use crate::turbo::calc::coerce::coerce_number;
use crate::turbo::calc::value::{CalcError, CalcValue};

fn ok_num(n: f64) -> Result<CalcValue, CalcError> {
    if n.is_finite() {
        Ok(CalcValue::Number(n))
    } else {
        Err(CalcError::Num)
    }
}

fn arg_num(ctx: &FuncCtx, arg: &FuncArg) -> Result<f64, CalcError> {
    coerce_number(&arg.value(ctx)?)
}

/// Coerce to number but reject booleans outright — Excel's EFFECT, NOMINAL,
/// DOLLARDE, DOLLARFR and FVSCHEDULE type their numeric arguments strictly and
/// return #VALUE! for a TRUE/FALSE where a rate or fraction belongs.
fn arg_num_typed(ctx: &FuncCtx, arg: &FuncArg) -> Result<f64, CalcError> {
    let v = arg.value(ctx)?;
    if matches!(v, CalcValue::Bool(_)) {
        return Err(CalcError::Value);
    }
    coerce_number(&v)
}

fn opt_num(ctx: &FuncCtx, args: &[FuncArg], i: usize, d: f64) -> Result<f64, CalcError> {
    if i < args.len() {
        arg_num(ctx, &args[i])
    } else {
        Ok(d)
    }
}

fn pay_type(t: f64) -> Result<f64, CalcError> {
    let t = t.trunc();
    if t == 0.0 || t == 1.0 {
        Ok(t)
    } else {
        Err(CalcError::Num)
    }
}

fn check_basis(b: f64) -> Result<u8, CalcError> {
    let b = b.trunc();
    if (0.0..=4.0).contains(&b) {
        Ok(b as u8)
    } else {
        Err(CalcError::Num)
    }
}

fn check_freq(f: f64) -> Result<i64, CalcError> {
    let f = f.trunc();
    if f == 1.0 || f == 2.0 || f == 4.0 {
        Ok(f as i64)
    } else {
        Err(CalcError::Num)
    }
}

// -- serial date arithmetic (1900 system incl. the leap-year bug) ------------

const DAYS_1899_TO_1970: i64 = 25568;
const DAYS_1899_TO_1904: i64 = 1461;

fn clamp_serial(s: f64) -> i64 {
    let s = s.trunc();
    if s > 1e7 {
        10_000_000
    } else if s < -1e7 {
        -10_000_000
    } else {
        s as i64
    }
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}

fn serial_to_civil(serial: f64, date1904: bool) -> (i64, i64, i64) {
    let s = clamp_serial(serial);
    if date1904 {
        civil_from_days(s + DAYS_1899_TO_1904 - DAYS_1899_TO_1970)
    } else if s == 0 {
        (1900, 1, 0)
    } else if s == 60 {
        (1900, 2, 29)
    } else {
        let real_day = if s <= 59 { s } else { s - 1 };
        civil_from_days(real_day - DAYS_1899_TO_1970)
    }
}

fn civil_to_serial(y: i64, m: i64, d: i64, date1904: bool) -> i64 {
    if !date1904 && (y, m, d) == (1900, 2, 29) {
        return 60;
    }
    let real_day = days_from_civil(y, m, d) + DAYS_1899_TO_1970;
    if date1904 {
        real_day - DAYS_1899_TO_1904
    } else if real_day <= 59 {
        real_day
    } else {
        real_day + 1
    }
}

fn is_leap_system(y: i64, date1904: bool) -> bool {
    if y == 1900 && !date1904 {
        return true;
    }
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: i64, date1904: bool) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_system(y, date1904) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

// -- day-count basis (ODF 4.11.7, shared by every securities function) -------

fn last_day_of_feb(y: i64, m: i64, d: i64, date1904: bool) -> bool {
    m == 2 && d == days_in_month(y, 2, date1904)
}

/// Basis 0: US (NASD) 30/360. The end-of-February rules are the fiddly part.
fn days_360_us(s1: f64, s2: f64, date1904: bool) -> i64 {
    let (mut y1, mut m1, mut d1) = serial_to_civil(s1, date1904);
    let (mut y2, mut m2, mut d2) = serial_to_civil(s2, date1904);
    if (y1, m1, d1) == (y2, m2, d2) {
        return 0;
    }
    if days_from_civil(y1, m1, d1) > days_from_civil(y2, m2, d2) {
        std::mem::swap(&mut y1, &mut y2);
        std::mem::swap(&mut m1, &mut m2);
        std::mem::swap(&mut d1, &mut d2);
    }
    if d1 == 31 {
        d1 = 30;
    }
    if d1 == 30 && d2 == 31 {
        d2 = 30;
    }
    if last_day_of_feb(y1, m1, d1, date1904) && last_day_of_feb(y2, m2, d2, date1904) {
        d2 = 30;
    }
    if last_day_of_feb(y1, m1, d1, date1904) {
        d1 = 30;
    }
    (y2 * 360 + m2 * 30 + d2) - (y1 * 360 + m1 * 30 + d1)
}

/// Basis 4: European 30/360.
fn days_360_eu(s1: f64, s2: f64, date1904: bool) -> i64 {
    let (mut y1, mut m1, mut d1) = serial_to_civil(s1, date1904);
    let (mut y2, mut m2, mut d2) = serial_to_civil(s2, date1904);
    if (y1, m1, d1) == (y2, m2, d2) {
        return 0;
    }
    if days_from_civil(y1, m1, d1) > days_from_civil(y2, m2, d2) {
        std::mem::swap(&mut y1, &mut y2);
        std::mem::swap(&mut m1, &mut m2);
        std::mem::swap(&mut d1, &mut d2);
    }
    if d1 == 31 {
        d1 = 30;
    }
    if d2 == 31 {
        d2 = 30;
    }
    (y2 * 360 + m2 * 30 + d2) - (y1 * 360 + m1 * 30 + d1)
}

/// Basis 1 days-in-year (Excel semantics). The average number of days per year
/// over the span: `(last day of the end year - first day of the start year + 1)
/// / number of years spanned` — so a same-year span yields that year's full
/// length, 366 for a leap year even when the interval itself holds no
/// 29-February. Truncating this to an integer misprices every basis-1
/// security, and the "Feb 29 strictly between" rule is NOT what Excel uses.
fn actual_actual_days_in_year(s1: f64, s2: f64, date1904: bool) -> f64 {
    let (mut y1, _, _) = serial_to_civil(s1, date1904);
    let (mut y2, _, _) = serial_to_civil(s2, date1904);
    if y1 > y2 {
        std::mem::swap(&mut y1, &mut y2);
    }
    let total_year = (y2 - y1 + 1) as f64;
    let mut start_first = civil_to_serial(y1, 1, 1, date1904);
    let end_last = civil_to_serial(y2, 12, 31, date1904);
    if !date1904 && y1 == 1900 {
        // The 1900 system counts 1900 as 365 real days (the leap bug only
        // invents 1900-02-29), so the serial arithmetic needs a one-day shave.
        start_first += 1;
    }
    (end_last - start_first + 1) as f64 / total_year
}

/// Days between two serials under a basis. `s1 <= s2` for every internal call.
fn day_count(s1: f64, s2: f64, basis: u8, date1904: bool) -> f64 {
    match basis {
        0 => days_360_us(s1, s2, date1904) as f64,
        1..=3 => (clamp_serial(s2) - clamp_serial(s1)) as f64,
        _ => days_360_eu(s1, s2, date1904) as f64,
    }
}

/// Days per year for the "B" factor (bases 0/2/4 -> 360, 3 -> 365, 1 -> actual).
fn days_in_year(s1: f64, s2: f64, basis: u8, date1904: bool) -> f64 {
    match basis {
        0 | 2 | 4 => 360.0,
        3 => 365.0,
        _ => actual_actual_days_in_year(s1, s2, date1904),
    }
}

/// Days in the single calendar year that contains `serial` (the "B" factor for
/// basis 1 in PRICEMAT / YIELDMAT / AMORLINC, which Excel anchors on the year
/// of the settlement date, not the average over the whole span).
fn days_in_year_of(serial: f64, basis: u8, date1904: bool) -> f64 {
    match basis {
        0 | 2 | 4 => 360.0,
        3 => 365.0,
        _ => {
            let (y, _, _) = serial_to_civil(serial, date1904);
            if is_leap_system(y, date1904) {
                366.0
            } else {
                365.0
            }
        }
    }
}

fn year_fraction(s1: f64, s2: f64, basis: u8, date1904: bool) -> f64 {
    match basis {
        0 => days_360_us(s1, s2, date1904) as f64 / 360.0,
        1 => {
            (clamp_serial(s2) - clamp_serial(s1)) as f64
                / actual_actual_days_in_year(s1, s2, date1904)
        }
        2 => (clamp_serial(s2) - clamp_serial(s1)) as f64 / 360.0,
        3 => (clamp_serial(s2) - clamp_serial(s1)) as f64 / 365.0,
        _ => days_360_eu(s1, s2, date1904) as f64 / 360.0,
    }
}

// -- Newton-Raphson (RATE, IRR, XIRR, YIELD; no closed form) ------------------

fn newtown<F: Fn(f64) -> f64, D: Fn(f64) -> f64>(f: &F, df: &D, mut x: f64) -> Option<f64> {
    for _ in 0..100 {
        let v = f(x);
        let d = df(x);
        if !v.is_finite() || !d.is_finite() || d == 0.0 {
            return None;
        }
        let step = v / d;
        let xn = x - step;
        if !xn.is_finite() {
            return None;
        }
        if step.abs() <= 1e-9 * xn.abs().max(1e-12) {
            return Some(xn);
        }
        x = xn;
    }
    None
}

fn solve_robust<F: Fn(f64) -> f64, D: Fn(f64) -> f64>(
    f: &F,
    df: &D,
    guesses: &[f64],
) -> Option<f64> {
    for g in guesses {
        if let Some(x) = newtown(f, df, *g) {
            if x.is_finite() {
                return Some(x);
            }
        }
    }
    None
}

// -- range / scalar collection for array-aware functions ---------------------

fn collect_values(ctx: &FuncCtx, args: &[FuncArg]) -> Result<Vec<f64>, CalcError> {
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

// -- time value of money ------------------------------------------------------

/// The constant payment solving the time-value equation, with `t` = payment
/// timing (0 = end, 1 = beginning).
fn payment(pv: f64, fv: f64, r: f64, n: f64, t: f64) -> f64 {
    if r == 0.0 {
        -(pv + fv) / n
    } else {
        -(pv * (1.0 + r).powf(n) + fv) * r / ((1.0 + r * t) * ((1.0 + r).powf(n) - 1.0))
    }
}

/// The future value at time n of `pv` plus `pmt` per period, negated exactly
/// as Excel's FV does, with `advance` = payments at the beginning of periods.
fn get_fv(rate: f64, nper: f64, pmt: f64, pv: f64, advance: bool) -> f64 {
    let fv = if rate == 0.0 {
        pv + pmt * nper
    } else {
        let term = (1.0 + rate).powf(nper);
        if advance {
            pv * term + pmt * (1.0 + rate) * (term - 1.0) / rate
        } else {
            pv * term + pmt * (term - 1.0) / rate
        }
    };
    -fv
}

fn get_pmt(rate: f64, nper: f64, pv: f64, fv: f64, advance: bool) -> f64 {
    if nper == 0.0 {
        return f64::NAN;
    }
    let payment = if rate == 0.0 {
        (pv + fv) / nper
    } else {
        let l = nper * rate.ln_1p();
        let base = fv + pv * l.exp();
        let denom = if advance {
            ((nper + 1.0) * rate.ln_1p()).exp_m1() - rate
        } else {
            l.exp_m1()
        };
        base * rate / denom
    };
    -payment
}

/// (payment, interest-in-period) matching Excel's IPMT: interest on the
/// balance at the start of the period, with type-1 period 1 interest zero.
fn get_ipmt(rate: f64, per: f64, nper: f64, pv: f64, fv: f64, advance: bool) -> (f64, f64) {
    let pmt = get_pmt(rate, nper, pv, fv, advance);
    let ipmt = if per == 1.0 {
        if advance { 0.0 } else { -pv }
    } else if advance {
        get_fv(rate, per - 2.0, pmt, pv, true) - pmt
    } else {
        get_fv(rate, per - 1.0, pmt, pv, false)
    };
    (pmt, ipmt * rate)
}

fn pv_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let r = arg_num(ctx, &args[0])?;
    let n = arg_num(ctx, &args[1])?;
    let pmt = arg_num(ctx, &args[2])?;
    let fv = opt_num(ctx, args, 3, 0.0)?;
    let t = pay_type(opt_num(ctx, args, 4, 0.0)?)?;
    let result = if r == 0.0 {
        -(fv + pmt * n)
    } else {
        -(fv + pmt * (1.0 + r * t) * (((1.0 + r).powf(n) - 1.0) / r)) / (1.0 + r).powf(n)
    };
    ok_num(result)
}

fn fv_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let r = arg_num(ctx, &args[0])?;
    let n = arg_num(ctx, &args[1])?;
    let pmt = arg_num(ctx, &args[2])?;
    let pv = opt_num(ctx, args, 3, 0.0)?;
    let t = pay_type(opt_num(ctx, args, 4, 0.0)?)?;
    // Excel guards the (1+rate)^nper degenerate case: FV(-1, 0, ...) is #NUM!.
    if r == -1.0 && n == 0.0 {
        return Err(CalcError::Num);
    }
    let result = if r == 0.0 {
        -(pv + pmt * n)
    } else {
        -(pv * (1.0 + r).powf(n) + pmt * (1.0 + r * t) * (((1.0 + r).powf(n) - 1.0) / r))
    };
    ok_num(result)
}

fn pmt_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let r = arg_num(ctx, &args[0])?;
    let n = arg_num(ctx, &args[1])?;
    let pv = arg_num(ctx, &args[2])?;
    let fv = opt_num(ctx, args, 3, 0.0)?;
    let t = pay_type(opt_num(ctx, args, 4, 0.0)?)?;
    if n == 0.0 {
        return Err(CalcError::Div0);
    }
    ok_num(payment(pv, fv, r, n, t))
}

fn nper_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let r = arg_num(ctx, &args[0])?;
    let pmt = arg_num(ctx, &args[1])?;
    let pv = arg_num(ctx, &args[2])?;
    let fv = opt_num(ctx, args, 3, 0.0)?;
    let t = pay_type(opt_num(ctx, args, 4, 0.0)?)?;
    let result = if pv + fv == 0.0 {
        0.0
    } else if r == 0.0 {
        if pmt == 0.0 {
            return Err(CalcError::Div0);
        }
        -(pv + fv) / pmt
    } else {
        if (1.0 + r) <= 0.0 {
            return Err(CalcError::Num);
        }
        let a = pmt * (1.0 + r * t) - fv * r;
        let b = pv * r + pmt * (1.0 + r * t);
        if b == 0.0 {
            return Err(CalcError::Div0);
        }
        let ratio = a / b;
        if ratio <= 0.0 {
            return Err(CalcError::Num);
        }
        ratio.ln() / (1.0 + r).ln()
    };
    ok_num(result)
}

fn rate_residual(r: f64, n: f64, pmt: f64, pv: f64, fv: f64, t: f64) -> f64 {
    if r <= -1.0 {
        return f64::NAN;
    }
    if r == 0.0 {
        pv + pmt * n + fv
    } else {
        pv * (1.0 + r).powf(n) + pmt * (1.0 + r * t) * (((1.0 + r).powf(n) - 1.0) / r) + fv
    }
}

fn rate_deriv(r: f64, n: f64, pmt: f64, pv: f64, t: f64) -> f64 {
    if r <= -1.0 {
        return f64::NAN;
    }
    if r == 0.0 {
        pv * n + pmt * (t * n + n * (n - 1.0) / 2.0)
    } else {
        let u = (1.0 + r).powf(n);
        pv * n * (1.0 + r).powf(n - 1.0)
            + pmt * (1.0 + r * t) * (n * (1.0 + r).powf(n - 1.0) * r - (u - 1.0)) / (r * r)
            + pmt * t * (u - 1.0) / r
    }
}

fn rate_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let nper = arg_num(ctx, &args[0])?;
    let pmt = arg_num(ctx, &args[1])?;
    let pv = arg_num(ctx, &args[2])?;
    let fv = opt_num(ctx, args, 3, 0.0)?;
    let t = pay_type(opt_num(ctx, args, 4, 0.0)?)?;
    let guess = opt_num(ctx, args, 5, 0.1)?;
    if nper <= 0.0 {
        return Err(CalcError::Num);
    }
    let f = |r| rate_residual(r, nper, pmt, pv, fv, t);
    let df = |r| rate_deriv(r, nper, pmt, pv, t);
    let x = solve_robust(&f, &df, &[guess, 0.1, 0.5, -0.5, 2.0]).ok_or(CalcError::Num)?;
    ok_num(x)
}

fn ipmt_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let r = arg_num(ctx, &args[0])?;
    let per = arg_num(ctx, &args[1])?;
    let nper = arg_num(ctx, &args[2])?;
    let pv = arg_num(ctx, &args[3])?;
    let fv = opt_num(ctx, args, 4, 0.0)?;
    let t = pay_type(opt_num(ctx, args, 5, 0.0)?)?;
    if per < 1.0 || per > nper {
        return Err(CalcError::Num);
    }
    let (_, ip) = get_ipmt(r, per, nper, pv, fv, t == 1.0);
    ok_num(ip)
}

fn ppmt_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let r = arg_num(ctx, &args[0])?;
    let per = arg_num(ctx, &args[1])?;
    let nper = arg_num(ctx, &args[2])?;
    let pv = arg_num(ctx, &args[3])?;
    let fv = opt_num(ctx, args, 4, 0.0)?;
    let t = pay_type(opt_num(ctx, args, 5, 0.0)?)?;
    if per < 1.0 || per > nper {
        return Err(CalcError::Num);
    }
    let (pmt, ip) = get_ipmt(r, per, nper, pv, fv, t == 1.0);
    ok_num(pmt - ip)
}

fn cumipmt_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let r = arg_num(ctx, &args[0])?;
    let nper = arg_num(ctx, &args[1])?;
    let pv = arg_num(ctx, &args[2])?;
    let start = arg_num(ctx, &args[3])?.trunc();
    let end = arg_num(ctx, &args[4])?.trunc();
    let t = pay_type(arg_num(ctx, &args[5])?)?;
    if r <= 0.0 || pv <= 0.0 || nper <= 0.0 || start < 1.0 || end < start || end > nper {
        return Err(CalcError::Num);
    }
    let advance = t == 1.0;
    let pmt = get_pmt(r, nper, pv, 0.0, advance);
    let mut total = 0.0;
    let mut n = start as i64;
    if n == 1 {
        if !advance {
            total = -pv;
        }
        n += 1;
    }
    for i in n..=(end as i64) {
        if advance {
            total += get_fv(r, i as f64 - 2.0, pmt, pv, true) - pmt;
        } else {
            total += get_fv(r, i as f64 - 1.0, pmt, pv, false);
        }
    }
    ok_num(total * r)
}

fn cumprinc_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let r = arg_num(ctx, &args[0])?;
    let nper = arg_num(ctx, &args[1])?;
    let pv = arg_num(ctx, &args[2])?;
    let start = arg_num(ctx, &args[3])?.trunc();
    let end = arg_num(ctx, &args[4])?.trunc();
    let t = pay_type(arg_num(ctx, &args[5])?)?;
    if r <= 0.0 || pv <= 0.0 || nper <= 0.0 || start < 1.0 || end < start || end > nper {
        return Err(CalcError::Num);
    }
    let advance = t == 1.0;
    let pmt = get_pmt(r, nper, pv, 0.0, advance);
    let mut total = 0.0;
    let mut n = start as i64;
    if n == 1 {
        total = if advance { pmt } else { pmt + pv * r };
        n += 1;
    }
    for i in n..=(end as i64) {
        if advance {
            total += pmt - (get_fv(r, i as f64 - 2.0, pmt, pv, true) - pmt) * r;
        } else {
            total += pmt - get_fv(r, i as f64 - 1.0, pmt, pv, false) * r;
        }
    }
    ok_num(total)
}

fn fvschedule_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let principal = arg_num_typed(ctx, &args[0])?;
    let mut rates = Vec::new();
    for arg in &args[1..] {
        match arg.value(ctx)? {
            CalcValue::Array(a) => {
                for v in a.iter() {
                    match v {
                        CalcValue::Blank => {}
                        CalcValue::Bool(_) => return Err(CalcError::Value),
                        other => rates.push(coerce_number(other)?),
                    }
                }
            }
            v => rates.push(coerce_number(&v)?),
        }
    }
    let mut result = principal;
    for &r in &rates {
        result *= 1.0 + r;
    }
    ok_num(result)
}

// -- return measures ----------------------------------------------------------

fn npv_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let r = arg_num(ctx, &args[0])?;
    if r <= -1.0 {
        return Err(CalcError::Num);
    }
    let values = collect_values(ctx, &args[1..])?;
    let mut total = 0.0;
    for (i, &v) in values.iter().enumerate() {
        total += v / (1.0 + r).powf((i + 1) as f64);
    }
    ok_num(total)
}

fn irr_residual(values: &[f64], r: f64) -> f64 {
    if r <= -1.0 {
        return f64::NAN;
    }
    let mut total = 0.0;
    let mut denom = 1.0;
    for &v in values {
        total += v / denom;
        denom *= 1.0 + r;
    }
    total
}

fn irr_deriv(values: &[f64], r: f64) -> f64 {
    if r <= -1.0 {
        return f64::NAN;
    }
    let mut total = 0.0;
    let mut denom = 1.0;
    for (i, &v) in values.iter().enumerate() {
        if i >= 1 {
            total -= (i as f64) * v / (denom * (1.0 + r));
        }
        denom *= 1.0 + r;
    }
    total
}

fn has_pos_and_neg(values: &[f64]) -> bool {
    let mut pos = false;
    let mut neg = false;
    for &v in values {
        if v > 0.0 {
            pos = true;
        }
        if v < 0.0 {
            neg = true;
        }
    }
    pos && neg
}

fn irr_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let values = collect_values(ctx, &args[..1])?;
    let guess = opt_num(ctx, args, 1, 0.1)?;
    if !has_pos_and_neg(&values) {
        return Err(CalcError::Num);
    }
    let f = |r| irr_residual(&values, r);
    let df = |r| irr_deriv(&values, r);
    let x = solve_robust(&f, &df, &[guess, 0.1, 0.5, -0.5, 2.0]).ok_or(CalcError::Num)?;
    ok_num(x)
}

fn mirr_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let values = collect_values(ctx, &args[..1])?;
    let fr = arg_num(ctx, &args[1])?;
    let rr = arg_num(ctx, &args[2])?;
    let n = values.len();
    // Excel reports #DIV/0! when there is nothing to discount at the finance
    // rate (no negative cash flows, too few flows) or reinvestment collapses.
    if n < 2 {
        return Err(CalcError::Div0);
    }
    if rr == -1.0 {
        return Err(CalcError::Div0);
    }
    let mut pv_pos = 0.0;
    let mut pv_neg = 0.0;
    for (i, &v) in values.iter().enumerate() {
        let t = (n - 1 - i) as f64;
        if v > 0.0 {
            pv_pos += v * (1.0 + rr).powf(t);
        } else if v < 0.0 {
            pv_neg += -v / (1.0 + fr).powf(i as f64);
        }
    }
    if pv_pos == 0.0 || pv_neg == 0.0 {
        return Err(CalcError::Div0);
    }
    ok_num((pv_pos / pv_neg).powf(1.0 / (n as f64 - 1.0)) - 1.0)
}

fn xnpv_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let r = arg_num(ctx, &args[0])?;
    if r <= -1.0 {
        return Err(CalcError::Num);
    }
    let values = collect_values(ctx, &args[1..2])?;
    let dates = collect_values(ctx, &args[2..3])?;
    if values.len() != dates.len() || values.is_empty() {
        return Err(CalcError::Num);
    }
    let d0 = dates[0].trunc();
    let mut total = 0.0;
    for (v, d) in values.iter().zip(dates.iter()) {
        let t = (d.trunc() - d0) / 365.0;
        total += v / (1.0 + r).powf(t);
    }
    ok_num(total)
}

fn xirr_residual(values: &[f64], dates: &[f64], d0: f64, r: f64) -> f64 {
    if r <= -1.0 {
        return f64::NAN;
    }
    let mut total = 0.0;
    for (v, d) in values.iter().zip(dates.iter()) {
        let t = (d.trunc() - d0) / 365.0;
        total += v / (1.0 + r).powf(t);
    }
    total
}

fn xirr_deriv(values: &[f64], dates: &[f64], d0: f64, r: f64) -> f64 {
    if r <= -1.0 {
        return f64::NAN;
    }
    let mut total = 0.0;
    for (v, d) in values.iter().zip(dates.iter()) {
        let t = (d.trunc() - d0) / 365.0;
        total -= t * v / (1.0 + r).powf(t + 1.0);
    }
    total
}

fn xirr_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let values = collect_values(ctx, &args[..1])?;
    let dates = collect_values(ctx, &args[1..2])?;
    let guess = opt_num(ctx, args, 2, 0.1)?;
    if values.len() != dates.len() || values.is_empty() || !has_pos_and_neg(&values) {
        return Err(CalcError::Num);
    }
    let d0 = dates[0].trunc();
    let f = |r| xirr_residual(&values, &dates, d0, r);
    let df = |r| xirr_deriv(&values, &dates, d0, r);
    let x = solve_robust(&f, &df, &[guess, 0.1, 0.5, 2.0, -0.5]).ok_or(CalcError::Num)?;
    ok_num(x)
}

fn rri_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let nper = arg_num(ctx, &args[0])?;
    let pv = arg_num(ctx, &args[1])?;
    let fv = arg_num(ctx, &args[2])?;
    if nper <= 0.0 || pv == 0.0 || fv / pv <= 0.0 {
        return Err(CalcError::Num);
    }
    ok_num((fv / pv).powf(1.0 / nper) - 1.0)
}

fn pduration_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let rate = arg_num(ctx, &args[0])?;
    let pv = arg_num(ctx, &args[1])?;
    let fv = arg_num(ctx, &args[2])?;
    if rate <= -1.0 || rate == 0.0 || pv <= 0.0 || fv <= 0.0 {
        return Err(CalcError::Num);
    }
    ok_num((fv / pv).ln() / (1.0 + rate).ln())
}

fn ispmt_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let r = arg_num(ctx, &args[0])?;
    let per = arg_num(ctx, &args[1])?;
    let nper = arg_num(ctx, &args[2])?;
    let pv = arg_num(ctx, &args[3])?;
    if nper == 0.0 {
        return Err(CalcError::Div0);
    }
    ok_num(pv * r * (per / nper - 1.0))
}

// -- depreciation -------------------------------------------------------------

fn sln_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let cost = arg_num(ctx, &args[0])?;
    let salvage = arg_num(ctx, &args[1])?;
    let life = arg_num(ctx, &args[2])?;
    if life == 0.0 {
        return Err(CalcError::Div0);
    }
    ok_num((cost - salvage) / life)
}

fn syd_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let cost = arg_num(ctx, &args[0])?;
    let salvage = arg_num(ctx, &args[1])?;
    let life = arg_num(ctx, &args[2])?;
    let per = arg_num(ctx, &args[3])?;
    if life <= 0.0 || salvage < 0.0 || per < 1.0 || per > life {
        return Err(CalcError::Num);
    }
    let base = cost - salvage;
    let denom = life * (life + 1.0) / 2.0;
    ok_num(base * (life - per + 1.0) / denom)
}

/// Fixed-declining balance; the rate is rounded to three decimals (a
/// documented Excel quirk) and the first/last periods are prorated by month.
fn db_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let cost = arg_num(ctx, &args[0])?;
    let salvage = arg_num(ctx, &args[1])?;
    let life = arg_num(ctx, &args[2])?;
    let period = arg_num(ctx, &args[3])?.trunc();
    let month = opt_num(ctx, args, 4, 12.0)?.trunc();
    if cost <= 0.0
        || salvage < 0.0
        || salvage > cost
        || life <= 0.0
        || period < 1.0
        || !(1.0..=12.0).contains(&month)
    {
        return Err(CalcError::Num);
    }
    let rate_raw = 1.0 - (salvage / cost).powf(1.0 / life);
    let rate = (rate_raw * 1000.0).round() / 1000.0;
    let mut acc = 0.0;
    let mut dep = 0.0;
    let p_max = (life as i64) + 1;
    if period as i64 > p_max {
        return ok_num(0.0);
    }
    for p in 1..=p_max {
        if p == 1 {
            dep = cost * rate * month / 12.0;
        } else if (p as f64) <= life {
            dep = (cost - acc) * rate;
        } else {
            dep = (cost - acc) * rate * (12.0 - month) / 12.0;
        }
        if dep < 0.0 {
            dep = 0.0;
        }
        acc += dep;
        if p == period as i64 {
            return ok_num(dep);
        }
    }
    ok_num(dep)
}

/// Closed-form double-declining-balance depreciation, matching Excel: the
/// asset declines at `factor/life` until the book value would fall below the
/// salvage value, at which point the period takes only the remaining amount.
fn get_ddb(cost: f64, salvage: f64, life: f64, period: f64, factor: f64) -> f64 {
    let rate = factor / life;
    let (rate, old) = if rate >= 1.0 {
        (1.0, if period == 1.0 { cost } else { 0.0 })
    } else {
        (rate, cost * (1.0 - rate).powf(period - 1.0))
    };
    let new = cost * (1.0 - rate).powf(period);
    let ddb = if new < salvage {
        old - salvage
    } else {
        old - new
    };
    if ddb < 0.0 { 0.0 } else { ddb }
}

fn ddb_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let cost = arg_num(ctx, &args[0])?;
    let salvage = arg_num(ctx, &args[1])?;
    let life = arg_num(ctx, &args[2])?;
    let period = arg_num(ctx, &args[3])?.trunc();
    let factor = opt_num(ctx, args, 4, 2.0)?;
    if cost < 0.0
        || salvage < 0.0
        || salvage > cost
        || life <= 0.0
        || factor <= 0.0
        || period < 1.0
        || period > life
    {
        return Err(CalcError::Num);
    }
    ok_num(get_ddb(cost, salvage, life, period, factor))
}

/// Per-span VDB core (Excel's algorithm, as ported by Univer): depreciation
/// over `[start_period, end_period]` with the SLN switch decided period by
/// period. `start_period` doubles as the remaining-life counter in the SLN
/// comparison — that coupling is what reproduces Excel's fractional-period
/// results (e.g. VDB(24000,3000,10,6.1,6.2,2) = 123.3125376, not 125.83).
fn vdb_core(
    cost: f64,
    salvage: f64,
    life: f64,
    start_period: f64,
    end_period: f64,
    factor: f64,
) -> f64 {
    let end = end_period.ceil();
    let loop_end = end as i64;
    let mut result = 0.0;
    let mut rest = cost - salvage;
    let mut sln = 0.0;
    let mut switched = false;
    for i in 1..=loop_end {
        let temp;
        if !switched {
            let ddb = get_ddb(cost, salvage, life, i as f64, factor);
            sln = rest / (start_period - (i as f64 - 1.0));
            if sln > ddb {
                temp = sln;
                switched = true;
            } else {
                temp = ddb;
                rest -= ddb;
            }
        } else {
            temp = sln;
        }
        let temp = if (i as f64) == end {
            temp * (end_period + 1.0 - end)
        } else {
            temp
        };
        result += temp;
    }
    result
}

fn vdb_value(
    cost: f64,
    salvage: f64,
    life: f64,
    start_period: f64,
    end_period: f64,
    factor: f64,
    no_switch: bool,
) -> f64 {
    let start = start_period.floor() as i64;
    let end = end_period.ceil() as i64;
    let mut result = 0.0;
    if cost < salvage {
        if start_period >= 1.0 || no_switch {
            return result;
        }
        let temp_minus = (cost - salvage).abs();
        let r = temp_minus * (end_period - start_period);
        return -if r > temp_minus { temp_minus } else { r };
    }
    if no_switch {
        for i in (start + 1)..=end {
            let mut ddb = get_ddb(cost, salvage, life, i as f64, factor);
            if i == start + 1 {
                ddb *= end_period.min(start as f64 + 1.0) - start_period;
            } else if i == end {
                ddb *= end_period + 1.0 - end as f64;
            }
            result += ddb;
        }
    } else {
        let cost2 = cost - vdb_core(cost, salvage, life, life, start_period, factor);
        result = vdb_core(
            cost2,
            salvage,
            life,
            life - start_period,
            end_period - start_period,
            factor,
        );
    }
    result
}

fn vdb_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let cost = arg_num(ctx, &args[0])?;
    let salvage = arg_num(ctx, &args[1])?;
    let life = arg_num(ctx, &args[2])?;
    let start = arg_num(ctx, &args[3])?;
    let end = arg_num(ctx, &args[4])?;
    let factor = opt_num(ctx, args, 5, 2.0)?;
    let no_switch = opt_num(ctx, args, 6, 0.0)?;
    if cost < 0.0
        || salvage < 0.0
        || life < 0.0
        || factor < 0.0
        || start < 0.0
        || end < start
        || end > life
    {
        return Err(CalcError::Num);
    }
    if life == 0.0 && start == 0.0 && end == 0.0 {
        return Err(CalcError::Div0);
    }
    ok_num(vdb_value(
        cost,
        salvage,
        life,
        start,
        end,
        factor,
        no_switch != 0.0,
    ))
}

// -- rate conversion ----------------------------------------------------------

fn effect_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let r = arg_num(ctx, &args[0])?;
    let n = arg_num_typed(ctx, &args[1])?.trunc();
    if r < 0.0 || n < 1.0 {
        return Err(CalcError::Num);
    }
    if r == 0.0 {
        return ok_num(0.0);
    }
    ok_num((1.0 + r / n).powf(n) - 1.0)
}

fn nominal_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let r = arg_num(ctx, &args[0])?;
    let n = arg_num_typed(ctx, &args[1])?.trunc();
    if r <= 0.0 || n < 1.0 {
        return Err(CalcError::Num);
    }
    ok_num(n * ((1.0 + r).powf(1.0 / n) - 1.0))
}

fn dollarde_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let d = arg_num(ctx, &args[0])?;
    let f = arg_num_typed(ctx, &args[1])?.trunc();
    if f < 0.0 {
        return Err(CalcError::Num);
    }
    if f < 1.0 {
        return Err(CalcError::Div0);
    }
    let int = d.floor();
    ok_num(int + (d - int) * 100.0 / f)
}

fn dollarfr_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let d = arg_num(ctx, &args[0])?;
    let f = arg_num_typed(ctx, &args[1])?.trunc();
    if f < 0.0 {
        return Err(CalcError::Num);
    }
    if f < 1.0 {
        return Err(CalcError::Div0);
    }
    let int = d.floor();
    ok_num(int + (d - int) * f / 100.0)
}

// -- securities ---------------------------------------------------------------

/// The coupon period containing `settlement` plus the coupon count between the
/// next coupon and maturity: returns (next_coupon, prev_coupon, n_coupons).
fn coupon_period(
    settlement: f64,
    maturity: f64,
    freq: i64,
    date1904: bool,
) -> Result<(i64, i64, i64), CalcError> {
    let months = 12 / freq;
    let s = settlement.trunc() as i64;
    let m = maturity.trunc() as i64;
    if s >= m {
        return Err(CalcError::Num);
    }
    let (mut y, mut mm, mut d) = serial_to_civil(maturity, date1904);
    let mut prev = civil_to_serial(y, mm, d, date1904);
    let mut n = 1i64;
    loop {
        let (ny, nm, nd) = shift_months(y, mm, d, months, date1904);
        let serial = civil_to_serial(ny, nm, nd, date1904);
        if serial <= s {
            return Ok((prev, serial, n));
        }
        prev = serial;
        y = ny;
        mm = nm;
        d = nd;
        n += 1;
    }
}

fn shift_months(y: i64, m: i64, d: i64, months: i64, date1904: bool) -> (i64, i64, i64) {
    let total = y * 12 + (m - 1) - months;
    let ny = total.div_euclid(12);
    let nm = total.rem_euclid(12) + 1;
    let nd = d.min(days_in_month(ny, nm, date1904));
    (ny, nm, nd)
}

fn shift_months_fwd(y: i64, m: i64, d: i64, months: i64, date1904: bool) -> (i64, i64, i64) {
    let total = y * 12 + (m - 1) + months;
    let ny = total.div_euclid(12);
    let nm = total.rem_euclid(12) + 1;
    let nd = d.min(days_in_month(ny, nm, date1904));
    (ny, nm, nd)
}

/// The last coupon date on or before `settlement`, stepping back `12/freq`
/// months from `maturity` (Excel's COUPPCD, used by the odd-coupon family).
fn prev_coupon(settlement: i64, maturity: i64, freq: i64, date1904: bool) -> i64 {
    let months = 12 / freq;
    let (sy, sm, sd) = serial_to_civil(settlement as f64, date1904);
    let s_days = days_from_civil(sy, sm, sd);
    let (_, mut m, mut d) = serial_to_civil(maturity as f64, date1904);
    let mut y = sy;
    if days_from_civil(y, m, d) < s_days {
        y += 1;
    }
    while days_from_civil(y, m, d) > s_days {
        let (ny, nm, nd) = shift_months(y, m, d, months, date1904);
        y = ny;
        m = nm;
        d = nd;
    }
    civil_to_serial(y, m, d, date1904)
}

/// The coupon date after `settlement` (Excel's COUPNCD).
fn next_coupon(settlement: i64, maturity: i64, freq: i64, date1904: bool) -> i64 {
    let months = 12 / freq;
    let (sy, sm, sd) = serial_to_civil(settlement as f64, date1904);
    let s_days = days_from_civil(sy, sm, sd);
    let (_, mut m, mut d) = serial_to_civil(maturity as f64, date1904);
    let mut y = sy;
    if days_from_civil(y, m, d) < s_days {
        y += 1;
    }
    while days_from_civil(y, m, d) > s_days {
        let (ny, nm, nd) = shift_months(y, m, d, months, date1904);
        y = ny;
        m = nm;
        d = nd;
    }
    let (ny, nm, nd) = shift_months_fwd(y, m, d, months, date1904);
    civil_to_serial(ny, nm, nd, date1904)
}

/// Number of coupon dates strictly after `settlement` and up to `maturity`
/// (Excel's COUPNUM).
fn coupon_count(settlement: i64, maturity: i64, freq: i64, date1904: bool) -> i64 {
    let months = 12 / freq;
    let (sy, sm, sd) = serial_to_civil(settlement as f64, date1904);
    let s_days = days_from_civil(sy, sm, sd);
    let (mut y, mut m, mut d) = serial_to_civil(maturity as f64, date1904);
    let mut n = 0i64;
    while days_from_civil(y, m, d) > s_days {
        let (ny, nm, nd) = shift_months(y, m, d, months, date1904);
        y = ny;
        m = nm;
        d = nd;
        n += 1;
    }
    n
}

/// Days in the coupon period containing `settlement` (Excel's COUPDAYS):
/// actual days between consecutive coupon dates for basis 1, else 365/360
/// scaled by frequency.
fn coup_days(settlement: i64, maturity: i64, freq: i64, basis: u8, date1904: bool) -> f64 {
    match basis {
        1 => {
            let before = prev_coupon(settlement, maturity, freq, date1904);
            let after = add_coupon_months(before, 12 / freq, date1904);
            if before < 0 && freq == 1 {
                365.0
            } else {
                (after - before) as f64
            }
        }
        3 => 365.0 / freq as f64,
        _ => 360.0 / freq as f64,
    }
}

/// `settlement` plus `months` coupon-steps with Excel's EDATE semantics: a
/// month-end source date lands on the target month's last day (2/29 + 6 months
/// is 8/31, not 8/29), which the odd-coupon and ACCRINT lattices rely on.
fn add_coupon_months(serial: i64, months: i64, date1904: bool) -> i64 {
    let (y, m, d) = serial_to_civil(serial as f64, date1904);
    let is_last = d == days_in_month(y, m, date1904);
    let (ny, nm, nd) = shift_months_fwd(y, m, d, months, date1904);
    let (ny, nm, nd) = if is_last {
        (ny, nm, days_in_month(ny, nm, date1904))
    } else {
        (ny, nm, nd)
    };
    civil_to_serial(ny, nm, nd, date1904)
}

fn coupdays_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let settlement = arg_num(ctx, &args[0])?;
    let maturity = arg_num(ctx, &args[1])?;
    let freq = check_freq(arg_num(ctx, &args[2])?)?;
    let basis = check_basis(opt_num(ctx, args, 3, 0.0)?)?;
    let (next, prev, _) = coupon_period(settlement, maturity, freq, ctx.date1904)?;
    ok_num(day_count(prev as f64, next as f64, basis, ctx.date1904))
}

fn coupdaybs_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let settlement = arg_num(ctx, &args[0])?;
    let maturity = arg_num(ctx, &args[1])?;
    let freq = check_freq(arg_num(ctx, &args[2])?)?;
    let basis = check_basis(opt_num(ctx, args, 3, 0.0)?)?;
    let (_, prev, _) = coupon_period(settlement, maturity, freq, ctx.date1904)?;
    ok_num(day_count(prev as f64, settlement, basis, ctx.date1904))
}

fn coupdaysnc_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let settlement = arg_num(ctx, &args[0])?;
    let maturity = arg_num(ctx, &args[1])?;
    let freq = check_freq(arg_num(ctx, &args[2])?)?;
    let basis = check_basis(opt_num(ctx, args, 3, 0.0)?)?;
    let (next, _, _) = coupon_period(settlement, maturity, freq, ctx.date1904)?;
    ok_num(day_count(settlement, next as f64, basis, ctx.date1904))
}

fn coupnum_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let settlement = arg_num(ctx, &args[0])?;
    let maturity = arg_num(ctx, &args[1])?;
    let freq = check_freq(arg_num(ctx, &args[2])?)?;
    check_basis(opt_num(ctx, args, 3, 0.0)?)?;
    let (_, _, n) = coupon_period(settlement, maturity, freq, ctx.date1904)?;
    ok_num(n as f64)
}

fn coupncd_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let settlement = arg_num(ctx, &args[0])?;
    let maturity = arg_num(ctx, &args[1])?;
    let freq = check_freq(arg_num(ctx, &args[2])?)?;
    check_basis(opt_num(ctx, args, 3, 0.0)?)?;
    let (next, _, _) = coupon_period(settlement, maturity, freq, ctx.date1904)?;
    ok_num(next as f64)
}

fn couppcd_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let settlement = arg_num(ctx, &args[0])?;
    let maturity = arg_num(ctx, &args[1])?;
    let freq = check_freq(arg_num(ctx, &args[2])?)?;
    check_basis(opt_num(ctx, args, 3, 0.0)?)?;
    let (_, prev, _) = coupon_period(settlement, maturity, freq, ctx.date1904)?;
    // Excel clamps coupon dates that step before the 1900 origin to serial 0.
    ok_num(if prev < 0 { 0.0 } else { prev as f64 })
}

#[allow(clippy::too_many_arguments)]
fn accrint_value(
    issue: f64,
    first: f64,
    settlement: f64,
    coupon: f64,
    par: f64,
    freq: i64,
    basis: u8,
    calc_method: f64,
    date1904: bool,
) -> Result<f64, CalcError> {
    let issue_s = issue.trunc() as i64;
    let first_s = first.trunc() as i64;
    let settlement_s = settlement.trunc() as i64;
    let months = 12 / freq;

    // Excel: the accrued fraction is built coupon period by coupon period, and
    // every segment is `days in the partial period / days in the whole period`
    // under the basis — never days/yearDays (that is what made basis 1 come out
    // ~0.8% too high). A coupon date at/before the 1900 origin yields 0.
    let pcd = prev_coupon(settlement_s, first_s, freq, date1904);
    if pcd <= 0 {
        return Ok(0.0);
    }

    let mut coup_date = add_coupon_months(first_s, -months, date1904);
    if settlement_s > first_s && calc_method != 0.0 {
        while coup_date < settlement_s {
            coup_date = add_coupon_months(coup_date, months, date1904);
        }
    }

    let mut first_date = issue_s.max(coup_date);
    let mut days = day_count(first_date as f64, settlement_s as f64, basis, date1904);
    if pcd >= issue_s {
        // Fresh issue inside the first coupon period: Excel switches the
        // accrued-day count to the 30/360 convention even under basis 1.
        let dfs_basis = if basis == 0 { 0 } else { 4 };
        days = day_count(first_date as f64, settlement_s as f64, dfs_basis, date1904);
    }
    if settlement_s < first_date {
        days = -days;
    }

    let mut coupdays = coup_days(coup_date, first_s, freq, basis, date1904);
    let mut accrued = days / coupdays;
    let mut start = coup_date;
    let mut guard = 0;
    while start > issue_s {
        let end = start;
        start = add_coupon_months(start, -months, date1904);
        first_date = issue_s.max(start);
        let dfe = day_count(first_date as f64, end as f64, basis, date1904);
        if basis == 0 {
            days = if end >= first_date || issue_s <= start {
                dfe
            } else {
                -dfe
            };
            coupdays = coup_days(start, end, freq, basis, date1904);
        } else {
            days = if end < first_date { -dfe } else { dfe };
            if basis == 3 {
                coupdays = 365.0 / freq as f64;
            } else {
                let dse = day_count(start as f64, end as f64, basis, date1904);
                coupdays = if end < start { -dse } else { dse };
            }
        }
        accrued += if issue_s <= start {
            if calc_method != 0.0 { 1.0 } else { 0.0 }
        } else {
            days / coupdays
        };
        guard += 1;
        if guard > 10000 {
            break;
        }
    }
    Ok(par * coupon / freq as f64 * accrued)
}

fn accrint_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let issue = arg_num(ctx, &args[0])?;
    let first = arg_num(ctx, &args[1])?;
    let settlement = arg_num(ctx, &args[2])?;
    let coupon = arg_num(ctx, &args[3])?;
    let par = arg_num(ctx, &args[4])?;
    let freq = arg_num(ctx, &args[5])?.trunc();
    let basis = check_basis(opt_num(ctx, args, 6, 0.0)?)?;
    let calc_method = opt_num(ctx, args, 7, 1.0)?;
    if coupon <= 0.0 || par <= 0.0 || (freq != 1.0 && freq != 2.0 && freq != 4.0) {
        return Err(CalcError::Num);
    }
    let i = issue.trunc();
    let f = first.trunc();
    let s = settlement.trunc();
    if i >= f || i >= s {
        return Err(CalcError::Num);
    }
    ok_num(accrint_value(
        issue,
        first,
        settlement,
        coupon,
        par,
        freq as i64,
        basis,
        calc_method,
        ctx.date1904,
    )?)
}

fn accrintm_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let issue = arg_num(ctx, &args[0])?;
    let settlement = arg_num(ctx, &args[1])?;
    let rate = arg_num(ctx, &args[2])?;
    let par = arg_num(ctx, &args[3])?;
    let basis = check_basis(opt_num(ctx, args, 4, 0.0)?)?;
    if rate <= 0.0 || par <= 0.0 || issue.trunc() >= settlement.trunc() {
        return Err(CalcError::Num);
    }
    ok_num(par * rate * year_fraction(issue, settlement, basis, ctx.date1904))
}

fn disc_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let settlement = arg_num(ctx, &args[0])?;
    let maturity = arg_num(ctx, &args[1])?;
    let price = arg_num(ctx, &args[2])?;
    let redemption = arg_num(ctx, &args[3])?;
    let basis = check_basis(opt_num(ctx, args, 4, 0.0)?)?;
    if settlement.trunc() >= maturity.trunc() || price <= 0.0 || redemption <= 0.0 {
        return Err(CalcError::Num);
    }
    let dsm = day_count(settlement, maturity, basis, ctx.date1904);
    if dsm == 0.0 {
        return Err(CalcError::Div0);
    }
    let b = days_in_year(settlement, maturity, basis, ctx.date1904);
    ok_num((redemption - price) / redemption * (b / dsm))
}

fn intrate_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let settlement = arg_num(ctx, &args[0])?;
    let maturity = arg_num(ctx, &args[1])?;
    let investment = arg_num(ctx, &args[2])?;
    let redemption = arg_num(ctx, &args[3])?;
    let basis = check_basis(opt_num(ctx, args, 4, 0.0)?)?;
    if settlement.trunc() >= maturity.trunc() || investment <= 0.0 || redemption <= 0.0 {
        return Err(CalcError::Num);
    }
    let dsm = day_count(settlement, maturity, basis, ctx.date1904);
    if dsm == 0.0 {
        return Err(CalcError::Div0);
    }
    let b = days_in_year(settlement, maturity, basis, ctx.date1904);
    ok_num((redemption - investment) / investment * (b / dsm))
}

fn received_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let settlement = arg_num(ctx, &args[0])?;
    let maturity = arg_num(ctx, &args[1])?;
    let investment = arg_num(ctx, &args[2])?;
    let discount = arg_num(ctx, &args[3])?;
    let basis = check_basis(opt_num(ctx, args, 4, 0.0)?)?;
    if settlement.trunc() >= maturity.trunc() || investment <= 0.0 || discount <= 0.0 {
        return Err(CalcError::Num);
    }
    let dsm = day_count(settlement, maturity, basis, ctx.date1904);
    let b = days_in_year(settlement, maturity, basis, ctx.date1904);
    let denom = 1.0 - discount * dsm / b;
    if denom == 0.0 {
        return Err(CalcError::Div0);
    }
    ok_num(investment / denom)
}

fn pricedisc_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let settlement = arg_num(ctx, &args[0])?;
    let maturity = arg_num(ctx, &args[1])?;
    let discount = arg_num(ctx, &args[2])?;
    let redemption = arg_num(ctx, &args[3])?;
    let basis = check_basis(opt_num(ctx, args, 4, 0.0)?)?;
    if settlement.trunc() >= maturity.trunc() || discount <= 0.0 || redemption <= 0.0 {
        return Err(CalcError::Num);
    }
    let dsm = day_count(settlement, maturity, basis, ctx.date1904);
    let b = days_in_year(settlement, maturity, basis, ctx.date1904);
    ok_num(redemption - redemption * discount * dsm / b)
}

fn yielddisc_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let settlement = arg_num(ctx, &args[0])?;
    let maturity = arg_num(ctx, &args[1])?;
    let price = arg_num(ctx, &args[2])?;
    let redemption = arg_num(ctx, &args[3])?;
    let basis = check_basis(opt_num(ctx, args, 4, 0.0)?)?;
    if settlement.trunc() >= maturity.trunc() || price <= 0.0 || redemption <= 0.0 {
        return Err(CalcError::Num);
    }
    let dsm = day_count(settlement, maturity, basis, ctx.date1904);
    if dsm == 0.0 {
        return Err(CalcError::Div0);
    }
    let b = days_in_year(settlement, maturity, basis, ctx.date1904);
    ok_num((redemption - price) / price * (b / dsm))
}

fn tbillprice_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let settlement = arg_num(ctx, &args[0])?;
    let maturity = arg_num(ctx, &args[1])?;
    let discount = arg_num(ctx, &args[2])?;
    if settlement.trunc() >= maturity.trunc() || discount <= 0.0 {
        return Err(CalcError::Num);
    }
    let dsm = day_count(settlement, maturity, 2, ctx.date1904);
    if dsm > 360.0 {
        return Err(CalcError::Num);
    }
    ok_num(100.0 * (1.0 - discount * dsm / 360.0))
}

fn tbillyield_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let settlement = arg_num(ctx, &args[0])?;
    let maturity = arg_num(ctx, &args[1])?;
    let price = arg_num(ctx, &args[2])?;
    if settlement.trunc() >= maturity.trunc() || price <= 0.0 {
        return Err(CalcError::Num);
    }
    let dsm = day_count(settlement, maturity, 2, ctx.date1904);
    if dsm > 360.0 {
        return Err(CalcError::Num);
    }
    ok_num((100.0 - price) / price * (360.0 / dsm))
}

fn t_bill_eq_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let settlement = arg_num(ctx, &args[0])?;
    let maturity = arg_num(ctx, &args[1])?;
    let discount = arg_num(ctx, &args[2])?;
    if settlement.trunc() >= maturity.trunc() || discount <= 0.0 {
        return Err(CalcError::Num);
    }
    let dsm = maturity.trunc() - settlement.trunc();
    let (y, _, _) = serial_to_civil(settlement, ctx.date1904);
    let year_days = if is_leap_system(y, ctx.date1904) {
        366.0
    } else {
        365.0
    };
    if dsm > year_days {
        return Err(CalcError::Num);
    }
    let denom = 360.0 - discount * dsm;
    if denom == 0.0 {
        return Err(CalcError::Div0);
    }
    let mut result = (365.0 * discount) / denom;
    // For maturities past 182 days Excel switches to the equivalent-yield
    // form; without it TBILLEQ(DATE(2008,3,31),DATE(2008,11,1),0.0914) is
    // ~0.0007 too high.
    if dsm > 182.0 {
        let price = 100.0 * (1.0 - discount * dsm / 360.0);
        let fraction = dsm / 365.0;
        let disc = fraction * fraction - (fraction * 2.0 - 1.0) * (1.0 - 100.0 / price);
        result = (-fraction + disc.sqrt()) / (fraction - 0.5);
        if !result.is_finite() {
            return Err(CalcError::Num);
        }
    }
    if result < 0.0 {
        return Err(CalcError::Num);
    }
    ok_num(result)
}

#[allow(clippy::too_many_arguments)]
fn price_impl(
    settlement: f64,
    maturity: f64,
    rate: f64,
    ann_yield: f64,
    redemption: f64,
    freq: i64,
    basis: u8,
    date1904: bool,
) -> Result<f64, CalcError> {
    let (next, prev, n) = coupon_period(settlement, maturity, freq, date1904)?;
    let a = day_count(prev as f64, settlement, basis, date1904);
    let e = day_count(prev as f64, next as f64, basis, date1904);
    let dsc = day_count(settlement, next as f64, basis, date1904);
    if e == 0.0 {
        return Err(CalcError::Num);
    }
    let r = ann_yield / freq as f64;
    let c = 100.0 * rate / freq as f64;
    let base = dsc / e;
    if n == 1 {
        // Single coupon left: Excel discounts with simple interest.
        let denom = 1.0 + r * base;
        if denom == 0.0 {
            return Err(CalcError::Num);
        }
        let price = (redemption + c) / denom - c * a / e;
        return if price.is_finite() {
            Ok(price)
        } else {
            Err(CalcError::Num)
        };
    }
    let mut price = -c * a / e;
    let mut t = base;
    for _ in 0..n {
        price += c / (1.0 + r).powf(t);
        t += 1.0;
    }
    price += redemption / (1.0 + r).powf(t - 1.0);
    if price.is_finite() {
        Ok(price)
    } else {
        Err(CalcError::Num)
    }
}

fn price_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let settlement = arg_num(ctx, &args[0])?;
    let maturity = arg_num(ctx, &args[1])?;
    let rate = arg_num(ctx, &args[2])?;
    let ann_yield = arg_num(ctx, &args[3])?;
    let redemption = arg_num(ctx, &args[4])?;
    let freq = check_freq(arg_num(ctx, &args[5])?)?;
    let basis = check_basis(opt_num(ctx, args, 6, 0.0)?)?;
    if rate < 0.0 || ann_yield < 0.0 || redemption <= 0.0 {
        return Err(CalcError::Num);
    }
    ok_num(price_impl(
        settlement,
        maturity,
        rate,
        ann_yield,
        redemption,
        freq,
        basis,
        ctx.date1904,
    )?)
}

fn yield_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let settlement = arg_num(ctx, &args[0])?;
    let maturity = arg_num(ctx, &args[1])?;
    let rate = arg_num(ctx, &args[2])?;
    let price = arg_num(ctx, &args[3])?;
    let redemption = arg_num(ctx, &args[4])?;
    let freq = check_freq(arg_num(ctx, &args[5])?)?;
    let basis = check_basis(opt_num(ctx, args, 6, 0.0)?)?;
    if rate < 0.0 || price <= 0.0 || redemption <= 0.0 {
        return Err(CalcError::Num);
    }
    let f = |x: f64| {
        price_impl(
            settlement,
            maturity,
            rate,
            x,
            redemption,
            freq,
            basis,
            ctx.date1904,
        )
        .unwrap_or(f64::NAN)
            - price
    };
    let df = |x: f64| {
        let h = 1e-7 * x.abs().max(1.0);
        (f(x + h) - f(x)) / h
    };
    let x = solve_robust(&f, &df, &[0.1, rate, 0.05, 0.01, 0.5]).ok_or(CalcError::Num)?;
    ok_num(x)
}

fn duration_value(
    settlement: f64,
    maturity: f64,
    coupon: f64,
    ann_yield: f64,
    freq: i64,
    basis: u8,
    date1904: bool,
) -> Result<f64, CalcError> {
    let s = settlement.trunc() as i64;
    let m = maturity.trunc() as i64;
    if s >= m {
        return Err(CalcError::Num);
    }
    // Excel: Macaulay duration weights each coupon and the redemption by its
    // PV. The index of the first coupon is (coupdays - coupdaybs)/coupdays —
    // the fraction of the coupon period remaining after settlement. Crucially
    // the denominator is the plain sum of PVs (the dirty price): subtracting
    // accrued interest here, as PRICE does, shifts the result by ~0.5 years.
    let pcd = prev_coupon(s, m, freq, date1904);
    let coupdaybs = day_count(pcd as f64, settlement, basis, date1904);
    let coupdays = coup_days(s, m, freq, basis, date1904);
    if coupdays == 0.0 {
        return Err(CalcError::Num);
    }
    let n = coupon_count(s, m, freq, date1904);
    if n == 0 {
        return Err(CalcError::Num);
    }
    let base_shift = (coupdays - coupdaybs) / coupdays - 1.0;
    let r = 1.0 + ann_yield / freq as f64;
    let c = 100.0 * coupon / freq as f64;
    let mut duration = 0.0;
    let mut den = 0.0;
    for i in 1..=n {
        let index = i as f64 + base_shift;
        let pv = c / r.powf(index);
        duration += index * pv;
        den += pv;
    }
    let index = n as f64 + base_shift;
    let pv = 100.0 / r.powf(index);
    duration += index * pv;
    den += pv;
    if den == 0.0 || !den.is_finite() || !duration.is_finite() {
        return Err(CalcError::Num);
    }
    Ok(duration / den / freq as f64)
}

fn duration_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let settlement = arg_num(ctx, &args[0])?;
    let maturity = arg_num(ctx, &args[1])?;
    let coupon = arg_num(ctx, &args[2])?;
    let ann_yield = arg_num(ctx, &args[3])?;
    let freq = check_freq(arg_num(ctx, &args[4])?)?;
    let basis = check_basis(opt_num(ctx, args, 5, 0.0)?)?;
    if coupon < 0.0 || ann_yield < 0.0 {
        return Err(CalcError::Num);
    }
    ok_num(duration_value(
        settlement,
        maturity,
        coupon,
        ann_yield,
        freq,
        basis,
        ctx.date1904,
    )?)
}

fn mduration_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let settlement = arg_num(ctx, &args[0])?;
    let maturity = arg_num(ctx, &args[1])?;
    let coupon = arg_num(ctx, &args[2])?;
    let ann_yield = arg_num(ctx, &args[3])?;
    let freq = check_freq(arg_num(ctx, &args[4])?)?;
    let basis = check_basis(opt_num(ctx, args, 5, 0.0)?)?;
    if coupon < 0.0 || ann_yield < 0.0 {
        return Err(CalcError::Num);
    }
    let d = duration_value(
        settlement,
        maturity,
        coupon,
        ann_yield,
        freq,
        basis,
        ctx.date1904,
    )?;
    ok_num(d / (1.0 + ann_yield / freq as f64))
}

// -- French linear amortization and interest-at-maturity securities -------------

fn amorlinc_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let cost = arg_num(ctx, &args[0])?;
    let date_purchased = arg_num(ctx, &args[1])?;
    let first_period = arg_num(ctx, &args[2])?;
    let salvage = arg_num(ctx, &args[3])?;
    let mut period = arg_num(ctx, &args[4])?;
    let rate = arg_num(ctx, &args[5])?;
    let basis = check_basis(opt_num(ctx, args, 6, 0.0)?)?;
    if cost <= 0.0
        || salvage < 0.0
        || cost < salvage
        || date_purchased.trunc() > first_period.trunc()
        || period < 0.0
        || rate <= 0.0
        || basis == 2
    {
        return Err(CalcError::Num);
    }
    // Excel rounds periods above 1 down and period 0 stays 0.
    period = if period > 1.0 {
        period.floor()
    } else {
        period.ceil()
    };
    let total_dep = cost - salvage;
    let base_dep = cost * rate;
    let frac = day_count(date_purchased, first_period, basis, ctx.date1904)
        / days_in_year(date_purchased, first_period, basis, ctx.date1904);
    let life = (total_dep / base_dep - frac).ceil();
    if life < 0.0 {
        return ok_num(0.0);
    }
    let result = if period == 0.0 {
        base_dep * frac
    } else if period == life {
        total_dep - base_dep * (frac + period - 1.0)
    } else if period > life {
        0.0
    } else {
        base_dep
    };
    ok_num(result)
}

fn pricemat_value(
    settlement: f64,
    maturity: f64,
    issue: f64,
    rate: f64,
    yld: f64,
    basis: u8,
    date1904: bool,
) -> Result<f64, CalcError> {
    let b = days_in_year_of(settlement, basis, date1904);
    let dsm = day_count(settlement, maturity, basis, date1904);
    let dim = day_count(issue, maturity, basis, date1904);
    let a = day_count(issue, settlement, basis, date1904);
    if b == 0.0 {
        return Err(CalcError::Num);
    }
    let denom = 1.0 + dsm / b * yld;
    if denom == 0.0 {
        return Err(CalcError::Num);
    }
    let price = (100.0 + dim / b * rate * 100.0) / denom - a / b * rate * 100.0;
    if price.is_finite() {
        Ok(price)
    } else {
        Err(CalcError::Num)
    }
}

fn pricemat_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let settlement = arg_num(ctx, &args[0])?;
    let maturity = arg_num(ctx, &args[1])?;
    let issue = arg_num(ctx, &args[2])?;
    let rate = arg_num(ctx, &args[3])?;
    let yld = arg_num(ctx, &args[4])?;
    let basis = check_basis(opt_num(ctx, args, 5, 0.0)?)?;
    if rate < 0.0 || yld < 0.0 {
        return Err(CalcError::Num);
    }
    if settlement.trunc() >= maturity.trunc() || maturity.trunc() <= issue.trunc() {
        return Err(CalcError::Num);
    }
    ok_num(pricemat_value(
        settlement,
        maturity,
        issue,
        rate,
        yld,
        basis,
        ctx.date1904,
    )?)
}

fn yieldmat_value(
    settlement: f64,
    maturity: f64,
    issue: f64,
    rate: f64,
    price: f64,
    basis: u8,
    date1904: bool,
) -> Result<f64, CalcError> {
    let b = days_in_year_of(settlement, basis, date1904);
    let dsm = day_count(settlement, maturity, basis, date1904);
    let dim = day_count(issue, maturity, basis, date1904);
    let a = day_count(issue, settlement, basis, date1904);
    if b == 0.0 || dsm == 0.0 {
        return Err(CalcError::Num);
    }
    let denom = price / 100.0 + a / b * rate;
    if denom == 0.0 {
        return Err(CalcError::Num);
    }
    let result = ((1.0 + dim / b * rate) / denom - 1.0) / (dsm / b);
    if result.is_finite() {
        Ok(result)
    } else {
        Err(CalcError::Num)
    }
}

fn yieldmat_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let settlement = arg_num(ctx, &args[0])?;
    let maturity = arg_num(ctx, &args[1])?;
    let issue = arg_num(ctx, &args[2])?;
    let rate = arg_num(ctx, &args[3])?;
    let price = arg_num(ctx, &args[4])?;
    let basis = check_basis(opt_num(ctx, args, 5, 0.0)?)?;
    if rate < 0.0 || price <= 0.0 {
        return Err(CalcError::Num);
    }
    if settlement.trunc() >= maturity.trunc() || maturity.trunc() <= issue.trunc() {
        return Err(CalcError::Num);
    }
    ok_num(yieldmat_value(
        settlement,
        maturity,
        issue,
        rate,
        price,
        basis,
        ctx.date1904,
    )?)
}

// -- odd first / last coupon securities -----------------------------------------

/// Days between two serials under `basis`, forced to 0 when the start is after
/// the end (Excel's getPositiveDaysBetween).
fn positive_days(s1: f64, s2: f64, basis: u8, date1904: bool) -> f64 {
    if s1 >= s2 {
        0.0
    } else {
        day_count(s1, s2, basis, date1904)
    }
}

#[allow(clippy::too_many_arguments)]
fn oddfprice_value(
    settlement: f64,
    maturity: f64,
    issue: f64,
    first: f64,
    rate: f64,
    yld: f64,
    redemption: f64,
    freq: i64,
    basis: u8,
    date1904: bool,
) -> Result<f64, CalcError> {
    let s = settlement.trunc() as i64;
    let m = maturity.trunc() as i64;
    let i = issue.trunc() as i64;
    let f = first.trunc() as i64;
    if m <= f || f <= s || s <= i {
        return Err(CalcError::Num);
    }
    // The odd first coupon period must align with the coupon lattice.
    if !coupon_lattice_ok(m, f, freq, date1904) || prev_coupon(i, m, freq, date1904) < 0 {
        return Err(CalcError::Num);
    }
    let dfc = positive_days(i as f64, f as f64, basis, date1904);
    let e = coup_days(s, f, freq, basis, date1904);
    if e == 0.0 {
        return Err(CalcError::Num);
    }
    let price = if dfc < e {
        oddf_short(
            s, m, i, f, rate, yld, redemption, freq, basis, dfc, e, date1904,
        )
    } else {
        oddf_long(s, m, i, f, rate, yld, redemption, freq, basis, e, date1904)
    };
    if price.is_finite() {
        Ok(price)
    } else {
        Err(CalcError::Num)
    }
}

/// The coupon lattice check: `date2` must fall on a coupon date counted back
/// from `date1` (Excel's validDaysBetweenIsWholeFrequencyByTwoDate).
fn coupon_lattice_ok(date1: i64, date2: i64, freq: i64, date1904: bool) -> bool {
    let (y1, m1, d1) = serial_to_civil(date1 as f64, date1904);
    let (y2, m2, d2) = serial_to_civil(date2 as f64, date1904);
    let same_day = d1 == d2
        || (d1 == days_in_month(y1, m1, date1904) && d2 == days_in_month(y2, m2, date1904));
    if !same_day {
        return false;
    }
    let months = (y2 - y1) * 12 + (m2 - m1);
    months % (12 / freq) == 0
}

#[allow(clippy::too_many_arguments)]
fn oddf_short(
    s: i64,
    m: i64,
    i: i64,
    f: i64,
    rate: f64,
    yld: f64,
    redemption: f64,
    freq: i64,
    basis: u8,
    dfc: f64,
    e: f64,
    date1904: bool,
) -> f64 {
    let n = coupon_count(s, m, freq, date1904);
    let dsc = positive_days(s as f64, f as f64, basis, date1904);
    let y = yld / freq as f64;
    let c = 100.0 * rate / freq as f64;
    let mut result = redemption / (1.0 + y).powf(n as f64 - 1.0 + dsc / e);
    result += c * dfc / e / (1.0 + y).powf(dsc / e);
    for k in 2..=n {
        result += c / (1.0 + y).powf(k as f64 - 1.0 + dsc / e);
    }
    let a = positive_days(i as f64, s as f64, basis, date1904);
    result - c * a / e
}

#[allow(clippy::too_many_arguments)]
fn oddf_long(
    s: i64,
    m: i64,
    i: i64,
    f: i64,
    rate: f64,
    yld: f64,
    redemption: f64,
    freq: i64,
    basis: u8,
    e: f64,
    date1904: bool,
) -> f64 {
    let n = coupon_count(f, m, freq, date1904);
    let nq = quasi_coupon_count(f, s, 12 / freq, date1904);
    let dsc = if basis == 2 || basis == 3 {
        let cn = next_coupon(s, f, freq, date1904);
        positive_days(s as f64, cn as f64, basis, date1904)
    } else {
        let cp = prev_coupon(s, f, freq, date1904);
        e - day_count(cp as f64, s as f64, basis, date1904)
    };
    let y = yld / freq as f64;
    let c = 100.0 * rate / freq as f64;
    let mut result = redemption / (1.0 + y).powf(n as f64 + nq as f64 + dsc / e);
    let nc = coupon_count(i, f, freq, date1904);
    let mut late = f;
    let mut dci_sum = 0.0;
    let mut ai_sum = 0.0;
    for idx in (1..=nc).rev() {
        let early = add_coupon_months(late, -(12 / freq), date1904);
        let nli = if basis == 1 { (late - early) as f64 } else { e };
        let dci = if idx > 1 {
            nli
        } else {
            positive_days(i as f64, late as f64, basis, date1904)
        };
        dci_sum += dci / nli;
        let start = if i > early { i } else { early };
        let end = if s < late { s } else { late };
        let ai = positive_days(start as f64, end as f64, basis, date1904);
        ai_sum += ai / nli;
        late = early;
    }
    result += c * dci_sum / (1.0 + y).powf(nq as f64 + dsc / e);
    for k in 1..=n {
        result += c / (1.0 + y).powf(k as f64 + nq as f64 + dsc / e);
    }
    result - c * ai_sum
}

/// Number of whole quasi-coupon periods of `months` length that fit between
/// `start` and `end` (Excel's getCouponsNumber).
fn quasi_coupon_count(start: i64, end: i64, months: i64, date1904: bool) -> i64 {
    let (sy, sm, sd) = serial_to_civil(start as f64, date1904);
    let (ey, em, ed) = serial_to_civil(end as f64, date1904);
    let end_of_month_start = sd == days_in_month(sy, sm, date1904);
    let end_of_month =
        if !end_of_month_start && sm != 1 && sd > 28 && sd < days_in_month(sy, sm, date1904) {
            ed == days_in_month(ey, em, date1904)
        } else {
            end_of_month_start
        };
    // The anchor is `end` stepped 0 months, optionally clamped to month-end.
    let mut new_date = end;
    if end_of_month {
        let (y, m, _) = serial_to_civil(end as f64, date1904);
        new_date = civil_to_serial(y, m, days_in_month(y, m, date1904), date1904);
    }
    let mut coupons = 1i64 + i64::from(end < new_date);
    let mut front = add_coupon_months(new_date, months, date1904);
    while !(front >= end) {
        front = add_coupon_months(front, months, date1904);
        coupons += 1;
    }
    coupons
}

fn oddfprice_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let settlement = arg_num(ctx, &args[0])?;
    let maturity = arg_num(ctx, &args[1])?;
    let issue = arg_num(ctx, &args[2])?;
    let first = arg_num(ctx, &args[3])?;
    let rate = arg_num(ctx, &args[4])?;
    let yld = arg_num(ctx, &args[5])?;
    let redemption = arg_num(ctx, &args[6])?;
    let freq = check_freq(arg_num(ctx, &args[7])?)?;
    let basis = check_basis(opt_num(ctx, args, 8, 0.0)?)?;
    if rate < 0.0 || yld < 0.0 || redemption <= 0.0 {
        return Err(CalcError::Num);
    }
    ok_num(oddfprice_value(
        settlement,
        maturity,
        issue,
        first,
        rate,
        yld,
        redemption,
        freq,
        basis,
        ctx.date1904,
    )?)
}

fn oddfyield_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let settlement = arg_num(ctx, &args[0])?;
    let maturity = arg_num(ctx, &args[1])?;
    let issue = arg_num(ctx, &args[2])?;
    let first = arg_num(ctx, &args[3])?;
    let rate = arg_num(ctx, &args[4])?;
    let price = arg_num(ctx, &args[5])?;
    let redemption = arg_num(ctx, &args[6])?;
    let freq = check_freq(arg_num(ctx, &args[7])?)?;
    let basis = check_basis(opt_num(ctx, args, 8, 0.0)?)?;
    if rate < 0.0 || price <= 0.0 || redemption <= 0.0 {
        return Err(CalcError::Num);
    }
    let s = settlement.trunc() as i64;
    let m = maturity.trunc() as i64;
    let i = issue.trunc() as i64;
    let f = first.trunc() as i64;
    if m <= f || f <= s || s <= i {
        return Err(CalcError::Num);
    }
    if !coupon_lattice_ok(m, f, freq, ctx.date1904) || prev_coupon(i, m, freq, ctx.date1904) < 0 {
        return Err(CalcError::Num);
    }
    let dsm = day_count(settlement, maturity, basis, ctx.date1904);
    let guess = (rate * dsm * 100.0 - (price - 100.0))
        / ((price - 100.0) * 0.25 * (1.0 + 2.0 * dsm) + dsm * 100.0);
    let f = |x: f64| {
        price
            - oddfprice_value(
                settlement,
                maturity,
                issue,
                first,
                rate,
                x,
                redemption,
                freq,
                basis,
                ctx.date1904,
            )
            .unwrap_or(f64::NAN)
    };
    let df = |x: f64| {
        let h = 1e-7 * x.abs().max(1.0);
        (f(x + h) - f(x)) / h
    };
    let x = solve_robust(&f, &df, &[guess, 0.1, 0.05, 0.01, 0.5]).ok_or(CalcError::Num)?;
    ok_num(x)
}

#[allow(clippy::too_many_arguments)]
fn oddlprice_value(
    settlement: f64,
    maturity: f64,
    last_interest: f64,
    rate: f64,
    yld: f64,
    redemption: f64,
    freq: i64,
    basis: u8,
    date1904: bool,
) -> Result<f64, CalcError> {
    let s = settlement.trunc() as i64;
    let m = maturity.trunc() as i64;
    let l = last_interest.trunc() as i64;
    if m <= s || s <= l {
        return Err(CalcError::Num);
    }
    if prev_coupon(l, m, freq, date1904) < 0 {
        return Err(CalcError::Num);
    }
    let coup = last_coupon_anchor(m, l, freq, date1904);
    let f_ai = odd_frac(l, s, coup, freq, basis, date1904);
    let f_dci = odd_frac(l, m, coup, freq, basis, date1904);
    let f_dsci = odd_frac(s, m, coup, freq, basis, date1904);
    let denom = yld * f_dsci + freq as f64;
    if denom == 0.0 {
        return Err(CalcError::Num);
    }
    let result = (redemption * freq as f64
        + 100.0 * rate * (f_dci - f_ai * (1.0 + yld * f_dsci / freq as f64)))
        / denom;
    if result.is_finite() {
        Ok(result)
    } else {
        Err(CalcError::Num)
    }
}

/// The anchor coupon date for the odd-last period: the coupon on/after
/// `maturity` that shares the last interest date's day-of-month.
fn last_coupon_anchor(maturity: i64, last: i64, freq: i64, date1904: bool) -> i64 {
    let months = 12 / freq;
    let (my, mm, md) = serial_to_civil(maturity as f64, date1904);
    let (_, lm, ld) = serial_to_civil(last as f64, date1904);
    // Set the last-interest date into the maturity year, then step forward to
    // the first coupon on/after maturity.
    let mut y = my;
    let mut m = lm;
    let mut d = ld;
    if days_from_civil(y, m, d) > days_from_civil(my, mm, md) {
        y -= 1;
    }
    let mut guard = 0;
    while days_from_civil(y, m, d) < days_from_civil(my, mm, md) {
        let (ny, nm, nd) = shift_months_fwd(y, m, d, months, date1904);
        y = ny;
        m = nm;
        d = nd;
        guard += 1;
        if guard > 10000 {
            break;
        }
    }
    civil_to_serial(y, m, d, date1904)
}

/// Number of coupon periods (fraction included) between `start` and `end`,
/// anchored on the odd-last coupon lattice (Excel's _getFrac).
fn odd_frac(start: i64, end: i64, coup: i64, freq: i64, basis: u8, date1904: bool) -> f64 {
    let months = 12 / freq;
    let (sy, sm, sd) = serial_to_civil(start as f64, date1904);
    // Step `coup` back to the coupon period containing `start`.
    let (_, mut m, mut d) = serial_to_civil(coup as f64, date1904);
    let mut y = sy;
    if days_from_civil(y, m, d) < days_from_civil(sy, sm, sd) {
        y += 1;
    }
    let mut guard = 0;
    while days_from_civil(y, m, d) > days_from_civil(sy, sm, sd) {
        let (ny, nm, nd) = shift_months(y, m, d, months, date1904);
        y = ny;
        m = nm;
        d = nd;
        guard += 1;
        if guard > 10000 {
            break;
        }
    }
    let early = civil_to_serial(y, m, d, date1904);
    let late_serial = add_coupon_months(early, months, date1904);
    if late_serial >= end {
        let days = day_count(start as f64, end as f64, basis, date1904);
        let coupdays = coup_days(early, late_serial, freq, basis, date1904);
        return if coupdays == 0.0 {
            0.0
        } else {
            days / coupdays
        };
    }
    let days_f = day_count(start as f64, late_serial as f64, basis, date1904);
    let coupdays_f = coup_days(early, late_serial, freq, basis, date1904);
    let mut result = if coupdays_f == 0.0 {
        0.0
    } else {
        days_f / coupdays_f
    };
    let mut early_d = late_serial;
    let mut late_d = add_coupon_months(late_serial, months, date1904);
    while late_d < end {
        early_d = add_coupon_months(early_d, months, date1904);
        late_d = add_coupon_months(late_d, months, date1904);
        result += 1.0;
    }
    let days_l = day_count(early_d as f64, end as f64, basis, date1904);
    let coupdays_l = coup_days(early_d, late_d, freq, basis, date1904);
    result
        + if coupdays_l == 0.0 {
            0.0
        } else {
            days_l / coupdays_l
        }
}

fn oddlprice_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let settlement = arg_num(ctx, &args[0])?;
    let maturity = arg_num(ctx, &args[1])?;
    let last_interest = arg_num(ctx, &args[2])?;
    let rate = arg_num(ctx, &args[3])?;
    let yld = arg_num(ctx, &args[4])?;
    let redemption = arg_num(ctx, &args[5])?;
    let freq = check_freq(arg_num(ctx, &args[6])?)?;
    let basis = check_basis(opt_num(ctx, args, 7, 0.0)?)?;
    if rate < 0.0 || yld < 0.0 || redemption <= 0.0 {
        return Err(CalcError::Num);
    }
    ok_num(oddlprice_value(
        settlement,
        maturity,
        last_interest,
        rate,
        yld,
        redemption,
        freq,
        basis,
        ctx.date1904,
    )?)
}

fn oddlyield_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let settlement = arg_num(ctx, &args[0])?;
    let maturity = arg_num(ctx, &args[1])?;
    let last_interest = arg_num(ctx, &args[2])?;
    let rate = arg_num(ctx, &args[3])?;
    let price = arg_num(ctx, &args[4])?;
    let redemption = arg_num(ctx, &args[5])?;
    let freq = check_freq(arg_num(ctx, &args[6])?)?;
    let basis = check_basis(opt_num(ctx, args, 7, 0.0)?)?;
    if rate < 0.0 || price <= 0.0 || redemption <= 0.0 {
        return Err(CalcError::Num);
    }
    let s = settlement.trunc() as i64;
    let m = maturity.trunc() as i64;
    let l = last_interest.trunc() as i64;
    if m <= s || s <= l {
        return Err(CalcError::Num);
    }
    if prev_coupon(l, m, freq, ctx.date1904) < 0 {
        return Err(CalcError::Num);
    }
    let coup = last_coupon_anchor(m, l, freq, ctx.date1904);
    let f_ai = odd_frac(l, s, coup, freq, basis, ctx.date1904);
    let f_dci = odd_frac(l, m, coup, freq, basis, ctx.date1904);
    let f_dsci = odd_frac(s, m, coup, freq, basis, ctx.date1904);
    let denom = f_dsci * price + 100.0 * rate * f_ai * f_dsci / freq as f64;
    if denom == 0.0 {
        return Err(CalcError::Num);
    }
    let result = (freq as f64 * (redemption - price) + 100.0 * rate * (f_dci - f_ai)) / denom;
    ok_num(result)
}

// -- registration -------------------------------------------------------------

const PV: FuncSpec = FuncSpec {
    name: "PV",
    min_args: 3,
    max_args: Some(5),
    volatile: false,
    array_aware: false,
    func: pv_fn,
};

const FV: FuncSpec = FuncSpec {
    name: "FV",
    min_args: 3,
    max_args: Some(5),
    volatile: false,
    array_aware: false,
    func: fv_fn,
};

const PMT: FuncSpec = FuncSpec {
    name: "PMT",
    min_args: 3,
    max_args: Some(5),
    volatile: false,
    array_aware: false,
    func: pmt_fn,
};

const IPMT: FuncSpec = FuncSpec {
    name: "IPMT",
    min_args: 4,
    max_args: Some(6),
    volatile: false,
    array_aware: false,
    func: ipmt_fn,
};

const PPMT: FuncSpec = FuncSpec {
    name: "PPMT",
    min_args: 4,
    max_args: Some(6),
    volatile: false,
    array_aware: false,
    func: ppmt_fn,
};

const NPER: FuncSpec = FuncSpec {
    name: "NPER",
    min_args: 3,
    max_args: Some(5),
    volatile: false,
    array_aware: false,
    func: nper_fn,
};

const RATE: FuncSpec = FuncSpec {
    name: "RATE",
    min_args: 3,
    max_args: Some(6),
    volatile: false,
    array_aware: false,
    func: rate_fn,
};

const CUMIPMT: FuncSpec = FuncSpec {
    name: "CUMIPMT",
    min_args: 6,
    max_args: Some(6),
    volatile: false,
    array_aware: false,
    func: cumipmt_fn,
};

const CUMPRINC: FuncSpec = FuncSpec {
    name: "CUMPRINC",
    min_args: 6,
    max_args: Some(6),
    volatile: false,
    array_aware: false,
    func: cumprinc_fn,
};

const FVSCHEDULE: FuncSpec = FuncSpec {
    name: "FVSCHEDULE",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: true,
    func: fvschedule_fn,
};

const NPV: FuncSpec = FuncSpec {
    name: "NPV",
    min_args: 2,
    max_args: None,
    volatile: false,
    array_aware: true,
    func: npv_fn,
};

const IRR: FuncSpec = FuncSpec {
    name: "IRR",
    min_args: 1,
    max_args: Some(2),
    volatile: false,
    array_aware: true,
    func: irr_fn,
};

const MIRR: FuncSpec = FuncSpec {
    name: "MIRR",
    min_args: 3,
    max_args: Some(3),
    volatile: false,
    array_aware: true,
    func: mirr_fn,
};

const XNPV: FuncSpec = FuncSpec {
    name: "XNPV",
    min_args: 3,
    max_args: Some(3),
    volatile: false,
    array_aware: true,
    func: xnpv_fn,
};

const XIRR: FuncSpec = FuncSpec {
    name: "XIRR",
    min_args: 2,
    max_args: Some(3),
    volatile: false,
    array_aware: true,
    func: xirr_fn,
};

const RRI: FuncSpec = FuncSpec {
    name: "RRI",
    min_args: 3,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: rri_fn,
};

const PDURATION: FuncSpec = FuncSpec {
    name: "PDURATION",
    min_args: 3,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: pduration_fn,
};

const ISPMT: FuncSpec = FuncSpec {
    name: "ISPMT",
    min_args: 4,
    max_args: Some(4),
    volatile: false,
    array_aware: false,
    func: ispmt_fn,
};

const SLN: FuncSpec = FuncSpec {
    name: "SLN",
    min_args: 3,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: sln_fn,
};

const SYD: FuncSpec = FuncSpec {
    name: "SYD",
    min_args: 4,
    max_args: Some(4),
    volatile: false,
    array_aware: false,
    func: syd_fn,
};

const DB: FuncSpec = FuncSpec {
    name: "DB",
    min_args: 4,
    max_args: Some(5),
    volatile: false,
    array_aware: false,
    func: db_fn,
};

const DDB: FuncSpec = FuncSpec {
    name: "DDB",
    min_args: 4,
    max_args: Some(5),
    volatile: false,
    array_aware: false,
    func: ddb_fn,
};

const VDB: FuncSpec = FuncSpec {
    name: "VDB",
    min_args: 5,
    max_args: Some(7),
    volatile: false,
    array_aware: false,
    func: vdb_fn,
};

const EFFECT: FuncSpec = FuncSpec {
    name: "EFFECT",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: effect_fn,
};

const NOMINAL: FuncSpec = FuncSpec {
    name: "NOMINAL",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: nominal_fn,
};

const DOLLARDE: FuncSpec = FuncSpec {
    name: "DOLLARDE",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: dollarde_fn,
};

const DOLLARFR: FuncSpec = FuncSpec {
    name: "DOLLARFR",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: dollarfr_fn,
};

const DISC: FuncSpec = FuncSpec {
    name: "DISC",
    min_args: 4,
    max_args: Some(5),
    volatile: false,
    array_aware: false,
    func: disc_fn,
};

const INTRATE: FuncSpec = FuncSpec {
    name: "INTRATE",
    min_args: 4,
    max_args: Some(5),
    volatile: false,
    array_aware: false,
    func: intrate_fn,
};

const RECEIVED: FuncSpec = FuncSpec {
    name: "RECEIVED",
    min_args: 4,
    max_args: Some(5),
    volatile: false,
    array_aware: false,
    func: received_fn,
};

const PRICEDISC: FuncSpec = FuncSpec {
    name: "PRICEDISC",
    min_args: 4,
    max_args: Some(5),
    volatile: false,
    array_aware: false,
    func: pricedisc_fn,
};

const YIELDDISC: FuncSpec = FuncSpec {
    name: "YIELDDISC",
    min_args: 4,
    max_args: Some(5),
    volatile: false,
    array_aware: false,
    func: yielddisc_fn,
};

const TBILLPRICE: FuncSpec = FuncSpec {
    name: "TBILLPRICE",
    min_args: 3,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: tbillprice_fn,
};

const TBILLYIELD: FuncSpec = FuncSpec {
    name: "TBILLYIELD",
    min_args: 3,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: tbillyield_fn,
};

const TBILLEQ: FuncSpec = FuncSpec {
    name: "TBILLEQ",
    min_args: 3,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: t_bill_eq_fn,
};

const COUPDAYS: FuncSpec = FuncSpec {
    name: "COUPDAYS",
    min_args: 3,
    max_args: Some(4),
    volatile: false,
    array_aware: false,
    func: coupdays_fn,
};

const COUPDAYBS: FuncSpec = FuncSpec {
    name: "COUPDAYBS",
    min_args: 3,
    max_args: Some(4),
    volatile: false,
    array_aware: false,
    func: coupdaybs_fn,
};

const COUPDAYSNC: FuncSpec = FuncSpec {
    name: "COUPDAYSNC",
    min_args: 3,
    max_args: Some(4),
    volatile: false,
    array_aware: false,
    func: coupdaysnc_fn,
};

const COUPNUM: FuncSpec = FuncSpec {
    name: "COUPNUM",
    min_args: 3,
    max_args: Some(4),
    volatile: false,
    array_aware: false,
    func: coupnum_fn,
};

const COUPNCD: FuncSpec = FuncSpec {
    name: "COUPNCD",
    min_args: 3,
    max_args: Some(4),
    volatile: false,
    array_aware: false,
    func: coupncd_fn,
};

const COUPPCD: FuncSpec = FuncSpec {
    name: "COUPPCD",
    min_args: 3,
    max_args: Some(4),
    volatile: false,
    array_aware: false,
    func: couppcd_fn,
};

const ACCRINT: FuncSpec = FuncSpec {
    name: "ACCRINT",
    min_args: 6,
    max_args: Some(8),
    volatile: false,
    array_aware: false,
    func: accrint_fn,
};

const ACCRINTM: FuncSpec = FuncSpec {
    name: "ACCRINTM",
    min_args: 4,
    max_args: Some(5),
    volatile: false,
    array_aware: false,
    func: accrintm_fn,
};

const DURATION: FuncSpec = FuncSpec {
    name: "DURATION",
    min_args: 5,
    max_args: Some(6),
    volatile: false,
    array_aware: false,
    func: duration_fn,
};

const MDURATION: FuncSpec = FuncSpec {
    name: "MDURATION",
    min_args: 5,
    max_args: Some(6),
    volatile: false,
    array_aware: false,
    func: mduration_fn,
};

const PRICE: FuncSpec = FuncSpec {
    name: "PRICE",
    min_args: 6,
    max_args: Some(7),
    volatile: false,
    array_aware: false,
    func: price_fn,
};

const YIELD: FuncSpec = FuncSpec {
    name: "YIELD",
    min_args: 6,
    max_args: Some(7),
    volatile: false,
    array_aware: false,
    func: yield_fn,
};

const AMORLINC: FuncSpec = FuncSpec {
    name: "AMORLINC",
    min_args: 6,
    max_args: Some(7),
    volatile: false,
    array_aware: false,
    func: amorlinc_fn,
};

const PRICEMAT: FuncSpec = FuncSpec {
    name: "PRICEMAT",
    min_args: 5,
    max_args: Some(6),
    volatile: false,
    array_aware: false,
    func: pricemat_fn,
};

const YIELDMAT: FuncSpec = FuncSpec {
    name: "YIELDMAT",
    min_args: 5,
    max_args: Some(6),
    volatile: false,
    array_aware: false,
    func: yieldmat_fn,
};

const ODDFPRICE: FuncSpec = FuncSpec {
    name: "ODDFPRICE",
    min_args: 8,
    max_args: Some(9),
    volatile: false,
    array_aware: false,
    func: oddfprice_fn,
};

const ODDFYIELD: FuncSpec = FuncSpec {
    name: "ODDFYIELD",
    min_args: 8,
    max_args: Some(9),
    volatile: false,
    array_aware: false,
    func: oddfyield_fn,
};

const ODDLPRICE: FuncSpec = FuncSpec {
    name: "ODDLPRICE",
    min_args: 7,
    max_args: Some(8),
    volatile: false,
    array_aware: false,
    func: oddlprice_fn,
};

const ODDLYIELD: FuncSpec = FuncSpec {
    name: "ODDLYIELD",
    min_args: 7,
    max_args: Some(8),
    volatile: false,
    array_aware: false,
    func: oddlyield_fn,
};

pub fn register(r: &mut Registry) {
    r.register(&PV);
    r.register(&FV);
    r.register(&PMT);
    r.register(&IPMT);
    r.register(&PPMT);
    r.register(&NPER);
    r.register(&RATE);
    r.register(&CUMIPMT);
    r.register(&CUMPRINC);
    r.register(&FVSCHEDULE);
    r.register(&NPV);
    r.register(&IRR);
    r.register(&MIRR);
    r.register(&XNPV);
    r.register(&XIRR);
    r.register(&RRI);
    r.register(&PDURATION);
    r.register(&ISPMT);
    r.register(&SLN);
    r.register(&SYD);
    r.register(&DB);
    r.register(&DDB);
    r.register(&VDB);
    r.register(&EFFECT);
    r.register(&NOMINAL);
    r.register(&DOLLARDE);
    r.register(&DOLLARFR);
    r.register(&DISC);
    r.register(&INTRATE);
    r.register(&RECEIVED);
    r.register(&PRICEDISC);
    r.register(&YIELDDISC);
    r.register(&TBILLPRICE);
    r.register(&TBILLYIELD);
    r.register(&TBILLEQ);
    r.register(&COUPDAYS);
    r.register(&COUPDAYBS);
    r.register(&COUPDAYSNC);
    r.register(&COUPNUM);
    r.register(&COUPNCD);
    r.register(&COUPPCD);
    r.register(&ACCRINT);
    r.register(&ACCRINTM);
    r.register(&DURATION);
    r.register(&MDURATION);
    r.register(&PRICE);
    r.register(&YIELD);
    r.register(&AMORLINC);
    r.register(&PRICEMAT);
    r.register(&YIELDMAT);
    r.register(&ODDFPRICE);
    r.register(&ODDFYIELD);
    r.register(&ODDLPRICE);
    r.register(&ODDLYIELD);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turbo::calc::testkit::Grid;
    use pretty_assertions::assert_eq;

    fn near(a: f64, b: f64, tol: f64) {
        assert!(
            (a - b).abs() <= tol * b.abs().max(1.0),
            "{a} vs expected {b} (tol {tol})"
        );
    }

    #[test]
    fn tvm_sign_conventions_match_excel() {
        let g = Grid::empty();
        near(g.num("=PMT(0.08/12, 10, 10000)"), -1037.03, 1e-4);
        near(g.num("=PMT(0.06/12, 18*12, 0, 50000)"), -129.08, 1e-3);
        near(g.num("=FV(0.06/12, 10, -200, -500, 1)"), 2581.40, 1e-4);
        near(g.num("=FV(0.12/12, 12, -1000)"), 12682.50, 1e-4);
        near(g.num("=PV(0.08/12, 20*12, 500)"), -59777.15, 1e-4);
        // type=1 shifts the annuity one period earlier.
        near(g.num("=PV(0.08/12, 20*12, 500, 0, 1)"), -60176.5, 1e-3);
        // PV and FV must invert each other for both timings.
        near(
            g.num("=FV(0.08/12, 20*12, 500, PV(0.08/12, 20*12, 500, 0, 1), 1)"),
            0.0,
            1e-6,
        );
    }

    #[test]
    fn ipmt_ppmt_split_and_period_1() {
        let g = Grid::empty();
        near(g.num("=IPMT(0.1/12, 1, 3*12, 8000)"), -66.67, 1e-4);
        near(g.num("=PPMT(0.1/12, 1, 3*12, 8000)"), -191.48, 1e-3);
        near(
            g.num("=IPMT(0.1/12, 2, 3*12, 8000) + PPMT(0.1/12, 2, 3*12, 8000)"),
            g.num("=PMT(0.1/12, 3*12, 8000)"),
            1e-9,
        );
    }

    #[test]
    fn type_argument_changes_tvm_results() {
        let g = Grid::empty();
        let end = g.num("=FV(0.05/12, 12, -100, 0, 0)");
        let begin = g.num("=FV(0.05/12, 12, -100, 0, 1)");
        assert!((begin - end).abs() > 0.5);
        near(begin, end * (1.0 + 0.05 / 12.0), 1e-9);
        assert_eq!(
            g.calc("=PMT(0.1, 12, 1000, 0, 2)"),
            crate::turbo::calc::testkit::Outcome::Err(CalcError::Num)
        );
    }

    #[test]
    fn nper_and_rate_roundtrip() {
        let g = Grid::empty();
        near(g.num("=NPER(0.12/12, -100, -1000, 10000)"), 60.08, 1e-3);
        near(g.num("=RATE(4*12, -200, 8000)"), 0.007701, 1e-3);
        let n = g.num("=NPER(0.007701, -200, 8000)");
        near(n, 48.0, 1e-3);
    }

    #[test]
    fn cumipmt_and_cumprinc() {
        let g = Grid::empty();
        near(
            g.num("=CUMIPMT(0.09/12, 30*12, 125000, 13, 24, 0)"),
            -11135.23,
            1e-4,
        );
        near(
            g.num("=CUMPRINC(0.09/12, 30*12, 125000, 13, 24, 0)"),
            -934.10,
            1e-3,
        );
        near(
            g.num("=CUMIPMT(0.09/12, 30*12, 125000, 13, 24, 1)"),
            -11052.34,
            1e-3,
        );
        near(
            g.num("=CUMPRINC(0.09/12, 30*12, 125000, 13, 24, 1)"),
            -927.15,
            1e-3,
        );
        // type=1 shifts payments to the start: interest drops, principal rises.
        let g2 = Grid::empty();
        let pmt1 = g2.num("=PMT(0.09/12, 30*12, 125000, 0, 1)");
        near(
            g2.num("=CUMIPMT(0.09/12, 30*12, 125000, 13, 24, 1) + CUMPRINC(0.09/12, 30*12, 125000, 13, 24, 1)"),
            12.0 * pmt1,
            1e-6,
        );
        assert_eq!(
            g.calc("=IPMT(0.1/12, 1, 3*12, 8000, 0, 1)"),
            crate::turbo::calc::testkit::Outcome::Value(CalcValue::Number(0.0))
        );
        assert_eq!(
            g.calc("=CUMIPMT(0.09/12, 30*12, 125000, 24, 13, 0)"),
            crate::turbo::calc::testkit::Outcome::Err(CalcError::Num)
        );
    }

    #[test]
    fn fvschedule_compounds() {
        let g = Grid::empty().col("A1", &[0.09, 0.11, 0.1]);
        near(g.num("=FVSCHEDULE(1, A1:A3)"), 1.33089, 1e-5);
        near(g.num("=FVSCHEDULE(1, {0.09;0.11;0.1})"), 1.33089, 1e-5);
    }

    #[test]
    fn npv_discounts_from_period_1() {
        let g = Grid::empty();
        near(g.num("=NPV(0.1, -10000, 3000, 4200, 6800)"), 1188.44, 1e-3);
        let g = Grid::empty().row("A1", &[-10000.0, 3000.0, 4200.0, 6800.0]);
        near(g.num("=NPV(0.1, A1:D1)"), 1188.44, 1e-3);
    }

    #[test]
    fn irr_requires_pos_and_neg_and_converges() {
        let g = Grid::empty().col(
            "A1",
            &[-70000.0, 12000.0, 15000.0, 18000.0, 21000.0, 26000.0],
        );
        near(g.num("=IRR(A1:A6)"), 0.0868, 5e-3);
        let all_pos = Grid::empty().col("A1", &[1.0, 2.0, 3.0]);
        assert_eq!(
            all_pos.calc("=IRR(A1:A3)"),
            crate::turbo::calc::testkit::Outcome::Err(CalcError::Num)
        );
    }

    #[test]
    fn mirr_matches_excel_example() {
        let g = Grid::empty().col(
            "A1",
            &[-120000.0, 39000.0, 30000.0, 21000.0, 37000.0, 46000.0],
        );
        near(g.num("=MIRR(A1:A6, 0.1, 0.12)"), 0.126094, 1e-3);
    }

    #[test]
    fn xnpv_and_xirr() {
        let g = Grid::empty()
            .col("A1", &[-10000.0, 2750.0, 4250.0, 3250.0, 2750.0])
            .col("B1", &[39448.0, 39508.0, 39751.0, 39859.0, 39904.0]);
        near(g.num("=XNPV(0.09, A1:A5, B1:B5)"), 2086.65, 1e-3);
        near(g.num("=XIRR(A1:A5, B1:B5)"), 0.373362535, 1e-3);
    }

    #[test]
    fn rri_pduration_ispmt() {
        let g = Grid::empty();
        near(g.num("=RRI(96, 10000, 11000)"), 0.0010, 1e-3);
        near(g.num("=PDURATION(0.025, 2000, 2200)"), 3.86, 1e-3);
        near(g.num("=ISPMT(0.1/12, 1, 3*12, 8000)"), -64.81, 1e-3);
        near(g.num("=ISPMT(0.1/12, 0, 3*12, 8000)"), -66.67, 1e-3);
    }

    #[test]
    fn straight_line_and_sum_of_years() {
        let g = Grid::empty();
        near(g.num("=SLN(30000, 7500, 10)"), 2250.0, 1e-9);
        near(g.num("=SYD(30000, 7500, 10, 1)"), 4090.91, 1e-4);
        near(g.num("=SYD(30000, 7500, 10, 10)"), 409.09, 1e-3);
    }

    #[test]
    fn db_rates_rounded_to_three_decimals() {
        let g = Grid::empty();
        near(g.num("=DB(1000000, 100000, 6, 1, 7)"), 186083.33, 1e-4);
        near(g.num("=DB(1000000, 100000, 6, 2, 7)"), 259639.42, 1e-4);
        near(g.num("=DB(1000000, 100000, 6, 7, 7)"), 15845.10, 1e-3);
        near(g.num("=DB(1000000, 100000, 6, 8, 7)"), 0.0, 1e-9);
    }

    #[test]
    fn ddb_clamps_at_salvage() {
        let g = Grid::empty();
        near(g.num("=DDB(2400, 300, 3650, 1)"), 1.32, 1e-2);
        near(g.num("=DDB(2400, 300, 120, 1, 2)"), 40.0, 1e-9);
        near(g.num("=DDB(2400, 300, 10, 1, 2)"), 480.0, 1e-9);
        near(g.num("=DDB(2400, 300, 10, 2, 1.5)"), 306.0, 1e-4);
        near(g.num("=DDB(2400, 300, 10, 9, 2)"), 80.53, 1e-3);
        near(g.num("=DDB(2400, 300, 10, 10, 2)"), 22.12, 1e-3);
    }

    #[test]
    fn vdb_matches_excel_examples() {
        let g = Grid::empty();
        near(g.num("=VDB(2400, 300, 10, 0, 1, 2)"), 480.0, 1e-6);
        near(g.num("=VDB(2400, 300, 10, 5, 6, 2)"), 157.29, 1e-3);
        near(g.num("=VDB(2400, 300, 10, 9, 10, 2)"), 22.12, 1e-3);
        near(g.num("=VDB(2400, 300, 10, 0, 10, 2)"), 2100.0, 1e-3);
    }

    #[test]
    fn vdb_switch_behaviour() {
        let g = Grid::empty();
        // With the switch on, VDB reaches exactly cost - salvage.
        near(g.num("=VDB(1000, 0, 5, 0, 5, 2)"), 1000.0, 1e-6);
        // With no_switch=TRUE it stays pure declining balance.
        near(g.num("=VDB(1000, 0, 5, 0, 5, 2, TRUE)"), 922.24, 1e-3);
    }

    #[test]
    fn rate_conversion_functions() {
        let g = Grid::empty();
        near(g.num("=EFFECT(0.0525, 4)"), 0.053543, 1e-5);
        near(g.num("=NOMINAL(0.053543, 4)"), 0.0525, 1e-4);
        near(g.num("=DOLLARDE(1.02, 16)"), 1.125, 1e-9);
        near(g.num("=DOLLARDE(1.1, 32)"), 1.3125, 1e-9);
        near(g.num("=DOLLARFR(1.125, 16)"), 1.02, 1e-9);
        near(g.num("=DOLLARFR(1.3125, 32)"), 1.1, 1e-9);
    }

    #[test]
    fn day_count_bases() {
        let g = Grid::empty();
        assert_eq!(
            g.num("=COUPDAYBS(DATE(2011,1,25), DATE(2011,11,15), 2, 1)"),
            71.0
        );
        assert_eq!(
            g.num("=COUPDAYS(DATE(2011,1,25), DATE(2011,11,15), 2, 1)"),
            181.0
        );
        assert_eq!(
            g.num("=COUPDAYSNC(DATE(2011,1,25), DATE(2011,11,15), 2, 1)"),
            110.0
        );
        assert_eq!(
            g.num("=COUPNUM(DATE(2011,1,25), DATE(2011,11,15), 2, 1)"),
            2.0
        );
        assert_eq!(
            g.num("=COUPNCD(DATE(2011,1,25), DATE(2011,11,15), 2, 1)"),
            g.num("=DATE(2011,5,15)")
        );
        assert_eq!(
            g.num("=COUPPCD(DATE(2011,1,25), DATE(2011,11,15), 2, 1)"),
            g.num("=DATE(2010,11,15)")
        );
        assert_eq!(
            g.num("=COUPDAYS(DATE(2011,1,25), DATE(2011,11,15), 2, 0)"),
            180.0
        );
    }

    #[test]
    fn accrint_accrues_by_period() {
        let g = Grid::empty();
        near(
            g.num("=ACCRINT(DATE(2008,3,1), DATE(2008,8,31), DATE(2008,5,1), 0.1, 1000, 2, 0)"),
            16.67,
            1e-3,
        );
        near(
            g.num("=ACCRINT(DATE(2008,3,5), DATE(2008,8,31), DATE(2008,5,1), 0.1, 1000, 2, 0)"),
            15.56,
            1e-3,
        );
        near(
            g.num("=ACCRINT(DATE(2008,3,5), DATE(2008,8,31), DATE(2008,8,31), 0.1, 1000, 2, 0)"),
            48.89,
            1e-3,
        );
        near(
            g.num("=ACCRINTM(DATE(2008,4,1), DATE(2008,12,31), 0.1, 1000, 0)"),
            75.0,
            1e-3,
        );
        near(
            g.num("=ACCRINTM(DATE(2008,4,1), DATE(2008,12,31), 0.1, 1000, 2)"),
            76.11,
            1e-3,
        );
    }

    #[test]
    fn discount_securities_are_mutually_consistent() {
        let g = Grid::empty();
        let price = g.num("=PRICEDISC(DATE(2008,2,15), DATE(2008,11,15), 0.0525, 100, 0)");
        near(price, 96.0625, 1e-6);
        near(
            g.num("=YIELDDISC(DATE(2008,2,15), DATE(2008,11,15), 96.0625, 100, 0)"),
            0.054649,
            1e-4,
        );
        near(
            g.num("=DISC(DATE(2008,2,15), DATE(2008,11,15), 96.0625, 100, 0)"),
            0.0525,
            1e-5,
        );
        near(
            g.num("=INTRATE(DATE(2008,2,15), DATE(2008,11,15), 1000000, 1060000, 0)"),
            0.08,
            1e-6,
        );
        near(
            g.num("=RECEIVED(DATE(2008,2,15), DATE(2008,11,15), 1000000, 0.0575, 2)"),
            1045766.0,
            1e-4,
        );
    }

    #[test]
    fn treasury_bill_functions() {
        let g = Grid::empty();
        near(
            g.num("=TBILLPRICE(DATE(2008,3,31), DATE(2008,6,1), 0.09)"),
            98.45,
            1e-4,
        );
        near(
            g.num("=TBILLYIELD(DATE(2008,3,31), DATE(2008,6,1), 98.45)"),
            0.091417,
            1e-4,
        );
        near(
            g.num("=TBILLEQ(DATE(2008,3,31), DATE(2008,6,1), 0.0914)"),
            0.094151,
            1e-4,
        );
    }

    #[test]
    fn price_and_yield_invert() {
        let g = Grid::empty();
        near(
            g.num("=PRICE(DATE(2008,2,15), DATE(2017,11,15), 0.0575, 0.065, 100, 2, 0)"),
            94.63,
            1e-3,
        );
        near(
            g.num("=YIELD(DATE(2008,2,15), DATE(2017,11,15), 0.0575, 94.63, 100, 2, 0)"),
            0.065,
            1e-4,
        );
    }

    #[test]
    fn duration_and_modified_duration() {
        let g = Grid::empty();
        near(
            g.num("=DURATION(DATE(2008,1,1), DATE(2016,1,1), 0.08, 0.09, 2, 1)"),
            5.9938,
            1e-4,
        );
        near(
            g.num("=MDURATION(DATE(2008,1,1), DATE(2016,1,1), 0.08, 0.09, 2, 1)"),
            5.7357,
            1e-4,
        );
    }

    #[test]
    fn error_cases() {
        let g = Grid::empty();
        assert_eq!(
            g.calc("=FV(0.1, 12)"),
            crate::turbo::calc::testkit::Outcome::Err(CalcError::Value)
        );
        assert_eq!(
            g.calc("=DOLLARFR(1.1, 0)"),
            crate::turbo::calc::testkit::Outcome::Err(CalcError::Div0)
        );
        assert_eq!(
            g.calc("=COUPDAYS(DATE(2011,11,15), DATE(2011,1,25), 2, 1)"),
            crate::turbo::calc::testkit::Outcome::Err(CalcError::Num)
        );
        assert_eq!(
            g.calc("=PRICE(DATE(2008,2,15), DATE(2017,11,15), 0.0575, 0.065, 100, 3, 0)"),
            crate::turbo::calc::testkit::Outcome::Err(CalcError::Num)
        );
        assert_eq!(
            g.calc("=DISC(DATE(2008,2,15), DATE(2008,11,15), 0, 100)"),
            crate::turbo::calc::testkit::Outcome::Err(CalcError::Num)
        );
        assert_eq!(
            g.calc("=RATE(0, -200, 8000)"),
            crate::turbo::calc::testkit::Outcome::Err(CalcError::Num)
        );
    }

    // ---- Lane B round-2 oracle cases (Excel-measured values) ----------------

    #[test]
    fn lane_b_basis1_day_counts_match_excel() {
        let g = Grid::empty();
        // DISC: 2018-07-01 -> 2048-01-01, basis 1. The 365-day year would give
        // 0.000685899; Excel keeps the 365.258-day average.
        near(
            g.num("=DISC(43282, 54058, 97.975, 100, 1)"),
            0.000686384169,
            1e-11,
        );
        near(
            g.num("=PRICEDISC(39763, 44256, 0.0625, 100, 1)"),
            23.1252444271,
            1e-8,
        );
        near(
            g.num("=YIELDDISC(39763, 44256, 98.45, 100, 1)"),
            0.00128000671242,
            1e-12,
        );
        near(
            g.num("=ACCRINTM(40941, 44266, 0.1, 1000, 1)"),
            910.210785655626,
            1e-8,
        );
        near(
            g.num("=ACCRINT(39507, 39691, 39569, 0.1, 1000, 2, 1)"),
            16.847826086957,
            1e-9,
        );
        near(
            g.num("=DURATION(43282, 54058, 0.08, 0.09, 1, 1)"),
            10.8778775299,
            1e-7,
        );
        near(
            g.num("=MDURATION(43282, 54058, 0.08, 0.09, 1, 1)"),
            9.97970415591,
            1e-7,
        );
    }

    #[test]
    fn lane_b_error_typing_matches_excel() {
        let g = Grid::empty();
        // DOLLARDE/DOLLARFR: a fraction in (0,1) truncates to 0 -> #DIV/0!.
        assert_eq!(
            g.calc("=DOLLARDE(1.02, 0.1)"),
            crate::turbo::calc::testkit::Outcome::Err(CalcError::Div0)
        );
        assert_eq!(
            g.calc("=DOLLARFR(1.02, 0.1)"),
            crate::turbo::calc::testkit::Outcome::Err(CalcError::Div0)
        );
        // EFFECT/NOMINAL reject a boolean compounding argument.
        assert_eq!(
            g.calc("=EFFECT(0.0525, TRUE)"),
            crate::turbo::calc::testkit::Outcome::Err(CalcError::Value)
        );
        assert_eq!(
            g.calc("=NOMINAL(0.053543, TRUE)"),
            crate::turbo::calc::testkit::Outcome::Err(CalcError::Value)
        );
        // FVSCHEDULE rejects a boolean principal.
        assert_eq!(
            g.calc("=FVSCHEDULE(TRUE, 0.1)"),
            crate::turbo::calc::testkit::Outcome::Err(CalcError::Value)
        );
        // FV(-1, 0, ...) is the degenerate (1+rate)^nper case.
        assert_eq!(
            g.calc("=FV(-1, 0, -200, -500, 0)"),
            crate::turbo::calc::testkit::Outcome::Err(CalcError::Num)
        );
        // MIRR with no negative cash flows: Excel reports #DIV/0!.
        let g2 = Grid::empty().row(
            "A1",
            &[700000.0, 120000.0, 150000.0, 180000.0, 210000.0, 260000.0],
        );
        assert_eq!(
            g2.calc("=MIRR(A1:F1, 0.1, 0.12)"),
            crate::turbo::calc::testkit::Outcome::Err(CalcError::Div0)
        );
        // SYD: per beyond life is #NUM!.
        assert_eq!(
            g.calc("=SYD(300000, 75000, 10, 11)"),
            crate::turbo::calc::testkit::Outcome::Err(CalcError::Num)
        );
        // VDB fractional period matches Excel (not 125.82912).
        near(
            g.num("=VDB(24000, 3000, 10, 6.1, 6.2, 2, 0)"),
            123.3125376,
            1e-8,
        );
        // COUPPCD clamps pre-1900 coupon dates to serial 0.
        near(g.num("=COUPPCD(1, 40862, 2, 1)"), 0.0, 1e-9);
        // TBILLEQ over 182 days switches to the equivalent-yield form.
        near(g.num("=TBILLEQ(39538, 39753, 0.0914)"), 0.09730435852, 1e-9);
        // FVSCHEDULE with a blank principal compounds from 0.
        near(g.num("=FVSCHEDULE(0, {0.09;0.11;0.1})"), 0.0, 1e-12);
    }

    #[test]
    fn lane_b_new_functions_match_excel() {
        let g = Grid::empty();
        // AMORLINC (French linear amortization).
        near(
            g.num("=AMORLINC(2400, DATE(2008,8,19), DATE(2008,12,31), 300, 1, 0.15, 0)"),
            360.0,
            1e-9,
        );
        near(
            g.num("=AMORLINC(2400, DATE(2008,8,19), DATE(2008,12,31), 300, 0, 0.15, 0)"),
            132.0,
            1e-9,
        );
        near(
            g.num("=AMORLINC(2400, DATE(2008,8,19), DATE(2008,12,31), 300, 6, 0.15, 0)"),
            168.0,
            1e-7,
        );
        near(
            g.num("=AMORLINC(2400, DATE(2008,8,19), DATE(2008,12,31), 300, 7, 0.15, 0)"),
            0.0,
            1e-9,
        );
        near(
            g.num("=AMORLINC(2400, DATE(2008,8,19), DATE(2008,12,31), 300, 0, 0.15, 1)"),
            131.803278688525,
            1e-8,
        );
        near(
            g.num("=AMORLINC(2400, DATE(2008,8,19), DATE(2008,12,31), 300, 0, 0.15, 3)"),
            132.164383561644,
            1e-8,
        );
        near(
            g.num("=AMORLINC(2400, DATE(2008,8,19), DATE(2008,12,31), 300, 0, 0.15, 4)"),
            131.0,
            1e-8,
        );
        // PRICEMAT / YIELDMAT (interest at maturity).
        near(
            g.num(
                "=PRICEMAT(DATE(2008,11,11), DATE(2021,3,1), DATE(2008,10,15), 0.0785, 0.0625, 1)",
            ),
            110.862780081706,
            1e-9,
        );
        near(
            g.num(
                "=PRICEMAT(DATE(2008,11,11), DATE(2021,3,1), DATE(2008,10,15), 0.0785, 0.0625, 0)",
            ),
            110.882869098244,
            1e-9,
        );
        near(
            g.num(
                "=YIELDMAT(DATE(2008,11,11), DATE(2021,3,1), DATE(2008,10,15), 0.0785, 98.45, 1)",
            ),
            0.080544639989,
            1e-10,
        );
        // ODDFPRICE / ODDFYIELD (irregular first coupon).
        near(g.num("=ODDFPRICE(DATE(2008,11,11), DATE(2021,3,1), DATE(2008,10,15), DATE(2009,3,1), 0.0785, 0.0625, 100, 2, 1)"), 113.597717474, 1e-8);
        near(g.num("=ODDFPRICE(DATE(2008,11,11), DATE(2021,3,1), DATE(2008,10,15), DATE(2009,3,1), 0.0785, 0.0625, 100, 1, 1)"), 113.494585545507, 1e-8);
        near(g.num("=ODDFPRICE(DATE(2008,11,11), DATE(2021,3,1), DATE(2008,10,15), DATE(2009,3,1), 0.0785, 0.0625, 100, 4, 1)"), 113.650021611, 1e-8);
        near(g.num("=ODDFYIELD(DATE(2008,11,11), DATE(2021,3,1), DATE(2008,10,15), DATE(2009,3,1), 0.0785, 84.5, 100, 2, 1)"), 0.100766449804, 1e-8);
        near(g.num("=ODDFYIELD(DATE(2008,11,11), DATE(2021,3,1), DATE(2008,10,15), DATE(2009,3,1), 0.0785, 84.5, 100, 1, 1)"), 0.101169886094, 1e-8);
        // ODDLPRICE / ODDLYIELD (irregular last coupon).
        near(g.num("=ODDLPRICE(DATE(2008,11,11), DATE(2021,3,1), DATE(2008,10,15), 0.0785, 0.0625, 100, 2, 1)"), 110.8745242842, 1e-8);
        near(g.num("=ODDLPRICE(DATE(2008,11,11), DATE(2021,3,1), DATE(2008,10,15), 0.0785, 0.0625, 100, 1, 1)"), 110.87480393587, 1e-8);
        near(g.num("=ODDLYIELD(DATE(2008,11,11), DATE(2021,3,1), DATE(2008,10,15), 0.0785, 84.5, 100, 2, 1)"), 0.107072088907, 1e-8);
        near(g.num("=ODDLYIELD(DATE(2008,11,11), DATE(2021,3,1), DATE(2008,10,15), 0.0785, 84.5, 100, 1, 1)"), 0.107075093237, 1e-8);
    }
}
