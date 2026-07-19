//! M6 item 4 — "Building combat/economy edges" (ra-tester charter): damage vs
//! Armor stats, building death mid-attack, footprint clearing, Sell refund
//! math, and elimination/game-over interactions, all through `World`'s public
//! API. Complements (does not duplicate) the colocated economy tests in
//! `world.rs` (`sell_refunds_fraction_and_frees_footprint`,
//! `building_destroyed_clears_occupancy_power_and_count`,
//! `house_elimination_yields_victory_and_defeat`) — this file adds: an
//! independent Verses/Armor cross-check with a truncation-sensitive refund
//! table, mid-approach stale-target handling, Sell-triggered elimination, and
//! a Sell-during-production interaction.
//!
//! **Not duplicated here:** the "same-tick mutual elimination always resolves
//! Defeat, never Victory" edge case (M6 task item 4.7) is already covered —
//! more thoroughly, with basic Victory/Defeat/sticky-terminal-state coverage
//! alongside it — by `ra-sim/tests/winlose_suite.rs`'s
//! `simultaneous_elimination_resolves_defeat_never_victory`, found already
//! present (by another concurrent ra-tester pass) while writing this file.
//! See this file's final report for the pointer instead of a second copy.
//!
//! Uses its own minimal fixture catalog (per the task brief: don't reach into
//! `world.rs`'s private test module).

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    AiPlayer, BuildItem, BuildingProto, Catalog, Command, Difficulty, EconRules, GameOver, Handle,
    MoveStats, Passability, Target, UnitProto, WarheadProfile, WeaponProfile, World,
};

// ===========================================================================
// Fixture
// ===========================================================================

// Building type ids.
const B_FACT: u32 = 0; // construction yard, 3x3
const B_POWR: u32 = 1; // power plant, 2x2, +100 output
const B_PROC: u32 = 2; // refinery, 3x3, -30 drain
const B_WEAP: u32 = 3; // war factory, 3x3, -20 drain
const B_PAD: u32 = 4; // 1x1 filler, armor=none, cost 101 (refund-truncation bait)
const B_PAD_WOOD: u32 = 5; // 1x1, armor=wood
const B_PAD_LIGHT: u32 = 6; // 1x1, armor=light
const B_PAD_HEAVY: u32 = 7; // 1x1, armor=heavy
const B_PAD_CONC: u32 = 8; // 1x1, armor=concrete

// Unit-proto ids (catalog production lanes — distinct from the plain
// `spawn_unit(0, ...)` attackers used directly throughout this file, which
// never go through the catalog at all).
const U_PROD: u32 = 1; // war-factory product, prereq WEAP, no weapon (production-lane filler)
const U_PROD_SPRITE: u32 = 77; // its spawned type_id, distinct from attackers' type_id 0

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

/// The fixture catalog. Costs/footprints mirror `world.rs`'s own test
/// catalog in spirit but are defined independently here (per the task brief).
/// `B_PAD`'s cost (101) is deliberately not a clean multiple of 100 — the
/// Sell-refund test below relies on that to pin exact truncating division.
fn catalog() -> Catalog {
    let bproto = |name: &str,
                  w: u8,
                  h: u8,
                  power: i32,
                  cost: i32,
                  armor: u8,
                  prereq: Vec<u32>,
                  cy: bool,
                  refin: bool,
                  wf: bool| BuildingProto {
        is_barracks: false,
        name: name.to_string(),
        foot_w: w,
        foot_h: h,
        max_health: 500,
        armor,
        power,
        cost,
        prereq,
        is_refinery: refin,
        is_construction_yard: cy,
        is_war_factory: wf,
        free_harvester_unit: None,
        sight: 4,
        sprite_id: 0,
        weapon: None,
        has_turret: false,
        charges: false,
        is_wall: false,
    };
    let uproto =
        |sprite_id: u32, cost: i32, prereq: Vec<u32>, weapon: Option<WeaponProfile>| UnitProto {
            is_infantry: false,
            locomotor: 1,
            name: "UNIT".to_string(),
            sprite_id,
            max_health: 400,
            stats: stats(),
            armor: 0,
            weapon,
            secondary: None,
            has_turret: weapon.is_some(),
            is_harvester: false,
            deploys_to: None,
            cost,
            prereq,
            sight: 2,
        };
    Catalog {
        buildings: vec![
            bproto("FACT", 3, 3, 0, 100, 0, vec![], true, false, false),
            bproto("POWR", 2, 2, 100, 30, 0, vec![B_FACT], false, false, false),
            bproto("PROC", 3, 3, -30, 50, 0, vec![B_POWR], false, true, false),
            bproto("WEAP", 3, 3, -20, 60, 0, vec![B_POWR], false, false, true),
            bproto("PAD", 1, 1, 0, 101, 0, vec![], false, false, false),
            bproto("PAD_WOOD", 1, 1, 0, 10, 1, vec![], false, false, false),
            bproto("PAD_LIGHT", 1, 1, 0, 10, 2, vec![], false, false, false),
            bproto("PAD_HEAVY", 1, 1, 0, 10, 3, vec![], false, false, false),
            bproto("PAD_CONC", 1, 1, 0, 10, 4, vec![], false, false, false),
        ],
        units: vec![
            uproto(10, 80, vec![], None),
            uproto(U_PROD_SPRITE, 120, vec![B_WEAP], None),
        ],
        econ: EconRules::default(),
    }
}

