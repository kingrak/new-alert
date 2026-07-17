//! The two Westwood byte codecs used by SHP (and many other RA formats):
//!
//! - **Format80 / LCW** — a general LZ-style compressor. See `common/lcw.cpp`.
//! - **Format40 / XOR-delta** — a frame is stored as a run-length XOR against a
//!   reference frame already sitting in the destination buffer. See
//!   `common/xordelta.cpp` (`Apply_XOR_Delta`).
//!
//! Both decoders are bounds-checked and never panic on malformed input, so they
//! are safe to fuzz over arbitrary bytes.

/// A tiny forward byte reader that yields `None` at end of input instead of
/// panicking.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }
    #[inline]
    fn u8(&mut self) -> Option<u8> {
        let b = self.data.get(self.pos).copied();
        if b.is_some() {
            self.pos += 1;
        }
        b
    }
    #[inline]
    fn u16le(&mut self) -> Option<u16> {
        let lo = self.u8()? as u16;
        let hi = self.u8()? as u16;
        Some(lo | (hi << 8))
    }
}

/// Decompress a Format80 (LCW) stream into `out`, returning the number of bytes
/// written. Stops at `out.len()`, at the 0x80 end marker, or when the input is
/// exhausted.
pub fn lcw_decompress(src: &[u8], out: &mut [u8]) -> usize {
    let mut r = Reader::new(src);
    let mut dest = 0usize;
    let end = out.len();

    while dest < end {
        let op = match r.u8() {
            Some(v) => v,
            None => break,
        };

        if op & 0x80 == 0 {
            // Short copy from earlier in the output: back-reference.
            let count = ((op >> 4) as usize) + 3;
            let lo = match r.u8() {
                Some(v) => v as usize,
                None => break,
            };
            let rel = lo + (((op & 0x0f) as usize) << 8);
            if rel == 0 || rel > dest {
                break; // corrupt back-reference
            }
            let mut src_idx = dest - rel;
            let n = count.min(end - dest);
            for _ in 0..n {
                out[dest] = out[src_idx];
                dest += 1;
                src_idx += 1;
            }
        } else if op & 0x40 == 0 {
            if op == 0x80 {
                break; // end of stream
            }
            // Medium copy straight from the source.
            let count = (op & 0x3f) as usize;
            let n = count.min(end - dest);
            for _ in 0..n {
                match r.u8() {
                    Some(v) => {
                        out[dest] = v;
                        dest += 1;
                    }
                    None => return dest,
                }
            }
        } else if op == 0xfe {
            // Long run of a single byte.
            let count = match r.u16le() {
                Some(v) => v as usize,
                None => break,
            };
            let value = match r.u8() {
                Some(v) => v,
                None => break,
            };
            let n = count.min(end - dest);
            for _ in 0..n {
                out[dest] = value;
                dest += 1;
            }
        } else if op == 0xff {
            // Long copy from an absolute output offset.
            let count = match r.u16le() {
                Some(v) => v as usize,
                None => break,
            };
            let mut src_idx = match r.u16le() {
                Some(v) => v as usize,
                None => break,
            };
            if src_idx >= end {
                break;
            }
            // Bound by both the destination *and* source ends: `src_idx`
            // advances in lockstep with `dest`, so a `count` that would walk
            // it past `end` must stop the copy early rather than read out of
            // bounds (see `codec::tests::lcw_long_copy_stops_at_source_end`).
            let n = count.min(end - dest).min(end - src_idx);
            for _ in 0..n {
                out[dest] = out[src_idx];
                dest += 1;
                src_idx += 1;
            }
        } else {
            // Medium copy from an absolute output offset.
            let count = ((op & 0x3f) as usize) + 3;
            let mut src_idx = match r.u16le() {
                Some(v) => v as usize,
                None => break,
            };
            if src_idx >= end {
                break;
            }
            // See the long-copy branch above: also bound by the source end.
            let n = count.min(end - dest).min(end - src_idx);
            for _ in 0..n {
                out[dest] = out[src_idx];
                dest += 1;
                src_idx += 1;
            }
        }
    }

    dest
}

