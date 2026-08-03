//! Power Query and data-model parts: inventory and preserve.
//!
//! openpyxl silently destroys a workbook's Power Query mashup, its
//! `xl/connections.xml`, the `xl/queryTables/*` and the data model at
//! `xl/model/` on save — a real data-loss bug kyrax is positioned to beat.
//! This module does not run M code (out of scope) and never base64-decodes the
//! `<DataMashup>` payload (real CPU for no gain). It inventories what is there
//! with a single central-directory pass — zero inflates when nothing is present
//! — and names every part the byte-preserving edit path must carry through
//! untouched, so an edit here can never be the thing that orphans a query.

use crate::turbo::decode::decode_bytes;
use crate::turbo::error::{TurboError, TurboResult};
use crate::turbo::structural::find_attr;
use crate::turbo::zipmin::inflate;

/// One `<connection>` record from `xl/connections.xml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    pub id: String,
    pub name: String,
    pub kind: Option<String>,
    pub command: Option<String>,
    /// True when the `dbPr` connection string names `Microsoft.Mashup`.
    pub is_power_query: bool,
}

/// Everything a workbook holds that kyrax must preserve byte-identical.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PowerQueryInventory {
    pub connections: Vec<Connection>,
    /// True when a `customXml/item*.xml` part carries a `<DataMashup>` element.
    pub has_data_mashup: bool,
    pub custom_xml_parts: Vec<String>,
    pub query_table_parts: Vec<String>,
    pub model_parts: Vec<String>,
}

/// Parse the `<connection>` records out of an already-inflated
/// `xl/connections.xml`. Tolerant: malformed elements that cannot be delimited
/// are skipped rather than panicking; a part with nothing parseable yields an
/// empty `Vec`.
pub fn parse_connections(part: &[u8]) -> TurboResult<Vec<Connection>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(o) = memchr::memmem::find(&part[pos..], b"<connection ") {
        let start = pos + o;
        let Some(gt) = memchr::memchr(b'>', &part[start..]) else {
            break;
        };
        let open_end = start + gt;
        let open_tag = &part[start..open_end];
        let body_start = open_end + 1;
        let body_end = memchr::memmem::find(&part[body_start..], b"</connection>")
            .map(|p| body_start + p)
            .unwrap_or(part.len());
        let body = &part[body_start..body_end];
        let mut scratch = Vec::new();
        let id = attr_str(open_tag, b"id", &mut scratch).unwrap_or_default();
        let name = attr_str(open_tag, b"name", &mut scratch).unwrap_or_default();
        let kind = attr_str(open_tag, b"type", &mut scratch);
        let (command, is_power_query) = parse_dbpr(body, &mut scratch);
        out.push(Connection {
            id,
            name,
            kind,
            command,
            is_power_query,
        });
        pos = body_end;
    }
    Ok(out)
}

/// Scan the body of one `<connection>` element for its `<dbPr .../>` tag and
/// return `(command, is_power_query)`. No `<dbPr>` present means a plain
/// connection: no command, not a Power Query.
fn parse_dbpr(body: &[u8], scratch: &mut Vec<u8>) -> (Option<String>, bool) {
    let Some(o) = memchr::memmem::find(body, b"<dbPr ") else {
        return (None, false);
    };
    let Some(gt) = memchr::memchr(b'>', &body[o..]) else {
        return (None, false);
    };
    let tag = &body[o..o + gt];
    let conn = find_attr(tag, b"connection").unwrap_or_default();
    let command = attr_str(tag, b"command", scratch);
    let is_power_query = memchr::memmem::find(conn, b"Microsoft.Mashup").is_some();
    (command, is_power_query)
}

/// Decode-and-own one XML attribute value.
fn attr_str(tag: &[u8], name: &[u8], scratch: &mut Vec<u8>) -> Option<String> {
    find_attr(tag, name).map(|raw| String::from_utf8_lossy(decode_bytes(raw, scratch)).into_owned())
}

/// Inventory the Power Query / data-model parts of a zip-backed workbook.
///
/// One central-directory pass collects the relevant entry names; only
/// `xl/connections.xml` and `customXml/item*.xml` are inflated, and only when
/// present. A workbook with none of these parts costs the single name pass and
/// nothing else.
pub fn inventory_power_query(zip_bytes: &[u8]) -> TurboResult<PowerQueryInventory> {
    let scan = scan_power_query_parts(zip_bytes)?;
    let mut inv = PowerQueryInventory {
        connections: Vec::new(),
        has_data_mashup: false,
        custom_xml_parts: Vec::new(),
        query_table_parts: Vec::new(),
        model_parts: Vec::new(),
    };
    for name in &scan.all {
        let owned = String::from_utf8_lossy(name).into_owned();
        if name.starts_with(b"customXml/") {
            inv.custom_xml_parts.push(owned);
        } else if name.starts_with(b"xl/queryTables/") {
            inv.query_table_parts.push(owned);
        } else if name.starts_with(b"xl/model/") {
            inv.model_parts.push(owned);
        }
    }
    if let Some(conn) = &scan.conn {
        let xml = inflate(conn.method, conn.comp, conn.usize_hint)?;
        inv.connections = parse_connections(&xml)?;
    }
    for item in &scan.mashup {
        let xml = inflate(item.method, item.comp, item.usize_hint)?;
        if memchr::memmem::find(&xml, b"DataMashup").is_some() {
            inv.has_data_mashup = true;
            break;
        }
    }
    Ok(inv)
}

