//! C2 validate & repair integration tests (Tier 3 HIGH).
//!
//! Order matters here: the CLEAN corpus is swept first so false positives are
//! fixed before the malformed assertions are trusted, exactly as the C2 plan
//! demands. A validator with false positives is useless.
#![cfg(feature = "__arrow")]

use std::path::Path;

use kyrax::turbo::{FindingCode, RepairOptions, repair_workbook, validate_workbook};
use pretty_assertions::assert_eq;

fn testdata(name: &str) -> String {
    format!("{}/testdata/{}", env!("CARGO_MANIFEST_DIR"), name)
}

fn temp_xlsx(name: &str) -> String {
    let mut p = std::env::temp_dir();
    p.push(format!("kyrax_validate_{name}.xlsx"));
    p.to_str().unwrap().to_string()
}

fn clean_corpus_files() -> Vec<String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata");
    let mut files: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("testdata dir") {
        let entry = entry.expect("entry");
        let name = entry.file_name().to_str().expect("utf8").to_string();
        if name.starts_with("stress2_malformed_") {
            continue;
        }
        if name.ends_with(".xlsx") || name.ends_with(".xlsm") {
            files.push(entry.path().to_str().unwrap().to_string());
        }
    }
    files.sort();
    files
}

fn malformed_corpus_files() -> Vec<String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata");
    let mut files: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("testdata dir") {
        let entry = entry.expect("entry");
        let name = entry.file_name().to_str().expect("utf8").to_string();
        if name.starts_with("stress2_malformed_") && name.ends_with(".xlsx") {
            files.push(entry.path().to_str().unwrap().to_string());
        }
    }
    files.sort();
    files
}

// ---------------------------------------------------------------------------
// CLEAN CORPUS FIRST — the assertion most likely to fail.
// ---------------------------------------------------------------------------

#[test]
fn clean_testdata_corpus_has_zero_findings() {
    let mut failures: Vec<String> = Vec::new();
    for path in clean_corpus_files() {
        let name = path.rsplit(['/', '\\']).next().unwrap().to_string();
        let report = validate_workbook(&path).expect("validate must not throw");
        if !report.is_clean() {
            for f in &report.findings {
                failures.push(format!(
                    "{name}: [{:?}/{:?}] {} {} — {}",
                    f.severity,
                    f.code,
                    f.part,
                    f.location.as_deref().unwrap_or(""),
                    f.message
                ));
            }
        }
    }
    if !failures.is_empty() {
        panic!("clean corpus findings:\n{}", failures.join("\n"));
    }
}

#[test]
fn clean_fixture_files_have_zero_findings() {
    let fixtures = [
        "sheet-with-defined-names.xlsx",
        "sheet-with-tables.xlsx",
        "sheet-with-na.xlsx",
        "sheet-null-strings-empty.xlsx",
        "no-header.xlsx",
        "decimal-numbers.xlsx",
        "div0.xlsx",
    ];
    let mut failures: Vec<String> = Vec::new();
    for name in fixtures {
        let path = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name);
        let report = validate_workbook(&path).expect("validate must not throw");
        if !report.is_clean() {
            for f in &report.findings {
                failures.push(format!(
                    "{name}: [{:?}/{:?}] {} {} — {}",
                    f.severity,
                    f.code,
                    f.part,
                    f.location.as_deref().unwrap_or(""),
                    f.message
                ));
            }
        }
    }
    if !failures.is_empty() {
        panic!("clean fixture findings:\n{}", failures.join("\n"));
    }
}

// ---------------------------------------------------------------------------
// MALFORMED CORPUS — specific findings, not "any finding".
// ---------------------------------------------------------------------------

#[test]
fn every_malformed_fixture_reports_at_least_one_finding() {
    let mut any = false;
    for path in malformed_corpus_files() {
        any = true;
        let report = validate_workbook(&path).expect("validate must not throw");
        assert!(
            !report.is_clean(),
            "{} must produce findings",
            path.rsplit(['/', '\\']).next().unwrap()
        );
    }
    assert!(any, "no stress2_malformed_ fixtures found");
}

