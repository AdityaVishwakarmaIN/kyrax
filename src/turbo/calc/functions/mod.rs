// calc/functions/mod.rs — the function registry contract.
//
// This module is the SINGLE place that knows the complete list of worksheet
// function families. It is written once and never edited by family agents:
//
//     math, statistical, logical, text, datetime, information, lookup,
//     financial, engineering
//
// Each family lives in its own file (`functions/<name>.rs`) and exposes one
// hook — `pub fn register(r: &mut Registry)` — which `build()` calls in the
// fixed order below. Family agents run concurrently and MUST never write the
// same file, so they edit only their own file; the `mod <name>;` lines and
// the `<name>::register(&mut r)` call sites here are already final. Adding a
// tenth family is a coordinated, sequential change to this file.
//
// # How the eval loop drives one function call
//
// 1. Lookup: `registry().get(&name)`. Names are matched case-insensitively
//    (Excel function names are case-insensitive). An unregistered name
//    returns `None`; the caller MUST route that cell to the uncomputed
//    fallback path (`CalcReport::fallback` / `fullCalcOnLoad="1"`) — never a
//    guessed value.
// 2. Arity: `spec.validate(args.len())`; an out-of-range count is
//    `Err(CalcError::Value)`.
// 3. Args: a bare reference/range argument (`Expr::Ref(..)`) is passed as
//    `FuncArg::Reference(RefExpr)` WITHOUT evaluating it — COUNTIF/SUMIF need
//    the range itself, ROW/COLUMN need the coordinates, and reference errors
//    (`#REF!`, external refs, unknown sheets) are the function's to surface
//    via `FuncCtx::resolve`. Every other argument is evaluated and passed as
//    `FuncArg::Value`. Simple functions never handle references explicitly:
//    `arg.value(ctx)?` resolves a `Reference` to its materialized value and
//    returns a `Value` untouched (one line per argument).
// 4. Invoke: `spec.func(&ctx, &args)`.
// 5. Caching: a `volatile: true` result must never be cached as `<v>` — the
//    loop recomputes it every pass. An `array_aware: false` function must
//    never receive `Array` arguments; the loop scalarizes 1x1 arrays and
//    applies the implicit-intersection / `#VALUE!` policy to larger ones.
// 6. Errors: any returned `CalcError` propagates as the cell's result;
//    internal-only codes route the cell to the fallback path.
//
// # Worked example — ABS (the template family agents copy)
//
// Lives in `functions/math.rs`. One line per argument, errors propagate,
// then register the spec. This is everything a simple function needs.
//
// ```rust,ignore
// use super::{FuncArg, FuncCtx, FuncSpec, Registry};
// use crate::turbo::calc::coerce::coerce_number;
// use crate::turbo::calc::value::{CalcError, CalcValue};
//
// /// |x| for a single number. Fixed arity 1, non-volatile, not array-aware.
// pub fn abs(ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
//     let n = coerce_number(&args[0].value(ctx)?)?;
//     Ok(CalcValue::number(n.abs()))
// }
//
// const ABS: FuncSpec = FuncSpec {
//     name: "ABS",
//     min_args: 1,
//     max_args: Some(1),
//     volatile: false,
//     array_aware: false,
//     func: abs,
// };
//
// pub fn register(r: &mut Registry) {
//     r.register(&ABS);
//     // ... the rest of the math family: one const + one register line each ...
// }
// ```
//
// ROW/COLUMN/COUNTIF-style functions instead read `arg.as_reference()`,
// take the coordinates from the `RefExpr`, and fetch values cell-by-cell via
// `ctx.cell(row, col)` — never by materializing a huge range.

use crate::turbo::calc::ast::{RefCore, RefExpr};
use crate::turbo::calc::value::{ArrayValue, CalcError, CalcValue};
use std::collections::HashMap;
use std::sync::OnceLock;

mod database;
mod datetime;
mod dynamic;
mod engineering;
mod financial;
mod information;
mod logical;
mod lookup;
mod math;
mod statistical;
mod text;

