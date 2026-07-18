//! SHP shape files — the RA/TD indexed-color sprite format used for units,
//! infantry, buildings, and effects. Every frame shares one width/height; each
//! frame is stored in one of three encodings selected by a per-frame format
//! byte:
//!
//! - **0x80 — Format80 (LCW)**: a full keyframe, LCW-compressed.
//! - **0x40 — Format40 (XOR-delta vs a keyframe)**: XOR-delta applied on top of
//!   an earlier keyframe identified by a reference data offset.
//! - **0x20 — Format40 (XOR-delta vs previous frame)**: XOR-delta applied on top
//!   of the immediately preceding frame.
//!
//! Because deltas reference earlier frames, decoding frame *N* decodes frames
//! `0..=N` in order. Ported from `common/keyframe.cpp` (`Build_Frame`).

use crate::codec::{apply_xor_delta, lcw_decompress};
use crate::{FormatError, Result};

const HEADER_LEN: usize = 14;
const FRAME_ENTRY_LEN: usize = 8;

const FMT_KEYFRAME: u8 = 0x80; // Format80 / LCW full frame
const FMT_XOR_KEY: u8 = 0x40; // Format40 XOR vs referenced keyframe
const FMT_XOR_PREV: u8 = 0x20; // Format40 XOR vs previous frame

/// Upper bound on a single frame's pixel count (`width * height`), enforced at
/// parse time. Real RA/TD shapes top out in the low tens of thousands of pixels
/// (the largest buildings are a few hundred px per side); this 4M-pixel cap is
/// far above any genuine asset while turning a corrupt/hostile header — where
/// `width` and `height` are arbitrary `u16`s whose product can reach ~4.3
/// billion — into a clean parse error instead of a multi-gigabyte allocation
/// per decoded frame. Requested by ra-tester as a structural hardening.
const MAX_FRAME_PIXELS: usize = 4 * 1024 * 1024;

/// The parsed SHP file header.
#[derive(Debug, Clone, Copy)]
pub struct ShpHeader {
    /// Number of frames in the file.
    pub frame_count: u16,
    /// Frame width in pixels (shared by all frames).
    pub width: u16,
    /// Frame height in pixels (shared by all frames).
    pub height: u16,
    /// Header flags; bit 0 set means a 768-byte palette precedes the frame data.
    pub flags: u16,
}

#[derive(Clone, Copy)]
struct FrameInfo {
    offset: u32,     // data offset (low 24 bits of word 0)
    format: u8,      // format byte (high 8 bits of word 0)
    ref_offset: u32, // reference data offset (low 24 bits of word 1)
}

/// A decoded indexed-color frame.
#[derive(Clone)]
pub struct Frame {
    /// Frame width in pixels.
    pub width: u16,
    /// Frame height in pixels.
    pub height: u16,
    /// `width * height` palette indices, row-major.
    pub pixels: Vec<u8>,
}

/// A parsed SHP file borrowing the underlying bytes.
pub struct Shp<'a> {
    data: &'a [u8],
    header: ShpHeader,
    frames: Vec<FrameInfo>,
    /// Offset added to every frame data offset when a palette is prepended.
    pal_shift: usize,
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

impl<'a> Shp<'a> {
    /// Parse the SHP header and frame table.
    pub fn parse(data: &'a [u8]) -> Result<Shp<'a>> {
        let frame_count = read_u16(data, 0, "shp frame count")?;
        let width = read_u16(data, 6, "shp width")?;
        let height = read_u16(data, 8, "shp height")?;
        let flags = read_u16(data, 12, "shp flags")?;

        if width == 0 || height == 0 {
            return Err(FormatError::Invalid {
                reason: "shp frame has zero dimension",
            });
        }

        // Reject absurd dimensions up front so a decoded frame can never demand
        // a runaway allocation (see MAX_FRAME_PIXELS).
        if (width as usize) * (height as usize) > MAX_FRAME_PIXELS {
            return Err(FormatError::Invalid {
                reason: "shp frame dimensions exceed the maximum pixel cap",
            });
        }

        let mut frames = Vec::with_capacity(frame_count as usize);
        for i in 0..frame_count as usize {
            let base = HEADER_LEN + i * FRAME_ENTRY_LEN;
            let word0 = read_u32(data, base, "shp frame entry")?;
            let word1 = read_u32(data, base + 4, "shp frame ref")?;
            frames.push(FrameInfo {
                offset: word0 & 0x00ff_ffff,
                format: (word0 >> 24) as u8,
                ref_offset: word1 & 0x00ff_ffff,
            });
        }

        let pal_shift = if flags & 1 != 0 { 768 } else { 0 };

        Ok(Shp {
            data,
            header: ShpHeader {
                frame_count,
                width,
                height,
                flags,
            },
            frames,
            pal_shift,
        })
    }

    /// The parsed header.
    pub fn header(&self) -> ShpHeader {
        self.header
    }

    /// Number of frames.
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    fn frame_bytes(&self, offset: u32) -> Result<&'a [u8]> {
        let start = self.pal_shift + offset as usize;
        self.data.get(start..).ok_or(FormatError::UnexpectedEof {
            context: "shp frame data",
        })
    }

