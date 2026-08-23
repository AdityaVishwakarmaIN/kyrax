//! PIVOT AUTHORING (Task B5b) — the write half of the pivot engine.
//!
//! Given a worksheet, a source range and a field layout, this module emits the
//! four-part pivot that Excel opens without a repair prompt:
//!
//!   1. `xl/pivotCache/pivotCacheDefinitionN.xml`  — the cache: `cacheSource`
//!      (the worksheet range) + one `cacheField` per source column carrying
//!      `sharedItems` with honest type flags (`containsNumber`,
//!      `containsString`, `containsBlank`, `containsDate`, `containsInteger`,
//!      `minValue`/`maxValue`) and the distinct values.
//!   2. `xl/pivotCache/pivotCacheRecordsN.xml`     — one `<r>` per source row,
//!      each field referencing a sharedItems index (`<s v="i"/>`) or carrying
//!      an inline numeric value (`<n v="..."/>`).
//!   3. `xl/pivotTables/pivotTableN.xml`           — the layout: `location`,
//!      `pivotFields` (axis + items per cache field), `rowFields` /
//!      `colFields` / `dataFields`, and `rowItems` / `colItems`.
//!   4. RELS + content types: the workbook `<pivotCaches>` / workbook rels, the
//!      worksheet rel → pivot table, the cache-def rel → records, and a
//!      `[Content_Types].xml` Override for every new part.
//!
//! Every index emitted here is a 0-based cache-field index (the `x`/`fld`
//! attributes are positions among the source columns, exactly as the READ path
//! `parse_pivot_table` / `parse_pivot_cache` in `structural.rs` interprets
//! them), so an authored pivot round-trips through the existing reader.
//!
//! Determinism is a hard guarantee: field order follows the layout the caller
//! passed in, distinct values follow first-appearance order in the source, the
//! row/col item tuples follow first-appearance order, and parts are emitted in
//! a fixed order. No HashMap iteration reaches the emitted XML.

use crate::turbo::write::model::{CachedValue, CellValue, Sheet};
use crate::turbo::write::xml::{dimension_ref, write_escaped_attr, write_i32, write_u32};
use crate::turbo::write::{RichRun, RichText};

const SHEET_NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const PKG_REL_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";

/// A field referenced in a pivot layout: by 0-based source-column index or by
/// header name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PivotField {
    Index(u32),
    Name(String),
}

/// Data-field aggregation. Maps to the `subtotal` attribute Excel validates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PivotAgg {
    Sum,
    Count,
    CountNums,
    Average,
    Max,
    Min,
    Product,
    StdDev,
    StdDevP,
    Var,
    VarP,
}

impl PivotAgg {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().replace(['-', ' '], "").as_str() {
            "sum" => Some(Self::Sum),
            "count" => Some(Self::Count),
            "countnums" => Some(Self::CountNums),
            "average" | "avg" | "mean" => Some(Self::Average),
            "max" => Some(Self::Max),
            "min" => Some(Self::Min),
            "product" => Some(Self::Product),
            "stddev" => Some(Self::StdDev),
            "stddevp" => Some(Self::StdDevP),
            "var" => Some(Self::Var),
            "varp" => Some(Self::VarP),
            _ => None,
        }
    }

    /// The `subtotal` attribute value on `<dataField>`.
    pub fn subtotal(self) -> &'static str {
        match self {
            Self::Sum => "sum",
            Self::Count => "count",
            Self::CountNums => "countNums",
            Self::Average => "average",
            Self::Max => "max",
            Self::Min => "min",
            Self::Product => "product",
            Self::StdDev => "stdDev",
            Self::StdDevP => "stdDevp",
            Self::Var => "var",
            Self::VarP => "varp",
        }
    }

    /// The display prefix for a data field name ("Sum of Amount", "Count of X").
    pub fn label(self) -> &'static str {
        match self {
            Self::Sum => "Sum of",
            Self::Count => "Count of",
            Self::CountNums => "Count of",
            Self::Average => "Average of",
            Self::Max => "Max of",
            Self::Min => "Min of",
            Self::Product => "Product of",
            Self::StdDev => "StdDev of",
            Self::StdDevP => "StdDevp of",
            Self::Var => "Var of",
            Self::VarP => "Varp of",
        }
    }
}

/// One data field: which source column, aggregated how.
#[derive(Debug, Clone)]
pub struct PivotDataField {
    pub field: PivotField,
    pub agg: PivotAgg,
}

/// A complete pivot request, stored on a [`Sheet`].
#[derive(Debug, Clone)]
pub struct PivotTableSpec {
    /// Pivot name (defaults to "PivotTable{n}" when empty).
    pub name: String,
    /// A1 source range including the header row, e.g. "A1:C5".
    pub source_range: String,
    /// Row-axis fields (order preserved).
    pub rows: Vec<PivotField>,
    /// Column-axis fields (order preserved).
    pub cols: Vec<PivotField>,
    /// Data fields (order preserved).
    pub data: Vec<PivotDataField>,
    /// Top-left A1 cell where the pivot renders, e.g. "E3".
    pub target_cell: String,
}

