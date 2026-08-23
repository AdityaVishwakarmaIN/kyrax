//! PyO3 bindings for the turbo write path.

use pyo3::{
    Bound, PyAny, PyResult, Python,
    exceptions::{PyTypeError, PyValueError},
    prelude::*,
    types::{PyBytes, PyDict, PyList, PyString, PyTuple},
};
use std::sync::{Arc, OnceLock};

use super::cf_dv::{CfRule, CfRuleKind, CfVo, ConditionalFormatting, DataValidation};
use super::charts::{Anchor, Chart, ChartType, ChartsheetSpec, Grouping, Series};
use super::model::*;
use super::pivot::{PivotAgg, PivotDataField, PivotField, PivotTableSpec};
use super::rich_text::{RichRun, RichText, RunFont};
use super::style_engine::{
    AlignDesc, BorderDesc, ColorSpec, DxfDesc, FillDesc, FontDesc, GradientKind, GradientStop,
    ProtDesc, SideDesc, StyleDesc,
};
use super::writer::{
    date_to_serial, datetime_to_serial, save_workbook, save_workbook_stream, write_workbook_bytes,
};
use crate::error::{KyraxError, KyraxErrorKind};
use crate::turbo::error::{TurboError, TurboResult};
use crate::turbo::meta::{AutoFilterMeta, FilterColumnMeta};
use crate::turbo::overlay::{WorkbookOverlay, hydrate_sheet_from_xml};
use crate::turbo::scan::{MAX_GRID_COLS, MAX_GRID_ROWS, parse_ref_range_strict};
use crate::turbo::structural::parse_range;
use crate::turbo::zipmin::{ArchiveMap, read_entry};

fn write_err_to_py(err: std::io::Error) -> PyErr {
    let fe: KyraxError = KyraxErrorKind::Internal(format!("write error: {err}")).into();
    fe.into()
}

fn turbo_err_to_py(err: TurboError) -> PyErr {
    let fe: KyraxError = match &err {
        // A refused row/column operation is a caller error, not an internal
        // failure: surface it as InvalidParameters so the reason (table header
        // row, shared-formula master, grid limit, ...) is discoverable.
        TurboError::Refused(msg) => KyraxErrorKind::InvalidParameters(msg.clone()).into(),
        _ => KyraxErrorKind::Internal(err.to_string()).into(),
    };
    fe.into()
}

fn parse_string_mode(s: &str) -> PyResult<StringMode> {
    match s {
        "inline" | "inlineStr" | "inline_str" => Ok(StringMode::InlineStr),
        "sst" | "shared" | "sharedStrings" => Ok(StringMode::SharedStrings),
        "auto" => Ok(StringMode::Auto),
        other => Err(PyValueError::new_err(format!(
            "string_mode must be 'inline', 'sst', or 'auto'; got {other:?}"
        ))),
    }
}

fn parse_write_features(features: Option<&Bound<'_, PyAny>>) -> PyResult<WriteFeatures> {
    let Some(obj) = features else {
        return Ok(WriteFeatures::CORE);
    };
    if let Ok(s) = obj.extract::<String>() {
        return match s.as_str() {
            "core" | "values" => Ok(WriteFeatures::CORE),
            "all" => Ok(WriteFeatures::ALL),
            "styles" => Ok(WriteFeatures::WITH_STYLES),
            other => Err(PyValueError::new_err(format!(
                "unknown write features string {other:?}; expected \"core\", \"all\", or \"styles\""
            ))),
        };
    }
    if let Ok(list) = obj.extract::<Vec<String>>() {
        let mut f = WriteFeatures::VALUES;
        for name in &list {
            f = f.union(match name.as_str() {
                "values" => WriteFeatures::VALUES,
                "formulas" => WriteFeatures::FORMULAS,
                "dims" | "sheet_meta" => WriteFeatures::DIMS,
                "styles" => WriteFeatures::STYLES,
                "merges" => WriteFeatures::MERGES,
                "hyperlinks" => WriteFeatures::HYPERLINKS,
                "comments" => WriteFeatures::COMMENTS,
                "tables" => WriteFeatures::TABLES,
                "defined_names" => WriteFeatures::DEFINED_NAMES,
                "cf_dv" | "cond_format" | "validations" => WriteFeatures::CF_DV,
                "charts" => WriteFeatures::CHARTS,
                "images" => WriteFeatures::IMAGES,
                "pivots" => WriteFeatures::PIVOTS,
                "props" | "workbook_meta" => WriteFeatures::PROPS,
                "all" => WriteFeatures::ALL,
                "core" => WriteFeatures::CORE,
                other => {
                    return Err(PyValueError::new_err(format!(
                        "unknown write feature {other:?}"
                    )));
                }
            });
        }
        // Always enable core content path
        return Ok(f.union(WriteFeatures::CORE));
    }
    Err(PyValueError::new_err(
        "features must be \"core\", \"all\", or a list of feature names",
    ))
}

// ---------------------------------------------------------------------------
// Style / CF / DV / rich-text Python parsers (W2)
// ---------------------------------------------------------------------------

fn opt_str(d: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<String>> {
    d.get_item(key)?.map(|v| v.extract::<String>()).transpose()
}

fn opt_bool(d: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<bool>> {
    d.get_item(key)?.map(|v| v.extract::<bool>()).transpose()
}

fn opt_f64(d: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<f64>> {
    d.get_item(key)?.map(|v| v.extract::<f64>()).transpose()
}

fn opt_i32(d: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<i32>> {
    d.get_item(key)?.map(|v| v.extract::<i32>()).transpose()
}

fn opt_u32(d: &Bound<'_, PyDict>, key: &str) -> PyResult<Option<u32>> {
    d.get_item(key)?.map(|v| v.extract::<u32>()).transpose()
}

fn parse_color(obj: &Bound<'_, PyAny>) -> PyResult<ColorSpec> {
    if let Ok(s) = obj.extract::<String>() {
        if let Some(rest) = s.strip_prefix("theme:") {
            let t: u32 = rest.parse().unwrap_or(0);
            return Ok(ColorSpec::theme(t));
        }
        return Ok(ColorSpec::from_rgb_hex(&s));
    }
    if let Ok(d) = obj.cast::<PyDict>() {
        if let Some(rgb) = d.get_item("rgb")? {
            return Ok(ColorSpec::from_rgb_hex(&rgb.extract::<String>()?));
        }
        if let Some(t) = d.get_item("theme")? {
            let idx: u32 = t.extract()?;
            let tint: f64 = d
                .get_item("tint")?
                .map(|v| v.extract())
                .transpose()?
                .unwrap_or(0.0);
            return Ok(ColorSpec::theme_tinted(idx, tint));
        }
        if let Some(i) = d.get_item("indexed")? {
            return Ok(ColorSpec::Indexed(i.extract()?));
        }
    }
    Err(PyValueError::new_err(
        "color must be hex str or dict{rgb|theme|indexed}",
    ))
}

fn parse_font(obj: &Bound<'_, PyAny>) -> PyResult<FontDesc> {
    let d = obj
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("font must be a dict"))?;
    let mut f = FontDesc {
        name: opt_str(d, "name")?,
        sz_bits: None,
        bold: opt_bool(d, "bold")?.or(opt_bool(d, "b")?),
        italic: opt_bool(d, "italic")?.or(opt_bool(d, "i")?),
        underline: opt_str(d, "underline")?.or(opt_str(d, "u")?),
        strike: opt_bool(d, "strike")?,
        outline: opt_bool(d, "outline")?,
        shadow: opt_bool(d, "shadow")?,
        condense: opt_bool(d, "condense")?,
        extend: opt_bool(d, "extend")?,
        color: None,
        family: opt_i32(d, "family")?,
        scheme: opt_str(d, "scheme")?,
        vert_align: opt_str(d, "vertAlign")?.or(opt_str(d, "vert_align")?),
        charset: opt_i32(d, "charset")?,
    };
    if let Some(sz) = opt_f64(d, "sz")?.or(opt_f64(d, "size")?) {
        f.set_sz(sz);
    }
    if let Some(c) = d.get_item("color")? {
        f.color = Some(parse_color(&c)?);
    }
    Ok(f)
}

fn parse_fill(obj: &Bound<'_, PyAny>) -> PyResult<FillDesc> {
    let d = obj
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("fill must be a dict"))?;
    // Gradient fill detection: openpyxl GradientFill uses type/degree/stops,
    // pattern fills use patternType. A "type" in {linear, path} or any of the
    // gradient-only keys routes to the gradient parser.
    let type_key = opt_str(d, "type")?.or(opt_str(d, "fill_type")?);
    let is_gradient = matches!(type_key.as_deref(), Some("linear") | Some("path"))
        || d.get_item("degree")?.is_some()
        || d.get_item("stops")?.is_some()
        || d.get_item("stop")?.is_some();
    if is_gradient {
        return parse_gradient_fill(d);
    }
    let pattern = opt_str(d, "patternType")?
        .or(opt_str(d, "pattern_type")?)
        .or(opt_str(d, "fill_type")?)
        .unwrap_or_else(|| "solid".into());
    let mut fg = None;
    let mut bg = None;
    if let Some(c) = d
        .get_item("fgColor")?
        .or(d.get_item("fg_color")?)
        .or(d.get_item("fgColorRgb")?)
        .or(d.get_item("start_color")?)
        .or(d.get_item("color")?)
    {
        fg = Some(parse_color(&c)?);
    }
    if let Some(c) = d
        .get_item("bgColor")?
        .or(d.get_item("bg_color")?)
        .or(d.get_item("end_color")?)
    {
        bg = Some(parse_color(&c)?);
    }
    // allow fg as plain hex under "fg"
    if fg.is_none() {
        if let Some(c) = d.get_item("fg")? {
            fg = Some(parse_color(&c)?);
        }
    }
    Ok(FillDesc::Pattern {
        pattern_type: Some(pattern),
        fg,
        bg,
    })
}

fn parse_gradient_fill(d: &Bound<'_, PyDict>) -> PyResult<FillDesc> {
    let kind_s = opt_str(d, "type")?
        .or(opt_str(d, "fill_type")?)
        .unwrap_or_else(|| "linear".into());
    let stops = parse_gradient_stops(d)?;
    match kind_s.as_str() {
        "linear" => {
            let degree = opt_f64(d, "degree")?.unwrap_or(0.0);
            Ok(FillDesc::Gradient {
                kind: GradientKind::linear(degree),
                stops,
            })
        }
        "path" => {
            let val = |key: &str| -> f64 { opt_f64(d, key).ok().flatten().unwrap_or(0.0) };
            Ok(FillDesc::Gradient {
                kind: GradientKind::path(val("left"), val("right"), val("top"), val("bottom")),
                stops,
            })
        }
        other => Err(PyValueError::new_err(format!(
            "gradient type must be 'linear' or 'path'; got {other:?}"
        ))),
    }
}

/// Stops as a list of `{position, color}` dicts, or a list of plain colors
/// (positions auto-assigned evenly, matching openpyxl `_assign_position`).
fn parse_gradient_stops(d: &Bound<'_, PyDict>) -> PyResult<Vec<GradientStop>> {
    let Some(stops_obj) = d.get_item("stops")?.or(d.get_item("stop")?) else {
        return Err(PyValueError::new_err(
            "gradient fill needs 'stops' (list of {position, color} dicts or colors)",
        ));
    };
    let items: Vec<Bound<'_, PyAny>> = if let Ok(list) = stops_obj.cast::<PyList>() {
        list.iter().collect()
    } else {
        vec![stops_obj.clone()]
    };
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let mut all_colors = true;
    for item in &items {
        if item.extract::<String>().is_err() {
            all_colors = false;
            break;
        }
    }
    let mut stops = Vec::with_capacity(items.len());
    if all_colors {
        // evenly spaced positions
        let n = items.len();
        let interval = if n > 2 { 1.0 / ((n - 1) as f64) } else { 1.0 };
        for (i, item) in items.iter().enumerate() {
            stops.push(GradientStop::new((i as f64) * interval, parse_color(item)?));
        }
    } else {
        for item in &items {
            let sd = item.cast::<PyDict>().map_err(|_| {
                PyValueError::new_err("each gradient stop must be a {position, color} dict")
            })?;
            let position: f64 = sd
                .get_item("position")?
                .map(|p| p.extract())
                .transpose()?
                .unwrap_or(0.0);
            let color_obj = sd
                .get_item("color")?
                .ok_or_else(|| PyValueError::new_err("gradient stop missing 'color'"))?;
            stops.push(GradientStop::new(position, parse_color(&color_obj)?));
        }
    }
    Ok(stops)
}

fn parse_side(obj: &Bound<'_, PyAny>) -> PyResult<SideDesc> {
    if let Ok(s) = obj.extract::<String>() {
        return Ok(SideDesc {
            style: Some(s),
            color: None,
        });
    }
    let d = obj
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("border side must be str or dict"))?;
    let mut side = SideDesc {
        style: opt_str(d, "style")?.or(opt_str(d, "border_style")?),
        color: None,
    };
    if let Some(c) = d.get_item("color")? {
        side.color = Some(parse_color(&c)?);
    }
    Ok(side)
}

fn parse_border(obj: &Bound<'_, PyAny>) -> PyResult<BorderDesc> {
    let d = obj
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("border must be a dict"))?;
    // shorthand: {"style":"thin","color":"FF0000"} applies to all sides
    if d.get_item("left")?.is_none()
        && d.get_item("right")?.is_none()
        && d.get_item("top")?.is_none()
        && d.get_item("bottom")?.is_none()
    {
        if let Some(st) = opt_str(d, "style")? {
            let color = d.get_item("color")?.map(|c| parse_color(&c)).transpose()?;
            let side = SideDesc {
                style: Some(st),
                color,
            };
            return Ok(BorderDesc {
                left: Some(side.clone()),
                right: Some(side.clone()),
                top: Some(side.clone()),
                bottom: Some(side),
                diagonal: None,
                diagonal_up: false,
                diagonal_down: false,
                outline: true,
                emit_empty_sides: false,
            });
        }
    }
    let mut b = BorderDesc::default();
    if let Some(s) = d.get_item("left")? {
        b.left = Some(parse_side(&s)?);
    }
    if let Some(s) = d.get_item("right")? {
        b.right = Some(parse_side(&s)?);
    }
    if let Some(s) = d.get_item("top")? {
        b.top = Some(parse_side(&s)?);
    }
    if let Some(s) = d.get_item("bottom")? {
        b.bottom = Some(parse_side(&s)?);
    }
    if let Some(s) = d.get_item("diagonal")? {
        b.diagonal = Some(parse_side(&s)?);
    }
    Ok(b)
}

fn parse_align(obj: &Bound<'_, PyAny>) -> PyResult<AlignDesc> {
    let d = obj
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("alignment must be a dict"))?;
    Ok(AlignDesc {
        horizontal: opt_str(d, "horizontal")?,
        vertical: opt_str(d, "vertical")?,
        text_rotation: opt_i32(d, "textRotation")?
            .or(opt_i32(d, "text_rotation")?)
            .unwrap_or(0),
        wrap_text: opt_bool(d, "wrapText")?.or(opt_bool(d, "wrap_text")?),
        shrink_to_fit: opt_bool(d, "shrinkToFit")?.or(opt_bool(d, "shrink_to_fit")?),
        indent: opt_i32(d, "indent")?.unwrap_or(0),
        relative_indent: opt_i32(d, "relativeIndent")?
            .or(opt_i32(d, "relative_indent")?)
            .unwrap_or(0),
        justify_last_line: opt_bool(d, "justifyLastLine")?.or(opt_bool(d, "justify_last_line")?),
        reading_order: opt_i32(d, "readingOrder")?
            .or(opt_i32(d, "reading_order")?)
            .unwrap_or(0),
    })
}

fn parse_prot(obj: &Bound<'_, PyAny>) -> PyResult<ProtDesc> {
    let d = obj
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("protection must be a dict"))?;
    Ok(ProtDesc {
        locked: opt_bool(d, "locked")?.unwrap_or(true),
        hidden: opt_bool(d, "hidden")?.unwrap_or(false),
    })
}

fn parse_style_desc(obj: &Bound<'_, PyAny>) -> PyResult<StyleDesc> {
    let d = obj
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("style must be a dict with font/fill/border/..."))?;
    let mut desc = StyleDesc::default();
    if let Some(f) = d.get_item("font")? {
        desc.font = Some(parse_font(&f)?);
    }
    if let Some(f) = d.get_item("fill")? {
        desc.fill = Some(parse_fill(&f)?);
    }
    if let Some(b) = d.get_item("border")? {
        desc.border = Some(parse_border(&b)?);
    }
    desc.num_fmt = opt_str(d, "num_fmt")?
        .or(opt_str(d, "numFmt")?)
        .or(opt_str(d, "number_format")?)
        .or(opt_str(d, "numberFormat")?);
    if let Some(a) = d.get_item("alignment")? {
        desc.alignment = Some(parse_align(&a)?);
    }
    if let Some(p) = d.get_item("protection")? {
        desc.protection = Some(parse_prot(&p)?);
    }
    desc.named_style = opt_str(d, "named_style")?
        .or(opt_str(d, "style")?)
        .or(opt_str(d, "namedStyle")?);
    desc.quote_prefix = opt_bool(d, "quotePrefix")?
        .or(opt_bool(d, "quote_prefix")?)
        .unwrap_or(false);
    desc.pivot_button = opt_bool(d, "pivotButton")?
        .or(opt_bool(d, "pivot_button")?)
        .unwrap_or(false);
    Ok(desc)
}

fn parse_dxf(obj: &Bound<'_, PyAny>) -> PyResult<DxfDesc> {
    let d = obj
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("dxf must be a dict"))?;
    let mut dxf = DxfDesc::default();
    if let Some(f) = d.get_item("font")? {
        dxf.font = Some(parse_font(&f)?);
    }
    if let Some(f) = d.get_item("fill")? {
        dxf.fill = Some(parse_fill(&f)?);
    }
    if let Some(b) = d.get_item("border")? {
        dxf.border = Some(parse_border(&b)?);
    }
    if let Some(a) = d.get_item("alignment")? {
        dxf.alignment = Some(parse_align(&a)?);
    }
    if let Some(p) = d.get_item("protection")? {
        dxf.protection = Some(parse_prot(&p)?);
    }
    Ok(dxf)
}

fn parse_run_font(obj: &Bound<'_, PyAny>) -> PyResult<RunFont> {
    let d = obj
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("run font must be a dict"))?;
    let mut rf = RunFont {
        r_font: opt_str(d, "rFont")?
            .or(opt_str(d, "r_font")?)
            .or(opt_str(d, "name")?),
        ..Default::default()
    };
    rf.sz = opt_f64(d, "sz")?.or(opt_f64(d, "size")?);
    rf.bold = opt_bool(d, "bold")?.or(opt_bool(d, "b")?);
    rf.italic = opt_bool(d, "italic")?.or(opt_bool(d, "i")?);
    rf.underline = opt_str(d, "underline")?;
    rf.strike = opt_bool(d, "strike")?;
    rf.vert_align = opt_str(d, "vertAlign")?.or(opt_str(d, "vert_align")?);
    if let Some(c) = d.get_item("color")? {
        rf.color = Some(parse_color(&c)?);
    }
    Ok(rf)
}

fn parse_rich_text(obj: &Bound<'_, PyAny>) -> PyResult<RichText> {
    // {"rich": [ "plain", {"text":"Bold","font":{...}}, ... ]}
    // or list of runs directly
    let runs_obj = if let Ok(d) = obj.cast::<PyDict>() {
        d.get_item("rich")?
            .or(d.get_item("runs")?)
            .ok_or_else(|| PyValueError::new_err("rich text dict needs 'rich' or 'runs'"))?
    } else {
        obj.clone()
    };
    let list = runs_obj
        .cast::<PyList>()
        .map_err(|_| PyValueError::new_err("rich text runs must be a list"))?;
    let mut runs = Vec::new();
    for item in list.iter() {
        if let Ok(s) = item.extract::<String>() {
            runs.push(RichRun::Text(s));
            continue;
        }
        let d = item
            .cast::<PyDict>()
            .map_err(|_| PyValueError::new_err("rich run must be str or dict{text, font?}"))?;
        let text: String = d
            .get_item("text")?
            .ok_or_else(|| PyValueError::new_err("rich run missing text"))?
            .extract()?;
        if let Some(f) = d.get_item("font")? {
            runs.push(RichRun::Block {
                font: parse_run_font(&f)?,
                text,
            });
        } else {
            runs.push(RichRun::Text(text));
        }
    }
    Ok(RichText { runs })
}

fn parse_cfvo(obj: &Bound<'_, PyAny>) -> PyResult<CfVo> {
    let d = obj
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("cfvo must be dict{type, val?}"))?;
    Ok(CfVo {
        type_: opt_str(d, "type")?
            .or(opt_str(d, "type_")?)
            .unwrap_or_else(|| "min".into()),
        val: opt_str(d, "val")?.or(opt_str(d, "value")?),
    })
}

