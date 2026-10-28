//! Single-pass, bounded worksheet grid reader. Unlike the Arrow table reader,
//! each cell retains its own type and row one is ordinary worksheet data.
#![cfg_attr(not(feature = "python"), allow(dead_code))]

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::sync::Arc;
use zip::HasZipMetadata;

use super::decode::decode_bytes;
use super::error::{TurboError, TurboResult};
use super::formula::{letters_to_index, translate_body};
use super::scan::{StringArena, find_lit, parse_shared_strings};
use super::stream::{
    EntryInflate, find_sheetdata_open_end, first_row_span, read_part_zip, resolve_sheet_path,
    tag_end,
};
use super::structural::{RelKind, parse_rels, parse_workbook};
use super::styles::parse_style_table;
use super::write::readback::{NumKind, classify_format};

const WINDOW: usize = 256 * 1024;
const MAX_ROW_BYTES: usize = 16 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 16 * 1024 * 1024;
const MAX_COLS: usize = 16_384;
const MAX_ROWS: usize = 1_048_576;

#[derive(Debug, Clone)]
pub(crate) enum GridValue {
    Empty,
    Integer(i64),
    BigInteger(String),
    Number(f64),
    Boolean(bool),
    String(String),
    Shared(u32),
    Serial(f64, NumKind),
    IsoDate(String),
}

/// Accept XML whitespace and either quote style. Attribute boundaries are
/// checked, so `r` cannot accidentally match the end of another attribute.
fn attr<'a>(tag: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    let mut i = 0;
    while i < tag.len() {
        if !tag[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        i += 1;
        if !tag.get(i..)?.starts_with(name) {
            continue;
        }
        let mut j = i + name.len();
        while tag.get(j).is_some_and(u8::is_ascii_whitespace) {
            j += 1;
        }
        if tag.get(j) != Some(&b'=') {
            continue;
        }
        j += 1;
        while tag.get(j).is_some_and(u8::is_ascii_whitespace) {
            j += 1;
        }
        let quote = *tag.get(j)?;
        if quote != b'\'' && quote != b'"' {
            continue;
        }
        let start = j + 1;
        let end = start + memchr::memchr(quote, &tag[start..])?;
        return Some(&tag[start..end]);
    }
    None
}

fn trim_xml(raw: &[u8]) -> &[u8] {
    raw.trim_ascii()
}

fn uint(raw: &[u8]) -> Option<usize> {
    std::str::from_utf8(raw).ok()?.parse().ok()
}
fn decoded(raw: &[u8]) -> String {
    let mut scratch = Vec::new();
    String::from_utf8_lossy(decode_bytes(raw, &mut scratch)).into_owned()
}
fn element<'a>(body: &'a [u8], name: &[u8]) -> Option<(&'a [u8], &'a [u8])> {
    let mut from = 0;
    while let Some(p) = memchr::memchr(b'<', &body[from..]) {
        let start = from + p;
        let ns = start + 1;
        if body.get(ns..).is_some_and(|x| x.starts_with(name)) {
            let after = ns + name.len();
            if body
                .get(after)
                .is_some_and(|b| b.is_ascii_whitespace() || *b == b'>' || *b == b'/')
            {
                let gt = tag_end(body, start)?;
                let tag = &body[start..=gt];
                if body.get(gt.wrapping_sub(1)) == Some(&b'/') {
                    return Some((tag, b""));
                }
                let mut close = [0u8; 32];
                let n = name.len() + 3;
                if n > close.len() {
                    return None;
                }
                close[..2].copy_from_slice(b"</");
                close[2..n - 1].copy_from_slice(name);
                close[n - 1] = b'>';
                let end = gt + 1 + find_lit(&body[gt + 1..], &close[..n])?;
                return Some((tag, &body[gt + 1..end]));
            }
        }
        from = ns;
    }
    None
}
fn iso_value(raw: &[u8]) -> GridValue {
    let text = decoded(raw);
    if let Some(duration) = text.strip_prefix("PT") {
        let mut seconds = 0.0;
        let mut from = 0;
        let mut valid = !duration.is_empty();
        for (i, unit) in duration.char_indices() {
            let factor = match unit {
                'H' => 3600.0,
                'M' => 60.0,
                'S' => 1.0,
                _ => continue,
            };
            match duration[from..i].parse::<f64>() {
                Ok(n) if n.is_finite() && n >= 0.0 => seconds += n * factor,
                _ => {
                    valid = false;
                    break;
                }
            }
            from = i + 1;
        }
        if valid && from == duration.len() {
            return GridValue::Serial(seconds / 86400.0, NumKind::Timedelta);
        }
    }
    GridValue::IsoDate(text)
}

