//! Package + worksheet XML emission and save orchestration (F102).
//! Silo A core + silo B StyleEngine (W2) + silo C structural/charts (W3).

use super::cf_dv::emit_data_validations;
use super::charts::{
    Anchor, ChartsheetSpec, DrawingImage, write_chart_space, write_chartsheet_xml, write_drawing,
    write_drawing_full,
};
use super::media::MediaInterner;
use super::model::*;
use super::pivot::build_pivot_parts;
use super::structural::{
    PKG_REL_NS, collect_defined_names, emit_auto_filter, emit_breaks, emit_default_page_margins,
    emit_defined_names_xml, emit_header_footer, emit_hyperlinks, emit_merges, emit_page_margins,
    emit_page_setup, emit_print_options, emit_scenarios, emit_sheet_protection, root_rels_xml,
    sheet_needs_r_ns, write_app_props, write_comments, write_core_props, write_custom_props,
    write_external_link_part, write_table,
};
use super::style_engine::{StyleDesc, StyleEngine};
use super::xml::*;
use super::zip::{PrecompressedPart, StreamingZipWriter, ZipWriter, compress_part};
use crate::turbo::meta::AutoFilterMeta;
use crate::turbo::range_a1;
use std::cell::RefCell;
use std::io::{self, Seek};

// Reused sheet-XML scratch across parts and successive writes (capacity retained).
// Oversized outliers above SCRATCH_RETAIN_MAX are released so long-lived workers
// do not pin hundreds of MB after a one-off giant workbook.
const SCRATCH_RETAIN_MAX: usize = 64 * 1024 * 1024;

thread_local! {
    static XML_SCRATCH: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

fn take_xml_scratch(min_cap: usize) -> Vec<u8> {
    XML_SCRATCH.with(|cell| {
        let mut v = std::mem::take(&mut *cell.borrow_mut());
        v.clear();
        if v.capacity() < min_cap {
            v.reserve(min_cap.saturating_sub(v.capacity()));
        }
        v
    })
}

fn put_xml_scratch(mut v: Vec<u8>) {
    v.clear();
    XML_SCRATCH.with(|cell| {
        let mut slot = cell.borrow_mut();
        if v.capacity() > SCRATCH_RETAIN_MAX {
            // Release oversized buffer to the OS; do not retain >64MB.
            *slot = Vec::new();
        } else if v.capacity() > slot.capacity() {
            *slot = v;
        }
    });
}

const SHEET_NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const REL_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

static THEME_XML: &str = include_str!("theme_office.xml");

const _: () = {
    let bytes = THEME_XML.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' {
            panic!("theme_office.xml must not contain CRLF (\\r\\n) line endings — canonical LF required for byte determinism");
        }
        i += 1;
    }
};

/// Builtin date format (numFmtId=14). openpyxl `is_date` true on read-back.
const DATE_NUM_FMT: &str = "mm-dd-yy";
/// Builtin datetime format (numFmtId=22).
const DATETIME_NUM_FMT: &str = "m/d/yy h:mm";

/// Global part counters for drawings/charts/tables/comments.
#[derive(Default, Clone, Copy)]
struct PartCounters {
    chart_id: usize,
    drawing_id: usize,
    table_id: usize,
    comment_id: usize,
}

/// SST access during sheet emission: build (serial) or fixed lookup (parallel).
enum SstAccess<'a> {
    Build(&'a mut SstBuilder),
    Fixed(&'a SstBuilder),
}

impl SstAccess<'_> {
    fn index(&mut self, s: &str) -> u32 {
        match self {
            SstAccess::Build(b) => b.intern(s),
            SstAccess::Fixed(b) => b.lookup(s),
        }
    }
}

