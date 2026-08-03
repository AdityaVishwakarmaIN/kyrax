//! A5 architecture-stress Wave-1 baseline: CSV/JSON interchange correctness,
//! determinism, and eager-vs-streaming fidelity.
//!
//! Wave-1 runs ONLY clean, non-hostile inputs in-process. Malformed-record
//! sweeps, Zip64 boundary execution (65,535/65,536/65,537, >4 GiB),
//! decompression bombs, and Excel-COM acceptance are deferred until the A6
//! coordinator and disk preflight exist — none appear here.
//!
//! Coverage implemented:
//!   IO-01  CSV and JSON correctness on clean inputs: Unicode, BOM, quoting /
//!          newlines, null vs "" vs missing-key, mixed types, dates, numeric
//!          extremes, and deterministic export (same input -> identical bytes).
//!   IO-02  streaming-vs-eager fidelity on an authored mixed-type workbook:
//!          concatenated stream batches match the eager read cell-for-cell,
//!          schema-for-schema, with both default and tiny windows.
//!   ZIP-01/02, IO-03 Python bindings, COM: deferred / covered in
//!          test_resilience.py / A5.md.

#![cfg(feature = "__arrow")]

use std::sync::atomic::{AtomicUsize, Ordering};

use arrow_array::types::Int32Type;
use arrow_array::{Array, DictionaryArray, Float64Array, RecordBatch, StringArray};
use kyrax::turbo::io::csv::{CsvOptions, csv_to_sheet, sheet_to_csv};
use kyrax::turbo::io::json::{JsonOptions, JsonShape, json_to_sheet, sheet_to_json};
use kyrax::turbo::write::{Cell, CellValue, Row, Workbook, date_to_serial, save_workbook};
use kyrax::turbo::{Features, SheetStream, StreamOptions, read_workbook_turbo_sheet};

/// Monotonic sequence so every temp path is unique even when tests run in
/// parallel — a shared filename (e.g. a common `import.csv`) previously caused
/// cross-test collisions.
static TMP_SEQ: AtomicUsize = AtomicUsize::new(0);

fn tmp(name: &str) -> String {
    let n = TMP_SEQ.fetch_add(1, Ordering::SeqCst);
    let d = std::env::temp_dir().join(format!("kyrax_a5_io_{}_{}", std::process::id(), n));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name).to_str().unwrap().to_string()
}

fn wb_with(rows: Vec<Vec<CellValue>>) -> Workbook {
    let mut wb = Workbook::with_sheet("Sheet1");
    for (ri, cells) in rows.into_iter().enumerate() {
        let mut r = Row::new(ri as u32 + 1);
        for (ci, v) in cells.into_iter().enumerate() {
            r.cells.push(Cell::new(ci as u32 + 1, v));
        }
        wb.sheets[0].rows.push(r);
    }
    wb
}

fn export_csv(path: &str, opts: &CsvOptions) -> String {
    let mut out = Vec::new();
    sheet_to_csv(path, "Sheet1", &mut out, opts).expect("csv export");
    String::from_utf8(out).unwrap()
}

fn import_csv(csv_text: &str, xlsx_out: &str, opts: &CsvOptions) {
    let csv_path = tmp("import.csv");
    std::fs::write(&csv_path, csv_text).unwrap();
    csv_to_sheet(&csv_path, xlsx_out, "Sheet1", opts).expect("csv import");
}

fn export_json(path: &str, opts: &JsonOptions) -> String {
    let mut out = Vec::new();
    sheet_to_json(path, "Sheet1", &mut out, opts).expect("json export");
    String::from_utf8(out).unwrap()
}

fn import_json(json_text: &str, xlsx_out: &str, opts: &JsonOptions) {
    let json_path = tmp("import.json");
    std::fs::write(&json_path, json_text).unwrap();
    json_to_sheet(&json_path, xlsx_out, "Sheet1", opts).expect("json import");
}

// ---------------------------------------------------------------------------
// IO-01 CSV correctness + determinism (clean inputs).
// ---------------------------------------------------------------------------

#[test]
fn csv_round_trip_unicode_bom_quotes_and_newlines() {
    let csv = "\u{feff}name,note\r\n\"\u{4f60}\u{597d}\",\"line1\nline2\"\r\n";
    let out = tmp("csv_unicode.xlsx");
    import_csv(csv, &out, &CsvOptions::default());
    let csv_out = export_csv(&out, &CsvOptions::default());
    assert!(
        !csv_out.starts_with('\u{feff}'),
        "IO-01: BOM must be consumed on import"
    );
    assert_eq!(
        csv_out,
        "name,note\r\n\u{4f60}\u{597d},\"line1\nline2\"\r\n"
    );
}

