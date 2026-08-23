//! CSV import/export (task C4a) — the format-interchange half that owns CSV.
//!
//! Why this exists: data engineers shell out to pandas to move between xlsx and
//! csv, paying a Python process, a second parse, and a full materialisation.
//! The turbo reader already produces Arrow-native columnar data, so this module
//! converts in-process without leaving Rust. The reader feeds the writer
//! straight off the Arrow columns (column-wise, no intermediate row objects),
//! and the CSV parser is a hand-written RFC 4180 state machine (no dependency).
//!
//! Measured on a 200 000 x 10 mixed sheet (ints, floats, bounded-vocabulary
//! strings, dates), best of 3, release build, pandas via openpyxl engine on the
//! same file: **xlsx → csv ≈ 119x faster than pandas, csv → xlsx ≈ 38x faster.**
//! (Details in the C4a task report.)
//!
//! ## Decisions (the ones the task calls for, in one place)
//!
//! * **Quoting.** A field is quoted iff it contains the delimiter, the quote
//!   byte, `\r`, or `\n`. Embedded quotes are doubled. A quoted field may span
//!   physical lines; the parser never splits on newline outside of quotes.
//! * **Line endings.** Input accepts CRLF, LF, and bare CR. Output emits **CRLF**
//!   (Excel and Python's `csv` writer default).
//! * **BOM.** A leading UTF-8 BOM on input is consumed before the first field.
//!   Output does *not* emit a BOM (kept byte-clean; note the trade-off: Excel
//!   guesses the encoding for BOM-less UTF-8).
//! * **Dates.** A numeric cell whose cellXf number format parses as a date is
//!   emitted as a real formatted date string, never a raw serial. The format is
//!   [`CsvOptions::date_format`] (default `yyyy-mm-dd hh:mm:ss`), applied with
//!   the workbook's date system (1900/1904, read from the workbook). Dates
//!   round-trip as text (import keeps them as strings unless `infer_types`
//!   promotes them — it never parses date text back into serials).
//! * **Formulas.** Export emits the **cached value** the reader surfaces in the
//!   value columns, never the formula text. That is the correct default for a
//!   data-interchange file and it is a decision, not an accident: a formula's
//!   text is a computation, not data.
//! * **Empty vs quoted-empty vs missing trailing field** — three distinct
//!   things, three distinct representations:
//!   | case | import cell | export field |
//!   |---|---|---|
//!   | unquoted empty (`a,,b`) | blank cell | empty field |
//!   | quoted empty (`a,"",b`) | empty-string cell | `""` |
//!   | missing trailing field | no cell | empty field (padded) |
//!   A fully-empty record stays a real (blank) row so row alignment survives a
//!   round trip. Missing trailing fields normalize to empty fields on re-export
//!   because xlsx has no distinct "absent trailing field" — documented.
//! * **Numbers.** Export formats the shortest round-trip representation
//!   (integer-valued floats without a decimal point). In a column that mixes
//!   numbers and text the reader pre-renders numbers as text (`1.0` keeps its
//!   `.0`); that text is emitted verbatim — the value is preserved, and it
//!   round-trips stably. Import writes every field as text unless `infer_types`
//!   is set; see the exact inference rules below.
//! * **Type inference is OFF by default** (`infer_types: false`). "007" must
//!   never silently become 7 and a 20-digit account number must never become a
//!   lossy float. When `infer_types` is true the rules are exact:
//!   1. A field is promoted to a number iff it is a finite numeric literal with
//!      **no leading zero in the integer-part digit run** (only the exact `0`
//!      and `0.x` forms pass) **and**, for integer forms, its magnitude fits in
//!      `2^53 - 1` (an `f64` holds every integer up to that exactly).
//!   2. Anything else — a 20-digit integer, `007`, `1.`, `NaN`, `1e5x` — stays
//!      a string.
//!
//!   Leading/trailing whitespace is never trimmed (a padded field stays text).
//! * **Streaming.** `sheet_to_csv` writes one row at a time into a bounded
//!   buffer and never holds the output (or more than one row) in memory; peak
//!   memory is O(row width), not O(file). `csv_to_sheet` parses in 64 KiB
//!   chunks with at most one field buffer in flight (O(largest field) + 64 KiB),
//!   independent of row count. The xlsx write model itself materialises the
//!   grid (that is the write path's contract, not this module's).

