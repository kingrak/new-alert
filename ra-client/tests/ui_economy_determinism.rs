//! Real-map (skip-clean) economy determinism check — M5 coverage item 3
//! ("Economy determinism suite ... synthetic + real-map (skip-clean)").
//!
//! `ra-sim/src/world.rs`'s own `m5_tests` module already covers the
//! synthetic-catalog half of this (`full_economy_loop_same_seed_twice_gives_identical_hash_chains`,
//! `full_economy_loop_command_log_replay_matches_live_run`). This file is the
//! real-map half, driven through the actual client stack (`ra-client`'s
//! `build_content` reading real `rules.ini`, real footprints, real ore from
//! the scenario overlay) via `AppCore`'s public seam.
//!
//! **Structural note.** `ra-client/src/bin/ra-client.rs`'s `econ` subcommand
//! (`cmd_econ`/`drive_econ`) already runs this exact deploy→build→harvest→
//! produce loop twice against real assets and compares the two hash chains —
//! but that is a human-run CLI verification tool (`cargo run --bin
//! ra-client -- econ`), not something `cargo test` exercises. Nothing in CI
//! would catch a determinism regression here. This file is the automated
//! equivalent of that check: the driving logic below is a deliberately
//! close port of `drive_econ`'s script (minus PNG dumping and the prose
//! report), so it inherits the same "actually representative of the real
//! game" property, now pinned as `cargo test` coverage.

mod support;

use ra_client::appcore::{AppCore, SidebarItem};
use ra_client::assets::{self, build_content, find_base_start, load_from_bytes, EconGame};
use ra_client::input::{InputEvent, Key, MouseButton};
use ra_client::terrain::rasterize;
use ra_data::house::{
    build_house_remaps, house_from_name, identity_remap, RemapTable, HOUSE_COUNT,
};
use ra_formats::cps::Cps;
use ra_formats::ini::Ini;
use ra_formats::mix::MixArchive;
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{BuildItem, OreField, Passability, World};

const CELL_PIXELS: i32 = 24;
const CREDITS: i32 = 8000;
// scg05eb has a large open temperate gold field near the centre (see
// `bin/ra-client.rs::cmd_econ`'s doc comment) — a reliable, fast-converging
// econ map, unlike scg01ea (M2/M3's reference scenario) which the other
// suites use for combat/movement.
const SCENARIO: &str = "scg05eb.ini";

fn sidebar_named(core: &AppCore, name: &str) -> Option<SidebarItem> {
    core.sidebar_items().into_iter().find(|i| i.name == name)
}

fn building_id(item: BuildItem) -> Option<u32> {
    match item {
        BuildItem::Building(id) => Some(id),
        _ => None,
    }
}

/// Scan cells around the controlled house's construction yard for the first
/// footprint top-left where `building_id` is a legal placement.
fn find_placement(core: &AppCore, house: u8, id: u32) -> Option<CellCoord> {
    let anchor = core
        .world()
        .buildings
        .iter()
        .find(|(_, b)| b.house == house && b.is_construction_yard)
        .or_else(|| {
            core.world()
                .buildings
                .iter()
                .find(|(_, b)| b.house == house)
        })
        .map(|(_, b)| b.cell)?;
    for r in 1..12 {
        for dy in -r..=r {
            for dx in -r..=r {
                let c = CellCoord::new(anchor.x + dx, anchor.y + dy);
                if core.world().can_place_building(house, id, c) {
                    return Some(c);
                }
            }
        }
    }
    None
}

/// Step the sim `n` virtual frames (~1 tick each), recording the per-tick hash.
fn econ_step(core: &mut AppCore, hashes: &mut Vec<u64>, n: u32) {
    for _ in 0..n {
        core.update(67);
        hashes.push(core.sim_hash());
    }
}

/// Step until `pred` holds or `max` ticks pass. Returns whether it held.
fn econ_wait<F: Fn(&AppCore) -> bool>(
    core: &mut AppCore,
    hashes: &mut Vec<u64>,
    max: u32,
    pred: F,
) -> bool {
    for _ in 0..max {
        if pred(core) {
            return true;
        }
        core.update(67);
        hashes.push(core.sim_hash());
    }
    pred(core)
}

