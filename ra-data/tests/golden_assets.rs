//! Golden-file integration tests against the real, copyrighted freeware
//! assets, exercising `ra-data`'s scenario + template-catalog layer on top of
//! `ra-formats`.
//!
//! Same policy as `ra-formats/tests/golden_assets.rs`: skip cleanly (never
//! fail) when the assets are absent, and every expectation is a size, a
//! count, a structural fact, or an FNV-1a hash of decoded data — never
//! checked-in extracted content.
//!
//! Asset location: `RA_ASSETS_DIR` env var if set, else
//! `<workspace root>/assets`. To verify the skip path:
//!
//! ```sh
//! RA_ASSETS_DIR=/nonexistent cargo test -p ra-data --test golden_assets
//! ```
//!
//! Reference scenario throughout: `scg01ea.ini` (the M2 reference scenario;
//! see `docs/DESIGN.md` and the M2 handoff notes), a Snow-theater map with
//! playable rect x=49 y=45 w=30 h=36.

use std::path::{Path, PathBuf};

use ra_data::scenario::{Scenario, Theater};
use ra_data::templates;
use ra_formats::mix::MixArchive;
use ra_formats::tmpl::Template;

fn assets_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("RA_ASSETS_DIR") {
        PathBuf::from(dir)
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../assets")
    }
}

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

/// Same dependency-free FNV-1a used by `ra-formats`' golden tests (kept
/// independent per-crate rather than shared, since these are test-only
/// utilities and neither crate should gain a test-support dependency on the
/// other).
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

/// Load `scg01ea.ini`'s parsed `Scenario`, or `None` (with a skip notice) if
/// `main.mix` is absent.
fn load_scg01ea() -> Option<Scenario> {
    let path = locate("main.mix")?;
    let main = read(&path);
    let main_arch = MixArchive::parse(&main).expect("main.mix should parse");
    let general = main_arch
        .open_nested("general.mix")
        .expect("general.mix should open as a nested archive");
    let ini_bytes = general
        .get("scg01ea.ini")
        .expect("scg01ea.ini present in general.mix");
    let ini_text = String::from_utf8_lossy(ini_bytes);
    Some(Scenario::parse(&ini_text).expect("scg01ea.ini should parse as a scenario"))
}

#[test]
fn scg01ea_map_facts() {
    let Some(scen) = load_scg01ea() else {
        return;
    };
    assert_eq!(scen.theater, Theater::Snow);
    assert_eq!(scen.map_x, 49);
    assert_eq!(scen.map_y, 45);
    assert_eq!(scen.map_width, 30);
    assert_eq!(scen.map_height, 36);
    assert_eq!(scen.cells.len(), 128 * 128);
    assert_eq!(scen.overlay.len(), 128 * 128);
}

#[test]
fn scg01ea_known_cells() {
    let Some(scen) = load_scg01ea() else {
        return;
    };
    // A handful of cells pinned from the current decoder output (regression
    // pins derived once via a throwaway probe against the real asset, not
    // independently re-verified against a second implementation — same
    // caveat as every other golden hash in this suite).
    assert_eq!(scen.cell(0, 0).template, 255); // legacy "no template" sentinel
    assert_eq!(scen.cell(0, 0).icon, 0);
    assert_eq!(
        scen.cell(49, 45),
        ra_data::scenario::MapCell {
            template: 71,
            icon: 3
        }
    ); // playable rect's top-left corner cell
    assert_eq!(
        scen.cell(50, 46),
        ra_data::scenario::MapCell {
            template: 87,
            icon: 1
        }
    );
    assert_eq!(
        scen.cell(78, 80),
        ra_data::scenario::MapCell {
            template: 1,
            icon: 0
        }
    ); // TEMPLATE_WATER
    assert_eq!(scen.cell(63, 62).template, 255);
}

