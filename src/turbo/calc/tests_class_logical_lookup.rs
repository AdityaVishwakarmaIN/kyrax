// tests_class_logical_lookup.rs — the LOGICAL and LOOKUP class matrices.
//
// Every case drives the REAL pipeline: parse_formula then eval (see
// testkit.rs). No function is called directly, so a test exercises the lexer,
// parser, reference resolution, coercion, the registry and the function body
// together. Excel behaviours pinned here on purpose:
//
//   * Text in a condition is #VALUE! — Excel never coerces a string to a
//     boolean. Numbers are true when non-zero; blanks are false.
//   * IF's omitted else branch is FALSE; an error in the condition propagates,
//     an error in a branch that is not taken does not.
//   * IFS evaluates its pairs in order (first match wins, #N/A when nothing
//     matches); an odd argument count is #VALUE!.
//   * AND/OR/XOR ignore text and blanks inside a range but reject a direct
//     scalar text argument; XOR is the parity of the true values.
//   * IFERROR swallows every error; IFNA swallows only #N/A and passes every
//     other error straight through as a value.
//   * SWITCH matches the expression against each value in turn, takes a
//     trailing odd argument as the default, and gives #N/A when nothing
//     matches and no default was supplied.
//   * VLOOKUP/HLOOKUP: exact vs approximate against the same table; a column/
//     row index below 1 is #VALUE!, one beyond the table is #REF! — two
//     distinct errors.
//   * XLOOKUP: exact by default, the if-not-found argument replacing #N/A,
//     next-smaller / next-larger / wildcard match modes; lookup and return
//     arrays of different lengths are #VALUE!.
//   * MATCH is 1-based; type 0 exact (wildcards on text), type 1 ascending,
//     type -1 descending; no match is #N/A.
//   * INDEX is 1-based; a 0 row/column returns the whole column/row as an
//     array; out of range is #REF!.
//   * CHOOSE is 1-based; an index outside the value list is #VALUE!.
//
// Cross-type ordering (number < text < FALSE < TRUE) is asserted only where
// the exact winner is certain; a mixed-type column is otherwise tested only
// for error-ness.
#![cfg(test)]

use super::testkit::{Grid, Outcome, approx, boolean, calc, error, num, text};
use crate::turbo::calc::value::{CalcError, CalcValue};

// -- shared grids ------------------------------------------------------------

/// A two-column key/value table: A = 1,2,3,5 and B = one,two,three,five.
fn kv_table() -> Grid {
    Grid::empty()
        .set_num("A1", 1.0)
        .set_text("B1", "one")
        .set_num("A2", 2.0)
        .set_text("B2", "two")
        .set_num("A3", 3.0)
        .set_text("B3", "three")
        .set_num("A4", 5.0)
        .set_text("B4", "five")
}

/// A row-based table for HLOOKUP: A1:C1 = 1,3,5 and A2:C2 = a,c,e.
fn hlookup_table() -> Grid {
    Grid::empty()
        .set_num("A1", 1.0)
        .set_num("B1", 3.0)
        .set_num("C1", 5.0)
        .set_text("A2", "a")
        .set_text("B2", "c")
        .set_text("C2", "e")
}

/// A 3x3 numeric block, A1:C3 = 1..9 in row-major order.
fn index_grid() -> Grid {
    Grid::empty()
        .set_num("A1", 1.0)
        .set_num("B1", 2.0)
        .set_num("C1", 3.0)
        .set_num("A2", 4.0)
        .set_num("B2", 5.0)
        .set_num("C2", 6.0)
        .set_num("A3", 7.0)
        .set_num("B3", 8.0)
        .set_num("C3", 9.0)
}

/// A text key/value table for text lookups: C = a,b,c and D = 1,2,3.
fn text_table() -> Grid {
    Grid::empty()
        .set_text("C1", "a")
        .set_num("D1", 1.0)
        .set_text("C2", "b")
        .set_num("D2", 2.0)
        .set_text("C3", "c")
        .set_num("D3", 3.0)
}

// ---------------------------------------------------------------------------
// IF
// ---------------------------------------------------------------------------
mod logical_if {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn a_number_is_true_when_non_zero_and_a_blank_is_false() {
        assert_eq!(num("=IF(1, 7, 8)"), 7.0);
        assert_eq!(num("=IF(-3, 7, 8)"), 7.0);
        assert_eq!(num("=IF(0.0001, 7, 8)"), 7.0);
        assert_eq!(num("=IF(0, 7, 8)"), 8.0);
        let g = Grid::empty();
        assert_eq!(g.num("=IF(A1, 7, 8)"), 8.0); // blank cell -> false
        assert!(!g.boolean("=IF(A1, 7)")); // blank -> false, no else -> FALSE
    }

    #[test]
    fn selects_branch_on_truthiness() {
        assert_eq!(num("=IF(TRUE, 7, 8)"), 7.0);
        assert_eq!(num("=IF(FALSE, 7, 8)"), 8.0);
        assert_eq!(text("=IF(TRUE, \"yes\", \"no\")"), "yes");
        assert_eq!(text("=IF(FALSE, \"yes\", \"no\")"), "no");
        assert!(boolean("=IF(TRUE, TRUE, FALSE)"));
        assert!(!boolean("=IF(FALSE, TRUE, FALSE)"));
    }

    #[test]
    fn omitted_else_branch_returns_false() {
        assert!(!boolean("=IF(FALSE, 1)"));
        assert!(!boolean("=IF(FALSE, TRUE)"));
        assert!(boolean("=IF(TRUE, TRUE)"));
        assert!(!boolean("=IF(TRUE, FALSE)"));
        assert_eq!(num("=IF(TRUE, 1)"), 1.0);
    }

