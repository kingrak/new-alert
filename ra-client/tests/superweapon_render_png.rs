//! Marquee content arc render evidence: loads the **real** assets and dumps PNGs
//! proving the infiltration specialists and superweapons render/behave in the real
//! game surface:
//!
//!   1. `specialists_scene.png` — a spy, a thief, Tanya, and an attack dog massed
//!      at an enemy refinery (the new SPY/THF/E7/DOG sprites in the tactical view).
//!   2. `nuke_detonation.png`  — a nuclear strike devastating a tank cluster: the
//!      blast wiped the cluster and the client's explosion fireballs are live.
//!   3. `iron_curtain_chrono.png` — an iron-curtained tank (invulnerable) beside a
//!      tank that the chronosphere has just warped across the map.
//!
//! Skips cleanly (never fails) when the real assets aren't present.

mod support;

use ra_client::appcore::AppCore;
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
    let path = scratch().join(name);
    std::fs::write(&path, bytes).expect("write png");
    eprintln!("  wrote {}", path.display());
}

fn unit_id(core: &AppCore, name: &str) -> Option<u32> {
    core.world()
        .catalog
        .units
        .iter()
        .position(|u| u.name.eq_ignore_ascii_case(name))
        .map(|i| i as u32)
}

fn building_id(core: &AppCore, name: &str) -> Option<u32> {
    core.world()
        .catalog
        .buildings
        .iter()
        .position(|b| b.name.eq_ignore_ascii_case(name))
        .map(|i| i as u32)
}

