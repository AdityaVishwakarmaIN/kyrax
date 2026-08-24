//! F10: synthetic, deterministic round-trip coverage for opaque VBA-signature
//! and custom-control package parts. No external binary fixture is required.

use std::sync::Arc;

use kyrax::turbo::overlay::WorkbookOverlay;
use kyrax::turbo::write::CellValue;
use kyrax::turbo::{ArchiveMap, read_entry};

const CONTENT_TYPES: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="emf" ContentType="image/x-emf"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.ms-excel.sheet.macroEnabled.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/vbaProject.bin" ContentType="application/vnd.ms-office.vbaProject"/><Override PartName="/xl/vbaProjectSignature.bin" ContentType="application/vnd.ms-office.vbaProjectSignature"/><Override PartName="/xl/controls/control1.xml" ContentType="application/vnd.ms-excel.controlproperties+xml"/></Types>"#;
const ROOT_RELS: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
const WORKBOOK: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets><calcPr fullCalcOnLoad="1"/></workbook>"#;
const WORKBOOK_RELS: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.microsoft.com/office/2006/relationships/vbaProject" Target="vbaProject.bin"/></Relationships>"#;
const SHEET: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><dimension ref="A1"/><sheetData><row r="1"><c r="A1"><v>1</v></c></row></sheetData><controls><control shapeId="1" r:id="rId1" name="Synthetic Control"/></controls></worksheet>"#;
const SHEET_RELS: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.microsoft.com/office/2006/relationships/control" Target="../controls/control1.xml"/></Relationships>"#;

const VBA_PROJECT: &[u8] = b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1\x00synthetic-vba-project\xff\x80";
const VBA_SIGNATURE: &[u8] = b"\x00\x01synthetic-vba-signature\xff\x80\x00";
const VBA_RELS: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.microsoft.com/office/2006/relationships/vbaProjectSignature" Target="vbaProjectSignature.bin"/></Relationships>"#;

const CONTROL: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><control xmlns="http://schemas.microsoft.com/office/spreadsheetml/2009/9/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" name="Synthetic Control" r:id="rId1"/>"#;
const CONTROL_RELS: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/control1.emf"/></Relationships>"#;
const CONTROL_MEDIA: &[u8] = b"\x01\x00\x00\x00synthetic-control-emf\x00\xff\x80";

