//! OLE2 / Compound File Binary (CFB) container reader.
//!
//! An encrypted XLSX workbook is not a zip archive: it is an OLE2 container
//! whose useful payload lives in two streams, `EncryptionInfo` and
//! `EncryptedPackage`. This module implements the container layer only — no
//! cryptography. Key derivation lives in `crypto/keys.rs` and the wiring in
//! `crypto/mod.rs` (other agents; do not edit either here).
//!
//! Every value read from the file is treated as hostile: sector indices and
//! chain pointers are bounds-checked, circular FAT/MiniFAT chains are detected
//! and reported as [`CfbError::CycleDetected`], and declared stream sizes are
//! capped to what the file can actually hold instead of trusting the header.

use std::collections::HashSet;

const MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

/// End-of-chain marker for FAT and MiniFAT chains.
// Kept: the OLE/CFB sentinel. Named so a reader of the FAT walk can see
// what 0xFFFFFFFE means without looking it up.
#[allow(dead_code)]
const ENDOFCHAIN: u32 = 0xFFFFFFFE;
/// Smallest special value; anything at or above this is not a real sector id.
const MAXREGSECT: u32 = 0xFFFFFFFA;

const HEADER_SIZE: usize = 512;
const DIR_ENTRY_SIZE: usize = 128;

/// Detected container kind for a non-zip input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CfbKind {
    EncryptedOoxml,
    LegacyBiff,
    Unknown,
}

/// Why a CFB container could not be parsed.
#[derive(Debug)]
pub enum CfbError {
    Truncated,
    BadHeader,
    BadFat,
    CycleDetected,
    StreamTooLarge,
}

/// One parsed directory entry that carries a payload (root, storage or stream).
#[derive(Debug)]
struct DirEntry {
    name: String,
    object_type: u8,
    start: u32,
    size: u64,
}

/// A parsed OLE2 / Compound File Binary container.
///
/// Owns a private copy of the input bytes plus the FAT, MiniFAT, the root
/// mini-stream and the flat directory entry list. Streams are materialised on
/// demand by [`Cfb::stream`].
pub struct Cfb {
    data: Vec<u8>,
    sector_size: usize,
    mini_sector_size: usize,
    mini_cutoff: u64,
    fat: Vec<u32>,
    mini_fat: Vec<u32>,
    mini_stream: Vec<u8>,
    entries: Vec<DirEntry>,
}

impl Cfb {
    /// Returns `None` when `data` is not a CFB container at all (no magic).
    ///
    /// A container whose magic is present but which cannot be parsed returns
    /// `Some(Err(..))` so the caller can tell "not CFB" from "broken CFB".
    pub fn parse(data: &[u8]) -> Option<Result<Cfb, CfbError>> {
        if data.len() < 8 || data[..8] != MAGIC {
            return None;
        }
        Some(parse_inner(data))
    }

    /// Names of every stream found in the directory (root storage included).
    pub fn stream_names(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for e in &self.entries {
            if e.object_type == 2 && seen.insert(e.name.clone()) {
                out.push(e.name.clone());
            }
        }
        out
    }

    /// Full contents of a named stream, walking FAT or MiniFAT as appropriate.
    ///
    /// Returns `None` for missing names, non-stream entries, and any stream
    /// whose chain is corrupt (out-of-range sector, cycle, or a declared size
    /// the file cannot back). A size that exceeds what the chain actually holds
    /// is capped to the available bytes; it never allocates the claimed size.
    pub fn stream(&self, name: &str) -> Option<Vec<u8>> {
        let e = self.find_stream(name)?;
        if e.size == 0 {
            return Some(Vec::new());
        }
        if e.size < self.mini_cutoff {
            self.read_mini(e)
        } else {
            self.read_full(e)
        }
    }

    /// Classify by which streams are present.
    pub fn kind(&self) -> CfbKind {
        if self.find_stream("EncryptionInfo").is_some() {
            return CfbKind::EncryptedOoxml;
        }
        if self.find_stream("Workbook").is_some() || self.find_stream("Book").is_some() {
            return CfbKind::LegacyBiff;
        }
        CfbKind::Unknown
    }

    fn find_stream(&self, name: &str) -> Option<&DirEntry> {
        self.entries
            .iter()
            .find(|e| e.object_type == 2 && e.name == name)
            .or_else(|| {
                self.entries
                    .iter()
                    .find(|e| e.object_type == 2 && e.name.eq_ignore_ascii_case(name))
            })
    }

