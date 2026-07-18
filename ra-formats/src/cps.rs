//! CPS images — a 320×200 indexed-color full-screen picture, LCW-compressed,
//! optionally carrying an embedded 768-byte palette. Red Alert stores
//! `PALETTE.CPS` (the house-colour remap source image) in this format.
//!
//! Header layout (`common/load.cpp`, `Load_Uncompress`):
//! `u16 file_size-2`, `u16 compression` (`4` = LCW), `u32 uncompressed_size`
//! (64000), `u16 palette_size` (`0x300` = 768 if an embedded palette follows,
//! else 0). The palette bytes (if any) come next, then the LCW image stream.

use crate::codec::lcw_decompress;
use crate::{FormatError, Result};

/// CPS image width in pixels.
pub const CPS_WIDTH: usize = 320;
/// CPS image height in pixels.
pub const CPS_HEIGHT: usize = 200;
/// Decoded CPS image size in bytes.
pub const CPS_SIZE: usize = CPS_WIDTH * CPS_HEIGHT;

/// LCW ("Format80") compression id in the CPS header.
const COMPRESSION_LCW: u16 = 4;

/// A decoded CPS image plus any embedded palette.
pub struct Cps {
    /// `320 * 200` palette indices, row-major.
    pub pixels: Vec<u8>,
    /// The embedded 6-bit VGA palette (768 bytes) if present, else empty.
    pub palette: Vec<u8>,
}

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

impl Cps {
    /// Decode a CPS file to its 320×200 indexed image.
    pub fn parse(data: &[u8]) -> Result<Cps> {
        let compression = read_u16(data, 2, "cps compression")?;
        if compression != COMPRESSION_LCW {
            return Err(FormatError::Invalid {
                reason: "unsupported CPS compression (only LCW/Format80)",
            });
        }
        let uncompressed = read_u32(data, 4, "cps uncompressed size")? as usize;
        if uncompressed != CPS_SIZE {
            return Err(FormatError::Invalid {
                reason: "CPS uncompressed size is not 320x200",
            });
        }
        let pal_size = read_u16(data, 8, "cps palette size")? as usize;

        let img_start = 10 + pal_size;
        let palette = data
            .get(10..img_start)
            .ok_or(FormatError::UnexpectedEof {
                context: "cps embedded palette",
            })?
            .to_vec();
        let src = data.get(img_start..).ok_or(FormatError::UnexpectedEof {
            context: "cps image data",
        })?;

        let mut pixels = vec![0u8; CPS_SIZE];
        lcw_decompress(src, &mut pixels);
        Ok(Cps { pixels, palette })
    }

    /// The palette index at (`x`, `y`); out-of-range coordinates return 0.
    pub fn pixel(&self, x: usize, y: usize) -> u8 {
        if x >= CPS_WIDTH || y >= CPS_HEIGHT {
            return 0;
        }
        self.pixels[y * CPS_WIDTH + x]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-build a tiny CPS: header + no palette + an LCW stream that fills the
    /// 64000-byte image with a known pattern, then verify the pixel accessor.
    #[test]
    fn decodes_header_and_pixels() {
        // Build the plaintext image: value = (x+y) & 0xFF via a simple scheme is
        // hard through LCW literals, so just fill with a constant we can check.
        let mut plain = vec![7u8; CPS_SIZE];
        plain[0] = 3;
        plain[CPS_WIDTH] = 9; // (0,1)

        // Emit as LCW medium copy-from-source runs (<=63 bytes each), end 0x80.
        let mut lcw = Vec::new();
        for chunk in plain.chunks(63) {
            lcw.push(0x80 | chunk.len() as u8);
            lcw.extend_from_slice(chunk);
        }
        lcw.push(0x80);

        let mut file = Vec::new();
        file.extend_from_slice(&((lcw.len() + 8) as u16).to_le_bytes()); // file size-2 (unused)
        file.extend_from_slice(&COMPRESSION_LCW.to_le_bytes());
        file.extend_from_slice(&(CPS_SIZE as u32).to_le_bytes());
        file.extend_from_slice(&0u16.to_le_bytes()); // no palette
        file.extend_from_slice(&lcw);

        let cps = Cps::parse(&file).unwrap();
        assert_eq!(cps.pixel(0, 0), 3);
        assert_eq!(cps.pixel(0, 1), 9);
        assert_eq!(cps.pixel(5, 5), 7);
        assert!(cps.palette.is_empty());
    }

    #[test]
    fn rejects_bad_compression() {
        let mut file = vec![0u8; 10];
        file[2] = 1; // not LCW
        assert!(Cps::parse(&file).is_err());
    }
}
