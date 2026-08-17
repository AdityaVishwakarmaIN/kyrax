//! Memchr single-pass sheet scanner with typed columnar output and rayon chunk-parallel parse.

use super::decode::{atoi, decode};
use super::error::{TurboError, TurboResult};
use super::formula;
use super::structural::CellRange;

// ----------------------------------------------------------------------------
// BitVec / Dict / StringArena / Column (from struct_proto)
// ----------------------------------------------------------------------------

pub(crate) struct BitVec {
    pub bits: Vec<u64>,
    pub len: usize,
}
impl BitVec {
    fn new() -> Self {
        BitVec {
            bits: Vec::new(),
            len: 0,
        }
    }
    #[inline]
    fn push(&mut self, v: bool) {
        if self.len % 64 == 0 {
            self.bits.push(0);
        }
        if v {
            let w = self.len / 64;
            self.bits[w] |= 1u64 << (self.len % 64);
        }
        self.len += 1;
    }
    #[allow(dead_code)]
    fn extend(&mut self, other: &BitVec) {
        for i in 0..other.len {
            let w = i / 64;
            let bit = (other.bits[w] >> (i % 64)) & 1 == 1;
            self.push(bit);
        }
    }
    #[inline]
    pub fn get(&self, i: usize) -> bool {
        if i >= self.len {
            return false;
        }
        (self.bits[i / 64] >> (i % 64)) & 1 == 1
    }
}

pub(crate) struct Dict {
    map: std::collections::HashMap<Box<[u8]>, u32>,
    offsets: Vec<usize>,
    bytes: Vec<u8>,
}
impl Dict {
    fn new() -> Self {
        Dict {
            map: std::collections::HashMap::new(),
            offsets: vec![0],
            bytes: Vec::new(),
        }
    }
    #[inline]
    fn intern(&mut self, key: &[u8]) -> u32 {
        if let Some(&id) = self.map.get(key) {
            return id;
        }
        let id = (self.offsets.len() - 1) as u32;
        self.bytes.extend_from_slice(key);
        self.offsets.push(self.bytes.len());
        self.map.insert(key.into(), id);
        id
    }
    /// Resolve an interned id. Returns empty slice if `id` is out of range
    /// (defensive — Dict ids are always produced by [`Dict::intern`]).
    #[inline]
    pub fn resolve(&self, id: u32) -> &[u8] {
        self.try_resolve(id).unwrap_or(b"")
    }
    #[inline]
    fn try_resolve(&self, id: u32) -> Option<&[u8]> {
        let i = id as usize;
        if i + 1 >= self.offsets.len() {
            return None;
        }
        Some(&self.bytes[self.offsets[i]..self.offsets[i + 1]])
    }
    fn ndistinct(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }
    pub fn strings(&self) -> Vec<String> {
        let n = self.ndistinct();
        (0..n)
            .map(|i| String::from_utf8_lossy(self.resolve(i as u32)).into_owned())
            .collect()
    }
}

const NULL_IDX: u32 = u32::MAX;

#[derive(Debug)]
pub struct StringArena {
    offsets: Vec<usize>,
    bytes: Vec<u8>,
}
impl StringArena {
    /// Resolve shared-string index `id`. Returns `None` when the index is past
    /// the SST (corrupt / truncated sharedStrings.xml) so callers can null the cell.
    #[inline]
    pub fn try_resolve(&self, id: u32) -> Option<&[u8]> {
        let i = id as usize;
        if i + 1 >= self.offsets.len() {
            return None;
        }
        Some(&self.bytes[self.offsets[i]..self.offsets[i + 1]])
    }
}

pub(crate) fn parse_shared_strings(xml: &[u8]) -> StringArena {
    let mut offsets = vec![0usize];
    let mut bytes: Vec<u8> = Vec::new();
    let mut scratch: Vec<u8> = Vec::new();
    let mut i = memchr::memmem::find(xml, b"<sst").unwrap_or(0);
    let end = xml.len();
    while let Some(so) = memchr::memmem::find(&xml[i..end], b"<si") {
        let si_start = i + so;
        let after = xml.get(si_start + 3).copied().unwrap_or(b'>');
        if !(after == b' ' || after == b'>' || after == b'/') {
            i = si_start + 3;
            continue;
        }
        let Some(gt) = memchr::memchr(b'>', &xml[si_start..end]) else {
            break; // truncated open tag
        };
        let si_tag_end = si_start + gt;
        if si_tag_end == 0 || xml.get(si_tag_end.saturating_sub(1)) == Some(&b'/') {
            offsets.push(bytes.len());
            i = si_tag_end + 1;
            continue;
        }
        let si_close = si_tag_end
            + memchr::memmem::find(&xml[si_tag_end..end], b"</si>").unwrap_or(end - si_tag_end);
        let mut p = si_tag_end;
        while let Some(to) = memchr::memmem::find(&xml[p..si_close], b"<t") {
            let topen = p + to;
            let after = xml.get(topen + 2).copied().unwrap_or(b'>');
            if !(after == b' ' || after == b'>' || after == b'/') {
                p = topen + 2;
                continue;
            }
            let topen_end =
                topen + memchr::memchr(b'>', &xml[topen..si_close]).unwrap_or(si_close - topen);
            if topen_end == 0 || xml.get(topen_end.saturating_sub(1)) == Some(&b'/') {
                p = topen_end + 1;
                continue;
            }
            let tclose = topen_end
                + memchr::memmem::find(&xml[topen_end..si_close], b"</t>")
                    .unwrap_or(si_close - topen_end);
            if topen_end + 1 > tclose || tclose > xml.len() {
                break;
            }
            let raw = &xml[topen_end + 1..tclose];
            let decoded = decode(raw, &mut scratch);
            bytes.extend_from_slice(decoded);
            p = tclose + 4;
        }
        offsets.push(bytes.len());
        i = si_close + 5;
    }
    StringArena { offsets, bytes }
}

// ----------------------------------------------------------------------------
#[derive(Debug, Clone)]
pub enum MixedValue {
    Null,
    Num(f64),
    Str(u32),
}

enum Column {
    Unset,
    Num { v: Vec<f64>, valid: BitVec },
    Str(Vec<u32>),
    Mixed(Vec<MixedValue>),
}
impl Column {
    #[inline]
    fn push_num(&mut self, dr: usize, x: f64) {
        if let Column::Unset = self {
            *self = Column::Num {
                v: Vec::new(),
                valid: BitVec::new(),
            };
        }
        match self {
            Column::Num { v, valid } => {
                while v.len() < dr {
                    v.push(f64::NAN);
                    valid.push(false);
                }
                v.push(x);
                valid.push(true);
            }
            Column::Str(s) => {
                let mut m: Vec<MixedValue> = s
                    .iter()
                    .map(|&idx| {
                        if idx == NULL_IDX {
                            MixedValue::Null
                        } else {
                            MixedValue::Str(idx)
                        }
                    })
                    .collect();
                while m.len() < dr {
                    m.push(MixedValue::Null);
                }
                m.push(MixedValue::Num(x));
                *self = Column::Mixed(m);
            }
            Column::Mixed(m) => {
                while m.len() < dr {
                    m.push(MixedValue::Null);
                }
                m.push(MixedValue::Num(x));
            }
            Column::Unset => unreachable!(),
        }
    }
    #[inline]
    fn push_str(&mut self, dr: usize, idx: u32) {
        if let Column::Unset = self {
            *self = Column::Str(Vec::new());
        }
        match self {
            Column::Str(s) => {
                while s.len() < dr {
                    s.push(NULL_IDX);
                }
                s.push(idx);
            }
            Column::Num { v, valid } => {
                let mut m: Vec<MixedValue> = (0..v.len())
                    .map(|i| {
                        if i < valid.len && valid.get(i) {
                            MixedValue::Num(v[i])
                        } else {
                            MixedValue::Null
                        }
                    })
                    .collect();
                while m.len() < dr {
                    m.push(MixedValue::Null);
                }
                m.push(MixedValue::Str(idx));
                *self = Column::Mixed(m);
            }
            Column::Mixed(m) => {
                while m.len() < dr {
                    m.push(MixedValue::Null);
                }
                m.push(MixedValue::Str(idx));
            }
            Column::Unset => unreachable!(),
        }
    }
    #[inline]
    fn push_null(&mut self, dr: usize) {
        match self {
            Column::Unset => {}
            Column::Num { v, valid } => {
                while v.len() < dr {
                    v.push(f64::NAN);
                    valid.push(false);
                }
                v.push(f64::NAN);
                valid.push(false);
            }
            Column::Str(s) => {
                while s.len() < dr {
                    s.push(NULL_IDX);
                }
                s.push(NULL_IDX);
            }
            Column::Mixed(m) => {
                while m.len() < dr {
                    m.push(MixedValue::Null);
                }
                m.push(MixedValue::Null);
            }
        }
    }
    fn pad_to(&mut self, n: usize) {
        match self {
            Column::Unset => {}
            Column::Num { v, valid } => {
                while v.len() < n {
                    v.push(f64::NAN);
                    valid.push(false);
                }
            }
            Column::Str(s) => {
                while s.len() < n {
                    s.push(NULL_IDX);
                }
            }
            Column::Mixed(m) => {
                while m.len() < n {
                    m.push(MixedValue::Null);
                }
            }
        }
    }
}

