//! PIVOT AUTHOR — Task B5b, the WRITE half.
//!
//! The preserve half (tests/pivot_preserve.rs) proves mutations leave a
//! real pivot's parts intact. This file proves the reverse direction: a pivot
//! AUTHORED from scratch — cache definition + records + table part + all the
//! rels/content-type wiring — is structurally correct.
//!
//! The strongest correctness signal is the round trip: author a pivot, save,
//! then read it back with the EXISTING `parse_pivot_table` reader and assert
//! the layout matches what was requested. On top of that:
//!
//!   * the emitted parts are asserted byte-for-byte for the fixture-shaped
//!     source (sharedItems flags honest, records materialised, cacheId wired
//!     consistently through workbook.xml / cache / table);
//!   * authoring a second pivot into the same workbook yields a distinct
//!     cacheId and both survive;
//!   * the same workbook written twice is byte-identical;
//!   * openpyxl (when present in the Python environment) loads the output
//!     without error — weak semantics, strong non-corruption signal.

use pretty_assertions::assert_eq;
use pretty_assertions::assert_ne;
use std::process::Command;

use kyrax::turbo::write::{
    CellValue, PivotAgg, PivotField, Row, Sheet, Workbook, save_workbook, save_workbook_stream,
    write_workbook_bytes,
};
use kyrax::turbo::{Features, read_entry, read_workbook_turbo};

// ---------------------------------------------------------------------------
// Fixture: the same data as testdata/pivot.xlsx (Region/Product/Amount).
// ---------------------------------------------------------------------------

fn data_sheet() -> Sheet {
    let mut s = Sheet::new("Data");
    let rows: &[&[CellValue]] = &[
        &[
            CellValue::Str("Region".into()),
            CellValue::Str("Product".into()),
            CellValue::Str("Amount".into()),
        ],
        &[
            CellValue::Str("East".into()),
            CellValue::Str("Widget".into()),
            CellValue::Number(100.0),
        ],
        &[
            CellValue::Str("East".into()),
            CellValue::Str("Gadget".into()),
            CellValue::Number(150.0),
        ],
        &[
            CellValue::Str("West".into()),
            CellValue::Str("Widget".into()),
            CellValue::Number(200.0),
        ],
        &[
            CellValue::Str("West".into()),
            CellValue::Str("Gadget".into()),
            CellValue::Number(50.0),
        ],
    ];
    for (ri, row) in rows.iter().enumerate() {
        let mut r = Row::new(ri as u32 + 1);
        for (ci, v) in row.iter().enumerate() {
            r.cells
                .push(kyrax::turbo::write::Cell::new(ci as u32 + 1, v.clone()));
        }
        s.rows.push(r);
    }
    s
}

fn wb_with_pivot() -> Workbook {
    let mut wb = Workbook::with_sheet("Data");
    wb.sheets[0] = data_sheet();
    wb.add_pivot_table(
        0,
        "A1:C5",
        &[PivotField::Name("Region".into())],
        &[PivotField::Name("Product".into())],
        &[(PivotField::Name("Amount".into()), PivotAgg::Sum)],
        "E3",
    )
    .expect("author pivot");
    wb
}

fn tmp_path(name: &str) -> String {
    std::env::temp_dir()
        .join(format!("kyrax_pivot_author_{name}.xlsx"))
        .to_string_lossy()
        .into_owned()
}

fn save_read(wb: &Workbook, name: &str) -> (Vec<u8>, kyrax::turbo::TurboWorkbook) {
    let bytes = write_workbook_bytes(wb).expect("write bytes");
    let path = tmp_path(name);
    std::fs::write(&path, &bytes).expect("write temp workbook");
    let read = read_workbook_turbo(&path, Features::PIVOTS).expect("read back");
    let _ = std::fs::remove_file(&path);
    (bytes, read)
}

// ---------------------------------------------------------------------------
// Round trip: the authored layout must come back exactly as requested.
// ---------------------------------------------------------------------------

