// formula.rs — shared-formula translation, ported from openpyxl formula/translate.py
// and formula/tokenizer.py.
//
// translate_body(anchor_text_without_eq, rdelta, cdelta) reproduces
// Translator.translate_formula: tokenize, and for OPERAND+RANGE tokens apply
// translate_range (translate.py:101-166). String literals / numbers / function
// names / error codes pass through unchanged. On out-of-range shift the offending
// reference becomes "#REF!" (Excel semantics; openpyxl raises TranslatorError).

// ---- column-letter helpers ----
pub fn letters_to_index(s: &[u8]) -> Option<u32> {
    if s.is_empty() || s.len() > 3 {
        return None;
    }
    let mut v: u32 = 0;
    for &b in s {
        let d = if b.is_ascii_uppercase() {
            b - b'A'
        } else if b.is_ascii_lowercase() {
            b - b'a'
        } else {
            return None;
        };
        v = v * 26 + (d as u32 + 1);
    }
    Some(v)
}
pub fn index_to_letters(mut n: u32) -> String {
    let mut s = Vec::new();
    while n > 0 {
        let r = ((n - 1) % 26) as u8;
        s.push(b'A' + r);
        n = (n - 1) / 26;
    }
    s.reverse();
    // Only ASCII A–Z bytes are pushed above.
    String::from_utf8(s).unwrap_or_default()
}

// ---- piece translators (translate.py:60-85) ----
fn tr_row(row_str: &str, rdelta: i32) -> Result<String, ()> {
    if row_str.starts_with('$') {
        return Ok(row_str.to_string());
    }
    let n: i32 = row_str.parse().map_err(|_| ())?;
    let nn = n + rdelta;
    if nn <= 0 {
        return Err(());
    }
    Ok(nn.to_string())
}
fn tr_col(col_str: &str, cdelta: i32) -> Result<String, ()> {
    if col_str.starts_with('$') {
        return Ok(col_str.to_string());
    }
    let idx = letters_to_index(col_str.as_bytes()).ok_or(())? as i32;
    let nn = idx + cdelta;
    if nn < 1 {
        return Err(());
    }
    Ok(index_to_letters(nn as u32))
}

// pattern matchers (anchored, whole string)
fn is_row_piece(s: &str) -> bool {
    // \$?[1-9][0-9]{0,6}
    let b = s.as_bytes();
    let mut i = 0;
    if b.get(i) == Some(&b'$') {
        i += 1;
    }
    if b.get(i).map_or(true, |c| !(b'1'..=b'9').contains(c)) {
        return false;
    }
    i += 1;
    let mut digits = 1;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
        digits += 1;
    }
    i == b.len() && digits <= 7
}
fn is_col_piece(s: &str) -> bool {
    // \$?[A-Za-z]{1,3}
    let b = s.as_bytes();
    let mut i = 0;
    if b.get(i) == Some(&b'$') {
        i += 1;
    }
    let mut letters = 0;
    while i < b.len() && b[i].is_ascii_alphabetic() {
        i += 1;
        letters += 1;
    }
    i == b.len() && (1..=3).contains(&letters)
}
// split a cell ref "$?COL$?ROW" -> (col_part, row_part)
fn match_cell_ref(s: &str) -> Option<(&str, &str)> {
    let b = s.as_bytes();
    let mut i = 0;
    if b.get(i) == Some(&b'$') {
        i += 1;
    }
    let col_start = 0;
    let mut letters = 0;
    while i < b.len() && b[i].is_ascii_alphabetic() {
        i += 1;
        letters += 1;
    }
    if !(1..=3).contains(&letters) {
        return None;
    }
    let col_end = i;
    if b.get(i) == Some(&b'$') {
        i += 1;
    }
    if b.get(i).map_or(true, |c| !(b'1'..=b'9').contains(c)) {
        return None;
    }
    i += 1;
    let mut digits = 1;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
        digits += 1;
    }
    if i != b.len() || digits > 7 {
        return None;
    }
    Some((&s[col_start..col_end], &s[col_end..]))
}

fn strip_ws_name(range_str: &str) -> (String, &str) {
    if let Some(pos) = range_str.rfind('!') {
        (format!("{}!", &range_str[..pos]), &range_str[pos + 1..])
    } else {
        (String::new(), range_str)
    }
}

