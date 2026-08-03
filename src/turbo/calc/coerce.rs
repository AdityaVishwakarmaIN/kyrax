// calc/coerce.rs — Excel coercion, comparison ordering, and blank semantics
// (spec `01_parser_reference.md` §7–§9). No new dependencies: number→text uses
// the existing `ryu` crate for shortest digits, then applies Excel's General
// formatting rules.

use crate::turbo::calc::value::{CalcError, CalcValue};
use std::cmp::Ordering;
use std::sync::Arc;

/// Classify a literal string per the ValueObjectFactory priority (spec §7.1):
/// error set → error; `TRUE`/`FALSE` → bool; numeric round-trip → number;
/// number pattern → number; else text.
pub fn classify_text(s: &str) -> CalcValue {
    let t = s.trim();
    if let Some(e) = CalcError::from_str_ci(t) {
        return CalcValue::Error(e);
    }
    match t.to_ascii_uppercase().as_str() {
        "TRUE" => return CalcValue::Bool(true),
        "FALSE" => return CalcValue::Bool(false),
        _ => {}
    }
    if is_real_num(t) {
        return CalcValue::Number(t.parse().unwrap_or(0.0));
    }
    if let Ok(n) = t.parse::<f64>() {
        return CalcValue::Number(n);
    }
    CalcValue::text(s)
}

/// Spec §7.1 round-trip check: the string must be a number whose General
/// representation equals the trimmed input (so `"000123456"` is not a real
/// number, though it still parses as one via the number-pattern branch).
pub fn is_real_num(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    match s.trim().parse::<f64>() {
        Ok(n) => number_to_general(n) == s.trim(),
        Err(_) => false,
    }
}

/// Coerce to number (spec §7.2): bool → 1/0, blank → 0, text → parse (empty or
/// non-numeric → `#VALUE!`), arrays → `#VALUE!` (eval scalarizes first).
/// Results are finite; NaN/Inf → `#NUM!`.
pub fn coerce_number(v: &CalcValue) -> Result<f64, CalcError> {
    match v {
        CalcValue::Number(n) => {
            if n.is_finite() {
                Ok(*n)
            } else {
                Err(CalcError::Num)
            }
        }
        CalcValue::Bool(b) => Ok(bool_num(*b)),
        CalcValue::Blank => Ok(0.0),
        CalcValue::Text(t) => text_to_number(t),
        CalcValue::Error(e) => Err(*e),
        CalcValue::Array(_) => Err(CalcError::Value),
    }
}

fn text_to_number(s: &str) -> Result<f64, CalcError> {
    let t = s.trim();
    if t.is_empty() {
        return Err(CalcError::Value);
    }
    if let Some(stripped) = t.strip_suffix('%') {
        return Ok(text_to_number(stripped)? / 100.0);
    }
    let n: f64 = t.parse().map_err(|_| CalcError::Value)?;
    if n.is_finite() {
        Ok(n)
    } else {
        Err(CalcError::Num)
    }
}

/// Coerce to text (spec §7.2): number → Excel General format, bool →
/// `"TRUE"`/`"FALSE"`, blank → `""`, errors propagate, arrays → `#VALUE!`.
pub fn coerce_text(v: &CalcValue) -> Result<String, CalcError> {
    match v {
        CalcValue::Number(n) => Ok(number_to_general(*n)),
        CalcValue::Text(t) => Ok(t.to_string()),
        CalcValue::Bool(b) => Ok(if *b { "TRUE" } else { "FALSE" }.to_string()),
        CalcValue::Blank => Ok(String::new()),
        CalcValue::Error(e) => Err(*e),
        CalcValue::Array(_) => Err(CalcError::Value),
    }
}

/// Blank-cell resolution (spec §9): a blank coerces to `""` vs a string,
/// `FALSE` vs a boolean, and `0` otherwise.
pub fn blank_as(v: &CalcValue) -> CalcValue {
    match v {
        CalcValue::Text(_) => CalcValue::Text(Arc::from("")),
        CalcValue::Bool(_) => CalcValue::Bool(false),
        _ => CalcValue::Number(0.0),
    }
}

