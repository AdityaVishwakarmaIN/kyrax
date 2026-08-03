// structfeat.rs — structural extensions for the fast xlsx reader:
// merged cells, defined names, tables, hyperlinks, comments.
//
// All extraction is additive to the value path. mergeCells / hyperlinks / tableParts
// are scanned from the sheet-XML TAIL (the already-inflated buffer, after </sheetData>),
// serial, no re-inflate, no second pass over sheetData. Defined names come from
// workbook.xml; tables & comments are resolved through sheetN.xml.rels and inflated.
//
// Rules cited to temp-openpyxl (openpyxl 3.1.5): worksheet/merge.py, hyperlink.py,
// table.py, workbook/defined_name.py, comments/comment_sheet.py, cell/text.py.

use std::collections::HashMap;
use std::sync::Arc;

// ----------------------------------------------------------------------------
// Data model (DESIGN sections 1-5).
// ----------------------------------------------------------------------------
#[derive(Clone, Copy, Debug)]
pub struct CellRange {
    pub r0: u32,
    pub c0: u32,
    pub r1: u32,
    pub c1: u32,
} // 0-based, inclusive

#[derive(Clone, Debug)]
pub enum Scope {
    Global,
    Sheet(u32),
} // Sheet = 0-based workbook sheet index (localSheetId, positional)

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NameKind {
    Range,
    Constant,
    Formula,
}

#[derive(Clone, Debug)]
pub struct DefinedName {
    pub name: String,
    pub scope: Scope,
    pub value: String, // raw, verbatim (the contract)
    pub hidden: bool,
    pub reserved: Option<String>, // e.g. "Print_Area" (from _xlnm.<Name>)
    pub external: bool,
    pub kind: NameKind,
}

#[derive(Clone, Debug)]
pub enum LinkTarget {
    External(String), // resolved url/path from r:id rel
    Internal,         // in-workbook, see `location`
}

#[derive(Clone, Debug)]
pub struct Hyperlink {
    pub ref_: CellRange, // kept as a RANGE (not exploded to cells)
    pub target: LinkTarget,
    pub location: Option<String>,
    pub display: Option<String>,
    pub tooltip: Option<String>,
}

#[derive(Clone, Debug)]
pub struct TableColumn {
    pub name: String,
    pub totals_fn: Option<String>,
    pub totals_label: Option<String>,
    pub calc_formula: Option<String>,
}

#[derive(Clone, Debug)]
pub struct TableStyle {
    pub name: String,
    pub show_first_col: bool,
    pub show_last_col: bool,
    pub show_row_stripes: bool,
    pub show_col_stripes: bool,
}

#[derive(Clone, Debug)]
pub struct Table {
    pub name: String,
    pub display_name: String,
    pub ref_: CellRange,
    pub header_row_count: u32,
    pub totals_row_count: u32,
    pub columns: Vec<TableColumn>,
    pub style: Option<TableStyle>,
    pub sheet: u32,
}

#[derive(Clone, Debug)]
pub struct Comment {
    pub row: u32, // 0-based
    pub col: u32, // 0-based
    pub author_id: u32,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct SheetComments {
    pub authors: Vec<String>,
    pub comments: Vec<Comment>,
    /// True when this sheet also has a threadedComments part (legacy is Excel mirror).
    pub legacy_is_mirror: bool,
}

// ----------------------------------------------------------------------------
// Stream C — charts, pivots, VBA, threaded comments
// ----------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChartType {
    Bar,
    Line,
    Pie,
    Scatter,
    Area,
    Radar,
    Bubble,
    Doughnut,
    Other(String),
}

impl ChartType {
    pub fn as_str(&self) -> &str {
        match self {
            ChartType::Bar => "bar",
            ChartType::Line => "line",
            ChartType::Pie => "pie",
            ChartType::Scatter => "scatter",
            ChartType::Area => "area",
            ChartType::Radar => "radar",
            ChartType::Bubble => "bubble",
            ChartType::Doughnut => "doughnut",
            ChartType::Other(s) => s.as_str(),
        }
    }

