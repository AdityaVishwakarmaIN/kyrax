//! Smoke tests for the turbo fast-path reader against testdata fixtures.
#![cfg(feature = "__arrow")]

use kyrax::turbo::{Features, LinkTarget, read_workbook_turbo};

fn testdata(name: &str) -> String {
    format!("{}/testdata/{}", env!("CARGO_MANIFEST_DIR"), name)
}

#[test]
fn turbo_mixed() {
    let wb = read_workbook_turbo(&testdata("mixed.xlsx"), Features::ALL).expect("read mixed");
    assert_eq!(wb.sheets.len(), 1);
    let s = &wb.sheets[0];
    assert_eq!(s.nrows, 200_000);
    assert_eq!(s.ncols, 15);
    assert_eq!(s.columns.len(), 15);
}

#[test]
fn turbo_styled() {
    let wb = read_workbook_turbo(&testdata("styled.xlsx"), Features::ALL).expect("read styled");
    let s = &wb.sheets[0];
    assert_eq!(s.nrows, 200_000);
    assert_eq!(s.ncols, 12);
    let st = wb.style_table.as_ref().expect("style table");
    assert_eq!(st.xfs.len(), 123, "expected 123 cellXfs");
    assert!(s.style_indices.is_some());
    assert_eq!(s.style_indices.as_ref().unwrap().len(), 12);
}

#[test]
fn turbo_formulas() {
    let wb = read_workbook_turbo(&testdata("formulas.xlsx"), Features::ALL).expect("read formulas");
    let s = &wb.sheets[0];
    assert_eq!(s.nrows, 200_000);
    assert_eq!(s.ncols, 8);
    let f = s.formulas.as_ref().expect("formulas");
    assert!(f.shared_count() > 0, "expected shared-formula records");
    // D3 = sheet row 3 = data row 1, col 3 (0-based). Anchor A2*2 → A3*2.
    let translated = f.translate(1, 3).expect("D3 formula present");
    assert_eq!(translated, "A3*2", "shared formula sibling translation");
}

#[test]
fn turbo_structured() {
    let wb =
        read_workbook_turbo(&testdata("structured.xlsx"), Features::ALL).expect("read structured");
    let s = &wb.sheets[0];
    // openpyxl max_row=161580 (header + data). Honoring sheet `@r` includes gap rows
    // between the dense value block and sparse hyperlink/merge anchors; sequential
    // packing previously under-counted at 150_820.
    assert_eq!(s.nrows, 161_579);
    // Value region is A–J (10 cols); sparse hyperlink/merge anchors place cells out
    // to column X (openpyxl max_column=24). Honoring cell `@r` yields ncols=24
    // (sequential packing previously under-counted at 12).
    assert!(
        s.ncols >= 10,
        "expected at least 10 value columns, got {}",
        s.ncols
    );
    assert_eq!(s.ncols, 24);
    assert_eq!(s.merges.as_ref().map(|m| m.len()), Some(20_000));
    let hlinks = s.hyperlinks.as_ref().expect("hyperlinks");
    assert_eq!(hlinks.len(), 30_000);
    let n_ext = hlinks
        .iter()
        .filter(|h| matches!(h.target, LinkTarget::External(_)))
        .count();
    let n_int = hlinks.len() - n_ext;
    assert_eq!(n_ext, 21_000);
    assert_eq!(n_int, 9_000);
    assert_eq!(wb.defined_names.as_ref().map(|d| d.len()), Some(198));
    assert_eq!(s.tables.as_ref().map(|t| t.len()), Some(20));
}

#[test]
fn turbo_comments() {
    let wb = read_workbook_turbo(&testdata("comments.xlsx"), Features::ALL).expect("read comments");
    let s = &wb.sheets[0];
    assert_eq!(s.nrows, 50_000);
    assert_eq!(s.ncols, 4);
    let c = s.comments.as_ref().expect("comments");
    assert_eq!(c.comments.len(), 30_000);
    assert_eq!(c.authors.len(), 10);
}

#[test]
fn turbo_strings_shared() {
    let wb = read_workbook_turbo(&testdata("strings_shared.xlsx"), Features::ALL)
        .expect("read strings_shared");
    let s = &wb.sheets[0];
    assert_eq!(s.nrows, 200_000);
    assert_eq!(s.ncols, 10);
}

#[test]
fn turbo_selective_flags_skip_work() {
    // Values-only: no styles / formulas / structural.
    let wb = read_workbook_turbo(&testdata("structured.xlsx"), Features::VALUES).expect("values");
    let s = &wb.sheets[0];
    assert_eq!(s.nrows, 161_579);
    assert!(s.style_indices.is_none());
    assert!(s.formulas.is_none());
    assert!(s.merges.is_none());
    assert!(s.hyperlinks.is_none());
    assert!(s.tables.is_none());
    assert!(s.comments.is_none());
    assert!(wb.defined_names.is_none());
    assert!(wb.style_table.is_none());
}