    fn num_sectors(&self) -> usize {
        self.data.len() / self.sector_size
    }

    /// Read a stream stored in full sectors addressed by the main FAT.
    fn read_full(&self, e: &DirEntry) -> Option<Vec<u8>> {
        let chain = collect_chain(&self.fat, e.start, self.num_sectors()).ok()?;
        if chain.is_empty() {
            return None;
        }
        let avail = chain.len().saturating_mul(self.sector_size);
        let cap = (e.size as usize).min(avail);
        let mut out = Vec::with_capacity(cap);
        for sid in chain {
            let off = (sid as usize + 1).checked_mul(self.sector_size)?;
            let sec = self.data.get(off..off + self.sector_size)?;
            out.extend_from_slice(sec);
        }
        out.truncate(cap);
        Some(out)
    }

    /// Read a stream stored as mini-sectors inside the root mini-stream.
    fn read_mini(&self, e: &DirEntry) -> Option<Vec<u8>> {
        if self.mini_stream.is_empty() {
            return None;
        }
        let chain = collect_chain(&self.mini_fat, e.start, self.mini_fat.len()).ok()?;
        if chain.is_empty() {
            return None;
        }
        let avail = chain.len().saturating_mul(self.mini_sector_size);
        let cap = (e.size as usize).min(avail);
        let mut out = Vec::with_capacity(cap);
        for msid in chain {
            let off = msid as usize * self.mini_sector_size;
            if off >= self.mini_stream.len() {
                break;
            }
            let end = (off + self.mini_sector_size).min(self.mini_stream.len());
            out.extend_from_slice(&self.mini_stream[off..end]);
        }
        out.truncate(cap);
        Some(out)
    }
}

