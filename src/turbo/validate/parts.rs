//! Required parts, content types, and relationship integrity.

use std::collections::{HashMap, HashSet};

use super::repair::{Fix, FixOp};
use super::{Finding, FindingCode, Severity, ValidateReport, attr, find_tag, tag_end, utf8};
use crate::turbo::structural::{RelKind, RelMap, parse_rels, parse_workbook, resolve_zip_path};

/// Check `[Content_Types].xml`: required overrides present, declared types
/// matching their parts, and no override naming a missing part.
pub fn check_content_types(
    parts: &HashMap<String, Vec<u8>>,
    entries_map: &HashMap<String, crate::turbo::zipmin::ZipEntryMeta>,
    report: &mut ValidateReport,
    fixes: &mut Vec<Fix>,
) {
    let Some(ct) = parts.get("[Content_Types].xml") else {
        return; // missing already flagged
    };
    let (_defaults, overrides) = parse_content_types(ct);
    let has_override = |partname: &str| overrides.iter().any(|(p, _)| p == partname);

    if !has_override("/xl/workbook.xml") {
        report.add(Finding::new(
            FindingCode::MissingContentType,
            Severity::Error,
            "[Content_Types].xml",
            Some("/xl/workbook.xml".into()),
            "no content-type override for the workbook",
            false,
        ));
    }

    for part in entries_map.keys() {
        if part.starts_with("xl/worksheets/") && part.ends_with(".xml") {
            let pn = format!("/{part}");
            if !has_override(&pn) {
                report.add(Finding::new(
                    FindingCode::MissingContentType,
                    Severity::Warning,
                    "[Content_Types].xml",
                    Some(part.clone()),
                    format!("no content-type override for worksheet {part}"),
                    false,
                ));
            }
        }
    }

    for (partname, declared) in &overrides {
        let part = partname.trim_start_matches('/');
        if let Some(expected) = classify_part(part) {
            let ok = if expected == "sheet.main+xml" {
                declared.ends_with("sheet.main+xml") || declared.ends_with("macroEnabled.main+xml")
            } else {
                declared.ends_with(expected)
            };
            if !ok {
                report.add(Finding::new(
                    FindingCode::ContentTypeMismatch,
                    Severity::Warning,
                    "[Content_Types].xml",
                    Some(part.to_string()),
                    format!("declared content type '{declared}' does not match part {part} (expected *{expected})"),
                    false,
                ));
            }
        }
        if !entries_map.contains_key(part) {
            report.add(Finding::new(
                FindingCode::MissingPartForContentType,
                Severity::Warning,
                "[Content_Types].xml",
                Some(part.to_string()),
                format!("content-type override names part {part} which is not in the archive"),
                true,
            ));
            fixes.push(Fix {
                code: FindingCode::MissingPartForContentType,
                severity: Severity::Warning,
                part: "[Content_Types].xml".into(),
                description: format!("dropped content-type override for missing part {part}"),
                op: FixOp::RemoveElement {
                    name: "Override".into(),
                    attr: "PartName".into(),
                    value: partname.clone(),
                },
            });
        }
    }
}