    fn from_tag_local(local: &str) -> ChartType {
        let base = local.strip_suffix("Chart").unwrap_or(local);
        let base = base.strip_suffix("3D").unwrap_or(base);
        match base {
            "bar" => ChartType::Bar,
            "line" => ChartType::Line,
            "pie" => ChartType::Pie,
            "scatter" => ChartType::Scatter,
            "area" => ChartType::Area,
            "radar" => ChartType::Radar,
            "bubble" => ChartType::Bubble,
            "doughnut" => ChartType::Doughnut,
            _ => ChartType::Other(local.to_string()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct AnchorCell {
    pub col: u32,
    pub row: u32,
}

#[derive(Clone, Debug)]
pub enum ChartAnchor {
    TwoCell { from: AnchorCell, to: AnchorCell },
    OneCell { from: AnchorCell },
    Absolute,
    Unknown,
}

impl ChartAnchor {
    pub fn kind_str(&self) -> &'static str {
        match self {
            ChartAnchor::TwoCell { .. } => "twoCell",
            ChartAnchor::OneCell { .. } => "oneCell",
            ChartAnchor::Absolute => "absolute",
            ChartAnchor::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SeriesMeta {
    pub title_ref: Option<String>,
    pub title_cache: Option<Vec<String>>,
    pub categories_ref: Option<String>,
    pub categories_cache: Option<Vec<String>>,
    pub values_ref: Option<String>,
    pub values_cache: Option<Vec<f64>>,
}

#[derive(Clone, Debug)]
pub struct ChartMeta {
    pub sheet: u32,
    pub part: String,
    pub chart_types: Vec<ChartType>,
    pub title: Option<String>,
    pub series: Vec<SeriesMeta>,
    pub x_axis_title: Option<String>,
    pub y_axis_title: Option<String>,
    pub anchor: ChartAnchor,
}

// ----------------------------------------------------------------------------
// Images: read-back of `xl/drawings/drawingN.xml` pic anchors + media bytes.
// ----------------------------------------------------------------------------

/// A cell-anchor marker as stored in the drawing part: `col`/`row` are 0-based,
/// offsets are EMU. Values are kept verbatim from the file (matching how chart
/// anchors keep raw 0-based cells).
#[derive(Clone, Debug)]
pub struct ReadImageMarker {
    pub col: u32,
    pub col_off: i64,
    pub row: u32,
    pub row_off: i64,
}

/// Placement anchor for a read-back image.
#[derive(Clone, Debug)]
pub enum ReadImageAnchor {
    Absolute {
        x: i64,
        y: i64,
        cx: i64,
        cy: i64,
    },
    OneCell {
        from: ReadImageMarker,
        cx: i64,
        cy: i64,
    },
    TwoCell {
        from: ReadImageMarker,
        to: ReadImageMarker,
        edit_as: Option<String>,
    },
}

impl ReadImageAnchor {
    pub fn kind_str(&self) -> &'static str {
        match self {
            ReadImageAnchor::TwoCell { .. } => "twoCell",
            ReadImageAnchor::OneCell { .. } => "oneCell",
            ReadImageAnchor::Absolute { .. } => "absolute",
        }
    }
}

/// One image read back from a worksheet drawing, gated by `Features::IMAGES`.
/// `bytes` are the raw media part contents (never decoded; STORE in the zip).
#[derive(Clone, Debug)]
pub struct ImageMeta {
    pub sheet: u32,
    pub part: String,
    pub anchor: ReadImageAnchor,
    pub bytes: Arc<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub struct PivotDataField {
    pub name: String,
    pub field_index: u32,
}

#[derive(Clone, Debug)]
pub struct PivotCacheMeta {
    pub part: String,
    pub source_type: String,
    pub worksheet_sheet: Option<String>,
    pub worksheet_ref: Option<String>,
    pub worksheet_name: Option<String>,
    pub field_names: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct PivotTableMeta {
    pub sheet: u32,
    pub name: String,
    pub location_ref: String,
    pub cache_id: u32,
    pub row_fields: Vec<String>,
    pub col_fields: Vec<String>,
    pub data_fields: Vec<PivotDataField>,
    pub cache: PivotCacheMeta,
}

#[derive(Clone, Debug, Default)]
pub struct VbaProject {
    pub present: bool,
    pub part: Option<String>,
    pub bytes: Option<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub struct Person {
    pub id: String,
    pub display_name: String,
    pub user_id: Option<String>,
    pub provider_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ThreadedComment {
    pub ref_cell: (u32, u32),
    pub ref_raw: String,
    pub id: String,
    pub person_id: String,
    /// Resolved from persons part when available; empty if unknown.
    pub person_display_name: String,
    pub parent_id: Option<String>,
    pub done: bool,
    pub text: String,
    pub datetime: Option<String>,
}

// Everything surfaced for one workbook (kept for API parity with the prototype).
#[allow(dead_code)]
pub struct Structures {
    pub merges: Vec<CellRange>,
    pub defined_names: Vec<DefinedName>,
    pub hyperlinks: Vec<Hyperlink>,
    pub tables: Vec<Table>,
    pub comments: Option<SheetComments>,
    // phase timings (seconds); filled by the instrumented driver, 0 otherwise
    pub t_tail: f64,
    pub t_rels: f64,
    pub t_names: f64,
    pub t_tables: f64,
    pub t_comments: f64,
}

// ----------------------------------------------------------------------------
// Low-level attribute / helper parsing.
// ----------------------------------------------------------------------------

// Find attribute `name` in a tag-body slice (bytes between `<tag` and `>`).
// Matches ` name="VALUE"`; the leading space disambiguates `id` from `r:id`.
// Uses a stack needle buffer — no heap allocation on the hot per-cell path.
#[inline]
pub fn find_attr<'a>(tag: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
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
fn attr_str(tag: &[u8], name: &[u8], scratch: &mut Vec<u8>) -> Option<String> {
    find_attr(tag, name)
        .map(|raw| String::from_utf8_lossy(super::decode::decode_bytes(raw, scratch)).into_owned())
}

// Parse "L1725:M1725" or "O2" into a 0-based inclusive CellRange.
#[inline]
pub fn parse_range(refr: &[u8]) -> CellRange {
    let (r0, c0, r1, c1) = super::scan::parse_ref_range(refr);
    CellRange { r0, c0, r1, c1 }
}

// Convert 0-based (row,col) to A1 (e.g. 0,11 -> "L1").
pub fn a1(row: u32, col: u32) -> String {
    format!("{}{}", super::formula::index_to_letters(col + 1), row + 1)
}
pub fn range_a1(r: &CellRange) -> String {
    if r.r0 == r.r1 && r.c0 == r.c1 {
        a1(r.r0, r.c0)
    } else {
        format!("{}:{}", a1(r.r0, r.c0), a1(r.r1, r.c1))
    }
}

// Normalize a rels Target to a zip entry path.
//   "/xl/tables/table1.xml"    -> "xl/tables/table1.xml"
//   "../tables/table1.xml"     -> "xl/tables/table1.xml"   (base = xl/worksheets/)
//   "tables/table1.xml"        -> "xl/worksheets/tables/table1.xml"
pub fn resolve_zip_path(base_dir: &str, target: &str) -> String {
    if let Some(rest) = target.strip_prefix('/') {
        return rest.to_string();
    }
    let mut parts: Vec<&str> = base_dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

// ----------------------------------------------------------------------------
// Rels map: sheetN.xml.rels -> { rid -> (target, type, mode) }.
// ----------------------------------------------------------------------------
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum RelKind {
    Hyperlink,
    Table,
    Comments,
    Worksheet,
    Chartsheet,
    Drawing,
    Chart,
    Image,
    PivotTable,
    PivotCacheDef,
    VbaProject,
    ThreadedComment,
    Person,
    Other,
}
pub struct RelInfo {
    pub target: String,
    pub kind: RelKind,
    #[allow(dead_code)]
    pub external: bool,
}
pub type RelMap = HashMap<String, RelInfo>;

// Classify a Relationship Type URL by its trailing segment without allocating.
#[inline]
fn rel_kind(tag: &[u8]) -> RelKind {
    match find_attr(tag, b"Type") {
        Some(t) if t.ends_with(b"/hyperlink") => RelKind::Hyperlink,
        Some(t) if t.ends_with(b"/table") => RelKind::Table,
        Some(t) if t.ends_with(b"/comments") => RelKind::Comments,
        Some(t) if t.ends_with(b"/worksheet") => RelKind::Worksheet,
        Some(t) if t.ends_with(b"/chartsheet") => RelKind::Chartsheet,
        Some(t) if t.ends_with(b"/drawing") => RelKind::Drawing,
        Some(t) if t.ends_with(b"/chart") => RelKind::Chart,
        Some(t) if t.ends_with(b"/image") => RelKind::Image,
        Some(t) if t.ends_with(b"/pivotTable") => RelKind::PivotTable,
        Some(t) if t.ends_with(b"/pivotCacheDefinition") => RelKind::PivotCacheDef,
        Some(t) if t.ends_with(b"/vbaProject") => RelKind::VbaProject,
        Some(t) if t.ends_with(b"/threadedComment") => RelKind::ThreadedComment,
        Some(t) if t.ends_with(b"/person") => RelKind::Person,
        _ => RelKind::Other,
    }
}

pub fn parse_rels(xml: &[u8]) -> RelMap {
    let mut map = HashMap::new();
    let mut scratch = Vec::new();
    let mut i = 0usize;
    let n = xml.len();
    // Namespace-tolerant: <Relationship …> or <ns0:Relationship …>
    while let Some(start) = find_open_local(xml, i, b"Relationship") {
        let te = start + memchr::memchr(b'>', &xml[start..n]).unwrap_or(n - start);
        let tag = &xml[start..te];
        let id = attr_str(tag, b"Id", &mut scratch);
        let target = attr_str(tag, b"Target", &mut scratch);
        let kind = rel_kind(tag);
        let mode = find_attr(tag, b"TargetMode")
            .map(|m| m == b"External")
            .unwrap_or(false);
        if let (Some(id), Some(target)) = (id, target) {
            map.insert(
                id,
                RelInfo {
                    target,
                    kind,
                    external: mode,
                },
            );
        }
        i = te + 1;
    }
    map
}

// ----------------------------------------------------------------------------
// 2. MERGED CELLS — tail scan.
// ----------------------------------------------------------------------------
pub fn scan_merges(tail: &[u8]) -> Vec<CellRange> {
    let mut out = Vec::new();
    let mut i = 0usize;
    let n = tail.len();
    // stop at the end of the mergeCells block if present (cheap bound; not required)
    while let Some(o) = memchr::memmem::find(&tail[i..n], b"<mergeCell ") {
        let start = i + o;
        let te = start + memchr::memchr(b'>', &tail[start..n]).unwrap_or(n - start);
        let tag = &tail[start..te];
        if let Some(r) = find_attr(tag, b"ref") {
            out.push(parse_range(r));
        }
        i = te + 1;
    }
    out
}

// ----------------------------------------------------------------------------
// 4. HYPERLINKS — tail scan + rels resolution.
// ----------------------------------------------------------------------------
pub fn scan_hyperlinks(tail: &[u8], rels: &RelMap) -> Vec<Hyperlink> {
    let mut out = Vec::new();
    let mut scratch = Vec::new();
    let mut i = 0usize;
    let n = tail.len();
    // `<hyperlink ` (trailing space) never matches the `<hyperlinks>` container.
    while let Some(o) = memchr::memmem::find(&tail[i..n], b"<hyperlink ") {
        let start = i + o;
        let te = start + memchr::memchr(b'>', &tail[start..n]).unwrap_or(n - start);
        let tag = &tail[start..te];
        let refr = match find_attr(tag, b"ref") {
            Some(r) => parse_range(r),
            None => {
                i = te + 1;
                continue;
            }
        };
        let location = attr_str(tag, b"location", &mut scratch);
        let display = attr_str(tag, b"display", &mut scratch);
        let tooltip = attr_str(tag, b"tooltip", &mut scratch);
        let rid = find_attr(tag, b"r:id").map(|r| String::from_utf8_lossy(r).into_owned());
        let target = match rid.as_deref().and_then(|id| rels.get(id)) {
            Some(rel) => LinkTarget::External(rel.target.clone()),
            None => LinkTarget::Internal, // no rid, or dangling rid -> location-only
        };
        out.push(Hyperlink {
            ref_: refr,
            target,
            location,
            display,
            tooltip,
        });
        i = te + 1;
    }
    out
}

// ----------------------------------------------------------------------------
// 3. DEFINED NAMES — from workbook.xml.
// ----------------------------------------------------------------------------
const RESERVED: &[&str] = &[
    "Print_Area",
    "Print_Titles",
    "_FilterDatabase",
    "Criteria",
    "Extract",
    "Consolidate_Area",
    "Sheet_Title",
];

fn classify_kind(v: &str) -> NameKind {
    let t = v.trim();
    if t.is_empty() {
        return NameKind::Formula;
    }
    let b = t.as_bytes();
    // constant: quoted string, boolean, or a plain number
    if b[0] == b'"' {
        return NameKind::Constant;
    }
    let up = t.to_ascii_uppercase();
    if up == "TRUE" || up == "FALSE" {
        return NameKind::Constant;
    }
    if t.parse::<f64>().is_ok() {
        return NameKind::Constant;
    }
    // formula if it contains a call / operator that a bare range can't
    if t.contains('(') || t.contains('+') || t.contains('*') || t.contains('/') {
        return NameKind::Formula;
    }
    // range-ish: reference chars only (letters/digits/$ ! : , ' space . _)
    let range_like = t.bytes().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(
                c,
                b'$' | b'!' | b':' | b',' | b'\'' | b' ' | b'.' | b'_' | b'-'
            )
    }) && t.bytes().any(|c| c.is_ascii_digit());
    if range_like {
        NameKind::Range
    } else {
        NameKind::Formula
    }
}

/// One workbook sheet entry (order = workbook.xml sheet order).
#[derive(Clone, Debug)]
pub struct SheetMeta {
    pub name: String,
    /// Relationship id (`r:id`), if present.
    pub rid: Option<String>,
    /// Sheet visibility state from workbook.xml `@state` (default visible).
    pub state: super::meta::SheetState,
    /// Worksheet vs chartsheet (resolved from workbook rels Type; default worksheet).
    pub kind: super::meta::SheetKind,
}

// Returns (sheets_in_order, defined_names).
pub fn parse_workbook(xml: &[u8]) -> (Vec<SheetMeta>, Vec<DefinedName>) {
    let mut scratch = Vec::new();
    let n = xml.len();

    // --- <sheets> order (namespace-tolerant: <sheets> or <s:sheets>) ---
    let mut sheets = Vec::new();
    if let Some(so) = find_open_local(xml, 0, b"sheets") {
        let se = find_close_local(xml, so + 1, b"sheets").unwrap_or(n);
        let mut i = so;
        while let Some(start) = find_open_local(xml, i, b"sheet") {
            if start >= se {
                break;
            }
            let te = start + memchr::memchr(b'>', &xml[start..se]).unwrap_or(se - start);
            let tag = &xml[start..te];
            if let Some(name) = attr_str(tag, b"name", &mut scratch) {
                let rid = find_attr(tag, b"r:id").map(|r| String::from_utf8_lossy(r).into_owned());
                let state = match find_attr(tag, b"state") {
                    Some(b"hidden") => super::meta::SheetState::Hidden,
                    Some(b"veryHidden") => super::meta::SheetState::VeryHidden,
                    _ => super::meta::SheetState::Visible,
                };
                sheets.push(SheetMeta {
                    name,
                    rid,
                    state,
                    kind: super::meta::SheetKind::Worksheet,
                });
            }
            i = te + 1;
        }
    }

    // --- <definedName ...>VALUE</definedName> ---
    let mut names = Vec::new();
    if let Some(do_) = find_open_local(xml, 0, b"definedNames") {
        let de = find_close_local(xml, do_ + 1, b"definedNames").unwrap_or(n);
        let mut i = do_;
        while let Some(start) = find_open_local(xml, i, b"definedName") {
            if start >= de {
                break;
            }
            let te = start + memchr::memchr(b'>', &xml[start..de]).unwrap_or(de - start);
            let tag = &xml[start..te];
            let self_closing = xml[te - 1] == b'/';
            let name = attr_str(tag, b"name", &mut scratch).unwrap_or_default();
            let local = find_attr(tag, b"localSheetId")
                .and_then(|v| std::str::from_utf8(v).ok()?.parse::<u32>().ok());
            let hidden = find_attr(tag, b"hidden")
                .map(|v| v == b"1" || v == b"true")
                .unwrap_or(false);
            let value = if self_closing {
                String::new()
            } else {
                let ve = find_close_local(xml, te, b"definedName").unwrap_or(de);
                String::from_utf8_lossy(super::decode::decode_bytes(&xml[te + 1..ve], &mut scratch))
                    .into_owned()
            };
            let reserved = name
                .strip_prefix("_xlnm.")
                .and_then(|base| RESERVED.iter().find(|r| **r == base).map(|r| r.to_string()));
            let external = {
                let b = value.as_bytes();
                b.first() == Some(&b'[') && b.get(1).map(|c| c.is_ascii_digit()).unwrap_or(false)
            };
            let kind = classify_kind(&value);
            let scope = match local {
                Some(idx) => Scope::Sheet(idx),
                None => Scope::Global,
            };
            names.push(DefinedName {
                name,
                scope,
                value,
                hidden,
                reserved,
                external,
                kind,
            });
            i = te + 1;
        }
    }
    (sheets, names)
}

// ----------------------------------------------------------------------------
// Namespace-tolerant open/close tag helpers
// ----------------------------------------------------------------------------

/// True if `xml[pos..]` is an open tag with local name `local` (`<local …>` or `<ns:local …>`).
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
    // Truncated chart/drawing XML can pass `from > len`; never slice past the end.
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

/// Local name of an open tag at `pos` (points at `<`).
fn open_tag_local<'a>(xml: &'a [u8], pos: usize) -> Option<&'a [u8]> {
    if !xml.get(pos).copied().eq(&Some(b'<')) {
        return None;
    }
    let after = pos + 1;
    if after >= xml.len() {
        return None;
    }
    let c0 = xml[after];
    if c0 == b'/' || c0 == b'!' || c0 == b'?' {
        return None;
    }
    let rest = &xml[after..];
    let name_end = rest
        .iter()
        .position(|&c| {
            c == b' ' || c == b'>' || c == b'/' || c == b'\n' || c == b'\r' || c == b'\t'
        })
        .unwrap_or(rest.len());
    let name = &rest[..name_end];
    Some(match memchr::memchr(b':', name) {
        Some(c) => &name[c + 1..],
        None => name,
    })
}

/// Text content of the first element with local name `local` under `xml` region.
fn first_elem_text(xml: &[u8], local: &[u8], scratch: &mut Vec<u8>) -> Option<String> {
    let start = find_open_local(xml, 0, local)?;
    let te = start + memchr::memchr(b'>', &xml[start..])?;
    if te > 0 && xml.get(te - 1) == Some(&b'/') {
        return Some(String::new());
    }
    let close = find_close_local(xml, te.saturating_add(1), local)?;
    if te + 1 > close || close > xml.len() {
        return Some(String::new());
    }
    Some(
        String::from_utf8_lossy(super::decode::decode_bytes(&xml[te + 1..close], scratch))
            .into_owned(),
    )
}

/// Concatenate all `<t>` / `<a:t>` run texts inside `region`.
fn concat_t_texts(region: &[u8], scratch: &mut Vec<u8>) -> String {
    let mut text = String::new();
    let mut p = 0usize;
    let bn = region.len();
    // Guard `p > bn` — truncated XML can advance past the end.
    while p < bn {
        let Some(to) = memchr::memmem::find(&region[p..bn], b"<") else {
            break;
        };
        let topen = p + to;
        if !is_open_local(region, topen, b"t") {
            p = topen.saturating_add(1);
            continue;
        }
        let topen_end = topen + memchr::memchr(b'>', &region[topen..bn]).unwrap_or(bn - topen);
        if region.get(topen_end.wrapping_sub(1)) == Some(&b'/') {
            p = topen_end.saturating_add(1);
            continue;
        }
        let tclose = find_close_local(region, topen_end.saturating_add(1), b"t").unwrap_or(bn);
        if topen_end + 1 <= tclose && tclose <= bn {
            let raw = &region[topen_end + 1..tclose];
            text.push_str(&String::from_utf8_lossy(super::decode::decode_bytes(
                raw, scratch,
            )));
        }
        // Advance at least one byte to avoid infinite loops on truncated markup.
        p = tclose.saturating_add(1).max(topen.saturating_add(1));
        if p > bn {
            break;
        }
    }
    text
}

/// First `<f>…</f>` formula text inside region (chart formula refs).
fn first_f_text(region: &[u8], scratch: &mut Vec<u8>) -> Option<String> {
    first_elem_text(region, b"f", scratch).filter(|s| !s.is_empty())
}

/// Parse strCache / numCache pt values ordered by idx (holes filled by sort).
fn parse_str_cache(region: &[u8], scratch: &mut Vec<u8>) -> Option<Vec<String>> {
    let cache_start = find_open_local(region, 0, b"strCache")?;
    let cache_end = find_close_local(region, cache_start + 1, b"strCache").unwrap_or(region.len());
    let body = &region[cache_start..cache_end];
    let mut pts: Vec<(u32, String)> = Vec::new();
    let mut i = 0usize;
    while let Some(start) = find_open_local(body, i, b"pt") {
        let te = start + memchr::memchr(b'>', &body[start..]).unwrap_or(body.len() - start);
        let tag = &body[start..te];
        let idx = find_attr(tag, b"idx")
            .and_then(|v| std::str::from_utf8(v).ok()?.parse().ok())
            .unwrap_or(pts.len() as u32);
        let close = find_close_local(body, te + 1, b"pt").unwrap_or(body.len());
        let inner = &body[te + 1..close];
        let val = first_elem_text(inner, b"v", scratch).unwrap_or_default();
        pts.push((idx, val));
        i = close + 1;
    }
    if pts.is_empty() {
        return None;
    }
    pts.sort_by_key(|(i, _)| *i);
    Some(pts.into_iter().map(|(_, v)| v).collect())
}

fn parse_num_cache(region: &[u8], scratch: &mut Vec<u8>) -> Option<Vec<f64>> {
    let cache_start = find_open_local(region, 0, b"numCache")?;
    let cache_end = find_close_local(region, cache_start + 1, b"numCache").unwrap_or(region.len());
    let body = &region[cache_start..cache_end];
    let mut pts: Vec<(u32, f64)> = Vec::new();
    let mut i = 0usize;
    while let Some(start) = find_open_local(body, i, b"pt") {
        let te = start + memchr::memchr(b'>', &body[start..]).unwrap_or(body.len() - start);
        let tag = &body[start..te];
        let idx = find_attr(tag, b"idx")
            .and_then(|v| std::str::from_utf8(v).ok()?.parse().ok())
            .unwrap_or(pts.len() as u32);
        let close = find_close_local(body, te + 1, b"pt").unwrap_or(body.len());
        let inner = &body[te + 1..close];
        if let Some(vs) = first_elem_text(inner, b"v", scratch) {
            if let Ok(n) = vs.parse::<f64>() {
                pts.push((idx, n));
            }
        }
        i = close + 1;
    }
    if pts.is_empty() {
        return None;
    }
    pts.sort_by_key(|(i, _)| *i);
    Some(pts.into_iter().map(|(_, v)| v).collect())
}

fn parse_ref_and_str_cache(
    region: &[u8],
    scratch: &mut Vec<u8>,
) -> (Option<String>, Option<Vec<String>>) {
    let r = first_f_text(region, scratch);
    let c = parse_str_cache(region, scratch);
    (r, c)
}

fn parse_ref_and_num_cache(
    region: &[u8],
    scratch: &mut Vec<u8>,
) -> (Option<String>, Option<Vec<f64>>) {
    let r = first_f_text(region, scratch);
    let c = parse_num_cache(region, scratch);
    (r, c)
}

fn child_elem_region<'a>(parent: &'a [u8], local: &[u8]) -> Option<&'a [u8]> {
    let start = find_open_local(parent, 0, local)?;
    let te = start + memchr::memchr(b'>', &parent[start..])?;
    if te > 0 && parent.get(te - 1) == Some(&b'/') {
        return Some(&parent[start..te + 1]);
    }
    let close = find_close_local(parent, te + 1, local)?;
    let end = memchr::memchr(b'>', &parent[close..])
        .map(|o| close + o + 1)
        .unwrap_or(parent.len());
    Some(&parent[start..end])
}

fn parse_series_block(ser: &[u8], scratch: &mut Vec<u8>) -> SeriesMeta {
    // title: tx/strRef or tx/v literal
    let mut title_ref = None;
    let mut title_cache = None;
    if let Some(tx) = child_elem_region(ser, b"tx") {
        if let Some(str_ref) = child_elem_region(tx, b"strRef") {
            let (r, c) = parse_ref_and_str_cache(str_ref, scratch);
            title_ref = r;
            title_cache = c;
        } else if let Some(v) = first_elem_text(tx, b"v", scratch) {
            title_ref = Some(format!("lit:{v}"));
        }
    }

    // categories: cat or xVal
    let mut categories_ref = None;
    let mut categories_cache = None;
    let cat_region = child_elem_region(ser, b"cat").or_else(|| child_elem_region(ser, b"xVal"));
    if let Some(cat) = cat_region {
        if let Some(str_ref) = child_elem_region(cat, b"strRef") {
            let (r, c) = parse_ref_and_str_cache(str_ref, scratch);
            categories_ref = r;
            categories_cache = c;
        } else if let Some(num_ref) = child_elem_region(cat, b"numRef") {
            let (r, _) = parse_ref_and_num_cache(num_ref, scratch);
            categories_ref = r;
            // categories stay as strings when only numCache — leave cache None unless strCache
            if let Some(sc) = parse_str_cache(num_ref, scratch) {
                categories_cache = Some(sc);
            }
        }
    }

    // values: val or yVal
    let mut values_ref = None;
    let mut values_cache = None;
    let val_region = child_elem_region(ser, b"val").or_else(|| child_elem_region(ser, b"yVal"));
    if let Some(val) = val_region {
        if let Some(num_ref) = child_elem_region(val, b"numRef") {
            let (r, c) = parse_ref_and_num_cache(num_ref, scratch);
            values_ref = r;
            values_cache = c;
        } else if let Some(str_ref) = child_elem_region(val, b"strRef") {
            let (r, _) = parse_ref_and_str_cache(str_ref, scratch);
            values_ref = r;
        }
    }

    SeriesMeta {
        title_ref,
        title_cache,
        categories_ref,
        categories_cache,
        values_ref,
        values_cache,
    }
}

fn parse_anchor_cell(region: &[u8]) -> Option<AnchorCell> {
    let mut scratch: Vec<u8> = Vec::new();
    let col = first_elem_text(region, b"col", &mut scratch).and_then(|s| s.parse().ok())?;
    let row = first_elem_text(region, b"row", &mut scratch).and_then(|s| s.parse().ok())?;
    Some(AnchorCell { col, row })
}

/// Chart anchors in a drawing part: `(rId, anchor)`.
pub fn parse_drawing_chart_anchors(xml: &[u8]) -> Vec<(String, ChartAnchor)> {
    let mut out = Vec::new();
    let n = xml.len();
    let mut i = 0usize;
    while let Some(o) = memchr::memchr(b'<', &xml[i..]) {
        let pos = i + o;
        let local = match open_tag_local(xml, pos) {
            Some(l) => l,
            None => {
                i = pos + 1;
                continue;
            }
        };
        let anchor_kind = if local == b"twoCellAnchor" {
            "two"
        } else if local == b"oneCellAnchor" {
            "one"
        } else if local == b"absoluteAnchor" {
            "abs"
        } else {
            i = pos + 1;
            continue;
        };
        let te = pos + memchr::memchr(b'>', &xml[pos..n]).unwrap_or(n - pos);
        let close_name = match anchor_kind {
            "two" => b"twoCellAnchor".as_ref(),
            "one" => b"oneCellAnchor".as_ref(),
            _ => b"absoluteAnchor".as_ref(),
        };
        let end = if te > 0 && xml.get(te - 1) == Some(&b'/') {
            (te + 1).min(n)
        } else {
            find_close_local(xml, te.saturating_add(1), close_name)
                .and_then(|c| memchr::memchr(b'>', &xml[c..]).map(|o| c + o + 1))
                .unwrap_or(n)
                .min(n)
        };
        if pos >= end || end > n {
            i = pos.saturating_add(1);
            continue;
        }
        let block = &xml[pos..end];

        // chart r:id inside block
        let mut j = 0usize;
        while let Some(co) = memchr::memmem::find(&block[j..], b"<") {
            let cp = j + co;
            if let Some(cl) = open_tag_local(block, cp) {
                if cl == b"chart" {
                    let cte = cp + memchr::memchr(b'>', &block[cp..]).unwrap_or(block.len() - cp);
                    let ctag = &block[cp..cte];
                    if let Some(rid) = find_attr(ctag, b"r:id") {
                        let rid = String::from_utf8_lossy(rid).into_owned();
                        let anchor = match anchor_kind {
                            "abs" => ChartAnchor::Absolute,
                            "one" => {
                                let from = child_elem_region(block, b"from")
                                    .and_then(parse_anchor_cell)
                                    .unwrap_or(AnchorCell { col: 0, row: 0 });
                                ChartAnchor::OneCell { from }
                            }
                            _ => {
                                let from = child_elem_region(block, b"from")
                                    .and_then(parse_anchor_cell)
                                    .unwrap_or(AnchorCell { col: 0, row: 0 });
                                let to = child_elem_region(block, b"to")
                                    .and_then(parse_anchor_cell)
                                    .unwrap_or(from.clone());
                                ChartAnchor::TwoCell { from, to }
                            }
                        };
                        out.push((rid, anchor));
                    }
                    break;
                }
            }
            j = cp + 1;
        }
        i = end;
    }
    out
}

/// Parse `<from>`/`<to>` marker text into a [`ReadImageMarker`]. Missing or
/// unparseable fields degrade to 0; never fails.
fn parse_image_marker(region: &[u8]) -> ReadImageMarker {
    let mut scratch = Vec::new();
    ReadImageMarker {
        col: first_elem_text(region, b"col", &mut scratch)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0),
        row: first_elem_text(region, b"row", &mut scratch)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0),
        col_off: first_elem_text(region, b"colOff", &mut scratch)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0),
        row_off: first_elem_text(region, b"rowOff", &mut scratch)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0),
    }
}