impl PivotTableSpec {
    /// Validate the layout against a worksheet up-front, before anything is
    /// emitted. Returns a human-readable reason on failure.
    pub fn validate(&self, sheet: &Sheet) -> Result<(), String> {
        let (r0, c0, r1, c1) = parse_range_a1(&self.source_range)?;
        if r1 < r0 || c1 < c0 {
            return Err(format!("source range {} is inverted", self.source_range));
        }
        let ncols = (c1 - c0 + 1) as usize;
        let headers = read_headers(sheet, r0, c0, ncols);
        for (kind, fields) in [("row", self.rows.as_slice()), ("col", self.cols.as_slice())] {
            for f in fields {
                resolve_field(f, &headers).map_err(|e| format!("{kind} field: {e}"))?;
            }
        }
        for df in &self.data {
            resolve_field(&df.field, &headers).map_err(|e| format!("data field: {e}"))?;
        }
        parse_a1(&self.target_cell).map_err(|e| format!("target_cell: {e}"))?;
        Ok(())
    }
}

/// Everything the writer needs to place a pivot in the package.
pub struct PivotParts {
    /// The workbook-scoped cache id (consistent across workbook.xml, the cache
    /// definition and the pivot table).
    pub cache_id: u32,
    /// 0-based pivot index; drives the deterministic part numbers.
    pub part_index: usize,
    /// Zip path of the pivot table part (`xl/pivotTables/pivotTableN.xml`).
    pub table_part: String,
    /// Zip parts in deterministic order: `(path, bytes)`.
    pub parts: Vec<(String, Vec<u8>)>,
    /// `[Content_Types].xml` Overrides for this pivot.
    pub content_types: Vec<(String, &'static str)>,
}

/// Build every part for one pivot. Returns `None` (with a log line) when the
/// spec cannot be resolved against the worksheet; the public authoring paths
/// validate before this is ever reached.
pub fn build_pivot_parts(
    sheet: &Sheet,
    spec: &PivotTableSpec,
    cache_id: u32,
    part_index: usize,
) -> Option<PivotParts> {
    let (r0, c0, r1, c1) = parse_range_a1(&spec.source_range).ok()?;
    if r1 < r0 || c1 < c0 {
        log::warn!(
            "pivot {}: source range {:?} inverted",
            spec.name,
            spec.source_range
        );
        return None;
    }
    let ncols = (c1 - c0 + 1) as usize;
    let n_data_rows = (r1 - r0) as usize;
    let headers = read_headers(sheet, r0, c0, ncols);

    let row_cols = match resolve_many(&spec.rows, &headers) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("pivot {}: {e}", spec.name);
            return None;
        }
    };
    let col_cols = match resolve_many(&spec.cols, &headers) {
        Ok(v) => v,
        Err(e) => {
            log::warn!("pivot {}: {e}", spec.name);
            return None;
        }
    };
    let data_cols: Vec<usize> = match spec
        .data
        .iter()
        .map(|d| resolve_field(&d.field, &headers))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(v) => v,
        Err(e) => {
            log::warn!("pivot {}: {e}", spec.name);
            return None;
        }
    };
    let aggs: Vec<PivotAgg> = spec.data.iter().map(|d| d.agg).collect();

    // Materialise the source into per-column value lists.
    let mut cols: Vec<ColumnData> = (0..ncols).map(|_| ColumnData::default()).collect();
    for (ci, col) in cols.iter_mut().enumerate() {
        for ri in 0..n_data_rows {
            let v = cell_value_at(sheet, r0 + 1 + ri as u32, c0 + ci as u32);
            col.values.push(v);
        }
    }

    // Classify each column: sharedItems + per-field distinct index maps.
    for c in cols.iter_mut() {
        c.classify();
    }

    // Distinct row/col key tuples (first-appearance order) for rowItems /
    // colItems. Each tuple stores the per-field distinct index per level.
    let row_keys = distinct_tuples(n_data_rows, &cols, &row_cols);
    let col_keys = distinct_tuples(n_data_rows, &cols, &col_cols);

    // Rendered pivot geometry.
    let header_rows = if data_cols.len() == 1 { 1usize } else { 2usize };
    let n_row_items = if row_cols.is_empty() {
        1
    } else {
        row_keys.len() + 1
    };
    let n_col_items = if col_cols.is_empty() {
        0
    } else {
        col_keys.len() + 1
    };
    let n_rows = header_rows + n_row_items;
    let n_cols = if col_cols.is_empty() {
        1 + data_cols.len()
    } else {
        1 + n_col_items * data_cols.len()
    };

    let name = if spec.name.is_empty() {
        format!("PivotTable{}", part_index + 1)
    } else {
        spec.name.clone()
    };
    let n = part_index + 1;
    let cache_def = format!("xl/pivotCache/pivotCacheDefinition{n}.xml");
    let records = format!("xl/pivotCache/pivotCacheRecords{n}.xml");
    let table = format!("xl/pivotTables/pivotTable{n}.xml");
    let cache_def_rels = format!("xl/pivotCache/_rels/pivotCacheDefinition{n}.xml.rels");
    let table_rels = format!("xl/pivotTables/_rels/pivotTable{n}.xml.rels");

    let cache_def_xml = emit_cache_definition(
        &headers,
        &cols,
        sheet.name.as_str(),
        &spec.source_range,
        n_data_rows,
    );
    let records_xml = emit_cache_records(&cols, n_data_rows);
    let table_xml = emit_pivot_table(
        &name,
        cache_id,
        spec.target_cell.as_str(),
        n_rows as u32,
        n_cols as u32,
        header_rows,
        &headers,
        &cols,
        &row_cols,
        &row_keys,
        &col_cols,
        &col_keys,
        &data_cols,
        &aggs,
    );
    let cache_def_rels_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="{PKG_REL_NS}"><Relationship Id="rId1" Type="{REL_NS}/pivotCacheRecords" Target="pivotCacheRecords{n}.xml"/></Relationships>"#
    );
    let table_rels_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="{PKG_REL_NS}"><Relationship Id="rId1" Type="{REL_NS}/pivotCacheDefinition" Target="../pivotCache/pivotCacheDefinition{n}.xml"/></Relationships>"#
    );

    let parts = vec![
        (table.clone(), table_xml),
        (table_rels.clone(), table_rels_xml.into_bytes()),
        (cache_def.clone(), cache_def_xml),
        (cache_def_rels.clone(), cache_def_rels_xml.into_bytes()),
        (records.clone(), records_xml),
    ];

    let content_types = vec![
        (
            format!("/{cache_def}"),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.pivotCacheDefinition+xml",
        ),
        (
            format!("/{records}"),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.pivotCacheRecords+xml",
        ),
        (
            format!("/{table}"),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.pivotTable+xml",
        ),
    ];

    Some(PivotParts {
        cache_id,
        part_index,
        table_part: table.clone(),
        parts,
        content_types,
    })
}