    #[test]
    fn text_in_a_condition_is_value() {
        assert_eq!(error("=IF(\"hello\", 1, 2)"), CalcError::Value);
        assert_eq!(error("=IF(\"1\", 1, 2)"), CalcError::Value);
        assert_eq!(error("=IF(\"TRUE\", 1, 2)"), CalcError::Value);
        let g = Grid::empty().set_text("A1", "hello");
        assert_eq!(g.error("=IF(A1, 1, 2)"), CalcError::Value);
    }

    #[test]
    fn a_taken_blank_branch_coerces_to_zero() {
        let g = Grid::empty();
        assert_eq!(g.num("=IF(TRUE, A1, 9)"), 0.0);
        assert_eq!(g.num("=IF(FALSE, 9, A1)"), 0.0);
    }

    #[test]
    fn condition_error_propagates_but_branch_errors_do_not() {
        assert_eq!(error("=IF(1/0, 7, 8)"), CalcError::Div0);
        assert_eq!(error("=IF(1/0, 7)"), CalcError::Div0);
        assert_eq!(error("=IF(SQRT(-1), 7, 8)"), CalcError::Num);
        // an error in the branch that is NOT taken must not surface
        assert_eq!(num("=IF(TRUE, 7, 1/0)"), 7.0);
        assert_eq!(num("=IF(FALSE, 1/0, 8)"), 8.0);
    }

    #[test]
    fn too_few_and_too_many_arguments_are_value() {
        assert_eq!(error("=IF(TRUE)"), CalcError::Value);
        assert_eq!(error("=IF(TRUE, 1, 2, 3)"), CalcError::Value);
    }
}

// ---------------------------------------------------------------------------
// IFS
// ---------------------------------------------------------------------------
mod logical_ifs {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn first_matching_value_wins() {
        assert_eq!(num("=IFS(TRUE, 1, TRUE, 2)"), 1.0);
        assert_eq!(num("=IFS(FALSE, 1, TRUE, 2)"), 2.0);
        assert_eq!(num("=IFS(FALSE, 1, FALSE, 2, TRUE, 3)"), 3.0);
        assert_eq!(num("=IFS(0, 1, 1, 2)"), 2.0); // 0 is false, 1 is true
        assert_eq!(num("=IFS(3, 1, 0, 2)"), 1.0); // any non-zero is true
        assert_eq!(text("=IFS(FALSE, \"a\", TRUE, \"b\")"), "b");
    }

    #[test]
    fn nothing_matching_is_na() {
        assert_eq!(error("=IFS(FALSE, 1)"), CalcError::Na);
        assert_eq!(error("=IFS(0, 1, 0, 2)"), CalcError::Na);
    }

    #[test]
    fn odd_argument_count_is_value() {
        assert_eq!(error("=IFS(TRUE, 1, 2)"), CalcError::Value);
        assert_eq!(error("=IFS(FALSE, 1, FALSE)"), CalcError::Value);
    }

    #[test]
    fn condition_error_propagates_only_when_reached() {
        assert_eq!(error("=IFS(1/0, 1, TRUE, 2)"), CalcError::Div0);
        assert_eq!(error("=IFS(FALSE, 1, 1/0, 2, TRUE, 3)"), CalcError::Div0);
        // a later pair's error is never reached once a match has been found
        assert_eq!(num("=IFS(FALSE, 1, TRUE, 2, 1/0, 3)"), 2.0);
    }

    #[test]
    fn text_condition_is_value() {
        assert_eq!(error("=IFS(\"x\", 1, TRUE, 2)"), CalcError::Value);
    }

    #[test]
    fn too_few_arguments_are_value() {
        assert_eq!(error("=IFS(TRUE)"), CalcError::Value);
    }
}

// ---------------------------------------------------------------------------
// AND / OR / XOR
// ---------------------------------------------------------------------------
mod logical_and_or_xor {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn and_is_all_true() {
        assert!(boolean("=AND(TRUE)"));
        assert!(!boolean("=AND(FALSE)"));
        assert!(boolean("=AND(TRUE, 1)"));
        assert!(!boolean("=AND(TRUE, 0)"));
        assert!(!boolean("=AND(1, 2, 0)"));
        assert!(boolean("=AND(1, 2, 3)"));
        assert!(!boolean("=AND(FALSE, TRUE, TRUE, TRUE, TRUE)"));
    }

    #[test]
    fn or_is_any_true() {
        assert!(!boolean("=OR(FALSE)"));
        assert!(boolean("=OR(FALSE, 1)"));
        assert!(!boolean("=OR(0, 0)"));
        assert!(boolean("=OR(0, 0, 1)"));
        assert!(boolean("=OR(TRUE, FALSE)"));
    }

    #[test]
    fn xor_is_the_parity_of_the_true_values() {
        assert!(boolean("=XOR(TRUE)"));
        assert!(!boolean("=XOR(FALSE)"));
        assert!(boolean("=XOR(TRUE, FALSE)"));
        assert!(!boolean("=XOR(TRUE, TRUE)"));
        assert!(boolean("=XOR(TRUE, TRUE, TRUE)"));
        assert!(!boolean("=XOR(FALSE, FALSE)"));
        assert!(!boolean("=XOR(1, 0, 1)")); // two trues -> even -> FALSE
        assert!(boolean("=XOR(1, 0)")); // one true -> odd -> TRUE
    }

