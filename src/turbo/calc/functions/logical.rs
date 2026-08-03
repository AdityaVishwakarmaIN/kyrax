// functions/logical.rs — the logical function family. Owned exclusively by the
// logical family agent; no other agent edits this file.
//
// Registry contract: implement `register` below and keep this exact signature.
// Do NOT edit functions/mod.rs — the `mod logical;` declaration and the
// `logical::register(&mut r)` call site in `build()` are already final.
// See functions/mod.rs for the worked ABS template.
use super::{FuncArg, FuncCtx, FuncSpec, Registry};
use crate::turbo::calc::coerce::coerce_number;
use crate::turbo::calc::coerce::compare_eq;
use crate::turbo::calc::value::{CalcError, CalcValue};

/// Excel truthiness. Bool is itself; Number is `n != 0`; Blank is false; Text
/// is `#VALUE!` (Excel does NOT coerce text to a boolean in a condition);
/// Error propagates; Array uses its first element.
fn truthy(v: &CalcValue) -> Result<bool, CalcError> {
    match v {
        CalcValue::Bool(b) => Ok(*b),
        CalcValue::Number(n) => Ok(*n != 0.0),
        CalcValue::Blank => Ok(false),
        CalcValue::Text(_) => Err(CalcError::Value),
        CalcValue::Error(e) => Err(*e),
        CalcValue::Array(a) => match a.iter().next() {
            Some(first) => truthy(first),
            None => Ok(false),
        },
    }
}

/// A taken branch that is `Blank` (an empty argument slot, e.g. the middle
/// comma in `IF(A1,,1)`, or an empty cell) is returned as `0`.
fn taken(v: &CalcValue) -> CalcValue {
    match v {
        CalcValue::Blank => CalcValue::Number(0.0),
        other => other.clone(),
    }
}

/// Collect the truthiness of every usable value in the argument list.
/// `AND`/`OR`/`XOR` ignore text and blanks inside arrays/ranges, coerce
/// numbers and booleans, propagate errors, and return `#VALUE!` when no usable
/// value exists at all. A direct scalar Text argument is `#VALUE!`.
fn collect_booleans(ctx: &FuncCtx, args: &[FuncArg]) -> Result<Vec<bool>, CalcError> {
    let mut out = Vec::new();
    for arg in args {
        let v = arg.value(ctx)?;
        match v {
            CalcValue::Array(a) => {
                for item in a.iter() {
                    match item {
                        CalcValue::Number(_) | CalcValue::Bool(_) => {
                            out.push(coerce_number(item)? != 0.0);
                        }
                        CalcValue::Text(_) | CalcValue::Blank => {}
                        CalcValue::Error(e) => return Err(*e),
                        CalcValue::Array(_) => return Err(CalcError::Value),
                    }
                }
            }
            CalcValue::Number(_) | CalcValue::Bool(_) => {
                out.push(coerce_number(&v)? != 0.0);
            }
            CalcValue::Blank => {}
            CalcValue::Text(_) => return Err(CalcError::Value),
            CalcValue::Error(e) => return Err(e),
        }
    }
    if out.is_empty() {
        return Err(CalcError::Value);
    }
    Ok(out)
}

fn f_if(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    if truthy(&args[0].value(ctx)?)? {
        Ok(taken(&args[1].value(ctx)?))
    } else if args.len() == 3 {
        Ok(taken(&args[2].value(ctx)?))
    } else {
        Ok(CalcValue::Bool(false))
    }
}

fn f_ifs(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    if args.len() % 2 != 0 {
        return Err(CalcError::Value);
    }
    for pair in args.chunks(2) {
        if truthy(&pair[0].value(ctx)?)? {
            return Ok(taken(&pair[1].value(ctx)?));
        }
    }
    Err(CalcError::Na)
}

fn f_and(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    Ok(CalcValue::Bool(
        collect_booleans(ctx, args)?.iter().all(|&b| b),
    ))
}

