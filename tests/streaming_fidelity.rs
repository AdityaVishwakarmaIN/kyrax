//! B6 — streaming fidelity: the streaming read must match the eager read
//! cell-for-cell AND schema-for-schema on every corpus file.
//!
//! For every worksheet in every `testdata/*.xlsx|xlsm`, we read it two ways:
//!
//! * eager: `read_workbook_turbo_sheet(path, Features::VALUES, idx)` — sees
//!   the whole sheet before choosing column types.
//! * streaming: `turbo::stream::SheetStream` — runs a type-only pre-pass, then
//!   emits one RecordBatch per bounded window.
//!
//! We assert, for EVERY streaming batch:
//!   1. its field names equal the eager column names,
//!   2. every field's Arrow `DataType` equals the eager column's `DataType`,
//!   3. all batches share the same schema (the pre-pass stabilised it),
//!   4. its cells equal the corresponding eager cells (values, not just
//!      dictionary layout).
//!
//! We also assert the aggregated sparse results (cell errors, row dimensions)
//! match the eager path, and that the batch windowing is exercised with BOTH
//! the default window and a tiny window so row-boundary framing is covered.

use std::path::{Path, PathBuf};

use arrow_array::types::Int32Type;
use arrow_array::{Array, ArrayRef, DictionaryArray, Float64Array, RecordBatch, StringArray};
use kyrax::turbo::{CellError, Features, SheetStream, StreamOptions, read_workbook_turbo_sheet};

// ---------------------------------------------------------------------------
// Corpus discovery
// ---------------------------------------------------------------------------

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

