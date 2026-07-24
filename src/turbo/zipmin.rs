//! Minimal ZIP central-directory reader (no zip64) + libdeflater inflate.

use crate::turbo::error::{TurboError, TurboResult};

fn u16le(b: &[u8], o: usize) -> usize {
    (b[o] as usize) | ((b[o + 1] as usize) << 8)
}

fn u32le(b: &[u8], o: usize) -> usize {
    (b[o] as usize)
        | ((b[o + 1] as usize) << 8)
        | ((b[o + 2] as usize) << 16)
        | ((b[o + 3] as usize) << 24)
}

/// Returns `(method, compressed_payload, uncompressed_size_hint)`.
///
/// All reads are bounds-checked against `zip.len()`. A corrupt central directory,
/// truncated EOCD, or entry whose `csize` overruns the file returns `None`
/// (never panics). Uncompressed-size lies are handled later by [`inflate`].
pub(crate) fn find_entry<'a>(zip: &'a [u8], name: &str) -> Option<(u16, &'a [u8], usize)> {
    let n = zip.len();
    if n < 22 {
        return None;
    }
    let mut eocd = None;
    let lo = n.saturating_sub(65_557);
    let mut i = n.saturating_sub(22);
    while i >= lo {
        if i + 4 <= n && &zip[i..i + 4] == b"\x50\x4b\x05\x06" {
            eocd = Some(i);
            break;
        }
        if i == 0 {
            break;
        }
        i -= 1;
    }
    let eocd = eocd?;
    // EOCD is 22 bytes minimum; refuse short tails.
    if eocd + 22 > n {
        return None;
    }
    let cd_count = u16le(zip, eocd + 10);
    let cd_off = u32le(zip, eocd + 16);
    if cd_off >= n {
        return None;
    }
    let mut p = cd_off;
    for _ in 0..cd_count {
        if p + 46 > n || &zip[p..p + 4] != b"\x50\x4b\x01\x02" {
            return None;
        }
        let method = u16le(zip, p + 10) as u16;
        let csize = u32le(zip, p + 20);
        let usize_ = u32le(zip, p + 24);
        let fname_len = u16le(zip, p + 28);
        let extra_len = u16le(zip, p + 30);
        let comment_len = u16le(zip, p + 32);
        let local_off = u32le(zip, p + 42);
        if p + 46 + fname_len > n {
            return None;
        }
        let fname = &zip[p + 46..p + 46 + fname_len];
        if fname == name.as_bytes() {
            let lh = local_off;
            if lh + 30 > n || &zip[lh..lh + 4] != b"\x50\x4b\x03\x04" {
                return None;
            }
            let l_fname = u16le(zip, lh + 26);
            let l_extra = u16le(zip, lh + 28);
            let data = lh.checked_add(30)?.checked_add(l_fname)?.checked_add(l_extra)?;
            if data.checked_add(csize)? > n {
                return None;
            }
            return Some((method, &zip[data..data + csize], usize_));
        }
        p = p
            .checked_add(46)?
            .checked_add(fname_len)?
            .checked_add(extra_len)?
            .checked_add(comment_len)?;
    }
    None
}

pub(crate) fn inflate(method: u16, comp: &[u8], usize_hint: usize) -> TurboResult<Vec<u8>> {
    match method {
        0 => Ok(comp.to_vec()),
        8 => {
            let mut out = vec![0u8; usize_hint.max(1)];
            let mut d = libdeflater::Decompressor::new();
            match d.deflate_decompress(comp, &mut out) {
                Ok(n) => {
                    out.truncate(n);
                    Ok(out)
                }
                Err(_) if usize_hint == 0 || out.len() < comp.len().saturating_mul(4) => {
                    // Retry with a larger buffer if the size hint was wrong.
                    let mut out = vec![0u8; (comp.len() * 8).max(64 * 1024)];
                    let n = d
                        .deflate_decompress(comp, &mut out)
                        .map_err(|e| TurboError::Inflate(format!("{e:?}")))?;
                    out.truncate(n);
                    Ok(out)
                }
                Err(e) => Err(TurboError::Inflate(format!("{e:?}"))),
            }
        }
        m => Err(TurboError::Inflate(format!(
            "unsupported compression method {m}"
        ))),
    }
}

pub(crate) fn read_entry(zip: &[u8], name: &str) -> TurboResult<Option<Vec<u8>>> {
    match find_entry(zip, name) {
        Some((m, c, u)) => Ok(Some(inflate(m, c, u)?)),
        None => Ok(None),
    }
}
