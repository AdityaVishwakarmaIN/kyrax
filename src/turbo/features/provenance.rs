//! provenance.rs — cell provenance / dependency-query API (Tier 3 MEDIUM).
//!
//! openpyxl has no evaluator and therefore no notion of which cells read which;
//! this is ground neither library held before. The dependency graph itself is
//! already built (and then thrown away) by `calc/deps.rs` during hydration, so
//! the whole cost of this feature is the query layer on top of it — nothing
//! here recomputes the graph.
//!
//! `Provenance` wraps a *borrowed* `DependencyGraph` (never cloned: a real
//! workbook graph can hold millions of edges and copying it would blow the RSS
//! budget that is half our north-star metric). Edges are materialized once at
//! construction — the same O(V+E) pass `topological` already pays — and reused
//! by every query, so a query costs only the cells it visits. All outputs are
//! sorted by `(sheet, row, col)` (`CellKey`'s derived `Ord`), so the same graph
//! always yields the same `Vec`s. Deep traversals are iterative worklists with
//! a visited set — never recursion — so a chain tens of thousands deep cannot
//! overflow the stack, and every traversal terminates on a cycle.

use crate::turbo::calc::deps::{CellKey, DependencyGraph};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

/// A read-only query handle over a borrowed dependency graph.
pub struct Provenance<'a> {
    /// The borrowed graph; every edge answer derives from this, it is never
    /// copied or mutated.
    graph: &'a DependencyGraph,
    /// `cell ->` the cells it reads directly (sorted, deduplicated).
    forward: HashMap<CellKey, Vec<CellKey>>,
    /// `cell ->` the cells that read it directly (sorted).
    reverse: HashMap<CellKey, Vec<CellKey>>,
    /// Deterministic topological order: every precedent precedes its
    /// dependents, so `impact_of` output can be recomputed in one pass.
    topo: Vec<CellKey>,
}

impl<'a> Provenance<'a> {
    /// Build the query handle. Materializes the graph's edges once (via
    /// `DependencyGraph::materialize_edges`) and computes the reverse index and
    /// a deterministic topological order, all in O(V+E).
    pub fn new(graph: &'a DependencyGraph) -> Self {
        let forward = graph.materialize_edges();
        let reverse = build_reverse(&forward);
        let topo = topo_order(&forward, &reverse);
        Self {
            graph,
            forward,
            reverse,
            topo,
        }
    }

    /// The borrowed graph this handle was built from. Kept so the handle
    /// stays tied to the graph's lifetime and callers can hand it back out.
    pub fn graph(&self) -> &'a DependencyGraph {
        self.graph
    }

    /// The cells `cell` reads directly. Sorted; empty for a cell with no
    /// precedents or a cell absent from the graph.
    pub fn precedents(&self, cell: CellKey) -> Vec<CellKey> {
        self.forward.get(&cell).cloned().unwrap_or_default()
    }

    /// The cells that read `cell` directly. Sorted; empty for a cell nothing
    /// reads or a cell absent from the graph.
    pub fn dependents(&self, cell: CellKey) -> Vec<CellKey> {
        self.reverse.get(&cell).cloned().unwrap_or_default()
    }

    /// Every cell `cell` reads, directly or transitively. The seed appears in
    /// the result only when reachable from itself through a cycle. Iterative
    /// worklist; terminates on any cycle.
    pub fn precedents_deep(&self, cell: CellKey) -> Vec<CellKey> {
        let mut visited: HashSet<CellKey> = HashSet::new();
        let mut stack: Vec<CellKey> = Vec::new();
        if let Some(pres) = self.forward.get(&cell) {
            stack.extend(pres.iter().copied());
        }
        while let Some(c) = stack.pop() {
            if !visited.insert(c) {
                continue;
            }
            if let Some(pres) = self.forward.get(&c) {
                stack.extend(pres.iter().copied());
            }
        }
        let mut out: Vec<CellKey> = visited.into_iter().collect();
        out.sort_unstable();
        out
    }

    /// Every cell that reads `cell`, directly or transitively. The seed appears
    /// in the result only when reachable from itself through a cycle. Iterative
    /// worklist; terminates on any cycle.
    pub fn dependents_deep(&self, cell: CellKey) -> Vec<CellKey> {
        let mut visited: HashSet<CellKey> = HashSet::new();
        let mut stack: Vec<CellKey> = Vec::new();
        if let Some(deps) = self.reverse.get(&cell) {
            stack.extend(deps.iter().copied());
        }
        while let Some(c) = stack.pop() {
            if !visited.insert(c) {
                continue;
            }
            if let Some(deps) = self.reverse.get(&c) {
                stack.extend(deps.iter().copied());
            }
        }
        let mut out: Vec<CellKey> = visited.into_iter().collect();
        out.sort_unstable();
        out
    }

    /// Everything that would need recalculating if every cell in `cells`
    /// changed: the transitive dependents of the given cells, deduplicated
    /// across overlapping subtrees and emitted in topological order (precedents
    /// before dependents) so a caller recomputes in one forward pass. The
    /// seeds themselves are not included — they are the already-changed inputs.
    pub fn impact_of(&self, cells: &[CellKey]) -> Vec<CellKey> {
        let mut visited: HashSet<CellKey> = HashSet::new();
        let mut stack: Vec<CellKey> = Vec::new();
        for &cell in cells {
            if let Some(deps) = self.reverse.get(&cell) {
                stack.extend(deps.iter().copied());
            }
        }
        while let Some(c) = stack.pop() {
            if !visited.insert(c) {
                continue;
            }
            if let Some(deps) = self.reverse.get(&c) {
                stack.extend(deps.iter().copied());
            }
        }
        self.topo
            .iter()
            .copied()
            .filter(|c| visited.contains(c))
            .collect()
    }

    /// True when nothing reads `cell`. A cell absent from the graph has no
    /// dependents, so it is reported as a leaf.
    pub fn is_leaf(&self, cell: CellKey) -> bool {
        self.reverse.get(&cell).is_none_or(|d| d.is_empty())
    }

    /// The true inputs of the model: every formula cell with no precedents.
    /// Sorted.
    pub fn roots(&self) -> Vec<CellKey> {
        let mut roots: Vec<CellKey> = self
            .forward
            .iter()
            .filter(|(_, pres)| pres.is_empty())
            .map(|(&cell, _)| cell)
            .collect();
        roots.sort_unstable();
        roots
    }
}

