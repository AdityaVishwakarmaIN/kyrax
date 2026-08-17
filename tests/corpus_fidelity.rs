//! ST-3 — Excel-fidelity corpus test over real workbooks.
//!
//! Tier 1 added row/column insert and delete (`turbo/mutate.rs`) and the
//! non-grid fixups around them (`turbo/fixup.rs`). Every prior test for that
//! work used SYNTHETIC XML written by the same hands as the splice — that proves
//! self-consistency, not fidelity. This file runs the mutation paths over the
//! REAL workbooks in `nextexcel/testdata/` and checks that nothing is corrupted.
//!
//! Files are discovered at runtime (never hardcoded) so new corpus files are
//! picked up automatically; a file that cannot even be opened is skipped
//! gracefully. Per file × op (insert/delete rows, insert/delete cols, at a low
//! index and at a high index) we assert:
//!
//!   1. NEVER PANIC. A refusal is a fine outcome; a panic is a bug.
//!   2. The output is a valid ZIP and every input part is still present.
//!   3. Every XML part still parses (tag-balance wellformedness, checked as a
//!      source→output comparison so a deliberately malformed corpus file never
//!      produces a false positive).
//!   4. Parts the operation has no business touching are BYTE-IDENTICAL to the
//!      input (compared on the raw compressed payloads, since the overlay path
//!      copies precompressed parts verbatim). The overlay's whole design promise
//!      is that it rewrites `<sheetData>` and leaves everything else alone.
//!   5. Cell COUNT is conserved: after inserting n rows/cols the sheet keeps the
//!      same number of non-empty cells, and after deleting a band it has lost
//!      only what was in that band.
//!   6. On a refusal, no output is produced and the SOURCE file is byte-identical
//!      on disk.
//!
//! The `stress2_malformed_*` files are DELIBERATELY malformed: for those the
//! correct outcome is a clean error or a clean refusal, never a panic and never
//! a silently corrupt output.
//!
//! RUN TIME. The full cross product (every file × 8 ops) is behind `#[ignore]`:
//!
//!   cargo test --features __arrow --test corpus_fidelity corpus_fidelity_sweep_all -- --ignored
//!
//! The default run covers a fixed representative subset (every file, but large
//! files get a reduced op matrix) and stays under ~30 s.
//!
//! SECOND READER. openpyxl is available in the Python environment and is used
//! as an extra cross-check: each produced output workbook is opened with
//! openpyxl and must load without error. That is weak evidence of correctness
//! but STRONG evidence of non-corruption. It is not an oracle for semantics —
//! openpyxl does not update merges on delete at all, so it is not the authority
//! on what the values SHOULD be. Files whose sheet is too large to load quickly
//! are excluded from the Python pass.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use kyrax::turbo::overlay::WorkbookOverlay;
use kyrax::turbo::{ArchiveMap, ZipEntryMeta};

const MAX_COL: u32 = 16_384;

/// Sheet-size thresholds for the default matrix (see `build_cases`).
const SMALL_SHEET: usize = 1024 * 1024;
const MEDIUM_SHEET: usize = 6 * 1024 * 1024;

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

/// Which files the default run covers. The default covers EVERY discovered
/// file with a size-reduced op matrix (see `build_cases`); the `#[ignore]`
/// sweep is the full cross product. Nothing is hardcoded here — new corpus
/// files are picked up automatically by discovery.
fn stem_of(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("file")
        .to_string()
}

// ---------------------------------------------------------------------------
// ZIP helpers (tests are a separate crate, so `pub(crate)` zipmin internals are
// out of reach; a minimal correct DEFLATE inflate lives here, validated by CRC32
// against every part it decodes).
// ---------------------------------------------------------------------------

fn payload(zip: &[u8], meta: &ZipEntryMeta) -> Option<Vec<u8>> {
    let s = meta.data_offset as usize;
    let e = s + meta.compressed_size as usize;
    if e > zip.len() {
        return None;
    }
    Some(zip[s..e].to_vec())
}

fn inflate_payload(method: u16, p: &[u8]) -> Option<Vec<u8>> {
    match method {
        0 => Some(p.to_vec()),
        8 => inflate_deflate(p),
        _ => None,
    }
}

fn inflate_part_from(map: &ArchiveMap, zip: &[u8], name: &str) -> Option<Vec<u8>> {
    let meta = map.entries.get(name)?;
    let p = payload(zip, meta)?;
    inflate_payload(meta.compression_method, &p)
}

