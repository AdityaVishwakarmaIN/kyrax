// tests_class_text_datetime.rs — the TEXT and DATE/TIME class matrix.
//
// Every test drives the real pipeline (parse_formula -> eval) through the
// shared testkit helpers. Expectations follow Excel ground truth; where the
// engine deviates, the test says so and stays failing (see the serial-zero
// case in `year_month_day`).

#![cfg(test)]

use super::testkit::{Grid, Outcome, approx, boolean, calc, error, num, text};
use crate::turbo::calc::value::CalcError;

mod left_right_mid {
    use super::*;

    #[test]
    fn defaults_and_basic_positions() {
        assert_eq!(text("=LEFT(\"hello\",3)"), "hel");
        assert_eq!(text("=LEFT(\"hello\")"), "h");
        assert_eq!(text("=LEFT(\"hello\",0)"), "");
        assert_eq!(text("=RIGHT(\"hello\",3)"), "llo");
        assert_eq!(text("=RIGHT(\"hello\")"), "o");
        assert_eq!(text("=RIGHT(\"hello\",0)"), "");
        assert_eq!(text("=MID(\"hello\",2,3)"), "ell");
        assert_eq!(text("=MID(\"hello\",1,5)"), "hello");
    }

    #[test]
    fn negative_count_is_value() {
        assert_eq!(error("=LEFT(\"hello\",-1)"), CalcError::Value);
        assert_eq!(error("=RIGHT(\"hello\",-1)"), CalcError::Value);
        assert_eq!(error("=MID(\"hello\",1,-1)"), CalcError::Value);
    }

    #[test]
    fn count_past_the_end_returns_the_whole_string() {
        assert_eq!(text("=LEFT(\"hello\",99)"), "hello");
        assert_eq!(text("=RIGHT(\"hello\",99)"), "hello");
        assert_eq!(text("=MID(\"hello\",2,99)"), "ello");
        assert_eq!(text("=MID(\"hello\",6,99)"), "");
    }

    #[test]
    fn mid_start_below_1_is_value_start_past_end_is_empty() {
        assert_eq!(error("=MID(\"hello\",0,3)"), CalcError::Value);
        assert_eq!(error("=MID(\"hello\",-2,3)"), CalcError::Value);
        assert_eq!(text("=MID(\"hello\",6,3)"), "");
        assert_eq!(text("=MID(\"hello\",99,3)"), "");
    }

    #[test]
    fn counts_characters_not_bytes_and_never_panics() {
        assert_eq!(text("=LEFT(\"héllo\",4)"), "héll");
        assert_eq!(text("=RIGHT(\"héllo\",3)"), "llo");
        assert_eq!(text("=MID(\"héllo\",2,3)"), "éll");
        assert_eq!(num("=LEN(\"héllo\")"), 5.0);
        assert_eq!(text("=LEFT(\"héllo\",99)"), "héllo");
        assert_eq!(text("=MID(\"héllo\",6,3)"), "");
        assert_eq!(text("=LEFT(\"こんにちは\",3)"), "こんに");
        assert_eq!(text("=RIGHT(\"こんにちは\",2)"), "ちは");
        assert_eq!(text("=MID(\"こんにちは\",4,2)"), "ちは");
        assert_eq!(num("=LEN(\"こんにちは\")"), 5.0);
    }

    #[test]
    fn counts_are_truncated_toward_zero() {
        assert_eq!(text("=LEFT(\"hello\",2.9)"), "he");
        assert_eq!(text("=RIGHT(\"hello\",2.9)"), "lo");
        assert_eq!(text("=MID(\"hello\",1.9,2)"), "he");
    }

    #[test]
    fn coerces_numbers_booleans_and_blanks() {
        assert_eq!(text("=LEFT(12345,3)"), "123");
        assert_eq!(text("=RIGHT(12345,3)"), "345");
        assert_eq!(text("=LEFT(TRUE,1)"), "T");
        assert_eq!(text("=RIGHT(TRUE,2)"), "UE");
        let g = Grid::empty();
        assert_eq!(g.text("=LEFT(A1,1)"), "");
        assert_eq!(g.text("=RIGHT(A1,1)"), "");
        assert_eq!(g.text("=MID(A1,1,1)"), "");
    }

    #[test]
    fn errors() {
        assert_eq!(error("=LEFT()"), CalcError::Value);
        assert_eq!(error("=LEFT(\"a\",1,1)"), CalcError::Value);
        assert_eq!(error("=MID(\"abc\",1)"), CalcError::Value);
        assert_eq!(error("=MID(\"abc\",1,1,1)"), CalcError::Value);
        assert_eq!(error("=LEFT(\"abc\",\"x\")"), CalcError::Value);
        assert_eq!(error("=LEFT(1/0,1)"), CalcError::Div0);
        assert_eq!(error("=RIGHT(\"abc\",1/0)"), CalcError::Div0);
        assert_eq!(error("=MID(1/0,1,1)"), CalcError::Div0);
    }
}

mod len_trim_case {
    use super::*;

    #[test]
    fn len_counts_characters() {
        assert_eq!(num("=LEN(\"hello\")"), 5.0);
        assert_eq!(num("=LEN(\"\")"), 0.0);
        assert_eq!(num("=LEN(\"héllo\")"), 5.0);
        assert_eq!(num("=LEN(\"こんにちは\")"), 5.0);
        assert_eq!(num("=LEN(12345)"), 5.0);
        assert_eq!(num("=LEN(1.5)"), 3.0);
        assert_eq!(num("=LEN(TRUE)"), 4.0);
        let g = Grid::empty();
        assert_eq!(g.num("=LEN(A1)"), 0.0);
    }

    #[test]
    fn trim_collapses_spaces_but_preserves_other_whitespace() {
        assert_eq!(text("=TRIM(\"  hello   world  \")"), "hello world");
        assert_eq!(text("=TRIM(\"a  b\")"), "a b");
        assert_eq!(text("=TRIM(\"a\tb\")"), "a\tb");
        assert_eq!(text("=TRIM(\"\")"), "");
        assert_eq!(text("=TRIM(123)"), "123");
    }

    #[test]
    fn upper_lower_proper() {
        assert_eq!(text("=UPPER(\"Hello World\")"), "HELLO WORLD");
        assert_eq!(text("=UPPER(\"héllo\")"), "HÉLLO");
        assert_eq!(text("=LOWER(\"HeLLo\")"), "hello");
        assert_eq!(text("=PROPER(\"hello WORLD\")"), "Hello World");
        assert_eq!(text("=PROPER(\"2-way street\")"), "2-Way Street");
        assert_eq!(text("=PROPER(\"o'brien\")"), "O'Brien");
    }

    #[test]
    fn errors() {
        assert_eq!(error("=LEN()"), CalcError::Value);
        assert_eq!(error("=LEN(\"a\",\"b\")"), CalcError::Value);
        assert_eq!(error("=LEN(1/0)"), CalcError::Div0);
        assert_eq!(error("=TRIM(1/0)"), CalcError::Div0);
        assert_eq!(error("=UPPER(1/0)"), CalcError::Div0);
        assert_eq!(error("=LOWER(1/0)"), CalcError::Div0);
        assert_eq!(error("=PROPER(1/0)"), CalcError::Div0);
    }
}

mod find_search {
    use super::*;

