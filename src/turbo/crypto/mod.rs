//! C1c — encrypted-workbook support wired together.
//!
//! This module owns the third, orchestration layer of encrypted OOXML reading:
//!
//! - [`cfb`] parses the OLE2/Compound File container (owned by the C1b agent).
//! - [`keys`] parses `EncryptionInfo` and derives a verified package key from a
//!   password (owned by the C1a agent).
//! - This file decrypts the `EncryptedPackage` stream segment by segment and
//!   exposes the public entry points ([`is_encrypted`], [`encryption_info`],
//!   [`decrypt_workbook`]).
//!
//! The whole crate is MIT; the crypto primitives are RustCrypto (`aes`, `cbc`,
//! `sha2`) pulled in by the optional `encryption` cargo feature. Key derivation
//! is deliberately expensive (spinCount hashes) and happens exactly once per
//! file; see the "cost of opening an encrypted workbook" test in
//! `tests/encrypted.rs`.
//!
//! Security rule (hard): `decrypt_workbook` returns the plaintext zip bytes in
//! memory and NEVER writes them to disk — writing the decrypted package out
//! would silently defeat the user's encryption. The loader hands the bytes to
//! the existing zip reader unchanged.

pub mod cfb;

#[cfg(feature = "encryption")]
pub mod keys;

#[cfg(feature = "encryption")]
pub use keys::{CryptoError, EncryptionScheme};

use cfb::{Cfb, CfbKind};

/// Every ECB-376 agile segment carries exactly this many plaintext bytes; the
/// ciphertext segments have the same size except possibly the last.
// Kept: the ECMA-376 agile segment size. Documents the format even where
// the constant is spelled inline at the one call site.
#[allow(dead_code)]
const SEGMENT_LENGTH: usize = 4096;

// ----------------------------------------------------------------------------
// Detection (always available — needs only the CFB container reader)
// ----------------------------------------------------------------------------

/// True when `data` is an OLE/CFB container that carries an `EncryptionInfo`
/// stream, i.e. an ECMA-376 encrypted workbook (agile or standard OOXML).
///
/// Plain zips, legacy BIFF, and corrupt/non-CFB input all return `false`; this
/// never panics and never needs a password.
pub fn is_encrypted(data: &[u8]) -> bool {
    match Cfb::parse(data) {
        Some(Ok(cfb)) => cfb.kind() == CfbKind::EncryptedOoxml,
        _ => false,
    }
}

// ----------------------------------------------------------------------------
// Metadata + decryption (require the `encryption` feature)
// ----------------------------------------------------------------------------

/// Human-readable facts about an encrypted workbook, obtainable WITHOUT a
/// password. Algorithm and spin count are the load-bearing fields.
#[cfg(feature = "encryption")]
pub struct EncryptionMetadata {
    /// `"agile"`, `"standard"` or `"unsupported"`.
    pub scheme: &'static str,
    /// e.g. `"AES"`.
    pub cipher_algorithm: String,
    /// e.g. `"SHA512"`.
    pub hash_algorithm: String,
    pub key_bits: u32,
    pub block_size: u32,
    pub salt_size: usize,
    /// Verifier spin count (agile only; 0 for standard).
    pub spin_count: u32,
    /// Why the scheme is unsupported, when it is.
    pub message: Option<String>,
}

/// Parse the `EncryptionInfo` stream and report the scheme without a password.
#[cfg(feature = "encryption")]
pub fn encryption_info(data: &[u8]) -> Result<EncryptionMetadata, CryptoError> {
    let info_stream = encryption_info_stream(data)?;
    let scheme = keys::parse_encryption_info(&info_stream)?;
    Ok(match scheme {
        EncryptionScheme::Agile(p) => EncryptionMetadata {
            scheme: "agile",
            cipher_algorithm: p.cipher_algorithm.clone(),
            hash_algorithm: p.hash_algorithm.clone(),
            key_bits: p.key_bits,
            block_size: p.block_size,
            salt_size: p.salt.len(),
            spin_count: p.spin_count,
            message: None,
        },
        EncryptionScheme::Standard(p) => EncryptionMetadata {
            scheme: "standard",
            cipher_algorithm: p.cipher_algorithm.clone(),
            hash_algorithm: p.hash_algorithm.clone(),
            key_bits: p.key_bits,
            block_size: 0,
            salt_size: p.salt.len(),
            spin_count: p.spin_count,
            message: None,
        },
        EncryptionScheme::Unsupported(msg) => EncryptionMetadata {
            scheme: "unsupported",
            cipher_algorithm: String::new(),
            hash_algorithm: String::new(),
            key_bits: 0,
            block_size: 0,
            salt_size: 0,
            spin_count: 0,
            message: Some(msg),
        },
    })
}