/// Check relationship integrity across every rels part, workbook sheet
/// bindings, defined names, pivot caches, and threaded-comment persons.
pub fn check_rels(
    parts: &HashMap<String, Vec<u8>>,
    entries_map: &HashMap<String, crate::turbo::zipmin::ZipEntryMeta>,
    report: &mut ValidateReport,
    fixes: &mut Vec<Fix>,
) {
    // Root rels must bind the office document.
    let root_rels: RelMap = parts
        .get("_rels/.rels")
        .map(|b| parse_rels(b))
        .unwrap_or_default();
    let mut office_found = false;
    for rel in root_rels.values() {
        if rel.external {
            continue;
        }
        if resolve_zip_path("", &rel.target) == "xl/workbook.xml" {
            office_found = true;
        }
    }
    if !office_found {
        report.add(Finding::new(
            FindingCode::MissingOfficeDocumentRel,
            Severity::Error,
            "_rels/.rels",
            None,
            "no officeDocument relationship targets xl/workbook.xml",
            false,
        ));
    }

    // Generic dangling-rel scan across every rels part.
    for rels_part in parts.keys() {
        if !rels_part.ends_with(".rels") {
            continue;
        }
        let base = base_dir_for_rels(rels_part);
        let rels = parse_rels(parts.get(rels_part).unwrap());
        for (rid, rel) in &rels {
            if rel.external {
                continue;
            }
            let path = resolve_zip_path(&base, &rel.target);
            if !entries_map.contains_key(&path) {
                let binds_sheet = rel.kind == RelKind::Worksheet || rel.kind == RelKind::Chartsheet;
                let severity = if binds_sheet {
                    Severity::Error
                } else {
                    Severity::Warning
                };
                report.add(Finding::new(
                    FindingCode::DanglingRel,
                    severity,
                    rels_part.clone(),
                    Some(rid.clone()),
                    format!("relationship {rid} targets missing part {path}"),
                    true,
                ));
                fixes.push(Fix {
                    code: FindingCode::DanglingRel,
                    severity,
                    part: rels_part.clone(),
                    description: format!(
                        "dropped relationship {rid} targeting missing part {path}"
                    ),
                    op: FixOp::RemoveElement {
                        name: "Relationship".into(),
                        attr: "Id".into(),
                        value: rid.clone(),
                    },
                });
            }
        }
    }

    let workbook_xml = parts.get("xl/workbook.xml").map(|b| b.as_slice());
    let (sheet_metas, defined_names) = match workbook_xml {
        Some(x) => parse_workbook(x),
        None => (Vec::new(), Vec::new()),
    };
    let wb_rels: RelMap = parts
        .get("xl/_rels/workbook.xml.rels")
        .map(|b| parse_rels(b))
        .unwrap_or_default();

    // Sheet bindings: every workbook sheet must resolve to an existing part of
    // the right kind.
    let mut bound_sheets: HashSet<String> = HashSet::new();
    for sm in &sheet_metas {
        match &sm.rid {
            Some(rid) => match wb_rels.get(rid) {
                Some(rel) => {
                    if rel.external {
                        report.add(Finding::new(
                            FindingCode::WrongRelKind,
                            Severity::Error,
                            "xl/workbook.xml",
                            Some(sm.name.clone()),
                            format!("sheet {} r:id {rid} points at an external target", sm.name),
                            false,
                        ));
                        continue;
                    }
                    let path = resolve_zip_path("xl/", &rel.target);
                    // The relationship Type is what decides worksheet vs
                    // chartsheet (workbook.xml sheet entries carry no kind), so
                    // only a rel that is neither is genuinely wrong.
                    if rel.kind != RelKind::Worksheet && rel.kind != RelKind::Chartsheet {
                        report.add(Finding::new(
                            FindingCode::WrongRelKind,
                            Severity::Error,
                            "xl/_rels/workbook.xml.rels",
                            Some(format!("sheet {} r:id {rid}", sm.name)),
                            format!(
                                "sheet relationship {rid} has kind {:?}; expected a worksheet or chartsheet relationship",
                                rel.kind
                            ),
                            false,
                        ));
                    }
                    if !entries_map.contains_key(&path) {
                        report.add(Finding::new(
                            FindingCode::MissingSheetPart,
                            Severity::Error,
                            path.clone(),
                            Some(sm.name.clone()),
                            format!("worksheet part {path} is missing"),
                            false,
                        ));
                    }
                    bound_sheets.insert(path);
                }
                None => {
                    report.add(Finding::new(
                        FindingCode::MissingRel,
                        Severity::Warning,
                        "xl/workbook.xml",
                        Some(sm.name.clone()),
                        format!(
                            "sheet {} r:id {rid} is not in the workbook relationships",
                            sm.name
                        ),
                        false,
                    ));
                }
            },
            None => {
                report.add(Finding::new(
                    FindingCode::MissingRel,
                    Severity::Warning,
                    "xl/workbook.xml",
                    Some(sm.name.clone()),
                    format!("sheet {} has no r:id", sm.name),
                    false,
                ));
            }
        }
    }
    if sheet_metas.is_empty() {
        report.add(Finding::new(
            FindingCode::NoSheets,
            Severity::Error,
            "xl/workbook.xml",
            None,
            "workbook declares no sheets",
            false,
        ));
    }

    // Worksheet parts that no workbook sheet binds.
    for part in parts.keys() {
        if part.starts_with("xl/worksheets/")
            && part.ends_with(".xml")
            && !bound_sheets.contains(part)
        {
            report.add(Finding::new(
                FindingCode::SheetNotReferenced,
                Severity::Error,
                part.clone(),
                None,
                format!("worksheet part {part} is not referenced by any workbook sheet"),
                false,
            ));
        }
    }

    // Use-site relationship kinds (drawing / hyperlink / tablePart / pivotTable).
    for part in parts.keys() {
        let (dir, file) = if part.starts_with("xl/worksheets/") {
            (
                "xl/worksheets/",
                part.strip_prefix("xl/worksheets/").unwrap(),
            )
        } else if part.starts_with("xl/chartsheets/") {
            (
                "xl/chartsheets/",
                part.strip_prefix("xl/chartsheets/").unwrap(),
            )
        } else {
            continue;
        };
        if !part.ends_with(".xml") {
            continue;
        }
        let rels_part = format!("{dir}_rels/{file}.rels");
        let rels = parts
            .get(&rels_part)
            .map(|b| parse_rels(b))
            .unwrap_or_default();
        check_use_sites(part, parts.get(part).unwrap(), &rels, report);
    }

    // Pivot tables must resolve a cacheDefinition relationship.
    for part in parts.keys() {
        if part.starts_with("xl/pivotTables/") && part.ends_with(".xml") {
            let file = part.rsplit('/').next().unwrap();
            let rels_part = format!("xl/pivotTables/_rels/{file}.rels");
            let rels = parts
                .get(&rels_part)
                .map(|b| parse_rels(b))
                .unwrap_or_default();
            if !rels.values().any(|r| r.kind == RelKind::PivotCacheDef) {
                report.add(Finding::new(
                    FindingCode::PivotCacheMissingRel,
                    Severity::Warning,
                    part.clone(),
                    None,
                    "pivot table has no cacheDefinition relationship",
                    false,
                ));
            }
        }
    }

    // Threaded comments must reference a known person.
    let persons_part = wb_rels
        .values()
        .find(|r| r.kind == RelKind::Person)
        .map(|r| resolve_zip_path("xl/", &r.target));
    let person_ids: HashSet<String> = match persons_part.as_deref().and_then(|p| parts.get(p)) {
        Some(b) => person_ids(b),
        None => HashSet::new(),
    };
    for part in parts.keys() {
        if part.starts_with("xl/threadedComments/") && part.ends_with(".xml") {
            for pid in threaded_person_ids(parts.get(part).unwrap()) {
                if !person_ids.contains(&pid) {
                    report.add(Finding::new(
                        FindingCode::ThreadedUnknownPerson,
                        Severity::Warning,
                        part.clone(),
                        Some(pid.clone()),
                        format!("threaded comment references unknown person {pid}"),
                        false,
                    ));
                }
            }
        }
    }

    // Defined names pointing at #REF!.
    for dn in &defined_names {
        if dn.external {
            continue;
        }
        if dn.value.contains("#REF!") {
            report.add(Finding::new(
                FindingCode::DefinedNameRefError,
                Severity::Warning,
                "xl/workbook.xml",
                Some(dn.name.clone()),
                format!("defined name '{}' points at #REF!", dn.name),
                true,
            ));
            fixes.push(Fix {
                code: FindingCode::DefinedNameRefError,
                severity: Severity::Warning,
                part: "xl/workbook.xml".into(),
                description: format!("dropped defined name '{}' that points at #REF!", dn.name),
                op: FixOp::RemoveElement {
                    name: "definedName".into(),
                    attr: "name".into(),
                    value: dn.name.clone(),
                },
            });
        }
    }
}

