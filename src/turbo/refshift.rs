// refshift.rs — formula reference shifting for row/column insert and delete.
//
// When rows or columns are inserted or deleted, every formula that references the
// moved region must have its references shifted, or the workbook is silently wrong.
// This is the P2 + Cow byte scan from PERF_EXPERIMENTS.md E3: one forward pass, copy
// clean runs, rewrite only the digits (or letters) that move. String literals are
// opaque, `$`-absolute components are pinned, and a delete that pushes an index below
// 1 destroys the reference into `#REF!`.
//
// This is a DIFFERENT problem from `formula.rs::translate_body`, which moves a formula
// to a new anchor cell. Here the formula stays put and the GRID moves under it.

use std::borrow::Cow;

use super::formula::{index_to_letters, letters_to_index};

/// The grid axis along which references shift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Row,
    Col,
}

/// Shift references in `formula` because the grid on `axis` at index `at` moved by
/// `delta` (negative delta is a delete). A reference whose index on `axis` is `>= at`
/// moves by `delta`; an absolute `$` pins that component; a delete pushing an index
/// below 1 turns the whole reference into `#REF!`. Returns `Cow::Borrowed` when
/// nothing changed (the common case for a sheet whose formulas do not point into the
/// shifted region).
pub fn shift_refs<'a>(formula: &'a str, axis: Axis, at: u32, delta: i64) -> Cow<'a, str> {
    let f = formula.as_bytes();
    let n = f.len();
    let mut out: Option<String> = None;
    let mut run = 0usize;
    let mut i = 0usize;

    while i < n {
        let c = f[i];
        // String literal: opaque span. A doubled "" is an escaped quote and does not
        // close the literal.
        if c == b'"' {
            i += skip_dquote(f, i);
            continue;
        }
        // Quoted sheet name ('My Sheet'!B5): opaque span, doubled '' likewise, then
        // continue into the reference after the '!'.
        if c == b'\'' {
            i += skip_squote(f, i);
            continue;
        }
        // A reference may only start at '$' or an ASCII letter, and only when the
        // preceding byte is not part of an identifier (not alphanumeric, '_', '.').
        // This stops LOG10(A5) being read as a reference named LOG followed by row 10.
        let prev_ident =
            i > 0 && (f[i - 1].is_ascii_alphanumeric() || f[i - 1] == b'_' || f[i - 1] == b'.');
        if (c == b'$' || c.is_ascii_alphabetic()) && !prev_ident {
            let start = i;
            let mut p = i;
            let abs_col = f[p] == b'$';
            if abs_col {
                p += 1;
            }
            let cs = p;
            while p < n && f[p].is_ascii_alphabetic() {
                p += 1;
            }
            let le = p; // letters end
            let abs_row = p < n && f[p] == b'$';
            if abs_row {
                p += 1;
            }
            let rs = p;
            while p < n && f[p].is_ascii_digit() {
                p += 1;
            }
            // It is only a reference if 1..=3 letters, 1+ digits, and the following
            // byte is not '(' (function call) and not a letter or '_'.
            let is_ref = (1..=3).contains(&(le - cs))
                && p > rs
                && !(p < n && f[p] == b'(')
                && !(p < n && (f[p].is_ascii_alphabetic() || f[p] == b'_'));
            if is_ref {
                // Decide whether the component on `axis` moves. An absolute marker
                // pins that component: $A$2 never moves, A$2 moves on the column axis
                // only, $A2 moves on the row axis only.
                let repl: Option<Vec<u8>> = match axis {
                    Axis::Row => {
                        if abs_row {
                            None
                        } else {
                            let row: u32 = std::str::from_utf8(&f[rs..p])
                                .unwrap_or("0")
                                .parse()
                                .unwrap_or(0);
                            if row >= at {
                                let nr = row as i64 + delta;
                                if nr < 1 {
                                    Some(b"#REF!".to_vec())
                                } else {
                                    let mut v = f[start..rs].to_vec();
                                    v.extend_from_slice(nr.to_string().as_bytes());
                                    Some(v)
                                }
                            } else {
                                None
                            }
                        }
                    }
                    Axis::Col => {
                        if abs_col {
                            None
                        } else if let Some(idx) = letters_to_index(&f[cs..le]) {
                            if idx < at {
                                None
                            } else {
                                let ni = idx as i64 + delta;
                                if ni < 1 {
                                    Some(b"#REF!".to_vec())
                                } else {
                                    let mut v = index_to_letters(ni as u32).into_bytes();
                                    v.extend_from_slice(&f[le..p]);
                                    Some(v)
                                }
                            }
                        } else {
                            None
                        }
                    }
                };
                if let Some(repl) = repl {
                    let repl_s = String::from_utf8(repl).unwrap_or_else(|_| String::from("#REF!"));
                    match &mut out {
                        None => {
                            let mut s = String::with_capacity(formula.len() + 8);
                            s.push_str(&formula[run..start]);
                            s.push_str(&repl_s);
                            out = Some(s);
                        }
                        Some(s) => {
                            s.push_str(&formula[run..start]);
                            s.push_str(&repl_s);
                        }
                    }
                    run = p;
                }
                i = p;
                continue;
            }
        }
        i += 1;
    }

    match out {
        Some(mut s) => {
            s.push_str(&formula[run..]);
            Cow::Owned(s)
        }
        None => Cow::Borrowed(formula),
    }
}

/// Bytes consumed starting at the opening quote of a `"..."` literal, treating a
/// doubled `""` inside as an escaped quote.
#[inline]
fn skip_dquote(f: &[u8], i: usize) -> usize {
    let mut j = i + 1;
    while j < f.len() {
        if f[j] == b'"' {
            if j + 1 < f.len() && f[j + 1] == b'"' {
                j += 2;
                continue;
            }
            return j + 1 - i;
        }
        j += 1;
    }
    f.len() - i
}

