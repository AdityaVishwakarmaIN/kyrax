//! Rich data types (stocks, geography, image-in-cell) — read + preserve
//! (Tier 3 MEDIUM).
//!
//! No library reads these today — openpyxl has none either — so this module is
//! purely additive: parse the two `xl/richData/` parts that carry the data and
//! hand the entry names to the byte-preserving edit path untouched.
//!
//! A workbook stores rich values across four places:
//!   * `xl/richData/rdrichvalue.xml` — the instances:
//!     `<rvData count="N"><rv s="0"><v>text</v><v>1234</v></rv></rvData>`.
//!   * `xl/richData/rdrichvaluestructure.xml` — the declared structs:
//!     `<rvStructures count="N"><s t="_linkedEntity2"><k n=".." t=".."/></s>…`.
//!   * `xl/richData/rdRichValueTypes.xml` / `richValueRel.xml` — carried through
//!     verbatim, never parsed.
//!   * `xl/metadata.xml` — the index a cell points at: `<c t="e" vm="1">`
//!     indexes this part, whose `<valueMetadata><bk><rc v="N"/></bk>` list maps
//!     the 1-based `vm` to a 0-based row in `rdrichvalue.xml`.
//!
//! The fast path is the absent one: a workbook without `xl/richData/` costs a
//! single central-directory name pass and returns an empty list — never an
//! inflate. All parsers are tolerant of truncated or hostile markup: they
//! return whatever elements they could read (often empty), never a panic and
//! never an out-of-bounds index. `resolve` is the only lookup that can fail,
//! and it answers `None` instead of panicking.

use crate::turbo::decode::decode_bytes;
use crate::turbo::error::TurboResult;
use crate::turbo::structural::find_attr;
use crate::turbo::zipmin::list_entries;

// ----------------------------------------------------------------------------
// Data model
// ----------------------------------------------------------------------------

/// One declared rich-value structure (`<s t="type">` with its `<k n=".." t=".."/>`
/// children): the (key name, key type) pairs a rich value is rendered against.
#[derive(Clone, Debug)]
pub struct RichValueStruct {
    pub type_name: String,
    pub keys: Vec<(String, String)>, // (key name, key type)
}

/// One rich-value instance (`<rv s="struct-index">`): its struct index and the
/// ordered `<v>` values, entity-decoded and verbatim.
#[derive(Clone, Debug)]
pub struct RichValue {
    pub struct_index: usize,
    pub values: Vec<String>,
}

// ----------------------------------------------------------------------------
// Shared low-level helpers (local-name tolerant, like src/turbo/structural.rs)
// ----------------------------------------------------------------------------

#[inline]
fn attr_string(tag: &[u8], name: &[u8], scratch: &mut Vec<u8>) -> Option<String> {
    find_attr(tag, name).map(|raw| String::from_utf8_lossy(decode_bytes(raw, scratch)).into_owned())
}

#[inline]
fn attr_usize(tag: &[u8], name: &[u8]) -> Option<usize> {
    find_attr(tag, name).and_then(|v| std::str::from_utf8(v).ok()?.trim().parse().ok())
}

/// True if `xml[pos..]` is an open tag whose local name is `local`
/// (`<local …>` or `<ns:local …>`). Namespace-tolerant; never panics.
#[inline]
fn is_open_local(xml: &[u8], pos: usize, local: &[u8]) -> bool {
    if xml.get(pos) != Some(&b'<') {
        return false;
    }
    let after = pos + 1;
    if after >= xml.len() {
        return false;
    }
    let c0 = xml[after];
    if c0 == b'/' || c0 == b'!' || c0 == b'?' {
        return false;
    }
    let rest = &xml[after..];
    let name_end = rest
        .iter()
        .position(|&c| {
            c == b' ' || c == b'>' || c == b'/' || c == b'\n' || c == b'\r' || c == b'\t'
        })
        .unwrap_or(rest.len());
    let name = &rest[..name_end];
    let local_name = match memchr::memchr(b':', name) {
        Some(c) => &name[c + 1..],
        None => name,
    };
    local_name == local
}

/// First open tag with local name `local` at or after `from`, returning its
/// absolute offset. `None` on a truncated part — never an error.
#[inline]
fn find_open_local(xml: &[u8], from: usize, local: &[u8]) -> Option<usize> {
    if from >= xml.len() {
        return None;
    }
    let mut i = from;
    while i < xml.len() {
        let Some(o) = memchr::memchr(b'<', &xml[i..]) else {
            break;
        };
        let pos = i + o;
        if is_open_local(xml, pos, local) {
            return Some(pos);
        }
        i = pos.saturating_add(1);
    }
    None
}

