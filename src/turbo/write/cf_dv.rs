//! Conditional formatting (F070) + data validations (F071).
//! CF dxf registration: StyleEngine::register_dxf → dxfId on cfRule.

use super::style_engine::{ColorSpec, DxfDesc, StyleEngine};
use super::xml::{
    push_str, write_escaped_attr, write_escaped_text, write_f64 as push_f64, write_u32 as push_u32,
};

#[inline]
fn esc_attr(s: &str, out: &mut Vec<u8>) {
    write_escaped_attr(out, s);
}

#[inline]
fn esc_text(s: &str, out: &mut Vec<u8>) {
    write_escaped_text(out, s);
}

// ---------------------------------------------------------------------------
// Conditional formatting
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct CfVo {
    pub type_: String, // min max num percent formula percentile
    pub val: Option<String>,
}

#[derive(Clone, Debug)]
pub enum CfRuleKind {
    ColorScale {
        cfvos: Vec<CfVo>,
        colors: Vec<ColorSpec>,
    },
    DataBar {
        cfvos: Vec<CfVo>,
        color: ColorSpec,
        show_value: Option<bool>,
        min_length: Option<u32>,
        max_length: Option<u32>,
    },
    IconSet {
        icon_set: String,
        cfvos: Vec<CfVo>,
        show_value: Option<bool>,
        reverse: Option<bool>,
        custom: Option<bool>,
        percent: Option<bool>,
    },
    CellIs {
        operator: String,
        formulas: Vec<String>,
        dxf: DxfDesc,
        stop_if_true: Option<bool>,
    },
    Expression {
        formulas: Vec<String>,
        dxf: DxfDesc,
        stop_if_true: Option<bool>,
    },
    Top10 {
        rank: u32,
        percent: Option<bool>,
        bottom: Option<bool>,
        dxf: DxfDesc,
        stop_if_true: Option<bool>,
    },
    AboveAverage {
        above_average: Option<bool>,
        equal_average: Option<bool>,
        std_dev: Option<i32>,
        dxf: DxfDesc,
        stop_if_true: Option<bool>,
    },
    UniqueValues {
        dxf: DxfDesc,
        stop_if_true: Option<bool>,
    },
    DuplicateValues {
        dxf: DxfDesc,
        stop_if_true: Option<bool>,
    },
    ContainsText {
        text: String,
        operator: Option<String>,
        formulas: Vec<String>,
        dxf: DxfDesc,
        stop_if_true: Option<bool>,
    },
    NotContainsText {
        text: String,
        operator: Option<String>,
        formulas: Vec<String>,
        dxf: DxfDesc,
        stop_if_true: Option<bool>,
    },
    BeginsWith {
        text: String,
        operator: Option<String>,
        formulas: Vec<String>,
        dxf: DxfDesc,
        stop_if_true: Option<bool>,
    },
    EndsWith {
        text: String,
        operator: Option<String>,
        formulas: Vec<String>,
        dxf: DxfDesc,
        stop_if_true: Option<bool>,
    },
    ContainsBlanks {
        formulas: Vec<String>,
        dxf: DxfDesc,
        stop_if_true: Option<bool>,
    },
    NotContainsBlanks {
        formulas: Vec<String>,
        dxf: DxfDesc,
        stop_if_true: Option<bool>,
    },
    ContainsErrors {
        formulas: Vec<String>,
        dxf: DxfDesc,
        stop_if_true: Option<bool>,
    },
    NotContainsErrors {
        formulas: Vec<String>,
        dxf: DxfDesc,
        stop_if_true: Option<bool>,
    },
    TimePeriod {
        time_period: String,
        formulas: Vec<String>,
        dxf: DxfDesc,
        stop_if_true: Option<bool>,
    },
}