fn parse_cf_rule(obj: &Bound<'_, PyAny>, priority: u32) -> PyResult<CfRule> {
    let d = obj
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("cf rule must be a dict"))?;
    let type_ = opt_str(d, "type")?
        .or(opt_str(d, "type_")?)
        .unwrap_or_else(|| "cellIs".into());
    let prio = d
        .get_item("priority")?
        .map(|x| x.extract::<u32>())
        .transpose()?
        .unwrap_or(priority);
    let kind = match type_.as_str() {
        "colorScale" | "color_scale" => {
            let mut cfvos = Vec::new();
            if let Some(vs) = d.get_item("cfvos")?.or(d.get_item("cfvo")?) {
                for v in vs.cast::<PyList>()?.iter() {
                    cfvos.push(parse_cfvo(&v)?);
                }
            }
            let mut colors = Vec::new();
            if let Some(cs) = d.get_item("colors")? {
                for c in cs.cast::<PyList>()?.iter() {
                    colors.push(parse_color(&c)?);
                }
            }
            CfRuleKind::ColorScale { cfvos, colors }
        }
        "dataBar" | "data_bar" => {
            let mut cfvos = Vec::new();
            if let Some(vs) = d.get_item("cfvos")?.or(d.get_item("cfvo")?) {
                for v in vs.cast::<PyList>()?.iter() {
                    cfvos.push(parse_cfvo(&v)?);
                }
            }
            let color = if let Some(c) = d.get_item("color")? {
                parse_color(&c)?
            } else {
                ColorSpec::from_rgb_hex("638EC6")
            };
            CfRuleKind::DataBar {
                cfvos,
                color,
                show_value: opt_bool(d, "showValue")?.or(opt_bool(d, "show_value")?),
                min_length: opt_u32(d, "minLength")?.or(opt_u32(d, "min_length")?),
                max_length: opt_u32(d, "maxLength")?.or(opt_u32(d, "max_length")?),
            }
        }
        "iconSet" | "icon_set" => {
            let mut cfvos = Vec::new();
            if let Some(vs) = d.get_item("cfvos")?.or(d.get_item("cfvo")?) {
                for v in vs.cast::<PyList>()?.iter() {
                    cfvos.push(parse_cfvo(&v)?);
                }
            }
            CfRuleKind::IconSet {
                icon_set: opt_str(d, "iconSet")?
                    .or(opt_str(d, "icon_set")?)
                    .unwrap_or_else(|| "3TrafficLights1".into()),
                cfvos,
                show_value: opt_bool(d, "showValue")?.or(opt_bool(d, "show_value")?),
                reverse: opt_bool(d, "reverse")?,
                custom: opt_bool(d, "custom")?,
                percent: opt_bool(d, "percent")?,
            }
        }
        "cellIs" | "cell_is" => {
            let mut formulas = Vec::new();
            if let Some(fs) = d.get_item("formulas")? {
                for f in fs.cast::<PyList>()?.iter() {
                    formulas.push(f.extract::<String>()?);
                }
            } else if let Some(f) = d.get_item("formula")? {
                formulas.push(f.extract()?);
            }
            let dxf = if let Some(dx) = d.get_item("dxf")? {
                parse_dxf(&dx)?
            } else {
                DxfDesc::default()
            };
            CfRuleKind::CellIs {
                operator: opt_str(d, "operator")?.unwrap_or_else(|| "greaterThan".into()),
                formulas,
                dxf,
                stop_if_true: opt_bool(d, "stopIfTrue")?.or(opt_bool(d, "stop_if_true")?),
            }
        }
        "expression" => {
            let mut formulas = Vec::new();
            if let Some(fs) = d.get_item("formulas")? {
                for f in fs.cast::<PyList>()?.iter() {
                    formulas.push(f.extract::<String>()?);
                }
            } else if let Some(f) = d.get_item("formula")? {
                formulas.push(f.extract()?);
            }
            let dxf = if let Some(dx) = d.get_item("dxf")? {
                parse_dxf(&dx)?
            } else {
                DxfDesc::default()
            };
            CfRuleKind::Expression {
                formulas,
                dxf,
                stop_if_true: opt_bool(d, "stopIfTrue")?.or(opt_bool(d, "stop_if_true")?),
            }
        }
        "top10" | "top_10" => {
            let rank = opt_u32(d, "rank")?.unwrap_or(10);
            let percent = opt_bool(d, "percent")?;
            let bottom = opt_bool(d, "bottom")?;
            let dxf = if let Some(dx) = d.get_item("dxf")? {
                parse_dxf(&dx)?
            } else {
                DxfDesc::default()
            };
            CfRuleKind::Top10 {
                rank,
                percent,
                bottom,
                dxf,
                stop_if_true: opt_bool(d, "stopIfTrue")?.or(opt_bool(d, "stop_if_true")?),
            }
        }
        "aboveAverage" | "above_average" => {
            let above_average = opt_bool(d, "aboveAverage")?.or(opt_bool(d, "above_average")?);
            let equal_average = opt_bool(d, "equalAverage")?.or(opt_bool(d, "equal_average")?);
            let std_dev = opt_i32(d, "stdDev")?.or(opt_i32(d, "std_dev")?);
            let dxf = if let Some(dx) = d.get_item("dxf")? {
                parse_dxf(&dx)?
            } else {
                DxfDesc::default()
            };
            CfRuleKind::AboveAverage {
                above_average,
                equal_average,
                std_dev,
                dxf,
                stop_if_true: opt_bool(d, "stopIfTrue")?.or(opt_bool(d, "stop_if_true")?),
            }
        }
        "uniqueValues" | "unique_values" => {
            let dxf = if let Some(dx) = d.get_item("dxf")? {
                parse_dxf(&dx)?
            } else {
                DxfDesc::default()
            };
            CfRuleKind::UniqueValues {
                dxf,
                stop_if_true: opt_bool(d, "stopIfTrue")?.or(opt_bool(d, "stop_if_true")?),
            }
        }
        "duplicateValues" | "duplicate_values" => {
            let dxf = if let Some(dx) = d.get_item("dxf")? {
                parse_dxf(&dx)?
            } else {
                DxfDesc::default()
            };
            CfRuleKind::DuplicateValues {
                dxf,
                stop_if_true: opt_bool(d, "stopIfTrue")?.or(opt_bool(d, "stop_if_true")?),
            }
        }
        "containsText" | "contains_text" => {
            let text = opt_str(d, "text")?.unwrap_or_default();
            let operator = opt_str(d, "operator")?;
            let mut formulas = Vec::new();
            if let Some(fs) = d.get_item("formulas")? {
                for f in fs.cast::<PyList>()?.iter() {
                    formulas.push(f.extract::<String>()?);
                }
            } else if let Some(f) = d.get_item("formula")? {
                formulas.push(f.extract()?);
            }
            let dxf = if let Some(dx) = d.get_item("dxf")? {
                parse_dxf(&dx)?
            } else {
                DxfDesc::default()
            };
            CfRuleKind::ContainsText {
                text,
                operator,
                formulas,
                dxf,
                stop_if_true: opt_bool(d, "stopIfTrue")?.or(opt_bool(d, "stop_if_true")?),
            }
        }
        "notContainsText" | "not_contains_text" => {
            let text = opt_str(d, "text")?.unwrap_or_default();
            let operator = opt_str(d, "operator")?;
            let mut formulas = Vec::new();
            if let Some(fs) = d.get_item("formulas")? {
                for f in fs.cast::<PyList>()?.iter() {
                    formulas.push(f.extract::<String>()?);
                }
            } else if let Some(f) = d.get_item("formula")? {
                formulas.push(f.extract()?);
            }
            let dxf = if let Some(dx) = d.get_item("dxf")? {
                parse_dxf(&dx)?
            } else {
                DxfDesc::default()
            };
            CfRuleKind::NotContainsText {
                text,
                operator,
                formulas,
                dxf,
                stop_if_true: opt_bool(d, "stopIfTrue")?.or(opt_bool(d, "stop_if_true")?),
            }
        }
        "beginsWith" | "begins_with" => {
            let text = opt_str(d, "text")?.unwrap_or_default();
            let operator = opt_str(d, "operator")?;
            let mut formulas = Vec::new();
            if let Some(fs) = d.get_item("formulas")? {
                for f in fs.cast::<PyList>()?.iter() {
                    formulas.push(f.extract::<String>()?);
                }
            } else if let Some(f) = d.get_item("formula")? {
                formulas.push(f.extract()?);
            }
            let dxf = if let Some(dx) = d.get_item("dxf")? {
                parse_dxf(&dx)?
            } else {
                DxfDesc::default()
            };
            CfRuleKind::BeginsWith {
                text,
                operator,
                formulas,
                dxf,
                stop_if_true: opt_bool(d, "stopIfTrue")?.or(opt_bool(d, "stop_if_true")?),
            }
        }
        "endsWith" | "ends_with" => {
            let text = opt_str(d, "text")?.unwrap_or_default();
            let operator = opt_str(d, "operator")?;
            let mut formulas = Vec::new();
            if let Some(fs) = d.get_item("formulas")? {
                for f in fs.cast::<PyList>()?.iter() {
                    formulas.push(f.extract::<String>()?);
                }
            } else if let Some(f) = d.get_item("formula")? {
                formulas.push(f.extract()?);
            }
            let dxf = if let Some(dx) = d.get_item("dxf")? {
                parse_dxf(&dx)?
            } else {
                DxfDesc::default()
            };
            CfRuleKind::EndsWith {
                text,
                operator,
                formulas,
                dxf,
                stop_if_true: opt_bool(d, "stopIfTrue")?.or(opt_bool(d, "stop_if_true")?),
            }
        }
        "containsBlanks" | "contains_blanks" => {
            let mut formulas = Vec::new();
            if let Some(fs) = d.get_item("formulas")? {
                for f in fs.cast::<PyList>()?.iter() {
                    formulas.push(f.extract::<String>()?);
                }
            } else if let Some(f) = d.get_item("formula")? {
                formulas.push(f.extract()?);
            }
            let dxf = if let Some(dx) = d.get_item("dxf")? {
                parse_dxf(&dx)?
            } else {
                DxfDesc::default()
            };
            CfRuleKind::ContainsBlanks {
                formulas,
                dxf,
                stop_if_true: opt_bool(d, "stopIfTrue")?.or(opt_bool(d, "stop_if_true")?),
            }
        }
        "notContainsBlanks" | "not_contains_blanks" => {
            let mut formulas = Vec::new();
            if let Some(fs) = d.get_item("formulas")? {
                for f in fs.cast::<PyList>()?.iter() {
                    formulas.push(f.extract::<String>()?);
                }
            } else if let Some(f) = d.get_item("formula")? {
                formulas.push(f.extract()?);
            }
            let dxf = if let Some(dx) = d.get_item("dxf")? {
                parse_dxf(&dx)?
            } else {
                DxfDesc::default()
            };
            CfRuleKind::NotContainsBlanks {
                formulas,
                dxf,
                stop_if_true: opt_bool(d, "stopIfTrue")?.or(opt_bool(d, "stop_if_true")?),
            }
        }
        "containsErrors" | "contains_errors" => {
            let mut formulas = Vec::new();
            if let Some(fs) = d.get_item("formulas")? {
                for f in fs.cast::<PyList>()?.iter() {
                    formulas.push(f.extract::<String>()?);
                }
            } else if let Some(f) = d.get_item("formula")? {
                formulas.push(f.extract()?);
            }
            let dxf = if let Some(dx) = d.get_item("dxf")? {
                parse_dxf(&dx)?
            } else {
                DxfDesc::default()
            };
            CfRuleKind::ContainsErrors {
                formulas,
                dxf,
                stop_if_true: opt_bool(d, "stopIfTrue")?.or(opt_bool(d, "stop_if_true")?),
            }
        }
        "notContainsErrors" | "not_contains_errors" => {
            let mut formulas = Vec::new();
            if let Some(fs) = d.get_item("formulas")? {
                for f in fs.cast::<PyList>()?.iter() {
                    formulas.push(f.extract::<String>()?);
                }
            } else if let Some(f) = d.get_item("formula")? {
                formulas.push(f.extract()?);
            }
            let dxf = if let Some(dx) = d.get_item("dxf")? {
                parse_dxf(&dx)?
            } else {
                DxfDesc::default()
            };
            CfRuleKind::NotContainsErrors {
                formulas,
                dxf,
                stop_if_true: opt_bool(d, "stopIfTrue")?.or(opt_bool(d, "stop_if_true")?),
            }
        }
        "timePeriod" | "time_period" => {
            let time_period = opt_str(d, "timePeriod")?
                .or(opt_str(d, "time_period")?)
                .unwrap_or_else(|| "today".into());
            let mut formulas = Vec::new();
            if let Some(fs) = d.get_item("formulas")? {
                for f in fs.cast::<PyList>()?.iter() {
                    formulas.push(f.extract::<String>()?);
                }
            } else if let Some(f) = d.get_item("formula")? {
                formulas.push(f.extract()?);
            }
            let dxf = if let Some(dx) = d.get_item("dxf")? {
                parse_dxf(&dx)?
            } else {
                DxfDesc::default()
            };
            CfRuleKind::TimePeriod {
                time_period,
                formulas,
                dxf,
                stop_if_true: opt_bool(d, "stopIfTrue")?.or(opt_bool(d, "stop_if_true")?),
            }
        }
        other => {
            return Err(PyValueError::new_err(format!(
                "unknown CF rule type {other:?}"
            )));
        }
    };
    Ok(CfRule {
        kind,
        priority: prio,
        dxf_id: None,
    })
}

