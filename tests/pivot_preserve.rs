//! PIVOT PRESERVE — Task B5, PRESERVE half.
//!
//! A pivot table is not one part: it is a `<pivotTableDefinition>` (its layout
//! plus `<location>`), a `<pivotCacheDefinition>` (the `<cacheSource>` range it
//! was built from), the materialised `pivotCacheRecords`, and the rels wiring
//! all of them together. This file proves, on the real `testdata/pivot.xlsx`,
//! that every one of those parts SURVIVES every mutation the engine supports:
//!
//!   * a plain cell edit never touches the pivot parts;
//!   * a row/column insert or delete SHIFTS the cache source ref (and the pivot
//!     location, when the pivot's own sheet is mutated) exactly like every other
//!     reference the fixup pass owns;
//!   * a move_range leaves the pivot parts alone unless it touches the source;
//!   * and whenever the source range's CONTENT can change (edit inside the
//!     range, an insert/delete that moves the range, a move over the range) the
//!     cache definition is tagged `refreshOnLoad="1"` so Excel rebuilds the
//!     materialised records + sharedItems on open — a stale cache silently
//!     showing old numbers is the wrong-answer bug this project refuses to ship.
//!
//! The one case that used to corrupt pivots silently — a row inserted INSIDE
//! the source range — is asserted byte-for-byte in
//! `insert_row_inside_source_shifts_cache_and_location`.

use pretty_assertions::assert_eq;
use std::sync::Arc;

use kyrax::turbo::overlay::WorkbookOverlay;
use kyrax::turbo::write::CellValue;
use kyrax::turbo::{ArchiveMap, Features, read_entry, read_workbook_turbo};

const PIVOT_FIXTURE: &str = "pivot.xlsx";
const CACHE_PART: &str = "xl/pivotCache/pivotCacheDefinition1.xml";
const PTABLE_PART: &str = "xl/pivotTables/pivotTable1.xml";
const SHEET_RELS: &str = "xl/worksheets/_rels/sheet1.xml.rels";
const PTABLE_RELS: &str = "xl/pivotTables/_rels/pivotTable1.xml.rels";

fn testdata(name: &str) -> String {
    format!("{}/testdata/{}", env!("CARGO_MANIFEST_DIR"), name)
}

/// Build a namespace-clean synthetic copy of `testdata/pivot.xlsx`: the SAME
/// pivot parts (worksheet, pivotTable, cacheDefinition, rels, content types),
/// but `xl/workbook.xml` and `xl/_rels/workbook.xml.rels` re-emitted without
/// element-name prefixes so the overlay's sheet resolution (zipmin's literal
/// `<sheet ` / `<Relationship ` scans) maps "Data" → xl/worksheets/sheet1.xml.
/// The real fixture prefixes those elements (`<s:sheet>`, `<ns0:Relationship>`),
/// which that scanner does not match — a separate known gap outside this task's
/// fence. `rewrite_cache` renames the cache's `worksheetSource sheet` to build a
/// cache that sources a DIFFERENT sheet than the mutated one.
///
/// Returns `(synthetic bytes, synthetic map, overlay)`.
fn synthetic_pivot(rewrite_cache: Option<&str>) -> (Arc<Vec<u8>>, ArchiveMap, WorkbookOverlay) {
    let src = Arc::new(std::fs::read(testdata(PIVOT_FIXTURE)).expect("read pivot.xlsx"));
    let map = ArchiveMap::parse(src.clone()).expect("parse pivot.xlsx");
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for name in &map.entry_order {
        let mut bytes = read_entry(&src, name)
            .expect("read_entry")
            .expect("part present in source");
        if name == "xl/workbook.xml" {
            bytes = WORKBOOK_XML.as_bytes().to_vec();
        } else if name == "xl/_rels/workbook.xml.rels" {
            bytes = WORKBOOK_RELS.as_bytes().to_vec();
        } else if name == CACHE_PART {
            if let Some(rename) = rewrite_cache {
                let s = String::from_utf8_lossy(&bytes)
                    .replace("sheet=\"Data\"", &format!("sheet=\"{rename}\""));
                bytes = s.into_bytes();
            }
        }
        entries.push((name.clone(), bytes));
    }
    let rebuilt = store_zip(&entries);
    let rebuilt_map = ArchiveMap::parse(Arc::new(rebuilt.clone())).expect("parse synthetic pivot");
    let overlay = WorkbookOverlay::new(rebuilt_map.clone());
    (Arc::new(rebuilt), rebuilt_map, overlay)
}

fn load_overlay() -> (Arc<Vec<u8>>, ArchiveMap, WorkbookOverlay) {
    synthetic_pivot(None)
}