#[derive(Clone, Debug)]
pub struct CfRule {
    pub kind: CfRuleKind,
    pub priority: u32,
    /// Filled at emit time via StyleEngine.
    pub dxf_id: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct ConditionalFormatting {
    pub sqref: String,
    pub rules: Vec<CfRule>,
}

impl ConditionalFormatting {
    /// Register dxfs into engine; set dxf_id on rules that need it.
    pub fn register_dxfs(&mut self, eng: &mut StyleEngine) {
        for rule in &mut self.rules {
            let dxf = match &rule.kind {
                CfRuleKind::CellIs { dxf, .. }
                | CfRuleKind::Expression { dxf, .. }
                | CfRuleKind::Top10 { dxf, .. }
                | CfRuleKind::AboveAverage { dxf, .. }
                | CfRuleKind::UniqueValues { dxf, .. }
                | CfRuleKind::DuplicateValues { dxf, .. }
                | CfRuleKind::ContainsText { dxf, .. }
                | CfRuleKind::NotContainsText { dxf, .. }
                | CfRuleKind::BeginsWith { dxf, .. }
                | CfRuleKind::EndsWith { dxf, .. }
                | CfRuleKind::ContainsBlanks { dxf, .. }
                | CfRuleKind::NotContainsBlanks { dxf, .. }
                | CfRuleKind::ContainsErrors { dxf, .. }
                | CfRuleKind::NotContainsErrors { dxf, .. }
                | CfRuleKind::TimePeriod { dxf, .. } => Some(dxf.clone()),
                _ => None,
            };
            if let Some(d) = dxf {
                rule.dxf_id = eng.register_dxf(d);
            }
        }
    }

