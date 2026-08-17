// functions/datetime.rs — the date/time function family. Owned exclusively by
// the datetime family agent; no other agent edits this file.
//
// Registry contract: implement `register` below and keep this exact signature.
// Do NOT edit functions/mod.rs — the `mod datetime;` declaration and the
// `datetime::register(&mut r)` call site in `build()` are already final.
// See functions/mod.rs for the worked ABS template.
//
// Serial-date arithmetic is implemented directly on top of a proleptic
// Gregorian day count (Hinnant's civil_from_days / days_from_civil), with no
// date crate dependency. The 1900 system reproduces the Excel leap-year bug:
// serial 60 is the non-existent 1900-02-29, and every serial above 60 is one
// day ahead of the true proleptic date. The 1904 system has no such bug: day 0
// is 1904-01-01.
use super::{FuncArg, FuncCtx, FuncSpec, Registry};
use crate::turbo::calc::coerce::{coerce_number, coerce_text};
use crate::turbo::calc::value::{CalcError, CalcValue};
use std::time::{SystemTime, UNIX_EPOCH};

/// Days from 1899-12-31 (1900-system epoch day) to 1970-01-01, in the real
/// proleptic Gregorian calendar (25568 real days; Excel's serial 25569 for
/// 1970-01-01 includes the phantom leap day).
const DAYS_1899_TO_1970: i64 = 25568;
/// Days from 1899-12-31 to 1904-01-01 (1904-system serial 0).
const DAYS_1899_TO_1904: i64 = 1461;
/// Largest valid 1900-system serial (9999-12-31).
const SERIAL_1900_MAX: i64 = 2958465;
/// Largest valid 1904-system serial (9999-12-31).
const SERIAL_1904_MAX: i64 = 2957003;

fn ok_num(n: f64) -> Result<CalcValue, CalcError> {
    if n.is_finite() {
        Ok(CalcValue::Number(n))
    } else {
        Err(CalcError::Num)
    }
}

/// Bound a raw serial to a range wide enough for any real Excel date but too
/// small to overflow the i64 civil arithmetic (serial 1e7 is year ~27390).
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

/// Days since 1970-01-01 for a civil date (proleptic Gregorian; Hinnant).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Civil date for a day count since 1970-01-01 (inverse of the above).
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

/// Civil date for a date-part serial in the given date system. The 1900
/// system maps the phantom serial 60 to the non-existent 1900-02-29.
fn serial_to_civil(serial: f64, date1904: bool) -> (i64, i64, i64) {
    let s = clamp_serial(serial);
    if date1904 {
        civil_from_days(s + DAYS_1899_TO_1904 - DAYS_1899_TO_1970)
    } else if s == 0 {
        // Excel shows serial 0 as "January 0, 1900" — a placeholder day, not
        // 1899-12-31. This is reachable in real files: a time-only cell has an
        // integer part of 0, so DAY() on it must give 0, not 31.
        (1900, 1, 0)
    } else if s == 60 {
        (1900, 2, 29)
    } else {
        let real_day = if s <= 59 { s } else { s - 1 };
        civil_from_days(real_day - DAYS_1899_TO_1970)
    }
}

/// Serial for a civil date in the given date system. In the 1900 system the
/// phantom 1900-02-29 is serial 60 and every real day on/after 1900-03-01 is
/// one ahead of the true proleptic day count.
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

/// Leap-year test in the workbook's calendar: the 1900 system (with its
/// phantom Feb 29) treats 1900 as a leap year; the 1904 system does not.
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

/// Split the time-of-day of a serial into (hour, minute, second). Computed
/// from the full serial's total seconds rather than from the fractional part
/// after `floor`, because subtracting a large integer floor from a big serial
/// drops the low-order bits of the fraction and can push a minute like 45 to
/// 44.99999... (Excel reads the same double and gets 45). Hours wrap the day,
/// so HOUR(1.5) == HOUR(0.5) == 12. Seconds are rounded, matching Excel (so
/// SECOND(0.99) is 36, not 35).
fn time_parts(serial: f64) -> (i64, i64, i64) {
    let total = serial * 86400.0;
    let h = (total / 3600.0).floor() % 24.0;
    let m = (total / 60.0).floor() % 60.0;
    let s = (total - (total / 60.0).floor() * 60.0).round();
    (h as i64, m as i64, s as i64)
}

/// Serial of "now" (UTC, from SystemTime) in the given date system.
fn now_serial(date1904: bool) -> Result<f64, CalcError> {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CalcError::Num)?;
    let days = dur.as_secs_f64() / 86400.0;
    let serial = if date1904 {
        days + 24107.0
    } else {
        days + 25569.0
    };
    if serial.is_finite() {
        Ok(serial)
    } else {
        Err(CalcError::Num)
    }
}

// ---------------------------------------------------------------------------
// Date/time text parsing and date-argument coercion
// ---------------------------------------------------------------------------

/// Read a 1-or-2 digit run of decimal digits at `b[i]`, advancing `i`. `None`
/// when there are no digits or more than two.
fn read_digits_12(b: &[u8], i: &mut usize) -> Option<i64> {
    let start = *i;
    let mut v = 0i64;
    while *i < b.len() && b[*i].is_ascii_digit() {
        v = v * 10 + (b[*i] - b'0') as i64;
        *i += 1;
    }
    let n = *i - start;
    if n == 0 || n > 2 { None } else { Some(v) }
}

/// Parse a `YYYY-MM-DD` or `YYYY/M/D` date prefix (1-2 digit month/day), the
/// 4-digit year first making the format unambiguous. Returns the validated
/// civil date and the byte index just past the day. The separator must match
/// within the date; locale forms without a leading year are not guessed.
fn parse_date_prefix(b: &[u8], date1904: bool) -> Option<(i64, i64, i64, usize)> {
    let mut i = 0;
    let mut year = 0i64;
    for _ in 0..4 {
        if i >= b.len() || !b[i].is_ascii_digit() {
            return None;
        }
        year = year * 10 + (b[i] - b'0') as i64;
        i += 1;
    }
    if i >= b.len() || (b[i] != b'-' && b[i] != b'/') {
        return None;
    }
    let sep = b[i];
    i += 1;
    let m = read_digits_12(b, &mut i)?;
    if i >= b.len() || b[i] != sep {
        return None;
    }
    i += 1;
    let d = read_digits_12(b, &mut i)?;
    if !(1..=12).contains(&m) || d < 1 || d > days_in_month(year, m, date1904) {
        return None;
    }
    Some((year, m, d, i))
}

/// Parse a time `H[:MM[:SS]]` with an optional `AM`/`PM` suffix, starting at
/// `b[i]` and consuming through the end of the trimmed string. Returns the
/// fraction of a day. A bare hour with no minutes is refused unless it carries
/// an AM/PM marker (Excel refuses `"8"` but accepts `"8 AM"`).
fn parse_time_at(b: &[u8], mut i: usize) -> Option<f64> {
    let mut h: i64 = 0;
    let mut hd = 0;
    while i < b.len() && b[i].is_ascii_digit() && hd < 2 {
        h = h * 10 + (b[i] - b'0') as i64;
        hd += 1;
        i += 1;
    }
    if hd == 0 {
        return None;
    }
    let mut m: i64 = 0;
    let mut sec: i64 = 0;
    let mut has_min = false;
    let mut has_sec = false;
    if i < b.len() && b[i] == b':' {
        has_min = true;
        i += 1;
        let mut md = 0;
        while i < b.len() && b[i].is_ascii_digit() && md < 2 {
            m = m * 10 + (b[i] - b'0') as i64;
            md += 1;
            i += 1;
        }
        if md != 2 {
            return None;
        }
        if i < b.len() && b[i] == b':' {
            has_sec = true;
            i += 1;
            let mut sd = 0;
            while i < b.len() && b[i].is_ascii_digit() && sd < 2 {
                sec = sec * 10 + (b[i] - b'0') as i64;
                sd += 1;
                i += 1;
            }
            if sd != 2 {
                return None;
            }
        }
    }
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    let mut pm = false;
    let mut has_ampm = false;
    match std::str::from_utf8(&b[i..])
        .ok()?
        .trim()
        .to_ascii_uppercase()
        .as_str()
    {
        "AM" => has_ampm = true,
        "PM" => {
            has_ampm = true;
            pm = true;
        }
        "" => {}
        _ => return None,
    }
    if m > 59 || sec > 59 {
        return None;
    }
    let hour: i64 = if has_ampm {
        if !(1..=12).contains(&h) {
            return None;
        }
        (h % 12) + if pm { 12 } else { 0 }
    } else {
        if !has_min && !has_sec {
            return None;
        }
        if h > 24 {
            return None;
        }
        if h == 24 && (m != 0 || sec != 0) {
            return None;
        }
        h % 24
    };
    Some(hour as f64 / 24.0 + m as f64 / 1440.0 + sec as f64 / 86400.0)
}

/// Parse a date/time string into `(date_serial, time_fraction)`. Accepted:
/// `YYYY-MM-DD` or `YYYY/M/D` (1-2 digit month/day), optionally followed by
/// whitespace or `T` and a time; or a bare time string (date serial 0).
/// Nothing else is guessed — locale `M/D/Y` forms are refused.
fn parse_date_time(s: &str, date1904: bool) -> Option<(f64, f64)> {
    let b = s.trim().as_bytes();
    if b.is_empty() {
        return None;
    }
    if let Some((y, m, d, mut i)) = parse_date_prefix(b, date1904) {
        if i < b.len() {
            if b[i] == b'T' {
                i += 1;
            } else if b[i].is_ascii_whitespace() {
                while i < b.len() && b[i].is_ascii_whitespace() {
                    i += 1;
                }
            } else {
                return None;
            }
        }
        let frac = if i < b.len() {
            parse_time_at(b, i)?
        } else {
            0.0
        };
        Some((civil_to_serial(y, m, d, date1904) as f64, frac))
    } else {
        Some((0.0, parse_time_at(b, 0)?))
    }
}

/// Coerce a date argument to a serial: numbers pass through, text is parsed as
/// a date string (ISO/slash with optional time) and falls back to plain
/// numeric coercion, blanks become 0, errors propagate.
fn coerce_date_serial(ctx: &FuncCtx, arg: &FuncArg) -> Result<f64, CalcError> {
    let v = arg.value(ctx)?;
    match &v {
        CalcValue::Text(t) => match parse_date_time(t, ctx.date1904) {
            Some((d, f)) => Ok(d + f),
            None => coerce_number(&v),
        },
        _ => coerce_number(&v),
    }
}

/// Like [`coerce_date_serial`], but Excel rejects negative serials in every
/// date-part function (`#NUM!`). Serial 0 stays valid (1900's "January 0").
fn date_serial(ctx: &FuncCtx, arg: &FuncArg) -> Result<f64, CalcError> {
    let s = coerce_date_serial(ctx, arg)?;
    if s < 0.0 { Err(CalcError::Num) } else { Ok(s) }
}

// ---------------------------------------------------------------------------
// Function implementations
// ---------------------------------------------------------------------------

fn date(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let y = coerce_number(&args[0].value(ctx)?)?;
    let m = coerce_number(&args[1].value(ctx)?)?;
    let d = coerce_number(&args[2].value(ctx)?)?;
    let y = y.trunc();
    let m = m.trunc();
    let d = d.trunc();
    let mut year = y;
    if (0.0..=1899.0).contains(&year) {
        year += 1900.0;
    }
    if !(0.0..=9999.0).contains(&year) {
        return Err(CalcError::Num);
    }
    let total = year * 12.0 + (m - 1.0);
    let ny = (total / 12.0).floor();
    let nm = total.rem_euclid(12.0) + 1.0;
    if !(0.0..=9999.0).contains(&ny) {
        return Err(CalcError::Num);
    }
    let base = civil_to_serial(ny as i64, nm as i64, 1, ctx.date1904) as f64;
    let serial = base + (d - 1.0);
    let max = if ctx.date1904 {
        SERIAL_1904_MAX
    } else {
        SERIAL_1900_MAX
    } as f64;
    if serial < 0.0 || serial > max {
        return Err(CalcError::Num);
    }
    ok_num(serial)
}

