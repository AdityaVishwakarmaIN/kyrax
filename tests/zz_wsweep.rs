//! Regression: the turbo WRITE path must serialize deterministically and
//! round-trip through the reader cell-for-cell. Formerly an ad-hoc profiling
//! sweep; now a bounded, non-panicking regression test. The original
//! write-path perf sweep survives as an opt-in `#[ignore]`d report, following
//! the `report_corpus_validation_cost` convention in turbo_validate.rs.
#![cfg(feature = "__arrow")]

use pretty_assertions::assert_eq;
use std::time::Instant;

use arrow_array::types::Int32Type;
use arrow_array::{Array, DictionaryArray, Float64Array, StringArray};
use kyrax::turbo::write::{Cell, CellValue, FormulaKind, Row, Workbook, write_workbook_bytes};
use kyrax::turbo::{Features, read_workbook_turbo_sheet};

fn build(rows: usize, cols: usize, mixed: bool, formulas: bool) -> Workbook {
    let mut wb = Workbook::with_sheet("S");
    let s = &mut wb.sheets[0];
    for r in 0..rows {
        let mut row = Row::new(r as u32 + 1);
        for c in 0..cols {
            let v = if formulas && c == cols - 1 {
                CellValue::Formula {
                    text: format!("SUM(A{}:B{})", r + 1, r + 1),
                    kind: FormulaKind::Normal,
                    cached: None,
                }
            } else if mixed && (r + c) % 3 == 0 {
                CellValue::Str(format!("w{r}_{c}"))
            } else {
                CellValue::Number(r as f64 + c as f64 / 10.0)
            };
            row.cells.push(Cell::new(c as u32 + 1, v));
        }
        s.rows.push(row);
    }
    wb
}

fn write_temp_xlsx(name: &str, bytes: &[u8]) -> String {
    let mut p = std::env::temp_dir();
    p.push(format!("kyrax_wsweep_{name}.xlsx"));
    std::fs::write(&p, bytes).unwrap();
    p.to_str().unwrap().to_string()
}

#[derive(Clone, PartialEq, Debug)]
enum CellVal {
    Num(f64),
    Str(String),
}

/// One cell of the write model.
fn expected_cell(r: usize, c: usize, mixed: bool, formulas: bool, cols: usize) -> Option<CellVal> {
    if formulas && c == cols - 1 {
        None
    } else if mixed && (r + c) % 3 == 0 {
        Some(CellVal::Str(format!("w{r}_{c}")))
    } else {
        Some(CellVal::Num(r as f64 + c as f64 / 10.0))
    }
}

/// One read-back cell, classified honestly: a real float column yields `Num`;
/// a string-typed column yields its raw text (`Text`), never a guessed parse.
#[derive(Clone, PartialEq, Debug)]
enum ReadVal {
    Num(f64),
    Text(String),
}

fn read_cell(col: &dyn Array, i: usize) -> Option<ReadVal> {
    if col.is_null(i) {
        return None;
    }
    if let Some(a) = col.as_any().downcast_ref::<Float64Array>() {
        return Some(ReadVal::Num(a.value(i)));
    }
    if let Some(a) = col.as_any().downcast_ref::<StringArray>() {
        return Some(ReadVal::Text(a.value(i).to_string()));
    }
    if let Some(a) = col.as_any().downcast_ref::<DictionaryArray<Int32Type>>() {
        let key = a.keys().value(i) as usize;
        let values = a.values().as_any().downcast_ref::<StringArray>().unwrap();
        return Some(ReadVal::Text(values.value(key).to_string()));
    }
    None
}

/// Equivalent values pass ("124.0" read as text equals the model's 124), but
/// a number/string swap can never pass: each side is classified on its own
/// merits and only the exact same kind compares.
fn cell_matches(want: &Option<CellVal>, read: &Option<ReadVal>) -> bool {
    match (want, read) {
        (None, None) => true,
        (Some(CellVal::Num(w)), Some(ReadVal::Num(r))) => w == r,
        (Some(CellVal::Num(w)), Some(ReadVal::Text(t))) => {
            t.parse::<f64>().map(|n| n == *w).unwrap_or(false)
        }
        (Some(CellVal::Str(w)), Some(ReadVal::Text(t))) => w == t,
        _ => false,
    }
}

