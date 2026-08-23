//! PyO3 bindings for the formula calc engine — the Rust half of the
//! `kyrax.formulas` facade.
//!
//! Thin by design (CLAUDE.md law): every argument is converted here and every
//! result is converted here, and nothing else. The parser, the evaluator, the
//! function registry and the reference walker all live in `calc/` and stay
//! language-agnostic — this file only adapts Python to those entry points.
//!
//! Surface (all registered in `src/lib.rs`):
//!   * `evaluate(formula, context=None)` — value of a formula string. The
//!     optional context maps A1 cell references to values used as the cell
//!     grid. The GIL is released for the compute itself.
//!   * `list_functions()` — `(name, category)` for every registered function.
//!   * `dependencies(formula)` — the cell references the formula reads.
//!   * `recalculate(sheets)` — builds a workbook from the write-path sheet
//!     dicts and returns the recalculated workbook bytes (identical to passing
//!     `recalculate=True` to `write_excel_turbo_bytes`).

use pyo3::IntoPyObject;
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};

use crate::turbo::calc::ast::{Expr, RefCore, RefExpr};
use crate::turbo::calc::eval::eval;
use crate::turbo::calc::functions::{CellResolver, FuncCtx, registry};
use crate::turbo::calc::parser::parse_formula;
use crate::turbo::calc::value::{CalcError, CalcValue};
use std::collections::HashMap;

/// The cell grid `evaluate` reads through: sheet 0 only, provided by the
/// optional `context` dict. Owns its data so the GIL can be released while the
/// evaluator reads it.
struct ContextGrid {
    cells: HashMap<(u32, u16), CalcValue>,
}

impl ContextGrid {
    fn from_py_dict(context: Option<&Bound<'_, PyDict>>) -> PyResult<Self> {
        let mut cells = HashMap::new();
        if let Some(d) = context {
            for (k, v) in d.iter() {
                let key: String = k.extract().map_err(|_| {
                    PyTypeError::new_err("context keys must be A1 cell references like 'A1'")
                })?;
                let Some((r1, c1, r2, c2)) =
                    crate::turbo::scan::parse_ref_range_strict(key.as_bytes())
                else {
                    return Err(PyValueError::new_err(format!(
                        "context key {key:?} is not a valid A1 cell reference"
                    )));
                };
                if r1 != r2 || c1 != c2 {
                    return Err(PyValueError::new_err(format!(
                        "context key {key:?} must be a single cell, not a range"
                    )));
                }
                let row = r1 - 1;
                let col = u16::try_from(c1 - 1).map_err(|_| {
                    PyValueError::new_err(format!("context key {key:?} out of grid"))
                })?;
                cells.insert((row, col), py_to_calc_value(&v)?);
            }
        }
        Ok(Self { cells })
    }
}

impl CellResolver for ContextGrid {
    fn cell(&self, sheet: u32, row: u32, col: u32) -> Option<CalcValue> {
        if sheet != 0 {
            return None;
        }
        let col = u16::try_from(col).ok()?;
        self.cells.get(&(row, col)).cloned()
    }

    fn sheet_index(&self, name: &str) -> Option<u32> {
        // The context is one virtual sheet named like Excel's default. Any
        // other name is a real unknown sheet: SHEET("nope") must be #N/A,
        // matching the engine resolver contract instead of guessing sheet 0.
        if name.eq_ignore_ascii_case("Sheet1") {
            Some(0)
        } else {
            None
        }
    }
}

/// Convert one Python scalar into a `CalcValue` (numbers, strings, bools,
/// error-code strings, `None`). Mirrors the write path's `py_to_cell_value`
/// for the scalar cases the formula API accepts.
fn py_to_calc_value(obj: &Bound<'_, PyAny>) -> PyResult<CalcValue> {
    if obj.is_none() {
        return Ok(CalcValue::Blank);
    }
    if obj.is_instance_of::<pyo3::types::PyBool>() {
        return Ok(CalcValue::Bool(obj.extract()?));
    }
    if let Ok(i) = obj.extract::<i64>() {
        return Ok(CalcValue::Number(i as f64));
    }
    if let Ok(f) = obj.extract::<f64>() {
        return Ok(CalcValue::Number(f));
    }
    if let Ok(s) = obj.extract::<String>() {
        if let Some(e) = CalcError::from_str_ci(&s) {
            return Ok(CalcValue::Error(e));
        }
        return Ok(CalcValue::text(s));
    }
    Err(PyTypeError::new_err(format!(
        "context values must be a number, string, bool, error code or None; got {}",
        obj.get_type().name()?
    )))
}

