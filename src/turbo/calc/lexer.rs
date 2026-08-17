// calc/lexer.rs — the Excel formula tokenizer.
//
// Turns a formula body (no leading '=') into a flat token stream, following
// `01_parser_reference.md` §2. Every token carries a byte span into the input;
// text is read back with `Token::text` / `Token::string_value`, so
// tokenization never allocates and never borrows the input.
//
// Semantics locked by the plan:
// - Numbers include scientific notation and a leading dot (`.5`, `.5e2`).
// - Whitespace is significant only as the range-intersection operator: an
//   `Intersect` token is emitted only between an operand-class token and a
//   token that starts an operand (never next to `(` `)` `,` `;` `:` an
//   operator, or EOF). So `SUM (A1)` lexes as `SUM` immediately followed by
//   `(` — the function-name shape — while `A1:A5 B1:B5` keeps its `Intersect`.
// - A `ReferenceLike` token is raw reference text (cell/range/sheet-prefixed/
//   table/bracket/name). Classification is the parser's job (spec §4), so the
//   lexer never splits a quoted sheet name, a `[Book]`/`Table[...]` bracket
//   run, or the sheet parts of a reference.
//
// Lexing errors return `Result`; the tokenizer never panics.

use crate::turbo::calc::value::CalcError;
use std::ops::Range;

/// One token in a formula's token stream.
#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    /// Byte span into the input formula body.
    pub span: Range<usize>,
}

/// The lexeme classes produced by [`tokenize`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TokenKind {
    /// Integer / decimal / scientific literal (including `.5`). The value is
    /// parsed from [`Token::text`] by the parser.
    Number,
    /// Double-quoted string literal; `""` is an escaped quote.
    String,
    /// `TRUE` / `FALSE` (case-insensitive).
    Bool(bool),
    /// An error literal, e.g. `#DIV/0!` (case-insensitive), matched through
    /// [`CalcError::from_str_ci`].
    Error(CalcError),
    /// Raw reference-ish text: cell/range refs, `'Sheet'!` and `[Book]`
    /// prefixes, structured-table brackets, defined names. The parser
    /// classifies it (spec §4).
    ReferenceLike,
    /// `@` implicit-intersection prefix.
    At,
    Plus,
    Minus,
    Mul,
    Div,
    Pow,
    Concat,
    Percent,
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    LParen,
    RParen,
    LBrace,
    RBrace,
    Comma,
    SemiColon,
    Colon,
    /// A significant space — the range-intersection operator.
    Intersect,
}

/// A tokenizer failure. `code` tells the caller how to route the cell; the
/// caller falls back rather than ever guessing at the formula's value.
#[derive(Clone, Debug, PartialEq)]
pub struct LexError {
    pub span: Range<usize>,
    pub code: CalcError,
}

impl Token {
    /// The input slice covered by this token.
    pub fn text<'a>(&self, src: &'a str) -> &'a str {
        &src[self.span.clone()]
    }

    /// The decoded content of a `String` token (`""` unescaped to `"`). Only
    /// meaningful for [`TokenKind::String`].
    pub fn string_value(&self, src: &str) -> String {
        let raw = self.text(src);
        let inner = &raw[1..raw.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                }
                out.push('"');
            } else {
                out.push(c);
            }
        }
        out
    }

    /// True when the token at `i` is a reference-like name immediately followed
    /// by `(`, i.e. the function-call shape. Whitespace between the name and
    /// `(` is dropped by the lexer (never an `Intersect`), so this is exact.
    pub fn is_function_head(tokens: &[Token], i: usize) -> bool {
        matches!(
            tokens.get(i).map(|t| &t.kind),
            Some(TokenKind::ReferenceLike)
        ) && matches!(tokens.get(i + 1).map(|t| &t.kind), Some(TokenKind::LParen))
    }
}

