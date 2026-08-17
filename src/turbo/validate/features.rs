//! Feature-presence detection surfaced inside the validate report (Tier 3).
//!
//! `validate_workbook` already walks the central directory, and every Phase 3
//! feature module in `src/turbo/features/` can answer "does this workbook have
//! X?" for free there: detection of an ABSENT feature is one entry-name pass —
//! measured at ~10 ns and flat in file size (PERF_EXPERIMENTS_PHASE3.md, E7) —
//! while inflating every part costs 23.8 ms on a 2.8 MB workbook and 152.9 ms
//! on a 36 MB one. This module turns that walk into a `FeatureFlags` struct and
//! a set of [`Finding`]s, so a pipeline learns what a workbook CONTAINS at the
//! same moment it learns whether the workbook is sound. No feature XML is ever
//! parsed here; presence is decided by entry names and, in the deep variant
//! only, a single memmem probe per inflated worksheet.
//!
//! Two functions rather than a boolean flag, so the cheap one cannot be called
//! expensively by accident:
//! * [`detect_features`] — one central-directory name pass, zero inflates. The
//!   E7 fast path.
//! * [`detect_features_deep`] — the same pass, then it inflates every worksheet
//!   part to probe for the three sheet-resident areas (sparklines, form/ActiveX
//!   controls, embedded OLE objects). Cost is linear in the sheet bytes; prefer
//!   the shallow call unless those flags are needed.

use crate::turbo::error::TurboResult;
use crate::turbo::zipmin::{ZipEntryMeta, inflate_entry, list_entries};

use super::{Finding, FindingCode, Severity};

/// Which feature areas a workbook contains, all decided in ONE pass over the
/// central directory. `has_sparklines`, `has_controls` and `has_ole_objects`
/// are only trustworthy after [`detect_features_deep`]: sparklines and OLE
/// objects live inside worksheet parts, and controls also get a sheet probe in
/// the deep variant (the `xl/ctrlProps|activeX|embeddings/` directories decide
/// `has_controls` in the shallow pass).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FeatureFlags {
    /// `xl/slicers/` or `xl/slicerCaches/` entries.
    pub has_slicers: bool,
    /// `xl/timelines/` or `xl/timelineCaches/` entries.
    pub has_timelines: bool,
    /// Worksheet `extLst` carries a `sparklineGroups` extension (deep only).
    pub has_sparklines: bool,
    /// `xl/richData/` entries (stocks, geography, image-in-cell).
    pub has_rich_data: bool,
    /// `xl/connections.xml`, `xl/queryTables/` or `xl/model/` entries.
    pub has_power_query: bool,
    /// `xl/threadedComments/` entries (their `xl/persons/` backer is counted,
    /// not flagged — a persons list without comments is not a threaded file).
    pub has_threaded_comments: bool,
    /// `xl/ctrlProps/`, `xl/activeX/` or `xl/embeddings/` entries; the deep
    /// variant also probes worksheets for a `<control>` element.
    pub has_controls: bool,
    /// A worksheet declares an `<oleObject>` (deep only).
    pub has_ole_objects: bool,
    /// `xl/externalLinks/` entries — cross-workbook formula caches.
    pub has_external_links: bool,
    /// `_xmlsignatures/` entries.
    pub is_signed: bool,
    /// Total number of entries belonging to any of the above areas.
    pub part_count: usize,
}

/// Detect features with the E7 fast path: one walk over the central directory,
/// no inflate at all. ~10 ns and flat in file size, versus ~23.8 ms to inflate
/// every part of a 2.8 MB workbook (152.9 ms for a 36 MB one). The three
/// sheet-resident areas (sparklines, OLE objects, and the sheet-side half of
/// controls) are left false — only [`detect_features_deep`] sets those, because
/// deciding them costs an inflate. A malformed zip is `Err`, never a panic; a
/// zip whose central directory parses returns `Ok` regardless of the errors the
/// walker salvaged around corrupt records.
pub fn detect_features(zip_bytes: &[u8]) -> TurboResult<FeatureFlags> {
    let (entries, _errors) = list_entries(zip_bytes)?;
    Ok(scan_entry_names(&entries))
}