/// Convert an eval result to the Python contract: floats, strings, bools,
/// error-code strings, `None` for blank, nested lists for arrays.
fn calc_value_to_py(
    py: Python<'_>,
    result: Result<CalcValue, CalcError>,
) -> PyResult<Bound<'_, PyAny>> {
    let value = match result {
        Ok(v) => v,
        Err(e) => return Ok(e.code().into_pyobject(py)?.into_any()),
    };
    match value {
        CalcValue::Number(n) => Ok(n.into_pyobject(py)?.into_any()),
        CalcValue::Text(s) => Ok(s.as_ref().into_pyobject(py)?.into_any()),
        // Bools are CPython singletons, so `PyBool::new` hands back a borrowed
        // `Bound`; `to_owned` clones the reference before moving it.
        CalcValue::Bool(b) => Ok(pyo3::types::PyBool::new(py, b).to_owned().into_any()),
        CalcValue::Blank => Ok(py.None().into_bound(py)),
        CalcValue::Error(e) => Ok(e.code().into_pyobject(py)?.into_any()),
        CalcValue::Array(a) => {
            let mut rows: Vec<Vec<Bound<'_, PyAny>>> = Vec::with_capacity(a.rows as usize);
            for r in 0..a.rows {
                let mut row: Vec<Bound<'_, PyAny>> = Vec::with_capacity(a.cols as usize);
                for c in 0..a.cols {
                    row.push(calc_value_to_py(py, Ok(a.get(r, c).clone()))?);
                }
                rows.push(row);
            }
            Ok(rows.into_pyobject(py)?.into_any())
        }
    }
}

/// `formulas.evaluate(formula, context=None)`.
#[pyfunction(name = "evaluate")]
#[pyo3(signature = (formula, context = None))]
pub fn py_evaluate<'py>(
    py: Python<'py>,
    formula: &str,
    context: Option<&Bound<'py, PyDict>>,
) -> PyResult<Bound<'py, PyAny>> {
    let expr = parse_formula(formula).map_err(|e| {
        PyValueError::new_err(format!("could not parse formula {formula:?}: {e:?}"))
    })?;
    let grid = ContextGrid::from_py_dict(context)?;
    // The compute is pure Rust; release the GIL for large formulas.
    let result = py.detach(|| {
        let ctx = FuncCtx {
            date1904: false,
            sheet: 0,
            row: 0,
            col: 0,
            resolver: &grid,
        };
        eval(&expr, &ctx)
    });
    calc_value_to_py(py, result)
}

