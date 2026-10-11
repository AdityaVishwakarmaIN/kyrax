//! A3 — the streaming reader must emit byte-identical Arrow output before and
//! after any internal optimization. Each stream (every sheet of every fixture,
//! default and tiny windows) is reduced to an FNV-1a digest covering batch
//! boundaries, schema, every cell (null flag + value bits / string bytes /
//! dictionary keys+values) and the sparse aggregates (cell errors, row dims,
//! totals).
//!
//! Regenerate the tables after an INTENTIONAL output change with
//! `KYRAX_PRINT_GOLDEN=1 cargo test --release --test stream_equivalence -- --nocapture`.
//! Large bench fixtures are covered by the `#[ignore]` tests (set
//! `KYRAX_BENCH_CACHE` to the directory holding `fixture_v1_*.xlsx`).

use arrow_array::cast::AsArray;
use arrow_array::types::Int32Type;
use arrow_array::{Array, Float64Array, RecordBatch};
use arrow_schema::DataType;
use kyrax::turbo::{SheetStream, StreamOptions};

struct Fnv(u64);
impl Fnv {
    fn new() -> Self {
        Fnv(0xcbf29ce484222325)
    }
    fn bytes(&mut self, b: &[u8]) {
        for &x in b {
            self.0 ^= x as u64;
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }
    fn u64(&mut self, v: u64) {
        self.bytes(&v.to_le_bytes());
    }
    fn str(&mut self, s: &str) {
        self.u64(s.len() as u64);
        self.bytes(s.as_bytes());
    }
}

fn hash_batch(h: &mut Fnv, b: &RecordBatch) {
    h.u64(b.num_rows() as u64);
    h.str(&format!("{:?}", b.schema()));
    for col in b.columns() {
        h.u64(col.len() as u64);
        match col.data_type() {
            DataType::Float64 => {
                let a: &Float64Array = col.as_primitive();
                for i in 0..a.len() {
                    if a.is_null(i) {
                        h.u64(u64::MAX - 1);
                    } else {
                        h.u64(a.value(i).to_bits());
                    }
                }
            }
            DataType::Utf8 => {
                let a = col.as_string::<i32>();
                for i in 0..a.len() {
                    if a.is_null(i) {
                        h.u64(u64::MAX - 1);
                    } else {
                        h.str(a.value(i));
                    }
                }
            }
            DataType::Dictionary(_, _) => {
                let a = col.as_dictionary::<Int32Type>();
                let vals = a.values().as_string::<i32>();
                for i in 0..vals.len() {
                    h.str(vals.value(i));
                }
                let keys = a.keys();
                for i in 0..keys.len() {
                    if keys.is_null(i) {
                        h.u64(u64::MAX - 1);
                    } else {
                        h.u64(keys.value(i) as u64);
                    }
                }
            }
            other => panic!("unexpected column type {other}"),
        }
    }
}

/// Digest + batch count for one sheet; `None` when the sheet cannot be opened
/// or streamed (the failure itself is part of the contract only as "None").
fn digest(path: &str, sheet: usize, opts: &StreamOptions) -> Option<(u64, usize)> {
    let mut s = SheetStream::open(path, sheet, opts.clone()).ok()?;
    let mut h = Fnv::new();
    let mut n = 0usize;
    while let Some(b) = s.next_batch(opts).ok()? {
        hash_batch(&mut h, &b);
        n += 1;
    }
    let sum = s.into_summary();
    h.u64(sum.nrows as u64);
    h.u64(sum.ncols as u64);
    for c in &sum.column_names {
        h.str(c);
    }
    for e in &sum.cell_errors {
        h.u64(e.row as u64);
        h.u64(e.col as u64);
        h.str(&e.code);
    }
    for r in &sum.row_dims {
        h.str(&format!("{r:?}"));
    }
    Some((h.0, n))
}

fn sheets_digest(path: &str, opts: &StreamOptions) -> String {
    let mut out = String::new();
    for sheet in 0..8 {
        match digest(path, sheet, opts) {
            Some((d, n)) => out.push_str(&format!("{sheet}:{d:016x}/{n};")),
            None => {
                out.push_str(&format!("{sheet}:none;"));
                break;
            }
        }
    }
    out
}

fn small_windows() -> StreamOptions {
    // Smallest window the reader allows (its 64 KiB chunk floor applies), so
    // the multi-window code path is exercised on the larger fixtures.
    StreamOptions {
        window_bytes: 1,
        ..Default::default()
    }
}

fn fixture_files(dir: &str) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "xlsx"))
                .map(|p| p.to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn run_table(files: &[String], golden: &[(&str, &str)], tag: &str) {
    let print = std::env::var("KYRAX_PRINT_GOLDEN").is_ok();
    let mut mismatches = Vec::new();
    for f in files {
        let name = std::path::Path::new(f)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let got = format!(
            "{}|{}",
            sheets_digest(f, &StreamOptions::default()),
            sheets_digest(f, &small_windows())
        );
        if print {
            println!("GOLDEN[{tag}] (\"{name}\", \"{got}\"),");
            continue;
        }
        match golden.iter().find(|(n, _)| *n == name) {
            Some((_, want)) if *want == got => {}
            Some((_, want)) => mismatches.push(format!("{name}: want {want} got {got}")),
            None => mismatches.push(format!("{name}: no golden entry ({tag})")),
        }
    }
    assert!(
        mismatches.is_empty(),
        "stream output changed:\n{}",
        mismatches.join("\n")
    );
}

#[test]
fn stream_output_matches_golden_testdata() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    run_table(&fixture_files(dir), GOLDEN_TESTDATA, "testdata");
}

