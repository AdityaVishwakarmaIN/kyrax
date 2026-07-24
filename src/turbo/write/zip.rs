//! Hand-assembled ZIP (local headers + central directory + EOCD).
//! DEFLATE via libdeflater; Zip64 when sizes/offsets exceed u32.
//!
//! Read path has zipmin (inflate-only). Compression path lives here;
//! dedup of CRC/constants deferred — see W1_REPORT.md.
//!
//! Write-through: each `add`/`add_buf`/`add_recycle` deflates immediately and
//! retains only compressed bytes. Compress scratch is thread-local so capacity
//! is reused across parts and across successive workbook writes in-process.

use libdeflater::{CompressionLvl, Compressor};
use std::cell::RefCell;
use std::io::{self, Write};

const SIG_LOCAL: u32 = 0x0403_4b50;
const SIG_CENTRAL: u32 = 0x0201_4b50;
const SIG_EOCD: u32 = 0x0605_4b50;
const SIG_ZIP64_EOCD: u32 = 0x0606_4b50;
const SIG_ZIP64_LOCATOR: u32 = 0x0706_4b50;
const METHOD_STORE: u16 = 0;
const METHOD_DEFLATE: u16 = 8;

// Retain moderate compress scratch for reuse; release only oversized outliers
// so long-lived workers do not pin hundreds of MB after a giant write.
const SCRATCH_RETAIN_MAX: usize = 64 * 1024 * 1024;