/// One row of the static name -> category inventory. The registry itself is
/// the authority on what is registered; `list_functions` filters this table by
/// a live `registry().get(name)` so a name removed from a family file simply
/// stops being reported and nothing stale is ever returned.
const FUNCTIONS_BY_CATEGORY: &[(&str, &str)] = &[
    // -- Math & Trig -------------------------------------------------------
    ("ABS", "Math & Trig"),
    ("ACOS", "Math & Trig"),
    ("ACOSH", "Math & Trig"),
    ("ACOT", "Math & Trig"),
    ("ACOTH", "Math & Trig"),
    ("AGGREGATE", "Math & Trig"),
    ("ARABIC", "Math & Trig"),
    ("ASIN", "Math & Trig"),
    ("ASINH", "Math & Trig"),
    ("ATAN", "Math & Trig"),
    ("ATAN2", "Math & Trig"),
    ("ATANH", "Math & Trig"),
    ("BASE", "Math & Trig"),
    ("CEILING", "Math & Trig"),
    ("CEILING.MATH", "Math & Trig"),
    ("CEILING.PRECISE", "Math & Trig"),
    ("COMBIN", "Math & Trig"),
    ("COMBINA", "Math & Trig"),
    ("COS", "Math & Trig"),
    ("COSH", "Math & Trig"),
    ("COT", "Math & Trig"),
    ("COTH", "Math & Trig"),
    ("CSC", "Math & Trig"),
    ("CSCH", "Math & Trig"),
    ("DECIMAL", "Math & Trig"),
    ("DEGREES", "Math & Trig"),
    ("EVEN", "Math & Trig"),
    ("EXP", "Math & Trig"),
    ("FACT", "Math & Trig"),
    ("FACTDOUBLE", "Math & Trig"),
    ("FLOOR", "Math & Trig"),
    ("FLOOR.MATH", "Math & Trig"),
    ("FLOOR.PRECISE", "Math & Trig"),
    ("GCD", "Math & Trig"),
    ("INT", "Math & Trig"),
    ("LCM", "Math & Trig"),
    ("LN", "Math & Trig"),
    ("LOG", "Math & Trig"),
    ("LOG10", "Math & Trig"),
    ("MDETERM", "Math & Trig"),
    ("MINVERSE", "Math & Trig"),
    ("MMULT", "Math & Trig"),
    ("MOD", "Math & Trig"),
    ("MROUND", "Math & Trig"),
    ("MULTINOMIAL", "Math & Trig"),
    ("MUNIT", "Math & Trig"),
    ("ODD", "Math & Trig"),
    ("PI", "Math & Trig"),
    ("POWER", "Math & Trig"),
    ("PRODUCT", "Math & Trig"),
    ("QUOTIENT", "Math & Trig"),
    ("RADIANS", "Math & Trig"),
    ("RAND", "Math & Trig"),
    ("RANDARRAY", "Math & Trig"),
    ("RANDBETWEEN", "Math & Trig"),
    ("ROMAN", "Math & Trig"),
    ("ROUND", "Math & Trig"),
    ("ROUNDDOWN", "Math & Trig"),
    ("ROUNDUP", "Math & Trig"),
    ("SEC", "Math & Trig"),
    ("SECH", "Math & Trig"),
    ("SEQUENCE", "Math & Trig"),
    ("SERIESSUM", "Math & Trig"),
    ("SIGN", "Math & Trig"),
    ("SIN", "Math & Trig"),
    ("SINH", "Math & Trig"),
    ("SQRT", "Math & Trig"),
    ("SUBTOTAL", "Math & Trig"),
    ("SUM", "Math & Trig"),
    ("SUMIF", "Math & Trig"),
    ("SUMIFS", "Math & Trig"),
    ("SUMPRODUCT", "Math & Trig"),
    ("SUMSQ", "Math & Trig"),
    ("SUMX2MY2", "Math & Trig"),
    ("SUMX2PY2", "Math & Trig"),
    ("SUMXMY2", "Math & Trig"),
    ("SQRTPI", "Math & Trig"),
    ("TAN", "Math & Trig"),
    ("TANH", "Math & Trig"),
    ("TRUNC", "Math & Trig"),
    // -- Statistical -------------------------------------------------------
    ("AVEDEV", "Statistical"),
    ("AVERAGE", "Statistical"),
    ("AVERAGEA", "Statistical"),
    ("AVERAGEIF", "Statistical"),
    ("AVERAGEIFS", "Statistical"),
    ("BETA.DIST", "Statistical"),
    ("BETA.INV", "Statistical"),
    ("BINOM.DIST", "Statistical"),
    ("BINOM.DIST.RANGE", "Statistical"),
    ("BINOM.INV", "Statistical"),
    ("CHISQ.DIST", "Statistical"),
    ("CHISQ.DIST.RT", "Statistical"),
    ("CHISQ.INV", "Statistical"),
    ("CHISQ.INV.RT", "Statistical"),
    ("CHISQ.TEST", "Statistical"),
    ("CONFIDENCE.NORM", "Statistical"),
    ("CONFIDENCE.T", "Statistical"),
    ("CORREL", "Statistical"),
    ("COUNT", "Statistical"),
    ("COUNTA", "Statistical"),
    ("COUNTBLANK", "Statistical"),
    ("COUNTIF", "Statistical"),
    ("COUNTIFS", "Statistical"),
    ("COVARIANCE.P", "Statistical"),
    ("COVARIANCE.S", "Statistical"),
    ("DEVSQ", "Statistical"),
    ("EXPON.DIST", "Statistical"),
    ("F.DIST", "Statistical"),
    ("F.DIST.RT", "Statistical"),
    ("F.INV", "Statistical"),
    ("F.INV.RT", "Statistical"),
    ("F.TEST", "Statistical"),
    ("FISHER", "Statistical"),
    ("FISHERINV", "Statistical"),
    ("FORECAST.LINEAR", "Statistical"),
    ("FREQUENCY", "Statistical"),
    ("GAMMA", "Statistical"),
    ("GAMMA.DIST", "Statistical"),
    ("GAMMA.INV", "Statistical"),
    ("GAMMALN", "Statistical"),
    ("GAUSS", "Statistical"),
    ("GEOMEAN", "Statistical"),
    ("GROWTH", "Statistical"),
    ("HARMEAN", "Statistical"),
    ("HYPGEOM.DIST", "Statistical"),
    ("INTERCEPT", "Statistical"),
    ("KURT", "Statistical"),
    ("LARGE", "Statistical"),
    ("LINEST", "Statistical"),
    ("LOGEST", "Statistical"),
    ("LOGNORM.DIST", "Statistical"),
    ("LOGNORM.INV", "Statistical"),
    ("MAX", "Statistical"),
    ("MAXA", "Statistical"),
    ("MAXIFS", "Statistical"),
    ("MEDIAN", "Statistical"),
    ("MIN", "Statistical"),
    ("MINA", "Statistical"),
    ("MINIFS", "Statistical"),
    ("MODE.MULT", "Statistical"),
    ("MODE.SNGL", "Statistical"),
    ("NEGBINOM.DIST", "Statistical"),
    ("NORM.DIST", "Statistical"),
    ("NORM.INV", "Statistical"),
    ("NORM.S.DIST", "Statistical"),
    ("NORM.S.INV", "Statistical"),
    ("PEARSON", "Statistical"),
    ("PERCENTILE.EXC", "Statistical"),
    ("PERCENTILE.INC", "Statistical"),
    ("PERCENTRANK.EXC", "Statistical"),
    ("PERCENTRANK.INC", "Statistical"),
    ("PERMUT", "Statistical"),
    ("PERMUTATIONA", "Statistical"),
    ("PHI", "Statistical"),
    ("POISSON.DIST", "Statistical"),
    ("PROB", "Statistical"),
    ("QUARTILE.EXC", "Statistical"),
    ("QUARTILE.INC", "Statistical"),
    ("RANK.AVG", "Statistical"),
    ("RANK.EQ", "Statistical"),
    ("RSQ", "Statistical"),
    ("SKEW", "Statistical"),
    ("SKEW.P", "Statistical"),
    ("SLOPE", "Statistical"),
    ("SMALL", "Statistical"),
    ("STANDARDIZE", "Statistical"),
    ("STDEV.P", "Statistical"),
    ("STDEV.S", "Statistical"),
    ("STDEVA", "Statistical"),
    ("STDEVPA", "Statistical"),
    ("STEYX", "Statistical"),
    ("T.DIST", "Statistical"),
    ("T.DIST.2T", "Statistical"),
    ("T.DIST.RT", "Statistical"),
    ("T.INV", "Statistical"),
    ("T.INV.2T", "Statistical"),
    ("T.TEST", "Statistical"),
    ("TREND", "Statistical"),
    ("TRIMMEAN", "Statistical"),
    ("VAR.P", "Statistical"),
    ("VAR.S", "Statistical"),
    ("VARA", "Statistical"),
    ("VARPA", "Statistical"),
    ("WEIBULL.DIST", "Statistical"),
    ("Z.TEST", "Statistical"),
    // Legacy compatibility names remain part of Excel's standard surface.
    ("BETADIST", "Statistical"),
    ("BETAINV", "Statistical"),
    ("BINOMDIST", "Statistical"),
    ("CHIDIST", "Statistical"),
    ("CHIINV", "Statistical"),
    ("CHITEST", "Statistical"),
    ("CONFIDENCE", "Statistical"),
    ("COVAR", "Statistical"),
    ("CRITBINOM", "Statistical"),
    ("EXPONDIST", "Statistical"),
    ("FDIST", "Statistical"),
    ("FINV", "Statistical"),
    ("FORECAST", "Statistical"),
    ("FTEST", "Statistical"),
    ("GAMMADIST", "Statistical"),
    ("GAMMAINV", "Statistical"),
    ("GAMMALN.PRECISE", "Statistical"),
    ("HYPGEOMDIST", "Statistical"),
    ("LOGINV", "Statistical"),
    ("LOGNORMDIST", "Statistical"),
    ("MODE", "Statistical"),
    ("NEGBINOMDIST", "Statistical"),
    ("NORMDIST", "Statistical"),
    ("NORMINV", "Statistical"),
    ("NORMSDIST", "Statistical"),
    ("NORMSINV", "Statistical"),
    ("PERCENTILE", "Statistical"),
    ("PERCENTRANK", "Statistical"),
    ("POISSON", "Statistical"),
    ("QUARTILE", "Statistical"),
    ("RANK", "Statistical"),
    ("STDEV", "Statistical"),
    ("STDEVP", "Statistical"),
    ("TDIST", "Statistical"),
    ("TINV", "Statistical"),
    ("TTEST", "Statistical"),
    ("VAR", "Statistical"),
    ("VARP", "Statistical"),
    ("WEIBULL", "Statistical"),
    ("ZTEST", "Statistical"),
    // -- Logical ------------------------------------------------------------
    ("AND", "Logical"),
    ("FALSE", "Logical"),
    ("IF", "Logical"),
    ("IFERROR", "Logical"),
    ("IFNA", "Logical"),
    ("IFS", "Logical"),
    ("NOT", "Logical"),
    ("OR", "Logical"),
    ("SWITCH", "Logical"),
    ("TRUE", "Logical"),
    ("XOR", "Logical"),
    ("LAMBDA", "Logical"),
    ("LET", "Logical"),
    // -- Text ---------------------------------------------------------------
    ("ARRAYTOTEXT", "Text"),
    ("ASC", "Text"),
    ("BAHTTEXT", "Text"),
    ("CHAR", "Text"),
    ("CLEAN", "Text"),
    ("CODE", "Text"),
    ("CONCAT", "Text"),
    ("CONCATENATE", "Text"),
    ("DOLLAR", "Text"),
    ("DBCS", "Text"),
    ("EXACT", "Text"),
    ("FIND", "Text"),
    ("FINDB", "Text"),
    ("FIXED", "Text"),
    ("LEFT", "Text"),
    ("LEFTB", "Text"),
    ("LEN", "Text"),
    ("LENB", "Text"),
    ("LOWER", "Text"),
    ("MID", "Text"),
    ("MIDB", "Text"),
    ("NUMBERVALUE", "Text"),
    ("NUMBERSTRING", "Text"),
    ("PROPER", "Text"),
    ("REPLACE", "Text"),
    ("REPLACEB", "Text"),
    ("REPT", "Text"),
    ("RIGHT", "Text"),
    ("RIGHTB", "Text"),
    ("SEARCH", "Text"),
    ("SEARCHB", "Text"),
    ("SUBSTITUTE", "Text"),
    ("T", "Text"),
    ("TEXT", "Text"),
    ("TEXTAFTER", "Text"),
    ("TEXTBEFORE", "Text"),
    ("TEXTJOIN", "Text"),
    ("TEXTSPLIT", "Text"),
    ("TRIM", "Text"),
    ("UNICHAR", "Text"),
    ("UNICODE", "Text"),
    ("UPPER", "Text"),
    ("VALUE", "Text"),
    ("VALUETOTEXT", "Text"),
    // -- Date & Time --------------------------------------------------------
    ("DATE", "Date & Time"),
    ("DATEDIF", "Date & Time"),
    ("DATEVALUE", "Date & Time"),
    ("DAY", "Date & Time"),
    ("DAYS", "Date & Time"),
    ("DAYS360", "Date & Time"),
    ("EDATE", "Date & Time"),
    ("EOMONTH", "Date & Time"),
    ("HOUR", "Date & Time"),
    ("ISOWEEKNUM", "Date & Time"),
    ("MINUTE", "Date & Time"),
    ("MONTH", "Date & Time"),
    ("NETWORKDAYS", "Date & Time"),
    ("NETWORKDAYS.INTL", "Date & Time"),
    ("NOW", "Date & Time"),
    ("SECOND", "Date & Time"),
    ("TIME", "Date & Time"),
    ("TIMEVALUE", "Date & Time"),
    ("TODAY", "Date & Time"),
    ("WEEKDAY", "Date & Time"),
    ("WEEKNUM", "Date & Time"),
    ("WORKDAY", "Date & Time"),
    ("WORKDAY.INTL", "Date & Time"),
    ("YEAR", "Date & Time"),
    ("YEARFRAC", "Date & Time"),
    // -- Lookup & Reference -------------------------------------------------
    ("ADDRESS", "Lookup & Reference"),
    ("AREAS", "Lookup & Reference"),
    ("BYCOL", "Lookup & Reference"),
    ("BYROW", "Lookup & Reference"),
    ("CHOOSE", "Lookup & Reference"),
    ("CHOOSECOLS", "Lookup & Reference"),
    ("CHOOSEROWS", "Lookup & Reference"),
    ("COLUMN", "Lookup & Reference"),
    ("COLUMNS", "Lookup & Reference"),
    ("DROP", "Lookup & Reference"),
    ("EXPAND", "Lookup & Reference"),
    ("FILTER", "Lookup & Reference"),
    ("FORMULATEXT", "Lookup & Reference"),
    ("HLOOKUP", "Lookup & Reference"),
    ("HSTACK", "Lookup & Reference"),
    ("HYPERLINK", "Lookup & Reference"),
    ("IMAGE", "Lookup & Reference"),
    ("INDEX", "Lookup & Reference"),
    ("INDIRECT", "Lookup & Reference"),
    ("LOOKUP", "Lookup & Reference"),
    ("MAKEARRAY", "Lookup & Reference"),
    ("MAP", "Lookup & Reference"),
    ("MATCH", "Lookup & Reference"),
    ("OFFSET", "Lookup & Reference"),
    ("ROW", "Lookup & Reference"),
    ("ROWS", "Lookup & Reference"),
    ("REDUCE", "Lookup & Reference"),
    ("SCAN", "Lookup & Reference"),
    ("SORT", "Lookup & Reference"),
    ("SORTBY", "Lookup & Reference"),
    ("TAKE", "Lookup & Reference"),
    ("TOCOL", "Lookup & Reference"),
    ("TOROW", "Lookup & Reference"),
    ("TRANSPOSE", "Lookup & Reference"),
    ("UNIQUE", "Lookup & Reference"),
    ("VSTACK", "Lookup & Reference"),
    ("VLOOKUP", "Lookup & Reference"),
    ("WRAPCOLS", "Lookup & Reference"),
    ("WRAPROWS", "Lookup & Reference"),
    ("XLOOKUP", "Lookup & Reference"),
    ("XMATCH", "Lookup & Reference"),
    // -- Information ----------------------------------------------------------
    ("CELL", "Information"),
    ("ERROR.TYPE", "Information"),
    ("ISBLANK", "Information"),
    ("ISERR", "Information"),
    ("ISERROR", "Information"),
    ("ISFORMULA", "Information"),
    ("ISEVEN", "Information"),
    ("ISLOGICAL", "Information"),
    ("ISNA", "Information"),
    ("ISNONTEXT", "Information"),
    ("ISNUMBER", "Information"),
    ("ISODD", "Information"),
    ("ISREF", "Information"),
    ("ISTEXT", "Information"),
    ("N", "Information"),
    ("NA", "Information"),
    ("SHEET", "Information"),
    ("SHEETS", "Information"),
    ("TYPE", "Information"),
    // -- Financial ------------------------------------------------------------
    ("ACCRINT", "Financial"),
    ("ACCRINTM", "Financial"),
    ("AMORLINC", "Financial"),
    ("COUPDAYBS", "Financial"),
    ("COUPDAYS", "Financial"),
    ("COUPDAYSNC", "Financial"),
    ("COUPNCD", "Financial"),
    ("COUPNUM", "Financial"),
    ("COUPPCD", "Financial"),
    ("CUMIPMT", "Financial"),
    ("CUMPRINC", "Financial"),
    ("DB", "Financial"),
    ("DDB", "Financial"),
    ("DISC", "Financial"),
    ("DOLLARDE", "Financial"),
    ("DOLLARFR", "Financial"),
    ("DURATION", "Financial"),
    ("EFFECT", "Financial"),
    ("FV", "Financial"),
    ("FVSCHEDULE", "Financial"),
    ("INTRATE", "Financial"),
    ("IPMT", "Financial"),
    ("IRR", "Financial"),
    ("ISPMT", "Financial"),
    ("MDURATION", "Financial"),
    ("MIRR", "Financial"),
    ("NOMINAL", "Financial"),
    ("NPER", "Financial"),
    ("NPV", "Financial"),
    ("ODDFPRICE", "Financial"),
    ("ODDFYIELD", "Financial"),
    ("ODDLPRICE", "Financial"),
    ("ODDLYIELD", "Financial"),
    ("PDURATION", "Financial"),
    ("PMT", "Financial"),
    ("PPMT", "Financial"),
    ("PRICE", "Financial"),
    ("PRICEMAT", "Financial"),
    ("PRICEDISC", "Financial"),
    ("PV", "Financial"),
    ("RATE", "Financial"),
    ("RECEIVED", "Financial"),
    ("RRI", "Financial"),
    ("SLN", "Financial"),
    ("SYD", "Financial"),
    ("TBILLEQ", "Financial"),
    ("TBILLPRICE", "Financial"),
    ("TBILLYIELD", "Financial"),
    ("VDB", "Financial"),
    ("XIRR", "Financial"),
    ("XNPV", "Financial"),
    ("YIELD", "Financial"),
    ("YIELDDISC", "Financial"),
    ("YIELDMAT", "Financial"),
    // -- Engineering -----------------------------------------------------------
    ("BESSELI", "Engineering"),
    ("BESSELJ", "Engineering"),
    ("BESSELK", "Engineering"),
    ("BESSELY", "Engineering"),
    ("BIN2DEC", "Engineering"),
    ("BIN2HEX", "Engineering"),
    ("BIN2OCT", "Engineering"),
    ("BITAND", "Engineering"),
    ("BITLSHIFT", "Engineering"),
    ("BITOR", "Engineering"),
    ("BITRSHIFT", "Engineering"),
    ("BITXOR", "Engineering"),
    ("COMPLEX", "Engineering"),
    ("CONVERT", "Engineering"),
    ("DEC2BIN", "Engineering"),
    ("DEC2HEX", "Engineering"),
    ("DEC2OCT", "Engineering"),
    ("DELTA", "Engineering"),
    ("ERF", "Engineering"),
    ("ERF.PRECISE", "Engineering"),
    ("ERFC", "Engineering"),
    ("ERFC.PRECISE", "Engineering"),
    ("GESTEP", "Engineering"),
    ("HEX2BIN", "Engineering"),
    ("HEX2DEC", "Engineering"),
    ("HEX2OCT", "Engineering"),
    ("IMABS", "Engineering"),
    ("IMAGINARY", "Engineering"),
    ("IMARGUMENT", "Engineering"),
    ("IMCONJUGATE", "Engineering"),
    ("IMCOS", "Engineering"),
    ("IMCOSH", "Engineering"),
    ("IMCOT", "Engineering"),
    ("IMCSC", "Engineering"),
    ("IMCSCH", "Engineering"),
    ("IMDIV", "Engineering"),
    ("IMEXP", "Engineering"),
    ("IMLN", "Engineering"),
    ("IMLOG10", "Engineering"),
    ("IMLOG2", "Engineering"),
    ("IMPOWER", "Engineering"),
    ("IMPRODUCT", "Engineering"),
    ("IMREAL", "Engineering"),
    ("IMSEC", "Engineering"),
    ("IMSECH", "Engineering"),
    ("IMSIN", "Engineering"),
    ("IMSINH", "Engineering"),
    ("IMSQRT", "Engineering"),
    ("IMSUB", "Engineering"),
    ("IMSUM", "Engineering"),
    ("IMTAN", "Engineering"),
    ("OCT2BIN", "Engineering"),
    ("OCT2DEC", "Engineering"),
    ("OCT2HEX", "Engineering"),
    // -- Database ---------------------------------------------------------------
    ("DAVERAGE", "Database"),
    ("DCOUNT", "Database"),
    ("DCOUNTA", "Database"),
    ("DGET", "Database"),
    ("DMAX", "Database"),
    ("DMIN", "Database"),
    ("DPRODUCT", "Database"),
    ("DSTDEV", "Database"),
    ("DSTDEVP", "Database"),
    ("DSUM", "Database"),
    ("DVAR", "Database"),
    ("DVARP", "Database"),
    // -- Web --------------------------------------------------------------------
    ("ENCODEURL", "Web"),
];

