//! Structural emitters: props, defined names, tables, comments/VML, protection,
//! external-link stub (F011–F013, F017, F021–F024, F066–F084, F100).

use super::charts::coord_to_tuple;
use super::model::{
    Comment, DefinedName, DocProps, HeaderFooter, Hyperlink, PageMargins, PageSetup, PrintOptions,
    Scenario, Sheet, SheetProtection, TableDef, Workbook,
};
use super::xml::{escape_text, push, push_str, write_escaped_attr, write_f64, write_u32};
use crate::turbo::meta::FilterColumnMeta;
use crate::turbo::range_a1;
use std::collections::HashMap;

pub const SHEET_NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
pub const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
pub const PKG_REL_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";

const RESERVED_NAMES: &[&str] = &[
    "Print_Area",
    "Print_Titles",
    "Criteria",
    "_FilterDatabase",
    "Extract",
    "Consolidate_Area",
    "Sheet_Title",
];

fn reserved_name_attr(name: &str) -> String {
    if RESERVED_NAMES.contains(&name) {
        format!("_xlnm.{name}")
    } else {
        name.to_string()
    }
}

/// Excel legacy sheet password hash (openpyxl `utils/protection.py`).
pub fn hash_password(plaintext: &str) -> String {
    let mut password: u32 = 0;
    for (idx, ch) in plaintext.chars().enumerate() {
        let idx = (idx + 1) as u32;
        let mut value = (ch as u32) << idx;
        let rotated = value >> 15;
        value &= 0x7fff;
        password ^= value | rotated;
    }
    password ^= plaintext.chars().count() as u32;
    password ^= 0xCE4B;
    format!("{:X}", password)
}

pub fn quote_sheetname(name: &str) -> String {
    format!("'{}'", name.replace('\'', "''"))
}

fn abs_cell(c: &str) -> String {
    let stripped: String = c.chars().filter(|ch| *ch != '$').collect();
    let (row, col) = coord_to_tuple(&stripped);
    let mut letters = String::new();
    let mut n = col;
    while n > 0 {
        n -= 1;
        letters.insert(0, (b'A' + (n % 26) as u8) as char);
        n /= 26;
    }
    format!("${letters}${row}")
}

pub fn abs_range(r: &str) -> String {
    if r.contains('$') {
        return r.to_string();
    }
    if let Some((a, b)) = r.split_once(':') {
        format!("{}:{}", abs_cell(a), abs_cell(b))
    } else {
        abs_cell(r)
    }
}

pub fn collect_defined_names(wb: &Workbook) -> Vec<DefinedName> {
    let mut names = wb.defined_names.clone();
    // FOUND: the writer already emits the hidden, sheet-scoped `_xlnm._FilterDatabase`
    // defined name whenever a sheet carries an autoFilter. Excel requires it or it
    // treats the filter as absent; it is always added here alongside `<autoFilter>`.
    for (idx, sheet) in wb.sheets.iter().enumerate() {
        let quoted = quote_sheetname(&sheet.name);
        if let Some(af) = &sheet.auto_filter {
            let abs = abs_range(&range_a1(&af.ref_));
            names.push(DefinedName {
                name: "_FilterDatabase".into(),
                value: format!("{quoted}!{abs}"),
                local_sheet_id: Some(idx as u32),
                hidden: true,
            });
        }
        if let Some(pt) = &sheet.print_titles {
            names.push(DefinedName {
                name: "Print_Titles".into(),
                value: pt.clone(),
                local_sheet_id: Some(idx as u32),
                hidden: false,
            });
        }
        if let Some(pa) = &sheet.print_area {
            let abs = abs_range(pa);
            names.push(DefinedName {
                name: "Print_Area".into(),
                value: format!("{quoted}!{abs}"),
                local_sheet_id: Some(idx as u32),
                hidden: false,
            });
        }
    }
    names
}

