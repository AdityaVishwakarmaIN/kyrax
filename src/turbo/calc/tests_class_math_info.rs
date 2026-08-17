// tests_class_math_info.rs — class test matrix for the math and information
// function families: SUM PRODUCT AVERAGE COUNT COUNTA MIN MAX, ABS INT MOD
// POWER SQRT ROUND ROUNDUP ROUNDDOWN, SUMIF SUMIFS COUNTIF COUNTIFS AVERAGEIF
// AVERAGEIFS, and ISBLANK ISNUMBER ISTEXT ISNONTEXT ISLOGICAL ISERROR ISERR
// ISNA ISREF ISEVEN ISODD NA N TYPE ERROR.TYPE ROW COLUMN ROWS COLUMNS.
//
// Every assertion drives the REAL pipeline (`parse_formula` then `eval`) via
// the shared testkit — never a function pointer directly. Each mod covers the
// four things the class matrix demands: normal results, coercion and edge
// inputs, the exact CalcError for each failure mode, and error propagation
// (unless the function is defined to absorb the error).
//
// Two Excel rules this suite pins down hard:
//   * an aggregate over a RANGE ignores text, blanks and booleans, while a
//     direct scalar argument is coerced;
//   * the IS predicates classify by KIND, not content, so an empty string is
//     not blank and numeric text is not a number.

#![cfg(test)]
#![allow(clippy::approx_constant)]

use super::testkit::{Grid, Outcome, approx, boolean, calc, error, num, text};
use crate::turbo::calc::value::{CalcError, CalcValue};

#[cfg(test)]
mod aggregates {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn sum_normal_and_variadic() {
        assert_eq!(num("=SUM(1,2,3)"), 6.0);
        assert_eq!(num("=SUM()"), 0.0, "SUM accepts zero arguments");
        let g = Grid::empty().col("A1", &[1.0, 2.0, 3.0]);
        assert_eq!(g.num("=SUM(A1:A3)"), 6.0);
        assert_eq!(g.num("=SUM(A1:A3,10)"), 16.0, "range plus scalar");
    }

    #[test]
    fn sum_range_ignores_text_blank_bool_but_scalars_coerce() {
        let g = Grid::empty()
            .set_num("A1", 1.0)
            .set_num("A2", 2.0)
            .set_text("A3", "5")
            .set_bool("A5", true);
        // A4 is left blank.
        assert_eq!(g.num("=SUM(A1:A5)"), 3.0, "a range ignores text/blank/bool");
        // The same values passed as literals follow the scalar coercion rule.
        assert_eq!(g.num("=SUM(1,2,\"5\",TRUE)"), 9.0);
        assert_eq!(num("=SUM(1,2,\"5\",TRUE)"), 9.0);
        // A text cell is ignored in a range, but a text literal is coerced.
        assert_eq!(g.num("=SUM(A1:A3)"), 3.0);
        assert_eq!(num("=SUM(\"7\")"), 7.0);
    }

    #[test]
    fn sum_errors() {
        assert_eq!(
            error("=SUM(\"abc\")"),
            CalcError::Value,
            "non-numeric scalar text"
        );
        assert_eq!(error("=SUM(1,\"abc\")"), CalcError::Value);
        let g = Grid::empty().set("A2", CalcValue::err(CalcError::Na));
        assert_eq!(
            g.error("=SUM(A1:A3)"),
            CalcError::Na,
            "an error in a range propagates"
        );
    }

    #[test]
    fn product_normal_and_edge() {
        assert_eq!(num("=PRODUCT(2,3,4)"), 24.0);
        let g = Grid::empty().col("A1", &[2.0, 3.0, 4.0]);
        assert_eq!(g.num("=PRODUCT(A1:A3)"), 24.0);
        let g2 = Grid::empty().set_text("B1", "x").set_bool("B2", true);
        assert_eq!(g2.num("=PRODUCT(B1:B3)"), 0.0, "no numeric cells -> 0");
    }

    #[test]
    fn product_errors() {
        assert_eq!(error("=PRODUCT()"), CalcError::Value);
        assert_eq!(error("=PRODUCT(\"abc\",2)"), CalcError::Value);
        assert_eq!(error("=PRODUCT(2,1/0)"), CalcError::Div0, "propagates");
    }

    #[test]
    fn average_normal() {
        approx("=AVERAGE(1,2)", 1.5, 1e-12);
        assert_eq!(num("=AVERAGE(1,2,3)"), 2.0);
        let g = Grid::empty().col("A1", &[10.0, 20.0, 30.0]);
        assert_eq!(g.num("=AVERAGE(A1:A3)"), 20.0);
    }

    #[test]
    fn average_range_ignores_text_blank_bool() {
        let g = Grid::empty()
            .set_num("A1", 1.0)
            .set_num("A2", 2.0)
            .set_text("A3", "5")
            .set_bool("A5", true);
        assert_eq!(
            g.num("=AVERAGE(A1:A5)"),
            1.5,
            "averages only the two numbers"
        );
        approx("=AVERAGE(1,2,\"5\")", 8.0 / 3.0, 1e-12);
    }

    #[test]
    fn average_no_numeric_cells_is_div0() {
        let g = Grid::empty().set_text("A1", "x").set_bool("A2", true);
        assert_eq!(g.error("=AVERAGE(A1:A2)"), CalcError::Div0);
        assert_eq!(
            Grid::empty().error("=AVERAGE(A1:A5)"),
            CalcError::Div0,
            "all blank"
        );
    }