// ---------------------------------------------------------------------------
// Model helpers
// ---------------------------------------------------------------------------

/// A single cell value as seen by the pivot engine.
#[derive(Debug, Clone)]
enum ColVal {
    Blank,
    Num(f64),
    Date(f64),
    Str(String),
}

/// Per-column accumulation: raw values, classification flags and the ordered
/// distinct items (first-appearance), which double as the sharedItems children
/// AND the per-field index map used by rowItems / colItems / pivotField items.
#[derive(Debug, Default)]
struct ColumnData {
    values: Vec<ColVal>,
    distinct: Vec<ColVal>,
    contains_number: bool,
    contains_string: bool,
    contains_date: bool,
    contains_blank: bool,
    contains_integer: bool,
    min: Option<f64>,
    max: Option<f64>,
    /// True when the column mixes types (numbers + strings + dates).
    mixed: bool,
}

impl ColumnData {
    fn classify(&mut self) {
        let mut contains_blank = false;
        let mut contains_number = false;
        let mut contains_string = false;
        let mut contains_date = false;
        let mut any_num = false;
        let mut any_non_integral = false;
        let mut min: Option<f64> = None;
        let mut max: Option<f64> = None;
        for v in &self.values {
            match v {
                ColVal::Blank => contains_blank = true,
                ColVal::Num(n) => {
                    contains_number = true;
                    any_num = true;
                    if let Some(m) = min {
                        min = Some(m.min(*n));
                    } else {
                        min = Some(*n);
                    }
                    if let Some(m) = max {
                        max = Some(m.max(*n));
                    } else {
                        max = Some(*n);
                    }
                    if n.fract() != 0.0 {
                        any_non_integral = true;
                    }
                }
                ColVal::Date(d) => {
                    contains_date = true;
                    any_num = true;
                    if let Some(m) = min {
                        min = Some(m.min(*d));
                    } else {
                        min = Some(*d);
                    }
                    if let Some(m) = max {
                        max = Some(m.max(*d));
                    } else {
                        max = Some(*d);
                    }
                    if d.fract() != 0.0 {
                        any_non_integral = true;
                    }
                }
                ColVal::Str(_) => contains_string = true,
            }
        }
        self.contains_blank = contains_blank;
        self.contains_number = contains_number;
        self.contains_string = contains_string;
        self.contains_date = contains_date;
        self.contains_integer = any_num && !any_non_integral;
        self.min = min;
        self.max = max;
        let kind_count = self.contains_number as usize
            + self.contains_string as usize
            + self.contains_date as usize;
        self.mixed = kind_count > 1;
        // Record distinct items in first-appearance order. A blank value is
        // NOT a shared item: Excel represents it as containsBlank="1" plus the
        // special item index -2 in the layout axes.
        for v in &self.values {
            if matches!(v, ColVal::Blank) {
                continue;
            }
            if !self.distinct.iter().any(|d| same_value(d, v)) {
                self.distinct.push(v.clone());
            }
        }
    }

    /// Distinct-list index for a value (first-appearance position).
    fn index_of(&self, v: &ColVal) -> Option<usize> {
        self.distinct.iter().position(|d| same_value(d, v))
    }
}

fn same_value(a: &ColVal, b: &ColVal) -> bool {
    match (a, b) {
        (ColVal::Blank, ColVal::Blank) => true,
        (ColVal::Num(x), ColVal::Num(y)) => x == y,
        (ColVal::Date(x), ColVal::Date(y)) => x == y,
        (ColVal::Str(x), ColVal::Str(y)) => x == y,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Source reading
// ---------------------------------------------------------------------------

/// Parse "A1:C5" / "A1" into 1-based inclusive (r0, c0, r1, c1).
fn parse_range_a1(s: &str) -> Result<(u32, u32, u32, u32), String> {
    let (a, b) = match s.split_once(':') {
        Some((a, b)) => (a, b),
        None => (s, s),
    };
    let (r0, c0) = parse_a1(a)?;
    let (r1, c1) = parse_a1(b)?;
    Ok((r0, c0, r1, c1))
}

/// Parse "E3" into 1-based (row, col).
fn parse_a1(s: &str) -> Result<(u32, u32), String> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        i += 1;
    }
    if i == 0 || i == bytes.len() {
        return Err(format!("{s:?} is not an A1 cell reference"));
    }
    let mut col = 0u32;
    for &b in &bytes[..i] {
        let c = b.to_ascii_uppercase();
        col = col * 26 + (c - b'A') as u32 + 1;
    }
    let row: u32 = std::str::from_utf8(&bytes[i..])
        .map_err(|_| format!("{s:?} is not an A1 cell reference"))?
        .parse()
        .map_err(|_| format!("{s:?} is not an A1 cell reference"))?;
    if row == 0 {
        return Err(format!("{s:?} row must be >= 1"));
    }
    Ok((row, col))
}

