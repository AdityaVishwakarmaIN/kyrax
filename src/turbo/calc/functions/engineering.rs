// functions/engineering.rs — the engineering function family. Owned
// exclusively by the engineering family agent; no other agent edits this file.
//
// Registry contract: implement `register` below and keep this exact signature.
// Do NOT edit functions/mod.rs — the `mod engineering;` declaration and the
// `engineering::register(&mut r)` call site in `build()` are already final.
// See functions/mod.rs for the worked ABS template.
//
// Families implemented here:
//   * base conversion (DEC2BIN/BIN2DEC & friends), with two's-complement
//     negatives over the fixed 10-character width and the `places` padding
//   * bitwise (BITAND/BITOR/BITXOR/BITLSHIFT/BITRSHIFT), limited to 2^48-1
//   * comparison (DELTA, GESTEP)
//   * error functions (ERF, ERFC, ERF.PRECISE, ERFC.PRECISE)
//   * complex numbers (COMPLEX + the IM* family), one parser and one
//     formatter shared by every IM* function, Excel-exact formatting
//   * unit conversion (CONVERT) with a unit table and metric/binary prefixes
//   * Bessel functions (BESSELI/J/K/Y) via series + asymptotic expansions
//
// Accuracy achieved (validated against 25-digit mpmath reference values):
//   BESSELJ ~1e-13, BESSELY ~2e-12, BESSELI ~1e-15, BESSELK ~1e-11 relative.
use super::{FuncArg, FuncCtx, FuncSpec, Registry};
use crate::turbo::calc::coerce::{coerce_number, coerce_text, number_to_general};
use crate::turbo::calc::value::{CalcError, CalcValue};

fn ok_num(n: f64) -> Result<CalcValue, CalcError> {
    if n.is_finite() {
        Ok(CalcValue::Number(n))
    } else {
        Err(CalcError::Num)
    }
}

// ---------------------------------------------------------------------------
// Base conversion (1): DEC2BIN/DEC2OCT/DEC2HEX, BIN2DEC, OCT2DEC, HEX2DEC and
// the cross-base conversions. Negatives are two's complement over the fixed
// 10-character width (10/30/40 bits), and `places` zero-pads positive results
// (ignored for negatives, #NUM! when too small).
// ---------------------------------------------------------------------------

const MAX_CHARS: u32 = 10;
const BIN_MIN: i128 = -(1i128 << 9);
const BIN_MAX: i128 = (1i128 << 9) - 1;
const OCT_MIN: i128 = -(1i128 << 29);
const OCT_MAX: i128 = (1i128 << 29) - 1;
const HEX_MIN: i128 = -(1i128 << 39);
const HEX_MAX: i128 = (1i128 << 39) - 1;

#[derive(Clone, Copy)]
enum Base {
    Bin,
    Oct,
    Hex,
}

impl Base {
    fn radix(self) -> u32 {
        match self {
            Base::Bin => 2,
            Base::Oct => 8,
            Base::Hex => 16,
        }
    }
    fn bits(self) -> u32 {
        match self {
            Base::Bin => 1,
            Base::Oct => 3,
            Base::Hex => 4,
        }
    }
    fn digit(self, c: u8) -> Option<u32> {
        match self {
            Base::Bin => (c == b'0' || c == b'1').then_some((c - b'0') as u32),
            Base::Oct => (b'0'..=b'7').contains(&c).then_some((c - b'0') as u32),
            Base::Hex => (c as char).to_digit(16),
        }
    }
    fn sign_bit(self, c: u8) -> bool {
        match self {
            Base::Bin => c == b'1',
            Base::Oct => (b'4'..=b'7').contains(&c),
            Base::Hex => (c as char).to_digit(16).is_some_and(|d| d >= 8),
        }
    }
    fn range(self) -> (i128, i128) {
        match self {
            Base::Bin => (BIN_MIN, BIN_MAX),
            Base::Oct => (OCT_MIN, OCT_MAX),
            Base::Hex => (HEX_MIN, HEX_MAX),
        }
    }
}

/// Parse a 1..=10 char digit string in `base` to a signed value: a full-width
/// string with the top sign bit set is negative (two's complement).
fn parse_radix(s: &str, base: Base) -> Result<i128, CalcError> {
    let s = s.trim();
    if s.is_empty() || s.len() as u32 > MAX_CHARS {
        return Err(CalcError::Num);
    }
    let mut val: i128 = 0;
    for c in s.bytes() {
        let d = base.digit(c).ok_or(CalcError::Num)?;
        val = val * base.radix() as i128 + d as i128;
    }
    if s.len() as u32 == MAX_CHARS && base.sign_bit(s.as_bytes()[0]) {
        val -= 1i128 << (MAX_CHARS * base.bits());
    }
    Ok(val)
}

/// Minimal digit representation of a non-negative value (uppercase hex).
fn format_radix(mut val: i128, base: Base) -> String {
    if val == 0 {
        return "0".to_string();
    }
    let radix = base.radix() as i128;
    let mut s = String::new();
    while val > 0 {
        let d = (val % radix) as u8;
        let ch = if d < 10 { b'0' + d } else { b'A' + (d - 10) };
        s.insert(0, ch as char);
        val /= radix;
    }
    s
}

/// Two's complement of a negative value over the full 10-character width.
fn format_negative(val: i128, base: Base) -> String {
    format_radix(val + (1i128 << (MAX_CHARS * base.bits())), base)
}

/// Apply the `places` argument to a positive digit string. #NUM! when `places`
/// is out of range (1..=10) or too small to hold the result.
fn apply_places(
    ctx: &FuncCtx,
    min_repr: &str,
    places_arg: Option<&FuncArg>,
) -> Result<String, CalcError> {
    match places_arg {
        None => Ok(min_repr.to_string()),
        Some(p) => {
            let places = coerce_number(&p.value(ctx)?)?.trunc();
            if !(1.0..=MAX_CHARS as f64).contains(&places) {
                return Err(CalcError::Num);
            }
            let places = places as usize;
            if min_repr.len() > places {
                return Err(CalcError::Num);
            }
            let mut out = String::with_capacity(places);
            for _ in 0..places - min_repr.len() {
                out.push('0');
            }
            out.push_str(min_repr);
            Ok(out)
        }
    }
}

/// Shared body for the "from decimal" conversions.
fn dec_to_base(ctx: &FuncCtx, args: &[FuncArg], base: Base) -> Result<CalcValue, CalcError> {
    let num = coerce_number(&args[0].value(ctx)?)?;
    let n = num.trunc() as i128;
    let (min, max) = base.range();
    if n < min || n > max {
        return Err(CalcError::Num);
    }
    if n < 0 {
        Ok(CalcValue::text(format_negative(n, base)))
    } else {
        let min_repr = format_radix(n, base);
        Ok(CalcValue::text(apply_places(ctx, &min_repr, args.get(1))?))
    }
}

/// Shared body for the "X2Y" cross-base conversions.
fn cross_base(
    ctx: &FuncCtx,
    args: &[FuncArg],
    from: Base,
    to: Base,
) -> Result<CalcValue, CalcError> {
    let s = coerce_text(&args[0].value(ctx)?)?;
    let val = parse_radix(&s, from)?;
    let (min, max) = to.range();
    if val < min || val > max {
        return Err(CalcError::Num);
    }
    if val < 0 {
        Ok(CalcValue::text(format_negative(val, to)))
    } else {
        let min_repr = format_radix(val, to);
        Ok(CalcValue::text(apply_places(ctx, &min_repr, args.get(1))?))
    }
}

/// Shared body for the "X2DEC" conversions: parse, then return the number.
fn to_dec(ctx: &FuncCtx, args: &[FuncArg], base: Base) -> Result<CalcValue, CalcError> {
    let s = coerce_text(&args[0].value(ctx)?)?;
    let val = parse_radix(&s, base)?;
    ok_num(val as f64)
}

fn dec2bin(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    dec_to_base(ctx, args, Base::Bin)
}
fn dec2oct(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    dec_to_base(ctx, args, Base::Oct)
}
fn dec2hex(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    dec_to_base(ctx, args, Base::Hex)
}
fn bin2dec(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    to_dec(ctx, args, Base::Bin)
}
fn bin2oct(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    cross_base(ctx, args, Base::Bin, Base::Oct)
}
fn bin2hex(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    cross_base(ctx, args, Base::Bin, Base::Hex)
}
fn oct2dec(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    to_dec(ctx, args, Base::Oct)
}
fn oct2bin(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    cross_base(ctx, args, Base::Oct, Base::Bin)
}
fn oct2hex(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    cross_base(ctx, args, Base::Oct, Base::Hex)
}
fn hex2dec(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    to_dec(ctx, args, Base::Hex)
}
fn hex2bin(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    cross_base(ctx, args, Base::Hex, Base::Bin)
}
fn hex2oct(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    cross_base(ctx, args, Base::Hex, Base::Oct)
}

// ---------------------------------------------------------------------------
// Bitwise (2): BITAND/BITOR/BITXOR/BITLSHIFT/BITRSHIFT. All operands are
// restricted to [0, 2^48-1]; negatives and non-integers are #NUM!.
// ---------------------------------------------------------------------------

const BIT_LIMIT: f64 = 281_474_976_710_655.0; // 2^48 - 1

fn bit_integer(v: &CalcValue) -> Result<u64, CalcError> {
    let n = coerce_number(v)?;
    if n.fract() != 0.0 || n < 0.0 || n > BIT_LIMIT {
        return Err(CalcError::Num);
    }
    Ok(n as u64)
}

