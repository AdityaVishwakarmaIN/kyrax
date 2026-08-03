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

/// Basis 1 days-in-year (ODF procedure E).
fn actual_actual_days_in_year(s1: f64, s2: f64, date1904: bool) -> i64 {
    let (y1, m1, d1) = serial_to_civil(s1, date1904);
    let (y2, m2, d2) = serial_to_civil(s2, date1904);
    let a = y1 != y2;
    let b = y2 != y1 + 1;
    let c = m1 < m2;
    let d = m1 == m2;
    let e = d1 < d2;
    if (a && b) || (a && c) || (a && d && e) {
        let mut sum = 0i64;
        for y in y1..=y2 {
            sum += if is_leap_system(y, date1904) {
                366
            } else {
                365
            };
        }
        return sum / (y2 - y1 + 1);
    }
    if a && is_leap_system(y1, date1904) {
        return 366;
    }
    let o1 = days_from_civil(y1, m1, d1);
    let o2 = days_from_civil(y2, m2, d2);
    for y in y1..=y2 {
        if is_leap_system(y, date1904) {
            let o = days_from_civil(y, 2, 29);
            if o1 < o && o < o2 {
                return 366;
            }
        }
    }
    if m2 == 2 && d2 == 29 {
        return 366;
    }
    365
}

/// Days between two serials under a basis. `s1 <= s2` for every internal call.
fn day_count(s1: f64, s2: f64, basis: u8, date1904: bool) -> f64 {
    match basis {
        0 => days_360_us(s1, s2, date1904) as f64,
        1 | 2 | 3 => (clamp_serial(s2) - clamp_serial(s1)) as f64,
        _ => days_360_eu(s1, s2, date1904) as f64,
    }
}

/// Days per year for the "B" factor (bases 0/2/4 -> 360, 3 -> 365, 1 -> actual).
fn days_in_year(s1: f64, s2: f64, basis: u8, date1904: bool) -> f64 {
    match basis {
        0 | 2 | 4 => 360.0,
        3 => 365.0,
        _ => actual_actual_days_in_year(s1, s2, date1904) as f64,
    }
}

