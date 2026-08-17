// calc/deps.rs — dependency graph, topological evaluation order, cycle detection.
//
// Consumes the parsed AST from `ast.rs` (`Expr`, `RefExpr`, `RefCore`) and
// produces, per formula-bearing cell, the set of cells it must be ordered after,
// the evaluation sequence, and any circular reference groups. This module holds
// no value semantics; the exec loop in a later wave reads `CalcValue`s by
// `CellKey`. Pure std — no pyo3, no external crates.
//
// The caller supplies name/sheet resolution through `RefResolver`; anything it
// cannot resolve (an unknown defined name, a missing sheet, an external ref, a
// colon whose endpoints are dynamic, an address function such as OFFSET/INDIRECT)
// marks the owning cell `unresolved`. Such cells are never emitted into the
// evaluation order — the caller must leave them uncomputed and set
// `fullCalcOnLoad="1"`. `topological` also cascades that status to every
// transitive dependent, so a cell that reads an unresolved or cyclic cell is
// likewise excluded. The engine never fabricates a value.

use crate::turbo::calc::ast::{Expr, RefCore, RefExpr, TableRef};
use std::collections::{HashMap, HashSet};

/// A formula-bearing cell, 0-based. `sheet` is a caller-assigned sheet id
/// (names are mapped to ids by `RefResolver::sheet_id`). Derives `Ord` so all
/// iteration (DFS roots, cycle reporting, output vectors) is deterministic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CellKey {
    pub sheet: u32,
    pub row: u32,
    pub col: u16,
}

impl CellKey {
    pub fn new(sheet: u32, row: u32, col: u16) -> Self {
        Self { sheet, row, col }
    }
}

/// A precedent rectangle: every formula cell whose position falls inside
/// `(sheet_from..=sheet_to, row_from..=row_to, col_from..=col_to)` is a
/// precedent of the cell that read this rect. Rows/columns are inclusive and
/// 0-based; `row_to`/`col_to` are `None` on the unbounded axis (whole-row and
/// whole-column refs). `sheet_from`/`sheet_to` carry 3-D refs (`S1:S3!A1:B5`);
/// a plain ref has both equal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rect {
    pub sheet_from: u32,
    pub sheet_to: u32,
    pub row_from: u32,
    pub row_to: Option<u32>,
    pub col_from: u16,
    pub col_to: Option<u16>,
}

/// What a defined name / structured table reference resolved to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NameTarget {
    /// A concrete cell/range/row/column on a sheet (a real dependency).
    Ref { sheet: u32, core: RefCore },
    /// A constant (e.g. `rate = 0.08`): no dependency edge at all.
    Constant,
    /// Not resolvable: the reading cell must be routed to the fallback path.
    Unknown,
}

/// Resolves sheet names, defined names and structured table references to the
/// ids/cores `deps.rs` needs. Implementations come from the sheetdata/name-table
/// layer at eval time. Default methods return `Unknown`, so a caller that does
/// not implement a capability gets safe fallback rather than a fabricated edge.
pub trait RefResolver {
    fn sheet_id(&self, name: &str) -> Option<u32>;

    fn resolve_name(&self, _name: &str) -> NameTarget {
        NameTarget::Unknown
    }

    /// Resolve a defined name using its explicit qualifier, when present, or
    /// the formula cell's sheet for an unqualified local name. Implementors
    /// that do not understand scope must fall back rather than ignore it.
    fn resolve_name_scoped(
        &self,
        _name: &str,
        _sheet: Option<&str>,
        _current_sheet: u32,
    ) -> NameTarget {
        NameTarget::Unknown
    }

    fn resolve_table(&self, _table: &TableRef) -> NameTarget {
        NameTarget::Unknown
    }
}

/// Why a cell's precedents could not be fully determined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnresolvedReason {
    /// A sheet name in a `!`-qualified or 3-D ref did not map to an id.
    Sheet(String),
    /// A defined name did not resolve.
    Name(String),
    /// A structured table reference did not resolve.
    Table(String),
    /// An external `[Book.xlsx]!...` reference (unsupported).
    External(String),
    /// `expr:expr` where at least one endpoint is not a static reference
    /// (e.g. `INDIRECT("A5"):B10`); the range is only known at eval time.
    DynamicColon,
    /// An address function (OFFSET/INDIRECT/INDEX/…) whose produced range is
    /// discovered at execution time, not statically.
    AddressFunction(String),
}

/// Error from `DependencyGraph::add_formula`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DepError {
    /// The same cell was registered twice.
    DuplicateCell(CellKey),
    /// The formula was registered with an incomplete precedent set (the node
    /// IS inserted, with its resolvable rects, so ordering stays a superset);
    /// the caller should route `cell` to the fallback path.
    Unresolved {
        cell: CellKey,
        reason: UnresolvedReason,
    },
}

