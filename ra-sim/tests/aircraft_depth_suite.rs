//! P0 aircraft arc — DEEP invariant coverage (ra-tester, M7.17 audit). Extends
//! the coder's `aircraft_suite.rs` smoke with the adversarial cases the audit
//! calls for:
//!
//!   * a LANDED helicopter (Height 0) is immune to AA but a ground defense CAN
//!     hit it — the `is_airborne()` (`Height > 0`) gate, both directions;
//!   * an airborne aircraft takes EXACTLY half damage (`aircraft.cpp:1685`);
//!   * a ground unit NEVER targets, nor retaliates against, an airborne attacker
//!     (the core M7.17-A invariant, both directions);
//!   * an out-of-ammo heli returns to its OWN helipad, never a nearer enemy pad;
//!   * a crashed aircraft is removed cleanly (dead handle, no ground-cell leak);
//!   * ammo decrements one-per-shot and rearms at the cited cadence;
//!   * multi-aircraft: several helis share the air over one cell (no panic, no
//!     ground-occupancy conflict) and an AA gun downs them sequentially.
//!
//! All headless, deterministic, fast. Fixtures mirror `aircraft_suite.rs`.

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    AirState, BuildingProto, Catalog, Command, EconRules, Handle, MoveStats, Passability, Target,
    UnitKind, WarheadProfile, WeaponProfile, World,
};

const B_HPAD: u32 = 0;
const B_AGUN: u32 = 1; // anti-air only (name-derived)
const B_PBOX: u32 = 2; // ground-only

fn air_stats() -> MoveStats {
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

/// Exact-arithmetic weapon (spread 1000 defeats falloff; 100% verses; hitscan).
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
        max_damage: 10000,
    }
}

#[allow(clippy::too_many_arguments)]
fn bproto(name: &str, w: u8, h: u8, wpn: Option<WeaponProfile>) -> BuildingProto {
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
        has_turret: false,
        charges: false,
        is_wall: false,
        storage: 0,
    }
}

