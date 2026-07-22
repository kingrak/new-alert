//! Marquee superweapon arc — the **player fire UI**, driven end-to-end through the
//! `AppCore` seam (handle/update/compose), no window and no assets required.
//!
//! Proves the playability gap is closed: a player who owns a ready superweapon can
//! (1) see it charge and go READY in the sidebar, (2) click its indicator to enter
//! target-select, and (3) fire it with a tactical click — the nuclear strike at a
//! cell, the iron curtain over a unit, and the chronosphere as a two-click warp —
//! emitting `Command::FireSuperWeapon` through the normal pipeline and producing
//! the sim effect (nuke strikes + detonation, iron_curtain countdown, teleport)
//! plus the cosmetic feedback (mushroom cluster, warp flash, EVA/SFX cues).
//!
//! Also pins that the fire-mode UI + SW effects are **sim-inert**: the same command
//! script with the cosmetic layer on vs off yields an identical sim-hash chain.

use ra_client::appcore::{AppCore, SoundEvent};
use ra_client::input::{InputEvent, MouseButton};
use ra_client::unit_render::{SpriteFrame, UnitSprite};
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    BuildingProto, Catalog, Command, EconRules, MoveStats, Passability, SuperKind, Target,
    WarheadProfile, WeaponProfile, World,
};

const CELL_PIXELS: i32 = 24;
const TICK_MS: u32 = 67;

fn weapon() -> WeaponProfile {
    WeaponProfile {
        damage: 30,
        rof: 20,
        range: 4096,
        proj_speed: 255,
        proj_rot: 0,
        invisible: false,
        instant: false,
        warhead: WarheadProfile {
            spread: 1000,
            verses: [65536; 5],
        },
        warhead_ap: false,
        arcing: false,
        ballistic_scatter: 256,
        homing_scatter: 512,
        min_damage: 1,
        max_damage: 1000,
    }
}

fn bproto(name: &str) -> BuildingProto {
    BuildingProto {
        name: name.to_string(),
        foot_w: 2,
        foot_h: 2,
        max_health: 400,
        armor: 0,
        power: 100,
        cost: 2500,
        prereq: vec![],
        is_refinery: false,
        is_construction_yard: false,
        is_war_factory: false,
        is_barracks: false,
        free_harvester_unit: None,
        sight: 6,
        sprite_id: 0,
        weapon: None,
        has_turret: false,
        charges: false,
        is_wall: false,
        storage: 0,
    }
}

/// A tiny non-empty explosion sprite so the mushroom cluster + death fireballs
/// actually spawn (a frameless sprite would no-op).
fn fake_explosion() -> UnitSprite {
    UnitSprite {
        frames: vec![SpriteFrame {
            width: 24,
            height: 24,
            pixels: vec![7u8; 24 * 24],
        }],
    }
}

const B_MSLO: u32 = 0;
const B_IRON: u32 = 1;
const B_PDOX: u32 = 2;
const U_TANK: u32 = 0;

/// Build a world where **house 1** (the player) owns all three superweapon
/// structures, plus two own tanks (iron/chrono targets) and an enemy cluster at
/// the nuke aim point. Returns the world and the two target unit handles.
fn build_world() -> (World, ra_sim::Handle, ra_sim::Handle) {
    let mut world = World::new(Passability::all_passable(), 0x5B_1234);
    world.set_catalog(Catalog {
        buildings: vec![bproto("MSLO"), bproto("IRON"), bproto("PDOX")],
        units: vec![],
        econ: EconRules::default(),
    });
    world.init_houses(3, 10_000);
    // Player superweapon structures (house 1), off to the side.
    world.spawn_building(B_MSLO, 1, CellCoord::new(2, 2));
    world.spawn_building(B_IRON, 1, CellCoord::new(6, 2));
    world.spawn_building(B_PDOX, 1, CellCoord::new(10, 2));
    let stats = MoveStats {
        max_speed: 30,
        rot: 12,
    };
    // Own tanks: iron-curtain + chronosphere targets.
    let iron_tgt = world.spawn_unit(U_TANK, 1, CellCoord::new(10, 8), Facing(0), 200, stats);
    world.set_unit_combat(iron_tgt, 0, Some(weapon()), false);
    let chrono_tgt = world.spawn_unit(U_TANK, 1, CellCoord::new(13, 8), Facing(0), 200, stats);
    world.set_unit_combat(chrono_tgt, 0, Some(weapon()), false);
    // Enemy infantry cluster at the nuke ground-zero (light → the blast kills them).
    for dx in -1..=1 {
        for dy in -1..=1 {
            let h = world.spawn_unit(
                U_TANK,
                2,
                CellCoord::new(6 + dx, 12 + dy),
                Facing(0),
                40,
                stats,
            );
            if let Some(u) = world.units.get_mut(h) {
                u.make_infantry(((dx + 1) * 3 + (dy + 1)) as u8 % 5);
            }
        }
    }
    (world, iron_tgt, chrono_tgt)
}

