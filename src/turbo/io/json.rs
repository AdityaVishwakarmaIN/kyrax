//! JSON / NDJSON import and export (C4b).
//!
//! Both directions, three shapes, no JSON dependency: the reader and writer are
//! hand-rolled and stream. This file is half of the format-interchange unit
//! (the CSV sibling lives in `io/csv.rs`); `io/mod.rs` is wired by the
//! coordinator — until then this module is simply not declared anywhere.
//!
//! # Representations (decisions, documented)
//!
//! * **NULL vs EMPTY STRING vs MISSING KEY.** An Excel grid has no "missing
//!   cell" state, so the three map onto two representable states plus one
//!   documented degradation:
//!   - Excel empty cell ↔ JSON `null` (both mean "no value").
//!   - Excel empty-string cell (`<t></t>`) ↔ JSON `""`.
//!   - JSON missing key → Excel empty cell → `null` on re-export. This is the
//!     only conflation and it is forced by the grid model; it is never merged
//!     with `""`. `null` and `""` round-trip losslessly in both directions.
//!   - Every exported record carries every key (grid semantics); the `null` is
//!     the empty cell, not the omission.
//!
//! * **Escaping.** `"` → `\"`, `\` → `\\`, control characters < 0x20 → `\u00XX`
//!   (lowercase hex), non-ASCII passes through raw as UTF-8. Output is always
//!   valid UTF-8 JSON. (Note: the xlsx WRITE path drops illegal control
//!   characters from cell text by design, so control chars survive JSON↔JSON
//!   but not a full xlsx round-trip; tab/LF/CR do survive.)
//!
//! * **NUMBER FIDELITY.** JSON consumers treat numbers as IEEE-754 doubles, so
//!   an integer beyond 2^53 cannot survive as a number.
//!   - Export: an integral value with `|v| > 2^53` is emitted as a **string**
//!     (shortest round-trip repr of the stored double). Exactly-representable
//!     integers emit as numbers. Silently mangling a 20-digit identifier is
//!     data corruption; a string is explicit.
//!   - Import: a JSON integer whose magnitude exceeds 2^53 is kept as its raw
//!     digit **string** rather than a lossy double.
//!   - `NaN` / `±Infinity` are not valid JSON; export emits `null` for them
//!     (pandas behaviour; the loss is documented, never silent in output).
//!   - Booleans and mixed-type columns: the turbo reader materialises `t="b"`
//!     cells as 0/1 numbers and mixed columns as string arrays, so they surface
//!     as numbers/strings here. Import accepts JSON booleans as real booleans.
//!
//! * **Dates.** Date-styled numeric cells (style table `is_date`) export as
//!   formatted dates, never raw serials. Default format is ISO 8601:
//!   `YYYY-MM-DD` for whole days, `YYYY-MM-DDTHH:MM:SS` plus `.mmm` when there
//!   is a time. `date_format` overrides with strftime tokens (`%Y %y %m %d %H
//!   %M %S %f %%`). Date strings on import stay strings (lossless, no guessing).
//!
//! * **Formulas.** Export emits the **cached value** (the `<v>` the reader
//!   already materialises), never the formula text.
//!
//! * **Import column order.** A `Records`/`Ndjson` stream with heterogeneous
//!   keys produces the union of keys as columns, in **first-seen** order, with
//!   missing values as empty cells. `Columns` uses its own top-level key order.
//!
//! * **NDJSON** is one JSON object per line, no wrapping array, no trailing
//!   comma. It streams naturally both ways.
//!
//! # Memory (the point)
//!
//! * Export streams into `W` in 1 MiB chunks; the full JSON document is never
//!   assembled. Peak = reader state + one row (or one column).
//! * Import never builds a DOM. `Records`/`Columns` are stream-scanned twice
//!   (pass 1 discovers the key union — a header row must precede data in the
//!   write target — pass 2 writes rows); peak is the current record plus the
//!   sheet model being built. `Columns` import materialises each column array
//!   in turn (peak ≈ sheet model, inherent to the row-based write target).
//! * `NDJSON` import is line-streamed (O(one record)).
//!
//! `has_header` on import is accepted for API symmetry; the key union always
//! lands in row 1 because a grid has no "unlabelled" state.

use std::collections::HashSet;
use std::io::{self, BufRead, Read, Write};

use arrow_array::array::DictionaryArray;
use arrow_array::types::Int32Type;
use arrow_array::{
    Array, ArrayRef, BooleanArray, Date32Array, Float64Array, Int64Array, StringArray,
    TimestampMillisecondArray, UInt32Array,
};

use crate::turbo::error::{TurboError, TurboResult};
use crate::turbo::write::{Cell, CellValue, Row, Sheet, Workbook, save_workbook};
use crate::turbo::{Features, StyleTable, TurboSheet, list_sheet_names, read_workbook_turbo_sheet};

// ----------------------------------------------------------------------------
// Public surface
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonShape {
    /// Row-oriented: `[{"col":v,...},...]`. The default; what most consumers expect.
    Records,
    /// Column-oriented: `{"col":[v,...],...}`.
    Columns,
    /// One JSON object per line; what data pipelines actually use, and it streams.
    Ndjson,
}

#[derive(Debug, Clone)]
pub struct JsonOptions {
    pub shape: JsonShape,
    /// `true` (default): the sheet's row 1 supplies JSON keys on export, and
    /// the key union becomes row 1 on import. `false` on export: no header is
    /// assumed and keys are positional `"1".."N"` (the whole sheet is data).
    pub has_header: bool,
    /// strftime-style date format on export (`%Y-%m-%d` default). Empty = ISO 8601.
    pub date_format: String,
}

impl Default for JsonOptions {
    fn default() -> Self {
        Self {
            shape: JsonShape::Records,
            has_header: true,
            date_format: String::new(),
        }
    }
}

/// Stream `sheet` from `path` into `out` as JSON/NDJSON.
///
/// The document is written incrementally — peak memory is O(one row/column),
/// never O(file). Formulas export as their cached values. `sheet` is the
/// workbook sheet name.
pub fn sheet_to_json<W: Write>(
    path: &str,
    sheet: &str,
    out: W,
    opts: &JsonOptions,
) -> TurboResult<()> {
    // STYLES is required for date detection (style-table `is_date`); VALUES is
    // always on. FORMULAS is deliberately absent: the value columns already
    // hold each formula's cached `<v>` result.
    let features = Features::VALUES.union(Features::STYLES);
    let names = list_sheet_names(path)?;
    let idx = names
        .iter()
        .position(|n| n == sheet)
        .ok_or_else(|| TurboError::Format(format!("sheet '{sheet}' not found")))?;
    let wb = read_workbook_turbo_sheet(path, features, idx)?;
    let sh = &wb.sheets[0];
    let style_cols = sh.style_indices.as_ref();
    let st = wb.style_table.as_ref();
    let date1904 = wb.date1904;
    let date_fmt = opts.date_format.as_str();

    let keys: Vec<String> = if opts.has_header {
        (0..sh.ncols)
            .map(|c| sh.column_names.get(c).cloned().unwrap_or_default())
            .collect()
    } else {
        (1..=sh.ncols).map(|c| c.to_string()).collect()
    };

    let mut out = JsonOut::new(out);
    match opts.shape {
        JsonShape::Records => write_records(
            &mut out,
            sh,
            style_cols,
            st,
            date1904,
            date_fmt,
            &keys,
            opts.has_header,
        )?,
        JsonShape::Columns => write_columns(
            &mut out,
            sh,
            style_cols,
            st,
            date1904,
            date_fmt,
            &keys,
            opts.has_header,
        )?,
        JsonShape::Ndjson => write_ndjson(
            &mut out,
            sh,
            style_cols,
            st,
            date1904,
            date_fmt,
            &keys,
            opts.has_header,
        )?,
    }
    Ok(())
}

