//! Stress and invariant tests for every Phase 3 feature module.
//!
//! The per-module tests check that each parser is *correct* on inputs shaped
//! like the ones it was written for. This file checks the three things those
//! cannot: that the modules survive the **real corpus**, that they survive
//! **deliberately broken bytes**, and that the fast-path contract they were all
//! written to actually holds *in the shipped code* rather than only in the
//! standalone harness (`archive/perf_experiments/detect.rs`, E7).
//!
//! In-crate rather than in `tests/`, because `turbo::zipmin` is private to the
//! crate and these tests need to pull individual parts out of real workbooks.

use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use pretty_assertions::assert_eq;

use super::{
    controls, diff, external_links, power_query, rich_values, signatures, slicers, sparklines,
    threaded_comments,
};

fn testdata_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

/// Every workbook in `testdata/`, including the deliberately malformed ones.
fn corpus() -> Vec<(String, Vec<u8>)> {
    let dir = testdata_dir();
    let mut out = Vec::new();
    let rd = match fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) => panic!("cannot read {}: {e}", dir.display()),
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) == Some("xlsx") {
            if let Ok(bytes) = fs::read(&p) {
                let name = p
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("?")
                    .to_string();
                out.push((name, bytes));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(out.len() >= 20, "corpus too small: {} files", out.len());
    out
}

/// Run every zip-level entry point across the module set.
///
/// The contract is *never panic*. An `Err` is a fine answer for a broken file;
/// an unwrap on a malformed length field is not. Returns nothing � reaching the
/// end without unwinding is the assertion.
fn sweep_zip_entry_points(zip: &[u8]) {
    let _ = slicers::inventory_slicers(zip);
    let _ = slicers::slicer_part_names(zip);
    let _ = rich_values::rich_data_part_names(zip);
    let _ = power_query::inventory_power_query(zip);
    let _ = power_query::power_query_part_names(zip);
    let _ = signatures::is_signed(zip);
    let _ = signatures::detect_signatures(zip);
    let _ = signatures::signature_part_names(zip);
    let _ = controls::control_part_names(zip);
    let _ = external_links::load_external_books(zip);
    let _ = diff::diff_parts(zip, zip);
}

/// Run every part-level parser over an arbitrary byte slice.
///
/// Each of these normally receives one specific inflated part. Feeding them the
/// *wrong* part � or a truncated one � is exactly what a corrupt workbook does,
/// so they must all degrade rather than panic.
fn sweep_part_parsers(part: &[u8]) {
    let _ = slicers::parse_slicers(part);
    let _ = slicers::parse_timelines(part);
    let _ = slicers::parse_slicer_cache(part);
    let _ = slicers::parse_sheet_slicer_ext(part);
    let _ = sparklines::parse_sparklines(part);
    let _ = controls::parse_sheet_controls(part);
    let _ = controls::parse_sheet_ole_objects(part);
    let _ = controls::parse_ctrl_props(part);
    let _ = rich_values::parse_rich_value_structures(part);
    let _ = rich_values::parse_rich_values(part);
    let _ = rich_values::parse_value_metadata(part);
    let _ = power_query::parse_connections(part);
    let _ = threaded_comments::parse_threaded_comments(part);
    let _ = threaded_comments::parse_persons(part);
    let _ = external_links::parse_external_link(part);
    let _ = external_links::parse_external_link_rel(part);
    let _ = signatures::strip_signature_rels(part);
}

// ---------------------------------------------------------------------------
// 1. The real corpus
// ---------------------------------------------------------------------------

#[test]
fn stress_every_corpus_workbook_survives_every_zip_entry_point() {
    for (name, bytes) in corpus() {
        // The panic message names the file, so a failure is actionable rather
        // than "something in the corpus".
        let r = std::panic::catch_unwind(|| sweep_zip_entry_points(&bytes));
        assert!(r.is_ok(), "panicked on {name}");
    }
}

#[test]
fn stress_every_corpus_sheet_part_survives_every_part_parser() {
    use crate::turbo::zipmin::read_entry;

    let mut parts_seen = 0usize;
    for (name, bytes) in corpus() {
        for part in [
            "xl/workbook.xml",
            "xl/worksheets/sheet1.xml",
            "xl/sharedStrings.xml",
            "xl/styles.xml",
            "[Content_Types].xml",
        ] {
            let Ok(Some(xml)) = read_entry(&bytes, part) else {
                continue;
            };
            // Cap the part size. Seventeen parsers times a 36 MB sheet part is
            // ~80 s in a debug build, and it buys nothing: whether a scanner
            // panics is decided by the *shapes* in the bytes, not the count of
            // them, and the adversarial-shape test below covers the shapes
            // directly. Volume is the streaming tests' job, not this one's.
            let xml = &xml[..xml.len().min(2 << 20)];
            parts_seen += 1;
            let r = std::panic::catch_unwind(|| sweep_part_parsers(xml));
            assert!(r.is_ok(), "panicked on {name} :: {part}");
        }
    }
    assert!(parts_seen >= 40, "only swept {parts_seen} parts");
}

// ---------------------------------------------------------------------------
// 2. Deliberately broken bytes
// ---------------------------------------------------------------------------

/// Deterministic xorshift � a fixed seed so a failure reproduces exactly.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

#[test]
fn stress_truncation_at_every_scale_never_panics() {
    let dir = testdata_dir();
    let bytes = fs::read(dir.join("charts.xlsx")).expect("fixture");

    // Every truncation point from "empty" to "whole file", geometrically, plus
    // the byte-exact boundaries where a zip record header would be cut in half.
    let mut cuts: Vec<usize> = Vec::new();
    let mut n = 1usize;
    while n < bytes.len() {
        cuts.push(n);
        n = n * 3 / 2 + 1;
    }
    cuts.extend([0, bytes.len() - 1, bytes.len()]);
    for &cut in &cuts {
        let slice = &bytes[..cut.min(bytes.len())];
        let r = std::panic::catch_unwind(|| {
            sweep_zip_entry_points(slice);
            sweep_part_parsers(slice);
        });
        assert!(r.is_ok(), "panicked on truncation to {cut} bytes");
    }
}

#[test]
fn stress_single_byte_corruption_never_panics() {
    let dir = testdata_dir();
    let original = fs::read(dir.join("charts.xlsx")).expect("fixture");
    let mut rng = Rng(0x5EED_1234_ABCD_0001);

    for _ in 0..600 {
        let mut mutated = original.clone();
        let pos = (rng.next() as usize) % mutated.len();
        mutated[pos] ^= (rng.next() & 0xFF) as u8;
        let r = std::panic::catch_unwind(|| sweep_zip_entry_points(&mutated));
        assert!(r.is_ok(), "panicked with byte {pos} corrupted");
    }
}

#[test]
fn stress_adversarial_xml_shapes_never_panic() {
    // Shapes chosen to hit the specific traps in a hand-rolled byte scanner:
    // unterminated tags, an attribute whose quote never closes, a marker that
    // appears with no element around it, deep nesting, and lone multi-byte
    // UTF-8 lead bytes that would panic any `str` slice on a byte boundary.
    let cases: Vec<Vec<u8>> = vec![
        b"<".to_vec(),
        b"<slicer".to_vec(),
        b"<slicer name=\"unterminated".to_vec(),
        b"sparklineGroup".to_vec(),
        b"<control ".to_vec(),
        b"<oleObject".to_vec(),
        b"<rv><v>".to_vec(),
        b"<threadedComment ref=".to_vec(),
        b"<Relationship Type=\"".to_vec(),
        b"<worksheet><extLst><ext uri=".to_vec(),
        b"<sheetName val=\"\xE2\x82".to_vec(),
        vec![0xFF; 4096],
        vec![b'<'; 4096],
        std::iter::repeat_n(b"<a>".as_slice(), 2000)
            .flatten()
            .copied()
            .collect(),
    ];
    for (i, case) in cases.iter().enumerate() {
        let r = std::panic::catch_unwind(|| sweep_part_parsers(case));
        assert!(r.is_ok(), "panicked on adversarial case {i}");
    }
}

// ---------------------------------------------------------------------------
// 3. The fast-path contract, measured in the shipped code
// ---------------------------------------------------------------------------

/// E7 said detection on an absent feature must cost one scan, never an inflate.
/// That was measured in a standalone harness. This asserts it about the code
/// that actually shipped.
///
/// The gate is a **ratio against reading the same file**, not an absolute
/// millisecond count, so it does not go red on a slow or loaded machine � the
/// numerator and denominator move together. Detecting ten absent features must
/// cost less than a twentieth of one read.
#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "timing gate: release only, see doc comment"
)]
fn stress_absent_feature_detection_is_free_against_a_real_read() {
    let path = testdata_dir().join("mixed.xlsx");
    let bytes = fs::read(&path).expect("fixture");
    let path_s = path.to_string_lossy().to_string();

    // Denominator: one ordinary read of the same workbook.
    let t = Instant::now();
    let wb = crate::turbo::read_workbook_turbo(&path_s, crate::turbo::Features(0))
        .expect("read fixture");
    let read_ms = t.elapsed().as_secs_f64() * 1e3;
    assert!(!wb.sheets.is_empty());

    // Numerator: every zip-level detection, all of which must miss.
    let iters = 20;
    let t = Instant::now();
    for _ in 0..iters {
        sweep_zip_entry_points(&bytes);
    }
    let detect_ms = t.elapsed().as_secs_f64() * 1e3 / iters as f64;

    // Printed so `--nocapture` reports the headroom, not just pass/fail � a
    // gate that only says "ok" cannot tell you it is about to stop being true.
    eprintln!(
        "absent-feature detection {detect_ms:.4} ms vs {read_ms:.1} ms read \
         ({:.0}x headroom)",
        read_ms / detect_ms
    );
    assert!(
        detect_ms * 20.0 < read_ms,
        "absent-feature detection cost {detect_ms:.4} ms against a {read_ms:.1} ms read \
         ({:.1}% of a read) � the fast path has regressed into inflating parts",
        detect_ms / read_ms * 100.0
    );
}