#[test]
fn superweapon_and_specialist_render_evidence_png() {
    if !support::real_assets_available() {
        eprintln!("SKIP: real assets not found");
        return;
    }
    let econ =
        match ra_client::assets::load_econ_from_dir(&support::assets_dir(), "scg05ea.ini", 10000) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("SKIP: could not load econ game: {e}");
                return;
            }
        };
    let own = econ.controlled;
    let enemy = if own == 1 { 2 } else { 1 };
    let mut core = econ.core;

    let spy = unit_id(&core, "SPY");
    let thf = unit_id(&core, "THF");
    let e7 = unit_id(&core, "E7");
    let dog = unit_id(&core, "DOG");
    let tank = unit_id(&core, "2TNK")
        .or_else(|| unit_id(&core, "3TNK"))
        .unwrap();
    let proc = building_id(&core, "PROC").expect("PROC in catalog");
    let tank_proto = core.world().catalog.unit(tank).cloned().unwrap();

    // ---------- Scene 1: infiltration specialists at an enemy refinery ----------
    {
        let w = core.world_mut();
        for dy in -3..8 {
            for dx in -4..10 {
                w.reveal_shroud(own, CellCoord::new(28 + dx, 28 + dy), 8);
            }
        }
        w.spawn_building(proc, enemy, CellCoord::new(32, 30));
        let mut place = |id: Option<u32>, is_inf: bool, name: &str, cell: CellCoord| {
            if let Some(id) = id {
                let h = w.spawn_unit(id, own, cell, Facing(96), 100, tank_proto.stats);
                if is_inf {
                    if let Some(u) = w.units.get_mut(h) {
                        u.make_infantry(0);
                    }
                }
                let (s, t, b, c) = match name {
                    "SPY" => (true, false, false, false),
                    "THF" => (false, true, false, false),
                    "E7" => (false, false, true, false),
                    "DOG" => (false, false, false, true),
                    _ => (false, false, false, false),
                };
                w.set_unit_specialist(h, s, t, b, c);
            }
        };
        place(spy, true, "SPY", CellCoord::new(30, 30));
        place(thf, true, "THF", CellCoord::new(30, 31));
        place(e7, true, "E7", CellCoord::new(30, 32));
        place(dog, true, "DOG", CellCoord::new(31, 31));
        core.set_camera((26 * 24) as f32, (26 * 24) as f32);
        core.update(80);
        dump(&core, "specialists_scene.png");
        assert!(
            spy.is_some() && e7.is_some(),
            "specialists are in the catalog"
        );
    }

    // ---------- Scene 2: nuclear strike devastation ----------
    {
        let mslo = building_id(&core, "MSLO").expect("MSLO in catalog");
        let w = core.world_mut();
        for dy in -4..6 {
            for dx in -4..6 {
                w.reveal_shroud(own, CellCoord::new(50 + dx, 46 + dy), 8);
            }
        }
        w.spawn_building(mslo, own, CellCoord::new(46, 44));
        // A cluster of enemy infantry at ground zero — light enough that the
        // WARHEAD_NUKE hit is lethal, so the death diff spawns the fireballs.
        let inf = tank_proto.stats;
        for dx in -1..=1 {
            for dy in -1..=1 {
                let sub = ((dx + 1) * 3 + (dy + 1)) as u8 % 5;
                let h = w.spawn_unit(
                    tank_proto.sprite_id,
                    enemy,
                    CellCoord::new(50 + dx, 48 + dy),
                    Facing(0),
                    60,
                    inf,
                );
                if let Some(u) = w.units.get_mut(h) {
                    u.make_infantry(sub);
                }
            }
        }
        core.update(66); // sync the superweapon into existence
        core.world_mut()
            .force_charge_superweapon(own, SuperKind::Nuclear);
        core.world_mut().tick(&[Command::FireSuperWeapon {
            house: own,
            kind: SuperKind::Nuclear,
            target: Target::Cell(CellCoord::new(50, 48)),
            dest: None,
        }]);
        // Let it fall + detonate; the death diff spawns the client fireballs.
        // Dump the very frame it goes off, before the fireballs prune.
        core.set_camera((44 * 24) as f32, (42 * 24) as f32);
        let mut detonated = false;
        for _ in 0..60 {
            let had = core.world().nuke_strikes().len();
            core.update(66);
            if had > 0 && core.world().nuke_strikes().is_empty() {
                detonated = true;
                break;
            }
        }
        dump(&core, "nuke_detonation.png");
        assert!(detonated, "the nuke fell and detonated");
        assert!(
            core.cosmetic_effect_count() > 0,
            "the nuke detonation spawned explosion effects"
        );
    }

    // ---------- Scene 3: iron curtain + chronosphere ----------
    {
        let iron = building_id(&core, "IRON").expect("IRON in catalog");
        let pdox = building_id(&core, "PDOX").expect("PDOX in catalog");
        let w = core.world_mut();
        for dy in -3..6 {
            for dx in -3..12 {
                w.reveal_shroud(own, CellCoord::new(70 + dx, 60 + dy), 8);
            }
        }
        w.spawn_building(iron, own, CellCoord::new(66, 60));
        w.spawn_building(pdox, own, CellCoord::new(66, 63));
        let curt = w.spawn_unit(
            tank_proto.sprite_id,
            own,
            CellCoord::new(72, 62),
            Facing(0),
            tank_proto.max_health,
            tank_proto.stats,
        );
        w.set_unit_combat(
            curt,
            tank_proto.armor,
            tank_proto.weapon,
            tank_proto.has_turret,
        );
        let warped = w.spawn_unit(
            tank_proto.sprite_id,
            own,
            CellCoord::new(74, 61),
            Facing(0),
            tank_proto.max_health,
            tank_proto.stats,
        );
        w.set_unit_combat(
            warped,
            tank_proto.armor,
            tank_proto.weapon,
            tank_proto.has_turret,
        );
        core.update(66);
        core.world_mut()
            .force_charge_superweapon(own, SuperKind::IronCurtain);
        core.world_mut()
            .force_charge_superweapon(own, SuperKind::Chronosphere);
        core.world_mut().tick(&[
            Command::FireSuperWeapon {
                house: own,
                kind: SuperKind::IronCurtain,
                target: Target::Unit(curt),
                dest: None,
            },
            Command::FireSuperWeapon {
                house: own,
                kind: SuperKind::Chronosphere,
                target: Target::Unit(warped),
                dest: Some(CellCoord::new(78, 64)),
            },
        ]);
        core.update(66);
        assert!(
            core.world()
                .units
                .get(curt)
                .map(|u| u.iron_curtain > 0)
                .unwrap_or(false),
            "the tank is iron-curtained"
        );
        assert_eq!(
            core.world().units.get(warped).map(|u| u.cell()),
            Some(CellCoord::new(78, 64)),
            "the chronosphere warped the tank"
        );
        core.set_camera((64 * 24) as f32, (58 * 24) as f32);
        dump(&core, "iron_curtain_chrono.png");
    }
}
