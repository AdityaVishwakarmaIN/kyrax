//! Minimal ZIP central-directory reader (Zip64-aware) + libdeflater inflate.

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

fn u64le(b: &[u8], o: usize) -> u64 {
    (u32le(b, o) as u64) | ((u32le(b, o + 4) as u64) << 32)
}

/// Central-directory location resolved from the (possibly Zip64) EOCD.
struct EocdInfo {
    cd_count: u64,
    cd_size: u64,
    cd_offset: u64,
}

/// Locate the EOCD and promote the real central-directory count/size/offset
/// from the Zip64 EOCD record (found via the Zip64 locator) when the 32-bit
/// EOCD fields carry their sentinels. Returns [`TurboError::Format`] for any
/// structural violation; never panics on truncated or hostile input.
fn parse_eocd(zip: &[u8]) -> TurboResult<EocdInfo> {
    let n = zip.len();
    if n < 22 {
        return Err(TurboError::Format("ZIP file too small".into()));
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
    let eocd = match eocd {
        Some(idx) => idx,
        None => return Err(TurboError::Format("EOCD signature not found".into())),
    };
    if eocd + 22 > n {
        return Err(TurboError::Format("Truncated EOCD".into()));
    }

    let mut cd_count = u16le(zip, eocd + 10) as u64;
    let mut cd_size = u32le(zip, eocd + 12) as u64;
    let mut cd_offset = u32le(zip, eocd + 16) as u64;

    let has_locator = eocd >= 20 && &zip[eocd - 20..eocd - 16] == b"\x50\x4b\x06\x07";
    if has_locator {
        // Zip64 EOCD locator: sig(4) + disk(4) + offset(8) + total disks(4).
        let loc = eocd - 20;
        if u32le(zip, loc + 4) != 0 || u32le(zip, loc + 16) != 1 {
            return Err(TurboError::Format(
                "Multidisk Zip64 archives are not supported".into(),
            ));
        }
        let z64 = u64le(zip, loc + 8) as usize;
        if z64.saturating_add(56) > n || &zip[z64..z64 + 4] != b"\x50\x4b\x06\x06" {
            return Err(TurboError::Format("Corrupt Zip64 EOCD record".into()));
        }
        if u64le(zip, z64 + 4) < 44 {
            return Err(TurboError::Format("Truncated Zip64 EOCD record".into()));
        }
        if u32le(zip, z64 + 16) != 0 || u32le(zip, z64 + 20) != 0 {
            return Err(TurboError::Format(
                "Multidisk Zip64 archives are not supported".into(),
            ));
        }
        cd_count = u64le(zip, z64 + 32);
        cd_size = u64le(zip, z64 + 40);
        cd_offset = u64le(zip, z64 + 48);
    } else if cd_count == 0xFFFF || cd_size == 0xFFFF_FFFF || cd_offset == 0xFFFF_FFFF {
        return Err(TurboError::Format(
            "Zip64 sentinels present without a Zip64 EOCD record".into(),
        ));
    }

    let cd_off_us = cd_offset as usize;
    if cd_off_us >= n || cd_size as usize > n.saturating_sub(cd_off_us) {
        return Err(TurboError::Format(
            "Invalid central directory offset".into(),
        ));
    }

    Ok(EocdInfo {
        cd_count,
        cd_size,
        cd_offset: cd_off_us as u64,
    })
}

/// 64-bit sizes and local-header offset for one central-directory entry,
/// promoted from the Zip64 extended-information extra field (ID 0x0001) when
/// the 32-bit fields carry the 0xFFFFFFFF sentinel.
#[derive(Clone, Copy)]
pub(crate) struct CentralSizes {
    csize: u64,
    usize_: u64,
    local_off: u64,
}

/// Read one central-directory entry's sizes and local-header offset.
///
/// Zip64 extra fields are positional and conditional: each 64-bit value appears
/// only when its matching 32-bit field is the sentinel, in the fixed order
/// uncompressed size, compressed size, header offset. Fields are consumed only
/// when sentineled; anything the extra lacks for a sentineled field, or any
/// structural truncation, is a [`TurboError::Format`] — never a panic and never
/// an out-of-bounds read.
pub(crate) fn read_central_sizes(
    zip: &[u8],
    p: usize,
    fname_len: usize,
) -> TurboResult<CentralSizes> {
    let n = zip.len();
    let csize32 = u32le(zip, p + 20) as u64;
    let usize32 = u32le(zip, p + 24) as u64;
    let off32 = u32le(zip, p + 42) as u64;

    let extra_len = u16le(zip, p + 30);
    let extra_start = p + 46 + fname_len;
    if extra_len > n.saturating_sub(extra_start) {
        return Err(TurboError::Format(
            "Truncated central directory extra field".into(),
        ));
    }
    let extra = &zip[extra_start..extra_start + extra_len];

    let mut csize = csize32;
    let mut usize_ = usize32;
    let mut local_off = off32;
    let need_64 = csize == 0xFFFF_FFFF || usize_ == 0xFFFF_FFFF || local_off == 0xFFFF_FFFF;

    if need_64 {
        let mut found = false;
        let mut e = 0;
        while e + 4 <= extra.len() {
            let id = u16le(extra, e);
            let sz = u16le(extra, e + 2);
            let data = e + 4;
            if sz > extra.len().saturating_sub(data) {
                return Err(TurboError::Format(
                    "Corrupt central directory extra field".into(),
                ));
            }
            if id == 0x0001 {
                found = true;
                let end = data + sz;
                let mut pos = data;
                if usize_ == 0xFFFF_FFFF {
                    if pos + 8 > end {
                        return Err(TurboError::Format(
                            "Zip64 extra too short for uncompressed size".into(),
                        ));
                    }
                    usize_ = u64le(extra, pos);
                    pos += 8;
                }
                if csize == 0xFFFF_FFFF {
                    if pos + 8 > end {
                        return Err(TurboError::Format(
                            "Zip64 extra too short for compressed size".into(),
                        ));
                    }
                    csize = u64le(extra, pos);
                    pos += 8;
                }
                if local_off == 0xFFFF_FFFF {
                    if pos + 8 > end {
                        return Err(TurboError::Format(
                            "Zip64 extra too short for header offset".into(),
                        ));
                    }
                    local_off = u64le(extra, pos);
                }
                break;
            }
            e = data + sz;
        }
        if !found {
            return Err(TurboError::Format(
                "Zip64 sentinel without a matching extra field value".into(),
            ));
        }
    }

    Ok(CentralSizes {
        csize,
        usize_,
        local_off,
    })
}

/// Returns `(method, compressed_payload, uncompressed_size_hint)`.
///
/// All reads are bounds-checked against `zip.len()`. A corrupt central
/// directory, truncated or hostile EOCD/Zip64 records, or an entry whose
/// `csize` overruns the file returns `Err` (never panics). `Ok(None)` means the
/// archive parsed cleanly but the name is not present. Uncompressed-size lies
/// are handled later by [`inflate`].
pub fn find_entry<'a>(zip: &'a [u8], name: &str) -> TurboResult<Option<(u16, &'a [u8], usize)>> {
    let n = zip.len();
    let eocd = parse_eocd(zip)?;
    let mut p = eocd.cd_offset as usize;
    for _ in 0..eocd.cd_count {
        if p + 46 > n || &zip[p..p + 4] != b"\x50\x4b\x01\x02" {
            return Err(TurboError::Format(
                "Corrupt central directory record".into(),
            ));
        }
        let method = u16le(zip, p + 10) as u16;
        let fname_len = u16le(zip, p + 28);
        let extra_len = u16le(zip, p + 30);
        let comment_len = u16le(zip, p + 32);
        if p + 46 + fname_len > n {
            return Err(TurboError::Format(
                "Truncated filename in central directory".into(),
            ));
        }
        let fname = &zip[p + 46..p + 46 + fname_len];
        let sizes = read_central_sizes(zip, p, fname_len)?;
        if fname == name.as_bytes() {
            let lh = sizes.local_off as usize;
            if lh + 30 > n || &zip[lh..lh + 4] != b"\x50\x4b\x03\x04" {
                return Err(TurboError::Format("Invalid local header".into()));
            }
            let l_fname = u16le(zip, lh + 26);
            let l_extra = u16le(zip, lh + 28);
            let data = lh
                .checked_add(30)
                .and_then(|x| x.checked_add(l_fname))
                .and_then(|x| x.checked_add(l_extra));
            let Some(data) = data else {
                return Err(TurboError::Format(
                    "Overflow calculating data offset".into(),
                ));
            };
            let csize = sizes.csize as usize;
            if csize > n.saturating_sub(data) {
                return Err(TurboError::Format("Entry payload overruns ZIP file".into()));
            }
            return Ok(Some((
                method,
                &zip[data..data + csize],
                sizes.usize_ as usize,
            )));
        }
        let Some(next) = p
            .checked_add(46)
            .and_then(|x| x.checked_add(fname_len))
            .and_then(|x| x.checked_add(extra_len))
            .and_then(|x| x.checked_add(comment_len))
        else {
            return Err(TurboError::Format(
                "Central directory offset overflow".into(),
            ));
        };
        p = next;
    }
    Ok(None)
}