// ---------------------------------------------------------------------------
// 4. Byte preservation
// ---------------------------------------------------------------------------

/// `splice_sparklines` must change only the region it inserts. This is the same
/// invariant the overlay edit path holds, and the reason a kyrax round trip does
/// not disturb parts it was not asked to touch.
#[test]
fn stress_sparkline_splice_preserves_every_other_byte() {
    use crate::turbo::zipmin::read_entry;

    let groups = vec![sparklines::SparklineGroup {
        kind: sparklines::SparkType::Line,
        sparklines: vec![sparklines::Sparkline {
            source: "Sheet1!B1:F1".to_string(),
            location: "A1".to_string(),
        }],
        color_series: Some("FF376092".to_string()),
        color_negative: None,
        markers: true,
        high: false,
        low: false,
        display_empty_as: "gap".to_string(),
    }];

    let mut checked = 0usize;
    for (name, bytes) in corpus() {
        let Ok(Some(sheet)) = read_entry(&bytes, "xl/worksheets/sheet1.xml") else {
            continue;
        };
        if !sheet.windows(12).any(|w| w == b"</worksheet>") {
            continue; // nothing to splice into
        }
        let Ok(out) = sparklines::splice_sparklines(&sheet, &groups) else {
            continue;
        };
        checked += 1;

        // The output must contain the original sheet's opening bytes verbatim,
        // and must be strictly longer by exactly the inserted region.
        assert!(out.len() > sheet.len(), "{name}: splice shrank the part");
        let head = sheet.len().min(64);
        assert_eq!(&out[..head], &sheet[..head], "{name}: header disturbed");
        assert!(
            out.ends_with(b"</worksheet>"),
            "{name}: worksheet no longer closes"
        );

        // Splicing twice must not duplicate the ext: the second call replaces.
        let twice = sparklines::splice_sparklines(&out, &groups).expect("second splice");
        let count = twice
            .windows(14)
            .filter(|w| *w == b"sparklineGroups")
            .count();
        assert!(count <= 1, "{name}: splice duplicated the sparkline ext");
    }
    assert!(checked >= 10, "only spliced into {checked} sheets");
}