#[test]
#[ignore]
fn stream_output_matches_golden_bench() {
    let Ok(dir) = std::env::var("KYRAX_BENCH_CACHE") else {
        return;
    };
    let files: Vec<String> = fixture_files(&dir)
        .into_iter()
        .filter(|f| !f.contains("large"))
        .collect();
    run_table(&files, GOLDEN_BENCH, "bench");
}

#[test]
#[ignore]
fn stream_output_matches_golden_bench_large() {
    let Ok(dir) = std::env::var("KYRAX_BENCH_CACHE") else {
        return;
    };
    let files: Vec<String> = fixture_files(&dir)
        .into_iter()
        .filter(|f| f.contains("large"))
        .collect();
    run_table(&files, GOLDEN_BENCH_LARGE, "bench-large");
}

/// Timing probe: `KYRAX_BENCH_FILE=... cargo test --release --test
/// stream_equivalence stream_cpu_probe -- --ignored --nocapture`.
#[test]
#[ignore]
fn stream_cpu_probe() {
    let Ok(path) = std::env::var("KYRAX_BENCH_FILE") else {
        return;
    };
    let opts = StreamOptions::from_batch_rows(10_000);
    let mut s = SheetStream::open(&path, 0, opts.clone()).unwrap();
    let mut rows = 0usize;
    while let Some(b) = s.next_batch(&opts).unwrap() {
        rows += b.num_rows();
    }
    println!("rows={rows}");
}

