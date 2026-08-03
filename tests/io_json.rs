//! Integration tests for the JSON / NDJSON interchange (C4b).
//!
//! These exercise the public `kyrax::turbo::io::json` API: round trips for all
//! three shapes, null vs empty-string vs missing-key, escaping, number
//! fidelity, dates, headerless mode, streaming writes, and large imports.
#![cfg(feature = "__arrow")]

use std::io::{self, Write};
use std::sync::atomic::{AtomicUsize, Ordering};

use kyrax::turbo::io::json::{JsonOptions, JsonShape, json_to_sheet, sheet_to_json};
use kyrax::turbo::write::{Cell, CellValue, Row, Sheet, Workbook, save_workbook};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn temp_path(tag: &str, ext: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir()
        .join(format!(
            "kyrax_json_{}_{}_{}.{}",
            std::process::id(),
            tag,
            n,
            ext
        ))
        .to_string_lossy()
        .into_owned()
}

fn save_sheet(path: &str, name: &str, rows: &[(u32, Vec<(u32, CellValue)>)], style_work: bool) {
    let mut sheet = Sheet::new(name);
    for (r, cells) in rows {
        let mut row = Row::new(*r);
        for (c, v) in cells {
            row.cells.push(Cell::new(*c, v.clone()));
        }
        sheet.rows.push(row);
    }
    let mut wb = Workbook::new();
    wb.sheets.clear();
    wb.sheets.push(sheet);
    wb.style_work = style_work;
    save_workbook(&wb, path).expect("save xlsx");
}

fn export(path: &str, sheet: &str, opts: &JsonOptions) -> String {
    let mut out = Vec::new();
    sheet_to_json(path, sheet, &mut out, opts).expect("sheet_to_json");
    String::from_utf8(out).expect("json is utf-8")
}

fn import(json_path: &str, xlsx_out: &str, name: &str, opts: &JsonOptions) {
    json_to_sheet(json_path, xlsx_out, name, opts).expect("json_to_sheet");
}

fn write_json(path: &str, contents: &str) {
    std::fs::write(path, contents).expect("write json");
}

fn records() -> JsonOptions {
    JsonOptions {
        shape: JsonShape::Records,
        has_header: true,
        date_format: String::new(),
    }
}

fn columns() -> JsonOptions {
    JsonOptions {
        shape: JsonShape::Columns,
        has_header: true,
        date_format: String::new(),
    }
}

fn ndjson() -> JsonOptions {
    JsonOptions {
        shape: JsonShape::Ndjson,
        has_header: true,
        date_format: String::new(),
    }
}

/// The fidelity sheet every round trip exercises: embedded quotes and
/// backslashes, an empty string, a null, a >2^53 number, a 20-digit numeric
/// string, unicode, and XML-special characters.
fn fidelity_sheet() -> (String, Vec<(u32, Vec<(u32, CellValue)>)>) {
    let xlsx = temp_path("fidelity", "xlsx");
    let rows = vec![
        (
            1,
            vec![
                (1, CellValue::Str("name".into())),
                (2, CellValue::Str("note".into())),
                (3, CellValue::Str("count".into())),
                (4, CellValue::Str("id".into())),
            ],
        ),
        (
            2,
            vec![
                (1, CellValue::Str("a\"b".into())),
                (2, CellValue::Str("c\\d".into())),
                (3, CellValue::Number(2.0)),
                (4, CellValue::Str("12345678901234567890".into())),
            ],
        ),
        (
            3,
            vec![
                (1, CellValue::Str(String::new())),
                (2, CellValue::Str(String::new())),
                (4, CellValue::Str("ok".into())),
            ],
        ),
        (
            4,
            vec![
                (1, CellValue::Str("café ☃".into())),
                (2, CellValue::Str("e&<>".into())),
                (3, CellValue::Number(3.5)),
                (4, CellValue::Str("x".into())),
            ],
        ),
    ];
    save_sheet(&xlsx, "Data", &rows, false);
    (xlsx, rows)
}

#[test]
fn round_trip_records() {
    let (xlsx, _) = fidelity_sheet();
    let json1 = export(&xlsx, "Data", &records());
    assert_eq!(
        json1,
        r#"[{"name":"a\"b","note":"c\\d","count":2,"id":"12345678901234567890"},{"name":"","note":"","count":null,"id":"ok"},{"name":"café ☃","note":"e&<>","count":3.5,"id":"x"}]"#,
        "records export pins the null/empty/escaped representation"
    );
    let jp = temp_path("rt_records", "json");
    write_json(&jp, &json1);
    let x2 = temp_path("rt_records2", "xlsx");
    import(&jp, &x2, "Data", &records());
    assert_eq!(export(&x2, "Data", &records()), json1, "records round trip");
    let _ = std::fs::remove_file(&xlsx);
}

