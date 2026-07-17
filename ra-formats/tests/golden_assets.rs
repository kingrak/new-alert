//! Golden-file integration tests against the real, copyrighted freeware
//! assets (`main.mix`, `redalert.mix`, …).
//!
//! These tests never fail when the assets are missing — they print a message
//! and return early. No extracted game content is ever committed here: every
//! expectation below is a size, a count, a structural fact, or an FNV-1a hash
//! of a decoded buffer (a regression pin, not independently-verified truth —
//! see the note on each hash below for how it was derived).
//!
//! Asset location: the `RA_ASSETS_DIR` environment variable if set, else
//! `<workspace root>/assets` (i.e. `ra-formats/../assets`). To verify the
//! skip path, point `RA_ASSETS_DIR` at a nonexistent directory, e.g.:
//!
//! ```sh
//! RA_ASSETS_DIR=/nonexistent cargo test -p ra-formats --test golden_assets
//! ```

use std::path::{Path, PathBuf};

use ra_formats::mix::MixArchive;
use ra_formats::pal::Palette;
use ra_formats::shp::Shp;

/// Resolve the assets directory: `RA_ASSETS_DIR` env var if set, otherwise
/// `<crate>/../assets` (the workspace-root `assets/` directory).
fn assets_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("RA_ASSETS_DIR") {
        PathBuf::from(dir)
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../assets")
    }
}

/// Locate a required asset file; if it (or the whole assets dir) is absent,
/// print a skip notice and return `None` so the caller can bail out cleanly
/// instead of failing.
fn locate(rel: &str) -> Option<PathBuf> {
    let path = assets_dir().join(rel);
    if path.is_file() {
        Some(path)
    } else {
        eprintln!(
            "SKIP: asset '{}' not found (looked in {}); \
             copy the freeware RA assets into assets/ or set RA_ASSETS_DIR to run this test",
            rel,
            assets_dir().display()
        );
        None
    }
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

/// Tiny dependency-free FNV-1a 64-bit hash, used only to pin regression
/// expectations for decoded buffers. Deterministic across platforms and Rust
/// versions (unlike `std`'s `DefaultHasher`, which explicitly makes no such
/// guarantee).
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

#[test]
fn redalert_mix_header_facts() {
    let Some(path) = locate("redalert.mix") else {
        return;
    };
    let data = read(&path);
    let mix = MixArchive::parse(&data).expect("redalert.mix should parse");

    assert!(mix.encrypted, "redalert.mix header is RA-flagged/encrypted");
    assert_eq!(mix.entries().len(), 6);
    assert_eq!(mix.data_start(), 164);

    // local.mix's index entry, cross-checked with `radump list redalert.mix`.
    let local = mix.find_id(0x97A7_9A21).expect("local.mix entry present");
    assert_eq!(local.offset, 6_467_865);
    assert_eq!(local.size, 3_829_837);
    // `find` by name should hash to the same entry.
    assert_eq!(mix.find("local.mix"), mix.find_id(0x97A7_9A21));

    // A couple of the other known nested archives, same provenance.
    let hires = mix.find_id(0xA7E3_821F).expect("hires.mix entry present");
    assert_eq!(hires.offset, 650_448);
    assert_eq!(hires.size, 5_817_417);
}

#[test]
fn temperat_pal_extraction() {
    let Some(path) = locate("redalert.mix") else {
        return;
    };
    let root = read(&path);
    let mix = MixArchive::parse(&root).expect("redalert.mix should parse");
    let local = mix
        .open_nested("local.mix")
        .expect("local.mix should open as a nested archive");

    let pal_bytes = local
        .get("temperat.pal")
        .expect("temperat.pal present in local.mix");
    assert_eq!(pal_bytes.len(), ra_formats::pal::PAL_BYTES);
    assert_eq!(pal_bytes.len(), 768);

    let pal = Palette::parse(pal_bytes).expect("temperat.pal should parse");
    let mut flat = Vec::with_capacity(768);
    for c in pal.colors.iter() {
        flat.extend_from_slice(c);
    }
    // Pinned from the current decoder output (derived once via a throwaway
    // probe against the real asset, not independently verified against a
    // second implementation).
    assert_eq!(
        fnv1a(&flat),
        0x9f39_e722_545a_185b,
        "temperat.pal decoded-palette hash changed"
    );
}

#[test]
fn tank_shp_frames() {
    let Some(main_path) = locate("main.mix") else {
        return;
    };
    let main = read(&main_path);
    let main_arch = MixArchive::parse(&main).expect("main.mix should parse");
    let conquer = main_arch
        .open_nested("conquer.mix")
        .expect("conquer.mix should open as a nested archive");
    let shp_bytes = conquer.get("2tnk.shp").expect("2tnk.shp present");

    let shp = Shp::parse(shp_bytes).expect("2tnk.shp should parse");
    let hdr = shp.header();
    assert_eq!(shp.frame_count(), 64);
    assert_eq!(hdr.width, 36);
    assert_eq!(hdr.height, 36);

    let frame0 = shp.decode_frame(0).unwrap();
    let frame8 = shp.decode_frame(8).unwrap();
    assert_eq!(frame0.pixels.len(), 36 * 36);
    assert_eq!(frame8.pixels.len(), 36 * 36);

    // Pinned from the current decoder output (regression pins, derived once
    // via a throwaway probe against the real asset — see module docs).
    assert_eq!(
        fnv1a(&frame0.pixels),
        0x66c0_7568_4304_6e7d,
        "2tnk.shp frame 0 pixel hash changed"
    );
    assert_eq!(
        fnv1a(&frame8.pixels),
        0x279b_71f1_3119_4105,
        "2tnk.shp frame 8 pixel hash changed"
    );

    // Determinism guard against the real asset (see also the hand-built
    // version of this guard in `src/shp.rs`'s unit tests): decoding twice
    // must yield byte-identical buffers.
    let frame0_again = shp.decode_frame(0).unwrap();
    let frame8_again = shp.decode_frame(8).unwrap();
    assert_eq!(frame0.pixels, frame0_again.pixels);
    assert_eq!(frame8.pixels, frame8_again.pixels);

    let all = shp.decode_all().unwrap();
    assert_eq!(all.len(), 64);
    assert_eq!(all[0].pixels, frame0.pixels);
    assert_eq!(all[8].pixels, frame8.pixels);
}