#[test]
fn authored_pivot_round_trips_layout() {
    let wb = wb_with_pivot();
    let (_bytes, read) = save_read(&wb, "roundtrip");
    assert_eq!(read.sheets.len(), 1);
    let pivs = read.sheets[0].pivots.as_ref().expect("pivots on");
    assert_eq!(pivs.len(), 1);
    let p = &pivs[0];
    assert_eq!(p.name, "PivotTable1");
    assert_eq!(p.row_fields, vec!["Region"]);
    assert_eq!(p.col_fields, vec!["Product"]);
    assert_eq!(p.data_fields.len(), 1);
    assert_eq!(p.data_fields[0].name, "Sum of Amount");
    assert_eq!(p.data_fields[0].field_index, 2);
    assert_eq!(p.location_ref, "E3:H6");
    assert_eq!(p.cache_id, 0);
    // The cache source survives on the cache definition.
    assert_eq!(p.cache.worksheet_sheet.as_deref(), Some("Data"));
    assert_eq!(p.cache.worksheet_ref.as_deref(), Some("A1:C5"));
    assert_eq!(p.cache.field_names, vec!["Region", "Product", "Amount"]);
}

#[test]
fn multiple_data_fields_and_row_fields_round_trip() {
    let mut wb = Workbook::with_sheet("Data");
    wb.sheets[0] = data_sheet();
    wb.add_pivot_table(
        0,
        "A1:C5",
        &[PivotField::Name("Region".into())],
        &[],
        &[
            (PivotField::Name("Amount".into()), PivotAgg::Sum),
            (PivotField::Name("Amount".into()), PivotAgg::Average),
        ],
        "J3",
    )
    .expect("author");
    let (_b, read) = save_read(&wb, "multi_data");
    let pivs = read.sheets[0].pivots.as_ref().expect("pivots");
    assert_eq!(pivs.len(), 1);
    let p = &pivs[0];
    assert_eq!(p.row_fields, vec!["Region"]);
    assert!(p.col_fields.is_empty(), "no col fields");
    assert_eq!(p.data_fields.len(), 2);
    assert_eq!(p.data_fields[0].name, "Sum of Amount");
    assert_eq!(p.data_fields[1].name, "Average of Amount");
    assert_eq!(p.data_fields[1].field_index, 2);
}

#[test]
fn two_pivots_get_distinct_cache_ids_and_both_survive() {
    let mut wb = Workbook::with_sheet("Data");
    wb.sheets[0] = data_sheet();
    wb.add_pivot_table(
        0,
        "A1:C5",
        &[PivotField::Name("Region".into())],
        &[PivotField::Name("Product".into())],
        &[(PivotField::Name("Amount".into()), PivotAgg::Sum)],
        "E3",
    )
    .expect("pivot 1");
    // Same source, different layout, different target.
    wb.add_pivot_table(
        0,
        "A1:C5",
        &[PivotField::Name("Product".into())],
        &[],
        &[(PivotField::Name("Amount".into()), PivotAgg::Count)],
        "J3",
    )
    .expect("pivot 2");
    let (_b, read) = save_read(&wb, "two_pivots");
    let pivs = read.sheets[0].pivots.as_ref().expect("pivots");
    assert_eq!(pivs.len(), 2, "both pivots survive the round trip");
    let mut ids: Vec<u32> = pivs.iter().map(|p| p.cache_id).collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![0, 1], "distinct cache ids");
    // The reader returns pivots in rel-map order (not authoring order), so
    // resolve by name and assert each requested layout survived.
    let by_name: Vec<&kyrax::turbo::PivotTableMeta> =
        pivs.iter().filter(|p| p.name == "PivotTable1").collect();
    assert_eq!(by_name.len(), 1);
    assert_eq!(by_name[0].row_fields, vec!["Region"]);
    assert_eq!(by_name[0].col_fields, vec!["Product"]);
    assert_eq!(by_name[0].data_fields[0].name, "Sum of Amount");
    let p2: Vec<&kyrax::turbo::PivotTableMeta> =
        pivs.iter().filter(|p| p.name == "PivotTable2").collect();
    assert_eq!(p2.len(), 1);
    assert_eq!(p2[0].row_fields, vec!["Product"]);
    assert_eq!(p2[0].data_fields[0].name, "Count of Amount");
    assert_ne!(by_name[0].cache_id, p2[0].cache_id);
}

// ---------------------------------------------------------------------------
// Byte determinism: same input, byte-identical output.
// ---------------------------------------------------------------------------

#[test]
fn authored_pivot_is_byte_deterministic() {
    let wb = wb_with_pivot();
    let a = write_workbook_bytes(&wb).expect("first");
    let b = write_workbook_bytes(&wb).expect("second");
    assert_eq!(a, b, "same workbook twice must produce identical bytes");
}

