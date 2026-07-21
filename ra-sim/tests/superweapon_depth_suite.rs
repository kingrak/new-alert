//! ra-tester depth coverage for the M7.19 marquee arc (specialists + superweapons).
//! Complements `superweapon_smoke.rs` (which proves the systems *run*) by pinning
//! the boundaries the brief calls load-bearing: the charge cycle timing, the fire
//! gate, superweapon presence tracking, the nuclear radius/damage/fall model, the
//! iron-curtain duration boundary, chronosphere one-way teleport, the thief/spy
//! credit fractions, the exact C4 fuse, the disguise/dog transitions, and
//! determinism of a charge+fire sequence.
//!
//! Fully deterministic and asset-free (synthetic catalog); no `#[ignore]`.

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    AiPlayer, BuildingProto, Catalog, Command, Difficulty, EconRules, MoveStats, Passability,
    SuperKind, Target, UnitProto, WarheadProfile, WeaponProfile, World,
};

const B_PROC: u32 = 0;
const B_TARGET: u32 = 1;
const B_MSLO: u32 = 2;
const B_IRON: u32 = 3;
const B_PDOX: u32 = 4;
const B_DOME: u32 = 5;
const B_SILO: u32 = 6;

const U_SPY: u32 = 0;
const U_THF: u32 = 1;
const U_E7: u32 = 2;
const U_TANK: u32 = 3;
const U_DOG: u32 = 4;

// stock-ish minute so recharge math is exercised but tests stay short.
const TPM: i32 = 60;

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 40,
        rot: 10,
    }
}

fn pct5(p: [i32; 5]) -> [i32; 5] {
    let mut o = [0i32; 5];
    for (d, v) in o.iter_mut().zip(p) {
        *d = v * 65536 / 100;
    }
    o
}

#[allow(clippy::too_many_arguments)]
fn bproto(
    name: &str,
    w: u8,
    h: u8,
    max_health: u16,
    is_refinery: bool,
    storage: i32,
) -> BuildingProto {
    BuildingProto {
        name: name.to_string(),
        foot_w: w,
        foot_h: h,
        max_health,
        armor: 0,
        power: 0,
        cost: 300,
        prereq: vec![],
        is_refinery,
        is_construction_yard: false,
        is_war_factory: false,
        is_barracks: false,
        free_harvester_unit: None,
        sight: 4,
        sprite_id: 0,
        weapon: None,
        has_turret: false,
        charges: false,
        is_wall: false,
        storage,
    }
}

fn uproto(name: &str, is_infantry: bool, weapon: Option<WeaponProfile>) -> UnitProto {
    UnitProto {
        name: name.to_string(),
        sprite_id: 0,
        max_health: 100,
        stats: stats(),
        armor: 0,
        weapon,
        secondary: None,
        has_turret: false,
        is_harvester: false,
        is_infantry,
        locomotor: if is_infantry { 0 } else { 1 },
        deploys_to: None,
        cost: 400,
        prereq: vec![],
        sight: 4,
        passengers: 0,
        ammo: 0,
    }
}

