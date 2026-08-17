//! Integration tests for turbo CSV import/export (task C4a).
#![cfg(feature = "__arrow")]

use std::io::{Read, Write};

use arrow_array::DictionaryArray;
use arrow_array::types::Int32Type;
use pretty_assertions::assert_eq;

use kyrax::turbo::io::csv::{
    CsvOptions, CsvReader, READ_CHUNK, RawField, csv_to_sheet, sheet_to_csv,
};
use kyrax::turbo::write::{
    CachedValue, Cell, CellValue, FormulaKind, Row, Workbook, date_to_serial, datetime_to_serial,
    save_workbook,
};
use kyrax::turbo::{Features, read_workbook_turbo_sheet};

fn tmp(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("kyrax_csv_test_{}", std::process::id()));
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
    sheet_to_csv(path, "Sheet1", &mut out, opts).expect("export");
    String::from_utf8(out).unwrap()
}

fn import_csv(csv_text: &str, xlsx_out: &str, opts: &CsvOptions) {
    let p = format!("{xlsx_out}.in.csv");
    std::fs::write(&p, csv_text).unwrap();
    csv_to_sheet(&p, xlsx_out, "Sheet1", opts).expect("import");
}

// ---------------------------------------------------------------------------
// Round trip: xlsx → csv → xlsx → csv must be stable and lossless.
// ---------------------------------------------------------------------------

#[test]
fn round_trip_values_survive() {
    let mut wb = wb_with(vec![
        vec![
            CellValue::Str("id".into()),
            CellValue::Str("name".into()),
            CellValue::Str("note".into()),
            CellValue::Str("num".into()),
        ],
        vec![
            CellValue::Str("1".into()),
            CellValue::Str("alice".into()),
            CellValue::Str("plain".into()),
            CellValue::Number(42.0),
        ],
        vec![
            CellValue::Str("2".into()),
            CellValue::Str("bob".into()),
            CellValue::Str("comma,here".into()),
            CellValue::Number(3.5),
        ],
        vec![
            CellValue::Str("3".into()),
            CellValue::Str("cece".into()),
            CellValue::Str("quote\"inside".into()),
            CellValue::Number(0.0),
        ],
        vec![
            CellValue::Str("4".into()),
            CellValue::Str(String::new()), // empty-string name
            CellValue::Str("line1\nline2".into()),
            CellValue::Number(7.0),
        ],
        vec![
            CellValue::Str("5".into()),
            CellValue::Str("世界 🎉".into()),
            CellValue::Str(String::new()), // empty-string note
            CellValue::Number(9007199254740990.0),
        ],
        vec![
            CellValue::Empty,
            CellValue::Empty,
            CellValue::Empty,
            CellValue::Empty,
        ],
    ]);
    wb.style_work = true;

    let src = tmp("rt_src.xlsx");
    save_workbook(&wb, &src).unwrap();
    let opts = CsvOptions::default();

    let csv1 = export_csv(&src, &opts);
    assert!(
        csv1.contains("\"comma,here\""),
        "delimiter must be quoted: {csv1:?}"
    );
    assert!(
        csv1.contains("\"quote\"\"inside\""),
        "quote must be doubled: {csv1:?}"
    );
    assert!(
        csv1.contains("\"line1\nline2\""),
        "newline field must span lines: {csv1:?}"
    );
    assert!(
        csv1.contains(",\"\","),
        "empty-string cell must export as \"\""
    );
    assert!(csv1.contains("世界 🎉"), "unicode must survive");
    assert!(csv1.contains(",42\r\n"), "integer float exports without .0");
    assert!(
        csv1.contains(",9007199254740990\r\n"),
        "large exact integer exports exactly"
    );
    assert!(
        csv1.contains(",,,\r\n"),
        "blank row exports as empty fields"
    );

    // csv → xlsx → csv must be byte-stable.
    let mid = tmp("rt_mid.xlsx");
    import_csv(&csv1, &mid, &opts);
    let csv2 = export_csv(&mid, &opts);
    assert_eq!(csv1, csv2, "round trip must be byte-stable");
}

// ---------------------------------------------------------------------------
// RFC 4180 quoting torture cases, both directions.
// ---------------------------------------------------------------------------

#[test]
fn quoted_field_with_newline_round_trips() {
    let csv = "a,b\r\n1,\"line1\nline2\"\r\n";
    let out = tmp("nl_out.xlsx");
    import_csv(csv, &out, &CsvOptions::default());
    assert_eq!(export_csv(&out, &CsvOptions::default()), csv);
}