fn crc32(b: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for i in 0..256u32 {
        let mut c = i;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        table[i as usize] = c;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for &x in b {
        crc = table[((crc ^ x as u32) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

/// Bit reader over the LSB-first DEFLATE bit stream, refilling a u64 window.
struct BR<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u64,
    cnt: u32,
}

impl<'a> BR<'a> {
    fn new(data: &'a [u8]) -> Self {
        BR {
            data,
            pos: 0,
            buf: 0,
            cnt: 0,
        }
    }
    #[inline]
    fn need(&mut self, n: u32) -> Option<()> {
        while self.cnt < n {
            let b = *self.data.get(self.pos)?;
            self.pos += 1;
            self.buf |= (b as u64) << self.cnt;
            self.cnt += 8;
        }
        Some(())
    }
    #[inline]
    fn peek(&mut self, n: u32) -> Option<u32> {
        self.need(n)?;
        Some((self.buf as u32) & ((1u32 << n) - 1))
    }
    #[inline]
    fn drop(&mut self, n: u32) {
        self.buf >>= n;
        self.cnt -= n;
    }
    #[inline]
    fn read(&mut self, n: u32) -> Option<u32> {
        let v = self.peek(n)?;
        self.drop(n);
        Some(v)
    }
    fn align_byte(&mut self) {
        let drop = self.cnt % 8;
        if drop > 0 {
            self.buf >>= drop;
            self.cnt -= drop;
        }
    }
}

/// Canonical Huffman decoder with a decode table indexed by the whole code
/// (maxlen bits). Table entry packs (length << 11) | symbol; 0 is the invalid
/// marker.
struct Huff {
    maxlen: u32,
    table: Vec<u16>,
}

impl Huff {
    #[inline]
    fn decode(&self, br: &mut BR<'_>) -> Option<u16> {
        // Peek only as many bits as are actually available: a valid stream can
        // end with fewer than `maxlen` bits left (padding in the final byte),
        // and a short trailing code must still decode.
        let avail = br.cnt + 8 * (br.data.len() - br.pos) as u32;
        let n = self.maxlen.min(avail);
        let idx = br.peek(n)? as usize;
        let e = *self.table.get(idx)?;
        if e == 0 {
            return None;
        }
        let len = (e >> 11) as u32;
        if len > n {
            return None;
        }
        br.drop(len);
        Some(e & 0x7FF)
    }
}

fn build_huffman(lengths: &[u8]) -> Option<Huff> {
    let mut count = [0u16; 16];
    for &l in lengths {
        if l > 15 {
            return None;
        }
        count[l as usize] += 1;
    }
    count[0] = 0;
    let mut left: i64 = 1;
    let mut maxlen = 0u32;
    for (len, &count_len) in count.iter().enumerate().skip(1).take(15) {
        left = (left << 1) - count_len as i64;
        if left < 0 {
            return None;
        }
        if count_len > 0 {
            maxlen = len as u32;
        }
    }
    let mut code: u32 = 0;
    let mut first = [0u32; 16];
    for len in 1..=15usize {
        code = (code + count[len - 1] as u32) << 1;
        first[len] = code;
    }
    let mut table = vec![0u16; 1usize << maxlen];
    let mut next = [0u32; 16];
    for (sym, &l) in lengths.iter().enumerate() {
        let l = l as usize;
        if l == 0 {
            continue;
        }
        let c = first[l] + next[l];
        next[l] += 1;
        // Bits are read LSB-first, so the canonical (MSB-first) code must be
        // bit-reversed before indexing the decode table.
        let rc = bit_reverse(c, l as u32);
        let entry = ((l as u16) << 11) | (sym as u16);
        for k in 0..(1u32 << (maxlen - l as u32)) as usize {
            table[rc as usize | (k << l)] = entry;
        }
    }
    Some(Huff { maxlen, table })
}

fn bit_reverse(mut v: u32, n: u32) -> u32 {
    let mut r = 0u32;
    for _ in 0..n {
        r = (r << 1) | (v & 1);
        v >>= 1;
    }
    r
}

const LENGTH_BASE: [(u32, u32); 29] = [
    (3, 0),
    (4, 0),
    (5, 0),
    (6, 0),
    (7, 0),
    (8, 0),
    (9, 0),
    (10, 0),
    (11, 1),
    (13, 1),
    (15, 1),
    (17, 1),
    (19, 2),
    (23, 2),
    (27, 2),
    (31, 2),
    (35, 3),
    (43, 3),
    (51, 3),
    (59, 3),
    (67, 4),
    (83, 4),
    (99, 4),
    (115, 4),
    (131, 5),
    (163, 5),
    (195, 5),
    (227, 5),
    (258, 0),
];

const DIST_BASE: [(u32, u32); 30] = [
    (1, 0),
    (2, 0),
    (3, 0),
    (4, 0),
    (5, 1),
    (7, 1),
    (9, 2),
    (13, 2),
    (17, 3),
    (25, 3),
    (33, 4),
    (49, 4),
    (65, 5),
    (97, 5),
    (129, 6),
    (193, 6),
    (257, 7),
    (385, 7),
    (513, 8),
    (769, 8),
    (1025, 9),
    (1537, 9),
    (2049, 10),
    (3073, 10),
    (4097, 11),
    (6145, 11),
    (8193, 12),
    (12289, 12),
    (16385, 13),
    (24577, 13),
];

const CL_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

static FIXED_TABLES: std::sync::OnceLock<(Huff, Huff)> = std::sync::OnceLock::new();

fn fixed_tables() -> &'static (Huff, Huff) {
    FIXED_TABLES.get_or_init(|| {
        let mut lit = vec![8u8; 288];
        lit[144..256].fill(9);
        lit[256..280].fill(7);
        let dist = vec![5u8; 32];
        (
            build_huffman(&lit).expect("fixed litlen table"),
            build_huffman(&dist).expect("fixed dist table"),
        )
    })
}

fn lz77_copy(out: &mut Vec<u8>, start: usize, length: usize, dist: usize) {
    out.reserve(length);
    // Copy the match with exponential doubling: after the first `dist` bytes
    // exist, the next chunk equals an earlier prefix of the match, so the
    // source region [start..start+take) is always already written and we can
    // memcpy it in doubling-sized pieces instead of byte by byte.
    let mut copied = 0usize;
    let mut n = dist.min(length);
    while copied < length {
        let take = (length - copied).min(n).min(copied + dist);
        out.extend_from_within(start..start + take);
        copied += take;
        n = (n << 1).min(length);
    }
}

fn inflate_compressed(br: &mut BR<'_>, out: &mut Vec<u8>, lit: &Huff, dist: &Huff) -> Option<()> {
    loop {
        let sym = lit.decode(br)?;
        match sym {
            0..=255 => out.push(sym as u8),
            256 => return Some(()),
            257..=285 => {
                let (base, extra) = LENGTH_BASE[sym as usize - 257];
                let length = base + br.read(extra)?;
                let dsym = dist.decode(br)? as usize;
                let (dbase, dextra) = *DIST_BASE.get(dsym)?;
                let d = dbase + br.read(dextra)?;
                if d == 0 || d as usize > out.len() {
                    return None;
                }
                let start = out.len() - d as usize;
                lz77_copy(out, start, length as usize, d as usize);
            }
            _ => return None,
        }
    }
}

/// Minimal DEFLATE inflate (fixed + dynamic + stored blocks). Validated by
/// CRC32 against every part it decodes in the corpus.
fn inflate_deflate(data: &[u8]) -> Option<Vec<u8>> {
    let mut br = BR::new(data);
    let mut out: Vec<u8> = Vec::new();
    loop {
        let bfinal = br.read(1)?;
        let btype = br.read(2)?;
        match btype {
            0 => {
                br.align_byte();
                let len = br.read(16)? as usize;
                let nlen = br.read(16)? as usize;
                if (nlen ^ 0xFFFF) & 0xFFFF != len {
                    return None;
                }
                out.reserve(len);
                for _ in 0..len {
                    out.push(br.read(8)? as u8);
                }
            }
            1 => {
                let (lit, dist) = fixed_tables();
                inflate_compressed(&mut br, &mut out, lit, dist)?;
            }
            2 => {
                let hlit = br.read(5)? + 257;
                let hdist = br.read(5)? + 1;
                let hclen = br.read(4)? + 4;
                let mut cl = [0u8; 19];
                for i in 0..hclen as usize {
                    cl[CL_ORDER[i]] = br.read(3)? as u8;
                }
                let cl_huff = build_huffman(&cl)?;
                let mut lens: Vec<u8> = Vec::with_capacity((hlit + hdist) as usize);
                while (lens.len() as u32) < hlit + hdist {
                    let sym = cl_huff.decode(&mut br)?;
                    match sym {
                        0..=15 => lens.push(sym as u8),
                        16 => {
                            let prev = *lens.last().unwrap_or(&0);
                            let rep = br.read(2)? + 3;
                            for _ in 0..rep {
                                lens.push(prev);
                            }
                        }
                        17 => {
                            let rep = br.read(3)? + 3;
                            lens.resize(lens.len() + rep as usize, 0);
                        }
                        18 => {
                            let rep = br.read(7)? + 11;
                            lens.resize(lens.len() + rep as usize, 0);
                        }
                        _ => return None,
                    }
                }
                let lit = build_huffman(&lens[..hlit as usize])?;
                let dist = build_huffman(&lens[hlit as usize..])?;
                inflate_compressed(&mut br, &mut out, &lit, &dist)?;
            }
            _ => return None,
        }
        if bfinal == 1 {
            break;
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Byte helpers
// ---------------------------------------------------------------------------

fn find_sub(b: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || from >= b.len() || from + needle.len() > b.len() {
        return None;
    }
    b[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// Find the byte offset of an opening `<tag` (boundary after the name).
fn find_tag_boundary(b: &[u8], tag: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 1 + tag.len() <= b.len() {
        if b[i] == b'<' && &b[i + 1..i + 1 + tag.len()] == tag {
            let after = i + 1 + tag.len();
            if after >= b.len() || matches!(b[after], b' ' | b'>' | b'/' | b'\t' | b'\r' | b'\n') {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// The `>` that closes the tag starting at `from`, plus whether it is
/// self-closing. Skips attribute values in quotes.
fn find_tag_gt(b: &[u8], from: usize) -> Option<(usize, bool)> {
    let n = b.len();
    let mut quote: u8 = 0;
    let mut last_nonspace = 0u8;
    let mut j = from;
    while j < n {
        let c = b[j];
        if quote != 0 {
            if c == quote {
                quote = 0;
            }
            j += 1;
            continue;
        }
        match c {
            b'"' | b'\'' => quote = c,
            b'>' => return Some((j, last_nonspace == b'/')),
            b' ' | b'\t' | b'\r' | b'\n' => {}
            _ => last_nonspace = c,
        }
        j += 1;
    }
    None
}

/// Cheap XML wellformedness: elements balance, attribute quotes close, PIs /
/// comments / CDATA / DOCTYPE are skipped as opaque units. This is the floor,
/// not a full parser; the openpyxl cross-check below is the real parser.
fn wellformed_xml(b: &[u8]) -> bool {
    let n = b.len();
    let mut i = 0usize;
    let mut stack: Vec<&[u8]> = Vec::new();
    while i < n {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        if i + 1 >= n {
            return false;
        }
        match b[i + 1] {
            b'?' => match find_sub(b, i + 2, b"?>") {
                Some(p) => i = p + 2,
                None => return false,
            },
            b'!' => {
                if b[i + 1..].starts_with(b"<![CDATA[") {
                    match find_sub(b, i + 9, b"]]>") {
                        Some(p) => i = p + 3,
                        None => return false,
                    }
                } else if b[i + 1..].starts_with(b"<!--") {
                    match find_sub(b, i + 4, b"-->") {
                        Some(p) => i = p + 3,
                        None => return false,
                    }
                } else if b[i + 1..].starts_with(b"<!DOCTYPE") {
                    let mut j = i + 9;
                    let mut subset = 0u32;
                    let mut ok = false;
                    while j < n {
                        match b[j] {
                            b'[' => subset += 1,
                            b']' => subset = subset.saturating_sub(1),
                            b'>' if subset == 0 => {
                                i = j + 1;
                                ok = true;
                                break;
                            }
                            _ => {}
                        }
                        j += 1;
                    }
                    if !ok {
                        return false;
                    }
                } else {
                    return false;
                }
            }
            b'/' => {
                let name_start = i + 2;
                let mut name_end = name_start;
                while name_end < n
                    && b[name_end] != b'>'
                    && b[name_end] != b' '
                    && b[name_end] != b'\t'
                    && b[name_end] != b'\r'
                    && b[name_end] != b'\n'
                {
                    name_end += 1;
                }
                if name_end == name_start {
                    return false;
                }
                let Some(gt) = find_sub(b, name_end, b">") else {
                    return false;
                };
                let name = &b[name_start..name_end];
                match stack.pop() {
                    Some(top) if top == name => {}
                    _ => return false,
                }
                i = gt + 1;
            }
            _ => {
                let name_start = i + 1;
                let mut name_end = name_start;
                while name_end < n
                    && b[name_end] != b'>'
                    && b[name_end] != b' '
                    && b[name_end] != b'\t'
                    && b[name_end] != b'\r'
                    && b[name_end] != b'\n'
                    && b[name_end] != b'/'
                {
                    name_end += 1;
                }
                if name_end == name_start {
                    return false;
                }
                let name = &b[name_start..name_end];
                let Some((gt, self_closing)) = find_tag_gt(b, name_end) else {
                    return false;
                };
                if !self_closing {
                    stack.push(name);
                }
                i = gt + 1;
            }
        }
    }
    stack.is_empty()
}

fn parse_a1(r: &str) -> Option<(u32, u32)> {
    let bytes = r.as_bytes();
    let mut ci = 0usize;
    while ci < bytes.len() && bytes[ci].is_ascii_alphabetic() {
        ci += 1;
    }
    if ci == 0 {
        return None;
    }
    let mut col: u32 = 0;
    for &b in &bytes[..ci] {
        let v = (b.to_ascii_uppercase() - b'A' + 1) as u32;
        col = col.checked_mul(26)?.checked_add(v)?;
    }
    let row: u32 = std::str::from_utf8(&bytes[ci..]).ok()?.parse().ok()?;
    if row == 0 || col > MAX_COL {
        return None;
    }
    Some((row, col))
}

/// Count `<c>` elements in a worksheet part and collect their refs + content
/// bounds. Cells without an `r=` are counted but contribute no ref.
fn scan_cells(xml: &[u8]) -> (usize, u32, u32, Vec<(u32, u32)>) {
    let mut count = 0usize;
    let mut max_row = 0u32;
    let mut max_col = 0u32;
    let mut refs: Vec<(u32, u32)> = Vec::new();
    let mut i = 0usize;
    while let Some(cs) = find_tag_boundary(xml, b"c", i) {
        count += 1;
        if let Some(vs) = find_sub(xml, cs, b"r=\"") {
            let vs = vs + 3;
            if let Some(q) = xml[vs..].iter().position(|&x| x == b'"') {
                if let Ok(s) = std::str::from_utf8(&xml[vs..vs + q]) {
                    if let Some((r, c)) = parse_a1(s) {
                        refs.push((r, c));
                        max_row = max_row.max(r);
                        max_col = max_col.max(c);
                    }
                }
            }
        }
        i = cs + 1;
    }
    (count, max_row, max_col, refs)
}

fn cells_in_band(refs: &[(u32, u32)], axis_rows: bool, at: u32, count: u32) -> usize {
    refs.iter()
        .filter(|(r, c)| {
            if axis_rows {
                (at..at + count).contains(r)
            } else {
                (at..at + count).contains(c)
            }
        })
        .count()
}

fn is_xml_part(name: &str) -> bool {
    name.ends_with(".xml")
        || name.ends_with(".rels")
        || name.ends_with(".vml")
        || name == "[Content_Types].xml"
}

fn attr_val_str<'a>(tag: &'a [u8], name: &[u8]) -> Option<&'a str> {
    let mut needle = Vec::with_capacity(name.len() + 2);
    needle.extend_from_slice(name);
    needle.extend_from_slice(b"=\"");
    let p = find_sub(tag, 0, &needle)?;
    let vs = p + needle.len();
    let q = tag[vs..].iter().position(|&x| x == b'"')?;
    std::str::from_utf8(&tag[vs..vs + q]).ok()
}

fn resolve_rel_target(base_dir: &str, target: &str) -> String {
    let absolute = target.starts_with('/');
    let t = target.trim_start_matches('/');
    let mut parts: Vec<String> = if absolute {
        Vec::new()
    } else {
        base_dir
            .split('/')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect()
    };
    for seg in t.split('/') {
        match seg {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            s => parts.push(s.to_string()),
        }
    }
    parts.join("/")
}

/// Find the start of an element whose LOCAL name is `local`, tolerating a
/// namespace prefix (`<pivotCache .../>` or `<s:pivotCache .../>`). The byte
/// before the local name must be `<` or `:`, the byte after a tag boundary.
fn find_element_local(xml: &[u8], local: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while let Some(s) = find_sub(xml, i, local) {
        let prev_ok = s > 0 && (xml[s - 1] == b'<' || xml[s - 1] == b':');
        let next = xml.get(s + local.len()).copied();
        let next_ok = matches!(
            next,
            None | Some(b' ') | Some(b'>') | Some(b'/') | Some(b'\t') | Some(b'\r') | Some(b'\n')
        );
        if prev_ok && next_ok {
            return Some(s);
        }
        i = s + local.len();
    }
    None
}

/// Open-tag bytes of every `<local ...>` / `<p:local ...>` element.
fn open_tags<'a>(xml: &'a [u8], local: &[u8]) -> Vec<&'a [u8]> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(s) = find_element_local(xml, local, from) {
        let Some(gt) = find_sub(xml, s, b">") else {
            break;
        };
        out.push(&xml[s..gt]);
        from = gt + 1;
    }
    out
}

/// Pivot parts owned by one sheet (the pivot table parts via its rels, plus
/// each one's cache definition part), resolved exactly the way the overlay save
/// path resolves them. These are the OTHER parts a row/column mutation
/// legitimately rewrites (a cache source ref shifts when its source sheet is
/// mutated, and a pivot location shifts when its host sheet is), so they join
/// the byte-identity exemption set.
fn sheet_pivot_parts(map: &ArchiveMap, zip: &[u8], sheet_path: &str) -> Vec<String> {
    let base = sheet_path.rsplit('/').next().unwrap_or("sheet1.xml");
    let rels_path = format!("xl/worksheets/_rels/{base}.rels");
    let Some(sheet_rels) = inflate_part_from(map, zip, &rels_path) else {
        return Vec::new();
    };
    let mut table_parts = Vec::new();
    for tag in open_tags(&sheet_rels, b"Relationship") {
        let t = attr_val_str(tag, b"Type").unwrap_or("");
        if t.ends_with("/pivotTable") {
            if let Some(target) = attr_val_str(tag, b"Target") {
                table_parts.push(resolve_rel_target("xl/worksheets/", target));
            }
        }
    }
    if table_parts.is_empty() {
        return Vec::new();
    }

    // Workbook <pivotCache cacheId r:id/> -> (cacheId, r:id); rels -> (r:id, target).
    let Some(wb_xml) = inflate_part_from(map, zip, "xl/workbook.xml") else {
        return Vec::new();
    };
    let mut cid_to_rid: Vec<(u32, String)> = Vec::new();
    for tag in open_tags(&wb_xml, b"pivotCache") {
        let cid = attr_val_str(tag, b"cacheId").and_then(|s| s.parse().ok());
        let rid = attr_val_str(tag, b"r:id").map(|s| s.to_string());
        if let (Some(cid), Some(rid)) = (cid, rid) {
            cid_to_rid.push((cid, rid));
        }
    }
    let mut rid_to_cache = HashMap::new();
    if let Some(wb_rels) = inflate_part_from(map, zip, "xl/_rels/workbook.xml.rels") {
        for tag in open_tags(&wb_rels, b"Relationship") {
            let id = attr_val_str(tag, b"Id").map(|s| s.to_string());
            let target = attr_val_str(tag, b"Target").map(|s| s.to_string());
            if let (Some(id), Some(target)) = (id, target) {
                rid_to_cache.insert(id, resolve_rel_target("xl/", &target));
            }
        }
    }

    let mut out = Vec::new();
    for pt in table_parts {
        out.push(pt.clone());
        let cid = inflate_part_from(map, zip, &pt).and_then(|px| peek_pivot_cache_id(&px));
        if let Some(cid) = cid {
            if let Some(rid) = cid_to_rid
                .iter()
                .find(|(c, _)| *c == cid)
                .map(|(_, r)| r.clone())
            {
                if let Some(cp) = rid_to_cache.get(&rid) {
                    out.push(cp.clone());
                }
            }
        }
    }
    out
}

fn peek_pivot_cache_id(xml: &[u8]) -> Option<u32> {
    let start = find_sub(xml, 0, b"cacheId=\"")?;
    let vs = start + 9;
    let q = xml[vs..].iter().position(|&x| x == b'"')?;
    std::str::from_utf8(&xml[vs..vs + q]).ok()?.parse().ok()
}

/// Table parts owned by one sheet, resolved exactly the way the overlay save
/// path resolves them (tableParts rids → rels → target). Used to build the
/// byte-identity exemption set: these are the only OTHER parts a mutation
/// legitimately rewrites besides the sheet part and workbook.xml.
fn sheet_table_parts(map: &ArchiveMap, zip: &[u8], sheet_path: &str) -> Vec<String> {
    let Some(sheet_xml) = inflate_part_from(map, zip, sheet_path) else {
        return Vec::new();
    };
    let tail = match find_sub(&sheet_xml, 0, b"</sheetData>") {
        Some(p) => &sheet_xml[p + b"</sheetData>".len()..],
        None => &sheet_xml[..],
    };
    let mut rids = Vec::new();
    let mut i = 0usize;
    while let Some(rs) = find_sub(tail, i, b"<tablePart") {
        if let Some(vs) = find_sub(tail, rs, b"r:id=\"") {
            let vs = vs + 6;
            if let Some(q) = tail[vs..].iter().position(|&x| x == b'"') {
                if let Ok(s) = std::str::from_utf8(&tail[vs..vs + q]) {
                    rids.push(s.to_string());
                }
            }
        }
        i = rs + 1;
    }
    if rids.is_empty() {
        return Vec::new();
    }
    let base = sheet_path.rsplit('/').next().unwrap_or("sheet1.xml");
    let rels_path = format!("xl/worksheets/_rels/{base}.rels");
    let Some(rels) = inflate_part_from(map, zip, &rels_path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut j = 0usize;
    while let Some(rs) = find_sub(&rels, j, b"<Relationship") {
        let re = match find_sub(&rels, rs, b">") {
            Some(re) => re,
            None => break,
        };
        let tag = &rels[rs..re];
        if let Some(id) = attr_val_str(tag, b"Id") {
            if rids.iter().any(|r| r == id) {
                if let Some(target) = attr_val_str(tag, b"Target") {
                    out.push(resolve_rel_target("xl/worksheets/", target));
                }
            }
        }
        j = re + 1;
    }
    out
}

// ---------------------------------------------------------------------------
// Per-file sheet info
// ---------------------------------------------------------------------------

struct SheetInfo {
    map: ArchiveMap,
    src_bytes: Arc<Vec<u8>>,
    path: PathBuf,
    sheet: String,
    sheet_path: String,
    xml: Vec<u8>,
    max_row: u32,
    max_col: u32,
    size: u64,
    table_parts: Vec<String>,
    pivot_parts: Vec<String>,
}

fn pick_sheet(map: &ArchiveMap) -> Option<String> {
    let mut names: Vec<&String> = map.sheet_name_map.keys().collect();
    names.sort();
    names
        .into_iter()
        .find(|n| map.sheet_name_map[n.as_str()].starts_with("xl/worksheets/"))
        .cloned()
        .or_else(|| map.sheet_name_map.keys().next().cloned())
}

/// Outcome of opening one corpus file.
#[allow(clippy::large_enum_variant)]
enum OpenOutcome {
    /// File could not be parsed as a zip / has no sheets.
    Skipped,
    /// Sheet is large enough that the default run defers it to the sweep.
    Deferred,
    Opened(SheetInfo),
}

fn build_info(path: &Path) -> OpenOutcome {
    let bytes = match std::fs::read(path) {
        Ok(b) => Arc::new(b),
        Err(_) => return OpenOutcome::Skipped,
    };
    let map = match ArchiveMap::parse(bytes.clone()) {
        Ok(m) => m,
        Err(_) => return OpenOutcome::Skipped,
    };
    if map.sheet_name_map.is_empty() {
        return OpenOutcome::Skipped;
    }
    let Some(sheet) = pick_sheet(&map) else {
        return OpenOutcome::Skipped;
    };
    let Some(sheet_path) = map.sheet_name_map.get(&sheet).cloned() else {
        return OpenOutcome::Skipped;
    };

    // Defer the LARGE sheets from the central-directory metadata alone, before
    // inflating anything. Inflating a 150 MB sheet just to decide "defer" is
    // most of the default run's wall time.
    let sheet_uncompressed = map
        .entries
        .get(&sheet_path)
        .map(|m| m.uncompressed_size as usize)
        .unwrap_or(0);
    if sheet_uncompressed >= MEDIUM_SHEET {
        return OpenOutcome::Deferred;
    }

    let Some(xml) = inflate_part_from(&map, &bytes, &sheet_path) else {
        return OpenOutcome::Skipped;
    };
    let (_, max_row, max_col, _) = scan_cells(&xml);
    let table_parts = sheet_table_parts(&map, &bytes, &sheet_path);
    let pivot_parts = sheet_pivot_parts(&map, &bytes, &sheet_path);
    OpenOutcome::Opened(SheetInfo {
        size: bytes.len() as u64,
        path: path.to_path_buf(),
        map,
        src_bytes: bytes,
        sheet,
        sheet_path,
        xml,
        max_row,
        max_col,
        table_parts,
        pivot_parts,
    })
}

// ---------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Op {
    InsertRows,
    DeleteRows,
    InsertCols,
    DeleteCols,
}

impl Op {
    fn label(&self) -> &'static str {
        match self {
            Op::InsertRows => "insert_rows",
            Op::DeleteRows => "delete_rows",
            Op::InsertCols => "insert_cols",
            Op::DeleteCols => "delete_cols",
        }
    }
    fn axis_rows(&self) -> bool {
        matches!(self, Op::InsertRows | Op::DeleteRows)
    }
    fn is_insert(&self) -> bool {
        matches!(self, Op::InsertRows | Op::InsertCols)
    }
}

#[derive(Clone, Copy)]
struct Case {
    op: Op,
    at: u32,
    count: u32,
    at_kind: &'static str,
}

impl Case {
    fn label(&self) -> String {
        format!("{}@{}", self.op.label(), self.at_kind)
    }
}

/// Build the op matrix for one file.
///
/// Exhaustive mode is the full cross product (4 ops × low/high) over every
/// file — that is the `#[ignore]` sweep.
///
/// Default mode sizes the matrix by the MUTATED SHEET's uncompressed size so
/// the debug-mode default run stays under ~30 s (the crate's own splice + zip
/// write is what is slow on multi-hundred-MB sheets, not this file's checks):
///   sheet < 1 MB: full 4×2 matrix (8 ops)
///   1..6 MB sheet: 4 ops at the low index
///   sheet >= 6 MB: no default coverage — deferred to the exhaustive sweep
fn build_cases(inf: &SheetInfo, exhaustive: bool) -> Vec<Case> {
    let ops = [
        Op::InsertRows,
        Op::DeleteRows,
        Op::InsertCols,
        Op::DeleteCols,
    ];

    if exhaustive {
        let mut cases = Vec::new();
        for &op in &ops {
            let lo = 1u32;
            let hi = if op.axis_rows() {
                inf.max_row.max(1)
            } else {
                inf.max_col.max(1)
            };
            cases.push(Case {
                op,
                at: lo,
                count: 1,
                at_kind: "low",
            });
            if hi != lo {
                cases.push(Case {
                    op,
                    at: hi,
                    count: 1,
                    at_kind: "high",
                });
            }
        }
        return cases;
    }

    let sheet_len = inf.xml.len();
    if sheet_len >= MEDIUM_SHEET {
        // Deferred to the sweep: mutating a 100+ MB sheet in a debug build
        // takes tens of seconds per op.
        return Vec::new();
    }
    let add_high = sheet_len < SMALL_SHEET;
    let mut cases = Vec::new();
    for &op in &ops {
        let hi = if op.axis_rows() {
            inf.max_row.max(1)
        } else {
            inf.max_col.max(1)
        };
        cases.push(Case {
            op,
            at: 1,
            count: 1,
            at_kind: "low",
        });
        if add_high && hi != 1 {
            cases.push(Case {
                op,
                at: hi,
                count: 1,
                at_kind: "high",
            });
        }
    }
    cases
}

enum CaseResult {
    Pass,
    Refused,
    Violation(String),
    Panic(String),
}

fn run_case(inf: &SheetInfo, case: &Case, saved_out: &mut Option<Vec<u8>>) -> CaseResult {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_case_inner(inf, case, saved_out)
    }));
    match outcome {
        Ok(r) => r,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic payload".to_string());
            CaseResult::Panic(msg)
        }
    }
}

fn run_case_inner(inf: &SheetInfo, case: &Case, saved_out: &mut Option<Vec<u8>>) -> CaseResult {
    let mut ov = WorkbookOverlay::new(inf.map.clone());
    match case.op {
        Op::InsertRows => ov.insert_rows(&inf.sheet, case.at, case.count),
        Op::DeleteRows => ov.delete_rows(&inf.sheet, case.at, case.count),
        Op::InsertCols => ov.insert_cols(&inf.sheet, case.at, case.count),
        Op::DeleteCols => ov.delete_cols(&inf.sheet, case.at, case.count),
    }
    let saved = match ov.save() {
        Ok(b) => b,
        Err(_) => {
            // Assertion 6: a refusal writes nothing and leaves the source
            // byte-identical on disk (we only ever read the corpus, but make
            // the promise explicit).
            let now = std::fs::read(&inf.path).ok();
            if now.as_deref() != Some(inf.src_bytes.as_ref().as_slice()) {
                return CaseResult::Violation(
                    "source file changed on disk after a refusal".to_string(),
                );
            }
            return CaseResult::Refused;
        }
    };
    let mut violations = Vec::new();
    validate_output(inf, &saved, case, &mut violations);
    if violations.is_empty() {
        *saved_out = Some(saved);
        CaseResult::Pass
    } else {
        CaseResult::Violation(violations.join(" | "))
    }
}

fn exempt_set(inf: &SheetInfo) -> HashSet<String> {
    let mut s = HashSet::new();
    s.insert(inf.sheet_path.clone());
    s.insert("xl/workbook.xml".to_string());
    for tp in &inf.table_parts {
        s.insert(tp.clone());
    }
    for pp in &inf.pivot_parts {
        s.insert(pp.clone());
    }
    s
}

fn validate_output(inf: &SheetInfo, saved: &[u8], case: &Case, violations: &mut Vec<String>) {
    let out_map = match ArchiveMap::parse(Arc::new(saved.to_vec())) {
        Ok(m) => m,
        Err(e) => {
            violations.push(format!("output is not a valid zip: {e}"));
            return;
        }
    };

    // Assertion 2: every part that was in the input is still present.
    for name in &inf.map.entry_order {
        if !out_map.entries.contains_key(name) {
            violations.push(format!("part '{name}' vanished from the output"));
        }
    }

    let exempt = exempt_set(inf);

    for name in &inf.map.entry_order {
        let src_meta = match inf.map.entries.get(name) {
            Some(m) => m,
            None => continue,
        };
        let src_payload = payload(inf.src_bytes.as_ref().as_slice(), src_meta);
        let out_meta = match out_map.entries.get(name) {
            Some(m) => m,
            None => continue,
        };
        let out_payload = payload(saved, out_meta);

        // Assertion 4: untouched parts are byte-identical (raw compressed
        // payloads, since the overlay copies precompressed parts verbatim).
        if !exempt.contains(name) && src_payload.as_deref() != out_payload.as_deref() {
            violations.push(format!(
                "part '{name}' changed despite {} having no business touching it",
                case.op.label()
            ));
        }

        // Assertion 3 (wellformedness) + CRC integrity, as a source→output
        // comparison so a deliberately malformed corpus part never false-fires.
        if is_xml_part(name) {
            let src_inf = src_payload
                .as_deref()
                .and_then(|p| inflate_payload(src_meta.compression_method, p));
            let out_inf = out_payload
                .as_deref()
                .and_then(|p| inflate_payload(out_meta.compression_method, p));
            let src_crc_ok = src_inf
                .as_ref()
                .map(|d| crc32(d) == src_meta.crc32)
                .unwrap_or(false);

            if let (Some(s), Some(o)) = (&src_inf, &out_inf) {
                if wellformed_xml(s) && !wellformed_xml(o) {
                    violations.push(format!(
                        "part '{name}' was well-formed before the {} and is malformed after",
                        case.op.label()
                    ));
                }
            } else if src_crc_ok {
                violations.push(format!("output part '{name}' could not be inflated"));
            }

            if src_crc_ok {
                if let Some(o) = &out_inf {
                    if crc32(o) != out_meta.crc32 {
                        violations.push(format!("part '{name}' fails its CRC in the output"));
                    }
                }
            }
        }
    }

    // Assertion 5: cell count is conserved.
    let out_sheet = inflate_part_from(&out_map, saved, &inf.sheet_path);
    match out_sheet {
        Some(o) => {
            let (src_count, _, _, src_refs) = scan_cells(&inf.xml);
            let (out_count, _, _, _) = scan_cells(&o);
            if case.op.is_insert() {
                if out_count != src_count {
                    violations.push(format!(
                        "cell count changed on {}: {} -> {}",
                        case.op.label(),
                        src_count,
                        out_count
                    ));
                }
            } else {
                let in_band = cells_in_band(&src_refs, case.op.axis_rows(), case.at, case.count);
                let expected = src_count.saturating_sub(in_band);
                if out_count != expected {
                    violations.push(format!(
                        "cell count wrong on {}: expected {} ({} minus {in_band} in band), got {}",
                        case.op.label(),
                        expected,
                        src_count,
                        out_count
                    ));
                }
            }
        }
        None => violations.push(format!(
            "output sheet part '{}' could not be inflated",
            inf.sheet_path
        )),
    }
}

// ---------------------------------------------------------------------------
// openpyxl cross-check (second reader)
// ---------------------------------------------------------------------------

const PY_SCRIPT: &str = r#"import sys, traceback
try:
    from openpyxl import load_workbook
except Exception:
    print("NOOPENPYXL")
    sys.exit(0)
for p in sys.argv[1:]:
    try:
        wb = load_workbook(p)
        for ws in wb.worksheets:
            _ = ws.max_row
            _ = ws.max_column
        print("OK\t" + p)
    except Exception:
        msg = traceback.format_exc().replace("\t", " ").replace("\n", "\x01")
        print("FAIL\t" + p + "\t" + msg)
"#;

fn find_python() -> Option<String> {
    for cand in ["python", "python3"] {
        if Command::new(cand)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            return Some(cand.to_string());
        }
    }
    None
}

fn sanitize_label(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Run the openpyxl cross-check over baselines (SRC_<stem>) and outputs
/// (<stem>_<n>). Returns a list of findings; empty on success or when the
/// Python environment is unavailable.
fn python_check(baselines: &[(String, Vec<u8>)], outputs: &[(String, Vec<u8>)]) -> Vec<String> {
    if outputs.is_empty() && baselines.is_empty() {
        return Vec::new();
    }
    let python = match find_python() {
        Some(p) => p,
        None => return Vec::new(),
    };
    let dir = std::env::temp_dir().join("kyrax_corpus_fidelity_py");
    if std::fs::create_dir_all(&dir).is_err() {
        return Vec::new();
    }
    let script_path = dir.join("openpyxl_check.py");
    if std::fs::write(&script_path, PY_SCRIPT).is_err() {
        return Vec::new();
    }

    let mut paths = Vec::new();
    for (label, bytes) in baselines.iter().chain(outputs.iter()) {
        let p = dir.join(sanitize_label(label));
        if std::fs::write(&p, bytes).is_ok() {
            paths.push(p);
        }
    }
    let out = match Command::new(&python)
        .arg(&script_path)
        .args(&paths)
        .output()
    {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    if stdout.contains("NOOPENPYXL") {
        return Vec::new();
    }

    let mut status: HashMap<String, Result<(), String>> = HashMap::new();
    for line in stdout.lines() {
        let mut parts = line.split('\t');
        let kind = parts.next().unwrap_or("");
        let path = parts.next().unwrap_or("").to_string();
        match kind {
            "OK" => {
                status.insert(path, Ok(()));
            }
            "FAIL" => {
                let raw = parts.next().unwrap_or("").replace('\x01', "\n");
                status.insert(path, Err(raw));
            }
            _ => {}
        }
    }

    let mut findings = Vec::new();
    for (label, _) in outputs {
        let stem = label
            .rsplit_once('_')
            .map(|(s, _)| s.to_string())
            .unwrap_or_else(|| label.clone());
        let base_label = format!("SRC_{stem}");
        let base_path = dir.join(sanitize_label(&base_label));
        let out_path = dir.join(sanitize_label(label));
        let base_ok = matches!(
            status.get(&base_path.to_string_lossy().to_string()),
            Some(Ok(()))
        );
        if !base_ok {
            continue;
        }
        match status.get(&out_path.to_string_lossy().to_string()) {
            Some(Ok(())) => {}
            Some(Err(e)) => {
                findings.push(format!(
                    "openpyxl failed to load {stem} output after {}:\n{e}",
                    label
                ));
            }
            None => {
                findings.push(format!("openpyxl produced no verdict for {label}"));
            }
        }
    }
    findings
}

// ---------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------

struct RunSummary {
    files_discovered: usize,
    files_opened: usize,
    files_skipped: usize,
    files_deferred: usize,
    cases: usize,
    passes: usize,
    refusals: usize,
    panics: usize,
    violations: usize,
    findings: Vec<String>,
    py_outputs: usize,
    py_fail: usize,
    py_skipped: usize,
}

impl RunSummary {
    fn new(discovered: usize) -> Self {
        RunSummary {
            files_discovered: discovered,
            files_opened: 0,
            files_skipped: 0,
            files_deferred: 0,
            cases: 0,
            passes: 0,
            refusals: 0,
            panics: 0,
            violations: 0,
            findings: Vec::new(),
            py_outputs: 0,
            py_fail: 0,
            py_skipped: 0,
        }
    }
    fn finish(&mut self) {
        // Clean up the Python scratch dir we created, if any.
        let dir = std::env::temp_dir().join("kyrax_corpus_fidelity_py");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

fn run_files(paths: &[PathBuf], exhaustive: bool) -> RunSummary {
    let mut summary = RunSummary::new(paths.len());
    let mut py_baselines: Vec<(String, Vec<u8>)> = Vec::new();
    let mut py_outputs: Vec<(String, Vec<u8>)> = Vec::new();

    // openpyxl is a slow second reader (seconds per file on the big corpora),
    // so the Python cross-check is limited to small sheets and a bounded number
    // of outputs. It is strong evidence of non-corruption, not the primary
    // assertion.
    const PY_MAX_SHEET: usize = 512 * 1024;
    const PY_MAX_OUTPUTS: usize = 20;

    // Debug hook: restrict the run to files whose stem contains this substring.
    let filter = std::env::var("KYRAX_CORPUS_FILTER").unwrap_or_default();

    for path in paths {
        let stem = stem_of(path);
        if !filter.is_empty() && !stem.contains(&filter) {
            continue;
        }
        let t0 = std::time::Instant::now();
        let inf = match build_info(path) {
            OpenOutcome::Skipped => {
                summary.files_skipped += 1;
                eprintln!("  skip  {stem} (cannot be opened)");
                continue;
            }
            OpenOutcome::Deferred => {
                summary.files_deferred += 1;
                eprintln!("  defer {stem} (large sheet; default skips it — see the sweep)");
                continue;
            }
            OpenOutcome::Opened(inf) => inf,
        };
        summary.files_opened += 1;

        let cases = build_cases(&inf, exhaustive);
        if cases.is_empty() && !exhaustive {
            summary.files_deferred += 1;
            eprintln!("  defer {stem} (large sheet; default skips it — see the sweep)");
            continue;
        }
        let mut file_pass = 0;
        let mut file_refuse = 0;
        let mut file_violate = 0;
        let mut file_panic = 0;
        for (ci, case) in cases.iter().enumerate() {
            summary.cases += 1;
            let mut saved: Option<Vec<u8>> = None;
            let res = run_case(&inf, case, &mut saved);
            match res {
                CaseResult::Pass => {
                    summary.passes += 1;
                    file_pass += 1;
                    if let Some(b) = saved {
                        if inf.xml.len() < PY_MAX_SHEET && py_outputs.len() < PY_MAX_OUTPUTS {
                            py_outputs.push((format!("{stem}_{ci}"), b));
                        } else {
                            summary.py_skipped += 1;
                        }
                    }
                }
                CaseResult::Refused => {
                    summary.refusals += 1;
                    file_refuse += 1;
                }
                CaseResult::Violation(v) => {
                    summary.violations += 1;
                    file_violate += 1;
                    summary
                        .findings
                        .push(format!("[{stem} {}] {}", case.label(), v));
                }
                CaseResult::Panic(m) => {
                    summary.panics += 1;
                    file_panic += 1;
                    summary
                        .findings
                        .push(format!("[{stem} {}] PANIC: {m}", case.label()));
                }
            }
        }

        if inf.xml.len() < PY_MAX_SHEET {
            py_baselines.push((format!("SRC_{stem}"), inf.src_bytes.as_ref().to_vec()));
        }

        let dt = t0.elapsed().as_secs_f32();
        eprintln!(
            "  done  {stem} ({:.0} KB, {} case(s)) pass {file_pass} refuse {file_refuse} \
             violate {file_violate} panic {file_panic} in {dt:.1}s",
            inf.size as f32 / 1024.0,
            cases.len()
        );
    }

    let py_findings = python_check(&py_baselines, &py_outputs);
    for f in &py_findings {
        summary.py_fail += 1;
        summary.findings.push(f.clone());
    }
    summary.py_outputs = py_outputs.len();

    let mut out = String::new();
    use std::fmt::Write as _;
    let _ = writeln!(out, "\n=== kyrax corpus fidelity (ST-3) ===");
    let _ = writeln!(
        out,
        "files: {} discovered, {} opened, {} skipped (unopenable), {} deferred to sweep",
        summary.files_discovered,
        summary.files_opened,
        summary.files_skipped,
        summary.files_deferred
    );
    let _ = writeln!(
        out,
        "cases run: {}  (pass {} / refuse {} / violate {} / panic {})",
        summary.cases, summary.passes, summary.refusals, summary.violations, summary.panics
    );
    let _ = writeln!(
        out,
        "openpyxl cross-check: {} outputs checked, {} failed, {} skipped",
        summary.py_outputs, summary.py_fail, summary.py_skipped
    );
    for f in &summary.findings {
        let _ = writeln!(out, "FINDING: {f}");
    }
    println!("{out}");
    summary
}

fn assert_clean(summary: &RunSummary) {
    assert!(
        summary.cases > 0,
        "no cases ran — corpus discovery may be broken"
    );
    let n_violations = summary.violations;
    let n_panics = summary.panics;
    let mut detail = String::new();
    use std::fmt::Write as _;
    for f in summary.findings.iter().take(5) {
        let _ = writeln!(detail, "    {f}");
    }
    assert!(
        n_panics == 0 && n_violations == 0,
        "corpus fidelity FAILED: {} violation(s), {} panic(s); first findings:\n{detail}",
        n_violations,
        n_panics
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Default run: every file, but large files get a reduced op matrix so the
/// whole thing stays under ~30 s. The exhaustive sweep is the ignored test
/// below.
#[test]
fn corpus_fidelity_representative() {
    let paths = discover_files();
    let mut summary = run_files(&paths, false);
    summary.finish();
    assert_clean(&summary);
}

/// Exhaustive sweep: every corpus file × the full 4-op × 2-index matrix.
/// Run with:
///   cargo test --features __arrow --test corpus_fidelity corpus_fidelity_sweep_all -- --ignored
#[test]
#[ignore = "exhaustive sweep over the full corpus × op matrix; run explicitly"]
fn corpus_fidelity_sweep_all() {
    let paths = discover_files();
    let mut summary = run_files(&paths, true);
    summary.finish();
    assert_clean(&summary);
}