/// Byte offset of `</local>` or `</ns:local>` (start of the closing tag).
#[inline]
fn find_close_local(xml: &[u8], from: usize, local: &[u8]) -> Option<usize> {
    if from >= xml.len() {
        return None;
    }
    let mut i = from;
    while i < xml.len() {
        let Some(o) = memchr::memmem::find(&xml[i..], b"</") else {
            break;
        };
        let pos = i + o;
        let name_start = pos + 2;
        if name_start >= xml.len() {
            return None;
        }
        let rest = &xml[name_start..];
        let name_end = rest
            .iter()
            .position(|&c| c == b'>' || c == b' ' || c == b'\n' || c == b'\r' || c == b'\t')
            .unwrap_or(rest.len());
        let name = &rest[..name_end];
        let local_name = match memchr::memchr(b':', name) {
            Some(c) => &name[c + 1..],
            None => name,
        };
        if local_name == local {
            return Some(pos);
        }
        i = pos.saturating_add(2);
    }
    None
}

/// End of the element whose open tag starts at `start` (after the `>` that
/// closes it, `>` of its close tag, or `n` on truncated input). Never panics.
#[inline]
fn elem_end(xml: &[u8], start: usize, local: &[u8]) -> usize {
    let n = xml.len();
    let te = start + memchr::memchr(b'>', &xml[start..n]).unwrap_or(n - start);
    if te > 0 && xml.get(te - 1) == Some(&b'/') {
        te.saturating_add(1).min(n)
    } else {
        find_close_local(xml, te.saturating_add(1), local)
            .and_then(|c| memchr::memchr(b'>', &xml[c..]).map(|o| c + o + 1))
            .unwrap_or(n)
            .min(n)
    }
}

// ----------------------------------------------------------------------------
// Parse
// ----------------------------------------------------------------------------

/// Parse the declared rich-value structs out of an already-inflated
/// `rdrichvaluestructure.xml` part.
///
/// Fast path first: a part with no `rvStructures` root (including an absent or
/// empty one) returns `Ok(vec![])` after a single memmem probe and touches
/// nothing else. A struct's `t` attribute becomes `type_name`; each `<k>` child
/// becomes a (name, type) pair in order. Missing attributes default to the
/// empty string, self-closing structs yield no keys, and the function never
/// panics on truncated markup.
pub fn parse_rich_value_structures(part: &[u8]) -> TurboResult<Vec<RichValueStruct>> {
    if memchr::memmem::find(part, b"rvStructures").is_none() {
        return Ok(Vec::new());
    }
    let n = part.len();
    let mut out = Vec::new();
    let mut scratch = Vec::new();
    let mut i = 0usize;

    while let Some(start) = find_open_local(part, i, b"s") {
        let te = start + memchr::memchr(b'>', &part[start..n]).unwrap_or(n - start);
        let tag = &part[start..te];
        let type_name = attr_string(tag, b"t", &mut scratch).unwrap_or_default();
        let block_end = elem_end(part, start, b"s");
        let body: &[u8] = if te + 1 < block_end {
            &part[te + 1..block_end]
        } else {
            &[]
        };

        let mut keys = Vec::new();
        let mut j = 0usize;
        while let Some(ks) = find_open_local(body, j, b"k") {
            let kte = ks + memchr::memchr(b'>', &body[ks..]).unwrap_or(body.len() - ks);
            let ktag = &body[ks..kte];
            let name = attr_string(ktag, b"n", &mut scratch).unwrap_or_default();
            let ktype = attr_string(ktag, b"t", &mut scratch).unwrap_or_default();
            keys.push((name, ktype));
            j = kte.saturating_add(1);
        }

        out.push(RichValueStruct { type_name, keys });
        i = block_end.max(start.saturating_add(1));
        if i > n {
            break;
        }
    }
    Ok(out)
}