#[test]
fn quoted_field_with_quote_round_trips() {
    let csv = "a,b\r\n1,\"say \"\"hi\"\" now\"\r\n";
    let out = tmp("qq_out.xlsx");
    import_csv(csv, &out, &CsvOptions::default());
    assert_eq!(export_csv(&out, &CsvOptions::default()), csv);
}

#[test]
fn crlf_input_accepted() {
    let csv = "a,b\r\n1,2\r\n3,4\r\n";
    let out = tmp("crlf_out.xlsx");
    import_csv(csv, &out, &CsvOptions::default());
    // LF and mixed endings also accepted; output is CRLF.
    let mixed = "a,b\n1,2\r\n3,4\n";
    let out2 = tmp("crlf_mixed.xlsx");
    import_csv(mixed, &out2, &CsvOptions::default());
    assert_eq!(export_csv(&out2, &CsvOptions::default()), csv);
}

#[test]
fn leading_bom_consumed() {
    let csv = "\u{feff}name,val\na,b\n";
    let out = tmp("bom_out.xlsx");
    import_csv(csv, &out, &CsvOptions::default());
    let csv_out = export_csv(&out, &CsvOptions::default());
    assert!(!csv_out.starts_with('\u{feff}'), "BOM must be consumed");
    assert_eq!(csv_out, "name,val\r\na,b\r\n");
}

#[test]
fn empty_vs_quoted_empty_vs_missing_trailing() {
    let csv = "a,b,c\r\nx,\"\",z\r\ny,,w\r\np,q\r\n";
    let out = tmp("empty_out.xlsx");
    import_csv(csv, &out, &CsvOptions::default());
    let csv_out = export_csv(&out, &CsvOptions::default());
    // Data-row empty vs quoted-empty survives. A missing trailing field
    // normalizes to a trailing empty field (xlsx has no "absent" cell).
    assert_eq!(csv_out, "a,b,c\r\nx,\"\",z\r\ny,,w\r\np,q,\r\n");
}

// ---------------------------------------------------------------------------
// Type inference: OFF by default, and exact when ON.
// ---------------------------------------------------------------------------

#[test]
fn infer_types_defaults_off_keeps_text() {
    let csv = "acct,name\n007,42\n12345678901234567890,3.50\n";
    let out = tmp("infer_off.xlsx");
    import_csv(csv, &out, &CsvOptions::default());
    let csv_out = export_csv(&out, &CsvOptions::default());
    assert!(
        csv_out.contains("007,42"),
        "leading zero must survive: {csv_out:?}"
    );
    assert!(
        csv_out.contains("12345678901234567890,3.50"),
        "20-digit string and trailing zero must survive: {csv_out:?}"
    );
}

#[test]
fn infer_types_rules_exact() {
    let opts = CsvOptions {
        infer_types: true,
        ..Default::default()
    };
    let csv = "n,code,big\n42,007,9007199254740992\n3.5,0.5,1e10\n";
    let out = tmp("infer_on.xlsx");
    import_csv(csv, &out, &opts);
    // 42 → number, 007 stays text, 2^53 stays text, 3.5/0.5 → numbers.
    // Column "big" mixes text + number, so its number renders via the reader's
    // ryu text ("10000000000.0"); the value is preserved either way.
    let csv_out = export_csv(&out, &CsvOptions::default());
    assert_eq!(
        csv_out,
        "n,code,big\r\n42,007,9007199254740992\r\n3.5,0.5,10000000000.0\r\n"
    );
}

#[test]
fn header_record_is_never_inferred() {
    let csv = "42\nx\n";
    let out = tmp("header_infer.xlsx");
    let opts = CsvOptions {
        infer_types: true,
        has_header: true,
        ..Default::default()
    };
    import_csv(csv, &out, &opts);
    // Header stays text → a pure string column (dictionary), not a float column.
    let wb = read_workbook_turbo_sheet(&out, Features::VALUES, 0).unwrap();
    let col = &wb.sheets[0].columns[0];
    assert!(
        col.as_any()
            .downcast_ref::<DictionaryArray<Int32Type>>()
            .is_some(),
        "header cell must remain text (not inferred)"
    );
}

// ---------------------------------------------------------------------------
// Dates: real formatted dates from serials, never raw serials.
// ---------------------------------------------------------------------------