    #[test]
    fn count_vs_counta() {
        let g = Grid::empty()
            .set_num("A1", 1.0)
            .set_num("A2", 2.0)
            .set_text("A3", "x")
            .set_bool("A4", true);
        assert_eq!(g.num("=COUNT(A1:A4)"), 2.0, "COUNT sees numbers only");
        assert_eq!(
            g.num("=COUNTA(A1:A4)"),
            4.0,
            "COUNTA sees anything non-blank"
        );
        assert_eq!(Grid::empty().num("=COUNT(A1:A5)"), 0.0);
        assert_eq!(Grid::empty().num("=COUNTA(A1:A5)"), 0.0);
    }

    #[test]
    fn count_and_counta_scalar_coercion() {
        assert_eq!(num("=COUNT(1,\"2\",TRUE)"), 3.0, "scalar args are coerced");
        assert_eq!(num("=COUNTA(1,\"x\",TRUE)"), 3.0);
        // Excel-measured: =COUNT("abc") is 0 — untranslatable scalar text is
        // not counted (no #VALUE!).
        assert_eq!(num("=COUNT(\"abc\")"), 0.0, "non-numeric text is skipped");
    }

    #[test]
    fn min_max_normal_and_empty() {
        let g = Grid::empty().col("A1", &[5.0, 2.0, 9.0]);
        assert_eq!(g.num("=MIN(A1:A3)"), 2.0);
        assert_eq!(g.num("=MAX(A1:A3)"), 9.0);
        assert_eq!(
            Grid::empty().num("=MIN(A1:A5)"),
            0.0,
            "no numeric cells -> 0"
        );
        assert_eq!(Grid::empty().num("=MAX(A1:A5)"), 0.0);
    }

    #[test]
    fn min_max_ignore_text_blank_bool_in_ranges() {
        let g = Grid::empty()
            .set_num("A1", 5.0)
            .set_text("A2", "100")
            .set_bool("A3", true)
            .set_num("A4", -3.0);
        assert_eq!(g.num("=MIN(A1:A4)"), -3.0, "the text \"100\" is not 100");
        assert_eq!(g.num("=MAX(A1:A4)"), 5.0);
    }

    #[test]
    fn min_max_coerce_scalar_args() {
        assert_eq!(num("=MIN(TRUE,3)"), 1.0, "a scalar boolean coerces to 1");
        assert_eq!(num("=MAX(\"5\",3)"), 5.0, "a scalar text literal coerces");
        assert_eq!(error("=MIN(\"abc\")"), CalcError::Value);
        assert_eq!(error("=MAX(\"abc\")"), CalcError::Value);
    }

    #[test]
    fn aggregate_propagate_error_arguments() {
        for f in ["SUM", "PRODUCT", "AVERAGE", "MIN", "MAX"] {
            assert_eq!(
                error(&format!("={f}(1/0)")),
                CalcError::Div0,
                "{f} must propagate an error argument"
            );
        }
        // Excel-measured: =COUNT(1/0) is 0 — COUNT skips computed errors
        // rather than propagating them (like =COUNT(#N/A) and errors in
        // ranges).
        assert_eq!(num("=COUNT(1/0)"), 0.0, "COUNT skips error arguments");
        assert_eq!(num("=COUNT({1,#N/A,3})"), 2.0, "array errors are skipped");
        // COUNTA counts an error value as a non-blank cell rather than
        // propagating it — matching Excel.
        assert_eq!(num("=COUNTA(1/0)"), 1.0);
        let g = Grid::empty()
            .set_num("A1", 1.0)
            .set("A2", CalcValue::err(CalcError::Na))
            .set_num("A3", 3.0);
        assert_eq!(g.num("=COUNTA(A1:A3)"), 3.0, "an error cell is not blank");
    }

    #[test]
    fn aggregate_arity_errors() {
        assert_eq!(error("=PRODUCT()"), CalcError::Value);
        assert_eq!(error("=AVERAGE()"), CalcError::Value);
        assert_eq!(error("=COUNT()"), CalcError::Value);
        assert_eq!(error("=COUNTA()"), CalcError::Value);
        assert_eq!(error("=MIN()"), CalcError::Value);
        assert_eq!(error("=MAX()"), CalcError::Value);
        // SUM is the one variadic-from-zero aggregate.
        assert_eq!(num("=SUM()"), 0.0);
        assert_eq!(num("=SUM(1,2,3,4,5)"), 15.0);
    }
}

#[cfg(test)]
mod abs_int {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn abs_normal() {
        assert_eq!(num("=ABS(3.5)"), 3.5);
        assert_eq!(num("=ABS(-3.5)"), 3.5);
        assert_eq!(num("=ABS(0)"), 0.0);
        let g = Grid::empty().set_num("A1", -7.0);
        assert_eq!(g.num("=ABS(A1)"), 7.0);
    }

    #[test]
    fn abs_coercion_and_errors() {
        assert_eq!(num("=ABS(\"-4\")"), 4.0, "numeric text coerces");
        assert_eq!(num("=ABS(TRUE)"), 1.0, "a boolean coerces");
        assert_eq!(error("=ABS(\"abc\")"), CalcError::Value);
        assert_eq!(error("=ABS()"), CalcError::Value, "too few arguments");
        assert_eq!(error("=ABS(1,2)"), CalcError::Value, "too many arguments");
        assert_eq!(error("=ABS(1/0)"), CalcError::Div0, "propagates");
        assert_eq!(error("=ABS(#N/A)"), CalcError::Na, "propagates");
        assert_eq!(error("=ABS(Nope!A1)"), CalcError::Ref, "bad sheet name");
        assert_eq!(
            error("=ABS(NOSUCHFUNC(1))"),
            CalcError::Name,
            "unknown function"
        );
    }

