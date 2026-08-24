//! Round-trip integration tests for dynamic array spills and #SPILL! errors
//! through the buffered workbook writer.
//!
//! These prove the plan's Edit-4 requirement: after `hydrate_workbook`, the
//! materialized spill values AND the anchor's `#SPILL!` error cache survive a
//! write -> read cycle through the real writer/reader, and `Sheet.spills` (live
//! ownership state) is NOT serialized into the package.

use arrow_array::{Array, Float64Array, StringArray};
use kyrax::turbo::calc::{hydrate_workbook, CalcOptions};
use kyrax::turbo::write::{
    write_workbook_bytes, Cell, CellValue, FormulaKind, Row, Sheet, Workbook,
};
use kyrax::turbo::Features;
use pretty_assertions::assert_eq;

#[test]
fn spill_workbook_buffered_round_trip() {
    let mut wb = Workbook::new();
    let mut sheet = Sheet::new("Spills");

    // A1 = SEQUENCE(3): anchor at A1, spills A2 & A3 (values 2, 3; A1 = 1).
    // B1 = SEQUENCE(2): blocked by the user value 99.0 at B2 -> B1 caches #SPILL!.
    let mut r1 = Row::new(1);
    r1.cells = vec![
        Cell::new(
            1,
            CellValue::Formula {
                text: "=SEQUENCE(3)".to_string(),
                kind: FormulaKind::Normal,
                cached: None,
            },
        ),
        Cell::new(
            2,
            CellValue::Formula {
                text: "=SEQUENCE(2)".to_string(),
                kind: FormulaKind::Normal,
                cached: None,
            },
        ),
    ];

    let mut r2 = Row::new(2);
    r2.cells = vec![Cell::new(2, CellValue::Number(99.0))];

    sheet.rows = vec![r1, r2];
    wb.sheets = vec![sheet];

    // Hydrate workbook: A1 spill succeeds, B1 blocked.
    let rep = hydrate_workbook(&mut wb, &CalcOptions::default());
    assert_eq!(rep.computed, 1, "{rep:?}");
    assert_eq!(rep.error_cells, 1, "{rep:?}");
    assert_eq!(wb.sheets[0].spills.len(), 1, "anchor A1 owns one spill region");

    // Save to bytes via buffered writer.
    let bytes = write_workbook_bytes(&wb).expect("write_workbook_bytes");
    assert!(!bytes.is_empty());

    // Push through the real reader.
    let tmp_dir = std::env::temp_dir();
    let file_path = tmp_dir.join("test_spill_roundtrip.xlsx");
    let file_path_str = file_path.to_str().expect("valid path");
    std::fs::write(&file_path, &bytes).expect("write temp file");

    let read_wb = kyrax::turbo::read_workbook_turbo_sheet_with_options(
        file_path_str,
        Features::ALL,
        0,
        None,
        None,
    )
    .expect("read_workbook_turbo");
    assert_eq!(read_wb.sheets.len(), 1);
    let s = &read_wb.sheets[0];

    // Dimensions preserved after the round trip.
    assert_eq!(s.nrows, 3, "A-spill adds rows 2 & 3");
    assert_eq!(s.ncols, 2, "columns A and B");

    // The published #SPILL! error cache must survive.
    let spill_errors: Vec<_> = s
        .cell_errors
        .iter()
        .filter(|e| e.code == "#SPILL!")
        .collect();
    assert_eq!(spill_errors.len(), 1, "exactly one #SPILL! in {:#?}", s.cell_errors);
    assert_eq!((spill_errors[0].row, spill_errors[0].col), (0, 1), "error at B1 (0-based)");

    // The materialized spill values must survive (column A: 1, 2, 3).
    let a_col = &s.columns[0];
    assert!(a_col.is_null(0) == false, "A1 holds the anchor value");
    let arr = a_col
        .as_any()
        .downcast_ref::<Float64Array>()
        .expect("column A should be Float64");
    assert!((arr.value(0) - 1.0).abs() < f64::EPSILON, "A1 = 1, got {}", arr.value(0));
    assert!((arr.value(1) - 2.0).abs() < f64::EPSILON, "A2 = 2, got {}", arr.value(1));
    assert!((arr.value(2) - 3.0).abs() < f64::EPSILON, "A3 = 3, got {}", arr.value(2));

    // The user obstacle at B2 must be preserved. Column B is Utf8 because the
    // `#SPILL!` error string lives in B1 and the reader upcasts the mixed column.
    let b_col = s.columns[1]
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("column B should be Utf8 (mixed #SPILL! error + number)");
    assert_eq!(b_col.value(1), "99.0", "B2 = 99 preserved through the round trip");

    let _ = std::fs::remove_file(&file_path);
}