/// Read an int attribute (`x`/`y`/`cx`/`cy`) off a small element region like
/// `<pos x="1" y="2"/>`. `find_attr` scans the whole region; pos/ext have no
/// nested children, so matching on the full region is safe.
fn elem_attr_i64(region: &[u8], attr: &[u8]) -> Option<i64> {
    let raw = find_attr(region, attr)?;
    std::str::from_utf8(raw).ok()?.trim().parse().ok()
}

/// Image anchors in a drawing part: `(blip r:embed rel-id, anchor)`, one entry
/// per `<pic>` that carries an `<a:blip r:embed>`. Anchors without a blip
/// (charts, shapes, or pics missing a blip) are skipped, as are unparseable
/// or truncated drawing parts — the scan is the same tolerant memchr walk as
/// [`parse_drawing_chart_anchors`] and never panics.
pub fn parse_drawing_image_anchors(xml: &[u8]) -> Vec<(String, ReadImageAnchor)> {
    let mut out = Vec::new();
    let n = xml.len();
    let mut i = 0usize;
    while let Some(o) = memchr::memchr(b'<', &xml[i..]) {
        let pos = i + o;
        let local = match open_tag_local(xml, pos) {
            Some(l) => l,
            None => {
                i = pos + 1;
                continue;
            }
        };
        let anchor_kind = if local == b"twoCellAnchor" {
            "two"
        } else if local == b"oneCellAnchor" {
            "one"
        } else if local == b"absoluteAnchor" {
            "abs"
        } else {
            i = pos + 1;
            continue;
        };
        let te = pos + memchr::memchr(b'>', &xml[pos..n]).unwrap_or(n - pos);
        let open_tag = &xml[pos..te];
        let close_name = match anchor_kind {
            "two" => b"twoCellAnchor".as_ref(),
            "one" => b"oneCellAnchor".as_ref(),
            _ => b"absoluteAnchor".as_ref(),
        };
        let end = if te > 0 && xml.get(te - 1) == Some(&b'/') {
            (te + 1).min(n)
        } else {
            find_close_local(xml, te.saturating_add(1), close_name)
                .and_then(|c| memchr::memchr(b'>', &xml[c..]).map(|o| c + o + 1))
                .unwrap_or(n)
                .min(n)
        };
        if pos >= end || end > n {
            i = pos.saturating_add(1);
            continue;
        }
        let block = &xml[pos..end];

        // A blip's r:embed anywhere in this anchor marks it as an image.
        let mut embed: Option<String> = None;
        let mut j = 0usize;
        while let Some(co) = memchr::memmem::find(&block[j..], b"<") {
            let cp = j + co;
            if let Some(l) = open_tag_local(block, cp) {
                if l == b"blip" {
                    let bte = cp + memchr::memchr(b'>', &block[cp..]).unwrap_or(block.len() - cp);
                    if let Some(r) = find_attr(&block[cp..bte], b"r:embed") {
                        embed = Some(String::from_utf8_lossy(r).into_owned());
                    }
                }
            }
            j = cp + 1;
        }
        let Some(embed) = embed else {
            i = end;
            continue;
        };

        let anchor = match anchor_kind {
            "abs" => ReadImageAnchor::Absolute {
                x: child_elem_region(block, b"pos")
                    .and_then(|r| elem_attr_i64(r, b"x"))
                    .unwrap_or(0),
                y: child_elem_region(block, b"pos")
                    .and_then(|r| elem_attr_i64(r, b"y"))
                    .unwrap_or(0),
                cx: child_elem_region(block, b"ext")
                    .and_then(|r| elem_attr_i64(r, b"cx"))
                    .unwrap_or(0),
                cy: child_elem_region(block, b"ext")
                    .and_then(|r| elem_attr_i64(r, b"cy"))
                    .unwrap_or(0),
            },
            "one" => ReadImageAnchor::OneCell {
                from: child_elem_region(block, b"from")
                    .map(parse_image_marker)
                    .unwrap_or(ReadImageMarker {
                        col: 0,
                        col_off: 0,
                        row: 0,
                        row_off: 0,
                    }),
                cx: child_elem_region(block, b"ext")
                    .and_then(|r| elem_attr_i64(r, b"cx"))
                    .unwrap_or(0),
                cy: child_elem_region(block, b"ext")
                    .and_then(|r| elem_attr_i64(r, b"cy"))
                    .unwrap_or(0),
            },
            _ => {
                let from = child_elem_region(block, b"from")
                    .map(parse_image_marker)
                    .unwrap_or(ReadImageMarker {
                        col: 0,
                        col_off: 0,
                        row: 0,
                        row_off: 0,
                    });
                let to = child_elem_region(block, b"to")
                    .map(parse_image_marker)
                    .unwrap_or_else(|| from.clone());
                ReadImageAnchor::TwoCell {
                    from,
                    to,
                    edit_as: find_attr(open_tag, b"editAs")
                        .map(|v| String::from_utf8_lossy(v).into_owned()),
                }
            }
        };
        out.push((embed, anchor));
        i = end;
    }
    out
}

