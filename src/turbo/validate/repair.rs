//! Conservative, transparent repair.
//!
//! Every repair is recorded as a [`RepairAction`] with a description plus the
//! bytes that were removed or replaced, so a user can audit exactly what
//! changed. Nothing is invented: the only fixes are *dropping* something broken
//! (a dangling rel, an empty-sqref validation, an inverted col, an overlapping
//! merge, a duplicate cell, a #REF! defined name, a content-type override for a
//! missing part) or rewriting an advisory `<dimension>` to match the actual
//! used range. Repair is opt-in per severity via [`super::RepairOptions`], the
//! source file is never modified, and untouched parts are copied back into the
//! new zip with their original compressed bytes.

use std::collections::HashMap;
use std::sync::Arc;

use super::{FindingCode, RepairAction, Severity};
use crate::turbo::error::{TurboError, TurboResult};
use crate::turbo::write::zip::{PrecompressedPart, ZipWriter};
use crate::turbo::zipmin::ZipEntryMeta;

/// What a fix does to one part's bytes.
#[derive(Clone, Debug)]
pub(crate) enum FixOp {
    /// Remove the first `<name attr="value">` element.
    RemoveElement {
        name: String,
        attr: String,
        value: String,
    },
    /// Remove the last `<name attr="value">` element.
    RemoveElementFromEnd {
        name: String,
        attr: String,
        value: String,
    },
    /// Remove every `<name>` element whose `attr` is missing or empty.
    RemoveElementWhere { name: String, attr: String },
    /// Replace the value of `attr` on the first `<name>` element.
    SetAttrValue {
        name: String,
        attr: String,
        value: String,
    },
}

/// One candidate repair, produced by the checkers.
#[derive(Clone, Debug)]
pub(crate) struct Fix {
    pub code: FindingCode,
    pub severity: Severity,
    pub part: String,
    pub description: String,
    pub op: FixOp,
}

impl Fix {
    /// Apply the fix to the in-memory parts. Always records a [`RepairAction`]
    /// so the user sees the attempt (with before/after) even if the target
    /// element was not found.
    pub fn apply(&self, parts: &mut HashMap<String, Vec<u8>>) -> RepairAction {
        let (changed, before, after) = match parts.get_mut(&self.part) {
            Some(b) => match &self.op {
                FixOp::RemoveElement { name, attr, value } => {
                    remove_element(b, name.as_bytes(), attr.as_bytes(), value.as_bytes())
                }
                FixOp::RemoveElementFromEnd { name, attr, value } => {
                    remove_element_from_end(b, name.as_bytes(), attr.as_bytes(), value.as_bytes())
                }
                FixOp::RemoveElementWhere { name, attr } => {
                    remove_elements_where(b, name.as_bytes(), attr.as_bytes())
                }
                FixOp::SetAttrValue { name, attr, value } => {
                    set_attr_value(b, name.as_bytes(), attr.as_bytes(), value.as_bytes())
                }
            },
            None => (false, String::new(), String::new()),
        };
        RepairAction {
            code: self.code,
            severity: self.severity,
            part: self.part.clone(),
            description: if changed {
                self.description.clone()
            } else {
                format!("{} (target element not found)", self.description)
            },
            before,
            after,
        }
    }
}

/// Rebuild the archive: untouched parts keep their exact compressed bytes and
/// order; fixed parts are recompressed from the edited bytes.
pub(crate) fn rewrite_zip(
    zip: &Arc<Vec<u8>>,
    entries: &[ZipEntryMeta],
    parts: &mut HashMap<String, Vec<u8>>,
    fixed_parts: &[String],
) -> TurboResult<Vec<u8>> {
    let mut w = ZipWriter::new();
    for meta in entries {
        if fixed_parts.iter().any(|p| p == &meta.name) {
            let data = parts.get(&meta.name).cloned().unwrap_or_default();
            w.add_buf(&meta.name, data);
        } else {
            let start = meta.data_offset as usize;
            let end = start.saturating_add(meta.compressed_size as usize);
            let comp = if end <= zip.len() {
                &zip[start..end]
            } else {
                &[]
            };
            w.add_precompressed(PrecompressedPart {
                name: meta.name.clone(),
                method: meta.compression_method,
                crc32: meta.crc32,
                uncomp_size: meta.uncompressed_size,
                data: comp.to_vec(),
            });
        }
    }
    w.finish().map_err(TurboError::Io)
}

// ----------------------------------------------------------------------------
// Byte-level element editing (validate-local; mutate.rs is owned elsewhere).
// ----------------------------------------------------------------------------

fn snippet(b: &[u8]) -> String {
    let s = super::utf8(b);
    if s.chars().count() <= 80 {
        s
    } else {
        s.chars().take(80).collect::<String>() + "\u{2026}"
    }
}

