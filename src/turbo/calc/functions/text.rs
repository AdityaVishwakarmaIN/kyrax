// functions/text.rs — the text function family. Owned exclusively by the text
// family agent; no other agent edits this file.
//
// Registry contract: implement `register` below and keep this exact signature.
// Do NOT edit functions/mod.rs — the `mod text;` declaration and the
// `text::register(&mut r)` call site in `build()` are already final.
// See functions/mod.rs for the worked ABS template.
//
// Semantics follow Excel: positions are 1-based and count characters, never
// bytes, so multi-byte text is handled by char iteration and never sliced
// mid-character. Unimplementable cases return the honest CalcError::Value
// rather than a guessed string.
//
// The `*B` functions (LEFTB, RIGHTB, MIDB, LENB, FINDB, SEARCHB) follow the
// measured en-IN (non-DBCS) byte policy: each UTF-16 code unit counts as 1
// byte and a surrogate pair (emoji) counts as 2, so LEN/LENB report code-unit
// counts while the positional `*B` functions return whole characters exactly
// like their non-B siblings (verified: LEFTB("😊Hello",3)="😊He",
// MIDB("中文测试",1,2)="中文", RIGHTB("中文测试",2)="测试",
// FINDB/SEARCHB("欢迎","你好，欢迎")=4, LENB(",。、；：{}")=7). DBCS-locale
// double-byte semantics would need a code page this engine does not carry;
// the divergence is documented, not guessed. ASC/DBCS convert only the
// full-width ASCII block (U+FF01–U+FF5E ↔ U+0021–U+007E and the ideographic
// space U+3000 ↔ U+0020); the full/half-width katakana tables are not
// carried, so katakana passes through untouched.

use super::{FuncArg, FuncCtx, FuncSpec, Registry};
use crate::turbo::calc::coerce::{coerce_number, coerce_text, number_to_general};
use crate::turbo::calc::value::{CalcError, CalcValue};

/// `n.abs()` is exact (IEEE sign flip), so a finite absolute value never
/// becomes NaN/Inf; the fallback is defensive only.
fn ok_num(n: f64) -> Result<CalcValue, CalcError> {
    if n.is_finite() {
        Ok(CalcValue::Number(n))
    } else {
        Err(CalcError::Num)
    }
}

/// Coerce to number and truncate toward zero (Excel uses the integer part of
/// count/position arguments). `coerce_number` never yields NaN/Inf.
fn trunc_arg(v: &CalcValue) -> Result<f64, CalcError> {
    Ok(coerce_number(v)?.trunc())
}

