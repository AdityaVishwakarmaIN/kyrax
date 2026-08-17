//! Image media-part packaging: dedup + STORE entries (T1-2a).
//!
//! Images are large and already compressed, so they are added to the ZIP with
//! the STORE method and never routed through deflate. Identical bytes placed on
//! several sheets share one `xl/media/imageN.{ext}` part. Identity is decided by
//! a fixed-seed aHash (64-bit, first-pass filter) CONFIRMED by a full memcmp of
//! the bytes: the hash never decides a match on its own, so two different images
//! that collide on the hash still get distinct media parts and the output can
//! never be mis-deduped.
//!
//! Determinism: the seed is a frozen constant, so the same input workbook
//! produces the same hashes, the same first-seen media ordering, and therefore
//! byte-identical archives across runs (ZIP timestamps are already zeroed by the
//! writer).

use std::hash::{BuildHasher, Hasher};
use std::sync::Arc;

use ahash::RandomState;

use super::model::ImageFormat;

/// FROZEN for release. Any fixed values work; changing these after a release
/// renumbers media parts (names are internal, so output stays valid, but keep
/// them immutable regardless).
const DEDUP_SEEDS: [u64; 4] = [
    0x243F_6A88_85A3_08D3,
    0x1319_8A2E_0370_7344,
    0xA409_3822_299F_31D0,
    0x082E_FA98_EC4E_6C89,
];

struct UniqueImage {
    hash: u64,
    bytes: Arc<[u8]>,
    format: ImageFormat,
}

/// Interns image byte slices to 0-based media part indices (first-seen order).
/// Built once serially before sheet emission; lookups are then read-only and
/// safe to share across the parallel sheet path.
pub struct MediaInterner {
    rs: RandomState,
    unique: Vec<UniqueImage>,
}

impl MediaInterner {
    pub fn new() -> Self {
        let [k0, k1, k2, k3] = DEDUP_SEEDS;
        Self {
            rs: RandomState::with_seeds(k0, k1, k2, k3),
            unique: Vec::new(),
        }
    }

    fn image_hash(&self, bytes: &[u8]) -> u64 {
        let mut h = self.rs.build_hasher();
        h.write(bytes);
        h.finish()
    }

    /// Intern `bytes`; returns the 0-based media part index. Equal bytes share
    /// one part (confirmed by memcmp — the hash is only a first-pass filter).
    pub fn intern(&mut self, bytes: &[u8], format: ImageFormat) -> usize {
        let hash = self.image_hash(bytes);
        self.intern_with_hash(bytes, format, hash)
    }

    /// Intern with an externally supplied hash (tests force a collision here).
    fn intern_with_hash(&mut self, bytes: &[u8], format: ImageFormat, hash: u64) -> usize {
        for (i, u) in self.unique.iter().enumerate() {
            if u.hash == hash && u.bytes.as_ref() == bytes {
                return i;
            }
        }
        self.unique.push(UniqueImage {
            hash,
            bytes: Arc::from(bytes),
            format,
        });
        self.unique.len() - 1
    }

    /// Resolve a previously interned slice back to its media index.
    pub fn lookup(&self, bytes: &[u8]) -> Option<usize> {
        let hash = self.image_hash(bytes);
        self.unique
            .iter()
            .position(|u| u.hash == hash && u.bytes.as_ref() == bytes)
    }

    pub fn len(&self) -> usize {
        self.unique.len()
    }

    /// Kept for symmetry with `len`; callers currently test `len() == 0`.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.unique.is_empty()
    }

    pub fn media_bytes(&self, i: usize) -> &[u8] {
        self.unique[i].bytes.as_ref()
    }

    /// ZIP part name for a media index: `xl/media/imageN.{ext}`.
    pub fn media_part_name(&self, i: usize) -> String {
        format!(
            "xl/media/image{}.{}",
            i + 1,
            self.unique[i].format.extension()
        )
    }

    /// Drawing rel Target from `xl/drawings/` to the media part.
    pub fn media_rel_target(&self, i: usize) -> String {
        format!(
            "../media/image{}.{}",
            i + 1,
            self.unique[i].format.extension()
        )
    }

    /// Default content-type entries for `[Content_Types].xml`, one per used
    /// extension (png / jpeg / gif).
    pub fn media_defaults(&self) -> Vec<(String, &'static str)> {
        let mut seen = Vec::new();
        let mut out = Vec::new();
        for u in &self.unique {
            let ext = u.format.extension();
            if !seen.contains(&ext) {
                seen.push(ext);
                out.push((ext.to_string(), u.format.content_type()));
            }
        }
        out
    }
}