#[test]
fn csv_quotes_and_quoted_empty_distinguished() {
    let csv = "a,b,c\r\nx,\"\",z\r\ny,\"say \"\"hi\"\"\",w\r\n";
    let out = tmp("csv_quotes.xlsx");
    import_csv(csv, &out, &CsvOptions::default());
    assert_eq!(
        export_csv(&out, &CsvOptions::default()),
        "a,b,c\r\nx,\"\",z\r\ny,\"say \"\"hi\"\"\",w\r\n"
    );
}

#[test]
fn csv_infer_types_off_keeps_numeric_extremes_as_text() {
    let csv = "acct,big\n007,9007199254740993\n12345678901234567890,3.50\n";
    let out = tmp("csv_extremes.xlsx");
    import_csv(csv, &out, &CsvOptions::default());
    let csv_out = export_csv(&out, &CsvOptions::default());
    assert!(
        csv_out.contains("007,9007199254740993"),
        "IO-01: leading zero and >2^53 integer must stay text: {csv_out:?}"
    );
    assert!(
        csv_out.contains("12345678901234567890,3.50"),
        "IO-01: 20-digit and trailing-zero must stay text: {csv_out:?}"
    );
}

#[test]
fn csv_date_serial_exported_as_formatted_date() {
    let mut wb = wb_with(vec![
        vec![CellValue::Str("d".into()), CellValue::Str("t".into())],
        vec![
            CellValue::DateSerial(date_to_serial(2024, 1, 5)),
            CellValue::Number(42.0),
        ],
    ]);
    wb.style_work = true;
    let src = tmp("csv_date.xlsx");
    save_workbook(&wb, &src).unwrap();
    let csv = export_csv(&src, &CsvOptions::default());
    assert!(
        csv.contains("2024-01-05 00:00:00"),
        "IO-01: date must render formatted, not as a serial: {csv:?}"
    );
    assert!(!csv.contains("45292"), "IO-01: raw serial must never leak");
}

#[test]
fn csv_export_is_deterministic() {
    let mut wb = wb_with(vec![
        vec![
            CellValue::Str("id".into()),
            CellValue::Str("name".into()),
            CellValue::Str("note".into()),
        ],
        vec![
            CellValue::Number(1.0),
            CellValue::Str("alice".into()),
            CellValue::Str("comma,here".into()),
        ],
        vec![
            CellValue::Number(2.0),
            CellValue::Str("\u{00fc}ber".into()),
            CellValue::Str("quote\"in".into()),
        ],
    ]);
    wb.style_work = true;
    let src = tmp("csv_deterministic.xlsx");
    save_workbook(&wb, &src).unwrap();
    let a = export_csv(&src, &CsvOptions::default());
    let b = export_csv(&src, &CsvOptions::default());
    assert_eq!(a, b, "IO-01: CSV export must be byte-deterministic");
}

// ---------------------------------------------------------------------------
// IO-01 JSON correctness: three shapes, null/""/missing, extremes, determinism.
// ---------------------------------------------------------------------------

#[test]
fn json_null_vs_empty_string_round_trips_losslessly() {
    let json = r#"[{"k":"","n":null,"x":1},{"k":"v","n":0,"x":2}]"#;
    let out = tmp("json_null.xlsx");
    import_json(json, &out, &JsonOptions::default());
    let records = JsonOptions {
        shape: JsonShape::Records,
        ..Default::default()
    };
    let re = export_json(&out, &records);
    assert!(
        re.contains("\"k\":\"\""),
        "IO-01: empty string must survive as \"\": {re}"
    );
    assert!(re.contains("\"n\":null"), "IO-01: null must survive: {re}");
    assert!(re.contains("\"x\":1"), "IO-01: number must survive: {re}");
}

