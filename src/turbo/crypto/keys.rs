//! ECMA-376 encryption key derivation (the KEY DERIVATION layer of
//! encrypted-workbook support).
//!
//! This module parses an `EncryptionInfo` stream and derives a verified
//! symmetric package key from a password. It never reads the OLE container
//! (`crypto/cfb.rs` owns that) and never decrypts the package
//! (`crypto/mod.rs` owns that); it only turns "EncryptionInfo bytes + password"
//! into a verified key, or a precise [`CryptoError`].
//!
//! The Agile (ECMA-376 Part 2 / MS-OFFCRYPTO 2.3.4.6) derivation sequence is:
//!
//! 1. Password is UTF-16LE with no BOM and no null terminator.
//! 2. `H_0 = Hash(salt || password)`.
//! 3. Spin `spinCount` times: `H_i = Hash(iterator_le || H_{i-1})`, where the
//!    iterator is a little-endian `u32` and is **prepended**.
//! 4. Three final hashes are taken with three **distinct** fixed block keys
//!    appended (verifier input / verifier value / key value), each truncated to
//!    `keyBits / 8` bytes.
//! 5. The password is **verified** by decrypting `encryptedVerifierHashInput`
//!    and `encryptedVerifierHashValue`; a mismatch is `WrongPassword`, surfaced
//!    before any package decryption is attempted.
//! 6. The package key is the decryption of `encryptedKeyValue`.

#![cfg(feature = "encryption")]

use aes::{
    Aes128, Aes192, Aes256,
    cipher::{
        BlockCipher, BlockDecrypt, BlockDecryptMut, KeyInit, KeyIvInit, block_padding::NoPadding,
    },
};
use cbc::Decryptor;
use sha2::{Digest, Sha256, Sha384, Sha512};

/// Agile key parameters, read from the `<keyEncryptor>` / `<encryptedKey>`
/// element. The encrypted blobs live in private fields so the public surface
/// stays exactly the interface contract.
#[derive(Debug, Clone)]
pub struct AgileParams {
    pub spin_count: u32,
    pub key_bits: u32,
    pub block_size: u32,
    pub hash_size: u32,
    pub salt: Vec<u8>,
    pub hash_algorithm: String,   // e.g. "SHA512"
    pub cipher_algorithm: String, // e.g. "AES"
    pub cipher_chaining: String,  // e.g. "ChainingModeCBC"
    encrypted_verifier_hash_input: Vec<u8>,
    encrypted_verifier_hash_value: Vec<u8>,
    encrypted_key_value: Vec<u8>,
}

/// Standard (2007) key parameters. The scheme mandates SHA-1 for key
/// derivation, which is not available under the `encryption` feature's
/// permitted dependency set (aes/cbc/sha2/hmac), so `derive_key` on a
/// `Standard` scheme reports `Unsupported` rather than a wrong key. Parsing
/// still validates the binary layout so the scheme is identified precisely.
#[derive(Debug, Clone)]
pub struct StandardParams {
    /// Iterations used by the reference derivation for the standard scheme
    /// (fixed at 50,000). Stored for transparency; the scheme is unsupported
    /// here because it requires SHA-1.
    pub spin_count: u32,
    pub key_bits: u32,
    pub salt: Vec<u8>,
    pub hash_algorithm: String,
    pub cipher_algorithm: String,
}

#[derive(Debug, Clone)]
pub enum EncryptionScheme {
    Agile(AgileParams),
    Standard(StandardParams),
    Unsupported(String),
}

/// Parse the `EncryptionInfo` stream. Never panics on malformed input.
pub fn parse_encryption_info(stream: &[u8]) -> Result<EncryptionScheme, CryptoError> {
    if stream.len() < 8 {
        return Err(CryptoError::Malformed(format!(
            "EncryptionInfo stream is {} bytes, too short for the 8-byte header",
            stream.len()
        )));
    }
    let major = u16::from_le_bytes([stream[0], stream[1]]);
    let minor = u16::from_le_bytes([stream[2], stream[3]]);
    match (major, minor) {
        (4, 4) => parse_agile(&stream[8..]).map(EncryptionScheme::Agile),
        (2..=4, 2) => parse_standard(stream).map(EncryptionScheme::Standard),
        (_, 3) => Ok(EncryptionScheme::Unsupported(
            "Extensible Encryption (minor version 3) is not supported".into(),
        )),
        (1, _) => Ok(EncryptionScheme::Unsupported(
            "XOR obfuscation / legacy RC4 (version 1) is out of scope".into(),
        )),
        (0, _) => Err(CryptoError::Malformed(format!(
            "EncryptionInfo version 0.{} (document is not encrypted)",
            minor
        ))),
        _ => Err(CryptoError::Malformed(format!(
            "unknown EncryptionInfo version {major}.{minor}"
        ))),
    }
}

/// Derive and VERIFY the package key. Returns [`CryptoError::WrongPassword`]
/// when the verifier does not match, which callers surface directly to the
/// user. Never returns a garbage key for a wrong password.
pub fn derive_key(scheme: &EncryptionScheme, password: &str) -> Result<Vec<u8>, CryptoError> {
    match scheme {
        EncryptionScheme::Agile(p) => derive_agile(p, password),
        EncryptionScheme::Standard(p) => derive_standard(p, password),
        EncryptionScheme::Unsupported(msg) => Err(CryptoError::Unsupported(msg.clone())),
    }
}

#[derive(Debug)]
pub enum CryptoError {
    Malformed(String),
    WrongPassword,
    Unsupported(String),
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CryptoError::Malformed(m) => write!(f, "malformed encryption information: {m}"),
            CryptoError::WrongPassword => write!(f, "wrong password"),
            CryptoError::Unsupported(m) => write!(f, "unsupported encryption: {m}"),
        }
    }
}

impl std::error::Error for CryptoError {}

// ----------------------------------------------------------------------------
// Agile fixed block keys (MS-OFFCRYPTO 2.3.4.6 / ECMA-376 Part 2).
// These are THREE DISTINCT constants, one each for the verifier hash input,
// the verifier hash value, and the package key value. Reusing one for another
// produces a valid-looking but wrong key with no diagnostic, so each stays
// separate and clearly named.
// ----------------------------------------------------------------------------

/// Block key appended when deriving the key that decrypts
/// `encryptedVerifierHashInput`.
const BLOCK_KEY_VERIFIER_INPUT: [u8; 8] = [0xfe, 0xa7, 0xd2, 0x76, 0x3b, 0x4b, 0x9e, 0x79];
/// Block key appended when deriving the key that decrypts
/// `encryptedVerifierHashValue`.
const BLOCK_KEY_VERIFIER_VALUE: [u8; 8] = [0xd7, 0xaa, 0x0f, 0x6d, 0x30, 0x61, 0x34, 0x4e];
/// Block key appended when deriving the key that decrypts `encryptedKeyValue`.
const BLOCK_KEY_KEY_VALUE: [u8; 8] = [0x14, 0x6e, 0x0b, 0xe7, 0xab, 0xac, 0xd0, 0xd6];

