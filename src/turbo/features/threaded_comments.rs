//! Threaded comments — the WRITE half of the round trip (Tier 3 MEDIUM).
//!
//! kyrax already reads `xl/threadedComments/threadedCommentN.xml` and the
//! `xl/persons/person.xml` author list (see `turbo/structural.rs`); openpyxl
//! cannot read them at all. Until now nothing could write them, so a
//! read-modify-write pass silently dropped every thread. This module closes
//! that: it parses and emits both parts byte-deterministically, so a
//! parsed-and-rewritten workbook survives the round trip with its threads and
//! authors intact.
//!
//! The parts are Office 2018 threaded comments:
//!   <ThreadedComments xmlns=".../2018/threadedcomments">
//!     <threadedComment ref="B2" dT="..." personId="{guid}" id="{c1}"
//!                      parentId="{c0}"><text>body</text></threadedComment>
//!   </ThreadedComments>
//!   <personList xmlns=".../2018/threadedcomments">
//!     <person displayName="Alice" id="{guid}"/>
//!   </personList>
//!
//! Parsing is the same hand-rolled memchr scan as structural.rs (no quick-xml,
//! no new dependency), O(size of the part handed in). The absent case is the
//! fast path: an empty slice returns an empty Vec immediately. Writing is a
//! single pass over the input slice with a fixed attribute order — same input
//! always yields byte-identical output, so rewriting a workbook is stable.
//! The legacy `<comment>` fallback part (`xl/comments1.xml`) is out of scope.

use crate::turbo::error::{TurboError, TurboResult};

/// One threaded comment, ready for round-tripping through both parts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadedComment {
    /// A1-style cell reference, e.g. "B2".
    pub cell: String,
    /// Comment body (plain text; the `<text>` child, entity-decoded).
    pub text: String,
    /// `personId` — the author, matching a [`Person::id`] in the persons part.
    pub author_id: String,
    /// `dT` datetime string, verbatim when present.
    pub created: Option<String>,
    /// `id` — the GUID identifying this comment (parents reference it).
    pub id: String,
    /// `parentId` — the GUID of the comment this replies to (`None` = root).
    pub parent_id: Option<String>,
}

/// One author in `xl/persons/person.xml`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Person {
    pub id: String,
    pub display_name: String,
}

// ----------------------------------------------------------------------------
// XML scanning helpers (mirror of structural.rs; private to this module).
// ----------------------------------------------------------------------------

/// True if `xml[pos..]` is an open tag with local name `local` (`<local …>`
/// or `<ns:local …>`).
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

