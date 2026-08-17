//! Chart + drawing writers (F085–F099). Ported from writelab/siloC.

use super::xml::{escape_text, write_escaped_attr, write_escaped_text};

pub const CHART_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/chart";
pub const DRAWING_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
pub const SHEET_DRAWING_NS: &str =
    "http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing";
pub const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
pub const PKG_REL_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";

#[derive(Clone, Debug)]
pub enum ChartType {
    Bar,
    Bar3D,
    Col,
    Col3D,
    Line,
    Line3D,
    Area,
    Area3D,
    Pie,
    Pie3D,
    Doughnut,
    Scatter,
    Bubble,
    Radar,
    Stock,
    Surface,
    Surface3D,
}

impl ChartType {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "bar" => Some(ChartType::Bar),
            "bar3d" | "bar_3d" => Some(ChartType::Bar3D),
            "col" | "column" => Some(ChartType::Col),
            "col3d" | "column3d" | "col_3d" => Some(ChartType::Col3D),
            "line" => Some(ChartType::Line),
            "line3d" | "line_3d" => Some(ChartType::Line3D),
            "area" => Some(ChartType::Area),
            "area3d" | "area_3d" => Some(ChartType::Area3D),
            "pie" => Some(ChartType::Pie),
            "pie3d" | "pie_3d" => Some(ChartType::Pie3D),
            "doughnut" | "donut" => Some(ChartType::Doughnut),
            "scatter" => Some(ChartType::Scatter),
            "bubble" => Some(ChartType::Bubble),
            "radar" => Some(ChartType::Radar),
            "stock" => Some(ChartType::Stock),
            "surface" => Some(ChartType::Surface),
            "surface3d" | "surface_3d" => Some(ChartType::Surface3D),
            _ => None,
        }
    }

    pub fn tag(&self) -> &'static str {
        match self {
            ChartType::Bar | ChartType::Col => "barChart",
            ChartType::Bar3D | ChartType::Col3D => "bar3DChart",
            ChartType::Line => "lineChart",
            ChartType::Line3D => "line3DChart",
            ChartType::Area => "areaChart",
            ChartType::Area3D => "area3DChart",
            ChartType::Pie => "pieChart",
            ChartType::Pie3D => "pie3DChart",
            ChartType::Doughnut => "doughnutChart",
            ChartType::Scatter => "scatterChart",
            ChartType::Bubble => "bubbleChart",
            ChartType::Radar => "radarChart",
            ChartType::Stock => "stockChart",
            ChartType::Surface => "surfaceChart",
            ChartType::Surface3D => "surface3DChart",
        }
    }

    pub fn is_pie_family(&self) -> bool {
        matches!(
            self,
            ChartType::Pie | ChartType::Pie3D | ChartType::Doughnut
        )
    }

    pub fn is_scatter_family(&self) -> bool {
        matches!(self, ChartType::Scatter | ChartType::Bubble)
    }

    pub fn is_3d(&self) -> bool {
        matches!(
            self,
            ChartType::Bar3D
                | ChartType::Col3D
                | ChartType::Line3D
                | ChartType::Area3D
                | ChartType::Pie3D
                | ChartType::Surface3D
        )
    }

    pub fn bar_dir(&self) -> Option<&'static str> {
        match self {
            ChartType::Bar | ChartType::Bar3D => Some("bar"),
            ChartType::Col | ChartType::Col3D => Some("col"),
            _ => None,
        }
    }
}

/// Bar/column and line/area grouping semantics.
///
/// `Clustered` and `Standard` are the family defaults: bar/column charts
/// emit `clustered`, line/area charts emit `standard`. `Stacked` and
/// `PercentStacked` are shared by both families.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Grouping {
    #[default]
    Clustered,
    Standard,
    Stacked,
    PercentStacked,
}

