//! Phase 3 — the Tier 3 MEDIUM and LOW capabilities from
//! `plans/capability_audit/ROADMAP.md`.
//!
//! Everything in here is ground **neither** kyrax nor openpyxl held before.
//! Each submodule owns exactly one OOXML feature area and follows the same
//! shape, which is what makes them cheap:
//!
//! * a parser over a single already-inflated part (`&[u8]` in, structs out),
//! * an inventory function over the whole zip that **lists entry names first**
//!   and only inflates the parts that actually exist, and
//! * a `*_part_names` function returning the entries the byte-preserving edit
//!   path must carry through untouched.
//!
//! The absent case is the common case for every feature here — most workbooks
//! have no slicers, no sparklines, no signatures. So the contract every
//! submodule honours is that **finding nothing costs one scan**: an entry-name
//! pass or a single `memmem` probe, never an inflate.
//!
//! That contract is not a guess. `archive/perf_experiments/detect.rs` (E7,
//! written up in `archive/PERF_EXPERIMENTS_PHASE3.md`) raced the three ways of answering "does
//! this workbook have feature X?" before any of this existed: inflating every
//! part costs 23.8 ms on a 2.8 MB workbook and 152.9 ms on a 36 MB one, while
//! the entry-name pass costs ~10 ns and is flat in file size. Ten feature
//! areas times every file in a fleet is what that saves.
//!
//! One exception to "preserve everything", in [`signatures`]: a digital
//! signature over content we just modified is worse than no signature, because
//! Excel reports the file as tampered. That module's part list is a *drop*
//! list, and says so.

pub mod controls;
pub mod diff;
pub mod external_links;
pub mod power_query;
pub mod provenance;
pub mod rich_values;
pub mod signatures;
pub mod slicers;
pub mod sparklines;
pub mod threaded_comments;

/// Cross-module stress: the real corpus, deliberately broken bytes, and the
/// fast-path contract asserted about the shipped code rather than a harness.
#[cfg(test)]
mod stress;

/// Proof that an overlay edit does not disturb the exotic parts above. The
/// claim "a workbook with slicers survives a kyrax round trip" was made before
/// it was tested; this is the test.
#[cfg(test)]
mod preserve_tests;

// Thin PyO3 marshalling only. All logic stays in the modules above — an engine
// capability nobody can call does not count as shipped, and a binding that
// contains logic is a second implementation waiting to drift.
#[cfg(feature = "python")]
pub mod python_inventory;
#[cfg(feature = "python")]
pub mod python_query;
#[cfg(feature = "python")]
pub mod python_sparkline;