    #[test]
    fn int_floors_toward_negative_infinity() {
        assert_eq!(num("=INT(2.5)"), 2.0);
        assert_eq!(num("=INT(-2.5)"), -3.0, "floor, not truncation");
        assert_eq!(num("=INT(1.9)"), 1.0);
        assert_eq!(num("=INT(-1.9)"), -2.0);
        assert_eq!(num("=INT(3)"), 3.0);
    }

    #[test]
    fn int_coercion_and_errors() {
        assert_eq!(num("=INT(\"2.9\")"), 2.0, "numeric text coerces");
        assert_eq!(error("=INT(\"abc\")"), CalcError::Value);
        assert_eq!(error("=INT()"), CalcError::Value);
        assert_eq!(error("=INT(1,2)"), CalcError::Value);
        assert_eq!(error("=INT(1/0)"), CalcError::Div0, "propagates");
    }
}

#[cfg(test)]
mod mod_power_sqrt {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn mod_normal_and_sign_of_divisor() {
        assert_eq!(num("=MOD(3,2)"), 1.0);
        assert_eq!(
            num("=MOD(-3,2)"),
            1.0,
            "sign follows the divisor, so this is +1"
        );
        assert_eq!(num("=MOD(3,-2)"), -1.0);
        assert_eq!(num("=MOD(-3,-2)"), -1.0);
        assert_eq!(num("=MOD(10,3)"), 1.0);
        assert_eq!(num("=MOD(-10,3)"), 2.0);
        assert_eq!(num("=MOD(10,-3)"), -2.0);
    }

    #[test]
    fn mod_errors() {
        assert_eq!(
            error("=MOD(3,0)"),
            CalcError::Div0,
            "modulo zero is #DIV/0!"
        );
        assert_eq!(error("=MOD(\"abc\",2)"), CalcError::Value);
        assert_eq!(error("=MOD(3,\"abc\")"), CalcError::Value);
        assert_eq!(error("=MOD(1)"), CalcError::Value);
        assert_eq!(error("=MOD(1,2,3)"), CalcError::Value);
        assert_eq!(error("=MOD(1/0,2)"), CalcError::Div0, "propagates");
    }

    #[test]
    fn power_normal() {
        assert_eq!(num("=POWER(2,3)"), 8.0);
        assert_eq!(num("=POWER(2,-1)"), 0.5);
        assert_eq!(
            num("=POWER(-8,3)"),
            -512.0,
            "an integer exponent on a negative base"
        );
        assert_eq!(num("=POWER(4,0.5)"), 2.0);
        assert_eq!(num("=POWER(0,2)"), 0.0);
        assert_eq!(num("=POWER(\"2\",3)"), 8.0, "numeric text coerces");
    }

    #[test]
    fn power_domain_errors() {
        assert_eq!(
            error("=POWER(-2,0.5)"),
            CalcError::Num,
            "a negative base and a fractional exponent is #NUM!"
        );
        assert_eq!(
            error("=POWER(0,-1)"),
            CalcError::Div0,
            "zero to a negative power"
        );
        assert_eq!(error("=POWER(\"abc\",2)"), CalcError::Value);
        assert_eq!(error("=POWER(1)"), CalcError::Value);
        assert_eq!(error("=POWER(1,2,3)"), CalcError::Value);
        assert_eq!(error("=POWER(1/0,2)"), CalcError::Div0, "propagates");
    }

    #[test]
    fn sqrt_normal_and_domain() {
        assert_eq!(num("=SQRT(9)"), 3.0);
        assert_eq!(num("=SQRT(0)"), 0.0);
        assert_eq!(num("=SQRT(2)"), 2f64.sqrt());
        assert_eq!(num("=SQRT(\"16\")"), 4.0, "numeric text coerces");
        assert_eq!(
            error("=SQRT(-1)"),
            CalcError::Num,
            "a negative square root is #NUM!"
        );
        assert_eq!(error("=SQRT(\"abc\")"), CalcError::Value);
        assert_eq!(error("=SQRT()"), CalcError::Value);
        assert_eq!(error("=SQRT(1/0)"), CalcError::Div0, "propagates");
    }
}

#[cfg(test)]
mod round_family {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn round_is_half_away_from_zero() {
        assert_eq!(num("=ROUND(2.4,0)"), 2.0);
        assert_eq!(num("=ROUND(2.5,0)"), 3.0);
        assert_eq!(num("=ROUND(2.6,0)"), 3.0);
        assert_eq!(num("=ROUND(-2.4,0)"), -2.0);
        assert_eq!(num("=ROUND(-2.5,0)"), -3.0);
    }

    #[test]
    fn round_negative_digits_round_to_tens_and_hundreds() {
        assert_eq!(num("=ROUND(155,-1)"), 160.0);
        assert_eq!(num("=ROUND(155,-2)"), 200.0);
        assert_eq!(num("=ROUND(1234,-2)"), 1200.0);
        assert_eq!(num("=ROUND(-155,-1)"), -160.0);
    }