    #[test]
    fn a_grid_column_ignores_text_and_blanks() {
        let g = Grid::empty()
            .set_num("A1", 1.0)
            .set_num("A2", 0.0)
            .set_bool("A3", true)
            .set_text("A4", "x");
        // A5 is blank; text and blank are ignored, the numbers and booleans decide.
        assert!(!g.boolean("=AND(A1:A5)"));
        assert!(g.boolean("=OR(A1:A5)"));
        assert!(!g.boolean("=XOR(A1:A5)"));
    }

    #[test]
    fn text_and_one_number_still_work() {
        let g = Grid::empty().set_text("B1", "x").set_num("B2", 1.0);
        assert!(g.boolean("=AND(B1:B2)"));
        assert!(g.boolean("=OR(B1:B2)"));
    }

    #[test]
    fn a_range_with_no_usable_value_is_value() {
        let g = Grid::empty().set_text("C1", "x").set_text("C2", "y");
        assert_eq!(g.error("=AND(C1:C2)"), CalcError::Value);
        assert_eq!(g.error("=OR(C1:C2)"), CalcError::Value);
    }

    #[test]
    fn a_direct_scalar_text_argument_is_value() {
        assert_eq!(error("=AND(\"x\")"), CalcError::Value);
        assert_eq!(error("=AND(\"TRUE\")"), CalcError::Value);
        assert_eq!(error("=OR(\"x\", TRUE)"), CalcError::Value);
        assert_eq!(error("=XOR(\"x\", TRUE)"), CalcError::Value);
        assert_eq!(error("=AND(TRUE, \"x\")"), CalcError::Value);
        assert_eq!(error("=OR(FALSE, \"x\")"), CalcError::Value);
        let g = Grid::empty().set_num("A1", 1.0).set_text("B1", "x");
        assert_eq!(g.error("=AND(A1, B1)"), CalcError::Value);
        assert_eq!(g.error("=OR(B1, A1)"), CalcError::Value);
    }

    #[test]
    fn error_arguments_propagate() {
        assert_eq!(error("=AND(TRUE, 1/0)"), CalcError::Div0);
        assert_eq!(error("=OR(FALSE, 1/0)"), CalcError::Div0);
        assert_eq!(error("=XOR(1/0, FALSE)"), CalcError::Div0);
        assert_eq!(error("=AND(1/0, TRUE)"), CalcError::Div0);
    }

    #[test]
    fn too_few_arguments_are_value() {
        assert_eq!(error("=AND()"), CalcError::Value);
        assert_eq!(error("=OR()"), CalcError::Value);
        assert_eq!(error("=XOR()"), CalcError::Value);
    }
}

// ---------------------------------------------------------------------------
// NOT
// ---------------------------------------------------------------------------
mod logical_not {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn flips_truthiness() {
        assert!(!boolean("=NOT(TRUE)"));
        assert!(boolean("=NOT(FALSE)"));
        assert!(!boolean("=NOT(1)"));
        assert!(boolean("=NOT(0)"));
        assert!(!boolean("=NOT(-1)"));
    }

    #[test]
    fn a_blank_is_false_so_not_is_true() {
        let g = Grid::empty();
        assert!(g.boolean("=NOT(A1)"));
    }

    #[test]
    fn text_is_value() {
        assert_eq!(error("=NOT(\"x\")"), CalcError::Value);
    }

    #[test]
    fn errors_propagate_and_arity() {
        assert_eq!(error("=NOT(1/0)"), CalcError::Div0);
        assert_eq!(error("=NOT()"), CalcError::Value);
        assert_eq!(error("=NOT(TRUE, FALSE)"), CalcError::Value);
    }
}

// ---------------------------------------------------------------------------
// TRUE / FALSE
// ---------------------------------------------------------------------------
mod logical_true_false {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn literal_values() {
        assert!(boolean("=TRUE()"));
        assert!(!boolean("=FALSE()"));
        assert_eq!(num("=IF(TRUE(), 5, 6)"), 5.0);
        assert_eq!(num("=IF(FALSE(), 5, 6)"), 6.0);
        assert!(boolean("=AND(TRUE(), TRUE())"));
        assert!(!boolean("=AND(TRUE(), FALSE())"));
    }

    #[test]
    fn too_many_arguments_are_value() {
        assert_eq!(error("=TRUE(1)"), CalcError::Value);
        assert_eq!(error("=FALSE(1)"), CalcError::Value);
    }
}

// ---------------------------------------------------------------------------
// IFERROR / IFNA
// ---------------------------------------------------------------------------
mod logical_iferror_ifna {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn iferror_passes_non_errors_through() {
        assert_eq!(num("=IFERROR(3, \"fallback\")"), 3.0);
        assert_eq!(text("=IFERROR(\"ok\", \"fallback\")"), "ok");
        assert!(!boolean("=IFERROR(FALSE, TRUE)"));
    }

    #[test]
    fn iferror_catches_every_error() {
        assert_eq!(text("=IFERROR(1/0, \"fallback\")"), "fallback");
        assert_eq!(text("=IFERROR(NA(), \"fallback\")"), "fallback");
        assert_eq!(num("=IFERROR(#N/A, 7)"), 7.0);
        assert_eq!(text("=IFERROR(SQRT(-1), \"fallback\")"), "fallback");
        assert_eq!(text("=IFERROR(0/0, \"fallback\")"), "fallback");
    }

    #[test]
    fn iferror_does_not_swallow_a_value() {
        assert_eq!(num("=IFERROR(1, 1/0)"), 1.0); // fallback error irrelevant when first arg is fine
    }