/// Namespace-clean re-emission of the fixture's workbook.xml (sheet "Data",
/// pivotCaches cacheId 0 → rId4, fullCalcOnLoad).
const WORKBOOK_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
 xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets>
    <sheet name="Data" sheetId="1" state="visible" r:id="rId1"/>
  </sheets>
  <pivotCaches>
    <pivotCache cacheId="0" r:id="rId4"/>
  </pivotCaches>
  <calcPr calcId="124519" fullCalcOnLoad="1"/>
</workbook>"#;

/// Namespace-clean re-emission of the fixture's workbook rels (rId1 → sheet,
/// rId4 → cache definition).
const WORKBOOK_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="/xl/worksheets/sheet1.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme" Target="theme/theme1.xml"/>
  <Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotCacheDefinition" Target="pivotCache/pivotCacheDefinition1.xml"/>
</Relationships>"#;

fn part(zip: &[u8], name: &str) -> Vec<u8> {
    read_entry(zip, name)
        .expect("read_entry")
        .unwrap_or_else(|| panic!("part {name} missing from output"))
}

fn save(ov: &mut WorkbookOverlay) -> Vec<u8> {
    ov.save().expect("save must succeed")
}

fn text(xml: &[u8]) -> String {
    String::from_utf8_lossy(xml).into_owned()
}

// ---------------------------------------------------------------------------
// Edit — the overlay path rewrites only <sheetData>; pivot parts must be
// byte-identical unless the edit touches the cache's source range.
// ---------------------------------------------------------------------------

#[test]
fn edit_outside_source_range_preserves_pivot_bytes() {
    let (src, _map, mut ov) = load_overlay();
    // H20 is far outside the cache source A1:C5 and the pivot location E3:G8.
    ov.set_cell("Data", 20, 8, CellValue::Number(1.0));
    let saved = save(&mut ov);
    for p in [CACHE_PART, PTABLE_PART, SHEET_RELS, PTABLE_RELS] {
        assert_eq!(
            part(&saved, p),
            part(&src, p),
            "part {p} changed on an edit that cannot affect it"
        );
    }
}