// ----------------------------------------------------------------------------
// Formula sparse column types
// ----------------------------------------------------------------------------
#[derive(Clone, Debug)]
pub enum FormulaKind {
    Plain,
    Shared { si: u32 },
    Array { r0: u32, c0: u32, r1: u32, c1: u32 },
    DataTable,
}

#[derive(Clone, Debug)]
pub struct FormulaRecord {
    /// 0-based data-row index (header excluded; sheet row = row + 2 when header present).
    pub row: u32,
    /// 0-based column index.
    pub col: u32,
    pub kind: FormulaKind,
}

#[derive(Clone)]
enum FCell {
    Plain(u32),
    /// Shared-formula dependent: the anchor is located by `si`; `rd`/`cd` are
    /// the row/column delta from the anchor already resolved at load, so
    /// translation on demand needs only the anchor text (E4).
    Shared {
        si: u32,
        rd: i32,
        cd: i32,
    },
    Array {
        r0: u32,
        c0: u32,
        r1: u32,
        c1: u32,
        text: u32,
    },
    DataTable(u32),
}
#[derive(Clone)]
struct FEntry {
    row: u32,
    col: u32,
    cell: FCell,
}
#[derive(Clone)]
struct AnchorDef {
    si: u32,
    text: u32,
    orow: u32,
    ocol: u32,
}

/// One typed Excel error cache (`t="e"`) at a data-row cell.
///
/// `row`/`col` are 0-based data indices (header excluded), matching formulas().
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellError {
    pub row: u32,
    pub col: u32,
    /// Error code from the cell's `<v>` payload (e.g. `"#DIV/0!"`).
    pub code: String,
}

/// Sparse formula column with lazy shared-formula translation.
pub struct FormulaColumn {
    entries: Vec<FEntry>,
    fdict: Dict,
    anchors: Vec<AnchorDef>,
}

/// One entry's byte span `(start, len)` inside a translated arena.
pub(crate) type ArenaSpan = (u32, u32);

/// A parallel chunk's local arena plus its entry spans, merged afterwards.
type ArenaChunk = (Vec<u8>, Vec<ArenaSpan>);

/// Value columns converted to Arrow arrays plus their metadata.
pub(crate) type ArrowColumns = (
    Vec<String>,
    Vec<arrow_array::ArrayRef>,
    Option<Vec<arrow_array::UInt32Array>>,
    Option<FormulaColumn>,
    Vec<CellError>,
    usize,
    usize,
);

impl FormulaColumn {
    /// Empty formula column (feature requested, sheet has no formulas).
    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
            fdict: Dict::new(),
            anchors: Vec::new(),
        }
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn shared_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e.cell, FCell::Shared { .. }))
            .count()
    }
    pub fn records(&self) -> Vec<FormulaRecord> {
        self.entries
            .iter()
            .map(|e| FormulaRecord {
                row: e.row,
                col: e.col,
                kind: match &e.cell {
                    FCell::Plain(_) => FormulaKind::Plain,
                    FCell::Shared { si, .. } => FormulaKind::Shared { si: *si },
                    FCell::Array { r0, c0, r1, c1, .. } => FormulaKind::Array {
                        r0: *r0,
                        c0: *c0,
                        r1: *r1,
                        c1: *c1,
                    },
                    FCell::DataTable(_) => FormulaKind::DataTable,
                },
            })
            .collect()
    }
    fn anchor_by_si(&self) -> Vec<Option<(u32, u32, u32)>> {
        let maxsi = self
            .anchors
            .iter()
            .map(|a| a.si)
            .max()
            .map(|m| m as usize + 1)
            .unwrap_or(0);
        let mut v = vec![None; maxsi];
        for a in &self.anchors {
            v[a.si as usize] = Some((a.text, a.orow, a.ocol));
        }
        v
    }
    /// Fill the stored `rd`/`cd` of every shared dependent from its anchor's
    /// coordinates. Runs once at load, after chunks are merged (anchor
    /// coordinates are final only then); it is a cheap pass over the entries,
    /// no translation.
    fn resolve_shared_deltas(&mut self) {
        let abs = self.anchor_by_si();
        for e in &mut self.entries {
            if let FCell::Shared { si, rd, cd } = &mut e.cell {
                if let Some((_, orow, ocol)) = abs.get(*si as usize).and_then(|o| *o) {
                    *rd = e.row as i32 - orow as i32;
                    *cd = e.col as i32 - ocol as i32;
                }
            }
        }
    }
    /// Position of the entry holding data-row `row`, data-col `col`.
    fn entry_index(&self, row: u32, col: u32) -> Option<usize> {
        self.entries
            .iter()
            .position(|e| e.row == row && e.col == col)
    }
    /// Append the translated text of entry `i` to `out` (no span bookkeeping).
    /// Plain / array / dataTable texts resolve straight out of the dict; a
    /// shared dependent is translated from its anchor by its stored delta.
    /// An orphan `si` (no anchor) appends nothing, matching the historical
    /// empty-string degradation.
    fn translate_into(&self, out: &mut Vec<u8>, i: usize, anchors: &[Option<(u32, u32, u32)>]) {
        let e = &self.entries[i];
        match &e.cell {
            FCell::Plain(id) => out.extend_from_slice(self.fdict.resolve(*id)),
            FCell::Array { text, .. } => out.extend_from_slice(self.fdict.resolve(*text)),
            FCell::DataTable(id) => out.extend_from_slice(self.fdict.resolve(*id)),
            FCell::Shared { si, rd, cd } => {
                if let Some((tid, _, _)) = anchors.get(*si as usize).and_then(|o| *o) {
                    let atext = std::str::from_utf8(self.fdict.resolve(tid)).unwrap_or("");
                    formula::translate_body_into(out, atext, *rd, *cd);
                }
            }
        }
    }
    /// Translate the formula at data-row/col (0-based, header excluded).
    /// Materialises an owned String on call (the historical entry point);
    /// the lazy arena-backed path lives in [`FormulaTexts`].
    pub fn translate(&self, row: u32, col: u32) -> Option<String> {
        let i = self.entry_index(row, col)?;
        let anchors = self.anchor_by_si();
        let mut out = Vec::with_capacity(32);
        self.translate_into(&mut out, i, &anchors);
        Some(String::from_utf8_lossy(&out).into_owned())
    }
    /// Lazily-hydrated view over this column. Returns a handle that owns the
    /// translation arena, so a caller can read a few formulas (or all of them)
    /// with each formula translated at most once and no per-formula allocation.
    pub fn lazy(&self) -> FormulaTexts<'_> {
        FormulaTexts {
            col: self,
            anchors: self.anchor_by_si(),
            bytes: Vec::new(),
            spans: vec![None; self.entries.len()],
            translated: 0,
        }
    }
    /// Materialize every formula string (rayon-parallel, arena-backed).
    pub fn materialize_all(&self) -> Vec<String> {
        use rayon::prelude::*;
        let (bytes, spans) = self.build_arena_all();
        spans
            .into_par_iter()
            .map(|(s, l)| {
                String::from_utf8_lossy(&bytes[s as usize..(s + l) as usize]).into_owned()
            })
            .collect()
    }

    /// Build one contiguous translation arena holding every entry's text,
    /// returning it alongside each entry's `(start, len)` span.
    ///
    /// Parallel over entry chunks, each with its own local arena, merged at
    /// the end by re-basing spans — no mutex on the hot path, no per-formula
    /// allocation (E4 candidate C).
    fn build_arena_all(&self) -> (Vec<u8>, Vec<ArenaSpan>) {
        use rayon::prelude::*;
        let n = self.entries.len();
        let anchors = self.anchor_by_si();
        const CHUNK: usize = 4096;
        let nchunks = n.div_ceil(CHUNK);
        let locals: Vec<ArenaChunk> = (0..nchunks)
            .into_par_iter()
            .map(|k| {
                let lo = k * CHUNK;
                let hi = lo + CHUNK;
                let hi = hi.min(n);
                let mut bytes = Vec::with_capacity((hi - lo) * 12);
                let mut spans = Vec::with_capacity(hi - lo);
                for i in lo..hi {
                    let start = bytes.len() as u32;
                    self.translate_into(&mut bytes, i, &anchors);
                    spans.push((start, bytes.len() as u32 - start));
                }
                (bytes, spans)
            })
            .collect();
        let mut all_bytes = Vec::new();
        let mut all_spans = Vec::with_capacity(n);
        let mut base: u32 = 0;
        for (bytes, spans) in locals {
            for (s, l) in spans {
                all_spans.push((s + base, l));
            }
            all_bytes.extend_from_slice(&bytes);
            base += bytes.len() as u32;
        }
        (all_bytes, all_spans)
    }

    /// One-pass export rows: (row, col, kind tag, text, array ref A1).
    /// Avoids a second `records()` allocation over the same entries.
    pub fn materialize_export_rows(&self) -> Vec<(u32, u32, &'static str, String, Option<String>)> {
        use rayon::prelude::*;
        let (bytes, spans) = self.build_arena_all();
        self.entries
            .par_iter()
            .enumerate()
            .map(|(i, e)| {
                let kind = match &e.cell {
                    FCell::Plain(_) => "plain",
                    FCell::Shared { .. } => "shared",
                    FCell::Array { .. } => "array",
                    FCell::DataTable(_) => "dataTable",
                };
                let (s, l) = spans[i];
                let text =
                    String::from_utf8_lossy(&bytes[s as usize..(s + l) as usize]).into_owned();
                let ref_a1 = match &e.cell {
                    FCell::Array { r0, c0, r1, c1, .. } => {
                        let range = CellRange {
                            r0: *r0,
                            c0: *c0,
                            r1: *r1,
                            c1: *c1,
                        };
                        Some(super::range_a1(&range))
                    }
                    _ => None,
                };
                (e.row, e.col, kind, text, ref_a1)
            })
            .collect()
    }
}

