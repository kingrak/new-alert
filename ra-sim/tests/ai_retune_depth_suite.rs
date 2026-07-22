//! Audit backfill (ra-tester, M7.11 handoff): mechanism-level depth pins for
//! the M7.11 AI retune (`ra-sim/src/ai.rs`, QUIRKS Q20 P1) that the
//! milestone's own behavioural/showcase suites (`ai_suite.rs`,
//! `ai_brain_suite.rs`, `ra-client/tests/ui_ai_vs_ai.rs`) exercise only
//! end-to-end, not in isolation:
//!
//! §1 `failed_attacks` escalation: each dissolved wave grows the next one.
//! §2 `sector_threat` routing: a hand-built base with one heavily-defended
//!    and one open-flank production building routes the attack through the
//!    open flank — even though the defended one is *closer*.
//! §3 production-quarry preference: a war factory is targeted over a much
//!    nearer non-production building.
//! §4 all-out boundary — per-difficulty since M7.20 P3
//!    (`Difficulty::all_out_escalation()`: Normal = 4, Hard = 2, Easy = 5);
//!    the staged-wave scripts below run at Normal (the historical 4-wave
//!    boundary) and a dedicated pin covers Hard's earlier trigger.
//! §5 determinism of the whole escalation/all-out state machine (same seed
//!    twice, forced dissolves included).
//!
//! All driven through the public `World`/`AiPlayer`/`Command` API, per the
//! ra-tester charter. Team-membership is identified the same way
//! `ai_brain_suite.rs`'s decimation-boundary test does: an Attacking-phase
//! team member always carries a live `Command::Attack` target, so
//! `u.target.is_some()` is a reliable proxy for "is a team member" in a
//! fixture where no other mechanism (guard acquisition, retaliation) can set
//! a target.

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    AiPlayer, BuildingProto, Catalog, Difficulty, EconRules, Handle, MoveStats, Passability,
    Target, UnitProto, WarheadProfile, WeaponProfile, World,
};

// ===========================================================================
// Shared fixture: a minimal catalog with one production building (WEAP), one
// armed non-production "defense" building (TURRET, for §2's sector-threat
// scan), and one unarmed non-production building (OTHER, for §3's
// quarry-preference check). Attacker units are cheap, fast-turning tanks.
// ===========================================================================

const B_FACT: u32 = 0;
const B_WEAP: u32 = 1; // production (war factory)
const B_TURRET: u32 = 2; // armed, non-production (defense)
const B_OTHER: u32 = 3; // unarmed, non-production

const U_TANK: u32 = 1;

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 40,
        rot: 10,
    }
}

