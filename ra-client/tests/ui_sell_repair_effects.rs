//! M7.9 art/feel pass — sell/repair **cursor + mode reminder + effects**
//! (sound + visual), driven end-to-end through the `AppCore` seam
//! (DESIGN.md §4.8 layer 1). Covers:
//!
//! - [`AppCore::cursor_kind`] resolves to the right sell/repair cursor over
//!   sellable / non-sellable / repairable / non-repairable / sidebar targets.
//! - Selling an own building queues the `Sell` (cash-turn) SFX + the
//!   `StructureSold` EVA line and spawns a cosmetic deconstruct effect.
//! - Toggling repair queues the `Repair` SFX and renders the pulsing wrench.
//! - Same script twice → identical sim-hash chain (effects are sim-inert).
//!
//! PNGs (sell-mode cursor + banner, repair wrench indicator) are written to the
//! scratchpad — with the **real** `SELL.SHP` / `SELECT.SHP` art installed when
//! the archives are present, and the synthetic fallback otherwise.

mod support;

use std::path::PathBuf;

use ra_client::appcore::{AppCore, CursorKind, SoundEvent};
use ra_client::input::{InputEvent, MouseButton};
use ra_client::unit_render::{SpriteFrame, UnitSprite};
use ra_sim::coords::CellCoord;
use ra_sim::Command;

const CELL_PX: i32 = 24;

fn scratch() -> PathBuf {
    PathBuf::from(
        "/tmp/claude-1000/-home-cshi-dev-game/f65beaba-9afb-445c-a6fd-47d2eb3dad49/scratchpad",
    )
}

/// A multi-frame fake buildup sprite so the sell **deconstruct** effect (reverse
/// buildup) actually spawns and paints (index 0 is transparent, others opaque).
fn fake_buildup() -> UnitSprite {
    UnitSprite {
        frames: (0..6)
            .map(|k| SpriteFrame {
                width: 3 * CELL_PX as u32,
                height: 3 * CELL_PX as u32,
                pixels: vec![(k + 1) as u8; (3 * CELL_PX * 3 * CELL_PX) as usize],
            })
            .collect(),
    }
}

/// `640×400` core: own PROC (house 1) at cell (2,2), enemy PROC (house 2) at
/// (10,2); own PROC damaged to half so repair has work to do. Fake buildup art
/// installed for the PROC type so the deconstruct anim spawns.
fn core_with_buildings() -> (AppCore, ra_sim::Handle, ra_sim::Handle) {
    let (mut world, _mcv) = support::synthetic_world_with_econ(0x5E11_9009, 6000);
    let own = world
        .spawn_building(support::ECON_B_PROC, 1, CellCoord::new(2, 2))
        .expect("own PROC");
    let enemy = world
        .spawn_building(support::ECON_B_PROC, 2, CellCoord::new(10, 2))
        .expect("enemy PROC");
    let max = world.buildings.get(own).unwrap().max_health;
    world.buildings.get_mut(own).unwrap().health = max / 2;

    let (raster, palette) = support::synthetic_fixture();
    let mut core = AppCore::with_sim(raster.clone(), *palette, world, Vec::new(), Vec::new());
    // Buildup art for the PROC type id, plus a 1-frame explosion so any fallback
    // effect also paints.
    let mut buildups: Vec<Option<UnitSprite>> = vec![None; (support::ECON_B_PROC + 1) as usize];
    buildups[support::ECON_B_PROC as usize] = Some(fake_buildup());
    core.set_effect_art(
        vec![UnitSprite {
            frames: vec![SpriteFrame {
                width: 16,
                height: 16,
                pixels: vec![7u8; 16 * 16],
            }],
        }],
        buildups,
    );
    core.enable_sidebar(1, support::econ_buildables());
    core.handle(InputEvent::Resize {
        width: 640,
        height: 400,
    });
    core.set_camera(0.0, 0.0);
    (core, own, enemy)
}

fn move_to(core: &mut AppCore, x: i32, y: i32) {
    core.handle(InputEvent::MouseMoved { x, y });
}
fn click(core: &mut AppCore, x: i32, y: i32) {
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x,
        y,
    });
}
fn cell_px(c: CellCoord) -> (i32, i32) {
    (c.x * CELL_PX + CELL_PX / 2, c.y * CELL_PX + CELL_PX / 2)
}

// ===========================================================================
// Cursor kind
// ===========================================================================

