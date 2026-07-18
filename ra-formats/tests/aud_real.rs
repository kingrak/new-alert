//! Decode real AUD sounds from the shipped archives (M7). Skip-clean when the
//! assets are absent, like the other golden-asset tests. Validates the Westwood
//! ADPCM decoder (`ra_formats::aud`) against genuine game audio: weapon SFX
//! from `sounds.mix` and an EVA speech line from `speech.mix`, all IMA-ADPCM
//! (compression 99) — see "codec coverage" below for why WS-delta
//! (compression 1) has no real-asset pin here.
//!
//! ## Codec coverage in the shipped assets
//!
//! A throwaway probe (structural: read every entry's raw bytes, not just a
//! filename sample) walked **every** entry in `main.mix`'s nested
//! `sounds.mix` (116 entries), `redalert.mix`'s nested `speech.mix` (107
//! entries), and the top-level `aud.mix` (47 entries) — 270 entries total,
//! each confirmed to be a real AUD stream by checking the first chunk's
//! `0xDEAF` magic at byte offset 16. **All 270 are compression byte `99`
//! (IMA ADPCM); zero are compression byte `1` (WS-delta).** `setup.mix` (the
//! installer archive) has a handful of entries whose byte 11 happens to equal
//! other values, but none of them have a valid `0xDEAF` chunk magic — they
//! are not AUD files at all (installer resource has a different index).
//!
//! This is an exhaustive check of every AUD file in this freeware asset set,
//! not a 40-50-file sample: real WS-delta (compression 1) audio is simply
//! absent here. The `decode_ws_chunk` path is therefore only exercised by the
//! hand-built unit test in `src/aud.rs` and the no-panic proptest in
//! `tests/property_no_panic.rs`, never against real data — noted here so a
//! future asset drop (e.g. the full retail CD) can be probed the same way and
//! this module extended if a real WS-delta file turns up.

use std::path::PathBuf;

use ra_formats::aud;
use ra_formats::mix::MixArchive;

fn assets_dir() -> PathBuf {
    std::env::var("RA_ASSETS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../assets"))
}

/// Tiny dependency-free FNV-1a 64-bit hash, used only to pin regression
/// expectations for decoded PCM buffers (same construction as
/// `golden_assets.rs`'s `fnv1a` and `ra-client/tests/support::fnv1a` — kept
/// local per-file rather than shared, matching this crate's existing
/// convention of not exporting a test-only hash helper).
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

