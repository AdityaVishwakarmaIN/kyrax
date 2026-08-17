//! Form controls, ActiveX controls and embedded OLE objects — inventory and
//! preserve (Tier 3 LOW).
//!
//! openpyxl drops these three families on round-trip; the only sane treatment
//! is to (a) inventory what a sheet actually carries and (b) hand the raw parts
//! through byte-for-byte. This module reads the `<controls>` / `<oleObjects>`
//! blocks from the sheet part tail (already-inflated, serial, one memmem probe
//! for the absent case) plus the `xl/ctrlProps/*` binding part, and lists the
//! pass-through entries (`xl/ctrlProps/*`, `xl/activeX/*`, `xl/embeddings/*`)
//! from the central directory without inflating anything.
//!
//! Both legacy forms are accepted: the bare `<control …>` / `<oleObject …>`
//! element (older files) and the same element wrapped in
//! `<mc:AlternateContent><mc:Choice Requires="x14">…` (modern Excel). The
//! wrapper is never required.

use crate::turbo::decode::decode_bytes;
use crate::turbo::error::TurboResult;
use crate::turbo::structural::find_attr;
use crate::turbo::zipmin::list_entries;

// ----------------------------------------------------------------------------
// Data model
// ----------------------------------------------------------------------------
#[derive(Clone, Debug)]
pub struct FormControl {
    pub shape_id: Option<u32>,
    pub rel_id: String,
    pub name: Option<String>,
    /// `objectType` from the matching `xl/ctrlProps` part (e.g. "CheckBox").
    pub object_type: Option<String>,
    /// `fmlaLink` from the ctrlProps part — the cell/range a form control is
    /// bound to.
    pub fmla_link: Option<String>,
    /// `fmlaRange` from the ctrlProps part (list controls: the item range).
    pub fmla_range: Option<String>,
}

#[derive(Clone, Debug)]
pub struct OleObject {
    pub prog_id: Option<String>,
    pub shape_id: Option<u32>,
    pub rel_id: String,
}

// ----------------------------------------------------------------------------
// Shared low-level helpers
// ----------------------------------------------------------------------------

#[inline]
fn attr_u32(tag: &[u8], name: &[u8]) -> Option<u32> {
    find_attr(tag, name).and_then(|v| std::str::from_utf8(v).ok()?.trim().parse().ok())
}

#[inline]
fn attr_string(tag: &[u8], name: &[u8], scratch: &mut Vec<u8>) -> Option<String> {
    find_attr(tag, name).map(|raw| String::from_utf8_lossy(decode_bytes(raw, scratch)).into_owned())
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

/// First open tag with local name `local`, returned as the tag slice
/// (through its closing `>`). `None` on a truncated part — never an error.
fn find_open_tag_slice<'a>(xml: &'a [u8], local: &[u8]) -> Option<&'a [u8]> {
    let n = xml.len();
    let mut i = 0usize;
    while i < n {
        let o = memchr::memchr(b'<', &xml[i..])?;
        let pos = i + o;
        if is_open_local(xml, pos, local) {
            let te = pos + memchr::memchr(b'>', &xml[pos..n])?;
            return Some(&xml[pos..te]);
        }
        i = pos + 1;
    }
    None
}

// ----------------------------------------------------------------------------
// Sheet-part parsers
// ----------------------------------------------------------------------------

