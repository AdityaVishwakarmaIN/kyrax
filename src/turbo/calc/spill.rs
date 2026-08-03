// calc/spill.rs — the spill region manager (dynamic-array support, part 1 of 3).
//
// A spilling formula (SEQUENCE, FILTER, SORT, XLOOKUP, ...) produces a RANGE of
// values from one anchor cell. The anchor holds the formula; the neighbouring
// cells are spilled *results*, not independent cells. This module owns the
// geometry of that relationship — which rectangle a spill would claim, who owns
// each spilled cell, and what `A1#` resolves to. It owns no values: the eval
// loop writes the spilled values into the grid; here lives only the map from
// spill rectangles to their anchors plus a per-cell ownership index that keeps
// `owner_of` a single hash lookup.
//
// # Sheet scoping
//
// Coordinates are 0-based `(row, col)` pairs and the interface carries no sheet
// id, so the caller keeps ONE `SpillMap` per sheet (in the eval/overlay layer),
// keyed by sheet id.
//
// # The model, in the four rules this module must get right
//
// 1. **`#SPILL!` never overwrites.** `probe` reports `Blocked` when the target
//    rectangle touches any non-empty cell not owned by the same anchor — a
//    foreign anchor's spill counts even if the grid value looks blank — and
//    `OffGrid` when it would cross row 1048576 or column XFD. Excel reports
//    rather than overwrites because overwriting user data is unrecoverable.
//    (Non-rectangular output never reaches here: ragged array literals are
//    padded at parse time and dynamic functions must return a rectangle — a
//    ragged result is an upstream contract violation, not a spill concern.)
// 2. **Recalculation clears the previous region first.** `commit` unclaims the
//    anchor's old rectangle before claiming the new one, so a spill that shrank
//    from 10 rows to 3 leaves 7 cells owned by nobody — and, after the eval
//    loop clears their grid values, actually empty instead of silently stale.
// 3. **Ownership is explicit.** A cell claimed by a DIFFERENT anchor blocks a
//    foreign probe even when the grid callback reports it empty; a cell claimed
//    by the SAME anchor is transparent to that anchor's own probe. The anchor
//    cell itself (the formula) is always transparent to its own spill. Modeled
//    directly in `probe`, not derived from the grid.
// 4. **`A1#` resolves through `region_of`.** `A1#` means "whatever the anchor
//    currently spills", so it reads the current region; when the region changes
//    the dependency edge (anchor -> reader) makes the reader recompute.
//
// # Performance
//
// `owner_of` runs per cell during evaluation. It is exactly one `HashMap`
// lookup — never a scan over regions. `commit` costs O(region area), which is
// fine because it runs once per spilling formula, not per cell (the P0 lesson:
// never rebuild a whole structure inside a per-cell loop).

use super::functions::{MAX_COLS, MAX_ROWS};
use std::collections::HashMap;

/// A rectangle owned by one anchor cell. Coordinates are 0-based `(row, col)`;
/// `rows`/`cols` count the anchor cell itself (a `1x1` region is a single-cell
/// spill, `0x0` denotes "no spill").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SpillRegion {
    pub anchor: (u32, u32),
    pub rows: u32,
    pub cols: u32,
}

impl SpillRegion {
    /// The region's cells in row-major order, starting at the anchor. Yields
    /// nothing for a zero-area region.
    pub fn cells(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        let (r0, c0) = self.anchor;
        (r0..r0 + self.rows).flat_map(move |r| (c0..c0 + self.cols).map(move |c| (r, c)))
    }
}

/// Result of a [`SpillMap::probe`] for a prospective spill.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpillOutcome {
    /// The rectangle is free; [`SpillMap::commit`] it.
    Ok(SpillRegion),
    /// A cell inside the rectangle is claimed by a different anchor or holds
    /// ordinary content. `by` is the first such cell in row-major order; the
    /// anchor cell evaluates to `#SPILL!`.
    Blocked { by: (u32, u32) },
    /// The rectangle extends past row 1048576 or column XFD.
    OffGrid,
}

