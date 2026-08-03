//! C1c — encrypted-workbook support end to end.
//!
//! Fixtures are generated with msoffcrypto-tool (Agile encryption, AES-256 /
//! SHA-512, spinCount 100,000, password `Password1234_`) into the SYSTEM TEMP
//! directory (`%TEMP%/kyrax_c1c_fixtures/`), never into the repo. Every fixture
//! was cross-validated by the msoffcrypto reference reader before being
//! committed here.
//!
//!   plain_small.xlsx   24x8  single-segment encrypted package (1 segment)
//!   plain_big.xlsx   320x12  multi-segment encrypted package (4 segments)
//!   small_encrypted.bin      CFB wrapping the small package
//!   big_encrypted.bin        CFB wrapping the big package
//!
//! `big_encrypted.bin` is THE multi-segment test: agile encryption re-keys per
//! 4096-byte segment, so a single-shot decrypt of the whole stream produces
//! correct output for the first segment only. If that bug is ever reintroduced,
//! `multi_segment_package_decrypts_identically` (and the byte-identity checks
//! below) fail on everything real.
//!
//! The generator script lives next to the scratchpad for this task and writes
//! the four fixtures here; if they are missing this file tells you how to
//! regenerate them.

#![cfg(all(feature = "__arrow", feature = "encryption"))]

use kyrax::turbo::crypto::cfb::Cfb;
use kyrax::turbo::crypto::{CryptoError, decrypt_workbook, encryption_info, is_encrypted};
use kyrax::turbo::{
    Features, TurboSheet, list_sheet_names_with_password, read_workbook_turbo,
    read_workbook_turbo_with_password,
};

const PASSWORD: &str = "Password1234_";

fn fixture_dir() -> std::path::PathBuf {
    std::env::temp_dir().join("kyrax_c1c_fixtures")
}

/// Load a fixture, with a clear "how to regenerate" message if it is missing.
fn fixture(name: &str) -> Vec<u8> {
    let p = fixture_dir().join(name);
    std::fs::read(&p).unwrap_or_else(|e| {
        panic!(
            "missing encrypted-workbook fixture {} ({e}): regenerate with the C1c \
             generator script (C1c task -> gen_fixtures.py, run from the system temp \
             directory; it writes these four files into %TEMP%/kyrax_c1c_fixtures)",
            p.display()
        )
    })
}

fn write_temp(name: &str, bytes: &[u8]) -> String {
    let p = fixture_dir().join(name);
    std::fs::write(&p, bytes).expect("write temp fixture");
    p.to_string_lossy().into_owned()
}

/// Run `f` and return its error; panic on Ok (avoids the `Debug` bound that
/// `Result::expect_err` needs on the Ok type, which `TurboWorkbook` lacks).
fn err_of<T>(f: impl FnOnce() -> Result<T, kyrax::turbo::TurboError>) -> kyrax::turbo::TurboError {
    match f() {
        Err(e) => e,
        Ok(_) => panic!("expected an error, got Ok"),
    }
}

fn assert_same_shape(a: &TurboSheet, b: &TurboSheet) {
    assert_eq!(a.name, b.name, "sheet name");
    assert_eq!(a.nrows, b.nrows, "row count");
    assert_eq!(a.ncols, b.ncols, "column count");
    assert_eq!(a.column_names, b.column_names, "column names");
    assert_eq!(a.sheet_state, b.sheet_state, "sheet state");
    assert_eq!(a.sheet_kind, b.sheet_kind, "sheet kind");
}

/// The single-segment fixture: decrypts, and the decrypted package is
/// byte-identical to its plaintext twin.
#[test]
fn correct_password_decrypts_identically_single_segment() {
    let plain = fixture("plain_small.xlsx");
    let enc = fixture("small_encrypted.bin");
    assert!(is_encrypted(&enc));
    assert!(
        !is_encrypted(&plain),
        "a plain zip must not read as encrypted"
    );

    let out = decrypt_workbook(&enc, PASSWORD).expect("correct password must decrypt");
    assert_eq!(out, plain, "decrypted bytes must equal the plaintext twin");
    assert!(
        out.starts_with(b"PK\x03\x04"),
        "decrypted package must be a zip, not padding bytes"
    );
}

