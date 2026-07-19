//! M7.5-A test coverage: the campaign trigger/teamtype engine
//! (`ra-sim/src/campaign.rs` + `run_campaign`/`maybe_spring`/`eval_event`/
//! `run_action` in `ra-sim/src/world.rs`), asset-free, against synthetic
//! scenarios. Every scenario is hand-built (no real INI), so expectations are
//! derived directly from the reference (`trigger.cpp`/`tevent.cpp`/
//! `taction.cpp`) and pinned with a citation in the doc comment.
//!
//! Layout: §1 per-event tests, §2 per-action tests, §3 persistence semantics,
//! §4 AND/OR/LINKED `MultiStyleType` semantics, §5 determinism/hash-gating.
//!
//! `ticks_per_minute` is set to 10 in the fixture catalog (vs. the real 900)
//! so `TEVENT_TIME`'s tenths-of-a-minute unit maps to exactly 1 tick/tenth —
//! keeps "fires at exact tick" assertions small and legible.

use ra_sim::campaign::{multi, persist, taction, tevent, tmission, TriggerState};
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    BuildingProto, Campaign, Catalog, EconRules, GameOver, MoveStats, Passability, SpawnProto,
    TActionDef, TEventDef, TeamClass, TeamMission, TeamType, TriggerType, World,
};

// ===========================================================================
// Fixture builders
// ===========================================================================

const B_HUT: u32 = 0; // plain non-combat structure, for BUILDING_EXISTS

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 20,
        rot: 8,
    }
}

fn catalog() -> Catalog {
    // 10 ticks/minute => ticks_per_tenth = max(10/10,1) = 1: TIME's `data` is
    // then exactly the tick count, one-for-one.
    let econ = EconRules {
        ticks_per_minute: 10,
        ..EconRules::default()
    };
    Catalog {
        buildings: vec![BuildingProto {
            name: "HUT".into(),
            foot_w: 1,
            foot_h: 1,
            max_health: 100,
            armor: 0,
            power: 0,
            cost: 100,
            prereq: vec![],
            is_refinery: false,
            is_construction_yard: false,
            is_war_factory: false,
            is_barracks: false,
            free_harvester_unit: None,
            sight: 5,
            sprite_id: 0,
            weapon: None,
            has_turret: false,
            charges: false,
            is_wall: false,
            storage: 0,
        }],
        units: vec![],
        econ,
    }
}

