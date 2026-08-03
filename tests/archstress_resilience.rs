//! A5 architecture-stress Wave-1 baseline: validate / repair / encryption.
//!
//! Wave-1 deliberately runs ONLY clean, non-hostile inputs in-process. Every
//! malformed, hostile, decompression-bomb, Zip64-large, and Excel-COM case is
//! deferred until the A6 exact-PID timeout/RSS coordinator exists; none of
//! those appear here.
//!
//! Coverage implemented (Wave-1, safe clean baselines only):
//!   VAL-01  clean corpus: zero error/warning findings; informational
//!           `FeaturePresent` findings are legitimate and asserted separately.
//!   REP-01  repair a clean file -> validate clean; a second repair is
//!           byte-identical (semantically AND byte stable).
//!   REP-02  repaired output preserves unrelated ZIP members byte-identically
//!           and the source file is never modified.
//!
//! Deferred / `#[ignore]`d (never counted as PASS):
//!   REP-03  hostile non-package refusal — requires A6 exact-PID/RSS guard.
//!   ENC-01  correct-password byte identity — requires F12 encrypted fixtures.
//!   ENC-03  `encryption_info` spin-cap metadata — requires F12 fixtures.
//!   ENC-04  validate/repair classify encrypted — requires F12 fixtures.
//!   ENC-05  KNOWN-GAP probe: encrypted-output authoring is documented absent.

#![cfg(feature = "__arrow")]

use std::io::Read;
use std::path::Path;

use kyrax::turbo::{FindingCode, RepairOptions, Severity, repair_workbook, validate_workbook};

fn testdata(name: &str) -> String {
    format!("{}/testdata/{}", env!("CARGO_MANIFEST_DIR"), name)
}

fn temp_file(tag: &str, ext: &str) -> String {
    let mut p = std::env::temp_dir();
    p.push(format!("kyrax_a5_{tag}_{}.{ext}", std::process::id()));
    p.to_str().unwrap().to_string()
}

/// A non-malformed corpus file: everything in testdata except the
/// `stress2_malformed_*` negative fixtures, which are out of Wave-1 scope.
fn clean_corpus_files() -> Vec<String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("testdata dir") {
        let name = entry
            .expect("entry")
            .file_name()
            .to_str()
            .unwrap()
            .to_string();
        if name.starts_with("stress2_malformed_") {
            continue;
        }
        if name.ends_with(".xlsx") || name.ends_with(".xlsm") {
            files.push(dir.join(name).to_str().unwrap().to_string());
        }
    }
    files.sort();
    files
}

fn assert_zero_error_warning(report: &kyrax::turbo::ValidateReport, label: &str) {
    let unexpected: Vec<String> = report
        .findings
        .iter()
        .filter(|f| f.severity >= Severity::Warning)
        .map(|f| format!("[{:?}/{:?}] {} {}", f.severity, f.code, f.part, f.message))
        .collect();
    assert!(
        unexpected.is_empty(),
        "{label}: expected zero error/warning findings, got:\n{}",
        unexpected.join("\n")
    );
}

// ---------------------------------------------------------------------------
// VAL-01: clean corpus, no false positives.
// ---------------------------------------------------------------------------

#[test]
fn clean_testdata_corpus_has_zero_error_or_warning_findings() {
    let files = clean_corpus_files();
    assert!(!files.is_empty(), "clean corpus must not be empty");
    let mut info_findings = 0usize;
    for path in files {
        let name = path.rsplit(['/', '\\']).next().unwrap().to_string();
        let report = validate_workbook(&path).expect("validate must never throw");
        assert_zero_error_warning(&report, &name);
        for f in &report.findings {
            if f.severity == Severity::Info {
                // Legitimate informational finding: must be the FeaturePresent
                // inventory code (anything else on a clean file is a false pos).
                assert_eq!(
                    f.code,
                    FindingCode::FeaturePresent,
                    "{name}: unexpected Info finding {f:?}"
                );
                info_findings += 1;
            }
        }
    }
    eprintln!("A5 VAL-01: clean corpus swept; Info FeaturePresent findings seen: {info_findings}");
}

// ---------------------------------------------------------------------------
// REP-01 / REP-02 / REP-03: conservative repair on clean input.
// ---------------------------------------------------------------------------

