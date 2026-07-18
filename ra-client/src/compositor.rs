//! Pure compositing core: indexed-color raster assembly and palette expansion to
//! RGBA. No macroquad, no platform, no I/O — just buffers and math, so it runs
//! headless and is trivially testable. The client may use floats freely, but
//! this module needs none.
//!
//! The terrain is assembled once into a single large indexed-color
//! [`IndexedImage`] (128×128 cells × 24 px = 3072×3072), then any camera
//! viewport is produced by [`viewport_rgba`], which palette-maps a sub-rectangle
//! to RGBA8. Both steps are byte-for-byte deterministic.

/// A 256-entry RGB palette (already expanded to 8-bit per channel).
pub type Palette = [[u8; 3]; 256];

/// An indexed-color image: one palette index per pixel, row-major.
#[derive(Debug, Clone)]
pub struct IndexedImage {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height` palette indices, row-major.
    pub pixels: Vec<u8>,
}

/// An RGBA8 image, row-major, 4 bytes per pixel.
#[derive(Debug, Clone)]
pub struct RgbaImage {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes (R, G, B, A), row-major.
    pub pixels: Vec<u8>,
}

impl IndexedImage {
    /// Create an image filled with palette index `fill`.
    pub fn filled(width: u32, height: u32, fill: u8) -> IndexedImage {
        IndexedImage {
            width,
            height,
            pixels: vec![fill; (width as usize) * (height as usize)],
        }
    }

    /// Blit a `tw`×`th` tile of indexed pixels at top-left (`dx`, `dy`).
    ///
    /// When `transparent` is true, source palette index 0 is skipped (leaves the
    /// destination pixel unchanged), matching the original's `TransFlag` blit;
    /// otherwise every pixel is copied. Out-of-bounds destination pixels are
    /// clipped.
    pub fn blit_tile(
        &mut self,
        dx: i64,
        dy: i64,
        tile: &[u8],
        tw: u32,
        th: u32,
        transparent: bool,
    ) {
        if tile.len() < (tw as usize) * (th as usize) {
            return;
        }
        for ty in 0..th as i64 {
            let py = dy + ty;
            if py < 0 || py >= self.height as i64 {
                continue;
            }
            let row = (py as usize) * (self.width as usize);
            let srow = (ty as usize) * (tw as usize);
            for tx in 0..tw as i64 {
                let px = dx + tx;
                if px < 0 || px >= self.width as i64 {
                    continue;
                }
                let s = tile[srow + tx as usize];
                if transparent && s == 0 {
                    continue;
                }
                self.pixels[row + px as usize] = s;
            }
        }
    }
}

/// Expand an entire indexed image to RGBA using `palette`, forcing alpha 255.
pub fn to_rgba(img: &IndexedImage, palette: &Palette) -> RgbaImage {
    viewport_rgba(img, palette, 0, 0, img.width, img.height)
}

/// Palette-map a sub-rectangle of `img` to a fresh RGBA image of size `w`×`h`.
///
/// The rectangle may extend outside the source; pixels outside the source are
/// emitted as opaque black. This is the camera-viewport operation: pass the
/// camera's top-left cell offset (in pixels) and the viewport size.
pub fn viewport_rgba(
    img: &IndexedImage,
    palette: &Palette,
    x: i64,
    y: i64,
    w: u32,
    h: u32,
) -> RgbaImage {
    let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
    for oy in 0..h as i64 {
        let sy = y + oy;
        for ox in 0..w as i64 {
            let sx = x + ox;
            let di = ((oy as usize) * (w as usize) + ox as usize) * 4;
            if sx >= 0 && sy >= 0 && (sx as u32) < img.width && (sy as u32) < img.height {
                let idx = img.pixels[(sy as usize) * (img.width as usize) + sx as usize];
                let [r, g, b] = palette[idx as usize];
                out[di] = r;
                out[di + 1] = g;
                out[di + 2] = b;
            }
            out[di + 3] = 255;
        }
    }
    RgbaImage {
        width: w,
        height: h,
        pixels: out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_palette() -> Palette {
        let mut p = [[0u8; 3]; 256];
        p[0] = [0, 0, 0];
        p[1] = [255, 0, 0];
        p[2] = [0, 255, 0];
        p
    }

    #[test]
    fn blit_opaque_copies_index_zero() {
        let mut img = IndexedImage::filled(4, 4, 9);
        let tile = [0u8, 1, 1, 0]; // 2x2
        img.blit_tile(0, 0, &tile, 2, 2, false);
        assert_eq!(&img.pixels[0..2], &[0, 1]);
        assert_eq!(img.pixels[img.width as usize], 1); // row 1 col 0
    }

    #[test]
    fn blit_transparent_skips_index_zero() {
        let mut img = IndexedImage::filled(2, 2, 9);
        let tile = [0u8, 1, 1, 0];
        img.blit_tile(0, 0, &tile, 2, 2, true);
        assert_eq!(img.pixels, vec![9, 1, 1, 9]); // index-0 pixels left as 9
    }

    #[test]
    fn blit_clips_out_of_bounds() {
        let mut img = IndexedImage::filled(2, 2, 0);
        let tile = [1u8; 4];
        img.blit_tile(1, 1, &tile, 2, 2, false); // only top-left of tile lands
        assert_eq!(img.pixels, vec![0, 0, 0, 1]);
    }

    #[test]
    fn viewport_maps_palette_and_pads_black() {
        let mut img = IndexedImage::filled(2, 2, 1);
        img.pixels = vec![1, 2, 2, 1];
        let pal = test_palette();
        // Viewport starting at (1,1) size 2x2 -> covers one real pixel then pad.
        let rgba = viewport_rgba(&img, &pal, 1, 1, 2, 2);
        assert_eq!(rgba.width, 2);
        // top-left = source (1,1) = index 1 = red
        assert_eq!(&rgba.pixels[0..4], &[255, 0, 0, 255]);
        // top-right = source (2,1) out of bounds -> black opaque
        assert_eq!(&rgba.pixels[4..8], &[0, 0, 0, 255]);
    }
}
