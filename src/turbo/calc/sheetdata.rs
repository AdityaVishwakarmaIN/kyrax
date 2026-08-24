// calc/sheetdata.rs — the workbook data resolver the eval loop reads through.
//
// This is the sheetdata/name-table layer: it owns a snapshot of the workbook's
// value grid (built once), implements `functions::CellResolver` so functions
// read cells through the registry's `FuncCtx`, implements `deps::RefResolver`
// so the dependency graph can map sheet/defined names to ids, and provides a
// write-back path that stores computed results into
// `CellValue::Formula.cached` on the caller's `Workbook`.
//
// Ownership model (see the gap round): the evaluator never holds a borrow of
// the `Workbook` during evaluation. `SheetData::build(&Workbook)` copies the
// values it needs into an owned grid and drops the borrow; functions read only
// through `&SheetData` (`&dyn CellResolver` inside `FuncCtx`, lifetime-free at
// the registry boundary via the HRTB `Func` type). Mid-evaluation updates go
// through `update_computed`, and the results are committed to the model once,
// at the end, by `write_back(&Workbook)`.
//
// # Used-region clamping rule
//
// Range reads are clamped to the sheet's **used region** — the inclusive
// bounding box of every value-bearing cell physically present on the sheet
// (0-based: `(row0, col0) ..= (row1, col1)`). A whole-column reference such as
// `C:C` therefore yields at most `used_rows × 1` cells, never 1,048,576, and
// a whole-row reference such as `3:5` yields at most `used_cols × n`, never
// 16,384 × n.
//
// Clamping is **value-safe**: every cell that exists lies inside the used
// region, and every cell outside it carries no value at all — it reads as
// blank, contributes 0 to `SUM`, and cannot carry an error. `SUM(C:C)` still
// returns exactly the full-column result, because a column's values cannot
// live beyond the sheet's used bounding box and anything outside contributes
// nothing. Cells whose only presence is formatting (styled-but-empty) extend
// nothing: they hold no value, so they cannot affect any computed result.
//
// No pyo3 anywhere; pure std.

use crate::turbo::calc::ast::{CellRef, RangeRef, RefCore};
use crate::turbo::calc::deps::{NameTarget, RefResolver};
use crate::turbo::calc::functions::{CellResolver, MAX_COLS, MAX_ROWS};
use crate::turbo::calc::value::{ArrayValue, CalcError, CalcValue};
use crate::turbo::write::model::{CachedValue, CellValue, DefinedName, Workbook};
use std::collections::HashMap;

/// Upper bound on the cells a single range read will materialize into a dense
/// array, mirroring `functions::MAX_MATERIALIZED_CELLS` (which is private).
/// Larger spans error with `#VALUE!` instead of risking a huge allocation;
/// whole-row/column consumers of genuinely huge sheets should iterate via
/// `CellResolver::cell` instead.
const MAX_DENSE_CELLS: usize = 4_000_000;

/// A cell position within the owned grid. Coordinates are 0-based; `col` stays
/// `u16` (Excel columns top out at 16,384).
#[derive(Clone, Debug)]
struct GridCell {
    row: u32,
    col: u16,
    value: CalcValue,
    is_formula: bool,
}

/// The 0-based inclusive used-region bounding box of one sheet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsedRegion {
    pub row0: u32,
    pub col0: u16,
    pub row1: u32,
    pub col1: u16,
}

impl UsedRegion {
    fn of(cells: &[GridCell]) -> Option<UsedRegion> {
        let mut min_r = u32::MAX;
        let mut max_r = 0u32;
        let mut min_c = u16::MAX;
        let mut max_c = 0u16;
        for c in cells {
            min_r = min_r.min(c.row);
            max_r = max_r.max(c.row);
            min_c = min_c.min(c.col);
            max_c = max_c.max(c.col);
        }
        if min_r == u32::MAX {
            None
        } else {
            Some(UsedRegion {
                row0: min_r,
                col0: min_c,
                row1: max_r,
                col1: max_c,
            })
        }
    }
}

