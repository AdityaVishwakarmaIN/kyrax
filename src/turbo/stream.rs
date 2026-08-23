//! B6 — bounded-memory streaming read.
//!
//! The eager read path (`read_workbook_turbo*`) inflates the whole sheet part
//! into RAM, so peak RSS is O(sheet): a 2 GB workbook needs ~2 GB to read a
//! single column. This module is an ADDITIONAL mode whose peak memory is
//! O(chunk): it inflates the sheet incrementally, frames it into bounded row
//! windows, and yields one Arrow `RecordBatch` per window. A caller that wants
//! the whole sheet can collect; a caller that wants one column never holds the
//! sheet.
//!
//! ## Why miniz_oxide and not libdeflate
//!
//! The task specified libdeflate for streaming inflate. Reality: libdeflate
//! (both `libdeflater`'s one-shot wrapper and the raw `_ex` entry point it
//! wraps) CANNOT stream. The `_ex` variant does not report input consumed on
//! partial output and does not persist the bitstream between calls, so it
//! cannot resume after an output window fills. `miniz_oxide` (already compiled
//! into every build as `zip`→`flate2`'s inflater) IS a resumable streaming
//! inflater, so it is promoted to a direct dependency here. libdeflate stays
//! on the eager path, untouched.
//!
//! ## Schema stability (the pre-pass)
//!
//! Column type is inferred from data. A streaming reader cannot see the whole
//! sheet before emitting the first batch, so we run a cheap type-only pre-pass
//! over the sheet (two bits per column: has_num / has_str) and seed every
//! window's columns with the resulting [`ColTarget`]s. Every batch therefore
//! has the SAME schema, and that schema is exactly what the eager path
//! produces for the same file (Float64 | Dictionary<Int32,Utf8> | Utf8).
//!
//! ## Memory profile
//!
//! Per batch: one bounded inflated window + the window's columnar output
//! (O(window_bytes)) + the per-window string dictionary. Sparse aggregates
//! (cell errors, row dimensions) accumulate across windows but are O(sparse
//! records). Shared strings load eagerly (bounded, small). Peak memory is
//! independent of sheet size.
//!
//! ## Rayon
//!
//! The streaming path does NOT use rayon. A single DEFLATE stream and its row
//! window are inherently serial (this matches the eager path's inflate, which
//! is also serial; only the eager post-inflate chunked parse is parallel).
//! Batch ORDER is deterministic: windows are produced in sheet order and
//! parsed in order. Callers that want parallelism can open one stream per
//! sheet (each is independent).

use std::io::{Read as _, Seek as _, SeekFrom};
use std::sync::Arc;

use arrow_array::types::Int32Type;
use arrow_array::{
    ArrayRef, DictionaryArray, RecordBatch,
    builder::{Float64Builder, Int32Builder, StringBuilder, UInt32Builder},
};
use arrow_schema::{DataType, Schema};
use zip::read::HasZipMetadata;

use crate::turbo::error::{TurboError, TurboResult};
use crate::turbo::meta::RowDim;
use crate::turbo::scan::{
    CellError, ColTarget, ScanFeat, StringArena, parse_region, parse_shared_strings,
};
use crate::turbo::structural::{RelKind, parse_rels, parse_workbook};

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

/// Tunables for the streaming reader. The memory bound that matters is
/// `window_bytes`; the other fields are safety caps.
#[derive(Clone, Debug)]
pub struct StreamOptions {
    /// Soft target for rows per batch (converts to an inflated-bytes window at
    /// ~64 B/row, so exact row counts per batch vary with row size).
    pub batch_rows: usize,
    /// Inflated bytes pulled per window — the per-batch memory bound.
    pub window_bytes: usize,
    /// A single row larger than this (a hostile or pathological sheet) is an
    /// error rather than an unbounded carry buffer.
    pub max_row_bytes: usize,
    /// The pre-`<sheetData>` region must fit in this (small in practice).
    pub max_pre_bytes: usize,
}

impl Default for StreamOptions {
    fn default() -> Self {
        StreamOptions {
            batch_rows: 65_536,
            window_bytes: 4 * 1024 * 1024,
            max_row_bytes: 16 * 1024 * 1024,
            max_pre_bytes: 1024 * 1024,
        }
    }
}