    #[test]
    fn find_is_case_sensitive_and_rejects_wildcards() {
        assert_eq!(num("=FIND(\"l\",\"hello\")"), 3.0);
        assert_eq!(error("=FIND(\"L\",\"hello\")"), CalcError::Value);
        assert_eq!(num("=FIND(\"h\",\"hello\")"), 1.0);
        assert_eq!(num("=FIND(\"cd\",\"abcd\")"), 3.0);
        assert_eq!(num("=FIND(\"l\",\"hello\",4)"), 4.0);
        assert_eq!(error("=FIND(\"l\",\"hello\",5)"), CalcError::Value);
        // `*` is a literal character for FIND, so this finds nothing.
        assert_eq!(error("=FIND(\"l*\",\"hello\")"), CalcError::Value);
    }

    #[test]
    fn search_is_case_insensitive_and_accepts_wildcards() {
        assert_eq!(num("=SEARCH(\"l\",\"hello\")"), 3.0);
        assert_eq!(num("=SEARCH(\"L\",\"hello\")"), 3.0);
        assert_eq!(num("=SEARCH(\"l*\",\"hello\")"), 3.0);
        assert_eq!(num("=SEARCH(\"?orld\",\"hello world\")"), 7.0);
        assert_eq!(num("=SEARCH(\"*o*\",\"hello\")"), 1.0);
        assert_eq!(num("=SEARCH(\"~*\",\"a*b\")"), 2.0);
        assert_eq!(num("=SEARCH(\"b\",\"abc\",2)"), 2.0);
        assert_eq!(error("=SEARCH(\"a\",\"abc\",2)"), CalcError::Value);
        assert_eq!(error("=SEARCH(\"x\",\"abc\")"), CalcError::Value);
        assert_eq!(error("=SEARCH(\"z*\",\"abc\")"), CalcError::Value);
    }

    #[test]
    fn the_same_needle_through_both() {
        assert_eq!(error("=FIND(\"W\",\"hello world\")"), CalcError::Value);
        assert_eq!(num("=SEARCH(\"W\",\"hello world\")"), 7.0);
        assert_eq!(error("=FIND(\"ABC\",\"aBCd\")"), CalcError::Value);
        assert_eq!(num("=SEARCH(\"abc\",\"aBCd\")"), 1.0);
    }

    #[test]
    fn empty_needle() {
        assert_eq!(num("=FIND(\"\",\"abc\")"), 1.0);
        assert_eq!(num("=FIND(\"\",\"abc\",3)"), 3.0);
        assert_eq!(error("=FIND(\"\",\"abc\",5)"), CalcError::Value);
        assert_eq!(num("=SEARCH(\"\",\"abc\",2)"), 2.0);
    }

    #[test]
    fn start_below_1_is_value() {
        assert_eq!(error("=FIND(\"l\",\"hello\",0)"), CalcError::Value);
        assert_eq!(error("=SEARCH(\"l\",\"hello\",0)"), CalcError::Value);
        assert_eq!(error("=FIND(\"l\",\"hello\",-1)"), CalcError::Value);
    }

    #[test]
    fn multibyte_positions_count_characters() {
        assert_eq!(num("=FIND(\"に\",\"こんにちは\")"), 3.0);
        assert_eq!(num("=SEARCH(\"に*\",\"こんにちは\")"), 3.0);
    }

    #[test]
    fn errors() {
        assert_eq!(error("=FIND(\"a\")"), CalcError::Value);
        assert_eq!(error("=SEARCH(\"a\",\"b\",1,1)"), CalcError::Value);
        assert_eq!(error("=FIND(1/0,\"abc\")"), CalcError::Div0);
        assert_eq!(error("=FIND(\"a\",1/0)"), CalcError::Div0);
        assert_eq!(error("=SEARCH(\"a\",1/0)"), CalcError::Div0);
    }
}

mod substitute_replace {
    use super::*;

    #[test]
    fn substitute_all_and_nth_instance() {
        assert_eq!(text("=SUBSTITUTE(\"a-b-c\",\"-\",\"_\")"), "a_b_c");
        assert_eq!(text("=SUBSTITUTE(\"a-b-c\",\"-\",\"_\",2)"), "a-b_c");
        assert_eq!(text("=SUBSTITUTE(\"a-b-c\",\"-\",\"_\",5)"), "a-b-c");
    }

    #[test]
    fn substitute_empty_old_text_leaves_text_unchanged() {
        assert_eq!(text("=SUBSTITUTE(\"a-b-c\",\"\",\"_\")"), "a-b-c");
        assert_eq!(text("=SUBSTITUTE(\"abc\",\"\",\"x\")"), "abc");
    }

    #[test]
    fn substitute_is_case_sensitive() {
        assert_eq!(text("=SUBSTITUTE(\"A-a\",\"a\",\"x\")"), "A-x");
        assert_eq!(text("=SUBSTITUTE(\"A-a\",\"A\",\"x\")"), "x-a");
    }

    #[test]
    fn replace_is_positional() {
        assert_eq!(text("=REPLACE(\"abcdef\",2,3,\"XY\")"), "aXYef");
        assert_eq!(text("=REPLACE(\"abc\",2,0,\"X\")"), "aXbc");
        assert_eq!(text("=REPLACE(\"abcdef\",2,100,\"X\")"), "aX");
        assert_eq!(text("=REPLACE(\"abc\",4,1,\"X\")"), "abcX");
        assert_eq!(text("=REPLACE(\"abc\",2,1,\"\")"), "ac");
    }

    #[test]
    fn errors() {
        assert_eq!(error("=SUBSTITUTE(\"a\",\"b\")"), CalcError::Value);
        assert_eq!(
            error("=SUBSTITUTE(\"aaa\",\"a\",\"x\",0)"),
            CalcError::Value
        );
        assert_eq!(
            error("=SUBSTITUTE(\"aaa\",\"a\",\"x\",-1)"),
            CalcError::Value
        );
        assert_eq!(error("=REPLACE(\"abc\",0,1,\"X\")"), CalcError::Value);
        assert_eq!(error("=REPLACE(\"abc\",2,-1,\"X\")"), CalcError::Value);
        assert_eq!(error("=REPLACE(\"abc\",\"x\",1,\"X\")"), CalcError::Value);
        assert_eq!(error("=SUBSTITUTE(1/0,\"a\",\"b\")"), CalcError::Div0);
        assert_eq!(error("=REPLACE(1/0,1,1,\"x\")"), CalcError::Div0);
    }
}

mod value_fn {
    use super::*;

    #[test]
    fn parses_numeric_text() {
        assert_eq!(num("=VALUE(\"123\")"), 123.0);
        assert_eq!(num("=VALUE(\"  42  \")"), 42.0);
        assert_eq!(num("=VALUE(\"12%\")"), 0.12);
        assert_eq!(num("=VALUE(\"1e3\")"), 1000.0);
        assert_eq!(num("=VALUE(\"123.45\")"), 123.45);
        assert_eq!(num("=VALUE(3.14)"), 3.14);
    }

    #[test]
    fn refuses_non_numeric_text() {
        assert_eq!(error("=VALUE(\"abc\")"), CalcError::Value);
        assert_eq!(error("=VALUE(\"\")"), CalcError::Value);
        assert_eq!(error("=VALUE(\"1,000\")"), CalcError::Value);
    }

    #[test]
    fn refuses_booleans_and_blanks() {
        assert_eq!(error("=VALUE(TRUE)"), CalcError::Value);
        assert_eq!(error("=VALUE(FALSE)"), CalcError::Value);
        let g = Grid::empty();
        assert_eq!(g.error("=VALUE(A1)"), CalcError::Value);
    }