#[test]
fn round_trip_columns() {
    let (xlsx, _) = fidelity_sheet();
    let json1 = export(&xlsx, "Data", &columns());
    assert_eq!(
        json1,
        r#"{"name":["a\"b","","café ☃"],"note":["c\\d","","e&<>"],"count":[2,null,3.5],"id":["12345678901234567890","ok","x"]}"#,
        "columns export"
    );
    let jp = temp_path("rt_columns", "json");
    write_json(&jp, &json1);
    let x2 = temp_path("rt_columns2", "xlsx");
    import(&jp, &x2, "Data", &columns());
    assert_eq!(export(&x2, "Data", &columns()), json1, "columns round trip");
    let _ = std::fs::remove_file(&xlsx);
}

#[test]
fn round_trip_ndjson() {
    let (xlsx, _) = fidelity_sheet();
    let json1 = export(&xlsx, "Data", &ndjson());
    let expected = concat!(
        r#"{"name":"a\"b","note":"c\\d","count":2,"id":"12345678901234567890"}"#,
        "\n",
        r#"{"name":"","note":"","count":null,"id":"ok"}"#,
        "\n",
        r#"{"name":"café ☃","note":"e&<>","count":3.5,"id":"x"}"#,
        "\n",
    );
    assert_eq!(json1, expected, "ndjson is one object per line");
    let jp = temp_path("rt_ndjson", "ndjson");
    write_json(&jp, &json1);
    let x2 = temp_path("rt_ndjson2", "xlsx");
    import(&jp, &x2, "Data", &ndjson());
    assert_eq!(export(&x2, "Data", &ndjson()), json1, "ndjson round trip");
    let _ = std::fs::remove_file(&xlsx);
}