/// The variant that also inflates worksheet parts, to probe for the
/// sheet-resident areas: sparklines, form/ActiveX controls and embedded OLE
/// objects. Same central-directory pass as [`detect_features`], then each
/// worksheet part is inflated once and scanned with a single memmem probe per
/// marker; a sheet part that will not inflate is skipped, never fatal. Cost is
/// linear in the sheet bytes, so use the shallow call unless these three flags
/// matter — this is E7 candidate B, which the experiment measured slower than
/// inflate-everything on a 36 MB file, precisely because the sheet is the
/// biggest part in the package.
pub fn detect_features_deep(zip_bytes: &[u8]) -> TurboResult<FeatureFlags> {
    let (entries, _errors) = list_entries(zip_bytes)?;
    let mut flags = scan_entry_names(&entries);
    for e in &entries {
        if !is_worksheet_part(&e.name) {
            continue;
        }
        // A corrupt sheet cannot be probed; keep scanning the others. This is
        // detection, not validation — a broken part is that sheet's problem.
        let Ok(xml) = inflate_entry(zip_bytes, e) else {
            continue;
        };
        if memchr::memmem::find(&xml, b"sparklineGroup").is_some() {
            flags.has_sparklines = true;
        }
        // Same needles as features/controls.rs's fast path: the trailing space
        // keeps `<controlPr>` and the `<controls>` container apart.
        if memchr::memmem::find(&xml, b"<controls").is_some()
            || memchr::memmem::find(&xml, b"<control ").is_some()
        {
            flags.has_controls = true;
        }
        if memchr::memmem::find(&xml, b"oleObject").is_some() {
            flags.has_ole_objects = true;
        }
    }
    Ok(flags)
}

/// Turn a [`FeatureFlags`] into the validate report's findings: one
/// `Severity::Info` finding per detected area, so a pipeline learns what the
/// workbook contains. The signature is the one non-informational finding and is
/// emitted at `Severity::Warning`: a signature over content we modified is
/// worse than none, because Excel reports the edited file as tampered (mirrors
/// `src/turbo/features/signatures.rs`).
///
/// Every finding is `repairable: false` — presence is a fact, not a defect —
/// and every one carries [`FindingCode::FeaturePresent`], so a caller can filter
/// the whole class out with one comparison when it only wants defects.
pub fn feature_findings(flags: &FeatureFlags) -> Vec<Finding> {
    let mut out = Vec::new();
    if flags.has_slicers {
        out.push(feature_finding(
            Severity::Info,
            "xl/slicers/",
            "workbook contains slicers (xl/slicers/ or xl/slicerCaches/)",
        ));
    }
    if flags.has_timelines {
        out.push(feature_finding(
            Severity::Info,
            "xl/timelines/",
            "workbook contains timelines (xl/timelines/ or xl/timelineCaches/)",
        ));
    }
    if flags.has_sparklines {
        out.push(feature_finding(
            Severity::Info,
            "xl/worksheets/",
            "a worksheet contains sparklines (extLst sparklineGroups extension)",
        ));
    }
    if flags.has_rich_data {
        out.push(feature_finding(
            Severity::Info,
            "xl/richData/",
            "workbook contains rich data values (stocks, geography, image-in-cell)",
        ));
    }
    if flags.has_power_query {
        out.push(feature_finding(
            Severity::Info,
            "xl/connections.xml",
            "workbook contains Power Query or data-model parts (xl/connections.xml, xl/queryTables/, xl/model/)",
        ));
    }
    if flags.has_threaded_comments {
        out.push(feature_finding(
            Severity::Info,
            "xl/threadedComments/",
            "workbook contains threaded comments (xl/threadedComments/)",
        ));
    }
    if flags.has_controls {
        out.push(feature_finding(
            Severity::Info,
            "xl/ctrlProps/",
            "workbook contains form/ActiveX controls or embedded-OLE payloads (xl/ctrlProps/, xl/activeX/, xl/embeddings/)",
        ));
    }
    if flags.has_ole_objects {
        out.push(feature_finding(
            Severity::Info,
            "xl/worksheets/",
            "a worksheet contains an embedded OLE object",
        ));
    }
    if flags.has_external_links {
        out.push(feature_finding(
            Severity::Info,
            "xl/externalLinks/",
            "workbook has external links (cross-workbook formula caches)",
        ));
    }
    if flags.is_signed {
        out.push(feature_finding(
            Severity::Warning,
            "_xmlsignatures/",
            "the workbook is digitally signed; the signature will be dropped if the workbook is modified, because a signature over modified content makes Excel report the file as tampered",
        ));
    }
    out
}

