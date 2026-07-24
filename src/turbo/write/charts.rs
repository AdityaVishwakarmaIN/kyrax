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

#[derive(Clone, Debug, Default)]
pub struct Series {
    pub title_ref: Option<String>,
    pub title_literal: Option<String>,
    pub cat_ref: Option<String>,
    pub val_ref: Option<String>,
    pub x_ref: Option<String>,
    pub y_ref: Option<String>,
    pub bubble_size_ref: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Anchor {
    OneCell {
        cell: String,
        width_cm: f64,
        height_cm: f64,
    },
    TwoCell {
        from_cell: String,
        to_cell: String,
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
        }
    }
}

#[derive(Clone, Debug)]
pub struct ChartsheetSpec {
    pub title: String,
    pub charts: Vec<Chart>,
}

/// cm → EMU (openpyxl: 1 cm = 360000 EMU).
#[inline]
pub fn cm_to_emu(cm: f64) -> i64 {
    (cm * 360000.0).round() as i64
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

fn series_xml(ser: &Series, idx: usize, ct: &ChartType) -> String {
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
    s.push_str(r#"<spPr><a:ln><a:prstDash val="solid"/></a:ln></spPr>"#);
    if matches!(
        ct,
        ChartType::Line | ChartType::Line3D | ChartType::Scatter | ChartType::Radar
    ) {
        s.push_str(
            r#"<marker><symbol val="none"/><spPr><a:ln><a:prstDash val="solid"/></a:ln></spPr></marker>"#,
        );
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
    s.push_str("</ser>");
    s
}

fn plot_chart_xml(chart: &Chart) -> String {
    let tag = chart.chart_type.tag();
    let mut s = format!("<{tag}>");
    if let Some(dir) = chart.chart_type.bar_dir() {
        s.push_str(&format!(r#"<barDir val="{dir}"/>"#));
        s.push_str(r#"<grouping val="clustered"/>"#);
    }
    match chart.chart_type {
        ChartType::Line | ChartType::Line3D => {
            s.push_str(r#"<grouping val="standard"/>"#);
        }
        ChartType::Area | ChartType::Area3D => {
            s.push_str(r#"<grouping val="standard"/>"#);
        }
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

/// Build drawing XML + chart rels.
pub fn write_drawing(charts: &[Chart], chart_paths: &[String]) -> (String, String) {
    let mut drawing = format!(
        r#"<wsDr xmlns:a="{DRAWING_NS}" xmlns:c="{CHART_NS}" xmlns:r="{REL_NS}" xmlns="{SHEET_DRAWING_NS}">"#
    );
    for (i, chart) in charts.iter().enumerate() {
        let rid = i + 1;
        let frame = graphic_frame(rid);
        match &chart.anchor {
            Anchor::OneCell {
                cell,
                width_cm,
                height_cm,
            } => {
                let (row, col) = coord_to_tuple(cell);
                let cx = cm_to_emu(*width_cm);
                let cy = cm_to_emu(*height_cm);
                drawing.push_str(&format!(
                    r#"<oneCellAnchor><from><col>{}</col><colOff>0</colOff><row>{}</row><rowOff>0</rowOff></from><ext cx="{cx}" cy="{cy}"/>{frame}<clientData/></oneCellAnchor>"#,
                    col - 1,
                    row - 1,
                ));
            }
            Anchor::TwoCell { from_cell, to_cell } => {
                let (fr, fc) = coord_to_tuple(from_cell);
                let (tr, tc) = coord_to_tuple(to_cell);
                drawing.push_str(&format!(
                    r#"<twoCellAnchor><from><col>{}</col><colOff>0</colOff><row>{}</row><rowOff>0</rowOff></from><to><col>{}</col><colOff>0</colOff><row>{}</row><rowOff>0</rowOff></to>{frame}<clientData/></twoCellAnchor>"#,
                    fc - 1,
                    fr - 1,
                    tc - 1,
                    tr - 1,
                ));
            }
            Anchor::Absolute {
                x_emu,
                y_emu,
                cx_emu,
                cy_emu,
            } => {
                drawing.push_str(&format!(
                    r#"<absoluteAnchor><pos x="{x_emu}" y="{y_emu}"/><ext cx="{cx_emu}" cy="{cy_emu}"/>{frame}<clientData/></absoluteAnchor>"#
                ));
            }
        }
    }
    drawing.push_str("</wsDr>");

    let mut rels = format!(r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="{PKG_REL_NS}">"#);
    for (i, path) in chart_paths.iter().enumerate() {
        let rid = i + 1;
        rels.push_str(&format!(
            r#"<Relationship Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart" Target="{}" Id="rId{rid}"/>"#,
            escape_attr(path)
        ));
    }
    rels.push_str("</Relationships>");
    (drawing, rels)
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