/// Excel TRIM removes leading/trailing ASCII spaces and collapses runs of
/// internal spaces to one. Other whitespace (tab, newline, NBSP) is preserved.
fn trim_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = true;
    for ch in s.chars() {
        if ch == ' ' {
            if !last_space && !out.is_empty() {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(ch);
            last_space = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// PROPER: the first letter of the string and any letter following a
/// non-letter become uppercase; all other letters become lowercase.
fn proper(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_alpha = false;
    for ch in s.chars() {
        if ch.is_alphabetic() {
            if prev_alpha {
                out.extend(ch.to_lowercase());
            } else {
                out.extend(ch.to_uppercase());
            }
            prev_alpha = true;
        } else {
            out.push(ch);
            prev_alpha = false;
        }
    }
    out
}

/// Match `pattern` against a PREFIX of `text` (both pre-lowercased char
/// vectors). SEARCH finds the earliest start at which the pattern matches some
/// substring, so a pattern may stop matching before the text runs out. `*` any
/// run, `?` one char, `~` escapes; same engine as `coerce::wildcard_match` but
/// returns true the moment the whole pattern is consumed.
fn wildcard_prefix_match(p: &[char], t: &[char]) -> bool {
    let mut pi = 0;
    let mut ti = 0;
    let mut star: Option<usize> = None;
    let mut star_ti = 0;
    loop {
        if pi == p.len() {
            return true;
        }
        if ti == t.len() {
            while pi < p.len() {
                if p[pi] == '*' {
                    pi += 1;
                } else {
                    return false;
                }
            }
            return true;
        }
        let pc = p[pi];
        if pc == '*' {
            star = Some(pi);
            star_ti = ti;
            pi += 1;
        } else if pc == '?' || pc == t[ti] {
            pi += 1;
            ti += 1;
        } else if pc == '~' && pi + 1 < p.len() && p[pi + 1] == t[ti] {
            pi += 2;
            ti += 1;
        } else if let Some(sp) = star {
            star_ti += 1;
            ti = star_ti;
            pi = sp + 1;
        } else {
            return false;
        }
    }
}

/// FIND (case-sensitive, no wildcards) and SEARCH (case-insensitive,
/// wildcards) share one skeleton. Returns the 1-based position or `#VALUE!`
/// when not found or the start is out of range.
fn find_impl(ctx: &FuncCtx, args: &[FuncArg], wildcards: bool) -> Result<CalcValue, CalcError> {
    let find = coerce_text(&args[0].value(ctx)?)?;
    let within = coerce_text(&args[1].value(ctx)?)?;
    let start = if args.len() == 3 {
        trunc_arg(&args[2].value(ctx)?)?
    } else {
        1.0
    };
    if start < 1.0 {
        return Err(CalcError::Value);
    }
    let chars: Vec<char> = within.chars().collect();
    let n = chars.len();
    let from = (start as usize).saturating_sub(1);
    if from > n {
        return Err(CalcError::Value);
    }
    if !wildcards {
        let find_chars: Vec<char> = find.chars().collect();
        if find_chars.is_empty() {
            return Ok(CalcValue::number((from + 1) as f64));
        }
        let flen = find_chars.len();
        if flen > n {
            return Err(CalcError::Value);
        }
        let limit = n - flen;
        let mut i = from;
        while i <= limit {
            if chars[i..i + flen] == find_chars[..] {
                return Ok(CalcValue::number((i + 1) as f64));
            }
            i += 1;
        }
        Err(CalcError::Value)
    } else {
        let p: Vec<char> = find.to_ascii_lowercase().chars().collect();
        let t: Vec<char> = within.to_ascii_lowercase().chars().collect();
        for si in from..=n {
            if wildcard_prefix_match(&p, &t[si..]) {
                return Ok(CalcValue::number((si + 1) as f64));
            }
        }
        Err(CalcError::Value)
    }
}

fn left(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let text = coerce_text(&args[0].value(ctx)?)?;
    let count = if args.len() == 2 {
        trunc_arg(&args[1].value(ctx)?)?
    } else {
        1.0
    };
    if count < 0.0 {
        return Err(CalcError::Value);
    }
    let chars: Vec<char> = text.chars().collect();
    let n = (count as usize).min(chars.len());
    Ok(CalcValue::text(chars[..n].iter().collect::<String>()))
}

fn right(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let text = coerce_text(&args[0].value(ctx)?)?;
    let count = if args.len() == 2 {
        trunc_arg(&args[1].value(ctx)?)?
    } else {
        1.0
    };
    if count < 0.0 {
        return Err(CalcError::Value);
    }
    let chars: Vec<char> = text.chars().collect();
    let n = (count as usize).min(chars.len());
    Ok(CalcValue::text(
        chars[chars.len() - n..].iter().collect::<String>(),
    ))
}

fn mid(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let text = coerce_text(&args[0].value(ctx)?)?;
    let start = trunc_arg(&args[1].value(ctx)?)?;
    let len = trunc_arg(&args[2].value(ctx)?)?;
    if start < 1.0 || len < 0.0 {
        return Err(CalcError::Value);
    }
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let from = (start - 1.0) as usize;
    if from >= n {
        return Ok(CalcValue::text(""));
    }
    let to = (from as f64 + len).min(n as f64) as usize;
    Ok(CalcValue::text(chars[from..to].iter().collect::<String>()))
}

/// LEN counts UTF-16 code units, not characters: every BMP character is 1 and
/// a surrogate pair (emoji) is 2 — the measured en-IN Excel behavior
/// (`LEN("😊") = 2`). LENB shares this implementation (non-DBCS byte policy:
/// one byte per code unit).
fn len(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let text = coerce_text(&args[0].value(ctx)?)?;
    ok_num(text.encode_utf16().count() as f64)
}

fn trim(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let text = coerce_text(&args[0].value(ctx)?)?;
    Ok(CalcValue::text(trim_spaces(&text)))
}

fn upper(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let text = coerce_text(&args[0].value(ctx)?)?;
    Ok(CalcValue::text(text.to_uppercase()))
}

fn lower(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let text = coerce_text(&args[0].value(ctx)?)?;
    Ok(CalcValue::text(text.to_lowercase()))
}

fn proper_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let text = coerce_text(&args[0].value(ctx)?)?;
    Ok(CalcValue::text(proper(&text)))
}

fn find(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    find_impl(ctx, args, false)
}

fn search(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    find_impl(ctx, args, true)
}

fn substitute(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let text = coerce_text(&args[0].value(ctx)?)?;
    let old = coerce_text(&args[1].value(ctx)?)?;
    let new = coerce_text(&args[2].value(ctx)?)?;
    let instance = if args.len() == 4 {
        let i = trunc_arg(&args[3].value(ctx)?)?;
        if i < 1.0 {
            return Err(CalcError::Value);
        }
        Some(i as usize)
    } else {
        None
    };
    let text_chars: Vec<char> = text.chars().collect();
    let old_chars: Vec<char> = old.chars().collect();
    if old_chars.is_empty() {
        return Ok(CalcValue::text(text));
    }
    let n = text_chars.len();
    let ol = old_chars.len();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut count = 0usize;
    while i < n {
        if i + ol <= n && text_chars[i..i + ol] == old_chars[..] {
            count += 1;
            if instance.is_none() || instance == Some(count) {
                out.push_str(&new);
            } else {
                out.extend(&old_chars);
            }
            i += ol;
        } else {
            out.push(text_chars[i]);
            i += 1;
        }
    }
    Ok(CalcValue::text(out))
}

fn replace(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let text = coerce_text(&args[0].value(ctx)?)?;
    let start = trunc_arg(&args[1].value(ctx)?)?;
    let count = trunc_arg(&args[2].value(ctx)?)?;
    let new = coerce_text(&args[3].value(ctx)?)?;
    if start < 1.0 || count < 0.0 {
        return Err(CalcError::Value);
    }
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let from = (start - 1.0) as usize;
    if from >= n {
        let mut out = text;
        out.push_str(&new);
        return Ok(CalcValue::text(out));
    }
    let end = (from as f64 + count).min(n as f64) as usize;
    let mut out = String::with_capacity(text.len());
    out.extend(&chars[..from]);
    out.push_str(&new);
    out.extend(&chars[end..]);
    Ok(CalcValue::text(out))
}

/// Days from 1970-01-01 for a civil date (proleptic Gregorian; Hinnant's
/// `days_from_civil`, mirrored here so VALUE can share the datetime family's
/// serial arithmetic without reaching into its private helpers).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn days_in_month_value(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

/// Excel serial for a civil date: the 1900 system includes the phantom
/// 1900-02-29 as serial 60, so every real day on/after 1900-03-01 is one
/// ahead of the proleptic day count; the 1904 system has serial 0 = 1904-01-01.
fn civil_to_serial_value(y: i64, m: i64, d: i64, date1904: bool) -> i64 {
    if !date1904 && (y, m, d) == (1900, 2, 29) {
        return 60;
    }
    const DAYS_1899_TO_1970: i64 = 25568;
    const DAYS_1899_TO_1904: i64 = 1461;
    let real_day = days_from_civil(y, m, d) + DAYS_1899_TO_1970;
    if date1904 {
        real_day - DAYS_1899_TO_1904
    } else if real_day <= 59 {
        real_day
    } else {
        real_day + 1
    }
}

/// Parse a bare time `H:MM[:SS]` (whole string, 2-digit minutes/seconds) into
/// a fraction of a day. `24:00` is exactly one day; hours above 24 are refused
/// and `H:MM:SS` must be well-formed. Matches `VALUE("16:48:00") = 0.7`.
fn parse_time_value(s: &str) -> Option<f64> {
    let b = s.as_bytes();
    let mut i = 0;
    let mut h = 0i64;
    let mut hd = 0;
    while i < b.len() && b[i].is_ascii_digit() && hd < 2 {
        h = h * 10 + (b[i] - b'0') as i64;
        hd += 1;
        i += 1;
    }
    if hd == 0 || i >= b.len() || b[i] != b':' {
        return None;
    }
    i += 1;
    let mut m = 0i64;
    let mut md = 0;
    while i < b.len() && b[i].is_ascii_digit() && md < 2 {
        m = m * 10 + (b[i] - b'0') as i64;
        md += 1;
        i += 1;
    }
    if md != 2 {
        return None;
    }
    let mut sec = 0i64;
    if i < b.len() && b[i] == b':' {
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
    if i != b.len() {
        return None;
    }
    if m > 59 || sec > 59 || h > 24 {
        return None;
    }
    if h == 24 && (m != 0 || sec != 0) {
        return None;
    }
    Some(h as f64 / 24.0 + m as f64 / 1440.0 + sec as f64 / 86400.0)
}

/// Parse a year-first date `YYYY-MM-DD` / `YYYY/M/D` with an optional time,
/// returning the full serial (mirrors the datetime family's accepted forms;
/// locale month-first forms are not guessed).
fn parse_date_value(s: &str, date1904: bool) -> Option<f64> {
    let b = s.as_bytes();
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
    let mut month = 0i64;
    let mut md = 0;
    while i < b.len() && b[i].is_ascii_digit() && md < 2 {
        month = month * 10 + (b[i] - b'0') as i64;
        md += 1;
        i += 1;
    }
    if md == 0 || i >= b.len() || b[i] != sep {
        return None;
    }
    i += 1;
    let mut day = 0i64;
    let mut dd = 0;
    while i < b.len() && b[i].is_ascii_digit() && dd < 2 {
        day = day * 10 + (b[i] - b'0') as i64;
        dd += 1;
        i += 1;
    }
    if dd == 0 {
        return None;
    }
    // The 1900 system's phantom 1900-02-29 (serial 60) is a valid day even
    // though it does not exist in the proleptic Gregorian calendar.
    let leap_day = !date1904 && year == 1900 && month == 2 && day == 29;
    if !(1..=12).contains(&month)
        || day < 1
        || (!leap_day && day > days_in_month_value(year, month))
    {
        return None;
    }
    let serial = civil_to_serial_value(year, month, day, date1904) as f64;
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
        Some(serial + parse_time_value(&s[i..])?)
    } else {
        Some(serial)
    }
}

/// VALUE parses plain numbers, percents, times and year-first dates; anything
/// else — including a locale currency-prefixed amount like "₹ 1,000" — is
/// `#VALUE!`.
fn value_from_text(t: &str, date1904: bool) -> Result<CalcValue, CalcError> {
    let s = t.trim();
    if s.is_empty() {
        return Err(CalcError::Value);
    }
    let mut pct = 0usize;
    let mut mant = s;
    while let Some(stripped) = mant.strip_suffix('%') {
        pct += 1;
        mant = stripped;
    }
    if pct > 0 {
        let mant = mant.trim();
        if mant.is_empty() {
            return Err(CalcError::Value);
        }
        let n: f64 = mant.parse().map_err(|_| CalcError::Value)?;
        return ok_num(n / 100f64.powi(pct as i32));
    }
    if let Some(t) = parse_time_value(s) {
        return ok_num(t);
    }
    if let Some(d) = parse_date_value(s, date1904) {
        return ok_num(d);
    }
    let n: f64 = s.parse().map_err(|_| CalcError::Value)?;
    ok_num(n)
}

fn value_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    match args[0].value(ctx)? {
        CalcValue::Number(n) => ok_num(n),
        CalcValue::Text(t) => value_from_text(&t, ctx.date1904),
        CalcValue::Error(e) => Err(e),
        _ => Err(CalcError::Value),
    }
}

/// Shortest round-trip digits via `ryu`, normalized to `(digits, dec_exp)`
/// with value = `0.digits × 10^dec_exp` (same contract as coerce.rs).
fn shortest_digits(a: f64) -> (Vec<u8>, i32) {
    let mut buf = ryu::Buffer::new();
    let s = buf.format(a);
    let bytes = s.as_bytes();
    let mut digits: Vec<u8> = Vec::with_capacity(17);
    let mut int_digits: i32 = 0;
    let mut seen_dot = false;
    let mut exp: i32 = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'.' => {
                seen_dot = true;
                i += 1;
            }
            b'e' | b'E' => {
                exp = s[i + 1..].parse::<i32>().unwrap_or(0);
                break;
            }
            b'0'..=b'9' => {
                digits.push(bytes[i] - b'0');
                if !seen_dot {
                    int_digits += 1;
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    let first = digits.iter().position(|&d| d != 0).unwrap_or(0);
    if first > 0 {
        digits.drain(..first);
    }
    let dec_exp = int_digits - first as i32 + exp;
    (digits, dec_exp)
}

/// Round `digits` to 15 significant digits (16th digit ≥ 5 rounds up, carrying
/// leftward), then strip trailing zeros — reproduces Excel's display precision.
fn round15(mut digits: Vec<u8>, mut dec_exp: i32) -> (Vec<u8>, i32) {
    if digits.len() > 15 {
        let mut res: Vec<u8> = digits[..15].to_vec();
        if digits[15] >= 5 {
            let mut carry = 1u8;
            for i in (0..15).rev() {
                let x = res[i] + carry;
                if x >= 10 {
                    res[i] = x - 10;
                    carry = 1;
                } else {
                    res[i] = x;
                    carry = 0;
                    break;
                }
            }
            if carry == 1 {
                res.insert(0, 1);
                dec_exp += 1;
            }
        }
        digits = res;
    }
    while digits.last() == Some(&0) {
        digits.pop();
    }
    (digits, dec_exp)
}

/// Split rounded digits into integer and fraction strings (the point sits
/// after `dec_exp` digits; `dec_exp ≤ 0` means leading zeros in the fraction).
fn split_digits(digits: &[u8], dec_exp: i32) -> (String, String) {
    let len = digits.len() as i32;
    let mut int = String::new();
    let mut frac = String::new();
    if dec_exp <= 0 {
        for _ in 0..(-dec_exp) {
            frac.push('0');
        }
        for &d in digits {
            frac.push(char::from(b'0' + d));
        }
    } else if dec_exp >= len {
        for &d in digits {
            int.push(char::from(b'0' + d));
        }
        for _ in 0..(dec_exp - len) {
            int.push('0');
        }
    } else {
        for &d in &digits[..dec_exp as usize] {
            int.push(char::from(b'0' + d));
        }
        for &d in &digits[dec_exp as usize..] {
            frac.push(char::from(b'0' + d));
        }
    }
    (int, frac)
}

/// Insert a thousands separator every three digits from the right. ASCII digits
/// only, so byte slicing is safe. Excel's grouping size is locale-driven (3 in
/// a US locale); the pattern commas merely enable grouping.
fn group_digits(s: &str) -> String {
    let len = s.len();
    if len <= 3 {
        return s.to_string();
    }
    let head = {
        let h = len % 3;
        if h == 0 { 3 } else { h }
    };
    let mut out = String::with_capacity(len + len / 3 + 1);
    out.push_str(&s[..head]);
    let mut i = head;
    while i < len {
        out.push(',');
        out.push_str(&s[i..i + 3]);
        i += 3;
    }
    out
}

/// One parsed TEXT-format segment. A `Lit` is emitted verbatim; a `Num` is a
/// digit pattern (`0` mandatory, `#` optional, `,` grouping, `.` point) with a
/// trailing percentage multiplier, and re-formats the whole number each time it
/// appears (so `"0-0"` renders `5-5`, as Excel does).
enum TextSeg {
    Lit(String),
    Num(NumPat),
}

struct NumPat {
    int: String,
    frac: String,
    in_frac: bool,
    percent: usize,
}

fn flush_lit(segs: &mut Vec<TextSeg>, lit: &mut String) {
    if !lit.is_empty() {
        segs.push(TextSeg::Lit(std::mem::take(lit)));
    }
}

fn flush_num(segs: &mut Vec<TextSeg>, num: &mut Option<NumPat>) {
    if let Some(p) = num.take() {
        segs.push(TextSeg::Num(p));
    }
}

/// Parse a TEXT format string into segments. Digit placeholders and their
/// grouping/decimal/percent markers form `Num` segments; everything else
/// (`"..."` quoted runs, `\x` escapes, currency symbols and ordinary text)
/// becomes literal output. Unsupported format machinery — scientific `E±`,
/// `@`, `?`, fraction `/`, section `;`, fill `*`, skip `_`, and `[..]` — is
/// `#VALUE!` rather than a guessed rendering.
fn parse_text_format(f: &str) -> Result<Vec<TextSeg>, CalcError> {
    let chars: Vec<char> = f.chars().collect();
    let mut segs: Vec<TextSeg> = Vec::new();
    let mut lit = String::new();
    let mut num: Option<NumPat> = None;
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        match ch {
            '0' | '#' => {
                flush_lit(&mut segs, &mut lit);
                let p = num.get_or_insert_with(|| NumPat {
                    int: String::new(),
                    frac: String::new(),
                    in_frac: false,
                    percent: 0,
                });
                if p.in_frac {
                    p.frac.push(ch);
                } else {
                    p.int.push(ch);
                }
                i += 1;
            }
            '.' => {
                flush_lit(&mut segs, &mut lit);
                let p = num.get_or_insert_with(|| NumPat {
                    int: String::new(),
                    frac: String::new(),
                    in_frac: false,
                    percent: 0,
                });
                if p.in_frac {
                    return Err(CalcError::Value);
                }
                p.in_frac = true;
                i += 1;
            }
            ',' => {
                flush_lit(&mut segs, &mut lit);
                let p = num.get_or_insert_with(|| NumPat {
                    int: String::new(),
                    frac: String::new(),
                    in_frac: false,
                    percent: 0,
                });
                if p.in_frac {
                    return Err(CalcError::Value);
                }
                p.int.push(',');
                i += 1;
            }
            '%' => {
                flush_lit(&mut segs, &mut lit);
                let p = num.get_or_insert_with(|| NumPat {
                    int: String::new(),
                    frac: String::new(),
                    in_frac: false,
                    percent: 0,
                });
                p.percent += 1;
                i += 1;
            }
            '"' => {
                flush_num(&mut segs, &mut num);
                i += 1;
                let mut closed = false;
                while i < chars.len() {
                    if chars[i] == '"' {
                        if i + 1 < chars.len() && chars[i + 1] == '"' {
                            lit.push('"');
                            i += 2;
                            continue;
                        }
                        closed = true;
                        i += 1;
                        break;
                    }
                    lit.push(chars[i]);
                    i += 1;
                }
                if !closed {
                    return Err(CalcError::Value);
                }
            }
            '\\' => {
                flush_num(&mut segs, &mut num);
                i += 1;
                if i >= chars.len() {
                    return Err(CalcError::Value);
                }
                lit.push(chars[i]);
                i += 1;
            }
            'E' | 'e' => {
                // scientific notation is not reproduced; a literal e followed
                // by a sign would be misread as an exponent, so reject it.
                if i + 1 < chars.len() && (chars[i + 1] == '+' || chars[i + 1] == '-') {
                    return Err(CalcError::Value);
                }
                flush_num(&mut segs, &mut num);
                lit.push(ch);
                i += 1;
            }
            '@' | '?' | ';' | '[' | ']' | '*' | '_' | '/' => {
                return Err(CalcError::Value);
            }
            _ => {
                flush_num(&mut segs, &mut num);
                lit.push(ch);
                i += 1;
            }
        }
    }
    flush_lit(&mut segs, &mut lit);
    flush_num(&mut segs, &mut num);
    Ok(segs)
}

/// Render one digit-pattern segment for `n`.
fn render_num(n: f64, p: &NumPat) -> Result<String, CalcError> {
    let min_int = p.int.chars().filter(|&c| c == '0').count();
    let grouped = p.int.contains(',');
    let frac_max = p.frac.chars().count();
    let frac_min = p.frac.chars().filter(|&c| c == '0').count();

    let value = n.abs() * 100f64.powi(p.percent as i32);
    if !value.is_finite() {
        return Err(CalcError::Num);
    }

    let rounded = if frac_max > 0 {
        let scale = 10f64.powi(frac_max as i32);
        let s = (value * scale).round();
        if s.is_finite() {
            s / scale
        } else {
            value.round()
        }
    } else {
        value.round()
    };
    if !rounded.is_finite() {
        return Err(CalcError::Num);
    }

    let (digits, dec_exp) = shortest_digits(rounded);
    let (digits, dec_exp) = round15(digits, dec_exp);
    let (mut int, frac) = split_digits(&digits, dec_exp);

    if min_int == 0 {
        let t = int.trim_start_matches('0');
        int = t.to_string();
    } else {
        while int.len() < min_int {
            int.insert(0, '0');
        }
    }
    if grouped {
        int = group_digits(&int);
    }

    let mut frac_out = frac;
    if frac_out.len() > frac_max {
        frac_out.truncate(frac_max);
    }
    while frac_out.len() < frac_max {
        frac_out.push('0');
    }
    while frac_out.len() > frac_min && frac_out.ends_with('0') {
        frac_out.pop();
    }

    let mut out = String::new();
    out.push_str(&int);
    if !frac_out.is_empty() {
        out.push('.');
        out.push_str(&frac_out);
    }
    for _ in 0..p.percent {
        out.push('%');
    }
    Ok(out)
}

/// TEXT formatting for the format codes this engine reproduces exactly:
/// `0`/`0.00`-style digit patterns (`0` mandatory, `#` optional), a comma
/// thousands-grouping pattern, a trailing `%` (each `%` scales by 100), and
/// `General` via `number_to_general`. Literal text (currency symbols, quoted
/// runs, `\x` escapes) passes through unchanged — measured:
/// `TEXT(111, "$#,##0.00") = "$111.00"`. Anything else returns `#VALUE!`.
fn text_format(n: f64, format: &str) -> Result<String, CalcError> {
    if !n.is_finite() {
        return Err(CalcError::Num);
    }
    let f = format.trim();
    if f.eq_ignore_ascii_case("general") {
        return Ok(number_to_general(n));
    }
    let segs = parse_text_format(f)?;
    let has_digit = segs.iter().any(|s| match s {
        TextSeg::Num(p) => !p.int.is_empty() || !p.frac.is_empty(),
        TextSeg::Lit(_) => false,
    });
    if !has_digit {
        return Err(CalcError::Value);
    }
    let mut out = String::new();
    for seg in &segs {
        match seg {
            TextSeg::Lit(s) => out.push_str(s),
            TextSeg::Num(p) => out.push_str(&render_num(n, p)?),
        }
    }
    // A negative number's minus sign precedes the whole formatted string,
    // currency symbols and all: TEXT(-111, "$#,##0.00") = "-$111.00".
    if n < 0.0 {
        out.insert(0, '-');
    }
    Ok(out)
}

fn text(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    let format = coerce_text(&args[1].value(ctx)?)?;
    Ok(CalcValue::text(text_format(n, &format)?))
}

fn concat(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let mut out = String::new();
    for arg in args {
        match arg.value(ctx)? {
            CalcValue::Array(a) => {
                for v in a.iter() {
                    out.push_str(&coerce_text(v)?);
                }
            }
            v => out.push_str(&coerce_text(&v)?),
        }
    }
    Ok(CalcValue::text(out))
}

fn concatenate(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let mut out = String::new();
    for arg in args {
        out.push_str(&coerce_text(&arg.value(ctx)?)?);
    }
    Ok(CalcValue::text(out))
}

fn ignore_empty_bool(v: &CalcValue) -> Result<bool, CalcError> {
    match v {
        CalcValue::Bool(b) => Ok(*b),
        CalcValue::Error(e) => Err(*e),
        CalcValue::Blank => Ok(false),
        _ => Ok(coerce_number(v)? != 0.0),
    }
}

fn textjoin(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let delim = coerce_text(&args[0].value(ctx)?)?;
    let ignore_empty = ignore_empty_bool(&args[1].value(ctx)?)?;
    let mut parts: Vec<String> = Vec::new();
    for arg in &args[2..] {
        match arg.value(ctx)? {
            CalcValue::Array(a) => {
                for v in a.iter() {
                    let s = coerce_text(v)?;
                    if !(ignore_empty && s.is_empty()) {
                        parts.push(s);
                    }
                }
            }
            v => {
                let s = coerce_text(&v)?;
                if !(ignore_empty && s.is_empty()) {
                    parts.push(s);
                }
            }
        }
    }
    Ok(CalcValue::text(parts.join(&delim)))
}

fn rept(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let text = coerce_text(&args[0].value(ctx)?)?;
    let count = trunc_arg(&args[1].value(ctx)?)?;
    if count < 0.0 {
        return Err(CalcError::Value);
    }
    if text.chars().count() as f64 * count > 32767.0 {
        return Err(CalcError::Value);
    }
    Ok(CalcValue::text(text.repeat(count as usize)))
}

fn exact(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let a = coerce_text(&args[0].value(ctx)?)?;
    let b = coerce_text(&args[1].value(ctx)?)?;
    Ok(CalcValue::bool(a == b))
}

/// Windows-1252 for 0x80–0x9F (the range where cp1252 differs from Latin-1);
/// the rest is Latin-1 identity. Matches Windows Excel's ANSI code page.
fn cp1252_to_char(code: u8) -> char {
    match code {
        0x80 => '\u{20AC}',
        0x82 => '\u{201A}',
        0x83 => '\u{0192}',
        0x84 => '\u{201E}',
        0x85 => '\u{2026}',
        0x86 => '\u{2020}',
        0x87 => '\u{2021}',
        0x88 => '\u{02C6}',
        0x89 => '\u{2030}',
        0x8A => '\u{0160}',
        0x8B => '\u{2039}',
        0x8C => '\u{0152}',
        0x8E => '\u{017D}',
        0x91 => '\u{2018}',
        0x92 => '\u{2019}',
        0x93 => '\u{201C}',
        0x94 => '\u{201D}',
        0x95 => '\u{2022}',
        0x96 => '\u{2013}',
        0x97 => '\u{2014}',
        0x98 => '\u{02DC}',
        0x99 => '\u{2122}',
        0x9A => '\u{0161}',
        0x9B => '\u{203A}',
        0x9C => '\u{0153}',
        0x9E => '\u{017E}',
        0x9F => '\u{0178}',
        _ => char::from(code),
    }
}

/// Reverse of `cp1252_to_char`; characters outside the code page return their
/// Unicode scalar value (Excel's behavior for non-ANSI characters).
fn char_to_code(ch: char) -> u32 {
    match ch {
        '\u{20AC}' => 0x80,
        '\u{201A}' => 0x82,
        '\u{0192}' => 0x83,
        '\u{201E}' => 0x84,
        '\u{2026}' => 0x85,
        '\u{2020}' => 0x86,
        '\u{2021}' => 0x87,
        '\u{02C6}' => 0x88,
        '\u{2030}' => 0x89,
        '\u{0160}' => 0x8A,
        '\u{2039}' => 0x8B,
        '\u{0152}' => 0x8C,
        '\u{017D}' => 0x8E,
        '\u{2018}' => 0x91,
        '\u{2019}' => 0x92,
        '\u{201C}' => 0x93,
        '\u{201D}' => 0x94,
        '\u{2022}' => 0x95,
        '\u{2013}' => 0x96,
        '\u{2014}' => 0x97,
        '\u{02DC}' => 0x98,
        '\u{2122}' => 0x99,
        '\u{0161}' => 0x9A,
        '\u{203A}' => 0x9B,
        '\u{0153}' => 0x9C,
        '\u{017E}' => 0x9E,
        '\u{0178}' => 0x9F,
        c => c as u32,
    }
}

fn char_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let code = trunc_arg(&args[0].value(ctx)?)?;
    if code < 1.0 || code > 255.0 {
        return Err(CalcError::Value);
    }
    Ok(CalcValue::text(cp1252_to_char(code as u8).to_string()))
}

fn code(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let text = coerce_text(&args[0].value(ctx)?)?;
    let first = text.chars().next().ok_or(CalcError::Value)?;
    ok_num(char_to_code(first) as f64)
}

fn t(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    match args[0].value(ctx)? {
        CalcValue::Text(s) => Ok(CalcValue::text(s)),
        CalcValue::Error(e) => Err(e),
        CalcValue::Array(_) => Err(CalcError::Value),
        _ => Ok(CalcValue::text("")),
    }
}

// -- TEXTBEFORE / TEXTAFTER ---------------------------------------------------

/// TEXTBEFORE / TEXTAFTER share one engine: find the delimiter occurrences in
/// `text` and slice on the requested instance.
///
/// Args: `(text, delimiter, [instance_num], [match_mode], [match_end],
/// [if_not_found])`.
///   * A negative `instance_num` counts from the end (‑1 = last occurrence);
///     0 is #VALUE!.
///   * `match_mode` 1 makes the search case-insensitive; anything but 0/1 is
///     #VALUE!.
///   * `match_end` 1 treats the end of `text` as one more delimiter position,
///     so a delimiter missing from the text yields the whole text (TEXTBEFORE)
///     or "" (TEXTAFTER) instead of #N/A.
///   * Without `if_not_found`, a genuinely missing delimiter is #N/A.
fn text_split(ctx: &FuncCtx, args: &[FuncArg], after: bool) -> Result<CalcValue, CalcError> {
    let text = coerce_text(&args[0].value(ctx)?)?;
    let delim = coerce_text(&args[1].value(ctx)?)?;
    if delim.is_empty() {
        return Err(CalcError::Value);
    }
    let instance = if args.len() >= 3 {
        let n = trunc_arg(&args[2].value(ctx)?)?;
        if n == 0.0 {
            return Err(CalcError::Value);
        }
        n as i64
    } else {
        1
    };
    let match_mode = if args.len() >= 4 {
        let m = coerce_number(&args[3].value(ctx)?)?;
        if m != 0.0 && m != 1.0 {
            return Err(CalcError::Value);
        }
        m != 0.0
    } else {
        false
    };
    let match_end = if args.len() >= 5 {
        let m = coerce_number(&args[4].value(ctx)?)?;
        if m != 0.0 && m != 1.0 {
            return Err(CalcError::Value);
        }
        m != 0.0
    } else {
        false
    };
    let if_not_found = if args.len() >= 6 {
        Some(args[5].value(ctx)?)
    } else {
        None
    };

    let hay: Vec<char> = text.chars().collect();
    let needle: Vec<char> = delim.chars().collect();
    let n = hay.len();
    let m = needle.len();
    let mut pos: Vec<usize> = Vec::new();
    if m <= n {
        if match_mode {
            for i in 0..=(n - m) {
                let mut ok = true;
                for j in 0..m {
                    if !hay[i + j].eq_ignore_ascii_case(&needle[j]) {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    pos.push(i);
                }
            }
        } else {
            for i in 0..=(n - m) {
                if hay[i..i + m] == needle[..] {
                    pos.push(i);
                }
            }
        }
    }
    if match_end {
        pos.push(n);
    }
    let k = pos.len();
    let pick: Option<usize> = if instance > 0 {
        pos.get(instance as usize - 1).copied()
    } else {
        let from_end = (-instance) as usize;
        if from_end <= k {
            Some(pos[k - from_end])
        } else {
            None
        }
    };
    match pick {
        Some(p) => {
            if after {
                if p + m <= n {
                    Ok(CalcValue::text(hay[p + m..].iter().collect::<String>()))
                } else {
                    Ok(CalcValue::text(""))
                }
            } else {
                Ok(CalcValue::text(hay[..p].iter().collect::<String>()))
            }
        }
        None => Ok(match if_not_found {
            Some(v) => v,
            None => CalcValue::err(CalcError::Na),
        }),
    }
}

fn textbefore(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    text_split(ctx, args, false)
}

fn textafter(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    text_split(ctx, args, true)
}

// -- NUMBERVALUE / FIXED / DOLLAR --------------------------------------------

/// NUMBERVALUE(text, [decimal_separator], [group_separator]): parse `text`
/// into a number honouring explicit separators, so "1.234,56" parses with
/// decimal `,` and group `.`. Leading/trailing spaces are ignored, a trailing
/// `%` divides by 100, and a group separator must split the integer part into
/// three-digit groups from the right (the fraction must contain no group
/// separator). Equal separators, a second decimal separator, malformed text
/// and the empty string are all #VALUE!.
fn numbervalue(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let text = coerce_text(&args[0].value(ctx)?)?;
    let dec = if args.len() >= 2 {
        coerce_text(&args[1].value(ctx)?)?
    } else {
        ".".to_string()
    };
    let grp = if args.len() >= 3 {
        coerce_text(&args[2].value(ctx)?)?
    } else {
        ",".to_string()
    };
    if dec.is_empty() || grp.is_empty() {
        return Err(CalcError::Value);
    }
    let dec = dec.chars().next().unwrap();
    let grp = grp.chars().next().unwrap();
    if dec == grp {
        return Err(CalcError::Value);
    }

    let t = text.trim();
    if t.is_empty() {
        return Err(CalcError::Value);
    }
    let (neg, rest) = match t.strip_prefix('-') {
        Some(r) => (true, r.trim()),
        None => (false, t.strip_prefix('+').unwrap_or(t).trim()),
    };
    // Every trailing `%` divides by 100 once — NUMBERVALUE("20%%") = 0.002.
    let mut pct = 0usize;
    let mut mant = rest;
    while let Some(r) = mant.strip_suffix('%') {
        pct += 1;
        mant = r.trim();
    }
    if mant.is_empty() {
        return Err(CalcError::Value);
    }

    let mut dec_parts = mant.split(dec);
    let int_part = dec_parts.next().unwrap_or("");
    let frac: &str = match dec_parts.next() {
        Some(f) => {
            if dec_parts.next().is_some() {
                return Err(CalcError::Value);
            }
            f
        }
        None => "",
    };
    if frac.contains(grp) || !frac.chars().all(|c| c.is_ascii_digit()) {
        return Err(CalcError::Value);
    }

    let mut int_digits = String::new();
    if !int_part.is_empty() {
        let groups: Vec<&str> = int_part.split(grp).collect();
        if groups
            .iter()
            .any(|g| g.is_empty() || !g.chars().all(|c| c.is_ascii_digit()))
        {
            return Err(CalcError::Value);
        }
        if groups.len() > 1 {
            if groups[0].len() > 3 {
                return Err(CalcError::Value);
            }
            if groups[1..].iter().any(|g| g.len() != 3) {
                return Err(CalcError::Value);
            }
        }
        for g in groups {
            int_digits.push_str(g);
        }
    }
    if int_digits.is_empty() && frac.is_empty() {
        return Err(CalcError::Value);
    }

    let mut num_str = String::with_capacity(int_digits.len() + frac.len() + 2);
    if neg {
        num_str.push('-');
    }
    if int_digits.is_empty() {
        num_str.push('0');
    } else {
        num_str.push_str(&int_digits);
    }
    if !frac.is_empty() {
        num_str.push('.');
        num_str.push_str(frac);
    }
    let n: f64 = num_str.parse().map_err(|_| CalcError::Value)?;
    ok_num(if pct > 0 {
        n / 100f64.powi(pct as i32)
    } else {
        n
    })
}

/// The FIXED/DOLLAR formatter: round `n` to `decimals` places (a negative
/// `decimals` rounds to the left of the decimal point), then emit a fixed
/// string with exactly `decimals` fraction digits and thousands separators
/// unless `no_commas`. Reuses the TEXT machinery so rounding agrees with
/// `TEXT`. Excel's format codes are length-bounded, so `decimals` outside
/// ±127 is `#VALUE!` (measured: `FIXED(1234.567, 128)` and `DOLLAR(x, 128)`).
fn fixed_impl(n: f64, decimals: i32, no_commas: bool) -> Result<String, CalcError> {
    if !n.is_finite() {
        return Err(CalcError::Num);
    }
    if !(-127..=127).contains(&decimals) {
        return Err(CalcError::Value);
    }
    let value = if decimals < 0 {
        let scale = 10f64.powi((-decimals) as i32);
        let r = (n / scale).round() * scale;
        if !r.is_finite() {
            return Err(CalcError::Num);
        }
        r
    } else {
        n
    };
    let d = decimals.max(0) as usize;
    let mut pat = if no_commas {
        String::from("0")
    } else {
        String::from("#,##0")
    };
    if d > 0 {
        pat.push('.');
        for _ in 0..d {
            pat.push('0');
        }
    }
    text_format(value, &pat)
}

fn fixed(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    let decimals = if args.len() >= 2 {
        trunc_arg(&args[1].value(ctx)?)?
    } else {
        2.0
    };
    let no_commas = if args.len() >= 3 {
        ignore_empty_bool(&args[2].value(ctx)?)?
    } else {
        false
    };
    Ok(CalcValue::text(fixed_impl(n, decimals as i32, no_commas)?))
}

/// Currency-prefix table for DOLLAR, keyed by locale. kyrax targets the
/// measured en-IN locale (Indian Rupee, a trailing-space separator), so that
/// is the default entry. The function never assumes a US dollar sign — a
/// future locale binding only adds a row to this table.
const CURRENCY_BY_LOCALE: &[(&str, &str)] = &[("en-IN", "₹ ")];

/// The active locale for currency rendering. The engine is measured against
/// en-IN Excel, so that is the default; the lookup exists so a locale-aware
/// front-end can select another row of `CURRENCY_BY_LOCALE` later.
const DEFAULT_LOCALE: &str = "en-IN";

fn currency_prefix() -> &'static str {
    CURRENCY_BY_LOCALE
        .iter()
        .find(|(loc, _)| *loc == DEFAULT_LOCALE)
        .map(|(_, s)| *s)
        .unwrap_or("₹ ")
}

fn dollar(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    let decimals = if args.len() >= 2 {
        trunc_arg(&args[1].value(ctx)?)?
    } else {
        2.0
    };
    let body = fixed_impl(n, decimals as i32, false)?;
    let prefix = currency_prefix();
    let out = match body.strip_prefix('-') {
        Some(rest) => format!("-{prefix}{rest}"),
        None => format!("{prefix}{body}"),
    };
    Ok(CalcValue::text(out))
}

// -- ASC / DBCS (full-width ⇄ half-width) -------------------------------------

/// ASC converts full-width ASCII-range characters to half-width: the
/// ideographic space U+3000 to a normal space and the full-width ASCII block
/// U+FF01–U+FF5E down to U+0021–U+007E. Double-byte katakana conversion needs
/// locale tables this engine does not carry, so katakana passes through
/// untouched — the documented partial, not a guessed table.
fn asc(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{3000}' => ' ',
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            _ => c,
        })
        .collect()
}

