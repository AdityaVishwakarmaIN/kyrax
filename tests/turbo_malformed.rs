//! Malformed / truncated XLSX fixtures: turbo must error or degrade — never panic.
#![cfg(feature = "__arrow")]

use std::io::Write;
use std::path::PathBuf;

use kyrax::turbo::{read_workbook_turbo, Features};

/// Minimal store-only (method 0) ZIP with the given entries.
fn write_store_zip(path: &std::path::Path, entries: &[(&str, &[u8])]) {
    let mut local_records: Vec<(String, usize, usize, usize)> = Vec::new();
    let mut body = Vec::new();
    for (name, data) in entries {
        let name_b = name.as_bytes();
        let offset = body.len();
        // local file header
        body.extend_from_slice(&0x04034b50u32.to_le_bytes());
        body.extend_from_slice(&20u16.to_le_bytes()); // version needed
        body.extend_from_slice(&0u16.to_le_bytes()); // flags
        body.extend_from_slice(&0u16.to_le_bytes()); // method = store
        body.extend_from_slice(&0u16.to_le_bytes()); // time
        body.extend_from_slice(&0u16.to_le_bytes()); // date
        body.extend_from_slice(&0u32.to_le_bytes()); // crc
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.extend_from_slice(&(name_b.len() as u16).to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes()); // extra
        body.extend_from_slice(name_b);
        body.extend_from_slice(data);
        local_records.push((
            name.to_string(),
            offset,
            data.len(),
            data.len(),
        ));
    }
    let cd_off = body.len();
    for (name, local_off, csize, usize_) in &local_records {
        let name_b = name.as_bytes();
        body.extend_from_slice(&0x02014b50u32.to_le_bytes());
        body.extend_from_slice(&20u16.to_le_bytes());
        body.extend_from_slice(&20u16.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes()); // method
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
    // EOCD
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
    p.push(format!("nextexcel_turbo_malformed_{name}.xlsx"));
    p
}

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
  <Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>
  <Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>
</Types>"#;

const RELS_ROOT: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#;

const WORKBOOK: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
 xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets>
    <sheet name="Sheet1" sheetId="1" r:id="rId1"/>
  </sheets>
</workbook>"#;

const WB_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/>
</Relationships>"#;

/// Truncated mid-sheetData: no closing tags. Must not panic.
#[test]
fn truncated_sheet_xml_no_panic() {
    let sheet = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1"><c r="A1" t="inlineStr"><is><t>H</t></is></c></row>
    <row r="2"><c r="A2"><v>1</v></c><c r="B2" t="inlineStr"><is><t>trunca"#;

    let path = tmp_xlsx("trunc_sheet");
    write_store_zip(
        &path,
        &[
            ("[Content_Types].xml", CONTENT_TYPES.as_bytes()),
            ("_rels/.rels", RELS_ROOT.as_bytes()),
            ("xl/workbook.xml", WORKBOOK.as_bytes()),
            ("xl/_rels/workbook.xml.rels", WB_RELS.as_bytes()),
            ("xl/worksheets/sheet1.xml", sheet),
            (
                "xl/styles.xml",
                br#"<?xml version="1.0"?><styleSheet><fonts count="1"><font/></fonts><fills count="1"><fill/></fills><cellXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellXfs></styleSheet>"#,
            ),
            (
                "xl/sharedStrings.xml",
                br#"<?xml version="1.0"?><sst count="0" uniqueCount="0"></sst>"#,
            ),
        ],
    );

    // Either Ok (partial degrade) or Err — never panic.
    let _ = std::panic::catch_unwind(|| {
        let _ = read_workbook_turbo(path.to_str().unwrap(), Features::ALL);
    })
    .expect("truncated sheet XML must not panic");
}

/// Cell `@s` past cellXfs length and shared-string index past SST — degrade, no panic.
#[test]
fn oob_style_and_shared_string_index_no_panic() {
    // One cellXfs only (index 0). Cell uses s="99". SST has 1 string; cell refs si=42.
    let sheet = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1"><c r="A1" t="inlineStr"><is><t>H</t></is></c></row>
    <row r="2">
      <c r="A2" s="99" t="s"><v>42</v></c>
      <c r="B2" s="0"><v>3.14</v></c>
    </row>
  </sheetData>
</worksheet>"#;
    let styles = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <fonts count="1"><font><sz val="11"/><name val="Calibri"/></font></fonts>
  <fills count="1"><fill><patternFill patternType="none"/></fill></fills>
  <borders count="1"><border/></borders>
  <cellXfs count="1">
    <xf numFmtId="0" fontId="0" fillId="0" borderId="0"/>
  </cellXfs>
</styleSheet>"#;
    let sst = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="1" uniqueCount="1">
  <si><t>only</t></si>
</sst>"#;

    let path = tmp_xlsx("oob_xf_sst");
    write_store_zip(
        &path,
        &[
            ("[Content_Types].xml", CONTENT_TYPES.as_bytes()),
            ("_rels/.rels", RELS_ROOT.as_bytes()),
            ("xl/workbook.xml", WORKBOOK.as_bytes()),
            ("xl/_rels/workbook.xml.rels", WB_RELS.as_bytes()),
            ("xl/worksheets/sheet1.xml", sheet),
            ("xl/styles.xml", styles),
            ("xl/sharedStrings.xml", sst),
        ],
    );

    let wb = read_workbook_turbo(path.to_str().unwrap(), Features::ALL)
        .expect("OOB xf/sst should degrade, not error");
    let s = &wb.sheets[0];
    assert_eq!(s.nrows, 1);
    assert!(s.ncols >= 2);
    // Style index 99 is stored as-is; StyleTable::resolve falls back to xf 0.
    let st = wb.style_table.as_ref().expect("styles");
    assert_eq!(st.xfs.len(), 1);
    let r = st.resolve(99);
    assert_eq!(r.number_format, "General");
    // Shared string 42 is null (not panic). Style index 99 is retained as raw @s.
    if let Some(si) = &s.style_indices {
        use arrow_array::Array;
        assert!(!si[0].is_null(0));
        assert_eq!(si[0].value(0), 99);
    }
}

/// Shared formula `si=` with no anchor definition → empty formula text, no panic.
#[test]
fn orphan_shared_formula_si_no_panic() {
    let sheet = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1"><c r="A1" t="inlineStr"><is><t>H</t></is></c><c r="B1" t="inlineStr"><is><t>F</t></is></c></row>
    <row r="2">
      <c r="A2"><v>1</v></c>
      <c r="B2"><f t="shared" si="7"/><v>2</v></c>
    </row>
  </sheetData>
</worksheet>"#;

    let path = tmp_xlsx("orphan_si");
    write_store_zip(
        &path,
        &[
            ("[Content_Types].xml", CONTENT_TYPES.as_bytes()),
            ("_rels/.rels", RELS_ROOT.as_bytes()),
            ("xl/workbook.xml", WORKBOOK.as_bytes()),
            ("xl/_rels/workbook.xml.rels", WB_RELS.as_bytes()),
            ("xl/worksheets/sheet1.xml", sheet),
            (
                "xl/styles.xml",
                br#"<?xml version="1.0"?><styleSheet><fonts count="1"><font/></fonts><fills count="1"><fill/></fills><cellXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellXfs></styleSheet>"#,
            ),
            (
                "xl/sharedStrings.xml",
                br#"<?xml version="1.0"?><sst count="0" uniqueCount="0"></sst>"#,
            ),
        ],
    );

    let wb = read_workbook_turbo(path.to_str().unwrap(), Features::ALL)
        .expect("orphan shared si should degrade");
    let f = wb.sheets[0].formulas.as_ref().expect("formulas flag on");
    assert!(f.len() >= 1);
    let texts = f.materialize_all();
    // Orphan si → empty string, not panic.
    assert!(
        texts.iter().any(|t| t.is_empty()),
        "expected empty translated formula for orphan si, got {texts:?}"
    );
}

/// Empty sqref dataValidation + min>max col + oob dxfId — skip / null, no panic.
#[test]
fn empty_sqref_minmax_col_oob_dxf_no_panic() {
    let sheet = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <cols>
    <col min="5" max="3" width="10"/>
    <col min="1" max="2" width="8"/>
  </cols>
  <sheetData>
    <row r="1"><c r="A1" t="inlineStr"><is><t>H</t></is></c></row>
    <row r="2"><c r="A2"><v>1</v></c></row>
  </sheetData>
  <conditionalFormatting sqref="A2">
    <cfRule type="cellIs" dxfId="99" priority="1" operator="equal">
      <formula>1</formula>
    </cfRule>
  </conditionalFormatting>
  <dataValidations count="2">
    <dataValidation type="whole" sqref="" allowBlank="1">
      <formula1>1</formula1>
    </dataValidation>
    <dataValidation type="whole" sqref="A2" allowBlank="1">
      <formula1>0</formula1>
    </dataValidation>
  </dataValidations>
</worksheet>"#;
    let styles = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <fonts count="1"><font/></fonts>
  <fills count="1"><fill/></fills>
  <borders count="1"><border/></borders>
  <cellXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellXfs>
  <dxfs count="1"><dxf><font><b/></font></dxf></dxfs>
</styleSheet>"#;

    let path = tmp_xlsx("empty_sqref_minmax");
    write_store_zip(
        &path,
        &[
            ("[Content_Types].xml", CONTENT_TYPES.as_bytes()),
            ("_rels/.rels", RELS_ROOT.as_bytes()),
            ("xl/workbook.xml", WORKBOOK.as_bytes()),
            ("xl/_rels/workbook.xml.rels", WB_RELS.as_bytes()),
            ("xl/worksheets/sheet1.xml", sheet),
            ("xl/styles.xml", styles),
            (
                "xl/sharedStrings.xml",
                br#"<?xml version="1.0"?><sst count="0" uniqueCount="0"></sst>"#,
            ),
        ],
    );

    let wb = read_workbook_turbo(path.to_str().unwrap(), Features::ALL)
        .expect("malformed meta must degrade");
    let s = &wb.sheets[0];
    // min>max col skipped
    if let Some(cols) = &s.column_dimensions {
        for c in cols {
            assert!(c.min <= c.max, "inverted col range {:?}", c);
        }
        assert!(cols.iter().any(|c| c.min == 1 && c.max == 2));
        assert!(!cols.iter().any(|c| c.min == 5 && c.max == 3));
    }
    // empty sqref skipped; valid A2 kept
    if let Some(dvs) = &s.data_validations {
        assert!(dvs.iter().all(|d| !d.sqref.trim().is_empty()));
        assert!(dvs.iter().any(|d| d.sqref.contains("A2")));
    }
    // oob dxfId still recorded on rule (resolve is None at Python layer)
    if let Some(cfs) = &s.cf_rules {
        assert!(cfs.iter().any(|r| r.dxf_id == Some(99)));
    }
}

/// VBA relationship present but part missing — present flag, no bytes, no panic.
#[test]
fn vba_rel_missing_part_no_panic() {
    let sheet = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1"><c r="A1" t="inlineStr"><is><t>H</t></is></c></row>
    <row r="2"><c r="A2"><v>1</v></c></row>
  </sheetData>
</worksheet>"#;
    let ct = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
  <Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>
  <Override PartName="/xl/vbaProject.bin" ContentType="application/vnd.ms-office.vbaProject"/>
</Types>"#;
    let wb_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
  <Relationship Id="rId3" Type="http://schemas.microsoft.com/office/2006/relationships/vbaProject" Target="vbaProject.bin"/>
</Relationships>"#;

    let path = tmp_xlsx("vba_missing_part");
    write_store_zip(
        &path,
        &[
            ("[Content_Types].xml", ct),
            ("_rels/.rels", RELS_ROOT.as_bytes()),
            ("xl/workbook.xml", WORKBOOK.as_bytes()),
            ("xl/_rels/workbook.xml.rels", wb_rels),
            ("xl/worksheets/sheet1.xml", sheet),
            (
                "xl/styles.xml",
                br#"<?xml version="1.0"?><styleSheet><fonts count="1"><font/></fonts><fills count="1"><fill/></fills><cellXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellXfs></styleSheet>"#,
            ),
        ],
    );

    let wb = std::panic::catch_unwind(|| {
        read_workbook_turbo(path.to_str().unwrap(), Features::ALL)
    })
    .expect("vba missing part must not panic")
    .expect("vba missing part should Ok");
    let vba = wb.vba.as_ref().expect("vba flag on");
    assert!(vba.present);
    assert!(vba.bytes.is_none());
}

/// Truncated chart XML via drawing rel — no panic (empty/partial charts).
#[test]
fn truncated_chart_xml_no_panic() {
    let sheet = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
 xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheetData>
    <row r="1"><c r="A1" t="inlineStr"><is><t>H</t></is></c></row>
    <row r="2"><c r="A2"><v>1</v></c></row>
  </sheetData>
  <drawing r:id="rId1"/>
</worksheet>"#;
    let sheet_rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing" Target="../drawings/drawing1.xml"/>
</Relationships>"#;
    let drawing = br#"<?xml version="1.0"?>
<xdr:wsDr xmlns:xdr="http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing"
 xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"
 xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
  <xdr:twoCellAnchor>
    <xdr:from><xdr:col>0</xdr:col><xdr:row>0</xdr:row></xdr:from>
    <xdr:to><xdr:col>2</xdr:col><xdr:row>2</xdr:row></xdr:to>
    <xdr:graphicFrame>
      <a:graphic>
        <a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/chart">
          <c:chart xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart" r:id="rId1"/>
        </a:graphicData>
      </a:graphic>
    </xdr:graphicFrame>
    <xdr:clientData/>
  </xdr:twoCellAnchor>
</xdr:wsDr>"#;
    let drawing_rels = br#"<?xml version="1.0"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart" Target="../charts/chart1.xml"/>
</Relationships>"#;
    let chart_trunc = br#"<?xml version="1.0"?>
<c:chartSpace xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart">
  <c:chart><c:title><c:tx><c:rich><a:p xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:r><a:t>Trunc"#;
    let ct = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
  <Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>
  <Override PartName="/xl/drawings/drawing1.xml" ContentType="application/vnd.openxmlformats-officedocument.drawing+xml"/>
  <Override PartName="/xl/charts/chart1.xml" ContentType="application/vnd.openxmlformats-officedocument.drawingml.chart+xml"/>
</Types>"#;

    let path = tmp_xlsx("trunc_chart");
    write_store_zip(
        &path,
        &[
            ("[Content_Types].xml", ct),
            ("_rels/.rels", RELS_ROOT.as_bytes()),
            ("xl/workbook.xml", WORKBOOK.as_bytes()),
            ("xl/_rels/workbook.xml.rels", WB_RELS.as_bytes()),
            ("xl/worksheets/sheet1.xml", sheet),
            ("xl/worksheets/_rels/sheet1.xml.rels", sheet_rels),
            ("xl/drawings/drawing1.xml", drawing),
            ("xl/drawings/_rels/drawing1.xml.rels", drawing_rels),
            ("xl/charts/chart1.xml", chart_trunc),
            (
                "xl/styles.xml",
                br#"<?xml version="1.0"?><styleSheet><fonts count="1"><font/></fonts><fills count="1"><fill/></fills><cellXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellXfs></styleSheet>"#,
            ),
        ],
    );

    let _ = std::panic::catch_unwind(|| {
        let _ = read_workbook_turbo(path.to_str().unwrap(), Features::ALL);
    })
    .expect("truncated chart XML must not panic");
}

/// Threaded comment with unknown personId — empty display name, no panic.
#[test]
fn threaded_unknown_person_no_panic() {
    let sheet = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
 xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheetData>
    <row r="1"><c r="A1" t="inlineStr"><is><t>H</t></is></c></row>
    <row r="2"><c r="A2" t="inlineStr"><is><t>x</t></is></c></row>
  </sheetData>
</worksheet>"#;
    let sheet_rels = br#"<?xml version="1.0"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/threadedComment" Target="../threadedComments/threadedComment1.xml"/>
</Relationships>"#;
    let persons = br#"<?xml version="1.0"?>
<personList xmlns="http://schemas.microsoft.com/office/spreadsheetml/2018/threadedcomments">
  <person displayName="Alice" id="{11111111-1111-1111-1111-111111111111}"/>
</personList>"#;
    let tc = br#"<?xml version="1.0"?>
<ThreadedComments xmlns="http://schemas.microsoft.com/office/spreadsheetml/2018/threadedcomments">
  <threadedComment ref="A2" personId="{99999999-9999-9999-9999-999999999999}" id="{aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa}" dT="2024-01-01T00:00:00Z">
    <text>orphan person</text>
  </threadedComment>
</ThreadedComments>"#;
    let wb_rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/person" Target="persons/person.xml"/>
</Relationships>"#;
    let ct = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
  <Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>
  <Override PartName="/xl/threadedComments/threadedComment1.xml" ContentType="application/vnd.ms-excel.threadedcomments+xml"/>
  <Override PartName="/xl/persons/person.xml" ContentType="application/vnd.ms-excel.person+xml"/>
</Types>"#;

    let path = tmp_xlsx("tc_unknown_person");
    write_store_zip(
        &path,
        &[
            ("[Content_Types].xml", ct),
            ("_rels/.rels", RELS_ROOT.as_bytes()),
            ("xl/workbook.xml", WORKBOOK.as_bytes()),
            ("xl/_rels/workbook.xml.rels", wb_rels),
            ("xl/worksheets/sheet1.xml", sheet),
            ("xl/worksheets/_rels/sheet1.xml.rels", sheet_rels),
            ("xl/threadedComments/threadedComment1.xml", tc),
            ("xl/persons/person.xml", persons),
            (
                "xl/styles.xml",
                br#"<?xml version="1.0"?><styleSheet><fonts count="1"><font/></fonts><fills count="1"><fill/></fills><cellXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellXfs></styleSheet>"#,
            ),
        ],
    );

    let wb = read_workbook_turbo(path.to_str().unwrap(), Features::ALL)
        .expect("unknown personId must degrade");
    let thr = wb.sheets[0]
        .threaded_comments
        .as_ref()
        .expect("comments flag");
    assert_eq!(thr.len(), 1);
    assert_eq!(thr[0].text, "orphan person");
    assert!(thr[0].person_display_name.is_empty());
}

/// Pivot table part with missing cacheDefinition rel — empty cache meta, no panic.
#[test]
fn pivot_missing_cache_rel_no_panic() {
    let sheet = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
 xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheetData>
    <row r="1"><c r="A1" t="inlineStr"><is><t>H</t></is></c></row>
    <row r="2"><c r="A2"><v>1</v></c></row>
  </sheetData>
</worksheet>"#;
    let sheet_rels = br#"<?xml version="1.0"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotTable" Target="../pivotTables/pivotTable1.xml"/>
</Relationships>"#;
    let pivot = br#"<?xml version="1.0" encoding="UTF-8"?>
<pivotTableDefinition xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
 name="P1" cacheId="0" dataCaption="Values">
  <location ref="E3:G8" firstHeaderRow="1" firstDataRow="2" firstDataCol="1"/>
  <pivotFields count="1"><pivotField dataField="1" showAll="0"/></pivotFields>
</pivotTableDefinition>"#;
    let pivot_rels = br#"<?xml version="1.0"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
</Relationships>"#;
    let ct = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
  <Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>
  <Override PartName="/xl/pivotTables/pivotTable1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.pivotTable+xml"/>
</Types>"#;

    let path = tmp_xlsx("pivot_no_cache");
    write_store_zip(
        &path,
        &[
            ("[Content_Types].xml", ct),
            ("_rels/.rels", RELS_ROOT.as_bytes()),
            ("xl/workbook.xml", WORKBOOK.as_bytes()),
            ("xl/_rels/workbook.xml.rels", WB_RELS.as_bytes()),
            ("xl/worksheets/sheet1.xml", sheet),
            ("xl/worksheets/_rels/sheet1.xml.rels", sheet_rels),
            ("xl/pivotTables/pivotTable1.xml", pivot),
            ("xl/pivotTables/_rels/pivotTable1.xml.rels", pivot_rels),
            (
                "xl/styles.xml",
                br#"<?xml version="1.0"?><styleSheet><fonts count="1"><font/></fonts><fills count="1"><fill/></fills><cellXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellXfs></styleSheet>"#,
            ),
        ],
    );

    let wb = std::panic::catch_unwind(|| {
        read_workbook_turbo(path.to_str().unwrap(), Features::ALL)
    })
    .expect("pivot missing cache must not panic")
    .expect("pivot missing cache should Ok");
    let pivs = wb.sheets[0].pivots.as_ref().expect("pivots flag");
    // May be empty or a pivot with empty cache fields — either is fine.
    assert!(pivs.len() <= 1);
}