/// Parse the header, DIFAT, FAT, directory, mini-stream and MiniFAT.
fn parse_inner(data: &[u8]) -> Result<Cfb, CfbError> {
    if data.len() < HEADER_SIZE {
        return Err(CfbError::Truncated);
    }

    let major = u16_at(data, 26)?;
    if major != 3 && major != 4 {
        return Err(CfbError::BadHeader);
    }
    let sector_shift = u16_at(data, 30)?;
    if !(9..=24).contains(&sector_shift) {
        return Err(CfbError::BadHeader);
    }
    let sector_size = 1usize << sector_shift;
    let num_sectors = data.len() / sector_size;
    if num_sectors == 0 {
        return Err(CfbError::BadHeader);
    }
    let mini_shift = u16_at(data, 32)?;
    if mini_shift > 24 {
        return Err(CfbError::BadHeader);
    }
    let mini_sector_size = 1usize << mini_shift;
    let mini_cutoff = u32_at(data, 56)? as u64;

    let first_dir = u32_at(data, 48)?;
    let first_mini_fat = u32_at(data, 60)?;
    let first_difat = u32_at(data, 68)?;
    let per = sector_size / 4;

    // DIFAT: the 109 header slots plus any chained DIFAT sectors.
    let mut difat: Vec<u32> = Vec::with_capacity(128);
    for i in 0..109 {
        difat.push(u32_at(data, 76 + i * 4)?);
    }
    let mut cur = first_difat;
    let mut seen = HashSet::new();
    while cur < MAXREGSECT {
        if cur as usize >= num_sectors {
            return Err(CfbError::BadFat);
        }
        if !seen.insert(cur) {
            return Err(CfbError::CycleDetected);
        }
        let off = (cur as usize + 1)
            .checked_mul(sector_size)
            .ok_or(CfbError::BadFat)?;
        for j in 0..per - 1 {
            difat.push(u32_at(data, off + j * 4)?);
        }
        cur = u32_at(data, off + (per - 1) * 4)?;
    }

    // FAT: one entry per sector, laid out across the FAT sectors in DIFAT
    // order. A valid file has at most one FAT sector per file sector, so the
    // build is bounded and the FAT array never exceeds the file size.
    let mut fat: Vec<u32> = Vec::new();
    let mut fat_sectors_collected = 0usize;
    for fs in difat {
        if fs >= MAXREGSECT {
            continue;
        }
        if fs as usize >= num_sectors {
            return Err(CfbError::BadFat);
        }
        if fat_sectors_collected >= num_sectors {
            return Err(CfbError::BadFat);
        }
        fat_sectors_collected += 1;
        let off = (fs as usize + 1)
            .checked_mul(sector_size)
            .ok_or(CfbError::BadFat)?;
        for j in 0..per {
            fat.push(u32_at(data, off + j * 4)?);
        }
    }

    // Directory: a FAT chain of 128-byte entries.
    if first_dir == 0 || first_dir >= MAXREGSECT {
        return Err(CfbError::BadHeader);
    }
    let dir_chain = collect_chain(&fat, first_dir, num_sectors)?;
    let dir_bytes = read_chain_bytes(data, sector_size, &dir_chain)?;

    let mut entries: Vec<DirEntry> = Vec::new();
    let mut root: Option<usize> = None;
    let mut off = 0usize;
    while off + DIR_ENTRY_SIZE <= dir_bytes.len() {
        if let Some(e) = parse_dir_entry(&dir_bytes, off)? {
            if e.object_type == 5 {
                root = Some(entries.len());
            }
            entries.push(e);
        }
        off += DIR_ENTRY_SIZE;
    }

    // Root entry's own stream is the mini-stream holding all small streams.
    let mut mini_stream: Vec<u8> = Vec::new();
    if let Some(ri) = root {
        let r = &entries[ri];
        if r.size > 0 && r.start != 0 && r.start < MAXREGSECT {
            let chain = collect_chain(&fat, r.start, num_sectors)?;
            let avail = chain.len().saturating_mul(sector_size) as u64;
            let cap = r.size.min(avail) as usize;
            let mut buf = Vec::with_capacity(cap);
            for sid in chain {
                let off = (sid as usize + 1)
                    .checked_mul(sector_size)
                    .ok_or(CfbError::BadFat)?;
                let sec = data.get(off..off + sector_size).ok_or(CfbError::BadFat)?;
                buf.extend_from_slice(sec);
            }
            buf.truncate(cap);
            mini_stream = buf;
        }
    }

    // MiniFAT: a FAT chain of 4-byte mini-sector pointers.
    let mut mini_fat: Vec<u32> = Vec::new();
    if first_mini_fat != 0 && first_mini_fat < MAXREGSECT {
        let mf_chain = collect_chain(&fat, first_mini_fat, num_sectors)?;
        for sid in mf_chain {
            let off = (sid as usize + 1)
                .checked_mul(sector_size)
                .ok_or(CfbError::BadFat)?;
            for j in 0..per {
                mini_fat.push(u32_at(data, off + j * 4)?);
            }
        }
    }

    // Reject stream size declarations the file can never back. This is a coarse
    // upper bound: full streams cannot exceed the file itself, mini streams
    // cannot exceed the mini-stream buffer.
    for e in &entries {
        if e.object_type != 2 {
            continue;
        }
        let available = if e.size < mini_cutoff {
            mini_stream.len() as u64
        } else {
            data.len() as u64
        };
        if e.size > available {
            return Err(CfbError::StreamTooLarge);
        }
    }

    Ok(Cfb {
        data: data.to_vec(),
        sector_size,
        mini_sector_size,
        mini_cutoff,
        fat,
        mini_fat,
        mini_stream,
        entries,
    })
}

/// Decode one 128-byte directory entry; `None` for unallocated entries.
fn parse_dir_entry(dir: &[u8], off: usize) -> Result<Option<DirEntry>, CfbError> {
    let slot = dir
        .get(off..off + DIR_ENTRY_SIZE)
        .ok_or(CfbError::Truncated)?;
    let object_type = slot[66];
    if object_type == 0 {
        return Ok(None);
    }
    let name_len = u16_at(dir, off + 64)? as usize;
    if !(2..=64).contains(&name_len) {
        return Ok(None);
    }
    let mut len = name_len;
    #[allow(clippy::manual_is_multiple_of)]
    if len % 2 != 0 {
        len -= 1;
    }
    let mut units: Vec<u16> = Vec::with_capacity(len / 2);
    for ch in slot[..len].chunks_exact(2) {
        units.push(u16::from_le_bytes([ch[0], ch[1]]));
    }
    if let Some(n) = units.iter().position(|&u| u == 0) {
        units.truncate(n);
    }
    let name = String::from_utf16_lossy(&units);
    let start = u32_at(dir, off + 116)?;
    let size = u64_at(dir, off + 120)?;
    Ok(Some(DirEntry {
        name,
        object_type,
        start,
        size,
    }))
}