#[test]
fn authored_pivot_stream_path_emits_all_parts() {
    use std::io::Cursor;
    let wb = wb_with_pivot();
    let mut out = Cursor::new(Vec::new());
    save_workbook_stream(&wb, &mut out).expect("stream");
    let streamed = out.into_inner();
    for part_name in [
        "xl/pivotTables/pivotTable1.xml",
        "xl/pivotTables/_rels/pivotTable1.xml.rels",
        "xl/pivotCache/pivotCacheDefinition1.xml",
        "xl/pivotCache/_rels/pivotCacheDefinition1.xml.rels",
        "xl/pivotCache/pivotCacheRecords1.xml",
    ] {
        assert!(
            read_entry(&streamed, part_name).expect("read").is_some(),
            "stream path missing {part_name}"
        );
    }
    let path = tmp_path("stream");
    std::fs::write(&path, &streamed).expect("write");
    let read = read_workbook_turbo(&path, Features::PIVOTS).expect("read streamed workbook");
    let _ = std::fs::remove_file(&path);
    let pivs = read.sheets[0].pivots.as_ref().expect("pivots");
    assert_eq!(pivs.len(), 1);
    assert_eq!(pivs[0].row_fields, vec!["Region"]);
}

// ---------------------------------------------------------------------------
// Package wiring: workbook pivotCaches, rels, content types, part XML shape.
// ---------------------------------------------------------------------------

fn part(zip: &[u8], name: &str) -> String {
    let bytes = read_entry(zip, name)
        .expect("read_entry")
        .unwrap_or_else(|| panic!("part {name} missing"));
    String::from_utf8_lossy(&bytes).into_owned()
}

