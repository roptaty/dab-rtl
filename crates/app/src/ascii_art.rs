//! JPEG-to-ASCII rendering for terminal slideshow / cover-art previews.

use jpeg_decoder::{Decoder, PixelFormat};

const RAMP: &[u8] = b" .:-=+*#%@";

/// Decode `bytes` as JPEG and render it into at most `max_width` by
/// `max_height` terminal cells. Returns `None` if the payload is not a
/// decodable JPEG or the target area is unusably small.
pub fn jpeg_to_ascii(bytes: &[u8], max_width: u16, max_height: u16) -> Option<Vec<String>> {
    if max_width < 8 || max_height < 4 {
        return None;
    }

    let mut decoder = Decoder::new(bytes);
    let pixels = decoder.decode().ok()?;
    let info = decoder.info()?;
    let width = info.width as usize;
    let height = info.height as usize;
    if width == 0 || height == 0 {
        return None;
    }

    let luma = pixels_to_luma(&pixels, info.pixel_format)?;
    Some(render_luma_ascii(
        &luma,
        width,
        height,
        max_width as usize,
        max_height as usize,
    ))
}

fn pixels_to_luma(pixels: &[u8], format: PixelFormat) -> Option<Vec<u8>> {
    match format {
        PixelFormat::L8 => Some(pixels.to_vec()),
        PixelFormat::RGB24 => {
            let mut out = Vec::with_capacity(pixels.len() / 3);
            for px in pixels.chunks_exact(3) {
                out.push(rgb_to_luma(px[0], px[1], px[2]));
            }
            Some(out)
        }
        PixelFormat::CMYK32 => {
            let mut out = Vec::with_capacity(pixels.len() / 4);
            for px in pixels.chunks_exact(4) {
                let c = px[0] as u16;
                let m = px[1] as u16;
                let y = px[2] as u16;
                let k = px[3] as u16;
                let r = 255u16.saturating_sub((c + k).min(255)) as u8;
                let g = 255u16.saturating_sub((m + k).min(255)) as u8;
                let b = 255u16.saturating_sub((y + k).min(255)) as u8;
                out.push(rgb_to_luma(r, g, b));
            }
            Some(out)
        }
        PixelFormat::L16 => {
            let mut out = Vec::with_capacity(pixels.len() / 2);
            for px in pixels.chunks_exact(2) {
                out.push(px[0]);
            }
            Some(out)
        }
    }
}

fn rgb_to_luma(r: u8, g: u8, b: u8) -> u8 {
    ((r as u32 * 299 + g as u32 * 587 + b as u32 * 114) / 1000) as u8
}

fn render_luma_ascii(
    luma: &[u8],
    width: usize,
    height: usize,
    max_width: usize,
    max_height: usize,
) -> Vec<String> {
    let target_width = width.min(max_width).max(1);
    // Terminal cells are taller than they are wide; using half the pixel
    // aspect ratio keeps cover art from looking vertically stretched.
    let aspect_height = ((height as f32 / width as f32) * target_width as f32 * 0.5).round();
    let target_height = (aspect_height as usize).clamp(1, max_height.max(1));

    let mut lines = Vec::with_capacity(target_height);
    for y in 0..target_height {
        let src_y0 = y * height / target_height;
        let src_y1 = ((y + 1) * height / target_height)
            .max(src_y0 + 1)
            .min(height);
        let mut line = String::with_capacity(target_width);
        for x in 0..target_width {
            let src_x0 = x * width / target_width;
            let src_x1 = ((x + 1) * width / target_width).max(src_x0 + 1).min(width);
            let mut sum = 0u32;
            let mut count = 0u32;
            for yy in src_y0..src_y1 {
                let row = yy * width;
                for xx in src_x0..src_x1 {
                    sum += luma[row + xx] as u32;
                    count += 1;
                }
            }
            let avg = sum.checked_div(count).unwrap_or(0);
            let idx = avg as usize * (RAMP.len() - 1) / 255;
            line.push(RAMP[idx] as char);
        }
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_luma_ascii_respects_bounds() {
        let luma = vec![0, 64, 128, 255, 255, 128, 64, 0];
        let lines = render_luma_ascii(&luma, 4, 2, 2, 2);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].len(), 2);
    }

    #[test]
    fn invalid_jpeg_returns_none() {
        assert!(jpeg_to_ascii(b"not a jpeg", 40, 12).is_none());
    }
}