#[test]
fn repair_clean_is_validate_clean_and_second_repair_is_byte_stable() {
    let src = testdata("mixed.xlsx");
    let out1 = temp_file("repair1", "xlsx");
    let out2 = temp_file("repair2", "xlsx");

    let (report1, actions1, did_work1) = repair_workbook(&src, &out1, &RepairOptions::default())
        .expect("repair must not throw on a clean file");
    assert_zero_error_warning(&report1, "post-repair validate");
    assert!(did_work1, "a readable package reports did_work=true");
    let _ = actions1;

    let report2 = validate_workbook(&out1).expect("repaired output must validate");
    assert_zero_error_warning(&report2, "repaired output validate");

    let (_, actions2, did_work2) = repair_workbook(&out1, &out2, &RepairOptions::default())
        .expect("second repair must not throw");
    assert!(did_work2);
    let _ = actions2;

    let b1 = std::fs::read(&out1).unwrap();
    let b2 = std::fs::read(&out2).unwrap();
    assert_eq!(
        b1, b2,
        "REP-01: a second repair must be byte-stable (semantically AND byte stable)"
    );

    let _ = std::fs::remove_file(&out1);
    let _ = std::fs::remove_file(&out2);
}

#[test]
fn repair_never_touches_source_and_preserves_unrelated_members_byte_identically() {
    let src = testdata("mixed.xlsx");
    let src_before = std::fs::read(&src).unwrap();
    let out = temp_file("repair_preserve", "xlsx");

    let (report, _, _) =
        repair_workbook(&src, &out, &RepairOptions::default()).expect("repair must not throw");
    assert_zero_error_warning(&report, "preserve test");

    let src_after = std::fs::read(&src).unwrap();
    assert_eq!(
        src_before, src_after,
        "REP-02: source must never be modified"
    );

    // Every member present in the source must survive byte-identically.
    let mut src_zip =
        zip::ZipArchive::new(std::io::Cursor::new(&src_before)).expect("source parses as zip");
    let mut out_zip = zip::ZipArchive::new(std::io::Cursor::new(std::fs::read(&out).unwrap()))
        .expect("repaired output parses as zip");
    assert_eq!(
        src_zip.len(),
        out_zip.len(),
        "entry count must be preserved by conservative repair"
    );
    // Collect owned names first: `by_name` borrows the archive mutably, so the
    // names must not borrow from the archive while the loop calls it.
    let names: Vec<String> = src_zip.file_names().map(str::to_owned).collect();
    for name in names {
        let mut a = Vec::new();
        let mut b = Vec::new();
        src_zip
            .by_name(&name)
            .expect("source entry")
            .read_to_end(&mut a)
            .unwrap();
        out_zip
            .by_name(&name)
            .expect("repaired entry must exist")
            .read_to_end(&mut b)
            .unwrap();
        assert_eq!(
            a, b,
            "REP-02: unrelated member {name} must be byte-identical"
        );
    }

    let _ = std::fs::remove_file(&out);
}