/// Excel grid limits, used to bound whole-row/column materialization.
pub const MAX_ROWS: u32 = 1_048_576;
pub const MAX_COLS: u16 = 16_384;

/// Maximum cells a single `resolve` call will materialize into a dense array
/// (~4M cells ≈ 64 MB of `CalcValue`). Larger spans error with `#VALUE!`
/// instead of risking a huge allocation; whole-row/column and huge-range
/// consumers should iterate via `CellResolver::cell` instead.
const MAX_MATERIALIZED_CELLS: usize = 4_000_000;

/// One call argument: either an already-evaluated value or a reference the
/// function may consume raw.
///
/// The eval loop constructs this: a bare reference/range AST node becomes
/// `Reference` (unevaluated); everything else becomes `Value`. Simple
/// functions call [`FuncArg::value`] and never touch the distinction;
/// lookup/coordinate functions call [`FuncArg::as_reference`].
#[derive(Clone, Debug)]
pub enum FuncArg {
    Value(CalcValue),
    Reference(RefExpr),
}

impl FuncArg {
    /// The argument's value, resolving a `Reference` through the resolver.
    /// A range resolves to a dense [`CalcValue::Array`]; a single cell to its
    /// value (blank when empty). Errors (external ref, unknown sheet, 3-D)
    /// propagate so the function short-circuits like any other error.
    pub fn value(&self, ctx: &FuncCtx) -> Result<CalcValue, CalcError> {
        match self {
            FuncArg::Value(v) => Ok(v.clone()),
            FuncArg::Reference(re) => ctx.resolve(re),
        }
    }

    /// The raw reference, if this argument was an unevaluated reference.
    /// `None` for everything else (literals, results of operators, array
    /// literals, ...).
    pub fn as_reference(&self) -> Option<&RefExpr> {
        match self {
            FuncArg::Reference(re) => Some(re),
            FuncArg::Value(_) => None,
        }
    }
}

/// The seam that reads cell values out of the workbook grid. Implemented by
/// the eval loop over the workbook; `calc/` stays language-agnostic and never
/// touches the workbook model directly.
///
/// Formula cells referenced by a function MUST already carry a computed
/// cache: the loop resolves dependencies before invoking the function, and
/// this resolver never triggers evaluation itself.
pub trait CellResolver {
    /// One cell's value. `None` means blank / empty / outside the used range.
    fn cell(&self, sheet: u32, row: u32, col: u32) -> Option<CalcValue>;

    /// The workbook sheet index for a name, or `None` if no such sheet.
    fn sheet_index(&self, name: &str) -> Option<u32>;

    /// Resolve a reference expression to its value. Provided default: local
    /// and `Sheet!` refs are handled; 3-D, table and external refs error with
    /// `#REF!`, unresolvable names with `#NAME?` — honest errors, never a
    /// guessed value. Implementers may override to add name/table support.
    fn resolve_ref(&self, current_sheet: u32, re: &RefExpr) -> Result<CalcValue, CalcError> {
        match re {
            RefExpr::Local(core) => self.resolve_core(current_sheet, core),
            RefExpr::Sheet { name, inner } => {
                let idx = self.sheet_index(name).ok_or(CalcError::Ref)?;
                self.resolve_core(idx, inner)
            }
            RefExpr::Sheet3D { .. } | RefExpr::External { .. } | RefExpr::Table(_) => {
                Err(CalcError::Ref)
            }
            RefExpr::Name { .. } => Err(CalcError::Name),
        }
    }

