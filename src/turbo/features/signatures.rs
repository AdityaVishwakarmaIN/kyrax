//! Digital signatures: detect, report, and correctly invalidate (Tier 3 LOW).
//!
//! OOXML workbooks can carry an XML digital signature: the `_xmlsignatures/`
//! folder holds the `sigN.xml` signature documents plus `origin.sigs` (a
//! relationships part), wired from `_rels/.rels` through a relationship whose
//! Type ends in `/digital-signature/origin`.
//!
//! We do not verify cryptograms — that is the caller's security decision — but
//! we do two things reliably and cheaply:
//!
//! * **detect without paying for it.** The absent case (no entry name starting
//!   with `_xmlsignatures/`) is the common case, and it costs a single
//!   central-directory listing and zero inflates. Only when a signature part
//!   actually exists do we inflate that part alone.
//! * **invalidate on edit.** A workbook we modify MUST lose its signature.
//!   [`signature_part_names`] returns the parts the edit path must *drop*, and
//!   [`strip_signature_rels`] removes the origin relationship from `_rels/.rels`.
//!   A stale signature is never carried forward: Excel presents an edited file
//!   with a now-invalid signature as *tampered*, which is strictly worse than
//!   an honest unsigned file.
//!
//! All parsing is hand-rolled byte scanning (memchr); nothing allocates in an
//! inner loop and malformed XML degrades to `None`/empty, never a panic.

use crate::turbo::error::{TurboError, TurboResult};
use crate::turbo::zipmin;

/// One detected signature part.
#[derive(Clone, Debug)]
pub struct SignatureInfo {
    /// Zip entry name, e.g. `_xmlsignatures/sig1.xml`.
    pub part_name: String,
    /// Value of the `<mdssi:SignatureTime><mdssi:Value>` element, if present.
    pub signed_at: Option<String>,
    /// Subject of the `<X509SubjectName>` element (best-effort signer hint).
    pub signer_hint: Option<String>,
}

/// Is the workbook digitally signed? Cheap entry-name-only check: the
/// signature parts live under `_xmlsignatures/`, so presence is decided by a
/// central-directory listing with no inflate at all.
pub fn is_signed(zip_bytes: &[u8]) -> TurboResult<bool> {
    let (entries, _) = zipmin::list_entries(zip_bytes)?;
    Ok(entries
        .iter()
        .any(|e| e.name.starts_with("_xmlsignatures/")))
}

/// Detect every signature part and report best-effort metadata.
///
/// Fast path: when no entry name starts with `_xmlsignatures/`, returns
/// `Ok(vec![])` after only listing entry names — nothing is inflated in the
/// common unsigned case. When signatures exist, only those parts are inflated
/// and parsed; a corrupt signature part is skipped (best-effort) rather than
/// failing the call.
pub fn detect_signatures(zip_bytes: &[u8]) -> TurboResult<Vec<SignatureInfo>> {
    let (entries, _) = zipmin::list_entries(zip_bytes)?;
    let mut out = Vec::new();
    for e in entries {
        if !e.name.starts_with("_xmlsignatures/") {
            continue;
        }
        // origin.sigs is a relationships part that *lists* the signature
        // documents; it is not itself a Signature element.
        if e.name.ends_with(".sigs") {
            continue;
        }
        let Ok(xml) = zipmin::inflate_entry(zip_bytes, &e) else {
            continue; // best-effort: a broken signature part is skipped, not fatal
        };
        let (signed_at, signer_hint) = scan_sig_meta(&xml);
        out.push(SignatureInfo {
            part_name: e.name,
            signed_at,
            signer_hint,
        });
    }
    Ok(out)
}