/// Walk a FAT/MiniFAT chain from `start`, returning sector ids in order.
///
/// `max_sectors` bounds the walk (the file's sector count for the FAT, the
/// MiniFAT length for the mini-stream). A chain that outgrows the bound must
/// have revisited a sector, so it is reported as a cycle. Any sector id that
/// is out of range of the file or of the FAT is `BadFat`.
fn collect_chain(fat: &[u32], start: u32, max_sectors: usize) -> Result<Vec<u32>, CfbError> {
    let mut chain = Vec::new();
    let mut cur = start;
    while cur < MAXREGSECT {
        if cur as usize >= max_sectors {
            return Err(CfbError::BadFat);
        }
        if cur as usize >= fat.len() {
            return Err(CfbError::BadFat);
        }
        if chain.len() >= max_sectors {
            return Err(CfbError::CycleDetected);
        }
        chain.push(cur);
        let next = fat[cur as usize];
        if next >= MAXREGSECT {
            break;
        }
        cur = next;
    }
    Ok(chain)
}

/// Concatenate the full sectors named by `chain`.
fn read_chain_bytes(data: &[u8], sector_size: usize, chain: &[u32]) -> Result<Vec<u8>, CfbError> {
    let mut out = Vec::with_capacity(chain.len().saturating_mul(sector_size));
    for sid in chain {
        let off = (*sid as usize + 1)
            .checked_mul(sector_size)
            .ok_or(CfbError::BadFat)?;
        let sec = data.get(off..off + sector_size).ok_or(CfbError::BadFat)?;
        out.extend_from_slice(sec);
    }
    Ok(out)
}

