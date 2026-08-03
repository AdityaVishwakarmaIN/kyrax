//! Slicers and timelines — inventory and preserve (Tier 3 MEDIUM).
//!
//! openpyxl has no slicer support, so a save through it silently drops a
//! workbook's slicers, timelines and their caches. kyrax fixes that the same way
//! it fixes every other sidecar: inventory what is there, and hand the
//! byte-preserving edit path the list of parts to carry through untouched.
//! This module authors nothing — no slicer XML is ever written here.
//!
//! Parts: `xl/slicers/slicerN.xml` (`<slicers><slicer name= cache= caption=
//! columnCount= …/></slicers>`), `xl/slicerCaches/slicerCacheN.xml`
//! (`<slicerCacheDefinition name= sourceName=>`), `xl/timelines/timelineN.xml`
//! (`<timelines><timeline name= cache= caption= …/></timelines>`) and
//! `xl/timelineCaches/timelineCacheN.xml`. Worksheets reference slicers through
//! an `<ext>` in the worksheet `extLst` whose URI is
//! `{A8765BA9-456A-4dab-B4F3-ACF1C6B7B5FF}` and which carries
//! `<x14:slicerList><x14:slicer r:id=…/></x14:slicerList>`; timelines use URI
//! `{7E03D99C-DC04-49d9-9315-930204A7B6E9}`.
//!
//! The absent case — most workbooks — costs one central-directory name pass and
//! zero inflates. Only the parts that actually exist are inflated.

use crate::turbo::error::{TurboError, TurboResult};
use crate::turbo::structural::find_attr;
use crate::turbo::zipmin::{ZipEntryMeta, inflate_entry, list_entries};

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// One `<slicer>` entry from `xl/slicers/slicerN.xml`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlicerRef {
    pub name: String,
    pub cache: String,
    pub caption: Option<String>,
    pub column_count: Option<u32>,
}

/// One `<timeline>` entry from `xl/timelines/timelineN.xml`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineRef {
    pub name: String,
    pub cache: String,
    pub caption: Option<String>,
}

/// One `<slicerCacheDefinition>` from `xl/slicerCaches/slicerCacheN.xml`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlicerCache {
    pub name: String,
    pub source_name: Option<String>,
    pub pivot_tables: Vec<String>,
}

/// Everything a workbook holds that kyrax must preserve: the slicer/timeline
/// parts themselves, their caches, and the worksheet `extLst` `r:id` refs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SlicerInventory {
    pub slicers: Vec<SlicerRef>,
    pub timelines: Vec<TimelineRef>,
    pub slicer_caches: Vec<SlicerCache>,
    pub timeline_cache_names: Vec<String>,
    pub sheet_slicer_refs: Vec<String>,
}

// ---------------------------------------------------------------------------
// Byte-scanning helpers (local-name tolerant, like src/turbo/structural.rs)
// ---------------------------------------------------------------------------

/// True if `xml[pos..]` is an open tag with local name `local` (`<local …>` or `<ns:local …>`).
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

/// Byte offset of the next open tag with local name `local` at or after `from`.
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

/// Value of attribute `name` on `tag`, entity-decoded.
fn attr_str<'a>(tag: &'a [u8], name: &[u8], scratch: &'a mut Vec<u8>) -> Option<String> {
    find_attr(tag, name).map(|raw| {
        String::from_utf8_lossy(crate::turbo::decode::decode_bytes(raw, scratch)).into_owned()
    })
}

// ---------------------------------------------------------------------------
// Per-part parsers
// ---------------------------------------------------------------------------

/// Parse every `<slicer>` entry out of an already-inflated `xl/slicers/slicerN.xml`.
///
/// A slicer is fully described by its open tag, so each entry is one attribute
/// read. A `name` or `cache` attribute is required; a part that cannot supply
/// one is `Err(Format)` rather than a panic.
pub fn parse_slicers(part: &[u8]) -> TurboResult<Vec<SlicerRef>> {
    let mut out = Vec::new();
    let mut scratch = Vec::new();
    let n = part.len();
    let mut i = 0usize;
    while let Some(start) = find_open_local(part, i, b"slicer") {
        let te = start + memchr::memchr(b'>', &part[start..n]).unwrap_or(n - start);
        let tag = &part[start..te];
        let name = attr_str(tag, b"name", &mut scratch)
            .ok_or_else(|| TurboError::Format("slicer missing name attribute".into()))?;
        let cache = attr_str(tag, b"cache", &mut scratch)
            .ok_or_else(|| TurboError::Format("slicer missing cache attribute".into()))?;
        let caption = attr_str(tag, b"caption", &mut scratch);
        let column_count =
            find_attr(tag, b"columnCount").and_then(|v| std::str::from_utf8(v).ok()?.parse().ok());
        out.push(SlicerRef {
            name,
            cache,
            caption,
            column_count,
        });
        i = te + 1;
    }
    Ok(out)
}