/// Parse the rich-value instances out of an already-inflated
/// `rdrichvalue.xml` part.
///
/// Fast path first: a part with no `rvData` root returns `Ok(vec![])` after a
/// single memmem probe. Each `<rv>` becomes one [`RichValue`]: its `s`
/// attribute is the `struct_index` (defaults to 0 when unparseable), and the
/// `<v>` children are collected in order, entity-decoded. Truncated or
/// unclosed `<v>` elements yield whatever values were readable — never a
/// panic.
pub fn parse_rich_values(part: &[u8]) -> TurboResult<Vec<RichValue>> {
    if memchr::memmem::find(part, b"rvData").is_none() {
        return Ok(Vec::new());
    }
    let n = part.len();
    let mut out = Vec::new();
    let mut scratch = Vec::new();
    let mut i = 0usize;

    while let Some(start) = find_open_local(part, i, b"rv") {
        let te = start + memchr::memchr(b'>', &part[start..n]).unwrap_or(n - start);
        let tag = &part[start..te];
        let struct_index = attr_usize(tag, b"s").unwrap_or(0);
        let block_end = elem_end(part, start, b"rv");
        let body: &[u8] = if te + 1 < block_end {
            &part[te + 1..block_end]
        } else {
            &[]
        };

        let mut values = Vec::new();
        let mut j = 0usize;
        while let Some(vs) = find_open_local(body, j, b"v") {
            let vte = vs + memchr::memchr(b'>', &body[vs..]).unwrap_or(body.len() - vs);
            if vte > 0 && body.get(vte - 1) == Some(&b'/') {
                values.push(String::new()); // self-closing <v/>
                j = vte.saturating_add(1);
                continue;
            }
            let vclose = find_close_local(body, vte.saturating_add(1), b"v").unwrap_or(body.len());
            if vte + 1 <= vclose && vclose <= body.len() {
                values.push(
                    String::from_utf8_lossy(decode_bytes(&body[vte + 1..vclose], &mut scratch))
                        .into_owned(),
                );
            } else {
                values.push(String::new());
            }
            j = vclose.saturating_add(1).max(vs.saturating_add(1));
            if j > body.len() {
                break;
            }
        }

        out.push(RichValue {
            struct_index,
            values,
        });
        i = block_end.max(start.saturating_add(1));
        if i > n {
            break;
        }
    }
    Ok(out)
}

/// Render a rich value as (key name, value) pairs by zipping its `<v>` values
/// against its structure's `<k>` keys.
///
/// Returns `None` when `index` is out of range or the value's `struct_index`
/// does not exist in `structures`. Extra values beyond the key count are
/// dropped; missing ones are simply absent from the result. Never panics,
/// never indexes out of bounds.
pub fn resolve(
    values: &[RichValue],
    structures: &[RichValueStruct],
    index: usize,
) -> Option<Vec<(String, String)>> {
    let rv = values.get(index)?;
    let st = structures.get(rv.struct_index)?;
    let mut out = Vec::with_capacity(st.keys.len().min(rv.values.len()));
    for (key, value) in st.keys.iter().zip(rv.values.iter()) {
        out.push((key.0.clone(), value.clone()));
    }
    Some(out)
}

/// Parse the rich-value index list out of an already-inflated `xl/metadata.xml`
/// part: the `<valueMetadata><bk><rc v="N"/>…` entries, in order.
///
/// A cell's `vm` attribute is **1-based** into this list, while the returned
/// `Vec` is **0-based** — so a cell with `vm="1"` resolves to rich-value index
/// `meta[0]`, `vm="2"` to `meta[1]`, and so on. `vm` values beyond
/// `meta.len()` have no rich value. Fast path first: a part with no
/// `valueMetadata` returns `Ok(vec![])` after a single probe. Non-numeric `v`
/// attributes are skipped; never panics.
pub fn parse_value_metadata(metadata_xml: &[u8]) -> TurboResult<Vec<usize>> {
    if memchr::memmem::find(metadata_xml, b"valueMetadata").is_none() {
        return Ok(Vec::new());
    }
    let n = metadata_xml.len();
    let mut out = Vec::new();
    let mut i = 0usize;

    while let Some(start) = find_open_local(metadata_xml, i, b"rc") {
        let te = start + memchr::memchr(b'>', &metadata_xml[start..n]).unwrap_or(n - start);
        let tag = &metadata_xml[start..te];
        if let Some(idx) = attr_usize(tag, b"v") {
            out.push(idx);
        }
        i = te.saturating_add(1);
        if i > n {
            break;
        }
    }
    Ok(out)
}

// ----------------------------------------------------------------------------
// Pass-through inventory
// ----------------------------------------------------------------------------