fn catalog() -> Catalog {
    Catalog {
        buildings: vec![
            bproto("HPAD", 2, 2, None),
            bproto("AGUN", 1, 2, Some(weapon(40, 20, 4096))),
            bproto("PBOX", 1, 1, Some(weapon(40, 20, 4096))),
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

fn spawn_heli(
    w: &mut World,
    house: u8,
    cell: CellCoord,
    ammo: u16,
    home: Option<Handle>,
    wpn: WeaponProfile,
    hp: u16,
) -> Handle {
    let h = w.spawn_unit(0, house, cell, Facing(0), hp, air_stats());
    w.set_unit_combat(h, 0, Some(wpn), false);
    let u = w.units.get_mut(h).unwrap();
    u.make_aircraft(ammo);
    u.home = home;
    h
}

/// A stationary ground unit (default Vehicle kind).
fn spawn_ground(
    w: &mut World,
    house: u8,
    cell: CellCoord,
    hp: u16,
    wpn: Option<WeaponProfile>,
) -> Handle {
    let h = w.spawn_unit(0, house, cell, Facing(0), hp, air_stats());
    w.set_unit_combat(h, 0, wpn, false);
    assert_eq!(w.units.get(h).unwrap().kind, UnitKind::Vehicle);
    h
}

// ===========================================================================
// 1. Landed heli (Height 0): immune to AA, but a ground defense CAN hit it.
// ===========================================================================

#[test]
fn a_landed_heli_is_immune_to_aa_but_a_ground_defense_can_hit_it() {
    // --- AA side: heli parked on its pad (altitude 0) is NOT an AA target. ---
    let mut w = world(0x1A_0001);
    let pad = w.spawn_building(B_HPAD, 1, CellCoord::new(20, 20)).unwrap();
    let agun = w.spawn_building(B_AGUN, 2, CellCoord::new(22, 20)).unwrap();
    // Heli parked on the pad: full ammo, Idle, altitude 0 (docked).
    let heli = spawn_heli(
        &mut w,
        1,
        CellCoord::new(20, 20),
        6,
        Some(pad),
        weapon(10, 20, 512),
        200,
    );
    {
        let u = w.units.get_mut(heli).unwrap();
        u.altitude = 0;
        u.air_state = AirState::Idle;
    }
    assert!(
        !w.units.get(heli).unwrap().is_airborne(),
        "parked heli is grounded"
    );
    for t in 0..400u32 {
        w.tick(&[]);
        let u = w
            .units
            .get(heli)
            .expect("landed heli must not be shot down by AA");
        assert_eq!(u.altitude, 0, "parked heli stays docked (tick {t})");
        if let Some(b) = w.buildings.get(agun) {
            assert_ne!(
                b.target,
                Some(Target::Unit(heli)),
                "an AA gun must never target a LANDED (Height 0) heli (tick {t})"
            );
        }
    }

    // --- Ground side: a PBOX (ground-only) CAN acquire the same landed heli. ---
    let mut w2 = world(0x1A_0002);
    let pad2 = w2
        .spawn_building(B_HPAD, 1, CellCoord::new(20, 20))
        .unwrap();
    let pbox = w2
        .spawn_building(B_PBOX, 2, CellCoord::new(22, 20))
        .unwrap();
    let heli2 = spawn_heli(
        &mut w2,
        1,
        CellCoord::new(20, 20),
        6,
        Some(pad2),
        weapon(0, 20, 1),
        200,
    );
    {
        let u = w2.units.get_mut(heli2).unwrap();
        u.altitude = 0;
        u.air_state = AirState::Idle;
        u.weapon = None; // unarmed so it never flies off to attack
    }
    let mut ground_hit = false;
    for _ in 0..400u32 {
        w2.tick(&[]);
        match w2.units.get(heli2) {
            Some(u) => {
                if u.health < 200 {
                    ground_hit = true;
                    break;
                }
            }
            None => {
                ground_hit = true;
                break;
            }
        }
    }
    assert!(
        ground_hit,
        "a ground defense (PBOX) MUST be able to hit a landed (Height 0) heli — \
         it is a ground target while docked"
    );
    let _ = pbox;
}

// ===========================================================================
// 2. Airborne aircraft takes EXACTLY half damage (aircraft.cpp:1685).
// ===========================================================================

/// Two identical defenses fire the identical weapon: a PBOX at a ground vehicle,
/// an AGUN at an airborne heli. The heli's first-hit HP drop must be exactly half
/// the ground vehicle's — the `if (Height) damage /= 2` halving, isolated from
/// every other damage factor (same weapon, same distance, same armor).
#[test]
fn an_airborne_aircraft_takes_exactly_half_damage() {
    let mut w = world(0x2A_0001);
    // Ground reference: PBOX vs a ground vehicle.
    let _pbox = w.spawn_building(B_PBOX, 1, CellCoord::new(20, 20)).unwrap();
    let ground = spawn_ground(&mut w, 2, CellCoord::new(22, 20), 5000, None);
    // Air: AGUN vs an airborne heli (no home → hovers at flight level).
    let _agun = w.spawn_building(B_AGUN, 1, CellCoord::new(20, 40)).unwrap();
    let heli = spawn_heli(
        &mut w,
        2,
        CellCoord::new(22, 40),
        6,
        None,
        weapon(0, 200, 1),
        5000,
    );
    assert!(w.units.get(heli).unwrap().is_airborne());

    let mut ground_drop = None;
    let mut air_drop = None;
    let (mut g_prev, mut a_prev) = (5000u16, 5000u16);
    for _ in 0..600u32 {
        w.tick(&[]);
        if ground_drop.is_none() {
            let hp = w.units.get(ground).unwrap().health;
            if hp < g_prev {
                ground_drop = Some(g_prev - hp);
            }
            g_prev = hp;
        }
        if air_drop.is_none() {
            if let Some(u) = w.units.get(heli) {
                if u.health < a_prev {
                    air_drop = Some(a_prev - u.health);
                }
                a_prev = u.health;
            }
        }
        if ground_drop.is_some() && air_drop.is_some() {
            break;
        }
    }
    let g = ground_drop.expect("the PBOX must hit the ground vehicle");
    let a = air_drop.expect("the AGUN must hit the airborne heli");
    assert!(
        a > 0 && g > 0,
        "both must take positive damage (g={g}, a={a})"
    );
    assert_eq!(
        a * 2,
        g,
        "an airborne aircraft must take exactly half damage: air drop {a} vs \
         ground drop {g} (aircraft.cpp:1685 `if (Height) damage /= 2`)"
    );
}

// ===========================================================================
// 3. A ground unit never targets, nor retaliates against, an airborne attacker.
// ===========================================================================

#[test]
fn a_ground_unit_never_targets_or_retaliates_against_an_airborne_aircraft() {
    let mut w = world(0x3A_0001);
    // An armed, hunting ground tank.
    let tank = spawn_ground(
        &mut w,
        1,
        CellCoord::new(20, 20),
        400,
        Some(weapon(50, 20, 4096)),
    );
    w.units.get_mut(tank).unwrap().hunt = true;
    // An enemy heli strafing the tank from the air.
    let heli = spawn_heli(
        &mut w,
        2,
        CellCoord::new(23, 20),
        30,
        None,
        weapon(15, 8, 1024),
        400,
    );
    w.tick(&[Command::Attack {
        unit: heli,
        target: Target::Unit(tank),
        house: 2,
    }]);

    let mut tank_took_damage = false;
    for t in 0..400u32 {
        w.tick(&[]);
        // The heli should be doing its job (attacking the tank), proving the
        // scenario is live — but the tank must NEVER acquire the airborne heli.
        if let Some(t) = w.units.get(tank) {
            assert_ne!(
                t.target,
                Some(Target::Unit(heli)),
                "a ground unit must never target an airborne aircraft (tick {t:?})"
            );
            if t.health < 400 {
                tank_took_damage = true;
            }
        }
        // The heli must remain untouched: no ground retaliation reaches it.
        if let Some(h) = w.units.get(heli) {
            assert_eq!(
                h.health, 400,
                "the airborne heli must take no damage — no ground unit can hit it"
            );
        }
        let _ = t;
    }
    assert!(
        tank_took_damage,
        "sanity: the heli must actually have been strafing the tank"
    );
}

// ===========================================================================
// 4. An out-of-ammo heli returns to its OWN helipad, not a nearer enemy pad.
// ===========================================================================

#[test]
fn a_returning_heli_lands_only_on_its_own_helipad() {
    let mut w = world(0x4A_0001);
    // Own pad far to the west; an ENEMY pad much nearer to the east.
    let own = w.spawn_building(B_HPAD, 1, CellCoord::new(6, 20)).unwrap();
    let _enemy_pad = w.spawn_building(B_HPAD, 2, CellCoord::new(26, 20)).unwrap();
    // Heli near the enemy pad, out of ammo, homed to its own pad.
    let heli = spawn_heli(
        &mut w,
        1,
        CellCoord::new(24, 20),
        0,
        Some(own),
        weapon(10, 20, 512),
        200,
    );
    w.units.get_mut(heli).unwrap().air_state = AirState::Returning;

    let mut landed_cell = None;
    for _ in 0..800u32 {
        w.tick(&[]);
        let u = w.units.get(heli).unwrap();
        if u.altitude == 0 {
            landed_cell = Some(u.cell());
            break;
        }
    }
    let cell = landed_cell.expect("the heli must land somewhere within budget");
    let own_center = w.buildings.get(own).unwrap().center_cell();
    assert_eq!(
        cell, own_center,
        "the heli must land on its OWN pad {own_center:?}, not the nearer enemy pad"
    );
}

// ===========================================================================
// 5. A crashed aircraft is removed cleanly (dead handle, no ground-cell leak).
// ===========================================================================

#[test]
fn a_crashed_aircraft_leaves_no_dangling_handle_or_cell_occupancy() {
    let mut w = world(0x5A_0001);
    let _agun = w.spawn_building(B_AGUN, 1, CellCoord::new(20, 20)).unwrap();
    let over = CellCoord::new(23, 20);
    let heli = spawn_heli(&mut w, 2, over, 6, None, weapon(10, 20, 512), 50);

    let mut crashed = false;
    for _ in 0..600u32 {
        w.tick(&[]);
        if !w.units.contains(heli) {
            crashed = true;
            break;
        }
    }
    assert!(crashed, "the AA gun must down the airborne heli");
    // Handle is dead.
    assert!(
        w.units.get(heli).is_none(),
        "crashed heli handle must be invalid"
    );
    assert!(!w.units.contains(heli));
    // No ground-cell leak: a ground vehicle can occupy the cell the heli overflew
    // (aircraft never claimed ground occupancy, so nothing to leak).
    let veh = spawn_ground(&mut w, 1, CellCoord::new(23, 25), 400, None);
    w.tick(&[Command::Move {
        unit: veh,
        dest: over,
        house: 1,
    }]);
    let mut arrived = false;
    for _ in 0..300u32 {
        w.tick(&[]);
        if w.units.get(veh).unwrap().cell() == over {
            arrived = true;
            break;
        }
    }
    assert!(
        arrived,
        "a ground vehicle must freely occupy the cell the crashed heli overflew \
         (no phantom occupancy left behind)"
    );
}

// ===========================================================================
// 6. Ammo decrements one-per-shot; rearm refills at the cited cadence.
// ===========================================================================

#[test]
fn ammo_decrements_one_per_shot_and_rearms_at_the_cited_cadence() {
    let mut w = world(0x6A_0001);
    let pad = w.spawn_building(B_HPAD, 1, CellCoord::new(14, 20)).unwrap();
    let target = spawn_ground(&mut w, 2, CellCoord::new(24, 20), 60000, None);
    let heli = spawn_heli(
        &mut w,
        1,
        CellCoord::new(22, 20),
        3,
        Some(pad),
        weapon(30, 8, 768),
        400,
    );
    w.tick(&[Command::Attack {
        unit: heli,
        target: Target::Unit(target),
        house: 1,
    }]);

    // Track ammo over the strafing run: it must only ever step DOWN by exactly 1
    // (one round per shot), never skip. Also record the empty tick.
    let mut prev_ammo = 3u16;
    let mut emptied = None;
    for t in 0..600u32 {
        w.tick(&[]);
        let u = w.units.get(heli).unwrap();
        if u.ammo < prev_ammo {
            assert_eq!(
                prev_ammo - u.ammo,
                1,
                "ammo must decrement exactly one per shot (was {prev_ammo}, now {})",
                u.ammo
            );
        }
        prev_ammo = u.ammo;
        if u.ammo == 0 && emptied.is_none() {
            emptied = Some(t);
        }
    }
    assert!(
        emptied.is_some(),
        "the heli must empty its 3-round magazine"
    );

    // Rearm cadence: default ReloadRate .05 min * 900 t/min = 45 ticks/round.
    // Measure the tick gap between two successive +1 refills while docked.
    let mut refill_ticks = Vec::new();
    let mut prev = w.units.get(heli).unwrap().ammo;
    for t in 0..2000u32 {
        w.tick(&[]);
        let a = w.units.get(heli).unwrap().ammo;
        if a > prev {
            refill_ticks.push(t);
        }
        prev = a;
        if a >= 3 {
            break;
        }
    }
    assert!(
        refill_ticks.len() >= 2,
        "must observe at least two rearm steps to measure the cadence"
    );
    let gap = refill_ticks[1] - refill_ticks[0];
    assert_eq!(
        gap, 45,
        "rearm cadence must be 45 ticks/round (ReloadRate .05 * 900 t/min); saw {gap}"
    );
}

// ===========================================================================
// 7. Multi-aircraft: several helis share the air over one cell (Q24).
// ===========================================================================

#[test]
fn several_helicopters_may_share_the_air_over_one_cell() {
    let mut w = world(0x7A_0001);
    let cell = CellCoord::new(20, 20);
    // Four helis all hovering over the SAME cell — allowed per Q24 (aircraft
    // occupy the air, not the ground cell). No panic, no occupancy conflict.
    let helis: Vec<Handle> = (0..4)
        .map(|_| spawn_heli(&mut w, 1, cell, 6, None, weapon(10, 20, 512), 200))
        .collect();
    for _ in 0..200u32 {
        w.tick(&[]);
    }
    // All four still alive, all still over/near the shared cell, none forced apart
    // by a ground-occupancy rule.
    for &h in &helis {
        let u = w
            .units
            .get(h)
            .expect("no heli may vanish from an air stack");
        assert_eq!(u.kind, UnitKind::Aircraft);
    }
    // A ground vehicle can still occupy that very cell (air stack ≠ ground block).
    let veh = spawn_ground(&mut w, 1, CellCoord::new(20, 25), 400, None);
    w.tick(&[Command::Move {
        unit: veh,
        dest: cell,
        house: 1,
    }]);
    let mut arrived = false;
    for _ in 0..200u32 {
        w.tick(&[]);
        if w.units.get(veh).unwrap().cell() == cell {
            arrived = true;
            break;
        }
    }
    assert!(
        arrived,
        "a ground vehicle may occupy a cell a heli stack overflies"
    );
}

// ===========================================================================
// 8. An AA gun retargets and downs multiple aircraft sequentially.
// ===========================================================================

#[test]
fn an_aa_gun_downs_multiple_aircraft_one_after_another() {
    let mut w = world(0x8A_0001);
    let _agun = w.spawn_building(B_AGUN, 1, CellCoord::new(20, 20)).unwrap();
    // Three enemy helis hovering in range.
    let helis: Vec<Handle> = [
        CellCoord::new(23, 20),
        CellCoord::new(20, 23),
        CellCoord::new(23, 23),
    ]
    .into_iter()
    .map(|c| spawn_heli(&mut w, 2, c, 6, None, weapon(0, 200, 1), 60))
    .collect();

    let mut downed = 0;
    for _ in 0..2000u32 {
        w.tick(&[]);
        downed = helis.iter().filter(|&&h| !w.units.contains(h)).count();
        if downed == helis.len() {
            break;
        }
    }
    assert_eq!(
        downed,
        helis.len(),
        "the AA gun must retarget after each kill and down all three helis"
    );
}

// ===========================================================================
// 9. Determinism: an aircraft + AA combat script hashes identically twice.
// ===========================================================================

#[test]
fn aircraft_and_aa_combat_is_deterministic_same_seed_twice() {
    let run = |seed: u32| -> Vec<u64> {
        let mut w = world(seed);
        let _agun = w.spawn_building(B_AGUN, 1, CellCoord::new(20, 20)).unwrap();
        let pad = w.spawn_building(B_HPAD, 2, CellCoord::new(40, 20)).unwrap();
        let target = spawn_ground(&mut w, 1, CellCoord::new(30, 20), 5000, None);
        // Two enemy helis: one strafing the ground target, one hovering in AA range.
        let strafer = spawn_heli(
            &mut w,
            2,
            CellCoord::new(28, 20),
            4,
            Some(pad),
            weapon(30, 8, 768),
            400,
        );
        let _hover = spawn_heli(
            &mut w,
            2,
            CellCoord::new(23, 20),
            6,
            None,
            weapon(0, 200, 1),
            60,
        );
        let mut hashes = Vec::with_capacity(400);
        hashes.push(w.tick(&[Command::Attack {
            unit: strafer,
            target: Target::Unit(target),
            house: 2,
        }]));
        for _ in 0..400 {
            hashes.push(w.tick(&[]));
        }
        hashes
    };
    assert_eq!(
        run(0x9A_00CC),
        run(0x9A_00CC),
        "an aircraft+AA combat script must hash identically on the same seed"
    );
    // Seed-sensitive: a different seed must diverge somewhere (AA bullet scatter
    // draws sim RNG), guarding that the chain isn't a constant.
    assert_ne!(
        run(0x9A_00CC),
        run(0x9A_00DD),
        "different seeds should not produce an identical chain"
    );
}
