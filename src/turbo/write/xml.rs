//! Fast XML helpers: escape, column letters, number formatting.
//!
//! Duplicates small utilities also present on the read path (entity escape,
//! A1/col-letter). Dedup deferred — see W1_REPORT.md.
//!
//! Escaping and illegal-control-char stripping are fused into one SWAR pass
//! (PERF_EXPERIMENTS.md E1, candidate D): a two-pass fix was measured
//! 1.75–2.32x slower than shipping, this fused scan is ~2x faster.

/// Column index 1-based → Excel letters (1=A, 26=Z, 27=AA).
#[inline]
pub fn col_letters(mut col: u32, buf: &mut [u8; 4]) -> &[u8] {
    debug_assert!((1..=16384).contains(&col));
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

const LO: u64 = 0x0101_0101_0101_0101;
const HI: u64 = 0x8080_8080_8080_8080;

/// High-bit mask over the lanes of `w` whose byte is < 0x20 (control candidate).
#[inline]
fn word_lt20(w: u64) -> u64 {
    w.wrapping_sub(0x2020_2020_2020_2020) & !w & HI
}

/// High-bit mask over the lanes of `w` whose byte equals `c`.
#[inline]
fn word_eq(w: u64, c: u8) -> u64 {
    let x = w ^ LO.wrapping_mul(c as u64);
    x.wrapping_sub(LO) & !x & HI
}

/// True if any of the 8 bytes needs the per-byte scan: a control char, or one
/// of the escape characters (plus `"` for the attribute variant).
#[inline]
fn word_dirty(w: u64, esc_quote: bool) -> bool {
    let mut mask = word_lt20(w) | word_eq(w, b'&') | word_eq(w, b'<') | word_eq(w, b'>');
    if esc_quote {
        mask |= word_eq(w, b'"');
    }
    mask != 0
}

/// True when `b` needs handling: `& < >` (plus `"` for attrs), or an illegal
/// control char. Tab (9), LF (10) and CR (13) are legal and never flagged.
#[inline]
fn byte_special(b: u8, esc_quote: bool) -> bool {
    (esc_quote && b == b'"')
        || b == b'&'
        || b == b'<'
        || b == b'>'
        || (b < 0x20 && b != 0x09 && b != 0x0A && b != 0x0D)
}

/// Emit the replacement for a special byte; illegal control chars emit nothing
/// (dropped). Legal control chars never reach here.
#[inline]
fn emit_replacement(out: &mut Vec<u8>, b: u8, esc_quote: bool) {
    match b {
        b'&' => out.extend_from_slice(b"&amp;"),
        b'<' => out.extend_from_slice(b"&lt;"),
        b'>' => out.extend_from_slice(b"&gt;"),
        b'"' if esc_quote => out.extend_from_slice(b"&quot;"),
        _ => {} // illegal control character: dropped
    }
}

/// Fused escape + illegal-char strip (PERF_EXPERIMENTS.md E1, candidate D).
///
/// Walks the input in 8-byte words via a SWAR prescan; clean words advance 8
/// bytes with no per-byte work. Only dirty words are scanned individually, and
/// clean runs between specials are copied with one `extend_from_slice` each.
///
/// UTF-8 safe: every byte of a multi-byte sequence is >= 0x80, so it can never
/// match a control char or an ASCII special and passes through untouched.
fn write_escaped(out: &mut Vec<u8>, s: &str, esc_quote: bool) {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut i = 0usize;
    let mut run_start = 0usize;
    while i + 8 <= n {
        let w = u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
        if !word_dirty(w, esc_quote) {
            i += 8;
            continue;
        }
        for j in i..i + 8 {
            let b = bytes[j];
            if byte_special(b, esc_quote) {
                if j > run_start {
                    out.extend_from_slice(&bytes[run_start..j]);
                }
                emit_replacement(out, b, esc_quote);
                run_start = j + 1;
            }
        }
        i += 8;
    }
    while i < n {
        let b = bytes[i];
        if byte_special(b, esc_quote) {
            if i > run_start {
                out.extend_from_slice(&bytes[run_start..i]);
            }
            emit_replacement(out, b, esc_quote);
            run_start = i + 1;
        }
        i += 1;
    }
    if run_start < n {
        out.extend_from_slice(&bytes[run_start..]);
    }
}

/// Escape `& < >` for XML text content, stripping illegal control chars.
pub fn escape_text(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len() + 8);
    write_escaped_text(&mut out, s);
    // Output differs from the input only in ASCII replacements and dropped
    // single-byte controls, so it is still valid UTF-8.
    unsafe { String::from_utf8_unchecked(out) }
}

