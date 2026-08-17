// calc/refs.rs — A1 reference parsing and emission (spec `01_parser_reference.md` §5).
//
// Builds `RefCore` AST nodes (`ast.rs`) from the cartesian part of an A1
// reference — no sheet qualifier, no leading `=`, no table/name parts:
//
//   A1 | $A$1 | $A1 | A$1   -> RefCore::Cell   (0-based col/row + `$` flags)
//   A1:B5                    -> RefCore::Range  (normalised start<=end on both axes)
//   3:7                      -> RefCore::Row    (0-based inclusive, normalised)
//   C:E                      -> RefCore::Column (0-based inclusive, normalised)
//
// Coordinates are checked against the Excel grid (`XFD` = 16384 columns,
// `1048576` rows). A syntactically valid reference that falls outside the grid
// is `RefParse::RefError` (#REF!) — never a wrapped or clamped coordinate.
// Emission reproduces the A1 spelling, round-tripping `$` flags for cells.
// `RowRef`/`ColumnRef` carry no absolute flags (`ast.rs`), so `$1:$3` / `$A:$C`
// parse but emit without `$`; the marker only affects shared-formula shifting
// (`formula.rs`), never the set of cells referenced.

use crate::turbo::calc::ast::{CellRef, ColumnRef, RangeRef, RefCore, RowRef};
use crate::turbo::formula::{index_to_letters, letters_to_index};

/// Column `XFD` (1-based). `u32` to match `letters_to_index`'s return type.
const MAX_COL1: u32 = 16_384;
/// Row `1048576` (1-based).
const MAX_ROW1: u64 = 1_048_576;

/// Outcome of parsing a bare A1 reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefParse {
    /// Parsed to an in-grid reference.
    Ref(RefCore),
    /// Syntactically a reference but outside the Excel grid -> `#REF!`.
    RefError,
    /// Not an A1 reference at all; the caller falls through to the next
    /// factory (defined name, table reference, function...).
    NotRef,
}

/// Parse a bare A1 reference (no sheet qualifier, no leading `=`).
pub fn parse_a1(s: &str) -> RefParse {
    let s = s.trim();
    match s.split_once(':') {
        Some((l, r)) => parse_range(l.trim(), r.trim()),
        None => match parse_cell(s) {
            CellOutcome::Cell(c) => RefParse::Ref(RefCore::Cell(c)),
            CellOutcome::OutOfRange => RefParse::RefError,
            CellOutcome::NotCell => RefParse::NotRef,
        },
    }
}

/// Emit one cell as A1 text, preserving `$` flags.
pub fn cell_to_a1(c: &CellRef) -> String {
    let mut s = String::new();
    if c.abs_col {
        s.push('$');
    }
    s.push_str(&index_to_letters(c.col as u32 + 1));
    if c.abs_row {
        s.push('$');
    }
    s.push_str(&(c.row + 1).to_string());
    s
}

/// Emit a range as `A1:B5`.
// The A1 formatters are the inverse of the parser in this module and are kept
// as a set: a reference model that can parse A1 but not print it is only half a
// model, and the printing half is what makes a failing parse test readable.
#[allow(dead_code)]
pub fn range_to_a1(r: &RangeRef) -> String {
    let mut s = cell_to_a1(&r.start);
    s.push(':');
    s.push_str(&cell_to_a1(&r.end));
    s
}

/// Emit a whole-row range as `3:7`.
#[allow(dead_code)]
pub fn row_to_a1(r: &RowRef) -> String {
    let mut s = (r.start + 1).to_string();
    s.push(':');
    s.push_str(&(r.end + 1).to_string());
    s
}

/// Emit a whole-column range as `C:E`.
#[allow(dead_code)]
pub fn col_to_a1(c: &ColumnRef) -> String {
    let mut s = index_to_letters(c.start as u32 + 1);
    s.push(':');
    s.push_str(&index_to_letters(c.end as u32 + 1));
    s
}