pub fn write_core_props(props: &DocProps, creator_fallback: &str) -> String {
    let creator = props.creator.as_deref().unwrap_or(creator_fallback);
    let mut s = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">"#,
    );
    s.push_str(&format!(
        "<dc:creator>{}</dc:creator>",
        escape_text(creator)
    ));
    if let Some(t) = &props.title {
        s.push_str(&format!("<dc:title>{}</dc:title>", escape_text(t)));
    }
    if let Some(d) = &props.description {
        s.push_str(&format!(
            "<dc:description>{}</dc:description>",
            escape_text(d)
        ));
    }
    if let Some(sub) = &props.subject {
        s.push_str(&format!("<dc:subject>{}</dc:subject>", escape_text(sub)));
    }
    if let Some(lm) = &props.last_modified_by {
        s.push_str(&format!(
            "<cp:lastModifiedBy>{}</cp:lastModifiedBy>",
            escape_text(lm)
        ));
    }
    let created = props.created.as_deref().unwrap_or("2020-01-01T00:00:00Z");
    let modified = props.modified.as_deref().unwrap_or("2020-01-01T00:00:00Z");
    s.push_str(&format!(
        r#"<dcterms:created xsi:type="dcterms:W3CDTF">{created}</dcterms:created>"#
    ));
    s.push_str(&format!(
        r#"<dcterms:modified xsi:type="dcterms:W3CDTF">{modified}</dcterms:modified>"#
    ));
    s.push_str("</cp:coreProperties>");
    s
}

pub fn write_app_props(props: &DocProps, sheet_titles: &[&str]) -> String {
    let mut vt = String::new();
    for t in sheet_titles {
        vt.push_str("<vt:lpstr>");
        vt.push_str(&escape_text(t));
        vt.push_str("</vt:lpstr>");
    }
    let mut s = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><Application>kyrax</Application><AppVersion>0.20</AppVersion><HeadingPairs><vt:vector size="2" baseType="variant"><vt:variant><vt:lpstr>Worksheets</vt:lpstr></vt:variant><vt:variant><vt:i4>{}</vt:i4></vt:variant></vt:vector></HeadingPairs><TitlesOfParts><vt:vector size="{}" baseType="lpstr">{}</vt:vector></TitlesOfParts>"#,
        sheet_titles.len(),
        sheet_titles.len(),
        vt
    );
    if let Some(c) = &props.company {
        s.push_str(&format!("<Company>{}</Company>", escape_text(c)));
    }
    s.push_str("</Properties>");
    s
}

pub fn write_custom_props(props: &[(String, String)]) -> String {
    let mut s = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Properties xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes" xmlns="http://schemas.openxmlformats.org/officeDocument/2006/custom-properties">"#,
    );
    for (i, (name, val)) in props.iter().enumerate() {
        let pid = i + 2;
        s.push_str(&format!(
            r#"<property name="{}" fmtid="{{D5CDD505-2E9C-101B-9397-08002B2CF9AE}}" pid="{pid}"><vt:lpwstr>{}</vt:lpwstr></property>"#,
            escape_attr(name),
            escape_text(val)
        ));
    }
    s.push_str("</Properties>");
    s
}

fn escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

pub fn write_external_link_part() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><externalLink xmlns="{SHEET_NS}" xmlns:r="{REL_NS}"><externalBook xmlns:r="{REL_NS}" r:id="rId1"><sheetNames><sheetName val="Sheet1"/></sheetNames><sheetDataSet><sheetData sheetId="0"/></sheetDataSet></externalBook></externalLink>"#
    )
}