/// Tokenize a formula body. A single leading `=` is skipped (the caller
/// usually strips it); `=` anywhere else is the equality operator. Returns an
/// error for unclosed strings/sheet-name quotes/bracket runs and unbalanced
/// `(`/`{` — never panics.
pub fn tokenize(src: &str) -> Result<Vec<Token>, LexError> {
    let f = src.as_bytes();
    let n = f.len();
    let mut tokens = Vec::with_capacity(n / 2);
    let mut paren_depth = 0i32;
    let mut brace_depth = 0i32;
    let mut last_open = n;

    let mut i = 0;
    if i < n && f[i] == b'=' {
        i += 1;
    }

    while i < n {
        let c = f[i];
        match c {
            b'0'..=b'9' => {
                let start = i;
                i += scan_number(f, i, n);
                tokens.push(Token {
                    kind: TokenKind::Number,
                    span: start..i,
                });
            }
            b'.' => {
                if i + 1 < n && f[i + 1].is_ascii_digit() {
                    let start = i;
                    i += scan_number(f, i, n);
                    tokens.push(Token {
                        kind: TokenKind::Number,
                        span: start..i,
                    });
                } else {
                    i = push_run(&mut tokens, src, i)?;
                }
            }
            b'#' => {
                if let Some(e) = match_error(src, i) {
                    let after = i + e.code().len();
                    let name_char =
                        after < n && !is_reserved(f[after]) && !f[after].is_ascii_whitespace();
                    if !name_char {
                        tokens.push(Token {
                            kind: TokenKind::Error(e),
                            span: i..after,
                        });
                        i = after;
                        continue;
                    }
                    // The code is glued to trailing reference text (e.g.
                    // `#REF!A1`, `#N/A!`), which Excel reads as an unknown
                    // name (#NAME?), not an error literal. Fold the code and
                    // its continuation into one ReferenceLike token; the run
                    // scanner cannot do this because `/` is reserved and would
                    // fragment `#DIV/0!x`.
                    let mut end = after;
                    while end < n && !is_reserved(f[end]) && !f[end].is_ascii_whitespace() {
                        end += 1;
                    }
                    tokens.push(Token {
                        kind: TokenKind::ReferenceLike,
                        span: i..end,
                    });
                    i = end;
                    continue;
                }
                i = push_run(&mut tokens, src, i)?;
            }
            b'"' => {
                let start = i;
                let mut j = i + 1;
                loop {
                    if j >= n {
                        return Err(LexError {
                            span: start..n,
                            code: CalcError::Value,
                        });
                    }
                    if f[j] == b'"' {
                        if j + 1 < n && f[j + 1] == b'"' {
                            j += 2;
                            continue;
                        }
                        break;
                    }
                    j += 1;
                }
                i = j + 1;
                tokens.push(Token {
                    kind: TokenKind::String,
                    span: start..i,
                });
            }
            b'+' => {
                tokens.push(Token {
                    kind: TokenKind::Plus,
                    span: i..i + 1,
                });
                i += 1;
            }
            b'-' => {
                tokens.push(Token {
                    kind: TokenKind::Minus,
                    span: i..i + 1,
                });
                i += 1;
            }
            b'*' => {
                tokens.push(Token {
                    kind: TokenKind::Mul,
                    span: i..i + 1,
                });
                i += 1;
            }
            b'/' => {
                tokens.push(Token {
                    kind: TokenKind::Div,
                    span: i..i + 1,
                });
                i += 1;
            }
            b'^' => {
                tokens.push(Token {
                    kind: TokenKind::Pow,
                    span: i..i + 1,
                });
                i += 1;
            }
            b'&' => {
                tokens.push(Token {
                    kind: TokenKind::Concat,
                    span: i..i + 1,
                });
                i += 1;
            }
            b'%' => {
                tokens.push(Token {
                    kind: TokenKind::Percent,
                    span: i..i + 1,
                });
                i += 1;
            }
            b'@' => {
                tokens.push(Token {
                    kind: TokenKind::At,
                    span: i..i + 1,
                });
                i += 1;
            }
            b'=' => {
                tokens.push(Token {
                    kind: TokenKind::Eq,
                    span: i..i + 1,
                });
                i += 1;
            }
            b'<' => {
                if i + 1 < n && f[i + 1] == b'=' {
                    tokens.push(Token {
                        kind: TokenKind::Le,
                        span: i..i + 2,
                    });
                    i += 2;
                } else if i + 1 < n && f[i + 1] == b'>' {
                    tokens.push(Token {
                        kind: TokenKind::Ne,
                        span: i..i + 2,
                    });
                    i += 2;
                } else {
                    tokens.push(Token {
                        kind: TokenKind::Lt,
                        span: i..i + 1,
                    });
                    i += 1;
                }
            }
            b'>' => {
                if i + 1 < n && f[i + 1] == b'=' {
                    tokens.push(Token {
                        kind: TokenKind::Ge,
                        span: i..i + 2,
                    });
                    i += 2;
                } else {
                    tokens.push(Token {
                        kind: TokenKind::Gt,
                        span: i..i + 1,
                    });
                    i += 1;
                }
            }
            b'(' => {
                paren_depth += 1;
                last_open = i;
                tokens.push(Token {
                    kind: TokenKind::LParen,
                    span: i..i + 1,
                });
                i += 1;
            }
            b')' => {
                paren_depth -= 1;
                if paren_depth < 0 {
                    return Err(LexError {
                        span: i..i + 1,
                        code: CalcError::Value,
                    });
                }
                tokens.push(Token {
                    kind: TokenKind::RParen,
                    span: i..i + 1,
                });
                i += 1;
            }
            b'{' => {
                brace_depth += 1;
                last_open = i;
                tokens.push(Token {
                    kind: TokenKind::LBrace,
                    span: i..i + 1,
                });
                i += 1;
            }
            b'}' => {
                brace_depth -= 1;
                if brace_depth < 0 {
                    return Err(LexError {
                        span: i..i + 1,
                        code: CalcError::Value,
                    });
                }
                tokens.push(Token {
                    kind: TokenKind::RBrace,
                    span: i..i + 1,
                });
                i += 1;
            }
            b',' => {
                tokens.push(Token {
                    kind: TokenKind::Comma,
                    span: i..i + 1,
                });
                i += 1;
            }
            b';' => {
                tokens.push(Token {
                    kind: TokenKind::SemiColon,
                    span: i..i + 1,
                });
                i += 1;
            }
            b':' => {
                tokens.push(Token {
                    kind: TokenKind::Colon,
                    span: i..i + 1,
                });
                i += 1;
            }
            c if c.is_ascii_whitespace() => {
                let start = i;
                while i < n && f[i].is_ascii_whitespace() {
                    i += 1;
                }
                let prev_operand = tokens
                    .last()
                    .map(|t| is_operand_token(&t.kind))
                    .unwrap_or(false);
                let next_operand = i < n && is_operand_start(f[i]);
                if prev_operand && next_operand {
                    tokens.push(Token {
                        kind: TokenKind::Intersect,
                        span: start..i,
                    });
                }
            }
            _ => {
                i = push_run(&mut tokens, src, i)?;
            }
        }
    }

    if paren_depth != 0 || brace_depth != 0 {
        return Err(LexError {
            span: last_open..n,
            code: CalcError::Value,
        });
    }
    Ok(tokens)
}