/// Excel comparison ordering (spec §8). Errors propagate, left operand wins
/// ties; arrays → `#VALUE!` (eval scalarizes first). Text-vs-text is
/// case-insensitive byte/codepoint order (collation is a later refinement).
pub fn compare(l: &CalcValue, r: &CalcValue) -> Result<Ordering, CalcError> {
    match (l, r) {
        (CalcValue::Error(e), _) | (_, CalcValue::Error(e)) => return Err(*e),
        _ => {}
    }
    if l.is_array() || r.is_array() {
        return Err(CalcError::Value);
    }
    // Blank resolves against the other side's type (spec §9).
    let (l, r) = if l.is_blank() || r.is_blank() {
        let l = if l.is_blank() { blank_as(r) } else { l.clone() };
        let r = if r.is_blank() {
            blank_as(&l)
        } else {
            r.clone()
        };
        (l, r)
    } else {
        (l.clone(), r.clone())
    };
    // Excel orders *across* types by type rank, not by coercion:
    //   any number < any text < FALSE < TRUE
    // (distinct from arithmetic, where TRUE coerces to 1). Only same-rank
    // operands are compared by value.
    if type_rank(&l) != type_rank(&r) {
        return Ok(type_rank(&l).cmp(&type_rank(&r)));
    }
    Ok(match (&l, &r) {
        (CalcValue::Number(a), CalcValue::Number(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
        (CalcValue::Bool(a), CalcValue::Bool(b)) => a.cmp(b), // FALSE < TRUE
        (CalcValue::Text(a), CalcValue::Text(b)) => {
            a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase())
        }
        _ => Ordering::Equal,
    })
}

/// Cross-type comparison rank (spec §8): number < text < boolean.
fn type_rank(v: &CalcValue) -> u8 {
    match v {
        CalcValue::Number(_) => 0,
        CalcValue::Text(_) => 1,
        CalcValue::Bool(_) => 2,
        _ => 0, // blank is resolved before this point
    }
}

/// Equality for `=`/`<>` and, later, the lookup family. Same ordering as
/// `compare`; when `wildcards` is set, a text right-hand side is matched as a
/// pattern (`*` any run, `?` one char, `~` escapes).
pub fn compare_eq(l: &CalcValue, r: &CalcValue, wildcards: bool) -> Result<bool, CalcError> {
    if wildcards {
        let lt = coerce_text(l)?;
        let rt = coerce_text(r)?;
        return Ok(wildcard_match(&rt, &lt));
    }
    Ok(compare(l, r)? == Ordering::Equal)
}

/// Match `text` against `pattern`, where `*` matches any run (including empty),
/// `?` matches exactly one char, and `~` escapes the next character. Matching
/// is case-insensitive via ASCII lowercase. No regex dependency.
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_ascii_lowercase().chars().collect();
    let t: Vec<char> = text.to_ascii_lowercase().chars().collect();
    let mut pi = 0;
    let mut ti = 0;
    let mut star: Option<usize> = None; // pattern index of the last '*'
    let mut star_ti = 0;
    while ti < t.len() {
        if pi < p.len() {
            let pc = p[pi];
            if pc == '*' {
                star = Some(pi);
                star_ti = ti;
                pi += 1;
                continue;
            }
            if pc == '?' || pc == t[ti] {
                pi += 1;
                ti += 1;
                continue;
            }
            if pc == '~' && pi + 1 < p.len() && p[pi + 1] == t[ti] {
                pi += 2;
                ti += 1;
                continue;
            }
        }
        if let Some(sp) = star {
            star_ti += 1;
            ti = star_ti;
            pi = sp + 1;
            continue;
        }
        return false;
    }
    // Text exhausted: only trailing '*' may remain.
    while pi < p.len() {
        if p[pi] == '*' {
            pi += 1;
        } else {
            return false;
        }
    }
    true
}

fn bool_num(b: bool) -> f64 {
    if b { 1.0 } else { 0.0 }
}

