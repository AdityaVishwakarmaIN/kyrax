// calc/parser.rs — the Excel formula parser (spec `01_parser_reference.md` §3-§7).
//
// A Pratt parser over `calc::lexer`'s token stream. Binding powers reproduce
// Excel's documented precedence, tightest first:
//
//   `:` range  >  space intersection  >  `%`  >  unary `+`/`-`/`@`  >  `^`
//   >  `*` `/`  >  `+` `-`  >  `&`  >  `=` `<>` `<` `<=` `>` `>=`
//
// so `-2^2` is `(-2)^2 = 4` and `1&2=3` compares the concatenation.
//
// Reference operands are re-joined from source text: the lexer splits `A1:B5`
// into three tokens because `:` is reserved, so the parser reads the combined
// span back and hands it to `refs::parse_a1`, which owns all grid-bounds
// checking. A syntactically valid but out-of-grid reference folds to the
// `#REF!` literal — exactly what Excel stores — never a clamped coordinate.
//
// Anything the AST cannot represent faithfully is a `ParseError`, never a
// guess: the caller routes that cell to the hydration fallback path. Space
// intersection is the one operator in that bucket today (`ast::Expr` has no
// node for it), so it errors instead of silently becoming something else.

use crate::turbo::calc::ast::{
    BinaryOp, Expr, RefExpr, SuffixOp, TableColRef, TableRef, TableSection, UnaryOp,
};
use crate::turbo::calc::lexer::{Token, TokenKind, tokenize};
use crate::turbo::calc::refs::{RefParse, parse_a1};
use crate::turbo::calc::value::{ArrayValue, CalcError, CalcValue};

/// A parse failure, with a byte span into the formula text passed to
/// [`parse_formula`] (leading whitespace included).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    pub start: usize,
    pub end: usize,
}

// Binding powers; higher binds tighter.
const BP_COMPARE: u8 = 10;
const BP_CONCAT: u8 = 20;
const BP_ADD: u8 = 30;
const BP_MUL: u8 = 40;
const BP_POW: u8 = 50;
const BP_UNARY: u8 = 55;
const BP_PERCENT: u8 = 60;
const BP_INTERSECT: u8 = 70;
const BP_COLON: u8 = 80;

/// Parse a formula body into an [`Expr`]. A single leading `=` is optional.
/// Never panics: malformed or unrepresentable input returns [`ParseError`].
pub fn parse_formula(source: &str) -> Result<Expr, ParseError> {
    let offset = source.len() - source.trim_start().len();
    let src = source.trim_start();
    if src.is_empty() {
        return Err(ParseError {
            message: "empty formula".into(),
            start: offset,
            end: source.len(),
        });
    }
    let tokens = tokenize(src).map_err(|e| ParseError {
        message: format!("lex error {}", e.code.code()),
        start: e.span.start,
        end: e.span.end,
    })?;
    if tokens.is_empty() {
        return Err(ParseError {
            message: "formula has no expression".into(),
            start: 0,
            end: src.len(),
        });
    }
    let mut p = Parser {
        src,
        toks: tokens,
        pos: 0,
    };
    let out = p.parse_expr(0).and_then(|e| {
        if p.pos < p.toks.len() {
            Err(p.err_at(p.pos, "unexpected trailing token"))
        } else {
            Ok(e)
        }
    });
    out.map_err(|mut e| {
        e.start += offset;
        e.end += offset;
        e
    })
}

