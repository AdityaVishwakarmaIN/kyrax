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
use crate::turbo::calc::value::{ArrayValue, CalcError, CalcValue};

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

/// The value an array-mode consumer sees at index `i` of an argument: the i-th
/// element of an array, or the argument itself when it is a scalar (Excel
/// broadcasts the scalar over the whole result). A 1x1 array is a scalar.
fn arr_elem(v: &CalcValue, i: usize) -> CalcValue {
    match v {
        CalcValue::Array(a) if a.is_scalar_array() => a.data[0].clone(),
        CalcValue::Array(a) => a.data.get(i).cloned().unwrap_or(CalcValue::Blank),
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

/// `IF(condition, if_true, [if_false])`. A single cell / scalar condition picks
/// one branch, as always. A **condition that is an array** is evaluated
/// element-wise: the scalar branches are broadcast, an array branch is picked
/// element by element, a blank branch coerces to `0`, an element that is an
/// error stays an error, and a missing `if_false` element is `FALSE` — the same
/// rules as the scalar form, applied per element.
fn f_if(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let cond = args[0].value(ctx)?;
    if let CalcValue::Array(a) = cond {
        let then_v = args[1].value(ctx)?;
        let else_v = if args.len() == 3 {
            Some(args[2].value(ctx)?)
        } else {
            None
        };
        let mut data = Vec::with_capacity(a.data.len());
        for (i, e) in a.iter().enumerate() {
            match truthy(e) {
                Ok(true) => data.push(taken(&arr_elem(&then_v, i))),
                Ok(false) => match &else_v {
                    Some(v) => data.push(taken(&arr_elem(v, i))),
                    None => data.push(CalcValue::Bool(false)),
                },
                Err(err) => data.push(CalcValue::Error(err)),
            }
        }
        return Ok(CalcValue::array(ArrayValue::new(a.rows, a.cols, data)));
    }
    if truthy(&cond)? {
        Ok(taken(&args[1].value(ctx)?))
    } else if args.len() == 3 {
        Ok(taken(&args[2].value(ctx)?))
    } else {
        Ok(CalcValue::Bool(false))
    }
}

/// `IFS(cond1, value1, cond2, value2, ...)`. The scalar form returns the first
/// matching value (`#N/A` when nothing matches). When **any condition is an
/// array** the pairs are evaluated element-wise: each element scans the pairs
/// in order (first truthy element wins), `#N/A` for an element that matches
/// nothing, and a condition that is an error becomes an error element.
fn f_ifs(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    if args.len() % 2 != 0 {
        return Err(CalcError::Value);
    }
    let mut conds: Vec<CalcValue> = Vec::with_capacity(args.len() / 2);
    let mut vals: Vec<CalcValue> = Vec::with_capacity(args.len() / 2);
    for pair in args.chunks(2) {
        conds.push(pair[0].value(ctx)?);
        vals.push(pair[1].value(ctx)?);
    }
    let shape = conds.iter().find_map(|c| match c {
        CalcValue::Array(a) => Some((a.rows, a.cols)),
        _ => None,
    });
    if let Some((rows, cols)) = shape {
        let n = (rows as usize) * (cols as usize);
        let mut data = Vec::with_capacity(n);
        for i in 0..n {
            let mut result = CalcValue::Error(CalcError::Na);
            for (cond, val) in conds.iter().zip(vals.iter()) {
                match truthy(&arr_elem(cond, i)) {
                    Ok(true) => {
                        result = taken(&arr_elem(val, i));
                        break;
                    }
                    Ok(false) => continue,
                    Err(err) => {
                        result = CalcValue::Error(err);
                        break;
                    }
                }
            }
            data.push(result);
        }
        return Ok(CalcValue::array(ArrayValue::new(rows, cols, data)));
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

/// `IFERROR(value, value_if_error)`. A scalar first argument is passed through
/// unless it is an error, which is replaced by the fallback. When **the first
/// argument is an array** the replacement happens element-wise (a scalar
/// fallback broadcasts, an array fallback is read element by element).
fn f_iferror(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let v = args[0].value(ctx)?;
    if let CalcValue::Array(a) = v {
        let fallback = args[1].value(ctx)?;
        let mut data = Vec::with_capacity(a.data.len());
        for (i, e) in a.iter().enumerate() {
            if e.is_error() {
                data.push(arr_elem(&fallback, i));
            } else {
                data.push(e.clone());
            }
        }
        return Ok(CalcValue::array(ArrayValue::new(a.rows, a.cols, data)));
    }
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

/// `SWITCH(expression, value1, result1, [value2, result2, ...], [default])`.
/// The scalar form matches the expression against each value in turn and takes
/// a trailing odd argument as the default (`#N/A` when nothing matches). When
/// **the expression is an array** the match is performed element-wise: values
/// and results that are arrays are read element by element, scalars broadcast.
fn f_switch(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let expr = args[0].value(ctx)?;
    if let CalcValue::Array(a) = expr {
        let mut data = Vec::with_capacity(a.data.len());
        for (i, e) in a.iter().enumerate() {
            let mut result = None;
            let mut j = 1usize;
            while j + 1 < args.len() {
                let value = arr_elem(&args[j].value(ctx)?, i);
                match compare_eq(e, &value, false) {
                    Ok(true) => {
                        result = Some(arr_elem(&args[j + 1].value(ctx)?, i));
                        break;
                    }
                    Ok(false) => {}
                    Err(err) => {
                        result = Some(CalcValue::Error(err));
                        break;
                    }
                }
                j += 2;
            }
            if result.is_none() && j < args.len() {
                result = Some(arr_elem(&args[j].value(ctx)?, i));
            }
            data.push(match result {
                Some(v) => v,
                None => CalcValue::Error(CalcError::Na),
            });
        }
        return Ok(CalcValue::array(ArrayValue::new(a.rows, a.cols, data)));
    }
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
    // Array-aware so an array condition (e.g. `IF({FALSE;TRUE;TRUE},1)`) is
    // handed over whole and mapped element-wise to an array result.
    array_aware: true,
    func: f_if,
};

const IFS: FuncSpec = FuncSpec {
    name: "IFS",
    min_args: 2,
    max_args: None,
    volatile: false,
    // Array-aware so array conditions map to array results, matching the
    // dynamic-array IFS.
    array_aware: true,
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
    // Array-aware so an error-bearing array is mapped element-wise, e.g.
    // `IFERROR({1;#N/A;1},"error")`.
    array_aware: true,
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
    // Array-aware so an array expression is matched element-wise to an array
    // result, e.g. `SWITCH({1;2;3},1,"One",2,"Two","Default")`.
    array_aware: true,
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
    use pretty_assertions::assert_eq;

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
        for spec in [&AND, &OR, &XOR, &IF, &IFS, &IFERROR, &SWITCH] {
            assert!(spec.array_aware, "{} must be array aware", spec.name);
        }
        for spec in [&NOT, &IFNA, &TRUE, &FALSE] {
            assert!(!spec.array_aware, "{} must not be array aware", spec.name);
        }
    }

    // -----------------------------------------------------------------------
    // Array-constant forms (Excel dynamic-array behavior): an array condition
    // or expression maps to an array result, scalar branches broadcast.
    // Persistence of the array to the sheet is the hydration lane's job — the
    // engine's contract is the value itself.
    // -----------------------------------------------------------------------
    mod array_forms {
        use super::*;
        use crate::turbo::calc::testkit::{Grid, Outcome, error};
        use pretty_assertions::assert_eq;

        fn value(g: &Grid, formula: &str) -> CalcValue {
            match g.calc(formula) {
                Outcome::Value(v) => v,
                other => panic!("{formula} -> {other:?}, expected a value"),
            }
        }

        fn arr_of(g: &Grid, formula: &str) -> ArrayValue {
            match value(g, formula) {
                CalcValue::Array(a) => (*a).clone(),
                other => panic!("{formula} -> {other:?}, expected an array"),
            }
        }

        #[test]
        fn if_maps_an_array_condition_elementwise() {
            let g = Grid::empty();
            let a = arr_of(&g, "=IF({FALSE;TRUE;TRUE},1)");
            assert_eq!(a.shape(), (3, 1));
            assert_eq!(a.get(0, 0), &CalcValue::Bool(false));
            assert_eq!(a.get(1, 0), &CalcValue::Number(1.0));
            assert_eq!(a.get(2, 0), &CalcValue::Number(1.0));
        }

        #[test]
        fn if_broadcasts_scalars_and_picks_array_branches() {
            let g = Grid::empty();
            let a = arr_of(&g, "=IF({TRUE;FALSE},{1;2},{3;4})");
            assert_eq!(a.shape(), (2, 1));
            assert_eq!(a.get(0, 0), &CalcValue::Number(1.0));
            assert_eq!(a.get(1, 0), &CalcValue::Number(4.0));

            // a scalar branch is broadcast over the whole array
            let a = arr_of(&g, "=IF({FALSE;TRUE},7,8)");
            assert_eq!(a.shape(), (2, 1));
            assert_eq!(a.get(0, 0), &CalcValue::Number(8.0));
            assert_eq!(a.get(1, 0), &CalcValue::Number(7.0));
        }

        #[test]
        fn if_array_condition_errors_become_error_elements() {
            let g = Grid::empty();
            let a = arr_of(&g, "=IF({TRUE;#N/A},1,2)");
            assert_eq!(a.get(0, 0), &CalcValue::Number(1.0));
            assert_eq!(a.get(1, 0), &CalcValue::Error(CalcError::Na));
        }

        #[test]
        fn iferror_replaces_error_elements_in_an_array() {
            let g = Grid::empty();
            let a = arr_of(&g, "=IFERROR({1;#N/A;1},\"error\")");
            assert_eq!(a.shape(), (3, 1));
            assert_eq!(a.get(0, 0), &CalcValue::Number(1.0));
            assert_eq!(a.get(1, 0), &CalcValue::text("error"));
            assert_eq!(a.get(2, 0), &CalcValue::Number(1.0));
        }

        #[test]
        fn ifs_maps_an_array_condition_to_an_array() {
            let g = Grid::empty();
            let a = arr_of(&g, "=IFS({FALSE;TRUE;FALSE},1)");
            assert_eq!(a.shape(), (3, 1));
            assert_eq!(a.get(0, 0), &CalcValue::Error(CalcError::Na));
            assert_eq!(a.get(1, 0), &CalcValue::Number(1.0));
            assert_eq!(a.get(2, 0), &CalcValue::Error(CalcError::Na));
        }

        #[test]
        fn switch_maps_an_array_expression_to_an_array() {
            let g = Grid::empty();
            let a = arr_of(&g, "=SWITCH({1;2;3},1,\"One\",2,\"Two\",\"Default\")");
            assert_eq!(a.shape(), (3, 1));
            assert_eq!(a.get(0, 0), &CalcValue::text("One"));
            assert_eq!(a.get(1, 0), &CalcValue::text("Two"));
            assert_eq!(a.get(2, 0), &CalcValue::text("Default"));
        }

        #[test]
        fn scalar_forms_are_unchanged_by_the_array_path() {
            let g = Grid::empty();
            assert_eq!(g.num("=IF(TRUE,7,8)"), 7.0);
            assert_eq!(g.num("=IFERROR(3,\"x\")"), 3.0);
            assert_eq!(g.num("=IFS(FALSE,1,TRUE,2)"), 2.0);
            assert_eq!(g.text("=SWITCH(2,1,\"one\",2,\"two\")"), "two");
            assert_eq!(error("=IF(1/0,1,2)"), CalcError::Div0);
        }
    }
}
