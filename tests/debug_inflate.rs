// Bounded regression probe for manual raw-DEFLATE streaming via miniz_oxide's
// resumable core decoder, cross-checked against the zip crate's inflater.
// Intentionally uses `inflate::core::decompress` (not the stream wrapper) to
// keep the low-level path under test. Memory stays O(chunk): compressed input
// is read from the file in chunks, and output is decoded into a fixed 64 KiB
// ring and fed to a rolling hash instead of being accumulated.
use pretty_assertions::assert_eq;
use std::io::{Read as _, Seek as _, SeekFrom};
use zip::read::HasZipMetadata;

/// Compressed input is read from the file in chunks of this size.
const COMP_CHUNK: usize = 256 * 1024;
/// Power-of-two ring buffer for decoded output (>= 32 KiB deflate window).
const DICT: usize = 64 * 1024;
/// Safety budget for the streaming loop; a real worksheet ends in a handful of
/// steps, so hitting this means the decoder stopped making progress.
const MAX_STEPS: u32 = 10_000;

/// FNV-1a 64-bit rolling hash. No extra dependency, good enough to prove two
/// byte streams are identical without holding either one in memory.
#[derive(Clone, Copy, Default)]
struct Fnv(u64);

impl Fnv {
    fn update(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }
}

#[test]
fn debug_inflate() {
    let path = "testdata/mixed.xlsx";
    let file = std::fs::File::open(path).expect("open testdata/mixed.xlsx");
    let mut archive = zip::ZipArchive::new(file).expect("parse testdata/mixed.xlsx as zip");

    // find sheet entry
    let mut target = None;
    for i in 0..archive.len() {
        let f = archive.by_index(i).expect("by_index");
        let md = f.get_metadata();
        if md.file_name.starts_with("xl/worksheets/") {
            target = Some((
                i,
                md.file_name.to_string(),
                md.compression_method,
                md.compressed_size,
                md.uncompressed_size,
            ));
            break;
        }
    }
    let (idx, name, method, csize, usize_) =
        target.expect("no xl/worksheets/* entry in testdata/mixed.xlsx");
    eprintln!("entry {idx}: {name} method={method:?} csize={csize} usize={usize_}");

    assert!(usize_ > 0, "worksheet entry has empty uncompressed size");
    assert!(csize > 0, "worksheet entry is stored, not deflated");

    // reference: stream the zip crate's own inflater through a rolling hash
    let mut expected = Fnv::default();
    {
        let mut f = archive.by_index(idx).expect("reopen sheet entry");
        let mut chunk = vec![0u8; COMP_CHUNK];
        let mut total = 0u64;
        loop {
            let n = f.read(&mut chunk).expect("zip crate inflate");
            if n == 0 {
                break;
            }
            expected.update(&chunk[..n]);
            total += n as u64;
        }
        eprintln!("zip crate inflate: {total} bytes");
        assert_eq!(
            total, usize_,
            "zip crate inflated size must match entry metadata"
        );
    }

    // raw file: get data_start and stream the compressed bytes from there
    let mut raw = std::fs::File::open(path).expect("reopen testdata/mixed.xlsx");
    let off = {
        let f = archive.by_index(idx).expect("metadata by_index");
        f.get_metadata()
            .data_start(&mut raw)
            .expect("resolve data_start")
    };
    eprintln!("data_start = {off}");

    let mut manual = Fnv::default();
    let total_out =
        inflate_streaming(&mut raw, off, csize, &mut manual).expect("manual raw-deflate inflate");
    eprintln!("miniz core streaming: {total_out} bytes");
    assert_eq!(
        total_out, usize_,
        "miniz streaming inflated size must match entry metadata"
    );
    assert_eq!(
        manual.0, expected.0,
        "manual raw-deflate streaming must match zip crate output"
    );
}

/// Feed the compressed bytes at `data_start` (length `csize`) to miniz_oxide's
/// resumable core decoder in bounded chunks, hashing the decoded output.
/// Returns the total inflated byte count.
fn inflate_streaming(
    file: &mut std::fs::File,
    data_start: u64,
    csize: u64,
    hasher: &mut Fnv,
) -> Result<u64, String> {
    let mut decomp = miniz_oxide::inflate::core::DecompressorOxide::new();
    let mut dict = vec![0u8; DICT];
    let mut dict_ofs = 0usize;
    let mut carry: Vec<u8> = Vec::new();
    let mut remaining = csize;
    let mut read_pos = data_start;
    let mut total_out = 0u64;
    let mut steps = 0u32;

    loop {
        steps += 1;
        if steps >= MAX_STEPS {
            return Err(format!(
                "streaming inflate stopped making progress after {steps} steps"
            ));
        }
        if carry.is_empty() && remaining > 0 {
            let want = COMP_CHUNK.min(remaining as usize);
            carry.resize(want, 0);
            file.seek(SeekFrom::Start(read_pos))
                .map_err(|e| format!("seek: {e}"))?;
            file.read_exact(&mut carry)
                .map_err(|e| format!("read: {e}"))?;
            read_pos += want as u64;
            remaining -= want as u64;
        }
        let more = remaining > 0;
        let flags = if more {
            miniz_oxide::inflate::core::inflate_flags::TINFL_FLAG_HAS_MORE_INPUT
        } else {
            0
        };
        let (status, inc, outc) =
            miniz_oxide::inflate::core::decompress(&mut decomp, &carry, &mut dict, dict_ofs, flags);
        carry.drain(..inc);

        let start = dict_ofs;
        dict_ofs += outc;
        debug_assert!(dict_ofs <= DICT, "decoder wrote past the ring buffer");
        hasher.update(&dict[start..dict_ofs]);
        if dict_ofs == DICT {
            dict_ofs = 0;
        }
        total_out += outc as u64;

        match status {
            miniz_oxide::inflate::TINFLStatus::Done => {
                if remaining != 0 {
                    return Err(format!(
                        "stream ended early: {remaining} compressed bytes unread"
                    ));
                }
                return Ok(total_out);
            }
            miniz_oxide::inflate::TINFLStatus::HasMoreOutput => {}
            miniz_oxide::inflate::TINFLStatus::NeedsMoreInput => {
                if !more {
                    return Err("ran out of compressed input before stream end".into());
                }
            }
            s => return Err(format!("decoder failed: {s:?}")),
        }
    }
}