fn fill_implied_formulas(sqref: &str, rules: &mut [CfRule]) -> PyResult<()> {
    let anchor = sqref.split_whitespace().next().unwrap_or("A1");
    let top_left = anchor.split(':').next().unwrap_or("A1");
    let cell_ref = top_left.split('!').last().unwrap_or("A1");

    for rule in rules.iter_mut() {
        match &mut rule.kind {
            CfRuleKind::ContainsText { text, formulas, .. } => {
                if formulas.is_empty() {
                    let escaped = text.replace('"', "\"\"");
                    formulas.push(format!("NOT(ISERROR(SEARCH(\"{escaped}\",{cell_ref})))"));
                }
            }
            CfRuleKind::NotContainsText { text, formulas, .. } => {
                if formulas.is_empty() {
                    let escaped = text.replace('"', "\"\"");
                    formulas.push(format!("ISERROR(SEARCH(\"{escaped}\",{cell_ref}))"));
                }
            }
            CfRuleKind::BeginsWith { text, formulas, .. } => {
                if formulas.is_empty() {
                    let escaped = text.replace('"', "\"\"");
                    let len = text.chars().count();
                    formulas.push(format!("LEFT({cell_ref},{len})=\"{escaped}\""));
                }
            }
            CfRuleKind::EndsWith { text, formulas, .. } => {
                if formulas.is_empty() {
                    let escaped = text.replace('"', "\"\"");
                    let len = text.chars().count();
                    formulas.push(format!("RIGHT({cell_ref},{len})=\"{escaped}\""));
                }
            }
            CfRuleKind::ContainsBlanks { formulas, .. } => {
                if formulas.is_empty() {
                    formulas.push(format!("ISBLANK({cell_ref})"));
                }
            }
            CfRuleKind::NotContainsBlanks { formulas, .. } => {
                if formulas.is_empty() {
                    formulas.push(format!("NOT(ISBLANK({cell_ref}))"));
                }
            }
            CfRuleKind::ContainsErrors { formulas, .. } => {
                if formulas.is_empty() {
                    formulas.push(format!("ISERROR({cell_ref})"));
                }
            }
            CfRuleKind::NotContainsErrors { formulas, .. } => {
                if formulas.is_empty() {
                    formulas.push(format!("NOT(ISERROR({cell_ref}))"));
                }
            }
            CfRuleKind::TimePeriod { time_period, formulas, .. } => {
                if formulas.is_empty() {
                    let f = match time_period.to_ascii_lowercase().as_str() {
                        "yesterday" => format!("FLOOR({cell_ref},1)=TODAY()-1"),
                        "today" => format!("FLOOR({cell_ref},1)=TODAY()"),
                        "tomorrow" => format!("FLOOR({cell_ref},1)=TODAY()+1"),
                        "last7days" => format!("AND(TODAY()-FLOOR({cell_ref},1)<=6,FLOOR({cell_ref},1)<=TODAY())"),
                        "lastweek" => format!("AND(TODAY()-ROUNDDOWN({cell_ref},0)>=WEEKDAY(TODAY()),TODAY()-ROUNDDOWN({cell_ref},0)<WEEKDAY(TODAY())+7)"),
                        "thisweek" => format!("AND(TODAY()-ROUNDDOWN({cell_ref},0)<=WEEKDAY(TODAY())-1,TODAY()-ROUNDDOWN({cell_ref},0)>=1-WEEKDAY(TODAY()))"),
                        "nextweek" => format!("AND(TODAY()-ROUNDDOWN({cell_ref},0)<=9-WEEKDAY(TODAY()),TODAY()-ROUNDDOWN({cell_ref},0)>=2-WEEKDAY(TODAY()))"),
                        "lastmonth" => format!("AND(MONTH(ROUNDDOWN({cell_ref},0))=MONTH(EDATE(TODAY(),-1)),YEAR(ROUNDDOWN({cell_ref},0))=YEAR(EDATE(TODAY(),-1)))"),
                        "thismonth" => format!("AND(MONTH(ROUNDDOWN({cell_ref},0))=MONTH(TODAY()),YEAR(ROUNDDOWN({cell_ref},0))=YEAR(TODAY()))"),
                        "nextmonth" => format!("AND(MONTH(ROUNDDOWN({cell_ref},0))=MONTH(EDATE(TODAY(),1)),YEAR(ROUNDDOWN({cell_ref},0))=YEAR(EDATE(TODAY(),1)))"),
                        unknown => {
                            return Err(PyValueError::new_err(format!("unknown time_period: {unknown}")));
                        }
                    };
                    formulas.push(f);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn parse_conditional_formatting(obj: &Bound<'_, PyAny>) -> PyResult<Vec<ConditionalFormatting>> {
    let list = obj
        .cast::<PyList>()
        .map_err(|_| PyValueError::new_err("conditional_formatting must be a list"))?;
    let mut out = Vec::new();
    for item in list.iter() {
        let d = item
            .cast::<PyDict>()
            .map_err(|_| PyValueError::new_err("CF item must be dict{sqref, rules}"))?;
        let sqref = opt_str(d, "sqref")?
            .or(opt_str(d, "range")?)
            .unwrap_or_else(|| "A1".into());
        let mut rules = Vec::new();
        if let Some(rs) = d.get_item("rules")? {
            for (i, r) in rs.cast::<PyList>()?.iter().enumerate() {
                rules.push(parse_cf_rule(&r, (i as u32) + 1)?);
            }
        } else {
            // single rule fields on the CF dict itself
            rules.push(parse_cf_rule(&item, 1)?);
        }
        fill_implied_formulas(&sqref, &mut rules)?;
        out.push(ConditionalFormatting { sqref, rules });
    }
    Ok(out)
}

fn parse_data_validations(obj: &Bound<'_, PyAny>) -> PyResult<Vec<DataValidation>> {
    let list = obj
        .cast::<PyList>()
        .map_err(|_| PyValueError::new_err("data_validations must be a list"))?;
    let mut out = Vec::new();
    for item in list.iter() {
        let d = item
            .cast::<PyDict>()
            .map_err(|_| PyValueError::new_err("DV item must be a dict"))?;
        out.push(DataValidation {
            type_: opt_str(d, "type")?.or(opt_str(d, "type_")?),
            operator: opt_str(d, "operator")?,
            formula1: opt_str(d, "formula1")?.or(opt_str(d, "formula")?),
            formula2: opt_str(d, "formula2")?,
            sqref: opt_str(d, "sqref")?
                .or(opt_str(d, "range")?)
                .unwrap_or_default(),
            allow_blank: opt_bool(d, "allow_blank")?
                .or(opt_bool(d, "allowBlank")?)
                .unwrap_or(true),
            show_error_message: opt_bool(d, "show_error_message")?
                .or(opt_bool(d, "showErrorMessage")?)
                .unwrap_or(false),
            show_input_message: opt_bool(d, "show_input_message")?
                .or(opt_bool(d, "showInputMessage")?)
                .unwrap_or(false),
            show_drop_down: opt_bool(d, "show_drop_down")?
                .or(opt_bool(d, "showDropDown")?)
                .unwrap_or(false),
            error_title: opt_str(d, "errorTitle")?.or(opt_str(d, "error_title")?),
            error: opt_str(d, "error")?,
            prompt_title: opt_str(d, "promptTitle")?.or(opt_str(d, "prompt_title")?),
            prompt: opt_str(d, "prompt")?,
        });
    }
    Ok(out)
}

fn parse_named_styles(obj: &Bound<'_, PyAny>) -> PyResult<Vec<NamedStyleInput>> {
    let list = obj
        .cast::<PyList>()
        .map_err(|_| PyValueError::new_err("named_styles must be a list of dicts"))?;
    let mut out = Vec::new();
    for item in list.iter() {
        let d = item
            .cast::<PyDict>()
            .map_err(|_| PyValueError::new_err("named style must be a dict"))?;
        let name =
            opt_str(d, "name")?.ok_or_else(|| PyValueError::new_err("named style needs name"))?;
        let desc = parse_style_desc(&item)?;
        let builtin_id = opt_i32(d, "builtinId")?.or(opt_i32(d, "builtin_id")?);
        out.push(NamedStyleInput {
            name,
            desc,
            builtin_id,
        });
    }
    Ok(out)
}

/// Apply cell_styles map: {(row0,col0): style_dict} or list of {row,col,style}
fn apply_cell_styles(sheet: &mut Sheet, styles: &Bound<'_, PyAny>) -> PyResult<()> {
    if let Ok(dict) = styles.cast::<PyDict>() {
        for (key, val) in dict.iter() {
            let (row0, col0) = extract_row_col(&key)?;
            let desc = parse_style_desc(&val)?;
            set_cell_style(sheet, row0 + 1, col0 + 1, desc);
        }
        return Ok(());
    }
    if let Ok(list) = styles.cast::<PyList>() {
        for item in list.iter() {
            let d = item
                .cast::<PyDict>()
                .map_err(|_| PyValueError::new_err("cell_styles list items must be dicts"))?;
            let row0: u32 = d
                .get_item("row")?
                .ok_or_else(|| PyValueError::new_err("style needs row"))?
                .extract()?;
            let col0: u32 = d
                .get_item("col")?
                .ok_or_else(|| PyValueError::new_err("style needs col"))?
                .extract()?;
            let style_obj = d
                .get_item("style")?
                .ok_or_else(|| PyValueError::new_err("style needs style dict"))?;
            let desc = parse_style_desc(&style_obj)?;
            let r = if row0 == 0 { 1 } else { row0 };
            let c = if col0 == 0 { 1 } else { col0 };
            set_cell_style(sheet, r, c, desc);
        }
        return Ok(());
    }
    Err(PyValueError::new_err(
        "cell_styles must be dict{(r,c):style} or list of {row,col,style}",
    ))
}

fn set_cell_style(sheet: &mut Sheet, row: u32, col: u32, desc: StyleDesc) {
    debug_assert!(
        sheet.rows.windows(2).all(|w| w[0].row < w[1].row),
        "sheet.rows invariant broken: rows must be strictly sorted"
    );
    match sheet.rows.binary_search_by(|r| r.row.cmp(&row)) {
        Ok(r_idx) => {
            let r = &mut sheet.rows[r_idx];
            debug_assert!(
                r.cells.windows(2).all(|w| w[0].col < w[1].col),
                "row.cells invariant broken: cells must be strictly sorted"
            );
            match r.cells.binary_search_by(|c| c.col.cmp(&col)) {
                Ok(c_idx) => r.cells[c_idx].style_desc = Some(Box::new(desc)),
                Err(c_idx) => {
                    let mut cell = Cell::new(col, CellValue::Empty);
                    cell.style_desc = Some(Box::new(desc));
                    r.cells.insert(c_idx, cell);
                }
            }
        }
        Err(r_idx) => {
            let mut r = Row::new(row);
            let mut cell = Cell::new(col, CellValue::Empty);
            cell.style_desc = Some(Box::new(desc));
            r.cells.push(cell);
            sheet.rows.insert(r_idx, r);
        }
    }
}

/// Cycle a style palette across existing cells (row-major by row.cells order).
fn apply_style_palette(sheet: &mut Sheet, palette: &[StyleDesc]) {
    if palette.is_empty() {
        return;
    }
    let mut i = 0usize;
    for row in &mut sheet.rows {
        for cell in &mut row.cells {
            cell.style_desc = Some(Box::new(palette[i % palette.len()].clone()));
            i += 1;
        }
    }
}

/// Convert a Python scalar to CellValue.
/// When `style_flag` is Some, set it if the value needs StyleEngine (dates/rich).
fn py_to_cell_value(obj: &Bound<'_, PyAny>, date1904: bool) -> PyResult<CellValue> {
    py_to_cell_value_flagged(obj, date1904, None)
}

fn extract_wrapper_style(obj: &Bound<'_, PyAny>) -> PyResult<Option<Box<StyleDesc>>> {
    if let Ok(d) = obj.cast::<PyDict>() {
        if d.get_item("value")?.is_some() {
            if let Some(s) = d.get_item("style")? {
                if !s.is_none() {
                    return Ok(Some(Box::new(parse_style_desc(&s)?)));
                }
            }
        }
    }
    Ok(None)
}

fn py_to_cell_value_flagged(
    obj: &Bound<'_, PyAny>,
    date1904: bool,
    mut style_flag: Option<&mut bool>,
) -> PyResult<CellValue> {
    let _ = date1904; // serial epoch reserved for W2 display; values use Windows 1900
    if obj.is_none() {
        return Ok(CellValue::Empty);
    }
    // Hot path first: numbers/bools/strings before dict (1M numeric must stay cheap)
    // bool before int (bool is subclass of int in Python)
    if obj.is_instance_of::<pyo3::types::PyBool>() {
        if let Ok(b) = obj.extract::<bool>() {
            return Ok(CellValue::Bool(b));
        }
    }
    if let Ok(i) = obj.extract::<i64>() {
        return Ok(CellValue::Number(i as f64));
    }
    if let Ok(f) = obj.extract::<f64>() {
        return Ok(CellValue::Number(f));
    }
    if let Ok(mut s) = obj.extract::<String>() {
        if s.starts_with('#') && is_excel_error(&s) {
            return Ok(CellValue::Error(s));
        }
        // Byte length is an upper bound on char count, so the common case is one
        // integer compare; only pay for the UTF-8 scan when it could actually trip.
        if s.len() > 32767 {
            let n = s.chars().count();
            if n > 32767 {
                pyo3::PyErr::warn(
                    obj.py(),
                    &obj.py().get_type::<pyo3::exceptions::PyUserWarning>(),
                    std::ffi::CString::new(format!(
                        "cell string is {n} characters; Excel's limit is 32767, so it was truncated. Shorten the value or split it across cells to silence this."
                    ))
                    .map_err(|e| PyValueError::new_err(e.to_string()))?
                    .as_c_str(),
                    0,
                )?;
                if let Some((idx, _)) = s.char_indices().nth(32767) {
                    s.truncate(idx);
                }
            }
        }
        return Ok(CellValue::Str(s));
    }
    // rich text / styled wrappers (dict) — rare relative to scalars
    if let Ok(d) = obj.cast::<PyDict>() {
        if d.get_item("rich")?.is_some() || d.get_item("runs")?.is_some() {
            if let Some(f) = style_flag.as_deref_mut() {
                *f = true;
            }
            return Ok(CellValue::Rich(parse_rich_text(obj)?));
        }
        // styled cell value: {"value": ..., "style": {...}}
        if let Some(v) = d.get_item("value")? {
            return py_to_cell_value_flagged(&v, date1904, style_flag);
        }
    }
    // datetime.date / datetime.datetime via attributes
    if let Ok(year) = obj.getattr("year") {
        if let (Ok(y), Ok(m), Ok(d)) = (
            year.extract::<i32>(),
            obj.getattr("month").and_then(|x| x.extract::<u32>()),
            obj.getattr("day").and_then(|x| x.extract::<u32>()),
        ) {
            // datetime has hour
            if let Ok(hour) = obj.getattr("hour").and_then(|x| x.extract::<u32>()) {
                let minute = obj
                    .getattr("minute")
                    .and_then(|x| x.extract::<u32>())
                    .unwrap_or(0);
                let second = obj
                    .getattr("second")
                    .and_then(|x| x.extract::<u32>())
                    .unwrap_or(0);
                let micros = obj
                    .getattr("microsecond")
                    .and_then(|x| x.extract::<u32>())
                    .unwrap_or(0);
                if let Some(f) = style_flag.as_deref_mut() {
                    *f = true;
                }
                return Ok(CellValue::DateSerial(datetime_to_serial(
                    y, m, d, hour, minute, second, micros,
                )));
            }
            if let Some(f) = style_flag {
                *f = true;
            }
            return Ok(CellValue::DateSerial(date_to_serial(y, m, d)));
        }
    }
    // datetime.time via attributes hour, minute, second (without year)
    if obj.getattr("year").is_err() {
        if let (Ok(hour), Ok(minute), Ok(second)) = (
            obj.getattr("hour").and_then(|x| x.extract::<u32>()),
            obj.getattr("minute").and_then(|x| x.extract::<u32>()),
            obj.getattr("second").and_then(|x| x.extract::<u32>()),
        ) {
            let micros = obj
                .getattr("microsecond")
                .and_then(|x| x.extract::<u32>())
                .unwrap_or(0);
            if let Some(f) = style_flag.as_deref_mut() {
                *f = true;
            }
            let serial = (hour as f64 * 3600.0
                + minute as f64 * 60.0
                + second as f64
                + micros as f64 / 1_000_000.0)
                / 86400.0;
            return Ok(CellValue::Time(serial));
        }
    }
    // datetime.timedelta via days, seconds (without year)
    if obj.getattr("year").is_err() {
        if let (Ok(days), Ok(seconds)) = (
            obj.getattr("days").and_then(|x| x.extract::<i64>()),
            obj.getattr("seconds").and_then(|x| x.extract::<i64>()),
        ) {
            let micros = obj
                .getattr("microseconds")
                .and_then(|x| x.extract::<i64>())
                .unwrap_or(0);
            if let Some(f) = style_flag.as_deref_mut() {
                *f = true;
            }
            let serial = days as f64 + (seconds as f64 + micros as f64 / 1_000_000.0) / 86400.0;
            return Ok(CellValue::Duration(serial));
        }
    }
    Err(pyo3::exceptions::PyTypeError::new_err(format!(
        "unsupported cell value type: {}",
        obj.get_type().name()?
    )))
}

fn is_excel_error(s: &str) -> bool {
    matches!(
        s,
        "#DIV/0!"
            | "#N/A"
            | "#NAME?"
            | "#NULL!"
            | "#NUM!"
            | "#REF!"
            | "#VALUE!"
            | "#GETTING_DATA"
            | "#SPILL!"
            | "#CALC!"
    )
}

/// Parse optional formulas map: {(row, col): formula_str | {text, cached, kind, ref}}
/// row/col are 0-based (Python API).
fn apply_formulas(
    sheet: &mut Sheet,
    formulas: Option<&Bound<'_, PyAny>>,
    emit_cached: bool,
) -> PyResult<()> {
    let Some(formulas) = formulas else {
        return Ok(());
    };
    // Accept dict with tuple keys or list of dicts
    if let Ok(dict) = formulas.cast::<PyDict>() {
        for (key, val) in dict.iter() {
            let (row0, col0) = extract_row_col(&key)?;
            let (text, kind, cached) = parse_formula_val(&val, emit_cached)?;
            set_cell(
                sheet,
                row0 + 1,
                col0 + 1,
                CellValue::Formula { text, kind, cached },
            );
        }
        return Ok(());
    }
    if let Ok(list) = formulas.cast::<PyList>() {
        for item in list.iter() {
            let d = item.cast::<PyDict>().map_err(|_| {
                PyValueError::new_err("formulas list items must be dicts with row, col, text")
            })?;
            let row0: u32 = d
                .get_item("row")?
                .ok_or_else(|| PyValueError::new_err("formula missing row"))?
                .extract()?;
            let col0: u32 = d
                .get_item("col")?
                .ok_or_else(|| PyValueError::new_err("formula missing col"))?
                .extract()?;
            let text: String = if let Some(t) = d.get_item("text")? {
                t.extract()?
            } else if let Some(t) = d.get_item("formula")? {
                t.extract()?
            } else {
                return Err(PyValueError::new_err("formula missing text"));
            };
            let cached = if emit_cached {
                d.get_item("cached")?
                    .map(|c| py_to_cached(&c))
                    .transpose()?
            } else {
                None
            };
            let kind = parse_formula_kind_from_dict(d)?;
            set_cell(
                sheet,
                row0 + 1,
                col0 + 1,
                CellValue::Formula { text, kind, cached },
            );
        }
        return Ok(());
    }
    Err(PyValueError::new_err(
        "formulas must be a dict{(row,col): ...} or list of dicts",
    ))
}

fn extract_row_col(key: &Bound<'_, PyAny>) -> PyResult<(u32, u32)> {
    if let Ok(t) = key.cast::<PyTuple>() {
        if t.len() == 2 {
            let r: u32 = t.get_item(0)?.extract()?;
            let c: u32 = t.get_item(1)?.extract()?;
            return Ok((r, c));
        }
    }
    if let Ok(list) = key.extract::<Vec<u32>>() {
        if list.len() == 2 {
            return Ok((list[0], list[1]));
        }
    }
    Err(PyValueError::new_err(
        "formula key must be (row, col) 0-based",
    ))
}

fn parse_formula_val(
    val: &Bound<'_, PyAny>,
    emit_cached: bool,
) -> PyResult<(String, FormulaKind, Option<CachedValue>)> {
    if let Ok(s) = val.extract::<String>() {
        return Ok((s, FormulaKind::Normal, None));
    }
    if let Ok(d) = val.cast::<PyDict>() {
        let text: String = if let Some(t) = d.get_item("text")? {
            t.extract()?
        } else if let Some(t) = d.get_item("formula")? {
            t.extract()?
        } else {
            return Err(PyValueError::new_err("formula dict needs text"));
        };
        let cached = if emit_cached {
            d.get_item("cached")?
                .map(|c| py_to_cached(&c))
                .transpose()?
        } else {
            None
        };
        let kind = parse_formula_kind_from_dict(d)?;
        return Ok((text, kind, cached));
    }
    Err(PyValueError::new_err(
        "formula value must be str or dict{text, cached?, kind?, ref?}",
    ))
}

fn parse_formula_kind_from_dict(d: &Bound<'_, PyDict>) -> PyResult<FormulaKind> {
    let kind_s: String = d
        .get_item("kind")?
        .map(|k| k.extract())
        .transpose()?
        .unwrap_or_else(|| "normal".into());
    match kind_s.as_str() {
        "normal" => Ok(FormulaKind::Normal),
        "array" => {
            let ref_ = d
                .get_item("ref")?
                .map(|r| r.extract::<String>())
                .transpose()?
                .unwrap_or_else(|| "A1".into());
            Ok(FormulaKind::Array { ref_ })
        }
        "dataTable" | "data_table" => {
            let ref_ = d
                .get_item("ref")?
                .map(|r| r.extract::<String>())
                .transpose()?
                .unwrap_or_else(|| "A1".into());
            Ok(FormulaKind::DataTable {
                ref_,
                dt2d: d
                    .get_item("dt2d")?
                    .map(|x| x.extract())
                    .transpose()?
                    .unwrap_or(false),
                dtr: d
                    .get_item("dtr")?
                    .map(|x| x.extract())
                    .transpose()?
                    .unwrap_or(false),
                r1: d.get_item("r1")?.map(|x| x.extract()).transpose()?,
                r2: d.get_item("r2")?.map(|x| x.extract()).transpose()?,
                del1: d
                    .get_item("del1")?
                    .map(|x| x.extract())
                    .transpose()?
                    .unwrap_or(false),
                del2: d
                    .get_item("del2")?
                    .map(|x| x.extract())
                    .transpose()?
                    .unwrap_or(false),
                ca: d
                    .get_item("ca")?
                    .map(|x| x.extract())
                    .transpose()?
                    .unwrap_or(false),
            })
        }
        other => Err(PyValueError::new_err(format!(
            "unknown formula kind {other:?}"
        ))),
    }
}

fn py_to_cached(obj: &Bound<'_, PyAny>) -> PyResult<CachedValue> {
    if obj.is_none() {
        return Ok(CachedValue::Number(0.0));
    }
    if obj.is_instance_of::<pyo3::types::PyBool>() {
        return Ok(CachedValue::Bool(obj.extract()?));
    }
    if let Ok(i) = obj.extract::<i64>() {
        return Ok(CachedValue::Number(i as f64));
    }
    if let Ok(f) = obj.extract::<f64>() {
        return Ok(CachedValue::Number(f));
    }
    if let Ok(s) = obj.extract::<String>() {
        if is_excel_error(&s) {
            return Ok(CachedValue::Error(s));
        }
        return Ok(CachedValue::Str(s));
    }
    Err(PyValueError::new_err("unsupported cached formula value"))
}

fn set_cell(sheet: &mut Sheet, row: u32, col: u32, value: CellValue) {
    debug_assert!(
        sheet.rows.windows(2).all(|w| w[0].row < w[1].row),
        "sheet.rows invariant broken: rows must be strictly sorted"
    );
    match sheet.rows.binary_search_by(|r| r.row.cmp(&row)) {
        Ok(r_idx) => {
            let r = &mut sheet.rows[r_idx];
            debug_assert!(
                r.cells.windows(2).all(|w| w[0].col < w[1].col),
                "row.cells invariant broken: cells must be strictly sorted"
            );
            match r.cells.binary_search_by(|c| c.col.cmp(&col)) {
                Ok(c_idx) => r.cells[c_idx].value = value,
                Err(c_idx) => r.cells.insert(c_idx, Cell::new(col, value)),
            }
        }
        Err(r_idx) => {
            let mut r = Row::new(row);
            r.cells.push(Cell::new(col, value));
            sheet.rows.insert(r_idx, r);
        }
    }
}

/// Build Sheet rows from columnar data (list of columns, each a sequence).
/// Returns whether any cell needs StyleEngine (dates/rich).
fn columns_to_sheet(
    sheet: &mut Sheet,
    columns: &Bound<'_, PyAny>,
    date1904: bool,
) -> PyResult<bool> {
    // Try pyarrow Table / RecordBatch
    if columns.hasattr("num_columns")? && columns.hasattr("column")? {
        return arrow_like_to_sheet(sheet, columns, date1904);
    }
    // list of columns
    let cols = columns.cast::<PyList>().map_err(|_| {
        PyValueError::new_err(
            "columns must be a list of column sequences, or a pyarrow Table/RecordBatch",
        )
    })?;
    let ncols = cols.len();
    if ncols == 0 {
        return Ok(false);
    }
    let col_data: Vec<Bound<'_, PyAny>> = cols.iter().collect();
    for (i, col) in col_data.iter().enumerate() {
        if col.is_instance_of::<PyString>() {
            return Err(PyValueError::new_err(format!(
                "columns[{i}] is a str; 'columns' takes columnar DATA (a list of column arrays), not header names. A str would be consumed character-by-character. Pass headers as the first entry of 'rows', or wrap this column in a list."
            )));
        }
        if col.is_instance_of::<PyBytes>() {
            return Err(PyValueError::new_err(format!(
                "columns[{i}] is a bytes; 'columns' takes columnar DATA (a list of column arrays), not header names. A str would be consumed character-by-character. Pass headers as the first entry of 'rows', or wrap this column in a list."
            )));
        }
    }
    let nrows = col_data[0].len()?;
    for c in &col_data {
        if c.len()? != nrows {
            return Err(PyValueError::new_err(
                "all columns must have the same length",
            ));
        }
    }
    if ncols > 16384 {
        return Err(PyValueError::new_err(format!(
            "column count {ncols} exceeds Excel limit of 16384"
        )));
    }
    if nrows > 1048576 {
        return Err(PyValueError::new_err(format!(
            "row count {nrows} exceeds Excel limit of 1048576"
        )));
    }
    let mut style_work = false;
    sheet.rows.reserve(nrows);
    for r in 0..nrows {
        let mut row = Row::new((r as u32) + 1);
        row.cells.reserve(ncols);
        for (ci, col) in col_data.iter().enumerate() {
            let cell_obj = col.get_item(r)?;
            let val = py_to_cell_value_flagged(&cell_obj, date1904, Some(&mut style_work))?;
            let wrap_style = extract_wrapper_style(&cell_obj)?;
            if !matches!(val, CellValue::Empty) || wrap_style.is_some() {
                let mut cell = Cell::new((ci as u32) + 1, val);
                if wrap_style.is_some() {
                    style_work = true;
                    cell.style_desc = wrap_style;
                }
                row.cells.push(cell);
            }
        }
        if !row.cells.is_empty() {
            sheet.rows.push(row);
        }
    }
    Ok(style_work)
}

fn arrow_array_to_cell_value(arr: &dyn arrow_array::Array, idx: usize) -> CellValue {
    if arr.is_null(idx) {
        return CellValue::Empty;
    }
    if let Some(f64_arr) = arr.as_any().downcast_ref::<arrow_array::Float64Array>() {
        CellValue::Number(f64_arr.value(idx))
    } else if let Some(f32_arr) = arr.as_any().downcast_ref::<arrow_array::Float32Array>() {
        CellValue::Number(f32_arr.value(idx) as f64)
    } else if let Some(i64_arr) = arr.as_any().downcast_ref::<arrow_array::Int64Array>() {
        CellValue::Number(i64_arr.value(idx) as f64)
    } else if let Some(i32_arr) = arr.as_any().downcast_ref::<arrow_array::Int32Array>() {
        CellValue::Number(i32_arr.value(idx) as f64)
    } else if let Some(b_arr) = arr.as_any().downcast_ref::<arrow_array::BooleanArray>() {
        CellValue::Bool(b_arr.value(idx))
    } else if let Some(str_arr) = arr.as_any().downcast_ref::<arrow_array::StringArray>() {
        CellValue::Str(str_arr.value(idx).to_string())
    } else if let Some(lstr_arr) = arr.as_any().downcast_ref::<arrow_array::LargeStringArray>() {
        CellValue::Str(lstr_arr.value(idx).to_string())
    } else {
        CellValue::Empty
    }
}

fn arrow_like_to_sheet(
    sheet: &mut Sheet,
    table: &Bound<'_, PyAny>,
    date1904: bool,
) -> PyResult<bool> {
    let ncols: usize = table.getattr("num_columns")?.extract()?;
    let nrows: usize = table.getattr("num_rows")?.extract()?;
    if ncols == 0 || nrows == 0 {
        return Ok(false);
    }
    let mut arrow_cols: Vec<arrow_array::ArrayRef> = Vec::with_capacity(ncols);
    let mut use_py_pylist = false;

    for c in 0..ncols {
        let col = table.call_method1("column", (c,))?;
        if let Ok(py_arr) = col.extract::<pyo3_arrow::PyArray>() {
            arrow_cols.push(py_arr.into_inner().0);
        } else {
            use_py_pylist = true;
            break;
        }
    }

    if !use_py_pylist && arrow_cols.len() == ncols {
        sheet.rows.reserve(nrows);
        for r in 0..nrows {
            let mut row = Row::new((r as u32) + 1);
            for (ci, arr) in arrow_cols.iter().enumerate() {
                let val = arrow_array_to_cell_value(arr.as_ref(), r);
                if !matches!(val, CellValue::Empty) {
                    row.cells.push(Cell::new((ci as u32) + 1, val));
                }
            }
            if !row.cells.is_empty() {
                sheet.rows.push(row);
            }
        }
        return Ok(false);
    }

    // Fallback: to_pylist per column for generic Python/Arrow array objects
    let mut col_lists: Vec<Bound<'_, PyList>> = Vec::with_capacity(ncols);
    for c in 0..ncols {
        let col = table.call_method1("column", (c,))?;
        let pylist = if col.hasattr("to_pylist")? {
            col.call_method0("to_pylist")?
        } else if col.hasattr("combine_chunks")? {
            let combined = col.call_method0("combine_chunks")?;
            combined.call_method0("to_pylist")?
        } else {
            return Err(PyValueError::new_err(
                "arrow column missing to_pylist; pass plain Python lists instead",
            ));
        };
        col_lists.push(
            pylist
                .cast::<PyList>()
                .map_err(|_| PyValueError::new_err("to_pylist did not return a list"))?
                .clone(),
        );
    }
    let mut style_work = false;
    sheet.rows.reserve(nrows);
    for r in 0..nrows {
        let mut row = Row::new((r as u32) + 1);
        for (ci, col) in col_lists.iter().enumerate() {
            let cell_obj = col.get_item(r)?;
            let val = py_to_cell_value_flagged(&cell_obj, date1904, Some(&mut style_work))?;
            let wrap_style = extract_wrapper_style(&cell_obj)?;
            if !matches!(val, CellValue::Empty) || wrap_style.is_some() {
                let mut cell = Cell::new((ci as u32) + 1, val);
                if wrap_style.is_some() {
                    style_work = true;
                    cell.style_desc = wrap_style;
                }
                row.cells.push(cell);
            }
        }
        if !row.cells.is_empty() {
            sheet.rows.push(row);
        }
    }
    Ok(style_work)
}

fn apply_row_dims(sheet: &mut Sheet, row_dims: Option<&Bound<'_, PyAny>>) -> PyResult<()> {
    let Some(row_dims) = row_dims else {
        return Ok(());
    };
    let list = row_dims.cast::<PyList>().map_err(|_| {
        PyValueError::new_err("row_dims must be a list of dicts {row, height?, hidden?}")
    })?;
    for item in list.iter() {
        let d = item
            .cast::<PyDict>()
            .map_err(|_| PyValueError::new_err("row_dims items must be dicts"))?;
        // 0-based or 1-based? Use 1-based `row` key; accept `row_idx` 0-based
        let row_num: u32 = if let Some(r) = d.get_item("row")? {
            r.extract()?
        } else if let Some(r) = d.get_item("row_idx")? {
            r.extract::<u32>()? + 1
        } else {
            return Err(PyValueError::new_err("row_dim needs row or row_idx"));
        };
        let height: Option<f64> = d.get_item("height")?.map(|h| h.extract()).transpose()?;
        let hidden: bool = d
            .get_item("hidden")?
            .map(|h| h.extract())
            .transpose()?
            .unwrap_or(false);
        let style_desc = if let Some(st) = d.get_item("style")? {
            if !st.is_none() {
                Some(parse_style_desc(&st)?)
            } else {
                None
            }
        } else {
            None
        };
        if let Some(r) = sheet.rows.iter_mut().find(|r| r.row == row_num) {
            r.height = height;
            r.hidden = hidden;
            r.custom_height = height.is_some();
        } else {
            let mut r = Row::new(row_num);
            r.height = height;
            r.hidden = hidden;
            r.custom_height = height.is_some();
            sheet.rows.push(r);
        }
        if let Some(desc) = style_desc {
            sheet.row_style_descs.push((row_num, desc));
        }
    }
    Ok(())
}

fn apply_col_dims(sheet: &mut Sheet, col_dims: Option<&Bound<'_, PyAny>>) -> PyResult<()> {
    let Some(col_dims) = col_dims else {
        return Ok(());
    };
    let list = col_dims.cast::<PyList>().map_err(|_| {
        PyValueError::new_err("col_dims must be a list of dicts {min, max, width?, hidden?}")
    })?;
    for item in list.iter() {
        let d = item
            .cast::<PyDict>()
            .map_err(|_| PyValueError::new_err("col_dims items must be dicts"))?;
        let min: u32 = d
            .get_item("min")?
            .or(d.get_item("col")?)
            .ok_or_else(|| PyValueError::new_err("col_dim needs min or col"))?
            .extract()?;
        let max: u32 = d
            .get_item("max")?
            .map(|m| m.extract())
            .transpose()?
            .unwrap_or(min);
        let width: Option<f64> = d.get_item("width")?.map(|w| w.extract()).transpose()?;
        let hidden: bool = d
            .get_item("hidden")?
            .map(|h| h.extract())
            .transpose()?
            .unwrap_or(false);
        let style_desc = if let Some(st) = d.get_item("style")? {
            if !st.is_none() {
                Some(parse_style_desc(&st)?)
            } else {
                None
            }
        } else {
            None
        };
        let col_idx = sheet.cols.len();
        sheet.cols.push(ColDim {
            min,
            max,
            width,
            hidden,
            style: None,
            best_fit: false,
            custom_width: width.is_some(),
            outline_level: 0,
        });
        if let Some(desc) = style_desc {
            sheet.col_style_descs.push((col_idx, desc));
        }
    }
    Ok(())
}

fn parse_sheet_dict(sheet_obj: &Bound<'_, PyAny>, opts: &WriteOptions) -> PyResult<Sheet> {
    // Accept dict or object with attributes
    let d = if let Ok(dict) = sheet_obj.cast::<PyDict>() {
        dict.clone()
    } else {
        // build from attributes into a temp approach: use getattr
        let name: String = sheet_obj
            .getattr("name")
            .and_then(|n| n.extract())
            .unwrap_or_else(|_| "Sheet".into());
        let mut sheet = Sheet::new(name);
        if let Ok(vis) = sheet_obj
            .getattr("visibility")
            .or_else(|_| sheet_obj.getattr("state"))
        {
            if let Ok(s) = vis.extract::<String>() {
                if let Some(st) = SheetState::parse(&s) {
                    sheet.state = st;
                }
            }
        }
        if let Ok(cols) = sheet_obj
            .getattr("columns")
            .or_else(|_| sheet_obj.getattr("data"))
        {
            if !cols.is_none() {
                columns_to_sheet(&mut sheet, &cols, opts.date1904)?;
            }
        }
        if let Ok(f) = sheet_obj.getattr("formulas") {
            if !f.is_none() {
                apply_formulas(&mut sheet, Some(&f), opts.emit_cached_values)?;
            }
        }
        return Ok(sheet);
    };

    let name: String = d
        .get_item("name")?
        .map(|n| n.extract())
        .transpose()?
        .unwrap_or_else(|| "Sheet".into());
    let mut sheet = Sheet::new(name);

    if let Some(vis) = d.get_item("visibility")?.or(d.get_item("state")?) {
        let s: String = vis.extract()?;
        sheet.state = SheetState::parse(&s).ok_or_else(|| {
            PyValueError::new_err(format!(
                "visibility must be visible|hidden|veryHidden; got {s:?}"
            ))
        })?;
    }

    let mut style_work = false;
    if let Some(cols) = d
        .get_item("columns")?
        .or(d.get_item("data")?)
        .or(d.get_item("table")?)
    {
        if !cols.is_none() && columns_to_sheet(&mut sheet, &cols, opts.date1904)? {
            style_work = true;
        }
    }

    // rows as list or iterator of row sequences (including rows_iter)
    if let Some(rows) = d.get_item("rows")?.or(d.get_item("rows_iter")?) {
        if !rows.is_none() {
            let mut ri: u32 = 1;
            let iter = rows.try_iter().map_err(|_| {
                PyValueError::new_err("rows / rows_iter must be an iterable of row sequences")
            })?;
            for row_obj in iter {
                let row_item = row_obj?;
                if ri > 1048576 {
                    return Err(PyValueError::new_err(
                        "row count exceeds Excel limit of 1048576",
                    ));
                }
                let mut row = Row::new(ri);
                let cell_iter = row_item.try_iter().map_err(|_| {
                    PyValueError::new_err("each row must be an iterable of cell values")
                })?;
                for (ci, cell_res) in cell_iter.enumerate() {
                    let cell = cell_res?;
                    if ci >= 16384 {
                        return Err(PyValueError::new_err(format!(
                            "row {ri} exceeds Excel's column limit of 16384"
                        )));
                    }
                    let val =
                        py_to_cell_value_flagged(&cell, opts.date1904, Some(&mut style_work))?;
                    let wrap_style = extract_wrapper_style(&cell)?;
                    if !matches!(val, CellValue::Empty) || wrap_style.is_some() {
                        let mut cell_rec = Cell::new((ci as u32) + 1, val);
                        if wrap_style.is_some() {
                            style_work = true;
                            cell_rec.style_desc = wrap_style;
                        }
                        row.cells.push(cell_rec);
                    }
                }
                if !row.cells.is_empty() {
                    sheet.rows.push(row);
                }
                ri += 1;
            }
        }
    }

    apply_formulas(
        &mut sheet,
        d.get_item("formulas")?.as_ref(),
        opts.emit_cached_values,
    )?;
    apply_row_dims(&mut sheet, d.get_item("row_dims")?.as_ref())?;
    apply_col_dims(&mut sheet, d.get_item("col_dims")?.as_ref())?;

    if let Some(fc) = d.get_item("freeze_panes")?.or(d.get_item("freeze_cell")?) {
        if !fc.is_none() {
            sheet.view.freeze_cell = Some(fc.extract()?);
        }
    }

    // W2 styles
    if let Some(cs) = d.get_item("cell_styles")?.or(d.get_item("styles")?) {
        if !cs.is_none() {
            apply_cell_styles(&mut sheet, &cs)?;
            style_work = true;
        }
    }
    if let Some(pal) = d.get_item("style_palette")? {
        if !pal.is_none() {
            let list = pal.cast::<PyList>().map_err(|_| {
                PyValueError::new_err("style_palette must be a list of style dicts")
            })?;
            let mut palette = Vec::with_capacity(list.len());
            for item in list.iter() {
                palette.push(parse_style_desc(&item)?);
            }
            apply_style_palette(&mut sheet, &palette);
            style_work = true;
        }
    }
    if let Some(cf) = d.get_item("conditional_formatting")?.or(d.get_item("cf")?) {
        if !cf.is_none() {
            sheet.conditional_formatting = parse_conditional_formatting(&cf)?;
            style_work = true;
        }
    }
    if let Some(dv) = d
        .get_item("data_validations")?
        .or(d.get_item("validations")?)
        .or(d.get_item("dv")?)
    {
        if !dv.is_none() {
            sheet.data_validations = parse_data_validations(&dv)?;
            style_work = true;
        }
    }
    if !sheet.row_style_descs.is_empty() || !sheet.col_style_descs.is_empty() {
        style_work = true;
    }
    sheet.needs_style_work = style_work;

    // W3 structural fields
    apply_structural_sheet(&mut sheet, &d)?;

    // sort rows by row index for stable XML
    sheet.rows.sort_by_key(|r| r.row);

    Ok(sheet)
}

fn parse_auto_filter(obj: &Bound<'_, PyAny>) -> PyResult<AutoFilterMeta> {
    // Accept a plain ref string (back-compat) or a dict like the read surface emits:
    //   {"ref": "A1:C10", "columns": [{"col_id": 0, "hidden_button": false,
    //                                  "show_button": true, "values": [...], "blank": false}]}
    if let Ok(ref_) = obj.extract::<String>() {
        return Ok(AutoFilterMeta {
            ref_: parse_range(ref_.as_bytes()),
            columns: Vec::new(),
        });
    }
    let d = obj
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("auto_filter must be a ref string or a dict"))?;
    let ref_ = d
        .get_item("ref")?
        .ok_or_else(|| PyValueError::new_err("auto_filter dict needs a 'ref'"))?
        .extract::<String>()?;
    let mut columns = Vec::new();
    if let Some(cols) = d.get_item("columns")? {
        if !cols.is_none() {
            let list = cols.cast::<PyList>().map_err(|_| {
                PyValueError::new_err("auto_filter 'columns' must be a list of dicts")
            })?;
            for item in list.iter() {
                columns.push(parse_filter_column(&item)?);
            }
        }
    }
    Ok(AutoFilterMeta {
        ref_: parse_range(ref_.as_bytes()),
        columns,
    })
}

fn parse_filter_column(obj: &Bound<'_, PyAny>) -> PyResult<FilterColumnMeta> {
    let d = obj
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("each auto_filter column must be a dict"))?;
    let col_id: u32 = d
        .get_item("col_id")?
        .or(d.get_item("colId")?)
        .ok_or_else(|| PyValueError::new_err("auto_filter column needs 'col_id'"))?
        .extract()?;
    let hidden_button: bool = d
        .get_item("hidden_button")?
        .or(d.get_item("hiddenButton")?)
        .filter(|v| !v.is_none())
        .map(|v| v.extract::<bool>())
        .transpose()?
        .unwrap_or(false);
    let show_button: bool = d
        .get_item("show_button")?
        .or(d.get_item("showButton")?)
        .filter(|v| !v.is_none())
        .map(|v| v.extract::<bool>())
        .transpose()?
        .unwrap_or(true);
    let mut values: Vec<String> = Vec::new();
    if let Some(vs) = d.get_item("values")? {
        if !vs.is_none() {
            values = vs.extract()?;
        }
    }
    let blank: Option<bool> = match d.get_item("blank")? {
        Some(v) if !v.is_none() => Some(v.extract()?),
        _ => None,
    };
    Ok(FilterColumnMeta {
        col_id,
        hidden_button,
        show_button,
        values,
        blank,
    })
}

fn apply_structural_sheet(sheet: &mut Sheet, d: &Bound<'_, PyDict>) -> PyResult<()> {
    if let Some(tc) = d.get_item("tab_color")?.or(d.get_item("tab_color_rgb")?) {
        if !tc.is_none() {
            sheet.tab_color_rgb = Some(tc.extract()?);
        }
    }
    if let Some(af) = d.get_item("auto_filter")? {
        if !af.is_none() {
            sheet.auto_filter = Some(parse_auto_filter(&af)?);
        }
    }
    if let Some(m) = d.get_item("merges")? {
        if !m.is_none() {
            sheet.merges = m.extract()?;
        }
    }
    if let Some(hl) = d.get_item("hyperlinks")? {
        if !hl.is_none() {
            sheet.hyperlinks = parse_hyperlinks(&hl)?;
        }
    }
    if let Some(p) = d
        .get_item("protection")?
        .or(d.get_item("sheet_protection")?)
    {
        if !p.is_none() {
            sheet.protection = Some(parse_sheet_protection(&p)?);
        }
    }
    if let Some(sc) = d.get_item("scenarios")? {
        if !sc.is_none() {
            sheet.scenarios = parse_scenarios(&sc)?;
        }
    }
    if let Some(po) = d.get_item("print_options")? {
        if !po.is_none() {
            sheet.print_options = Some(parse_print_options(&po)?);
        }
    }
    if let Some(pm) = d.get_item("page_margins")? {
        if !pm.is_none() {
            sheet.page_margins = Some(parse_page_margins(&pm)?);
        }
    }
    if let Some(ps) = d.get_item("page_setup")? {
        if !ps.is_none() {
            sheet.page_setup = Some(parse_page_setup(&ps)?);
        }
    }
    if let Some(hf) = d.get_item("header_footer")? {
        if !hf.is_none() {
            sheet.header_footer = Some(parse_header_footer(&hf)?);
        }
    }
    if let Some(rb) = d.get_item("row_breaks")? {
        if !rb.is_none() {
            sheet.row_breaks = rb.extract()?;
        }
    }
    if let Some(cb) = d.get_item("col_breaks")? {
        if !cb.is_none() {
            sheet.col_breaks = cb.extract()?;
        }
    }
    if let Some(t) = d.get_item("tables")? {
        if !t.is_none() {
            sheet.tables = parse_tables(&t)?;
        }
    }
    if let Some(c) = d.get_item("comments")? {
        if !c.is_none() {
            sheet.comments = parse_comments(&c)?;
        }
    }
    if let Some(ch) = d.get_item("charts")? {
        if !ch.is_none() {
            sheet.charts = parse_charts(&ch)?;
        }
    }
    if let Some(im) = d.get_item("images")? {
        if !im.is_none() {
            sheet.images = parse_images(&im)?;
        }
    }
    if let Some(pa) = d.get_item("print_area")? {
        if !pa.is_none() {
            sheet.print_area = Some(pa.extract()?);
        }
    }
    if let Some(pt) = d.get_item("print_titles")? {
        if !pt.is_none() {
            sheet.print_titles = Some(pt.extract()?);
        }
    }
    if let Some(pv) = d.get_item("pivots")? {
        if !pv.is_none() {
            sheet.pivots = parse_pivots(&pv, sheet)?;
        }
    }
    Ok(())
}

fn parse_hyperlinks(obj: &Bound<'_, PyAny>) -> PyResult<Vec<Hyperlink>> {
    let list = obj
        .cast::<PyList>()
        .map_err(|_| PyValueError::new_err("hyperlinks must be a list of dicts"))?;
    let mut out = Vec::with_capacity(list.len());
    for item in list.iter() {
        let d = item
            .cast::<PyDict>()
            .map_err(|_| PyValueError::new_err("each hyperlink must be a dict"))?;
        let ref_: String = d
            .get_item("ref")?
            .or(d.get_item("ref_")?)
            .ok_or_else(|| PyValueError::new_err("hyperlink requires ref"))?
            .extract()?;
        let target = d.get_item("target")?.map(|v| v.extract()).transpose()?;
        let location = d.get_item("location")?.map(|v| v.extract()).transpose()?;
        let display = d.get_item("display")?.map(|v| v.extract()).transpose()?;
        out.push(Hyperlink {
            ref_,
            target,
            location,
            display,
        });
    }
    Ok(out)
}

fn parse_sheet_protection(obj: &Bound<'_, PyAny>) -> PyResult<SheetProtection> {
    if let Ok(b) = obj.extract::<bool>() {
        return Ok(SheetProtection {
            sheet: b,
            password: None,
            already_hashed: false,
        });
    }
    let d = obj
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("protection must be bool or dict"))?;
    let sheet = d
        .get_item("sheet")?
        .map(|v| v.extract::<bool>())
        .transpose()?
        .unwrap_or(true);
    let password = d.get_item("password")?.map(|v| v.extract()).transpose()?;
    let already_hashed = d
        .get_item("already_hashed")?
        .map(|v| v.extract::<bool>())
        .transpose()?
        .unwrap_or(false);
    Ok(SheetProtection {
        sheet,
        password,
        already_hashed,
    })
}

fn parse_scenarios(obj: &Bound<'_, PyAny>) -> PyResult<Vec<Scenario>> {
    let list = obj
        .cast::<PyList>()
        .map_err(|_| PyValueError::new_err("scenarios must be a list"))?;
    let mut out = Vec::new();
    for item in list.iter() {
        let d = item
            .cast::<PyDict>()
            .map_err(|_| PyValueError::new_err("scenario must be a dict"))?;
        let name: String = d
            .get_item("name")?
            .ok_or_else(|| PyValueError::new_err("scenario requires name"))?
            .extract()?;
        let mut cells = Vec::new();
        if let Some(c) = d.get_item("cells")? {
            if let Ok(dict) = c.cast::<PyDict>() {
                for (k, v) in dict.iter() {
                    cells.push((k.extract()?, v.extract()?));
                }
            } else if let Ok(lst) = c.cast::<PyList>() {
                for pair in lst.iter() {
                    let t = pair.cast::<PyTuple>().map_err(|_| {
                        PyValueError::new_err("scenario cells list items must be (ref, val)")
                    })?;
                    cells.push((t.get_item(0)?.extract()?, t.get_item(1)?.extract()?));
                }
            }
        }
        out.push(Scenario { name, cells });
    }
    Ok(out)
}

fn parse_print_options(obj: &Bound<'_, PyAny>) -> PyResult<PrintOptions> {
    let d = obj
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("print_options must be a dict"))?;
    Ok(PrintOptions {
        horizontal_centered: d
            .get_item("horizontal_centered")?
            .map(|v| v.extract())
            .transpose()?
            .unwrap_or(false),
        vertical_centered: d
            .get_item("vertical_centered")?
            .map(|v| v.extract())
            .transpose()?
            .unwrap_or(false),
        headings: d
            .get_item("headings")?
            .map(|v| v.extract())
            .transpose()?
            .unwrap_or(false),
        grid_lines: d
            .get_item("grid_lines")?
            .map(|v| v.extract())
            .transpose()?
            .unwrap_or(false),
    })
}

fn parse_page_margins(obj: &Bound<'_, PyAny>) -> PyResult<PageMargins> {
    let d = obj
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("page_margins must be a dict"))?;
    let mut m = PageMargins::default();
    if let Some(v) = d.get_item("left")? {
        m.left = v.extract()?;
    }
    if let Some(v) = d.get_item("right")? {
        m.right = v.extract()?;
    }
    if let Some(v) = d.get_item("top")? {
        m.top = v.extract()?;
    }
    if let Some(v) = d.get_item("bottom")? {
        m.bottom = v.extract()?;
    }
    if let Some(v) = d.get_item("header")? {
        m.header = v.extract()?;
    }
    if let Some(v) = d.get_item("footer")? {
        m.footer = v.extract()?;
    }
    Ok(m)
}

