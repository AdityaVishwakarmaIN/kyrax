//! Stage -1 direct-evaluator classification harness.
//!
//! For every fail row in the in-repo fixture `tests_formulas/fail_rows.csv`
//! (a small, hermetic slice of the round-2 fail set; one row per function
//! flagged `fail` in the external `formula-validation/matrix.csv`), evaluate
//! the formula DIRECTLY in the kyrax calc engine — parse → dependency order →
//! eval → cache — against an in-memory single-sheet workbook built from the
//! row's context, and NEVER through the Excel write path (no XML, no file, no
//! round-trip). The fixture is owned by this harness and must never be replaced
//! by a path outside the crate: the external `formula-validation/` directory is
//! git-ignored and absent in clean CI.
//!
//! The verdict splits matrix failures into:
//!   * `MATCH`     — the engine computes exactly the oracle `expected` value;
//!     the matrix failure was a write/hydration artifact, not a
//!     calc-engine defect.
//!   * `MISMATCH`  — the engine computes something else; candidate calc-engine
//!     defect (or, for rows evaluated against a blank context, a
//!     missing-input artifact — see `context_kind`).
//!   * `EVAL_ERROR`— the engine could not produce a cacheable scalar at all
//!     (unparseable, uncached/fallback, or an array result the
//!     cache layer cannot hold).
//!
//! Evaluation drives the same public surface the write path drives
//! (`kyrax::turbo::calc::hydrate_workbook` on a `write::model::Workbook`); the
//! calc engine's private `eval` is not reachable from an integration test, and
//! `calc::testkit` is `#[cfg(test)]`-gated inside the crate. The write model is
//! used purely as the in-memory grid container — nothing is ever serialized.
//!
//! Rerunnable: `cargo test --test direct_eval` from `kyrax/` re-reads the
//! fixture and overwrites `target/direct_eval_results.csv` with one result row
//! per input row. No external crates: CSV and the small JSON payloads are
//! parsed by the minimal readers below (the crate's dev-dependencies expose no
//! `csv`/`serde`). The fixture's classification is pinned by the assertion in
//! `direct_eval_classification`, so a clean CI run is self-checking.

use kyrax::turbo::calc::{CalcOptions, hydrate_workbook};
use kyrax::turbo::write::{CachedValue, Cell, CellValue, FormulaKind, Row, Sheet, Workbook};
use pretty_assertions::assert_eq;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

// The input fixture lives inside the crate so `cargo test` is hermetic.
const INPUT_REL: &str = "tests_formulas/fail_rows.csv";
// The result CSV is a generated report, so it goes to the gitignored build dir
// (always present by the time the test binary runs) rather than into the tree.
const OUTPUT_REL: &str = "target/direct_eval_results.csv";

// ---------------------------------------------------------------------------
// Minimal RFC-4180 CSV reader (quoted fields, doubled quotes, CRLF tolerant)
// ---------------------------------------------------------------------------

/// Parse a CSV document into rows of unquoted fields.
fn parse_csv(input: &str) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    let mut field = String::new();
    let b: Vec<char> = input.chars().collect();
    let mut i = 0;
    let mut in_quotes = false;
    let mut field_started = false;
    while i < b.len() {
        let c = b[i];
        if in_quotes {
            if c == '"' {
                if i + 1 < b.len() && b[i + 1] == '"' {
                    field.push('"');
                    i += 2;
                    continue;
                }
                in_quotes = false;
                i += 1;
                continue;
            }
            field.push(c);
            i += 1;
            continue;
        }
        match c {
            '"' => {
                if field.is_empty() && !field_started {
                    in_quotes = true;
                    field_started = true;
                } else {
                    // stray quote inside an unquoted field: keep it literally
                    field.push(c);
                }
                i += 1;
            }
            ',' => {
                cur.push(std::mem::take(&mut field));
                field_started = false;
                i += 1;
            }
            '\r' => {
                i += 1; // fold CRLF to LF
            }
            '\n' => {
                cur.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut cur));
                field_started = false;
                i += 1;
            }
            _ => {
                field.push(c);
                field_started = true;
                i += 1;
            }
        }
    }
    if !field.is_empty() || !cur.is_empty() || field_started {
        cur.push(field);
        rows.push(cur);
    }
    rows
}