    #[test]
    fn iferror_with_a_failing_fallback_propagates_the_fallback() {
        assert_eq!(error("=IFERROR(1/0, 1/0)"), CalcError::Div0);
    }

    #[test]
    fn ifna_catches_only_na() {
        assert_eq!(num("=IFNA(NA(), 7)"), 7.0);
        assert_eq!(num("=IFNA(#N/A, 7)"), 7.0);
        assert_eq!(num("=IFNA(1, 7)"), 1.0);
        assert_eq!(text("=IFNA(\"x\", 7)"), "x");
    }

    #[test]
    fn ifna_passes_every_other_error_through_as_a_value() {
        assert_eq!(error("=IFNA(1/0, \"fallback\")"), CalcError::Div0);
        assert_eq!(error("=IFNA(SQRT(-1), \"fallback\")"), CalcError::Num);
    }

    #[test]
    fn arity() {
        assert_eq!(error("=IFERROR(1)"), CalcError::Value);
        assert_eq!(error("=IFERROR(1, 2, 3)"), CalcError::Value);
        assert_eq!(error("=IFNA(1)"), CalcError::Value);
        assert_eq!(error("=IFNA(1, 2, 3)"), CalcError::Value);
    }
}

// ---------------------------------------------------------------------------
// SWITCH
// ---------------------------------------------------------------------------
mod logical_switch {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn matches_the_expression_against_each_value_in_turn() {
        assert_eq!(text("=SWITCH(1, 1, \"one\", 2, \"two\")"), "one");
        assert_eq!(text("=SWITCH(2, 1, \"one\", 2, \"two\")"), "two");
        assert_eq!(
            text("=SWITCH(3, 1, \"one\", 2, \"two\", 3, \"three\")"),
            "three"
        );
        assert_eq!(num("=SWITCH(5, 5, 10, 6, 20)"), 10.0);
    }

    #[test]
    fn a_trailing_odd_argument_is_the_default() {
        assert_eq!(text("=SWITCH(5, 1, \"one\", \"default\")"), "default");
        assert_eq!(num("=SWITCH(9, 1, 10, 2, 20, 0)"), 0.0);
        assert_eq!(
            text("=SWITCH(\"b\", \"a\", \"A\", \"b\", \"B\", \"other\")"),
            "B"
        );
    }

    #[test]
    fn no_match_without_default_is_na() {
        assert_eq!(error("=SWITCH(5, 1, \"one\", 2, \"two\")"), CalcError::Na);
        assert_eq!(error("=SWITCH(5, 1, \"one\")"), CalcError::Na);
    }

    #[test]
    fn an_error_in_the_expression_propagates() {
        assert_eq!(error("=SWITCH(1/0, 1, \"one\")"), CalcError::Div0);
    }

    #[test]
    fn an_error_in_an_unreached_value_is_ignored() {
        assert_eq!(text("=SWITCH(1, 1, \"one\", 2, 1/0)"), "one");
    }

    #[test]
    fn too_few_arguments_are_value() {
        assert_eq!(error("=SWITCH(1, 2)"), CalcError::Value);
        assert_eq!(error("=SWITCH(1)"), CalcError::Value);
    }
}

// ---------------------------------------------------------------------------
// VLOOKUP
// ---------------------------------------------------------------------------
mod lookup_vlookup {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn exact_vs_approximate_against_the_same_table() {
        let g = kv_table();
        // exact (FALSE) finds 2 and misses 4 ...
        assert_eq!(g.text("=VLOOKUP(2, A1:B4, 2, FALSE)"), "two");
        assert_eq!(g.error("=VLOOKUP(4, A1:B4, 2, FALSE)"), CalcError::Na);
        // ... approximate (TRUE / omitted) rounds 4 down to the largest <= 4
        assert_eq!(g.text("=VLOOKUP(4, A1:B4, 2, TRUE)"), "three");
        assert_eq!(g.text("=VLOOKUP(4, A1:B4, 2)"), "three");
        assert_eq!(g.text("=VLOOKUP(4, A1:B4, 2, 1)"), "three"); // 1 == TRUE
        assert_eq!(g.text("=VLOOKUP(2, A1:B4, 2, 1)"), "two"); // exact tie works too
    }

    #[test]
    fn approximate_below_the_smallest_is_na() {
        let g = kv_table();
        assert_eq!(g.error("=VLOOKUP(0, A1:B4, 2, TRUE)"), CalcError::Na);
    }

    #[test]
    fn exact_match_does_not_coerce_types() {
        let g = kv_table();
        // the first column holds the NUMBER 2, not the text "2"
        assert_eq!(g.error("=VLOOKUP(\"2\", A1:B4, 2, FALSE)"), CalcError::Na);
    }

    #[test]
    fn range_lookup_accepts_numeric_and_text_flags() {
        let g = kv_table();
        assert_eq!(g.text("=VLOOKUP(2, A1:B4, 2, 0)"), "two"); // 0 == FALSE
        assert_eq!(g.text("=VLOOKUP(2, A1:B4, 2, \"FALSE\")"), "two");
        assert_eq!(g.text("=VLOOKUP(2, A1:B4, 2, \"TRUE\")"), "two");
    }

    #[test]
    fn column_index_below_1_is_value_beyond_width_is_ref() {
        let g = kv_table();
        // two DIFFERENT errors: 0 -> #VALUE!, past the width -> #REF!
        assert_eq!(g.error("=VLOOKUP(2, A1:B4, 0, FALSE)"), CalcError::Value);
        assert_eq!(g.error("=VLOOKUP(2, A1:B4, -1, FALSE)"), CalcError::Value);
        assert_eq!(g.error("=VLOOKUP(2, A1:B4, 3, FALSE)"), CalcError::Ref);
        assert_eq!(g.error("=VLOOKUP(2, A1:B4, 99, FALSE)"), CalcError::Ref);
    }

