//! M7.6 land-type suite, real-map half (QUIRKS Q6): per-theater *template*
//! `land_control` spot checks against actual map content. Complements
//! `ra-sim/tests/landtype_suite.rs`'s rules.ini mask-correctness table and
//! synthetic per-locomotor property tests — this file is the one that needs
//! `ra-client`'s `TileSet::land_type` (template id + icon -> `LandType`,
//! `ra-client/src/terrain.rs`), so it lives here rather than in `ra-sim`.
//!
//! Skips cleanly (never fails) without the real assets, per repo policy.

mod support;

use ra_client::assets;
use ra_data::landtype::LandType;

/// Load the real `scg01ea` (Snow theater) scenario's terrain, or print a
/// skip notice and return `None` — mirrors `support::load_real_core`'s skip
/// message but only needs `main.mix` (no `redalert.mix`/rules.ini), since
/// `TileSet::land_type` reads template art, not rules.ini.
fn load_terrain() -> Option<ra_client::assets::LoadedTerrain> {
    let dir = support::assets_dir();
    if !dir.join("main.mix").is_file() {
        eprintln!(
            "SKIP: real assets not found under {} (set RA_ASSETS_DIR or copy main.mix into \
             assets/ to run this test)",
            dir.display()
        );
        return None;
    }
    Some(
        assets::load_from_dir(&dir, "scg01ea.ini")
            .expect("scg01ea.ini should load from the real assets"),
    )
}

/// Tally every real map cell's resolved `LandType` (`TileSet::land_type`,
/// which runs each cell's `(template, icon)` through `Template::land_control`
/// -> `land_from_control`; the `land_from_control` table itself was verified
/// byte-for-byte against `cdata.cpp`'s `_land[16]` table during the M7.6
/// re-pin audit — not re-derived here). A real 128x128 skirmish map is
/// overwhelmingly Clear, but a coastal/snow map like `scg01ea` should have
/// at least some Water (shoreline) and — this is the actual "known cliff
/// template" spot check the M7.6 plan calls for — at least some Rock
/// (impassable cliff/mountain terrain), proving the derivation resolves real
/// map content to the hard-blocking classes, not just a theoretical table.
#[test]
fn real_scg01ea_map_resolves_water_and_rock_cells_from_real_template_art() {
    let Some(terrain) = load_terrain() else {
        return;
    };
    let mut counts: std::collections::BTreeMap<&'static str, u32> =
        std::collections::BTreeMap::new();
    for cell in &terrain.scenario.cells {
        let land = terrain.tiles.land_type(cell.template, cell.icon);
        let name = match land {
            LandType::Clear => "Clear",
            LandType::Road => "Road",
            LandType::Water => "Water",
            LandType::Rock => "Rock",
            LandType::Wall => "Wall",
            LandType::Ore => "Ore",
            LandType::Beach => "Beach",
            LandType::Rough => "Rough",
            LandType::River => "River",
        };
        *counts.entry(name).or_insert(0) += 1;
    }
    eprintln!("scg01ea land-type cell counts: {counts:?}");

    assert!(
        counts.get("Clear").copied().unwrap_or(0) > 0,
        "sanity: a real map should have some Clear terrain"
    );
    assert!(
        counts.get("Water").copied().unwrap_or(0) > 0,
        "scg01ea (a coastal Snow-theater map) should resolve at least one cell to LAND_WATER \
         from its real template art — if this ever reads 0, either the map changed or the \
         ColorMap/ land_from_control derivation regressed"
    );
    assert!(
        counts.get("Rock").copied().unwrap_or(0) > 0,
        "scg01ea should resolve at least one cell to LAND_ROCK (cliff/mountain template) from \
         its real template art — the specific 'known cliff template icon -> LAND_ROCK' spot \
         check the M7.6 plan calls for, exercised against real map content rather than a single \
         hand-picked template id (more robust to template-id renumbering across theaters)"
    );
}

/// Every `Water`/`Rock` cell found above must actually be impassable to
/// every ground locomotor once run through `build_passability_masks` — ties
/// the template-resolution half (this file) back to the rules.ini-driven
/// passability half (`ra-sim/tests/landtype_suite.rs`), on real map content.
#[test]
fn real_scg01ea_water_and_rock_cells_are_impassable_in_the_built_passability_masks() {
    let Some(terrain) = load_terrain() else {
        return;
    };
    let dir = support::assets_dir();
    let redalert_bytes = match std::fs::read(dir.join("redalert.mix")) {
        Ok(b) => b,
        Err(_) => {
            eprintln!("SKIP: redalert.mix not found (needed for rules.ini)");
            return;
        }
    };
    let redalert = ra_formats::mix::MixArchive::parse(&redalert_bytes).expect("parse redalert.mix");
    let local = redalert.open_nested("local.mix").expect("open local.mix");
    let rules_bytes = local.get("rules.ini").expect("rules.ini present");
    let rules = ra_formats::ini::Ini::parse(&String::from_utf8_lossy(rules_bytes));

    let (foot, track, wheel) =
        ra_client::terrain::build_passability_masks(&terrain.scenario, &terrain.tiles, &rules);

    let mut checked_water = 0;
    let mut checked_rock = 0;
    for cy in 0..ra_data::scenario::MAP_CELL_H {
        for cx in 0..ra_data::scenario::MAP_CELL_W {
            let cell = terrain.scenario.cell(cx, cy);
            let land = terrain.tiles.land_type(cell.template, cell.icon);
            let i = (cy * ra_data::scenario::MAP_CELL_W + cx) as usize;
            match land {
                LandType::Water => {
                    checked_water += 1;
                    assert!(!foot[i], "Water cell ({cx},{cy}) should be Foot-impassable");
                    assert!(
                        !track[i],
                        "Water cell ({cx},{cy}) should be Track-impassable"
                    );
                    assert!(
                        !wheel[i],
                        "Water cell ({cx},{cy}) should be Wheel-impassable"
                    );
                }
                LandType::Rock => {
                    checked_rock += 1;
                    assert!(!foot[i], "Rock cell ({cx},{cy}) should be Foot-impassable");
                    assert!(
                        !track[i],
                        "Rock cell ({cx},{cy}) should be Track-impassable"
                    );
                    assert!(
                        !wheel[i],
                        "Rock cell ({cx},{cy}) should be Wheel-impassable"
                    );
                }
                _ => {}
            }
        }
    }
    assert!(
        checked_water > 0,
        "sanity: should have found Water cells to check"
    );
    assert!(
        checked_rock > 0,
        "sanity: should have found Rock cells to check"
    );
}