/// Emit any `RefCore` back to A1 text.
#[allow(dead_code)]
pub fn core_to_a1(r: &RefCore) -> String {
    match r {
        RefCore::Cell(c) => cell_to_a1(c),
        RefCore::Range(r) => range_to_a1(r),
        RefCore::Row(r) => row_to_a1(r),
        RefCore::Column(c) => col_to_a1(c),
    }
}

fn parse_range(l: &str, r: &str) -> RefParse {
    match (parse_endpoint(l), parse_endpoint(r)) {
        (End::OutOfRange, _) | (_, End::OutOfRange) => RefParse::RefError,
        (End::Cell(a), End::Cell(b)) => RefParse::Ref(RefCore::Range(normalise_range(a, b))),
        (End::Row(a), End::Row(b)) => RefParse::Ref(RefCore::Row(normalise_rows(a, b))),
        (End::Col(a), End::Col(b)) => RefParse::Ref(RefCore::Column(normalise_cols(a, b))),
        _ => RefParse::NotRef,
    }
}

/// One endpoint of a range (`A1`, `$B$2`, `3`, `$7`, `C`, `$E`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum End {
    Cell(CellRef),
    Row(u32),
    Col(u16),
    OutOfRange,
    None,
}

/// Classify one side of a range.
fn parse_endpoint(s: &str) -> End {
    let b = s.as_bytes();
    let mut i = 0;
    let col_abs = b.get(i) == Some(&b'$');
    if col_abs {
        i += 1;
    }
    let lstart = i;
    while b.get(i).is_some_and(|c| c.is_ascii_alphabetic()) {
        i += 1;
    }
    let letters = &s[lstart..i];
    let row_abs = b.get(i) == Some(&b'$');
    if row_abs {
        i += 1;
    }
    let dstart = i;
    while b.get(i).is_some_and(|c| c.is_ascii_digit()) {
        i += 1;
    }
    let digits = &s[dstart..i];
    if i != b.len() {
        return End::None;
    }
    match (letters.is_empty(), digits.is_empty()) {
        (false, false) => {
            let col1 = match letters_to_index(letters.as_bytes()) {
                Some(v) => v,
                None => return End::None,
            };
            let row1 = match parse_row_digits(digits) {
                Some(v) => v,
                None => return End::None,
            };
            if col1 > MAX_COL1 || row1 > MAX_ROW1 {
                return End::OutOfRange;
            }
            End::Cell(CellRef {
                col: (col1 - 1) as u16,
                row: (row1 - 1) as u32,
                abs_col: col_abs,
                abs_row: row_abs,
            })
        }
        (false, true) => {
            if row_abs {
                // `$C$` — a `$` where a row should be; not a valid column piece.
                return End::None;
            }
            let col1 = match letters_to_index(letters.as_bytes()) {
                Some(v) => v,
                None => return End::None,
            };
            if col1 > MAX_COL1 {
                return End::OutOfRange;
            }
            End::Col((col1 - 1) as u16)
        }
        (true, false) => {
            let row1 = match parse_row_digits(digits) {
                Some(v) => v,
                None => return End::None,
            };
            if row1 > MAX_ROW1 {
                return End::OutOfRange;
            }
            End::Row((row1 - 1) as u32)
        }
        (true, true) => End::None,
    }
}

/// A full cell reference `$?COL$?ROW` (whole string, or not a cell).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CellOutcome {
    Cell(CellRef),
    OutOfRange,
    NotCell,
}

/// Parse a standalone cell reference (bare row/column tokens are not cells —
/// a bare row is a number literal, a bare column is a defined-name candidate).
fn parse_cell(s: &str) -> CellOutcome {
    let b = s.as_bytes();
    let mut i = 0;
    let abs_col = b.get(i) == Some(&b'$');
    if abs_col {
        i += 1;
    }
    let lstart = i;
    while b.get(i).is_some_and(|c| c.is_ascii_alphabetic()) {
        i += 1;
    }
    let letters = &s[lstart..i];
    let abs_row = b.get(i) == Some(&b'$');
    if abs_row {
        i += 1;
    }
    let dstart = i;
    while b.get(i).is_some_and(|c| c.is_ascii_digit()) {
        i += 1;
    }
    let digits = &s[dstart..i];
    if i != b.len() || letters.is_empty() || digits.is_empty() {
        return CellOutcome::NotCell;
    }
    let col1 = match letters_to_index(letters.as_bytes()) {
        Some(v) => v,
        None => return CellOutcome::NotCell,
    };
    let row1 = match parse_row_digits(digits) {
        Some(v) => v,
        None => return CellOutcome::NotCell,
    };
    if col1 > MAX_COL1 || row1 > MAX_ROW1 {
        return CellOutcome::OutOfRange;
    }
    CellOutcome::Cell(CellRef {
        col: (col1 - 1) as u16,
        row: (row1 - 1) as u32,
        abs_col,
        abs_row,
    })
}