/// Lazily-hydrated view over a [`FormulaColumn`] (E4 decision D, with C as the
/// materialisation strategy).
///
/// Holds one shared translation arena: each formula is translated into it on
/// first access and its `(start, len)` span is remembered, so a later read of
/// the same formula borrows the already-produced bytes instead of translating
/// again. A caller that reads everything pays candidate C's single-arena cost
/// (no per-formula allocation) rather than candidate A's String per formula.
///
/// Thread-safety: this handle is not `Sync` — a caller sharing it across
/// threads should instead use [`FormulaColumn::materialize_all`] /
/// [`FormulaColumn::materialize_export_rows`], which build per-chunk arenas in
/// rayon and merge them at the end.
pub struct FormulaTexts<'a> {
    col: &'a FormulaColumn,
    anchors: Vec<Option<(u32, u32, u32)>>,
    bytes: Vec<u8>,
    spans: Vec<Option<(u32, u32)>>,
    translated: usize,
}

impl<'a> FormulaTexts<'a> {
    /// Number of formulas translated so far (i.e. distinct first accesses).
    /// Reading a formula twice does not increment this.
    pub fn translated(&self) -> usize {
        self.translated
    }
    pub fn len(&self) -> usize {
        self.spans.len()
    }
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }
    /// Translate the formula at data-row/col (0-based, header excluded) into
    /// the shared arena and return its text as a borrow of the arena. The same
    /// formula read twice returns the same text without re-translating.
    pub fn text(&mut self, row: u32, col: u32) -> Option<&str> {
        let i = self.col.entry_index(row, col)?;
        Some(self.text_at(i))
    }
    /// Translate entry `i` (index into `len()`) on first access.
    pub fn text_at(&mut self, i: usize) -> &str {
        if self.spans[i].is_none() {
            let start = self.bytes.len() as u32;
            self.col.translate_into(&mut self.bytes, i, &self.anchors);
            let len = self.bytes.len() as u32 - start;
            self.spans[i] = Some((start, len));
            self.translated += 1;
        }
        let (s, l) = self.spans[i].expect("span filled above");
        std::str::from_utf8(&self.bytes[s as usize..(s + l) as usize]).unwrap_or("")
    }
    /// Translate every formula into the arena (idempotent; only first accesses
    /// do work). After this, every span is filled.
    pub fn hydrate_all(&mut self) {
        for i in 0..self.spans.len() {
            if self.spans[i].is_none() {
                let start = self.bytes.len() as u32;
                self.col.translate_into(&mut self.bytes, i, &self.anchors);
                let len = self.bytes.len() as u32 - start;
                self.spans[i] = Some((start, len));
                self.translated += 1;
            }
        }
    }
    /// `(start, len)` into [`FormulaTexts::bytes`] for entry `i` (filled by an
    /// earlier `text_at`/`hydrate_all`).
    pub fn byte_span(&self, i: usize) -> (usize, usize) {
        let (s, l) = self.spans[i].expect("entry not yet hydrated");
        (s as usize, l as usize)
    }
    /// The shared translation arena bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ScanFeat {
    pub styles: bool,
    pub formulas: bool,
    /// Capture sparse row dimensions from `<row>` attrs (Stream A; zero extra pass).
    pub row_meta: bool,
}

pub(crate) struct Partial {
    pub header: Vec<Vec<u8>>,
    cols: Vec<Column>,
    dict: Dict,
    pub nrows: usize,
    pub ncols: usize,
    /// Absolute 0-based data-row index of local row 0 (header excluded).
    /// Enables correct merge across rayon chunks when sheet row `@r` is honored.
    pub abs_start: usize,
    pub style_cols: Vec<Vec<u32>>,
    fentries: Vec<FEntry>,
    fdict: Dict,
    anchors: Vec<AnchorDef>,
    /// Sparse `t="e"` error caches (always collected on the value path).
    pub cell_errors: Vec<CellError>,
    /// Sparse row dimensions (only when `ScanFeat.row_meta`).
    pub row_dims: Vec<super::meta::RowDim>,
}

impl Partial {
    pub fn take_formula_column(&mut self) -> FormulaColumn {
        let mut col = FormulaColumn {
            entries: std::mem::take(&mut self.fentries),
            fdict: std::mem::replace(&mut self.fdict, Dict::new()),
            anchors: std::mem::take(&mut self.anchors),
        };
        // E4: resolve each shared dependent's (row, col) delta from its anchor
        // once, at load, so on-demand translation only touches the anchor text.
        col.resolve_shared_deltas();
        col
    }
    /// Convert value columns to Arrow arrays; consumes column buffers.
    pub fn into_arrow_columns(mut self) -> TurboResult<ArrowColumns> {
        use arrow_array::builder::{Float64Builder, StringBuilder};
        use arrow_array::types::Int32Type;
        use arrow_array::{ArrayRef, Float64Array, UInt32Array};
        use std::sync::Arc;

        let nrows = self.nrows;
        let ncols = self.ncols;
        let header: Vec<String> = (0..ncols)
            .map(|c| {
                self.header
                    .get(c)
                    .map(|h| String::from_utf8_lossy(h).into_owned())
                    .unwrap_or_default()
            })
            .collect();

        let style_cols = if self.style_cols.is_empty() {
            None
        } else {
            let mut out = Vec::with_capacity(ncols);
            for c in 0..ncols {
                let mut sc = if c < self.style_cols.len() {
                    std::mem::take(&mut self.style_cols[c])
                } else {
                    Vec::new()
                };
                while sc.len() < nrows {
                    sc.push(0);
                }
                out.push(UInt32Array::from(sc));
            }
            Some(out)
        };

        let formulas = if self.fentries.is_empty() && self.anchors.is_empty() {
            None
        } else {
            Some(self.take_formula_column())
        };

        let cell_errors = std::mem::take(&mut self.cell_errors);

        // One shared dictionary values array for all Str columns on this sheet.
        // Column keys index into Partial.dict (sheet-local intern pool); cloning
        // the Arc is a refcount bump — not a full copy of the string pool.
        let dict_values: ArrayRef = Arc::new(arrow_array::StringArray::from(self.dict.strings()));
        let mut columns: Vec<ArrayRef> = Vec::with_capacity(ncols);

        for c in 0..ncols {
            let col = if c < self.cols.len() {
                std::mem::replace(&mut self.cols[c], Column::Unset)
            } else {
                Column::Unset
            };
            match col {
                Column::Num { v, valid } => {
                    let mut b = Float64Builder::with_capacity(nrows);
                    for (i, &val) in v.iter().take(nrows).enumerate() {
                        if valid.get(i) {
                            b.append_value(val);
                        } else {
                            b.append_null();
                        }
                    }
                    for _ in v.len().min(nrows)..nrows {
                        b.append_null();
                    }
                    columns.push(Arc::new(b.finish()) as ArrayRef);
                }
                Column::Str(ids) => {
                    let keys: Vec<Option<i32>> = (0..nrows)
                        .map(|i| {
                            let id = ids.get(i).copied().unwrap_or(NULL_IDX);
                            if id == NULL_IDX {
                                None
                            } else {
                                Some(id as i32)
                            }
                        })
                        .collect();
                    let key_arr = arrow_array::Int32Array::from(keys);
                    let dict = arrow_array::DictionaryArray::<Int32Type>::try_new(
                        key_arr,
                        Arc::clone(&dict_values),
                    )
                    .map_err(|e| TurboError::Arrow(e.to_string()))?;
                    columns.push(Arc::new(dict) as ArrayRef);
                }
                Column::Mixed(m) => {
                    let mut sb = StringBuilder::with_capacity(nrows, nrows * 16);
                    for i in 0..nrows {
                        match m.get(i) {
                            Some(MixedValue::Null) | None => sb.append_null(),
                            Some(MixedValue::Num(x)) => {
                                let mut ryu_buf = ryu::Buffer::new();
                                sb.append_value(ryu_buf.format(*x));
                            }
                            Some(MixedValue::Str(id)) => {
                                if *id == NULL_IDX {
                                    sb.append_null();
                                } else {
                                    // Resolve straight out of the intern pool.
                                    //
                                    // This used to call `self.dict.strings()`, which
                                    // rebuilds the ENTIRE pool into a fresh
                                    // `Vec<String>` — allocating every distinct string
                                    // — just to index one element and drop it. Inside
                                    // this per-cell loop that is O(cells x distinct)
                                    // with an allocation per string per cell, and it
                                    // made mixed-type columns quadratic: a 50k-row
                                    // sheet took 118 s against 0.065 s for the same
                                    // sheet with homogeneous columns.
                                    //
                                    // `try_resolve` is an offset lookup, and
                                    // `from_utf8_lossy` borrows when the bytes are
                                    // valid UTF-8, which interned XML text always is.
                                    match self.dict.try_resolve(*id) {
                                        Some(b) => sb.append_value(String::from_utf8_lossy(b)),
                                        // Out of range cannot happen for ids produced
                                        // by `intern`, but append SOMETHING regardless:
                                        // the old code appended neither value nor null
                                        // here, which would silently leave this column
                                        // shorter than `nrows` and misalign the batch.
                                        None => sb.append_null(),
                                    }
                                }
                            }
                        }
                    }
                    columns.push(Arc::new(sb.finish()) as ArrayRef);
                }
                Column::Unset => {
                    let arr = Float64Array::from(vec![None::<f64>; nrows]);
                    columns.push(Arc::new(arr) as ArrayRef);
                }
            }
        }

        Ok((
            header,
            columns,
            style_cols,
            formulas,
            cell_errors,
            nrows,
            ncols,
        ))
    }
}