/// Every part the edit path must **drop** when the workbook is modified.
///
/// This is deliberately a DROP list, not a preserve list. A signature is a
/// cryptographic binding over the bytes of the parts it covers; once those
/// bytes change, the binding is broken. Carrying a now-invalid signature
/// forward is worse than dropping it, because Excel then flags the file as
/// tampered. Dropping the `_xmlsignatures/` entries (and removing the origin
/// relationship via [`strip_signature_rels`]) turns the edited file back into
/// an honest, cleanly unsigned workbook.
pub fn signature_part_names(zip_bytes: &[u8]) -> TurboResult<Vec<String>> {
    let (entries, _) = zipmin::list_entries(zip_bytes)?;
    Ok(entries
        .into_iter()
        .filter(|e| e.name.starts_with("_xmlsignatures/"))
        .map(|e| e.name)
        .collect())
}

/// Remove every signature-related `<Relationship>` from a rels part (the caller
/// passes the inflated `_rels/.rels`): the digital-signature **origin**
/// relationship (Type ends in `/digital-signature/origin`) and any relationship
/// whose Target points into `_xmlsignatures/` (which would otherwise dangle once
/// the signature parts are dropped). Every other byte is preserved exactly.
///
/// When there are no such relationships the input is returned unchanged
/// (a content-identical copy). Callers rely on that byte-for-byte equality to
/// skip rewriting the part, so this function must never reorder or reformat
/// the XML — it only excises whole relationship elements.
pub fn strip_signature_rels(rels_xml: &[u8]) -> TurboResult<Vec<u8>> {
    let mut removes: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    while i < rels_xml.len() {
        let Some(o) = memchr::memchr(b'<', &rels_xml[i..]) else {
            break;
        };
        let start = i + o;
        if !is_rel_open(&rels_xml[start + 1..]) {
            i = start + 1;
            continue;
        }
        let te = start
            + memchr::memchr(b'>', &rels_xml[start..]).ok_or_else(|| {
                TurboError::Format("unterminated <Relationship> tag in .rels".into())
            })?;
        let close_end = if rels_xml.get(te.wrapping_sub(1)) == Some(&b'/') {
            te + 1
        } else {
            find_close_rel(rels_xml, te + 1).ok_or_else(|| {
                TurboError::Format("unterminated <Relationship> element in .rels".into())
            })?
        };
        if rel_is_signature_related(&rels_xml[start..te]) {
            removes.push((start, close_end));
        }
        i = close_end;
    }
    if removes.is_empty() {
        return Ok(rels_xml.to_vec());
    }
    let mut out = Vec::with_capacity(rels_xml.len());
    let mut prev = 0usize;
    for (s, e) in removes {
        out.extend_from_slice(&rels_xml[prev..s]);
        prev = e;
    }
    out.extend_from_slice(&rels_xml[prev..]);
    Ok(out)
}

/// Remove every signature-related declaration from a `[Content_Types].xml` part
/// (the caller passes the inflated content types): `<Override>` elements whose
/// `PartName` lives under `/_xmlsignatures/`, and `<Default>` elements for the
/// `sigs` extension. Every other byte is preserved exactly.
///
/// Identity contract: when no signature declarations are present the input is
/// returned unchanged, so callers can skip rewriting the part. This function
/// never reorders or reformats the XML — it only excises whole declarations.
pub fn strip_signature_content_types(ct_xml: &[u8]) -> TurboResult<Vec<u8>> {
    let mut removes: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    while i < ct_xml.len() {
        let Some(o) = memchr::memchr(b'<', &ct_xml[i..]) else {
            break;
        };
        let start = i + o;
        if !is_ct_open(&ct_xml[start + 1..]) {
            i = start + 1;
            continue;
        }
        let te = start
            + memchr::memchr(b'>', &ct_xml[start..]).ok_or_else(|| {
                TurboError::Format("unterminated declaration tag in [Content_Types].xml".into())
            })?;
        // Content types only carry self-closing declarations; refuse an unclosed
        // element rather than guessing at its extent.
        let close_end = if ct_xml.get(te.wrapping_sub(1)) == Some(&b'/') {
            te + 1
        } else {
            return Err(TurboError::Format(
                "unterminated declaration element in [Content_Types].xml".into(),
            ));
        };
        if ct_decl_is_signature(&ct_xml[start..te]) {
            removes.push((start, close_end));
        }
        i = close_end;
    }
    if removes.is_empty() {
        return Ok(ct_xml.to_vec());
    }
    let mut out = Vec::with_capacity(ct_xml.len());
    let mut prev = 0usize;
    for (s, e) in removes {
        out.extend_from_slice(&ct_xml[prev..s]);
        prev = e;
    }
    out.extend_from_slice(&ct_xml[prev..]);
    Ok(out)
}

