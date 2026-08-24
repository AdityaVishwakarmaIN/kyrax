//! StyleEngine: hash-keyed component pools -> StyleArray -> cellXfs index.
//! Port of writelab/siloB StyleEngine (ledger 14-19).

use ahash::AHashMap;
use std::hash::Hash;

use super::xml::{
    push_str, write_escaped_attr, write_escaped_text, write_f64 as push_f64, write_i32 as push_i32,
    write_u32 as push_u32,
};

#[inline]
fn esc_attr(s: &str, out: &mut Vec<u8>) {
    write_escaped_attr(out, s);
}

#[inline]
fn esc_text(s: &str, out: &mut Vec<u8>) {
    write_escaped_text(out, s);
}

/// Emit an f64 as Excel would for integral values (90 not 90.0); non-integral
/// values keep the shortest round-trip decimal (ryu).
#[inline]
fn push_f64_trim(out: &mut Vec<u8>, v: f64) {
    if v == v.trunc() && (-9_007_199_254_740_992.0..=9_007_199_254_740_992.0).contains(&v) {
        let mut buf = itoa::Buffer::new();
        out.extend_from_slice(buf.format(v as i64).as_bytes());
    } else {
        push_f64(out, v);
    }
}

// ---------------------------------------------------------------------------
// Colors
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ColorSpec {
    Rgb(u32),        // aRGB 0xAARRGGBB (openpyxl often stores AA=00 for 6-digit input)
    Theme(u32, u64), // theme index, tint as f64 IEEE bits (Eq/Hash-stable, like sz_bits)
    Indexed(u32),
    Auto,
}

impl ColorSpec {
    /// Parse openpyxl-style color: "FF0000" or "00FF0000" → aRGB.
    /// 6-digit input gets AA=00 prefix (openpyxl RGB descriptor).
    pub fn from_rgb_hex(s: &str) -> Self {
        let h = s.trim_start_matches('#');
        let v = if h.len() == 6 || h.len() == 8 {
            u32::from_str_radix(h, 16).unwrap_or(0)
        } else {
            0
        };
        ColorSpec::Rgb(v)
    }

    /// Theme color with no tint (common case — emits `<color theme="N" />`).
    pub fn theme(index: u32) -> Self {
        ColorSpec::Theme(index, 0.0f64.to_bits())
    }

    /// Theme color with a tint in -1.0..=1.0.
    pub fn theme_tinted(index: u32, tint: f64) -> Self {
        ColorSpec::Theme(index, tint.to_bits())
    }

    /// The tint of this color (0.0 unless it is a themed color).
    pub fn tint(&self) -> f64 {
        match self {
            ColorSpec::Theme(_, bits) => f64::from_bits(*bits),
            _ => 0.0,
        }
    }
}

fn push_hex8(out: &mut Vec<u8>, v: u32) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for i in (0..8).rev() {
        let nibble = ((v >> (i * 4)) & 0xF) as usize;
        out.push(HEX[nibble]);
    }
}

impl ColorSpec {
    pub fn emit(&self, tag: &str, out: &mut Vec<u8>) {
        out.push(b'<');
        push_str(out, tag);
        match self {
            ColorSpec::Rgb(v) => {
                push_str(out, " rgb=\"");
                push_hex8(out, *v);
                out.push(b'"');
            }
            ColorSpec::Theme(t, tint_bits) => {
                push_str(out, " theme=\"");
                push_u32(out, *t);
                out.push(b'"');
                // Tint emitted only when non-zero so the common case stays byte-stable.
                let tint = f64::from_bits(*tint_bits);
                if tint != 0.0 {
                    push_str(out, " tint=\"");
                    push_f64(out, tint);
                    out.push(b'"');
                }
            }
            ColorSpec::Indexed(i) => {
                push_str(out, " indexed=\"");
                push_u32(out, *i);
                out.push(b'"');
            }
            ColorSpec::Auto => {
                push_str(out, " auto=\"1\"");
            }
        }
        push_str(out, " />");
    }
}

// ---------------------------------------------------------------------------
// Font / Fill / Border / Alignment / Protection
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct FontDesc {
    pub name: Option<String>,
    /// Font size stored as IEEE bits for Eq/Hash (openpyxl uses float).
    pub sz_bits: Option<u64>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<String>,
    pub strike: Option<bool>,
    /// Font effects: valueless empty elements (`<outline/>` etc.) in OOXML.
    pub outline: Option<bool>,
    pub shadow: Option<bool>,
    pub condense: Option<bool>,
    pub extend: Option<bool>,
    pub color: Option<ColorSpec>,
    pub family: Option<i32>,
    pub scheme: Option<String>,
    pub vert_align: Option<String>,
    pub charset: Option<i32>,
}

impl FontDesc {
    pub fn sz(&self) -> Option<f64> {
        self.sz_bits.map(f64::from_bits)
    }
    pub fn set_sz(&mut self, v: f64) {
        self.sz_bits = Some(v.to_bits());
    }
}

impl FontDesc {
    pub fn default_calibri() -> Self {
        FontDesc {
            name: Some("Calibri".into()),
            sz_bits: Some(11.0f64.to_bits()),
            bold: Some(false),
            italic: Some(false),
            underline: None,
            strike: None,
            outline: None,
            shadow: None,
            condense: None,
            extend: None,
            color: Some(ColorSpec::theme(1)),
            family: Some(2),
            scheme: Some("minor".into()),
            vert_align: None,
            charset: None,
        }
    }

    pub fn simple(name: &str, sz: f64, bold: bool, color_hex: Option<&str>) -> Self {
        FontDesc {
            name: Some(name.into()),
            sz_bits: Some(sz.to_bits()),
            bold: if bold { Some(true) } else { None },
            italic: None,
            underline: None,
            strike: None,
            outline: None,
            shadow: None,
            condense: None,
            extend: None,
            color: color_hex.map(ColorSpec::from_rgb_hex),
            family: None,
            scheme: None,
            vert_align: None,
            charset: None,
        }
    }

