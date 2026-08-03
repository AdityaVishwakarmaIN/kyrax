//! End-to-end proof of the round-trip preservation claim.
//!
//! The public claim: "a workbook with slicers, rich values, Power Query or
//! embedded controls now survives a kyrax round trip instead of losing them —
//! which is the openpyxl data-loss bug closed." This module is where that claim
//! is actually tested rather than assumed.
//!
//! The mechanism under test is `overlay.rs` Phase 2 (`save()`): every part NOT
//! in the modified set is copied verbatim from the `ArchiveMap` by slicing the
//! original compressed payload and re-adding it with its original method, CRC
//! and sizes. If that is true, exotic parts — slicers, rich values, Power
//! Query, controls, external links, a binary OLE object — survive a real edit
//! byte-identically. If any part is dropped, reordered into loss, re-compressed
//! differently, or silently rewritten, the byte-equality assertions below fail.
//! A failing test here is a SUCCESS: it reports the claim as false. The tests
//! must never be weakened to pass around a real defect.
//!
//! WIRING: add `#[cfg(test)] mod preserve_tests;` to src/turbo/features/mod.rs.

use std::sync::Arc;

use crate::turbo::features::external_links::{ExternalBook, load_external_books};
use crate::turbo::features::power_query::inventory_power_query;
use crate::turbo::features::slicers::inventory_slicers;
use crate::turbo::overlay::WorkbookOverlay;
use crate::turbo::write::model::CellValue;
use crate::turbo::zipmin::{ArchiveMap, read_entry};

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

const OLE_BIN: &[u8] =
    b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1\x00OLE2\x00\xff\xfe\x80kyrax-ole-object\x00\xff\xff\x80\x00";

const CONTENT_TYPES_XML: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#;

const ROOT_RELS_XML: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;

const WORKBOOK_XML: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>"#;

const WORKBOOK_RELS_XML: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#;

const SHEET1_XML: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><dimension ref="A1:A2"/><sheetData><row r="1"><c r="A1"><v>10</v></c></row><row r="2"><c r="A2"><v>20</v></c></row></sheetData></worksheet>"#;

