//! P1 aircraft-render evidence (P0 aircraft arc, playability pass). Loads the
//! **real** assets (so real HELI/HPAD/AGUN sprites, RROTOR blades, and the
//! `<NAME>ICON.SHP` sidebar cameos are installed), builds a controlled airborne
//! scene through the `world_mut` seam, and dumps PNGs proving:
//!
//!   1. `aircraft_scene.png`  — a helicopter airborne (lifted by its altitude,
//!      with a ground shadow and spinning rotor) firing at a ground target while
//!      an enemy AA gun fires a tracer up at it.
//!   2. `aircraft_crash.png`  — the same heli one tick after it is destroyed:
//!      the crash fireball spawns at the flight altitude (lifted explosion).
//!   3. `aircraft_cameos.png` — the sidebar scrolled to reveal the new HPAD/
//!      AGUN/SAM (structures) and HELI/HIND (units) cameos in both strips.
//!
//! Skips cleanly (never fails) when the real assets aren't present — the render
//! path itself is exercised without assets by the determinism/sim suites.

mod support;

use ra_client::appcore::AppCore;
use ra_client::input::InputEvent;
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::unit::{AirState, FLIGHT_LEVEL};
use ra_sim::Target;
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
fn aircraft_render_scene_crash_and_cameos_png() {
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

    let heli = unit_id(&core, "HELI").expect("HELI in catalog");
    let target_uid = unit_id(&core, "2TNK")
        .or_else(|| unit_id(&core, "E1"))
        .unwrap();
    let hpad = building_id(&core, "HPAD").expect("HPAD in catalog");
    let agun = building_id(&core, "AGUN").expect("AGUN in catalog");

    // Authentic weapons straight from the catalog.
    let heli_wpn = core.world().catalog.unit(heli).and_then(|u| u.weapon);
    let agun_wpn = core.world().catalog.building(agun).and_then(|b| b.weapon);
    let heli_proto = core.world().catalog.unit(heli).cloned().unwrap();
    let tgt_proto = core.world().catalog.unit(target_uid).cloned().unwrap();

    // Scene cells (aircraft ignore terrain; buildings just stamp occupancy).
    let heli_cell = CellCoord::new(30, 30);
    let target_cell = CellCoord::new(33, 30);
    let pad_cell = CellCoord::new(28, 32);
    let agun_cell = CellCoord::new(33, 33);

    let w = core.world_mut();

    // Reveal the shroud over the scene so every piece shows in the frame.
    for dy in -2..12 {
        for dx in -4..12 {
            w.reveal_shroud(own, CellCoord::new(26 + dx, 28 + dy), 8);
        }
    }

    // Helipad (own) + a ground target tank (enemy).
    w.spawn_building(hpad, own, pad_cell);
    let tgt = w.spawn_unit(
        tgt_proto.sprite_id,
        enemy,
        target_cell,
        Facing(192),
        tgt_proto.max_health,
        tgt_proto.stats,
    );
    w.set_unit_combat(tgt, tgt_proto.armor, tgt_proto.weapon, tgt_proto.has_turret);

    // Airborne attack helicopter (own): full altitude, Attack FSM, aimed
    // at the tank, arm one tick shy of a shot so the muzzle flash renders.
    let heli_h = w.spawn_unit(
        heli_proto.sprite_id,
        own,
        heli_cell,
        Facing(160),
        heli_proto.max_health,
        heli_proto.stats,
    );
    w.set_unit_combat(heli_h, heli_proto.armor, heli_wpn, heli_proto.has_turret);
    w.set_unit_max_health(heli_h, heli_proto.max_health);
    if let Some(u) = w.units.get_mut(heli_h) {
        u.make_aircraft(heli_proto.ammo);
        u.altitude = FLIGHT_LEVEL;
        u.air_state = AirState::Attack;
        u.target = Some(Target::Unit(tgt));
        u.turret_facing = Facing(160);
        if let Some(rof) = heli_wpn.map(|wp| wp.rof) {
            u.arm = rof.saturating_sub(1).max(1);
        }
    }

    // Enemy AA gun firing up at the airborne heli: give it the heli as its
    // target and arm it one tick shy so `draw_defense_effects` paints the AA
    // tracer/flash toward the lifted heli.
    if let Some(pad) = w.spawn_building(agun, enemy, agun_cell) {
        if let Some(b) = w.buildings.get_mut(pad) {
            b.weapon = agun_wpn;
            b.has_turret = true;
            b.target = Some(Target::Unit(heli_h));
            b.turret_facing = Facing(64);
            if let Some(rof) = agun_wpn.map(|wp| wp.rof) {
                b.arm = rof.saturating_sub(1).max(1);
            }
        }
    }

    // Frame the scene: centre the camera on the cluster.
    core.set_camera((26 * 24) as f32, (26 * 24) as f32);
    // Advance the cosmetic clock a hair so the rotor is mid-spin (frame != 0).
    core.update(80);
    dump(&core, "aircraft_scene.png");

    // Crash: soften the heli to near-death and let the enemy AA gun finish it
    // through the real combat path (so the death diff sees it vanish and spawns
    // the lifted crash explosion at the flight altitude). Capture the frame right
    // after it goes down, while the fireball is fresh.
    if let Some(u) = core.world_mut().units.get_mut(heli_h) {
        u.health = 1;
    }
    let mut crashed = false;
    for _ in 0..90 {
        core.update(66);
        if !core.world().units.contains(heli_h) {
            crashed = true;
            core.update(66); // advance the explosion a couple frames so it shows
            break;
        }
    }
    dump(&core, "aircraft_crash.png");
    assert!(
        crashed && core.cosmetic_effect_count() > 0,
        "the AA gun should down the heli and a crash explosion should be live"
    );

    // Sidebar cameos: the two strips scroll independently, so scroll the
    // structures column down to the HPAD/AGUN/SAM rows (after the defenses) and
    // the units column to the HELI/HIND rows (after the vehicles), so a single
    // frame shows the new aircraft cameos in *both* strips.
    for _ in 0..14 {
        core.handle(InputEvent::SidebarScroll {
            column: 0,
            up: false,
        });
    }
    for _ in 0..8 {
        core.handle(InputEvent::SidebarScroll {
            column: 1,
            up: false,
        });
    }
    dump(&core, "aircraft_cameos.png");

    eprintln!("aircraft render PNGs written to {}", scratch().display());
}