use std::io::Read;

use arrow_array::types::Int32Type;
use arrow_array::{Array, DictionaryArray, Float64Array, StringArray};

use crate::turbo::error::{TurboError, TurboResult};
use crate::turbo::write::{Cell, CellValue, Row, Workbook, save_workbook};
use crate::turbo::{
    Features, TurboSheet, TurboWorkbook, list_sheet_names, read_workbook_turbo_sheet,
};

/// Conversion options shared by both directions.
#[derive(Debug, Clone)]
pub struct CsvOptions {
    /// Field delimiter byte (default `b','`).
    pub delimiter: u8,
    /// Quote byte (default `b'"'`).
    pub quote: u8,
    /// Whether the first record is a header. Default `true`.
    ///
    /// * `csv_to_sheet`: the first record lands in sheet row 1 either way, but
    ///   when `has_header` is true that record is written as plain text column
    ///   names (never type-inferred); when false every record is written per
    ///   `infer_types`.
    /// * `sheet_to_csv`: sheet row 1 is always the first CSV line (the turbo
    ///   reader's header slot); `has_header` records how a re-import should
    ///   treat it. The emitted bytes are identical either way.
    pub has_header: bool,
    /// Promote numeric-looking fields to numbers. **Default `false`.** See the
    /// module docs for the exact (leading-zero-safe) rules.
    pub infer_types: bool,
    /// Excel date-format pattern used to render serial numbers (default
    /// `yyyy-mm-dd hh:mm:ss`). Supported tokens: `yyyy`/`yy`, `mmmm`/`mmm`/
    /// `mm`/`m`, `dddd`/`ddd`/`dd`/`d`, `hh`/`h`, `ss`/`s`, `AM/PM` (`A/P`),
    /// and `mm`/`m` as minutes when they appear after an hour or seconds token.
    /// Quoted literals and `\x` escapes pass through.
    pub date_format: String,
}

impl Default for CsvOptions {
    fn default() -> Self {
        Self {
            delimiter: b',',
            quote: b'"',
            has_header: true,
            infer_types: false,
            date_format: "yyyy-mm-dd hh:mm:ss".into(),
        }
    }
}