#[derive(Clone, Copy, PartialEq)]
enum CellType {
    Num,
    Inline,
    Shared,
    StrVal,
    Bool,
    Err,
}

// ----------------------------------------------------------------------------
// Cell / row @r helpers (honor sparse coordinates; sequential when absent)
// ----------------------------------------------------------------------------

/// Parse column letters from a cell ref payload starting at the first letter
/// (e.g. `A1...` or `BC12...`) → 0-based column index.
#[inline]
fn col_from_ref_bytes(bytes: &[u8]) -> Option<usize> {
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    let col1 = formula::letters_to_index(&bytes[..i])?;
    Some((col1 - 1) as usize)
}

/// Parse `r="A1"` (or ` r="A1"`) from a `<c ...>` attribute blob → 0-based col.
/// Fast path: when refs are contiguous the caller still just uses this col, which
/// equals the sequential counter — no extra gap logic.
#[inline]
fn parse_cell_col_from_r(tag: &[u8]) -> Option<usize> {
    // Prefer ` r="` (normal OOXML); also accept leading `r="` at start of tag.
    let vs = if let Some(o) = memchr::memmem::find(tag, b" r=\"") {
        o + 4
    } else if tag.starts_with(b"r=\"") {
        3
    } else {
        let o = memchr::memmem::find(tag, b"r=\"")?;
        // Guard: attribute name must be exactly `r`, not `xr` / `pr` etc.
        if o > 0 {
            let p = tag[o - 1];
            if p.is_ascii_alphanumeric() || p == b'_' {
                return None;
            }
        }
        o + 3
    };
    col_from_ref_bytes(&tag[vs..])
}

/// Parse `r="12"` from a `<row ...>` open tag → 1-based sheet row number.
#[inline]
fn parse_row_r_attr(row_tag: &[u8]) -> Option<u32> {
    let vs = if let Some(o) = memchr::memmem::find(row_tag, b" r=\"") {
        o + 4
    } else if row_tag.starts_with(b"r=\"") {
        3
    } else {
        let o = memchr::memmem::find(row_tag, b"r=\"")?;
        if o > 0 {
            let p = row_tag[o - 1];
            if p.is_ascii_alphanumeric() || p == b'_' {
                return None;
            }
        }
        o + 3
    };
    let ve = vs + memchr::memchr(b'"', &row_tag[vs..]).unwrap_or(row_tag.len() - vs);
    atoi(&row_tag[vs..ve])
}

/// Sheet row (1-based) → 0-based data-row index under the turbo convention that
/// spreadsheet row 1 is the header (when the first chunk consumes a header).
#[inline]
fn sheet_row_to_data_row(sheet_row: u32) -> usize {
    (sheet_row as usize).saturating_sub(2)
}

// ----------------------------------------------------------------------------
// Core scanner
// ----------------------------------------------------------------------------
/// The fixed schema a streaming window must emit for one column (B6).
///
/// The eager path infers a column's type from every row before building any
/// Arrow array. A streaming reader cannot see the whole sheet before emitting
/// the first batch, so it runs a type-only pre-pass over the sheet and then
/// seeds every window's columns with these targets — making every batch's
/// schema identical AND equal to what the eager path produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ColTarget {
    /// All-null or numeric-only column → Float64 with nulls (eager `Column::Unset`/`Num`).
    Num,
    /// String-only column → Dictionary<Int32, Utf8> (eager `Column::Str`).
    Str,
    /// Mixed numeric + string column → Utf8 via ryu + pool resolution (eager `Column::Mixed`).
    Mixed,
}

impl ColTarget {
    fn seed(self) -> Column {
        match self {
            ColTarget::Num => Column::Num {
                v: Vec::new(),
                valid: BitVec::new(),
            },
            ColTarget::Str => Column::Str(Vec::new()),
            ColTarget::Mixed => Column::Mixed(Vec::new()),
        }
    }
}

