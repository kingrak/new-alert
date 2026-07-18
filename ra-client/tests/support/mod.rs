//! Shared test support for `ra-client`'s UI test suites (DESIGN.md §4.8).
//! Not a test binary itself (`tests/support/mod.rs`, not `tests/support.rs`)
//! — Cargo only auto-discovers direct children of `tests/` as test targets,
//! so each suite pulls this in with `mod support;`.
//!
//! Each `tests/*.rs` file is compiled as its own separate crate, and not
//! every suite uses every helper here — `#![allow(dead_code)]` avoids a
//! per-binary "unused function" warning for the ones a given suite doesn't
//! need (which `-D warnings` would otherwise turn into a build failure).
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::OnceLock;

use ra_client::appcore::AppCore;
use ra_client::assets;
use ra_client::compositor::{IndexedImage, Palette};
use ra_client::terrain::{self, TileSet};
use ra_data::scenario::{MapCell, Scenario, Theater, MAP_CELL_H, MAP_CELL_W};
use ra_data::templates;
use ra_formats::tmpl::Template;

/// Tiny dependency-free FNV-1a 64-bit hash — same algorithm used by the
/// `ra-formats` and `ra-data` golden tests, reimplemented here rather than
/// shared across crates (it's a test-only utility, not worth a dependency).
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

/// Resolve the assets directory: `RA_ASSETS_DIR` env var if set, else
/// `<crate>/../assets`.
pub fn assets_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("RA_ASSETS_DIR") {
        PathBuf::from(dir)
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../assets")
    }
}

/// Whether both real archives needed to load a scenario are present.
pub fn real_assets_available() -> bool {
    let dir = assets_dir();
    dir.join("main.mix").is_file() && dir.join("redalert.mix").is_file()
}

/// Load the M2 reference scenario (`scg01ea.ini`, Snow theater) into an
/// `AppCore` from the real assets, or print a skip notice and return `None`.
pub fn load_real_core() -> Option<AppCore> {
    if !real_assets_available() {
        eprintln!(
            "SKIP: real assets not found under {} (set RA_ASSETS_DIR or copy \
             main.mix/redalert.mix into assets/ to run this test)",
            assets_dir().display()
        );
        return None;
    }
    let loaded = assets::load_from_dir(&assets_dir(), "scg01ea.ini")
        .expect("scg01ea.ini should load from the real assets");
    Some(loaded.into_appcore())
}

/// Build a hand-rolled RA-layout template file (see `ra_formats::tmpl`
/// module docs for the header layout) with `count` icons, each a solid
/// `ICON_WIDTH`x`ICON_HEIGHT` block of a distinct, deterministic palette
/// index (`1..=count`, so index 0 never appears from a drawn tile — makes
/// "was this cell actually drawn" trivial to assert on). All icons are
/// opaque (no transparent index-0 punch-through), and the icon-number to
/// image-index map is the identity, i.e. no deduplication.
fn build_hand_template(count: u16) -> Vec<u8> {
    use ra_formats::tmpl::{ICON_HEIGHT, ICON_WIDTH};
    let icon_size = ICON_WIDTH * ICON_HEIGHT;

    let header_len = 0x28u32; // RA layout: Map field ends at 0x28.
    let icons_off = header_len;
    let icons_len = icon_size as u32 * count as u32;
    let map_off = icons_off + icons_len;
    let map_len = count as u32;
    let trans_off = map_off + map_len;
    let trans_len = count as u32;
    let colormap_off = trans_off + trans_len;
    let colormap_len = count as u32;

    let mut out = Vec::new();
    out.extend_from_slice(&(ICON_WIDTH as u16).to_le_bytes()); // 0x00 width
    out.extend_from_slice(&(ICON_HEIGHT as u16).to_le_bytes()); // 0x02 height
    out.extend_from_slice(&count.to_le_bytes()); // 0x04 count
    out.extend_from_slice(&0u16.to_le_bytes()); // 0x06 Allocated
    out.extend_from_slice(&1u16.to_le_bytes()); // 0x08 MapWidth
    out.extend_from_slice(&1u16.to_le_bytes()); // 0x0A MapHeight
    out.extend_from_slice(&(colormap_off + colormap_len).to_le_bytes()); // 0x0C Size (RA disc.)
    out.extend_from_slice(&icons_off.to_le_bytes()); // 0x10 Icons
    out.extend_from_slice(&0u32.to_le_bytes()); // 0x14 Palettes
    out.extend_from_slice(&0u32.to_le_bytes()); // 0x18 Remaps
    out.extend_from_slice(&trans_off.to_le_bytes()); // 0x1C TransFlag
    out.extend_from_slice(&colormap_off.to_le_bytes()); // 0x20 ColorMap
    out.extend_from_slice(&map_off.to_le_bytes()); // 0x24 Map
    assert_eq!(out.len() as u32, header_len);

    for i in 0..count {
        let fill = (i as u8).wrapping_add(1); // 1..=count, never 0
        out.extend(std::iter::repeat_n(fill, icon_size));
    }
    for i in 0..count {
        out.push(i as u8); // identity map: icon i -> image i
    }
    out.extend(std::iter::repeat_n(0u8, count as usize)); // trans flags: all opaque
    out.extend(std::iter::repeat_n(0u8, count as usize)); // colormap: unused by tests

    out
}

