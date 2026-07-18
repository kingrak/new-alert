//! Theater template files — the icon/tileset ("TMP") format used by Red Alert's
//! per-theater terrain art: `*.tem` (temperate), `*.sno` (snow), `*.int`
//! (interior), living inside `temperat.mix` / `snow.mix` / `interior.mix`.
//!
//! A template is a set of fixed-size (24×24) indexed-color *icons* (a.k.a.
//! stamps). A scenario cell names a template id plus an *icon number* within it;
//! the template's map table turns that icon number into an index into the stored
//! image list, so identical icons are deduplicated on disk.
//!
//! Two on-disk header layouts exist — the older Tiberian-Dawn one and the Red
//! Alert one — distinguished exactly as the original does (`common/stamp.cpp`,
//! `Init_Stamps`): the 32-bit field at offset `0x0C` equals `0x20` only for the
//! TD layout (there it is the `Icons` offset, which is always `0x20`); anything
//! else means the RA layout (there `0x0C` is the file size). Ported from
//! `common/stamp.cpp` (`IconControlType`, `Buffer_Draw_Stamp`).

use crate::{FormatError, Result};

/// Icon (stamp) width in pixels — always 24 in RA/TD.
pub const ICON_WIDTH: usize = 24;
/// Icon (stamp) height in pixels — always 24 in RA/TD.
pub const ICON_HEIGHT: usize = 24;

/// The sentinel value that marks a discriminating TD `Icons` offset.
const TD_TILESET_CHECK: u32 = 0x20;

fn read_u16(data: &[u8], at: usize, ctx: &'static str) -> Result<u16> {
    data.get(at..at + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or(FormatError::UnexpectedEof { context: ctx })
}

fn read_u32(data: &[u8], at: usize, ctx: &'static str) -> Result<u32> {
    data.get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or(FormatError::UnexpectedEof { context: ctx })
}

/// One decoded 24×24 icon: a borrowed slice of indexed pixels plus whether
/// palette index 0 is transparent for this icon.
#[derive(Debug, Clone, Copy)]
pub struct Icon<'a> {
    /// `width * height` palette indices, row-major.
    pub pixels: &'a [u8],
    /// When true, palette index 0 is transparent (skip it when compositing);
    /// when false the icon is fully opaque and index 0 is a real color.
    ///
    /// Mirrors the original's `TransFlagPtr[icon_index]` test in
    /// `Buffer_Draw_Stamp`.
    pub transparent: bool,
}

/// A parsed theater template (owns a copy of the file bytes so callers need not
/// keep the archive slice alive).
#[derive(Debug, Clone)]
pub struct Template {
    raw: Vec<u8>,
    width: u16,
    height: u16,
    /// Number of *logical* icons (cells) in the template — `MapWidth*MapHeight`
    /// for placed templates, or the count of random variants for clear terrain.
    count: u16,
    icon_size: usize,
    icons_off: usize,
    trans_off: usize,
    /// Offset of the icon-number → image-index map (present in every RA/TD tile
    /// file we handle). `None` would mean "index images directly".
    map_off: Option<usize>,
    /// Offset of the per-icon land/terrain-type table, RA layout only. Best
    /// effort: the field exists but its length is not self-describing.
    color_map_off: Option<usize>,
}

impl Template {
    /// Parse a template/iconset file.
    pub fn parse(data: &[u8]) -> Result<Template> {
        let width = read_u16(data, 0, "tmpl width")?;
        let height = read_u16(data, 2, "tmpl height")?;
        let count = read_u16(data, 4, "tmpl count")?;

        if width == 0 || height == 0 {
            return Err(FormatError::Invalid {
                reason: "template icon has zero dimension",
            });
        }

        // Discriminate TD vs RA by the field at 0x0C (see module docs).
        let disc = read_u32(data, 0x0C, "tmpl format discriminator")?;
        let (icons_off, trans_off, color_map_off, map_off) = if disc == TD_TILESET_CHECK {
            // TD: Icons@0x0C, Palettes@0x10, Remaps@0x14, TransFlag@0x18, Map@0x1C.
            (
                read_u32(data, 0x0C, "tmpl icons")? as usize,
                read_u32(data, 0x18, "tmpl transflag")? as usize,
                None,
                read_u32(data, 0x1C, "tmpl map")? as usize,
            )
        } else {
            // RA: Icons@0x10, Palettes@0x14, Remaps@0x18, TransFlag@0x1C,
            // ColorMap@0x20, Map@0x24.
            (
                read_u32(data, 0x10, "tmpl icons")? as usize,
                read_u32(data, 0x1C, "tmpl transflag")? as usize,
                Some(read_u32(data, 0x20, "tmpl colormap")? as usize),
                read_u32(data, 0x24, "tmpl map")? as usize,
            )
        };

        let icon_size = width as usize * height as usize;

        // Sanity: the image region must at least start inside the file.
        if icons_off > data.len() {
            return Err(FormatError::Invalid {
                reason: "template image offset past end of file",
            });
        }

        Ok(Template {
            raw: data.to_vec(),
            width,
            height,
            count,
            icon_size,
            icons_off,
            trans_off,
            color_map_off: color_map_off.filter(|&o| o != 0 && o < data.len()),
            map_off: Some(map_off).filter(|&o| o != 0 && o < data.len()),
        })
    }