/// Parse one row-aligned byte region of a worksheet (streaming path uses this
/// per window; `parse_parallel` chunks a full sheet into these regions).
#[allow(clippy::too_many_arguments)]
pub(crate) fn parse_region(
    x: &[u8],
    lo: usize,
    hi: usize,
    has_header: bool,
    shared: Option<&StringArena>,
    feat: ScanFeat,
    // Base data-row index for the sequential (no `@r`) fallback. 0 for the
    // eager path and the first streaming window; the count of data rows
    // already emitted for later streaming windows, so sparse coordinates stay
    // globally absolute across windows.
    row_offset: usize,
    // Fixed per-column schema from the streaming pre-pass. `None` keeps the
    // eager inferred behavior; `Some` seeds every column (and pre-sizes the
    // column set to the target count) so a window emits the pre-pass schema.
    targets: Option<&[ColTarget]>,
) -> Partial {
    let mut cols: Vec<Column> = if let Some(t) = targets {
        t.iter().map(|&t| t.seed()).collect()
    } else {
        Vec::new()
    };
    let mut style_cols: Vec<Vec<u32>> = Vec::new();
    let mut header: Vec<Vec<u8>> = Vec::new();
    let mut dict = Dict::new();
    let mut fdict = Dict::new();
    let mut fentries: Vec<FEntry> = Vec::new();
    let mut anchors: Vec<AnchorDef> = Vec::new();
    let mut cell_errors: Vec<CellError> = Vec::new();
    let mut row_dims: Vec<super::meta::RowDim> = Vec::new();
    let mut scratch: Vec<u8> = Vec::new();
    let mut fscratch: Vec<u8> = Vec::new();
    let mut ncols = if let Some(t) = targets {
        t.len()
    } else {
        0usize
    };

    // Local data-row cursor (0-based within this partial). Absolute data-row =
    // abs_start + dr. abs_start is fixed from the first data row's sheet `@r`
    // (or 0 when `@r` is absent — sequential fallback).
    let mut dr = 0usize;
    let mut abs_start = 0usize;
    let mut abs_start_set = false;
    let mut first_row = true;
    let mut i = lo;

    /// Advance local `dr` to the absolute data-row for this sheet row, padding
    /// gaps. Returns the absolute data-row index used for formula/error coords.
    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn align_data_row(
        sheet_row: Option<u32>,
        dr: &mut usize,
        abs_start: &mut usize,
        abs_start_set: &mut bool,
        cols: &mut [Column],
        style_cols: &mut [Vec<u32>],
        feat: ScanFeat,
        row_offset: usize,
    ) -> u32 {
        let abs = if let Some(sr) = sheet_row {
            let a = sheet_row_to_data_row(sr);
            if !*abs_start_set {
                *abs_start = a;
                *abs_start_set = true;
            }
            a
        } else {
            // No row @r: sequential packing from abs_start. The streaming path
            // passes the global data-row count already emitted as `row_offset`
            // so sparse coordinates stay absolute across windows; eager passes 0.
            if !*abs_start_set {
                *abs_start = row_offset;
                *abs_start_set = true;
            }
            *abs_start + *dr
        };
        let target_local = abs.saturating_sub(*abs_start);
        if target_local > *dr {
            for c in cols.iter_mut() {
                c.pad_to(target_local);
            }
            if feat.styles {
                for sc in style_cols.iter_mut() {
                    while sc.len() < target_local {
                        sc.push(0);
                    }
                }
            }
            *dr = target_local;
        } else if target_local < *dr {
            // Out-of-order rows (rare): snap back so we write the right local slot.
            *dr = target_local;
        }
        abs as u32
    }

    while i < hi {
        let roff = match memchr::memmem::find(&x[i..hi], b"<row") {
            Some(o) => i + o,
            None => break,
        };
        // Ensure it's <row ...> not <rows
        let after_row = x.get(roff + 4).copied().unwrap_or(b'>');
        if !(after_row == b' ' || after_row == b'>' || after_row == b'/') {
            i = roff + 4;
            continue;
        }
        let rtag_end = roff + memchr::memchr(b'>', &x[roff..hi]).unwrap_or(hi - roff);
        if rtag_end <= roff + 1 || rtag_end > x.len() {
            break; // truncated <row open tag
        }
        let row_self_closing = x.get(rtag_end.saturating_sub(1)) == Some(&b'/');
        let row_tag = &x[roff + 4..rtag_end];
        let sheet_row = parse_row_r_attr(row_tag);
        let is_header = has_header && first_row;
        first_row = false;

        // Stream A: capture interesting row attrs once per `<row>` (no per-cell alloc).
        if feat.row_meta {
            if let Some(rd) = super::meta::parse_row_dim(row_tag, sheet_row) {
                row_dims.push(rd);
            }
        }

        if row_self_closing {
            if !is_header {
                let _abs = align_data_row(
                    sheet_row,
                    &mut dr,
                    &mut abs_start,
                    &mut abs_start_set,
                    &mut cols,
                    &mut style_cols,
                    feat,
                    row_offset,
                );
                for c in cols.iter_mut() {
                    c.pad_to(dr + 1);
                }
                if feat.styles {
                    for sc in style_cols.iter_mut() {
                        while sc.len() < dr + 1 {
                            sc.push(0);
                        }
                    }
                }
                dr += 1;
            }
            i = rtag_end + 1;
            continue;
        }

        let row_close_rel =
            memchr::memmem::find(&x[rtag_end..hi], b"</row>").unwrap_or(hi - rtag_end);
        let cells_end = rtag_end + row_close_rel;

        // For data rows, align local dr to sheet @r before reading cells.
        let abs_row: u32 = if !is_header {
            align_data_row(
                sheet_row,
                &mut dr,
                &mut abs_start,
                &mut abs_start_set,
                &mut cols,
                &mut style_cols,
                feat,
                row_offset,
            )
        } else {
            0
        };

        let mut ci = rtag_end + 1;
        // Sequential column cursor; overridden by cell @r when present.
        let mut next_col = 0usize;
        while ci < cells_end {
            let coff = match memchr::memmem::find(&x[ci..cells_end], b"<c") {
                Some(o) => ci + o,
                None => break,
            };
            let after = x.get(coff + 2).copied().unwrap_or(0);
            if !(after == b' ' || after == b'>' || after == b'/') {
                ci = coff + 2;
                continue;
            }
            let ctag_end = coff + memchr::memchr(b'>', &x[coff..hi]).unwrap_or(hi - coff);
            if ctag_end <= coff + 1 || ctag_end > x.len() {
                break; // truncated cell open tag
            }
            let self_closing = x.get(ctag_end.saturating_sub(1)) == Some(&b'/');
            let tag = &x[coff + 2..ctag_end];

            // Honor @r when present (sparse rows); else sequential XML order.
            let col = if let Some(c) = parse_cell_col_from_r(tag) {
                c
            } else {
                next_col
            };
            next_col = col + 1;

            let ctype = match memchr::memmem::find(tag, b" t=\"") {
                Some(o) => {
                    let vs = o + 4;
                    let ve = vs + memchr::memchr(b'"', &tag[vs..]).unwrap_or(tag.len() - vs);
                    match &tag[vs..ve] {
                        b"inlineStr" => CellType::Inline,
                        b"s" => CellType::Shared,
                        b"str" => CellType::StrVal,
                        b"b" => CellType::Bool,
                        b"e" => CellType::Err,
                        _ => CellType::Num,
                    }
                }
                None => CellType::Num,
            };

            let sidx: u32 = if feat.styles {
                match memchr::memmem::find(tag, b" s=\"") {
                    Some(o) => {
                        let vs = o + 4;
                        let ve = vs + memchr::memchr(b'"', &tag[vs..]).unwrap_or(tag.len() - vs);
                        atoi(&tag[vs..ve]).unwrap_or(0)
                    }
                    None => 0,
                }
            } else {
                0
            };

            if col >= cols.len() {
                cols.resize_with(col + 1, || {
                    targets
                        .and_then(|t| t.get(col).copied())
                        .map(ColTarget::seed)
                        .unwrap_or(Column::Unset)
                });
            }
            if feat.styles && col >= style_cols.len() {
                style_cols.resize_with(col + 1, Vec::new);
            }

            if !is_header && feat.styles {
                let sc = &mut style_cols[col];
                while sc.len() < dr {
                    sc.push(0);
                }
                // Sparse mid-row: may write style for col C before A is touched.
                if sc.len() == dr {
                    sc.push(sidx);
                } else if sc.len() > dr {
                    sc[dr] = sidx;
                } else {
                    sc.push(sidx);
                }
            }

            if self_closing {
                if !is_header {
                    cols[col].push_null(dr);
                }
                if col + 1 > ncols {
                    ncols = col + 1;
                }
                ci = ctag_end + 1;
                continue;
            }

            let cclose_rel = if ctag_end < hi {
                memchr::memmem::find(&x[ctag_end..hi], b"</c>").unwrap_or(hi - ctag_end)
            } else {
                0
            };
            let cend = (ctag_end + cclose_rel).min(x.len());
            if ctag_end + 1 > cend {
                ci = ctag_end + 1;
                continue;
            }
            let content = &x[ctag_end + 1..cend];

            if feat.formulas && !is_header {
                if let Some(fo) = memchr::memmem::find(content, b"<f") {
                    let fafter = content.get(fo + 2).copied().unwrap_or(b'>');
                    if fafter == b' ' || fafter == b'>' || fafter == b'/' {
                        let ftag_end =
                            fo + memchr::memchr(b'>', &content[fo..]).unwrap_or(content.len() - fo);
                        if ftag_end <= fo + 1 || ftag_end > content.len() {
                            // truncated <f ...
                        } else {
                            let ftag = &content[fo + 2..ftag_end];
                            let f_self_closing =
                                content.get(ftag_end.saturating_sub(1)) == Some(&b'/');
                            let fattr = |name: &[u8]| -> Option<&[u8]> {
                                let mut pat = Vec::with_capacity(name.len() + 3);
                                pat.push(b' ');
                                pat.extend_from_slice(name);
                                pat.extend_from_slice(b"=\"");
                                let p = memchr::memmem::find(ftag, &pat)?;
                                let vs = p + pat.len();
                                let ve = vs + memchr::memchr(b'"', &ftag[vs..])?;
                                Some(&ftag[vs..ve])
                            };
                            let ftype = fattr(b"t");
                            let ftext: &[u8] = if f_self_closing {
                                b""
                            } else {
                                let te = memchr::memmem::find(&content[ftag_end..], b"</f>")
                                    .map(|o| ftag_end + o)
                                    .unwrap_or(content.len());
                                &content[ftag_end + 1..te]
                            };
                            let decoded = decode(ftext, &mut fscratch).to_vec();
                            match ftype {
                                None => {
                                    let id = fdict.intern(&decoded);
                                    fentries.push(FEntry {
                                        row: abs_row,
                                        col: col as u32,
                                        cell: FCell::Plain(id),
                                    });
                                }
                                Some(b"shared") => {
                                    let si = fattr(b"si").and_then(atoi).unwrap_or(0);
                                    if fattr(b"ref").is_some() {
                                        let id = fdict.intern(&decoded);
                                        anchors.push(AnchorDef {
                                            si,
                                            text: id,
                                            orow: abs_row,
                                            ocol: col as u32,
                                        });
                                    }
                                    fentries.push(FEntry {
                                        row: abs_row,
                                        col: col as u32,
                                        // Deltas are resolved once, after chunks
                                        // merge (see resolve_shared_deltas).
                                        cell: FCell::Shared { si, rd: 0, cd: 0 },
                                    });
                                }
                                Some(b"array") => {
                                    let (r0, c0, r1, c1) =
                                        fattr(b"ref").map(parse_ref_range).unwrap_or((0, 0, 0, 0));
                                    let id = fdict.intern(&decoded);
                                    fentries.push(FEntry {
                                        row: abs_row,
                                        col: col as u32,
                                        cell: FCell::Array {
                                            r0,
                                            c0,
                                            r1,
                                            c1,
                                            text: id,
                                        },
                                    });
                                }
                                Some(b"dataTable") => {
                                    let id = fdict.intern(ftag);
                                    fentries.push(FEntry {
                                        row: abs_row,
                                        col: col as u32,
                                        cell: FCell::DataTable(id),
                                    });
                                }
                                Some(_) => {
                                    let id = fdict.intern(&decoded);
                                    fentries.push(FEntry {
                                        row: abs_row,
                                        col: col as u32,
                                        cell: FCell::Plain(id),
                                    });
                                }
                            }
                        } // end non-truncated f tag
                    }
                }
            }

            match ctype {
                CellType::Shared => {
                    let idx = if let Some(vo) = memchr::memmem::find(content, b"<v>") {
                        let vs = vo + 3;
                        let ve = memchr::memmem::find(&content[vs..], b"</v>")
                            .map(|o| vs + o)
                            .unwrap_or(content.len());
                        atoi(&content[vs..ve])
                    } else {
                        None
                    };
                    // OOB shared-string index → null cell (never panic).
                    let resolved: Option<&[u8]> = match (idx, shared) {
                        (Some(i), Some(a)) => a.try_resolve(i),
                        _ => None,
                    };
                    if is_header {
                        if col >= header.len() {
                            header.resize_with(col + 1, Vec::new);
                        }
                        header[col] = resolved.map(|s| s.to_vec()).unwrap_or_default();
                    } else {
                        match resolved {
                            Some(s) => {
                                let id = dict.intern(s);
                                cols[col].push_str(dr, id);
                            }
                            None => cols[col].push_null(dr),
                        }
                    }
                }
                CellType::Inline | CellType::StrVal | CellType::Err => {
                    let text: &[u8] = if ctype == CellType::Inline {
                        if let Some(to) = memchr::memmem::find(content, b"<t") {
                            let topen_end = to
                                + memchr::memchr(b'>', &content[to..])
                                    .unwrap_or(content.len() - to);
                            if topen_end == 0
                                || content.get(topen_end.saturating_sub(1)) == Some(&b'/')
                                || topen_end >= content.len()
                            {
                                b""
                            } else {
                                let te = memchr::memmem::find(&content[topen_end..], b"</t>")
                                    .map(|o| topen_end + o)
                                    .unwrap_or(content.len());
                                if topen_end < te {
                                    &content[topen_end + 1..te]
                                } else {
                                    b""
                                }
                            }
                        } else {
                            b""
                        }
                    } else if let Some(vo) = memchr::memmem::find(content, b"<v>") {
                        let vs = vo + 3;
                        let ve = memchr::memmem::find(&content[vs..], b"</v>")
                            .map(|o| vs + o)
                            .unwrap_or(content.len());
                        &content[vs..ve]
                    } else {
                        b""
                    };
                    let decoded = decode(text, &mut scratch);
                    if is_header {
                        if col >= header.len() {
                            header.resize_with(col + 1, Vec::new);
                        }
                        header[col] = decoded.to_vec();
                    } else {
                        // Typed error caches: always record sparsely. Value columns may
                        // still show the code when the column is string-typed, or null
                        // when mixed into a numeric column (push_str → null on Num).
                        if ctype == CellType::Err {
                            cell_errors.push(CellError {
                                row: abs_row,
                                col: col as u32,
                                code: String::from_utf8_lossy(decoded).into_owned(),
                            });
                        }
                        let id = dict.intern(decoded);
                        cols[col].push_str(dr, id);
                    }
                }
                CellType::Num | CellType::Bool => {
                    let val = if let Some(vo) = memchr::memmem::find(content, b"<v>") {
                        let vs = vo + 3;
                        let ve = memchr::memmem::find(&content[vs..], b"</v>")
                            .map(|o| vs + o)
                            .unwrap_or(content.len());
                        let raw = &content[vs..ve];
                        if raw.is_empty() {
                            None
                        } else {
                            fast_float2::parse::<f64, _>(raw).ok()
                        }
                    } else {
                        None
                    };
                    if is_header {
                        if col >= header.len() {
                            header.resize_with(col + 1, Vec::new);
                        }
                        header[col] = val.map(|v| v.to_string().into_bytes()).unwrap_or_default();
                    } else {
                        match val {
                            Some(v) => cols[col].push_num(dr, v),
                            None => cols[col].push_null(dr),
                        }
                    }
                }
            }

            if col + 1 > ncols {
                ncols = col + 1;
            }
            ci = cend + 4;
        }

        if header.len() > ncols {
            ncols = header.len();
        }
        if !is_header {
            for c in cols.iter_mut() {
                c.pad_to(dr + 1);
            }
            if feat.styles {
                for sc in style_cols.iter_mut() {
                    while sc.len() < dr + 1 {
                        sc.push(0);
                    }
                }
            }
            dr += 1;
        }
        i = cells_end + 6;
    }

    for c in cols.iter_mut() {
        c.pad_to(dr);
    }
    if feat.styles {
        for sc in style_cols.iter_mut() {
            while sc.len() < dr {
                sc.push(0);
            }
        }
    }
    // Formula/error rows are already absolute; remap to local for storage only when
    // abs_start > 0 so merge can re-base. Keep absolute in entries (merge uses them
    // as absolute when abs_start is set on the partial).
    Partial {
        header,
        cols,
        dict,
        nrows: dr,
        ncols,
        abs_start: if abs_start_set { abs_start } else { 0 },
        style_cols,
        fentries,
        fdict,
        anchors,
        cell_errors,
        row_dims,
    }
}

