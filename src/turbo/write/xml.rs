//! Fast XML helpers: escape, column letters, number formatting.
//!
//! Duplicates small utilities also present on the read path (entity escape,
//! A1/col-letter). Dedup deferred — see W1_REPORT.md.

use std::io::Write;

/// Column index 1-based → Excel letters (1=A, 26=Z, 27=AA).
#[inline]
pub fn col_letters(mut col: u32, buf: &mut [u8; 4]) -> &[u8] {
    debug_assert!(col >= 1 && col <= 16384);
    let mut i = 4;
    while col > 0 {
        col -= 1;
        i -= 1;
        buf[i] = b'A' + (col % 26) as u8;
        col /= 26;
    }
    &buf[i..]
}

/// Write `A1`-style ref for (row, col) both 1-based.
#[inline]
pub fn write_coord(out: &mut Vec<u8>, row: u32, col: u32) {
    let mut buf = [0u8; 4];
    out.extend_from_slice(col_letters(col, &mut buf));
    let mut ib = itoa::Buffer::new();
    out.extend_from_slice(ib.format(row).as_bytes());
}

/// Format dimension ref `A1:C10`.
pub fn dimension_ref(min_row: u32, min_col: u32, max_row: u32, max_col: u32) -> String {
    let mut s = Vec::with_capacity(16);
    write_coord(&mut s, min_row, min_col);
    s.push(b':');
    write_coord(&mut s, max_row, max_col);
    String::from_utf8(s).unwrap()
}

#[inline]
pub fn write_f64(out: &mut Vec<u8>, v: f64) {
    let mut buf = ryu::Buffer::new();
    out.extend_from_slice(buf.format(v).as_bytes());
}

#[inline]
pub fn write_u32(out: &mut Vec<u8>, v: u32) {
    let mut buf = itoa::Buffer::new();
    out.extend_from_slice(buf.format(v).as_bytes());
}

/// Escape `& < >` for XML text content.
pub fn escape_text(s: &str) -> String {
    let mut needs = false;
    for b in s.bytes() {
        if matches!(b, b'&' | b'<' | b'>') {
            needs = true;
            break;
        }
    }
    if !needs {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Write escaped text directly into buffer.
pub fn write_escaped_text(out: &mut Vec<u8>, s: &str) {
    for b in s.bytes() {
        match b {
            b'&' => out.extend_from_slice(b"&amp;"),
            b'<' => out.extend_from_slice(b"&lt;"),
            b'>' => out.extend_from_slice(b"&gt;"),
            _ => out.push(b),
        }
    }
}

#[inline]
pub fn needs_preserve(s: &str) -> bool {
    s.as_bytes()
        .first()
        .map(|b| b.is_ascii_whitespace())
        .unwrap_or(false)
        || s.as_bytes()
            .last()
            .map(|b| b.is_ascii_whitespace())
            .unwrap_or(false)
}

/// Truncate to Excel's 32767 char limit.
pub fn truncate_str(s: &str) -> &str {
    if s.chars().count() <= 32767 {
        s
    } else {
        let mut end = 0;
        for (i, (byte_idx, _)) in s.char_indices().enumerate() {
            if i == 32767 {
                end = byte_idx;
                break;
            }
        }
        if end == 0 { s } else { &s[..end] }
    }
}

/// openpyxl ILLEGAL_CHARACTERS_RE: control chars except tab/LF/CR.
#[allow(dead_code)] // available for Python validation / W2+
pub fn has_illegal_chars(s: &str) -> bool {
    s.chars().any(|c| {
        let u = c as u32;
        u <= 8 || u == 11 || u == 12 || (14..=31).contains(&u)
    })
}

#[inline]
pub fn push(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(b);
}

#[inline]
pub fn push_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
}

/// Escape `& < > "` for XML attribute values.
#[inline]
pub fn write_escaped_attr(out: &mut Vec<u8>, s: &str) {
    for b in s.bytes() {
        match b {
            b'&' => out.extend_from_slice(b"&amp;"),
            b'<' => out.extend_from_slice(b"&lt;"),
            b'>' => out.extend_from_slice(b"&gt;"),
            b'"' => out.extend_from_slice(b"&quot;"),
            _ => out.push(b),
        }
    }
}

#[inline]
pub fn write_i32(out: &mut Vec<u8>, v: i32) {
    let mut buf = itoa::Buffer::new();
    out.extend_from_slice(buf.format(v).as_bytes());
}

// silence unused import warning if Write not used elsewhere
#[allow(dead_code)]
fn _write_trait_use(w: &mut Vec<u8>) -> std::io::Result<()> {
    w.write_all(b"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn col_letters_basic() {
        let mut buf = [0u8; 4];
        assert_eq!(col_letters(1, &mut buf), b"A");
        assert_eq!(col_letters(26, &mut buf), b"Z");
        assert_eq!(col_letters(27, &mut buf), b"AA");
        assert_eq!(col_letters(28, &mut buf), b"AB");
    }

    #[test]
    fn write_coord_a1() {
        let mut out = Vec::new();
        write_coord(&mut out, 1, 1);
        assert_eq!(&out, b"A1");
        out.clear();
        write_coord(&mut out, 10, 27);
        assert_eq!(&out, b"AA10");
    }

    #[test]
    fn escape_amp() {
        assert_eq!(escape_text("a&b<c>"), "a&amp;b&lt;c&gt;");
        assert_eq!(escape_text("plain"), "plain");
    }

    #[test]
    fn preserve_ws() {
        assert!(needs_preserve(" hi"));
        assert!(needs_preserve("hi "));
        assert!(!needs_preserve("hi"));
    }

    #[test]
    fn dimension() {
        assert_eq!(dimension_ref(1, 1, 3, 2), "A1:B3");
    }
}
