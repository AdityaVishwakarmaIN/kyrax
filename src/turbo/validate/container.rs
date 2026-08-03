//! Step 0 — container classification.
//!
//! Distinguishes the inputs a validation pipeline actually sees, so an
//! encrypted workbook, a legacy .xls, or a non-spreadsheet file is never
//! reported as generic corruption:
//! - OLE/CFB + `EncryptionInfo` stream → encrypted workbook (reserved for the
//!   C1 encryption agent; validation never attempts decryption).
//! - OLE/CFB + `Workbook`/`Book` stream → legacy .xls (BIFF).
//! - OLE/CFB with neither → unknown CFB container.
//! - Anything else is handed back to the zip path.

use super::{Finding, FindingCode, Severity, ValidateReport};

/// OLE/CFB compound-file signature.
pub const CFB_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

pub fn is_cfb(bytes: &[u8]) -> bool {
    bytes.len() >= 8 && bytes[..8] == CFB_MAGIC
}

fn u32le(b: &[u8], o: usize) -> u32 {
    b[o] as u32 | (b[o + 1] as u32) << 8 | (b[o + 2] as u32) << 16 | (b[o + 3] as u32) << 24
}

/// Classify a CFB container and record the appropriate finding.
pub fn classify_cfb(bytes: &[u8], report: &mut ValidateReport) {
    match cfb_stream_names(bytes) {
        Some(names) => {
            if names.iter().any(|n| n == "EncryptionInfo") {
                report.add(Finding::new(
                    FindingCode::EncryptedWorkbook,
                    Severity::Error,
                    "package",
                    None,
                    "encrypted workbook (ECMA-376 OLE/CFB); validation cannot read it - use the encryption-aware path",
                    false,
                ));
            } else if names.iter().any(|n| n == "Workbook" || n == "Book") {
                report.add(Finding::new(
                    FindingCode::LegacyBiff,
                    Severity::Error,
                    "package",
                    None,
                    "legacy .xls (BIFF) container, not OOXML - use the legacy reader",
                    false,
                ));
            } else {
                report.add(Finding::new(
                    FindingCode::UnknownCfb,
                    Severity::Error,
                    "package",
                    None,
                    "OLE/CFB container that is neither an encrypted OOXML package nor a recognized BIFF workbook",
                    false,
                ));
            }
        }
        None => report.add(Finding::new(
            FindingCode::UnknownCfb,
            Severity::Error,
            "package",
            None,
            "OLE/CFB container whose directory could not be read",
            false,
        )),
    }
}

