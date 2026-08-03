//! Sheet-level and cross-part validation: dimension vs used range, cell refs
//! in/out of grid, duplicate cells, row order, overlapping merges, inverted
//! col ranges, empty-sqref data validations, out-of-range style / shared-string
//! / dxf indices, and formulas referencing missing sheets.

use std::collections::{HashMap, HashSet};

use super::repair::{Fix, FixOp};
use super::{
    Finding, FindingCode, Severity, ValidateReport, a1_to_rc, attr, find_tag, parse_range,
    range_to_a1, tag_end, utf8,
};

const MAX_ROW: u32 = 1_048_576;
const MAX_COL: u32 = 16_384;

/// Run every sheet-level check over all worksheet parts.
pub fn check_sheets(
    parts: &HashMap<String, Vec<u8>>,
    report: &mut ValidateReport,
    fixes: &mut Vec<Fix>,
) {
    let sst_count = parts
        .get("xl/sharedStrings.xml")
        .map(|b| count_si(b))
        .unwrap_or(0);
    let (xf_count, dxf_count) = parts
        .get("xl/styles.xml")
        .map(|b| count_xf_dxf(b))
        .unwrap_or((0, 0));
    let names: Vec<String> = match parts.get("xl/workbook.xml") {
        Some(wb) => crate::turbo::structural::parse_workbook(wb)
            .0
            .into_iter()
            .map(|m| m.name)
            .collect(),
        None => Vec::new(),
    };

    let mut part_names: Vec<&str> = parts.keys().map(|s| s.as_str()).collect();
    part_names.sort();
    for part in part_names {
        if !(part.starts_with("xl/worksheets/") && part.ends_with(".xml")) {
            continue;
        }
        let xml = parts.get(part).unwrap();
        sheet_checks(
            part,
            xml,
            Some(&sst_count),
            Some(&xf_count),
            Some(&dxf_count),
            &names,
            report,
            fixes,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn sheet_checks(
    part: &str,
    xml: &[u8],
    sst_count: Option<&usize>,
    xf_count: Option<&usize>,
    dxf_count: Option<&usize>,
    names: &[String],
    report: &mut ValidateReport,
    fixes: &mut Vec<Fix>,
) {
    // Declared dimension.
    let declared = dimension_of(xml);

    // Cell pass: used range, grid bounds, duplicates, style/ss indices, formulas.
    let mut used: (u32, u32, u32, u32) = (u32::MAX, u32::MAX, 0, 0);
    let mut cells: HashSet<(u32, u32)> = HashSet::new();
    let mut cell_count: usize = 0;
    for_each_cell(xml, |tag, body| {
        cell_count += 1;

        if let Some(rb) = attr(tag, b"r") {
            match a1_to_rc(rb) {
                Some((row, col)) => {
                    if row == 0 || col == 0 || row > MAX_ROW || col > MAX_COL {
                        report.add(Finding::new(
                            FindingCode::CellOutOfGrid,
                            Severity::Error,
                            part.to_string(),
                            Some(utf8(rb)),
                            format!(
                                "cell {} is outside the {MAX_ROW} x {MAX_COL} grid",
                                utf8(rb)
                            ),
                            false,
                        ));
                    } else {
                        used.0 = used.0.min(row);
                        used.1 = used.1.min(col);
                        used.2 = used.2.max(row);
                        used.3 = used.3.max(col);
                        let key = (row, col);
                        if !cells.insert(key) {
                            report.add(Finding::new(
                                FindingCode::DuplicateCell,
                                Severity::Error,
                                part.to_string(),
                                Some(utf8(rb)),
                                format!("duplicate cell reference {}", utf8(rb)),
                                true,
                            ));
                            fixes.push(Fix {
                                code: FindingCode::DuplicateCell,
                                severity: Severity::Error,
                                part: part.to_string(),
                                description: format!(
                                    "dropped duplicate cell {} (kept first)",
                                    utf8(rb)
                                ),
                                op: FixOp::RemoveElementFromEnd {
                                    name: "c".into(),
                                    attr: "r".into(),
                                    value: utf8(rb),
                                },
                            });
                        }
                    }
                }
                None => {
                    report.add(Finding::new(
                        FindingCode::InvalidCellRef,
                        Severity::Warning,
                        part.to_string(),
                        Some(utf8(rb)),
                        format!("could not parse cell reference {}", utf8(rb)),
                        false,
                    ));
                }
            }
        } else {
            report.add(Finding::new(
                FindingCode::InvalidCellRef,
                Severity::Warning,
                part.to_string(),
                None,
                "cell element without an r reference",
                false,
            ));
        }

        if let Some(sb) = attr(tag, b"s") {
            if let Ok(idx) = std::str::from_utf8(sb).unwrap_or("").trim().parse::<u32>() {
                if let Some(cap) = xf_count {
                    if *cap > 0 && idx >= *cap as u32 {
                        report.add(Finding::new(
                            FindingCode::StyleIndexOor,
                            Severity::Warning,
                            part.to_string(),
                            Some(format!("s={idx}")),
                            format!("style index {idx} is out of range ({cap} cellXfs)"),
                            false,
                        ));
                    }
                }
            }
        }

        if attr(tag, b"t") == Some(b"s") {
            if let Some(cap) = sst_count {
                if let Some(v) = element_text(body, b"v") {
                    if let Ok(idx) = std::str::from_utf8(v).unwrap_or("").trim().parse::<u32>() {
                        if *cap > 0 && idx >= *cap as u32 {
                            report.add(Finding::new(
                                FindingCode::SharedStringIndexOor,
                                Severity::Warning,
                                part.to_string(),
                                Some(format!("si={idx}")),
                                format!(
                                    "shared-string index {idx} is out of range ({cap} strings)"
                                ),
                                false,
                            ));
                        }
                    }
                }
            }
        }

        if let Some(ftxt) = element_text(body, b"f") {
            check_formula_sheet(ftxt, names, part, report);
        }
    });

    // Dimension vs actual used range.
    if let Some((dr0, dc0, dr1, dc1)) = declared {
        let actual: Option<(u32, u32, u32, u32)> = if cell_count == 0 {
            Some((1, 1, 1, 1))
        } else {
            Some(used)
        };
        if let Some(actual) = actual {
            if (dr0, dc0, dr1, dc1) != actual {
                let declared_a1 = range_to_a1((dr0, dc0, dr1, dc1));
                let actual_a1 = range_to_a1(actual);
                report.add(Finding::new(
                    FindingCode::DimensionMismatch,
                    Severity::Warning,
                    part.to_string(),
                    Some(declared_a1.clone()),
                    format!("declared dimension {declared_a1} disagrees with actual used range {actual_a1}"),
                    true,
                ));
                fixes.push(Fix {
                    code: FindingCode::DimensionMismatch,
                    severity: Severity::Warning,
                    part: part.to_string(),
                    description: format!("rewrote <dimension> from {declared_a1} to {actual_a1}"),
                    op: FixOp::SetAttrValue {
                        name: "dimension".into(),
                        attr: "ref".into(),
                        value: actual_a1,
                    },
                });
            }
        }
    }

    // Row order.
    if memchr::memmem::find(xml, b"<row").is_some() {
        let mut prev_row: Option<u32> = None;
        let mut pos = 0usize;
        loop {
            let s = match memchr::memmem::find(&xml[pos..], b"<row ") {
                Some(o) => pos + o,
                None => match find_tag(xml, b"row", pos) {
                    Some(s) => s,
                    None => break,
                },
            };
            let Some(gt) = tag_end(xml, s) else {
                break;
            };
            let tag = &xml[s + 1..gt];
            if let Some(rb) = attr(tag, b"r") {
                if let Ok(r) = std::str::from_utf8(rb).unwrap_or("").trim().parse::<u32>() {
                    if let Some(p) = prev_row {
                        if r < p {
                            report.add(Finding::new(
                                FindingCode::RowOutOfOrder,
                                Severity::Warning,
                                part.to_string(),
                                Some(format!("row {r} after row {p}")),
                                format!("row {r} appears after row {p}"),
                                false,
                            ));
                        }
                    }
                    prev_row = Some(r);
                }
            }
            pos = gt + 1;
        }
    }

    // Inverted col ranges.
    if memchr::memmem::find(xml, b"<col").is_some() {
        let mut pos = 0usize;
        while let Some(s) = find_tag(xml, b"col", pos) {
            let Some(gt) = tag_end(xml, s) else {
                break;
            };
            let tag = &xml[s + 1..gt];
            let mn = attr(tag, b"min")
                .and_then(|v| std::str::from_utf8(v).ok()?.trim().parse::<u32>().ok());
            let mx = attr(tag, b"max")
                .and_then(|v| std::str::from_utf8(v).ok()?.trim().parse::<u32>().ok());
            if let (Some(mn), Some(mx)) = (mn, mx) {
                if mn > mx {
                    report.add(Finding::new(
                        FindingCode::InvertedColRange,
                        Severity::Warning,
                        part.to_string(),
                        Some(format!("min={mn} max={mx}")),
                        format!("col range min={mn} is greater than max={mx}"),
                        true,
                    ));
                    fixes.push(Fix {
                        code: FindingCode::InvertedColRange,
                        severity: Severity::Warning,
                        part: part.to_string(),
                        description: format!("dropped inverted <col min={mn} max={mx}>"),
                        op: FixOp::RemoveElement {
                            name: "col".into(),
                            attr: "min".into(),
                            value: mn.to_string(),
                        },
                    });
                }
            }
            pos = gt + 1;
        }
    }

    // Empty-sqref data validations.
    if memchr::memmem::find(xml, b"<dataValidation").is_some() {
        let mut pos = 0usize;
        let mut empty_dv = false;
        while let Some(s) = find_tag(xml, b"dataValidation", pos) {
            let Some(gt) = tag_end(xml, s) else {
                break;
            };
            let tag = &xml[s + 1..gt];
            let empty = match attr(tag, b"sqref") {
                Some(sq) => sq.iter().all(|b| b.is_ascii_whitespace()),
                None => true,
            };
            if empty {
                empty_dv = true;
            }
            pos = gt + 1;
        }
        if empty_dv {
            report.add(Finding::new(
                FindingCode::EmptyValidationSqref,
                Severity::Warning,
                part.to_string(),
                None,
                "data validation with an empty sqref",
                true,
            ));
            fixes.push(Fix {
                code: FindingCode::EmptyValidationSqref,
                severity: Severity::Warning,
                part: part.to_string(),
                description: "dropped data validation(s) with an empty sqref".to_string(),
                op: FixOp::RemoveElementWhere {
                    name: "dataValidation".into(),
                    attr: "sqref".into(),
                },
            });
        }
    }

    // Out-of-range conditional-format dxfId.
    if memchr::memmem::find(xml, b"<cfRule").is_some() {
        let mut pos = 0usize;
        while let Some(s) = find_tag(xml, b"cfRule", pos) {
            let Some(gt) = tag_end(xml, s) else {
                break;
            };
            let tag = &xml[s + 1..gt];
            if let Some(db) = attr(tag, b"dxfId") {
                if let Ok(idx) = std::str::from_utf8(db).unwrap_or("").trim().parse::<u32>() {
                    if let Some(cap) = dxf_count {
                        if *cap > 0 && idx >= *cap as u32 {
                            report.add(Finding::new(
                                FindingCode::DxfIndexOor,
                                Severity::Warning,
                                part.to_string(),
                                Some(format!("dxfId={idx}")),
                                format!(
                                    "conditional-format dxfId {idx} is out of range ({cap} dxfs)"
                                ),
                                false,
                            ));
                        }
                    }
                }
            }
            pos = gt + 1;
        }
    }

    // Overlapping merged ranges (Excel silently drops one — that is data loss).
    let mut merges: Vec<(u32, u32, u32, u32)> = Vec::new();
    if memchr::memmem::find(xml, b"<mergeCell").is_some() {
        let mut pos = 0usize;
        while let Some(s) = find_tag(xml, b"mergeCell", pos) {
            let Some(gt) = tag_end(xml, s) else {
                break;
            };
            let tag = &xml[s + 1..gt];
            if let Some(rb) = attr(tag, b"ref") {
                if let Some(m) = parse_range(rb) {
                    merges.push(m);
                }
            }
            pos = gt + 1;
        }
    }
    merges.sort_by_key(|m| m.0);
    let mut active: Vec<(u32, u32, u32, u32)> = Vec::new();
    for m in merges {
        active.retain(|a| a.2 >= m.0);
        for a in &active {
            if a.1 <= m.3 && m.1 <= a.3 {
                let later = range_to_a1(m);
                report.add(Finding::new(
                    FindingCode::OverlappingMerge,
                    Severity::Error,
                    part.to_string(),
                    Some(later.clone()),
                    format!(
                        "merged range {} overlaps {}; Excel silently drops one and data is lost",
                        later,
                        range_to_a1(*a)
                    ),
                    true,
                ));
                fixes.push(Fix {
                    code: FindingCode::OverlappingMerge,
                    severity: Severity::Error,
                    part: part.to_string(),
                    description: format!("dropped overlapping merge {later}"),
                    op: FixOp::RemoveElement {
                        name: "mergeCell".into(),
                        attr: "ref".into(),
                        value: later,
                    },
                });
            }
        }
        active.push(m);
    }
}

fn dimension_of(xml: &[u8]) -> Option<(u32, u32, u32, u32)> {
    let o = match memchr::memmem::find(xml, b"<dimension") {
        Some(o) => o,
        None => find_tag(xml, b"dimension", 0)?,
    };
    let gt = tag_end(xml, o)?;
    let tag = &xml[o + 1..gt];
    let rb = attr(tag, b"ref")?;
    parse_range(rb)
}

/// Visit every `<c ...>...</c>` cell: its open-tag bytes and its body (the
/// region between the open tag and `</c>`). Fast path is a SIMD scan for the
/// un-namespaced `<c ` form; a namespace-prefixed sheet falls back to the
/// tolerant per-tag walk. Never panics.
fn for_each_cell(xml: &[u8], mut f: impl FnMut(&[u8], &[u8])) {
    let has_prefix = memchr::memmem::find(xml, b":c ").is_some()
        || memchr::memmem::find(xml, b":c>").is_some()
        || memchr::memmem::find(xml, b":c/").is_some();
    let mut pos = 0usize;
    loop {
        let s = if has_prefix {
            match find_tag(xml, b"c", pos) {
                Some(s) => s,
                None => break,
            }
        } else {
            match memchr::memmem::find(&xml[pos..], b"<c ") {
                Some(o) => pos + o,
                None => break,
            }
        };
        let Some(gt) = tag_end(xml, s) else {
            break;
        };
        let tag = &xml[s + 1..gt];
        let self_close = xml.get(gt.saturating_sub(1)) == Some(&b'/');
        let body_start = gt + 1;
        let (next_pos, body) = if self_close {
            (body_start, &xml[body_start..body_start])
        } else {
            match memchr::memmem::find(&xml[body_start..], b"</c>") {
                Some(c) => (body_start + c + 4, &xml[body_start..body_start + c]),
                None => (xml.len(), &xml[body_start..]),
            }
        };
        f(tag, body);
        pos = next_pos;
    }
}

/// Text content of the first `<name>...</name>` element in `body`.
/// Allocation-free: the closing tag is matched without building a needle.
fn element_text<'a>(body: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    let o = memchr::memmem::find(body, name)?;
    let gt = memchr::memchr(b'>', &body[o..])? + o;
    if body.get(gt.saturating_sub(1)) == Some(&b'/') {
        return None;
    }
    let mut pos = gt + 1;
    while let Some(rel) = memchr::memmem::find(&body[pos..], b"</") {
        let close = pos + rel;
        let rest = &body[close + 2..];
        if rest.len() > name.len()
            && &rest[..name.len()] == name
            && rest.get(name.len()) == Some(&b'>')
        {
            return Some(&body[gt + 1..close]);
        }
        pos = close + 2;
    }
    None
}

/// Flag a formula whose sheet-qualified reference names a sheet that does not
/// exist. Only `!` tokens that are followed by a cell-like ref are considered,
/// so string literals containing `!` are not misread.
fn check_formula_sheet(text: &[u8], names: &[String], part: &str, report: &mut ValidateReport) {
    let s = match std::str::from_utf8(text) {
        Ok(s) => s,
        Err(_) => return,
    };
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let Some(bang_rel) = memchr::memchr(b'!', &bytes[i..]) else {
            break;
        };
        let bang = i + bang_rel;
        // Must be followed by a cell-like reference (letter or $).
        match bytes.get(bang + 1) {
            Some(b) if b.is_ascii_alphabetic() || *b == b'$' => {}
            _ => {
                i = bang + 1;
                continue;
            }
        }
        let mut j = bang;
        while j > 0 {
            let c = bytes[j - 1];
            if c == b'\'' {
                j -= 1;
                break;
            }
            if c.is_ascii_alphanumeric() || c == b'_' || c == b'.' || c == b'$' || c == b' ' {
                j -= 1;
            } else {
                break;
            }
        }
        if j < bang {
            let qual = &s[j..bang];
            let q = qual.trim_matches('\'').trim();
            if !q.is_empty() && !q.contains('[') && !names.iter().any(|n| n.eq_ignore_ascii_case(q))
            {
                report.add(Finding::new(
                    FindingCode::FormulaMissingSheet,
                    Severity::Warning,
                    part.to_string(),
                    Some(q.to_string()),
                    format!("formula references sheet '{q}' which does not exist"),
                    false,
                ));
            }
        }
        i = bang + 1;
    }
}

/// Count `<si>` elements in a sharedStrings part.
fn count_si(xml: &[u8]) -> usize {
    let mut c = 0usize;
    let mut i = 0usize;
    while let Some(o) = memchr::memmem::find(&xml[i..], b"<si") {
        let s = i + o;
        let after = xml.get(s + 3).copied().unwrap_or(b'>');
        if after == b' ' || after == b'>' || after == b'/' {
            c += 1;
        }
        i = s + 3;
    }
    c
}

/// Count `<xf>` in cellXfs and `<dxf>` in dxfs (bounding regions avoid
/// overcounting the cellStyleXfs block).
fn count_xf_dxf(xml: &[u8]) -> (usize, usize) {
    let mut xfs = 0usize;
    if let Some(o) = memchr::memmem::find(xml, b"<cellXfs") {
        let s = o + memchr::memchr(b'>', &xml[o..]).unwrap_or(0);
        let e = memchr::memmem::find(&xml[s..], b"</cellXfs>")
            .map(|p| s + p)
            .unwrap_or(xml.len());
        let mut i = s;
        while let Some(p) = memchr::memmem::find(&xml[i..e], b"<xf") {
            xfs += 1;
            i += p + 3;
        }
    }
    let mut dxfs = 0usize;
    if let Some(o) = memchr::memmem::find(xml, b"<dxfs") {
        let s = o + memchr::memchr(b'>', &xml[o..]).unwrap_or(0);
        let e = memchr::memmem::find(&xml[s..], b"</dxfs>")
            .map(|p| s + p)
            .unwrap_or(xml.len());
        let mut i = s;
        while let Some(p) = memchr::memmem::find(&xml[i..e], b"<dxf") {
            dxfs += 1;
            i += p + 4;
        }
    }
    (xfs, dxfs)
}