fn world(credits: i32) -> World {
    let mut w = World::new(Passability::all_passable(), 0xACED_5EED);
    w.set_catalog(catalog());
    w.init_houses(4, credits);
    w
}

fn world_with_refund(credits: i32, refund_percent: i32) -> World {
    let mut w = World::new(Passability::all_passable(), 0xACED_5EED);
    let mut cat = catalog();
    cat.econ.refund_percent = refund_percent;
    w.set_catalog(cat);
    w.init_houses(4, credits);
    w
}

/// A weapon that hits the moment it's fired (`instant=true`, huge ROF cooldown
/// so a single Attack command reliably yields exactly one shot within the
/// test's observation window) with the given base damage and per-armor
/// `Verses=` percentages. `warhead_ap` only matters for ground/infantry
/// scatter (`Target::Cell`) — it is irrelevant for building targets, which
/// (per `world.rs`'s `fire`) never scatter (`is_ground` is false for
/// `Target::Building`), so a building target's impact point always exactly
/// equals its center, making `distance == 0` on every hit.
fn instant_weapon(base_damage: i32, verses_pct: [i32; 5], warhead_ap: bool) -> WeaponProfile {
    WeaponProfile {
        damage: base_damage,
        rof: 60_000,
        range: 600,
        proj_speed: 999,
        proj_rot: 0,
        invisible: true,
        instant: true,
        warhead: WarheadProfile {
            spread: 3,
            verses: pct5(verses_pct),
        },
        warhead_ap,
        arcing: false,
        ballistic_scatter: 256,
        homing_scatter: 512,
        min_damage: 1,
        max_damage: 1000,
    }
}

/// Spawn an attacker at `cell`, facing `facing` (the caller picks a facing
/// that already equals the desired aim direction toward its target, so the
/// turret-alignment gate does not delay the first shot — see
/// `firing_fsm.rs`'s `turret_must_align_before_firing` for the alignment
/// mechanics this sidesteps).
fn spawn_attacker(
    w: &mut World,
    house: u8,
    cell: CellCoord,
    facing: Facing,
    weapon: WeaponProfile,
) -> Handle {
    let h = w.spawn_unit(0, house, cell, facing, 400, stats());
    w.set_unit_combat(h, 0, Some(weapon), true);
    h
}

// ===========================================================================
// 1. Damage vs Armor: real Verses/Armor stats table check.
// ===========================================================================