/// The exotic parts under test. Every one must round-trip byte-identically.
/// `oleObject1.bin` is binary (0x00, 0xFF, lone 0x80) and must never be
/// treated as text or XML.
const EXOTIC_ENTRIES: &[(&str, &[u8])] = &[
    (
        "xl/slicers/slicer1.xml",
        br#"<slicers xmlns="http://schemas.microsoft.com/office/spreadsheetml/2009/9/main"><slicer name="Slicer_Product" cache="SlicerCache_Product" caption="Product Line" columnCount="2"/></slicers>"#,
    ),
    (
        "xl/slicerCaches/slicerCache1.xml",
        br#"<slicerCacheDefinition xmlns="http://schemas.microsoft.com/office/spreadsheetml/2009/9/main" name="SlicerCache_Product" sourceName="Table1" filterColumn="0"><pivotTables><pivotTable tabId="0" name="PivotTable1"/></pivotTables><slicerCacheData><slicerCacheColumn count="1"><slicerCacheItem uniqueName="[Table1].Product" xDynamic="0"/></slicerCacheColumn></slicerCacheData></slicerCacheDefinition>"#,
    ),
    (
        "xl/richData/rdrichvalue.xml",
        br#"<richValueRanges xmlns="http://schemas.microsoft.com/office/spreadsheetml/2017/richData2"><richValueStructure key="dataType" keyRefs="dataTypeValue"><richValueType value="M"><m property="product" values="value" /></richValueType></richValueStructure></richValueRanges>"#,
    ),
    (
        "xl/metadata.xml",
        br#"<metadata xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:xlrd="http://schemas.microsoft.com/office/spreadsheetml/2017/richData"><metadataTypes count="1"><metadataType name="XLRICHVALUE" minSupportedVersion="120000" copy="1" pasteAll="1" pasteValues="1" merge="1" splitFirst="1" rowColShift="1" clearFormats="1" clearComments="1" assign="1" coerce="1" cellMeta="1"/></metadataTypes><futureMetadata name="XLRICHVALUE" count="1"><bk><extLst><ext uri="{3e2802c4-a4d2-4d8b-9148-e3be6c30e623}"><xlrd:richValueMetadata /></ext></extLst></bk></futureMetadata></metadata>"#,
    ),
    (
        "customXml/item1.xml",
        br#"<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties"><cp:title>Mashup</cp:title><DataMashup>UEsDBBQAAAAAAJ9QX0wAAAAAAAAAAAAAAAAaAAAAaHR0cHM6Ly93d3cuZGF0YXNldHMuZm9v</DataMashup></cp:coreProperties>"#,
    ),
    (
        "xl/connections.xml",
        br#"<connections xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><connection id="1" name="Query - Sales" type="5"><dbPr connection="Provider=Microsoft.Mashup.OleDb.1;Data Source=$workbook$;Location=Sales;Extended Properties=&quot;&quot;" command="SELECT * FROM [Sales]"/></connection></connections>"#,
    ),
    (
        "xl/ctrlProps/ctrlProp1.xml",
        br#"<ctrlProp xmlns="http://schemas.microsoft.com/office/spreadsheetml/2009/9/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" shapeId="1025" name="Check Box 1" locked="0" defaultSize="0" print="0" disabled="1" r:id="rId1"><formControlPr xmlns="http://schemas.microsoft.com/office/spreadsheetml/2009/9/main" objectType="CheckBox"/></ctrlProp>"#,
    ),
    ("xl/embeddings/oleObject1.bin", OLE_BIN),
    (
        "xl/externalLinks/externalLink1.xml",
        br#"<externalLink xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><externalBook r:id="rId1"><sheetNames><sheetName val="Sheet1"/><sheetName val="2024"/></sheetNames><definedNames><definedName name="Foo" refersTo="Sheet1!$A$1"/></definedNames><sheetDataSet><sheetData sheetId="0"><row r="1"><cell r="A1" t="str"><v>cached</v></cell></row></sheetData><sheetData sheetId="1"><row r="1"><cell r="B1" t="str"><v>second sheet</v></cell></row></sheetData></sheetDataSet></externalBook></externalLink>"#,
    ),
    (
        "xl/externalLinks/_rels/externalLink1.xml.rels",
        br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/externalLinkPath" Target="../../book2.xlsx" TargetMode="External"/></Relationships>"#,
    ),
];

fn w_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn w_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Build a STORE-method zip with the given `(name, payload)` entries. CRC is
/// not computed; this module never verifies it, so a zero is fine.
///
/// Field order here is load-bearing: the central-directory external attributes
/// are the last field before the local-header offset, so a stray u16 shifts
/// the offset by two bytes and every later record points at garbage. Reused
/// from `features/slicers.rs` tests exactly.
fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut cd = Vec::new();
    for (name, payload) in entries {
        let name_b = name.as_bytes();
        let lh_pos = out.len() as u32;
        out.extend_from_slice(b"PK\x03\x04");
        w_u16(&mut out, 20);
        w_u16(&mut out, 0);
        w_u16(&mut out, 0);
        w_u16(&mut out, 0);
        w_u16(&mut out, 0);
        w_u32(&mut out, 0);
        w_u32(&mut out, payload.len() as u32);
        w_u32(&mut out, payload.len() as u32);
        w_u16(&mut out, name_b.len() as u16);
        w_u16(&mut out, 0);
        out.extend_from_slice(name_b);
        out.extend_from_slice(payload);

        cd.extend_from_slice(b"PK\x01\x02");
        w_u16(&mut cd, 20);
        w_u16(&mut cd, 20);
        w_u16(&mut cd, 0);
        w_u16(&mut cd, 0);
        w_u16(&mut cd, 0);
        w_u16(&mut cd, 0);
        w_u32(&mut cd, 0);
        w_u32(&mut cd, payload.len() as u32);
        w_u32(&mut cd, payload.len() as u32);
        w_u16(&mut cd, name_b.len() as u16);
        w_u16(&mut cd, 0);
        w_u16(&mut cd, 0);
        w_u16(&mut cd, 0);
        w_u16(&mut cd, 0);
        w_u32(&mut cd, 0);
        w_u32(&mut cd, lh_pos);
        cd.extend_from_slice(name_b);
    }
    let cd_offset = out.len() as u32;
    let cd_size = cd.len() as u32;
    out.extend_from_slice(&cd);
    out.extend_from_slice(b"PK\x05\x06");
    w_u16(&mut out, 0);
    w_u16(&mut out, 0);
    w_u16(&mut out, entries.len() as u16);
    w_u16(&mut out, entries.len() as u16);
    w_u32(&mut out, cd_size);
    w_u32(&mut out, cd_offset);
    w_u16(&mut out, 0);
    out
}