pub fn inflate(method: u16, comp: &[u8], usize_hint: usize) -> TurboResult<Vec<u8>> {
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

pub fn read_entry(zip: &[u8], name: &str) -> TurboResult<Option<Vec<u8>>> {
    match find_entry(zip, name)? {
        Some((m, c, u)) => Ok(Some(inflate(m, c, u)?)),
        None => Ok(None),
    }
}

/// Tolerant central-directory walker for the validator.
///
/// Unlike [`ArchiveMap::parse`], a single corrupt record does not abort the
/// walk: bad records are reported as error strings and scanning continues at
/// the next plausible central-directory signature, so as much of the archive
/// as possible is salvaged for a validation report. Entries whose records
/// parsed cleanly are returned with full metadata. Never panics.
pub(crate) fn list_entries(zip: &[u8]) -> TurboResult<(Vec<ZipEntryMeta>, Vec<String>)> {
    let n = zip.len();
    let eocd = parse_eocd(zip)?;
    let mut p = eocd.cd_offset as usize;
    let mut entries = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for _ in 0..eocd.cd_count {
        if p + 46 > n || &zip[p..p + 4] != b"\x50\x4b\x01\x02" {
            errors.push(format!("corrupt central directory record at offset {p}"));
            let search_from = p.saturating_add(1);
            if search_from >= n {
                break;
            }
            match memchr::memmem::find(&zip[search_from..], b"\x50\x4b\x01\x02") {
                Some(o) => p = search_from + o,
                None => break,
            }
            continue;
        }
        let method = u16le(zip, p + 10) as u16;
        let crc32 = (u32le(zip, p + 16) & 0xFFFF_FFFF) as u32;
        let fname_len = u16le(zip, p + 28);
        let extra_len = u16le(zip, p + 30);
        let comment_len = u16le(zip, p + 32);
        if p + 46 + fname_len > n {
            errors.push(format!(
                "truncated filename at central-directory offset {p}"
            ));
            break;
        }
        let name = String::from_utf8_lossy(&zip[p + 46..p + 46 + fname_len]).into_owned();
        let sizes = match read_central_sizes(zip, p, fname_len) {
            Ok(s) => s,
            Err(e) => {
                errors.push(format!("{name}: {e}"));
                let next = match p
                    .checked_add(46)
                    .and_then(|x| x.checked_add(fname_len))
                    .and_then(|x| x.checked_add(extra_len))
                    .and_then(|x| x.checked_add(comment_len))
                {
                    Some(np) => np,
                    None => break,
                };
                p = next;
                continue;
            }
        };
        let lh = sizes.local_off as usize;
        if lh + 30 <= n && &zip[lh..lh + 4] == b"\x50\x4b\x03\x04" {
            let l_fname = u16le(zip, lh + 26);
            let l_extra = u16le(zip, lh + 28);
            if let Some(d) = lh
                .checked_add(30)
                .and_then(|x| x.checked_add(l_fname))
                .and_then(|x| x.checked_add(l_extra))
            {
                entries.push(ZipEntryMeta {
                    name: name.clone(),
                    compression_method: method,
                    crc32,
                    compressed_size: sizes.csize,
                    uncompressed_size: sizes.usize_,
                    local_header_offset: sizes.local_off,
                    data_offset: d as u64,
                });
                let next = match p
                    .checked_add(46)
                    .and_then(|x| x.checked_add(fname_len))
                    .and_then(|x| x.checked_add(extra_len))
                    .and_then(|x| x.checked_add(comment_len))
                {
                    Some(np) => np,
                    None => break,
                };
                p = next;
                continue;
            }
        }
        errors.push(format!("invalid local header for entry {name}"));
        let next = match p
            .checked_add(46)
            .and_then(|x| x.checked_add(fname_len))
            .and_then(|x| x.checked_add(extra_len))
            .and_then(|x| x.checked_add(comment_len))
        {
            Some(np) => np,
            None => break,
        };
        p = next;
    }

    Ok((entries, errors))
}

/// Inflate one archive entry's payload from the raw zip bytes (validation path).
/// Never panics; returns `Format` for a payload that overruns the file.
pub(crate) fn inflate_entry(zip: &[u8], meta: &ZipEntryMeta) -> TurboResult<Vec<u8>> {
    let start = meta.data_offset as usize;
    let end = start.saturating_add(meta.compressed_size as usize);
    if end > zip.len() {
        return Err(TurboError::Format(format!(
            "entry payload for {} overruns ZIP file",
            meta.name
        )));
    }
    inflate(
        meta.compression_method,
        &zip[start..end],
        meta.uncompressed_size as usize,
    )
}

use ahash::AHashMap;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct ZipEntryMeta {
    pub name: String,
    pub compression_method: u16,
    pub crc32: u32,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
    pub local_header_offset: u64,
    pub data_offset: u64,
}

#[derive(Debug, Clone)]
pub struct ArchiveMap {
    pub source_bytes: Arc<Vec<u8>>,
    pub entries: AHashMap<String, ZipEntryMeta>,
    pub entry_order: Vec<String>,
    pub sheet_name_map: AHashMap<String, String>,
    pub shared_strings: Arc<crate::turbo::scan::StringArena>,
}

impl ArchiveMap {
    pub fn parse(zip_bytes: Arc<Vec<u8>>) -> TurboResult<Self> {
        let zip = zip_bytes.as_slice();
        let n = zip.len();
        let eocd = parse_eocd(zip)?;
        let cd_count = eocd.cd_count;
        let cd_size = eocd.cd_size;
        let mut p = eocd.cd_offset as usize;
        let mut entries = AHashMap::default();
        let capacity = cd_count.min(cd_size / 46).min(1 << 20) as usize;
        let mut entry_order = Vec::with_capacity(capacity);

        for _ in 0..cd_count {
            if p + 46 > n || &zip[p..p + 4] != b"\x50\x4b\x01\x02" {
                return Err(TurboError::Format(
                    "Corrupt central directory record".into(),
                ));
            }
            let method = u16le(zip, p + 10) as u16;
            let crc32 = (u32le(zip, p + 16) & 0xFFFFFFFF) as u32;
            let fname_len = u16le(zip, p + 28);
            let extra_len = u16le(zip, p + 30);
            let comment_len = u16le(zip, p + 32);
            if p + 46 + fname_len > n {
                return Err(TurboError::Format(
                    "Truncated filename in central directory".into(),
                ));
            }
            let fname_bytes = &zip[p + 46..p + 46 + fname_len];
            let name = String::from_utf8_lossy(fname_bytes).into_owned();
            let sizes = read_central_sizes(zip, p, fname_len)?;
            let csize = sizes.csize;
            let usize_ = sizes.usize_;
            let local_off = sizes.local_off;

            let lh = local_off as usize;
            if lh + 30 > n || &zip[lh..lh + 4] != b"\x50\x4b\x03\x04" {
                return Err(TurboError::Format(format!(
                    "Invalid local header for entry {name}"
                )));
            }
            let l_fname = u16le(zip, lh + 26);
            let l_extra = u16le(zip, lh + 28);
            let data_offset = match lh
                .checked_add(30)
                .and_then(|x| x.checked_add(l_fname))
                .and_then(|x| x.checked_add(l_extra))
            {
                Some(d) => d as u64,
                None => {
                    return Err(TurboError::Format(format!(
                        "Overflow calculating data offset for {name}"
                    )));
                }
            };

            if csize as usize > n.saturating_sub(data_offset as usize) {
                return Err(TurboError::Format(format!(
                    "Entry payload overruns ZIP file for {name}"
                )));
            }

            let meta = ZipEntryMeta {
                name: name.clone(),
                compression_method: method,
                crc32,
                compressed_size: csize,
                uncompressed_size: usize_,
                local_header_offset: local_off,
                data_offset,
            };

            entries.insert(name.clone(), meta);
            entry_order.push(name);

            p = match p
                .checked_add(46)
                .and_then(|x| x.checked_add(fname_len))
                .and_then(|x| x.checked_add(extra_len))
                .and_then(|x| x.checked_add(comment_len))
            {
                Some(next_p) => next_p,
                None => {
                    return Err(TurboError::Format(
                        "Central directory offset overflow".into(),
                    ));
                }
            };
        }

        let sheet_name_map = parse_workbook_sheet_map(&entries, zip);
        let shared_strings = if let Some(sst_entry) = entries.get("xl/sharedStrings.xml") {
            let start = sst_entry.data_offset as usize;
            let end = start + sst_entry.compressed_size as usize;
            if end <= zip.len() {
                if let Ok(xml) = inflate(
                    sst_entry.compression_method,
                    &zip[start..end],
                    sst_entry.uncompressed_size as usize,
                ) {
                    Arc::new(crate::turbo::scan::parse_shared_strings(&xml))
                } else {
                    Arc::new(crate::turbo::scan::parse_shared_strings(b""))
                }
            } else {
                Arc::new(crate::turbo::scan::parse_shared_strings(b""))
            }
        } else {
            Arc::new(crate::turbo::scan::parse_shared_strings(b""))
        };

        Ok(ArchiveMap {
            source_bytes: zip_bytes,
            entries,
            entry_order,
            sheet_name_map,
            shared_strings,
        })
    }
}

fn parse_workbook_sheet_map(
    entries: &AHashMap<String, ZipEntryMeta>,
    zip: &[u8],
) -> AHashMap<String, String> {
    let mut name_to_rid = AHashMap::default();
    let mut rid_to_target = AHashMap::default();
    let mut map = AHashMap::default();

    if let Some(wb_entry) = entries.get("xl/workbook.xml") {
        let start = wb_entry.data_offset as usize;
        let end = start + wb_entry.compressed_size as usize;
        if end <= zip.len() {
            if let Ok(xml) = inflate(
                wb_entry.compression_method,
                &zip[start..end],
                wb_entry.uncompressed_size as usize,
            ) {
                let mut pos = 0;
                while let Some(so) = memchr::memmem::find(&xml[pos..], b"<sheet ") {
                    let s_start = pos + so;
                    let Some(gt) = memchr::memchr(b'>', &xml[s_start..]) else {
                        break;
                    };
                    let tag = &xml[s_start..s_start + gt + 1];
                    pos = s_start + gt + 1;

                    let name = extract_xml_attr(tag, b"name");
                    let rid = extract_xml_attr(tag, b"r:id");
                    if let (Some(n), Some(r)) = (name, rid) {
                        name_to_rid.insert(r, n);
                    }
                }
            }
        }
    }

    if let Some(rels_entry) = entries.get("xl/_rels/workbook.xml.rels") {
        let start = rels_entry.data_offset as usize;
        let end = start + rels_entry.compressed_size as usize;
        if end <= zip.len() {
            if let Ok(xml) = inflate(
                rels_entry.compression_method,
                &zip[start..end],
                rels_entry.uncompressed_size as usize,
            ) {
                let mut pos = 0;
                while let Some(ro) = memchr::memmem::find(&xml[pos..], b"<Relationship ") {
                    let r_start = pos + ro;
                    let Some(gt) = memchr::memchr(b'>', &xml[r_start..]) else {
                        break;
                    };
                    let tag = &xml[r_start..r_start + gt + 1];
                    pos = r_start + gt + 1;

                    let id = extract_xml_attr(tag, b"Id");
                    let target = extract_xml_attr(tag, b"Target");
                    if let (Some(i), Some(t)) = (id, target) {
                        let normalized_target = if t.starts_with("xl/") {
                            t
                        } else if t.starts_with('/') {
                            t.trim_start_matches('/').to_string()
                        } else {
                            format!("xl/{t}")
                        };
                        rid_to_target.insert(i, normalized_target);
                    }
                }
            }
        }
    }

    for (rid, sheet_name) in name_to_rid {
        if let Some(target) = rid_to_target.get(&rid) {
            map.insert(sheet_name, target.clone());
        }
    }

    if map.is_empty() {
        for name in entries.keys() {
            if name.starts_with("xl/worksheets/sheet") && name.ends_with(".xml") {
                let stem = name
                    .trim_start_matches("xl/worksheets/")
                    .trim_end_matches(".xml");
                let display_name = format!("Sheet{}", stem.trim_start_matches("sheet"));
                map.insert(display_name, name.clone());
            }
        }
    }

    map
}

fn extract_xml_attr(tag: &[u8], attr: &[u8]) -> Option<String> {
    let mut search = Vec::with_capacity(attr.len() + 2);
    search.extend_from_slice(attr);
    search.extend_from_slice(b"=\"");
    let o = memchr::memmem::find(tag, &search)?;
    let val_start = o + search.len();
    let q = memchr::memchr(b'"', &tag[val_start..])?;
    let val_bytes = &tag[val_start..val_start + q];
    Some(String::from_utf8_lossy(val_bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[derive(Clone, Copy)]
    struct Z64 {
        usize_: Option<u64>,
        csize: Option<u64>,
        off: Option<u64>,
    }

    #[derive(Clone)]
    struct Opts {
        name: &'static str,
        payload: &'static [u8],
        crc: u32,
        central_z64: Option<Z64>,
        central_extra_override: Option<Vec<u8>>,
        zip64_eocd_count: Option<u64>,
        force_eocd_sentinels: bool,
        corrupt_locator_off: bool,
        corrupt_zip64_cd_off: bool,
        truncate: Option<usize>,
    }

    impl Default for Opts {
        fn default() -> Self {
            Opts {
                name: "a.txt",
                payload: b"hello",
                crc: 0,
                central_z64: None,
                central_extra_override: None,
                zip64_eocd_count: None,
                force_eocd_sentinels: false,
                corrupt_locator_off: false,
                corrupt_zip64_cd_off: false,
                truncate: None,
            }
        }
    }

    fn w_u16(out: &mut Vec<u8>, v: u16) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    fn w_u32(out: &mut Vec<u8>, v: u32) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    fn w_u64(out: &mut Vec<u8>, v: u64) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    fn find_sig(bytes: &[u8], sig: &[u8; 4]) -> Option<usize> {
        bytes.windows(4).position(|w| w == sig)
    }

    /// Build a tiny single-entry STORE archive with full control over the
    /// central-directory sizes, the 0x0001 extra field, and the Zip64 EOCD /
    /// locator presence. Nothing here allocates more than a few hundred bytes.
    fn build(o: &Opts) -> Vec<u8> {
        let name = o.name.as_bytes();
        let payload_len = o.payload.len() as u64;
        let z = o.central_z64;
        let has_central_z64 = z.is_some();
        let mut out = Vec::new();

        // ---- local file header
        out.extend_from_slice(b"PK\x03\x04");
        w_u16(&mut out, 20);
        w_u16(&mut out, 0);
        w_u16(&mut out, 0);
        w_u16(&mut out, 0);
        w_u16(&mut out, 0);
        w_u32(&mut out, o.crc);
        let l_zip64 = z.map_or(false, |z| z.usize_.is_some() || z.csize.is_some());
        if l_zip64 {
            w_u32(&mut out, 0xFFFF_FFFF);
            w_u32(&mut out, 0xFFFF_FFFF);
        } else {
            w_u32(&mut out, payload_len as u32);
            w_u32(&mut out, payload_len as u32);
        }
        w_u16(&mut out, name.len() as u16);
        let mut l_extra = Vec::new();
        if l_zip64 {
            w_u16(&mut l_extra, 0x0001);
            let mut data = Vec::new();
            if let Some(z) = z {
                if let Some(v) = z.usize_ {
                    w_u64(&mut data, v);
                }
                if let Some(v) = z.csize {
                    w_u64(&mut data, v);
                }
            }
            w_u16(&mut l_extra, data.len() as u16);
            l_extra.extend_from_slice(&data);
        }
        w_u16(&mut out, l_extra.len() as u16);
        out.extend_from_slice(name);
        out.extend_from_slice(&l_extra);
        out.extend_from_slice(o.payload);

        // ---- central directory header
        let cd_start = out.len() as u64;
        out.extend_from_slice(b"PK\x01\x02");
        w_u16(&mut out, if has_central_z64 { 45 } else { 20 });
        w_u16(&mut out, if has_central_z64 { 45 } else { 20 });
        w_u16(&mut out, 0);
        w_u16(&mut out, 0);
        w_u16(&mut out, 0);
        w_u16(&mut out, 0);
        w_u32(&mut out, o.crc);
        w_u32(
            &mut out,
            if z.and_then(|z| z.csize).is_some() {
                0xFFFF_FFFF
            } else {
                payload_len as u32
            },
        );
        w_u32(
            &mut out,
            if z.and_then(|z| z.usize_).is_some() {
                0xFFFF_FFFF
            } else {
                payload_len as u32
            },
        );
        w_u16(&mut out, name.len() as u16);
        let central_extra = match &o.central_extra_override {
            Some(e) => e.clone(),
            None => {
                let mut e = Vec::new();
                if let Some(z) = z {
                    w_u16(&mut e, 0x0001);
                    let mut data = Vec::new();
                    if let Some(v) = z.usize_ {
                        w_u64(&mut data, v);
                    }
                    if let Some(v) = z.csize {
                        w_u64(&mut data, v);
                    }
                    if let Some(v) = z.off {
                        w_u64(&mut data, v);
                    }
                    w_u16(&mut e, data.len() as u16);
                    e.extend_from_slice(&data);
                }
                e
            }
        };
        w_u16(&mut out, central_extra.len() as u16);
        w_u16(&mut out, 0);
        w_u16(&mut out, 0);
        w_u16(&mut out, 0);
        w_u32(&mut out, 0);
        w_u32(
            &mut out,
            if z.and_then(|z| z.off).is_some() {
                0xFFFF_FFFF
            } else {
                0
            },
        );
        out.extend_from_slice(name);
        out.extend_from_slice(&central_extra);
        let cd_size = (out.len() as u64) - cd_start;

        // ---- optional Zip64 EOCD record + locator
        if let Some(count) = o.zip64_eocd_count {
            let zip64_eocd_off = out.len() as u64;
            out.extend_from_slice(b"PK\x06\x06");
            w_u64(&mut out, 44);
            w_u16(&mut out, 45);
            w_u16(&mut out, 45);
            w_u32(&mut out, 0);
            w_u32(&mut out, 0);
            w_u64(&mut out, count);
            w_u64(&mut out, count);
            w_u64(&mut out, cd_size);
            w_u64(
                &mut out,
                if o.corrupt_zip64_cd_off {
                    u64::MAX
                } else {
                    cd_start
                },
            );
            out.extend_from_slice(b"PK\x06\x07");
            w_u32(&mut out, 0);
            w_u64(
                &mut out,
                if o.corrupt_locator_off {
                    u64::MAX
                } else {
                    zip64_eocd_off
                },
            );
            w_u32(&mut out, 1);
        }

        // ---- EOCD
        out.extend_from_slice(b"PK\x05\x06");
        w_u16(&mut out, 0);
        w_u16(&mut out, 0);
        let use_sentinels = o.zip64_eocd_count.is_some() || o.force_eocd_sentinels;
        if use_sentinels {
            w_u16(&mut out, 0xFFFF);
            w_u16(&mut out, 0xFFFF);
        } else {
            w_u16(&mut out, 1);
            w_u16(&mut out, 1);
        }
        if use_sentinels {
            w_u32(&mut out, 0xFFFF_FFFF);
            w_u32(&mut out, 0xFFFF_FFFF);
        } else {
            w_u32(&mut out, cd_size as u32);
            w_u32(&mut out, cd_start as u32);
        }
        w_u16(&mut out, 0);

        if let Some(t) = o.truncate {
            out.truncate(t);
        }
        out
    }

    #[test]
    fn zip64_writer_shape_roundtrips_find_and_map() {
        // Model the turbo writer's Zip64 output: all three 32-bit central fields
        // sentineled, all three 64-bit values present in the 0x0001 extra.
        let bytes = build(&Opts {
            central_z64: Some(Z64 {
                usize_: Some(5),
                csize: Some(5),
                off: Some(0),
            }),
            zip64_eocd_count: Some(1),
            ..Opts::default()
        });
        let (m, c, u) = find_entry(&bytes, "a.txt").unwrap().unwrap();
        assert_eq!(m, 0);
        assert_eq!(c, b"hello");
        assert_eq!(u, 5);
        let read = read_entry(&bytes, "a.txt").unwrap();
        assert_eq!(read, Some(b"hello".to_vec()));
        let map = ArchiveMap::parse(Arc::new(bytes)).unwrap();
        let e = map.entries.get("a.txt").unwrap();
        assert_eq!(e.compressed_size, 5);
        assert_eq!(e.uncompressed_size, 5);
        assert_eq!(e.local_header_offset, 0);
        assert_eq!(e.data_offset, 55);
    }

    #[test]
    fn zip64_partial_extra_sizes_only() {
        // Sizes are 64-bit, header offset stays 32-bit: the conditional extra
        // carries only the sentineled fields, in fixed order.
        let bytes = build(&Opts {
            central_z64: Some(Z64 {
                usize_: Some(5),
                csize: Some(5),
                off: None,
            }),
            zip64_eocd_count: Some(1),
            ..Opts::default()
        });
        let (m, c, u) = find_entry(&bytes, "a.txt").unwrap().unwrap();
        assert_eq!((m, c, u), (0, b"hello".as_slice(), 5));
        let map = ArchiveMap::parse(Arc::new(bytes)).unwrap();
        assert_eq!(map.entries.get("a.txt").unwrap().compressed_size, 5);
    }

    #[test]
    fn zip64_extra_too_short_is_error() {
        // usize_ and csize sentineled but the extra carries only 8 bytes: the
        // second sentineled field must produce a Format error, not a panic.
        let bytes = build(&Opts {
            central_z64: Some(Z64 {
                usize_: Some(5),
                csize: Some(5),
                off: Some(0),
            }),
            central_extra_override: Some(vec![0x01, 0x00, 0x08, 0x00, 0, 0, 0, 0, 0, 0, 0, 0]),
            zip64_eocd_count: Some(1),
            ..Opts::default()
        });
        assert!(find_entry(&bytes, "a.txt").is_err());
        assert!(ArchiveMap::parse(Arc::new(bytes)).is_err());
    }

    #[test]
    fn zip64_sentinel_without_record_is_error() {
        let bytes = build(&Opts {
            force_eocd_sentinels: true,
            ..Opts::default()
        });
        assert!(find_entry(&bytes, "a.txt").is_err());
        assert!(ArchiveMap::parse(Arc::new(bytes)).is_err());
    }

    #[test]
    fn corrupt_zip64_locator_offset_is_error() {
        let bytes = build(&Opts {
            zip64_eocd_count: Some(1),
            corrupt_locator_off: true,
            ..Opts::default()
        });
        assert!(find_entry(&bytes, "a.txt").is_err());
        assert!(ArchiveMap::parse(Arc::new(bytes)).is_err());
    }

    #[test]
    fn corrupt_zip64_cd_offset_is_error() {
        let bytes = build(&Opts {
            zip64_eocd_count: Some(1),
            corrupt_zip64_cd_off: true,
            ..Opts::default()
        });
        assert!(find_entry(&bytes, "a.txt").is_err());
    }

    #[test]
    fn undersized_zip64_eocd_record_is_error() {
        let mut bytes = build(&Opts {
            zip64_eocd_count: Some(1),
            ..Opts::default()
        });
        let pos = find_sig(&bytes, b"PK\x06\x06").expect("zip64 eocd signature");
        bytes[pos + 4..pos + 12].fill(0); // record size 0 < 44
        assert!(find_entry(&bytes, "a.txt").is_err());
        assert!(ArchiveMap::parse(Arc::new(bytes)).is_err());
    }

    #[test]
    fn truncated_zip64_archive_is_error() {
        let bytes = build(&Opts {
            zip64_eocd_count: Some(1),
            truncate: Some(60),
            ..Opts::default()
        });
        assert!(find_entry(&bytes, "a.txt").is_err());
        assert!(ArchiveMap::parse(Arc::new(bytes)).is_err());
    }

    #[test]
    fn missing_eocd_is_error() {
        let bytes = b"PK\x03\x04garbage that never contains an EOCD signature".to_vec();
        assert!(find_entry(&bytes, "a.txt").is_err());
        assert!(ArchiveMap::parse(Arc::new(bytes)).is_err());
    }

    #[test]
    fn entry_payload_overrun_is_error() {
        let bytes = build(&Opts {
            central_z64: Some(Z64 {
                usize_: Some(5),
                csize: Some(1_000_000),
                off: None,
            }),
            zip64_eocd_count: Some(1),
            ..Opts::default()
        });
        assert!(find_entry(&bytes, "a.txt").is_err());
        assert!(ArchiveMap::parse(Arc::new(bytes)).is_err());
    }

    #[test]
    fn corrupt_central_record_signature_is_error() {
        let mut bytes = build(&Opts::default());
        let cd = 40; // 30 (local header) + 5 (name) + 5 (payload)
        assert_eq!(&bytes[cd..cd + 4], b"PK\x01\x02");
        bytes[cd] ^= 0xFF;
        assert!(find_entry(&bytes, "a.txt").is_err());
        assert!(ArchiveMap::parse(Arc::new(bytes)).is_err());
    }

    #[test]
    fn absent_part_is_none_not_error() {
        let bytes = build(&Opts::default());
        assert!(find_entry(&bytes, "nope.txt").unwrap().is_none());
        assert!(read_entry(&bytes, "nope.txt").unwrap().is_none());
    }

    #[test]
    fn plain_archive_still_roundtrips() {
        let bytes = build(&Opts::default());
        let (m, c, u) = find_entry(&bytes, "a.txt").unwrap().unwrap();
        assert_eq!((m, c, u), (0, b"hello".as_slice(), 5));
        let map = ArchiveMap::parse(Arc::new(bytes)).unwrap();
        assert_eq!(map.entries.get("a.txt").unwrap().data_offset, 35);
    }
}