    #[test]
    fn column_index_truncates_and_coerces() {
        let g = kv_table();
        assert_eq!(g.text("=VLOOKUP(2, A1:B4, 2.7, FALSE)"), "two");
        assert_eq!(g.text("=VLOOKUP(2, A1:B4, \"2\", FALSE)"), "two");
        assert_eq!(
            g.error("=VLOOKUP(2, A1:B4, \"abc\", FALSE)"),
            CalcError::Value
        );
    }

    #[test]
    fn text_first_column_lookups() {
        let g = text_table();
        assert_eq!(g.num("=VLOOKUP(\"b\", C1:D3, 2, FALSE)"), 2.0);
        assert_eq!(g.num("=VLOOKUP(\"c\", C1:D3, 2)"), 3.0); // approximate exact tie
        assert_eq!(g.error("=VLOOKUP(\"d\", C1:D3, 2, FALSE)"), CalcError::Na);
    }

    #[test]
    fn a_table_cell_error_surfaces_only_when_that_row_is_returned() {
        let g = Grid::empty()
            .set_num("E1", 1.0)
            .set_num("F1", 10.0)
            .set_num("E2", 2.0)
            .set("F2", CalcValue::err(CalcError::Div0))
            .set_num("E3", 3.0)
            .set_num("F3", 30.0);
        assert_eq!(g.error("=VLOOKUP(2, E1:F3, 2, FALSE)"), CalcError::Div0);
        assert_eq!(g.num("=VLOOKUP(3, E1:F3, 2, FALSE)"), 30.0);
    }

    #[test]
    fn a_single_column_table_works() {
        let g = kv_table();
        assert_eq!(g.num("=VLOOKUP(2, A1:A4, 1, FALSE)"), 2.0);
    }

    #[test]
    fn arity_and_bad_sheet() {
        assert_eq!(error("=VLOOKUP(1, A1:B4)"), CalcError::Value);
        assert_eq!(error("=VLOOKUP(1, A1:B4, 2, FALSE, 5)"), CalcError::Value);
        assert_eq!(error("=VLOOKUP(1, Sheet2!A1:B4, 2)"), CalcError::Ref);
    }
}

// ---------------------------------------------------------------------------
// HLOOKUP
// ---------------------------------------------------------------------------
mod lookup_hlookup {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn exact_and_approximate_mirror_vlookup() {
        let g = hlookup_table();
        assert_eq!(g.text("=HLOOKUP(3, A1:C2, 2, FALSE)"), "c");
        assert_eq!(g.text("=HLOOKUP(1, A1:C2, 2, FALSE)"), "a");
        assert_eq!(g.error("=HLOOKUP(4, A1:C2, 2, FALSE)"), CalcError::Na);
        assert_eq!(g.text("=HLOOKUP(4, A1:C2, 2)"), "c"); // largest <= 4 is 3
        assert_eq!(g.text("=HLOOKUP(4, A1:C2, 2, TRUE)"), "c");
        assert_eq!(g.error("=HLOOKUP(0, A1:C2, 2, TRUE)"), CalcError::Na);
    }

    #[test]
    fn returns_from_either_row() {
        let g = hlookup_table();
        assert_eq!(g.num("=HLOOKUP(5, A1:C2, 1, FALSE)"), 5.0);
        assert_eq!(g.text("=HLOOKUP(5, A1:C2, 2, FALSE)"), "e");
    }

    #[test]
    fn row_index_below_1_is_value_beyond_height_is_ref() {
        let g = hlookup_table();
        assert_eq!(g.error("=HLOOKUP(3, A1:C2, 0, FALSE)"), CalcError::Value);
        assert_eq!(g.error("=HLOOKUP(3, A1:C2, -1, FALSE)"), CalcError::Value);
        assert_eq!(g.error("=HLOOKUP(3, A1:C2, 3, FALSE)"), CalcError::Ref);
        assert_eq!(g.error("=HLOOKUP(3, A1:C2, 9, FALSE)"), CalcError::Ref);
    }

    #[test]
    fn arity() {
        assert_eq!(error("=HLOOKUP(1, A1:C2)"), CalcError::Value);
        assert_eq!(error("=HLOOKUP(1, A1:C2, 1, TRUE, 5)"), CalcError::Value);
    }
}

// ---------------------------------------------------------------------------
// XLOOKUP
// ---------------------------------------------------------------------------
mod lookup_xlookup {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn exact_match_by_default() {
        let g = kv_table();
        assert_eq!(g.text("=XLOOKUP(2, A1:A4, B1:B4)"), "two");
        assert_eq!(g.text("=XLOOKUP(5, A1:A4, B1:B4)"), "five");
        assert_eq!(g.error("=XLOOKUP(6, A1:A4, B1:B4)"), CalcError::Na);
    }

    #[test]
    fn if_not_found_argument_replaces_the_na() {
        let g = kv_table();
        assert_eq!(g.text("=XLOOKUP(6, A1:A4, B1:B4, \"nf\")"), "nf");
        assert_eq!(g.text("=XLOOKUP(2, A1:A4, B1:B4, \"nf\")"), "two");
    }