fn asc_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let text = coerce_text(&args[0].value(ctx)?)?;
    Ok(CalcValue::text(asc(&text)))
}

/// DBCS is ASC's inverse: a half-width space and the ASCII block U+0021–U+007E
/// widen to U+3000 / U+FF01–U+FF5E. Katakana conversion is likewise not
/// carried.
fn dbcs_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let text = coerce_text(&args[0].value(ctx)?)?;
    let out: String = text
        .chars()
        .map(|c| match c {
            ' ' => '\u{3000}',
            '\u{0021}'..='\u{007E}' => char::from_u32(c as u32 + 0xFEE0).unwrap_or(c),
            _ => c,
        })
        .collect();
    Ok(CalcValue::text(out))
}

// -- BAHTTEXT (Thai) ----------------------------------------------------------

/// Thai digit words for 0..10.
const THAI_DIGITS: [&str; 10] = [
    "ศูนย์",
    "หนึ่ง",
    "สอง",
    "สาม",
    "สี่",
    "ห้า",
    "หก",
    "เจ็ด",
    "แปด",
    "เก้า",
];

/// Place words for a 6-digit group (units, tens, hundreds, thousands,
/// ten-thousands, hundred-thousands).
const THAI_PLACE: [&str; 6] = ["", "สิบ", "ร้อย", "พัน", "หมื่น", "แสน"];

