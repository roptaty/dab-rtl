/// Decode DAB text using the registered charset values from ETSI TS 101 756.
///
/// Supported charsets:
/// - `0x0`: Complete EBU Latin based repertoire (Annex C)
/// - `0x6`: ISO/IEC 10646 using UCS-2 transformation format, big-endian
/// - `0xF`: ISO/IEC 10646 using UTF-8 transformation format
///
/// Unknown charsets fall back to EBU Latin.
pub fn decode_dab_text(bytes: &[u8], charset: u8) -> String {
    decode_dab_text_raw(bytes, charset)
        .trim_matches(|c: char| c == '\0' || c.is_whitespace())
        .to_string()
}

/// Decode DAB text without trimming. DL+ markers (TS 102 980 §7) reference
/// character offsets within the *transmitted* dynamic label text, so DLS
/// callers must preserve leading whitespace to keep tag offsets aligned.
pub fn decode_dab_text_raw(bytes: &[u8], charset: u8) -> String {
    match charset {
        0x06 => decode_charset_06(bytes),
        0x0F => String::from_utf8_lossy(bytes).into_owned(),
        _ => decode_ebu_latin(bytes),
    }
}

fn decode_charset_06(bytes: &[u8]) -> String {
    // In practice, some streams advertise charset 0x06 but still carry
    // single-byte UTF-8/ASCII text. Prefer UTF-16BE only when the byte
    // pattern actually looks like it.
    if looks_like_ucs2_be(bytes) {
        decode_ucs2_be(bytes)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

fn decode_ucs2_be(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks_exact(2) {
        let code = u16::from_be_bytes([chunk[0], chunk[1]]) as u32;
        if let Some(ch) = char::from_u32(code) {
            out.push(ch);
        }
    }
    out
}

fn decode_ebu_latin(bytes: &[u8]) -> String {
    bytes
        .iter()
        .filter_map(|&b| {
            let code = EBU_LATIN[b as usize];
            if code == 0 {
                None
            } else {
                char::from_u32(code)
            }
        })
        .collect()
}

fn looks_like_ucs2_be(bytes: &[u8]) -> bool {
    if bytes.len() < 4 || !bytes.len().is_multiple_of(2) {
        return false;
    }

    if bytes.starts_with(&[0xFE, 0xFF]) || bytes.starts_with(&[0xFF, 0xFE]) {
        return true;
    }

    let pairs = bytes.chunks_exact(2);
    let pair_count = pairs.len();
    let mut plausible_high_bytes = 0usize;
    let mut plausible_low_bytes = 0usize;

    for pair in bytes.chunks_exact(2) {
        let high = pair[0];
        let low = pair[1];
        if high <= 0x04 || high == 0x20 {
            plausible_high_bytes += 1;
        }
        if low == b' ' || low.is_ascii_graphic() || low >= 0xA0 {
            plausible_low_bytes += 1;
        }
    }

    plausible_high_bytes * 4 >= pair_count * 3 && plausible_low_bytes * 4 >= pair_count * 3
}

const EBU_LATIN: [u32; 256] = [
    0x0000, 0x0118, 0x012E, 0x0172, 0x0102, 0x0116, 0x010E, 0x0218, 0x021A, 0x010A, 0x0000, 0x0000,
    0x0120, 0x0139, 0x017B, 0x0143, 0x0105, 0x0119, 0x012F, 0x0173, 0x0103, 0x0117, 0x010F, 0x0219,
    0x021B, 0x010B, 0x0147, 0x011A, 0x0121, 0x013A, 0x017C, 0x0000, 0x0020, 0x0021, 0x0022, 0x0023,
    0x0142, 0x0025, 0x0026, 0x0027, 0x0028, 0x0029, 0x002A, 0x002B, 0x002C, 0x002D, 0x002E, 0x002F,
    0x0030, 0x0031, 0x0032, 0x0033, 0x0034, 0x0035, 0x0036, 0x0037, 0x0038, 0x0039, 0x003A, 0x003B,
    0x003C, 0x003D, 0x003E, 0x003F, 0x0040, 0x0041, 0x0042, 0x0043, 0x0044, 0x0045, 0x0046, 0x0047,
    0x0048, 0x0049, 0x004A, 0x004B, 0x004C, 0x004D, 0x004E, 0x004F, 0x0050, 0x0051, 0x0052, 0x0053,
    0x0054, 0x0055, 0x0056, 0x0057, 0x0058, 0x0059, 0x005A, 0x005B, 0x016E, 0x005D, 0x0141, 0x005F,
    0x0104, 0x0061, 0x0062, 0x0063, 0x0064, 0x0065, 0x0066, 0x0067, 0x0068, 0x0069, 0x006A, 0x006B,
    0x006C, 0x006D, 0x006E, 0x006F, 0x0070, 0x0071, 0x0072, 0x0073, 0x0074, 0x0075, 0x0076, 0x0077,
    0x0078, 0x0079, 0x007A, 0x00AB, 0x016F, 0x00BB, 0x013D, 0x0126, 0x00E1, 0x00E0, 0x00E9, 0x00E8,
    0x00ED, 0x00EC, 0x00F3, 0x00F2, 0x00FA, 0x00F9, 0x00D1, 0x00C7, 0x015E, 0x00DF, 0x00A1, 0x0178,
    0x00E2, 0x00E4, 0x00EA, 0x00EB, 0x00EE, 0x00EF, 0x00F4, 0x00F6, 0x00FB, 0x00FC, 0x00F1, 0x00E7,
    0x015F, 0x011F, 0x0131, 0x00FF, 0x0136, 0x0145, 0x00A9, 0x0122, 0x011E, 0x011B, 0x0148, 0x0151,
    0x0150, 0x20AC, 0x00A3, 0x0024, 0x0100, 0x0112, 0x012A, 0x016A, 0x0137, 0x0146, 0x013B, 0x0123,
    0x013C, 0x0130, 0x0144, 0x0171, 0x0170, 0x00BF, 0x013E, 0x00B0, 0x0101, 0x0113, 0x012B, 0x016B,
    0x00C1, 0x00C0, 0x00C9, 0x00C8, 0x00CD, 0x00CC, 0x00D3, 0x00D2, 0x00DA, 0x00D9, 0x0158, 0x010C,
    0x0160, 0x017D, 0x00D0, 0x013F, 0x00C2, 0x00C4, 0x00CA, 0x00CB, 0x00CE, 0x00CF, 0x00D4, 0x00D6,
    0x00DB, 0x00DC, 0x0159, 0x010D, 0x0161, 0x017E, 0x0111, 0x0140, 0x00C3, 0x00C5, 0x00C6, 0x0152,
    0x0177, 0x00DD, 0x00D5, 0x00D8, 0x00DE, 0x014A, 0x0154, 0x0106, 0x015A, 0x0179, 0x0164, 0x00F0,
    0x00E3, 0x00E5, 0x00E6, 0x0153, 0x0175, 0x00FD, 0x00F5, 0x00F8, 0x00FE, 0x014B, 0x0155, 0x0107,
    0x015B, 0x017A, 0x0165, 0x0127,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ebu_latin_maps_non_ascii() {
        assert_eq!(decode_dab_text(&[0x24, 0x5C, 0x80], 0x00), "łŮá");
    }

    #[test]
    fn utf8_charset_decodes_utf8() {
        assert_eq!(decode_dab_text("Hélló".as_bytes(), 0x0F), "Hélló");
    }

    #[test]
    fn ucs2_charset_decodes_big_endian() {
        let bytes = [0x00, b'H', 0x00, b'i', 0x01, 0x42];
        assert_eq!(decode_dab_text(&bytes, 0x06), "Hił");
    }

    #[test]
    fn charset_06_falls_back_to_utf8_for_single_byte_text() {
        assert_eq!(decode_dab_text(b"Title", 0x06), "Title");
    }

    #[test]
    fn charset_06_falls_back_to_utf8_for_even_length_single_byte_text() {
        assert_eq!(decode_dab_text(b"Song 12", 0x06), "Song 12");
    }
}