fn check_roundtrip(
    label: &str,
    rows: usize,
    cols: usize,
    mixed: bool,
    formulas: bool,
    findings: &mut Vec<String>,
) {
    let wb = build(rows, cols, mixed, formulas);
    let bytes = match write_workbook_bytes(&wb) {
        Ok(b) => b,
        Err(e) => {
            findings.push(format!("[{label}] write failed: {e}"));
            return;
        }
    };
    let path = write_temp_xlsx(label, &bytes);
    let rb = match read_workbook_turbo_sheet(&path, Features::VALUES, 0) {
        Ok(wb) => wb,
        Err(e) => {
            findings.push(format!("[{label}] read-back failed: {e}"));
            return;
        }
    };
    let sheet = &rb.sheets[0];

    // The reader treats sheet row 1 as the header, so the first model row is
    // consumed as column names and the data grid holds rows..rows-1.
    if sheet.nrows != rows.saturating_sub(1) {
        findings.push(format!(
            "[{label}] read-back nrows {} != expected {}",
            sheet.nrows,
            rows.saturating_sub(1)
        ));
    }
    if sheet.ncols != cols {
        findings.push(format!(
            "[{label}] read-back ncols {} != expected {cols}",
            sheet.ncols
        ));
    }

    for r in 0..sheet.nrows {
        let model_row = r + 1;
        for c in 0..cols {
            let Some(col) = sheet.columns.get(c) else {
                continue;
            };
            if r >= col.len() {
                findings.push(format!("[{label}] col {c} shorter than nrows"));
                continue;
            }
            let got = read_cell(col.as_ref(), r);
            let want = expected_cell(model_row, c, mixed, formulas, cols);
            if !cell_matches(&want, &got) {
                findings.push(format!(
                    "[{label}] cell (r{model_row},c{c}): read {got:?} != expected {want:?}"
                ));
            }
        }
    }
}

#[test]
fn write_roundtrip_numeric_mixed_and_formula_modes() {
    let mut findings = Vec::new();
    for (label, mixed, fx) in [
        ("numeric", false, false),
        ("mixed", true, false),
        ("mixed+formula", true, true),
    ] {
        check_roundtrip(label, 200, 8, mixed, fx, &mut findings);
    }
    assert!(
        findings.is_empty(),
        "write round-trip mismatches:\n{}",
        findings.join("\n")
    );
}

#[test]
fn write_is_deterministic() {
    let wb = build(200, 8, true, true);
    let a = write_workbook_bytes(&wb).expect("first write");
    let b = write_workbook_bytes(&wb).expect("second write");
    assert_eq!(a, b, "identical input must produce identical bytes");
}

fn best(n: usize, mut f: impl FnMut() -> usize) -> (f64, usize) {
    let mut sz = f();
    let mut b = f64::MAX;
    for _ in 0..n {
        let t = Instant::now();
        sz = f();
        b = b.min(t.elapsed().as_secs_f64());
    }
    (b * 1000.0, sz)
}

/// Original profiling intent, kept opt-in: write-path throughput sweep.
#[test]
#[ignore]
fn report_write_path_sweep() {
    let cols = 8usize;
    println!(
        "{:<40} {:>9} {:>10} {:>12}",
        "case", "ms", "MB out", "ns/cell"
    );
    for rows in [12_500usize, 25_000, 50_000, 100_000] {
        for (label, mixed, fx) in [
            ("numeric", false, false),
            ("mixed", true, false),
            ("mixed+formula", true, true),
        ] {
            let wb = build(rows, cols, mixed, fx);
            let (ms, sz) = best(3, || write_workbook_bytes(&wb).expect("write").len());
            let cells = (rows * cols) as f64;
            println!(
                "{:<40} {ms:>9.1} {:>10.2} {:>12.0}",
                format!("{label} {rows}x{cols}"),
                sz as f64 / 1e6,
                ms * 1e6 / cells
            );
        }
    }
}