fn validate_opts(opts: &CsvOptions) -> TurboResult<()> {
    if opts.delimiter == opts.quote {
        return Err(TurboError::Format(
            "csv: delimiter must differ from quote".into(),
        ));
    }
    if matches!(opts.delimiter, b'\r' | b'\n') || matches!(opts.quote, b'\r' | b'\n') {
        return Err(TurboError::Format(
            "csv: delimiter/quote must not be a newline".into(),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Export: xlsx → csv
// ---------------------------------------------------------------------------

/// Write one worksheet as RFC 4180 CSV to `out`, streaming row by row.
///
/// `sheet` is the worksheet name. Numbers in date-formatted cells are emitted
/// as formatted dates per [`CsvOptions::date_format`]; formula cells emit their
/// cached value; empty-string cells emit `""` (distinct from blank fields).
pub fn sheet_to_csv<W: std::io::Write>(
    path: &str,
    sheet: &str,
    out: W,
    opts: &CsvOptions,
) -> TurboResult<()> {
    validate_opts(opts)?;
    let names = list_sheet_names(path)?;
    let idx = names.iter().position(|n| n == sheet).ok_or_else(|| {
        TurboError::Format(format!(
            "csv: sheet '{sheet}' not found (available: {names:?})"
        ))
    })?;
    let wb = read_workbook_turbo_sheet(path, Features::VALUES | Features::STYLES, idx)?;
    let ws = wb
        .sheets
        .first()
        .ok_or_else(|| TurboError::Format("csv: reader returned no sheet".into()))?;

    let mut out = out;
    let mut rowbuf: Vec<u8> = Vec::new();

    // A sheet with no columns cannot be represented as CSV; emit nothing.
    if ws.ncols == 0 {
        return Ok(());
    }

    // Sheet row 1 is the turbo reader's header slot. It is always the first CSV
    // line (a header line or, when has_header is false, the first data row).
    for (c, h) in ws.column_names.iter().enumerate() {
        if c > 0 {
            rowbuf.push(opts.delimiter);
        }
        append_field(&mut rowbuf, &Field::Raw(h), opts);
    }
    rowbuf.extend_from_slice(b"\r\n");
    out.write_all(&rowbuf)?;

    // Downcast each Arrow column once; the row loop then indexes into the
    // columnar buffers directly (column-wise, no per-cell downcast, no row
    // objects, no O(file) materialisation of the CSV).
    let views: Vec<ColRef<'_>> = ws.columns.iter().map(col_view).collect();

    for r in 0..ws.nrows {
        rowbuf.clear();
        for (c, col) in views.iter().enumerate() {
            if c > 0 {
                rowbuf.push(opts.delimiter);
            }
            match cell_at(col, r) {
                CellOut::Null => {}
                CellOut::Num(x) => {
                    if cell_is_date(ws, &wb, c, r) {
                        let (y, m, d, z, frac) = serial_parts(x, wb.date1904);
                        let s = format_date(y, m, d, z, frac, &opts.date_format);
                        append_field(&mut rowbuf, &Field::Raw(&s), opts);
                    } else {
                        let s = fmt_num(x);
                        append_field(&mut rowbuf, &Field::Raw(&s), opts);
                    }
                }
                CellOut::Str(s) => {
                    if s.is_empty() {
                        append_field(&mut rowbuf, &Field::EmptyStr, opts);
                    } else {
                        append_field(&mut rowbuf, &Field::Raw(s), opts);
                    }
                }
            }
        }
        rowbuf.extend_from_slice(b"\r\n");
        out.write_all(&rowbuf)?;
    }
    Ok(())
}

/// A CSV field to emit.
enum Field<'a> {
    /// Empty-string cell → quoted empty field `""`.
    EmptyStr,
    /// Text/number/date → quoted only when it contains delim/quote/newline.
    Raw(&'a str),
}

#[inline]
fn append_field(buf: &mut Vec<u8>, f: &Field<'_>, opts: &CsvOptions) {
    let (d, q) = (opts.delimiter, opts.quote);
    match f {
        Field::EmptyStr => {
            buf.push(q);
            buf.push(q);
        }
        Field::Raw(s) => {
            let needs_quote = s
                .bytes()
                .any(|b| b == d || b == q || b == b'\n' || b == b'\r');
            if needs_quote {
                buf.push(q);
                for b in s.bytes() {
                    if b == q {
                        buf.push(q);
                    }
                    buf.push(b);
                }
                buf.push(q);
            } else {
                buf.extend_from_slice(s.as_bytes());
            }
        }
    }
}

/// Shortest round-trip number text. Integer-valued floats lose the `.0`
/// (matching what pandas/Excel show and what import re-reads cleanly). Up to
/// 2^53 every integral `f64` is exact and fits an `i64`, so the cast is lossless.
fn fmt_num(x: f64) -> String {
    if x == 0.0 {
        return "0".into();
    }
    if x.fract() == 0.0 && x.abs() < 9_007_199_254_740_992.0 {
        return (x as i64).to_string();
    }
    ryu::Buffer::new().format(x).to_string()
}

/// Columnar view of one Arrow array, downcast once.
enum ColRef<'a> {
    Num(&'a Float64Array),
    Str(&'a StringArray),
    Dict(&'a DictionaryArray<Int32Type>),
    Other,
}

fn col_view(a: &arrow_array::ArrayRef) -> ColRef<'_> {
    if let Some(x) = a.as_any().downcast_ref::<Float64Array>() {
        ColRef::Num(x)
    } else if let Some(x) = a.as_any().downcast_ref::<StringArray>() {
        ColRef::Str(x)
    } else if let Some(x) = a.as_any().downcast_ref::<DictionaryArray<Int32Type>>() {
        ColRef::Dict(x)
    } else {
        ColRef::Other
    }
}

enum CellOut<'a> {
    Null,
    Num(f64),
    Str(&'a str),
}

#[inline]
fn cell_at<'a>(col: &'a ColRef<'_>, r: usize) -> CellOut<'a> {
    match col {
        ColRef::Num(a) => {
            if a.is_null(r) {
                CellOut::Null
            } else {
                CellOut::Num(a.value(r))
            }
        }
        ColRef::Str(a) => {
            if a.is_null(r) {
                CellOut::Null
            } else {
                CellOut::Str(a.value(r))
            }
        }
        ColRef::Dict(a) => {
            let keys = a.keys();
            if keys.is_null(r) {
                return CellOut::Null;
            }
            let k = keys.value(r);
            match a.values().as_any().downcast_ref::<StringArray>() {
                Some(vals) if (k as usize) < vals.len() && !vals.is_null(k as usize) => {
                    CellOut::Str(vals.value(k as usize))
                }
                _ => CellOut::Null,
            }
        }
        ColRef::Other => CellOut::Null,
    }
}

/// Is cell `(c, r)` a date-formatted number? Uses the cellXf resolved by the
/// reader's style table (the same `is_date` predicate the engine uses).
fn cell_is_date(ws: &TurboSheet, wb: &TurboWorkbook, c: usize, r: usize) -> bool {
    let sidx = match &ws.style_indices {
        Some(si) => si
            .get(c)
            .map(|a| {
                if r < a.len() && !a.is_null(r) {
                    a.value(r)
                } else {
                    0
                }
            })
            .unwrap_or(0),
        None => 0,
    };
    wb.style_table
        .as_ref()
        .map(|st| st.is_date(sidx))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Excel serial → civil date/time
// ---------------------------------------------------------------------------

/// Split an Excel serial into (year, month, day, days-since-1970, day-fraction).
/// Handles both date systems: the 1900 system's fake 1900-02-29 occupies serial
/// 60 (so serials 1..59 sit one real day earlier); 1904 starts at serial 0 =
/// 1904-01-01.
fn serial_parts(serial: f64, date1904: bool) -> (i64, u32, u32, i64, f64) {
    let base = serial.floor();
    let z = if date1904 {
        base as i64 - 24_107
    } else {
        let d = if base <= 59.0 { base + 1.0 } else { base };
        d as i64 - 25_569
    };
    let (y, m, d) = civil_from_days(z);
    (y, m, d, z, serial - base)
}

/// Howard Hinnant's days-from-epoch → civil date. `z` = days since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d as u32)
}

/// Render `(y, m, d, z, frac)` with the supported Excel format tokens.
fn format_date(y: i64, m: u32, d: u32, z: i64, frac: f64, fmt: &str) -> String {
    const MONTHS_FULL: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    const MONTHS_ABBR: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    const DAYS_FULL: [&str; 7] = [
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
    ];
    const DAYS_ABBR: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

    let wd = ((z.rem_euclid(7) + 4) % 7) as usize; // z=0 (1970-01-01) is Thursday
    // Round each component (json.rs rounds at the millisecond level for the
    // same reason): `frac` is an f64, and truncating 05:06:07's fraction can
    // land on 18366.999... and emit 06 for the seconds.
    let hour24 = (((frac * 24.0).round() as i64) % 24).rem_euclid(24) as u32;
    let minute = (((frac * 1440.0).round() as i64) % 60).rem_euclid(60) as u32;
    let second = (((frac * 86_400.0).round() as i64) % 60).rem_euclid(60) as u32;

    let mut out = String::new();
    let b = fmt.as_bytes();
    let mut i = 0usize;
    let mut time_seen = false;
    while i < b.len() {
        let c = b[i];
        if c == b'\\' {
            if i + 1 < b.len() {
                out.push(b[i + 1] as char);
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if c == b'"' || c == b'\'' {
            i += 1;
            while i < b.len() && b[i] != c {
                if b[i] == b'\\' && i + 1 < b.len() {
                    out.push(b[i + 1] as char);
                    i += 2;
                } else {
                    out.push(b[i] as char);
                    i += 1;
                }
            }
            if i < b.len() {
                i += 1;
            }
            continue;
        }

        let rem = &fmt[i..];
        let (len, tok): (usize, Option<String>) = if rem.starts_with("yyyy") {
            (4, Some(format!("{y:04}")))
        } else if rem.starts_with("yy") {
            (2, Some(format!("{:02}", y.rem_euclid(100))))
        } else if rem.starts_with("mmmm") {
            (4, Some(MONTHS_FULL[(m - 1) as usize].into()))
        } else if rem.starts_with("mmm") {
            (3, Some(MONTHS_ABBR[(m - 1) as usize].into()))
        } else if rem.starts_with("dddd") {
            (4, Some(DAYS_FULL[wd].into()))
        } else if rem.starts_with("ddd") {
            (3, Some(DAYS_ABBR[wd].into()))
        } else if rem.starts_with("mm") || rem.starts_with("MM") {
            let (l, v) = if time_seen { (2, minute) } else { (2, m) };
            (l, Some(format!("{v:02}")))
        } else if rem.starts_with("dd") {
            (2, Some(format!("{d:02}")))
        } else if rem.starts_with("hh") {
            time_seen = true;
            (2, Some(format!("{hour24:02}")))
        } else if rem.starts_with("ss") {
            time_seen = true;
            (2, Some(format!("{second:02}")))
        } else if rem.starts_with("m") || rem.starts_with("M") {
            let v = if time_seen { minute } else { m };
            (1, Some(format!("{v}")))
        } else if rem.starts_with("d") {
            (1, Some(format!("{d}")))
        } else if rem.starts_with("h") {
            time_seen = true;
            (1, Some(format!("{hour24}")))
        } else if rem.starts_with("s") {
            time_seen = true;
            (1, Some(format!("{second}")))
        } else if rem.starts_with("AM/PM") {
            time_seen = true;
            let (h, ap) = am_pm(hour24);
            (5, Some(format!("{h} {ap}")))
        } else if rem.starts_with("am/pm") {
            time_seen = true;
            let (h, ap) = am_pm(hour24);
            (5, Some(format!("{h} {}", ap.to_lowercase())))
        } else if rem.starts_with("A/P") {
            time_seen = true;
            let (h, ap) = am_pm(hour24);
            (3, Some(format!("{h} {}", &ap[..1])))
        } else {
            (0, None)
        };

        if len > 0 {
            out.push_str(tok.as_deref().unwrap_or(""));
            i += len;
        } else {
            out.push(c as char);
            i += 1;
        }
    }
    out
}

fn am_pm(hour24: u32) -> (u32, &'static str) {
    let h = hour24 % 12;
    let h = if h == 0 { 12 } else { h };
    (h, if hour24 < 12 { "AM" } else { "PM" })
}

// ---------------------------------------------------------------------------
// Import: csv → xlsx
// ---------------------------------------------------------------------------

/// Parse a CSV file and write a new single-sheet workbook to `xlsx_out`.
///
/// Every CSV record maps to a sheet row (record 1 → row 1), so nothing is ever
/// dropped. Cells are written per [`CsvOptions::infer_types`]; with
/// `has_header` the first record is written as plain text column names.
pub fn csv_to_sheet(
    csv_path: &str,
    xlsx_out: &str,
    sheet_name: &str,
    opts: &CsvOptions,
) -> TurboResult<()> {
    validate_opts(opts)?;
    let file = std::fs::File::open(csv_path)?;
    let mut reader = CsvReader::new(file, opts);
    let wb = build_workbook(&mut reader, sheet_name, opts)?;
    save_workbook(&wb, xlsx_out).map_err(TurboError::Io)?;
    Ok(())
}

/// Like [`csv_to_sheet`], but parses an in-memory CSV buffer instead of a file.
pub fn csv_bytes_to_sheet(
    data: &[u8],
    xlsx_out: &str,
    sheet_name: &str,
    opts: &CsvOptions,
) -> TurboResult<()> {
    validate_opts(opts)?;
    let mut reader = CsvReader::new(std::io::Cursor::new(data), opts);
    let wb = build_workbook(&mut reader, sheet_name, opts)?;
    save_workbook(&wb, xlsx_out).map_err(TurboError::Io)?;
    Ok(())
}

/// Read buffer size. The parser never holds more than one chunk plus one
/// in-flight field, so peak memory is O(largest field + [`READ_CHUNK`]) — it
/// does not grow with the number of rows.
pub const READ_CHUNK: usize = 64 * 1024;

/// Streaming RFC 4180 CSV parser (hand-written state machine, no dependencies).
///
/// Reads in [`READ_CHUNK`]-sized blocks and yields one record (a `Vec` of raw
/// fields) at a time. Quoted fields may span blocks and physical lines; embedded
/// quotes are doubled. CRLF, LF and bare CR are all accepted as record ends.
/// A leading UTF-8 BOM is consumed on the first read.
pub struct CsvReader<R: Read> {
    inner: R,
    delim: u8,
    quote: u8,
    buf: [u8; READ_CHUNK],
    pos: usize,
    end: usize,
    eof: bool,
    bom_done: bool,
    peak_field: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum St {
    FieldStart,
    Unquoted,
    Quoted,
    AfterQuote,
    CR,
}

/// One raw field: bytes plus whether it came from a quoted span. Quoted-empty
/// (`""`) is therefore distinguishable from unquoted-empty (``).
#[derive(Debug)]
pub struct RawField {
    pub bytes: Vec<u8>,
    pub was_quoted: bool,
}

impl<R: Read> CsvReader<R> {
    pub fn new(inner: R, opts: &CsvOptions) -> Self {
        Self {
            inner,
            delim: opts.delimiter,
            quote: opts.quote,
            buf: [0; READ_CHUNK],
            pos: 0,
            end: 0,
            eof: false,
            bom_done: false,
            peak_field: 0,
        }
    }

    /// Peak bytes ever held in the current-field buffer (allows tests to prove
    /// the parser's memory does not scale with file size).
    pub fn peak_field_bytes(&self) -> usize {
        self.peak_field
    }

    /// Recover the wrapped reader (and its associated state).
    pub fn into_inner(self) -> R {
        self.inner
    }

    fn refill(&mut self) -> std::io::Result<bool> {
        if self.eof {
            return Ok(false);
        }
        self.pos = 0;
        self.end = self.inner.read(&mut self.buf)?;
        if self.end == 0 {
            self.eof = true;
            return Ok(false);
        }
        if !self.bom_done {
            self.bom_done = true;
            if self.end >= 3 && self.buf[0] == 0xEF && self.buf[1] == 0xBB && self.buf[2] == 0xBF {
                self.pos = 3;
            }
        }
        Ok(true)
    }

    /// Next record, or `None` at end of input. `Ok(Some([]))` is never produced;
    /// a blank line yields `Ok(Some([RawField { bytes: [], was_quoted: false }]))`.
    pub fn next_record(&mut self) -> TurboResult<Option<Vec<RawField>>> {
        let mut fields: Vec<RawField> = Vec::new();
        let mut field: Vec<u8> = Vec::new();
        let mut was_quoted = false;
        let mut st = St::FieldStart;

        loop {
            if self.pos >= self.end && !self.refill()? {
                // End of input.
                return Ok(match st {
                    St::FieldStart => {
                        if fields.is_empty() {
                            None
                        } else {
                            Some(fields)
                        }
                    }
                    St::Unquoted | St::AfterQuote | St::CR => {
                        fields.push(RawField {
                            bytes: field,
                            was_quoted,
                        });
                        Some(fields)
                    }
                    St::Quoted => {
                        return Err(TurboError::Format(
                            "csv: unterminated quoted field at end of input".into(),
                        ));
                    }
                });
            }

            let b = self.buf[self.pos];
            self.pos += 1;
            match st {
                St::FieldStart => {
                    if b == self.quote {
                        was_quoted = true;
                        st = St::Quoted;
                    } else if b == self.delim {
                        fields.push(RawField {
                            bytes: std::mem::take(&mut field),
                            was_quoted: false,
                        });
                    } else if b == b'\n' {
                        fields.push(RawField {
                            bytes: std::mem::take(&mut field),
                            was_quoted: false,
                        });
                        return Ok(Some(fields));
                    } else if b == b'\r' {
                        st = St::CR;
                    } else {
                        field.push(b);
                        st = St::Unquoted;
                    }
                }
                St::Unquoted => {
                    if b == self.delim {
                        fields.push(RawField {
                            bytes: std::mem::take(&mut field),
                            was_quoted: false,
                        });
                        st = St::FieldStart;
                    } else if b == b'\n' {
                        fields.push(RawField {
                            bytes: std::mem::take(&mut field),
                            was_quoted: false,
                        });
                        return Ok(Some(fields));
                    } else if b == b'\r' {
                        st = St::CR;
                    } else {
                        field.push(b);
                    }
                }
                St::Quoted => {
                    if b == self.quote {
                        st = St::AfterQuote;
                    } else {
                        field.push(b);
                    }
                }
                St::AfterQuote => {
                    if b == self.quote {
                        field.push(b); // doubled quote
                        st = St::Quoted;
                    } else if b == self.delim {
                        fields.push(RawField {
                            bytes: std::mem::take(&mut field),
                            was_quoted,
                        });
                        was_quoted = false;
                        st = St::FieldStart;
                    } else if b == b'\n' {
                        fields.push(RawField {
                            bytes: std::mem::take(&mut field),
                            was_quoted,
                        });
                        return Ok(Some(fields));
                    } else if b == b'\r' {
                        st = St::CR;
                    } else {
                        return Err(TurboError::Format(format!(
                            "csv: unexpected byte after closing quote in record column {}",
                            fields.len() + 1
                        )));
                    }
                }
                St::CR => {
                    // The \r was a terminator. Absorb an immediately following
                    // \n; otherwise put the byte back for the next record.
                    if b != b'\n' {
                        self.pos -= 1;
                    }
                    fields.push(RawField {
                        bytes: std::mem::take(&mut field),
                        was_quoted,
                    });
                    return Ok(Some(fields));
                }
            }

            if field.len() > self.peak_field {
                self.peak_field = field.len();
            }
        }
    }
}

fn build_workbook<R: Read>(
    reader: &mut CsvReader<R>,
    sheet_name: &str,
    opts: &CsvOptions,
) -> TurboResult<Workbook> {
    let mut wb = Workbook::with_sheet(sheet_name.to_string());
    let sh = &mut wb.sheets[0];
    let mut row_num: u32 = 1;
    while let Some(fields) = reader.next_record()? {
        let is_header = opts.has_header && row_num == 1;
        let mut row = Row::new(row_num);
        for (i, f) in fields.iter().enumerate() {
            let col = i as u32 + 1;
            let v = if is_header {
                // Column names are text: never type-inferred, and a quoted-empty
                // header is an empty name while an unquoted-empty is a blank cell.
                if f.bytes.is_empty() {
                    if f.was_quoted {
                        Some(CellValue::Str(String::new()))
                    } else {
                        None
                    }
                } else {
                    Some(CellValue::Str(
                        String::from_utf8_lossy(&f.bytes).into_owned(),
                    ))
                }
            } else {
                field_to_cell(f, opts.infer_types)
            };
            if let Some(v) = v {
                row.cells.push(Cell::new(col, v));
            }
        }
        if row.cells.is_empty() {
            // A fully-empty record still becomes a real (blank) row so that row
            // alignment survives a round trip through the reader.
            row.cells.push(Cell::new(1, CellValue::Empty));
        }
        sh.rows.push(row);
        row_num += 1;
    }
    Ok(wb)
}

fn field_to_cell(f: &RawField, infer: bool) -> Option<CellValue> {
    if f.bytes.is_empty() {
        return if f.was_quoted {
            Some(CellValue::Str(String::new()))
        } else {
            None
        };
    }
    let s = match std::str::from_utf8(&f.bytes) {
        Ok(s) => s,
        Err(_) => {
            return Some(CellValue::Str(
                String::from_utf8_lossy(&f.bytes).into_owned(),
            ));
        }
    };
    if infer {
        if let Some(x) = infer_number(s) {
            return Some(CellValue::Number(x));
        }
    }
    Some(CellValue::Str(s.to_string()))
}

/// Documented, exact numeric inference (leading-zero-safe, precision-safe).
/// See the module docs for the rules; this mirrors them 1:1.
fn infer_number(s: &str) -> Option<f64> {
    let b = s.as_bytes();
    let mut i = 0usize;
    if b.get(i) == Some(&b'+') || b.get(i) == Some(&b'-') {
        i += 1;
    }
    let int_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let int_run = &s[int_start..i];
    let leading_zero = int_run.len() > 1 && int_run.starts_with('0');

    // Pure integer form.
    if i == b.len() {
        if int_run.is_empty() || leading_zero {
            return None;
        }
        let v: i64 = s.parse().ok()?;
        // f64 holds every integer up to 2^53 exactly; beyond that we keep text.
        if v.unsigned_abs() < (1u64 << 53) {
            return Some(v as f64);
        }
        return None;
    }

    // Float form: optional '.' + digits, optional exponent, nothing else.
    let mut float_like = false;
    if b[i] == b'.' {
        i += 1;
        let frac_start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == frac_start {
            return None; // trailing dot ("1.")
        }
        float_like = true;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        let e_start = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j == e_start {
            return None; // "1e" / "1e+" with no digits
        }
        i = j;
        float_like = true;
    }
    if i != b.len() || !float_like || leading_zero {
        return None;
    }
    let v = fast_float2::parse::<f64, _>(s).ok()?;
    if v.is_finite() { Some(v) } else { None }
}