/// Every `xl/richData/` entry plus `xl/metadata.xml` when present, for the
/// byte-preserving edit path to carry through untouched. One pass over the
/// central directory, no inflation; a workbook with no rich data returns an
/// empty list.
pub fn rich_data_part_names(zip_bytes: &[u8]) -> TurboResult<Vec<String>> {
    let (entries, _errors) = list_entries(zip_bytes)?;
    let mut out = Vec::new();
    let mut has_metadata = false;
    for e in entries {
        if e.name == "xl/metadata.xml" {
            has_metadata = true;
        } else if e.name.starts_with("xl/richData/") {
            out.push(e.name);
        }
    }
    if has_metadata {
        out.push("xl/metadata.xml".into());
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STRUCTURES: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<rvStructures xmlns="http://schemas.microsoft.com/office/spreadsheetml/2017/richdata" count="2">
  <s t="_linkedEntity2">
    <k n="_rvRel:LocalImageIdentifier" t="i"/>
    <k n="_display" t="s"/>
    <k n="local" t="s"/>
  </s>
  <s t="_linkedEntity">
    <k n="Symbol" t="s"/>
    <k n="Price" t="n"/>
    <k n="Change" t="n"/>
  </s>
</rvStructures>"#;

    const RICH_VALUES: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<rvData xmlns="http://schemas.microsoft.com/office/spreadsheetml/2017/richdata" count="3">
  <rv s="0"><v>rIdImage1</v><v>Alphabet &amp; Co</v><v>extra</v></rv>
  <rv s="1"><v>AAPL</v></rv>
  <rv s="1"><v>MSFT</v><v>999</v></rv>
</rvData>"#;

    const METADATA: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<metadata xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="2">
  <valueMetadata count="2">
    <bk>
      <rc v="0"/>
      <rc v="2"/>
    </bk>
  </valueMetadata>
  <metadataType name="XLRICHVALUE" minSupportedVersion="120000" copy="1" pasteAll="1" pasteValues="1" merge="1" splitFirst="1" rowColShift="1" clearFormats="1" clearComments="1" assign="1" coerce="1" cellMeta="1"/>
</metadata>"#;

    #[test]
    fn rv_structures_happy_path() {
        let out = parse_rich_value_structures(STRUCTURES).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].type_name, "_linkedEntity2");
        assert_eq!(
            out[0].keys,
            vec![
                ("_rvRel:LocalImageIdentifier".to_string(), "i".to_string()),
                ("_display".to_string(), "s".to_string()),
                ("local".to_string(), "s".to_string()),
            ]
        );
        assert_eq!(out[1].type_name, "_linkedEntity");
        assert_eq!(
            out[1].keys,
            vec![
                ("Symbol".to_string(), "s".to_string()),
                ("Price".to_string(), "n".to_string()),
                ("Change".to_string(), "n".to_string()),
            ]
        );
    }

    #[test]
    fn rv_structures_absent_fast_path() {
        assert!(parse_rich_value_structures(b"").unwrap().is_empty());
        assert!(
            parse_rich_value_structures(b"<worksheet/>")
                .unwrap()
                .is_empty()
        );
        // Namespace-prefixed root must still be found.
        let prefixed = b"<x:rvStructures xmlns:x=\"x\"><x:s t=\"t1\"><x:k n=\"a\" t=\"s\"/></x:s></x:rvStructures>";
        let out = parse_rich_value_structures(prefixed).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].type_name, "t1");
        assert_eq!(out[0].keys, vec![("a".to_string(), "s".to_string())]);
    }

    #[test]
    fn rv_rich_values_happy_path() {
        let out = parse_rich_values(RICH_VALUES).unwrap();
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].struct_index, 0);
        assert_eq!(
            out[0].values,
            vec![
                "rIdImage1".to_string(),
                "Alphabet & Co".to_string(),
                "extra".to_string()
            ]
        );
        assert_eq!(out[1].struct_index, 1);
        assert_eq!(out[1].values, vec!["AAPL".to_string()]);
        assert_eq!(out[2].struct_index, 1);
        assert_eq!(out[2].values, vec!["MSFT".to_string(), "999".to_string()]);
    }

    #[test]
    fn rv_rich_values_absent_fast_path() {
        assert!(parse_rich_values(b"").unwrap().is_empty());
        assert!(parse_rich_values(b"<worksheet/>").unwrap().is_empty());
    }

    #[test]
    fn rv_resolve_zip_pairs() {
        let structures = parse_rich_value_structures(STRUCTURES).unwrap();
        let values = parse_rich_values(RICH_VALUES).unwrap();
        // index 0 → struct 0, three values for three keys, fully zipped.
        let pairs = resolve(&values, &structures, 0).unwrap();
        assert_eq!(
            pairs,
            vec![
                (
                    "_rvRel:LocalImageIdentifier".to_string(),
                    "rIdImage1".to_string()
                ),
                ("_display".to_string(), "Alphabet & Co".to_string()),
                ("local".to_string(), "extra".to_string()),
            ]
        );
        // index 2 → struct 1, two values against three keys.
        let pairs = resolve(&values, &structures, 2).unwrap();
        assert_eq!(
            pairs,
            vec![
                ("Symbol".to_string(), "MSFT".to_string()),
                ("Price".to_string(), "999".to_string()),
            ]
        );
    }

    #[test]
    fn rv_resolve_extra_dropped_missing_absent() {
        let structures = parse_rich_value_structures(STRUCTURES).unwrap();
        let values = parse_rich_values(RICH_VALUES).unwrap();
        // index 1 has one value for struct 1's three keys → one pair, others absent.
        let pairs = resolve(&values, &structures, 1).unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0], ("Symbol".to_string(), "AAPL".to_string()));
        // Extra values beyond the key count are dropped.
        let rv = RichValue {
            struct_index: 0,
            values: vec!["a".into(), "b".into(), "c".into(), "d".into()],
        };
        let pairs = resolve(&[rv], &structures, 0).unwrap();
        assert_eq!(pairs.len(), 3);
    }

    #[test]
    fn rv_resolve_out_of_range_is_none() {
        let structures = parse_rich_value_structures(STRUCTURES).unwrap();
        let values = parse_rich_values(RICH_VALUES).unwrap();
        assert!(resolve(&values, &structures, 99).is_none());
        // struct_index that does not exist in structures.
        let rv = RichValue {
            struct_index: 77,
            values: vec!["x".into()],
        };
        assert!(resolve(&[rv], &structures, 0).is_none());
    }

    #[test]
    fn rv_value_metadata_happy_path() {
        // vm is 1-based into the returned 0-based Vec: vm="1" → meta[0] → 0.
        let meta = parse_value_metadata(METADATA).unwrap();
        assert_eq!(meta, vec![0, 2]);
        assert_eq!(meta[0], 0); // vm=1
        assert_eq!(meta[1], 2); // vm=2
    }

    #[test]
    fn rv_value_metadata_absent_and_non_numeric() {
        assert!(parse_value_metadata(b"").unwrap().is_empty());
        let non_numeric =
            b"<metadata><valueMetadata><bk><rc v=\"abc\"/><rc v=\"7\"/></bk></valueMetadata></metadata>";
        let out = parse_value_metadata(non_numeric).unwrap();
        assert_eq!(out, vec![7]);
    }

    /// Minimal store-only zip: names + payloads, correct offsets, CRC unused
    /// by the reader. Local headers, central directory and EOCD are all valid.
    fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip: Vec<u8> = Vec::new();
        let mut cd: Vec<u8> = Vec::new();
        for (name, data) in entries {
            let name_b = name.as_bytes();
            let local_off = zip.len() as u32;
            zip.extend_from_slice(&[0x50, 0x4b, 0x03, 0x04]);
            zip.extend_from_slice(&20u16.to_le_bytes()); // version needed
            zip.extend_from_slice(&0u16.to_le_bytes()); // flags
            zip.extend_from_slice(&0u16.to_le_bytes()); // method: stored
            zip.extend_from_slice(&0u16.to_le_bytes()); // mod time
            zip.extend_from_slice(&0u16.to_le_bytes()); // mod date
            zip.extend_from_slice(&0u32.to_le_bytes()); // crc (unchecked)
            zip.extend_from_slice(&(data.len() as u32).to_le_bytes());
            zip.extend_from_slice(&(data.len() as u32).to_le_bytes());
            zip.extend_from_slice(&(name_b.len() as u16).to_le_bytes());
            zip.extend_from_slice(&0u16.to_le_bytes()); // extra len
            zip.extend_from_slice(name_b);
            zip.extend_from_slice(data);

            cd.extend_from_slice(&[0x50, 0x4b, 0x01, 0x02]);
            cd.extend_from_slice(&20u16.to_le_bytes()); // version made by
            cd.extend_from_slice(&20u16.to_le_bytes()); // version needed
            cd.extend_from_slice(&0u16.to_le_bytes()); // flags
            cd.extend_from_slice(&0u16.to_le_bytes()); // method
            cd.extend_from_slice(&0u16.to_le_bytes()); // mod time
            cd.extend_from_slice(&0u16.to_le_bytes()); // mod date
            cd.extend_from_slice(&0u32.to_le_bytes()); // crc
            cd.extend_from_slice(&(data.len() as u32).to_le_bytes());
            cd.extend_from_slice(&(data.len() as u32).to_le_bytes());
            cd.extend_from_slice(&(name_b.len() as u16).to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes()); // extra len
            cd.extend_from_slice(&0u16.to_le_bytes()); // comment len
            cd.extend_from_slice(&0u16.to_le_bytes()); // disk start
            cd.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
            cd.extend_from_slice(&0u32.to_le_bytes()); // external attrs
            cd.extend_from_slice(&local_off.to_le_bytes());
            cd.extend_from_slice(name_b);
        }
        let cd_offset = zip.len() as u32;
        zip.extend_from_slice(&cd);
        zip.extend_from_slice(&[0x50, 0x4b, 0x05, 0x06]);
        zip.extend_from_slice(&0u16.to_le_bytes()); // disk
        zip.extend_from_slice(&0u16.to_le_bytes()); // cd start disk
        zip.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        zip.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        zip.extend_from_slice(&(cd.len() as u32).to_le_bytes());
        zip.extend_from_slice(&cd_offset.to_le_bytes());
        zip.extend_from_slice(&0u16.to_le_bytes()); // comment len
        zip
    }

    #[test]
    fn rv_part_names_lists_pass_through() {
        let zip = build_zip(&[
            ("xl/workbook.xml", b"<workbook/>"),
            ("xl/worksheets/sheet1.xml", b"<worksheet/>"),
            ("xl/richData/rdrichvalue.xml", b"<rvData/>"),
            ("xl/richData/rdrichvaluestructure.xml", b"<rvStructures/>"),
            ("xl/richData/rdRichValueTypes.xml", b"<richValueTypes/>"),
            ("xl/metadata.xml", b"<metadata/>"),
        ]);
        let names = rich_data_part_names(&zip).unwrap();
        assert_eq!(
            names,
            vec![
                "xl/metadata.xml",
                "xl/richData/rdRichValueTypes.xml",
                "xl/richData/rdrichvalue.xml",
                "xl/richData/rdrichvaluestructure.xml",
            ]
        );
    }

    #[test]
    fn rv_part_names_metadata_only() {
        // metadata.xml without any richData/ dir still carries through.
        let zip = build_zip(&[
            ("xl/workbook.xml", b"<workbook/>"),
            ("xl/metadata.xml", b"<metadata/>"),
        ]);
        let names = rich_data_part_names(&zip).unwrap();
        assert_eq!(names, vec!["xl/metadata.xml"]);
    }

    #[test]
    fn rv_part_names_absent_fast_path() {
        let zip = build_zip(&[("xl/workbook.xml", b"<workbook/>")]);
        assert!(rich_data_part_names(&zip).unwrap().is_empty());
    }

    #[test]
    fn rv_part_names_malformed_zip() {
        assert!(rich_data_part_names(b"not a zip").is_err());
        // Truncated EOCD — Err or empty, never a panic.
        let cut: &[u8] = &build_zip(&[("xl/richData/rdrichvalue.xml", b"<rvData/>")])[..20];
        let r = rich_data_part_names(cut);
        assert!(r.is_err() || r.unwrap().is_empty());
    }

    #[test]
    fn rv_malformed_structures_no_panic() {
        // Truncated mid-attribute: nothing to read, but no panic.
        let truncated = b"<rvStructures count=\"2\"><s t=\"_linkedEntity2\"><k n=\"_rvRel";
        let out = parse_rich_value_structures(truncated).unwrap();
        assert!(out.len() <= 1);
        // Junk that never forms a structure.
        assert!(parse_rich_value_structures(b"<><><>").unwrap().is_empty());
    }

    #[test]
    fn rv_malformed_values_no_panic() {
        // Open rv with an unterminated <v> — tolerant partial result.
        let truncated = b"<rvData><rv s=\"0\"><v>unterminated";
        let out = parse_rich_values(truncated).unwrap();
        assert!(out.len() <= 1);
        // Junk with no rv elements.
        assert!(
            parse_rich_values(b"garbage without tags <rvData")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn rv_malformed_metadata_no_panic() {
        // Unclosed rc attribute — skipped, empty result, no panic.
        let truncated = b"<metadata><valueMetadata><bk><rc v=\"5";
        assert!(parse_value_metadata(truncated).unwrap().is_empty());
        // Junk.
        assert!(
            parse_value_metadata(b"<><valueMetadata>")
                .unwrap()
                .is_empty()
        );
    }
}