/// Every part the byte-preserving edit path must pass through byte-identical
/// to preserve queries: `customXml/*`, `xl/connections.xml`, `xl/queryTables/*`
/// and `xl/model/*`. Empty when the workbook has none.
pub fn power_query_part_names(zip_bytes: &[u8]) -> TurboResult<Vec<String>> {
    let scan = scan_power_query_parts(zip_bytes)?;
    Ok(scan
        .all
        .iter()
        .map(|n| String::from_utf8_lossy(n).into_owned())
        .collect())
}

// ----------------------------------------------------------------------------
// One-pass central-directory walk.
// ----------------------------------------------------------------------------

/// Payload location of an entry we actually have to inflate.
struct LocatedEntry<'a> {
    method: u16,
    comp: &'a [u8],
    usize_hint: usize,
}

struct PqScan<'a> {
    /// Every relevant entry name, in central-directory order.
    all: Vec<&'a [u8]>,
    /// `xl/connections.xml`, when present.
    conn: Option<LocatedEntry<'a>>,
    /// `customXml/item*.xml` (excluding `itemProps`), when present.
    mashup: Vec<LocatedEntry<'a>>,
}

/// A part we must preserve byte-identical.
fn is_relevant(name: &[u8]) -> bool {
    name.starts_with(b"customXml/")
        || name == b"xl/connections.xml"
        || name.starts_with(b"xl/queryTables/")
        || name.starts_with(b"xl/model/")
}

/// A `customXml/item*.xml` candidate for a `<DataMashup>` payload. `itemProps`
/// metadata files carry no mashup, so they are excluded from inflation.
fn is_mashup_item(name: &[u8]) -> bool {
    name.starts_with(b"customXml/item")
        && !name.starts_with(b"customXml/itemProps")
        && name.ends_with(b".xml")
}

/// Parts we inflate during inventory; everything else is just named.
fn is_inflatable(name: &[u8]) -> bool {
    name == b"xl/connections.xml" || is_mashup_item(name)
}

/// Walk the central directory once, categorizing entries by name without
/// allocating. Local headers are resolved only for the handful of parts that
/// get inflated.
fn scan_power_query_parts<'a>(zip: &'a [u8]) -> TurboResult<PqScan<'a>> {
    let n = zip.len();
    let (cd_count, cd_offset) = resolve_eocd(zip)?;
    let mut p = cd_offset;
    let mut all: Vec<&'a [u8]> = Vec::new();
    let mut conn: Option<LocatedEntry<'a>> = None;
    let mut mashup: Vec<LocatedEntry<'a>> = Vec::new();

    for _ in 0..cd_count {
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
        let name = &zip[p + 46..p + 46 + fname_len];

        if is_relevant(name) {
            let entry = if is_inflatable(name) {
                let (csize, usize_, local_off) = central_sizes(zip, p, fname_len)?;
                Some(locate_payload(
                    zip,
                    local_off,
                    csize,
                    usize_ as usize,
                    method,
                )?)
            } else {
                None
            };
            if name == b"xl/connections.xml" {
                conn = entry;
            } else if is_mashup_item(name) {
                if let Some(e) = entry {
                    mashup.push(e);
                }
            }
            all.push(name);
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

    Ok(PqScan { all, conn, mashup })
}

/// Resolve one entry's local header to its compressed payload.
fn locate_payload<'a>(
    zip: &'a [u8],
    local_off: u64,
    csize: u64,
    usize_hint: usize,
    method: u16,
) -> TurboResult<LocatedEntry<'a>> {
    let n = zip.len();
    let lh = local_off as usize;
    if lh + 30 > n || &zip[lh..lh + 4] != b"\x50\x4b\x03\x04" {
        return Err(TurboError::Format("Invalid local header".into()));
    }
    let l_fname = u16le(zip, lh + 26);
    let l_extra = u16le(zip, lh + 28);
    let data = match lh
        .checked_add(30)
        .and_then(|x| x.checked_add(l_fname))
        .and_then(|x| x.checked_add(l_extra))
    {
        Some(d) => d,
        None => {
            return Err(TurboError::Format(
                "Overflow calculating data offset".into(),
            ));
        }
    };
    let csize = csize as usize;
    if csize > n.saturating_sub(data) {
        return Err(TurboError::Format("Entry payload overruns ZIP file".into()));
    }
    Ok(LocatedEntry {
        method,
        comp: &zip[data..data + csize],
        usize_hint,
    })
}