#[test]
fn date_serial_exported_as_formatted_date() {
    let mut wb = wb_with(vec![
        vec![CellValue::Str("d".into()), CellValue::Str("t".into())],
        vec![
            CellValue::DateSerial(date_to_serial(2024, 1, 5)),
            CellValue::DateSerial(datetime_to_serial(2024, 1, 5, 10, 30, 0, 0)),
        ],
    ]);
    wb.style_work = true;
    let src = tmp("date_src.xlsx");
    save_workbook(&wb, &src).unwrap();
    let opts = CsvOptions::default();
    let csv = export_csv(&src, &opts);
    assert!(
        csv.contains("2024-01-05 00:00:00"),
        "date must be formatted: {csv:?}"
    );
    assert!(
        csv.contains("2024-01-05 10:30:00"),
        "datetime must be formatted: {csv:?}"
    );
    assert!(
        !csv.contains("45292"),
        "raw serial must never leak: {csv:?}"
    );
}

#[test]
fn date_serial_1904_system() {
    // Serial for 2024-01-05 in the 1904 system is the 1900 serial minus 1462.
    let serial_1904 = date_to_serial(2024, 1, 5) - 1462.0;
    let mut wb = wb_with(vec![
        vec![CellValue::Str("d".into())],
        vec![CellValue::DateSerial(serial_1904)],
    ]);
    wb.style_work = true;
    wb.options.date1904 = true;
    let src = tmp("date1904_src.xlsx");
    save_workbook(&wb, &src).unwrap();
    let csv = export_csv(&src, &CsvOptions::default());
    assert!(
        csv.contains("2024-01-05 00:00:00"),
        "1904 dates must convert: {csv:?}"
    );
}

#[test]
fn custom_date_format_respected() {
    let mut wb = wb_with(vec![
        vec![CellValue::Str("d".into())],
        vec![CellValue::DateSerial(date_to_serial(2024, 1, 5))],
    ]);
    wb.style_work = true;
    let src = tmp("date_fmt_src.xlsx");
    save_workbook(&wb, &src).unwrap();
    let opts = CsvOptions {
        date_format: "dd/mm/yyyy".into(),
        ..Default::default()
    };
    assert!(export_csv(&src, &opts).contains("05/01/2024"));
}

// ---------------------------------------------------------------------------
// Formulas: export the cached value, never the formula text.
// ---------------------------------------------------------------------------

#[test]
fn formula_exports_cached_value() {
    let mut wb = Workbook::with_sheet("Sheet1");
    wb.sheets[0]
        .rows
        .push(Row::new(1).with_cell(1, CellValue::Str("val".into())));
    wb.sheets[0].rows.push(Row::new(2).with_cell(
        1,
        CellValue::Formula {
            text: "=1+1".into(),
            kind: FormulaKind::Normal,
            cached: Some(CachedValue::Number(2.0)),
        },
    ));
    let src = tmp("formula_src.xlsx");
    save_workbook(&wb, &src).unwrap();
    let csv = export_csv(&src, &CsvOptions::default());
    assert!(
        csv.contains("2\r\n"),
        "cached value must be exported: {csv:?}"
    );
    assert!(!csv.contains("=1+1"), "formula text must not leak: {csv:?}");
}

// ---------------------------------------------------------------------------
// Streaming: peak memory must be O(chunk), not O(file).
// ---------------------------------------------------------------------------

struct MaxChunk<W: Write> {
    inner: W,
    max_chunk: usize,
    total: usize,
}

impl<W: Write> Write for MaxChunk<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.max_chunk = self.max_chunk.max(buf.len());
        self.total += buf.len();
        self.inner.write_all(buf)?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn make_wide_wb(nrows: u32, field: &str) -> Workbook {
    let mut wb = Workbook::with_sheet("Sheet1");
    for ri in 0..nrows {
        let mut r = Row::new(ri + 1);
        for ci in 0..5u32 {
            r.cells
                .push(Cell::new(ci + 1, CellValue::Str(field.into())));
        }
        wb.sheets[0].rows.push(r);
    }
    wb
}

#[test]
fn export_memory_does_not_scale_with_file_size() {
    let field = "x".repeat(100);
    let small = tmp("mem_small.xlsx");
    save_workbook(&make_wide_wb(1_000, &field), &small).unwrap();
    let large = tmp("mem_large.xlsx");
    save_workbook(&make_wide_wb(50_000, &field), &large).unwrap();

    let sink_small = MaxChunk {
        inner: std::io::sink(),
        max_chunk: 0,
        total: 0,
    };
    let mut s1 = sink_small;
    sheet_to_csv(&small, "Sheet1", &mut s1, &CsvOptions::default()).unwrap();

    let sink_large = MaxChunk {
        inner: std::io::sink(),
        max_chunk: 0,
        total: 0,
    };
    let mut s2 = sink_large;
    sheet_to_csv(&large, "Sheet1", &mut s2, &CsvOptions::default()).unwrap();

    // One row is ~507 bytes (5 × 100 + 4 delimiters + CRLF). The writer must
    // never buffer more than a single row regardless of file size.
    assert!(s1.max_chunk <= 600, "small file max chunk {}", s1.max_chunk);
    assert!(s2.max_chunk <= 600, "large file max chunk {}", s2.max_chunk);
    assert!(
        s2.total > 20 * s1.total,
        "output must still scale: {} vs {}",
        s2.total,
        s1.total
    );
}