const GOLDEN_TESTDATA: &[(&str, &str)] = &[
    ("decimal-numbers.xlsx",
     "0:aa6166c8ffb33c14/1;1:none;|0:aa6166c8ffb33c14/1;1:none;"),
    ("div0.xlsx",
     "0:00892e9cdee01ff1/1;1:none;|0:00892e9cdee01ff1/1;1:none;"),
    ("empty.xlsx",
     "0:f0c0dde4eca6e07b/1;1:none;|0:f0c0dde4eca6e07b/1;1:none;"),
    ("fixture-changing-header-location.xlsx",
     "0:91c381d2202e7a5c/1;1:4c114ce4f0791b1d/1;2:20d56a58b20a86e6/1;3:none;|0:91c381d2202e7a5c/1;1:4c114ce4f0791b1d/1;2:20d56a58b20a86e6/1;3:none;"),
    ("fixture-invalid-cell-value-num.xlsx",
     "0:12f52f075639a0b5/1;1:none;|0:12f52f075639a0b5/1;1:none;"),
    ("fixture-invalid-cell-value.xlsx",
     "0:fc365f2c69ac66e7/1;1:none;|0:fc365f2c69ac66e7/1;1:none;"),
    ("fixture-multi-dtypes-columns.xlsx",
     "0:308b18a22169571c/1;1:none;|0:308b18a22169571c/1;1:none;"),
    ("fixture-multi-sheet.xlsx",
     "0:3921405afd77d8e4/1;1:a85be506718f942c/1;2:ae8238a172267278/1;3:none;|0:3921405afd77d8e4/1;1:a85be506718f942c/1;2:ae8238a172267278/1;3:none;"),
    ("fixture-sheets-different-visibilities.xlsx",
     "0:3bef337cc88b69ad/1;1:b4268913199bada3/1;2:96a82415c45e90b5/1;3:none;|0:3bef337cc88b69ad/1;1:b4268913199bada3/1;2:96a82415c45e90b5/1;3:none;"),
    ("fixture-single-sheet-duplicated-columns.xlsx",
     "0:214af4d5dbc6deb2/1;1:none;|0:214af4d5dbc6deb2/1;1:none;"),
    ("fixture-single-sheet-with-types.xlsx",
     "0:4e396e3f162e482d/1;1:none;|0:4e396e3f162e482d/1;1:none;"),
    ("fixture-single-sheet.xlsx",
     "0:a6eb59da5730a809/1;1:none;|0:a6eb59da5730a809/1;1:none;"),
    ("fixture-type-errors.xlsx",
     "0:7c50ade80e73a6c9/1;1:none;|0:7c50ade80e73a6c9/1;1:none;"),
    ("infer-dtypes-fallback.xlsx",
     "0:965750dde13897f9/3;1:none;|0:61fb05aaa3b56f44/187;1:none;"),
    ("no-header.xlsx",
     "0:1fb9d3fb0270a726/1;1:none;|0:1fb9d3fb0270a726/1;1:none;"),
    ("null-column.xlsx",
     "0:bae28ba675569b9c/1;1:none;|0:bae28ba675569b9c/1;1:none;"),
    ("sheet-and-table-with-offset.xlsx",
     "0:dba330cfb92f93bd/1;1:12cf36206307ff66/1;2:none;|0:dba330cfb92f93bd/1;1:12cf36206307ff66/1;2:none;"),
    ("sheet-and-table-with-whitespace.xlsx",
     "0:16e3162c05f3e1e5/1;1:a4bc71982af9647d/1;2:none;|0:16e3162c05f3e1e5/1;1:a4bc71982af9647d/1;2:none;"),
    ("sheet-null-strings-empty.xlsx",
     "0:5641a303e26b4bfc/1;1:none;|0:5641a303e26b4bfc/1;1:none;"),
    ("sheet-null-strings.xlsx",
     "0:bc7c877ec8e29616/1;1:none;|0:bc7c877ec8e29616/1;1:none;"),
    ("sheet-with-defined-names.xlsx",
     "0:4c65b8ab9d12567b/1;1:none;|0:4c65b8ab9d12567b/1;1:none;"),
    ("sheet-with-na.xlsx",
     "0:f31d542244f24703/1;1:b7322495a19877c8/1;2:none;|0:f31d542244f24703/1;1:b7322495a19877c8/1;2:none;"),
    ("sheet-with-tables.xlsx",
     "0:a6b88f381efbacdf/1;1:none;|0:a6b88f381efbacdf/1;1:none;"),
    ("single-sheet-skip-rows-durations.xlsx",
     "0:daeef8368886e5bf/1;1:none;|0:daeef8368886e5bf/1;1:none;"),
];
const GOLDEN_BENCH: &[(&str, &str)] = &[
    ("fixture_v1_medium.xlsx",
     "0:92f02c3cd837f2ac/7;1:none;|0:c7ba5b7f8b86ae41/446;1:none;"),
    ("fixture_v1_small.xlsx",
     "0:f24aee5d79fc1580/1;1:none;|0:3c3cb5bc6dc13f13/5;1:none;"),
];
const GOLDEN_BENCH_LARGE: &[(&str, &str)] = &[
    ("fixture_v1_large.xlsx",
     "0:be6e7ff4c18f1491/73;1:none;|0:f77e9fceef98521f/4620;1:none;"),
];

// ---------------------------------------------------------------------------
// Synthetic hostile worksheets: shapes the real fixtures do not contain
// (non-canonical cell tags, odd attribute order/quoting, sparse/unordered
// rows, errors, inline strings, entities, row metadata, ...). The stream
// output for each is pinned by digest, so the fast paths are proven equal to
// the general paths on input they were NOT tuned for.
// ---------------------------------------------------------------------------

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn col_letters(mut c: usize) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'A' + (c % 26) as u8);
        if c < 26 {
            break;
        }
        c = c / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).unwrap()
}

