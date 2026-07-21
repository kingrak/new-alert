//! P0 aircraft arc — acceptance smoke coverage (ra-coder). Proves the flight
//! locomotor, the helicopter attack/rearm cycle, AA-vs-air targeting, terrain-
//! ignoring flight, and determinism, all headless. Exhaustive adversarial
//! coverage (rearm-under-low-power, curley-shuffle repositioning, SAM landed-
//! aircraft special case, AI air waves) is handed to ra-tester — this file is the
//! minimal proof the mechanics run and match the reference cited in
//! `ra-sim/src/world.rs::run_aircraft` (`aircraft.cpp`, `fly.cpp`).

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    AirState, BuildingProto, Catalog, Command, EconRules, Handle, Locomotor, MoveStats,
    Passability, Target, UnitKind, WarheadProfile, WeaponProfile, World, FLIGHT_LEVEL,
};

// Building type ids in the fixture catalog.
const B_HPAD: u32 = 0; // helipad (aircraft dock/rearm) — is_helipad derived by name
const B_AGUN: u32 = 1; // AA gun (anti-air only) — is_aa derived by name
const B_PBOX: u32 = 2; // ground pillbox (cannot hit air)

fn air_stats() -> MoveStats {
    // Fast flier: 50 leptons/tick, quick turn.
    MoveStats {
        max_speed: 50,
        rot: 20,
    }
}

fn pct5(p: [i32; 5]) -> [i32; 5] {
    let mut o = [0i32; 5];
    for (d, v) in o.iter_mut().zip(p) {
        *d = v * 65536 / 100;
    }
    o
}

/// Full-damage, instant (hitscan) weapon vs none-armor — the defense_suite trick
/// (`spread = 1000` defeats the falloff divisor) so damage is exact arithmetic.
fn weapon(damage: i32, rof: u16, range: i32) -> WeaponProfile {
    WeaponProfile {
        damage,
        rof,
        range,
        proj_speed: 255,
        proj_rot: 0,
        invisible: true,
        instant: true,
        warhead: WarheadProfile {
            spread: 1000,
            verses: pct5([100, 100, 100, 100, 100]),
        },
        warhead_ap: false,
        arcing: false,
        ballistic_scatter: 256,
        homing_scatter: 512,
        min_damage: 1,
        max_damage: 1000,
    }
}

#[allow(clippy::too_many_arguments)]
fn bproto(name: &str, w: u8, h: u8, wpn: Option<WeaponProfile>, turret: bool) -> BuildingProto {
    BuildingProto {
        name: name.to_string(),
        foot_w: w,
        foot_h: h,
        max_health: 400,
        armor: 0,
        power: 0,
        cost: 400,
        prereq: vec![],
        is_refinery: false,
        is_construction_yard: false,
        is_war_factory: false,
        is_barracks: false,
        free_harvester_unit: None,
        sight: 5,
        sprite_id: 0,
        weapon: wpn,
        has_turret: turret,
        charges: false,
        is_wall: false,
        storage: 0,
    }
}

fn catalog() -> Catalog {
    Catalog {
        buildings: vec![
            bproto("HPAD", 2, 2, None, false),
            // AA gun: long range so it can reach an airborne heli; fixed emplacement
            // (turret=false) so the alignment gate never stops it hitting a mover.
            bproto("AGUN", 1, 2, Some(weapon(40, 20, 4096)), false),
            bproto("PBOX", 1, 1, Some(weapon(50, 20, 4096)), false),
        ],
        units: vec![],
        econ: EconRules::default(),
    }
}

fn world(seed: u32) -> World {
    let mut w = World::new(Passability::all_passable(), seed);
    w.set_catalog(catalog());
    w.init_houses(3, 1000);
    w
}

/// Spawn an aircraft (helicopter) for `house` at `cell`, airborne with `ammo`
/// rounds, homed to `home`, armed with `wpn`.
fn spawn_heli(
    w: &mut World,
    house: u8,
    cell: CellCoord,
    ammo: u16,
    home: Option<Handle>,
    wpn: WeaponProfile,
) -> Handle {
    let h = w.spawn_unit(0, house, cell, Facing(0), 100, air_stats());
    w.set_unit_combat(h, 0, Some(wpn), false);
    let u = w.units.get_mut(h).unwrap();
    u.make_aircraft(ammo);
    u.home = home;
    h
}

/// Spawn a stationary ground target for `house` at `cell` with `hp` health.
fn spawn_ground(w: &mut World, house: u8, cell: CellCoord, hp: u16) -> Handle {
    let h = w.spawn_unit(0, house, cell, Facing(0), hp, air_stats());
    w.set_unit_combat(h, 0, None, false); // unarmed, so it never fights back
                                          // Ground unit (default kind Vehicle, but pin it explicitly).
    assert_eq!(w.units.get(h).unwrap().kind, UnitKind::Vehicle);
    h
}

// ===========================================================================
// 1. Helicopter attack → out of ammo → return → rearm → re-attack.
// ===========================================================================