impl Default for MediaInterner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::{assert_eq, assert_ne};
    // Test-only: the non-test code detects format at the call site.
    use super::super::model::detect_image_format;

    pub const PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52,
    ];
    pub const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46];
    pub const GIF: &[u8] = b"GIF89a\x00\x00\x00\x00\x00\x00";

    #[test]
    fn image_format_detected_by_magic_bytes() {
        assert_eq!(detect_image_format(PNG), Some(ImageFormat::Png));
        assert_eq!(detect_image_format(JPEG), Some(ImageFormat::Jpeg));
        assert_eq!(detect_image_format(GIF), Some(ImageFormat::Gif));
        assert_eq!(detect_image_format(b"not an image"), None);
        // PNG signature truncated must not match (starts_with is prefix-safe).
        assert_eq!(detect_image_format(&PNG[..4]), None);
    }

    #[test]
    fn image_dedup_collapses_identical_bytes() {
        let mut m = MediaInterner::new();
        let a = m.intern(PNG, ImageFormat::Png);
        let b = m.intern(PNG, ImageFormat::Png);
        assert_eq!(a, b);
        assert_eq!(m.len(), 1);
        assert_eq!(m.lookup(PNG), Some(a));
        assert_eq!(m.media_part_name(a), "xl/media/image1.png");
        assert_eq!(m.media_rel_target(a), "../media/image1.png");
    }

    #[test]
    fn image_distinct_bytes_get_distinct_parts() {
        let mut m = MediaInterner::new();
        let a = m.intern(PNG, ImageFormat::Png);
        let b = m.intern(JPEG, ImageFormat::Jpeg);
        let c = m.intern(GIF, ImageFormat::Gif);
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(b, c);
        assert_eq!(m.len(), 3);
        assert_eq!(m.media_part_name(b), "xl/media/image2.jpeg");
        assert_eq!(m.media_part_name(c), "xl/media/image3.gif");
    }

    #[test]
    fn image_hash_collision_cannot_mis_dedup() {
        let mut m = MediaInterner::new();
        // Force an identical 64-bit hash for two DIFFERENT byte slices: the
        // memcmp arbiter must keep them as two distinct media parts.
        let a = m.intern_with_hash(b"AAAAAAAA", ImageFormat::Png, 42);
        let b = m.intern_with_hash(b"BBBBBBBB", ImageFormat::Png, 42);
        assert_ne!(a, b, "hash collision must not collapse different images");
        assert_eq!(m.len(), 2);
        // Equal bytes under the same forced hash still dedup via memcmp.
        let a2 = m.intern_with_hash(b"AAAAAAAA", ImageFormat::Png, 42);
        assert_eq!(a2, a);
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn image_deterministic_across_interners() {
        let mut m1 = MediaInterner::new();
        let mut m2 = MediaInterner::new();
        for (bytes, fmt) in [
            (PNG, ImageFormat::Png),
            (JPEG, ImageFormat::Jpeg),
            (GIF, ImageFormat::Gif),
        ] {
            m1.intern(bytes, fmt);
            m2.intern(bytes, fmt);
        }
        for i in 0..m1.len() {
            assert_eq!(m1.media_part_name(i), m2.media_part_name(i));
        }
        assert_eq!(m1.media_defaults(), m2.media_defaults());
    }

    #[test]
    fn image_media_defaults_covers_used_extensions_once() {
        let mut m = MediaInterner::new();
        m.intern(PNG, ImageFormat::Png);
        m.intern(PNG, ImageFormat::Png);
        m.intern(JPEG, ImageFormat::Jpeg);
        let defaults = m.media_defaults();
        assert_eq!(
            defaults,
            vec![
                ("png".to_string(), "image/png"),
                ("jpeg".to_string(), "image/jpeg")
            ]
        );
    }
}
