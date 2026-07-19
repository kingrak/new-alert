//! Audit coverage (ra-tester, post-M7.5-B): edge-case depth for the APC
//! transport system (Q18 P1, `ra-sim/src/world.rs` `run_transports`/
//! `apply_load`/`apply_unload`/`board_passenger`/`free_unload_cell`) that the
//! coder's `mission_transport_suite.rs` smoke tests deliberately leave to us.
//!
//! Layout:
//! §1 capacity-full rejection
//! §2 board while the transport is moving (board_target chase + re-path)
//! §3 unload with zero free adjacent spots (no panic, cargo stays aboard)
//! §4 unload respects locomotor passability (water/impassable) + Q5.3 no
//!    vehicle/infantry co-occupancy
//! §5 transport destroyed mid-load-walk (board_target cleared, no dangling
//!    handle / panic)
//! §6 teamtype LOAD -> move -> UNLOAD -> attack, end to end
//! §7 a loaded transport that is part of a dissolving AI-composed team
//! §8 cargo hash: loaded vs unloaded worlds hash differently; same cargo
//!    hashes identically

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    AiPlayer, BuildingProto, Campaign, Catalog, Command, Difficulty, EconRules, Handle, Mission,
    MoveStats, Passability, TActionDef, TEventDef, TeamClass, TeamMission, TeamType, TriggerType,
    WarheadProfile, WeaponProfile, World,
};

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 40,
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