// ----------------------------------------------------------------------------
// Hand-rolled byte scanning (no regex, no quick-xml, no allocation in loops).
// ----------------------------------------------------------------------------

/// Local (post-colon) name of the tag that starts right after `rest` (i.e.
/// after a `<` for opens or a `</` for closes). Namespace prefixes are ignored.
#[inline]
fn tag_local_name(rest: &[u8]) -> &[u8] {
    let name_end = rest
        .iter()
        .position(|&b| {
            b == b'>' || b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' || b == b'/'
        })
        .unwrap_or(rest.len());
    let name = &rest[..name_end];
    match memchr::memchr(b':', name) {
        Some(c) => &name[c + 1..],
        None => name,
    }
}

/// `rest` is everything after a `<`. True when it opens an element whose local
/// name is `local` (never a close tag, declaration, or comment).
#[inline]
fn is_open_local(rest: &[u8], local: &[u8]) -> bool {
    match rest.first() {
        Some(b'/' | b'?' | b'!') => false,
        _ => tag_local_name(rest) == local,
    }
}

/// `rest` is everything after a `<`. True when it opens a `Relationship`
/// element (namespace-prefix tolerant).
#[inline]
fn is_rel_open(rest: &[u8]) -> bool {
    match rest.first() {
        Some(b'/' | b'?' | b'!') => false,
        _ => tag_local_name(rest) == b"Relationship",
    }
}

/// End offset (after the closing `>`) of the `</Relationship>` element whose
/// open tag ended at `from`. `None` when malformed — never panics.
fn find_close_rel(xml: &[u8], from: usize) -> Option<usize> {
    let mut j = from;
    while j < xml.len() {
        let o = memchr::memmem::find(&xml[j..], b"</")?;
        let cpos = j + o;
        if tag_local_name(&xml[cpos + 2..]) == b"Relationship" {
            return Some(cpos + memchr::memchr(b'>', &xml[cpos..])? + 1);
        }
        j = cpos + 2;
    }
    None
}

/// Is this `<Relationship>` open tag signature-related? Yes when its Type is the
/// digital-signature origin, or when its Target points into `_xmlsignatures/`
/// (covers the per-signature relationships that would dangle after the drop).
#[inline]
fn rel_is_signature_related(tag: &[u8]) -> bool {
    if crate::turbo::structural::find_attr(tag, b"Type")
        .is_some_and(|t| t.ends_with(b"/digital-signature/origin"))
    {
        return true;
    }
    crate::turbo::structural::find_attr(tag, b"Target")
        .is_some_and(|t| t.starts_with(b"_xmlsignatures/"))
}

/// `rest` is everything after a `<`. True for a `<Default` or `<Override`
/// declaration open tag (never a close tag, processing instruction, or comment).
#[inline]
fn is_ct_open(rest: &[u8]) -> bool {
    match rest.first() {
        Some(b'/' | b'?' | b'!') => false,
        _ => {
            let local = tag_local_name(rest);
            local == b"Default" || local == b"Override"
        }
    }
}

/// Is this a signature-related content-type declaration? An Override whose
/// PartName lives under `/_xmlsignatures/`, or a Default for the `.sigs`
/// extension.
#[inline]
fn ct_decl_is_signature(tag: &[u8]) -> bool {
    if crate::turbo::structural::find_attr(tag, b"PartName")
        .is_some_and(|p| p.starts_with(b"/_xmlsignatures/"))
    {
        return true;
    }
    crate::turbo::structural::find_attr(tag, b"Extension")
        .is_some_and(|e| e.eq_ignore_ascii_case(b"sigs"))
}

