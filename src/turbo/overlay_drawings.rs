//! Charts + images authored through the editable overlay (`load_workbook` /
//! `Workbook()`), openpyxl-style: `ws.add_chart(chart, "F2")`,
//! `ws.add_image(img, "E5")`.
//!
//! The chart/drawing XML comes from the batch writer's emitters
//! (`write_chart_space`, `write_drawing_full`); this module only converts the
//! familiar Python data holders into those models and, at save time, packages
//! the new parts (chart, drawing + rels, media, sheet rel, `<drawing>` element,
//! content types) without colliding with existing part names.
//!
//! Limitation: a sheet that already has a drawing part (existing charts,
//! images or shapes) is refused rather than merged.

use ahash::AHashMap;

use crate::turbo::error::{TurboError, TurboResult};
use crate::turbo::overlay::{WorkbookOverlay, find_element, inflate_entry, splice_tail_control};
use crate::turbo::write::charts::{DrawingImage, write_chart_space, write_drawing_full};
use crate::turbo::write::media::MediaInterner;

const CHART_CT: &str = "application/vnd.openxmlformats-officedocument.drawingml.chart+xml";
const DRAWING_CT: &str = "application/vnd.openxmlformats-officedocument.drawing+xml";
const DRAWING_REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing";
const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/// CT_Worksheet children that must precede `<drawing>`.
const DRAWING_PREDECESSORS: &[&[u8]] = &[
    b"sheetData",
    b"sheetCalcPr",
    b"sheetProtection",
    b"protectedRanges",
    b"scenarios",
    b"autoFilter",
    b"sortState",
    b"dataConsolidate",
    b"customSheetViews",
    b"mergeCells",
    b"phoneticPr",
    b"conditionalFormatting",
    b"dataValidations",
    b"hyperlinks",
    b"printOptions",
    b"pageMargins",
    b"pageSetup",
    b"headerFooter",
    b"rowBreaks",
    b"colBreaks",
    b"customProperties",
    b"cellWatches",
    b"ignoredErrors",
    b"smartTags",
];

/// Pixel size `(width, height)` read from a PNG / GIF / JPEG header.
pub fn image_pixel_size(b: &[u8]) -> Option<(u32, u32)> {
    let be16 = |i: usize| Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]) as u32);
    if b.starts_with(&[0x89, b'P', b'N', b'G']) {
        let w = u32::from_be_bytes(b.get(16..20)?.try_into().ok()?);
        let h = u32::from_be_bytes(b.get(20..24)?.try_into().ok()?);
        return Some((w, h));
    }
    if b.starts_with(b"GIF8") {
        let w = u16::from_le_bytes([*b.get(6)?, *b.get(7)?]) as u32;
        let h = u16::from_le_bytes([*b.get(8)?, *b.get(9)?]) as u32;
        return Some((w, h));
    }
    if b.starts_with(&[0xFF, 0xD8]) {
        let mut i = 2;
        while i + 4 <= b.len() {
            if b[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = b[i + 1];
            if marker == 0xFF {
                i += 1;
                continue;
            }
            if marker == 0xD8 || (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
                i += 2;
                continue;
            }
            let len = be16(i + 2)? as usize;
            // SOF0..SOF15 except DHT (C4), JPG (C8), DAC (CC).
            if (0xC0..=0xCF).contains(&marker) && ![0xC4, 0xC8, 0xCC].contains(&marker) {
                let h = be16(i + 5)?;
                let w = be16(i + 7)?;
                return Some((w, h));
            }
            i += 2 + len;
        }
    }
    None
}