/// Known plotArea chart type local names (suffix Chart).
const CHART_TYPE_LOCALS: &[&[u8]] = &[
    b"barChart",
    b"bar3DChart",
    b"lineChart",
    b"line3DChart",
    b"pieChart",
    b"pie3DChart",
    b"scatterChart",
    b"areaChart",
    b"area3DChart",
    b"radarChart",
    b"bubbleChart",
    b"doughnutChart",
    b"ofPieChart",
    b"surfaceChart",
    b"surface3DChart",
    b"stockChart",
];

pub fn parse_chart(xml: &[u8], sheet: u32, part: String, anchor: ChartAnchor) -> ChartMeta {
    let mut scratch = Vec::new();
    let n = xml.len();

    // chart-level title: first <title> before <plotArea>
    let plot_start = find_open_local(xml, 0, b"plotArea").unwrap_or(n);
    let title = {
        let head = &xml[..plot_start];
        find_open_local(head, 0, b"title")
            .map(|ts| {
                let te = ts + memchr::memchr(b'>', &head[ts..]).unwrap_or(0);
                let end = find_close_local(head, te + 1, b"title").unwrap_or(head.len());
                let region = &head[ts..end];
                let t = concat_t_texts(region, &mut scratch);
                if !t.is_empty() {
                    t
                } else {
                    // strRef title
                    first_f_text(region, &mut scratch).unwrap_or_default()
                }
            })
            .filter(|s| !s.is_empty())
    };

    let plot_end = find_close_local(xml, plot_start + 1, b"plotArea").unwrap_or(n);
    let plot = if plot_start < n {
        &xml[plot_start..plot_end.min(n)]
    } else {
        &xml[..0]
    };

    let mut chart_types = Vec::new();
    let mut series = Vec::new();
    let mut i = 0usize;
    while let Some(o) = memchr::memchr(b'<', &plot[i..]) {
        let pos = i + o;
        let Some(local) = open_tag_local(plot, pos) else {
            i = pos + 1;
            continue;
        };
        if CHART_TYPE_LOCALS.iter().any(|c| *c == local) {
            let local_str = String::from_utf8_lossy(local).into_owned();
            chart_types.push(ChartType::from_tag_local(&local_str));
            let te = pos + memchr::memchr(b'>', &plot[pos..]).unwrap_or(plot.len() - pos);
            let end = if te > 0 && plot.get(te - 1) == Some(&b'/') {
                (te + 1).min(plot.len())
            } else {
                find_close_local(plot, te.saturating_add(1), local)
                    .and_then(|c| memchr::memchr(b'>', &plot[c..]).map(|o| c + o + 1))
                    .unwrap_or(plot.len())
                    .min(plot.len())
            };
            if pos >= end {
                i = pos.saturating_add(1);
                continue;
            }
            let block = &plot[pos..end];
            let mut si = 0usize;
            while let Some(ss) = find_open_local(block, si, b"ser") {
                let ste = ss + memchr::memchr(b'>', &block[ss..]).unwrap_or(block.len() - ss);
                let send = if ste > 0 && block.get(ste - 1) == Some(&b'/') {
                    (ste + 1).min(block.len())
                } else {
                    find_close_local(block, ste.saturating_add(1), b"ser")
                        .and_then(|c| memchr::memchr(b'>', &block[c..]).map(|o| c + o + 1))
                        .unwrap_or(block.len())
                        .min(block.len())
                };
                if ss < send {
                    series.push(parse_series_block(&block[ss..send], &mut scratch));
                }
                si = send.max(ss.saturating_add(1));
            }
            i = end.max(pos.saturating_add(1));
        } else {
            i = pos + 1;
        }
    }

    // axis titles: catAx first → x; valAx → y (scatter: first valAx x, second y)
    let mut cat_titles = Vec::new();
    let mut val_titles = Vec::new();
    let mut i = 0usize;
    while let Some(o) = memchr::memchr(b'<', &plot[i..]) {
        let pos = i + o;
        let Some(local) = open_tag_local(plot, pos) else {
            i = pos + 1;
            continue;
        };
        let is_cat = local == b"catAx";
        let is_val = local == b"valAx" || local == b"dateAx";
        if !is_cat && !is_val {
            i = pos + 1;
            continue;
        }
        let te = pos + memchr::memchr(b'>', &plot[pos..]).unwrap_or(plot.len() - pos);
        let end = if plot[te - 1] == b'/' {
            te + 1
        } else {
            find_close_local(plot, te + 1, local)
                .and_then(|c| memchr::memchr(b'>', &plot[c..]).map(|o| c + o + 1))
                .unwrap_or(plot.len())
        };
        let block = &plot[pos..end];
        let ax_title = find_open_local(block, 0, b"title")
            .map(|ts| {
                let tte = ts + memchr::memchr(b'>', &block[ts..]).unwrap_or(0);
                let tend = find_close_local(block, tte + 1, b"title").unwrap_or(block.len());
                concat_t_texts(&block[ts..tend], &mut scratch)
            })
            .filter(|s| !s.is_empty());
        if let Some(t) = ax_title {
            if is_cat {
                cat_titles.push(t);
            } else {
                val_titles.push(t);
            }
        }
        i = end;
    }

    let (x_axis_title, y_axis_title) = if !cat_titles.is_empty() {
        (cat_titles.into_iter().next(), val_titles.into_iter().next())
    } else {
        // scatter / pure val axes
        let mut it = val_titles.into_iter();
        (it.next(), it.next())
    };

    ChartMeta {
        sheet,
        part,
        chart_types,
        title,
        series,
        x_axis_title,
        y_axis_title,
        anchor,
    }
}