    #[test]
    fn errors() {
        assert_eq!(error("=VALUE()"), CalcError::Value);
        assert_eq!(error("=VALUE(1,2)"), CalcError::Value);
        assert_eq!(error("=VALUE(1/0)"), CalcError::Div0);
    }
}

mod text_formatting {
    use super::*;

    #[test]
    fn digit_patterns() {
        assert_eq!(text("=TEXT(1234.5,\"0\")"), "1235");
        assert_eq!(text("=TEXT(2.5,\"0\")"), "3");
        assert_eq!(text("=TEXT(-2.5,\"0\")"), "-3");
        assert_eq!(text("=TEXT(3.14159,\"0.00\")"), "3.14");
        assert_eq!(text("=TEXT(0.1+0.2,\"0.00\")"), "0.30");
        assert_eq!(text("=TEXT(5,\"#\")"), "5");
        assert_eq!(text("=TEXT(0,\"#\")"), "");
        assert_eq!(text("=TEXT(0.5,\"0.##\")"), "0.5");
        assert_eq!(text("=TEXT(1.2,\"0.0#\")"), "1.2");
        assert_eq!(text("=TEXT(0,\"0.00\")"), "0.00");
        // a boolean coerces to its numeric 1, as it does in Excel
        assert_eq!(text("=TEXT(TRUE,\"0\")"), "1");
        assert_eq!(text("=TEXT(FALSE,\"0\")"), "0");
    }

    #[test]
    fn grouping_percent_and_general() {
        assert_eq!(text("=TEXT(1234567.891,\"#,##0\")"), "1,234,568");
        assert_eq!(text("=TEXT(1234567.891,\"#,##0.00\")"), "1,234,567.89");
        assert_eq!(text("=TEXT(123,\"0,000\")"), "0,123");
        assert_eq!(text("=TEXT(0.5,\"0%\")"), "50%");
        assert_eq!(text("=TEXT(-0.25,\"0.0%\")"), "-25.0%");
        assert_eq!(text("=TEXT(12345.678,\"General\")"), "12345.678");
        assert_eq!(text("=TEXT(1,\"GENERAL\")"), "1");
    }

    #[test]
    fn refuses_format_codes_it_cannot_reproduce_exactly() {
        // A date code is an approximation, not an exact reproduction; refusal is the feature.
        assert_eq!(error("=TEXT(45352,\"yyyy-mm-dd\")"), CalcError::Value);
        assert_eq!(error("=TEXT(45352,\"mm/dd/yyyy\")"), CalcError::Value);
        assert_eq!(error("=TEXT(45352,\"dd/mm/yyyy hh:mm\")"), CalcError::Value);
        assert_eq!(error("=TEXT(123,\"0.00E+00\")"), CalcError::Value);
        assert_eq!(error("=TEXT(5,\"@\")"), CalcError::Value);
        assert_eq!(error("=TEXT(5,\"0.00;[Red]-0.00\")"), CalcError::Value);
        // quoted literals are now reproduced exactly, as Excel does
        assert_eq!(text("=TEXT(5,\"0 \"\"units\"\"\")"), "5 units");
    }

    #[test]
    fn refuses_a_non_numeric_value() {
        assert_eq!(error("=TEXT(\"abc\",\"0\")"), CalcError::Value);
        assert_eq!(error("=TEXT(\"12x\",\"0\")"), CalcError::Value);
    }

    #[test]
    fn errors() {
        assert_eq!(error("=TEXT(1)"), CalcError::Value);
        assert_eq!(error("=TEXT(1,\"0\",0)"), CalcError::Value);
        assert_eq!(error("=TEXT(1/0,\"0\")"), CalcError::Div0);
        assert_eq!(error("=TEXT(1,\"yyyy\")"), CalcError::Value);
    }
}

mod concat_family {
    use super::*;

    #[test]
    fn concat_scalars() {
        assert_eq!(text("=CONCAT(\"ab\",\"cd\")"), "abcd");
        assert_eq!(text("=CONCAT(1,2)"), "12");
        assert_eq!(text("=CONCAT(TRUE,FALSE)"), "TRUEFALSE");
        assert_eq!(text("=CONCAT(1.5,2)"), "1.52");
    }

    #[test]
    fn concat_flattens_ranges() {
        let g = Grid::empty().col("A1", &[1.0, 2.0, 3.0]);
        assert_eq!(g.text("=CONCAT(A1:A3)"), "123");
        let g = Grid::empty().row("A1", &[1.0, 2.0, 3.0]);
        assert_eq!(g.text("=CONCAT(A1:C1)"), "123");
        let g = Grid::empty().set_text("A1", "a").set_text("C1", "c");
        assert_eq!(g.text("=CONCAT(A1:C1)"), "ac");
    }

    #[test]
    fn concatenate_does_not_flatten_ranges() {
        let g = Grid::empty().col("A1", &[1.0, 2.0, 3.0]);
        assert_eq!(g.text("=CONCATENATE(1,2,3)"), "123");
        assert_eq!(g.error("=CONCATENATE(A1:A3)"), CalcError::Value);
    }

    #[test]
    fn textjoin_joins_with_delimiter() {
        let g = Grid::empty().col("A1", &[1.0, 2.0, 3.0]);
        assert_eq!(g.text("=TEXTJOIN(\",\",TRUE,A1:A3)"), "1,2,3");
        assert_eq!(g.text("=TEXTJOIN(\"-\",FALSE,A1:A3)"), "1-2-3");
        assert_eq!(g.text("=TEXTJOIN(\",\",1,A1:A3)"), "1,2,3");
        assert_eq!(text("=TEXTJOIN(\",\",TRUE,\"a\",\"b\")"), "a,b");
        assert_eq!(text("=TEXTJOIN(\",\",TRUE,1,2)"), "1,2");
        assert_eq!(text("=TEXTJOIN(\",\",TRUE,0)"), "0");
    }

    #[test]
    fn textjoin_honours_ignore_empty() {
        let g = Grid::empty().set_text("A1", "a").set_text("A3", "c");
        assert_eq!(g.text("=TEXTJOIN(\",\",TRUE,A1:A3)"), "a,c");
        assert_eq!(g.text("=TEXTJOIN(\",\",FALSE,A1:A3)"), "a,,c");
        assert_eq!(text("=TEXTJOIN(\",\",TRUE,\"\")"), "");
        assert_eq!(text("=TEXTJOIN(\",\",FALSE,\"a\",\"\")"), "a,");
    }

    #[test]
    fn textjoin_and_concat_flatten_array_literals() {
        assert_eq!(text("=TEXTJOIN(\",\",TRUE,{1,2;3,4})"), "1,2,3,4");
        assert_eq!(text("=CONCAT({1,2;3,4})"), "1234");
    }

    #[test]
    fn errors() {
        assert_eq!(error("=CONCAT()"), CalcError::Value);
        assert_eq!(error("=CONCATENATE()"), CalcError::Value);
        assert_eq!(error("=TEXTJOIN(\",\",TRUE)"), CalcError::Value);
        assert_eq!(error("=CONCAT(1/0,\"x\")"), CalcError::Div0);
        assert_eq!(error("=CONCATENATE(1/0,\"x\")"), CalcError::Div0);
        assert_eq!(error("=TEXTJOIN(\",\",TRUE,1/0)"), CalcError::Div0);
    }
}

mod rept {
    use super::*;