/// Per-sheet registry of active spill regions.
///
/// Two structures, both keyed in O(1):
/// * `regions: anchor -> SpillRegion` — for `region_of` / `A1#` resolution.
/// * `owners: cell -> anchor` — for `owner_of`, the per-cell lookup the eval
///   loop performs during evaluation. Never a scan: exactly one hash lookup.
///
/// Invariant: no cell is owned by two anchors. `probe` is the gatekeeper (a
/// cell claimed by a different anchor blocks a foreign spill); `commit`
/// re-asserts the invariant with a `debug_assert` so a caller that bypasses
/// `probe` fails loudly in debug builds instead of silently clobbering.
#[derive(Debug, Default)]
pub struct SpillMap {
    regions: HashMap<(u32, u32), SpillRegion>,
    owners: HashMap<(u32, u32), (u32, u32)>,
}

impl SpillMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Can `anchor` spill a `rows x cols` rectangle right now?
    ///
    /// `occupied` reports ordinary grid content — `true` when the cell holds a
    /// value or formula. It need NOT know about spill cells: ownership is
    /// consulted separately (rule 3), so a cell claimed by another anchor
    /// blocks even when `occupied` says it is empty, and cells claimed by
    /// `anchor` itself — including the anchor's own formula cell — never
    /// block. The one caveat: the anchor's formula cell is transparent only
    /// while no foreign anchor claims it; if another anchor already owns that
    /// cell, even the anchor cannot spill over it. Grid bounds are checked
    /// before any cell. The rectangle is inherently rectangular (`rows x
    /// cols`); a zero-area probe is `Ok` (nothing to claim).
    pub fn probe(
        &self,
        occupied: &dyn Fn(u32, u32) -> bool,
        anchor: (u32, u32),
        rows: u32,
        cols: u32,
    ) -> SpillOutcome {
        if rows == 0 || cols == 0 {
            return SpillOutcome::Ok(SpillRegion { anchor, rows, cols });
        }
        let (r0, c0) = anchor;
        let r1 = r0 + rows;
        let c1 = c0 + cols;
        if r1 > MAX_ROWS || c1 > MAX_COLS as u32 {
            return SpillOutcome::OffGrid;
        }
        for r in r0..r1 {
            for c in c0..c1 {
                let cell = (r, c);
                match self.owners.get(&cell) {
                    Some(&own) if own == anchor => continue,
                    Some(_) => return SpillOutcome::Blocked { by: cell },
                    None if cell == anchor => continue,
                    None if occupied(r, c) => return SpillOutcome::Blocked { by: cell },
                    None => {}
                }
            }
        }
        SpillOutcome::Ok(SpillRegion { anchor, rows, cols })
    }

    /// Record a successful spill for `region.anchor`, clearing any previous
    /// region the anchor owned (rule 2).
    ///
    /// Unclaiming happens FIRST: the cells of the anchor's old rectangle that
    /// are not in the new one drop out of the ownership index, so a spill that
    /// shrank leaves its vacated cells owned by nobody (the eval loop clears
    /// their grid values; `owner_of` reports them free immediately).
    ///
    /// A zero-area region (`rows == 0 || cols == 0`) clears the anchor's
    /// previous region and records nothing — the correct outcome for a formula
    /// that now errors (`#SPILL!`) or spills to nothing.
    pub fn commit(&mut self, region: SpillRegion) {
        if let Some(prev) = self.regions.remove(&region.anchor) {
            for cell in prev.cells() {
                if self.owners.get(&cell) == Some(&region.anchor) {
                    self.owners.remove(&cell);
                }
            }
        }
        if region.rows == 0 || region.cols == 0 {
            return;
        }
        for cell in region.cells() {
            debug_assert!(
                self.owners
                    .get(&cell)
                    .is_none_or(|&own| own == region.anchor),
                "spill at {:?} would claim {cell:?}, already owned by {:?}",
                region.anchor,
                self.owners.get(&cell)
            );
            self.owners.insert(cell, region.anchor);
        }
        self.regions.insert(region.anchor, region);
    }

    /// Which anchor, if any, owns `cell` as a spill — the anchor cell or a
    /// spilled result. O(1): a single hash lookup, safe to call per cell
    /// during evaluation.
    pub fn owner_of(&self, cell: (u32, u32)) -> Option<(u32, u32)> {
        self.owners.get(&cell).copied()
    }

    /// The current region for `anchor` — what `A1#` resolves to (rule 4).
    /// `None` when the anchor spills nothing right now (never spilled, or its
    /// last commit was a zero-area clear).
    pub fn region_of(&self, anchor: (u32, u32)) -> Option<&SpillRegion> {
        self.regions.get(&anchor)
    }

    /// Every anchor whose current region intersects the inclusive rectangle
    /// `(row0, col0)..=(row1, col1)`.
    ///
    /// The dependency-graph hook: a formula cell that reads inside this rect
    /// must depend on each returned anchor, because the anchor's recalc can
    /// rewrite those cells. Linear in the number of active regions (tiny),
    /// never in grid area. Output is sorted for deterministic edges.
    pub fn anchors_overlapping(
        &self,
        row0: u32,
        col0: u32,
        row1: u32,
        col1: u32,
    ) -> Vec<(u32, u32)> {
        let mut out: Vec<(u32, u32)> = self
            .regions
            .values()
            .filter(|r| {
                let (ar, ac) = r.anchor;
                r.rows > 0
                    && r.cols > 0
                    && ar <= row1
                    && ar + r.rows > row0
                    && ac <= col1
                    && ac + r.cols > col0
            })
            .map(|r| r.anchor)
            .collect();
        out.sort_unstable();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty(_r: u32, _c: u32) -> bool {
        false
    }
    fn full(_r: u32, _c: u32) -> bool {
        true
    }
    fn region(anchor: (u32, u32), rows: u32, cols: u32) -> SpillRegion {
        SpillRegion { anchor, rows, cols }
    }

    #[test]
    fn basic_spill_commits_and_is_queryable() {
        let mut m = SpillMap::new();
        assert_eq!(m.owner_of((2, 3)), None);
        assert_eq!(m.region_of((2, 3)), None);
        m.commit(region((2, 3), 2, 3));
        // the anchor cell and every spilled result report their anchor
        assert_eq!(m.owner_of((2, 3)), Some((2, 3)));
        assert_eq!(m.owner_of((2, 5)), Some((2, 3)));
        assert_eq!(m.owner_of((3, 5)), Some((2, 3)));
        // outside the rectangle stays unowned
        assert_eq!(m.owner_of((1, 5)), None);
        assert_eq!(m.owner_of((4, 3)), None);
        assert_eq!(m.region_of((2, 3)), Some(&region((2, 3), 2, 3)));
        assert_eq!(m.region_of((9, 9)), None);
        // a free spill right next door is unblocked
        assert_eq!(
            m.probe(&empty, (0, 0), 1, 1),
            SpillOutcome::Ok(region((0, 0), 1, 1))
        );
        // a fresh single-cell spill is never blocked by its own formula cell
        assert_eq!(
            m.probe(&full, (10, 10), 1, 1),
            SpillOutcome::Ok(region((10, 10), 1, 1))
        );
    }

    #[test]
    fn blocked_rectangle_reports_the_blocking_cell() {
        // ordinary user content occupies (0,1)
        let m = SpillMap::new();
        let occupied = |r: u32, c: u32| r == 0 && c == 1;
        assert_eq!(
            m.probe(&occupied, (0, 0), 2, 2),
            SpillOutcome::Blocked { by: (0, 1) }
        );
        // a foreign anchor's region blocks even when the grid looks empty.
        // A=(5,5) spills 3x3 (rows 5..=7, cols 5..=7); B anchored at (4,6)
        // grows 2x2 into rows 5..=6, cols 6..=7, so its first step over A's
        // spilled cells is (5,6).
        let mut m2 = SpillMap::new();
        m2.commit(region((5, 5), 3, 3));
        assert_eq!(
            m2.probe(&empty, (4, 6), 2, 2),
            SpillOutcome::Blocked { by: (5, 6) }
        );
        // a foreign anchor whose own cell sits inside A's region is blocked
        // at its very first cell
        assert_eq!(
            m2.probe(&empty, (6, 5), 2, 2),
            SpillOutcome::Blocked { by: (6, 5) }
        );
    }

    #[test]
    fn off_grid_spill_is_rejected() {
        let m = SpillMap::new();
        // bottom row
        assert_eq!(
            m.probe(&empty, (MAX_ROWS - 1, 0), 2, 1),
            SpillOutcome::OffGrid
        );
        // rightmost column
        assert_eq!(
            m.probe(&empty, (0, MAX_COLS as u32 - 1), 1, 2),
            SpillOutcome::OffGrid
        );
        // anchor itself off the grid
        assert_eq!(m.probe(&empty, (MAX_ROWS, 0), 1, 1), SpillOutcome::OffGrid);
        // hugging the edges is fine
        assert!(matches!(
            m.probe(&empty, (MAX_ROWS - 1, MAX_COLS as u32 - 1), 1, 1),
            SpillOutcome::Ok(_)
        ));
    }

    #[test]
    fn shrinking_recommit_clears_vacated_cells() {
        let mut m = SpillMap::new();
        m.commit(region((0, 0), 3, 1));
        assert_eq!(m.owner_of((2, 0)), Some((0, 0)));
        m.commit(region((0, 0), 1, 1));
        assert_eq!(m.owner_of((0, 0)), Some((0, 0)));
        assert_eq!(m.owner_of((1, 0)), None);
        assert_eq!(m.owner_of((2, 0)), None);
        assert_eq!(m.region_of((0, 0)), Some(&region((0, 0), 1, 1)));
    }

    #[test]
    fn two_anchors_cannot_claim_the_same_cell() {
        let mut m = SpillMap::new();
        m.commit(region((0, 0), 2, 2));
        // a neighbouring anchor takes a free cell...
        assert_eq!(
            m.probe(&empty, (0, 2), 1, 1),
            SpillOutcome::Ok(region((0, 2), 1, 1))
        );
        m.commit(region((0, 2), 1, 1));
        assert_eq!(m.owner_of((0, 2)), Some((0, 2)));
        // ...but a foreign anchor cannot claim A's cells, even when the grid
        // callback reports them empty — ownership blocks, not just content.
        // B=(1,1) sits on A's spilled (1,1), so its probe dies at its anchor.
        assert_eq!(
            m.probe(&empty, (1, 1), 1, 1),
            SpillOutcome::Blocked { by: (1, 1) }
        );
        // A's own anchor cell belongs to A, so A itself is never blocked here
        assert_eq!(
            m.probe(&empty, (0, 0), 1, 1),
            SpillOutcome::Ok(region((0, 0), 1, 1))
        );
        // once A's spill is cleared, the cells are claimable again
        m.commit(region((0, 0), 0, 0));
        assert_eq!(m.owner_of((1, 0)), None);
        assert_eq!(
            m.probe(&empty, (1, 0), 1, 1),
            SpillOutcome::Ok(region((1, 0), 1, 1))
        );
    }

    #[test]
    fn anchor_recalculating_over_own_region_is_not_blocked_by_itself() {
        let mut m = SpillMap::new();
        m.commit(region((1, 1), 2, 2));
        // grid reports every cell occupied (stale spill + user data), but the
        // anchor's own cells are transparent to its own probe
        assert_eq!(
            m.probe(&full, (1, 1), 2, 2),
            SpillOutcome::Ok(region((1, 1), 2, 2))
        );
        // growth into genuinely new, occupied space is still blocked
        assert_eq!(
            m.probe(&full, (1, 1), 3, 2),
            SpillOutcome::Blocked { by: (3, 1) }
        );
    }

    #[test]
    fn region_of_tracks_a_changed_region() {
        let mut m = SpillMap::new();
        m.commit(region((0, 0), 2, 1));
        assert_eq!(m.region_of((0, 0)).map(|r| (r.rows, r.cols)), Some((2, 1)));
        m.commit(region((0, 0), 5, 1));
        assert_eq!(m.region_of((0, 0)).map(|r| (r.rows, r.cols)), Some((5, 1)));
        assert_eq!(m.owner_of((4, 0)), Some((0, 0)));
        // a zero-area commit clears the region entirely (formula now errors)
        m.commit(region((0, 0), 0, 0));
        assert_eq!(m.region_of((0, 0)), None);
        assert_eq!(m.owner_of((0, 0)), None);
    }

    #[test]
    fn anchors_overlapping_finds_precedents_for_dependency_wiring() {
        let mut m = SpillMap::new();
        m.commit(region((5, 5), 3, 3)); // rows 5..=7, cols 5..=7
        m.commit(region((20, 0), 1, 1));
        assert_eq!(m.anchors_overlapping(0, 0, 10, 10), vec![(5, 5)]);
        assert_eq!(m.anchors_overlapping(20, 0, 20, 0), vec![(20, 0)]);
        assert_eq!(m.anchors_overlapping(8, 5, 9, 9), Vec::<(u32, u32)>::new());
        assert_eq!(m.anchors_overlapping(5, 8, 5, 8), Vec::<(u32, u32)>::new());
    }
}