/// Row number `[1-9][0-9]*` -> 1-based `u64`, or `None` on shape/overflow.
fn parse_row_digits(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.first().is_none_or(|c| !(b'1'..=b'9').contains(c)) {
        return None;
    }
    s.parse::<u64>().ok()
}

/// Normalise a cell range so start <= end on both axes. The `$` flags of the
/// endpoint that contributes each edge coordinate are preserved.
fn normalise_range(a: CellRef, b: CellRef) -> RangeRef {
    let (start_col, end_col, sc_abs, ec_abs) = if a.col <= b.col {
        (a.col, b.col, a.abs_col, b.abs_col)
    } else {
        (b.col, a.col, b.abs_col, a.abs_col)
    };
    let (start_row, end_row, sr_abs, er_abs) = if a.row <= b.row {
        (a.row, b.row, a.abs_row, b.abs_row)
    } else {
        (b.row, a.row, b.abs_row, a.abs_row)
    };
    RangeRef {
        start: CellRef {
            col: start_col,
            row: start_row,
            abs_col: sc_abs,
            abs_row: sr_abs,
        },
        end: CellRef {
            col: end_col,
            row: end_row,
            abs_col: ec_abs,
            abs_row: er_abs,
        },
    }
}

fn normalise_rows(a: u32, b: u32) -> RowRef {
    RowRef {
        start: a.min(b),
        end: a.max(b),
    }
}