#[test]
fn cursor_kind_tracks_mode_and_hover_target() {
    let (mut core, _own, _enemy) = core_with_buildings();
    let (ownx, owny) = cell_px(CellCoord::new(3, 3)); // inside own PROC footprint
    let (enx, eny) = cell_px(CellCoord::new(11, 3)); // inside enemy PROC
    let (empx, empy) = cell_px(CellCoord::new(15, 12)); // empty ground, inside tactical strip

    // No mode → always Normal.
    move_to(&mut core, ownx, owny);
    assert_eq!(core.cursor_kind(), CursorKind::Normal);

    // Sell mode.
    core.toggle_sell_mode();
    move_to(&mut core, ownx, owny);
    assert_eq!(core.cursor_kind(), CursorKind::Sell, "own building → Sell");
    move_to(&mut core, enx, eny);
    assert_eq!(core.cursor_kind(), CursorKind::NoSell, "enemy → NoSell");
    move_to(&mut core, empx, empy);
    assert_eq!(core.cursor_kind(), CursorKind::NoSell, "empty → NoSell");
    // Over the sidebar strip → Normal.
    let sidebar_x = core.viewport_size().0 as i32 - 4;
    move_to(&mut core, sidebar_x, 40);
    assert_eq!(core.cursor_kind(), CursorKind::Normal, "sidebar → Normal");

    // Repair mode.
    core.toggle_repair_mode();
    move_to(&mut core, ownx, owny);
    assert_eq!(
        core.cursor_kind(),
        CursorKind::Repair,
        "own building → Repair"
    );
    move_to(&mut core, enx, eny);
    assert_eq!(core.cursor_kind(), CursorKind::NoRepair, "enemy → NoRepair");
}

// ===========================================================================
// Sell effect (sound + visual)
// ===========================================================================

#[test]
fn selling_queues_cash_and_eva_and_spawns_deconstruct() {
    let (mut core, own, _enemy) = core_with_buildings();
    core.drain_sounds();
    core.toggle_sell_mode();
    let (px, py) = cell_px(CellCoord::new(3, 3));
    move_to(&mut core, px, py);
    click(&mut core, px, py);
    assert_eq!(
        core.drain_commands(),
        vec![Command::Sell {
            house: 1,
            building: own
        }]
    );

    // Apply the tick: the building sells this tick, spawning the effect + cues.
    let before = core.cosmetic_effect_count();
    core.update(support::TICK_MS);
    assert!(core.world().buildings.get(own).is_none(), "building sold");
    let cues = core.drain_sounds();
    assert!(
        cues.contains(&SoundEvent::Sell),
        "cash-turn SFX queued: {cues:?}"
    );
    assert!(
        cues.contains(&SoundEvent::StructureSold),
        "EVA 'Structure sold' queued: {cues:?}"
    );
    assert!(
        !cues.contains(&SoundEvent::Explosion),
        "a sale must NOT play the combat-death explosion cue: {cues:?}"
    );
    assert!(
        core.cosmetic_effect_count() > before,
        "a deconstruct effect must spawn on sale"
    );
    // Composes without panic while the deconstruct anim plays.
    let _ = core.compose_game();
}

// ===========================================================================
// Repair effect (sound + visual)
// ===========================================================================

#[test]
fn toggling_repair_queues_sfx_and_renders_wrench() {
    let (mut core, _own, _enemy) = core_with_buildings();
    core.drain_sounds();
    core.toggle_repair_mode();
    let (px, py) = cell_px(CellCoord::new(3, 3));
    move_to(&mut core, px, py);
    click(&mut core, px, py);
    core.update(support::TICK_MS);
    let cues = core.drain_sounds();
    assert!(
        cues.contains(&SoundEvent::Repair),
        "repair SFX queued: {cues:?}"
    );

    // The building is now repairing → the wrench pulses over it. Prove it renders
    // by diffing the frame against the same frame with repair off (the only delta
    // is the wrench overlay + the mode banner, both localised).
    assert!(
        core.world().buildings.iter().any(|(_, b)| b.is_repairing),
        "a building should be repairing after the toggle"
    );
    let with_wrench = core.compose_game();
    // Clear repair state and disarm the mode → wrench + banner gone.
    for h in core
        .world()
        .buildings
        .iter()
        .map(|(h, _)| h)
        .collect::<Vec<_>>()
    {
        core.world_mut().buildings.get_mut(h).unwrap().is_repairing = false;
    }
    core.toggle_repair_mode();
    let without = core.compose_game();
    assert_ne!(
        with_wrench.pixels, without.pixels,
        "the repair wrench/banner must change the composited frame"
    );
}