fn core_from(world: World, art: bool) -> AppCore {
    let raster = ra_client::compositor::IndexedImage::filled(
        64 * CELL_PIXELS as u32,
        32 * CELL_PIXELS as u32,
        0,
    );
    let palette: ra_client::compositor::Palette = [[40, 40, 48]; 256];
    let mut core = AppCore::with_sim(raster, palette, world, Vec::new(), Vec::new());
    core.enable_sidebar(1, Vec::new());
    core.handle(InputEvent::Resize {
        width: 640,
        height: 400,
    });
    core.set_camera(0.0, 0.0);
    if art {
        core.set_effect_art(vec![fake_explosion()], Vec::new());
    }
    core
}

fn click(core: &mut AppCore, x: i32, y: i32) {
    core.handle(InputEvent::MouseMoved { x, y });
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x,
        y,
    });
    core.handle(InputEvent::MouseUp {
        button: MouseButton::Left,
        x,
        y,
    });
}

/// Viewport pixel at the centre of cell `(cx, cy)` with the camera at the origin.
fn cell_px(cx: i32, cy: i32) -> (i32, i32) {
    (
        cx * CELL_PIXELS + CELL_PIXELS / 2,
        cy * CELL_PIXELS + CELL_PIXELS / 2,
    )
}

/// The centre of the ready-indicator button for `kind` in the sidebar (three SW
/// owned → Nuclear/Iron/Chrono stacked at the bottom, 22px tall each).
fn sw_button_px(core: &AppCore, kind: SuperKind) -> (i32, i32) {
    let idx = match kind {
        SuperKind::Nuclear => 0,
        SuperKind::IronCurtain => 1,
        SuperKind::Chronosphere => 2,
    };
    let x = core.tactical_width() as i32 + 20;
    let y = 400 - (3 - idx) * 22 + 11;
    (x, y)
}

fn charge_all(core: &mut AppCore) {
    let w = core.world_mut();
    w.force_charge_superweapon(1, SuperKind::Nuclear);
    w.force_charge_superweapon(1, SuperKind::IronCurtain);
    w.force_charge_superweapon(1, SuperKind::Chronosphere);
}

