//! PNG evidence for the **player superweapon fire UI** (marquee arc P1/P2),
//! captured through the real `AppCore` game surface with real assets:
//!
//!   1. `sw_ready_clock.png`   — the sidebar ready/charge indicators (recharge
//!      clock + READY state) for the three owned superweapons.
//!   2. `nuke_fire_mode.png`   — the nuke target-select mode: the reminder banner
//!      + the targeting reticle cursor over the tactical map.
//!   3. `nuke_mushroom.png`    — the mushroom explosion cluster at a nuke strike.
//!   4. `iron_curtain_glow.png`— a unit glowing under the iron-curtain tint.
//!   5. `chrono_warp.png`      — a unit warped by the chronosphere (warp flash).
//!
//! Skips cleanly when the real assets aren't present.

mod support;

use ra_client::appcore::AppCore;
use ra_client::input::InputEvent;
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{Command, SuperKind, Target};
use std::path::PathBuf;

fn scratch() -> PathBuf {
    PathBuf::from(
        "/tmp/claude-1000/-home-cshi-dev-game/f65beaba-9afb-445c-a6fd-47d2eb3dad49/scratchpad",
    )
}

fn dump(core: &AppCore, name: &str) {
    let f = core.compose_game();
    let bytes = ra_client::png::encode_rgba(f.width, f.height, &f.pixels);
    std::fs::write(scratch().join(name), bytes).expect("write png");
    eprintln!("  wrote {name}");
}

fn building_id(core: &AppCore, name: &str) -> u32 {
    core.world()
        .catalog
        .buildings
        .iter()
        .position(|b| b.name.eq_ignore_ascii_case(name))
        .unwrap_or_else(|| panic!("{name} in catalog")) as u32
}

fn tank_id(core: &AppCore) -> u32 {
    core.world()
        .catalog
        .units
        .iter()
        .position(|u| u.name.eq_ignore_ascii_case("2TNK") || u.name.eq_ignore_ascii_case("3TNK"))
        .expect("a tank in the catalog") as u32
}

const CELL: i32 = 24;