struct MaxRead<R: Read> {
    inner: R,
    max_req: usize,
    total: usize,
}

impl<R: Read> Read for MaxRead<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.max_req = self.max_req.max(buf.len());
        let n = self.inner.read(buf)?;
        self.total += n;
        Ok(n)
    }
}

#[test]
fn import_parses_in_bounded_chunks() {
    let field = "y".repeat(100);
    let small_csv = tmp("mem_small.csv");
    let mut f = std::fs::File::create(&small_csv).unwrap();
    for _ in 0..2_000 {
        f.write_all(field.as_bytes()).unwrap();
        f.write_all(b"\r\n").unwrap();
    }
    drop(f);
    let large_csv = tmp("mem_large.csv");
    let mut f = std::fs::File::create(&large_csv).unwrap();
    for _ in 0..50_000 {
        f.write_all(field.as_bytes()).unwrap();
        f.write_all(b"\r\n").unwrap();
    }
    drop(f);

    let drain = |path: &str| -> (usize, usize, usize) {
        let inner = MaxRead {
            inner: std::fs::File::open(path).unwrap(),
            max_req: 0,
            total: 0,
        };
        let mut r = CsvReader::new(inner, &CsvOptions::default());
        let mut peak = 0usize;
        while let Some(_rec) = r.next_record().unwrap() {
            peak = peak.max(r.peak_field_bytes());
        }
        let stats = r.into_inner();
        (stats.max_req, stats.total, peak)
    };

    let (req_s, total_s, peak_s) = drain(&small_csv);
    let (req_l, total_l, peak_l) = drain(&large_csv);

    assert_eq!(req_s, READ_CHUNK, "reads are bounded to one chunk");
    assert_eq!(req_l, READ_CHUNK, "reads are bounded to one chunk");
    assert!(total_l > 20 * total_s, "must still read the whole file");
    assert_eq!(
        peak_s, peak_l,
        "peak field buffer must not scale with row count"
    );
    assert!(peak_l <= 110, "peak field buffer {} ", peak_l);
}

// ---------------------------------------------------------------------------
// Reader unit behaviour via the public parser.
// ---------------------------------------------------------------------------

#[test]
fn parser_distinguishes_quoted_empty_from_empty() {
    let data = b"a,\"\",c\r\nx,,z\r\n".to_vec();
    let r = CsvReader::new(data.as_slice(), &CsvOptions::default());
    let mut r = r;
    let rec1 = r.next_record().unwrap().unwrap();
    assert_eq!(rec1[0].bytes, b"a");
    assert_eq!(rec1[1].bytes, b"");
    assert!(rec1[1].was_quoted, "quoted empty must be marked quoted");
    assert_eq!(rec1[2].bytes, b"c");
    let rec2 = r.next_record().unwrap().unwrap();
    assert_eq!(rec2[1].bytes, b"");
    assert!(
        !rec2[1].was_quoted,
        "unquoted empty must not be marked quoted"
    );
    assert!(r.next_record().unwrap().is_none());
}

#[test]
fn parser_accepts_crlf_lf_and_bare_cr() {
    for line_end in ["\r\n", "\n", "\r"] {
        let csv = format!("a{line_end}b{line_end}");
        let mut r = CsvReader::new(csv.as_bytes(), &CsvOptions::default());
        let recs: Vec<Vec<RawField>> = std::iter::from_fn(|| r.next_record().unwrap()).collect();
        assert_eq!(recs.len(), 2, "line ending {:?}", line_end.as_bytes());
        assert_eq!(recs[0][0].bytes, b"a");
        assert_eq!(recs[1][0].bytes, b"b");
    }
}

#[test]
fn parser_errors_on_unterminated_quote() {
    let data = b"a,\"unterminated".to_vec();
    let mut r = CsvReader::new(data.as_slice(), &CsvOptions::default());
    let err = r.next_record().unwrap_err();
    assert!(err.to_string().contains("unterminated"), "{err}");
}