struct Parser<'a> {
    src: &'a str,
    toks: Vec<Token>,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn kind(&self, i: usize) -> Option<&TokenKind> {
        self.toks.get(i).map(|t| &t.kind)
    }

    fn peek(&self) -> Option<&TokenKind> {
        self.kind(self.pos)
    }

    fn text(&self, i: usize) -> &'a str {
        self.toks[i].text(self.src)
    }

    fn eat(&mut self, k: &TokenKind) -> bool {
        if self.peek() == Some(k) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, k: &TokenKind, what: &str) -> Result<(), ParseError> {
        if self.eat(k) {
            Ok(())
        } else {
            Err(self.err_at(self.pos, what))
        }
    }

    fn err_at(&self, i: usize, message: &str) -> ParseError {
        let (start, end) = match self.toks.get(i) {
            Some(t) => (t.span.start, t.span.end),
            None => (self.src.len(), self.src.len()),
        };
        ParseError {
            message: message.to_string(),
            start,
            end,
        }
    }

    fn parse_expr(&mut self, min_bp: u8) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_prefix()?;
        while let Some(kind) = self.kind(self.pos).cloned() {
            let (bp, op) = match kind {
                TokenKind::Colon => (BP_COLON, Infix::Colon),
                TokenKind::Intersect => (BP_INTERSECT, Infix::Intersect),
                TokenKind::Percent => (BP_PERCENT, Infix::Percent),
                TokenKind::Pow => (BP_POW, Infix::Bin(BinaryOp::Pow)),
                TokenKind::Mul => (BP_MUL, Infix::Bin(BinaryOp::Mul)),
                TokenKind::Div => (BP_MUL, Infix::Bin(BinaryOp::Div)),
                TokenKind::Plus => (BP_ADD, Infix::Bin(BinaryOp::Add)),
                TokenKind::Minus => (BP_ADD, Infix::Bin(BinaryOp::Sub)),
                TokenKind::Concat => (BP_CONCAT, Infix::Bin(BinaryOp::Concat)),
                TokenKind::Eq => (BP_COMPARE, Infix::Bin(BinaryOp::Eq)),
                TokenKind::Ne => (BP_COMPARE, Infix::Bin(BinaryOp::Ne)),
                TokenKind::Gt => (BP_COMPARE, Infix::Bin(BinaryOp::Gt)),
                TokenKind::Ge => (BP_COMPARE, Infix::Bin(BinaryOp::Ge)),
                TokenKind::Lt => (BP_COMPARE, Infix::Bin(BinaryOp::Lt)),
                TokenKind::Le => (BP_COMPARE, Infix::Bin(BinaryOp::Le)),
                _ => break,
            };
            if bp < min_bp {
                break;
            }
            let op_at = self.pos;
            self.pos += 1;
            lhs = match op {
                Infix::Percent => Expr::Suffix(SuffixOp::Percent, Box::new(lhs)),
                // No AST node represents range intersection, so it is refused
                // rather than approximated — the cell falls back instead.
                Infix::Intersect => {
                    return Err(self.err_at(op_at, "range intersection is not supported"));
                }
                Infix::Colon => {
                    let rhs = self.parse_expr(bp + 1)?;
                    Expr::Colon(Box::new(lhs), Box::new(rhs))
                }
                // All binary operators are left-associative in Excel,
                // including `^`: `2^3^2` is 64, not 512.
                Infix::Bin(b) => {
                    let rhs = self.parse_expr(bp + 1)?;
                    Expr::Binary(b, Box::new(lhs), Box::new(rhs))
                }
            };
        }
        Ok(lhs)
    }

    fn parse_prefix(&mut self) -> Result<Expr, ParseError> {
        let Some(kind) = self.peek().cloned() else {
            return Err(self.err_at(self.pos, "expected an operand"));
        };
        match kind {
            TokenKind::Plus => {
                self.pos += 1;
                let e = self.parse_expr(BP_UNARY)?;
                Ok(Expr::Unary(UnaryOp::Plus, Box::new(e)))
            }
            TokenKind::Minus => {
                self.pos += 1;
                let e = self.parse_expr(BP_UNARY)?;
                Ok(Expr::Unary(UnaryOp::Minus, Box::new(e)))
            }
            TokenKind::At => {
                self.pos += 1;
                let e = self.parse_expr(BP_UNARY)?;
                Ok(Expr::Unary(UnaryOp::ImplicitIntersect, Box::new(e)))
            }
            TokenKind::String => {
                let s = self.toks[self.pos].string_value(self.src);
                self.pos += 1;
                Ok(Expr::Value(CalcValue::text(s)))
            }
            TokenKind::Bool(b) => {
                self.pos += 1;
                Ok(Expr::Value(CalcValue::bool(b)))
            }
            TokenKind::Error(e) => {
                self.pos += 1;
                Ok(Expr::Value(CalcValue::err(e)))
            }
            TokenKind::LParen => {
                self.pos += 1;
                let first = self.parse_expr(0)?;
                if self.peek() == Some(&TokenKind::Comma) {
                    let mut items = vec![first];
                    while self.eat(&TokenKind::Comma) {
                        items.push(self.parse_expr(0)?);
                    }
                    self.expect(&TokenKind::RParen, "expected ')'")?;
                    return Ok(Expr::Union(items));
                }
                self.expect(&TokenKind::RParen, "expected ')'")?;
                Ok(first)
            }
            TokenKind::LBrace => self.parse_array(),
            TokenKind::Number | TokenKind::ReferenceLike => self.parse_operand(),
            _ => Err(self.err_at(self.pos, "expected an operand")),
        }
    }

    /// A number, reference, name, table reference or function call — plus the
    /// `A1:B5` / `1:3` / `A:C` fold that re-joins a range the lexer split on
    /// its reserved `:`.
    fn parse_operand(&mut self) -> Result<Expr, ParseError> {
        let i = self.pos;
        if Token::is_function_head(&self.toks, i) {
            return self.parse_call();
        }
        if let Some(e) = self.try_fold_range(i) {
            self.pos = i + 3;
            return Ok(e);
        }
        match self.kind(i) {
            Some(TokenKind::Number) => {
                let raw = self.text(i);
                let n: f64 = raw
                    .parse()
                    .map_err(|_| self.err_at(i, "malformed number literal"))?;
                if !n.is_finite() {
                    return Err(self.err_at(i, "number literal out of range"));
                }
                self.pos += 1;
                Ok(Expr::Value(CalcValue::number(n)))
            }
            _ => {
                let raw = self.text(i);
                match classify_reference(raw) {
                    Classified::Ref(r) => {
                        self.pos += 1;
                        Ok(Expr::Ref(r))
                    }
                    // Excel stores an out-of-grid reference as the literal
                    // `#REF!`; reproduce that rather than inventing a cell.
                    Classified::RefError => {
                        self.pos += 1;
                        Ok(Expr::Value(CalcValue::err(CalcError::Ref)))
                    }
                    Classified::NotRef => Err(self.err_at(i, "unrecognised operand")),
                }
            }
        }
    }

    /// Try to read tokens `i`, `i+1` (`:`), `i+2` back as one A1 range. Returns
    /// `None` unless the joined text really is a reference — `foo:bar` stays
    /// two names joined by the range operator.
    fn try_fold_range(&self, i: usize) -> Option<Expr> {
        if self.kind(i + 1) != Some(&TokenKind::Colon) {
            return None;
        }
        match self.kind(i + 2) {
            Some(TokenKind::Number) | Some(TokenKind::ReferenceLike) => {}
            _ => return None,
        }
        if Token::is_function_head(&self.toks, i + 2) {
            return None;
        }
        let joined = &self.src[self.toks[i].span.start..self.toks[i + 2].span.end];
        match classify_reference(joined) {
            Classified::Ref(RefExpr::Name { .. }) | Classified::NotRef => None,
            Classified::Ref(r) => Some(Expr::Ref(r)),
            Classified::RefError => Some(Expr::Value(CalcValue::err(CalcError::Ref))),
        }
    }

    fn parse_call(&mut self) -> Result<Expr, ParseError> {
        let name = normalize_func_name(self.text(self.pos));
        self.pos += 2; // name + '('
        let mut args: Vec<Expr> = Vec::new();
        if self.eat(&TokenKind::RParen) {
            return Ok(Expr::Function { name, args });
        }
        loop {
            match self.peek() {
                Some(TokenKind::Comma) | Some(TokenKind::RParen) => args.push(Expr::Null),
                _ => args.push(self.parse_expr(0)?),
            }
            if self.eat(&TokenKind::Comma) {
                continue;
            }
            self.expect(&TokenKind::RParen, "expected ',' or ')'")?;
            break;
        }
        Ok(Expr::Function { name, args })
    }

    /// `{1,2;3,4}` — literal elements only. Short rows are padded with `#N/A`,
    /// which is what Excel shows for the missing cells.
    fn parse_array(&mut self) -> Result<Expr, ParseError> {
        let open = self.pos;
        self.pos += 1; // '{'
        let mut rows: Vec<Vec<CalcValue>> = vec![Vec::new()];
        loop {
            let v = self.parse_array_element()?;
            rows.last_mut().expect("row present").push(v);
            if self.eat(&TokenKind::Comma) {
                continue;
            }
            if self.eat(&TokenKind::SemiColon) {
                rows.push(Vec::new());
                continue;
            }
            self.expect(&TokenKind::RBrace, "expected ',', ';' or '}'")?;
            break;
        }
        let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
        if cols == 0 || rows.is_empty() {
            return Err(self.err_at(open, "empty array literal"));
        }
        let mut data = Vec::with_capacity(rows.len() * cols);
        for row in &rows {
            for c in 0..cols {
                data.push(
                    row.get(c)
                        .cloned()
                        .unwrap_or(CalcValue::Error(CalcError::Na)),
                );
            }
        }
        Ok(Expr::Value(CalcValue::array(ArrayValue::new(
            rows.len() as u32,
            cols as u32,
            data,
        ))))
    }

    fn parse_array_element(&mut self) -> Result<CalcValue, ParseError> {
        let mut negate = false;
        loop {
            match self.peek() {
                Some(TokenKind::Minus) => {
                    negate = !negate;
                    self.pos += 1;
                }
                Some(TokenKind::Plus) => self.pos += 1,
                _ => break,
            }
        }
        let i = self.pos;
        let v = match self.peek().cloned() {
            Some(TokenKind::Number) => {
                let n: f64 = self
                    .text(i)
                    .parse()
                    .map_err(|_| self.err_at(i, "malformed number in array"))?;
                if !n.is_finite() {
                    return Err(self.err_at(i, "number in array out of range"));
                }
                CalcValue::number(if negate { -n } else { n })
            }
            Some(TokenKind::String) => CalcValue::text(self.toks[i].string_value(self.src)),
            Some(TokenKind::Bool(b)) => CalcValue::bool(b),
            Some(TokenKind::Error(e)) => CalcValue::err(e),
            _ => return Err(self.err_at(i, "array elements must be literals")),
        };
        if negate && !matches!(v, CalcValue::Number(_)) {
            return Err(self.err_at(i, "unary minus on a non-numeric array element"));
        }
        self.pos += 1;
        Ok(v)
    }
}