#[test]
fn player_fires_all_three_superweapons_through_the_ui() {
    let (world, iron_tgt, chrono_tgt) = build_world();
    let mut core = core_from(world, true);

    // One tick so the superweapons sync into existence (present + charging).
    core.update(TICK_MS);
    assert!(
        !core.world().superweapon_ready(1, SuperKind::Nuclear),
        "SW starts charging, not ready"
    );
    let permille = core
        .world()
        .superweapon_charge_permille(1, SuperKind::Nuclear)
        .expect("player owns the nuke");
    assert!(
        (0..1000).contains(&permille),
        "the ready-clock reflects a partial charge ({permille}‰)"
    );

    // Force them ready (skip the multi-minute recharge) and prove the clock reads
    // fully charged now.
    charge_all(&mut core);
    assert_eq!(
        core.world()
            .superweapon_charge_permille(1, SuperKind::Nuclear),
        Some(1000),
        "the ready-clock now reads 100%"
    );

    // ---------- Nuclear: click the ready button, then a target cell ----------
    let (bx, by) = sw_button_px(&core, SuperKind::Nuclear);
    click(&mut core, bx, by);
    assert_eq!(
        core.superweapon_fire_mode(),
        Some(SuperKind::Nuclear),
        "clicking the ready NUKE indicator armed target-select"
    );
    let (tx, ty) = cell_px(6, 12);
    click(&mut core, tx, ty);
    assert_eq!(
        core.superweapon_fire_mode(),
        None,
        "fire mode exits on fire"
    );
    let cmds = core.drain_commands();
    assert!(
        cmds.iter().any(|c| matches!(
            c,
            Command::FireSuperWeapon {
                kind: SuperKind::Nuclear,
                target: Target::Cell(cell),
                ..
            } if *cell == CellCoord::new(6, 12)
        )),
        "a nuclear FireSuperWeapon at cell (6,12) was emitted: {cmds:?}"
    );

    // Advance until the strike falls and detonates; collect the cues.
    let mut saw_launch = false;
    let mut saw_impact = false;
    let mut detonated = false;
    let mut effects_after_detonation = 0;
    for _ in 0..40 {
        let had = core.world().nuke_strikes().len();
        core.update(TICK_MS);
        for cue in core.drain_sounds() {
            match cue {
                SoundEvent::NukeLaunch => saw_launch = true,
                SoundEvent::NukeImpact => saw_impact = true,
                _ => {}
            }
        }
        if had > 0 && core.world().nuke_strikes().is_empty() {
            detonated = true;
            effects_after_detonation = core.cosmetic_effect_count();
            break;
        }
    }
    assert!(saw_launch, "EVA nuke-launch cue queued");
    assert!(detonated, "the nuke fell and detonated");
    assert!(saw_impact, "nuke impact SFX queued");
    assert!(
        effects_after_detonation > 0,
        "the detonation spawned the mushroom explosion cluster"
    );

    // ---------- Iron curtain: click the ready button, then a friendly unit ----------
    charge_all(&mut core);
    let (bx, by) = sw_button_px(&core, SuperKind::IronCurtain);
    click(&mut core, bx, by);
    assert_eq!(
        core.superweapon_fire_mode(),
        Some(SuperKind::IronCurtain),
        "armed iron-curtain target-select"
    );
    let (ux, uy) = cell_px(10, 8);
    click(&mut core, ux, uy);
    assert_eq!(core.superweapon_fire_mode(), None);
    let cmds = core.drain_commands();
    assert!(
        cmds.iter().any(|c| matches!(
            c,
            Command::FireSuperWeapon {
                kind: SuperKind::IronCurtain,
                ..
            }
        )),
        "an iron-curtain FireSuperWeapon was emitted: {cmds:?}"
    );
    let mut saw_iron = false;
    for _ in 0..3 {
        core.update(TICK_MS);
        if core.drain_sounds().contains(&SoundEvent::IronCurtain) {
            saw_iron = true;
        }
    }
    assert!(
        core.world()
            .units
            .get(iron_tgt)
            .map(|u| u.iron_curtain > 0)
            .unwrap_or(false),
        "the targeted tank is iron-curtained (invulnerable)"
    );
    assert!(saw_iron, "iron-curtain SFX queued");

    // ---------- Chronosphere: two-click warp (unit, then destination) ----------
    charge_all(&mut core);
    let (bx, by) = sw_button_px(&core, SuperKind::Chronosphere);
    click(&mut core, bx, by);
    assert_eq!(
        core.superweapon_fire_mode(),
        Some(SuperKind::Chronosphere),
        "armed chrono target-select"
    );
    // First click: pick the unit (stays armed, source recorded).
    let (sx, sy) = cell_px(13, 8);
    click(&mut core, sx, sy);
    assert_eq!(
        core.superweapon_fire_mode(),
        Some(SuperKind::Chronosphere),
        "chrono stays armed after the first (unit) click"
    );
    assert!(
        core.chrono_pending_source().is_some(),
        "the warp source unit was recorded by the first click"
    );
    // Second click: the destination cell → fire and exit.
    let (dx, dy) = cell_px(18, 14);
    click(&mut core, dx, dy);
    assert_eq!(
        core.superweapon_fire_mode(),
        None,
        "chrono fires on 2nd click"
    );
    let cmds = core.drain_commands();
    assert!(
        cmds.iter()
            .any(|c| matches!(c, Command::FireSuperWeapon { kind: SuperKind::Chronosphere, dest: Some(d), .. } if *d == CellCoord::new(18, 14))),
        "a chronosphere FireSuperWeapon to (18,14) was emitted: {cmds:?}"
    );
    let mut saw_chrono = false;
    for _ in 0..3 {
        core.update(TICK_MS);
        if core.drain_sounds().contains(&SoundEvent::Chronosphere) {
            saw_chrono = true;
        }
    }
    assert_eq!(
        core.world().units.get(chrono_tgt).map(|u| u.cell()),
        Some(CellCoord::new(18, 14)),
        "the chronosphere warped the tank to the destination"
    );
    assert!(saw_chrono, "chronosphere SFX queued");
}