    pub fn emit(&self, out: &mut Vec<u8>) {
        push_str(out, "<font>");
        if let Some(n) = self.name.as_ref() {
            push_str(out, "<name val=\"");
            esc_attr(n, out);
            push_str(out, "\" />");
        }
        if let Some(f) = self.family {
            push_str(out, "<family val=\"");
            push_i32(out, f);
            push_str(out, "\" />");
        }
        if let Some(c) = self.charset {
            push_str(out, "<charset val=\"");
            push_i32(out, c);
            push_str(out, "\" />");
        }
        // openpyxl NestedBool with _no_value: only emit bold/italic if truthy
        if self.bold == Some(true) {
            push_str(out, "<b val=\"1\" />");
        }
        if self.italic == Some(true) {
            push_str(out, "<i val=\"1\" />");
        }
        if self.strike == Some(true) {
            push_str(out, "<strike val=\"1\" />");
        } else if self.strike == Some(false) {
            push_str(out, "<strike val=\"0\" />");
        }
        // Font effects: valueless empty elements, OOXML order after <strike/>.
        if self.outline == Some(true) {
            push_str(out, "<outline/>");
        }
        if self.shadow == Some(true) {
            push_str(out, "<shadow/>");
        }
        if self.condense == Some(true) {
            push_str(out, "<condense/>");
        }
        if self.extend == Some(true) {
            push_str(out, "<extend/>");
        }
        if let Some(c) = self.color.as_ref() {
            c.emit("color", out);
        }
        if let Some(sz) = self.sz() {
            push_str(out, "<sz val=\"");
            push_f64(out, sz);
            push_str(out, "\" />");
        }
        if let Some(u) = self.underline.as_ref() {
            push_str(out, "<u val=\"");
            esc_attr(u, out);
            push_str(out, "\" />");
        }
        if let Some(v) = self.vert_align.as_ref() {
            push_str(out, "<vertAlign val=\"");
            esc_attr(v, out);
            push_str(out, "\" />");
        }
        if let Some(s) = self.scheme.as_ref() {
            push_str(out, "<scheme val=\"");
            esc_attr(s, out);
            push_str(out, "\" />");
        }
        push_str(out, "</font>");
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FillDesc {
    /// patternType None → empty <patternFill/>; Some("none") also common on read.
    Pattern {
        pattern_type: Option<String>,
        fg: Option<ColorSpec>,
        bg: Option<ColorSpec>,
    },
    Gradient {
        kind: GradientKind,
        stops: Vec<GradientStop>,
    },
}

/// One `position` (0.0..=1.0) + color of a gradient stop.
/// The position is stored as f64 IEEE bits for Eq/Hash stability.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GradientStop {
    pub position: u64, // f64 IEEE bits
    pub color: ColorSpec,
}

impl GradientStop {
    pub fn new(position: f64, color: ColorSpec) -> Self {
        GradientStop {
            position: position.to_bits(),
            color,
        }
    }
    #[inline]
    pub fn pos(&self) -> f64 {
        f64::from_bits(self.position)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum GradientKind {
    /// linear: degree is an f64 angle (stored as IEEE bits).
    Linear { degree: u64 },
    /// path: extent from each edge, f64 in 0.0..=1.0 (stored as IEEE bits).
    Path {
        left: u64,
        right: u64,
        top: u64,
        bottom: u64,
    },
}

impl GradientKind {
    pub fn linear(degree: f64) -> Self {
        GradientKind::Linear {
            degree: degree.to_bits(),
        }
    }
    pub fn path(left: f64, right: f64, top: f64, bottom: f64) -> Self {
        GradientKind::Path {
            left: left.to_bits(),
            right: right.to_bits(),
            top: top.to_bits(),
            bottom: bottom.to_bits(),
        }
    }
}

impl FillDesc {
    pub fn none() -> Self {
        FillDesc::Pattern {
            pattern_type: None,
            fg: None,
            bg: None,
        }
    }
    pub fn gray125() -> Self {
        FillDesc::Pattern {
            pattern_type: Some("gray125".into()),
            fg: None,
            bg: None,
        }
    }
    pub fn solid(fg_rgb_hex: &str) -> Self {
        FillDesc::Pattern {
            pattern_type: Some("solid".into()),
            fg: Some(ColorSpec::from_rgb_hex(fg_rgb_hex)),
            bg: None,
        }
    }

    pub fn emit(&self, out: &mut Vec<u8>) {
        push_str(out, "<fill>");
        match self {
            FillDesc::Pattern {
                pattern_type,
                fg,
                bg,
            } => {
                push_str(out, "<patternFill");
                if let Some(pt) = pattern_type.as_ref() {
                    push_str(out, " patternType=\"");
                    esc_attr(pt, out);
                    out.push(b'"');
                }
                if fg.is_none() && bg.is_none() {
                    push_str(out, " />");
                } else {
                    push_str(out, ">");
                    if let Some(c) = fg.as_ref() {
                        c.emit("fgColor", out);
                    }
                    if let Some(c) = bg.as_ref() {
                        c.emit("bgColor", out);
                    }
                    push_str(out, "</patternFill>");
                }
            }
            FillDesc::Gradient { kind, stops } => {
                push_str(out, "<gradientFill type=\"");
                match kind {
                    GradientKind::Linear { degree } => {
                        push_str(out, "linear\"");
                        let d = f64::from_bits(*degree);
                        // degree emitted only when non-zero (matches openpyxl).
                        if d != 0.0 {
                            push_str(out, " degree=\"");
                            push_f64_trim(out, d);
                            out.push(b'"');
                        }
                    }
                    GradientKind::Path {
                        left,
                        right,
                        top,
                        bottom,
                    } => {
                        push_str(out, "path\"");
                        // path emits extent attrs instead of degree
                        for (name, bits) in [
                            ("left", *left),
                            ("right", *right),
                            ("top", *top),
                            ("bottom", *bottom),
                        ] {
                            let v = f64::from_bits(bits);
                            if v != 0.0 {
                                push_str(out, " ");
                                push_str(out, name);
                                push_str(out, "=\"");
                                push_f64_trim(out, v);
                                out.push(b'"');
                            }
                        }
                    }
                }
                push_str(out, ">");
                for stop in stops {
                    push_str(out, "<stop position=\"");
                    push_f64_trim(out, stop.pos());
                    push_str(out, "\">");
                    stop.color.emit("color", out);
                    push_str(out, "</stop>");
                }
                push_str(out, "</gradientFill>");
            }
        }
        push_str(out, "</fill>");
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct SideDesc {
    pub style: Option<String>,
    pub color: Option<ColorSpec>,
}

impl SideDesc {
    pub fn emit(&self, tag: &str, out: &mut Vec<u8>) {
        out.push(b'<');
        push_str(out, tag);
        if let Some(st) = self.style.as_ref() {
            push_str(out, " style=\"");
            esc_attr(st, out);
            out.push(b'"');
        }
        if self.color.is_none() {
            push_str(out, " />");
        } else {
            push_str(out, ">");
            if let Some(c) = self.color.as_ref() {
                c.emit("color", out);
            }
            push_str(out, "</");
            push_str(out, tag);
            out.push(b'>');
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BorderDesc {
    pub left: Option<SideDesc>,
    pub right: Option<SideDesc>,
    pub top: Option<SideDesc>,
    pub bottom: Option<SideDesc>,
    pub diagonal: Option<SideDesc>,
    pub diagonal_up: bool,
    pub diagonal_down: bool,
    pub outline: bool,
    /// When true, emit empty side tags even without style (DEFAULT_BORDER).
    pub emit_empty_sides: bool,
}

impl Default for BorderDesc {
    fn default() -> Self {
        BorderDesc {
            left: None,
            right: None,
            top: None,
            bottom: None,
            diagonal: None,
            diagonal_up: false,
            diagonal_down: false,
            outline: true,
            emit_empty_sides: false,
        }
    }
}

impl BorderDesc {
    pub fn default_border() -> Self {
        BorderDesc {
            left: Some(SideDesc::default()),
            right: Some(SideDesc::default()),
            top: Some(SideDesc::default()),
            bottom: Some(SideDesc::default()),
            diagonal: Some(SideDesc::default()),
            diagonal_up: false,
            diagonal_down: false,
            outline: true,
            emit_empty_sides: true,
        }
    }

    pub fn thin_all(color_hex: &str) -> Self {
        let side = SideDesc {
            style: Some("thin".into()),
            color: Some(ColorSpec::from_rgb_hex(color_hex)),
        };
        BorderDesc {
            left: Some(side.clone()),
            right: Some(side.clone()),
            top: Some(side.clone()),
            bottom: Some(side),
            diagonal: None,
            diagonal_up: false,
            diagonal_down: false,
            outline: true,
            emit_empty_sides: false,
        }
    }

    pub fn emit(&self, out: &mut Vec<u8>) {
        push_str(out, "<border");
        if self.diagonal_up {
            push_str(out, " diagonalUp=\"1\"");
        }
        if self.diagonal_down {
            push_str(out, " diagonalDown=\"1\"");
        }
        if !self.outline {
            push_str(out, " outline=\"0\"");
        }
        let has_any = self.left.is_some()
            || self.right.is_some()
            || self.top.is_some()
            || self.bottom.is_some()
            || self.diagonal.is_some();
        if !has_any {
            push_str(out, " />");
            return;
        }
        push_str(out, ">");
        for (tag, side) in [
            ("left", &self.left),
            ("right", &self.right),
            ("top", &self.top),
            ("bottom", &self.bottom),
            ("diagonal", &self.diagonal),
        ] {
            if let Some(s) = side {
                s.emit(tag, out);
            }
        }
        push_str(out, "</border>");
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct AlignDesc {
    pub horizontal: Option<String>,
    pub vertical: Option<String>,
    pub text_rotation: i32,
    pub wrap_text: Option<bool>,
    pub shrink_to_fit: Option<bool>,
    pub indent: i32,
    pub relative_indent: i32,
    pub justify_last_line: Option<bool>,
    pub reading_order: i32,
}

impl AlignDesc {
    pub fn is_default(&self) -> bool {
        self == &AlignDesc::default()
    }

    pub fn emit(&self, out: &mut Vec<u8>) {
        push_str(out, "<alignment");
        if let Some(h) = self.horizontal.as_ref() {
            push_str(out, " horizontal=\"");
            esc_attr(h, out);
            out.push(b'"');
        }
        if let Some(v) = self.vertical.as_ref() {
            push_str(out, " vertical=\"");
            esc_attr(v, out);
            out.push(b'"');
        }
        if self.text_rotation != 0 {
            push_str(out, " textRotation=\"");
            push_i32(out, self.text_rotation);
            out.push(b'"');
        }
        if self.wrap_text == Some(true) {
            push_str(out, " wrapText=\"1\"");
        }
        if self.shrink_to_fit == Some(true) {
            push_str(out, " shrinkToFit=\"1\"");
        }
        if self.indent != 0 {
            push_str(out, " indent=\"");
            push_i32(out, self.indent);
            out.push(b'"');
        }
        if self.relative_indent != 0 {
            push_str(out, " relativeIndent=\"");
            push_i32(out, self.relative_indent);
            out.push(b'"');
        }
        if self.justify_last_line == Some(true) {
            push_str(out, " justifyLastLine=\"1\"");
        }
        if self.reading_order != 0 {
            push_str(out, " readingOrder=\"");
            push_i32(out, self.reading_order);
            out.push(b'"');
        }
        push_str(out, " />");
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProtDesc {
    pub locked: bool,
    pub hidden: bool,
}

impl Default for ProtDesc {
    fn default() -> Self {
        ProtDesc {
            locked: true,
            hidden: false,
        }
    }
}

impl ProtDesc {
    pub fn is_default(&self) -> bool {
        self == &ProtDesc::default()
    }
    pub fn emit(&self, out: &mut Vec<u8>) {
        push_str(out, "<protection locked=\"");
        out.push(if self.locked { b'1' } else { b'0' });
        push_str(out, "\" hidden=\"");
        out.push(if self.hidden { b'1' } else { b'0' });
        push_str(out, "\" />");
    }
}

// ---------------------------------------------------------------------------
// Style descriptor (user-facing) + StyleArray
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct StyleDesc {
    pub font: Option<FontDesc>,
    pub fill: Option<FillDesc>,
    pub border: Option<BorderDesc>,
    pub num_fmt: Option<String>,
    pub alignment: Option<AlignDesc>,
    pub protection: Option<ProtDesc>,
    pub named_style: Option<String>,
    pub quote_prefix: bool,
    pub pivot_button: bool,
}

/// openpyxl StyleArray: 9 i32 slots.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct StyleArray {
    pub font_id: i32,
    pub fill_id: i32,
    pub border_id: i32,
    pub num_fmt_id: i32,
    pub protection_id: i32,
    pub alignment_id: i32,
    pub pivot_button: i32,
    pub quote_prefix: i32,
    pub xf_id: i32,
}

impl StyleArray {
    pub fn any_nonzero(&self) -> bool {
        self.font_id != 0
            || self.fill_id != 0
            || self.border_id != 0
            || self.num_fmt_id != 0
            || self.protection_id != 0
            || self.alignment_id != 0
            || self.pivot_button != 0
            || self.quote_prefix != 0
            || self.xf_id != 0
    }
}

// ---------------------------------------------------------------------------
// Builtin number formats (numbers.py)
// ---------------------------------------------------------------------------

pub const BUILTIN_FORMATS_MAX_SIZE: i32 = 164;

fn builtin_formats() -> &'static [(&'static str, i32)] {
    &[
        ("General", 0),
        ("0", 1),
        ("0.00", 2),
        ("#,##0", 3),
        ("#,##0.00", 4),
        ("\"$\"#,##0_);(\"$\"#,##0)", 5),
        ("\"$\"#,##0_);[Red](\"$\"#,##0)", 6),
        ("\"$\"#,##0.00_);(\"$\"#,##0.00)", 7),
        ("\"$\"#,##0.00_);[Red](\"$\"#,##0.00)", 8),
        ("0%", 9),
        ("0.00%", 10),
        ("0.00E+00", 11),
        ("# ?/?", 12),
        ("# ??/??", 13),
        ("mm-dd-yy", 14),
        ("d-mmm-yy", 15),
        ("d-mmm", 16),
        ("mmm-yy", 17),
        ("h:mm AM/PM", 18),
        ("h:mm:ss AM/PM", 19),
        ("h:mm", 20),
        ("h:mm:ss", 21),
        ("m/d/yy h:mm", 22),
        ("#,##0_);(#,##0)", 37),
        ("#,##0_);[Red](#,##0)", 38),
        ("#,##0.00_);(#,##0.00)", 39),
        ("#,##0.00_);[Red](#,##0.00)", 40),
        ("_(* #,##0_);_(* \\(#,##0\\);_(* \"-\"_);_(@_)", 41),
        (
            "_(\"$\"* #,##0_);_(\"$\"* \\(#,##0\\);_(\"$\"* \"-\"_);_(@_)",
            42,
        ),
        ("_(* #,##0.00_);_(* \\(#,##0.00\\);_(* \"-\"??_);_(@_)", 43),
        (
            "_(\"$\"* #,##0.00_)_(\"$\"* \\(#,##0.00\\)_(\"$\"* \"-\"??_)_(@_)",
            44,
        ),
        ("mm:ss", 45),
        ("[h]:mm:ss", 46),
        ("mmss.0", 47),
        ("##0.0E+0", 48),
        ("@", 49),
    ]
}

pub fn builtin_id(code: &str) -> Option<i32> {
    for (c, id) in builtin_formats() {
        if *c == code {
            return Some(*id);
        }
    }
    None
}

// Kept: the builtin number-format table is authoritative here even though
// the writer currently emits every format explicitly.
#[allow(dead_code)]
pub fn is_builtin_format(code: &str) -> bool {
    builtin_id(code).is_some()
}

// ---------------------------------------------------------------------------
// Indexed color palette (colors.py COLOR_INDEX)
// ---------------------------------------------------------------------------

pub const COLOR_INDEX: &[&str] = &[
    "00000000", "00FFFFFF", "00FF0000", "0000FF00", "000000FF", "00FFFF00", "00FF00FF", "0000FFFF",
    "00000000", "00FFFFFF", "00FF0000", "0000FF00", "000000FF", "00FFFF00", "00FF00FF", "0000FFFF",
    "00800000", "00008000", "00000080", "00808000", "00800080", "00008080", "00C0C0C0", "00808080",
    "009999FF", "00993366", "00FFFFCC", "00CCFFFF", "00660066", "00FF8080", "000066CC", "00CCCCFF",
    "00000080", "00FF00FF", "00FFFF00", "0000FFFF", "00800080", "00800000", "00008080", "000000FF",
    "0000CCFF", "00CCFFFF", "00CCFFCC", "00FFFF99", "0099CCFF", "00FF99CC", "00CC99FF", "00FFCC99",
    "003366FF", "0033CCCC", "0099CC00", "00FFCC00", "00FF9900", "00FF6600", "00666699", "00969696",
    "00003366", "00339966", "00003300", "00333300", "00993300", "00993366", "00333399", "00333333",
];

// ---------------------------------------------------------------------------
// Differential styles (dxf)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct DxfDesc {
    pub font: Option<FontDesc>,
    pub fill: Option<FillDesc>,
    pub border: Option<BorderDesc>,
    pub num_fmt: Option<(i32, String)>, // id + code for emission
    pub alignment: Option<AlignDesc>,
    pub protection: Option<ProtDesc>,
}

impl DxfDesc {
    pub fn is_empty(&self) -> bool {
        self.font.is_none()
            && self.fill.is_none()
            && self.border.is_none()
            && self.num_fmt.is_none()
            && self.alignment.is_none()
            && self.protection.is_none()
    }

    pub fn emit(&self, out: &mut Vec<u8>) {
        push_str(out, "<dxf>");
        if let Some(f) = self.font.as_ref() {
            f.emit(out);
        }
        if let Some((id, ref code)) = self.num_fmt {
            push_str(out, "<numFmt numFmtId=\"");
            push_i32(out, id);
            push_str(out, "\" formatCode=\"");
            esc_attr(code, out);
            push_str(out, "\" />");
        }
        if let Some(f) = self.fill.as_ref() {
            f.emit(out);
        }
        if let Some(a) = self.alignment.as_ref() {
            a.emit(out);
        }
        if let Some(b) = self.border.as_ref() {
            b.emit(out);
        }
        if let Some(p) = self.protection.as_ref() {
            p.emit(out);
        }
        push_str(out, "</dxf>");
    }
}

// ---------------------------------------------------------------------------
// Named style record
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct NamedStyleRec {
    pub name: String,
    pub builtin_id: Option<i32>,
    pub hidden: bool,
    pub style: StyleArray,
    pub alignment: Option<AlignDesc>,
    pub protection: Option<ProtDesc>,
}

// ---------------------------------------------------------------------------
// Pool
// ---------------------------------------------------------------------------

struct Pool<T: Eq + Hash + Clone> {
    map: AHashMap<T, u32>,
    items: Vec<T>,
}

impl<T: Eq + Hash + Clone> Pool<T> {
    fn new() -> Self {
        Pool {
            map: AHashMap::new(),
            items: Vec::new(),
        }
    }
    fn with_capacity(n: usize) -> Self {
        Pool {
            map: AHashMap::with_capacity(n),
            items: Vec::with_capacity(n),
        }
    }
    fn seed(&mut self, item: T) -> u32 {
        let id = self.items.len() as u32;
        self.map.insert(item.clone(), id);
        self.items.push(item);
        id
    }
    fn add(&mut self, item: T) -> u32 {
        if let Some(&id) = self.map.get(&item) {
            return id;
        }
        let id = self.items.len() as u32;
        self.map.insert(item.clone(), id);
        self.items.push(item);
        id
    }
    fn len(&self) -> usize {
        self.items.len()
    }
}

// ---------------------------------------------------------------------------
// StyleEngine
// ---------------------------------------------------------------------------

pub struct StyleEngine {
    fonts: Pool<FontDesc>,
    fills: Pool<FillDesc>,
    borders: Pool<BorderDesc>,
    alignments: Pool<AlignDesc>,
    protections: Pool<ProtDesc>,
    number_formats: Pool<String>,
    cell_styles: Pool<StyleArray>,
    named_styles: Vec<NamedStyleRec>,
    named_by_name: AHashMap<String, u32>,
    dxfs: Pool<DxfDesc>,
    /// Cache StyleDesc → cellXf, keyed by the descriptor itself (same
    /// convention as `Pool`, so a hash collision cannot alias two styles).
    desc_cache: AHashMap<StyleDesc, u32>,
}

impl Default for StyleEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl StyleEngine {
    pub fn new() -> Self {
        let mut eng = StyleEngine {
            fonts: Pool::new(),
            fills: Pool::new(),
            borders: Pool::new(),
            alignments: Pool::new(),
            protections: Pool::new(),
            number_formats: Pool::new(),
            cell_styles: Pool::with_capacity(256),
            named_styles: Vec::new(),
            named_by_name: AHashMap::new(),
            dxfs: Pool::new(),
            desc_cache: AHashMap::new(),
        };
        eng.bootstrap();
        eng
    }

    /// Ledger 14–16.
    fn bootstrap(&mut self) {
        self.fonts.seed(FontDesc::default_calibri());
        self.fills.seed(FillDesc::none());
        self.fills.seed(FillDesc::gray125());
        self.borders.seed(BorderDesc::default_border());
        self.alignments.seed(AlignDesc::default());
        self.protections.seed(ProtDesc::default());
        self.cell_styles.seed(StyleArray::default());

        // Normal named style (builtinId=0)
        let normal = NamedStyleRec {
            name: "Normal".into(),
            builtin_id: Some(0),
            hidden: false,
            style: StyleArray::default(),
            alignment: None,
            protection: None,
        };
        self.named_by_name.insert("Normal".into(), 0);
        self.named_styles.push(normal);
    }

    pub fn register_named_style(
        &mut self,
        name: &str,
        desc: &StyleDesc,
        builtin_id: Option<i32>,
    ) -> u32 {
        if let Some(&id) = self.named_by_name.get(name) {
            return id;
        }
        let mut arr = StyleArray::default();
        if let Some(f) = desc.font.as_ref() {
            arr.font_id = self.fonts.add(f.clone()) as i32;
        }
        if let Some(f) = desc.fill.as_ref() {
            arr.fill_id = self.fills.add(f.clone()) as i32;
        }
        if let Some(b) = desc.border.as_ref() {
            arr.border_id = self.borders.add(b.clone()) as i32;
        } else {
            // NamedStyle default Border() is empty border (not DEFAULT_BORDER)
            arr.border_id = self.borders.add(BorderDesc::default()) as i32;
        }
        if let Some(fmt) = desc.num_fmt.as_ref() {
            arr.num_fmt_id = self.resolve_num_fmt(fmt);
        }
        let align = desc.alignment.clone().filter(|a| !a.is_default());
        let prot = desc.protection.clone().filter(|p| !p.is_default());
        if let Some(a) = align.as_ref() {
            arr.alignment_id = self.alignments.add(a.clone()) as i32;
        }
        if let Some(p) = prot.as_ref() {
            arr.protection_id = self.protections.add(p.clone()) as i32;
        }
        let xf_id = self.named_styles.len() as u32;
        arr.xf_id = xf_id as i32;
        // Note: named style's internal xfId is its own index; cellStyleXfs entry uses components
        // but as_xf clears xfId on the xf element. The StyleArray for cells keeps xfId.

        let rec = NamedStyleRec {
            name: name.into(),
            builtin_id,
            hidden: false,
            style: StyleArray {
                font_id: arr.font_id,
                fill_id: arr.fill_id,
                border_id: arr.border_id,
                num_fmt_id: arr.num_fmt_id,
                protection_id: arr.protection_id,
                alignment_id: arr.alignment_id,
                pivot_button: 0,
                quote_prefix: 0,
                xf_id: xf_id as i32,
            },
            alignment: align,
            protection: prot,
        };
        self.named_by_name.insert(name.into(), xf_id);
        self.named_styles.push(rec);
        xf_id
    }

    pub fn resolve_num_fmt(&mut self, code: &str) -> i32 {
        if let Some(id) = builtin_id(code) {
            return id;
        }
        self.number_formats.add(code.to_string()) as i32 + BUILTIN_FORMATS_MAX_SIZE
    }

    /// Full StyleDesc → cellXf index (deduped).
    pub fn resolve(&mut self, desc: &StyleDesc) -> u32 {
        if let Some(&xf_idx) = self.desc_cache.get(desc) {
            return xf_idx;
        }

        let mut arr = StyleArray::default();

        if let Some(name) = desc.named_style.as_ref() {
            if let Some(&nid) = self.named_by_name.get(name) {
                let ns = &self.named_styles[nid as usize];
                arr = ns.style;
                // allow overrides below
            }
        }

        if let Some(f) = desc.font.as_ref() {
            arr.font_id = self.fonts.add(f.clone()) as i32;
        }
        if let Some(f) = desc.fill.as_ref() {
            arr.fill_id = self.fills.add(f.clone()) as i32;
        }
        if let Some(b) = desc.border.as_ref() {
            arr.border_id = self.borders.add(b.clone()) as i32;
        }
        if let Some(fmt) = desc.num_fmt.as_ref() {
            arr.num_fmt_id = self.resolve_num_fmt(fmt);
        }
        if let Some(a) = desc.alignment.as_ref() {
            if a.is_default() {
                arr.alignment_id = 0;
            } else {
                arr.alignment_id = self.alignments.add(a.clone()) as i32;
            }
        }
        if let Some(p) = desc.protection.as_ref() {
            if p.is_default() {
                arr.protection_id = 0;
            } else {
                arr.protection_id = self.protections.add(p.clone()) as i32;
            }
        }
        if desc.quote_prefix {
            arr.quote_prefix = 1;
        }
        if desc.pivot_button {
            arr.pivot_button = 1;
        }

        let xf_idx = self.cell_styles.add(arr);
        self.desc_cache.insert(desc.clone(), xf_idx);
        xf_idx
    }

    /// Access to internal pools for append-only splice.
    pub fn fonts(&self) -> &[FontDesc] {
        &self.fonts.items
    }
    pub fn fills(&self) -> &[FillDesc] {
        &self.fills.items
    }
    pub fn borders(&self) -> &[BorderDesc] {
        &self.borders.items
    }
    pub fn cell_xfs(&self) -> &[StyleArray] {
        &self.cell_styles.items
    }

    /// Register CF differential style → dxfId (rule 19).
    pub fn register_dxf(&mut self, dxf: DxfDesc) -> Option<u32> {
        if dxf.is_empty() {
            return None;
        }
        Some(self.dxfs.add(dxf))
    }

    pub fn cell_xf_count(&self) -> usize {
        self.cell_styles.len()
    }
    pub fn font_count(&self) -> usize {
        self.fonts.len()
    }
    pub fn fill_count(&self) -> usize {
        self.fills.len()
    }
    pub fn border_count(&self) -> usize {
        self.borders.len()
    }
    pub fn custom_num_fmt_count(&self) -> usize {
        self.number_formats.len()
    }
    pub fn dxf_count(&self) -> usize {
        self.dxfs.len()
    }
    pub fn named_style_count(&self) -> usize {
        self.named_styles.len()
    }

    // -----------------------------------------------------------------------
    // styles.xml emission (write_stylesheet analogue)
    // -----------------------------------------------------------------------

    pub fn emit_styles_xml(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(16 * 1024);
        push_str(
            &mut out,
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        );
        push_str(
            &mut out,
            r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">"#,
        );

        // numFmts — custom only, ids from 164
        push_str(&mut out, "<numFmts count=\"");
        push_u32(&mut out, self.number_formats.len() as u32);
        push_str(&mut out, "\">");
        for (i, code) in self.number_formats.items.iter().enumerate() {
            let id = i as i32 + BUILTIN_FORMATS_MAX_SIZE;
            push_str(&mut out, "<numFmt numFmtId=\"");
            push_i32(&mut out, id);
            push_str(&mut out, "\" formatCode=\"");
            esc_attr(code, &mut out);
            push_str(&mut out, "\" />");
        }
        push_str(&mut out, "</numFmts>");

        // fonts
        push_str(&mut out, "<fonts count=\"");
        push_u32(&mut out, self.fonts.len() as u32);
        push_str(&mut out, "\">");
        for f in &self.fonts.items {
            f.emit(&mut out);
        }
        push_str(&mut out, "</fonts>");

        // fills
        push_str(&mut out, "<fills count=\"");
        push_u32(&mut out, self.fills.len() as u32);
        push_str(&mut out, "\">");
        for f in &self.fills.items {
            f.emit(&mut out);
        }
        push_str(&mut out, "</fills>");

        // borders
        push_str(&mut out, "<borders count=\"");
        push_u32(&mut out, self.borders.len() as u32);
        push_str(&mut out, "\">");
        for b in &self.borders.items {
            b.emit(&mut out);
        }
        push_str(&mut out, "</borders>");

        // cellStyleXfs from named styles
        push_str(&mut out, "<cellStyleXfs count=\"");
        push_u32(&mut out, self.named_styles.len() as u32);
        push_str(&mut out, "\">");
        for ns in &self.named_styles {
            // as_xf: no xfId, no pivot/quote; alignment/protection if non-default
            push_str(&mut out, "<xf numFmtId=\"");
            push_i32(&mut out, ns.style.num_fmt_id);
            push_str(&mut out, "\" fontId=\"");
            push_i32(&mut out, ns.style.font_id);
            push_str(&mut out, "\" fillId=\"");
            push_i32(&mut out, ns.style.fill_id);
            push_str(&mut out, "\" borderId=\"");
            push_i32(&mut out, ns.style.border_id);
            out.push(b'"');
            if ns.alignment.is_some() {
                push_str(&mut out, " applyAlignment=\"1\"");
            }
            if ns.protection.is_some() {
                push_str(&mut out, " applyProtection=\"1\"");
            }
            if ns.alignment.is_some() || ns.protection.is_some() {
                push_str(&mut out, ">");
                if let Some(a) = ns.alignment.as_ref() {
                    a.emit(&mut out);
                }
                if let Some(p) = ns.protection.as_ref() {
                    p.emit(&mut out);
                }
                push_str(&mut out, "</xf>");
            } else {
                push_str(&mut out, " />");
            }
        }
        push_str(&mut out, "</cellStyleXfs>");

        // cellXfs
        push_str(&mut out, "<cellXfs count=\"");
        push_u32(&mut out, self.cell_styles.len() as u32);
        push_str(&mut out, "\">");
        for st in &self.cell_styles.items {
            // Inherited named style (cellStyleXf) this xf builds on. xf_id is the
            // index into named_styles (== cellStyleXfs order).
            let ns = self
                .named_styles
                .get(st.xf_id as usize)
                .unwrap_or(&self.named_styles[0]);
            push_str(&mut out, "<xf numFmtId=\"");
            push_i32(&mut out, st.num_fmt_id);
            push_str(&mut out, "\" fontId=\"");
            push_i32(&mut out, st.font_id);
            push_str(&mut out, "\" fillId=\"");
            push_i32(&mut out, st.fill_id);
            push_str(&mut out, "\" borderId=\"");
            push_i32(&mut out, st.border_id);
            out.push(b'"');
            // Apply flags: emitted ="1" only when the component differs from the
            // named style this xf inherits from — never unconditionally, or
            // inherited components would be re-applied by Excel. applyAlignment /
            // applyProtection stay gated on presence (openpyxl behaviour).
            if st.num_fmt_id != ns.style.num_fmt_id {
                push_str(&mut out, " applyNumberFormat=\"1\"");
            }
            if st.font_id != ns.style.font_id {
                push_str(&mut out, " applyFont=\"1\"");
            }
            if st.fill_id != ns.style.fill_id {
                push_str(&mut out, " applyFill=\"1\"");
            }
            if st.border_id != ns.style.border_id {
                push_str(&mut out, " applyBorder=\"1\"");
            }
            if st.alignment_id != 0 {
                push_str(&mut out, " applyAlignment=\"1\"");
            }
            if st.protection_id != 0 {
                push_str(&mut out, " applyProtection=\"1\"");
            }
            // openpyxl emits pivotButton/quotePrefix from StyleArray (0/1)
            push_str(&mut out, " pivotButton=\"");
            push_i32(&mut out, st.pivot_button);
            push_str(&mut out, "\" quotePrefix=\"");
            push_i32(&mut out, st.quote_prefix);
            push_str(&mut out, "\" xfId=\"");
            push_i32(&mut out, st.xf_id);
            out.push(b'"');

            let has_align = st.alignment_id != 0;
            let has_prot = st.protection_id != 0;
            if has_align || has_prot {
                push_str(&mut out, ">");
                if has_align {
                    let a = &self.alignments.items[st.alignment_id as usize];
                    a.emit(&mut out);
                }
                if has_prot {
                    let p = &self.protections.items[st.protection_id as usize];
                    p.emit(&mut out);
                }
                push_str(&mut out, "</xf>");
            } else {
                push_str(&mut out, " />");
            }
        }
        push_str(&mut out, "</cellXfs>");

        // cellStyles
        push_str(&mut out, "<cellStyles count=\"");
        push_u32(&mut out, self.named_styles.len() as u32);
        push_str(&mut out, "\">");
        for (i, ns) in self.named_styles.iter().enumerate() {
            push_str(&mut out, "<cellStyle name=\"");
            esc_attr(&ns.name, &mut out);
            push_str(&mut out, "\" xfId=\"");
            push_u32(&mut out, i as u32);
            out.push(b'"');
            if let Some(bid) = ns.builtin_id {
                push_str(&mut out, " builtinId=\"");
                push_i32(&mut out, bid);
                out.push(b'"');
            }
            push_str(&mut out, " hidden=\"");
            out.push(if ns.hidden { b'1' } else { b'0' });
            push_str(&mut out, "\" />");
        }
        push_str(&mut out, "</cellStyles>");

        // dxfs
        if !self.dxfs.items.is_empty() {
            push_str(&mut out, "<dxfs count=\"");
            push_u32(&mut out, self.dxfs.len() as u32);
            push_str(&mut out, "\">");
            for d in &self.dxfs.items {
                d.emit(&mut out);
            }
            push_str(&mut out, "</dxfs>");
        }

        // tableStyles (defaults)
        push_str(
            &mut out,
            r#"<tableStyles count="0" defaultTableStyle="TableStyleMedium9" defaultPivotStyle="PivotStyleLight16" />"#,
        );

        // indexed colors
        push_str(&mut out, "<colors><indexedColors>");
        for c in COLOR_INDEX {
            push_str(&mut out, "<rgbColor rgb=\"");
            push_str(&mut out, c);
            push_str(&mut out, "\" />");
        }
        push_str(&mut out, "</indexedColors></colors>");

        push_str(&mut out, "</styleSheet>");
        out
    }
}

// silence unused import warning for esc_text in this module (used by rich text)
#[allow(dead_code)]
fn _use_esc_text(s: &str, o: &mut Vec<u8>) {
    esc_text(s, o);
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::{assert_eq, assert_ne};

    #[test]
    fn bootstrap_ledger_14_16() {
        let eng = StyleEngine::new();
        assert_eq!(eng.cell_xf_count(), 1);
        assert_eq!(eng.font_count(), 1);
        assert_eq!(eng.fill_count(), 2);
        assert_eq!(eng.border_count(), 1);
        assert_eq!(eng.named_style_count(), 1);
        assert_eq!(eng.custom_num_fmt_count(), 0);
        let xml = String::from_utf8(eng.emit_styles_xml()).unwrap();
        assert!(xml.contains("gray125"));
        assert!(xml.contains("Normal"));
        assert!(xml.contains("cellXfs"));
    }

    #[test]
    fn resolve_dedup_and_custom_numfmt() {
        let mut eng = StyleEngine::new();
        let s1 = eng.resolve(&StyleDesc {
            font: Some(FontDesc::simple("Arial", 14.0, true, Some("FF0000"))),
            num_fmt: Some("0.00".into()),
            ..Default::default()
        });
        let s1b = eng.resolve(&StyleDesc {
            font: Some(FontDesc::simple("Arial", 14.0, true, Some("FF0000"))),
            num_fmt: Some("0.00".into()),
            ..Default::default()
        });
        assert_eq!(s1, s1b);
        let s2 = eng.resolve(&StyleDesc {
            num_fmt: Some("\"USD\"#,##0.00".into()),
            ..Default::default()
        });
        assert_ne!(s1, s2);
        assert!(eng.custom_num_fmt_count() >= 1);
        // builtin 0.00 not re-emitted
        let xml = String::from_utf8(eng.emit_styles_xml()).unwrap();
        assert!(xml.contains("numFmtId=\"164\"") || xml.contains("numFmtId=\"164\""));
        assert!(xml.contains("USD"));
    }

    #[test]
    fn dense_styles_collapse() {
        let mut eng = StyleEngine::new();
        let mut idxs = Vec::new();
        // Match silo B palette: force 200 distinct StyleArrays via unique custom numFmt.
        for i in 0..200u32 {
            let fg = format!(
                "{:02X}{:02X}{:02X}",
                ((i * 37) % 256) as u8,
                ((i * 59) % 256) as u8,
                ((i * 97) % 256) as u8
            );
            idxs.push(eng.resolve(&StyleDesc {
                fill: Some(FillDesc::solid(&fg)),
                font: Some(FontDesc::simple(
                    "Arial",
                    10.0 + (i % 5) as f64,
                    i % 2 == 0,
                    Some(&fg),
                )),
                num_fmt: Some(format!("0.0\"d{i}\"")),
                ..Default::default()
            }));
        }
        assert_eq!(idxs.len(), 200);
        // 200 unique + xf0
        assert_eq!(eng.cell_xf_count(), 201);
    }

    #[test]
    fn unique_styles_linear() {
        let mut eng = StyleEngine::new();
        for i in 0..1000u32 {
            let fg = format!(
                "{:02X}{:02X}{:02X}",
                (i & 0xFF) as u8,
                ((i >> 8) & 0xFF) as u8,
                0
            );
            eng.resolve(&StyleDesc {
                fill: Some(FillDesc::solid(&fg)),
                num_fmt: Some(format!("0.0\"x{i}\"")),
                ..Default::default()
            });
        }
        assert_eq!(eng.cell_xf_count(), 1001);
    }

    #[test]
    fn dxf_register() {
        let mut eng = StyleEngine::new();
        let id = eng.register_dxf(DxfDesc {
            fill: Some(FillDesc::solid("FFC7CE")),
            ..Default::default()
        });
        assert_eq!(id, Some(0));
        assert_eq!(eng.dxf_count(), 1);
        let xml = String::from_utf8(eng.emit_styles_xml()).unwrap();
        assert!(xml.contains("<dxfs"));
    }

    #[test]
    fn omit_s_zero() {
        let mut eng = StyleEngine::new();
        let z = eng.resolve(&StyleDesc::default());
        assert_eq!(z, 0);
    }

    // -----------------------------------------------------------------------
    // T1-5: theme tint, font effects, gradient stops, apply flags
    // -----------------------------------------------------------------------

    #[test]
    fn theme_tint_emitted() {
        let mut eng = StyleEngine::new();
        let mut font = FontDesc::default_calibri();
        font.color = Some(ColorSpec::theme_tinted(4, -0.5));
        eng.resolve(&StyleDesc {
            font: Some(font),
            ..Default::default()
        });
        let xml = String::from_utf8(eng.emit_styles_xml()).unwrap();
        // tint emitted on theme colors only when non-zero
        assert!(xml.contains(r#"<color theme="4" tint="-0.5" />"#));
        assert!(xml.contains(r#"<color theme="1" />"#));
        assert!(!xml.contains(r#"<color theme="1" tint="0" />"#));
    }

    #[test]
    fn font_effects_emitted() {
        let mut eng = StyleEngine::new();
        let mut font = FontDesc::default_calibri();
        font.outline = Some(true);
        font.shadow = Some(true);
        font.condense = Some(true);
        font.extend = Some(true);
        eng.resolve(&StyleDesc {
            font: Some(font),
            ..Default::default()
        });
        let xml = String::from_utf8(eng.emit_styles_xml()).unwrap();
        // valueless empty elements in OOXML order after <strike/>
        assert!(xml.contains("<outline/><shadow/><condense/><extend/>"));
    }

    #[test]
    fn gradient_fill_emitted() {
        let mut eng = StyleEngine::new();
        eng.resolve(&StyleDesc {
            fill: Some(FillDesc::Gradient {
                kind: GradientKind::linear(90.0),
                stops: vec![
                    GradientStop::new(0.0, ColorSpec::from_rgb_hex("FFFF0000")),
                    GradientStop::new(1.0, ColorSpec::from_rgb_hex("00FF0000")),
                ],
            }),
            ..Default::default()
        });
        eng.resolve(&StyleDesc {
            fill: Some(FillDesc::Gradient {
                kind: GradientKind::path(0.0, 1.0, 0.0, 0.5),
                stops: vec![GradientStop::new(0.5, ColorSpec::from_rgb_hex("FF0000"))],
            }),
            ..Default::default()
        });
        let xml = String::from_utf8(eng.emit_styles_xml()).unwrap();
        assert!(xml.contains(
            r#"<gradientFill type="linear" degree="90"><stop position="0"><color rgb="FFFF0000" /></stop><stop position="1"><color rgb="00FF0000" /></stop></gradientFill>"#
        ));
        // path emits extents instead of degree
        assert!(xml.contains(r#"<gradientFill type="path" right="1" bottom="0.5">"#));
        assert!(xml.contains(r#"<stop position="0.5"><color rgb="00FF0000" /></stop>"#));
    }

    #[test]
    fn apply_flags_emitted() {
        let mut eng = StyleEngine::new();
        eng.resolve(&StyleDesc {
            font: Some(FontDesc::simple("Arial", 12.0, true, Some("FF0000"))),
            fill: Some(FillDesc::solid("FFFF00")),
            border: Some(BorderDesc::thin_all("FF0000")),
            num_fmt: Some("0.00".into()),
            alignment: Some(AlignDesc {
                horizontal: Some("center".into()),
                ..Default::default()
            }),
            ..Default::default()
        });
        let xml = String::from_utf8(eng.emit_styles_xml()).unwrap();
        assert!(xml.contains("applyNumberFormat=\"1\""));
        assert!(xml.contains("applyFont=\"1\""));
        assert!(xml.contains("applyFill=\"1\""));
        assert!(xml.contains("applyBorder=\"1\""));
        assert!(xml.contains("applyAlignment=\"1\""));
    }

    #[test]
    fn tint_dedup_collision() {
        // Two styles differing ONLY in the tint must produce two distinct xf
        // records — the pool hash must include the tint.
        let mut eng = StyleEngine::new();
        let mk = |tint: f64| {
            let mut font = FontDesc::default_calibri();
            font.color = Some(ColorSpec::theme_tinted(4, tint));
            StyleDesc {
                font: Some(font),
                ..Default::default()
            }
        };
        let a = eng.resolve(&mk(-0.5));
        let b = eng.resolve(&mk(0.5));
        assert_ne!(a, b);
        // identical tinted color still dedups
        let c = eng.resolve(&mk(-0.5));
        assert_eq!(a, c);
        assert_eq!(eng.cell_xf_count(), 3); // xf0 + two distinct
    }

    #[test]
    fn byte_stability_untouched_features() {
        // A workbook that uses none of the four new capabilities must emit the
        // exact bytes it produces today: no tint, no effects, no gradient, and
        // no apply-flag divergence from the inherited named style.
        let mut eng = StyleEngine::new();
        eng.register_named_style(
            "Foo",
            &StyleDesc {
                fill: Some(FillDesc::solid("FF0000")),
                ..Default::default()
            },
            None,
        );
        let idx = eng.resolve(&StyleDesc {
            named_style: Some("Foo".into()),
            ..Default::default()
        });
        assert_eq!(idx, 1);
        let xml = String::from_utf8(eng.emit_styles_xml()).unwrap();

        // xf records carry no apply* attributes when nothing diverges
        assert!(xml.contains(r#"<xf numFmtId="0" fontId="0" fillId="0" borderId="0" pivotButton="0" quotePrefix="0" xfId="0" />"#));
        assert!(xml.contains(r#"<xf numFmtId="0" fontId="0" fillId="2" borderId="1" pivotButton="0" quotePrefix="0" xfId="1" />"#));

        // none of the four new capabilities may leak into the output
        assert!(!xml.contains("applyNumberFormat="));
        assert!(!xml.contains("applyFont="));
        assert!(!xml.contains("applyFill="));
        assert!(!xml.contains("applyBorder="));
        assert!(!xml.contains("tint="));
        assert!(!xml.contains("<outline"));
        assert!(!xml.contains("<shadow"));
        assert!(!xml.contains("<condense"));
        assert!(!xml.contains("<extend"));
        assert!(!xml.contains("gradientFill"));

        // bootstrap component records unchanged
        assert!(xml.contains(
            r#"<font><name val="Calibri" /><family val="2" /><color theme="1" /><sz val="11.0" /><scheme val="minor" /></font>"#
        ));
        assert!(xml.contains(r#"<fill><patternFill /></fill>"#));
        assert!(xml.contains(r#"<fill><patternFill patternType="gray125" /></fill>"#));
        assert!(xml.contains(r#"<border><left /><right /><top /><bottom /><diagonal /></border>"#));
    }
}
