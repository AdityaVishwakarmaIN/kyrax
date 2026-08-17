//! Regression: the streaming read path (`SheetStream`) must agree with the
//! eager read path (`read_workbook_turbo_sheet`) on the mixed and charts
//! fixtures. Formerly an ad-hoc debug probe; now a bounded, non-panicking
//! regression test: each fixture is read independently, mismatches are
//! collected and reported in one place, and batch iteration is capped
//! defensively so a non-terminating stream cannot hang the suite.
#![cfg(feature = "__arrow")]

use arrow_array::types::Int32Type;
use arrow_array::{Array, DictionaryArray, Float64Array, StringArray};
use kyrax::turbo::{Features, SheetStream, StreamOptions, read_workbook_turbo_sheet};

fn testdata(name: &str) -> String {
    format!("{}/testdata/{}", env!("CARGO_MANIFEST_DIR"), name)
}

/// Defensive cap on total batches per fixture: a real sheet needs only a
/// handful; more than this means the stream never terminates.
const MAX_BATCHES_PER_FIXTURE: usize = 10_000;

#[derive(Clone, PartialEq, Debug)]
enum CellVal {
    Num(f64),
    Str(String),
}

/// Normalise one cell of an Arrow column to (null, value).
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
    (false, None)
}

fn check_fixture(stem: &str, path: &str, findings: &mut Vec<String>) {
    let eager = match read_workbook_turbo_sheet(path, Features::VALUES, 0) {
        Ok(wb) => wb,
        Err(e) => {
            findings.push(format!("[{stem}] eager read failed: {e}"));
            return;
        }
    };
    let sheet = &eager.sheets[0];

    let opts = StreamOptions::default();
    let mut stream = match SheetStream::open(path, 0, opts.clone()) {
        Ok(s) => s,
        Err(e) => {
            findings.push(format!("[{stem}] stream open failed: {e}"));
            return;
        }
    };

    let mut batches = Vec::new();
    loop {
        if batches.len() >= MAX_BATCHES_PER_FIXTURE {
            findings.push(format!(
                "[{stem}] exceeded {MAX_BATCHES_PER_FIXTURE} batches; stream did not terminate"
            ));
            return;
        }
        match stream.next_batch(&opts) {
            Ok(Some(b)) => batches.push(b),
            Ok(None) => break,
            Err(e) => {
                findings.push(format!("[{stem}] streaming error: {e}"));
                return;
            }
        }
    }

    if batches.is_empty() {
        findings.push(format!(
            "[{stem}] streaming yielded no batches for a {}x{} sheet",
            sheet.nrows, sheet.ncols
        ));
        return;
    }

    // Per-batch schema and cell values vs the eager sheet.
    let mut offset = 0usize;
    for (k, b) in batches.iter().enumerate() {
        if b.num_columns() != sheet.ncols {
            findings.push(format!(
                "[{stem}] batch {k} has {} columns, eager has {}",
                b.num_columns(),
                sheet.ncols
            ));
        }
        for c in 0..sheet.columns.len().min(b.num_columns()) {
            let schema = b.schema();
            let bname = schema.field(c).name();
            if bname.as_str() != sheet.column_names[c].as_str() {
                findings.push(format!(
                    "[{stem}] batch {k} column {c} name {bname:?} != eager {:?}",
                    sheet.column_names[c]
                ));
            }
            let bcol = b.column(c).as_ref();
            let ecol = sheet.columns[c].as_ref();
            if bcol.data_type() != ecol.data_type() {
                findings.push(format!(
                    "[{stem}] batch {k} column {c} type {} != eager {}",
                    bcol.data_type(),
                    ecol.data_type()
                ));
            }
            if offset + bcol.len() > ecol.len() {
                findings.push(format!(
                    "[{stem}] batch {k} column {c} runs past eager rows"
                ));
                continue;
            }
            for i in 0..bcol.len() {
                let (bn, bv) = cell_of(bcol, i);
                let (en, ev) = cell_of(ecol, offset + i);
                if bn != en || bv != ev {
                    findings.push(format!(
                        "[{stem}] batch {k} col {c} row {}: stream {:?} != eager {:?}",
                        offset + i,
                        if bn { None } else { bv },
                        if en { None } else { ev }
                    ));
                }
            }
        }
        offset += b.num_rows();
    }

    let agg = stream.summary();
    if agg.nrows != sheet.nrows {
        findings.push(format!(
            "[{stem}] streaming nrows {} != eager {}",
            agg.nrows, sheet.nrows
        ));
    }
    if agg.ncols != sheet.ncols {
        findings.push(format!(
            "[{stem}] streaming ncols {} != eager {}",
            agg.ncols, sheet.ncols
        ));
    }
}

#[test]
fn streaming_matches_eager_on_probe_fixtures() {
    let mut findings = Vec::new();
    check_fixture("mixed", &testdata("mixed.xlsx"), &mut findings);
    check_fixture("charts", &testdata("charts.xlsx"), &mut findings);
    assert!(
        findings.is_empty(),
        "streaming/eager mismatches:\n{}",
        findings.join("\n")
    );
}