impl Grouping {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "clustered" | "cluster" => Some(Grouping::Clustered),
            "standard" => Some(Grouping::Standard),
            "stacked" => Some(Grouping::Stacked),
            "percentstacked" | "percent_stacked" | "percent" => Some(Grouping::PercentStacked),
            _ => None,
        }
    }

    /// Stacked and percent-stacked need an overlap of 100 on 2D bar/column,
    /// or Excel renders the bars side by side and the chart looks broken.
    pub fn is_stacked(self) -> bool {
        matches!(self, Grouping::Stacked | Grouping::PercentStacked)
    }

    /// Map to the OOXML grouping vocabulary valid for the chart family.
    fn ooxml_val(self, ct: &ChartType) -> &'static str {
        if ct.bar_dir().is_some() {
            match self {
                Grouping::Clustered | Grouping::Standard => "clustered",
                Grouping::Stacked => "stacked",
                Grouping::PercentStacked => "percentStacked",
            }
        } else {
            match self {
                Grouping::Clustered | Grouping::Standard => "standard",
                Grouping::Stacked => "stacked",
                Grouping::PercentStacked => "percentStacked",
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Series {
    pub title_ref: Option<String>,
    pub title_literal: Option<String>,
    pub cat_ref: Option<String>,
    pub val_ref: Option<String>,
    pub x_ref: Option<String>,
    pub y_ref: Option<String>,
    pub bubble_size_ref: Option<String>,
    /// srgbClr hex (e.g. "FF0000"); emitted as a solidFill inside spPr.
    pub colour: Option<String>,
    /// Marker symbol for line/scatter/radar series: circle, dash, diamond,
    /// dot, none, plus, square, star, triangle, x, auto.
    pub marker_symbol: Option<String>,
    pub marker_size: Option<u8>,
    /// Line/scatter smooth flag; emitted as `<smooth val="0|1"/>`.
    pub smooth: Option<bool>,
}

#[derive(Clone, Debug)]
pub enum Anchor {
    OneCell {
        cell: String,
        /// EMU offset from the cell's top-left corner.
        col_off: i64,
        row_off: i64,
        width_cm: f64,
        height_cm: f64,
    },
    TwoCell {
        from_cell: String,
        from_off: (i64, i64),
        to_cell: String,
        to_off: (i64, i64),
        /// `editAs` attribute: twoCell | oneCell | absolute. Omitted when None.
        edit_as: Option<String>,
    },
    Absolute {
        x_emu: i64,
        y_emu: i64,
        cx_emu: i64,
        cy_emu: i64,
    },
}

impl Default for Anchor {
    fn default() -> Self {
        Anchor::OneCell {
            cell: "E15".into(),
            col_off: 0,
            row_off: 0,
            width_cm: 15.0,
            height_cm: 7.5,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Chart {
    pub chart_type: ChartType,
    pub title: Option<String>,
    pub series: Vec<Series>,
    pub anchor: Anchor,
    pub style: Option<u8>,
    pub legend_pos: Option<String>,
    pub grouping: Grouping,
}

impl Default for Chart {
    fn default() -> Self {
        Self {
            chart_type: ChartType::Col,
            title: None,
            series: Vec::new(),
            anchor: Anchor::default(),
            style: None,
            legend_pos: Some("r".into()),
            grouping: Grouping::Clustered,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ChartsheetSpec {
    pub title: String,
    pub charts: Vec<Chart>,
}

// EMU = English Metric Units: 914400 per inch, 12700 per point, 360000 per
// centimetre (the openpyxl convention). Every anchor conversion goes through
// these constants — never scatter magic numbers.
pub const EMU_PER_INCH: f64 = 914400.0;
pub const EMU_PER_POINT: f64 = 12700.0;
pub const EMU_PER_CM: f64 = 360000.0;

/// cm → EMU (1 cm = [`EMU_PER_CM`]).
#[inline]
pub fn cm_to_emu(cm: f64) -> i64 {
    (cm * EMU_PER_CM).round() as i64
}

/// A1 → (row 1-based, col 1-based).
pub fn coord_to_tuple(coord: &str) -> (u32, u32) {
    let bytes = coord.as_bytes();
    let mut i = 0;
    let mut col: u32 = 0;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        col = col * 26 + (bytes[i].to_ascii_uppercase() - b'A' + 1) as u32;
        i += 1;
    }
    let row: u32 = std::str::from_utf8(&bytes[i..])
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    (row, col)
}

pub fn write_chart_space(chart: &Chart) -> String {
    let mut s = String::with_capacity(4096);
    s.push_str(&format!(
        r#"<chartSpace xmlns:a="{DRAWING_NS}" xmlns="{CHART_NS}">"#
    ));
    s.push_str("<chart>");
    if let Some(t) = &chart.title {
        s.push_str(&title_xml(t));
    }
    if chart.chart_type.is_3d() {
        s.push_str(
            r#"<view3D><rotX val="15"/><rotY val="20"/><rAngAx val="0"/><perspective val="30"/></view3D>"#,
        );
    }
    s.push_str("<plotArea>");
    s.push_str(&plot_chart_xml(chart));
    s.push_str(&axes_xml(chart));
    s.push_str("</plotArea>");
    let pos = chart.legend_pos.as_deref().unwrap_or("r");
    s.push_str(&format!(
        r#"<legend><legendPos val="{}"/></legend>"#,
        escape_attr(pos)
    ));
    s.push_str(r#"<plotVisOnly val="1"/><dispBlanksAs val="gap"/>"#);
    s.push_str("</chart>");
    if let Some(st) = chart.style {
        s.push_str(&format!(r#"<style val="{st}"/>"#));
    }
    s.push_str("</chartSpace>");
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

fn title_xml(text: &str) -> String {
    format!(
        r#"<title><tx><rich><a:bodyPr/><a:p><a:pPr><a:defRPr/></a:pPr><a:r><a:t>{}</a:t></a:r></a:p></rich></tx></title>"#,
        escape_text(text)
    )
}

fn sp_pr_xml(ser: &Series) -> String {
    let mut s = String::from("<spPr>");
    if let Some(col) = &ser.colour {
        s.push_str(&format!(
            r#"<a:solidFill><a:srgbClr val="{}"/></a:solidFill>"#,
            escape_attr(col)
        ));
    }
    s.push_str(r#"<a:ln><a:prstDash val="solid"/></a:ln></spPr>"#);
    s
}

fn marker_xml(ser: &Series) -> String {
    let symbol = ser.marker_symbol.as_deref().unwrap_or("none");
    let mut s = String::from("<marker>");
    s.push_str(&format!(r#"<symbol val="{symbol}"/>"#));
    if let Some(size) = ser.marker_size {
        s.push_str(&format!(r#"<size val="{size}"/>"#));
    }
    s.push_str(r#"<spPr><a:ln><a:prstDash val="solid"/></a:ln></spPr>"#);
    s.push_str("</marker>");
    s
}

fn series_xml(ser: &Series, idx: usize, ct: &ChartType) -> String {
    // Schema order inside <ser>: idx, order, tx, spPr, marker, dPt, dLbls,
    // cat, val, smooth (xVal/yVal in place of cat/val for scatter). Excel
    // rejects a chart part with out-of-order children, so keep this order.
    let mut s = String::new();
    s.push_str("<ser>");
    s.push_str(&format!(r#"<idx val="{idx}"/><order val="{idx}"/>"#));
    if let Some(r) = &ser.title_ref {
        s.push_str(&format!(
            r#"<tx><strRef><f>{}</f></strRef></tx>"#,
            escape_text(r)
        ));
    } else if let Some(v) = &ser.title_literal {
        s.push_str(&format!(r#"<tx><v>{}</v></tx>"#, escape_text(v)));
    }
    s.push_str(&sp_pr_xml(ser));
    if matches!(
        ct,
        ChartType::Line | ChartType::Line3D | ChartType::Scatter | ChartType::Radar
    ) {
        s.push_str(&marker_xml(ser));
    }
    if ct.is_scatter_family() {
        if let Some(x) = &ser.x_ref {
            s.push_str(&format!(
                r#"<xVal><numRef><f>{}</f></numRef></xVal>"#,
                escape_text(x)
            ));
        }
        if let Some(y) = &ser.y_ref {
            s.push_str(&format!(
                r#"<yVal><numRef><f>{}</f></numRef></yVal>"#,
                escape_text(y)
            ));
        }
        if matches!(ct, ChartType::Bubble) {
            if let Some(b) = &ser.bubble_size_ref {
                s.push_str(&format!(
                    r#"<bubbleSize><numRef><f>{}</f></numRef></bubbleSize>"#,
                    escape_text(b)
                ));
            }
        }
    } else {
        if let Some(c) = &ser.cat_ref {
            s.push_str(&format!(
                r#"<cat><numRef><f>{}</f></numRef></cat>"#,
                escape_text(c)
            ));
        }
        if let Some(v) = &ser.val_ref {
            s.push_str(&format!(
                r#"<val><numRef><f>{}</f></numRef></val>"#,
                escape_text(v)
            ));
        }
    }
    if matches!(ct, ChartType::Line | ChartType::Line3D | ChartType::Scatter) {
        // Excel defaults scatter-with-lines to smooth=true when the element
        // is absent, so emit an explicit 0 unless the caller asked for a curve.
        let v = if ser.smooth.unwrap_or(false) {
            "1"
        } else {
            "0"
        };
        s.push_str(&format!(r#"<smooth val="{v}"/>"#));
    }
    s.push_str("</ser>");
    s
}

fn plot_chart_xml(chart: &Chart) -> String {
    let tag = chart.chart_type.tag();
    let mut s = format!("<{tag}>");
    let grouping = chart.grouping.ooxml_val(&chart.chart_type);
    if let Some(dir) = chart.chart_type.bar_dir() {
        s.push_str(&format!(r#"<barDir val="{dir}"/>"#));
        s.push_str(&format!(r#"<grouping val="{grouping}"/>"#));
    } else if matches!(
        chart.chart_type,
        ChartType::Line | ChartType::Line3D | ChartType::Area | ChartType::Area3D
    ) {
        s.push_str(&format!(r#"<grouping val="{grouping}"/>"#));
    }
    match chart.chart_type {
        ChartType::Pie | ChartType::Pie3D | ChartType::Doughnut => {
            s.push_str(r#"<varyColors val="1"/>"#);
        }
        ChartType::Radar => {
            s.push_str(r#"<radarStyle val="standard"/>"#);
        }
        ChartType::Surface | ChartType::Surface3D => {
            s.push_str(r#"<wireframe val="0"/>"#);
        }
        _ => {}
    }
    for (i, ser) in chart.series.iter().enumerate() {
        s.push_str(&series_xml(ser, i, &chart.chart_type));
    }
    match chart.chart_type {
        ChartType::Bar | ChartType::Col | ChartType::Bar3D | ChartType::Col3D => {
            s.push_str(r#"<gapWidth val="150"/>"#);
            if matches!(chart.chart_type, ChartType::Bar3D | ChartType::Col3D) {
                s.push_str(r#"<gapDepth val="150"/>"#);
            } else if chart.grouping.is_stacked() {
                // Stacked bars sit on top of each other; without overlap=100
                // Excel renders them side by side and the chart looks broken.
                s.push_str(r#"<overlap val="100"/>"#);
            }
        }
        ChartType::Pie => {
            s.push_str(r#"<firstSliceAng val="0"/>"#);
        }
        ChartType::Doughnut => {
            s.push_str(r#"<firstSliceAng val="0"/><holeSize val="50"/>"#);
        }
        ChartType::Stock => {
            s.push_str(r#"<hiLowLines/>"#);
        }
        _ => {}
    }
    if !chart.chart_type.is_pie_family() {
        if chart.chart_type.is_scatter_family() {
            s.push_str(r#"<axId val="10"/><axId val="20"/>"#);
        } else if matches!(
            chart.chart_type,
            ChartType::Line3D
                | ChartType::Bar3D
                | ChartType::Col3D
                | ChartType::Surface3D
                | ChartType::Surface
        ) {
            s.push_str(r#"<axId val="10"/><axId val="100"/><axId val="1000"/>"#);
        } else {
            s.push_str(r#"<axId val="10"/><axId val="100"/>"#);
        }
    }
    s.push_str(&format!("</{tag}>"));
    s
}

fn cat_ax(ax_id: i32, cross: i32) -> String {
    format!(
        r#"<catAx><axId val="{ax_id}"/><scaling><orientation val="minMax"/></scaling><axPos val="l"/><majorTickMark val="none"/><minorTickMark val="none"/><crossAx val="{cross}"/><lblOffset val="100"/></catAx>"#
    )
}

fn val_ax(ax_id: i32, cross: i32, grid: bool) -> String {
    let g = if grid { "<majorGridlines/>" } else { "" };
    format!(
        r#"<valAx><axId val="{ax_id}"/><scaling><orientation val="minMax"/></scaling><axPos val="l"/>{g}<majorTickMark val="none"/><minorTickMark val="none"/><crossAx val="{cross}"/></valAx>"#
    )
}

fn ser_ax(ax_id: i32, cross: i32) -> String {
    format!(
        r#"<serAx><axId val="{ax_id}"/><scaling><orientation val="minMax"/></scaling><axPos val="b"/><crossAx val="{cross}"/></serAx>"#
    )
}

fn axes_xml(chart: &Chart) -> String {
    if chart.chart_type.is_pie_family() {
        return String::new();
    }
    if chart.chart_type.is_scatter_family() {
        return format!("{}{}", val_ax(10, 20, true), val_ax(20, 10, true));
    }
    let mut s = String::new();
    s.push_str(&cat_ax(10, 100));
    s.push_str(&val_ax(100, 10, true));
    if matches!(
        chart.chart_type,
        ChartType::Line3D
            | ChartType::Bar3D
            | ChartType::Col3D
            | ChartType::Surface
            | ChartType::Surface3D
            | ChartType::Area3D
    ) {
        s.push_str(&ser_ax(1000, 10));
    }
    s
}

/// One image placed on a worksheet drawing (T1-2a).
pub struct DrawingImage {
    /// Placement anchor (shared chart anchor vocabulary).
    pub anchor: Anchor,
    /// Drawing rel id of this image within `drawingD.xml.rels` (1-based).
    pub rel_id: usize,
    /// `cNvPr id` within the drawing; must be unique across charts + images.
    pub cnv_id: usize,
}

/// Build drawing XML + rels for charts only (chartsheets / existing callers).
pub fn write_drawing(charts: &[Chart], chart_paths: &[String]) -> (String, String) {
    write_drawing_full(charts, chart_paths, &[], &[])
}

/// Build drawing XML + merged rels for charts AND images in one worksheet
/// drawing. OOXML allows exactly one drawing part per worksheet, so images join
/// the chart drawing rather than creating a second part. Chart rels get
/// `rId1..chart_count`; image rels continue from there (`rIdN+1..`), and each
/// image's `<a:blip r:embed>` references its own rel id. `media_targets` are the
/// drawing-relative Targets (e.g. `../media/image1.png`) aligned with `images`.
pub fn write_drawing_full(
    charts: &[Chart],
    chart_paths: &[String],
    images: &[DrawingImage],
    media_targets: &[String],
) -> (String, String) {
    let mut drawing = format!(
        r#"<wsDr xmlns:a="{DRAWING_NS}" xmlns:c="{CHART_NS}" xmlns:r="{REL_NS}" xmlns="{SHEET_DRAWING_NS}">"#
    );
    for (i, chart) in charts.iter().enumerate() {
        let rid = i + 1;
        drawing.push_str(&anchor_wrap(&chart.anchor, &graphic_frame(rid)));
    }
    for img in images {
        drawing.push_str(&anchor_wrap(&img.anchor, &picture_xml(img)));
    }
    drawing.push_str("</wsDr>");

    let mut rels =
        format!(r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="{PKG_REL_NS}">"#);
    for (i, path) in chart_paths.iter().enumerate() {
        let rid = i + 1;
        rels.push_str(&format!(
            r#"<Relationship Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart" Target="{}" Id="rId{rid}"/>"#,
            escape_attr(path)
        ));
    }
    for (i, img) in images.iter().enumerate() {
        rels.push_str(&format!(
            r#"<Relationship Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="{}" Id="rId{}"/>"#,
            escape_attr(&media_targets[i]),
            img.rel_id
        ));
    }
    rels.push_str("</Relationships>");
    (drawing, rels)
}

/// Wrap an anchor-agnostic drawing body (`graphicFrame` or `pic`) in its
/// anchor element.
fn anchor_wrap(anchor: &Anchor, body: &str) -> String {
    match anchor {
        Anchor::OneCell {
            cell,
            col_off,
            row_off,
            width_cm,
            height_cm,
        } => {
            let (row, col) = coord_to_tuple(cell);
            let cx = cm_to_emu(*width_cm);
            let cy = cm_to_emu(*height_cm);
            format!(
                r#"<oneCellAnchor><from><col>{}</col><colOff>{col_off}</colOff><row>{}</row><rowOff>{row_off}</rowOff></from><ext cx="{cx}" cy="{cy}"/>{body}<clientData/></oneCellAnchor>"#,
                col - 1,
                row - 1,
            )
        }
        Anchor::TwoCell {
            from_cell,
            from_off,
            to_cell,
            to_off,
            edit_as,
        } => {
            let (fr, fc) = coord_to_tuple(from_cell);
            let (tr, tc) = coord_to_tuple(to_cell);
            let edit = edit_as
                .as_ref()
                .map(|e| format!(r#" editAs="{}""#, escape_attr(e)))
                .unwrap_or_default();
            format!(
                r#"<twoCellAnchor{edit}><from><col>{}</col><colOff>{}</colOff><row>{}</row><rowOff>{}</rowOff></from><to><col>{}</col><colOff>{}</colOff><row>{}</row><rowOff>{}</rowOff></to>{body}<clientData/></twoCellAnchor>"#,
                fc - 1,
                from_off.0,
                fr - 1,
                from_off.1,
                tc - 1,
                to_off.0,
                tr - 1,
                to_off.1,
            )
        }
        Anchor::Absolute {
            x_emu,
            y_emu,
            cx_emu,
            cy_emu,
        } => {
            format!(
                r#"<absoluteAnchor><pos x="{x_emu}" y="{y_emu}"/><ext cx="{cx_emu}" cy="{cy_emu}"/>{body}<clientData/></absoluteAnchor>"#
            )
        }
    }
}

/// Minimal valid `<pic>` for an image; the blip references the drawing rel
/// whose id is `rel_id` (the actual media part lives in `xl/media/`).
fn picture_xml(img: &DrawingImage) -> String {
    let cnv = img.cnv_id;
    format!(
        r#"<pic><nvPicPr><cNvPr id="{cnv}" name="Picture {cnv}"/><cNvPicPr><a:picLocks noChangeAspect="1"/></cNvPicPr></nvPicPr><blipFill><a:blip r:embed="rId{}"/><a:stretch><a:fillRect/></a:stretch></blipFill><spPr><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></spPr></pic>"#,
        img.rel_id
    )
}

fn graphic_frame(idx: usize) -> String {
    format!(
        r#"<graphicFrame><nvGraphicFramePr><cNvPr id="{idx}" name="Chart {idx}"/><cNvGraphicFramePr/></nvGraphicFramePr><xfrm/><a:graphic><a:graphicData uri="{CHART_NS}"><c:chart r:id="rId{idx}"/></a:graphicData></a:graphic></graphicFrame>"#
    )
}

pub fn write_chartsheet_xml(_title: &str, drawing_rid: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><chartsheet xmlns:r="{REL_NS}" xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetPr/><sheetViews><sheetView workbookViewId="0" zoomToFit="1"/></sheetViews><pageMargins left="0.7" right="0.7" top="0.75" bottom="0.75" header="0.3" footer="0.3"/><drawing r:id="{drawing_rid}"/></chartsheet>"#
    )
}

// Keep write_escaped_attr / write_escaped_text linked for consistency.
#[allow(dead_code)]
fn _use_xml_helpers(out: &mut Vec<u8>, s: &str) {
    write_escaped_attr(out, s);
    write_escaped_text(out, s);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar_chart(grouping: Grouping) -> Chart {
        Chart {
            chart_type: ChartType::Bar,
            series: vec![Series {
                cat_ref: Some("Sheet1!$A$1:$A$3".into()),
                val_ref: Some("Sheet1!$B$1:$B$3".into()),
                ..Series::default()
            }],
            grouping,
            ..Chart::default()
        }
    }

    fn col_chart(grouping: Grouping) -> Chart {
        Chart {
            chart_type: ChartType::Col,
            series: vec![Series {
                cat_ref: Some("Sheet1!$A$1:$A$3".into()),
                val_ref: Some("Sheet1!$B$1:$B$3".into()),
                ..Series::default()
            }],
            grouping,
            ..Chart::default()
        }
    }

    fn ser_block(xml: &str) -> &str {
        let start = xml.find("<ser>").expect("chart has a <ser> element");
        let end = xml.find("</ser>").expect("chart has a </ser> element");
        &xml[start..end]
    }

    fn idx_of(hay: &str, needle: &str) -> usize {
        hay.find(needle)
            .unwrap_or_else(|| panic!("expected {needle:?} in {hay:?}"))
    }

    #[test]
    fn stacked_bar_emits_grouping_and_overlap() {
        let xml = write_chart_space(&bar_chart(Grouping::Stacked));
        assert!(xml.contains(r#"<grouping val="stacked"/>"#), "{xml}");
        assert!(xml.contains(r#"<overlap val="100"/>"#), "{xml}");
    }

    #[test]
    fn percent_stacked_col_emits_grouping_and_overlap() {
        let xml = write_chart_space(&col_chart(Grouping::PercentStacked));
        assert!(xml.contains(r#"<grouping val="percentStacked"/>"#), "{xml}");
        assert!(xml.contains(r#"<overlap val="100"/>"#), "{xml}");
    }

    #[test]
    fn clustered_bar_has_no_overlap() {
        let xml = write_chart_space(&bar_chart(Grouping::Clustered));
        assert!(xml.contains(r#"<grouping val="clustered"/>"#), "{xml}");
        assert!(!xml.contains("<overlap"), "{xml}");
    }

    #[test]
    fn line_uses_standard_grouping_vocabulary() {
        let chart = Chart {
            chart_type: ChartType::Line,
            grouping: Grouping::Stacked,
            ..Chart::default()
        };
        let xml = write_chart_space(&chart);
        assert!(xml.contains(r#"<grouping val="stacked"/>"#), "{xml}");
    }

    #[test]
    fn series_colour_emits_solid_fill() {
        let chart = Chart {
            series: vec![Series {
                colour: Some("FF0000".into()),
                val_ref: Some("Sheet1!$B$1:$B$3".into()),
                ..Series::default()
            }],
            ..Chart::default()
        };
        let xml = write_chart_space(&chart);
        assert!(
            xml.contains(r#"<spPr><a:solidFill><a:srgbClr val="FF0000"/></a:solidFill>"#),
            "{xml}"
        );
    }

    #[test]
    fn marker_symbol_and_size_roundtrip() {
        let chart = Chart {
            chart_type: ChartType::Line,
            series: vec![Series {
                marker_symbol: Some("diamond".into()),
                marker_size: Some(7),
                val_ref: Some("Sheet1!$B$1:$B$3".into()),
                ..Series::default()
            }],
            ..Chart::default()
        };
        let xml = write_chart_space(&chart);
        assert!(xml.contains(r#"<symbol val="diamond"/>"#), "{xml}");
        assert!(xml.contains(r#"<size val="7"/>"#), "{xml}");
    }

    #[test]
    fn marker_defaults_to_symbol_none() {
        let chart = Chart {
            chart_type: ChartType::Line,
            series: vec![Series {
                val_ref: Some("Sheet1!$B$1:$B$3".into()),
                ..Series::default()
            }],
            ..Chart::default()
        };
        let xml = write_chart_space(&chart);
        assert!(xml.contains(r#"<symbol val="none"/>"#), "{xml}");
    }

    #[test]
    fn smooth_emitted_for_line_series() {
        let chart = Chart {
            chart_type: ChartType::Line,
            series: vec![Series {
                smooth: Some(true),
                val_ref: Some("Sheet1!$B$1:$B$3".into()),
                ..Series::default()
            }],
            ..Chart::default()
        };
        let xml = write_chart_space(&chart);
        assert!(xml.contains(r#"<smooth val="1"/>"#), "{xml}");
    }

    #[test]
    fn smooth_defaults_to_straight_lines() {
        let chart = Chart {
            chart_type: ChartType::Scatter,
            series: vec![Series {
                x_ref: Some("Sheet1!$A$1:$A$3".into()),
                y_ref: Some("Sheet1!$B$1:$B$3".into()),
                ..Series::default()
            }],
            ..Chart::default()
        };
        let xml = write_chart_space(&chart);
        assert!(xml.contains(r#"<smooth val="0"/>"#), "{xml}");
    }

    #[test]
    fn ser_child_order_with_all_features() {
        let chart = Chart {
            chart_type: ChartType::Line,
            series: vec![Series {
                title_literal: Some("Sales".into()),
                colour: Some("0000FF".into()),
                marker_symbol: Some("circle".into()),
                marker_size: Some(5),
                cat_ref: Some("Sheet1!$A$1:$A$3".into()),
                val_ref: Some("Sheet1!$B$1:$B$3".into()),
                smooth: Some(true),
                ..Series::default()
            }],
            ..Chart::default()
        };
        let xml = write_chart_space(&chart);
        let ser = ser_block(&xml);
        let order = [
            "<idx", "<order", "<tx>", "<spPr>", "<marker>", "<cat>", "<val>", "<smooth",
        ];
        let mut prev = 0usize;
        for needle in order {
            let at = idx_of(ser, needle);
            assert!(at > prev, "element {needle:?} out of order in {ser:?}");
            prev = at;
        }
    }
}
