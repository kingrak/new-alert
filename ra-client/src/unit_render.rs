//! Unit sprite decoding and compositing — the presentation half of a unit
//! (DESIGN.md §3.9, §4.5). SHP frames are decoded to indexed pixels once; at
//! draw time each index passes through the owning house's remap LUT and then
//! the palette, so house colours are byte-identical to the original's blit-time
//! remap. The sim is never touched here — this reads a position and facing and
//! nothing else.

use ra_formats::shp::Shp;
use ra_sim::coords::{dir_to_32, Facing};

use crate::compositor::{Palette, RgbaImage};
use ra_data::house::RemapTable;

/// One decoded sprite frame: indexed pixels plus dimensions.
#[derive(Clone, Debug)]
pub struct SpriteFrame {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// `width * height` palette indices, row-major (index 0 = transparent).
    pub pixels: Vec<u8>,
}

/// A unit type's decoded body frames (32 rotation frames for a vehicle).
#[derive(Clone, Debug)]
pub struct UnitSprite {
    /// All decoded frames in file order.
    pub frames: Vec<SpriteFrame>,
}

impl UnitSprite {
    /// Decode every frame of a unit SHP.
    pub fn from_shp_bytes(bytes: &[u8]) -> Result<UnitSprite, ra_formats::FormatError> {
        let shp = Shp::parse(bytes)?;
        let frames = shp
            .decode_all()?
            .into_iter()
            .map(|f| SpriteFrame {
                width: f.width as u32,
                height: f.height as u32,
                pixels: f.pixels,
            })
            .collect();
        Ok(UnitSprite { frames })
    }

    /// Number of frames.
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// The body frame index for a given facing. Port of the vehicle case of
    /// `UnitClass::Shape_Number`: `shapenum = BodyShape[Dir_To_32(facing)]`,
    /// where `BodyShape[i] = (32 - i) mod 32` (`techno.cpp:220`). Falls back to
    /// modulo when a sprite has fewer than 32 frames.
    pub fn body_frame(&self, facing: Facing) -> usize {
        let i = dir_to_32(facing) as usize;
        let shapenum = (32 - i) % 32;
        if self.frames.is_empty() {
            0
        } else if shapenum < self.frames.len() {
            shapenum
        } else {
            shapenum % self.frames.len()
        }
    }

    /// The frame to draw for `facing`, or `None` if the sprite has no frames.
    pub fn frame_for(&self, facing: Facing) -> Option<&SpriteFrame> {
        self.frames.get(self.body_frame(facing))
    }

    /// Whether this sprite carries a separate turret (≥ 64 frames: 32 body +
    /// 32 turret, e.g. the 2TNK). Turretless vehicle SHPs have 32 frames.
    pub fn has_turret_frames(&self) -> bool {
        self.frames.len() >= 64
    }

    /// The turret frame index for a given turret facing. Port of the turret
    /// case of `UnitClass::Draw_It` (`unit.cpp:2174`): `shapenum =
    /// BodyShape[Dir_To_32(turret_facing)] + 32`, i.e. the body remap plus the
    /// 32-frame turret block. Returns `None` if the sprite has no turret frames.
    pub fn turret_frame_for(&self, turret_facing: Facing) -> Option<&SpriteFrame> {
        if !self.has_turret_frames() {
            return None;
        }
        let i = dir_to_32(turret_facing) as usize;
        let shapenum = (32 - i) % 32 + 32;
        self.frames.get(shapenum)
    }
}