fn today(ctx: &FuncCtx, _args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    ok_num(now_serial(ctx.date1904)?.floor())
}

fn now(ctx: &FuncCtx, _args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    ok_num(now_serial(ctx.date1904)?)
}

fn year(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let s = date_serial(ctx, &args[0])?;
    let (y, _, _) = serial_to_civil(s, ctx.date1904);
    ok_num(y as f64)
}

fn month(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let s = date_serial(ctx, &args[0])?;
    let (_, m, _) = serial_to_civil(s, ctx.date1904);
    ok_num(m as f64)
}

fn day(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let s = date_serial(ctx, &args[0])?;
    let (_, _, d) = serial_to_civil(s, ctx.date1904);
    ok_num(d as f64)
}

fn hour(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let s = date_serial(ctx, &args[0])?;
    let (h, _, _) = time_parts(s);
    ok_num(h as f64)
}

fn minute(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let s = date_serial(ctx, &args[0])?;
    let (_, m, _) = time_parts(s);
    ok_num(m as f64)
}

fn second(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let s = date_serial(ctx, &args[0])?;
    let (_, _, sec) = time_parts(s);
    ok_num(sec as f64)
}

fn time(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let h = coerce_number(&args[0].value(ctx)?)?;
    let m = coerce_number(&args[1].value(ctx)?)?;
    let s = coerce_number(&args[2].value(ctx)?)?;
    if h < 0.0 || m < 0.0 || s < 0.0 {
        return Err(CalcError::Num);
    }
    let frac = h.trunc() / 24.0 + m.trunc() / 1440.0 + s.trunc() / 86400.0;
    ok_num(frac.rem_euclid(1.0))
}

fn weekday(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let serial = date_serial(ctx, &args[0])?;
    let rtype = if args.len() > 1 {
        coerce_number(&args[1].value(ctx)?)?
    } else {
        1.0
    };
    // Day-of-week from the serial count itself. In the 1900 system this
    // reproduces Excel's weekday shift for dates after the phantom 1900-02-29
    // (real weekdays are one day behind the serial-based weekday there).
    let s = clamp_serial(serial);
    let idx = if ctx.date1904 {
        (s + 5).rem_euclid(7) // serial 0 = 1904-01-01 = Friday
    } else {
        // Excel anchors the 1900 system on serial 1 = Sunday. Checked against
        // three independent dates: 1900-03-01 (serial 61) really was a
        // Thursday and 2024-01-01 (serial 45292) a Monday, both of which this
        // reproduces; serial 1 comes out Sunday even though the real
        // 1900-01-01 was a Monday, which is the phantom-day artifact Excel
        // itself carries for serials below 61.
        (s - 1).rem_euclid(7)
    };
    let out = match rtype.trunc() as i64 {
        1 => idx + 1,
        2 => (idx + 6).rem_euclid(7) + 1,
        3 => (idx + 6).rem_euclid(7),
        _ => return Err(CalcError::Num),
    };
    ok_num(out as f64)
}

/// Shift a (y, m) by whole months, rolling the year, using f64 to avoid
/// overflow on absurd month counts. Returns the target (year, month).
fn shift_months(y: i64, m: i64, months: f64) -> (f64, f64) {
    let total = y as f64 * 12.0 + (m as f64 - 1.0) + months;
    let ny = (total / 12.0).floor();
    let nm = total.rem_euclid(12.0) + 1.0;
    (ny, nm)
}

fn edate(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let start = date_serial(ctx, &args[0])?;
    let months = coerce_number(&args[1].value(ctx)?)?;
    if !(-1e7..=1e7).contains(&months) {
        return Err(CalcError::Num);
    }
    let (y, m, d) = serial_to_civil(start, ctx.date1904);
    let (ny, nm) = shift_months(y, m, months.trunc());
    if !(0.0..=9999.0).contains(&ny) {
        return Err(CalcError::Num);
    }
    let ny = ny as i64;
    let nm = nm as i64;
    let nd = d.min(days_in_month(ny, nm, ctx.date1904));
    let serial = civil_to_serial(ny, nm, nd, ctx.date1904);
    let max = if ctx.date1904 {
        SERIAL_1904_MAX
    } else {
        SERIAL_1900_MAX
    };
    if serial < 0 || serial > max {
        return Err(CalcError::Num);
    }
    ok_num(serial as f64)
}

fn eomonth(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let start = date_serial(ctx, &args[0])?;
    let months = coerce_number(&args[1].value(ctx)?)?;
    if !(-1e7..=1e7).contains(&months) {
        return Err(CalcError::Num);
    }
    let (y, m, _d) = serial_to_civil(start, ctx.date1904);
    let (ny, nm) = shift_months(y, m, months.trunc());
    if !(0.0..=9999.0).contains(&ny) {
        return Err(CalcError::Num);
    }
    let ny = ny as i64;
    let nm = nm as i64;
    let nd = days_in_month(ny, nm, ctx.date1904);
    let serial = civil_to_serial(ny, nm, nd, ctx.date1904);
    let max = if ctx.date1904 {
        SERIAL_1904_MAX
    } else {
        SERIAL_1900_MAX
    };
    if serial < 0 || serial > max {
        return Err(CalcError::Num);
    }
    ok_num(serial as f64)
}

fn datedif(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let start = date_serial(ctx, &args[0])?;
    let end = date_serial(ctx, &args[1])?;
    let unit = coerce_text(&args[2].value(ctx)?)?;
    let s = start.trunc();
    let e = end.trunc();
    if s > e {
        return Err(CalcError::Num);
    }
    let (y1, m1, d1) = serial_to_civil(s, ctx.date1904);
    let (y2, m2, d2) = serial_to_civil(e, ctx.date1904);
    let unit = unit.trim().to_ascii_uppercase();
    let result: f64 = match unit.as_str() {
        "D" => e - s,
        "M" => {
            let mut m = (y2 - y1) * 12 + (m2 - m1);
            if d2 < d1 {
                m -= 1;
            }
            m as f64
        }
        "Y" => {
            let mut y = y2 - y1;
            if m2 * 100 + d2 < m1 * 100 + d1 {
                y -= 1;
            }
            y as f64
        }
        "YM" => {
            let mut m = m2 - m1;
            if d2 < d1 {
                m -= 1;
            }
            if m < 0 {
                m += 12;
            }
            m as f64
        }
        "MD" => {
            let mut days = d2 - d1;
            if days < 0 {
                let (bm, by) = if m2 == 1 { (12, y2 - 1) } else { (m2 - 1, y2) };
                days += days_in_month(by, bm, ctx.date1904);
            }
            days as f64
        }
        "YD" => {
            let mut y = y2 - y1;
            if m2 * 100 + d2 < m1 * 100 + d1 {
                y -= 1;
            }
            let ay = y1 + y;
            let a_base = civil_to_serial(ay, m1, 1, ctx.date1904);
            let a_serial = a_base + (d1 - 1);
            (e as i64 - a_serial) as f64
        }
        _ => return Err(CalcError::Num),
    };
    ok_num(result)
}

fn datevalue(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let text = coerce_text(&args[0].value(ctx)?)?;
    let Some((date, _)) = parse_date_time(&text, ctx.date1904) else {
        return Err(CalcError::Value);
    };
    let max = if ctx.date1904 {
        SERIAL_1904_MAX
    } else {
        SERIAL_1900_MAX
    } as f64;
    if date < 0.0 {
        Err(CalcError::Value)
    } else if date > max {
        Err(CalcError::Num)
    } else {
        ok_num(date)
    }
}

fn days(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let end = coerce_date_serial(ctx, &args[0])?.trunc();
    let start = coerce_date_serial(ctx, &args[1])?.trunc();
    ok_num(end - start)
}

// ---------------------------------------------------------------------------
// Working-day arithmetic (NETWORKDAYS / WORKDAY and their .INTL variants)
// ---------------------------------------------------------------------------

/// Sunday-based day-of-week (0 = Sunday), from the serial count. In the 1900
/// system this reproduces Excel's own weekday shift for serials below 61.
fn dow_sun0(s: i64, date1904: bool) -> i64 {
    if date1904 {
        (s + 5).rem_euclid(7)
    } else {
        (s - 1).rem_euclid(7)
    }
}

/// Monday-based day-of-week (0 = Monday .. 6 = Sunday).
fn dow_mon0(s: i64, date1904: bool) -> i64 {
    (dow_sun0(s, date1904) + 6).rem_euclid(7)
}

/// The default weekend (Saturday, Sunday) as a Monday-based mask.
const DEFAULT_WEEKEND: [bool; 7] = [false, false, false, false, false, true, true];

/// Monday-based weekend mask for the numeric `weekend` patterns 1-17.
fn weekend_mask_from_num(n: i64) -> Option<[bool; 7]> {
    let mask = match n {
        1 => [false, false, false, false, false, true, true],
        2 => [true, false, false, false, false, false, true],
        3 => [true, true, false, false, false, false, false],
        4 => [false, true, true, false, false, false, false],
        5 => [false, false, true, true, false, false, false],
        6 => [false, false, false, true, true, false, false],
        7 => [false, false, false, false, true, true, false],
        8 => [false, false, false, false, false, false, true],
        9 => [true, false, false, false, false, false, false],
        10 => [false, true, false, false, false, false, false],
        11 => [false, false, true, false, false, false, false],
        12 => [false, false, false, true, false, false, false],
        13 => [false, false, false, false, true, false, false],
        14 => [false, false, false, false, false, true, false],
        15 => [false, false, true, false, false, false, true],
        16 => [false, true, false, false, false, false, true],
        17 => [true, false, true, false, false, false, false],
        _ => return None,
    };
    Some(mask)
}

/// Parse a 7-character `0`/`1` weekend string, first char = Monday. An all-1s
/// string (every day a weekend) is #VALUE! in Excel.
fn weekend_mask_from_str(s: &str) -> Result<[bool; 7], CalcError> {
    let b = s.as_bytes();
    if b.len() != 7 {
        return Err(CalcError::Value);
    }
    let mut mask = [false; 7];
    let mut all_weekend = true;
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'0' => all_weekend = false,
            b'1' => mask[i] = true,
            _ => return Err(CalcError::Value),
        }
    }
    if all_weekend {
        Err(CalcError::Value)
    } else {
        Ok(mask)
    }
}

/// Resolve the optional `.INTL` `weekend` argument: a number 1-17, a
/// 7-character `0`/`1` string, or the default Saturday/Sunday when omitted.
fn weekend_mask_arg(ctx: &FuncCtx, arg: Option<&FuncArg>) -> Result<[bool; 7], CalcError> {
    let Some(arg) = arg else {
        return Ok(DEFAULT_WEEKEND);
    };
    match arg.value(ctx)? {
        CalcValue::Number(n) => {
            let t = n.trunc();
            if !(1.0..=17.0).contains(&t) {
                return Err(CalcError::Value);
            }
            weekend_mask_from_num(t as i64).ok_or(CalcError::Value)
        }
        CalcValue::Text(t) => weekend_mask_from_str(&t),
        CalcValue::Error(e) => Err(e),
        _ => Err(CalcError::Value),
    }
}