#[test]
fn malformed_meta_reports_specific_findings() {
    let report = validate_workbook(&testdata("stress2_malformed_meta.xlsx"))
        .expect("validate must not throw");
    let codes: Vec<FindingCode> = report.findings.iter().map(|f| f.code).collect();
    assert!(
        codes.contains(&FindingCode::InvertedColRange),
        "expected inverted col range, got {codes:?}"
    );
    assert!(
        codes.contains(&FindingCode::EmptyValidationSqref),
        "expected empty validation sqref, got {codes:?}"
    );
    assert!(
        codes.contains(&FindingCode::DxfIndexOor),
        "expected dxf index OOR, got {codes:?}"
    );
}

#[test]
fn malformed_chart_reports_unwellformed_chart_xml() {
    let report = validate_workbook(&testdata("stress2_malformed_chart.xlsx"))
        .expect("validate must not throw");
    let hit = report
        .findings
        .iter()
        .find(|f| f.code == FindingCode::XmlNotWellformed && f.part == "xl/charts/chart1.xml");
    assert!(hit.is_some(), "expected unwell-formed chart XML finding");
}

#[test]
fn malformed_pivot_reports_missing_cache_rel() {
    let report = validate_workbook(&testdata("stress2_malformed_pivot.xlsx"))
        .expect("validate must not throw");
    assert!(
        report
            .findings
            .iter()
            .any(|f| f.code == FindingCode::PivotCacheMissingRel),
        "expected pivot cache rel finding"
    );
}

#[test]
fn malformed_threaded_reports_unknown_person() {
    let report = validate_workbook(&testdata("stress2_malformed_threaded.xlsx"))
        .expect("validate must not throw");
    assert!(
        report
            .findings
            .iter()
            .any(|f| f.code == FindingCode::ThreadedUnknownPerson),
        "expected unknown-person finding"
    );
}

#[test]
fn malformed_vba_reports_dangling_rel_and_missing_part() {
    let report = validate_workbook(&testdata("stress2_malformed_vba.xlsx"))
        .expect("validate must not throw");
    let codes: Vec<FindingCode> = report.findings.iter().map(|f| f.code).collect();
    assert!(
        codes.contains(&FindingCode::DanglingRel),
        "expected dangling rel, got {codes:?}"
    );
    assert!(
        codes.contains(&FindingCode::MissingPartForContentType),
        "expected missing part for content type, got {codes:?}"
    );
}

// ---------------------------------------------------------------------------
// REPAIR ROUND-TRIP.
// ---------------------------------------------------------------------------

#[test]
fn repair_roundtrip_meta_removes_repairable_findings() {
    let out = temp_xlsx("repair_meta");
    let (before, actions, wrote) = repair_workbook(
        &testdata("stress2_malformed_meta.xlsx"),
        &out,
        &RepairOptions::default(),
    )
    .expect("repair must not throw");
    assert!(wrote, "package repair must write output");
    assert!(!actions.is_empty(), "expected repair actions");

    let after = validate_workbook(&out).expect("re-validate must not throw");
    let before_codes: Vec<FindingCode> = before.findings.iter().map(|f| f.code).collect();
    let after_codes: Vec<FindingCode> = after.findings.iter().map(|f| f.code).collect();
    // The two repairable warnings are gone; the unrepairable dxf OOR remains.
    assert!(
        !after_codes.contains(&FindingCode::InvertedColRange),
        "{after_codes:?}"
    );
    assert!(
        !after_codes.contains(&FindingCode::EmptyValidationSqref),
        "{after_codes:?}"
    );
    assert!(
        after_codes.contains(&FindingCode::DxfIndexOor),
        "{after_codes:?}"
    );
    // Nothing new appeared.
    for c in &after_codes {
        assert!(
            before_codes.contains(c),
            "repair introduced a new finding code {c:?}"
        );
    }
}

#[test]
fn repair_roundtrip_vba_is_clean_after() {
    let out = temp_xlsx("repair_vba");
    let (before, actions, wrote) = repair_workbook(
        &testdata("stress2_malformed_vba.xlsx"),
        &out,
        &RepairOptions::default(),
    )
    .expect("repair must not throw");
    assert!(wrote);
    assert!(!actions.is_empty());

    let after = validate_workbook(&out).expect("re-validate must not throw");
    assert!(
        after.is_clean(),
        "repaired vba fixture must be clean, got {:?}",
        after.findings.iter().map(|f| f.code).collect::<Vec<_>>()
    );
    assert!(!before.is_clean());
}