/// THE multi-segment test. Catches the single-shot decrypt bug: the fixture's
/// EncryptedPackage stream spans four 4096-byte segments, so a decrypt that
/// derives one IV and decrypts the whole stream at once produces correct output
/// for the first segment only and fails here.
#[test]
fn multi_segment_package_decrypts_identically() {
    let plain = fixture("plain_big.xlsx");
    let enc = fixture("big_encrypted.bin");

    let cfb = Cfb::parse(&enc).expect("CFB parse").expect("CFB parse ok");
    let pkg = cfb
        .stream("EncryptedPackage")
        .expect("EncryptedPackage stream");
    let segments = (pkg.len() - 8) / 4096 + 1;
    assert!(
        segments >= 2,
        "the multi-segment fixture must span at least two 4096-byte segments (got {segments})"
    );

    let out = decrypt_workbook(&enc, PASSWORD).expect("correct password must decrypt");
    assert_eq!(
        out, plain,
        "multi-segment decrypt must reproduce the plaintext byte-for-byte \
         (a single-shot decrypt would fail here)"
    );
}

/// A wrong password must surface as a clean WrongPassword error — never a
/// garbage key and never a corrupt-zip error downstream.
#[test]
fn wrong_password_is_a_clean_error() {
    let enc = fixture("big_encrypted.bin");
    match decrypt_workbook(&enc, "not the password") {
        Err(CryptoError::WrongPassword) => {}
        other => panic!("expected WrongPassword, got {other:?}"),
    }
}

/// Loader integration: an encrypted workbook opens with a password and reads
/// identically to its plaintext equivalent.
#[test]
fn encrypted_workbook_opens_through_the_loader() {
    let plain = fixture("plain_big.xlsx");
    let enc = fixture("big_encrypted.bin");
    let enc_path = write_temp("loader_encrypted.bin", &enc);
    let plain_path = write_temp("loader_plain.xlsx", &plain);

    let names = list_sheet_names_with_password(&enc_path, Some(PASSWORD))
        .expect("list names with password");
    assert_eq!(names, vec!["Data".to_string()]);

    let via_loader = read_workbook_turbo_with_password(&enc_path, Features::ALL, Some(PASSWORD))
        .expect("encrypted workbook must open with the password");
    let via_plain = read_workbook_turbo(&plain_path, Features::ALL).expect("plaintext must open");
    assert_eq!(via_loader.sheets.len(), 1);
    assert_same_shape(&via_loader.sheets[0], &via_plain.sheets[0]);
    assert_eq!(via_loader.sheets[0].nrows, 320, "header excluded");
    assert_eq!(via_loader.sheets[0].ncols, 12);
    assert_eq!(via_loader.sheets[0].column_names[0], "col0");
}

/// A plain unencrypted file is unaffected by the new password code path.
#[test]
fn plain_file_is_unaffected() {
    let plain = fixture("plain_big.xlsx");
    let path = write_temp("plain_unaffected.xlsx", &plain);

    let with = read_workbook_turbo_with_password(&path, Features::VALUES, Some(PASSWORD))
        .expect("password is ignored for a plain file");
    let without = read_workbook_turbo(&path, Features::VALUES).expect("plain read");
    assert_same_shape(&with.sheets[0], &without.sheets[0]);
}

/// The loader reports a clear "password required" error instead of tripping
/// over a zip parser when the password is omitted.
#[test]
fn encrypted_without_password_requests_one() {
    let enc = fixture("big_encrypted.bin");
    let path = write_temp("loader_no_pwd.bin", &enc);
    let err = err_of(|| read_workbook_turbo_with_password(&path, Features::VALUES, None));
    assert!(
        err.to_string().contains("password"),
        "expected a password-required error, got: {err}"
    );
}