/// Scan a number starting at `i` (`[0-9]` or `.` immediately followed by a
/// digit). Grammar: integer digits, optional `.` + digits, optional
/// `[eE][+-]?digits` (the exponent is consumed only when digits follow, so
/// `1e` leaves the `e` for a run → `#NAME?`, matching Excel). Returns the byte
/// length consumed.
fn scan_number(f: &[u8], i: usize, n: usize) -> usize {
    let mut j = i;
    let mut dot_consumed = false;
    if f[j] == b'.' {
        dot_consumed = true;
        j += 1;
    }
    while j < n && f[j].is_ascii_digit() {
        j += 1;
    }
    if !dot_consumed && j < n && f[j] == b'.' {
        j += 1;
        while j < n && f[j].is_ascii_digit() {
            j += 1;
        }
    }
    if j < n && (f[j] == b'e' || f[j] == b'E') {
        let k = j + 1;
        let k2 = if k < n && (f[k] == b'+' || f[k] == b'-') {
            k + 1
        } else {
            k
        };
        if k2 < n && f[k2].is_ascii_digit() {
            j = k2;
            while j < n && f[j].is_ascii_digit() {
                j += 1;
            }
        }
    }
    j - i
}

/// Consume a reference-like run starting at `i`. `'...'` (doubled `''`) sheet
/// names and depth-counted `[...]` bracket runs are folded in verbatim, so a
/// quoted sheet name or `Table[[#Data],[col1]:[col2]]` is never split and its
/// internal spaces/commas/colons never become tokens. Returns the end index.
fn scan_run(f: &[u8], i: usize, n: usize) -> Result<usize, LexError> {
    let mut j = i;
    while j < n {
        let b = f[j];
        if b.is_ascii_whitespace() || is_reserved(b) {
            break;
        }
        match b {
            b'\'' => {
                let start = j;
                j += 1;
                loop {
                    if j >= n {
                        return Err(LexError {
                            span: start..n,
                            code: CalcError::Value,
                        });
                    }
                    if f[j] == b'\'' {
                        if j + 1 < n && f[j + 1] == b'\'' {
                            j += 2;
                            continue;
                        }
                        j += 1;
                        break;
                    }
                    j += 1;
                }
            }
            b'[' => {
                let start = j;
                let mut depth = 0;
                while j < n {
                    if f[j] == b'[' {
                        depth += 1;
                        j += 1;
                    } else if f[j] == b']' {
                        depth -= 1;
                        j += 1;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        j += 1;
                    }
                }
                if depth != 0 {
                    return Err(LexError {
                        span: start..n,
                        code: CalcError::Value,
                    });
                }
            }
            _ => j += 1,
        }
    }
    Ok(j)
}

