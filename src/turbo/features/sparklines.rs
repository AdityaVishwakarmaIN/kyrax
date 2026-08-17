//! Sparklines (F---): read/write of the X14 worksheet `extLst` extension.
//!
//! Sparklines are a Tier 3 MEDIUM capability that neither kyrax nor openpyxl
//! held before. They live exclusively in the worksheet part, in the extension
//! list at the tail of `<worksheet>`:
//!
//!   <extLst><ext uri="{05C60535-1F16-4fd2-B633-F4F36F0B64E0}" …>
//!     <x14:sparklineGroups …><x14:sparklineGroup …>…</x14:sparklineGroup>
//!   </x14:sparklineGroups></ext></extLst>
//!
//! This module does three things, all byte-preserving:
//!   * `parse_sparklines` — a single memmem probe on the sheet part. If the
//!     part has no `sparklineGroup`, it costs one scan and nothing else.
//!     Absent is the common case for a workbook, so that scan is the fast path.
//!   * `write_sparkline_ext` — deterministic emitter for the complete `<ext>`
//!     element, ready to be spliced into a worksheet `extLst`.
//!   * `splice_sparklines` — inserts that ext into the sheet part: inside an
//!     existing `<extLst>`, or into a freshly-created one before `</worksheet>`,
//!     replacing any ext that already carries our URI. Every other byte of the
//!     sheet is preserved verbatim (the engine's edit path is byte-preserving).

use crate::turbo::error::{TurboError, TurboResult};
use crate::turbo::structural::find_attr;
use crate::turbo::write::xml::{push_str, write_escaped_attr, write_escaped_text};

/// X14 sparkline extension URI (ECMA-376 x14, `05C60535-1F16-4fd2-B633-F4F36F0B64E0`).
const SPARK_URI: &[u8] = b"{05C60535-1F16-4fd2-B633-F4F36F0B64E0}";

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// Sparkline chart kind. `Stacked` is Excel's win/loss chart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SparkType {
    Line,
    Column,
    Stacked,
}

impl SparkType {
    pub fn as_str(&self) -> &'static str {
        match self {
            SparkType::Line => "line",
            SparkType::Column => "column",
            SparkType::Stacked => "stacked",
        }
    }
}

/// One sparkline: `f` holds the source range (e.g. `Sheet1!B1:F1`), `sqref`
/// holds the anchor cell (e.g. `A1`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sparkline {
    pub source: String,
    pub location: String,
}

/// A group of sparklines sharing one type / colouring.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SparklineGroup {
    pub kind: SparkType,
    pub sparklines: Vec<Sparkline>,
    pub color_series: Option<String>,
    pub color_negative: Option<String>,
    pub markers: bool,
    pub high: bool,
    pub low: bool,
    pub display_empty_as: String,
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

/// Value of attribute `name` on `tag`, entity-decoded.
fn attr_str<'a>(tag: &'a [u8], name: &[u8], scratch: &'a mut Vec<u8>) -> Option<String> {
    find_attr(tag, name).map(|raw| {
        String::from_utf8_lossy(crate::turbo::decode::decode_bytes(raw, scratch)).into_owned()
    })
}