// translate.py:101-134
fn translate_range(range_str: &str, rd: i32, cd: i32) -> Result<String, ()> {
    let (ws, rest) = strip_ws_name(range_str);
    // ROW_RANGE  e.g. 3:4
    if let Some((a, b)) = rest.split_once(':') {
        if is_row_piece(a) && is_row_piece(b) {
            return Ok(format!("{}{}:{}", ws, tr_row(a, rd)?, tr_row(b, rd)?));
        }
        if is_col_piece(a) && is_col_piece(b) {
            return Ok(format!("{}{}:{}", ws, tr_col(a, cd)?, tr_col(b, cd)?));
        }
        // generic: split on ':' and translate each piece
        let mut pieces = Vec::new();
        for p in rest.split(':') {
            pieces.push(translate_range(p, rd, cd)?);
        }
        return Ok(format!("{}{}", ws, pieces.join(":")));
    }
    if let Some((col, row)) = match_cell_ref(rest) {
        return Ok(format!("{}{}{}", ws, tr_col(col, cd)?, tr_row(row, rd)?));
    }
    // named range: unchanged (return original incl. any ws part)
    Ok(range_str.to_string())
}

// ---- operand classification (tokenizer.py Token.make_operand) ----
fn is_range_operand(tok: &[u8]) -> bool {
    if tok.is_empty() {
        return false;
    }
    if tok[0] == b'"' || tok[0] == b'#' {
        return false; // text / error
    }
    if tok == b"TRUE" || tok == b"FALSE" {
        return false; // logical
    }
    // number?
    if let Ok(s) = std::str::from_utf8(tok) {
        if s.parse::<f64>().is_ok() {
            return false;
        }
    }
    true
}

const ERROR_CODES: [&[u8]; 8] = [
    b"#NULL!",
    b"#DIV/0!",
    b"#VALUE!",
    b"#REF!",
    b"#NAME?",
    b"#NUM!",
    b"#N/A",
    b"#GETTING_DATA",
];

fn match_error(s: &[u8]) -> Option<&'static [u8]> {
    for &e in ERROR_CODES.iter() {
        if s.starts_with(e) {
            return Some(e);
        }
    }
    None
}

// scientific-notation check: token so far matches ^[1-9](\.[0-9]+)?[Ee]$
fn is_sci(tok: &[u8]) -> bool {
    if tok.len() < 2 {
        return false;
    }
    if !(b'1'..=b'9').contains(&tok[0]) {
        return false;
    }
    let last = tok[tok.len() - 1];
    if last != b'E' && last != b'e' {
        return false;
    }
    let mid = &tok[1..tok.len() - 1];
    if mid.is_empty() {
        return true;
    }
    if mid[0] != b'.' {
        return false;
    }
    mid[1..].iter().all(|c| c.is_ascii_digit()) && mid.len() >= 2
}

// scan a "..." literal starting at i (regex "(?:[^"]*"")*[^"]*"(?!"))
fn scan_dquote(f: &[u8], i: usize) -> usize {
    // returns length consumed including both quotes
    let mut j = i + 1;
    while j < f.len() {
        if f[j] == b'"' {
            if j + 1 < f.len() && f[j + 1] == b'"' {
                j += 2; // escaped ""
                continue;
            }
            return j + 1 - i;
        }
        j += 1;
    }
    f.len() - i
}
fn scan_squote(f: &[u8], i: usize) -> usize {
    let mut j = i + 1;
    while j < f.len() {
        if f[j] == b'\'' {
            if j + 1 < f.len() && f[j + 1] == b'\'' {
                j += 2;
                continue;
            }
            return j + 1 - i;
        }
        j += 1;
    }
    f.len() - i
}
fn scan_brackets(f: &[u8], i: usize) -> usize {
    let mut depth = 0i32;
    let mut j = i;
    while j < f.len() {
        match f[j] {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return j + 1 - i;
                }
            }
            _ => {}
        }
        j += 1;
    }
    f.len() - i
}