/// Find a cell's value in the row-major sheet model. Rows and cells are kept
/// strictly sorted, so binary search applies.
fn cell_value_at(sheet: &Sheet, row: u32, col: u32) -> ColVal {
    let Some(r) = sheet
        .rows
        .binary_search_by(|r| r.row.cmp(&row))
        .ok()
        .map(|i| &sheet.rows[i])
    else {
        return ColVal::Blank;
    };
    let Some(c) = r
        .cells
        .binary_search_by(|c| c.col.cmp(&col))
        .ok()
        .map(|i| &r.cells[i])
    else {
        return ColVal::Blank;
    };
    match &c.value {
        CellValue::Empty => ColVal::Blank,
        CellValue::Number(n) => ColVal::Num(*n),
        CellValue::Bool(b) => ColVal::Num(if *b { 1.0 } else { 0.0 }),
        CellValue::Error(e) => ColVal::Str(e.clone()),
        CellValue::Str(s) => ColVal::Str(s.clone()),
        CellValue::DateSerial(d) | CellValue::Time(d) | CellValue::Duration(d) => ColVal::Date(*d),
        CellValue::Rich(rt) => ColVal::Str(rich_text_plain(rt)),
        CellValue::Formula { cached, .. } => match cached {
            Some(cv) => match cv {
                CachedValue::Number(n) => ColVal::Num(*n),
                CachedValue::Bool(b) => ColVal::Num(if *b { 1.0 } else { 0.0 }),
                CachedValue::Error(e) => ColVal::Str(e.clone()),
                CachedValue::Str(s) => ColVal::Str(s.clone()),
            },
            None => ColVal::Blank,
        },
    }
}

fn rich_text_plain(rt: &RichText) -> String {
    let mut out = String::new();
    for run in &rt.runs {
        match run {
            RichRun::Text(t) => out.push_str(t.as_str()),
            RichRun::Block { text, .. } => out.push_str(text.as_str()),
        }
    }
    out
}

/// Header cell of every source column; missing cells get a deterministic
/// generated name.
fn read_headers(sheet: &Sheet, r0: u32, c0: u32, ncols: usize) -> Vec<String> {
    let mut out = Vec::with_capacity(ncols);
    for ci in 0..ncols {
        let name = match cell_value_at(sheet, r0, c0 + ci as u32) {
            ColVal::Str(s) => s,
            ColVal::Num(n) => fmt_num(n),
            ColVal::Date(d) => fmt_num(d),
            _ => format!("Column{}", ci + 1),
        };
        out.push(name);
    }
    out
}

fn resolve_field(f: &PivotField, headers: &[String]) -> Result<usize, String> {
    match f {
        PivotField::Index(i) => {
            let i = *i as usize;
            if i >= headers.len() {
                Err(format!(
                    "field index {i} out of range (source has {} columns)",
                    headers.len()
                ))
            } else {
                Ok(i)
            }
        }
        PivotField::Name(n) => headers
            .iter()
            .position(|h| h == n)
            .ok_or_else(|| format!("field {:?} not found in source header", n)),
    }
}

fn resolve_many(fields: &[PivotField], headers: &[String]) -> Result<Vec<usize>, String> {
    fields.iter().map(|f| resolve_field(f, headers)).collect()
}

/// Deterministic number formatting: integral values lose the trailing ".0",
/// everything else uses ryu (matches `write_f64` output).
fn fmt_num(n: f64) -> String {
    if n.is_finite() && n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 {
        format!("{}", n as i64)
    } else {
        let mut b = ryu::Buffer::new();
        b.format(n).to_string()
    }
}

/// The item index Excel uses for a blank value on a pivot axis.
const BLANK_ITEM: i32 = -2;