fn element_span(hay: &[u8], s: usize, gt: usize, name: &[u8]) -> (usize, usize) {
    let self_close = hay.get(gt.saturating_sub(1)) == Some(&b'/');
    if self_close {
        (s, gt + 1)
    } else {
        let mut close = Vec::with_capacity(name.len() + 3);
        close.extend_from_slice(b"</");
        close.extend_from_slice(name);
        close.push(b'>');
        match memchr::memmem::find(&hay[gt + 1..], &close) {
            Some(c) => (s, gt + 1 + c + close.len()),
            None => (s, hay.len()),
        }
    }
}

/// Remove the first `<name attr="value">` element.
fn remove_element(
    hay: &mut Vec<u8>,
    name: &[u8],
    attr_name: &[u8],
    value: &[u8],
) -> (bool, String, String) {
    let mut pos = 0usize;
    while let Some(o) = memchr::memmem::find(&hay[pos..], b"<") {
        let s = pos + o;
        let Some(gt) = super::tag_end(hay, s) else {
            break;
        };
        let tag = &hay[s + 1..gt];
        if super::tag_local_name(tag) != name {
            pos = gt + 1;
            continue;
        }
        let Some(v) = super::attr(tag, attr_name) else {
            pos = gt + 1;
            continue;
        };
        if v != value {
            pos = gt + 1;
            continue;
        }
        let (es, ee) = element_span(hay, s, gt, name);
        let before = snippet(&hay[es..ee]);
        hay.drain(es..ee);
        return (true, before, String::new());
    }
    (false, String::new(), String::new())
}

/// Remove the LAST `<name attr="value">` element (the duplicate-cell case keeps
/// the first occurrence).
fn remove_element_from_end(
    hay: &mut Vec<u8>,
    name: &[u8],
    attr_name: &[u8],
    value: &[u8],
) -> (bool, String, String) {
    let mut last: Option<(usize, usize)> = None;
    let mut pos = 0usize;
    while let Some(o) = memchr::memmem::find(&hay[pos..], b"<") {
        let s = pos + o;
        let Some(gt) = super::tag_end(hay, s) else {
            break;
        };
        let tag = &hay[s + 1..gt];
        if super::tag_local_name(tag) != name {
            pos = gt + 1;
            continue;
        }
        let Some(v) = super::attr(tag, attr_name) else {
            pos = gt + 1;
            continue;
        };
        if v != value {
            pos = gt + 1;
            continue;
        }
        let (es, ee) = element_span(hay, s, gt, name);
        last = Some((es, ee));
        pos = ee;
    }
    if let Some((es, ee)) = last {
        let before = snippet(&hay[es..ee]);
        hay.drain(es..ee);
        (true, before, String::new())
    } else {
        (false, String::new(), String::new())
    }
}

/// Remove every `<name>` element whose `attr` is missing or empty (e.g. an
/// empty `sqref` on a dataValidation).
fn remove_elements_where(
    hay: &mut Vec<u8>,
    name: &[u8],
    attr_name: &[u8],
) -> (bool, String, String) {
    let mut removed = 0usize;
    let mut befores: Vec<String> = Vec::new();
    let mut pos = 0usize;
    while pos < hay.len() {
        let Some(o) = memchr::memmem::find(&hay[pos..], b"<") else {
            break;
        };
        let s = pos + o;
        let Some(gt) = super::tag_end(hay, s) else {
            break;
        };
        let tag = &hay[s + 1..gt];
        let local = super::tag_local_name(tag);
        if local == name {
            let empty = match super::attr(tag, attr_name) {
                Some(v) => v.iter().all(|b| b.is_ascii_whitespace()),
                None => true,
            };
            if empty {
                let (es, ee) = element_span(hay, s, gt, name);
                befores.push(snippet(&hay[es..ee]));
                hay.drain(es..ee);
                removed += 1;
                continue;
            }
        }
        pos = gt + 1;
    }
    if removed > 0 {
        (true, befores.join("; "), String::new())
    } else {
        (false, String::new(), String::new())
    }
}

/// Replace the value of `attr` on the first `<name>` element.
fn set_attr_value(
    hay: &mut Vec<u8>,
    name: &[u8],
    attr_name: &[u8],
    value: &[u8],
) -> (bool, String, String) {
    let mut pos = 0usize;
    while let Some(o) = memchr::memmem::find(&hay[pos..], b"<") {
        let s = pos + o;
        let Some(gt) = super::tag_end(hay, s) else {
            break;
        };
        let tag = &hay[s + 1..gt];
        if super::tag_local_name(tag) != name {
            pos = gt + 1;
            continue;
        }
        let mut needle = Vec::with_capacity(attr_name.len() + 3);
        needle.extend_from_slice(attr_name);
        needle.extend_from_slice(b"=\"");
        let Some(ao) = memchr::memmem::find(tag, &needle) else {
            pos = gt + 1;
            continue;
        };
        let vs = s + 1 + ao + needle.len();
        let Some(ve_rel) = memchr::memchr(b'"', &hay[vs..]) else {
            pos = gt + 1;
            continue;
        };
        let ve = vs + ve_rel;
        let before = super::utf8(&hay[vs..ve]);
        hay.splice(vs..ve, value.iter().copied());
        return (true, before, super::utf8(value));
    }
    (false, String::new(), String::new())
}