    #[test]
    fn round_fractional_digits() {
        assert_eq!(num("=ROUND(2.346,2)"), 2.35);
        assert_eq!(num("=ROUND(3.14159,3)"), 3.142);
    }

    #[test]
    fn round_coercion_and_errors() {
        assert_eq!(num("=ROUND(\"2.5\",0)"), 3.0, "numeric text coerces");
        assert_eq!(error("=ROUND(\"abc\",0)"), CalcError::Value);
        assert_eq!(error("=ROUND(1,\"abc\")"), CalcError::Value);
        assert_eq!(error("=ROUND(1)"), CalcError::Value);
        assert_eq!(error("=ROUND(1,2,3)"), CalcError::Value);
        assert_eq!(error("=ROUND(1/0,1)"), CalcError::Div0, "propagates");
    }

    #[test]
    fn roundup_is_away_from_zero() {
        assert_eq!(num("=ROUNDUP(2.11,1)"), 2.2);
        assert_eq!(
            num("=ROUNDUP(-2.11,1)"),
            -2.2,
            "a negative rounds further from zero"
        );
        assert_eq!(num("=ROUNDUP(2.0,0)"), 2.0);
        assert_eq!(num("=ROUNDUP(155,-1)"), 160.0);
    }

    #[test]
    fn rounddown_is_toward_zero() {
        assert_eq!(num("=ROUNDDOWN(2.19,1)"), 2.1);
        assert_eq!(
            num("=ROUNDDOWN(-2.19,1)"),
            -2.1,
            "a negative rounds toward zero"
        );
        assert_eq!(num("=ROUNDDOWN(2.0,0)"), 2.0);
        assert_eq!(num("=ROUNDDOWN(155,-1)"), 150.0);
    }

    #[test]
    fn int_versus_rounddown_for_negatives() {
        assert_eq!(num("=INT(-2.5)"), -3.0, "INT floors toward -infinity");
        assert_eq!(
            num("=ROUNDDOWN(-2.5,0)"),
            -2.0,
            "ROUNDDOWN truncates toward zero"
        );
    }

    #[test]
    fn roundup_rounddown_errors() {
        assert_eq!(error("=ROUNDUP(\"abc\",1)"), CalcError::Value);
        assert_eq!(error("=ROUNDDOWN(1,\"abc\")"), CalcError::Value);
        assert_eq!(error("=ROUNDUP(1)"), CalcError::Value);
        assert_eq!(error("=ROUNDDOWN()"), CalcError::Value);
        assert_eq!(error("=ROUNDUP(1/0,1)"), CalcError::Div0, "propagates");
        assert_eq!(error("=ROUNDDOWN(1,1/0)"), CalcError::Div0, "propagates");
    }
}

#[cfg(test)]
mod conditional {
    use super::*;
    use pretty_assertions::assert_eq;

    fn data() -> Grid {
        Grid::empty()
            .col("A1", &[1.0, 5.0, 6.0, 10.0])
            .col("B1", &[10.0, 20.0, 30.0, 40.0])
    }

    #[test]
    fn sumif_criteria_kinds() {
        let g = data();
        assert_eq!(g.num("=SUMIF(A1:A4,5)"), 5.0, "bare value criterion");
        assert_eq!(g.num("=SUMIF(A1:A4,\">5\")"), 16.0, "comparison string");
        assert_eq!(g.num("=SUMIF(A1:A4,\">=6\")"), 16.0);
        assert_eq!(g.num("=SUMIF(A1:A4,\"<>5\")"), 17.0);
        assert_eq!(g.num("=SUMIF(A1:A4,5,B1:B4)"), 20.0, "separate sum range");
        assert_eq!(g.num("=SUMIF(A1:A4,\">5\",B1:B4)"), 70.0);
    }

    #[test]
    fn sumif_no_match_is_zero() {
        let g = data();
        assert_eq!(g.num("=SUMIF(A1:A4,\">100\")"), 0.0);
    }

    #[test]
    fn sumif_shape_mismatch_is_value() {
        let g = Grid::empty()
            .col("A1", &[1.0, 2.0, 3.0])
            .col("B1", &[1.0, 2.0]);
        assert_eq!(
            g.error("=SUMIF(A1:A3,\">0\",B1:B2)"),
            CalcError::Value,
            "a criteria range of a different shape is #VALUE!"
        );
    }

    #[test]
    fn sumif_errors() {
        assert_eq!(
            error("=SUMIF(A1:A2)"),
            CalcError::Value,
            "too few arguments"
        );
        assert_eq!(
            error("=SUMIF(A1:A4,\">0\",B1:B4,C1)"),
            CalcError::Value,
            "too many arguments"
        );
    }

    #[test]
    fn countif_normal() {
        let g = Grid::empty()
            .set_text("A1", "apple")
            .set_text("A2", "apricot")
            .set_text("A3", "banana")
            .set_text("A4", "apple");
        assert_eq!(g.num("=COUNTIF(A1:A4,\"apple\")"), 2.0);
        assert_eq!(g.num("=COUNTIF(A1:A4,\"ap*\")"), 3.0, "wildcard star");
        assert_eq!(
            g.num("=COUNTIF(A1:A4,\"a?ple\")"),
            2.0,
            "wildcard question mark"
        );
        assert_eq!(g.num("=COUNTIF(A1:A4,\"banana\")"), 1.0);
        assert_eq!(g.num("=COUNTIF(A1:A4,\"<>banana\")"), 3.0);
        assert_eq!(g.num("=COUNTIF(A1:A4,\"*\")"), 4.0, "anything text");
    }

