//! PAL palettes: 256 RGB triplets stored as 6-bit VGA DAC values (0..=63),
//! expanded here to full 8-bit range.
//!
//! A `.pal` file is exactly 768 bytes (256 × 3). The original hardware used the
//! top 6 bits of each DAC register, so each stored byte is 0..=63. We expand
//! with `(v << 2) | (v >> 4)` so that 63 maps to 255 (full white) rather than
//! 252 — this is display-oriented; the game itself just shifts left by 2.

use crate::{FormatError, Result};

/// Number of colors in a palette.
pub const COLORS: usize = 256;
/// On-disk size of a `.pal` file.
pub const PAL_BYTES: usize = COLORS * 3;

/// A decoded 8-bit RGB palette.
#[derive(Clone)]
pub struct Palette {
    /// 256 `[r, g, b]` entries, each channel expanded to 0..=255.
    pub colors: [[u8; 3]; COLORS],
}

impl Palette {
    /// Parse a 768-byte 6-bit palette, expanding each channel to 8-bit.
    pub fn parse(data: &[u8]) -> Result<Palette> {
        if data.len() < PAL_BYTES {
            return Err(FormatError::UnexpectedEof {
                context: "palette (need 768 bytes)",
            });
        }
        let mut colors = [[0u8; 3]; COLORS];
        for (i, color) in colors.iter_mut().enumerate() {
            for (c, channel) in color.iter_mut().enumerate() {
                let v = data[i * 3 + c] & 0x3f; // 6-bit
                *channel = (v << 2) | (v >> 4);
            }
        }
        Ok(Palette { colors })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_six_bit() {
        let mut data = vec![0u8; PAL_BYTES];
        data[0] = 63; // full red DAC value
        let pal = Palette::parse(&data).unwrap();
        assert_eq!(pal.colors[0], [255, 0, 0]);
    }

    #[test]
    fn rejects_short() {
        assert!(Palette::parse(&[0u8; 10]).is_err());
    }
}