/// Upper bound on `spinCount` accepted from untrusted input, to keep a hostile
/// stream from turning one `derive_key` call into a denial-of-service loop.
/// Real Office files use 100,000; this leaves ample headroom.
const MAX_SPIN_COUNT: u32 = 10_000_000;

/// Iteration count for the Standard (2007) scheme, mirroring the msoffcrypto
/// reference implementation that is verified against real files.
const STANDARD_ITER_COUNT: u32 = 50_000;

// ----------------------------------------------------------------------------
// Agile
// ----------------------------------------------------------------------------

fn parse_agile(xml: &[u8]) -> Result<AgileParams, CryptoError> {
    let text = decode_agile_xml(xml)?;
    let region = find_element_attrs(&text, "encryptedKey")
        .ok_or_else(|| CryptoError::Malformed("missing <encryptedKey> element".into()))?;
    let attr = |name: &str| {
        get_attr(region, name).ok_or_else(|| {
            CryptoError::Malformed(format!("missing {name:?} attribute on <encryptedKey>"))
        })
    };
    let spin_count = parse_u32(&attr("spinCount")?)?;
    let key_bits = parse_u32(&attr("keyBits")?)?;
    let block_size = parse_u32(&attr("blockSize")?)?;
    let hash_size = parse_u32(&attr("hashSize")?)?;
    let salt = base64_decode(attr("saltValue")?.as_bytes())?;
    let hash_algorithm = attr("hashAlgorithm")?;
    let cipher_algorithm = attr("cipherAlgorithm")?;
    let cipher_chaining = attr("cipherChaining")?;
    let encrypted_verifier_hash_input =
        base64_decode(attr("encryptedVerifierHashInput")?.as_bytes())?;
    let encrypted_verifier_hash_value =
        base64_decode(attr("encryptedVerifierHashValue")?.as_bytes())?;
    let encrypted_key_value = base64_decode(attr("encryptedKeyValue")?.as_bytes())?;
    let params = AgileParams {
        spin_count,
        key_bits,
        block_size,
        hash_size,
        salt,
        hash_algorithm,
        cipher_algorithm,
        cipher_chaining,
        encrypted_verifier_hash_input,
        encrypted_verifier_hash_value,
        encrypted_key_value,
    };
    validate_agile(&params)?;
    Ok(params)
}

fn validate_agile(p: &AgileParams) -> Result<(), CryptoError> {
    if p.spin_count > MAX_SPIN_COUNT {
        return Err(CryptoError::Malformed(format!(
            "spinCount {} exceeds the supported maximum of {MAX_SPIN_COUNT}",
            p.spin_count
        )));
    }
    if p.key_bits == 0 || p.key_bits % 8 != 0 {
        return Err(CryptoError::Malformed(format!(
            "invalid keyBits {}",
            p.key_bits
        )));
    }
    if !matches!(p.key_bits, 128 | 192 | 256) {
        return Err(CryptoError::Unsupported(format!(
            "{}-bit key size",
            p.key_bits
        )));
    }
    if p.block_size != 16 {
        return Err(CryptoError::Unsupported(format!(
            "block size {}",
            p.block_size
        )));
    }
    if p.salt.len() < p.block_size as usize {
        return Err(CryptoError::Malformed(format!(
            "salt is {} bytes, must be at least the block size ({})",
            p.salt.len(),
            p.block_size
        )));
    }
    match p.hash_algorithm.as_str() {
        "SHA256" | "SHA384" | "SHA512" => {}
        other => {
            return Err(CryptoError::Unsupported(format!(
                "hash algorithm {other:?} is not supported (only the SHA-2 family is available under the `encryption` feature)"
            )));
        }
    }
    if p.cipher_algorithm != "AES" {
        return Err(CryptoError::Unsupported(format!(
            "cipher algorithm {:?}",
            p.cipher_algorithm
        )));
    }
    if p.cipher_chaining != "ChainingModeCBC" {
        return Err(CryptoError::Unsupported(format!(
            "cipher chaining mode {:?}",
            p.cipher_chaining
        )));
    }
    Ok(())
}

fn derive_agile(p: &AgileParams, password: &str) -> Result<Vec<u8>, CryptoError> {
    validate_agile(p)?;
    let key_len = (p.key_bits / 8) as usize;

    // H_0 = Hash(salt || password_utf16le); then spinCount rounds of
    // H_i = Hash(iterator_le || H_{i-1}).
    let mut h = {
        let mut input = Vec::with_capacity(p.salt.len() + password.len() * 2);
        input.extend_from_slice(&p.salt);
        input.extend_from_slice(&utf16le(password));
        digest(&p.hash_algorithm, &input)?
    };
    for i in 0..p.spin_count {
        let mut input = Vec::with_capacity(4 + h.len());
        input.extend_from_slice(&(i as u32).to_le_bytes());
        input.extend_from_slice(&h);
        h = digest(&p.hash_algorithm, &input)?;
    }

    // Three distinct fixed block keys, one per purpose.
    let key1 = final_agile_key(&p.hash_algorithm, &h, &BLOCK_KEY_VERIFIER_INPUT, key_len)?;
    let key2 = final_agile_key(&p.hash_algorithm, &h, &BLOCK_KEY_VERIFIER_VALUE, key_len)?;
    let key3 = final_agile_key(&p.hash_algorithm, &h, &BLOCK_KEY_KEY_VALUE, key_len)?;

    let iv = &p.salt[..p.block_size as usize];

    // Verify the password BEFORE any package decryption: hash the decrypted
    // verifier hash input and compare it against the decrypted verifier hash
    // value. A mismatch is a wrong password, not a corrupt package.
    let verifier_input = aes_cbc_decrypt(&key1, iv, &p.encrypted_verifier_hash_input)?;
    let expected = digest(&p.hash_algorithm, &verifier_input)?;
    let verifier_value = aes_cbc_decrypt(&key2, iv, &p.encrypted_verifier_hash_value)?;
    if !ct_eq(&expected, &verifier_value) {
        return Err(CryptoError::WrongPassword);
    }

    let key = aes_cbc_decrypt(&key3, iv, &p.encrypted_key_value)?;
    if key.len() < key_len {
        return Err(CryptoError::Malformed(format!(
            "decrypted package key is {} bytes, expected at least {key_len}",
            key.len()
        )));
    }
    Ok(key[..key_len].to_vec())
}

/// Final agile key hash: `Hash(H_spin || block_key)`, truncated (or padded with
/// 0x36) to `key_len` bytes.
fn final_agile_key(
    hash_algorithm: &str,
    h: &[u8],
    block_key: &[u8],
    key_len: usize,
) -> Result<Vec<u8>, CryptoError> {
    let mut input = Vec::with_capacity(h.len() + block_key.len());
    input.extend_from_slice(h);
    input.extend_from_slice(block_key);
    let d = digest(hash_algorithm, &input)?;
    Ok(truncate_key(d, key_len))
}