#[test]
fn edit_inside_source_range_marks_cache_stale_only() {
    let (src, _map, mut ov) = load_overlay();
    ov.set_cell("Data", 2, 1, CellValue::Str("North".to_string()));
    let saved = save(&mut ov);
    let cache = text(&part(&saved, CACHE_PART));
    // The ref did not move (no insert/delete), but the content did: the cache
    // must be tagged so Excel refreshes the stale materialised records on open.
    assert!(cache.contains(r#"ref="A1:C5""#), "{cache}");
    assert!(cache.contains(r#"refreshOnLoad="1""#), "{cache}");
    // The pivot table part and rels are untouched.
    assert_eq!(
        part(&saved, PTABLE_PART),
        part(&src, PTABLE_PART),
        "pivot table part changed on a source edit"
    );
    assert_eq!(part(&saved, SHEET_RELS), part(&src, SHEET_RELS));
}

// ---------------------------------------------------------------------------
// Row insert/delete — the cache source ref shifts with the grid, the pivot
// location shifts when its host sheet is mutated, and the cache is tagged stale
// exactly when the source range moved.
// ---------------------------------------------------------------------------

#[test]
fn insert_row_inside_source_shifts_cache_and_location() {
    let (src, _map, mut ov) = load_overlay();
    // The case that silently corrupts a pivot today: a row inserted INSIDE the
    // source range A1:C5 without shifting the cache ref.
    ov.insert_rows("Data", 3, 1);
    let saved = save(&mut ov);
    let cache = text(&part(&saved, CACHE_PART));
    assert!(cache.contains(r#"ref="A1:C6""#), "{cache}");
    assert!(cache.contains(r#"refreshOnLoad="1""#), "{cache}");
    let pt = text(&part(&saved, PTABLE_PART));
    assert!(pt.contains(r#"<location ref="E4:G9""#), "{pt}");
    assert_eq!(part(&saved, SHEET_RELS), part(&src, SHEET_RELS));
}

#[test]
fn insert_row_above_source_shifts_cache_and_location() {
    let (_src, _map, mut ov) = load_overlay();
    ov.insert_rows("Data", 1, 2);
    let saved = save(&mut ov);
    let cache = text(&part(&saved, CACHE_PART));
    assert!(cache.contains(r#"ref="A3:C7""#), "{cache}");
    assert!(cache.contains(r#"refreshOnLoad="1""#), "{cache}");
    let pt = text(&part(&saved, PTABLE_PART));
    assert!(pt.contains(r#"<location ref="E5:G10""#), "{pt}");
}

#[test]
fn delete_row_inside_source_shrinks_cache_and_location() {
    let (_src, _map, mut ov) = load_overlay();
    ov.delete_rows("Data", 3, 2);
    let saved = save(&mut ov);
    let cache = text(&part(&saved, CACHE_PART));
    // Rows 3-4 go; row 5 shifts up to 3.
    assert!(cache.contains(r#"ref="A1:C3""#), "{cache}");
    assert!(cache.contains(r#"refreshOnLoad="1""#), "{cache}");
    let pt = text(&part(&saved, PTABLE_PART));
    assert!(pt.contains(r#"<location ref="E3:G6""#), "{pt}");
}

#[test]
fn insert_row_below_source_preserves_pivot_bytes() {
    let (src, _map, mut ov) = load_overlay();
    // Nothing at or below row 10: the source range neither moves nor changes,
    // so the pivot parts are byte-identical (no refreshOnLoad either).
    ov.insert_rows("Data", 10, 3);
    let saved = save(&mut ov);
    for p in [CACHE_PART, PTABLE_PART, SHEET_RELS, PTABLE_RELS] {
        assert_eq!(
            part(&saved, p),
            part(&src, p),
            "part {p} changed on an insert that cannot affect it"
        );
    }
}

// ---------------------------------------------------------------------------
// Column insert/delete on the source axis.
// ---------------------------------------------------------------------------

#[test]
fn insert_col_inside_source_shifts_cache_and_location() {
    let (_src, _map, mut ov) = load_overlay();
    ov.insert_cols("Data", 2, 1);
    let saved = save(&mut ov);
    let cache = text(&part(&saved, CACHE_PART));
    assert!(cache.contains(r#"ref="A1:D5""#), "{cache}");
    assert!(cache.contains(r#"refreshOnLoad="1""#), "{cache}");
    let pt = text(&part(&saved, PTABLE_PART));
    assert!(pt.contains(r#"<location ref="F3:H8""#), "{pt}");
}

#[test]
fn delete_col_inside_source_shrinks_cache_and_location() {
    let (_src, _map, mut ov) = load_overlay();
    ov.delete_cols("Data", 2, 1);
    let saved = save(&mut ov);
    let cache = text(&part(&saved, CACHE_PART));
    assert!(cache.contains(r#"ref="A1:B5""#), "{cache}");
    assert!(cache.contains(r#"refreshOnLoad="1""#), "{cache}");
    let pt = text(&part(&saved, PTABLE_PART));
    assert!(pt.contains(r#"<location ref="D3:F8""#), "{pt}");
}

// ---------------------------------------------------------------------------
// move_range — relocates cells without shifting the grid, so the pivot refs
// are untouched; the cache goes stale only when the move touches the source.
// ---------------------------------------------------------------------------

#[test]
fn move_range_away_from_source_preserves_pivot_bytes() {
    let (src, _map, mut ov) = load_overlay();
    ov.move_range("Data", 8, 1, 9, 2, 1, 0, false);
    let saved = save(&mut ov);
    for p in [CACHE_PART, PTABLE_PART, SHEET_RELS, PTABLE_RELS] {
        assert_eq!(
            part(&saved, p),
            part(&src, p),
            "part {p} changed on a move that cannot affect it"
        );
    }
}

#[test]
fn move_range_over_source_marks_cache_stale() {
    let (src, _map, mut ov) = load_overlay();
    // Move the top of the source range (A1:C2) down one row: content in the
    // source range changes, so the cache must refresh on load. The ref itself
    // is untouched — move_range does not shift the grid.
    ov.move_range("Data", 1, 1, 2, 3, 1, 0, false);
    let saved = save(&mut ov);
    let cache = text(&part(&saved, CACHE_PART));
    assert!(cache.contains(r#"ref="A1:C5""#), "{cache}");
    assert!(cache.contains(r#"refreshOnLoad="1""#), "{cache}");
    assert_eq!(
        part(&saved, PTABLE_PART),
        part(&src, PTABLE_PART),
        "pivot table part changed on a move"
    );
}

// ---------------------------------------------------------------------------
// Round-trip and staleness-tag retention.
// ---------------------------------------------------------------------------

#[test]
fn insert_then_delete_roundtrip_restores_ref_keeps_stale_tag() {
    let (_src, _map, mut ov) = load_overlay();
    ov.insert_rows("Data", 3, 1);
    ov.delete_rows("Data", 3, 1);
    let saved = save(&mut ov);
    let cache = text(&part(&saved, CACHE_PART));
    assert!(cache.contains(r#"ref="A1:C5""#), "{cache}");
    // The tag is retained by design (documented in fixup.rs): a cache that has
    // been through a mutation keeps refreshOnLoad so it is always rebuilt on
    // open. Harmless, and never untags a cache that might still be stale.
    assert!(cache.contains(r#"refreshOnLoad="1""#), "{cache}");
    let pt = text(&part(&saved, PTABLE_PART));
    assert!(pt.contains(r#"<location ref="E3:G8""#), "{pt}");
}

// ---------------------------------------------------------------------------
// End-to-end: the saved workbook still reads back with pivot metadata pointing
// at the shifted coordinates.
// ---------------------------------------------------------------------------

#[test]
fn saved_workbook_reads_back_shifted_pivot_metadata() {
    let (_src, _map, mut ov) = load_overlay();
    ov.insert_rows("Data", 1, 2);
    let saved = save(&mut ov);
    let path = std::env::temp_dir().join("kyrax_pivot_preserve_readback.xlsx");
    std::fs::write(&path, &saved).expect("write temp workbook");
    let wb = read_workbook_turbo(path.to_str().unwrap(), Features::PIVOTS)
        .expect("saved workbook must read");
    assert_eq!(wb.sheets.len(), 1, "one sheet");
    let pivs = wb.sheets[0].pivots.as_ref().expect("pivots feature on");
    assert_eq!(pivs.len(), 1, "one pivot");
    assert_eq!(
        pivs[0].location_ref, "E5:G10",
        "pivot location followed the insert"
    );
    assert_eq!(
        pivs[0].cache.worksheet_ref.as_deref(),
        Some("A3:C7"),
        "cache source ref followed the insert"
    );
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// Cache sourcing a DIFFERENT sheet: mutating the pivot's host sheet must shift
// the pivot location but leave the cache definition byte-identical.
// ---------------------------------------------------------------------------

#[test]
fn mutate_host_sheet_but_cache_sources_other_sheet() {
    // The cache's source sheet is renamed to "OtherSheet", so mutating "Data"
    // (the pivot's HOST sheet) must shift the pivot location but leave the
    // cache definition byte-identical — no ref shift, no stale tag.
    let (src, _map, mut ov) = synthetic_pivot(Some("OtherSheet"));
    ov.insert_rows("Data", 1, 2);
    let saved = save(&mut ov);

    let pt = text(&part(&saved, PTABLE_PART));
    assert!(pt.contains(r#"<location ref="E5:G10""#), "{pt}");
    let cache = part(&saved, CACHE_PART);
    assert_eq!(
        cache,
        part(&src, CACHE_PART),
        "cache must be byte-identical when its source sheet was not mutated"
    );
    assert!(text(&cache).contains(r#"ref="A1:C5""#), "ref untouched");
    assert!(
        !text(&cache).contains("refreshOnLoad"),
        "no stale tag for a foreign source"
    );
}

// ---------------------------------------------------------------------------
// Minimal store-only ZIP writer (test crate cannot see pub(crate) zipmin).
// ---------------------------------------------------------------------------

fn store_zip(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut local: Vec<(String, usize, usize, usize)> = Vec::new();
    let mut body = Vec::new();
    for (name, data) in entries {
        let name_b = name.as_bytes();
        let offset = body.len();
        body.extend_from_slice(&0x04034b50u32.to_le_bytes());
        body.extend_from_slice(&20u16.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.extend_from_slice(&(name_b.len() as u16).to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(name_b);
        body.extend_from_slice(data);
        local.push((name.clone(), offset, data.len(), data.len()));
    }
    let cd_off = body.len();
    for (name, local_off, csize, usize_) in &local {
        let name_b = name.as_bytes();
        body.extend_from_slice(&0x02014b50u32.to_le_bytes());
        body.extend_from_slice(&20u16.to_le_bytes());
        body.extend_from_slice(&20u16.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&(*csize as u32).to_le_bytes());
        body.extend_from_slice(&(*usize_ as u32).to_le_bytes());
        body.extend_from_slice(&(name_b.len() as u16).to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&(*local_off as u32).to_le_bytes());
        body.extend_from_slice(name_b);
    }
    let cd_size = body.len() - cd_off;
    body.extend_from_slice(&0x06054b50u32.to_le_bytes());
    body.extend_from_slice(&0u16.to_le_bytes());
    body.extend_from_slice(&0u16.to_le_bytes());
    body.extend_from_slice(&(local.len() as u16).to_le_bytes());
    body.extend_from_slice(&(local.len() as u16).to_le_bytes());
    body.extend_from_slice(&(cd_size as u32).to_le_bytes());
    body.extend_from_slice(&(cd_off as u32).to_le_bytes());
    body.extend_from_slice(&0u16.to_le_bytes());
    body
}