/// Form controls declared in a sheet part: `<controls>` block. Handles both
/// the bare `<control …>` form and the `mc:AlternateContent` wrapper (the
/// wrapper is not required).
///
/// Fast path: a sheet with no `<controls>` container and no `<control `
/// element returns an empty list after a single memmem probe — no scan, no
/// inflate. Truncated or hostile markup yields the entries parsed so far
/// (possibly empty) rather than an error or a panic.
pub fn parse_sheet_controls(sheet_xml: &[u8]) -> TurboResult<Vec<FormControl>> {
    // Absent is the common case: one probe, no scan. The trailing space on the
    // second needle keeps `<controls>` and `<controlPr>` from tripping it.
    if memchr::memmem::find(sheet_xml, b"<controls").is_none()
        && memchr::memmem::find(sheet_xml, b"<control ").is_none()
    {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut scratch = Vec::new();
    let n = sheet_xml.len();
    let mut i = 0usize;
    while i < n {
        let Some(o) = memchr::memmem::find(&sheet_xml[i..n], b"<control ") else {
            break;
        };
        let start = i + o;
        let Some(rel) = memchr::memchr(b'>', &sheet_xml[start..n]) else {
            break; // truncated open tag: keep what we have, never panic
        };
        let te = start + rel;
        let tag = &sheet_xml[start..te];
        out.push(FormControl {
            shape_id: attr_u32(tag, b"shapeId"),
            rel_id: attr_string(tag, b"r:id", &mut scratch).unwrap_or_default(),
            name: attr_string(tag, b"name", &mut scratch),
            // objectType / fmlaLink / fmlaRange live in the xl/ctrlProps part,
            // joined back to this control by the coordinator.
            object_type: None,
            fmla_link: None,
            fmla_range: None,
        });
        i = te + 1;
    }
    Ok(out)
}

/// OLE objects declared in a sheet part: `<oleObjects>` block. Same tolerance
/// and fast-path discipline as [`parse_sheet_controls`]: the needle `oleObject`
/// matches both the `<oleObjects>` container and a bare `<oleObject …>`
/// element, so the absent case is one probe.
pub fn parse_sheet_ole_objects(sheet_xml: &[u8]) -> TurboResult<Vec<OleObject>> {
    if memchr::memmem::find(sheet_xml, b"oleObject").is_none() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut scratch = Vec::new();
    let n = sheet_xml.len();
    let mut i = 0usize;
    while i < n {
        let Some(o) = memchr::memmem::find(&sheet_xml[i..n], b"<oleObject") else {
            break;
        };
        let start = i + o;
        if sheet_xml.get(start + 10) == Some(&b's') {
            i = start + 10; // <oleObjects> container / closing tag — skip
            continue;
        }
        let Some(rel) = memchr::memchr(b'>', &sheet_xml[start..n]) else {
            break; // truncated open tag — keep what we have
        };
        let te = start + rel;
        let tag = &sheet_xml[start..te];
        out.push(OleObject {
            prog_id: attr_string(tag, b"progId", &mut scratch),
            shape_id: attr_u32(tag, b"shapeId"),
            rel_id: attr_string(tag, b"r:id", &mut scratch).unwrap_or_default(),
        });
        i = te + 1;
    }
    Ok(out)
}

// ----------------------------------------------------------------------------
// ctrlProps part
// ----------------------------------------------------------------------------

/// Parse one `xl/ctrlProps/ctrlPropN.xml` part into a [`FormControl`] carrying
/// `objectType` / `fmlaLink` / `fmlaRange`.
///
/// `rel_id` is left empty here: the relationship id lives on the worksheet's
/// `<control r:id>`, and the ctrlProp part is matched back to that control by
/// its own `r:id` attribute — it is not carried by this struct. `shape_id` and
/// `name` are likewise sheet-side and stay `None`.
///
/// A missing or truncated `<formControlPr>` element yields a control with all
/// `None` fields, never an error and never a panic.
pub fn parse_ctrl_props(part: &[u8]) -> TurboResult<FormControl> {
    let mut scratch = Vec::new();
    let mut empty = FormControl {
        shape_id: None,
        rel_id: String::new(),
        name: None,
        object_type: None,
        fmla_link: None,
        fmla_range: None,
    };
    let Some(tag) = find_open_tag_slice(part, b"formControlPr") else {
        return Ok(empty);
    };
    // fmlaLink / fmlaRange can be namespace-prefixed in the wild; try the bare
    // attribute first, then the prefixed spelling.
    let fmla_link = attr_string(tag, b"fmlaLink", &mut scratch)
        .or_else(|| attr_string(tag, b"x14:fmlaLink", &mut scratch));
    let fmla_range = attr_string(tag, b"fmlaRange", &mut scratch)
        .or_else(|| attr_string(tag, b"x14:fmlaRange", &mut scratch));
    empty.object_type = attr_string(tag, b"objectType", &mut scratch);
    empty.fmla_link = fmla_link;
    empty.fmla_range = fmla_range;
    Ok(empty)
}

// ----------------------------------------------------------------------------
// Pass-through inventory
// ----------------------------------------------------------------------------

/// Entry-name prefixes the byte-preserving edit path must carry through
/// untouched: ActiveX XML + its compiled .bin, embedded OLE payloads, and the
/// form-control property parts.
const CONTROL_DIRS: &[&str] = &["xl/ctrlProps/", "xl/activeX/", "xl/embeddings/"];

/// List the zip entries under `xl/ctrlProps/*`, `xl/activeX/*` and
/// `xl/embeddings/*`. One pass over the central directory, no inflation; a
/// workbook with none of these parts returns an empty list.
pub fn control_part_names(zip_bytes: &[u8]) -> TurboResult<Vec<String>> {
    let (entries, _errors) = list_entries(zip_bytes)?;
    let mut out = Vec::new();
    for e in entries {
        if CONTROL_DIRS.iter().any(|d| e.name.starts_with(d)) {
            out.push(e.name);
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const WRAPPED: &[u8] = b"<?xml version=\"1.0\"?><worksheet \
        xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" \
        xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
        <sheetData/>\
        <controls><mc:AlternateContent><mc:Choice Requires=\"x14\">\
        <control shapeId=\"1025\" r:id=\"rId1\" name=\"Check Box 1\">\
        <controlPr defaultSize=\"0\" print=\"0\"><anchor/></controlPr></control>\
        </mc:Choice></mc:AlternateContent></controls>\
        <oleObjects><mc:AlternateContent><mc:Choice>\
        <oleObject progId=\"Word.Document.12\" shapeId=\"1026\" r:id=\"rId2\"/>\
        </mc:Choice></mc:AlternateContent></oleObjects>\
        </worksheet>";

    #[test]
    fn ctrl_wrapped_controls() {
        let c = parse_sheet_controls(WRAPPED).unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].shape_id, Some(1025));
        assert_eq!(c[0].rel_id, "rId1");
        assert_eq!(c[0].name.as_deref(), Some("Check Box 1"));
        assert!(c[0].object_type.is_none());
    }

    #[test]
    fn ctrl_bare_controls() {
        let bare: &[u8] = b"<worksheet><controls>\
            <control shapeId=\"7\" r:id=\"rId2\" name=\"Button 1\"/>\
            </controls></worksheet>";
        let c = parse_sheet_controls(bare).unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].shape_id, Some(7));
        assert_eq!(c[0].rel_id, "rId2");
        assert_eq!(c[0].name.as_deref(), Some("Button 1"));
    }

    #[test]
    fn ctrl_multiple_controls() {
        let xml: &[u8] = b"<worksheet><controls>\
            <control shapeId=\"1\" r:id=\"rId1\" name=\"A\"/>\
            <control shapeId=\"2\" r:id=\"rId2\" name=\"B\"/>\
            </controls></worksheet>";
        let c = parse_sheet_controls(xml).unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].rel_id, "rId1");
        assert_eq!(c[1].rel_id, "rId2");
    }

    #[test]
    fn ctrl_empty_sheet_fast_path() {
        assert!(
            parse_sheet_controls(b"<worksheet><sheetData/></worksheet>")
                .unwrap()
                .is_empty()
        );
        assert!(parse_sheet_controls(b"").unwrap().is_empty());
    }

    #[test]
    fn ctrl_container_without_control() {
        let xml: &[u8] = b"<worksheet><controls>\
            <mc:AlternateContent></mc:AlternateContent>\
            </controls></worksheet>";
        assert!(parse_sheet_controls(xml).unwrap().is_empty());
    }

    #[test]
    fn ctrl_truncated_no_panic() {
        // Open tag cut off before '>' — must return empty, not panic.
        let truncated: &[u8] =
            b"<worksheet><controls><mc:Choice><control shapeId=\"5\" r:id=\"rId1\"";
        assert!(parse_sheet_controls(truncated).unwrap().is_empty());
        // Cut mid-fragment.
        let cut: &[u8] = b"<control shapeId=\"5\" r:id=\"r";
        assert!(parse_sheet_controls(cut).unwrap().is_empty());
    }

    #[test]
    fn ctrl_wrapped_ole_objects() {
        let o = parse_sheet_ole_objects(WRAPPED).unwrap();
        assert_eq!(o.len(), 1);
        assert_eq!(o[0].prog_id.as_deref(), Some("Word.Document.12"));
        assert_eq!(o[0].shape_id, Some(1026));
        assert_eq!(o[0].rel_id, "rId2");
    }

    #[test]
    fn ctrl_bare_ole_objects() {
        let bare: &[u8] = b"<worksheet><oleObjects>\
            <oleObject progId=\"AcroExch.Document\" shapeId=\"9\" r:id=\"rId3\"/>\
            </oleObjects></worksheet>";
        let o = parse_sheet_ole_objects(bare).unwrap();
        assert_eq!(o.len(), 1);
        assert_eq!(o[0].prog_id.as_deref(), Some("AcroExch.Document"));
        assert_eq!(o[0].shape_id, Some(9));
        assert_eq!(o[0].rel_id, "rId3");
    }

    #[test]
    fn ctrl_ole_empty_sheet_fast_path() {
        assert!(
            parse_sheet_ole_objects(b"<worksheet><sheetData/></worksheet>")
                .unwrap()
                .is_empty()
        );
        assert!(parse_sheet_ole_objects(b"<sheetData/>").unwrap().is_empty());
    }

    #[test]
    fn ctrl_ole_truncated_no_panic() {
        let truncated: &[u8] = b"<worksheet><oleObjects><oleObject progId=\"Word";
        assert!(parse_sheet_ole_objects(truncated).unwrap().is_empty());
        let cut: &[u8] = b"<oleObject progId=\"";
        assert!(parse_sheet_ole_objects(cut).unwrap().is_empty());
    }

    #[test]
    fn ctrl_ctrl_props_happy_path() {
        let part: &[u8] = b"<?xml version=\"1.0\"?><ctrlProp xmlns=\"http://schemas.microsoft.com/office/spreadsheetml/2016/ctrlprop\" \
            xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" r:id=\"rId1\">\
            <formControlPr objectType=\"CheckBox\" checked=\"Checked\" fmlaLink=\"$A$1\"/>\
            </ctrlProp>";
        let f = parse_ctrl_props(part).unwrap();
        assert_eq!(f.object_type.as_deref(), Some("CheckBox"));
        assert_eq!(f.fmla_link.as_deref(), Some("$A$1"));
        assert!(f.fmla_range.is_none());
        assert!(f.rel_id.is_empty());
        assert!(f.shape_id.is_none());
    }

    #[test]
    fn ctrl_ctrl_props_with_range_and_prefix() {
        let part: &[u8] = b"<ctrlProp><formControlPr objectType=\"ScrollBar\" \
            fmlaLink=\"$C$1\" fmlaRange=\"$D$1:$D$50\"/></ctrlProp>";
        let f = parse_ctrl_props(part).unwrap();
        assert_eq!(f.object_type.as_deref(), Some("ScrollBar"));
        assert_eq!(f.fmla_link.as_deref(), Some("$C$1"));
        assert_eq!(f.fmla_range.as_deref(), Some("$D$1:$D$50"));

        // Namespace-prefixed element name must still be found.
        let prefixed: &[u8] = b"<ctrlProp><x14:formControlPr objectType=\"CheckBox\"/></ctrlProp>";
        let f = parse_ctrl_props(prefixed).unwrap();
        assert_eq!(f.object_type.as_deref(), Some("CheckBox"));
    }

    #[test]
    fn ctrl_ctrl_props_absent_and_truncated() {
        let absent = parse_ctrl_props(b"<ctrlProp/>").unwrap();
        assert!(absent.object_type.is_none());
        assert!(absent.fmla_link.is_none());
        assert!(absent.fmla_range.is_none());
        // Truncated before '>' — empty control, no panic.
        let cut = parse_ctrl_props(b"<ctrlProp><formControlPr objectType=\"Check").unwrap();
        assert!(cut.object_type.is_none());
        assert!(parse_ctrl_props(b"").unwrap().object_type.is_none());
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
    fn ctrl_part_names_lists_pass_through() {
        let zip = build_zip(&[
            ("xl/workbook.xml", b"<workbook/>"),
            ("xl/worksheets/sheet1.xml", b"<worksheet/>"),
            ("xl/ctrlProps/ctrlProp1.xml", b"<ctrlProp/>"),
            ("xl/activeX/activeX1.xml", b"<activeX/>"),
            ("xl/activeX/activeX1.bin", b"\x00\x01\x02"),
            ("xl/embeddings/oleObject1.bin", b"OLE\x00payload"),
        ]);
        let names = control_part_names(&zip).unwrap();
        assert_eq!(
            names,
            vec![
                "xl/activeX/activeX1.bin",
                "xl/activeX/activeX1.xml",
                "xl/ctrlProps/ctrlProp1.xml",
                "xl/embeddings/oleObject1.bin",
            ]
        );
    }

    #[test]
    fn ctrl_part_names_absent_fast_path() {
        let zip = build_zip(&[("xl/workbook.xml", b"<workbook/>")]);
        assert!(control_part_names(&zip).unwrap().is_empty());
    }

    #[test]
    fn ctrl_part_names_malformed_zip() {
        assert!(control_part_names(b"not a zip").is_err());
        // Truncated EOCD — Err or empty, never a panic.
        let cut: &[u8] = &build_zip(&[("xl/activeX/activeX1.bin", b"x")])[..20];
        let r = control_part_names(cut);
        assert!(r.is_err() || r.unwrap().is_empty());
    }
}