fn parse_page_setup(obj: &Bound<'_, PyAny>) -> PyResult<PageSetup> {
    let d = obj
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("page_setup must be a dict"))?;
    Ok(PageSetup {
        orientation: d
            .get_item("orientation")?
            .map(|v| v.extract())
            .transpose()?,
        paper_size: d.get_item("paper_size")?.map(|v| v.extract()).transpose()?,
        fit_to_page: d
            .get_item("fit_to_page")?
            .map(|v| v.extract())
            .transpose()?
            .unwrap_or(false),
        fit_to_width: d
            .get_item("fit_to_width")?
            .map(|v| v.extract())
            .transpose()?,
        fit_to_height: d
            .get_item("fit_to_height")?
            .map(|v| v.extract())
            .transpose()?,
        scale: d.get_item("scale")?.map(|v| v.extract()).transpose()?,
    })
}

fn parse_header_footer(obj: &Bound<'_, PyAny>) -> PyResult<HeaderFooter> {
    let d = obj
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("header_footer must be a dict"))?;
    Ok(HeaderFooter {
        odd_header_center: d
            .get_item("odd_header_center")?
            .or(d.get_item("header_center")?)
            .map(|v| v.extract())
            .transpose()?,
        odd_header_left: d
            .get_item("odd_header_left")?
            .or(d.get_item("header_left")?)
            .map(|v| v.extract())
            .transpose()?,
        odd_header_right: d
            .get_item("odd_header_right")?
            .or(d.get_item("header_right")?)
            .map(|v| v.extract())
            .transpose()?,
        odd_footer_center: d
            .get_item("odd_footer_center")?
            .or(d.get_item("footer_center")?)
            .map(|v| v.extract())
            .transpose()?,
        odd_footer_left: d
            .get_item("odd_footer_left")?
            .or(d.get_item("footer_left")?)
            .map(|v| v.extract())
            .transpose()?,
        odd_footer_right: d
            .get_item("odd_footer_right")?
            .or(d.get_item("footer_right")?)
            .map(|v| v.extract())
            .transpose()?,
    })
}