const SHARED: &[&str] = &[
    "id", "name", "city", "note", "alpha", "beta &amp; gamma", "x&lt;y", "  spaced  ", "", "ünï",
    "line one", "9", "1e3", "TRUE",
];

fn random_sheet(seed: u64, rows: usize, ncols: usize) -> String {
    let mut g = Lcg(seed);
    let mut x = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><dimension ref=\"A1\"/><sheetData>",
    );
    for r in 1..=rows {
        // row tag variants
        match g.below(12) {
            0 => x.push_str(&format!("<row r=\"{r}\" spans=\"1:{ncols}\" ht=\"20\" customHeight=\"1\">")),
            1 => x.push_str(&format!("<row r=\"{r}\" hidden=\"1\">")),
            2 => x.push_str(&format!("<row r=\"{r}\" outlineLevel=\"1\" collapsed=\"1\">")),
            3 => x.push_str("<row>"),
            4 => x.push_str(&format!("<row r='{r}'>")),
            5 if r > 1 => {
                x.push_str(&format!("<row r=\"{r}\"/>"));
                continue;
            }
            6 => x.push_str(&format!("<row r=\"{r}\" s=\"2\" customFormat=\"1\" x14ac:dyDescent=\"0.25\">")),
            _ => x.push_str(&format!("<row r=\"{r}\" spans=\"1:{ncols}\">")),
        }
        for c in 0..ncols {
            if r > 1 && g.below(9) == 0 {
                continue; // sparse
            }
            let col = col_letters(c);
            let rr = format!("{col}{r}");
            if r == 1 {
                let si = g.below(SHARED.len() as u64);
                x.push_str(&format!("<c r=\"{rr}\" t=\"s\"><v>{si}</v></c>"));
                continue;
            }
            let v = match g.below(30) {
                0 => format!("<c r=\"{rr}\"/>"),
                1 => format!("<c r=\"{rr}\" s=\"1\"/>"),
                2 => format!("<c r=\"{rr}\"><v></v></c>"),
                3 => format!("<c r=\"{rr}\"></c>"),
                4 => format!("<c r=\"{rr}\" t=\"e\"><v>#DIV/0!</v></c>"),
                5 => format!("<c r=\"{rr}\" t=\"e\"><v>#N/A</v></c>"),
                6 => format!("<c r=\"{rr}\" t=\"b\"><v>{}</v></c>", g.below(2)),
                7 => format!("<c r=\"{rr}\" t=\"inlineStr\"><is><t>in {r} &amp; {c}</t></is></c>"),
                8 => format!("<c r=\"{rr}\" t=\"str\"><v>computed {r}</v></c>"),
                9 => format!("<c r=\"{rr}\" t=\"str\"><f>A1&amp;B1</f><v>f {r}</v></c>"),
                10 => format!("<c r=\"{rr}\"><f>SUM(A1:A2)</f><v>{}</v></c>", g.below(1000)),
                11 => format!("<c r=\"{rr}\" t=\"d\"><v>2024-0{}-1{}</v></c>", 1 + g.below(9), g.below(9)),
                12 => format!("<c t=\"s\" r=\"{rr}\"><v>{}</v></c>", g.below(SHARED.len() as u64)),
                13 => format!("<c s=\"3\" r=\"{rr}\" t=\"s\"><v>{}</v></c>", g.below(SHARED.len() as u64)),
                14 => format!("<c r=\"{rr}\" cm=\"1\"><v>{}.5</v></c>", g.below(100)),
                15 => format!("<c r='{rr}' t='s'><v>{}</v></c>", g.below(SHARED.len() as u64)),
                16 => format!("<c r=\"{rr}\" t=\"s\"><v>{}</v></c>", 500 + g.below(10)), // OOB shared idx
                17 => format!("<c><v>{}</v></c>", g.below(50)), // no r
                18 => format!("<c r=\"{}\"><v>1</v></c>", rr.to_lowercase()),
                19 => format!("<c r=\"ABCD{r}\"><v>7</v></c>"), // 4-letter col
                20 => format!("<c r=\"{rr}\" foo=\"a>b\" t=\"s\"><v>{}</v></c>", g.below(SHARED.len() as u64)),
                21 => format!("<c r=\"{rr}\" t=\"s\" t=\"str\"><v>3</v></c>"), // duplicate t
                22 => format!("<c r=\"{rr}\"><v>{}</v><extLst/></c>", g.below(9)),
                23 => format!("<c r=\"{rr}\" t=\"n\"><v>-{}.25e2</v></c>", g.below(9)),
                24 => format!("<c r=\"{rr}\"><v>not a number</v></c>"),
                25 => format!("<c r=\"{rr}\" t=\"s\"><v>abc</v></c>"),
                26 => format!("<c r=\"{rr}\" s=\"1\" t=\"s\"><v>{}</v></c>", g.below(SHARED.len() as u64)),
                27 => format!("<c r=\"{rr}\" t=\"inlineStr\"><is><r><t>rich</t></r><r><t> text</t></r></is></c>"),
                _ => format!("<c r=\"{rr}\"><v>{}</v></c>", g.below(100000)),
            };
            x.push_str(&v);
        }
        x.push_str("</row>");
    }
    x.push_str("</sheetData></worksheet>");
    x
}