fn check_use_sites(part: &str, xml: &[u8], rels: &RelMap, report: &mut ValidateReport) {
    check_site(
        xml,
        rels,
        b"drawing",
        b"r:id",
        RelKind::Drawing,
        part,
        report,
    );
    check_site(
        xml,
        rels,
        b"hyperlink",
        b"r:id",
        RelKind::Hyperlink,
        part,
        report,
    );
    check_site(
        xml,
        rels,
        b"tablePart",
        b"r:id",
        RelKind::Table,
        part,
        report,
    );
    check_site(
        xml,
        rels,
        b"pivotTable",
        b"r:id",
        RelKind::PivotTable,
        part,
        report,
    );
}

fn check_site(
    xml: &[u8],
    rels: &RelMap,
    name: &[u8],
    attr_name: &[u8],
    expected: RelKind,
    part: &str,
    report: &mut ValidateReport,
) {
    // `<name ` and `:name ` (namespace-prefixed) needles; SIMD-gated so sheets
    // without the element are skipped in a single fast scan.
    let mut needle = Vec::with_capacity(name.len() + 2);
    needle.push(b'<');
    needle.extend_from_slice(name);
    needle.push(b' ');
    let mut prefixed = Vec::with_capacity(name.len() + 2);
    prefixed.push(b':');
    prefixed.extend_from_slice(name);
    prefixed.push(b' ');
    if memchr::memmem::find(xml, &needle).is_none()
        && memchr::memmem::find(xml, &prefixed).is_none()
    {
        return;
    }
    let mut pos = 0usize;
    loop {
        let s = match memchr::memmem::find(&xml[pos..], &needle) {
            Some(o) => pos + o,
            None => match memchr::memmem::find(&xml[pos..], &prefixed) {
                Some(o) => pos + o,
                None => break,
            },
        };
        let Some(gt) = tag_end(xml, s) else {
            break;
        };
        let tag = &xml[s + 1..gt];
        if let Some(rid) = attr(tag, attr_name) {
            let rid = utf8(rid);
            match rels.get(&rid) {
                Some(rel) => {
                    if rel.kind != expected {
                        report.add(Finding::new(
                            FindingCode::WrongRelKind,
                            Severity::Warning,
                            part.to_string(),
                            Some(rid.clone()),
                            format!(
                                "relationship {rid} at this use site has kind {:?}, expected {:?}",
                                rel.kind, expected
                            ),
                            false,
                        ));
                    }
                }
                None => {
                    report.add(Finding::new(
                        FindingCode::MissingRel,
                        Severity::Warning,
                        part.to_string(),
                        Some(rid.clone()),
                        format!(
                            "relationship id {rid} used here is not in the sheet relationships"
                        ),
                        false,
                    ));
                }
            }
        }
        pos = gt + 1;
    }
}