    #[test]
    fn countif_numeric_criteria() {
        let g = data();
        assert_eq!(g.num("=COUNTIF(A1:A4,5)"), 1.0, "bare value");
        assert_eq!(g.num("=COUNTIF(A1:A4,\">5\")"), 2.0);
        assert_eq!(g.num("=COUNTIF(A1:A4,\">=1\")"), 4.0);
        assert_eq!(g.num("=COUNTIF(A1:A4,\">100\")"), 0.0, "no match -> 0");
    }

    #[test]
    fn averageif_normal_and_div0() {
        let g = data();
        assert_eq!(g.num("=AVERAGEIF(A1:A4,\">5\")"), 8.0);
        assert_eq!(g.num("=AVERAGEIF(A1:A4,\">=5\",B1:B4)"), 30.0);
        assert_eq!(
            g.error("=AVERAGEIF(A1:A4,\">100\")"),
            CalcError::Div0,
            "no matching cells is #DIV/0!"
        );
    }

    #[test]
    fn averageif_errors() {
        assert_eq!(error("=AVERAGEIF(A1:A2)"), CalcError::Value);
        let g = Grid::empty()
            .col("A1", &[1.0, 2.0, 3.0])
            .col("B1", &[1.0, 2.0]);
        assert_eq!(
            g.error("=AVERAGEIF(A1:A3,\">0\",B1:B2)"),
            CalcError::Value,
            "shape mismatch"
        );
    }

    #[test]
    fn sumifs_multiple_criteria() {
        let g = Grid::empty()
            .col("A1", &[1.0, 2.0, 3.0, 4.0])
            .col("B1", &[10.0, 20.0, 30.0, 40.0])
            .col("C1", &[1.0, 0.0, 1.0, 0.0]);
        assert_eq!(g.num("=SUMIFS(A1:A4,B1:B4,\">15\",C1:C4,1)"), 3.0);
        assert_eq!(g.num("=SUMIFS(A1:A4,B1:B4,\">100\")"), 0.0, "no match -> 0");
    }

    #[test]
    fn sumifs_errors() {
        assert_eq!(
            error("=SUMIFS(A1:A2)"),
            CalcError::Value,
            "too few arguments"
        );
        let g = Grid::empty().col("A1", &[1.0, 2.0, 3.0]);
        assert_eq!(
            g.error("=SUMIFS(A1:A3,B1:B3,\">0\",C1:C3)"),
            CalcError::Value,
            "an odd trailing pair count is #VALUE!"
        );
        assert_eq!(
            g.error("=SUMIFS(A1:A3,B1:B3,\">0\",C1:C2,\">0\")"),
            CalcError::Value,
            "mismatched criteria shapes"
        );
    }

    #[test]
    fn countifs_multiple_criteria() {
        let g = Grid::empty()
            .set_text("A1", "a")
            .set_num("B1", 1.0)
            .set_text("A2", "b")
            .set_num("B2", 2.0)
            .set_text("A3", "a")
            .set_num("B3", 3.0);
        assert_eq!(g.num("=COUNTIFS(A1:A3,\"a\",B1:B3,\">1\")"), 1.0);
        assert_eq!(g.num("=COUNTIFS(A1:A3,\"a\")"), 2.0);
        assert_eq!(g.num("=COUNTIFS(A1:A3,\"b\",B1:B3,\">5\")"), 0.0);
    }

    #[test]
    fn countifs_errors() {
        assert_eq!(
            error("=COUNTIFS(A1:A3)"),
            CalcError::Value,
            "an odd argument count"
        );
        let g = Grid::empty()
            .col("A1", &[1.0, 2.0, 3.0])
            .col("B1", &[1.0, 2.0]);
        assert_eq!(
            g.error("=COUNTIFS(A1:A3,\">0\",B1:B2,\">0\")"),
            CalcError::Value,
            "mismatched criteria shapes"
        );
    }

    #[test]
    fn averageifs_normal_and_div0() {
        let g = Grid::empty()
            .col("A1", &[1.0, 2.0, 3.0, 4.0])
            .col("B1", &[10.0, 20.0, 30.0, 40.0]);
        assert_eq!(g.num("=AVERAGEIFS(A1:A4,B1:B4,\">15\")"), 3.0);
        assert_eq!(
            g.error("=AVERAGEIFS(A1:A4,B1:B4,\">100\")"),
            CalcError::Div0,
            "no matching cells is #DIV/0!"
        );
    }

    #[test]
    fn averageifs_errors() {
        assert_eq!(
            error("=AVERAGEIFS(A1:A2)"),
            CalcError::Value,
            "too few arguments"
        );
        let g = Grid::empty().col("A1", &[1.0, 2.0, 3.0]);
        assert_eq!(
            g.error("=AVERAGEIFS(A1:A3,B1:B3,\">0\",C1:C3)"),
            CalcError::Value,
            "an odd trailing pair count is #VALUE!"
        );
    }

    #[test]
    fn conditional_propagate_range_errors() {
        let g = Grid::empty()
            .set_num("A1", 1.0)
            .set("A2", CalcValue::err(CalcError::Na))
            .set_num("A3", 3.0);
        assert_eq!(g.error("=SUMIF(A1:A3,\">0\")"), CalcError::Na);
        assert_eq!(g.error("=COUNTIF(A1:A3,\">0\")"), CalcError::Na);
        assert_eq!(g.error("=AVERAGEIF(A1:A3,\">0\")"), CalcError::Na);
    }
}