/// Apply a Format40 (XOR-delta) stream onto `dst`, which must already hold the
/// reference frame. Bounds-checked; a write that would run past `dst` stops the
/// decode.
pub fn apply_xor_delta(dst: &mut [u8], delta: &[u8]) {
    let mut r = Reader::new(delta);
    let mut put = 0usize;
    let end = dst.len();

    loop {
        let cmd = match r.u8() {
            Some(v) => v,
            None => return,
        };

        // Determine (count, xor_value, from_source) for this command.
        let (count, xor_value, from_source): (usize, u8, bool) = if cmd & 0x80 == 0 {
            if cmd == 0 {
                // 0 count value : XOR a run of `value`.
                let count = match r.u8() {
                    Some(v) => v as usize,
                    None => return,
                };
                let value = match r.u8() {
                    Some(v) => v,
                    None => return,
                };
                (count, value, false)
            } else {
                // 0b0nnnnnnn : XOR the next `cmd` source bytes.
                (cmd as usize, 0, true)
            }
        } else {
            let short = (cmd & 0x7f) as usize;
            if short != 0 {
                // Skip `short` bytes (they equal the reference).
                put = put.saturating_add(short);
                continue;
            }
            let big = match r.u16le() {
                Some(v) => v,
                None => return,
            };
            if big == 0 {
                return; // end marker
            }
            if big & 0x8000 == 0 {
                // Skip `big` bytes.
                put = put.saturating_add(big as usize);
                continue;
            }
            if big & 0x4000 != 0 {
                // XOR a run of `value`.
                let count = (big & 0x3fff) as usize;
                let value = match r.u8() {
                    Some(v) => v,
                    None => return,
                };
                (count, value, false)
            } else {
                // XOR the next `count` source bytes.
                ((big & 0x3fff) as usize, 0, true)
            }
        };

        if from_source {
            for _ in 0..count {
                if put >= end {
                    return;
                }
                match r.u8() {
                    Some(v) => {
                        dst[put] ^= v;
                        put += 1;
                    }
                    None => return,
                }
            }
        } else {
            for _ in 0..count {
                if put >= end {
                    return;
                }
                dst[put] ^= xor_value;
                put += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Hand-built LCW (Format80) streams, one opcode family per test. Opcode
    // layout is taken directly from the comment block in `common/lcw.cpp`
    // (`LCW_Uncompress`):
    //
    //   n=0xxxyyyy,yyyyyyyy       short copy back y bytes, run x+3 from dest
    //   n=10xxxxxx,n1,..,nx+1     medium copy: next x+1 bytes from source
    //   n=11xxxxxx,w1             medium copy from dest, x+3 bytes at offset w1
    //   n=11111111,w1,w2          long copy from dest, w1 bytes at offset w2
    //   n=11111110,w1,b1          long run of byte b1 for w1 bytes
    //   n=10000000                end of data

    #[test]
    fn lcw_medium_copy_from_source() {
        // 0x84 = 0b10_000100 -> medium copy, count = op & 0x3f = 4 literal bytes.
        let src = [0x84, b'A', b'B', b'C', b'D', 0x80];
        let mut out = [0u8; 4];
        let n = lcw_decompress(&src, &mut out);
        assert_eq!(n, 4);
        assert_eq!(&out, b"ABCD");
    }

    #[test]
    fn lcw_short_copy_backreference() {
        // Write "XYZ" (medium copy of 3), then a short back-reference that
        // repeats the last 3 bytes: op=0x00 -> count=(0>>4)+3=3, offset byte=3.
        let src = [0x83, b'X', b'Y', b'Z', 0x00, 0x03, 0x80];
        let mut out = [0u8; 6];
        let n = lcw_decompress(&src, &mut out);
        assert_eq!(n, 6);
        assert_eq!(&out, b"XYZXYZ");
    }

    #[test]
    fn lcw_long_run() {
        // 0xFE, count=5 (u16 LE), value='Q' -> five 'Q's.
        let src = [0xFE, 5, 0, b'Q', 0x80];
        let mut out = [0u8; 5];
        let n = lcw_decompress(&src, &mut out);
        assert_eq!(n, 5);
        assert_eq!(&out, b"QQQQQ");
    }

    #[test]
    fn lcw_long_copy_from_absolute_offset() {
        // Write "ABCDEF" (medium copy of 6), then 0xFF long-copy 2 bytes from
        // absolute output offset 1 ('B','C').
        let src = [
            0x86, b'A', b'B', b'C', b'D', b'E', b'F', 0xFF, 2, 0, 1, 0, 0x80,
        ];
        let mut out = [0u8; 8];
        let n = lcw_decompress(&src, &mut out);
        assert_eq!(n, 8);
        assert_eq!(&out, b"ABCDEFBC");
    }

    #[test]
    fn lcw_medium_copy_from_absolute_offset_overlapping() {
        // Write "MN" (medium copy of 2), then 0xC0 medium-copy-from-dest:
        // count=(0)+3=3 bytes starting at absolute offset 0. Because the copy
        // is byte-at-a-time it can read bytes it just wrote, matching classic
        // LZ77 run-extension behavior.
        let src = [0x82, b'M', b'N', 0xC0, 0, 0, 0x80];
        let mut out = [0u8; 5];
        let n = lcw_decompress(&src, &mut out);
        assert_eq!(n, 5);
        assert_eq!(&out, b"MNMNM");
    }

    #[test]
    fn lcw_long_copy_stops_at_source_end() {
        // Regression test for a bug found by `tests/property_no_panic.rs`:
        // op=0xFF, count=0x0600 (1536), src_idx=0x0100 (256), out_len=257.
        // `src_idx` starts inside bounds (256 < 257) but increments in
        // lockstep with `dest`; a `count` this large used to walk `src_idx`
        // one past `end` and panic with an out-of-bounds index instead of
        // stopping the copy early. Minimized from proptest's failing case:
        // `src = [255, 0, 6, 0, 1], out_len = 257`.
        let src = [0xFF, 0x00, 0x06, 0x00, 0x01];
        let mut out = [0u8; 257];
        // Must not panic; the copy simply stops once it would read past the
        // end of `out`.
        let n = lcw_decompress(&src, &mut out);
        assert!(n <= 257);
    }

    #[test]
    fn lcw_medium_copy_abs_stops_at_source_end() {
        // Same class of bug as above, but via the 0xC0..=0xFD "medium copy
        // from absolute offset" branch: count=(op&0x3f)+3=3, src_idx set to
        // one byte before the end of a small output buffer.
        let src = [0xC0, 0x02, 0x00]; // src_idx = 2, out_len = 3
        let mut out = [0u8; 3];
        let n = lcw_decompress(&src, &mut out);
        assert!(n <= 3);
    }

    #[test]
    fn lcw_end_marker_stops_early() {
        // 0x80 alone should stop decoding immediately, writing nothing.
        let src = [0x80];
        let mut out = [0xAAu8; 4];
        let n = lcw_decompress(&src, &mut out);
        assert_eq!(n, 0);
        assert_eq!(&out, &[0xAA; 4]);
    }

    // Hand-built XOR-delta (Format40) streams, per `common/xordelta.cpp`
    // (`Apply_XOR_Delta`):
    //
    //   cmd == 0                  : count=byte, value=byte -> xor-fill run
    //   cmd & 0x80 == 0, cmd != 0 : xor next `cmd` source bytes
    //   cmd & 0x80 != 0, low7 !=0 : skip (unchanged) `low7` bytes
    //   cmd == 0x80, word == 0    : end of stream
    //   cmd == 0x80, word bit15=0 : skip `word` bytes
    //   cmd == 0x80, bits 15+14=1 : xor-fill run, count = word & 0x3fff
    //   cmd == 0x80, bit15=1 only : xor next (word & 0x3fff) source bytes

    #[test]
    fn xor_delta_skip_then_xor_from_source() {
        let mut dst = [1u8, 2, 3, 4, 5];
        // skip 3 (cmd=0x83), then xor next 2 source bytes with 0xFF,0xFF.
        let delta = [0x83, 0x02, 0xFF, 0xFF];
        apply_xor_delta(&mut dst, &delta);
        assert_eq!(dst, [1, 2, 3, 4 ^ 0xFF, 5 ^ 0xFF]);
    }

    #[test]
    fn xor_delta_small_fill_run() {
        let mut dst = [10u8, 10, 10, 10];
        // cmd=0 -> count=4, value=0x0A: xor-fills every byte to zero.
        let delta = [0x00, 0x04, 0x0A];
        apply_xor_delta(&mut dst, &delta);
        assert_eq!(dst, [0, 0, 0, 0]);
    }

    #[test]
    fn xor_delta_big_skip_then_small_xor() {
        let mut dst = [9u8, 9, 9, 9, 9, 9];
        // cmd=0x80, word=4 (bit15 clear) -> skip 4 bytes; then cmd=2 -> xor
        // next 2 source bytes with 0x01, 0x02.
        let delta = [0x80, 0x04, 0x00, 0x02, 0x01, 0x02];
        apply_xor_delta(&mut dst, &delta);
        assert_eq!(dst, [9, 9, 9, 9, 9 ^ 0x01, 9 ^ 0x02]);
    }

    #[test]
    fn xor_delta_big_fill_run() {
        let mut dst = [0x0Au8; 3];
        // cmd=0x80, word=0xC003 (bit15+bit14 set, count=3) -> fill-run of
        // value 0x05 for 3 bytes.
        let delta = [0x80, 0x03, 0xC0, 0x05];
        apply_xor_delta(&mut dst, &delta);
        assert_eq!(dst, [0x0A ^ 0x05; 3]);
    }

    #[test]
    fn xor_delta_big_xor_from_source() {
        let mut dst = [0x00u8, 0x00];
        // cmd=0x80, word=0x8002 (bit15 set, bit14 clear, count=2) -> xor next
        // 2 source bytes.
        let delta = [0x80, 0x02, 0x80, 0x11, 0x22];
        apply_xor_delta(&mut dst, &delta);
        assert_eq!(dst, [0x11, 0x22]);
    }

    #[test]
    fn xor_delta_end_marker_leaves_dst_untouched() {
        let mut dst = [7u8, 7];
        let delta = [0x80, 0x00, 0x00];
        apply_xor_delta(&mut dst, &delta);
        assert_eq!(dst, [7, 7]);
    }
}
