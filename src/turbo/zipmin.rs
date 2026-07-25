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
            let data = lh
                .checked_add(30)?
                .checked_add(l_fname)?
                .checked_add(l_extra)?;
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

        if eocd >= 20 && &zip[eocd - 20..eocd - 16] == b"\x50\x4b\x06\x07" {
            return Err(TurboError::Format(
                "Zip64 input archives are not supported in v1 edit_excel mode".into(),
            ));
        }

        let cd_count = u16le(zip, eocd + 10);
        let cd_off = u32le(zip, eocd + 16);
        if cd_count == 0xFFFF || cd_off == 0xFFFFFFFF {
            return Err(TurboError::Format(
                "Zip64 input archives are not supported in v1 edit_excel mode".into(),
            ));
        }
        if cd_off >= n {
            return Err(TurboError::Format(
                "Invalid central directory offset".into(),
            ));
        }

        let mut p = cd_off;
        let mut entries = AHashMap::default();
        let mut entry_order = Vec::with_capacity(cd_count);

        for _ in 0..cd_count {
            if p + 46 > n || &zip[p..p + 4] != b"\x50\x4b\x01\x02" {
                return Err(TurboError::Format(
                    "Corrupt central directory record".into(),
                ));
            }
            let method = u16le(zip, p + 10) as u16;
            let crc32 = (u32le(zip, p + 16) & 0xFFFFFFFF) as u32;
            let csize = u32le(zip, p + 20) as u64;
            let usize_ = u32le(zip, p + 24) as u64;
            let fname_len = u16le(zip, p + 28);
            let extra_len = u16le(zip, p + 30);
            let comment_len = u16le(zip, p + 32);
            let local_off = u32le(zip, p + 42) as u64;

            if csize == 0xFFFFFFFF || usize_ == 0xFFFFFFFF || local_off == 0xFFFFFFFF {
                return Err(TurboError::Format(
                    "Zip64 entry bounds not supported in v1 edit_excel mode".into(),
                ));
            }
            if p + 46 + fname_len > n {
                return Err(TurboError::Format(
                    "Truncated filename in central directory".into(),
                ));
            }
            let fname_bytes = &zip[p + 46..p + 46 + fname_len];
            let name = String::from_utf8_lossy(fname_bytes).into_owned();

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

            if (data_offset as usize) + (csize as usize) > n {
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