/// Value of attribute `name` in a tag-body slice (`<tag …>`), matched as
/// ` name="VALUE"` (the leading space disambiguates `id` from `personId`).
#[inline]
fn find_attr<'a>(tag: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    let plen = name.len() + 3;
    let p = if plen <= 48 {
        let mut buf = [0u8; 48];
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

/// Decode XML character references into `out`, leniently. Handles the five
/// predefined entities and `&#N;` / `&#xH;` numeric references; anything else
/// (unknown name, unterminated `&`, out-of-range code point) is copied
/// literally so malformed parts degrade instead of failing.
fn decode_escaped(src: &[u8], out: &mut Vec<u8>) {
    let n = src.len();
    let mut i = 0usize;
    while i < n {
        let b = src[i];
        if b != b'&' {
            out.push(b);
            i += 1;
            continue;
        }
        let Some(rel) = memchr::memchr(b';', &src[i..n]) else {
            out.extend_from_slice(&src[i..]);
            break;
        };
        let semi = i + rel;
        let ent = &src[i + 1..semi];
        let repl: Option<&[u8]> = match ent {
            b"amp" => Some(b"&"),
            b"lt" => Some(b"<"),
            b"gt" => Some(b">"),
            b"quot" => Some(b"\""),
            b"apos" => Some(b"'"),
            _ => None,
        };
        if let Some(r) = repl {
            out.extend_from_slice(r);
            i = semi + 1;
            continue;
        }
        if ent.len() >= 2 && ent[0] == b'#' {
            let (radix, digits) = if matches!(ent.get(1), Some(&b'x') | Some(&b'X')) {
                (16, &ent[2..])
            } else {
                (10, &ent[1..])
            };
            if let Ok(vs) = std::str::from_utf8(digits) {
                if let Ok(v) = u32::from_str_radix(vs.trim(), radix) {
                    if let Some(ch) = char::from_u32(v) {
                        let mut buf = [0u8; 4];
                        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        i = semi + 1;
                        continue;
                    }
                }
            }
        }
        out.push(b'&');
        i += 1;
    }
}

/// Decoded, owned attribute value (reuses `scratch` between reads).
fn attr_str(tag: &[u8], name: &[u8], scratch: &mut Vec<u8>) -> Option<String> {
    let raw = find_attr(tag, name)?;
    scratch.clear();
    decode_escaped(raw, scratch);
    Some(String::from_utf8_lossy(scratch).into_owned())
}

/// Text content of the first element with local name `local` under `region`,
/// entity-decoded (`""` for empty / self-closing / unparseable elements).
fn first_elem_text(region: &[u8], local: &[u8], scratch: &mut Vec<u8>) -> Option<String> {
    let start = find_open_local(region, 0, local)?;
    let te = start + memchr::memchr(b'>', &region[start..])?;
    if te > 0 && region.get(te - 1) == Some(&b'/') {
        return Some(String::new());
    }
    let close = find_close_local(region, te.saturating_add(1), local)?;
    if te + 1 > close || close > region.len() {
        return Some(String::new());
    }
    scratch.clear();
    decode_escaped(&region[te + 1..close], scratch);
    Some(String::from_utf8_lossy(scratch).into_owned())
}

// ----------------------------------------------------------------------------
// Parsers.
// ----------------------------------------------------------------------------

/// Parse a `xl/threadedComments/threadedCommentN.xml` part.
///
/// Absent fast path: an empty part returns `Ok(vec![])` without any scan. A
/// non-empty part that is not a threadedComments document (no `<ThreadedComments`
/// root) is a format error. Malformed element markup inside the root degrades
/// gracefully (fields default to empty / comments skipped), never panics.
pub fn parse_threaded_comments(part: &[u8]) -> TurboResult<Vec<ThreadedComment>> {
    let mut out = Vec::new();
    if part.is_empty() {
        return Ok(out);
    }
    if find_open_local(part, 0, b"ThreadedComments").is_none() {
        return Err(TurboError::Format(
            "threadedComments part: missing <ThreadedComments> root".into(),
        ));
    }
    let n = part.len();
    let mut scratch = Vec::new();
    let mut i = 0usize;
    while let Some(start) = find_open_local(part, i, b"threadedComment") {
        let te = start + memchr::memchr(b'>', &part[start..]).unwrap_or(n - start);
        let tag = &part[start..te];
        let self_closing = te > 0 && part.get(te - 1) == Some(&b'/');
        let cell = attr_str(tag, b"ref", &mut scratch).unwrap_or_default();
        let author_id = attr_str(tag, b"personId", &mut scratch).unwrap_or_default();
        let id = attr_str(tag, b"id", &mut scratch).unwrap_or_default();
        let parent_id = attr_str(tag, b"parentId", &mut scratch);
        let created = attr_str(tag, b"dT", &mut scratch);
        let text = if self_closing {
            String::new()
        } else {
            let end = find_close_local(part, te + 1, b"threadedComment").unwrap_or(n);
            if te < end && end <= n {
                first_elem_text(&part[te + 1..end], b"text", &mut scratch).unwrap_or_default()
            } else {
                String::new()
            }
        };
        out.push(ThreadedComment {
            cell,
            text,
            author_id,
            created,
            id,
            parent_id,
        });
        i = te.saturating_add(1);
        if i > n {
            break;
        }
    }
    Ok(out)
}

/// Parse the `xl/persons/person.xml` author list.
///
/// Same contract as [`parse_threaded_comments`]: empty part → empty Vec;
/// non-empty part without a `<personList>` root → format error; malformed
/// `<person>` rows degrade gracefully.
pub fn parse_persons(part: &[u8]) -> TurboResult<Vec<Person>> {
    let mut out = Vec::new();
    if part.is_empty() {
        return Ok(out);
    }
    if find_open_local(part, 0, b"personList").is_none() {
        return Err(TurboError::Format(
            "persons part: missing <personList> root".into(),
        ));
    }
    let n = part.len();
    let mut scratch = Vec::new();
    let mut i = 0usize;
    while let Some(start) = find_open_local(part, i, b"person") {
        let te = start + memchr::memchr(b'>', &part[start..]).unwrap_or(n - start);
        let tag = &part[start..te];
        let id = attr_str(tag, b"id", &mut scratch).unwrap_or_default();
        let display_name = attr_str(tag, b"displayName", &mut scratch).unwrap_or_default();
        out.push(Person { id, display_name });
        i = te.saturating_add(1);
        if i > n {
            break;
        }
    }
    Ok(out)
}

// ----------------------------------------------------------------------------
// Writers (deterministic: fixed header, fixed attribute order, slice order).
// ----------------------------------------------------------------------------

const TC_NS: &[u8] = b"http://schemas.microsoft.com/office/spreadsheetml/2018/threadedcomments";

/// Escape `& < > "` into `out`. A single pass over the string; clean runs are
/// copied with one `extend_from_slice` each, so escaping is linear and there
/// is no per-char allocation.
fn write_escaped(out: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut run = 0usize;
    let mut i = 0usize;
    while i < n {
        let b = bytes[i];
        if b != b'&' && b != b'<' && b != b'>' && b != b'"' {
            i += 1;
            continue;
        }
        if i > run {
            out.extend_from_slice(&bytes[run..i]);
        }
        match b {
            b'&' => out.extend_from_slice(b"&amp;"),
            b'<' => out.extend_from_slice(b"&lt;"),
            b'>' => out.extend_from_slice(b"&gt;"),
            _ => out.extend_from_slice(b"&quot;"),
        }
        run = i + 1;
        i += 1;
    }
    if run < n {
        out.extend_from_slice(&bytes[run..]);
    }
}

/// Emit a `xl/threadedComments/threadedCommentN.xml` part.
///
/// Byte-identical for the same input: no timestamps, no randomness, comments
/// written in slice order, attributes in a fixed order. Only comments
/// referencing a non-empty author and id are materialized as `<text>` bodies;
/// empty ones still emit a valid self-contained element (empty `<text/>` is
/// written as `<text></text>` for readability — byte-stable either way).
pub fn write_threaded_comments(comments: &[ThreadedComment]) -> Vec<u8> {
    let mut out = Vec::with_capacity(160 + comments.len() * 160);
    out.extend_from_slice(b"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n");
    out.extend_from_slice(b"<ThreadedComments xmlns=\"");
    out.extend_from_slice(TC_NS);
    out.extend_from_slice(b"\">");
    for c in comments {
        out.extend_from_slice(b"<threadedComment ref=\"");
        write_escaped(&mut out, &c.cell);
        if let Some(ts) = &c.created {
            out.extend_from_slice(b"\" dT=\"");
            write_escaped(&mut out, ts);
        }
        out.extend_from_slice(b"\" personId=\"");
        write_escaped(&mut out, &c.author_id);
        out.extend_from_slice(b"\" id=\"");
        write_escaped(&mut out, &c.id);
        out.extend_from_slice(b"\"");
        if let Some(pid) = &c.parent_id {
            out.extend_from_slice(b" parentId=\"");
            write_escaped(&mut out, pid);
            out.extend_from_slice(b"\"");
        }
        out.extend_from_slice(b"><text>");
        write_escaped(&mut out, &c.text);
        out.extend_from_slice(b"</text></threadedComment>");
    }
    out.extend_from_slice(b"</ThreadedComments>");
    out
}

/// Emit the `xl/persons/person.xml` author list. Same determinism contract as
/// [`write_threaded_comments`]; only `displayName` and `id` are written (the
/// schema allows optional `userId`/`providerId`, which are not modelled here).
pub fn write_persons(persons: &[Person]) -> Vec<u8> {
    let mut out = Vec::with_capacity(160 + persons.len() * 96);
    out.extend_from_slice(b"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n");
    out.extend_from_slice(b"<personList xmlns=\"");
    out.extend_from_slice(TC_NS);
    out.extend_from_slice(
        b"\" xmlns:x=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">",
    );
    for p in persons {
        out.extend_from_slice(b"<person displayName=\"");
        write_escaped(&mut out, &p.display_name);
        out.extend_from_slice(b"\" id=\"");
        write_escaped(&mut out, &p.id);
        out.extend_from_slice(b"\"/>");
    }
    out.extend_from_slice(b"</personList>");
    out
}

// ----------------------------------------------------------------------------
// Reply chain.
// ----------------------------------------------------------------------------

/// The reply chain for one root comment: the root plus every direct or
/// transitive reply, in document order (the order they appear in `all`).
///
/// Excel writes replies after their parents, so one in-order pass over the
/// slice is enough: a comment joins the thread the moment its `parentId`
/// names something already in it. Comments whose `parentId` chain does not
/// reach `root_id` (other threads, orphans, or a missing root) are excluded.
/// Returns empty when `root_id` is not present.
pub fn thread_replies<'a>(all: &'a [ThreadedComment], root_id: &str) -> Vec<&'a ThreadedComment> {
    let mut out: Vec<&'a ThreadedComment> = Vec::new();
    let root_pos = match all.iter().position(|c| c.id == root_id) {
        Some(p) => p,
        None => return out,
    };
    let mut in_thread: Vec<&str> = Vec::with_capacity(all.len());
    in_thread.push(root_id);
    out.push(&all[root_pos]);
    for c in all.iter().skip(root_pos + 1) {
        match c.parent_id.as_deref() {
            Some(pid) if in_thread.contains(&pid) => {
                in_thread.push(c.id.as_str());
                out.push(c);
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn person(id: &str, name: &str) -> Person {
        Person {
            id: id.into(),
            display_name: name.into(),
        }
    }

    fn comment(
        cell: &str,
        text: &str,
        author: &str,
        created: Option<&str>,
        id: &str,
        parent: Option<&str>,
    ) -> ThreadedComment {
        ThreadedComment {
            cell: cell.into(),
            text: text.into(),
            author_id: author.into(),
            created: created.map(|s| s.into()),
            id: id.into(),
            parent_id: parent.map(|s| s.into()),
        }
    }

    const TC_XML: &[u8] = b"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<ThreadedComments xmlns=\"http://schemas.microsoft.com/office/spreadsheetml/2018/threadedcomments\">\
  <threadedComment ref=\"B2\" dT=\"2024-01-01T00:00:00.00\" personId=\"{alice}\" id=\"{c1}\">\
    <text>Root note &amp; more</text>\
  </threadedComment>\
  <threadedComment ref=\"B2\" personId=\"{bob}\" id=\"{c2}\" parentId=\"{c1}\">\
    <text>Reply &lt;ok&gt;</text>\
  </threadedComment>\
</ThreadedComments>";

    const PERSONS_XML: &[u8] = b"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<personList xmlns=\"http://schemas.microsoft.com/office/spreadsheetml/2018/threadedcomments\">\
  <person displayName=\"Alice\" id=\"{alice}\"/>\
  <person displayName=\"Bob &amp; Co\" id=\"{bob}\"/>\
</personList>";

    #[test]
    fn tc_parse_happy_path() {
        let parsed = parse_threaded_comments(TC_XML).expect("parse ok");
        assert_eq!(parsed.len(), 2);
        assert_eq!(
            parsed[0],
            comment(
                "B2",
                "Root note & more",
                "{alice}",
                Some("2024-01-01T00:00:00.00"),
                "{c1}",
                None
            )
        );
        assert_eq!(
            parsed[1],
            comment("B2", "Reply <ok>", "{bob}", None, "{c2}", Some("{c1}"))
        );
    }

    #[test]
    fn tc_parse_persons_happy_path() {
        let parsed = parse_persons(PERSONS_XML).expect("parse ok");
        assert_eq!(
            parsed,
            vec![person("{alice}", "Alice"), person("{bob}", "Bob & Co")]
        );
    }

    #[test]
    fn tc_round_trip_threaded() {
        let first = parse_threaded_comments(TC_XML).expect("parse ok");
        let bytes = write_threaded_comments(&first);
        let second = parse_threaded_comments(&bytes).expect("reparse ok");
        assert_eq!(first, second);
    }

    #[test]
    fn tc_round_trip_persons() {
        let first = parse_persons(PERSONS_XML).expect("parse ok");
        let bytes = write_persons(&first);
        let second = parse_persons(&bytes).expect("reparse ok");
        assert_eq!(first, second);
    }

    #[test]
    fn tc_write_escaping() {
        let c = vec![comment(
            "A<1\"&",
            "x & y < z > \" w",
            "{a\"<&}",
            None,
            "{c1}",
            Some("{c0}"),
        )];
        let bytes = write_threaded_comments(&c);
        let s = String::from_utf8_lossy(&bytes).into_owned();
        assert!(s.contains("ref=\"A&lt;1&quot;&amp;\""), "cell escaped: {s}");
        assert!(
            s.contains("personId=\"{a&quot;&lt;&amp;}\""),
            "author escaped: {s}"
        );
        assert!(
            s.contains("<text>x &amp; y &lt; z &gt; &quot; w</text>"),
            "text escaped: {s}"
        );
        assert!(s.contains("parentId=\"{c0}\""), "parent kept: {s}");
        let back = parse_threaded_comments(&bytes).expect("reparse ok");
        assert_eq!(back, c);
    }

    #[test]
    fn tc_write_deterministic() {
        let c = vec![
            comment(
                "A1",
                "first",
                "{alice}",
                Some("2024-01-01T00:00:00.00"),
                "{c1}",
                None,
            ),
            comment("B2", "second", "{bob}", None, "{c2}", Some("{c1}")),
        ];
        let a = write_threaded_comments(&c);
        let b = write_threaded_comments(&c);
        assert_eq!(a, b);
        let p = vec![person("{alice}", "Alice"), person("{bob}", "Bob")];
        assert_eq!(write_persons(&p), write_persons(&p));
    }

    #[test]
    fn tc_absent_part() {
        assert_eq!(parse_threaded_comments(b"").expect("absent ok"), Vec::new());
        assert_eq!(parse_persons(b"").expect("absent ok"), Vec::new());
    }

    #[test]
    fn tc_malformed_not_xml() {
        assert!(parse_threaded_comments(b"not xml at all \x00\x01").is_err());
        assert!(parse_persons(b"<foo/>").is_err());
    }

    #[test]
    fn tc_malformed_truncated() {
        // Root present, element never closed: must degrade, never panic.
        let r = parse_threaded_comments(
            b"<ThreadedComments><threadedComment ref=\"A1\" personId=\"{a}\" id=\"{",
        );
        assert!(r.is_ok(), "truncated threadedComment must not panic");
        assert!(parse_persons(b"<personList><person displayName=\"A\"").is_ok());
    }

    #[test]
    fn tc_malformed_unterminated_entity() {
        // Text with a dangling '&' is copied literally; parser survives.
        let xml = b"<ThreadedComments>\
<threadedComment ref=\"A1\" personId=\"{a}\" id=\"{c1}\"><text>see &foo</text></threadedComment>\
</ThreadedComments>";
        let parsed = parse_threaded_comments(xml).expect("parse ok");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].text, "see &foo");
    }

    #[test]
    fn tc_self_closing_comment() {
        let xml = b"<ThreadedComments>\
<threadedComment ref=\"C3\" personId=\"{a}\" id=\"{c3}\" done=\"1\"/>\
</ThreadedComments>";
        let parsed = parse_threaded_comments(xml).expect("parse ok");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].cell, "C3");
        assert_eq!(parsed[0].text, "");
    }

    #[test]
    fn tc_thread_replies_chain() {
        let all = vec![
            comment("A1", "root", "{alice}", None, "{c1}", None),
            comment("A1", "r1", "{bob}", None, "{c2}", Some("{c1}")),
            comment("A1", "r1.1", "{alice}", None, "{c3}", Some("{c2}")),
            comment("A1", "r2", "{carol}", None, "{c4}", Some("{c1}")),
            comment("A2", "other thread", "{alice}", None, "{x1}", None),
        ];
        let chain = thread_replies(&all, "{c1}");
        let ids: Vec<&str> = chain.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["{c1}", "{c2}", "{c3}", "{c4}"]);
    }

    #[test]
    fn tc_thread_replies_missing_root() {
        let all = vec![comment("A1", "root", "{alice}", None, "{c1}", None)];
        assert!(thread_replies(&all, "{nope}").is_empty());
        assert!(thread_replies(&[], "{c1}").is_empty());
    }
}