    /// Icon width in pixels.
    pub fn width(&self) -> u16 {
        self.width
    }
    /// Icon height in pixels.
    pub fn height(&self) -> u16 {
        self.height
    }
    /// Number of logical icons in the template.
    pub fn count(&self) -> u16 {
        self.count
    }

    /// Resolve a logical icon number to its decoded 24×24 pixels, or `None` when
    /// the icon is empty/transparent-only (the original skips the draw when the
    /// mapped image index is not `< Count`).
    pub fn icon(&self, icon: usize) -> Option<Icon<'_>> {
        if icon >= self.count as usize {
            return None;
        }
        // icon_number -> image index via the map table (or identity).
        let image_index = match self.map_off {
            Some(mo) => *self.raw.get(mo + icon)? as usize,
            None => icon,
        };
        // The original guards `icon_index < Count`; a mapped value of 0xFF (or
        // anything >= Count) marks an empty cell.
        if image_index >= self.count as usize {
            return None;
        }
        let start = self.icons_off + image_index * self.icon_size;
        let pixels = self.raw.get(start..start + self.icon_size)?;
        let transparent = self
            .raw
            .get(self.trans_off + image_index)
            .map(|&b| b != 0)
            .unwrap_or(true);
        Some(Icon {
            pixels,
            transparent,
        })
    }

    /// Best-effort per-icon land/terrain-type bytes (RA layout only). The
    /// original stores this as `ColorMap`; its length is not self-describing, so
    /// this returns the raw tail from the field offset. Prefer the per-template
    /// land type from the template catalog for gameplay decisions.
    pub fn color_map(&self) -> Option<&[u8]> {
        self.color_map_off.map(|o| &self.raw[o..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal RA-layout template: 2 icons, 2×2 pixels, one image
    /// deduplicated (icon 1 -> image 0), the second icon empty (map = 0xFF).
    fn build_ra_template() -> Vec<u8> {
        // Header is 40 bytes (RA layout). Sections laid out after it:
        //   images  @ 40 : one 2x2 image = 4 bytes [1,2,3,4]
        //   map     @ 44 : count(3) bytes [0, 0, 0xFF]
        //   trans   @ 47 : one image => 1 byte [1] (transparent)
        //   colormap@ 48 : 1 byte [0]
        let width = 2u16;
        let height = 2u16;
        let count = 3u16;
        let icons_off = 40u32;
        let map_off = 44u32;
        let trans_off = 47u32;
        let colormap_off = 48u32;
        let size = 49u32;

        let mut out = Vec::new();
        out.extend_from_slice(&width.to_le_bytes()); // 0x00
        out.extend_from_slice(&height.to_le_bytes()); // 0x02
        out.extend_from_slice(&count.to_le_bytes()); // 0x04
        out.extend_from_slice(&0u16.to_le_bytes()); // 0x06 Allocated
        out.extend_from_slice(&1u16.to_le_bytes()); // 0x08 MapWidth
        out.extend_from_slice(&1u16.to_le_bytes()); // 0x0A MapHeight
        out.extend_from_slice(&size.to_le_bytes()); // 0x0C Size (not 0x20 -> RA)
        out.extend_from_slice(&icons_off.to_le_bytes()); // 0x10 Icons
        out.extend_from_slice(&0u32.to_le_bytes()); // 0x14 Palettes
        out.extend_from_slice(&0u32.to_le_bytes()); // 0x18 Remaps
        out.extend_from_slice(&trans_off.to_le_bytes()); // 0x1C TransFlag
        out.extend_from_slice(&colormap_off.to_le_bytes()); // 0x20 ColorMap
        out.extend_from_slice(&map_off.to_le_bytes()); // 0x24 Map
        assert_eq!(out.len(), 40);
        out.extend_from_slice(&[1, 2, 3, 4]); // image 0
        out.extend_from_slice(&[0, 0, 0xFF]); // map
        out.extend_from_slice(&[1]); // trans (transparent)
        out.extend_from_slice(&[0]); // colormap
        out
    }

    #[test]
    fn parses_ra_header() {
        let bytes = build_ra_template();
        let t = Template::parse(&bytes).unwrap();
        assert_eq!(t.width(), 2);
        assert_eq!(t.height(), 2);
        assert_eq!(t.count(), 3);
    }

    #[test]
    fn resolves_icon_via_map_and_dedup() {
        let bytes = build_ra_template();
        let t = Template::parse(&bytes).unwrap();
        let i0 = t.icon(0).unwrap();
        assert_eq!(i0.pixels, &[1, 2, 3, 4]);
        assert!(i0.transparent);
        // icon 1 maps to image 0 as well (dedup).
        assert_eq!(t.icon(1).unwrap().pixels, &[1, 2, 3, 4]);
        // icon 2 is empty (map value 0xFF >= count).
        assert!(t.icon(2).is_none());
        // out-of-range icon.
        assert!(t.icon(3).is_none());
    }

    #[test]
    fn rejects_zero_dimension() {
        let mut bytes = build_ra_template();
        bytes[0..2].copy_from_slice(&0u16.to_le_bytes());
        assert!(Template::parse(&bytes).is_err());
    }
}