    pub fn emit(&self, out: &mut Vec<u8>) {
        push_str(out, "<conditionalFormatting sqref=\"");
        esc_attr(&self.sqref, out);
        push_str(out, "\">");
        for rule in &self.rules {
            emit_rule(rule, out);
        }
        push_str(out, "</conditionalFormatting>");
    }
}

fn emit_common_rule_attrs(rule: &CfRule, stop_if_true: Option<bool>, out: &mut Vec<u8>) {
    if let Some(id) = rule.dxf_id {
        push_str(out, " dxfId=\"");
        push_u32(out, id);
        out.push(b'"');
    }
    if let Some(s) = stop_if_true {
        push_str(out, " stopIfTrue=\"");
        out.push(if s { b'1' } else { b'0' });
        out.push(b'"');
    }
}

fn emit_rule(rule: &CfRule, out: &mut Vec<u8>) {
    match &rule.kind {
        CfRuleKind::ColorScale { cfvos, colors } => {
            push_str(out, "<cfRule type=\"colorScale\" priority=\"");
            push_u32(out, rule.priority);
            push_str(out, "\"><colorScale>");
            for v in cfvos {
                emit_cfvo(v, out);
            }
            for c in colors {
                c.emit("color", out);
            }
            push_str(out, "</colorScale></cfRule>");
        }
        CfRuleKind::DataBar {
            cfvos,
            color,
            show_value,
            min_length,
            max_length,
        } => {
            push_str(out, "<cfRule type=\"dataBar\" priority=\"");
            push_u32(out, rule.priority);
            push_str(out, "\"><dataBar");
            if let Some(sv) = show_value {
                push_str(out, " showValue=\"");
                out.push(if *sv { b'1' } else { b'0' });
                out.push(b'"');
            }
            if let Some(ml) = min_length {
                push_str(out, " minLength=\"");
                push_u32(out, *ml);
                out.push(b'"');
            }
            if let Some(ml) = max_length {
                push_str(out, " maxLength=\"");
                push_u32(out, *ml);
                out.push(b'"');
            }
            push_str(out, ">");
            for v in cfvos {
                emit_cfvo(v, out);
            }
            color.emit("color", out);
            push_str(out, "</dataBar></cfRule>");
        }
        CfRuleKind::IconSet {
            icon_set,
            cfvos,
            show_value,
            reverse,
            custom,
            percent,
        } => {
            push_str(out, "<cfRule type=\"iconSet\" priority=\"");
            push_u32(out, rule.priority);
            if let Some(p) = percent {
                push_str(out, "\" percent=\"");
                out.push(if *p { b'1' } else { b'0' });
            }
            push_str(out, "\"><iconSet iconSet=\"");
            esc_attr(icon_set, out);
            out.push(b'"');
            if let Some(sv) = show_value {
                push_str(out, " showValue=\"");
                out.push(if *sv { b'1' } else { b'0' });
                out.push(b'"');
            }
            if let Some(r) = reverse {
                push_str(out, " reverse=\"");
                out.push(if *r { b'1' } else { b'0' });
                out.push(b'"');
            }
            if let Some(c) = custom {
                push_str(out, " custom=\"");
                out.push(if *c { b'1' } else { b'0' });
                out.push(b'"');
            }
            push_str(out, ">");
            for v in cfvos {
                emit_cfvo(v, out);
            }
            push_str(out, "</iconSet></cfRule>");
        }
        CfRuleKind::CellIs {
            operator,
            formulas,
            stop_if_true,
            ..
        } => {
            push_str(out, "<cfRule type=\"cellIs\" priority=\"");
            push_u32(out, rule.priority);
            push_str(out, "\" operator=\"");
            esc_attr(operator, out);
            out.push(b'"');
            emit_common_rule_attrs(rule, *stop_if_true, out);
            push_str(out, ">");
            for f in formulas {
                push_str(out, "<formula>");
                esc_text(f, out);
                push_str(out, "</formula>");
            }
            push_str(out, "</cfRule>");
        }
        CfRuleKind::Expression {
            formulas,
            stop_if_true,
            ..
        } => {
            push_str(out, "<cfRule type=\"expression\" priority=\"");
            push_u32(out, rule.priority);
            out.push(b'"');
            emit_common_rule_attrs(rule, *stop_if_true, out);
            push_str(out, ">");
            for f in formulas {
                push_str(out, "<formula>");
                esc_text(f, out);
                push_str(out, "</formula>");
            }
            push_str(out, "</cfRule>");
        }
        CfRuleKind::Top10 {
            rank,
            percent,
            bottom,
            stop_if_true,
            ..
        } => {
            push_str(out, "<cfRule type=\"top10\" priority=\"");
            push_u32(out, rule.priority);
            push_str(out, "\" rank=\"");
            push_u32(out, *rank);
            out.push(b'"');
            if let Some(p) = percent {
                push_str(out, " percent=\"");
                out.push(if *p { b'1' } else { b'0' });
                out.push(b'"');
            }
            if let Some(b) = bottom {
                push_str(out, " bottom=\"");
                out.push(if *b { b'1' } else { b'0' });
                out.push(b'"');
            }
            emit_common_rule_attrs(rule, *stop_if_true, out);
            push_str(out, " />");
        }
        CfRuleKind::AboveAverage {
            above_average,
            equal_average,
            std_dev,
            stop_if_true,
            ..
        } => {
            push_str(out, "<cfRule type=\"aboveAverage\" priority=\"");
            push_u32(out, rule.priority);
            out.push(b'"');
            if let Some(aa) = above_average {
                push_str(out, " aboveAverage=\"");
                out.push(if *aa { b'1' } else { b'0' });
                out.push(b'"');
            }
            if let Some(ea) = equal_average {
                push_str(out, " equalAverage=\"");
                out.push(if *ea { b'1' } else { b'0' });
                out.push(b'"');
            }
            if let Some(sd) = std_dev {
                push_str(out, " stdDev=\"");
                push_str(out, &sd.to_string());
                out.push(b'"');
            }
            emit_common_rule_attrs(rule, *stop_if_true, out);
            push_str(out, " />");
        }
        CfRuleKind::UniqueValues { stop_if_true, .. } => {
            push_str(out, "<cfRule type=\"uniqueValues\" priority=\"");
            push_u32(out, rule.priority);
            out.push(b'"');
            emit_common_rule_attrs(rule, *stop_if_true, out);
            push_str(out, " />");
        }
        CfRuleKind::DuplicateValues { stop_if_true, .. } => {
            push_str(out, "<cfRule type=\"duplicateValues\" priority=\"");
            push_u32(out, rule.priority);
            out.push(b'"');
            emit_common_rule_attrs(rule, *stop_if_true, out);
            push_str(out, " />");
        }
        CfRuleKind::ContainsText {
            text,
            operator,
            formulas,
            stop_if_true,
            ..
        } => {
            push_str(out, "<cfRule type=\"containsText\" priority=\"");
            push_u32(out, rule.priority);
            push_str(out, "\" text=\"");
            esc_attr(text, out);
            out.push(b'"');
            if let Some(op) = operator {
                push_str(out, " operator=\"");
                esc_attr(op, out);
                out.push(b'"');
            }
            emit_common_rule_attrs(rule, *stop_if_true, out);
            push_str(out, ">");
            for f in formulas {
                push_str(out, "<formula>");
                esc_text(f, out);
                push_str(out, "</formula>");
            }
            push_str(out, "</cfRule>");
        }
        CfRuleKind::NotContainsText {
            text,
            operator,
            formulas,
            stop_if_true,
            ..
        } => {
            push_str(out, "<cfRule type=\"notContainsText\" priority=\"");
            push_u32(out, rule.priority);
            push_str(out, "\" text=\"");
            esc_attr(text, out);
            out.push(b'"');
            if let Some(op) = operator {
                push_str(out, " operator=\"");
                esc_attr(op, out);
                out.push(b'"');
            }
            emit_common_rule_attrs(rule, *stop_if_true, out);
            push_str(out, ">");
            for f in formulas {
                push_str(out, "<formula>");
                esc_text(f, out);
                push_str(out, "</formula>");
            }
            push_str(out, "</cfRule>");
        }
        CfRuleKind::BeginsWith {
            text,
            operator,
            formulas,
            stop_if_true,
            ..
        } => {
            push_str(out, "<cfRule type=\"beginsWith\" priority=\"");
            push_u32(out, rule.priority);
            push_str(out, "\" text=\"");
            esc_attr(text, out);
            out.push(b'"');
            if let Some(op) = operator {
                push_str(out, " operator=\"");
                esc_attr(op, out);
                out.push(b'"');
            }
            emit_common_rule_attrs(rule, *stop_if_true, out);
            push_str(out, ">");
            for f in formulas {
                push_str(out, "<formula>");
                esc_text(f, out);
                push_str(out, "</formula>");
            }
            push_str(out, "</cfRule>");
        }
        CfRuleKind::EndsWith {
            text,
            operator,
            formulas,
            stop_if_true,
            ..
        } => {
            push_str(out, "<cfRule type=\"endsWith\" priority=\"");
            push_u32(out, rule.priority);
            push_str(out, "\" text=\"");
            esc_attr(text, out);
            out.push(b'"');
            if let Some(op) = operator {
                push_str(out, " operator=\"");
                esc_attr(op, out);
                out.push(b'"');
            }
            emit_common_rule_attrs(rule, *stop_if_true, out);
            push_str(out, ">");
            for f in formulas {
                push_str(out, "<formula>");
                esc_text(f, out);
                push_str(out, "</formula>");
            }
            push_str(out, "</cfRule>");
        }
        CfRuleKind::ContainsBlanks {
            formulas,
            stop_if_true,
            ..
        } => {
            push_str(out, "<cfRule type=\"containsBlanks\" priority=\"");
            push_u32(out, rule.priority);
            out.push(b'"');
            emit_common_rule_attrs(rule, *stop_if_true, out);
            if formulas.is_empty() {
                push_str(out, " />");
            } else {
                push_str(out, ">");
                for f in formulas {
                    push_str(out, "<formula>");
                    esc_text(f, out);
                    push_str(out, "</formula>");
                }
                push_str(out, "</cfRule>");
            }
        }
        CfRuleKind::NotContainsBlanks {
            formulas,
            stop_if_true,
            ..
        } => {
            push_str(out, "<cfRule type=\"notContainsBlanks\" priority=\"");
            push_u32(out, rule.priority);
            out.push(b'"');
            emit_common_rule_attrs(rule, *stop_if_true, out);
            if formulas.is_empty() {
                push_str(out, " />");
            } else {
                push_str(out, ">");
                for f in formulas {
                    push_str(out, "<formula>");
                    esc_text(f, out);
                    push_str(out, "</formula>");
                }
                push_str(out, "</cfRule>");
            }
        }
        CfRuleKind::ContainsErrors {
            formulas,
            stop_if_true,
            ..
        } => {
            push_str(out, "<cfRule type=\"containsErrors\" priority=\"");
            push_u32(out, rule.priority);
            out.push(b'"');
            emit_common_rule_attrs(rule, *stop_if_true, out);
            if formulas.is_empty() {
                push_str(out, " />");
            } else {
                push_str(out, ">");
                for f in formulas {
                    push_str(out, "<formula>");
                    esc_text(f, out);
                    push_str(out, "</formula>");
                }
                push_str(out, "</cfRule>");
            }
        }
        CfRuleKind::NotContainsErrors {
            formulas,
            stop_if_true,
            ..
        } => {
            push_str(out, "<cfRule type=\"notContainsErrors\" priority=\"");
            push_u32(out, rule.priority);
            out.push(b'"');
            emit_common_rule_attrs(rule, *stop_if_true, out);
            if formulas.is_empty() {
                push_str(out, " />");
            } else {
                push_str(out, ">");
                for f in formulas {
                    push_str(out, "<formula>");
                    esc_text(f, out);
                    push_str(out, "</formula>");
                }
                push_str(out, "</cfRule>");
            }
        }
        CfRuleKind::TimePeriod {
            time_period,
            formulas,
            stop_if_true,
            ..
        } => {
            push_str(out, "<cfRule type=\"timePeriod\" priority=\"");
            push_u32(out, rule.priority);
            push_str(out, "\" timePeriod=\"");
            esc_attr(time_period, out);
            out.push(b'"');
            emit_common_rule_attrs(rule, *stop_if_true, out);
            if formulas.is_empty() {
                push_str(out, " />");
            } else {
                push_str(out, ">");
                for f in formulas {
                    push_str(out, "<formula>");
                    esc_text(f, out);
                    push_str(out, "</formula>");
                }
                push_str(out, "</cfRule>");
            }
        }
    }
}

fn emit_cfvo(v: &CfVo, out: &mut Vec<u8>) {
    push_str(out, "<cfvo type=\"");
    esc_attr(&v.type_, out);
    out.push(b'"');
    if let Some(ref val) = v.val {
        push_str(out, " val=\"");
        esc_attr(val, out);
        out.push(b'"');
    }
    push_str(out, " />");
}

// ---------------------------------------------------------------------------
// Data validations
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct DataValidation {
    pub type_: Option<String>,
    pub operator: Option<String>,
    pub formula1: Option<String>,
    pub formula2: Option<String>,
    pub sqref: String,
    pub allow_blank: bool,
    pub show_error_message: bool,
    pub show_input_message: bool,
    pub show_drop_down: bool, // openpyxl: showDropDown True means HIDE dropdown (Excel inverted)
    pub error_title: Option<String>,
    pub error: Option<String>,
    pub prompt_title: Option<String>,
    pub prompt: Option<String>,
}

impl DataValidation {
    pub fn emit(&self, out: &mut Vec<u8>) {
        push_str(out, "<dataValidation sqref=\"");
        esc_attr(&self.sqref, out);
        out.push(b'"');
        push_str(out, " showDropDown=\"");
        out.push(if self.show_drop_down { b'1' } else { b'0' });
        push_str(out, "\" showInputMessage=\"");
        out.push(if self.show_input_message { b'1' } else { b'0' });
        push_str(out, "\" showErrorMessage=\"");
        out.push(if self.show_error_message { b'1' } else { b'0' });
        push_str(out, "\" allowBlank=\"");
        out.push(if self.allow_blank { b'1' } else { b'0' });
        out.push(b'"');
        if let Some(ref t) = self.type_ {
            push_str(out, " type=\"");
            esc_attr(t, out);
            out.push(b'"');
        }
        if let Some(ref op) = self.operator {
            push_str(out, " operator=\"");
            esc_attr(op, out);
            out.push(b'"');
        }
        if let Some(ref e) = self.error_title {
            push_str(out, " errorTitle=\"");
            esc_attr(e, out);
            out.push(b'"');
        }
        if let Some(ref e) = self.error {
            push_str(out, " error=\"");
            esc_attr(e, out);
            out.push(b'"');
        }
        if let Some(ref p) = self.prompt_title {
            push_str(out, " promptTitle=\"");
            esc_attr(p, out);
            out.push(b'"');
        }
        if let Some(ref p) = self.prompt {
            push_str(out, " prompt=\"");
            esc_attr(p, out);
            out.push(b'"');
        }
        push_str(out, ">");
        if let Some(ref f) = self.formula1 {
            push_str(out, "<formula1>");
            esc_text(f, out);
            push_str(out, "</formula1>");
        }
        if let Some(ref f) = self.formula2 {
            push_str(out, "<formula2>");
            esc_text(f, out);
            push_str(out, "</formula2>");
        }
        push_str(out, "</dataValidation>");
    }
}

pub fn emit_data_validations(dvs: &[DataValidation], out: &mut Vec<u8>) {
    let active: Vec<_> = dvs.iter().filter(|d| !d.sqref.is_empty()).collect();
    if active.is_empty() {
        return;
    }
    push_str(out, "<dataValidations count=\"");
    push_u32(out, active.len() as u32);
    push_str(out, "\">");
    for d in active {
        d.emit(out);
    }
    push_str(out, "</dataValidations>");
}

// silence unused
#[allow(dead_code)]
fn _pf(v: f64, o: &mut Vec<u8>) {
    push_f64(o, v);
}