fn parse_pivot_field(obj: &Bound<'_, PyAny>) -> PyResult<PivotField> {
    if let Ok(s) = obj.extract::<String>() {
        return Ok(PivotField::Name(s));
    }
    if let Ok(i) = obj.extract::<u32>() {
        return Ok(PivotField::Index(i));
    }
    Err(PyValueError::new_err(
        "pivot field must be a header name (str) or a 0-based column index (int)",
    ))
}

fn parse_pivot_fields(obj: &Bound<'_, PyAny>) -> PyResult<Vec<PivotField>> {
    let list = obj
        .cast::<PyList>()
        .map_err(|_| PyValueError::new_err("rows/cols must be a list of fields"))?;
    list.iter().map(|f| parse_pivot_field(&f)).collect()
}

fn parse_pivot_data(obj: &Bound<'_, PyAny>) -> PyResult<Vec<PivotDataField>> {
    let list = obj
        .cast::<PyList>()
        .map_err(|_| PyValueError::new_err("data must be a list of {field, agg} dicts"))?;
    let mut out = Vec::with_capacity(list.len());
    for item in list.iter() {
        let d = item
            .cast::<PyDict>()
            .map_err(|_| PyValueError::new_err("each pivot data field must be a dict"))?;
        let field_obj = d
            .get_item("field")?
            .ok_or_else(|| PyValueError::new_err("pivot data field requires 'field'"))?;
        let field = parse_pivot_field(&field_obj)?;
        let agg_s: String = d
            .get_item("agg")?
            .or(d.get_item("subtotal")?)
            .or(d.get_item("aggregation")?)
            .map(|a| a.extract())
            .transpose()?
            .unwrap_or_else(|| "sum".into());
        let agg = PivotAgg::parse(&agg_s).ok_or_else(|| {
            PyValueError::new_err(format!(
                "unknown pivot aggregation {agg_s:?}; expected sum|count|countNums|average|max|min|product|stdDev|stdDevp|var|varp"
            ))
        })?;
        out.push(PivotDataField { field, agg });
    }
    Ok(out)
}

fn parse_pivots(obj: &Bound<'_, PyAny>, sheet: &Sheet) -> PyResult<Vec<PivotTableSpec>> {
    let list = obj
        .cast::<PyList>()
        .map_err(|_| PyValueError::new_err("pivots must be a list of dicts"))?;
    let mut out = Vec::with_capacity(list.len());
    for (i, item) in list.iter().enumerate() {
        let d = item
            .cast::<PyDict>()
            .map_err(|_| PyValueError::new_err("each pivot must be a dict"))?;
        let spec = PivotTableSpec {
            name: opt_str(d, "name")?.unwrap_or_default(),
            source_range: d
                .get_item("source_range")?
                .or(d.get_item("source")?)
                .or(d.get_item("range")?)
                .ok_or_else(|| PyValueError::new_err("pivot requires source_range"))?
                .extract()?,
            rows: d
                .get_item("rows")?
                .map(|v| parse_pivot_fields(&v))
                .transpose()?
                .unwrap_or_default(),
            cols: d
                .get_item("cols")?
                .map(|v| parse_pivot_fields(&v))
                .transpose()?
                .unwrap_or_default(),
            data: d
                .get_item("data")?
                .map(|v| parse_pivot_data(&v))
                .transpose()?
                .unwrap_or_default(),
            target_cell: d
                .get_item("target_cell")?
                .or(d.get_item("target")?)
                .ok_or_else(|| PyValueError::new_err("pivot requires target_cell"))?
                .extract()?,
        };
        spec.validate(sheet)
            .map_err(|e| PyValueError::new_err(format!("pivots[{i}]: {e}")))?;
        out.push(spec);
    }
    Ok(out)
}

fn parse_tables(obj: &Bound<'_, PyAny>) -> PyResult<Vec<TableDef>> {
    let list = obj
        .cast::<PyList>()
        .map_err(|_| PyValueError::new_err("tables must be a list"))?;
    let mut out = Vec::new();
    for item in list.iter() {
        let d = item
            .cast::<PyDict>()
            .map_err(|_| PyValueError::new_err("table must be a dict"))?;
        let display_name: String = d
            .get_item("display_name")?
            .or(d.get_item("name")?)
            .ok_or_else(|| PyValueError::new_err("table requires display_name"))?
            .extract()?;
        let ref_: String = d
            .get_item("ref")?
            .or(d.get_item("ref_")?)
            .ok_or_else(|| PyValueError::new_err("table requires ref"))?
            .extract()?;
        let columns: Vec<String> = d
            .get_item("columns")?
            .map(|v| v.extract())
            .transpose()?
            .unwrap_or_default();
        let style_name = d
            .get_item("style_name")?
            .or(d.get_item("style")?)
            .map(|v| v.extract())
            .transpose()?;
        let show_row_stripes = d
            .get_item("show_row_stripes")?
            .map(|v| v.extract())
            .transpose()?
            .unwrap_or(true);
        out.push(TableDef {
            display_name,
            ref_,
            columns,
            style_name,
            show_row_stripes,
        });
    }
    Ok(out)
}

fn parse_comments(obj: &Bound<'_, PyAny>) -> PyResult<Vec<Comment>> {
    let list = obj
        .cast::<PyList>()
        .map_err(|_| PyValueError::new_err("comments must be a list"))?;
    let mut out = Vec::new();
    for item in list.iter() {
        let d = item
            .cast::<PyDict>()
            .map_err(|_| PyValueError::new_err("comment must be a dict"))?;
        let ref_: String = d
            .get_item("ref")?
            .or(d.get_item("ref_")?)
            .ok_or_else(|| PyValueError::new_err("comment requires ref"))?
            .extract()?;
        let author: String = d
            .get_item("author")?
            .map(|v| v.extract())
            .transpose()?
            .unwrap_or_else(|| "Author".into());
        let text: String = d
            .get_item("text")?
            .ok_or_else(|| PyValueError::new_err("comment requires text"))?
            .extract()?;
        let height = d
            .get_item("height")?
            .map(|v| v.extract())
            .transpose()?
            .unwrap_or(79);
        let width = d
            .get_item("width")?
            .map(|v| v.extract())
            .transpose()?
            .unwrap_or(144);
        out.push(Comment {
            ref_,
            author,
            text,
            height,
            width,
        });
    }
    Ok(out)
}

const VALID_MARKER_SYMBOLS: [&str; 11] = [
    "circle", "dash", "diamond", "dot", "none", "plus", "square", "star", "triangle", "x", "auto",
];

/// Marker as `{symbol, size}` dict, a bare symbol string, or flat keys
/// `marker_symbol`/`marker_size`.
fn parse_marker(d: &Bound<'_, PyDict>) -> PyResult<(Option<String>, Option<u8>)> {
    let mut symbol = None;
    let mut size = None;
    if let Some(m) = d.get_item("marker")? {
        if let Ok(md) = m.cast::<PyDict>() {
            symbol = md.get_item("symbol")?.map(|v| v.extract()).transpose()?;
            size = md.get_item("size")?.map(|v| v.extract()).transpose()?;
        } else if let Ok(s) = m.extract::<String>() {
            symbol = Some(s);
        }
    }
    if symbol.is_none() {
        symbol = d
            .get_item("marker_symbol")?
            .or(d.get_item("symbol")?)
            .map(|v| v.extract())
            .transpose()?;
    }
    if size.is_none() {
        size = d
            .get_item("marker_size")?
            .or(d.get_item("size")?)
            .map(|v| v.extract())
            .transpose()?;
    }
    if let Some(sym) = &symbol {
        if !VALID_MARKER_SYMBOLS.contains(&sym.as_str()) {
            return Err(PyValueError::new_err(format!(
                "unknown marker symbol {sym:?}; expected one of circle, dash, diamond, dot, none, plus, square, star, triangle, x, auto"
            )));
        }
    }
    Ok((symbol, size))
}

/// Series colour: hex str / `{rgb: ...}` under `colour`/`color`, or openpyxl's
/// `graphicalProperties: {solidFill: ...}`.
fn parse_series_colour(d: &Bound<'_, PyDict>) -> PyResult<Option<String>> {
    if let Some(v) = d.get_item("colour")?.or(d.get_item("color")?) {
        if let Ok(s) = v.extract::<String>() {
            return Ok(Some(s));
        }
        if let Ok(cd) = v.cast::<PyDict>() {
            if let Some(rgb) = cd.get_item("rgb")? {
                return Ok(Some(rgb.extract()?));
            }
        }
        return Err(PyValueError::new_err(
            "series color must be hex str or {rgb: ...}",
        ));
    }
    if let Some(gp) = d.get_item("graphicalProperties")? {
        if let Ok(gpd) = gp.cast::<PyDict>() {
            if let Some(sf) = gpd.get_item("solidFill")? {
                if let Ok(s) = sf.extract::<String>() {
                    return Ok(Some(s));
                }
                if let Ok(sfd) = sf.cast::<PyDict>() {
                    if let Some(rgb) = sfd.get_item("rgb")?.or(sfd.get_item("color")?) {
                        return Ok(Some(rgb.extract()?));
                    }
                }
            }
        }
    }
    Ok(None)
}

fn parse_series(obj: &Bound<'_, PyAny>) -> PyResult<Series> {
    let d = obj
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("series must be a dict"))?;
    let (marker_symbol, marker_size) = parse_marker(d)?;
    let smooth = d
        .get_item("smooth")?
        .map(|v| {
            if let Ok(b) = v.extract::<bool>() {
                Ok(b)
            } else if let Ok(i) = v.extract::<i32>() {
                Ok(i != 0)
            } else {
                Err(PyValueError::new_err("smooth must be a bool"))
            }
        })
        .transpose()?;
    Ok(Series {
        title_ref: d.get_item("title_ref")?.map(|v| v.extract()).transpose()?,
        title_literal: d
            .get_item("title_literal")?
            .or(d.get_item("title")?)
            .map(|v| v.extract())
            .transpose()?,
        cat_ref: d
            .get_item("cat_ref")?
            .or(d.get_item("cat")?)
            .map(|v| v.extract())
            .transpose()?,
        val_ref: d
            .get_item("val_ref")?
            .or(d.get_item("val")?)
            .map(|v| v.extract())
            .transpose()?,
        x_ref: d
            .get_item("x_ref")?
            .or(d.get_item("x")?)
            .map(|v| v.extract())
            .transpose()?,
        y_ref: d
            .get_item("y_ref")?
            .or(d.get_item("y")?)
            .map(|v| v.extract())
            .transpose()?,
        bubble_size_ref: d
            .get_item("bubble_size_ref")?
            .or(d.get_item("bubble")?)
            .map(|v| v.extract())
            .transpose()?,
        colour: parse_series_colour(d)?,
        marker_symbol,
        marker_size,
        smooth,
    })
}

/// Optional EMU int from a dict key; default 0.
fn opt_emu(d: &Bound<'_, PyDict>, name: &str) -> PyResult<i64> {
    Ok(d.get_item(name)?
        .filter(|v| !v.is_none())
        .map(|v| v.extract())
        .transpose()?
        .unwrap_or(0))
}

/// Optional EMU offset pair from a dict key; accepts a 2-list `[x, y]` or a
/// `"x,y"` string. Defaults to `(0, 0)`.
fn opt_off_pair(d: &Bound<'_, PyDict>, name: &str) -> PyResult<(i64, i64)> {
    let Some(v) = d.get_item(name)? else {
        return Ok((0, 0));
    };
    if v.is_none() {
        return Ok((0, 0));
    }
    if let Ok(pair) = v.extract::<Vec<i64>>() {
        if pair.len() == 2 {
            return Ok((pair[0], pair[1]));
        }
    }
    if let Ok(s) = v.extract::<String>() {
        let parts: Vec<&str> = s.split(',').collect();
        if parts.len() == 2 {
            if let (Ok(a), Ok(b)) = (parts[0].trim().parse(), parts[1].trim().parse()) {
                return Ok((a, b));
            }
        }
    }
    Ok((0, 0))
}

fn parse_anchor(obj: &Bound<'_, PyAny>) -> PyResult<Anchor> {
    if let Ok(s) = obj.extract::<String>() {
        return Ok(Anchor::OneCell {
            cell: s,
            col_off: 0,
            row_off: 0,
            width_cm: 15.0,
            height_cm: 7.5,
        });
    }
    let d = obj
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("anchor must be str or dict"))?;
    let kind: String = d
        .get_item("type")?
        .map(|v| v.extract())
        .transpose()?
        .unwrap_or_else(|| "oneCell".into());
    match kind.as_str() {
        "twoCell" | "two_cell" => Ok(Anchor::TwoCell {
            from_cell: d
                .get_item("from")?
                .or(d.get_item("from_cell")?)
                .ok_or_else(|| PyValueError::new_err("twoCell needs from"))?
                .extract()?,
            from_off: opt_off_pair(d, "from_off")?,
            to_cell: d
                .get_item("to")?
                .or(d.get_item("to_cell")?)
                .ok_or_else(|| PyValueError::new_err("twoCell needs to"))?
                .extract()?,
            to_off: opt_off_pair(d, "to_off")?,
            edit_as: d.get_item("edit_as")?.map(|v| v.extract()).transpose()?,
        }),
        "absolute" => Ok(Anchor::Absolute {
            x_emu: opt_emu(d, "x")?,
            y_emu: opt_emu(d, "y")?,
            cx_emu: d
                .get_item("cx")?
                .map(|v| v.extract())
                .transpose()?
                .unwrap_or(5_400_000),
            cy_emu: d
                .get_item("cy")?
                .map(|v| v.extract())
                .transpose()?
                .unwrap_or(2_700_000),
        }),
        _ => {
            let cell: String = d
                .get_item("cell")?
                .map(|v| v.extract())
                .transpose()?
                .unwrap_or_else(|| "E15".into());
            let width_cm = d
                .get_item("width_cm")?
                .or(d.get_item("width")?)
                .map(|v| v.extract())
                .transpose()?
                .unwrap_or(15.0);
            let height_cm = d
                .get_item("height_cm")?
                .or(d.get_item("height")?)
                .map(|v| v.extract())
                .transpose()?
                .unwrap_or(7.5);
            Ok(Anchor::OneCell {
                cell,
                col_off: opt_emu(d, "col_off")?,
                row_off: opt_emu(d, "row_off")?,
                width_cm,
                height_cm,
            })
        }
    }
}

fn parse_charts(obj: &Bound<'_, PyAny>) -> PyResult<Vec<Chart>> {
    let list = obj
        .cast::<PyList>()
        .map_err(|_| PyValueError::new_err("charts must be a list"))?;
    let mut out = Vec::new();
    for item in list.iter() {
        let d = item
            .cast::<PyDict>()
            .map_err(|_| PyValueError::new_err("chart must be a dict"))?;
        let type_s: String = d
            .get_item("type")?
            .or(d.get_item("chart_type")?)
            .ok_or_else(|| PyValueError::new_err("chart requires type"))?
            .extract()?;
        let chart_type = ChartType::parse(&type_s)
            .ok_or_else(|| PyValueError::new_err(format!("unknown chart type {type_s:?}")))?;
        let title = d.get_item("title")?.map(|v| v.extract()).transpose()?;
        let mut series = Vec::new();
        if let Some(s) = d.get_item("series")? {
            let sl = s
                .cast::<PyList>()
                .map_err(|_| PyValueError::new_err("chart series must be a list"))?;
            for ser in sl.iter() {
                series.push(parse_series(&ser)?);
            }
        }
        let anchor = if let Some(a) = d.get_item("anchor")? {
            parse_anchor(&a)?
        } else {
            Anchor::default()
        };
        let legend_pos = d.get_item("legend_pos")?.map(|v| v.extract()).transpose()?;
        let style = d.get_item("style")?.map(|v| v.extract()).transpose()?;
        let grouping = if let Some(g) = d.get_item("grouping")? {
            let gs: String = g.extract()?;
            Grouping::parse(&gs)
                .ok_or_else(|| PyValueError::new_err(format!("unknown grouping {gs:?}")))?
        } else {
            Grouping::default()
        };
        out.push(Chart {
            chart_type,
            title,
            series,
            anchor,
            style,
            legend_pos,
            grouping,
        });
    }
    Ok(out)
}