/// Workbook pivotCache map: cacheId → zip path of cache definition.
pub fn parse_workbook_pivot_caches(wb_xml: &[u8], wb_rels: &RelMap) -> HashMap<u32, String> {
    let mut out = HashMap::new();
    let mut i = 0usize;
    while let Some(start) = find_open_local(wb_xml, i, b"pivotCache") {
        let te = start + memchr::memchr(b'>', &wb_xml[start..]).unwrap_or(wb_xml.len() - start);
        let tag = &wb_xml[start..te];
        // skip pivotCaches container: local name pivotCaches != pivotCache
        let cache_id =
            find_attr(tag, b"cacheId").and_then(|v| std::str::from_utf8(v).ok()?.parse().ok());
        let rid = find_attr(tag, b"r:id").map(|r| String::from_utf8_lossy(r).into_owned());
        if let (Some(cid), Some(rid)) = (cache_id, rid) {
            if let Some(rel) = wb_rels.get(&rid) {
                let path = resolve_zip_path("xl/", &rel.target);
                out.insert(cid, path);
            }
        }
        i = te + 1;
    }
    out
}

pub fn parse_pivot_cache(xml: &[u8], part: String) -> PivotCacheMeta {
    let mut scratch = Vec::new();
    let mut source_type = String::from("worksheet");
    let mut worksheet_sheet = None;
    let mut worksheet_ref = None;
    let mut worksheet_name = None;

    if let Some(cs) = find_open_local(xml, 0, b"cacheSource") {
        let te = cs + memchr::memchr(b'>', &xml[cs..]).unwrap_or(0);
        let tag = &xml[cs..te];
        if let Some(t) = attr_str(tag, b"type", &mut scratch) {
            source_type = t;
        }
        let end = if xml[te - 1] == b'/' {
            te + 1
        } else {
            find_close_local(xml, te + 1, b"cacheSource")
                .and_then(|c| memchr::memchr(b'>', &xml[c..]).map(|o| c + o + 1))
                .unwrap_or(xml.len())
        };
        let body = &xml[cs..end];
        if let Some(ws) = find_open_local(body, 0, b"worksheetSource") {
            let wte = ws + memchr::memchr(b'>', &body[ws..]).unwrap_or(0);
            let wtag = &body[ws..wte];
            worksheet_sheet = attr_str(wtag, b"sheet", &mut scratch);
            worksheet_ref = attr_str(wtag, b"ref", &mut scratch);
            worksheet_name = attr_str(wtag, b"name", &mut scratch);
        }
    }

    let mut field_names = Vec::new();
    let mut i = 0usize;
    while let Some(start) = find_open_local(xml, i, b"cacheField") {
        let te = start + memchr::memchr(b'>', &xml[start..]).unwrap_or(xml.len() - start);
        let tag = &xml[start..te];
        if let Some(name) = attr_str(tag, b"name", &mut scratch) {
            field_names.push(name);
        }
        i = te + 1;
    }

    PivotCacheMeta {
        part,
        source_type,
        worksheet_sheet,
        worksheet_ref,
        worksheet_name,
        field_names,
    }
}