/// One presence finding. Always [`FindingCode::FeaturePresent`].
fn feature_finding(severity: Severity, part: &str, message: &str) -> Finding {
    Finding {
        code: FindingCode::FeaturePresent,
        severity,
        part: part.to_string(),
        location: None,
        message: message.to_string(),
        repairable: false,
    }
}

/// Apply every area's name-prefix test in a single walk over the entry list.
fn scan_entry_names(entries: &[ZipEntryMeta]) -> FeatureFlags {
    let mut flags = FeatureFlags::default();
    for e in entries {
        let n = e.name.as_str();
        if n.starts_with("xl/slicers/") || n.starts_with("xl/slicerCaches/") {
            flags.has_slicers = true;
            flags.part_count += 1;
        } else if n.starts_with("xl/timelines/") || n.starts_with("xl/timelineCaches/") {
            flags.has_timelines = true;
            flags.part_count += 1;
        } else if n.starts_with("xl/richData/") {
            flags.has_rich_data = true;
            flags.part_count += 1;
        } else if n == "xl/connections.xml"
            || n.starts_with("xl/queryTables/")
            || n.starts_with("xl/model/")
        {
            flags.has_power_query = true;
            flags.part_count += 1;
        } else if n.starts_with("xl/threadedComments/") {
            flags.has_threaded_comments = true;
            flags.part_count += 1;
        } else if n.starts_with("xl/persons/") {
            // The persons list backs threaded comments; it is part of that
            // area's byte-preservation list even though it is not the flag.
            flags.part_count += 1;
        } else if n.starts_with("xl/ctrlProps/")
            || n.starts_with("xl/activeX/")
            || n.starts_with("xl/embeddings/")
        {
            // CONTROL_DIRS in features/controls.rs groups these three together.
            flags.has_controls = true;
            flags.part_count += 1;
        } else if n.starts_with("xl/externalLinks/") {
            flags.has_external_links = true;
            flags.part_count += 1;
        } else if n.starts_with("_xmlsignatures/") {
            flags.is_signed = true;
            flags.part_count += 1;
        }
    }
    flags
}