/// REP-03 hostile refusal is DEFERRED: a deliberately non-package input is
/// malformed/hostile and must run under the A6 exact-PID timeout/RSS guard,
/// not in-process.
#[test]
#[ignore = "requires A6 exact-PID timeout/RSS guard; non-package input is hostile (REP-03 deferred)"]
fn repair_refuses_non_package_without_writing_anything() {
    let non_package = temp_file("not_a_package", "bin");
    let out = temp_file("repair_refuse", "bin");
    std::fs::write(
        &non_package,
        b"this is definitely not a spreadsheet package",
    )
    .expect("write garbage fixture");

    let (report, actions, did_work) =
        repair_workbook(&non_package, &out, &RepairOptions::default())
            .expect("repair returns a report, never throws, for a non-package input");
    assert!(
        !did_work,
        "REP-03: non-package input must not be repairable"
    );
    assert!(
        actions.is_empty(),
        "REP-03: no repair actions for a non-package"
    );
    assert!(
        !report.is_clean(),
        "a non-package must carry a classification finding"
    );
    assert!(
        !Path::new(&out).exists(),
        "REP-03: nothing may be written for an unrepairable input"
    );

    let _ = std::fs::remove_file(&non_package);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn repair_options_filters_apply_without_error_on_clean_input() {
    let src = testdata("styled.xlsx");
    let out = temp_file("repair_filtered", "xlsx");

    let opts = RepairOptions {
        max_severity: Severity::Error,
        allowed_codes: Some(vec![FindingCode::XmlNotWellformed]),
        ..Default::default()
    };
    let (report, actions, did_work) =
        repair_workbook(&src, &out, &opts).expect("filtered repair must not throw");
    let _ = report;
    assert!(did_work);
    assert!(
        actions.is_empty(),
        "REP-03: a clean file must produce no actions under any filter"
    );
    let _ = std::fs::remove_file(&out);
}

// ---------------------------------------------------------------------------
// ENC-01 / ENC-03 / ENC-04 / ENC-05: encrypted packages.
//
// The committed C1c encrypted fixtures live OUTSIDE the repo in
// `%TEMP%/kyrax_c1c_fixtures` and are generated by the C1c generator script
// (tests/encrypted.rs documents regeneration). F12 is not present in Wave-1,
// so every fixture-dependent ENC test is `#[ignore]`d — an absent fixture must
// never count as a passing test. ENC-05 is a KNOWN-GAP probe and is likewise
// ignored so the documented gap cannot inflate the PASS count.
// ---------------------------------------------------------------------------

#[cfg(feature = "encryption")]
mod encryption {
    use kyrax::turbo::crypto::{decrypt_workbook, encryption_info};
    use kyrax::turbo::{FindingCode, repair_workbook, validate_workbook};

    const FIXTURE_DIR: &str = "kyrax_c1c_fixtures";
    const PASSWORD: &str = "Password1234_";

    fn fixture(name: &str) -> Vec<u8> {
        let p = std::env::temp_dir().join(FIXTURE_DIR).join(name);
        std::fs::read(&p).unwrap_or_else(|e| {
            panic!(
                "missing encrypted fixture {} ({e}): regenerate with the C1c generator \
                 into %TEMP%/{FIXTURE_DIR} before running this ignored test with --ignored",
                p.display()
            )
        })
    }

    #[test]
    #[ignore = "requires F12 encrypted fixtures (C1c generator) + A6 guards; deferred, not implemented PASS"]
    fn correct_password_decrypts_byte_identical_when_fixtures_present() {
        let plain = fixture("plain_big.xlsx");
        let enc = fixture("big_encrypted.bin");
        let out = decrypt_workbook(&enc, PASSWORD)
            .expect("correct password must decrypt the committed fixture");
        assert_eq!(
            out, plain,
            "ENC-01: decrypt must reproduce the plaintext package byte-for-byte"
        );
        assert!(
            out.starts_with(b"PK\x03\x04"),
            "decrypted output must be a zip"
        );
    }

    #[test]
    #[ignore = "requires F12 encrypted fixtures (C1c generator) + A6 guards; deferred, not implemented PASS"]
    fn encryption_info_reports_bounded_spin_count_when_fixtures_present() {
        let enc = fixture("big_encrypted.bin");
        let meta = encryption_info(&enc).expect("metadata needs no password");
        assert_eq!(meta.scheme, "agile");
        assert_eq!(meta.cipher_algorithm, "AES");
        assert_eq!(meta.hash_algorithm, "SHA512");
        assert_eq!(meta.key_bits, 256);
        assert!(
            meta.spin_count <= 10_000_000,
            "ENC-03: spin count {} must not exceed the 10,000,000 cap",
            meta.spin_count
        );
        assert_eq!(
            meta.spin_count, 100_000,
            "committed fixture uses spinCount 100k"
        );
    }

    #[test]
    #[ignore = "requires F12 encrypted fixtures (C1c generator) + A6 guards; deferred, not implemented PASS"]
    fn validate_and_repair_classify_encrypted_and_refuse_without_modifying_input() {
        let enc = fixture("big_encrypted.bin");
        let p = std::env::temp_dir()
            .join(FIXTURE_DIR)
            .join(format!("a5_enc_classify_{}.bin", std::process::id()));
        std::fs::write(&p, &enc).expect("write temp encrypted copy");
        let before = std::fs::read(&p).unwrap();

        let report = validate_workbook(&p.to_str().unwrap())
            .expect("validate must never throw on an encrypted package");
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.code == FindingCode::EncryptedWorkbook),
            "ENC-04: encrypted package must be classified EncryptedWorkbook: {:?}",
            report.findings
        );

        let out = std::env::temp_dir()
            .join(FIXTURE_DIR)
            .join(format!("a5_enc_classify_out_{}.bin", std::process::id()));
        let (r2, actions, did_work) = repair_workbook(
            &p.to_str().unwrap(),
            &out.to_str().unwrap(),
            &kyrax::turbo::RepairOptions::default(),
        )
        .expect("repair must not throw on an encrypted package");
        assert!(!did_work, "ENC-04: repair must refuse an encrypted package");
        assert!(actions.is_empty());
        let _ = r2;
        assert!(
            !out.exists(),
            "ENC-04: repair must write nothing for encrypted input"
        );

        assert_eq!(
            before,
            std::fs::read(&p).unwrap(),
            "ENC-04: input bytes must be unchanged"
        );
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(&out);
    }

    /// ENC-05 — KNOWN-GAP probe (documentation only, not implementation):
    /// encrypted-OUTPUT authoring (save a workbook encrypted with a password)
    /// does not exist in the current public API (`save_workbook` has no
    /// password parameter). Ignored so the documented gap never inflates the
    /// PASS count; it exists only to make the gap visible in `--ignored` runs.
    #[test]
    #[ignore = "KNOWN-GAP: encrypted output authoring absent from the public API"]
    fn encrypted_output_authoring_is_a_documented_known_gap() {
        eprintln!(
            "A5 ENC-05 KNOWN-GAP: encrypted-output authoring is absent from the public \
             API (save_workbook accepts no password). Probe only; no implementation in A5."
        );
    }
}