thread_local! {
    /// Reused deflate output scratch (capacity retained across writes).
    static COMP_SCRATCH: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Free this thread's COMP_SCRATCH if it exceeds SCRATCH_RETAIN_MAX.
/// Call on the main thread after a write, and via rayon::broadcast for workers
/// when the multi-sheet parallel path ran.
pub(crate) fn shrink_comp_scratch() {
    COMP_SCRATCH.with(|cell| {
        let mut scratch = cell.borrow_mut();
        if scratch.capacity() > SCRATCH_RETAIN_MAX {
            *scratch = Vec::new();
        }
    });
}

struct Entry {
    name: String,
    method: u16,
    crc32: u32,
    comp_size: u64,
    uncomp_size: u64,
    local_offset: u64,
    data: Vec<u8>,
}

/// Pre-deflated zip part (CRC + method + sizes + payload). Built in parallel
/// workers; appended to [`ZipWriter`] serially to keep local-file order fixed.
pub struct PrecompressedPart {
    pub name: String,
    pub method: u16,
    pub crc32: u32,
    pub uncomp_size: u64,
    pub data: Vec<u8>,
}

/// Deflate (or store) one buffer with the same level-6 policy as [`ZipWriter`].
/// Thread-safe: uses thread-local compress scratch.
pub fn compress_part(name: String, data: &[u8]) -> PrecompressedPart {
    let crc32 = crc32_ieee(data);
    let uncomp_size = data.len() as u64;
    let level = CompressionLvl::new(6).unwrap_or_else(|_| CompressionLvl::default());
    let (method, payload) = deflate_or_store_level(data, level);
    PrecompressedPart {
        name,
        method,
        crc32,
        uncomp_size,
        data: payload,
    }
}

fn deflate_or_store_level(data: &[u8], level: CompressionLvl) -> (u16, Vec<u8>) {
    let uncomp_size = data.len() as u64;
    let bound = compress_bound(data.len());
    COMP_SCRATCH.with(|cell| {
        let mut scratch = cell.borrow_mut();
        if scratch.len() < bound {
            scratch.resize(bound, 0);
        }
        let mut compressor = Compressor::new(level);
        let n = compressor
            .deflate_compress(data, &mut scratch[..bound])
            .unwrap_or(0);
        if n > 0 && (n as u64) < uncomp_size {
            (METHOD_DEFLATE, scratch[..n].to_vec())
        } else {
            (METHOD_STORE, data.to_vec())
        }
    })
}

pub struct ZipWriter {
    entries: Vec<Entry>,
    level: CompressionLvl,
}

impl ZipWriter {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            level: CompressionLvl::new(6).unwrap_or_else(|_| CompressionLvl::default()),
        }
    }

    pub fn with_level(level: i32) -> Self {
        let level = CompressionLvl::new(level).unwrap_or_else(|_| CompressionLvl::default());
        Self {
            entries: Vec::new(),
            level,
        }
    }

    /// Add a part. Deflates when compressed size wins; otherwise stores.
    pub fn add(&mut self, name: &str, data: &[u8]) {
        let crc32 = crc32_ieee(data);
        let uncomp_size = data.len() as u64;
        let (method, payload) = self.deflate_or_store(data);
        self.entries.push(Entry {
            name: name.to_string(),
            method,
            crc32,
            comp_size: payload.len() as u64,
            uncomp_size,
            local_offset: 0,
            data: payload,
        });
    }

    /// Like [`add`], but takes ownership of the uncompressed buffer so it is
    /// dropped as soon as deflate finishes (STORE path reuses the Vec).
    pub fn add_buf(&mut self, name: &str, data: Vec<u8>) {
        let crc32 = crc32_ieee(&data);
        let uncomp_size = data.len() as u64;
        let (method, payload) = self.deflate_or_store_owned(data);
        self.entries.push(Entry {
            name: name.to_string(),
            method,
            crc32,
            comp_size: payload.len() as u64,
            uncomp_size,
            local_offset: 0,
            data: payload,
        });
    }

    /// Deflate from `data`, then `clear()` it (capacity kept for the caller to
    /// reuse as a scratch buffer across parts/sheets).
    pub fn add_recycle(&mut self, name: &str, data: &mut Vec<u8>) {
        let crc32 = crc32_ieee(data);
        let uncomp_size = data.len() as u64;
        let (method, payload) = self.deflate_or_store(data);
        data.clear();
        self.entries.push(Entry {
            name: name.to_string(),
            method,
            crc32,
            comp_size: payload.len() as u64,
            uncomp_size,
            local_offset: 0,
            data: payload,
        });
    }

    /// Force-store (no compression).
    pub fn add_stored(&mut self, name: &str, data: &[u8]) {
        self.entries.push(Entry {
            name: name.to_string(),
            method: METHOD_STORE,
            crc32: crc32_ieee(data),
            comp_size: data.len() as u64,
            uncomp_size: data.len() as u64,
            local_offset: 0,
            data: data.to_vec(),
        });
    }

    /// Append a part that was already CRC'd + deflated off-thread (P4).
    /// Zip local-file order is determined solely by call order here.
    pub fn add_precompressed(&mut self, part: PrecompressedPart) {
        self.entries.push(Entry {
            name: part.name,
            method: part.method,
            crc32: part.crc32,
            comp_size: part.data.len() as u64,
            uncomp_size: part.uncomp_size,
            local_offset: 0,
            data: part.data,
        });
    }

    fn deflate_or_store(&self, data: &[u8]) -> (u16, Vec<u8>) {
        deflate_or_store_level(data, self.level)
    }

    fn deflate_or_store_owned(&self, data: Vec<u8>) -> (u16, Vec<u8>) {
        let uncomp_size = data.len() as u64;
        let bound = compress_bound(data.len());
        let level = self.level;
        COMP_SCRATCH.with(|cell| {
            let mut scratch = cell.borrow_mut();
            if scratch.len() < bound {
                scratch.resize(bound, 0);
            }
            let mut compressor = Compressor::new(level);
            let n = compressor
                .deflate_compress(&data, &mut scratch[..bound])
                .unwrap_or(0);
            if n > 0 && (n as u64) < uncomp_size {
                // uncompressed `data` drops at end of with-closure / function
                (METHOD_DEFLATE, scratch[..n].to_vec())
            } else {
                (METHOD_STORE, data)
            }
        })
    }

    pub fn finish(mut self) -> io::Result<Vec<u8>> {
        let mut out = Vec::with_capacity(
            self.entries
                .iter()
                .map(|e| 30 + e.name.len() + e.data.len() + 46 + e.name.len())
                .sum::<usize>()
                + 64,
        );

        for e in &mut self.entries {
            e.local_offset = out.len() as u64;
            write_local_header(&mut out, e)?;
            out.write_all(&e.data)?;
        }

        let cd_offset = out.len() as u64;
        for e in &self.entries {
            write_central_header(&mut out, e)?;
        }
        let cd_size = out.len() as u64 - cd_offset;
        let count = self.entries.len() as u64;

        let need_zip64 = count >= 0xFFFF
            || cd_size >= 0xFFFF_FFFF
            || cd_offset >= 0xFFFF_FFFF
            || self.entries.iter().any(|e| {
                e.comp_size >= 0xFFFF_FFFF
                    || e.uncomp_size >= 0xFFFF_FFFF
                    || e.local_offset >= 0xFFFF_FFFF
            });

        if need_zip64 {
            let zip64_eocd_offset = out.len() as u64;
            out.write_all(&SIG_ZIP64_EOCD.to_le_bytes())?;
            out.write_all(&44u64.to_le_bytes())?;
            out.write_all(&45u16.to_le_bytes())?;
            out.write_all(&45u16.to_le_bytes())?;
            out.write_all(&0u32.to_le_bytes())?;
            out.write_all(&0u32.to_le_bytes())?;
            out.write_all(&count.to_le_bytes())?;
            out.write_all(&count.to_le_bytes())?;
            out.write_all(&cd_size.to_le_bytes())?;
            out.write_all(&cd_offset.to_le_bytes())?;
            out.write_all(&SIG_ZIP64_LOCATOR.to_le_bytes())?;
            out.write_all(&0u32.to_le_bytes())?;
            out.write_all(&zip64_eocd_offset.to_le_bytes())?;
            out.write_all(&1u32.to_le_bytes())?;
        }

        out.write_all(&SIG_EOCD.to_le_bytes())?;
        out.write_all(&0u16.to_le_bytes())?;
        out.write_all(&0u16.to_le_bytes())?;
        let c16 = if count >= 0xFFFF {
            0xFFFF
        } else {
            count as u16
        };
        out.write_all(&c16.to_le_bytes())?;
        out.write_all(&c16.to_le_bytes())?;
        let cd_size32 = if cd_size >= 0xFFFF_FFFF {
            0xFFFF_FFFF
        } else {
            cd_size as u32
        };
        let cd_off32 = if cd_offset >= 0xFFFF_FFFF {
            0xFFFF_FFFF
        } else {
            cd_offset as u32
        };
        out.write_all(&cd_size32.to_le_bytes())?;
        out.write_all(&cd_off32.to_le_bytes())?;
        out.write_all(&0u16.to_le_bytes())?;

        Ok(out)
    }
}