#[allow(clippy::type_complexity)]
fn parse_content_types(xml: &[u8]) -> (Vec<(String, String)>, Vec<(String, String)>) {
    let mut defaults = Vec::new();
    let mut overrides = Vec::new();
    let mut i = 0usize;
    while let Some(s) = find_tag(xml, b"Default", i) {
        let Some(gt) = tag_end(xml, s) else {
            break;
        };
        let tag = &xml[s + 1..gt];
        if let (Some(e), Some(ct)) = (attr(tag, b"Extension"), attr(tag, b"ContentType")) {
            defaults.push((utf8(e), utf8(ct)));
        }
        i = gt + 1;
    }
    let mut i = 0usize;
    while let Some(s) = find_tag(xml, b"Override", i) {
        let Some(gt) = tag_end(xml, s) else {
            break;
        };
        let tag = &xml[s + 1..gt];
        if let (Some(p), Some(ct)) = (attr(tag, b"PartName"), attr(tag, b"ContentType")) {
            overrides.push((utf8(p), utf8(ct)));
        }
        i = gt + 1;
    }
    (defaults, overrides)
}

/// Expected content-type *suffix* for a well-known part, or `None` when the
/// part's type is not something we assert on (avoids false positives).
fn classify_part(part: &str) -> Option<&'static str> {
    if part == "xl/workbook.xml" {
        return Some("sheet.main+xml");
    }
    if part.starts_with("xl/worksheets/") && part.ends_with(".xml") {
        return Some("worksheet+xml");
    }
    if part.starts_with("xl/chartsheets/") && part.ends_with(".xml") {
        return Some("chartsheet+xml");
    }
    if part == "xl/styles.xml" {
        return Some("styles+xml");
    }
    if part == "xl/sharedStrings.xml" {
        return Some("sharedStrings+xml");
    }
    if part.starts_with("xl/tables/") && part.ends_with(".xml") {
        return Some("table+xml");
    }
    if part.starts_with("xl/comments") && part.ends_with(".xml") {
        return Some("comments+xml");
    }
    if part.starts_with("xl/threadedComments/") && part.ends_with(".xml") {
        return Some("threadedcomments+xml");
    }
    if part.starts_with("xl/persons/") && part.ends_with(".xml") {
        return Some("person+xml");
    }
    if part.starts_with("xl/drawings/") && part.ends_with(".xml") {
        return Some("drawing+xml");
    }
    if part.starts_with("xl/charts/") && part.ends_with(".xml") {
        return Some("chart+xml");
    }
    if part.starts_with("xl/pivotTables/") && part.ends_with(".xml") {
        return Some("pivotTable+xml");
    }
    if part.starts_with("xl/pivotCache/") && part.ends_with(".xml") {
        return Some("pivotCacheDefinition+xml");
    }
    if part == "docProps/core.xml" {
        return Some("core-properties+xml");
    }
    if part == "docProps/app.xml" {
        return Some("extended-properties+xml");
    }
    if part == "xl/vbaProject.bin" {
        return Some("vbaProject");
    }
    None
}

fn base_dir_for_rels(rels_part: &str) -> String {
    let dir = match rels_part.rsplit_once('/') {
        Some((dir, _file)) => dir,
        None => "",
    };
    let dir = dir.strip_suffix("/_rels").unwrap_or(dir);
    let dir = match dir {
        "_rels" => "",
        other => other,
    };
    format!("{}/", dir.trim_end_matches('/'))
}

/// `person` ids from a persons part.
fn person_ids(xml: &[u8]) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut i = 0usize;
    while let Some(s) = find_tag(xml, b"person", i) {
        let Some(gt) = tag_end(xml, s) else {
            break;
        };
        let tag = &xml[s + 1..gt];
        if let Some(id) = attr(tag, b"id") {
            out.insert(utf8(id));
        }
        i = gt + 1;
    }
    out
}

/// `personId` values from a threadedComments part.
fn threaded_person_ids(xml: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while let Some(s) = find_tag(xml, b"threadedComment", i) {
        let Some(gt) = tag_end(xml, s) else {
            break;
        };
        let tag = &xml[s + 1..gt];
        if let Some(id) = attr(tag, b"personId") {
            out.push(utf8(id));
        }
        i = gt + 1;
    }
    out
}