/// Cross-checks building damage resolution against the same real RA1
/// `Verses=` percentages `damage_matrix.rs` validates against the actual
/// `rules.ini` (AP/90mm `Verses=30%,75%,75%,100%,50%`; SA/M60mg
/// `Verses=100%,50%,60%,25%,25%`; HE/155mm `Verses=90%,75%,60%,25%,100%`,
/// armor order `[none,wood,light,heavy,concrete]`). Because a building target
/// always resolves at `distance == 0` (see `instant_weapon`'s doc comment),
/// the expected damage is exactly the warhead-vs-armor modifier rounded to
/// nearest (`(damage*raw+32768)/65536`), floored at `MinDamage` — the
/// `damage_matrix.rs` module doc comment derives these exact per-armor
/// modifier values by hand; this test spot-checks four of that table's rows
/// end-to-end through `World`'s combat pipeline instead of calling
/// `modify_damage` directly.
#[test]
fn damage_vs_armor_matches_real_verses_table_end_to_end() {
    struct Case {
        label: &'static str,
        base_damage: i32,
        verses_pct: [i32; 5],
        warhead_ap: bool,
        armor_building: u32,
        expected_damage: u16,
    }
    let cases = [
        // AP/90mm vs heavy (Verses[3]=100%): full damage, no reduction.
        Case {
            label: "AP vs heavy",
            base_damage: 30,
            verses_pct: [30, 75, 75, 100, 50],
            warhead_ap: true,
            armor_building: B_PAD_HEAVY,
            expected_damage: 30,
        },
        // AP/90mm vs concrete (Verses[4]=50%): half damage.
        Case {
            label: "AP vs concrete",
            base_damage: 30,
            verses_pct: [30, 75, 75, 100, 50],
            warhead_ap: true,
            armor_building: B_PAD_CONC,
            expected_damage: 15,
        },
        // SA/M60mg vs wood (Verses[1]=50%): 15*32768+32768 / 65536 = 8.
        Case {
            label: "SA vs wood",
            base_damage: 15,
            verses_pct: [100, 50, 60, 25, 25],
            warhead_ap: false,
            armor_building: B_PAD_WOOD,
            expected_damage: 8,
        },
        // HE/155mm vs light (Verses[2]=60%): 150*39321+32768 / 65536 = 90.
        Case {
            label: "HE vs light",
            base_damage: 150,
            verses_pct: [90, 75, 60, 25, 100],
            warhead_ap: false,
            armor_building: B_PAD_LIGHT,
            expected_damage: 90,
        },
    ];

    for case in cases {
        let mut w = world(1000);
        // Building at (20,20); attacker directly north at (20,19) facing
        // south (Facing 128) — already aligned, already in range (adjacent).
        let bh = w
            .spawn_building(case.armor_building, 2, CellCoord::new(20, 20))
            .unwrap();
        let weapon = instant_weapon(case.base_damage, case.verses_pct, case.warhead_ap);
        let atk = spawn_attacker(&mut w, 1, CellCoord::new(20, 19), Facing(128), weapon);
        let before = w.buildings.get(bh).unwrap().health;

        w.tick(&[Command::Attack {
            unit: atk,
            target: Target::Building(bh),
            house: 1,
        }]);

        let after = w.buildings.get(bh).unwrap().health;
        assert_eq!(
            before - after,
            case.expected_damage,
            "{}: expected exactly {} damage on the first (instant, unobstructed) shot, got {}",
            case.label,
            case.expected_damage,
            before - after
        );
        // Confirm it really did fire immediately (this is the "already
        // aligned, already in range" setup working as intended), not that
        // some other tick's shot happened to match by coincidence.
        assert_eq!(
            w.tick_count(),
            1,
            "{}: sanity — only one tick should have elapsed",
            case.label
        );
    }
}

// ===========================================================================
// 2. Building death mid-attacker-approach.
// ===========================================================================