#[inline]
fn u16_at(data: &[u8], off: usize) -> Result<u16, CfbError> {
    let b = data.get(off..off + 2).ok_or(CfbError::Truncated)?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

#[inline]
fn u32_at(data: &[u8], off: usize) -> Result<u32, CfbError> {
    let b = data.get(off..off + 4).ok_or(CfbError::Truncated)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

#[inline]
fn u64_at(data: &[u8], off: usize) -> Result<u64, CfbError> {
    let b = data.get(off..off + 8).ok_or(CfbError::Truncated)?;
    Ok(u64::from_le_bytes([
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::collections::HashMap;

    const FREESECT: u32 = 0xFFFFFFFF;
    const ENDOFCHAIN: u32 = 0xFFFFFFFE;
    const FATSECT: u32 = 0xFFFFFFFD;

    fn put_u16(b: &mut [u8], off: usize, v: u16) {
        b[off] = (v & 0xFF) as u8;
        b[off + 1] = (v >> 8) as u8;
    }

    fn put_u32(b: &mut [u8], off: usize, v: u32) {
        b[off] = (v & 0xFF) as u8;
        b[off + 1] = ((v >> 8) & 0xFF) as u8;
        b[off + 2] = ((v >> 16) & 0xFF) as u8;
        b[off + 3] = ((v >> 24) & 0xFF) as u8;
    }

    fn put_u64(b: &mut [u8], off: usize, v: u64) {
        put_u32(b, off, v as u32);
        put_u32(b, off + 4, (v >> 32) as u32);
    }

    struct StreamSpec {
        name: &'static str,
        data: Vec<u8>,
    }

    impl StreamSpec {
        fn new(name: &'static str, data: Vec<u8>) -> Self {
            StreamSpec { name, data }
        }
    }

    /// A built container plus enough layout knowledge to corrupt it in tests.
    struct Built {
        bytes: Vec<u8>,
        sector_size: usize,
        dir_first: u32,
        dir_sectors: Vec<u32>,
        chain: HashMap<String, Vec<u32>>,
        stream_slot: HashMap<String, usize>,
    }

    impl Built {
        fn fat_entry_off(&self, sector: u32) -> usize {
            self.sector_size + sector as usize * 4
        }

        fn patch_fat(&mut self, sector: u32, val: u32) {
            let off = self.fat_entry_off(sector);
            put_u32(&mut self.bytes, off, val);
        }

        fn dir_entry_off(&self, slot: usize) -> usize {
            (self.dir_first as usize + 1) * self.sector_size + slot * 128
        }

        fn patch_stream_size(&mut self, name: &str, size: u64) {
            let slot = self.stream_slot[name];
            let off = self.dir_entry_off(slot) + 120;
            put_u64(&mut self.bytes, off, size);
        }

        fn patch_stream_start(&mut self, name: &str, start: u32) {
            let slot = self.stream_slot[name];
            let off = self.dir_entry_off(slot) + 116;
            put_u32(&mut self.bytes, off, start);
        }
    }

    /// Grow-only padding: push zeros until `buf.len() >= target`. Unlike
    /// `Vec::resize` this never truncates.
    fn pad_to(buf: &mut Vec<u8>, target: usize) {
        if buf.len() < target {
            buf.resize(target, 0);
        }
    }

    /// Build a minimal valid CFB file from stream specs.
    /// Streams smaller than `mini_cutoff` go through the MiniFAT (root
    /// mini-stream); the rest go through the main FAT. Sector 0 is the header,
    /// followed by FAT sectors, the directory, the MiniFAT, the root
    /// mini-stream and finally the full-stream data.
    fn build_cfb(sector_shift: u8, mini_cutoff: u32, streams: Vec<StreamSpec>) -> Built {
        let sector_size = 1usize << sector_shift;
        let mini_size = 1usize << 6;
        let per = sector_size / 4;

        let mut mini_buf: Vec<u8> = Vec::new();
        let mut mini_meta: Vec<(String, u32, u64)> = Vec::new();
        let mut full: Vec<(String, Vec<u8>)> = Vec::new();
        for s in streams {
            if (s.data.len() as u64) < mini_cutoff as u64 {
                let start = (mini_buf.len() / mini_size) as u32;
                mini_buf.extend_from_slice(&s.data);
                while mini_buf.len() % mini_size != 0 {
                    mini_buf.push(0);
                }
                mini_meta.push((s.name.to_string(), start, s.data.len() as u64));
            } else {
                full.push((s.name.to_string(), s.data));
            }
        }
        let mini_sector_count = mini_buf.len() / mini_size;
        let mut mini_fat: Vec<u32> = (0..mini_sector_count)
            .map(|i| {
                if i + 1 < mini_sector_count {
                    (i + 1) as u32
                } else {
                    ENDOFCHAIN
                }
            })
            .collect();
        while mini_fat.len() % per != 0 {
            mini_fat.push(FREESECT);
        }

        let dir_entries_count = 1 + mini_meta.len() + full.len();
        let d = (dir_entries_count * 128).div_ceil(sector_size);
        let m = (mini_fat.len() * 4).div_ceil(sector_size);
        let r = mini_buf.len().div_ceil(sector_size);
        let full_sectors: Vec<usize> = full
            .iter()
            .map(|(_, data)| data.len().div_ceil(sector_size))
            .collect();
        // Sector 0 is the first sector after the 512-byte header; the FAT
        // occupies the first `f` sectors, then directory, MiniFAT, the root
        // mini-stream and the full-stream data.
        let base = d + m + r + full_sectors.iter().sum::<usize>();

        let mut f = 1usize;
        loop {
            let f2 = (base + f).div_ceil(per);
            if f2 == f {
                break;
            }
            f = f2;
        }
        let total = base + f;

        let dir_start = f;
        let minifat_start = dir_start + d;
        let root_start = minifat_start + m;
        let full_start = root_start + r;

        let mut fat: Vec<u32> = vec![FREESECT; f * per];
        fat[..f].fill(FATSECT);
        let dir_sectors: Vec<u32> = (0..d).map(|i| (dir_start + i) as u32).collect();
        let mf_sectors: Vec<u32> = (0..m).map(|i| (minifat_start + i) as u32).collect();
        let root_sectors: Vec<u32> = (0..r).map(|i| (root_start + i) as u32).collect();
        for (i, &s) in dir_sectors.iter().enumerate() {
            fat[s as usize] = if i + 1 < d {
                dir_sectors[i + 1]
            } else {
                ENDOFCHAIN
            };
        }
        for (i, &s) in mf_sectors.iter().enumerate() {
            fat[s as usize] = if i + 1 < m {
                mf_sectors[i + 1]
            } else {
                ENDOFCHAIN
            };
        }
        for (i, &s) in root_sectors.iter().enumerate() {
            fat[s as usize] = if i + 1 < r {
                root_sectors[i + 1]
            } else {
                ENDOFCHAIN
            };
        }
        let mut full_chains: Vec<Vec<u32>> = Vec::new();
        let mut cur = full_start as u32;
        for (_, data) in &full {
            let n = data.len().div_ceil(sector_size);
            let mut ch = Vec::with_capacity(n);
            for j in 0..n {
                let s = cur + j as u32;
                ch.push(s);
                fat[s as usize] = if j + 1 < n {
                    cur + (j + 1) as u32
                } else {
                    ENDOFCHAIN
                };
            }
            cur += n as u32;
            full_chains.push(ch);
        }

        let mut hdr = vec![0u8; HEADER_SIZE];
        hdr[..8].copy_from_slice(&MAGIC);
        put_u16(&mut hdr, 24, 0x003E);
        put_u16(&mut hdr, 26, if sector_shift == 12 { 4 } else { 3 });
        put_u16(&mut hdr, 28, 0xFFFE);
        put_u16(&mut hdr, 30, sector_shift as u16);
        put_u16(&mut hdr, 32, 6);
        put_u32(&mut hdr, 40, 0);
        put_u32(&mut hdr, 44, f as u32);
        put_u32(&mut hdr, 48, dir_start as u32);
        put_u32(&mut hdr, 52, 0);
        put_u32(&mut hdr, 56, mini_cutoff);
        put_u32(
            &mut hdr,
            60,
            if m > 0 {
                minifat_start as u32
            } else {
                ENDOFCHAIN
            },
        );
        put_u32(&mut hdr, 64, m as u32);
        put_u32(&mut hdr, 68, ENDOFCHAIN);
        put_u32(&mut hdr, 72, 0);
        for i in 0..109 {
            let v = if i < f { i as u32 } else { FREESECT };
            put_u32(&mut hdr, 76 + i * 4, v);
        }

        let mut dirb = vec![0u8; d * sector_size];
        {
            let name = "Root Entry";
            let units: Vec<u16> = name.encode_utf16().collect();
            put_u16(&mut dirb, 64, (units.len() as u16 + 1) * 2);
            for (i, c) in units.iter().enumerate() {
                dirb[i * 2] = (c & 0xFF) as u8;
                dirb[i * 2 + 1] = (c >> 8) as u8;
            }
            dirb[66] = 5;
            dirb[67] = 1;
            put_u32(&mut dirb, 76, 1);
            put_u32(
                &mut dirb,
                116,
                if r > 0 { root_start as u32 } else { ENDOFCHAIN },
            );
            put_u64(&mut dirb, 120, mini_buf.len() as u64);
        }

        let mut all_streams: Vec<(String, u32, u64)> = Vec::new();
        for (name, start, size) in mini_meta {
            all_streams.push((name, start, size));
        }
        for (i, (name, data)) in full.iter().enumerate() {
            let start = full_chains[i][0];
            all_streams.push((name.clone(), start, data.len() as u64));
        }

        let mut stream_slot = HashMap::new();
        let mut chain = HashMap::new();
        let n = all_streams.len();
        for (i, (name, start, size)) in all_streams.iter().enumerate() {
            let slot = 1 + i;
            let right = if i + 1 < n {
                (slot + 1) as u32
            } else {
                FREESECT
            };
            let off = slot * 128;
            let units: Vec<u16> = name.encode_utf16().collect();
            put_u16(&mut dirb, off + 64, (units.len() as u16 + 1) * 2);
            for (j, c) in units.iter().enumerate() {
                dirb[off + j * 2] = (c & 0xFF) as u8;
                dirb[off + j * 2 + 1] = (c >> 8) as u8;
            }
            dirb[off + 66] = 2;
            dirb[off + 67] = 1;
            put_u32(&mut dirb, off + 68, FREESECT);
            put_u32(&mut dirb, off + 72, right);
            put_u32(&mut dirb, off + 76, FREESECT);
            put_u32(&mut dirb, off + 116, *start);
            put_u64(&mut dirb, off + 120, *size);
            stream_slot.insert(name.clone(), slot);
        }
        for (i, (name, _)) in full.iter().enumerate() {
            chain.insert(name.clone(), full_chains[i].clone());
        }

        let mut fat_bytes = Vec::with_capacity(f * sector_size);
        for v in &fat {
            fat_bytes.extend_from_slice(&v.to_le_bytes());
        }

        let mut bytes = Vec::with_capacity((total + 1) * sector_size);
        bytes.extend_from_slice(&hdr);
        pad_to(&mut bytes, sector_size);
        bytes.extend_from_slice(&fat_bytes);
        bytes.extend_from_slice(&dirb);
        for v in &mini_fat {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        pad_to(&mut bytes, (minifat_start + m + 1) * sector_size);
        bytes.extend_from_slice(&mini_buf);
        pad_to(&mut bytes, (root_start + r + 1) * sector_size);
        let mut full_cur = full_start;
        for (_, data) in &full {
            bytes.extend_from_slice(data);
            full_cur += data.len().div_ceil(sector_size);
            pad_to(&mut bytes, (full_cur + 1) * sector_size);
        }

        Built {
            bytes,
            sector_size,
            dir_first: dir_start as u32,
            dir_sectors,
            chain,
            stream_slot,
        }
    }

    #[test]
    fn parse_missing_magic_returns_none() {
        assert!(Cfb::parse(&[]).is_none());
        assert!(Cfb::parse(b"x").is_none());
        assert!(Cfb::parse(b"not a compound file at all").is_none());
        let b = build_cfb(9, 4096, vec![StreamSpec::new("S", b"hi".to_vec())]);
        let mut bad = b.bytes.clone();
        bad[0] ^= 0xFF;
        assert!(Cfb::parse(&bad).is_none());
    }

    #[test]
    fn truncated_file_returns_truncated() {
        let b = build_cfb(
            9,
            4096,
            vec![StreamSpec::new("EncryptionInfo", b"x".to_vec())],
        );
        assert!(matches!(
            Cfb::parse(&b.bytes[..100]),
            Some(Err(CfbError::Truncated))
        ));
        assert!(matches!(
            Cfb::parse(&b.bytes[..300]),
            Some(Err(CfbError::Truncated))
        ));
    }

    #[test]
    fn small_stream_reads_through_minifat() {
        let data = b"tiny encrypted info".to_vec();
        let b = build_cfb(
            9,
            4096,
            vec![StreamSpec::new("EncryptionInfo", data.clone())],
        );
        let cfb = Cfb::parse(&b.bytes).expect("parse").expect("ok");
        assert_eq!(cfb.stream_names(), vec!["EncryptionInfo".to_string()]);
        assert_eq!(cfb.stream("EncryptionInfo"), Some(data));
        assert_eq!(cfb.kind(), CfbKind::EncryptedOoxml);
    }

    #[test]
    fn large_stream_reads_through_fat() {
        let data: Vec<u8> = (0..10000u32).map(|i| (i % 251) as u8).collect();
        let b = build_cfb(
            9,
            4096,
            vec![StreamSpec::new("EncryptedPackage", data.clone())],
        );
        let cfb = Cfb::parse(&b.bytes).expect("parse").expect("ok");
        assert_eq!(cfb.stream("EncryptedPackage"), Some(data.clone()));
        assert_eq!(cfb.kind(), CfbKind::Unknown);
        assert_eq!(cfb.stream("EncryptionInfo"), None);
    }

    #[test]
    fn mixed_mini_and_full_streams() {
        let small = b"small".to_vec();
        let big: Vec<u8> = (0..5000u32).map(|i| (i % 97) as u8).collect();
        let b = build_cfb(
            9,
            4096,
            vec![
                StreamSpec::new("EncryptionInfo", small.clone()),
                StreamSpec::new("EncryptedPackage", big.clone()),
            ],
        );
        let cfb = Cfb::parse(&b.bytes).expect("parse").expect("ok");
        assert_eq!(cfb.stream("EncryptionInfo"), Some(small));
        assert_eq!(cfb.stream("EncryptedPackage"), Some(big));
        assert_eq!(cfb.kind(), CfbKind::EncryptedOoxml);
        let names = cfb.stream_names();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"EncryptionInfo".to_string()));
        assert!(names.contains(&"EncryptedPackage".to_string()));
    }

    #[test]
    fn sector_size_4096() {
        let small = b"v4 mini stream".to_vec();
        let big: Vec<u8> = (0..20000u32).map(|i| (i % 253) as u8).collect();
        let b = build_cfb(
            12,
            4096,
            vec![
                StreamSpec::new("EncryptionInfo", small.clone()),
                StreamSpec::new("EncryptedPackage", big.clone()),
            ],
        );
        assert_eq!(b.bytes.len() % 4096, 0);
        let cfb = Cfb::parse(&b.bytes).expect("parse").expect("ok");
        assert_eq!(cfb.stream("EncryptionInfo"), Some(small));
        assert_eq!(cfb.stream("EncryptedPackage"), Some(big));
        assert_eq!(cfb.kind(), CfbKind::EncryptedOoxml);
    }

    #[test]
    fn circular_fat_chain_terminates() {
        let big: Vec<u8> = (0..10000u32).map(|i| (i % 101) as u8).collect();
        let mut b = build_cfb(9, 4096, vec![StreamSpec::new("Big", big.clone())]);
        let chain = b.chain["Big"].clone();
        assert!(chain.len() >= 2);
        b.patch_fat(chain[chain.len() - 1], chain[0]);
        let cfb = Cfb::parse(&b.bytes).expect("parse").expect("ok");
        assert_eq!(cfb.stream("Big"), None);
    }

    #[test]
    fn circular_directory_chain_returns_cycle() {
        let mut b = build_cfb(9, 4096, vec![StreamSpec::new("S", b"x".to_vec())]);
        let ds = b.dir_sectors.clone();
        assert!(!ds.is_empty());
        b.patch_fat(ds[ds.len() - 1], ds[0]);
        assert!(matches!(
            Cfb::parse(&b.bytes),
            Some(Err(CfbError::CycleDetected))
        ));
    }

    #[test]
    fn sector_index_out_of_range() {
        let mut b = build_cfb(9, 4096, vec![StreamSpec::new("Big", vec![0u8; 6000])]);
        let start = (b.bytes.len() / b.sector_size) as u32 + 5;
        b.patch_stream_start("Big", start);
        let cfb = Cfb::parse(&b.bytes).expect("parse").expect("ok");
        assert_eq!(cfb.stream("Big"), None);
    }

    #[test]
    fn declared_size_larger_than_file() {
        let mut b = build_cfb(9, 4096, vec![StreamSpec::new("Big", vec![0u8; 6000])]);
        b.patch_stream_size("Big", 1 << 40);
        assert!(matches!(
            Cfb::parse(&b.bytes),
            Some(Err(CfbError::StreamTooLarge))
        ));
    }

    #[test]
    fn declared_mini_size_larger_than_mini_stream() {
        let mut b = build_cfb(
            9,
            4096,
            vec![StreamSpec::new("EncryptionInfo", b"ab".to_vec())],
        );
        b.patch_stream_size("EncryptionInfo", 500);
        assert!(matches!(
            Cfb::parse(&b.bytes),
            Some(Err(CfbError::StreamTooLarge))
        ));
    }

    #[test]
    fn classifies_every_kind() {
        let ooxml = build_cfb(
            9,
            4096,
            vec![
                StreamSpec::new("EncryptionInfo", b"ei".to_vec()),
                StreamSpec::new("EncryptedPackage", b"ep".to_vec()),
            ],
        );
        assert_eq!(
            Cfb::parse(&ooxml.bytes).expect("parse").expect("ok").kind(),
            CfbKind::EncryptedOoxml
        );

        let workbook = build_cfb(9, 4096, vec![StreamSpec::new("Workbook", b"w".to_vec())]);
        assert_eq!(
            Cfb::parse(&workbook.bytes)
                .expect("parse")
                .expect("ok")
                .kind(),
            CfbKind::LegacyBiff
        );

        let book = build_cfb(9, 4096, vec![StreamSpec::new("Book", b"b".to_vec())]);
        assert_eq!(
            Cfb::parse(&book.bytes).expect("parse").expect("ok").kind(),
            CfbKind::LegacyBiff
        );

        let unknown = build_cfb(9, 4096, vec![StreamSpec::new("Whatever", b"x".to_vec())]);
        assert_eq!(
            Cfb::parse(&unknown.bytes)
                .expect("parse")
                .expect("ok")
                .kind(),
            CfbKind::Unknown
        );
    }

    #[test]
    fn stream_name_lookup_is_case_insensitive_fallback() {
        let b = build_cfb(
            9,
            4096,
            vec![StreamSpec::new("EncryptionInfo", b"data".to_vec())],
        );
        let cfb = Cfb::parse(&b.bytes).expect("parse").expect("ok");
        assert_eq!(cfb.stream("ENCRYPTIONINFO"), Some(b"data".to_vec()));
    }
}