    #[test]
    fn repeats_and_truncates_count() {
        assert_eq!(text("=REPT(\"ab\",3)"), "ababab");
        assert_eq!(text("=REPT(\"x\",0)"), "");
        assert_eq!(text("=REPT(\"ab\",2.9)"), "abab");
    }

    #[test]
    fn negative_count_is_value() {
        assert_eq!(error("=REPT(\"x\",-1)"), CalcError::Value);
    }

    #[test]
    fn longer_than_32767_is_value() {
        assert_eq!(error("=REPT(\"a\",40000)"), CalcError::Value);
        assert_eq!(error("=REPT(\"ab\",16384)"), CalcError::Value);
        assert_eq!(text("=REPT(\"a\",32767)").chars().count(), 32767);
        assert_eq!(text("=REPT(\"ab\",16383)").chars().count(), 32766);
    }

    #[test]
    fn errors() {
        assert_eq!(error("=REPT(\"a\")"), CalcError::Value);
        assert_eq!(error("=REPT(\"a\",\"x\")"), CalcError::Value);
        assert_eq!(error("=REPT(1/0,2)"), CalcError::Div0);
    }
}

mod exact_char_code_t {
    use super::*;

    #[test]
    fn exact_compares_case_sensitively_after_coercion() {
        assert_eq!(boolean("=EXACT(\"abc\",\"abc\")"), true);
        assert_eq!(boolean("=EXACT(\"abc\",\"ABC\")"), false);
        assert_eq!(boolean("=EXACT(1,\"1\")"), true);
        assert_eq!(boolean("=EXACT(1.5,\"1.5\")"), true);
        assert_eq!(boolean("=EXACT(TRUE,\"TRUE\")"), true);
        assert_eq!(boolean("=EXACT(\"a\",\"A\")"), false);
    }

    #[test]
    fn char_maps_the_ansi_code_page() {
        assert_eq!(text("=CHAR(65)"), "A");
        assert_eq!(text("=CHAR(65.9)"), "A");
        assert_eq!(text("=CHAR(128)"), "€");
        assert_eq!(text("=CHAR(255)"), "ÿ");
    }

    #[test]
    fn char_outside_1_to_255_is_value() {
        assert_eq!(error("=CHAR(0)"), CalcError::Value);
        assert_eq!(error("=CHAR(256)"), CalcError::Value);
        assert_eq!(error("=CHAR(-1)"), CalcError::Value);
        assert_eq!(error("=CHAR(300)"), CalcError::Value);
    }

    #[test]
    fn code_reads_the_first_character() {
        assert_eq!(num("=CODE(\"A\")"), 65.0);
        assert_eq!(num("=CODE(\"abc\")"), 97.0);
        assert_eq!(num("=CODE(\"€\")"), 128.0);
        assert_eq!(num("=CODE(\"中\")"), 20013.0);
        // a number coerces to text: CODE(65) is the code of "6"
        assert_eq!(num("=CODE(65)"), 54.0);
    }

    #[test]
    fn code_of_empty_string_is_value() {
        assert_eq!(error("=CODE(\"\")"), CalcError::Value);
    }

    #[test]
    fn t_returns_text_only() {
        assert_eq!(text("=T(\"hello\")"), "hello");
        assert_eq!(text("=T(123)"), "");
        assert_eq!(text("=T(TRUE)"), "");
        let g = Grid::empty();
        assert_eq!(g.text("=T(A1)"), "");
    }

    #[test]
    fn errors() {
        assert_eq!(error("=EXACT(\"a\")"), CalcError::Value);
        assert_eq!(error("=CHAR()"), CalcError::Value);
        assert_eq!(error("=CHAR(\"x\")"), CalcError::Value);
        assert_eq!(error("=CODE(1,2)"), CalcError::Value);
        assert_eq!(error("=T()"), CalcError::Value);
        assert_eq!(error("=EXACT(1/0,\"x\")"), CalcError::Div0);
        assert_eq!(error("=CHAR(1/0)"), CalcError::Div0);
        assert_eq!(error("=CODE(1/0)"), CalcError::Div0);
        assert_eq!(error("=T(1/0)"), CalcError::Div0);
        assert_eq!(error("=T(NA())"), CalcError::Na);
    }
}

mod date {
    use super::*;

    #[test]
    fn serial_anchors_including_the_phantom_leap_day() {
        assert_eq!(num("=DATE(1900,1,1)"), 1.0);
        assert_eq!(num("=DATE(1900,2,28)"), 59.0);
        assert_eq!(
            num("=DATE(1900,2,29)"),
            60.0,
            "serial 60 is the phantom 1900-02-29"
        );
        assert_eq!(num("=DATE(1900,3,1)"), 61.0);
        assert_eq!(num("=DATE(2024,3,1)"), 45352.0);
        assert_eq!(num("=DATE(2024,2,29)"), 45351.0);
        assert_eq!(num("=DATE(9999,12,31)"), 2958465.0);
    }

    #[test]
    fn round_trips_at_the_anchors() {
        assert_eq!(num("=YEAR(DATE(1900,1,1))"), 1900.0);
        assert_eq!(num("=MONTH(DATE(1900,1,1))"), 1.0);
        assert_eq!(num("=DAY(DATE(1900,1,1))"), 1.0);
        assert_eq!(num("=YEAR(DATE(1900,2,28))"), 1900.0);
        assert_eq!(num("=DAY(DATE(1900,2,28))"), 28.0);
        assert_eq!(num("=YEAR(DATE(1900,2,29))"), 1900.0);
        assert_eq!(num("=MONTH(DATE(1900,2,29))"), 2.0);
        assert_eq!(num("=DAY(DATE(1900,2,29))"), 29.0);
        assert_eq!(num("=DAY(DATE(1900,3,1))"), 1.0);
        assert_eq!(num("=YEAR(DATE(9999,12,31))"), 9999.0);
        assert_eq!(num("=MONTH(DATE(9999,12,31))"), 12.0);
        assert_eq!(num("=DAY(DATE(9999,12,31))"), 31.0);
    }

    #[test]
    fn out_of_range_arguments_roll_over() {
        assert_eq!(num("=DATE(2024,13,1)"), num("=DATE(2025,1,1)"));
        assert_eq!(num("=DATE(2024,0,1)"), num("=DATE(2023,12,1)"));
        assert_eq!(num("=DATE(2024,-1,1)"), num("=DATE(2023,11,1)"));
        assert_eq!(num("=DATE(2024,1,0)"), num("=DATE(2023,12,31)"));
        assert_eq!(num("=DATE(2024,1,32)"), num("=DATE(2024,2,1)"));
        assert_eq!(num("=DATE(2024,2,30)"), num("=DATE(2024,3,1)"));
        assert_eq!(num("=DATE(1900,2,30)"), num("=DATE(1900,3,1)"));
    }

    #[test]
    fn year_0_to_1899_is_shifted_into_the_1900s() {
        assert_eq!(num("=DATE(24,1,1)"), num("=DATE(1924,1,1)"));
        assert_eq!(num("=DATE(0,1,1)"), num("=DATE(1900,1,1)"));
        assert_eq!(num("=DATE(5,1,1)"), num("=DATE(1905,1,1)"));
    }

    #[test]
    fn arguments_are_truncated() {
        assert_eq!(num("=DATE(2024.9,3.9,1.9)"), num("=DATE(2024,3,1)"));
    }