/// An attacker (`slow_atk`) is ordered to attack a building from far enough
/// away that it must spend several ticks approaching. Before it arrives, a
/// second, already-adjacent attacker (`fast_atk`) kills the building in one
/// shot. Pin the actual observed behavior of the first attacker's now-stale
/// `Target::Building` handle: no panic, and (per `run_combat`'s
/// `drop_target` path, shared with the unit-target case `firing_fsm.rs`
/// pins) the TarCom clears the very next combat tick. Since `drop_target`
/// `continue`s before reaching the approach/movement logic, the attacker's
/// in-flight path is left untouched that tick — it is NOT stopped or
/// retargeted, it simply keeps walking its last-assigned path (toward where
/// the building used to be, which is passable again) until that path runs
/// out, then goes idle. This test pins exactly that, rather than assuming a
/// "stop immediately" or "auto-retarget" behavior.
#[test]
fn building_death_mid_approach_clears_stale_target_without_panic() {
    let mut w = world(1000);
    let bh = w.spawn_building(B_PAD, 2, CellCoord::new(50, 50)).unwrap();
    w.buildings.get_mut(bh).unwrap().health = 500; // full, so the slow attacker can't finish it alone quickly

    // Attacker that must approach: 15 cells away, well outside range.
    let approach_weapon = instant_weapon(30, [100, 100, 100, 100, 100], false);
    let slow_atk = spawn_attacker(
        &mut w,
        1,
        CellCoord::new(35, 50),
        Facing(128),
        approach_weapon,
    );

    // Attacker already adjacent, one-shots the building.
    let lethal_weapon = instant_weapon(1000, [100, 100, 100, 100, 100], false);
    let fast_atk = spawn_attacker(
        &mut w,
        1,
        CellCoord::new(50, 49),
        Facing(128),
        lethal_weapon,
    );

    w.tick(&[
        Command::Attack {
            unit: slow_atk,
            target: Target::Building(bh),
            house: 1,
        },
        Command::Attack {
            unit: fast_atk,
            target: Target::Building(bh),
            house: 1,
        },
    ]);
    // The building must already be dead (fast_atk was adjacent, aligned, and
    // one-shot lethal) and the slow attacker must be underway (still far).
    assert!(
        !w.buildings.contains(bh),
        "fast attacker should have already destroyed the building"
    );
    assert!(
        w.units.get(slow_atk).unwrap().is_moving(),
        "slow attacker should be approaching, not idle, on the tick the building died"
    );
    // The target resolution in `run_combat` runs before the building-death
    // consequences are visible to a *new* tick, so the slow attacker still
    // nominally has a target this same tick (it will only see the stale
    // handle on its *next* combat pass).
    assert!(w.units.get(slow_atk).unwrap().has_target());

    // Next tick: the stale target must be dropped, with no panic.
    w.tick(&[]);
    assert!(
        !w.units.get(slow_atk).unwrap().has_target(),
        "slow attacker should have dropped its stale Target::Building handle"
    );

    // Run to settle: no panic, and the attacker eventually stops moving
    // (its old approach path runs out) rather than looping forever or
    // auto-reacquiring a new target (Attack is a one-shot TarCom assignment,
    // never an auto-retarget in this sim, per firing_fsm.rs's pinned
    // behavior for the analogous unit-target case).
    let mut settled = false;
    for _ in 0..300 {
        w.tick(&[]); // must not panic even though the target is long gone
        if !w.units.get(slow_atk).unwrap().is_moving() {
            settled = true;
            break;
        }
    }
    assert!(
        settled,
        "slow attacker never stopped moving — stale-target handling looks stuck"
    );
    assert!(
        !w.units.get(slow_atk).unwrap().has_target(),
        "no auto-retarget should have occurred"
    );
}

// ===========================================================================
// 3. Footprint actually clears (combat death AND Sell).
// ===========================================================================

#[test]
fn footprint_clears_after_combat_death_and_after_sell() {
    // --- 3a. Combat death ---
    let mut w = world(1000);
    let bh = w.spawn_building(B_PAD, 2, CellCoord::new(20, 20)).unwrap();
    w.buildings.get_mut(bh).unwrap().health = 1;
    assert!(!w.passability().is_passable(CellCoord::new(20, 20)));
    let weapon = instant_weapon(1000, [100, 100, 100, 100, 100], false);
    let atk = spawn_attacker(&mut w, 1, CellCoord::new(20, 19), Facing(128), weapon);
    w.tick(&[Command::Attack {
        unit: atk,
        target: Target::Building(bh),
        house: 1,
    }]);
    assert!(
        !w.buildings.contains(bh),
        "combat death did not remove the building"
    );
    assert!(
        w.passability().is_passable(CellCoord::new(20, 20)),
        "combat death did not free the footprint"
    );
    // (a) A unit can path/move through the freed cell.
    let mover = w.spawn_unit(0, 1, CellCoord::new(18, 20), Facing(0), 400, stats());
    w.tick(&[Command::Move {
        unit: mover,
        dest: CellCoord::new(22, 20),
        house: 1,
    }]);
    let mut arrived = false;
    for _ in 0..200 {
        w.tick(&[]);
        if w.units.get(mover).unwrap().cell() == CellCoord::new(22, 20) {
            arrived = true;
            break;
        }
    }
    assert!(arrived, "unit failed to path through the freed footprint");
    assert_eq!(
        w.units.get(mover).unwrap().cell(),
        CellCoord::new(22, 20),
        "unit should have actually crossed the former footprint cell (20,20) en route"
    );
    // (b) A new building can be legally placed on the exact former footprint.
    assert!(
        w.can_place_building(2, B_PAD, CellCoord::new(20, 20)),
        "a new building should be legally placeable on the cleared footprint"
    );

    // --- 3b. Sell ---
    let mut w = world(1000);
    let bh2 = w.spawn_building(B_PAD, 2, CellCoord::new(60, 60)).unwrap();
    assert!(!w.passability().is_passable(CellCoord::new(60, 60)));
    w.tick(&[Command::Sell {
        house: 2,
        building: bh2,
    }]);
    assert!(
        !w.buildings.contains(bh2),
        "Sell did not remove the building"
    );
    assert!(
        w.passability().is_passable(CellCoord::new(60, 60)),
        "Sell did not free the footprint"
    );
    let mover2 = w.spawn_unit(0, 1, CellCoord::new(58, 60), Facing(0), 400, stats());
    w.tick(&[Command::Move {
        unit: mover2,
        dest: CellCoord::new(62, 60),
        house: 1,
    }]);
    let mut arrived2 = false;
    for _ in 0..200 {
        w.tick(&[]);
        if w.units.get(mover2).unwrap().cell() == CellCoord::new(62, 60) {
            arrived2 = true;
            break;
        }
    }
    assert!(
        arrived2,
        "unit failed to path through the sold building's footprint"
    );
    assert!(
        w.can_place_building(2, B_PAD, CellCoord::new(60, 60)),
        "a new building should be legally placeable where the sold building stood"
    );
}