/// Span of the first element whose local tag name is `local`, searching from
/// `from`. Returns `(open_gt, close)` where `open_gt` is the position of the
/// `>` ending the open tag and `close` is the position of the `<` of the
/// matching closing tag. `None` for absent or malformed XML — never panics.
fn element_span(xml: &[u8], from: usize, local: &[u8]) -> Option<(usize, usize)> {
    let mut i = from;
    while i < xml.len() {
        let pos = i + memchr::memchr(b'<', &xml[i..])?;
        if is_open_local(&xml[pos + 1..], local) {
            let open_gt = pos + memchr::memchr(b'>', &xml[pos..])?;
            let mut j = open_gt + 1;
            while j < xml.len() {
                let o = memchr::memmem::find(&xml[j..], b"</")?;
                let cpos = j + o;
                if tag_local_name(&xml[cpos + 2..]) == local {
                    return Some((open_gt, cpos));
                }
                j = cpos + 2;
            }
            return None;
        }
        i = pos + 1;
    }
    None
}

/// Text content of the first element with local name `local`, searched from
/// `from`. Best-effort: `None` for absent or malformed input.
fn element_text(xml: &[u8], from: usize, local: &[u8]) -> Option<String> {
    let (open_gt, close) = element_span(xml, from, local)?;
    Some(String::from_utf8_lossy(&xml[open_gt + 1..close]).into_owned())
}