fn normalise_cols(a: u16, b: u16) -> ColumnRef {
    ColumnRef {
        start: a.min(b),
        end: a.max(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn cell(col: u16, row: u32, abs_col: bool, abs_row: bool) -> CellRef {
        CellRef {
            col,
            row,
            abs_col,
            abs_row,
        }
    }

    fn expect_cell(s: &str) -> CellRef {
        match parse_a1(s) {
            RefParse::Ref(RefCore::Cell(c)) => c,
            other => panic!("expected Ref::Cell for {s:?}, got {other:?}"),
        }
    }

    #[test]
    fn cell_round_trip_all_abs_combos() {
        for (text, col, row, ac, ar) in [
            ("A1", 0u16, 0u32, false, false),
            ("$A$1", 0u16, 0u32, true, true),
            ("$A1", 0u16, 0u32, true, false),
            ("A$1", 0u16, 0u32, false, true),
            ("BZ$250", 77u16, 249u32, false, true),
            ("$XFD$1048576", 16383u16, 1048575u32, true, true),
        ] {
            let c = expect_cell(text);
            assert_eq!(
                (c.col, c.row, c.abs_col, c.abs_row),
                (col, row, ac, ar),
                "{text}"
            );
            assert_eq!(cell_to_a1(&c), text, "emission {text}");
        }
    }

    #[test]
    fn max_corner_accepted() {
        let c = expect_cell("XFD1048576");
        assert_eq!((c.col, c.row), (16383, 1048575));
        assert_eq!(cell_to_a1(&c), "XFD1048576");
        // lower-case input accepted, emitted uppercase
        let c = expect_cell("xfd1048576");
        assert_eq!(cell_to_a1(&c), "XFD1048576");
    }

    #[test]
    fn out_of_grid_is_ref() {
        for s in [
            "XFE1",
            "A1048577",
            "XFD1048577",
            "XFE1048576",
            "$XFE$1",
            "A1:XFE1",
            "XFE1:ABC1",
            "A1048577:A1048578",
            "XFE:XFG",
            "A:XFE",
            "1048577:1048578",
            "1:1048577",
        ] {
            assert_eq!(parse_a1(s), RefParse::RefError, "{s}");
        }
    }

    #[test]
    fn ranges_parse_and_normalise() {
        let r = match parse_a1("B5:A1") {
            RefParse::Ref(RefCore::Range(r)) => r,
            other => panic!("{other:?}"),
        };
        assert_eq!(r.start, cell(0, 0, false, false));
        assert_eq!(r.end, cell(1, 4, false, false));
        assert_eq!(range_to_a1(&r), "A1:B5");

        // per-edge `$` flags survive normalisation
        let r = match parse_a1("$B$5:A1") {
            RefParse::Ref(RefCore::Range(r)) => r,
            other => panic!("{other:?}"),
        };
        assert_eq!(range_to_a1(&r), "A1:$B$5");

        // already-canonical order round-trips with all flags
        let r = match parse_a1("$A$1:$B$5") {
            RefParse::Ref(RefCore::Range(r)) => r,
            other => panic!("{other:?}"),
        };
        assert_eq!(range_to_a1(&r), "$A$1:$B$5");

        // degenerate range is a valid single-cell range
        let r = match parse_a1("C3:C3") {
            RefParse::Ref(RefCore::Range(r)) => r,
            other => panic!("{other:?}"),
        };
        assert_eq!(range_to_a1(&r), "C3:C3");
    }

    #[test]
    fn whole_row_and_column() {
        for (text, start, end) in [
            ("3:7", 2u32, 6u32),
            ("7:3", 2u32, 6u32),
            ("3:3", 2u32, 2u32),
            ("$1:$4", 0u32, 3u32),
        ] {
            let r = match parse_a1(text) {
                RefParse::Ref(RefCore::Row(r)) => r,
                other => panic!("{text}: {other:?}"),
            };
            assert_eq!((r.start, r.end), (start, end), "{text}");
        }
        let r = match parse_a1("3:7") {
            RefParse::Ref(RefCore::Row(r)) => r,
            other => panic!("{other:?}"),
        };
        assert_eq!(row_to_a1(&r), "3:7");

        for (text, start, end) in [
            ("C:E", 2u16, 4u16),
            ("E:C", 2u16, 4u16),
            ("C:C", 2u16, 2u16),
            ("A:XFD", 0u16, 16383u16),
        ] {
            let c = match parse_a1(text) {
                RefParse::Ref(RefCore::Column(c)) => c,
                other => panic!("{text}: {other:?}"),
            };
            assert_eq!((c.start, c.end), (start, end), "{text}");
        }
        let c = match parse_a1("C:E") {
            RefParse::Ref(RefCore::Column(c)) => c,
            other => panic!("{other:?}"),
        };
        assert_eq!(col_to_a1(&c), "C:E");
    }

    #[test]
    fn non_references_fall_through() {
        for s in [
            "", "3",       // bare row is a number literal
            "C",       // bare col is a defined-name candidate
            "XFE",     // bare col, out of grid but still a name candidate
            "1048577", // number literal
            "AAAA1",   // four-letter column
            "A0",      // row cannot start with 0
            "A1B",     // trailing junk
            "A:B5",    // mixed endpoint kinds
            "A1:B",    // mixed endpoint kinds
            "$",       // lone dollar
        ] {
            assert_eq!(parse_a1(s), RefParse::NotRef, "{s:?}");
        }
    }

    #[test]
    fn core_emission_covers_all_variants() {
        assert_eq!(core_to_a1(&RefCore::Cell(cell(2, 3, true, false))), "$C4");
        assert_eq!(
            core_to_a1(&RefCore::Row(RowRef { start: 4, end: 9 })),
            "5:10"
        );
        assert_eq!(
            core_to_a1(&RefCore::Column(ColumnRef { start: 1, end: 4 })),
            "B:E"
        );
        assert_eq!(
            core_to_a1(&RefCore::Range(RangeRef {
                start: cell(0, 0, false, false),
                end: cell(1, 4, false, false),
            })),
            "A1:B5"
        );
    }
}