/// One worksheet's deflated zip parts + content-type metadata (P4 parallel path).
struct SheetZipParts {
    parts: Vec<PrecompressedPart>,
    need_vml: bool,
    ct_overrides: Vec<(String, &'static str)>,
    pivot_wirings: Vec<PivotCacheWiring>,
}

/// Extra package part produced while writing a sheet.
struct ExtraPart {
    path: String,
    data: Vec<u8>,
    content_type: Option<&'static str>,
}

/// Package sidecar parts for a sheet. Sheet XML itself is written into a
/// caller-owned scratch buffer (write-through into the zip).
struct SheetEmit {
    rels: Option<Vec<u8>>,
    extras: Vec<ExtraPart>,
    /// One entry per authored pivot on this sheet: the cache id the pivot parts
    /// were emitted with + the pivot's global part index (for the workbook
    /// `<pivotCaches>` / workbook rels wiring).
    pivot_wirings: Vec<PivotCacheWiring>,
}

/// Deterministic per-pivot assignment: the global part index (drives
/// `pivotCacheDefinitionN.xml` / `pivotTableN.xml` numbering) and the cache id.
#[derive(Clone, Copy)]
struct PivotAssign {
    part_index: usize,
    cache_id: u32,
}

/// A pivot cache that reached the workbook level: its cacheId (referenced from
/// workbook.xml `<pivotCaches>` and the pivot table) and its part index
/// (determines the `pivotCache/pivotCacheDefinitionN.xml` rel target).
struct PivotCacheWiring {
    cache_id: u32,
    part_index: usize,
}

/// Assign a global part index + cache id to every authored pivot, in sheet and
/// spec order, so single- and multi-sheet writes number parts identically.
fn assign_pivot_parts(sheets: &[Sheet]) -> Vec<Vec<PivotAssign>> {
    let mut out = Vec::with_capacity(sheets.len());
    let mut next = 0usize;
    for sheet in sheets {
        let per: Vec<PivotAssign> = sheet
            .pivots
            .iter()
            .map(|_| {
                let a = PivotAssign {
                    part_index: next,
                    cache_id: next as u32,
                };
                next += 1;
                a
            })
            .collect();
        out.push(per);
    }
    out
}

pub fn save_workbook(wb: &Workbook, path: &str) -> io::Result<()> {
    let bytes = write_workbook_bytes(wb)?;
    std::fs::write(path, bytes)
}

pub fn write_workbook_bytes(wb: &Workbook) -> io::Result<Vec<u8>> {
    // Auto-enable structural flags from content without requiring callers to set them.
    let mut wb_local;
    let wb = if needs_auto_features(wb) {
        wb_local = wb.clone();
        wb_local.auto_enable_structural_features();
        &wb_local
    } else {
        wb
    };

    let mut zip = ZipWriter::new();

    let resolved_mode = wb.resolve_string_mode();
    let use_sst = resolved_mode == StringMode::SharedStrings;
    let emit_cache = wb.options.emit_cached_values;
    let features = wb.options.features;

    // Ordering (ledger 19): resolve styles → CF dxfs → styles.xml → sheets.
    let need_resolve = workbook_needs_style_resolve(wb);
    let mut eng = StyleEngine::new();
    let styles_xml: Vec<u8>;
    let mut owned_sheets: Option<Vec<Sheet>> = None;

    if need_resolve {
        let mut sheets = wb.sheets.clone();
        for ns in &wb.named_styles {
            eng.register_named_style(&ns.name, &ns.desc, ns.builtin_id);
        }
        resolve_workbook_styles(&mut eng, &mut sheets, &wb.options);
        for sh in &mut sheets {
            for cf in &mut sh.conditional_formatting {
                cf.register_dxfs(&mut eng);
            }
        }
        styles_xml = eng.emit_styles_xml();
        owned_sheets = Some(sheets);
    } else if !wb.named_styles.is_empty() {
        for ns in &wb.named_styles {
            eng.register_named_style(&ns.name, &ns.desc, ns.builtin_id);
        }
        styles_xml = eng.emit_styles_xml();
    } else {
        styles_xml = eng.emit_styles_xml();
    }

    let sheets_ref: &[Sheet] = owned_sheets.as_deref().unwrap_or(&wb.sheets);

    // Intern all images up front (first-seen order → deterministic media
    // numbering). The registry is then read-only for the parallel sheet path.
    let mut interner = MediaInterner::new();
    if features.contains(WriteFeatures::IMAGES) {
        for sh in sheets_ref {
            for img in &sh.images {
                interner.intern(&img.bytes, img.format);
            }
        }
    }

    // Styles already resolved serially above (W2). SST: pre-build only when
    // multi-sheet parallel emit needs a frozen table; single-sheet builds during
    // emission (no extra scan, no rayon).
    let mut sst = SstBuilder::new();
    let mut counters = PartCounters::default();
    let mut all_ct_overrides: Vec<(String, &'static str)> = Vec::new();
    let mut need_vml_default = false;

    // Counts / names available before sheet emission (same zip part order as before).
    let ws_count = if wb.numeric_columns.is_some() {
        1usize
    } else {
        sheets_ref.len().max(1)
    };
    let cs_count = if features.contains(WriteFeatures::CHARTS) {
        wb.chartsheets.len()
    } else {
        0
    };
    let has_custom = !wb.props.custom.is_empty() && features.contains(WriteFeatures::PROPS);

    // Doc props + theme + styles first (zip local-file order), then stream sheets.
    let titles: Vec<&str> = if let Some(ref g) = wb.numeric_columns {
        vec![g.sheet_name.as_str()]
    } else {
        let mut t: Vec<&str> = sheets_ref.iter().map(|s| s.name.as_str()).collect();
        for cs in &wb.chartsheets {
            t.push(cs.title.as_str());
        }
        t
    };
    let core = if features.contains(WriteFeatures::PROPS) || wb.props.creator.is_some() {
        write_core_props(&wb.props, &wb.creator)
    } else {
        write_core_props(
            &DocProps {
                creator: Some(wb.creator.clone()),
                ..Default::default()
            },
            &wb.creator,
        )
    };
    let app = write_app_props(&wb.props, &titles);
    zip.add("docProps/core.xml", core.as_bytes());
    zip.add("docProps/app.xml", app.as_bytes());
    if has_custom {
        zip.add(
            "docProps/custom.xml",
            write_custom_props(&wb.props.custom).as_bytes(),
        );
        all_ct_overrides.push((
            "/docProps/custom.xml".into(),
            "application/vnd.openxmlformats-officedocument.custom-properties+xml",
        ));
    }

    zip.add("xl/theme/theme1.xml", THEME_XML.as_bytes());
    // Deflate styles and drop uncompressed buffer immediately.
    zip.add_buf("xl/styles.xml", styles_xml);

    // Write-through single-sheet / numeric: one scratch → deflate → clear.
    // Multi-sheet (len > 1): rayon emit+deflate per sheet, then zip append in
    // sheet order (deterministic part order). Peak RAM ≈ threads × sheet XML
    // (rayon default pool) vs serial max(one sheet); see P4_NOTES.
    let mut took_parallel_path = false;
    let mut xml_scratch = take_xml_scratch(0);
    let pivot_assigns = assign_pivot_parts(sheets_ref);
    let mut pivot_wirings: Vec<PivotCacheWiring> = Vec::new();
    if let Some(ref grid) = wb.numeric_columns {
        write_numeric_grid_sheet_into(grid, &mut xml_scratch);
        zip.add_recycle("xl/worksheets/sheet1.xml", &mut xml_scratch);
    } else if sheets_ref.len() <= 1 {
        // Single-sheet: no rayon (avoid thread-pool spin cost).
        for (i, sheet) in sheets_ref.iter().enumerate() {
            let emit = write_sheet_package(
                sheet,
                use_sst,
                emit_cache,
                &mut SstAccess::Build(&mut sst),
                &mut counters,
                features,
                &interner,
                &pivot_assigns[i],
                wb.options.date_iso,
                wb.options.date1904,
                &mut xml_scratch,
            );
            pivot_wirings.extend(emit.pivot_wirings);
            for ex in &emit.extras {
                if ex.path.ends_with(".vml") {
                    need_vml_default = true;
                }
                if let Some(ct) = ex.content_type {
                    all_ct_overrides.push((format!("/{}", ex.path), ct));
                }
            }
            let path = format!("xl/worksheets/sheet{}.xml", i + 1);
            zip.add_recycle(&path, &mut xml_scratch);
            if let Some(mut rels) = emit.rels {
                zip.add_recycle(
                    &format!("xl/worksheets/_rels/sheet{}.xml.rels", i + 1),
                    &mut rels,
                );
            }
            for mut ex in emit.extras {
                zip.add_recycle(&ex.path, &mut ex.data);
            }
        }
    } else {
        took_parallel_path = true;
        // Multi-sheet parallel path — byte-budgeted batches so peak in-flight
        // uncompressed sheet XML is bounded by ~budget regardless of core count.
        if use_sst {
            sst = prebuild_sst(sheets_ref);
        }
        let counter_starts = preassign_counter_starts(sheets_ref, features);
        // Final counters after all worksheets (chartsheets continue from here).
        if let Some(last) = counter_starts.last() {
            let last_sheet = &sheets_ref[sheets_ref.len() - 1];
            counters = *last;
            advance_counters_for_sheet(&mut counters, last_sheet, features);
        }

        const EST_BYTES_PER_CELL: usize = 24;
        const DEFAULT_WRITE_BUDGET_BYTES: usize = 256 * 1024 * 1024;
        let budget = {
            let from_env = std::env::var("KYRAX_WRITE_BUDGET_MB")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .filter(|&mb| mb > 0)
                .map(|mb| mb.saturating_mul(1024 * 1024));
            from_env.unwrap_or(DEFAULT_WRITE_BUDGET_BYTES).max(1)
        };

        // Cheap per-sheet size estimates (cell count × constant; no XML emit).
        let sheet_ests: Vec<usize> = sheets_ref
            .iter()
            .map(|sheet| {
                sheet
                    .rows
                    .iter()
                    .map(|r| r.cells.len())
                    .sum::<usize>()
                    .saturating_mul(EST_BYTES_PER_CELL)
                    + 4096
            })
            .collect();

        // Greedy contiguous batches: add next sheet while batch_bytes + est <= budget.
        // A single sheet with est > budget forms a batch of exactly 1 (serial degrade).
        let mut batches: Vec<(usize, usize)> = Vec::new(); // (start, end) half-open
        {
            let n = sheets_ref.len();
            let mut start = 0usize;
            while start < n {
                let mut end = start;
                let mut batch_bytes = 0usize;
                while end < n {
                    let est = sheet_ests[end];
                    if end > start && batch_bytes.saturating_add(est) > budget {
                        break;
                    }
                    batch_bytes = batch_bytes.saturating_add(est);
                    end += 1;
                    // Oversized first sheet of a batch: always alone (est > budget).
                    if batch_bytes > budget && end == start + 1 {
                        break;
                    }
                }
                // Progress guard: at least one sheet per batch.
                if end == start {
                    end = start + 1;
                }
                batches.push((start, end));
                start = end;
            }
        }

        let date_iso = wb.options.date_iso;
        let date1904 = wb.options.date1904;
        use rayon::prelude::*;
        for (batch_start, batch_end) in batches {
            let indices: Vec<usize> = (batch_start..batch_end).collect();
            let sheet_results: Vec<SheetZipParts> = indices
                .par_iter()
                .map(|&i| {
                    let sheet = &sheets_ref[i];
                    let mut local_counters = counter_starts[i];
                    let mut xml = Vec::new();
                    let emit = write_sheet_package(
                        sheet,
                        use_sst,
                        emit_cache,
                        &mut SstAccess::Fixed(&sst),
                        &mut local_counters,
                        features,
                        &interner,
                        &pivot_assigns[i],
                        date_iso,
                        date1904,
                        &mut xml,
                    );
                    let mut parts =
                        Vec::with_capacity(1 + emit.rels.is_some() as usize + emit.extras.len());
                    let mut need_vml = false;
                    let mut ct_overrides = Vec::new();
                    for ex in &emit.extras {
                        if ex.path.ends_with(".vml") {
                            need_vml = true;
                        }
                        if let Some(ct) = ex.content_type {
                            ct_overrides.push((format!("/{}", ex.path), ct));
                        }
                    }
                    let pivot_wirings = emit.pivot_wirings;
                    parts.push(compress_part(
                        format!("xl/worksheets/sheet{}.xml", i + 1),
                        &xml,
                    ));
                    drop(xml); // free uncompressed sheet ASAP
                    if let Some(rels) = emit.rels {
                        parts.push(compress_part(
                            format!("xl/worksheets/_rels/sheet{}.xml.rels", i + 1),
                            &rels,
                        ));
                    }
                    for ex in emit.extras {
                        parts.push(compress_part(ex.path, &ex.data));
                    }
                    SheetZipParts {
                        parts,
                        need_vml,
                        ct_overrides,
                        pivot_wirings,
                    }
                })
                .collect();

            // Append in ascending sheet index (par_iter preserves index order via
            // indices vec; batches are contiguous ascending ranges).
            for result in sheet_results {
                if result.need_vml {
                    need_vml_default = true;
                }
                all_ct_overrides.extend(result.ct_overrides);
                pivot_wirings.extend(result.pivot_wirings);
                for part in result.parts {
                    zip.add_precompressed(part);
                }
            }
            // sheet_results dropped here before next batch allocates.
        }
    }

    // Chartsheets (F099) — same write-through pattern.
    if features.contains(WriteFeatures::CHARTS) {
        for (ci, cs) in wb.chartsheets.iter().enumerate() {
            let emit = emit_chartsheet(cs, &mut counters, &mut xml_scratch);
            for ex in &emit.extras {
                if let Some(ct) = ex.content_type {
                    all_ct_overrides.push((format!("/{}", ex.path), ct));
                }
            }
            let path = format!("xl/chartsheets/sheet{}.xml", ci + 1);
            zip.add_recycle(&path, &mut xml_scratch);
            if let Some(mut rels) = emit.rels {
                zip.add_recycle(
                    &format!("xl/chartsheets/_rels/sheet{}.xml.rels", ci + 1),
                    &mut rels,
                );
            }
            for mut ex in emit.extras {
                zip.add_recycle(&ex.path, &mut ex.data);
            }
            all_ct_overrides.push((
                format!("/{path}"),
                "application/vnd.openxmlformats-officedocument.spreadsheetml.chartsheet+xml",
            ));
        }
    }
    put_xml_scratch(xml_scratch);

    // Media parts: one per unique image, STORE (never deflate — PNG/JPEG/GIF
    // are already compressed). Global parts, emitted once in dedup order.
    for i in 0..interner.len() {
        let name = interner.media_part_name(i);
        zip.add_stored(&name, interner.media_bytes(i));
    }

    // External links (F100 thin stub)
    for (i, link) in wb.external_links.iter().enumerate() {
        let n = i + 1;
        let path = format!("xl/externalLinks/externalLink{n}.xml");
        let el_xml = write_external_link_part();
        let el_rels = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="{PKG_REL_NS}"><Relationship Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/externalLinkPath" Target="{}" TargetMode="External" Id="rId1"/></Relationships>"#,
            escape_attr_simple(&link.target)
        );
        all_ct_overrides.push((
            format!("/{path}"),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.externalLink+xml",
        ));
        zip.add(&path, el_xml.as_bytes());
        zip.add(
            &format!("xl/externalLinks/_rels/externalLink{n}.xml.rels"),
            el_rels.as_bytes(),
        );
    }

    let has_sst = use_sst && !sst.is_empty();
    if has_sst {
        zip.add_buf("xl/sharedStrings.xml", write_sst(&sst));
    }

    // Workbook-level pivot wiring: after sheets/chartsheets/extlinks/styles/
    // theme (+sst) the pivot-cache relationships get the next contiguous rIds.
    // workbook.xml's `<pivotCaches>` and workbook.xml.rels must agree.
    let pivot_rel_base = ws_count + cs_count + wb.external_links.len() + (has_sst as usize) + 2;
    let pivot_rel_ids: Vec<String> = pivot_wirings
        .iter()
        .enumerate()
        .map(|(i, _)| format!("rId{}", pivot_rel_base + i + 1))
        .collect();

    let sheet_names_states: Vec<(String, SheetState)> = if let Some(g) = &wb.numeric_columns {
        vec![(g.sheet_name.clone(), SheetState::Visible)]
    } else {
        sheets_ref
            .iter()
            .map(|s| (s.name.clone(), s.state))
            .collect()
    };
    let chartsheet_names: Vec<String> = wb.chartsheets.iter().map(|c| c.title.clone()).collect();

    zip.add_buf(
        "xl/workbook.xml",
        write_workbook_xml(
            wb,
            &sheet_names_states,
            &chartsheet_names,
            features,
            &pivot_wirings,
            &pivot_rel_ids,
        ),
    );
    zip.add_buf(
        "xl/_rels/workbook.xml.rels",
        write_workbook_rels(
            ws_count,
            cs_count,
            wb.external_links.len(),
            has_sst,
            &pivot_wirings,
            &pivot_rel_ids,
        ),
    );
    zip.add("_rels/.rels", root_rels_xml(has_custom).as_bytes());
    zip.add_buf(
        "[Content_Types].xml",
        write_content_types(
            ws_count,
            cs_count,
            has_sst,
            wb.macro_enabled,
            need_vml_default,
            &all_ct_overrides,
            &interner.media_defaults(),
        ),
    );

    // F101 VBA preserve: deferred (no zip-read dep for copy-if-present in this merge).
    let _ = &wb.vba_archive_path;

    let bytes = zip.finish()?;

    // Release oversized main-thread COMP_SCRATCH after this write (XML_SCRATCH
    // is handled by put_xml_scratch above when capacity exceeds retain max).
    super::zip::shrink_comp_scratch();
    // Only broadcast to rayon workers when the multi-sheet parallel path ran;
    // single-sheet / numeric writes never touch worker COMP_SCRATCH.
    if took_parallel_path {
        rayon::broadcast(|_| super::zip::shrink_comp_scratch());
    }

    Ok(bytes)
}

const METHOD_DEFLATE: u16 = 8;

fn stream_add_entry<W: io::Write + Seek>(
    zip: &mut StreamingZipWriter<W>,
    name: &str,
    data: &[u8],
) -> io::Result<()> {
    zip.start_entry(name, METHOD_DEFLATE)?;
    zip.write_chunk(data)?;
    zip.finish_entry()
}

/// Stream an already-compressed part (images) with the STORE method — the
/// chunk passes straight through to the sink, never through deflate.
fn stream_add_stored<W: io::Write + Seek>(
    zip: &mut StreamingZipWriter<W>,
    name: &str,
    data: &[u8],
) -> io::Result<()> {
    zip.start_entry(name, METHOD_STORE)?;
    zip.write_chunk(data)?;
    zip.finish_entry()
}

const METHOD_STORE: u16 = 0;

pub fn save_workbook_stream<W: io::Write + Seek>(wb: &Workbook, w: W) -> io::Result<W> {
    let mut wb_local;
    let wb = if needs_auto_features(wb) {
        wb_local = wb.clone();
        wb_local.auto_enable_structural_features();
        &wb_local
    } else {
        wb
    };

    let mut zip = StreamingZipWriter::new(w);

    let resolved_mode = wb.resolve_string_mode();
    let use_sst = resolved_mode == StringMode::SharedStrings;
    let emit_cache = wb.options.emit_cached_values;
    let features = wb.options.features;

    let need_resolve = workbook_needs_style_resolve(wb);
    let mut eng = StyleEngine::new();
    let styles_xml: Vec<u8>;
    let mut owned_sheets: Option<Vec<Sheet>> = None;

    if need_resolve {
        let mut sheets = wb.sheets.clone();
        for ns in &wb.named_styles {
            eng.register_named_style(&ns.name, &ns.desc, ns.builtin_id);
        }
        resolve_workbook_styles(&mut eng, &mut sheets, &wb.options);
        for sh in &mut sheets {
            for cf in &mut sh.conditional_formatting {
                cf.register_dxfs(&mut eng);
            }
        }
        styles_xml = eng.emit_styles_xml();
        owned_sheets = Some(sheets);
    } else if !wb.named_styles.is_empty() {
        for ns in &wb.named_styles {
            eng.register_named_style(&ns.name, &ns.desc, ns.builtin_id);
        }
        styles_xml = eng.emit_styles_xml();
    } else {
        styles_xml = eng.emit_styles_xml();
    }

    let sheets_ref: &[Sheet] = owned_sheets.as_deref().unwrap_or(&wb.sheets);

    // Intern all images up front (first-seen order → deterministic media
    // numbering), same as the buffered path.
    let mut interner = MediaInterner::new();
    if features.contains(WriteFeatures::IMAGES) {
        for sh in sheets_ref {
            for img in &sh.images {
                interner.intern(&img.bytes, img.format);
            }
        }
    }

    let mut sst = SstBuilder::new();
    let mut counters = PartCounters::default();
    let mut all_ct_overrides: Vec<(String, &'static str)> = Vec::new();
    let mut need_vml_default = false;

    let ws_count = if wb.numeric_columns.is_some() {
        1usize
    } else {
        sheets_ref.len().max(1)
    };
    let cs_count = if features.contains(WriteFeatures::CHARTS) {
        wb.chartsheets.len()
    } else {
        0
    };
    let has_custom = !wb.props.custom.is_empty() && features.contains(WriteFeatures::PROPS);

    let titles: Vec<&str> = if let Some(ref g) = wb.numeric_columns {
        vec![g.sheet_name.as_str()]
    } else {
        let mut t: Vec<&str> = sheets_ref.iter().map(|s| s.name.as_str()).collect();
        for cs in &wb.chartsheets {
            t.push(cs.title.as_str());
        }
        t
    };
    let core = if features.contains(WriteFeatures::PROPS) || wb.props.creator.is_some() {
        write_core_props(&wb.props, &wb.creator)
    } else {
        write_core_props(
            &DocProps {
                creator: Some(wb.creator.clone()),
                ..Default::default()
            },
            &wb.creator,
        )
    };
    let app = write_app_props(&wb.props, &titles);
    stream_add_entry(&mut zip, "docProps/core.xml", core.as_bytes())?;
    stream_add_entry(&mut zip, "docProps/app.xml", app.as_bytes())?;
    if has_custom {
        stream_add_entry(
            &mut zip,
            "docProps/custom.xml",
            write_custom_props(&wb.props.custom).as_bytes(),
        )?;
        all_ct_overrides.push((
            "/docProps/custom.xml".into(),
            "application/vnd.openxmlformats-officedocument.custom-properties+xml",
        ));
    }

    stream_add_entry(&mut zip, "xl/theme/theme1.xml", THEME_XML.as_bytes())?;
    stream_add_entry(&mut zip, "xl/styles.xml", &styles_xml)?;

    let mut xml_scratch = take_xml_scratch(0);
    let pivot_assigns = assign_pivot_parts(sheets_ref);
    let mut pivot_wirings: Vec<PivotCacheWiring> = Vec::new();
    if let Some(ref grid) = wb.numeric_columns {
        zip.start_entry("xl/worksheets/sheet1.xml", METHOD_DEFLATE)?;
        write_numeric_grid_sheet_stream(&mut zip, grid, &mut xml_scratch)?;
        zip.finish_entry()?;
    } else {
        for (i, sheet) in sheets_ref.iter().enumerate() {
            let path = format!("xl/worksheets/sheet{}.xml", i + 1);
            let emit = write_sheet_package_stream(
                &mut zip,
                &path,
                sheet,
                use_sst,
                emit_cache,
                &mut SstAccess::Build(&mut sst),
                &mut counters,
                features,
                &interner,
                &pivot_assigns[i],
                wb.options.date_iso,
                wb.options.date1904,
                &mut xml_scratch,
            )?;
            pivot_wirings.extend(emit.pivot_wirings);
            for ex in &emit.extras {
                if ex.path.ends_with(".vml") {
                    need_vml_default = true;
                }
                if let Some(ct) = ex.content_type {
                    all_ct_overrides.push((format!("/{}", ex.path), ct));
                }
            }
            if let Some(rels) = emit.rels {
                stream_add_entry(
                    &mut zip,
                    &format!("xl/worksheets/_rels/sheet{}.xml.rels", i + 1),
                    &rels,
                )?;
            }
            for ex in emit.extras {
                stream_add_entry(&mut zip, &ex.path, &ex.data)?;
            }
        }
    }

    if features.contains(WriteFeatures::CHARTS) {
        for (ci, cs) in wb.chartsheets.iter().enumerate() {
            let emit = emit_chartsheet(cs, &mut counters, &mut xml_scratch);
            for ex in &emit.extras {
                if let Some(ct) = ex.content_type {
                    all_ct_overrides.push((format!("/{}", ex.path), ct));
                }
            }
            let path = format!("xl/chartsheets/sheet{}.xml", ci + 1);
            stream_add_entry(&mut zip, &path, &xml_scratch)?;
            if let Some(rels) = emit.rels {
                stream_add_entry(
                    &mut zip,
                    &format!("xl/chartsheets/_rels/sheet{}.xml.rels", ci + 1),
                    &rels,
                )?;
            }
            for ex in emit.extras {
                stream_add_entry(&mut zip, &ex.path, &ex.data)?;
            }
            all_ct_overrides.push((
                format!("/{path}"),
                "application/vnd.openxmlformats-officedocument.spreadsheetml.chartsheet+xml",
            ));
        }
    }
    put_xml_scratch(xml_scratch);

    // Media parts: STORE only (never deflate), in dedup order.
    for i in 0..interner.len() {
        stream_add_stored(
            &mut zip,
            &interner.media_part_name(i),
            interner.media_bytes(i),
        )?;
    }

    for (i, link) in wb.external_links.iter().enumerate() {
        let n = i + 1;
        let path = format!("xl/externalLinks/externalLink{n}.xml");
        let el_xml = write_external_link_part();
        let el_rels = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="{PKG_REL_NS}"><Relationship Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/externalLinkPath" Target="{}" TargetMode="External" Id="rId1"/></Relationships>"#,
            escape_attr_simple(&link.target)
        );
        all_ct_overrides.push((
            format!("/{path}"),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.externalLink+xml",
        ));
        stream_add_entry(&mut zip, &path, el_xml.as_bytes())?;
        stream_add_entry(
            &mut zip,
            &format!("xl/externalLinks/_rels/externalLink{n}.xml.rels"),
            el_rels.as_bytes(),
        )?;
    }

    let has_sst = use_sst && !sst.is_empty();
    if has_sst {
        stream_add_entry(&mut zip, "xl/sharedStrings.xml", &write_sst(&sst))?;
    }

    // Workbook-level pivot wiring: sheets/chartsheets/extlinks/styles/theme
    // (+sst) precede the pivot-cache relationships.
    let pivot_rel_base = ws_count + cs_count + wb.external_links.len() + (has_sst as usize) + 2;
    let pivot_rel_ids: Vec<String> = pivot_wirings
        .iter()
        .enumerate()
        .map(|(i, _)| format!("rId{}", pivot_rel_base + i + 1))
        .collect();

    let sheet_names_states: Vec<(String, SheetState)> = if let Some(g) = &wb.numeric_columns {
        vec![(g.sheet_name.clone(), SheetState::Visible)]
    } else {
        sheets_ref
            .iter()
            .map(|s| (s.name.clone(), s.state))
            .collect()
    };
    let chartsheet_names: Vec<String> = wb.chartsheets.iter().map(|c| c.title.clone()).collect();

    stream_add_entry(
        &mut zip,
        "xl/workbook.xml",
        &write_workbook_xml(
            wb,
            &sheet_names_states,
            &chartsheet_names,
            features,
            &pivot_wirings,
            &pivot_rel_ids,
        ),
    )?;
    stream_add_entry(
        &mut zip,
        "xl/_rels/workbook.xml.rels",
        &write_workbook_rels(
            ws_count,
            cs_count,
            wb.external_links.len(),
            has_sst,
            &pivot_wirings,
            &pivot_rel_ids,
        ),
    )?;
    stream_add_entry(
        &mut zip,
        "_rels/.rels",
        root_rels_xml(has_custom).as_bytes(),
    )?;
    stream_add_entry(
        &mut zip,
        "[Content_Types].xml",
        &write_content_types(
            ws_count,
            cs_count,
            has_sst,
            wb.macro_enabled,
            need_vml_default,
            &all_ct_overrides,
            &interner.media_defaults(),
        ),
    )?;

    let finished = zip.finish()?;
    super::zip::shrink_comp_scratch();
    Ok(finished)
}

/// Emit `<autoFilter>`. The `ref` is always written when the model carries it;
/// the `filterColumn` children only under the MERGES feature family, so a
/// values-only workload (flag off) pays nothing extra.
fn emit_sheet_auto_filter(out: &mut Vec<u8>, af: &AutoFilterMeta, features: WriteFeatures) {
    let with_cols =
        features.contains(WriteFeatures::MERGES) || features.contains(WriteFeatures::ALL);
    let cols: &[crate::turbo::meta::FilterColumnMeta] = if with_cols { &af.columns } else { &[] };
    emit_auto_filter(out, &range_a1(&af.ref_), cols);
}

#[allow(clippy::too_many_arguments)]
fn write_sheet_package_stream<W: io::Write + Seek>(
    zip: &mut StreamingZipWriter<W>,
    sheet_path: &str,
    sheet: &Sheet,
    use_sst: bool,
    emit_cache: bool,
    sst: &mut SstAccess<'_>,
    counters: &mut PartCounters,
    features: WriteFeatures,
    interner: &MediaInterner,
    pivot_assigns: &[PivotAssign],
    date_iso: bool,
    date1904: bool,
    chunk_buf: &mut Vec<u8>,
) -> io::Result<SheetEmit> {
    zip.start_entry(sheet_path, METHOD_DEFLATE)?;

    let need_r = sheet_needs_r_ns(sheet);
    chunk_buf.clear();
    write_sheet_open(chunk_buf, need_r);

    push(chunk_buf, b"<sheetPr");
    if let Some(ref cn) = sheet.code_name {
        push(chunk_buf, br#" codeName=""#);
        write_escaped_text(chunk_buf, cn);
        push(chunk_buf, b"\"");
    }
    push(chunk_buf, b">");
    if let Some(rgb) = &sheet.tab_color_rgb {
        let rgb = if rgb.len() == 6 {
            format!("00{rgb}")
        } else {
            rgb.clone()
        };
        push(chunk_buf, br#"<tabColor rgb=""#);
        write_escaped_attr(chunk_buf, &rgb);
        push(chunk_buf, br#""/>"#);
    }
    push(
        chunk_buf,
        br#"<outlinePr summaryBelow="1" summaryRight="1"/>"#,
    );
    if sheet
        .page_setup
        .as_ref()
        .map(|p| p.fit_to_page)
        .unwrap_or(false)
    {
        push(chunk_buf, br#"<pageSetUpPr fitToPage="1"/>"#);
    } else {
        push(chunk_buf, br#"<pageSetUpPr/>"#);
    }
    push(chunk_buf, b"</sheetPr>");

    let dim = if let Some(ref d) = sheet.dimension {
        d.clone()
    } else if let Some((r0, c0, r1, c1)) = sheet.bounds() {
        dimension_ref(r0, c0, r1, c1)
    } else {
        "A1".into()
    };
    push(chunk_buf, br#"<dimension ref=""#);
    push(chunk_buf, dim.as_bytes());
    push(chunk_buf, br#""/>"#);

    push(chunk_buf, br#"<sheetViews><sheetView workbookViewId="0""#);
    if let Some(ref freeze) = sheet.view.freeze_cell {
        if let Some((fr, fc)) = parse_a1(freeze) {
            let y_split = fr.saturating_sub(1);
            let x_split = fc.saturating_sub(1);
            if y_split > 0 || x_split > 0 {
                push(chunk_buf, br#"><pane"#);
                if x_split > 0 {
                    push(chunk_buf, br#" xSplit=""#);
                    write_u32(chunk_buf, x_split);
                    push(chunk_buf, b"\"");
                }
                if y_split > 0 {
                    push(chunk_buf, br#" ySplit=""#);
                    write_u32(chunk_buf, y_split);
                    push(chunk_buf, b"\"");
                }
                let active = if x_split > 0 && y_split > 0 {
                    "bottomRight"
                } else if y_split > 0 {
                    "bottomLeft"
                } else {
                    "topRight"
                };
                push(chunk_buf, br#" topLeftCell=""#);
                write_escaped_text(chunk_buf, freeze);
                push(chunk_buf, br#"" activePane=""#);
                push_str(chunk_buf, active);
                push(chunk_buf, br#"" state="frozen"/>"#);
                if x_split > 0 && y_split > 0 {
                    push(
                        chunk_buf,
                        br#"<selection pane="topRight"/><selection pane="bottomLeft"/><selection pane="bottomRight" activeCell="A1" sqref="A1"/>"#,
                    );
                } else if y_split > 0 {
                    push(
                        chunk_buf,
                        br#"<selection pane="bottomLeft" activeCell="A1" sqref="A1"/>"#,
                    );
                } else {
                    push(
                        chunk_buf,
                        br#"<selection pane="topRight" activeCell="A1" sqref="A1"/>"#,
                    );
                }
                push(chunk_buf, br#"</sheetView>"#);
            } else {
                push(
                    chunk_buf,
                    br#"><selection activeCell="A1" sqref="A1"/></sheetView>"#,
                );
            }
        } else {
            push(
                chunk_buf,
                br#"><selection activeCell="A1" sqref="A1"/></sheetView>"#,
            );
        }
    } else {
        push(
            chunk_buf,
            br#"><selection activeCell="A1" sqref="A1"/></sheetView>"#,
        );
    }
    push(chunk_buf, br#"</sheetViews>"#);

    push(chunk_buf, br#"<sheetFormatPr baseColWidth=""#);
    write_u32(chunk_buf, sheet.base_col_width);
    push(chunk_buf, br#"" defaultRowHeight=""#);
    write_f64(chunk_buf, sheet.default_row_height);
    if let Some(w) = sheet.default_col_width {
        push(chunk_buf, br#"" defaultColWidth=""#);
        write_f64(chunk_buf, w);
    }
    push(chunk_buf, br#""/>"#);

    if !sheet.cols.is_empty() {
        push(chunk_buf, b"<cols>");
        for col in &sheet.cols {
            push(chunk_buf, br#"<col min=""#);
            write_u32(chunk_buf, col.min);
            push(chunk_buf, br#"" max=""#);
            write_u32(chunk_buf, col.max);
            if let Some(w) = col.width {
                push(chunk_buf, br#"" width=""#);
                write_f64(chunk_buf, w);
                if col.custom_width {
                    push(chunk_buf, br#"" customWidth="1"#);
                }
            }
            if col.hidden {
                push(chunk_buf, br#"" hidden="1"#);
            }
            if let Some(s) = col.style {
                if s != 0 {
                    push(chunk_buf, br#"" style=""#);
                    write_u32(chunk_buf, s);
                }
            }
            if col.best_fit {
                push(chunk_buf, br#"" bestFit="1"#);
            }
            if col.outline_level > 0 {
                push(chunk_buf, br#"" outlineLevel=""#);
                write_u32(chunk_buf, col.outline_level as u32);
            }
            push(chunk_buf, br#""/>"#);
        }
        push(chunk_buf, b"</cols>");
    }

    push(chunk_buf, b"<sheetData>");
    zip.write_chunk(chunk_buf)?;
    chunk_buf.clear();

    for row in &sheet.rows {
        write_row(chunk_buf, row, use_sst, emit_cache, sst, date_iso, date1904);
        if !chunk_buf.is_empty() {
            zip.write_chunk(chunk_buf)?;
            chunk_buf.clear();
        }
    }

    push(chunk_buf, b"</sheetData>");

    let mut rels: Vec<(String, String, Option<String>)> = Vec::new();
    let mut next_rid: usize = 0;
    let mut extras: Vec<ExtraPart> = Vec::new();
    let mut pivot_wirings: Vec<PivotCacheWiring> = Vec::new();

    let do_merges =
        features.contains(WriteFeatures::MERGES) || features.contains(WriteFeatures::ALL);
    if let Some(prot) = &sheet.protection {
        emit_sheet_protection(chunk_buf, prot);
    }
    if !sheet.scenarios.is_empty() {
        emit_scenarios(chunk_buf, &sheet.scenarios);
    }
    if let Some(af) = &sheet.auto_filter {
        emit_sheet_auto_filter(chunk_buf, af, features);
    }
    if !sheet.merges.is_empty() {
        let _ = do_merges;
        emit_merges(chunk_buf, &sheet.merges);
    }

    for cf in &sheet.conditional_formatting {
        cf.emit(chunk_buf);
    }
    emit_data_validations(&sheet.data_validations, chunk_buf);

    if !sheet.hyperlinks.is_empty() {
        let hl_rels = emit_hyperlinks(chunk_buf, &sheet.hyperlinks, &mut next_rid);
        rels.extend(hl_rels);
    }

    if let Some(po) = &sheet.print_options {
        emit_print_options(chunk_buf, po);
    }
    if let Some(m) = &sheet.page_margins {
        emit_page_margins(chunk_buf, m);
    } else {
        emit_default_page_margins(chunk_buf);
    }
    if let Some(ps) = &sheet.page_setup {
        emit_page_setup(chunk_buf, ps);
    }
    if let Some(hf) = &sheet.header_footer {
        emit_header_footer(chunk_buf, hf);
    }
    emit_breaks(chunk_buf, &sheet.row_breaks, &sheet.col_breaks);

    let has_charts = !sheet.charts.is_empty() && features.contains(WriteFeatures::CHARTS);
    let has_images = !sheet.images.is_empty() && features.contains(WriteFeatures::IMAGES);
    if has_charts || has_images {
        counters.drawing_id += 1;
        let did = counters.drawing_id;
        let mut chart_rel_paths = Vec::new();
        if has_charts {
            for ch in &sheet.charts {
                counters.chart_id += 1;
                let cid = counters.chart_id;
                extras.push(ExtraPart {
                    path: format!("xl/charts/chart{cid}.xml"),
                    data: write_chart_space(ch).into_bytes(),
                    content_type: Some(
                        "application/vnd.openxmlformats-officedocument.drawingml.chart+xml",
                    ),
                });
                chart_rel_paths.push(format!("../charts/chart{cid}.xml"));
            }
        }
        let (drawing_images, media_targets) = build_drawing_images(sheet, interner);
        let (drawing_xml, drawing_rels) = write_drawing_full(
            &sheet.charts,
            &chart_rel_paths,
            &drawing_images,
            &media_targets,
        );
        extras.push(ExtraPart {
            path: format!("xl/drawings/drawing{did}.xml"),
            data: drawing_xml.into_bytes(),
            content_type: Some("application/vnd.openxmlformats-officedocument.drawing+xml"),
        });
        extras.push(ExtraPart {
            path: format!("xl/drawings/_rels/drawing{did}.xml.rels"),
            data: drawing_rels.into_bytes(),
            content_type: None,
        });
        next_rid += 1;
        let id = format!("rId{next_rid}");
        rels.push((
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing".into(),
            format!("/xl/drawings/drawing{did}.xml"),
            None,
        ));
        push(chunk_buf, br#"<drawing r:id=""#);
        push_str(chunk_buf, &id);
        push(chunk_buf, br#""/>"#);
    }

    if !sheet.comments.is_empty() && features.contains(WriteFeatures::COMMENTS) {
        counters.comment_id += 1;
        let cid = counters.comment_id;
        let (comments_xml, vml) = write_comments(&sheet.comments);
        extras.push(ExtraPart {
            path: format!("xl/comments/comment{cid}.xml"),
            data: comments_xml.into_bytes(),
            content_type: Some(
                "application/vnd.openxmlformats-officedocument.spreadsheetml.comments+xml",
            ),
        });
        extras.push(ExtraPart {
            path: format!("xl/drawings/commentsDrawing{cid}.vml"),
            data: vml.into_bytes(),
            content_type: None,
        });
        push(chunk_buf, br#"<legacyDrawing r:id="anysvml"/>"#);
    }

    if !sheet.tables.is_empty() && features.contains(WriteFeatures::TABLES) {
        push(chunk_buf, br#"<tableParts count=""#);
        write_u32(chunk_buf, sheet.tables.len() as u32);
        push(chunk_buf, b"\">");
        for t in &sheet.tables {
            counters.table_id += 1;
            let tid = counters.table_id;
            extras.push(ExtraPart {
                path: format!("xl/tables/table{tid}.xml"),
                data: write_table(t, tid).into_bytes(),
                content_type: Some(
                    "application/vnd.openxmlformats-officedocument.spreadsheetml.table+xml",
                ),
            });
            next_rid += 1;
            let id = format!("rId{next_rid}");
            rels.push((
                "http://schemas.openxmlformats.org/officeDocument/2006/relationships/table".into(),
                format!("/xl/tables/table{tid}.xml"),
                None,
            ));
            push(chunk_buf, br#"<tablePart r:id=""#);
            push_str(chunk_buf, &id);
            push(chunk_buf, br#""/>"#);
        }
        push(chunk_buf, b"</tableParts>");
    }

    // Pivots (Task B5b): same parts as the buffered path, written through the
    // streaming sink via `extras`; the sheet rel entry carries the pivot table.
    if !sheet.pivots.is_empty() && features.contains(WriteFeatures::PIVOTS) {
        for (pivot, assign) in sheet.pivots.iter().zip(pivot_assigns.iter()) {
            let parts = build_pivot_parts(sheet, pivot, assign.cache_id, assign.part_index);
            let Some(parts) = parts else {
                continue;
            };
            for (path, data) in parts.parts {
                let content_type = parts
                    .content_types
                    .iter()
                    .find(|(p, _)| *p == format!("/{path}"))
                    .map(|(_, c)| *c);
                extras.push(ExtraPart {
                    path,
                    data,
                    content_type,
                });
            }
            // No `next_rid += 1` here: this is the last relationship of
            // the loop body and nothing reads the counter again.
            rels.push((
                "http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotTable"
                    .into(),
                format!("/{}", parts.table_part),
                None,
            ));
            pivot_wirings.push(PivotCacheWiring {
                cache_id: parts.cache_id,
                part_index: parts.part_index,
            });
        }
    }

    push(chunk_buf, b"</worksheet>");

    let mut rels_xml: Option<Vec<u8>> = None;
    if !rels.is_empty()
        || (!sheet.comments.is_empty() && features.contains(WriteFeatures::COMMENTS))
    {
        let mut r = Vec::new();
        push(
            &mut r,
            br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
        );
        for (i, (ty, target, mode)) in rels.iter().enumerate() {
            let id = i + 1;
            push(&mut r, br#"<Relationship Id="rId"#);
            write_u32(&mut r, id as u32);
            push(&mut r, br#"" Type=""#);
            push_str(&mut r, ty);
            push(&mut r, br#"" Target=""#);
            write_escaped_attr(&mut r, target);
            push(&mut r, b"\"");
            if let Some(m) = mode {
                push(&mut r, br#" TargetMode=""#);
                push_str(&mut r, m);
                push(&mut r, b"\"");
            }
            push(&mut r, br#"/>"#);
        }
        if !sheet.comments.is_empty() && features.contains(WriteFeatures::COMMENTS) {
            let cid = counters.comment_id;
            push(
                &mut r,
                br#"<Relationship Id="comments" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="/xl/comments/comment"#,
            );
            write_u32(&mut r, cid as u32);
            push(&mut r, br#".xml"/>"#);
            push(
                &mut r,
                br#"<Relationship Id="anysvml" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/vmlDrawing" Target="/xl/drawings/commentsDrawing"#,
            );
            write_u32(&mut r, cid as u32);
            push(&mut r, br#".vml"/>"#);
        }
        push(&mut r, b"</Relationships>");
        rels_xml = Some(r);
    }

    if !chunk_buf.is_empty() {
        zip.write_chunk(chunk_buf)?;
        chunk_buf.clear();
    }

    zip.finish_entry()?;

    Ok(SheetEmit {
        rels: rels_xml,
        extras,
        pivot_wirings,
    })
}

fn write_numeric_grid_sheet_stream<W: io::Write + Seek>(
    zip: &mut StreamingZipWriter<W>,
    grid: &NumericGrid,
    chunk_buf: &mut Vec<u8>,
) -> io::Result<()> {
    chunk_buf.clear();
    write_sheet_open(chunk_buf, false);
    push(
        chunk_buf,
        br#"<sheetPr><outlinePr summaryBelow="1" summaryRight="1"/><pageSetUpPr/></sheetPr>"#,
    );
    push(chunk_buf, br#"<dimension ref=""#);
    let nrows = grid.nrows;
    let ncols = grid.ncols;
    if nrows > 0 && ncols > 0 {
        let dim = dimension_ref(1, 1, nrows, ncols);
        push(chunk_buf, dim.as_bytes());
    } else {
        push(chunk_buf, b"A1");
    }
    push(chunk_buf, br#""/>"#);
    push(
        chunk_buf,
        br#"<sheetViews><sheetView workbookViewId="0"><selection activeCell="A1" sqref="A1"/></sheetView></sheetViews>"#,
    );
    push(
        chunk_buf,
        br#"<sheetFormatPr baseColWidth="8" defaultRowHeight="15"/>"#,
    );
    push(chunk_buf, b"<sheetData>");
    zip.write_chunk(chunk_buf)?;
    chunk_buf.clear();

    let vals = &grid.values;
    let mut coord_buf = [0u8; 4];
    for r in 0..nrows {
        push(chunk_buf, br#"<row r=""#);
        write_u32(chunk_buf, r + 1);
        push(chunk_buf, br#"">"#);
        let base = (r as usize) * (ncols as usize);
        for c in 0..ncols {
            let v = vals[base + c as usize];
            if v.is_nan() {
                continue;
            }
            push(chunk_buf, br#"<c r=""#);
            let letters = col_letters(c + 1, &mut coord_buf);
            chunk_buf.extend_from_slice(letters);
            write_u32(chunk_buf, r + 1);
            push(chunk_buf, br#""><v>"#);
            write_f64(chunk_buf, v);
            push(chunk_buf, br#"</v></c>"#);
        }
        push(chunk_buf, b"</row>");
        if !chunk_buf.is_empty() {
            zip.write_chunk(chunk_buf)?;
            chunk_buf.clear();
        }
    }

    push(
        chunk_buf,
        br#"</sheetData><pageMargins left="0.75" right="0.75" top="1" bottom="1" header="0.5" footer="0.5"/></worksheet>"#,
    );
    if !chunk_buf.is_empty() {
        zip.write_chunk(chunk_buf)?;
        chunk_buf.clear();
    }
    Ok(())
}

fn needs_auto_features(wb: &Workbook) -> bool {
    wb.sheets.iter().any(|s| {
        !s.merges.is_empty()
            || !s.hyperlinks.is_empty()
            || !s.comments.is_empty()
            || !s.tables.is_empty()
            || !s.charts.is_empty()
            || !s.images.is_empty()
            || !s.pivots.is_empty()
            || s.protection.is_some()
            || s.auto_filter.is_some()
            || s.tab_color_rgb.is_some()
            || s.print_area.is_some()
            || s.page_setup.is_some()
            || s.page_margins.is_some()
    }) || !wb.defined_names.is_empty()
        || !wb.chartsheets.is_empty()
        || !wb.external_links.is_empty()
        || wb.lock_structure
        || !wb.props.custom.is_empty()
        || wb.props.title.is_some()
}

fn escape_attr_simple(s: &str) -> String {
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

fn emit_chartsheet(
    cs: &ChartsheetSpec,
    counters: &mut PartCounters,
    out: &mut Vec<u8>,
) -> SheetEmit {
    counters.drawing_id += 1;
    let did = counters.drawing_id;
    let mut extras = Vec::new();
    let mut chart_paths = Vec::new();
    let mut charts_abs = Vec::new();
    for ch in &cs.charts {
        counters.chart_id += 1;
        let cid = counters.chart_id;
        let path = format!("xl/charts/chart{cid}.xml");
        chart_paths.push(format!("../charts/chart{cid}.xml"));
        extras.push(ExtraPart {
            path,
            data: write_chart_space(ch).into_bytes(),
            content_type: Some("application/vnd.openxmlformats-officedocument.drawingml.chart+xml"),
        });
        let mut c = ch.clone();
        c.anchor = Anchor::Absolute {
            x_emu: 0,
            y_emu: 0,
            cx_emu: 5_400_000,
            cy_emu: 2_700_000,
        };
        charts_abs.push(c);
    }
    let (drawing_xml, drawing_rels) = write_drawing(&charts_abs, &chart_paths);
    extras.push(ExtraPart {
        path: format!("xl/drawings/drawing{did}.xml"),
        data: drawing_xml.into_bytes(),
        content_type: Some("application/vnd.openxmlformats-officedocument.drawing+xml"),
    });
    extras.push(ExtraPart {
        path: format!("xl/drawings/_rels/drawing{did}.xml.rels"),
        data: drawing_rels.into_bytes(),
        content_type: None,
    });
    let cs_xml = write_chartsheet_xml(&cs.title, "rId1");
    let cs_rels = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="{PKG_REL_NS}"><Relationship Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing" Target="/xl/drawings/drawing{did}.xml" Id="rId1"/></Relationships>"#
    );
    out.clear();
    out.extend_from_slice(cs_xml.as_bytes());
    SheetEmit {
        rels: Some(cs_rels.into_bytes()),
        extras,
        pivot_wirings: Vec::new(),
    }
}

/// True when write must clone sheets to resolve StyleDescs / dates / CF dxfs.
#[inline]
fn workbook_needs_style_resolve(wb: &Workbook) -> bool {
    wb.needs_style_engine()
}

/// Resolve pending StyleDescs and auto date xfs. Mutates sheets in place.
pub fn resolve_workbook_styles(eng: &mut StyleEngine, sheets: &mut [Sheet], options: &WriteOptions) {
    for sh in sheets.iter_mut() {
        // Row style descs
        for (row_num, desc) in std::mem::take(&mut sh.row_style_descs) {
            let idx = eng.resolve(&desc);
            if let Some(r) = sh.rows.iter_mut().find(|r| r.row == row_num) {
                r.style = Some(idx).filter(|&i| i != 0);
            } else {
                let mut r = Row::new(row_num);
                r.style = Some(idx).filter(|&i| i != 0);
                sh.rows.push(r);
            }
        }
        // Col style descs
        for (ci, desc) in std::mem::take(&mut sh.col_style_descs) {
            if let Some(col) = sh.cols.get_mut(ci) {
                let idx = eng.resolve(&desc);
                col.style = Some(idx).filter(|&i| i != 0);
            }
        }
        for row in &mut sh.rows {
            for cell in &mut row.cells {
                if let Some(desc) = cell.style_desc.take() {
                    let idx = eng.resolve(desc.as_ref());
                    cell.style = Some(idx).filter(|&i| i != 0);
                } else if cell.style == Some(0) {
                    // ledger 14: omit s=0
                    cell.style = None;
                }
                // Date/time display: attach numFmt xf when DateSerial/Time/Duration has no style
                if !options.date_iso && cell.style.is_none() {
                    let inferred_fmt = match cell.value {
                        CellValue::DateSerial(n) => {
                            let has_time = (n.fract()).abs() > 1e-12;
                            if has_time {
                                Some(DATETIME_NUM_FMT)
                            } else {
                                Some(DATE_NUM_FMT)
                            }
                        }
                        CellValue::Time(t) => {
                            if ((t * 86400.0).round() % 60.0).abs() > 1e-6 {
                                Some("hh:mm:ss")
                            } else {
                                Some("hh:mm")
                            }
                        }
                        CellValue::Duration(_) => Some("[h]:mm:ss"),
                        _ => None,
                    };
                    if let Some(fmt) = inferred_fmt {
                        let idx = eng.resolve(&StyleDesc {
                            num_fmt: Some(fmt.into()),
                            ..Default::default()
                        });
                        if idx != 0 {
                            cell.style = Some(idx);
                        }
                    }
                }
            }
        }
    }
}

fn write_content_types(
    sheet_count: usize,
    chartsheet_count: usize,
    has_sst: bool,
    macro_enabled: bool,
    need_vml: bool,
    extra_overrides: &[(String, &'static str)],
    media_defaults: &[(String, &'static str)],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(2048);
    push(
        &mut out,
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/>"#,
    );
    if need_vml {
        push(
            &mut out,
            br#"<Default Extension="vml" ContentType="application/vnd.openxmlformats-officedocument.vmlDrawing"/>"#,
        );
    }
    for (ext, ct) in media_defaults {
        push(&mut out, br#"<Default Extension=""#);
        push_str(&mut out, ext);
        push(&mut out, br#"" ContentType=""#);
        push_str(&mut out, ct);
        push(&mut out, br#""/>"#);
    }
    let wb_ct = if macro_enabled {
        "application/vnd.ms-excel.sheet.macroEnabled.main+xml"
    } else {
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"
    };
    push(
        &mut out,
        br#"<Override PartName="/xl/workbook.xml" ContentType=""#,
    );
    push_str(&mut out, wb_ct);
    push(
        &mut out,
        br#""/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/><Override PartName="/xl/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/><Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/>"#,
    );
    for i in 1..=sheet_count {
        push(&mut out, br#"<Override PartName="/xl/worksheets/sheet"#);
        write_u32(&mut out, i as u32);
        push(
            &mut out,
            br#".xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>"#,
        );
    }
    for i in 1..=chartsheet_count {
        push(&mut out, br#"<Override PartName="/xl/chartsheets/sheet"#);
        write_u32(&mut out, i as u32);
        push(
            &mut out,
            br#".xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.chartsheet+xml"/>"#,
        );
    }
    if has_sst {
        push(
            &mut out,
            br#"<Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>"#,
        );
    }
    for (part, ct) in extra_overrides {
        if part.contains("/chartsheets/") {
            continue;
        }
        push(&mut out, br#"<Override PartName=""#);
        push_str(&mut out, part);
        push(&mut out, br#"" ContentType=""#);
        push_str(&mut out, ct);
        push(&mut out, br#""/>"#);
    }
    push(&mut out, b"</Types>");
    out
}

fn write_workbook_rels(
    sheet_count: usize,
    chartsheet_count: usize,
    ext_link_count: usize,
    has_sst: bool,
    pivot_wirings: &[PivotCacheWiring],
    pivot_rel_ids: &[String],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(1024);
    push(
        &mut out,
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
    );
    let mut rid = 1u32;
    for i in 1..=sheet_count {
        push(
            &mut out,
            br#"<Relationship Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet"#,
        );
        write_u32(&mut out, i as u32);
        push(&mut out, br#".xml" Id="rId"#);
        write_u32(&mut out, rid);
        push(&mut out, br#""/>"#);
        rid += 1;
    }
    for i in 1..=chartsheet_count {
        push(
            &mut out,
            br#"<Relationship Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/chartsheet" Target="chartsheets/sheet"#,
        );
        write_u32(&mut out, i as u32);
        push(&mut out, br#".xml" Id="rId"#);
        write_u32(&mut out, rid);
        push(&mut out, br#""/>"#);
        rid += 1;
    }
    for i in 1..=ext_link_count {
        push(
            &mut out,
            br#"<Relationship Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/externalLink" Target="externalLinks/externalLink"#,
        );
        write_u32(&mut out, i as u32);
        push(&mut out, br#".xml" Id="rId"#);
        write_u32(&mut out, rid);
        push(&mut out, br#""/>"#);
        rid += 1;
    }
    push(
        &mut out,
        br#"<Relationship Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml" Id="rId"#,
    );
    write_u32(&mut out, rid);
    push(&mut out, br#""/>"#);
    rid += 1;
    push(
        &mut out,
        br#"<Relationship Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme" Target="theme/theme1.xml" Id="rId"#,
    );
    write_u32(&mut out, rid);
    push(&mut out, br#""/>"#);
    rid += 1;
    if has_sst {
        push(
            &mut out,
            br#"<Relationship Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml" Id="rId"#,
        );
        write_u32(&mut out, rid);
        push(&mut out, br#""/>"#);
    }
    for (w, rid) in pivot_wirings.iter().zip(pivot_rel_ids.iter()) {
        push(
            &mut out,
            br#"<Relationship Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotCacheDefinition" Target="pivotCache/pivotCacheDefinition"#,
        );
        write_u32(&mut out, (w.part_index + 1) as u32);
        push(&mut out, br#".xml" Id=""#);
        push_str(&mut out, rid);
        push(&mut out, br#""/>"#);
    }
    push(&mut out, b"</Relationships>");
    out
}

fn write_workbook_xml(
    wb: &Workbook,
    sheets: &[(String, SheetState)],
    chartsheets: &[String],
    _features: WriteFeatures,
    pivot_wirings: &[PivotCacheWiring],
    pivot_rel_ids: &[String],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(1024);
    push(
        &mut out,
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns=""#,
    );
    push(&mut out, SHEET_NS.as_bytes());
    push(&mut out, br#"" xmlns:r=""#);
    push(&mut out, REL_NS.as_bytes());
    push(&mut out, br#"">"#);

    if wb.options.date1904 {
        push(&mut out, br#"<workbookPr date1904="1"/>"#);
    } else {
        push(&mut out, br#"<workbookPr/>"#);
    }

    if wb.lock_structure {
        push(&mut out, br#"<workbookProtection lockStructure="1"/>"#);
    }

    push(&mut out, br#"<bookViews><workbookView activeTab=""#);
    write_u32(&mut out, wb.active_tab);
    push(&mut out, br#""/></bookViews>"#);

    push(&mut out, b"<sheets>");
    let mut rid = 0u32;
    let mut sheet_id = 0u32;
    for (name, state) in sheets {
        rid += 1;
        sheet_id += 1;
        push(&mut out, br#"<sheet name=""#);
        write_escaped_text(&mut out, name);
        push(&mut out, br#"" sheetId=""#);
        write_u32(&mut out, sheet_id);
        if *state != SheetState::Visible {
            push(&mut out, br#"" state=""#);
            push(&mut out, state.as_str().as_bytes());
        }
        push(&mut out, br#"" r:id="rId"#);
        write_u32(&mut out, rid);
        push(&mut out, br#""/>"#);
    }
    for name in chartsheets {
        rid += 1;
        sheet_id += 1;
        push(&mut out, br#"<sheet name=""#);
        write_escaped_text(&mut out, name);
        push(&mut out, br#"" sheetId=""#);
        write_u32(&mut out, sheet_id);
        push(&mut out, br#"" r:id="rId"#);
        write_u32(&mut out, rid);
        push(&mut out, br#""/>"#);
    }
    push(&mut out, b"</sheets>");

    if !wb.external_links.is_empty() {
        push(&mut out, b"<externalReferences>");
        for _ in &wb.external_links {
            rid += 1;
            push(&mut out, br#"<externalReference r:id="rId"#);
            write_u32(&mut out, rid);
            push(&mut out, br#""/>"#);
        }
        push(&mut out, b"</externalReferences>");
    }

    let names = collect_defined_names(wb);
    if !names.is_empty() {
        push_str(&mut out, &emit_defined_names_xml(&names));
    }

    push(&mut out, br#"<calcPr calcId="124519" fullCalcOnLoad="1"/>"#);
    if !pivot_wirings.is_empty() {
        push(&mut out, b"<pivotCaches>");
        for (w, rid) in pivot_wirings.iter().zip(pivot_rel_ids.iter()) {
            push(&mut out, br#"<pivotCache cacheId=""#);
            write_u32(&mut out, w.cache_id);
            push(&mut out, br#"" r:id=""#);
            push_str(&mut out, rid);
            push(&mut out, br#""/>"#);
        }
        push(&mut out, b"</pivotCaches>");
    }
    push(&mut out, b"</workbook>");
    out
}

fn write_sst(sst: &SstBuilder) -> Vec<u8> {
    let mut out = Vec::with_capacity(sst.len() * 32 + 128);
    let unique = sst.len() as u32;
    let count = sst.total_refs();
    push(
        &mut out,
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns=""#,
    );
    push(&mut out, SHEET_NS.as_bytes());
    push(&mut out, br#"" count=""#);
    write_u32(&mut out, count);
    push(&mut out, br#"" uniqueCount=""#);
    write_u32(&mut out, unique);
    push(&mut out, br#"">"#);
    for s in sst.strings() {
        let t = truncate_str(s);
        push(&mut out, b"<si><t");
        if needs_preserve(t) {
            push(&mut out, br#" xml:space="preserve""#);
        }
        push(&mut out, b">");
        write_escaped_text(&mut out, t);
        push(&mut out, b"</t></si>");
    }
    push(&mut out, b"</sst>");
    out
}

/// Fast path: dense f64 grid → worksheet XML.
pub fn write_numeric_grid_sheet(grid: &NumericGrid) -> Vec<u8> {
    let est = (grid.nrows as usize)
        .saturating_mul(grid.ncols as usize)
        .saturating_mul(40)
        .saturating_add(512);
    let mut out = Vec::with_capacity(est);
    write_numeric_grid_sheet_into(grid, &mut out);
    out
}

/// Write dense f64 grid XML into `out` (cleared first; capacity retained).
pub fn write_numeric_grid_sheet_into(grid: &NumericGrid, out: &mut Vec<u8>) {
    let nrows = grid.nrows;
    let ncols = grid.ncols;
    let est = (nrows as usize)
        .saturating_mul(ncols as usize)
        .saturating_mul(40)
        .saturating_add(512);
    out.clear();
    if out.capacity() < est {
        out.reserve(est.saturating_sub(out.capacity()));
    }

    write_sheet_open(out, false);
    push(
        out,
        br#"<sheetPr><outlinePr summaryBelow="1" summaryRight="1"/><pageSetUpPr/></sheetPr>"#,
    );
    push(out, br#"<dimension ref=""#);
    if nrows > 0 && ncols > 0 {
        let dim = dimension_ref(1, 1, nrows, ncols);
        push(out, dim.as_bytes());
    } else {
        push(out, b"A1");
    }
    push(out, br#""/>"#);
    push(
        out,
        br#"<sheetViews><sheetView workbookViewId="0"><selection activeCell="A1" sqref="A1"/></sheetView></sheetViews>"#,
    );
    push(
        out,
        br#"<sheetFormatPr baseColWidth="8" defaultRowHeight="15"/>"#,
    );
    push(out, b"<sheetData>");
    let vals = &grid.values;
    let mut coord_buf = [0u8; 4];
    for r in 0..nrows {
        push(out, br#"<row r=""#);
        write_u32(out, r + 1);
        push(out, br#"">"#);
        let base = (r as usize) * (ncols as usize);
        for c in 0..ncols {
            let v = vals[base + c as usize];
            if v.is_nan() {
                continue;
            }
            push(out, br#"<c r=""#);
            let letters = col_letters(c + 1, &mut coord_buf);
            out.extend_from_slice(letters);
            write_u32(out, r + 1);
            push(out, br#""><v>"#);
            write_f64(out, v);
            push(out, br#"</v></c>"#);
        }
        push(out, b"</row>");
    }
    push(out, b"</sheetData>");
    push(
        out,
        br#"<pageMargins left="0.75" right="0.75" top="1" bottom="1" header="0.5" footer="0.5"/>"#,
    );
    push(out, b"</worksheet>");
}

fn write_sheet_open(out: &mut Vec<u8>, with_r: bool) {
    push(
        out,
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns=""#,
    );
    push(out, SHEET_NS.as_bytes());
    if with_r {
        push(out, br#"" xmlns:r=""#);
        push(out, REL_NS.as_bytes());
    }
    push(out, br#"">"#);
}

/// Public helper used by unit tests — plain worksheet XML (no package extras).
pub fn write_worksheet(
    sheet: &Sheet,
    use_sst: bool,
    emit_cache: bool,
    sst: &mut SstBuilder,
) -> Vec<u8> {
    let mut counters = PartCounters::default();
    let mut interner = MediaInterner::new();
    for img in &sheet.images {
        interner.intern(&img.bytes, img.format);
    }
    let mut out = Vec::new();
    let _ = write_sheet_package(
        sheet,
        use_sst,
        emit_cache,
        &mut SstAccess::Build(sst),
        &mut counters,
        WriteFeatures::ALL,
        &interner,
        &[],
        false,
        false,
        &mut out,
    );
    out
}

/// Serial SST pre-build in sheet/row/cell order (matches emission order).
fn prebuild_sst(sheets: &[Sheet]) -> SstBuilder {
    let mut sst = SstBuilder::new();
    for sheet in sheets {
        for row in &sheet.rows {
            for cell in &row.cells {
                if let CellValue::Str(s) = &cell.value {
                    sst.intern(truncate_str(s));
                }
            }
        }
    }
    sst
}

/// Starting PartCounters for each sheet so parallel IDs match serial assignment.
fn preassign_counter_starts(sheets: &[Sheet], features: WriteFeatures) -> Vec<PartCounters> {
    let mut starts = Vec::with_capacity(sheets.len());
    let mut acc = PartCounters::default();
    for sheet in sheets {
        starts.push(acc);
        advance_counters_for_sheet(&mut acc, sheet, features);
    }
    starts
}

fn advance_counters_for_sheet(c: &mut PartCounters, sheet: &Sheet, features: WriteFeatures) {
    let has_charts = !sheet.charts.is_empty() && features.contains(WriteFeatures::CHARTS);
    let has_images = !sheet.images.is_empty() && features.contains(WriteFeatures::IMAGES);
    if has_charts || has_images {
        c.drawing_id += 1;
        c.chart_id += sheet.charts.len();
    }
    if !sheet.comments.is_empty() && features.contains(WriteFeatures::COMMENTS) {
        c.comment_id += 1;
    }
    if !sheet.tables.is_empty() && features.contains(WriteFeatures::TABLES) {
        c.table_id += sheet.tables.len();
    }
}

#[allow(clippy::too_many_arguments)]
fn write_sheet_package(
    sheet: &Sheet,
    use_sst: bool,
    emit_cache: bool,
    sst: &mut SstAccess<'_>,
    counters: &mut PartCounters,
    features: WriteFeatures,
    interner: &MediaInterner,
    pivot_assigns: &[PivotAssign],
    date_iso: bool,
    date1904: bool,
    out: &mut Vec<u8>,
) -> SheetEmit {
    let need_r = sheet_needs_r_ns(sheet);
    let est = estimate_sheet_size(sheet);
    out.clear();
    if out.capacity() < est {
        out.reserve(est.saturating_sub(out.capacity()));
    }
    write_sheet_open(out, need_r);

    // sheetPr: tabColor + fitToPage
    push(out, b"<sheetPr");
    if let Some(ref cn) = sheet.code_name {
        push(out, br#" codeName=""#);
        write_escaped_text(out, cn);
        push(out, b"\"");
    }
    push(out, b">");
    if let Some(rgb) = &sheet.tab_color_rgb {
        let rgb = if rgb.len() == 6 {
            format!("00{rgb}")
        } else {
            rgb.clone()
        };
        push(out, br#"<tabColor rgb=""#);
        write_escaped_attr(out, &rgb);
        push(out, br#""/>"#);
    }
    push(out, br#"<outlinePr summaryBelow="1" summaryRight="1"/>"#);
    if sheet
        .page_setup
        .as_ref()
        .map(|p| p.fit_to_page)
        .unwrap_or(false)
    {
        push(out, br#"<pageSetUpPr fitToPage="1"/>"#);
    } else {
        push(out, br#"<pageSetUpPr/>"#);
    }
    push(out, b"</sheetPr>");

    let dim = if let Some(ref d) = sheet.dimension {
        d.clone()
    } else if let Some((r0, c0, r1, c1)) = sheet.bounds() {
        dimension_ref(r0, c0, r1, c1)
    } else {
        "A1".into()
    };
    push(out, br#"<dimension ref=""#);
    push(out, dim.as_bytes());
    push(out, br#""/>"#);

    // sheetViews + freeze
    push(out, br#"<sheetViews><sheetView workbookViewId="0""#);
    if let Some(ref freeze) = sheet.view.freeze_cell {
        if let Some((fr, fc)) = parse_a1(freeze) {
            let y_split = fr.saturating_sub(1);
            let x_split = fc.saturating_sub(1);
            if y_split > 0 || x_split > 0 {
                push(out, br#"><pane"#);
                if x_split > 0 {
                    push(out, br#" xSplit=""#);
                    write_u32(out, x_split);
                    push(out, b"\"");
                }
                if y_split > 0 {
                    push(out, br#" ySplit=""#);
                    write_u32(out, y_split);
                    push(out, b"\"");
                }
                let active = if x_split > 0 && y_split > 0 {
                    "bottomRight"
                } else if y_split > 0 {
                    "bottomLeft"
                } else {
                    "topRight"
                };
                push(out, br#" topLeftCell=""#);
                write_escaped_text(out, freeze);
                push(out, br#"" activePane=""#);
                push_str(out, active);
                push(out, br#"" state="frozen"/>"#);
                if x_split > 0 && y_split > 0 {
                    push(
                        out,
                        br#"<selection pane="topRight"/><selection pane="bottomLeft"/><selection pane="bottomRight" activeCell="A1" sqref="A1"/>"#,
                    );
                } else if y_split > 0 {
                    push(
                        out,
                        br#"<selection pane="bottomLeft" activeCell="A1" sqref="A1"/>"#,
                    );
                } else {
                    push(
                        out,
                        br#"<selection pane="topRight" activeCell="A1" sqref="A1"/>"#,
                    );
                }
                push(out, br#"</sheetView>"#);
            } else {
                push(
                    out,
                    br#"><selection activeCell="A1" sqref="A1"/></sheetView>"#,
                );
            }
        } else {
            push(
                out,
                br#"><selection activeCell="A1" sqref="A1"/></sheetView>"#,
            );
        }
    } else {
        push(
            out,
            br#"><selection activeCell="A1" sqref="A1"/></sheetView>"#,
        );
    }
    push(out, br#"</sheetViews>"#);

    push(out, br#"<sheetFormatPr baseColWidth=""#);
    write_u32(out, sheet.base_col_width);
    push(out, br#"" defaultRowHeight=""#);
    write_f64(out, sheet.default_row_height);
    if let Some(w) = sheet.default_col_width {
        push(out, br#"" defaultColWidth=""#);
        write_f64(out, w);
    }
    push(out, br#""/>"#);

    if !sheet.cols.is_empty() {
        push(out, b"<cols>");
        for col in &sheet.cols {
            push(out, br#"<col min=""#);
            write_u32(out, col.min);
            push(out, br#"" max=""#);
            write_u32(out, col.max);
            if let Some(w) = col.width {
                push(out, br#"" width=""#);
                write_f64(out, w);
                if col.custom_width {
                    push(out, br#"" customWidth="1"#);
                }
            }
            if col.hidden {
                push(out, br#"" hidden="1"#);
            }
            if let Some(s) = col.style {
                if s != 0 {
                    push(out, br#"" style=""#);
                    write_u32(out, s);
                }
            }
            if col.best_fit {
                push(out, br#"" bestFit="1"#);
            }
            if col.outline_level > 0 {
                push(out, br#"" outlineLevel=""#);
                write_u32(out, col.outline_level as u32);
            }
            push(out, br#""/>"#);
        }
        push(out, b"</cols>");
    }

    push(out, b"<sheetData>");
    for row in &sheet.rows {
        write_row(out, row, use_sst, emit_cache, sst, date_iso, date1904);
    }
    push(out, b"</sheetData>");

    // ---- ledger 20 tail ----
    let mut rels: Vec<(String, String, Option<String>)> = Vec::new();
    let mut next_rid: usize = 0;
    let mut extras: Vec<ExtraPart> = Vec::new();
    let mut pivot_wirings: Vec<PivotCacheWiring> = Vec::new();

    let do_merges =
        features.contains(WriteFeatures::MERGES) || features.contains(WriteFeatures::ALL);
    // Content-driven: emit if present (auto features already enabled flags)
    if let Some(prot) = &sheet.protection {
        emit_sheet_protection(out, prot);
    }
    if !sheet.scenarios.is_empty() {
        emit_scenarios(out, &sheet.scenarios);
    }
    if let Some(af) = &sheet.auto_filter {
        emit_sheet_auto_filter(out, af, features);
    }
    if !sheet.merges.is_empty() {
        let _ = do_merges;
        emit_merges(out, &sheet.merges);
    }

    // CF then DV (W2)
    for cf in &sheet.conditional_formatting {
        cf.emit(out);
    }
    emit_data_validations(&sheet.data_validations, out);

    if !sheet.hyperlinks.is_empty() {
        let hl_rels = emit_hyperlinks(out, &sheet.hyperlinks, &mut next_rid);
        rels.extend(hl_rels);
    }

    if let Some(po) = &sheet.print_options {
        emit_print_options(out, po);
    }
    if let Some(m) = &sheet.page_margins {
        emit_page_margins(out, m);
    } else {
        emit_default_page_margins(out);
    }
    if let Some(ps) = &sheet.page_setup {
        emit_page_setup(out, ps);
    }
    if let Some(hf) = &sheet.header_footer {
        emit_header_footer(out, hf);
    }
    emit_breaks(out, &sheet.row_breaks, &sheet.col_breaks);

    // Drawing + charts + images (one drawing part per worksheet)
    let has_charts = !sheet.charts.is_empty() && features.contains(WriteFeatures::CHARTS);
    let has_images = !sheet.images.is_empty() && features.contains(WriteFeatures::IMAGES);
    if has_charts || has_images {
        counters.drawing_id += 1;
        let did = counters.drawing_id;
        let mut chart_rel_paths = Vec::new();
        if has_charts {
            for ch in &sheet.charts {
                counters.chart_id += 1;
                let cid = counters.chart_id;
                extras.push(ExtraPart {
                    path: format!("xl/charts/chart{cid}.xml"),
                    data: write_chart_space(ch).into_bytes(),
                    content_type: Some(
                        "application/vnd.openxmlformats-officedocument.drawingml.chart+xml",
                    ),
                });
                chart_rel_paths.push(format!("../charts/chart{cid}.xml"));
            }
        }
        let (drawing_images, media_targets) = build_drawing_images(sheet, interner);
        let (drawing_xml, drawing_rels) = write_drawing_full(
            &sheet.charts,
            &chart_rel_paths,
            &drawing_images,
            &media_targets,
        );
        extras.push(ExtraPart {
            path: format!("xl/drawings/drawing{did}.xml"),
            data: drawing_xml.into_bytes(),
            content_type: Some("application/vnd.openxmlformats-officedocument.drawing+xml"),
        });
        extras.push(ExtraPart {
            path: format!("xl/drawings/_rels/drawing{did}.xml.rels"),
            data: drawing_rels.into_bytes(),
            content_type: None,
        });
        next_rid += 1;
        let id = format!("rId{next_rid}");
        rels.push((
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing".into(),
            format!("/xl/drawings/drawing{did}.xml"),
            None,
        ));
        push(out, br#"<drawing r:id=""#);
        push_str(out, &id);
        push(out, br#""/>"#);
    }

    // Comments + legacyDrawing
    if !sheet.comments.is_empty() && features.contains(WriteFeatures::COMMENTS) {
        counters.comment_id += 1;
        let cid = counters.comment_id;
        let (comments_xml, vml) = write_comments(&sheet.comments);
        extras.push(ExtraPart {
            path: format!("xl/comments/comment{cid}.xml"),
            data: comments_xml.into_bytes(),
            content_type: Some(
                "application/vnd.openxmlformats-officedocument.spreadsheetml.comments+xml",
            ),
        });
        extras.push(ExtraPart {
            path: format!("xl/drawings/commentsDrawing{cid}.vml"),
            data: vml.into_bytes(),
            content_type: None,
        });
        push(out, br#"<legacyDrawing r:id="anysvml"/>"#);
    }

    // tableParts
    if !sheet.tables.is_empty() && features.contains(WriteFeatures::TABLES) {
        push(out, br#"<tableParts count=""#);
        write_u32(out, sheet.tables.len() as u32);
        push(out, b"\">");
        for t in &sheet.tables {
            counters.table_id += 1;
            let tid = counters.table_id;
            extras.push(ExtraPart {
                path: format!("xl/tables/table{tid}.xml"),
                data: write_table(t, tid).into_bytes(),
                content_type: Some(
                    "application/vnd.openxmlformats-officedocument.spreadsheetml.table+xml",
                ),
            });
            next_rid += 1;
            let id = format!("rId{next_rid}");
            rels.push((
                "http://schemas.openxmlformats.org/officeDocument/2006/relationships/table".into(),
                format!("/xl/tables/table{tid}.xml"),
                None,
            ));
            push(out, br#"<tablePart r:id=""#);
            push_str(out, &id);
            push(out, br#""/>"#);
        }
        push(out, b"</tableParts>");
    }

    // Pivots (Task B5b): cache definition + records + table part + their rels.
    // A pivot is referenced from a worksheet ONLY through the sheet rels — the
    // sheet XML has no pivot element — so all parts ship as extras and the rel
    // is appended below like a table rel.
    if !sheet.pivots.is_empty() && features.contains(WriteFeatures::PIVOTS) {
        for (pivot, assign) in sheet.pivots.iter().zip(pivot_assigns.iter()) {
            let parts = build_pivot_parts(sheet, pivot, assign.cache_id, assign.part_index);
            let Some(parts) = parts else {
                continue;
            };
            for (path, data) in parts.parts {
                let content_type = parts
                    .content_types
                    .iter()
                    .find(|(p, _)| *p == format!("/{path}"))
                    .map(|(_, c)| *c);
                extras.push(ExtraPart {
                    path,
                    data,
                    content_type,
                });
            }
            // No `next_rid += 1` here: this is the last relationship of
            // the loop body and nothing reads the counter again.
            rels.push((
                "http://schemas.openxmlformats.org/officeDocument/2006/relationships/pivotTable"
                    .into(),
                format!("/{}", parts.table_part),
                None,
            ));
            pivot_wirings.push(PivotCacheWiring {
                cache_id: parts.cache_id,
                part_index: parts.part_index,
            });
        }
    }

    push(out, b"</worksheet>");

    // sheet rels
    let mut rels_xml: Option<Vec<u8>> = None;
    if !rels.is_empty()
        || (!sheet.comments.is_empty() && features.contains(WriteFeatures::COMMENTS))
    {
        let mut r = Vec::new();
        push(
            &mut r,
            br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
        );
        for (i, (ty, target, mode)) in rels.iter().enumerate() {
            let id = i + 1;
            push(&mut r, br#"<Relationship Id="rId"#);
            write_u32(&mut r, id as u32);
            push(&mut r, br#"" Type=""#);
            push_str(&mut r, ty);
            push(&mut r, br#"" Target=""#);
            write_escaped_attr(&mut r, target);
            push(&mut r, b"\"");
            if let Some(m) = mode {
                push(&mut r, br#" TargetMode=""#);
                push_str(&mut r, m);
                push(&mut r, b"\"");
            }
            push(&mut r, br#"/>"#);
        }
        if !sheet.comments.is_empty() && features.contains(WriteFeatures::COMMENTS) {
            let cid = counters.comment_id;
            push(
                &mut r,
                br#"<Relationship Id="comments" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments" Target="/xl/comments/comment"#,
            );
            write_u32(&mut r, cid as u32);
            push(&mut r, br#".xml"/>"#);
            push(
                &mut r,
                br#"<Relationship Id="anysvml" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/vmlDrawing" Target="/xl/drawings/commentsDrawing"#,
            );
            write_u32(&mut r, cid as u32);
            push(&mut r, br#".vml"/>"#);
        }
        push(&mut r, b"</Relationships>");
        rels_xml = Some(r);
    }

    // Sheet XML already written into `out` (caller deflates via zip.add_recycle).
    SheetEmit {
        rels: rels_xml,
        extras,
        pivot_wirings,
    }
}

/// Resolve a sheet's images into drawing entries. Rel ids continue after the
/// chart rels (`rId chart_count + 1 ..`); `cNvPr` ids stay unique across both.
/// `media_targets` are the drawing-relative media paths, aligned with `images`.
fn build_drawing_images(
    sheet: &Sheet,
    interner: &MediaInterner,
) -> (Vec<DrawingImage>, Vec<String>) {
    let mut images = Vec::new();
    let mut targets = Vec::new();
    for (i, img) in sheet.images.iter().enumerate() {
        let media_index = interner
            .lookup(&img.bytes)
            .expect("image was interned before sheet emission");
        images.push(DrawingImage {
            anchor: img.anchor.clone(),
            rel_id: sheet.charts.len() + i + 1,
            cnv_id: sheet.charts.len() + i + 1,
        });
        targets.push(interner.media_rel_target(media_index));
    }
    (images, targets)
}

/// Parse simple A1 (no sheet, no $) → (row, col) 1-based.
fn parse_a1(s: &str) -> Option<(u32, u32)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        i += 1;
    }
    if i == 0 || i == bytes.len() {
        return None;
    }
    let mut col = 0u32;
    for &b in &bytes[..i] {
        let c = b.to_ascii_uppercase();
        if !c.is_ascii_uppercase() {
            return None;
        }
        col = col * 26 + (c - b'A') as u32 + 1;
    }
    let row: u32 = std::str::from_utf8(&bytes[i..]).ok()?.parse().ok()?;
    if row == 0 {
        return None;
    }
    Some((row, col))
}

fn estimate_sheet_size(sheet: &Sheet) -> usize {
    let cells: usize = sheet.rows.iter().map(|r| r.cells.len()).sum();
    cells.saturating_mul(48).saturating_add(1024)
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d as u32)
}

fn serial_parts(serial: f64, date1904: bool) -> (i64, u32, u32, f64) {
    let base = serial.floor();
    let z = if date1904 {
        base as i64 - 24_107
    } else {
        let d = if base <= 59.0 { base + 1.0 } else { base };
        d as i64 - 25_569
    };
    let (y, m, d) = civil_from_days(z);
    (y, m, d, (serial - base).max(0.0))
}

fn format_iso_datetime(serial: f64, date1904: bool) -> Option<String> {
    let (y, m, d, frac) = serial_parts(serial, date1904);
    if frac.abs() < 1e-10 {
        Some(format!("{y:04}-{m:02}-{d:02}"))
    } else {
        let total_seconds = (frac * 86400.0).round() as u64;
        let hours = (total_seconds / 3600) % 24;
        let minutes = (total_seconds % 3600) / 60;
        let seconds = total_seconds % 60;
        Some(format!("{y:04}-{m:02}-{d:02}T{hours:02}:{minutes:02}:{seconds:02}"))
    }
}

fn write_row(
    out: &mut Vec<u8>,
    row: &Row,
    use_sst: bool,
    emit_cache: bool,
    sst: &mut SstAccess<'_>,
    date_iso: bool,
    date1904: bool,
) {
    let has_attrs = row.height.is_some() || row.hidden || row.style.is_some();
    if row.cells.is_empty() && !has_attrs {
        return;
    }

    push(out, br#"<row r=""#);
    write_u32(out, row.row);
    if let Some(ht) = row.height {
        push(out, br#"" ht=""#);
        write_f64(out, ht);
        if row.custom_height {
            push(out, br#"" customHeight="1"#);
        }
    }
    if row.hidden {
        push(out, br#"" hidden="1"#);
    }
    if let Some(s) = row.style {
        if s != 0 {
            push(out, br#"" s=""#);
            write_u32(out, s);
            push(out, br#"" customFormat="1"#);
        }
    }
    push(out, br#"">"#);

    for cell in &row.cells {
        write_cell(out, row.row, cell, use_sst, emit_cache, sst, date_iso, date1904);
    }
    push(out, b"</row>");
}

#[inline]
fn write_style_attr(out: &mut Vec<u8>, style: Option<u32>) {
    if let Some(s) = style {
        if s != 0 {
            push(out, br#"" s=""#);
            write_u32(out, s);
        }
    }
}

fn write_cell(
    out: &mut Vec<u8>,
    row: u32,
    cell: &Cell,
    use_sst: bool,
    emit_cache: bool,
    sst: &mut SstAccess<'_>,
    date_iso: bool,
    date1904: bool,
) {
    if let CellValue::Empty = &cell.value {
        if cell.style.is_none() || cell.style == Some(0) {
            return;
        }
        push(out, br#"<c r=""#);
        write_coord(out, row, cell.col);
        write_style_attr(out, cell.style);
        push(out, br#""/>"#);
        return;
    }

    if date_iso {
        if let CellValue::DateSerial(n) = &cell.value {
            if let Some(iso) = format_iso_datetime(*n, date1904) {
                push(out, br#"<c r=""#);
                write_coord(out, row, cell.col);
                write_style_attr(out, cell.style);
                push(out, br#"" t="d"><v>"#);
                push_str(out, &iso);
                push(out, br#"</v></c>"#);
                return;
            }
        }
    }

    push(out, br#"<c r=""#);
    write_coord(out, row, cell.col);
    write_style_attr(out, cell.style);

    match &cell.value {
        CellValue::Empty => unreachable!(),
        CellValue::Number(n) | CellValue::DateSerial(n) | CellValue::Time(n) | CellValue::Duration(n) => {
            push(out, br#""><v>"#);
            write_f64(out, *n);
            push(out, br#"</v></c>"#);
        }
        CellValue::Bool(b) => {
            push(out, br#"" t="b"><v>"#);
            push(out, if *b { b"1" } else { b"0" });
            push(out, br#"</v></c>"#);
        }
        CellValue::Error(e) => {
            push(out, br#"" t="e"><v>"#);
            write_escaped_text(out, e);
            push(out, br#"</v></c>"#);
        }
        CellValue::Str(s) => {
            let t = truncate_str(s);
            if use_sst {
                let idx = sst.index(t);
                push(out, br#"" t="s"><v>"#);
                write_u32(out, idx);
                push(out, br#"</v></c>"#);
            } else {
                push(out, br#"" t="inlineStr"><is><t"#);
                if needs_preserve(t) {
                    push(out, br#" xml:space="preserve""#);
                }
                push(out, b">");
                write_escaped_text(out, t);
                push(out, br#"</t></is></c>"#);
            }
        }
        CellValue::Rich(rt) => {
            // Rich text always inlineStr (not SST)
            push(out, br#"" t="inlineStr">"#);
            rt.emit_is(out);
            push(out, b"</c>");
        }
        CellValue::Formula { text, kind, cached } => {
            push(out, b"\"");
            // The cached result's type must be declared on the cell, or Excel
            // reads a non-numeric `<v>` as a number and shows garbage. Numeric
            // caches take the implicit default and emit no `t=`.
            if let Some(cv) = cached {
                push(out, formula_result_type(cv));
            }
            match kind {
                FormulaKind::Normal => {
                    push(out, b"><f>");
                    let body = text.strip_prefix('=').unwrap_or(text.as_str());
                    write_escaped_text(out, body);
                    push(out, b"</f>");
                }
                FormulaKind::Array { ref_ } => {
                    push(out, br#"><f t="array" ref=""#);
                    write_escaped_text(out, ref_);
                    push(out, b"\">");
                    let body = text.strip_prefix('=').unwrap_or(text.as_str());
                    write_escaped_text(out, body);
                    push(out, b"</f>");
                }
                FormulaKind::DataTable {
                    ref_,
                    dt2d,
                    dtr,
                    r1,
                    r2,
                    del1,
                    del2,
                    ca,
                } => {
                    push(out, br#"><f t="dataTable" ref=""#);
                    write_escaped_text(out, ref_);
                    push(out, b"\"");
                    if *dt2d {
                        push(out, br#" dt2D="1""#);
                    }
                    if *dtr {
                        push(out, br#" dtr="1""#);
                    }
                    if let Some(r) = r1 {
                        push(out, br#" r1=""#);
                        write_escaped_text(out, r);
                        push(out, b"\"");
                    }
                    if let Some(r) = r2 {
                        push(out, br#" r2=""#);
                        write_escaped_text(out, r);
                        push(out, b"\"");
                    }
                    if *del1 {
                        push(out, br#" del1="1""#);
                    }
                    if *del2 {
                        push(out, br#" del2="1""#);
                    }
                    if *ca {
                        push(out, br#" ca="1""#);
                    }
                    push(out, b"/>");
                }
            }
            if emit_cache {
                if let Some(cv) = cached {
                    write_cached_v(out, cv);
                }
            }
            push(out, b"</c>");
        }
    }
}

/// The `t=` attribute a formula cell needs so Excel reads its cached `<v>`
/// with the right type. A number carries no attribute (that is the default);
/// `str` is a computed string, which is deliberately NOT the shared-string
/// `t="s"` form — a formula result is never an SST index.
fn formula_result_type(cv: &CachedValue) -> &'static [u8] {
    match cv {
        CachedValue::Number(_) => b"",
        CachedValue::Bool(_) => br#" t="b""#,
        CachedValue::Error(_) => br#" t="e""#,
        CachedValue::Str(_) => br#" t="str""#,
    }
}

fn write_cached_v(out: &mut Vec<u8>, cv: &CachedValue) {
    match cv {
        CachedValue::Number(n) => {
            push(out, b"<v>");
            write_f64(out, *n);
            push(out, b"</v>");
        }
        CachedValue::Bool(b) => {
            push(out, b"<v>");
            push(out, if *b { b"1" } else { b"0" });
            push(out, b"</v>");
        }
        CachedValue::Error(e) => {
            push(out, b"<v>");
            write_escaped_text(out, e);
            push(out, b"</v>");
        }
        CachedValue::Str(s) => {
            push(out, b"<v>");
            write_escaped_text(out, truncate_str(s));
            push(out, b"</v>");
        }
    }
}

/// Excel serial from civil date (Windows 1900 system), matching openpyxl `to_excel` for date-only.
pub fn date_to_serial(year: i32, month: u32, day: u32) -> f64 {
    let epoch = chrono_days(1899, 12, 30);
    let d = chrono_days(year, month, day);
    let mut days = d - epoch;
    if days > 0 && days <= 60 {
        days -= 1;
    }
    days as f64
}

/// Excel serial from datetime (fractional day).
pub fn datetime_to_serial(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    micros: u32,
) -> f64 {
    let base = date_to_serial(year, month, day);
    let frac = (hour as f64) / 24.0
        + (minute as f64) / 1440.0
        + (second as f64) / 86400.0
        + (micros as f64) / 86_400_000_000.0;
    base + frac
}

fn chrono_days(year: i32, month: u32, day: u32) -> i64 {
    let y = year as i64;
    let m = month as i64;
    let d = day as i64;
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

#[cfg(test)]
mod tests {
    use super::super::charts::{Chart, ChartType, Series};
    use super::*;
    use pretty_assertions::assert_eq;
    use std::sync::Arc;

    #[test]
    fn serial_known() {
        assert_eq!(date_to_serial(2020, 1, 15), 43845.0);
    }

    #[test]
    fn smoke_inline_bytes() {
        let mut wb = Workbook::with_sheet("Data");
        wb.sheets[0].rows.push(
            Row::new(1)
                .with_cell(1, CellValue::Number(1.5))
                .with_cell(2, CellValue::Str("hi".into()))
                .with_cell(3, CellValue::Bool(true)),
        );
        let bytes = write_workbook_bytes(&wb).unwrap();
        assert!(bytes.len() > 1000);
        // ZIP local header
        assert_eq!(&bytes[0..4], &[0x50, 0x4b, 0x03, 0x04]);
    }

    #[test]
    fn formula_cache_in_xml() {
        let mut sheet = Sheet::new("F");
        sheet.rows.push(Row::new(1).with_cell(
            1,
            CellValue::Formula {
                text: "=1+1".into(),
                kind: FormulaKind::Normal,
                cached: Some(CachedValue::Number(2.0)),
            },
        ));
        let mut sst = SstBuilder::new();
        let xml = write_worksheet(&sheet, false, true, &mut sst);
        let s = String::from_utf8_lossy(&xml);
        assert!(s.contains("<f>1+1</f>"));
        assert!(s.contains("<v>2</v>") || s.contains("<v>2.0</v>"));
    }

    #[test]
    fn sst_emits_index() {
        let mut sheet = Sheet::new("S");
        sheet.rows.push(
            Row::new(1)
                .with_cell(1, CellValue::Str("foo".into()))
                .with_cell(2, CellValue::Str("foo".into())),
        );
        let mut sst = SstBuilder::new();
        let xml = write_worksheet(&sheet, true, true, &mut sst);
        let s = String::from_utf8_lossy(&xml);
        assert!(s.contains(r#"t="s""#));
        assert_eq!(sst.len(), 1);
        assert_eq!(sst.total_refs(), 2);
    }

    #[test]
    fn numeric_grid() {
        let grid = NumericGrid {
            sheet_name: "N".into(),
            nrows: 2,
            ncols: 2,
            values: Arc::new(vec![1.0, 2.0, 3.0, 4.0]),
        };
        let xml = write_numeric_grid_sheet(&grid);
        let s = String::from_utf8_lossy(&xml);
        assert!(s.contains("A1"));
        assert!(s.contains("<v>1</v>") || s.contains("<v>1.0</v>"));
    }

    #[test]
    fn parse_a1_ok() {
        assert_eq!(parse_a1("B2"), Some((2, 2)));
        assert_eq!(parse_a1("AA10"), Some((10, 27)));
    }

    #[test]
    fn empty_sheet_package() {
        let wb = Workbook::with_sheet("Empty");
        let bytes = write_workbook_bytes(&wb).unwrap();
        assert!(bytes.len() > 500);
    }

    #[test]
    fn hidden_sheet_state() {
        let mut wb = Workbook::with_sheet("Vis");
        wb.sheets.push(Sheet::new("Hid"));
        wb.sheets[1].state = SheetState::Hidden;
        let bytes = write_workbook_bytes(&wb).unwrap();
        // inflate not needed; name appears in workbook.xml compressed or stored
        // just ensure write succeeds multi-sheet
        assert!(bytes.len() > 1000);
    }

    fn sample_auto_filter() -> AutoFilterMeta {
        AutoFilterMeta {
            ref_: crate::turbo::structural::CellRange {
                r0: 0,
                c0: 0,
                r1: 5,
                c1: 3,
            },
            columns: vec![crate::turbo::meta::FilterColumnMeta {
                col_id: 0,
                hidden_button: false,
                show_button: true,
                values: vec!["Alice".into(), "Carol".into()],
                blank: Some(false),
            }],
        }
    }

    #[test]
    fn auto_filter_flag_on_emits_columns() {
        let mut sheet = Sheet::new("F");
        sheet.auto_filter = Some(sample_auto_filter());
        let mut counters = PartCounters::default();
        let mut sst = SstBuilder::new();
        let mut out = Vec::new();
        let _ = write_sheet_package(
            &sheet,
            false,
            true,
            &mut SstAccess::Build(&mut sst),
            &mut counters,
            WriteFeatures::MERGES,
            &MediaInterner::new(),
            &[],
            false,
            false,
            &mut out,
        );
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains(r#"<autoFilter ref="A1:D6">"#), "{s}");
        assert!(s.contains("<filterColumn"), "{s}");
        assert!(s.contains(r#"<filter val="Alice"/>"#), "{s}");
    }

    #[test]
    fn auto_filter_flag_off_emits_no_columns() {
        let mut sheet = Sheet::new("F");
        sheet.auto_filter = Some(sample_auto_filter());
        let mut counters = PartCounters::default();
        let mut sst = SstBuilder::new();
        let mut out = Vec::new();
        let _ = write_sheet_package(
            &sheet,
            false,
            true,
            &mut SstAccess::Build(&mut sst),
            &mut counters,
            WriteFeatures::CORE,
            &MediaInterner::new(),
            &[],
            false,
            false,
            &mut out,
        );
        let s = String::from_utf8_lossy(&out);
        assert!(!s.contains("<filterColumn"), "{s}");
        assert!(!s.contains("<filters"), "{s}");
    }

    #[test]
    fn auto_filter_roundtrip_survives() {
        let path = format!("{}/testdata/gap_sheetmeta.xlsx", env!("CARGO_MANIFEST_DIR"));
        let read = crate::turbo::read_workbook_turbo(&path, crate::turbo::Features::SHEET_META)
            .expect("read fixture");
        let af = read.sheets[0]
            .auto_filter
            .clone()
            .expect("fixture has an autofilter with filters");
        assert_eq!(af.columns.len(), 1);
        assert_eq!(
            af.columns[0].values,
            vec!["Alice".to_string(), "Carol".to_string()]
        );

        let mut wb = Workbook::new();
        let mut sheet = Sheet::new("Out");
        sheet.auto_filter = Some(af);
        wb.sheets = vec![sheet];
        let bytes = write_workbook_bytes(&wb).unwrap();
        let tmp = std::env::temp_dir().join("kyrax_autofilter_roundtrip.xlsx");
        std::fs::write(&tmp, &bytes).unwrap();
        let re = crate::turbo::read_workbook_turbo(
            tmp.to_str().unwrap(),
            crate::turbo::Features::SHEET_META,
        )
        .expect("re-read written workbook");
        let af2 = re.sheets[0]
            .auto_filter
            .as_ref()
            .expect("autofilter survived");
        assert_eq!(af2.columns.len(), 1);
        assert_eq!(af2.columns[0].col_id, 0);
        assert_eq!(af2.columns[0].hidden_button, false);
        assert_eq!(af2.columns[0].show_button, true);
        assert_eq!(
            af2.columns[0].values,
            vec!["Alice".to_string(), "Carol".to_string()]
        );
        assert_eq!(af2.columns[0].blank, Some(false));
    }

    // ------------------------------------------------------------------
    // Images (T1-2a): packaging + rels + content types + dedup + determinism.
    // ------------------------------------------------------------------

    const TEST_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x01, 0x02, 0x03,
    ];
    const TEST_JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46];

    fn entry(bytes: &[u8], name: &str) -> String {
        let v = crate::turbo::zipmin::read_entry(bytes, name)
            .unwrap()
            .unwrap_or_else(|| panic!("missing entry {name}"));
        String::from_utf8_lossy(&v).into_owned()
    }

    fn wb_with_image(png: bool) -> Workbook {
        let mut wb = Workbook::with_sheet("Data");
        wb.sheets[0].images.push(Image {
            bytes: Arc::from(if png { TEST_PNG } else { TEST_JPEG }),
            format: if png {
                ImageFormat::Png
            } else {
                ImageFormat::Jpeg
            },
            anchor: Anchor::OneCell {
                cell: "B2".into(),
                col_off: 0,
                row_off: 0,
                width_cm: 4.0,
                height_cm: 3.0,
            },
        });
        wb
    }

    #[test]
    fn image_roundtrips_into_valid_package() {
        let bytes = write_workbook_bytes(&wb_with_image(true)).unwrap();

        // Media part: stored verbatim, never deflated.
        let (method, data, _) = crate::turbo::zipmin::find_entry(&bytes, "xl/media/image1.png")
            .unwrap()
            .unwrap();
        assert_eq!(method, 0, "media must be STORE, not deflate");
        assert_eq!(data, TEST_PNG);

        // Worksheet references its drawing.
        let sheet_xml = entry(&bytes, "xl/worksheets/sheet1.xml");
        assert!(
            sheet_xml.contains(r#"<drawing r:id="rId1"/>"#),
            "{sheet_xml}"
        );

        // Sheet rels point at the drawing part.
        let sheet_rels = entry(&bytes, "xl/worksheets/_rels/sheet1.xml.rels");
        assert!(
            sheet_rels.contains("relationships/drawing")
                && sheet_rels.contains(r#"Target="/xl/drawings/drawing1.xml""#)
                && sheet_rels.contains(r#"Id="rId1""#),
            "{sheet_rels}"
        );

        // Drawing part: one pic referencing the media rel.
        let drawing = entry(&bytes, "xl/drawings/drawing1.xml");
        assert!(drawing.contains("<pic>"), "{drawing}");
        assert!(drawing.contains(r#"<a:blip r:embed="rId1"/>"#), "{drawing}");

        // Drawing rels: image relationship to the media part.
        let drawing_rels = entry(&bytes, "xl/drawings/_rels/drawing1.xml.rels");
        assert!(
            drawing_rels.contains("relationships/image")
                && drawing_rels.contains(r#"Target="../media/image1.png""#)
                && drawing_rels.contains(r#"Id="rId1""#),
            "{drawing_rels}"
        );

        // Content types: png Default + drawing Override.
        let ct = entry(&bytes, "[Content_Types].xml");
        assert!(
            ct.contains(r#"<Default Extension="png" ContentType="image/png"/>"#),
            "{ct}"
        );
        assert!(
            ct.contains(r#"PartName="/xl/drawings/drawing1.xml" ContentType="application/vnd.openxmlformats-officedocument.drawing+xml""#),
            "{ct}"
        );
    }

    #[test]
    fn image_dedup_on_two_sheets_collapses_to_one_media_part() {
        let mut wb = Workbook::with_sheet("A");
        wb.sheets.push(Sheet::new("B"));
        for sh in &mut wb.sheets {
            sh.images.push(Image {
                bytes: Arc::from(TEST_PNG),
                format: ImageFormat::Png,
                anchor: Anchor::default(),
            });
        }
        let bytes = write_workbook_bytes(&wb).unwrap();

        // One media part shared by both sheets.
        let (_, data, _) = crate::turbo::zipmin::find_entry(&bytes, "xl/media/image1.png")
            .unwrap()
            .unwrap();
        assert_eq!(data, TEST_PNG);
        assert!(
            crate::turbo::zipmin::find_entry(&bytes, "xl/media/image2.png")
                .unwrap()
                .is_none(),
            "dedup must not create a second media part"
        );

        // Both drawings reference image1.
        let d1 = entry(&bytes, "xl/drawings/_rels/drawing1.xml.rels");
        let d2 = entry(&bytes, "xl/drawings/_rels/drawing2.xml.rels");
        assert!(d1.contains(r#"Target="../media/image1.png""#), "{d1}");
        assert!(d2.contains(r#"Target="../media/image1.png""#), "{d2}");
    }

    #[test]
    fn image_output_is_byte_identical_across_runs() {
        let wb = wb_with_image(true);
        let a = write_workbook_bytes(&wb).unwrap();
        let b = write_workbook_bytes(&wb).unwrap();
        assert_eq!(a, b, "two runs over the same input must be byte-identical");
    }

    #[test]
    fn chart_and_image_share_one_drawing_part() {
        let mut wb = Workbook::with_sheet("Data");
        wb.sheets[0].charts.push(Chart {
            chart_type: ChartType::Col,
            series: vec![Series {
                cat_ref: Some("Data!$A$1:$A$3".into()),
                val_ref: Some("Data!$B$1:$B$3".into()),
                ..Series::default()
            }],
            ..Chart::default()
        });
        wb.sheets[0].images.push(Image {
            bytes: Arc::from(TEST_JPEG),
            format: ImageFormat::Jpeg,
            anchor: Anchor::default(),
        });
        let bytes = write_workbook_bytes(&wb).unwrap();

        // Exactly one drawing part, containing both a chart frame and a pic.
        let drawing = entry(&bytes, "xl/drawings/drawing1.xml");
        assert!(drawing.contains("<graphicFrame>"), "{drawing}");
        assert!(drawing.contains("<pic>"), "{drawing}");
        assert!(drawing.contains(r#"<a:blip r:embed="rId2"/>"#), "{drawing}");
        assert!(
            crate::turbo::zipmin::find_entry(&bytes, "xl/drawings/drawing2.xml")
                .unwrap()
                .is_none(),
            "images must join the chart drawing, not create a second one"
        );

        // Chart rel rId1, image rel rId2.
        let drawing_rels = entry(&bytes, "xl/drawings/_rels/drawing1.xml.rels");
        assert!(
            drawing_rels.contains(r#"Id="rId1""#) && drawing_rels.contains("relationships/chart"),
            "{drawing_rels}"
        );
        assert!(
            drawing_rels.contains(r#"Id="rId2""#)
                && drawing_rels.contains("relationships/image")
                && drawing_rels.contains(r#"Target="../media/image1.jpeg""#),
            "{drawing_rels}"
        );

        let ct = entry(&bytes, "[Content_Types].xml");
        assert!(
            ct.contains(r#"<Default Extension="jpeg" ContentType="image/jpeg"/>"#),
            "{ct}"
        );
    }

    fn wb_with_three_anchor_images() -> Workbook {
        let mut wb = Workbook::with_sheet("Data");
        wb.sheets[0].images.push(Image {
            bytes: Arc::from(TEST_PNG),
            format: ImageFormat::Png,
            anchor: Anchor::OneCell {
                cell: "B2".into(),
                col_off: 76200,
                row_off: 50800,
                width_cm: 4.0,
                height_cm: 3.0,
            },
        });
        wb.sheets[0].images.push(Image {
            bytes: Arc::from(TEST_JPEG),
            format: ImageFormat::Jpeg,
            anchor: Anchor::TwoCell {
                from_cell: "C3".into(),
                from_off: (1000, 2000),
                to_cell: "F6".into(),
                to_off: (3000, 4000),
                edit_as: Some("oneCell".into()),
            },
        });
        wb.sheets[0].images.push(Image {
            bytes: Arc::from(TEST_PNG),
            format: ImageFormat::Png,
            anchor: Anchor::Absolute {
                x_emu: 1_000_000,
                y_emu: 2_000_000,
                cx_emu: 3_000_000,
                cy_emu: 4_000_000,
            },
        });
        wb
    }

    #[test]
    fn all_three_anchor_kinds_emit_offsets_and_edit_as() {
        let bytes = write_workbook_bytes(&wb_with_three_anchor_images()).unwrap();
        let drawing = entry(&bytes, "xl/drawings/drawing1.xml");
        // oneCell: cell B2 -> col 1 / row 1 (0-based) with EMU offsets.
        assert!(
            drawing.contains(r#"<oneCellAnchor><from><col>1</col><colOff>76200</colOff><row>1</row><rowOff>50800</rowOff></from>"#),
            "{drawing}"
        );
        let cx = crate::turbo::write::charts::cm_to_emu(4.0);
        let cy = crate::turbo::write::charts::cm_to_emu(3.0);
        assert!(
            drawing.contains(&format!(r#"<ext cx="{cx}" cy="{cy}"/>"#)),
            "{drawing}"
        );
        // twoCell: from C3, to F6, with offsets and editAs.
        assert!(
            drawing.contains(
                r#"<twoCellAnchor editAs="oneCell"><from><col>2</col><colOff>1000</colOff><row>2</row><rowOff>2000</rowOff></from><to><col>5</col><colOff>3000</colOff><row>5</row><rowOff>4000</rowOff></to>"#
            ),
            "{drawing}"
        );
        // absolute: pos + ext in EMU.
        assert!(
            drawing.contains(
                r#"<absoluteAnchor><pos x="1000000" y="2000000"/><ext cx="3000000" cy="4000000"/>"#
            ),
            "{drawing}"
        );
    }

    #[test]
    fn dedup_with_different_anchors_collapses_media_but_keeps_both_pics() {
        let wb = wb_with_three_anchor_images();
        let bytes = write_workbook_bytes(&wb).unwrap();

        // Identical PNG bytes (images 1 and 3) share ONE media part; the JPEG is
        // a second part. Two distinct byte sets -> exactly two media parts.
        let (_, png_data, _) = crate::turbo::zipmin::find_entry(&bytes, "xl/media/image1.png")
            .unwrap()
            .unwrap();
        assert_eq!(png_data, TEST_PNG);
        assert!(
            crate::turbo::zipmin::find_entry(&bytes, "xl/media/image2.png")
                .unwrap()
                .is_none(),
            "second PNG must be deduped into image1.png"
        );
        let (_, jpeg_data, _) = crate::turbo::zipmin::find_entry(&bytes, "xl/media/image2.jpeg")
            .unwrap()
            .unwrap();
        assert_eq!(jpeg_data, TEST_JPEG);

        // Two anchor entries reference the SAME media part (rId1 and rId3).
        let drawing = entry(&bytes, "xl/drawings/drawing1.xml");
        assert_eq!(drawing.matches("<pic>").count(), 3, "{drawing}");
        assert_eq!(
            drawing.matches(r#"<a:blip r:embed="#).count(),
            3,
            "every pic needs a blip: {drawing}"
        );
        let drels = entry(&bytes, "xl/drawings/_rels/drawing1.xml.rels");
        let target = r#"Target="../media/image1.png""#;
        let first = drels.find(target).expect("media rel present");
        assert!(
            drels[first + 1..].contains(target),
            "identical bytes with different anchors must both reference image1.png: {drels}"
        );
        assert_eq!(
            drels.matches(target).count(),
            2,
            "exactly two rels must point at the shared media part: {drels}"
        );
    }
}
