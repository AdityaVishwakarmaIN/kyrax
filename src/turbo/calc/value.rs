// calc/value.rs — the calculation value and error contract.
//
// Scalars are inline; text and arrays are `Arc`-shared so cloning a value is a
// refcount bump, never a deep copy. Errors are a `Copy` enum (no payloads), so
// propagation is zero-alloc and the eval loop can short-circuit on `is_error()`.

use std::fmt;
use std::sync::Arc;

/// Excel error taxonomy (spec `01_parser_reference.md` §6).
///
/// `cacheable()` partitions the codes: the nine legal Excel error codes may be
/// written to XML as a cached `t="e"` value; the rest are internal-only and
/// MUST route the cell to the uncomputed fallback path. Writing an internal
/// code as a cache would produce a file Excel cannot read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CalcError {
    /// `#NULL!`
    Null,
    /// `#DIV/0!`
    Div0,
    /// `#VALUE!`
    Value,
    /// `#REF!`
    Ref,
    /// `#NAME?`
    Name,
    /// `#NUM!`
    Num,
    /// `#N/A`
    Na,
    /// `#SPILL!`
    Spill,
    /// `#CALC!`
    Calc,
    /// `#CYCLE!` — internal-only; never a cached value.
    Cycle,
    /// `#ERROR!` — internal-only (parser-level generic).
    Error,
    /// `#GETTING_DATA` — internal-only.
    GettingData,
}

impl CalcError {
    /// The nine codes Excel accepts as cached `t="e"` values.
    pub const CACHEABLE: [CalcError; 9] = [
        CalcError::Null,
        CalcError::Div0,
        CalcError::Value,
        CalcError::Ref,
        CalcError::Name,
        CalcError::Num,
        CalcError::Na,
        CalcError::Spill,
        CalcError::Calc,
    ];

    /// Whether this code may be written to XML as a cached `t="e"` value.
    pub fn cacheable(self) -> bool {
        matches!(
            self,
            CalcError::Null
                | CalcError::Div0
                | CalcError::Value
                | CalcError::Ref
                | CalcError::Name
                | CalcError::Num
                | CalcError::Na
                | CalcError::Spill
                | CalcError::Calc
        )
    }

    /// Internal-only codes: a cell evaluating to one of these must be routed
    /// to the uncomputed fallback path (`CalcReport::fallback`), never written
    /// as a cached `<v>`.
    pub fn is_internal(self) -> bool {
        !self.cacheable()
    }

    /// The literal code, e.g. `"#DIV/0!"`. Safe for XML display only when
    /// `cacheable()` is true.
    pub fn code(self) -> &'static str {
        match self {
            CalcError::Null => "#NULL!",
            CalcError::Div0 => "#DIV/0!",
            CalcError::Value => "#VALUE!",
            CalcError::Ref => "#REF!",
            CalcError::Name => "#NAME?",
            CalcError::Num => "#NUM!",
            CalcError::Na => "#N/A",
            CalcError::Spill => "#SPILL!",
            CalcError::Calc => "#CALC!",
            CalcError::Cycle => "#CYCLE!",
            CalcError::Error => "#ERROR!",
            CalcError::GettingData => "#GETTING_DATA",
        }
    }

    /// Parse an error literal (case-insensitive, optional leading `=`).
    pub fn from_str_ci(s: &str) -> Option<CalcError> {
        let s = s.trim();
        let s = s.strip_prefix('=').unwrap_or(s);
        match s.to_ascii_uppercase().as_str() {
            "#NULL!" => Some(CalcError::Null),
            "#DIV/0!" => Some(CalcError::Div0),
            "#VALUE!" => Some(CalcError::Value),
            "#REF!" => Some(CalcError::Ref),
            "#NAME?" => Some(CalcError::Name),
            "#NUM!" => Some(CalcError::Num),
            "#N/A" => Some(CalcError::Na),
            "#SPILL!" => Some(CalcError::Spill),
            "#CALC!" => Some(CalcError::Calc),
            "#CYCLE!" => Some(CalcError::Cycle),
            "#ERROR!" => Some(CalcError::Error),
            "#GETTING_DATA" => Some(CalcError::GettingData),
            _ => None,
        }
    }
}

impl fmt::Display for CalcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl std::str::FromStr for CalcError {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_str_ci(s).ok_or(())
    }
}

/// A calculation value.
///
/// Invariant: `NaN`/`Inf` never enter a `CalcValue` — coercion maps them to
/// `CalcError::Num` — so `PartialEq` is total and `Debug` output stays clean.
#[derive(Clone, Debug, PartialEq)]
pub enum CalcValue {
    Number(f64),
    /// Shared text; cloning is a refcount bump.
    Text(Arc<str>),
    Bool(bool),
    /// Blank cell / empty argument; coerces to `0`, `""` or `FALSE` depending
    /// on context (spec §9).
    Blank,
    Error(CalcError),
    /// Dense array; cloning is a refcount bump.
    Array(Arc<ArrayValue>),
}

/// Dense row-major array value. Non-rectangular `{...}` literals are padded to
/// the maximum column count at parse time (spec §7.3).
#[derive(Clone, Debug, PartialEq)]
pub struct ArrayValue {
    pub rows: u32,
    pub cols: u32,
    pub data: Box<[CalcValue]>,
}

impl ArrayValue {
    pub fn new(rows: u32, cols: u32, data: Vec<CalcValue>) -> Self {
        debug_assert!(data.len() == (rows as usize) * (cols as usize));
        Self {
            rows,
            cols,
            data: data.into_boxed_slice(),
        }
    }

    /// Row-major element access; panics on out-of-range coordinates.
    pub fn get(&self, r: u32, c: u32) -> &CalcValue {
        &self.data[(r as usize) * (self.cols as usize) + (c as usize)]
    }

