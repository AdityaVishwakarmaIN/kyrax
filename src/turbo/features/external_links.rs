//! External links (Tier 3 LOW) — multi-workbook cross-references.
//!
//! A formula like `='[1]Sheet1'!A1` or `=[1]!DefinedName` refers to an
//! `xl/externalLinks/externalLinkN.xml` part whose index comes from the
//! workbook's `<externalReferences>` + `xl/_rels/workbook.xml.rels`. Excel stores
//! the referenced cells' CACHED values in that part so the file opens without the
//! other workbook present. We parse the cache (part + its `.rels` Target) and
//! resolve references against it; we never open the other file.
//!
//! The absent case is the common case: workbooks without cross-workbook formulas
//! have no `xl/externalLinks/` entries, and detection costs one central-directory
//! name pass with no inflate.

use crate::turbo::error::{TurboError, TurboResult};
use crate::turbo::zipmin::{list_entries, read_entry};

/// One external workbook referenced by this workbook.
pub struct ExternalBook {
    /// Filename digits (`externalLink3.xml` → `3`). The caller is responsible
    /// for filling it when the index is known; [`parse_external_link`] sets 0.
    pub index: usize,
    /// Resolved `Target` of the `externalLinkPath` relationship, if present.
    pub target: Option<String>,
    /// `<sheetName val=…/>` in order; the position is the `sheetId` used by the cache.
    pub sheet_names: Vec<String>,
    /// `(name, refersTo)` pairs from `<definedName>`.
    pub defined_names: Vec<(String, String)>,
    /// Cached cell values from `<sheetDataSet>`, in document order.
    pub cached: Vec<CachedCell>,
}

/// One cached cell from an external book's `<sheetDataSet>`.
pub struct CachedCell {
    /// The `<sheetData sheetId=…/>` this cell lives under (position in `sheet_names`).
    pub sheet_id: u32,
    /// The cell reference as written (e.g. `A1`).
    pub cell: String,
    /// The cached `<v>` value.
    pub value: String,
    /// The `t` attribute (`str`, `n`, …), if any.
    pub kind: Option<String>,
}

/// Extract the value bytes of attribute `name` from a tag-body slice.
/// Matches ` name="VALUE"`; the leading space disambiguates `id` from `r:id`.
#[inline]
fn attr<'a>(tag: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    let mut buf = [0u8; 48];
    let plen = name.len() + 3;
    let p = if plen <= buf.len() {
        buf[0] = b' ';
        buf[1..1 + name.len()].copy_from_slice(name);
        buf[1 + name.len()] = b'=';
        buf[2 + name.len()] = b'"';
        memchr::memmem::find(tag, &buf[..plen])?
    } else {
        let mut v = Vec::with_capacity(plen);
        v.push(b' ');
        v.extend_from_slice(name);
        v.extend_from_slice(b"=\"");
        memchr::memmem::find(tag, &v)?
    };
    let vs = p + plen;
    let ve = vs + memchr::memchr(b'"', &tag[vs..])?;
    Some(&tag[vs..ve])
}

#[inline]
fn attr_str(scratch: &mut Vec<u8>, tag: &[u8], name: &[u8]) -> Option<String> {
    attr(tag, name).map(|raw| {
        String::from_utf8_lossy(crate::turbo::decode::decode_bytes(raw, scratch)).into_owned()
    })
}

