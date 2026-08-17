// calc/lambda.rs — LAMBDA and its helper functions (dynamic-array support, part 2 of 3).
//
// This file is ONE THIRD of the dynamic-array build-out and is owned exclusively
// by this agent. It owns CALLABLE VALUES and LEXICAL SCOPING:
//
//   * `LambdaValue` — an anonymous function produced by LAMBDA, carrying its
//     parameter names, body AST, and the scope it was defined in.
//   * `Scope` — a chain of bindings. Resolution is INNERMOST-FIRST:
//     a parameter shadows an outer LET binding, which shadows a defined name.
//   * The functions LAMBDA, LET, MAP, REDUCE, SCAN, BYROW, BYCOL, MAKEARRAY and
//     ISOMITTED, registered through `pub fn register` (see the final note).
//
// The spill machinery (`calc/spill.rs`) and the other dynamic-array functions
// (`calc/functions/dynamic.rs`) live in sibling files owned by other agents;
// this file neither reads nor edits them.
//
// # How this integrates with the evaluator
//
// `calc/eval.rs` hands every lambda-family call its arguments UNEVALUATED (as
// `FuncArg::Expr`), because a LAMBDA body, a LET calculation and a parameter
// name must be inspected, not evaluated, before the function acts on them. This
// file then runs its own scope-aware evaluator (`eval_in`) over those raw AST
// nodes. Non-lambda function calls inside a body fall back to the ordinary
// registry, so `SUM`, `IF`, `IFERROR`, ... behave exactly as they do at the
// top level.
//
// # Recursion must terminate
//
// A lambda bound to a name can call itself (Excel's recursion-through-LET and
// recursion-through-defined-names features). That is also the fastest way to
// hang the engine or blow the native stack, so this evaluator never relies on
// Rust's call stack for user-controlled depth. Three guards:
//
//   * `MAX_LAMBDA_DEPTH` — nested LAMBDA invocations (128).
//   * `MAX_DEPTH` — AST nesting inside the scope-aware evaluator (1024).
//   * `MAX_STEPS` — total evaluation steps across one top-level call
//     (10,000,000), the guard for wide-but-shallow loops such as a 100k-cell
//     MAP.
//
// All three return `#NUM!` (the closest Excel error to "gave up") rather than
// hanging. The `deps.rs` precedent is followed: deep work is never unbounded
// recursion. A self-referential lambda therefore terminates with `#NUM!`
// instead of aborting the process.

use crate::turbo::calc::ast::{BinaryOp, Expr, RefExpr, SuffixOp, UnaryOp};
use crate::turbo::calc::coerce::{coerce_number, coerce_text, compare, compare_eq};
use crate::turbo::calc::functions::{FuncArg, FuncCtx, FuncSpec, Registry, registry};
use crate::turbo::calc::value::{ArrayValue, CalcError, CalcValue};
use std::cell::RefCell;
use std::cmp::Ordering;
use std::fmt;
use std::rc::Rc;
use std::sync::Arc;

/// Nested LAMBDA invocations before `#NUM!`. The primary guard against runaway
/// self-reference; see the module docs.
const MAX_LAMBDA_DEPTH: u32 = 128;
/// AST nesting inside the scope-aware evaluator before `#NUM!`. Bounds native
/// stack use so a single pathological body cannot overflow it.
const MAX_DEPTH: u32 = 1024;
/// Total evaluation steps before `#NUM!` (guards wide loops such as a 100k-cell
/// MAP whose per-call work is shallow).
const MAX_STEPS: u64 = 10_000_000;
/// Broadcast / result-cap identical to `eval.rs`: never allocate more than 4M
/// cells for one result.
const MAX_CELLS: u64 = 4_000_000;

/// Functions that must receive raw arguments for lexical evaluation.
const LAMBDA_FAMILY: [&str; 9] = [
    "LAMBDA",
    "LET",
    "MAP",
    "REDUCE",
    "SCAN",
    "BYROW",
    "BYCOL",
    "MAKEARRAY",
    "ISOMITTED",
];

/// Whether a call's arguments must remain unevaluated.
#[allow(dead_code)]
pub fn wants_raw_args(name: &str) -> bool {
    let up = name.to_ascii_uppercase();
    LAMBDA_FAMILY.contains(&up.as_str())
}

// ---------------------------------------------------------------------------
// The callable value and the scope
// ---------------------------------------------------------------------------

/// A user-defined function value produced by LAMBDA.
///
/// `body` is the lone calculation AST node; `params` are its parameter names.
/// `captured` is the lexical scope active when the LAMBDA was evaluated, so a
/// body can reference outer LET bindings and defined-name values. The body AST
/// is cloned ONCE at construction — never per invocation — so a MAP over 100k
/// cells pays zero AST-copy cost.
pub struct LambdaValue {
    pub params: Vec<String>,
    pub body: Expr,
    captured: Scope,
}

impl LambdaValue {
    pub fn new(params: Vec<String>, body: Expr, captured: Scope) -> Self {
        Self {
            params,
            body,
            captured,
        }
    }
}

/// What a scope binding holds. Values are the ordinary calculation values;
/// lambdas and omitted parameters get their own slots because neither is a
/// `CalcValue`.
#[derive(Clone)]
pub(crate) enum Bound {
    Value(CalcValue),
    Lambda(Rc<LambdaValue>),
    Omitted,
}

/// Lexical scope: a chain of frames ending at a root.
///
/// Each frame is `Rc<RefCell<Vec<(name, Bound)>>>` so that `bind` on a scope
/// handle is visible through every clone of it — including a `LambdaValue`'s
/// captured scope. That liveness is what makes recursion-through-LET work:
/// `LET(f, LAMBDA(x, f(x)), f(1))` binds `f` AFTER the LAMBDA captured the LET
/// frame, yet `f` is still visible to the body.
///
/// Lookups are a linear scan of a tiny vector (parameter counts are single
/// digits), not a HashMap allocation. Names are matched case-insensitively,
/// as Excel's defined names and LET names are.
#[derive(Clone)]
pub struct Scope {
    parent: Option<Rc<Scope>>,
    frame: Rc<RefCell<Vec<(String, Bound)>>>,
}