/// Blit an indexed sprite frame onto an RGBA image, its **centre** at
/// (`cx`, `cy`) in destination pixels. Index 0 is transparent; every other
/// index is remapped through `remap` then expanded through `palette`.
pub fn draw_sprite_centered(
    dst: &mut RgbaImage,
    cx: i32,
    cy: i32,
    frame: &SpriteFrame,
    remap: &RemapTable,
    palette: &Palette,
) {
    let top = cx - (frame.width as i32) / 2;
    let left = cy - (frame.height as i32) / 2;
    for sy in 0..frame.height as i32 {
        let py = left + sy;
        if py < 0 || py >= dst.height as i32 {
            continue;
        }
        for sx in 0..frame.width as i32 {
            let px = top + sx;
            if px < 0 || px >= dst.width as i32 {
                continue;
            }
            let idx = frame.pixels[(sy as u32 * frame.width + sx as u32) as usize];
            if idx == 0 {
                continue; // transparent
            }
            let [r, g, b] = palette[remap[idx as usize] as usize];
            let di = ((py as u32 * dst.width + px as u32) * 4) as usize;
            dst.pixels[di] = r;
            dst.pixels[di + 1] = g;
            dst.pixels[di + 2] = b;
            dst.pixels[di + 3] = 255;
        }
    }
}

/// Blit an indexed sprite frame with its **top-left** at (`x`, `y`) in
/// destination pixels — the anchoring buildings use (their SHP art aligns to the
/// footprint's upper-left cell). Index 0 is transparent; other indices are
/// remapped then palette-expanded.
pub fn draw_sprite_topleft(
    dst: &mut RgbaImage,
    x: i32,
    y: i32,
    frame: &SpriteFrame,
    remap: &RemapTable,
    palette: &Palette,
) {
    for sy in 0..frame.height as i32 {
        let py = y + sy;
        if py < 0 || py >= dst.height as i32 {
            continue;
        }
        for sx in 0..frame.width as i32 {
            let px = x + sx;
            if px < 0 || px >= dst.width as i32 {
                continue;
            }
            let idx = frame.pixels[(sy as u32 * frame.width + sx as u32) as usize];
            if idx == 0 {
                continue;
            }
            let [r, g, b] = palette[remap[idx as usize] as usize];
            let di = ((py as u32 * dst.width + px as u32) * 4) as usize;
            dst.pixels[di] = r;
            dst.pixels[di + 1] = g;
            dst.pixels[di + 2] = b;
            dst.pixels[di + 3] = 255;
        }
    }
}

/// Draw a filled rectangle in `[r, g, b]`, clipped to the image. Used for
/// health bars and muzzle flashes.
pub fn fill_rect(dst: &mut RgbaImage, x0: i32, y0: i32, x1: i32, y1: i32, rgb: [u8; 3]) {
    let (xa, xb) = (x0.min(x1).max(0), x0.max(x1).min(dst.width as i32 - 1));
    let (ya, yb) = (y0.min(y1).max(0), y0.max(y1).min(dst.height as i32 - 1));
    for y in ya..=yb {
        for x in xa..=xb {
            let di = ((y as u32 * dst.width + x as u32) * 4) as usize;
            dst.pixels[di] = rgb[0];
            dst.pixels[di + 1] = rgb[1];
            dst.pixels[di + 2] = rgb[2];
            dst.pixels[di + 3] = 255;
        }
    }
}

/// Draw a unit health bar centred at `cx`, sitting `above` pixels over the unit
/// centre. `frac` is health/maxhealth in the range 0..=1000 (integer permille,
/// so no float enters presentation state needlessly). Classic RA colouring:
/// green > 50%, yellow > 25%, red below. Width is `CELL` pixels.
pub fn draw_health_bar(dst: &mut RgbaImage, cx: i32, cy_top: i32, width: i32, frac_permille: i32) {
    let frac = frac_permille.clamp(0, 1000);
    let w = width.max(4);
    let x0 = cx - w / 2;
    let x1 = x0 + w;
    let y0 = cy_top;
    let y1 = cy_top + 2;
    // Dark backing.
    fill_rect(dst, x0 - 1, y0 - 1, x1 + 1, y1 + 1, [0, 0, 0]);
    // Filled portion.
    let filled = x0 + (w * frac / 1000);
    let color = if frac > 500 {
        [0, 200, 0]
    } else if frac > 250 {
        [220, 200, 0]
    } else {
        [220, 0, 0]
    };
    if filled > x0 {
        fill_rect(dst, x0, y0, filled, y1, color);
    }
}