/// Build a single-sheet workbook from a JSON/NDJSON document and save it.
///
/// `Records`/`Ndjson` accept heterogeneous keys per record; the union of keys
/// (first-seen order) becomes the sheet columns, missing values become empty
/// cells. The parser streams and never builds a JSON DOM.
pub fn json_to_sheet(
    json_path: &str,
    xlsx_out: &str,
    sheet_name: &str,
    opts: &JsonOptions,
) -> TurboResult<()> {
    let mut sheet = Sheet::new(sheet_name);
    match opts.shape {
        JsonShape::Records => {
            let keys = discover_records_keys(json_path)?;
            read_records_into_sheet(&mut sheet, json_path, &keys)?;
        }
        JsonShape::Ndjson => {
            let keys = discover_ndjson_keys(json_path)?;
            read_ndjson_into_sheet(&mut sheet, json_path, &keys)?;
        }
        JsonShape::Columns => {
            read_columns_into_sheet(&mut sheet, json_path)?;
        }
    }
    let mut wb = Workbook::new();
    wb.sheets.clear();
    wb.sheets.push(sheet);
    save_workbook(&wb, xlsx_out)?;
    Ok(())
}

/// Like [`json_to_sheet`], but parses an in-memory document (any `BufRead` +
/// `Copy`, e.g. `&[u8]`) instead of a file. `Records`/`Ndjson` need two passes
/// over the document (key discovery then row fill), which is why `R` must be
/// `Copy`.
pub fn json_to_sheet_from<R: BufRead + Copy>(
    data: R,
    xlsx_out: &str,
    sheet_name: &str,
    opts: &JsonOptions,
) -> TurboResult<()> {
    let mut sheet = Sheet::new(sheet_name);
    match opts.shape {
        JsonShape::Records => {
            let keys = discover_records_keys_from(data)?;
            read_records_into_sheet_from(&mut sheet, data, &keys)?;
        }
        JsonShape::Ndjson => {
            let keys = discover_ndjson_keys_from(data)?;
            read_ndjson_into_sheet_from(&mut sheet, data, &keys)?;
        }
        JsonShape::Columns => {
            read_columns_into_sheet_from(&mut sheet, data)?;
        }
    }
    let mut wb = Workbook::new();
    wb.sheets.clear();
    wb.sheets.push(sheet);
    save_workbook(&wb, xlsx_out)?;
    Ok(())
}

// ----------------------------------------------------------------------------
// Export internals
// ----------------------------------------------------------------------------

const OUT_CHUNK: usize = 1 << 20;
const TWO_POW_53: f64 = 9_007_199_254_740_992.0;

enum CellVal {
    Null,
    Num(f64),
    Int(i64),
    Bool(bool),
    Date32(i32),
    TimestampMs(i64),
    Str(String),
}

struct JsonOut<W: Write> {
    w: W,
    buf: Vec<u8>,
}

impl<W: Write> JsonOut<W> {
    fn new(w: W) -> Self {
        Self {
            w,
            buf: Vec::with_capacity(OUT_CHUNK),
        }
    }
    fn push(&mut self, b: &[u8]) -> io::Result<()> {
        self.buf.extend_from_slice(b);
        if self.buf.len() >= OUT_CHUNK {
            self.w.write_all(&self.buf)?;
            self.buf.clear();
        }
        Ok(())
    }
    fn flush(&mut self) -> io::Result<()> {
        if !self.buf.is_empty() {
            self.w.write_all(&self.buf)?;
            self.buf.clear();
        }
        Ok(())
    }
    /// Emit a JSON string literal with full escaping.
    fn write_str(&mut self, s: &str) -> io::Result<()> {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        self.buf.push(b'"');
        let bytes = s.as_bytes();
        let mut start = 0usize;
        for (i, &b) in bytes.iter().enumerate() {
            match b {
                b'"' | b'\\' | 0x00..=0x1F => {
                    self.buf.extend_from_slice(&bytes[start..i]);
                    match b {
                        b'"' => self.buf.extend_from_slice(b"\\\""),
                        b'\\' => self.buf.extend_from_slice(b"\\\\"),
                        _ => {
                            // Control characters as \u00XX (lowercase hex).
                            self.buf.extend_from_slice(b"\\u00");
                            self.buf.push(HEX[(b >> 4) as usize]);
                            self.buf.push(HEX[(b & 0x0F) as usize]);
                        }
                    }
                    start = i + 1;
                    if self.buf.len() >= OUT_CHUNK {
                        self.w.write_all(&self.buf)?;
                        self.buf.clear();
                    }
                }
                _ => {}
            }
        }
        self.buf.extend_from_slice(&bytes[start..]);
        self.buf.push(b'"');
        if self.buf.len() >= OUT_CHUNK {
            self.w.write_all(&self.buf)?;
            self.buf.clear();
        }
        Ok(())
    }
    /// Emit a JSON number. NaN/±Inf → `null`; integers ≤ 2^53 → integer
    /// literal; integers > 2^53 → string (see module docs); else shortest
    /// round-trip f64 (ryu, valid JSON).
    fn push_number(&mut self, v: f64) -> io::Result<()> {
        if v.is_nan() || v.is_infinite() {
            self.push(b"null")
        } else if v == v.trunc() && v.abs() <= TWO_POW_53 {
            let mut ib = itoa::Buffer::new();
            self.push(ib.format(v as i64).as_bytes())
        } else if v == v.trunc() {
            // Integral but beyond 2^53: a JSON number would silently lose
            // digits, so emit the exact double as a string instead (with the
            // trailing ".0" ryu appends stripped).
            self.push(b"\"")?;
            let mut rb = ryu::Buffer::new();
            let s = rb.format(v);
            let s = s.strip_suffix(".0").unwrap_or(s);
            self.push(s.as_bytes())?;
            self.push(b"\"")
        } else {
            let mut rb = ryu::Buffer::new();
            self.push(rb.format(v).as_bytes())
        }
    }
}

/// Read one Arrow value cell from a value column.
fn cell_at(col: &ArrayRef, i: usize) -> CellVal {
    if let Some(f) = col.as_any().downcast_ref::<Float64Array>() {
        if f.is_null(i) {
            CellVal::Null
        } else {
            CellVal::Num(f.value(i))
        }
    } else if let Some(x) = col.as_any().downcast_ref::<Int64Array>() {
        if x.is_null(i) {
            CellVal::Null
        } else {
            CellVal::Int(x.value(i))
        }
    } else if let Some(x) = col.as_any().downcast_ref::<BooleanArray>() {
        if x.is_null(i) {
            CellVal::Null
        } else {
            CellVal::Bool(x.value(i))
        }
    } else if let Some(x) = col.as_any().downcast_ref::<Date32Array>() {
        if x.is_null(i) {
            CellVal::Null
        } else {
            CellVal::Date32(x.value(i))
        }
    } else if let Some(x) = col.as_any().downcast_ref::<TimestampMillisecondArray>() {
        if x.is_null(i) {
            CellVal::Null
        } else {
            CellVal::TimestampMs(x.value(i))
        }
    } else if let Some(s) = col.as_any().downcast_ref::<StringArray>() {
        if s.is_null(i) {
            CellVal::Null
        } else {
            CellVal::Str(s.value(i).to_string())
        }
    } else if let Some(d) = col.as_any().downcast_ref::<DictionaryArray<Int32Type>>() {
        let keys = d.keys();
        if keys.is_null(i) {
            return CellVal::Null;
        }
        let idx = keys.value(i);
        match d.values().as_any().downcast_ref::<StringArray>() {
            Some(vals) if idx >= 0 && (idx as usize) < vals.len() => {
                CellVal::Str(vals.value(idx as usize).to_string())
            }
            _ => CellVal::Null,
        }
    } else {
        CellVal::Null
    }
}

fn cell_style(style_cols: Option<&Vec<UInt32Array>>, c: usize, r: usize) -> Option<u32> {
    let cols = style_cols?;
    let arr = cols.get(c)?;
    if r < arr.len() && !arr.is_null(r) {
        Some(arr.value(r))
    } else {
        None
    }
}

