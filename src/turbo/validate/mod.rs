//! C2 — validate & repair (Tier 3 HIGH).
//!
//! Surfaces the tolerant reader's degradation as a **checked report** instead
//! of a success-or-throw. `validate_workbook` never throws for an input
//! problem: a missing file, an encrypted workbook, a legacy .xls, a non-zip,
//! a corrupt zip, or a valid zip that is not an OOXML package all become
//! `Finding`s inside an `Ok(report)`. `repair_workbook` applies the opt-in,
//! conservative fixes and writes a corrected copy — never the source.
//!
//! Severity model (applied consistently everywhere):
//! - [`Severity::Error`]: Excel refuses to open, or opening destroys data.
//! - [`Severity::Warning`]: Excel opens but silently repairs / reads wrong.
//! - [`Severity::Info`]: untidy but harmless.
//!
//! Every finding carries a stable machine-readable [`FindingCode`] so callers
//! can branch on the code without string matching.

pub(crate) mod container;
/// Phase 3 feature presence, decided in the central-directory walk this module
/// already does. Free here and nowhere else — see `PERF_EXPERIMENTS_PHASE3.md`.
pub mod features;
pub(crate) mod parts;
pub(crate) mod repair;
pub(crate) mod sheet;
pub(crate) mod xml;
pub(crate) mod zip;

#[cfg(feature = "python")]
pub mod python;

use std::collections::HashMap;
use std::sync::Arc;

use crate::turbo::error::TurboResult;
use crate::turbo::zipmin::{ZipEntryMeta, inflate_entry, list_entries};

use repair::Fix;
use zip::{check_duplicate_names, crc32_ieee};

/// Stable machine-readable finding codes. Callers branch on these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FindingCode {
    /// Could not open/read the file at all (missing, permissions, I/O).
    Io,
    /// ECMA-376 encrypted workbook (OLE/CFB with an `EncryptionInfo` stream).
    EncryptedWorkbook,
    /// Legacy .xls (BIFF) container — CFB with a `Workbook`/`Book` stream.
    LegacyBiff,
    /// OLE/CFB container that is neither encrypted OOXML nor BIFF.
    UnknownCfb,
    /// Zip signature present but the central directory / EOCD is invalid.
    CorruptZip,
    /// A valid ZIP that is not a SpreadsheetML (OOXML) package.
    NotOoxmlPackage,
    /// No recognized spreadsheet container signature at all.
    NotSpreadsheet,
    /// An entry's declared CRC does not match its decompressed bytes.
    CrcMismatch,
    /// An entry could not be inflated.
    PartUnreadable,
    /// An entry name appears more than once in the central directory.
    DuplicateEntry,
    /// Required part `[Content_Types].xml` is missing.
    MissingContentTypes,
    /// Required part `_rels/.rels` is missing.
    MissingRootRels,
    /// Required part `xl/workbook.xml` is missing.
    MissingWorkbook,
    /// A sheet bound by the workbook has no worksheet part in the archive.
    MissingSheetPart,
    /// The workbook declares no sheets at all.
    NoSheets,
    /// No content-type override for a required part.
    MissingContentType,
    /// A declared content type does not match the part it names.
    ContentTypeMismatch,
    /// A content-type override names a part that does not exist.
    MissingPartForContentType,
    /// The root relationships have no officeDocument relationship.
    MissingOfficeDocumentRel,
    /// A relationship points at a part that does not exist.
    DanglingRel,
    /// A use site references a relationship id that is not in the rels file.
    MissingRel,
    /// A use site's relationship is of the wrong kind.
    WrongRelKind,
    /// A worksheet part exists but no workbook sheet binds it.
    SheetNotReferenced,
    /// A pivot table has no cacheDefinition relationship.
    PivotCacheMissingRel,
    /// A threaded comment references a person that does not exist.
    ThreadedUnknownPerson,
    /// A Phase 3 feature area is present in the package (slicers, sparklines,
    /// rich data, Power Query, controls, external links, a signature).
    ///
    /// Not a defect. It is reported because a pipeline needs to know what a
    /// workbook *contains* before it decides how to handle it, and this is the
    /// only place that answer is free — the central-directory walk has already
    /// happened by the time validation runs.
    FeaturePresent,
    /// An XML part is not well-formed.
    XmlNotWellformed,
    /// The declared `<dimension>` disagrees with the actual used range.
    DimensionMismatch,
    /// A cell reference is outside the 1,048,576 × 16,384 grid.
    CellOutOfGrid,
    /// A cell reference cannot be parsed.
    InvalidCellRef,
    /// Two cells share the same reference.
    DuplicateCell,
    /// `<row @r>` values are not monotonically increasing.
    RowOutOfOrder,
    /// Two merged ranges overlap; Excel silently drops one.
    OverlappingMerge,
    /// A cell style index is out of range of the cellXfs.
    StyleIndexOor,
    /// A shared-string index is out of range of the sharedStrings part.
    SharedStringIndexOor,
    /// A conditional-format dxfId is out of range of the dxfs.
    DxfIndexOor,
    /// A formula references a sheet that does not exist.
    FormulaMissingSheet,
    /// A defined name points at #REF!.
    DefinedNameRefError,
    /// A data validation has an empty `sqref`.
    EmptyValidationSqref,
    /// A `<col>` has `min` greater than `max`.
    InvertedColRange,
}