#[test]
fn json_numeric_extremes_exported_as_string() {
    // 2^53 + 2 (9_007_199_254_740_994) is exactly representable as an f64 and
    // is strictly greater than 2^53, so the JSON contract must emit it as a
    // string (an integer beyond 2^53 cannot survive as an IEEE-754 number).
    let mut wb = wb_with(vec![
        vec![CellValue::Str("big".into())],
        vec![CellValue::Number(9_007_199_254_740_994.0)],
    ]);
    wb.style_work = true;
    let src = tmp("json_extreme.xlsx");
    save_workbook(&wb, &src).unwrap();
    let records = JsonOptions {
        shape: JsonShape::Records,
        ..Default::default()
    };
    let re = export_json(&src, &records);
    assert!(
        re.contains("\"big\":\"9007199254740994\""),
        "IO-01: an integer beyond 2^53 must export as a string, not a lossy number: {re}"
    );
    assert!(
        !re.contains("\"big\":9007199254740994"),
        "IO-01: the extreme integer must not be emitted as a bare JSON number: {re}"
    );
}

#[test]
fn json_three_shapes_and_determinism() {
    let mut wb = wb_with(vec![
        vec![CellValue::Str("a".into()), CellValue::Str("b".into())],
        vec![CellValue::Number(1.0), CellValue::Str("x".into())],
        vec![CellValue::Number(2.0), CellValue::Str("y".into())],
    ]);
    wb.style_work = true;
    let src = tmp("json_shapes.xlsx");
    save_workbook(&wb, &src).unwrap();

    let records = JsonOptions {
        shape: JsonShape::Records,
        ..Default::default()
    };
    let columns = JsonOptions {
        shape: JsonShape::Columns,
        ..Default::default()
    };
    let ndjson = JsonOptions {
        shape: JsonShape::Ndjson,
        ..Default::default()
    };

    let r1 = export_json(&src, &records);
    assert_eq!(
        export_json(&src, &records),
        r1,
        "records export deterministic"
    );
    assert!(r1.contains("[{\"a\":1,\"b\":\"x\"}"), "records shape: {r1}");

    let c1 = export_json(&src, &columns);
    assert_eq!(
        export_json(&src, &columns),
        c1,
        "columns export deterministic"
    );
    assert!(c1.contains("\"a\":[1,2]"), "columns shape: {c1}");

    let n1 = export_json(&src, &ndjson);
    assert_eq!(
        export_json(&src, &ndjson),
        n1,
        "ndjson export deterministic"
    );
    assert!(
        n1.contains("\n") && n1.starts_with('{'),
        "ndjson shape: {n1}"
    );
}

/// NDJSON round trip: the public `json_to_sheet` supports `JsonShape::Ndjson`
/// via `discover_ndjson_keys` / `read_ndjson_into_sheet`, and export supports it
/// via `write_ndjson`. The earlier Phase-3 `Format("expected object")` failure
/// was a cross-test temp-file collision — unique temp paths now make this test
/// safe under parallel execution.
#[test]
fn json_ndjson_import_round_trips() {
    let nd = "{\"a\":1,\"b\":\"x\"}\n{\"a\":2,\"b\":\"y\"}\n";
    let out = tmp("json_ndjson.xlsx");
    import_json(
        nd,
        &out,
        &JsonOptions {
            shape: JsonShape::Ndjson,
            ..Default::default()
        },
    );
    let re = export_json(
        &out,
        &JsonOptions {
            shape: JsonShape::Ndjson,
            ..Default::default()
        },
    );
    assert_eq!(re, "{\"a\":1,\"b\":\"x\"}\n{\"a\":2,\"b\":\"y\"}\n");
}

// ---------------------------------------------------------------------------
// IO-02 streaming-vs-eager fidelity on a clean authored workbook.
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Debug)]
enum CellVal {
    Num(f64),
    Str(String),
}

fn cell_of(col: &dyn Array, i: usize) -> (bool, Option<CellVal>) {
    if col.is_null(i) {
        return (true, None);
    }
    if let Some(a) = col.as_any().downcast_ref::<Float64Array>() {
        return (false, Some(CellVal::Num(a.value(i))));
    }
    if let Some(a) = col.as_any().downcast_ref::<StringArray>() {
        return (false, Some(CellVal::Str(a.value(i).to_string())));
    }
    if let Some(a) = col.as_any().downcast_ref::<DictionaryArray<Int32Type>>() {
        let key = a.keys().value(i) as usize;
        let values = a.values().as_any().downcast_ref::<StringArray>().unwrap();
        return (false, Some(CellVal::Str(values.value(key).to_string())));
    }
    panic!("unexpected column data type {}", col.data_type())
}