struct RowView {
    vals: Vec<CellVal>,
    styles: Vec<Option<u32>>,
}

fn data_row_view(sh: &TurboSheet, r: usize, style_cols: Option<&Vec<UInt32Array>>) -> RowView {
    let mut vals = Vec::with_capacity(sh.ncols);
    let mut styles = Vec::with_capacity(sh.ncols);
    for c in 0..sh.ncols {
        vals.push(cell_at(&sh.columns[c], r));
        styles.push(cell_style(style_cols, c, r));
    }
    RowView { vals, styles }
}

/// Row-as-data for `has_header=false` export: the header row becomes the first
/// data record (its values arrive as strings from the reader).
fn header_row_view(sh: &TurboSheet) -> RowView {
    RowView {
        vals: sh
            .column_names
            .iter()
            .map(|s| CellVal::Str(s.clone()))
            .collect(),
        styles: vec![None; sh.ncols],
    }
}

fn for_each_export_row<W: Write, F: FnMut(&RowView, &mut JsonOut<W>) -> io::Result<()>>(
    sh: &TurboSheet,
    style_cols: Option<&Vec<UInt32Array>>,
    has_header: bool,
    out: &mut JsonOut<W>,
    mut f: F,
) -> io::Result<()> {
    if !has_header {
        let rv = header_row_view(sh);
        f(&rv, out)?;
    }
    for r in 0..sh.nrows {
        let rv = data_row_view(sh, r, style_cols);
        f(&rv, out)?;
    }
    Ok(())
}