// ---------------------------------------------------------------------------
// Minimal JSON reader for the machine-generated payloads we own
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum J {
    Obj(Vec<(String, J)>),
    Arr(Vec<J>),
    Str(String),
    Num(f64),
    Bool(bool),
    Null,
}

fn skip_ws(b: &[u8], i: &mut usize) {
    while *i < b.len() && matches!(b[*i], b' ' | b'\t' | b'\r' | b'\n') {
        *i += 1;
    }
}

fn parse_json_str(b: &[u8], i: &mut usize) -> Result<String, String> {
    // caller positions *i at the opening quote
    *i += 1;
    let mut s = String::new();
    while *i < b.len() {
        let c = b[*i];
        if c == b'"' {
            *i += 1;
            return Ok(s);
        }
        if c == b'\\' {
            *i += 1;
            if *i >= b.len() {
                return Err("truncated escape".into());
            }
            match b[*i] {
                b'"' => s.push('"'),
                b'\\' => s.push('\\'),
                b'/' => s.push('/'),
                b'b' => s.push('\u{0008}'),
                b'f' => s.push('\u{000C}'),
                b'n' => s.push('\n'),
                b'r' => s.push('\r'),
                b't' => s.push('\t'),
                b'u' => {
                    if *i + 4 >= b.len() {
                        return Err("truncated \\u".into());
                    }
                    let hex = std::str::from_utf8(&b[*i + 1..*i + 5]).map_err(|e| e.to_string())?;
                    let code = u32::from_str_radix(hex, 16).map_err(|e| e.to_string())?;
                    s.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                    *i += 4;
                }
                other => return Err(format!("bad escape \\{}", other as char)),
            }
            *i += 1;
            continue;
        }
        // copy one UTF-8 scalar
        let ch_len = utf8_len(c);
        let chunk = &b[*i..*i + ch_len];
        s.push_str(std::str::from_utf8(chunk).map_err(|e| e.to_string())?);
        *i += ch_len;
    }
    Err("unterminated string".into())
}

fn utf8_len(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first < 0xE0 {
        2
    } else if first < 0xF0 {
        3
    } else {
        4
    }
}

fn parse_json_number(b: &[u8], i: &mut usize) -> Result<J, String> {
    let start = *i;
    if *i < b.len() && b[*i] == b'-' {
        *i += 1;
    }
    while *i < b.len()
        && (b[*i].is_ascii_digit() || matches!(b[*i], b'.' | b'e' | b'E' | b'+' | b'-'))
    {
        *i += 1;
    }
    let text = std::str::from_utf8(&b[start..*i]).map_err(|e| e.to_string())?;
    let n: f64 = text
        .parse()
        .map_err(|e| format!("bad number {text}: {e}"))?;
    Ok(J::Num(n))
}

/// Parse a JSON value starting at `*i` (which is advanced past it).
fn parse_json_value(b: &[u8], i: &mut usize) -> Result<J, String> {
    skip_ws(b, i);
    if *i >= b.len() {
        return Err("unexpected end of JSON".into());
    }
    match b[*i] {
        b'{' => {
            *i += 1;
            let mut out = Vec::new();
            loop {
                skip_ws(b, i);
                if *i >= b.len() {
                    return Err("unterminated object".into());
                }
                if b[*i] == b'}' {
                    *i += 1;
                    break;
                }
                skip_ws(b, i);
                if b[*i] != b'"' {
                    return Err("object key must be a string".into());
                }
                let key = parse_json_str(b, i)?;
                skip_ws(b, i);
                if b[*i] != b':' {
                    return Err("expected ':'".into());
                }
                *i += 1;
                let val = parse_json_value(b, i)?;
                out.push((key, val));
                skip_ws(b, i);
                if *i < b.len() && b[*i] == b',' {
                    *i += 1;
                }
            }
            Ok(J::Obj(out))
        }
        b'[' => {
            *i += 1;
            let mut out = Vec::new();
            loop {
                skip_ws(b, i);
                if *i >= b.len() {
                    return Err("unterminated array".into());
                }
                if b[*i] == b']' {
                    *i += 1;
                    break;
                }
                out.push(parse_json_value(b, i)?);
                skip_ws(b, i);
                if *i < b.len() && b[*i] == b',' {
                    *i += 1;
                }
            }
            Ok(J::Arr(out))
        }
        b'"' => parse_json_str(b, i).map(J::Str),
        b't' => {
            if b.get(*i..*i + 4) == Some(b"true") {
                *i += 4;
                Ok(J::Bool(true))
            } else {
                Err("bad literal".into())
            }
        }
        b'f' => {
            if b.get(*i..*i + 5) == Some(b"false") {
                *i += 5;
                Ok(J::Bool(false))
            } else {
                Err("bad literal".into())
            }
        }
        b'n' => {
            if b.get(*i..*i + 4) == Some(b"null") {
                *i += 4;
                Ok(J::Null)
            } else {
                Err("bad literal".into())
            }
        }
        c if c == b'-' || c.is_ascii_digit() => parse_json_number(b, i),
        other => Err(format!("unexpected JSON char {:?}", other as char)),
    }
}

