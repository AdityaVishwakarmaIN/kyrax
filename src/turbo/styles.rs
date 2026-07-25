// styles.rs — StyleTable built once from xl/styles.xml.
// Implements DESIGN.md phase 1 + 3: numFmts (builtins 0-163 hardcoded + custom >=164),
// cellXfs, fonts, fills, per-xf is_date/is_timedelta via openpyxl regex rules (ported),
// colors tagged rgb|indexed|theme+tint|auto|none.
//
// Rules cited to temp-openpyxl/openpyxl:
//   styles/numbers.py:13-52 BUILTIN_FORMATS, :95-116 is_date_format/is_timedelta_format
//   styles/colors.py:16-30 COLOR_INDEX, :80-95 Color union + default rgb '00000000'
//   styles/fills.py:84 PatternFill default fgColor=Color() (rgb 00000000)
//   styles/cell_style.py:94 missing id -> 0

use super::decode::decode_bytes;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum CKind {
    None,
    Auto,
    Rgb,
    Indexed,
    Theme,
}

#[derive(Clone, Copy, Debug)]
pub struct Color {
    pub kind: CKind,
    pub val: u32, // argb (rgb) | indexed id | theme id
    pub tint: f32,
}
impl Color {
    pub fn default_rgb() -> Color {
        // matches openpyxl Color() default: type 'rgb', rgb '00000000'
        Color {
            kind: CKind::Rgb,
            val: 0x0000_0000,
            tint: 0.0,
        }
    }
    pub fn none() -> Color {
        Color {
            kind: CKind::None,
            val: 0,
            tint: 0.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Font {
    pub name: String,
    pub sz: f32,
    pub bold: bool,
    pub italic: bool,
    pub underline: Option<String>,
    pub color: Color,
    pub family: i32,
    pub scheme: Option<String>,
}
impl Font {
    fn default_calibri() -> Font {
        Font {
            name: "Calibri".into(),
            sz: 11.0,
            bold: false,
            italic: false,
            underline: None,
            color: Color::default_rgb(),
            family: 0,
            scheme: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Fill {
    pub pattern: String, // "none","solid","gray125",..., "gradient"
    pub fg: Color,
    pub bg: Color,
}
impl Fill {
    fn default() -> Fill {
        Fill {
            pattern: "none".into(),
            fg: Color::default_rgb(),
            bg: Color::default_rgb(),
        }
    }
}

// ----------------------------------------------------------------------------
// B1 — Full borders
// ----------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Side {
    pub style: Option<String>, // None = no border
    pub color: Color,          // meaningful only if style is Some
}

impl Side {
    pub fn none() -> Side {
        Side {
            style: None,
            color: Color::none(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Border {
    pub left: Side,
    pub right: Side,
    pub top: Side,
    pub bottom: Side,
    pub diagonal: Side,
    pub diagonal_up: bool,
    pub diagonal_down: bool,
    pub outline: bool,
}

impl Border {
    pub fn default_empty() -> Border {
        Border {
            left: Side::none(),
            right: Side::none(),
            top: Side::none(),
            bottom: Side::none(),
            diagonal: Side::none(),
            diagonal_up: false,
            diagonal_down: false,
            outline: true,
        }
    }
}

// ----------------------------------------------------------------------------
// B2 — Alignment + protection
// ----------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Alignment {
    pub horizontal: Option<String>,
    pub vertical: Option<String>,
    pub text_rotation: u16,
    pub wrap_text: Option<bool>,
    pub shrink_to_fit: Option<bool>,
    pub indent: u8,
    pub relative_indent: i16,
    pub justify_last_line: Option<bool>,
    pub reading_order: u8,
}

impl Default for Alignment {
    fn default() -> Self {
        Self {
            horizontal: None,
            vertical: None,
            text_rotation: 0,
            wrap_text: None,
            shrink_to_fit: None,
            indent: 0,
            relative_indent: 0,
            justify_last_line: None,
            reading_order: 0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Protection {
    pub locked: bool,
    pub hidden: bool,
}

impl Default for Protection {
    fn default() -> Self {
        Self {
            locked: true,
            hidden: false,
        }
    }
}

// ----------------------------------------------------------------------------
// B3 — Named styles
// ----------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct NamedStyleRec {
    pub name: String,
    pub xf_id: u16,
    pub builtin_id: Option<u16>,
    pub hidden: bool,
}

// ----------------------------------------------------------------------------
// B5 — Differential styles (dxfs)
// ----------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct DxfFont {
    pub name: Option<String>,
    pub sz: Option<f32>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<String>,
    pub color: Option<Color>,
}

#[derive(Clone, Debug, Default)]
pub struct Dxf {
    pub font: Option<DxfFont>,
    pub fill: Option<Fill>,
    pub border: Option<Border>,
    pub num_fmt: Option<String>,
    pub alignment: Option<Alignment>,
    pub protection: Option<Protection>,
}

#[derive(Clone, Debug)]
pub struct Xf {
    pub num_fmt_id: u16,
    pub font_id: u16,
    pub fill_id: u16,
    pub border_id: u16,
    pub alignment: Alignment,
    pub protection: Protection,
    pub xf_id: u16, // index into cellStyleXfs / named styles
}

#[derive(Clone, Debug)]
pub struct Resolved {
    pub number_format: String,
    pub is_date: bool,
    pub is_timedelta: bool,
    pub font: Font,
    pub fill: Fill,
    pub border_id: u16,
    pub border: Border,
    pub alignment: Alignment,
    pub protection: Protection,
    pub style_name: Option<String>,
}

#[derive(Clone)]
pub struct StyleTable {
    pub custom_numfmt: std::collections::HashMap<u16, String>,
    pub fonts: Vec<Font>,
    pub fills: Vec<Fill>,
    pub borders: Vec<Border>,
    pub xfs: Vec<Xf>,
    pub border_count: usize,
    pub named_styles: Vec<NamedStyleRec>,
    pub dxfs: Vec<Dxf>,
    pub indexed_palette: Option<Vec<u32>>,
    // precomputed per-xf (openpyxl's date_formats set analogue)
    pub xf_is_date: Vec<bool>,
    pub xf_is_timedelta: Vec<bool>,
    pub xf_numfmt_code: Vec<String>,
}

// ----------------------------------------------------------------------------
// Builtin number formats (numbers.py:13-52). Sparse; unlisted <164 => General.
// ----------------------------------------------------------------------------
pub fn builtin_format(id: u16) -> Option<&'static str> {
    Some(match id {
        0 => "General",
        1 => "0",
        2 => "0.00",
        3 => "#,##0",
        4 => "#,##0.00",
        5 => "\"$\"#,##0_);(\"$\"#,##0)",
        6 => "\"$\"#,##0_);[Red](\"$\"#,##0)",
        7 => "\"$\"#,##0.00_);(\"$\"#,##0.00)",
        8 => "\"$\"#,##0.00_);[Red](\"$\"#,##0.00)",
        9 => "0%",
        10 => "0.00%",
        11 => "0.00E+00",
        12 => "# ?/?",
        13 => "# ??/??",
        14 => "mm-dd-yy",
        15 => "d-mmm-yy",
        16 => "d-mmm",
        17 => "mmm-yy",
        18 => "h:mm AM/PM",
        19 => "h:mm:ss AM/PM",
        20 => "h:mm",
        21 => "h:mm:ss",
        22 => "m/d/yy h:mm",
        37 => "#,##0_);(#,##0)",
        38 => "#,##0_);[Red](#,##0)",
        39 => "#,##0.00_);(#,##0.00)",
        40 => "#,##0.00_);[Red](#,##0.00)",
        41 => "_(* #,##0_);_(* \\(#,##0\\);_(* \"-\"_);_(@_)",
        42 => "_(\"$\"* #,##0_);_(\"$\"* \\(#,##0\\);_(\"$\"* \"-\"_);_(@_)",
        43 => "_(* #,##0.00_);_(* \\(#,##0.00\\);_(* \"-\"??_);_(@_)",
        44 => "_(\"$\"* #,##0.00_)_(\"$\"* \\(#,##0.00\\)_(\"$\"* \"-\"??_)_(@_)",
        45 => "mm:ss",
        46 => "[h]:mm:ss",
        47 => "mmss.0",
        48 => "##0.0E+0",
        49 => "@",
        _ => return None,
    })
}

// ----------------------------------------------------------------------------
// is_date_format / is_timedelta_format — ported from numbers.py:95-116.
// STRIP_RE removes quoted literals and [..] locale groups EXCEPT [h]/[m]/[s].
// then date iff /(?<![_\\])[dmhysDMHYS]/ matches.
// ----------------------------------------------------------------------------
fn strip_literals(fmt: &str) -> String {
    let b = fmt.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                // remove quoted literal ".*?"
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += 1;
                }
                if i < b.len() {
                    i += 1;
                } // skip closing quote
            }
            b'[' => {
                // find closing ]
                let mut j = i + 1;
                while j < b.len() && b[j] != b']' {
                    j += 1;
                }
                let inner = &b[i + 1..j.min(b.len())];
                // keep iff inner is h/hh/m/mm/s/ss (elapsed) — LOCALE_GROUP negative lookahead
                let kept = matches!(inner, b"h" | b"hh" | b"m" | b"mm" | b"s" | b"ss");
                if kept {
                    out.extend_from_slice(&b[i..(j + 1).min(b.len())]);
                }
                i = (j + 1).min(b.len());
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn is_date_format(fmt: &str) -> bool {
    let first = fmt.split(';').next().unwrap_or("");
    let stripped = strip_literals(first);
    let b = stripped.as_bytes();
    for i in 0..b.len() {
        let c = b[i];
        if matches!(
            c,
            b'd' | b'm' | b'h' | b'y' | b's' | b'D' | b'M' | b'H' | b'Y' | b'S'
        ) {
            // negative lookbehind (?<![_\\])
            if i == 0 || (b[i - 1] != b'_' && b[i - 1] != b'\\') {
                return true;
            }
        }
    }
    false
}

pub fn is_timedelta_format(fmt: &str) -> bool {
    // TIMEDELTA_RE search on first section: presence of an elapsed bracket [h]/[hh]/[mm]/[ss] (case-insens).
    let first = fmt.split(';').next().unwrap_or("");
    let b = first.as_bytes();
    let mut i = 0;
    while i + 2 < b.len() {
        if b[i] == b'[' {
            // read run of same letter in {h,m,s}
            let c = b[i + 1].to_ascii_lowercase();
            if matches!(c, b'h' | b'm' | b's') {
                let mut j = i + 1;
                while j < b.len() && b[j].to_ascii_lowercase() == c {
                    j += 1;
                }
                let run = j - (i + 1);
                if (run == 1 || run == 2) && j < b.len() && b[j] == b']' {
                    return true;
                }
            }
        }
        i += 1;
    }
    false
}

// ----------------------------------------------------------------------------
// Tiny attribute reader over a single element open-tag slice.
// ----------------------------------------------------------------------------
fn attr<'a>(tag: &'a [u8], name: &str) -> Option<&'a [u8]> {
    // match ` name="` (leading space to avoid substring hits) OR name at start.
    let pat = format!(" {}=\"", name);
    let pos = memchr::memmem::find(tag, pat.as_bytes())?;
    let vs = pos + pat.len();
    let ve = vs + memchr::memchr(b'"', &tag[vs..])?;
    Some(&tag[vs..ve])
}

fn attr_str(tag: &[u8], name: &str, scratch: &mut Vec<u8>) -> Option<String> {
    let raw = attr(tag, name)?;
    let dec = decode_bytes(raw, scratch);
    Some(String::from_utf8_lossy(dec).into_owned())
}

fn parse_bool_flag(tag: &[u8], _name: &str) -> Option<bool> {
    // returns Some(true) for bare <b/> or val="1"/"true", Some(false) for val="0", None if absent.
    // caller passes the whole child element tag e.g. b`<b val="1"/>` or b`<b/>`.
    match attr(tag, "val") {
        Some(v) => Some(v == b"1" || v.eq_ignore_ascii_case(b"true")),
        None => Some(true),
    }
}

// Parse a color child element (open tag slice, attrs only). Absent => caller decides.
pub(crate) fn parse_color(tag: &[u8]) -> Color {
    if let Some(rgb) = attr(tag, "rgb") {
        let s = std::str::from_utf8(rgb).unwrap_or("");
        let s = if s.len() == 6 {
            format!("00{}", s)
        } else {
            s.to_string()
        };
        let v = u32::from_str_radix(&s, 16).unwrap_or(0);
        return Color {
            kind: CKind::Rgb,
            val: v,
            tint: 0.0,
        };
    }
    if let Some(idx) = attr(tag, "indexed") {
        let v: u32 = std::str::from_utf8(idx).unwrap_or("0").parse().unwrap_or(0);
        return Color {
            kind: CKind::Indexed,
            val: v,
            tint: 0.0,
        };
    }
    if let Some(th) = attr(tag, "theme") {
        let v: u32 = std::str::from_utf8(th).unwrap_or("0").parse().unwrap_or(0);
        let tint: f32 = attr(tag, "tint")
            .and_then(|t| std::str::from_utf8(t).ok())
            .and_then(|t| t.parse().ok())
            .unwrap_or(0.0);
        return Color {
            kind: CKind::Theme,
            val: v,
            tint,
        };
    }
    if let Some(a) = attr(tag, "auto") {
        if a == b"1" || a.eq_ignore_ascii_case(b"true") {
            return Color {
                kind: CKind::Auto,
                val: 1,
                tint: 0.0,
            };
        }
    }
    Color::default_rgb()
}

// find region [after '>' of <tag ...>, position of </tag>] for a named container that occurs once
fn container<'a>(x: &'a [u8], open: &str, close: &str) -> Option<(&'a [u8], usize, usize)> {
    let o = memchr::memmem::find(x, open.as_bytes())?;
    let start = o + memchr::memchr(b'>', &x[o..])?;
    // handle self-closing container (e.g. <numFmts/>): no content
    if x[start - 1] == b'/' {
        return None;
    }
    let start = start + 1;
    let c = memchr::memmem::find(&x[start..], close.as_bytes()).map(|p| start + p)?;
    Some((&x[start..c], start, c))
}

/// Iterate direct child elements named `child` (open tag..close) inside `region`.
/// Returns Vec of (full_element_slice, open_tag_slice) where open_tag_slice excludes '<'/'>'.
fn each_element<'a>(region: &'a [u8], child: &str) -> Vec<(&'a [u8], &'a [u8])> {
    let open = format!("<{}", child);
    let close = format!("</{}>", child);
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(p) = memchr::memmem::find(&region[i..], open.as_bytes()) {
        let s = i + p;
        let after = region.get(s + open.len()).copied().unwrap_or(b'>');
        if !(after == b' ' || after == b'>' || after == b'/') {
            i = s + open.len();
            continue;
        }
        let Some(gt) = memchr::memchr(b'>', &region[s..]) else {
            break; // truncated open tag — degrade without panic
        };
        let tag_end = s + gt;
        if tag_end == 0 || tag_end >= region.len() {
            break;
        }
        let open_tag = &region[s + 1..tag_end]; // includes attrs, excludes < >
        if region.get(tag_end.saturating_sub(1)) == Some(&b'/') {
            // self-closing
            let ot = if open_tag.is_empty() {
                open_tag
            } else {
                &open_tag[..open_tag.len() - 1]
            };
            out.push((&region[s..tag_end + 1], ot));
            i = tag_end + 1;
            continue;
        }
        let ce = memchr::memmem::find(&region[tag_end..], close.as_bytes())
            .map(|p| tag_end + p)
            .unwrap_or(region.len());
        out.push((&region[s..(ce + close.len()).min(region.len())], open_tag));
        i = (ce + close.len()).min(region.len());
    }
    out
}

pub fn parse_style_table(xml: &[u8]) -> StyleTable {
    let mut scratch = Vec::new();
    let mut custom_numfmt = std::collections::HashMap::new();

    // numFmts
    if let Some((region, _, _)) = container(xml, "<numFmts", "</numFmts>") {
        for (_, tag) in each_element(region, "numFmt") {
            if let (Some(id), Some(code)) = (
                attr(tag, "numFmtId")
                    .and_then(|v| std::str::from_utf8(v).ok())
                    .and_then(|v| v.parse::<u16>().ok()),
                attr_str(tag, "formatCode", &mut scratch),
            ) {
                custom_numfmt.insert(id, code);
            }
        }
    }

    // fonts
    let mut fonts = Vec::new();
    if let Some((region, _, _)) = container(xml, "<fonts", "</fonts>") {
        for (elem, _) in each_element(region, "font") {
            let mut f = Font::default_calibri();
            // sub-elements
            if let Some(v) = each_element(elem, "name").first() {
                if let Some(n) = attr_str(v.1, "val", &mut scratch) {
                    f.name = n;
                }
            }
            if let Some(v) = each_element(elem, "sz").first() {
                if let Some(s) = attr(v.1, "val")
                    .and_then(|v| std::str::from_utf8(v).ok())
                    .and_then(|v| v.parse::<f32>().ok())
                {
                    f.sz = s;
                }
            }
            // bold: <b/> or <b val="1"/>
            f.bold = each_element(elem, "b")
                .first()
                .map(|v| parse_bool_flag(v.1, "b").unwrap_or(true))
                .unwrap_or(false);
            f.italic = each_element(elem, "i")
                .first()
                .map(|v| parse_bool_flag(v.1, "i").unwrap_or(true))
                .unwrap_or(false);
            if let Some(v) = each_element(elem, "u").first() {
                f.underline =
                    Some(attr_str(v.1, "val", &mut scratch).unwrap_or_else(|| "single".into()));
            }
            if let Some(v) = each_element(elem, "color").first() {
                f.color = parse_color(v.1);
            }
            if let Some(v) = each_element(elem, "family").first() {
                f.family = attr(v.1, "val")
                    .and_then(|v| std::str::from_utf8(v).ok())
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
            }
            if let Some(v) = each_element(elem, "scheme").first() {
                f.scheme = attr_str(v.1, "val", &mut scratch);
            }
            fonts.push(f);
        }
    }
    if fonts.is_empty() {
        fonts.push(Font::default_calibri());
    }

    // fills
    let mut fills = Vec::new();
    if let Some((region, _, _)) = container(xml, "<fills", "</fills>") {
        for (elem, _) in each_element(region, "fill") {
            let mut fill = Fill::default();
            if let Some((pf, ptag)) = each_element(elem, "patternFill").first().copied() {
                fill.pattern =
                    attr_str(ptag, "patternType", &mut scratch).unwrap_or_else(|| "none".into());
                fill.fg = each_element(pf, "fgColor")
                    .first()
                    .map(|v| parse_color(v.1))
                    .unwrap_or_else(Color::default_rgb);
                fill.bg = each_element(pf, "bgColor")
                    .first()
                    .map(|v| parse_color(v.1))
                    .unwrap_or_else(Color::default_rgb);
            } else if each_element(elem, "gradientFill").first().is_some() {
                fill.pattern = "gradient".into();
            }
            fills.push(fill);
        }
    }
    if fills.is_empty() {
        fills.push(Fill::default());
    }

    // borders (full Side records)
    let mut borders = Vec::new();
    if let Some((region, _, _)) = container(xml, "<borders", "</borders>") {
        for (elem, tag) in each_element(region, "border") {
            borders.push(parse_border_elem(elem, tag, &mut scratch));
        }
    }
    if borders.is_empty() {
        borders.push(Border::default_empty());
    }
    let border_count = borders.len();

    // cellStyleXfs (master styles; used for named-style linkage only)
    let _cell_style_xfs_count = container(xml, "<cellStyleXfs", "</cellStyleXfs>")
        .map(|(r, _, _)| each_element(r, "xf").len())
        .unwrap_or(0);

    // cellStyles → named styles (dedupe by xfId, first wins)
    let mut named_styles = Vec::new();
    let mut seen_xf: std::collections::HashSet<u16> = std::collections::HashSet::new();
    if let Some((region, _, _)) = container(xml, "<cellStyles", "</cellStyles>") {
        for (_, tag) in each_element(region, "cellStyle") {
            let name = attr_str(tag, "name", &mut scratch).unwrap_or_default();
            let xf_id: u16 = attr(tag, "xfId")
                .and_then(|v| std::str::from_utf8(v).ok())
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            if !seen_xf.insert(xf_id) {
                continue;
            }
            let builtin_id = attr(tag, "builtinId")
                .and_then(|v| std::str::from_utf8(v).ok())
                .and_then(|v| v.parse().ok());
            let hidden = attr(tag, "hidden")
                .map(|v| v == b"1" || v.eq_ignore_ascii_case(b"true"))
                .unwrap_or(false);
            named_styles.push(NamedStyleRec {
                name,
                xf_id,
                builtin_id,
                hidden,
            });
        }
    }
    // Ensure Normal at xfId 0 if missing
    if !named_styles.iter().any(|n| n.xf_id == 0) {
        named_styles.insert(
            0,
            NamedStyleRec {
                name: "Normal".into(),
                xf_id: 0,
                builtin_id: Some(0),
                hidden: false,
            },
        );
    }

    // cellXfs
    let mut xfs = Vec::new();
    if let Some((region, _, _)) = container(xml, "<cellXfs", "</cellXfs>") {
        for (elem, tag) in each_element(region, "xf") {
            let gi = |n: &str| -> u16 {
                attr(tag, n)
                    .and_then(|v| std::str::from_utf8(v).ok())
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0)
            };
            let alignment = each_element(elem, "alignment")
                .first()
                .map(|v| parse_alignment(v.1))
                .unwrap_or_default();
            let protection = each_element(elem, "protection")
                .first()
                .map(|v| parse_protection(v.1))
                .unwrap_or_default();
            xfs.push(Xf {
                num_fmt_id: gi("numFmtId"),
                font_id: gi("fontId"),
                fill_id: gi("fillId"),
                border_id: gi("borderId"),
                alignment,
                protection,
                xf_id: gi("xfId"),
            });
        }
    }
    if xfs.is_empty() {
        xfs.push(Xf {
            num_fmt_id: 0,
            font_id: 0,
            fill_id: 0,
            border_id: 0,
            alignment: Alignment::default(),
            protection: Protection::default(),
            xf_id: 0,
        });
    }

    // dxfs
    let mut dxfs = Vec::new();
    if let Some((region, _, _)) = container(xml, "<dxfs", "</dxfs>") {
        for (elem, _) in each_element(region, "dxf") {
            dxfs.push(parse_dxf(elem, &mut scratch));
        }
    }

    // indexed palette override
    let indexed_palette = container(xml, "<indexedColors", "</indexedColors>").map(|(r, _, _)| {
        each_element(r, "rgbColor")
            .iter()
            .map(|(_, tag)| {
                attr(tag, "rgb")
                    .and_then(|v| std::str::from_utf8(v).ok())
                    .and_then(|v| u32::from_str_radix(v, 16).ok())
                    .unwrap_or(0)
            })
            .collect::<Vec<u32>>()
    });

    // precompute per-xf numfmt code + is_date/is_timedelta (openpyxl date_formats set)
    let numfmt_code_for = |id: u16| -> String {
        if let Some(c) = custom_numfmt.get(&id) {
            c.clone()
        } else if let Some(b) = builtin_format(id) {
            b.to_string()
        } else {
            "General".to_string() // unlisted <164 => General
        }
    };
    let mut xf_is_date = Vec::with_capacity(xfs.len());
    let mut xf_is_timedelta = Vec::with_capacity(xfs.len());
    let mut xf_numfmt_code = Vec::with_capacity(xfs.len());
    for xf in &xfs {
        let code = numfmt_code_for(xf.num_fmt_id);
        xf_is_date.push(is_date_format(&code));
        xf_is_timedelta.push(is_timedelta_format(&code));
        xf_numfmt_code.push(code);
    }

    StyleTable {
        custom_numfmt,
        fonts,
        fills,
        borders,
        xfs,
        border_count,
        named_styles,
        dxfs,
        indexed_palette,
        xf_is_date,
        xf_is_timedelta,
        xf_numfmt_code,
    }
}

fn parse_side(elem: &[u8], open_tag: &[u8]) -> Side {
    let style = attr(open_tag, "style").and_then(|v| {
        let s = std::str::from_utf8(v).ok()?.to_string();
        if s.is_empty() { None } else { Some(s) }
    });
    let color = each_element(elem, "color")
        .first()
        .map(|v| parse_color(v.1))
        .unwrap_or_else(Color::none);
    Side { style, color }
}

fn parse_border_elem(elem: &[u8], open_tag: &[u8], _scratch: &mut Vec<u8>) -> Border {
    let side = |name: &str| -> Side {
        each_element(elem, name)
            .first()
            .map(|v| parse_side(v.0, v.1))
            .unwrap_or_else(Side::none)
    };
    let flag = |n: &str| -> bool {
        attr(open_tag, n)
            .map(|v| v == b"1" || v.eq_ignore_ascii_case(b"true"))
            .unwrap_or(false)
    };
    let outline = attr(open_tag, "outline")
        .map(|v| v == b"1" || v.eq_ignore_ascii_case(b"true"))
        .unwrap_or(true);
    Border {
        left: side("left"),
        right: side("right"),
        top: side("top"),
        bottom: side("bottom"),
        diagonal: side("diagonal"),
        diagonal_up: flag("diagonalUp"),
        diagonal_down: flag("diagonalDown"),
        outline,
    }
}

fn parse_alignment(tag: &[u8]) -> Alignment {
    let mut a = Alignment::default();
    a.horizontal = attr(tag, "horizontal")
        .and_then(|v| std::str::from_utf8(v).ok())
        .map(|s| s.to_string());
    a.vertical = attr(tag, "vertical")
        .and_then(|v| std::str::from_utf8(v).ok())
        .map(|s| s.to_string());
    a.text_rotation = attr(tag, "textRotation")
        .and_then(|v| std::str::from_utf8(v).ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    a.wrap_text = attr(tag, "wrapText").map(|v| v == b"1" || v.eq_ignore_ascii_case(b"true"));
    a.shrink_to_fit =
        attr(tag, "shrinkToFit").map(|v| v == b"1" || v.eq_ignore_ascii_case(b"true"));
    a.indent = attr(tag, "indent")
        .and_then(|v| std::str::from_utf8(v).ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    a.relative_indent = attr(tag, "relativeIndent")
        .and_then(|v| std::str::from_utf8(v).ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    a.justify_last_line =
        attr(tag, "justifyLastLine").map(|v| v == b"1" || v.eq_ignore_ascii_case(b"true"));
    a.reading_order = attr(tag, "readingOrder")
        .and_then(|v| std::str::from_utf8(v).ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    a
}

fn parse_protection(tag: &[u8]) -> Protection {
    Protection {
        locked: attr(tag, "locked")
            .map(|v| v == b"1" || v.eq_ignore_ascii_case(b"true"))
            .unwrap_or(true),
        hidden: attr(tag, "hidden")
            .map(|v| v == b"1" || v.eq_ignore_ascii_case(b"true"))
            .unwrap_or(false),
    }
}

fn parse_dxf(elem: &[u8], scratch: &mut Vec<u8>) -> Dxf {
    let mut dxf = Dxf::default();
    // font — sparse
    if let Some((fe, _)) = each_element(elem, "font").first().copied() {
        let mut df = DxfFont {
            name: None,
            sz: None,
            bold: None,
            italic: None,
            underline: None,
            color: None,
        };
        if let Some(v) = each_element(fe, "name").first() {
            df.name = attr_str(v.1, "val", scratch);
        }
        if let Some(v) = each_element(fe, "sz").first() {
            df.sz = attr(v.1, "val")
                .and_then(|v| std::str::from_utf8(v).ok())
                .and_then(|v| v.parse().ok());
        }
        if let Some(v) = each_element(fe, "b").first() {
            df.bold = parse_bool_flag(v.1, "b");
        }
        if let Some(v) = each_element(fe, "i").first() {
            df.italic = parse_bool_flag(v.1, "i");
        }
        if let Some(v) = each_element(fe, "u").first() {
            df.underline = Some(attr_str(v.1, "val", scratch).unwrap_or_else(|| "single".into()));
        }
        if let Some(v) = each_element(fe, "color").first() {
            df.color = Some(parse_color(v.1));
        }
        dxf.font = Some(df);
    }
    if let Some((fe, _)) = each_element(elem, "fill").first().copied() {
        let mut fill = Fill::default();
        if let Some((pf, ptag)) = each_element(fe, "patternFill").first().copied() {
            fill.pattern = attr_str(ptag, "patternType", scratch).unwrap_or_else(|| "none".into());
            fill.fg = each_element(pf, "fgColor")
                .first()
                .map(|v| parse_color(v.1))
                .unwrap_or_else(Color::default_rgb);
            fill.bg = each_element(pf, "bgColor")
                .first()
                .map(|v| parse_color(v.1))
                .unwrap_or_else(Color::default_rgb);
        }
        dxf.fill = Some(fill);
    }
    if let Some((be, btag)) = each_element(elem, "border").first().copied() {
        dxf.border = Some(parse_border_elem(be, btag, scratch));
    }
    if let Some((_, ntag)) = each_element(elem, "numFmt").first().copied() {
        dxf.num_fmt = attr_str(ntag, "formatCode", scratch);
    }
    if let Some((_, atag)) = each_element(elem, "alignment").first().copied() {
        dxf.alignment = Some(parse_alignment(atag));
    }
    if let Some((_, ptag)) = each_element(elem, "protection").first().copied() {
        dxf.protection = Some(parse_protection(ptag));
    }
    dxf
}

impl StyleTable {
    #[inline]
    fn xf(&self, s: u32) -> &Xf {
        self.xfs.get(s as usize).unwrap_or(&self.xfs[0])
    }
    pub fn numfmt_code(&self, s: u32) -> &str {
        self.xf_numfmt_code
            .get(s as usize)
            .map(|c| c.as_str())
            .unwrap_or("General")
    }
    pub fn is_date(&self, s: u32) -> bool {
        self.xf_is_date.get(s as usize).copied().unwrap_or(false)
    }
    pub fn font(&self, s: u32) -> &Font {
        let xf = self.xf(s);
        self.fonts
            .get(xf.font_id as usize)
            .unwrap_or(&self.fonts[0])
    }
    pub fn fill(&self, s: u32) -> &Fill {
        let xf = self.xf(s);
        self.fills
            .get(xf.fill_id as usize)
            .unwrap_or(&self.fills[0])
    }
    pub fn border(&self, s: u32) -> &Border {
        let xf = self.xf(s);
        self.borders
            .get(xf.border_id as usize)
            .unwrap_or(&self.borders[0])
    }
    pub fn style_name(&self, s: u32) -> Option<&str> {
        let xf = self.xf(s);
        self.named_styles
            .iter()
            .find(|n| n.xf_id == xf.xf_id)
            .map(|n| n.name.as_str())
    }
    pub fn resolve(&self, s: u32) -> Resolved {
        let xf = self.xf(s);
        Resolved {
            number_format: self.numfmt_code(s).to_string(),
            is_date: self.is_date(s),
            is_timedelta: self
                .xf_is_timedelta
                .get(s as usize)
                .copied()
                .unwrap_or(false),
            font: self.font(s).clone(),
            fill: self.fill(s).clone(),
            border_id: xf.border_id,
            border: self.border(s).clone(),
            alignment: xf.alignment.clone(),
            protection: xf.protection,
            style_name: self.style_name(s).map(|s| s.to_string()),
        }
    }
}