/// Render one 6-digit group (0..999999) in Thai. Tens digit 1 reads "สิบ"
/// without "หนึ่ง", tens digit 2 reads "ยี่สิบ", and a ones digit 1 with any
/// higher non-zero digit reads "เอ็ด".
fn thai_group(g: i64) -> String {
    if g == 0 {
        return String::new();
    }
    let digits: [i64; 6] = [
        g % 10,
        (g / 10) % 10,
        (g / 100) % 10,
        (g / 1000) % 10,
        (g / 10000) % 10,
        (g / 100000) % 10,
    ];
    let higher = digits[1..].iter().any(|&d| d != 0);
    let mut out = String::new();
    for i in (0..6).rev() {
        let d = digits[i];
        if d == 0 {
            continue;
        }
        match i {
            1 => {
                if d == 1 {
                    out.push_str("สิบ");
                } else if d == 2 {
                    out.push_str("ยี่สิบ");
                } else {
                    out.push_str(THAI_DIGITS[d as usize]);
                    out.push_str("สิบ");
                }
            }
            0 => {
                if d == 1 && higher {
                    out.push_str("เอ็ด");
                } else {
                    out.push_str(THAI_DIGITS[d as usize]);
                }
            }
            _ => {
                out.push_str(THAI_DIGITS[d as usize]);
                out.push_str(THAI_PLACE[i]);
            }
        }
    }
    out
}

/// Convert a non-negative integer to Thai words. Groups of six digits each
/// carry one "ล้าน" per level above the first (10^6, 10^12, ...).
fn thai_num(mut n: u64) -> String {
    if n == 0 {
        return "ศูนย์".to_string();
    }
    let mut parts: Vec<String> = Vec::new();
    let mut level = 0usize;
    while n > 0 {
        let g = (n % 1_000_000) as i64;
        n /= 1_000_000;
        if g > 0 {
            let mut s = thai_group(g);
            for _ in 0..level {
                s.push_str("ล้าน");
            }
            parts.push(s);
        }
        level += 1;
    }
    parts.reverse();
    parts.concat()
}

/// BAHTTEXT renders an amount in Thai words: the integer part with "บาท"
/// (baht) and, when the rounded two-decimal fraction is non-zero, the satang
/// part with "สตางค์". Negative amounts carry a "ลบ" prefix (Thai "minus").
fn bahttext(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    if !n.is_finite() {
        return Err(CalcError::Num);
    }
    let neg = n < 0.0;
    let scaled = (n.abs() * 100.0).round();
    let mut baht = (scaled / 100.0).floor() as u64;
    let mut satang = (scaled % 100.0) as u64;
    if satang == 100 {
        satang = 0;
        baht += 1;
    }
    let mut out = String::new();
    if neg {
        out.push_str("ลบ");
    }
    out.push_str(&thai_num(baht));
    out.push_str("บาท");
    if satang > 0 {
        out.push_str(&thai_num(satang));
        out.push_str("สตางค์");
    }
    Ok(CalcValue::text(out))
}

// -- NUMBERSTRING (Chinese) ---------------------------------------------------

/// NUMBERSTRING renders an integer in one of three Chinese forms: type 1 is
/// the ordinary lower-case numerals ("一百二十三"), type 2 the financial
/// upper-case numerals ("壹佰贰拾叁"), type 3 one digit word per digit
/// ("一二三").
///
/// `leading_one` controls whether a leading "10" reads "十" (type 1) or keeps
/// the digit "壹拾" (type 2, financial).
fn chinese_number(digits: &[u32], upper: bool, leading_one: bool) -> String {
    const LOW: [&str; 10] = ["零", "一", "二", "三", "四", "五", "六", "七", "八", "九"];
    const UPP: [&str; 10] = ["零", "壹", "贰", "叁", "肆", "伍", "陆", "柒", "捌", "玖"];
    // Place words within a 4-digit group, indexed by `pos % 4`.
    const WITHIN_LOW: [&str; 4] = ["", "十", "百", "千"];
    const WITHIN_UPP: [&str; 4] = ["", "拾", "佰", "仟"];
    // Group names after each full 4-digit group, indexed by `pos / 4`.
    const GROUP_LOW: [&str; 5] = ["", "万", "亿", "兆", "京"];
    const GROUP_UPP: [&str; 5] = ["", "萬", "億", "兆", "京"];

    let (d, within, group): (&[&str; 10], &[&str; 4], &[&str; 5]) = if upper {
        (&UPP, &WITHIN_UPP, &GROUP_UPP)
    } else {
        (&LOW, &WITHIN_LOW, &GROUP_LOW)
    };

    if digits.iter().all(|&x| x == 0) {
        return d[0].to_string();
    }
    let n = digits.len();
    let mut hi = 0usize;
    for (i, &digit) in digits.iter().enumerate() {
        if digit != 0 {
            hi = n - 1 - i;
            break;
        }
    }
    // A leading "10" / "10万" / "10亿" drops the "一" in the lower form only:
    // 12 reads 十二 but the financial form keeps 壹拾贰.
    let leading_tens = leading_one && hi % 4 == 1 && digits[n - 1 - hi] == 1;
    let mut lowest = [usize::MAX; 5];
    for (i, &digit) in digits.iter().enumerate() {
        if digit != 0 {
            let pos = n - 1 - i;
            lowest[pos / 4] = lowest[pos / 4].min(pos);
        }
    }
    let mut out = String::new();
    let mut pending_zero = false;
    for i in 0..n {
        let pos = n - 1 - i;
        let digit = digits[i];
        if digit == 0 {
            if !out.is_empty() {
                pending_zero = true;
            }
            continue;
        }
        if pending_zero {
            out.push_str(d[0]);
            pending_zero = false;
        }
        if pos == hi && leading_tens {
            out.push_str(within[1]);
        } else {
            out.push_str(d[digit as usize]);
            out.push_str(within[pos % 4]);
        }
        if lowest[pos / 4] == pos {
            out.push_str(group[pos / 4]);
        }
    }
    out
}

fn numberstring(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = coerce_number(&args[0].value(ctx)?)?;
    let kind = trunc_arg(&args[1].value(ctx)?)?;
    if kind < 1.0 || kind > 3.0 {
        return Err(CalcError::Value);
    }
    if !n.is_finite() {
        return Err(CalcError::Num);
    }
    let i = n.trunc();
    let neg = i < 0.0;
    let digits: Vec<u32> = i
        .abs()
        .to_string()
        .chars()
        .map(|c| c.to_digit(10).unwrap_or(0))
        .collect();
    let out = match kind as i32 {
        1 => chinese_number(&digits, false, true),
        2 => chinese_number(&digits, true, false),
        _ => digits
            .iter()
            .map(|&d| chinese_digit_raw(d))
            .collect::<String>(),
    };
    Ok(CalcValue::text(if neg { format!("-{out}") } else { out }))
}

fn chinese_digit_raw(d: u32) -> &'static str {
    ["零", "一", "二", "三", "四", "五", "六", "七", "八", "九"][d as usize]
}

// -- CLEAN / UNICHAR / UNICODE -----------------------------------------------

/// CLEAN strips only the ASCII control characters 0–31, matching Excel; DEL
/// (127) and other non-printable Unicode are preserved.
fn clean(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let text = coerce_text(&args[0].value(ctx)?)?;
    let out: String = text.chars().filter(|&c| u32::from(c) > 31).collect();
    Ok(CalcValue::text(out))
}

/// UNICHAR of 0 or a surrogate code point is #VALUE!; every other code point
/// in 1..=0x10FFFF maps to its Unicode character.
fn unichar(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let code = trunc_arg(&args[0].value(ctx)?)?;
    if code < 1.0 || code > 0x10_FFFF as f64 {
        return Err(CalcError::Value);
    }
    let u = code as u32;
    if (0xD800..=0xDFFF).contains(&u) {
        return Err(CalcError::Value);
    }
    match char::from_u32(u) {
        Some(c) => Ok(CalcValue::text(c.to_string())),
        None => Err(CalcError::Value),
    }
}