pub fn parse_pivot_table(xml: &[u8], sheet: u32, cache: PivotCacheMeta) -> Option<PivotTableMeta> {
    let mut scratch = Vec::new();
    let start = find_open_local(xml, 0, b"pivotTableDefinition")?;
    let te = start + memchr::memchr(b'>', &xml[start..])?;
    let tag = &xml[start..te];
    let name = attr_str(tag, b"name", &mut scratch).unwrap_or_default();
    let cache_id = find_attr(tag, b"cacheId")
        .and_then(|v| std::str::from_utf8(v).ok()?.parse().ok())
        .unwrap_or(0);

    let location_ref = find_open_local(xml, 0, b"location")
        .and_then(|ls| {
            let lte = ls + memchr::memchr(b'>', &xml[ls..])?;
            attr_str(&xml[ls..lte], b"ref", &mut scratch)
        })
        .unwrap_or_default();

    let resolve_fields = |local: &[u8]| -> Vec<String> {
        let mut names = Vec::new();
        let Some(fs) = find_open_local(xml, 0, local) else {
            return names;
        };
        let fe = find_close_local(xml, fs + 1, local).unwrap_or(xml.len());
        let body = &xml[fs..fe];
        let mut i = 0usize;
        while let Some(start) = find_open_local(body, i, b"field") {
            let fte = start + memchr::memchr(b'>', &body[start..]).unwrap_or(body.len() - start);
            let ftag = &body[start..fte];
            if let Some(x) = find_attr(ftag, b"x")
                .and_then(|v| std::str::from_utf8(v).ok()?.parse::<usize>().ok())
            {
                let nm = cache
                    .field_names
                    .get(x)
                    .cloned()
                    .unwrap_or_else(|| format!("field{x}"));
                names.push(nm);
            }
            i = fte + 1;
        }
        names
    };

    let row_fields = resolve_fields(b"rowFields");
    let col_fields = resolve_fields(b"colFields");

    let mut data_fields = Vec::new();
    if let Some(dfs) = find_open_local(xml, 0, b"dataFields") {
        let dfe = find_close_local(xml, dfs + 1, b"dataFields").unwrap_or(xml.len());
        let body = &xml[dfs..dfe];
        let mut i = 0usize;
        while let Some(start) = find_open_local(body, i, b"dataField") {
            let fte = start + memchr::memchr(b'>', &body[start..]).unwrap_or(body.len() - start);
            let ftag = &body[start..fte];
            let fld = find_attr(ftag, b"fld")
                .and_then(|v| std::str::from_utf8(v).ok()?.parse().ok())
                .unwrap_or(0);
            let name = attr_str(ftag, b"name", &mut scratch).unwrap_or_else(|| {
                cache
                    .field_names
                    .get(fld as usize)
                    .cloned()
                    .unwrap_or_else(|| format!("field{fld}"))
            });
            data_fields.push(PivotDataField {
                name,
                field_index: fld,
            });
            i = fte + 1;
        }
    }

    Some(PivotTableMeta {
        sheet,
        name,
        location_ref,
        cache_id,
        row_fields,
        col_fields,
        data_fields,
        cache,
    })
}