fn parse_images(obj: &Bound<'_, PyAny>) -> PyResult<Vec<Image>> {
    let list = obj
        .cast::<PyList>()
        .map_err(|_| PyValueError::new_err("images must be a list of dicts"))?;
    let mut out = Vec::new();
    for item in list.iter() {
        let d = item
            .cast::<PyDict>()
            .map_err(|_| PyValueError::new_err("each image must be a dict"))?;
        // Accept `data`/`bytes` (raw bytes) or a `path` resolved to bytes here;
        // the format is always detected from magic bytes, never the extension.
        let data_item = d
            .get_item("data")?
            .or(d.get_item("bytes")?)
            .or(d.get_item("path")?);
        let bytes: Vec<u8> = match data_item {
            Some(v) if !v.is_none() => {
                if let Ok(b) = v.extract::<Vec<u8>>() {
                    b
                } else {
                    let path: String = v
                        .extract()
                        .map_err(|_| PyValueError::new_err("image 'data' must be bytes"))?;
                    std::fs::read(&path).map_err(|e| {
                        PyValueError::new_err(format!("cannot read image path {path:?}: {e}"))
                    })?
                }
            }
            _ => {
                return Err(PyValueError::new_err(
                    "image requires 'data' (bytes) or 'path' (str)",
                ));
            }
        };
        let format = detect_image_format(&bytes).ok_or_else(|| {
            PyValueError::new_err(
                "unsupported image format: expected png, jpeg, or gif magic bytes",
            )
        })?;
        let anchor = if let Some(a) = d.get_item("anchor")? {
            if a.is_none() {
                Anchor::default()
            } else {
                parse_anchor(&a)?
            }
        } else {
            Anchor::default()
        };
        out.push(Image {
            bytes: Arc::from(bytes),
            format,
            anchor,
        });
    }
    Ok(out)
}

fn parse_defined_names(obj: &Bound<'_, PyAny>) -> PyResult<Vec<DefinedName>> {
    let list = obj
        .cast::<PyList>()
        .map_err(|_| PyValueError::new_err("defined_names must be a list"))?;
    let mut out = Vec::new();
    for item in list.iter() {
        let d = item
            .cast::<PyDict>()
            .map_err(|_| PyValueError::new_err("defined name must be a dict"))?;
        let name: String = d
            .get_item("name")?
            .ok_or_else(|| PyValueError::new_err("defined name requires name"))?
            .extract()?;
        let value: String = d
            .get_item("value")?
            .or(d.get_item("attr_text")?)
            .ok_or_else(|| PyValueError::new_err("defined name requires value"))?
            .extract()?;
        let local_sheet_id = d
            .get_item("local_sheet_id")?
            .map(|v| v.extract())
            .transpose()?;
        let hidden = d
            .get_item("hidden")?
            .map(|v| v.extract())
            .transpose()?
            .unwrap_or(false);
        out.push(DefinedName {
            name,
            value,
            local_sheet_id,
            hidden,
        });
    }
    Ok(out)
}

fn parse_chartsheets(obj: &Bound<'_, PyAny>) -> PyResult<Vec<ChartsheetSpec>> {
    let list = obj
        .cast::<PyList>()
        .map_err(|_| PyValueError::new_err("chartsheets must be a list"))?;
    let mut out = Vec::new();
    for item in list.iter() {
        let d = item
            .cast::<PyDict>()
            .map_err(|_| PyValueError::new_err("chartsheet must be a dict"))?;
        let title: String = d
            .get_item("name")?
            .or(d.get_item("title")?)
            .ok_or_else(|| PyValueError::new_err("chartsheet requires name"))?
            .extract()?;
        let charts = if let Some(c) = d.get_item("charts")? {
            parse_charts(&c)?
        } else {
            Vec::new()
        };
        out.push(ChartsheetSpec { title, charts });
    }
    Ok(out)
}

fn parse_doc_props(obj: &Bound<'_, PyAny>) -> PyResult<DocProps> {
    let d = obj
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("props must be a dict"))?;
    let mut p = DocProps::default();
    if let Some(v) = d.get_item("title")? {
        p.title = Some(v.extract()?);
    }
    if let Some(v) = d.get_item("creator")? {
        p.creator = Some(v.extract()?);
    }
    if let Some(v) = d.get_item("description")? {
        p.description = Some(v.extract()?);
    }
    if let Some(v) = d.get_item("subject")? {
        p.subject = Some(v.extract()?);
    }
    if let Some(v) = d.get_item("last_modified_by")? {
        p.last_modified_by = Some(v.extract()?);
    }
    if let Some(v) = d.get_item("company")? {
        p.company = Some(v.extract()?);
    }
    if let Some(v) = d.get_item("custom")? {
        if let Ok(dict) = v.cast::<PyDict>() {
            for (k, val) in dict.iter() {
                p.custom.push((k.extract()?, val.extract()?));
            }
        } else if let Ok(list) = v.cast::<PyList>() {
            for item in list.iter() {
                let t = item.cast::<PyDict>().map_err(|_| {
                    PyValueError::new_err("custom prop must be dict with name/value")
                })?;
                let name: String = t
                    .get_item("name")?
                    .ok_or_else(|| PyValueError::new_err("custom prop name"))?
                    .extract()?;
                let value: String = t
                    .get_item("value")?
                    .ok_or_else(|| PyValueError::new_err("custom prop value"))?
                    .extract()?;
                p.custom.push((name, value));
            }
        }
    }
    Ok(p)
}

fn try_parse_numeric_grid(
    sheet_name: String,
    data_obj: &Bound<'_, PyAny>,
) -> PyResult<Option<NumericGrid>> {
    if let Ok(interface) = data_obj.getattr("__array_interface__") {
        if let Ok(dict) = interface.cast::<PyDict>() {
            let shape_obj = dict.get_item("shape")?;
            let typestr_obj = dict.get_item("typestr")?;
            let data_obj_item = dict.get_item("data")?;
            let strides = dict.get_item("strides")?;

            if let (Some(shape_any), Some(type_any), Some(data_any)) =
                (shape_obj, typestr_obj, data_obj_item)
            {
                if let (Ok(shape), Ok(typestr), Ok(data)) = (
                    shape_any.cast::<PyTuple>(),
                    type_any.extract::<String>(),
                    data_any.cast::<PyTuple>(),
                ) {
                    let is_c_contiguous =
                        strides.is_none() || strides.as_ref().is_some_and(|s| s.is_none());
                    if shape.len() == 2 && is_c_contiguous {
                        let nrows: usize = shape.get_item(0)?.extract()?;
                        let ncols: usize = shape.get_item(1)?.extract()?;
                        let ptr_val: usize = data.get_item(0)?.extract()?;
                        if ptr_val != 0 {
                            if let Some(total) = nrows.checked_mul(ncols) {
                                if typestr.ends_with("f8") {
                                    let slice = unsafe {
                                        std::slice::from_raw_parts(ptr_val as *const f64, total)
                                    };
                                    return Ok(Some(NumericGrid {
                                        sheet_name,
                                        nrows: nrows as u32,
                                        ncols: ncols as u32,
                                        values: Arc::new(slice.to_vec()),
                                    }));
                                } else if typestr.ends_with("f4") {
                                    let slice = unsafe {
                                        std::slice::from_raw_parts(ptr_val as *const f32, total)
                                    };
                                    let f64_values: Vec<f64> =
                                        slice.iter().map(|&x| x as f64).collect();
                                    return Ok(Some(NumericGrid {
                                        sheet_name,
                                        nrows: nrows as u32,
                                        ncols: ncols as u32,
                                        values: Arc::new(f64_values),
                                    }));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(None)
}

#[allow(clippy::too_many_arguments)]
fn build_workbook_from_py(
    sheets: &Bound<'_, PyAny>,
    string_mode: &str,
    emit_cached_values: bool,
    date1904: bool,
    date_iso: bool,
    features: Option<&Bound<'_, PyAny>>,
    active_tab: u32,
    named_styles: Option<&Bound<'_, PyAny>>,
    props: Option<&Bound<'_, PyAny>>,
    defined_names: Option<&Bound<'_, PyAny>>,
    chartsheets: Option<&Bound<'_, PyAny>>,
    lock_structure: bool,
    external_links: Option<&Bound<'_, PyAny>>,
    creator: Option<&str>,
    macro_enabled: bool,
    recalculate: bool,
) -> PyResult<Workbook> {
    let mut opts = WriteOptions {
        string_mode: parse_string_mode(string_mode)?,
        emit_cached_values,
        date1904,
        date_iso,
        features: parse_write_features(features)?,
        auto_sst_threshold: AUTO_SST_THRESHOLD,
    };

    let sheet_list = sheets
        .cast::<PyList>()
        .map_err(|_| PyValueError::new_err("sheets must be a list of sheet dicts"))?;
    if sheet_list.is_empty() {
        return Err(PyValueError::new_err("sheets must be non-empty"));
    }

    let mut wb = Workbook {
        sheets: Vec::with_capacity(sheet_list.len()),
        options: opts.clone(),
        active_tab,
        creator: creator.unwrap_or("kyrax").into(),
        numeric_columns: None,
        named_styles: Vec::new(),
        style_work: false,
        props: DocProps::default(),
        lock_structure,
        defined_names: Vec::new(),
        external_links: Vec::new(),
        chartsheets: Vec::new(),
        macro_enabled,
        vba_archive_path: None,
    };

    // `grid` is a write-only fast path: it is consumed here into `numeric_columns`
    // and never read by `parse_sheet_dict`, so any rejection must raise rather than
    // silently emit an empty sheet.
    for s in sheet_list.iter() {
        let Ok(dict) = s.cast::<PyDict>() else {
            continue;
        };
        if !dict.contains("grid")? {
            continue;
        }
        let name = opt_str(dict, "name")?.unwrap_or_else(|| "Sheet1".to_string());

        if dict.contains("columns")? || dict.contains("data")? || dict.contains("rows")? {
            return Err(PyValueError::new_err(
                "'grid' key is mutually exclusive with 'columns' and 'rows'",
            ));
        }
        if sheet_list.len() != 1 {
            return Err(PyValueError::new_err(
                "'grid' is only supported for a single-sheet workbook; \
                 use 'columns' or 'rows' for multi-sheet workbooks",
            ));
        }

        // Check eligibility: no styles, merges, CF, DV, freeze_panes attached
        for key in [
            "cell_styles",
            "style_palette",
            "conditional_formatting",
            "data_validations",
            "merged_cells",
            "freeze_panes",
        ] {
            if dict.contains(key)? {
                return Err(PyValueError::new_err(format!(
                    "'grid' is incompatible with '{key}'; use 'columns' or 'rows' instead"
                )));
            }
        }

        let grid_obj = dict
            .get_item("grid")?
            .ok_or_else(|| PyValueError::new_err("'grid' must not be None"))?;
        match try_parse_numeric_grid(name, &grid_obj)? {
            Some(grid) => wb.numeric_columns = Some(grid),
            None => {
                return Err(PyValueError::new_err(
                    "'grid' must be a 2-D C-contiguous NumPy array of dtype float32 or float64",
                ));
            }
        }
    }

    if let Some(p) = props {
        if !p.is_none() {
            wb.props = parse_doc_props(p)?;
            if let Some(c) = &wb.props.creator {
                wb.creator = c.clone();
            }
        }
    }
    if let Some(dn) = defined_names {
        if !dn.is_none() {
            wb.defined_names = parse_defined_names(dn)?;
        }
    }
    if let Some(cs) = chartsheets {
        if !cs.is_none() {
            wb.chartsheets = parse_chartsheets(cs)?;
        }
    }
    if let Some(el) = external_links {
        if !el.is_none() {
            let list = el.cast::<PyList>().map_err(|_| {
                PyValueError::new_err("external_links must be a list of targets or dicts")
            })?;
            for item in list.iter() {
                if let Ok(s) = item.extract::<String>() {
                    wb.external_links.push(ExternalLink { target: s });
                } else if let Ok(d) = item.cast::<PyDict>() {
                    let target: String = d
                        .get_item("target")?
                        .ok_or_else(|| PyValueError::new_err("external_link needs target"))?
                        .extract()?;
                    wb.external_links.push(ExternalLink { target });
                }
            }
        }
    }

    if let Some(ns) = named_styles {
        if !ns.is_none() {
            wb.named_styles = parse_named_styles(ns)?;
            wb.style_work = true;
        }
    }

    for s in sheet_list.iter() {
        // per-sheet named_styles merge
        if let Ok(d) = s.cast::<PyDict>() {
            if let Some(ns) = d.get_item("named_styles")? {
                if !ns.is_none() {
                    wb.named_styles.extend(parse_named_styles(&ns)?);
                    wb.style_work = true;
                }
            }
        }
        let sheet = parse_sheet_dict(&s, &opts)?;
        if sheet.needs_style_work {
            wb.style_work = true;
        }
        wb.sheets.push(sheet);
    }

    // Auto-enable STYLES/CF_DV when content requires them
    if wb.needs_style_engine() {
        opts.features = opts
            .features
            .union(WriteFeatures::STYLES)
            .union(WriteFeatures::CF_DV);
    }
    wb.options = opts;
    wb.auto_enable_structural_features();

    // Formula hydration runs here, on the fully built model, so the writer sees
    // computed caches as ordinary `cached` values and needs no special case.
    // `force_recalc` is on: a caller asking to recalculate wants its own
    // freshly written formulas computed, not whatever cache came along.
    if recalculate {
        let options = crate::turbo::calc::CalcOptions {
            date1904: wb.options.date1904,
            force_recalc: true,
            max_iterations: 0,
        };
        crate::turbo::calc::hydrate_workbook(&mut wb, &options);
    }

    Ok(wb)
}

/// Write an XLSX workbook (turbo fast path).
///
/// Parameters
/// ----------
/// path : str
///     Output file path.
/// sheets : list[dict]
///     Each sheet: ``name``, optional ``visibility``, ``columns`` / ``rows``,
///     ``formulas``, ``row_dims``, ``col_dims``, ``freeze_panes``, plus W2
///     ``cell_styles``, ``style_palette``, ``conditional_formatting``,
///     ``data_validations``, ``named_styles``.
/// string_mode : str
///     ``"inline"`` (default), ``"sst"``, or ``"auto"``.
/// emit_cached_values : bool
///     Emit formula cached ``<v>`` when supplied (default True).
/// date1904 : bool
///     Workbook 1904 date system flag (default False).
/// date_iso : bool
///     Write date/datetime values as ISO 8601 strings with type "d" (default False).
/// features : str | list[str] | None
///     Write feature flags; ``"core"`` | ``"all"`` | ``"styles"`` or list.
/// active_tab : int
///     Active sheet index (default 0).
/// named_styles : list[dict] | None
///     Workbook-level named styles (W2).
#[pyfunction(name = "write_excel_turbo")]
#[pyo3(signature = (
    path,
    sheets,
    *,
    string_mode = "inline",
    emit_cached_values = true,
    date1904 = false,
    date_iso = false,
    features = None,
    active_tab = 0,
    named_styles = None,
    props = None,
    defined_names = None,
    chartsheets = None,
    lock_structure = false,
    external_links = None,
    creator = None,
    macro_enabled = false,
    recalculate = false,
))]
#[allow(clippy::too_many_arguments)]
pub fn py_write_excel_turbo(
    py: Python<'_>,
    path: &str,
    sheets: &Bound<'_, PyAny>,
    string_mode: &str,
    emit_cached_values: bool,
    date1904: bool,
    date_iso: bool,
    features: Option<&Bound<'_, PyAny>>,
    active_tab: u32,
    named_styles: Option<&Bound<'_, PyAny>>,
    props: Option<&Bound<'_, PyAny>>,
    defined_names: Option<&Bound<'_, PyAny>>,
    chartsheets: Option<&Bound<'_, PyAny>>,
    lock_structure: bool,
    external_links: Option<&Bound<'_, PyAny>>,
    creator: Option<&str>,
    macro_enabled: bool,
    recalculate: bool,
) -> PyResult<()> {
    let wb = build_workbook_from_py(
        sheets,
        string_mode,
        emit_cached_values,
        date1904,
        date_iso,
        features,
        active_tab,
        named_styles,
        props,
        defined_names,
        chartsheets,
        lock_structure,
        external_links,
        creator,
        macro_enabled,
        recalculate,
    )?;
    py.detach(|| save_workbook(&wb, path))
        .map_err(write_err_to_py)
}

/// Write an XLSX workbook streaming to a file path.
#[pyfunction(name = "write_excel_turbo_stream")]
#[pyo3(signature = (
    path,
    sheets,
    *,
    string_mode = "inline",
    emit_cached_values = true,
    date1904 = false,
    date_iso = false,
    features = None,
    active_tab = 0,
    named_styles = None,
    props = None,
    defined_names = None,
    chartsheets = None,
    lock_structure = false,
    external_links = None,
    creator = None,
    macro_enabled = false,
    recalculate = false,
))]
#[allow(clippy::too_many_arguments)]
pub fn py_write_excel_turbo_stream(
    py: Python<'_>,
    path: &str,
    sheets: &Bound<'_, PyAny>,
    string_mode: &str,
    emit_cached_values: bool,
    date1904: bool,
    date_iso: bool,
    features: Option<&Bound<'_, PyAny>>,
    active_tab: u32,
    named_styles: Option<&Bound<'_, PyAny>>,
    props: Option<&Bound<'_, PyAny>>,
    defined_names: Option<&Bound<'_, PyAny>>,
    chartsheets: Option<&Bound<'_, PyAny>>,
    lock_structure: bool,
    external_links: Option<&Bound<'_, PyAny>>,
    creator: Option<&str>,
    macro_enabled: bool,
    recalculate: bool,
) -> PyResult<()> {
    let wb = build_workbook_from_py(
        sheets,
        string_mode,
        emit_cached_values,
        date1904,
        date_iso,
        features,
        active_tab,
        named_styles,
        props,
        defined_names,
        chartsheets,
        lock_structure,
        external_links,
        creator,
        macro_enabled,
        recalculate,
    )?;
    py.detach(|| {
        let file = std::fs::File::create(path)?;
        save_workbook_stream(&wb, file)?;
        Ok::<(), std::io::Error>(())
    })
    .map_err(write_err_to_py)
}

/// Write workbook and return XLSX bytes (no filesystem).
#[pyfunction(name = "write_excel_turbo_bytes")]
#[pyo3(signature = (
    sheets,
    *,
    string_mode = "inline",
    emit_cached_values = true,
    date1904 = false,
    date_iso = false,
    features = None,
    active_tab = 0,
    named_styles = None,
    props = None,
    defined_names = None,
    chartsheets = None,
    lock_structure = false,
    external_links = None,
    creator = None,
    macro_enabled = false,
    recalculate = false,
))]
#[allow(clippy::too_many_arguments)]
pub fn py_write_excel_turbo_bytes<'py>(
    py: Python<'py>,
    sheets: &Bound<'_, PyAny>,
    string_mode: &str,
    emit_cached_values: bool,
    date1904: bool,
    date_iso: bool,
    features: Option<&Bound<'_, PyAny>>,
    active_tab: u32,
    named_styles: Option<&Bound<'_, PyAny>>,
    props: Option<&Bound<'_, PyAny>>,
    defined_names: Option<&Bound<'_, PyAny>>,
    chartsheets: Option<&Bound<'_, PyAny>>,
    lock_structure: bool,
    external_links: Option<&Bound<'_, PyAny>>,
    creator: Option<&str>,
    macro_enabled: bool,
    recalculate: bool,
) -> PyResult<Bound<'py, pyo3::types::PyBytes>> {
    let wb = build_workbook_from_py(
        sheets,
        string_mode,
        emit_cached_values,
        date1904,
        date_iso,
        features,
        active_tab,
        named_styles,
        props,
        defined_names,
        chartsheets,
        lock_structure,
        external_links,
        creator,
        macro_enabled,
        recalculate,
    )?;
    let bytes = py
        .detach(|| write_workbook_bytes(&wb))
        .map_err(write_err_to_py)?;
    Ok(pyo3::types::PyBytes::new(py, &bytes))
}

// silence unused
#[allow(dead_code)]
fn _use_turbo_err(e: TurboError) -> PyErr {
    turbo_err_to_py(e)
}

