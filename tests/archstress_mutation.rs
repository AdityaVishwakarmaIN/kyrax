//! A3 Wave-1 mutation/refusal invariants over public Rust APIs.

use std::borrow::Cow;

use kyrax::turbo::mutate::{delete_rows, insert_cols, insert_rows, move_range};

const SHEET: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<worksheet><dimension ref="A1:B2"/><sheetData>
<row r="1"><c r="A1"><v>1</v></c><c r="B1"><f>A1</f><v>1</v></c></row>
<row r="2"><c r="A2"><v>2</v></c></row>
</sheetData><mergeCells count="1"><mergeCell ref="A1:B1"/></mergeCells></worksheet>"#;

const EDGE_COL: &[u8] = br#"<worksheet><dimension ref="XFD1"/><sheetData>
<row r="1"><c r="XFD1"><v>1</v></c></row>
</sheetData></worksheet>"#;

fn owned(result: Option<Cow<'_, [u8]>>) -> Vec<u8> {
    result.expect("operation should succeed").into_owned()
}

#[test]
fn a3_insert_rows_moves_cells_and_dimension() {
    let out = String::from_utf8(owned(insert_rows(SHEET, 1, 1))).unwrap();
    assert!(out.contains("dimension ref=\"A2:B3\""));
    assert!(out.contains("<row r=\"2\">"));
    assert!(out.contains("<c r=\"A3\""));
}

#[test]
fn a3_insert_cols_moves_cells_without_wraparound() {
    let out = String::from_utf8(owned(insert_cols(SHEET, 1, 1))).unwrap();
    assert!(out.contains("dimension ref=\"B1:C2\""));
    assert!(out.contains("<c r=\"B1\""));
    assert!(out.contains("<c r=\"C1\""));
}

#[test]
fn a3_delete_band_removes_rows_and_shrinks_dimension() {
    let out = String::from_utf8(owned(delete_rows(SHEET, 1, 1))).unwrap();
    assert!(out.contains("dimension ref=\"A1:B1\""));
    assert!(out.contains("<c r=\"A1\"><v>2</v></c>"));
}

#[test]
fn a3_move_range_translates_formula_when_requested() {
    let out = String::from_utf8(owned(move_range(SHEET, 1, 1, 1, 2, 1, 0, true))).unwrap();
    assert!(out.contains("<c r=\"B2\"><f>A2</f>"));
}

#[test]
fn a3_boundary_and_malformed_operations_refuse() {
    assert!(insert_rows(SHEET, 0, 1).is_none());
    assert!(insert_cols(EDGE_COL, 16_384, 1).is_none());
    assert!(move_range(SHEET, 1_048_576, 1, 1_048_576, 1, 1, 0, false).is_none());
    assert!(insert_rows(b"<worksheet/>", 1, 1).is_none());
}