impl FindingCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            FindingCode::Io => "io_error",
            FindingCode::EncryptedWorkbook => "encrypted_workbook",
            FindingCode::LegacyBiff => "legacy_biff",
            FindingCode::UnknownCfb => "unknown_cfb",
            FindingCode::CorruptZip => "corrupt_zip",
            FindingCode::NotOoxmlPackage => "not_ooxml_package",
            FindingCode::NotSpreadsheet => "not_spreadsheet",
            FindingCode::CrcMismatch => "crc_mismatch",
            FindingCode::PartUnreadable => "part_unreadable",
            FindingCode::DuplicateEntry => "duplicate_entry",
            FindingCode::MissingContentTypes => "missing_content_types",
            FindingCode::MissingRootRels => "missing_root_rels",
            FindingCode::MissingWorkbook => "missing_workbook",
            FindingCode::MissingSheetPart => "missing_sheet_part",
            FindingCode::NoSheets => "no_sheets",
            FindingCode::MissingContentType => "missing_content_type",
            FindingCode::ContentTypeMismatch => "content_type_mismatch",
            FindingCode::MissingPartForContentType => "missing_part_for_content_type",
            FindingCode::MissingOfficeDocumentRel => "missing_office_document_rel",
            FindingCode::DanglingRel => "dangling_rel",
            FindingCode::MissingRel => "missing_rel",
            FindingCode::WrongRelKind => "wrong_rel_kind",
            FindingCode::SheetNotReferenced => "sheet_not_referenced",
            FindingCode::PivotCacheMissingRel => "pivot_cache_missing_rel",
            FindingCode::ThreadedUnknownPerson => "threaded_unknown_person",
            FindingCode::FeaturePresent => "feature_present",
            FindingCode::XmlNotWellformed => "xml_not_wellformed",
            FindingCode::DimensionMismatch => "dimension_mismatch",
            FindingCode::CellOutOfGrid => "cell_out_of_grid",
            FindingCode::InvalidCellRef => "invalid_cell_ref",
            FindingCode::DuplicateCell => "duplicate_cell",
            FindingCode::RowOutOfOrder => "row_out_of_order",
            FindingCode::OverlappingMerge => "overlapping_merge",
            FindingCode::StyleIndexOor => "style_index_oor",
            FindingCode::SharedStringIndexOor => "shared_string_index_oor",
            FindingCode::DxfIndexOor => "dxf_index_oor",
            FindingCode::FormulaMissingSheet => "formula_missing_sheet",
            FindingCode::DefinedNameRefError => "defined_name_ref_error",
            FindingCode::EmptyValidationSqref => "empty_validation_sqref",
            FindingCode::InvertedColRange => "inverted_col_range",
        }
    }
}

/// How bad a finding is. Error > Warning > Info.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        }
    }
}