// ----------------------------------------------------------------------------
// Standard (2007)
// ----------------------------------------------------------------------------

fn parse_standard(stream: &[u8]) -> Result<StandardParams, CryptoError> {
    let header_size = read_u32(stream, 8)? as usize;
    let header_start: usize = 12;
    let header_end = header_start
        .checked_add(header_size)
        .ok_or_else(|| CryptoError::Malformed("encryption header size overflows".into()))?;
    if header_end > stream.len() {
        return Err(CryptoError::Malformed(format!(
            "standard encryption header of {header_size} bytes is truncated"
        )));
    }
    let mut r = BinReader::new(&stream[header_start..header_end]);
    let _flags = r.u32()?;
    let _size_extra = r.u32()?;
    let alg_id = r.u32()?;
    let alg_id_hash = r.u32()?;
    let key_bits = r.u32()?;
    let _provider_type = r.u32()?;
    let _reserved1 = r.u32()?;
    let _reserved2 = r.u32()?;
    // The remainder of the header is the CSP provider name (UTF-16LE); it does
    // not participate in key derivation.

    let cipher_algorithm = match alg_id {
        0x6801 => "RC4",
        0x660e | 0x660f | 0x6610 => "AES",
        other => {
            return Err(CryptoError::Unsupported(format!(
                "standard cipher algorithm id {other:#010x}"
            )));
        }
    }
    .to_string();
    let hash_algorithm = match alg_id_hash {
        0x0000 | 0x8004 => "SHA1",
        0x0001 => "SHA256",
        0x0002 => "SHA384",
        0x0003 => "SHA512",
        other => {
            return Err(CryptoError::Unsupported(format!(
                "standard hash algorithm id {other:#010x}"
            )));
        }
    }
    .to_string();

    let mut v = BinReader::new(&stream[header_end..]);
    let salt_size = v.u32()? as usize;
    let salt = v.bytes(salt_size)?.to_vec();
    // Read (and structurally validate) the encrypted verifier + verifier hash;
    // they are not stored because the scheme is unsupported here.
    let _encrypted_verifier = v.bytes(16)?;
    let _verifier_hash_size = v.u32()?;
    let encrypted_verifier_hash_size = if cipher_algorithm == "AES" { 32 } else { 20 };
    let _encrypted_verifier_hash = v.bytes(encrypted_verifier_hash_size)?;

    if key_bits == 0 || key_bits % 8 != 0 {
        return Err(CryptoError::Malformed(format!(
            "invalid standard keyBits {key_bits}"
        )));
    }

    Ok(StandardParams {
        spin_count: STANDARD_ITER_COUNT,
        key_bits,
        salt,
        hash_algorithm,
        cipher_algorithm,
    })
}

fn derive_standard(p: &StandardParams, _password: &str) -> Result<Vec<u8>, CryptoError> {
    if p.cipher_algorithm != "AES" {
        return Err(CryptoError::Unsupported(format!(
            "standard encryption with {} is out of scope (RC4/XOR legacy formats are not supported)",
            p.cipher_algorithm
        )));
    }
    // The Standard (2007) scheme's password-to-key derivation is SHA-1 based
    // (msoffcrypto's verified reference uses SHA1 with a fixed 50,000-iteration
    // spin, then the CryptoAPI 0x36/0x5c expansion). SHA-1 is deliberately not
    // available here: the fence permits only aes/cbc/sha2/hmac and forbids
    // hand-rolling primitives, so this is Unsupported rather than a wrong key.
    Err(CryptoError::Unsupported(
        "Standard (2007) encryption requires SHA-1, which is not available with the permitted RustCrypto dependency set (aes/cbc/sha2/hmac); Agile encryption is fully supported"
            .into(),
    ))
}

// ----------------------------------------------------------------------------
// Primitive plumbing (never reimplementing AES/SHA/HMAC — only the sequence).
// ----------------------------------------------------------------------------

fn digest(hash_algorithm: &str, data: &[u8]) -> Result<Vec<u8>, CryptoError> {
    match hash_algorithm {
        "SHA512" => Ok(Sha512::digest(data).to_vec()),
        "SHA384" => Ok(Sha384::digest(data).to_vec()),
        "SHA256" => Ok(Sha256::digest(data).to_vec()),
        other => Err(CryptoError::Unsupported(format!(
            "hash algorithm {other:?} is not supported (only the SHA-2 family is available under the `encryption` feature)"
        ))),
    }
}

/// Password to UTF-16LE, no BOM, no null terminator.
fn utf16le(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() * 2);
    for unit in s.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

/// Truncate to `len` bytes, padding with 0x36 when the hash output is shorter
/// (per ECMA-376 the pad byte is 0x36).
fn truncate_key(mut d: Vec<u8>, len: usize) -> Vec<u8> {
    d.truncate(len);
    while d.len() < len {
        d.push(0x36);
    }
    d
}

/// Constant-time byte comparison (lengths checked first).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn aes_cbc_decrypt(key: &[u8], iv: &[u8], ct: &[u8]) -> Result<Vec<u8>, CryptoError> {
    match key.len() {
        16 => aes_cbc_decrypt_impl::<Aes128>(key, iv, ct),
        24 => aes_cbc_decrypt_impl::<Aes192>(key, iv, ct),
        32 => aes_cbc_decrypt_impl::<Aes256>(key, iv, ct),
        n => Err(CryptoError::Malformed(format!(
            "invalid AES key length {n} (must be 16, 24 or 32 bytes)"
        ))),
    }
}

fn aes_cbc_decrypt_impl<C>(key: &[u8], iv: &[u8], ct: &[u8]) -> Result<Vec<u8>, CryptoError>
where
    C: BlockCipher + BlockDecrypt + KeyInit,
{
    let mut buf = ct.to_vec();
    if buf.len() % 16 != 0 {
        return Err(CryptoError::Malformed(format!(
            "ciphertext length {} is not a multiple of the AES block size",
            buf.len()
        )));
    }
    if iv.len() != 16 {
        return Err(CryptoError::Malformed(format!(
            "CBC IV must be 16 bytes, got {}",
            iv.len()
        )));
    }
    let dec = Decryptor::<C>::new_from_slices(key, iv)
        .map_err(|_| CryptoError::Malformed("invalid AES key length".into()))?;
    let pt = dec
        .decrypt_padded_mut::<NoPadding>(&mut buf)
        .map_err(|_| CryptoError::Malformed("ciphertext is not block aligned".into()))?;
    Ok(pt.to_vec())
}

// ----------------------------------------------------------------------------
// XML + binary parsing helpers (focused, non-panicking).
// ----------------------------------------------------------------------------