/// Lex one run and push its token: [`TokenKind::ReferenceLike`], or
/// [`TokenKind::Bool`] for a bare `TRUE`/`FALSE` not immediately followed by
/// `(` (so `TRUE(...)` stays a function-call shape). Returns the end index.
fn push_run(tokens: &mut Vec<Token>, src: &str, i: usize) -> Result<usize, LexError> {
    let f = src.as_bytes();
    let n = f.len();
    let start = i;
    let end = scan_run(f, i, n)?;
    let text = &src[start..end];
    let is_true = text.eq_ignore_ascii_case("TRUE");
    let is_false = text.eq_ignore_ascii_case("FALSE");
    let followed_by_lparen = end < n && f[end] == b'(';
    let kind = if (is_true || is_false) && !followed_by_lparen {
        TokenKind::Bool(is_true)
    } else {
        TokenKind::ReferenceLike
    };
    tokens.push(Token {
        kind,
        span: start..end,
    });
    Ok(end)
}

/// Match an error literal at byte `i`, reusing [`CalcError::from_str_ci`] on
/// each candidate code slice. Returns the longest case-insensitive match, so
/// the full spec §6 set (incl. `#SPILL!` `#CALC!` `#CYCLE!` `#ERROR!`) works.
fn match_error(src: &str, i: usize) -> Option<CalcError> {
    const CODES: [&str; 12] = [
        "#NULL!",
        "#DIV/0!",
        "#VALUE!",
        "#REF!",
        "#NAME?",
        "#NUM!",
        "#N/A",
        "#SPILL!",
        "#CALC!",
        "#CYCLE!",
        "#ERROR!",
        "#GETTING_DATA",
    ];
    let rest = &src[i..];
    CODES
        .iter()
        .filter_map(|c| {
            rest.get(..c.len())
                .and_then(|p| CalcError::from_str_ci(p).map(|e| (c.len(), e)))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, e)| e)
}

/// Whether a token can be the left operand of a space intersection.
fn is_operand_token(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::ReferenceLike
            | TokenKind::Number
            | TokenKind::String
            | TokenKind::Bool(_)
            | TokenKind::Error(_)
            | TokenKind::RParen
            | TokenKind::RBrace
    )
}

/// The first byte of a token that can be an intersection operand.
fn is_operand_start(b: u8) -> bool {
    b.is_ascii_alphabetic()
        || b.is_ascii_digit()
        || b == b'$'
        || b == b'.'
        || b == b'\''
        || b == b'['
        || b == b'#'
        || b == b'"'
        || b == b'{'
}