fn synthetic_sheets() -> Vec<String> {
    let pre = "<?xml version=\"1.0\"?><worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData>";
    let post = "</sheetData></worksheet>";
    let wrap = |body: &str| format!("{pre}{body}{post}");
    let mut v = vec![
        // 0: sparse + out-of-order rows, gaps
        wrap("<row r=\"1\"><c r=\"A1\" t=\"s\"><v>0</v></c><c r=\"D1\" t=\"s\"><v>1</v></c></row><row r=\"4\"><c r=\"C4\"><v>5</v></c></row><row r=\"2\"><c r=\"A2\"><v>1</v></c></row><row r=\"9\"><c r=\"D9\" t=\"s\"><v>2</v></c><c r=\"B9\"><v>2.5</v></c></row>"),
        // 1: no r anywhere
        wrap("<row><c t=\"s\"><v>0</v></c><c t=\"s\"><v>1</v></c></row><row><c><v>1</v></c><c t=\"s\"><v>4</v></c></row><row><c><v>2</v></c><c/></row>"),
        // 2: single-quoted + exotic ordering
        wrap("<row r='1'><c r='A1' t='s'><v>0</v></c><c r='B1' t='s'><v>1</v></c></row><row r='2'><c r='A2' t='s'><v>4</v></c><c t='s' r='B2'><v>5</v></c></row>"),
        // 3: empties / self-closing rows+cells / formula only
        wrap("<row r=\"1\"><c r=\"A1\" t=\"s\"><v>0</v></c></row><row r=\"2\"/><row r=\"3\"><c r=\"A3\"/><c r=\"B3\"><f>1+1</f></c></row><row r=\"4\"></row><row r=\"5\"><c r=\"A5\"><v/></c></row>"),
        // 4: entity-rich strings, OOB shared
        wrap("<row r=\"1\"><c r=\"A1\" t=\"s\"><v>5</v></c><c r=\"B1\" t=\"s\"><v>6</v></c></row><row r=\"2\"><c r=\"A2\" t=\"s\"><v>99</v></c><c r=\"B2\" t=\"s\"><v>9</v></c></row><row r=\"3\"><c r=\"A3\" t=\"inlineStr\"><is><t xml:space=\"preserve\"> a &amp; b </t></is></c></row>"),
        // 5: numeric-first, no string header
        wrap("<row r=\"1\"><c r=\"A1\"><v>1</v></c><c r=\"B1\"><v>2</v></c></row><row r=\"2\"><c r=\"A2\"><v>3.5</v></c><c r=\"B2\" t=\"b\"><v>1</v></c></row>"),
        // 6: only a header
        wrap("<row r=\"1\"><c r=\"A1\" t=\"s\"><v>0</v></c></row>"),
        // 7: empty sheetData
        "<?xml version=\"1.0\"?><worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData/></worksheet>".to_string(),
        // 8: row metadata variants incl. single-quoted + unusual spacing
        wrap("<row r=\"1\" spans=\"1:2\"><c r=\"A1\" t=\"s\"><v>0</v></c></row><row r=\"2\" ht=\"30\" customHeight=\"1\"><c r=\"A2\"><v>1</v></c></row><row r=\"3\" hidden='1'><c r=\"A3\"><v>2</v></c></row><row r=\"4\"  spans=\"1:1\"><c r=\"A4\"><v>3</v></c></row><row r=\"5\" thickBot=\"1\" outlineLevel=\"2\"><c r=\"A5\"><v>4</v></c></row><row r=\"6\" s=\"1\" customFormat=\"1\"><c r=\"A6\"><v>5</v></c></row>"),
    ];
    v.push(random_sheet(1, 40, 6));
    v.push(random_sheet(7, 3000, 9)); // > 64 KiB: multi-window
    v.push(random_sheet(99, 9000, 14));
    v
}

