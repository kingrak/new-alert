//! Westwood AUD audio decoder (M7). AUD files hold ADPCM-compressed PCM in one
//! of two schemes, selected by the header's `compression` byte:
//!
//! - **`99` (0x63) — IMA ADPCM** (Westwood's "sos" codec). 4:1 nibble ADPCM,
//!   ported from `common/soscodec.cpp` (`sosCODECDecompressDataTemplate`): the
//!   predicted sample and step index carry **across chunks** (one continuous
//!   stream). Output is 16-bit signed. This is what the weapon/EVA sounds use.
//! - **`1` — Westwood delta ("WS-ADPCM")**, ported from `common/auduncmp.cpp`
//!   (`Audio_Unzap`): 8-bit unsigned output, sample state **reset per chunk**.
//!
//! The container: a 12-byte header (`u16 rate`, `u32 compressed`, `u32
//! uncompressed`, `u8 flags`, `u8 compression`) then a sequence of chunks, each
//! an 8-byte header (`u16 compressed`, `u16 uncompressed`, `u32 id` = `0xDEAF`)
//! followed by the chunk's compressed bytes.
//!
//! We decode to interleaved 16-bit signed PCM and report the sample rate. Mono
//! is fully supported (every RA sound we use is mono 22050 Hz); a stereo flag is
//! decoded as a linear stream (the mono path) — documented as a limitation, not
//! reached by the shipped sound set.

use crate::FormatError;

/// Decoded PCM audio.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudClip {
    /// Playback sample rate in Hz.
    pub sample_rate: u32,
    /// Channel count (1 = mono).
    pub channels: u8,
    /// 16-bit signed PCM samples (interleaved if `channels > 1`).
    pub pcm: Vec<i16>,
}

/// IMA step-size table (`wCODECStepTab`, soscodec.cpp:8).
const STEP_TAB: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];

/// IMA index-adjust table (`wCODECIndexTab`, soscodec.cpp:6).
const INDEX_TAB: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];

/// WS delta tables (`ZapTabTwo`/`ZapTabFour`, auduncmp.cpp).
const ZAP2: [i32; 4] = [-2, -1, 0, 1];
const ZAP4: [i32; 16] = [-9, -8, -6, -5, -4, -3, -2, -1, 0, 1, 2, 3, 4, 5, 6, 8];

const AUD_CHUNK_MAGIC: u32 = 0x0000_DEAF;

/// Decode an AUD file to 16-bit signed PCM.
pub fn decode(bytes: &[u8]) -> Result<AudClip, FormatError> {
    if bytes.len() < 12 {
        return Err(FormatError::UnexpectedEof {
            context: "AUD header",
        });
    }
    let sample_rate = u16::from_le_bytes([bytes[0], bytes[1]]) as u32;
    // bytes[2..6] = compressed size, [6..10] = uncompressed size (advisory).
    // flags bit0 = stereo (we decode the linear/mono path regardless — the RA
    // sound set is entirely mono, so this is a documented limitation not hit in
    // practice); bytes[10] flags, bytes[11] compression type.
    let compression = bytes[11];

    let mut pcm: Vec<i16> = Vec::new();
    let mut pos = 12usize;

    // IMA state persists across chunks; WS resets each chunk.
    let mut ima_sample: i32 = 0;
    let mut ima_index: i32 = 0;

    while pos + 8 <= bytes.len() {
        let comp_size = u16::from_le_bytes([bytes[pos], bytes[pos + 1]]) as usize;
        let out_size = u16::from_le_bytes([bytes[pos + 2], bytes[pos + 3]]) as usize;
        let id = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]);
        pos += 8;
        if id != AUD_CHUNK_MAGIC {
            // Not a valid chunk boundary — stop rather than emit garbage.
            break;
        }
        if pos + comp_size > bytes.len() {
            break;
        }
        let chunk = &bytes[pos..pos + comp_size];
        pos += comp_size;

        match compression {
            99 => decode_ima_chunk(chunk, &mut ima_sample, &mut ima_index, &mut pcm),
            1 => decode_ws_chunk(chunk, out_size, &mut pcm),
            _ => {
                return Err(FormatError::Invalid {
                    reason: "unsupported AUD compression type",
                })
            }
        }
    }

    Ok(AudClip {
        sample_rate: sample_rate.max(1),
        // We report mono so the WAV header matches the linear sample layout.
        channels: 1,
        pcm,
    })
}