fn compare_batches(path: &str, opts: &StreamOptions) {
    let eager = read_workbook_turbo_sheet(path, Features::VALUES, 0)
        .expect("eager read of a clean authored workbook");
    let sh = &eager.sheets[0];
    let nrows = sh.nrows;
    let ncols = sh.ncols;

    let mut stream =
        SheetStream::open(path, 0, opts.clone()).expect("stream open of a clean authored workbook");

    // NOTE: `column_names` / `total_nrows` are populated during EMISSION (the
    // first `next_batch` fills column_names, and total_nrows accumulates across
    // batches), so no metadata is asserted immediately after `open`. We drain
    // every batch first, then compare stable schema/values and the
    // post-emission totals.
    let mut collected: Vec<RecordBatch> = Vec::new();
    loop {
        match stream.next_batch(opts) {
            Ok(Some(b)) => {
                assert_eq!(
                    b.num_columns(),
                    ncols,
                    "IO-02: every batch must expose the full column set"
                );
                collected.push(b);
            }
            Ok(None) => break,
            Err(e) => panic!("IO-02: streaming error on clean input: {e}"),
        }
    }
    assert!(
        !collected.is_empty(),
        "IO-02: streaming must yield at least one batch"
    );

    // Post-emission metadata: column names and totals are now populated.
    assert_eq!(stream.column_names(), &sh.column_names[..]);
    assert_eq!(stream.total_ncols(), ncols);
    assert_eq!(stream.total_nrows(), nrows);

    let total: usize = collected.iter().map(|b| b.num_rows()).sum();
    assert_eq!(
        total, nrows,
        "IO-02: streamed row count must equal eager nrows"
    );

    // Schema stability across batches and equality with the eager schema.
    for (bi, b) in collected.iter().enumerate() {
        for c in 0..ncols {
            assert_eq!(
                b.schema().field(c).data_type(),
                sh.columns[c].data_type(),
                "IO-02: batch {bi} col {c} schema must equal eager schema"
            );
        }
    }

    // Cell-for-cell values across the concatenated batches.
    let mut row = 0usize;
    for b in &collected {
        for r in 0..b.num_rows() {
            for c in 0..ncols {
                let (an, av) = cell_of(b.column(c).as_ref(), r);
                let (bn, bv) = cell_of(sh.columns[c].as_ref(), row);
                assert_eq!(
                    (an, av),
                    (bn, bv),
                    "IO-02: value mismatch at row {row} col {c}"
                );
            }
            row += 1;
        }
    }
}

#[test]
fn stream_matches_eager_with_default_window() {
    let mut wb = wb_with(vec![
        vec![
            CellValue::Str("id".into()),
            CellValue::Str("tag".into()),
            CellValue::Str("val".into()),
        ],
        vec![
            CellValue::Number(1.0),
            CellValue::Str("a".into()),
            CellValue::Number(1.5),
        ],
        vec![
            CellValue::Number(2.0),
            CellValue::Str("b".into()),
            CellValue::Number(2.5),
        ],
        vec![
            CellValue::Number(3.0),
            CellValue::Str("a".into()),
            CellValue::Number(3.5),
        ],
    ]);
    wb.style_work = true;
    let src = tmp("stream_default.xlsx");
    save_workbook(&wb, &src).unwrap();
    compare_batches(&src, &StreamOptions::default());
}

#[test]
fn stream_matches_eager_with_tiny_window() {
    let mut wb = wb_with(vec![
        vec![CellValue::Str("k".into()), CellValue::Str("v".into())],
        vec![CellValue::Str("r1".into()), CellValue::Number(1.0)],
        vec![CellValue::Str("r2".into()), CellValue::Number(2.0)],
        vec![CellValue::Str("r3".into()), CellValue::Number(3.0)],
        vec![CellValue::Str("r4".into()), CellValue::Number(4.0)],
    ]);
    wb.style_work = true;
    let src = tmp("stream_tiny.xlsx");
    save_workbook(&wb, &src).unwrap();
    let mut tiny = StreamOptions::from_batch_rows(2);
    tiny.max_row_bytes = 1024 * 1024;
    tiny.max_pre_bytes = 256 * 1024;
    compare_batches(&src, &tiny);
}

// ---------------------------------------------------------------------------
// Wave-1 explicitly defers (documented, not tested here):
//   - malformed / hostile record sweeps (A6 subprocess guards required)
//   - Zip64 boundary execution 65,535/65,536/65,537 and >4 GiB (disk preflight)
//   - Excel-COM acceptance of repaired/streamed output (A1 runner)
//   - Python binding reachability for CSV/JSON/stream (test_resilience.py)
// ---------------------------------------------------------------------------