    #[test]
    fn match_modes_next_smaller_and_next_larger() {
        let g = kv_table();
        // -1 = next smaller, 1 = next larger, 0 = exact
        assert_eq!(g.text("=XLOOKUP(4, A1:A4, B1:B4, \"\", -1)"), "three");
        assert_eq!(g.text("=XLOOKUP(4, A1:A4, B1:B4, \"\", 1)"), "five");
        assert_eq!(g.text("=XLOOKUP(3, A1:A4, B1:B4, \"none\", 0)"), "three");
        assert_eq!(g.text("=XLOOKUP(4, A1:A4, B1:B4, \"none\", 0)"), "none");
    }

    #[test]
    fn wildcard_mode() {
        let g = Grid::empty()
            .set_text("C1", "apple")
            .set_num("D1", 10.0)
            .set_text("C2", "banana")
            .set_num("D2", 20.0)
            .set_text("C3", "cherry")
            .set_num("D3", 30.0);
        assert_eq!(g.num("=XLOOKUP(\"a*\", C1:C3, D1:D3, \"\", 2)"), 10.0);
        assert_eq!(g.num("=XLOOKUP(\"?herry\", C1:C3, D1:D3, \"\", 2)"), 30.0);
        assert_eq!(g.num("=XLOOKUP(\"*n*\", C1:C3, D1:D3, \"\", 2)"), 20.0);
    }

    #[test]
    fn lookup_and_return_arrays_of_different_lengths_are_value() {
        let g = kv_table();
        assert_eq!(g.error("=XLOOKUP(1, A1:A4, B1:B2)"), CalcError::Value);
    }

    #[test]
    fn invalid_match_and_search_modes_are_value() {
        let g = kv_table();
        assert_eq!(
            g.error("=XLOOKUP(1, A1:A4, B1:B4, \"\", 5)"),
            CalcError::Value
        );
        assert_eq!(
            g.error("=XLOOKUP(1, A1:A4, B1:B4, \"\", 0, 0)"),
            CalcError::Value
        );
        assert_eq!(
            g.error("=XLOOKUP(1, A1:A4, B1:B4, \"\", \"abc\")"),
            CalcError::Value
        );
    }

    #[test]
    fn arity() {
        assert_eq!(error("=XLOOKUP(1, A1:A4)"), CalcError::Value);
        assert_eq!(
            error("=XLOOKUP(1, A1:A4, B1:B4, \"\", 0, 1, 0)"),
            CalcError::Value
        );
    }
}

// ---------------------------------------------------------------------------
// MATCH
// ---------------------------------------------------------------------------
mod lookup_match {
    use super::*;
    use pretty_assertions::assert_eq;

    fn ascending() -> Grid {
        Grid::empty().col("A1", &[1.0, 2.0, 3.0, 5.0])
    }

    fn descending() -> Grid {
        Grid::empty().col("D1", &[5.0, 4.0, 3.0, 2.0])
    }

    fn text_words() -> Grid {
        Grid::empty()
            .set_text("E1", "apple")
            .set_text("E2", "apricot")
            .set_text("E3", "banana")
    }

    #[test]
    fn exact_match_is_one_based_and_na_when_absent() {
        let g = ascending();
        assert_eq!(g.num("=MATCH(2, A1:A4, 0)"), 2.0);
        assert_eq!(g.num("=MATCH(1, A1:A4, 0)"), 1.0);
        assert_eq!(g.num("=MATCH(5, A1:A4, 0)"), 4.0);
        assert_eq!(g.error("=MATCH(4, A1:A4, 0)"), CalcError::Na);
        assert_eq!(g.error("=MATCH(9, A1:A4, 0)"), CalcError::Na);
    }

    #[test]
    fn match_type_1_needs_ascending_data() {
        let g = ascending();
        assert_eq!(g.num("=MATCH(4, A1:A4, 1)"), 3.0); // largest <= 4 is 3
        assert_eq!(g.num("=MATCH(3, A1:A4, 1)"), 3.0);
        assert_eq!(g.num("=MATCH(5, A1:A4, 1)"), 4.0);
        assert_eq!(g.num("=MATCH(4, A1:A4)"), 3.0); // match type defaults to 1
        assert_eq!(g.error("=MATCH(0, A1:A4, 1)"), CalcError::Na);
    }

    #[test]
    fn match_type_minus_1_needs_descending_data() {
        let g = descending();
        assert_eq!(g.num("=MATCH(3, D1:D4, -1)"), 3.0);
        assert_eq!(g.num("=MATCH(4, D1:D4, -1)"), 2.0);
        assert_eq!(g.num("=MATCH(5, D1:D4, -1)"), 1.0);
        assert_eq!(g.num("=MATCH(1, D1:D4, -1)"), 4.0);
        assert_eq!(g.error("=MATCH(6, D1:D4, -1)"), CalcError::Na);
    }

    #[test]
    fn match_type_0_allows_wildcards_on_text() {
        let g = text_words();
        assert_eq!(g.num("=MATCH(\"ap*\", E1:E3, 0)"), 1.0);
        assert_eq!(g.num("=MATCH(\"apri?ot\", E1:E3, 0)"), 2.0);
        assert_eq!(g.num("=MATCH(\"*na\", E1:E3, 0)"), 3.0);
        assert_eq!(g.error("=MATCH(\"z*\", E1:E3, 0)"), CalcError::Na);
    }

    #[test]
    fn exact_match_does_not_coerce_types() {
        let g = Grid::empty()
            .set_num("F1", 1.0)
            .set_text("F2", "2")
            .set_num("F3", 2.0);
        // the NUMBER 2 (position 3) is found for a numeric lookup, the TEXT "2"
        // (position 2) for a text lookup — each finds its own kind
        assert_eq!(g.num("=MATCH(2, F1:F3, 0)"), 3.0);
        assert_eq!(g.num("=MATCH(\"2\", F1:F3, 0)"), 2.0);
    }