fn discover_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(corpus_dir()) {
        for e in rd.flatten() {
            let p = e.path();
            let ext = p
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if ext == "xlsx" || ext == "xlsm" {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn stem_of(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("file")
        .to_string()
}

/// Deliberately-malformed negative fixtures (the validator's job, not the
/// reader's) are excluded from fidelity.
fn is_negative_fixture(stem: &str) -> bool {
    stem.starts_with("stress2_malformed")
}

// ---------------------------------------------------------------------------
// Cell comparison
// ---------------------------------------------------------------------------

/// Normalise one cell of an Arrow column to (null, value-as-f64-or-utf8).
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

#[derive(Clone, PartialEq, Debug)]
enum CellVal {
    Num(f64),
    Str(String),
}

/// Compare streaming batches against the eager sheet's columns.
fn check_sheet(
    stem: &str,
    sheet_name: &str,
    opts: &StreamOptions,
    stream: &mut SheetStream,
    eager_cols: &[ArrayRef],
    eager_names: &[String],
    eager_errors: &[CellError],
) -> Vec<String> {
    let mut findings = Vec::new();

    // Column count / names / types from the first batch.
    let mut batches: Vec<RecordBatch> = Vec::new();
    loop {
        match stream.next_batch(opts) {
            Ok(Some(b)) => batches.push(b),
            Ok(None) => break,
            Err(e) => {
                findings.push(format!("[{stem}/{sheet_name}] streaming error: {e}"));
                return findings;
            }
        }
    }

    if batches.is_empty() {
        findings.push(format!(
            "[{stem}/{sheet_name}] streaming yielded no batches for a {}x{} sheet",
            eager_cols.len(),
            eager_names.len()
        ));
        return findings;
    }

    // Schema equality across all batches + with eager.
    for (k, b) in batches.iter().enumerate() {
        if b.num_columns() != eager_cols.len() {
            findings.push(format!(
                "[{stem}/{sheet_name}] batch {k} has {} columns, eager has {}",
                b.num_columns(),
                eager_cols.len()
            ));
        }
        for c in 0..eager_cols.len().min(b.num_columns()) {
            let ename = &eager_names[c];
            let schema = b.schema();
            let bname = schema.field(c).name();
            if bname != ename {
                findings.push(format!(
                    "[{stem}/{sheet_name}] batch {k} column {c} name {bname:?} != eager {ename:?}"
                ));
            }
            let btype = b.column(c).data_type();
            let etype = eager_cols[c].data_type();
            if btype != etype {
                findings.push(format!(
                    "[{stem}/{sheet_name}] batch {k} column {c} type {btype} != eager {etype}"
                ));
            }
        }
        if k > 0 && batches[k].schema() != batches[0].schema() {
            findings.push(format!(
                "[{stem}/{sheet_name}] batch {k} schema differs from batch 0"
            ));
        }
    }

    // Cell-by-cell values (row-aligned across batches).
    let mut offset = 0usize;
    for (k, b) in batches.iter().enumerate() {
        for c in 0..eager_cols.len().min(b.num_columns()) {
            let bcol = b.column(c).as_ref();
            let ecol = eager_cols[c].as_ref();
            let n = bcol.len();
            if offset + n > ecol.len() {
                findings.push(format!(
                    "[{stem}/{sheet_name}] batch {k} col {c} runs past eager rows"
                ));
                continue;
            }
            for i in 0..n {
                let (bn, bv) = cell_of(bcol, i);
                let (en, ev) = cell_of(ecol, offset + i);
                if bn != en || bv != ev {
                    findings.push(format!(
                        "[{stem}/{sheet_name}] batch {k} col {c} row {}: streaming {:?} != eager {:?}",
                        offset + i,
                        if bn { None } else { bv },
                        if en { None } else { ev }
                    ));
                }
            }
        }
        offset += b.num_rows();
    }

    // Aggregates (cell errors, row dims, totals).
    let agg = stream.summary();
    let eager_nrows = eager_cols.first().map(|c| c.len()).unwrap_or(0);
    if agg.nrows != eager_nrows {
        findings.push(format!(
            "[{stem}/{sheet_name}] streaming nrows {} != eager {eager_nrows}",
            agg.nrows
        ));
    }
    if agg.ncols != eager_cols.len() {
        findings.push(format!(
            "[{stem}/{sheet_name}] streaming ncols {} != eager {}",
            agg.ncols,
            eager_cols.len()
        ));
    }
    if agg.cell_errors.len() != eager_errors.len() {
        findings.push(format!(
            "[{stem}/{sheet_name}] streaming cell_errors {} != eager {}",
            agg.cell_errors.len(),
            eager_errors.len()
        ));
    } else {
        for (a, b) in agg.cell_errors.iter().zip(eager_errors.iter()) {
            if a.row != b.row || a.col != b.col || a.code != b.code {
                findings.push(format!(
                    "[{stem}/{sheet_name}] cell error {:?} != eager {:?}",
                    a, b
                ));
            }
        }
    }
    let _ = &agg.column_names;

    findings
}

// ---------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------

fn run_all(opts: &StreamOptions) -> Vec<String> {
    let mut findings = Vec::new();
    for path in discover_files() {
        let stem = stem_of(&path);
        if is_negative_fixture(&stem) {
            continue;
        }
        let p = path.to_str().unwrap();
        let eager = match read_workbook_turbo_sheet(p, Features::VALUES, 0) {
            Ok(w) => w,
            Err(e) => {
                findings.push(format!("[{stem}] eager open failed: {e}"));
                continue;
            }
        };
        let ns = eager.sheets.len();
        for sheet_idx in 0..ns {
            let sheet = &eager.sheets[sheet_idx];
            let eager_cols = sheet.columns.clone();
            let eager_names = sheet.column_names.clone();
            let eager_errors = sheet.cell_errors.clone();
            let name = sheet.name.clone();
            let mut stream = match SheetStream::open(p, sheet_idx, opts.clone()) {
                Ok(s) => s,
                Err(e) => {
                    findings.push(format!("[{stem}/{name}] streaming open failed: {e}"));
                    continue;
                }
            };
            findings.extend(check_sheet(
                &stem,
                &name,
                opts,
                &mut stream,
                &eager_cols,
                &eager_names,
                &eager_errors,
            ));
        }
    }
    findings
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn streaming_fidelity_default_window() {
    let findings = run_all(&StreamOptions::default());
    assert_clean(&findings);
}

#[test]
fn streaming_fidelity_tiny_window() {
    // A tiny window forces rows to be split across many windows, exercising
    // the carry/framing logic hard.
    let opts = StreamOptions {
        batch_rows: 1,
        window_bytes: 48 * 1024,
        max_row_bytes: 4 * 1024 * 1024,
        max_pre_bytes: 64 * 1024,
    };
    let findings = run_all(&opts);
    assert_clean(&findings);
}

fn assert_clean(findings: &[String]) {
    if !findings.is_empty() {
        let mut msg = format!("streaming fidelity FAILED ({} findings):", findings.len());
        for f in findings.iter().take(20) {
            msg.push_str("\n  ");
            msg.push_str(f);
        }
        panic!("{msg}");
    }
}