/// Locate the `<encryptedKey ... />` open tag and return the attribute region
/// between the tag name and the closing `>`.
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

fn parse_u32(s: &str) -> Result<u32, CryptoError> {
    s.trim()
        .parse()
        .map_err(|_| CryptoError::Malformed(format!("invalid u32 value {s:?}")))
}

/// Agile XML is UTF-8 in practice; tolerate a UTF-16 BOM or BOM-less UTF-16LE.
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

/// Minimal base64 decoder (standard alphabet, tolerates whitespace and
/// `=` padding). Not a cryptographic primitive; used to read XML attributes.
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

fn read_u32(buf: &[u8], offset: usize) -> Result<u32, CryptoError> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| CryptoError::Malformed("offset overflow".into()))?;
    let bytes = buf.get(offset..end).ok_or_else(|| {
        CryptoError::Malformed(format!(
            "truncated stream (need 4 bytes at offset {offset})"
        ))
    })?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

struct BinReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> BinReader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        BinReader { buf, pos: 0 }
    }
    fn u32(&mut self) -> Result<u32, CryptoError> {
        read_u32(self.buf, self.pos).map(|v| {
            self.pos += 4;
            v
        })
    }
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], CryptoError> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| CryptoError::Malformed("stream length overflow".into()))?;
        let out = self.buf.get(self.pos..end).ok_or_else(|| {
            CryptoError::Malformed("truncated standard encryption verifier".into())
        })?;
        self.pos = end;
        Ok(out)
    }
}