// ===========================================================================
// 4. Sell refund math: exact, with integer truncation.
// ===========================================================================

/// `B_PAD` costs 101 — deliberately not evenly divisible by common refund
/// percentages, so both the default (50%) and a custom (33%) refund
/// percentage exercise `apply_sell`'s `(cost as i64 * refund_percent as i64 /
/// 100) as i32` truncating division for real: 101*50/100 = 50.5 -> 50 (not
/// 50.5, not 51), and 101*33/100 = 33.33 -> 33.
#[test]
fn sell_refund_is_exact_with_integer_truncation_default_and_custom_percent() {
    // Default 50%.
    {
        let mut w = world(1000);
        let bh = w.spawn_building(B_PAD, 1, CellCoord::new(30, 30)).unwrap();
        assert_eq!(w.catalog.econ.refund_percent, 50);
        let before = w.house_credits(1);
        w.tick(&[Command::Sell {
            house: 1,
            building: bh,
        }]);
        assert_eq!(
            w.house_credits(1),
            before + 50,
            "101 * 50% should truncate to exactly 50, not 50.5 or 51"
        );
    }
    // Custom 33%.
    {
        let mut w = world_with_refund(1000, 33);
        let bh = w.spawn_building(B_PAD, 1, CellCoord::new(30, 30)).unwrap();
        let before = w.house_credits(1);
        w.tick(&[Command::Sell {
            house: 1,
            building: bh,
        }]);
        assert_eq!(
            w.house_credits(1),
            before + 33,
            "101 * 33% should truncate to exactly 33, not 33.33"
        );
    }
    // Power output/drain and building_counts revert correctly (POWR output,
    // PROC drain, both distinct from the PAD cases above).
    {
        let mut w = world(1000);
        let powr = w.spawn_building(B_POWR, 1, CellCoord::new(30, 30)).unwrap();
        let proc = w.spawn_building(B_PROC, 1, CellCoord::new(40, 40)).unwrap();
        assert_eq!(w.house(1).unwrap().power_output, 100);
        assert_eq!(w.house(1).unwrap().power_drain, 30);
        assert!(w.house(1).unwrap().owns_building(B_POWR));
        assert!(w.house(1).unwrap().owns_building(B_PROC));

        w.tick(&[
            Command::Sell {
                house: 1,
                building: powr,
            },
            Command::Sell {
                house: 1,
                building: proc,
            },
        ]);
        assert_eq!(
            w.house(1).unwrap().power_output,
            0,
            "selling the POWR should reverse its +100 output"
        );
        assert_eq!(
            w.house(1).unwrap().power_drain,
            0,
            "selling the PROC should reverse its -30 (30 drain)"
        );
        assert!(!w.house(1).unwrap().owns_building(B_POWR));
        assert!(!w.house(1).unwrap().owns_building(B_PROC));
    }
}

// ===========================================================================
// 5. Sell-while-producing.
// ===========================================================================