/// One problem found in the workbook.
#[derive(Clone, Debug)]
pub struct Finding {
    pub code: FindingCode,
    pub severity: Severity,
    /// The zip part the problem is in, or "package" for container-level issues.
    pub part: String,
    /// Optional human-readable location (A1 ref, rel id, byte offset, ...).
    pub location: Option<String>,
    /// Human-readable message.
    pub message: String,
    /// Whether a conservative repair exists for this finding.
    pub repairable: bool,
}

impl Finding {
    fn new(
        code: FindingCode,
        severity: Severity,
        part: impl Into<String>,
        location: Option<String>,
        message: impl Into<String>,
        repairable: bool,
    ) -> Self {
        Finding {
            code,
            severity,
            part: part.into(),
            location,
            message: message.into(),
            repairable,
        }
    }

    fn io(e: std::io::Error, path: &str) -> Self {
        Finding::new(
            FindingCode::Io,
            Severity::Error,
            "package",
            Some(path.to_string()),
            format!("could not open file: {e}"),
            false,
        )
    }
}

/// The validation report: every finding plus per-severity counts.
#[derive(Clone, Debug, Default)]
pub struct ValidateReport {
    pub findings: Vec<Finding>,
    pub errors: usize,
    pub warnings: usize,
    pub infos: usize,
}

impl ValidateReport {
    fn add(&mut self, f: Finding) {
        match f.severity {
            Severity::Error => self.errors += 1,
            Severity::Warning => self.warnings += 1,
            Severity::Info => self.infos += 1,
        }
        self.findings.push(f);
    }

    /// True when there are no findings at all.
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }
}

/// Repair options: opt-in per severity (and optionally per code).
#[derive(Clone, Debug)]
pub struct RepairOptions {
    /// Apply repairs for findings of this severity and above. Default `Warning`.
    pub max_severity: Severity,
    /// Restrict repairs to these codes, if set.
    pub allowed_codes: Option<Vec<FindingCode>>,
}

impl Default for RepairOptions {
    fn default() -> Self {
        RepairOptions {
            max_severity: Severity::Warning,
            allowed_codes: None,
        }
    }
}

/// One applied repair, reported so the user can audit it.
#[derive(Clone, Debug)]
pub struct RepairAction {
    pub code: FindingCode,
    pub severity: Severity,
    pub part: String,
    pub description: String,
    pub before: String,
    pub after: String,
}

/// Everything the validation pass learned, shared with repair.
pub(crate) struct Inspect {
    pub report: ValidateReport,
    pub fixes: Vec<Fix>,
    pub parts: HashMap<String, Vec<u8>>,
    pub entries: Vec<ZipEntryMeta>,
    pub zip: Arc<Vec<u8>>,
    pub is_package: bool,
}

/// Validate a workbook path. Returns a report for ANY input problem — this is
/// the entire point of the feature — and only errors on an internal bug.
pub fn validate_workbook(path: &str) -> TurboResult<ValidateReport> {
    Ok(inspect(path)?.report)
}

/// Repair a workbook conservatively: apply the opt-in fixes to an in-memory
/// copy and write `out_path`. The source file is never modified. When the
/// input is not a readable OOXML package (encrypted, legacy, corrupt, not a
/// spreadsheet, or unreadable), nothing is written and the report carries the
/// classification finding.
pub fn repair_workbook(
    path: &str,
    out_path: &str,
    opts: &RepairOptions,
) -> TurboResult<(ValidateReport, Vec<RepairAction>, bool)> {
    let mut insp = inspect(path)?;
    if !insp.is_package {
        return Ok((insp.report, Vec::new(), false));
    }
    let mut actions = Vec::new();
    for fix in &insp.fixes {
        if fix.severity < opts.max_severity {
            continue;
        }
        if let Some(allowed) = &opts.allowed_codes {
            if !allowed.contains(&fix.code) {
                continue;
            }
        }
        actions.push(fix.apply(&mut insp.parts));
    }
    let fixed_parts: Vec<String> = actions
        .iter()
        .map(|a| a.part.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let zip_out = repair::rewrite_zip(&insp.zip, &insp.entries, &mut insp.parts, &fixed_parts)?;
    std::fs::write(out_path, zip_out).map_err(crate::turbo::error::TurboError::Io)?;
    Ok((insp.report, actions, true))
}

/// The single driver for both validate and repair.
fn inspect(path: &str) -> TurboResult<Inspect> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            let mut report = ValidateReport::default();
            report.add(Finding::io(e, path));
            return Ok(Inspect {
                report,
                fixes: Vec::new(),
                parts: HashMap::new(),
                entries: Vec::new(),
                zip: Arc::new(Vec::new()),
                is_package: false,
            });
        }
    };
    let zip = Arc::new(bytes);
    inspect_bytes(zip, path)
}