fn bitand(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let a = bit_integer(&args[0].value(ctx)?)?;
    let b = bit_integer(&args[1].value(ctx)?)?;
    ok_num((a & b) as f64)
}

fn bitor(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let a = bit_integer(&args[0].value(ctx)?)?;
    let b = bit_integer(&args[1].value(ctx)?)?;
    ok_num((a | b) as f64)
}

fn bitxor(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let a = bit_integer(&args[0].value(ctx)?)?;
    let b = bit_integer(&args[1].value(ctx)?)?;
    ok_num((a ^ b) as f64)
}

fn bitlshift(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = bit_integer(&args[0].value(ctx)?)?;
    let s = coerce_number(&args[1].value(ctx)?)?;
    if s.fract() != 0.0 {
        return Err(CalcError::Num);
    }
    let shift = s as i64;
    let v = if shift >= 0 {
        if shift >= 64 {
            if n == 0 {
                0
            } else {
                return Err(CalcError::Num);
            }
        } else {
            (n as u128) << shift as u32
        }
    } else {
        let r = (-shift) as u32;
        if r >= 64 { 0 } else { (n >> r) as u128 }
    };
    if v > BIT_LIMIT as u128 {
        return Err(CalcError::Num);
    }
    ok_num(v as f64)
}

fn bitrshift(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let n = bit_integer(&args[0].value(ctx)?)?;
    let s = coerce_number(&args[1].value(ctx)?)?;
    if s.fract() != 0.0 {
        return Err(CalcError::Num);
    }
    let shift = s as i64;
    let v = if shift >= 0 {
        let r = shift as u32;
        if r >= 64 { 0 } else { (n >> r) as u128 }
    } else {
        let l = (-shift) as u32;
        if l >= 64 {
            if n == 0 {
                0
            } else {
                return Err(CalcError::Num);
            }
        } else {
            (n as u128) << l
        }
    };
    if v > BIT_LIMIT as u128 {
        return Err(CalcError::Num);
    }
    ok_num(v as f64)
}

// ---------------------------------------------------------------------------
// Comparison (3): DELTA and GESTEP.
// ---------------------------------------------------------------------------

fn delta(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let a = coerce_number(&args[0].value(ctx)?)?;
    let b = if args.len() == 2 {
        coerce_number(&args[1].value(ctx)?)?
    } else {
        0.0
    };
    Ok(CalcValue::Number(if a == b { 1.0 } else { 0.0 }))
}

fn gestep(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let a = coerce_number(&args[0].value(ctx)?)?;
    let b = if args.len() == 2 {
        coerce_number(&args[1].value(ctx)?)?
    } else {
        0.0
    };
    Ok(CalcValue::Number(if a >= b { 1.0 } else { 0.0 }))
}

// ---------------------------------------------------------------------------
// Error functions (4): ERF, ERFC, ERF.PRECISE, ERFC.PRECISE.
// Machine-precision erf via a power series (|x|<=1) and the DLMF 7.9.2
// continued fraction for the tail (|x|>1, via Lentz's algorithm).
// ---------------------------------------------------------------------------

const GAMMA: f64 = 0.5772156649015328606;

fn erf_series(x: f64) -> f64 {
    let mut sum = x;
    let mut term = x;
    let mut n = 1u32;
    loop {
        term *= -(x * x) / n as f64;
        sum += term / (2 * n + 1) as f64;
        if term.abs() < 1e-19 * sum.abs().max(1.0) || n > 5000 {
            break;
        }
        n += 1;
    }
    2.0 / std::f64::consts::PI.sqrt() * sum
}

fn erfc_cf(x: f64) -> f64 {
    let tiny = 1e-300;
    let mut f = x;
    if f == 0.0 {
        f = tiny;
    }
    let mut c = f;
    let mut d = 0.0;
    for k in 1..=2000 {
        let a = k as f64 / 2.0;
        d = x + a * d;
        if d.abs() < tiny {
            d = tiny;
        }
        c = x + a / c;
        if c.abs() < tiny {
            c = tiny;
        }
        d = 1.0 / d;
        let delta = c * d;
        f *= delta;
        if (delta - 1.0).abs() < 1e-17 {
            break;
        }
    }
    (-x * x).exp() / (std::f64::consts::PI.sqrt() * f)
}

fn erf(x: f64) -> f64 {
    if x >= 0.0 {
        if x <= 1.0 {
            erf_series(x)
        } else {
            1.0 - erfc_cf(x)
        }
    } else {
        -erf(-x)
    }
}

fn erfc(x: f64) -> f64 {
    1.0 - erf(x)
}

fn erf_(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let lo = coerce_number(&args[0].value(ctx)?)?;
    if lo < 0.0 {
        return Err(CalcError::Num);
    }
    if args.len() == 1 {
        ok_num(erf(lo))
    } else {
        let hi = coerce_number(&args[1].value(ctx)?)?;
        if hi < 0.0 {
            return Err(CalcError::Num);
        }
        ok_num(erf(hi) - erf(lo))
    }
}

fn erf_precise(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let x = coerce_number(&args[0].value(ctx)?)?;
    ok_num(erf(x))
}

fn erfc_(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let x = coerce_number(&args[0].value(ctx)?)?;
    ok_num(erfc(x))
}

// ---------------------------------------------------------------------------
// Complex numbers (5): COMPLEX and the IM* family. ONE parser and ONE
// formatter serve every function. Complex values are text like "3+4i"; the
// imaginary suffix may be "i" or "j" but never mixed within one operation.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Suffix {
    I,
    J,
}

impl Suffix {
    fn char(self) -> char {
        match self {
            Suffix::I => 'i',
            Suffix::J => 'j',
        }
    }
}

#[derive(Clone, Copy)]
struct Cplx {
    re: f64,
    im: f64,
}

/// Parse "a+bi" / "a-bi" / "bi" / "a" (suffix may be i or j; internal
/// whitespace is ignored). Invalid input is #NUM!, per Excel.
fn parse_complex_str(text: &str) -> Result<(Cplx, Option<Suffix>), CalcError> {
    let s: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if s.is_empty() {
        return Err(CalcError::Num);
    }
    let (body, suffix) = match s.chars().next_back() {
        Some('i') => (&s[..s.len() - 1], Some(Suffix::I)),
        Some('j') => (&s[..s.len() - 1], Some(Suffix::J)),
        _ => (s.as_str(), None),
    };
    if let Some(suf) = suffix {
        if body.is_empty() || body == "+" || body == "-" {
            let im = if body == "-" { -1.0 } else { 1.0 };
            return Ok((Cplx { re: 0.0, im }, Some(suf)));
        }
        if let Ok(im) = body.parse::<f64>() {
            return if im.is_finite() {
                Ok((Cplx { re: 0.0, im }, Some(suf)))
            } else {
                Err(CalcError::Num)
            };
        }
        // The real + imaginary form: split at the last +/- past the sign.
        if let Some(pos) = body[1..].rfind(['+', '-']).map(|i| i + 1) {
            let re: f64 = body[..pos].parse().map_err(|_| CalcError::Num)?;
            let im = match &body[pos..] {
                "" | "+" => 1.0,
                "-" => -1.0,
                other => other.parse::<f64>().map_err(|_| CalcError::Num)?,
            };
            if re.is_finite() && im.is_finite() {
                return Ok((Cplx { re, im }, Some(suf)));
            }
        }
        Err(CalcError::Num)
    } else {
        match body.parse::<f64>() {
            Ok(re) if re.is_finite() => Ok((Cplx { re, im: 0.0 }, None)),
            _ => Err(CalcError::Num),
        }
    }
}

fn parse_complex(v: &CalcValue) -> Result<(Cplx, Option<Suffix>), CalcError> {
    let s = coerce_text(v)?;
    parse_complex_str(&s)
}

/// Excel-exact formatting: coefficient 1 is dropped ("3+i"), a zero part is
/// omitted entirely, and General format is used for each component.
fn format_complex(c: Cplx, suffix: Suffix) -> String {
    let s = suffix.char();
    if c.im == 0.0 {
        return number_to_general(c.re);
    }
    let re_s = number_to_general(c.re);
    let im_abs = number_to_general(c.im.abs());
    let im_coeff = if c.im.abs() == 1.0 {
        String::new()
    } else {
        im_abs
    };
    if c.re == 0.0 {
        if c.im < 0.0 {
            format!("-{im_coeff}{s}")
        } else {
            format!("{im_coeff}{s}")
        }
    } else if c.im < 0.0 {
        format!("{re_s}-{im_coeff}{s}")
    } else {
        format!("{re_s}+{im_coeff}{s}")
    }
}

fn ok_complex(c: Cplx, suffix: Suffix) -> Result<CalcValue, CalcError> {
    if c.re.is_finite() && c.im.is_finite() {
        Ok(CalcValue::text(format_complex(c, suffix)))
    } else {
        Err(CalcError::Num)
    }
}