impl Scope {
    /// A scope with no parent.
    pub fn root() -> Scope {
        Scope {
            parent: None,
            frame: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// A new innermost frame over `self`. Bindings added to it are invisible to
    /// `self`, and vice versa; the parent chain remains shared.
    pub fn child(&self) -> Scope {
        Scope {
            parent: Some(Rc::new(self.clone())),
            frame: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// Bind `name` to a value in the innermost frame.
    pub fn bind(&mut self, name: &str, value: CalcValue) {
        self.frame
            .borrow_mut()
            .push((name.to_ascii_lowercase(), Bound::Value(value)));
    }

    pub(crate) fn bind_lambda(&mut self, name: &str, value: Rc<LambdaValue>) {
        self.frame
            .borrow_mut()
            .push((name.to_ascii_lowercase(), Bound::Lambda(value)));
    }

    pub(crate) fn bind_omitted(&mut self, name: &str) {
        self.frame
            .borrow_mut()
            .push((name.to_ascii_lowercase(), Bound::Omitted));
    }

    /// The value bound to `name`, innermost-first. `None` when the name is
    /// unbound, bound to a lambda, or bound but omitted — callers that need
    /// those cases use [`Scope::lookup_bound`].
    ///
    /// Kept for the `Scope` API contract; the evaluator itself needs the full
    /// [`Bound`] (a value vs a lambda vs omitted), so it uses `lookup_bound`.
    #[allow(dead_code)]
    pub fn lookup(&self, name: &str) -> Option<CalcValue> {
        match self.lookup_bound(name) {
            Some(Bound::Value(v)) => Some(v),
            _ => None,
        }
    }

    /// The full binding for `name`, innermost-first, cloned out of the shared
    /// frame (the clone of a value is a refcount bump — cheap).
    pub(crate) fn lookup_bound(&self, name: &str) -> Option<Bound> {
        let key = name.to_ascii_lowercase();
        let mut cur: Option<&Scope> = Some(self);
        while let Some(s) = cur {
            let frame = s.frame.borrow();
            for (n, b) in frame.iter().rev() {
                if *n == key {
                    return Some(b.clone());
                }
            }
            cur = s.parent.as_deref();
        }
        None
    }
}

impl fmt::Debug for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Scope")
            .field("bindings", &self.frame.borrow().len())
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for LambdaValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LambdaValue")
            .field("params", &self.params)
            .field("body", &self.body)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// The scope-aware evaluator
// ---------------------------------------------------------------------------

/// The result of one evaluation in the lambda evaluator: either a value or a
/// freshly-constructed callable (a LAMBDA expression that was not applied).
enum Out {
    V(CalcValue),
    L(Rc<LambdaValue>),
}

impl Out {
    /// Turn an evaluation result into a plain value. A callable that escapes
    /// into a data position is `#CALC!`, matching Excel's display of a lambda
    /// that was not applied.
    fn into_value(self) -> Result<CalcValue, CalcError> {
        match self {
            Out::V(v) => Ok(v),
            Out::L(_) => Err(CalcError::Calc),
        }
    }
}

/// Per-call evaluation budget: total steps, current AST nesting, and current
/// lambda-invocation depth. Threaded through every evaluation; one instance per
/// top-level lambda-family call (so a workbook with many independent calls each
/// gets a fresh budget).
struct EvalState {
    steps: u64,
    depth: u32,
    lambda_depth: u32,
}

impl EvalState {
    fn new() -> Self {
        EvalState {
            steps: 0,
            depth: 0,
            lambda_depth: 0,
        }
    }
}

/// Evaluate `expr` against `scope`, where `frame` is the innermost enclosing
/// LAMBDA's supplied arguments (for `Expr::LambdaParam`). All three budgets are
/// enforced here, at the single chokepoint every node passes through.
fn eval_in(
    expr: &Expr,
    ctx: &FuncCtx,
    scope: &Scope,
    frame: &[CalcValue],
    state: &mut EvalState,
) -> Result<Out, CalcError> {
    state.steps += 1;
    if state.steps > MAX_STEPS {
        return Err(CalcError::Num);
    }
    state.depth += 1;
    if state.depth > MAX_DEPTH {
        state.depth -= 1;
        return Err(CalcError::Num);
    }
    let r = eval_node(expr, ctx, scope, frame, state);
    state.depth -= 1;
    r
}

fn eval_node(
    expr: &Expr,
    ctx: &FuncCtx,
    scope: &Scope,
    frame: &[CalcValue],
    state: &mut EvalState,
) -> Result<Out, CalcError> {
    match expr {
        Expr::Value(v) => Ok(Out::V(v.clone())),
        Expr::Null => Ok(Out::V(CalcValue::Blank)),
        Expr::Ref(re) => eval_ref(re, ctx, scope),
        Expr::LambdaParam(i) => match frame.get(*i) {
            Some(v) => Ok(Out::V(v.clone())),
            None => Err(CalcError::Value), // the parameter was not supplied
        },
        Expr::Lambda { params, body } => Ok(Out::L(Rc::new(LambdaValue::new(
            params.clone(),
            (**body).clone(),
            scope.clone(),
        )))),
        Expr::Unary(op, inner) => {
            let v = eval_in(inner, ctx, scope, frame, state)?.into_value()?;
            apply_unary(*op, v, ctx).map(Out::V)
        }
        Expr::Suffix(SuffixOp::Percent, inner) => {
            let v = eval_in(inner, ctx, scope, frame, state)?.into_value()?;
            percent(v).map(Out::V)
        }
        Expr::Binary(op, l, r) => {
            let lv = eval_in(l, ctx, scope, frame, state)?.into_value()?;
            let rv = eval_in(r, ctx, scope, frame, state)?.into_value()?;
            apply_binary(*op, lv, rv).map(Out::V)
        }
        Expr::Colon(_, _) | Expr::Union(_) => Err(CalcError::Ref),
        Expr::Formula(children) => {
            // Mirror `eval.rs`: a formula node with exactly one child evaluates
            // through; more than one has no single value.
            if children.len() == 1 {
                eval_in(&children[0], ctx, scope, frame, state)
            } else {
                Err(CalcError::Value)
            }
        }
        Expr::Function { name, args } => eval_call(name, args, ctx, scope, frame, state),
    }
}

fn eval_ref(re: &RefExpr, ctx: &FuncCtx, scope: &Scope) -> Result<Out, CalcError> {
    // A name resolves innermost-first: a LET binding / lambda parameter (case-
    // insensitive) shadows any defined name the resolver might know.
    let bound = match re {
        RefExpr::Name { name, sheet: None } => scope.lookup_bound(name),
        _ => None,
    };
    if let Some(b) = bound {
        return match b {
            Bound::Value(v) => Ok(Out::V(v)),
            Bound::Lambda(lv) => Ok(Out::L(lv)),
            Bound::Omitted => Err(CalcError::Value),
        };
    }
    ctx.resolve(re).map(Out::V)
}

fn eval_call(
    name: &str,
    call_args: &[Expr],
    ctx: &FuncCtx,
    scope: &Scope,
    frame: &[CalcValue],
    state: &mut EvalState,
) -> Result<Out, CalcError> {
    let up = name.to_ascii_uppercase();
    if LAMBDA_FAMILY.contains(&up.as_str()) {
        return eval_function(&up, call_args, ctx, scope, frame, state);
    }
    // A name bound to a callable wins over the registry (the function-name
    // spelling itself, e.g. `SUM`, cannot be shadowed — only the registry path
    // is consulted for those).
    if let Some(b) = scope.lookup_bound(name) {
        match b {
            Bound::Lambda(lv) => {
                let supplied = eval_args_to_values(call_args, ctx, scope, frame, state)?;
                return invoke_lambda(&lv, &supplied, ctx, state);
            }
            Bound::Value(_) | Bound::Omitted => {
                // Bound to a non-callable and called: Excel reports #VALUE!.
                return Err(CalcError::Value);
            }
        }
    }
    call_registry(&up, call_args, ctx, scope, frame, state).map(Out::V)
}

/// Evaluate `call_args` in the *caller's* scope into plain values. Argument
/// expressions belong to the call site, not to the callee.
fn eval_args_to_values(
    call_args: &[Expr],
    ctx: &FuncCtx,
    scope: &Scope,
    frame: &[CalcValue],
    state: &mut EvalState,
) -> Result<Vec<CalcValue>, CalcError> {
    let mut out = Vec::with_capacity(call_args.len());
    for e in call_args {
        out.push(eval_in(e, ctx, scope, frame, state)?.into_value()?);
    }
    Ok(out)
}

/// Invoke a lambda with already-evaluated arguments. Fewer arguments than
/// parameters is allowed (trailing parameters become OMITTED, detectable via
/// ISOMITTED); more is `#VALUE!`.
fn invoke_lambda(
    lv: &LambdaValue,
    supplied: &[CalcValue],
    ctx: &FuncCtx,
    state: &mut EvalState,
) -> Result<Out, CalcError> {
    if supplied.len() > lv.params.len() {
        return Err(CalcError::Value);
    }
    state.lambda_depth += 1;
    if state.lambda_depth > MAX_LAMBDA_DEPTH {
        state.lambda_depth -= 1;
        return Err(CalcError::Num);
    }
    // Fresh innermost frame over the lambda's captured scope; parameters land
    // here so they never leak into the captured scope or across invocations.
    let mut s = lv.captured.child();
    let mut frame: Vec<CalcValue> = Vec::with_capacity(supplied.len());
    for (i, p) in lv.params.iter().enumerate() {
        if let Some(v) = supplied.get(i) {
            s.bind(p, v.clone());
            frame.push(v.clone());
        } else {
            s.bind_omitted(p);
        }
    }
    let r = eval_in(&lv.body, ctx, &s, &frame, state);
    state.lambda_depth -= 1;
    r
}

/// Dispatch a lambda-family call. `name` is canonical upper-case. Nested calls
/// arrive through `eval_call` (with the current scope); top-level calls arrive
/// through the registry functions below (with a root scope).
fn eval_function(
    name: &str,
    args: &[Expr],
    ctx: &FuncCtx,
    scope: &Scope,
    frame: &[CalcValue],
    state: &mut EvalState,
) -> Result<Out, CalcError> {
    match name {
        "LAMBDA" => Ok(Out::L(Rc::new(lambda_from_args(args, scope)?))),
        "LET" => let_impl(args, ctx, scope, frame, state).map(Out::V),
        "MAP" => map_impl(args, ctx, scope, frame, state).map(Out::V),
        "REDUCE" => reduce_impl(args, ctx, scope, frame, state).map(Out::V),
        "SCAN" => scan_impl(args, ctx, scope, frame, state).map(Out::V),
        "BYROW" => byrow_impl(args, ctx, scope, frame, state).map(Out::V),
        "BYCOL" => bycol_impl(args, ctx, scope, frame, state).map(Out::V),
        "MAKEARRAY" => makearray_impl(args, ctx, scope, frame, state).map(Out::V),
        "ISOMITTED" => isomitted_impl(args, scope).map(Out::V),
        _ => unreachable!("not a lambda-family function: {name}"),
    }
}

/// The dispatch entry the registered function pointers funnel through. Each
/// top-level call starts from a root scope and a fresh budget.
fn dispatch(name: &str, ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    let mut exprs: Vec<Expr> = Vec::with_capacity(args.len());
    for a in args {
        match a {
            FuncArg::Expr(e) => exprs.push((**e).clone()),
            _ => return Err(CalcError::Value),
        }
    }
    let mut state = EvalState::new();
    let scope = Scope::root();
    eval_function(name, &exprs, ctx, &scope, &[], &mut state)?.into_value()
}

// ---------------------------------------------------------------------------
// The nine functions
// ---------------------------------------------------------------------------

/// LAMBDA(parameter, ..., calculation): the last argument is the body, every
/// preceding argument is a parameter name. The result is a callable value.
fn lambda_from_args(args: &[Expr], scope: &Scope) -> Result<LambdaValue, CalcError> {
    if args.is_empty() {
        return Err(CalcError::Value);
    }
    let (body, params_part) = args.split_last().expect("non-empty");
    let mut params = Vec::with_capacity(params_part.len());
    for a in params_part {
        params.push(param_name(a)?);
    }
    Ok(LambdaValue::new(params, body.clone(), scope.clone()))
}

/// The name of a LAMBDA/LET parameter: a bare, unqualified name reference.
fn param_name(e: &Expr) -> Result<String, CalcError> {
    match e {
        Expr::Ref(RefExpr::Name { name, sheet: None }) => Ok(name.clone()),
        _ => Err(CalcError::Value),
    }
}

/// LET(name1, value1, [name2, value2, ...], calculation).
///
/// The arity shape is validated EXPLICITLY: argument count must be odd and at
/// least 3, so the trailing calculation can never be mistaken for a binding's
/// value or vice versa. Bindings evaluate in order and a later value may
/// reference an earlier binding.
fn let_impl(
    args: &[Expr],
    ctx: &FuncCtx,
    scope: &Scope,
    frame: &[CalcValue],
    state: &mut EvalState,
) -> Result<CalcValue, CalcError> {
    if args.len() < 3 || (args.len() - 1) & 1 != 0 {
        return Err(CalcError::Value);
    }
    let mut s = scope.child();
    let n = (args.len() - 1) / 2;
    for i in 0..n {
        let name = param_name(&args[i * 2])?;
        match eval_in(&args[i * 2 + 1], ctx, &s, frame, state)? {
            Out::V(v) => s.bind(&name, v),
            Out::L(lv) => s.bind_lambda(&name, lv),
        }
    }
    eval_in(&args[args.len() - 1], ctx, &s, frame, state)?.into_value()
}

/// Build the callable a MAP/REDUCE/SCAN/BYROW/BYCOL/MAKEARRAY expects as its
/// final argument: an inline LAMBDA call, an `Expr::Lambda` node, or a name
/// bound to a lambda.
fn extract_lambda(arg: &Expr, scope: &Scope) -> Result<Rc<LambdaValue>, CalcError> {
    match arg {
        Expr::Function { name, args } if name.eq_ignore_ascii_case("LAMBDA") => {
            Ok(Rc::new(lambda_from_args(args, scope)?))
        }
        Expr::Lambda { params, body } => Ok(Rc::new(LambdaValue::new(
            params.clone(),
            (**body).clone(),
            scope.clone(),
        ))),
        Expr::Ref(RefExpr::Name { name, sheet: None }) => match scope.lookup_bound(name) {
            Some(Bound::Lambda(lv)) => Ok(lv),
            _ => Err(CalcError::Value),
        },
        _ => Err(CalcError::Value),
    }
}

/// Treat a value as a dense array: arrays pass through, scalars become 1x1.
fn as_array(v: CalcValue) -> Arc<ArrayValue> {
    match v {
        CalcValue::Array(a) => a,
        other => Arc::new(ArrayValue::new(1, 1, vec![other])),
    }
}

/// A result that must be a scalar: a 1x1 array unwraps, a larger array is the
/// BYROW/BYCOL contract violation `#VALUE!`.
fn scalarize_1x1(v: CalcValue) -> Result<CalcValue, CalcError> {
    match v {
        CalcValue::Array(a) if a.is_scalar_array() => Ok(a.get(0, 0).clone()),
        CalcValue::Array(_) => Err(CalcError::Value),
        other => Ok(other),
    }
}

/// MAP(array1, [array2, ...], lambda): apply the lambda element-wise. Every
/// array must have identical dimensions, else `#VALUE!`. Errors inside an
/// input land in the output cell when the body propagates them (so
/// `IFERROR(x, dflt)` still works per cell), matching Excel's MAP.
fn map_impl(
    args: &[Expr],
    ctx: &FuncCtx,
    scope: &Scope,
    frame: &[CalcValue],
    state: &mut EvalState,
) -> Result<CalcValue, CalcError> {
    if args.len() < 2 {
        return Err(CalcError::Value);
    }
    let (lambda_arg, data_args) = args.split_last().expect("non-empty");
    let lv = extract_lambda(lambda_arg, scope)?;
    let mut arrays: Vec<Arc<ArrayValue>> = Vec::with_capacity(data_args.len());
    for a in data_args {
        let v = eval_in(a, ctx, scope, frame, state)?.into_value()?;
        arrays.push(as_array(v));
    }
    let (rows, cols) = arrays[0].shape();
    for a in &arrays[1..] {
        if a.shape() != (rows, cols) {
            return Err(CalcError::Value);
        }
    }
    if rows as u64 * cols as u64 > MAX_CELLS {
        return Err(CalcError::Value);
    }
    let mut out = Vec::with_capacity((rows as usize) * (cols as usize));
    for r in 0..rows {
        for c in 0..cols {
            let mut supplied = Vec::with_capacity(arrays.len());
            for a in &arrays {
                supplied.push(a.get(r, c).clone());
            }
            let cell = match invoke_lambda(&lv, &supplied, ctx, state) {
                Ok(Out::V(v)) => v,
                Ok(Out::L(_)) => CalcValue::Error(CalcError::Calc),
                Err(e) => CalcValue::Error(e),
            };
            out.push(cell);
        }
    }
    Ok(CalcValue::array(ArrayValue::new(rows, cols, out)))
}

/// REDUCE(initial, array, lambda): fold with `lambda(accumulator, value)` in
/// row-major order. The final accumulator is a scalar. Errors abort the fold.
fn reduce_impl(
    args: &[Expr],
    ctx: &FuncCtx,
    scope: &Scope,
    frame: &[CalcValue],
    state: &mut EvalState,
) -> Result<CalcValue, CalcError> {
    if args.len() != 3 {
        return Err(CalcError::Value);
    }
    let init = eval_in(&args[0], ctx, scope, frame, state)?.into_value()?;
    let array = as_array(eval_in(&args[1], ctx, scope, frame, state)?.into_value()?);
    let lv = extract_lambda(&args[2], scope)?;
    let mut acc = init;
    for v in array.iter() {
        acc = invoke_lambda(&lv, &[acc, v.clone()], ctx, state)?.into_value()?;
    }
    Ok(acc)
}

/// SCAN(initial, array, lambda): REDUCE, but the result is the array of every
/// intermediate accumulator, one per input cell.
fn scan_impl(
    args: &[Expr],
    ctx: &FuncCtx,
    scope: &Scope,
    frame: &[CalcValue],
    state: &mut EvalState,
) -> Result<CalcValue, CalcError> {
    if args.len() != 3 {
        return Err(CalcError::Value);
    }
    let init = eval_in(&args[0], ctx, scope, frame, state)?.into_value()?;
    let array = as_array(eval_in(&args[1], ctx, scope, frame, state)?.into_value()?);
    let lv = extract_lambda(&args[2], scope)?;
    let (rows, cols) = array.shape();
    if rows as u64 * cols as u64 > MAX_CELLS {
        return Err(CalcError::Value);
    }
    let mut out = Vec::with_capacity(array.data.len());
    let mut acc = init;
    for v in array.iter() {
        let cell = match invoke_lambda(&lv, &[acc.clone(), v.clone()], ctx, state) {
            Ok(Out::V(next)) => {
                acc = next.clone();
                next
            }
            Ok(Out::L(_)) => CalcValue::Error(CalcError::Calc),
            Err(e) => CalcValue::Error(e),
        };
        out.push(cell);
    }
    Ok(CalcValue::array(ArrayValue::new(rows, cols, out)))
}

/// BYROW(array, lambda): call the lambda once per row with the row as a 1 x n
/// array; each result must be a scalar, collected into an n x 1 column vector.
fn byrow_impl(
    args: &[Expr],
    ctx: &FuncCtx,
    scope: &Scope,
    frame: &[CalcValue],
    state: &mut EvalState,
) -> Result<CalcValue, CalcError> {
    if args.len() != 2 {
        return Err(CalcError::Value);
    }
    let array = as_array(eval_in(&args[0], ctx, scope, frame, state)?.into_value()?);
    let lv = extract_lambda(&args[1], scope)?;
    let rows = array.rows;
    let mut out = Vec::with_capacity(rows as usize);
    for r in 0..rows {
        let row_vals: Vec<CalcValue> = (0..array.cols).map(|c| array.get(r, c).clone()).collect();
        let row = CalcValue::array(ArrayValue::new(1, array.cols, row_vals));
        let v = invoke_lambda(&lv, &[row], ctx, state)?;
        let v = match v {
            Out::V(v) => scalarize_1x1(v)?,
            Out::L(_) => return Err(CalcError::Calc),
        };
        out.push(v);
    }
    Ok(CalcValue::array(ArrayValue::new(rows, 1, out)))
}

/// BYCOL(array, lambda): the transpose of BYROW — one call per column with the
/// column as an m x 1 array, collected into a 1 x n row vector.
fn bycol_impl(
    args: &[Expr],
    ctx: &FuncCtx,
    scope: &Scope,
    frame: &[CalcValue],
    state: &mut EvalState,
) -> Result<CalcValue, CalcError> {
    if args.len() != 2 {
        return Err(CalcError::Value);
    }
    let array = as_array(eval_in(&args[0], ctx, scope, frame, state)?.into_value()?);
    let lv = extract_lambda(&args[1], scope)?;
    let cols = array.cols;
    let mut out = Vec::with_capacity(cols as usize);
    for c in 0..cols {
        let col_vals: Vec<CalcValue> = (0..array.rows).map(|r| array.get(r, c).clone()).collect();
        let col = CalcValue::array(ArrayValue::new(array.rows, 1, col_vals));
        let v = invoke_lambda(&lv, &[col], ctx, state)?;
        let v = match v {
            Out::V(v) => scalarize_1x1(v)?,
            Out::L(_) => return Err(CalcError::Calc),
        };
        out.push(v);
    }
    Ok(CalcValue::array(ArrayValue::new(1, cols, out)))
}

/// MAKEARRAY(rows, cols, lambda): call `lambda(row_index, col_index)`, both
/// 1-based, for every cell. `rows`/`cols` are positive integers.
fn makearray_impl(
    args: &[Expr],
    ctx: &FuncCtx,
    scope: &Scope,
    frame: &[CalcValue],
    state: &mut EvalState,
) -> Result<CalcValue, CalcError> {
    if args.len() != 3 {
        return Err(CalcError::Value);
    }
    let rows = dimension(&eval_in(&args[0], ctx, scope, frame, state)?.into_value()?)?;
    let cols = dimension(&eval_in(&args[1], ctx, scope, frame, state)?.into_value()?)?;
    let lv = extract_lambda(&args[2], scope)?;
    if rows as u64 * cols as u64 > MAX_CELLS {
        return Err(CalcError::Value);
    }
    let mut out = Vec::with_capacity(rows * cols);
    for r in 1..=rows {
        for c in 1..=cols {
            let supplied = vec![CalcValue::number(r as f64), CalcValue::number(c as f64)];
            let cell = match invoke_lambda(&lv, &supplied, ctx, state) {
                Ok(Out::V(v)) => v,
                Ok(Out::L(_)) => CalcValue::Error(CalcError::Calc),
                Err(e) => CalcValue::Error(e),
            };
            out.push(cell);
        }
    }
    Ok(CalcValue::array(ArrayValue::new(
        rows as u32,
        cols as u32,
        out,
    )))
}

/// Coerce a MAKEARRAY dimension: a finite integer ≥ 1 (truncated toward zero).
fn dimension(v: &CalcValue) -> Result<usize, CalcError> {
    let n = coerce_number(v)?.trunc();
    if !(1.0..=1_048_576.0).contains(&n) {
        return Err(CalcError::Value);
    }
    Ok(n as usize)
}

/// ISOMITTED(arg): TRUE only when the argument is a lambda parameter that was
/// not supplied (trailing parameters of an under-supplied invocation). Any
/// other argument — a supplied value, a literal, a reference — is FALSE.
fn isomitted_impl(args: &[Expr], scope: &Scope) -> Result<CalcValue, CalcError> {
    if args.len() != 1 {
        return Err(CalcError::Value);
    }
    let omitted = match &args[0] {
        Expr::Ref(RefExpr::Name { name, sheet: None }) => {
            matches!(scope.lookup_bound(name), Some(Bound::Omitted))
        }
        _ => false,
    };
    Ok(CalcValue::bool(omitted))
}

// ---------------------------------------------------------------------------
// Operators (mirror `calc/eval.rs` so bodies behave identically to top-level)
// ---------------------------------------------------------------------------

fn apply_binary(op: BinaryOp, l: CalcValue, r: CalcValue) -> Result<CalcValue, CalcError> {
    // Error operands propagate immediately, left side first.
    if let CalcValue::Error(e) = l {
        return Err(e);
    }
    if let CalcValue::Error(e) = r {
        return Err(e);
    }
    if l.is_array() || r.is_array() {
        broadcast_binary(op, l, r)
    } else {
        scalar_binary(op, l, r)
    }
}

fn scalar_binary(op: BinaryOp, l: CalcValue, r: CalcValue) -> Result<CalcValue, CalcError> {
    match op {
        BinaryOp::Add => numeric_binary(l, r, |a, b| a + b),
        BinaryOp::Sub => numeric_binary(l, r, |a, b| a - b),
        BinaryOp::Mul => numeric_binary(l, r, |a, b| a * b),
        BinaryOp::Div => {
            let a = coerce_number(&l)?;
            let b = coerce_number(&r)?;
            if b == 0.0 {
                return Err(CalcError::Div0);
            }
            finite_result(a / b)
        }
        BinaryOp::Pow => {
            let a = coerce_number(&l)?;
            let b = coerce_number(&r)?;
            finite_result(a.powf(b))
        }
        BinaryOp::Concat => {
            let a = coerce_text(&l)?;
            let b = coerce_text(&r)?;
            Ok(CalcValue::text(a + &b))
        }
        BinaryOp::Eq => compare_eq(&l, &r, false).map(CalcValue::Bool),
        BinaryOp::Ne => compare_eq(&l, &r, false).map(|b| CalcValue::Bool(!b)),
        BinaryOp::Gt => compare(&l, &r).map(|o| CalcValue::Bool(o == Ordering::Greater)),
        BinaryOp::Ge => compare(&l, &r).map(|o| CalcValue::Bool(o != Ordering::Less)),
        BinaryOp::Lt => compare(&l, &r).map(|o| CalcValue::Bool(o == Ordering::Less)),
        BinaryOp::Le => compare(&l, &r).map(|o| CalcValue::Bool(o != Ordering::Greater)),
    }
}

fn numeric_binary(
    l: CalcValue,
    r: CalcValue,
    f: impl Fn(f64, f64) -> f64,
) -> Result<CalcValue, CalcError> {
    let a = coerce_number(&l)?;
    let b = coerce_number(&r)?;
    finite_result(f(a, b))
}

fn finite_result(n: f64) -> Result<CalcValue, CalcError> {
    if n.is_finite() {
        Ok(CalcValue::Number(n))
    } else {
        Err(CalcError::Num)
    }
}

fn broadcast_binary(op: BinaryOp, l: CalcValue, r: CalcValue) -> Result<CalcValue, CalcError> {
    let a = match l {
        CalcValue::Array(a) => a,
        other => Arc::new(ArrayValue::new(1, 1, vec![other])),
    };
    let b = match r {
        CalcValue::Array(a) => a,
        other => Arc::new(ArrayValue::new(1, 1, vec![other])),
    };
    let rows = a.rows.max(b.rows);
    let cols = a.cols.max(b.cols);
    if rows as u64 * cols as u64 > MAX_CELLS {
        return Err(CalcError::Value);
    }
    let mut data = Vec::with_capacity((rows * cols) as usize);
    for i in 0..rows {
        for j in 0..cols {
            let elem = match (
                broadcast_index(&a, rows, cols, i, j),
                broadcast_index(&b, rows, cols, i, j),
            ) {
                (Some(x), Some(y)) => match scalar_binary(op, x, y) {
                    Ok(v) => v,
                    Err(e) => CalcValue::Error(e),
                },
                _ => CalcValue::Error(CalcError::Na),
            };
            data.push(elem);
        }
    }
    Ok(CalcValue::array(ArrayValue::new(rows, cols, data)))
}

/// Pick the operand cell for result cell `(i, j)` of a broadcast to `rows x
/// cols`. A dimension of 1 broadcasts across the whole axis; a dimension equal
/// to the target pairs element-wise; anything else is out of range → `None`.
fn broadcast_index(a: &ArrayValue, rows: u32, cols: u32, i: u32, j: u32) -> Option<CalcValue> {
    let ri = if a.rows == 1 {
        0
    } else if a.rows == rows {
        i
    } else {
        return None;
    };
    let ci = if a.cols == 1 {
        0
    } else if a.cols == cols {
        j
    } else {
        return None;
    };
    Some(a.get(ri, ci).clone())
}

fn apply_unary(op: UnaryOp, v: CalcValue, ctx: &FuncCtx) -> Result<CalcValue, CalcError> {
    match op {
        UnaryOp::ImplicitIntersect => intersect_or_pass(v, ctx),
        UnaryOp::Plus | UnaryOp::Minus => {
            let negate = op == UnaryOp::Minus;
            match v {
                CalcValue::Array(a) => {
                    let rows = a.rows;
                    let cols = a.cols;
                    if rows as u64 * cols as u64 > MAX_CELLS {
                        return Err(CalcError::Value);
                    }
                    let mut data = Vec::with_capacity((rows * cols) as usize);
                    for e in a.iter() {
                        match coerce_number(e) {
                            Ok(n) => data.push(CalcValue::Number(if negate { -n } else { n })),
                            Err(err) => data.push(CalcValue::Error(err)),
                        }
                    }
                    Ok(CalcValue::array(ArrayValue::new(rows, cols, data)))
                }
                other => {
                    coerce_number(&other).map(|n| CalcValue::Number(if negate { -n } else { n }))
                }
            }
        }
    }
}

fn intersect_or_pass(v: CalcValue, ctx: &FuncCtx) -> Result<CalcValue, CalcError> {
    match v {
        CalcValue::Array(a) => implicit_intersect(&a, ctx.row, ctx.col),
        other => Ok(other),
    }
}

fn implicit_intersect(a: &ArrayValue, row: u32, col: u32) -> Result<CalcValue, CalcError> {
    let (r, c) = match (a.rows, a.cols) {
        (1, 1) => (0, 0),
        (1, _) => (0, col),
        (_, 1) => (row, 0),
        _ => (row, col),
    };
    if r < a.rows && c < a.cols {
        Ok(a.get(r, c).clone())
    } else {
        Err(CalcError::Value)
    }
}

fn percent(v: CalcValue) -> Result<CalcValue, CalcError> {
    Ok(CalcValue::Number(coerce_number(&v)? / 100.0))
}

// ---------------------------------------------------------------------------
// Function calls to the ordinary registry from inside a body
// ---------------------------------------------------------------------------

fn call_registry(
    name: &str,
    call_args: &[Expr],
    ctx: &FuncCtx,
    scope: &Scope,
    frame: &[CalcValue],
    state: &mut EvalState,
) -> Result<CalcValue, CalcError> {
    let spec = registry().get(name).ok_or(CalcError::Name)?;
    spec.validate(call_args.len())?;
    let mut cargs: Vec<FuncArg> = Vec::with_capacity(call_args.len());
    for arg in call_args {
        match arg {
            // A scope-bound name must be evaluated to its value; a bare grid
            // reference stays unevaluated so range-consuming functions
            // (SUM/COUNTIF/ROW/...) keep working exactly as at the top level.
            // An evaluation error becomes an error VALUE (not a propagated
            // `Err`), so IF/IFERROR can inspect or discard it — matching the
            // eager-argument path below and `eval.rs`.
            Expr::Ref(RefExpr::Name {
                name: n,
                sheet: None,
            }) if scope.lookup_bound(n).is_some() => {
                let v = match eval_in(arg, ctx, scope, frame, state) {
                    Ok(Out::V(v)) => v,
                    Ok(Out::L(_)) => return Err(CalcError::Calc),
                    Err(e) => CalcValue::Error(e),
                };
                cargs.push(FuncArg::Value(v));
            }
            Expr::Ref(re) => cargs.push(FuncArg::Reference(re.clone())),
            _ => {
                let v = match eval_in(arg, ctx, scope, frame, state) {
                    Ok(Out::V(v)) => v,
                    Ok(Out::L(_)) => return Err(CalcError::Calc),
                    Err(e) => CalcValue::Error(e),
                };
                cargs.push(FuncArg::Value(v));
            }
        }
    }
    if !spec.array_aware {
        for arg in cargs.iter_mut() {
            match arg {
                FuncArg::Value(v) if v.is_array() => {
                    *v = scalarize_value(v.clone(), ctx)?;
                }
                _ => {}
            }
        }
    }
    (spec.func)(ctx, &cargs)
}

fn scalarize_value(v: CalcValue, ctx: &FuncCtx) -> Result<CalcValue, CalcError> {
    match v {
        CalcValue::Array(a) => implicit_intersect(&a, ctx.row, ctx.col),
        other => Ok(other),
    }
}

// ---------------------------------------------------------------------------
// Registry plumbing
// ---------------------------------------------------------------------------

fn lambda_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    dispatch("LAMBDA", ctx, args)
}
fn let_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    dispatch("LET", ctx, args)
}
fn map_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    dispatch("MAP", ctx, args)
}
fn reduce_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    dispatch("REDUCE", ctx, args)
}
fn scan_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    dispatch("SCAN", ctx, args)
}
fn byrow_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    dispatch("BYROW", ctx, args)
}
fn bycol_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    dispatch("BYCOL", ctx, args)
}
fn makearray_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    dispatch("MAKEARRAY", ctx, args)
}
fn isomitted_fn(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
    dispatch("ISOMITTED", ctx, args)
}