fn f_or(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    Ok(CalcValue::Bool(
        collect_booleans(ctx, args)?.iter().any(|&b| b),
    ))
}

fn f_not(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    Ok(CalcValue::Bool(!truthy(&args[0].value(ctx)?)?))
}

fn f_xor(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let bs = collect_booleans(ctx, args)?;
    Ok(CalcValue::Bool(bs.iter().filter(|&&b| b).count() % 2 == 1))
}

fn f_iferror(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    match args[0].value(ctx) {
        Ok(CalcValue::Error(_)) | Err(_) => args[1].value(ctx),
        Ok(v) => Ok(v),
    }
}

fn f_ifna(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    match args[0].value(ctx) {
        Ok(CalcValue::Error(CalcError::Na)) | Err(CalcError::Na) => args[1].value(ctx),
        Ok(v) => Ok(v),
        Err(e) => Err(e),
    }
}

fn f_true(_ctx: &FuncCtx, _args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    Ok(CalcValue::Bool(true))
}

fn f_false(_ctx: &FuncCtx, _args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    Ok(CalcValue::Bool(false))
}

fn f_switch(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let expr = args[0].value(ctx)?;
    let mut i = 1;
    while i + 1 < args.len() {
        if compare_eq(&expr, &args[i].value(ctx)?, false)? {
            return args[i + 1].value(ctx);
        }
        i += 2;
    }
    if i < args.len() {
        Ok(args[i].value(ctx)?)
    } else {
        Err(CalcError::Na)
    }
}

const IF: FuncSpec = FuncSpec {
    name: "IF",
    min_args: 2,
    max_args: Some(3),
    volatile: false,
    array_aware: false,
    func: f_if,
};

const IFS: FuncSpec = FuncSpec {
    name: "IFS",
    min_args: 2,
    max_args: None,
    volatile: false,
    array_aware: false,
    func: f_ifs,
};

const AND: FuncSpec = FuncSpec {
    name: "AND",
    min_args: 1,
    max_args: None,
    volatile: false,
    array_aware: true,
    func: f_and,
};

const OR: FuncSpec = FuncSpec {
    name: "OR",
    min_args: 1,
    max_args: None,
    volatile: false,
    array_aware: true,
    func: f_or,
};

const NOT: FuncSpec = FuncSpec {
    name: "NOT",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: false,
    func: f_not,
};

const XOR: FuncSpec = FuncSpec {
    name: "XOR",
    min_args: 1,
    max_args: None,
    volatile: false,
    array_aware: true,
    func: f_xor,
};

const IFERROR: FuncSpec = FuncSpec {
    name: "IFERROR",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: f_iferror,
};

const IFNA: FuncSpec = FuncSpec {
    name: "IFNA",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: false,
    func: f_ifna,
};

const TRUE: FuncSpec = FuncSpec {
    name: "TRUE",
    min_args: 0,
    max_args: Some(0),
    volatile: false,
    array_aware: false,
    func: f_true,
};

const FALSE: FuncSpec = FuncSpec {
    name: "FALSE",
    min_args: 0,
    max_args: Some(0),
    volatile: false,
    array_aware: false,
    func: f_false,
};

const SWITCH: FuncSpec = FuncSpec {
    name: "SWITCH",
    min_args: 3,
    max_args: None,
    volatile: false,
    array_aware: false,
    func: f_switch,
};