/// True if `xml[pos..]` is an open tag with local name `local` (`<local …>` or
/// `<ns:local …>`), tolerating the XML declaration and comments.
#[inline]
fn is_open(xml: &[u8], pos: usize, local: &[u8]) -> bool {
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

/// Byte offset of the first open tag with local name `local` at or after `from`.
#[inline]
fn find_open_tag(xml: &[u8], from: usize, local: &[u8]) -> Option<usize> {
    let n = xml.len();
    let mut i = from;
    while i < n {
        let Some(o) = memchr::memchr(b'<', &xml[i..n]) else {
            break;
        };
        let pos = i + o;
        if is_open(xml, pos, local) {
            return Some(pos);
        }
        i = pos.saturating_add(1);
    }
    None
}

/// Byte offset of the matching `</local>` / `</ns:local>` closing tag at or after `from`.
#[inline]
fn find_close_tag(xml: &[u8], from: usize, local: &[u8]) -> Option<usize> {
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

/// Parse one external link part into an [`ExternalBook`]. `index` is set to 0;
/// the caller fills it (see [`load_external_books`]).
///
/// Never panics: structural problems degrade to missing fields, and only input
/// that is not XML at all (empty, or no markup) is a [`TurboError::Format`].
pub fn parse_external_link(part: &[u8]) -> TurboResult<ExternalBook> {
    let n = part.len();
    if n == 0 || memchr::memchr(b'<', part).is_none() {
        return Err(TurboError::Format("external link part is not XML".into()));
    }
    let mut book = ExternalBook {
        index: 0,
        target: None,
        sheet_names: Vec::new(),
        defined_names: Vec::new(),
        cached: Vec::new(),
    };
    let mut scratch = Vec::new();
    let mut cur_sheet_id: u32 = 0;
    let mut pos = 0usize;
    while pos < n {
        let Some(o) = memchr::memchr(b'<', &part[pos..n]) else {
            break;
        };
        let start = pos + o;
        if is_open(part, start, b"sheetName") {
            let te = start + memchr::memchr(b'>', &part[start..n]).unwrap_or(n - start);
            if let Some(v) = attr_str(&mut scratch, &part[start..te], b"val") {
                book.sheet_names.push(v);
            }
            pos = te + 1;
        } else if is_open(part, start, b"definedName") {
            let te = start + memchr::memchr(b'>', &part[start..n]).unwrap_or(n - start);
            let tag = &part[start..te];
            let name = attr_str(&mut scratch, tag, b"name").unwrap_or_default();
            let refers = attr_str(&mut scratch, tag, b"refersTo").unwrap_or_default();
            let self_closing = te > start && part[te - 1] == b'/';
            let value = if !refers.is_empty() {
                refers
            } else if self_closing {
                String::new()
            } else if let Some(close) = find_close_tag(part, te + 1, b"definedName") {
                String::from_utf8_lossy(crate::turbo::decode::decode_bytes(
                    &part[te + 1..close],
                    &mut scratch,
                ))
                .into_owned()
            } else {
                String::new()
            };
            book.defined_names.push((name, value));
            pos = te + 1;
        } else if is_open(part, start, b"sheetData") {
            let te = start + memchr::memchr(b'>', &part[start..n]).unwrap_or(n - start);
            if let Some(sid) = attr(&part[start..te], b"sheetId") {
                cur_sheet_id = crate::turbo::decode::atoi(sid).unwrap_or(0);
            }
            pos = te + 1;
        } else if is_open(part, start, b"cell") {
            let te = start + memchr::memchr(b'>', &part[start..n]).unwrap_or(n - start);
            let tag = &part[start..te];
            let cell = attr_str(&mut scratch, tag, b"r").unwrap_or_default();
            let kind = attr_str(&mut scratch, tag, b"t");
            let self_closing = te > start && part[te - 1] == b'/';
            let value = if self_closing {
                String::new()
            } else if let Some(close) = find_close_tag(part, te + 1, b"cell") {
                let inner = &part[te + 1..close];
                match find_open_tag(inner, 0, b"v") {
                    Some(vs) => {
                        let vte =
                            vs + memchr::memchr(b'>', &inner[vs..]).unwrap_or(inner.len() - vs);
                        if vte > vs && inner[vte - 1] == b'/' {
                            String::new()
                        } else if let Some(vc) = find_close_tag(inner, vte + 1, b"v") {
                            String::from_utf8_lossy(crate::turbo::decode::decode_bytes(
                                &inner[vte + 1..vc],
                                &mut scratch,
                            ))
                            .into_owned()
                        } else {
                            String::new()
                        }
                    }
                    None => String::new(),
                }
            } else {
                String::new()
            };
            book.cached.push(CachedCell {
                sheet_id: cur_sheet_id,
                cell,
                value,
                kind,
            });
            pos = te + 1;
        } else {
            pos = start + 1;
        }
    }
    Ok(book)
}

/// The `Target` of the `externalLinkPath` relationship in a part's `.rels`.
///
/// Returns `Ok(None)` when there is no such relationship or the rels XML is
/// empty / malformed; never panics.
pub fn parse_external_link_rel(rels_xml: &[u8]) -> TurboResult<Option<String>> {
    let n = rels_xml.len();
    let mut scratch = Vec::new();
    let mut pos = 0usize;
    while pos < n {
        let Some(o) = memchr::memmem::find(&rels_xml[pos..n], b"<Relationship") else {
            break;
        };
        let start = pos + o;
        let te = start + memchr::memchr(b'>', &rels_xml[start..n]).unwrap_or(n - start);
        let tag = &rels_xml[start..te];
        if let Some(ty) = attr(tag, b"Type") {
            if ty.ends_with(b"/externalLinkPath") {
                let target = attr(tag, b"Target").map(|raw| {
                    String::from_utf8_lossy(crate::turbo::decode::decode_bytes(raw, &mut scratch))
                        .into_owned()
                });
                return Ok(target);
            }
        }
        pos = te + 1;
    }
    Ok(None)
}

/// Filename digits of an `xl/externalLinks/externalLinkN.xml` entry name.
fn external_index_from_name(name: &str) -> usize {
    let rest = name.strip_prefix("xl/externalLinks/").unwrap_or(name);
    let rest = rest.strip_prefix("externalLink").unwrap_or(rest);
    let digits = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
    crate::turbo::decode::atoi(rest[..digits].as_bytes()).unwrap_or(0) as usize
}

/// Rels path for an external link part: `xl/externalLinks/_rels/<file>.rels`.
fn rels_name_for(name: &str) -> String {
    let rest = name.strip_prefix("xl/externalLinks/").unwrap_or(name);
    let mut s = String::with_capacity(name.len() + 8);
    s.push_str("xl/externalLinks/_rels/");
    s.push_str(rest);
    s.push_str(".rels");
    s
}

/// Load every external book from the zip.
///
/// FAST PATH: if no entry name starts with `xl/externalLinks/`, returns
/// `Ok(vec![])` after listing entry names only — nothing is inflated.
///
/// Each present part is parsed with its `.rels`; `index` is set from the
/// filename digits (`externalLink3.xml` → `3`).
pub fn load_external_books(zip_bytes: &[u8]) -> TurboResult<Vec<ExternalBook>> {
    let (entries, _) = list_entries(zip_bytes)?;
    let mut names: Vec<&str> = Vec::new();
    for e in &entries {
        let n = e.name.as_str();
        if n.starts_with("xl/externalLinks/") && n.ends_with(".xml") {
            names.push(n);
        }
    }
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let mut books = Vec::with_capacity(names.len());
    for name in names {
        let index = external_index_from_name(name);
        if let Some(part) = read_entry(zip_bytes, name)? {
            let mut book = parse_external_link(&part)?;
            book.index = index;
            let rels_name = rels_name_for(name);
            if let Some(rels) = read_entry(zip_bytes, &rels_name)? {
                if let Ok(target) = parse_external_link_rel(&rels) {
                    book.target = target;
                }
            }
            books.push(book);
        }
    }
    Ok(books)
}

/// Resolve the CACHED value for an external reference without opening the other
/// workbook. Excel caches these values precisely so the file opens without the
/// other workbook present: we read the cache, we do NOT open the other file.
///
/// `index` matches [`ExternalBook::index`], `sheet` is matched against the
/// book's `sheet_names` (case-insensitive, Excel semantics) to obtain its
/// `sheetId`, and `cell` is matched case-insensitively against the cached cells.
/// Returns `None` when the book, sheet or cell is unknown.
pub fn resolve_reference<'a>(
    books: &'a [ExternalBook],
    index: usize,
    sheet: &str,
    cell: &str,
) -> Option<&'a str> {
    let book = books.iter().find(|b| b.index == index)?;
    let sheet_id = book
        .sheet_names
        .iter()
        .position(|s| s.eq_ignore_ascii_case(sheet))? as u32;
    book.cached
        .iter()
        .find(|c| c.sheet_id == sheet_id && c.cell.eq_ignore_ascii_case(cell))
        .map(|c| c.value.as_str())
}