/// Parse "C2:D5" (or "C2") -> (r0,c0,r1,c1); rows/cols 0-based sheet coordinates.
pub(crate) fn parse_ref_range(refr: &[u8]) -> (u32, u32, u32, u32) {
    let s = std::str::from_utf8(refr).unwrap_or("");
    let parse_one = |a: &str| -> (u32, u32) {
        let bytes = a.as_bytes();
        let mut i = 0;
        while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
        let col = formula::letters_to_index(&bytes[..i]).unwrap_or(1) - 1;
        let row: u32 = a[i..].parse().unwrap_or(1);
        (row.saturating_sub(1), col)
    };
    if let Some((a, b)) = s.split_once(':') {
        let (r0, c0) = parse_one(a);
        let (r1, c1) = parse_one(b);
        (r0, c0, r1, c1)
    } else {
        let (r0, c0) = parse_one(s);
        (r0, c0, r0, c0)
    }
}

/// Excel grid limits for strict A1 validation.
#[allow(dead_code)] // used by the `python` feature bindings
pub(crate) const MAX_GRID_ROWS: u32 = 1_048_576;
#[allow(dead_code)] // used by the `python` feature bindings
pub(crate) const MAX_GRID_COLS: u32 = 16_384;

/// Strictly parse a single A1 cell ref or an A1 range into normalized 1-based
/// inclusive corners `(r1, c1, r2, c2)` (reversed corners are sorted).
///
/// Grammar (ASCII only): `[$]?COL[$]?ROW[:[$]?COL[$]?ROW]?` where COL is 1..=3
/// letters mapping to `A..XFD` and ROW is 1..=1_048_576. `$` markers are
/// optional and allowed only immediately before the column letters and/or the
/// row digits. Returns `None` for empty, malformed, zero, non-ASCII, or
/// out-of-grid input (including extra colons or trailing characters).
#[allow(dead_code)] // used by the `python` feature bindings
pub(crate) fn parse_ref_range_strict(refr: &[u8]) -> Option<(u32, u32, u32, u32)> {
    let mut colon_at: Option<usize> = None;
    for (i, &b) in refr.iter().enumerate() {
        if b == b':' {
            if colon_at.is_some() {
                return None; // more than one colon
            }
            colon_at = Some(i);
        }
    }
    let (a, b) = match colon_at {
        Some(i) => (&refr[..i], &refr[i + 1..]),
        None => (refr, &[][..]),
    };
    let (r1, c1) = parse_ref_component(a)?;
    let (r2, c2) = if colon_at.is_some() {
        parse_ref_component(b)?
    } else {
        (r1, c1)
    };
    Some((r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)))
}

/// Parse one `[$]?COL[$]?ROW` endpoint → 1-based `(row, col)`; `None` on any
/// syntax or bounds violation.
#[allow(dead_code)] // used by `parse_ref_range_strict` (python feature bindings)
fn parse_ref_component(s: &[u8]) -> Option<(u32, u32)> {
    if s.is_empty() {
        return None;
    }
    let n = s.len();
    let mut i = 0;
    // Optional column absolute marker.
    if s[i] == b'$' {
        i += 1;
        if i == n {
            return None;
        }
    }
    let lstart = i;
    while i < n && s[i].is_ascii_alphabetic() {
        i += 1;
    }
    let letters = &s[lstart..i];
    if letters.is_empty() || letters.len() > 3 {
        return None;
    }
    // Optional row absolute marker.
    if i < n && s[i] == b'$' {
        i += 1;
    }
    let dstart = i;
    while i < n && s[i].is_ascii_digit() {
        i += 1;
    }
    if i != n || dstart == i {
        // Trailing garbage or missing row digits.
        return None;
    }
    let col1 = formula::letters_to_index(letters)?;
    if col1 == 0 || col1 > MAX_GRID_COLS {
        return None;
    }
    let row1: u32 = std::str::from_utf8(&s[dstart..i]).ok()?.parse().ok()?;
    if row1 == 0 || row1 > MAX_GRID_ROWS {
        return None;
    }
    Some((row1, col1))
}

pub(crate) fn sheet_data_region(x: &[u8]) -> TurboResult<(usize, usize)> {
    let sd = memchr::memmem::find(x, b"<sheetData")
        .ok_or_else(|| TurboError::Format("missing <sheetData> in worksheet".into()))?;
    let gt = memchr::memchr(b'>', &x[sd..])
        .ok_or_else(|| TurboError::Format("unclosed <sheetData".into()))?;
    let start = sd + gt + 1;
    // Self-closing <sheetData/>
    if x[sd + gt - 1] == b'/' {
        return Ok((start, start));
    }
    let end = memchr::memmem::find(&x[start..], b"</sheetData>")
        .map(|o| start + o)
        .unwrap_or(x.len());
    Ok((start, end))
}