pub fn write_table(t: &TableDef, id: usize) -> String {
    let mut s = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><table xmlns="{SHEET_NS}" id="{id}" name="{}" displayName="{}" ref="{}" headerRowCount="1">"#,
        escape_attr(&t.display_name),
        escape_attr(&t.display_name),
        escape_attr(&t.ref_)
    );
    s.push_str(&format!(r#"<autoFilter ref="{}"/>"#, escape_attr(&t.ref_)));
    s.push_str(&format!(r#"<tableColumns count="{}">"#, t.columns.len()));
    for (i, col) in t.columns.iter().enumerate() {
        s.push_str(&format!(
            r#"<tableColumn id="{}" name="{}"/>"#,
            i + 1,
            escape_attr(col)
        ));
    }
    s.push_str("</tableColumns>");
    if let Some(style) = &t.style_name {
        let stripes = if t.show_row_stripes { "1" } else { "0" };
        s.push_str(&format!(
            r#"<tableStyleInfo name="{}" showRowStripes="{stripes}"/>"#,
            escape_attr(style)
        ));
    }
    s.push_str("</table>");
    s
}

pub fn write_comments(comments: &[Comment]) -> (String, String) {
    let mut authors: Vec<String> = Vec::new();
    let mut author_idx: HashMap<String, usize> = HashMap::new();
    for c in comments {
        if !author_idx.contains_key(&c.author) {
            author_idx.insert(c.author.clone(), authors.len());
            authors.push(c.author.clone());
        }
    }
    let mut xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><comments xmlns="{SHEET_NS}"><authors>"#
    );
    for a in &authors {
        xml.push_str(&format!("<author>{}</author>", escape_text(a)));
    }
    xml.push_str("</authors><commentList>");
    for c in comments {
        let aid = author_idx[&c.author];
        xml.push_str(&format!(
            r#"<comment ref="{}" authorId="{aid}" shapeId="0"><text><t>{}</t></text></comment>"#,
            escape_attr(&c.ref_),
            escape_text(&c.text)
        ));
    }
    xml.push_str("</commentList></comments>");

    let mut vml = String::from(
        r#"<xml xmlns:o="urn:schemas-microsoft-com:office:office" xmlns:v="urn:schemas-microsoft-com:vml" xmlns:x="urn:schemas-microsoft-com:office:excel">"#,
    );
    vml.push_str(r#"<o:shapelayout v:ext="edit"><o:idmap v:ext="edit" data="1"/></o:shapelayout>"#);
    vml.push_str(
        r#"<v:shapetype id="_x0000_t202" coordsize="21600,21600" o:spt="202" path="m,l,21600r21600,l21600,xe"><v:stroke joinstyle="miter"/><v:path gradientshapeok="t" o:connecttype="rect"/></v:shapetype>"#,
    );
    for (i, c) in comments.iter().enumerate() {
        let shape_id = 1026 + i;
        let (row, col) = coord_to_tuple(&c.ref_);
        vml.push_str("<v:shape type=\"#_x0000_t202\" style=\"position:absolute;margin-left:59.25pt;margin-top:1.5pt;width:");
        vml.push_str(&c.width.to_string());
        vml.push_str("px;height:");
        vml.push_str(&c.height.to_string());
        vml.push_str(
            "px;z-index:1;visibility:hidden\" fillcolor=\"#ffffe1\" o:insetmode=\"auto\" id=\"_x0000_s",
        );
        vml.push_str(&format!("{:04}", shape_id));
        vml.push_str("\"><v:fill color2=\"#ffffe1\"/><v:shadow color=\"black\" obscured=\"t\"/><v:path o:connecttype=\"none\"/><v:textbox style=\"mso-direction-alt:auto\"><div style=\"text-align:left\"/></v:textbox><x:ClientData ObjectType=\"Note\"><x:MoveWithCells/><x:SizeWithCells/><x:AutoFill>False</x:AutoFill><x:Row>");
        vml.push_str(&(row - 1).to_string());
        vml.push_str("</x:Row><x:Column>");
        vml.push_str(&(col - 1).to_string());
        vml.push_str("</x:Column></x:ClientData></v:shape>");
    }
    vml.push_str("</xml>");
    (xml, vml)
}

/// Emit sheet tail after sheetData (ledger 20 partial helpers).
/// CF/DV are emitted by caller between mergeCells and hyperlinks.
pub fn emit_sheet_protection(out: &mut Vec<u8>, prot: &SheetProtection) {
    if !prot.sheet {
        return;
    }
    push(out, br#"<sheetProtection sheet="1""#);
    if let Some(pw) = &prot.password {
        let h = if prot.already_hashed {
            pw.clone()
        } else {
            hash_password(pw)
        };
        push(out, br#" password=""#);
        push_str(out, &h);
        push(out, b"\"");
    }
    push(
        out,
        br#" objects="0" scenarios="0" formatCells="1" formatColumns="1" formatRows="1" insertColumns="1" insertRows="1" insertHyperlinks="1" deleteColumns="1" deleteRows="1" selectLockedCells="0" selectUnlockedCells="0" sort="1" autoFilter="1" pivotTables="1"/>"#,
    );
}

pub fn emit_scenarios(out: &mut Vec<u8>, scenarios: &[Scenario]) {
    if scenarios.is_empty() {
        return;
    }
    push(out, b"<scenarios>");
    for sc in scenarios {
        push(out, br#"<scenario name=""#);
        write_escaped_attr(out, &sc.name);
        push(out, br#"" count=""#);
        write_u32(out, sc.cells.len() as u32);
        push(out, b"\">");
        for (r, v) in &sc.cells {
            push(out, br#"<inputCells r=""#);
            write_escaped_attr(out, r);
            push(out, br#"" val=""#);
            write_escaped_attr(out, v);
            push(out, br#""/>"#);
        }
        push(out, b"</scenario>");
    }
    push(out, b"</scenarios>");
}

/// Emit `<autoFilter ref="...">` plus its `filterColumn` children.
///
/// We emit exactly what the reader parses (see `crate::turbo::meta::scan_auto_filter`):
/// `filterColumn` with `colId` / `hiddenButton` / `showButton`, containing a
/// `<filters>` block with an optional `blank` attribute and `<filter val="..."/>`
/// children. Known remaining read/write gaps (no read model, so nothing to emit):
/// `customFilters` / `customFilter`, `top10`, `dynamicFilter`, `colorFilter`,
/// `iconFilter`, and `sortState` / `sortCondition`.
pub fn emit_auto_filter(out: &mut Vec<u8>, ref_: &str, columns: &[FilterColumnMeta]) {
    push(out, br#"<autoFilter ref=""#);
    write_escaped_attr(out, ref_);
    if columns.is_empty() {
        push(out, br#""/>"#);
        return;
    }
    push(out, b"\">");
    for col in columns {
        push(out, br#"<filterColumn colId=""#);
        write_u32(out, col.col_id);
        push(out, br#"" hiddenButton=""#);
        write_u32(out, col.hidden_button as u32);
        push(out, br#"" showButton=""#);
        write_u32(out, col.show_button as u32);
        if col.values.is_empty() && col.blank.is_none() {
            push(out, br#""/>"#);
            continue;
        }
        push(out, b"\">");
        push(out, b"<filters");
        if let Some(blank) = col.blank {
            push(out, br#" blank=""#);
            write_u32(out, blank as u32);
            push(out, b"\"");
        }
        if col.values.is_empty() {
            push(out, br#"/>"#);
        } else {
            push(out, b">");
            for v in &col.values {
                push(out, br#"<filter val=""#);
                write_escaped_attr(out, v);
                push(out, br#""/>"#);
            }
            push(out, b"</filters>");
        }
        push(out, b"</filterColumn>");
    }
    push(out, b"</autoFilter>");
}

pub fn emit_merges(out: &mut Vec<u8>, merges: &[String]) {
    if merges.is_empty() {
        return;
    }
    push(out, br#"<mergeCells count=""#);
    write_u32(out, merges.len() as u32);
    push(out, b"\">");
    for m in merges {
        push(out, br#"<mergeCell ref=""#);
        write_escaped_attr(out, m);
        push(out, br#""/>"#);
    }
    push(out, b"</mergeCells>");
}

/// Emit hyperlinks; returns rel entries (type, target, target_mode).
pub fn emit_hyperlinks(
    out: &mut Vec<u8>,
    links: &[Hyperlink],
    next_rid: &mut usize,
) -> Vec<(String, String, Option<String>)> {
    let mut rels = Vec::new();
    if links.is_empty() {
        return rels;
    }
    push(out, b"<hyperlinks>");
    for hl in links {
        if let Some(t) = &hl.target {
            *next_rid += 1;
            let id = format!("rId{next_rid}");
            rels.push((
                "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink"
                    .into(),
                t.clone(),
                Some("External".into()),
            ));
            push(out, br#"<hyperlink ref=""#);
            write_escaped_attr(out, &hl.ref_);
            push(out, br#"" r:id=""#);
            push_str(out, &id);
            push(out, b"\"");
            if let Some(d) = &hl.display {
                push(out, br#" display=""#);
                write_escaped_attr(out, d);
                push(out, b"\"");
            }
            push(out, b"/>");
        } else if let Some(loc) = &hl.location {
            push(out, br#"<hyperlink ref=""#);
            write_escaped_attr(out, &hl.ref_);
            push(out, br#"" location=""#);
            write_escaped_attr(out, loc);
            push(out, b"\"");
            if let Some(d) = &hl.display {
                push(out, br#" display=""#);
                write_escaped_attr(out, d);
                push(out, b"\"");
            }
            push(out, b"/>");
        }
    }
    push(out, b"</hyperlinks>");
    rels
}

pub fn emit_print_options(out: &mut Vec<u8>, po: &PrintOptions) {
    let mut parts = Vec::new();
    if po.horizontal_centered {
        parts.push(r#"horizontalCentered="1""#);
    }
    if po.vertical_centered {
        parts.push(r#"verticalCentered="1""#);
    }
    if po.headings {
        parts.push(r#"headings="1""#);
    }
    if po.grid_lines {
        parts.push(r#"gridLines="1""#);
    }
    if parts.is_empty() {
        return;
    }
    push(out, b"<printOptions ");
    push_str(out, &parts.join(" "));
    push(out, b"/>");
}

pub fn emit_page_margins(out: &mut Vec<u8>, m: &PageMargins) {
    push(out, br#"<pageMargins left=""#);
    write_f64(out, m.left);
    push(out, br#"" right=""#);
    write_f64(out, m.right);
    push(out, br#"" top=""#);
    write_f64(out, m.top);
    push(out, br#"" bottom=""#);
    write_f64(out, m.bottom);
    push(out, br#"" header=""#);
    write_f64(out, m.header);
    push(out, br#"" footer=""#);
    write_f64(out, m.footer);
    push(out, br#""/>"#);
}

pub fn emit_default_page_margins(out: &mut Vec<u8>) {
    push(
        out,
        br#"<pageMargins left="0.75" right="0.75" top="1" bottom="1" header="0.5" footer="0.5"/>"#,
    );
}

pub fn emit_page_setup(out: &mut Vec<u8>, ps: &PageSetup) {
    let mut a = Vec::new();
    if let Some(o) = &ps.orientation {
        a.push(format!(r#"orientation="{}""#, escape_attr(o)));
    }
    if let Some(p) = ps.paper_size {
        a.push(format!(r#"paperSize="{p}""#));
    }
    if let Some(sc) = ps.scale {
        a.push(format!(r#"scale="{sc}""#));
    }
    if let Some(w) = ps.fit_to_width {
        a.push(format!(r#"fitToWidth="{w}""#));
    }
    if let Some(h) = ps.fit_to_height {
        a.push(format!(r#"fitToHeight="{h}""#));
    }
    if a.is_empty() {
        return;
    }
    push(out, b"<pageSetup ");
    push_str(out, &a.join(" "));
    push(out, b"/>");
}

pub fn emit_header_footer(out: &mut Vec<u8>, hf: &HeaderFooter) {
    let mut body = String::new();
    let mut oh = String::new();
    if let Some(l) = &hf.odd_header_left {
        oh.push_str(&format!("&amp;L{}", escape_text(l)));
    }
    if let Some(c) = &hf.odd_header_center {
        oh.push_str(&format!("&amp;C{}", escape_text(c)));
    }
    if let Some(r) = &hf.odd_header_right {
        oh.push_str(&format!("&amp;R{}", escape_text(r)));
    }
    if !oh.is_empty() {
        body.push_str(&format!("<oddHeader>{oh}</oddHeader>"));
    }
    let mut of = String::new();
    if let Some(l) = &hf.odd_footer_left {
        of.push_str(&format!("&amp;L{}", escape_text(l)));
    }
    if let Some(c) = &hf.odd_footer_center {
        of.push_str(&format!("&amp;C{}", escape_text(c)));
    }
    if let Some(r) = &hf.odd_footer_right {
        of.push_str(&format!("&amp;R{}", escape_text(r)));
    }
    if !of.is_empty() {
        body.push_str(&format!("<oddFooter>{of}</oddFooter>"));
    }
    if body.is_empty() {
        return;
    }
    push(out, b"<headerFooter>");
    push_str(out, &body);
    push(out, b"</headerFooter>");
}

pub fn emit_breaks(out: &mut Vec<u8>, row_breaks: &[u32], col_breaks: &[u32]) {
    if !row_breaks.is_empty() {
        let n = row_breaks.len() as u32;
        push(out, br#"<rowBreaks count=""#);
        write_u32(out, n);
        push(out, br#"" manualBreakCount=""#);
        write_u32(out, n);
        push(out, b"\">");
        for id in row_breaks {
            push(out, br#"<brk id=""#);
            write_u32(out, *id);
            push(out, br#"" min="0" max="16383" man="1"/>"#);
        }
        push(out, b"</rowBreaks>");
    }
    if !col_breaks.is_empty() {
        let n = col_breaks.len() as u32;
        push(out, br#"<colBreaks count=""#);
        write_u32(out, n);
        push(out, br#"" manualBreakCount=""#);
        write_u32(out, n);
        push(out, b"\">");
        for id in col_breaks {
            push(out, br#"<brk id=""#);
            write_u32(out, *id);
            push(out, br#"" min="0" max="16383" man="1"/>"#);
        }
        push(out, b"</colBreaks>");
    }
}

/// Whether sheet needs `xmlns:r` on the worksheet root.
pub fn sheet_needs_r_ns(sheet: &Sheet) -> bool {
    sheet.hyperlinks.iter().any(|h| h.target.is_some())
        || !sheet.charts.is_empty()
        || !sheet.comments.is_empty()
        || !sheet.tables.is_empty()
}

/// Root rels with optional custom props.
pub fn root_rels_xml(has_custom: bool) -> String {
    let mut s = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml" Id="rId1"/><Relationship Type="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" Target="docProps/core.xml" Id="rId2"/><Relationship Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties" Target="docProps/app.xml" Id="rId3"/>"#,
    );
    if has_custom {
        s.push_str(
            r#"<Relationship Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/custom-properties" Target="docProps/custom.xml" Id="rId4"/>"#,
        );
    }
    s.push_str("</Relationships>");
    s
}

/// Emit definedNames block content (opening/closing tags included if non-empty).
pub fn emit_defined_names_xml(names: &[DefinedName]) -> String {
    if names.is_empty() {
        return String::new();
    }
    let mut s = String::from("<definedNames>");
    for n in names {
        let nm = reserved_name_attr(&n.name);
        s.push_str(&format!(r#"<definedName name="{}""#, escape_attr(&nm)));
        if let Some(ls) = n.local_sheet_id {
            s.push_str(&format!(r#" localSheetId="{ls}""#));
        }
        if n.hidden {
            s.push_str(r#" hidden="1""#);
        }
        s.push_str(&format!(">{}</definedName>", escape_text(&n.value)));
    }
    s.push_str("</definedNames>");
    s
}

// Kept: the single predicate for "does this sheet need a structural part".
// The writer inlines the check per part; this is the canonical statement.
#[allow(dead_code)]
pub fn sheet_has_structural(sheet: &Sheet) -> bool {
    sheet.tab_color_rgb.is_some()
        || sheet.protection.is_some()
        || !sheet.scenarios.is_empty()
        || sheet.auto_filter.is_some()
        || !sheet.merges.is_empty()
        || !sheet.hyperlinks.is_empty()
        || sheet.print_options.is_some()
        || sheet.page_margins.is_some()
        || sheet.page_setup.is_some()
        || sheet.header_footer.is_some()
        || !sheet.row_breaks.is_empty()
        || !sheet.col_breaks.is_empty()
        || !sheet.tables.is_empty()
        || !sheet.comments.is_empty()
        || !sheet.charts.is_empty()
        || sheet.print_area.is_some()
        || sheet.print_titles.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_hash_secret() {
        // openpyxl: hash_password("secret") == "DAA7"
        assert_eq!(hash_password("secret").to_uppercase(), "DAA7");
    }

    #[test]
    fn abs_range_basic() {
        assert_eq!(abs_range("A1:C5"), "$A$1:$C$5");
    }

    fn fc(col_id: u32, values: Vec<&str>, blank: Option<bool>) -> FilterColumnMeta {
        FilterColumnMeta {
            col_id,
            hidden_button: false,
            show_button: true,
            values: values.into_iter().map(String::from).collect(),
            blank,
        }
    }

    #[test]
    fn emit_auto_filter_value_filters() {
        let mut out = Vec::new();
        let cols = vec![fc(0, vec!["Alice", "Carol"], Some(false))];
        emit_auto_filter(&mut out, "A1:D6", &cols);
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains(r#"<autoFilter ref="A1:D6">"#), "{s}");
        assert!(
            s.contains(r#"<filterColumn colId="0" hiddenButton="0" showButton="1">"#),
            "{s}"
        );
        assert!(s.contains(r#"<filters blank="0">"#), "{s}");
        assert!(s.contains(r#"<filter val="Alice"/>"#), "{s}");
        assert!(s.contains(r#"<filter val="Carol"/>"#), "{s}");
        assert!(s.ends_with("</autoFilter>"), "{s}");
    }

    #[test]
    fn emit_auto_filter_blank_only_self_closes_filters() {
        let mut out = Vec::new();
        let mut col = fc(3, vec![], Some(true));
        col.hidden_button = true;
        col.show_button = false;
        emit_auto_filter(&mut out, "A1:B2", &[col]);
        let s = String::from_utf8_lossy(&out);
        assert!(
            s.contains(r#"<filterColumn colId="3" hiddenButton="1" showButton="0">"#),
            "{s}"
        );
        assert!(s.contains(r#"<filters blank="1"/>"#), "{s}");
    }

    #[test]
    fn emit_auto_filter_empty_column_self_closes() {
        let mut out = Vec::new();
        emit_auto_filter(&mut out, "A1:C1", &[fc(1, vec![], None)]);
        let s = String::from_utf8_lossy(&out);
        assert!(
            s.contains(r#"<filterColumn colId="1" hiddenButton="0" showButton="1"/>"#),
            "{s}"
        );
    }

    #[test]
    fn emit_auto_filter_no_columns_self_closes() {
        let mut out = Vec::new();
        emit_auto_filter(&mut out, "A1:C3", &[]);
        assert_eq!(
            String::from_utf8_lossy(&out),
            r#"<autoFilter ref="A1:C3"/>"#
        );
    }
}