/// UNICODE returns the code point of the first character (where CODE is
/// limited to the ANSI code page). The empty string is #VALUE!.
fn unicode(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let text = coerce_text(&args[0].value(ctx)?)?;
    let c = text.chars().next().ok_or(CalcError::Value)?;
    ok_num(u32::from(c) as f64)
}

// -- VALUETOTEXT / ARRAYTOTEXT -----------------------------------------------

/// Render one value as its text form. format 0 (compact) uses General for
/// numbers, the raw text for strings, TRUE/FALSE for booleans and the error
/// code for errors. format 1 (strict) additionally quotes text, doubling
/// internal quotes, so the result could stand inside a formula.
fn value_to_text(v: &CalcValue, strict: bool) -> String {
    match v {
        CalcValue::Number(n) => number_to_general(*n),
        CalcValue::Text(t) => {
            if strict {
                let mut s = String::with_capacity(t.len() + 2);
                s.push('"');
                for ch in t.chars() {
                    if ch == '"' {
                        s.push('"');
                    }
                    s.push(ch);
                }
                s.push('"');
                s
            } else {
                t.to_string()
            }
        }
        CalcValue::Bool(b) => (if *b { "TRUE" } else { "FALSE" }).to_string(),
        CalcValue::Blank => String::new(),
        CalcValue::Error(e) => e.code().to_string(),
        // Unreachable through eval (the loop scalarizes arrays first for this
        // function); kept for a total match.
        CalcValue::Array(a) => format!("Array({}x{})", a.rows, a.cols),
    }
}

fn valuetotext(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let format = if args.len() >= 2 {
        trunc_arg(&args[1].value(ctx)?)?
    } else {
        0.0
    };
    if format != 0.0 && format != 1.0 {
        return Err(CalcError::Value);
    }
    let v = args[0].value(ctx)?;
    Ok(CalcValue::text(value_to_text(&v, format == 1.0)))
}

/// ARRAYTOTEXT joins every element (row-major for a 2-D array, a scalar as a
/// 1-element array) with `", "` — measured en-IN Excel:
/// `ARRAYTOTEXT({1," ",1.23,TRUE,FALSE}) = "1,  , 1.23, TRUE, FALSE"`. The 0/1
/// format argument is the same as VALUETOTEXT (strict quotes strings and
/// doubles internal quotes).
fn arraytotext(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let format = if args.len() >= 2 {
        trunc_arg(&args[1].value(ctx)?)?
    } else {
        0.0
    };
    if format != 0.0 && format != 1.0 {
        return Err(CalcError::Value);
    }
    let strict = format == 1.0;
    let v = args[0].value(ctx)?;
    let mut parts: Vec<String> = Vec::new();
    match &v {
        CalcValue::Array(a) => {
            for r in 0..a.rows {
                for c in 0..a.cols {
                    parts.push(value_to_text(a.get(r, c), strict));
                }
            }
        }
        other => parts.push(value_to_text(other, strict)),
    }
    Ok(CalcValue::text(parts.join(", ")))
}

const LEFT: FuncSpec = FuncSpec {
    name: "LEFT",
    min_args: 1,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: left,
};

const RIGHT: FuncSpec = FuncSpec {
    name: "RIGHT",
    min_args: 1,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: right,
};

const MID: FuncSpec = FuncSpec {
    name: "MID",
    min_args: 3,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: mid,
};

const LEN: FuncSpec = FuncSpec {
    name: "LEN",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: len,
};

const TRIM: FuncSpec = FuncSpec {
    name: "TRIM",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: trim,
};

const UPPER: FuncSpec = FuncSpec {
    name: "UPPER",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: upper,
};

const LOWER: FuncSpec = FuncSpec {
    name: "LOWER",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: lower,
};

const PROPER: FuncSpec = FuncSpec {
    name: "PROPER",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: proper_fn,
};

const FIND: FuncSpec = FuncSpec {
    name: "FIND",
    min_args: 2,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: find,
};

const SEARCH: FuncSpec = FuncSpec {
    name: "SEARCH",
    min_args: 2,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: search,
};

const SUBSTITUTE: FuncSpec = FuncSpec {
    name: "SUBSTITUTE",
    min_args: 3,
    max_args: Some(4),
    volatile: false,
    array_aware: false,
    func: substitute,
};

const REPLACE: FuncSpec = FuncSpec {
    name: "REPLACE",
    min_args: 4,
    max_args: Some(4),
    volatile: false,
    array_aware: false,
    func: replace,
};

/// REPLACEB is byte-oriented in a double-byte code page; under the engine's
/// non-DBCS byte policy (one byte per UTF-16 code unit) it equals REPLACE,
/// operating on whole characters — the documented divergence for a DBCS
/// locale, not a guessed byte split.
const REPLACEB: FuncSpec = FuncSpec {
    name: "REPLACEB",
    min_args: 4,
    max_args: Some(4),
    volatile: false,
    array_aware: false,
    func: replace,
};

const ASC: FuncSpec = FuncSpec {
    name: "ASC",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: asc_fn,
};

const DBCS: FuncSpec = FuncSpec {
    name: "DBCS",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: dbcs_fn,
};

const BAHTTEXT: FuncSpec = FuncSpec {
    name: "BAHTTEXT",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: bahttext,
};

const NUMBERSTRING: FuncSpec = FuncSpec {
    name: "NUMBERSTRING",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: numberstring,
};

const VALUE: FuncSpec = FuncSpec {
    name: "VALUE",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: value_fn,
};

const TEXT: FuncSpec = FuncSpec {
    name: "TEXT",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: text,
};

const CONCAT: FuncSpec = FuncSpec {
    name: "CONCAT",
    min_args: 1,
    max_args: None,
    volatile: false,
    array_aware: true,
    func: concat,
};

const CONCATENATE: FuncSpec = FuncSpec {
    name: "CONCATENATE",
    min_args: 1,
    max_args: None,
    volatile: false,
    array_aware: false,
    func: concatenate,
};

const TEXTJOIN: FuncSpec = FuncSpec {
    name: "TEXTJOIN",
    min_args: 3,
    max_args: None,
    volatile: false,
    array_aware: true,
    func: textjoin,
};

const REPT: FuncSpec = FuncSpec {
    name: "REPT",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: rept,
};

const EXACT: FuncSpec = FuncSpec {
    name: "EXACT",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: exact,
};

const CHAR: FuncSpec = FuncSpec {
    name: "CHAR",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: char_fn,
};

const CODE: FuncSpec = FuncSpec {
    name: "CODE",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: code,
};

const T: FuncSpec = FuncSpec {
    name: "T",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: t,
};

const TEXTBEFORE: FuncSpec = FuncSpec {
    name: "TEXTBEFORE",
    min_args: 2,
    max_args: Some(6),
    volatile: false,
    array_aware: false,
    func: textbefore,
};

const TEXTAFTER: FuncSpec = FuncSpec {
    name: "TEXTAFTER",
    min_args: 2,
    max_args: Some(6),
    volatile: false,
    array_aware: false,
    func: textafter,
};

const NUMBERVALUE: FuncSpec = FuncSpec {
    name: "NUMBERVALUE",
    min_args: 1,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: numbervalue,
};

const FIXED: FuncSpec = FuncSpec {
    name: "FIXED",
    min_args: 1,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: fixed,
};

const DOLLAR: FuncSpec = FuncSpec {
    name: "DOLLAR",
    min_args: 1,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: dollar,
};

const CLEAN: FuncSpec = FuncSpec {
    name: "CLEAN",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: clean,
};

const UNICHAR: FuncSpec = FuncSpec {
    name: "UNICHAR",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: unichar,
};

const UNICODE: FuncSpec = FuncSpec {
    name: "UNICODE",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: unicode,
};

// The `*B` functions are char-based equivalents of their non-B siblings (see
// the module docs): exact for single-byte text, documented divergence for
// DBCS locales. They share the implementation pointers.
const LEFTB: FuncSpec = FuncSpec {
    name: "LEFTB",
    min_args: 1,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: left,
};

const RIGHTB: FuncSpec = FuncSpec {
    name: "RIGHTB",
    min_args: 1,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: right,
};

const MIDB: FuncSpec = FuncSpec {
    name: "MIDB",
    min_args: 3,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: mid,
};

const LENB: FuncSpec = FuncSpec {
    name: "LENB",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: len,
};

const FINDB: FuncSpec = FuncSpec {
    name: "FINDB",
    min_args: 2,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: find,
};

const SEARCHB: FuncSpec = FuncSpec {
    name: "SEARCHB",
    min_args: 2,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: search,
};

const VALUETOTEXT: FuncSpec = FuncSpec {
    name: "VALUETOTEXT",
    min_args: 1,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: valuetotext,
};

const ARRAYTOTEXT: FuncSpec = FuncSpec {
    name: "ARRAYTOTEXT",
    min_args: 1,
    max_args: Some(2),
    volatile: false,
    array_aware: true,
    func: arraytotext,
};