/// `formulas.list_functions()`.
#[pyfunction(name = "list_functions")]
pub fn py_list_functions() -> PyResult<Vec<(String, String)>> {
    let mut out: Vec<(String, String)> = Vec::with_capacity(FUNCTIONS_BY_CATEGORY.len());
    for (name, category) in FUNCTIONS_BY_CATEGORY {
        // The registry is the authority: report only what is actually
        // registered, under the registry's own canonical name.
        if let Some(spec) = registry().get(name) {
            out.push((spec.name.to_string(), (*category).to_string()));
        }
    }
    Ok(out)
}

/// `formulas.dependencies(formula)` — every static cell reference the formula
/// reads, as A1 strings (deduplicated, sorted).
#[pyfunction(name = "dependencies")]
pub fn py_dependencies(py: Python<'_>, formula: &str) -> PyResult<Vec<String>> {
    let result = py.detach(|| {
        let expr = parse_formula(formula)
            .map_err(|e| format!("could not parse formula {formula:?}: {e:?}"))?;
        let mut refs: Vec<String> = Vec::new();
        collect_refs(&expr, &mut refs);
        refs.sort();
        refs.dedup();
        Ok::<Vec<String>, String>(refs)
    });
    result.map_err(PyValueError::new_err)
}

/// Walk the AST collecting every statically-known cell reference.
fn collect_refs(expr: &Expr, out: &mut Vec<String>) {
    match expr {
        Expr::Value(_) | Expr::Null | Expr::LambdaParam(_) => {}
        Expr::Ref(re) => collect_ref(re, out),
        Expr::Unary(_, inner) | Expr::Suffix(_, inner) => collect_refs(inner, out),
        Expr::Binary(_, l, r) => {
            collect_refs(l, out);
            collect_refs(r, out);
        }
        Expr::Colon(l, r) => {
            collect_refs(l, out);
            collect_refs(r, out);
        }
        Expr::Union(children) => {
            for c in children {
                collect_refs(c, out);
            }
        }
        Expr::Function { args, .. } => {
            for a in args {
                collect_refs(a, out);
            }
        }
        Expr::Lambda { body, .. } => collect_refs(body, out),
        Expr::Formula(children) => {
            for c in children {
                collect_refs(c, out);
            }
        }
    }
}