/// Output of `DependencyGraph::topological`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TopoResult {
    /// Evaluation order, precedents before dependents. Contains exactly the
    /// cells the caller may compute and cache — cyclic cells, unresolved cells,
    /// and every transitive dependent of either are excluded (never emitted).
    pub order: Vec<CellKey>,
    /// Each distinct cycle, members only (sorted; order of the outer vec is
    /// deterministic). One entry per unique member set.
    pub cycles: Vec<Vec<CellKey>>,
    /// Cells excluded from `order`: cycle members + unresolved cells + all
    /// cells transitively reading any of them. The caller must leave these
    /// uncomputed (`CalcReport::fallback`).
    pub fallback: Vec<CellKey>,
}

/// The dependency graph.
///
/// # Keying strategy
///
/// Nodes are formula-bearing cells keyed by `(sheet, row, col)`. Each node
/// stores its precedent **rectangles, unexpanded** — never one key per cell of
/// a range — so memory is proportional to the number of formulas plus the
/// number of ranges they read, not to range area.
///
/// Edges are materialized once, lazily, inside `topological`, via a dual
/// spatial index over node positions: per sheet, `column -> sorted rows` and
/// `row -> sorted columns` of formula cells. A rect query picks the smaller of
/// the two axes and binary-searches:
///
/// * whole-row ref `3:5` → 3 row lookups (columns unbounded),
/// * whole-column ref `B:D` → 3 column lookups (rows unbounded),
/// * a bounded `A1:B10000` → iterates the narrower axis.
///
/// Query cost is therefore `min(Δcols, Δrows) · log n`, i.e. proportional to
/// the number of nonempty rows/columns the rect spans, never to the rect's
/// cell count. A 200k-cell sheet stays flat in both time and memory.
///
/// Ordering and cycle detection run as an **iterative** DFS with an explicit
/// frame stack (never recursion), so a chain of any depth cannot overflow the
/// Rust stack. A gray-node back edge identifies a cycle; its member set is
/// sliced out of the current stack.
pub struct DependencyGraph {
    nodes: HashMap<CellKey, Node>,
    col_rows: HashMap<u32, HashMap<u16, Vec<u32>>>,
    row_cols: HashMap<u32, HashMap<u32, Vec<u16>>>,
}

struct Node {
    rects: Vec<Rect>,
    unresolved: bool,
}