#[test]
fn heterogeneous_keys_union_first_seen_order() {
    let jp = temp_path("het", "json");
    write_json(&jp, r#"[{"a":1,"b":2},{"b":3,"c":4}]"#);
    let x = temp_path("het_x", "xlsx");
    import(&jp, &x, "S", &records());
    assert_eq!(
        export(&x, "S", &records()),
        r#"[{"a":1,"b":2,"c":null},{"a":null,"b":3,"c":4}]"#,
        "heterogeneous keys become the union in first-seen order, missing = null"
    );
}

#[test]
fn null_vs_empty_string_survive() {
    // null and "" must stay distinct through import -> xlsx -> export.
    let jp = temp_path("nul", "json");
    write_json(&jp, r#"[{"name":"","note":null},{"name":null,"note":"x"}]"#);
    let x = temp_path("nul_x", "xlsx");
    import(&jp, &x, "S", &records());
    assert_eq!(
        export(&x, "S", &records()),
        r#"[{"name":"","note":null},{"name":null,"note":"x"}]"#,
        "null and empty string never conflate"
    );
}

#[test]
fn number_fidelity_beyond_two_pow_53() {
    // Numeric cells above 2^53 export as strings (never a silently-lossy
    // number); a homogeneous >2^53 column round-trips exactly. 2^53 itself is
    // exactly representable and stays a number (see the unit tests).
    let xlsx = temp_path("big", "xlsx");
    save_sheet(
        &xlsx,
        "N",
        &[
            (1, vec![(1, CellValue::Str("v".into()))]),
            (2, vec![(1, CellValue::Number(9_007_199_254_740_994.0))]),
            (3, vec![(1, CellValue::Number(9_007_199_254_740_998.0))]),
        ],
        false,
    );
    let json1 = export(&xlsx, "N", &records());
    assert_eq!(
        json1, r#"[{"v":"9007199254740994"},{"v":"9007199254740998"}]"#,
        ">2^53 integral cells export as exact digit strings"
    );
    let jp = temp_path("big_j", "json");
    write_json(&jp, &json1);
    let x2 = temp_path("big_x2", "xlsx");
    import(&jp, &x2, "N", &records());
    assert_eq!(export(&x2, "N", &records()), json1, ">2^53 round trip");
    let _ = std::fs::remove_file(&xlsx);
}

#[test]
fn big_json_integer_preserved_as_string() {
    // A 20-digit JSON integer cannot be an exact f64: it imports as its raw
    // digit string, so the identifier is never mangled. A small integer stays
    // a number.
    let jp = temp_path("bigin", "json");
    write_json(
        &jp,
        r#"[{"id":12345678901234567890},{"id":12345678901234567891}]"#,
    );
    let x = temp_path("bigin_x", "xlsx");
    import(&jp, &x, "S", &records());
    assert_eq!(
        export(&x, "S", &records()),
        r#"[{"id":"12345678901234567890"},{"id":"12345678901234567891"}]"#,
        "20-digit JSON integers round-trip as exact strings"
    );

    let jp2 = temp_path("smallin", "json");
    write_json(&jp2, r#"[{"n":42}]"#);
    let x2 = temp_path("smallin_x", "xlsx");
    import(&jp2, &x2, "S", &records());
    assert_eq!(export(&x2, "S", &records()), r#"[{"n":42}]"#);
}

#[test]
fn dates_export_iso_and_respect_date_format() {
    let xlsx = temp_path("date", "xlsx");
    save_sheet(
        &xlsx,
        "D",
        &[
            (
                1,
                vec![
                    (1, CellValue::Str("d".into())),
                    (2, CellValue::Str("dt".into())),
                ],
            ),
            (
                2,
                vec![
                    (1, CellValue::DateSerial(43_845.0)),
                    (2, CellValue::DateSerial(43_845.5)),
                ],
            ),
        ],
        true,
    );
    assert_eq!(
        export(&xlsx, "D", &records()),
        r#"[{"d":"2020-01-15","dt":"2020-01-15T12:00:00"}]"#,
        "default ISO 8601"
    );
    let custom = JsonOptions {
        shape: JsonShape::Records,
        has_header: true,
        date_format: "%d/%m/%Y".into(),
    };
    assert_eq!(
        export(&xlsx, "D", &custom),
        r#"[{"d":"15/01/2020","dt":"15/01/2020"}]"#,
        "strftime date_format"
    );
    let _ = std::fs::remove_file(&xlsx);
}

#[test]
fn has_header_false_uses_positional_keys() {
    let xlsx = temp_path("noh", "xlsx");
    save_sheet(
        &xlsx,
        "S",
        &[
            (
                1,
                vec![
                    (1, CellValue::Str("h1".into())),
                    (2, CellValue::Str("h2".into())),
                ],
            ),
            (
                2,
                vec![(1, CellValue::Number(1.0)), (2, CellValue::Str("x".into()))],
            ),
        ],
        false,
    );
    let opts = JsonOptions {
        shape: JsonShape::Records,
        has_header: false,
        date_format: String::new(),
    };
    assert_eq!(
        export(&xlsx, "S", &opts),
        r#"[{"1":"h1","2":"h2"},{"1":1,"2":"x"}]"#,
        "no-header export treats row 1 as data with positional keys"
    );
    let _ = std::fs::remove_file(&xlsx);
}

#[test]
fn control_chars_from_json_import_without_error() {
    // JSON \u0001 decodes to a control char; the xlsx WRITE path drops illegal
    // controls by design, while tab/LF/CR survive. The import must not error.
    let jp = temp_path("ctrl", "json");
    write_json(&jp, r#"[{"s":"a\u0001b","t":"tab\tnewline\n"}]"#);
    let x = temp_path("ctrl_x", "xlsx");
    import(&jp, &x, "S", &records());
    assert_eq!(
        export(&x, "S", &records()),
        r#"[{"s":"ab","t":"tab\u0009newline\u000a"}]"#,
        "illegal control stripped by writer; legal tab/LF preserved and escaped"
    );
}

/// A write sink that counts how many times it is written to, proving the JSON
/// emitter flushes incrementally in bounded chunks instead of assembling the
/// whole document in memory.
struct CountingSink {
    writes: usize,
    max_chunk: usize,
    total: usize,
}

impl Write for CountingSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.total += buf.len();
        self.writes += 1;
        self.max_chunk = self.max_chunk.max(buf.len());
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn export_streams_in_bounded_chunks() {
    // ~8 cols x 45k rows ≈ >11 MB of JSON. The emitter must flush many times,
    // each chunk bounded around 1 MiB — peak memory O(chunk), not O(file).
    let xlsx = temp_path("stream", "xlsx");
    let mut rows: Vec<(u32, Vec<(u32, CellValue)>)> = vec![(
        1,
        (1..=8)
            .map(|c| (c, CellValue::Str(format!("c{c}"))))
            .collect(),
    )];
    for r in 2..=45_001u32 {
        let cells: Vec<(u32, CellValue)> = (1..=8u32)
            .map(|c| (c, CellValue::Str(format!("row{r}col{c} value value"))))
            .collect();
        rows.push((r, cells));
    }
    save_sheet(&xlsx, "S", &rows, false);

    let mut sink = CountingSink {
        writes: 0,
        max_chunk: 0,
        total: 0,
    };
    sheet_to_json(&xlsx, "S", &mut sink, &records()).expect("export");
    assert!(
        sink.writes > 3,
        "expected many incremental writes, got {}",
        sink.writes
    );
    assert!(
        sink.max_chunk < 2 * 1024 * 1024,
        "single chunk of {} bytes — the output is being buffered, not streamed",
        sink.max_chunk
    );
    assert!(
        sink.total > 9 * 1024 * 1024,
        "expected a large document, got {} bytes",
        sink.total
    );
    let _ = std::fs::remove_file(&xlsx);
}

#[test]
fn large_ndjson_import_streams() {
    let n = 20_000usize;
    let jp = temp_path("large", "ndjson");
    {
        let mut f = std::fs::File::create(&jp).unwrap();
        let mut line = String::with_capacity(80);
        for i in 0..n {
            line.clear();
            line.push_str(&format!(
                r#"{{"row":{i},"name":"user {i}","score":{}.5}}"#,
                i % 10
            ));
            line.push('\n');
            f.write_all(line.as_bytes()).unwrap();
        }
    }
    let x = temp_path("large_x", "xlsx");
    import(&jp, &x, "S", &ndjson());
    let json = export(&x, "S", &ndjson());
    let lines: Vec<&str> = json.lines().collect();
    assert_eq!(lines.len(), n, "every NDJSON line becomes a row");
    assert!(lines[0].starts_with(r#"{"row":0,"name":"user 0""#));
    assert!(lines[1].starts_with(r#"{"row":1,"name":"user 1""#));
    assert!(lines[n - 1].starts_with(&format!(r#"{{"row":{},"name":"user {}""#, n - 1, n - 1)));
    let _ = std::fs::remove_file(&jp);
    let _ = std::fs::remove_file(&x);
}

/// Benchmark harness against pandas (run explicitly with
/// `cargo test --release --features __arrow --test io_json -- --ignored --nocapture bench_vs_pandas`).
/// Prints the kyrax wall time; a python script measures pandas on the same
/// file (see the C4b report).
#[test]
#[ignore]
fn bench_vs_pandas() {
    let xlsx = std::env::temp_dir().join("kyrax_io_json_bench.xlsx");
    let path = xlsx.to_string_lossy().into_owned();
    let nrows: u32 = 100_000;
    let mut rows: Vec<(u32, Vec<(u32, CellValue)>)> = vec![(
        1,
        (1..=8u32)
            .map(|c| {
                (
                    c,
                    CellValue::Str(match c {
                        1 => "name".into(),
                        2 => "date".into(),
                        3 => "amount".into(),
                        4 => "quantity".into(),
                        5 => "region".into(),
                        6 => "active".into(),
                        7 => "notes".into(),
                        _ => "tag".into(),
                    }),
                )
            })
            .collect(),
    )];
    for r in 2..=(nrows + 1) {
        let i = r as i64 - 2;
        let cells = vec![
            (1, CellValue::Str(format!("customer {i}"))),
            (2, CellValue::DateSerial(43_845.0 + (i % 365) as f64)),
            (3, CellValue::Number((i % 10_000) as f64 * 1.5)),
            (4, CellValue::Number((i % 100) as f64)),
            (
                5,
                CellValue::Str(if i % 2 == 0 {
                    "east".into()
                } else {
                    "west".into()
                }),
            ),
            (
                6,
                CellValue::Str(if i % 3 == 0 { "y".into() } else { "n".into() }),
            ),
            (
                7,
                CellValue::Str(format!("note number {i} for the pipeline")),
            ),
            (8, CellValue::Str(format!("tag-{}-{}", i % 7, i % 11))),
        ];
        rows.push((r, cells));
    }
    save_sheet(&path, "Data", &rows, true);

    let opts = records();
    let mut sink = io::sink();
    sheet_to_json(&path, "Data", &mut sink, &opts).expect("warmup");

    let mut best = f64::MAX;
    for _ in 0..3 {
        let t = std::time::Instant::now();
        let mut sink = io::sink();
        sheet_to_json(&path, "Data", &mut sink, &opts).expect("bench run");
        best = best.min(t.elapsed().as_secs_f64());
    }
    println!("KYRAX_MS {:.3} FILE {path}", best * 1000.0);
    println!("KYRAX_ROWS {nrows}");
}
