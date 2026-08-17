//! Zip container-level checks for the validator.
//!
//! CRC verification reuses the writer's IEEE CRC-32. Duplicate entry names in
//! the central directory are flagged because a part read by name is ambiguous.

pub(crate) use crate::turbo::write::zip::crc32_ieee;

use std::collections::HashSet;

use super::{Finding, FindingCode, Severity, ValidateReport};
use crate::turbo::zipmin::ZipEntryMeta;

/// Flag duplicate entry names (a later record silently shadows the earlier one).
pub fn check_duplicate_names(entries: &[ZipEntryMeta], report: &mut ValidateReport) {
    let mut seen: HashSet<&str> = HashSet::new();
    for e in entries {
        if !seen.insert(e.name.as_str()) {
            report.add(Finding::new(
                FindingCode::DuplicateEntry,
                Severity::Warning,
                e.name.clone(),
                None,
                "entry appears more than once in the central directory",
                false,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn duplicate_names_flagged_once_each() {
        let mut report = ValidateReport::default();
        let mk = |name: &str| ZipEntryMeta {
            name: name.to_string(),
            compression_method: 0,
            crc32: 0,
            compressed_size: 0,
            uncompressed_size: 0,
            local_header_offset: 0,
            data_offset: 0,
        };
        check_duplicate_names(&[mk("a.xml"), mk("a.xml"), mk("b.xml")], &mut report);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].code, FindingCode::DuplicateEntry);
    }
}