enum Infix {
    Bin(BinaryOp),
    Colon,
    Intersect,
    Percent,
}

/// Canonical function name: upper-cased, with the OOXML future-function
/// prefixes stripped so `_xlfn.XLOOKUP` resolves as `XLOOKUP`.
fn normalize_func_name(raw: &str) -> String {
    let up = raw.trim().to_ascii_uppercase();
    for prefix in ["_XLFN._XLWS.", "_XLFN.", "_XLWS."] {
        if let Some(rest) = up.strip_prefix(prefix) {
            return rest.to_string();
        }
    }
    up
}

/// Outcome of classifying a reference-like run.
enum Classified {
    Ref(RefExpr),
    /// Well-formed but off-grid → the `#REF!` literal.
    RefError,
    /// Not a reference, a name, or a table reference.
    NotRef,
}

/// Turn one reference-like run into a [`RefExpr`] (spec §5).
fn classify_reference(text: &str) -> Classified {
    let text = text.trim();
    if text.is_empty() {
        return Classified::NotRef;
    }
    match find_bang(text) {
        Some(i) => {
            let prefix = unquote_sheet(text[..i].trim());
            let rest = text[i + 1..].trim();
            if rest.is_empty() || prefix.is_empty() {
                return Classified::NotRef;
            }
            if let Some(after_book) = prefix.strip_prefix('[') {
                // `[Book.xlsx]Sheet1!A1` — external, always the fallback path.
                let Some(close) = after_book.find(']') else {
                    return Classified::NotRef;
                };
                let book = after_book[..close].to_string();
                let sheet = after_book[close + 1..].trim();
                let inner = if sheet.is_empty() {
                    rest.to_string()
                } else {
                    format!("{sheet}!{rest}")
                };
                return Classified::Ref(RefExpr::External { book, inner });
            }
            if let Some((from, to)) = prefix.split_once(':') {
                return match parse_a1(rest) {
                    RefParse::Ref(core) => Classified::Ref(RefExpr::Sheet3D {
                        from: from.trim().to_string(),
                        to: to.trim().to_string(),
                        inner: Box::new(core),
                    }),
                    RefParse::RefError => Classified::RefError,
                    RefParse::NotRef => Classified::NotRef,
                };
            }
            match parse_a1(rest) {
                RefParse::Ref(core) => Classified::Ref(RefExpr::Sheet {
                    name: prefix,
                    inner: Box::new(core),
                }),
                RefParse::RefError => Classified::RefError,
                RefParse::NotRef => {
                    if rest.contains('[') {
                        match parse_table(rest) {
                            Some(t) => Classified::Ref(RefExpr::Table(t)),
                            None => Classified::NotRef,
                        }
                    } else if is_name_like(rest) {
                        Classified::Ref(RefExpr::Name {
                            name: rest.to_string(),
                            sheet: Some(prefix),
                        })
                    } else {
                        Classified::NotRef
                    }
                }
            }
        }
        None => {
            if text.starts_with('[') {
                return Classified::NotRef;
            }
            if text.contains('[') {
                return match parse_table(text) {
                    Some(t) => Classified::Ref(RefExpr::Table(t)),
                    None => Classified::NotRef,
                };
            }
            match parse_a1(text) {
                RefParse::Ref(core) => Classified::Ref(RefExpr::Local(core)),
                RefParse::RefError => Classified::RefError,
                RefParse::NotRef => {
                    if is_name_like(text) {
                        Classified::Ref(RefExpr::Name {
                            name: text.to_string(),
                            sheet: None,
                        })
                    } else {
                        Classified::NotRef
                    }
                }
            }
        }
    }
}