    #[test]
    fn invalid_match_type_and_non_numeric_text_are_value() {
        let g = ascending();
        assert_eq!(g.error("=MATCH(2, A1:A4, 5)"), CalcError::Value);
        assert_eq!(g.error("=MATCH(2, A1:A4, \"abc\")"), CalcError::Value);
    }

    #[test]
    fn an_error_cell_in_the_array_propagates() {
        let g = Grid::empty()
            .set("M1", CalcValue::err(CalcError::Div0))
            .set_num("M2", 1.0);
        assert_eq!(g.error("=MATCH(1, M1:M2, 0)"), CalcError::Div0);
    }

    #[test]
    fn arity() {
        assert_eq!(error("=MATCH(1)"), CalcError::Value);
        assert_eq!(error("=MATCH(1, A1:A4, 0, 1)"), CalcError::Value);
    }
}

// ---------------------------------------------------------------------------
// INDEX
// ---------------------------------------------------------------------------
mod lookup_index {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn picks_one_based_cells() {
        let g = index_grid();
        assert_eq!(g.num("=INDEX(A1:C3, 2, 2)"), 5.0);
        assert_eq!(g.num("=INDEX(A1:C3, 3, 1)"), 7.0);
        assert_eq!(g.num("=INDEX(A1:C3, 1, 3)"), 3.0);
        assert_eq!(g.num("=INDEX(A1:C3, 2.7, 1)"), 4.0); // index is truncated
    }

    #[test]
    fn a_zero_row_or_column_returns_the_whole_slice_as_an_array() {
        let g = index_grid();
        let col = g.array("=INDEX(A1:C3, 0, 2)");
        assert_eq!(col.shape(), (3, 1));
        assert_eq!(col.get(0, 0), &CalcValue::Number(2.0));
        assert_eq!(col.get(2, 0), &CalcValue::Number(8.0));

        let row = g.array("=INDEX(A1:C3, 2, 0)");
        assert_eq!(row.shape(), (1, 3));
        assert_eq!(row.get(0, 0), &CalcValue::Number(4.0));
        assert_eq!(row.get(0, 2), &CalcValue::Number(6.0));

        let whole = g.array("=INDEX(A1:C3, 0, 0)");
        assert_eq!(whole.shape(), (3, 3));
        assert_eq!(whole.get(0, 0), &CalcValue::Number(1.0));
        assert_eq!(whole.get(2, 2), &CalcValue::Number(9.0));
    }

    #[test]
    fn omitted_col_num_returns_a_row_or_a_single_cell() {
        let g = index_grid();
        let row2 = g.array("=INDEX(A1:C3, 2)");
        assert_eq!(row2.shape(), (1, 3));
        assert_eq!(row2.get(0, 1), &CalcValue::Number(5.0));

        let single = Grid::empty().col("A1", &[10.0, 20.0, 30.0]);
        assert_eq!(single.num("=INDEX(A1:A3, 3)"), 30.0);

        let row1 = Grid::empty().row("A1", &[1.0, 2.0, 3.0]);
        assert_eq!(row1.num("=INDEX(A1:C1, 1, 2)"), 2.0);
    }

    #[test]
    fn out_of_range_is_ref_and_negative_is_value() {
        let g = index_grid();
        assert_eq!(g.error("=INDEX(A1:C3, 5, 1)"), CalcError::Ref);
        assert_eq!(g.error("=INDEX(A1:C3, 1, 5)"), CalcError::Ref);
        assert_eq!(g.error("=INDEX(A1:C3, 0, 5)"), CalcError::Ref);
        assert_eq!(g.error("=INDEX(A1:C3, 5, 0)"), CalcError::Ref);
        assert_eq!(g.error("=INDEX(A1:C3, -1, 1)"), CalcError::Value);
        assert_eq!(g.error("=INDEX(A1:C3, 1, -1)"), CalcError::Value);
    }

    #[test]
    fn arity_and_non_numeric_index() {
        assert_eq!(error("=INDEX(A1:C3)"), CalcError::Value);
        assert_eq!(error("=INDEX(A1:C3, 1, 1, 1)"), CalcError::Value);
        assert_eq!(error("=INDEX(A1:C3, \"abc\")"), CalcError::Value);
    }
}

// ---------------------------------------------------------------------------
// LOOKUP
// ---------------------------------------------------------------------------
mod lookup_lookup {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn vector_form_is_approximate_over_ascending_data() {
        let g = kv_table();
        assert_eq!(g.text("=LOOKUP(4, A1:A4, B1:B4)"), "three");
        assert_eq!(g.text("=LOOKUP(2, A1:A4, B1:B4)"), "two");
        assert_eq!(g.text("=LOOKUP(1, A1:A4, B1:B4)"), "one");
        assert_eq!(g.text("=LOOKUP(5, A1:A4, B1:B4)"), "five");
        assert_eq!(g.text("=LOOKUP(6, A1:A4, B1:B4)"), "five"); // rounds up to the largest
        assert_eq!(g.error("=LOOKUP(0, A1:A4, B1:B4)"), CalcError::Na);
    }

    #[test]
    fn text_vectors_look_up_alphabetically() {
        let g = text_table();
        assert_eq!(g.num("=LOOKUP(\"b\", C1:C3, D1:D3)"), 2.0);
        assert_eq!(g.num("=LOOKUP(\"z\", C1:C3, D1:D3)"), 3.0);
        assert_eq!(g.error("=LOOKUP(\"0\", C1:C3, D1:D3)"), CalcError::Na);
    }