/// The nine specs, each forced to `'static` (they live for the process).
const LAMBDA_SPEC: FuncSpec = FuncSpec {
    name: "LAMBDA",
    min_args: 1,
    max_args: None,
    volatile: false,
    array_aware: true,
    func: lambda_fn,
};
const LET_SPEC: FuncSpec = FuncSpec {
    name: "LET",
    min_args: 3,
    max_args: None,
    volatile: false,
    array_aware: true,
    func: let_fn,
};
const MAP_SPEC: FuncSpec = FuncSpec {
    name: "MAP",
    min_args: 2,
    max_args: None,
    volatile: false,
    array_aware: true,
    func: map_fn,
};
const REDUCE_SPEC: FuncSpec = FuncSpec {
    name: "REDUCE",
    min_args: 3,
    max_args: Some(3),
    volatile: false,
    array_aware: true,
    func: reduce_fn,
};
const SCAN_SPEC: FuncSpec = FuncSpec {
    name: "SCAN",
    min_args: 3,
    max_args: Some(3),
    volatile: false,
    array_aware: true,
    func: scan_fn,
};
const BYROW_SPEC: FuncSpec = FuncSpec {
    name: "BYROW",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: true,
    func: byrow_fn,
};
const BYCOL_SPEC: FuncSpec = FuncSpec {
    name: "BYCOL",
    min_args: 2,
    max_args: Some(2),
    volatile: false,
    array_aware: true,
    func: bycol_fn,
};
const MAKEARRAY_SPEC: FuncSpec = FuncSpec {
    name: "MAKEARRAY",
    min_args: 3,
    max_args: Some(3),
    volatile: false,
    array_aware: true,
    func: makearray_fn,
};
const ISOMITTED_SPEC: FuncSpec = FuncSpec {
    name: "ISOMITTED",
    min_args: 1,
    max_args: Some(1),
    volatile: false,
    array_aware: true,
    func: isomitted_fn,
};