/// Distinct value tuples for one axis, in first-appearance order. Each tuple is
/// the per-level item index of its column's value: the position in the field's
/// distinct list, or [`BLANK_ITEM`] for a blank value.
fn distinct_tuples(n_rows: usize, cols: &[ColumnData], axis: &[usize]) -> Vec<Vec<i32>> {
    let mut out: Vec<Vec<i32>> = Vec::new();
    if axis.is_empty() {
        return out;
    }
    for ri in 0..n_rows {
        let mut idx = Vec::with_capacity(axis.len());
        for &ci in axis {
            match cols[ci].index_of(&cols[ci].values[ri]) {
                Some(i) => idx.push(i as i32),
                None => idx.push(BLANK_ITEM),
            }
        }
        if !out.iter().any(|t| t == &idx) {
            out.push(idx);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Part emitters
// ---------------------------------------------------------------------------

fn emit_cache_definition(
    headers: &[String],
    cols: &[ColumnData],
    sheet_name: &str,
    source_range: &str,
    record_count: usize,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(1024);
    push(
        &mut out,
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<pivotCacheDefinition xmlns=""#,
    );
    push(&mut out, SHEET_NS.as_bytes());
    push(&mut out, br#"" xmlns:r=""#);
    push(&mut out, REL_NS.as_bytes());
    push(&mut out, br#"""#);
    push(
        &mut out,
        br#" refreshedBy="kyrax" createdVersion="8" refreshedVersion="8" minRefreshableVersion="3" recordCount=""#,
    );
    write_u32(&mut out, record_count as u32);
    push(&mut out, br#"" refreshOnLoad="1">"#);
    push(
        &mut out,
        br#"<cacheSource type="worksheet"><worksheetSource sheet=""#,
    );
    write_escaped_attr(&mut out, sheet_name);
    push(&mut out, br#"" ref=""#);
    write_escaped_attr(&mut out, source_range);
    push(&mut out, br#""/></cacheSource>"#);
    push(&mut out, br#"<cacheFields count=""#);
    write_u32(&mut out, cols.len() as u32);
    push(&mut out, b"\">");
    for (i, c) in cols.iter().enumerate() {
        push(&mut out, br#"<cacheField name=""#);
        write_escaped_attr(&mut out, &headers[i]);
        push(&mut out, br#"" numFmtId="0">"#);
        emit_shared_items(&mut out, c);
        push(&mut out, b"</cacheField>");
    }
    push(&mut out, b"</cacheFields></pivotCacheDefinition>");
    out
}

fn emit_shared_items(out: &mut Vec<u8>, c: &ColumnData) {
    let is_numeric = c.contains_number && !c.contains_string && !c.contains_date;
    let is_date = c.contains_date && !c.contains_number && !c.contains_string;
    let is_string = c.contains_string && !c.contains_number && !c.contains_date;
    let has_children = !is_numeric;

    push(out, b"<sharedItems");
    if has_children && !c.distinct.is_empty() {
        push(out, br#" count=""#);
        write_u32(out, c.distinct.len() as u32);
        push(out, b"\"");
    }
    push(out, br#" containsSemiMixedTypes=""#);
    push(out, if c.mixed { b"1" } else { b"0" });
    push(out, b"\"");
    push(out, br#" containsString=""#);
    push(out, if c.contains_string { b"1" } else { b"0" });
    push(out, b"\"");
    push(out, br#" containsBlank=""#);
    push(out, if c.contains_blank { b"1" } else { b"0" });
    push(out, b"\"");
    push(out, br#" containsNumber=""#);
    push(out, if c.contains_number { b"1" } else { b"0" });
    push(out, b"\"");
    if is_numeric {
        push(out, br#" containsInteger=""#);
        push(out, if c.contains_integer { b"1" } else { b"0" });
        push(out, b"\"");
    }
    if c.contains_date {
        push(out, br#" containsDate=""#);
        push(out, b"1");
        push(out, b"\"");
    }
    if let (Some(mn), Some(mx)) = (c.min, c.max) {
        push(out, br#" minValue=""#);
        write_escaped_attr(out, &fmt_num(mn));
        push(out, b"\"");
        push(out, br#" maxValue=""#);
        write_escaped_attr(out, &fmt_num(mx));
        push(out, b"\"");
    }
    if is_numeric {
        push(out, b"/>");
        return;
    }
    push(out, b">");
    if is_string || c.mixed {
        for v in &c.distinct {
            let s = match v {
                ColVal::Str(s) => s.clone(),
                ColVal::Num(n) => fmt_num(*n),
                ColVal::Date(d) => fmt_num(*d),
                ColVal::Blank => String::new(),
            };
            push(out, br#"<s v=""#);
            write_escaped_attr(out, &s);
            push(out, br#""/>"#);
        }
    } else if is_date {
        for v in &c.distinct {
            if let ColVal::Date(d) = v {
                push(out, br#"<d v=""#);
                write_escaped_attr(out, &fmt_num(*d));
                push(out, br#""/>"#);
            }
        }
    }
    push(out, b"</sharedItems>");
}

fn emit_cache_records(cols: &[ColumnData], n_rows: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n_rows.saturating_mul(cols.len()).saturating_mul(8) + 256);
    push(
        &mut out,
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<pivotCacheRecords xmlns=""#,
    );
    push(&mut out, SHEET_NS.as_bytes());
    push(&mut out, br#"" count=""#);
    write_u32(&mut out, n_rows as u32);
    push(&mut out, b"\">");
    for ri in 0..n_rows {
        push(&mut out, b"<r>");
        for c in cols {
            let v = &c.values[ri];
            if matches!(v, ColVal::Blank) {
                continue;
            }
            if c.contains_number && !c.contains_string && !c.contains_date {
                if let ColVal::Num(n) = v {
                    push(&mut out, b"<n v=\"");
                    write_escaped_attr(&mut out, &fmt_num(*n));
                    push(&mut out, br#""/>"#);
                }
            } else if let Some(idx) = c.index_of(v) {
                push(&mut out, b"<s v=\"");
                write_u32(&mut out, idx as u32);
                push(&mut out, br#""/>"#);
            }
        }
        push(&mut out, b"</r>");
    }
    push(&mut out, b"</pivotCacheRecords>");
    out
}

#[allow(clippy::too_many_arguments)]
fn emit_pivot_table(
    name: &str,
    cache_id: u32,
    target_cell: &str,
    n_rows: u32,
    n_cols: u32,
    header_rows: usize,
    headers: &[String],
    cols: &[ColumnData],
    row_cols: &[usize],
    row_keys: &[Vec<i32>],
    col_cols: &[usize],
    col_keys: &[Vec<i32>],
    data_cols: &[usize],
    aggs: &[PivotAgg],
) -> Vec<u8> {
    let (tr, tc) = parse_a1(target_cell).unwrap_or((1, 1));
    let location = dimension_ref(tr, tc, tr + n_rows - 1, tc + n_cols - 1);

    let mut out = Vec::with_capacity(2048);
    push(
        &mut out,
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<pivotTableDefinition xmlns=""#,
    );
    push(&mut out, SHEET_NS.as_bytes());
    push(&mut out, br#"" name=""#);
    write_escaped_attr(&mut out, name);
    push(&mut out, br#"" cacheId=""#);
    write_u32(&mut out, cache_id);
    push(
        &mut out,
        br#"" dataCaption="Values" createdVersion="8" updatedVersion="8" minRefreshableVersion="3" useAutoFormatting="1" rowGrandTotals="1" colGrandTotals="1">"#,
    );
    push(&mut out, br#"<location ref=""#);
    write_escaped_attr(&mut out, &location);
    push(&mut out, br#"" firstHeaderRow="1" firstDataRow=""#);
    write_u32(&mut out, header_rows as u32 + 1);
    push(&mut out, br#"" firstDataCol="1"/>"#);

    // pivotFields: one per cache field.
    push(&mut out, br#"<pivotFields count=""#);
    write_u32(&mut out, headers.len() as u32);
    push(&mut out, b"\">");
    let in_rows = |i: usize| row_cols.contains(&i);
    let in_cols = |i: usize| col_cols.contains(&i);
    let in_data = |i: usize| data_cols.contains(&i);
    for (i, _) in headers.iter().enumerate() {
        if in_rows(i) {
            push(&mut out, br#"<pivotField axis="axisRow" showAll="0">"#);
            emit_pivot_field_items(&mut out, cols[i].distinct.len(), cols[i].contains_blank);
            push(&mut out, b"</pivotField>");
        } else if in_cols(i) {
            push(&mut out, br#"<pivotField axis="axisCol" showAll="0">"#);
            emit_pivot_field_items(&mut out, cols[i].distinct.len(), cols[i].contains_blank);
            push(&mut out, b"</pivotField>");
        } else if in_data(i) {
            push(&mut out, br#"<pivotField dataField="1" showAll="0"/>"#);
        } else {
            push(&mut out, br#"<pivotField showAll="0"/>"#);
        }
    }
    push(&mut out, b"</pivotFields>");

    if !row_cols.is_empty() {
        emit_axis(&mut out, b"rowFields", b"rowItems", row_cols, row_keys);
    }
    if !col_cols.is_empty() {
        emit_axis(&mut out, b"colFields", b"colItems", col_cols, col_keys);
    }

    // dataFields
    push(&mut out, br#"<dataFields count=""#);
    write_u32(&mut out, data_cols.len() as u32);
    push(&mut out, b"\">");
    for (i, &ci) in data_cols.iter().enumerate() {
        push(&mut out, br#"<dataField name=""#);
        let df_name = format!("{} {}", aggs[i].label(), headers[ci]);
        write_escaped_attr(&mut out, &df_name);
        push(&mut out, br#"" fld=""#);
        write_u32(&mut out, ci as u32);
        push(&mut out, br#"" baseField="0" baseItem="0" subtotal=""#);
        push(&mut out, aggs[i].subtotal().as_bytes());
        push(&mut out, br#""/>"#);
    }
    push(&mut out, b"</dataFields>");
    push(&mut out, b"</pivotTableDefinition>");
    out
}

/// `<field x=".."/>` list + `<items>` list for one axis.
fn emit_axis(
    out: &mut Vec<u8>,
    fields_tag: &[u8],
    items_tag: &[u8],
    cols: &[usize],
    keys: &[Vec<i32>],
) {
    push(out, b"<");
    push(out, fields_tag);
    push(out, br#" count=""#);
    write_u32(out, cols.len() as u32);
    push(out, b"\">");
    for &ci in cols {
        push(out, br#"<field x=""#);
        write_u32(out, ci as u32);
        push(out, br#""/>"#);
    }
    push(out, b"</");
    push(out, fields_tag);
    push(out, b">");

    push(out, b"<");
    push(out, items_tag);
    push(out, br#" count=""#);
    write_u32(out, (keys.len() + 1) as u32);
    push(out, b"\">");
    for key in keys {
        push(out, b"<i>");
        for &x in key {
            if x == 0 {
                push(out, b"<x/>");
            } else {
                push(out, b"<x v=\"");
                write_i32(out, x);
                push(out, b"\"/>");
            }
        }
        push(out, b"</i>");
    }
    push(out, b"<i t=\"grand\">");
    for _ in 0..cols.len() {
        push(out, b"<x/>");
    }
    push(out, b"</i></");
    push(out, items_tag);
    push(out, b">");
}

/// `<items count="{k + blank + 1}">` with `<item x="i"/>` per distinct value, an
/// `<item x="-2"/>` for the "(blank)" group when the column has blanks, and the
/// trailing `<item t="default"/>`.
fn emit_pivot_field_items(out: &mut Vec<u8>, distinct: usize, has_blank: bool) {
    let count = distinct as u32 + 1 + has_blank as u32;
    push(out, br#"<items count=""#);
    write_u32(out, count);
    push(out, b"\">");
    for i in 0..distinct {
        push(out, b"<item x=\"");
        write_u32(out, i as u32);
        push(out, b"\"/>");
    }
    if has_blank {
        push(out, br#"<item x="-2"/>"#);
    }
    push(out, br#"<item t="default"/>"#);
    push(out, b"</items>");
}

// ---------------------------------------------------------------------------
// Small XML helpers (kept local so pivot.rs stays self-contained).
// ---------------------------------------------------------------------------

#[inline]
fn push(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(b);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turbo::write::model::{Cell, Row, Sheet};
    use pretty_assertions::assert_eq;

    fn sheet_with(data: &[&[CellValue]]) -> Sheet {
        let mut s = Sheet::new("Data");
        for (ri, row) in data.iter().enumerate() {
            let mut r = Row::new(ri as u32 + 1);
            for (ci, v) in row.iter().enumerate() {
                r.cells.push(Cell::new(ci as u32 + 1, v.clone()));
            }
            s.rows.push(r);
        }
        s
    }

    fn spec() -> PivotTableSpec {
        PivotTableSpec {
            name: "PivotTable1".into(),
            source_range: "A1:C5".into(),
            rows: vec![PivotField::Name("Region".into())],
            cols: vec![PivotField::Name("Product".into())],
            data: vec![PivotDataField {
                field: PivotField::Name("Amount".into()),
                agg: PivotAgg::Sum,
            }],
            target_cell: "E3".into(),
        }
    }

    fn fixture_sheet() -> Sheet {
        sheet_with(&[
            &[
                CellValue::Str("Region".into()),
                CellValue::Str("Product".into()),
                CellValue::Str("Amount".into()),
            ],
            &[
                CellValue::Str("East".into()),
                CellValue::Str("Widget".into()),
                CellValue::Number(100.0),
            ],
            &[
                CellValue::Str("East".into()),
                CellValue::Str("Gadget".into()),
                CellValue::Number(150.0),
            ],
            &[
                CellValue::Str("West".into()),
                CellValue::Str("Widget".into()),
                CellValue::Number(200.0),
            ],
            &[
                CellValue::Str("West".into()),
                CellValue::Str("Gadget".into()),
                CellValue::Number(50.0),
            ],
        ])
    }

    #[test]
    fn parse_a1_basic() {
        assert_eq!(parse_a1("E3").unwrap(), (3, 5));
        assert_eq!(parse_a1("A1").unwrap(), (1, 1));
        assert_eq!(parse_range_a1("A1:C5").unwrap(), (1, 1, 5, 3));
        assert!(parse_a1("12").is_err());
        assert!(parse_a1("E").is_err());
    }

    #[test]
    fn validate_fixture_spec_ok() {
        let sheet = fixture_sheet();
        assert!(spec().validate(&sheet).is_ok());
    }

    #[test]
    fn validate_bad_field_fails() {
        let sheet = fixture_sheet();
        let mut s = spec();
        s.rows = vec![PivotField::Name("Nope".into())];
        assert!(s.validate(&sheet).is_err());
    }

    #[test]
    fn shared_items_flags_are_honest() {
        let sheet = fixture_sheet();
        let parts = build_pivot_parts(&sheet, &spec(), 0, 0).expect("build");
        let (_, def) = parts
            .parts
            .iter()
            .find(|(p, _)| p.contains("pivotCacheDefinition1"))
            .expect("cache def part");
        let s = String::from_utf8_lossy(def).into_owned();
        // Region: pure strings, two distinct.
        assert!(s.contains(r#"<sharedItems count="2" containsSemiMixedTypes="0" containsString="1" containsBlank="0" containsNumber="0">"#), "{s}");
        assert!(s.contains(r#"<s v="East"/><s v="West"/>"#), "{s}");
        // Amount: pure numbers, integer, min 50 max 200, no children.
        assert!(
            s.contains(
                r#"containsSemiMixedTypes="0" containsString="0" containsBlank="0" containsNumber="1" containsInteger="1" minValue="50" maxValue="200"/>"#
            ),
            "{s}"
        );
        assert!(s.contains(r#"ref="A1:C5""#), "{s}");
        assert!(s.contains(r#"refreshOnLoad="1""#), "{s}");
        assert!(s.contains(r#"recordCount="4""#), "{s}");
    }

    #[test]
    fn records_are_inline_or_shared() {
        let sheet = fixture_sheet();
        let parts = build_pivot_parts(&sheet, &spec(), 0, 0).expect("build");
        let (_, rec) = parts
            .parts
            .iter()
            .find(|(p, _)| p.contains("pivotCacheRecords1"))
            .expect("records part");
        let s = String::from_utf8_lossy(rec).into_owned();
        assert!(s.contains(r#"count="4""#), "{s}");
        assert!(
            s.contains(r#"<r><s v="0"/><s v="0"/><n v="100"/></r>"#),
            "{s}"
        );
        assert!(
            s.contains(r#"<r><s v="1"/><s v="1"/><n v="50"/></r>"#),
            "{s}"
        );
    }

    #[test]
    fn location_and_items_match_layout() {
        let sheet = fixture_sheet();
        let parts = build_pivot_parts(&sheet, &spec(), 0, 0).expect("build");
        let (_, table) = parts
            .parts
            .iter()
            .find(|(p, _)| p.contains("pivotTable1"))
            .expect("table part");
        let s = String::from_utf8_lossy(table).into_owned();
        // 4 rows (header + East + West + grand) x 4 cols (caption + Widget +
        // Gadget + Grand Total) from E3.
        assert!(
            s.contains(
                r#"<location ref="E3:H6" firstHeaderRow="1" firstDataRow="2" firstDataCol="1"/>"#
            ),
            "{s}"
        );
        assert!(
            s.contains(r#"<rowFields count="1"><field x="0"/></rowFields>"#),
            "{s}"
        );
        assert!(
            s.contains(r#"<colFields count="1"><field x="1"/></colFields>"#),
            "{s}"
        );
        assert!(s.contains(r#"<rowItems count="3"><i><x/></i><i><x v="1"/></i><i t="grand"><x/></i></rowItems>"#), "{s}");
        assert!(s.contains(r#"<dataField name="Sum of Amount" fld="2" baseField="0" baseItem="0" subtotal="sum"/>"#), "{s}");
        assert!(s.contains(r#"<pivotField axis="axisRow" showAll="0"><items count="3"><item x="0"/><item x="1"/><item t="default"/></items></pivotField>"#), "{s}");
    }

    #[test]
    fn content_types_and_rel_targets() {
        let sheet = fixture_sheet();
        let parts = build_pivot_parts(&sheet, &spec(), 0, 0).expect("build");
        let names: Vec<&str> = parts.parts.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            names,
            [
                "xl/pivotTables/pivotTable1.xml",
                "xl/pivotTables/_rels/pivotTable1.xml.rels",
                "xl/pivotCache/pivotCacheDefinition1.xml",
                "xl/pivotCache/_rels/pivotCacheDefinition1.xml.rels",
                "xl/pivotCache/pivotCacheRecords1.xml",
            ]
        );
        assert_eq!(parts.content_types.len(), 3);
        assert!(
            parts
                .content_types
                .iter()
                .any(|(p, _)| p == "/xl/pivotCache/pivotCacheDefinition1.xml")
        );
        let rels = String::from_utf8_lossy(&parts.parts[3].1).into_owned();
        assert!(rels.contains("pivotCacheRecords1.xml"), "{rels}");
    }

    #[test]
    fn deterministic_build() {
        let sheet = fixture_sheet();
        let a = build_pivot_parts(&sheet, &spec(), 0, 0).expect("a");
        let b = build_pivot_parts(&sheet, &spec(), 0, 0).expect("b");
        for ((pa, ba), (pb, bb)) in a.parts.iter().zip(b.parts.iter()) {
            assert_eq!(pa, pb);
            assert_eq!(ba, bb);
        }
    }

    #[test]
    fn blank_source_cells_flagged() {
        let sheet = sheet_with(&[
            &[
                CellValue::Str("A".into()),
                CellValue::Str("B".into()),
                CellValue::Str("V".into()),
            ],
            &[
                CellValue::Str("x".into()),
                CellValue::Empty,
                CellValue::Number(1.0),
            ],
            &[
                CellValue::Str("y".into()),
                CellValue::Str("z".into()),
                CellValue::Number(2.0),
            ],
        ]);
        let s = PivotTableSpec {
            name: "P1".into(),
            source_range: "A1:C3".into(),
            rows: vec![PivotField::Name("B".into())],
            cols: vec![],
            data: vec![PivotDataField {
                field: PivotField::Name("V".into()),
                agg: PivotAgg::Sum,
            }],
            target_cell: "E3".into(),
        };
        let parts = build_pivot_parts(&sheet, &s, 0, 0).expect("build");
        let (_, def) = parts
            .parts
            .iter()
            .find(|(p, _)| p.contains("pivotCacheDefinition1"))
            .expect("def");
        let text = String::from_utf8_lossy(def).into_owned();
        assert!(text.contains(r#"containsBlank="1""#), "{text}");
        let (_, rec) = parts
            .parts
            .iter()
            .find(|(p, _)| p.contains("pivotCacheRecords1"))
            .expect("rec");
        let rtext = String::from_utf8_lossy(rec).into_owned();
        // First record: A="x" (idx 0), field B blank -> omitted, V=1.
        assert!(rtext.contains(r#"<r><s v="0"/><n v="1"/></r>"#), "{rtext}");
        // Second record: A="y" (idx 1), B="z" (idx 0), V=2.
        assert!(
            rtext.contains(r#"<r><s v="1"/><s v="0"/><n v="2"/></r>"#),
            "{rtext}"
        );
    }

    #[test]
    fn date_column_declares_contains_date() {
        let sheet = sheet_with(&[
            &[CellValue::Str("D".into()), CellValue::Str("V".into())],
            &[CellValue::DateSerial(43831.0), CellValue::Number(1.0)],
            &[CellValue::DateSerial(43832.0), CellValue::Number(2.0)],
        ]);
        let s = PivotTableSpec {
            name: "P1".into(),
            source_range: "A1:B3".into(),
            rows: vec![PivotField::Name("D".into())],
            cols: vec![],
            data: vec![PivotDataField {
                field: PivotField::Name("V".into()),
                agg: PivotAgg::Sum,
            }],
            target_cell: "E3".into(),
        };
        let parts = build_pivot_parts(&sheet, &s, 0, 0).expect("build");
        let (_, def) = parts
            .parts
            .iter()
            .find(|(p, _)| p.contains("pivotCacheDefinition1"))
            .expect("def");
        let text = String::from_utf8_lossy(def).into_owned();
        assert!(text.contains(r#"containsDate="1""#), "{text}");
        assert!(text.contains(r#"<d v="43831"/>"#), "{text}");
        let (_, rec) = parts
            .parts
            .iter()
            .find(|(p, _)| p.contains("pivotCacheRecords1"))
            .expect("rec");
        let rtext = String::from_utf8_lossy(rec).into_owned();
        assert!(rtext.contains(r#"<s v="0"/><n v="1"/>"#), "{rtext}");
    }
}