/// Two sub-cases, both asserting no panic:
/// (a) selling a building UNRELATED to the in-flight production lane does not
///     disturb it — production continues to completion exactly as if nothing
///     happened.
/// (b) selling the war factory itself mid-unit-production **abandons the lane
///     and refunds the credits already spent** (M7 fix — was a pinned soft-lock
///     in M6). This is the faithful port of `BuildingClass::Detach_All` →
///     `FactoryClass::Abandon` (`building.cpp:5138`, `factory.cpp:479`
///     `Refund_Money(money - Balance)`): removing the last builder of a category
///     abandons the shared production and returns the progress money. The lane
///     clears (no permanently-stuck `done` build), no unit ever spawns, and the
///     abandoned build is net-zero in credits.
#[test]
fn sell_while_producing_unrelated_continues_and_factory_sale_abandons_with_refund() {
    // (a) Unrelated building sold mid-unit-production: production unaffected.
    {
        let mut w = world(1000);
        w.spawn_building(B_FACT, 1, CellCoord::new(10, 10)).unwrap();
        w.spawn_building(B_POWR, 1, CellCoord::new(20, 20)).unwrap();
        w.spawn_building(B_WEAP, 1, CellCoord::new(30, 30)).unwrap();
        let unrelated = w.spawn_building(B_PAD, 1, CellCoord::new(40, 40)).unwrap();

        w.tick(&[Command::StartProduction {
            house: 1,
            item: BuildItem::Unit(U_PROD),
        }]);
        assert!(
            w.house(1).unwrap().unit_prod.is_some(),
            "production did not start"
        );

        // Sell the unrelated PAD partway through.
        for _ in 0..5 {
            w.tick(&[]);
        }
        w.tick(&[Command::Sell {
            house: 1,
            building: unrelated,
        }]);
        assert!(!w.buildings.contains(unrelated));

        let mut spawned = false;
        for _ in 0..3000 {
            w.tick(&[]);
            if w.units
                .iter()
                .any(|(_, u)| u.house == 1 && u.type_id == U_PROD_SPRITE)
            {
                spawned = true;
                break;
            }
        }
        assert!(
            spawned,
            "unrelated Sell should not have blocked production from completing"
        );
        assert!(
            w.house(1).unwrap().unit_prod.is_none(),
            "completed production should have cleared its lane"
        );
        // `spawn_building` (used above for FACT/POWR/WEAP/PAD) is the direct
        // loader path and does not touch credits — only `StartProduction`'s
        // installments and `Sell`'s refund do. So the only credit movements
        // here are: the unit's full cost (120) paid in installments, and the
        // unrelated PAD's sell refund (cost 101 @ 50% = 50).
        assert_eq!(
            w.house_credits(1),
            1000 - 120 + 50,
            "unrelated Sell must not touch the in-flight lane's spend"
        );
    }

    // (b) Selling the war factory itself mid-production: abandons + refunds.
    {
        let mut w = world(1000);
        w.spawn_building(B_FACT, 1, CellCoord::new(10, 10)).unwrap();
        w.spawn_building(B_POWR, 1, CellCoord::new(20, 20)).unwrap();
        let weap = w.spawn_building(B_WEAP, 1, CellCoord::new(30, 30)).unwrap();

        w.tick(&[Command::StartProduction {
            house: 1,
            item: BuildItem::Unit(U_PROD),
        }]);
        assert!(w.house(1).unwrap().unit_prod.is_some());

        // Let a few installments land, then sell the war factory.
        for _ in 0..5 {
            w.tick(&[]);
        }
        let credits_before_sale = w.house_credits(1);
        let spent_before_sale = w.house(1).unwrap().unit_prod.unwrap().spent;
        assert!(
            spent_before_sale > 0,
            "sanity: some credits should already be spent"
        );

        w.tick(&[Command::Sell {
            house: 1,
            building: weap,
        }]);
        assert!(!w.buildings.contains(weap));

        // The lane is abandoned immediately: it clears (no stuck `done` build),
        // and the spent portion is refunded (FactoryClass::Abandon).
        assert!(
            w.house(1).unwrap().unit_prod.is_none(),
            "selling the last war factory mid-production should abandon the lane"
        );
        // Refund: the `spent_before_sale` credits paid so far are returned, and
        // selling the WEAP refunds 60 * 50% = 30. So relative to the pre-sale
        // balance we gain exactly (spent_before_sale + 30).
        assert_eq!(
            w.house_credits(1),
            credits_before_sale + spent_before_sale + 30,
            "abandon must refund the spent progress; the WEAP sale refunds 30"
        );

        // Run far past when the unit would normally have completed. Must not
        // panic and must never spawn a unit (the lane is gone).
        for _ in 0..5000 {
            w.tick(&[]);
        }
        assert!(
            !w.units
                .iter()
                .any(|(_, u)| u.house == 1 && u.type_id == U_PROD_SPRITE),
            "no unit should ever spawn once its war factory was sold mid-production"
        );
        assert!(
            w.house(1).unwrap().unit_prod.is_none(),
            "the abandoned lane stays clear"
        );
        // Net credit position: production abandoned net-zero (full cost never
        // completed; spent portion refunded), plus the WEAP sale refund of 30.
        assert_eq!(
            w.house_credits(1),
            1000 + 30,
            "abandoned build is net-zero; only the WEAP sale's 30 refund remains"
        );
    }
}