fn collect_ref(re: &RefExpr, out: &mut Vec<String>) {
    match re {
        RefExpr::Local(core) => push_core(core, None, out),
        RefExpr::Sheet { name, inner } => push_core(inner, Some(name), out),
        RefExpr::Sheet3D { from, to, inner } => {
            push_core(inner, Some(&format!("{from}:{to}")), out)
        }
        RefExpr::Name { name, sheet } => match sheet {
            Some(s) => out.push(format!("{s}!{name}")),
            None => out.push(name.clone()),
        },
        RefExpr::Table(t) => out.push(format!("#{}", t.name)),
        RefExpr::External { book, inner } => out.push(format!("[{book}]{inner}")),
    }
}

fn push_core(core: &RefCore, sheet: Option<&str>, out: &mut Vec<String>) {
    let qualified = |a1: String| match sheet {
        Some(s) => format!("{s}!{a1}"),
        None => a1,
    };
    match core {
        RefCore::Cell(c) => out.push(qualified(cell_a1(c.col, c.row))),
        RefCore::Range(r) => {
            let start = cell_a1(r.start.col, r.start.row);
            let end = cell_a1(r.end.col, r.end.row);
            if start == end {
                out.push(qualified(start));
            } else {
                out.push(qualified(format!("{start}:{end}")));
            }
        }
        RefCore::Row(r) => {
            // Whole-row refs render as "3:5" even for a single row ("3:3"),
            // so a column-range-style reference is never confused with a cell.
            let s = r.start + 1;
            let e = r.end + 1;
            out.push(qualified(format!("{s}:{e}")));
        }
        RefCore::Column(c) => {
            // Whole-column refs render as "B:D" even for a single column.
            let s = crate::turbo::formula::index_to_letters(u32::from(c.start) + 1);
            let e = crate::turbo::formula::index_to_letters(u32::from(c.end) + 1);
            out.push(qualified(format!("{s}:{e}")));
        }
    }
}

