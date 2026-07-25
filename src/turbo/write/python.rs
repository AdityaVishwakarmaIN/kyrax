//! PyO3 bindings for the turbo write path.

use pyo3::{
    Bound, PyAny, PyResult, Python,
    exceptions::PyValueError,
    prelude::*,
    types::{PyDict, PyList, PyTuple},
};
use std::sync::Arc;

use super::cf_dv::{CfRule, CfRuleKind, CfVo, ConditionalFormatting, DataValidation};
use super::charts::{Anchor, Chart, ChartType, ChartsheetSpec, Series};
use super::model::*;
use super::rich_text::{RichRun, RichText, RunFont};
use super::style_engine::{
    AlignDesc, BorderDesc, ColorSpec, DxfDesc, FillDesc, FontDesc, ProtDesc, SideDesc, StyleDesc,
};
use super::writer::{
    date_to_serial, datetime_to_serial, save_workbook, save_workbook_stream, write_workbook_bytes,
};
use crate::error::{KyraxError, KyraxErrorKind};
use crate::turbo::error::TurboError;
use crate::turbo::overlay::WorkbookOverlay;
use crate::turbo::zipmin::ArchiveMap;

fn write_err_to_py(err: std::io::Error) -> PyErr {
    let fe: KyraxError = KyraxErrorKind::Internal(format!("write error: {err}")).into();
    fe.into()
}