/// Translate a formula body (no leading '=') by (rdelta, cdelta), appending
/// the translated text into `out`. The buffer keeps growing across calls, so a
/// caller materialising many formulas into one arena pays no per-formula
/// allocation. The caller pre-sizes when it wants a tight first allocation.
pub fn translate_body_into(out: &mut Vec<u8>, body: &str, rd: i32, cd: i32) {
    let f = body.as_bytes();
    let n = f.len();
    let mut tok: Vec<u8> = Vec::new();

    fn flush(tok: &mut Vec<u8>, out: &mut Vec<u8>, rd: i32, cd: i32) {
        if tok.is_empty() {
            return;
        }
        if is_range_operand(tok) {
            let s = std::str::from_utf8(tok).unwrap_or("");
            match translate_range(s, rd, cd) {
                Ok(t) => out.extend_from_slice(t.as_bytes()),
                Err(_) => out.extend_from_slice(b"#REF!"),
            }
        } else {
            out.extend_from_slice(tok);
        }
        tok.clear();
    }

    let mut i = 0;
    while i < n {
        let c = f[i];
        match c {
            b'"' => {
                flush(&mut tok, out, rd, cd);
                let adv = scan_dquote(f, i);
                out.extend_from_slice(&f[i..i + adv]);
                i += adv;
            }
            b'\'' => {
                let adv = scan_squote(f, i);
                tok.extend_from_slice(&f[i..i + adv]);
                i += adv;
            }
            b'#' => {
                if let Some(err) = match_error(&f[i..]) {
                    // operand = pending token content + error code (tokenizer.py:161)
                    tok.extend_from_slice(err);
                    out.extend_from_slice(&tok); // error operand: passthrough
                    tok.clear();
                    i += err.len();
                } else {
                    tok.push(c);
                    i += 1;
                }
            }
            b'[' => {
                let adv = scan_brackets(f, i);
                tok.extend_from_slice(&f[i..i + adv]);
                i += adv;
            }
            b' ' | b'\n' => {
                flush(&mut tok, out, rd, cd);
                out.push(c);
                i += 1;
            }
            b'+' | b'-' | b'*' | b'/' | b'^' | b'&' | b'=' | b'>' | b'<' | b'%' => {
                if (c == b'+' || c == b'-') && is_sci(&tok) {
                    tok.push(c);
                    i += 1;
                } else {
                    flush(&mut tok, out, rd, cd);
                    // two-char ops >= <= <>
                    if (c == b'>' && f.get(i + 1) == Some(&b'='))
                        || (c == b'<'
                            && (f.get(i + 1) == Some(&b'=') || f.get(i + 1) == Some(&b'>')))
                    {
                        out.push(c);
                        out.push(f[i + 1]);
                        i += 2;
                    } else {
                        out.push(c);
                        i += 1;
                    }
                }
            }
            b'(' | b'{' => {
                // pending token is a function name / opener -> passthrough (never a range)
                out.extend_from_slice(&tok);
                tok.clear();
                out.push(c);
                i += 1;
            }
            b')' | b'}' => {
                flush(&mut tok, out, rd, cd);
                out.push(c);
                i += 1;
            }
            b',' | b';' => {
                flush(&mut tok, out, rd, cd);
                out.push(c);
                i += 1;
            }
            _ => {
                tok.push(c);
                i += 1;
            }
        }
    }
    flush(&mut tok, out, rd, cd);
}

/// Translate a formula body (no leading '=') by (rdelta, cdelta) into an owned
/// String. Thin wrapper over [`translate_body_into`]; kept for callers that
/// need a fresh `String` per formula.
pub fn translate_body(body: &str, rd: i32, cd: i32) -> String {
    let mut out = Vec::with_capacity(body.len() + 8);
    translate_body_into(&mut out, body, rd, cd);
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn simple() {
        assert_eq!(translate_body("A2*2", 1, 0), "A3*2");
        assert_eq!(translate_body("B2+G2", 1, 0), "B3+G3");
        assert_eq!(translate_body("A2*2", 5, 0), "A7*2");
        assert_eq!(translate_body("$A$1+B1", 1, 1), "$A$1+C2");
        assert_eq!(translate_body("SUM(A1:A5)", 2, 0), "SUM(A3:A7)");
        assert_eq!(
            translate_body("CONCATENATE(\"A1\",B1)", 1, 0),
            "CONCATENATE(\"A1\",B2)"
        );
    }
}