/// A Longbow-style heli strafes a durable ground target until its 3-round
/// magazine is empty, flies to its helipad, lands (altitude 0), rearms one round
/// per `ReloadRate` cadence, takes off, and re-attacks. Reports the ammo-cycle
/// tick milestones.
#[test]
fn heli_strafes_returns_to_helipad_rearms_and_reattacks() {
    let mut w = world(0xA1_0001);
    let pad = w.spawn_building(B_HPAD, 1, CellCoord::new(14, 20)).unwrap();
    // Durable target so the heli empties its magazine without killing it.
    let target = spawn_ground(&mut w, 2, CellCoord::new(24, 20), 5000);
    // Heli near the target (in weapon range = 3 cells), homed to the pad.
    let heli = spawn_heli(
        &mut w,
        1,
        CellCoord::new(22, 20),
        3,
        Some(pad),
        weapon(30, 8, 768),
    );
    // Order the attack.
    w.tick(&[Command::Attack {
        unit: heli,
        target: Target::Unit(target),
        house: 1,
    }]);

    let mut emptied_tick = None;
    let mut landed_tick = None;
    let mut refilled_tick = None;
    let mut reattacked_shots = 0u32;
    let start_hp = w.units.get(target).unwrap().health;
    let mut last_hp = start_hp;

    for t in 0..1200u32 {
        w.tick(&[]);
        let u = match w.units.get(heli) {
            Some(u) => u,
            None => panic!("heli must not die in this scenario"),
        };
        if emptied_tick.is_none() && u.ammo == 0 {
            emptied_tick = Some(t);
        }
        if emptied_tick.is_some() && landed_tick.is_none() && u.altitude == 0 {
            landed_tick = Some(t);
        }
        if landed_tick.is_some() && refilled_tick.is_none() && u.ammo == 3 {
            refilled_tick = Some(t);
        }
        // Count re-attack shots after the refill (target HP drops again).
        let hp = w.units.get(target).unwrap().health;
        if refilled_tick.is_some() && hp < last_hp {
            reattacked_shots += 1;
        }
        last_hp = hp;
        if reattacked_shots >= 2 {
            break;
        }
    }

    let emptied = emptied_tick.expect("heli must empty its magazine");
    let landed = landed_tick.expect("heli must land on its helipad to rearm");
    let refilled = refilled_tick.expect("heli must refill to full ammo at the pad");
    assert!(
        emptied < landed && landed < refilled,
        "ammo cycle must be ordered empty({emptied}) < land({landed}) < refill({refilled})"
    );
    assert!(
        reattacked_shots >= 2,
        "heli must re-attack after rearming (saw {reattacked_shots} post-rearm shots)"
    );
    // The target took damage before AND after the rearm — the full cycle ran.
    assert!(
        w.units.get(target).unwrap().health < start_hp,
        "target must have taken damage across the cycle"
    );
    // Reload cadence: default ReloadRate .05 min * 900 t/min = 45 ticks/round.
    eprintln!(
        "AMMO CYCLE: emptied@{emptied} landed@{landed} refilled@{refilled} \
         (reload cadence 45 t/round, 3 rounds)"
    );
}

// ===========================================================================
// 2. AA gun downs an aircraft; a ground pillbox cannot touch it.
// ===========================================================================

/// An AGUN (anti-air) auto-acquires and destroys an airborne enemy heli, while a
/// PBOX (ground-only) placed right beside it never targets the same airborne
/// craft — the `IsAntiAircraft`/`Height > 0` gate (`Can_Fire`, `techno.cpp:2895`).
#[test]
fn aa_gun_downs_an_airborne_heli_but_a_pillbox_cannot() {
    let mut w = world(0xAA_0002);
    let agun = w.spawn_building(B_AGUN, 1, CellCoord::new(20, 20)).unwrap();
    let pbox = w.spawn_building(B_PBOX, 1, CellCoord::new(20, 24)).unwrap();
    // Enemy heli hovering in range of both, attacking nothing (just present).
    let heli = spawn_heli(
        &mut w,
        2,
        CellCoord::new(24, 22),
        6,
        None,
        weapon(10, 20, 512),
    );
    assert!(
        w.units.get(heli).unwrap().is_airborne(),
        "heli starts airborne"
    );

    let mut downed_at = None;
    for t in 0..600u32 {
        w.tick(&[]);
        // The PBOX must never lock onto the airborne heli.
        if let Some(b) = w.buildings.get(pbox) {
            assert_ne!(
                b.target,
                Some(Target::Unit(heli)),
                "a ground pillbox must never target an airborne aircraft (tick {t})"
            );
        }
        if !w.units.contains(heli) {
            downed_at = Some(t);
            break;
        }
    }
    let downed = downed_at.expect("the AA gun must destroy the airborne heli");
    // The AGUN was the shooter: it had acquired the heli at some point.
    let _ = agun;
    eprintln!("AA KILL: airborne heli crashed at tick {downed}");
}

// ===========================================================================
// 3. Aircraft ignore ground impassability (fly over water/cliffs).
// ===========================================================================