pub(crate) fn parse_parallel(
    xml: &[u8],
    nchunks: usize,
    shared: Option<&StringArena>,
    feat: ScanFeat,
) -> TurboResult<Partial> {
    use rayon::prelude::*;
    let (s, e) = sheet_data_region(xml)?;
    let mut bounds = vec![s];
    if nchunks > 1 {
        for k in 1..nchunks {
            let target = s + (e - s) * k / nchunks;
            if let Some(o) = memchr::memmem::find(&xml[target..e], b"<row") {
                let b = target + o;
                if b > *bounds.last().unwrap() && b < e {
                    bounds.push(b);
                }
            }
        }
    }
    bounds.push(e);
    let ranges: Vec<(usize, usize, bool)> = (0..bounds.len() - 1)
        .map(|k| (bounds[k], bounds[k + 1], k == 0))
        .collect();
    let partials: Vec<Partial> = ranges
        .par_iter()
        .map(|&(lo, hi, has_header)| parse_region(xml, lo, hi, has_header, shared, feat, 0, None))
        .collect();
    Ok(merge_partials(partials, feat))
}

#[allow(clippy::needless_range_loop)] // parallel arrays of differing lengths (guards + gap padding)
fn merge_partials(mut partials: Vec<Partial>, feat: ScanFeat) -> Partial {
    if partials.is_empty() {
        return Partial {
            header: Vec::new(),
            cols: Vec::new(),
            dict: Dict::new(),
            nrows: 0,
            ncols: 0,
            abs_start: 0,
            style_cols: Vec::new(),
            fentries: Vec::new(),
            fdict: Dict::new(),
            anchors: Vec::new(),
            cell_errors: Vec::new(),
            row_dims: Vec::new(),
        };
    }
    let ncols = partials.iter().map(|p| p.ncols).max().unwrap_or(0);
    let header = std::mem::take(&mut partials[0].header);

    // Sort by absolute start so we can pad gaps then extend (chunks partition the sheet).
    // When every abs_start is 0 (no row @r), assign concatenate bases in original order.
    let any_abs = partials.iter().any(|p| p.abs_start > 0);
    let mut order: Vec<usize> = (0..partials.len()).collect();
    if any_abs {
        order.sort_by_key(|&i| partials[i].abs_start);
    }
    let mut starts: Vec<usize> = vec![0; partials.len()];
    if any_abs {
        for &i in &order {
            starts[i] = partials[i].abs_start;
        }
    } else {
        let mut run = 0usize;
        for &i in &order {
            starts[i] = run;
            run += partials[i].nrows;
        }
    }
    let total_rows = order
        .iter()
        .map(|&i| starts[i] + partials[i].nrows)
        .max()
        .unwrap_or(0);

    let mut gdict = Dict::new();
    let mut gcols: Vec<Column> = (0..ncols).map(|_| Column::Unset).collect();
    let mut gstyle: Vec<Vec<u32>> = if feat.styles {
        (0..ncols).map(|_| Vec::new()).collect()
    } else {
        Vec::new()
    };
    let mut gfdict = Dict::new();
    let mut gfentries: Vec<FEntry> = Vec::new();
    let mut ganchors: Vec<AnchorDef> = Vec::new();
    let mut gerrors: Vec<CellError> = Vec::new();
    let mut grow_dims: Vec<super::meta::RowDim> = Vec::new();

    for &i in &order {
        let p = &partials[i];
        let base = starts[i];
        let remap: Vec<u32> = {
            let nd = p.dict.ndistinct();
            (0..nd)
                .map(|id| gdict.intern(p.dict.resolve(id as u32)))
                .collect()
        };
        for c in 0..ncols {
            let pcol = match p.cols.get(c) {
                Some(col) => col,
                None => &Column::Unset,
            };

            if let Column::Unset = gcols[c] {
                match pcol {
                    Column::Num { .. } => {
                        gcols[c] = Column::Num {
                            v: Vec::new(),
                            valid: BitVec::new(),
                        }
                    }
                    Column::Str(_) => gcols[c] = Column::Str(Vec::new()),
                    Column::Mixed(_) => gcols[c] = Column::Mixed(Vec::new()),
                    Column::Unset => {}
                }
            }

            match (pcol, &mut gcols[c]) {
                (
                    Column::Num { v, valid },
                    Column::Num {
                        v: gv,
                        valid: gvalid,
                    },
                ) => {
                    while gv.len() < base {
                        gv.push(f64::NAN);
                        gvalid.push(false);
                    }
                    for irow in 0..p.nrows {
                        if irow < valid.len && valid.get(irow) {
                            gv.push(v[irow]);
                            gvalid.push(true);
                        } else {
                            gv.push(f64::NAN);
                            gvalid.push(false);
                        }
                    }
                }
                (Column::Str(sidx), Column::Str(gs)) => {
                    while gs.len() < base {
                        gs.push(NULL_IDX);
                    }
                    for irow in 0..p.nrows {
                        let id = sidx.get(irow).copied().unwrap_or(NULL_IDX);
                        gs.push(if id == NULL_IDX {
                            NULL_IDX
                        } else {
                            remap[id as usize]
                        });
                    }
                }
                (Column::Mixed(pm), Column::Mixed(gm)) => {
                    while gm.len() < base {
                        gm.push(MixedValue::Null);
                    }
                    for irow in 0..p.nrows {
                        let val = pm.get(irow).cloned().unwrap_or(MixedValue::Null);
                        let mapped_val = match val {
                            MixedValue::Str(id) => {
                                if id == NULL_IDX {
                                    MixedValue::Null
                                } else {
                                    MixedValue::Str(remap[id as usize])
                                }
                            }
                            other => other,
                        };
                        gm.push(mapped_val);
                    }
                }
                (Column::Unset, gcol) => match gcol {
                    Column::Num { v, valid } => {
                        while v.len() < base {
                            v.push(f64::NAN);
                            valid.push(false);
                        }
                        for _ in 0..p.nrows {
                            v.push(f64::NAN);
                            valid.push(false);
                        }
                    }
                    Column::Str(s) => {
                        while s.len() < base {
                            s.push(NULL_IDX);
                        }
                        for _ in 0..p.nrows {
                            s.push(NULL_IDX);
                        }
                    }
                    Column::Mixed(m) => {
                        while m.len() < base {
                            m.push(MixedValue::Null);
                        }
                        for _ in 0..p.nrows {
                            m.push(MixedValue::Null);
                        }
                    }
                    Column::Unset => {}
                },
                (p_other, gcol) => {
                    let mut mixed: Vec<MixedValue> = match gcol {
                        Column::Num {
                            v: gv,
                            valid: gvalid,
                        } => (0..gv.len())
                            .map(|i| {
                                if i < gvalid.len && gvalid.get(i) {
                                    MixedValue::Num(gv[i])
                                } else {
                                    MixedValue::Null
                                }
                            })
                            .collect(),
                        Column::Str(gs) => gs
                            .iter()
                            .map(|&idx| {
                                if idx == NULL_IDX {
                                    MixedValue::Null
                                } else {
                                    MixedValue::Str(idx)
                                }
                            })
                            .collect(),
                        Column::Mixed(m) => std::mem::take(m),
                        Column::Unset => Vec::new(),
                    };

                    while mixed.len() < base {
                        mixed.push(MixedValue::Null);
                    }

                    match p_other {
                        Column::Num { v, valid } => {
                            for irow in 0..p.nrows {
                                if irow < valid.len && valid.get(irow) {
                                    mixed.push(MixedValue::Num(v[irow]));
                                } else {
                                    mixed.push(MixedValue::Null);
                                }
                            }
                        }
                        Column::Str(sidx) => {
                            for irow in 0..p.nrows {
                                let id = sidx.get(irow).copied().unwrap_or(NULL_IDX);
                                if id == NULL_IDX {
                                    mixed.push(MixedValue::Null);
                                } else {
                                    mixed.push(MixedValue::Str(remap[id as usize]));
                                }
                            }
                        }
                        Column::Mixed(pm) => {
                            for irow in 0..p.nrows {
                                let val = pm.get(irow).cloned().unwrap_or(MixedValue::Null);
                                let mapped_val = match val {
                                    MixedValue::Str(id) => {
                                        if id == NULL_IDX {
                                            MixedValue::Null
                                        } else {
                                            MixedValue::Str(remap[id as usize])
                                        }
                                    }
                                    other => other,
                                };
                                mixed.push(mapped_val);
                            }
                        }
                        Column::Unset => {
                            for _ in 0..p.nrows {
                                mixed.push(MixedValue::Null);
                            }
                        }
                    }

                    *gcol = Column::Mixed(mixed);
                }
            }
        }
        if feat.styles {
            for c in 0..ncols {
                while gstyle[c].len() < base {
                    gstyle[c].push(0);
                }
                match p.style_cols.get(c) {
                    Some(sc) => {
                        for irow in 0..p.nrows {
                            gstyle[c].push(sc.get(irow).copied().unwrap_or(0));
                        }
                    }
                    None => {
                        for _ in 0..p.nrows {
                            gstyle[c].push(0);
                        }
                    }
                }
            }
        }
        if feat.formulas {
            let nd = p.fdict.ndistinct();
            let fremap: Vec<u32> = (0..nd)
                .map(|id| gfdict.intern(p.fdict.resolve(id as u32)))
                .collect();
            // With sheet @r, formula rows are absolute data indices. Without, local + base.
            let formula_abs = any_abs;
            for e in &p.fentries {
                let cell = match &e.cell {
                    FCell::Plain(id) => FCell::Plain(fremap[*id as usize]),
                    FCell::Shared { si, .. } => FCell::Shared {
                        si: *si,
                        rd: 0,
                        cd: 0,
                    },
                    FCell::Array {
                        r0,
                        c0,
                        r1,
                        c1,
                        text,
                    } => FCell::Array {
                        r0: *r0,
                        c0: *c0,
                        r1: *r1,
                        c1: *c1,
                        text: fremap[*text as usize],
                    },
                    FCell::DataTable(id) => FCell::DataTable(fremap[*id as usize]),
                };
                let row = if formula_abs {
                    e.row
                } else {
                    e.row + base as u32
                };
                gfentries.push(FEntry {
                    row,
                    col: e.col,
                    cell,
                });
            }
            for a in &p.anchors {
                let orow = if formula_abs {
                    a.orow
                } else {
                    a.orow + base as u32
                };
                ganchors.push(AnchorDef {
                    si: a.si,
                    text: fremap[a.text as usize],
                    orow,
                    ocol: a.ocol,
                });
            }
        }
        let err_abs = any_abs;
        for e in &p.cell_errors {
            let row = if err_abs { e.row } else { e.row + base as u32 };
            gerrors.push(CellError {
                row,
                col: e.col,
                code: e.code.clone(),
            });
        }
    }
    for c in gcols.iter_mut() {
        c.pad_to(total_rows);
    }
    if feat.styles {
        for c in gstyle.iter_mut() {
            while c.len() < total_rows {
                c.push(0);
            }
        }
    }
    // Merge sparse row dims from all chunks (dedupe by row index, first wins).
    if feat.row_meta {
        let mut seen = std::collections::HashSet::new();
        for &i in &order {
            for rd in std::mem::take(&mut partials[i].row_dims) {
                if seen.insert(rd.row) {
                    grow_dims.push(rd);
                }
            }
        }
        grow_dims.sort_by_key(|r| r.row);
    }
    Partial {
        header,
        cols: gcols,
        dict: gdict,
        nrows: total_rows,
        ncols,
        abs_start: 0,
        style_cols: gstyle,
        fentries: gfentries,
        fdict: gfdict,
        anchors: ganchors,
        cell_errors: gerrors,
        row_dims: grow_dims,
    }
}

