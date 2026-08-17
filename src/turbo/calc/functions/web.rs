// functions/web.rs — the web function family. Owned exclusively by the web
// family agent; no other agent edits this file. `functions/mod.rs` declares
// `mod web;` and calls `web::register(&mut r)` from `build()`.
//
// Registry contract: implement `register` below and keep this exact signature.
// See functions/mod.rs for the worked ABS template.
//
// The family is deliberately small: ENCODEURL is the only Excel-standard web
// function in the round-2 contract. WEBSERVICE would need a network hop at
// calculation time, which a deterministic library must not do.

use super::{FuncArg, FuncCtx, FuncSpec, Registry};
use crate::turbo::calc::coerce::coerce_text;
use crate::turbo::calc::value::{CalcError, CalcValue};

/// Uppercase hex digits for percent-encoding.
const HEX: &[u8; 16] = b"0123456789ABCDEF";

/// `ENCODEURL(text)`: RFC-3986 percent-encoding. Every byte outside the
/// unreserved set (`A-Z a-z 0-9 - . _ ~`) is emitted as `%XX`; non-ASCII text
/// is encoded UTF-8 byte by byte, so `"你好"` becomes `%E4%BD%A0%E5%A5%BD`.
fn encodeurl(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let text = coerce_text(&args[0].value(ctx)?)?;
    let mut out = String::with_capacity(text.len() * 3);
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0F) as usize] as char);
        }
    }
    Ok(CalcValue::text(out))
}

const ENCODEURL: FuncSpec = FuncSpec {
    name: "ENCODEURL",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: encodeurl,
};

pub fn register(r: &mut Registry) {
    r.register(&ENCODEURL);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turbo::calc::testkit::{Grid, Outcome};

    #[test]
    fn unreserved_characters_pass_through_untouched() {
        let g = Grid::empty();
        assert_eq!(g.text("=ENCODEURL(\"abcXYZ0129-._~\")"), "abcXYZ0129-._~");
        assert_eq!(g.text("=ENCODEURL(\"\")"), "");
    }

    #[test]
    fn reserved_and_space_bytes_are_percent_encoded() {
        let g = Grid::empty();
        assert_eq!(g.text("=ENCODEURL(\"a b\")"), "a%20b");
        assert_eq!(g.text("=ENCODEURL(\"?q=1&x=2\")"), "%3Fq%3D1%26x%3D2");
        assert_eq!(
            g.text("=ENCODEURL(\"https://x.com/a:b\")"),
            "https%3A%2F%2Fx.com%2Fa%3Ab"
        );
    }

    #[test]
    fn non_ascii_utf8_bytes_are_encoded() {
        let g = Grid::empty();
        assert_eq!(g.text("=ENCODEURL(\"你好\")"), "%E4%BD%A0%E5%A5%BD");
        assert_eq!(g.text("=ENCODEURL(\"😊\")"), "%F0%9F%98%8A");
    }

    #[test]
    fn numbers_booleans_and_blank_coerce_to_text() {
        let g = Grid::empty();
        assert_eq!(g.text("=ENCODEURL(123)"), "123");
        assert_eq!(g.text("=ENCODEURL(TRUE)"), "TRUE");
        assert_eq!(g.text("=ENCODEURL(A1)"), "");
    }

    #[test]
    fn arity() {
        let g = Grid::empty();
        assert_eq!(g.error("=ENCODEURL()"), CalcError::Value);
        assert_eq!(g.error("=ENCODEURL(\"a\",\"b\")"), CalcError::Value);
    }

    #[test]
    fn registered_in_the_live_registry() {
        let spec = crate::turbo::calc::functions::registry()
            .get("encodeurl")
            .expect("ENCODEURL must be registered");
        assert_eq!(spec.name, "ENCODEURL");
        assert!(spec.validate(1).is_ok());
        assert_eq!(spec.validate(0), Err(CalcError::Value));
    }
}