fn emit_value<W: Write>(
    out: &mut JsonOut<W>,
    val: &CellVal,
    style: Option<u32>,
    st: Option<&StyleTable>,
    date1904: bool,
    date_fmt: &str,
) -> io::Result<()> {
    match val {
        CellVal::Null => out.push(b"null"),
        CellVal::Str(s) => out.write_str(s),
        CellVal::Bool(b) => out.push(if *b { b"true" } else { b"false" }),
        CellVal::Int(i) => {
            let mut ib = itoa::Buffer::new();
            out.push(ib.format(*i).as_bytes())
        }
        CellVal::Date32(days) => {
            let unix_epoch_excel_serial = 25569.0;
            let serial = unix_epoch_excel_serial + (*days as f64);
            out.write_str(&format_date_serial(serial, false, date_fmt))
        }
        CellVal::TimestampMs(ms) => {
            let unix_epoch_excel_serial = 25569.0;
            let serial = unix_epoch_excel_serial + (*ms as f64 / 86400000.0);
            out.write_str(&format_date_serial(serial, false, date_fmt))
        }
        CellVal::Num(v) => {
            // Formulas: the value column already holds the cached `<v>` result,
            // so this is exporting cached values — never formula text.
            if let Some(st) = st {
                if let Some(s) = style {
                    if st.is_date(s) {
                        return out.write_str(&format_date_serial(*v, date1904, date_fmt));
                    }
                }
            }
            out.push_number(*v)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn write_records<W: Write>(
    out: &mut JsonOut<W>,
    sh: &TurboSheet,
    style_cols: Option<&Vec<UInt32Array>>,
    st: Option<&StyleTable>,
    date1904: bool,
    date_fmt: &str,
    keys: &[String],
    has_header: bool,
) -> io::Result<()> {
    out.push(b"[")?;
    let mut first = true;
    for_each_export_row(sh, style_cols, has_header, out, |rv, out| {
        if !first {
            out.push(b",")?;
        }
        out.push(b"{")?;
        for (c, key) in keys.iter().enumerate() {
            if c > 0 {
                out.push(b",")?;
            }
            out.write_str(key)?;
            out.push(b":")?;
            let val = rv.vals.get(c).unwrap_or(&CellVal::Null);
            let style = rv.styles.get(c).copied().flatten();
            emit_value(out, val, style, st, date1904, date_fmt)?;
        }
        out.push(b"}")?;
        first = false;
        Ok(())
    })?;
    out.push(b"]")?;
    out.flush()
}

#[allow(clippy::too_many_arguments)]
fn write_columns<W: Write>(
    out: &mut JsonOut<W>,
    sh: &TurboSheet,
    style_cols: Option<&Vec<UInt32Array>>,
    st: Option<&StyleTable>,
    date1904: bool,
    date_fmt: &str,
    keys: &[String],
    has_header: bool,
) -> io::Result<()> {
    out.push(b"{")?;
    for (c, key) in keys.iter().enumerate() {
        if c > 0 {
            out.push(b",")?;
        }
        out.write_str(key)?;
        out.push(b":[")?;
        let mut first = true;
        if !has_header {
            match sh.column_names.get(c) {
                Some(h) => out.write_str(h)?,
                None => out.push(b"null")?,
            }
            first = false;
        }
        for r in 0..sh.nrows {
            if !first {
                out.push(b",")?;
            }
            let val = cell_at(&sh.columns[c], r);
            let style = cell_style(style_cols, c, r);
            emit_value(out, &val, style, st, date1904, date_fmt)?;
            first = false;
        }
        out.push(b"]")?;
    }
    out.push(b"}")?;
    out.flush()
}

#[allow(clippy::too_many_arguments)]
fn write_ndjson<W: Write>(
    out: &mut JsonOut<W>,
    sh: &TurboSheet,
    style_cols: Option<&Vec<UInt32Array>>,
    st: Option<&StyleTable>,
    date1904: bool,
    date_fmt: &str,
    keys: &[String],
    has_header: bool,
) -> io::Result<()> {
    for_each_export_row(sh, style_cols, has_header, out, |rv, out| {
        out.push(b"{")?;
        for (c, key) in keys.iter().enumerate() {
            if c > 0 {
                out.push(b",")?;
            }
            out.write_str(key)?;
            out.push(b":")?;
            let val = rv.vals.get(c).unwrap_or(&CellVal::Null);
            let style = rv.styles.get(c).copied().flatten();
            emit_value(out, val, style, st, date1904, date_fmt)?;
        }
        out.push(b"}\n")?;
        Ok(())
    })?;
    out.flush()
}

// ----------------------------------------------------------------------------
// Date serials → ISO 8601 (openpyxl `from_excel` semantics)
// ----------------------------------------------------------------------------

/// Excel serial → civil datetime. Follows openpyxl `utils/datetime.from_excel`:
/// day = floor(serial), fraction rounded to milliseconds, and the Windows 1900
/// leap bug means serials in (0, 60) shift +1 day.
fn serial_to_civil(serial: f64, date1904: bool) -> (i64, i64, i64, i64, i64, i64, i64) {
    let day = serial.floor();
    let ms_round = ((serial - day) * 86_400_000.0).round() as i64;
    let mut day_adj = day as i64;
    if !date1904 && serial > 0.0 && serial < 60.0 {
        day_adj += 1;
    }
    let base = if date1904 {
        days_from_civil(1904, 1, 1)
    } else {
        days_from_civil(1899, 12, 30)
    };
    let total = base + day_adj + ms_round / 86_400_000;
    let rem = ms_round % 86_400_000;
    let (y, mo, d) = civil_from_days(total);
    (
        y,
        mo,
        d,
        rem / 3_600_000,
        (rem / 60_000) % 60,
        (rem / 1_000) % 60,
        rem % 1_000,
    )
}

/// Howard Hinnant's days-from-civil (proleptic Gregorian, no year 0).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Minimal strftime: `%Y %y %m %d %H %M %S %f %%`. Anything else is literal.
#[allow(clippy::too_many_arguments)]
fn strftime(fmt: &str, y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64, ms: i64) -> String {
    let chars: Vec<char> = fmt.chars().collect();
    let mut out = String::with_capacity(fmt.len() + 8);
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '%' && i + 1 < chars.len() {
            match chars[i + 1] {
                'Y' => out.push_str(&format!("{y:04}")),
                'y' => out.push_str(&format!("{:02}", y % 100)),
                'm' => out.push_str(&format!("{mo:02}")),
                'd' => out.push_str(&format!("{d:02}")),
                'H' => out.push_str(&format!("{h:02}")),
                'M' => out.push_str(&format!("{mi:02}")),
                'S' => out.push_str(&format!("{s:02}")),
                'f' => out.push_str(&format!("{ms:03}")),
                '%' => out.push('%'),
                other => {
                    out.push('%');
                    out.push(other);
                }
            }
            i += 2;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn format_date_serial(serial: f64, date1904: bool, fmt: &str) -> String {
    let (y, mo, d, h, mi, s, ms) = serial_to_civil(serial, date1904);
    if fmt.is_empty() {
        // ISO 8601 default: date-only for whole days, else date + time (+ .mmm).
        if h == 0 && mi == 0 && s == 0 && ms == 0 {
            strftime("%Y-%m-%d", y, mo, d, h, mi, s, ms)
        } else {
            let base = strftime("%Y-%m-%dT%H:%M:%S", y, mo, d, h, mi, s, ms);
            if ms != 0 {
                format!("{base}.{ms:03}")
            } else {
                base
            }
        }
    } else {
        strftime(fmt, y, mo, d, h, mi, s, ms)
    }
}

// ----------------------------------------------------------------------------
// Import: hand-rolled streaming JSON parser
// ----------------------------------------------------------------------------

#[derive(Debug)]
enum JsonErr {
    /// The value would extend past the current byte slice; caller must fill
    /// the reader and retry from the same start offset.
    NeedMore,
    Format(String),
}

#[derive(Debug, Clone)]
enum JVal {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    /// Nested object/array, kept as its raw JSON text (a grid cell is scalar).
    Other(String),
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | b'\r') {
        i += 1;
    }
    i
}

fn trim_ws(b: &[u8]) -> &[u8] {
    let mut s = 0;
    let mut e = b.len();
    while s < e && matches!(b[s], b' ' | b'\t' | b'\n' | b'\r') {
        s += 1;
    }
    while e > s && matches!(b[e - 1], b' ' | b'\t' | b'\n' | b'\r') {
        e -= 1;
    }
    &b[s..e]
}

fn parse_string(b: &[u8], i: usize) -> Result<(usize, String), JsonErr> {
    if b.get(i) != Some(&b'"') {
        return Err(JsonErr::Format("expected string".into()));
    }
    let mut j = i + 1;
    let mut out = String::new();
    let mut start = j;
    loop {
        if j >= b.len() {
            return Err(JsonErr::NeedMore);
        }
        match b[j] {
            b'"' => {
                out.push_str(
                    std::str::from_utf8(&b[start..j])
                        .map_err(|_| JsonErr::Format("invalid UTF-8 in string".into()))?,
                );
                return Ok((j + 1 - i, out));
            }
            b'\\' => {
                out.push_str(
                    std::str::from_utf8(&b[start..j])
                        .map_err(|_| JsonErr::Format("invalid UTF-8 in string".into()))?,
                );
                j += 1;
                if j >= b.len() {
                    return Err(JsonErr::NeedMore);
                }
                match b[j] {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'b' => out.push('\u{0008}'),
                    b'f' => out.push('\u{000C}'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => {
                        if j + 5 > b.len() {
                            return Err(JsonErr::NeedMore);
                        }
                        let hex = std::str::from_utf8(&b[j + 1..j + 5])
                            .map_err(|_| JsonErr::Format("bad \\u escape".into()))?;
                        let cp = u32::from_str_radix(hex, 16)
                            .map_err(|_| JsonErr::Format("bad \\u escape".into()))?;
                        if (0xD800..=0xDBFF).contains(&cp) {
                            // High surrogate: expect a following \uDC00..\uDFFF.
                            if j + 11 > b.len() {
                                return Err(JsonErr::NeedMore); // pair may split the buffer
                            }
                            let mut combined = false;
                            if b[j + 5] == b'\\' && b[j + 6] == b'u' {
                                if let Ok(hex2) = std::str::from_utf8(&b[j + 7..j + 11]) {
                                    if let Ok(lo) = u32::from_str_radix(hex2, 16) {
                                        if (0xDC00..=0xDFFF).contains(&lo) {
                                            let c = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                            out.push(char::from_u32(c).unwrap_or('\u{FFFD}'));
                                            j += 10; // consume \uXXXX\uXXXX
                                            combined = true;
                                        }
                                    }
                                }
                            }
                            if !combined {
                                out.push('\u{FFFD}');
                                j += 4;
                            }
                        } else if (0xDC00..=0xDFFF).contains(&cp) {
                            // Lone low surrogate → U+FFFD (never invalid UTF-8).
                            out.push('\u{FFFD}');
                            j += 4;
                        } else {
                            out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                            j += 4;
                        }
                    }
                    _ => return Err(JsonErr::Format("bad string escape".into())),
                }
                j += 1;
                start = j;
            }
            _ => j += 1,
        }
    }
}

/// JSON number. `capture` controls whether the value is built (skip mode still
/// advances over the token). Integers beyond 2^53 become their raw digit string.
fn parse_number(b: &[u8], i: usize, capture: bool) -> Result<(usize, Option<JVal>), JsonErr> {
    let mut j = i;
    if b[j] == b'-' {
        j += 1;
        if j >= b.len() {
            return Err(JsonErr::NeedMore);
        }
    }
    if b[j] == b'0' {
        j += 1;
    } else if b[j].is_ascii_digit() {
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
    } else {
        return Err(JsonErr::Format("invalid number".into()));
    }
    let mut is_int = true;
    if j < b.len() && b[j] == b'.' {
        is_int = false;
        j += 1;
        if j >= b.len() {
            return Err(JsonErr::NeedMore);
        }
        if !b[j].is_ascii_digit() {
            return Err(JsonErr::Format("invalid number".into()));
        }
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
    }
    if j < b.len() && (b[j] == b'e' || b[j] == b'E') {
        is_int = false;
        j += 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        if j >= b.len() {
            return Err(JsonErr::NeedMore);
        }
        if !b[j].is_ascii_digit() {
            return Err(JsonErr::Format("invalid number".into()));
        }
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
    }
    if !capture {
        return Ok((j - i, None));
    }
    let raw = &b[i..j];
    let val = if is_int {
        let digits = raw.strip_prefix(b"-").unwrap_or(raw);
        let mut mag: u128 = 0;
        for &d in digits {
            mag = mag.wrapping_mul(10).wrapping_add((d - b'0') as u128);
        }
        if mag > 9_007_199_254_740_992u128 {
            // A JSON integer beyond 2^53 cannot survive as an f64: keep the
            // raw digits as a string (never a lossy double).
            JVal::Str(String::from_utf8_lossy(raw).into_owned())
        } else {
            JVal::Num(
                fast_float2::parse::<f64, _>(raw)
                    .map_err(|_| JsonErr::Format("invalid number".into()))?,
            )
        }
    } else {
        JVal::Num(
            fast_float2::parse::<f64, _>(raw)
                .map_err(|_| JsonErr::Format("invalid number".into()))?,
        )
    };
    Ok((j - i, Some(val)))
}

fn parse_lit(b: &[u8], i: usize, lit: &[u8]) -> Result<usize, JsonErr> {
    let n = lit.len();
    if b.len() - i < n {
        if b[i..].iter().zip(lit).all(|(x, y)| x == y) {
            return Err(JsonErr::NeedMore);
        }
        return Err(JsonErr::Format("invalid literal".into()));
    }
    if &b[i..i + n] == lit {
        Ok(n)
    } else {
        Err(JsonErr::Format("invalid literal".into()))
    }
}

fn parse_value_skip_len(b: &[u8], i: usize) -> Result<usize, JsonErr> {
    let c = *b.get(i).ok_or(JsonErr::NeedMore)?;
    match c {
        b'"' => Ok(parse_string(b, i)?.0),
        b'{' => parse_object_skip(b, i),
        b'[' => parse_array_skip(b, i),
        b't' => parse_lit(b, i, b"true"),
        b'f' => parse_lit(b, i, b"false"),
        b'n' => parse_lit(b, i, b"null"),
        b'-' | b'0'..=b'9' => parse_number(b, i, false).map(|(n, _)| n),
        _ => Err(JsonErr::Format("unexpected value".into())),
    }
}

fn parse_object_skip(b: &[u8], i: usize) -> Result<usize, JsonErr> {
    let mut j = i + 1;
    j = skip_ws(b, j);
    if j >= b.len() {
        return Err(JsonErr::NeedMore);
    }
    if b[j] == b'}' {
        return Ok(j + 1 - i);
    }
    loop {
        if j >= b.len() || b[j] != b'"' {
            return Err(if j >= b.len() {
                JsonErr::NeedMore
            } else {
                JsonErr::Format("expected object key".into())
            });
        }
        let (n, _) = parse_string(b, j)?;
        j += n;
        j = skip_ws(b, j);
        if j >= b.len() {
            return Err(JsonErr::NeedMore);
        }
        if b[j] != b':' {
            return Err(JsonErr::Format("expected ':'".into()));
        }
        j += 1;
        j = skip_ws(b, j);
        if j >= b.len() {
            return Err(JsonErr::NeedMore);
        }
        let n = parse_value_skip_len(b, j)?;
        j += n;
        j = skip_ws(b, j);
        if j >= b.len() {
            return Err(JsonErr::NeedMore);
        }
        match b[j] {
            b',' => {
                j += 1;
                j = skip_ws(b, j);
                if j >= b.len() {
                    return Err(JsonErr::NeedMore);
                }
            }
            b'}' => return Ok(j + 1 - i),
            _ => return Err(JsonErr::Format("expected ',' or '}'".into())),
        }
    }
}

fn parse_array_skip(b: &[u8], i: usize) -> Result<usize, JsonErr> {
    let mut j = i + 1;
    j = skip_ws(b, j);
    if j >= b.len() {
        return Err(JsonErr::NeedMore);
    }
    if b[j] == b']' {
        return Ok(j + 1 - i);
    }
    loop {
        let n = parse_value_skip_len(b, j)?;
        j += n;
        j = skip_ws(b, j);
        if j >= b.len() {
            return Err(JsonErr::NeedMore);
        }
        match b[j] {
            b',' => {
                j += 1;
                j = skip_ws(b, j);
                if j >= b.len() {
                    return Err(JsonErr::NeedMore);
                }
            }
            b']' => return Ok(j + 1 - i),
            _ => return Err(JsonErr::Format("expected ',' or ']'".into())),
        }
    }
}

fn parse_value(b: &[u8], i: usize) -> Result<(usize, JVal), JsonErr> {
    let c = *b.get(i).ok_or(JsonErr::NeedMore)?;
    match c {
        b'"' => {
            let (n, s) = parse_string(b, i)?;
            Ok((n, JVal::Str(s)))
        }
        b'{' => {
            let n = parse_object_skip(b, i)?;
            Ok((
                n,
                JVal::Other(std::str::from_utf8(&b[i..i + n]).unwrap_or("").to_string()),
            ))
        }
        b'[' => {
            let n = parse_array_skip(b, i)?;
            Ok((
                n,
                JVal::Other(std::str::from_utf8(&b[i..i + n]).unwrap_or("").to_string()),
            ))
        }
        b't' => Ok((parse_lit(b, i, b"true")?, JVal::Bool(true))),
        b'f' => Ok((parse_lit(b, i, b"false")?, JVal::Bool(false))),
        b'n' => Ok((parse_lit(b, i, b"null")?, JVal::Null)),
        b'-' | b'0'..=b'9' => parse_number(b, i, true).map(|(n, v)| (n, v.unwrap_or(JVal::Null))),
        _ => Err(JsonErr::Format("unexpected value".into())),
    }
}

fn parse_object_keys(b: &[u8], i: usize) -> Result<(usize, Vec<String>), JsonErr> {
    if b.get(i) != Some(&b'{') {
        return Err(JsonErr::Format("expected object".into()));
    }
    let mut j = i + 1;
    let mut keys = Vec::new();
    j = skip_ws(b, j);
    if j >= b.len() {
        return Err(JsonErr::NeedMore);
    }
    if b[j] == b'}' {
        return Ok((j + 1 - i, keys));
    }
    loop {
        if j >= b.len() || b[j] != b'"' {
            return Err(if j >= b.len() {
                JsonErr::NeedMore
            } else {
                JsonErr::Format("expected object key".into())
            });
        }
        let (n, key) = parse_string(b, j)?;
        keys.push(key);
        j += n;
        j = skip_ws(b, j);
        if j >= b.len() {
            return Err(JsonErr::NeedMore);
        }
        if b[j] != b':' {
            return Err(JsonErr::Format("expected ':'".into()));
        }
        j += 1;
        j = skip_ws(b, j);
        if j >= b.len() {
            return Err(JsonErr::NeedMore);
        }
        let n = parse_value_skip_len(b, j)?;
        j += n;
        j = skip_ws(b, j);
        if j >= b.len() {
            return Err(JsonErr::NeedMore);
        }
        match b[j] {
            b',' => {
                j += 1;
                j = skip_ws(b, j);
                if j >= b.len() {
                    return Err(JsonErr::NeedMore);
                }
            }
            b'}' => return Ok((j + 1 - i, keys)),
            _ => return Err(JsonErr::Format("expected ',' or '}'".into())),
        }
    }
}

fn parse_object_full(b: &[u8], i: usize) -> Result<(usize, Vec<(String, JVal)>), JsonErr> {
    if b.get(i) != Some(&b'{') {
        return Err(JsonErr::Format("expected object".into()));
    }
    let mut j = i + 1;
    let mut out = Vec::new();
    j = skip_ws(b, j);
    if j >= b.len() {
        return Err(JsonErr::NeedMore);
    }
    if b[j] == b'}' {
        return Ok((j + 1 - i, out));
    }
    loop {
        if j >= b.len() || b[j] != b'"' {
            return Err(if j >= b.len() {
                JsonErr::NeedMore
            } else {
                JsonErr::Format("expected object key".into())
            });
        }
        let (n, key) = parse_string(b, j)?;
        j += n;
        j = skip_ws(b, j);
        if j >= b.len() {
            return Err(JsonErr::NeedMore);
        }
        if b[j] != b':' {
            return Err(JsonErr::Format("expected ':'".into()));
        }
        j += 1;
        j = skip_ws(b, j);
        if j >= b.len() {
            return Err(JsonErr::NeedMore);
        }
        let (n, val) = parse_value(b, j)?;
        out.push((key, val));
        j += n;
        j = skip_ws(b, j);
        if j >= b.len() {
            return Err(JsonErr::NeedMore);
        }
        match b[j] {
            b',' => {
                j += 1;
                j = skip_ws(b, j);
                if j >= b.len() {
                    return Err(JsonErr::NeedMore);
                }
            }
            b'}' => return Ok((j + 1 - i, out)),
            _ => return Err(JsonErr::Format("expected ',' or '}'".into())),
        }
    }
}

// ----------------------------------------------------------------------------
// Import: streaming drivers
// ----------------------------------------------------------------------------

const READ_CHUNK: usize = 1 << 20;

/// Bounded streaming reader: holds only the current chunk plus whatever is
/// needed to complete the record currently being parsed. Compacted on fill so
/// memory stays O(record), not O(file).
struct ChunkReader<R: Read> {
    inner: R,
    buf: Vec<u8>,
    start: usize,
    eof: bool,
}

impl<R: Read> ChunkReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            buf: Vec::with_capacity(READ_CHUNK),
            start: 0,
            eof: false,
        }
    }
    fn slice(&self) -> &[u8] {
        &self.buf[self.start..]
    }
    fn advance(&mut self, n: usize) {
        debug_assert!(self.start + n <= self.buf.len());
        self.start += n;
        if self.start >= READ_CHUNK {
            let rest = self.buf.len() - self.start;
            self.buf.copy_within(self.start.., 0);
            self.buf.truncate(rest);
            self.start = 0;
        }
    }
    /// Read more data. Returns false at EOF.
    fn fill(&mut self) -> io::Result<bool> {
        if self.eof {
            return Ok(false);
        }
        if self.start > 0 {
            let rest = self.buf.len() - self.start;
            self.buf.copy_within(self.start.., 0);
            self.buf.truncate(rest);
            self.start = 0;
        }
        let prev = self.buf.len();
        self.buf.resize(prev + READ_CHUNK, 0);
        let n = self.inner.read(&mut self.buf[prev..])?;
        if n > 0 {
            self.buf.truncate(prev + n);
            Ok(true)
        } else {
            self.buf.truncate(prev);
            self.eof = true;
            Ok(false)
        }
    }
}

fn truncated() -> TurboError {
    TurboError::Format("truncated JSON document".into())
}

/// Parse a complete value at offset 0 of the current slice, refilling the
/// reader and retrying until it completes. `T` is owned (never borrows the
/// slice), so memory is bounded by one value.
fn retry_parse<R: Read, T>(
    r: &mut ChunkReader<R>,
    mut f: impl FnMut(&[u8]) -> Result<(usize, T), JsonErr>,
) -> TurboResult<(usize, T)> {
    loop {
        let res = {
            let s = r.slice();
            f(s)
        };
        match res {
            Ok(v) => return Ok(v),
            Err(JsonErr::NeedMore) => {
                if !r.fill()? {
                    return Err(truncated());
                }
            }
            Err(JsonErr::Format(e)) => return Err(TurboError::Format(e)),
        }
    }
}

fn skip_bom<R: Read>(r: &mut ChunkReader<R>) -> TurboResult<()> {
    loop {
        let has_bom;
        {
            let s = r.slice();
            has_bom = s.len() >= 3 && &s[..3] == b"\xEF\xBB\xBF";
        }
        if has_bom {
            r.advance(3);
            return Ok(());
        }
        let empty = {
            let s = r.slice();
            s.is_empty()
        };
        if empty {
            if !r.fill()? {
                return Ok(());
            }
        } else {
            return Ok(());
        }
    }
}

fn skip_ws_in_reader<R: Read>(r: &mut ChunkReader<R>) -> TurboResult<()> {
    loop {
        let n = {
            let s = r.slice();
            let i = skip_ws(s, 0);
            if i < s.len() { Some(i) } else { None }
        };
        match n {
            Some(i) => {
                r.advance(i);
                return Ok(());
            }
            None => {
                if !r.fill()? {
                    return Err(truncated());
                }
            }
        }
    }
}

fn expect_char<R: Read>(r: &mut ChunkReader<R>, want: u8) -> TurboResult<()> {
    loop {
        let next;
        let found;
        {
            let s = r.slice();
            let i = skip_ws(s, 0);
            if i < s.len() {
                next = s[i];
                found = true;
            } else {
                next = 0;
                found = false;
            }
        }
        if found {
            if next == want {
                let i = {
                    let s = r.slice();
                    skip_ws(s, 0)
                };
                r.advance(i + 1);
                return Ok(());
            }
            return Err(TurboError::Format(format!(
                "expected '{}' in JSON document",
                want as char
            )));
        }
        if !r.fill()? {
            return Err(truncated());
        }
    }
}

/// Stream the elements of a top-level JSON array. Tolerates trailing commas.
fn for_each_records_element<R: Read, T, F: FnMut(T) -> TurboResult<()>>(
    r: &mut ChunkReader<R>,
    parse: impl Fn(&[u8]) -> Result<(usize, T), JsonErr>,
    mut f: F,
) -> TurboResult<()> {
    expect_char(r, b'[')?;
    loop {
        skip_ws_in_reader(r)?;
        let is_end = {
            let s = r.slice();
            if s.is_empty() {
                return Err(truncated());
            }
            s[0] == b']'
        };
        if is_end {
            r.advance(1);
            return Ok(());
        }
        let (n, val) = retry_parse(r, &parse)?;
        f(val)?;
        r.advance(n);
        skip_ws_in_reader(r)?;
        let is_end = {
            let s = r.slice();
            if s.is_empty() {
                return Err(truncated());
            }
            match s[0] {
                b',' => false,
                b']' => true,
                _ => {
                    return Err(TurboError::Format(
                        "expected ',' or ']' in JSON array".into(),
                    ));
                }
            }
        };
        r.advance(1);
        if is_end {
            return Ok(());
        }
    }
}

/// Stream the key union of a `Records` document from any `BufRead` (a file, or
/// an in-memory buffer for the Python bytes entry point).
fn discover_records_keys_from<R: BufRead>(r: R) -> TurboResult<Vec<String>> {
    let mut r = ChunkReader::new(r);
    skip_bom(&mut r)?;
    let mut keys: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for_each_records_element(
        &mut r,
        |b| parse_object_keys(b, 0),
        |ks| {
            for k in ks {
                if seen.insert(k.clone()) {
                    keys.push(k);
                }
            }
            Ok(())
        },
    )?;
    Ok(keys)
}

fn discover_records_keys(path: &str) -> TurboResult<Vec<String>> {
    let f = std::fs::File::open(path)?;
    discover_records_keys_from(std::io::BufReader::new(f))
}

fn header_row(keys: &[String]) -> Row {
    let mut row = Row::new(1);
    for (ci, k) in keys.iter().enumerate() {
        row.cells
            .push(Cell::new(ci as u32 + 1, CellValue::Str(k.clone())));
    }
    row
}

fn record_to_row(obj: &[(String, JVal)], keys: &[String], row_no: u32) -> Row {
    let mut row = Row::new(row_no);
    if obj.is_empty() {
        return row;
    }
    let mut map = ahash::AHashMap::with_capacity(obj.len());
    for (k, v) in obj {
        map.insert(k.as_str(), v);
    }
    row.cells.reserve(keys.len());
    for (ci, key) in keys.iter().enumerate() {
        match map.get(key.as_str()) {
            None | Some(JVal::Null) => {} // missing key / null → empty cell
            Some(JVal::Num(x)) => row
                .cells
                .push(Cell::new(ci as u32 + 1, CellValue::Number(*x))),
            Some(JVal::Bool(b)) => row
                .cells
                .push(Cell::new(ci as u32 + 1, CellValue::Bool(*b))),
            Some(JVal::Str(s)) => row
                .cells
                .push(Cell::new(ci as u32 + 1, CellValue::Str(s.clone()))),
            Some(JVal::Other(t)) => row
                .cells
                .push(Cell::new(ci as u32 + 1, CellValue::Str(t.clone()))),
        }
    }
    row
}

/// Stream `Records` rows into a sheet from any `BufRead`.
fn read_records_into_sheet_from<R: BufRead>(
    sheet: &mut Sheet,
    r: R,
    keys: &[String],
) -> TurboResult<()> {
    sheet.rows.push(header_row(keys));
    let mut row_no = 2u32;
    let mut r = ChunkReader::new(r);
    skip_bom(&mut r)?;
    for_each_records_element(
        &mut r,
        |b| parse_object_full(b, 0),
        |obj| {
            sheet.rows.push(record_to_row(&obj, keys, row_no));
            row_no += 1;
            Ok(())
        },
    )?;
    Ok(())
}

fn read_records_into_sheet(sheet: &mut Sheet, path: &str, keys: &[String]) -> TurboResult<()> {
    let f = std::fs::File::open(path)?;
    read_records_into_sheet_from(sheet, std::io::BufReader::new(f), keys)
}

/// NDJSON: stream one object per line (no wrapping array). Objects may be
/// pretty-printed across several physical lines; they are accumulated until
/// one complete object parses. Peak memory is one record.
fn ndjson_object_stream<R: BufRead>(
    mut r: R,
    mut f: impl FnMut(&[u8]) -> TurboResult<()>,
) -> TurboResult<()> {
    let mut pending: Vec<u8> = Vec::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = r.read_until(b'\n', &mut line)?;
        if n == 0 {
            let t = trim_ws(&pending);
            if !t.is_empty() {
                f(t)?;
            }
            break;
        }
        let t0 = trim_ws(&line);
        if t0.is_empty() {
            continue;
        }
        let t = if pending.is_empty() && t0.starts_with(b"\xEF\xBB\xBF") {
            &t0[3..]
        } else {
            t0
        };
        if t.is_empty() {
            continue;
        }
        pending.extend_from_slice(t);
        match parse_object_full(&pending, 0) {
            Ok((n, _)) => {
                if trim_ws(&pending[n..]).is_empty() {
                    let obj = std::mem::take(&mut pending);
                    f(&obj)?;
                } else {
                    return Err(TurboError::Format(
                        "more than one JSON value on one NDJSON record".into(),
                    ));
                }
            }
            Err(JsonErr::NeedMore) => continue,
            Err(JsonErr::Format(e)) => return Err(TurboError::Format(e)),
        }
    }
    Ok(())
}

/// Discover the NDJSON key union from any `BufRead`.
fn discover_ndjson_keys_from<R: BufRead>(r: R) -> TurboResult<Vec<String>> {
    let mut keys: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    ndjson_object_stream(r, |obj| {
        let (_, ks) = parse_object_keys(obj, 0).map_err(|e| match e {
            JsonErr::NeedMore => TurboError::Format("truncated NDJSON object".into()),
            JsonErr::Format(s) => TurboError::Format(s),
        })?;
        for k in ks {
            if seen.insert(k.clone()) {
                keys.push(k);
            }
        }
        Ok(())
    })?;
    Ok(keys)
}

fn discover_ndjson_keys(path: &str) -> TurboResult<Vec<String>> {
    let f = std::fs::File::open(path)?;
    discover_ndjson_keys_from(std::io::BufReader::new(f))
}

/// Stream NDJSON rows into a sheet from any `BufRead`.
fn read_ndjson_into_sheet_from<R: BufRead>(
    sheet: &mut Sheet,
    r: R,
    keys: &[String],
) -> TurboResult<()> {
    sheet.rows.push(header_row(keys));
    let mut row_no = 2u32;
    ndjson_object_stream(r, |obj| {
        let (_, pairs) = parse_object_full(obj, 0).map_err(|e| match e {
            JsonErr::NeedMore => TurboError::Format("truncated NDJSON object".into()),
            JsonErr::Format(s) => TurboError::Format(s),
        })?;
        sheet.rows.push(record_to_row(&pairs, keys, row_no));
        row_no += 1;
        Ok(())
    })?;
    Ok(())
}

fn read_ndjson_into_sheet(sheet: &mut Sheet, path: &str, keys: &[String]) -> TurboResult<()> {
    let f = std::fs::File::open(path)?;
    read_ndjson_into_sheet_from(sheet, std::io::BufReader::new(f), keys)
}

/// Columns shape: top-level object of `"key": [v, ...]`. Each column array is
/// parsed into memory in turn (peak ≈ sheet model, inherent to the row-based
/// write target); column arrays may differ in length — shorter columns pad
/// with empty cells. Reads from any `BufRead`.
fn read_columns_into_sheet_from<R: BufRead>(sheet: &mut Sheet, r: R) -> TurboResult<()> {
    let mut r = ChunkReader::new(r);
    skip_bom(&mut r)?;
    expect_char(&mut r, b'{')?;

    let mut keys: Vec<String> = Vec::new();
    let mut columns: Vec<Vec<JVal>> = Vec::new();
    let mut nrows = 0usize;

    loop {
        skip_ws_in_reader(&mut r)?;
        let is_end = {
            let s = r.slice();
            if s.is_empty() {
                return Err(truncated());
            }
            s[0] == b'}'
        };
        if is_end {
            r.advance(1);
            break;
        }
        let (n, key) = retry_parse(&mut r, |b| parse_string(b, 0))?;
        r.advance(n);
        keys.push(key);
        expect_char(&mut r, b':')?;
        expect_char(&mut r, b'[')?;

        let mut col: Vec<JVal> = Vec::new();
        loop {
            skip_ws_in_reader(&mut r)?;
            let is_end = {
                let s = r.slice();
                if s.is_empty() {
                    return Err(truncated());
                }
                s[0] == b']'
            };
            if is_end {
                r.advance(1);
                break;
            }
            let (n, v) = retry_parse(&mut r, |b| parse_value(b, 0))?;
            col.push(v);
            r.advance(n);
            skip_ws_in_reader(&mut r)?;
            let is_end = {
                let s = r.slice();
                if s.is_empty() {
                    return Err(truncated());
                }
                match s[0] {
                    b',' => false,
                    b']' => true,
                    _ => {
                        return Err(TurboError::Format(
                            "expected ',' or ']' in column array".into(),
                        ));
                    }
                }
            };
            r.advance(1);
            if is_end {
                break;
            }
        }
        nrows = nrows.max(col.len());
        columns.push(col);

        skip_ws_in_reader(&mut r)?;
        let is_end = {
            let s = r.slice();
            if s.is_empty() {
                return Err(truncated());
            }
            match s[0] {
                b',' => false,
                b'}' => true,
                _ => {
                    return Err(TurboError::Format(
                        "expected ',' or '}' in columns object".into(),
                    ));
                }
            }
        };
        r.advance(1);
        if is_end {
            break;
        }
    }

    sheet.rows.push(header_row(&keys));
    for rr in 0..nrows {
        let mut row = Row::new(rr as u32 + 2);
        for (ci, col) in columns.iter().enumerate() {
            match col.get(rr) {
                None | Some(JVal::Null) => {}
                Some(JVal::Num(x)) => row
                    .cells
                    .push(Cell::new(ci as u32 + 1, CellValue::Number(*x))),
                Some(JVal::Bool(b)) => row
                    .cells
                    .push(Cell::new(ci as u32 + 1, CellValue::Bool(*b))),
                Some(JVal::Str(s)) => row
                    .cells
                    .push(Cell::new(ci as u32 + 1, CellValue::Str(s.clone()))),
                Some(JVal::Other(t)) => row
                    .cells
                    .push(Cell::new(ci as u32 + 1, CellValue::Str(t.clone()))),
            }
        }
        sheet.rows.push(row);
    }
    Ok(())
}

fn read_columns_into_sheet(sheet: &mut Sheet, path: &str) -> TurboResult<()> {
    let f = std::fs::File::open(path)?;
    read_columns_into_sheet_from(sheet, std::io::BufReader::new(f))
}

// ----------------------------------------------------------------------------
// Unit tests
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::io::Cursor;

    fn json_out() -> JsonOut<Vec<u8>> {
        JsonOut::new(Vec::new())
    }

    #[test]
    fn serial_1900_system() {
        // 2020-01-15, 1900-02-28 (=59), 1900-03-01 (=61), 1900-01-01 (=1,
        // the Windows leap-bug shift; openpyxl `from_excel` maps 0<serial<60
        // to day+1, so serial 1 and 2 both land on 1900-01-01).
        assert_eq!(serial_to_civil(43845.0, false).0, 2020);
        assert_eq!(serial_to_civil(43845.0, false).1, 1);
        assert_eq!(serial_to_civil(43845.0, false).2, 15);
        assert_eq!(
            (
                serial_to_civil(59.0, false).1,
                serial_to_civil(59.0, false).2
            ),
            (2, 28)
        );
        assert_eq!(
            (
                serial_to_civil(61.0, false).1,
                serial_to_civil(61.0, false).2
            ),
            (3, 1)
        );
        assert_eq!(serial_to_civil(1.0, false), (1900, 1, 1, 0, 0, 0, 0));
    }

    #[test]
    fn serial_1904_system() {
        // Serial 0 = 1904-01-01; 43845 days later is 2024-01-16.
        assert_eq!(serial_to_civil(0.0, true), (1904, 1, 1, 0, 0, 0, 0));
        assert_eq!(serial_to_civil(43845.0, true), (2024, 1, 16, 0, 0, 0, 0));
    }

    #[test]
    fn format_date_iso_defaults() {
        assert_eq!(format_date_serial(43845.0, false, ""), "2020-01-15");
        assert_eq!(
            format_date_serial(43845.5, false, ""),
            "2020-01-15T12:00:00"
        );
        // 14:00 → fraction 7/12 of a day.
        assert_eq!(
            format_date_serial(43845.5833333, false, ""),
            "2020-01-15T13:59:59.997"
        );
        assert_eq!(format_date_serial(43845.0, false, "%d/%m/%Y"), "15/01/2020");
        assert_eq!(format_date_serial(43845.0, false, "%Y-%m"), "2020-01");
    }

    #[test]
    fn strftime_tokens() {
        assert_eq!(strftime("%Y-%m-%d", 2020, 1, 15, 0, 0, 0, 0), "2020-01-15");
        assert_eq!(
            strftime("%y/%m/%d %H:%M:%S", 2020, 1, 15, 9, 5, 3, 0),
            "20/01/15 09:05:03"
        );
        assert_eq!(strftime("100%% %f", 2020, 1, 15, 0, 0, 0, 7), "100% 007");
    }

    #[test]
    fn number_emission() {
        let mut o = json_out();
        o.push_number(3.0).unwrap();
        o.push(b",").unwrap();
        o.push_number(-2.5).unwrap();
        o.push(b",").unwrap();
        o.push_number(9_007_199_254_740_992.0).unwrap(); // 2^53 exact → number
        o.push(b",").unwrap();
        o.push_number(9_007_199_254_740_993.0).unwrap(); // stored as 2^53 → number
        o.push(b",").unwrap();
        o.push_number(f64::NAN).unwrap();
        o.push(b",").unwrap();
        o.push_number(f64::INFINITY).unwrap();
        o.push(b",").unwrap();
        o.push_number(f64::NEG_INFINITY).unwrap();
        o.push(b",").unwrap();
        o.push_number(0.1).unwrap();
        o.push(b",").unwrap();
        o.push_number(1e300).unwrap();
        o.flush().unwrap();
        // 1e300 is integral and > 2^53 → emitted as a string (documented fidelity rule).
        assert_eq!(
            String::from_utf8(o.w).unwrap(),
            "3,-2.5,9007199254740992,9007199254740992,null,null,null,0.1,\"1e300\""
        );
    }

    #[test]
    fn string_escaping() {
        let mut o = json_out();
        o.write_str("a\"b\\c\u{1}café ☃").unwrap();
        o.flush().unwrap();
        assert_eq!(
            String::from_utf8(o.w).unwrap(),
            "\"a\\\"b\\\\c\\u0001café ☃\""
        );
        let mut o = json_out();
        o.write_str("tab\tlf\ncr\rs").unwrap();
        o.flush().unwrap();
        assert_eq!(
            String::from_utf8(o.w).unwrap(),
            "\"tab\\u0009lf\\u000acr\\u000ds\""
        );
    }

    #[test]
    fn parse_string_escapes() {
        let s = br#""a\"b\\c\ud83d\ude00\t""#;
        let (n, out) = parse_string(s, 0).unwrap();
        assert_eq!(n, s.len());
        assert_eq!(out, "a\"b\\c\u{1F600}\t");
    }

    #[test]
    fn parse_number_fidelity() {
        // 20-digit identifier as a number: kept as a string.
        let s = b"12345678901234567890";
        let (n, v) = parse_number(s, 0, true).unwrap();
        assert_eq!(n, s.len());
        assert!(matches!(v, Some(JVal::Str(ref x)) if x == "12345678901234567890"));
        // Exactly 2^53 still a number.
        let s = b"9007199254740992";
        let (_, v) = parse_number(s, 0, true).unwrap();
        assert!(matches!(v, Some(JVal::Num(x)) if x == 9007199254740992.0));
        // Fraction stays a number.
        let s = b"3.5";
        let (_, v) = parse_number(s, 0, true).unwrap();
        assert!(matches!(v, Some(JVal::Num(x)) if x == 3.5));
    }

    #[test]
    fn parse_object_basics() {
        let s = br#"{"a": 1, "b": "x", "c": null, "d": true, "e": [1,2], "f": {"g":1}}"#;
        let (n, obj) = parse_object_full(s, 0).unwrap();
        assert_eq!(n, s.len());
        assert_eq!(obj.len(), 6);
        assert!(matches!(obj[0], (ref k, JVal::Num(1.0)) if k == "a"));
        assert!(matches!(obj[1], (ref k, JVal::Str(ref v)) if k == "b" && v == "x"));
        assert!(matches!(obj[2], (ref k, JVal::Null) if k == "c"));
        assert!(matches!(obj[3], (ref k, JVal::Bool(true)) if k == "d"));
        assert!(matches!(obj[4], (ref k, JVal::Other(ref t)) if k == "e" && t == "[1,2]"));
        assert!(matches!(obj[5], (ref k, JVal::Other(ref t)) if k == "f" && t == "{\"g\":1}"));
        let (_, keys) = parse_object_keys(s, 0).unwrap();
        assert_eq!(keys, vec!["a", "b", "c", "d", "e", "f"]);
    }

    #[test]
    fn parse_heterogeneous_objects() {
        let s = br#"[{"a":1,"b":2},{"b":3,"c":4}]"#;
        // first-seen key union
        let mut r = ChunkReader::new(Cursor::new(s));
        let mut keys: Vec<String> = Vec::new();
        let mut seen = HashSet::new();
        for_each_records_element(
            &mut r,
            |b| parse_object_keys(b, 0),
            |ks: Vec<String>| {
                for k in ks {
                    if seen.insert(k.clone()) {
                        keys.push(k);
                    }
                }
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(keys, vec!["a", "b", "c"]);
        assert!(r.slice().is_empty());
    }

    /// A reader that yields at most `max` bytes per read, forcing the parser's
    /// NeedMore/refill path across chunk boundaries.
    struct PieceReader {
        data: Vec<u8>,
        pos: usize,
        max: usize,
    }
    impl Read for PieceReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let want = self.max.min(buf.len());
            let n = want.min(self.data.len() - self.pos);
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    #[test]
    fn streaming_parse_across_chunks() {
        let doc = b"  [{\"a\":\"split\",\"b\":12345678901234567890}, {\"c\":[1,2,3]}]  ";
        for max in [1usize, 3, 7, 64] {
            let mut r = ChunkReader::new(PieceReader {
                data: doc.to_vec(),
                pos: 0,
                max,
            });
            let mut out: Vec<(String, JVal)> = Vec::new();
            for_each_records_element(
                &mut r,
                |b| parse_object_full(b, 0),
                |mut obj| {
                    out.append(&mut obj);
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(out.len(), 3, "max={max}: 2 members of obj 1 + 1 of obj 2");
            assert_eq!(out[0].0, "a");
            assert!(matches!(&out[0].1, JVal::Str(s) if s == "split"));
            // Big integer kept as a string across chunk splits.
            assert_eq!(out[1].0, "b");
            assert!(matches!(&out[1].1, JVal::Str(s) if s == "12345678901234567890"));
            assert_eq!(out[2].0, "c");
            assert!(matches!(&out[2].1, JVal::Other(t) if t == "[1,2,3]"));
        }
    }

    #[test]
    fn number_emitter_via_records_roundtrip_uses_string_for_big_int() {
        // Emitting a numeric cell > 2^53 produces a quoted string (fidelity).
        let mut o = json_out();
        o.push_number(1.0e19).unwrap();
        o.flush().unwrap();
        assert_eq!(String::from_utf8(o.w).unwrap(), "\"1e19\"");
    }
}