#[test]
fn repair_of_clean_file_changes_nothing_contentwise() {
    let out = temp_xlsx("repair_clean");
    let path = testdata("mixed.xlsx");
    let (_before, actions, wrote) =
        repair_workbook(&path, &out, &RepairOptions::default()).expect("repair must not throw");
    assert!(wrote);
    assert!(actions.is_empty(), "clean file must produce zero repairs");

    let after = validate_workbook(&out).expect("re-validate must not throw");
    assert!(after.is_clean(), "repaired clean file must stay clean");

    // Every part's uncompressed content must be identical.
    assert_parts_content_identical(&path, &out);
}

#[test]
fn repair_never_touches_source() {
    let src = testdata("stress2_malformed_meta.xlsx");
    let before = std::fs::read(&src).unwrap();
    let out = temp_xlsx("repair_source");
    repair_workbook(&src, &out, &RepairOptions::default()).expect("repair must not throw");
    let after = std::fs::read(&src).unwrap();
    assert_eq!(before, after, "source file must be unmodified");
}

/// Corpus validation cost vs a plain read, printed for the C2 report.
/// Ignored by default because it reads the 200k-row perf fixtures.
#[test]
#[ignore]
fn report_corpus_validation_cost() {
    use kyrax::turbo::{Features, read_workbook_turbo};
    use std::time::Instant;
    for f in ["mixed.xlsx", "styled.xlsx"] {
        let path = testdata(f);
        let v = || {
            validate_workbook(&path).unwrap();
        };
        let r = || {
            read_workbook_turbo(&path, Features::ALL).unwrap();
        };
        v();
        r();
        let mut bv = u64::MAX;
        let mut br = u64::MAX;
        for _ in 0..3 {
            let t = Instant::now();
            v();
            bv = bv.min(t.elapsed().as_micros() as u64);
            let t = Instant::now();
            r();
            br = br.min(t.elapsed().as_micros() as u64);
        }
        println!(
            "{f}: validate {bv}us, plain read {br}us, ratio {:.2}x",
            bv as f64 / br as f64
        );
    }
}

// ---------------------------------------------------------------------------
// CONTAINER CLASSIFICATION — never report these as generic corruption, and
// validate must return a report (never throw) for any input problem.
// ---------------------------------------------------------------------------

const CFB_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