/// Convert a finite `f64` to Excel's General-format text (the exact string
/// `=A1&B1` and numeric-key text lookups see). Contract, corpus-locked by
/// `#[cfg(test)]` below:
///
/// 1. `0` (incl. `-0.0`) → `"0"`.
/// 2. Take the shortest round-trip decimal digits (`ryu`), round them to 15
///    significant digits — incrementing when the 16th digit ≥ 5, carrying
///    leftward — on the *shortest representation*, not the exact binary
///    expansion (this reproduces Excel: `(1/3)*3 → "1"`, `=1-2E-16 → "1"`).
/// 3. Normalized exponent `E` (`d.ddd × 10^E`): scientific iff `E ≥ 11`
///    (12+ integer digits) or `E ≤ −10` (`< 1E-9`); fixed otherwise.
/// 4. Fixed: point placed by `E`, trailing zeros trimmed, integer parts shown
///    in full. Scientific: mantissa with point after the first digit, trailing
///    zeros trimmed, exponent `E`+sign+digits (always ≥ 2 digits, e.g.
///    `E+11`, `E-10`, `E+300`).
pub fn number_to_general(n: f64) -> String {
    debug_assert!(n.is_finite());
    if n == 0.0 {
        return "0".to_string();
    }
    let neg = n < 0.0;
    let (digits, dec_exp) = shortest_digits(n.abs());
    let (digits, dec_exp) = round15(digits, dec_exp);
    let e_norm = dec_exp - 1; // value = d1.d2... × 10^e_norm
    let out = if e_norm >= 11 || e_norm <= -10 {
        scientific(&digits, e_norm)
    } else {
        fixed(&digits, dec_exp)
    };
    if neg { format!("-{out}") } else { out }
}

/// Shortest round-trip digits via `ryu`, normalized to `(digits, dec_exp)`
/// with value = `0.digits × 10^dec_exp`. The digit slice has no leading zeros.
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
    // Strip leading zeros (the `0` of `0.xxx`) and fold them into dec_exp.
    let first = digits.iter().position(|&d| d != 0).unwrap_or(0);
    if first > 0 {
        digits.drain(..first);
    }
    let dec_exp = int_digits - first as i32 + exp;
    (digits, dec_exp)
}

/// Round `digits` to 15 significant digits (16th digit ≥ 5 rounds up, carrying
/// leftward; overflow shifts the exponent), then strip trailing zeros.
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

fn scientific(digits: &[u8], e: i32) -> String {
    let mut m = String::new();
    m.push(char::from(b'0' + digits[0]));
    if digits.len() > 1 {
        m.push('.');
        for &d in &digits[1..] {
            m.push(char::from(b'0' + d));
        }
    }
    let sign = if e < 0 { '-' } else { '+' };
    format!("{m}E{sign}{}", e.abs())
}