/// A minimum viable workbook with a few real rows PLUS every exotic part under
/// test. The sheet is named "Sheet1" so the overlay can address it by name.
fn build_exotic_workbook() -> Vec<u8> {
    let mut entries: Vec<(&str, &[u8])> = vec![
        ("[Content_Types].xml", CONTENT_TYPES_XML),
        ("_rels/.rels", ROOT_RELS_XML),
        ("xl/workbook.xml", WORKBOOK_XML),
        ("xl/_rels/workbook.xml.rels", WORKBOOK_RELS_XML),
        ("xl/worksheets/sheet1.xml", SHEET1_XML),
    ];
    entries.extend_from_slice(EXOTIC_ENTRIES);
    build_zip(&entries)
}

// ---------------------------------------------------------------------------
// Round-trip harness
// ---------------------------------------------------------------------------

/// Parse `source`, apply `edit` through a `WorkbookOverlay`, and save.
fn edit_save(source: &[u8], mut edit: impl FnMut(&mut WorkbookOverlay)) -> Vec<u8> {
    let map = ArchiveMap::parse(Arc::new(source.to_vec())).expect("fixture must parse");
    let mut ov = WorkbookOverlay::new(map);
    edit(&mut ov);
    ov.save().expect("overlay save must succeed")
}

/// Inflate one part from a saved workbook, panicking with the part name if it
/// is gone or unreadable.
fn read_part(zip: &[u8], name: &str) -> Vec<u8> {
    read_entry(zip, name)
        .unwrap_or_else(|e| panic!("part '{name}' could not be read back: {e}"))
        .unwrap_or_else(|| panic!("part '{name}' is missing from the saved workbook"))
}