#[pyfunction(name = "get_column_letter")]
pub fn py_get_column_letter(col: u32) -> PyResult<String> {
    if col == 0 || col > MAX_GRID_COLS {
        return Err(PyValueError::new_err(format!(
            "column index {col} is out of bounds (must be 1..={MAX_GRID_COLS})"
        )));
    }
    let mut num = col;
    let mut s = Vec::new();
    while num > 0 {
        let rem = (num - 1) % 26;
        s.push((b'A' + rem as u8) as char);
        num = (num - 1) / 26;
    }
    s.reverse();
    Ok(s.into_iter().collect())
}

#[pyfunction(name = "column_index_from_string")]
pub fn py_column_index_from_string(s: &str) -> PyResult<u32> {
    let trimmed = s.trim();
    if trimmed.is_empty() || trimmed.len() > 3 || !trimmed.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(PyValueError::new_err(format!(
            "invalid column coordinate string: '{s}'"
        )));
    }
    let mut col: u32 = 0;
    for b in trimmed.bytes() {
        let v = (b.to_ascii_uppercase() - b'A' + 1) as u32;
        col = col * 26 + v;
    }
    if col == 0 || col > MAX_GRID_COLS {
        return Err(PyValueError::new_err(format!(
            "column coordinate '{s}' resolves to index {col} out of bounds (1..={MAX_GRID_COLS})"
        )));
    }
    Ok(col)
}

#[pyfunction(name = "coordinate_to_tuple")]
pub fn py_coordinate_to_tuple(coord: &str) -> PyResult<(u32, u32)> {
    let Some((r1, c1, _r2, _c2)) = parse_ref_range_strict(coord.as_bytes()) else {
        return Err(PyValueError::new_err(format!(
            "invalid cell coordinate: '{coord}'"
        )));
    };
    Ok((r1, c1))
}

#[pyfunction(name = "range_boundaries")]
pub fn py_range_boundaries(range_str: &str) -> PyResult<(u32, u32, u32, u32)> {
    let Some((r1, c1, r2, c2)) = parse_ref_range_strict(range_str.as_bytes()) else {
        return Err(PyValueError::new_err(format!(
            "invalid range coordinate: '{range_str}'"
        )));
    };
    Ok((c1, r1, c2, r2))
}

#[pyfunction(name = "quote_sheetname")]
pub fn py_quote_sheetname(name: &str) -> String {
    let needs_quote = name.contains(' ')
        || name.contains('\'')
        || name.contains('!')
        || name.contains('-')
        || name.contains('+')
        || (!name.is_empty() && name.chars().all(|c| c.is_ascii_digit()));
    if needs_quote {
        let escaped = name.replace('\'', "''");
        format!("'{escaped}'")
    } else {
        name.to_string()
    }
}

fn original_sheet_locked_for_overlay(ov: &WorkbookOverlay, sheet_name: &str) -> TurboResult<Option<Sheet>> {
    let Some(target) = ov.archive_map.sheet_name_map.get(sheet_name) else {
        return Ok(None);
    };
    let Some(xml) = read_entry(&ov.archive_map.source_bytes, target)? else {
        return Ok(None);
    };
    let sheet = hydrate_sheet_from_xml(&xml, &ov.archive_map.shared_strings)?;
    Ok(Some(sheet))
}

fn cell_value_locked_for_overlay(
    ov: &WorkbookOverlay,
    original: Option<&Sheet>,
    sheet_name: &str,
    row: u32,
    col: u32,
) -> TurboResult<CellValue> {
    if let Some(so) = ov.sheet_overlays.get(sheet_name) {
        if let Some(v) = so.modified_cells.get(&(row, col)) {
            return Ok(v.clone());
        }
    }
    if let Some(sheet) = original {
        for r in &sheet.rows {
            if r.row != row {
                continue;
            }
            for c in &r.cells {
                if c.col == col {
                    return Ok(c.value.clone());
                }
            }
        }
    }
    Ok(CellValue::Empty)
}

#[pyclass(name = "Cell")]
pub struct PyCell {
    sheet_name: String,
    overlay: Arc<std::sync::Mutex<WorkbookOverlay>>,
    row: u32,
    col: u32,
}

#[pymethods]
impl PyCell {
    #[getter]
    fn row(&self) -> u32 {
        self.row
    }

    #[getter]
    fn column(&self) -> u32 {
        self.col
    }

    #[getter]
    fn coordinate(&self) -> String {
        let col_letter = py_get_column_letter(self.col).unwrap_or_else(|_| "A".into());
        format!("{}{}", col_letter, self.row)
    }

    #[getter]
    fn value<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let mut ov = self
            .overlay
            .lock()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        if let Some(so) = ov.sheet_overlays.get(&self.sheet_name) {
            if let Some(v) = so.modified_cells.get(&(self.row, self.col)) {
                return cell_value_to_py(py, v);
            }
        }
        let hydrated = ov
            .hydrated_sheet(&self.sheet_name)
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        drop(ov);

        if let Some(sheet) = hydrated {
            if let Ok(idx) = sheet.rows.binary_search_by(|r| r.row.cmp(&self.row)) {
                let r = &sheet.rows[idx];
                if let Ok(cidx) = r.cells.binary_search_by(|c| c.col.cmp(&self.col)) {
                    return cell_value_to_py(py, &r.cells[cidx].value);
                }
            }
        }
        cell_value_to_py(py, &CellValue::Empty)
    }

    #[setter]
    fn set_value(&self, value: &Bound<'_, PyAny>) -> PyResult<()> {
        let cell_val = py_to_cell_value_flagged(value, false, None)?;
        let mut ov = self
            .overlay
            .lock()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        ov.set_cell(&self.sheet_name, self.row, self.col, cell_val);
        Ok(())
    }

    #[pyo3(signature = (row = 0, column = 0))]
    fn offset(&self, row: i64, column: i64) -> PyResult<PyCell> {
        let new_row = self.row as i64 + row;
        let new_col = self.col as i64 + column;
        if new_row < 1 || new_row > MAX_GRID_ROWS as i64 || new_col < 1 || new_col > MAX_GRID_COLS as i64 {
            return Err(PyValueError::new_err(format!(
                "offset result ({new_row}, {new_col}) is out of grid (rows 1..={MAX_GRID_ROWS}, cols 1..={MAX_GRID_COLS})"
            )));
        }
        Ok(PyCell {
            sheet_name: self.sheet_name.clone(),
            overlay: Arc::clone(&self.overlay),
            row: new_row as u32,
            col: new_col as u32,
        })
    }

    #[getter]
    fn number_format(&self) -> Option<String> {
        let ov = self.overlay.lock().ok()?;
        if let Some(so) = ov.sheet_overlays.get(&self.sheet_name) {
            if let Some(desc) = so.modified_styles.get(&(self.row, self.col)) {
                return desc.num_fmt.clone();
            }
        }
        None
    }

    #[setter]
    fn set_number_format(&self, fmt: Option<&str>) -> PyResult<()> {
        let mut ov = self
            .overlay
            .lock()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let so = ov.sheet_overlays.entry(self.sheet_name.clone()).or_default();
        let desc = so.modified_styles.entry((self.row, self.col)).or_default();
        desc.num_fmt = fmt.map(|s| s.to_string());
        so.is_dirty = true;
        Ok(())
    }

    #[getter]
    fn hyperlink(&self) -> Option<String> {
        None
    }

    #[setter]
    fn set_hyperlink(&self, _link: Option<&str>) -> PyResult<()> {
        Err(pyo3::exceptions::PyNotImplementedError::new_err(
            "hyperlink editing is not supported yet",
        ))
    }

    #[getter]
    fn comment(&self) -> Option<String> {
        None
    }

    #[setter]
    fn set_comment(&self, _comment: Option<&str>) -> PyResult<()> {
        Err(pyo3::exceptions::PyNotImplementedError::new_err(
            "comment editing is not supported yet",
        ))
    }
}

#[pyclass(name = "SheetRowIter")]
pub struct PySheetRowIter {
    sheet_name: String,
    overlay: Arc<std::sync::Mutex<WorkbookOverlay>>,
    r1: u32,
    r2: u32,
    c1: u32,
    c2: u32,
    cursor: u32,
    values_only: bool,
    col_major: bool,
}

#[pymethods]
impl PySheetRowIter {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__<'py>(&mut self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        let max_idx = if self.col_major { self.c2 } else { self.r2 };
        if self.cursor > max_idx {
            return Ok(None);
        }
        let current = self.cursor;
        self.cursor += 1;

        let mut ov = self.overlay.lock().map_err(|e| {
            PyValueError::new_err(format!("lock error: {e}"))
        })?;
        let hydrated = ov.hydrated_sheet(&self.sheet_name).map_err(|e| {
            PyValueError::new_err(format!("hydration error: {e}"))
        })?;

        if !self.col_major {
            let row = current;
            let mut items = Vec::with_capacity((self.c2.saturating_sub(self.c1) + 1) as usize);
            for col in self.c1..=self.c2 {
                if self.values_only {
                    let mut val = CellValue::Empty;
                    if let Some(so) = ov.sheet_overlays.get(&self.sheet_name) {
                        if let Some(v) = so.modified_cells.get(&(row, col)) {
                            val = v.clone();
                        }
                    }
                    if matches!(val, CellValue::Empty) {
                        if let Some(sheet) = &hydrated {
                            if let Ok(idx) = sheet.rows.binary_search_by(|r| r.row.cmp(&row)) {
                                let r = &sheet.rows[idx];
                                if let Ok(cidx) = r.cells.binary_search_by(|c| c.col.cmp(&col)) {
                                    val = r.cells[cidx].value.clone();
                                }
                            }
                        }
                    }
                    items.push(cell_value_to_py(py, &val)?);
                } else {
                    let cell = PyCell {
                        sheet_name: self.sheet_name.clone(),
                        overlay: Arc::clone(&self.overlay),
                        row,
                        col,
                    };
                    use pyo3::IntoPyObjectExt;
                    items.push(cell.into_bound_py_any(py)?);
                }
            }
            let tuple = PyTuple::new(py, items)?;
            Ok(Some(tuple.into_any()))
        } else {
            let col = current;
            let mut items = Vec::with_capacity((self.r2.saturating_sub(self.r1) + 1) as usize);
            for row in self.r1..=self.r2 {
                if self.values_only {
                    let mut val = CellValue::Empty;
                    if let Some(so) = ov.sheet_overlays.get(&self.sheet_name) {
                        if let Some(v) = so.modified_cells.get(&(row, col)) {
                            val = v.clone();
                        }
                    }
                    if matches!(val, CellValue::Empty) {
                        if let Some(sheet) = &hydrated {
                            if let Ok(idx) = sheet.rows.binary_search_by(|r| r.row.cmp(&row)) {
                                let r = &sheet.rows[idx];
                                if let Ok(cidx) = r.cells.binary_search_by(|c| c.col.cmp(&col)) {
                                    val = r.cells[cidx].value.clone();
                                }
                            }
                        }
                    }
                    items.push(cell_value_to_py(py, &val)?);
                } else {
                    let cell = PyCell {
                        sheet_name: self.sheet_name.clone(),
                        overlay: Arc::clone(&self.overlay),
                        row,
                        col,
                    };
                    use pyo3::IntoPyObjectExt;
                    items.push(cell.into_bound_py_any(py)?);
                }
            }
            let tuple = PyTuple::new(py, items)?;
            Ok(Some(tuple.into_any()))
        }
    }
}

#[pyclass(name = "EditableSheet")]
pub struct PyEditableSheet {
    sheet_name: String,
    overlay: Arc<std::sync::Mutex<WorkbookOverlay>>,
    original: OnceLock<Option<Sheet>>,
}

#[pymethods]
impl PyEditableSheet {
    #[getter]
    fn title(&self) -> String {
        self.sheet_name.clone()
    }

    #[setter]
    fn set_title(&mut self, new_title: &str) -> PyResult<()> {
        let mut ov = self
            .overlay
            .lock()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let existing = ov.sheet_names();
        let other_names: Vec<String> = existing.into_iter().filter(|n| n != &self.sheet_name).collect();
        validate_sheet_name(new_title, &other_names).map_err(turbo_err_to_py)?;
        ov.rename_sheet(&self.sheet_name, new_title).map_err(turbo_err_to_py)?;
        self.sheet_name = new_title.to_string();
        Ok(())
    }

    #[getter]
    fn min_row(&self) -> PyResult<u32> {
        let (min_r, _, _, _) = self.compute_bounds()?;
        Ok(min_r)
    }

    #[getter]
    fn max_row(&self) -> PyResult<u32> {
        let (_, max_r, _, _) = self.compute_bounds()?;
        Ok(max_r)
    }

    #[getter]
    fn min_column(&self) -> PyResult<u32> {
        let (_, _, min_c, _) = self.compute_bounds()?;
        Ok(min_c)
    }

    #[getter]
    fn max_column(&self) -> PyResult<u32> {
        let (_, _, _, max_c) = self.compute_bounds()?;
        Ok(max_c)
    }

    #[getter]
    fn dimensions(&self) -> PyResult<String> {
        let (min_r, max_r, min_c, max_c) = self.compute_bounds()?;
        if max_r == 0 || max_c == 0 {
            return Ok("A1:A1".into());
        }
        let c1 = py_get_column_letter(min_c)?;
        let c2 = py_get_column_letter(max_c)?;
        Ok(format!("{c1}{min_r}:{c2}{max_r}"))
    }

    fn append(&self, iterable: &Bound<'_, PyAny>) -> PyResult<()> {
        let (_, max_r, _, _) = self.compute_bounds()?;
        let append_row = if max_r == 0 { 1 } else { max_r + 1 };
        let items: Vec<Bound<'_, PyAny>> = if let Ok(it) = iterable.try_iter() {
            let mut list = Vec::new();
            for elem in it {
                list.push(elem?);
            }
            list
        } else {
            return Err(PyTypeError::new_err("append argument must be iterable"));
        };
        let mut style_work = false;
        let mut ov = self
            .overlay
            .lock()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        for (i, item) in items.iter().enumerate() {
            let col = (i + 1) as u32;
            let val = py_to_cell_value_flagged(item, false, Some(&mut style_work))?;
            let wrap_style = extract_wrapper_style(item)?;
            ov.set_cell(&self.sheet_name, append_row, col, val);
            if let Some(desc) = wrap_style {
                ov.set_cell_style(&self.sheet_name, append_row, col, *desc);
            }
        }
        Ok(())
    }

    #[pyo3(signature = (row, column, value = None))]
    fn cell(&self, row: u32, column: u32, value: Option<&Bound<'_, PyAny>>) -> PyResult<PyCell> {
        if row == 0 || column == 0 || row > MAX_GRID_ROWS || column > MAX_GRID_COLS {
            return Err(PyValueError::new_err(format!(
                "cell: ({row}, {column}) is out of grid (rows 1..={MAX_GRID_ROWS}, columns 1..={MAX_GRID_COLS})"
            )));
        }
        if let Some(v) = value {
            let mut style_work = false;
            let cell_val = py_to_cell_value_flagged(v, false, Some(&mut style_work))?;
            let wrap_style = extract_wrapper_style(v)?;
            let mut ov = self
                .overlay
                .lock()
                .map_err(|e| PyValueError::new_err(e.to_string()))?;
            ov.set_cell(&self.sheet_name, row, column, cell_val);
            if let Some(desc) = wrap_style {
                ov.set_cell_style(&self.sheet_name, row, column, *desc);
            }
        }
        Ok(PyCell {
            sheet_name: self.sheet_name.clone(),
            overlay: Arc::clone(&self.overlay),
            row,
            col: column,
        })
    }

    #[pyo3(signature = (min_row = None, max_row = None, min_col = None, max_col = None, values_only = false))]
    fn iter_rows(
        &self,
        min_row: Option<u32>,
        max_row: Option<u32>,
        min_col: Option<u32>,
        max_col: Option<u32>,
        values_only: bool,
    ) -> PyResult<PySheetRowIter> {
        let (b_min_r, b_max_r, b_min_c, b_max_c) = self.compute_bounds()?;
        let r1 = min_row.unwrap_or(b_min_r.max(1));
        let r2 = max_row.unwrap_or(b_max_r.max(r1));
        let c1 = min_col.unwrap_or(b_min_c.max(1));
        let c2 = max_col.unwrap_or(b_max_c.max(c1));
        Ok(PySheetRowIter {
            sheet_name: self.sheet_name.clone(),
            overlay: Arc::clone(&self.overlay),
            r1,
            r2,
            c1,
            c2,
            cursor: r1,
            values_only,
            col_major: false,
        })
    }

    #[pyo3(signature = (min_row = None, max_row = None, min_col = None, max_col = None, values_only = false))]
    fn iter_cols(
        &self,
        min_row: Option<u32>,
        max_row: Option<u32>,
        min_col: Option<u32>,
        max_col: Option<u32>,
        values_only: bool,
    ) -> PyResult<PySheetRowIter> {
        let (b_min_r, b_max_r, b_min_c, b_max_c) = self.compute_bounds()?;
        let r1 = min_row.unwrap_or(b_min_r.max(1));
        let r2 = max_row.unwrap_or(b_max_r.max(r1));
        let c1 = min_col.unwrap_or(b_min_c.max(1));
        let c2 = max_col.unwrap_or(b_max_c.max(c1));
        Ok(PySheetRowIter {
            sheet_name: self.sheet_name.clone(),
            overlay: Arc::clone(&self.overlay),
            r1,
            r2,
            c1,
            c2,
            cursor: c1,
            values_only,
            col_major: true,
        })
    }

    #[getter]
    fn values(&self) -> PyResult<PySheetRowIter> {
        self.iter_rows(None, None, None, None, true)
    }

    #[pyo3(name = "set_cell")]
    fn py_set_cell(&self, row: u32, col: u32, value: &Bound<'_, PyAny>) -> PyResult<()> {
        if row == 0 || col == 0 || row > MAX_GRID_ROWS || col > MAX_GRID_COLS {
            return Err(PyValueError::new_err(format!(
                "set_cell: ({row}, {col}) is out of grid (rows 1..={MAX_GRID_ROWS}, columns 1..={MAX_GRID_COLS})"
            )));
        }
        let cell_val = py_to_cell_value(value, false)?;
        let mut ov = self
            .overlay
            .lock()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        ov.set_cell(&self.sheet_name, row, col, cell_val);
        Ok(())
    }

    #[pyo3(name = "insert_rows", signature = (idx, amount = 1))]
    fn py_insert_rows(&self, idx: u32, amount: u32) -> PyResult<()> {
        if idx == 0 {
            return Err(PyValueError::new_err(
                "insert_rows: idx is 1-based and must be >= 1",
            ));
        }
        self.record(|ov| ov.insert_rows(&self.sheet_name, idx, amount))
    }

    #[pyo3(name = "delete_rows", signature = (idx, amount = 1))]
    fn py_delete_rows(&self, idx: u32, amount: u32) -> PyResult<()> {
        if idx == 0 {
            return Err(PyValueError::new_err(
                "delete_rows: idx is 1-based and must be >= 1",
            ));
        }
        self.record(|ov| ov.delete_rows(&self.sheet_name, idx, amount))
    }

    #[pyo3(name = "insert_cols", signature = (idx, amount = 1))]
    fn py_insert_cols(&self, idx: u32, amount: u32) -> PyResult<()> {
        if idx == 0 {
            return Err(PyValueError::new_err(
                "insert_cols: idx is 1-based and must be >= 1",
            ));
        }
        self.record(|ov| ov.insert_cols(&self.sheet_name, idx, amount))
    }

    #[pyo3(name = "delete_cols", signature = (idx, amount = 1))]
    fn py_delete_cols(&self, idx: u32, amount: u32) -> PyResult<()> {
        if idx == 0 {
            return Err(PyValueError::new_err(
                "delete_cols: idx is 1-based and must be >= 1",
            ));
        }
        self.record(|ov| ov.delete_cols(&self.sheet_name, idx, amount))
    }

    #[pyo3(name = "move_range", signature = (range_string, rows = 0, cols = 0, translate = false))]
    fn py_move_range(
        &self,
        range_string: &str,
        rows: i64,
        cols: i64,
        translate: bool,
    ) -> PyResult<()> {
        let Some((r1, c1, r2, c2)) = parse_ref_range_strict(range_string.as_bytes()) else {
            return Err(PyValueError::new_err(format!(
                "move_range: '{range_string}' is not a valid A1 range"
            )));
        };
        self.record(|ov| ov.move_range(&self.sheet_name, r1, c1, r2, c2, rows, cols, translate))
    }

    #[pyo3(name = "set_cell_style", signature = (row, col, *, font=None, fill=None, border=None, num_fmt=None))]
    fn py_set_cell_style(
        &self,
        row: u32,
        col: u32,
        font: Option<&Bound<'_, PyAny>>,
        fill: Option<&Bound<'_, PyAny>>,
        border: Option<&Bound<'_, PyAny>>,
        num_fmt: Option<&str>,
    ) -> PyResult<()> {
        if row == 0 || col == 0 || row > MAX_GRID_ROWS || col > MAX_GRID_COLS {
            return Err(PyValueError::new_err(format!(
                "set_cell_style: ({row}, {col}) is out of grid (rows 1..={MAX_GRID_ROWS}, columns 1..={MAX_GRID_COLS})"
            )));
        }
        let mut desc = StyleDesc::default();
        if let Some(f) = font {
            desc.font = Some(parse_font(f)?);
        }
        if let Some(f) = fill {
            desc.fill = Some(parse_fill(f)?);
        }
        if let Some(b) = border {
            desc.border = Some(parse_border(b)?);
        }
        if let Some(fmt) = num_fmt {
            desc.num_fmt = Some(fmt.to_string());
        }

        let mut ov = self
            .overlay
            .lock()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        ov.set_cell_style(&self.sheet_name, row, col, desc);
        Ok(())
    }

    fn __getitem__<'py>(&self, py: Python<'py>, key: &str) -> PyResult<Bound<'py, PyAny>> {
        let Some((r1, c1, r2, c2)) = parse_ref_range_strict(key.as_bytes()) else {
            return Err(PyValueError::new_err(format!(
                "'{key}' is not a valid A1 cell or range"
            )));
        };
        let ov = self
            .overlay
            .lock()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let original = self
            .original_sheet_locked(&ov)
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let resolve = |row: u32, col: u32| {
            self.cell_value_locked(&ov, original, row, col)
                .map_err(|e| PyValueError::new_err(e.to_string()))
        };
        if r1 == r2 && c1 == c2 {
            let v = resolve(r1, c1)?;
            return cell_value_to_py(py, &v);
        }
        let mut rows: Vec<Bound<'_, PyAny>> = Vec::with_capacity((r2 - r1 + 1) as usize);
        for r in r1..=r2 {
            let mut row_items: Vec<Bound<'_, PyAny>> = Vec::with_capacity((c2 - c1 + 1) as usize);
            for c in c1..=c2 {
                let v = resolve(r, c)?;
                row_items.push(cell_value_to_py(py, &v)?);
            }
            rows.push(PyList::new(py, row_items)?.into_any());
        }
        Ok(PyList::new(py, rows)?.into_any())
    }

    fn __setitem__(&self, key: &str, value: &Bound<'_, PyAny>) -> PyResult<()> {
        let Some((r1, c1, r2, c2)) = parse_ref_range_strict(key.as_bytes()) else {
            return Err(PyValueError::new_err(format!(
                "'{key}' is not a valid A1 cell or range"
            )));
        };
        if r1 == r2 && c1 == c2 {
            let cell_val = py_to_cell_value(value, false)?;
            let mut ov = self
                .overlay
                .lock()
                .map_err(|e| PyValueError::new_err(e.to_string()))?;
            ov.set_cell(&self.sheet_name, r1, c1, cell_val);
            return Ok(());
        }
        let nrows = (r2 - r1 + 1) as usize;
        let ncols = (c2 - c1 + 1) as usize;
        let matrix = extract_2d_values(value, nrows, ncols)?;
        let mut ov = self
            .overlay
            .lock()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        for (dr, row_vals) in matrix.iter().enumerate() {
            for (dc, v) in row_vals.iter().enumerate() {
                ov.set_cell(&self.sheet_name, r1 + dr as u32, c1 + dc as u32, v.clone());
            }
        }
        Ok(())
    }
}