fn rich_text(body: &[u8]) -> String {
    let mut out = String::new();
    let mut from = 0;
    while from < body.len() {
        let Some(p) = find_lit(&body[from..], b"<t") else {
            break;
        };
        let start = from + p;
        // Phonetic guides are annotations, not part of the cell's text.
        if let Some(phonetic) = find_lit(&body[from..start], b"<rPh") {
            let begin = from + phonetic;
            if let Some(end) = find_lit(&body[begin..], b"</rPh>") {
                from = begin + end + 6;
                continue;
            }
        }
        let after = start + 2;
        if !body
            .get(after)
            .is_some_and(|b| b.is_ascii_whitespace() || *b == b'>' || *b == b'/')
        {
            from = after;
            continue;
        }
        let Some(gt) = tag_end(body, start) else {
            break;
        };
        if body.get(gt.wrapping_sub(1)) == Some(&b'/') {
            from = gt + 1;
            continue;
        }
        let Some(end) = find_lit(&body[gt + 1..], b"</t>").map(|x| gt + 1 + x) else {
            break;
        };
        out.push_str(&decoded(&body[gt + 1..end]));
        from = end + 4;
    }
    out
}

pub(crate) struct GridStream {
    inflate: Option<EntryInflate>,
    pending: Vec<u8>,
    consumed: usize,
    chunk: Vec<u8>,
    pub shared: Arc<StringArena>,
    kinds: Vec<NumKind>,
    pub date1904: bool,
    data_only: bool,
    done: bool,
    next_row: usize,
    physical_row: usize,
    staged: Option<(usize, Vec<GridValue>)>,
    shared_formulas: HashMap<usize, (usize, usize, String)>,
    pub width: usize,
    pub height: usize,
    dimension_width: Option<usize>,
}

impl GridStream {
    pub fn open(path: &str, sheet_idx: usize, data_only: bool) -> TurboResult<Self> {
        let mut archive = zip::ZipArchive::new(File::open(path)?)
            .map_err(|e| TurboError::Format(format!("grid read: invalid ZIP: {e}")))?;
        let wb_xml = read_part_zip(&mut archive, "xl/workbook.xml")?
            .ok_or_else(|| TurboError::MissingPart("xl/workbook.xml".into()))?;
        let (sheets, _) = parse_workbook(&wb_xml);
        let meta = sheets.get(sheet_idx).ok_or_else(|| {
            TurboError::Format(format!(
                "sheet index {sheet_idx} out of range ({} sheets)",
                sheets.len()
            ))
        })?;
        let rels = read_part_zip(&mut archive, "xl/_rels/workbook.xml.rels")?
            .map(|x| parse_rels(&x))
            .unwrap_or_default();
        let shared = Arc::new(
            read_part_zip(&mut archive, "xl/sharedStrings.xml")?
                .map(|x| parse_shared_strings(&x))
                .unwrap_or_else(|| parse_shared_strings(b"")),
        );
        let kinds = read_part_zip(&mut archive, "xl/styles.xml")?
            .map(|x| {
                let styles = parse_style_table(&x);
                (0..styles.xfs.len())
                    .map(|i| classify_format(styles.numfmt_code(i as u32)))
                    .collect()
            })
            .unwrap_or_default();
        let chartsheet = meta
            .rid
            .as_ref()
            .and_then(|id| rels.get(id))
            .is_some_and(|rel| rel.kind == RelKind::Chartsheet);
        let inflate = if chartsheet {
            None
        } else {
            let sheet_path = resolve_sheet_path(meta, sheet_idx, &rels);
            let mut raw = File::open(path)?;
            let sheet = archive
                .by_name(&sheet_path)
                .map_err(|e| TurboError::Format(format!("grid read: {sheet_path}: {e}")))?;
            let md = sheet.get_metadata();
            let offset = md
                .data_start(&mut raw)
                .map_err(|e| TurboError::Format(format!("grid read: {e}")))?;
            Some(match md.compression_method {
                zip::CompressionMethod::Stored => {
                    EntryInflate::stored(raw, offset, md.compressed_size as usize)
                }
                zip::CompressionMethod::Deflated => {
                    EntryInflate::deflated(raw, offset, md.compressed_size as usize)
                }
                m => {
                    return Err(TurboError::Inflate(format!(
                        "unsupported compression method {m:?}"
                    )));
                }
            })
        };
        let mut this = Self {
            inflate,
            pending: Vec::new(),
            consumed: 0,
            chunk: vec![0; WINDOW],
            shared,
            kinds,
            date1904: super::meta::parse_workbook_pr(&wb_xml).0,
            data_only,
            done: chartsheet,
            next_row: 1,
            physical_row: 0,
            staged: None,
            shared_formulas: HashMap::new(),
            width: 0,
            height: 0,
            dimension_width: None,
        };
        if !chartsheet {
            this.read_header()?;
        }
        Ok(this)
    }