pub fn register(r: &mut Registry) {
    r.register(&IF);
    r.register(&IFS);
    r.register(&AND);
    r.register(&OR);
    r.register(&NOT);
    r.register(&XOR);
    r.register(&IFERROR);
    r.register(&IFNA);
    r.register(&TRUE);
    r.register(&FALSE);
    r.register(&SWITCH);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turbo::calc::functions::CellResolver;
    use crate::turbo::calc::value::ArrayValue;

    struct NoCells;
    impl CellResolver for NoCells {
        fn cell(&self, _sheet: u32, _row: u32, _col: u32) -> Option<CalcValue> {
            None
        }
        fn sheet_index(&self, _name: &str) -> Option<u32> {
            None
        }
    }

    fn call(spec: &FuncSpec, args: Vec<CalcValue>) -> Result<CalcValue, CalcError> {
        let resolver = NoCells;
        let ctx = FuncCtx {
            date1904: false,
            sheet: 0,
            row: 0,
            col: 0,
            resolver: &resolver,
        };
        let fargs: Vec<FuncArg> = args.into_iter().map(FuncArg::Value).collect();
        (spec.func)(&ctx, &fargs)
    }

    #[test]
    fn if_selects_branch_and_omitted_else() {
        let t = CalcValue::Bool(true);
        let f = CalcValue::Bool(false);
        assert_eq!(
            call(
                &IF,
                vec![t.clone(), CalcValue::Number(7.0), CalcValue::Number(8.0)]
            ),
            Ok(CalcValue::Number(7.0))
        );
        assert_eq!(
            call(
                &IF,
                vec![f.clone(), CalcValue::Number(7.0), CalcValue::Number(8.0)]
            ),
            Ok(CalcValue::Number(8.0))
        );
        assert_eq!(
            call(&IF, vec![f.clone(), CalcValue::Number(7.0)]),
            Ok(CalcValue::Bool(false))
        );
        assert_eq!(
            call(&IF, vec![CalcValue::Number(0.5), CalcValue::Number(1.0)]),
            Ok(CalcValue::Number(1.0))
        );
        assert_eq!(
            call(
                &IF,
                vec![
                    CalcValue::Number(0.0),
                    CalcValue::Number(1.0),
                    CalcValue::Number(2.0)
                ]
            ),
            Ok(CalcValue::Number(2.0))
        );
        assert_eq!(
            call(&IF, vec![t, CalcValue::Blank, CalcValue::Number(2.0)]),
            Ok(CalcValue::Number(0.0))
        );
    }

    #[test]
    fn if_text_condition_is_value_error() {
        assert_eq!(
            call(
                &IF,
                vec![
                    CalcValue::text("nope"),
                    CalcValue::Number(1.0),
                    CalcValue::Number(2.0)
                ]
            ),
            Err(CalcError::Value)
        );
    }

    #[test]
    fn if_condition_error_propagates() {
        assert_eq!(
            call(
                &IF,
                vec![
                    CalcValue::err(CalcError::Div0),
                    CalcValue::Number(1.0),
                    CalcValue::Number(2.0)
                ]
            ),
            Err(CalcError::Div0)
        );
    }

    #[test]
    fn ifs_first_match_and_na() {
        assert_eq!(
            call(
                &IFS,
                vec![
                    CalcValue::Bool(false),
                    CalcValue::Number(1.0),
                    CalcValue::Bool(true),
                    CalcValue::Number(2.0)
                ]
            ),
            Ok(CalcValue::Number(2.0))
        );
        assert_eq!(
            call(
                &IFS,
                vec![
                    CalcValue::Number(1.0),
                    CalcValue::text("first"),
                    CalcValue::Number(0.0),
                    CalcValue::text("second")
                ]
            ),
            Ok(CalcValue::text("first"))
        );
        assert_eq!(
            call(&IFS, vec![CalcValue::Bool(false), CalcValue::Number(1.0)]),
            Err(CalcError::Na)
        );
        assert_eq!(
            call(
                &IFS,
                vec![
                    CalcValue::Bool(true),
                    CalcValue::Number(1.0),
                    CalcValue::Number(2.0)
                ]
            ),
            Err(CalcError::Value)
        );
    }

    #[test]
    fn and_or_ignore_text_and_blanks_in_arrays() {
        let arr = CalcValue::array(ArrayValue::new(
            2,
            2,
            vec![
                CalcValue::Number(1.0),
                CalcValue::text("x"),
                CalcValue::Blank,
                CalcValue::Bool(true),
            ],
        ));
        assert_eq!(call(&AND, vec![arr.clone()]), Ok(CalcValue::Bool(true)));
        assert_eq!(call(&OR, vec![arr.clone()]), Ok(CalcValue::Bool(true)));

        let no_usable = CalcValue::array(ArrayValue::new(
            1,
            2,
            vec![CalcValue::text("x"), CalcValue::Blank],
        ));
        assert_eq!(call(&AND, vec![no_usable.clone()]), Err(CalcError::Value));
        assert_eq!(call(&OR, vec![no_usable.clone()]), Err(CalcError::Value));
        assert_eq!(
            call(&AND, vec![CalcValue::text("x")]),
            Err(CalcError::Value)
        );
    }

    #[test]
    fn and_or_coerce_and_propagate() {
        assert_eq!(
            call(&AND, vec![CalcValue::Bool(true), CalcValue::Number(1.0)]),
            Ok(CalcValue::Bool(true))
        );
        assert_eq!(
            call(&AND, vec![CalcValue::Bool(true), CalcValue::Number(0.0)]),
            Ok(CalcValue::Bool(false))
        );
        assert_eq!(
            call(
                &OR,
                vec![
                    CalcValue::Bool(false),
                    CalcValue::Number(0.0),
                    CalcValue::Number(0.5)
                ]
            ),
            Ok(CalcValue::Bool(true))
        );
        assert_eq!(
            call(&OR, vec![CalcValue::Bool(false)]),
            Ok(CalcValue::Bool(false))
        );
        assert_eq!(
            call(
                &AND,
                vec![CalcValue::err(CalcError::Na), CalcValue::Bool(true)]
            ),
            Err(CalcError::Na)
        );
    }

    #[test]
    fn not_flips_truthiness() {
        assert_eq!(
            call(&NOT, vec![CalcValue::Bool(true)]),
            Ok(CalcValue::Bool(false))
        );
        assert_eq!(
            call(&NOT, vec![CalcValue::Number(0.0)]),
            Ok(CalcValue::Bool(true))
        );
        assert_eq!(
            call(&NOT, vec![CalcValue::Blank]),
            Ok(CalcValue::Bool(true))
        );
        assert_eq!(
            call(&NOT, vec![CalcValue::text("x")]),
            Err(CalcError::Value)
        );
    }

    #[test]
    fn xor_is_parity_of_true() {
        assert_eq!(
            call(&XOR, vec![CalcValue::Bool(true), CalcValue::Bool(false)]),
            Ok(CalcValue::Bool(true))
        );
        assert_eq!(
            call(&XOR, vec![CalcValue::Bool(true), CalcValue::Bool(true)]),
            Ok(CalcValue::Bool(false))
        );
        assert_eq!(
            call(
                &XOR,
                vec![
                    CalcValue::Bool(true),
                    CalcValue::Bool(true),
                    CalcValue::Bool(true)
                ]
            ),
            Ok(CalcValue::Bool(true))
        );
        assert_eq!(
            call(&XOR, vec![CalcValue::Bool(false), CalcValue::Bool(false)]),
            Ok(CalcValue::Bool(false))
        );
    }

    #[test]
    fn iferror_passthrough_and_catch() {
        assert_eq!(
            call(
                &IFERROR,
                vec![CalcValue::Number(3.0), CalcValue::text("fallback")]
            ),
            Ok(CalcValue::Number(3.0))
        );
        assert_eq!(
            call(
                &IFERROR,
                vec![CalcValue::err(CalcError::Div0), CalcValue::text("fallback")]
            ),
            Ok(CalcValue::text("fallback"))
        );
        assert_eq!(
            call(
                &IFERROR,
                vec![CalcValue::err(CalcError::Na), CalcValue::Number(9.0)]
            ),
            Ok(CalcValue::Number(9.0))
        );
        assert_eq!(
            call(&IFERROR, vec![CalcValue::Blank, CalcValue::Number(9.0)]),
            Ok(CalcValue::Blank)
        );
    }

    #[test]
    fn ifna_only_catches_na() {
        assert_eq!(
            call(
                &IFNA,
                vec![CalcValue::err(CalcError::Na), CalcValue::text("fallback")]
            ),
            Ok(CalcValue::text("fallback"))
        );
        assert_eq!(
            // a non-#N/A error is passed through unchanged, as a value
            call(
                &IFNA,
                vec![CalcValue::err(CalcError::Div0), CalcValue::text("fallback")]
            ),
            Ok(CalcValue::err(CalcError::Div0))
        );
        assert_eq!(
            call(
                &IFNA,
                vec![CalcValue::Number(3.0), CalcValue::text("fallback")]
            ),
            Ok(CalcValue::Number(3.0))
        );
    }

    #[test]
    fn switch_matches_and_default() {
        assert_eq!(
            call(
                &SWITCH,
                vec![
                    CalcValue::Number(2.0),
                    CalcValue::Number(1.0),
                    CalcValue::text("one"),
                    CalcValue::Number(2.0),
                    CalcValue::text("two")
                ]
            ),
            Ok(CalcValue::text("two"))
        );
        assert_eq!(
            call(
                &SWITCH,
                vec![
                    CalcValue::Number(5.0),
                    CalcValue::Number(1.0),
                    CalcValue::text("one"),
                    CalcValue::text("default")
                ]
            ),
            Ok(CalcValue::text("default"))
        );
        assert_eq!(
            call(
                &SWITCH,
                vec![
                    CalcValue::Number(5.0),
                    CalcValue::Number(1.0),
                    CalcValue::text("one")
                ]
            ),
            Err(CalcError::Na)
        );
        assert_eq!(
            call(
                &SWITCH,
                vec![
                    CalcValue::text("ABC"),
                    CalcValue::text("abc"),
                    CalcValue::Number(7.0)
                ]
            ),
            Ok(CalcValue::Number(7.0))
        );
    }

    #[test]
    fn true_false_literals() {
        assert_eq!(call(&TRUE, vec![]), Ok(CalcValue::Bool(true)));
        assert_eq!(call(&FALSE, vec![]), Ok(CalcValue::Bool(false)));
        assert!(TRUE.validate(0).is_ok());
        assert_eq!(TRUE.validate(1), Err(CalcError::Value));
        assert_eq!(FALSE.validate(0), Ok(()));
    }

    #[test]
    fn arity_and_flags() {
        assert!(IF.validate(2).is_ok());
        assert!(IF.validate(3).is_ok());
        assert_eq!(IF.validate(1), Err(CalcError::Value));
        assert_eq!(IF.validate(4), Err(CalcError::Value));
        assert!(IFS.validate(2).is_ok());
        assert!(IFS.validate(8).is_ok());
        assert_eq!(IFS.validate(0), Err(CalcError::Value));
        assert!(AND.validate(1).is_ok());
        assert_eq!(AND.validate(0), Err(CalcError::Value));
        assert!(NOT.validate(1).is_ok());
        assert_eq!(NOT.validate(2), Err(CalcError::Value));
        assert_eq!(IFERROR.validate(2), Ok(()));
        assert!(SWITCH.validate(3).is_ok());
        assert_eq!(SWITCH.validate(2), Err(CalcError::Value));
        for spec in [
            &IF, &IFS, &AND, &OR, &NOT, &XOR, &IFERROR, &IFNA, &TRUE, &FALSE, &SWITCH,
        ] {
            assert!(!spec.volatile, "{} must not be volatile", spec.name);
        }
        for spec in [&AND, &OR, &XOR] {
            assert!(spec.array_aware, "{} must be array aware", spec.name);
        }
        for spec in [&IF, &IFS, &NOT, &IFERROR, &IFNA, &TRUE, &FALSE, &SWITCH] {
            assert!(!spec.array_aware, "{} must not be array aware", spec.name);
        }
    }
}