pub fn parse_persons(xml: &[u8]) -> Vec<Person> {
    let mut out = Vec::new();
    let mut scratch = Vec::new();
    let mut i = 0usize;
    while let Some(start) = find_open_local(xml, i, b"person") {
        let te = start + memchr::memchr(b'>', &xml[start..]).unwrap_or(xml.len() - start);
        let tag = &xml[start..te];
        let id = attr_str(tag, b"id", &mut scratch).unwrap_or_default();
        let display_name = attr_str(tag, b"displayName", &mut scratch).unwrap_or_default();
        let user_id = attr_str(tag, b"userId", &mut scratch);
        let provider_id = attr_str(tag, b"providerId", &mut scratch);
        out.push(Person {
            id,
            display_name,
            user_id,
            provider_id,
        });
        i = te + 1;
    }
    out
}

pub fn parse_threaded_comments(xml: &[u8]) -> Vec<ThreadedComment> {
    let mut out = Vec::new();
    let mut scratch = Vec::new();
    let mut i = 0usize;
    while let Some(start) = find_open_local(xml, i, b"threadedComment") {
        let te = start + memchr::memchr(b'>', &xml[start..]).unwrap_or(xml.len() - start);
        let tag = &xml[start..te];
        // Guard te==0: never index xml[te-1] unchecked.
        let self_closing = te > 0 && xml.get(te - 1) == Some(&b'/');
        let ref_raw = attr_str(tag, b"ref", &mut scratch).unwrap_or_else(|| "A1".into());
        let cr = parse_range(ref_raw.as_bytes());
        let id = attr_str(tag, b"id", &mut scratch).unwrap_or_default();
        let person_id = attr_str(tag, b"personId", &mut scratch).unwrap_or_default();
        let parent_id = attr_str(tag, b"parentId", &mut scratch);
        let done = find_attr(tag, b"done")
            .map(|v| v == b"1" || v == b"true")
            .unwrap_or(false);
        let datetime = attr_str(tag, b"dT", &mut scratch);
        let text = if self_closing {
            String::new()
        } else {
            let end = find_close_local(xml, te + 1, b"threadedComment").unwrap_or(xml.len());
            let body = if te + 1 <= end {
                &xml[te + 1..end]
            } else {
                &xml[te..te]
            };
            first_elem_text(body, b"text", &mut scratch)
                .unwrap_or_else(|| concat_t_texts(body, &mut scratch))
        };
        out.push(ThreadedComment {
            ref_cell: (cr.r0, cr.c0),
            ref_raw,
            id,
            person_id,
            person_display_name: String::new(),
            parent_id,
            done,
            text,
            datetime,
        });
        i = te + 1;
    }
    out
}

/// Fill `person_display_name` from a persons list (by id).
pub fn resolve_threaded_person_names(comments: &mut [ThreadedComment], persons: &[Person]) {
    for c in comments {
        if let Some(p) = persons.iter().find(|p| p.id == c.person_id) {
            c.person_display_name = p.display_name.clone();
        }
    }
}

