//! Marquee content arc smoke tests (ra-coder): infiltration specialists
//! (spy / thief / Tanya-C4) and superweapons (nuclear strike / iron curtain /
//! chronosphere), plus the superweapon charge/fire cycle. These prove the new
//! systems *run* and produce their headline effect; ra-tester owns the boundary
//! coverage (exact fuse ticks, disguise/dog interaction, AI firing, determinism
//! matrix, real-asset acceptance).

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

const U_SPY: u32 = 0;
const U_THF: u32 = 1;
const U_E7: u32 = 2;
const U_TANK: u32 = 3;

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
fn bproto(name: &str, w: u8, h: u8, max_health: u16, is_refinery: bool) -> BuildingProto {
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
        storage: if is_refinery { 2000 } else { 0 },
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

fn catalog() -> Catalog {
    // Shrink the minute so the iron-curtain window (ticks_per_minute/2) and the
    // recharge are test-length; force_charge bypasses recharge anyway.
    let econ = EconRules {
        ticks_per_minute: 60,
        ..EconRules::default()
    };
    Catalog {
        buildings: vec![
            bproto("PROC", 3, 3, 900, true),    // B_PROC (refinery, storage)
            bproto("TARGET", 2, 2, 300, false), // B_TARGET
            bproto("MSLO", 2, 1, 400, false),   // B_MSLO (nuke)
            bproto("IRON", 2, 2, 400, false),   // B_IRON (iron curtain)
            bproto("PDOX", 2, 2, 400, false),   // B_PDOX (chronosphere)
        ],
        units: vec![
            uproto("SPY", true, None),
            uproto("THF", true, None),
            uproto("E7", true, Some(cannon())),
            uproto("TANK", false, Some(cannon())),
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

/// Spawn an infantry specialist for `house` at `cell`, wiring its capability by
/// name (as the production/loader path does).
fn spawn_spec(w: &mut World, id: u32, name: &str, house: u8, cell: CellCoord) -> ra_sim::Handle {
    let h = w.spawn_unit(id, house, cell, Facing(0), 100, stats());
    if let Some(u) = w.units.get_mut(h) {
        u.make_infantry(0);
    }
    let (spy, thief, bomber, canine) = match name {
        "SPY" => (true, false, false, false),
        "THF" => (false, true, false, false),
        "E7" => (false, false, true, false),
        _ => (false, false, false, false),
    };
    w.set_unit_specialist(h, spy, thief, bomber, canine);
    if name == "E7" {
        w.set_unit_combat(h, 0, Some(cannon()), false);
    }
    h
}

#[test]
fn thief_steals_half_the_victims_money_from_a_refinery() {
    let mut w = world();
    w.set_house_credits(0, 0); // thief owner starts broke
    w.set_house_credits(1, 8000); // victim
    let _proc = w
        .spawn_building(B_PROC, 1, CellCoord::new(10, 10))
        .expect("refinery");
    let thf = spawn_spec(&mut w, U_THF, "THF", 0, CellCoord::new(9, 10));
    // Order the thief onto the enemy refinery; it is already adjacent.
    let proc_handle = w
        .buildings
        .iter()
        .find(|(_, b)| b.is_refinery)
        .map(|(h, _)| h)
        .unwrap();
    w.tick(&[Command::Attack {
        unit: thf,
        target: Target::Building(proc_handle),
        house: 0,
    }]);
    assert!(
        !w.units.contains(thf),
        "thief is consumed on infiltration (infantry.cpp:783)"
    );
    // Half of the victim's 8000 → 4000 transferred.
    assert_eq!(
        w.house_credits(0),
        4000,
        "thief steals half the victim's cash"
    );
    assert_eq!(w.house_credits(1), 4000, "victim loses half its cash");
}

#[test]
fn spy_reveals_the_map_and_leaks_refinery_credits() {
    let mut w = world();
    w.set_house_credits(0, 0);
    w.set_house_credits(1, 8000);
    w.spawn_building(B_PROC, 1, CellCoord::new(10, 10))
        .expect("refinery");
    let spy = spawn_spec(&mut w, U_SPY, "SPY", 0, CellCoord::new(9, 10));
    let explored_before = w.shroud.explored_count(0);
    let proc_handle = w
        .buildings
        .iter()
        .find(|(_, b)| b.is_refinery)
        .map(|(h, _)| h)
        .unwrap();
    w.tick(&[Command::Attack {
        unit: spy,
        target: Target::Building(proc_handle),
        house: 0,
    }]);
    assert!(!w.units.contains(spy), "spy consumed on infiltration");
    // Quarter of 8000 → 2000 leaked to the spy's house.
    assert_eq!(
        w.house_credits(0),
        2000,
        "spy leaks a quarter of refinery cash"
    );
    assert!(
        w.shroud.explored_count(0) > explored_before,
        "spy revealed shroud around the infiltrated building"
    );
}

#[test]
fn tanya_c4_demolishes_a_building() {
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
    // C4 planted; Tanya survives.
    assert!(
        w.units.contains(tanya),
        "Tanya is not consumed by planting C4"
    );
    // Fuse = round(20 * 0.03) = 1 tick here (tiny minute); run the fuse out.
    for _ in 0..5 {
        if !w.buildings.contains(target) {
            break;
        }
        w.tick(&[]);
    }
    assert!(
        !w.buildings.contains(target),
        "the C4 fuse blew the building up (building.cpp:995-1013)"
    );
}

#[test]
fn iron_curtain_makes_a_unit_invulnerable_for_the_duration() {
    let mut w = world();
    // House 0 owns the iron-curtain building; protect its own tank.
    w.spawn_building(B_IRON, 0, CellCoord::new(2, 2))
        .expect("iron");
    let tank = w.spawn_unit(U_TANK, 0, CellCoord::new(20, 20), Facing(0), 100, stats());
    w.set_unit_combat(tank, 0, None, false);
    // Charge the iron curtain and fire it on the tank.
    w.tick(&[]); // sync creates the superweapon entry
    w.force_charge_superweapon(0, SuperKind::IronCurtain);
    assert!(w.superweapon_ready(0, SuperKind::IronCurtain));
    w.tick(&[Command::FireSuperWeapon {
        house: 0,
        kind: SuperKind::IronCurtain,
        target: Target::Unit(tank),
        dest: None,
    }]);
    let curtain = w.units.get(tank).unwrap().iron_curtain;
    assert!(curtain > 0, "iron curtain armed on the tank");
    // Hammer it with an enemy tank while curtained — no damage.
    let atk = w.spawn_unit(U_TANK, 1, CellCoord::new(21, 20), Facing(0), 100, stats());
    w.set_unit_combat(atk, 0, Some(cannon()), false);
    let before = w.units.get(tank).unwrap().health;
    for _ in 0..8 {
        if w.units.get(tank).map(|u| u.iron_curtain).unwrap_or(0) == 0 {
            break;
        }
        w.tick(&[Command::Attack {
            unit: atk,
            target: Target::Unit(tank),
            house: 1,
        }]);
    }
    assert_eq!(
        w.units.get(tank).map(|u| u.health),
        Some(before),
        "no damage while iron-curtained (techno.cpp:4102)"
    );
    // After the curtain lapses, the same fire does damage.
    for _ in 0..40 {
        w.tick(&[Command::Attack {
            unit: atk,
            target: Target::Unit(tank),
            house: 1,
        }]);
        if w.units.get(tank).map(|u| u.health < before).unwrap_or(true) {
            break;
        }
    }
    assert!(
        w.units.get(tank).map(|u| u.health < before).unwrap_or(true),
        "takes normal damage once the curtain expires"
    );
}

#[test]
fn chronosphere_teleports_a_vehicle() {
    let mut w = world();
    w.spawn_building(B_PDOX, 0, CellCoord::new(2, 2))
        .expect("pdox");
    let tank = w.spawn_unit(U_TANK, 0, CellCoord::new(20, 20), Facing(0), 100, stats());
    w.tick(&[]);
    w.force_charge_superweapon(0, SuperKind::Chronosphere);
    let dest = CellCoord::new(40, 40);
    w.tick(&[Command::FireSuperWeapon {
        house: 0,
        kind: SuperKind::Chronosphere,
        target: Target::Unit(tank),
        dest: Some(dest),
    }]);
    assert_eq!(
        w.units.get(tank).map(|u| u.cell()),
        Some(dest),
        "chronosphere warped the tank to the destination cell"
    );
}

#[test]
fn nuclear_strike_devastates_the_target_area() {
    let mut w = world();
    w.spawn_building(B_MSLO, 0, CellCoord::new(2, 2))
        .expect("mslo");
    // A cluster of enemy tanks at ground zero.
    let mut victims = Vec::new();
    for dx in -1..=1 {
        for dy in -1..=1 {
            let h = w.spawn_unit(
                U_TANK,
                1,
                CellCoord::new(40 + dx, 40 + dy),
                Facing(0),
                100,
                stats(),
            );
            w.set_unit_combat(h, 0, None, false);
            victims.push(h);
        }
    }
    w.tick(&[]);
    w.force_charge_superweapon(0, SuperKind::Nuclear);
    w.tick(&[Command::FireSuperWeapon {
        house: 0,
        kind: SuperKind::Nuclear,
        target: Target::Cell(CellCoord::new(40, 40)),
        dest: None,
    }]);
    assert_eq!(w.nuke_strikes().len(), 1, "a nuke is in flight");
    // Let it fall and detonate.
    for _ in 0..25 {
        w.tick(&[]);
    }
    let survivors = victims.iter().filter(|h| w.units.contains(**h)).count();
    assert_eq!(survivors, 0, "the nuclear blast wiped out the cluster");
    // Recharge restarted (no longer ready).
    assert!(!w.superweapon_ready(0, SuperKind::Nuclear));
}

#[test]
fn a_disguised_spy_is_hidden_from_guards_until_a_dog_sniffs_it_out() {
    let mut w = world();
    // Enemy guard tank (house 1) sitting next to a disguised spy (house 0).
    let guard = w.spawn_unit(U_TANK, 1, CellCoord::new(20, 20), Facing(0), 100, stats());
    w.set_unit_combat(guard, 0, Some(cannon()), false);
    let spy = spawn_spec(&mut w, U_SPY, "SPY", 0, CellCoord::new(21, 20));
    assert!(w.units.get(spy).unwrap().disguised, "spy spawns disguised");
    // The guard does not auto-acquire the disguised spy.
    for _ in 0..10 {
        w.tick(&[]);
    }
    assert!(
        w.units
            .get(guard)
            .map(|u| u.target.is_none())
            .unwrap_or(false),
        "a disguised enemy spy is invisible to guard acquisition"
    );
    // Bring an enemy dog adjacent → it strips the disguise.
    let _dog = w.spawn_unit(U_TANK, 1, CellCoord::new(22, 20), Facing(0), 100, stats());
    // Make it a canine.
    let dog = w
        .units
        .iter()
        .filter(|(_, u)| u.house == 1 && u.cell() == CellCoord::new(22, 20))
        .map(|(h, _)| h)
        .next()
        .unwrap();
    w.set_unit_specialist(dog, false, false, false, true);
    w.tick(&[]);
    assert!(
        !w.units.get(spy).map(|u| u.disguised).unwrap_or(true),
        "an adjacent enemy dog sniffs out the spy (strips disguise)"
    );
}

#[test]
fn ai_fires_its_nuke_at_the_enemy_base_when_charged() {
    let mut w = world();
    // Player (house 0) owns a valuable building; the AI (house 1) owns a silo.
    let victim = w
        .spawn_building(B_PROC, 0, CellCoord::new(40, 40))
        .expect("player refinery");
    w.spawn_building(B_MSLO, 1, CellCoord::new(4, 4))
        .expect("ai mslo");
    w.set_player_house(0);
    w.set_ai(vec![AiPlayer::new(1, Difficulty::Hard)]); // IQ = MaxIQ ≥ IQSuperWeapons
                                                        // First tick creates + the AI house's superweapon; force it charged.
    w.tick(&[]);
    w.force_charge_superweapon(1, SuperKind::Nuclear);
    // The AI should fire within a few decide cadences (90% per attempt).
    let mut fired = false;
    for _ in 0..200 {
        w.tick(&[]);
        if !w.nuke_strikes().is_empty() || !w.buildings.contains(victim) {
            fired = true;
            break;
        }
    }
    assert!(fired, "the AI fired its charged nuke at the enemy base");
}

#[test]
fn superweapon_charges_over_time_and_becomes_ready() {
    let mut w = world();
    w.spawn_building(B_MSLO, 0, CellCoord::new(2, 2))
        .expect("mslo");
    w.tick(&[]);
    // Present but not instantly ready.
    assert!(!w.superweapon_ready(0, SuperKind::Nuclear));
    let p0 = w
        .superweapon_charge_permille(0, SuperKind::Nuclear)
        .unwrap();
    for _ in 0..50 {
        w.tick(&[]);
    }
    let p1 = w
        .superweapon_charge_permille(0, SuperKind::Nuclear)
        .unwrap();
    assert!(p1 > p0, "the superweapon charges over time");
    // Losing the building removes the superweapon.
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
        "the superweapon is gone once its building is sold"
    );
}