fn inspect_bytes(zip: Arc<Vec<u8>>, _path: &str) -> TurboResult<Inspect> {
    let mut report = ValidateReport::default();
    let mut fixes = Vec::new();

    // Step 0 — container classification. Anything that is not a parseable
    // OOXML package gets a distinct finding, never generic corruption.
    if container::is_cfb(&zip) {
        container::classify_cfb(&zip, &mut report);
        return Ok(Inspect {
            report,
            fixes,
            parts: HashMap::new(),
            entries: Vec::new(),
            zip,
            is_package: false,
        });
    }

    let (entries, errs) = match list_entries(&zip) {
        Ok(x) => x,
        Err(e) => {
            report.add(Finding::new(
                if looks_like_zip(&zip) {
                    FindingCode::CorruptZip
                } else {
                    FindingCode::NotSpreadsheet
                },
                Severity::Error,
                "package",
                None,
                if looks_like_zip(&zip) {
                    format!("invalid ZIP: {e}")
                } else {
                    format!(
                        "not a recognized spreadsheet container (expected an OOXML ZIP, OLE/CFB, or BIFF file): {e}"
                    )
                },
                false,
            ));
            return Ok(Inspect {
                report,
                fixes,
                parts: HashMap::new(),
                entries: Vec::new(),
                zip,
                is_package: false,
            });
        }
    };

    let mut parts: HashMap<String, Vec<u8>> = HashMap::new();
    for meta in &entries {
        match inflate_entry(&zip, meta) {
            Ok(data) => {
                if crc32_ieee(&data) != meta.crc32 {
                    report.add(Finding::new(
                        FindingCode::CrcMismatch,
                        Severity::Error,
                        meta.name.clone(),
                        None,
                        format!(
                            "CRC mismatch: declared {:08X}, computed {:08X}",
                            meta.crc32,
                            crc32_ieee(&data)
                        ),
                        false,
                    ));
                }
                if is_interesting(&meta.name) {
                    parts.insert(meta.name.clone(), data);
                }
            }
            Err(e) => {
                report.add(Finding::new(
                    FindingCode::PartUnreadable,
                    Severity::Error,
                    meta.name.clone(),
                    None,
                    format!("could not inflate entry: {e}"),
                    false,
                ));
            }
        }
    }
    for e in &errs {
        report.add(Finding::new(
            FindingCode::CorruptZip,
            Severity::Error,
            "package",
            Some(e.clone()),
            e.clone(),
            false,
        ));
    }
    check_duplicate_names(&entries, &mut report);

    // Root presence.
    if !parts.contains_key("[Content_Types].xml") {
        report.add(Finding::new(
            FindingCode::MissingContentTypes,
            Severity::Error,
            "[Content_Types].xml",
            None,
            "missing [Content_Types].xml",
            false,
        ));
    }
    if !parts.contains_key("xl/workbook.xml") {
        report.add(Finding::new(
            FindingCode::MissingWorkbook,
            Severity::Error,
            "xl/workbook.xml",
            None,
            "missing xl/workbook.xml",
            false,
        ));
    }
    if !parts.contains_key("_rels/.rels") {
        report.add(Finding::new(
            FindingCode::MissingRootRels,
            Severity::Error,
            "_rels/.rels",
            None,
            "missing _rels/.rels",
            false,
        ));
    }

    // A valid zip that lacks both OOXML roots is not a package.
    if !parts.contains_key("[Content_Types].xml") && !parts.contains_key("xl/workbook.xml") {
        report.add(Finding::new(
            FindingCode::NotOoxmlPackage,
            Severity::Error,
            "package",
            None,
            "valid ZIP but not a SpreadsheetML (OOXML) package",
            false,
        ));
        return Ok(Inspect {
            report,
            fixes,
            parts,
            entries,
            zip,
            is_package: false,
        });
    }

    let entries_map: HashMap<String, ZipEntryMeta> = entries
        .iter()
        .map(|m| (m.name.clone(), m.clone()))
        .collect();

    xml::check_wellformedness(&parts, &mut report);
    parts::check_content_types(&parts, &entries_map, &mut report, &mut fixes);
    parts::check_rels(&parts, &entries_map, &mut report, &mut fixes);
    sheet::check_sheets(&parts, &mut report, &mut fixes);

    Ok(Inspect {
        report,
        fixes,
        parts,
        entries,
        zip,
        is_package: true,
    })
}