fn fixed(digits: &[u8], dec_exp: i32) -> String {
    let len = digits.len() as i32;
    if dec_exp >= len {
        let mut s = String::new();
        for &d in digits {
            s.push(char::from(b'0' + d));
        }
        for _ in 0..(dec_exp - len) {
            s.push('0');
        }
        s
    } else if dec_exp > 0 {
        let mut s = String::new();
        for &d in &digits[..dec_exp as usize] {
            s.push(char::from(b'0' + d));
        }
        s.push('.');
        for &d in &digits[dec_exp as usize..] {
            s.push(char::from(b'0' + d));
        }
        s
    } else if dec_exp == 0 {
        let mut s = String::from("0.");
        for &d in digits {
            s.push(char::from(b'0' + d));
        }
        s
    } else {
        let mut s = String::from("0.");
        for _ in 0..(-dec_exp) {
            s.push('0');
        }
        for &d in digits {
            s.push(char::from(b'0' + d));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turbo::calc::value::ArrayValue;

    #[test]
    fn number_to_general_corpus() {
        let cases: &[(f64, &str)] = &[
            (0.0, "0"),
            (-0.0, "0"),
            (1.0, "1"),
            (100.0, "100"),
            (12345678901.0, "12345678901"),
            (123456789012.0, "1.23456789012E+11"),
            (1234000000000.0, "1.234E+12"),
            (1000000000000.0, "1E+12"),
            (123456789012345.0, "1.23456789012345E+14"),
            (99999999999.99, "99999999999.99"),
            (0.5, "0.5"),
            (0.1 + 0.2, "0.3"),
            (1.2 - 1.1, "0.0999999999999999"),
            (1.0 / 3.0, "0.333333333333333"),
            ((1.0 / 3.0) * 3.0, "1"),
            (1.0 - 2e-16, "1"),
            (1.2345678901234567, "1.23456789012346"),
            (std::f64::consts::PI, "3.14159265358979"),
            (1e-9, "0.000000001"),
            (1.5e-9, "0.0000000015"),
            (1e-10, "1E-10"),
            (-(2f64.powi(-55)), "-2.77555756156289E-17"),
            (1e11, "1E+11"),
            (-1.5e11, "-1.5E+11"),
            (1e300, "1E+300"),
            (5e-324, "5E-324"),
        ];
        for (input, expected) in cases {
            assert_eq!(
                number_to_general(*input),
                *expected,
                "number_to_general({input})"
            );
        }
    }

    #[test]
    fn general_format_roundtrips_through_parse() {
        // is_real_num uses number_to_general; the round-trip must hold.
        assert!(is_real_num("123.45"));
        assert!(is_real_num("123456"));
        // "1e5" formats as "100000", so it is NOT a round-trip real number;
        // classify_text still turns it into a Number via the pattern branch.
        assert!(!is_real_num("1e5"));
        assert!(!is_real_num("000123456"));
        assert!(!is_real_num("1.2.3"));
        assert!(!is_real_num(""));
    }

    #[test]
    fn coerce_number_basics() {
        assert_eq!(coerce_number(&CalcValue::Number(3.5)), Ok(3.5));
        assert_eq!(coerce_number(&CalcValue::Bool(true)), Ok(1.0));
        assert_eq!(coerce_number(&CalcValue::Bool(false)), Ok(0.0));
        assert_eq!(coerce_number(&CalcValue::Blank), Ok(0.0));
        assert_eq!(coerce_number(&CalcValue::text("42")), Ok(42.0));
        assert_eq!(coerce_number(&CalcValue::text(" 7 ")), Ok(7.0));
        assert_eq!(coerce_number(&CalcValue::text("12%")), Ok(0.12));
        assert_eq!(
            coerce_number(&CalcValue::text("abc")),
            Err(CalcError::Value)
        );
        assert_eq!(coerce_number(&CalcValue::text("")), Err(CalcError::Value));
        assert_eq!(
            coerce_number(&CalcValue::err(CalcError::Div0)),
            Err(CalcError::Div0)
        );
        assert_eq!(
            coerce_number(&CalcValue::array(ArrayValue::new(
                1,
                1,
                vec![CalcValue::Number(1.0)]
            ))),
            Err(CalcError::Value)
        );
    }

    #[test]
    fn coerce_text_basics() {
        assert_eq!(
            coerce_text(&CalcValue::Number(0.1 + 0.2)),
            Ok("0.3".to_string())
        );
        assert_eq!(coerce_text(&CalcValue::Bool(true)), Ok("TRUE".to_string()));
        assert_eq!(
            coerce_text(&CalcValue::Bool(false)),
            Ok("FALSE".to_string())
        );
        assert_eq!(coerce_text(&CalcValue::Blank), Ok(String::new()));
        assert_eq!(coerce_text(&CalcValue::text("hi")), Ok("hi".to_string()));
        assert_eq!(
            coerce_text(&CalcValue::err(CalcError::Na)),
            Err(CalcError::Na)
        );
    }

    #[test]
    fn comparison_order() {
        use std::cmp::Ordering::*;
        assert_eq!(
            compare(&CalcValue::Number(1.0), &CalcValue::Number(2.0)),
            Ok(Less)
        );
        // cross-type rank: any number < any text < FALSE < TRUE
        assert_eq!(
            compare(&CalcValue::Number(1.0), &CalcValue::text("abc")),
            Ok(Less)
        );
        assert_eq!(
            compare(&CalcValue::text("abc"), &CalcValue::Number(1.0)),
            Ok(Greater)
        );
        assert_eq!(
            compare(&CalcValue::Number(1e300), &CalcValue::text("")),
            Ok(Less)
        );
        assert_eq!(
            compare(&CalcValue::Bool(false), &CalcValue::Bool(true)),
            Ok(Less)
        );
        assert_eq!(
            compare(&CalcValue::Number(0.0), &CalcValue::Bool(false)),
            Ok(Less)
        );
        assert_eq!(
            compare(&CalcValue::Number(1.0), &CalcValue::Bool(true)),
            Ok(Less)
        );
        assert_eq!(
            compare(&CalcValue::text("z"), &CalcValue::Bool(false)),
            Ok(Less)
        );
        assert_eq!(
            compare(&CalcValue::Bool(true), &CalcValue::text("z")),
            Ok(Greater)
        );
        // case-insensitive text order: "B" equals "b", and "b" < "c"
        assert_eq!(
            compare(&CalcValue::text("B"), &CalcValue::text("b")),
            Ok(Equal)
        );
        assert_eq!(
            compare(&CalcValue::text("b"), &CalcValue::text("C")),
            Ok(Less)
        );
        assert_eq!(compare(&CalcValue::Blank, &CalcValue::text("x")), Ok(Less));
        assert_eq!(
            compare(&CalcValue::Blank, &CalcValue::Number(0.0)),
            Ok(Equal)
        );
        assert_eq!(compare(&CalcValue::Blank, &CalcValue::Blank), Ok(Equal));
        assert_eq!(
            compare(&CalcValue::err(CalcError::Na), &CalcValue::Number(1.0)),
            Err(CalcError::Na)
        );
        assert_eq!(
            compare(&CalcValue::Number(1.0), &CalcValue::err(CalcError::Num)),
            Err(CalcError::Num)
        );
        assert_eq!(
            compare_eq(&CalcValue::Number(1.0), &CalcValue::text("1"), false),
            Ok(false)
        );
        assert_eq!(
            compare_eq(&CalcValue::text("1"), &CalcValue::text("1"), false),
            Ok(true)
        );
    }

    #[test]
    fn wildcard_matching() {
        assert!(wildcard_match("a*", "abc"));
        assert!(wildcard_match("*", ""));
        assert!(wildcard_match("*b", "ab"));
        assert!(wildcard_match("a?c", "abc"));
        assert!(!wildcard_match("a?c", "ac"));
        assert!(wildcard_match("a~*", "a*"));
        assert!(!wildcard_match("a~*", "a"));
        assert!(wildcard_match("a~~b", "a~b"));
        assert!(wildcard_match("A*", "abc"));
        assert_eq!(
            compare_eq(&CalcValue::text("abc"), &CalcValue::text("a*"), true),
            Ok(true)
        );
        assert_eq!(
            compare_eq(&CalcValue::text("abc"), &CalcValue::text("b*"), true),
            Ok(false)
        );
    }

    #[test]
    fn classify_text_basics() {
        assert_eq!(classify_text("123"), CalcValue::Number(123.0));
        assert_eq!(classify_text("TRUE"), CalcValue::Bool(true));
        assert_eq!(classify_text("false"), CalcValue::Bool(false));
        assert_eq!(classify_text("#DIV/0!"), CalcValue::err(CalcError::Div0));
        assert_eq!(classify_text("#n/a"), CalcValue::err(CalcError::Na));
        assert_eq!(classify_text("hello"), CalcValue::text("hello"));
        assert_eq!(classify_text("1e5"), CalcValue::Number(100000.0));
        assert_eq!(classify_text("000123456"), CalcValue::Number(123456.0));
        assert_eq!(classify_text(""), CalcValue::text(""));
    }
}