#[cfg(test)]
mod is_predicates {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn isblank_distinguishes_blank_from_empty_text() {
        assert_eq!(boolean("=ISBLANK(A1)"), true, "an empty cell is blank");
        let g = Grid::empty().set_text("A1", "");
        assert_eq!(
            g.boolean("=ISBLANK(A1)"),
            false,
            "an empty string is NOT a blank cell"
        );
        let g = Grid::empty().set_num("A1", 0.0);
        assert_eq!(g.boolean("=ISBLANK(A1)"), false);
    }

    #[test]
    fn predicates_classify_by_kind_not_content() {
        assert_eq!(boolean("=ISNUMBER(1)"), true);
        assert_eq!(
            boolean("=ISNUMBER(\"1\")"),
            false,
            "numeric text is text, not a number"
        );
        assert_eq!(
            boolean("=ISNUMBER(TRUE)"),
            false,
            "a boolean is not a number"
        );
        assert_eq!(boolean("=ISTEXT(\"x\")"), true);
        assert_eq!(boolean("=ISTEXT(1)"), false);
        assert_eq!(boolean("=ISTEXT(TRUE)"), false);
        assert_eq!(boolean("=ISNONTEXT(1)"), true);
        assert_eq!(boolean("=ISNONTEXT(\"x\")"), false);
        assert_eq!(boolean("=ISNONTEXT(TRUE)"), true);
        assert_eq!(boolean("=ISLOGICAL(TRUE)"), true);
        assert_eq!(boolean("=ISLOGICAL(FALSE)"), true);
        assert_eq!(boolean("=ISLOGICAL(1)"), false);
        assert_eq!(boolean("=ISLOGICAL(\"TRUE\")"), false);
    }

    #[test]
    fn predicates_read_cell_kinds_from_the_grid() {
        let g = Grid::empty().set_num("A1", 42.0).set_text("A2", "42");
        assert_eq!(g.boolean("=ISNUMBER(A1)"), true);
        assert_eq!(g.boolean("=ISNUMBER(A2)"), false);
        assert_eq!(g.boolean("=ISTEXT(A2)"), true);
        assert_eq!(g.boolean("=ISBLANK(A3)"), true);
    }

    #[test]
    fn predicates_scalar_pick_array_arguments() {
        // Excel COM referee: =ISNUMBER({1," ",1.23,TRUE,FALSE,"",#N/A,#DIV/0!,
        // #SPILL!,#NULL!;0,"100","2.34","test",-3,#VALUE!,#REF!,#NUM!,#NAME?,""})
        // reads True — the top-left element's answer, never a spill, never the
        // formula-position element (anchor B1 would give the text " ").
        assert_eq!(
            boolean(
                "=ISNUMBER({1,\" \",1.23,TRUE,FALSE,\"\",#N/A,#DIV/0!,#SPILL!,#NULL!;0,\"100\",\"2.34\",\"test\",-3,#VALUE!,#REF!,#NUM!,#NAME?,\"\"})"
            ),
            true,
            "top-left element 1 is a number"
        );
        assert_eq!(
            boolean("=ISNUMBER({\"x\",1})"),
            false,
            "top-left element \"x\" is text"
        );
        assert_eq!(boolean("=ISTEXT({\"x\",1})"), true);
        assert_eq!(boolean("=ISNONTEXT({\"x\",1})"), false);
        assert_eq!(boolean("=ISLOGICAL({1,TRUE})"), false);
        assert_eq!(boolean("=ISLOGICAL({TRUE,1})"), true);
        assert_eq!(boolean("=ISERROR({1,#N/A})"), false);
        assert_eq!(boolean("=ISERROR({#N/A,1})"), true);
        assert_eq!(boolean("=ISNA({1,#N/A})"), false);
        assert_eq!(boolean("=ISNA({#N/A,1})"), true);
        assert_eq!(
            boolean("=ISBLANK({\"\",1})"),
            false,
            "top-left is empty text, not blank"
        );
    }

    #[test]
    fn iserror_iserr_isna_split_the_errors() {
        assert_eq!(boolean("=ISERROR(1/0)"), true, "ISERROR includes #DIV/0!");
        assert_eq!(boolean("=ISERROR(#N/A)"), true, "ISERROR includes #N/A");
        assert_eq!(boolean("=ISERROR(\"x\")"), false);
        assert_eq!(boolean("=ISERROR(1)"), false);
        assert_eq!(boolean("=ISERR(1/0)"), true);
        assert_eq!(boolean("=ISERR(#N/A)"), false, "ISERR excludes #N/A");
        assert_eq!(boolean("=ISERR(SQRT(-1))"), true, "#NUM! is not #N/A");
        assert_eq!(boolean("=ISNA(#N/A)"), true);
        assert_eq!(boolean("=ISNA(1/0)"), false);
        assert_eq!(boolean("=ISNA(SQRT(-1))"), false);
    }

    #[test]
    fn isref_accepts_only_references() {
        assert_eq!(boolean("=ISREF(A1)"), true);
        assert_eq!(boolean("=ISREF(A1:B5)"), true);
        assert_eq!(boolean("=ISREF(A:A)"), true);
        assert_eq!(boolean("=ISREF(1:1)"), true);
        assert_eq!(boolean("=ISREF(Sheet1!A1)"), true);
        assert_eq!(boolean("=ISREF(5)"), false);
        assert_eq!(boolean("=ISREF(\"x\")"), false);
        assert_eq!(boolean("=ISREF(TRUE)"), false);
    }