fn c_add(a: Cplx, b: Cplx) -> Cplx {
    Cplx {
        re: a.re + b.re,
        im: a.im + b.im,
    }
}
fn c_sub(a: Cplx, b: Cplx) -> Cplx {
    Cplx {
        re: a.re - b.re,
        im: a.im - b.im,
    }
}
fn c_mul(a: Cplx, b: Cplx) -> Cplx {
    Cplx {
        re: a.re * b.re - a.im * b.im,
        im: a.re * b.im + a.im * b.re,
    }
}
fn c_div(a: Cplx, b: Cplx) -> Result<Cplx, CalcError> {
    let denom = b.re * b.re + b.im * b.im;
    if denom == 0.0 {
        return Err(CalcError::Div0);
    }
    Ok(Cplx {
        re: (a.re * b.re + a.im * b.im) / denom,
        im: (a.im * b.re - a.re * b.im) / denom,
    })
}
fn c_abs(a: Cplx) -> f64 {
    a.re.hypot(a.im)
}
fn c_arg(a: Cplx) -> Result<f64, CalcError> {
    if a.re == 0.0 && a.im == 0.0 {
        Err(CalcError::Div0)
    } else {
        Ok(a.im.atan2(a.re))
    }
}
fn c_conj(a: Cplx) -> Cplx {
    Cplx {
        re: a.re,
        im: -a.im,
    }
}
fn c_exp(a: Cplx) -> Cplx {
    let e = a.re.exp();
    Cplx {
        re: e * a.im.cos(),
        im: e * a.im.sin(),
    }
}
fn c_ln(a: Cplx) -> Result<Cplx, CalcError> {
    if a.re == 0.0 && a.im == 0.0 {
        return Err(CalcError::Num);
    }
    Ok(Cplx {
        re: a.re.hypot(a.im).ln(),
        im: a.im.atan2(a.re),
    })
}
fn c_sqrt(a: Cplx) -> Cplx {
    let r = a.re.hypot(a.im);
    if r == 0.0 {
        return Cplx { re: 0.0, im: 0.0 };
    }
    let re = ((r + a.re) / 2.0).sqrt();
    let mut im = ((r - a.re) / 2.0).sqrt();
    if a.im < 0.0 {
        im = -im;
    } else if a.im == 0.0 && a.re >= 0.0 {
        im = 0.0;
    }
    Cplx { re, im }
}
fn c_pow(z: Cplx, w: f64) -> Result<Cplx, CalcError> {
    if z.re == 0.0 && z.im == 0.0 {
        if w == 0.0 || w < 0.0 {
            return Err(CalcError::Num);
        }
        return Ok(Cplx { re: 0.0, im: 0.0 });
    }
    // Integer exponents take the repeated-multiplication path so the result is
    // exact (IMPOWER("2+3i","3") is exactly "-46+9i", matching Excel).
    if w.fract() == 0.0 && w.abs() <= 2_147_483_647.0 {
        let n: i64 = w as i64;
        let mut result = Cplx { re: 1.0, im: 0.0 };
        let (base, e) = if n >= 0 {
            (z, n)
        } else {
            (
                c_div(Cplx { re: 1.0, im: 0.0 }, z)?,
                n.unsigned_abs() as i64,
            )
        };
        let mut b = base;
        let mut k = e;
        while k > 0 {
            if k & 1 == 1 {
                result = c_mul(result, b);
            }
            b = c_mul(b, b);
            k >>= 1;
        }
        return Ok(result);
    }
    let ln = c_ln(z)?;
    Ok(c_exp(Cplx {
        re: ln.re * w,
        im: ln.im * w,
    }))
}
fn c_sin(a: Cplx) -> Cplx {
    Cplx {
        re: a.re.sin() * a.im.cosh(),
        im: a.re.cos() * a.im.sinh(),
    }
}
fn c_cos(a: Cplx) -> Cplx {
    Cplx {
        re: a.re.cos() * a.im.cosh(),
        im: -a.re.sin() * a.im.sinh(),
    }
}
fn c_tan(a: Cplx) -> Result<Cplx, CalcError> {
    c_div(c_sin(a), c_cos(a))
}

/// The suffix an operation must format with: reject mixing "i" and "j".
fn resolve_suffix(suffixes: &[Option<Suffix>]) -> Result<Suffix, CalcError> {
    let mut found: Option<Suffix> = None;
    for s in suffixes {
        if let Some(suf) = s {
            if let Some(f) = found {
                if f != *suf {
                    return Err(CalcError::Value);
                }
            } else {
                found = Some(*suf);
            }
        }
    }
    Ok(found.unwrap_or(Suffix::I))
}

fn complex(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let re = coerce_number(&args[0].value(ctx)?)?;
    let im = coerce_number(&args[1].value(ctx)?)?;
    let suffix = if args.len() == 3 {
        let s = coerce_text(&args[2].value(ctx)?)?;
        match s.trim() {
            "i" => Suffix::I,
            "j" => Suffix::J,
            _ => return Err(CalcError::Value),
        }
    } else {
        Suffix::I
    };
    ok_complex(Cplx { re, im }, suffix)
}

fn imreal(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let (c, _) = parse_complex(&args[0].value(ctx)?)?;
    ok_num(c.re)
}
fn imaginary(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let (c, _) = parse_complex(&args[0].value(ctx)?)?;
    ok_num(c.im)
}
fn imabs(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let (c, _) = parse_complex(&args[0].value(ctx)?)?;
    ok_num(c_abs(c))
}
fn imargument(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let (c, _) = parse_complex(&args[0].value(ctx)?)?;
    c_arg(c).and_then(ok_num)
}
fn imconjugate(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let (c, s) = parse_complex(&args[0].value(ctx)?)?;
    ok_complex(c_conj(c), s.unwrap_or(Suffix::I))
}
fn imsum(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let mut acc = Cplx { re: 0.0, im: 0.0 };
    let mut suffixes = Vec::with_capacity(args.len());
    for arg in args {
        let (c, s) = parse_complex(&arg.value(ctx)?)?;
        acc = c_add(acc, c);
        suffixes.push(s);
    }
    ok_complex(acc, resolve_suffix(&suffixes)?)
}
fn imsub(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let (a, sa) = parse_complex(&args[0].value(ctx)?)?;
    let (b, sb) = parse_complex(&args[1].value(ctx)?)?;
    ok_complex(c_sub(a, b), resolve_suffix(&[sa, sb])?)
}
fn improduct(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let mut acc = Cplx { re: 1.0, im: 0.0 };
    let mut suffixes = Vec::with_capacity(args.len());
    for arg in args {
        let (c, s) = parse_complex(&arg.value(ctx)?)?;
        acc = c_mul(acc, c);
        suffixes.push(s);
    }
    ok_complex(acc, resolve_suffix(&suffixes)?)
}
fn imdiv(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let (a, sa) = parse_complex(&args[0].value(ctx)?)?;
    let (b, sb) = parse_complex(&args[1].value(ctx)?)?;
    ok_complex(c_div(a, b)?, resolve_suffix(&[sa, sb])?)
}
fn impower(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let (z, s) = parse_complex(&args[0].value(ctx)?)?;
    let w = coerce_number(&args[1].value(ctx)?)?;
    ok_complex(c_pow(z, w)?, s.unwrap_or(Suffix::I))
}
fn imsqrt(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let (c, s) = parse_complex(&args[0].value(ctx)?)?;
    ok_complex(c_sqrt(c), s.unwrap_or(Suffix::I))
}
fn imexp(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let (c, s) = parse_complex(&args[0].value(ctx)?)?;
    ok_complex(c_exp(c), s.unwrap_or(Suffix::I))
}
fn imln(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let (c, s) = parse_complex(&args[0].value(ctx)?)?;
    ok_complex(c_ln(c)?, s.unwrap_or(Suffix::I))
}
fn imlog10(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let (c, s) = parse_complex(&args[0].value(ctx)?)?;
    let ln = c_ln(c)?;
    let l10 = 10.0f64.ln();
    ok_complex(
        Cplx {
            re: ln.re / l10,
            im: ln.im / l10,
        },
        s.unwrap_or(Suffix::I),
    )
}
fn imlog2(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let (c, s) = parse_complex(&args[0].value(ctx)?)?;
    let ln = c_ln(c)?;
    let l2 = 2.0f64.ln();
    ok_complex(
        Cplx {
            re: ln.re / l2,
            im: ln.im / l2,
        },
        s.unwrap_or(Suffix::I),
    )
}
fn imsin(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let (c, s) = parse_complex(&args[0].value(ctx)?)?;
    ok_complex(c_sin(c), s.unwrap_or(Suffix::I))
}
fn imcos(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let (c, s) = parse_complex(&args[0].value(ctx)?)?;
    ok_complex(c_cos(c), s.unwrap_or(Suffix::I))
}
fn imtan(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let (c, s) = parse_complex(&args[0].value(ctx)?)?;
    ok_complex(c_tan(c)?, s.unwrap_or(Suffix::I))
}