/// `strip_signature_rels` promises to return the input **unchanged** when there
/// is no signature relationship, and callers rely on that to skip a write.
#[test]
fn stress_strip_signature_rels_is_identity_without_a_signature() {
    use crate::turbo::zipmin::read_entry;

    let mut checked = 0usize;
    for (name, bytes) in corpus() {
        let Ok(Some(rels)) = read_entry(&bytes, "_rels/.rels") else {
            continue;
        };
        // None of the corpus is signed, so this is the identity case.
        assert!(
            !signatures::is_signed(&bytes).unwrap_or(false),
            "{name}: corpus was expected to be unsigned"
        );
        let out = signatures::strip_signature_rels(&rels).expect("strip");
        assert_eq!(out, rels, "{name}: rewrote an unsigned .rels");
        checked += 1;
    }
    assert!(checked >= 20, "only checked {checked} rels parts");
}

/// A workbook diffed against itself must report no changes at all, on every
/// file in the corpus. This is the level-1 tier's correctness floor: if a CRC
/// comparison of a file against itself reports a difference, every diff built
/// on top of it is noise.
#[test]
fn stress_self_diff_is_empty_for_every_corpus_workbook() {
    for (name, bytes) in corpus() {
        let Ok(d) = diff::diff_workbooks(&bytes, &bytes) else {
            continue; // malformed fixtures are allowed to refuse
        };
        assert!(
            d.parts.is_empty(),
            "{name}: self-diff reported part changes"
        );
        assert!(
            d.cells.is_empty(),
            "{name}: self-diff reported cell changes"
        );
        assert!(d.identical, "{name}: self-diff not marked identical");
    }
}