    fn fill(&mut self) -> TurboResult<bool> {
        if self.consumed > 0 {
            self.pending.drain(..self.consumed);
            self.consumed = 0;
        }
        let n = match self.inflate.as_mut() {
            Some(x) => x.read(&mut self.chunk)?,
            None => 0,
        };
        self.pending.extend_from_slice(&self.chunk[..n]);
        Ok(n != 0)
    }
    fn read_header(&mut self) -> TurboResult<()> {
        loop {
            if let Some(end) = find_sheetdata_open_end(&self.pending) {
                if let Some((tag, _)) = element(&self.pending[..end], b"dimension") {
                    if let Some(raw) = attr(tag, b"ref") {
                        let range = super::structural::parse_range(raw);
                        self.width = (range.c1 as usize + 1).min(MAX_COLS);
                        self.dimension_width = Some(self.width);
                        self.height = (range.r1 as usize + 1).min(MAX_ROWS);
                    }
                }
                // dimension is self closing in conforming worksheets. The
                // generic element helper deliberately also handles full tags.
                self.done = self.pending.get(end.wrapping_sub(2)) == Some(&b'/');
                self.consumed = end;
                return Ok(());
            }
            if self.pending.len() > MAX_HEADER_BYTES {
                return Err(TurboError::Format(
                    "grid read: worksheet header exceeds size cap".into(),
                ));
            }
            if !self.fill()? {
                return Err(TurboError::Format("grid read: missing sheetData".into()));
            }
        }
    }
    fn physical(&mut self) -> TurboResult<Option<(usize, Vec<GridValue>)>> {
        if self.done {
            return Ok(None);
        }
        loop {
            if let Some((start, end)) =
                first_row_span(&self.pending, self.consumed, self.pending.len())
            {
                let gt = tag_end(&self.pending, start)
                    .ok_or_else(|| TurboError::Format("grid read: truncated row".into()))?;
                let row = attr(&self.pending[start..=gt], b"r")
                    .and_then(uint)
                    .unwrap_or(self.physical_row + 1);
                if row == 0 || row > MAX_ROWS {
                    return Err(TurboError::Format(
                        "grid read: row index outside Excel limits".into(),
                    ));
                }
                if row <= self.physical_row {
                    return Err(TurboError::Format(
                        "grid read: rows must be in increasing order".into(),
                    ));
                }
                self.physical_row = row;
                let raw = &self.pending[gt + 1..end];
                let cells = parse_cells(
                    raw,
                    row,
                    self.data_only,
                    &self.kinds,
                    &mut self.shared_formulas,
                )?;
                self.width = self.width.max(cells.len());
                self.height = self.height.max(row);
                self.consumed = end;
                return Ok(Some((row, cells)));
            }
            if find_lit(&self.pending[self.consumed..], b"</sheetData>").is_some() {
                self.done = true;
                self.inflate = None;
                return Ok(None);
            }
            if self.pending.len().saturating_sub(self.consumed) > MAX_ROW_BYTES {
                return Err(TurboError::Format(
                    "grid read: a single row exceeds size cap".into(),
                ));
            }
            if !self.fill()? {
                return Err(TurboError::Format("grid read: truncated sheetData".into()));
            }
        }
    }

    /// Row bounds are inclusive, one based. Explicit column bounds are padded
    /// with None, including columns beyond the stored worksheet dimension.
    pub fn next_rows(
        &mut self,
        count: usize,
        min_row: usize,
        max_row: Option<usize>,
        min_col: usize,
        max_col: Option<usize>,
    ) -> TurboResult<VecDeque<Vec<GridValue>>> {
        let mut out = VecDeque::new();
        let mut batch_bytes = 0usize;
        let min_col = min_col.max(1);
        let min_row = min_row.max(1);
        while out.len() < count.max(1) {
            if max_row.is_some_and(|m| self.next_row > m) {
                break;
            }
            if self.staged.is_none() {
                self.staged = self.physical()?;
            }
            // A worksheet dimension is advisory; do not manufacture rows
            // after physical EOF (including an empty sheet with ref="A1").
            if self.staged.is_none() {
                break;
            }
            let mut values = match self.staged.take() {
                Some((row, cells)) if row == self.next_row => cells,
                Some((row, cells)) => {
                    self.staged = Some((row, cells));
                    Vec::new()
                }
                None => Vec::new(),
            };
            let current = self.next_row;
            self.next_row += 1;
            if current < min_row {
                continue;
            }
            let end = max_col
                .unwrap_or_else(|| {
                    self.dimension_width
                        .unwrap_or(values.len())
                        .max(values.len())
                })
                .min(MAX_COLS);
            if min_col > end {
                out.push_back(Vec::new());
                continue;
            }
            values.resize(end, GridValue::Empty);
            let selected: Vec<_> = values.drain(min_col - 1..end).collect();
            batch_bytes += selected.len() * std::mem::size_of::<GridValue>();
            batch_bytes += selected
                .iter()
                .map(|v| match v {
                    GridValue::String(s) | GridValue::BigInteger(s) | GridValue::IsoDate(s) => {
                        s.len()
                    }
                    _ => 0,
                })
                .sum::<usize>();
            out.push_back(selected);
            if batch_bytes >= 1024 * 1024 {
                break;
            }
        }
        Ok(out)
    }
}