impl Default for ZipWriter {
    fn default() -> Self {
        Self::new()
    }
}

fn write_local_header(out: &mut Vec<u8>, e: &Entry) -> io::Result<()> {
    out.write_all(&SIG_LOCAL.to_le_bytes())?;
    let (ver, flags, extra) = zip64_local_fields(e);
    out.write_all(&ver.to_le_bytes())?;
    out.write_all(&flags.to_le_bytes())?;
    out.write_all(&e.method.to_le_bytes())?;
    out.write_all(&0u16.to_le_bytes())?;
    out.write_all(&0u16.to_le_bytes())?;
    out.write_all(&e.crc32.to_le_bytes())?;
    let (cs, us) = if !extra.is_empty() {
        (0xFFFF_FFFFu32, 0xFFFF_FFFFu32)
    } else {
        (e.comp_size as u32, e.uncomp_size as u32)
    };
    out.write_all(&cs.to_le_bytes())?;
    out.write_all(&us.to_le_bytes())?;
    let nlen = e.name.len() as u16;
    out.write_all(&nlen.to_le_bytes())?;
    out.write_all(&(extra.len() as u16).to_le_bytes())?;
    out.write_all(e.name.as_bytes())?;
    out.write_all(&extra)?;
    Ok(())
}

fn write_central_header(out: &mut Vec<u8>, e: &Entry) -> io::Result<()> {
    out.write_all(&SIG_CENTRAL.to_le_bytes())?;
    let (ver, flags, extra) = zip64_central_fields(e);
    out.write_all(&ver.to_le_bytes())?;
    out.write_all(&ver.to_le_bytes())?;
    out.write_all(&flags.to_le_bytes())?;
    out.write_all(&e.method.to_le_bytes())?;
    out.write_all(&0u16.to_le_bytes())?;
    out.write_all(&0u16.to_le_bytes())?;
    out.write_all(&e.crc32.to_le_bytes())?;
    let use64 = !extra.is_empty();
    let cs = if use64 {
        0xFFFF_FFFFu32
    } else {
        e.comp_size as u32
    };
    let us = if use64 {
        0xFFFF_FFFFu32
    } else {
        e.uncomp_size as u32
    };
    let off = if use64 {
        0xFFFF_FFFFu32
    } else {
        e.local_offset as u32
    };
    out.write_all(&cs.to_le_bytes())?;
    out.write_all(&us.to_le_bytes())?;
    let nlen = e.name.len() as u16;
    out.write_all(&nlen.to_le_bytes())?;
    out.write_all(&(extra.len() as u16).to_le_bytes())?;
    out.write_all(&0u16.to_le_bytes())?;
    out.write_all(&0u16.to_le_bytes())?;
    out.write_all(&0u16.to_le_bytes())?;
    out.write_all(&0u32.to_le_bytes())?;
    out.write_all(&off.to_le_bytes())?;
    out.write_all(e.name.as_bytes())?;
    out.write_all(&extra)?;
    Ok(())
}