pub fn register(r: &mut Registry) {
    r.register(&LEFT);
    r.register(&RIGHT);
    r.register(&MID);
    r.register(&LEN);
    r.register(&TRIM);
    r.register(&UPPER);
    r.register(&LOWER);
    r.register(&PROPER);
    r.register(&FIND);
    r.register(&SEARCH);
    r.register(&SUBSTITUTE);
    r.register(&REPLACE);
    r.register(&REPLACEB);
    r.register(&ASC);
    r.register(&DBCS);
    r.register(&BAHTTEXT);
    r.register(&NUMBERSTRING);
    r.register(&VALUE);
    r.register(&TEXT);
    r.register(&CONCAT);
    r.register(&CONCATENATE);
    r.register(&TEXTJOIN);
    r.register(&REPT);
    r.register(&EXACT);
    r.register(&CHAR);
    r.register(&CODE);
    r.register(&T);
    r.register(&TEXTBEFORE);
    r.register(&TEXTAFTER);
    r.register(&NUMBERVALUE);
    r.register(&FIXED);
    r.register(&DOLLAR);
    r.register(&CLEAN);
    r.register(&UNICHAR);
    r.register(&UNICODE);
    r.register(&LEFTB);
    r.register(&RIGHTB);
    r.register(&MIDB);
    r.register(&LENB);
    r.register(&FINDB);
    r.register(&SEARCHB);
    r.register(&VALUETOTEXT);
    r.register(&ARRAYTOTEXT);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turbo::calc::functions::CellResolver;
    use crate::turbo::calc::functions::registry;
    use crate::turbo::calc::testkit::Grid;
    use crate::turbo::calc::value::ArrayValue;

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

    fn call(spec: &'static FuncSpec, vals: Vec<CalcValue>) -> Result<CalcValue, CalcError> {
        let c = ctx();
        let args: Vec<FuncArg> = vals.into_iter().map(FuncArg::Value).collect();
        (spec.func)(&c, &args)
    }

    fn num(n: f64) -> CalcValue {
        CalcValue::number(n)
    }

    fn txt(s: &str) -> CalcValue {
        CalcValue::text(s)
    }

    fn row(data: Vec<CalcValue>) -> CalcValue {
        CalcValue::array(ArrayValue::new(1, data.len() as u32, data))
    }

    fn s(v: CalcValue) -> String {
        match v {
            CalcValue::Text(t) => t.to_string(),
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn left_right_defaults_and_bounds() {
        assert_eq!(call(&LEFT, vec![txt("hello"), num(3.0)]), Ok(txt("hel")));
        assert_eq!(call(&LEFT, vec![txt("hello")]), Ok(txt("h")));
        assert_eq!(call(&LEFT, vec![txt("hello"), num(0.0)]), Ok(txt("")));
        assert_eq!(call(&LEFT, vec![txt("hello"), num(99.0)]), Ok(txt("hello")));
        assert_eq!(
            call(&LEFT, vec![txt("hello"), num(-1.0)]),
            Err(CalcError::Value)
        );
        assert_eq!(call(&RIGHT, vec![txt("hello"), num(3.0)]), Ok(txt("llo")));
        assert_eq!(call(&RIGHT, vec![txt("hello")]), Ok(txt("o")));
        assert_eq!(
            call(&RIGHT, vec![txt("hello"), num(99.0)]),
            Ok(txt("hello"))
        );
        assert_eq!(
            call(&RIGHT, vec![txt("hello"), num(-1.0)]),
            Err(CalcError::Value)
        );
    }

    #[test]
    fn left_right_count_characters_not_bytes() {
        assert_eq!(call(&LEFT, vec![txt("héllo"), num(4.0)]), Ok(txt("héll")));
        assert_eq!(call(&RIGHT, vec![txt("héllo"), num(3.0)]), Ok(txt("llo")));
    }

    #[test]
    fn mid_semantics() {
        assert_eq!(
            call(&MID, vec![txt("hello"), num(2.0), num(3.0)]),
            Ok(txt("ell"))
        );
        assert_eq!(
            call(&MID, vec![txt("hello"), num(6.0), num(3.0)]),
            Ok(txt(""))
        );
        assert_eq!(
            call(&MID, vec![txt("hello"), num(0.0), num(3.0)]),
            Err(CalcError::Value)
        );
        assert_eq!(
            call(&MID, vec![txt("hello"), num(2.0), num(-1.0)]),
            Err(CalcError::Value)
        );
        assert_eq!(
            call(&MID, vec![txt("hello"), num(2.0), num(0.0)]),
            Ok(txt(""))
        );
        assert_eq!(
            call(&MID, vec![txt("héllo"), num(2.0), num(3.0)]),
            Ok(txt("éll"))
        );
    }

    #[test]
    fn len_trim_upper_lower_proper() {
        assert_eq!(call(&LEN, vec![txt("hello")]), Ok(num(5.0)));
        assert_eq!(call(&LEN, vec![txt("héllo")]), Ok(num(5.0)));
        assert_eq!(call(&LEN, vec![txt("")]), Ok(num(0.0)));
        assert_eq!(call(&LEN, vec![num(123.45)]), Ok(num(6.0)));
        assert_eq!(
            call(&TRIM, vec![txt("  hello   world  ")]),
            Ok(txt("hello world"))
        );
        assert_eq!(call(&TRIM, vec![txt("  a\tb  ")]), Ok(txt("a\tb")));
        assert_eq!(call(&TRIM, vec![txt("")]), Ok(txt("")));
        assert_eq!(
            call(&UPPER, vec![txt("Hello World")]),
            Ok(txt("HELLO WORLD"))
        );
        assert_eq!(call(&UPPER, vec![txt("héllo")]), Ok(txt("HÉLLO")));
        assert_eq!(call(&LOWER, vec![txt("HeLLo")]), Ok(txt("hello")));
        assert_eq!(
            call(&PROPER, vec![txt("hello WORLD")]),
            Ok(txt("Hello World"))
        );
        assert_eq!(
            call(&PROPER, vec![txt("2-way street")]),
            Ok(txt("2-Way Street"))
        );
        assert_eq!(call(&PROPER, vec![txt("o'brien")]), Ok(txt("O'Brien")));
    }

    #[test]
    fn find_is_case_sensitive_no_wildcards() {
        assert_eq!(call(&FIND, vec![txt("l"), txt("hello")]), Ok(num(3.0)));
        assert_eq!(
            call(&FIND, vec![txt("L"), txt("hello")]),
            Err(CalcError::Value)
        );
        assert_eq!(call(&FIND, vec![txt("h"), txt("hello")]), Ok(num(1.0)));
        assert_eq!(
            call(&FIND, vec![txt("l"), txt("hello"), num(4.0)]),
            Ok(num(4.0))
        );
        assert_eq!(
            call(&FIND, vec![txt("l"), txt("hello"), num(5.0)]),
            Err(CalcError::Value)
        );
        assert_eq!(call(&FIND, vec![txt("cd"), txt("abcd")]), Ok(num(3.0)));
        assert_eq!(
            call(&FIND, vec![txt("x"), txt("abc")]),
            Err(CalcError::Value)
        );
        assert_eq!(call(&FIND, vec![txt(""), txt("abc")]), Ok(num(1.0)));
        assert_eq!(
            call(&FIND, vec![txt(""), txt("abc"), num(3.0)]),
            Ok(num(3.0))
        );
        assert_eq!(
            call(&FIND, vec![txt(""), txt("abc"), num(5.0)]),
            Err(CalcError::Value)
        );
        assert_eq!(
            call(&FIND, vec![txt("l"), txt("hello"), num(0.0)]),
            Err(CalcError::Value)
        );
    }

    #[test]
    fn search_is_case_insensitive_with_wildcards() {
        assert_eq!(call(&SEARCH, vec![txt("l"), txt("hello")]), Ok(num(3.0)));
        assert_eq!(call(&SEARCH, vec![txt("L"), txt("hello")]), Ok(num(3.0)));
        assert_eq!(
            call(&SEARCH, vec![txt("w"), txt("hello world")]),
            Ok(num(7.0))
        );
        assert_eq!(
            call(&SEARCH, vec![txt("W"), txt("hello world")]),
            Ok(num(7.0))
        );
        assert_eq!(call(&SEARCH, vec![txt("l*"), txt("hello")]), Ok(num(3.0)));
        assert_eq!(
            call(&SEARCH, vec![txt("?orld"), txt("hello world")]),
            Ok(num(7.0))
        );
        assert_eq!(call(&SEARCH, vec![txt("*o*"), txt("hello")]), Ok(num(1.0)));
        assert_eq!(
            call(&SEARCH, vec![txt("b"), txt("abc"), num(2.0)]),
            Ok(num(2.0))
        );
        assert_eq!(
            call(&SEARCH, vec![txt("a"), txt("abc"), num(2.0)]),
            Err(CalcError::Value)
        );
        assert_eq!(
            call(&SEARCH, vec![txt(""), txt("abc"), num(2.0)]),
            Ok(num(2.0))
        );
        assert_eq!(
            call(&SEARCH, vec![txt("x"), txt("abc")]),
            Err(CalcError::Value)
        );
        assert_eq!(call(&SEARCH, vec![txt("~*"), txt("a*b")]), Ok(num(2.0)));
        assert_eq!(
            call(&SEARCH, vec![txt("z*"), txt("abc")]),
            Err(CalcError::Value)
        );
    }

    #[test]
    fn substitute_all_and_instance() {
        assert_eq!(
            call(&SUBSTITUTE, vec![txt("a-b-c"), txt("-"), txt("_")]),
            Ok(txt("a_b_c"))
        );
        assert_eq!(
            call(
                &SUBSTITUTE,
                vec![txt("a-b-c"), txt("-"), txt("_"), num(2.0)]
            ),
            Ok(txt("a-b_c"))
        );
        assert_eq!(
            call(
                &SUBSTITUTE,
                vec![txt("a-b-c"), txt("-"), txt("_"), num(5.0)]
            ),
            Ok(txt("a-b-c"))
        );
        assert_eq!(
            call(&SUBSTITUTE, vec![txt("a-b-c"), txt(""), txt("_")]),
            Ok(txt("a-b-c"))
        );
        assert_eq!(
            call(&SUBSTITUTE, vec![txt("A-a"), txt("a"), txt("x")]),
            Ok(txt("A-x"))
        );
        assert_eq!(
            call(&SUBSTITUTE, vec![txt("aaa"), txt("a"), txt("x"), num(0.0)]),
            Err(CalcError::Value)
        );
    }

    #[test]
    fn replace_is_positional() {
        assert_eq!(
            call(&REPLACE, vec![txt("abcdef"), num(2.0), num(3.0), txt("XY")]),
            Ok(txt("aXYef"))
        );
        assert_eq!(
            call(&REPLACE, vec![txt("abc"), num(2.0), num(0.0), txt("X")]),
            Ok(txt("aXbc"))
        );
        assert_eq!(
            call(&REPLACE, vec![txt("abc"), num(4.0), num(1.0), txt("X")]),
            Ok(txt("abcX"))
        );
        assert_eq!(
            call(&REPLACE, vec![txt("abc"), num(0.0), num(1.0), txt("X")]),
            Err(CalcError::Value)
        );
        assert_eq!(
            call(&REPLACE, vec![txt("abc"), num(2.0), num(-1.0), txt("X")]),
            Err(CalcError::Value)
        );
    }

    #[test]
    fn value_parses_numeric_strings() {
        assert_eq!(call(&VALUE, vec![txt("123")]), Ok(num(123.0)));
        assert_eq!(call(&VALUE, vec![txt("  42  ")]), Ok(num(42.0)));
        assert_eq!(call(&VALUE, vec![txt("12%")]), Ok(num(0.12)));
        assert_eq!(call(&VALUE, vec![txt("1e3")]), Ok(num(1000.0)));
        assert_eq!(call(&VALUE, vec![num(3.14)]), Ok(num(3.14)));
        assert_eq!(call(&VALUE, vec![txt("abc")]), Err(CalcError::Value));
        assert_eq!(call(&VALUE, vec![txt("")]), Err(CalcError::Value));
        assert_eq!(call(&VALUE, vec![txt("1,000")]), Err(CalcError::Value));
        assert_eq!(
            call(&VALUE, vec![CalcValue::Bool(true)]),
            Err(CalcError::Value)
        );
        assert_eq!(call(&VALUE, vec![CalcValue::Blank]), Err(CalcError::Value));
    }

    #[test]
    fn value_parses_times_and_dates() {
        // Measured en-IN Excel: VALUE("16:48:00") = 0.7.
        assert_eq!(call(&VALUE, vec![txt("16:48:00")]), Ok(num(0.7)));
        assert_eq!(call(&VALUE, vec![txt("16:48")]), Ok(num(0.7)));
        assert_eq!(call(&VALUE, vec![txt("12:00:00")]), Ok(num(0.5)));
        assert_eq!(call(&VALUE, vec![txt("24:00")]), Ok(num(1.0)));
        // a currency-prefixed number is #VALUE! in this locale, not parsed
        assert_eq!(call(&VALUE, vec![txt("₹ 1,000")]), Err(CalcError::Value));
        // year-first dates with an optional time resolve to a serial
        assert_eq!(call(&VALUE, vec![txt("1900-01-01")]), Ok(num(1.0)));
        assert_eq!(call(&VALUE, vec![txt("1900-02-29")]), Ok(num(60.0)));
        assert_eq!(call(&VALUE, vec![txt("1900-03-01")]), Ok(num(61.0)));
        let d = call(&VALUE, vec![txt("2026-08-16 16:48:00")]).unwrap();
        let whole = d.as_number().unwrap().floor();
        assert_eq!(whole, 46250.0);
        assert!((d.as_number().unwrap() - whole - 0.7).abs() < 1e-9);
    }

    #[test]
    fn text_formats_digit_patterns() {
        assert_eq!(call(&TEXT, vec![num(1234.5), txt("0")]), Ok(txt("1235")));
        assert_eq!(call(&TEXT, vec![num(2.5), txt("0")]), Ok(txt("3")));
        assert_eq!(call(&TEXT, vec![num(-2.5), txt("0")]), Ok(txt("-3")));
        assert_eq!(
            call(&TEXT, vec![num(3.14159), txt("0.00")]),
            Ok(txt("3.14"))
        );
        assert_eq!(
            call(&TEXT, vec![num(0.1 + 0.2), txt("0.00")]),
            Ok(txt("0.30"))
        );
        assert_eq!(call(&TEXT, vec![num(5.0), txt("#")]), Ok(txt("5")));
        assert_eq!(call(&TEXT, vec![num(0.0), txt("#")]), Ok(txt("")));
        assert_eq!(call(&TEXT, vec![num(0.5), txt("0.##")]), Ok(txt("0.5")));
        assert_eq!(call(&TEXT, vec![num(1.2), txt("0.0#")]), Ok(txt("1.2")));
        assert_eq!(call(&TEXT, vec![num(0.0), txt("0.00")]), Ok(txt("0.00")));
        assert_eq!(call(&TEXT, vec![num(0.05), txt("0.00")]), Ok(txt("0.05")));
    }

    #[test]
    fn text_format_passes_literal_currency_symbols_through() {
        // Measured en-IN Excel: TEXT(111, "$#,##0.00") = "$111.00" — a literal
        // currency symbol in the format is emitted as-is.
        assert_eq!(
            call(&TEXT, vec![num(111.0), txt("$#,##0.00")]),
            Ok(txt("$111.00"))
        );
        assert_eq!(
            call(&TEXT, vec![num(111.0), txt("₹#,##0.00")]),
            Ok(txt("₹111.00"))
        );
        assert_eq!(
            call(&TEXT, vec![num(-111.0), txt("$#,##0.00")]),
            Ok(txt("-$111.00"))
        );
        assert_eq!(
            call(&TEXT, vec![num(5.0), txt("0 \"units\"")]),
            Ok(txt("5 units"))
        );
        assert_eq!(
            call(&TEXT, vec![num(12.5), txt("0.00 $")]),
            Ok(txt("12.50 $"))
        );
        assert_eq!(
            call(&TEXT, vec![num(5.0), txt("Rate: 0.0")]),
            Ok(txt("Rate: 5.0"))
        );
        assert_eq!(call(&TEXT, vec![num(5.0), txt("0\\$")]), Ok(txt("5$")));
    }

    #[test]
    fn text_formats_grouping_percent_general() {
        assert_eq!(
            call(&TEXT, vec![num(1234567.891), txt("#,##0")]),
            Ok(txt("1,234,568"))
        );
        assert_eq!(
            call(&TEXT, vec![num(1234567.891), txt("#,##0.00")]),
            Ok(txt("1,234,567.89"))
        );
        assert_eq!(
            call(&TEXT, vec![num(123.0), txt("0,000")]),
            Ok(txt("0,123"))
        );
        assert_eq!(call(&TEXT, vec![num(0.5), txt("0%")]), Ok(txt("50%")));
        assert_eq!(
            call(&TEXT, vec![num(-0.25), txt("0.0%")]),
            Ok(txt("-25.0%"))
        );
        assert_eq!(
            call(&TEXT, vec![num(12345.678), txt("General")]),
            Ok(txt("12345.678"))
        );
        assert_eq!(call(&TEXT, vec![num(1.0), txt("GENERAL")]), Ok(txt("1")));
        assert_eq!(call(&TEXT, vec![num(0.0), txt("0.00%")]), Ok(txt("0.00%")));
    }

    #[test]
    fn text_rejects_unsupported_formats() {
        assert_eq!(
            call(&TEXT, vec![num(123.0), txt("0.00E+00")]),
            Err(CalcError::Value)
        );
        assert_eq!(call(&TEXT, vec![num(5.0), txt("@")]), Err(CalcError::Value));
        assert_eq!(
            call(&TEXT, vec![num(5.0), txt("0.0?")]),
            Err(CalcError::Value)
        );
        assert_eq!(
            call(&TEXT, vec![num(5.0), txt("0/4")]),
            Err(CalcError::Value)
        );
        assert_eq!(
            call(&TEXT, vec![num(5.0), txt("0;0")]),
            Err(CalcError::Value)
        );
        assert_eq!(
            call(&TEXT, vec![num(5.0), txt("0*#")]),
            Err(CalcError::Value)
        );
        assert_eq!(
            call(&TEXT, vec![txt("abc"), txt("0")]),
            Err(CalcError::Value)
        );
    }

    #[test]
    fn concat_flattens_arrays_and_coerces() {
        assert_eq!(call(&CONCAT, vec![txt("ab"), txt("cd")]), Ok(txt("abcd")));
        assert_eq!(call(&CONCAT, vec![num(1.0), num(2.0)]), Ok(txt("12")));
        let a = row(vec![
            num(1.0),
            txt("x"),
            CalcValue::Blank,
            CalcValue::Bool(true),
        ]);
        assert_eq!(call(&CONCAT, vec![a]), Ok(txt("1xTRUE")));
        assert_eq!(
            call(&CONCATENATE, vec![txt("ab"), txt("cd")]),
            Ok(txt("abcd"))
        );
        assert_eq!(call(&CONCATENATE, vec![num(1.0), txt("x")]), Ok(txt("1x")));
        assert_eq!(
            call(&CONCAT, vec![txt("a"), CalcValue::err(CalcError::Na)]),
            Err(CalcError::Na)
        );
    }

    #[test]
    fn textjoin_delimiter_and_ignore_empty() {
        let a = row(vec![txt("a"), txt("b"), CalcValue::Blank, txt("c")]);
        assert_eq!(
            call(&TEXTJOIN, vec![txt(","), CalcValue::Bool(true), a.clone()]),
            Ok(txt("a,b,c"))
        );
        assert_eq!(
            call(&TEXTJOIN, vec![txt("-"), CalcValue::Bool(false), a]),
            Ok(txt("a-b--c"))
        );
        assert_eq!(
            call(
                &TEXTJOIN,
                vec![txt(","), CalcValue::Bool(true), txt("a"), txt("b")]
            ),
            Ok(txt("a,b"))
        );
        assert_eq!(
            call(
                &TEXTJOIN,
                vec![txt(","), CalcValue::Bool(true), num(1.0), num(2.0)]
            ),
            Ok(txt("1,2"))
        );
        assert_eq!(
            call(&TEXTJOIN, vec![txt(","), CalcValue::Bool(true), num(0.0)]),
            Ok(txt("0"))
        );
        assert_eq!(
            call(&TEXTJOIN, vec![txt(","), CalcValue::Bool(true), txt("")]),
            Ok(txt(""))
        );
        let empty = row(vec![CalcValue::Blank, CalcValue::Blank]);
        assert_eq!(
            call(&TEXTJOIN, vec![txt(","), CalcValue::Bool(true), empty]),
            Ok(txt(""))
        );
    }

    #[test]
    fn rept_repeats_and_caps() {
        assert_eq!(call(&REPT, vec![txt("ab"), num(3.0)]), Ok(txt("ababab")));
        assert_eq!(call(&REPT, vec![txt("x"), num(0.0)]), Ok(txt("")));
        assert_eq!(call(&REPT, vec![txt("ab"), num(2.9)]), Ok(txt("abab")));
        assert_eq!(
            call(&REPT, vec![txt("x"), num(-1.0)]),
            Err(CalcError::Value)
        );
        assert_eq!(
            call(&REPT, vec![txt("a"), num(40000.0)]),
            Err(CalcError::Value)
        );
        assert_eq!(
            call(&REPT, vec![txt("abc"), num(11000.0)]),
            Err(CalcError::Value)
        );
        assert_eq!(
            call(&REPT, vec![txt("ab"), num(16384.0)]),
            Err(CalcError::Value)
        );
        let ok = call(&REPT, vec![txt("a"), num(32767.0)]).unwrap();
        assert_eq!(s(ok).chars().count(), 32767);
    }

    #[test]
    fn exact_is_case_sensitive() {
        assert_eq!(
            call(&EXACT, vec![txt("abc"), txt("abc")]),
            Ok(CalcValue::Bool(true))
        );
        assert_eq!(
            call(&EXACT, vec![txt("abc"), txt("ABC")]),
            Ok(CalcValue::Bool(false))
        );
        assert_eq!(
            call(&EXACT, vec![num(1.0), txt("1")]),
            Ok(CalcValue::Bool(true))
        );
        assert_eq!(
            call(&EXACT, vec![txt("1.5"), num(1.5)]),
            Ok(CalcValue::Bool(true))
        );
    }

    #[test]
    fn char_and_code_roundtrip() {
        assert_eq!(call(&CHAR, vec![num(65.0)]), Ok(txt("A")));
        assert_eq!(call(&CHAR, vec![num(65.9)]), Ok(txt("A")));
        assert_eq!(call(&CHAR, vec![num(128.0)]), Ok(txt("€")));
        assert_eq!(call(&CHAR, vec![num(255.0)]), Ok(txt("ÿ")));
        assert_eq!(call(&CHAR, vec![num(0.0)]), Err(CalcError::Value));
        assert_eq!(call(&CHAR, vec![num(256.0)]), Err(CalcError::Value));
        assert_eq!(call(&CODE, vec![txt("A")]), Ok(num(65.0)));
        assert_eq!(call(&CODE, vec![txt("abc")]), Ok(num(97.0)));
        assert_eq!(call(&CODE, vec![txt("€")]), Ok(num(128.0)));
        assert_eq!(call(&CODE, vec![txt("")]), Err(CalcError::Value));
        assert_eq!(call(&CODE, vec![txt("中")]), Ok(num(20013.0)));
    }

    #[test]
    fn t_returns_text_only() {
        assert_eq!(call(&T, vec![txt("hello")]), Ok(txt("hello")));
        assert_eq!(call(&T, vec![num(123.0)]), Ok(txt("")));
        assert_eq!(call(&T, vec![CalcValue::Bool(true)]), Ok(txt("")));
        assert_eq!(call(&T, vec![CalcValue::Blank]), Ok(txt("")));
        assert_eq!(
            call(&T, vec![CalcValue::err(CalcError::Na)]),
            Err(CalcError::Na)
        );
    }

    #[test]
    fn text_family_arities() {
        assert!(LEFT.validate(1).is_ok());
        assert!(LEFT.validate(2).is_ok());
        assert_eq!(LEFT.validate(0), Err(CalcError::Value));
        assert_eq!(LEFT.validate(3), Err(CalcError::Value));
        assert!(MID.validate(3).is_ok());
        assert_eq!(MID.validate(2), Err(CalcError::Value));
        assert!(TEXTJOIN.validate(3).is_ok());
        assert_eq!(TEXTJOIN.validate(2), Err(CalcError::Value));
        assert_eq!(TEXT.validate(1), Err(CalcError::Value));
        assert_eq!(REPLACE.validate(3), Err(CalcError::Value));
    }

    // -- TEXTBEFORE / TEXTAFTER (through the real parse→eval path) ------------

    #[test]
    fn textbefore_finds_the_requested_instance() {
        let g = Grid::empty();
        assert_eq!(g.text("=TEXTBEFORE(\"abc,def,ghi\", \",\")"), "abc");
        assert_eq!(g.text("=TEXTBEFORE(\"abc,def,ghi\", \",\", 2)"), "abc,def");
        assert_eq!(
            g.text("=TEXTBEFORE(\"abc,def,ghi,jkl\", \",\", 3)"),
            "abc,def,ghi"
        );
        assert_eq!(g.text("=TEXTBEFORE(\"abc,def,ghi\", \"ghi\")"), "abc,def,");
    }

    #[test]
    fn textbefore_negative_instance_counts_from_the_end() {
        let g = Grid::empty();
        assert_eq!(g.text("=TEXTBEFORE(\"abc,def,ghi\", \",\", -1)"), "abc,def");
        assert_eq!(g.text("=TEXTBEFORE(\"abc,def,ghi\", \",\", -2)"), "abc");
        assert_eq!(g.text("=TEXTBEFORE(\"abc,def,ghi,jkl\", \",\", -3)"), "abc");
        assert_eq!(
            g.text("=TEXTBEFORE(\",a,b,c\", \",\", -3)"),
            "",
            "a leading delimiter leaves an empty prefix"
        );
        assert_eq!(
            g.error("=TEXTBEFORE(\"abc,def,ghi\", \",\", 3)"),
            CalcError::Na,
            "two delimiters cannot serve instance 3"
        );
    }

    #[test]
    fn textbefore_match_mode_end_and_if_not_found() {
        let g = Grid::empty();
        assert_eq!(
            g.text("=TEXTBEFORE(\"Apples,Bananas,Cherries\", \"b\", 1, 1)"),
            "Apples,"
        );
        assert_eq!(
            g.error("=TEXTBEFORE(\"Apples,Bananas,Cherries\", \"b\", 1, 0)"),
            CalcError::Na
        );
        assert_eq!(
            g.text("=TEXTBEFORE(\"red blue\", \",\", 1, 0, 1)"),
            "red blue"
        );
        assert_eq!(
            g.text("=TEXTBEFORE(\"abc\", \",\", 1, 0, 0, \"none\")"),
            "none"
        );
        assert_eq!(g.error("=TEXTBEFORE(\"red blue\", \",\")"), CalcError::Na);
    }

    #[test]
    fn textbefore_errors() {
        let g = Grid::empty();
        assert_eq!(g.error("=TEXTBEFORE(\"abc,def\", \"\")"), CalcError::Value);
        assert_eq!(
            g.error("=TEXTBEFORE(\"abc,def\", \",\", 0)"),
            CalcError::Value
        );
        assert_eq!(
            g.error("=TEXTBEFORE(\"abc,def\", \",\", 1, 2)"),
            CalcError::Value
        );
        assert_eq!(
            g.error("=TEXTBEFORE(\"abc,def\", \",\", 1, 0, 2)"),
            CalcError::Value
        );
        assert_eq!(
            g.error("=TEXTBEFORE(\"abc\", \",\", 1, 0, 0, #DIV/0!)"),
            CalcError::Div0,
            "a missing delimiter hands back if_not_found as-is, error included"
        );
    }

    #[test]
    fn textafter_keeps_the_tail() {
        let g = Grid::empty();
        assert_eq!(g.text("=TEXTAFTER(\"abc,def,ghi\", \",\")"), "def,ghi");
        assert_eq!(g.text("=TEXTAFTER(\"abc,def,ghi\", \",\", 2)"), "ghi");
        assert_eq!(g.text("=TEXTAFTER(\"abc,def,ghi\", \",\", -1)"), "ghi");
        assert_eq!(g.text("=TEXTAFTER(\"abc,def,ghi\", \",\", -2)"), "def,ghi");
        assert_eq!(g.text("=TEXTAFTER(\"abc\", \",\", 1, 0, 1)"), "");
        assert_eq!(g.error("=TEXTAFTER(\"abc\", \",\")"), CalcError::Na);
    }

    // -- NUMBERVALUE / FIXED / DOLLAR -----------------------------------------

    #[test]
    fn numbervalue_parses_explicit_separators() {
        let g = Grid::empty();
        assert_eq!(g.num("=NUMBERVALUE(\"2.500,27\", \",\", \".\")"), 2500.27);
        assert_eq!(g.num("=NUMBERVALUE(\"1,234.56\")"), 1234.56);
        assert_eq!(g.num("=NUMBERVALUE(\"1,234,567\")"), 1234567.0);
        assert_eq!(g.num("=NUMBERVALUE(\"3.5%\")"), 0.035);
        assert_eq!(g.num("=NUMBERVALUE(\" 1.000 \")"), 1.0);
        assert_eq!(g.num("=NUMBERVALUE(\".5\")"), 0.5);
        assert_eq!(g.num("=NUMBERVALUE(\"-1.234,56\", \",\", \".\")"), -1234.56);
    }

    #[test]
    fn numbervalue_rejects_malformed_text() {
        let g = Grid::empty();
        assert_eq!(g.error("=NUMBERVALUE(\"abc\")"), CalcError::Value);
        assert_eq!(g.error("=NUMBERVALUE(\"\")"), CalcError::Value);
        assert_eq!(g.error("=NUMBERVALUE(\"1.2.3\")"), CalcError::Value);
        assert_eq!(g.error("=NUMBERVALUE(\"12,34\")"), CalcError::Value);
        assert_eq!(g.error("=NUMBERVALUE(\"1,2345\")"), CalcError::Value);
        assert_eq!(
            g.error("=NUMBERVALUE(\"1,234.56\", \",\", \",\")"),
            CalcError::Value
        );
        assert_eq!(
            g.error("=NUMBERVALUE(\"1,234.56\", \"\", \",\")"),
            CalcError::Value
        );
    }

    #[test]
    fn fixed_rounds_and_formats() {
        let g = Grid::empty();
        assert_eq!(g.text("=FIXED(1234.567, 1)"), "1,234.6");
        assert_eq!(g.text("=FIXED(1234.567)"), "1,234.57");
        assert_eq!(g.text("=FIXED(1234.567, -1)"), "1,230");
        assert_eq!(g.text("=FIXED(1234.567, -1, TRUE)"), "1230");
        assert_eq!(g.text("=FIXED(44.332)"), "44.33");
        assert_eq!(g.text("=FIXED(2.5, 0)"), "3");
        assert_eq!(g.text("=FIXED(-2.5, 0)"), "-3");
        assert_eq!(g.text("=FIXED(0.1+0.2, 2)"), "0.30");
        assert_eq!(g.error("=FIXED(\"abc\", 2)"), CalcError::Value);
        // decimals beyond ±127 is #VALUE! (measured: FIXED(1234.567,128)).
        assert_eq!(g.error("=FIXED(1234.567, 128)"), CalcError::Value);
        assert_eq!(g.error("=FIXED(1234.567, -128)"), CalcError::Value);
    }

    #[test]
    fn dollar_formats_with_locale_currency_symbol() {
        let g = Grid::empty();
        // Measured en-IN Excel: ₹ (rupee) symbol + space, two decimals,
        // thousands separator — never a hardcoded US dollar sign.
        assert_eq!(g.text("=DOLLAR(1234.567)"), "₹ 1,234.57");
        assert_eq!(g.text("=DOLLAR(1234.567, 0)"), "₹ 1,235");
        assert_eq!(g.text("=DOLLAR(-1234.567, -2)"), "-₹ 1,200");
        assert_eq!(g.text("=DOLLAR(0)"), "₹ 0.00");
        assert_eq!(g.error("=DOLLAR(\"abc\")"), CalcError::Value);
        assert_eq!(g.error("=DOLLAR(1, 128)"), CalcError::Value);
    }

    // -- CLEAN / UNICHAR / UNICODE --------------------------------------------

    #[test]
    fn clean_strips_only_ascii_control_chars() {
        let g = Grid::empty();
        assert_eq!(g.text("=CLEAN(\"a\"&CHAR(9)&\"b\")"), "ab");
        assert_eq!(g.text("=CLEAN(CHAR(7)&\"x\")"), "x");
        assert_eq!(g.text("=CLEAN(\"hello\")"), "hello");
        assert_eq!(g.text("=CLEAN(\"a\"&CHAR(127)&\"b\")"), "a\u{7f}b");
    }

    #[test]
    fn unichar_maps_code_points() {
        let g = Grid::empty();
        assert_eq!(g.text("=UNICHAR(65)"), "A");
        assert_eq!(g.text("=UNICHAR(8364)"), "€");
        assert_eq!(g.text("=UNICHAR(255)"), "ÿ");
        assert_eq!(g.error("=UNICHAR(0)"), CalcError::Value);
        assert_eq!(g.error("=UNICHAR(55296)"), CalcError::Value);
        assert_eq!(g.error("=UNICHAR(1114112)"), CalcError::Value);
    }

    #[test]
    fn unicode_reports_first_code_point() {
        let g = Grid::empty();
        assert_eq!(g.num("=UNICODE(\"A\")"), 65.0);
        assert_eq!(g.num("=UNICODE(\"€\")"), 8364.0);
        assert_eq!(g.error("=UNICODE(\"\")"), CalcError::Value);
    }

    // -- the *B byte functions (char equivalents, exact for single-byte text) -

    #[test]
    fn byte_functions_track_their_char_equivalents() {
        let g = Grid::empty();
        assert_eq!(g.text("=LEFTB(\"hello\", 3)"), "hel");
        assert_eq!(g.text("=RIGHTB(\"hello\", 2)"), "lo");
        assert_eq!(g.text("=MIDB(\"hello\", 2, 3)"), "ell");
        assert_eq!(g.num("=LENB(\"hello\")"), 5.0);
        assert_eq!(g.num("=FINDB(\"l\", \"hello\")"), 3.0);
        assert_eq!(g.num("=SEARCHB(\"L\", \"hello\")"), 3.0);
        assert_eq!(g.error("=FINDB(\"x\", \"hello\")"), CalcError::Value);
    }

    #[test]
    fn non_dbcs_byte_semantics_match_measured_excel() {
        let g = Grid::empty();
        // each UTF-16 code unit counts 1; a surrogate pair (emoji) counts 2
        assert_eq!(g.num("=LEN(\"😊\")"), 2.0);
        assert_eq!(g.num("=LENB(\"😊\")"), 2.0);
        assert_eq!(g.num("=LEN(\"😊Hello\")"), 7.0);
        assert_eq!(g.num("=LENB(\"😊Hello\")"), 7.0);
        // LEFTB(emoji+Hello,3) = emoji+He (the chars Excel returns)
        assert_eq!(g.text("=LEFTB(\"😊Hello\", 3)"), "😊He");
        assert_eq!(g.text("=MIDB(\"中文测试\", 1, 2)"), "中文");
        assert_eq!(g.text("=RIGHTB(\"中文测试\", 2)"), "测试");
        assert_eq!(g.num("=LENB(\"，。、；：{}\")"), 7.0);
        // a Chinese substring is found at byte position 4 (chars == bytes in a
        // non-DBCS locale; emoji would be 2)
        assert_eq!(g.num("=FINDB(\"欢迎\", \"你好，欢迎\")"), 4.0);
        assert_eq!(g.num("=SEARCHB(\"欢迎\", \"你好，欢迎\")"), 4.0);
        // REPLACEB = REPLACE on whole characters
        assert_eq!(g.text("=REPLACEB(\"abcdef\", 2, 3, \"XY\")"), "aXYef");
        assert_eq!(g.text("=REPLACEB(\"中文测试\", 1, 2, \"好\")"), "好测试");
    }

    // -- VALUETOTEXT / ARRAYTOTEXT --------------------------------------------

    #[test]
    fn valuetotext_renders_compact_and_strict() {
        let g = Grid::empty();
        assert_eq!(g.text("=VALUETOTEXT(123.45)"), "123.45");
        assert_eq!(g.text("=VALUETOTEXT(\"abc\")"), "abc");
        assert_eq!(g.text("=VALUETOTEXT(TRUE)"), "TRUE");
        assert_eq!(g.text("=VALUETOTEXT(NA())"), "#N/A");
        assert_eq!(g.text("=VALUETOTEXT(1/0)"), "#DIV/0!");
        assert_eq!(g.text("=VALUETOTEXT(\"a\"\"b\", 1)"), "\"a\"\"b\"");
        assert_eq!(g.text("=VALUETOTEXT(3, 1)"), "3");
        assert_eq!(g.error("=VALUETOTEXT(123, 2)"), CalcError::Value);
    }

    #[test]
    fn arraytotext_joins_with_comma_space() {
        let g = Grid::empty();
        // Measured en-IN Excel: elements are joined with ", " (comma + space),
        // no braces; the blank-string element contributes an empty field.
        assert_eq!(g.text("=ARRAYTOTEXT({1,2,3})"), "1, 2, 3");
        assert_eq!(g.text("=ARRAYTOTEXT({1,2;3,4})"), "1, 2, 3, 4");
        assert_eq!(g.text("=ARRAYTOTEXT({\"a\",\"b\"})"), "a, b");
        assert_eq!(
            g.text("=ARRAYTOTEXT({1,\" \",1.23,TRUE,FALSE})"),
            "1,  , 1.23, TRUE, FALSE"
        );
        assert_eq!(g.text("=ARRAYTOTEXT({\"a\",\"b\"}, 1)"), "\"a\", \"b\"");
        assert_eq!(g.text("=ARRAYTOTEXT(123)"), "123");
        assert_eq!(g.text("=ARRAYTOTEXT(1/0)"), "#DIV/0!");
        assert_eq!(g.error("=ARRAYTOTEXT({1,2}, 2)"), CalcError::Value);
    }

    #[test]
    fn numbervalue_applies_each_percent_sign() {
        let g = Grid::empty();
        // Measured en-IN Excel: NUMBERVALUE("20%%") = 0.002 — every % divides
        // by 100.
        assert_eq!(g.num("=NUMBERVALUE(\"20%%\")"), 0.002);
        assert_eq!(g.num("=NUMBERVALUE(\"3.5%\")"), 0.035);
        assert_eq!(g.num("=NUMBERVALUE(\"3.5%%\")"), 0.00035);
        // three percent signs divide by 100 three times
        assert_eq!(g.num("=NUMBERVALUE(\"100%%%\", \"%\", \",\")"), 0.0001);
    }

    #[test]
    fn asc_and_dbcs_convert_the_fullwidth_ascii_block() {
        let g = Grid::empty();
        assert_eq!(g.text("=ASC(\"ＡＢＣ１２３\")"), "ABC123");
        assert_eq!(g.text("=ASC(\"　\")"), " ");
        assert_eq!(g.text("=ASC(\"abc\")"), "abc");
        // katakana is not carried by this engine; it passes through untouched
        assert_eq!(g.text("=ASC(\"アイウ\")"), "アイウ");
        assert_eq!(g.text("=DBCS(\"ABC123\")"), "ＡＢＣ１２３");
        assert_eq!(g.text("=DBCS(\" \")"), "　");
        assert_eq!(g.text("=DBCS(\"已全角\")"), "已全角");
        assert_eq!(g.error("=ASC()"), CalcError::Value);
        assert_eq!(g.error("=ASC(\"a\", \"b\")"), CalcError::Value);
    }

    #[test]
    fn bahttext_renders_thai_words() {
        let g = Grid::empty();
        assert_eq!(g.text("=BAHTTEXT(0)"), "ศูนย์บาท");
        assert_eq!(g.text("=BAHTTEXT(1)"), "หนึ่งบาท");
        assert_eq!(g.text("=BAHTTEXT(21)"), "ยี่สิบเอ็ดบาท");
        assert_eq!(g.text("=BAHTTEXT(123.45)"), "หนึ่งร้อยยี่สิบสามบาทสี่สิบห้าสตางค์");
        assert_eq!(g.text("=BAHTTEXT(1.05)"), "หนึ่งบาทห้าสตางค์");
        assert_eq!(g.text("=BAHTTEXT(1000001)"), "หนึ่งล้านหนึ่งบาท");
        assert_eq!(
            g.text("=BAHTTEXT(1234567)"),
            "หนึ่งล้านสองแสนสามหมื่นสี่พันห้าร้อยหกสิบเจ็ดบาท"
        );
        assert_eq!(g.error("=BAHTTEXT(\"abc\")"), CalcError::Value);
        assert_eq!(g.error("=BAHTTEXT(1, 2)"), CalcError::Value);
    }

    #[test]
    fn numberstring_renders_chinese_forms() {
        let g = Grid::empty();
        assert_eq!(g.text("=NUMBERSTRING(123, 1)"), "一百二十三");
        assert_eq!(g.text("=NUMBERSTRING(123, 2)"), "壹佰贰拾叁");
        assert_eq!(g.text("=NUMBERSTRING(123, 3)"), "一二三");
        assert_eq!(g.text("=NUMBERSTRING(0, 1)"), "零");
        assert_eq!(g.text("=NUMBERSTRING(12, 1)"), "十二");
        assert_eq!(g.text("=NUMBERSTRING(12, 2)"), "壹拾贰");
        assert_eq!(g.text("=NUMBERSTRING(1005, 1)"), "一千零五");
        assert_eq!(
            g.text("=NUMBERSTRING(1234567, 1)"),
            "一百二十三万四千五百六十七"
        );
        assert_eq!(g.text("=NUMBERSTRING(1000001, 1)"), "一百万零一");
        assert_eq!(g.error("=NUMBERSTRING(123, 0)"), CalcError::Value);
        assert_eq!(g.error("=NUMBERSTRING(123, 4)"), CalcError::Value);
        assert_eq!(g.error("=NUMBERSTRING(123)"), CalcError::Value);
    }

    #[test]
    fn new_functions_are_registered() {
        for name in ["ASC", "DBCS", "BAHTTEXT", "NUMBERSTRING", "REPLACEB"] {
            let r = registry();
            assert!(
                r.get(name).is_some(),
                "{name} must be registered (a missing registration silently \
                 routes every call to the fallback path)"
            );
        }
    }
}