// ===========================================================================
// Determinism: same sell+repair script twice → identical hash chain
// ===========================================================================

#[test]
fn sell_and_repair_script_is_deterministic() {
    let run = || {
        let (mut core, _own, _enemy) = core_with_buildings();
        core.toggle_repair_mode();
        let (px, py) = cell_px(CellCoord::new(3, 3));
        move_to(&mut core, px, py);
        click(&mut core, px, py); // repair toggle
        core.update(support::TICK_MS);
        core.toggle_sell_mode();
        click(&mut core, px, py); // sell
        let mut hashes = Vec::new();
        for _ in 0..12 {
            core.update(support::TICK_MS);
            let _ = core.compose_game(); // exercise the whole cosmetic path
            let _ = core.drain_sounds();
            hashes.push(core.sim_hash());
        }
        hashes
    };
    assert_eq!(run(), run(), "sell+repair script must be deterministic");
}

// ===========================================================================
// PNG artifacts (sell cursor + banner, repair wrench). Real art when present.
// ===========================================================================

fn assets_dir() -> Option<PathBuf> {
    let dir = std::env::var("RA_ASSETS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../assets"));
    (dir.join("main.mix").is_file() && dir.join("redalert.mix").is_file()).then_some(dir)
}

/// Install the real `SELL.SHP` (hires.mix) and `SELECT.SHP` (conquer.mix) art
/// onto `core` when the archives are available, so the PNGs show authentic art.
/// Returns whether real art was installed.
fn install_real_art(core: &mut AppCore) -> bool {
    use ra_formats::mix::MixArchive;
    let Some(dir) = assets_dir() else {
        return false;
    };
    let (Ok(main_b), Ok(ra_b)) = (
        std::fs::read(dir.join("main.mix")),
        std::fs::read(dir.join("redalert.mix")),
    ) else {
        return false;
    };
    let (Ok(main), Ok(ra)) = (MixArchive::parse(&main_b), MixArchive::parse(&ra_b)) else {
        return false;
    };
    let load = |mix: &MixArchive, name: &str| {
        mix.get(name)
            .and_then(|b| UnitSprite::from_shp_bytes(b).ok())
    };
    let mut installed = false;
    if let Ok(conquer) = main.open_nested("conquer.mix") {
        if let Some(sel) = load(&conquer, "SELECT.SHP") {
            core.set_indicator_art(Some(sel));
            installed = true;
        }
    }
    if let Ok(hires) = ra.open_nested("hires.mix") {
        core.set_mode_button_art(load(&hires, "SELL.SHP"), load(&hires, "REPAIR.SHP"));
    }
    installed
}

fn dump(core: &AppCore, name: &str) {
    let f = core.compose_game();
    let bytes = ra_client::png::encode_rgba(f.width, f.height, &f.pixels);
    let _ = std::fs::create_dir_all(scratch());
    std::fs::write(scratch().join(name), bytes).expect("write png");
}

#[test]
fn dump_sell_and_repair_pngs() {
    // --- Sell-mode cursor + banner over an own building ---
    let (mut core, _own, _enemy) = core_with_buildings();
    let real = install_real_art(&mut core);
    core.toggle_sell_mode();
    let (px, py) = cell_px(CellCoord::new(3, 3));
    move_to(&mut core, px, py);
    assert_eq!(core.cursor_kind(), CursorKind::Sell);
    dump(
        &core,
        if real {
            "sell_cursor_banner_real.png"
        } else {
            "sell_cursor_banner_synth.png"
        },
    );

    // --- Repair wrench indicator over a repairing building ---
    let (mut core, own, _enemy) = core_with_buildings();
    let real = install_real_art(&mut core);
    core.world_mut()
        .buildings
        .get_mut(own)
        .unwrap()
        .is_repairing = true;
    core.toggle_repair_mode();
    let (rx, ry) = cell_px(CellCoord::new(11, 3)); // point cursor at the enemy → NoRepair glyph
    move_to(&mut core, rx, ry);
    dump(
        &core,
        if real {
            "repair_wrench_real.png"
        } else {
            "repair_wrench_synth.png"
        },
    );
}