fn turbo_err_to_py(err: TurboError) -> PyErr {
    let fe: KyraxError = KyraxErrorKind::Internal(err.to_string()).into();
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

fn parse_color(obj: &Bound<'_, PyAny>) -> PyResult<ColorSpec> {
    if let Ok(s) = obj.extract::<String>() {
        if let Some(rest) = s.strip_prefix("theme:") {
            let t: u32 = rest.parse().unwrap_or(0);
            return Ok(ColorSpec::Theme(t));
        }
        return Ok(ColorSpec::from_rgb_hex(&s));
    }
    if let Ok(d) = obj.cast::<PyDict>() {
        if let Some(rgb) = d.get_item("rgb")? {
            return Ok(ColorSpec::from_rgb_hex(&rgb.extract::<String>()?));
        }
        if let Some(t) = d.get_item("theme")? {
            return Ok(ColorSpec::Theme(t.extract()?));
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
    let mut rf = RunFont::default();
    rf.r_font = opt_str(d, "rFont")?
        .or(opt_str(d, "r_font")?)
        .or(opt_str(d, "name")?);
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
    if let Ok(s) = obj.extract::<String>() {
        if s.starts_with('#') && is_excel_error(&s) {
            return Ok(CellValue::Error(s));
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
            if let Some(f) = style_flag.as_deref_mut() {
                *f = true;
            }
            return Ok(CellValue::DateSerial(date_to_serial(y, m, d)));
        }
    }
    // fallback str
    if let Ok(s) = obj.str().map(|s| s.to_string()) {
        return Ok(CellValue::Str(s));
    }
    Err(PyValueError::new_err(format!(
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
    let nrows = col_data[0].len()?;
    for c in &col_data {
        if c.len()? != nrows {
            return Err(PyValueError::new_err(
                "all columns must have the same length",
            ));
        }
    }
    let mut style_work = false;
    sheet.rows.reserve(nrows);
    for r in 0..nrows {
        let mut row = Row::new((r as u32) + 1);
        row.cells.reserve(ncols);
        for (ci, col) in col_data.iter().enumerate() {
            let cell_obj = col.get_item(r)?;
            let val = py_to_cell_value_flagged(&cell_obj, date1904, Some(&mut style_work))?;
            match &val {
                CellValue::Empty => {}
                _ => {
                    row.cells.push(Cell::new((ci as u32) + 1, val));
                }
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
            match &val {
                CellValue::Empty => {}
                _ => {
                    row.cells.push(Cell::new((ci as u32) + 1, val));
                }
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
        if !cols.is_none() {
            if columns_to_sheet(&mut sheet, &cols, opts.date1904)? {
                style_work = true;
            }
        }
    }

    // rows as list of lists (row-major alternative)
    if let Some(rows) = d.get_item("rows")? {
        if !rows.is_none() {
            let row_list = rows
                .cast::<PyList>()
                .map_err(|_| PyValueError::new_err("rows must be a list of row sequences"))?;
            for (ri, row_obj) in row_list.iter().enumerate() {
                let cells = row_obj
                    .cast::<PyList>()
                    .map_err(|_| PyValueError::new_err("each row must be a list of cell values"))?;
                let mut row = Row::new((ri as u32) + 1);
                for (ci, cell) in cells.iter().enumerate() {
                    let val =
                        py_to_cell_value_flagged(&cell, opts.date1904, Some(&mut style_work))?;
                    match &val {
                        CellValue::Empty => {}
                        _ => row.cells.push(Cell::new((ci as u32) + 1, val)),
                    }
                }
                if !row.cells.is_empty() {
                    sheet.rows.push(row);
                }
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

fn apply_structural_sheet(sheet: &mut Sheet, d: &Bound<'_, PyDict>) -> PyResult<()> {
    if let Some(tc) = d.get_item("tab_color")?.or(d.get_item("tab_color_rgb")?) {
        if !tc.is_none() {
            sheet.tab_color_rgb = Some(tc.extract()?);
        }
    }
    if let Some(af) = d.get_item("auto_filter")? {
        if !af.is_none() {
            sheet.auto_filter = Some(af.extract()?);
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

fn parse_series(obj: &Bound<'_, PyAny>) -> PyResult<Series> {
    let d = obj
        .cast::<PyDict>()
        .map_err(|_| PyValueError::new_err("series must be a dict"))?;
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
    })
}

fn parse_anchor(obj: &Bound<'_, PyAny>) -> PyResult<Anchor> {
    if let Ok(s) = obj.extract::<String>() {
        return Ok(Anchor::OneCell {
            cell: s,
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
            to_cell: d
                .get_item("to")?
                .or(d.get_item("to_cell")?)
                .ok_or_else(|| PyValueError::new_err("twoCell needs to"))?
                .extract()?,
        }),
        "absolute" => Ok(Anchor::Absolute {
            x_emu: d
                .get_item("x")?
                .map(|v| v.extract())
                .transpose()?
                .unwrap_or(0),
            y_emu: d
                .get_item("y")?
                .map(|v| v.extract())
                .transpose()?
                .unwrap_or(0),
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
        out.push(Chart {
            chart_type,
            title,
            series,
            anchor,
            style,
            legend_pos,
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
                        strides.is_none() || strides.as_ref().map_or(false, |s| s.is_none());
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

fn build_workbook_from_py(
    sheets: &Bound<'_, PyAny>,
    string_mode: &str,
    emit_cached_values: bool,
    date1904: bool,
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
) -> PyResult<Workbook> {
    let mut opts = WriteOptions {
        string_mode: parse_string_mode(string_mode)?,
        emit_cached_values,
        date1904,
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

    if sheet_list.len() == 1 {
        let first_sheet = &sheet_list.get_item(0)?;
        if let Ok(dict) = first_sheet.cast::<PyDict>() {
            let name = opt_str(dict, "name")?.unwrap_or_else(|| "Sheet1".to_string());
            let has_grid = dict.contains("grid")?;
            let has_cols = dict.contains("columns")? || dict.contains("data")?;
            let has_rows = dict.contains("rows")?;

            if has_grid {
                if has_cols || has_rows {
                    return Err(PyValueError::new_err(
                        "'grid' key is mutually exclusive with 'columns' and 'rows'",
                    ));
                }

                // Check eligibility: no styles, merges, CF, DV, freeze_panes attached
                let is_eligible = !dict.contains("cell_styles")?
                    && !dict.contains("style_palette")?
                    && !dict.contains("conditional_formatting")?
                    && !dict.contains("data_validations")?
                    && !dict.contains("merged_cells")?
                    && !dict.contains("freeze_panes")?;

                if is_eligible {
                    if let Some(grid_obj) = dict.get_item("grid")? {
                        if let Ok(Some(grid)) = try_parse_numeric_grid(name, &grid_obj) {
                            wb.numeric_columns = Some(grid);
                        }
                    }
                }
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
))]
pub fn py_write_excel_turbo(
    py: Python<'_>,
    path: &str,
    sheets: &Bound<'_, PyAny>,
    string_mode: &str,
    emit_cached_values: bool,
    date1904: bool,
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
) -> PyResult<()> {
    let wb = build_workbook_from_py(
        sheets,
        string_mode,
        emit_cached_values,
        date1904,
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
))]
pub fn py_write_excel_turbo_stream(
    py: Python<'_>,
    path: &str,
    sheets: &Bound<'_, PyAny>,
    string_mode: &str,
    emit_cached_values: bool,
    date1904: bool,
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
) -> PyResult<()> {
    let wb = build_workbook_from_py(
        sheets,
        string_mode,
        emit_cached_values,
        date1904,
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
))]
pub fn py_write_excel_turbo_bytes<'py>(
    py: Python<'py>,
    sheets: &Bound<'_, PyAny>,
    string_mode: &str,
    emit_cached_values: bool,
    date1904: bool,
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
) -> PyResult<Bound<'py, pyo3::types::PyBytes>> {
    let wb = build_workbook_from_py(
        sheets,
        string_mode,
        emit_cached_values,
        date1904,
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

#[pyclass(name = "EditableSheet")]
pub struct PyEditableSheet {
    sheet_name: String,
    overlay: Arc<std::sync::Mutex<WorkbookOverlay>>,
}

#[pymethods]
impl PyEditableSheet {
    #[pyo3(name = "set_cell")]
    fn py_set_cell(&self, row: u32, col: u32, value: &Bound<'_, PyAny>) -> PyResult<()> {
        let cell_val = py_to_cell_value(value, false)?;
        let mut ov = self
            .overlay
            .lock()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        ov.set_cell(&self.sheet_name, row, col, cell_val);
        Ok(())
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
}

#[pyclass(name = "EditableWorkbook")]
pub struct PyEditableWorkbook {
    overlay: Arc<std::sync::Mutex<WorkbookOverlay>>,
}

#[pymethods]
impl PyEditableWorkbook {
    fn __getitem__(&self, sheet_name: &str) -> PyResult<PyEditableSheet> {
        let ov = self
            .overlay
            .lock()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        if !ov.archive_map.sheet_name_map.contains_key(sheet_name)
            && !ov.sheet_overlays.contains_key(sheet_name)
        {
            return Err(pyo3::exceptions::PyKeyError::new_err(format!(
                "Sheet '{sheet_name}' not found in workbook"
            )));
        }
        Ok(PyEditableSheet {
            sheet_name: sheet_name.to_string(),
            overlay: Arc::clone(&self.overlay),
        })
    }

    fn save(&self, path: &str) -> PyResult<()> {
        let ov = self
            .overlay
            .lock()
            .map_err(|e| PyValueError::new_err(e.to_string()))?;
        let bytes = ov.save().map_err(turbo_err_to_py)?;
        std::fs::write(path, bytes).map_err(write_err_to_py)?;
        Ok(())
    }
}

#[pyfunction(name = "edit_excel")]
pub fn py_edit_excel(py: Python<'_>, path: &str) -> PyResult<PyEditableWorkbook> {
    let zip_bytes = py.detach(|| std::fs::read(path)).map_err(write_err_to_py)?;
    let archive_map = ArchiveMap::parse(Arc::new(zip_bytes)).map_err(turbo_err_to_py)?;
    let overlay = WorkbookOverlay::new(archive_map);
    Ok(PyEditableWorkbook {
        overlay: Arc::new(std::sync::Mutex::new(overlay)),
    })
}