// ----------------------------------------------------------------------------
// 5. TABLES — tail tableParts -> rels -> inflate tableN.xml.
// ----------------------------------------------------------------------------
pub fn scan_table_part_rids(tail: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0usize;
    let n = tail.len();
    while let Some(o) = memchr::memmem::find(&tail[i..n], b"<tablePart ") {
        let start = i + o;
        let te = start + memchr::memchr(b'>', &tail[start..n]).unwrap_or(n - start);
        let tag = &tail[start..te];
        if let Some(rid) = find_attr(tag, b"r:id") {
            out.push(String::from_utf8_lossy(rid).into_owned());
        }
        i = te + 1;
    }
    out
}

pub fn parse_table(xml: &[u8], sheet: u32) -> Option<Table> {
    let mut scratch = Vec::new();
    let n = xml.len();
    let to = memchr::memmem::find(xml, b"<table ")?;
    let te = to + memchr::memchr(b'>', &xml[to..n])?;
    let tag = &xml[to..te];
    let display_name = attr_str(tag, b"displayName", &mut scratch).unwrap_or_default();
    let name = attr_str(tag, b"name", &mut scratch).unwrap_or_else(|| display_name.clone());
    let ref_ = find_attr(tag, b"ref")
        .map(parse_range)
        .unwrap_or(CellRange {
            r0: 0,
            c0: 0,
            r1: 0,
            c1: 0,
        });
    let header_row_count = find_attr(tag, b"headerRowCount")
        .and_then(|v| std::str::from_utf8(v).ok()?.parse().ok())
        .unwrap_or(1); // default 1
    let totals_row_count = find_attr(tag, b"totalsRowCount")
        .and_then(|v| std::str::from_utf8(v).ok()?.parse().ok())
        .unwrap_or(0);

    // columns
    let mut columns = Vec::new();
    if let Some(co) = memchr::memmem::find(xml, b"<tableColumns") {
        let ce = memchr::memmem::find(&xml[co..n], b"</tableColumns>")
            .map(|o| co + o)
            .unwrap_or(n);
        let mut i = co;
        while let Some(o) = memchr::memmem::find(&xml[i..ce], b"<tableColumn ") {
            let start = i + o;
            let cte = start + memchr::memchr(b'>', &xml[start..ce]).unwrap_or(ce - start);
            let ctag = &xml[start..cte];
            let cself = xml[cte - 1] == b'/';
            let cname = attr_str(ctag, b"name", &mut scratch).unwrap_or_default();
            let totals_fn = attr_str(ctag, b"totalsRowFunction", &mut scratch);
            let totals_label = attr_str(ctag, b"totalsRowLabel", &mut scratch);
            // calculatedColumnFormula: child element <calculatedColumnFormula>TEXT</...>
            let calc_formula = if cself {
                None
            } else {
                let colend = memchr::memmem::find(&xml[cte..ce], b"</tableColumn>")
                    .map(|o| cte + o)
                    .unwrap_or(ce);
                let inner = &xml[cte..colend];
                memchr::memmem::find(inner, b"<calculatedColumnFormula").map(|fo| {
                    let fte = fo + memchr::memchr(b'>', &inner[fo..]).unwrap_or(0);
                    let fclose = memchr::memmem::find(&inner[fte..], b"</calculatedColumnFormula>")
                        .map(|o| fte + o)
                        .unwrap_or(inner.len());
                    String::from_utf8_lossy(super::decode::decode_bytes(
                        &inner[fte + 1..fclose],
                        &mut scratch,
                    ))
                    .into_owned()
                })
            };
            columns.push(TableColumn {
                name: cname,
                totals_fn,
                totals_label,
                calc_formula,
            });
            i = cte + 1;
        }
    }

    // style
    let style = memchr::memmem::find(xml, b"<tableStyleInfo").map(|so| {
        let se = so + memchr::memchr(b'>', &xml[so..n]).unwrap_or(0);
        let stag = &xml[so..se];
        let flag = |a: &[u8]| {
            find_attr(stag, a)
                .map(|v| v == b"1" || v == b"true")
                .unwrap_or(false)
        };
        TableStyle {
            name: attr_str(stag, b"name", &mut scratch).unwrap_or_default(),
            show_first_col: flag(b"showFirstColumn"),
            show_last_col: flag(b"showLastColumn"),
            show_row_stripes: flag(b"showRowStripes"),
            show_col_stripes: flag(b"showColumnStripes"),
        }
    });

    Some(Table {
        name,
        display_name,
        ref_,
        header_row_count,
        totals_row_count,
        columns,
        style,
        sheet,
    })
}

// ----------------------------------------------------------------------------
// 6. COMMENTS — rels -> inflate commentsN.xml.
// ----------------------------------------------------------------------------
pub fn parse_comments(xml: &[u8]) -> SheetComments {
    let mut scratch = Vec::new();
    let n = xml.len();

    // authors
    let mut authors = Vec::new();
    if let Some(ao) = memchr::memmem::find(xml, b"<authors>") {
        let ae = memchr::memmem::find(&xml[ao..n], b"</authors>")
            .map(|o| ao + o)
            .unwrap_or(n);
        let mut i = ao;
        while let Some(o) = memchr::memmem::find(&xml[i..ae], b"<author>") {
            let s = i + o + 8;
            let e = memchr::memmem::find(&xml[s..ae], b"</author>")
                .map(|o| s + o)
                .unwrap_or(ae);
            authors.push(
                String::from_utf8_lossy(super::decode::decode_bytes(&xml[s..e], &mut scratch))
                    .into_owned(),
            );
            i = e + 9;
        }
    }

    // comments
    let mut comments = Vec::new();
    let clist_o = memchr::memmem::find(xml, b"<commentList>")
        .map(|o| o + 13)
        .unwrap_or(0);
    let mut i = clist_o;
    while let Some(o) = memchr::memmem::find(&xml[i..n], b"<comment ") {
        let start = i + o;
        let te = start + memchr::memchr(b'>', &xml[start..n]).unwrap_or(n - start);
        let tag = &xml[start..te];
        let cr = parse_range(find_attr(tag, b"ref").unwrap_or(b"A1"));
        let author_id = find_attr(tag, b"authorId")
            .and_then(|v| std::str::from_utf8(v).ok()?.parse::<u32>().ok())
            .unwrap_or(0);
        // comment body up to </comment>
        let cend = memchr::memmem::find(&xml[te..n], b"</comment>")
            .map(|o| te + o)
            .unwrap_or(n);
        let body = &xml[te..cend];
        // text = concat every <t>...</t> (plain + <r><t> runs), entity-decoded
        let mut text = String::new();
        let mut p = 0usize;
        let bn = body.len();
        while let Some(to) = memchr::memmem::find(&body[p..bn], b"<t") {
            let topen = p + to;
            let after = body.get(topen + 2).copied().unwrap_or(b'>');
            if !(after == b' ' || after == b'>' || after == b'/') {
                p = topen + 2;
                continue;
            }
            let topen_end = topen + memchr::memchr(b'>', &body[topen..bn]).unwrap_or(bn - topen);
            if body[topen_end - 1] == b'/' {
                p = topen_end + 1;
                continue; // <t/>
            }
            let tclose = memchr::memmem::find(&body[topen_end..bn], b"</t>")
                .map(|o| topen_end + o)
                .unwrap_or(bn);
            let raw = &body[topen_end + 1..tclose];
            text.push_str(&String::from_utf8_lossy(super::decode::decode_bytes(
                raw,
                &mut scratch,
            )));
            p = tclose + 4;
        }
        comments.push(Comment {
            row: cr.r0,
            col: cr.c0,
            author_id,
            text,
        });
        i = cend + 10;
    }

    SheetComments {
        authors,
        comments,
        legacy_is_mirror: false,
    }
}