#[test]
fn scg01ea_cell_plane_hash() {
    let Some(scen) = load_scg01ea() else {
        return;
    };
    // Encode every cell as [u16 template LE][u8 icon] and hash the whole
    // plane: a single regression pin over the entire decoded [MapPack],
    // catching any drift in the pack/base64/LCW pipeline that per-cell spot
    // checks alone might miss.
    let mut buf = Vec::with_capacity(scen.cells.len() * 3);
    for c in &scen.cells {
        buf.extend_from_slice(&c.template.to_le_bytes());
        buf.push(c.icon);
    }
    assert_eq!(
        fnv1a(&buf),
        0xbfcf_199f_1876_ed58,
        "scg01ea cell-plane hash changed"
    );

    // The overlay plane too (present for this scenario).
    assert_eq!(
        fnv1a(&scen.overlay),
        0x30f2_9ae9_2b4a_1ed5,
        "scg01ea overlay-plane hash changed"
    );
}

/// Catalog spot-check: for a sample of template ids actually referenced by
/// scg01ea's cells, `template_filename` must produce a name that really
/// exists inside the Snow theater mix — i.e. the static catalog (`ra-data`)
/// and the real asset archive agree on template naming for this theater.
#[test]
fn scg01ea_template_catalog_matches_theater_mix() {
    let Some(scen) = load_scg01ea() else {
        return;
    };
    let Some(main_path) = locate("main.mix") else {
        return;
    };
    let main = read(&main_path);
    let main_arch = MixArchive::parse(&main).expect("main.mix should parse");
    let theater_mix = main_arch
        .open_nested(scen.theater.mix_name())
        .expect("snow.mix should open as a nested archive");
    let suffix = scen.theater.suffix();
    assert_eq!(suffix, "SNO");

    // Known ids referenced by scg01ea, cross-checked against the real
    // archive contents (derived from the scenario's own distinct-template
    // set, so these are guaranteed present in *some* real map, not
    // hand-picked hopefully-valid ids).
    let known_used_ids: &[u16] = &[1, 2, 33, 60, 61, 62, 63, 64, 68, 71];
    let expected_names: &[&str] = &[
        "W1.SNO", "W2.SNO", "SH31.SNO", "WC02.SNO", "WC03.SNO", "WC04.SNO", "WC05.SNO", "WC06.SNO",
        "WC10.SNO", "WC13.SNO",
    ];

    let mut used: std::collections::BTreeSet<u16> = std::collections::BTreeSet::new();
    for c in &scen.cells {
        if c.template != templates::TEMPLATE_NONE && c.template != 255 {
            used.insert(c.template);
        }
    }

    for (&id, &expected_name) in known_used_ids.iter().zip(expected_names) {
        assert!(
            used.contains(&id),
            "expected template id {id} to be used by scg01ea (fixture drift?)"
        );
        let filename =
            templates::template_filename(id, suffix).expect("id should be in the catalog");
        assert_eq!(filename, expected_name);
        assert!(
            theater_mix.get(&filename).is_some(),
            "catalog says template {id} lives at '{filename}', but it's not in snow.mix"
        );
    }
}

/// One template file's icon-count + icon-pixel hash: `W1.SNO` (id 1,
/// `TEMPLATE_WATER`), a single-icon template, so a simple case to pin.
#[test]
fn w1_sno_template_icon_hash() {
    let Some(main_path) = locate("main.mix") else {
        return;
    };
    let main = read(&main_path);
    let main_arch = MixArchive::parse(&main).expect("main.mix should parse");
    let snow_mix = main_arch
        .open_nested("snow.mix")
        .expect("snow.mix should open as a nested archive");
    let w1_bytes = snow_mix.get("W1.SNO").expect("W1.SNO present in snow.mix");

    let w1 = Template::parse(w1_bytes).expect("W1.SNO should parse");
    assert_eq!(w1.width(), 24);
    assert_eq!(w1.height(), 24);
    assert_eq!(w1.count(), 1);

    let icon0 = w1.icon(0).expect("W1.SNO icon 0 should be drawable");
    assert_eq!(icon0.pixels.len(), 24 * 24);
    assert!(!icon0.transparent);
    assert_eq!(
        fnv1a(icon0.pixels),
        0x7c6b_be3c_6846_6cb6,
        "W1.SNO icon 0 pixel hash changed"
    );
    // Only one logical icon in this template.
    assert!(w1.icon(1).is_none());
}