const ENTRIES: &[(&str, &[u8])] = &[
    ("[Content_Types].xml", CONTENT_TYPES),
    ("_rels/.rels", ROOT_RELS),
    ("xl/workbook.xml", WORKBOOK),
    ("xl/_rels/workbook.xml.rels", WORKBOOK_RELS),
    ("xl/worksheets/sheet1.xml", SHEET),
    ("xl/worksheets/_rels/sheet1.xml.rels", SHEET_RELS),
    ("xl/vbaProject.bin", VBA_PROJECT),
    ("xl/vbaProjectSignature.bin", VBA_SIGNATURE),
    ("xl/_rels/vbaProject.bin.rels", VBA_RELS),
    ("xl/controls/control1.xml", CONTROL),
    ("xl/controls/_rels/control1.xml.rels", CONTROL_RELS),
    ("xl/media/control1.emf", CONTROL_MEDIA),
];

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// Construct a stable STORE-only ZIP. The core reader does not verify CRCs, so
/// zero CRC fields keep this fixture small without weakening payload equality.
fn build_fixture() -> Vec<u8> {
    let mut zip = Vec::new();
    let mut central_directory = Vec::new();

    for (name, payload) in ENTRIES {
        let name = name.as_bytes();
        let local_offset = zip.len() as u32;

        zip.extend_from_slice(b"PK\x03\x04");
        push_u16(&mut zip, 20);
        push_u16(&mut zip, 0);
        push_u16(&mut zip, 0);
        push_u16(&mut zip, 0);
        push_u16(&mut zip, 0);
        push_u32(&mut zip, 0);
        push_u32(&mut zip, payload.len() as u32);
        push_u32(&mut zip, payload.len() as u32);
        push_u16(&mut zip, name.len() as u16);
        push_u16(&mut zip, 0);
        zip.extend_from_slice(name);
        zip.extend_from_slice(payload);

        central_directory.extend_from_slice(b"PK\x01\x02");
        push_u16(&mut central_directory, 20);
        push_u16(&mut central_directory, 20);
        push_u16(&mut central_directory, 0);
        push_u16(&mut central_directory, 0);
        push_u16(&mut central_directory, 0);
        push_u16(&mut central_directory, 0);
        push_u32(&mut central_directory, 0);
        push_u32(&mut central_directory, payload.len() as u32);
        push_u32(&mut central_directory, payload.len() as u32);
        push_u16(&mut central_directory, name.len() as u16);
        push_u16(&mut central_directory, 0);
        push_u16(&mut central_directory, 0);
        push_u16(&mut central_directory, 0);
        push_u16(&mut central_directory, 0);
        push_u32(&mut central_directory, 0);
        push_u32(&mut central_directory, local_offset);
        central_directory.extend_from_slice(name);
    }

    let central_offset = zip.len() as u32;
    let central_size = central_directory.len() as u32;
    zip.extend_from_slice(&central_directory);
    zip.extend_from_slice(b"PK\x05\x06");
    push_u16(&mut zip, 0);
    push_u16(&mut zip, 0);
    push_u16(&mut zip, ENTRIES.len() as u16);
    push_u16(&mut zip, ENTRIES.len() as u16);
    push_u32(&mut zip, central_size);
    push_u32(&mut zip, central_offset);
    push_u16(&mut zip, 0);
    zip
}

fn load_modify_save() -> (Vec<u8>, Vec<u8>) {
    let source = build_fixture();
    let map = ArchiveMap::parse(Arc::new(source.clone())).expect("fixture must load");
    let mut workbook = WorkbookOverlay::new(map);
    workbook.set_cell("Sheet1", 1, 1, CellValue::Str("modified".into()));
    let saved = workbook.save().expect("modified fixture must save");
    let sheet = read_part(&saved, "xl/worksheets/sheet1.xml");
    assert!(
        memchr::memmem::find(&sheet, b"modified").is_some(),
        "roundtrip must perform a real cell edit"
    );
    (source, saved)
}

fn read_part(zip: &[u8], name: &str) -> Vec<u8> {
    read_entry(zip, name)
        .unwrap_or_else(|error| panic!("failed to read {name}: {error}"))
        .unwrap_or_else(|| panic!("missing part {name}"))
}

fn assert_parts_unchanged(source: &[u8], saved: &[u8], names: &[&str]) {
    for name in names {
        assert_eq!(
            read_part(saved, name),
            read_part(source, name),
            "{name} changed during load-modify-save"
        );
    }
}

#[test]
fn fabricated_vba_signature_and_relationship_graph_survive_byte_for_byte() {
    // This is a VBA-project signature, not an OOXML package signature under
    // `_xmlsignatures/**`, which is correctly invalidated after an edit.
    let (source, saved) = load_modify_save();
    assert_parts_unchanged(
        &source,
        &saved,
        &[
            "xl/vbaProject.bin",
            "xl/vbaProjectSignature.bin",
            "xl/_rels/vbaProject.bin.rels",
            "xl/_rels/workbook.xml.rels",
            "[Content_Types].xml",
        ],
    );
}

#[test]
fn custom_control_parts_relationships_and_content_types_survive_byte_for_byte() {
    let (source, saved) = load_modify_save();
    assert_parts_unchanged(
        &source,
        &saved,
        &[
            "xl/controls/control1.xml",
            "xl/controls/_rels/control1.xml.rels",
            "xl/media/control1.emf",
            "xl/worksheets/_rels/sheet1.xml.rels",
            "[Content_Types].xml",
        ],
    );
}