#[derive(Clone, Debug)]
struct GridSheet {
    bounds: Option<UsedRegion>,
    cells: Vec<GridCell>,
}

#[derive(Clone, Debug)]
struct GridData {
    sheets: Vec<GridSheet>,
    /// Lowercased sheet name → sheet index (Excel sheet lookup is
    /// case-insensitive).
    name_to_sheet: HashMap<String, u32>,
    /// Lowercased defined-name → defined name (Excel name lookup is
    /// case-insensitive).
    defined: HashMap<(String, Option<u32>), DefinedName>,
    /// Copied from `WriteOptions` at build time.
    date1904: bool,
}

/// The workbook data resolver. Owns an immutable snapshot of the grid; the
/// caller's `Workbook` is only borrowed at `build` and `write_back`.
#[derive(Clone, Debug)]
pub struct SheetData {
    data: GridData,
}

fn lower(s: &str) -> String {
    s.to_ascii_lowercase()
}

/// Excel sheet names arrive in references possibly wrapped in single quotes
/// (`'My Sheet'`); strip a balanced pair before lookup. A name that genuinely
/// begins and ends with `'` is not a legal Excel sheet name, so this is safe.
fn unquoted(s: &str) -> &str {
    let b = s.as_bytes();
    if b.len() >= 2 && b[0] == b'\'' && b[b.len() - 1] == b'\'' {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

impl SheetData {
    /// Build the resolver from the write-model workbook. Copies every
    /// value-bearing cell into the owned grid (texts/arrays are `Arc`-shared
    /// by `CalcValue`, so this is a refcount bump per cell, never a deep copy)
    /// and precomputes the used-region bounds and the name maps.
    pub fn build(wb: &Workbook) -> Self {
        let mut sheets = Vec::with_capacity(wb.sheets.len());
        let mut name_to_sheet = HashMap::with_capacity(wb.sheets.len());
        for (i, s) in wb.sheets.iter().enumerate() {
            let mut cells = Vec::new();
            for row in &s.rows {
                let r = row.row.saturating_sub(1);
                for cell in &row.cells {
                    let col = cell.col.saturating_sub(1);
                    let col = match u16::try_from(col) {
                        Ok(c) => c,
                        Err(_) => continue, // col beyond 16,384 cannot hold a value
                    };
                    let Some(value) = cell_value(&cell.value) else {
                        continue; // Empty cells (and rich text we cannot read) carry no value
                    };
                    cells.push(GridCell {
                        row: r,
                        col,
                        value,
                        is_formula: matches!(cell.value, CellValue::Formula { .. }),
                    });
                }
            }
            cells.sort_by_key(|c| (c.row, c.col));
            let bounds = UsedRegion::of(&cells);
            let name_lower = lower(unquoted(&s.name));
            name_to_sheet.insert(name_lower, i as u32);
            sheets.push(GridSheet { bounds, cells });
        }
        let mut defined = HashMap::with_capacity(wb.defined_names.len());
        for dn in &wb.defined_names {
            defined.insert((lower(&dn.name), dn.local_sheet_id), dn.clone());
        }
        let date1904 = wb.options.date1904;
        Self {
            data: GridData {
                sheets,
                name_to_sheet,
                defined,
                date1904,
            },
        }
    }

    /// The workbook's date system (copied from `WriteOptions` at build).
    pub fn date1904(&self) -> bool {
        self.data.date1904
    }

    /// Case-insensitive lookup of a defined name. Returns the raw
    /// `DefinedName` (name text, value string, scope); resolving the value
    /// string into coordinates is the reference-parsing layer's job, so this
    /// never guesses at a target.
    pub fn defined_name(&self, name: &str, scope: Option<u32>) -> Option<&DefinedName> {
        self.data.defined.get(&(lower(unquoted(name)), scope))
    }

    fn scoped_name_target(&self, name: &str, scope: u32) -> NameTarget {
        let Some(dn) = self.defined_name(name, Some(scope)) else {
            return NameTarget::Unknown;
        };
        let Some(core) = bare_ref_core(&dn.value) else {
            return NameTarget::Unknown;
        };
        NameTarget::Ref { sheet: scope, core }
    }

    /// Store a freshly computed value for a formula or materialized spill cell
    /// so that dependent formulas read it through `CellResolver::cell`. Returns
    /// `false` only if the sheet index is out of range.
    pub fn update_computed(&mut self, sheet: u32, row: u32, col: u32, value: CalcValue) -> bool {
        let Some(gs) = self.data.sheets.get_mut(sheet as usize) else {
            return false;
        };
        let Ok(col) = u16::try_from(col) else {
            return false;
        };
        match gs
            .cells
            .binary_search_by(|c| (c.row, c.col).cmp(&(row, col)))
        {
            Ok(i) => {
                gs.cells[i].value = value;
            }
            Err(pos) => {
                gs.cells.insert(
                    pos,
                    GridCell {
                        row,
                        col,
                        value,
                        is_formula: false,
                    },
                );
                if let Some(ref mut b) = gs.bounds {
                    b.row0 = b.row0.min(row);
                    b.row1 = b.row1.max(row);
                    b.col0 = b.col0.min(col);
                    b.col1 = b.col1.max(col);
                } else {
                    gs.bounds = Some(UsedRegion {
                        row0: row,
                        col0: col,
                        row1: row,
                        col1: col,
                    });
                }
            }
        }
        true
    }

    /// Commit computed results to the caller's workbook: for every formula
    /// cell whose grid value is a cacheable scalar, set
    /// `CellValue::Formula.cached`. Cells whose value is blank, an array, or an
    /// internal-only error are left untouched — the caller must route those to
    /// the fallback path (`fullCalcOnLoad="1"`); a cache is never fabricated.
    /// Returns the number of cached slots written.
    pub fn write_back(&self, wb: &mut Workbook) -> usize {
        let mut written = 0;
        for (s, gs) in self.data.sheets.iter().enumerate() {
            let Some(sheet) = wb.sheets.get_mut(s) else {
                continue;
            };
            for g in &gs.cells {
                if !g.is_formula {
                    continue;
                }
                let Some(cv) = cache_of(&g.value) else {
                    continue;
                };
                let r = g.row + 1;
                let c = u32::from(g.col) + 1;
                let Some(row) = sheet.rows.iter_mut().find(|row| row.row == r) else {
                    continue;
                };
                let Some(cell) = row.cells.iter_mut().find(|cell| cell.col == c) else {
                    continue;
                };
                if let CellValue::Formula { cached, .. } = &mut cell.value {
                    *cached = Some(cv);
                    written += 1;
                }
            }
        }
        written
    }

    /// Read a clamped rectangular span of the grid as a dense array, honoring
    /// the used-region clamping rule (see the module doc). `Err(CalcError::Ref)`
    /// for an out-of-range sheet, `Err(CalcError::Value)` when the clamped span
    /// still exceeds [`MAX_DENSE_CELLS`].
    fn clamped_dense(
        &self,
        sheet: u32,
        r0: u32,
        r1: u32,
        c0: u16,
        c1: u16,
    ) -> Result<CalcValue, CalcError> {
        let gs = self.data.sheets.get(sheet as usize).ok_or(CalcError::Ref)?;
        let Some(b) = gs.bounds else {
            // No used region: the span is empty, never a million-cell array.
            return Ok(CalcValue::array(ArrayValue::new(0, 0, Vec::new())));
        };
        let cr0 = r0.max(b.row0);
        let cr1 = r1.min(b.row1);
        let cc0 = c0.max(b.col0);
        let cc1 = c1.min(b.col1);
        if cr0 > cr1 || cc0 > cc1 {
            return Ok(CalcValue::array(ArrayValue::new(0, 0, Vec::new())));
        }
        let nrows = cr1 - cr0 + 1;
        let ncols = u32::from(cc1 - cc0 + 1);
        if (nrows as usize).saturating_mul(ncols as usize) > MAX_DENSE_CELLS {
            return Err(CalcError::Value);
        }
        let nrows_u = nrows as usize;
        let ncols_u = ncols as usize;
        let mut data = Vec::with_capacity(nrows_u * ncols_u);
        let mut idx = gs.cells.partition_point(|c| (c.row, c.col) < (cr0, cc0));
        for r in cr0..=cr1 {
            for c in cc0..=cc1 {
                while idx < gs.cells.len() && (gs.cells[idx].row, gs.cells[idx].col) < (r, c) {
                    idx += 1;
                }
                if idx < gs.cells.len() && gs.cells[idx].row == r && gs.cells[idx].col == c {
                    data.push(if gs.cells[idx].value.is_blank() {
                        CalcValue::Blank
                    } else {
                        gs.cells[idx].value.clone()
                    });
                    idx += 1;
                } else {
                    data.push(CalcValue::Blank);
                }
            }
        }
        Ok(CalcValue::array(ArrayValue::new(nrows, ncols, data)))
    }
}

impl CellResolver for SheetData {
    fn cell(&self, sheet: u32, row: u32, col: u32) -> Option<CalcValue> {
        let gs = self.data.sheets.get(sheet as usize)?;
        let Ok(col) = u16::try_from(col) else {
            return None;
        };
        let i = gs
            .cells
            .binary_search_by(|c| (c.row, c.col).cmp(&(row, col)))
            .ok()?;
        let v = &gs.cells[i].value;
        if v.is_blank() {
            None
        } else {
            Some(v.clone())
        }
    }

    fn sheet_index(&self, name: &str) -> Option<u32> {
        self.data.name_to_sheet.get(&lower(unquoted(name))).copied()
    }

    /// Overridden to enforce the used-region clamping rule on every reference
    /// shape; the default `resolve_core` would materialize a whole-column
    /// reference as a 1,048,576-cell array.
    fn resolve_core(&self, sheet: u32, core: &RefCore) -> Result<CalcValue, CalcError> {
        match core {
            RefCore::Cell(c) => Ok(self
                .cell(sheet, c.row, u32::from(c.col))
                .unwrap_or(CalcValue::Blank)),
            RefCore::Range(r) => {
                if r.end.row < r.start.row || r.end.col < r.start.col {
                    return Err(CalcError::Value);
                }
                self.clamped_dense(sheet, r.start.row, r.end.row, r.start.col, r.end.col)
            }
            RefCore::Row(r) => {
                if r.end < r.start {
                    return Err(CalcError::Value);
                }
                self.clamped_dense(sheet, r.start, r.end, 0, MAX_COLS - 1)
            }
            RefCore::Column(c) => {
                if c.end < c.start {
                    return Err(CalcError::Value);
                }
                self.clamped_dense(sheet, 0, MAX_ROWS - 1, c.start, c.end)
            }
        }
    }
}

impl RefResolver for SheetData {
    fn sheet_id(&self, name: &str) -> Option<u32> {
        self.sheet_index(name)
    }

    /// Resolve a defined name to a static target when its value is a plain
    /// local cell/range reference (e.g. `=$B$2` or `A1:C3`) scoped to a sheet.
    /// Everything else — constants, sheet-qualified values, function values,
    /// workbook-scope bare references — resolves `Unknown` so the caller routes
    /// the cell to the fallback path. Never guesses.
    fn resolve_name_scoped(
        &self,
        name: &str,
        sheet: Option<&str>,
        current_sheet: u32,
    ) -> NameTarget {
        if let Some(sheet_name) = sheet {
            let Some(scope) = self.sheet_id(sheet_name) else {
                return NameTarget::Unknown;
            };
            return self.scoped_name_target(name, scope);
        }

        // A local name shadows a workbook name on its own sheet. Workbook
        // names remain a safe fallback until their potentially relative or
        // sheet-qualified definitions are represented by the reference AST.
        if self.defined_name(name, Some(current_sheet)).is_some() {
            self.scoped_name_target(name, current_sheet)
        } else {
            NameTarget::Unknown
        }
    }
}

/// A `CachedValue` is only written back when it is a scalar the XML schema can
/// represent. Blank, arrays, and internal-only error codes produce `None` —
/// never a fabricated cache.
fn cache_of(v: &CalcValue) -> Option<CachedValue> {
    match v {
        CalcValue::Number(n) => Some(CachedValue::Number(*n)),
        CalcValue::Text(s) => Some(CachedValue::Str(s.to_string())),
        CalcValue::Bool(b) => Some(CachedValue::Bool(*b)),
        CalcValue::Error(e) if e.cacheable() => Some(CachedValue::Error(e.code().to_string())),
        _ => None,
    }
}

/// Map a write-model cell value into a `CalcValue`. `None` means the cell
/// carries no value (`Empty`, or rich text whose plain text this layer cannot
/// read without a type-name it is not allowed to see) — it reads as blank.
fn cell_value(cv: &CellValue) -> Option<CalcValue> {
    match cv {
        CellValue::Empty => None,
        CellValue::Number(n) => Some(CalcValue::Number(*n)),
        CellValue::Bool(b) => Some(CalcValue::Bool(*b)),
        CellValue::Error(s) => Some(CalcValue::Error(
            CalcError::from_str_ci(s).unwrap_or(CalcError::Error),
        )),
        CellValue::Str(s) => Some(CalcValue::text(s.as_str())),
        CellValue::DateSerial(n) | CellValue::Time(n) | CellValue::Duration(n) => {
            Some(CalcValue::Number(*n))
        }
        CellValue::Rich(_) => None,
        CellValue::Formula { cached, .. } => Some(match cached {
            Some(CachedValue::Number(n)) => CalcValue::Number(*n),
            Some(CachedValue::Bool(b)) => CalcValue::Bool(*b),
            Some(CachedValue::Error(s)) => {
                CalcValue::Error(CalcError::from_str_ci(s).unwrap_or(CalcError::Error))
            }
            Some(CachedValue::Str(s)) => CalcValue::text(s.as_str()),
            None => CalcValue::Blank,
        }),
    }
}

/// Parse a defined-name value that is a bare local cell/range reference like
/// `=$B$2` or `A1:C3`. `None` for anything else — constants, sheet-qualified
/// values, function calls, malformed text. The `$` markers are stripped; each
/// endpoint must be `letters + digits` so `TRUE`, `A`, and `0.08` never
/// misparse as cells.
fn bare_ref_core(value: &str) -> Option<RefCore> {
    let v = value.trim();
    let v = v.strip_prefix('=').unwrap_or(v).trim();
    if v.is_empty() {
        return None;
    }
    if v.contains('!') || v.contains('(') || v.contains('[') || v.contains(' ') {
        return None;
    }
    let cleaned = v.replace('$', "");
    if cleaned.is_empty() || !cleaned.as_bytes()[0].is_ascii_alphabetic() {
        return None;
    }
    let parts: Vec<&str> = cleaned.split(':').collect();
    if parts.is_empty() || parts.len() > 2 {
        return None;
    }
    if !parts.iter().all(|p| is_a1_cell(p)) {
        return None;
    }
    let (r0, c0, r1, c1) = crate::turbo::scan::parse_ref_range(cleaned.as_bytes());
    let c0 = u16::try_from(c0).ok()?;
    let c1 = u16::try_from(c1).ok()?;
    let start = CellRef {
        col: c0,
        row: r0,
        abs_col: false,
        abs_row: false,
    };
    if parts.len() == 1 {
        Some(RefCore::Cell(start))
    } else {
        Some(RefCore::Range(RangeRef {
            start,
            end: CellRef {
                col: c1,
                row: r1,
                abs_col: false,
                abs_row: false,
            },
        }))
    }
}

/// A single A1 endpoint: one-or-more letters followed by one-or-more digits.
fn is_a1_cell(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_alphabetic() {
        i += 1;
    }
    i > 0 && i < b.len() && b[i..].iter().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turbo::calc::ast::{ColumnRef, RefExpr};
    use crate::turbo::calc::functions::FuncCtx;
    use crate::turbo::write::model::{Cell, FormulaKind, Row, Sheet};
    use pretty_assertions::assert_eq;

    /// Sales sheet: A1=10, B1=20, C1 =A1+B1 (cached 30), A2=5, C2 formula
    /// with no cache, C3=7. Used region = rows 1..3, cols A..C.
    fn workbook() -> Workbook {
        let mut r1 = Row::new(1);
        r1.cells.push(Cell::new(1, CellValue::Number(10.0)));
        r1.cells.push(Cell::new(2, CellValue::Number(20.0)));
        r1.cells.push(Cell::new(
            3,
            CellValue::Formula {
                text: "=A1+B1".into(),
                kind: FormulaKind::Normal,
                cached: Some(CachedValue::Number(30.0)),
            },
        ));
        let mut r2 = Row::new(2);
        r2.cells.push(Cell::new(1, CellValue::Number(5.0)));
        r2.cells.push(Cell::new(
            3,
            CellValue::Formula {
                text: "=A2*2".into(),
                kind: FormulaKind::Normal,
                cached: None,
            },
        ));
        let mut r3 = Row::new(3);
        r3.cells.push(Cell::new(3, CellValue::Number(7.0)));
        let mut sheet = Sheet::new("Sales");
        sheet.rows = vec![r1, r2, r3];
        let mut wb = Workbook::new();
        wb.sheets = vec![sheet];
        wb
    }

    /// A bare `CellRef` (range endpoints need this, not a wrapped `RefCore`).
    fn cref(col: u16, row: u32) -> CellRef {
        CellRef {
            col,
            row,
            abs_col: false,
            abs_row: false,
        }
    }

    fn cellref(col: u16, row: u32) -> RefCore {
        RefCore::Cell(cref(col, row))
    }

    #[test]
    fn sheet_lookup_is_case_insensitive() {
        let sd = SheetData::build(&workbook());
        assert_eq!(sd.sheet_index("Sales"), Some(0));
        assert_eq!(sd.sheet_index("SALES"), Some(0));
        assert_eq!(sd.sheet_index("sales"), Some(0));
        assert_eq!(sd.sheet_index("Nope"), None);
        assert_eq!(sd.sheet_index(""), None);
    }

    #[test]
    fn missing_cell_is_blank_not_zero() {
        let sd = SheetData::build(&workbook());
        assert_eq!(sd.cell(0, 100, 0), None);
        assert_eq!(sd.cell(0, 0, 99), None);
        assert_eq!(sd.cell(0, 3, 0), None); // beyond used region
        assert_eq!(sd.cell(9, 0, 0), None); // bad sheet
        assert_eq!(
            sd.resolve_core(0, &cellref(5, 5)).unwrap(),
            CalcValue::Blank
        );
        // Uncomputed formula cell (C2) reads as blank, not zero.
        assert_eq!(sd.cell(0, 1, 2), None);
    }

    #[test]
    fn range_shape_is_exact() {
        let sd = SheetData::build(&workbook());
        let rng = RefCore::Range(RangeRef {
            start: cref(0, 0),
            end: cref(2, 2),
        });
        match sd.resolve_core(0, &rng).unwrap() {
            CalcValue::Array(a) => {
                assert_eq!(a.shape(), (3, 3));
                assert_eq!(a.get(0, 0), &CalcValue::Number(10.0));
                assert_eq!(a.get(0, 1), &CalcValue::Number(20.0));
                assert_eq!(a.get(0, 2), &CalcValue::Number(30.0));
                assert_eq!(a.get(1, 0), &CalcValue::Number(5.0));
                assert_eq!(a.get(1, 2), &CalcValue::Blank);
                assert_eq!(a.get(2, 2), &CalcValue::Number(7.0));
            }
            other => panic!("expected an array, got {other:?}"),
        }
    }

    #[test]
    fn whole_column_is_clamped_to_used_region() {
        let sd = SheetData::build(&workbook());
        let col = RefCore::Column(ColumnRef { start: 2, end: 2 });
        match sd.resolve_core(0, &col).unwrap() {
            CalcValue::Array(a) => {
                // 3 rows, NOT 1,048,576.
                assert_eq!(a.shape(), (3, 1));
                assert_eq!(a.get(0, 0), &CalcValue::Number(30.0));
                assert_eq!(a.get(1, 0), &CalcValue::Blank);
                assert_eq!(a.get(2, 0), &CalcValue::Number(7.0));
                let sum: f64 = a.iter().filter_map(CalcValue::as_number).sum();
                assert_eq!(sum, 37.0); // SUM(C:C) = 30 + 0 + 7
            }
            other => panic!("expected an array, got {other:?}"),
        }
    }

    #[test]
    fn whole_row_is_clamped_to_used_columns() {
        let sd = SheetData::build(&workbook());
        let row = RefCore::Row(crate::turbo::calc::ast::RowRef { start: 0, end: 0 });
        match sd.resolve_core(0, &row).unwrap() {
            CalcValue::Array(a) => {
                assert_eq!(a.shape(), (1, 3)); // NOT 16,384 columns
                assert_eq!(a.get(0, 0), &CalcValue::Number(10.0));
                assert_eq!(a.get(0, 1), &CalcValue::Number(20.0));
                assert_eq!(a.get(0, 2), &CalcValue::Number(30.0));
            }
            other => panic!("expected an array, got {other:?}"),
        }
    }

    #[test]
    fn oversized_explicit_range_is_clamped() {
        let sd = SheetData::build(&workbook());
        let rng = RefCore::Range(RangeRef {
            start: cref(0, 0),
            end: cref(u16::MAX, MAX_ROWS - 1),
        });
        match sd.resolve_core(0, &rng).unwrap() {
            CalcValue::Array(a) => assert_eq!(a.shape(), (3, 3)),
            other => panic!("expected an array, got {other:?}"),
        }
    }

    #[test]
    fn empty_sheet_whole_column_is_empty_array() {
        let mut wb = workbook();
        wb.sheets.push(Sheet::new("Empty"));
        let sd = SheetData::build(&wb);
        let col = RefCore::Column(ColumnRef { start: 0, end: 5 });
        match sd.resolve_core(1, &col).unwrap() {
            CalcValue::Array(a) => assert_eq!(a.shape(), (0, 0)),
            other => panic!("expected an empty array, got {other:?}"),
        }
    }

    #[test]
    fn resolves_through_the_registry_funcctx() {
        let sd = SheetData::build(&workbook());
        let ctx = FuncCtx {
            date1904: false,
            sheet: 0,
            row: 0,
            col: 0,
            resolver: &sd,
        };
        let col = RefExpr::Local(RefCore::Column(ColumnRef { start: 2, end: 2 }));
        match ctx.resolve(&col).unwrap() {
            CalcValue::Array(a) => assert_eq!(a.shape(), (3, 1)),
            other => panic!("expected a clamped array, got {other:?}"),
        }
        assert_eq!(ctx.cell(0, 2), Some(CalcValue::Number(30.0)));
        assert_eq!(ctx.cell(5, 2), None);
    }

    #[test]
    fn defined_names_resolve_case_insensitively() {
        let mut wb = workbook();
        wb.defined_names.push(DefinedName {
            name: "Tax".into(),
            value: "=$B$2".into(),
            local_sheet_id: Some(0),
            hidden: false,
        });
        wb.defined_names.push(DefinedName {
            name: "Gross".into(),
            value: "A1:C3".into(),
            local_sheet_id: None,
            hidden: false,
        });
        let sd = SheetData::build(&wb);
        assert_eq!(
            sd.defined_name("Tax", Some(0)).map(|d| d.name.as_str()),
            Some("Tax")
        );
        assert_eq!(
            sd.defined_name("TAX", Some(0)).map(|d| d.name.as_str()),
            Some("Tax")
        );
        assert_eq!(
            sd.defined_name("tax", Some(0)).map(|d| d.name.as_str()),
            Some("Tax")
        );
        assert_eq!(
            sd.defined_name("Gross", None).map(|d| d.name.as_str()),
            Some("Gross")
        );
        assert_eq!(sd.defined_name("zzz", None), None);

        // Sheet-scoped bare ref → real edge; workbook-scope bare ref → Unknown.
        assert_eq!(
            sd.resolve_name_scoped("Tax", None, 0),
            NameTarget::Ref {
                sheet: 0,
                core: cellref(1, 1),
            }
        );
        assert_eq!(
            sd.resolve_name_scoped("Gross", None, 0),
            NameTarget::Unknown
        );
        assert_eq!(sd.resolve_name_scoped("zzz", None, 0), NameTarget::Unknown);
    }

    #[test]
    fn same_named_locals_keep_their_sheet_scope() {
        let mut wb = workbook();
        wb.sheets.push(Sheet::new("Other"));
        wb.defined_names.push(DefinedName {
            name: "Tax".into(),
            value: "=$B$2".into(),
            local_sheet_id: Some(0),
            hidden: false,
        });
        wb.defined_names.push(DefinedName {
            name: "tax".into(),
            value: "=$C$3".into(),
            local_sheet_id: Some(1),
            hidden: false,
        });
        let sd = SheetData::build(&wb);

        assert_eq!(
            sd.resolve_name_scoped("TAX", Some("Sales"), 1),
            NameTarget::Ref {
                sheet: 0,
                core: cellref(1, 1),
            }
        );
        assert_eq!(
            sd.resolve_name_scoped("Tax", Some("Other"), 0),
            NameTarget::Ref {
                sheet: 1,
                core: cellref(2, 2),
            }
        );
        assert_eq!(
            sd.resolve_name_scoped("tax", None, 1),
            NameTarget::Ref {
                sheet: 1,
                core: cellref(2, 2),
            }
        );
        assert_eq!(
            sd.resolve_name_scoped("Tax", Some("Missing"), 0),
            NameTarget::Unknown
        );
    }

    #[test]
    fn date1904_flag_is_exposed() {
        let mut wb = workbook();
        assert!(!SheetData::build(&wb).date1904());
        wb.options.date1904 = true;
        assert!(SheetData::build(&wb).date1904());
    }

    #[test]
    fn write_back_fills_formula_caches() {
        let mut wb = workbook();
        let mut sd = SheetData::build(&wb);
        // C1 recomputes to 99; C2 stays uncomputed (blank).
        assert!(sd.update_computed(0, 0, 2, CalcValue::Number(99.0)));
        assert_eq!(sd.cell(0, 0, 2), Some(CalcValue::Number(99.0))); // dependents see it
        assert!(!sd.update_computed(0, 0, 0, CalcValue::Number(1.0))); // not a formula
        assert!(!sd.update_computed(9, 0, 0, CalcValue::Number(1.0))); // bad sheet

        assert_eq!(sd.write_back(&mut wb), 1);

        let c1 = &wb.sheets[0].rows[0].cells[2].value;
        match c1 {
            CellValue::Formula { cached, .. } => {
                assert_eq!(cached, &Some(CachedValue::Number(99.0)))
            }
            other => panic!("expected a formula with a fresh cache, got {other:?}"),
        }
        let c2 = &wb.sheets[0].rows[1].cells[1].value;
        match c2 {
            CellValue::Formula { cached, .. } => assert_eq!(cached, &None),
            other => panic!("expected an untouched formula, got {other:?}"),
        }
    }
}
