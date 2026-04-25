//! Lightweight image dimension parser for PNG and JPEG.
//!
//! Used by the TUI to display "Image: WxH" alongside slideshow / cover-art
//! content items without pulling in an `image` crate dependency. We only
//! support the two formats DAB MOT slideshow actually carries (TS 101 499).

/// Width and height in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dimensions {
    pub width: u32,
    pub height: u32,
}

/// Sniff the first few bytes of an image payload and return its dimensions
/// when the format is recognised.
pub fn sniff(bytes: &[u8]) -> Option<Dimensions> {
    if let Some(d) = sniff_png(bytes) {
        return Some(d);
    }
    sniff_jpeg(bytes)
}

fn sniff_png(bytes: &[u8]) -> Option<Dimensions> {
    // PNG signature, then IHDR chunk: 8-byte sig + 4-byte length + 4-byte
    // "IHDR" + 4-byte width + 4-byte height.
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1A\n" || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    Some(Dimensions { width, height })
}

fn sniff_jpeg(bytes: &[u8]) -> Option<Dimensions> {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return None;
    }
    let mut i = 2usize;
    while i + 3 < bytes.len() {
        if bytes[i] != 0xFF {
            return None;
        }
        // Skip fill bytes.
        let mut marker = bytes[i + 1];
        let mut adv = 1usize;
        while marker == 0xFF && i + 1 + adv < bytes.len() {
            marker = bytes[i + 1 + adv];
            adv += 1;
        }
        i += 1 + adv;
        // Markers without payload.
        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            continue;
        }
        if i + 1 >= bytes.len() {
            return None;
        }
        let seg_len = u16::from_be_bytes([bytes[i], bytes[i + 1]]) as usize;
        if seg_len < 2 {
            return None;
        }
        // SOFn markers (excluding DHT 0xC4, JPG 0xC8, DAC 0xCC) carry frame
        // header: precision(1) | height(2) | width(2) | components(1).
        let is_sof =
            (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC;
        if is_sof {
            if i + 7 >= bytes.len() {
                return None;
            }
            let height = u16::from_be_bytes([bytes[i + 3], bytes[i + 4]]) as u32;
            let width = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
            if width == 0 || height == 0 {
                return None;
            }
            return Some(Dimensions { width, height });
        }
        i += seg_len;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_dimensions_from_ihdr() {
        // Minimal PNG: signature + IHDR chunk (length=13, type, 13-byte data, fake CRC).
        let mut buf = Vec::new();
        buf.extend_from_slice(b"\x89PNG\r\n\x1A\n");
        buf.extend_from_slice(&13u32.to_be_bytes());
        buf.extend_from_slice(b"IHDR");
        buf.extend_from_slice(&320u32.to_be_bytes()); // width
        buf.extend_from_slice(&240u32.to_be_bytes()); // height
        buf.extend_from_slice(&[8, 2, 0, 0, 0]); // bit depth, color type, etc.
        buf.extend_from_slice(&[0, 0, 0, 0]); // CRC
        let dim = sniff(&buf).expect("png dim");
        assert_eq!(dim.width, 320);
        assert_eq!(dim.height, 240);
    }

    #[test]
    fn jpeg_dimensions_from_sof0() {
        // SOI, then a JFIF APP0 segment, then SOF0 with 640x480.
        let mut buf = Vec::new();
        buf.extend_from_slice(&[0xFF, 0xD8]); // SOI
        buf.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10]); // APP0, length=16
        buf.extend_from_slice(b"JFIF\0");
        buf.extend_from_slice(&[1, 1, 0, 0, 1, 0, 1, 0, 0]); // 9 bytes JFIF body
        buf.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11]); // SOF0, length=17
        buf.push(8); // precision
        buf.extend_from_slice(&480u16.to_be_bytes()); // height
        buf.extend_from_slice(&640u16.to_be_bytes()); // width
        buf.push(3); // components
        buf.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]); // padding for length
        let dim = sniff(&buf).expect("jpeg dim");
        assert_eq!(dim.width, 640);
        assert_eq!(dim.height, 480);
    }

    #[test]
    fn unknown_format_returns_none() {
        assert!(sniff(b"not an image").is_none());
        assert!(sniff(&[]).is_none());
    }
}