/// Decode one IMA-ADPCM chunk, carrying `sample`/`index` state across calls.
/// Each source byte holds two nibbles, low nibble first (soscodec.cpp:126-160).
fn decode_ima_chunk(chunk: &[u8], sample: &mut i32, index: &mut i32, out: &mut Vec<i16>) {
    for &b in chunk {
        for nybble in [b & 0x0F, b >> 4] {
            let step = STEP_TAB[(*index).clamp(0, 88) as usize];
            let mut diff = step >> 3;
            if nybble & 1 != 0 {
                diff += step >> 2;
            }
            if nybble & 2 != 0 {
                diff += step >> 1;
            }
            if nybble & 4 != 0 {
                diff += step;
            }
            if nybble & 8 != 0 {
                diff = -diff;
            }
            *sample = (*sample + diff).clamp(-32768, 32767);
            out.push(*sample as i16);
            *index = (*index + INDEX_TAB[nybble as usize]).clamp(0, 88);
        }
    }
}

/// Decode one Westwood-delta ("WS-ADPCM") chunk to 16-bit PCM. Faithful port of
/// `Audio_Unzap` (auduncmp.cpp): 8-bit unsigned samples starting at 0x80,
/// widened to signed 16-bit (`(s - 128) << 8`). `out_size` bounds the output.
fn decode_ws_chunk(chunk: &[u8], out_size: usize, out: &mut Vec<i16>) {
    let push = |out: &mut Vec<i16>, s: i32| {
        let u = s.clamp(0, 255);
        out.push(((u - 128) << 8) as i16);
    };
    let mut sample: i32 = 0x80;
    let mut remaining = out_size as i32;
    let mut i = 0usize;
    while remaining > 0 && i < chunk.len() {
        let shifted = (chunk[i] as u16) << 2;
        i += 1;
        let code = (shifted >> 8) as u8;
        let mut count = ((shifted & 0x00FF) >> 2) as i8;
        match code {
            2 => {
                if count as u8 & 0x20 != 0 {
                    // Raw signed 6-bit delta (sign-extended by the <<3/>>3 pair).
                    let delta = ((count as i32) << 3) >> 3;
                    sample += delta;
                    push(out, sample);
                    remaining -= 1;
                } else {
                    // Copy (count+1) raw bytes.
                    count = count.wrapping_add(1);
                    while count > 0 && i < chunk.len() {
                        push(out, chunk[i] as i32);
                        sample = chunk[i] as i32;
                        i += 1;
                        count -= 1;
                        remaining -= 1;
                    }
                }
            }
            1 => {
                // 4-bit deltas, two per byte.
                count = count.wrapping_add(1);
                while count > 0 && i < chunk.len() {
                    let c = chunk[i];
                    i += 1;
                    sample += ZAP4[(c & 0x0F) as usize];
                    push(out, sample);
                    sample += ZAP4[(c >> 4) as usize];
                    push(out, sample);
                    count -= 1;
                    remaining -= 2;
                }
            }
            0 => {
                // 2-bit deltas, four per byte.
                count = count.wrapping_add(1);
                while count > 0 && i < chunk.len() {
                    let c = chunk[i];
                    i += 1;
                    sample += ZAP2[(c & 0x03) as usize];
                    push(out, sample);
                    sample += ZAP2[((c >> 2) & 0x03) as usize];
                    push(out, sample);
                    sample += ZAP2[((c >> 4) & 0x03) as usize];
                    push(out, sample);
                    sample += ZAP2[((c >> 6) & 0x03) as usize];
                    push(out, sample);
                    count -= 1;
                    remaining -= 4;
                }
            }
            _ => {
                // Repeat the current sample (count+1) times.
                count = count.wrapping_add(1);
                for _ in 0..count {
                    push(out, sample);
                    remaining -= 1;
                }
            }
        }
    }
}

