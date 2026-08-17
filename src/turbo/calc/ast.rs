// calc/ast.rs — AST node types and operator enums produced by the formula parser.
//
// Node shapes follow `01_parser_reference.md` §4. References carry resolved
// 0-based coordinates so the parser can normalize them and deps.rs can extract
// ranges cheaply; sheet/name/table parts stay as `String`s and are resolved to
// indices by sheetdata at eval time.

use crate::turbo::calc::value::CalcValue;

/// Binary operators (spec §2.1); precedence in spec §3.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Concat,
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}

impl BinaryOp {
    /// Binding level; lower number binds tighter. `^` is level 0.
    pub fn precedence(self) -> u8 {
        match self {
            BinaryOp::Pow => 0,
            BinaryOp::Mul | BinaryOp::Div => 1,
            BinaryOp::Add | BinaryOp::Sub => 2,
            BinaryOp::Concat => 3,
            BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::Gt
            | BinaryOp::Ge
            | BinaryOp::Lt
            | BinaryOp::Le => 4,
        }
    }

    pub fn is_compare(self) -> bool {
        matches!(
            self,
            BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Gt | BinaryOp::Ge | BinaryOp::Lt | BinaryOp::Le
        )
    }
}

/// Prefix operators (spec §2.3): unary `+`/`-` and `@` implicit intersection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    Plus,
    Minus,
    ImplicitIntersect,
}

/// Suffix operators (spec §2.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SuffixOp {
    Percent,
}

/// One cell coordinate. 0-based; `abs_*` mark the `$` pieces so the
/// shared-formula shifter and re-emitters can reproduce them (spec §5.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CellRef {
    pub col: u16,
    pub row: u32,
    pub abs_col: bool,
    pub abs_row: bool,
}

/// Inclusive 0-based cell range (`A1:B5`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RangeRef {
    pub start: CellRef,
    pub end: CellRef,
}

/// Full-width row range (`3:5`), 0-based inclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RowRef {
    pub start: u32,
    pub end: u32,
}

/// Full-height column range (`B:D`), 0-based inclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ColumnRef {
    pub start: u16,
    pub end: u16,
}

/// The cartesian part of a reference, without any sheet qualifier.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RefCore {
    Cell(CellRef),
    Range(RangeRef),
    Row(RowRef),
    Column(ColumnRef),
}

/// Column part of a structured table reference (spec §5.3).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TableColRef {
    Column(String),
    Range(String, String),
}

/// Section part of a structured table reference (spec §5.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TableSection {
    All,
    Data,
    Headers,
    Totals,
    ThisRow,
}

/// A structured table reference (spec §5.3); names are resolved against the
/// table map by sheetdata at eval time.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TableRef {
    pub book: Option<String>,
    pub name: String,
    pub sections: Vec<TableSection>,
    pub columns: Vec<TableColRef>,
}

/// A reference operand (spec §5).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum RefExpr {
    /// Bare `A1`, `A1:B5`, `1:3`, `A:C` on the formula's own sheet.
    Local(RefCore),
    /// `'My Sheet'!A1` — name resolved to a sheet index by sheetdata.
    Sheet { name: String, inner: Box<RefCore> },
    /// `Sheet5:Sheet6!A1:B10` (3-D, spec §5.2).
    Sheet3D {
        from: String,
        to: String,
        inner: Box<RefCore>,
    },
    /// Defined name, resolved against the name table at eval. `sheet` is
    /// present for an explicit qualifier such as `Sheet1!TaxRate`.
    Name { name: String, sheet: Option<String> },
    /// Structured table reference.
    Table(TableRef),
    /// `[Book.xlsx]...` — external; unsupported → fallback path, never a guess.
    External { book: String, inner: String },
}

/// A formula AST node.
#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    /// Literal: number, text, bool, error, or a `{...}` array folded to
    /// `CalcValue::Array` at parse time (spec §4 VALUE, §7.3).
    Value(CalcValue),
    /// Empty argument (e.g. `SUM(,A1)`) → evaluates to `CalcValue::Blank`.
    Null,
    Ref(RefExpr),
    /// Prefix: unary `+`/`-` and `@` implicit intersection.
    Unary(UnaryOp, Box<Expr>),
    /// Suffix: `%`.
    Suffix(SuffixOp, Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    /// Range colon; endpoints may be arbitrary expressions
    /// (`INDIRECT("A5"):B10`), not just references.
    Colon(Box<Expr>, Box<Expr>),
    /// `,` union of references in cube context (spec §4 UNION).
    Union(Vec<Expr>),
    Function {
        name: String,
        args: Vec<Expr>,
    },
    Lambda {
        params: Vec<String>,
        body: Box<Expr>,
    },
    /// Reference to an enclosing `LAMBDA` parameter by index.
    LambdaParam(usize),
    /// Formula root; more than one child → `#VALUE!` at eval (spec §4).
    Formula(Vec<Expr>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn precedence_matches_spec() {
        assert_eq!(BinaryOp::Pow.precedence(), 0);
        assert_eq!(BinaryOp::Mul.precedence(), 1);
        assert_eq!(BinaryOp::Div.precedence(), 1);
        assert_eq!(BinaryOp::Add.precedence(), 2);
        assert_eq!(BinaryOp::Sub.precedence(), 2);
        assert_eq!(BinaryOp::Concat.precedence(), 3);
        for op in [
            BinaryOp::Eq,
            BinaryOp::Ne,
            BinaryOp::Gt,
            BinaryOp::Ge,
            BinaryOp::Lt,
            BinaryOp::Le,
        ] {
            assert_eq!(op.precedence(), 4, "{op:?}");
            assert!(op.is_compare());
        }
        assert!(!BinaryOp::Add.is_compare());
    }
}