/// Decrypt an encrypted workbook to its plaintext zip bytes, in memory.
///
/// The password is verified by `keys::derive_key` before any package
/// decryption is attempted, so a wrong password surfaces as
/// [`CryptoError::WrongPassword`] — never as a corrupt-zip error downstream.
#[cfg(feature = "encryption")]
pub fn decrypt_workbook(data: &[u8], password: &str) -> Result<Vec<u8>, CryptoError> {
    let cfb = Cfb::parse(data)
        .ok_or_else(|| CryptoError::Malformed("not an OLE/CFB container".into()))?
        .map_err(cfb_to_crypto)?;

    let info_stream = cfb
        .stream("EncryptionInfo")
        .ok_or_else(|| CryptoError::Malformed("missing EncryptionInfo stream".into()))?;
    let package = cfb
        .stream("EncryptedPackage")
        .ok_or_else(|| CryptoError::Malformed("missing EncryptedPackage stream".into()))?;

    let scheme = keys::parse_encryption_info(&info_stream)?;
    let package_key = keys::derive_key(&scheme, password)?;
    match &scheme {
        EncryptionScheme::Agile(_) => {
            let key_data = parse_key_data(&info_stream)?;
            decrypt_agile_package(&package, &package_key, &key_data)
        }
        EncryptionScheme::Standard(_) => Err(CryptoError::Unsupported(
            "Standard (2007) package decryption requires SHA-1, which is not available with the permitted RustCrypto dependency set (aes/cbc/sha2/hmac); agile-encrypted workbooks are fully supported".into(),
        )),
        EncryptionScheme::Unsupported(msg) => Err(CryptoError::Unsupported(msg.clone())),
    }
}

// ----------------------------------------------------------------------------
// Package decryption — the agile segment trap
// ----------------------------------------------------------------------------

/// The `keyData` element of an agile `EncryptionInfo` stream. The payload
/// segment IVs are derived from THIS salt (not the `encryptedKey` salt that key
/// derivation uses), so it is parsed here rather than in `keys.rs`.
#[cfg(feature = "encryption")]
struct KeyData {
    salt: Vec<u8>,
    hash_algorithm: String,
    block_size: u32,
    // Parsed from EncryptionInfo and kept: the key length is part of the
    // format's self-description, and a reader that silently drops it cannot
    // report why a file it refuses is shaped the way it is.
    #[allow(dead_code)]
    key_bits: u32,
}

/// Decrypt the `EncryptedPackage` stream segment by segment.
///
/// The stream starts with an 8-byte little-endian plaintext length prefix,
/// then the ciphertext (final block padded). AGILE ENCRYPTION RE-KEYS PER
/// 4096-BYTE SEGMENT: the IV for segment `i` is `Hash(keyDataSalt || i_le32)`,
/// truncated to the block size. A single-shot decrypt of the whole stream
/// produces correct output for the FIRST SEGMENT ONLY — which looks like "it
/// works on small files" and fails on everything real. We decrypt segment by
/// segment and truncate to the declared length.
#[cfg(feature = "encryption")]
fn decrypt_agile_package(
    package: &[u8],
    key: &[u8],
    key_data: &KeyData,
) -> Result<Vec<u8>, CryptoError> {
    if package.len() < 8 {
        return Err(CryptoError::Malformed(
            "EncryptedPackage stream is too short for its 8-byte length prefix".into(),
        ));
    }
    let mut len_bytes = [0u8; 8];
    len_bytes.copy_from_slice(&package[..8]);
    let declared = u64::from_le_bytes(len_bytes) as usize;

    let ct = &package[8..];
    if declared > ct.len() {
        return Err(CryptoError::Malformed(format!(
            "EncryptedPackage declares {declared} plaintext bytes but only {} are present",
            ct.len()
        )));
    }
    let block = key_data.block_size as usize;
    if block == 0 || ct.len() % block != 0 {
        return Err(CryptoError::Malformed(format!(
            "EncryptedPackage ciphertext ({} bytes) is not aligned to the {block}-byte block size",
            ct.len()
        )));
    }
    if declared == 0 {
        return Ok(Vec::new());
    }

    let mut out = Vec::with_capacity(declared);
    let mut pos = 0usize;
    let mut segment = 0u32;
    while pos < ct.len() {
        let take = (ct.len() - pos).min(SEGMENT_LENGTH);
        let iv = segment_iv(key_data, segment)?;
        let dec = aes_cbc_decrypt(key, &iv, &ct[pos..pos + take])?;
        out.extend_from_slice(&dec);
        pos += take;
        segment += 1;
    }
    out.truncate(declared);
    Ok(out)
}