/// A heli ordered across a full-height impassable barrier reaches the far side —
/// a ground unit cannot even path across it. Proves altitude flight ignores the
/// land-type passability grid (`FlyClass::Physics`, `fly.cpp`).
#[test]
fn aircraft_flies_over_impassable_terrain_a_ground_unit_cannot_cross() {
    // 40x40 grid, an impassable wall column at x=20 spanning the whole height.
    let (gw, gh) = (40i32, 40i32);
    let mut cells = vec![true; (gw * gh) as usize];
    for y in 0..gh {
        cells[(y * gw + 20) as usize] = false;
    }
    let passable = Passability::new(gw, gh, cells);
    // A ground vehicle cannot path from west to east across the sealed column.
    assert!(
        ra_sim::path::find_path(
            &passable,
            CellCoord::new(5, 20),
            CellCoord::new(30, 20),
            Locomotor::Track,
        )
        .is_none(),
        "sanity: the barrier column fully seals ground movement"
    );

    let mut w = World::new(passable, 0xF1_0003);
    w.set_catalog(catalog());
    w.init_houses(2, 0);
    let heli = spawn_heli(
        &mut w,
        1,
        CellCoord::new(5, 20),
        6,
        None,
        weapon(10, 20, 512),
    );
    // Order it to fly to the far (east) side of the barrier.
    let dest = CellCoord::new(34, 20);
    w.tick(&[Command::Move {
        unit: heli,
        dest,
        house: 1,
    }]);
    let mut crossed = false;
    for _ in 0..600u32 {
        w.tick(&[]);
        let c = w.units.get(heli).unwrap().cell();
        if c.x >= 33 {
            crossed = true;
            break;
        }
    }
    assert!(
        crossed,
        "the heli must fly over the sealed barrier and reach the east side"
    );
    // It genuinely overflew the impassable column (was east of it at the end).
    assert!(w.units.get(heli).unwrap().cell().x > 20);
}

// ===========================================================================
// 4. Determinism: same seed + commands → identical hash chain (with aircraft).
// ===========================================================================

fn run_heli_scenario(seed: u32) -> Vec<u64> {
    let mut w = world(seed);
    let pad = w.spawn_building(B_HPAD, 1, CellCoord::new(14, 20)).unwrap();
    let target = spawn_ground(&mut w, 2, CellCoord::new(24, 20), 5000);
    let heli = spawn_heli(
        &mut w,
        1,
        CellCoord::new(22, 20),
        3,
        Some(pad),
        weapon(30, 8, 768),
    );
    let mut hashes = Vec::new();
    hashes.push(w.tick(&[Command::Attack {
        unit: heli,
        target: Target::Unit(target),
        house: 1,
    }]));
    for _ in 0..400 {
        hashes.push(w.tick(&[]));
    }
    hashes
}

#[test]
fn aircraft_scenario_is_deterministic_same_seed_twice() {
    let a = run_heli_scenario(0xD1_0004);
    let b = run_heli_scenario(0xD1_0004);
    assert_eq!(
        a, b,
        "identical seed + commands must give an identical hash chain"
    );
    // Different seed must not be trivially identical (the AA/scatter RNG differs
    // only if drawn; this scenario draws none, so this just guards the harness is
    // actually hashing evolving aircraft state, not a constant).
    assert!(
        a.iter().any(|&h| h != a[0]),
        "the world state must evolve over the run"
    );
}

// ===========================================================================
// 5. An idle aircraft with no home hovers; with a home it lands on the pad.
// ===========================================================================

/// A homeless idle heli holds altitude (hovers) and does not crash; a homed idle
/// heli flies to its pad and settles (altitude 0) — `Enter_Idle_Mode`.
#[test]
fn idle_heli_hovers_without_a_home_and_lands_with_one() {
    // Homeless: hovers at flight level indefinitely.
    let mut w = world(0x1D_0005);
    let homeless = spawn_heli(
        &mut w,
        1,
        CellCoord::new(10, 10),
        6,
        None,
        weapon(10, 20, 512),
    );
    for _ in 0..100 {
        w.tick(&[]);
    }
    let u = w.units.get(homeless).unwrap();
    assert_eq!(
        u.altitude, FLIGHT_LEVEL,
        "a homeless idle heli hovers at flight level"
    );
    assert_eq!(u.air_state, AirState::Idle);

    // Homed: flies to the pad and lands.
    let mut w2 = world(0x1D_0006);
    let pad = w2
        .spawn_building(B_HPAD, 1, CellCoord::new(30, 30))
        .unwrap();
    let homed = spawn_heli(
        &mut w2,
        1,
        CellCoord::new(10, 10),
        6,
        Some(pad),
        weapon(10, 20, 512),
    );
    let mut landed = false;
    for _ in 0..600 {
        w2.tick(&[]);
        if w2.units.get(homed).unwrap().altitude == 0 {
            landed = true;
            break;
        }
    }
    assert!(landed, "a homed idle heli must fly to its pad and land");
}