impl DependencyGraph {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            col_rows: HashMap::new(),
            row_cols: HashMap::new(),
        }
    }

    /// Register one formula cell. Extracts precedent rectangles from the parsed
    /// `expr`; sheet names / defined names / table refs go through `resolver`.
    ///
    /// Returns `Ok` when every dependency resolved statically. On
    /// `Err(DepError::Unresolved)` the node is still inserted (with the subset
    /// of rects that did resolve) so ordering stays a conservative superset —
    /// the error only tells the caller to mark the cell fallback. The cell's
    /// row/column are added to the spatial index, sorted at `topological` time.
    pub fn add_formula(
        &mut self,
        cell: CellKey,
        expr: &Expr,
        resolver: &dyn RefResolver,
    ) -> Result<(), DepError> {
        if self.nodes.contains_key(&cell) {
            return Err(DepError::DuplicateCell(cell));
        }
        let mut rects = Vec::new();
        let mut reason = None;
        collect(expr, cell.sheet, resolver, &mut rects, &mut reason);
        self.nodes.insert(
            cell,
            Node {
                rects,
                unresolved: reason.is_some(),
            },
        );
        self.col_rows
            .entry(cell.sheet)
            .or_default()
            .entry(cell.col)
            .or_default()
            .push(cell.row);
        self.row_cols
            .entry(cell.sheet)
            .or_default()
            .entry(cell.row)
            .or_default()
            .push(cell.col);
        match reason {
            Some(reason) => Err(DepError::Unresolved { cell, reason }),
            None => Ok(()),
        }
    }

    /// Register a formula cell whose expression could not be produced at all —
    /// the formula text failed to parse, or uses syntax the AST cannot
    /// represent. The node carries no precedent rects but IS marked
    /// unresolved, so `topological` keeps it and every cell that reads it out
    /// of `order` and lists them in `fallback`. Without this the caller would
    /// compute a dependent from a stale or blank value and cache a wrong
    /// number — the one unacceptable failure mode.
    pub fn add_unparseable(&mut self, cell: CellKey) -> Result<(), DepError> {
        if self.nodes.contains_key(&cell) {
            return Err(DepError::DuplicateCell(cell));
        }
        self.nodes.insert(
            cell,
            Node {
                rects: Vec::new(),
                unresolved: true,
            },
        );
        self.col_rows
            .entry(cell.sheet)
            .or_default()
            .entry(cell.col)
            .or_default()
            .push(cell.row);
        self.row_cols
            .entry(cell.sheet)
            .or_default()
            .entry(cell.row)
            .or_default()
            .push(cell.col);
        Ok(())
    }

    /// Sort the spatial index, resolve every node's rects to concrete
    /// precedent cells, and run an iterative DFS producing the evaluation
    /// order and the cycle report. Excludes cyclic / unresolved / cascaded
    /// cells from `order`; the caller computes only `order`.
    pub fn topological(&mut self) -> TopoResult {
        for map in self.col_rows.values_mut() {
            for rows in map.values_mut() {
                rows.sort_unstable();
            }
        }
        for map in self.row_cols.values_mut() {
            for cols in map.values_mut() {
                cols.sort_unstable();
            }
        }

        let edges = self.materialize_edges();

        // Seed the skip set with unresolved cells; DFS exit-time checks cascade
        // the status to every dependent (a node exits only after all its
        // precedents have exited, so their final status is known here).
        let mut skip: HashSet<CellKey> = HashSet::new();
        for (cell, node) in &self.nodes {
            if node.unresolved {
                skip.insert(*cell);
            }
        }

        let mut color: HashMap<CellKey, u8> = HashMap::with_capacity(self.nodes.len());
        let mut order: Vec<CellKey> = Vec::with_capacity(self.nodes.len());
        let mut cycle_members: HashSet<CellKey> = HashSet::new();
        let mut cycles_set: HashSet<Vec<CellKey>> = HashSet::new();

        let mut roots: Vec<CellKey> = self.nodes.keys().copied().collect();
        roots.sort_unstable();

        for root in roots {
            if color.get(&root).copied().unwrap_or(0) != 0 {
                continue;
            }
            let mut frames: Vec<(CellKey, usize)> = Vec::new();
            let mut stack: Vec<CellKey> = Vec::new();
            color.insert(root, 1);
            stack.push(root);
            frames.push((root, 0));

            while let Some(&(cell, idx)) = frames.last() {
                let pres: &[CellKey] = edges.get(&cell).map(|v| v.as_slice()).unwrap_or(&[]);
                if idx >= pres.len() {
                    frames.pop();
                    stack.pop();
                    color.insert(cell, 2);
                    let in_cycle = cycle_members.contains(&cell)
                        || pres.iter().any(|p| cycle_members.contains(p));
                    let blocked = skip.contains(&cell) || pres.iter().any(|p| skip.contains(p));
                    if in_cycle || blocked {
                        skip.insert(cell);
                    } else {
                        order.push(cell);
                    }
                } else {
                    let p = pres[idx];
                    frames.last_mut().unwrap().1 += 1;
                    match color.get(&p).copied().unwrap_or(0) {
                        0 => {
                            color.insert(p, 1);
                            stack.push(p);
                            frames.push((p, 0));
                        }
                        1 => {
                            // Back edge: p is an ancestor (or the node itself,
                            // a self-loop); stack[p..] is a cycle.
                            let pos = stack.iter().position(|&x| x == p).unwrap();
                            let mut cycle: Vec<CellKey> = stack[pos..].to_vec();
                            cycle.sort_unstable();
                            cycles_set.insert(cycle);
                            for &c in &stack[pos..] {
                                cycle_members.insert(c);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        let mut cycles: Vec<Vec<CellKey>> = cycles_set.into_iter().collect();
        cycles.sort();

        let mut fallback: Vec<CellKey> = skip.into_iter().collect();
        fallback.sort_unstable();

        TopoResult {
            order,
            cycles,
            fallback,
        }
    }

    /// Every formula cell's direct precedents, sorted and deduplicated.
    ///
    /// Public because [`crate::turbo::features::provenance`] is the only way to
    /// get at the graph after hydration: `topological` consumes the edges and
    /// returns an order, and an order cannot be run backwards into edges. This
    /// is the sole accessor that layer needs — it takes `&self`, so a caller
    /// can query a graph it only borrows.
    pub fn materialize_edges(&self) -> HashMap<CellKey, Vec<CellKey>> {
        let mut edges: HashMap<CellKey, Vec<CellKey>> = HashMap::with_capacity(self.nodes.len());
        for (cell, node) in &self.nodes {
            let mut pre: Vec<CellKey> = Vec::new();
            for rect in &node.rects {
                self.query_into(rect, &mut pre);
            }
            pre.sort_unstable();
            pre.dedup();
            edges.insert(*cell, pre);
        }
        edges
    }

    /// Append every registered formula cell inside `rect` to `out`. Iterates
    /// the smaller of the two axes (rows vs columns), binary-searching the
    /// other, so cost tracks the rect's span of nonempty rows/columns.
    fn query_into(&self, rect: &Rect, out: &mut Vec<CellKey>) {
        for sheet in rect.sheet_from..=rect.sheet_to {
            let row_span: u64 = match rect.row_to {
                Some(t) => (t as u64).saturating_sub(rect.row_from as u64) + 1,
                None => u64::MAX,
            };
            let col_span: u64 = match rect.col_to {
                Some(t) => (t as u64).saturating_sub(rect.col_from as u64) + 1,
                None => u64::MAX,
            };
            if col_span <= row_span {
                let Some(map) = self.col_rows.get(&sheet) else {
                    continue;
                };
                match rect.col_to {
                    Some(col_to) => {
                        for c in rect.col_from..=col_to {
                            let Some(rows) = map.get(&c) else {
                                continue;
                            };
                            let lo = rows.partition_point(|&r| r < rect.row_from);
                            let hi = match rect.row_to {
                                Some(t) => rows.partition_point(|&r| r <= t),
                                None => rows.len(),
                            };
                            for &r in &rows[lo..hi] {
                                out.push(CellKey {
                                    sheet,
                                    row: r,
                                    col: c,
                                });
                            }
                        }
                    }
                    None => {
                        for (&c, rows) in map {
                            if c < rect.col_from {
                                continue;
                            }
                            let lo = rows.partition_point(|&r| r < rect.row_from);
                            let hi = match rect.row_to {
                                Some(t) => rows.partition_point(|&r| r <= t),
                                None => rows.len(),
                            };
                            for &r in &rows[lo..hi] {
                                out.push(CellKey {
                                    sheet,
                                    row: r,
                                    col: c,
                                });
                            }
                        }
                    }
                }
            } else {
                let Some(map) = self.row_cols.get(&sheet) else {
                    continue;
                };
                match rect.row_to {
                    Some(row_to) => {
                        for r in rect.row_from..=row_to {
                            let Some(cols) = map.get(&r) else {
                                continue;
                            };
                            let lo = cols.partition_point(|&c| c < rect.col_from);
                            let hi = match rect.col_to {
                                Some(t) => cols.partition_point(|&c| c <= t),
                                None => cols.len(),
                            };
                            for &c in &cols[lo..hi] {
                                out.push(CellKey {
                                    sheet,
                                    row: r,
                                    col: c,
                                });
                            }
                        }
                    }
                    None => {
                        for (&r, cols) in map {
                            if r < rect.row_from {
                                continue;
                            }
                            let lo = cols.partition_point(|&c| c < rect.col_from);
                            let hi = match rect.col_to {
                                Some(t) => cols.partition_point(|&c| c <= t),
                                None => cols.len(),
                            };
                            for &c in &cols[lo..hi] {
                                out.push(CellKey {
                                    sheet,
                                    row: r,
                                    col: c,
                                });
                            }
                        }
                    }
                }
            }
        }
    }
}

impl Default for DependencyGraph {
    fn default() -> Self {
        Self::new()
    }
}

/// Walk the AST, collecting every statically-determinable precedent rect.
/// Anything that cannot be resolved marks `reason`; refs still found along the
/// way are kept (conservative superset — safe for ordering).
fn collect(
    expr: &Expr,
    own_sheet: u32,
    resolver: &dyn RefResolver,
    out: &mut Vec<Rect>,
    reason: &mut Option<UnresolvedReason>,
) {
    match expr {
        Expr::Value(_) | Expr::Null | Expr::LambdaParam(_) => {}
        Expr::Ref(r) => collect_ref(r, own_sheet, resolver, out, reason),
        Expr::Unary(_, inner) | Expr::Suffix(_, inner) => {
            collect(inner, own_sheet, resolver, out, reason)
        }
        Expr::Binary(_, l, r) => {
            collect(l, own_sheet, resolver, out, reason);
            collect(r, own_sheet, resolver, out, reason);
        }
        Expr::Colon(l, r) => {
            // Static endpoints were already folded to RefCore::Range by the
            // parser; a Colon node with a static endpoint pair is a union of
            // two rects. A dynamic endpoint (INDIRECT(...):B10) means the real
            // range is unknown until eval — flag it.
            let both_static = matches!(l.as_ref(), Expr::Ref(RefExpr::Local(_)))
                && matches!(r.as_ref(), Expr::Ref(RefExpr::Local(_)));
            if !both_static {
                mark(reason, UnresolvedReason::DynamicColon);
            }
            collect(l, own_sheet, resolver, out, reason);
            collect(r, own_sheet, resolver, out, reason);
        }
        Expr::Union(children) => {
            for c in children {
                collect(c, own_sheet, resolver, out, reason);
            }
        }
        Expr::Function { name, args } => {
            let up = name.to_ascii_uppercase();
            if is_address_function(&up) {
                mark(reason, UnresolvedReason::AddressFunction(up));
            }
            for a in args {
                collect(a, own_sheet, resolver, out, reason);
            }
        }
        Expr::Lambda { body, .. } => collect(body, own_sheet, resolver, out, reason),
        Expr::Formula(children) => {
            for c in children {
                collect(c, own_sheet, resolver, out, reason);
            }
        }
    }
}

fn collect_ref(
    r: &RefExpr,
    own_sheet: u32,
    resolver: &dyn RefResolver,
    out: &mut Vec<Rect>,
    reason: &mut Option<UnresolvedReason>,
) {
    match r {
        RefExpr::Local(core) => push_core(core, own_sheet, own_sheet, out),
        RefExpr::Sheet { name, inner } => match resolver.sheet_id(name) {
            Some(id) => push_core(inner, id, id, out),
            None => mark(reason, UnresolvedReason::Sheet(name.clone())),
        },
        RefExpr::Sheet3D { from, to, inner } => {
            let (f, t) = match (resolver.sheet_id(from), resolver.sheet_id(to)) {
                (Some(f), Some(t)) => (f, t),
                _ => {
                    mark(reason, UnresolvedReason::Sheet(format!("{from}:{to}")));
                    return;
                }
            };
            let (f, t) = if f <= t { (f, t) } else { (t, f) };
            push_core(inner, f, t, out);
        }
        RefExpr::Name { name, sheet } => {
            match resolver.resolve_name_scoped(name, sheet.as_deref(), own_sheet) {
                NameTarget::Ref { sheet, core } => push_core(&core, sheet, sheet, out),
                NameTarget::Constant => {}
                NameTarget::Unknown => mark(reason, UnresolvedReason::Name(name.clone())),
            }
        }
        RefExpr::Table(t) => match resolver.resolve_table(t) {
            NameTarget::Ref { sheet, core } => push_core(&core, sheet, sheet, out),
            NameTarget::Constant => {}
            NameTarget::Unknown => mark(reason, UnresolvedReason::Table(t.name.clone())),
        },
        RefExpr::External { book, .. } => {
            mark(reason, UnresolvedReason::External(book.clone()));
        }
    }
}

fn push_core(core: &RefCore, sheet_from: u32, sheet_to: u32, out: &mut Vec<Rect>) {
    let rect = match core {
        RefCore::Cell(c) => Rect {
            sheet_from,
            sheet_to,
            row_from: c.row,
            row_to: Some(c.row),
            col_from: c.col,
            col_to: Some(c.col),
        },
        RefCore::Range(r) => Rect {
            sheet_from,
            sheet_to,
            row_from: r.start.row.min(r.end.row),
            row_to: Some(r.start.row.max(r.end.row)),
            col_from: r.start.col.min(r.end.col),
            col_to: Some(r.start.col.max(r.end.col)),
        },
        RefCore::Row(r) => Rect {
            sheet_from,
            sheet_to,
            row_from: r.start.min(r.end),
            row_to: Some(r.start.max(r.end)),
            col_from: 0,
            col_to: None,
        },
        RefCore::Column(c) => Rect {
            sheet_from,
            sheet_to,
            row_from: 0,
            row_to: None,
            col_from: c.start.min(c.end),
            col_to: Some(c.start.max(c.end)),
        },
    };
    out.push(rect);
}

fn mark(reason: &mut Option<UnresolvedReason>, r: UnresolvedReason) {
    if reason.is_none() {
        *reason = Some(r);
    }
}

/// Functions whose output is a reference whose extent is only known at
/// execution time (spec `02_eval_reference.md` §4.3 `needsReferenceObject` /
/// `isAddress`). Reading cells that call these are routed to fallback until the
/// exec wave can pre-execute them for range discovery.
fn is_address_function(name: &str) -> bool {
    matches!(
        name,
        "OFFSET"
            | "INDIRECT"
            | "INDEX"
            | "RANK"
            | "ISREF"
            | "TYPE"
            | "ADDRESS"
            | "MAP"
            | "REDUCE"
            | "SCAN"
            | "BYROW"
            | "BYCOL"
            | "TO_DATE"
            | "EPOCHTODATE"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turbo::calc::CalcValue;
    use crate::turbo::calc::ast::{BinaryOp, CellRef, ColumnRef, RangeRef, RowRef};
    use pretty_assertions::assert_eq;

    fn ck(row: u32, col: u16) -> CellKey {
        CellKey::new(0, row, col)
    }

    fn cell(row: u32, col: u16) -> Expr {
        Expr::Ref(RefExpr::Local(RefCore::Cell(CellRef {
            col,
            row,
            abs_col: false,
            abs_row: false,
        })))
    }

    fn range(r1: u32, c1: u16, r2: u32, c2: u16) -> Expr {
        let start = CellRef {
            col: c1,
            row: r1,
            abs_col: false,
            abs_row: false,
        };
        let end = CellRef {
            col: c2,
            row: r2,
            abs_col: false,
            abs_row: false,
        };
        Expr::Ref(RefExpr::Local(RefCore::Range(RangeRef { start, end })))
    }

    fn rowref(r1: u32, r2: u32) -> Expr {
        Expr::Ref(RefExpr::Local(RefCore::Row(RowRef { start: r1, end: r2 })))
    }

    fn colref(c1: u16, c2: u16) -> Expr {
        Expr::Ref(RefExpr::Local(RefCore::Column(ColumnRef {
            start: c1,
            end: c2,
        })))
    }

    fn sum(args: Vec<Expr>) -> Expr {
        Expr::Function {
            name: "SUM".into(),
            args,
        }
    }

    fn binary(l: Expr, r: Expr) -> Expr {
        Expr::Binary(BinaryOp::Add, Box::new(l), Box::new(r))
    }

    fn lit(n: f64) -> Expr {
        Expr::Value(CalcValue::Number(n))
    }

    struct NullResolver;

    impl RefResolver for NullResolver {
        fn sheet_id(&self, _name: &str) -> Option<u32> {
            None
        }
        fn resolve_name(&self, _name: &str) -> NameTarget {
            NameTarget::Unknown
        }
    }

    struct MapResolver {
        sheets: HashMap<String, u32>,
        names: HashMap<String, NameTarget>,
    }

    impl MapResolver {
        fn new() -> Self {
            Self {
                sheets: HashMap::new(),
                names: HashMap::new(),
            }
        }
    }

    impl RefResolver for MapResolver {
        fn sheet_id(&self, name: &str) -> Option<u32> {
            self.sheets.get(name).copied()
        }
        fn resolve_name(&self, name: &str) -> NameTarget {
            self.names.get(name).cloned().unwrap_or(NameTarget::Unknown)
        }

        fn resolve_name_scoped(
            &self,
            name: &str,
            sheet: Option<&str>,
            _current_sheet: u32,
        ) -> NameTarget {
            if sheet.is_some() {
                NameTarget::Unknown
            } else {
                self.resolve_name(name)
            }
        }
    }

    #[test]
    fn linear_chain_ordering() {
        let mut g = DependencyGraph::new();
        let a = ck(0, 0);
        let b = ck(0, 1);
        let c = ck(0, 2);
        g.add_formula(a, &lit(1.0), &NullResolver).unwrap();
        g.add_formula(b, &cell(0, 0), &NullResolver).unwrap();
        g.add_formula(c, &cell(0, 1), &NullResolver).unwrap();
        let r = g.topological();
        assert_eq!(r.order, vec![a, b, c]);
        assert!(r.cycles.is_empty());
        assert!(r.fallback.is_empty());
    }

    #[test]
    fn diamond_dependency() {
        let mut g = DependencyGraph::new();
        let a1 = ck(0, 0);
        let b1 = ck(0, 1);
        let c1 = ck(0, 2);
        let d1 = ck(0, 3);
        g.add_formula(a1, &lit(1.0), &NullResolver).unwrap();
        g.add_formula(b1, &cell(0, 0), &NullResolver).unwrap();
        g.add_formula(c1, &cell(0, 0), &NullResolver).unwrap();
        g.add_formula(d1, &binary(cell(0, 1), cell(0, 2)), &NullResolver)
            .unwrap();
        let r = g.topological();
        let pos = |k: CellKey| r.order.iter().position(|&x| x == k).unwrap();
        assert!(pos(a1) < pos(b1));
        assert!(pos(a1) < pos(c1));
        assert!(pos(b1) < pos(d1));
        assert!(pos(c1) < pos(d1));
        assert_eq!(r.order.len(), 4);
        assert!(r.cycles.is_empty());
        assert!(r.fallback.is_empty());
    }

    #[test]
    fn range_precedent() {
        let mut g = DependencyGraph::new();
        let a1 = ck(0, 0);
        let a2 = ck(1, 0);
        let b1 = ck(0, 1);
        g.add_formula(a1, &lit(1.0), &NullResolver).unwrap();
        g.add_formula(a2, &lit(2.0), &NullResolver).unwrap();
        g.add_formula(b1, &sum(vec![range(0, 0, 2, 0)]), &NullResolver)
            .unwrap();
        let r = g.topological();
        let pos = |k: CellKey| r.order.iter().position(|&x| x == k).unwrap();
        assert!(pos(a1) < pos(b1));
        assert!(pos(a2) < pos(b1));
        assert!(!r.order.contains(&ck(2, 0))); // A3 has no formula, not a node
        assert!(r.cycles.is_empty());
        assert!(r.fallback.is_empty());
    }

    #[test]
    fn huge_row_range_is_not_expanded() {
        let mut g = DependencyGraph::new();
        let far = ck(999_999, 3);
        let reader = ck(0, 1);
        g.add_formula(far, &lit(7.0), &NullResolver).unwrap();
        // Rows 2..=1048575 across every column — still ~1M rows, so the test
        // proves no per-cell expansion. Starts at row 2 because a whole-sheet
        // row ref would cover the reader's own cell (row 0), which is a genuine
        // circular reference in Excel and is correctly detected as a cycle.
        g.add_formula(reader, &sum(vec![rowref(2, 1_048_575)]), &NullResolver)
            .unwrap();
        let r = g.topological();
        let pos = |k: CellKey| r.order.iter().position(|&x| x == k).unwrap();
        assert!(pos(far) < pos(reader));
        assert_eq!(r.order.len(), 2);
        assert!(r.cycles.is_empty());
    }

    #[test]
    fn whole_column_precedent() {
        let mut g = DependencyGraph::new();
        let in_a = ck(500, 0);
        let other = ck(3, 1);
        let reader = ck(0, 2);
        g.add_formula(in_a, &lit(1.0), &NullResolver).unwrap();
        g.add_formula(other, &lit(2.0), &NullResolver).unwrap();
        g.add_formula(reader, &sum(vec![colref(0, 0)]), &NullResolver)
            .unwrap();
        let r = g.topological();
        let pos = |k: CellKey| r.order.iter().position(|&x| x == k).unwrap();
        assert!(pos(in_a) < pos(reader));
        assert!(r.order.contains(&other)); // B4 not a precedent, still ordered
        assert_eq!(r.order.len(), 3);
        assert!(r.cycles.is_empty());
    }

    #[test]
    fn self_reference_cycle() {
        let mut g = DependencyGraph::new();
        let a1 = ck(0, 0);
        g.add_formula(a1, &binary(cell(0, 0), lit(1.0)), &NullResolver)
            .unwrap();
        let r = g.topological();
        assert_eq!(r.cycles, vec![vec![a1]]);
        assert!(r.order.is_empty());
        assert_eq!(r.fallback, vec![a1]);
    }

    #[test]
    fn two_cell_mutual_cycle() {
        let mut g = DependencyGraph::new();
        let a = ck(0, 0);
        let b = ck(0, 1);
        g.add_formula(a, &cell(0, 1), &NullResolver).unwrap();
        g.add_formula(b, &cell(0, 0), &NullResolver).unwrap();
        let r = g.topological();
        assert_eq!(r.cycles, vec![vec![a, b]]);
        assert!(r.order.is_empty());
        assert_eq!(r.fallback, vec![a, b]);
    }

    #[test]
    fn downstream_of_cycle_is_fallback() {
        let mut g = DependencyGraph::new();
        let a = ck(0, 0);
        let b = ck(0, 1);
        let c = ck(0, 2);
        g.add_formula(a, &cell(0, 1), &NullResolver).unwrap();
        g.add_formula(b, &cell(0, 0), &NullResolver).unwrap();
        g.add_formula(c, &cell(0, 1), &NullResolver).unwrap();
        let r = g.topological();
        assert_eq!(r.cycles, vec![vec![a, b]]);
        assert!(r.order.is_empty());
        assert_eq!(r.fallback, vec![a, b, c]);
    }

    #[test]
    fn cycle_does_not_swallow_independent_cells() {
        let mut g = DependencyGraph::new();
        let a = ck(0, 0);
        let b = ck(0, 1);
        let z = ck(9, 9);
        g.add_formula(a, &cell(0, 1), &NullResolver).unwrap();
        g.add_formula(b, &cell(0, 0), &NullResolver).unwrap();
        g.add_formula(z, &lit(5.0), &NullResolver).unwrap();
        let r = g.topological();
        assert_eq!(r.order, vec![z]);
        assert_eq!(r.cycles, vec![vec![a, b]]);
        assert_eq!(r.fallback, vec![a, b]);
    }

    #[test]
    fn defined_name_edge_and_constant() {
        let mut resolver = MapResolver::new();
        resolver.sheets.insert("Sheet1".into(), 0);
        resolver.names.insert(
            "tax".into(),
            NameTarget::Ref {
                sheet: 0,
                core: RefCore::Cell(CellRef {
                    col: 4,
                    row: 0,
                    abs_col: false,
                    abs_row: false,
                }),
            },
        );
        resolver.names.insert("rate".into(), NameTarget::Constant);

        let mut g = DependencyGraph::new();
        let e1 = ck(0, 4);
        let b1 = ck(0, 1);
        let c1 = ck(0, 2);
        g.add_formula(e1, &lit(0.1), &resolver).unwrap();
        g.add_formula(
            b1,
            &binary(
                Expr::Ref(RefExpr::Name {
                    name: "tax".into(),
                    sheet: None,
                }),
                lit(2.0),
            ),
            &resolver,
        )
        .unwrap();
        g.add_formula(
            c1,
            &binary(
                Expr::Ref(RefExpr::Name {
                    name: "rate".into(),
                    sheet: None,
                }),
                lit(2.0),
            ),
            &resolver,
        )
        .unwrap();
        let r = g.topological();
        let pos = |k: CellKey| r.order.iter().position(|&x| x == k).unwrap();
        assert!(pos(e1) < pos(b1)); // name → edge to its target cell
        assert!(r.order.contains(&c1)); // constant name → no edge
        assert!(r.cycles.is_empty());
        assert!(r.fallback.is_empty());
    }

    #[test]
    fn unknown_name_marks_fallback_and_cascades() {
        let mut g = DependencyGraph::new();
        let a = ck(0, 0);
        let b = ck(0, 1);
        let err = g
            .add_formula(
                a,
                &Expr::Ref(RefExpr::Name {
                    name: "nope".into(),
                    sheet: None,
                }),
                &NullResolver,
            )
            .unwrap_err();
        assert!(matches!(err, DepError::Unresolved { .. }));
        g.add_formula(b, &cell(0, 0), &NullResolver).unwrap();
        let r = g.topological();
        assert!(r.order.is_empty());
        assert_eq!(r.fallback, vec![a, b]); // downstream of unresolved cascades
        assert!(r.cycles.is_empty());
    }

    #[test]
    fn three_dimensional_ref_edges() {
        let mut resolver = MapResolver::new();
        for (n, id) in [("S1", 0u32), ("S2", 1), ("S3", 2)] {
            resolver.sheets.insert(n.to_string(), id);
        }
        let mut g = DependencyGraph::new();
        // S1!D1 reads S1:S3!A1:A5. The formula's own cell is outside column A,
        // so it is not covered by its precedent rect — a formula whose own cell
        // sits inside the range it reads is a genuine circular reference and is
        // correctly reported as a cycle instead of being ordered.
        let sum_cell = CellKey::new(0, 0, 3);
        let p1 = CellKey::new(0, 2, 0);
        let p2 = CellKey::new(1, 0, 0);
        let p3 = CellKey::new(2, 3, 0);
        let off = CellKey::new(1, 4, 1);
        for p in [p1, p2, p3, off] {
            g.add_formula(p, &lit(1.0), &resolver).unwrap();
        }
        let inner = RefCore::Range(RangeRef {
            start: CellRef {
                col: 0,
                row: 0,
                abs_col: false,
                abs_row: false,
            },
            end: CellRef {
                col: 0,
                row: 4,
                abs_col: false,
                abs_row: false,
            },
        });
        let e = Expr::Ref(RefExpr::Sheet3D {
            from: "S1".into(),
            to: "S3".into(),
            inner: Box::new(inner),
        });
        g.add_formula(sum_cell, &sum(vec![e]), &resolver).unwrap();
        let r = g.topological();
        let pos = |k: CellKey| r.order.iter().position(|&x| x == k).unwrap();
        for p in [p1, p2, p3] {
            assert!(pos(p) < pos(sum_cell), "{p:?} must precede the 3-D sum");
        }
        assert!(r.order.contains(&off)); // B5 outside A1:A5, not a precedent
        assert!(r.cycles.is_empty());
        assert!(r.fallback.is_empty());
    }

    #[test]
    fn deep_chain_uses_explicit_stack() {
        let mut g = DependencyGraph::new();
        let n: u32 = 20_000;
        g.add_formula(ck(0, 0), &lit(1.0), &NullResolver).unwrap();
        for i in 1..n {
            g.add_formula(ck(i, 0), &cell(i - 1, 0), &NullResolver)
                .unwrap();
        }
        let r = g.topological();
        assert_eq!(r.order.len(), n as usize);
        assert_eq!(r.order[0], ck(0, 0));
        assert!(r.cycles.is_empty());
        assert!(r.fallback.is_empty());
    }
}