/// `Hash(keyDataSalt || segment_index_le32)`, truncated to the block size.
#[cfg(feature = "encryption")]
fn segment_iv(key_data: &KeyData, segment: u32) -> Result<Vec<u8>, CryptoError> {
    let mut input = Vec::with_capacity(key_data.salt.len() + 4);
    input.extend_from_slice(&key_data.salt);
    input.extend_from_slice(&segment.to_le_bytes());
    let d = digest(&key_data.hash_algorithm, &input)?;
    let block = key_data.block_size as usize;
    if d.len() < block {
        return Err(CryptoError::Malformed(format!(
            "keyData hash {} produces {} bytes, fewer than the {block}-byte block size",
            key_data.hash_algorithm,
            d.len()
        )));
    }
    Ok(d[..block].to_vec())
}

// ----------------------------------------------------------------------------
// keyData parsing (focused; mirrors the helpers in keys.rs without touching it)
// ----------------------------------------------------------------------------

#[cfg(feature = "encryption")]
fn parse_key_data(info_stream: &[u8]) -> Result<KeyData, CryptoError> {
    if info_stream.len() < 8 {
        return Err(CryptoError::Malformed(format!(
            "EncryptionInfo stream is {} bytes, too short for the 8-byte header",
            info_stream.len()
        )));
    }
    let text = decode_agile_xml(&info_stream[8..])?;
    let region = find_element_attrs(&text, "keyData")
        .ok_or_else(|| CryptoError::Malformed("missing <keyData> element".into()))?;
    let attr = |name: &str| {
        get_attr(region, name).ok_or_else(|| {
            CryptoError::Malformed(format!("missing {name:?} attribute on <keyData>"))
        })
    };
    let salt = base64_decode(attr("saltValue")?.as_bytes())?;
    let hash_algorithm = attr("hashAlgorithm")?;
    let block_size = parse_u32(&attr("blockSize")?)?;
    let key_bits = parse_u32(&attr("keyBits")?)?;
    let cipher_algorithm = attr("cipherAlgorithm")?;
    if cipher_algorithm != "AES" {
        return Err(CryptoError::Unsupported(format!(
            "payload cipher algorithm {cipher_algorithm:?} is not supported"
        )));
    }
    if block_size != 16 {
        return Err(CryptoError::Unsupported(format!(
            "payload block size {block_size} is not supported (only AES/16-byte blocks)"
        )));
    }
    if salt.len() < block_size as usize {
        return Err(CryptoError::Malformed(format!(
            "keyData salt is {} bytes, must be at least the block size ({block_size})",
            salt.len()
        )));
    }
    Ok(KeyData {
        salt,
        hash_algorithm,
        block_size,
        key_bits,
    })
}

// ----------------------------------------------------------------------------
// Primitives (RustCrypto — never reimplemented; only wired)
// ----------------------------------------------------------------------------

#[cfg(feature = "encryption")]
fn digest(alg: &str, data: &[u8]) -> Result<Vec<u8>, CryptoError> {
    use sha2::{Digest, Sha256, Sha384, Sha512};
    match alg {
        "SHA512" => Ok(Sha512::digest(data).to_vec()),
        "SHA384" => Ok(Sha384::digest(data).to_vec()),
        "SHA256" => Ok(Sha256::digest(data).to_vec()),
        other => Err(CryptoError::Unsupported(format!(
            "payload hash algorithm {other:?} is not supported (only the SHA-2 family is available under the `encryption` feature)"
        ))),
    }
}

#[cfg(feature = "encryption")]
fn aes_cbc_decrypt(key: &[u8], iv: &[u8], ct: &[u8]) -> Result<Vec<u8>, CryptoError> {
    use aes::{
        Aes128, Aes192, Aes256,
        cipher::{
            BlockCipher, BlockDecrypt, BlockDecryptMut, KeyInit, KeyIvInit,
            block_padding::NoPadding,
        },
    };
    use cbc::Decryptor;

    fn impl_<C>(key: &[u8], iv: &[u8], ct: &[u8]) -> Result<Vec<u8>, CryptoError>
    where
        C: BlockCipher + BlockDecrypt + KeyInit,
    {
        if iv.len() != 16 {
            return Err(CryptoError::Malformed(format!(
                "CBC IV must be 16 bytes, got {}",
                iv.len()
            )));
        }
        if ct.len() % 16 != 0 {
            return Err(CryptoError::Malformed(format!(
                "ciphertext length {} is not a multiple of the AES block size",
                ct.len()
            )));
        }
        let mut buf = ct.to_vec();
        let dec = Decryptor::<C>::new_from_slices(key, iv)
            .map_err(|_| CryptoError::Malformed("invalid AES key length".into()))?;
        let pt = dec
            .decrypt_padded_mut::<NoPadding>(&mut buf)
            .map_err(|_| CryptoError::Malformed("ciphertext is not block aligned".into()))?;
        Ok(pt.to_vec())
    }

    match key.len() {
        16 => impl_::<Aes128>(key, iv, ct),
        24 => impl_::<Aes192>(key, iv, ct),
        32 => impl_::<Aes256>(key, iv, ct),
        n => Err(CryptoError::Malformed(format!(
            "invalid AES key length {n} (must be 16, 24 or 32 bytes)"
        ))),
    }
}