    #[test]
    fn coercive_numeric_text_is_accepted() {
        assert_eq!(num("=DATE(\"2024\",\"3\",\"1\")"), num("=DATE(2024,3,1)"));
    }

    #[test]
    fn errors() {
        assert_eq!(error("=DATE(10000,1,1)"), CalcError::Num);
        assert_eq!(error("=DATE(-1,1,1)"), CalcError::Num);
        assert_eq!(error("=DATE(9999,12,32)"), CalcError::Num);
        assert_eq!(error("=DATE(\"x\",1,1)"), CalcError::Value);
        assert_eq!(error("=DATE(2024,1)"), CalcError::Value);
        assert_eq!(error("=DATE(2024,1,1,1)"), CalcError::Value);
        assert_eq!(error("=DATE(1/0,1,1)"), CalcError::Div0);
    }
}

mod year_month_day {
    use super::*;

    #[test]
    fn reads_a_modern_anchor_back_consistently() {
        // 2024-01-01 = 45292 and 2024-03-01 = 45352.
        assert_eq!(num("=YEAR(45292)"), 2024.0);
        assert_eq!(num("=MONTH(45292)"), 1.0);
        assert_eq!(num("=DAY(45292)"), 1.0);
        assert_eq!(num("=YEAR(45352)"), 2024.0);
        assert_eq!(num("=MONTH(45352)"), 3.0);
        assert_eq!(num("=DAY(45352)"), 1.0);
    }

    #[test]
    fn reads_the_phantom_day_and_the_max_serial() {
        assert_eq!(num("=YEAR(60)"), 1900.0);
        assert_eq!(num("=MONTH(60)"), 2.0);
        assert_eq!(num("=DAY(60)"), 29.0);
        assert_eq!(num("=DAY(61)"), 1.0);
        assert_eq!(num("=YEAR(2958465)"), 9999.0);
        assert_eq!(num("=MONTH(2958465)"), 12.0);
        assert_eq!(num("=DAY(2958465)"), 31.0);
    }

    #[test]
    fn fractional_serials_are_truncated() {
        assert_eq!(num("=YEAR(45352.75)"), 2024.0);
        assert_eq!(num("=MONTH(45352.75)"), 3.0);
        assert_eq!(num("=DAY(45352.75)"), 1.0);
    }

    #[test]
    fn coerces_numbers_booleans_and_numeric_text() {
        assert_eq!(num("=YEAR(TRUE)"), 1900.0);
        assert_eq!(num("=YEAR(\"45352\")"), num("=YEAR(45352)"));
        assert_eq!(num("=MONTH(\"45352\")"), num("=MONTH(45352)"));
        assert_eq!(num("=DAY(\"45352\")"), num("=DAY(45352)"));
    }

    #[test]
    fn serial_zero_reads_back_as_january_zero_in_excel() {
        // Excel: DATE(1900,1,0) is serial 0, read back as year 1900, month 1, day 0.
        // The engine maps serial 0 to the proleptic 1899-12-31 instead, so YEAR/MONTH/DAY
        // come back 1899/12/31. This is a genuine engine-vs-Excel discrepancy; the test
        // pins Excel's answer and is expected to fail until the engine agrees.
        assert_eq!(num("=DATE(1900,1,0)"), 0.0);
        assert_eq!(num("=YEAR(DATE(1900,1,0))"), 1900.0);
        assert_eq!(num("=MONTH(DATE(1900,1,0))"), 1.0);
        assert_eq!(num("=DAY(DATE(1900,1,0))"), 0.0);
    }

    #[test]
    fn errors() {
        assert_eq!(error("=YEAR(\"abc\")"), CalcError::Value);
        assert_eq!(error("=MONTH(\"abc\")"), CalcError::Value);
        assert_eq!(error("=DAY(\"abc\")"), CalcError::Value);
        assert_eq!(error("=YEAR()"), CalcError::Value);
        assert_eq!(error("=YEAR(1/0)"), CalcError::Div0);
        assert_eq!(error("=MONTH(1/0)"), CalcError::Div0);
        assert_eq!(error("=DAY(1/0)"), CalcError::Div0);
    }
}

mod time {
    use super::*;

    #[test]
    fn builds_fraction_of_day() {
        assert_eq!(num("=TIME(12,0,0)"), 0.5);
        assert_eq!(num("=TIME(6,0,0)"), 0.25);
        assert_eq!(num("=TIME(24,0,0)"), 0.0);
        assert_eq!(num("=TIME(0,0,0)"), 0.0);
        approx("=TIME(25,0,0)", 1.0 / 24.0, 1e-9);
        // Excel-COM measured: any negative TIME argument is #NUM! (not a wrap)
        assert_eq!(error("=TIME(-1,0,0)"), CalcError::Num);
        // hours above 24 wrap around to the same time of day
        assert_eq!(num("=HOUR(TIME(25,0,0))"), num("=HOUR(TIME(1,0,0))"));
    }

    #[test]
    fn reads_back_hours_minutes_seconds() {
        assert_eq!(num("=HOUR(TIME(23,59,59))"), 23.0);
        assert_eq!(num("=MINUTE(TIME(23,59,59))"), 59.0);
        assert_eq!(num("=SECOND(TIME(23,59,59))"), 59.0);
    }

    #[test]
    fn extracts_from_fractional_serials() {
        assert_eq!(num("=HOUR(0.5)"), 12.0);
        assert_eq!(num("=MINUTE(0.5)"), 0.0);
        assert_eq!(num("=SECOND(0.5)"), 0.0);
        assert_eq!(num("=HOUR(0.25)"), 6.0);
        assert_eq!(num("=HOUR(1.5)"), 12.0);
        assert_eq!(num("=HOUR(0)"), 0.0);
        assert_eq!(num("=MINUTE(0.99)"), 45.0);
        assert_eq!(num("=SECOND(0.99)"), 36.0);
    }

    #[test]
    fn coerces_and_errors() {
        assert_eq!(num("=HOUR(\"0.5\")"), 12.0);
        assert_eq!(num("=HOUR(TRUE)"), 0.0);
        assert_eq!(error("=HOUR(\"abc\")"), CalcError::Value);
        assert_eq!(error("=TIME(1,2)"), CalcError::Value);
        assert_eq!(error("=TIME(\"x\",0,0)"), CalcError::Value);
        assert_eq!(error("=TIME(1/0,0,0)"), CalcError::Div0);
        assert_eq!(error("=HOUR(1/0)"), CalcError::Div0);
        assert_eq!(error("=MINUTE(1/0)"), CalcError::Div0);
        assert_eq!(error("=SECOND(1/0)"), CalcError::Div0);
    }
}

mod weekday {
    use super::*;

    #[test]
    fn consecutive_serials_advance_by_one_and_wrap_after_seven() {
        let g = Grid::empty();
        for s in [1.0, 2.0, 3.0, 6.0, 7.0, 8.0, 60.0, 61.0, 45292.0, 45352.0] {
            let w = g.num(&format!("=WEEKDAY({s},1)"));
            assert!(
                (1.0..=7.0).contains(&w),
                "WEEKDAY({s},1) -> {w}, outside 1..7"
            );
            let next = g.num(&format!("=WEEKDAY({},1)", s + 1.0));
            assert_eq!(
                next,
                w % 7.0 + 1.0,
                "serial {s} weekday {w} must advance to {next} at serial {}",
                s + 1.0
            );
        }
    }