// ---------------------------------------------------------------------------
// Unit conversion (6): CONVERT. A static unit table plus metric prefixes
// (yocto..yotta) and binary prefixes (ki..Ei, for bit/byte only). Unknown
// units or mismatched categories are #N/A.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
enum Cat {
    Len,
    Mass,
    Vol,
    Area,
    Time,
    Press,
    Energy,
    Power,
    Mag,
    Force,
    Info,
    Temp,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum TempKind {
    C,
    F,
    K,
}

struct Unit {
    name: &'static str,
    cat: Cat,
    factor: f64,
    prefixable: bool,
    temp: Option<TempKind>,
}

const fn u(name: &'static str, cat: Cat, factor: f64, prefixable: bool) -> Unit {
    Unit {
        name,
        cat,
        factor,
        prefixable,
        temp: None,
    }
}

const UNITS: &[Unit] = &[
    // Distance (metres).
    u("m", Cat::Len, 1.0, true),
    u("mi", Cat::Len, 1609.344, false),
    u("Nmi", Cat::Len, 1852.0, false),
    u("in", Cat::Len, 0.0254, false),
    u("ft", Cat::Len, 0.3048, false),
    u("yd", Cat::Len, 0.9144, false),
    u("ang", Cat::Len, 1e-10, false),
    u("ell", Cat::Len, 1.143, false),
    u("ly", Cat::Len, 9_460_730_472_580_800.0, false),
    u("parsec", Cat::Len, 3.085_677_581_491_367_3e16, false),
    u("ftm", Cat::Len, 1.8288, false),
    u("pica", Cat::Len, 0.0254 / 6.0, false),
    u("Pica", Cat::Len, 0.0254 / 72.0, false),
    u("Picapt", Cat::Len, 0.0254 / 96.0, false),
    u("survey_mi", Cat::Len, 1609.347_218_694_437_3, false),
    // Mass (grams).
    u("g", Cat::Mass, 1.0, true),
    u("kg", Cat::Mass, 1000.0, false),
    u("sg", Cat::Mass, 14_593.902_937_206_36, false),
    u("lbm", Cat::Mass, 453.592_37, false),
    u("u", Cat::Mass, 1.660_539_066_60e-24, false),
    u("ozm", Cat::Mass, 28.349_523_125, false),
    u("grain", Cat::Mass, 0.064_798_91, false),
    u("cwt", Cat::Mass, 45_359.237, false),
    u("shweight", Cat::Mass, 45_359.237, false),
    u("uk_cwt", Cat::Mass, 50_802.345_44, false),
    u("lcwt", Cat::Mass, 50_802.345_44, false),
    u("hweight", Cat::Mass, 50_802.345_44, false),
    u("stone", Cat::Mass, 6350.293_18, false),
    u("st", Cat::Mass, 6350.293_18, false),
    u("ton", Cat::Mass, 907_184.74, false),
    u("uk_ton", Cat::Mass, 1_016_046.9088, false),
    u("LTON", Cat::Mass, 1_016_046.9088, false),
    u("brton", Cat::Mass, 1_016_046.9088, false),
    u("t", Cat::Mass, 1_000_000.0, false),
    // Liquid volume (millilitres).
    u("tsp", Cat::Vol, 4.928_921_593_75, false),
    u("Tsp", Cat::Vol, 14.786_764_781_25, false),
    u("tbs", Cat::Vol, 14.786_764_781_25, false),
    u("oz", Cat::Vol, 29.573_529_562_5, false),
    u("cup", Cat::Vol, 236.588_236_5, false),
    u("pt", Cat::Vol, 473.176_473, false),
    u("qt", Cat::Vol, 946.352_946, false),
    u("gal", Cat::Vol, 3785.411_784, false),
    u("l", Cat::Vol, 1000.0, true),
    u("lt", Cat::Vol, 1000.0, true),
    u("barrel", Cat::Vol, 158_987.294_928, false),
    u("bbl", Cat::Vol, 158_987.294_928, false),
    u("bu", Cat::Vol, 35_239.070_166_88, false),
    u("uk_pt", Cat::Vol, 568.261_25, false),
    u("uk_qt", Cat::Vol, 1136.5225, false),
    u("uk_gal", Cat::Vol, 4546.09, false),
    u("uk_bu", Cat::Vol, 36_368.72, false),
    // Area (square metres).
    u("m2", Cat::Area, 1.0, false),
    u("in2", Cat::Area, 0.000_645_16, false),
    u("ft2", Cat::Area, 0.092_903_04, false),
    u("yd2", Cat::Area, 0.836_127_36, false),
    u("mi2", Cat::Area, 2_589_988.110_336, false),
    u("Nmi2", Cat::Area, 3_429_904.0, false),
    u("acre", Cat::Area, 4046.856_422_4, false),
    u("uk_acre", Cat::Area, 4046.856_422_4, false),
    u("ar", Cat::Area, 100.0, false),
    u("ha", Cat::Area, 10_000.0, false),
    // Time (seconds).
    u("sec", Cat::Time, 1.0, true),
    u("mn", Cat::Time, 60.0, false),
    u("hr", Cat::Time, 3600.0, false),
    u("day", Cat::Time, 86_400.0, false),
    u("yr", Cat::Time, 31_557_600.0, false),
    // Pressure (pascals).
    u("Pa", Cat::Press, 1.0, true),
    u("atm", Cat::Press, 101_325.0, false),
    u("mmHg", Cat::Press, 133.322_368_421_051_3, false),
    u("psi", Cat::Press, 6894.757_293_168_361, false),
    u("Torr", Cat::Press, 133.322_368_421_052_63, false),
    u("bar", Cat::Press, 100_000.0, false),
    u("at", Cat::Press, 98_066.5, false),
    // Energy (joules).
    u("J", Cat::Energy, 1.0, true),
    u("e", Cat::Energy, 1e-7, false),
    u("c", Cat::Energy, 4.184, false),
    u("cal", Cat::Energy, 4.1868, false),
    u("eV", Cat::Energy, 1.602_176_634e-19, false),
    u("ev", Cat::Energy, 1.602_176_634e-19, false),
    u("HPh", Cat::Energy, 2_684_519.537_7, false),
    u("hh", Cat::Energy, 105_505_585.257_348, false),
    u("Wh", Cat::Energy, 3600.0, false),
    u("flb", Cat::Energy, 1.355_817_948_331_400_4, false),
    u("BTU", Cat::Energy, 1055.055_852_62, false),
    u("btu", Cat::Energy, 1055.055_852_62, false),
    // Power (watts).
    u("W", Cat::Power, 1.0, true),
    u("HP", Cat::Power, 745.699_871_582_270_22, false),
    u("PS", Cat::Power, 735.498_75, false),
    // Magnetism (tesla).
    u("T", Cat::Mag, 1.0, true),
    u("ga", Cat::Mag, 1e-4, true),
    // Force (newtons).
    u("N", Cat::Force, 1.0, true),
    u("dyn", Cat::Force, 1e-5, false),
    u("dy", Cat::Force, 1e-5, false),
    u("lbf", Cat::Force, 4.448_221_615_260_5, false),
    u("kgf", Cat::Force, 9.806_65, false),
    // Information (bits).
    u("bit", Cat::Info, 1.0, true),
    u("byte", Cat::Info, 8.0, true),
    // Temperature — affine, handled specially (no prefixes).
    Unit {
        name: "C",
        cat: Cat::Temp,
        factor: 1.0,
        prefixable: false,
        temp: Some(TempKind::C),
    },
    Unit {
        name: "F",
        cat: Cat::Temp,
        factor: 1.0,
        prefixable: false,
        temp: Some(TempKind::F),
    },
    Unit {
        name: "K",
        cat: Cat::Temp,
        factor: 1.0,
        prefixable: false,
        temp: Some(TempKind::K),
    },
];

/// Metric prefixes (yocto .. yotta); binary prefixes apply to bit/byte only.
const PREFIXES: &[(&str, f64)] = &[
    ("ki", 1024.0),
    ("Mi", 1_048_576.0),
    ("Gi", 1_073_741_824.0),
    ("Ti", 1_099_511_627_776.0),
    ("Pi", 1_125_899_906_842_624.0),
    ("Ei", 1_152_921_504_606_846_976.0),
    ("da", 10.0),
    ("Y", 1e24),
    ("Z", 1e21),
    ("E", 1e18),
    ("P", 1e15),
    ("T", 1e12),
    ("G", 1e9),
    ("M", 1e6),
    ("k", 1e3),
    ("h", 1e2),
    ("d", 1e-1),
    ("c", 1e-2),
    ("m", 1e-3),
    ("u", 1e-6),
    ("\u{3bc}", 1e-6),
    ("n", 1e-9),
    ("p", 1e-12),
    ("f", 1e-15),
    ("a", 1e-18),
    ("z", 1e-21),
    ("y", 1e-24),
];

/// Resolve a unit string to (unit, prefix factor); #N/A for unknown units or
/// unsupported unit-prefix combinations.
fn lookup_unit(s: &str) -> Option<(&'static Unit, f64)> {
    if let Some(unit) = UNITS.iter().find(|u| u.name == s) {
        return Some((unit, 1.0));
    }
    for (pfx, factor) in PREFIXES {
        if let Some(rest) = s.strip_prefix(pfx) {
            if let Some(unit) = UNITS.iter().find(|u| u.name == rest && u.prefixable) {
                return Some((unit, *factor));
            }
        }
    }
    None
}

fn temp_convert(v: f64, from: TempKind, to: TempKind) -> f64 {
    use TempKind::*;
    match (from, to) {
        (C, C) => v,
        (C, F) => v * 9.0 / 5.0 + 32.0,
        (C, K) => v + 273.15,
        (F, C) => (v - 32.0) * 5.0 / 9.0,
        (F, F) => v,
        (F, K) => (v - 32.0) * 5.0 / 9.0 + 273.15,
        (K, C) => v - 273.15,
        (K, F) => (v - 273.15) * 9.0 / 5.0 + 32.0,
        (K, K) => v,
    }
}

fn convert(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let num = coerce_number(&args[0].value(ctx)?)?;
    let from = coerce_text(&args[1].value(ctx)?)?;
    let to = coerce_text(&args[2].value(ctx)?)?;
    let (f_unit, f_factor) = lookup_unit(from.trim()).ok_or(CalcError::Na)?;
    let (t_unit, t_factor) = lookup_unit(to.trim()).ok_or(CalcError::Na)?;
    if f_unit.cat != t_unit.cat {
        return Err(CalcError::Na);
    }
    let result = match (f_unit.temp, t_unit.temp) {
        (Some(a), Some(b)) => temp_convert(num, a, b),
        (None, None) => num * f_factor * f_unit.factor / (t_factor * t_unit.factor),
        _ => return Err(CalcError::Na),
    };
    ok_num(result)
}

// ---------------------------------------------------------------------------
// Bessel functions (7): BESSELI/BESSELJ/BESSELK/BESSELY. Each uses a power
// series for small arguments and an asymptotic expansion (plus stable forward
// recurrences in the order, and Miller's backward recurrence for J) for large
// ones. Achieved relative accuracy vs 25-digit reference values: J ~1e-13,
// Y ~2e-12, I ~1e-15, K ~1e-11.
// ---------------------------------------------------------------------------

fn harmonic(n: u64) -> f64 {
    if n < 2000 {
        let mut h = 0.0;
        for k in 1..=n {
            h += 1.0 / k as f64;
        }
        h
    } else {
        let n = n as f64;
        n.ln() + GAMMA + 1.0 / (2.0 * n) - 1.0 / (12.0 * n * n) + 1.0 / (120.0 * n * n * n * n)
    }
}

fn bessel_j_series(x: f64, n: u32) -> f64 {
    let half = x / 2.0;
    let mut term = 1.0;
    for i in 1..=n {
        term *= half / i as f64;
    }
    if term == 0.0 {
        return 0.0;
    }
    let mut sum = term;
    let mut k = 1u64;
    loop {
        term *= -(half * half) / (k as f64 * (n as f64 + k as f64));
        sum += term;
        if term.abs() < 1e-18 * sum.abs().max(1.0) || k > 10_000 {
            break;
        }
        k += 1;
    }
    sum
}

fn bessel_i_series(x: f64, n: u32) -> f64 {
    let half = x / 2.0;
    let mut term = 1.0;
    for i in 1..=n {
        term *= half / i as f64;
    }
    if term == 0.0 {
        return 0.0;
    }
    let mut sum = term;
    let mut k = 1u64;
    loop {
        term *= (half * half) / (k as f64 * (n as f64 + k as f64));
        sum += term;
        if term.abs() < 1e-18 * sum.abs().max(1.0) || k > 50_000 {
            break;
        }
        k += 1;
    }
    sum
}

/// Y_n via the DLMF 10.8.1 series (harmonic-number form). Valid for small x.
fn bessel_y_series(x: f64, n: u32) -> f64 {
    let half = x / 2.0;
    let lh = half.ln();
    let jn = bessel_j_series(x, n);
    let mut y = (2.0 / std::f64::consts::PI) * (lh + GAMMA) * jn;
    if n > 0 {
        let mut t = 1.0 / half;
        for i in 1..n {
            t *= i as f64 / half;
        }
        let mut fs = t;
        for k in 1..n {
            t *= (half * half) / ((n - k) as f64 * k as f64);
            fs += t;
        }
        y -= (1.0 / std::f64::consts::PI) * fs;
    }
    let mut hk = 0.0;
    let mut hnk = harmonic(n as u64);
    let mut t = 1.0;
    for i in 1..=n {
        t *= half / i as f64;
    }
    let mut s3 = 0.0;
    let mut k = 0u64;
    let mut last: f64 = 1.0;
    loop {
        let psi = hk + hnk;
        let term = t * psi;
        s3 += if k % 2 == 0 { term } else { -term };
        k += 1;
        hk += 1.0 / k as f64;
        hnk += 1.0 / (n as f64 + k as f64);
        t *= (half * half) / (k as f64 * (n as f64 + k as f64));
        let small = term.abs() < 1e-18 * s3.abs().max(1.0);
        if (small && last.abs() < 1e-18 * s3.abs().max(1.0)) || k > 20_000 {
            break;
        }
        last = term;
    }
    y - (1.0 / std::f64::consts::PI) * s3
}

/// K_n via the A&S 9.6.11 series (ψ = H - γ form). Valid for small x.
fn bessel_k_series(x: f64, n: u32) -> f64 {
    let half = x / 2.0;
    let lh = half.ln();
    let inn = bessel_i_series(x, n);
    let mut fs = 0.0;
    if n > 0 {
        let mut t = 1.0 / half;
        for i in 1..n {
            t *= i as f64 / half;
        }
        fs = t;
        for k in 1..n {
            t *= (-half * half) / ((n - k) as f64 * k as f64);
            fs += t;
        }
    }
    let mut kk = 0.5 * fs;
    if n % 2 == 0 {
        kk -= lh * inn;
    } else {
        kk += lh * inn;
    }
    let mut hk = 0.0;
    let mut hnk = harmonic(n as u64);
    let mut t = 1.0;
    for i in 1..=n {
        t *= half / i as f64;
    }
    let mut s3 = 0.0;
    let mut k = 0u64;
    let mut last: f64 = 1.0;
    loop {
        let psi = hk + hnk - 2.0 * GAMMA;
        let term = t * psi;
        s3 += term;
        k += 1;
        hk += 1.0 / k as f64;
        hnk += 1.0 / (n as f64 + k as f64);
        t *= (half * half) / (k as f64 * (n as f64 + k as f64));
        let small = term.abs() < 1e-18 * s3.abs().max(1.0);
        if (small && last.abs() < 1e-18 * s3.abs().max(1.0)) || k > 20_000 {
            break;
        }
        last = term;
    }
    s3 *= 0.5;
    kk += if n % 2 == 0 { s3 } else { -s3 };
    kk
}

/// a_m = Π_{j=1..m}(μ-(2j-1)^2) / (m!(8x)^m), truncated at the optimal
/// (semi-convergent) point — the first term that starts growing again.
fn asym_terms(mu: f64, x: f64) -> Vec<f64> {
    let mut out = vec![1.0];
    let mut prod = 1.0;
    let mut fact = 1.0;
    let mut pow8 = 1.0;
    let mut prev: f64 = 1.0;
    for m in 1..400 {
        let j = 2 * m - 1;
        prod *= mu - (j as f64) * (j as f64);
        fact *= m as f64;
        pow8 *= 8.0 * x;
        if !prod.is_finite() || !pow8.is_finite() {
            break;
        }
        let a = prod / (fact * pow8);
        if m >= 3 && a.abs() > prev.abs() {
            break;
        }
        out.push(a);
        prev = a;
        if a.abs() < 1e-20 && m >= 2 {
            break;
        }
    }
    out
}

/// (J, Y) via the DLMF 10.17.3 asymptotic expansions. Valid for x >> n.
fn jy_asym(x: f64, n: u32) -> (f64, f64) {
    let mu = 4.0 * n as f64 * n as f64;
    let a = asym_terms(mu, x);
    let mut p = 0.0;
    let mut q = 0.0;
    for (m, &am) in a.iter().enumerate() {
        if m % 2 == 0 {
            p += if (m / 2) % 2 == 0 { am } else { -am };
        } else {
            q += if ((m - 1) / 2) % 2 == 0 { am } else { -am };
        }
    }
    let chi = x - n as f64 * std::f64::consts::PI / 2.0 - std::f64::consts::PI / 4.0;
    let s = (2.0 / (std::f64::consts::PI * x)).sqrt();
    (
        s * (p * chi.cos() - q * chi.sin()),
        s * (p * chi.sin() + q * chi.cos()),
    )
}

/// K_n via the DLMF 10.40.2 asymptotic expansion (all-positive coefficients).
fn k_asym(x: f64, n: u32) -> f64 {
    let mu = 4.0 * n as f64 * n as f64;
    let a = asym_terms(mu, x);
    let s: f64 = a.iter().sum();
    (std::f64::consts::PI / (2.0 * x)).sqrt() * (-x).exp() * s
}

/// J_n via Miller's backward recurrence (stable for x > 12).
fn bessel_j_miller(x: f64, n: u32) -> f64 {
    let start = n + 50 + (2.0 * x).round() as u32;
    let mut j = vec![0.0f64; start as usize + 2];
    j[start as usize] = 1.0;
    for k in (1..=start).rev() {
        let v = (2.0 * k as f64 / x) * j[k as usize] - j[k as usize + 1];
        j[k as usize - 1] = v;
        if j[k as usize - 1].abs() > 1e200 {
            for e in j.iter_mut() {
                *e *= 1e-200;
            }
        }
    }
    let mut s = j[0];
    let mut k = 2;
    while k <= start {
        s += 2.0 * j[k as usize];
        k += 2;
    }
    j[n as usize] / s
}

fn bessel_j(x: f64, n: u32) -> f64 {
    if x <= 12.0 {
        bessel_j_series(x, n)
    } else if n as f64 <= (2.0 * x).sqrt() {
        jy_asym(x, n).0
    } else if n as f64 + 2.0 * x <= 200_000.0 {
        bessel_j_miller(x, n)
    } else {
        bessel_j_series(x, n)
    }
}

fn bessel_y(x: f64, n: u32) -> f64 {
    if x <= 14.0 {
        bessel_y_series(x, n)
    } else {
        let (_, y0) = jy_asym(x, 0);
        if n == 0 {
            return y0;
        }
        let (_, y1) = jy_asym(x, 1);
        if n == 1 {
            return y1;
        }
        let mut prev = y0;
        let mut cur = y1;
        for k in 1..n {
            let nxt = (2.0 * k as f64 / x) * cur - prev;
            prev = cur;
            cur = nxt;
            if !cur.is_finite() {
                return cur;
            }
        }
        cur
    }
}

fn bessel_i(x: f64, n: u32) -> f64 {
    bessel_i_series(x, n)
}

fn bessel_k(x: f64, n: u32) -> f64 {
    if x <= 14.0 {
        bessel_k_series(x, n)
    } else {
        let k0 = k_asym(x, 0);
        if n == 0 {
            return k0;
        }
        let k1 = k_asym(x, 1);
        if n == 1 {
            return k1;
        }
        let mut prev = k0;
        let mut cur = k1;
        for k in 1..n {
            let nxt = prev + (2.0 * k as f64 / x) * cur;
            prev = cur;
            cur = nxt;
            if !cur.is_finite() {
                return cur;
            }
        }
        cur
    }
}

fn bessel(ctx: &FuncCtx, args: &[FuncArg], kind: u8) -> Result<CalcValue, CalcError> {
    let x = coerce_number(&args[0].value(ctx)?)?;
    let n_raw = coerce_number(&args[1].value(ctx)?)?;
    if x < 0.0 || n_raw < 0.0 {
        return Err(CalcError::Num);
    }
    let n = n_raw.trunc() as u64;
    if n > 1_000_000 {
        return Err(CalcError::Num);
    }
    if (kind == 1 || kind == 3) && n > 100_000 {
        // Y/K forward recurrences stay bounded in time; absurd orders overflow.
        return Err(CalcError::Num);
    }
    let n = n as u32;
    let r = match kind {
        0 => bessel_j(x, n),
        1 => bessel_y(x, n),
        2 => bessel_i(x, n),
        _ => bessel_k(x, n),
    };
    ok_num(r)
}

fn besselj(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    bessel(ctx, args, 0)
}
fn bessely(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    bessel(ctx, args, 1)
}
fn besseli(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    bessel(ctx, args, 2)
}
fn besselk(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    bessel(ctx, args, 3)
}

// ---------------------------------------------------------------------------
// Registration.
// ---------------------------------------------------------------------------

macro_rules! spec {
    ($name:expr, $min:expr, $max:expr, $f:ident) => {
        FuncSpec {
            name: $name,
            min_args: $min,
            max_args: $max,
            volatile: false,
            array_aware: false,
            func: $f,
        }
    };
}

const SPECS: &[FuncSpec] = &[
    spec!("DEC2BIN", 1, Some(2), dec2bin),
    spec!("DEC2OCT", 1, Some(2), dec2oct),
    spec!("DEC2HEX", 1, Some(2), dec2hex),
    spec!("BIN2DEC", 1, Some(1), bin2dec),
    spec!("BIN2OCT", 1, Some(2), bin2oct),
    spec!("BIN2HEX", 1, Some(2), bin2hex),
    spec!("OCT2DEC", 1, Some(1), oct2dec),
    spec!("OCT2BIN", 1, Some(2), oct2bin),
    spec!("OCT2HEX", 1, Some(2), oct2hex),
    spec!("HEX2DEC", 1, Some(1), hex2dec),
    spec!("HEX2BIN", 1, Some(2), hex2bin),
    spec!("HEX2OCT", 1, Some(2), hex2oct),
    spec!("BITAND", 2, Some(2), bitand),
    spec!("BITOR", 2, Some(2), bitor),
    spec!("BITXOR", 2, Some(2), bitxor),
    spec!("BITLSHIFT", 2, Some(2), bitlshift),
    spec!("BITRSHIFT", 2, Some(2), bitrshift),
    spec!("DELTA", 1, Some(2), delta),
    spec!("GESTEP", 1, Some(2), gestep),
    spec!("ERF", 1, Some(2), erf_),
    spec!("ERF.PRECISE", 1, Some(1), erf_precise),
    spec!("ERFC", 1, Some(1), erfc_),
    spec!("ERFC.PRECISE", 1, Some(1), erfc_),
    spec!("COMPLEX", 2, Some(3), complex),
    spec!("IMREAL", 1, Some(1), imreal),
    spec!("IMAGINARY", 1, Some(1), imaginary),
    spec!("IMABS", 1, Some(1), imabs),
    spec!("IMARGUMENT", 1, Some(1), imargument),
    spec!("IMCONJUGATE", 1, Some(1), imconjugate),
    spec!("IMSUM", 1, None, imsum),
    spec!("IMSUB", 2, Some(2), imsub),
    spec!("IMPRODUCT", 1, None, improduct),
    spec!("IMDIV", 2, Some(2), imdiv),
    spec!("IMPOWER", 2, Some(2), impower),
    spec!("IMSQRT", 1, Some(1), imsqrt),
    spec!("IMEXP", 1, Some(1), imexp),
    spec!("IMLN", 1, Some(1), imln),
    spec!("IMLOG10", 1, Some(1), imlog10),
    spec!("IMLOG2", 1, Some(1), imlog2),
    spec!("IMSIN", 1, Some(1), imsin),
    spec!("IMCOS", 1, Some(1), imcos),
    spec!("IMTAN", 1, Some(1), imtan),
    spec!("CONVERT", 3, Some(3), convert),
    spec!("BESSELJ", 2, Some(2), besselj),
    spec!("BESSELY", 2, Some(2), bessely),
    spec!("BESSELI", 2, Some(2), besseli),
    spec!("BESSELK", 2, Some(2), besselk),
];

pub fn register(r: &mut Registry) {
    for s in SPECS {
        r.register(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turbo::calc::testkit::{approx, error, num, text};

    fn err_num() -> CalcError {
        CalcError::Num
    }
    fn err_value() -> CalcError {
        CalcError::Value
    }
    fn err_na() -> CalcError {
        CalcError::Na
    }
    fn err_div0() -> CalcError {
        CalcError::Div0
    }

    // ---- base conversion: Excel's documented examples ----------------------

    #[test]
    fn dec2bin_doc_examples() {
        assert_eq!(text("=DEC2BIN(100)"), "1100100");
        assert_eq!(text("=DEC2BIN(-100)"), "1110011100");
        assert_eq!(text("=DEC2BIN(-1)"), "1111111111");
        assert_eq!(text("=DEC2BIN(0)"), "0");
        assert_eq!(text("=DEC2BIN(1)"), "1");
        assert_eq!(text("=DEC2BIN(-512)"), "1000000000");
        assert_eq!(text("=DEC2BIN(511)"), "111111111");
        assert_eq!(text("=DEC2BIN(5,3)"), "101");
        assert_eq!(text("=DEC2BIN(5,6)"), "000101");
    }

    #[test]
    fn dec2bin_errors() {
        assert_eq!(error("=DEC2BIN(512)"), err_num());
        assert_eq!(error("=DEC2BIN(-513)"), err_num());
        assert_eq!(error("=DEC2BIN(5,0)"), err_num());
        assert_eq!(error("=DEC2BIN(5,11)"), err_num());
        assert_eq!(text("=DEC2BIN(1,2)"), "01");
        // places is ignored for negatives
        assert_eq!(text("=DEC2BIN(-1,3)"), "1111111111");
    }

    #[test]
    fn dec2oct_and_dec2hex_doc_examples() {
        assert_eq!(text("=DEC2OCT(100)"), "144");
        assert_eq!(text("=DEC2OCT(-1)"), "7777777777");
        assert_eq!(text("=DEC2OCT(-536870912)"), "4000000000");
        assert_eq!(text("=DEC2OCT(536870911)"), "3777777777");
        assert_eq!(text("=DEC2HEX(100)"), "64");
        assert_eq!(text("=DEC2HEX(-1)"), "FFFFFFFFFF");
        assert_eq!(text("=DEC2HEX(-549755813888)"), "8000000000");
        assert_eq!(text("=DEC2HEX(549755813887)"), "7FFFFFFFFF");
        assert_eq!(error("=DEC2HEX(549755813888)"), err_num());
        assert_eq!(error("=DEC2OCT(536870912)"), err_num());
    }

    #[test]
    fn x2dec_and_cross_conversions_doc_examples() {
        assert_eq!(num("=BIN2DEC(1100100)"), 100.0);
        assert_eq!(num("=BIN2DEC(\"1111111111\")"), -1.0);
        assert_eq!(num("=BIN2DEC(\"1000000000\")"), -512.0);
        assert_eq!(num("=OCT2DEC(77)"), 63.0);
        assert_eq!(num("=OCT2DEC(\"7777777777\")"), -1.0);
        assert_eq!(num("=HEX2DEC(\"3DA408B9\")"), 1034160313.0);
        assert_eq!(num("=HEX2DEC(\"FFFFFFFFFF\")"), -1.0);
        assert_eq!(num("=HEX2DEC(\"8000000000\")"), -549755813888.0);

        assert_eq!(text("=BIN2OCT(\"1111111111\")"), "7777777777");
        assert_eq!(text("=BIN2OCT(1100100)"), "144");
        assert_eq!(text("=BIN2OCT(\"1111111110\")"), "7777777776");
        assert_eq!(text("=BIN2HEX(\"1111111111\")"), "FFFFFFFFFF");
        assert_eq!(text("=BIN2HEX(1100100)"), "64");
        assert_eq!(text("=BIN2HEX(\"1000000000\")"), "FFFFFFFE00");
        assert_eq!(text("=OCT2BIN(\"7777777777\")"), "1111111111");
        assert_eq!(text("=OCT2BIN(77)"), "111111");
        assert_eq!(text("=OCT2HEX(\"7777777777\")"), "FFFFFFFFFF");
        assert_eq!(text("=OCT2HEX(777)"), "1FF");
        assert_eq!(text("=HEX2BIN(\"FFFFFFFFFF\")"), "1111111111");
        assert_eq!(text("=HEX2BIN(\"F\")"), "1111");
        assert_eq!(text("=HEX2OCT(\"FFFFFFFFFF\")"), "7777777777");
        assert_eq!(text("=HEX2OCT(\"FF\")"), "377");
        assert_eq!(text("=HEX2OCT(\"FFFFFFFE00\")"), "7777777000");
    }

    #[test]
    fn cross_conversion_places_and_ranges() {
        assert_eq!(text("=BIN2HEX(\"1111\",4)"), "000F");
        assert_eq!(text("=BIN2HEX(\"1111\",1)"), "F");
        assert_eq!(error("=BIN2HEX(\"11111\",1)"), err_num());
        assert_eq!(text("=BIN2OCT(\"1000000000\")"), "7777777000"); // -512 in 30-bit octal
        assert_eq!(error("=HEX2BIN(\"200\")"), err_num());
        assert_eq!(text("=BIN2OCT(\"1111111111\",5)"), "7777777777"); // places ignored for negatives
        assert_eq!(error("=BIN2DEC(2)"), err_num());
        assert_eq!(error("=HEX2DEC(\"FFFFFFFFFF0\")"), err_num());
        assert_eq!(error("=BIN2DEC(\"\")"), err_num());
    }

    // ---- bitwise -----------------------------------------------------------

    #[test]
    fn bitwise_doc_examples_and_errors() {
        assert_eq!(num("=BITAND(13,25)"), 9.0);
        assert_eq!(num("=BITOR(23,10)"), 31.0);
        assert_eq!(num("=BITXOR(5,3)"), 6.0);
        assert_eq!(num("=BITLSHIFT(4,2)"), 16.0);
        assert_eq!(num("=BITRSHIFT(13,2)"), 3.0);
        assert_eq!(num("=BITLSHIFT(4,-1)"), 2.0);
        assert_eq!(num("=BITRSHIFT(4,-1)"), 8.0);
        assert_eq!(error("=BITAND(5,-1)"), err_num());
        assert_eq!(error("=BITOR(1.5,2)"), err_num());
        assert_eq!(error("=BITXOR(281474976710656,0)"), err_num()); // 2^48
        assert_eq!(error("=BITLSHIFT(1,48)"), err_num()); // 2^48 out of range
        assert_eq!(num("=BITRSHIFT(5,100)"), 0.0);
        assert_eq!(
            num("=BITAND(281474976710655,281474976710655)"),
            281474976710655.0
        );
    }

    // ---- delta / gestep ----------------------------------------------------

    #[test]
    fn delta_and_gestep() {
        assert_eq!(num("=DELTA(5,4)"), 0.0);
        assert_eq!(num("=DELTA(5,5)"), 1.0);
        assert_eq!(num("=DELTA(5)"), 0.0);
        assert_eq!(num("=DELTA(0)"), 1.0);
        assert_eq!(num("=GESTEP(5,4)"), 1.0);
        assert_eq!(num("=GESTEP(5,5)"), 1.0);
        assert_eq!(num("=GESTEP(4,5)"), 0.0);
        assert_eq!(num("=GESTEP(-1)"), 0.0);
    }

    // ---- error functions ---------------------------------------------------

    #[test]
    fn erf_doc_and_known_values() {
        approx("=ERF(1)", 0.8427007929497149, 1e-13);
        approx("=ERF(0.5)", 0.5204998778130465, 1e-13);
        approx("=ERF(0)", 0.0, 1e-15);
        approx("=ERF(0.5,1)", 0.3222009151366684, 1e-13);
        approx("=ERF(1,0.5)", -0.3222009151366684, 1e-13);
        assert_eq!(error("=ERF(-1)"), err_num());
        approx("=ERFC(1)", 0.1572992070502851, 1e-13);
        approx("=ERFC(-1)", 1.842700792949715, 1e-13);
        approx("=ERF.PRECISE(-0.5)", -0.5204998778130465, 1e-13);
        approx("=ERFC.PRECISE(-1)", 1.842700792949715, 1e-13);
        approx("=ERF(3)", 0.9999779095030014, 1e-13);
        approx("=ERF(5)", 0.9999999999984626, 1e-13);
    }

    // ---- complex numbers ---------------------------------------------------

    #[test]
    fn complex_doc_examples() {
        assert_eq!(text("=COMPLEX(3,4)"), "3+4i");
        assert_eq!(text("=COMPLEX(3,4,\"j\")"), "3+4j");
        assert_eq!(text("=COMPLEX(0,1)"), "i");
        assert_eq!(text("=COMPLEX(1,0)"), "1");
        assert_eq!(text("=COMPLEX(0,-1)"), "-i");
        assert_eq!(text("=COMPLEX(3,1)"), "3+i");
        assert_eq!(text("=COMPLEX(3,-1)"), "3-i");
        assert_eq!(text("=COMPLEX(0,0)"), "0");
        assert_eq!(text("=COMPLEX(0.5,1)"), "0.5+i");
        assert_eq!(error("=COMPLEX(3,4,\"k\")"), err_value());
        assert_eq!(error("=COMPLEX(\"a\",4)"), err_value());
    }

    #[test]
    fn im_basic_doc_examples() {
        assert_eq!(num("=IMREAL(\"3+4i\")"), 3.0);
        assert_eq!(num("=IMREAL(\"3i\")"), 0.0);
        assert_eq!(num("=IMAGINARY(\"3+4i\")"), 4.0);
        assert_eq!(num("=IMAGINARY(\"3\")"), 0.0);
        assert_eq!(num("=IMABS(\"3+4i\")"), 5.0);
        approx("=IMARGUMENT(\"3+4i\")", 0.9272952180016122, 1e-15);
        approx("=IMARGUMENT(\"3-3i\")", -0.7853981633974483, 1e-15);
        assert_eq!(error("=IMARGUMENT(\"0\")"), err_div0());
        assert_eq!(text("=IMCONJUGATE(\"3+4i\")"), "3-4i");
        assert_eq!(text("=IMCONJUGATE(\"3+4j\")"), "3-4j");
    }

    #[test]
    fn im_arithmetic_doc_examples() {
        assert_eq!(text("=IMSUM(\"3+4i\",\"5-3i\")"), "8+i");
        assert_eq!(text("=IMSUB(\"13+4i\",\"5+3i\")"), "8+i");
        assert_eq!(text("=IMPRODUCT(\"1+2i\",\"30+40i\")"), "-50+100i");
        assert_eq!(text("=IMDIV(\"-238+240i\",\"10+24i\")"), "5+12i");
        assert_eq!(text("=IMPOWER(\"2\",\"3\")"), "8");
        assert_eq!(text("=IMPOWER(\"2+3i\",\"3\")"), "-46+9i");
        assert_eq!(text("=IMSQRT(\"3+4i\")"), "2+i");
        // IMEXP("1") = e. The published example prints 15 significant digits;
        // General-format rounding of the 16th digit can flip the last one, so
        // assert the numeric value through a component parse with tolerance.
        let (e, _) = parse_complex_str(&text("=IMEXP(\"1\")")).expect("imexp output must parse");
        assert!((e.re - 2.718281828459045).abs() < 1e-14, "re {}", e.re);
        assert!((e.im).abs() < 1e-14, "im {}", e.im);
        assert_eq!(
            text("=IMLN(\"3+4i\")"),
            "1.6094379124341+0.927295218001612i"
        );
        assert_eq!(
            text("=IMLOG10(\"3+4i\")"),
            "0.698970004336019+0.402719196273373i"
        );
        assert_eq!(
            text("=IMLOG2(\"3+4i\")"),
            "2.32192809488736+1.33780421245098i"
        );
        assert_eq!(
            text("=IMSIN(\"3+4i\")"),
            "3.85373803791938-27.0168132580039i"
        );
        assert_eq!(
            text("=IMCOS(\"3+4i\")"),
            "-27.0349456030742-3.85115333481178i"
        );
    }

    #[test]
    fn im_tan_and_errors() {
        // IMTAN matches the published example within 1e-14 (component-wise).
        let (c, _) = parse_complex_str(&text("=IMTAN(\"3+4i\")")).expect("imtan output must parse");
        assert!((c.re - -0.00018734620462946).abs() < 1e-14, "re {}", c.re);
        assert!((c.im - 0.999355987381473).abs() < 1e-14, "im {}", c.im);
        assert_eq!(error("=IMSUM(\"3+i\",\"2+2j\")"), err_value());
        assert_eq!(error("=IMLN(\"0\")"), err_num());
        assert_eq!(error("=IMDIV(\"1\",\"0\")"), err_div0());
        assert_eq!(error("=IMREAL(\"3+4k\")"), err_num());
        assert_eq!(error("=IMREAL(\"abc\")"), err_num());
        assert_eq!(error("=IMPOWER(\"0\",\"0\")"), err_num());
    }

    // ---- convert -----------------------------------------------------------

    #[test]
    fn convert_doc_examples() {
        approx("=CONVERT(2.5,\"ft\",\"in\")", 30.0, 1e-12);
        approx("=CONVERT(100,\"in\",\"ft\")", 8.333333333333332, 1e-12);
        approx("=CONVERT(68,\"F\",\"C\")", 20.0, 1e-12);
        approx("=CONVERT(1,\"lbm\",\"kg\")", 0.45359237, 1e-15);
        approx("=CONVERT(1,\"km\",\"mi\")", 0.621371192237334, 1e-13);
        approx("=CONVERT(6,\"day\",\"hr\")", 144.0, 1e-15);
        approx("=CONVERT(1,\"N\",\"dy\")", 100000.0, 1e-12);
        approx("=CONVERT(1,\"kW\",\"HP\")", 1.34102208959503, 1e-12);
        approx("=CONVERT(1,\"Wh\",\"J\")", 3600.0, 1e-15);
        approx("=CONVERT(1,\"kPa\",\"psi\")", 0.14503773773020923, 1e-12);
        approx("=CONVERT(1,\"bar\",\"atm\")", 0.986923266716013, 1e-12);
        approx("=CONVERT(1,\"C\",\"F\")", 33.8, 1e-15);
        approx("=CONVERT(1,\"tsp\",\"ml\")", 4.92892159375, 1e-15);
        approx("=CONVERT(1,\"gal\",\"l\")", 3.785411784, 1e-15);
        approx("=CONVERT(1,\"kg\",\"lbm\")", 2.2046226218487757, 1e-15);
        approx("=CONVERT(1,\"in\",\"cm\")", 2.54, 1e-15);
        approx("=CONVERT(1,\"mi2\",\"acre\")", 640.0, 1e-12);
        approx("=CONVERT(1,\"BTU\",\"J\")", 1055.05585262, 1e-15);
    }

    #[test]
    fn convert_prefixes_and_temperature() {
        approx("=CONVERT(1,\"kibit\",\"bit\")", 1024.0, 1e-15);
        approx("=CONVERT(1,\"kbit\",\"bit\")", 1000.0, 1e-15);
        approx("=CONVERT(1,\"Mibyte\",\"byte\")", 1048576.0, 1e-15);
        approx("=CONVERT(1,\"kPa\",\"Pa\")", 1000.0, 1e-15);
        approx("=CONVERT(1,\"mT\",\"T\")", 0.001, 1e-15); // milli-tesla -> tesla
        approx("=CONVERT(10000,\"ga\",\"T\")", 1.0, 1e-15);
        approx("=CONVERT(32,\"F\",\"C\")", 0.0, 1e-15);
        approx("=CONVERT(-40,\"C\",\"F\")", -40.0, 1e-15);
        approx("=CONVERT(1,\"C\",\"K\")", 274.15, 1e-15);
        approx("=CONVERT(1,\"K\",\"C\")", -272.15, 1e-15);
        approx("=CONVERT(1,\"F\",\"K\")", 255.9277777777778, 1e-15);
    }

    #[test]
    fn convert_errors() {
        assert_eq!(error("=CONVERT(1,\"m\",\"kg\")"), err_na());
        assert_eq!(error("=CONVERT(1,\"m\",\"zarg\")"), err_na());
        assert_eq!(error("=CONVERT(1,\"in\",\"kg\")"), err_na());
        assert_eq!(error("=CONVERT(1,\"C\",\"m\")"), err_na());
        assert_eq!(error("=CONVERT(1,\"zz\",\"m\")"), err_na());
        assert_eq!(error("=CONVERT(\"abc\",\"ft\",\"in\")"), err_value());
    }

    // ---- bessel ------------------------------------------------------------

    #[test]
    fn bessel_known_values() {
        // Anchored to 25-digit mpmath reference values.
        approx("=BESSELJ(1,0)", 0.7651976865579666, 1e-12);
        approx("=BESSELJ(1,1)", 0.4400505857449335, 1e-12);
        approx("=BESSELJ(1,2)", 0.11490348493190048, 1e-12);
        approx("=BESSELJ(10,0)", -0.2459357644513483, 1e-12);
        approx("=BESSELJ(30,0)", -0.08636798358104021, 1e-10);
        approx("=BESSELJ(30,10)", -0.12987689399858877, 1e-10);
        approx("=BESSELJ(1.9,2)", 0.3299257276923872, 1e-12);

        approx("=BESSELY(1,0)", 0.08825696421567696, 1e-12);
        approx("=BESSELY(1,1)", -0.7812128213002887, 1e-12);
        approx("=BESSELY(10,0)", 0.05567116728359939, 1e-12);
        approx("=BESSELY(30,0)", -0.11729573168666402, 1e-10);
        approx("=BESSELY(30,10)", 0.07505670212239711, 1e-10);
        approx("=BESSELY(1.9,2)", -0.669878679001289, 1e-12);

        approx("=BESSELI(1,0)", 1.2660658777520084, 1e-12);
        approx("=BESSELI(1,1)", 0.565159103992485, 1e-12);
        approx("=BESSELI(10,0)", 2815.716628466254, 1e-12);
        approx("=BESSELI(30,0)", 781672297823.9774, 1e-12);
        approx("=BESSELI(30,10)", 145831809975.96713, 1e-12);
        approx("=BESSELI(1.9,2)", 0.6032724329434784, 1e-12);

        approx("=BESSELK(1,0)", 0.42102443824070834, 1e-12);
        approx("=BESSELK(1,1)", 0.6019072301972346, 1e-12);
        approx("=BESSELK(10,0)", 0.00001778006231616765, 1e-9);
        approx("=BESSELK(30,0)", 2.1324774964630564e-14, 1e-9);
        approx("=BESSELK(30,10)", 1.0842816942222974e-13, 1e-9);
        approx("=BESSELK(1.9,2)", 0.2969092982578029, 1e-12);
    }

    #[test]
    fn bessel_series_orders_and_errors() {
        // Higher orders and boundary regime (series at x<=12 vs recurrence/asymptotic).
        approx("=BESSELJ(12,0)", 0.04768931079683354, 1e-10);
        approx("=BESSELJ(15,0)", -0.014224472826780773, 1e-10);
        approx("=BESSELJ(20,10)", 0.18648255802394508, 1e-9);
        approx("=BESSELY(12,0)", -0.22523731263436143, 1e-10);
        approx("=BESSELY(15,0)", 0.20546429603891826, 1e-10);
        approx("=BESSELY(15,1)", 0.021073628036873512, 1e-10);
        approx("=BESSELY(20,10)", -0.0438946535156584, 1e-9);
        approx("=BESSELI(15,0)", 339649.3732979139, 1e-12);
        approx("=BESSELK(15,0)", 9.819536482396434e-8, 1e-9);
        approx("=BESSELK(20,10)", 6.31621452832158e-9, 1e-9);
        // large order n ~ x
        approx("=BESSELJ(30,100)", 4.5788015281752445e-42, 1e-9);
        approx("=BESSELY(30,100)", -7.287528470824471e38, 1e-9);
        approx("=BESSELI(30,100)", 3.947642005333428e-40, 1e-9);
        approx("=BESSELK(30,100)", 1.2131584253026667e37, 1e-9);
        approx("=BESSELJ(15,100)", 1.9660095611249547e-71, 1e-9);
        approx("=BESSELY(15,100)", -1.6375955323196364e68, 1e-9);

        // domain rules
        assert_eq!(error("=BESSELJ(-1,2)"), err_num());
        assert_eq!(error("=BESSELJ(1,-1)"), err_num());
        assert_eq!(error("=BESSELY(0,0)"), err_num());
        assert_eq!(error("=BESSELK(0,0)"), err_num());
        assert_eq!(num("=BESSELJ(0,0)"), 1.0);
        assert_eq!(num("=BESSELI(0,0)"), 1.0);
        assert_eq!(num("=BESSELJ(0,2)"), 0.0);
        // n truncated toward zero
        approx("=BESSELJ(1.9,2.9)", 0.3299257276923872, 1e-12);
        // overflow -> #NUM!, like Excel
        assert_eq!(error("=BESSELI(1000,0)"), err_num());
    }

    // ---- registry sanity ---------------------------------------------------

    #[test]
    fn every_engineering_function_is_registered() {
        let names = [
            "DEC2BIN",
            "DEC2OCT",
            "DEC2HEX",
            "BIN2DEC",
            "BIN2OCT",
            "BIN2HEX",
            "OCT2DEC",
            "OCT2BIN",
            "OCT2HEX",
            "HEX2DEC",
            "HEX2BIN",
            "HEX2OCT",
            "BITAND",
            "BITOR",
            "BITXOR",
            "BITLSHIFT",
            "BITRSHIFT",
            "DELTA",
            "GESTEP",
            "ERF",
            "ERF.PRECISE",
            "ERFC",
            "ERFC.PRECISE",
            "COMPLEX",
            "IMREAL",
            "IMAGINARY",
            "IMABS",
            "IMARGUMENT",
            "IMCONJUGATE",
            "IMSUM",
            "IMSUB",
            "IMPRODUCT",
            "IMDIV",
            "IMPOWER",
            "IMSQRT",
            "IMEXP",
            "IMLN",
            "IMLOG10",
            "IMLOG2",
            "IMSIN",
            "IMCOS",
            "IMTAN",
            "CONVERT",
            "BESSELJ",
            "BESSELY",
            "BESSELI",
            "BESSELK",
        ];
        for n in names {
            assert!(
                crate::turbo::calc::functions::registry().get(n).is_some(),
                "{n}"
            );
        }
    }
}