/// `'Sheet'!$A$1:$B$2` (or a single `$A$1`) for 1-based inclusive bounds.
pub fn abs_ref(sheet: &str, r1: u32, c1: u32, r2: u32, c2: u32) -> String {
    fn cell(r: u32, c: u32) -> String {
        let mut buf = [0u8; 4];
        let letters = crate::turbo::write::xml::col_letters(c, &mut buf);
        format!("${}${r}", std::str::from_utf8(letters).unwrap_or("A"))
    }
    let q = format!("'{}'", sheet.replace('\'', "''"));
    if r1 == r2 && c1 == c2 {
        format!("{q}!{}", cell(r1, c1))
    } else {
        format!("{q}!{}:{}", cell(r1, c1), cell(r2, c2))
    }
}

/// openpyxl `add_data` split: one `(values, title)` per column (or per row
/// when `from_rows`), the first cell becoming the title when
/// `titles_from_data`. Bounds are 1-based inclusive `(r1, c1, r2, c2)`.
pub fn split_data(
    sheet: &str,
    (r1, c1, r2, c2): (u32, u32, u32, u32),
    titles_from_data: bool,
    from_rows: bool,
) -> Vec<(String, Option<String>)> {
    let skip = u32::from(titles_from_data);
    let mut out = Vec::new();
    if from_rows {
        for r in r1..=r2 {
            let title = titles_from_data.then(|| abs_ref(sheet, r, c1, r, c1));
            out.push((abs_ref(sheet, r, c1 + skip, r, c2.max(c1 + skip)), title));
        }
    } else {
        for c in c1..=c2 {
            let title = titles_from_data.then(|| abs_ref(sheet, r1, c, r1, c));
            out.push((abs_ref(sheet, r1 + skip, c, r2.max(r1 + skip), c), title));
        }
    }
    out
}

/// True when the sheet's source XML already carries a `<drawing>` element.
pub fn sheet_has_drawing(ov: &WorkbookOverlay, sheet_name: &str) -> TurboResult<bool> {
    let Some(entry) = ov.archive_map.sheet_name_map.get(sheet_name) else {
        return Ok(false);
    };
    Ok(inflate_entry(&ov.archive_map, entry)?
        .is_some_and(|xml| find_element(&xml, b"drawing", 0).is_some()))
}

fn max_part_number(names: &[&str], prefix: &str) -> u32 {
    names
        .iter()
        .filter_map(|n| n.strip_prefix(prefix))
        .filter_map(|rest| rest.split('.').next()?.parse::<u32>().ok())
        .max()
        .unwrap_or(0)
}

fn max_rid(rels: &str) -> u32 {
    let mut max_id = 0u32;
    let mut pos = 0usize;
    while let Some(off) = rels[pos..].find("Id=\"rId") {
        let start = pos + off + "Id=\"rId".len();
        let end = rels[start..].find('"').map_or(start, |e| start + e);
        if let Ok(n) = rels[start..end].parse::<u32>() {
            max_id = max_id.max(n);
        }
        pos = end;
    }
    max_id
}

/// Make sure the worksheet root declares the `r:` prefix used by `<drawing>`.
fn ensure_r_namespace(xml: &mut Vec<u8>) {
    let Some(start) = find_element(xml, b"worksheet", 0) else {
        return;
    };
    let Some(gt) = memchr::memchr(b'>', &xml[start..]).map(|g| start + g) else {
        return;
    };
    if memchr::memmem::find(&xml[start..gt], b"xmlns:r=").is_some() {
        return;
    }
    let decl = format!(" xmlns:r=\"{REL_NS}\"");
    let at = if xml[gt - 1] == b'/' { gt - 1 } else { gt };
    xml.splice(at..at, decl.into_bytes());
}