/// Draw a 1-pixel-thick rectangle outline in `[r, g, b]` on an RGBA image.
/// Used for selection markers and the drag-select box. Coordinates are in
/// destination pixels; the rectangle is clipped to the image.
pub fn draw_rect_outline(dst: &mut RgbaImage, x0: i32, y0: i32, x1: i32, y1: i32, rgb: [u8; 3]) {
    let (xa, xb) = (x0.min(x1), x0.max(x1));
    let (ya, yb) = (y0.min(y1), y0.max(y1));
    let mut put = |x: i32, y: i32| {
        if x >= 0 && y >= 0 && (x as u32) < dst.width && (y as u32) < dst.height {
            let di = ((y as u32 * dst.width + x as u32) * 4) as usize;
            dst.pixels[di] = rgb[0];
            dst.pixels[di + 1] = rgb[1];
            dst.pixels[di + 2] = rgb[2];
            dst.pixels[di + 3] = 255;
        }
    };
    for x in xa..=xb {
        put(x, ya);
        put(x, yb);
    }
    for y in ya..=yb {
        put(xa, y);
        put(xb, y);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_frame(w: u32, h: u32, idx: u8) -> SpriteFrame {
        SpriteFrame {
            width: w,
            height: h,
            pixels: vec![idx; (w * h) as usize],
        }
    }

    fn palette() -> Palette {
        let mut p = [[0u8; 3]; 256];
        p[5] = [10, 20, 30];
        p[9] = [90, 90, 90];
        p
    }

    #[test]
    fn body_frame_maps_facings() {
        let sprite = UnitSprite {
            frames: (0..32).map(|_| solid_frame(1, 1, 1)).collect(),
        };
        assert_eq!(sprite.body_frame(Facing(0)), 0); // north
                                                     // East (dir 64 -> dir32 8) -> (32-8)%32 = 24.
        assert_eq!(sprite.body_frame(Facing(64)), 24);
    }

    #[test]
    fn draw_applies_remap_and_transparency() {
        let mut dst = RgbaImage {
            width: 4,
            height: 4,
            pixels: vec![0u8; 4 * 4 * 4],
        };
        // 2x2 frame: index 5 everywhere except a transparent index-0 corner.
        let mut frame = solid_frame(2, 2, 5);
        frame.pixels[0] = 0;
        // Remap 5 -> 9.
        let mut remap = [0u8; 256];
        for (i, e) in remap.iter_mut().enumerate() {
            *e = i as u8;
        }
        remap[5] = 9;
        draw_sprite_centered(&mut dst, 1, 1, &frame, &remap, &palette());
        // The frame centre (1,1) puts its top-left at (0,0). (0,0) was
        // transparent, so stays black; (1,0) is index 5 -> remap 9 -> [90,90,90].
        assert_eq!(&dst.pixels[0..4], &[0, 0, 0, 0]); // untouched transparent
        assert_eq!(&dst.pixels[4..8], &[90, 90, 90, 255]);
    }

    #[test]
    fn rect_outline_draws_border_only() {
        let mut dst = RgbaImage {
            width: 5,
            height: 5,
            pixels: vec![0u8; 5 * 5 * 4],
        };
        draw_rect_outline(&mut dst, 1, 1, 3, 3, [1, 2, 3]);
        // Corner on the border is set: pixel (1,1) = index 6.
        let corner = (6 * 4) as usize;
        assert_eq!(&dst.pixels[corner..corner + 4], &[1, 2, 3, 255]);
        // Centre (2,2) is inside, not on the border -> untouched.
        let center = ((2 * 5 + 2) * 4) as usize;
        assert_eq!(&dst.pixels[center..center + 4], &[0, 0, 0, 0]);
    }
}
