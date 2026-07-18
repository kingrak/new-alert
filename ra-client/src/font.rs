//! A tiny built-in 5×7 bitmap font for the minimal sidebar (DESIGN.md §4.9 M5:
//! "functional, not pretty"). This is client-side presentation only — no sim
//! state, no external asset — so the build UI can print credits, power, item
//! names, costs, and build progress without a real font pipeline (which lands
//! with the polished sidebar in M7).
//!
//! Each glyph is 5 wide × 7 tall, one `u8` per row (top row first); the low 5
//! bits are the pixel columns, MSB = leftmost column.

use crate::compositor::RgbaImage;

/// Glyph cell width in pixels.
pub const GLYPH_W: i32 = 5;
/// Glyph cell height in pixels.
pub const GLYPH_H: i32 = 7;
/// Horizontal advance per character (glyph + 1px gap).
pub const ADVANCE: i32 = GLYPH_W + 1;

/// Look up a glyph's 7 row-bitmaps for an ASCII byte (uppercased). Unknown
/// characters render as blank.
fn glyph(c: u8) -> [u8; 7] {
    let c = c.to_ascii_uppercase();
    match c {
        b' ' => [0, 0, 0, 0, 0, 0, 0],
        b'0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        b'1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        b'2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        b'3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        b'4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        b'5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        b'6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        b'7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        b'8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        b'9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        b'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        b'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        b'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        b'D' => [0x1C, 0x12, 0x11, 0x11, 0x11, 0x12, 0x1C],
        b'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        b'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        b'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        b'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        b'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        b'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        b'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        b'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        b'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        b'N' => [0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11],
        b'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        b'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        b'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        b'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        b'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        b'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        b'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        b'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        b'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x1B, 0x11],
        b'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        b'Y' => [0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x04],
        b'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        b'-' => [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00],
        b'%' => [0x19, 0x1A, 0x02, 0x04, 0x08, 0x0B, 0x13],
        b'/' => [0x01, 0x02, 0x02, 0x04, 0x08, 0x08, 0x10],
        b':' => [0x00, 0x04, 0x04, 0x00, 0x04, 0x04, 0x00],
        b'.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C],
        b'$' => [0x04, 0x0F, 0x14, 0x0E, 0x05, 0x1E, 0x04],
        _ => [0, 0, 0, 0, 0, 0, 0],
    }
}

/// Draw a single character at pixel (x, y) (top-left), in `rgb`. Clipped.
pub fn draw_char(dst: &mut RgbaImage, x: i32, y: i32, c: u8, rgb: [u8; 3]) {
    let rows = glyph(c);
    for (ry, bits) in rows.iter().enumerate() {
        for cx in 0..GLYPH_W {
            // MSB is the leftmost of the 5 columns.
            if bits & (1 << (GLYPH_W - 1 - cx)) != 0 {
                let px = x + cx;
                let py = y + ry as i32;
                if px >= 0 && py >= 0 && (px as u32) < dst.width && (py as u32) < dst.height {
                    let di = ((py as u32 * dst.width + px as u32) * 4) as usize;
                    dst.pixels[di] = rgb[0];
                    dst.pixels[di + 1] = rgb[1];
                    dst.pixels[di + 2] = rgb[2];
                    dst.pixels[di + 3] = 255;
                }
            }
        }
    }
}

/// Draw a left-aligned string at (x, y). Returns the x just past the text.
pub fn draw_text(dst: &mut RgbaImage, x: i32, y: i32, text: &str, rgb: [u8; 3]) -> i32 {
    let mut cx = x;
    for &b in text.as_bytes() {
        draw_char(dst, cx, y, b, rgb);
        cx += ADVANCE;
    }
    cx
}

/// Pixel width a string will occupy.
pub fn text_width(text: &str) -> i32 {
    text.len() as i32 * ADVANCE
}

/// Draw a left-aligned string at (x, y) magnified by an integer `scale` (each
/// glyph pixel becomes a `scale`×`scale` block) — for the big VICTORY/DEFEAT
/// banner. Clipped. Returns the x just past the text.
pub fn draw_text_scaled(
    dst: &mut RgbaImage,
    x: i32,
    y: i32,
    text: &str,
    rgb: [u8; 3],
    scale: i32,
) -> i32 {
    let scale = scale.max(1);
    let mut cx = x;
    for &b in text.as_bytes() {
        let rows = glyph(b);
        for (ry, bits) in rows.iter().enumerate() {
            for gx in 0..GLYPH_W {
                if bits & (1 << (GLYPH_W - 1 - gx)) == 0 {
                    continue;
                }
                for sy in 0..scale {
                    for sx in 0..scale {
                        let px = cx + gx * scale + sx;
                        let py = y + ry as i32 * scale + sy;
                        if px >= 0 && py >= 0 && (px as u32) < dst.width && (py as u32) < dst.height
                        {
                            let di = ((py as u32 * dst.width + px as u32) * 4) as usize;
                            dst.pixels[di] = rgb[0];
                            dst.pixels[di + 1] = rgb[1];
                            dst.pixels[di + 2] = rgb[2];
                            dst.pixels[di + 3] = 255;
                        }
                    }
                }
            }
        }
        cx += ADVANCE * scale;
    }
    cx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_within_bounds_and_marks_pixels() {
        let mut img = RgbaImage {
            width: 40,
            height: 10,
            pixels: vec![0u8; 40 * 10 * 4],
        };
        draw_text(&mut img, 1, 1, "AB1", [255, 255, 255]);
        // Some pixel should have been set (non-zero alpha somewhere).
        let any = img.pixels.chunks_exact(4).any(|p| p[3] == 255);
        assert!(any, "text drew nothing");
    }

    #[test]
    fn clips_off_screen() {
        let mut img = RgbaImage {
            width: 6,
            height: 8,
            pixels: vec![0u8; 6 * 8 * 4],
        };
        // Far off-screen: must not panic.
        draw_text(&mut img, 100, 100, "Z", [1, 2, 3]);
        draw_text(&mut img, -50, -50, "Z", [1, 2, 3]);
    }
}