/// Pull the `[N]` index and optional sheet name out of an external-reference
/// formula fragment like `[1]Sheet1!A1`, `[1]!DefinedName` or `'[1]Sheet1'!A1`.
/// Accepts a leading `=`. The hot path: borrows only, never allocates. Returns
/// `None` when the fragment does not start with a valid `[digits]` external
/// prefix ending in `!`.
pub fn parse_external_ref_prefix(formula: &str) -> Option<(usize, Option<&str>)> {
    let b = formula.as_bytes();
    let n = b.len();
    let mut i = 0;
    if i < n && b[i] == b'=' {
        i += 1;
    }
    let quoted = i < n && b[i] == b'\'';
    if quoted {
        i += 1;
    }
    if i >= n || b[i] != b'[' {
        return None;
    }
    i += 1;
    let dstart = i;
    while i < n && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == dstart || i >= n || b[i] != b']' {
        return None;
    }
    let index = crate::turbo::decode::atoi(&b[dstart..i])? as usize;
    i += 1;
    if i < n && b[i] == b'!' {
        return Some((index, None));
    }
    if quoted {
        let sstart = i;
        while i < n && b[i] != b'\'' {
            i += 1;
        }
        if i >= n {
            return None;
        }
        let sheet = &formula[sstart..i];
        i += 1;
        if i < n && b[i] == b'!' {
            return Some((index, Some(sheet)));
        }
        return None;
    }
    let sstart = i;
    while i < n && b[i] != b'!' {
        i += 1;
    }
    if i >= n {
        return None;
    }
    Some((index, Some(&formula[sstart..i])))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PART: &[u8] = br#"<externalLink xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><externalBook r:id="rId1"><sheetNames><sheetName val="Sheet1"/><sheetName val="2024"/></sheetNames><definedNames><definedName name="Foo" refersTo="Sheet1!$A$1"/></definedNames><sheetDataSet><sheetData sheetId="0"><row r="1"><cell r="A1" t="str"><v>cached</v></cell></row><row r="2"><cell r="A2"><v>42</v></cell></row></sheetData><sheetData sheetId="1"><row r="1"><cell r="B1" t="str"><v>second sheet</v></cell></row></sheetData></sheetDataSet></externalBook></externalLink>"#;

    const RELS: &[u8] = br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/externalLinkPath" Target="../../book2.xlsx" TargetMode="External"/></Relationships>"#;

    /// Build a tiny STORE-method zip with the given (name, payload) entries.
    fn store_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut body = Vec::new();
        let mut cd = Vec::new();
        for (name, payload) in entries {
            let nb = name.as_bytes();
            let off = body.len() as u32;
            body.extend_from_slice(b"PK\x03\x04");
            body.extend_from_slice(&20u16.to_le_bytes());
            body.extend_from_slice(&0u16.to_le_bytes());
            body.extend_from_slice(&0u16.to_le_bytes());
            body.extend_from_slice(&0u16.to_le_bytes());
            body.extend_from_slice(&0u16.to_le_bytes());
            body.extend_from_slice(&0u32.to_le_bytes());
            body.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            body.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            body.extend_from_slice(&(nb.len() as u16).to_le_bytes());
            body.extend_from_slice(&0u16.to_le_bytes());
            body.extend_from_slice(nb);
            body.extend_from_slice(payload);

            cd.extend_from_slice(b"PK\x01\x02");
            cd.extend_from_slice(&20u16.to_le_bytes());
            cd.extend_from_slice(&20u16.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0u32.to_le_bytes());
            cd.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            cd.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            cd.extend_from_slice(&(nb.len() as u16).to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0u16.to_le_bytes());
            cd.extend_from_slice(&0u32.to_le_bytes());
            cd.extend_from_slice(&off.to_le_bytes());
            cd.extend_from_slice(nb);
        }
        let cd_start = body.len() as u32;
        body.extend_from_slice(&cd);
        let cd_size = cd.len() as u32;
        body.extend_from_slice(b"PK\x05\x06");
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        body.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        body.extend_from_slice(&cd_size.to_le_bytes());
        body.extend_from_slice(&cd_start.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body
    }

    #[test]
    fn extlink_parse_happy_path() {
        let book = parse_external_link(PART).unwrap();
        assert_eq!(book.index, 0);
        assert!(book.target.is_none());
        assert_eq!(book.sheet_names, vec!["Sheet1", "2024"]);
        assert_eq!(
            book.defined_names,
            vec![("Foo".to_string(), "Sheet1!$A$1".to_string())]
        );
        assert_eq!(book.cached.len(), 3);
        assert_eq!(book.cached[0].sheet_id, 0);
        assert_eq!(book.cached[0].cell, "A1");
        assert_eq!(book.cached[0].value, "cached");
        assert_eq!(book.cached[0].kind.as_deref(), Some("str"));
        assert_eq!(book.cached[1].sheet_id, 0);
        assert_eq!(book.cached[1].cell, "A2");
        assert_eq!(book.cached[1].value, "42");
        assert_eq!(book.cached[1].kind, None);
        assert_eq!(book.cached[2].sheet_id, 1);
        assert_eq!(book.cached[2].cell, "B1");
        assert_eq!(book.cached[2].value, "second sheet");
    }

    #[test]
    fn extlink_rel_happy_path() {
        let target = parse_external_link_rel(RELS).unwrap();
        assert_eq!(target.as_deref(), Some("../../book2.xlsx"));
    }

    #[test]
    fn extlink_rel_absent_or_unrelated_is_none() {
        assert_eq!(
            parse_external_link_rel(b"<Relationships><Relationship Id=\"rId1\" Type=\"http://x/worksheet\" Target=\"sheet1.xml\"/></Relationships>")
                .unwrap(),
            None
        );
        assert_eq!(parse_external_link_rel(b"not xml at all").unwrap(), None);
        assert_eq!(parse_external_link_rel(b"").unwrap(), None);
    }

    #[test]
    fn extlink_load_external_books_happy() {
        let zip = store_zip(&[
            ("xl/workbook.xml", b"<workbook/>"),
            ("xl/externalLinks/externalLink1.xml", PART),
            ("xl/externalLinks/_rels/externalLink1.xml.rels", RELS),
        ]);
        let books = load_external_books(&zip).unwrap();
        assert_eq!(books.len(), 1);
        let b = &books[0];
        assert_eq!(b.index, 1);
        assert_eq!(b.target.as_deref(), Some("../../book2.xlsx"));
        assert_eq!(b.sheet_names, vec!["Sheet1", "2024"]);
    }

    #[test]
    fn extlink_absent_part_is_empty() {
        let zip = store_zip(&[
            ("xl/workbook.xml", b"<workbook/>"),
            ("xl/sharedStrings.xml", b"<sst/>"),
        ]);
        let books = load_external_books(&zip).unwrap();
        assert!(books.is_empty());
    }

    #[test]
    fn extlink_index_from_filename_digits() {
        let zip = store_zip(&[
            ("xl/externalLinks/externalLink2.xml", PART),
            ("xl/externalLinks/externalLink3.xml", PART),
        ]);
        let books = load_external_books(&zip).unwrap();
        assert_eq!(books.len(), 2);
        assert_eq!(books[0].index, 2);
        assert_eq!(books[1].index, 3);
    }

    #[test]
    fn extlink_resolve_reference_uses_cache() {
        let zip = store_zip(&[
            ("xl/externalLinks/externalLink1.xml", PART),
            ("xl/externalLinks/_rels/externalLink1.xml.rels", RELS),
        ]);
        let books = load_external_books(&zip).unwrap();
        assert_eq!(resolve_reference(&books, 1, "Sheet1", "A1"), Some("cached"));
        assert_eq!(resolve_reference(&books, 1, "Sheet1", "a1"), Some("cached"));
        assert_eq!(
            resolve_reference(&books, 1, "2024", "B1"),
            Some("second sheet")
        );
        assert_eq!(resolve_reference(&books, 1, "Sheet1", "Z99"), None);
        assert_eq!(resolve_reference(&books, 1, "Nope", "A1"), None);
        assert_eq!(resolve_reference(&books, 9, "Sheet1", "A1"), None);
        assert_eq!(resolve_reference(&[], 1, "Sheet1", "A1"), None);
    }

    #[test]
    fn extlink_ref_prefix_hot_path() {
        assert_eq!(
            parse_external_ref_prefix("[1]Sheet1!A1"),
            Some((1, Some("Sheet1")))
        );
        assert_eq!(
            parse_external_ref_prefix("[1]!DefinedName"),
            Some((1, None))
        );
        assert_eq!(
            parse_external_ref_prefix("'[1]Sheet1'!A1"),
            Some((1, Some("Sheet1")))
        );
        assert_eq!(parse_external_ref_prefix("=[1]!Foo"), Some((1, None)));
        assert_eq!(
            parse_external_ref_prefix("[12]My Sheet!B2"),
            Some((12, Some("My Sheet")))
        );
        assert_eq!(
            parse_external_ref_prefix("'[1]2024 Report'!B2"),
            Some((1, Some("2024 Report")))
        );
        assert_eq!(
            parse_external_ref_prefix("[1]Sheet1!$A$1"),
            Some((1, Some("Sheet1")))
        );
        assert_eq!(parse_external_ref_prefix("[abc]Sheet1!A1"), None);
        assert_eq!(parse_external_ref_prefix("[]Sheet1!A1"), None);
        assert_eq!(parse_external_ref_prefix("[1]Sheet1"), None);
        assert_eq!(parse_external_ref_prefix("Sheet1!A1"), None);
        assert_eq!(parse_external_ref_prefix(""), None);
    }

    #[test]
    fn extlink_malformed_part_is_err_or_empty() {
        assert!(parse_external_link(b"").is_err());
        assert!(parse_external_link(b"garbage with no angle bracket").is_err());
        assert!(load_external_books(b"PK\x03\x04garbage that never contains an EOCD").is_err());
    }

    #[test]
    fn extlink_truncated_markup_is_tolerant() {
        let truncated = br#"<externalLink><externalBook><sheetNames><sheetName val="Sheet1"#;
        let book = parse_external_link(truncated).unwrap();
        assert!(book.sheet_names.is_empty());
        assert!(book.cached.is_empty());
        assert!(book.defined_names.is_empty());
    }
}