fn write_synthetic(path: &std::path::Path) {
    use std::io::Write as _;
    use zip::write::SimpleFileOptions;
    let sheets = synthetic_sheets();
    let f = std::fs::File::create(path).unwrap();
    let mut z = zip::ZipWriter::new(f);
    let o = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut wb = String::from("<?xml version=\"1.0\"?><workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets>");
    let mut rels = String::from("<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">");
    for i in 0..sheets.len() {
        wb.push_str(&format!("<sheet name=\"S{i}\" sheetId=\"{}\" r:id=\"rId{}\"/>", i + 1, i + 1));
        rels.push_str(&format!("<Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet{}.xml\"/>", i + 1, i + 1));
    }
    wb.push_str("</sheets></workbook>");
    rels.push_str("</Relationships>");
    z.start_file("xl/workbook.xml", o).unwrap();
    z.write_all(wb.as_bytes()).unwrap();
    z.start_file("xl/_rels/workbook.xml.rels", o).unwrap();
    z.write_all(rels.as_bytes()).unwrap();
    let mut sst = String::from("<?xml version=\"1.0\"?><sst xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">");
    for s in SHARED {
        sst.push_str(&format!("<si><t>{s}</t></si>"));
    }
    sst.push_str("</sst>");
    z.start_file("xl/sharedStrings.xml", o).unwrap();
    z.write_all(sst.as_bytes()).unwrap();
    for (i, s) in sheets.iter().enumerate() {
        z.start_file(format!("xl/worksheets/sheet{}.xml", i + 1), o).unwrap();
        z.write_all(s.as_bytes()).unwrap();
    }
    z.finish().unwrap();
}

#[test]
fn stream_output_matches_golden_synthetic() {
    let path = std::env::temp_dir().join(format!("kyrax_synth_{}.xlsx", std::process::id()));
    write_synthetic(&path);
    let p = path.to_string_lossy().into_owned();
    // `sheets_digest` stops at the first sheet that fails to open/stream, so
    // digest each sheet independently here.
    let mut got = String::new();
    for sheet in 0..16 {
        let a = digest(&p, sheet, &StreamOptions::default());
        let b = digest(&p, sheet, &small_windows());
        got.push_str(&format!("{sheet}:{a:?}|{b:?};"));
    }
    let _ = std::fs::remove_file(&path);
    if std::env::var("KYRAX_PRINT_GOLDEN").is_ok() {
        println!("GOLDEN_SYNTH {got}");
        return;
    }
    assert_eq!(got, GOLDEN_SYNTH, "synthetic stream output changed");
}

const GOLDEN_SYNTH: &str = concat!(
    "0:Some((17464195968080072544, 1))|Some((17464195968080072544, 1));1:Some((8794923314412140749, 1))|S",
    "ome((8794923314412140749, 1));2:Some((13133120563409903823, 1))|Some((13133120563409903823, 1));3:So",
    "me((131021050556314339, 1))|Some((131021050556314339, 1));4:Some((8121592293107885874, 1))|Some((812",
    "1592293107885874, 1));5:Some((15161299101531238821, 1))|Some((15161299101531238821, 1));6:Some((9334",
    "932443731930425, 1))|Some((9334932443731930425, 1));7:Some((17348109739923791995, 1))|Some((17348109",
    "739923791995, 1));8:Some((9389813672875499542, 1))|Some((9389813672875499542, 1));9:Some((1923591618",
    "184205199, 1))|Some((1923591618184205199, 1));10:Some((3774800143757513924, 1))|Some((70111582792115",
    "1931, 14));11:Some((8305864042636325038, 1))|Some((17830019340390225918, 60));12:None|None;13:None|N",
    "one;14:None|None;15:None|None;",
);