/// Hold inflated bytes only for the parts the semantic checks need.
fn is_interesting(name: &str) -> bool {
    name.ends_with(".xml") || name.ends_with(".rels")
}

fn looks_like_zip(b: &[u8]) -> bool {
    b.starts_with(b"PK\x03\x04") || b.starts_with(b"PK\x05\x06") || b.starts_with(b"PK\x01\x02")
}

// ----------------------------------------------------------------------------
// Shared XML scanning helpers (validate-local; scan.rs is owned by another
// agent and must not be edited).
// ----------------------------------------------------------------------------

/// Index of the `>` that ends the tag starting at `from`, honoring quoted
/// attribute values so `>` inside a value does not terminate the tag early.
pub(crate) fn tag_end(xml: &[u8], from: usize) -> Option<usize> {
    let mut quote: Option<u8> = None;
    let mut i = from;
    while i < xml.len() {
        let b = xml[i];
        if let Some(q) = quote {
            if b == q {
                quote = None;
            }
        } else if b == b'"' || b == b'\'' {
            quote = Some(b);
        } else if b == b'>' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Value of an XML attribute inside an open-tag byte slice (`name="..."`).
/// Allocation-free: matches the name, then requires `="` immediately after.
pub(crate) fn attr<'a>(tag: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    let mut search = 0usize;
    while search < tag.len() {
        let Some(o) = memchr::memmem::find(&tag[search..], name) else {
            break;
        };
        let o = search + o;
        let after = o + name.len();
        if tag.get(after) == Some(&b'=') && tag.get(after + 1) == Some(&b'"') {
            let vs = after + 2;
            let q = memchr::memchr(b'"', &tag[vs..])?;
            return Some(&tag[vs..vs + q]);
        }
        search = o + 1;
    }
    None
}

pub(crate) fn utf8(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// `A1` (or `$A$1`) → (1-based row, 1-based col). `None` when unparseable.
/// Allocation-free.
pub(crate) fn a1_to_rc(s: &[u8]) -> Option<(u32, u32)> {
    let mut i = 0usize;
    while i < s.len() && s[i] == b'$' {
        i += 1;
    }
    let col_start = i;
    while i < s.len() && s[i].is_ascii_alphabetic() {
        i += 1;
    }
    if i == col_start {
        return None;
    }
    let mut col: u32 = 0;
    for &b in &s[col_start..i] {
        col = col
            .checked_mul(26)?
            .checked_add((b.to_ascii_uppercase() - b'A' + 1) as u32)?;
    }
    while i < s.len() && s[i] == b'$' {
        i += 1;
    }
    if i == s.len() {
        return None;
    }
    let row: u32 = std::str::from_utf8(&s[i..]).ok()?.trim().parse().ok()?;
    if row == 0 {
        return None;
    }
    Some((row, col))
}

/// Parse `A1`, `A1:C3`, `A:C`, `1:5` into normalized (r0, c0, r1, c1), 1-based.
/// Returns `None` for anything that is not a clean range.
pub(crate) fn parse_range(s: &[u8]) -> Option<(u32, u32, u32, u32)> {
    let clean: Vec<u8> = s.iter().copied().filter(|&b| b != b'$').collect();
    if clean.is_empty() {
        return None;
    }
    let colon = clean.iter().position(|&b| b == b':');
    let (p0, p1) = match colon {
        None => (&clean[..], None),
        Some(c) => (&clean[..c], Some(&clean[c + 1..])),
    };
    if p0.is_empty() || p1 == Some(&[]) {
        return None;
    }
    let (mut r0, mut c0) = a1_to_rc(p0)?;
    let (mut r1, mut c1) = match p1 {
        Some(p) => a1_to_rc(p)?,
        None => (r0, c0),
    };
    if r0 > r1 {
        std::mem::swap(&mut r0, &mut r1);
    }
    if c0 > c1 {
        std::mem::swap(&mut c0, &mut c1);
    }
    Some((r0, c0, r1, c1))
}

pub(crate) fn range_to_a1((r0, c0, r1, c1): (u32, u32, u32, u32)) -> String {
    if r0 == r1 && c0 == c1 {
        a1_of(r0, c0)
    } else {
        format!("{}:{}", a1_of(r0, c0), a1_of(r1, c1))
    }
}

/// `(row, col)` 1-based → `A1` string.
pub(crate) fn a1_of(row: u32, col: u32) -> String {
    let mut s = String::with_capacity(8);
    let mut c = col;
    let mut stack = [0u8; 8];
    let mut n = 0usize;
    while c > 0 {
        c -= 1;
        stack[n] = b'A' + (c % 26) as u8;
        c /= 26;
        n += 1;
    }
    for &b in stack[..n].iter().rev() {
        s.push(b as char);
    }
    s.push_str(&row.to_string());
    s
}

/// Local (namespace-stripped) name of the open tag whose `>` is at `gt`.
pub(crate) fn tag_local_name(tag: &[u8]) -> &[u8] {
    let name_end = tag
        .iter()
        .position(|&c| c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' || c == b'/')
        .unwrap_or(tag.len());
    let raw = &tag[..name_end];
    match memchr::memchr(b':', raw) {
        Some(c) => &raw[c + 1..],
        None => raw,
    }
}

/// Find the next `<name` / `<prefix:name` open tag at or after `from`,
/// returning its absolute start offset. Matches `<c ` but not `</c>` or `<cols>`.
pub(crate) fn find_tag(hay: &[u8], name: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i < hay.len() {
        let Some(o) = memchr::memchr(b'<', &hay[i..]) else {
            break;
        };
        let s = i + o;
        let rest = &hay[s + 1..];
        let mut j = 0usize;
        while j < rest.len()
            && (rest[j].is_ascii_alphanumeric() || rest[j] == b'_' || rest[j] == b'-')
        {
            j += 1;
        }
        let (mut ns, mut ne) = (0usize, j);
        if j < rest.len() && rest[j] == b':' {
            ns = j + 1;
            ne = ns;
            while ne < rest.len()
                && (rest[ne].is_ascii_alphanumeric() || rest[ne] == b'_' || rest[ne] == b'-')
            {
                ne += 1;
            }
        }
        if &rest[ns..ne] == name {
            let after = rest.get(ne).copied().unwrap_or(b'>');
            if matches!(after, b' ' | b'>' | b'/' | b'\t' | b'\n' | b'\r') {
                return Some(s);
            }
        }
        i = s + 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::hint::black_box;
    use std::time::Instant;

    #[test]
    fn a1_roundtrip() {
        assert_eq!(a1_to_rc(b"A1"), Some((1, 1)));
        assert_eq!(a1_to_rc(b"$A$1"), Some((1, 1)));
        assert_eq!(a1_to_rc(b"XFD1048576"), Some((1_048_576, 16_384)));
        assert_eq!(a1_to_rc(b"Z9"), Some((9, 26)));
        assert_eq!(a1_to_rc(b""), None);
        assert_eq!(a1_of(1, 1), "A1");
        assert_eq!(a1_of(1_048_576, 16_384), "XFD1048576");
        assert_eq!(range_to_a1((1, 1, 3, 2)), "A1:B3");
    }

    /// Validation cost vs a plain read. The point of the feature is that
    /// validation is cheap enough for a pipeline, so the ratio is pinned here.
    ///
    /// RELEASE ONLY, deliberately. Unlike the ratio gates in `turbo::perfgate`,
    /// this comparison is not symmetric under a debug build: validation is
    /// dominated by many small calls (per-entry CRC, per-tag scanning,
    /// wellformedness walks) while the plain read is dominated by a few tight
    /// loops. Debug penalises the former far more than the latter, so the same
    /// code measures 4.1x in release and 7-13x in debug — the gate would be
    /// permanently red for anyone running a plain `cargo test`, which makes it
    /// noise rather than a signal.
    ///
    /// Measured in release: 4.11x on this fixture, and 2.5x (mixed.xlsx) to
    /// 5.8x (styled.xlsx) on the real corpus — all well under the 10x mark
    /// where a pipeline would stop using it.
    ///
    ///     cargo test --release --lib --features __arrow validate_cost -- --nocapture
    #[test]
    #[cfg_attr(
        debug_assertions,
        ignore = "timing gate: release only, see doc comment"
    )]
    fn validate_cost_stays_near_plain_read() {
        use crate::turbo::write::zip::ZipWriter;
        use crate::turbo::{Features, read_workbook_turbo};

        let path = {
            let mut p = std::env::temp_dir();
            p.push("kyrax_validate_perfgate.xlsx");
            p
        };
        let ct = b"<?xml version=\"1.0\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
            <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
            <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
            <Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/>\
            <Override PartName=\"/xl/worksheets/sheet1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>\
            <Override PartName=\"/xl/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml\"/>\
            </Types>";
        let root = b"<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
            <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>";
        let wb = b"<?xml version=\"1.0\"?><workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
            <sheets><sheet name=\"S\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>";
        let wb_rels = b"<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
            <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet1.xml\"/>\
            <Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/></Relationships>";
        let styles = b"<?xml version=\"1.0\"?><styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">\
            <fonts count=\"1\"><font/></fonts><fills count=\"1\"><fill/></fills>\
            <borders count=\"1\"><border/></borders>\
            <cellXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/></cellXfs></styleSheet>";
        let rows = 5000usize;
        let mut sheet = String::with_capacity(rows * 220);
        sheet.push_str(
            "<?xml version=\"1.0\"?><worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><dimension ref=\"A1:H5000\"/><sheetData>",
        );
        for r in 1..=rows {
            sheet.push_str(&format!("<row r=\"{r}\">"));
            for c in 1..=8u32 {
                let col = a1_of(r as u32, c);
                sheet.push_str(&format!("<c r=\"{col}\"><v>{r}</v></c>"));
            }
            sheet.push_str("</row>");
        }
        sheet.push_str("</sheetData></worksheet>");

        let mut z = ZipWriter::new();
        z.add("[Content_Types].xml", ct);
        z.add("_rels/.rels", root);
        z.add("xl/workbook.xml", wb);
        z.add("xl/_rels/workbook.xml.rels", wb_rels);
        z.add("xl/worksheets/sheet1.xml", sheet.as_bytes());
        z.add("xl/styles.xml", styles);
        std::fs::write(&path, z.finish().unwrap()).unwrap();
        let path = path.to_str().unwrap().to_string();

        let validate = || {
            black_box(validate_workbook(&path).expect("validate"));
        };
        let read = || {
            black_box(read_workbook_turbo(&path, Features::ALL).expect("read"));
        };
        // Warm up both so a cold first call's allocator cost is not measured.
        validate();
        read();
        let mut best_v = u64::MAX;
        let mut best_r = u64::MAX;
        for _ in 0..7 {
            let t = Instant::now();
            validate();
            best_v = best_v.min(t.elapsed().as_micros() as u64);
            let t = Instant::now();
            read();
            best_r = best_r.min(t.elapsed().as_micros() as u64);
        }
        let ratio = best_v as f64 / best_r as f64;
        println!(
            "validate perf gate: {rows} rows x 8 cols — validate {best_v}us, plain read {best_r}us, ratio {ratio:.2}x"
        );
        // Validation is for pipelines: the requirement is that it is nowhere
        // near 10x a plain read. The measured corpus ratio is reported by the
        // implementing agent; this gate pins a tolerant upper bound so machine
        // contention on a shared runner does not flake it.
        assert!(
            ratio < 6.0,
            "validation cost {ratio:.2}x the plain read — the pipeline check is too expensive"
        );
    }
}