    /// Materialize one cartesian core reference. Cells read as `Blank` when
    /// empty; ranges become dense arrays (blanks preserved as
    /// [`CalcValue::Blank`]).
    fn resolve_core(&self, sheet: u32, core: &RefCore) -> Result<CalcValue, CalcError> {
        match core {
            RefCore::Cell(c) => Ok(self
                .cell(sheet, c.row, u32::from(c.col))
                .unwrap_or(CalcValue::Blank)),
            RefCore::Range(r) => {
                if r.end.row < r.start.row || r.end.col < r.start.col {
                    return Err(CalcError::Value);
                }
                self.dense(sheet, r.start.row..=r.end.row, r.start.col..=r.end.col)
            }
            RefCore::Row(r) => {
                if r.end < r.start {
                    return Err(CalcError::Value);
                }
                self.dense(sheet, r.start..=r.end, 0..=MAX_COLS - 1)
            }
            RefCore::Column(c) => {
                if c.end < c.start {
                    return Err(CalcError::Value);
                }
                self.dense(sheet, 0..=MAX_ROWS - 1, c.start..=c.end)
            }
        }
    }

    /// Build a dense row-major array, refusing spans that would exceed
    /// [`MAX_MATERIALIZED_CELLS`] instead of risking a huge allocation.
    fn dense(
        &self,
        sheet: u32,
        rows: std::ops::RangeInclusive<u32>,
        cols: std::ops::RangeInclusive<u16>,
    ) -> Result<CalcValue, CalcError> {
        let nrows = rows.end() - rows.start() + 1;
        let ncols = cols.end() - cols.start() + 1;
        if nrows as usize * ncols as usize > MAX_MATERIALIZED_CELLS {
            return Err(CalcError::Value);
        }
        let mut data = Vec::with_capacity(nrows as usize * ncols as usize);
        for row in rows {
            for col in cols.clone() {
                data.push(
                    self.cell(sheet, row, u32::from(col))
                        .unwrap_or(CalcValue::Blank),
                );
            }
        }
        Ok(CalcValue::array(ArrayValue::new(
            nrows,
            u32::from(ncols),
            data,
        )))
    }
}

/// Everything a function knows about its invocation: the workbook's date
/// system, the formula cell's location (0-based), and the value resolver.
/// Built by the eval loop per call; cheap to copy.
#[derive(Clone, Copy)]
pub struct FuncCtx<'a> {
    /// 1904 date system flag (affects date-serial arithmetic only).
    pub date1904: bool,
    /// Sheet index of the formula cell.
    pub sheet: u32,
    /// 0-based row of the formula cell.
    pub row: u32,
    /// 0-based column of the formula cell.
    pub col: u32,
    /// Read handle into the workbook grid.
    pub resolver: &'a dyn CellResolver,
}

impl<'a> FuncCtx<'a> {
    /// Resolve a reference expression against the current sheet.
    pub fn resolve(&self, re: &RefExpr) -> Result<CalcValue, CalcError> {
        self.resolver.resolve_ref(self.sheet, re)
    }

    /// Read one cell on the current sheet; `None` means blank.
    pub fn cell(&self, row: u32, col: u32) -> Option<CalcValue> {
        self.resolver.cell(self.sheet, row, col)
    }
}

/// The function pointer contract. Higher-ranked so no lifetime appears in
/// `FuncSpec` or the registry; function authors write `fn f(ctx: &FuncCtx,
/// args: &[FuncArg])` with elided lifetimes.
pub type Func = for<'a> fn(&FuncCtx<'a>, &[FuncArg]) -> Result<CalcValue, CalcError>;

/// Registration metadata for one worksheet function.
#[derive(Clone, Copy, Debug)]
pub struct FuncSpec {
    /// Canonical name, e.g. `"ABS"`. Lookup is case-insensitive.
    pub name: &'static str,
    /// Minimum accepted argument count.
    pub min_args: usize,
    /// Maximum accepted argument count; `None` = variadic.
    pub max_args: Option<usize>,
    /// Volatile functions (`RAND`, `NOW`, ...) must never be cached.
    pub volatile: bool,
    /// Array-aware functions (`SUM`) receive arrays as-is; others must be
    /// handed scalarized arguments by the eval loop.
    pub array_aware: bool,
    /// The implementation.
    pub func: Func,
}