#[test]
fn superweapon_fire_ui_render_evidence_png() {
    if !support::real_assets_available() {
        eprintln!("SKIP: real assets not found");
        return;
    }
    let econ =
        match ra_client::assets::load_econ_from_dir(&support::assets_dir(), "scg05ea.ini", 20000) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("SKIP: could not load econ game: {e}");
                return;
            }
        };
    let own = econ.controlled;
    let enemy = if own == 1 { 2 } else { 1 };
    let mut core = econ.core;
    core.handle(InputEvent::Resize {
        width: 1024,
        height: 768,
    });

    let mslo = building_id(&core, "MSLO");
    let iron = building_id(&core, "IRON");
    let pdox = building_id(&core, "PDOX");
    let tank = tank_id(&core);
    let tproto = core.world().catalog.unit(tank).cloned().unwrap();

    // Own the three superweapon structures; reveal the working area.
    {
        let w = core.world_mut();
        for dy in -6..14 {
            for dx in -6..24 {
                w.reveal_shroud(own, CellCoord::new(46 + dx, 44 + dy), 8);
            }
        }
        w.spawn_building(mslo, own, CellCoord::new(46, 40));
        w.spawn_building(iron, own, CellCoord::new(50, 40));
        w.spawn_building(pdox, own, CellCoord::new(54, 40));
    }
    core.update(100); // >=1 tick: sync the superweapons into existence (charging)
    core.set_camera((40 * CELL) as f32, (36 * CELL) as f32);

    // ---- Scene 1: the sidebar ready-clock (force ready → READY state) ----
    core.world_mut()
        .force_charge_superweapon(own, SuperKind::Nuclear);
    core.world_mut()
        .force_charge_superweapon(own, SuperKind::IronCurtain);
    core.world_mut()
        .force_charge_superweapon(own, SuperKind::Chronosphere);
    dump(&core, "sw_ready_clock.png");

    // ---- Scene 2: nuke target-select banner + reticle cursor ----
    core.arm_superweapon(SuperKind::Nuclear);
    core.handle(InputEvent::MouseMoved { x: 300, y: 360 });
    dump(&core, "nuke_fire_mode.png");
    // Cancel the target-select mode so the later scenes don't carry the banner.
    core.handle(InputEvent::MouseDown {
        button: ra_client::input::MouseButton::Right,
        x: 300,
        y: 360,
    });

    // ---- Scene 3: fire the nuke at an enemy cluster → mushroom cluster ----
    // A cluster of light enemy infantry at ground zero so the blast is lethal and
    // the death diff + the dedicated mushroom cluster both fire.
    {
        let w = core.world_mut();
        for dx in -1..=1 {
            for dy in -1..=1 {
                let h = w.spawn_unit(
                    tproto.sprite_id,
                    enemy,
                    CellCoord::new(52 + dx, 50 + dy),
                    Facing(0),
                    60,
                    tproto.stats,
                );
                if let Some(u) = w.units.get_mut(h) {
                    u.make_infantry(((dx + 1) * 3 + (dy + 1)) as u8 % 5);
                }
            }
        }
    }
    core.world_mut().tick(&[Command::FireSuperWeapon {
        house: own,
        kind: SuperKind::Nuclear,
        target: Target::Cell(CellCoord::new(52, 50)),
        dest: None,
    }]);
    core.set_camera((46 * CELL) as f32, (44 * CELL) as f32);
    for _ in 0..40 {
        let had = core.world().nuke_strikes().len();
        core.update(66);
        if had > 0 && core.world().nuke_strikes().is_empty() {
            break;
        }
    }
    dump(&core, "nuke_mushroom.png");

    // ---- Scene 4: iron-curtain glow on a unit ----
    let curt = {
        let w = core.world_mut();
        let h = w.spawn_unit(
            tproto.sprite_id,
            own,
            CellCoord::new(58, 48),
            Facing(0),
            tproto.max_health,
            tproto.stats,
        );
        w.set_unit_combat(h, tproto.armor, tproto.weapon, tproto.has_turret);
        h
    };
    core.world_mut()
        .force_charge_superweapon(own, SuperKind::IronCurtain);
    core.world_mut().tick(&[Command::FireSuperWeapon {
        house: own,
        kind: SuperKind::IronCurtain,
        target: Target::Unit(curt),
        dest: None,
    }]);
    core.update(66);
    core.set_camera((52 * CELL) as f32, (42 * CELL) as f32);
    assert!(
        core.world()
            .units
            .get(curt)
            .map(|u| u.iron_curtain > 0)
            .unwrap_or(false),
        "unit is iron-curtained"
    );
    dump(&core, "iron_curtain_glow.png");

    // ---- Scene 5: chronosphere warp ----
    let warp = {
        let w = core.world_mut();
        let h = w.spawn_unit(
            tproto.sprite_id,
            own,
            CellCoord::new(60, 50),
            Facing(0),
            tproto.max_health,
            tproto.stats,
        );
        w.set_unit_combat(h, tproto.armor, tproto.weapon, tproto.has_turret);
        h
    };
    core.world_mut()
        .force_charge_superweapon(own, SuperKind::Chronosphere);
    core.world_mut().tick(&[Command::FireSuperWeapon {
        house: own,
        kind: SuperKind::Chronosphere,
        target: Target::Unit(warp),
        dest: Some(CellCoord::new(64, 54)),
    }]);
    core.update(66);
    assert_eq!(
        core.world().units.get(warp).map(|u| u.cell()),
        Some(CellCoord::new(64, 54)),
        "unit warped by the chronosphere"
    );
    core.set_camera((56 * CELL) as f32, (46 * CELL) as f32);
    dump(&core, "chrono_warp.png");
}