#[test]
fn emitted_parts_wire_the_package() {
    let wb = wb_with_pivot();
    let bytes = write_workbook_bytes(&wb).expect("write");

    // workbook.xml: pivotCaches after calcPr, cacheId 0 → rId4 (sheets=1,
    // styles=2, theme=3, then the cache definition).
    let wb_xml = part(&bytes, "xl/workbook.xml");
    assert!(
        wb_xml.contains(r#"<calcPr calcId="124519" fullCalcOnLoad="1"/><pivotCaches><pivotCache cacheId="0" r:id="rId4"/></pivotCaches>"#),
        "{wb_xml}"
    );

    // workbook rels: rId1=sheet, rId2=styles, rId3=theme, rId4=pivot cache def.
    let wb_rels = part(&bytes, "xl/_rels/workbook.xml.rels");
    assert!(
        wb_rels.contains(r#"Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotCacheDefinition" Target="pivotCache/pivotCacheDefinition1.xml" Id="rId4"/>"#),
        "{wb_rels}"
    );

    // Sheet rels point at the pivot table part.
    let sheet_rels = part(&bytes, "xl/worksheets/_rels/sheet1.xml.rels");
    assert!(
        sheet_rels.contains(r#"Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotTable" Target="/xl/pivotTables/pivotTable1.xml""#),
        "{sheet_rels}"
    );

    // Cache-def rels point at the records part.
    let cache_rels = part(&bytes, "xl/pivotCache/_rels/pivotCacheDefinition1.xml.rels");
    assert!(
        cache_rels.contains("pivotCacheRecords1.xml"),
        "{cache_rels}"
    );

    // Table rels point back at the cache definition.
    let table_rels = part(&bytes, "xl/pivotTables/_rels/pivotTable1.xml.rels");
    assert!(
        table_rels.contains(r#"Target="../pivotCache/pivotCacheDefinition1.xml""#),
        "{table_rels}"
    );

    // Content types: every new part has an Override.
    let ct = part(&bytes, "[Content_Types].xml");
    for needle in [
        r#"PartName="/xl/pivotCache/pivotCacheDefinition1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.pivotCacheDefinition+xml""#,
        r#"PartName="/xl/pivotCache/pivotCacheRecords1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.pivotCacheRecords+xml""#,
        r#"PartName="/xl/pivotTables/pivotTable1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.pivotTable+xml""#,
    ] {
        assert!(ct.contains(needle), "content type missing {needle}: {ct}");
    }

    // Cache definition: honest flags + records count + refreshOnLoad.
    let cache = part(&bytes, "xl/pivotCache/pivotCacheDefinition1.xml");
    assert!(cache.contains(r#"recordCount="4""#), "{cache}");
    assert!(cache.contains(r#"refreshOnLoad="1""#), "{cache}");
    assert!(
        cache.contains(r#"<worksheetSource sheet="Data" ref="A1:C5"/>"#),
        "{cache}"
    );
    assert!(
        cache.contains(r#"<sharedItems count="2" containsSemiMixedTypes="0" containsString="1" containsBlank="0" containsNumber="0"><s v="East"/><s v="West"/></sharedItems>"#),
        "{cache}"
    );
    assert!(
        cache.contains(r#"<sharedItems containsSemiMixedTypes="0" containsString="0" containsBlank="0" containsNumber="1" containsInteger="1" minValue="50" maxValue="200"/>"#),
        "{cache}"
    );

    // Records: one per source row, inline numbers + shared indices.
    let recs = part(&bytes, "xl/pivotCache/pivotCacheRecords1.xml");
    assert!(recs.contains(r#"count="4""#), "{recs}");
    assert!(
        recs.contains(r#"<r><s v="0"/><s v="0"/><n v="100"/></r>"#),
        "{recs}"
    );
    assert!(
        recs.contains(r#"<r><s v="1"/><s v="1"/><n v="50"/></r>"#),
        "{recs}"
    );

    // Table part: location + axes + data fields.
    let table = part(&bytes, "xl/pivotTables/pivotTable1.xml");
    assert!(
        table.contains(
            r#"<location ref="E3:H6" firstHeaderRow="1" firstDataRow="2" firstDataCol="1"/>"#
        ),
        "{table}"
    );
    assert!(
        table.contains(r#"<rowFields count="1"><field x="0"/></rowFields>"#),
        "{table}"
    );
    assert!(
        table.contains(r#"<colFields count="1"><field x="1"/></colFields>"#),
        "{table}"
    );
    assert!(
        table.contains(
            r#"<dataField name="Sum of Amount" fld="2" baseField="0" baseItem="0" subtotal="sum"/>"#
        ),
        "{table}"
    );
    assert!(table.contains(r#"cacheId="0""#), "{table}");
}

// ---------------------------------------------------------------------------
// openpyxl second-reader cross-check (weak semantics, strong non-corruption).
// ---------------------------------------------------------------------------

#[test]
fn openpyxl_loads_authored_pivot() {
    let wb = wb_with_pivot();
    let path = tmp_path("openpyxl");
    save_workbook(&wb, &path).expect("save");
    let outcome = run_openpyxl_check(&path);
    let _ = std::fs::remove_file(&path);
    let outcome = match outcome {
        Some(o) => o,
        None => return, // no Python/openpyxl in this environment
    };
    assert!(outcome.starts_with("OK"), "openpyxl: {outcome}");
}

fn run_openpyxl_check(path: &str) -> Option<String> {
    let python = ["python", "python3"].iter().find(|c| {
        Command::new(c)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })?;
    let script = r#"import sys, traceback
try:
    from openpyxl import load_workbook
except Exception:
    sys.exit(3)
try:
    wb = load_workbook(sys.argv[1])
    for ws in wb.worksheets:
        _ = ws.max_row
        _ = ws.max_column
    print("OK")
except Exception:
    print("FAIL\t" + traceback.format_exc().replace("\t", " ").replace("\n", " | "))
"#;
    let dir = std::env::temp_dir().join("kyrax_pivot_author_py");
    let _ = std::fs::create_dir_all(&dir);
    let script_path = dir.join("openpyxl_check.py");
    if std::fs::write(&script_path, script).is_err() {
        return None;
    }
    let out = Command::new(python)
        .arg(&script_path)
        .arg(path)
        .output()
        .ok()?;
    if out.status.code() == Some(3) {
        return None; // openpyxl not installed
    }
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim() == "OK" {
        Some("OK".into())
    } else {
        Some(stdout.trim().to_string())
    }
}

// ---------------------------------------------------------------------------
// Validation errors surface through add_pivot_table, not silent skips.
// ---------------------------------------------------------------------------

#[test]
fn bad_field_name_is_rejected() {
    let mut wb = Workbook::with_sheet("Data");
    wb.sheets[0] = data_sheet();
    let err = wb
        .add_pivot_table(
            0,
            "A1:C5",
            &[PivotField::Name("Nope".into())],
            &[],
            &[(PivotField::Name("Amount".into()), PivotAgg::Sum)],
            "E3",
        )
        .unwrap_err();
    assert!(err.contains("Nope"), "{err}");
}

#[test]
fn out_of_range_sheet_index_is_rejected() {
    let mut wb = Workbook::with_sheet("Data");
    wb.sheets[0] = data_sheet();
    assert!(
        wb.add_pivot_table(
            9,
            "A1:C5",
            &[PivotField::Index(0)],
            &[],
            &[(PivotField::Index(2), PivotAgg::Sum)],
            "E3",
        )
        .is_err()
    );
}