impl StreamOptions {
    /// Build options from a target batch size (soft: converts to a byte
    /// window at ~64 B/row, so per-batch row counts vary with row size).
    pub fn from_batch_rows(batch_rows: usize) -> Self {
        let rows = batch_rows.max(1);
        let window_bytes = rows.saturating_mul(64).clamp(256 * 1024, 8 * 1024 * 1024);
        StreamOptions {
            batch_rows: rows,
            window_bytes,
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Bounded streaming inflate of one ZIP entry via miniz_oxide
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum InflateMethod {
    Stored,
    Deflated,
}

/// Streaming inflate of a single zip entry's payload: reads the compressed
/// bytes from the file in bounded chunks and feeds miniz_oxide's resumable
/// stream inflater. Both the compressed and the inflated side stay O(chunk).
struct EntryInflate {
    file: std::fs::File,
    data_offset: u64,
    remaining_compressed: usize,
    method: InflateMethod,
    /// Resumable raw-DEFLATE inflater (keeps its own 32 KiB back-reference
    /// window, so matches referencing earlier output survive window boundaries).
    decomp: Option<miniz_oxide::inflate::stream::InflateState>,
    /// Compressed bytes already read but not yet consumed by the inflater.
    comp_carry: Vec<u8>,
    eof: bool,
}

const COMP_CHUNK: usize = 256 * 1024;

impl EntryInflate {
    fn stored(file: std::fs::File, data_offset: u64, csize: usize) -> Self {
        EntryInflate {
            file,
            data_offset,
            remaining_compressed: csize,
            method: InflateMethod::Stored,
            decomp: None,
            comp_carry: Vec::new(),
            eof: csize == 0,
        }
    }

    fn deflated(file: std::fs::File, data_offset: u64, csize: usize) -> Self {
        EntryInflate {
            file,
            data_offset,
            remaining_compressed: csize,
            method: InflateMethod::Deflated,
            decomp: Some(miniz_oxide::inflate::stream::InflateState::new(
                miniz_oxide::DataFormat::Raw,
            )),
            comp_carry: Vec::new(),
            eof: csize == 0,
        }
    }

    /// Read up to `out.len()` inflated bytes. `Ok(0)` means end of entry.
    fn read(&mut self, out: &mut [u8]) -> TurboResult<usize> {
        if self.eof || out.is_empty() {
            return Ok(0);
        }
        match self.method {
            InflateMethod::Stored => {
                let mut n = 0usize;
                while n < out.len() && self.remaining_compressed > 0 {
                    let to_read = (out.len() - n).min(self.remaining_compressed);
                    let got = file_read_at(&mut self.file, &mut out[n..n + to_read], self.data_offset)?;
                    if got == 0 {
                        self.eof = true;
                        break;
                    }
                    self.data_offset += got as u64;
                    self.remaining_compressed = self.remaining_compressed.saturating_sub(got);
                    n += got;
                }
                if self.remaining_compressed == 0 {
                    self.eof = true;
                }
                Ok(n)
            }
            InflateMethod::Deflated => self.read_deflated(out),
        }
    }

    fn read_deflated(&mut self, out: &mut [u8]) -> TurboResult<usize> {
        use miniz_oxide::inflate::stream::inflate;
        use miniz_oxide::{MZError as MzErr, MZFlush, MZStatus};
        let state = self.decomp.as_mut().expect("deflate stream present");
        let mut out_pos = 0usize;
        loop {
            if out_pos == out.len() {
                break;
            }
            if self.comp_carry.is_empty() && self.remaining_compressed > 0 {
                let want = COMP_CHUNK.min(self.remaining_compressed);
                self.comp_carry.resize(want, 0);
                let got = file_read_at(&mut self.file, &mut self.comp_carry, self.data_offset)?;
                self.data_offset += got as u64;
                self.remaining_compressed -= got;
                self.comp_carry.truncate(got);
                if got == 0 {
                    self.remaining_compressed = 0;
                }
            }
            let no_more_input = self.remaining_compressed == 0 && self.comp_carry.is_empty();
            let flush = if no_more_input {
                MZFlush::Finish
            } else {
                MZFlush::None
            };
            let res = inflate(state, &self.comp_carry, &mut out[out_pos..], flush);
            if res.bytes_consumed < self.comp_carry.len() {
                self.comp_carry = self.comp_carry[res.bytes_consumed..].to_vec();
            } else {
                self.comp_carry.clear();
            }
            out_pos += res.bytes_written;
            match res.status {
                Ok(MZStatus::StreamEnd) => {
                    self.eof = true;
                    break;
                }
                Ok(MZStatus::Ok) => {
                    // Either the window filled (loop breaks) or more input is
                    // wanted (loop reads the next file chunk).
                    continue;
                }
                Ok(MZStatus::NeedDict) => {
                    return Err(TurboError::Inflate(
                        "DEFLATE stream requested an unexpected dictionary".into(),
                    ));
                }
                Err(MzErr::Buf) => {
                    // Output window full before the stream ended; the next
                    // `read` call continues with a fresh window.
                    break;
                }
                Err(e) => {
                    return Err(TurboError::Inflate(format!(
                        "corrupt DEFLATE stream in sheet part: {e:?}"
                    )));
                }
            }
        }
        Ok(out_pos)
    }
}

fn file_read_at(file: &mut std::fs::File, buf: &mut [u8], at: u64) -> TurboResult<usize> {
    file.seek(SeekFrom::Start(at))?;
    file.read(buf).map_err(TurboError::Io)
}

// ---------------------------------------------------------------------------
// Row framing
// ---------------------------------------------------------------------------

/// Find the `>` that closes the tag starting at `from`, honoring quoted
/// attribute values so `>` inside a value does not end the tag early.
fn tag_end(x: &[u8], from: usize) -> Option<usize> {
    let mut quote: Option<u8> = None;
    let mut i = from;
    while i < x.len() {
        let b = x[i];
        if let Some(q) = quote {
            if b == q {
                quote = None;
            }
        } else if b == b'"' || b == b'\'' {
            quote = Some(b);
        } else if b == b'>' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// `(start, end)` of the first complete `<row>` in `buf[begin..end]`. A
/// complete row is `<row .../>` or `<row ...>...</row>`. `None` when truncated.
fn first_row_span(buf: &[u8], begin: usize, end: usize) -> Option<(usize, usize)> {
    let o = memchr::memmem::find(&buf[begin..end], b"<row")?;
    let s = begin + o;
    match buf.get(s + 4) {
        Some(b' ') | Some(b'>') | Some(b'/') | Some(b'\t') | Some(b'\n') | Some(b'\r') => {}
        _ => return None,
    }
    let gt = tag_end(buf, s)?;
    if gt >= end {
        return None;
    }
    if buf.get(gt.wrapping_sub(1)) == Some(&b'/') {
        return Some((s, gt + 1));
    }
    let after = gt + 1;
    let rel = memchr::memmem::find(&buf[after..end], b"</row>")?;
    Some((s, after + rel + b"</row>".len()))
}

/// Index just past the last complete row in `buf[begin..end]`.
fn last_complete_row_end(buf: &[u8], begin: usize, end: usize) -> Option<usize> {
    let mut i = begin;
    let mut last = None;
    while i < end {
        match first_row_span(buf, i, end) {
            Some((_, e)) => {
                last = Some(e);
                i = e;
            }
            None => break,
        }
    }
    last
}

/// End of the first complete row, or `None` if truncated.
fn first_complete_row_end(buf: &[u8], begin: usize, end: usize) -> Option<usize> {
    first_row_span(buf, begin, end).map(|(_, e)| e)
}

/// Absolute end of the `<sheetData ...>` open tag (index just past `>`).
fn find_sheetdata_open_end(buf: &[u8]) -> Option<usize> {
    let o = memchr::memmem::find(buf, b"<sheetData")?;
    let gt = tag_end(buf, o)?;
    Some(gt + 1)
}

/// Absolute index of the `</sheetData>` close tag, or `None`.
fn find_sheetdata_close(buf: &[u8]) -> Option<usize> {
    memchr::memmem::find(buf, b"</sheetData>")
}

// ---------------------------------------------------------------------------
// Cell helpers shared by the type pre-pass
// ---------------------------------------------------------------------------

/// `<c` open tag → cell `@r` column index, or `None` when the ref is absent /
/// unparseable (caller falls back to sequential order, matching `parse_region`).
fn cell_col(tag: &[u8]) -> Option<usize> {
    // Naive scan, not `memmem::find`: a cell tag is around twenty bytes and
    // SIMD setup costs more than walking it. `r=` is almost always the first
    // attribute, so this usually matches within a few bytes.
    let vs = if tag.starts_with(b"r=\"") {
        3
    } else {
        let mut i = 0usize;
        loop {
            if i + 4 > tag.len() {
                return None;
            }
            if tag[i] == b' ' && &tag[i..i + 4] == b" r=\"" {
                break i + 4;
            }
            i += 1;
        }
    };
    let ve = vs + memchr::memchr(b'"', &tag[vs..])?;
    let bytes = &tag[vs..ve];
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    let col1 = crate::turbo::formula::letters_to_index(&bytes[..i])?;
    Some((col1 - 1) as usize)
}

fn attr_value<'a>(tag: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    // Stack buffer, not a Vec. This is called once or twice per CELL, and the
    // old version heap-allocated the search pattern every time — hundreds of
    // thousands of allocations inside the pre-pass loop, which is the same
    // shape of mistake as the quadratic documented in
    // PERF_EXPERIMENTS_PHASE2.md section P0: building a structure inside a
    // per-element loop.
    //
    // Attribute names here are `r`, `t`, `s` — two bytes is already generous,
    // but fall back to the allocating path if a caller ever passes a long one
    // rather than silently truncating.
    let need = name.len() + 3;
    if need > 16 {
        let mut pat = Vec::with_capacity(need);
        pat.push(b' ');
        pat.extend_from_slice(name);
        pat.extend_from_slice(b"=\"");
        let o = memchr::memmem::find(tag, &pat)?;
        let vs = o + pat.len();
        let ve = vs + memchr::memchr(b'"', &tag[vs..])?;
        return Some(&tag[vs..ve]);
    }
    let mut buf = [0u8; 16];
    buf[0] = b' ';
    buf[1..1 + name.len()].copy_from_slice(name);
    buf[1 + name.len()] = b'=';
    buf[2 + name.len()] = b'"';
    let pat = &buf[..need];

    // A cell tag is ~20 bytes. `memmem::find` pays SIMD setup that a naive
    // scan over that length does not, so match the pattern head directly and
    // only compare further on a hit.
    let n = pat.len();
    if tag.len() < n {
        return None;
    }
    let first = pat[0];
    let mut i = 0usize;
    while i + n <= tag.len() {
        if tag[i] == first && &tag[i..i + n] == pat {
            let vs = i + n;
            let ve = vs + memchr::memchr(b'"', &tag[vs..])?;
            return Some(&tag[vs..ve]);
        }
        i += 1;
    }
    None
}

// Kept alongside the E5 scan rewrite: the readable predicate the SWAR path
// replaced, retained as the executable statement of what that path decides.
#[allow(dead_code)]
fn is_data_cell(tag: &[u8]) -> bool {
    // Only the value-bearing cell types produce a push. Shared strings need a
    // resolvable index; others are always strings. Everything else is numeric.
    matches!(
        attr_value(tag, b"t"),
        Some(b"inlineStr") | Some(b"str") | Some(b"e")
    ) || attr_value(tag, b"t") == Some(b"s")
}

fn shared_index_in_range(body: &[u8], shared: &StringArena) -> bool {
    let vo = match memchr::memmem::find(body, b"<v>") {
        Some(o) => o + 3,
        None => return false,
    };
    let Some(rel) = memchr::memmem::find(&body[vo..], b"</v>") else {
        return false;
    };
    let raw = &body[vo..vo + rel];
    match crate::turbo::decode::atoi(raw) {
        Some(idx) => shared.try_resolve(idx).is_some(),
        None => false,
    }
}

/// Record type bits for the data cells in a row-aligned region. When
/// `skip_first_row` is true the first row is treated as the header (it still
/// contributes `ncols` but never type bits), mirroring `parse_region`.
fn scan_region_types(
    region: &[u8],
    shared: &StringArena,
    has_num: &mut Vec<bool>,
    has_str: &mut Vec<bool>,
    ncols: &mut usize,
    skip_first_row: bool,
) {
    let data_start = if skip_first_row {
        match first_complete_row_end(region, 0, region.len()) {
            Some(first_end) => {
                scan_region_ncols_only(&region[..first_end], ncols);
                first_end
            }
            None => return,
        }
    } else {
        0
    };
    scan_region_cells(&region[data_start..], shared, has_num, has_str, ncols);
}

fn scan_region_ncols_only(region: &[u8], ncols: &mut usize) {
    let mut ci = 0usize;
    let mut next_col = 0usize;
    while ci < region.len() {
        let Some(o) = memchr::memmem::find(&region[ci..], b"<c") else {
            break;
        };
        let s = ci + o;
        if !matches!(region.get(s + 2), Some(b' ') | Some(b'>') | Some(b'/')) {
            ci = s + 2;
            continue;
        }
        let Some(gt) = tag_end(region, s) else { break };
        let tag = &region[s + 2..gt];
        let col = cell_col(tag).unwrap_or(next_col);
        next_col = col + 1;
        if col + 1 > *ncols {
            *ncols = col + 1;
        }
        ci = s + 1;
    }
}

fn scan_region_cells(
    region: &[u8],
    shared: &StringArena,
    has_num: &mut Vec<bool>,
    has_str: &mut Vec<bool>,
    ncols: &mut usize,
) {
    let mut ci = 0usize;
    let mut next_col = 0usize;
    while ci < region.len() {
        let Some(o) = memchr::memmem::find(&region[ci..], b"<c") else {
            break;
        };
        let s = ci + o;
        if !matches!(region.get(s + 2), Some(b' ') | Some(b'>') | Some(b'/')) {
            ci = s + 2;
            continue;
        }
        let Some(gt) = tag_end(region, s) else { break };
        let self_closing = region.get(gt.wrapping_sub(1)) == Some(&b'/');
        let tag = &region[s + 2..gt];
        let col = cell_col(tag).unwrap_or(next_col);
        next_col = col + 1;
        let nc = col + 1;
        if nc > has_num.len() {
            has_num.resize(nc, false);
        }
        if nc > has_str.len() {
            has_str.resize(nc, false);
        }
        if nc > *ncols {
            *ncols = nc;
        }
        if !self_closing {
            // A column's target is decided by two bits, and neither can be
            // un-set. So once a bit is true, every further cell that would only
            // set that same bit is DEAD WORK — and the dead work here is
            // expensive: finding `</c>`, extracting `<v>`, and fully parsing
            // the value as f64 purely to answer "is this column numeric".
            //
            // Measured: on a 50k x 4 homogeneous numeric sheet the pre-pass
            // spent 57-64 ms, of which inflate was 11 ms and buffer management
            // 0 ms — the rest was this loop parsing every value and discarding
            // it. The type tag alone is enough to classify a string-ish cell,
            // and it is already in `tag`, so the only cell that needs the body
            // is one that might set a bit which is not yet set.
            //
            // This changes no result: skipping a numeric cell when has_num is
            // already true cannot alter the column's target.
            let ty = attr_value(tag, b"t");
            let needs_body = match ty {
                // `t="s"` still needs the body: an out-of-range shared index is
                // NOT a string (the eager path nulls it), so the index must be
                // checked before setting has_str.
                Some(b"s") => !has_str[col],
                Some(b"inlineStr") | Some(b"str") | Some(b"e") => false,
                _ => !has_num[col],
            };
            let e = if needs_body {
                memchr::memmem::find(&region[gt + 1..], b"</c>")
                    .map(|r| gt + 1 + r)
                    .unwrap_or(region.len())
                    .min(region.len())
            } else {
                // Still must step past this cell, but without searching for the
                // close tag: the next `<c` scan finds it anyway.
                gt
            };
            match ty {
                Some(b"s") => {
                    if needs_body && shared_index_in_range(&region[gt + 1..e], shared) {
                        has_str[col] = true;
                    }
                }
                Some(b"inlineStr") | Some(b"str") | Some(b"e") => has_str[col] = true,
                _ => {
                    if needs_body {
                        if let Some(v) = v_content(&region[gt + 1..e]) {
                            if !v.is_empty() && fast_float2::parse::<f64, _>(v).is_ok() {
                                has_num[col] = true;
                            }
                        }
                    }
                }
            }
            ci = if needs_body {
                e.saturating_add(4).min(region.len())
            } else {
                gt + 1
            };
        } else {
            ci = gt + 1;
        }
    }
}

fn v_content(body: &[u8]) -> Option<&[u8]> {
    let vo = memchr::memmem::find(body, b"<v>")?;
    let vs = vo + 3;
    let ve = vs + memchr::memmem::find(&body[vs..], b"</v>")?;
    Some(&body[vs..ve])
}

// ---------------------------------------------------------------------------
// Workbook-level open
// ---------------------------------------------------------------------------

/// Small part reader via the zip crate (bounded; used for workbook.xml, rels,
/// sharedStrings — all small relative to sheet data).
fn read_part_zip(
    archive: &mut zip::ZipArchive<std::fs::File>,
    name: &str,
) -> TurboResult<Option<Vec<u8>>> {
    let mut f = match archive.by_name(name) {
        Ok(f) => f,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => {
            return Err(TurboError::Format(format!(
                "streaming read: zip error for {name}: {e}"
            )));
        }
    };
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).map_err(TurboError::Io)?;
    Ok(Some(buf))
}

fn resolve_sheet_path(
    meta: &crate::turbo::structural::SheetMeta,
    sheet_idx: usize,
    wb_rels: &crate::turbo::structural::RelMap,
) -> String {
    if let Some(rid) = &meta.rid {
        if let Some(rel) = wb_rels.get(rid) {
            return crate::turbo::structural::resolve_zip_path("xl/", &rel.target);
        }
    }
    format!("xl/worksheets/sheet{}.xml", sheet_idx + 1)
}

// ---------------------------------------------------------------------------
// Per-sheet streaming reader
// ---------------------------------------------------------------------------

/// A per-sheet bounded-memory reader. Owns everything it needs: the raw file
/// handle, the streaming inflater, the schema pre-pass result, and the window
/// state. Drop it to free the file handle.
pub struct SheetStream {
    path: String,
    inflate: Option<EntryInflate>,
    entry_offset: u64,
    entry_csize: usize,
    entry_method: InflateMethod,
    shared: Arc<StringArena>,
    targets: Vec<ColTarget>,
    // emit-pass window state
    pending: Vec<u8>,
    phase: StreamPhase,
    header_consumed: bool,
    emitted_any: bool,
    finished: bool,
    data_rows_emitted: usize,
    /// Absolute data-row index where the next emitted batch's rows begin. The
    /// eager path indexes columns by absolute data row (data row 0 = sheet
    /// row 2), padding gaps for sparse `@r` sheets; streaming must match, so
    /// each window's leading gap is padded with nulls.
    emitted_abs_end: usize,
    column_names: Vec<String>,
    total_nrows: usize,
    total_ncols: usize,
    // sparse aggregates (absolute coordinates, deduped)
    cell_errors: Vec<CellError>,
    row_dims: Vec<RowDim>,
    row_dims_seen: std::collections::HashSet<u32>,
}

#[derive(Clone, Copy, PartialEq)]
enum StreamPhase {
    /// Before `<sheetData>` open tag seen.
    Pre,
    /// Inside `<sheetData>` (rows).
    Rows,
    /// Past `</sheetData>` (or self-closing `<sheetData/>`).
    Done,
}

/// Final aggregate result after the last batch has been yielded.
pub struct StreamSummary {
    pub nrows: usize,
    pub ncols: usize,
    pub column_names: Vec<String>,
    pub cell_errors: Vec<CellError>,
    pub row_dims: Vec<RowDim>,
}

impl SheetStream {
    /// Open `path`, resolve sheet `sheet_idx` (workbook order), run the schema
    /// pre-pass, and return a reader positioned to yield its first batch.
    pub fn open(path: &str, sheet_idx: usize, opts: StreamOptions) -> TurboResult<Self> {
        let cd_file = std::fs::File::open(path)?;
        let mut archive = zip::ZipArchive::new(cd_file)
            .map_err(|e| TurboError::Format(format!("streaming read: not a readable ZIP: {e}")))?;

        let wb_xml = read_part_zip(&mut archive, "xl/workbook.xml")?
            .ok_or_else(|| TurboError::MissingPart("xl/workbook.xml".into()))?;
        let (sheet_metas, _) = parse_workbook(&wb_xml);
        if sheet_idx >= sheet_metas.len() {
            return Err(TurboError::Format(format!(
                "sheet index {sheet_idx} out of range ({} sheets)",
                sheet_metas.len()
            )));
        }
        let wb_rels = read_part_zip(&mut archive, "xl/_rels/workbook.xml.rels")?
            .map(|rx| parse_rels(&rx))
            .unwrap_or_default();
        let meta = &sheet_metas[sheet_idx];
        if let Some(rid) = &meta.rid {
            if let Some(rel) = wb_rels.get(rid) {
                if rel.kind == RelKind::Chartsheet {
                    return Self::empty(path, opts);
                }
            }
        }
        let sheet_path = resolve_sheet_path(meta, sheet_idx, &wb_rels);

        let shared = Arc::new(
            read_part_zip(&mut archive, "xl/sharedStrings.xml")?
                .map(|sx| parse_shared_strings(&sx))
                .unwrap_or_else(|| parse_shared_strings(b"")),
        );

        // Locate the sheet entry's raw payload in the file (a second, raw
        // handle: the zip crate owns its own file).
        let mut raw_file = std::fs::File::open(path)?;
        let (data_offset, csize, cmethod) = {
            let f = match archive.by_name(&sheet_path) {
                Ok(f) => f,
                Err(e) => {
                    return Err(TurboError::Format(format!(
                        "streaming read: sheet part {sheet_path}: {e}"
                    )));
                }
            };
            let md = f.get_metadata();
            let off = md
                .data_start(&mut raw_file)
                .map_err(|e| TurboError::Format(format!("streaming read: {e}")))?;
            (off, md.compressed_size as usize, md.compression_method)
        };

        let method = match cmethod {
            zip::CompressionMethod::Stored => InflateMethod::Stored,
            zip::CompressionMethod::Deflated => InflateMethod::Deflated,
            m => {
                return Err(TurboError::Inflate(format!(
                    "unsupported compression method {m:?}"
                )));
            }
        };

        let mut this = SheetStream {
            path: path.to_owned(),
            inflate: None,
            entry_offset: data_offset,
            entry_csize: csize,
            entry_method: method,
            shared,
            targets: Vec::new(),
            pending: Vec::new(),
            phase: StreamPhase::Pre,
            header_consumed: false,
            emitted_any: false,
            finished: false,
            data_rows_emitted: 0,
            emitted_abs_end: 0,
            column_names: Vec::new(),
            total_nrows: 0,
            total_ncols: 0,
            cell_errors: Vec::new(),
            row_dims: Vec::new(),
            row_dims_seen: std::collections::HashSet::new(),
        };
        this.open_inflate()?;
        let (targets, ncols) = this.run_prepass(&opts)?;
        // The pre-pass consumes the whole sheet stream; re-open it for the
        // emit pass (both passes use identical framing, so the windows match).
        this.open_inflate()?;
        this.targets = targets;
        this.total_ncols = ncols;
        Ok(this)
    }

    /// (Re)open a fresh stream over the sheet entry's raw payload.
    fn open_inflate(&mut self) -> TurboResult<()> {
        let file = std::fs::File::open(&self.path)?;
        self.inflate = Some(match self.entry_method {
            InflateMethod::Stored => {
                EntryInflate::stored(file, self.entry_offset, self.entry_csize)
            }
            InflateMethod::Deflated => {
                EntryInflate::deflated(file, self.entry_offset, self.entry_csize)
            }
        });
        Ok(())
    }

    /// An empty sheet (chartsheet with no grid): yields a single 0-row batch.
    fn empty(path: &str, opts: StreamOptions) -> TurboResult<Self> {
        let _ = (path, opts);
        Ok(SheetStream {
            path: path.to_owned(),
            inflate: None,
            entry_offset: 0,
            entry_csize: 0,
            entry_method: InflateMethod::Stored,
            shared: Arc::new(parse_shared_strings(b"")),
            targets: Vec::new(),
            pending: Vec::new(),
            phase: StreamPhase::Done,
            header_consumed: true,
            emitted_any: false,
            finished: false,
            data_rows_emitted: 0,
            emitted_abs_end: 0,
            column_names: Vec::new(),
            total_nrows: 0,
            total_ncols: 0,
            cell_errors: Vec::new(),
            row_dims: Vec::new(),
            row_dims_seen: std::collections::HashSet::new(),
        })
    }

    pub fn column_names(&self) -> &[String] {
        &self.column_names
    }

    pub fn total_nrows(&self) -> usize {
        self.total_nrows
    }

    pub fn total_ncols(&self) -> usize {
        self.total_ncols
    }

    // -- schema pre-pass ---------------------------------------------------

    /// Stream the whole sheet once, framing rows and recording per-column type
    /// bits. This is what fixes the schema before the first batch is emitted.
    fn run_prepass(&mut self, opts: &StreamOptions) -> TurboResult<(Vec<ColTarget>, usize)> {
        let mut has_num: Vec<bool> = Vec::new();
        let mut has_str: Vec<bool> = Vec::new();
        let mut ncols = 0usize;
        let mut pending: Vec<u8> = Vec::new();
        let mut phase = StreamPhase::Pre;
        let mut header_skipped = false;
        let mut chunk = vec![0u8; opts.window_bytes.max(64 * 1024)];
        let mut inflate = self.inflate.as_mut();

        loop {
            if phase == StreamPhase::Pre {
                if let Some(p) = find_sheetdata_open_end(&pending) {
                    let self_closing = pending.get(p.wrapping_sub(1)) == Some(&b'/');
                    pending.drain(..p);
                    phase = if self_closing {
                        StreamPhase::Done
                    } else {
                        StreamPhase::Rows
                    };
                    continue;
                } else if pending.len() > opts.max_pre_bytes {
                    return Err(TurboError::Format(
                        "streaming read: <sheetData> not found in worksheet header".into(),
                    ));
                }
            }

            if phase == StreamPhase::Rows {
                let rows_end = find_sheetdata_close(&pending).unwrap_or(pending.len());
                if let Some(cut) = last_complete_row_end(&pending, 0, rows_end) {
                    scan_region_types(
                        &pending[..cut],
                        self.shared.as_ref(),
                        &mut has_num,
                        &mut has_str,
                        &mut ncols,
                        !header_skipped,
                    );
                    header_skipped = true;
                    pending.drain(..cut);
                    continue;
                } else if pending.len() > opts.max_row_bytes {
                    return Err(TurboError::Format(
                        "streaming read: a single row exceeds the row-size cap".into(),
                    ));
                }
            } else if phase == StreamPhase::Done {
                break;
            }

            let n = match inflate {
                Some(ref mut i) => i.read(&mut chunk)?,
                None => 0,
            };
            if n == 0 {
                if phase == StreamPhase::Rows {
                    if let Some(cut) = last_complete_row_end(&pending, 0, pending.len()) {
                        scan_region_types(
                            &pending[..cut],
                            self.shared.as_ref(),
                            &mut has_num,
                            &mut has_str,
                            &mut ncols,
                            !header_skipped,
                        );
                    }
                }
                break;
            }
            pending.extend_from_slice(&chunk[..n]);
        }

        let ncols_final = ncols.max(has_num.len()).max(has_str.len());
        let targets = (0..ncols_final)
            .map(|c| {
                let num = has_num.get(c).copied().unwrap_or(false);
                let s = has_str.get(c).copied().unwrap_or(false);
                match (num, s) {
                    (true, true) => ColTarget::Mixed,
                    (false, true) => ColTarget::Str,
                    _ => ColTarget::Num,
                }
            })
            .collect();
        Ok((targets, ncols_final))
    }

    // -- emit pass ---------------------------------------------------------

    /// Yield the next RecordBatch, or `None` at the end of the sheet. Each
    /// batch is a bounded window of complete rows with a stable schema.
    pub fn next_batch(&mut self, opts: &StreamOptions) -> TurboResult<Option<RecordBatch>> {
        if self.finished {
            return Ok(None);
        }
        let mut chunk = vec![0u8; opts.window_bytes.max(64 * 1024)];
        loop {
            if self.phase == StreamPhase::Pre {
                if let Some(p) = find_sheetdata_open_end(&self.pending) {
                    let self_closing = self.pending.get(p.wrapping_sub(1)) == Some(&b'/');
                    self.pending.drain(..p);
                    self.phase = if self_closing {
                        StreamPhase::Done
                    } else {
                        StreamPhase::Rows
                    };
                    continue;
                } else if self.pending.len() > opts.max_pre_bytes {
                    return Err(TurboError::Format(
                        "streaming read: <sheetData> not found in worksheet header".into(),
                    ));
                }
            }

            if self.phase == StreamPhase::Rows {
                let rows_end = find_sheetdata_close(&self.pending).unwrap_or(self.pending.len());
                if let Some(cut) = last_complete_row_end(&self.pending, 0, rows_end) {
                    let region = self.pending[..cut].to_vec();
                    self.pending.drain(..cut);
                    return self.emit_region(&region);
                } else if self.pending.len() > opts.max_row_bytes {
                    return Err(TurboError::Format(
                        "streaming read: a single row exceeds the row-size cap".into(),
                    ));
                }
            }

            let n = match self.inflate.as_mut() {
                Some(i) => i.read(&mut chunk)?,
                None => 0,
            };
            if n == 0 {
                // End of entry: flush any complete rows that remain.
                if self.phase == StreamPhase::Rows {
                    if let Some(cut) = last_complete_row_end(&self.pending, 0, self.pending.len()) {
                        let region = self.pending[..cut].to_vec();
                        self.pending.drain(..cut);
                        return self.emit_region(&region);
                    }
                }
                self.finish();
                if !self.emitted_any {
                    // Empty sheet: one empty batch, matching the eager path's
                    // 0-row TurboSheet.
                    let batch = build_values_batch(&self.column_names, Vec::new(), 0)?;
                    self.emitted_any = true;
                    return Ok(Some(batch));
                }
                return Ok(None);
            }
            self.pending.extend_from_slice(&chunk[..n]);
        }
    }

    fn emit_region(&mut self, region: &[u8]) -> TurboResult<Option<RecordBatch>> {
        let has_header = !self.header_consumed;
        self.header_consumed = true;
        let feat = ScanFeat {
            styles: false,
            formulas: false,
            row_meta: true,
        };
        let mut partial = parse_region(
            region,
            0,
            region.len(),
            has_header,
            Some(self.shared.as_ref()),
            feat,
            self.data_rows_emitted,
            Some(&self.targets),
        );
        let abs_start = partial.abs_start;
        let window_row_dims = std::mem::take(&mut partial.row_dims);
        let (header, columns, _style, _formulas, window_errors, nrows, _ncols) =
            partial.into_arrow_columns()?;

        // Align to the eager path's absolute row indexing: pad the leading gap
        // between the previous batch's end and this window's first data row.
        let pad = abs_start.saturating_sub(self.emitted_abs_end);
        let columns = pad_batch_columns(columns, pad);
        self.emitted_abs_end = self.emitted_abs_end.max(abs_start + nrows);

        if self.column_names.is_empty() {
            self.column_names = header;
        }
        self.total_nrows += pad + nrows;
        self.data_rows_emitted += nrows;
        for rd in window_row_dims {
            if self.row_dims_seen.insert(rd.row) {
                self.row_dims.push(rd);
            }
        }
        for e in window_errors {
            self.cell_errors.push(e);
        }
        self.emitted_any = true;
        let batch = build_values_batch(&self.column_names, columns, nrows)?;
        Ok(Some(batch))
    }

    fn finish(&mut self) {
        self.finished = true;
        self.phase = StreamPhase::Done;
        self.row_dims.sort_by_key(|r| r.row);
    }

    /// Consume the reader and return the aggregated result.
    pub fn into_summary(mut self) -> StreamSummary {
        if !self.finished {
            self.finish();
        }
        StreamSummary {
            nrows: self.total_nrows,
            ncols: self.total_ncols,
            column_names: self.column_names,
            cell_errors: self.cell_errors,
            row_dims: self.row_dims,
        }
    }

    /// Clone of the aggregated result (borrows; the stream need not be consumed).
    pub fn summary(&self) -> StreamSummary {
        StreamSummary {
            nrows: self.total_nrows,
            ncols: self.total_ncols,
            column_names: self.column_names.clone(),
            cell_errors: self.cell_errors.clone(),
            row_dims: self.row_dims.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// RecordBatch / aggregate builders
// ---------------------------------------------------------------------------

/// Prepend `n` nulls to each column (sparse-`@r` gap padding so batches stay
/// aligned with the eager path's absolute row indexing).
fn pad_batch_columns(columns: Vec<ArrayRef>, n: usize) -> Vec<ArrayRef> {
    if n == 0 {
        return columns;
    }
    columns.into_iter().map(|c| prepend_nulls(c, n)).collect()
}

fn prepend_nulls(col: ArrayRef, n: usize) -> ArrayRef {
    use arrow_array::Array;
    match col.data_type() {
        DataType::Float64 => {
            let src = col
                .as_any()
                .downcast_ref::<arrow_array::Float64Array>()
                .unwrap();
            let mut b = Float64Builder::with_capacity(n + src.len());
            for _ in 0..n {
                b.append_null();
            }
            for i in 0..src.len() {
                if src.is_null(i) {
                    b.append_null();
                } else {
                    b.append_value(src.value(i));
                }
            }
            Arc::new(b.finish())
        }
        DataType::Utf8 => {
            let src = col
                .as_any()
                .downcast_ref::<arrow_array::StringArray>()
                .unwrap();
            let mut b = StringBuilder::with_capacity(n + src.len(), 0);
            for _ in 0..n {
                b.append_null();
            }
            for i in 0..src.len() {
                if src.is_null(i) {
                    b.append_null();
                } else {
                    b.append_value(src.value(i));
                }
            }
            Arc::new(b.finish())
        }
        DataType::Dictionary(k, v)
            if matches!(k.as_ref(), DataType::Int32) && matches!(v.as_ref(), DataType::Utf8) =>
        {
            let src = col
                .as_any()
                .downcast_ref::<DictionaryArray<Int32Type>>()
                .unwrap();
            let keys = src.keys();
            let values = src.values().clone();
            let mut kb = Int32Builder::with_capacity(n + keys.len());
            for _ in 0..n {
                kb.append_null();
            }
            for i in 0..keys.len() {
                if keys.is_null(i) {
                    kb.append_null();
                } else {
                    kb.append_value(keys.value(i));
                }
            }
            let arr = DictionaryArray::<Int32Type>::try_new(kb.finish(), values)
                .expect("dictionary keys in range");
            Arc::new(arr)
        }
        other => panic!("unexpected streaming column type {other}"),
    }
}

/// Build a RecordBatch with the given field names from already-typed Arrow
/// columns, mirroring `PyTurboSheet::to_arrow` so the streaming schema equals
/// the eager schema exactly. Every field is declared nullable: a streaming
/// consumer must accept the same schema for every batch, and nullability can
/// vary per batch only if declared nullable up front.
pub fn build_values_batch(
    names: &[String],
    columns: Vec<ArrayRef>,
    nrows: usize,
) -> TurboResult<RecordBatch> {
    use arrow_array::RecordBatchOptions;
    if columns.is_empty() {
        return RecordBatch::try_new_with_options(
            Arc::new(Schema::empty()),
            vec![],
            &RecordBatchOptions::new().with_row_count(Some(nrows)),
        )
        .map_err(|e| TurboError::Arrow(e.to_string()));
    }
    RecordBatch::try_from_iter_with_nullable(
        names
            .iter()
            .cloned()
            .zip(columns)
            .map(|(name, arr)| (name, arr, true)),
    )
    .map_err(|e| TurboError::Arrow(e.to_string()))
}

/// Sparse typed error caches → RecordBatch: row, col, code.
pub fn cell_errors_to_batch(errs: &[CellError]) -> TurboResult<RecordBatch> {
    let n = errs.len();
    let mut row_b = UInt32Builder::with_capacity(n);
    let mut col_b = UInt32Builder::with_capacity(n);
    let mut code_b = StringBuilder::with_capacity(n, n.saturating_mul(8));
    for e in errs {
        row_b.append_value(e.row);
        col_b.append_value(e.col);
        code_b.append_value(&e.code);
    }
    let row_arr: ArrayRef = Arc::new(row_b.finish());
    let col_arr: ArrayRef = Arc::new(col_b.finish());
    let code_arr: ArrayRef = Arc::new(code_b.finish());
    RecordBatch::try_from_iter_with_nullable([
        ("row", row_arr, false),
        ("col", col_arr, false),
        ("code", code_arr, false),
    ])
    .map_err(|e| TurboError::Arrow(e.to_string()))
}