// ===========================================================================
// 6. Sell-last-building -> elimination -> game over (both directions).
// ===========================================================================

/// Complements the colocated `house_elimination_yields_victory_and_defeat`
/// test in `world.rs` (which starts each house already owning nothing) by
/// driving elimination through the actual `Command::Sell` path — i.e.
/// confirming `apply_sell` -> `remove_building` -> `house_alive` ->
/// `update_game_over` chains correctly end-to-end, in both the Defeat
/// (player sells their last building) and Victory (the sole AI house's last
/// building is sold) directions. Both AI houses here own exactly one
/// building and no units, matching the already-validated "building-only AI
/// house is a safe no-op for `set_ai`" pattern from `world.rs`'s own test.
#[test]
fn selling_the_last_building_triggers_elimination_and_resolves_game_over() {
    // Defeat: the player (house 1) sells their only building.
    {
        let mut w = world(1000);
        let bh = w.spawn_building(B_PAD, 1, CellCoord::new(20, 20)).unwrap();
        // The AI (house 2) stays alive throughout, so this Defeat is really
        // caused by the player's own elimination, not a coincidental Victory.
        w.spawn_building(B_PAD, 2, CellCoord::new(60, 60)).unwrap();
        w.set_player_house(1);
        w.set_ai(vec![AiPlayer::new(2, Difficulty::Normal)]);
        assert_eq!(w.game_over(), GameOver::Ongoing);
        assert!(w.house_alive(1));

        w.tick(&[Command::Sell {
            house: 1,
            building: bh,
        }]);
        assert!(
            !w.house_alive(1),
            "selling the player's only building should eliminate house 1"
        );
        assert_eq!(
            w.game_over(),
            GameOver::Defeat,
            "player elimination via Sell should resolve to Defeat"
        );
    }

    // Victory: the sole AI (house 2) sells its only building (no units of
    // its own — a deliberate stand-in for "loses its last asset").
    {
        let mut w = world(1000);
        w.spawn_building(B_PAD, 1, CellCoord::new(20, 20)).unwrap();
        let bh2 = w.spawn_building(B_PAD, 2, CellCoord::new(60, 60)).unwrap();
        w.set_player_house(1);
        w.set_ai(vec![AiPlayer::new(2, Difficulty::Normal)]);
        assert_eq!(w.game_over(), GameOver::Ongoing);

        w.tick(&[Command::Sell {
            house: 2,
            building: bh2,
        }]);
        assert!(
            !w.house_alive(2),
            "selling the AI's only building should eliminate house 2"
        );
        assert_eq!(
            w.game_over(),
            GameOver::Victory,
            "the last AI house's elimination should resolve to player Victory"
        );
    }

    // Buildings-then-units: a house that still has a live unit after losing
    // its only building is NOT eliminated (elimination requires losing
    // both), and only dies once the unit is gone too.
    {
        let mut w = world(1000);
        let bh = w.spawn_building(B_PAD, 1, CellCoord::new(20, 20)).unwrap();
        let survivor = w.spawn_unit(0, 1, CellCoord::new(25, 25), Facing(0), 100, stats());
        w.spawn_building(B_PAD, 2, CellCoord::new(60, 60)).unwrap();
        w.set_player_house(1);
        w.set_ai(vec![AiPlayer::new(2, Difficulty::Normal)]);

        w.tick(&[Command::Sell {
            house: 1,
            building: bh,
        }]);
        assert!(
            w.house_alive(1),
            "a house with a live unit should survive losing its only building"
        );
        assert_eq!(w.game_over(), GameOver::Ongoing);

        w.units.remove(survivor);
        w.tick(&[]);
        assert!(!w.house_alive(1));
        assert_eq!(
            w.game_over(),
            GameOver::Defeat,
            "losing the last unit too should now resolve Defeat"
        );
    }
}