fn none_event() -> TEventDef {
    TEventDef {
        code: tevent::NONE,
        team: -1,
        data: 0,
    }
}
fn none_action() -> TActionDef {
    TActionDef {
        code: taction::NONE,
        team: -1,
        trigger: -1,
        data: -1,
    }
}
fn ev(code: u8, data: i32) -> TEventDef {
    TEventDef {
        code,
        team: -1,
        data,
    }
}
fn act(code: u8, data: i32) -> TActionDef {
    TActionDef {
        code,
        team: -1,
        trigger: -1,
        data,
    }
}
fn act_trig(code: u8, trigger: i32) -> TActionDef {
    TActionDef {
        code,
        team: -1,
        trigger,
        data: -1,
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

/// A one-event/one-action trigger (`MULTI_ONLY` on both control fields — the
/// common case).
#[allow(clippy::too_many_arguments)]
fn trig1(name: &str, persist: u8, house: i32, e1: TEventDef, a1: TActionDef) -> TriggerType {
    TriggerType {
        name: name.into(),
        persist,
        house,
        event_ctrl: multi::ONLY,
        action_ctrl: multi::ONLY,
        e1,
        e2: none_event(),
        a1,
        a2: none_action(),
    }
}

/// A two-event/two-action trigger with explicit event/action control.
#[allow(clippy::too_many_arguments)]
fn trig2(
    name: &str,
    persist: u8,
    house: i32,
    ectrl: u8,
    actctrl: u8,
    e1: TEventDef,
    e2: TEventDef,
    a1: TActionDef,
    a2: TActionDef,
) -> TriggerType {
    TriggerType {
        name: name.into(),
        persist,
        house,
        event_ctrl: ectrl,
        action_ctrl: actctrl,
        e1,
        e2,
        a1,
        a2,
    }
}

/// Build a `World` (8 houses, player = house 1) carrying the given campaign
/// state. `waypoints`/`cell_triggers` default empty/unset unless the caller
/// mutates the returned `Campaign` before attaching — most tests use
/// [`world_with`] directly.
fn base_campaign(triggers: Vec<TriggerType>, teamtypes: Vec<TeamType>) -> Campaign {
    let n = triggers.len();
    Campaign {
        triggers,
        teamtypes,
        waypoints: vec![-1; 101],
        globals: vec![false; 16],
        cell_triggers: Vec::new(),
        state: vec![TriggerState::default(); n],
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

fn world_with(camp: Campaign) -> World {
    let mut w = World::new(Passability::all_passable(), 0xC0FF_EE01);
    w.set_catalog(catalog());
    w.init_houses(8, 0);
    w.set_player_house(1);
    w.set_campaign(camp);
    w
}

fn globals(w: &World) -> &[bool] {
    &w.campaign().unwrap().globals
}

// ===========================================================================
// §1 Per-event tests
// ===========================================================================

/// `TEVENT_TIME` (`tevent.cpp:256`): `if (td.Timer != 0) return(false); return
/// (true);`. Our port seeds `e1_timer = data * ticks_per_tenth` on the FIRST
/// `run_campaign` call (which is itself tick 1) and then, in that same call,
/// falls through to the "advance timers" step that decrements it once — so
/// the seed-and-first-decrement happen on the same tick. With `data = 3` and
/// `ticks_per_tenth = 1` (fixture), the timer reads 3 (seeded), 2 (tick 1's
/// decrement), 1 (tick 2), 0 (tick 3) — firing on the 3rd `tick()` call.
#[test]
fn time_event_fires_at_the_exact_tick_not_one_off() {
    let t = trig1(
        "t",
        persist::VOLATILE,
        1,
        ev(tevent::TIME, 3),
        act(taction::SET_GLOBAL, 0),
    );
    let mut w = world_with(base_campaign(vec![t], vec![]));
    for i in 1..=2 {
        w.tick(&[]);
        assert!(
            !globals(&w)[0],
            "tick {i}/3: TIME(3) must not have fired yet"
        );
    }
    w.tick(&[]);
    assert!(globals(&w)[0], "tick 3: TIME(3) must fire exactly now");
}

/// `TEVENT_DESTROYED` VOLATILE: any carrier death latches `any_destroyed` and
/// fires on the very next evaluation, regardless of how many carriers remain
/// (`trigger.cpp` VOLATILE deletes on first spring; our port's `sprung` gate
/// achieves the same one-shot effect).
#[test]
fn destroyed_volatile_fires_on_first_carrier_death_with_survivors_remaining() {
    let t = trig1(
        "t",
        persist::VOLATILE,
        1,
        ev(tevent::DESTROYED, 0),
        act(taction::SET_GLOBAL, 0),
    );
    let mut w = world_with(base_campaign(vec![t], vec![]));
    let a = w.spawn_unit(0, 1, CellCoord::new(5, 5), Facing(0), 50, stats());
    let b = w.spawn_unit(0, 1, CellCoord::new(6, 5), Facing(0), 50, stats());
    w.units.get_mut(a).unwrap().trigger = Some(0);
    w.units.get_mut(b).unwrap().trigger = Some(0);
    w.tick(&[]); // seeds carriers_init = 2
    assert!(!globals(&w)[0]);
    w.units.remove(a); // one of two dies — survivor `b` remains alive
    w.tick(&[]);
    assert!(
        globals(&w)[0],
        "VOLATILE DESTROYED must fire on the FIRST death, not wait for all carriers"
    );
}

/// `TEVENT_DESTROYED` SEMIPERSISTANT: the reference detaches the dying
/// object's trigger and only springs once `AttachCount` drops to zero
/// (`trigger.cpp:275-294`). Our port mirrors this via `carriers == 0 &&
/// carriers_init > 0`: must NOT fire while any carrier survives.
#[test]
fn destroyed_semi_persistent_waits_for_all_carriers_dead() {
    let t = trig1(
        "t",
        persist::SEMI,
        1,
        ev(tevent::DESTROYED, 0),
        act(taction::SET_GLOBAL, 0),
    );
    let mut w = world_with(base_campaign(vec![t], vec![]));
    let a = w.spawn_unit(0, 1, CellCoord::new(5, 5), Facing(0), 50, stats());
    let b = w.spawn_unit(0, 1, CellCoord::new(6, 5), Facing(0), 50, stats());
    w.units.get_mut(a).unwrap().trigger = Some(0);
    w.units.get_mut(b).unwrap().trigger = Some(0);
    w.tick(&[]);
    w.units.remove(a);
    w.tick(&[]);
    assert!(!globals(&w)[0], "one carrier still alive: must not fire");
    w.units.remove(b);
    w.tick(&[]);
    assert!(globals(&w)[0], "all carriers dead: must fire now");
}

/// `GLOBAL_SET`/`GLOBAL_CLEAR` gating, including a trigger disabled-then-
/// re-enabled purely by global flag flips (no other event drives it).
#[test]
fn global_set_and_clear_gate_a_trigger_disabled_then_enabled() {
    // Fires only while global[2] is CLEAR (the "disabled by a global" case).
    let gated = trig1(
        "gated",
        persist::PERSISTANT,
        1,
        ev(tevent::GLOBAL_CLEAR, 2),
        act(taction::SET_GLOBAL, 5),
    );
    let mut w = world_with(base_campaign(vec![gated], vec![]));
    w.tick(&[]);
    assert!(
        globals(&w)[5],
        "global[2] starts clear: GLOBAL_CLEAR event true, fires"
    );
    // Now flip global[2] SET, which must disable it (event false from here).
    if let Some(c) = w.campaign_mut() {
        c.globals[2] = true;
        c.globals[5] = false; // reset the effect flag to observe re-firing
    }
    w.tick(&[]);
    assert!(
        !globals(&w)[5],
        "global[2] now set: GLOBAL_CLEAR must be false, no re-fire"
    );
}

/// A GLOBAL_SET-gated trigger: false until set, true after.
#[test]
fn global_set_event_only_fires_after_the_flag_is_raised() {
    let setter = trig1(
        "setter",
        persist::VOLATILE,
        1,
        ev(tevent::TIME, 2),
        act(taction::SET_GLOBAL, 0),
    );
    let gated = trig1(
        "gated",
        persist::VOLATILE,
        1,
        ev(tevent::GLOBAL_SET, 0),
        act(taction::SET_GLOBAL, 1),
    );
    let mut w = world_with(base_campaign(vec![setter, gated], vec![]));
    w.tick(&[]);
    assert!(
        !globals(&w)[1],
        "global[0] not set yet: gated must be false"
    );
    w.tick(&[]); // TIME(2): timer reads 2,1 on ticks1,2 -> fires on tick 2? verify boundary
                 // (setter fires on the 3rd tick per the TIME semantics pinned above; data=2)
    w.tick(&[]);
    assert!(
        globals(&w)[1],
        "global[0] set by `setter`: `gated` must now fire"
    );
}

/// `PLAYER_ENTERED`/`CROSS_*` cell triggers: a unit of the matching house
/// standing on a `[CellTriggers]` cell satisfies the event
/// (`foot.cpp:1500/1517/1533` call `Spring(TEVENT_PLAYER_ENTERED/CROSS_*, ...)`
/// only when the mover is on that exact cell; our port polls `cell()==cc`
/// every tick, which is behaviorally equivalent for a stationary/settled unit).
#[test]
fn player_entered_cell_trigger_fires_when_the_right_house_stands_on_it() {
    let cell = CellCoord::new(8, 8);
    let t = trig1(
        "enter",
        persist::VOLATILE,
        1,
        ev(tevent::PLAYER_ENTERED, 1), // Data.House = house 1
        act(taction::SET_GLOBAL, 0),
    );
    let mut camp = base_campaign(vec![t], vec![]);
    camp.cell_triggers.push((cell.to_index().unwrap(), 0));
    let mut w = world_with(camp);
    w.tick(&[]);
    assert!(!globals(&w)[0], "nobody on the cell yet");
    // Wrong house on the cell must not satisfy it.
    let intruder = w.spawn_unit(0, 2, cell, Facing(0), 50, stats());
    w.tick(&[]);
    assert!(
        !globals(&w)[0],
        "house 2 on the cell: house-1 trigger must not fire"
    );
    w.units.remove(intruder);
    // Correct house.
    w.spawn_unit(0, 1, cell, Facing(0), 50, stats());
    w.tick(&[]);
    assert!(globals(&w)[0], "house 1 now on the cell: must fire");
}

/// `CROSS_HORIZONTAL`/`CROSS_VERTICAL` share the same cell-trigger evaluation
/// path in our port (`eval_event`'s `PLAYER_ENTERED | CROSS_HORIZONTAL |
/// CROSS_VERTICAL` arm) — pin that both codes are wired, not just
/// PLAYER_ENTERED.
#[test]
fn cross_horizontal_and_vertical_use_the_same_cell_trigger_path() {
    let cell = CellCoord::new(9, 9);
    let th = trig1(
        "ch",
        persist::VOLATILE,
        1,
        ev(tevent::CROSS_HORIZONTAL, 1),
        act(taction::SET_GLOBAL, 0),
    );
    let tv = trig1(
        "cv",
        persist::VOLATILE,
        1,
        ev(tevent::CROSS_VERTICAL, 1),
        act(taction::SET_GLOBAL, 1),
    );
    let mut camp = base_campaign(vec![th, tv], vec![]);
    camp.cell_triggers.push((cell.to_index().unwrap(), 0));
    camp.cell_triggers.push((cell.to_index().unwrap(), 1));
    let mut w = world_with(camp);
    w.spawn_unit(0, 1, cell, Facing(0), 50, stats());
    w.tick(&[]);
    assert!(globals(&w)[0], "CROSS_HORIZONTAL must fire");
    assert!(globals(&w)[1], "CROSS_VERTICAL must fire");
}

/// `TEVENT_LOW_POWER`: `eval_event`'s `LOW_POWER` arm reads the house index
/// from the event's `Data` (`ev.data`), not the trigger's owning `House` —
/// pin that this is the field actually consulted.
#[test]
fn low_power_event_reads_the_house_from_event_data() {
    let t = trig1(
        "lp",
        persist::VOLATILE,
        1,
        ev(tevent::LOW_POWER, 3), // house 3, NOT the trigger's house=1
        act(taction::SET_GLOBAL, 0),
    );
    let mut w = world_with(base_campaign(vec![t], vec![]));
    w.tick(&[]);
    assert!(!globals(&w)[0], "house 3 has no drain yet: not low power");
    w.houses[3].power_drain = 100;
    w.houses[3].power_output = 10;
    w.tick(&[]);
    assert!(globals(&w)[0], "house 3 is now low on power: must fire");
}

/// `TEVENT_BUILDING_EXISTS`: true while the trigger's OWN house (`t.house`,
/// unlike LOW_POWER) has any live non-wall building.
#[test]
fn building_exists_event_tracks_the_triggers_own_house() {
    let t = trig1(
        "be",
        persist::VOLATILE,
        1,
        ev(tevent::BUILDING_EXISTS, 0),
        act(taction::SET_GLOBAL, 0),
    );
    let mut w = world_with(base_campaign(vec![t], vec![]));
    w.tick(&[]);
    assert!(!globals(&w)[0], "house 1 owns no building yet");
    let b = w.spawn_building(B_HUT, 1, CellCoord::new(20, 20)).unwrap();
    w.tick(&[]);
    assert!(globals(&w)[0], "house 1 now owns a building: must fire");
    let _ = b;
}

// ===========================================================================
// §2 Per-action tests
// ===========================================================================

#[test]
fn win_action_sets_victory() {
    let t = trig1(
        "w",
        persist::VOLATILE,
        1,
        ev(tevent::TIME, 0),
        act(taction::WIN, -1),
    );
    let mut w = world_with(base_campaign(vec![t], vec![]));
    w.tick(&[]);
    assert_eq!(w.game_over(), GameOver::Victory);
}

#[test]
fn lose_action_sets_defeat() {
    let t = trig1(
        "l",
        persist::VOLATILE,
        1,
        ev(tevent::TIME, 0),
        act(taction::LOSE, -1),
    );
    let mut w = world_with(base_campaign(vec![t], vec![]));
    w.tick(&[]);
    assert_eq!(w.game_over(), GameOver::Defeat);
}

fn simple_proto() -> SpawnProto {
    SpawnProto {
        type_id: 7,
        max_health: 80,
        stats: stats(),
        armor: 0,
        weapon: None,
        secondary: None,
        has_turret: false,
        sight: 4,
        is_infantry: false,
        is_harvester: false,
        is_civ_evac: false,
    }
}

/// `TACTION_REINFORCEMENTS` (`reinf.cpp` `Do_Reinforcements`): spawns the
/// team's members at its origin waypoint, tagged with the team's house.
#[test]
fn reinforcements_spawns_team_at_waypoint_with_correct_house_and_count() {
    let team = TeamType {
        name: "rf".into(),
        house: 2,
        flags: 0,
        recruit: 0,
        init_num: 1,
        max_allowed: 1,
        origin: 10,
        trigger: -1,
        classes: vec![TeamClass {
            proto: Some(simple_proto()),
            count: 3,
        }],
        missions: vec![],
    };
    let t = trig1(
        "spawn",
        persist::VOLATILE,
        1,
        ev(tevent::TIME, 0),
        act_team(taction::REINFORCEMENTS, 0),
    );
    let mut camp = base_campaign(vec![t], vec![team]);
    camp.waypoints[10] = CellCoord::new(15, 15).to_index().unwrap() as i32;
    let mut w = world_with(camp);
    let before = w.units.len();
    w.tick(&[]);
    assert_eq!(
        w.units.len(),
        before + 3,
        "3 members of the team must spawn"
    );
    let spawned: Vec<_> = w
        .units
        .iter()
        .filter(|(_, u)| u.type_id == 7)
        .map(|(_, u)| u.house)
        .collect();
    assert_eq!(spawned.len(), 3);
    assert!(
        spawned.iter().all(|&h| h == 2),
        "all spawn as house 2 (the team's house)"
    );
}

/// `TACTION_CREATE_TEAM` (Q17 deviation #2: recruits existing idle units, no
/// per-class matching) — recruits up to the team's total class count from the
/// matching house only, and applies the team's mission (hunt).
#[test]
fn create_team_recruits_idle_units_of_the_right_house_only() {
    let team = TeamType {
        name: "ct".into(),
        house: 1,
        flags: 0,
        recruit: 0,
        init_num: 1,
        max_allowed: 1,
        origin: -1,
        trigger: -1,
        classes: vec![TeamClass {
            proto: None,
            count: 2,
        }],
        missions: vec![TeamMission {
            code: tmission::ATTACK,
            arg: 0,
        }],
    };
    let t = trig1(
        "recruit",
        persist::VOLATILE,
        1,
        ev(tevent::TIME, 0),
        act_team(taction::CREATE_TEAM, 0),
    );
    let mut w = world_with(base_campaign(vec![t], vec![team]));
    // 3 idle house-1 units (only 2 should be recruited) + 1 house-2 unit (must
    // never be recruited).
    let h1a = w.spawn_unit(0, 1, CellCoord::new(1, 1), Facing(0), 50, stats());
    let h1b = w.spawn_unit(0, 1, CellCoord::new(2, 1), Facing(0), 50, stats());
    let h1c = w.spawn_unit(0, 1, CellCoord::new(3, 1), Facing(0), 50, stats());
    let h2 = w.spawn_unit(0, 2, CellCoord::new(4, 1), Facing(0), 50, stats());
    w.tick(&[]);
    let recruited = [h1a, h1b, h1c]
        .iter()
        .filter(|&&h| w.units.get(h).unwrap().hunt)
        .count();
    assert_eq!(
        recruited, 2,
        "exactly 2 of the 3 idle house-1 units recruited (want=count=2)"
    );
    assert!(
        !w.units.get(h2).unwrap().hunt,
        "house-2 unit must never be recruited"
    );
}

/// `TACTION_FORCE_TRIGGER` (`taction.cpp:584-588`): forces another trigger's
/// `Spring(..., forced=true)` — our port resolves this in the SAME tick via
/// the `forced` worklist, so a chain of two triggers both take effect on one
/// `tick()`.
#[test]
fn force_trigger_chains_resolve_within_the_same_tick() {
    let a = trig1(
        "a",
        persist::VOLATILE,
        1,
        ev(tevent::TIME, 0),
        act_trig(taction::FORCE_TRIGGER, 1),
    );
    // `b`'s own event (GLOBAL_SET on a global that's never set) would never
    // naturally fire — only FORCE_TRIGGER can spring it.
    let b = trig1(
        "b",
        persist::VOLATILE,
        1,
        ev(tevent::GLOBAL_SET, 9),
        act(taction::SET_GLOBAL, 0),
    );
    let mut w = world_with(base_campaign(vec![a, b], vec![]));
    w.tick(&[]);
    assert!(
        globals(&w)[0],
        "FORCE_TRIGGER(b) must spring b's action in the same tick as a"
    );
}

/// `TACTION_DESTROY_TRIGGER` on a non-persistent target: prevents it from
/// ever firing, even though its own event would have qualified later
/// (`taction.cpp:568-578` deletes the trigger instance outright).
#[test]
fn destroy_trigger_prevents_a_volatile_target_from_ever_firing() {
    let killer = trig1(
        "killer",
        persist::VOLATILE,
        1,
        ev(tevent::TIME, 0),
        act_trig(taction::DESTROY_TRIGGER, 1),
    );
    let victim = trig1(
        "victim",
        persist::VOLATILE,
        1,
        ev(tevent::TIME, 5),
        act(taction::SET_GLOBAL, 0),
    );
    let mut w = world_with(base_campaign(vec![killer, victim], vec![]));
    for _ in 0..10 {
        w.tick(&[]);
    }
    assert!(
        !globals(&w)[0],
        "victim's TIME(5) must never fire: it was destroyed at tick 0"
    );
}

/// KNOWN BUG (report): `TACTION_DESTROY_TRIGGER` on a PERSISTANT target.
/// Reference `taction.cpp:568-578` deletes the target `TriggerClass`
/// instance outright, UNCONDITIONALLY — persistence only controls whether a
/// trigger deletes ITSELF after firing, it has no bearing on whether an
/// explicit `DESTROY_TRIGGER` action can remove it.
///
/// Our port's `maybe_spring` (`ra-sim/src/world.rs`) gates re-evaluation on
/// `camp.state[i].sprung && persist != PERSISTANT && !forced` — i.e. the
/// "already handled, skip" check is deliberately bypassed for PERSISTANT
/// triggers (so they keep re-arming/re-firing normally). `DESTROY_TRIGGER`
/// sets `sprung = true` on its target, which is exactly the flag that gate
/// ignores for PERSISTANT triggers — so destroying a PERSISTANT trigger is a
/// silent no-op in our port: it keeps firing on schedule as if never
/// destroyed. This test encodes the reference-correct expectation (destroyed
/// PERSISTANT trigger never fires) and is expected to currently FAIL.
#[test]
fn destroy_trigger_should_also_stop_a_persistant_target() {
    let killer = trig1(
        "killer",
        persist::VOLATILE,
        1,
        ev(tevent::TIME, 0),
        act_trig(taction::DESTROY_TRIGGER, 1),
    );
    let victim = trig1(
        "victim",
        persist::PERSISTANT,
        1,
        ev(tevent::TIME, 5),
        act(taction::SET_GLOBAL, 0),
    );
    let mut w = world_with(base_campaign(vec![killer, victim], vec![]));
    for _ in 0..10 {
        w.tick(&[]);
    }
    assert!(
        !globals(&w)[0],
        "reference: a destroyed PERSISTANT trigger must never fire, regardless of persistence"
    );
}

#[test]
fn set_and_clear_global_actions_flip_the_flag() {
    let setter = trig1(
        "s",
        persist::VOLATILE,
        1,
        ev(tevent::TIME, 0),
        act(taction::SET_GLOBAL, 4),
    );
    // TIME(2), not TIME(1): per the fixture's tick cadence (pinned in
    // `time_event_fires_at_the_exact_tick_not_one_off`), TIME(0) and TIME(1)
    // both fire on tick 1 (the seed-and-first-decrement collapse onto the
    // same `run_campaign` call) — using TIME(1) here would clear the flag on
    // the SAME tick it was set, defeating the point of this test.
    let clearer = trig1(
        "c",
        persist::VOLATILE,
        1,
        ev(tevent::TIME, 2),
        act(taction::CLEAR_GLOBAL, 4),
    );
    let mut w = world_with(base_campaign(vec![setter, clearer], vec![]));
    w.tick(&[]);
    assert!(globals(&w)[4], "SET_GLOBAL(4) must raise it");
    w.tick(&[]);
    assert!(!globals(&w)[4], "CLEAR_GLOBAL(4) must lower it");
}

/// `TACTION_REVEAL_ALL`/`REVEAL_SOME` affect the player house's shroud.
#[test]
fn reveal_actions_affect_shroud() {
    let all_t = trig1(
        "ra",
        persist::VOLATILE,
        1,
        ev(tevent::TIME, 0),
        act(taction::REVEAL_ALL, -1),
    );
    let mut camp = base_campaign(vec![all_t], vec![]);
    camp.waypoints[20] = CellCoord::new(30, 30).to_index().unwrap() as i32;
    let mut w = world_with(camp);
    w.shroud.enable();
    let far = CellCoord::new(60, 60);
    assert!(!w.shroud.is_explored(1, far), "shroud starts unexplored");
    w.tick(&[]);
    assert!(
        w.shroud.is_explored(1, far),
        "REVEAL_ALL must explore the whole map for the player house"
    );
}

#[test]
fn reveal_some_reveals_only_around_the_waypoint() {
    let t = trig1(
        "rs",
        persist::VOLATILE,
        1,
        ev(tevent::TIME, 0),
        act(taction::REVEAL_SOME, 20),
    );
    let mut camp = base_campaign(vec![t], vec![]);
    camp.waypoints[20] = CellCoord::new(30, 30).to_index().unwrap() as i32;
    let mut w = world_with(camp);
    w.shroud.enable();
    let far = CellCoord::new(90, 90);
    w.tick(&[]);
    assert!(
        w.shroud.is_explored(1, CellCoord::new(30, 30)),
        "REVEAL_SOME must explore around its waypoint"
    );
    assert!(
        !w.shroud.is_explored(1, far),
        "REVEAL_SOME must NOT explore the whole map"
    );
}

/// `TACTION_ALL_HUNT`: every live unit of the target house goes to hunt mode.
#[test]
fn all_hunt_sets_hunt_on_every_live_unit_of_the_house() {
    let t = trig1(
        "hunt",
        persist::VOLATILE,
        1,
        ev(tevent::TIME, 0),
        act(taction::ALL_HUNT, 2),
    );
    let mut w = world_with(base_campaign(vec![t], vec![]));
    let a = w.spawn_unit(0, 2, CellCoord::new(1, 1), Facing(0), 50, stats());
    let b = w.spawn_unit(0, 2, CellCoord::new(2, 1), Facing(0), 50, stats());
    let other_house = w.spawn_unit(0, 3, CellCoord::new(3, 1), Facing(0), 50, stats());
    w.tick(&[]);
    assert!(w.units.get(a).unwrap().hunt);
    assert!(w.units.get(b).unwrap().hunt);
    assert!(
        !w.units.get(other_house).unwrap().hunt,
        "other houses unaffected"
    );
}

// ===========================================================================
// §3 Persistence semantics (`trigger.cpp:275-360`)
// ===========================================================================

/// VOLATILE fires exactly once even though its qualifying condition (a global
/// that stays set) remains true for many subsequent ticks
/// (`trigger.cpp:277-282`: VOLATILE is deleted after firing).
#[test]
fn volatile_trigger_fires_exactly_once_while_condition_stays_true() {
    let t = trig1(
        "v",
        persist::VOLATILE,
        1,
        ev(tevent::GLOBAL_CLEAR, 0), // global[0] starts false => always true
        act(taction::SET_GLOBAL, 1),
    );
    let mut w = world_with(base_campaign(vec![t], vec![]));
    let mut fire_count = 0;
    for _ in 0..20 {
        // reset the observation flag each tick so we can count re-firings
        if let Some(c) = w.campaign_mut() {
            if c.globals[1] {
                fire_count += 1;
                c.globals[1] = false;
            }
        }
        w.tick(&[]);
    }
    assert_eq!(fire_count, 1, "a VOLATILE trigger must only ever fire once");
}

/// KNOWN BUG (report, do not silently weaken): PERSISTANT persistence never
/// resets, in the reference, the underlying event's "already tripped" state —
/// `trigger.cpp` calls `Class->Event1.Reset(Event1)` after every firing of a
/// non-deleted (SEMI-with-survivors or PERSISTANT) trigger
/// (`trigger.cpp:355-360`), and `TEventClass::Reset` (`tevent.cpp:181-187`)
/// re-arms a `TEVENT_TIME` event's `Timer` to `Data.Value *
/// (TICKS_PER_MINUTE/10)` — i.e. a PERSISTANT `TIME` trigger re-fires once per
/// *interval*, not once per tick.
///
/// Our port's `maybe_spring`/`run_campaign` (`ra-sim/src/world.rs`) never
/// resets `TriggerState::e1_timer`/`e2_timer` for a PERSISTANT trigger (the
/// countdown is only ever decremented, and `sprung` is deliberately left
/// `false` for PERSISTANT so the *next* tick re-evaluates it) — so once the
/// timer reaches 0 it stays at 0 forever, and the PERSISTANT trigger fires on
/// **every subsequent tick**, not once every `data` tenths-of-a-minute.
///
/// This test encodes the REFERENCE-correct expectation (fires on ticks 3, 6,
/// 9, ... not every tick from 3 onward) and is expected to currently FAIL —
/// that failure is the bug report. See the suite's final report for the
/// pinned repro.
#[test]
fn persistant_time_trigger_should_rearm_the_interval_not_fire_every_tick() {
    let t = trig1(
        "p",
        persist::PERSISTANT,
        1,
        ev(tevent::TIME, 3),
        act(taction::SET_GLOBAL, 0),
    );
    let mut w = world_with(base_campaign(vec![t], vec![]));
    let mut fire_ticks: Vec<u32> = Vec::new();
    for i in 1..=9u32 {
        if let Some(c) = w.campaign_mut() {
            c.globals[0] = false;
        }
        w.tick(&[]);
        if globals(&w)[0] {
            fire_ticks.push(i);
        }
    }
    assert_eq!(
        fire_ticks,
        vec![3, 6, 9],
        "reference semantics: PERSISTANT TIME(3) re-arms and fires every 3rd tick"
    );
}

/// SEMI persists until all carriers are gone but the reference still deletes
/// it at that point (`trigger.cpp:288-292`: `AttachCount <= 0` after the
/// SEMIPERSISTANT branch also falls into the VOLATILE-style delete): must
/// fire exactly once, not repeatedly, once its last carrier dies.
#[test]
fn semi_persistent_fires_exactly_once_once_all_carriers_are_gone() {
    let t = trig1(
        "s",
        persist::SEMI,
        1,
        ev(tevent::DESTROYED, 0),
        act(taction::SET_GLOBAL, 0),
    );
    let mut w = world_with(base_campaign(vec![t], vec![]));
    let a = w.spawn_unit(0, 1, CellCoord::new(5, 5), Facing(0), 50, stats());
    w.units.get_mut(a).unwrap().trigger = Some(0);
    w.tick(&[]);
    w.units.remove(a);
    let mut fire_count = 0;
    for _ in 0..5 {
        if let Some(c) = w.campaign_mut() {
            if c.globals[0] {
                fire_count += 1;
                c.globals[0] = false;
            }
        }
        w.tick(&[]);
    }
    assert_eq!(
        fire_count, 1,
        "SEMI must fire exactly once, not keep re-firing"
    );
}

// ===========================================================================
// §4 `MultiStyleType` (event/action control) semantics — `trigger.cpp:248-323`
// ===========================================================================

/// `MULTI_AND`: both events must be true; once satisfied, BOTH actions run
/// unconditionally (`trigger.cpp:311-317`: the non-LINKED `default: case
/// MULTI_AND` branch runs `Action1` then `Action2` regardless of the
/// individual e1/e2 split).
#[test]
fn and_event_control_requires_both_events_and_runs_both_actions() {
    let t = trig2(
        "and",
        persist::VOLATILE,
        1,
        multi::AND,
        multi::AND,
        ev(tevent::GLOBAL_SET, 0),
        ev(tevent::GLOBAL_SET, 1),
        act(taction::SET_GLOBAL, 2),
        act(taction::SET_GLOBAL, 3),
    );
    let mut w = world_with(base_campaign(vec![t], vec![]));
    w.tick(&[]);
    assert!(
        !globals(&w)[2] && !globals(&w)[3],
        "neither event true: no fire"
    );
    if let Some(c) = w.campaign_mut() {
        c.globals[0] = true;
    }
    w.tick(&[]);
    assert!(!globals(&w)[2], "only event1 true (AND): must not fire");
    if let Some(c) = w.campaign_mut() {
        c.globals[1] = true;
    }
    w.tick(&[]);
    assert!(
        globals(&w)[2] && globals(&w)[3],
        "both events true: BOTH actions run"
    );
}

/// `MULTI_OR`: either event suffices; with `action_ctrl = AND` (non-LINKED,
/// non-ONLY), both actions still run even though only e1 was true — the
/// reference does not gate actions by which event fired unless
/// `EventControl == MULTI_LINKED`.
#[test]
fn or_event_control_fires_on_either_event_and_runs_both_actions_when_not_linked() {
    let t = trig2(
        "or",
        persist::VOLATILE,
        1,
        multi::OR,
        multi::AND,
        ev(tevent::GLOBAL_SET, 0),
        ev(tevent::GLOBAL_SET, 1), // never set in this test
        act(taction::SET_GLOBAL, 2),
        act(taction::SET_GLOBAL, 3),
    );
    let mut w = world_with(base_campaign(vec![t], vec![]));
    if let Some(c) = w.campaign_mut() {
        c.globals[0] = true;
    }
    w.tick(&[]);
    assert!(globals(&w)[2], "e1 alone satisfies OR: action1 runs");
    assert!(
        globals(&w)[3],
        "action_ctrl=AND (non-LINKED): action2 ALSO runs even though only e1 fired"
    );
}

/// `MULTI_OR` with `action_ctrl = ONLY`: only action1 ever runs.
#[test]
fn or_event_control_with_action_only_runs_just_action1() {
    let t = trig2(
        "or_only",
        persist::VOLATILE,
        1,
        multi::OR,
        multi::ONLY,
        ev(tevent::GLOBAL_SET, 0),
        ev(tevent::GLOBAL_SET, 1),
        act(taction::SET_GLOBAL, 2),
        act(taction::SET_GLOBAL, 3),
    );
    let mut w = world_with(base_campaign(vec![t], vec![]));
    if let Some(c) = w.campaign_mut() {
        c.globals[1] = true; // satisfy via e2 this time
    }
    w.tick(&[]);
    assert!(globals(&w)[2], "action1 runs");
    assert!(!globals(&w)[3], "action_ctrl=ONLY: action2 must never run");
}

/// `MULTI_LINKED` (non-forced): action1 runs iff e1 fired, action2 runs iff
/// e2 fired — independent per-event action mapping, unlike AND/OR
/// (`trigger.cpp:301-308`).
#[test]
fn linked_maps_each_event_independently_to_its_own_action() {
    let t = trig2(
        "linked",
        persist::PERSISTANT, // persistent so we can observe e1-only and e2-only ticks separately
        1,
        multi::LINKED,
        multi::AND, // action_ctrl is irrelevant for LINKED (non-forced) per trigger.cpp
        ev(tevent::GLOBAL_SET, 0),
        ev(tevent::GLOBAL_SET, 1),
        act(taction::SET_GLOBAL, 2),
        act(taction::SET_GLOBAL, 3),
    );
    let mut w = world_with(base_campaign(vec![t], vec![]));
    if let Some(c) = w.campaign_mut() {
        c.globals[0] = true; // only e1
    }
    w.tick(&[]);
    assert!(globals(&w)[2], "e1 true: action1 (LINKED to e1) runs");
    assert!(
        !globals(&w)[3],
        "e2 false: action2 must NOT run under LINKED"
    );
}

/// KNOWN BUG (report): forced `LINKED` triggers. Reference `Spring(...,
/// forced=true)` (`trigger.cpp:239-243` sets `cell = Class->Cell` and SKIPS
/// the `EventControl` switch entirely — `e2` is left at its default `false`
/// and never evaluated) then, in the action-selection block
/// (`trigger.cpp:301-306`):
/// ```
/// if (Class->EventControl == MULTI_LINKED) {
///     if (e1 || forced) ok |= Action1(...);      // always true when forced
///     if (e2 && !forced) ok |= Action2(...);      // always FALSE when forced
/// }
/// ```
/// i.e. a forced LINKED trigger must run ONLY action1, regardless of
/// `action_ctrl`. Our port's `maybe_spring` (`ra-sim/src/world.rs`) computes
/// `run_a1/run_a2` as `if !forced && ectrl==LINKED {(e1,e2)} else {(true,
/// actctrl != ONLY)}` — the `else` arm is also taken when `forced==true`, so
/// a forced LINKED trigger with `action_ctrl != ONLY` incorrectly ALSO runs
/// action2. This test encodes the reference-correct expectation and is
/// expected to currently FAIL.
#[test]
fn forced_linked_trigger_should_run_only_action1() {
    let target = trig2(
        "target",
        persist::VOLATILE,
        1,
        multi::LINKED,
        multi::AND, // deliberately NOT `ONLY`, to surface the bug
        ev(tevent::NONE, 0),
        ev(tevent::NONE, 0),
        act(taction::SET_GLOBAL, 2),
        act(taction::SET_GLOBAL, 3),
    );
    let forcer = trig1(
        "forcer",
        persist::VOLATILE,
        1,
        ev(tevent::TIME, 0),
        act_trig(taction::FORCE_TRIGGER, 1),
    );
    let mut w = world_with(base_campaign(vec![forcer, target], vec![]));
    w.tick(&[]);
    assert!(globals(&w)[2], "forced LINKED: action1 must run");
    assert!(
        !globals(&w)[3],
        "reference: forced LINKED must NOT run action2 even when action_ctrl != ONLY"
    );
}

// ===========================================================================
// §5 Determinism / hash-gating
// ===========================================================================

/// Flipping a global must change the world hash (trigger state is
/// hash-relevant, per `Campaign::hash_into`).
#[test]
fn flipping_a_global_changes_the_state_hash() {
    let t = trig1(
        "noop",
        persist::PERSISTANT,
        1,
        ev(tevent::NONE, 0),
        act(taction::NONE, -1),
    );
    let mut w = world_with(base_campaign(vec![t], vec![]));
    let h0 = w.state_hash();
    if let Some(c) = w.campaign_mut() {
        c.globals[0] = true;
    }
    let h1 = w.state_hash();
    assert_ne!(h0, h1, "a global flip must change the state hash");
}

/// Same script run twice (with reinforcements + a CREATE_TEAM recruit in the
/// chain) must produce byte-identical hash chains.
#[test]
fn same_script_twice_with_reinforcements_and_teams_is_deterministic() {
    let run = || -> Vec<u64> {
        let team = TeamType {
            name: "rf".into(),
            house: 2,
            flags: 0,
            recruit: 0,
            init_num: 1,
            max_allowed: 1,
            origin: 10,
            trigger: -1,
            classes: vec![TeamClass {
                proto: Some(simple_proto()),
                count: 2,
            }],
            missions: vec![TeamMission {
                code: tmission::MOVE,
                arg: 11,
            }],
        };
        let spawn_t = trig1(
            "spawn",
            persist::VOLATILE,
            1,
            ev(tevent::TIME, 0),
            act_team(taction::REINFORCEMENTS, 0),
        );
        let recruit_t = trig1(
            "recruit",
            persist::VOLATILE,
            1,
            ev(tevent::TIME, 1),
            act_team(taction::CREATE_TEAM, 0),
        );
        let mut camp = base_campaign(vec![spawn_t, recruit_t], vec![team]);
        camp.waypoints[10] = CellCoord::new(12, 12).to_index().unwrap() as i32;
        camp.waypoints[11] = CellCoord::new(20, 20).to_index().unwrap() as i32;
        let mut w = world_with(camp);
        let mut hashes = Vec::new();
        for _ in 0..10 {
            hashes.push(w.tick(&[]));
        }
        hashes
    };
    assert_eq!(run(), run(), "same script twice must hash-match exactly");
}

/// Campaign world vs. skirmish (no campaign) world hash-gating. `World` has
/// no public API to detach a `Campaign` once attached (by design — a
/// skirmish world simply never calls [`World::set_campaign`]), so this test
/// pins the two halves it CAN exercise from outside the crate:
///
/// 1. Attaching a campaign changes the hash (bytes ARE added) — even an
///    inert single-trigger campaign, proving the gate is presence-based.
/// 2. Two independently-built plain (never-campaigned) worlds, constructed
///    identically, hash identically — the skirmish path is unaffected by
///    the campaign machinery merely existing in the same binary.
///
/// The other half of the claim — that a `None` campaign contributes
/// **zero** bytes — is a direct read of `World::state_hash`
/// (`ra-sim/src/world.rs`): `if let Some(c) = &self.campaign { c.hash_into(&mut
/// h); }` is the ONLY call site, so `None` cannot emit anything. The
/// regression sweep (running every pre-existing skirmish/combat golden,
/// which never touch `Campaign`, unchanged post-M7.5-A) is the empirical
/// confirmation of that half.
#[test]
fn skirmish_worlds_carry_zero_campaign_hash_bytes() {
    let build_plain = || -> u64 {
        let mut w = World::new(Passability::all_passable(), 0xC0FF_EE01);
        w.set_catalog(catalog());
        w.init_houses(8, 0);
        w.set_player_house(1);
        w.state_hash()
    };
    let h_plain_a = build_plain();
    let h_plain_b = build_plain();
    assert_eq!(
        h_plain_a, h_plain_b,
        "two identically-built skirmish worlds must hash identically"
    );

    let t = trig1(
        "noop",
        persist::PERSISTANT,
        1,
        ev(tevent::NONE, 0),
        act(taction::NONE, -1),
    );
    let with_campaign = world_with(base_campaign(vec![t], vec![]));
    let h_with = with_campaign.state_hash();
    assert_ne!(
        h_plain_a, h_with,
        "attaching even an inert campaign must change the hash (it adds bytes)"
    );
}