/// A distinguishable, deterministic 256-entry palette (not the flat test
/// palettes used by `compositor`'s own unit tests, so sweep/monkey output
/// visibly varies pixel-to-pixel, catching index-mixups a flat palette
/// couldn't).
fn synthetic_palette() -> Palette {
    let mut pal = [[0u8; 3]; 256];
    for (i, entry) in pal.iter_mut().enumerate() {
        *entry = [i as u8, 255u8.wrapping_sub(i as u8), 128];
    }
    pal
}

/// Memoized [`synthetic_raster_and_palette`]: rasterizing walks all 16384
/// cells through a hashmap lookup, which is cheap once but adds up when a
/// test needs a fresh core per proptest case (dozens to hundreds of times).
/// Callers that need their own mutable `AppCore` should clone the raster
/// (`IndexedImage` is a plain `Vec<u8>` wrapper — cloning is a memcpy, far
/// cheaper than re-rasterizing) rather than calling the unmemoized builder
/// repeatedly.
pub fn synthetic_fixture() -> &'static (IndexedImage, Palette) {
    static FIXTURE: OnceLock<(IndexedImage, Palette)> = OnceLock::new();
    FIXTURE.get_or_init(synthetic_raster_and_palette)
}

/// Build a synthetic-map `AppCore` that needs no real assets: a hand-built
/// 16-icon RA-layout `Template` (see [`build_hand_template`]) installed as
/// the `CLEAR1` template, applied via the real `terrain::rasterize` pipeline
/// over an all-"clear" 128x128 `Scenario`. Because clear cells resolve
/// through `Clear_Icon`'s `(x&3) | ((y&3)<<2)` scramble, this produces a
/// full 3072x3072 raster tiled with a deterministic 4x4-icon repeating
/// pattern — a genuinely hand-built map + hand-built template exercised
/// through the same `Scenario` -> `TileSet` -> `rasterize` -> `AppCore` path
/// the real asset loader uses, just with synthetic inputs so it always runs.
pub fn synthetic_core() -> AppCore {
    let (raster, palette) = synthetic_fixture();
    AppCore::new(raster.clone(), *palette)
}

/// The raw pieces behind [`synthetic_core`], exposed separately so tests can
/// also sanity-check the rasterized image directly (not just through
/// `AppCore`).
pub fn synthetic_raster_and_palette() -> (IndexedImage, Palette) {
    let template_bytes = build_hand_template(16);
    let template = Template::parse(&template_bytes).expect("hand-built template should parse");

    let mut tiles = TileSet::new();
    tiles.insert(templates::TEMPLATE_CLEAR1, template);

    let total = (MAP_CELL_W * MAP_CELL_H) as usize;
    let cells = vec![
        MapCell {
            template: 0xFFFF, // "no template" -> resolved as clear terrain
            icon: 0,
        };
        total
    ];
    let scenario = Scenario {
        theater: Theater::Snow, // arbitrary; unused by rasterize
        map_x: 0,
        map_y: 0,
        map_width: 4,
        map_height: 4,
        cells,
        overlay: Vec::new(),
    };

    let raster = terrain::rasterize(&scenario, &tiles);
    (raster, synthetic_palette())
}