/// Best-effort metadata extraction: signature timestamp and signer subject.
/// Both degrade to `None`; this never errors and never panics.
fn scan_sig_meta(xml: &[u8]) -> (Option<String>, Option<String>) {
    let signed_at = element_span(xml, 0, b"SignatureTime")
        .and_then(|(open_gt, _)| element_text(xml, open_gt + 1, b"Value"))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let signer_hint = element_text(xml, 0, b"X509SubjectName")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    (signed_at, signer_hint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    /// Minimal single- or multi-entry STORE zip built fully in memory. The
    /// reader never verifies CRC, so that field stays zero. `method` lets a
    /// test plant a deflate(8) entry with garbage bytes to prove that no
    /// function inflates what it does not need to.
    fn zip_with_entries(entries: &[(&str, u16, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut cds: Vec<(u32, &str, u16, usize)> = Vec::new();
        for (name, method, payload) in entries {
            let lh = out.len() as u32;
            out.extend_from_slice(b"PK\x03\x04");
            out.extend_from_slice(&[20, 0]); // version needed
            out.extend_from_slice(&[0, 0]); // general purpose flags
            out.extend_from_slice(&method.to_le_bytes()); // compression method
            out.extend_from_slice(&[0, 0]); // mod time
            out.extend_from_slice(&[0, 0]); // mod date
            out.extend_from_slice(&[0, 0, 0, 0]); // crc-32 (not verified)
            out.extend_from_slice(&(payload.len() as u32).to_le_bytes()); // csize
            out.extend_from_slice(&(payload.len() as u32).to_le_bytes()); // usize
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&[0, 0]); // extra len
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(payload);
            cds.push((lh, name, *method, payload.len()));
        }
        let cd_start = out.len() as u32;
        for (lh, name, method, plen) in cds {
            out.extend_from_slice(b"PK\x01\x02");
            out.extend_from_slice(&[20, 0]); // version made by
            out.extend_from_slice(&[20, 0]); // version needed
            out.extend_from_slice(&[0, 0]); // flags
            out.extend_from_slice(&method.to_le_bytes());
            out.extend_from_slice(&[0, 0]); // mod time
            out.extend_from_slice(&[0, 0]); // mod date
            out.extend_from_slice(&[0, 0, 0, 0]); // crc
            out.extend_from_slice(&(plen as u32).to_le_bytes()); // csize
            out.extend_from_slice(&(plen as u32).to_le_bytes()); // usize
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&[0, 0]); // extra len
            out.extend_from_slice(&[0, 0]); // comment len
            out.extend_from_slice(&[0, 0]); // disk number start
            out.extend_from_slice(&[0, 0]); // internal attrs
            out.extend_from_slice(&[0, 0, 0, 0]); // external attrs
            out.extend_from_slice(&lh.to_le_bytes()); // local header offset
            out.extend_from_slice(name.as_bytes());
        }
        let cd_size = out.len() as u32 - cd_start;
        out.extend_from_slice(b"PK\x05\x06");
        out.extend_from_slice(&[0, 0]); // disk
        out.extend_from_slice(&[0, 0]); // cd disk
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_start.to_le_bytes());
        out.extend_from_slice(&[0, 0]); // comment len
        out
    }

    const SIG_XML: &[u8] = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><Signature xmlns=\"http://www.w3.org/2000/09/xmldsig#\"><SignedInfo><SignatureMethod Algorithm=\"http://www.w3.org/2001/04/xmldsig-more#rsa-sha256\"/></SignedInfo><SignatureValue>dGVzdA==</SignatureValue><Object><mdssi:SignatureTime xmlns:mdssi=\"http://schemas.openxmlformats.org/package/2006/digital-signature\"><mdssi:Value>2024-01-15T10:30:00Z</mdssi:Value></mdssi:SignatureTime></Object><KeyInfo><X509Data><X509Certificate>QUJD</X509Certificate><X509SubjectName>CN=Test Signer, O=ACME</X509SubjectName></X509Data></KeyInfo></Signature>";

    #[test]
    fn sig_detects_signed_workbook() {
        let zip = zip_with_entries(&[
            ("[Content_Types].xml", 0, b"<Types/>"),
            ("_rels/.rels", 0, b"<Relationships/>"),
            ("_xmlsignatures/origin.sigs", 0, b"<Relationships/>"),
            ("_xmlsignatures/sig1.xml", 0, SIG_XML),
        ]);
        assert!(is_signed(&zip).unwrap());
        let sigs = detect_signatures(&zip).unwrap();
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0].part_name, "_xmlsignatures/sig1.xml");
        assert_eq!(sigs[0].signed_at.as_deref(), Some("2024-01-15T10:30:00Z"));
        assert_eq!(
            sigs[0].signer_hint.as_deref(),
            Some("CN=Test Signer, O=ACME")
        );
        assert_eq!(
            signature_part_names(&zip).unwrap(),
            vec!["_xmlsignatures/origin.sigs", "_xmlsignatures/sig1.xml"]
        );
    }

    #[test]
    fn sig_unsigned_workbook_is_empty_and_costs_no_inflate() {
        // The workbook parts claim deflate(8) but their payloads are garbage:
        // if any function inflated a part it would surface an Inflate error.
        // All three entry-name-only functions must instead succeed with
        // empty/false, proving the unsigned path never inflates anything.
        let zip = zip_with_entries(&[
            ("[Content_Types].xml", 0, b"<Types/>"),
            ("xl/workbook.xml", 8, b"\x00"),
            ("xl/sharedStrings.xml", 8, b"\xff\xff"),
        ]);
        assert!(!is_signed(&zip).unwrap());
        assert!(detect_signatures(&zip).unwrap().is_empty());
        assert!(signature_part_names(&zip).unwrap().is_empty());
    }

    #[test]
    fn sig_corrupt_sig_part_is_skipped_not_panic() {
        // A deflate(8) signature part with garbage bytes: detection stays true
        // but the corrupt part is skipped best-effort instead of crashing.
        let zip = zip_with_entries(&[
            ("_xmlsignatures/origin.sigs", 0, b"<Relationships/>"),
            ("_xmlsignatures/sig1.xml", 8, b"\x00"),
        ]);
        assert!(is_signed(&zip).unwrap());
        let sigs = detect_signatures(&zip).unwrap();
        assert!(
            sigs.iter()
                .all(|s| s.signed_at.is_none() && s.signer_hint.is_none())
        );
    }

    #[test]
    fn sig_truncated_sig_xml_yields_empty_meta() {
        // Unclosed <mdssi:SignatureTime>: best-effort yields None, no panic.
        let xml =
            b"<Signature xmlns=\"http://www.w3.org/2000/09/xmldsig#\"><mdssi:SignatureTime><mdssi:Value>2024-01-15T10:30:00Z";
        let zip = zip_with_entries(&[("_xmlsignatures/sig1.xml", 0, xml)]);
        let sigs = detect_signatures(&zip).unwrap();
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0].signed_at, None);
        assert_eq!(sigs[0].signer_hint, None);
    }

    #[test]
    fn sig_strip_signature_rels_removes_origin_only() {
        let head = b"<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">";
        let r1 = b"<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/>";
        let r2 = b"<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/>";
        let rsig = b"<Relationship Id=\"rId3\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/digital-signature/origin\" Target=\"_xmlsignatures/origin.sigs\"/>";
        let r4 = b"<Relationship Id=\"rId4\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/digital-signature/signature\" Target=\"_xmlsignatures/sig1.xml\"/>";
        let tail = b"</Relationships>";
        // Slices, not arrays: byte-string literals of different lengths are
        // different types, so `[head, r1, ..]` will not build an array.
        let mut input = Vec::new();
        for seg in [&head[..], r1, r2, rsig, r4, tail] {
            input.extend_from_slice(seg);
        }
        // Both the origin rel AND the per-signature rel (Target into
        // _xmlsignatures/) are excised; everything else is byte-identical.
        let mut expected = Vec::new();
        for seg in [&head[..], r1, r2, tail] {
            expected.extend_from_slice(seg);
        }
        let out = strip_signature_rels(&input).unwrap();
        assert_eq!(out, expected);
    }

    #[test]
    fn sig_strip_rels_unchanged_when_no_signature_rel() {
        let rels = b"<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>";
        let out = strip_signature_rels(rels).unwrap();
        assert_eq!(out, rels);
    }

    #[test]
    fn sig_strip_rels_unterminated_tag_is_error() {
        // An open <Relationship> with no close: Format error, not a panic.
        let rels = b"<Relationships><Relationship Id=\"rId3\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/digital-signature/origin\" Target=\"_xmlsignatures/origin.sigs\">";
        assert!(strip_signature_rels(rels).is_err());
    }

    #[test]
    fn sig_detect_on_non_zip_is_error() {
        assert!(is_signed(b"not a zip archive").is_err());
        assert!(detect_signatures(b"PK\x03\x04too short").is_err());
        assert!(signature_part_names(b"").is_err());
    }

    #[test]
    fn sig_strip_content_types_removes_signature_decls() {
        let head = b"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">";
        let d_plain = b"<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>";
        let o_wb = b"<Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/>";
        let o_sig = b"<Override PartName=\"/_xmlsignatures/sig1.xml\" ContentType=\"application/vnd.openxmlformats-package.digital-signature-xml\"/>";
        let d_sigs = b"<Default Extension=\"sigs\" ContentType=\"application/vnd.openxmlformats-package.digital-signature-origin\"/>";
        let tail = b"</Types>";
        let mut input = Vec::new();
        for seg in [&head[..], d_plain, o_wb, o_sig, d_sigs, tail] {
            input.extend_from_slice(seg);
        }
        let mut expected = Vec::new();
        for seg in [&head[..], d_plain, o_wb, tail] {
            expected.extend_from_slice(seg);
        }
        let out = strip_signature_content_types(&input).unwrap();
        assert_eq!(out, expected);
    }

    #[test]
    fn sig_strip_content_types_unchanged_when_no_signature() {
        let ct = b"<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/></Types>";
        let out = strip_signature_content_types(ct).unwrap();
        assert_eq!(out, ct);
    }

    #[test]
    fn sig_strip_content_types_unterminated_is_error() {
        let ct = b"<Types><Default Extension=\"sigs\" ContentType=\"application/vnd.openxmlformats-package.digital-signature-origin\">";
        assert!(strip_signature_content_types(ct).is_err());
    }
}