/// Byte index of the sheet-separating `!`, skipping `'...'` quoted names
/// (`''` is an escaped quote) and `[...]` bracket runs.
fn find_bang(t: &str) -> Option<usize> {
    let b = t.as_bytes();
    let mut i = 0usize;
    let mut depth = 0i32;
    while i < b.len() {
        match b[i] {
            b'\'' => {
                i += 1;
                while i < b.len() {
                    if b[i] == b'\'' {
                        if i + 1 < b.len() && b[i + 1] == b'\'' {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
            }
            b'[' => depth += 1,
            b']' => depth -= 1,
            b'!' if depth <= 0 => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// `'It''s'` → `It's`; an unquoted prefix is returned as-is.
fn unquote_sheet(p: &str) -> String {
    if p.len() >= 2 && p.starts_with('\'') && p.ends_with('\'') {
        p[1..p.len() - 1].replace("''", "'")
    } else {
        p.to_string()
    }
}

/// Defined-name shape: starts with a letter, `_` or `\`, and contains only
/// name characters afterwards. Anything else is refused so a malformed run
/// becomes a `ParseError` rather than a bogus name lookup.
fn is_name_like(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' || c == '\\' => {}
        _ => return false,
    }
    chars.all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '\\' || c == '?')
}

/// `Table1[[#Data],[Col1]:[Col2]]` → [`TableRef`] (spec §5.3).
fn parse_table(text: &str) -> Option<TableRef> {
    let open = text.find('[')?;
    let name = text[..open].trim();
    if name.is_empty() || !text.ends_with(']') {
        return None;
    }
    let body = &text[open + 1..text.len() - 1];
    let mut sections: Vec<TableSection> = Vec::new();
    let mut columns: Vec<TableColRef> = Vec::new();
    for part in split_top(body) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some(idx) = top_colon(part) {
            columns.push(TableColRef::Range(
                strip_brackets(&part[..idx]),
                strip_brackets(&part[idx + 1..]),
            ));
            continue;
        }
        let mut p = part;
        if let Some(rest) = p.strip_prefix('@') {
            sections.push(TableSection::ThisRow);
            p = rest.trim();
            if p.is_empty() {
                continue;
            }
        }
        let inner = strip_brackets(p);
        if let Some(kw) = inner.strip_prefix('#') {
            match kw.trim().to_ascii_lowercase().as_str() {
                "all" => sections.push(TableSection::All),
                "data" => sections.push(TableSection::Data),
                "headers" => sections.push(TableSection::Headers),
                "totals" => sections.push(TableSection::Totals),
                "this row" => sections.push(TableSection::ThisRow),
                _ => return None,
            }
        } else if !inner.is_empty() {
            columns.push(TableColRef::Column(inner));
        }
    }
    Some(TableRef {
        book: None,
        name: name.to_string(),
        sections,
        columns,
    })
}

/// Split a table body on depth-0 commas.
fn split_top(body: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (i, byte) in body.bytes().enumerate() {
        match byte {
            b'[' => depth += 1,
            b']' => depth -= 1,
            b',' if depth == 0 => {
                out.push(&body[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&body[start..]);
    out
}

/// Byte index of a depth-0 `:` in a table part, or `None`.
fn top_colon(part: &str) -> Option<usize> {
    let mut depth = 0i32;
    for (i, byte) in part.bytes().enumerate() {
        match byte {
            b'[' => depth += 1,
            b']' => depth -= 1,
            b':' if depth == 0 => return Some(i),
            _ => {}
        }
    }
    None
}

/// Remove one `[...]` wrapper, if present.
fn strip_brackets(s: &str) -> String {
    let s = s.trim();
    let inner = s
        .strip_prefix('[')
        .and_then(|r| r.strip_suffix(']'))
        .unwrap_or(s);
    inner.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turbo::calc::ast::{CellRef, ColumnRef, RangeRef, RefCore, RowRef};
    use pretty_assertions::assert_eq;

    fn p(s: &str) -> Expr {
        parse_formula(s).unwrap_or_else(|e| panic!("{s:?} failed: {e:?}"))
    }

    fn num(n: f64) -> Expr {
        Expr::Value(CalcValue::number(n))
    }

    fn cell(col: u16, row: u32) -> CellRef {
        CellRef {
            col,
            row,
            abs_col: false,
            abs_row: false,
        }
    }

    fn local(core: RefCore) -> Expr {
        Expr::Ref(RefExpr::Local(core))
    }

    #[test]
    fn arithmetic_precedence() {
        assert_eq!(
            p("=1+2*3"),
            Expr::Binary(
                BinaryOp::Add,
                Box::new(num(1.0)),
                Box::new(Expr::Binary(
                    BinaryOp::Mul,
                    Box::new(num(2.0)),
                    Box::new(num(3.0))
                ))
            )
        );
        assert_eq!(
            p("(1+2)*3"),
            Expr::Binary(
                BinaryOp::Mul,
                Box::new(Expr::Binary(
                    BinaryOp::Add,
                    Box::new(num(1.0)),
                    Box::new(num(2.0))
                )),
                Box::new(num(3.0))
            )
        );
    }

    #[test]
    fn unary_minus_binds_tighter_than_power() {
        // Excel: -2^2 = 4, so the negation must be the base.
        assert_eq!(
            p("=-2^2"),
            Expr::Binary(
                BinaryOp::Pow,
                Box::new(Expr::Unary(UnaryOp::Minus, Box::new(num(2.0)))),
                Box::new(num(2.0))
            )
        );
        // and `^` is left-associative: 2^3^2 = 64.
        assert_eq!(
            p("2^3^2"),
            Expr::Binary(
                BinaryOp::Pow,
                Box::new(Expr::Binary(
                    BinaryOp::Pow,
                    Box::new(num(2.0)),
                    Box::new(num(3.0))
                )),
                Box::new(num(2.0))
            )
        );
    }

    #[test]
    fn comparison_is_loosest_and_concat_next() {
        assert_eq!(
            p("=1&2=3"),
            Expr::Binary(
                BinaryOp::Eq,
                Box::new(Expr::Binary(
                    BinaryOp::Concat,
                    Box::new(num(1.0)),
                    Box::new(num(2.0))
                )),
                Box::new(num(3.0))
            )
        );
        assert_eq!(
            p("=1+2&3"),
            Expr::Binary(
                BinaryOp::Concat,
                Box::new(Expr::Binary(
                    BinaryOp::Add,
                    Box::new(num(1.0)),
                    Box::new(num(2.0))
                )),
                Box::new(num(3.0))
            )
        );
    }

    #[test]
    fn percent_is_a_suffix() {
        assert_eq!(
            p("=50%"),
            Expr::Suffix(SuffixOp::Percent, Box::new(num(50.0)))
        );
        assert_eq!(
            p("=-50%"),
            Expr::Unary(
                UnaryOp::Minus,
                Box::new(Expr::Suffix(SuffixOp::Percent, Box::new(num(50.0))))
            )
        );
    }

    #[test]
    fn functions_and_empty_arguments() {
        assert_eq!(
            p("=SUM(A1,,5)"),
            Expr::Function {
                name: "SUM".into(),
                args: vec![local(RefCore::Cell(cell(0, 0))), Expr::Null, num(5.0)]
            }
        );
        assert_eq!(
            p("=NOW()"),
            Expr::Function {
                name: "NOW".into(),
                args: vec![]
            }
        );
        // future-function prefix is normalised away
        match p("=_xlfn.XLOOKUP(A1,B1:B2,C1:C2)") {
            Expr::Function { name, args } => {
                assert_eq!(name, "XLOOKUP");
                assert_eq!(args.len(), 3);
            }
            other => panic!("{other:?}"),
        }
        // nested calls, case-insensitive names
        match p("=if(sum(A1:A2)>1,\"y\",\"n\")") {
            Expr::Function { name, args } => {
                assert_eq!(name, "IF");
                assert_eq!(args.len(), 3);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn array_literals() {
        match p("={1,2;3,4}") {
            Expr::Value(CalcValue::Array(a)) => {
                assert_eq!(a.shape(), (2, 2));
                assert_eq!(*a.get(1, 0), CalcValue::number(3.0));
            }
            other => panic!("{other:?}"),
        }
        // short rows pad with #N/A, negatives and mixed literals allowed
        match p("={-1,\"a\";TRUE}") {
            Expr::Value(CalcValue::Array(a)) => {
                assert_eq!(a.shape(), (2, 2));
                assert_eq!(*a.get(0, 0), CalcValue::number(-1.0));
                assert_eq!(*a.get(0, 1), CalcValue::text("a"));
                assert_eq!(*a.get(1, 0), CalcValue::bool(true));
                assert_eq!(*a.get(1, 1), CalcValue::err(CalcError::Na));
            }
            other => panic!("{other:?}"),
        }
        assert!(parse_formula("={A1}").is_err());
    }

    #[test]
    fn ranges_fold_back_together() {
        assert_eq!(
            p("=A1:B5"),
            local(RefCore::Range(RangeRef {
                start: cell(0, 0),
                end: cell(1, 4)
            }))
        );
        assert_eq!(p("=1:3"), local(RefCore::Row(RowRef { start: 0, end: 2 })));
        assert_eq!(
            p("=A:C"),
            local(RefCore::Column(ColumnRef { start: 0, end: 2 }))
        );
        assert_eq!(
            p("=$A$1"),
            local(RefCore::Cell(CellRef {
                col: 0,
                row: 0,
                abs_col: true,
                abs_row: true
            }))
        );
        // a colon between non-reference operands stays a Colon node
        assert!(matches!(p("=INDIRECT(\"A5\"):B10"), Expr::Colon(_, _)));
    }

    #[test]
    fn sheet_qualified_references() {
        assert_eq!(
            p("=Sheet1!A1"),
            Expr::Ref(RefExpr::Sheet {
                name: "Sheet1".into(),
                inner: Box::new(RefCore::Cell(cell(0, 0)))
            })
        );
        assert_eq!(
            p("='My Sheet'!A1:B2"),
            Expr::Ref(RefExpr::Sheet {
                name: "My Sheet".into(),
                inner: Box::new(RefCore::Range(RangeRef {
                    start: cell(0, 0),
                    end: cell(1, 1)
                }))
            })
        );
        assert_eq!(
            p("=Sheet1:Sheet3!A1"),
            Expr::Ref(RefExpr::Sheet3D {
                from: "Sheet1".into(),
                to: "Sheet3".into(),
                inner: Box::new(RefCore::Cell(cell(0, 0)))
            })
        );
    }

    #[test]
    fn defined_names_carry_their_sheet_qualifier() {
        assert_eq!(
            p("=foo"),
            Expr::Ref(RefExpr::Name {
                name: "foo".into(),
                sheet: None
            })
        );
        assert_eq!(
            p("=Sheet1!foo"),
            Expr::Ref(RefExpr::Name {
                name: "foo".into(),
                sheet: Some("Sheet1".into())
            })
        );
        assert_eq!(
            p("='My Sheet'!foo"),
            Expr::Ref(RefExpr::Name {
                name: "foo".into(),
                sheet: Some("My Sheet".into())
            })
        );
        assert_eq!(
            p("='It''s'!foo"),
            Expr::Ref(RefExpr::Name {
                name: "foo".into(),
                sheet: Some("It's".into())
            })
        );
        // `foo:bar` IS a column range in Excel (columns FOO through BAR), and
        // folds like one; a name that cannot spell a column stays two names
        // joined by the range operator.
        assert_eq!(
            p("=foo:bar"),
            local(RefCore::Column(ColumnRef {
                start: 1395, // BAR
                end: 4460,   // FOO
            }))
        );
        assert!(matches!(p("=my_name:other_name"), Expr::Colon(_, _)));
    }

    #[test]
    fn out_of_grid_reference_becomes_the_ref_error_literal() {
        assert_eq!(
            p("=Sheet1!XFE1"),
            Expr::Value(CalcValue::err(CalcError::Ref))
        );
        assert_eq!(p("=A1048577"), Expr::Value(CalcValue::err(CalcError::Ref)));
    }

    #[test]
    fn error_literals() {
        assert_eq!(p("=#DIV/0!"), Expr::Value(CalcValue::err(CalcError::Div0)));
        assert_eq!(
            p("=IFERROR(A1,#N/A)"),
            Expr::Function {
                name: "IFERROR".into(),
                args: vec![
                    local(RefCore::Cell(cell(0, 0))),
                    Expr::Value(CalcValue::err(CalcError::Na))
                ]
            }
        );
    }

    #[test]
    fn external_and_table_references_are_preserved_for_fallback() {
        assert_eq!(
            p("=[Book.xlsx]Sheet1!A1"),
            Expr::Ref(RefExpr::External {
                book: "Book.xlsx".into(),
                inner: "Sheet1!A1".into()
            })
        );
        match p("=Table1[Col]") {
            Expr::Ref(RefExpr::Table(t)) => {
                assert_eq!(t.name, "Table1");
                assert_eq!(t.columns, vec![TableColRef::Column("Col".into())]);
                assert!(t.sections.is_empty());
            }
            other => panic!("{other:?}"),
        }
        match p("=Table1[[#Data],[C1]:[C2]]") {
            Expr::Ref(RefExpr::Table(t)) => {
                assert_eq!(t.sections, vec![TableSection::Data]);
                assert_eq!(
                    t.columns,
                    vec![TableColRef::Range("C1".into(), "C2".into())]
                );
            }
            other => panic!("{other:?}"),
        }
        match p("=Table1[@Amount]") {
            Expr::Ref(RefExpr::Table(t)) => {
                assert_eq!(t.sections, vec![TableSection::ThisRow]);
                assert_eq!(t.columns, vec![TableColRef::Column("Amount".into())]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn union_inside_parentheses() {
        match p("=SUM((A1,B2))") {
            Expr::Function { args, .. } => match &args[0] {
                Expr::Union(items) => assert_eq!(items.len(), 2),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn implicit_intersection_prefix() {
        assert_eq!(
            p("=@A1:A5"),
            Expr::Unary(
                UnaryOp::ImplicitIntersect,
                Box::new(local(RefCore::Range(RangeRef {
                    start: cell(0, 0),
                    end: cell(0, 4)
                })))
            )
        );
    }

    #[test]
    fn unsupported_and_malformed_input_errors_instead_of_guessing() {
        // space intersection has no AST node — refuse it
        assert!(parse_formula("=A1:A5 B1:B5").is_err());
        assert!(parse_formula("=1+").is_err());
        assert!(parse_formula("=SUM(A1").is_err());
        assert!(parse_formula("=(1+2))").is_err());
        assert!(parse_formula("=").is_err());
        assert!(parse_formula("").is_err());
        assert!(parse_formula("=1 2").is_err());
    }

    #[test]
    fn error_spans_point_into_the_original_text() {
        let e = parse_formula("  =1+").unwrap_err();
        assert!(e.start >= 2, "{e:?}");
        assert!(e.end <= "  =1+".len(), "{e:?}");
    }
}