/// Bytes consumed starting at the opening quote of a `'...'` sheet name, treating a
/// doubled `''` inside as an escaped quote.
#[inline]
fn skip_squote(f: &[u8], i: usize) -> usize {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    fn row(s: &str, at: u32, delta: i64) -> Cow<'_, str> {
        shift_refs(s, Axis::Row, at, delta)
    }
    fn col(s: &str, at: u32, delta: i64) -> Cow<'_, str> {
        shift_refs(s, Axis::Col, at, delta)
    }

    // The six trap cases from PERF_EXPERIMENTS.md E3, verbatim (insert 2 rows at 3).
    #[test]
    fn trap_cases_verbatim() {
        assert_eq!(row("SUM(A1:A10)", 3, 2), "SUM(A1:A12)");
        assert_eq!(
            row("VLOOKUP($A$2,$B$1:$D$99,3,FALSE)", 3, 2),
            "VLOOKUP($A$2,$B$1:$D$99,3,FALSE)"
        );
        assert_eq!(row("B1+B2", 3, 2), "B1+B2");
        assert_eq!(
            row("IF(C7>0,D7,\"A1 not found\")", 3, 2),
            "IF(C9>0,D9,\"A1 not found\")"
        );
        assert_eq!(row("LOG10(A5)", 3, 2), "LOG10(A7)");
        assert_eq!(row("Sheet2!B5*2", 3, 2), "Sheet2!B7*2");
    }

    // Column axis, including a Z -> AA letter-width change.
    #[test]
    fn col_axis_width_change() {
        assert_eq!(col("Z1+AA2", 26, 1), "AA1+AB2");
        assert_eq!(col("AZ9", 26, 1), "BA9");
        assert_eq!(col("A1", 1, 1), "B1");
        assert_eq!(col("LOG10(B5)", 2, 1), "LOG10(C5)");
    }

    // A delete shifts rows (or columns) up; refs above the point stay put.
    #[test]
    fn delete_shifts_up() {
        assert_eq!(row("SUM(A5:A10)", 3, -2), "SUM(A3:A8)");
        assert_eq!(row("A2+B2", 3, -2), "A2+B2");
        assert_eq!(col("SUM(C5:C10)", 3, -2), "SUM(A5:A10)");
    }

    // A delete that pushes an index below 1 destroys the reference into #REF!.
    #[test]
    fn delete_destroys_to_ref() {
        assert_eq!(row("A2", 2, -3), "#REF!");
        assert_eq!(row("B1", 1, -1), "#REF!");
        assert_eq!(col("B1", 2, -2), "#REF!");
        assert_eq!(row("SUM(A1:A2)", 1, -1), "SUM(#REF!:A1)");
    }

    // Mixed absolute markers on both axes.
    #[test]
    fn mixed_absolute_pins() {
        assert_eq!(row("A$5+$A5+$A$2+B5", 3, 2), "A$5+$A7+$A$2+B7");
        assert_eq!(col("A$5+$A5+$A$2+B5", 1, 1), "B$5+$A5+$A$2+C5");
    }

    // A quoted sheet name with a space shifts its cell reference.
    #[test]
    fn quoted_sheet_name_with_space() {
        assert_eq!(row("'My Sheet'!B5", 3, 2), "'My Sheet'!B7");
    }

    // A doubled "" inside a string literal is an escaped quote, not a closer.
    #[test]
    fn doubled_quote_in_literal_is_opaque() {
        assert_eq!(
            row("IF(A5=\"say \"\"hi\"\"\",B5,0)", 3, 2),
            "IF(A7=\"say \"\"hi\"\"\",B7,0)"
        );
    }

    // A formula with no shiftable reference returns Cow::Borrowed.
    #[test]
    fn no_change_returns_borrowed() {
        assert!(matches!(row("B1+B2", 3, 2), Cow::Borrowed(_)));
        assert!(matches!(
            row("SUM($A$2,$B$1:$D$99,3,FALSE)", 3, 2),
            Cow::Borrowed(_)
        ));
        assert!(matches!(row("\"A1 not found\"&1", 3, 2), Cow::Borrowed(_)));
        assert!(matches!(col("A1+B1", 5, 2), Cow::Borrowed(_)));
    }

    #[test]
    fn coordinator_adversarial() {
        // A sheet named like a cell reference. Excel permits sheet names such as Q1.
        assert_eq!(row("Q1!A5", 3, 2), "Q1!A7");
        assert_eq!(row("'Q1'!A5", 3, 2), "'Q1'!A7");
        // Defined names must never shift.
        assert_eq!(row("SUM(MyRange)", 3, 2), "SUM(MyRange)");
        assert_eq!(row("TaxRate2*A5", 3, 2), "TaxRate2*A7");
        // Numeric literals adjacent to refs.
        assert_eq!(row("1.5*A5", 3, 2), "1.5*A7");
        assert_eq!(row("A5*1E5", 3, 2), "A7*1E5");
        // Ref at the very start and very end of the string.
        assert_eq!(row("A5", 3, 2), "A7");
        assert_eq!(row("SUM(B1,A5)", 3, 2), "SUM(B1,A7)");
        // Grid boundary.
        assert_eq!(row("XFD1048574", 3, 2), "XFD1048576");
        // Booleans and error literals are not refs.
        assert_eq!(row("IF(A5,TRUE,FALSE)", 3, 2), "IF(A7,TRUE,FALSE)");
        assert_eq!(row("IFERROR(A5,#N/A)", 3, 2), "IFERROR(A7,#N/A)");
        // Table structured reference must be left alone.
        assert_eq!(row("SUM(Table1[Amount])", 3, 2), "SUM(Table1[Amount])");
    }
}