    #[test]
    fn return_types_1_2_3_relate_to_each_other() {
        let g = Grid::empty();
        for s in [1.0, 60.0, 61.0, 45292.0, 45352.0] {
            let w1 = g.num(&format!("=WEEKDAY({s},1)"));
            let w2 = g.num(&format!("=WEEKDAY({s},2)"));
            let w3 = g.num(&format!("=WEEKDAY({s},3)"));
            // type 2 shifts the week to start on Monday; type 3 shifts again to 0-based.
            assert_eq!(w2, (w1 + 5.0) % 7.0 + 1.0, "serial {s}");
            assert_eq!(w3, w2 - 1.0, "serial {s}");
        }
    }

    #[test]
    fn known_anchors() {
        // 1900-01-01 (serial 1) is a Sunday in Excel's 1900 system.
        assert_eq!(num("=WEEKDAY(1,1)"), 1.0);
        // 2024-01-01 was a Monday, 2024-03-01 a Friday.
        assert_eq!(num("=WEEKDAY(DATE(2024,1,1),1)"), 2.0);
        assert_eq!(num("=WEEKDAY(DATE(2024,3,1),1)"), 6.0);
        // 1904-01-01 is serial 0 in the 1904 system and was a Friday.
        let g = Grid::empty().with_1904();
        assert_eq!(g.num("=WEEKDAY(0,1)"), 6.0);
    }

    #[test]
    fn unsupported_return_type_is_num() {
        assert_eq!(error("=WEEKDAY(1,5)"), CalcError::Num);
        assert_eq!(error("=WEEKDAY(1,0)"), CalcError::Num);
        assert_eq!(error("=WEEKDAY(1,-1)"), CalcError::Num);
    }

    #[test]
    fn errors() {
        assert_eq!(error("=WEEKDAY(\"abc\")"), CalcError::Value);
        assert_eq!(error("=WEEKDAY(1,1,1)"), CalcError::Value);
        assert_eq!(error("=WEEKDAY(1/0)"), CalcError::Div0);
    }
}

mod edate_eomonth {
    use super::*;

    #[test]
    fn edate_adds_whole_months() {
        assert_eq!(num("=EDATE(DATE(2024,3,15),1)"), num("=DATE(2024,4,15)"));
        assert_eq!(num("=EDATE(DATE(2024,3,15),-1)"), num("=DATE(2024,2,15)"));
        assert_eq!(num("=EDATE(DATE(2024,3,15),12)"), num("=DATE(2025,3,15)"));
        assert_eq!(num("=EDATE(DATE(2024,3,15),-12)"), num("=DATE(2023,3,15)"));
    }

    #[test]
    fn edate_clamps_day_to_the_target_month() {
        // leap year: 2024-01-31 + 1 month clamps to 2024-02-29.
        assert_eq!(num("=EDATE(DATE(2024,1,31),1)"), num("=DATE(2024,2,29)"));
        // non-leap year: 2023-01-31 + 1 month clamps to 2023-02-28.
        assert_eq!(num("=EDATE(DATE(2023,1,31),1)"), num("=DATE(2023,2,28)"));
        // 2024-02-29 keeps its day: + 1 month -> 2024-03-29.
        assert_eq!(num("=EDATE(DATE(2024,2,29),1)"), num("=DATE(2024,3,29)"));
        // backward clamp: 2024-03-31 - 1 month -> 2024-02-29.
        assert_eq!(num("=EDATE(DATE(2024,3,31),-1)"), num("=DATE(2024,2,29)"));
        assert_eq!(num("=EDATE(DATE(2023,3,31),-1)"), num("=DATE(2023,2,28)"));
    }

    #[test]
    fn eomonth_returns_the_last_day_of_the_target_month() {
        assert_eq!(num("=EOMONTH(DATE(2024,3,15),0)"), num("=DATE(2024,3,31)"));
        assert_eq!(num("=EOMONTH(DATE(2024,1,31),1)"), num("=DATE(2024,2,29)"));
        assert_eq!(num("=EOMONTH(DATE(2023,1,31),1)"), num("=DATE(2023,2,28)"));
        assert_eq!(num("=EOMONTH(DATE(2024,3,15),-1)"), num("=DATE(2024,2,29)"));
        assert_eq!(num("=EOMONTH(DATE(2024,3,15),12)"), num("=DATE(2025,3,31)"));
        assert_eq!(num("=EOMONTH(DATE(2024,1,1),1)"), num("=DATE(2024,2,29)"));
    }

    #[test]
    fn errors() {
        assert_eq!(error("=EDATE(DATE(9999,12,31),1)"), CalcError::Num);
        assert_eq!(error("=EOMONTH(DATE(9999,12,31),1)"), CalcError::Num);
        assert_eq!(error("=EDATE(DATE(2024,1,1))"), CalcError::Value);
        assert_eq!(error("=EOMONTH(DATE(2024,1,1))"), CalcError::Value);
        assert_eq!(error("=EDATE(45292,\"x\")"), CalcError::Value);
        assert_eq!(error("=EOMONTH(45292,\"x\")"), CalcError::Value);
        assert_eq!(error("=EDATE(1/0,1)"), CalcError::Div0);
        assert_eq!(error("=EOMONTH(1/0,1)"), CalcError::Div0);
    }
}

mod datedif {
    use super::*;

    #[test]
    fn whole_month_units() {
        // 2024-01-01 -> 2024-03-01
        assert_eq!(num("=DATEDIF(DATE(2024,1,1),DATE(2024,3,1),\"D\")"), 60.0);
        assert_eq!(num("=DATEDIF(DATE(2024,1,1),DATE(2024,3,1),\"M\")"), 2.0);
        assert_eq!(num("=DATEDIF(DATE(2024,1,1),DATE(2024,3,1),\"Y\")"), 0.0);
        // 2024-01-01 -> 2025-01-01 (2024 is a leap year)
        assert_eq!(num("=DATEDIF(DATE(2024,1,1),DATE(2025,1,1),\"D\")"), 366.0);
        assert_eq!(num("=DATEDIF(DATE(2024,1,1),DATE(2025,1,1),\"M\")"), 12.0);
        assert_eq!(num("=DATEDIF(DATE(2024,1,1),DATE(2025,1,1),\"Y\")"), 1.0);
        // 2020-02-29 -> 2021-02-28
        assert_eq!(num("=DATEDIF(DATE(2020,2,29),DATE(2021,2,28),\"Y\")"), 0.0);
        assert_eq!(
            num("=DATEDIF(DATE(2020,2,29),DATE(2021,2,28),\"D\")"),
            365.0
        );
    }

    #[test]
    fn remaining_day_and_month_units() {
        // 2024-01-01 -> 2024-02-15
        assert_eq!(num("=DATEDIF(DATE(2024,1,1),DATE(2024,2,15),\"MD\")"), 14.0);
        assert_eq!(num("=DATEDIF(DATE(2024,1,1),DATE(2024,2,15),\"YM\")"), 1.0);
        // 2024-01-15 -> 2024-12-20
        assert_eq!(
            num("=DATEDIF(DATE(2024,1,15),DATE(2024,12,20),\"YM\")"),
            11.0
        );
        // 2024-01-01 -> 2024-03-01
        assert_eq!(num("=DATEDIF(DATE(2024,1,1),DATE(2024,3,1),\"MD\")"), 0.0);
        assert_eq!(num("=DATEDIF(DATE(2024,1,1),DATE(2024,3,1),\"YM\")"), 2.0);
        assert_eq!(num("=DATEDIF(DATE(2024,1,1),DATE(2024,3,1),\"YD\")"), 60.0);
    }

