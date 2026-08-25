//! Absent-style default: when a workbook omits xl/styles.xml but styles are
//! requested, turbo must expose a valid default StyleTable resolving index 0
//! (the implicit Excel style). When styles are not requested, the table must
//! stay None (selective loading — no manufactured/parsed styles).
#![cfg(feature = "__arrow")]

use std::io::Write;
use std::path::PathBuf;

use kyrax::turbo::{Features, read_workbook_turbo};

/// Minimal store-only (method 0) ZIP with the given entries.
fn write_store_zip(path: &std::path::Path, entries: &[(&str, &[u8])]) {
    let mut local_records: Vec<(String, usize, usize, usize)> = Vec::new();
    let mut body = Vec::new();
    for (name, data) in entries {
        let name_b = name.as_bytes();
        let offset = body.len();
        body.extend_from_slice(&0x04034b50u32.to_le_bytes());
        body.extend_from_slice(&20u16.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes()); // method = store
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.extend_from_slice(&(name_b.len() as u16).to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(name_b);
        body.extend_from_slice(data);
        local_records.push((name.to_string(), offset, data.len(), data.len()));
    }
    let cd_off = body.len();
    for (name, local_off, csize, usize_) in &local_records {
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
    body.extend_from_slice(&(local_records.len() as u16).to_le_bytes());
    body.extend_from_slice(&(local_records.len() as u16).to_le_bytes());
    body.extend_from_slice(&(cd_size as u32).to_le_bytes());
    body.extend_from_slice(&(cd_off as u32).to_le_bytes());
    body.extend_from_slice(&0u16.to_le_bytes());

    let mut f = std::fs::File::create(path).expect("create zip");
    f.write_all(&body).expect("write zip");
}

fn tmp_xlsx(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("nextexcel_turbo_nostyles_{name}.xlsx"));
    p
}

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>"#;

const RELS_ROOT: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#;

const WORKBOOK: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
 xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets>
    <sheet name="NoStyles" sheetId="1" r:id="rId1"/>
  </sheets>
</workbook>"#;

const WB_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"#;

const SHEET: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1">
      <c r="A1" t="inlineStr"><is><t>h1</t></is></c>
      <c r="B1" t="inlineStr"><is><t>h2</t></is></c>
    </row>
    <row r="2">
      <c r="A2" t="n"><v>1</v></c>
      <c r="B2" t="n"><v>2</v></c>
    </row>
    <row r="3">
      <c r="A3" t="n"><v>3</v></c>
      <c r="C3" t="n"><v>9</v></c>
    </row>
  </sheetData>
</worksheet>"#;

fn write_nostyles(path: &PathBuf) {
    write_store_zip(
        path,
        &[
            ("[Content_Types].xml", CONTENT_TYPES.as_bytes()),
            ("_rels/.rels", RELS_ROOT.as_bytes()),
            ("xl/workbook.xml", WORKBOOK.as_bytes()),
            ("xl/_rels/workbook.xml.rels", WB_RELS.as_bytes()),
            ("xl/worksheets/sheet1.xml", SHEET.as_bytes()),
            // deliberately no xl/styles.xml
        ],
    );
}

/// Styles requested, styles.xml absent -> valid default StyleTable resolving index 0.
#[test]
fn styles_requested_get_default_table() {
    let path = tmp_xlsx("requested");
    write_nostyles(&path);

    let wb = read_workbook_turbo(path.to_str().unwrap(), Features::ALL).expect("read ok");
    let st = wb
        .style_table
        .as_ref()
        .expect("style_table must be Some when styles are requested");

    // The implicit Excel style index 0 resolves to a default xf.
    let r = st.resolve(0);
    assert_eq!(r.number_format, "General");
    assert!(!r.is_date);
    assert!(!r.is_timedelta);
    // Default font/fill/border xf ids are all 0 and resolvable.
    assert_eq!(r.font.name, "Calibri");
    assert_eq!(r.fill.pattern, "none");
    assert_eq!(r.border_id, 0);
    assert_eq!(r.style_name.as_deref(), Some("Normal"));

    // Style indices are present and all resolve to the default.
    let sheet = &wb.sheets[0];
    let si = sheet.style_indices.as_ref().expect("style_indices present");
    assert_eq!(si.len(), sheet.ncols);
    for col in si {
        for v in 0..sheet.nrows {
            let idx = col.value(v);
            assert_eq!(idx, 0, "implicit default style index is 0");
            let _ = st.resolve(idx);
        }
    }

    let _ = std::fs::remove_file(&path);
}

/// Styles NOT requested -> no manufactured/parsed styles, table stays None.
#[test]
fn styles_not_requested_stays_none() {
    let path = tmp_xlsx("not_requested");
    write_nostyles(&path);

    let wb = read_workbook_turbo(path.to_str().unwrap(), Features::VALUES).expect("read ok");
    assert!(
        wb.style_table.is_none(),
        "style_table must be None when styles are not requested"
    );
    let sheet = &wb.sheets[0];
    assert!(
        sheet.style_indices.is_none(),
        "style_indices must be None when styles are not requested"
    );

    let _ = std::fs::remove_file(&path);
}