fn parse_cells(
    raw: &[u8],
    row: usize,
    data_only: bool,
    kinds: &[NumKind],
    shared: &mut HashMap<usize, (usize, usize, String)>,
) -> TurboResult<Vec<GridValue>> {
    let mut values = Vec::new();
    let mut from = 0;
    let mut sequential_col = 0;
    while from < raw.len() {
        let Some(pos) = find_lit(&raw[from..], b"<c") else {
            break;
        };
        let start = from + pos;
        if !raw
            .get(start + 2)
            .is_some_and(|b| b.is_ascii_whitespace() || *b == b'>' || *b == b'/')
        {
            from = start + 2;
            continue;
        }
        let gt = tag_end(raw, start)
            .ok_or_else(|| TurboError::Format("grid read: truncated cell".into()))?;
        let tag = &raw[start..=gt];
        let col = attr(tag, b"r")
            .and_then(|r| {
                let letters = r.iter().take_while(|b| b.is_ascii_alphabetic()).count();
                letters_to_index(&r[..letters]).map(|x| x as usize - 1)
            })
            .unwrap_or(sequential_col);
        if col >= MAX_COLS {
            return Err(TurboError::Format(
                "grid read: column index outside Excel limits".into(),
            ));
        }
        sequential_col = col + 1;
        let (body, end) = if raw.get(gt.wrapping_sub(1)) == Some(&b'/') {
            (&b""[..], gt + 1)
        } else {
            let close = gt
                + 1
                + find_lit(&raw[gt + 1..], b"</c>")
                    .ok_or_else(|| TurboError::Format("grid read: truncated cell body".into()))?;
            (&raw[gt + 1..close], close + 4)
        };
        let formula = element(body, b"f");
        let value = if let Some((ftag, fbody)) = formula.filter(|_| !data_only) {
            let text = decoded(fbody);
            let resolved = if attr(ftag, b"t") == Some(b"shared") {
                let si = attr(ftag, b"si").and_then(uint).ok_or_else(|| {
                    TurboError::Format("grid read: shared formula has no index".into())
                })?;
                if !text.is_empty() {
                    shared.insert(si, (row, col, text.clone()));
                    text
                } else if let Some((ar, ac, text)) = shared.get(&si) {
                    translate_body(text, row as i32 - *ar as i32, col as i32 - *ac as i32)
                } else {
                    return Err(TurboError::Format(format!(
                        "grid read: shared formula {si} precedes its anchor"
                    )));
                }
            } else {
                text
            };
            GridValue::String(format!("={resolved}"))
        } else {
            let v = element(body, b"v")
                .map(|(_, x)| x)
                .filter(|x| !x.is_empty());
            match attr(tag, b"t") {
                Some(b"inlineStr") => element(body, b"is")
                    .map(|(_, x)| GridValue::String(rich_text(x)))
                    .unwrap_or(GridValue::Empty),
                Some(b"s") => v
                    .map(trim_xml)
                    .and_then(uint)
                    .and_then(|x| u32::try_from(x).ok())
                    .map(GridValue::Shared)
                    .unwrap_or(GridValue::Empty),
                Some(b"b") => v
                    .map(|x| {
                        GridValue::Boolean(
                            trim_xml(x) == b"1" || trim_xml(x).eq_ignore_ascii_case(b"true"),
                        )
                    })
                    .unwrap_or(GridValue::Empty),
                Some(b"e") | Some(b"str") => v
                    .map(|x| GridValue::String(decoded(x)))
                    .unwrap_or(GridValue::Empty),
                Some(b"d") => v.map(iso_value).unwrap_or(GridValue::Empty),
                _ => match v.filter(|x| !x.is_empty()) {
                    None => GridValue::Empty,
                    Some(raw_number) => {
                        let raw_number = trim_xml(raw_number);
                        let text = std::str::from_utf8(raw_number).map_err(|_| {
                            TurboError::Format("grid read: invalid numeric value".into())
                        })?;
                        let kind = attr(tag, b"s")
                            .and_then(uint)
                            .and_then(|s| kinds.get(s))
                            .copied()
                            .unwrap_or(NumKind::Plain);
                        let num = fast_float2::parse::<f64, _>(raw_number).map_err(|_| {
                            TurboError::Format(format!("grid read: invalid numeric value {text:?}"))
                        })?;
                        if kind != NumKind::Plain {
                            GridValue::Serial(num, kind)
                        } else if !raw_number.iter().any(|b| matches!(b, b'.' | b'e' | b'E')) {
                            text.parse::<i64>()
                                .map(GridValue::Integer)
                                .unwrap_or_else(|_| GridValue::BigInteger(text.into()))
                        } else {
                            GridValue::Number(num)
                        }
                    }
                },
            }
        };
        if values.len() <= col {
            values.resize(col + 1, GridValue::Empty);
        }
        values[col] = value;
        from = end;
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_types_do_not_depend_on_column_or_first_row() {
        let mut shared = HashMap::new();
        let values = parse_cells(br#"<c r="A1"><v>7</v></c><c r="B1" t="b"><v>1</v></c><c r="C1" t="e"><v>#DIV/0!</v></c><c r="D1" t="inlineStr"><is><r><t>A&amp;</t></r><r><t xml:space="preserve"> B</t></r></is></c><c r="F1"><v>2.5</v></c>"#,
            1, false, &[], &mut shared).unwrap();
        assert!(matches!(values[0], GridValue::Integer(7)));
        assert!(matches!(values[1], GridValue::Boolean(true)));
        assert!(matches!(&values[2], GridValue::String(s) if s == "#DIV/0!"));
        assert!(matches!(&values[3], GridValue::String(s) if s == "A& B"));
        assert!(matches!(values[4], GridValue::Empty));
        assert!(matches!(values[5], GridValue::Number(n) if n == 2.5));
    }

    #[test]
    fn attributes_accept_quotes_spacing_and_sequential_cells() {
        let values = parse_cells(
            b"<c\n t = 'b' ><v>0</v></c><c r='C2' s = '1'><v>61</v></c>",
            2,
            true,
            &[NumKind::Plain, NumKind::Date],
            &mut HashMap::new(),
        )
        .unwrap();
        assert!(matches!(values[0], GridValue::Boolean(false)));
        assert!(matches!(values[1], GridValue::Empty));
        assert!(matches!(values[2], GridValue::Serial(61., NumKind::Date)));
    }

    #[test]
    fn shared_formulas_translate_and_cached_values_keep_their_type() {
        let mut shared = HashMap::new();
        let anchor = br#"<c r="B2"><f t="shared" si="0" ref="B2:C3">A2+$A$1</f><v>4</v></c>"#;
        let next = br#"<c r="C3" t="b"><f t="shared" si="0"/><v>1</v></c>"#;
        let a = parse_cells(anchor, 2, false, &[], &mut shared).unwrap();
        let b = parse_cells(next, 3, false, &[], &mut shared).unwrap();
        assert!(matches!(&a[1], GridValue::String(s) if s == "=A2+$A$1"));
        assert!(matches!(&b[2], GridValue::String(s) if s == "=B3+$A$1"));
        let cached = parse_cells(next, 3, true, &[], &mut shared).unwrap();
        assert!(matches!(cached[2], GridValue::Boolean(true)));
    }

    #[test]
    fn inline_phonetics_are_not_values() {
        assert_eq!(
            rich_text(r#"<r><t>漢字</t></r><rPh sb="0" eb="2"><t>かんじ</t></rPh>"#.as_bytes()),
            "漢字"
        );
    }

    #[test]
    fn empty_missing_values_and_large_integers() {
        let values = parse_cells(br#"<c r="A1"/><c r="B1"><v/></c><c r="C1"><v>18446744073709551616</v></c><c r="D1" t="inlineStr"><is><t/></is></c>"#,
            1, false, &[], &mut HashMap::new()).unwrap();
        assert!(matches!(values[0], GridValue::Empty));
        assert!(matches!(values[1], GridValue::Empty));
        assert!(matches!(&values[2], GridValue::BigInteger(s) if s == "18446744073709551616"));
        assert!(matches!(&values[3], GridValue::String(s) if s.is_empty()));
    }
}
