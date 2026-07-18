//! Format-agnostic "pack" decoding used by RA scenario INI blocks
//! (`[MapPack]`, `[OverlayPack]`, `[Digest]`): a base64 text payload wrapping a
//! chunked LCW (Format80) stream.
//!
//! Two independent steps, kept here because neither needs any game knowledge:
//!
//! 1. [`decode_base64`] — standard-alphabet base64 decode. The original streams
//!    the numbered INI lines through `Base64Pipe` (`common/base64.cpp`); any
//!    byte outside the alphabet is skipped, and `'='` ends the stream.
//! 2. [`decompress_pack`] — the chunked LCW container. After base64-decoding, the
//!    bytes are a sequence of chunks, each a 4-byte header
//!    `[u16 comp_size][u16 uncomp_size]` (both little-endian) followed by
//!    `comp_size` bytes of LCW data. Ported from `common/lcwstraw.cpp`
//!    (`LCWStraw::Get`, `BlockHeader`).

use crate::codec::lcw_decompress;

/// Standard base64 alphabet (RFC 4648), matching `common/base64.cpp`.
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Decode base64 text into bytes. Non-alphabet bytes (whitespace, newlines) are
/// skipped; `'='` padding ends a quantum. Never panics.
pub fn decode_base64(text: &[u8]) -> Vec<u8> {
    // Reverse lookup: byte -> 0..=63, or 0xFF for "not an alphabet character".
    let mut rev = [0xFFu8; 256];
    for (i, &c) in ALPHABET.iter().enumerate() {
        rev[c as usize] = i as u8;
    }

    let mut out = Vec::with_capacity(text.len() * 3 / 4 + 3);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for &b in text {
        if b == b'=' {
            break;
        }
        let v = rev[b as usize];
        if v == 0xFF {
            continue; // skip whitespace / stray bytes
        }
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

/// Decompress a chunked-LCW pack (already base64-decoded) into a flat buffer.
///
/// Reads `[u16 comp][u16 uncomp]` chunk headers and LCW-decompresses each chunk,
/// concatenating the results, until the input is exhausted or a header is
/// truncated. Bounds-checked; never panics on malformed input.
pub fn decompress_pack(mut data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while data.len() >= 4 {
        let comp = u16::from_le_bytes([data[0], data[1]]) as usize;
        let uncomp = u16::from_le_bytes([data[2], data[3]]) as usize;
        let body = &data[4..];
        if comp == 0 || body.len() < comp {
            break; // truncated / terminal chunk
        }
        let mut chunk = vec![0u8; uncomp];
        lcw_decompress(&body[..comp], &mut chunk);
        out.extend_from_slice(&chunk);
        data = &body[comp..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::lcw_decompress; // ensure symmetry reference

    #[test]
    fn base64_basic() {
        // "Man" -> "TWFu"; "Ma" -> "TWE="; "M" -> "TQ=="
        assert_eq!(decode_base64(b"TWFu"), b"Man");
        assert_eq!(decode_base64(b"TWE="), b"Ma");
        assert_eq!(decode_base64(b"TQ=="), b"M");
    }

    #[test]
    fn base64_skips_whitespace() {
        assert_eq!(decode_base64(b"T W\nF\r u"), b"Man");
    }

    #[test]
    fn base64_empty() {
        assert_eq!(decode_base64(b""), Vec::<u8>::new());
        assert_eq!(decode_base64(b"====").len(), 0);
    }

    /// Round-trip a small pack: build one LCW chunk by hand and wrap it in a
    /// chunk header, then decompress. The LCW stream `[0x84,'A','B','C','D',0x80]`
    /// decodes to "ABCD" (see `codec` tests).
    #[test]
    fn pack_single_chunk() {
        let lcw = [0x84u8, b'A', b'B', b'C', b'D', 0x80];
        // Sanity: the codec really produces "ABCD".
        let mut check = [0u8; 4];
        assert_eq!(lcw_decompress(&lcw, &mut check), 4);

        let mut pack = Vec::new();
        pack.extend_from_slice(&(lcw.len() as u16).to_le_bytes()); // comp
        pack.extend_from_slice(&4u16.to_le_bytes()); // uncomp
        pack.extend_from_slice(&lcw);
        assert_eq!(decompress_pack(&pack), b"ABCD");
    }

    #[test]
    fn pack_two_chunks_concatenate() {
        let lcw = [0x84u8, b'A', b'B', b'C', b'D', 0x80];
        let mut pack = Vec::new();
        for _ in 0..2 {
            pack.extend_from_slice(&(lcw.len() as u16).to_le_bytes());
            pack.extend_from_slice(&4u16.to_le_bytes());
            pack.extend_from_slice(&lcw);
        }
        assert_eq!(decompress_pack(&pack), b"ABCDABCD");
    }

    #[test]
    fn pack_truncated_header_stops() {
        // Only 2 bytes: not a full header -> empty, no panic.
        assert_eq!(decompress_pack(&[1, 0]), Vec::<u8>::new());
    }

    #[test]
    fn pack_truncated_body_stops() {
        // Header claims 10 comp bytes but only 2 present -> stop cleanly.
        let mut pack = Vec::new();
        pack.extend_from_slice(&10u16.to_le_bytes());
        pack.extend_from_slice(&4u16.to_le_bytes());
        pack.extend_from_slice(&[0x84, b'A']);
        assert_eq!(decompress_pack(&pack), Vec::<u8>::new());
    }

    #[test]
    fn base64_all_invalid_bytes_yield_empty() {
        // Every byte outside the alphabet (and not '=') is skipped; with no
        // valid sextets accumulated, output is empty.
        assert_eq!(decode_base64(b"!!!***###"), Vec::<u8>::new());
    }

    #[test]
    fn base64_padding_mid_stream_ends_decode() {
        // A '=' terminates the stream even if more alphabet bytes follow —
        // matches `Base64Pipe`'s "stop at first padding" behavior.
        assert_eq!(decode_base64(b"TWE=TWFu"), b"Ma");
    }

    #[test]
    fn base64_lone_equals_is_empty() {
        assert_eq!(decode_base64(b"="), Vec::<u8>::new());
    }

    #[test]
    fn base64_single_invalid_char_between_valid_ones_is_skipped() {
        // A stray non-alphabet byte mid-quantum (e.g. a control character
        // slipped into an INI line) is simply skipped, not treated as an error.
        assert_eq!(decode_base64(b"T\x01WFu"), b"Man");
    }

    #[test]
    fn pack_zero_comp_size_stops_immediately() {
        // A chunk header with comp_size == 0 is treated as a terminal marker
        // (never a valid LCW chunk is zero bytes), so decoding stops with
        // whatever was already accumulated.
        let mut pack = Vec::new();
        pack.extend_from_slice(&0u16.to_le_bytes()); // comp = 0
        pack.extend_from_slice(&4u16.to_le_bytes()); // uncomp
                                                     // Trailing bytes after the zero-comp header must never be consulted.
        pack.extend_from_slice(&[0xAA; 8]);
        assert_eq!(decompress_pack(&pack), Vec::<u8>::new());
    }

    #[test]
    fn pack_second_chunk_truncated_keeps_first() {
        // First chunk decodes fully; the header for a second chunk is present
        // but its body is short -> stop after the first chunk's output.
        let lcw = [0x84u8, b'A', b'B', b'C', b'D', 0x80];
        let mut pack = Vec::new();
        pack.extend_from_slice(&(lcw.len() as u16).to_le_bytes());
        pack.extend_from_slice(&4u16.to_le_bytes());
        pack.extend_from_slice(&lcw);
        // Second chunk header claims 100 bytes of body; none follow.
        pack.extend_from_slice(&100u16.to_le_bytes());
        pack.extend_from_slice(&4u16.to_le_bytes());
        assert_eq!(decompress_pack(&pack), b"ABCD");
    }

    #[test]
    fn pack_uncomp_shorter_than_lcw_output_truncates_to_uncomp() {
        // The chunk header's uncomp size is authoritative for the output
        // buffer even when the LCW stream could produce more: decompress into
        // exactly `uncomp` bytes.
        let lcw = [0x84u8, b'A', b'B', b'C', b'D', 0x80];
        let mut pack = Vec::new();
        pack.extend_from_slice(&(lcw.len() as u16).to_le_bytes());
        pack.extend_from_slice(&2u16.to_le_bytes()); // uncomp shorter than "ABCD"
        pack.extend_from_slice(&lcw);
        assert_eq!(decompress_pack(&pack), b"AB");
    }

    #[test]
    fn pack_empty_input_is_empty() {
        assert_eq!(decompress_pack(&[]), Vec::<u8>::new());
    }
}