/// Parse every `<timeline>` entry out of an already-inflated `xl/timelines/timelineN.xml`.
pub fn parse_timelines(part: &[u8]) -> TurboResult<Vec<TimelineRef>> {
    let mut out = Vec::new();
    let mut scratch = Vec::new();
    let n = part.len();
    let mut i = 0usize;
    while let Some(start) = find_open_local(part, i, b"timeline") {
        let te = start + memchr::memchr(b'>', &part[start..n]).unwrap_or(n - start);
        let tag = &part[start..te];
        let name = attr_str(tag, b"name", &mut scratch)
            .ok_or_else(|| TurboError::Format("timeline missing name attribute".into()))?;
        let cache = attr_str(tag, b"cache", &mut scratch)
            .ok_or_else(|| TurboError::Format("timeline missing cache attribute".into()))?;
        let caption = attr_str(tag, b"caption", &mut scratch);
        out.push(TimelineRef {
            name,
            cache,
            caption,
        });
        i = te + 1;
    }
    Ok(out)
}

/// Parse one already-inflated `xl/slicerCaches/slicerCacheN.xml`.
///
/// `name` comes from the root `<slicerCacheDefinition>` open tag and is
/// required; `sourceName` is optional. `pivot_tables` are the `name` attributes
/// of the `<pivotTable>` children of the `<pivotTables>` container.
pub fn parse_slicer_cache(part: &[u8]) -> TurboResult<SlicerCache> {
    let n = part.len();
    let start = find_open_local(part, 0, b"slicerCacheDefinition")
        .ok_or_else(|| TurboError::Format("slicerCacheDefinition root element not found".into()))?;
    let te = start + memchr::memchr(b'>', &part[start..n]).unwrap_or(n - start);
    let tag = &part[start..te];
    let mut scratch = Vec::new();
    let name = attr_str(tag, b"name", &mut scratch)
        .ok_or_else(|| TurboError::Format("slicerCacheDefinition missing name attribute".into()))?;
    let source_name = attr_str(tag, b"sourceName", &mut scratch);

    let mut pivot_tables = Vec::new();
    let mut i = 0usize;
    while let Some(ps) = find_open_local(part, i, b"pivotTable") {
        let pte = ps + memchr::memchr(b'>', &part[ps..n]).unwrap_or(n - ps);
        let ptag = &part[ps..pte];
        if let Some(pname) = attr_str(ptag, b"name", &mut scratch) {
            pivot_tables.push(pname);
        }
        i = pte + 1;
    }

    Ok(SlicerCache {
        name,
        source_name,
        pivot_tables,
    })
}

/// The `name` attribute of an already-inflated `xl/timelineCaches/timelineCacheN.xml`.
fn timeline_cache_name(part: &[u8]) -> TurboResult<Option<String>> {
    let n = part.len();
    let start = find_open_local(part, 0, b"timelineCacheDefinition").ok_or_else(|| {
        TurboError::Format("timelineCacheDefinition root element not found".into())
    })?;
    let te = start + memchr::memchr(b'>', &part[start..n]).unwrap_or(n - start);
    let tag = &part[start..te];
    let mut scratch = Vec::new();
    Ok(attr_str(tag, b"name", &mut scratch))
}