fn weapon(damage: i32) -> WeaponProfile {
    WeaponProfile {
        damage,
        rof: 30,
        range: 5 * 256,
        proj_speed: 100,
        proj_rot: 0,
        invisible: false,
        instant: true,
        warhead: WarheadProfile {
            spread: 1,
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

fn catalog() -> Catalog {
    let bproto = |name: &str, wf: bool, def_weapon: Option<WeaponProfile>| BuildingProto {
        is_barracks: false,
        name: name.to_string(),
        foot_w: 2,
        foot_h: 2,
        max_health: 500,
        armor: 0,
        power: 0,
        cost: 60,
        prereq: vec![],
        is_refinery: false,
        is_construction_yard: name == "FACT",
        is_war_factory: wf,
        free_harvester_unit: None,
        sight: 5,
        sprite_id: 0,
        weapon: def_weapon,
        has_turret: def_weapon.is_some(),
        charges: false,
        is_wall: false,
        storage: 0,
    };
    Catalog {
        buildings: vec![
            bproto("FACT", false, None),
            bproto("WEAP", true, None),
            bproto("TURRET", false, Some(weapon(50))),
            bproto("OTHER", false, None),
        ],
        units: vec![
            UnitProto {
                is_infantry: false,
                locomotor: 1,
                name: "MCV".into(),
                sprite_id: 0,
                max_health: 400,
                stats: stats(),
                armor: 0,
                weapon: None,
                secondary: None,
                has_turret: false,
                is_harvester: false,
                deploys_to: Some(B_FACT),
                cost: 100,
                prereq: vec![],
                sight: 4,
                passengers: 0,
                ammo: 0,
            },
            UnitProto {
                is_infantry: false,
                locomotor: 1,
                name: "TANK".into(),
                sprite_id: 1,
                max_health: 400,
                stats: stats(),
                armor: 0,
                weapon: Some(weapon(25)),
                secondary: None,
                has_turret: true,
                is_harvester: false,
                deploys_to: None,
                cost: 80,
                prereq: vec![],
                sight: 4,
                passengers: 0,
                ammo: 0,
            },
        ],
        econ: EconRules::default(),
    }
}

fn home1() -> CellCoord {
    CellCoord::new(15, 15)
}

const CREDITS: i32 = 6000;

/// Spawn `n` idle, armed, house-1 tanks in a small cluster near `home1()`, on
/// a fresh house-1 `FACT` (so `base_center`/staging are stable from tick 0 —
/// no MCV-deploy delay to model).
fn attacker_world(seed: u32, n_tanks: u32, difficulty: Difficulty) -> World {
    let mut w = World::new(Passability::all_passable(), seed);
    w.set_catalog(catalog());
    w.init_houses(3, CREDITS);
    w.spawn_building(B_FACT, 1, home1()).unwrap();
    for i in 0..n_tanks {
        let cell = CellCoord::new(
            home1().x + 4 + (i % 8) as i32,
            home1().y + 4 + (i / 8) as i32,
        );
        let h = w.spawn_unit(U_TANK, 1, cell, Facing(0), 400, stats());
        w.set_unit_combat(h, 0, Some(weapon(25)), true);
    }
    w.set_ai(vec![AiPlayer::new(1, difficulty)]);
    w
}

// ===========================================================================
// §1 + §4 — failed_attacks escalation and the all-out boundary at exactly 4.
// ===========================================================================

/// Wait until house 1's team reaches the Attacking phase (not just Staging),
/// returning `(member_count, initial_size)`, or `None` if the budget runs out.
fn wait_for_attacking(w: &mut World, max_ticks: u32) -> Option<(usize, usize)> {
    for _ in 0..max_ticks {
        w.tick(&[]);
        if let Some((n, init, staging, _)) = w
            .ai()
            .iter()
            .find(|a| a.house() == 1)
            .unwrap()
            .team_summary()
        {
            if !staging {
                return Some((n, init));
            }
        }
    }
    None
}

/// Force the currently-Attacking team to dissolve by removing members down to
/// one below the (recomputed) retreat floor, then tick once so `advance_team`
/// observes the decimation and bumps `failed_attacks`. Team members are
/// identified via the `target.is_some()` proxy (see module doc).
fn force_current_team_to_dissolve(w: &mut World) {
    let (_, init) = w
        .ai()
        .iter()
        .find(|a| a.house() == 1)
        .unwrap()
        .team_summary()
        .map(|(n, init, _, _)| (n, init))
        .expect("a team must be active to dissolve");
    let floor = (init / 2).max(2);
    loop {
        let members: Vec<Handle> = w
            .units
            .iter()
            .filter(|(_, u)| u.house == 1 && u.target.is_some())
            .map(|(h, _)| h)
            .collect();
        if members.len() < floor {
            break;
        }
        w.units.remove(members[0]);
    }
    w.tick(&[]);
}

/// Run the escalation/all-out state machine to completion against an
/// undefended, far-away single-target house 2, recording each successive
/// wave's `initial_size` (one entry per forced dissolve) and the tick chain,
/// for both the growth pin and the determinism pin.
fn run_escalation_script(seed: u32) -> (Vec<usize>, bool, Vec<u64>) {
    // Ample tanks: worst case (escalation=3, Normal) wants up to
    // `team_vehicles(3) + jitter(<=1) + escalation*2(6)` = 10 in a single
    // wave, across 4 waves with survivors recycling back into the pool —
    // 60 is comfortably more than any sequence of 4 waves can ever consume.
    // Normal (not Hard): since M7.20 P3 the all-out boundary is per-difficulty
    // and Hard now commits after only 2 dissolves — Normal keeps the
    // historical 4-staged-wave escalation this script pins.
    let mut w = attacker_world(seed, 60, Difficulty::Normal);
    // A single, undefended, far-away production building: the sole target,
    // isolating escalation/all-out from §2/§3's target-selection logic.
    w.spawn_building(B_WEAP, 2, CellCoord::new(110, 110))
        .unwrap();

    let mut hashes = Vec::new();
    let mut sizes = Vec::new();
    for _ in 0..4 {
        let (_, init) = wait_for_attacking(&mut w, 3000).unwrap_or_else(|| {
            panic!("house 1 never reached Attacking (wave {})", sizes.len() + 1)
        });
        sizes.push(init);
        force_current_team_to_dissolve(&mut w);
    }
    // Past the 4th dissolve, `failed_attacks` must be >= ALL_OUT_ESCALATION:
    // no 5th team should ever form again — confirm over a healthy budget,
    // and additionally confirm units are being individually attack-ordered
    // (the all-out signature) rather than idling.
    let mut all_out_confirmed = false;
    for _ in 0..1500 {
        let h = w.tick(&[]);
        hashes.push(h);
        if w.ai()
            .iter()
            .find(|a| a.house() == 1)
            .unwrap()
            .team_summary()
            .is_some()
        {
            panic!("a 5th team formed after 4 dissolves — all-out escalation did not engage");
        }
        if !all_out_confirmed
            && w.units
                .iter()
                .any(|(_, u)| u.house == 1 && matches!(u.target, Some(Target::Building(_))))
        {
            all_out_confirmed = true;
        }
    }
    (sizes, all_out_confirmed, hashes)
}

#[test]
fn failed_attacks_escalation_grows_each_successive_wave() {
    let (sizes, all_out_confirmed, _) = run_escalation_script(0xE5CA_1001);
    assert_eq!(sizes.len(), 4, "sanity: exactly 4 waves were forced");
    // Non-decreasing, not strictly increasing, adjacent-pair by adjacent-pair:
    // `want_v`'s ±1 jitter can tie two consecutive escalation levels (e.g.
    // wave 3's jitter=+1 landing on the same total as wave 4's jitter=-1).
    // The escalation *trend* is what's pinned: it must never shrink between
    // waves, and the deterministic `escalation*2` term must show up as a
    // clear net gain from the first (unescalated) wave to the last.
    for w in sizes.windows(2) {
        assert!(
            w[1] >= w[0],
            "a wave must never be smaller than the previous one after a \
             dissolve (M7.11 P1a escalation must not go backwards): sizes so far {sizes:?}"
        );
    }
    assert!(
        sizes.last().unwrap() > sizes.first().unwrap(),
        "the escalation trend across 4 dissolves must show a clear net increase \
         over the unescalated first wave: {sizes:?}"
    );
    assert!(
        all_out_confirmed,
        "after 4 dissolves the AI must switch to all-out (individual \
         Command::Attack orders), not stay idle"
    );
}

#[test]
fn all_out_engages_at_exactly_the_fourth_dissolve_not_the_third() {
    // Repeat the first 3 dissolves only, and confirm a 4th team CAN still
    // form (all-out has not engaged yet) — the boundary's "not early" half.
    // Normal difficulty: its `all_out_escalation()` is the historical 4.
    let mut w = attacker_world(0xE5CA_1002, 60, Difficulty::Normal);
    w.spawn_building(B_WEAP, 2, CellCoord::new(110, 110))
        .unwrap();

    for _ in 0..3 {
        wait_for_attacking(&mut w, 3000).expect("house 1 should reach Attacking");
        force_current_team_to_dissolve(&mut w);
    }
    // A 4th team must still be able to form (failed_attacks == 3 < 4).
    let formed_a_fourth = wait_for_attacking(&mut w, 3000).is_some();
    assert!(
        formed_a_fourth,
        "at exactly 3 prior dissolves, a 4th team must still form (all-out \
         must not engage before the boundary)"
    );
}

/// M7.20 P3 pin — Hard's earlier all-out trigger
/// (`Difficulty::all_out_escalation()` = 2). After ONE dissolve a second team
/// still forms; after the SECOND dissolve no further staged team may ever
/// form (the AI is all-out), and individual attack orders appear. Reverting
/// the per-difficulty knob to the old flat 4 makes the "no 3rd team" half
/// fail (a 3rd team would form), so the knob is proven load-bearing.
#[test]
fn hard_goes_all_out_after_exactly_two_dissolves() {
    let mut w = attacker_world(0xE5CA_1003, 60, Difficulty::Hard);
    w.spawn_building(B_WEAP, 2, CellCoord::new(110, 110))
        .unwrap();

    wait_for_attacking(&mut w, 3000).expect("wave 1 should form and attack");
    force_current_team_to_dissolve(&mut w); // failed_attacks -> 1
    wait_for_attacking(&mut w, 3000)
        .expect("at 1 prior dissolve (< 2), a 2nd Hard team must still form");
    force_current_team_to_dissolve(&mut w); // failed_attacks -> 2 == boundary

    let mut all_out_confirmed = false;
    for _ in 0..1500 {
        w.tick(&[]);
        assert!(
            w.ai()
                .iter()
                .find(|a| a.house() == 1)
                .unwrap()
                .team_summary()
                .is_none(),
            "a 3rd staged team formed after 2 dissolves at Hard — the M7.20 \
             per-difficulty all-out boundary (Hard = 2) has regressed"
        );
        if w.units
            .iter()
            .any(|(_, u)| u.house == 1 && matches!(u.target, Some(Target::Building(_))))
        {
            all_out_confirmed = true;
        }
    }
    assert!(
        all_out_confirmed,
        "after 2 dissolves a Hard AI must be all-out (individual attack orders), not idle"
    );
}

/// Regression pin for a bug found while building this suite (fixed in the
/// same pass, `ra-sim/src/ai.rs::advance_team`): a team that gets wiped out
/// **entirely** (`alive == 0`) used to hit an early `return` before the
/// `alive < retreat_floor` escalation check, even though `retreat_floor` is
/// always `>= 2` and so already covers `alive == 0` — meaning a total
/// wipeout, arguably the *strongest* failure signal an attack wave can send,
/// silently failed to grow the next wave the way a merely-half-decimated one
/// does. Fixed by removing the redundant, incorrect early return.
#[test]
fn total_wipeout_escalates_the_next_wave_same_as_partial_decimation() {
    // Normal: needs 3 staged waves, and Hard now goes all-out after 2
    // dissolves (M7.20 P3), which would suppress wave 3.
    let mut w = attacker_world(0xE5CA_5001, 30, Difficulty::Normal);
    w.spawn_building(B_WEAP, 2, CellCoord::new(110, 110))
        .unwrap();

    let (_, wave1_size) = wait_for_attacking(&mut w, 3000).expect("wave 1 should attack");
    wipe_current_team_entirely(&mut w); // failed_attacks 0 -> 1, if the fix holds
    let (_, wave2_size) = wait_for_attacking(&mut w, 3000)
        .expect("wave 2 should still form and attack after a total wipeout");
    wipe_current_team_entirely(&mut w); // failed_attacks 1 -> 2, if the fix holds
    let (_, wave3_size) = wait_for_attacking(&mut w, 3000)
        .expect("wave 3 should still form and attack after a second total wipeout");

    // Two consecutive TOTAL wipeouts (not just below-half decimation) must
    // raise `failed_attacks` from 0 to 2, adding a deterministic `+2` to
    // `want_v` per level (`+4` total from wave 1 to wave 3) — enough to
    // dominate the RNG jitter (`±1` per wave, so at most a `2`-unit swing
    // between any two waves) regardless of this seed's draws. A single
    // wipeout -> next-wave comparison is NOT reliably distinguishing here
    // (a `+2` per-level gain can tie against jitter noise), which is why
    // this pins the 2-level comparison instead.
    assert!(
        wave3_size > wave1_size,
        "two consecutive TOTAL wipeouts must escalate wave size just like partial \
         decimation does (wave 1 size {wave1_size}, wave 2 size {wave2_size}, wave 3 \
         size {wave3_size}) — a regression here means `alive == 0` is (again) \
         skipping the escalation bump"
    );
}

/// Remove every current team member (identified via the `target.is_some()`
/// proxy) in one shot — a total wipeout, not a down-to-the-floor partial
/// decimation — then tick once so `advance_team` observes it.
fn wipe_current_team_entirely(w: &mut World) {
    let members: Vec<Handle> = w
        .units
        .iter()
        .filter(|(_, u)| u.house == 1 && u.target.is_some())
        .map(|(h, _)| h)
        .collect();
    assert!(
        !members.is_empty(),
        "sanity: a team must be active to wipe out"
    );
    for h in members {
        w.units.remove(h);
    }
    w.tick(&[]);
}

#[test]
fn determinism_holds_across_the_escalation_and_all_out_state_machine() {
    let (sizes_a, all_out_a, hashes_a) = run_escalation_script(0xE5CA_2001);
    let (sizes_b, all_out_b, hashes_b) = run_escalation_script(0xE5CA_2001);
    assert_eq!(
        sizes_a, sizes_b,
        "escalating wave sizes diverged between two identical-seed runs"
    );
    assert_eq!(all_out_a, all_out_b);
    assert_eq!(
        hashes_a, hashes_b,
        "the post-all-out tick hash chain diverged between two identical-seed runs"
    );
}

// ===========================================================================
// §2 — sector_threat routing: a heavily-defended production building must be
// passed over for an open-flank one, even though the defended one is closer.
// ===========================================================================

#[test]
fn team_routes_through_the_open_flank_not_the_nearer_defended_production_building() {
    let mut w = attacker_world(0xE5CA_3001, 8, Difficulty::Normal);

    // Defended candidate: CLOSE to the attacker, but ringed by three armed
    // TURRETs within `SECTOR_THREAT_RADIUS` (6 cells) — high sector_threat.
    let defended_cell = CellCoord::new(40, 15);
    let defended = w.spawn_building(B_WEAP, 2, defended_cell).unwrap();
    for (dx, dy) in [(-3, 0), (3, 0), (0, 3)] {
        w.spawn_building(
            B_TURRET,
            2,
            CellCoord::new(defended_cell.x + dx, defended_cell.y + dy),
        )
        .unwrap();
    }

    // Open-flank candidate: FAR from the attacker (so a naive "nearest"
    // heuristic would never pick it), with zero armed buildings nearby —
    // zero sector_threat.
    let open_cell = CellCoord::new(110, 110);
    let open = w.spawn_building(B_WEAP, 2, open_cell).unwrap();

    let (_, _) = wait_for_attacking(&mut w, 4000).expect("house 1 should form and attack a team");

    // Only recruited team members carry a target (see module doc); the rest
    // of the 8-tank pool stays idle (`target == None`) and is irrelevant here.
    let members: Vec<_> = w
        .units
        .iter()
        .filter(|(_, u)| u.house == 1 && u.target.is_some())
        .map(|(_, u)| u.target)
        .collect();
    assert!(
        !members.is_empty(),
        "sanity: the team must have recruited members"
    );
    let targets_open = members.iter().all(|&t| t == Some(Target::Building(open)));
    let any_targets_defended = members.contains(&Some(Target::Building(defended)));

    assert!(
        targets_open,
        "every team member must target the open-flank production building \
         (zero sector_threat), despite it being far the farther of the two \
         candidates from the attacker's base: {members:?}"
    );
    assert!(
        !any_targets_defended,
        "no team member should target the heavily-defended production \
         building — sector_threat must route around it even though it's closer"
    );
}

// ===========================================================================
// §3 — production-quarry preference: a war factory (production) is targeted
// over a much nearer non-production building.
// ===========================================================================

#[test]
fn team_prefers_the_production_building_over_a_much_nearer_non_production_one() {
    let mut w = attacker_world(0xE5CA_4001, 8, Difficulty::Normal);

    // Non-production candidate: right next to the attacker's base.
    let near_other = w
        .spawn_building(B_OTHER, 2, CellCoord::new(25, 15))
        .unwrap();
    // Production candidate: far across the map — the only WEAP that exists.
    let far_weap = w
        .spawn_building(B_WEAP, 2, CellCoord::new(110, 110))
        .unwrap();

    wait_for_attacking(&mut w, 4000).expect("house 1 should form and attack a team");

    let members: Vec<_> = w
        .units
        .iter()
        .filter(|(_, u)| u.house == 1 && u.target.is_some())
        .map(|(_, u)| u.target)
        .collect();
    assert!(
        !members.is_empty(),
        "sanity: the team must have recruited members"
    );
    let targets_weap = members
        .iter()
        .all(|&t| t == Some(Target::Building(far_weap)));
    let any_targets_other = members.contains(&Some(Target::Building(near_other)));

    assert!(
        targets_weap,
        "the team must target the (distant) production building, not the \
         (much nearer) non-production one — the QUARRY_FACTORIES preference \
         (M7.11 P1c) only considers production buildings while any are alive: {members:?}"
    );
    assert!(!any_targets_other);
}