/// Equality is by **canonical name**, which is the registry's actual key.
///
/// This is written by hand rather than derived because deriving it would
/// compare `func`, and comparing function pointers does not produce a
/// meaningful answer: the compiler may merge two identical functions to one
/// address or duplicate one across codegen units, so `==` can report either
/// answer for reasons that have nothing to do with the spec. A derived
/// `PartialEq` here was a trap waiting for the first caller to compare two
/// specs — nobody had yet, which is why it went unnoticed.
impl PartialEq for FuncSpec {
    fn eq(&self, other: &Self) -> bool {
        self.name.eq_ignore_ascii_case(other.name)
            && self.min_args == other.min_args
            && self.max_args == other.max_args
            && self.volatile == other.volatile
            && self.array_aware == other.array_aware
    }
}

impl FuncSpec {
    /// Arity check: `Err(CalcError::Value)` when `n` is outside
    /// `min_args..=max_args`.
    pub fn validate(&self, n: usize) -> Result<(), CalcError> {
        if n < self.min_args {
            return Err(CalcError::Value);
        }
        if let Some(max) = self.max_args {
            if n > max {
                return Err(CalcError::Value);
            }
        }
        Ok(())
    }
}

/// Case-insensitive function-name registry. Keys are the lowercase canonical
/// names (Excel function names are ASCII).
pub struct Registry {
    map: HashMap<String, &'static FuncSpec>,
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl Registry {
    pub fn new() -> Self {
        Registry {
            map: HashMap::new(),
        }
    }

    /// Register one spec, keyed by its lowercase name. A duplicate name is a
    /// programming error — caught by `debug_assert` at first use.
    pub fn register(&mut self, spec: &'static FuncSpec) {
        let key = spec.name.to_ascii_lowercase();
        debug_assert!(
            !self.map.contains_key(&key),
            "duplicate function registration: {}",
            spec.name
        );
        self.map.insert(key, spec);
    }

    /// Look up a function by name (case-insensitive). `None` for unregistered
    /// names — the caller MUST route the cell to the fallback path.
    pub fn get(&self, name: &str) -> Option<&'static FuncSpec> {
        self.map.get(&name.to_ascii_lowercase()).copied()
    }
}

/// Build the registry once. Family agents never edit this function: each
/// family's `register` hook is already wired here in fixed order.
fn build() -> Registry {
    let mut r = Registry::new();
    math::register(&mut r);
    statistical::register(&mut r);
    logical::register(&mut r);
    text::register(&mut r);
    datetime::register(&mut r);
    information::register(&mut r);
    lookup::register(&mut r);
    financial::register(&mut r);
    engineering::register(&mut r);
    dynamic::register(&mut r);
    database::register(&mut r);
    r
}

static REGISTRY: OnceLock<Registry> = OnceLock::new();