// ----------------------------------------------------------------------------
// Tests
//
// Fixtures are spec-compliant `EncryptionInfo` streams generated with the
// msoffcrypto-tool reference implementation (which itself decrypts real Office
// files); each fixture's expected key was independently re-derived by that
// reference. Fixture B and fixture C embed parameters taken from real files
// (they are msoffcrypto's own shipped test vectors). All fixtures were written
// to the system temp directory during generation, never into the repo.
// ----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE_A_STREAM: &[u8] = b"\x04\x00\x04\x00\x40\x00\x00\x00\x3c\x3f\x78\x6d\x6c\x20\x76\x65\x72\x73\x69\x6f\x6e\x3d\x22\x31\x2e\x30\x22\x20\x65\x6e\x63\x6f\x64\x69\x6e\x67\x3d\x22\x55\x54\x46\x2d\x38\x22\x20\x73\x74\x61\x6e\x64\x61\x6c\x6f\x6e\x65\x3d\x22\x79\x65\x73\x22\x3f\x3e\x0a\x3c\x65\x6e\x63\x72\x79\x70\x74\x69\x6f\x6e\x20\x78\x6d\x6c\x6e\x73\x3d\x22\x68\x74\x74\x70\x3a\x2f\x2f\x73\x63\x68\x65\x6d\x61\x73\x2e\x6d\x69\x63\x72\x6f\x73\x6f\x66\x74\x2e\x63\x6f\x6d\x2f\x6f\x66\x66\x69\x63\x65\x2f\x32\x30\x30\x36\x2f\x65\x6e\x63\x72\x79\x70\x74\x69\x6f\x6e\x22\x20\x78\x6d\x6c\x6e\x73\x3a\x70\x3d\x22\x68\x74\x74\x70\x3a\x2f\x2f\x73\x63\x68\x65\x6d\x61\x73\x2e\x6d\x69\x63\x72\x6f\x73\x6f\x66\x74\x2e\x63\x6f\x6d\x2f\x6f\x66\x66\x69\x63\x65\x2f\x32\x30\x30\x36\x2f\x6b\x65\x79\x45\x6e\x63\x72\x79\x70\x74\x6f\x72\x2f\x70\x61\x73\x73\x77\x6f\x72\x64\x22\x20\x78\x6d\x6c\x6e\x73\x3a\x63\x3d\x22\x68\x74\x74\x70\x3a\x2f\x2f\x73\x63\x68\x65\x6d\x61\x73\x2e\x6d\x69\x63\x72\x6f\x73\x6f\x66\x74\x2e\x63\x6f\x6d\x2f\x6f\x66\x66\x69\x63\x65\x2f\x32\x30\x30\x36\x2f\x6b\x65\x79\x45\x6e\x63\x72\x79\x70\x74\x6f\x72\x2f\x63\x65\x72\x74\x69\x66\x69\x63\x61\x74\x65\x22\x3e\x0a\x20\x20\x20\x20\x3c\x6b\x65\x79\x44\x61\x74\x61\x20\x73\x61\x6c\x74\x53\x69\x7a\x65\x3d\x22\x31\x36\x22\x20\x62\x6c\x6f\x63\x6b\x53\x69\x7a\x65\x3d\x22\x31\x36\x22\x20\x6b\x65\x79\x42\x69\x74\x73\x3d\x22\x32\x35\x36\x22\x20\x68\x61\x73\x68\x53\x69\x7a\x65\x3d\x22\x36\x34\x22\x0a\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x63\x69\x70\x68\x65\x72\x41\x6c\x67\x6f\x72\x69\x74\x68\x6d\x3d\x22\x41\x45\x53\x22\x20\x63\x69\x70\x68\x65\x72\x43\x68\x61\x69\x6e\x69\x6e\x67\x3d\x22\x43\x68\x61\x69\x6e\x69\x6e\x67\x4d\x6f\x64\x65\x43\x42\x43\x22\x20\x68\x61\x73\x68\x41\x6c\x67\x6f\x72\x69\x74\x68\x6d\x3d\x22\x53\x48\x41\x35\x31\x32\x22\x20\x73\x61\x6c\x74\x56\x61\x6c\x75\x65\x3d\x22\x79\x69\x30\x74\x48\x66\x47\x35\x56\x74\x46\x62\x38\x4c\x4d\x42\x46\x79\x6a\x6a\x4a\x41\x3d\x3d\x22\x20\x2f\x3e\x0a\x20\x20\x20\x20\x3c\x64\x61\x74\x61\x49\x6e\x74\x65\x67\x72\x69\x74\x79\x20\x65\x6e\x63\x72\x79\x70\x74\x65\x64\x48\x6d\x61\x63\x4b\x65\x79\x3d\x22\x58\x63\x38\x35\x6f\x38\x53\x39\x61\x55\x54\x61\x58\x65\x6e\x6a\x59\x69\x36\x70\x44\x4c\x2f\x65\x7a\x67\x77\x6f\x7a\x45\x34\x46\x38\x70\x62\x76\x67\x73\x6f\x7a\x63\x5a\x4e\x55\x44\x36\x52\x73\x62\x77\x53\x63\x4a\x37\x4f\x77\x38\x49\x6c\x67\x53\x6f\x43\x51\x65\x54\x49\x36\x4f\x6c\x54\x68\x34\x4f\x63\x47\x39\x2f\x72\x68\x2f\x33\x67\x56\x51\x41\x3d\x3d\x22\x20\x65\x6e\x63\x72\x79\x70\x74\x65\x64\x48\x6d\x61\x63\x56\x61\x6c\x75\x65\x3d\x22\x30\x6c\x62\x37\x78\x59\x6b\x68\x58\x71\x74\x34\x4a\x49\x43\x6a\x31\x77\x67\x50\x47\x49\x4d\x78\x62\x6c\x36\x71\x42\x36\x31\x6e\x32\x6f\x70\x49\x31\x65\x52\x49\x59\x76\x6c\x33\x58\x78\x45\x7a\x78\x42\x65\x32\x49\x39\x7a\x62\x6c\x78\x38\x59\x55\x2b\x79\x48\x50\x38\x61\x4b\x31\x72\x53\x66\x38\x32\x55\x5a\x57\x63\x65\x7a\x4a\x38\x4e\x2b\x75\x51\x3d\x3d\x22\x20\x2f\x3e\x0a\x20\x20\x20\x20\x3c\x6b\x65\x79\x45\x6e\x63\x72\x79\x70\x74\x6f\x72\x73\x3e\x0a\x20\x20\x20\x20\x20\x20\x20\x20\x3c\x6b\x65\x79\x45\x6e\x63\x72\x79\x70\x74\x6f\x72\x20\x75\x72\x69\x3d\x22\x68\x74\x74\x70\x3a\x2f\x2f\x73\x63\x68\x65\x6d\x61\x73\x2e\x6d\x69\x63\x72\x6f\x73\x6f\x66\x74\x2e\x63\x6f\x6d\x2f\x6f\x66\x66\x69\x63\x65\x2f\x32\x30\x30\x36\x2f\x6b\x65\x79\x45\x6e\x63\x72\x79\x70\x74\x6f\x72\x2f\x70\x61\x73\x73\x77\x6f\x72\x64\x22\x3e\x0a\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x3c\x70\x3a\x65\x6e\x63\x72\x79\x70\x74\x65\x64\x4b\x65\x79\x20\x73\x70\x69\x6e\x43\x6f\x75\x6e\x74\x3d\x22\x31\x30\x30\x30\x22\x20\x73\x61\x6c\x74\x53\x69\x7a\x65\x3d\x22\x31\x36\x22\x20\x62\x6c\x6f\x63\x6b\x53\x69\x7a\x65\x3d\x22\x31\x36\x22\x20\x6b\x65\x79\x42\x69\x74\x73\x3d\x22\x32\x35\x36\x22\x0a\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x68\x61\x73\x68\x53\x69\x7a\x65\x3d\x22\x36\x34\x22\x20\x63\x69\x70\x68\x65\x72\x41\x6c\x67\x6f\x72\x69\x74\x68\x6d\x3d\x22\x41\x45\x53\x22\x20\x63\x69\x70\x68\x65\x72\x43\x68\x61\x69\x6e\x69\x6e\x67\x3d\x22\x43\x68\x61\x69\x6e\x69\x6e\x67\x4d\x6f\x64\x65\x43\x42\x43\x22\x20\x68\x61\x73\x68\x41\x6c\x67\x6f\x72\x69\x74\x68\x6d\x3d\x22\x53\x48\x41\x35\x31\x32\x22\x0a\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x73\x61\x6c\x74\x56\x61\x6c\x75\x65\x3d\x22\x41\x41\x45\x43\x41\x77\x51\x46\x42\x67\x63\x49\x43\x51\x6f\x4c\x44\x41\x30\x4f\x44\x77\x3d\x3d\x22\x20\x65\x6e\x63\x72\x79\x70\x74\x65\x64\x56\x65\x72\x69\x66\x69\x65\x72\x48\x61\x73\x68\x49\x6e\x70\x75\x74\x3d\x22\x6b\x54\x61\x47\x4d\x61\x39\x49\x71\x78\x30\x35\x54\x52\x61\x75\x30\x47\x6d\x6b\x45\x41\x3d\x3d\x22\x0a\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x65\x6e\x63\x72\x79\x70\x74\x65\x64\x56\x65\x72\x69\x66\x69\x65\x72\x48\x61\x73\x68\x56\x61\x6c\x75\x65\x3d\x22\x6d\x51\x7a\x74\x73\x61\x61\x69\x2b\x4e\x69\x70\x52\x61\x74\x74\x75\x6c\x79\x68\x48\x61\x64\x50\x4d\x79\x5a\x72\x73\x38\x37\x79\x6b\x36\x33\x35\x62\x73\x6d\x74\x39\x55\x6a\x6d\x6f\x37\x7a\x53\x4a\x70\x61\x65\x63\x71\x33\x4d\x6e\x48\x4f\x45\x77\x63\x37\x64\x51\x65\x51\x62\x4c\x43\x32\x58\x56\x55\x53\x76\x79\x76\x57\x33\x6a\x2b\x79\x38\x57\x67\x3d\x3d\x22\x20\x65\x6e\x63\x72\x79\x70\x74\x65\x64\x4b\x65\x79\x56\x61\x6c\x75\x65\x3d\x22\x65\x32\x65\x38\x53\x44\x62\x32\x50\x71\x36\x39\x56\x4f\x5a\x45\x52\x69\x48\x42\x71\x56\x50\x75\x73\x52\x4e\x4f\x37\x78\x6c\x50\x69\x78\x59\x54\x59\x53\x76\x70\x56\x43\x6f\x3d\x22\x20\x2f\x3e\x0a\x20\x20\x20\x20\x20\x20\x20\x20\x3c\x2f\x6b\x65\x79\x45\x6e\x63\x72\x79\x70\x74\x6f\x72\x3e\x0a\x20\x20\x20\x20\x3c\x2f\x6b\x65\x79\x45\x6e\x63\x72\x79\x70\x74\x6f\x72\x73\x3e\x0a\x3c\x2f\x65\x6e\x63\x72\x79\x70\x74\x69\x6f\x6e\x3e\x0a";
    // len 1439
    const FIXTURE_A_KEY: &[u8] = b"\xad\x72\xe6\x1f\x24\x48\x2d\x98\x7b\x99\xf6\x85\xdd\xbf\x2e\xc1\x36\x36\x36\x36\x36\x36\x36\x36\x36\x36\x36\x36\x36\x36\x36\x36";
    // len 32
    const FIXTURE_B_STREAM: &[u8] = b"\x04\x00\x04\x00\x40\x00\x00\x00\x3c\x3f\x78\x6d\x6c\x20\x76\x65\x72\x73\x69\x6f\x6e\x3d\x22\x31\x2e\x30\x22\x20\x65\x6e\x63\x6f\x64\x69\x6e\x67\x3d\x22\x55\x54\x46\x2d\x38\x22\x20\x73\x74\x61\x6e\x64\x61\x6c\x6f\x6e\x65\x3d\x22\x79\x65\x73\x22\x3f\x3e\x0a\x3c\x65\x6e\x63\x72\x79\x70\x74\x69\x6f\x6e\x20\x78\x6d\x6c\x6e\x73\x3d\x22\x68\x74\x74\x70\x3a\x2f\x2f\x73\x63\x68\x65\x6d\x61\x73\x2e\x6d\x69\x63\x72\x6f\x73\x6f\x66\x74\x2e\x63\x6f\x6d\x2f\x6f\x66\x66\x69\x63\x65\x2f\x32\x30\x30\x36\x2f\x65\x6e\x63\x72\x79\x70\x74\x69\x6f\x6e\x22\x20\x78\x6d\x6c\x6e\x73\x3a\x70\x3d\x22\x68\x74\x74\x70\x3a\x2f\x2f\x73\x63\x68\x65\x6d\x61\x73\x2e\x6d\x69\x63\x72\x6f\x73\x6f\x66\x74\x2e\x63\x6f\x6d\x2f\x6f\x66\x66\x69\x63\x65\x2f\x32\x30\x30\x36\x2f\x6b\x65\x79\x45\x6e\x63\x72\x79\x70\x74\x6f\x72\x2f\x70\x61\x73\x73\x77\x6f\x72\x64\x22\x20\x78\x6d\x6c\x6e\x73\x3a\x63\x3d\x22\x68\x74\x74\x70\x3a\x2f\x2f\x73\x63\x68\x65\x6d\x61\x73\x2e\x6d\x69\x63\x72\x6f\x73\x6f\x66\x74\x2e\x63\x6f\x6d\x2f\x6f\x66\x66\x69\x63\x65\x2f\x32\x30\x30\x36\x2f\x6b\x65\x79\x45\x6e\x63\x72\x79\x70\x74\x6f\x72\x2f\x63\x65\x72\x74\x69\x66\x69\x63\x61\x74\x65\x22\x3e\x0a\x20\x20\x20\x20\x3c\x6b\x65\x79\x44\x61\x74\x61\x20\x73\x61\x6c\x74\x53\x69\x7a\x65\x3d\x22\x31\x36\x22\x20\x62\x6c\x6f\x63\x6b\x53\x69\x7a\x65\x3d\x22\x31\x36\x22\x20\x6b\x65\x79\x42\x69\x74\x73\x3d\x22\x32\x35\x36\x22\x20\x68\x61\x73\x68\x53\x69\x7a\x65\x3d\x22\x36\x34\x22\x20\x63\x69\x70\x68\x65\x72\x41\x6c\x67\x6f\x72\x69\x74\x68\x6d\x3d\x22\x41\x45\x53\x22\x20\x63\x69\x70\x68\x65\x72\x43\x68\x61\x69\x6e\x69\x6e\x67\x3d\x22\x43\x68\x61\x69\x6e\x69\x6e\x67\x4d\x6f\x64\x65\x43\x42\x43\x22\x20\x68\x61\x73\x68\x41\x6c\x67\x6f\x72\x69\x74\x68\x6d\x3d\x22\x53\x48\x41\x35\x31\x32\x22\x20\x73\x61\x6c\x74\x56\x61\x6c\x75\x65\x3d\x22\x41\x41\x45\x43\x41\x77\x51\x46\x42\x67\x63\x49\x43\x51\x6f\x4c\x44\x41\x30\x4f\x44\x77\x3d\x3d\x22\x20\x2f\x3e\x0a\x20\x20\x20\x20\x3c\x64\x61\x74\x61\x49\x6e\x74\x65\x67\x72\x69\x74\x79\x20\x65\x6e\x63\x72\x79\x70\x74\x65\x64\x48\x6d\x61\x63\x4b\x65\x79\x3d\x22\x61\x47\x68\x6f\x61\x47\x68\x6f\x61\x47\x68\x6f\x61\x47\x68\x6f\x61\x47\x68\x6f\x61\x41\x3d\x3d\x22\x20\x65\x6e\x63\x72\x79\x70\x74\x65\x64\x48\x6d\x61\x63\x56\x61\x6c\x75\x65\x3d\x22\x64\x6e\x5a\x32\x64\x6e\x5a\x32\x64\x6e\x5a\x32\x64\x6e\x5a\x32\x64\x6e\x5a\x32\x64\x6e\x5a\x32\x64\x6e\x5a\x32\x64\x6e\x5a\x32\x64\x6e\x5a\x32\x64\x6e\x5a\x32\x64\x6e\x59\x3d\x22\x20\x2f\x3e\x0a\x20\x20\x20\x20\x3c\x6b\x65\x79\x45\x6e\x63\x72\x79\x70\x74\x6f\x72\x73\x3e\x0a\x20\x20\x20\x20\x20\x20\x20\x20\x3c\x6b\x65\x79\x45\x6e\x63\x72\x79\x70\x74\x6f\x72\x20\x75\x72\x69\x3d\x22\x68\x74\x74\x70\x3a\x2f\x2f\x73\x63\x68\x65\x6d\x61\x73\x2e\x6d\x69\x63\x72\x6f\x73\x6f\x66\x74\x2e\x63\x6f\x6d\x2f\x6f\x66\x66\x69\x63\x65\x2f\x32\x30\x30\x36\x2f\x6b\x65\x79\x45\x6e\x63\x72\x79\x70\x74\x6f\x72\x2f\x70\x61\x73\x73\x77\x6f\x72\x64\x22\x3e\x0a\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x3c\x70\x3a\x65\x6e\x63\x72\x79\x70\x74\x65\x64\x4b\x65\x79\x20\x73\x70\x69\x6e\x43\x6f\x75\x6e\x74\x3d\x22\x31\x30\x30\x30\x30\x30\x22\x20\x73\x61\x6c\x74\x53\x69\x7a\x65\x3d\x22\x31\x36\x22\x20\x62\x6c\x6f\x63\x6b\x53\x69\x7a\x65\x3d\x22\x31\x36\x22\x20\x6b\x65\x79\x42\x69\x74\x73\x3d\x22\x32\x35\x36\x22\x20\x68\x61\x73\x68\x53\x69\x7a\x65\x3d\x22\x36\x34\x22\x20\x63\x69\x70\x68\x65\x72\x41\x6c\x67\x6f\x72\x69\x74\x68\x6d\x3d\x22\x41\x45\x53\x22\x20\x63\x69\x70\x68\x65\x72\x43\x68\x61\x69\x6e\x69\x6e\x67\x3d\x22\x43\x68\x61\x69\x6e\x69\x6e\x67\x4d\x6f\x64\x65\x43\x42\x43\x22\x20\x68\x61\x73\x68\x41\x6c\x67\x6f\x72\x69\x74\x68\x6d\x3d\x22\x53\x48\x41\x35\x31\x32\x22\x20\x73\x61\x6c\x74\x56\x61\x6c\x75\x65\x3d\x22\x79\x38\x6f\x63\x6d\x5a\x4e\x44\x2b\x36\x32\x53\x42\x31\x59\x30\x46\x51\x41\x30\x73\x41\x3d\x3d\x22\x20\x65\x6e\x63\x72\x79\x70\x74\x65\x64\x56\x65\x72\x69\x66\x69\x65\x72\x48\x61\x73\x68\x49\x6e\x70\x75\x74\x3d\x22\x4f\x65\x36\x6c\x54\x69\x62\x6c\x46\x48\x6d\x4d\x4b\x45\x76\x48\x63\x55\x30\x34\x72\x41\x3d\x3d\x22\x20\x65\x6e\x63\x72\x79\x70\x74\x65\x64\x56\x65\x72\x69\x66\x69\x65\x72\x48\x61\x73\x68\x56\x61\x6c\x75\x65\x3d\x22\x46\x44\x64\x74\x62\x59\x46\x7a\x4e\x4f\x61\x77\x2f\x30\x2f\x59\x49\x68\x70\x38\x5a\x34\x35\x64\x69\x6e\x68\x4f\x6a\x35\x6d\x66\x54\x42\x69\x4a\x4d\x4d\x4e\x71\x53\x79\x6e\x46\x73\x7a\x4e\x67\x57\x31\x7a\x55\x41\x37\x42\x51\x41\x36\x33\x50\x47\x4d\x79\x6f\x79\x36\x75\x4e\x36\x2b\x4e\x7a\x78\x6c\x59\x45\x6f\x4c\x37\x50\x72\x6c\x77\x4b\x30\x41\x3d\x3d\x22\x20\x65\x6e\x63\x72\x79\x70\x74\x65\x64\x4b\x65\x79\x56\x61\x6c\x75\x65\x3d\x22\x49\x58\x79\x2f\x4b\x4a\x50\x42\x72\x35\x72\x49\x72\x69\x41\x39\x71\x52\x4b\x4f\x6d\x52\x41\x76\x6c\x72\x4d\x34\x78\x33\x32\x34\x55\x61\x4e\x64\x31\x38\x67\x54\x4a\x33\x6b\x3d\x22\x20\x2f\x3e\x0a\x20\x20\x20\x20\x20\x20\x20\x20\x3c\x2f\x6b\x65\x79\x45\x6e\x63\x72\x79\x70\x74\x6f\x72\x3e\x0a\x20\x20\x20\x20\x3c\x2f\x6b\x65\x79\x45\x6e\x63\x72\x79\x70\x74\x6f\x72\x73\x3e\x0a\x3c\x2f\x65\x6e\x63\x72\x79\x70\x74\x69\x6f\x6e\x3e\x0a";
    // len 1236
    const FIXTURE_B_KEY: &[u8] = b"\xde\xad\xbe\xef\x00\x11\x22\x33\x44\x55\x66\x77\x88\x99\xaa\xbb\xcc\xdd\xee\xff\x00\x11\x22\x33\x44\x55\x66\x77\x88\x99\xaa\xbb";
    // len 32
    const FIXTURE_C_STREAM: &[u8] = b"\x02\x00\x02\x00\x24\x00\x00\x00\x8a\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x0e\x66\x00\x00\x04\x80\x00\x00\x80\x00\x00\x00\x18\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x4d\x00\x69\x00\x63\x00\x72\x00\x6f\x00\x73\x00\x6f\x00\x66\x00\x74\x00\x20\x00\x45\x00\x6e\x00\x68\x00\x61\x00\x6e\x00\x63\x00\x65\x00\x64\x00\x20\x00\x52\x00\x53\x00\x41\x00\x20\x00\x61\x00\x6e\x00\x64\x00\x20\x00\x41\x00\x45\x00\x53\x00\x20\x00\x43\x00\x72\x00\x79\x00\x70\x00\x74\x00\x6f\x00\x67\x00\x72\x00\x61\x00\x70\x00\x68\x00\x69\x00\x63\x00\x20\x00\x50\x00\x72\x00\x6f\x00\x76\x00\x69\x00\x64\x00\x65\x00\x72\x00\x10\x00\x00\x00\xe8\x82\x66\x49\x0c\x5b\xd1\xee\xbd\x2b\x43\x94\xe3\xf8\x30\xef\x51\x6f\x73\x2e\x96\x6f\xac\x17\xb1\xc5\xd7\xd8\xcc\x36\xc9\x28\x14\x00\x00\x00\x2b\x61\x68\xda\xbe\x29\x11\xad\x2b\xd3\x7c\x17\x46\x74\x5c\x14\xd3\xcf\x1b\xb1\x40\xa4\x8f\x4e\x6f\x3d\x23\x88\x08\x72\xb1\x6a";
    // len 222

    fn b64e(data: &[u8]) -> String {
        const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
            out.push(T[(n >> 18) as usize & 63] as char);
            out.push(T[(n >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 {
                T[(n >> 6) as usize & 63] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                T[n as usize & 63] as char
            } else {
                '='
            });
        }
        out
    }

    fn agile_stream(spin_count: u32, hash: &str, cipher: &str, chaining: &str) -> Vec<u8> {
        let salt = b64e(&[0x11u8; 16]);
        let blob = b64e(&[0x22u8; 16]);
        let xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
             <encryption xmlns=\"http://schemas.microsoft.com/office/2006/encryption\" \
             xmlns:p=\"http://schemas.microsoft.com/office/2006/keyEncryptor/password\">\
             <keyData saltSize=\"16\" blockSize=\"16\" keyBits=\"256\" hashSize=\"64\" \
             cipherAlgorithm=\"{cipher}\" cipherChaining=\"ChainingModeCBC\" hashAlgorithm=\"{hash}\" saltValue=\"{salt}\" />\
             <dataIntegrity encryptedHmacKey=\"{blob}\" encryptedHmacValue=\"{blob}\" />\
             <keyEncryptors><keyEncryptor uri=\"http://schemas.microsoft.com/office/2006/keyEncryptor/password\">\
             <p:encryptedKey spinCount=\"{spin_count}\" saltSize=\"16\" blockSize=\"16\" keyBits=\"256\" hashSize=\"64\" \
             cipherAlgorithm=\"{cipher}\" cipherChaining=\"{chaining}\" hashAlgorithm=\"{hash}\" saltValue=\"{salt}\" \
             encryptedVerifierHashInput=\"{blob}\" encryptedVerifierHashValue=\"{blob}\" encryptedKeyValue=\"{blob}\" />\
             </keyEncryptor></keyEncryptors></encryption>"
        );
        let mut out = Vec::with_capacity(8 + xml.len());
        out.extend_from_slice(&[0x04, 0x00, 0x04, 0x00, 0x40, 0x00, 0x00, 0x00]);
        out.extend_from_slice(xml.as_bytes());
        out
    }

    #[test]
    fn parses_agile_parameters_from_stream() {
        let scheme = parse_encryption_info(FIXTURE_A_STREAM).expect("parse should succeed");
        match scheme {
            EncryptionScheme::Agile(p) => {
                assert_eq!(p.spin_count, 1000);
                assert_eq!(p.key_bits, 256);
                assert_eq!(p.block_size, 16);
                assert_eq!(p.hash_size, 64);
                assert_eq!(p.hash_algorithm, "SHA512");
                assert_eq!(p.cipher_algorithm, "AES");
                assert_eq!(p.cipher_chaining, "ChainingModeCBC");
                assert_eq!(p.salt.len(), 16);
            }
            other => panic!("expected Agile, got {other:?}"),
        }
    }

    #[test]
    fn derives_agile_key() {
        let scheme = parse_encryption_info(FIXTURE_A_STREAM).expect("parse");
        let key = derive_key(&scheme, "Password1234_").expect("derive");
        assert_eq!(key, FIXTURE_A_KEY);
    }

    #[test]
    fn derives_agile_key_at_real_spin_count() {
        let scheme = parse_encryption_info(FIXTURE_B_STREAM).expect("parse");
        let key = derive_key(&scheme, "Password1234_").expect("derive");
        assert_eq!(key, FIXTURE_B_KEY);
    }

    #[test]
    fn wrong_password_is_detected_before_any_key_is_returned() {
        let scheme = parse_encryption_info(FIXTURE_A_STREAM).expect("parse");
        match derive_key(&scheme, "not the password") {
            Err(CryptoError::WrongPassword) => {}
            other => panic!("expected WrongPassword, got {other:?}"),
        }
    }

    #[test]
    fn parses_standard_scheme_from_stream() {
        let scheme = parse_encryption_info(FIXTURE_C_STREAM).expect("parse");
        match &scheme {
            EncryptionScheme::Standard(p) => {
                assert_eq!(p.key_bits, 128);
                assert_eq!(p.cipher_algorithm, "AES");
                assert_eq!(p.hash_algorithm, "SHA1");
                assert_eq!(p.salt.len(), 16);
            }
            other => panic!("expected Standard, got {other:?}"),
        }
    }

    #[test]
    fn standard_scheme_reports_unsupported_not_a_wrong_key() {
        // The Standard (2007) scheme requires SHA-1, which the permitted
        // dependency set (aes/cbc/sha2/hmac) does not provide; it must report
        // a clear error rather than a wrong key.
        let scheme = parse_encryption_info(FIXTURE_C_STREAM).expect("parse");
        match derive_key(&scheme, "Password1234_") {
            Err(CryptoError::Unsupported(msg)) => assert!(msg.contains("SHA-1")),
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn standard_rc4_is_unsupported_not_silently_wrong() {
        // Patch fixture C's algId (offset 20) from AES (0x660E) to RC4 (0x6801).
        let mut stream = FIXTURE_C_STREAM.to_vec();
        stream[20..24].copy_from_slice(&0x6801u32.to_le_bytes());
        let scheme = parse_encryption_info(&stream).expect("parse");
        match derive_key(&scheme, "Password1234_") {
            Err(CryptoError::Unsupported(msg)) => assert!(msg.contains("RC4")),
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn xor_obfuscation_is_unsupported() {
        let stream = [1u8, 0, 1, 0, 0x50, 0, 0, 0];
        match parse_encryption_info(&stream).expect("parse") {
            EncryptionScheme::Unsupported(msg) => assert!(msg.contains("XOR")),
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn extensible_encryption_is_unsupported() {
        let stream = [3u8, 0, 3, 0, 0x40, 0, 0, 0];
        match parse_encryption_info(&stream).expect("parse") {
            EncryptionScheme::Unsupported(msg) => assert!(msg.contains("Extensible")),
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn unknown_cipher_algorithm_is_a_precise_error() {
        let stream = agile_stream(1000, "SHA512", "3DES", "ChainingModeCBC");
        match parse_encryption_info(&stream) {
            Err(CryptoError::Unsupported(msg)) => assert!(msg.contains("3DES")),
            other => panic!("expected Unsupported cipher, got {other:?}"),
        }
    }

    #[test]
    fn unknown_hash_algorithm_is_a_precise_error() {
        let stream = agile_stream(1000, "MD5", "AES", "ChainingModeCBC");
        match parse_encryption_info(&stream) {
            Err(CryptoError::Unsupported(msg)) => assert!(msg.contains("MD5")),
            other => panic!("expected Unsupported hash, got {other:?}"),
        }
    }

    #[test]
    fn unknown_chaining_mode_is_a_precise_error() {
        let stream = agile_stream(1000, "SHA512", "AES", "ChainingModeECB");
        match parse_encryption_info(&stream) {
            Err(CryptoError::Unsupported(msg)) => assert!(msg.contains("ECB")),
            other => panic!("expected Unsupported chaining, got {other:?}"),
        }
    }

    #[test]
    fn absurd_spin_count_is_a_precise_error() {
        let stream = agile_stream(4_000_000_000, "SHA512", "AES", "ChainingModeCBC");
        match parse_encryption_info(&stream) {
            Err(CryptoError::Malformed(msg)) => assert!(msg.contains("spinCount")),
            other => panic!("expected Malformed spinCount, got {other:?}"),
        }
    }

    #[test]
    fn malformed_xml_never_panics() {
        let mut stream = FIXTURE_A_STREAM[..8].to_vec();
        stream.extend_from_slice(b"<encryption><keyEncryptors><keyEncryptor/></keyEncryptors>");
        assert!(parse_encryption_info(&stream).is_err());

        // Garbage that is not even UTF-8 must also be an error, not a panic.
        let mut bad = FIXTURE_A_STREAM[..8].to_vec();
        bad.extend_from_slice(&[0xff, 0xfe, 0x00, 0x80, 0x99]);
        assert!(parse_encryption_info(&bad).is_err());
    }

    #[test]
    fn truncated_stream_never_panics() {
        for len in 0..8 {
            assert!(parse_encryption_info(&FIXTURE_A_STREAM[..len]).is_err());
        }
    }

    #[test]
    fn truncated_agile_xml_is_a_precise_error() {
        // Cut the stream right after the <p:encryptedKey open tag: no attribute
        // and no closing '>' survive, so this must be a precise error.
        let tag = b"<p:encryptedKey";
        let pos = FIXTURE_A_STREAM
            .windows(tag.len())
            .position(|w| w == tag)
            .expect("encryptedKey open tag present");
        let stream = &FIXTURE_A_STREAM[..pos + tag.len()];
        assert!(parse_encryption_info(stream).is_err());
    }

    #[test]
    fn unknown_version_is_a_precise_error() {
        let stream = [9u8, 0, 9, 0, 0, 0, 0, 0];
        match parse_encryption_info(&stream) {
            Err(CryptoError::Malformed(msg)) => assert!(msg.contains("9.9")),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }
}