    #[test]
    fn is_predicate_arity_errors() {
        for f in [
            "ISBLANK",
            "ISNUMBER",
            "ISTEXT",
            "ISNONTEXT",
            "ISLOGICAL",
            "ISERROR",
            "ISERR",
            "ISNA",
            "ISREF",
        ] {
            assert_eq!(
                error(&format!("={f}()")),
                CalcError::Value,
                "{f} needs an argument"
            );
        }
        assert_eq!(
            error("=ISBLANK(1,2)"),
            CalcError::Value,
            "too many arguments"
        );
        assert_eq!(error("=ISREF(1,2)"), CalcError::Value, "too many arguments");
    }

    #[test]
    fn predicates_absorb_error_arguments() {
        // The whole point of the IS family: an error argument is inspected,
        // never re-raised.
        assert_eq!(boolean("=ISERROR(1/0)"), true);
        assert_eq!(boolean("=ISERR(1/0)"), true);
        assert_eq!(boolean("=ISNA(#N/A)"), true);
        assert_eq!(boolean("=ISNUMBER(1/0)"), false);
        assert_eq!(boolean("=ISTEXT(1/0)"), false);
        assert_eq!(boolean("=ISNONTEXT(1/0)"), true);
        assert_eq!(boolean("=ISLOGICAL(1/0)"), false);
        assert_eq!(boolean("=ISBLANK(1/0)"), false);
        assert_eq!(boolean("=ISREF(1/0)"), false);
    }
}

#[cfg(test)]
mod iseven_isodd {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parity_of_integers() {
        assert_eq!(boolean("=ISEVEN(0)"), true);
        assert_eq!(boolean("=ISEVEN(2)"), true);
        assert_eq!(boolean("=ISEVEN(1)"), false);
        assert_eq!(boolean("=ISODD(1)"), true);
        assert_eq!(boolean("=ISODD(2)"), false);
        assert_eq!(boolean("=ISEVEN(-2)"), true);
        assert_eq!(boolean("=ISODD(-3)"), true);
        assert_eq!(boolean("=ISEVEN(-3)"), false);
    }

    #[test]
    fn parity_truncates_toward_zero() {
        assert_eq!(boolean("=ISEVEN(2.9)"), true, "truncates to 2");
        assert_eq!(boolean("=ISODD(3.9)"), true, "truncates to 3");
        assert_eq!(boolean("=ISEVEN(-1.9)"), false, "truncates to -1");
        assert_eq!(boolean("=ISODD(-2.9)"), false, "truncates to -2");
    }

    #[test]
    fn parity_coercion_and_errors() {
        assert_eq!(boolean("=ISEVEN(\"4\")"), true, "numeric text coerces");
        assert_eq!(error("=ISEVEN(\"abc\")"), CalcError::Value);
        assert_eq!(error("=ISODD(\"abc\")"), CalcError::Value);
        assert_eq!(error("=ISEVEN()"), CalcError::Value);
        assert_eq!(error("=ISODD(1,2)"), CalcError::Value);
        assert_eq!(
            error("=ISEVEN(1E20)"),
            CalcError::Num,
            "parity beyond 2^53 is #NUM!"
        );
        assert_eq!(error("=ISODD(1/0)"), CalcError::Div0, "propagates");
    }
}

#[cfg(test)]
mod na_n_type_error_type {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn na_produces_the_na_error_value() {
        assert_eq!(
            calc("=NA()"),
            Outcome::Value(CalcValue::Error(CalcError::Na))
        );
        assert_eq!(error("=NA()"), CalcError::Na);
        assert_eq!(error("=NA(1)"), CalcError::Value, "too many arguments");
    }

    #[test]
    fn n_passes_numbers_converts_booleans_and_zeroes_text() {
        assert_eq!(num("=N(5)"), 5.0);
        assert_eq!(num("=N(TRUE)"), 1.0);
        assert_eq!(num("=N(FALSE)"), 0.0);
        assert_eq!(num("=N(\"abc\")"), 0.0);
        assert_eq!(num("=N(\"42\")"), 0.0, "N does not parse text");
        let g = Grid::empty().set_num("A1", 7.0);
        assert_eq!(g.num("=N(A1)"), 7.0);
        assert_eq!(
            Grid::empty().num("=N(A1)"),
            0.0,
            "a blank is not a number for N"
        );
    }

    #[test]
    fn n_keeps_errors() {
        assert_eq!(
            error("=N(1/0)"),
            CalcError::Div0,
            "an error argument stays an error"
        );
        assert_eq!(error("=N(#N/A)"), CalcError::Na);
        assert_eq!(error("=N()"), CalcError::Value);
    }

    #[test]
    fn type_reports_excel_kind_codes() {
        assert_eq!(num("=TYPE(1)"), 1.0);
        assert_eq!(num("=TYPE(\"a\")"), 2.0);
        assert_eq!(num("=TYPE(TRUE)"), 4.0);
        assert_eq!(num("=TYPE(#REF!)"), 16.0, "an error types as 16");
        assert_eq!(num("=TYPE(1/0)"), 16.0);
        assert_eq!(num("=TYPE(A1)"), 1.0, "a blank cell types as a number");
        assert_eq!(error("=TYPE()"), CalcError::Value);
    }