/// Tolerant OLE/CFB directory scan: returns the stream names in the root
/// directory. Follows the FAT chain so it is correct for real files, but fails
/// soft (None) on any structural problem so classification never panics.
fn cfb_stream_names(bytes: &[u8]) -> Option<Vec<String>> {
    if bytes.len() < 512 {
        return None;
    }
    let h = &bytes[..512];
    let sect_shift = u16::from_le_bytes([h[22], h[23]]) as usize;
    if !(7..=20).contains(&sect_shift) {
        return None;
    }
    let sect_size = 1usize << sect_shift;
    let nfat = u32le(h, 32) as usize;
    if nfat == 0 || nfat > 4096 {
        return None;
    }
    let mut fat_sectors = Vec::with_capacity(nfat);
    for i in 0..nfat.min(109) {
        let v = u32le(h, 76 + i * 4);
        if v == 0xFFFF_FFFF || v == 0xFFFF_FFFE {
            break;
        }
        fat_sectors.push(v as usize);
    }
    if fat_sectors.is_empty() {
        return None;
    }
    let mut fat: Vec<u32> = Vec::with_capacity(fat_sectors.len() * (sect_size / 4));
    for fs in &fat_sectors {
        let off = 512usize.checked_add(fs.checked_mul(sect_size)?)?;
        if off + sect_size > bytes.len() {
            return None;
        }
        for i in 0..sect_size / 4 {
            fat.push(u32le(bytes, off + i * 4));
        }
    }
    let dir_start = u32le(h, 36) as usize;
    if dir_start == 0xFFFF_FFFF || dir_start == 0xFFFF_FFFE {
        return None;
    }
    let mut names: Vec<String> = Vec::new();
    let mut sector = dir_start;
    let mut guard = 0usize;
    while sector < 0xFFFF_FFF0 && guard < 4096 {
        guard += 1;
        let off = 512usize.checked_add(sector.checked_mul(sect_size)?)?;
        if off + sect_size > bytes.len() {
            return None;
        }
        let n_entries = sect_size / 128;
        for i in 0..n_entries {
            let e = off + i * 128;
            if e + 128 > bytes.len() {
                return None;
            }
            let name_len = u16::from_le_bytes([bytes[e + 64], bytes[e + 65]]) as usize;
            // Unused directory slots are zeroed padding: stop, do not fail.
            if name_len == 0 {
                break;
            }
            if name_len > 64 || name_len % 2 != 0 {
                break;
            }
            let mut name = String::new();
            for k in 0..name_len / 2 {
                let c = u16::from_le_bytes([bytes[e + k * 2], bytes[e + k * 2 + 1]]);
                if c == 0 {
                    break;
                }
                name.push(char::from_u32(c as u32)?);
            }
            if name.is_empty() {
                break;
            }
            names.push(name);
        }
        let next = fat.get(sector).copied().unwrap_or(0xFFFF_FFFE);
        if next == 0xFFFF_FFFE || next == 0xFFFF_FFFF {
            break;
        }
        sector = next as usize;
    }
    Some(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal OLE/CFB skeleton: 512-byte header, one FAT sector,
    /// one directory sector carrying the given stream names.
    fn cfb_skeleton(streams: &[&str]) -> Vec<u8> {
        let sect_size = 512usize;
        let mut out = vec![0u8; 512 + 2 * sect_size];
        out[0..8].copy_from_slice(&CFB_MAGIC);
        out[22..24].copy_from_slice(&9u16.to_le_bytes()); // 512-byte sectors
        out[24..26].copy_from_slice(&6u16.to_le_bytes()); // 64-byte mini sectors
        out[32..36].copy_from_slice(&1u32.to_le_bytes()); // 1 FAT sector
        out[36..40].copy_from_slice(&1u32.to_le_bytes()); // first directory sector = 1
        out[44..48].copy_from_slice(&0x1000u32.to_le_bytes()); // mini-stream cutoff
        out[48..52].copy_from_slice(&0xFFFF_FFFEu32.to_le_bytes()); // no mini FAT
        out[56..60].copy_from_slice(&0xFFFF_FFFEu32.to_le_bytes()); // no DIFAT sector
        // DIFAT[0] = FAT sector 0; rest FREE
        out[76..80].copy_from_slice(&0u32.to_le_bytes());
        for i in 1..109 {
            out[76 + i * 4..80 + i * 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        }
        // Sector 0 (512..1024): FAT. FAT[0] next-FAT end, FAT[1] dir chain end.
        let fat = 512usize;
        out[fat..fat + 4].copy_from_slice(&0xFFFF_FFFEu32.to_le_bytes());
        out[fat + 4..fat + 8].copy_from_slice(&0xFFFF_FFFEu32.to_le_bytes());
        // Sector 1 (1024..1536): directory entries.
        let dir = 1024usize;
        for (i, name) in streams.iter().enumerate() {
            let e = dir + i * 128;
            let units: Vec<u16> = name.encode_utf16().collect();
            for (k, c) in units.iter().enumerate() {
                out[e + k * 2..e + k * 2 + 2].copy_from_slice(&c.to_le_bytes());
            }
            out[e + 64..e + 66].copy_from_slice(&((units.len() * 2) as u16).to_le_bytes());
        }
        out
    }

    fn classify(streams: &[&str]) -> FindingCode {
        let bytes = cfb_skeleton(streams);
        let mut report = ValidateReport::default();
        classify_cfb(&bytes, &mut report);
        report.findings[0].code
    }

    #[test]
    fn encrypted_is_encrypted_not_corruption() {
        assert_eq!(
            classify(&["Root Entry", "EncryptionInfo", "EncryptedPackage"]),
            FindingCode::EncryptedWorkbook
        );
        assert_eq!(
            classify(&["Root Entry", "Workbook", "Table"]),
            FindingCode::LegacyBiff
        );
        assert_eq!(
            classify(&["Root Entry", "Book", "Globals"]),
            FindingCode::LegacyBiff
        );
        assert_eq!(
            classify(&["Root Entry", "SomethingElse"]),
            FindingCode::UnknownCfb
        );
    }

    #[test]
    fn is_cfb_magic() {
        let bytes = cfb_skeleton(&["Workbook"]);
        assert!(is_cfb(&bytes));
        assert!(!is_cfb(b"PK\x03\x04hello"));
        assert!(!is_cfb(b"short"));
    }
}