fn is_worksheet_part(name: &str) -> bool {
    name.starts_with("xl/worksheets/") && name.ends_with(".xml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn w_u16(out: &mut Vec<u8>, v: u16) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    fn w_u32(out: &mut Vec<u8>, v: u32) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    /// Minimal STORE zip with one compressible payload per entry. `method` lets
    /// a test plant a deflate(8) entry with garbage bytes to prove that a
    /// function inflates nothing it does not need to — the reader never verifies
    /// CRC, so that field stays zero.
    ///
    /// The central-directory field order matters: `external attrs` is four bytes
    /// and must be written before the local-header offset. A stray u16 shifts
    /// that offset by two bytes and every later record with it, so the directory
    /// parses but every payload points at garbage.
    fn build_zip(entries: &[(&str, u16, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cds: Vec<(u32, &str, u16, usize)> = Vec::new();
        for (name, method, payload) in entries {
            let lh = out.len() as u32;
            out.extend_from_slice(b"PK\x03\x04");
            w_u16(&mut out, 20); // version needed
            w_u16(&mut out, 0); // general purpose flags
            w_u16(&mut out, *method); // compression method
            w_u16(&mut out, 0); // mod time
            w_u16(&mut out, 0); // mod date
            w_u32(&mut out, 0); // crc-32 (not verified)
            w_u32(&mut out, payload.len() as u32); // compressed size
            w_u32(&mut out, payload.len() as u32); // uncompressed size
            w_u16(&mut out, name.len() as u16); // name length
            w_u16(&mut out, 0); // extra length
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(payload);
            cds.push((lh, name, *method, payload.len()));
        }
        let cd_start = out.len() as u32;
        for (lh, name, method, plen) in cds {
            out.extend_from_slice(b"PK\x01\x02");
            w_u16(&mut out, 20); // version made by
            w_u16(&mut out, 20); // version needed
            w_u16(&mut out, 0); // flags
            w_u16(&mut out, method); // method
            w_u16(&mut out, 0); // mod time
            w_u16(&mut out, 0); // mod date
            w_u32(&mut out, 0); // crc
            w_u32(&mut out, plen as u32); // csize
            w_u32(&mut out, plen as u32); // usize
            w_u16(&mut out, name.len() as u16); // name length
            w_u16(&mut out, 0); // extra len
            w_u16(&mut out, 0); // comment len
            w_u16(&mut out, 0); // disk number start
            w_u16(&mut out, 0); // internal attrs
            w_u32(&mut out, 0); // external attrs
            w_u32(&mut out, lh); // local header offset
            out.extend_from_slice(name.as_bytes());
        }
        let cd_size = out.len() as u32 - cd_start;
        out.extend_from_slice(b"PK\x05\x06");
        w_u16(&mut out, 0); // disk
        w_u16(&mut out, 0); // cd start disk
        w_u16(&mut out, entries.len() as u16);
        w_u16(&mut out, entries.len() as u16);
        w_u32(&mut out, cd_size);
        w_u32(&mut out, cd_start);
        w_u16(&mut out, 0); // comment len
        out
    }

    #[test]
    fn vfeat_empty_workbook_detects_nothing() {
        let zip = build_zip(&[("[Content_Types].xml", 0, b"<Types/>")]);
        let flags = detect_features(&zip).unwrap();
        assert_eq!(flags, FeatureFlags::default());
        assert_eq!(flags.part_count, 0);
        assert!(feature_findings(&flags).is_empty());
    }

    /// The test that matters most in this file: the shallow contract is that
    /// detection of an ABSENT feature costs one entry-name pass and nothing
    /// else. Every entry here claims deflate(8) but its payload is garbage, so
    /// any inflate would surface an error. `detect_features` must instead return
    /// `Ok` with every flag false.
    #[test]
    fn vfeat_shallow_never_inflates() {
        let zip = build_zip(&[
            ("[Content_Types].xml", 8, b"\x00"),
            ("xl/workbook.xml", 8, b"\xff\xff"),
            ("xl/worksheets/sheet1.xml", 8, b"\xde\xad"),
        ]);
        let flags = detect_features(&zip).unwrap();
        assert_eq!(flags, FeatureFlags::default());
        assert_eq!(flags.part_count, 0);
    }

    #[test]
    fn vfeat_each_area_detected_from_a_single_entry() {
        let cases: Vec<(Vec<u8>, &str)> = vec![
            (
                build_zip(&[("xl/slicers/slicer1.xml", 0, b"<slicers/>")]),
                "slicers",
            ),
            (
                build_zip(&[(
                    "xl/slicerCaches/slicerCache1.xml",
                    0,
                    b"<slicerCacheDefinition/>",
                )]),
                "slicers",
            ),
            (
                build_zip(&[("xl/timelines/timeline1.xml", 0, b"<timelines/>")]),
                "timelines",
            ),
            (
                build_zip(&[(
                    "xl/timelineCaches/timelineCache1.xml",
                    0,
                    b"<timelineCacheDefinition/>",
                )]),
                "timelines",
            ),
            (
                build_zip(&[("xl/richData/rdrichvalue.xml", 0, b"<rvData/>")]),
                "rich_data",
            ),
            (
                build_zip(&[("xl/connections.xml", 0, b"<connections/>")]),
                "power_query",
            ),
            (
                build_zip(&[("xl/queryTables/queryTable1.xml", 0, b"<qTable/>")]),
                "power_query",
            ),
            (
                build_zip(&[("xl/model/dataModel.xml", 0, b"<dataModel/>")]),
                "power_query",
            ),
            (
                build_zip(&[(
                    "xl/threadedComments/threadedComment1.xml",
                    0,
                    b"<ThreadedComments/>",
                )]),
                "threaded_comments",
            ),
            (
                build_zip(&[("xl/ctrlProps/ctrlProp1.xml", 0, b"<ctrlProp/>")]),
                "controls",
            ),
            (
                build_zip(&[("xl/activeX/activeX1.xml", 0, b"<activeX/>")]),
                "controls",
            ),
            (
                build_zip(&[("xl/embeddings/oleObject1.bin", 0, b"\x00\x01\x02")]),
                "controls",
            ),
            (
                build_zip(&[("xl/externalLinks/externalLink1.xml", 0, b"<externalLink/>")]),
                "external_links",
            ),
            (
                build_zip(&[("_xmlsignatures/sig1.xml", 0, b"<Signature/>")]),
                "signed",
            ),
        ];
        for (zip, area) in cases {
            let flags = detect_features(&zip).unwrap();
            let mut expected = FeatureFlags::default();
            match area {
                "slicers" => expected.has_slicers = true,
                "timelines" => expected.has_timelines = true,
                "rich_data" => expected.has_rich_data = true,
                "power_query" => expected.has_power_query = true,
                "threaded_comments" => expected.has_threaded_comments = true,
                "controls" => expected.has_controls = true,
                "external_links" => expected.has_external_links = true,
                "signed" => expected.is_signed = true,
                _ => unreachable!(),
            }
            expected.part_count = 1;
            assert_eq!(flags, expected, "area {area}");
        }
    }

    #[test]
    fn vfeat_multiple_features_detected_in_one_walk() {
        let zip = build_zip(&[
            ("[Content_Types].xml", 0, b"<Types/>"),
            ("xl/slicers/slicer1.xml", 0, b"<slicers/>"),
            (
                "xl/slicerCaches/slicerCache1.xml",
                0,
                b"<slicerCacheDefinition/>",
            ),
            ("xl/richData/rdrichvalue.xml", 0, b"<rvData/>"),
            ("xl/externalLinks/externalLink1.xml", 0, b"<externalLink/>"),
            ("_xmlsignatures/sig1.xml", 0, b"<Signature/>"),
            ("_xmlsignatures/origin.sigs", 0, b"<Relationships/>"),
            ("xl/persons/person.xml", 0, b"<personList/>"),
        ]);
        let flags = detect_features(&zip).unwrap();
        assert!(flags.has_slicers);
        assert!(flags.has_rich_data);
        assert!(flags.has_external_links);
        assert!(flags.is_signed);
        assert!(!flags.has_timelines);
        assert!(!flags.has_sparklines);
        assert!(!flags.has_power_query);
        assert!(!flags.has_threaded_comments);
        assert!(!flags.has_controls);
        assert!(!flags.has_ole_objects);
        // 2 slicer + 1 rich data + 1 external link + 2 signature + 1 persons.
        assert_eq!(flags.part_count, 7);
    }

    #[test]
    fn vfeat_persons_part_counts_but_does_not_flag_comments() {
        let zip = build_zip(&[("xl/persons/person.xml", 0, b"<personList/>")]);
        let flags = detect_features(&zip).unwrap();
        assert!(!flags.has_threaded_comments);
        assert_eq!(flags.part_count, 1);
    }

    /// A worksheet carrying all three sheet-resident areas. The shallow pass
    /// must leave all three flags false (no feature part directories exist);
    /// only the deep variant's inflate-and-probe decides them.
    const SHEET_RESIDENT: &[u8] = b"<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData/>\
        <extLst><ext uri=\"{05C60535-1F16-4fd2-B633-F4F36F0B64E0}\"><x14:sparklineGroups><x14:sparklineGroup type=\"line\"><x14:sparkline f=\"Sheet1!B1:F1\" sqref=\"A1\"/></x14:sparklineGroup></x14:sparklineGroups></ext></extLst>\
        <controls><control shapeId=\"1\" r:id=\"rId1\" name=\"Button 1\"/></controls>\
        <oleObjects><oleObject progId=\"Word.Document.12\" shapeId=\"2\" r:id=\"rId2\"/></oleObjects>\
        </worksheet>";

    #[test]
    fn vfeat_deep_probes_sheets_for_sheet_resident_features() {
        let zip = build_zip(&[("xl/worksheets/sheet1.xml", 0, SHEET_RESIDENT)]);

        let shallow = detect_features(&zip).unwrap();
        assert!(!shallow.has_sparklines);
        assert!(!shallow.has_controls);
        assert!(!shallow.has_ole_objects);
        assert_eq!(shallow.part_count, 0);

        let deep = detect_features_deep(&zip).unwrap();
        assert!(deep.has_sparklines);
        assert!(deep.has_controls);
        assert!(deep.has_ole_objects);
        assert_eq!(deep.part_count, 0);
    }

    #[test]
    fn vfeat_deep_unreadable_sheet_degrades_not_panics() {
        // A deflate(8) worksheet whose payload is garbage: the deep variant
        // skips it and keeps going rather than failing or panicking.
        let zip = build_zip(&[("xl/worksheets/sheet1.xml", 8, b"\x00")]);
        let deep = detect_features_deep(&zip).unwrap();
        assert!(!deep.has_sparklines);
        assert!(!deep.has_controls);
        assert!(!deep.has_ole_objects);
        assert_eq!(deep.part_count, 0);
    }

    #[test]
    fn vfeat_malformed_zip_is_err_not_panic() {
        assert!(detect_features(b"this is not a zip").is_err());
        assert!(detect_features_deep(b"").is_err());
        let mut truncated = build_zip(&[("xl/richData/rdrichvalue.xml", 0, b"<rvData/>")]);
        truncated.truncate(16);
        assert!(detect_features(&truncated).is_err());
        assert!(detect_features_deep(&truncated).is_err());
    }

    #[test]
    fn vfeat_findings_one_per_area_and_signature_is_warning() {
        let flags = FeatureFlags {
            has_slicers: true,
            has_timelines: true,
            has_sparklines: true,
            has_rich_data: true,
            has_power_query: true,
            has_threaded_comments: true,
            has_controls: true,
            has_ole_objects: true,
            has_external_links: true,
            is_signed: true,
            part_count: 10,
        };
        let findings = feature_findings(&flags);
        assert_eq!(findings.len(), 10);
        assert_eq!(
            findings
                .iter()
                .filter(|f| f.severity == Severity::Info)
                .count(),
            9
        );
        assert_eq!(
            findings
                .iter()
                .filter(|f| f.severity == Severity::Warning)
                .count(),
            1
        );
        let sig = findings
            .iter()
            .find(|f| f.severity == Severity::Warning)
            .unwrap();
        assert_eq!(sig.part, "_xmlsignatures/");
        assert!(sig.message.contains("tampered"));
        // Presence is a fact, not a defect: nothing is repairable, and every
        // Presence findings all carry FeaturePresent, defects never do.
        for f in &findings {
            assert!(!f.repairable);
            assert_eq!(f.code, FindingCode::FeaturePresent);
        }
    }
}