/// The `r:id` list from the worksheet `extLst` slicer extension.
///
/// Fast path first: if the sheet part does not contain the substring
/// `slicerList`, this returns `Ok(vec![])` after one memmem probe and touches
/// nothing else — absent is the common case and must cost one scan. Slicer
/// elements without an `r:id` are skipped, never a panic.
pub fn parse_sheet_slicer_ext(sheet_xml: &[u8]) -> TurboResult<Vec<String>> {
    if memchr::memmem::find(sheet_xml, b"slicerList").is_none() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let n = sheet_xml.len();
    let mut i = 0usize;
    while let Some(start) = find_open_local(sheet_xml, i, b"slicer") {
        let te = start + memchr::memchr(b'>', &sheet_xml[start..n]).unwrap_or(n - start);
        let tag = &sheet_xml[start..te];
        if let Some(rid) = find_attr(tag, b"r:id") {
            out.push(String::from_utf8_lossy(rid).into_owned());
        }
        i = te + 1;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Zip-level inventory
// ---------------------------------------------------------------------------

/// True when a worksheet rels part carries a slicer relationship. Sheet-level
/// slicer rels have `Type` ending in `/slicer`; timelines end in `/timeline`.
fn rels_has_slicer(rels: &[u8]) -> bool {
    let mut i = 0usize;
    let n = rels.len();
    while let Some(o) = memchr::memmem::find(&rels[i..n], b"<Relationship ") {
        let start = i + o;
        let te = start + memchr::memchr(b'>', &rels[start..n]).unwrap_or(n - start);
        let tag = &rels[start..te];
        if let Some(t) = find_attr(tag, b"Type") {
            if t.ends_with(b"/slicer") {
                return true;
            }
        }
        i = te + 1;
    }
    false
}

/// Collect the slicer `r:id` refs from every worksheet that actually references
/// slicers. Sheets are filtered by their rels part first so a sheet with no
/// slicer relationship is never inflated; only its (tiny) rels part is read.
fn scan_sheet_slicer_refs(zip_bytes: &[u8], entries: &[ZipEntryMeta]) -> TurboResult<Vec<String>> {
    let mut refs = Vec::new();
    for e in entries {
        let Some(file) = e.name.strip_prefix("xl/worksheets/sheet") else {
            continue;
        };
        let Some(stem) = file.strip_suffix(".xml") else {
            continue;
        };
        if stem.is_empty() || stem.contains('/') {
            continue;
        }
        let rels_path = format!("xl/worksheets/_rels/sheet{stem}.xml.rels");
        let has_slicer = match entries.iter().find(|r| r.name == rels_path) {
            Some(rel_meta) => {
                let rels = inflate_entry(zip_bytes, rel_meta)?;
                rels_has_slicer(&rels)
            }
            None => false,
        };
        if has_slicer {
            let sheet_xml = inflate_entry(zip_bytes, e)?;
            refs.extend(parse_sheet_slicer_ext(&sheet_xml)?);
        }
    }
    Ok(refs)
}

/// Inventory the slicer/timeline parts of a zip-backed workbook.
///
/// One central-directory pass collects every entry name; only the
/// `xl/slicers/`, `xl/slicerCaches/`, `xl/timelines/` and `xl/timelineCaches/`
/// parts that actually exist are inflated. A workbook with none of these parts
/// costs the single name pass and nothing else — the empty inventory is
/// returned before any inflate.
pub fn inventory_slicers(zip_bytes: &[u8]) -> TurboResult<SlicerInventory> {
    let (entries, _errors) = list_entries(zip_bytes)?;

    let mut inv = SlicerInventory::default();
    let mut seen = false;
    for e in &entries {
        let name = e.name.as_str();
        if name.starts_with("xl/slicers/") {
            seen = true;
            let xml = inflate_entry(zip_bytes, e)?;
            inv.slicers.extend(parse_slicers(&xml)?);
        } else if name.starts_with("xl/timelines/") {
            seen = true;
            let xml = inflate_entry(zip_bytes, e)?;
            inv.timelines.extend(parse_timelines(&xml)?);
        } else if name.starts_with("xl/slicerCaches/") {
            seen = true;
            let xml = inflate_entry(zip_bytes, e)?;
            inv.slicer_caches.push(parse_slicer_cache(&xml)?);
        } else if name.starts_with("xl/timelineCaches/") {
            seen = true;
            let xml = inflate_entry(zip_bytes, e)?;
            if let Some(nm) = timeline_cache_name(&xml)? {
                inv.timeline_cache_names.push(nm);
            }
        }
    }

    // Sheet-level refs only when some slicer/timeline part exists.
    if seen {
        inv.sheet_slicer_refs = scan_sheet_slicer_refs(zip_bytes, &entries)?;
    }

    Ok(inv)
}

/// Every zip entry that belongs to a slicer or timeline, so the byte-preserving
/// edit path can pass them through untouched. Empty when the workbook has none.
pub fn slicer_part_names(zip_bytes: &[u8]) -> TurboResult<Vec<String>> {
    let (entries, _errors) = list_entries(zip_bytes)?;
    let mut out = Vec::new();
    for e in &entries {
        let name = e.name.as_str();
        if name.starts_with("xl/slicers/")
            || name.starts_with("xl/slicerCaches/")
            || name.starts_with("xl/timelines/")
            || name.starts_with("xl/timelineCaches/")
        {
            out.push(e.name.clone());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLICER_URI: &str = "{A8765BA9-456A-4dab-B4F3-ACF1C6B7B5FF}";

    const SLICERS_XML: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<slicers xmlns="http://schemas.microsoft.com/office/spreadsheetml/2009/9/main">
  <slicer name="Slicer_Product" cache="SlicerCache_Product" caption="Product &amp; Line" columnCount="2"/>
  <slicer name="Slicer_Region" cache="SlicerCache_Region"/>
</slicers>"#;

    const TIMELINES_XML: &[u8] =
        br#"<timelines xmlns="http://schemas.microsoft.com/office/spreadsheetml/2009/9/main">
  <timeline name="Timeline_Date" cache="TimelineCache_Date" caption="Order Date"/>
</timelines>"#;

    const SLICER_CACHE_XML: &[u8] = br#"<slicerCacheDefinition xmlns="http://schemas.microsoft.com/office/spreadsheetml/2009/9/main" name="SlicerCache_Product" sourceName="Table1" filterColumn="0">
  <pivotTables>
    <pivotTable tabId="0" name="PivotTable1"/>
    <pivotTable tabId="0" name="PivotTable2"/>
  </pivotTables>
  <slicerCacheData>
    <slicerCacheColumn count="2">
      <slicerCacheItem uniqueName="[Table1].Product" xDynamic="0"/>
    </slicerCacheColumn>
  </slicerCacheData>
</slicerCacheDefinition>"#;

    const TIMELINE_CACHE_XML: &[u8] =
        br#"<timelineCacheDefinition name="TimelineCache_Date" sourceName="Dates"/>"#;

    const SHEET_WITH_SLICER_EXT: &[u8] = br#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData/><extLst><ext uri="{A8765BA9-456A-4dab-B4F3-ACF1C6B7B5FF}" xmlns:x14="http://schemas.microsoft.com/office/spreadsheetml/2009/9/main"><x14:slicerList><x14:slicer r:id="rId1"/></x14:slicerList></ext></extLst></worksheet>"#;

    const SHEET_RELS_WITH_SLICER: &[u8] = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.microsoft.com/office/2007/relationships/slicer" Target="slicers/slicer1.xml"/></Relationships>"#;

    fn w_u16(out: &mut Vec<u8>, v: u16) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    fn w_u32(out: &mut Vec<u8>, v: u32) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    /// Build a STORE-method zip with the given `(name, payload)` entries. CRC
    /// is not computed; this module never verifies it, so a zero is fine.
    fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cd = Vec::new();
        for (name, payload) in entries {
            let name_b = name.as_bytes();
            let lh_pos = out.len() as u32;
            out.extend_from_slice(b"PK\x03\x04");
            w_u16(&mut out, 20);
            w_u16(&mut out, 0);
            w_u16(&mut out, 0);
            w_u16(&mut out, 0);
            w_u16(&mut out, 0);
            w_u32(&mut out, 0);
            w_u32(&mut out, payload.len() as u32);
            w_u32(&mut out, payload.len() as u32);
            w_u16(&mut out, name_b.len() as u16);
            w_u16(&mut out, 0);
            out.extend_from_slice(name_b);
            out.extend_from_slice(payload);

            cd.extend_from_slice(b"PK\x01\x02");
            w_u16(&mut cd, 20);
            w_u16(&mut cd, 20);
            w_u16(&mut cd, 0);
            w_u16(&mut cd, 0);
            w_u16(&mut cd, 0);
            w_u16(&mut cd, 0);
            w_u32(&mut cd, 0);
            w_u32(&mut cd, payload.len() as u32);
            w_u32(&mut cd, payload.len() as u32);
            w_u16(&mut cd, name_b.len() as u16);
            w_u16(&mut cd, 0);
            w_u16(&mut cd, 0);
            w_u16(&mut cd, 0);
            w_u16(&mut cd, 0);
            // External attributes are the last field before the local-header
            // offset. A stray u16 here shifts the offset by two bytes and every
            // later record with it, so the directory parses but every payload
            // points at garbage.
            w_u32(&mut cd, 0);
            w_u32(&mut cd, lh_pos);
            cd.extend_from_slice(name_b);
        }
        let cd_offset = out.len() as u32;
        let cd_size = cd.len() as u32;
        out.extend_from_slice(&cd);
        out.extend_from_slice(b"PK\x05\x06");
        w_u16(&mut out, 0);
        w_u16(&mut out, 0);
        w_u16(&mut out, entries.len() as u16);
        w_u16(&mut out, entries.len() as u16);
        w_u32(&mut out, cd_size);
        w_u32(&mut out, cd_offset);
        w_u16(&mut out, 0);
        out
    }

    // -------------------------------------------------------------------
    // Per-part parsers
    // -------------------------------------------------------------------

    #[test]
    fn slicer_parse_slicers_happy_path() {
        let got = parse_slicers(SLICERS_XML).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "Slicer_Product");
        assert_eq!(got[0].cache, "SlicerCache_Product");
        assert_eq!(got[0].caption.as_deref(), Some("Product & Line"));
        assert_eq!(got[0].column_count, Some(2));
        assert_eq!(got[1].name, "Slicer_Region");
        assert_eq!(got[1].caption, None);
        assert_eq!(got[1].column_count, None);
    }

    #[test]
    fn slicer_parse_timelines_happy_path() {
        let got = parse_timelines(TIMELINES_XML).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "Timeline_Date");
        assert_eq!(got[0].cache, "TimelineCache_Date");
        assert_eq!(got[0].caption.as_deref(), Some("Order Date"));
    }

    #[test]
    fn slicer_parse_slicer_cache_happy_path() {
        let got = parse_slicer_cache(SLICER_CACHE_XML).unwrap();
        assert_eq!(got.name, "SlicerCache_Product");
        assert_eq!(got.source_name.as_deref(), Some("Table1"));
        assert_eq!(got.pivot_tables, vec!["PivotTable1", "PivotTable2"]);
    }

    #[test]
    fn slicer_parse_sheet_ext_happy_path() {
        let got = parse_sheet_slicer_ext(SHEET_WITH_SLICER_EXT).unwrap();
        assert_eq!(got, vec!["rId1"]);
    }

    #[test]
    fn slicer_parse_sheet_ext_absent_is_empty() {
        let sheet = b"<worksheet xmlns=\"x\"><sheetData><c r=\"A1\"/></sheetData></worksheet>";
        assert!(parse_sheet_slicer_ext(sheet).unwrap().is_empty());
    }

    #[test]
    fn slicer_parse_sheet_ext_ignores_timeline_list() {
        // A timelineList ext must not leak its r:ids into the slicer refs.
        let sheet = br#"<worksheet><extLst><ext uri="{7E03D99C-DC04-49d9-9315-930204A7B6E9}"><x14:timelineList xmlns:x14="x"><x14:timeline r:id="rId9"/></x14:timelineList></ext></extLst></worksheet>"#;
        assert!(parse_sheet_slicer_ext(sheet).unwrap().is_empty());
    }

    #[test]
    fn slicer_parse_slicers_missing_name_is_err() {
        let xml = br#"<slicers><slicer cache="c1"/></slicers>"#;
        assert!(parse_slicers(xml).is_err());
    }

    #[test]
    fn slicer_parse_timelines_truncated_is_err() {
        let xml = b"<timelines><timeline name=\"t1\" cache=\"c1";
        assert!(parse_timelines(xml).is_err());
    }

    #[test]
    fn slicer_parse_slicer_cache_missing_name_is_err() {
        let xml = br#"<slicerCacheDefinition sourceName="Table1"/>"#;
        assert!(parse_slicer_cache(xml).is_err());
    }

    #[test]
    fn slicer_parse_sheet_ext_slicer_without_rid_is_empty() {
        let sheet = br#"<worksheet><extLst><ext uri="{A8765BA9-456A-4dab-B4F3-ACF1C6B7B5FF}"><x14:slicerList xmlns:x14="x"><x14:slicer/></x14:slicerList></ext></extLst></worksheet>"#;
        assert!(parse_sheet_slicer_ext(sheet).unwrap().is_empty());
    }

    #[test]
    fn slicer_parse_slicers_garbage_no_panic() {
        // No '<slicer ' tag at all yields an empty vec, never a panic.
        assert!(parse_slicers(b"<slicers/>").unwrap().is_empty());
        assert!(parse_slicers(b"garbage").unwrap().is_empty());
    }

    // -------------------------------------------------------------------
    // Zip-level inventory
    // -------------------------------------------------------------------

    #[test]
    fn slicer_inventory_absent_parts_is_empty() {
        let zip = build_zip(&[("xl/workbook.xml", b"<workbook/>")]);
        let inv = inventory_slicers(&zip).unwrap();
        assert!(inv.slicers.is_empty());
        assert!(inv.timelines.is_empty());
        assert!(inv.slicer_caches.is_empty());
        assert!(inv.timeline_cache_names.is_empty());
        assert!(inv.sheet_slicer_refs.is_empty());
        assert!(slicer_part_names(&zip).unwrap().is_empty());
    }

    #[test]
    fn slicer_inventory_happy_path() {
        let zip = build_zip(&[
            ("xl/workbook.xml", b"<workbook/>"),
            ("xl/slicers/slicer1.xml", SLICERS_XML),
            ("xl/slicerCaches/slicerCache1.xml", SLICER_CACHE_XML),
            ("xl/timelines/timeline1.xml", TIMELINES_XML),
            ("xl/timelineCaches/timelineCache1.xml", TIMELINE_CACHE_XML),
            ("xl/worksheets/sheet1.xml", SHEET_WITH_SLICER_EXT),
            (
                "xl/worksheets/_rels/sheet1.xml.rels",
                SHEET_RELS_WITH_SLICER,
            ),
        ]);
        let inv = inventory_slicers(&zip).unwrap();
        assert_eq!(inv.slicers.len(), 2);
        assert_eq!(inv.slicers[0].name, "Slicer_Product");
        assert_eq!(inv.slicers[0].column_count, Some(2));
        assert_eq!(inv.timelines.len(), 1);
        assert_eq!(inv.timelines[0].name, "Timeline_Date");
        assert_eq!(inv.slicer_caches.len(), 1);
        assert_eq!(inv.slicer_caches[0].name, "SlicerCache_Product");
        assert_eq!(
            inv.slicer_caches[0].pivot_tables,
            vec!["PivotTable1", "PivotTable2"]
        );
        assert_eq!(inv.timeline_cache_names, vec!["TimelineCache_Date"]);
        assert_eq!(inv.sheet_slicer_refs, vec!["rId1"]);
    }

    #[test]
    fn slicer_inventory_sheet_without_slicer_rel_is_ignored() {
        // A worksheet with no slicer relationship must not be inflated and
        // must not contribute any refs.
        let zip = build_zip(&[
            ("xl/slicers/slicer1.xml", SLICERS_XML),
            ("xl/worksheets/sheet1.xml", SHEET_WITH_SLICER_EXT),
        ]);
        let inv = inventory_slicers(&zip).unwrap();
        assert_eq!(inv.slicers.len(), 2);
        assert!(inv.sheet_slicer_refs.is_empty());
    }

    #[test]
    fn slicer_part_names_lists_only_slicer_entries() {
        let zip = build_zip(&[
            ("xl/workbook.xml", b"<workbook/>"),
            ("xl/slicers/slicer1.xml", SLICERS_XML),
            ("xl/slicerCaches/slicerCache1.xml", SLICER_CACHE_XML),
            ("xl/timelines/timeline1.xml", TIMELINES_XML),
            ("xl/timelineCaches/timelineCache1.xml", TIMELINE_CACHE_XML),
            ("xl/worksheets/sheet1.xml", SHEET_WITH_SLICER_EXT),
            ("xl/sharedStrings.xml", b"<sst/>"),
        ]);
        let names = slicer_part_names(&zip).unwrap();
        assert_eq!(
            names,
            vec![
                "xl/slicers/slicer1.xml",
                "xl/slicerCaches/slicerCache1.xml",
                "xl/timelines/timeline1.xml",
                "xl/timelineCaches/timelineCache1.xml",
            ]
        );
    }

    #[test]
    fn slicer_inventory_corrupt_zip_is_err() {
        assert!(inventory_slicers(b"this is not a zip file at all").is_err());
        assert!(slicer_part_names(b"this is not a zip file at all").is_err());
    }

    #[test]
    fn slicer_inventory_truncated_zip_is_err() {
        let mut zip = build_zip(&[("xl/slicers/slicer1.xml", SLICERS_XML)]);
        zip.truncate(20);
        assert!(inventory_slicers(&zip).is_err());
    }
}