/// Build one structure end to end: start production, wait for it to finish,
/// then place it at a found cell.
fn build_structure(
    core: &mut AppCore,
    hashes: &mut Vec<u64>,
    house: u8,
    name: &str,
) -> Result<CellCoord, String> {
    let item = sidebar_named(core, name).ok_or_else(|| format!("{name} not in sidebar"))?;
    if !item.buildable {
        return Err(format!("{name} not buildable (prereqs/factory/funds)"));
    }
    core.start_production(item.item);
    let ready = econ_wait(core, hashes, 4000, |c| {
        sidebar_named(c, name).map(|i| i.ready).unwrap_or(false)
    });
    if !ready {
        return Err(format!("{name} never completed"));
    }
    let id = building_id(item.item).ok_or("not a building")?;
    let cell = find_placement(core, house, id).ok_or_else(|| format!("no spot for {name}"))?;
    core.begin_placement(id);
    core.place_building(id, cell);
    econ_step(core, hashes, 2);
    let placed = core
        .world()
        .buildings
        .iter()
        .any(|(_, b)| b.house == house && b.cell == cell);
    if !placed {
        return Err(format!(
            "{name} placement rejected at ({},{})",
            cell.x, cell.y
        ));
    }
    Ok(cell)
}

/// Drive the whole M5 economy loop -- deploy the MCV, build+place POWR then
/// PROC (spawns the free harvester), wait for a harvest+unload, build+place
/// WEAP, then produce a 2TNK -- through `AppCore`'s real `handle`/`update`
/// seam, exactly mirroring `bin/ra-client.rs`'s `drive_econ` script. Returns
/// the per-tick sim-hash chain plus the final `AppCore` (so callers can
/// inspect end state, e.g. the final RNG seed), or an error describing where
/// the script got stuck (a hard failure, not a silent skip -- unlike asset
/// absence).
fn drive_econ_script(game: EconGame) -> Result<(Vec<u64>, AppCore), String> {
    let EconGame {
        mut core,
        controlled,
        start_cell,
        ..
    } = game;
    let mut hashes: Vec<u64> = Vec::new();

    let (vw, vh) = (1000u32, 720u32);
    core.handle(InputEvent::Resize {
        width: vw,
        height: vh,
    });
    let tw = core.tactical_width();
    core.set_camera(
        (start_cell.x * CELL_PIXELS) as f32 - tw as f32 / 2.0,
        (start_cell.y * CELL_PIXELS) as f32 - vh as f32 / 2.0,
    );

    // 1) Select the MCV (click it) and deploy it into a construction yard.
    let r = core.camera_rect();
    let mx = (start_cell.x * CELL_PIXELS + CELL_PIXELS / 2) as i64 - r.x;
    let my = (start_cell.y * CELL_PIXELS + CELL_PIXELS / 2) as i64 - r.y;
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x: mx as i32,
        y: my as i32,
    });
    core.handle(InputEvent::MouseUp {
        button: MouseButton::Left,
        x: mx as i32,
        y: my as i32,
    });
    core.handle(InputEvent::KeyDown(Key::Deploy));
    core.handle(InputEvent::KeyUp(Key::Deploy));
    econ_step(&mut core, &mut hashes, 3);
    let has_cy = core
        .world()
        .buildings
        .iter()
        .any(|(_, b)| b.house == controlled && b.is_construction_yard);
    if !has_cy {
        return Err("MCV failed to deploy".into());
    }

    // 2) Build + place POWR, then PROC (spawns the free harvester).
    build_structure(&mut core, &mut hashes, controlled, "POWR")
        .map_err(|e| format!("POWR: {e}"))?;
    build_structure(&mut core, &mut hashes, controlled, "PROC")
        .map_err(|e| format!("PROC: {e}"))?;

    // 3) Let the harvester mine and unload at least once.
    let credits_before_harvest = core.credits();
    let unloaded = econ_wait(&mut core, &mut hashes, 8000, |c| {
        c.credits() > credits_before_harvest
    });
    if !unloaded {
        return Err("harvester never banked any credits".into());
    }

    // 4) Build + place WEAP, then produce a 2TNK; confirm it spawns.
    build_structure(&mut core, &mut hashes, controlled, "WEAP")
        .map_err(|e| format!("WEAP: {e}"))?;
    let vehicles_before = core.world().units.len();
    let tnk = sidebar_named(&core, "2TNK").ok_or("2TNK not in sidebar")?;
    if !tnk.buildable {
        return Err("2TNK not buildable after WEAP".into());
    }
    core.start_production(tnk.item);
    let spawned = econ_wait(&mut core, &mut hashes, 4000, |c| {
        c.world().units.len() > vehicles_before
    });
    if !spawned {
        return Err("2TNK never spawned".into());
    }
    econ_step(&mut core, &mut hashes, 20);

    Ok((hashes, core))
}