/// Bytes that never appear inside a reference-like run. `'` and `[` are
/// handled specially by [`scan_run`]; `#` mid-run is a plain run character.
fn is_reserved(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')'
            | b'{'
            | b'}'
            | b','
            | b';'
            | b':'
            | b'+'
            | b'-'
            | b'*'
            | b'/'
            | b'^'
            | b'&'
            | b'='
            | b'<'
            | b'>'
            | b'%'
            | b'@'
            | b'"'
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn lex(src: &str) -> Vec<Token> {
        tokenize(src).unwrap()
    }

    fn kinds(src: &str) -> Vec<TokenKind> {
        tokenize(src).unwrap().into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn numbers() {
        assert_eq!(kinds("1"), vec![TokenKind::Number]);
        assert_eq!(kinds("1."), vec![TokenKind::Number]);
        assert_eq!(kinds("123.45"), vec![TokenKind::Number]);
        assert_eq!(kinds("007"), vec![TokenKind::Number]);
        assert_eq!(kinds("3e+1"), vec![TokenKind::Number]);
        assert_eq!(kinds("3e-1"), vec![TokenKind::Number]);
        assert_eq!(kinds("1E5"), vec![TokenKind::Number]);
        assert_eq!(kinds("1.5e-3"), vec![TokenKind::Number]);
        assert_eq!(kinds("10%"), vec![TokenKind::Number, TokenKind::Percent]);
        assert_eq!(
            kinds("10%%"),
            vec![TokenKind::Number, TokenKind::Percent, TokenKind::Percent]
        );
    }

    #[test]
    fn leading_dot_numbers() {
        assert_eq!(kinds(".5"), vec![TokenKind::Number]);
        assert_eq!(
            kinds(".5+1"),
            vec![TokenKind::Number, TokenKind::Plus, TokenKind::Number]
        );
        assert_eq!(kinds("-.5"), vec![TokenKind::Minus, TokenKind::Number]);
        assert_eq!(kinds("+.5e2"), vec![TokenKind::Plus, TokenKind::Number]);
        assert_eq!(
            kinds("2-.5"),
            vec![TokenKind::Number, TokenKind::Minus, TokenKind::Number]
        );
        let t = lex(".5e2");
        assert_eq!(t[0].text(".5e2"), ".5e2");
        assert_eq!(t[0].text(".5e2").parse::<f64>(), Ok(50.0));
        // a dot that does not start a number stays part of a reference run
        assert_eq!(kinds("A1.5"), vec![TokenKind::ReferenceLike]);
    }

    #[test]
    fn strings_with_escaped_quotes() {
        assert_eq!(kinds("\"hi\""), vec![TokenKind::String]);
        assert_eq!(kinds("\"a\"\"b\""), vec![TokenKind::String]);
        assert_eq!(kinds("\"\""), vec![TokenKind::String]);
        assert_eq!(
            kinds("A1&\"x\"&B1"),
            vec![
                TokenKind::ReferenceLike,
                TokenKind::Concat,
                TokenKind::String,
                TokenKind::Concat,
                TokenKind::ReferenceLike
            ]
        );
        let t = lex("\"a\"\"b\"");
        assert_eq!(t[0].kind, TokenKind::String);
        assert_eq!(t[0].string_value("\"a\"\"b\""), "a\"b");
        let t = lex("\"hi\"");
        assert_eq!(t[0].string_value("\"hi\""), "hi");
        assert_eq!(t[0].span, 0..4);
    }

    #[test]
    fn booleans() {
        assert_eq!(kinds("TRUE"), vec![TokenKind::Bool(true)]);
        assert_eq!(kinds("FALSE"), vec![TokenKind::Bool(false)]);
        assert_eq!(kinds("true"), vec![TokenKind::Bool(true)]);
        assert_eq!(kinds("TrUe"), vec![TokenKind::Bool(true)]);
        // function-call shape: TRUE( stays a name (Excel: #NAME?)
        assert_eq!(
            kinds("TRUE()"),
            vec![
                TokenKind::ReferenceLike,
                TokenKind::LParen,
                TokenKind::RParen
            ]
        );
        // not exact matches stay names
        assert_eq!(kinds("TRUEX"), vec![TokenKind::ReferenceLike]);
        assert_eq!(kinds("TRUE1"), vec![TokenKind::ReferenceLike]);
    }

    #[test]
    fn error_literals() {
        for code in [
            "#NULL!",
            "#DIV/0!",
            "#VALUE!",
            "#REF!",
            "#NAME?",
            "#NUM!",
            "#N/A",
            "#SPILL!",
            "#CALC!",
            "#CYCLE!",
            "#ERROR!",
            "#GETTING_DATA",
        ] {
            let toks = lex(code);
            assert_eq!(toks.len(), 1, "{code}");
            assert!(
                matches!(toks[0].kind, TokenKind::Error(_)),
                "{code} -> {:?}",
                toks[0].kind
            );
            assert_eq!(toks[0].text(code), code);
        }
        assert_eq!(kinds("#n/a"), vec![TokenKind::Error(CalcError::Na)]);
        assert_eq!(
            kinds("1+#REF!"),
            vec![
                TokenKind::Number,
                TokenKind::Plus,
                TokenKind::Error(CalcError::Ref)
            ]
        );
        // error code followed by a name char is not an error literal (Excel: #NAME?)
        assert_eq!(kinds("#REF!A1"), vec![TokenKind::ReferenceLike]);
        assert_eq!(kinds("#N/A!"), vec![TokenKind::ReferenceLike]);
        assert_eq!(kinds("#DIV/0!x"), vec![TokenKind::ReferenceLike]);
        // unknown #... is not an error literal
        assert_eq!(kinds("#FOO"), vec![TokenKind::ReferenceLike]);
        assert_eq!(kinds("A1#REF!"), vec![TokenKind::ReferenceLike]);
    }

    #[test]
    fn operators() {
        assert_eq!(kinds("+"), vec![TokenKind::Plus]);
        assert_eq!(kinds("-"), vec![TokenKind::Minus]);
        assert_eq!(kinds("*"), vec![TokenKind::Mul]);
        assert_eq!(kinds("/"), vec![TokenKind::Div]);
        assert_eq!(kinds("^"), vec![TokenKind::Pow]);
        assert_eq!(kinds("&"), vec![TokenKind::Concat]);
        assert_eq!(kinds("%"), vec![TokenKind::Percent]);
        assert_eq!(kinds("@"), vec![TokenKind::At]);
        assert_eq!(
            kinds("1=2"),
            vec![TokenKind::Number, TokenKind::Eq, TokenKind::Number]
        );
        assert_eq!(kinds("<"), vec![TokenKind::Lt]);
        assert_eq!(kinds(">"), vec![TokenKind::Gt]);
        assert_eq!(kinds("<>"), vec![TokenKind::Ne]);
        assert_eq!(kinds("<="), vec![TokenKind::Le]);
        assert_eq!(kinds(">="), vec![TokenKind::Ge]);
        assert_eq!(
            kinds("1<>2"),
            vec![TokenKind::Number, TokenKind::Ne, TokenKind::Number]
        );
        assert_eq!(
            kinds("<>=<"),
            vec![TokenKind::Ne, TokenKind::Eq, TokenKind::Lt]
        );
        assert_eq!(kinds("<><"), vec![TokenKind::Ne, TokenKind::Lt]);
    }

    #[test]
    fn structural_tokens() {
        assert_eq!(kinds("()"), vec![TokenKind::LParen, TokenKind::RParen]);
        assert_eq!(kinds("{}"), vec![TokenKind::LBrace, TokenKind::RBrace]);
        assert_eq!(kinds(","), vec![TokenKind::Comma]);
        assert_eq!(kinds(";"), vec![TokenKind::SemiColon]);
        assert_eq!(kinds(":"), vec![TokenKind::Colon]);
        assert_eq!(
            kinds("(1,2)"),
            vec![
                TokenKind::LParen,
                TokenKind::Number,
                TokenKind::Comma,
                TokenKind::Number,
                TokenKind::RParen
            ]
        );
        assert_eq!(
            kinds("{1;2}"),
            vec![
                TokenKind::LBrace,
                TokenKind::Number,
                TokenKind::SemiColon,
                TokenKind::Number,
                TokenKind::RBrace
            ]
        );
        assert_eq!(
            kinds("A1:B5"),
            vec![
                TokenKind::ReferenceLike,
                TokenKind::Colon,
                TokenKind::ReferenceLike
            ]
        );
    }

    #[test]
    fn references() {
        let t = lex("A1");
        assert_eq!(t[0].kind, TokenKind::ReferenceLike);
        assert_eq!(t[0].span, 0..2);
        assert_eq!(t[0].text("A1"), "A1");
        assert_eq!(kinds("$B$2"), vec![TokenKind::ReferenceLike]);
        assert_eq!(kinds("B$2"), vec![TokenKind::ReferenceLike]);

        let t = lex("A1:B5");
        assert_eq!(t[0].span, 0..2);
        assert_eq!(t[1].span, 2..3);
        assert_eq!(t[2].span, 3..5);

        // quoted sheet name containing a space: one token, space is not an Intersect
        let t = lex("'My Sheet'!A1");
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].kind, TokenKind::ReferenceLike);
        assert_eq!(t[0].span, 0..13);
        assert_eq!(t[0].text("'My Sheet'!A1"), "'My Sheet'!A1");

        // doubled '' escape inside a sheet name
        let t = lex("'It''s'!A1");
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].text("'It''s'!A1"), "'It''s'!A1");

        // external prefix
        let t = lex("[Book.xlsx]Sheet1!A1");
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].text("[Book.xlsx]Sheet1!A1"), "[Book.xlsx]Sheet1!A1");

        // structured reference: internal commas/colons stay inside the token
        let t = lex("Table1[[#Data],[col1]:[col2]]");
        assert_eq!(t.len(), 1);
        assert_eq!(
            t[0].text("Table1[[#Data],[col1]:[col2]]"),
            "Table1[[#Data],[col1]:[col2]]"
        );

        let t = lex("Table1[#All]");
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].text("Table1[#All]"), "Table1[#All]");
    }

    #[test]
    fn function_heads() {
        assert_eq!(
            kinds("SUM(1)"),
            vec![
                TokenKind::ReferenceLike,
                TokenKind::LParen,
                TokenKind::Number,
                TokenKind::RParen
            ]
        );
        assert_eq!(
            kinds("SUM(A1:A5)"),
            vec![
                TokenKind::ReferenceLike,
                TokenKind::LParen,
                TokenKind::ReferenceLike,
                TokenKind::Colon,
                TokenKind::ReferenceLike,
                TokenKind::RParen
            ]
        );
        assert_eq!(
            kinds("_xlfn.XLOOKUP()"),
            vec![
                TokenKind::ReferenceLike,
                TokenKind::LParen,
                TokenKind::RParen
            ]
        );
        assert_eq!(
            kinds("TODAY()"),
            vec![
                TokenKind::ReferenceLike,
                TokenKind::LParen,
                TokenKind::RParen
            ]
        );
    }

    #[test]
    fn function_head_helper() {
        let t = lex("SUM (A1)");
        assert!(Token::is_function_head(&t, 0));
        assert!(!Token::is_function_head(&t, 2));
        let t = lex("A1+B1");
        assert!(!Token::is_function_head(&t, 0));
        let t = lex("IF (a,b,c)");
        assert!(Token::is_function_head(&t, 0));
    }

    #[test]
    fn intersection_space() {
        assert_eq!(
            kinds("A1 B2"),
            vec![
                TokenKind::ReferenceLike,
                TokenKind::Intersect,
                TokenKind::ReferenceLike
            ]
        );
        assert_eq!(
            kinds("A1:A5 B1:B5"),
            vec![
                TokenKind::ReferenceLike,
                TokenKind::Colon,
                TokenKind::ReferenceLike,
                TokenKind::Intersect,
                TokenKind::ReferenceLike,
                TokenKind::Colon,
                TokenKind::ReferenceLike
            ]
        );
        assert_eq!(
            kinds("1 2"),
            vec![TokenKind::Number, TokenKind::Intersect, TokenKind::Number]
        );
        // runs of whitespace collapse into a single Intersect
        assert_eq!(
            kinds("A1  B2"),
            vec![
                TokenKind::ReferenceLike,
                TokenKind::Intersect,
                TokenKind::ReferenceLike
            ]
        );
        assert_eq!(
            kinds("A1 $B$1"),
            vec![
                TokenKind::ReferenceLike,
                TokenKind::Intersect,
                TokenKind::ReferenceLike
            ]
        );
        assert_eq!(
            kinds("A1 .5"),
            vec![
                TokenKind::ReferenceLike,
                TokenKind::Intersect,
                TokenKind::Number
            ]
        );
    }

    #[test]
    fn insignificant_space() {
        // space before '(' is function-call whitespace, never an Intersect
        assert_eq!(
            kinds("SUM (A1)"),
            vec![
                TokenKind::ReferenceLike,
                TokenKind::LParen,
                TokenKind::ReferenceLike,
                TokenKind::RParen
            ]
        );
        assert_eq!(
            kinds("IF (a,b,c)"),
            vec![
                TokenKind::ReferenceLike,
                TokenKind::LParen,
                TokenKind::ReferenceLike,
                TokenKind::Comma,
                TokenKind::ReferenceLike,
                TokenKind::Comma,
                TokenKind::ReferenceLike,
                TokenKind::RParen
            ]
        );
        // spaces around separators / operators / parens are dropped
        assert_eq!(
            kinds("A1 : B5"),
            vec![
                TokenKind::ReferenceLike,
                TokenKind::Colon,
                TokenKind::ReferenceLike
            ]
        );
        assert_eq!(
            kinds("A1 + 1"),
            vec![TokenKind::ReferenceLike, TokenKind::Plus, TokenKind::Number]
        );
        assert_eq!(
            kinds("( A1 )"),
            vec![
                TokenKind::LParen,
                TokenKind::ReferenceLike,
                TokenKind::RParen
            ]
        );
        assert_eq!(kinds("= A1"), vec![TokenKind::ReferenceLike]);
        assert_eq!(
            kinds("SUM( A1 )"),
            vec![
                TokenKind::ReferenceLike,
                TokenKind::LParen,
                TokenKind::ReferenceLike,
                TokenKind::RParen
            ]
        );
    }

    #[test]
    fn leading_equals_is_skipped() {
        assert_eq!(
            kinds("=1+1"),
            vec![TokenKind::Number, TokenKind::Plus, TokenKind::Number]
        );
        assert_eq!(kinds("=A1"), vec![TokenKind::ReferenceLike]);
    }

    #[test]
    fn lexing_errors() {
        assert!(tokenize("\"abc").is_err());
        assert!(tokenize("'My Sheet!A1").is_err());
        assert!(tokenize("(1+2").is_err());
        assert!(tokenize("SUM(").is_err());
        assert!(tokenize("{1,2").is_err());
        assert!(tokenize("Table1[[#Data]").is_err());
        assert!(tokenize(")").is_err());
        assert!(tokenize("}").is_err());
        let e = tokenize("\"abc").unwrap_err();
        assert_eq!(e.code, CalcError::Value);
        assert_eq!(e.span, 0..4);
    }

    #[test]
    fn every_token_has_a_valid_span() {
        for src in [
            ".5+1",
            "'My Sheet'!A1",
            "[Book.xlsx]Sheet1!A1",
            "Table1[[#Data],[col1]:[col2]]",
            "SUM (A1)",
            "A1:A5 B1:B5",
            "\"a\"\"b\"",
            "1<>2",
        ] {
            let toks = tokenize(src).unwrap();
            for t in &toks {
                // slicing via the span must not panic and must match the span length
                let text = t.text(src);
                assert_eq!(text.len(), t.span.len(), "{src}: {t:?}");
                assert_eq!(text, &src[t.span.clone()], "{src}: {t:?}");
            }
        }
    }
}