impl PyEditableSheet {
    fn compute_bounds(&self) -> PyResult<(u32, u32, u32, u32)> {
        let ov = self
            .overlay
            .lock()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let orig = self
            .original_sheet_locked(&ov)
            .map_err(|e| PyValueError::new_err(e.to_string()))?;

        let mut min_r = u32::MAX;
        let mut max_r = 0u32;
        let mut min_c = u32::MAX;
        let mut max_c = 0u32;

        if let Some(sheet) = orig {
            for row in &sheet.rows {
                for cell in &row.cells {
                    if !matches!(cell.value, CellValue::Empty) {
                        min_r = min_r.min(row.row);
                        max_r = max_r.max(row.row);
                        min_c = min_c.min(cell.col);
                        max_c = max_c.max(cell.col);
                    }
                }
            }
        }

        if let Some(so) = ov.sheet_overlays.get(&self.sheet_name) {
            for (&(r, c), val) in &so.modified_cells {
                if !matches!(val, CellValue::Empty) {
                    min_r = min_r.min(r);
                    max_r = max_r.max(r);
                    min_c = min_c.min(c);
                    max_c = max_c.max(c);
                }
            }
        }

        if min_r == u32::MAX {
            Ok((1, 0, 1, 0))
        } else {
            Ok((min_r, max_r, min_c, max_c))
        }
    }

    fn record(&self, f: impl FnOnce(&mut WorkbookOverlay)) -> PyResult<()> {
        let mut ov = self
            .overlay
            .lock()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        f(&mut ov);
        Ok(())
    }

    fn original_sheet_locked(&self, ov: &WorkbookOverlay) -> TurboResult<Option<&Sheet>> {
        if let Some(cached) = self.original.get() {
            return Ok(cached.as_ref());
        }
        let Some(target) = ov.archive_map.sheet_name_map.get(&self.sheet_name) else {
            self.original.set(None).ok();
            return Ok(None);
        };
        let Some(xml) = read_entry(&ov.archive_map.source_bytes, target)? else {
            self.original.set(None).ok();
            return Ok(None);
        };
        let sheet = hydrate_sheet_from_xml(&xml, &ov.archive_map.shared_strings)?;
        let _ = self.original.set(Some(sheet));
        Ok(self.original.get().and_then(Option::as_ref))
    }

    fn cell_value_locked(
        &self,
        ov: &WorkbookOverlay,
        original: Option<&Sheet>,
        row: u32,
        col: u32,
    ) -> TurboResult<CellValue> {
        if let Some(so) = ov.sheet_overlays.get(&self.sheet_name) {
            if let Some(v) = so.modified_cells.get(&(row, col)) {
                return Ok(v.clone());
            }
        }
        if let Some(sheet) = original {
            for r in &sheet.rows {
                if r.row != row {
                    continue;
                }
                for c in &r.cells {
                    if c.col == col {
                        return Ok(c.value.clone());
                    }
                }
            }
        }
        Ok(CellValue::Empty)
    }
}

/// Convert a `CellValue` to the Python scalar returned by `ws[key]`.
fn cell_value_to_py<'py>(py: Python<'py>, v: &CellValue) -> PyResult<Bound<'py, PyAny>> {
    use pyo3::IntoPyObjectExt;
    Ok(match v {
        CellValue::Empty => py.None().into_bound(py),
        CellValue::Number(n) | CellValue::DateSerial(n) => n.into_bound_py_any(py)?,
        CellValue::Time(t) => {
            let total_secs = (t * 86400.0).round() as i64;
            let hour = ((total_secs / 3600) % 24) as u32;
            let minute = (((total_secs % 3600) / 60) % 60) as u32;
            let second = (total_secs % 60) as u32;
            let dt_mod = py.import("datetime")?;
            let time_cls = dt_mod.getattr("time")?;
            time_cls.call1((hour, minute, second))?
        }
        CellValue::Duration(d) => {
            let days = d.floor() as i64;
            let rem_secs = ((d - days as f64) * 86400.0).round() as i64;
            let dt_mod = py.import("datetime")?;
            let timedelta_cls = dt_mod.getattr("timedelta")?;
            timedelta_cls.call1((days, rem_secs))?
        }
        CellValue::Bool(b) => b.into_bound_py_any(py)?,
        CellValue::Error(s) | CellValue::Str(s) => s.as_str().into_bound_py_any(py)?,
        CellValue::Rich(rt) => {
            let mut text = String::new();
            for run in &rt.runs {
                match run {
                    RichRun::Text(t) => text.push_str(t.as_str()),
                    RichRun::Block {
                        text: block_text, ..
                    } => {
                        text.push_str(block_text.as_str());
                    }
                }
            }
            text.into_bound_py_any(py)?
        }
        CellValue::Formula { text, .. } => {
            let trimmed = text.strip_prefix('=').unwrap_or(text);
            let mut s = String::with_capacity(trimmed.len() + 1);
            s.push('=');
            s.push_str(trimmed);
            s.into_bound_py_any(py)?
        }
    })
}

fn extract_2d_values(
    value: &Bound<'_, PyAny>,
    nrows: usize,
    ncols: usize,
) -> PyResult<Vec<Vec<CellValue>>> {
    let outer: Vec<Bound<'_, PyAny>> = if let Ok(l) = value.cast::<PyList>() {
        l.iter().collect()
    } else if let Ok(t) = value.cast::<PyTuple>() {
        t.iter().collect()
    } else {
        return Err(PyTypeError::new_err(
            "range assignment value must be a 2D list or tuple",
        ));
    };
    if outer.len() != nrows {
        return Err(PyTypeError::new_err(format!(
            "range assignment expects {nrows} rows, got {}",
            outer.len()
        )));
    }
    let mut matrix: Vec<Vec<CellValue>> = Vec::with_capacity(nrows);
    for row_obj in outer {
        let row_items: Vec<Bound<'_, PyAny>> = if let Ok(l) = row_obj.cast::<PyList>() {
            l.iter().collect()
        } else if let Ok(t) = row_obj.cast::<PyTuple>() {
            t.iter().collect()
        } else {
            return Err(PyTypeError::new_err(
                "each range-assignment row must be a list or tuple",
            ));
        };
        if row_items.len() != ncols {
            return Err(PyTypeError::new_err(format!(
                "range assignment expects {ncols} columns per row, got {}",
                row_items.len()
            )));
        }
        let mut row_vals: Vec<CellValue> = Vec::with_capacity(ncols);
        for item in row_items {
            row_vals.push(py_to_cell_value(&item, false)?);
        }
        matrix.push(row_vals);
    }
    Ok(matrix)
}

#[pyclass(name = "EditableWorkbook")]
pub struct PyEditableWorkbook {
    overlay: Arc<std::sync::Mutex<WorkbookOverlay>>,
    active_idx: std::sync::atomic::AtomicUsize,
}

#[pymethods]
impl PyEditableWorkbook {
    #[new]
    fn new() -> PyResult<Self> {
        let overlay = WorkbookOverlay::new_blank().map_err(turbo_err_to_py)?;
        Ok(Self {
            overlay: Arc::new(std::sync::Mutex::new(overlay)),
            active_idx: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    #[getter]
    fn sheetnames(&self) -> PyResult<Vec<String>> {
        let ov = self
            .overlay
            .lock()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        Ok(ov.sheet_names())
    }

    #[getter]
    fn worksheets(&self) -> PyResult<Vec<PyEditableSheet>> {
        let ov = self
            .overlay
            .lock()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let names = ov.sheet_names();
        let sheets = names
            .into_iter()
            .map(|name| PyEditableSheet {
                sheet_name: name,
                overlay: Arc::clone(&self.overlay),
                original: OnceLock::new(),
            })
            .collect();
        Ok(sheets)
    }

    #[getter]
    fn active(&self) -> PyResult<PyEditableSheet> {
        let names = self.sheetnames()?;
        if names.is_empty() {
            return Err(PyValueError::new_err("workbook contains no sheets"));
        }
        let cur = self.active_idx.load(std::sync::atomic::Ordering::Relaxed);
        let idx = cur.min(names.len() - 1);
        Ok(PyEditableSheet {
            sheet_name: names[idx].clone(),
            overlay: Arc::clone(&self.overlay),
            original: OnceLock::new(),
        })
    }

    #[setter]
    fn set_active(&self, target: &Bound<'_, PyAny>) -> PyResult<()> {
        let names = self.sheetnames()?;
        if let Ok(idx) = target.extract::<usize>() {
            if idx >= names.len() {
                return Err(PyValueError::new_err(format!(
                    "sheet index {idx} out of range (workbook has {} sheets)",
                    names.len()
                )));
            }
            self.active_idx.store(idx, std::sync::atomic::Ordering::Relaxed);
            return Ok(());
        }
        let target_name = if let Ok(s) = target.extract::<String>() {
            s
        } else if let Ok(ws) = target.cast::<PyEditableSheet>() {
            ws.borrow().sheet_name.clone()
        } else {
            return Err(PyTypeError::new_err("active sheet must be integer index, sheet name, or EditableSheet"));
        };
        if let Some(pos) = names.iter().position(|n| n == &target_name) {
            self.active_idx.store(pos, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        } else {
            Err(pyo3::exceptions::PyKeyError::new_err(format!(
                "sheet '{target_name}' not found in workbook"
            )))
        }
    }

    #[pyo3(signature = (title = None, index = None))]
    fn create_sheet(&self, title: Option<&str>, index: Option<usize>) -> PyResult<PyEditableSheet> {
        let mut ov = self
            .overlay
            .lock()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let existing = ov.sheet_names();
        let final_title = match title {
            Some(t) => {
                validate_sheet_name(t, &existing).map_err(turbo_err_to_py)?;
                t.to_string()
            }
            None => {
                let mut n = existing.len() + 1;
                loop {
                    let candidate = format!("Sheet{n}");
                    if !existing.iter().any(|s| s.eq_ignore_ascii_case(&candidate)) {
                        break candidate;
                    }
                    n += 1;
                }
            }
        };
        ov.create_sheet(&final_title, index).map_err(turbo_err_to_py)?;
        Ok(PyEditableSheet {
            sheet_name: final_title,
            overlay: Arc::clone(&self.overlay),
            original: OnceLock::new(),
        })
    }

    fn remove(&self, worksheet_or_name: &Bound<'_, PyAny>) -> PyResult<()> {
        let name = if let Ok(s) = worksheet_or_name.extract::<String>() {
            s
        } else if let Ok(ws) = worksheet_or_name.cast::<PyEditableSheet>() {
            ws.borrow().sheet_name.clone()
        } else {
            return Err(PyTypeError::new_err("remove expects sheet name or EditableSheet"));
        };
        let mut ov = self
            .overlay
            .lock()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let names = ov.sheet_names();
        if !names.contains(&name) {
            return Err(pyo3::exceptions::PyKeyError::new_err(format!(
                "sheet '{name}' not found in workbook"
            )));
        }
        if names.len() <= 1 {
            return Err(PyValueError::new_err("cannot remove the only worksheet in workbook"));
        }
        ov.delete_sheet(&name).map_err(turbo_err_to_py)?;
        Ok(())
    }

    fn __delitem__(&self, sheet_name: &str) -> PyResult<()> {
        let mut ov = self
            .overlay
            .lock()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let names = ov.sheet_names();
        if !names.contains(&sheet_name.to_string()) {
            return Err(pyo3::exceptions::PyKeyError::new_err(format!(
                "sheet '{sheet_name}' not found in workbook"
            )));
        }
        if names.len() <= 1 {
            return Err(PyValueError::new_err("cannot remove the only worksheet in workbook"));
        }
        ov.delete_sheet(sheet_name).map_err(turbo_err_to_py)?;
        Ok(())
    }

    fn copy_worksheet(&self, source: &Bound<'_, PyAny>) -> PyResult<PyEditableSheet> {
        let src_name = if let Ok(s) = source.extract::<String>() {
            s
        } else if let Ok(ws) = source.cast::<PyEditableSheet>() {
            ws.borrow().sheet_name.clone()
        } else {
            return Err(PyTypeError::new_err("copy_worksheet expects sheet name or EditableSheet"));
        };
        let mut ov = self
            .overlay
            .lock()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let existing = ov.sheet_names();
        if !existing.contains(&src_name) {
            return Err(pyo3::exceptions::PyKeyError::new_err(format!(
                "source sheet '{src_name}' not found"
            )));
        }

        let mut copy_title = format!("{src_name} Copy");
        if existing.iter().any(|s| s.eq_ignore_ascii_case(&copy_title)) {
            let mut n = 1;
            loop {
                let cand = format!("{src_name} Copy ({n})");
                if !existing.iter().any(|s| s.eq_ignore_ascii_case(&cand)) {
                    copy_title = cand;
                    break;
                }
                n += 1;
            }
        }

        let orig = original_sheet_locked_for_overlay(&ov, &src_name)
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let src_overlay = ov.sheet_overlays.get(&src_name).cloned();

        ov.create_sheet(&copy_title, None).map_err(turbo_err_to_py)?;

        let dest_overlay = ov.sheet_overlays.entry(copy_title.clone()).or_default();
        if let Some(sheet) = orig {
            for row in sheet.rows {
                for cell in row.cells {
                    if !matches!(cell.value, CellValue::Empty) {
                        dest_overlay.modified_cells.insert((row.row, cell.col), cell.value);
                    }
                    if let Some(desc) = cell.style_desc {
                        dest_overlay.modified_styles.insert((row.row, cell.col), *desc);
                    }
                }
            }
        }
        if let Some(so) = src_overlay {
            for (coord, val) in so.modified_cells {
                dest_overlay.modified_cells.insert(coord, val);
            }
            for (coord, st) in so.modified_styles {
                dest_overlay.modified_styles.insert(coord, st);
            }
        }
        dest_overlay.is_dirty = true;

        Ok(PyEditableSheet {
            sheet_name: copy_title,
            overlay: Arc::clone(&self.overlay),
            original: OnceLock::new(),
        })
    }

    #[pyo3(signature = (worksheet_or_name, offset = 0))]
    fn move_sheet(&self, worksheet_or_name: &Bound<'_, PyAny>, offset: i64) -> PyResult<()> {
        let name = if let Ok(s) = worksheet_or_name.extract::<String>() {
            s
        } else if let Ok(ws) = worksheet_or_name.cast::<PyEditableSheet>() {
            ws.borrow().sheet_name.clone()
        } else {
            return Err(PyTypeError::new_err("move_sheet expects sheet name or EditableSheet"));
        };
        let mut ov = self
            .overlay
            .lock()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let mut names = ov.sheet_names();
        let Some(pos) = names.iter().position(|n| n == &name) else {
            return Err(pyo3::exceptions::PyKeyError::new_err(format!(
                "sheet '{name}' not found"
            )));
        };
        let len = names.len() as i64;
        let new_pos = (pos as i64 + offset).clamp(0, len - 1) as usize;
        if new_pos != pos {
            let item = names.remove(pos);
            names.insert(new_pos, item);
            ov.archive_map.sheet_names = names;
        }
        Ok(())
    }

    fn __getitem__(&self, sheet_name: &str) -> PyResult<PyEditableSheet> {
        let ov = self
            .overlay
            .lock()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let names = ov.sheet_names();
        if !names.contains(&sheet_name.to_string()) {
            return Err(pyo3::exceptions::PyKeyError::new_err(format!(
                "Sheet '{sheet_name}' not found in workbook"
            )));
        }
        Ok(PyEditableSheet {
            sheet_name: sheet_name.to_string(),
            overlay: Arc::clone(&self.overlay),
            original: OnceLock::new(),
        })
    }

    fn save(&self, py: Python<'_>, target: &Bound<'_, PyAny>) -> PyResult<()> {
        let is_path = if let Ok(path) = target.extract::<String>() {
            Some(path)
        } else if let Ok(path) = target.call_method0("__fspath__").and_then(|f| f.extract::<String>()) {
            Some(path)
        } else {
            None
        };

        if let Some(path) = is_path {
            let mut ov = self
                .overlay
                .lock()
                .map_err(|e| PyValueError::new_err(e.to_string()))?;
            let bytes = ov.save().map_err(turbo_err_to_py)?;
            std::fs::write(&path, &bytes).map_err(write_err_to_py)?;
            return Ok(());
        }

        if target.hasattr("write")? {
            if !target.hasattr("seek")? {
                return Err(PyValueError::new_err(
                    "save(): target stream must be seekable for zip central directory",
                ));
            }
            let mut ov = self
                .overlay
                .lock()
                .map_err(|e| PyValueError::new_err(e.to_string()))?;
            let bytes = ov.save().map_err(turbo_err_to_py)?;
            let py_bytes = pyo3::types::PyBytes::new(py, &bytes);
            target.call_method1("write", (py_bytes,))?;
            return Ok(());
        }

        Err(PyTypeError::new_err(
            "save() target must be a path string, PathLike, or seekable file-like object",
        ))
    }
}

#[pyfunction(name = "edit_excel")]
pub fn py_edit_excel(py: Python<'_>, path: &str) -> PyResult<PyEditableWorkbook> {
    let zip_bytes = py.detach(|| std::fs::read(path)).map_err(write_err_to_py)?;
    let archive_map = ArchiveMap::parse(Arc::new(zip_bytes)).map_err(turbo_err_to_py)?;
    let overlay = WorkbookOverlay::new(archive_map);
    Ok(PyEditableWorkbook {
        overlay: Arc::new(std::sync::Mutex::new(overlay)),
        active_idx: std::sync::atomic::AtomicUsize::new(0),
    })
}