#[test]
fn real_scg05eb_full_economy_loop_same_seed_twice_gives_identical_hash_chains() {
    if !support::real_assets_available() {
        eprintln!(
            "SKIP: real assets not found under {} (set RA_ASSETS_DIR or copy \
             main.mix/redalert.mix into assets/ to run this test)",
            support::assets_dir().display()
        );
        return;
    }
    let dir = support::assets_dir();
    let g1 = assets::load_econ_from_dir(&dir, SCENARIO, CREDITS)
        .expect("scg05eb.ini should load a playable econ game from present assets");
    let g2 = assets::load_econ_from_dir(&dir, SCENARIO, CREDITS)
        .expect("scg05eb.ini should load a playable econ game from present assets");

    let (hashes1, _core1) = drive_econ_script(g1).expect("first economy-loop run should complete");
    let (hashes2, _core2) = drive_econ_script(g2).expect("second economy-loop run should complete");

    assert_eq!(
        hashes1,
        hashes2,
        "two independent runs of the identical real-map economy script must give identical \
         per-tick hash chains (divergence at tick {})",
        hashes1
            .iter()
            .zip(&hashes2)
            .position(|(a, b)| a != b)
            .unwrap_or(hashes1.len())
    );
    assert!(!hashes1.is_empty());
}

#[test]
fn real_scg05eb_economy_loop_now_draws_the_sim_rng_via_ore_growth() {
    // *** SANCTIONED M6 PIN UPDATE (DESIGN §4.9 M6, item 2). ***
    //
    // Through M5 this test asserted the opposite: that an economy-only script
    // (deploy/build/harvest/produce, no combat) never drew the sim RNG. M6 ports
    // ore growth/spread, which the original drives from `MapClass::Logic` and
    // which *legitimately* consumes the sync RNG (`Random_Pick` at map.cpp:1367/
    // 1385 and cell.cpp:3182/3187 — see `ra-sim/src/world.rs::run_ore_growth`).
    // The econ loader now enables growth from rules.ini `OreGrows`/`OreSpreads`
    // (default yes), so the sim RNG legitimately advances during the run. This is
    // the ONE deliberately-updated pin; determinism itself is unchanged and still
    // proven by `..._same_seed_twice_gives_identical_hash_chains` above (same
    // seed → identical growth draws → identical chains).
    if !support::real_assets_available() {
        eprintln!(
            "SKIP: real assets not found under {} (set RA_ASSETS_DIR or copy \
             main.mix/redalert.mix into assets/ to run this test)",
            support::assets_dir().display()
        );
        return;
    }
    let dir = support::assets_dir();
    let game = assets::load_econ_from_dir(&dir, SCENARIO, CREDITS)
        .expect("scg05eb.ini should load a playable econ game from present assets");
    let seed_before = game.core.world().rng_seed();

    let (hashes, core_after) = drive_econ_script(game).expect("economy-loop run should complete");
    assert!(!hashes.is_empty());

    assert_ne!(
        core_after.world().rng_seed(),
        seed_before,
        "M6 ore growth/spread must draw the sync RNG over the course of an economy run"
    );
}