/// Hash a decoded PCM buffer: each `i16` sample's little-endian bytes fed
/// through `fnv1a`, in sample order.
fn pcm_fnv1a(pcm: &[i16]) -> u64 {
    let mut bytes = Vec::with_capacity(pcm.len() * 2);
    for s in pcm {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    fnv1a(&bytes)
}

/// Parse the fields of a minimal 44-byte RIFF/WAVE header that
/// `ra_formats::aud::to_wav` writes, so callers can assert on them by value
/// rather than just checking magic bytes.
struct WavHeader {
    channels: u16,
    sample_rate: u32,
    byte_rate: u32,
    block_align: u16,
    bits_per_sample: u16,
    data_len: u32,
}

fn parse_wav_header(wav: &[u8]) -> WavHeader {
    assert!(wav.len() >= 44, "WAV header truncated");
    assert_eq!(&wav[0..4], b"RIFF");
    assert_eq!(&wav[8..12], b"WAVE");
    assert_eq!(&wav[12..16], b"fmt ");
    assert_eq!(&wav[36..40], b"data");
    let u16le = |lo: usize| u16::from_le_bytes([wav[lo], wav[lo + 1]]);
    let u32le = |lo: usize| u32::from_le_bytes([wav[lo], wav[lo + 1], wav[lo + 2], wav[lo + 3]]);
    WavHeader {
        channels: u16le(22),
        sample_rate: u32le(24),
        byte_rate: u32le(28),
        block_align: u16le(32),
        bits_per_sample: u16le(34),
        data_len: u32le(40),
    }
}

/// Assert every WAV header field is internally consistent with the decoded
/// clip it was wrapped from (16-bit PCM is the only format `to_wav` writes).
fn assert_wav_header_consistent(clip: &aud::AudClip, wav: &[u8]) {
    let hdr = parse_wav_header(wav);
    let channels = clip.channels.max(1) as u32;
    assert_eq!(hdr.channels as u32, channels, "WAV channel count mismatch");
    assert_eq!(
        hdr.sample_rate, clip.sample_rate,
        "WAV sample rate mismatch"
    );
    assert_eq!(hdr.bits_per_sample, 16, "WAV bits-per-sample must be 16");
    let expected_block_align = (channels * 2) as u16;
    assert_eq!(
        hdr.block_align, expected_block_align,
        "WAV block align mismatch"
    );
    let expected_byte_rate = clip.sample_rate * channels * 2;
    assert_eq!(hdr.byte_rate, expected_byte_rate, "WAV byte rate mismatch");
    let expected_data_len = (clip.pcm.len() * 2) as u32;
    assert_eq!(
        hdr.data_len, expected_data_len,
        "WAV data chunk length mismatch"
    );
    assert_eq!(
        wav.len(),
        44 + expected_data_len as usize,
        "WAV file length mismatch"
    );
}

#[test]
fn decodes_real_weapon_and_speech_auds() {
    let dir = assets_dir();
    if !dir.join("main.mix").is_file() || !dir.join("redalert.mix").is_file() {
        eprintln!("SKIP: assets not found under {}", dir.display());
        return;
    }
    let main_bytes = std::fs::read(dir.join("main.mix")).expect("read main.mix");
    let redalert_bytes = std::fs::read(dir.join("redalert.mix")).expect("read redalert.mix");
    let main = MixArchive::parse(&main_bytes).expect("parse main.mix");
    let redalert = MixArchive::parse(&redalert_bytes).expect("parse redalert.mix");

    // Weapon SFX (IMA / compression 99).
    let sounds = main.open_nested("sounds.mix").expect("open sounds.mix");
    let gun = sounds.get("GUN5.AUD").expect("GUN5.AUD present");
    let clip = aud::decode(gun).expect("decode GUN5");
    assert_eq!(clip.sample_rate, 22050, "GUN5 expected 22050 Hz");
    assert!(clip.pcm.len() > 1000, "GUN5 decoded too few samples");
    // Not silent, and within 16-bit range (clamp is enforced by the decoder).
    assert!(
        clip.pcm.iter().any(|&s| s.abs() > 64),
        "GUN5 decoded to silence"
    );

    // The WAV wrapper produces a well-formed 44-byte RIFF header, and every
    // header field (channels, rate, byte rate, block align, bits/sample,
    // data length) is internally consistent with the decoded clip.
    let wav = aud::to_wav(&clip);
    assert_wav_header_consistent(&clip, &wav);

    // EVA speech line (construction complete).
    let speech = redalert.open_nested("speech.mix").expect("open speech.mix");
    if let Some(evadata) = speech.get("CONSCMP1.AUD") {
        let eva = aud::decode(evadata).expect("decode CONSCMP1");
        assert!(eva.pcm.len() > 1000, "CONSCMP1 decoded too few samples");
        assert!(
            eva.pcm.iter().any(|&s| s.abs() > 64),
            "CONSCMP1 decoded to silence"
        );
        eprintln!(
            "decoded CONSCMP1: {} Hz, {} samples",
            eva.sample_rate,
            eva.pcm.len()
        );
    }
}

/// Pinned PCM sample counts + FNV-1a hashes for a handful of real, named AUD
/// files, covering both `sounds.mix` (weapon SFX) and `speech.mix` (EVA
/// speech) — the *only* codec present in this asset set is IMA/compression
/// 99 (see the module doc comment: an exhaustive structural probe of all 270
/// real AUD entries across `sounds.mix` + `speech.mix` + `aud.mix` found zero
/// WS-delta/compression-1 files), so these are all IMA.
///
/// Every hash below was derived once with a throwaway probe (a `#[test]`
/// identical in shape to this one, since deleted) that ran:
///
/// ```text
/// let bytes = mix.get(NAME).unwrap();
/// let clip = ra_formats::aud::decode(bytes).unwrap();
/// println!("samples={} pcm_fnv1a=0x{:016x}", clip.pcm.len(), pcm_fnv1a(&clip.pcm));
/// ```
///
/// against `assets/main.mix` -> `sounds.mix` and `assets/redalert.mix` ->
/// `speech.mix` from this workspace's `assets/` directory, then hardcoded the
/// printed values here as the pinned expectation (same "derive once, pin the
/// value" policy as `golden_assets.rs`) — not independently verified against
/// a second decoder.
#[test]
fn pinned_pcm_hashes_for_real_auds() {
    let dir = assets_dir();
    if !dir.join("main.mix").is_file() || !dir.join("redalert.mix").is_file() {
        eprintln!("SKIP: assets not found under {}", dir.display());
        return;
    }
    let main_bytes = std::fs::read(dir.join("main.mix")).expect("read main.mix");
    let redalert_bytes = std::fs::read(dir.join("redalert.mix")).expect("read redalert.mix");
    let main = MixArchive::parse(&main_bytes).expect("parse main.mix");
    let redalert = MixArchive::parse(&redalert_bytes).expect("parse redalert.mix");
    let sounds = main.open_nested("sounds.mix").expect("open sounds.mix");
    let speech = redalert.open_nested("speech.mix").expect("open speech.mix");

    // (label, archive, entry name, expected sample_rate, expected channels,
    // expected pcm.len(), expected FNV-1a hash of the PCM bytes).
    let cases: [(&str, &MixArchive, &str, u32, u8, usize, u64); 3] = [
        (
            "sounds.mix/GUN5.AUD",
            &sounds,
            "GUN5.AUD",
            22050,
            1,
            20384,
            0xc2fb_78bb_5b9a_11d6,
        ),
        (
            "sounds.mix/CANNON1.AUD",
            &sounds,
            "CANNON1.AUD",
            22050,
            1,
            22784,
            0x048b_b510_5902_01c0,
        ),
        (
            "speech.mix/CONSCMP1.AUD",
            &speech,
            "CONSCMP1.AUD",
            22050,
            1,
            31992,
            0x459e_bf86_87b7_38c4,
        ),
    ];

    for (label, mix, name, exp_rate, exp_channels, exp_len, exp_hash) in cases {
        let bytes = mix.get(name).unwrap_or_else(|| panic!("{name} present"));
        let clip = aud::decode(bytes).unwrap_or_else(|e| panic!("decode {name}: {e}"));
        assert_eq!(clip.sample_rate, exp_rate, "{label} sample_rate mismatch");
        assert_eq!(clip.channels, exp_channels, "{label} channels mismatch");
        assert_eq!(clip.pcm.len(), exp_len, "{label} sample count changed");
        let hash = pcm_fnv1a(&clip.pcm);
        assert_eq!(hash, exp_hash, "{label} PCM hash changed (0x{hash:016x})");
    }
}