#[test]
fn right_click_cancels_superweapon_fire_mode() {
    let (world, _, _) = build_world();
    let mut core = core_from(world, false);
    core.update(TICK_MS);
    charge_all(&mut core);
    core.arm_superweapon(SuperKind::Nuclear);
    assert_eq!(core.superweapon_fire_mode(), Some(SuperKind::Nuclear));
    assert!(core.action_mode_armed());
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Right,
        x: 100,
        y: 100,
    });
    assert_eq!(core.superweapon_fire_mode(), None, "right-click cancels");
    assert!(!core.action_mode_armed());
}

#[test]
fn arming_an_unready_superweapon_is_a_no_op() {
    let (world, _, _) = build_world();
    let mut core = core_from(world, false);
    core.update(TICK_MS); // present but still charging (not forced ready)
    core.arm_superweapon(SuperKind::Nuclear);
    assert_eq!(
        core.superweapon_fire_mode(),
        None,
        "cannot arm a superweapon that is still charging"
    );
}

/// The fire-mode UI + SW effects are cosmetic: the SAME command script with the
/// cosmetic layer ON (art installed, `compose_game`/`drain_sounds` exercised) vs
/// OFF must yield an identical sim-hash chain.
#[test]
fn superweapon_effects_are_sim_inert() {
    let build = || {
        let (world, iron_tgt, chrono_tgt) = build_world();
        (world, iron_tgt, chrono_tgt)
    };
    let (w_on, on_iron, on_chrono) = build();
    let (w_off, off_iron, off_chrono) = build();
    let mut on = core_from(w_on, true);
    let mut off = core_from(w_off, false);

    // Identical FireSuperWeapon script injected into both cores (real sim input).
    for (core, iron, chrono) in [
        (&mut on, on_iron, on_chrono),
        (&mut off, off_iron, off_chrono),
    ] {
        core.update(TICK_MS);
        {
            let w = core.world_mut();
            w.force_charge_superweapon(1, SuperKind::Nuclear);
            w.force_charge_superweapon(1, SuperKind::IronCurtain);
            w.force_charge_superweapon(1, SuperKind::Chronosphere);
        }
        core.inject_command(Command::FireSuperWeapon {
            house: 1,
            kind: SuperKind::Nuclear,
            target: Target::Cell(CellCoord::new(6, 12)),
            dest: None,
        });
        core.inject_command(Command::FireSuperWeapon {
            house: 1,
            kind: SuperKind::IronCurtain,
            target: Target::Unit(iron),
            dest: None,
        });
        core.inject_command(Command::FireSuperWeapon {
            house: 1,
            kind: SuperKind::Chronosphere,
            target: Target::Unit(chrono),
            dest: Some(CellCoord::new(18, 14)),
        });
    }

    let mut h_on = Vec::new();
    let mut h_off = Vec::new();
    for _ in 0..60 {
        on.update(TICK_MS);
        let _ = on.compose_game(); // spawns/draws mushroom + warp + iron tint
        let _ = on.drain_sounds();
        h_on.push(on.sim_hash());

        off.update(TICK_MS);
        h_off.push(off.sim_hash());
    }
    assert_eq!(
        h_on, h_off,
        "superweapon fire-mode UI + effects perturbed the sim hash chain"
    );
    assert!(
        h_on.windows(2).any(|w| w[0] != w[1]),
        "sim never advanced — the SW determinism check was vacuous"
    );
}