/// 0-based `(col, row)` -> `"A1"` style reference.
fn cell_a1(col: u16, row: u32) -> String {
    format!(
        "{}{}",
        crate::turbo::formula::index_to_letters(u32::from(col) + 1),
        row + 1
    )
}

/// `formulas.recalculate(sheets)` — returns the recalculated workbook bytes.
///
/// Same sheet-dict schema as `kyrax.write_excel_turbo`. Delegates to the write
/// path's own `recalculate=True` hook, so the semantics are exactly those of
/// `write_excel_turbo_bytes(..., recalculate=True)`: every formula is computed
/// in Rust and written as its cached value; anything kyrax cannot compute
/// exactly is left uncomputed with `fullCalcOnLoad="1"`.
#[pyfunction(name = "recalculate")]
pub fn py_recalculate<'py>(
    py: Python<'py>,
    sheets: &Bound<'py, pyo3::types::PyAny>,
) -> PyResult<Bound<'py, PyBytes>> {
    crate::turbo::write::python::py_write_excel_turbo_bytes(
        py, sheets, "inline", true, false, false, None, 0, None, None, None, None, false, None,
        None, false, true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn parse(s: &str) -> Expr {
        parse_formula(s).unwrap_or_else(|e| panic!("parse {s:?} failed: {e:?}"))
    }

    fn refs(s: &str) -> Vec<String> {
        let mut out = Vec::new();
        collect_refs(&parse(s), &mut out);
        out.sort();
        out.dedup();
        out
    }

    #[test]
    fn dependencies_walker_collects_cell_refs() {
        assert_eq!(refs("=A1*2"), vec!["A1".to_string()]);
        assert_eq!(
            refs("=SUM(A1:B2)+C5"),
            vec!["A1:B2".to_string(), "C5".to_string()]
        );
        assert_eq!(
            refs("=SUM(A1:B2, C1)"),
            vec!["A1:B2".to_string(), "C1".to_string()]
        );
        assert_eq!(
            refs("=IF(A1>0, 'Sheet2'!B2, 0)"),
            vec!["A1".to_string(), "Sheet2!B2".to_string()]
        );
        assert_eq!(refs("=SUM(1,2,3)"), Vec::<String>::new());
        assert_eq!(
            refs("=XLOOKUP(A1, B:B, C2:C5)"),
            vec!["A1".to_string(), "B:B".to_string(), "C2:C5".to_string(),]
        );
    }

    #[test]
    fn dependencies_deduplicates_repeated_refs() {
        assert_eq!(refs("=A1+A1+A1"), vec!["A1".to_string()]);
    }

    /// Every name in the static inventory must actually be registered, so the
    /// inventory can never drift into reporting a function that does not exist.
    #[test]
    fn category_table_entries_are_all_registered() {
        let missing: Vec<&str> = FUNCTIONS_BY_CATEGORY
            .iter()
            .map(|(n, _)| *n)
            .filter(|n| registry().get(n).is_none())
            .collect();
        assert!(
            missing.is_empty(),
            "table names not registered: {missing:?}"
        );
    }

    #[test]
    fn public_inventory_is_the_488_function_standard_contract() {
        let names: std::collections::HashSet<&str> = FUNCTIONS_BY_CATEGORY
            .iter()
            .map(|(name, _)| *name)
            .collect();
        assert_eq!(FUNCTIONS_BY_CATEGORY.len(), 488, "inventory row count");
        assert_eq!(names.len(), 488, "inventory contains duplicate names");
    }

    #[test]
    fn context_grid_resolves_cells() {
        let mut g = ContextGrid {
            cells: HashMap::new(),
        };
        g.cells.insert((0, 0), CalcValue::Number(42.0));
        assert_eq!(g.cell(0, 0, 0), Some(CalcValue::Number(42.0)));
        assert_eq!(g.cell(0, 1, 0), None);
        assert_eq!(g.cell(1, 0, 0), None);
        assert_eq!(g.sheet_index("Sheet1"), Some(0));
        assert_eq!(g.sheet_index("sheet1"), Some(0));
        assert_eq!(g.sheet_index("Anything"), None);
    }
}