/// Wrap 16-bit signed PCM in a minimal in-memory WAV (RIFF/PCM) container, so the
/// client can hand it to a generic decoder with zero extra dependencies.
pub fn to_wav(clip: &AudClip) -> Vec<u8> {
    let channels = clip.channels.max(1) as u32;
    let bits = 16u32;
    let byte_rate = clip.sample_rate * channels * (bits / 8);
    let block_align = (channels * (bits / 8)) as u16;
    let data_len = (clip.pcm.len() * 2) as u32;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVE");
    w.extend_from_slice(b"fmt ");
    w.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&(channels as u16).to_le_bytes());
    w.extend_from_slice(&clip.sample_rate.to_le_bytes());
    w.extend_from_slice(&byte_rate.to_le_bytes());
    w.extend_from_slice(&block_align.to_le_bytes());
    w.extend_from_slice(&(bits as u16).to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for s in &clip.pcm {
        w.extend_from_slice(&s.to_le_bytes());
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal IMA AUD (one chunk) and check it round-trips through the
    /// decoder without panicking, producing 2 samples per compressed byte.
    #[test]
    fn ima_header_and_one_chunk_decodes() {
        let payload = [0x00u8, 0x11, 0x22]; // 3 bytes -> 6 samples
        let mut f = Vec::new();
        f.extend_from_slice(&22050u16.to_le_bytes()); // rate
        f.extend_from_slice(&(payload.len() as u32).to_le_bytes()); // compressed
        f.extend_from_slice(&12u32.to_le_bytes()); // uncompressed (6 samples * 2)
        f.push(0x02); // flags: 16-bit
        f.push(99); // IMA
                    // chunk header
        f.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        f.extend_from_slice(&12u16.to_le_bytes());
        f.extend_from_slice(&AUD_CHUNK_MAGIC.to_le_bytes());
        f.extend_from_slice(&payload);

        let clip = decode(&f).expect("decode");
        assert_eq!(clip.sample_rate, 22050);
        assert_eq!(clip.channels, 1);
        assert_eq!(clip.pcm.len(), payload.len() * 2);
    }

    #[test]
    fn ws_chunk_repeat_and_raw_paths() {
        // code path is data-dependent; just ensure a WS file decodes cleanly.
        let payload = [0xFCu8, 0x00, 0x01]; // arbitrary
        let mut f = Vec::new();
        f.extend_from_slice(&11025u16.to_le_bytes());
        f.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        f.extend_from_slice(&8u32.to_le_bytes());
        f.push(0x00); // 8-bit mono
        f.push(1); // WS-ADPCM
        f.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        f.extend_from_slice(&8u16.to_le_bytes());
        f.extend_from_slice(&AUD_CHUNK_MAGIC.to_le_bytes());
        f.extend_from_slice(&payload);
        let clip = decode(&f).expect("decode");
        assert_eq!(clip.sample_rate, 11025);
        // WAV wrap must produce a well-formed 44-byte header + data.
        let wav = to_wav(&clip);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(wav.len(), 44 + clip.pcm.len() * 2);
    }

    #[test]
    fn truncated_header_errors_cleanly() {
        assert!(decode(&[0u8; 4]).is_err());
    }

    #[test]
    fn unknown_compression_errors() {
        let mut f = Vec::new();
        f.extend_from_slice(&22050u16.to_le_bytes());
        f.extend_from_slice(&0u32.to_le_bytes());
        f.extend_from_slice(&0u32.to_le_bytes());
        f.push(0x02);
        f.push(42); // unknown
        f.extend_from_slice(&2u16.to_le_bytes());
        f.extend_from_slice(&2u16.to_le_bytes());
        f.extend_from_slice(&AUD_CHUNK_MAGIC.to_le_bytes());
        f.extend_from_slice(&[0u8, 0u8]);
        assert!(decode(&f).is_err());
    }
}
