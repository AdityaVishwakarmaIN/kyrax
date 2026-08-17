//! Rich-text inline runs (F056). openpyxl cell/rich_text.py CellRichText.to_tree.

use super::style_engine::ColorSpec;
use super::xml::{
    needs_preserve as needs_space_preserve, push_str, write_escaped_attr, write_escaped_text,
    write_f64 as push_f64,
};

#[inline]
fn esc_attr(s: &str, out: &mut Vec<u8>) {
    write_escaped_attr(out, s);
}

#[inline]
fn esc_text(s: &str, out: &mut Vec<u8>) {
    write_escaped_text(out, s);
}

#[derive(Clone, Debug, Default)]
pub struct RunFont {
    pub r_font: Option<String>,
    pub sz: Option<f64>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<String>,
    pub strike: Option<bool>,
    pub color: Option<ColorSpec>,
    pub vert_align: Option<String>,
}

impl RunFont {
    pub fn emit_rpr(&self, out: &mut Vec<u8>) {
        push_str(out, "<rPr>");
        // InlineFont element order: rFont, charset, family, b, i, strike, outline,
        // shadow, condense, extend, color, sz, u, vertAlign, scheme
        if let Some(n) = self.r_font.as_ref() {
            push_str(out, "<rFont val=\"");
            esc_attr(n, out);
            push_str(out, "\" />");
        }
        if self.bold == Some(true) {
            push_str(out, "<b val=\"1\" />");
        }
        if self.italic == Some(true) {
            push_str(out, "<i val=\"1\" />");
        }
        if self.strike == Some(true) {
            push_str(out, "<strike val=\"1\" />");
        }
        if let Some(ref c) = self.color {
            c.emit("color", out);
        }
        if let Some(sz) = self.sz {
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
        push_str(out, "</rPr>");
    }
}

#[derive(Clone, Debug)]
pub enum RichRun {
    Text(String),
    Block { font: RunFont, text: String },
}

#[derive(Clone, Debug)]
pub struct RichText {
    pub runs: Vec<RichRun>,
}

impl RichText {
    /// Emit `<is>…</is>` body (caller wraps in cell).
    pub fn emit_is(&self, out: &mut Vec<u8>) {
        push_str(out, "<is>");
        for run in &self.runs {
            match run {
                RichRun::Text(t) => {
                    push_str(out, "<r>");
                    emit_t(t, out);
                    push_str(out, "</r>");
                }
                RichRun::Block { font, text } => {
                    push_str(out, "<r>");
                    font.emit_rpr(out);
                    emit_t(text, out);
                    push_str(out, "</r>");
                }
            }
        }
        push_str(out, "</is>");
    }
}

fn emit_t(text: &str, out: &mut Vec<u8>) {
    if needs_space_preserve(text) {
        push_str(out, "<t xml:space=\"preserve\">");
    } else {
        push_str(out, "<t>");
    }
    esc_text(text, out);
    push_str(out, "</t>");
}
