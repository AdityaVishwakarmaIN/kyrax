//! XML well-formedness checking for the validator.
//!
//! The crate has no XML parser dependency, so this is a hand-rolled,
//! memchr-based balanced-tag scanner in the codebase's own style. It handles
//! comments, CDATA, processing instructions, namespaced tag names, self-closing
//! tags, and quoted attribute values. It reports a single first problem.

use std::collections::HashMap;

use super::{Finding, FindingCode, Severity, ValidateReport, tag_end, tag_local_name, utf8};

/// Check every XML part (and every rels part) for well-formedness.
pub fn check_wellformedness(parts: &HashMap<String, Vec<u8>>, report: &mut ValidateReport) {
    for (name, bytes) in parts {
        if !(name.ends_with(".xml") || name.ends_with(".rels")) {
            continue;
        }
        if let Some(msg) = wellformed(bytes) {
            report.add(Finding::new(
                FindingCode::XmlNotWellformed,
                Severity::Error,
                name.clone(),
                None,
                format!("XML is not well-formed: {msg}"),
                false,
            ));
        }
    }
}

/// Returns a description of the first well-formedness problem, or `None` when
/// the document is balanced. Never panics on truncated or hostile input.
///
/// The open-tag stack holds byte slices into the input (no per-tag
/// allocation) so the scan stays cheap enough to run on every sheet of a
/// big workbook — validation is meant for pipelines.
pub fn wellformed(xml: &[u8]) -> Option<String> {
    let n = xml.len();
    let mut stack: Vec<&[u8]> = Vec::new();
    let mut i = 0usize;
    while i < n {
        let c = xml[i];
        if c != b'<' {
            i += 1;
            continue;
        }
        match xml.get(i + 1).copied() {
            Some(b'!') => {
                // Comment, CDATA, or a plain declaration.
                if xml[i..].starts_with(b"<!--") {
                    let rel = memchr::memmem::find(&xml[i + 4..], b"-->")?;
                    i = i + 4 + rel + 3;
                } else if xml[i..].starts_with(b"<![CDATA[") {
                    let rel = memchr::memmem::find(&xml[i + 9..], b"]]>")?;
                    i = i + 9 + rel + 3;
                } else {
                    let gt = memchr::memchr(b'>', &xml[i..])?;
                    i = i + gt + 1;
                }
                continue;
            }
            Some(b'?') => {
                // Processing instruction `<?...?>`
                let rel = memchr::memmem::find(&xml[i + 2..], b"?>")?;
                i = i + 2 + rel + 2;
                continue;
            }
            Some(b'/') => {
                // End tag `</...>`
                let gt = tag_end(xml, i)?;
                let name = tag_local_name(&xml[i + 2..gt]);
                if name.is_empty() {
                    return Some("empty end tag name".to_string());
                }
                match stack.pop() {
                    Some(top) if top == name => {}
                    Some(top) => {
                        return Some(format!(
                            "mismatched close tag </{}> (expected </{}>)",
                            utf8(name),
                            utf8(top)
                        ));
                    }
                    None => return Some(format!("close tag </{}> with no open tag", utf8(name))),
                }
                i = gt + 1;
                continue;
            }
            _ => {
                // Open tag
                let gt = tag_end(xml, i)?;
                let tag = &xml[i + 1..gt];
                let name = tag_local_name(tag);
                if name.is_empty() {
                    return Some("empty tag name".to_string());
                }
                let self_close = xml.get(gt.saturating_sub(1)) == Some(&b'/');
                if !self_close {
                    stack.push(name);
                }
                i = gt + 1;
                continue;
            }
        }
    }
    if let Some(top) = stack.last() {
        return Some(format!("unclosed element <{}>", utf8(top)));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn balanced_document_is_ok() {
        let xml = br#"<?xml version="1.0"?><a:b xmlns:a="x"><c d=">"><e/></c></a:b>"#;
        assert_eq!(wellformed(xml), None);
    }

    #[test]
    fn unclosed_element_is_flagged() {
        let xml = b"<worksheet><sheetData><row r=\"1\"><c r=\"A1\"><v>1";
        let err = wellformed(xml).expect("must flag");
        assert!(err.contains("unclosed"), "{err}");
    }

    #[test]
    fn mismatched_close_is_flagged() {
        let err = wellformed(b"<a><b></a>").expect("must flag");
        assert!(err.contains("mismatched"), "{err}");
    }

    #[test]
    fn stray_close_is_flagged() {
        assert!(wellformed(b"</a>").is_some());
    }

    #[test]
    fn comments_and_cdata_are_skipped() {
        let xml = b"<a><!-- comment with <tags> --><![CDATA[ <b> not a tag ]]></a>";
        assert_eq!(wellformed(xml), None);
    }
}