    #[test]
    fn unit_is_case_insensitive_and_trimmed() {
        assert_eq!(num("=DATEDIF(DATE(2024,1,1),DATE(2024,3,1),\"d\")"), 60.0);
        assert_eq!(num("=DATEDIF(DATE(2024,1,1),DATE(2024,3,1),\" m \")"), 2.0);
    }

    #[test]
    fn errors() {
        // end before start is NUM
        assert_eq!(
            error("=DATEDIF(DATE(2024,3,1),DATE(2024,1,1),\"D\")"),
            CalcError::Num
        );
        // unknown or empty units are NUM
        assert_eq!(
            error("=DATEDIF(DATE(2024,1,1),DATE(2024,3,1),\"X\")"),
            CalcError::Num
        );
        assert_eq!(
            error("=DATEDIF(DATE(2024,1,1),DATE(2024,3,1),\"\")"),
            CalcError::Num
        );
        assert_eq!(
            error("=DATEDIF(DATE(2024,1,1),DATE(2024,3,1),\"YYYY\")"),
            CalcError::Num
        );
        // arity and propagation
        assert_eq!(
            error("=DATEDIF(DATE(2024,1,1),DATE(2024,3,1))"),
            CalcError::Value
        );
        assert_eq!(error("=DATEDIF(1/0,DATE(2024,1,1),\"D\")"), CalcError::Div0);
        assert_eq!(error("=DATEDIF(DATE(2024,1,1),1/0,\"D\")"), CalcError::Div0);
    }
}

mod datevalue {
    use super::*;

    #[test]
    fn parses_the_unambiguous_iso_form() {
        assert_eq!(num("=DATEVALUE(\"2024-03-01\")"), 45352.0);
        assert_eq!(num("=DATEVALUE(\"1900-01-01\")"), 1.0);
        assert_eq!(num("=DATEVALUE(\"1900-02-29\")"), 60.0);
        assert_eq!(num("=DATEVALUE(\"9999-12-31\")"), 2958465.0);
        assert_eq!(num("=DATEVALUE(\" 2024-03-01 \")"), 45352.0);
        assert_eq!(num("=DATEVALUE(\"2024-03-01\")"), num("=DATE(2024,3,1)"));
        assert_eq!(num("=DATEVALUE(\"2024-02-29\")"), num("=DATE(2024,2,29)"));
        // unpadded ISO and year-first slash are unambiguous; a trailing time
        // contributes nothing to the serial (all Excel-COM measured)
        assert_eq!(num("=DATEVALUE(\"2024-3-1\")"), 45352.0);
        assert_eq!(num("=DATEVALUE(\"2024/03/01\")"), 45352.0);
        assert_eq!(num("=DATEVALUE(\"2020-01-02 13:14:15\")"), 43832.0);
    }

    #[test]
    fn refuses_ambiguous_or_otherwise_unsupported_forms() {
        for bad in [
            "03/01/2024",
            "03-01-2024",
            "2024-13-01",
            "2024-02-30",
            "hello",
            "",
        ] {
            assert_eq!(
                error(&format!("=DATEVALUE(\"{bad}\")")),
                CalcError::Value,
                "DATEVALUE({bad:?}) must refuse"
            );
        }
        // a number argument coerces to text that is not ISO, so it refuses too
        assert_eq!(error("=DATEVALUE(45352)"), CalcError::Value);
    }

    #[test]
    fn datevalue_in_the_1904_system() {
        let g = Grid::empty().with_1904();
        assert_eq!(
            g.num("=DATEVALUE(\"2024-03-01\")"),
            g.num("=DATE(2024,3,1)")
        );
        assert_eq!(g.num("=DATEVALUE(\"1904-01-01\")"), 0.0);
        // 1900-02-29 does not exist in the 1904 system
        assert_eq!(g.error("=DATEVALUE(\"1900-02-29\")"), CalcError::Value);
    }

    #[test]
    fn errors() {
        assert_eq!(error("=DATEVALUE()"), CalcError::Value);
        assert_eq!(error("=DATEVALUE(1/0)"), CalcError::Div0);
    }
}

mod days {
    use super::*;

    #[test]
    fn is_the_serial_difference() {
        assert_eq!(num("=DAYS(DATE(2024,3,1),DATE(2024,1,1))"), 60.0);
        assert_eq!(num("=DAYS(DATE(2024,1,1),DATE(2024,3,1))"), -60.0);
        assert_eq!(num("=DAYS(DATE(2024,3,1),DATE(2024,3,1))"), 0.0);
        assert_eq!(num("=DAYS(DATE(2024,3,1),DATE(2024,2,29))"), 1.0);
        // Excel truncates the time portions of both serials to integers.
        assert_eq!(num("=DAYS(45352.5,45292.25)"), 60.0);
        assert_eq!(num("=DAYS(43832.233,1)"), 43831.0);
    }

    #[test]
    fn errors() {
        assert_eq!(error("=DAYS(1)"), CalcError::Value);
        assert_eq!(error("=DAYS(1/0,45292)"), CalcError::Div0);
        assert_eq!(error("=DAYS(45292,\"x\")"), CalcError::Value);
    }
}

mod today_now {
    use super::*;

    #[test]
    fn today_is_the_floor_of_now_and_in_a_sane_range() {
        let today = num("=TODAY()");
        let now = num("=NOW()");
        assert!(
            today.fract() == 0.0,
            "TODAY must be an integer serial, got {today}"
        );
        // 2020-01-01 = 43831, 2100-01-01 = 73051
        assert!(
            (43831.0..73051.0).contains(&today),
            "TODAY {today} is outside 2020..2100"
        );
        // guard against the midnight boundary between the two volatile reads
        assert!(
            now >= today - 1.0 && now < today + 1.0,
            "NOW {now} is not within a day of TODAY {today}"
        );
        assert!(now.fract() >= 0.0 && now.fract() < 1.0);
    }

    #[test]
    fn the_1904_system_shifts_serials_relative_to_1900() {
        let g0 = Grid::empty();
        let g4 = Grid::empty().with_1904();
        assert_eq!(g4.num("=DATE(1904,1,1)"), 0.0);
        assert_eq!(
            g4.num("=DATE(2024,3,1)"),
            g0.num("=DATE(2024,3,1)") - 1462.0
        );
        // volatile, so allow a hair of clock skew between the two reads
        let d0 = g0.num("=TODAY()");
        let d4 = g4.num("=TODAY()");
        assert!(
            (d4 - (d0 - 1462.0)).abs() < 1.0,
            "TODAY shift: 1900 {d0}, 1904 {d4}"
        );
        let n0 = g0.num("=NOW()");
        let n4 = g4.num("=NOW()");
        assert!(
            (n4 - (n0 - 1462.0)).abs() < 1e-6,
            "NOW shift: 1900 {n0}, 1904 {n4}"
        );
        // the same real date has the same weekday in both systems
        assert_eq!(
            g4.num("=WEEKDAY(DATE(2024,3,1),1)"),
            g0.num("=WEEKDAY(DATE(2024,3,1),1)")
        );
    }

    #[test]
    fn errors() {
        assert_eq!(error("=TODAY(1)"), CalcError::Value);
        assert_eq!(error("=NOW(1)"), CalcError::Value);
    }
}

mod error_taxonomy {
    use super::*;