/// The process-wide function registry, built on first use.
pub fn registry() -> &'static Registry {
    REGISTRY.get_or_init(build)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turbo::calc::ast::{CellRef, RangeRef};

    fn identity_value(_ctx: &FuncCtx, args: &[FuncArg]) -> Result<CalcValue, CalcError> {
        args[0].value(_ctx)
    }

    const TEST_ABS: FuncSpec = FuncSpec {
        name: "ABS",
        min_args: 1,
        max_args: Some(1),
        volatile: false,
        array_aware: false,
        func: identity_value,
    };

    const TEST_SUM: FuncSpec = FuncSpec {
        name: "SUM",
        min_args: 0,
        max_args: None,
        volatile: false,
        array_aware: true,
        func: identity_value,
    };

    fn test_registry() -> Registry {
        let mut r = Registry::new();
        r.register(&TEST_ABS);
        r.register(&TEST_SUM);
        r
    }

    #[test]
    fn lookup_is_case_insensitive() {
        let r = test_registry();
        for name in ["ABS", "abs", "Abs", "aBs"] {
            let spec = r.get(name).expect("registered name must resolve");
            assert_eq!(spec.name, "ABS");
        }
        assert_eq!(r.get("SUM"), Some(&TEST_SUM));
    }

    #[test]
    fn unknown_name_returns_none() {
        let r = test_registry();
        assert_eq!(r.get("NXKFN12345"), None);
        assert_eq!(r.get(""), None);
        // The global registry resolves nothing until a family lands; a miss
        // must route to the fallback path, never a guessed value.
        assert_eq!(registry().get("NXKFN12345"), None);
    }

    #[test]
    fn arity_validation() {
        assert!(TEST_ABS.validate(1).is_ok());
        assert_eq!(TEST_ABS.validate(0), Err(CalcError::Value));
        assert_eq!(TEST_ABS.validate(2), Err(CalcError::Value));
        assert!(TEST_SUM.validate(0).is_ok());
        assert!(TEST_SUM.validate(255).is_ok());
    }

    #[test]
    fn volatility_and_array_awareness_flags() {
        assert!(!TEST_ABS.volatile);
        assert!(!TEST_ABS.array_aware);
        assert!(TEST_SUM.array_aware);
    }

    struct StubResolver;
    impl CellResolver for StubResolver {
        fn cell(&self, _sheet: u32, row: u32, col: u32) -> Option<CalcValue> {
            if row == 1 && col == 1 {
                Some(CalcValue::Number(42.0))
            } else {
                None
            }
        }
        fn sheet_index(&self, name: &str) -> Option<u32> {
            if name == "Data" { Some(0) } else { None }
        }
    }

    fn ctx<'a>(resolver: &'a dyn CellResolver) -> FuncCtx<'a> {
        FuncCtx {
            date1904: false,
            sheet: 0,
            row: 0,
            col: 0,
            resolver,
        }
    }

    fn a1() -> RefExpr {
        RefExpr::Local(RefCore::Cell(CellRef {
            col: 1,
            row: 1,
            abs_col: false,
            abs_row: false,
        }))
    }

    #[test]
    fn reference_args_resolve_through_ctx() {
        let resolver = StubResolver;
        let c = ctx(&resolver);
        assert_eq!(
            FuncArg::Reference(a1()).value(&c),
            Ok(CalcValue::Number(42.0))
        );
        assert_eq!(
            FuncArg::Value(CalcValue::Bool(true)).value(&c),
            Ok(CalcValue::Bool(true))
        );
    }

    #[test]
    fn range_args_materialize_to_dense_arrays() {
        let resolver = StubResolver;
        let c = ctx(&resolver);
        let rng = FuncArg::Reference(RefExpr::Local(RefCore::Range(RangeRef {
            start: CellRef {
                col: 1,
                row: 1,
                abs_col: false,
                abs_row: false,
            },
            end: CellRef {
                col: 2,
                row: 2,
                abs_col: false,
                abs_row: false,
            },
        })));
        match rng.value(&c).unwrap() {
            CalcValue::Array(a) => {
                assert_eq!(a.shape(), (2, 2));
                assert_eq!(a.get(0, 0), &CalcValue::Number(42.0));
                assert_eq!(a.get(0, 1), &CalcValue::Blank);
                assert_eq!(a.get(1, 1), &CalcValue::Blank);
            }
            other => panic!("expected a dense array, got {other:?}"),
        }
    }

    #[test]
    fn reference_errors_surface_honestly() {
        let resolver = StubResolver;
        let c = ctx(&resolver);
        let bad_sheet = RefExpr::Sheet {
            name: "Nope".into(),
            inner: Box::new(RefCore::Cell(CellRef {
                col: 0,
                row: 0,
                abs_col: false,
                abs_row: false,
            })),
        };
        assert_eq!(FuncArg::Reference(bad_sheet).value(&c), Err(CalcError::Ref));
        assert_eq!(
            FuncArg::Reference(RefExpr::Name {
                name: "Unresolvable".into(),
                sheet: None,
            })
            .value(&c),
            Err(CalcError::Name)
        );
    }

    #[test]
    fn as_reference_returns_coordinates_only() {
        assert!(FuncArg::Reference(a1()).as_reference().is_some());
        assert_eq!(FuncArg::Value(CalcValue::Blank).as_reference(), None);
    }

    /// Plan §5 Tier 1 verbatim. This is the gate for DoD item 4: a function
    /// present in a family file but never handed to `Registry::register` would
    /// silently route its cells to the fallback path, so presence in source is
    /// not evidence — only a live lookup is.
    const TIER_1: [&str; 71] = [
        "SUM",
        "AVERAGE",
        "COUNT",
        "COUNTA",
        "MIN",
        "MAX",
        "ROUND",
        "ROUNDUP",
        "ROUNDDOWN",
        "ABS",
        "INT",
        "MOD",
        "POWER",
        "SQRT",
        "PRODUCT",
        "IF",
        "IFS",
        "AND",
        "OR",
        "NOT",
        "IFERROR",
        "IFNA",
        "SUMIF",
        "SUMIFS",
        "COUNTIF",
        "COUNTIFS",
        "AVERAGEIF",
        "AVERAGEIFS",
        "VLOOKUP",
        "HLOOKUP",
        "XLOOKUP",
        "INDEX",
        "MATCH",
        "LOOKUP",
        "CONCAT",
        "CONCATENATE",
        "TEXTJOIN",
        "LEFT",
        "RIGHT",
        "MID",
        "LEN",
        "TRIM",
        "UPPER",
        "LOWER",
        "FIND",
        "SEARCH",
        "SUBSTITUTE",
        "REPLACE",
        "VALUE",
        "TEXT",
        "DATE",
        "TODAY",
        "NOW",
        "YEAR",
        "MONTH",
        "DAY",
        "EDATE",
        "EOMONTH",
        "DATEDIF",
        "ISBLANK",
        "ISNUMBER",
        "ISTEXT",
        "ISERROR",
        "ISERR",
        "ISNA",
        "ISLOGICAL",
        "NA",
        "ROW",
        "COLUMN",
        "ROWS",
        "COLUMNS",
    ];

    #[test]
    fn every_tier_1_function_is_registered() {
        let r = registry();
        let missing: Vec<&str> = TIER_1
            .iter()
            .copied()
            .filter(|n| r.get(n).is_none())
            .collect();
        assert!(
            missing.is_empty(),
            "unregistered Tier-1 functions: {missing:?}"
        );
        // lookup is case-insensitive, as Excel's is
        assert!(r.get("sUmIfS").is_some());
        // and an unknown name must stay unknown so the cell falls back
        assert!(r.get("NOSUCHFUNCTION").is_none());
    }

    /// Cross-check of the date family against Excel ground truth stated here,
    /// independently of the assertions the family's own author wrote, and going
    /// through the public registry rather than private helpers. Serial-date
    /// arithmetic is the easiest place in the engine to be confidently wrong.
    #[test]
    fn date_serials_match_excel_including_the_1900_leap_bug() {
        struct NoCells;
        impl CellResolver for NoCells {
            fn cell(&self, _s: u32, _r: u32, _c: u32) -> Option<CalcValue> {
                None
            }
            fn sheet_index(&self, _n: &str) -> Option<u32> {
                None
            }
        }
        let resolver = NoCells;
        let ctx = FuncCtx {
            date1904: false,
            sheet: 0,
            row: 0,
            col: 0,
            resolver: &resolver,
        };
        let call = |name: &str, nums: &[f64]| -> Result<CalcValue, CalcError> {
            let spec = registry().get(name).unwrap_or_else(|| panic!("{name}"));
            let args: Vec<FuncArg> = nums
                .iter()
                .map(|n| FuncArg::Value(CalcValue::Number(*n)))
                .collect();
            (spec.func)(&ctx, &args)
        };
        let num = |name: &str, nums: &[f64]| -> f64 {
            call(name, nums)
                .unwrap_or_else(|e| panic!("{name} -> {e:?}"))
                .as_number()
                .unwrap_or_else(|| panic!("{name} was not a number"))
        };

        // Serials Excel itself produces.
        assert_eq!(num("DATE", &[1900.0, 1.0, 1.0]), 1.0);
        assert_eq!(num("DATE", &[1900.0, 2.0, 28.0]), 59.0);
        // 61, not 60: the phantom 1900-02-29 occupies serial 60.
        assert_eq!(num("DATE", &[1900.0, 3.0, 1.0]), 61.0);
        assert_eq!(num("DATE", &[2024.0, 3.0, 1.0]), 45352.0);
        assert_eq!(num("DATE", &[9999.0, 12.0, 31.0]), 2_958_465.0);

        // ...and read back.
        assert_eq!(num("YEAR", &[45352.0]), 2024.0);
        assert_eq!(num("MONTH", &[45352.0]), 3.0);
        assert_eq!(num("DAY", &[45352.0]), 1.0);
        assert_eq!(num("DAY", &[60.0]), 29.0, "serial 60 is the phantom Feb 29");
        assert_eq!(num("MONTH", &[60.0]), 2.0);
        assert_eq!(num("YEAR", &[60.0]), 1900.0);

        // Serial 1 is a Sunday in Excel's 1900 system; 2024-03-01 was a Friday.
        assert_eq!(num("WEEKDAY", &[1.0]), 1.0);
        assert_eq!(num("WEEKDAY", &[45352.0]), 6.0);

        assert_eq!(num("DAYS", &[45352.0, 45351.0]), 1.0);
        // Jan 31 + 1 month clamps to Feb 29 in a leap year.
        let jan31 = num("DATE", &[2024.0, 1.0, 31.0]);
        let feb29 = num("DATE", &[2024.0, 2.0, 29.0]);
        assert_eq!(num("EDATE", &[jan31, 1.0]), feb29);
        assert_eq!(num("EOMONTH", &[jan31, 1.0]), feb29);
    }

    /// Excel's `AND`/`OR`/`XOR` do **not** short-circuit: every argument is
    /// evaluated and an error in any of them propagates, so `=OR(TRUE,NA())`
    /// is `#N/A`, not `TRUE`. Pinned here because it looks like a bug from the
    /// outside — a reviewer who "fixes" it into short-circuiting would be
    /// making the engine disagree with Excel.
    #[test]
    fn boolean_aggregates_do_not_short_circuit() {
        struct NoCells;
        impl CellResolver for NoCells {
            fn cell(&self, _s: u32, _r: u32, _c: u32) -> Option<CalcValue> {
                None
            }
            fn sheet_index(&self, _n: &str) -> Option<u32> {
                None
            }
        }
        let resolver = NoCells;
        let ctx = FuncCtx {
            date1904: false,
            sheet: 0,
            row: 0,
            col: 0,
            resolver: &resolver,
        };
        for name in ["OR", "AND", "XOR"] {
            let spec = registry().get(name).expect(name);
            let args = vec![
                FuncArg::Value(CalcValue::Bool(true)),
                FuncArg::Value(CalcValue::Error(CalcError::Div0)),
            ];
            assert_eq!(
                (spec.func)(&ctx, &args),
                Err(CalcError::Div0),
                "{name} must propagate an error argument, not short-circuit past it"
            );
        }
    }

    #[test]
    fn volatile_functions_are_flagged() {
        // A cached NOW/TODAY would be a stale number in the file.
        for name in ["NOW", "TODAY"] {
            let spec = registry().get(name).expect(name);
            assert!(spec.volatile, "{name} must be volatile");
        }
    }
}

#[cfg(test)]
mod coordinator_coverage {
    use super::*;

    /// Reports how many worksheet functions are actually REGISTERED, as opposed
    /// to written. Counting `register` call sites with grep gives the wrong
    /// answer because the families use three different registration styles
    /// (direct `r.register(&CONST)`, a `reg(r, spec(..))` helper, and a loop
    /// over a `SPECS` array). The registry itself is the only authority.
    #[test]
    fn coordinator_report_registry_size() {
        let r = registry();
        let mut names: Vec<&str> = r.map.keys().map(|s| s.as_str()).collect();
        names.sort_unstable();
        println!("REGISTERED FUNCTIONS: {}", names.len());
        println!("{}", names.join(" "));
    }
}