/// Register the lambda family. NOTE FOR THE COORDINATOR: call this from
/// `calc/functions/mod.rs` `build()` (a `crate::turbo::calc::lambda::register`
/// line) and declare `mod lambda;` in `calc/mod.rs`.
pub fn register(r: &mut Registry) {
    r.register(&LAMBDA_SPEC);
    r.register(&LET_SPEC);
    r.register(&MAP_SPEC);
    r.register(&REDUCE_SPEC);
    r.register(&SCAN_SPEC);
    r.register(&BYROW_SPEC);
    r.register(&BYCOL_SPEC);
    r.register(&MAKEARRAY_SPEC);
    r.register(&ISOMITTED_SPEC);
}

// ---------------------------------------------------------------------------
// Tests — driven through the real parse-then-eval path (calc/testkit.rs)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turbo::calc::testkit::{Grid, Outcome};

    fn arr(rows: u32, cols: u32, data: Vec<f64>) -> CalcValue {
        CalcValue::array(ArrayValue::new(
            rows,
            cols,
            data.into_iter().map(CalcValue::number).collect(),
        ))
    }

    fn num(g: &Grid, formula: &str) -> f64 {
        g.num(formula)
    }

    #[test]
    fn basic_lambda_via_map() {
        // 1x3 array literal, doubled element-wise.
        let g = Grid::empty();
        match g.calc("=MAP({1,2,3}, LAMBDA(x, x*2))") {
            Outcome::Value(v) => assert_eq!(v, arr(1, 3, vec![2.0, 4.0, 6.0])),
            other => panic!("{other:?}"),
        }
        // through a real range on the grid, summed back to a scalar
        let g = Grid::empty().col("A1", &[1.0, 2.0, 3.0]);
        assert_eq!(num(&g, "=SUM(MAP(A1:A3, LAMBDA(x, x*2)))"), 12.0);
    }

    #[test]
    fn let_bindings_in_order_with_forward_reference() {
        let g = Grid::empty();
        assert_eq!(num(&g, "=LET(n, 5, m, n*2, n+m)"), 15.0);
        assert_eq!(num(&g, "=LET(a, 1, b, 2, c, 3, a+b+c)"), 6.0);
        // the odd/even arity trap: 4 arguments is 2 bindings and NO calc
        assert_eq!(g.error("=LET(a, 1, b, 2)"), CalcError::Value);
    }

    #[test]
    fn reduce_sums_an_array() {
        let g = Grid::empty();
        assert_eq!(num(&g, "=REDUCE(0, {1,2,3,4}, LAMBDA(a, b, a+b))"), 10.0);
    }

    #[test]
    fn scan_returns_running_totals() {
        let g = Grid::empty();
        match g.calc("=SCAN(0, {1,2,3}, LAMBDA(a, b, a+b))") {
            Outcome::Value(v) => assert_eq!(v, arr(1, 3, vec![1.0, 3.0, 6.0])),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn byrow_and_bycol_pass_whole_rows_and_columns() {
        let g = Grid::empty();
        // BYROW over a 2x3 array: sums of rows {1,2,3} and {4,5,6} -> column vector
        match g.calc("=BYROW({1,2,3;4,5,6}, LAMBDA(r, SUM(r)))") {
            Outcome::Value(v) => assert_eq!(v, arr(2, 1, vec![6.0, 15.0])),
            other => panic!("{other:?}"),
        }
        // BYCOL over the same array: sums of columns {1,4} {2,5} {3,6} -> row vector
        match g.calc("=BYCOL({1,2,3;4,5,6}, LAMBDA(c, SUM(c)))") {
            Outcome::Value(v) => assert_eq!(v, arr(1, 3, vec![5.0, 7.0, 9.0])),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn makearray_builds_a_multiplication_table() {
        let g = Grid::empty();
        match g.calc("=MAKEARRAY(3, 3, LAMBDA(r, c, r*c))") {
            Outcome::Value(v) => assert_eq!(
                v,
                arr(3, 3, vec![1.0, 2.0, 3.0, 2.0, 4.0, 6.0, 3.0, 6.0, 9.0])
            ),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parameter_shadows_outer_binding_and_capture_is_lexical() {
        let g = Grid::empty();
        // the lambda's `x` hides the outer `x = 10`
        assert_eq!(
            num(&g, "=LET(x, 10, SUM(MAP({1,2,3}, LAMBDA(x, x+1))))"),
            9.0
        );
        // without a shadowing param the outer binding is captured
        assert_eq!(
            num(&g, "=LET(x, 10, SUM(MAP({1,2,3}, LAMBDA(y, x+y))))"),
            36.0
        );
    }

    #[test]
    fn wrong_argument_count_is_value_error() {
        let g = Grid::empty();
        // LAMBDA takes 1 parameter; called with 2 -> #VALUE!
        assert_eq!(
            g.error("=LET(f, LAMBDA(x, x*2), f(1, 2))"),
            CalcError::Value
        );
        // a bare LAMBDA with no body is #VALUE! too
        assert_eq!(g.error("=LAMBDA()"), CalcError::Value);
    }

    #[test]
    fn self_recursive_lambda_terminates_with_num_not_a_hang() {
        let g = Grid::empty();
        // no base case: recursion is provably bounded by the depth cap
        assert_eq!(g.error("=LET(f, LAMBDA(x, f(x+1)), f(0))"), CalcError::Num);
    }

    #[test]
    fn recursion_through_let_actually_works() {
        let g = Grid::empty();
        // terminating recursion: f(4) = 4 + f(3) ... = 10
        assert_eq!(
            num(&g, "=LET(f, LAMBDA(n, IF(n<=0, 0, n + f(n-1))), f(4))"),
            10.0
        );
    }

    #[test]
    fn isomitted_detects_unsupplied_parameters() {
        let g = Grid::empty();
        assert_eq!(
            g.text("=LET(f, LAMBDA(x, IF(ISOMITTED(x), \"none\", x)), f())"),
            "none"
        );
        assert_eq!(
            g.text("=LET(f, LAMBDA(x, IF(ISOMITTED(x), \"none\", x)), f(\"yes\"))"),
            "yes"
        );
        // ISOMITTED of a supplied literal is FALSE
        assert!(!g.boolean("=LET(f, LAMBDA(x, ISOMITTED(x)), f(1))"));
    }

    #[test]
    fn map_over_multiple_arrays_and_mismatched_shapes() {
        let g = Grid::empty();
        // two equal-length arrays combine element-wise
        assert_eq!(
            g.num("=SUM(MAP({1,2,3}, {10,20,30}, LAMBDA(a, b, a+b)))"),
            66.0
        );
        // mismatched shapes are #VALUE!
        assert_eq!(
            g.error("=MAP({1,2,3}, {1,2}, LAMBDA(a, b, a+b))"),
            CalcError::Value
        );
    }

    #[test]
    fn byrow_result_must_be_scalar() {
        let g = Grid::empty();
        // LAMBDA(r, r) returns the 1 x n row array -> #VALUE!
        assert_eq!(g.error("=BYROW({1,2;3,4}, LAMBDA(r, r))"), CalcError::Value);
    }

    #[test]
    fn lambda_param_must_be_a_name() {
        let g = Grid::empty();
        // LAMBDA with a numeric "parameter" is #VALUE!
        assert_eq!(g.error("=LET(f, LAMBDA(5, 6), f(1))"), CalcError::Value);
    }

    #[test]
    fn nested_lambda_captures_enclosing_parameters() {
        let g = Grid::empty();
        // the inner lambda references the outer lambda's `x` through capture:
        // x=1 -> 1+10, x=2 -> 2+10, summed = 23
        assert_eq!(
            g.num("=SUM(MAP({1,2}, LAMBDA(x, SUM(MAP({10}, LAMBDA(y, x+y))))))"),
            23.0
        );
    }

    #[test]
    fn errors_in_map_inputs_become_cell_errors() {
        let g = Grid::empty();
        // the #N/A element passes through the body and lands in its cell
        assert_eq!(
            g.calc("=MAP({1,#N/A,3}, LAMBDA(x, x*2))"),
            Outcome::Value(CalcValue::array(ArrayValue::new(
                1,
                3,
                vec![
                    CalcValue::number(2.0),
                    CalcValue::err(CalcError::Na),
                    CalcValue::number(6.0),
                ],
            )))
        );
    }
}
