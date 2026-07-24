//! XML entity decoding and small parse helpers shared across turbo submodules.

#[inline]
pub(crate) fn atoi(raw: &[u8]) -> Option<u32> {
    if raw.is_empty() {
        return None;
    }
    let mut v: u32 = 0;
    for &b in raw {
        if b.is_ascii_digit() {
            v = v.wrapping_mul(10).wrapping_add((b - b'0') as u32);
        } else {
            return None;
        }
    }
    Some(v)
}

#[inline]
pub(crate) fn decode<'a>(raw: &'a [u8], scratch: &'a mut Vec<u8>) -> &'a [u8] {
    if memchr::memchr(b'&', raw).is_none() {
        return raw;
    }
    scratch.clear();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'&' {
            if let Some(semi) = memchr::memchr(b';', &raw[i..]) {
                let ent = &raw[i + 1..i + semi];
                match ent {
                    b"amp" => scratch.push(b'&'),
                    b"lt" => scratch.push(b'<'),
                    b"gt" => scratch.push(b'>'),
                    b"quot" => scratch.push(b'"'),
                    b"apos" => scratch.push(b'\''),
                    _ => {
                        if ent.first() == Some(&b'#') {
                            let cp: u32 = if ent.get(1) == Some(&b'x') || ent.get(1) == Some(&b'X')
                            {
                                u32::from_str_radix(
                                    std::str::from_utf8(&ent[2..]).unwrap_or("0"),
                                    16,
                                )
                                .unwrap_or(0)
                            } else {
                                std::str::from_utf8(&ent[1..])
                                    .unwrap_or("0")
                                    .parse()
                                    .unwrap_or(0)
                            };
                            if let Some(c) = char::from_u32(cp) {
                                let mut b = [0u8; 4];
                                scratch.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
                            }
                        } else {
                            scratch.extend_from_slice(&raw[i..i + semi + 1]);
                        }
                    }
                }
                i += semi + 1;
                continue;
            }
        }
        scratch.push(raw[i]);
        i += 1;
    }
    &scratch[..]
}

/// Public wrapper used by styles / structural parsers.
pub(crate) fn decode_bytes<'a>(raw: &'a [u8], scratch: &'a mut Vec<u8>) -> &'a [u8] {
    decode(raw, scratch)
}