/// Save hook: package every chart/image queued on the overlay's sheets.
/// Runs after the sheet XML has been rendered (dirty sheets are in `rendered`).
pub(crate) fn apply_drawings(
    ov: &WorkbookOverlay,
    rendered: &mut AHashMap<String, Vec<u8>>,
    rewritten_parts: &mut AHashMap<String, Vec<u8>>,
) -> TurboResult<()> {
    let mut names: Vec<&String> = ov
        .sheet_overlays
        .iter()
        .filter(|(n, o)| {
            (!o.new_charts.is_empty() || !o.new_images.is_empty())
                && !ov.deleted_sheets.contains(*n)
        })
        .map(|(n, _)| n)
        .collect();
    if names.is_empty() {
        return Ok(());
    }
    names.sort();

    let all_parts: Vec<String> = ov
        .archive_map
        .entry_order
        .iter()
        .chain(rendered.keys())
        .cloned()
        .collect();
    let part_refs: Vec<&str> = all_parts.iter().map(String::as_str).collect();
    let mut next_chart = max_part_number(&part_refs, "xl/charts/chart");
    let mut next_drawing = max_part_number(&part_refs, "xl/drawings/drawing");
    let media_base = max_part_number(&part_refs, "xl/media/image");

    let mut interner = MediaInterner::new();
    let mut overrides: Vec<(String, &'static str)> = Vec::new();

    for sheet_name in names {
        let so = &ov.sheet_overlays[sheet_name];
        let entry = ov
            .archive_map
            .sheet_name_map
            .get(sheet_name)
            .cloned()
            .ok_or_else(|| TurboError::Format(format!("sheet '{sheet_name}' has no part")))?;
        let Some(sheet_xml) = rendered.get(&entry) else {
            return Err(TurboError::Format(format!(
                "sheet '{sheet_name}' was not rendered; cannot attach its drawing"
            )));
        };
        if find_element(sheet_xml, b"drawing", 0).is_some() {
            return Err(TurboError::Refused(format!(
                "sheet '{sheet_name}' already has a drawing (existing charts/images); \
                 adding new charts or images to it is not supported yet"
            )));
        }

        let mut chart_paths = Vec::with_capacity(so.new_charts.len());
        for chart in &so.new_charts {
            next_chart += 1;
            let path = format!("xl/charts/chart{next_chart}.xml");
            rendered.insert(path.clone(), write_chart_space(chart).into_bytes());
            chart_paths.push(format!("/{path}"));
            overrides.push((path, CHART_CT));
        }
        let mut images = Vec::with_capacity(so.new_images.len());
        let mut media_targets = Vec::with_capacity(so.new_images.len());
        for (i, img) in so.new_images.iter().enumerate() {
            let idx = interner.intern(&img.bytes, img.format);
            let id = so.new_charts.len() + i + 1;
            images.push(DrawingImage {
                anchor: img.anchor.clone(),
                rel_id: id,
                cnv_id: id,
            });
            media_targets.push(format!(
                "/xl/media/image{}.{}",
                media_base as usize + idx + 1,
                img.format.extension()
            ));
        }
        let (drawing_xml, drawing_rels) =
            write_drawing_full(&so.new_charts, &chart_paths, &images, &media_targets);
        next_drawing += 1;
        let drawing_path = format!("xl/drawings/drawing{next_drawing}.xml");
        rendered.insert(drawing_path.clone(), drawing_xml.into_bytes());
        rendered.insert(
            format!("xl/drawings/_rels/drawing{next_drawing}.xml.rels"),
            drawing_rels.into_bytes(),
        );
        overrides.push((drawing_path.clone(), DRAWING_CT));

        // Sheet rels: append the drawing relationship with a fresh rId.
        let (dir, file) = entry.rsplit_once('/').unwrap_or(("xl/worksheets", &entry));
        let rels_path = format!("{dir}/_rels/{file}.rels");
        let mut rels = match rendered.get(&rels_path) {
            Some(r) => String::from_utf8_lossy(r).into_owned(),
            None => match inflate_entry(&ov.archive_map, &rels_path)? {
                Some(r) => String::from_utf8_lossy(&r).into_owned(),
                None => "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"></Relationships>".to_string(),
            },
        };
        let rid = format!("rId{}", max_rid(&rels) + 1);
        let Some(ins) = rels.rfind("</Relationships>") else {
            return Err(TurboError::Format(format!("malformed {rels_path}")));
        };
        rels.insert_str(
            ins,
            &format!("<Relationship Id=\"{rid}\" Type=\"{DRAWING_REL}\" Target=\"/{drawing_path}\"/>"),
        );
        rendered.insert(rels_path, rels.into_bytes());

        let sheet_xml = rendered.get_mut(&entry).expect("checked above");
        ensure_r_namespace(sheet_xml);
        let tag = format!("<drawing r:id=\"{rid}\"/>");
        splice_tail_control(
            sheet_xml,
            b"drawing",
            tag.as_bytes(),
            DRAWING_PREDECESSORS,
            b"</worksheet>",
        );
    }

    for i in 0..interner.len() {
        let ext = interner.media_part_name(i);
        let ext = ext.rsplit('.').next().unwrap_or("png");
        rendered.insert(
            format!("xl/media/image{}.{ext}", media_base as usize + i + 1),
            interner.media_bytes(i).to_vec(),
        );
    }

    // [Content_Types].xml: latest rendition wins (rendered > structure rewrite > source).
    let ct_bytes = match rendered.remove("[Content_Types].xml") {
        Some(c) => Some(c),
        None => match rewritten_parts.remove("[Content_Types].xml") {
            Some(c) => Some(c),
            None => inflate_entry(&ov.archive_map, "[Content_Types].xml")?,
        },
    };
    let Some(ct_bytes) = ct_bytes else {
        return Err(TurboError::Format("missing [Content_Types].xml".into()));
    };
    let mut ct = String::from_utf8_lossy(&ct_bytes).into_owned();
    let mut add = String::new();
    let lower = ct.to_ascii_lowercase();
    for (ext, mime) in interner.media_defaults() {
        if !lower.contains(&format!("extension=\"{ext}\"")) {
            add.push_str(&format!("<Default Extension=\"{ext}\" ContentType=\"{mime}\"/>"));
        }
    }
    for (path, mime) in overrides {
        add.push_str(&format!("<Override PartName=\"/{path}\" ContentType=\"{mime}\"/>"));
    }
    let Some(ins) = ct.rfind("</Types>") else {
        return Err(TurboError::Format("malformed [Content_Types].xml".into()));
    };
    ct.insert_str(ins, &add);
    rendered.insert("[Content_Types].xml".into(), ct.into_bytes());
    Ok(())
}

#[cfg(feature = "python")]
pub(crate) mod py {
    //! Familiar-API conversion: Python data holders (`kyrax.chart`,
    //! `kyrax.drawing.image`) → batch-writer chart / image models.
    use std::sync::{Arc, Mutex};

    use pyo3::exceptions::{PyNotImplementedError, PyValueError};
    use pyo3::prelude::*;

    use super::{abs_ref, image_pixel_size, sheet_has_drawing, split_data};
    use crate::turbo::overlay::WorkbookOverlay;
    use crate::turbo::scan::parse_ref_range_strict;
    use crate::turbo::write::charts::{Anchor, Chart, ChartType, Grouping, Series};
    use crate::turbo::write::model::{Image, detect_image_format};

    /// Attribute or `None` when missing / Python `None`.
    fn attr<'py>(o: &Bound<'py, PyAny>, name: &str) -> PyResult<Option<Bound<'py, PyAny>>> {
        match o.getattr(name) {
            Ok(v) if !v.is_none() => Ok(Some(v)),
            Ok(_) => Ok(None),
            Err(e) if e.is_instance_of::<pyo3::exceptions::PyAttributeError>(o.py()) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn opt<'py, T: FromPyObjectOwned<'py>>(o: &Bound<'py, PyAny>, name: &str) -> PyResult<Option<T>> {
        attr(o, name)?.map(|v| v.extract::<T>().map_err(Into::into)).transpose()
    }

    /// `Reference` → (sheet title, 1-based inclusive bounds).
    fn reference(r: &Bound<'_, PyAny>) -> PyResult<(String, (u32, u32, u32, u32))> {
        if let Some(rs) = opt::<String>(r, "range_string")? {
            let (sheet, rng) = rs
                .rsplit_once('!')
                .ok_or_else(|| PyValueError::new_err(format!("range_string {rs:?} needs Sheet!A1:B2")))?;
            let sheet = sheet.trim_matches('\'').replace("''", "'");
            let b = parse_ref_range_strict(rng.as_bytes())
                .ok_or_else(|| PyValueError::new_err(format!("bad range {rng:?}")))?;
            return Ok((sheet, b));
        }
        let ws = attr(r, "worksheet")?
            .ok_or_else(|| PyValueError::new_err("Reference needs a worksheet or range_string"))?;
        let sheet: String = match ws.extract::<String>() {
            Ok(s) => s,
            Err(_) => ws.getattr("title")?.extract()?,
        };
        let min_col: u32 = opt(r, "min_col")?.unwrap_or(1);
        let min_row: u32 = opt(r, "min_row")?.unwrap_or(1);
        let max_col: u32 = opt(r, "max_col")?.unwrap_or(min_col);
        let max_row: u32 = opt(r, "max_row")?.unwrap_or(min_row);
        if min_col == 0 || min_row == 0 || max_col < min_col || max_row < min_row {
            return Err(PyValueError::new_err("Reference bounds must be 1-based with max >= min"));
        }
        Ok((sheet, (min_row, min_col, max_row, max_col)))
    }

    fn ref_formula(r: &Bound<'_, PyAny>) -> PyResult<String> {
        let (sheet, (r1, c1, r2, c2)) = reference(r)?;
        Ok(abs_ref(&sheet, r1, c1, r2, c2))
    }

    fn one_cell(cell: &str, width_cm: f64, height_cm: f64) -> PyResult<Anchor> {
        match parse_ref_range_strict(cell.as_bytes()) {
            Some((r1, c1, r2, c2)) if r1 == r2 && c1 == c2 => Ok(Anchor::OneCell {
                cell: cell.replace('$', ""),
                col_off: 0,
                row_off: 0,
                width_cm,
                height_cm,
            }),
            _ => Err(PyValueError::new_err(format!("anchor must be a cell like 'E5', got {cell:?}"))),
        }
    }

    fn chart_from_py(c: &Bound<'_, PyAny>, anchor: Option<&str>) -> PyResult<Chart> {
        let kind: String = opt(c, "_kind")?
            .ok_or_else(|| PyValueError::new_err("add_chart expects a kyrax.chart chart object"))?;
        let type_s = if kind == "bar" { opt::<String>(c, "type")?.unwrap_or_else(|| "col".into()) } else { kind };
        let chart_type = ChartType::parse(&type_s)
            .filter(|t| matches!(t, ChartType::Bar | ChartType::Col | ChartType::Line | ChartType::Pie | ChartType::Scatter | ChartType::Area))
            .ok_or_else(|| PyNotImplementedError::new_err(format!("chart type {type_s:?} is not supported")))?;
        let grouping = match opt::<String>(c, "grouping")? {
            Some(g) => Grouping::parse(&g)
                .ok_or_else(|| PyValueError::new_err(format!("unknown grouping {g:?}")))?,
            None => Grouping::default(),
        };
        let title: Option<String> = match attr(c, "title")? {
            None => None,
            Some(t) => Some(t.extract().map_err(|_| {
                PyNotImplementedError::new_err("chart.title must be a plain string")
            })?),
        };
        let style: Option<u8> = opt(c, "style")?;
        let width: f64 = opt(c, "width")?.unwrap_or(15.0);
        let height: f64 = opt(c, "height")?.unwrap_or(7.5);
        let legend_pos: Option<String> = match attr(c, "legend")? {
            Some(l) => opt(&l, "position")?,
            None => None,
        };
        let scatter = chart_type.is_scatter_family();

        let mut series: Vec<Series> = Vec::new();
        if let Some(data) = attr(c, "_data")? {
            for item in data.try_iter()? {
                let (r, titles, rows): (Bound<'_, PyAny>, bool, bool) = item?.extract()?;
                let (sheet, b) = reference(&r)?;
                for (vals, title_ref) in split_data(&sheet, b, titles, rows) {
                    let mut s = Series { title_ref, ..Series::default() };
                    if scatter { s.y_ref = Some(vals) } else { s.val_ref = Some(vals) }
                    series.push(s);
                }
            }
        }
        if let Some(list) = attr(c, "series")? {
            for s in list.try_iter()? {
                let s = s?;
                let values = attr(&s, "values")?
                    .ok_or_else(|| PyValueError::new_err("Series needs values"))?;
                let (sheet, (r1, c1, r2, c2)) = reference(&values)?;
                let mut out = Series::default();
                let vals = if opt::<bool>(&s, "title_from_data")?.unwrap_or(false) {
                    out.title_ref = Some(abs_ref(&sheet, r1, c1, r1, c1));
                    let (nr, nc) = if r2 > r1 { (r1 + 1, c1) } else { (r1, c1 + 1) };
                    abs_ref(&sheet, nr, nc, r2.max(nr), c2.max(nc))
                } else {
                    abs_ref(&sheet, r1, c1, r2, c2)
                };
                out.title_literal = opt(&s, "title")?;
                let x = attr(&s, "xvalues")?.map(|x| ref_formula(&x)).transpose()?;
                if scatter {
                    out.y_ref = Some(vals);
                    out.x_ref = x;
                } else {
                    out.val_ref = Some(vals);
                    out.cat_ref = x;
                }
                series.push(out);
            }
        }
        if let Some(cats) = attr(c, "_categories")? {
            let f = ref_formula(&cats)?;
            for s in &mut series {
                let slot = if scatter { &mut s.x_ref } else { &mut s.cat_ref };
                if slot.is_none() {
                    *slot = Some(f.clone());
                }
            }
        }
        let cell = match anchor {
            Some(a) => a.to_string(),
            None => opt(c, "anchor")?.unwrap_or_else(|| "E15".into()),
        };
        Ok(Chart {
            chart_type,
            title,
            series,
            anchor: one_cell(&cell, width, height)?,
            style,
            legend_pos,
            grouping,
        })
    }

    fn image_from_py(img: &Bound<'_, PyAny>, anchor: Option<&str>) -> PyResult<Image> {
        let bytes: Vec<u8> = opt(img, "_data")?
            .ok_or_else(|| PyValueError::new_err("add_image expects a kyrax.drawing.image.Image"))?;
        let format = detect_image_format(&bytes)
            .ok_or_else(|| PyValueError::new_err("unsupported image format: expected png, jpeg or gif"))?;
        let (pw, ph) = image_pixel_size(&bytes).unwrap_or((0, 0));
        let w: f64 = opt(img, "width")?.unwrap_or(pw as f64);
        let h: f64 = opt(img, "height")?.unwrap_or(ph as f64);
        if w <= 0.0 || h <= 0.0 {
            return Err(PyValueError::new_err("image width/height must be positive pixels"));
        }
        let cell = match anchor {
            Some(a) => a.to_string(),
            None => opt(img, "anchor")?.unwrap_or_else(|| "A1".into()),
        };
        // 96 dpi pixels → cm (1 px = 9525 EMU).
        let px_cm = 2.54 / 96.0;
        Ok(Image {
            bytes: Arc::from(bytes),
            format,
            anchor: one_cell(&cell, w * px_cm, h * px_cm)?,
        })
    }

    fn lock(ov: &Mutex<WorkbookOverlay>) -> PyResult<std::sync::MutexGuard<'_, WorkbookOverlay>> {
        ov.lock().map_err(|e| PyValueError::new_err(e.to_string()))
    }

    fn refuse_existing_drawing(ov: &WorkbookOverlay, sheet: &str) -> PyResult<()> {
        if sheet_has_drawing(ov, sheet).map_err(|e| PyValueError::new_err(e.to_string()))? {
            return Err(PyNotImplementedError::new_err(format!(
                "sheet '{sheet}' already has charts/images/shapes; adding more to it is not supported yet"
            )));
        }
        Ok(())
    }

    pub(crate) fn add_chart(
        ov: &Mutex<WorkbookOverlay>,
        sheet: &str,
        chart: &Bound<'_, PyAny>,
        anchor: Option<&str>,
    ) -> PyResult<()> {
        let chart = chart_from_py(chart, anchor)?;
        let mut ov = lock(ov)?;
        refuse_existing_drawing(&ov, sheet)?;
        let so = ov.sheet_overlays.entry(sheet.to_string()).or_default();
        so.new_charts.push(chart);
        so.is_dirty = true;
        Ok(())
    }

    pub(crate) fn add_image(
        ov: &Mutex<WorkbookOverlay>,
        sheet: &str,
        img: &Bound<'_, PyAny>,
        anchor: Option<&str>,
    ) -> PyResult<()> {
        let image = image_from_py(img, anchor)?;
        let mut ov = lock(ov)?;
        refuse_existing_drawing(&ov, sheet)?;
        let so = ov.sheet_overlays.entry(sheet.to_string()).or_default();
        so.new_images.push(image);
        so.is_dirty = true;
        Ok(())
    }

    /// `(width, height)` in pixels of PNG / JPEG / GIF bytes.
    #[pyfunction(name = "image_size")]
    pub fn py_image_size(data: &[u8]) -> PyResult<(u32, u32)> {
        image_pixel_size(data)
            .ok_or_else(|| PyValueError::new_err("unsupported image format: expected png, jpeg or gif"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawings_png_and_gif_pixel_size() {
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&300u32.to_be_bytes());
        png.extend_from_slice(&120u32.to_be_bytes());
        assert_eq!(image_pixel_size(&png), Some((300, 120)));
        let gif = b"GIF89a\x40\x01\x20\x00";
        assert_eq!(image_pixel_size(gif), Some((320, 32)));
        // JPEG: SOI, APP0 (len 4), SOF0 with h=50 w=70.
        let jpg = [
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00, 0xFF, 0xC0, 0x00, 0x11, 0x08, 0x00,
            0x32, 0x00, 0x46,
        ];
        assert_eq!(image_pixel_size(&jpg), Some((70, 50)));
        assert_eq!(image_pixel_size(b"nope"), None);
    }

    #[test]
    fn drawings_split_data_titles_from_data() {
        let s = split_data("Data 1", (1, 2, 5, 3), true, false);
        assert_eq!(
            s,
            vec![
                ("'Data 1'!$B$2:$B$5".to_string(), Some("'Data 1'!$B$1".to_string())),
                ("'Data 1'!$C$2:$C$5".to_string(), Some("'Data 1'!$C$1".to_string())),
            ]
        );
        assert_eq!(abs_ref("O'k", 1, 1, 1, 1), "'O''k'!$A$1");
    }

    #[test]
    fn drawings_drawing_element_lands_before_legacy_parts() {
        let mut xml = br#"<worksheet xmlns="x"><sheetData/><pageMargins left="1"/><tableParts count="0"/></worksheet>"#.to_vec();
        ensure_r_namespace(&mut xml);
        splice_tail_control(&mut xml, b"drawing", br#"<drawing r:id="rId1"/>"#, DRAWING_PREDECESSORS, b"</worksheet>");
        let s = String::from_utf8(xml).unwrap();
        assert!(s.contains("xmlns:r="), "{s}");
        assert!(s.contains(r#"<pageMargins left="1"/><drawing r:id="rId1"/><tableParts"#), "{s}");
    }
}