/// Invert `forward` into `cell -> direct dependents`, each list sorted so every
/// public vector is deterministic.
fn build_reverse(forward: &HashMap<CellKey, Vec<CellKey>>) -> HashMap<CellKey, Vec<CellKey>> {
    let mut reverse: HashMap<CellKey, Vec<CellKey>> = HashMap::new();
    for (&cell, pres) in forward {
        for &p in pres {
            reverse.entry(p).or_default().push(cell);
        }
    }
    for v in reverse.values_mut() {
        v.sort_unstable();
    }
    reverse
}

/// Kahn's algorithm over the materialized edges: a node is emitted only after
/// all of its precedents, so every precedent precedes its dependents. The
/// ready set is a min-heap keyed by `CellKey`, so the emitted order is
/// identical for identical graphs regardless of `HashMap` iteration order.
/// Cycle members (never reaching in-degree zero) are appended in sorted order
/// at the end, together with anything that transitively reads them; a caller
/// recomputing `impact_of` output therefore never evaluates a cell before its
/// acyclic precedents.
fn topo_order(
    forward: &HashMap<CellKey, Vec<CellKey>>,
    reverse: &HashMap<CellKey, Vec<CellKey>>,
) -> Vec<CellKey> {
    let mut indegree: HashMap<CellKey, usize> = HashMap::with_capacity(forward.len());
    for (&cell, pres) in forward {
        indegree.entry(cell).or_insert(pres.len());
    }

    let mut ready: BinaryHeap<Reverse<CellKey>> = BinaryHeap::new();
    for (&cell, &deg) in &indegree {
        if deg == 0 {
            ready.push(Reverse(cell));
        }
    }

    let mut order: Vec<CellKey> = Vec::with_capacity(forward.len());
    while let Some(Reverse(cell)) = ready.pop() {
        order.push(cell);
        if let Some(deps) = reverse.get(&cell) {
            for &d in deps {
                if let Some(deg) = indegree.get_mut(&d) {
                    *deg -= 1;
                    if *deg == 0 {
                        ready.push(Reverse(d));
                    }
                }
            }
        }
    }

    let mut leftover: Vec<CellKey> = indegree
        .iter()
        .filter(|&(_, &deg)| deg > 0)
        .map(|(&cell, _)| cell)
        .collect();
    leftover.sort_unstable();
    order.extend(leftover);
    order
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turbo::calc::deps::{NameTarget, RefResolver};
    use crate::turbo::calc::{BinaryOp, CalcValue, CellRef, Expr, RefCore, RefExpr};
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

    #[test]
    fn prov_chain_precedents_and_dependents() {
        let mut g = DependencyGraph::new();
        let a = ck(0, 0);
        let b = ck(0, 1);
        let c = ck(0, 2);
        g.add_formula(a, &lit(1.0), &NullResolver).unwrap();
        g.add_formula(b, &cell(0, 0), &NullResolver).unwrap();
        g.add_formula(c, &cell(0, 1), &NullResolver).unwrap();
        let p = Provenance::new(&g);

        assert_eq!(p.precedents(a), Vec::<CellKey>::new());
        assert_eq!(p.precedents(b), vec![a]);
        assert_eq!(p.precedents(c), vec![b]);
        assert_eq!(p.dependents(a), vec![b]);
        assert_eq!(p.dependents(b), vec![c]);
        assert_eq!(p.dependents(c), Vec::<CellKey>::new());

        assert_eq!(p.precedents_deep(c), vec![a, b]);
        assert_eq!(p.dependents_deep(a), vec![b, c]);
    }

    #[test]
    fn prov_deep_chain_does_not_blow_stack() {
        let mut g = DependencyGraph::new();
        let n: u32 = 20_000;
        g.add_formula(ck(0, 0), &lit(1.0), &NullResolver).unwrap();
        for i in 1..n {
            g.add_formula(ck(i, 0), &cell(i - 1, 0), &NullResolver)
                .unwrap();
        }
        let p = Provenance::new(&g);
        let pres = p.precedents_deep(ck(n - 1, 0));
        assert_eq!(pres.len(), (n - 1) as usize);
        assert_eq!(pres[0], ck(0, 0));
        assert_eq!(pres[(n - 2) as usize], ck(n - 2, 0));
    }

    #[test]
    fn prov_cycle_terminates() {
        let mut g = DependencyGraph::new();
        let a = ck(0, 0);
        let b = ck(0, 1);
        g.add_formula(a, &cell(0, 1), &NullResolver).unwrap();
        g.add_formula(b, &cell(0, 0), &NullResolver).unwrap();
        let p = Provenance::new(&g);
        assert_eq!(p.precedents_deep(a), vec![a, b]);
        assert_eq!(p.dependents_deep(a), vec![a, b]);
    }

    #[test]
    fn prov_self_loop_terminates() {
        let mut g = DependencyGraph::new();
        let a = ck(0, 0);
        g.add_formula(a, &binary(cell(0, 0), lit(1.0)), &NullResolver)
            .unwrap();
        let p = Provenance::new(&g);
        assert_eq!(p.precedents_deep(a), vec![a]);
        assert_eq!(p.dependents_deep(a), vec![a]);
    }

    #[test]
    fn prov_impact_of_dedupes_overlapping_subtrees() {
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
        let p = Provenance::new(&g);

        let impact = p.impact_of(&[a1]);
        assert_eq!(impact, vec![b1, c1, d1]);
        let d_pos = impact.iter().position(|&x| x == d1).unwrap();
        let b_pos = impact.iter().position(|&x| x == b1).unwrap();
        let c_pos = impact.iter().position(|&x| x == c1).unwrap();
        assert!(b_pos < d_pos && c_pos < d_pos);

        assert_eq!(p.impact_of(&[b1, c1]), vec![d1]);
        assert_eq!(p.impact_of(&[d1]), Vec::<CellKey>::new());
    }

    #[test]
    fn prov_roots_find_inputs() {
        let mut g = DependencyGraph::new();
        let a1 = ck(0, 0);
        let b1 = ck(0, 1);
        let c1 = ck(0, 2);
        g.add_formula(a1, &lit(1.0), &NullResolver).unwrap();
        g.add_formula(b1, &cell(0, 0), &NullResolver).unwrap();
        g.add_formula(c1, &cell(0, 1), &NullResolver).unwrap();
        let p = Provenance::new(&g);
        assert_eq!(p.roots(), vec![a1]);

        let mut g2 = DependencyGraph::new();
        let x = ck(0, 0);
        let y = ck(0, 5);
        let z = ck(0, 9);
        g2.add_formula(x, &lit(1.0), &NullResolver).unwrap();
        g2.add_formula(y, &lit(2.0), &NullResolver).unwrap();
        g2.add_formula(z, &binary(cell(0, 0), cell(0, 5)), &NullResolver)
            .unwrap();
        let p2 = Provenance::new(&g2);
        assert_eq!(p2.roots(), vec![x, y]);
    }

    #[test]
    fn prov_is_leaf() {
        let mut g = DependencyGraph::new();
        let a = ck(0, 0);
        let b = ck(0, 1);
        let c = ck(0, 2);
        g.add_formula(a, &lit(1.0), &NullResolver).unwrap();
        g.add_formula(b, &cell(0, 0), &NullResolver).unwrap();
        g.add_formula(c, &cell(0, 1), &NullResolver).unwrap();
        let p = Provenance::new(&g);
        assert!(p.is_leaf(c));
        assert!(!p.is_leaf(a));
        assert!(!p.is_leaf(b));
        assert!(p.is_leaf(ck(99, 99)));
    }

    #[test]
    fn prov_unknown_cell_returns_empty() {
        let mut g = DependencyGraph::new();
        let a = ck(0, 0);
        g.add_formula(a, &lit(1.0), &NullResolver).unwrap();
        let p = Provenance::new(&g);
        let ghost = ck(7, 7);
        assert!(p.precedents(ghost).is_empty());
        assert!(p.dependents(ghost).is_empty());
        assert!(p.precedents_deep(ghost).is_empty());
        assert!(p.dependents_deep(ghost).is_empty());
        assert!(p.impact_of(&[ghost]).is_empty());
    }

    #[test]
    fn prov_deterministic_output() {
        fn build() -> DependencyGraph {
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
            g
        }
        let g1 = build();
        let g2 = build();
        let p1 = Provenance::new(&g1);
        let p2 = Provenance::new(&g2);
        assert_eq!(p1.roots(), p2.roots());
        assert_eq!(p1.impact_of(&[ck(0, 0)]), p2.impact_of(&[ck(0, 0)]));
        assert_eq!(p1.dependents_deep(ck(0, 0)), p2.dependents_deep(ck(0, 0)));
    }
}