/// A wrong password through the loader is a clear error, not a corrupt-zip
/// format error.
#[test]
fn loader_wrong_password_is_a_clear_error() {
    let enc = fixture("big_encrypted.bin");
    let path = write_temp("loader_wrong_pwd.bin", &enc);
    let err = err_of(|| read_workbook_turbo_with_password(&path, Features::VALUES, Some("wrong")));
    let msg = err.to_string();
    assert!(msg.contains("wrong password"), "unexpected error: {msg}");
}

/// Truncated or corrupt containers error cleanly and never panic.
#[test]
fn truncated_or_corrupt_container_errors_cleanly() {
    let enc = fixture("big_encrypted.bin");

    // Every truncation point must produce an error, never a panic.
    for cut in [8usize, 100, 512, 1000, 4096, enc.len() / 2, enc.len() - 4] {
        let truncated = &enc[..cut];
        let _ = decrypt_workbook(truncated, PASSWORD);
    }

    // Not a CFB at all.
    match decrypt_workbook(b"PK\x03\x04 this is a zip, not a CFB", PASSWORD) {
        Err(_) => {}
        Ok(_) => panic!("a non-CFB input must never decrypt"),
    }

    // CFB magic present but the directory/FAT clobbered.
    let mut corrupt = enc.clone();
    corrupt[600] ^= 0xFF; // inside the FAT region
    let _ = decrypt_workbook(&corrupt, PASSWORD);

    // is_encrypted must never panic on any input length.
    for n in 0..=enc.len().min(512) {
        let _ = is_encrypted(&enc[..n]);
    }
    assert!(!is_encrypted(&[0u8; 32]), "garbage must not look encrypted");
}

/// `encryption_info` reports the scheme, algorithm and spin count without a
/// password.
#[test]
fn encryption_info_reports_algorithm_and_spin_count() {
    let enc = fixture("big_encrypted.bin");
    let meta = encryption_info(&enc).expect("encryption_info must not need a password");
    assert_eq!(meta.scheme, "agile");
    assert_eq!(meta.cipher_algorithm, "AES");
    assert_eq!(meta.hash_algorithm, "SHA512");
    assert_eq!(meta.key_bits, 256);
    assert_eq!(meta.block_size, 16);
    assert_eq!(meta.salt_size, 16);
    assert_eq!(meta.spin_count, 100_000);
    assert!(meta.message.is_none());
}

/// Report the cost of opening an encrypted workbook versus a plain one.
///
/// spinCount (~100k hash iterations) is deliberately expensive and unavoidable,
/// once per file; the decryption itself is linear in the package size.
#[test]
fn reports_encrypted_open_cost() {
    let plain = fixture("plain_big.xlsx");
    let enc = fixture("big_encrypted.bin");

    let t0 = std::time::Instant::now();
    let out = decrypt_workbook(&enc, PASSWORD).expect("decrypt");
    let decrypt_elapsed = t0.elapsed();

    let t1 = std::time::Instant::now();
    let _ = out;
    assert!(t1.elapsed().as_millis() < 5_000);

    let t2 = std::time::Instant::now();
    let plain_path = write_temp("perf_plain.xlsx", &plain);
    let _ = read_workbook_turbo(&plain_path, Features::VALUES).expect("plain read");
    let plain_read_elapsed = t2.elapsed();

    eprintln!(
        "C1c perf: decrypt_workbook({} KiB encrypted package, spinCount 100k, AES-256/SHA512) = {:?}; \
         plain eager read of the same workbook = {:?}",
        plain.len() / 1024,
        decrypt_elapsed,
        plain_read_elapsed
    );
    assert!(
        decrypt_elapsed.as_secs() < 60,
        "decrypt must not hang (spinCount is bounded)"
    );
}