fn zip64_local_fields(e: &Entry) -> (u16, u16, Vec<u8>) {
    if e.comp_size >= 0xFFFF_FFFF || e.uncomp_size >= 0xFFFF_FFFF {
        let mut extra = Vec::with_capacity(20);
        extra.extend_from_slice(&1u16.to_le_bytes());
        extra.extend_from_slice(&16u16.to_le_bytes());
        extra.extend_from_slice(&e.uncomp_size.to_le_bytes());
        extra.extend_from_slice(&e.comp_size.to_le_bytes());
        (45, 0, extra)
    } else {
        (20, 0, Vec::new())
    }
}

fn zip64_central_fields(e: &Entry) -> (u16, u16, Vec<u8>) {
    let need = e.comp_size >= 0xFFFF_FFFF
        || e.uncomp_size >= 0xFFFF_FFFF
        || e.local_offset >= 0xFFFF_FFFF;
    if need {
        let mut extra = Vec::with_capacity(32);
        extra.extend_from_slice(&1u16.to_le_bytes());
        extra.extend_from_slice(&24u16.to_le_bytes());
        extra.extend_from_slice(&e.uncomp_size.to_le_bytes());
        extra.extend_from_slice(&e.comp_size.to_le_bytes());
        extra.extend_from_slice(&e.local_offset.to_le_bytes());
        (45, 0, extra)
    } else {
        (20, 0, Vec::new())
    }
}

fn compress_bound(n: usize) -> usize {
    n + (n / 8) + 256
}

/// IEEE CRC-32 (ZIP polynomial).
pub fn crc32_ieee(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        let idx = ((crc ^ b as u32) & 0xFF) as usize;
        crc = CRC_TABLE[idx] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

static CRC_TABLE: [u32; 256] = make_crc_table();

const fn make_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            if c & 1 != 0 {
                c = 0xEDB8_8320 ^ (c >> 1);
            } else {
                c >>= 1;
            }
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_known() {
        // CRC-32 of "123456789" is 0xCBF43926
        assert_eq!(crc32_ieee(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn zip_roundtrip_local_sig() {
        let mut z = ZipWriter::new();
        z.add("hello.txt", b"hello world");
        let bytes = z.finish().unwrap();
        assert_eq!(&bytes[0..4], &SIG_LOCAL.to_le_bytes());
        // EOCD signature near end
        let eocd = bytes.len() - 22;
        assert_eq!(&bytes[eocd..eocd + 4], &SIG_EOCD.to_le_bytes());
    }

    #[test]
    fn zip_contains_name() {
        let mut z = ZipWriter::new();
        z.add("xl/workbook.xml", br#"<?xml version="1.0"?><workbook/>"#);
        let bytes = z.finish().unwrap();
        let hay = b"xl/workbook.xml";
        assert!(bytes.windows(hay.len()).any(|w| w == hay));
    }
}