/// Write escaped text directly into buffer, stripping illegal control chars.
pub fn write_escaped_text(out: &mut Vec<u8>, s: &str) {
    write_escaped(out, s, false)
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

#[inline]
pub fn push(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(b);
}

#[inline]
pub fn push_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
}

/// Escape `& < > "` for XML attribute values, stripping illegal control chars.
#[inline]
pub fn write_escaped_attr(out: &mut Vec<u8>, s: &str) {
    write_escaped(out, s, true)
}

#[inline]
pub fn write_i32(out: &mut Vec<u8>, v: i32) {
    let mut buf = itoa::Buffer::new();
    out.extend_from_slice(buf.format(v).as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

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

    // ---------- T0-A: fused escape + illegal-char strip ----------

    /// The pre-fusion text escaper (no control-char handling), for proving
    /// byte-identical output on inputs without illegal controls.
    fn old_escaped_text(out: &mut Vec<u8>, s: &str) {
        for b in s.bytes() {
            match b {
                b'&' => out.extend_from_slice(b"&amp;"),
                b'<' => out.extend_from_slice(b"&lt;"),
                b'>' => out.extend_from_slice(b"&gt;"),
                _ => out.push(b),
            }
        }
    }

    /// The pre-fusion attr escaper.
    fn old_escaped_attr(out: &mut Vec<u8>, s: &str) {
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

    #[test]
    fn clean_ascii_round_trips_byte_identically() {
        let samples = [
            "",
            "plain",
            "Customer Account 42",
            "Line item description from the warehouse",
            "abc123_-. !",
            "with\ttab and\nnewline and\rcarriage",
        ];
        for s in samples {
            let mut a = Vec::new();
            let mut b = Vec::new();
            write_escaped_text(&mut a, s);
            old_escaped_text(&mut b, s);
            assert_eq!(a, b, "text {s:?}");
            assert_eq!(a, s.as_bytes(), "text identity {s:?}");
            let mut a = Vec::new();
            let mut b = Vec::new();
            write_escaped_attr(&mut a, s);
            old_escaped_attr(&mut b, s);
            assert_eq!(a, b, "attr {s:?}");
            assert_eq!(a, s.as_bytes(), "attr identity {s:?}");
            assert_eq!(escape_text(s), s, "escape_text {s:?}");
        }
    }

    #[test]
    fn escapes_match_previous_escaper() {
        let samples = [
            "&",
            "<",
            ">",
            "<a&b>",
            "&&<>",
            "&at both ends<",
            "end&",
            "<all>",
            "R&D <dept 7> margin > 10%",
        ];
        for s in samples {
            let mut a = Vec::new();
            let mut b = Vec::new();
            write_escaped_text(&mut a, s);
            old_escaped_text(&mut b, s);
            assert_eq!(a, b, "text {s:?}");
            let mut a = Vec::new();
            let mut b = Vec::new();
            write_escaped_attr(&mut a, s);
            old_escaped_attr(&mut b, s);
            assert_eq!(a, b, "attr {s:?}");
        }
    }

    #[test]
    fn escapes_text_specials() {
        let cases: &[(&str, &str)] = &[
            ("&", "&amp;"),
            ("<", "&lt;"),
            (">", "&gt;"),
            ("<a&b>", "&lt;a&amp;b&gt;"),
            ("&&<<>>", "&amp;&amp;&lt;&lt;&gt;&gt;"),
            ("&at both ends<", "&amp;at both ends&lt;"),
            ("end&", "end&amp;"),
            ("<all>", "&lt;all&gt;"),
        ];
        for (input, expected) in cases {
            let mut out = Vec::new();
            write_escaped_text(&mut out, input);
            assert_eq!(out, expected.as_bytes(), "write_escaped_text({input:?})");
            assert_eq!(escape_text(input), *expected, "escape_text({input:?})");
        }
    }

    #[test]
    fn escapes_attr_specials() {
        let cases: &[(&str, &str)] = &[
            ("\"", "&quot;"),
            ("\"a\"\"b\"", "&quot;a&quot;&quot;b&quot;"),
            ("<\"&>", "&lt;&quot;&amp;&gt;"),
            ("\"at both ends\"", "&quot;at both ends&quot;"),
            ("end\"", "end&quot;"),
            ("&&\"\"<>", "&amp;&amp;&quot;&quot;&lt;&gt;"),
        ];
        for (input, expected) in cases {
            let mut out = Vec::new();
            write_escaped_attr(&mut out, input);
            assert_eq!(out, expected.as_bytes(), "write_escaped_attr({input:?})");
        }
    }

    #[test]
    fn word_boundaries_and_tail() {
        for n in [8usize, 9, 15, 16, 17] {
            let clean = "a".repeat(n);
            let mut out = Vec::new();
            write_escaped_text(&mut out, &clean);
            assert_eq!(out, clean.as_bytes(), "clean len {n}");

            let mut s = "a".repeat(n - 1);
            s.push('&');
            let expected = format!("{}&amp;", "a".repeat(n - 1));
            let mut out = Vec::new();
            write_escaped_text(&mut out, &s);
            assert_eq!(out, expected.as_bytes(), "trailing special len {n}");

            let s = format!("<{}", "a".repeat(n - 1));
            let expected = format!("&lt;{}", "a".repeat(n - 1));
            let mut out = Vec::new();
            write_escaped_text(&mut out, &s);
            assert_eq!(out, expected.as_bytes(), "leading special len {n}");

            let mut s = "a".repeat(n - 1);
            s.push('\u{1F}');
            let mut out = Vec::new();
            write_escaped_text(&mut out, &s);
            assert_eq!(
                out,
                "a".repeat(n - 1).as_bytes(),
                "trailing control len {n}"
            );
        }
    }

    #[test]
    fn runs_span_word_boundaries() {
        // '&' at the last byte of one word, '<' at the first byte of the next.
        let s = "aaaaaaa&<aaaaaaa";
        let mut out = Vec::new();
        write_escaped_text(&mut out, s);
        assert_eq!(out, b"aaaaaaa&amp;&lt;aaaaaaa");

        // a control byte at a word boundary is dropped, neighbours kept.
        let s = "abcdefgh\u{1}ijklmnop";
        let mut out = Vec::new();
        write_escaped_text(&mut out, s);
        assert_eq!(out, b"abcdefghijklmnop");
    }

    #[test]
    fn illegal_controls_dropped() {
        let input = "\u{0}a\u{8}b\u{B}c\u{C}d\u{1F}e";
        let mut out = Vec::new();
        write_escaped_text(&mut out, input);
        assert_eq!(out, b"abcde");
        let mut out = Vec::new();
        write_escaped_attr(&mut out, input);
        assert_eq!(out, b"abcde");
        assert_eq!(escape_text(input), "abcde");
    }

    #[test]
    fn legal_controls_survive() {
        let input = "a\tb\nc\rd";
        let mut out = Vec::new();
        write_escaped_text(&mut out, input);
        assert_eq!(out, input.as_bytes());
        let mut out = Vec::new();
        write_escaped_attr(&mut out, input);
        assert_eq!(out, input.as_bytes());
        assert_eq!(escape_text(input), input);
    }

    #[test]
    fn multibyte_utf8_untouched() {
        let samples = [
            "caf\u{e9}",
            "\u{65e5}\u{672c}\u{8a9e}",
            "\u{30a2}\u{30fc}",
            "mo\u{308f}\u{308a}",
            "caf\u{e9} \u{65e5}\u{672c}",
            "\u{e9}\u{30a2}",
        ];
        for s in samples {
            let mut out = Vec::new();
            write_escaped_text(&mut out, s);
            assert_eq!(out, s.as_bytes(), "text {s:?}");
            let mut out = Vec::new();
            write_escaped_attr(&mut out, s);
            assert_eq!(out, s.as_bytes(), "attr {s:?}");
            assert_eq!(escape_text(s), s, "escape_text {s:?}");
        }
        let mixed = "caf\u{e9} & \u{65e5}\u{672c}";
        let mut out = Vec::new();
        write_escaped_text(&mut out, mixed);
        assert_eq!(out, "caf\u{e9} &amp; \u{65e5}\u{672c}".as_bytes());
        let mixed = "<\u{e9}\u{30a2}>";
        let mut out = Vec::new();
        write_escaped_text(&mut out, mixed);
        assert_eq!(out, "&lt;\u{e9}\u{30a2}&gt;".as_bytes());
    }

    #[test]
    fn control_adjacent_to_escape() {
        // control before escape
        let mut out = Vec::new();
        write_escaped_text(&mut out, "\u{1}&x");
        assert_eq!(out, b"&amp;x");
        // escape before control
        let mut out = Vec::new();
        write_escaped_text(&mut out, "x&\u{1}");
        assert_eq!(out, b"x&amp;");
        // control surrounded by escapes
        let mut out = Vec::new();
        write_escaped_text(&mut out, "&\u{1}&");
        assert_eq!(out, b"&amp;&amp;");
        // attr variant, quote then control
        let mut out = Vec::new();
        write_escaped_attr(&mut out, "\"\u{8}a");
        assert_eq!(out, b"&quot;a");
        // attr variant, control then quote
        let mut out = Vec::new();
        write_escaped_attr(&mut out, "a\u{8}\"");
        assert_eq!(out, b"a&quot;");
    }
}