    #[test]
    fn error_type_codes_the_seven_errors() {
        assert_eq!(num("=ERROR.TYPE(#NULL!)"), 1.0);
        assert_eq!(num("=ERROR.TYPE(#DIV/0!)"), 2.0);
        assert_eq!(num("=ERROR.TYPE(#VALUE!)"), 3.0);
        assert_eq!(num("=ERROR.TYPE(#REF!)"), 4.0);
        assert_eq!(num("=ERROR.TYPE(#NAME?)"), 5.0);
        assert_eq!(num("=ERROR.TYPE(#NUM!)"), 6.0);
        assert_eq!(num("=ERROR.TYPE(#N/A)"), 7.0);
        assert_eq!(num("=ERROR.TYPE(1/0)"), 2.0, "a produced error works too");
    }

    #[test]
    fn error_type_of_a_non_error_is_na() {
        assert_eq!(error("=ERROR.TYPE(1)"), CalcError::Na);
        assert_eq!(error("=ERROR.TYPE(\"x\")"), CalcError::Na);
        assert_eq!(error("=ERROR.TYPE(TRUE)"), CalcError::Na);
        assert_eq!(error("=ERROR.TYPE()"), CalcError::Value);
    }
}

#[cfg(test)]
mod row_column {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn bare_row_and_column_report_the_formula_cell() {
        let g = Grid::empty();
        // Placed away from A1 on purpose: ROW()/COLUMN() must report the
        // formula cell, not the top-left corner of the grid.
        assert_eq!(g.at("C5", "=ROW()"), Outcome::Value(CalcValue::Number(5.0)));
        assert_eq!(
            g.at("C5", "=COLUMN()"),
            Outcome::Value(CalcValue::Number(3.0))
        );
        assert_eq!(
            g.at("Z100", "=ROW()"),
            Outcome::Value(CalcValue::Number(100.0))
        );
        assert_eq!(
            g.at("Z100", "=COLUMN()"),
            Outcome::Value(CalcValue::Number(26.0))
        );
    }

    #[test]
    fn row_and_column_of_a_reference() {
        assert_eq!(num("=ROW(B3)"), 3.0);
        assert_eq!(num("=COLUMN(B3)"), 2.0);
        assert_eq!(num("=ROW(A1:B5)"), 1.0, "the range's first row");
        assert_eq!(num("=COLUMN(A1:B5)"), 1.0, "the range's first column");
        assert_eq!(num("=ROW(C2:D4)"), 2.0);
        assert_eq!(num("=COLUMN(C2:D4)"), 3.0);
        assert_eq!(num("=ROW(3:3)"), 3.0, "a whole row");
        assert_eq!(num("=COLUMN(C:C)"), 3.0, "a whole column");
        assert_eq!(num("=ROW(Sheet1!C7)"), 7.0);
    }

    #[test]
    fn row_column_errors() {
        assert_eq!(error("=ROW(A1,B2)"), CalcError::Value, "too many arguments");
        assert_eq!(
            error("=COLUMN(1,2)"),
            CalcError::Value,
            "too many arguments"
        );
    }
}

#[cfg(test)]
mod rows_columns {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn rows_and_columns_of_ranges() {
        assert_eq!(num("=ROWS(A1:B5)"), 5.0);
        assert_eq!(num("=COLUMNS(A1:B5)"), 2.0);
        assert_eq!(num("=ROWS(A1:A1)"), 1.0);
        assert_eq!(num("=COLUMNS(A1:A1)"), 1.0);
        assert_eq!(num("=ROWS(B3)"), 1.0, "a single cell is a 1x1 area");
        assert_eq!(num("=COLUMNS(B3)"), 1.0);
    }

    #[test]
    fn whole_column_and_row_extent_without_materialisation() {
        // A whole-column reference reports the full grid extent and is never
        // materialised — ROWS(A:A) is a coordinate read, not a 1M-cell walk.
        assert_eq!(num("=ROWS(A:A)"), 1_048_576.0, "full grid rows");
        assert_eq!(num("=COLUMNS(A:A)"), 1.0);
        assert_eq!(num("=COLUMNS(A:C)"), 3.0);
        assert_eq!(num("=ROWS(1:1)"), 1.0);
        assert_eq!(num("=COLUMNS(1:1)"), 16_384.0, "full grid columns");
        assert_eq!(num("=ROWS(2:4)"), 3.0);
        assert_eq!(num("=COLUMNS(B:C)"), 2.0);
        // The extent survives coercion to text through the concat operator.
        assert_eq!(text("=ROWS(A:A)&\"\""), "1048576");
    }

    #[test]
    fn scalar_argument_is_a_one_by_one_area() {
        assert_eq!(num("=ROWS(5)"), 1.0);
        assert_eq!(num("=COLUMNS(\"x\")"), 1.0);
    }

    #[test]
    fn rows_columns_errors() {
        assert_eq!(error("=ROWS()"), CalcError::Value);
        assert_eq!(error("=COLUMNS()"), CalcError::Value);
        assert_eq!(error("=ROWS(1,2)"), CalcError::Value);
        assert_eq!(error("=COLUMNS(A1,B2)"), CalcError::Value);
        assert_eq!(
            error("=ROWS(NOSUCHNAME)"),
            CalcError::Name,
            "an unknown name is #NAME?"
        );
        assert_eq!(
            error("=COLUMNS(1/0)"),
            CalcError::Div0,
            "an error argument propagates"
        );
    }
}