/// A near-exact copy of `assets::load_econ_from_bytes`, with one deliberate
/// difference: ore growth/spread is force-disabled instead of being read from
/// rules.ini. This exists ONLY to isolate the M6 pin-flip audit below --
/// `ore_growth_flags`/`ini_bool` are private to `ra-client::assets`, so there
/// is no public way to override them from outside the crate.
///
/// (ra-tester note: this is the ra-tester audit of the sanctioned pin-flip in
/// `real_scg05eb_economy_loop_now_draws_the_sim_rng_via_ore_growth` above --
/// see that test's doc comment. If `ra-client::assets` ever grows a public
/// `EconOptions`-style knob for this, this duplicate should be deleted in
/// favor of it; flagged to ra-coder in the M6 audit report.)
fn load_econ_from_bytes_growth_disabled(
    main_bytes: &[u8],
    redalert_bytes: &[u8],
    scenario_name: &str,
    starting_credits: i32,
) -> Result<EconGame, Box<dyn std::error::Error>> {
    let main = MixArchive::parse(main_bytes)?;
    let redalert = MixArchive::parse(redalert_bytes)?;

    let loaded = load_from_bytes(main_bytes, redalert_bytes, scenario_name)?;
    let raster = rasterize(&loaded.scenario, &loaded.tiles);
    let palette = loaded.palette;
    let scenario = loaded.scenario;

    let general = main.open_nested("general.mix")?;
    let ini_bytes = general
        .get(scenario_name)
        .ok_or_else(|| format!("scenario '{scenario_name}' not found"))?;
    let scen_ini = Ini::parse(&String::from_utf8_lossy(ini_bytes));

    let local = redalert.open_nested("local.mix")?;
    let rules = Ini::parse(&String::from_utf8_lossy(
        local.get("rules.ini").ok_or("rules.ini not found")?,
    ));
    let remaps: Vec<RemapTable> = match local.get("palette.cps") {
        Some(cps_bytes) => match Cps::parse(cps_bytes) {
            Ok(cps) => build_house_remaps(&cps).to_vec(),
            Err(_) => vec![identity_remap(); HOUSE_COUNT],
        },
        None => vec![identity_remap(); HOUSE_COUNT],
    };
    let controlled = scen_ini
        .get("Basic", "Player")
        .and_then(house_from_name)
        .unwrap_or(1);

    let conquer = main.open_nested("conquer.mix")?;
    let content = build_content(&rules, &conquer)?;

    let passable = ra_data::passability::build(&scenario);
    let grid = Passability::new(128, 128, passable);
    let mut world = World::new(grid, 0x1234_5678);
    world.set_catalog(content.catalog.clone());
    world.init_houses(HOUSE_COUNT, starting_credits);
    world.set_ore(OreField::from_overlay(128, 128, &scenario.overlay));
    // The one deliberate difference from `load_econ_from_bytes`: growth OFF
    // regardless of what rules.ini says.
    world.set_ore_growth(false, false);

    let near = CellCoord::new(
        scenario.map_x as i32 + scenario.map_width as i32 / 2,
        scenario.map_y as i32 + scenario.map_height as i32 / 2,
    );
    let (start_cell, ore_cell) = find_base_start(world.passability(), &world.ore, near);

    let mcv_proto = &content.catalog.units[0]; // U_MCV
    let mcv = world.spawn_unit(
        mcv_proto.sprite_id,
        controlled,
        start_cell,
        Facing(0),
        mcv_proto.max_health,
        mcv_proto.stats,
    );
    world.set_unit_max_health(mcv, mcv_proto.max_health);
    world.set_unit_combat(mcv, mcv_proto.armor, mcv_proto.weapon, mcv_proto.has_turret);
    world.set_unit_sight(mcv, mcv_proto.sight);

    let mut core = AppCore::with_sim(raster, palette, world, content.unit_sprites, remaps);
    core.set_building_sprites(content.building_sprites);
    core.enable_sidebar(controlled, content.buildables);

    Ok(EconGame {
        core,
        controlled,
        mcv,
        start_cell,
        ore_cell,
        mcv_unit_id: 0, // U_MCV
    })
}

#[test]
fn real_scg05eb_economy_loop_with_growth_disabled_never_consumes_the_sim_rng() {
    // *** ra-tester M6 audit test for the sanctioned pin flip above. ***
    //
    // The flip from assert_eq to assert_ne in
    // `real_scg05eb_economy_loop_now_draws_the_sim_rng_via_ore_growth` is only
    // justified if the RNG draw is *exactly* attributable to ore growth/spread
    // and not to some other M6 change riding along unnoticed (AI, shroud,
    // building death, sell, ...). This test runs the identical real-map
    // economy script with growth force-disabled (bypassing the rules.ini
    // OreGrows/OreSpreads read) and asserts the OLD invariant still holds:
    // seed unchanged. If this test ever fails, the M6 pin-flip's justification
    // is wrong -- something else started drawing the sim RNG on the
    // economy-only path.
    if !support::real_assets_available() {
        eprintln!(
            "SKIP: real assets not found under {} (set RA_ASSETS_DIR or copy \
             main.mix/redalert.mix into assets/ to run this test)",
            support::assets_dir().display()
        );
        return;
    }
    let dir = support::assets_dir();
    let main_bytes = std::fs::read(dir.join("main.mix")).expect("main.mix should read");
    let redalert_bytes = std::fs::read(dir.join("redalert.mix")).expect("redalert.mix should read");
    let game =
        load_econ_from_bytes_growth_disabled(&main_bytes, &redalert_bytes, SCENARIO, CREDITS)
            .expect("scg05eb.ini should load a playable econ game with growth disabled");
    let seed_before = game.core.world().rng_seed();

    let (hashes, core_after) =
        drive_econ_script(game).expect("economy-loop run (growth disabled) should complete");
    assert!(!hashes.is_empty());

    assert_eq!(
        core_after.world().rng_seed(),
        seed_before,
        "with ore growth/spread disabled, an economy-only script (deploy/build/harvest/produce, \
         no combat) must still never draw the sim RNG -- confirms the M6 pin flip in \
         `real_scg05eb_economy_loop_now_draws_the_sim_rng_via_ore_growth` is attributable \
         exactly to ore growth"
    );
}