    #[test]
    fn division_by_zero() {
        assert_eq!(error("=1/0"), CalcError::Div0);
        assert_eq!(error("=LEFT(1/0,1)"), CalcError::Div0);
    }

    #[test]
    fn value_from_non_numeric_text() {
        assert_eq!(error("=1+\"abc\""), CalcError::Value);
        assert_eq!(error("=LEFT(\"abc\",\"x\")"), CalcError::Value);
    }

    #[test]
    fn num_domain_errors() {
        assert_eq!(error("=SQRT(-1)"), CalcError::Num);
        assert_eq!(error("=WEEKDAY(1,5)"), CalcError::Num);
        assert_eq!(error("=DATE(10000,1,1)"), CalcError::Num);
    }

    #[test]
    fn na() {
        assert_eq!(error("=NA()"), CalcError::Na);
        assert_eq!(error("=UPPER(NA())"), CalcError::Na);
    }

    #[test]
    fn ref_from_a_bad_sheet_name() {
        assert_eq!(error("=LEN(Nope!A1)"), CalcError::Ref);
        assert_eq!(error("=UPPER(Nope!A1)"), CalcError::Ref);
    }

    #[test]
    fn name_from_an_unknown_function() {
        assert_eq!(error("=NOSUCHFUNC(1)"), CalcError::Name);
        assert_eq!(error("=LEN(NOSUCHFUNC(1))"), CalcError::Name);
    }

    #[test]
    fn arity_too_few_and_too_many() {
        assert_eq!(error("=LEFT()"), CalcError::Value);
        assert_eq!(error("=LEFT(\"a\",1,1)"), CalcError::Value);
        assert_eq!(error("=MID(\"abc\",1)"), CalcError::Value);
        assert_eq!(error("=LEN()"), CalcError::Value);
        assert_eq!(error("=TEXT(1)"), CalcError::Value);
        assert_eq!(error("=DATE(2024,1)"), CalcError::Value);
        assert_eq!(error("=WEEKDAY(1,1,1)"), CalcError::Value);
        assert_eq!(error("=TIME(1,2)"), CalcError::Value);
        assert_eq!(error("=DAYS(1)"), CalcError::Value);
    }

    #[test]
    fn non_numeric_text_where_a_number_is_required() {
        assert_eq!(error("=LEFT(\"abc\",\"x\")"), CalcError::Value);
        assert_eq!(error("=REPT(\"a\",\"x\")"), CalcError::Value);
        assert_eq!(error("=CHAR(\"x\")"), CalcError::Value);
        assert_eq!(error("=DATE(\"x\",1,1)"), CalcError::Value);
        assert_eq!(error("=YEAR(\"abc\")"), CalcError::Value);
        assert_eq!(error("=HOUR(\"abc\")"), CalcError::Value);
        assert_eq!(error("=TIME(\"x\",0,0)"), CalcError::Value);
    }

    #[test]
    fn a_parse_failure_is_not_an_error_value() {
        assert_eq!(calc("=1+"), Outcome::ParseError);
        assert_eq!(calc("=LEFT("), Outcome::ParseError);
    }
}

mod error_propagation {
    use super::*;

    #[test]
    fn an_error_argument_comes_out_of_every_text_function_unchanged() {
        assert_eq!(error("=LEFT(1/0,1)"), CalcError::Div0);
        assert_eq!(error("=RIGHT(1/0,1)"), CalcError::Div0);
        assert_eq!(error("=MID(1/0,1,1)"), CalcError::Div0);
        assert_eq!(error("=LEN(1/0)"), CalcError::Div0);
        assert_eq!(error("=TRIM(1/0)"), CalcError::Div0);
        assert_eq!(error("=UPPER(1/0)"), CalcError::Div0);
        assert_eq!(error("=LOWER(1/0)"), CalcError::Div0);
        assert_eq!(error("=PROPER(1/0)"), CalcError::Div0);
        assert_eq!(error("=FIND(1/0,\"abc\")"), CalcError::Div0);
        assert_eq!(error("=FIND(\"a\",1/0)"), CalcError::Div0);
        assert_eq!(error("=SEARCH(\"a\",1/0)"), CalcError::Div0);
        assert_eq!(error("=SUBSTITUTE(1/0,\"a\",\"b\")"), CalcError::Div0);
        assert_eq!(error("=SUBSTITUTE(\"a\",1/0,\"b\")"), CalcError::Div0);
        assert_eq!(error("=REPLACE(1/0,1,1,\"x\")"), CalcError::Div0);
        assert_eq!(error("=VALUE(1/0)"), CalcError::Div0);
        assert_eq!(error("=TEXT(1/0,\"0\")"), CalcError::Div0);
        assert_eq!(error("=CONCAT(1/0,\"x\")"), CalcError::Div0);
        assert_eq!(error("=CONCATENATE(1/0,\"x\")"), CalcError::Div0);
        assert_eq!(error("=TEXTJOIN(\",\",TRUE,1/0)"), CalcError::Div0);
        assert_eq!(error("=REPT(1/0,2)"), CalcError::Div0);
        assert_eq!(error("=EXACT(1/0,\"x\")"), CalcError::Div0);
        assert_eq!(error("=CHAR(1/0)"), CalcError::Div0);
        assert_eq!(error("=CODE(1/0)"), CalcError::Div0);
        assert_eq!(error("=T(1/0)"), CalcError::Div0);
    }

    #[test]
    fn an_error_argument_comes_out_of_every_datetime_function_unchanged() {
        assert_eq!(error("=DATE(1/0,1,1)"), CalcError::Div0);
        assert_eq!(error("=YEAR(1/0)"), CalcError::Div0);
        assert_eq!(error("=MONTH(1/0)"), CalcError::Div0);
        assert_eq!(error("=DAY(1/0)"), CalcError::Div0);
        assert_eq!(error("=HOUR(1/0)"), CalcError::Div0);
        assert_eq!(error("=MINUTE(1/0)"), CalcError::Div0);
        assert_eq!(error("=SECOND(1/0)"), CalcError::Div0);
        assert_eq!(error("=TIME(1/0,0,0)"), CalcError::Div0);
        assert_eq!(error("=WEEKDAY(1/0)"), CalcError::Div0);
        assert_eq!(error("=EDATE(1/0,1)"), CalcError::Div0);
        assert_eq!(error("=EOMONTH(1/0,1)"), CalcError::Div0);
        assert_eq!(error("=DATEDIF(1/0,DATE(2024,1,1),\"D\")"), CalcError::Div0);
        assert_eq!(error("=DATEVALUE(1/0)"), CalcError::Div0);
        assert_eq!(error("=DAYS(1/0,DATE(2024,1,1))"), CalcError::Div0);
    }

    #[test]
    fn na_propagates_through() {
        assert_eq!(error("=UPPER(NA())"), CalcError::Na);
        assert_eq!(error("=LEFT(NA(),1)"), CalcError::Na);
        assert_eq!(error("=YEAR(NA())"), CalcError::Na);
        assert_eq!(error("=T(NA())"), CalcError::Na);
    }

    #[test]
    fn value_errors_propagate_through() {
        assert_eq!(error("=UPPER(1+\"abc\")"), CalcError::Value);
        assert_eq!(error("=YEAR(1+\"abc\")"), CalcError::Value);
    }

    #[test]
    fn error_handlers_are_the_only_absorbers() {
        assert_eq!(boolean("=ISERROR(1/0)"), true);
        assert_eq!(boolean("=ISERROR(\"x\")"), false);
    }
}