    pub fn shape(&self) -> (u32, u32) {
        (self.rows, self.cols)
    }

    /// A 1x1 array — the spill-scalarization case at eval.
    pub fn is_scalar_array(&self) -> bool {
        self.rows == 1 && self.cols == 1
    }

    pub fn iter(&self) -> impl Iterator<Item = &CalcValue> {
        self.data.iter()
    }
}

impl CalcValue {
    pub fn number(n: f64) -> Self {
        CalcValue::Number(n)
    }

    pub fn text(s: impl Into<Arc<str>>) -> Self {
        CalcValue::Text(s.into())
    }

    pub fn bool(b: bool) -> Self {
        CalcValue::Bool(b)
    }

    pub fn blank() -> Self {
        CalcValue::Blank
    }

    pub fn err(e: CalcError) -> Self {
        CalcValue::Error(e)
    }

    pub fn array(a: ArrayValue) -> Self {
        CalcValue::Array(Arc::new(a))
    }

    /// Error short-circuit. `Copy`, so the check is zero-alloc.
    pub fn is_error(&self) -> bool {
        matches!(self, CalcValue::Error(_))
    }

    pub fn error(&self) -> Option<CalcError> {
        match self {
            CalcValue::Error(e) => Some(*e),
            _ => None,
        }
    }

    pub fn is_blank(&self) -> bool {
        matches!(self, CalcValue::Blank)
    }

    pub fn is_array(&self) -> bool {
        matches!(self, CalcValue::Array(_))
    }

    /// Exact variant access; no coercion.
    pub fn as_number(&self) -> Option<f64> {
        match self {
            CalcValue::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            CalcValue::Text(s) => Some(s.as_ref()),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            CalcValue::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

impl From<f64> for CalcValue {
    fn from(n: f64) -> Self {
        CalcValue::Number(n)
    }
}

impl From<bool> for CalcValue {
    fn from(b: bool) -> Self {
        CalcValue::Bool(b)
    }
}

impl From<CalcError> for CalcValue {
    fn from(e: CalcError) -> Self {
        CalcValue::Error(e)
    }
}

impl From<String> for CalcValue {
    fn from(s: String) -> Self {
        CalcValue::text(s)
    }
}

impl From<&str> for CalcValue {
    fn from(s: &str) -> Self {
        CalcValue::text(s)
    }
}

impl From<ArrayValue> for CalcValue {
    fn from(a: ArrayValue) -> Self {
        CalcValue::array(a)
    }
}

impl fmt::Display for CalcValue {
    /// General-format rendering: `Number` via [`crate::turbo::calc::coerce`]'s
    /// `number_to_general`, errors as their code, blank as `""`, arrays as a
    /// shape summary (not a value Excel would produce).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CalcValue::Number(n) => f.write_str(&super::coerce::number_to_general(*n)),
            CalcValue::Text(s) => f.write_str(s),
            CalcValue::Bool(b) => f.write_str(if *b { "TRUE" } else { "FALSE" }),
            CalcValue::Blank => Ok(()),
            CalcValue::Error(e) => f.write_str(e.code()),
            CalcValue::Array(a) => write!(f, "Array({}x{})", a.rows, a.cols),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn cacheable_partition() {
        for e in CalcError::CACHEABLE {
            assert!(e.cacheable(), "{} should be cacheable", e.code());
            assert!(!e.is_internal(), "{} should not be internal", e.code());
        }
        for e in [CalcError::Cycle, CalcError::Error, CalcError::GettingData] {
            assert!(!e.cacheable(), "{} must not be cacheable", e.code());
            assert!(e.is_internal(), "{} must be internal", e.code());
        }
    }

    #[test]
    fn parse_all_codes_case_insensitive() {
        let all = [
            CalcError::Null,
            CalcError::Div0,
            CalcError::Value,
            CalcError::Ref,
            CalcError::Name,
            CalcError::Num,
            CalcError::Na,
            CalcError::Spill,
            CalcError::Calc,
            CalcError::Cycle,
            CalcError::Error,
            CalcError::GettingData,
        ];
        for e in all {
            assert_eq!(CalcError::from_str_ci(e.code()), Some(e), "{}", e.code());
            let lower = e.code().to_ascii_lowercase();
            assert_eq!(CalcError::from_str_ci(&lower), Some(e), "{lower}");
        }
        assert_eq!(CalcError::from_str_ci("=#N/A"), Some(CalcError::Na));
        assert_eq!(CalcError::from_str_ci("=#n/a"), Some(CalcError::Na));
        assert_eq!(CalcError::from_str_ci(" nope "), None);
    }

    #[test]
    fn value_roundtrips() {
        let t = CalcValue::text("hi");
        assert_eq!(t.as_text(), Some("hi"));
        assert_eq!(CalcValue::number(2.5).as_number(), Some(2.5));
        assert_eq!(CalcValue::bool(false).as_bool(), Some(false));
        assert_eq!(CalcValue::err(CalcError::Na).error(), Some(CalcError::Na));
        assert!(CalcValue::Blank.is_blank());
        let a = CalcValue::array(ArrayValue::new(
            2,
            2,
            vec![
                CalcValue::Number(1.0),
                CalcValue::Number(2.0),
                CalcValue::Number(3.0),
                CalcValue::Number(4.0),
            ],
        ));
        assert!(a.is_array());
        let arr = match &a {
            CalcValue::Array(x) => x.clone(),
            _ => unreachable!(),
        };
        assert_eq!(arr.shape(), (2, 2));
        assert_eq!(arr.get(1, 0), &CalcValue::Number(3.0));
        assert_eq!(a.to_string(), "Array(2x2)");
    }
}