fn cannon() -> WeaponProfile {
    WeaponProfile {
        damage: 30,
        rof: 20,
        range: 1536,
        proj_speed: 40,
        proj_rot: 0,
        invisible: false,
        instant: false,
        warhead: WarheadProfile {
            spread: 3,
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

/// An instant (hitscan) weapon so damage lands on the firing tick — lets a combat
/// revert-sensitivity test connect reliably inside a short curtain window.
fn zap() -> WeaponProfile {
    WeaponProfile {
        damage: 40,
        rof: 5,
        range: 2048,
        proj_speed: 0,
        proj_rot: 0,
        invisible: false,
        instant: true,
        warhead: WarheadProfile {
            spread: 1,
            verses: pct5([100, 100, 100, 100, 100]),
        },
        warhead_ap: false,
        arcing: false,
        ballistic_scatter: 0,
        homing_scatter: 0,
        min_damage: 1,
        max_damage: 1000,
    }
}

fn catalog() -> Catalog {
    let econ = EconRules {
        ticks_per_minute: TPM,
        ..EconRules::default()
    };
    Catalog {
        buildings: vec![
            bproto("PROC", 3, 3, 900, true, 2000), // B_PROC refinery+storage
            bproto("TARGET", 2, 2, 300, false, 0), // B_TARGET plain
            bproto("MSLO", 2, 1, 400, false, 0),   // B_MSLO nuke
            bproto("IRON", 2, 2, 400, false, 0),   // B_IRON iron curtain
            bproto("PDOX", 2, 2, 400, false, 0),   // B_PDOX chronosphere
            bproto("DOME", 2, 2, 500, false, 0),   // B_DOME radar dome
            bproto("SILO", 1, 1, 300, false, 3000), // B_SILO storage, not refinery
        ],
        units: vec![
            uproto("SPY", true, None),
            uproto("THF", true, None),
            uproto("E7", true, Some(cannon())),
            uproto("TANK", false, Some(cannon())),
            uproto("DOG", true, None),
        ],
        econ,
    }
}

fn world() -> World {
    let mut w = World::new(Passability::all_passable(), 0xF00D_5EED);
    w.set_catalog(catalog());
    w.init_houses(2, 10_000);
    w.enable_shroud();
    w
}

fn spawn_spec(w: &mut World, id: u32, name: &str, house: u8, cell: CellCoord) -> ra_sim::Handle {
    let h = w.spawn_unit(id, house, cell, Facing(0), 100, stats());
    if let Some(u) = w.units.get_mut(h) {
        u.make_infantry(0);
    }
    let (spy, thief, bomber, canine) = match name {
        "SPY" => (true, false, false, false),
        "THF" => (false, true, false, false),
        "E7" => (false, false, true, false),
        "DOG" => (false, false, false, true),
        _ => (false, false, false, false),
    };
    w.set_unit_specialist(h, spy, thief, bomber, canine);
    if name == "E7" {
        w.set_unit_combat(h, 0, Some(cannon()), false);
    }
    h
}

// ===========================================================================
// Superweapon framework: charge cycle, fire gate, presence tracking.
// ===========================================================================

#[test]
fn nuke_recharge_is_exactly_recharge_minutes_times_tpm() {
    // Nuclear recharge = 13 min. At TPM=60 that is 780 ticks; permille after the
    // first tick reflects (recharge-control)/recharge. Pin the total charge length
    // by force-charging is bypass; here we assert the *seeded* recharge value via
    // the permille curve reaching 1000 only at full charge.
    let mut w = world();
    w.spawn_building(B_MSLO, 0, CellCoord::new(2, 2))
        .expect("mslo");
    w.tick(&[]); // sync creates the SW, then charges one tick
                 // recharge = 13*60 = 780; after 1 charge tick control = 779, permille = 1000/780.
    let p = w
        .superweapon_charge_permille(0, SuperKind::Nuclear)
        .unwrap();
    assert_eq!(
        p,
        (1000 / 780),
        "one tick of a 780-tick recharge is ~1 permille"
    );
    assert!(!w.superweapon_ready(0, SuperKind::Nuclear));
    // It is not ready until the full 780 ticks elapse (spot check a mid value).
    for _ in 0..389 {
        w.tick(&[]);
    }
    // 390 ticks total charged of 780 => ~50%.
    let p = w
        .superweapon_charge_permille(0, SuperKind::Nuclear)
        .unwrap();
    assert_eq!(
        p,
        390 * 1000 / 780,
        "half-charged at half the recharge time"
    );
    assert!(!w.superweapon_ready(0, SuperKind::Nuclear));
}

#[test]
fn charge_permille_is_monotone_and_reaches_1000_only_when_ready() {
    let mut w = world();
    w.spawn_building(B_PDOX, 0, CellCoord::new(2, 2))
        .expect("pdox");
    w.tick(&[]);
    // Chrono recharge = 7 min = 420 ticks.
    let mut last = w
        .superweapon_charge_permille(0, SuperKind::Chronosphere)
        .unwrap();
    for _ in 0..419 {
        w.tick(&[]);
        let p = w
            .superweapon_charge_permille(0, SuperKind::Chronosphere)
            .unwrap();
        assert!(p >= last, "charge permille never decreases while charging");
        assert!(p < 1000 || w.superweapon_ready(0, SuperKind::Chronosphere));
        last = p;
    }
    // 420 charge ticks => ready, permille pinned to 1000.
    assert!(w.superweapon_ready(0, SuperKind::Chronosphere));
    assert_eq!(
        w.superweapon_charge_permille(0, SuperKind::Chronosphere),
        Some(1000)
    );
}

#[test]
fn firing_an_uncharged_superweapon_is_rejected() {
    let mut w = world();
    w.spawn_building(B_MSLO, 0, CellCoord::new(2, 2))
        .expect("mslo");
    w.tick(&[]);
    assert!(!w.superweapon_ready(0, SuperKind::Nuclear));
    // Fire while not ready: no strike is launched.
    w.tick(&[Command::FireSuperWeapon {
        house: 0,
        kind: SuperKind::Nuclear,
        target: Target::Cell(CellCoord::new(40, 40)),
        dest: None,
    }]);
    assert!(
        w.nuke_strikes().is_empty(),
        "an uncharged nuke cannot be fired"
    );
}

#[test]
fn firing_resets_the_recharge_and_it_climbs_again_from_zero() {
    let mut w = world();
    w.spawn_building(B_MSLO, 0, CellCoord::new(2, 2))
        .expect("mslo");
    w.tick(&[]);
    w.force_charge_superweapon(0, SuperKind::Nuclear);
    assert!(w.superweapon_ready(0, SuperKind::Nuclear));
    w.tick(&[Command::FireSuperWeapon {
        house: 0,
        kind: SuperKind::Nuclear,
        target: Target::Cell(CellCoord::new(40, 40)),
        dest: None,
    }]);
    // Discharged: no longer ready and the permille dropped back near zero.
    assert!(!w.superweapon_ready(0, SuperKind::Nuclear));
    let p = w
        .superweapon_charge_permille(0, SuperKind::Nuclear)
        .unwrap();
    assert!(p < 20, "recharge restarted from the top ({p} permille)");
}

#[test]
fn presence_tracks_building_ownership_appear_and_disappear() {
    let mut w = world();
    // No superweapon building yet.
    w.tick(&[]);
    assert!(
        w.superweapons().is_empty(),
        "no SW without a granting building"
    );
    // Build MSLO -> nuke appears (present, charging).
    w.spawn_building(B_MSLO, 0, CellCoord::new(2, 2))
        .expect("mslo");
    w.tick(&[]);
    assert!(
        w.superweapon_charge_permille(0, SuperKind::Nuclear)
            .is_some(),
        "MSLO grants the nuke"
    );
    // Destroy MSLO -> nuke gone next sync.
    let mslo = w
        .buildings
        .iter()
        .find(|(_, b)| b.type_id == B_MSLO)
        .map(|(h, _)| h)
        .unwrap();
    w.tick(&[Command::Sell {
        house: 0,
        building: mslo,
    }]);
    assert!(
        w.superweapon_charge_permille(0, SuperKind::Nuclear)
            .is_none(),
        "removing MSLO removes the nuke"
    );
    assert!(w.superweapons().is_empty());
}

#[test]
fn low_power_suspends_charging_then_resumes_when_restored() {
    let mut w = world();
    w.spawn_building(B_MSLO, 0, CellCoord::new(2, 2))
        .expect("mslo");
    w.tick(&[]);
    // Force the house into low power (drain exceeds output).
    w.houses[0].power_output = 50;
    w.houses[0].power_drain = 100;
    assert!(w.houses[0].low_power());
    let p_before = w
        .superweapon_charge_permille(0, SuperKind::Nuclear)
        .unwrap();
    for _ in 0..50 {
        w.tick(&[]);
    }
    let p_suspended = w
        .superweapon_charge_permille(0, SuperKind::Nuclear)
        .unwrap();
    assert_eq!(
        p_suspended, p_before,
        "charging is frozen while the house is low on power"
    );
    // Restore power -> charging resumes.
    w.houses[0].power_output = 200;
    assert!(!w.houses[0].low_power());
    w.tick(&[]);
    let p_after = w
        .superweapon_charge_permille(0, SuperKind::Nuclear)
        .unwrap();
    assert!(p_after > p_suspended, "charging resumes once power is back");
}

// ===========================================================================
// Nuclear strike: fall delay, radius boundary, damage magnitude.
// ===========================================================================

/// Fire a charged nuke at (40,40) and report the tick count from the fire tick
/// until the strike leaves the air (detonates).
fn measure_nuke_fall(w: &mut World) -> u32 {
    w.tick(&[Command::FireSuperWeapon {
        house: 0,
        kind: SuperKind::Nuclear,
        target: Target::Cell(CellCoord::new(40, 40)),
        dest: None,
    }]);
    assert_eq!(w.nuke_strikes().len(), 1, "nuke launched");
    let mut ticks = 0u32;
    while !w.nuke_strikes().is_empty() {
        w.tick(&[]);
        ticks += 1;
        assert!(ticks < 100, "nuke never fell");
    }
    ticks
}

#[test]
fn nuke_falls_for_the_full_fall_delay_before_detonating() {
    let mut w = world();
    w.spawn_building(B_MSLO, 0, CellCoord::new(2, 2))
        .expect("mslo");
    w.tick(&[]);
    w.force_charge_superweapon(0, SuperKind::Nuclear);
    // NUKE_FALL_TICKS = 20; the strike is created at 20 and detonates when it hits
    // 0. One decrement happens on the fire tick (command applied before systems),
    // so it clears the air 19 ticks after the fire tick.
    let ticks = measure_nuke_fall(&mut w);
    assert_eq!(ticks, 19, "nuke falls for the pinned fall delay");
}

#[test]
fn nuke_area_boundary_is_exactly_three_cells_chebyshev() {
    let mut w = world();
    w.spawn_building(B_MSLO, 0, CellCoord::new(2, 2))
        .expect("mslo");
    let gz = CellCoord::new(40, 40);
    // Inside the radius: Chebyshev distance 3 (corner) -> destroyed.
    let inside = w.spawn_unit(U_TANK, 1, CellCoord::new(43, 43), Facing(0), 100, stats());
    w.set_unit_combat(inside, 0, None, false);
    // Outside: Chebyshev distance 4 -> untouched.
    let outside = w.spawn_unit(U_TANK, 1, CellCoord::new(44, 40), Facing(0), 100, stats());
    w.set_unit_combat(outside, 0, None, false);
    w.tick(&[]);
    w.force_charge_superweapon(0, SuperKind::Nuclear);
    w.tick(&[Command::FireSuperWeapon {
        house: 0,
        kind: SuperKind::Nuclear,
        target: Target::Cell(gz),
        dest: None,
    }]);
    for _ in 0..25 {
        w.tick(&[]);
    }
    assert!(
        !w.units.contains(inside),
        "unit at Chebyshev 3 is inside the blast"
    );
    assert!(
        w.units.contains(outside),
        "unit at Chebyshev 4 is outside the blast"
    );
}

#[test]
fn nuke_deals_exactly_200_to_everything_in_radius() {
    let mut w = world();
    w.spawn_building(B_MSLO, 0, CellCoord::new(2, 2))
        .expect("mslo");
    // A 250-hp tank at ground zero should survive with exactly 50.
    let tough = w.spawn_unit(U_TANK, 1, CellCoord::new(40, 40), Facing(0), 250, stats());
    w.set_unit_combat(tough, 0, None, false);
    w.tick(&[]);
    w.force_charge_superweapon(0, SuperKind::Nuclear);
    w.tick(&[Command::FireSuperWeapon {
        house: 0,
        kind: SuperKind::Nuclear,
        target: Target::Cell(CellCoord::new(40, 40)),
        dest: None,
    }]);
    for _ in 0..25 {
        w.tick(&[]);
    }
    assert_eq!(
        w.units.get(tough).map(|u| u.health),
        Some(50),
        "nuke dealt exactly 200 damage (250 - 200 = 50)"
    );
}

// ===========================================================================
// Iron curtain: fired duration + the exact damage-gate boundary (via C4).
// ===========================================================================

#[test]
fn fired_iron_curtain_lasts_exactly_tpm_over_two_ticks() {
    let mut w = world();
    w.spawn_building(B_IRON, 0, CellCoord::new(2, 2))
        .expect("iron");
    let tank = w.spawn_unit(U_TANK, 0, CellCoord::new(20, 20), Facing(0), 100, stats());
    w.set_unit_combat(tank, 0, None, false);
    w.tick(&[]);
    w.force_charge_superweapon(0, SuperKind::IronCurtain);
    w.tick(&[Command::FireSuperWeapon {
        house: 0,
        kind: SuperKind::IronCurtain,
        target: Target::Unit(tank),
        dest: None,
    }]);
    // dur = TPM/2 = 30; decremented once on the fire tick => 29 remaining.
    assert_eq!(
        w.units.get(tank).map(|u| u.iron_curtain),
        Some(29),
        "curtain seeded to TPM/2 and ticked once on the fire tick"
    );
    // It reaches zero after exactly 29 more ticks.
    for i in 0..29 {
        assert!(
            w.units
                .get(tank)
                .map(|u| u.iron_curtain > 0)
                .unwrap_or(false),
            "still curtained before expiry (i={i})"
        );
        w.tick(&[]);
    }
    assert_eq!(
        w.units.get(tank).map(|u| u.iron_curtain),
        Some(0),
        "curtain expired after exactly TPM/2 ticks"
    );
}

#[test]
fn iron_curtain_damage_gate_boundary_is_exact_via_c4() {
    // The curtain decrements before the C4 fuse is checked each tick, so a building
    // survives a fuse of F iff its curtain C > F (curtain still > 0 at the blast).
    // Boundary: C == F blows, C == F+1 survives. This pins the countdown rate AND
    // the `iron_curtain == 0` gate deterministically (no weapon cadence).
    fn survives(fuse: u16, curtain: u16) -> bool {
        let mut w = world();
        let b = w
            .spawn_building(B_TARGET, 1, CellCoord::new(10, 10))
            .expect("target");
        {
            let bld = w.buildings.get_mut(b).unwrap();
            bld.c4_fuse = fuse;
            bld.c4_by = 0;
            bld.iron_curtain = curtain;
        }
        for _ in 0..(fuse as usize + 2) {
            w.tick(&[]);
        }
        w.buildings.contains(b)
    }
    assert!(
        !survives(10, 10),
        "C == F: curtain hits 0 the blast tick -> blows"
    );
    assert!(
        survives(10, 11),
        "C == F+1: curtain still active -> survives"
    );
    assert!(!survives(10, 9), "C < F: curtain long gone -> blows");
}

#[test]
fn iron_curtained_unit_takes_no_combat_damage_then_normal_damage_after_lapse() {
    // Isolates the `explosion_damage` unit guard (line ~5551) with a hitscan weapon
    // so the attacker connects every few ticks inside the curtain window. This is
    // the revert-sensitive combat test: remove the guard and phase 1 shows damage.
    let mut w = world();
    let tank = w.spawn_unit(U_TANK, 0, CellCoord::new(20, 20), Facing(0), 100, stats());
    w.set_unit_combat(tank, 0, None, false);
    // Manually curtain the tank for a long window.
    w.units.get_mut(tank).unwrap().iron_curtain = 200;
    let atk = w.spawn_unit(U_TANK, 1, CellCoord::new(21, 20), Facing(0), 100, stats());
    w.set_unit_combat(atk, 0, Some(zap()), false);
    let before = w.units.get(tank).unwrap().health;
    // Phase 1: many attacks while curtained -> zero damage.
    for _ in 0..30 {
        w.tick(&[Command::Attack {
            unit: atk,
            target: Target::Unit(tank),
            house: 1,
        }]);
    }
    assert_eq!(
        w.units.get(tank).map(|u| u.health),
        Some(before),
        "no combat damage while iron-curtained"
    );
    // Phase 2: drop the curtain -> the same fire now bites (proves the attacker
    // actually connects, so phase 1 was not vacuous).
    w.units.get_mut(tank).unwrap().iron_curtain = 0;
    let mut hurt = false;
    for _ in 0..20 {
        w.tick(&[Command::Attack {
            unit: atk,
            target: Target::Unit(tank),
            house: 1,
        }]);
        if w.units.get(tank).map(|u| u.health < before).unwrap_or(true) {
            hurt = true;
            break;
        }
    }
    assert!(hurt, "takes normal damage once the curtain lapses");
}

#[test]
fn iron_curtained_unit_and_building_survive_a_direct_nuke() {
    // Isolates the `nuke_detonate` iron-curtain guards (unit ~4617, building ~4638),
    // which are separate from the `explosion_damage` guards.
    let mut w = world();
    w.spawn_building(B_MSLO, 1, CellCoord::new(2, 2))
        .expect("mslo");
    let tank = w.spawn_unit(U_TANK, 0, CellCoord::new(40, 40), Facing(0), 100, stats());
    w.set_unit_combat(tank, 0, None, false);
    let bld = w
        .spawn_building(B_TARGET, 0, CellCoord::new(41, 41))
        .expect("bld");
    w.units.get_mut(tank).unwrap().iron_curtain = 400;
    w.buildings.get_mut(bld).unwrap().iron_curtain = 400;
    w.tick(&[]);
    w.force_charge_superweapon(1, SuperKind::Nuclear);
    w.tick(&[Command::FireSuperWeapon {
        house: 1,
        kind: SuperKind::Nuclear,
        target: Target::Cell(CellCoord::new(40, 40)),
        dest: None,
    }]);
    for _ in 0..25 {
        w.tick(&[]);
    }
    assert!(
        w.units.contains(tank),
        "curtained unit survives a direct nuke"
    );
    assert!(
        w.buildings.contains(bld),
        "curtained building survives a direct nuke"
    );
    assert_eq!(
        w.units.get(tank).map(|u| u.health),
        Some(100),
        "no nuke damage taken"
    );
}

// ===========================================================================
// Chronosphere: one-way vehicle teleport, infantry killed.
// ===========================================================================

#[test]
fn chrono_teleports_vehicle_one_way_no_warp_back() {
    let mut w = world();
    w.spawn_building(B_PDOX, 0, CellCoord::new(2, 2))
        .expect("pdox");
    let tank = w.spawn_unit(U_TANK, 0, CellCoord::new(20, 20), Facing(0), 100, stats());
    w.tick(&[]);
    w.force_charge_superweapon(0, SuperKind::Chronosphere);
    let dest = CellCoord::new(45, 45);
    w.tick(&[Command::FireSuperWeapon {
        house: 0,
        kind: SuperKind::Chronosphere,
        target: Target::Unit(tank),
        dest: Some(dest),
    }]);
    assert_eq!(
        w.units.get(tank).map(|u| u.cell()),
        Some(dest),
        "warped to dest"
    );
    // One-way: it stays put over a long span (no MoebiusCountDown warp-back).
    for _ in 0..300 {
        w.tick(&[]);
    }
    assert_eq!(
        w.units.get(tank).map(|u| u.cell()),
        Some(dest),
        "no warp-back: the teleport is permanent (documented deviation)"
    );
}

#[test]
fn chrono_kills_an_infantry_target_instead_of_teleporting_it() {
    let mut w = world();
    w.spawn_building(B_PDOX, 0, CellCoord::new(2, 2))
        .expect("pdox");
    let inf = spawn_spec(&mut w, U_THF, "THF", 0, CellCoord::new(20, 20));
    w.tick(&[]);
    w.force_charge_superweapon(0, SuperKind::Chronosphere);
    w.tick(&[Command::FireSuperWeapon {
        house: 0,
        kind: SuperKind::Chronosphere,
        target: Target::Unit(inf),
        dest: Some(CellCoord::new(45, 45)),
    }]);
    assert!(
        !w.units.contains(inf),
        "infantry cannot survive the chronoshift (killed, house.cpp:3021)"
    );
}

#[test]
fn chrono_without_a_destination_does_not_fire() {
    let mut w = world();
    w.spawn_building(B_PDOX, 0, CellCoord::new(2, 2))
        .expect("pdox");
    let tank = w.spawn_unit(U_TANK, 0, CellCoord::new(20, 20), Facing(0), 100, stats());
    w.tick(&[]);
    w.force_charge_superweapon(0, SuperKind::Chronosphere);
    w.tick(&[Command::FireSuperWeapon {
        house: 0,
        kind: SuperKind::Chronosphere,
        target: Target::Unit(tank),
        dest: None,
    }]);
    assert_eq!(
        w.units.get(tank).map(|u| u.cell()),
        Some(CellCoord::new(20, 20))
    );
    // Not discharged: still ready (nothing happened).
    assert!(
        w.superweapon_ready(0, SuperKind::Chronosphere),
        "no dest -> no fire, still charged"
    );
}

// ===========================================================================
// Specialists: thief / spy fractions, consumed-vs-persist, C4 fuse, disguise.
// ===========================================================================

#[test]
fn thief_steals_exactly_half_from_a_non_refinery_storage_silo() {
    let mut w = world();
    w.set_house_credits(0, 0);
    w.set_house_credits(1, 6000);
    let silo = w
        .spawn_building(B_SILO, 1, CellCoord::new(10, 10))
        .expect("silo");
    let thf = spawn_spec(&mut w, U_THF, "THF", 0, CellCoord::new(9, 10));
    w.tick(&[Command::Attack {
        unit: thf,
        target: Target::Building(silo),
        house: 0,
    }]);
    assert!(!w.units.contains(thf), "thief consumed on infiltration");
    assert_eq!(w.house_credits(0), 3000, "half of 6000 stolen");
    assert_eq!(w.house_credits(1), 3000, "victim lost half");
}

#[test]
fn thief_refuses_a_non_storage_building() {
    let mut w = world();
    w.set_house_credits(0, 0);
    w.set_house_credits(1, 6000);
    let plain = w
        .spawn_building(B_TARGET, 1, CellCoord::new(10, 10))
        .expect("plain");
    let thf = spawn_spec(&mut w, U_THF, "THF", 0, CellCoord::new(9, 10));
    w.tick(&[Command::Attack {
        unit: thf,
        target: Target::Building(plain),
        house: 0,
    }]);
    // Still consumed on arrival, but no credits move (no storage capacity).
    assert_eq!(
        w.house_credits(1),
        6000,
        "no steal from a storage-less building"
    );
    assert_eq!(w.house_credits(0), 0);
}

#[test]
fn spy_on_a_radar_dome_reveals_the_whole_map() {
    let mut w = world();
    let dome = w
        .spawn_building(B_DOME, 1, CellCoord::new(30, 30))
        .expect("dome");
    let spy = spawn_spec(&mut w, U_SPY, "SPY", 0, CellCoord::new(29, 30));
    let before = w.shroud.explored_count(0);
    w.tick(&[Command::Attack {
        unit: spy,
        target: Target::Building(dome),
        house: 0,
    }]);
    assert!(!w.units.contains(spy), "spy consumed");
    let after = w.shroud.explored_count(0);
    // A radar-dome spy reveals every cell (RadarSpied) -> explored jumps far beyond
    // a 10-cell disc.
    assert!(
        after > before + 400,
        "radar-dome spy reveals the whole map (before={before} after={after})"
    );
}

#[test]
fn spy_on_a_plain_building_reveals_only_a_local_disc_and_leaks_no_credit() {
    let mut w = world();
    w.set_house_credits(0, 0);
    w.set_house_credits(1, 8000);
    let plain = w
        .spawn_building(B_TARGET, 1, CellCoord::new(30, 30))
        .expect("plain");
    let spy = spawn_spec(&mut w, U_SPY, "SPY", 0, CellCoord::new(29, 30));
    let before = w.shroud.explored_count(0);
    w.tick(&[Command::Attack {
        unit: spy,
        target: Target::Building(plain),
        house: 0,
    }]);
    let after = w.shroud.explored_count(0);
    assert!(after > before, "a local disc is revealed");
    // Non-refinery, non-dome: no credit leak (the leak is refinery-only).
    assert_eq!(
        w.house_credits(1),
        8000,
        "no credit leak from a plain building"
    );
    assert_eq!(w.house_credits(0), 0);
}

#[test]
fn spy_leaks_exactly_a_quarter_of_refinery_credits() {
    let mut w = world();
    w.set_house_credits(0, 0);
    w.set_house_credits(1, 8000);
    let proc = w
        .spawn_building(B_PROC, 1, CellCoord::new(10, 10))
        .expect("proc");
    let spy = spawn_spec(&mut w, U_SPY, "SPY", 0, CellCoord::new(9, 10));
    w.tick(&[Command::Attack {
        unit: spy,
        target: Target::Building(proc),
        house: 0,
    }]);
    // Documented divergence: spy leaks a QUARTER of a refinery (the thief takes half).
    assert_eq!(w.house_credits(0), 2000, "spy leaks 1/4 of 8000");
    assert_eq!(w.house_credits(1), 6000, "victim keeps 3/4");
}

#[test]
fn tanya_c4_at_tiny_tpm_blows_on_the_planting_pass_and_tanya_survives() {
    // C4Delay = 0.03 min. fuse = (C4Delay * TPM).max(1). At TPM=60 that is
    // (60*3/100).max(1) = 1. NOTE: `run_infiltrators` (plant) and
    // `run_superweapons`/`tick_building_timers` (fuse decrement) run in the SAME
    // apply() pass, infiltrators first — so the 1-tick fuse decrements to 0 and
    // detonates on the very tick it is planted.
    let mut w = world();
    let target = w
        .spawn_building(B_TARGET, 1, CellCoord::new(10, 10))
        .expect("target");
    let tanya = spawn_spec(&mut w, U_E7, "E7", 0, CellCoord::new(9, 10));
    w.tick(&[Command::Attack {
        unit: tanya,
        target: Target::Building(target),
        house: 0,
    }]);
    assert!(
        w.units.contains(tanya),
        "Tanya is NOT consumed by planting C4"
    );
    assert!(
        !w.buildings.contains(target),
        "a 1-tick fuse blows on the planting pass (same-tick decrement)"
    );
}

#[test]
fn tanya_c4_fuse_is_round_c4delay_times_tpm_and_counts_down_exactly() {
    // At a stock-like TPM=900, fuse armed = 900*3/100 = 27. The same-pass decrement
    // leaves 26 remaining once the planting tick completes; the building then
    // survives exactly 26 more ticks and blows on the 26th.
    let mut w = World::new(Passability::all_passable(), 0xF00D_5EED);
    let mut cat = catalog();
    cat.econ.ticks_per_minute = 900;
    w.set_catalog(cat);
    w.init_houses(2, 10_000);
    w.enable_shroud();
    let target = w
        .spawn_building(B_TARGET, 1, CellCoord::new(10, 10))
        .expect("target");
    let tanya = spawn_spec(&mut w, U_E7, "E7", 0, CellCoord::new(9, 10));
    w.tick(&[Command::Attack {
        unit: tanya,
        target: Target::Building(target),
        house: 0,
    }]);
    assert_eq!(
        w.buildings.get(target).map(|b| b.c4_fuse),
        Some(26),
        "armed 27, ticked once on the planting pass -> 26 remaining"
    );
    for _ in 0..25 {
        w.tick(&[]);
        assert!(
            w.buildings.contains(target),
            "building stands while the fuse burns"
        );
    }
    w.tick(&[]); // 26th post-plant tick: fuse hits 0
    assert!(
        !w.buildings.contains(target),
        "C4 fuse expired -> demolished"
    );
}

#[test]
fn an_iron_curtained_building_is_immune_to_tanya_c4() {
    let mut w = world();
    let target = w
        .spawn_building(B_TARGET, 1, CellCoord::new(10, 10))
        .expect("target");
    // Curtain the building generously.
    w.buildings.get_mut(target).unwrap().iron_curtain = 500;
    let tanya = spawn_spec(&mut w, U_E7, "E7", 0, CellCoord::new(9, 10));
    w.tick(&[Command::Attack {
        unit: tanya,
        target: Target::Building(target),
        house: 0,
    }]);
    // No fuse is armed on a curtained building (infantry.cpp:919).
    assert_eq!(
        w.buildings.get(target).map(|b| b.c4_fuse),
        Some(0),
        "C4 refused on an iron-curtained building"
    );
    for _ in 0..40 {
        w.tick(&[]);
    }
    assert!(
        w.buildings.contains(target),
        "curtained building survives the C4 attempt"
    );
}

#[test]
fn disguised_spy_becomes_targetable_after_a_dog_strips_the_disguise() {
    let mut w = world();
    let guard = w.spawn_unit(U_TANK, 1, CellCoord::new(20, 20), Facing(0), 100, stats());
    w.set_unit_combat(guard, 0, Some(cannon()), false);
    let spy = spawn_spec(&mut w, U_SPY, "SPY", 0, CellCoord::new(21, 20));
    assert!(w.units.get(spy).unwrap().disguised);
    // Hidden: guard never acquires.
    for _ in 0..10 {
        w.tick(&[]);
    }
    assert!(
        w.units
            .get(guard)
            .map(|u| u.target.is_none())
            .unwrap_or(false),
        "disguised spy not acquired"
    );
    // Enemy dog adjacent -> strips disguise.
    let dog = spawn_spec(&mut w, U_DOG, "DOG", 1, CellCoord::new(22, 20));
    let _ = dog;
    w.tick(&[]);
    assert!(
        !w.units.get(spy).map(|u| u.disguised).unwrap_or(true),
        "disguise stripped"
    );
    // Now the guard CAN acquire it.
    let mut acquired = false;
    for _ in 0..20 {
        w.tick(&[]);
        if w.units
            .get(guard)
            .map(|u| u.target.is_some())
            .unwrap_or(false)
        {
            acquired = true;
            break;
        }
    }
    assert!(acquired, "a revealed spy is a valid acquisition target");
}

#[test]
fn an_allied_dog_does_not_strip_its_own_houses_spy() {
    // Same-house (allied) dog must not reveal the spy (is_hidden_spy allies gate).
    let mut w = world();
    let spy = spawn_spec(&mut w, U_SPY, "SPY", 0, CellCoord::new(21, 20));
    let _friendly_dog = spawn_spec(&mut w, U_DOG, "DOG", 0, CellCoord::new(22, 20));
    for _ in 0..5 {
        w.tick(&[]);
    }
    assert!(
        w.units.get(spy).map(|u| u.disguised).unwrap_or(false),
        "a friendly dog leaves its own spy disguised"
    );
}

// ===========================================================================
// Determinism: a full charge+fire sequence is byte-identical same-seed-twice.
// ===========================================================================

fn scripted_sw_run() -> Vec<u64> {
    let mut w = world();
    w.spawn_building(B_MSLO, 0, CellCoord::new(2, 2))
        .expect("mslo");
    w.spawn_building(B_IRON, 0, CellCoord::new(6, 2))
        .expect("iron");
    let tank = w.spawn_unit(U_TANK, 0, CellCoord::new(10, 10), Facing(0), 100, stats());
    w.set_unit_combat(tank, 0, None, false);
    for i in 0..9 {
        w.spawn_unit(
            U_TANK,
            1,
            CellCoord::new(40 + (i % 3), 40 + (i / 3)),
            Facing(0),
            100,
            stats(),
        );
    }
    let mut hashes = Vec::new();
    hashes.push(w.tick(&[]));
    w.force_charge_superweapon(0, SuperKind::Nuclear);
    w.force_charge_superweapon(0, SuperKind::IronCurtain);
    hashes.push(w.tick(&[
        Command::FireSuperWeapon {
            house: 0,
            kind: SuperKind::IronCurtain,
            target: Target::Unit(tank),
            dest: None,
        },
        Command::FireSuperWeapon {
            house: 0,
            kind: SuperKind::Nuclear,
            target: Target::Cell(CellCoord::new(40, 40)),
            dest: None,
        },
    ]));
    for _ in 0..40 {
        hashes.push(w.tick(&[]));
    }
    hashes
}

#[test]
fn charge_and_fire_sequence_is_deterministic_same_seed_twice() {
    let a = scripted_sw_run();
    let b = scripted_sw_run();
    assert_eq!(
        a, b,
        "identical seed + script -> identical per-tick hash chain"
    );
}

// ===========================================================================
// AI superweapon use: highest-value target selection + RNG safety.
// ===========================================================================

#[test]
fn ai_nuke_aims_at_the_highest_value_enemy_building() {
    // The AI (house 1) owns a charged nuke. The player (house 0) owns a cheap
    // building far from an expensive one. `best_enemy_building_cell` picks max cost,
    // so the launched strike's ground-zero must be the expensive building's centre
    // (aim point), not the cheap one.
    let mut w = world();
    let cheap = w
        .spawn_building(B_TARGET, 0, CellCoord::new(50, 50))
        .expect("cheap");
    let pricey = w
        .spawn_building(B_TARGET, 0, CellCoord::new(10, 10))
        .expect("pricey");
    w.buildings.get_mut(cheap).unwrap().cost = 300;
    w.buildings.get_mut(pricey).unwrap().cost = 5000;
    let pricey_gz = w.buildings.get(pricey).unwrap().center_cell();
    let cheap_gz = w.buildings.get(cheap).unwrap().center_cell();
    w.spawn_building(B_MSLO, 1, CellCoord::new(4, 4))
        .expect("ai mslo");
    w.set_player_house(0);
    w.set_ai(vec![AiPlayer::new(1, Difficulty::Hard)]);
    w.tick(&[]);
    w.force_charge_superweapon(1, SuperKind::Nuclear);
    // Run until the AI launches a nuke, then read its aim point.
    let mut gz = None;
    for _ in 0..400 {
        w.tick(&[]);
        if let Some(n) = w.nuke_strikes().first() {
            gz = Some(n.cell);
            break;
        }
    }
    let gz = gz.expect("the AI launched a nuke within the window");
    assert_eq!(
        gz, pricey_gz,
        "AI aimed at the highest-value (cost 5000) building"
    );
    assert_ne!(gz, cheap_gz, "AI did not aim at the cheap building");
}

#[test]
fn ai_below_iq_super_weapons_never_fires() {
    // An AI whose IQ is below IQSuperWeapons must not fire even a charged nuke.
    let mut w = world();
    w.spawn_building(B_TARGET, 0, CellCoord::new(40, 40))
        .expect("victim");
    w.spawn_building(B_MSLO, 1, CellCoord::new(4, 4))
        .expect("ai mslo");
    w.set_player_house(0);
    w.set_ai(vec![AiPlayer::new(1, Difficulty::Hard)]);
    w.tick(&[]);
    w.set_house_iq(1, 0); // below econ.iq.super_weapons (4)
    w.force_charge_superweapon(1, SuperKind::Nuclear);
    for _ in 0..200 {
        w.tick(&[]);
    }
    assert!(
        w.nuke_strikes().is_empty() && w.superweapon_ready(1, SuperKind::Nuclear),
        "a sub-IQSuperWeapons AI holds fire (still charged, no strike)"
    );
}