/// Collect the optional `holidays` range into a sorted, deduplicated serial
/// list. Blank cells are ignored; text or boolean holidays are #VALUE!.
fn collect_holidays(ctx: &FuncCtx, arg: Option<&FuncArg>) -> Result<Vec<i64>, CalcError> {
    let Some(arg) = arg else {
        return Ok(Vec::new());
    };
    let mut out: Vec<i64> = Vec::new();
    match arg.value(ctx)? {
        CalcValue::Array(a) => {
            for v in a.iter() {
                match v {
                    CalcValue::Number(n) => out.push(clamp_serial(*n)),
                    CalcValue::Blank => {}
                    CalcValue::Error(e) => return Err(*e),
                    _ => return Err(CalcError::Value),
                }
            }
        }
        CalcValue::Number(n) => out.push(clamp_serial(n)),
        CalcValue::Blank => {}
        CalcValue::Error(e) => return Err(e),
        _ => return Err(CalcError::Value),
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// Number of non-weekend days in `[lo, hi]` (inclusive), counted per weekday
/// arithmetically rather than walking the range, so the cost is O(1).
fn count_workdays(lo: i64, hi: i64, mask: &[bool; 7], date1904: bool) -> i64 {
    if hi < lo {
        return 0;
    }
    let start = dow_mon0(lo, date1904);
    let mut count = 0;
    for (i, &is_weekend) in mask.iter().enumerate() {
        if is_weekend {
            continue;
        }
        let first = lo + (i as i64 - start).rem_euclid(7);
        if first <= hi {
            count += (hi - first) / 7 + 1;
        }
    }
    count
}

/// NETWORKDAYS-style count over `[start, end]`, inclusive of both endpoints,
/// negative when `start` is later than `end`. A holiday on a weekend is not
/// double-counted because it is excluded as a weekend first.
fn networkdays_count(
    start: i64,
    end: i64,
    holidays: &[i64],
    mask: &[bool; 7],
    date1904: bool,
) -> i64 {
    let sign = if start <= end { 1 } else { -1 };
    let (lo, hi) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    let mut count = count_workdays(lo, hi, mask, date1904);
    for &h in holidays {
        if lo <= h && h <= hi && !mask[dow_mon0(h, date1904) as usize] {
            count -= 1;
        }
    }
    count * sign
}

/// The `k`-th workday strictly after `anchor` (k = 0 returns `anchor`).
fn nth_workday_after(anchor: i64, k: i64, mask: &[bool; 7], date1904: bool) -> i64 {
    if k <= 0 {
        return anchor;
    }
    let wpc = (7 - mask.iter().filter(|&&b| b).count()) as i64;
    let mut d = anchor + (k / wpc) * 7;
    let rem = k % wpc;
    let mut added = 0;
    while added < rem {
        d += 1;
        if !mask[dow_mon0(d, date1904) as usize] {
            added += 1;
        }
    }
    d
}

/// The `k`-th workday strictly before `anchor` (k = 0 returns `anchor`).
fn nth_workday_before(anchor: i64, k: i64, mask: &[bool; 7], date1904: bool) -> i64 {
    if k <= 0 {
        return anchor;
    }
    let wpc = (7 - mask.iter().filter(|&&b| b).count()) as i64;
    let mut d = anchor - (k / wpc) * 7;
    let rem = k % wpc;
    let mut added = 0;
    while added < rem {
        d -= 1;
        if !mask[dow_mon0(d, date1904) as usize] {
            added += 1;
        }
    }
    d
}

/// Smallest/minimal valid 1900/1904 serials and the 9999-12-31 maximum.
fn serial_bounds(date1904: bool) -> (i64, i64) {
    if date1904 {
        (0, SERIAL_1904_MAX)
    } else {
        (1, SERIAL_1900_MAX)
    }
}

/// WORKDAY core: the workday `days` working days before/after `start`, skipping
/// `holidays`. A holiday on a weekend is not double-counted. `#NUM!` when the
/// result leaves the workbook's date range.
fn workday_add(
    start: i64,
    days: i64,
    holidays: &[i64],
    mask: &[bool; 7],
    date1904: bool,
    min_serial: i64,
    max_serial: i64,
) -> Result<i64, CalcError> {
    if days == 0 {
        return Ok(start);
    }
    let forward = days > 0;
    let k = days.abs();
    let mut d = if forward {
        nth_workday_after(start, k, mask, date1904)
    } else {
        nth_workday_before(start, k, mask, date1904)
    };
    // Holidays on workdays inside the traversed span each reduce the count by
    // one, so push the result across them and re-check until it is exact.
    loop {
        let g = if forward {
            count_workdays(start + 1, d, mask, date1904)
        } else {
            count_workdays(d, start - 1, mask, date1904)
        };
        let mut f = 0;
        for &h in holidays {
            if !mask[dow_mon0(h, date1904) as usize]
                && if forward {
                    start < h && h <= d
                } else {
                    d <= h && h < start
                }
            {
                f += 1;
            }
        }
        if g - f == k {
            break;
        }
        let short = k - (g - f);
        d = if forward {
            nth_workday_after(d, short, mask, date1904)
        } else {
            nth_workday_before(d, short, mask, date1904)
        };
    }
    if d < min_serial || d > max_serial {
        return Err(CalcError::Num);
    }
    Ok(d)
}

fn networkdays(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let start = date_serial(ctx, &args[0])?;
    let end = date_serial(ctx, &args[1])?;
    let holidays = collect_holidays(ctx, args.get(2))?;
    let count = networkdays_count(
        clamp_serial(start),
        clamp_serial(end),
        &holidays,
        &DEFAULT_WEEKEND,
        ctx.date1904,
    );
    ok_num(count as f64)
}

fn networkdays_intl(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let start = date_serial(ctx, &args[0])?;
    let end = date_serial(ctx, &args[1])?;
    let mask = weekend_mask_arg(ctx, args.get(2))?;
    let holidays = collect_holidays(ctx, args.get(3))?;
    let count = networkdays_count(
        clamp_serial(start),
        clamp_serial(end),
        &holidays,
        &mask,
        ctx.date1904,
    );
    ok_num(count as f64)
}

fn workday(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let start = date_serial(ctx, &args[0])?;
    let ndays = coerce_number(&args[1].value(ctx)?)?;
    let holidays = collect_holidays(ctx, args.get(2))?;
    let ndays = ndays.trunc();
    if ndays.abs() > 1e9 {
        return Err(CalcError::Num);
    }
    let (min_s, max_s) = serial_bounds(ctx.date1904);
    let result = workday_add(
        clamp_serial(start),
        ndays as i64,
        &holidays,
        &DEFAULT_WEEKEND,
        ctx.date1904,
        min_s,
        max_s,
    )?;
    ok_num(result as f64)
}

fn workday_intl(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let start = date_serial(ctx, &args[0])?;
    let ndays = coerce_number(&args[1].value(ctx)?)?;
    let mask = weekend_mask_arg(ctx, args.get(2))?;
    let holidays = collect_holidays(ctx, args.get(3))?;
    let ndays = ndays.trunc();
    if ndays.abs() > 1e9 {
        return Err(CalcError::Num);
    }
    let (min_s, max_s) = serial_bounds(ctx.date1904);
    let result = workday_add(
        clamp_serial(start),
        ndays as i64,
        &holidays,
        &mask,
        ctx.date1904,
        min_s,
        max_s,
    )?;
    ok_num(result as f64)
}

// ---------------------------------------------------------------------------
// Day-count bases for YEARFRAC and DAYS360. This is an independent copy of the
// financial family's helpers (the two coexist so a later pass can unify them).
// ---------------------------------------------------------------------------

fn last_day_of_feb(y: i64, m: i64, d: i64, date1904: bool) -> bool {
    m == 2 && d == days_in_month(y, 2, date1904)
}

/// Basis 0: US (NASD) 30/360, including the end-of-February rules.
fn days_360_us(s1: f64, s2: f64, date1904: bool) -> i64 {
    let (mut y1, mut m1, mut d1) = serial_to_civil(s1, date1904);
    let (mut y2, mut m2, mut d2) = serial_to_civil(s2, date1904);
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

/// Basis 1 days-in-year, per the published Excel actual/actual algorithm.
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

/// YEARFRAC fraction over `s1 <= s2`.
fn year_fraction_core(s1: f64, s2: f64, basis: u8, date1904: bool) -> f64 {
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

/// DAYS360 day count in the given argument order (the sign is preserved).
fn days360_core(s1: i64, s2: i64, european: bool, date1904: bool) -> i64 {
    let (y1, m1, d1) = serial_to_civil(s1 as f64, date1904);
    let (y2, m2, d2) = serial_to_civil(s2 as f64, date1904);
    let (mut d1, mut d2, mut y2, mut m2) = (d1, d2, y2, m2);
    if european {
        if d1 == 31 {
            d1 = 30;
        }
        if d2 == 31 {
            d2 = 30;
        }
    } else {
        if d1 == days_in_month(y1, m1, date1904) {
            d1 = 30;
        }
        if d2 == days_in_month(y2, m2, date1904) {
            if d1 < 30 {
                m2 += 1;
                if m2 == 13 {
                    m2 = 1;
                    y2 += 1;
                }
                d2 = 1;
            } else {
                d2 = 30;
            }
        }
    }
    (y2 * 360 + m2 * 30 + d2) - (y1 * 360 + m1 * 30 + d1)
}

fn yearfrac(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let start = date_serial(ctx, &args[0])?;
    let end = date_serial(ctx, &args[1])?;
    let basis = if args.len() > 2 {
        coerce_number(&args[2].value(ctx)?)?.trunc()
    } else {
        0.0
    };
    if !(0.0..=4.0).contains(&basis) {
        return Err(CalcError::Num);
    }
    // Excel always reports the positive fraction regardless of argument order.
    let (s1, s2) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    ok_num(year_fraction_core(s1, s2, basis as u8, ctx.date1904))
}

fn days360(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let start = date_serial(ctx, &args[0])?;
    let end = date_serial(ctx, &args[1])?;
    let european = if args.len() > 2 {
        match args[2].value(ctx)? {
            CalcValue::Bool(b) => b,
            CalcValue::Number(n) => n != 0.0,
            CalcValue::Text(t) => match t.trim().to_ascii_uppercase().as_str() {
                "TRUE" => true,
                "FALSE" => false,
                _ => return Err(CalcError::Value),
            },
            CalcValue::Error(e) => return Err(e),
            _ => return Err(CalcError::Value),
        }
    } else {
        false
    };
    let d = days360_core(
        clamp_serial(start),
        clamp_serial(end),
        european,
        ctx.date1904,
    );
    ok_num(d as f64)
}

// ---------------------------------------------------------------------------
// Week numbering (WEEKNUM / ISOWEEKNUM)
// ---------------------------------------------------------------------------

/// Week number for the "week 1 contains January 1" schemes. `start` is the
/// Sunday-based weekday the week begins on (0 = Sunday).
fn weeknum_jan1(s: i64, start: i64, date1904: bool) -> i64 {
    let (y, _, _) = serial_to_civil(s as f64, date1904);
    let jan1 = civil_to_serial(y, 1, 1, date1904);
    let offset = (dow_sun0(jan1, date1904) - start).rem_euclid(7);
    ((s - jan1 + offset) / 7) + 1
}

/// ISO 8601 week number. Week 1 contains the first Thursday, so early January
/// can belong to week 52 or 53 of the previous year.
fn isoweek_number(s: i64, date1904: bool) -> i64 {
    let wd = dow_mon0(s, date1904); // 0 = Monday
    let thursday = s + (3 - wd); // the Thursday of the current Mon-Sun week
    let (ty, _, _) = serial_to_civil(thursday as f64, date1904);
    let jan1 = civil_to_serial(ty, 1, 1, date1904);
    let first_thursday = jan1 + (3 - dow_mon0(jan1, date1904)).rem_euclid(7);
    ((thursday - first_thursday) / 7) + 1
}

fn weeknum(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let serial = date_serial(ctx, &args[0])?;
    let rtype = if args.len() > 1 {
        coerce_number(&args[1].value(ctx)?)?.trunc()
    } else {
        1.0
    };
    let s = clamp_serial(serial);
    let out = match rtype as i64 {
        1 | 17 => weeknum_jan1(s, 0, ctx.date1904),
        2 | 11 => weeknum_jan1(s, 1, ctx.date1904),
        12 => weeknum_jan1(s, 2, ctx.date1904),
        13 => weeknum_jan1(s, 3, ctx.date1904),
        14 => weeknum_jan1(s, 4, ctx.date1904),
        15 => weeknum_jan1(s, 5, ctx.date1904),
        16 => weeknum_jan1(s, 6, ctx.date1904),
        21 => isoweek_number(s, ctx.date1904),
        _ => return Err(CalcError::Num),
    };
    ok_num(out as f64)
}

fn isoweeknum(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let serial = date_serial(ctx, &args[0])?;
    ok_num(isoweek_number(clamp_serial(serial), ctx.date1904) as f64)
}

// ---------------------------------------------------------------------------
// TIMEVALUE
// ---------------------------------------------------------------------------

/// TIMEVALUE returns the time-of-day fraction of a parsed date/time string;
/// a date with no time contributes zero (Excel: `TIMEVALUE("2020-01-02")` is
/// 0). Parsing is shared with the family's DATEVALUE via `parse_date_time`.
fn timevalue(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let text = coerce_text(&args[0].value(ctx)?)?;
    match parse_date_time(&text, ctx.date1904) {
        Some((_, frac)) => ok_num(frac),
        None => Err(CalcError::Value),
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

const NETWORKDAYS: FuncSpec = FuncSpec {
    name: "NETWORKDAYS",
    min_args: 2,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: networkdays,
};

const NETWORKDAYS_INTL: FuncSpec = FuncSpec {
    name: "NETWORKDAYS.INTL",
    min_args: 2,
    max_args: Some(4),
    volatile: false,
    array_aware: false,
    func: networkdays_intl,
};

const WORKDAY: FuncSpec = FuncSpec {
    name: "WORKDAY",
    min_args: 2,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: workday,
};

const WORKDAY_INTL: FuncSpec = FuncSpec {
    name: "WORKDAY.INTL",
    min_args: 2,
    max_args: Some(4),
    volatile: false,
    array_aware: false,
    func: workday_intl,
};

const YEARFRAC: FuncSpec = FuncSpec {
    name: "YEARFRAC",
    min_args: 2,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: yearfrac,
};

const DAYS360: FuncSpec = FuncSpec {
    name: "DAYS360",
    min_args: 2,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: days360,
};

const WEEKNUM: FuncSpec = FuncSpec {
    name: "WEEKNUM",
    min_args: 1,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: weeknum,
};

const ISOWEEKNUM: FuncSpec = FuncSpec {
    name: "ISOWEEKNUM",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: isoweeknum,
};

const TIMEVALUE: FuncSpec = FuncSpec {
    name: "TIMEVALUE",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: timevalue,
};

const DATE: FuncSpec = FuncSpec {
    name: "DATE",
    min_args: 3,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: date,
};

const TODAY: FuncSpec = FuncSpec {
    name: "TODAY",
    min_args: 0,
    max_args: Some(0),
    volatile: true,
    array_aware: false,
    func: today,
};

const NOW: FuncSpec = FuncSpec {
    name: "NOW",
    min_args: 0,
    max_args: Some(0),
    volatile: true,
    array_aware: false,
    func: now,
};

const YEAR: FuncSpec = FuncSpec {
    name: "YEAR",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: year,
};

const MONTH: FuncSpec = FuncSpec {
    name: "MONTH",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: month,
};

const DAY: FuncSpec = FuncSpec {
    name: "DAY",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: day,
};

const HOUR: FuncSpec = FuncSpec {
    name: "HOUR",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: hour,
};

const MINUTE: FuncSpec = FuncSpec {
    name: "MINUTE",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: minute,
};

const SECOND: FuncSpec = FuncSpec {
    name: "SECOND",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: second,
};

const TIME: FuncSpec = FuncSpec {
    name: "TIME",
    min_args: 3,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: time,
};

const WEEKDAY: FuncSpec = FuncSpec {
    name: "WEEKDAY",
    min_args: 1,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: weekday,
};

const EDATE: FuncSpec = FuncSpec {
    name: "EDATE",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: edate,
};

const EOMONTH: FuncSpec = FuncSpec {
    name: "EOMONTH",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: eomonth,
};

const DATEDIF: FuncSpec = FuncSpec {
    name: "DATEDIF",
    min_args: 3,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: datedif,
};

const DATEVALUE: FuncSpec = FuncSpec {
    name: "DATEVALUE",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: datevalue,
};

const DAYS: FuncSpec = FuncSpec {
    name: "DAYS",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: days,
};

pub fn register(r: &mut Registry) {
    r.register(&NETWORKDAYS);
    r.register(&NETWORKDAYS_INTL);
    r.register(&WORKDAY);
    r.register(&WORKDAY_INTL);
    r.register(&YEARFRAC);
    r.register(&DAYS360);
    r.register(&WEEKNUM);
    r.register(&ISOWEEKNUM);
    r.register(&TIMEVALUE);
    r.register(&DATE);
    r.register(&TODAY);
    r.register(&NOW);
    r.register(&YEAR);
    r.register(&MONTH);
    r.register(&DAY);
    r.register(&HOUR);
    r.register(&MINUTE);
    r.register(&SECOND);
    r.register(&TIME);
    r.register(&WEEKDAY);
    r.register(&EDATE);
    r.register(&EOMONTH);
    r.register(&DATEDIF);
    r.register(&DATEVALUE);
    r.register(&DAYS);
}

#[cfg(test)]
#[allow(clippy::assertions_on_constants)]
mod tests {
    use super::*;
    use crate::turbo::calc::functions::CellResolver;
    use crate::turbo::calc::testkit::Grid;
    use crate::turbo::calc::value::ArrayValue;
    use pretty_assertions::assert_eq;

    struct TestResolver;

    impl CellResolver for TestResolver {
        fn cell(&self, _sheet: u32, _row: u32, _col: u32) -> Option<CalcValue> {
            None
        }
        fn sheet_index(&self, _name: &str) -> Option<u32> {
            None
        }
    }

    fn call(spec: &FuncSpec, date1904: bool, args: Vec<CalcValue>) -> Result<CalcValue, CalcError> {
        let resolver = TestResolver;
        let ctx = FuncCtx {
            date1904,
            sheet: 0,
            row: 0,
            col: 0,
            resolver: &resolver,
        };
        let fargs: Vec<FuncArg> = args.into_iter().map(FuncArg::Value).collect();
        (spec.func)(&ctx, &fargs)
    }

    fn n(v: f64) -> CalcValue {
        CalcValue::number(v)
    }

    fn t(v: &str) -> CalcValue {
        CalcValue::text(v)
    }

    fn near(a: CalcValue, b: f64) -> bool {
        match a {
            CalcValue::Number(x) => (x - b).abs() < 1e-9,
            _ => false,
        }
    }

    // --- serial <-> civil boundary conversions -----------------------------

    #[test]
    fn serial_to_civil_1900_boundaries() {
        assert_eq!(serial_to_civil(1.0, false), (1900, 1, 1));
        assert_eq!(serial_to_civil(59.0, false), (1900, 2, 28));
        assert_eq!(serial_to_civil(60.0, false), (1900, 2, 29));
        assert_eq!(serial_to_civil(61.0, false), (1900, 3, 1));
        assert_eq!(serial_to_civil(2958465.0, false), (9999, 12, 31));
    }

    #[test]
    fn serial_to_civil_1904_boundaries() {
        assert_eq!(serial_to_civil(0.0, true), (1904, 1, 1));
        assert_eq!(serial_to_civil(59.0, true), (1904, 2, 29));
        assert_eq!(serial_to_civil(60.0, true), (1904, 3, 1));
        assert_eq!(serial_to_civil(2957003.0, true), (9999, 12, 31));
    }

    #[test]
    fn civil_to_serial_1900_boundaries() {
        assert_eq!(civil_to_serial(1900, 1, 1, false), 1);
        assert_eq!(civil_to_serial(1900, 2, 28, false), 59);
        assert_eq!(civil_to_serial(1900, 2, 29, false), 60);
        assert_eq!(civil_to_serial(1900, 3, 1, false), 61);
        assert_eq!(civil_to_serial(9999, 12, 31, false), 2958465);
    }

    #[test]
    fn civil_to_serial_1904_boundaries() {
        assert_eq!(civil_to_serial(1904, 1, 1, true), 0);
        assert_eq!(civil_to_serial(1904, 2, 29, true), 59);
        assert_eq!(civil_to_serial(1904, 3, 1, true), 60);
        assert_eq!(civil_to_serial(9999, 12, 31, true), 2957003);
    }

    #[test]
    fn conversions_roundtrip() {
        for s in [1.0, 59.0, 60.0, 61.0, 45292.0, 2958465.0] {
            let (y, m, d) = serial_to_civil(s, false);
            assert_eq!(civil_to_serial(y, m, d, false), s as i64);
        }
        for s in [0.0, 59.0, 60.0, 43890.0, 2957003.0] {
            let (y, m, d) = serial_to_civil(s, true);
            assert_eq!(civil_to_serial(y, m, d, true), s as i64);
        }
    }

    // --- DATE ---------------------------------------------------------------

    #[test]
    fn date_basics() {
        assert_eq!(
            call(&DATE, false, vec![n(2024.0), n(3.0), n(1.0)]),
            Ok(n(45352.0))
        );
        assert_eq!(
            call(&DATE, false, vec![n(1900.0), n(1.0), n(1.0)]),
            Ok(n(1.0))
        );
        assert_eq!(
            call(&DATE, false, vec![n(9999.0), n(12.0), n(31.0)]),
            Ok(n(2958465.0))
        );
        assert_eq!(
            call(&DATE, false, vec![n(1900.0), n(2.0), n(29.0)]),
            Ok(n(60.0))
        );
        assert_eq!(
            call(&DATE, false, vec![n(2024.0), n(2.0), n(29.0)]),
            Ok(n(45351.0))
        );
        assert_eq!(
            call(&DATE, false, vec![n(5.0), n(1.0), n(1.0)]),
            Ok(n(1828.0))
        );
    }

    #[test]
    fn date_rolls_over() {
        assert_eq!(
            call(&DATE, false, vec![n(2024.0), n(13.0), n(1.0)]),
            Ok(n(45658.0))
        );
        assert_eq!(
            call(&DATE, false, vec![n(2024.0), n(0.0), n(1.0)]),
            Ok(n(45261.0))
        );
        assert_eq!(
            call(&DATE, false, vec![n(2024.0), n(-1.0), n(1.0)]),
            Ok(n(45231.0))
        );
        assert_eq!(
            call(&DATE, false, vec![n(2024.0), n(1.0), n(0.0)]),
            Ok(n(45291.0))
        );
        assert_eq!(
            call(&DATE, false, vec![n(2024.0), n(1.0), n(-1.0)]),
            Ok(n(45290.0))
        );
        assert_eq!(
            call(&DATE, false, vec![n(1900.0), n(2.0), n(30.0)]),
            Ok(n(61.0))
        );
    }

    #[test]
    fn date_truncates_arguments() {
        assert_eq!(
            call(&DATE, false, vec![n(2024.9), n(3.9), n(1.9)]),
            Ok(n(45352.0))
        );
    }

    #[test]
    fn date_errors() {
        assert_eq!(
            call(&DATE, false, vec![n(10000.0), n(1.0), n(1.0)]),
            Err(CalcError::Num)
        );
        assert_eq!(
            call(&DATE, false, vec![n(-1.0), n(1.0), n(1.0)]),
            Err(CalcError::Num)
        );
        assert_eq!(
            call(&DATE, false, vec![n(9999.0), n(12.0), n(32.0)]),
            Err(CalcError::Num)
        );
        assert_eq!(
            call(&DATE, false, vec![t("abc"), n(1.0), n(1.0)]),
            Err(CalcError::Value)
        );
        assert_eq!(
            call(
                &DATE,
                false,
                vec![n(2024.0), n(1.0), CalcValue::err(CalcError::Div0)]
            ),
            Err(CalcError::Div0)
        );
    }

    #[test]
    fn date_1904_system() {
        assert_eq!(
            call(&DATE, true, vec![n(1904.0), n(1.0), n(1.0)]),
            Ok(n(0.0))
        );
        assert_eq!(
            call(&DATE, true, vec![n(2024.0), n(3.0), n(1.0)]),
            Ok(n(43890.0))
        );
        assert_eq!(
            call(&DATE, true, vec![n(1900.0), n(2.0), n(29.0)]),
            Err(CalcError::Num)
        );
    }

    // --- YEAR / MONTH / DAY ------------------------------------------------

    #[test]
    fn year_month_day() {
        assert_eq!(call(&YEAR, false, vec![n(45352.0)]), Ok(n(2024.0)));
        assert_eq!(call(&MONTH, false, vec![n(45352.0)]), Ok(n(3.0)));
        assert_eq!(call(&DAY, false, vec![n(45352.0)]), Ok(n(1.0)));
        assert_eq!(call(&YEAR, false, vec![n(60.0)]), Ok(n(1900.0)));
        assert_eq!(call(&MONTH, false, vec![n(60.0)]), Ok(n(2.0)));
        assert_eq!(call(&DAY, false, vec![n(60.0)]), Ok(n(29.0)));
        assert_eq!(call(&DAY, false, vec![n(61.0)]), Ok(n(1.0)));
        assert_eq!(call(&YEAR, false, vec![n(45352.75)]), Ok(n(2024.0)));
        assert_eq!(call(&YEAR, false, vec![n(2958465.0)]), Ok(n(9999.0)));
        assert_eq!(call(&MONTH, false, vec![n(2958465.0)]), Ok(n(12.0)));
        assert_eq!(call(&DAY, false, vec![n(2958465.0)]), Ok(n(31.0)));
        assert_eq!(call(&YEAR, true, vec![n(0.0)]), Ok(n(1904.0)));
        assert_eq!(call(&DAY, true, vec![n(0.0)]), Ok(n(1.0)));
        assert_eq!(call(&DAY, true, vec![n(2957003.0)]), Ok(n(31.0)));
    }

    // --- HOUR / MINUTE / SECOND / TIME --------------------------------------

    #[test]
    fn time_parts_read_fraction() {
        assert_eq!(call(&HOUR, false, vec![n(0.5)]), Ok(n(12.0)));
        assert_eq!(call(&MINUTE, false, vec![n(0.5)]), Ok(n(0.0)));
        assert_eq!(call(&SECOND, false, vec![n(0.5)]), Ok(n(0.0)));
        assert_eq!(call(&HOUR, false, vec![n(0.25)]), Ok(n(6.0)));
        assert_eq!(call(&HOUR, false, vec![n(1.5)]), Ok(n(12.0)));
        assert_eq!(call(&MINUTE, false, vec![n(0.99)]), Ok(n(45.0)));
        assert_eq!(call(&SECOND, false, vec![n(0.99)]), Ok(n(36.0)));
        assert_eq!(call(&HOUR, false, vec![n(0.0)]), Ok(n(0.0)));
        assert_eq!(call(&MINUTE, false, vec![n(0.0)]), Ok(n(0.0)));
        assert_eq!(call(&SECOND, false, vec![n(0.0)]), Ok(n(0.0)));
    }

    #[test]
    fn time_builds_fraction() {
        assert_eq!(
            call(&TIME, false, vec![n(12.0), n(0.0), n(0.0)]),
            Ok(n(0.5))
        );
        assert_eq!(
            call(&TIME, false, vec![n(6.0), n(0.0), n(0.0)]),
            Ok(n(0.25))
        );
        assert_eq!(
            call(&TIME, false, vec![n(24.0), n(0.0), n(0.0)]),
            Ok(n(0.0))
        );
        assert_eq!(call(&TIME, false, vec![n(0.0), n(0.0), n(0.0)]), Ok(n(0.0)));
        assert!(near(
            call(&TIME, false, vec![n(25.0), n(0.0), n(0.0)]).unwrap(),
            1.0 / 24.0
        ));
        assert!(near(
            call(&TIME, false, vec![n(1.5), n(0.0), n(0.0)]).unwrap(),
            1.0 / 24.0
        ));
    }

    #[test]
    fn time_wraps_and_reads_back() {
        let serial = call(&TIME, false, vec![n(23.0), n(59.0), n(59.0)]).unwrap();
        assert_eq!(serial.as_number().unwrap() < 1.0, true);
        assert_eq!(call(&HOUR, false, vec![serial.clone()]), Ok(n(23.0)));
        assert_eq!(call(&MINUTE, false, vec![serial.clone()]), Ok(n(59.0)));
        assert_eq!(call(&SECOND, false, vec![serial]), Ok(n(59.0)));
    }

    #[test]
    fn time_rejects_negative_arguments() {
        // Excel-COM measured: any negative TIME argument is #NUM!.
        for args in [
            vec![n(-1.0), n(0.0), n(0.0)],
            vec![n(0.0), n(-1.0), n(0.0)],
            vec![n(0.0), n(0.0), n(-1.0)],
            vec![n(-1.0), n(-50.0), n(-70.0)],
        ] {
            assert_eq!(call(&TIME, false, args), Err(CalcError::Num));
        }
    }

    // --- WEEKDAY ------------------------------------------------------------

    #[test]
    fn weekday_basics() {
        // Excel anchors serial 1 on Sunday in the 1900 system.
        assert_eq!(call(&WEEKDAY, false, vec![n(1.0)]), Ok(n(1.0)));
        assert_eq!(call(&WEEKDAY, false, vec![n(7.0)]), Ok(n(7.0)));
        assert_eq!(call(&WEEKDAY, false, vec![n(1.0), n(2.0)]), Ok(n(7.0)));
        assert_eq!(call(&WEEKDAY, false, vec![n(1.0), n(3.0)]), Ok(n(6.0)));
        assert_eq!(call(&WEEKDAY, false, vec![n(7.0), n(2.0)]), Ok(n(6.0)));
        assert_eq!(call(&WEEKDAY, false, vec![n(7.0), n(3.0)]), Ok(n(5.0)));
        assert_eq!(call(&WEEKDAY, false, vec![n(60.0)]), Ok(n(4.0)));
        // serial 61 is 1900-03-01, a real Thursday
        assert_eq!(call(&WEEKDAY, false, vec![n(61.0)]), Ok(n(5.0)));
        // serial 45292 is 2024-01-01, a real Monday
        assert_eq!(call(&WEEKDAY, false, vec![n(45292.0)]), Ok(n(2.0)));
        assert_eq!(call(&WEEKDAY, false, vec![n(45292.0), n(2.0)]), Ok(n(1.0)));
        assert_eq!(call(&WEEKDAY, false, vec![n(45292.0), n(3.0)]), Ok(n(0.0)));
    }

    #[test]
    fn weekday_1904() {
        assert_eq!(call(&WEEKDAY, true, vec![n(0.0)]), Ok(n(6.0)));
        assert_eq!(call(&WEEKDAY, true, vec![n(0.0), n(2.0)]), Ok(n(5.0)));
        assert_eq!(call(&WEEKDAY, true, vec![n(0.0), n(3.0)]), Ok(n(4.0)));
        assert_eq!(call(&WEEKDAY, true, vec![n(2.0)]), Ok(n(1.0)));
    }

    #[test]
    fn weekday_unsupported_type_is_num() {
        assert_eq!(
            call(&WEEKDAY, false, vec![n(1.0), n(5.0)]),
            Err(CalcError::Num)
        );
    }

    // --- EDATE / EOMONTH ----------------------------------------------------

    #[test]
    fn edate_adds_whole_months() {
        assert_eq!(
            call(&EDATE, false, vec![n(45352.0), n(1.0)]),
            Ok(n(45383.0))
        );
        assert_eq!(
            call(&EDATE, false, vec![n(45352.0), n(-1.0)]),
            Ok(n(45323.0))
        );
        assert_eq!(
            call(&EDATE, false, vec![n(45323.0), n(1.0)]),
            Ok(n(45352.0))
        );
        assert_eq!(
            call(&EDATE, false, vec![n(45322.0), n(12.0)]),
            Ok(n(45688.0))
        );
        assert_eq!(
            call(&EDATE, false, vec![n(45322.0), n(-12.0)]),
            Ok(n(44957.0))
        );
    }

    #[test]
    fn edate_clamps_day() {
        // 2024-01-31 + 1 month -> 2024-02-29
        assert_eq!(
            call(&EDATE, false, vec![n(45322.0), n(1.0)]),
            Ok(n(45351.0))
        );
        // 2024-02-29 + 1 month -> 2024-03-29
        assert_eq!(
            call(&EDATE, false, vec![n(45351.0), n(1.0)]),
            Ok(n(45380.0))
        );
    }

    #[test]
    fn edate_errors() {
        assert_eq!(
            call(&EDATE, false, vec![n(1.0), n(-1.0)]),
            Err(CalcError::Num)
        );
        assert_eq!(
            call(&EDATE, false, vec![n(2958465.0), n(1.0)]),
            Err(CalcError::Num)
        );
        assert_eq!(call(&EDATE, true, vec![n(43890.0), n(1.0)]), Ok(n(43921.0)));
    }

    #[test]
    fn eomonth_last_day() {
        assert_eq!(
            call(&EOMONTH, false, vec![n(45352.0), n(0.0)]),
            Ok(n(45382.0))
        );
        assert_eq!(
            call(&EOMONTH, false, vec![n(45352.0), n(1.0)]),
            Ok(n(45412.0))
        );
        assert_eq!(
            call(&EOMONTH, false, vec![n(45322.0), n(1.0)]),
            Ok(n(45351.0))
        );
        assert_eq!(
            call(&EOMONTH, false, vec![n(45322.0), n(-1.0)]),
            Ok(n(45291.0))
        );
        assert_eq!(
            call(&EOMONTH, false, vec![n(45352.0), n(12.0)]),
            Ok(n(45747.0))
        );
        // Feb 1900 has 29 phantom days in the 1900 system.
        assert_eq!(call(&EOMONTH, false, vec![n(32.0), n(0.0)]), Ok(n(60.0)));
        assert_eq!(call(&EOMONTH, false, vec![n(1.0), n(0.0)]), Ok(n(31.0)));
    }

    // --- DATEDIF ------------------------------------------------------------

    #[test]
    fn datedif_units() {
        let start = || n(45292.0); // 2024-01-01
        let end = || n(45352.0); // 2024-03-01
        assert_eq!(
            call(&DATEDIF, false, vec![start(), end(), t("D")]),
            Ok(n(60.0))
        );
        assert_eq!(
            call(&DATEDIF, false, vec![start(), end(), t("M")]),
            Ok(n(2.0))
        );
        assert_eq!(
            call(&DATEDIF, false, vec![start(), end(), t("Y")]),
            Ok(n(0.0))
        );
        assert_eq!(
            call(&DATEDIF, false, vec![start(), end(), t("MD")]),
            Ok(n(0.0))
        );
        assert_eq!(
            call(&DATEDIF, false, vec![start(), end(), t("YM")]),
            Ok(n(2.0))
        );
        assert_eq!(
            call(&DATEDIF, false, vec![start(), end(), t("YD")]),
            Ok(n(60.0))
        );

        let start = || n(45292.0); // 2024-01-01
        let end = || n(45658.0); // 2025-01-01
        assert_eq!(
            call(&DATEDIF, false, vec![start(), end(), t("D")]),
            Ok(n(366.0))
        );
        assert_eq!(
            call(&DATEDIF, false, vec![start(), end(), t("M")]),
            Ok(n(12.0))
        );
        assert_eq!(
            call(&DATEDIF, false, vec![start(), end(), t("Y")]),
            Ok(n(1.0))
        );
        assert_eq!(
            call(&DATEDIF, false, vec![start(), end(), t("MD")]),
            Ok(n(0.0))
        );
        assert_eq!(
            call(&DATEDIF, false, vec![start(), end(), t("YM")]),
            Ok(n(0.0))
        );
        assert_eq!(
            call(&DATEDIF, false, vec![start(), end(), t("YD")]),
            Ok(n(0.0))
        );
    }

    #[test]
    fn datedif_month_end_quirk() {
        // 2024-01-31 -> 2024-03-01: MD reproduces Excel's negative result.
        let start = || n(45322.0);
        let end = || n(45352.0);
        assert_eq!(
            call(&DATEDIF, false, vec![start(), end(), t("M")]),
            Ok(n(1.0))
        );
        assert_eq!(
            call(&DATEDIF, false, vec![start(), end(), t("MD")]),
            Ok(n(-1.0))
        );
        assert_eq!(
            call(&DATEDIF, false, vec![start(), end(), t("YM")]),
            Ok(n(1.0))
        );
        assert_eq!(
            call(&DATEDIF, false, vec![start(), end(), t("YD")]),
            Ok(n(30.0))
        );
        // 2014-08-31 -> 2014-09-30: M stays 0 (day 30 < day 31), MD is the raw day gap.
        assert_eq!(
            call(&DATEDIF, false, vec![n(41882.0), n(41912.0), t("D")]),
            Ok(n(30.0))
        );
        assert_eq!(
            call(&DATEDIF, false, vec![n(41882.0), n(41912.0), t("M")]),
            Ok(n(0.0))
        );
        assert_eq!(
            call(&DATEDIF, false, vec![n(41882.0), n(41912.0), t("MD")]),
            Ok(n(30.0))
        );
    }

    #[test]
    fn datedif_is_case_insensitive() {
        assert_eq!(
            call(&DATEDIF, false, vec![n(45292.0), n(45352.0), t("d")]),
            Ok(n(60.0))
        );
        assert_eq!(
            call(&DATEDIF, false, vec![n(45292.0), n(45352.0), t(" m ")]).map(|v| v.to_string()),
            Ok("2".to_string())
        );
    }

    #[test]
    fn datedif_errors() {
        assert_eq!(
            call(&DATEDIF, false, vec![n(45352.0), n(45292.0), t("D")]),
            Err(CalcError::Num)
        );
        assert_eq!(
            call(&DATEDIF, false, vec![n(45292.0), n(45352.0), t("X")]),
            Err(CalcError::Num)
        );
        assert_eq!(
            call(&DATEDIF, false, vec![n(45292.0), n(45352.0), t("")]),
            Err(CalcError::Num)
        );
    }

    // --- DATEVALUE ----------------------------------------------------------

    #[test]
    fn datevalue_parses_iso_slash_and_time() {
        assert_eq!(
            call(&DATEVALUE, false, vec![t("2024-03-01")]),
            Ok(n(45352.0))
        );
        assert_eq!(
            call(&DATEVALUE, false, vec![t("2024-02-29")]),
            Ok(n(45351.0))
        );
        assert_eq!(call(&DATEVALUE, false, vec![t("1900-02-29")]), Ok(n(60.0)));
        assert_eq!(call(&DATEVALUE, false, vec![t("1900-01-01")]), Ok(n(1.0)));
        assert_eq!(
            call(&DATEVALUE, false, vec![t("9999-12-31")]),
            Ok(n(2958465.0))
        );
        assert_eq!(
            call(&DATEVALUE, false, vec![t(" 2024-03-01 ")]),
            Ok(n(45352.0))
        );
        assert_eq!(
            call(&DATEVALUE, true, vec![t("2024-03-01")]),
            Ok(n(43890.0))
        );
        // unpadded ISO and year-first slash are unambiguous, so they parse
        assert_eq!(call(&DATEVALUE, false, vec![t("2024-3-1")]), Ok(n(45352.0)));
        assert_eq!(
            call(&DATEVALUE, false, vec![t("2024/03/01")]),
            Ok(n(45352.0))
        );
        assert_eq!(call(&DATEVALUE, false, vec![t("2011/1/1")]), Ok(n(40544.0)));
        // a trailing time contributes nothing to the serial
        assert_eq!(
            call(&DATEVALUE, false, vec![t("2020-01-02 13:14:15")]),
            Ok(n(43832.0))
        );
        assert_eq!(
            call(&DATEVALUE, false, vec![t("2024-02-29 12:00")]),
            Ok(n(45351.0))
        );
        // a bare time string has date part 0
        assert_eq!(call(&DATEVALUE, false, vec![t("8:30 AM")]), Ok(n(0.0)));
    }

    #[test]
    fn datevalue_rejects_ambiguity() {
        for bad in [
            "03/01/2024",
            "03-01-2024",
            "2024-13-01",
            "2024-02-30",
            "2024/13/01",
            "2024/2/30",
            "hello",
            "2020-01-02XYZ",
            "",
        ] {
            assert_eq!(
                call(&DATEVALUE, false, vec![t(bad)]),
                Err(CalcError::Value),
                "expected VALUE for {bad:?}"
            );
        }
        // 1900-02-29 does not exist in the 1904 system.
        assert_eq!(
            call(&DATEVALUE, true, vec![t("1900-02-29")]),
            Err(CalcError::Value)
        );
        assert_eq!(
            call(&DATEVALUE, false, vec![n(45292.0)]),
            Err(CalcError::Value)
        );
    }

    // --- DAYS ---------------------------------------------------------------

    #[test]
    fn days_is_serial_difference() {
        assert_eq!(
            call(&DAYS, false, vec![n(45352.0), n(45292.0)]),
            Ok(n(60.0))
        );
        assert_eq!(
            call(&DAYS, false, vec![n(45292.0), n(45352.0)]),
            Ok(n(-60.0))
        );
        assert_eq!(call(&DAYS, false, vec![n(45352.0), n(45352.0)]), Ok(n(0.0)));
        // Excel truncates the fractional time portions of both serials.
        assert_eq!(
            call(&DAYS, false, vec![n(45352.5), n(45292.25)]),
            Ok(n(60.0))
        );
        assert_eq!(
            call(&DAYS, false, vec![n(43832.233), n(1.0)]),
            Ok(n(43831.0))
        );
        // date-text arguments are coerced like serials
        assert_eq!(
            call(&DAYS, false, vec![t("2020-01-02"), n(1.0)]),
            Ok(n(43831.0))
        );
        assert_eq!(
            call(&DAYS, false, vec![t("2011/1/29"), t("2011/1/1")]),
            Ok(n(28.0))
        );
    }

    // --- TODAY / NOW --------------------------------------------------------

    #[test]
    fn today_and_now_are_volatile() {
        assert!(TODAY.volatile);
        assert!(NOW.volatile);
        assert!(!DATE.volatile);
        assert!(!WEEKDAY.volatile);
    }

    #[test]
    fn today_is_current_date() {
        let today = call(&TODAY, false, vec![]).unwrap();
        let tv = today.as_number().unwrap();
        assert!(tv.is_finite());
        assert_eq!(tv, tv.trunc(), "TODAY must be an integer serial");
        assert!((40000.0..60000.0).contains(&tv));
    }

    #[test]
    fn now_has_time_fraction() {
        let now = call(&NOW, false, vec![]).unwrap();
        let nv = now.as_number().unwrap();
        assert!(nv.is_finite());
        assert!((40000.0..60000.0).contains(&nv));
        assert!(nv.fract() >= 0.0 && nv.fract() < 1.0);
    }

    // --- NETWORKDAYS / NETWORKDAYS.INTL --------------------------------------

    // Published Excel example: 10/1/2012 to 3/1/2013 is 110 working days,
    // 109 with 11/22/2012 as a holiday.
    #[test]
    fn networkdays_published_example() {
        let g = Grid::empty()
            .set_num("A1", 41183.0) // 10/1/2012
            .set_num("A2", 41334.0) // 3/1/2013
            .set_num("A3", 41235.0); // 11/22/2012
        assert_eq!(g.num("=NETWORKDAYS(A1,A2)"), 110.0);
        assert_eq!(g.num("=NETWORKDAYS(A1,A2,A3)"), 109.0);
        assert_eq!(g.num("=NETWORKDAYS(A1,A2,A3:A3)"), 109.0);
    }

    #[test]
    fn networkdays_counts_inclusive_endpoints() {
        assert_eq!(
            call(&NETWORKDAYS, false, vec![n(45292.0), n(45292.0)]),
            Ok(n(1.0))
        );
        assert_eq!(
            call(&NETWORKDAYS, false, vec![n(45296.0), n(45296.0)]),
            Ok(n(1.0))
        );
        assert_eq!(
            call(&NETWORKDAYS, false, vec![n(45297.0), n(45297.0)]),
            Ok(n(0.0))
        );
        assert_eq!(
            call(&NETWORKDAYS, false, vec![n(45292.0), n(45298.0)]),
            Ok(n(5.0))
        );
        assert_eq!(
            call(&NETWORKDAYS, false, vec![n(45292.0), n(45299.0)]),
            Ok(n(6.0))
        );
    }

    #[test]
    fn networkdays_reversed_dates_are_negative() {
        assert_eq!(
            call(&NETWORKDAYS, false, vec![n(45298.0), n(45292.0)]),
            Ok(n(-5.0))
        );
    }

    #[test]
    fn networkdays_holiday_on_weekend_not_double_counted() {
        // 45297 is the Saturday of the 2024-01-01 week; it was never counted.
        assert_eq!(
            call(
                &NETWORKDAYS,
                false,
                vec![n(45292.0), n(45299.0), n(45297.0)]
            ),
            Ok(n(6.0))
        );
        // ...and a holiday on a real workday subtracts one.
        assert_eq!(
            call(
                &NETWORKDAYS,
                false,
                vec![n(45292.0), n(45299.0), n(45293.0)]
            ),
            Ok(n(5.0))
        );
    }

    #[test]
    fn networkdays_accepts_an_array_holiday_list() {
        let arr = CalcValue::array(ArrayValue::new(2, 1, vec![n(45293.0), n(45294.0)]));
        assert_eq!(
            call(&NETWORKDAYS, false, vec![n(45292.0), n(45299.0), arr]),
            Ok(n(4.0))
        );
    }

    #[test]
    fn networkdays_intl_weekend_number_and_string() {
        // 2024-01-01 (Mon) .. 2024-01-07 (Sun).
        assert_eq!(
            call(
                &NETWORKDAYS_INTL,
                false,
                vec![n(45292.0), n(45298.0), t("0000011")]
            ),
            Ok(n(5.0))
        );
        assert_eq!(
            call(
                &NETWORKDAYS_INTL,
                false,
                vec![n(45292.0), n(45298.0), n(1.0)]
            ),
            Ok(n(5.0))
        );
        // Weekend pattern 11: only Monday is a weekend.
        assert_eq!(
            call(
                &NETWORKDAYS_INTL,
                false,
                vec![n(45292.0), n(45298.0), n(11.0)]
            ),
            Ok(n(6.0))
        );
        assert_eq!(
            call(
                &NETWORKDAYS_INTL,
                false,
                vec![n(45292.0), n(45298.0), t("1000000")]
            ),
            Ok(n(6.0))
        );
    }

    #[test]
    fn networkdays_intl_weekend_errors() {
        assert_eq!(
            call(
                &NETWORKDAYS_INTL,
                false,
                vec![n(45292.0), n(45298.0), t("1111111")]
            ),
            Err(CalcError::Value)
        );
        assert_eq!(
            call(
                &NETWORKDAYS_INTL,
                false,
                vec![n(45292.0), n(45298.0), t("000011")]
            ),
            Err(CalcError::Value)
        );
        assert_eq!(
            call(
                &NETWORKDAYS_INTL,
                false,
                vec![n(45292.0), n(45298.0), n(18.0)]
            ),
            Err(CalcError::Value)
        );
        assert_eq!(
            call(
                &NETWORKDAYS_INTL,
                false,
                vec![n(45292.0), n(45298.0), t("abc")]
            ),
            Err(CalcError::Value)
        );
    }

    #[test]
    fn networkdays_1904_system() {
        // Serial 0 = 1904-01-01 = Friday; serials 1, 2 = Sat, Sun.
        assert_eq!(call(&NETWORKDAYS, true, vec![n(0.0), n(0.0)]), Ok(n(1.0)));
        assert_eq!(call(&NETWORKDAYS, true, vec![n(0.0), n(2.0)]), Ok(n(1.0)));
        assert_eq!(call(&NETWORKDAYS, true, vec![n(2.0), n(2.0)]), Ok(n(0.0)));
    }

    // --- WORKDAY / WORKDAY.INTL ----------------------------------------------

    #[test]
    fn workday_adds_and_subtracts() {
        assert_eq!(
            call(&WORKDAY, false, vec![n(45292.0), n(1.0)]),
            Ok(n(45293.0))
        );
        assert_eq!(
            call(&WORKDAY, false, vec![n(45292.0), n(5.0)]),
            Ok(n(45299.0))
        );
        assert_eq!(
            call(&WORKDAY, false, vec![n(45296.0), n(1.0)]),
            Ok(n(45299.0))
        );
        assert_eq!(
            call(&WORKDAY, false, vec![n(45296.0), n(-1.0)]),
            Ok(n(45295.0))
        );
        assert_eq!(
            call(&WORKDAY, false, vec![n(45299.0), n(-1.0)]),
            Ok(n(45296.0))
        );
        assert_eq!(
            call(&WORKDAY, false, vec![n(45292.0), n(0.0)]),
            Ok(n(45292.0))
        );
    }

    #[test]
    fn workday_skips_holidays() {
        // 2024-01-02 is a holiday: +1 workday from Monday lands on Wednesday.
        assert_eq!(
            call(&WORKDAY, false, vec![n(45292.0), n(1.0), n(45293.0)]),
            Ok(n(45294.0))
        );
        // ...and backwards past a holiday on Thursday lands on Wednesday.
        assert_eq!(
            call(&WORKDAY, false, vec![n(45296.0), n(-1.0), n(45295.0)]),
            Ok(n(45294.0))
        );
    }

    #[test]
    fn workday_holiday_on_weekend_ignored() {
        // 45297 is a Saturday; it must not shift the result.
        assert_eq!(
            call(&WORKDAY, false, vec![n(45292.0), n(1.0), n(45297.0)]),
            Ok(n(45293.0))
        );
    }

    #[test]
    fn workday_intl_goes_backwards() {
        // Weekend 11 = Monday only. From Monday 1/8, one workday back is 1/7.
        assert_eq!(
            call(&WORKDAY_INTL, false, vec![n(45299.0), n(-1.0), n(11.0)]),
            Ok(n(45298.0))
        );
        // Forward from a Monday weekend with the same pattern lands on Tuesday.
        assert_eq!(
            call(&WORKDAY_INTL, false, vec![n(45292.0), n(1.0), n(11.0)]),
            Ok(n(45293.0))
        );
        // String form, backwards.
        assert_eq!(
            call(
                &WORKDAY_INTL,
                false,
                vec![n(45299.0), n(-1.0), t("1000000")]
            ),
            Ok(n(45298.0))
        );
    }

    #[test]
    fn workday_intl_weekend_errors() {
        assert_eq!(
            call(&WORKDAY_INTL, false, vec![n(45292.0), n(1.0), t("1111111")]),
            Err(CalcError::Value)
        );
        assert_eq!(
            call(&WORKDAY_INTL, false, vec![n(45292.0), n(1.0), n(0.0)]),
            Err(CalcError::Value)
        );
    }

    #[test]
    fn workday_result_out_of_range_is_num() {
        assert_eq!(
            call(&WORKDAY, false, vec![n(2958465.0), n(1.0)]),
            Err(CalcError::Num)
        );
        assert_eq!(
            call(&WORKDAY, false, vec![n(1.0), n(-1.0)]),
            Err(CalcError::Num)
        );
    }

    #[test]
    fn workday_1904_system() {
        // 1904-01-01 (Fri) + 1 workday -> 1904-01-04 (Mon).
        assert_eq!(call(&WORKDAY, true, vec![n(0.0), n(1.0)]), Ok(n(3.0)));
    }

    // --- YEARFRAC ------------------------------------------------------------

    // Published Excel example: 1/1/2012 to 7/30/2012 (211 actual days, 2012
    // leap). basis 0 -> 0.58055556, basis 1 -> 0.57650273, basis 3 -> 0.57808219.
    #[test]
    fn yearfrac_all_five_bases() {
        let start = 40909.0; // 2012-01-01
        let end = 41120.0; // 2012-07-30
        assert_eq!(
            call(&YEARFRAC, false, vec![n(start), n(end)]),
            Ok(n(209.0 / 360.0))
        );
        assert_eq!(
            call(&YEARFRAC, false, vec![n(start), n(end), n(0.0)]),
            Ok(n(209.0 / 360.0))
        );
        assert_eq!(
            call(&YEARFRAC, false, vec![n(start), n(end), n(1.0)]),
            Ok(n(211.0 / 366.0))
        );
        assert_eq!(
            call(&YEARFRAC, false, vec![n(start), n(end), n(2.0)]),
            Ok(n(211.0 / 360.0))
        );
        assert_eq!(
            call(&YEARFRAC, false, vec![n(start), n(end), n(3.0)]),
            Ok(n(211.0 / 365.0))
        );
        assert_eq!(
            call(&YEARFRAC, false, vec![n(start), n(end), n(4.0)]),
            Ok(n(209.0 / 360.0))
        );
    }

    #[test]
    fn yearfrac_is_order_independent() {
        let start = 40909.0;
        let end = 41120.0;
        assert_eq!(
            call(&YEARFRAC, false, vec![n(end), n(start), n(1.0)]),
            call(&YEARFRAC, false, vec![n(start), n(end), n(1.0)])
        );
    }

    #[test]
    fn yearfrac_end_of_february_basis_zero() {
        // 2011-02-28 to 2011-03-31, basis 0: d1 is the last day of February,
        // so it becomes the 30th; the end stays the 31st -> 31/360.
        let feb = 40602.0; // 2011-02-28
        let mar = 40633.0; // 2011-03-31
        assert_eq!(
            call(&YEARFRAC, false, vec![n(feb), n(mar), n(0.0)]),
            Ok(n(31.0 / 360.0))
        );
        // European basis 4: the 31st becomes the 30th -> 32/360.
        assert_eq!(
            call(&YEARFRAC, false, vec![n(feb), n(mar), n(4.0)]),
            Ok(n(32.0 / 360.0))
        );
    }

    #[test]
    fn yearfrac_errors() {
        assert_eq!(
            call(&YEARFRAC, false, vec![n(40909.0), n(41120.0), n(5.0)]),
            Err(CalcError::Num)
        );
        assert_eq!(
            call(&YEARFRAC, false, vec![n(40909.0), n(41120.0), n(-1.0)]),
            Err(CalcError::Num)
        );
        assert_eq!(
            call(&YEARFRAC, false, vec![t("abc"), n(41120.0)]),
            Err(CalcError::Value)
        );
    }

    // --- DAYS360 -------------------------------------------------------------

    // Published Excel examples (2011 dates).
    #[test]
    fn days360_published_examples() {
        let jan1 = 40544.0; // 2011-01-01
        let jan30 = 40573.0; // 2011-01-30
        let feb1 = 40575.0; // 2011-02-01
        let dec31 = 40908.0; // 2011-12-31
        assert_eq!(call(&DAYS360, false, vec![n(jan30), n(feb1)]), Ok(n(1.0)));
        assert_eq!(call(&DAYS360, false, vec![n(jan1), n(dec31)]), Ok(n(360.0)));
        assert_eq!(call(&DAYS360, false, vec![n(jan1), n(feb1)]), Ok(n(30.0)));
    }

    #[test]
    fn days360_us_vs_european() {
        let jan1 = 40544.0;
        let dec31 = 40908.0;
        // US: Dec 31 is the last day of the month, start day < 30, so the end
        // becomes 1/1 of next year -> 360. European: Dec 31 -> 30 -> 359.
        assert_eq!(
            call(&DAYS360, false, vec![n(jan1), n(dec31), n(0.0)]),
            Ok(n(360.0))
        );
        assert_eq!(
            call(&DAYS360, false, vec![n(jan1), n(dec31), n(1.0)]),
            Ok(n(359.0))
        );
        assert_eq!(
            call(
                &DAYS360,
                false,
                vec![n(jan1), n(dec31), CalcValue::Bool(true)]
            ),
            Ok(n(359.0))
        );
        // Start date on the 31st: US pulls it back to the 30th.
        let jan31 = 40574.0; // 2011-01-31
        assert_eq!(
            call(&DAYS360, false, vec![n(jan31), n(40575.0)]),
            Ok(n(1.0))
        );
    }

    #[test]
    fn days360_reversed_is_negative() {
        let jan1 = 40544.0;
        let dec31 = 40908.0;
        // Rules applied in the given order, so the reverse is not symmetric.
        assert_eq!(
            call(&DAYS360, false, vec![n(dec31), n(jan1)]),
            Ok(n(-359.0))
        );
        assert_eq!(call(&DAYS360, false, vec![n(jan1), n(dec31)]), Ok(n(360.0)));
    }

    #[test]
    fn days360_end_of_february() {
        let feb28 = 40602.0; // 2011-02-28
        let jan31 = 40574.0; // 2011-01-31
        assert_eq!(call(&DAYS360, false, vec![n(jan31), n(feb28)]), Ok(n(30.0)));
        assert_eq!(
            call(&DAYS360, false, vec![n(jan31), n(feb28), n(1.0)]),
            Ok(n(28.0))
        );
    }

    #[test]
    fn days360_1900_system_phantom_leap_day() {
        // Serial 60 is 1900-02-29, the last day of February in the 1900 system.
        assert_eq!(call(&DAYS360, false, vec![n(60.0), n(61.0)]), Ok(n(1.0)));
        assert_eq!(call(&DAYS360, false, vec![n(60.0), n(90.0)]), Ok(n(30.0)));
    }

    // --- WEEKNUM / ISOWEEKNUM ------------------------------------------------

    #[test]
    fn weeknum_published_example() {
        assert_eq!(call(&WEEKNUM, false, vec![n(40909.0)]), Ok(n(1.0)));
        assert_eq!(call(&WEEKNUM, false, vec![n(40909.0), n(2.0)]), Ok(n(1.0)));
        assert_eq!(call(&WEEKNUM, false, vec![n(40977.0)]), Ok(n(10.0)));
        assert_eq!(call(&WEEKNUM, false, vec![n(40977.0), n(2.0)]), Ok(n(11.0)));
        // ISO: 3/9/2012 is Friday of ISO week 10.
        assert_eq!(
            call(&WEEKNUM, false, vec![n(40977.0), n(21.0)]),
            Ok(n(10.0))
        );
    }

    #[test]
    fn weeknum_iso_and_weekday_starts() {
        // 2024-01-01 (Mon) is ISO W1; 2024-01-08 (Mon) is ISO W2.
        assert_eq!(call(&WEEKNUM, false, vec![n(45292.0), n(21.0)]), Ok(n(1.0)));
        assert_eq!(call(&WEEKNUM, false, vec![n(45299.0), n(21.0)]), Ok(n(2.0)));
        // 2023-01-01 (Sunday) is ISO 2022-W52.
        assert_eq!(
            call(&WEEKNUM, false, vec![n(44927.0), n(21.0)]),
            Ok(n(52.0))
        );
        // return_type 14 starts the week on Thursday.
        assert_eq!(
            call(&WEEKNUM, false, vec![n(40977.0), n(14.0)]),
            Ok(n(11.0))
        );
    }

    #[test]
    fn weeknum_unsupported_return_type_is_num() {
        for r in [3.0, 5.0, 10.0, 18.0, 22.0] {
            assert_eq!(
                call(&WEEKNUM, false, vec![n(45292.0), n(r)]),
                Err(CalcError::Num),
                "return_type {r}"
            );
        }
    }

    #[test]
    fn isoweeknum_january_belongs_to_previous_year() {
        // 2021-01-01 (Friday) is 2020-W53.
        assert_eq!(call(&ISOWEEKNUM, false, vec![n(44197.0)]), Ok(n(53.0)));
        // 2016-01-01 (Friday) is 2015-W53.
        assert_eq!(call(&ISOWEEKNUM, false, vec![n(42370.0)]), Ok(n(53.0)));
        // 2023-01-01 (Sunday) is 2022-W52.
        assert_eq!(call(&ISOWEEKNUM, false, vec![n(44927.0)]), Ok(n(52.0)));
        // 2024-01-01 (Monday) contains the first Thursday, so it is 2024-W1.
        assert_eq!(call(&ISOWEEKNUM, false, vec![n(45292.0)]), Ok(n(1.0)));
        assert_eq!(call(&ISOWEEKNUM, false, vec![n(45295.0)]), Ok(n(1.0)));
    }

    #[test]
    fn isoweeknum_1904_system() {
        // 1904-01-01 (Friday) is ISO 1903-W53 (2021 analogue: 1904 leap).
        assert_eq!(call(&ISOWEEKNUM, true, vec![n(0.0)]), Ok(n(53.0)));
    }

    // --- TIMEVALUE -----------------------------------------------------------

    #[test]
    fn timevalue_published_examples() {
        assert!(near(
            call(&TIMEVALUE, false, vec![t("8:30 AM")]).unwrap(),
            8.5 / 24.0
        ));
        assert!(near(
            call(&TIMEVALUE, false, vec![t("3:30 PM")]).unwrap(),
            15.5 / 24.0
        ));
        assert!(near(
            call(&TIMEVALUE, false, vec![t("12:00 AM")]).unwrap(),
            0.0
        ));
        assert!(near(
            call(&TIMEVALUE, false, vec![t("6:30 AM")]).unwrap(),
            6.5 / 24.0
        ));
    }

    #[test]
    fn timevalue_parses_24h_and_seconds() {
        assert!(near(
            call(&TIMEVALUE, false, vec![t("13:30")]).unwrap(),
            13.5 / 24.0
        ));
        assert!(near(
            call(&TIMEVALUE, false, vec![t("13:30:45")]).unwrap(),
            13.5 / 24.0 + 45.0 / 86400.0
        ));
        assert!(near(
            call(&TIMEVALUE, false, vec![t("24:00")]).unwrap(),
            0.0
        ));
        assert!(near(
            call(&TIMEVALUE, false, vec![t(" 8:30 PM ")]).unwrap(),
            20.5 / 24.0
        ));
        // A leading ISO date contributes only the time fraction.
        assert!(near(
            call(&TIMEVALUE, false, vec![t("2024-01-01 14:30")]).unwrap(),
            14.5 / 24.0
        ));
    }

    #[test]
    fn timevalue_rejects_ambiguity() {
        for bad in [
            "8",
            "garbage",
            "8:60",
            "8:30:75",
            "25:00",
            "13:30 PM",
            "0 AM",
            "2024-13-01 8:00",
            "",
        ] {
            assert_eq!(
                call(&TIMEVALUE, false, vec![t(bad)]),
                Err(CalcError::Value),
                "expected VALUE for {bad:?}"
            );
        }
        assert_eq!(
            call(&TIMEVALUE, false, vec![n(45292.0)]),
            Err(CalcError::Value)
        );
    }

    // --- lane E: date-string coercion family (brief_lane_E.csv) ---------------

    // Every row of brief_lane_E.csv, whose expected values are Excel-COM
    // measured (referee_v2_full.txt). The theme: date strings (ISO and
    // year-first slash, optional time) coerce in every date-part function,
    // negative serials/times are #NUM!, and DAYS truncates to integers.
    #[test]
    fn lane_e_date_string_coercion() {
        assert_eq!(call(&DAY, false, vec![t("2020-01-02")]), Ok(n(2.0)));
        assert_eq!(call(&MONTH, false, vec![t("2020-01-02")]), Ok(n(1.0)));
        assert_eq!(call(&YEAR, false, vec![t("2020-01-02")]), Ok(n(2020.0)));
        assert_eq!(call(&HOUR, false, vec![t("2020-01-02 7:45")]), Ok(n(7.0)));
        assert_eq!(
            call(&MINUTE, false, vec![t("2020-01-02 7:45")]),
            Ok(n(45.0))
        );
        assert_eq!(
            call(&SECOND, false, vec![t("2020-01-02 7:45:18")]),
            Ok(n(18.0))
        );
        assert_eq!(
            call(&DATEVALUE, false, vec![t("2020-01-02 13:14:15")]),
            Ok(n(43832.0))
        );
        assert_eq!(call(&TIMEVALUE, false, vec![t("2020-01-02")]), Ok(n(0.0)));
        assert_eq!(
            call(&WEEKDAY, false, vec![t("2008-11-26"), n(1.0)]),
            Ok(n(4.0))
        );
        assert_eq!(call(&ISOWEEKNUM, false, vec![t("2008-11-26")]), Ok(n(48.0)));
        assert_eq!(
            call(&WEEKNUM, false, vec![t("2011-1-1"), n(21.0)]),
            Ok(n(52.0))
        );
        assert_eq!(
            call(&EOMONTH, false, vec![t("2011/1/1"), n(1.0)]),
            Ok(n(40602.0))
        );
        assert_eq!(
            call(&NETWORKDAYS, false, vec![t("2012-10-1"), t("2013-3-1")]),
            Ok(n(110.0))
        );
        assert_eq!(
            call(
                &NETWORKDAYS_INTL,
                false,
                vec![t("2012-10-1"), t("2013-3-1")]
            ),
            Ok(n(110.0))
        );
        assert_eq!(
            call(&WORKDAY, false, vec![t("2008-10-1"), n(151.0)]),
            Ok(n(39933.0))
        );
        assert_eq!(
            call(&WORKDAY_INTL, false, vec![t("2008-10-1"), n(151.0)]),
            Ok(n(39933.0))
        );
        assert_eq!(
            call(
                &DATEDIF,
                false,
                vec![t("2011/1/29"), t("2021/3/31"), t("Y")]
            ),
            Ok(n(10.0))
        );
        assert_eq!(
            call(&DAYS360, false, vec![t("2021/1/29"), t("2021/3/31")]),
            Ok(n(62.0))
        );
        assert_eq!(
            call(&DAYS, false, vec![n(43832.233), n(1.0)]),
            Ok(n(43831.0))
        );
        assert_eq!(
            call(&TIME, false, vec![n(-1.0), n(-50.0), n(-70.0)]),
            Err(CalcError::Num)
        );
        assert!(near(
            call(&YEARFRAC, false, vec![t("2012/2/2"), t("2021/3/11")]).unwrap(),
            9.108333333333333
        ));
    }

    #[test]
    fn lane_e_slash_dates_and_ampm() {
        // Excel-COM measured probes beyond the brief's one row per function.
        assert_eq!(
            call(&WEEKDAY, false, vec![t("2011/1/29"), n(1.0)]),
            Ok(n(7.0))
        );
        assert_eq!(
            call(&EDATE, false, vec![t("2011/1/1"), n(1.0)]),
            Ok(n(40575.0))
        );
        assert_eq!(
            call(&EOMONTH, false, vec![t("2011/1/1"), n(1.0)]),
            Ok(n(40602.0))
        );
        assert_eq!(
            call(&NETWORKDAYS, false, vec![t("2011/1/29"), t("2021/3/31")]),
            Ok(n(2653.0))
        );
        assert_eq!(
            call(&DAYS, false, vec![t("2020-01-02"), n(1.0)]),
            Ok(n(43831.0))
        );
        assert_eq!(call(&DAY, false, vec![t("8:30 AM")]), Ok(n(0.0)));
        assert_eq!(
            call(&HOUR, false, vec![t("2020-01-02 7:45 PM")]),
            Ok(n(19.0))
        );
        assert_eq!(call(&TIMEVALUE, false, vec![t("2011/1/29")]), Ok(n(0.0)));
        assert!(near(
            call(&TIMEVALUE, false, vec![t("2020-01-02 13:14:15")]).unwrap(),
            0.5515625
        ));
        assert!(near(
            call(&TIMEVALUE, false, vec![t("2020-01-02 7:45")]).unwrap(),
            7.75 / 24.0
        ));
        assert!(near(
            call(&TIMEVALUE, false, vec![t("2024-02-29 12:00")]).unwrap(),
            0.5
        ));
        assert_eq!(call(&DAY, false, vec![t("2020-1-2")]), Ok(n(2.0)));
    }

    #[test]
    fn negative_serials_are_num() {
        for f in [
            (&DAY, vec![n(-1.0)]),
            (&MONTH, vec![n(-1.0)]),
            (&YEAR, vec![n(-1.0)]),
            (&HOUR, vec![n(-1.0)]),
            (&MINUTE, vec![n(-0.5)]),
            (&SECOND, vec![n(-1.5)]),
            (&WEEKDAY, vec![n(-1.0)]),
            (&ISOWEEKNUM, vec![n(-1.0)]),
            (&WEEKNUM, vec![n(-1.0)]),
        ] {
            assert_eq!(call(f.0, false, f.1), Err(CalcError::Num));
        }
        assert_eq!(
            call(&EDATE, false, vec![n(-1.0), n(1.0)]),
            Err(CalcError::Num)
        );
        assert_eq!(
            call(&EOMONTH, false, vec![n(-1.0), n(1.0)]),
            Err(CalcError::Num)
        );
        assert_eq!(
            call(&NETWORKDAYS, false, vec![n(-1.0), n(100.0)]),
            Err(CalcError::Num)
        );
        assert_eq!(
            call(&WORKDAY, false, vec![n(-1.0), n(1.0)]),
            Err(CalcError::Num)
        );
        assert_eq!(
            call(&DATEDIF, false, vec![n(-1.0), n(100.0), t("D")]),
            Err(CalcError::Num)
        );
        assert_eq!(
            call(&YEARFRAC, false, vec![n(-1.0), n(100.0)]),
            Err(CalcError::Num)
        );
        assert_eq!(
            call(&DAYS360, false, vec![n(-1.0), n(100.0)]),
            Err(CalcError::Num)
        );
        // serial 0 stays valid: 1900's "January 0" / 1904-01-01
        assert_eq!(call(&DAY, false, vec![n(0.0)]), Ok(n(0.0)));
        assert_eq!(call(&YEAR, true, vec![n(0.0)]), Ok(n(1904.0)));
    }

    // --- registry wiring through the real path -------------------------------

    #[test]
    fn new_functions_reach_the_registry() {
        let g = Grid::empty();
        assert_eq!(g.num("=NETWORKDAYS(DATE(2012,10,1),DATE(2013,3,1))"), 110.0);
        assert_eq!(
            g.num("=WORKDAY(DATE(2024,1,1),1)"),
            g.num("=DATE(2024,1,2)")
        );
        assert_eq!(g.num("=ISOWEEKNUM(DATE(2021,1,1))"), 53.0);
        assert_eq!(g.num("=WEEKNUM(DATE(2012,3,9))"), 10.0);
        assert_eq!(g.num("=DAYS360(DATE(2011,1,1),DATE(2011,2,1))"), 30.0);
        assert_eq!(
            g.error("=YEARFRAC(DATE(2012,1,1),DATE(2012,7,30),9)"),
            CalcError::Num
        );
        assert_eq!(
            g.error("=NETWORKDAYS.INTL(DATE(2024,1,1),DATE(2024,1,7),\"1111111\")"),
            CalcError::Value
        );
        assert_eq!(
            g.num("=NETWORKDAYS.INTL(DATE(2024,1,1),DATE(2024,1,7))"),
            5.0
        );
        assert_eq!(g.error("=TIMEVALUE(\"garbage\")"), CalcError::Value);
    }
}