    #[test]
    fn two_argument_array_form_searches_first_row_or_column() {
        let tall = kv_table();
        assert_eq!(tall.text("=LOOKUP(4, A1:B4)"), "three");

        let wide = hlookup_table();
        assert_eq!(wide.text("=LOOKUP(4, A1:C2)"), "c");
    }

    #[test]
    fn length_mismatch_and_arity() {
        let g = kv_table();
        assert_eq!(g.error("=LOOKUP(1, A1:A4, B1:B2)"), CalcError::Value);
        assert_eq!(error("=LOOKUP(1)"), CalcError::Value);
        assert_eq!(error("=LOOKUP(1, A1:A4, B1:B4, 2)"), CalcError::Value);
    }
}

// ---------------------------------------------------------------------------
// CHOOSE
// ---------------------------------------------------------------------------
mod lookup_choose {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn picks_by_one_based_index() {
        assert_eq!(text("=CHOOSE(2, \"a\", \"b\", \"c\")"), "b");
        assert_eq!(text("=CHOOSE(1, \"a\", \"b\")"), "a");
        assert_eq!(num("=CHOOSE(3, 10, 20, 30)"), 30.0);
        assert_eq!(num("=CHOOSE(1, 100)"), 100.0);
    }

    #[test]
    fn index_truncates_and_coerces_numeric_text() {
        assert_eq!(num("=CHOOSE(1.9, 10, 20)"), 10.0);
        assert_eq!(num("=CHOOSE(2.1, 10, 20)"), 20.0);
        assert_eq!(num("=CHOOSE(\"2\", 10, 20)"), 20.0);
    }

    #[test]
    fn index_outside_the_value_list_is_value() {
        assert_eq!(error("=CHOOSE(0, \"a\")"), CalcError::Value);
        assert_eq!(error("=CHOOSE(2, \"a\")"), CalcError::Value);
        assert_eq!(error("=CHOOSE(3, \"a\", \"b\")"), CalcError::Value);
        assert_eq!(error("=CHOOSE(-1, \"a\", \"b\")"), CalcError::Value);
    }

    #[test]
    fn only_the_selected_value_is_evaluated() {
        assert_eq!(num("=CHOOSE(2, 1/0, 5)"), 5.0);
        assert_eq!(error("=CHOOSE(1, 1/0, 5)"), CalcError::Div0);
    }

    #[test]
    fn arity_and_non_numeric_index() {
        assert_eq!(error("=CHOOSE(1)"), CalcError::Value);
        assert_eq!(error("=CHOOSE(\"abc\", \"a\", \"b\")"), CalcError::Value);
    }

    #[test]
    fn floating_point_results_need_approx() {
        approx("=CHOOSE(1, 0.1 + 0.2, 7)", 0.3, 1e-9);
    }
}

// ---------------------------------------------------------------------------
// cross-type comparison ordering
// ---------------------------------------------------------------------------
mod cross_type_ordering {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn excel_orders_numbers_below_text_below_false_below_true() {
        // G is sorted in Excel's cross-type order: 1 < "a" < FALSE < TRUE.
        let g = Grid::empty()
            .set_num("G1", 1.0)
            .set_text("H1", "one")
            .set_text("G2", "a")
            .set_text("H2", "A")
            .set_bool("G3", false)
            .set_text("H3", "F")
            .set_bool("G4", true)
            .set_text("H4", "T");
        // a numeric needle can only be satisfied by a number: every text or
        // boolean ranks above every number, so the largest entry <= 2 is the 1
        assert_eq!(g.text("=VLOOKUP(2, G1:H4, 2)"), "one");
        // a text needle is satisfied by numbers and text, and the text wins:
        // the largest entry <= "x" is "a"
        assert_eq!(g.text("=VLOOKUP(\"x\", G1:H4, 2)"), "A");
        // a boolean needle is satisfied by everything, and TRUE wins
        assert_eq!(g.text("=VLOOKUP(TRUE, G1:H4, 2)"), "T");
    }
}

// ---------------------------------------------------------------------------
// shared error taxonomy
// ---------------------------------------------------------------------------
mod error_taxonomy {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn the_excel_error_codes_map_to_the_right_calc_error() {
        assert_eq!(error("=1/0"), CalcError::Div0);
        assert_eq!(error("=SQRT(-1)"), CalcError::Num);
        assert_eq!(error("=NOSUCHFUNC(1)"), CalcError::Name);
        assert_eq!(error("=Sheet2!A1"), CalcError::Ref);
        assert_eq!(error("=VLOOKUP(99, A1:B4, 2, FALSE)"), CalcError::Na);
        assert_eq!(error("=IF(\"x\", 1, 2)"), CalcError::Value);
    }

    #[test]
    fn an_unknown_function_is_name_not_a_parse_failure() {
        assert_eq!(calc("=BOGUSFN(1)"), Outcome::Err(CalcError::Name));
        assert_eq!(calc("=1+"), Outcome::ParseError);
    }

    #[test]
    fn an_unregistered_tier1_gate_still_holds_for_this_class() {
        // TRUE/FALSE/IF must be live in the registry, exactly as Excel spells them.
        assert!(boolean("=IF(TRUE(), TRUE, FALSE)"));
        assert!(boolean("=NOT(FALSE())"));
        assert!(!boolean("=AND(FALSE(), TRUE)"));
    }
}