    /// Decode a single frame (decoding all preceding frames it depends on).
    pub fn decode_frame(&self, index: usize) -> Result<Frame> {
        Ok(self.decode_upto(index)?.pop().expect("frame present"))
    }

    /// Decode every frame in the file.
    pub fn decode_all(&self) -> Result<Vec<Frame>> {
        if self.frames.is_empty() {
            return Ok(Vec::new());
        }
        self.decode_upto(self.frames.len() - 1)
    }

    /// Decode frames `0..=index`, returning all of them (deltas need the chain).
    fn decode_upto(&self, index: usize) -> Result<Vec<Frame>> {
        if index >= self.frames.len() {
            return Err(FormatError::Invalid {
                reason: "shp frame index out of range",
            });
        }
        let w = self.header.width as usize;
        let h = self.header.height as usize;
        let size = w * h;

        let mut decoded: Vec<Vec<u8>> = Vec::with_capacity(index + 1);
        for i in 0..=index {
            let info = self.frames[i];
            let mut buf = vec![0u8; size];

            match info.format {
                FMT_KEYFRAME => {
                    let src = self.frame_bytes(info.offset)?;
                    lcw_decompress(src, &mut buf);
                }
                FMT_XOR_KEY => {
                    // Reference is an earlier frame identified by its data offset
                    // (normally the base keyframe, frame 0).
                    let base = decoded
                        .iter()
                        .enumerate()
                        .find(|(j, _)| self.frames[*j].offset == info.ref_offset)
                        .map(|(_, b)| b.clone())
                        .unwrap_or_else(|| {
                            decoded.last().cloned().unwrap_or_else(|| vec![0u8; size])
                        });
                    buf.copy_from_slice(&base);
                    let delta = self.frame_bytes(info.offset)?;
                    apply_xor_delta(&mut buf, delta);
                }
                FMT_XOR_PREV => {
                    if let Some(prev) = decoded.last() {
                        buf.copy_from_slice(prev);
                    }
                    let delta = self.frame_bytes(info.offset)?;
                    apply_xor_delta(&mut buf, delta);
                }
                _ => {
                    return Err(FormatError::Invalid {
                        reason: "unknown shp frame format",
                    });
                }
            }
            decoded.push(buf);
        }

        Ok(decoded
            .into_iter()
            .map(|pixels| Frame {
                width: self.header.width,
                height: self.header.height,
                pixels,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a tiny hand-crafted SHP file (no dependency on real assets):
    /// 2 frames, 2x2 pixels each.
    ///
    /// Layout:
    ///   header (14 bytes)
    ///   frame table (2 * 8 = 16 bytes), spanning bytes 14..30
    ///   frame 0 data at byte 30: FMT_KEYFRAME (LCW), literal pixels [1,2,3,4]
    ///   frame 1 data at byte 36: FMT_XOR_PREV, xor-delta vs frame 0
    ///
    /// Frame 0's LCW stream is `[0x84, 1, 2, 3, 4, 0x80]`: a medium
    /// copy-from-source of 4 literal bytes (see `codec.rs` tests for the
    /// opcode layout), producing pixels [1, 2, 3, 4].
    ///
    /// Frame 1's XOR-delta is `[0x83, 0x01, 0x0D]`: skip 3 bytes (cmd=0x83),
    /// then xor 1 source byte (cmd=0x01) with 0x0D onto the copied-forward
    /// frame-0 buffer, flipping the last pixel from 4 to `4 ^ 0x0D == 9`.
    fn build_two_frame_shp() -> Vec<u8> {
        let mut out = vec![0u8; 14];
        out[0..2].copy_from_slice(&2u16.to_le_bytes()); // frame_count
        out[6..8].copy_from_slice(&2u16.to_le_bytes()); // width
        out[8..10].copy_from_slice(&2u16.to_le_bytes()); // height
        out[12..14].copy_from_slice(&0u16.to_le_bytes()); // flags (no palette)

        // Frame table: entry 0 (keyframe at offset 30), entry 1 (xor-prev at
        // offset 36).
        let word0_frame0: u32 = 30 | (0x80 << 24);
        let word0_frame1: u32 = 36 | (0x20 << 24);
        out.extend_from_slice(&word0_frame0.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // ref_offset, unused
        out.extend_from_slice(&word0_frame1.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // ref_offset, unused

        assert_eq!(out.len(), 30, "frame 0 must start at byte 30");
        out.extend_from_slice(&[0x84, 1, 2, 3, 4, 0x80]); // frame 0: LCW keyframe
        assert_eq!(out.len(), 36, "frame 1 must start at byte 36");
        out.extend_from_slice(&[0x83, 0x01, 0x0D]); // frame 1: xor-delta

        out
    }

    #[test]
    fn parses_header() {
        let bytes = build_two_frame_shp();
        let shp = Shp::parse(&bytes).unwrap();
        let hdr = shp.header();
        assert_eq!(hdr.frame_count, 2);
        assert_eq!(hdr.width, 2);
        assert_eq!(hdr.height, 2);
        assert_eq!(hdr.flags, 0);
        assert_eq!(shp.frame_count(), 2);
    }

    #[test]
    fn decodes_keyframe() {
        let bytes = build_two_frame_shp();
        let shp = Shp::parse(&bytes).unwrap();
        let f0 = shp.decode_frame(0).unwrap();
        assert_eq!(f0.pixels, vec![1, 2, 3, 4]);
    }

    #[test]
    fn decodes_xor_prev_chain() {
        let bytes = build_two_frame_shp();
        let shp = Shp::parse(&bytes).unwrap();
        let f1 = shp.decode_frame(1).unwrap();
        // frame 0 was [1,2,3,4]; the delta flips only the last pixel to
        // 4 ^ 0x0D == 9.
        assert_eq!(f1.pixels, vec![1, 2, 3, 9]);
    }

    #[test]
    fn decode_all_matches_individual_decodes() {
        let bytes = build_two_frame_shp();
        let shp = Shp::parse(&bytes).unwrap();
        let all = shp.decode_all().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].pixels, shp.decode_frame(0).unwrap().pixels);
        assert_eq!(all[1].pixels, shp.decode_frame(1).unwrap().pixels);
    }

    /// Determinism guard: decoding the same frame twice must yield identical
    /// buffers. This pins the API contract independent of any real asset
    /// (see `tests/golden_assets.rs` for the same guard against a real SHP).
    #[test]
    fn decode_is_deterministic() {
        let bytes = build_two_frame_shp();
        let shp = Shp::parse(&bytes).unwrap();
        let first = shp.decode_frame(1).unwrap();
        let second = shp.decode_frame(1).unwrap();
        assert_eq!(first.pixels, second.pixels);

        let all_a = shp.decode_all().unwrap();
        let all_b = shp.decode_all().unwrap();
        for (a, b) in all_a.iter().zip(all_b.iter()) {
            assert_eq!(a.pixels, b.pixels);
        }
    }

    #[test]
    fn rejects_oversized_dimensions() {
        // width = height = 0xFFFF -> ~4.29e9 pixels, well over the cap.
        let mut bytes = build_two_frame_shp();
        bytes[6..8].copy_from_slice(&0xFFFFu16.to_le_bytes());
        bytes[8..10].copy_from_slice(&0xFFFFu16.to_le_bytes());
        assert!(Shp::parse(&bytes).is_err());
    }

    #[test]
    fn rejects_zero_dimension() {
        let mut bytes = build_two_frame_shp();
        bytes[6..8].copy_from_slice(&0u16.to_le_bytes()); // width = 0
        assert!(Shp::parse(&bytes).is_err());
    }

    #[test]
    fn out_of_range_index_is_error_not_panic() {
        let bytes = build_two_frame_shp();
        let shp = Shp::parse(&bytes).unwrap();
        assert!(shp.decode_frame(2).is_err());
        assert!(shp.decode_frame(usize::MAX).is_err());
    }
}