fn json_get<'a>(obj: &'a [(String, J)], key: &str) -> Option<&'a J> {
    obj.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn json_str(v: &J) -> Option<&str> {
    match v {
        J::Str(s) => Some(s),
        _ => None,
    }
}

fn json_num(v: &J) -> Option<f64> {
    match v {
        J::Num(n) => Some(*n),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Oracle expected value decoding
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Exp {
    Num(f64),
    Bool(bool),
    Err(String),
    Str(String),
    /// A rectangular dense array of cell values (row-major).
    Arr(u32, u32, Vec<Exp>),
    /// The lane1 `array` marker: oracle expects an array result.
    ArrayMarker,
}

fn decode_cell(j: &J) -> Result<Exp, String> {
    let obj = match j {
        J::Obj(o) => o,
        _ => return Err("cell value must be an object".into()),
    };
    let t = json_str(json_get(obj, "t").ok_or("cell missing t")?).unwrap_or("");
    match t {
        "n" => Ok(Exp::Num(
            json_num(json_get(obj, "v").ok_or("cell missing v")?).unwrap_or(f64::NAN),
        )),
        "b" => match json_get(obj, "v") {
            Some(J::Bool(b)) => Ok(Exp::Bool(*b)),
            _ => Ok(Exp::Bool(false)),
        },
        "e" => Ok(Exp::Err(
            json_str(json_get(obj, "v").ok_or("cell missing v")?)
                .unwrap_or("#ERROR!")
                .to_string(),
        )),
        "s" => Ok(Exp::Str(
            json_str(json_get(obj, "v").ok_or("cell missing v")?)
                .unwrap_or("")
                .to_string(),
        )),
        other => Err(format!("unknown cell type {other}")),
    }
}

fn decode_expected(j: &J) -> Result<Exp, String> {
    let obj = match j {
        J::Obj(o) => o,
        _ => return Err("expected must be an object".into()),
    };
    let t = json_str(json_get(obj, "t").ok_or("expected missing t")?).unwrap_or("");
    match t {
        "array" => Ok(Exp::ArrayMarker),
        "n" => Ok(Exp::Num(
            json_num(json_get(obj, "v").ok_or("expected missing v")?).unwrap_or(f64::NAN),
        )),
        "b" => match json_get(obj, "v") {
            Some(J::Bool(b)) => Ok(Exp::Bool(*b)),
            _ => Ok(Exp::Bool(false)),
        },
        "e" => Ok(Exp::Err(
            json_str(json_get(obj, "v").ok_or("expected missing v")?)
                .unwrap_or("#ERROR!")
                .to_string(),
        )),
        "s" => Ok(Exp::Str(
            json_str(json_get(obj, "v").ok_or("expected missing v")?)
                .unwrap_or("")
                .to_string(),
        )),
        "a" => {
            let rows =
                json_num(json_get(obj, "rows").ok_or("array missing rows")?).unwrap_or(0.0) as u32;
            let cols =
                json_num(json_get(obj, "cols").ok_or("array missing cols")?).unwrap_or(0.0) as u32;
            let mut data = Vec::new();
            if let Some(J::Arr(items)) = json_get(obj, "data") {
                for it in items {
                    data.push(decode_cell(it)?);
                }
            }
            Ok(Exp::Arr(rows, cols, data))
        }
        other => Err(format!("unknown expected type {other}")),
    }
}

// ---------------------------------------------------------------------------
// A1 addressing
// ---------------------------------------------------------------------------

/// `A1`-style ref -> (1-based row, 1-based col). Letters may be preceded by
/// `$`; case-insensitive.
fn a1_to_rc(ref_str: &str) -> (u32, u32) {
    let s = ref_str.trim().trim_start_matches('$');
    let mut letters = String::new();
    let mut digits = String::new();
    let mut saw_digit = false;
    for ch in s.chars() {
        if ch.is_ascii_alphabetic() && !saw_digit {
            letters.push(ch);
        } else if ch.is_ascii_digit() {
            saw_digit = true;
            digits.push(ch);
        }
    }
    let mut col = 0u32;
    for ch in letters.to_ascii_uppercase().chars() {
        col = col * 26 + (ch as u32 - 'A' as u32 + 1);
    }
    let row: u32 = digits.parse().unwrap_or(1);
    (row, col)
}

// ---------------------------------------------------------------------------
// Workbook building from the row's context
// ---------------------------------------------------------------------------

fn jval_to_cell_value(j: &J) -> Result<CellValue, String> {
    let obj = match j {
        J::Obj(o) => o,
        _ => return Err("context cell must be an object".into()),
    };
    let t = json_str(json_get(obj, "t").ok_or("cell missing t")?).unwrap_or("");
    let v = json_get(obj, "v").ok_or("cell missing v")?;
    match t {
        "n" => Ok(CellValue::Number(json_num(v).unwrap_or(f64::NAN))),
        "b" => match v {
            J::Bool(b) => Ok(CellValue::Bool(*b)),
            _ => Ok(CellValue::Bool(false)),
        },
        "e" => Ok(CellValue::Error(
            json_str(v).unwrap_or("#ERROR!").to_string(),
        )),
        "s" => Ok(CellValue::Str(json_str(v).unwrap_or("").to_string())),
        other => Err(format!("unknown context cell type {other}")),
    }
}

/// Build a one-sheet workbook: data cells from the context plus one formula
/// cell at the anchor. Row/col are 1-based in the write model.
fn build_workbook(formula: &str, anchor: &str, cells: &[(String, J)]) -> Result<Workbook, String> {
    let mut rows_map: BTreeMap<u32, Vec<(u32, CellValue)>> = BTreeMap::new();
    for (ref_str, jval) in cells {
        let (r, c) = a1_to_rc(ref_str);
        rows_map
            .entry(r)
            .or_default()
            .push((c, jval_to_cell_value(jval)?));
    }
    let (fr, fc) = a1_to_rc(anchor);
    rows_map.entry(fr).or_default().push((
        fc,
        CellValue::Formula {
            text: formula.to_string(),
            kind: FormulaKind::Normal,
            cached: None,
        },
    ));

    let mut sheet = Sheet::new("Sheet1");
    let mut rows: Vec<Row> = Vec::new();
    for (r, mut cs) in rows_map {
        cs.sort_by_key(|(c, _)| *c);
        cs.dedup_by_key(|(c, _)| *c);
        let mut row = Row::new(r);
        row.cells = cs.into_iter().map(|(c, v)| Cell::new(c, v)).collect();
        rows.push(row);
    }
    sheet.rows = rows;
    let mut wb = Workbook::new();
    wb.sheets = vec![sheet];
    Ok(wb)
}

/// Read the formula cell's cached value back out of the hydrated workbook.
fn cached_of(wb: &Workbook, anchor: &str) -> Option<CachedValue> {
    let (fr, fc) = a1_to_rc(anchor);
    let sheet = wb.sheets.first()?;
    for row in &sheet.rows {
        if row.row == fr {
            for cell in &row.cells {
                if cell.col == fc {
                    if let CellValue::Formula { cached, .. } = &cell.value {
                        return cached.clone();
                    }
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Comparison
// ---------------------------------------------------------------------------

fn num_close(a: f64, b: f64) -> bool {
    if a == b {
        return true;
    }
    if a.is_nan() || b.is_nan() || a.is_infinite() || b.is_infinite() {
        return false;
    }
    let scale = a.abs().max(b.abs()).max(1.0);
    (a - b).abs() <= 1e-9 * scale
}

fn exp_close(exp: &Exp, got: &Exp) -> bool {
    match (exp, got) {
        (Exp::Num(x), Exp::Num(y)) => num_close(*x, *y),
        (Exp::Bool(x), Exp::Bool(y)) => x == y,
        (Exp::Err(x), Exp::Err(y)) => x == y,
        (Exp::Str(x), Exp::Str(y)) => x == y,
        (Exp::Arr(er, ec, ed), Exp::Arr(gr, gc, gd)) => {
            er == gr
                && ec == gc
                && ed.len() == gd.len()
                && ed.iter().zip(gd.iter()).all(|(a, b)| exp_close(a, b))
        }
        // a 1x1 array and a bare scalar compare equal (Excel's scalar form)
        (Exp::Arr(1, 1, d), s) => d.first().map(|x| exp_close(x, s)).unwrap_or(false),
        (s, Exp::Arr(1, 1, d)) => d.first().map(|x| exp_close(s, x)).unwrap_or(false),
        _ => false,
    }
}

/// Render a CachedValue as the canonical JSON a downstream consumer can reuse.
fn got_json(cached: &CachedValue) -> String {
    match cached {
        CachedValue::Number(n) => {
            let body = if n.fract() == 0.0 && n.abs() < 1e15 {
                format!("{}", *n as i64)
            } else {
                format!("{n}")
            };
            format!("{{\"t\":\"n\",\"v\":{body}}}")
        }
        CachedValue::Bool(b) => format!("{{\"t\":\"b\",\"v\":{b}}}"),
        CachedValue::Error(s) => format!("{{\"t\":\"e\",\"v\":\"{s}\"}}"),
        CachedValue::Str(s) => format!("{{\"t\":\"s\",\"v\":\"{}\"}}", escape_json(s)),
    }
}

fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Minimal CSV writer
// ---------------------------------------------------------------------------

fn csv_escape(field: &str) -> String {
    if field.contains(',') || field.contains('"') || field.contains('\n') || field.contains('\r') {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

fn write_results_csv(path: &Path, rows: &[Vec<String>]) -> std::io::Result<()> {
    let mut out = String::new();
    for row in rows {
        let line: Vec<String> = row.iter().map(|f| csv_escape(f)).collect();
        out.push_str(&line.join(","));
        out.push('\n');
    }
    fs::write(path, out)
}

// ---------------------------------------------------------------------------
// The classification pass
// ---------------------------------------------------------------------------

struct RowInput {
    function: String,
    formula: String,
    expected_text: String,
    expected: Exp,
    anchor: String,
    cells: Vec<(String, J)>,
    has_data: bool,
    source: String,
}

fn decode_context(j: &J) -> Result<(String, Vec<(String, J)>), String> {
    let obj = match j {
        J::Obj(o) => o,
        _ => return Err("context must be an object".into()),
    };
    let anchor = json_str(json_get(obj, "anchor").ok_or("context missing anchor")?)
        .unwrap_or("A1")
        .to_string();
    let mut cells = Vec::new();
    if let Some(J::Obj(map)) = json_get(obj, "cells") {
        for (k, v) in map {
            cells.push((k.clone(), v.clone()));
        }
    }
    Ok((anchor, cells))
}

fn classify_one(row: &RowInput) -> (String, String, String) {
    // (verdict, got, detail)
    let wb = match build_workbook(&row.formula, &row.anchor, &row.cells) {
        Ok(wb) => wb,
        Err(e) => return ("EVAL_ERROR".into(), "MODEL_ERROR".into(), e),
    };
    let mut wb = wb;
    let opts = CalcOptions {
        date1904: false,
        force_recalc: true,
        max_iterations: 0,
    };
    let report = hydrate_workbook(&mut wb, &opts);
    let detail_report = format!(
        "fallback={} computed={} error_cells={} cycles={}",
        report.fallback, report.computed, report.error_cells, report.cycles
    );

    // Array-marker oracles can never surface through the cache layer.
    if matches!(row.expected, Exp::ArrayMarker) {
        return (
            "EVAL_ERROR".into(),
            "UNCACHED".into(),
            format!(
                "oracle expects an array result; the cache layer cannot hold arrays ({detail_report})"
            ),
        );
    }

    let Some(cached) = cached_of(&wb, &row.anchor) else {
        return (
            "EVAL_ERROR".into(),
            "UNCACHED".into(),
            format!("no cached value produced ({detail_report})"),
        );
    };

    let got = match &cached {
        CachedValue::Number(n) => Exp::Num(*n),
        CachedValue::Bool(b) => Exp::Bool(*b),
        CachedValue::Error(s) => Exp::Err(s.clone()),
        CachedValue::Str(s) => Exp::Str(s.clone()),
    };
    let got_str = got_json(&cached);
    if exp_close(&row.expected, &got) {
        (
            "MATCH".into(),
            got_str.clone(),
            format!("engine {got_str} == expected ({detail_report})"),
        )
    } else {
        (
            "MISMATCH".into(),
            got_str.clone(),
            format!("engine {got_str} != expected ({detail_report})"),
        )
    }
}

fn read_input(path: &Path) -> Result<Vec<RowInput>, String> {
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let rows = parse_csv(&text);
    let mut header: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for (ri, row) in rows.iter().enumerate() {
        if ri == 0 {
            header = row.clone();
            continue;
        }
        let mut record: BTreeMap<String, String> = BTreeMap::new();
        for (i, h) in header.iter().enumerate() {
            record.insert(h.clone(), row.get(i).cloned().unwrap_or_default());
        }
        let function = record.get("function").cloned().unwrap_or_default();
        let formula = record.get("formula").cloned().unwrap_or_default();
        let source = record.get("source").cloned().unwrap_or_default();
        let exp_text = record.get("expected").cloned().unwrap_or_default();
        let ctx_text = record.get("context").cloned().unwrap_or_default();
        if formula.is_empty() {
            continue;
        }
        let exp_b = exp_text.as_bytes();
        let mut i = 0;
        let expected = parse_json_value(exp_b, &mut i)
            .and_then(|j| decode_expected(&j))
            .map_err(|e| format!("row {ri}: bad expected: {e}"))?;
        let ctx_b = ctx_text.as_bytes();
        let mut i = 0;
        let ctx = parse_json_value(ctx_b, &mut i)
            .and_then(|j| decode_context(&j))
            .map_err(|e| format!("row {ri}: bad context: {e}"))?;
        let has_data = !ctx.1.is_empty();
        out.push(RowInput {
            function,
            formula,
            expected_text: exp_text,
            expected,
            anchor: ctx.0,
            cells: ctx.1,
            has_data,
            source,
        });
    }
    Ok(out)
}

#[test]
fn direct_eval_classification() {
    let root = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let input_path = root.join(INPUT_REL);
    let output_path = root.join(OUTPUT_REL);
    let inputs = read_input(&input_path).unwrap_or_else(|e| panic!("cannot read {INPUT_REL}: {e}"));

    let mut counts = BTreeMap::new();
    let mut out_rows: Vec<Vec<String>> = vec![vec![
        "function".into(),
        "formula".into(),
        "expected".into(),
        "verdict".into(),
        "got".into(),
        "detail".into(),
        "context_kind".into(),
        "source".into(),
    ]];

    for row in &inputs {
        let (verdict, got, detail) = classify_one(row);
        *counts.entry(verdict.clone()).or_insert(0usize) += 1;
        let context_kind = if row.has_data { "data" } else { "blank" };
        out_rows.push(vec![
            row.function.clone(),
            row.formula.clone(),
            row.expected_text.clone(),
            verdict,
            got,
            detail,
            context_kind.into(),
            row.source.clone(),
        ]);
    }

    write_results_csv(&output_path, &out_rows)
        .unwrap_or_else(|e| panic!("cannot write {OUTPUT_REL}: {e}"));

    // The DoD: one result row per input row.
    assert_eq!(
        out_rows.len() - 1,
        inputs.len(),
        "result rows must equal input rows"
    );

    let mat = counts.get("MATCH").copied().unwrap_or(0);
    let mis = counts.get("MISMATCH").copied().unwrap_or(0);
    let evl = counts.get("EVAL_ERROR").copied().unwrap_or(0);

    // Hermetic regression pin: the fixture is a fixed slice of the round-2 fail
    // set, and its classification must not drift. MATCH = ACCRINT, ARABIC,
    // ARRAYTOTEXT; MISMATCH = CHISQ.TEST, CHOOSE; EVAL_ERROR = MUNIT (array
    // marker), ADDRESS (no cached value).
    assert_eq!(mat, 3, "fixture MATCH count drifted");
    assert_eq!(mis, 2, "fixture MISMATCH count drifted");
    assert_eq!(evl, 2, "fixture EVAL_ERROR count drifted");

    println!(
        "direct_eval: {} input rows -> MATCH={} MISMATCH={} EVAL_ERROR={}",
        inputs.len(),
        mat,
        mis,
        evl
    );
    println!("wrote {OUTPUT_REL}");
}