/// Inner text of the first element with local name `local`, entity-decoded.
fn elem_text(xml: &[u8], local: &[u8], scratch: &mut Vec<u8>) -> Option<String> {
    let start = find_open_local(xml, 0, local)?;
    let te = start + memchr::memchr(b'>', &xml[start..])?;
    if xml.get(te.wrapping_sub(1)) == Some(&b'/') {
        return Some(String::new());
    }
    let close = find_close_local(xml, te.saturating_add(1), local)?;
    if te + 1 > close || close > xml.len() {
        return Some(String::new());
    }
    Some(
        String::from_utf8_lossy(crate::turbo::decode::decode_bytes(
            &xml[te + 1..close],
            scratch,
        ))
        .into_owned(),
    )
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

// ---------------------------------------------------------------------------
// Parse
// ---------------------------------------------------------------------------

/// Parse the sparkline groups out of an already-inflated worksheet part.
///
/// Fast path first: if the part does not contain the substring `sparklineGroup`,
/// this returns `Ok(vec![])` after a single memmem scan and touches nothing
/// else — absent is the common case and must cost one scan. Tolerant of
/// truncated or malformed markup: unknown attributes default, missing children
/// default, and the function never panics.
pub fn parse_sparklines(sheet_xml: &[u8]) -> TurboResult<Vec<SparklineGroup>> {
    if memchr::memmem::find(sheet_xml, b"sparklineGroup").is_none() {
        return Ok(Vec::new());
    }

    let n = sheet_xml.len();
    let mut out = Vec::new();
    let mut scratch = Vec::new();
    let mut i = 0usize;

    while let Some(start) = find_open_local(sheet_xml, i, b"sparklineGroup") {
        let te = start + memchr::memchr(b'>', &sheet_xml[start..n]).unwrap_or(n - start);
        let tag = &sheet_xml[start..te];
        let kind = match find_attr(tag, b"type") {
            Some(b"column") => SparkType::Column,
            Some(b"stacked") => SparkType::Stacked,
            _ => SparkType::Line,
        };
        let display_empty_as = attr_str(tag, b"displayEmptyCellsAs", &mut scratch)
            .unwrap_or_else(|| String::from("gap"));
        let markers = match find_attr(tag, b"markers") {
            Some(v) => v == b"1" || v == b"true",
            None => false,
        };
        let high = match find_attr(tag, b"high") {
            Some(v) => v == b"1" || v == b"true",
            None => false,
        };
        let low = match find_attr(tag, b"low") {
            Some(v) => v == b"1" || v == b"true",
            None => false,
        };

        let group_end = elem_end(sheet_xml, start, b"sparklineGroup");
        let body: &[u8] = if te + 1 < group_end {
            &sheet_xml[te + 1..group_end]
        } else {
            &[]
        };

        let mut color_series = None;
        if let Some(cs) = find_open_local(body, 0, b"colorSeries") {
            let cte = cs + memchr::memchr(b'>', &body[cs..]).unwrap_or(0);
            color_series = attr_str(&body[cs..cte], b"rgb", &mut scratch);
        }
        let mut color_negative = None;
        if let Some(cn) = find_open_local(body, 0, b"colorNegative") {
            let cte = cn + memchr::memchr(b'>', &body[cn..]).unwrap_or(0);
            color_negative = attr_str(&body[cn..cte], b"rgb", &mut scratch);
        }

        let mut sparklines = Vec::new();
        if let Some(ss) = find_open_local(body, 0, b"sparklines") {
            let ste = ss + memchr::memchr(b'>', &body[ss..]).unwrap_or(0);
            let mut j = ste.saturating_add(1);
            while let Some(sp) = find_open_local(body, j, b"sparkline") {
                let spe = sp + memchr::memchr(b'>', &body[sp..]).unwrap_or(0);
                let send = find_close_local(body, spe.saturating_add(1), b"sparkline")
                    .unwrap_or(body.len());
                let sbody = if spe + 1 < send {
                    &body[spe + 1..send]
                } else {
                    &[]
                };
                sparklines.push(Sparkline {
                    source: elem_text(sbody, b"f", &mut scratch).unwrap_or_default(),
                    location: elem_text(sbody, b"sqref", &mut scratch).unwrap_or_default(),
                });
                j = send.saturating_add(1);
                if j >= body.len() {
                    break;
                }
            }
        }

        out.push(SparklineGroup {
            kind,
            sparklines,
            color_series,
            color_negative,
            markers,
            high,
            low,
            display_empty_as,
        });

        i = group_end.max(start.saturating_add(1));
        if i > n {
            break;
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Write
// ---------------------------------------------------------------------------

fn write_group(g: &SparklineGroup, out: &mut Vec<u8>) {
    out.extend_from_slice(b"<x14:sparklineGroup type=\"");
    push_str(out, g.kind.as_str());
    out.extend_from_slice(b"\" displayEmptyCellsAs=\"");
    write_escaped_attr(out, &g.display_empty_as);
    out.push(b'"');
    if g.markers {
        out.extend_from_slice(b" markers=\"1\"");
    }
    if g.high {
        out.extend_from_slice(b" high=\"1\"");
    }
    if g.low {
        out.extend_from_slice(b" low=\"1\"");
    }
    out.push(b'>');
    if let Some(c) = &g.color_series {
        out.extend_from_slice(b"<x14:colorSeries rgb=\"");
        write_escaped_attr(out, c);
        out.extend_from_slice(b"\"/>");
    }
    if let Some(c) = &g.color_negative {
        out.extend_from_slice(b"<x14:colorNegative rgb=\"");
        write_escaped_attr(out, c);
        out.extend_from_slice(b"\"/>");
    }
    out.extend_from_slice(b"<x14:sparklines>");
    for s in &g.sparklines {
        out.extend_from_slice(b"<x14:sparkline><xm:f>");
        write_escaped_text(out, &s.source);
        out.extend_from_slice(b"</xm:f><xm:sqref>");
        write_escaped_text(out, &s.location);
        out.extend_from_slice(b"</xm:sqref></x14:sparkline>");
    }
    out.extend_from_slice(b"</x14:sparklines></x14:sparklineGroup>");
}

/// Emit the complete `<ext>` element (ready to splice into a worksheet
/// `extLst`). Deterministic: same groups in, same bytes out, attributes
/// escaped.
pub fn write_sparkline_ext(groups: &[SparklineGroup]) -> Vec<u8> {
    let mut out = Vec::with_capacity(256 + groups.len() * 128);
    out.extend_from_slice(
        b"<ext uri=\"{05C60535-1F16-4fd2-B633-F4F36F0B64E0}\" \
          xmlns:x14=\"http://schemas.microsoft.com/office/spreadsheetml/2009/9/main\">\
          <x14:sparklineGroups xmlns:xm=\"http://schemas.microsoft.com/office/excel/2006/main\">",
    );
    for g in groups {
        write_group(g, &mut out);
    }
    out.extend_from_slice(b"</x14:sparklineGroups></ext>");
    out
}

// ---------------------------------------------------------------------------
// Splice into the worksheet part
// ---------------------------------------------------------------------------

/// Return the sheet part with the sparkline ext inserted into its extension
/// list.
///
/// * If `<extLst>` exists, the ext is inserted inside it (before `</extLst>`).
/// * If not, `<extLst>` is created immediately before `</worksheet>`.
/// * If an ext with our URI already exists, it is replaced — never duplicated.
///
/// Every other byte of the sheet is preserved exactly.
pub fn splice_sparklines(sheet_xml: &[u8], groups: &[SparklineGroup]) -> TurboResult<Vec<u8>> {
    let ext = write_sparkline_ext(groups);
    let n = sheet_xml.len();

    // Locate every existing ext element that carries our URI.
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    while let Some(start) = find_open_local(sheet_xml, i, b"ext") {
        let te = start + memchr::memchr(b'>', &sheet_xml[start..n]).unwrap_or(n - start);
        let has_uri = find_attr(&sheet_xml[start..te], b"uri")
            .map(|v| v == SPARK_URI)
            .unwrap_or(false);
        if has_uri {
            spans.push((start, elem_end(sheet_xml, start, b"ext")));
        }
        i = te.saturating_add(1);
        if i > n {
            break;
        }
    }

    let mut out = Vec::with_capacity(n + ext.len() + 16);

    // Replace existing spark ext(s) in place — first position, single result.
    if !spans.is_empty() {
        out.extend_from_slice(&sheet_xml[..spans[0].0]);
        out.extend_from_slice(&ext);
        let mut prev_end = spans[0].1;
        for &(s, e) in spans.iter().skip(1) {
            out.extend_from_slice(&sheet_xml[prev_end..s]);
            prev_end = e;
        }
        out.extend_from_slice(&sheet_xml[prev_end..]);
        return Ok(out);
    }

    // No spark ext yet: insert inside the existing extLst.
    if let Some(ls) = find_open_local(sheet_xml, 0, b"extLst") {
        let lte = ls + memchr::memchr(b'>', &sheet_xml[ls..n]).unwrap_or(n - ls);
        if lte > 0 && sheet_xml.get(lte - 1) == Some(&b'/') {
            // <extLst/> → <extLst>…ext…</extLst>
            let self_close_end = lte.saturating_add(1).min(n);
            out.extend_from_slice(&sheet_xml[..ls]);
            out.extend_from_slice(b"<extLst>");
            out.extend_from_slice(&ext);
            out.extend_from_slice(b"</extLst>");
            out.extend_from_slice(&sheet_xml[self_close_end..]);
            return Ok(out);
        }
        let close = find_close_local(sheet_xml, lte.saturating_add(1), b"extLst").unwrap_or(n);
        out.extend_from_slice(&sheet_xml[..close]);
        out.extend_from_slice(&ext);
        out.extend_from_slice(&sheet_xml[close..]);
        return Ok(out);
    }

    // No extLst at all: create one immediately before </worksheet>.
    let ws_close = memchr::memmem::find(sheet_xml, b"</worksheet>")
        .ok_or_else(|| TurboError::Format("worksheet part has no </worksheet>".into()))?;
    out.extend_from_slice(&sheet_xml[..ws_close]);
    out.extend_from_slice(b"<extLst>");
    out.extend_from_slice(&ext);
    out.extend_from_slice(b"</extLst>");
    out.extend_from_slice(&sheet_xml[ws_close..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const EXT: &[u8] = b"<ext uri=\"{05C60535-1F16-4fd2-B633-F4F36F0B64E0}\" xmlns:x14=\"http://schemas.microsoft.com/office/spreadsheetml/2009/9/main\"><x14:sparklineGroups xmlns:xm=\"http://schemas.microsoft.com/office/excel/2006/main\"><x14:sparklineGroup type=\"line\" displayEmptyCellsAs=\"gap\" markers=\"1\" high=\"1\" low=\"1\"><x14:colorSeries rgb=\"FF376092\"/><x14:colorNegative rgb=\"FFD00000\"/><x14:sparklines><x14:sparkline><xm:f>Sheet1!B1:F1</xm:f><xm:sqref>A1</xm:sqref></x14:sparkline></x14:sparklines></x14:sparklineGroup></x14:sparklineGroups></ext>";

    fn sample_group() -> SparklineGroup {
        SparklineGroup {
            kind: SparkType::Line,
            sparklines: vec![Sparkline {
                source: "Sheet1!B1:F1".into(),
                location: "A1".into(),
            }],
            color_series: Some("FF376092".into()),
            color_negative: Some("FFD00000".into()),
            markers: true,
            high: true,
            low: true,
            display_empty_as: "gap".into(),
        }
    }

    fn sheet_with_ext(ext: &[u8]) -> Vec<u8> {
        let mut s = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><worksheet xmlns=\"x\"><sheetData><row r=\"1\"><c r=\"A1\"><v>1</v></c></row></sheetData><extLst>".to_vec();
        s.extend_from_slice(ext);
        s.extend_from_slice(b"</extLst></worksheet>");
        s
    }

    #[test]
    fn spark_absent_part_is_one_scan() {
        let sheet = b"<worksheet xmlns=\"x\"><sheetData><c r=\"A1\"/></sheetData></worksheet>";
        let got = parse_sparklines(sheet).unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn spark_parse_happy_path() {
        let sheet = sheet_with_ext(EXT);
        let got = parse_sparklines(&sheet).unwrap();
        assert_eq!(got, vec![sample_group()]);
    }

    #[test]
    fn spark_parse_column_and_winloss() {
        let xml = b"<worksheet><extLst><ext uri=\"{05C60535-1F16-4fd2-B633-F4F36F0B64E0}\"><x14:sparklineGroups xmlns:xm=\"m\"><x14:sparklineGroup type=\"column\"><x14:sparklines><x14:sparkline><xm:f>S1!A1:A3</xm:f><xm:sqref>B1</xm:sqref></x14:sparkline></x14:sparklines></x14:sparklineGroup><x14:sparklineGroup type=\"stacked\" displayEmptyCellsAs=\"zero\"><x14:sparklines><x14:sparkline><xm:f>S2!C1</xm:f><xm:sqref>C1</xm:sqref></x14:sparkline></x14:sparklines></x14:sparklineGroup></x14:sparklineGroups></ext></extLst></worksheet>";
        let got = parse_sparklines(xml).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].kind, SparkType::Column);
        assert_eq!(got[0].sparklines[0].source, "S1!A1:A3");
        assert_eq!(got[1].kind, SparkType::Stacked);
        assert_eq!(got[1].display_empty_as, "zero");
        assert!(!got[1].markers);
    }

    #[test]
    fn spark_write_is_deterministic() {
        let g = sample_group();
        let a = write_sparkline_ext(std::slice::from_ref(&g));
        let b = write_sparkline_ext(&[g]);
        assert_eq!(a, EXT);
        assert_eq!(b, EXT);
    }

    #[test]
    fn spark_write_escapes_attributes() {
        let g = SparklineGroup {
            kind: SparkType::Column,
            sparklines: vec![Sparkline {
                source: "O'Reilly & Sons!A1:B2".into(),
                location: "A1".into(),
            }],
            color_series: None,
            color_negative: None,
            markers: false,
            high: false,
            low: false,
            display_empty_as: "gap\"nul".into(),
        };
        let ext = write_sparkline_ext(&[g]);
        let s = String::from_utf8(ext).unwrap();
        assert!(s.contains("&quot;"));
        assert!(s.contains("&amp;"));
        assert!(s.contains("type=\"column\""));
        // re-parse round trip
        let sheet = sheet_with_ext(s.as_bytes());
        let got = parse_sparklines(&sheet).unwrap();
        assert_eq!(got[0].sparklines[0].source, "O'Reilly & Sons!A1:B2");
        assert_eq!(got[0].display_empty_as, "gap\"nul");
    }

    #[test]
    fn spark_splice_no_extlst_creates_one() {
        let sheet = b"<worksheet xmlns=\"x\"><sheetData><c r=\"A1\"/></sheetData></worksheet>";
        let got = splice_sparklines(sheet, &[sample_group()]).unwrap();
        let s = String::from_utf8(got.clone()).unwrap();
        assert!(s.contains("<extLst>"));
        assert!(s.ends_with("</extLst></worksheet>"));
        // prefix before the splice point is untouched
        assert!(got.starts_with(b"<worksheet xmlns=\"x\"><sheetData><c r=\"A1\"/></sheetData>"));
        // exactly one ext
        assert_eq!(memchr::memmem::find_iter(&got, b"<ext uri=\"").count(), 1);
        // round trip
        let parsed = parse_sparklines(&got).unwrap();
        assert_eq!(parsed, vec![sample_group()]);
    }

    #[test]
    fn spark_splice_unrelated_ext_preserved() {
        let unrelated = b"<ext uri=\"{SOMETHING-ELSE}\" xmlns:s=\"u\"><s:data/></ext>";
        let sheet = sheet_with_ext(unrelated);
        let got = splice_sparklines(&sheet, &[sample_group()]).unwrap();
        assert!(
            memchr::memmem::find(&got, unrelated).is_some(),
            "unrelated ext untouched"
        );
        assert_eq!(memchr::memmem::find_iter(&got, b"<ext uri=\"").count(), 2);
        assert_eq!(
            memchr::memmem::find_iter(&got, b"{05C60535-1F16-4fd2-B633-F4F36F0B64E0}").count(),
            1
        );
    }

    #[test]
    fn spark_splice_replaces_existing_not_duplicates() {
        let sheet = sheet_with_ext(EXT);
        let got = splice_sparklines(&sheet, &[sample_group()]).unwrap();
        assert_eq!(
            memchr::memmem::find_iter(&got, b"{05C60535-1F16-4fd2-B633-F4F36F0B64E0}").count(),
            1,
            "never duplicate"
        );
        assert_eq!(memchr::memmem::find_iter(&got, b"<ext uri=\"").count(), 1);
        assert!(got.starts_with(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert!(got.ends_with(b"</extLst></worksheet>"));
        let parsed = parse_sparklines(&got).unwrap();
        assert_eq!(parsed, vec![sample_group()]);
    }

    #[test]
    fn spark_splice_self_closing_extlst() {
        let sheet = b"<worksheet xmlns=\"x\"><sheetData/><extLst/></worksheet>";
        let got = splice_sparklines(sheet, &[sample_group()]).unwrap();
        assert_eq!(memchr::memmem::find_iter(&got, b"<ext uri=\"").count(), 1);
        assert!(got.ends_with(b"</extLst></worksheet>"));
        assert!(memchr::memmem::find(&got, b"<extLst>").is_some());
    }

    #[test]
    fn spark_malformed_truncated_group_no_panic() {
        let sheet = b"<worksheet><extLst><x14:sparklineGroup type=\"line\" markers=\"1\"";
        let got = parse_sparklines(sheet).unwrap();
        // tolerant: may yield a partial group, never a panic
        assert!(got.len() <= 1);
    }

    #[test]
    fn spark_malformed_garbage_no_panic() {
        let sheet = b"<sparklineGroup"; // no '>', no close
        let got = parse_sparklines(sheet).unwrap();
        assert!(got.len() <= 1);
        let got2 = parse_sparklines(b"garbage that never closes <x14:sparkline").unwrap();
        assert!(got2.is_empty());
    }

    #[test]
    fn spark_malformed_splice_missing_worksheet_is_err() {
        let sheet = b"<worksheet><sheetData><c r=\"A1\"/></sheetData>"; // no </worksheet>
        assert!(splice_sparklines(sheet, &[sample_group()]).is_err());
    }
}