/// `load_external_books` returns structs without `PartialEq`, so the
/// equivalence is asserted field by field at the level a user experiences it.
fn external_books_eq(a: &[ExternalBook], b: &[ExternalBook]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).all(|(x, y)| {
        x.index == y.index
            && x.target == y.target
            && x.sheet_names == y.sheet_names
            && x.defined_names == y.defined_names
            && x.cached.len() == y.cached.len()
            && x.cached.iter().zip(y.cached.iter()).all(|(c1, c2)| {
                c1.sheet_id == c2.sheet_id
                    && c1.cell == c2.cell
                    && c1.value == c2.value
                    && c1.kind == c2.kind
            })
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The claim's core: every exotic part must survive a REAL edit with its
/// inflated bytes EXACTLY equal to what went in. Two real edits are tried
/// separately: a cell edit and a row insert.
#[test]
fn preserve_exotic_parts_survive_an_overlay_edit() {
    let source = build_exotic_workbook();

    let saved = edit_save(&source, |ov| {
        ov.set_cell("Sheet1", 1, 1, CellValue::Str("edited".into()));
    });
    let sheet = read_part(&saved, "xl/worksheets/sheet1.xml");
    assert!(
        memchr::memmem::find(&sheet, b"edited").is_some(),
        "the set_cell edit must actually land, or this is not a real edit"
    );
    for (name, payload) in EXOTIC_ENTRIES {
        let got = read_part(&saved, name);
        assert_eq!(
            got, *payload,
            "part '{name}' was not preserved byte-identically after a set_cell edit"
        );
    }

    let saved = edit_save(&source, |ov| {
        ov.insert_rows("Sheet1", 2, 1);
    });
    let sheet = read_part(&saved, "xl/worksheets/sheet1.xml");
    assert!(
        memchr::memmem::find(&sheet, b"<row r=\"3\">").is_some(),
        "the insert_rows edit must actually shift the grid, or this is not a real edit"
    );
    for (name, payload) in EXOTIC_ENTRIES {
        let got = read_part(&saved, name);
        assert_eq!(
            got, *payload,
            "part '{name}' was not preserved byte-identically after an insert_rows edit"
        );
    }
}

/// The binary OLE object must round-trip byte-for-byte, including the bytes
/// that break every UTF-8 or XML assumption: 0x00, 0xFF and a lone 0x80.
/// Treating this part as text would corrupt it; the equality check is on raw
/// bytes, not on "contains".
#[test]
fn preserve_binary_part_is_not_corrupted() {
    let source = build_exotic_workbook();
    let saved = edit_save(&source, |ov| {
        ov.set_cell("Sheet1", 2, 2, CellValue::Number(99.0));
    });

    let got = read_part(&saved, "xl/embeddings/oleObject1.bin");
    assert_eq!(
        got, OLE_BIN,
        "oleObject1.bin must round-trip exactly (0x00/0xFF/0x80 hostile bytes intact)"
    );
    assert_eq!(got.len(), OLE_BIN.len());
    assert!(got.contains(&0x00), "0x00 must survive");
    assert!(got.contains(&0xFF), "0xFF must survive");
    assert!(got.contains(&0x80), "lone 0x80 must survive");
}

/// A save must never shrink the package: every source entry must still be
/// present, and the saved count must be at least the source count. Missing
/// parts are named in the failure message.
#[test]
fn preserve_part_count_does_not_shrink() {
    let source = build_exotic_workbook();
    let saved = edit_save(&source, |ov| {
        ov.set_cell("Sheet1", 1, 1, CellValue::Str("edited".into()));
    });

    let src_map = ArchiveMap::parse(Arc::new(source)).expect("source must parse");
    let saved_map = ArchiveMap::parse(Arc::new(saved)).expect("saved must parse");

    let missing: Vec<&String> = src_map
        .entry_order
        .iter()
        .filter(|n| !saved_map.entries.contains_key(*n))
        .collect();
    assert!(
        missing.is_empty(),
        "parts dropped by the round trip: {missing:?}"
    );
    assert!(
        saved_map.entry_order.len() >= src_map.entry_order.len(),
        "saved has {} entries, source had {}",
        saved_map.entry_order.len(),
        src_map.entry_order.len()
    );
}

/// The feature inventories must agree before and after the round trip. This
/// tests the claim at the level a user experiences it — not at the level of
/// zip mechanics.
#[test]
fn preserve_feature_inventories_agree_before_and_after() {
    let source = build_exotic_workbook();
    let saved = edit_save(&source, |ov| {
        ov.set_cell("Sheet1", 1, 1, CellValue::Str("edited".into()));
    });

    let src_slicers = inventory_slicers(&source).expect("slicer inventory must parse");
    let saved_slicers = inventory_slicers(&saved).expect("saved slicer inventory must parse");
    assert_eq!(
        src_slicers, saved_slicers,
        "slicer inventory changed across the round trip"
    );

    let src_pq = inventory_power_query(&source).expect("power query inventory must parse");
    let saved_pq = inventory_power_query(&saved).expect("saved power query inventory must parse");
    assert_eq!(
        src_pq, saved_pq,
        "power query inventory changed across the round trip"
    );

    let src_books = load_external_books(&source).expect("external books must parse");
    let saved_books = load_external_books(&saved).expect("saved external books must parse");
    assert!(
        external_books_eq(&src_books, &saved_books),
        "external link inventory changed across the round trip"
    );
}

/// Malformed input must degrade to an error, never panic — the overlay cannot
/// even be constructed from bytes that are not a zip.
#[test]
fn preserve_garbage_input_degrades_not_panics() {
    assert!(ArchiveMap::parse(Arc::new(b"this is not a zip file at all".to_vec())).is_err());
    assert!(inventory_slicers(b"this is not a zip file at all").is_err());
    assert!(inventory_power_query(b"this is not a zip file at all").is_err());
    assert!(load_external_books(b"this is not a zip file at all").is_err());
}