fn year_fraction(s1: f64, s2: f64, basis: u8, date1904: bool) -> f64 {
    match basis {
        0 => days_360_us(s1, s2, date1904) as f64 / 360.0,
        1 => {
            (clamp_serial(s2) - clamp_serial(s1)) as f64
                / actual_actual_days_in_year(s1, s2, date1904) as f64
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
    let principal = arg_num(ctx, &args[0])?;
    let rates = collect_values(ctx, &args[1..])?;
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
    if n < 2 {
        return Err(CalcError::Num);
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
        return Err(CalcError::Num);
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
        let t = (d.trunc() - d0) as f64 / 365.0;
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
    if life <= 0.0 {
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
        || month < 1.0
        || month > 12.0
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

/// Per-period VDB core with the optional straight-line switch (LibreOffice's
/// ScInterVDB). `life1` is the remaining life counter used for the switch.
fn inter_vdb(cost: f64, salvage: f64, life: f64, life1: f64, period: f64, factor: f64) -> f64 {
    let int_end = period.ceil();
    let loop_end = int_end as i64;
    let mut vdb = 0.0;
    let mut sln = 0.0;
    let mut salvage_value = cost - salvage;
    let mut now_sln = false;
    for i in 1..=loop_end {
        let term;
        if !now_sln {
            let ddb = get_ddb(cost, salvage, life, i as f64, factor);
            sln = salvage_value / (life1 - (i as f64 - 1.0));
            if sln > ddb {
                term = sln;
                now_sln = true;
            } else {
                term = ddb;
                salvage_value -= ddb;
            }
        } else {
            term = sln;
        }
        let term = if i == loop_end {
            term * (period + 1.0 - int_end)
        } else {
            term
        };
        vdb += term;
    }
    vdb
}

fn vdb_value(
    cost: f64,
    salvage: f64,
    life: f64,
    start: f64,
    end: f64,
    factor: f64,
    no_switch: bool,
) -> f64 {
    let int_start = start.floor();
    let int_end = end.ceil();
    let loop_start = int_start as i64;
    let loop_end = int_end as i64;
    if no_switch {
        let mut vdb = 0.0;
        for i in (loop_start + 1)..=loop_end {
            let mut term = get_ddb(cost, salvage, life, i as f64, factor);
            if i == loop_start + 1 {
                term *= end.min(int_start + 1.0) - start;
            } else if i == loop_end {
                term *= end + 1.0 - int_end;
            }
            vdb += term;
        }
        vdb
    } else {
        let mut part = 0.0;
        if start != int_start || end != int_end {
            if start != int_start {
                let temp_int_end = int_start + 1.0;
                let temp_value = cost - inter_vdb(cost, salvage, life, life, int_start, factor);
                part += (start - int_start)
                    * inter_vdb(
                        temp_value,
                        salvage,
                        life,
                        life - int_start,
                        temp_int_end - int_start,
                        factor,
                    );
            }
            if end != int_end {
                let temp_int_start = int_end - 1.0;
                let temp_value =
                    cost - inter_vdb(cost, salvage, life, life, temp_int_start, factor);
                part += (int_end - end)
                    * inter_vdb(
                        temp_value,
                        salvage,
                        life,
                        life - temp_int_start,
                        int_end - temp_int_start,
                        factor,
                    );
            }
        }
        let cost2 = cost - inter_vdb(cost, salvage, life, life, int_start, factor);
        let mut vdb = inter_vdb(
            cost2,
            salvage,
            life,
            life - int_start,
            int_end - int_start,
            factor,
        );
        vdb -= part;
        vdb
    }
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
        || salvage > cost
        || life <= 0.0
        || factor <= 0.0
        || start < 0.0
        || end < start
        || end > life
    {
        return Err(CalcError::Num);
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
    let n = arg_num(ctx, &args[1])?.trunc();
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
    let n = arg_num(ctx, &args[1])?.trunc();
    if r <= 0.0 || n < 1.0 {
        return Err(CalcError::Num);
    }
    ok_num(n * ((1.0 + r).powf(1.0 / n) - 1.0))
}

fn dollarde_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let d = arg_num(ctx, &args[0])?;
    let f = arg_num(ctx, &args[1])?;
    if f < 0.0 {
        return Err(CalcError::Num);
    }
    if f == 0.0 {
        return Err(CalcError::Div0);
    }
    let int = d.floor();
    ok_num(int + (d - int) * 100.0 / f)
}

fn dollarfr_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let d = arg_num(ctx, &args[0])?;
    let f = arg_num(ctx, &args[1])?;
    if f < 0.0 {
        return Err(CalcError::Num);
    }
    if f == 0.0 {
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
    ok_num(prev as f64)
}

fn accrint_total(
    issue: f64,
    first: f64,
    settlement: f64,
    coupon: f64,
    par: f64,
    freq: i64,
    basis: u8,
    from_issue: bool,
    date1904: bool,
) -> f64 {
    let months = 12 / freq;
    let s = settlement.trunc() as i64;
    let (fy, fm, fd) = serial_to_civil(first, date1904);
    let (ps_y, ps_m, ps_d) = if from_issue {
        serial_to_civil(issue, date1904)
    } else {
        (fy, fm, fd)
    };
    let (mut pe_y, mut pe_m, mut pe_d) = (fy, fm, fd);
    let mut ps = civil_to_serial(ps_y, ps_m, ps_d, date1904);
    let mut pe = civil_to_serial(fy, fm, fd, date1904);
    let mut acc = 0.0;
    let mut guard = 0;
    loop {
        if ps >= s {
            break;
        }
        let eff_end = pe.min(s);
        acc += par * coupon * year_fraction(ps as f64, eff_end as f64, basis, date1904);
        let (ny, nm, nd) = shift_months_fwd(pe_y, pe_m, pe_d, months, date1904);
        ps = pe;
        pe = civil_to_serial(ny, nm, nd, date1904);
        pe_y = ny;
        pe_m = nm;
        pe_d = nd;
        guard += 1;
        if guard > 10000 {
            break;
        }
    }
    acc
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
    if coupon <= 0.0 || par <= 0.0 || (freq != 1.0 && freq != 2.0 && freq != 4.0 && freq != 12.0) {
        return Err(CalcError::Num);
    }
    let i = issue.trunc();
    let f = first.trunc();
    let s = settlement.trunc();
    if i >= f || i >= s {
        return Err(CalcError::Num);
    }
    let total = accrint_total(
        issue,
        first,
        settlement,
        coupon,
        par,
        freq as i64,
        basis,
        calc_method != 0.0,
        ctx.date1904,
    );
    ok_num(total)
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
    let dsm = day_count(settlement, maturity, 2, ctx.date1904);
    if dsm > 360.0 {
        return Err(CalcError::Num);
    }
    let denom = 360.0 - discount * dsm;
    if denom == 0.0 {
        return Err(CalcError::Div0);
    }
    ok_num((365.0 * discount) / denom)
}

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
    let (next, prev, n) = coupon_period(settlement, maturity, freq, date1904)?;
    let a = day_count(prev as f64, settlement, basis, date1904);
    let e = day_count(prev as f64, next as f64, basis, date1904);
    let dsc = day_count(settlement, next as f64, basis, date1904);
    if e == 0.0 {
        return Err(CalcError::Num);
    }
    let r = ann_yield / freq as f64;
    let c = 100.0 * coupon / freq as f64;
    let base = dsc / e;
    let mut price = -c * a / e;
    let mut num = 0.0;
    let mut t = base;
    for _ in 0..n {
        let pv = c / (1.0 + r).powf(t);
        price += pv;
        num += t * pv;
        t += 1.0;
    }
    let pv_red = 100.0 / (1.0 + r).powf(t - 1.0);
    num += (t - 1.0) * pv_red;
    price += pv_red;
    if price == 0.0 || !price.is_finite() {
        return Err(CalcError::Num);
    }
    Ok(num / price / freq as f64)
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turbo::calc::testkit::Grid;

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
}