// ----------------------------------------------------------------------------
// XML + base64 helpers (focused; mirrors keys.rs)
// ----------------------------------------------------------------------------

/// Locate an open tag's attribute region (`<name ... >`).
#[cfg(feature = "encryption")]
fn find_element_attrs<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let b = xml.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'<' {
            let mut j = i + 1;
            let start = j;
            while j < b.len() && b[j] != b'>' && !b[j].is_ascii_whitespace() {
                j += 1;
            }
            let tag = &xml[start..j];
            if tag == name || tag.ends_with(&format!(":{name}")) {
                let mut k = j;
                while k < b.len() && b[k] != b'>' {
                    k += 1;
                }
                if k < b.len() {
                    return Some(&xml[j..k]);
                }
            }
        }
        i += 1;
    }
    None
}

/// Find `name="value"` in an attribute region and return the value.
#[cfg(feature = "encryption")]
fn get_attr(region: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let needle_b = needle.as_bytes();
    let b = region.as_bytes();
    let mut pos = 0;
    while pos + needle_b.len() <= b.len() {
        if &b[pos..pos + needle_b.len()] == needle_b {
            let start = pos + needle_b.len();
            let mut end = start;
            while end < b.len() && b[end] != b'"' {
                end += 1;
            }
            if end < b.len() {
                return Some(region[start..end].to_string());
            }
            return None;
        }
        pos += 1;
    }
    None
}

#[cfg(feature = "encryption")]
fn parse_u32(s: &str) -> Result<u32, CryptoError> {
    s.trim()
        .parse()
        .map_err(|_| CryptoError::Malformed(format!("invalid u32 value {s:?}")))
}

/// Agile XML is UTF-8 in practice; tolerate a UTF-16 BOM or BOM-less UTF-16LE.
#[cfg(feature = "encryption")]
fn decode_agile_xml(bytes: &[u8]) -> Result<String, CryptoError> {
    if bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] == 0xfe {
        return utf16le_to_string(&bytes[2..]);
    }
    if bytes.len() >= 2 && bytes[0] == 0xfe && bytes[1] == 0xff {
        return Err(CryptoError::Unsupported("UTF-16BE encryption XML".into()));
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => Ok(s.to_string()),
        Err(_) => {
            if bytes.len() % 2 == 0 && bytes.get(1) == Some(&0) {
                utf16le_to_string(bytes)
            } else {
                Err(CryptoError::Malformed(
                    "encryption XML is neither UTF-8 nor UTF-16LE".into(),
                ))
            }
        }
    }
}

#[cfg(feature = "encryption")]
fn utf16le_to_string(bytes: &[u8]) -> Result<String, CryptoError> {
    if bytes.len() % 2 != 0 {
        return Err(CryptoError::Malformed("odd-length UTF-16 data".into()));
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16(&units).map_err(|_| CryptoError::Malformed("invalid UTF-16 data".into()))
}

/// Minimal base64 decoder (standard alphabet, tolerates whitespace and `=`
/// padding). Not a cryptographic primitive; used to read XML attributes.
#[cfg(feature = "encryption")]
fn base64_decode(input: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for &b in input {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b'\r' | b'\n' | b' ' | b'\t' => continue,
            _ => {
                return Err(CryptoError::Malformed(format!(
                    "invalid base64 character {b:#04x}"
                )));
            }
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    Ok(out)
}

/// Map the container-layer error onto the key-derivation error type.
#[cfg(feature = "encryption")]
fn cfb_to_crypto(e: cfb::CfbError) -> CryptoError {
    use cfb::CfbError;
    let msg = match e {
        CfbError::Truncated => "truncated OLE/CFB container".into(),
        CfbError::BadHeader => "malformed OLE/CFB header".into(),
        CfbError::BadFat => "corrupt OLE/CFB FAT".into(),
        CfbError::CycleDetected => "circular OLE/CFB chain".into(),
        CfbError::StreamTooLarge => "OLE/CFB stream larger than the container".into(),
    };
    CryptoError::Malformed(msg)
}

/// Helper for the loader: the `EncryptionInfo` stream or a precise error.
#[cfg(feature = "encryption")]
fn encryption_info_stream(data: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let cfb = Cfb::parse(data)
        .ok_or_else(|| CryptoError::Malformed("not an OLE/CFB container".into()))?
        .map_err(cfb_to_crypto)?;
    cfb.stream("EncryptionInfo")
        .ok_or_else(|| CryptoError::Malformed("missing EncryptionInfo stream".into()))
}