fn gun() -> WeaponProfile {
    WeaponProfile {
        damage: 20,
        rof: 20,
        range: 3 * 256,
        proj_speed: 999,
        proj_rot: 0,
        invisible: true,
        instant: true,
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

fn make_soldier(w: &mut World, house: u8, cell: CellCoord) -> Handle {
    let h = w.spawn_unit(0, house, cell, Facing(0), 50, stats());
    w.units.get_mut(h).unwrap().make_infantry(0);
    h
}

// ===========================================================================
// §1 — capacity-full rejection: a third Load onto a capacity-2 transport is a
// clean no-op — the extra passenger stays on the map, cargo does not exceed
// capacity, and no panic.
// ===========================================================================

#[test]
fn loading_beyond_capacity_is_refused_cleanly() {
    let mut w = World::new(Passability::all_passable(), 0xCA9A_0001);
    let apc = w.spawn_unit(0, 1, CellCoord::new(5, 5), Facing(0), 200, stats());
    w.set_unit_capacity(apc, 2);
    let a = make_soldier(&mut w, 1, CellCoord::new(6, 5));
    let b = make_soldier(&mut w, 1, CellCoord::new(4, 5));
    let c = make_soldier(&mut w, 1, CellCoord::new(5, 6));

    w.tick(&[
        Command::Load {
            passenger: a,
            transport: apc,
            house: 1,
        },
        Command::Load {
            passenger: b,
            transport: apc,
            house: 1,
        },
    ]);
    assert_eq!(
        w.units.get(apc).unwrap().cargo.len(),
        2,
        "filled to capacity"
    );
    assert!(w.units.get(a).is_none() && w.units.get(b).is_none());

    // The transport is now full; a third Load must be refused cleanly.
    w.tick(&[Command::Load {
        passenger: c,
        transport: apc,
        house: 1,
    }]);
    assert_eq!(
        w.units.get(apc).unwrap().cargo.len(),
        2,
        "cargo must not exceed capacity"
    );
    assert!(
        w.units.get(c).is_some(),
        "the refused passenger must remain a live unit on the map, not vanish"
    );
    assert!(
        w.units.get(c).unwrap().board_target.is_none(),
        "a cleanly-refused Load must not leave a dangling board intent"
    );
}

// ===========================================================================
// §2 — board while the transport is moving: a distant passenger's Load walks
// toward the transport's cell at command time; if the transport has moved on
// by the time it arrives, `run_transports` re-paths toward the transport's
// *current* cell rather than giving up. Documents + pins this exact behavior.
// ===========================================================================

#[test]
fn boarding_a_moving_transport_chases_and_re_paths_until_it_catches_up() {
    let mut w = World::new(Passability::all_passable(), 0xCA9A_0002);
    let apc = w.spawn_unit(0, 1, CellCoord::new(5, 5), Facing(0), 200, stats());
    w.set_unit_capacity(apc, 2);
    // The passenger starts far from the transport's initial position.
    let soldier = make_soldier(&mut w, 1, CellCoord::new(5, 25));

    // Order the transport to drive away immediately, and the passenger to
    // board it, in the same tick — the passenger commits to the transport's
    // cell at *that* moment, which will be stale within a few ticks.
    w.tick(&[
        Command::Move {
            unit: apc,
            dest: CellCoord::new(30, 5),
            house: 1,
        },
        Command::Load {
            passenger: soldier,
            transport: apc,
            house: 1,
        },
    ]);
    assert_eq!(
        w.units.get(soldier).unwrap().board_target,
        Some(apc),
        "the Load intent must be recorded even though the transport isn't adjacent"
    );

    // Let the transport finish its drive and stop; give the passenger a long
    // budget to catch up (it must re-path at least once en route).
    let mut boarded = false;
    for _ in 0..800 {
        w.tick(&[]);
        if w.units.get(soldier).is_none() {
            boarded = true;
            break;
        }
    }
    assert!(
        boarded,
        "a passenger chasing a moving transport must eventually catch up and \
         board once the transport settles, not give up"
    );
    assert_eq!(w.units.get(apc).unwrap().cargo.len(), 1);
}

// ===========================================================================
// §3 — unload with zero free adjacent spots: passengers stay aboard, no
// panic; a scripted (`unload_at`) transport automatically retries once space
// opens, while a manually-`Command::Unload`ed transport does not auto-retry
// (it is a one-shot order, matching `Command::Load`/`Unload`'s "issue an
// order" semantics elsewhere in the API).
// ===========================================================================

/// Fully boxes in `center` with 8 vehicles (one per neighbour) so no adjacent
/// cell is free for a disgorging *vehicle* passenger, and also saturates each
/// neighbour's infantry sub-cell spots so no *infantry* passenger has room
/// either. The transport's own cell is excluded (it holds the transport).
fn box_in_with_vehicles(w: &mut World, center: CellCoord, house: u8) {
    let offs = [
        (0, -1),
        (1, 0),
        (0, 1),
        (-1, 0),
        (-1, -1),
        (1, -1),
        (1, 1),
        (-1, 1),
    ];
    for (dx, dy) in offs {
        let c = CellCoord::new(center.x + dx, center.y + dy);
        w.spawn_unit(0, house, c, Facing(0), 100, stats());
    }
}

#[test]
fn manual_unload_with_no_free_spots_leaves_cargo_aboard_no_panic_no_auto_retry() {
    let mut w = World::new(Passability::all_passable(), 0xCA9A_0003);
    let apc = w.spawn_unit(0, 1, CellCoord::new(50, 50), Facing(0), 200, stats());
    w.set_unit_capacity(apc, 1);
    let soldier = make_soldier(&mut w, 1, CellCoord::new(50, 49));
    w.tick(&[Command::Load {
        passenger: soldier,
        transport: apc,
        house: 1,
    }]);
    assert_eq!(w.units.get(apc).unwrap().cargo.len(), 1);

    // Every neighbouring cell (and the transport's own, via the vehicle
    // occupancy rule) is blocked by a vehicle — no free spot exists anywhere
    // `free_unload_cell` looks.
    box_in_with_vehicles(&mut w, CellCoord::new(50, 50), 3);

    w.tick(&[Command::Unload {
        transport: apc,
        house: 1,
    }]);
    assert_eq!(
        w.units.get(apc).unwrap().cargo.len(),
        1,
        "with no free adjacent spot, the passenger must stay aboard"
    );

    // Clear a path (remove one blocker) but do NOT reissue Unload: a manual
    // Command::Unload is one-shot, not an auto-retrying scripted order.
    let blocker = w
        .units
        .iter()
        .find(|(h, u)| *h != apc && u.house == 3 && u.cell() == CellCoord::new(50, 49))
        .map(|(h, _)| h)
        .unwrap();
    w.units.remove(blocker);
    for _ in 0..20 {
        w.tick(&[]);
    }
    assert_eq!(
        w.units.get(apc).unwrap().cargo.len(),
        1,
        "a manual Unload must not silently keep retrying on its own once space frees up"
    );
}

#[test]
fn scripted_unload_at_automatically_retries_once_space_opens() {
    let mut w = World::new(Passability::all_passable(), 0xCA9A_0004);
    let apc = w.spawn_unit(0, 1, CellCoord::new(60, 60), Facing(0), 200, stats());
    w.set_unit_capacity(apc, 1);
    let soldier = make_soldier(&mut w, 1, CellCoord::new(60, 59));
    w.tick(&[Command::Load {
        passenger: soldier,
        transport: apc,
        house: 1,
    }]);
    box_in_with_vehicles(&mut w, CellCoord::new(60, 60), 3);

    // Flag it as a scripted auto-unload at its own (current) cell, as
    // `spawn_team`'s UNLOAD mission does — the transport is already "arrived".
    w.units.get_mut(apc).unwrap().unload_at = Some(CellCoord::new(60, 60));
    for _ in 0..5 {
        w.tick(&[]);
    }
    assert_eq!(
        w.units.get(apc).unwrap().cargo.len(),
        1,
        "still boxed in: cargo must stay aboard"
    );

    // Free one neighbour; the scripted transport must pick it up on its own,
    // without any further command, within a handful of ticks.
    let blocker = w
        .units
        .iter()
        .find(|(h, u)| *h != apc && u.house == 3 && u.cell() == CellCoord::new(60, 59))
        .map(|(h, _)| h)
        .unwrap();
    w.units.remove(blocker);
    let mut disgorged = false;
    for _ in 0..10 {
        w.tick(&[]);
        if w.units.get(apc).unwrap().cargo.is_empty() {
            disgorged = true;
            break;
        }
    }
    assert!(
        disgorged,
        "a scripted `unload_at` transport must auto-retry each tick until a spot opens"
    );
}

// ===========================================================================
// §4 — unload respects the passenger's locomotor passability (water/
// impassable terrain) and Q5.3 (no vehicle/infantry co-occupancy): a spot
// that is passable but already holds a vehicle is skipped even though it is
// otherwise "free" terrain.
// ===========================================================================

#[test]
fn unload_skips_impassable_water_and_picks_the_one_passable_neighbour() {
    // An island: every cell impassable for Foot except (51,50), one cell east
    // of the transport — the unload must land there, nowhere else.
    let (w2, h2) = (128, 128);
    let mut foot = vec![false; (w2 * h2) as usize];
    let mut wheel = vec![true; (w2 * h2) as usize];
    let idx = |c: CellCoord| (c.y * w2 + c.x) as usize;
    foot[idx(CellCoord::new(51, 50))] = true; // the sole passable-for-Foot cell
    wheel[idx(CellCoord::new(50, 50))] = true; // transport's own cell (Wheel)
    let track = wheel.clone();
    let mut w = World::new(
        Passability::per_locomotor(w2, h2, foot, track, wheel),
        0xCA9A_0005,
    );
    let apc = w.spawn_unit(0, 1, CellCoord::new(50, 50), Facing(0), 200, stats());
    w.set_unit_capacity(apc, 1);
    let soldier = make_soldier(&mut w, 1, CellCoord::new(50, 50));
    w.units.get_mut(soldier).unwrap().coord = CellCoord::new(50, 50).center();
    // Directly stow the passenger (bypassing the walk-to-board path, which
    // would itself need Foot passability at the transport's cell) to isolate
    // the unload-cell-selection logic under test.
    w.tick(&[Command::Load {
        passenger: soldier,
        transport: apc,
        house: 1,
    }]);
    assert_eq!(w.units.get(apc).unwrap().cargo.len(), 1);

    w.tick(&[Command::Unload {
        transport: apc,
        house: 1,
    }]);
    assert!(
        w.units.get(apc).unwrap().cargo.is_empty(),
        "the one Foot-passable neighbour must have been used"
    );
    let landed = w
        .units
        .iter()
        .find(|(h, u)| *h != apc && u.is_infantry())
        .map(|(_, u)| u.cell())
        .expect("the passenger re-materialised");
    assert_eq!(
        landed,
        CellCoord::new(51, 50),
        "unload must respect Foot passability — every other neighbour is water"
    );
}

#[test]
fn unload_never_places_infantry_onto_a_cell_already_holding_a_vehicle() {
    let mut w = World::new(Passability::all_passable(), 0xCA9A_0006);
    let apc = w.spawn_unit(0, 1, CellCoord::new(70, 70), Facing(0), 200, stats());
    w.set_unit_capacity(apc, 1);
    let soldier = make_soldier(&mut w, 1, CellCoord::new(70, 70));
    w.units.get_mut(soldier).unwrap().coord = CellCoord::new(70, 70).center();
    w.tick(&[Command::Load {
        passenger: soldier,
        transport: apc,
        house: 1,
    }]);
    // Occupy every neighbour with a vehicle EXCEPT (71,70), which is left
    // clear — Q5.3 must skip the vehicle-holding cells and land on the clear
    // one, even though their terrain is passable.
    let offs = [(0, -1), (0, 1), (-1, 0), (-1, -1), (1, -1), (1, 1), (-1, 1)];
    for (dx, dy) in offs {
        w.spawn_unit(
            0,
            3,
            CellCoord::new(70 + dx, 70 + dy),
            Facing(0),
            100,
            stats(),
        );
    }
    w.tick(&[Command::Unload {
        transport: apc,
        house: 1,
    }]);
    let landed = w
        .units
        .iter()
        .find(|(h, u)| *h != apc && u.is_infantry())
        .map(|(_, u)| u.cell());
    assert_eq!(
        landed,
        Some(CellCoord::new(71, 70)),
        "Q5.3: a vehicle-occupied cell must be skipped even though its terrain is passable"
    );
}

// ===========================================================================
// §5 — transport destroyed mid-load-walk: the boarder's `board_target` is
// cleared cleanly (a stale `Handle` never panics via the generational arena,
// but the intent must not dangle forever either).
// ===========================================================================

#[test]
fn transport_destroyed_while_a_passenger_is_walking_to_board_clears_the_intent() {
    let mut w = World::new(Passability::all_passable(), 0xCA9A_0007);
    let apc = w.spawn_unit(0, 1, CellCoord::new(40, 40), Facing(0), 200, stats());
    w.set_unit_capacity(apc, 2);
    let soldier = make_soldier(&mut w, 1, CellCoord::new(40, 60)); // far away — will walk

    w.tick(&[Command::Load {
        passenger: soldier,
        transport: apc,
        house: 1,
    }]);
    assert_eq!(w.units.get(soldier).unwrap().board_target, Some(apc));

    // A few ticks into the walk, destroy the transport outright.
    for _ in 0..5 {
        w.tick(&[]);
    }
    assert!(
        w.units.get(soldier).is_some(),
        "still walking, not yet aboard"
    );
    w.units.remove(apc);

    // No panic on the tick that discovers the transport is gone, and the
    // dangling intent is cleared rather than retried forever.
    w.tick(&[]);
    let s = w.units.get(soldier).unwrap();
    assert!(
        s.board_target.is_none(),
        "a destroyed transport must clear the boarder's intent, not leave a dangling handle"
    );
    // The soldier is otherwise unharmed and simulable — a few more ticks must
    // not panic either (regression guard for any leftover stale-handle path).
    for _ in 0..20 {
        w.tick(&[]);
    }
    assert!(w.units.get(soldier).is_some());
}

// ===========================================================================
// §6 — teamtype LOAD -> move -> UNLOAD -> attack, end to end: a scripted
// campaign assault team boards its transport at spawn, rides to the
// objective, disgorges, and the (now-Hunt) riders find and attack an enemy.
// ===========================================================================

fn ev(code: u8, data: i32) -> TEventDef {
    TEventDef {
        code,
        team: -1,
        data,
    }
}
fn act_team(code: u8, team: i32) -> TActionDef {
    TActionDef {
        code,
        team,
        trigger: -1,
        data: -1,
    }
}
#[allow(clippy::too_many_arguments)]
fn trig1(name: &str, persist: u8, house: i32, e1: TEventDef, a1: TActionDef) -> TriggerType {
    TriggerType {
        name: name.into(),
        persist,
        house,
        event_ctrl: ra_sim::campaign::multi::ONLY,
        action_ctrl: ra_sim::campaign::multi::ONLY,
        e1,
        e2: TEventDef {
            code: ra_sim::campaign::tevent::NONE,
            team: -1,
            data: -1,
        },
        a1,
        a2: TActionDef {
            code: ra_sim::campaign::taction::NONE,
            team: -1,
            trigger: -1,
            data: -1,
        },
    }
}

fn transport_proto() -> ra_sim::campaign::SpawnProto {
    ra_sim::campaign::SpawnProto {
        type_id: 100,
        max_health: 200,
        stats: stats(),
        armor: 0,
        weapon: None,
        secondary: None,
        has_turret: false,
        sight: 4,
        is_infantry: false,
        is_harvester: false,
        is_civ_evac: false,
        passengers: 2,
    }
}
fn rider_proto() -> ra_sim::campaign::SpawnProto {
    ra_sim::campaign::SpawnProto {
        type_id: 101,
        max_health: 50,
        stats: stats(),
        armor: 0,
        weapon: Some(gun()),
        secondary: None,
        has_turret: false,
        sight: 4,
        is_infantry: true,
        is_harvester: false,
        is_civ_evac: false,
        passengers: 0,
    }
}

fn base_campaign(triggers: Vec<TriggerType>, teamtypes: Vec<TeamType>) -> Campaign {
    let n = triggers.len();
    Campaign {
        triggers,
        teamtypes,
        waypoints: vec![-1; 101],
        globals: vec![false; 16],
        cell_triggers: Vec::new(),
        state: vec![ra_sim::campaign::TriggerState::default(); n],
        started: false,
        mission_timer: None,
        evac_cells: Vec::new(),
        civ_evacuated: vec![false; 8],
        reveal_all: false,
        reveal_cells: Vec::new(),
        pending_texts: Vec::new(),
        pending_speech: Vec::new(),
    }
}

#[test]
fn teamtype_load_move_unload_attack_runs_end_to_end() {
    // House 2 team: one transport + two infantry riders. Missions:
    // LOAD (board at spawn) -> ATT_WAYPT (ride to waypoint 5, "attack team" so
    // riders resume as Hunt on unload) -> UNLOAD (disgorge at the objective).
    let team = TeamType {
        name: "assault".into(),
        house: 2,
        flags: 0,
        recruit: 0,
        init_num: 1,
        max_allowed: 1,
        origin: 10, // spawn waypoint
        trigger: -1,
        classes: vec![
            TeamClass {
                proto: Some(transport_proto()),
                count: 1,
            },
            TeamClass {
                proto: Some(rider_proto()),
                count: 2,
            },
        ],
        missions: vec![
            TeamMission {
                code: ra_sim::campaign::tmission::LOAD,
                arg: 0,
            },
            TeamMission {
                code: ra_sim::campaign::tmission::ATT_WAYPT,
                arg: 5, // objective waypoint
            },
            TeamMission {
                code: ra_sim::campaign::tmission::UNLOAD,
                arg: 0,
            },
        ],
    };
    let t = trig1(
        "spawn",
        ra_sim::campaign::persist::VOLATILE,
        2,
        ev(ra_sim::campaign::tevent::TIME, 0),
        act_team(ra_sim::campaign::taction::REINFORCEMENTS, 0),
    );
    let mut camp = base_campaign(vec![t], vec![team]);
    camp.waypoints[10] = CellCoord::new(10, 10).to_index().unwrap() as i32;
    camp.waypoints[5] = CellCoord::new(40, 10).to_index().unwrap() as i32;

    let mut w = World::new(Passability::all_passable(), 0xCA9A_0008);
    w.init_houses(8, 0);
    w.set_player_house(1);
    w.set_campaign(camp);
    // A lone house-1 defender sitting at the objective, for the disgorged
    // Hunt riders to find and attack.
    let defender = w.spawn_unit(0, 1, CellCoord::new(41, 10), Facing(0), 100, stats());
    w.set_unit_combat(defender, 0, None, false);

    // Tick 1: REINFORCEMENTS spawns the team (transport + 2 riders adjacent),
    // LOAD boards the riders immediately, and the transport is ordered toward
    // waypoint 5 with `unload_at` set to the same objective.
    w.tick(&[]);
    let transport = w
        .units
        .iter()
        .find(|(_, u)| u.house == 2 && u.capacity > 0)
        .map(|(h, _)| h)
        .expect("the transport must have spawned");
    assert_eq!(
        w.units.get(transport).unwrap().cargo.len(),
        2,
        "both riders must have boarded immediately (LOAD, spawned adjacent)"
    );
    assert!(
        w.units
            .iter()
            .filter(|(_, u)| u.house == 2 && u.is_infantry())
            .count()
            == 0,
        "no house-2 infantry should be on the map while boarded"
    );

    // Ride to the objective and unload.
    let mut unloaded = false;
    for _ in 0..600 {
        w.tick(&[]);
        if w.units.get(transport).unwrap().cargo.is_empty() {
            unloaded = true;
            break;
        }
    }
    assert!(
        unloaded,
        "the transport must reach the objective and disgorge"
    );
    let riders: Vec<Handle> = w
        .units
        .iter()
        .filter(|(_, u)| u.house == 2 && u.is_infantry())
        .map(|(h, _)| h)
        .collect();
    assert_eq!(riders.len(), 2, "both riders must have re-materialised");
    assert!(
        riders
            .iter()
            .all(|&h| w.units.get(h).unwrap().mission == Mission::Hunt),
        "disgorged riders of an attack team (ATT_WAYPT) must resume as Hunt"
    );

    // The Hunt riders must find and actually damage the defender.
    let start_hp = w.units.get(defender).unwrap().health;
    let mut attacked = false;
    for _ in 0..400 {
        w.tick(&[]);
        if w.units.get(defender).map(|u| u.health).unwrap_or(0) < start_hp {
            attacked = true;
            break;
        }
    }
    assert!(
        attacked,
        "the unloaded assault squad must actually attack the objective's defender"
    );
}

// ===========================================================================
// §7 — a loaded transport that is part of a dissolving AI-composed team: the
// dissolve path (`AiPlayer::advance_team`) only ever issues retreat `Move`
// commands for surviving members — it must never touch cargo. With only 2
// armed house-1 vehicles (min_force at Hard difficulty is 2), the team's
// composition is deterministic: BOTH are always recruited.
// ===========================================================================

fn one_by_one(name: &str) -> BuildingProto {
    BuildingProto {
        is_barracks: false,
        name: name.to_string(),
        foot_w: 1,
        foot_h: 1,
        max_health: 100,
        armor: 0,
        power: 0,
        cost: 50,
        prereq: vec![],
        is_refinery: false,
        is_construction_yard: false,
        is_war_factory: false,
        free_harvester_unit: None,
        sight: 1,
        sprite_id: 0,
        weapon: None,
        has_turret: false,
        charges: false,
        is_wall: false,
        storage: 0,
    }
}

#[test]
fn loaded_transport_survives_a_team_dissolve_with_cargo_intact() {
    let mut w = World::new(Passability::all_passable(), 0x7EA5_0001);
    w.set_catalog(Catalog {
        buildings: vec![one_by_one("BASE")],
        units: vec![],
        econ: EconRules::default(),
    });
    w.init_houses(3, 0);
    // House 2: a static target so Expert_AI scoring has a candidate.
    w.spawn_building(0, 2, CellCoord::new(90, 64)).unwrap();

    // House 1: exactly two armed vehicles — an APC (capacity 2) and a plain
    // gunner. At Hard difficulty `min_force == 2 == vehicles.len()`, so both
    // are deterministically recruited into any team that forms, regardless
    // of the RNG jitter on `want_v`.
    let apc = w.spawn_unit(0, 1, CellCoord::new(20, 64), Facing(0), 200, stats());
    w.set_unit_combat(apc, 0, Some(gun()), true);
    w.set_unit_capacity(apc, 2);
    let gunner = w.spawn_unit(0, 1, CellCoord::new(21, 64), Facing(0), 200, stats());
    w.set_unit_combat(gunner, 0, Some(gun()), true);
    let rider = make_soldier(&mut w, 1, CellCoord::new(20, 65));
    w.units.get_mut(rider).unwrap().weapon = None; // never itself recruitable

    w.tick(&[Command::Load {
        passenger: rider,
        transport: apc,
        house: 1,
    }]);
    assert_eq!(w.units.get(apc).unwrap().cargo.len(), 1);

    w.set_ai(vec![AiPlayer::new(1, Difficulty::Hard)]);
    let mut formed = false;
    for _ in 0..4000 {
        w.tick(&[]);
        if let Some((n, init, _staging, _harass)) = w
            .ai()
            .iter()
            .find(|a| a.house() == 1)
            .unwrap()
            .team_summary()
        {
            assert_eq!(
                init, 2,
                "both house-1 vehicles must be recruited (min_force==2)"
            );
            assert_eq!(n, 2, "team starts at full strength");
            formed = true;
            break;
        }
    }
    assert!(formed, "house 1 must have formed a team within budget");
    assert_eq!(
        w.units.get(apc).unwrap().cargo.len(),
        1,
        "team formation (a Move to the rally point) must not disturb cargo"
    );

    // Kill the OTHER member (never the APC) so `alive` drops to 1, strictly
    // below `retreat_floor = max(2/2, 2) == 2` — forcing an immediate dissolve.
    w.units.remove(gunner);
    let mut dissolved = false;
    for _ in 0..50 {
        w.tick(&[]);
        if w.ai()
            .iter()
            .find(|a| a.house() == 1)
            .unwrap()
            .team_summary()
            .is_none()
        {
            dissolved = true;
            break;
        }
    }
    assert!(
        dissolved,
        "the team must dissolve one member below the floor"
    );
    assert!(
        w.units.get(apc).is_some(),
        "the APC itself must have survived"
    );
    assert_eq!(
        w.units.get(apc).unwrap().cargo.len(),
        1,
        "dissolve only issues a retreat Move for survivors — cargo must remain \
         exactly as it was, not dropped or lost"
    );
}

// ===========================================================================
// §8 — cargo hash: a loaded transport hashes differently from an unloaded
// one; two worlds with identical cargo hash identically (determinism / the
// hash-gating the coder's QUIRKS entry claims).
// ===========================================================================

#[test]
fn cargo_changes_the_state_hash_and_identical_cargo_hashes_identically() {
    let build = |seed: u32, load: bool| -> World {
        let mut w = World::new(Passability::all_passable(), seed);
        let apc = w.spawn_unit(0, 1, CellCoord::new(30, 30), Facing(0), 200, stats());
        w.set_unit_capacity(apc, 2);
        if load {
            let soldier = make_soldier(&mut w, 1, CellCoord::new(30, 30));
            w.units.get_mut(soldier).unwrap().coord = CellCoord::new(30, 30).center();
            w.tick(&[Command::Load {
                passenger: soldier,
                transport: apc,
                house: 1,
            }]);
        }
        w
    };
    let empty1 = build(0x1111, false);
    let empty2 = build(0x1111, false);
    let loaded1 = build(0x1111, true);
    let loaded2 = build(0x1111, true);

    assert_eq!(
        empty1.state_hash(),
        empty2.state_hash(),
        "same seed, no cargo: identical hash"
    );
    assert_eq!(
        loaded1.state_hash(),
        loaded2.state_hash(),
        "same seed, identical cargo: identical hash"
    );
    assert_ne!(
        empty1.state_hash(),
        loaded1.state_hash(),
        "loaded vs unloaded must hash differently — cargo is folded into the hash"
    );
}