#[cfg(test)]
pub(crate) fn synthetic_hydration_column(n: usize) -> FormulaColumn {
    let bodies = [
        "A2*2",
        "A2+B2",
        "SUM(A2:A50)",
        "IF(C2>0,D2,\"none\")",
        "VLOOKUP($A$2,$B$1:$D$99,2,FALSE)",
        "CONCATENATE(\"R\",ROW())",
    ];
    let mut fdict = Dict::new();
    let mut anchors = Vec::new();
    for (si, b) in bodies.iter().enumerate() {
        let id = fdict.intern(b.as_bytes());
        anchors.push(AnchorDef {
            si: si as u32,
            text: id,
            orow: 1,
            ocol: 1,
        });
    }
    let mut entries = Vec::with_capacity(n + bodies.len());
    for (si, _) in bodies.iter().enumerate() {
        entries.push(FEntry {
            row: 1,
            col: 1,
            cell: FCell::Shared {
                si: si as u32,
                rd: 0,
                cd: 0,
            },
        });
    }
    for k in 0..n {
        let si = (k % bodies.len()) as u32;
        entries.push(FEntry {
            row: (2 + k / bodies.len()) as u32,
            col: 1,
            cell: FCell::Shared {
                si,
                rd: (1 + k / bodies.len()) as i32,
                cd: 0,
            },
        });
    }
    FormulaColumn {
        entries,
        fdict,
        anchors,
    }
}

#[cfg(test)]
impl FormulaColumn {
    /// E4 candidate A reference shape: a fresh `String` per formula (via
    /// `translate_into` into a fresh buffer), sequential like the E4 race's
    /// `cand_a`. What shipped before the arena; the perf gate races the lazy
    /// arena path against this.
    pub(crate) fn materialize_all_naive(&self) -> Vec<String> {
        let anchors = self.anchor_by_si();
        self.entries
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let mut out = Vec::with_capacity(32);
                self.translate_into(&mut out, i, &anchors);
                String::from_utf8_lossy(&out).into_owned()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// Build a formula column from anchor bodies and shared dependents.
    /// Anchors sit at data-row 1 / data-col 1 (delta 0 for the anchor cell
    /// itself); each dependent carries its already-resolved (si, rd, cd).
    fn col(anchor_texts: &[&str], deps: &[(u32, u32, u32, i32, i32)]) -> FormulaColumn {
        let mut fdict = Dict::new();
        let mut anchors = Vec::new();
        for (si, t) in anchor_texts.iter().enumerate() {
            let id = fdict.intern(t.as_bytes());
            anchors.push(AnchorDef {
                si: si as u32,
                text: id,
                orow: 1,
                ocol: 1,
            });
        }
        let mut entries = Vec::new();
        for (si, _) in anchor_texts.iter().enumerate() {
            entries.push(FEntry {
                row: 1,
                col: 1,
                cell: FCell::Shared {
                    si: si as u32,
                    rd: 0,
                    cd: 0,
                },
            });
        }
        for &(row, col, si, rd, cd) in deps {
            entries.push(FEntry {
                row,
                col,
                cell: FCell::Shared { si, rd, cd },
            });
        }
        FormulaColumn {
            entries,
            fdict,
            anchors,
        }
    }

    #[test]
    fn dependent_translates_correctly_on_first_access() {
        let c = col(&["A2*2"], &[(3, 1, 0, 2, 0)]);
        assert_eq!(c.translate(3, 1).as_deref(), Some("A4*2"));
        // The anchor itself reads back its own text (delta 0).
        assert_eq!(c.translate(1, 1).as_deref(), Some("A2*2"));
    }

    #[test]
    fn same_dependent_read_twice_translates_once() {
        let c = col(&["B2+G2"], &[(4, 2, 0, 3, 1)]);
        let mut texts = c.lazy();
        let first = texts.text(4, 2).expect("dependent present").to_string();
        assert_eq!(texts.translated(), 1);
        let second = texts.text(4, 2).expect("dependent present").to_string();
        assert_eq!(texts.translated(), 1, "second read must not re-translate");
        assert_eq!(first, second);
        assert_eq!(first, "C5+H5");
    }

    #[test]
    fn dependent_translation_out_of_grid_is_ref() {
        // Anchor "A1" one row below the dependent: rd = -1 pushes row 1 → 0,
        // which is out of grid, so the operand degrades to #REF!.
        let c = col(&["A1"], &[(0, 1, 0, -1, 0)]);
        assert_eq!(c.translate(0, 1).as_deref(), Some("#REF!"));
    }

    #[test]
    fn anchor_with_no_dependents_reads_its_own_text() {
        let c = col(&["SUM(A1:A5)"], &[]);
        assert_eq!(c.translate(1, 1).as_deref(), Some("SUM(A1:A5)"));
        // materialize over just the anchor is a single-row arena build.
        assert_eq!(c.materialize_all(), vec!["SUM(A1:A5)".to_string()]);
    }

    #[test]
    fn orphan_shared_si_still_degrades_to_empty() {
        // A dependent whose si has no anchor must stay an empty string, never
        // panic — the historical degradation (turbo_malformed pins it too).
        let c = FormulaColumn {
            entries: vec![FEntry {
                row: 5,
                col: 0,
                cell: FCell::Shared {
                    si: 99,
                    rd: 0,
                    cd: 0,
                },
            }],
            fdict: Dict::new(),
            anchors: Vec::new(),
        };
        assert_eq!(c.translate(5, 0).as_deref(), Some(""));
        assert_eq!(c.materialize_all(), vec!["".to_string()]);
    }

    #[test]
    fn shared_deltas_resolved_at_take() {
        // Exercises the real construction path: deltas are filled by
        // resolve_shared_deltas from anchor coordinates, not handed in.
        let mut p = Partial {
            header: Vec::new(),
            cols: Vec::new(),
            dict: Dict::new(),
            nrows: 0,
            ncols: 0,
            abs_start: 0,
            style_cols: Vec::new(),
            fentries: vec![
                FEntry {
                    row: 2,
                    col: 0,
                    cell: FCell::Shared {
                        si: 0,
                        rd: 0,
                        cd: 0,
                    },
                },
                FEntry {
                    row: 7,
                    col: 2,
                    cell: FCell::Shared {
                        si: 0,
                        rd: 0,
                        cd: 0,
                    },
                },
            ],
            fdict: {
                let mut d = Dict::new();
                d.intern(b"A2*2");
                d
            },
            anchors: vec![AnchorDef {
                si: 0,
                text: 0,
                orow: 2,
                ocol: 0,
            }],
            cell_errors: Vec::new(),
            row_dims: Vec::new(),
        };
        let col = p.take_formula_column();
        // Anchor cell (row 2, col 0) → delta 0 → reads its own text.
        assert_eq!(col.translate(2, 0).as_deref(), Some("A2*2"));
        // Dependent (row 7, col 2) → delta (5, 2) → A2*2 becomes C7*2.
        assert_eq!(col.translate(7, 2).as_deref(), Some("C7*2"));
    }
}