/// 64-bit sizes and local-header offset for one central-directory entry,
/// promoted from the Zip64 extended-information extra field (ID 0x0001) when
/// the 32-bit fields carry their 0xFFFFFFFF sentinels. Mirrors
/// `zipmin::read_central_sizes`; never panics on truncated or hostile input.
fn central_sizes(zip: &[u8], p: usize, fname_len: usize) -> TurboResult<(u64, u64, u64)> {
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

    Ok((csize, usize_, local_off))
}

/// Resolve the (possibly Zip64) central-directory location. Returns
/// `(cd_count, cd_offset_usize)`; a structural violation is a
/// [`TurboError::Format`], never a panic.
fn resolve_eocd(zip: &[u8]) -> TurboResult<(u64, usize)> {
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

    Ok((cd_count, cd_off_us))
}

#[inline]
fn u16le(b: &[u8], o: usize) -> usize {
    (b[o] as usize) | ((b[o + 1] as usize) << 8)
}

#[inline]
fn u32le(b: &[u8], o: usize) -> usize {
    (b[o] as usize)
        | ((b[o + 1] as usize) << 8)
        | ((b[o + 2] as usize) << 16)
        | ((b[o + 3] as usize) << 24)
}

#[inline]
fn u64le(b: &[u8], o: usize) -> u64 {
    (u32le(b, o) as u64) | ((u32le(b, o + 4) as u64) << 32)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONNECTIONS_XML: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><connections xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><connection id="1" name="Query - foo" type="5"><dbPr connection="Provider=Microsoft.Mashup.OleDb.1;Data Source=$workbook$;Location=foo;Extended Properties=&quot;&quot;" command="SELECT * FROM [foo]"/></connection><connection id="2" name="ODBC DSN" type="2"><dbPr connection="DSN=MyDSN;UID=u;PWD=p" command="SELECT * FROM [table]"/></connection></connections>"#;

    const ODBC_ONLY_XML: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><connections xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><connection id="7" name="ODBC DSN" type="2"><dbPr connection="DSN=MyDSN;UID=u;PWD=p" command="SELECT * FROM [table]"/></connection></connections>"#;

    const MASHUP_ITEM_XML: &[u8] = br#"<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties"><cp:title>Mashup</cp:title><DataMashup>UEsDBBQAAAAAAJ9QX0wAAAAAAAAAAAAAAAAaAAAAaHR0cHM6Ly93d3cuZGF0YXNldHMuZm9vL0Zvcm11bGFzL1NlY3Rpb24xLm0=</DataMashup></cp:coreProperties>"#;

    const ITEM_PROPS_XML: &[u8] =
        br#"<ds:datastoreItem ds:itemID="{00000000-0000-0000-0000-000000000001}" xmlns:ds="http://schemas.openxmlformats.org/officeDocument/2006/customXml"><ds:schemaRefs><ds:schemaRef ds:uri="http://schemas.microsoft.com/office/2006/mashup"/></ds:schemaRefs></ds:datastoreItem>"#;

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

    #[test]
    fn pq_power_query_connection_true_odbc_false() {
        let zip = build_zip(&[("xl/connections.xml", CONNECTIONS_XML)]);
        let inv = inventory_power_query(&zip).unwrap();
        assert_eq!(inv.connections.len(), 2);

        let pq = &inv.connections[0];
        assert_eq!(pq.id, "1");
        assert_eq!(pq.name, "Query - foo");
        assert_eq!(pq.kind.as_deref(), Some("5"));
        assert_eq!(pq.command.as_deref(), Some("SELECT * FROM [foo]"));
        assert!(
            pq.is_power_query,
            "Mashup connection string must flag is_power_query"
        );

        let odbc = &inv.connections[1];
        assert_eq!(odbc.id, "2");
        assert_eq!(odbc.command.as_deref(), Some("SELECT * FROM [table]"));
        assert!(!odbc.is_power_query, "plain ODBC must not be a Power Query");
        assert!(!inv.has_data_mashup);
        assert!(inv.custom_xml_parts.is_empty());
        assert!(inv.query_table_parts.is_empty());
        assert!(inv.model_parts.is_empty());
    }

    #[test]
    fn pq_odbc_only_connection_reports_false() {
        let zip = build_zip(&[("xl/connections.xml", ODBC_ONLY_XML)]);
        let inv = inventory_power_query(&zip).unwrap();
        assert_eq!(inv.connections.len(), 1);
        assert!(!inv.connections[0].is_power_query);
        assert_eq!(inv.connections[0].id, "7");
    }

    #[test]
    fn pq_data_mashup_detected() {
        let zip = build_zip(&[
            ("customXml/item1.xml", MASHUP_ITEM_XML),
            ("customXml/itemProps1.xml", ITEM_PROPS_XML),
        ]);
        let inv = inventory_power_query(&zip).unwrap();
        assert!(
            inv.has_data_mashup,
            "item1.xml carries a DataMashup element"
        );
        assert_eq!(
            inv.custom_xml_parts,
            vec!["customXml/item1.xml", "customXml/itemProps1.xml"]
        );
        assert!(inv.connections.is_empty());
        assert!(inv.query_table_parts.is_empty());
        assert!(inv.model_parts.is_empty());
    }

    #[test]
    fn pq_item_props_alone_is_not_a_mashup() {
        // itemProps metadata must not be mistaken for a mashup payload.
        let zip = build_zip(&[("customXml/itemProps1.xml", ITEM_PROPS_XML)]);
        let inv = inventory_power_query(&zip).unwrap();
        assert!(!inv.has_data_mashup);
        assert_eq!(inv.custom_xml_parts, vec!["customXml/itemProps1.xml"]);
    }

    #[test]
    fn pq_part_names_preserve_list() {
        let zip = build_zip(&[
            ("xl/workbook.xml", b"<workbook/>"),
            ("customXml/item1.xml", MASHUP_ITEM_XML),
            ("customXml/itemProps1.xml", ITEM_PROPS_XML),
            ("xl/connections.xml", CONNECTIONS_XML),
            ("xl/queryTables/queryTable1.xml", b"<queryTable/>"),
            ("xl/model/dataModel.xml", b"<dataModel/>"),
            ("xl/model/relationships/modelRelationships1.xml", b"<rels/>"),
            ("xl/worksheets/sheet1.xml", b"<worksheet/>"),
        ]);
        let names = power_query_part_names(&zip).unwrap();
        assert_eq!(
            names,
            vec![
                "customXml/item1.xml",
                "customXml/itemProps1.xml",
                "xl/connections.xml",
                "xl/queryTables/queryTable1.xml",
                "xl/model/dataModel.xml",
                "xl/model/relationships/modelRelationships1.xml",
            ]
        );
    }

    #[test]
    fn pq_absent_parts_empty_inventory() {
        let zip = build_zip(&[("xl/workbook.xml", b"<workbook/>")]);
        let inv = inventory_power_query(&zip).unwrap();
        assert!(inv.connections.is_empty());
        assert!(!inv.has_data_mashup);
        assert!(inv.custom_xml_parts.is_empty());
        assert!(inv.query_table_parts.is_empty());
        assert!(inv.model_parts.is_empty());
        assert!(power_query_part_names(&zip).unwrap().is_empty());
    }

    #[test]
    fn pq_parse_connections_missing_close_tag() {
        // A connection whose `</connection>` never arrives still parses.
        let part = br#"<connections><connection id="1" name="x"><dbPr connection="Provider=Microsoft.Mashup.OleDb.1;x" command="SELECT * FROM [x]"/>"#;
        let conns = parse_connections(part).unwrap();
        assert_eq!(conns.len(), 1);
        assert!(conns[0].is_power_query);
        assert_eq!(conns[0].command.as_deref(), Some("SELECT * FROM [x]"));
    }

    #[test]
    fn pq_parse_connections_truncated_open_tag() {
        // Unterminated open tag must be tolerated, not panic.
        let part = br#"<connections><connection id="1""#;
        let conns = parse_connections(part).unwrap();
        assert!(conns.is_empty());
    }

    #[test]
    fn pq_parse_connections_plain_text() {
        // No `<connection ` at all yields an empty vec.
        let part =
            br#"<connections xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"/>"#;
        assert!(parse_connections(part).unwrap().is_empty());
    }

    #[test]
    fn pq_corrupt_zip_is_error() {
        assert!(inventory_power_query(b"this is not a zip file at all").is_err());
        assert!(power_query_part_names(b"this is not a zip file at all").is_err());
    }

    #[test]
    fn pq_truncated_zip_is_error() {
        let mut zip = build_zip(&[("xl/connections.xml", CONNECTIONS_XML)]);
        zip.truncate(20);
        assert!(inventory_power_query(&zip).is_err());
    }

    #[test]
    fn pq_corrupt_central_directory_is_error() {
        let mut zip = build_zip(&[("xl/connections.xml", CONNECTIONS_XML)]);
        let cd = zip
            .windows(4)
            .position(|w| w == b"PK\x01\x02")
            .expect("central directory record");
        zip[cd] ^= 0xFF;
        assert!(inventory_power_query(&zip).is_err());
        assert!(power_query_part_names(&zip).is_err());
    }
}