fn cfb_skeleton(streams: &[&str]) -> Vec<u8> {
    let sect_size = 512usize;
    let mut out = vec![0u8; 512 + 2 * sect_size];
    out[0..8].copy_from_slice(&CFB_MAGIC);
    out[22..24].copy_from_slice(&9u16.to_le_bytes());
    out[24..26].copy_from_slice(&6u16.to_le_bytes());
    out[32..36].copy_from_slice(&1u32.to_le_bytes());
    out[36..40].copy_from_slice(&1u32.to_le_bytes());
    out[44..48].copy_from_slice(&0x1000u32.to_le_bytes());
    out[48..52].copy_from_slice(&0xFFFF_FFFEu32.to_le_bytes());
    out[56..60].copy_from_slice(&0xFFFF_FFFEu32.to_le_bytes());
    out[76..80].copy_from_slice(&0u32.to_le_bytes());
    for i in 1..109 {
        out[76 + i * 4..80 + i * 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    }
    let fat = 512usize;
    out[fat..fat + 4].copy_from_slice(&0xFFFF_FFFEu32.to_le_bytes());
    out[fat + 4..fat + 8].copy_from_slice(&0xFFFF_FFFEu32.to_le_bytes());
    let dir = 1024usize;
    for (i, name) in streams.iter().enumerate() {
        let e = dir + i * 128;
        let units: Vec<u16> = name.encode_utf16().collect();
        for (k, c) in units.iter().enumerate() {
            out[e + k * 2..e + k * 2 + 2].copy_from_slice(&c.to_le_bytes());
        }
        out[e + 64..e + 66].copy_from_slice(&((units.len() * 2) as u16).to_le_bytes());
    }
    out
}

fn write_temp(name: &str, bytes: &[u8]) -> String {
    let mut p = std::env::temp_dir();
    p.push(format!("kyrax_validate_{name}.bin"));
    std::fs::write(&p, bytes).unwrap();
    p.to_str().unwrap().to_string()
}

fn codes_of(report: &kyrax::turbo::ValidateReport) -> Vec<FindingCode> {
    report.findings.iter().map(|f| f.code).collect()
}

#[test]
fn encrypted_cfb_reports_encrypted_workbook() {
    let path = write_temp(
        "encrypted",
        &cfb_skeleton(&["Root Entry", "EncryptionInfo", "EncryptedPackage"]),
    );
    let report = validate_workbook(&path).expect("validate must not throw");
    assert_eq!(
        codes_of(&report),
        vec![FindingCode::EncryptedWorkbook],
        "encrypted workbook must not be reported as corruption"
    );
}

#[test]
fn legacy_biff_reports_legacy_biff() {
    let path = write_temp("biff", &cfb_skeleton(&["Root Entry", "Workbook", "Table"]));
    let report = validate_workbook(&path).expect("validate must not throw");
    assert_eq!(codes_of(&report), vec![FindingCode::LegacyBiff]);
}

#[test]
fn not_spreadsheet_reports_not_spreadsheet() {
    let path = write_temp("text", b"this is a plain text file, not a workbook");
    let report = validate_workbook(&path).expect("validate must not throw");
    assert_eq!(codes_of(&report), vec![FindingCode::NotSpreadsheet]);
}

#[test]
fn valid_zip_but_not_ooxml_reports_not_ooxml() {
    use kyrax::turbo::write::ZipWriter;
    let mut z = ZipWriter::new();
    z.add("hello.txt", b"not a spreadsheet");
    let path = write_temp("notooxml", &z.finish().unwrap());
    let report = validate_workbook(&path).expect("validate must not throw");
    assert!(
        codes_of(&report).contains(&FindingCode::NotOoxmlPackage),
        "got {:?}",
        codes_of(&report)
    );
}

#[test]
fn corrupt_zip_reports_corrupt_zip() {
    let mut bytes = vec![0u8; 128];
    bytes[0..4].copy_from_slice(b"PK\x03\x04");
    bytes[100..104].copy_from_slice(b"PK\x05\x06");
    // Central-directory offset pointing outside the file forces an EOCD error.
    bytes[116..120].copy_from_slice(&0x00FF_FFFFu32.to_le_bytes());
    let path = write_temp("corrupt", &bytes);
    let report = validate_workbook(&path).expect("validate must not throw");
    assert!(
        codes_of(&report).contains(&FindingCode::CorruptZip),
        "got {:?}",
        codes_of(&report)
    );
}

#[test]
fn missing_file_returns_report_not_error() {
    let report = validate_workbook("C:/definitely/not/a/real/file.xlsx")
        .expect("validate must not throw for a missing file");
    assert_eq!(codes_of(&report), vec![FindingCode::Io]);
}

#[test]
fn encrypted_workbook_cannot_be_repaired_and_writes_nothing() {
    let path = write_temp(
        "enc_repair",
        &cfb_skeleton(&["Root Entry", "EncryptionInfo", "EncryptedPackage"]),
    );
    let out = temp_xlsx("enc_out");
    let (report, actions, wrote) =
        repair_workbook(&path, &out, &RepairOptions::default()).expect("repair must not throw");
    assert_eq!(codes_of(&report), vec![FindingCode::EncryptedWorkbook]);
    assert!(actions.is_empty());
    assert!(!wrote, "no output must be written for a non-package input");
    assert!(!std::path::Path::new(&out).exists());
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Inflate every entry of a zip and compare per-part content between two files.
fn assert_parts_content_identical(a: &str, b: &str) {
    use kyrax::turbo::{ArchiveMap, read_entry};
    use std::sync::Arc;
    let az = std::fs::read(a).unwrap();
    let bz = std::fs::read(b).unwrap();
    let am = ArchiveMap::parse(Arc::new(az.clone())).expect("parse a");
    let bm = ArchiveMap::parse(Arc::new(bz.clone())).expect("parse b");
    let bnames: std::collections::HashSet<&str> = bm.entries.keys().map(|s| s.as_str()).collect();
    assert_eq!(am.entries.len(), bm.entries.len(), "entry counts differ");
    for name in am.entries.keys() {
        assert!(
            bnames.contains(name.as_str()),
            "output is missing part {name}"
        );
        let ac = read_entry(&az, name)
            .expect("inflate a")
            .unwrap_or_default();
        let bc = read_entry(&bz, name)
            .expect("inflate b")
            .unwrap_or_default();
        assert_eq!(ac, bc, "content of {} changed", name);
    }
}
