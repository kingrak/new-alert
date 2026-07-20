//! Audit coverage (ra-tester, post-M7.5-B): exhaustive-boundary depth tests for
//! the per-unit mission layer (Q18 P0, `ra-sim/src/world.rs`/`unit.rs`) that the
//! coder's `mission_transport_suite.rs` smoke tests deliberately leave to us.
//!
//! Layout:
//! §1 skirmish-vs-campaign scoping (both directions, pinned exactly)
//! §2 Guard leash: exact drop boundary (in-range vs one-lepton-out)
//! §3 Area Guard: 2x-range acquire, chase, and the exact race-home boundary
//! §4 Hunt: seeks an enemy anywhere on the map
//! §5 Sleep/Sticky: no acquire AND no retaliate, both pinned
//! §6 Base-under-attack alert: the exact 4-cell boundary, and the alert leash
//!    (an alerted guard does not abandon its post to chase across the map)

use ra_sim::campaign::Campaign;
use ra_sim::coords::{leptons_distance, CellCoord, Facing, Lepton, WorldCoord};
use ra_sim::{
    Command, Mission, MoveStats, Passability, Target, WarheadProfile, WeaponProfile, World,
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

/// A short-range instant, non-AP weapon (so `impact == target_coord` exactly,
/// with no ballistic scatter) — three cells (768 leptons) of range, matching
/// the coder's smoke-test fixture.
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

/// A short (1-cell) weapon, used where a unit must be armed (eligible for
/// guard/alert logic) but must NOT be able to organically acquire a distant
/// target on its own — isolating whichever mechanism is under test.
fn short_gun() -> WeaponProfile {
    WeaponProfile {
        range: 256,
        ..gun()
    }
}

fn empty_campaign() -> Campaign {
    Campaign {
        triggers: Vec::new(),
        teamtypes: Vec::new(),
        waypoints: vec![-1; 101],
        globals: vec![false; 64],
        cell_triggers: Vec::new(),
        state: Vec::new(),
        started: true,
        mission_timer: None,
        evac_cells: Vec::new(),
        civ_evacuated: vec![false; 16],
        reveal_all: false,
        reveal_cells: Vec::new(),
        pending_texts: Vec::new(),
        pending_speech: Vec::new(),
    }
}

fn campaign_world() -> World {
    let mut w = World::new(Passability::all_passable(), 0x0BAD_F00D);
    w.set_campaign(empty_campaign());
    w
}

fn skirmish_world() -> World {
    World::new(Passability::all_passable(), 0x5C1A_1234)
}

// ===========================================================================
// §1 — universal guard acquisition (M7.11 — the Q18 "campaign only" gate is
// REMOVED). Proactive Guard acquisition and the base-alert now run in ALL
// worlds, matching the original (`Enter_Idle_Mode` → MISSION_GUARD is universal,
// `unit.cpp:1343`). These pins previously encoded the OLD skirmish-scoped
// behaviour; they now pin the NEW universal behaviour: a skirmish world's idle
// armed unit near an enemy MUST auto-acquire, exactly as a campaign one does.
// The M7.11 milestone retunes the skirmish AI (see `ai.rs`) to stay decisive
// with active defenders rather than by suppressing acquisition.
// ===========================================================================

#[test]
fn skirmish_world_idle_armed_unit_auto_acquires_a_nearby_enemy() {
    // M7.11: FLIPPED from the M7.5-B `..._never_auto_acquires_...` pin. Guard
    // acquisition is now universal, so a skirmish world behaves exactly like the
    // campaign one below — this is the whole point of the milestone (the playtest
    // complaint "AI players still don't do active fight" in skirmish).
    let mut w = skirmish_world();
    let guard = w.spawn_unit(0, 1, CellCoord::new(10, 10), Facing(0), 400, stats());
    w.set_unit_combat(guard, 0, Some(gun()), true);
    // Default mission is Guard — no explicit `set_unit_mission` call needed,
    // exercising the actual skirmish/produced-unit spawn default.
    let enemy = w.spawn_unit(0, 2, CellCoord::new(11, 10), Facing(0), 400, stats());
    w.set_unit_combat(enemy, 0, Some(gun()), true);
    w.set_unit_mission(enemy, Mission::Sleep); // keep the enemy passive too

    let mut acquired = false;
    for _ in 0..40 {
        w.tick(&[]);
        if w.units.get(guard).unwrap().target.is_some() {
            acquired = true;
        }
        if w.units.get(enemy).unwrap().health < 400 {
            break;
        }
    }
    assert!(
        acquired,
        "M7.11: a skirmish Guard unit MUST proactively acquire a nearby enemy \
         (the campaign-only gate is gone; acquisition is now universal)"
    );
    assert!(
        w.units.get(enemy).unwrap().health < 400,
        "acquisition must lead to an actual engagement even in skirmish"
    );
}

#[test]
fn campaign_world_identical_geometry_does_auto_acquire() {
    let mut w = campaign_world();
    let guard = w.spawn_unit(0, 1, CellCoord::new(10, 10), Facing(0), 400, stats());
    w.set_unit_combat(guard, 0, Some(gun()), true);
    let enemy = w.spawn_unit(0, 2, CellCoord::new(11, 10), Facing(0), 400, stats());
    w.set_unit_combat(enemy, 0, Some(gun()), true);
    w.set_unit_mission(enemy, Mission::Sleep);

    let mut acquired = false;
    for _ in 0..40 {
        w.tick(&[]);
        if w.units.get(guard).unwrap().target.is_some() {
            acquired = true;
        }
        if w.units.get(enemy).unwrap().health < 400 {
            break;
        }
    }
    assert!(
        acquired,
        "a campaign world must auto-acquire (M7.11: now the SAME code path as \
         the skirmish case above — the campaign/skirmish distinction is gone)"
    );
    assert!(
        w.units.get(enemy).unwrap().health < 400,
        "acquisition must lead to an actual engagement"
    );
}

/// `guard_target` — the flag that gates the leash — is now set in skirmish
/// worlds too (M7.11: universal guard acquisition). This FLIPS the M7.5-B
/// `..._never_becomes_true` pin: a skirmish Guard unit that proactively
/// acquires flags its auto-target `guard_target=true` (so the leash applies),
/// exactly as a campaign one does.
#[test]
fn skirmish_world_guard_target_flag_becomes_true_on_proactive_acquire() {
    let mut w = skirmish_world();
    let guard = w.spawn_unit(0, 1, CellCoord::new(10, 10), Facing(0), 400, stats());
    w.set_unit_combat(guard, 0, Some(gun()), true);
    let enemy = w.spawn_unit(0, 2, CellCoord::new(11, 10), Facing(0), 400, stats());
    w.set_unit_combat(enemy, 0, Some(gun()), true);
    w.set_unit_mission(enemy, Mission::Sleep); // passive: only proactive acquire can set the flag

    let mut flagged = false;
    for _ in 0..40 {
        w.tick(&[]);
        if w.units.get(guard).unwrap().guard_target {
            flagged = true;
            break;
        }
    }
    assert!(
        flagged,
        "M7.11: a skirmish Guard unit's proactively-acquired target must be \
         flagged `guard_target` (leashed), just like in campaign"
    );
}

/// M7.11 healer carve-out — `maybe_acquire_guard_target` and
/// `alert_nearby_guards` exclude any unit whose weapon deals non-positive
/// ("healer") damage from proactive guard acquisition. Without this
/// carve-out, universal guard acquisition (this milestone) would have an
/// idle, default-Guard medic pick the nearest ENEMY as its guard target and
/// then fire its heal weapon at it — healing the enemy. This is pinned
/// directly against `maybe_acquire_guard_target`, isolated from
/// `maybe_acquire_heal_target`'s own separate ally/infantry filter (which
/// only gates the *heal-scan* path, not this one): the enemy here is fully
/// armed, well within both weapons' range, and left passive (Sleep) so the
/// only mechanism that could ever move the medic's target is the guard-
/// acquisition path under test.
#[test]
fn medic_never_guard_acquires_an_enemy_even_when_idle_beside_one() {
    let mut w = skirmish_world();
    let medic = w.spawn_unit(0, 1, CellCoord::new(10, 10), Facing(0), 45, stats());
    let heal_weapon = WeaponProfile {
        damage: -50,
        rof: 80,
        range: 3 * 256, // comparable reach to `gun()`, so range is never the reason it fails to acquire
        proj_speed: 255,
        proj_rot: 0,
        invisible: true,
        instant: true,
        warhead: WarheadProfile {
            spread: 0,
            verses: pct5([100, 0, 0, 0, 0]),
        },
        warhead_ap: false,
        arcing: false,
        ballistic_scatter: 256,
        homing_scatter: 512,
        min_damage: 1,
        max_damage: 1000,
    };
    w.set_unit_combat(medic, 0, Some(heal_weapon), true);
    // Default mission is Guard — the exact scenario the carve-out exists for.
    let enemy = w.spawn_unit(0, 2, CellCoord::new(11, 10), Facing(0), 400, stats());
    w.set_unit_combat(enemy, 0, Some(gun()), true);
    w.set_unit_mission(enemy, Mission::Sleep); // keep the enemy passive too

    for _ in 0..200 {
        w.tick(&[]);
    }
    assert!(
        w.units.get(medic).unwrap().target.is_none(),
        "M7.11 healer carve-out: an idle Guard medic must NEVER proactively \
         guard-acquire a nearby enemy, no matter how long it waits"
    );
    assert!(
        !w.units.get(medic).unwrap().guard_target,
        "a medic that never acquires must never have guard_target flagged either"
    );
    assert_eq!(
        w.units.get(enemy).unwrap().health,
        400,
        "no heal (or anything else) was ever fired at the enemy"
    );
}

/// M7.11 audit backfill: the literal user-facing scenario the milestone was
/// driven by ("AI players still don't do active fight" — a **player** tank
/// driving past a skirmish AI base's defenders used to take a free pass,
/// since the M7.5-B guard layer was campaign-only). Neither unit here has an
/// `AiPlayer` — both are plain skirmish units, so this exercises exactly what
/// a human player controls, not AI-vs-AI. The defender is idle (default
/// Guard) at a fixed post; the "player" unit is only ever given a `Move`
/// order (never `Attack`), simulating driving past without engaging. The
/// defender must acquire and land a hit as soon as the player unit enters its
/// weapon range — engaging first, since the player unit never fires at all.
#[test]
fn a_moving_player_unit_driving_past_a_skirmish_defender_gets_engaged_first() {
    let mut w = skirmish_world();
    let defender = w.spawn_unit(0, 1, CellCoord::new(20, 10), Facing(0), 400, stats());
    w.set_unit_combat(defender, 0, Some(gun()), true);
    // Default mission is Guard — a defender sitting at its post, exactly as a
    // produced/placed skirmish unit would be.

    // The "player" unit starts well outside the defender's 3-cell weapon
    // range and is walked straight through it via an ordinary Move order —
    // never an Attack order, so it can never be the one to fire first.
    let player = w.spawn_unit(0, 2, CellCoord::new(0, 10), Facing(0), 400, stats());
    w.set_unit_combat(player, 0, Some(gun()), true);
    w.tick(&[Command::Move {
        unit: player,
        dest: CellCoord::new(40, 10),
        house: 2,
    }]);

    let mut engaged_tick = None;
    for t in 0..200 {
        w.tick(&[]);
        if w.units.get(player).unwrap().health < 400 {
            engaged_tick = Some(t);
            break;
        }
    }
    assert!(
        engaged_tick.is_some(),
        "M7.11: a skirmish defender must engage a player unit that merely \
         drives within weapon range, with no Attack order from either side"
    );
    assert_eq!(
        w.units.get(defender).unwrap().health,
        400,
        "the defender must land the first hit — the player unit, which was \
         only ever Move-ordered, can never have fired back yet"
    );
}

// ===========================================================================
// §2 — Guard leash: the exact drop boundary. `In_Range` uses `dist <=
// weapon.range`; one lepton beyond must drop on the very next combat pass,
// with the unit never leaving its post (no path, cell unchanged).
// ===========================================================================

#[test]
fn guard_leash_drops_exactly_one_lepton_past_weapon_range_not_a_tick_late() {
    let mut w = campaign_world();
    let guard = w.spawn_unit(0, 1, CellCoord::new(10, 10), Facing(0), 400, stats());
    w.set_unit_combat(guard, 0, Some(gun()), true);
    w.set_unit_mission(guard, Mission::Guard);
    // Same y (dy=0) so `leptons_distance` reduces to plain |dx| — no octagonal
    // rounding to account for. dx = 3 cells = 768 leptons = weapon.range exactly.
    let enemy = w.spawn_unit(0, 2, CellCoord::new(13, 10), Facing(0), 4000, stats());
    w.set_unit_combat(enemy, 0, Some(gun()), true);
    w.set_unit_mission(enemy, Mission::Sleep);

    let guard_coord = w.units.get(guard).unwrap().coord;
    let enemy_coord = w.units.get(enemy).unwrap().coord;
    assert_eq!(
        leptons_distance(guard_coord, enemy_coord),
        gun().range,
        "fixture sanity: spawned exactly at the weapon-range boundary"
    );

    w.tick(&[]);
    assert_eq!(
        w.units.get(guard).unwrap().target,
        Some(Target::Unit(enemy)),
        "at exactly weapon range (`dist <= range`), the guard must acquire \
         and stay engaged — the boundary is inclusive"
    );

    // Nudge the enemy exactly one lepton further out (769 leptons — one past
    // the boundary) and tick once more.
    {
        let u = w.units.get_mut(enemy).unwrap();
        u.coord = WorldCoord {
            x: Lepton(u.coord.x.0 + 1),
            y: u.coord.y,
        };
    }
    assert_eq!(
        leptons_distance(
            w.units.get(guard).unwrap().coord,
            w.units.get(enemy).unwrap().coord
        ),
        gun().range + 1,
        "fixture sanity: now exactly one lepton past range"
    );
    w.tick(&[]);
    let g = w.units.get(guard).unwrap();
    assert!(
        g.target.is_none(),
        "one lepton past weapon range, plain Guard must drop the target on \
         the very next combat pass — no grace tick"
    );
    assert!(!g.guard_target);
    assert!(
        g.path.is_empty(),
        "plain Guard never chases — it must not path toward the dropped target"
    );
    assert_eq!(
        g.cell(),
        CellCoord::new(10, 10),
        "the guard must not have moved at all"
    );
}

// ===========================================================================
// §3 — Area Guard: acquires within 2x weapon range of its POST (not its
// current position), chases, and races home the instant it strays more than
// weapon range from the post — pinned at the exact boundary.
// ===========================================================================

#[test]
fn area_guard_acquires_within_double_range_of_post_and_chases() {
    let mut w = campaign_world();
    let guard = w.spawn_unit(0, 1, CellCoord::new(10, 10), Facing(0), 400, stats());
    w.set_unit_combat(guard, 0, Some(gun()), true);
    w.set_unit_mission(guard, Mission::AreaGuard); // records (10,10) as guard_post
    assert_eq!(
        w.units.get(guard).unwrap().guard_post,
        Some(CellCoord::new(10, 10))
    );

    // 5 cells out (1280 leptons): beyond plain weapon range (768) but within
    // 2x weapon range (1536) of the post — Area Guard-only acquire territory.
    let enemy = w.spawn_unit(0, 2, CellCoord::new(15, 10), Facing(0), 4000, stats());
    w.set_unit_combat(enemy, 0, Some(gun()), true);
    w.set_unit_mission(enemy, Mission::Sleep);

    w.tick(&[]);
    assert_eq!(
        w.units.get(guard).unwrap().target,
        Some(Target::Unit(enemy)),
        "Area Guard must acquire at 5 cells (within 2x range of its post), \
         which plain Guard (§2) never would"
    );
    assert!(w.units.get(guard).unwrap().guard_target);

    // It must actually give chase (path toward the target), since it is
    // presently within the weapon-range leash of its post (post == its own
    // coord right now, distance 0).
    for _ in 0..20 {
        w.tick(&[]);
    }
    let g = w.units.get(guard).unwrap();
    assert_ne!(
        g.cell(),
        CellCoord::new(10, 10),
        "Area Guard must actually chase (unlike plain Guard), moving off its post"
    );
}

#[test]
fn area_guard_races_home_exactly_one_lepton_past_weapon_range_of_its_post() {
    let mut w = campaign_world();
    let guard = w.spawn_unit(0, 1, CellCoord::new(10, 10), Facing(0), 400, stats());
    w.set_unit_combat(guard, 0, Some(gun()), true);
    w.set_unit_mission(guard, Mission::AreaGuard);
    let post = w.units.get(guard).unwrap().guard_post.unwrap();
    let post_coord = post.center();

    // A distant, out-of-weapon-range enemy so the leash branch (not a direct
    // in-range engagement) is what's under test.
    let enemy = w.spawn_unit(0, 2, CellCoord::new(80, 80), Facing(0), 4000, stats());
    w.set_unit_combat(enemy, 0, Some(gun()), true);
    w.set_unit_mission(enemy, Mission::Sleep);

    // Manually place the guard exactly at the "strayed" boundary: distance to
    // its OWN post equal to weapon.range (not yet strayed), already chasing.
    {
        let u = w.units.get_mut(guard).unwrap();
        u.coord = WorldCoord {
            x: Lepton(post_coord.x.0 + gun().range),
            y: post_coord.y,
        };
        u.target = Some(Target::Unit(enemy));
        u.guard_target = true;
    }
    assert_eq!(
        leptons_distance(w.units.get(guard).unwrap().coord, post_coord),
        gun().range
    );
    w.tick(&[]);
    let g = w.units.get(guard).unwrap();
    assert_eq!(
        g.target,
        Some(Target::Unit(enemy)),
        "at exactly weapon range from post (not yet '> range'), Area Guard \
         must still be chasing — the race-home boundary is `>`, not `>=`"
    );

    // Now push it one lepton further from the post (strictly beyond) and tick.
    {
        let u = w.units.get_mut(guard).unwrap();
        u.coord = WorldCoord {
            x: Lepton(post_coord.x.0 + gun().range + 1),
            y: post_coord.y,
        };
    }
    w.tick(&[]);
    let g = w.units.get(guard).unwrap();
    assert!(
        g.target.is_none(),
        "one lepton past weapon range from its post, Area Guard must abandon \
         the chase and race home instead of continuing to close on the target"
    );
    assert!(!g.guard_target);
    assert_eq!(
        g.dest,
        Some(post),
        "the abandoned chase must be replaced by a path home to the guard post"
    );
}

// ===========================================================================
// §4 — Hunt: seeks the nearest enemy anywhere on the map, with no acquire
// radius limit (unlike Guard/Area Guard) and no campaign gating.
// ===========================================================================

#[test]
fn hunt_mission_seeks_and_engages_an_enemy_far_across_the_map() {
    // Deliberately a *skirmish* world: Hunt (unlike Guard/Area-Guard) is not
    // campaign-scoped — `maybe_acquire_hunt_target` fires unconditionally.
    let mut w = skirmish_world();
    let hunter = w.spawn_unit(0, 1, CellCoord::new(5, 5), Facing(0), 400, stats());
    w.set_unit_combat(hunter, 0, Some(gun()), true);
    w.set_unit_mission(hunter, Mission::Hunt);
    // Far away — outside both Guard's and Area-Guard's acquire radius, so
    // only Hunt's unlimited-range scan can find it.
    let enemy = w.spawn_unit(0, 2, CellCoord::new(70, 70), Facing(0), 4000, stats());
    w.set_unit_combat(enemy, 0, Some(gun()), true);
    w.set_unit_mission(enemy, Mission::Sleep);

    let mut acquired_early = false;
    let mut engaged = false;
    for i in 0..2000 {
        w.tick(&[]);
        if i == 0 && w.units.get(hunter).unwrap().target.is_some() {
            acquired_early = true;
        }
        if w.units.get(enemy).unwrap().health < 4000 {
            engaged = true;
            break;
        }
    }
    assert!(
        acquired_early,
        "Hunt has no acquire-radius limit — it must target the enemy from \
         tick 1, however far away, unlike Guard/Area-Guard"
    );
    assert!(
        engaged,
        "the Hunt unit must path all the way across the map and actually \
         land a hit on the far enemy"
    );
}

// ===========================================================================
// §5 — Sleep/Sticky: fully inert. Neither auto-acquires NOR retaliates, even
// under repeated fire from an explicit player order (isolating "no retaliate"
// from "no acquire" — this hits the `assign_retaliation` early-return
// directly, not just the absence of proactive scanning).
// ===========================================================================

#[test]
fn sleep_mission_never_acquires_and_never_retaliates_under_repeated_fire() {
    let mut w = campaign_world(); // campaign world: the *hardest* setting to
                                  // stay inert under (proactive guard would
                                  // otherwise be enabled for anyone else)
    let victim = w.spawn_unit(0, 2, CellCoord::new(10, 10), Facing(0), 4000, stats());
    w.set_unit_combat(victim, 0, Some(gun()), true);
    w.set_unit_mission(victim, Mission::Sleep);
    let attacker = w.spawn_unit(0, 1, CellCoord::new(11, 10), Facing(0), 400, stats());
    w.set_unit_combat(attacker, 0, Some(gun()), true);
    w.set_unit_mission(attacker, Mission::Sleep); // isolate: attacker must not
                                                  // auto-acquire either; only
                                                  // the explicit order below fires

    for _ in 0..30 {
        w.tick(&[Command::Attack {
            unit: attacker,
            target: Target::Unit(victim),
            house: 1,
        }]);
        assert!(
            w.units.get(victim).unwrap().target.is_none(),
            "a Sleep unit must never retaliate, even under sustained fire"
        );
    }
    assert!(
        w.units.get(victim).unwrap().health < 4000,
        "fixture sanity: the victim really was being hit"
    );
    assert_eq!(
        w.units.get(attacker).unwrap().health,
        400,
        "since the victim never retaliated, the attacker took no return fire"
    );
}

#[test]
fn sticky_mission_never_acquires_and_never_retaliates_under_repeated_fire() {
    let mut w = campaign_world();
    let victim = w.spawn_unit(0, 2, CellCoord::new(10, 10), Facing(0), 4000, stats());
    w.set_unit_combat(victim, 0, Some(gun()), true);
    w.set_unit_mission(victim, Mission::Sticky);
    let attacker = w.spawn_unit(0, 1, CellCoord::new(11, 10), Facing(0), 400, stats());
    w.set_unit_combat(attacker, 0, Some(gun()), true);
    w.set_unit_mission(attacker, Mission::Sleep);

    for _ in 0..30 {
        w.tick(&[Command::Attack {
            unit: attacker,
            target: Target::Unit(victim),
            house: 1,
        }]);
        assert!(w.units.get(victim).unwrap().target.is_none());
    }
    assert!(w.units.get(victim).unwrap().health < 4000);
}

/// Sleep/Sticky must not *proactively* acquire either — the "no acquire"
/// half, isolated from "no retaliate" (§ above never gives them a chance to
/// acquire since they're never idle-and-in-range of an active shooter; here
/// an enemy sits in range and NEVER shoots, so only proactive scanning could
/// possibly produce a target).
#[test]
fn sleep_and_sticky_never_proactively_acquire_an_idle_enemy_in_range() {
    let mut w = campaign_world();
    let sleeper = w.spawn_unit(0, 1, CellCoord::new(10, 10), Facing(0), 400, stats());
    w.set_unit_combat(sleeper, 0, Some(gun()), true);
    w.set_unit_mission(sleeper, Mission::Sleep);
    let sticky = w.spawn_unit(0, 1, CellCoord::new(20, 20), Facing(0), 400, stats());
    w.set_unit_combat(sticky, 0, Some(gun()), true);
    w.set_unit_mission(sticky, Mission::Sticky);
    let enemy1 = w.spawn_unit(0, 2, CellCoord::new(11, 10), Facing(0), 400, stats());
    w.set_unit_combat(enemy1, 0, Some(gun()), true);
    w.set_unit_mission(enemy1, Mission::Sleep);
    let enemy2 = w.spawn_unit(0, 2, CellCoord::new(21, 20), Facing(0), 400, stats());
    w.set_unit_combat(enemy2, 0, Some(gun()), true);
    w.set_unit_mission(enemy2, Mission::Sleep);

    for _ in 0..60 {
        w.tick(&[]);
    }
    assert!(w.units.get(sleeper).unwrap().target.is_none());
    assert!(w.units.get(sticky).unwrap().target.is_none());
    assert_eq!(w.units.get(enemy1).unwrap().health, 400);
    assert_eq!(w.units.get(enemy2).unwrap().health, 400);
}

// ===========================================================================
// §6 — Base-under-attack alert: the exact 4-cell (Chebyshev) boundary, and
// the alert leash (an alerted guard engages if in its own weapon range but
// does not abandon its post to chase across the map).
// ===========================================================================

/// Builds an attacker(house1)-vs-victim(house2) pair one cell apart, plus two
/// idle house-2 Guard sentries too far from the fight to organically acquire
/// the attacker on their own (their own weapon is short-ranged) — isolating
/// the alert broadcast as the only possible acquisition path. Returns
/// (world, attacker, victim, guard_within_4, guard_beyond_4).
fn alert_boundary_fixture() -> (
    World,
    ra_sim::Handle,
    ra_sim::Handle,
    ra_sim::Handle,
    ra_sim::Handle,
) {
    let mut w = campaign_world();
    let attacker = w.spawn_unit(0, 1, CellCoord::new(10, 10), Facing(0), 400, stats());
    w.set_unit_combat(attacker, 0, Some(gun()), true);
    w.set_unit_mission(attacker, Mission::Sleep); // fires only via explicit order below
    let victim = w.spawn_unit(0, 2, CellCoord::new(11, 10), Facing(0), 4000, stats());
    w.set_unit_combat(victim, 0, Some(gun()), true);
    w.set_unit_mission(victim, Mission::Sleep);

    // Impact cell == victim's cell (11,10) (non-AP weapon, no scatter).
    // guard_in: |15-11|=4, |10-10|=0 -> Chebyshev 4 (must react).
    let guard_in = w.spawn_unit(0, 2, CellCoord::new(15, 10), Facing(0), 400, stats());
    w.set_unit_combat(guard_in, 0, Some(short_gun()), true);
    w.set_unit_mission(guard_in, Mission::Guard);
    // guard_out: |16-11|=5 -> Chebyshev 5 (must NOT react).
    let guard_out = w.spawn_unit(0, 2, CellCoord::new(16, 10), Facing(0), 400, stats());
    w.set_unit_combat(guard_out, 0, Some(short_gun()), true);
    w.set_unit_mission(guard_out, Mission::Guard);

    // Fixture sanity: neither sentry's OWN weapon range (1 cell) can reach the
    // attacker at (10,10) — distance 5 and 6 cells respectively — so any
    // acquisition they show can only come from the alert broadcast, not
    // organic `maybe_acquire_guard_target`.
    assert!(
        leptons_distance(
            w.units.get(guard_in).unwrap().coord,
            w.units.get(attacker).unwrap().coord
        ) > short_gun().range
    );
    assert!(
        leptons_distance(
            w.units.get(guard_out).unwrap().coord,
            w.units.get(attacker).unwrap().coord
        ) > short_gun().range
    );

    (w, attacker, victim, guard_in, guard_out)
}

/// Issue the Attack order once, then tick (facing rotation + rate-of-fire
/// mean the shot does not necessarily land on tick 1) until the victim's
/// health first drops — the tick the impact (and thus the alert broadcast)
/// actually happens. Panics if no hit lands within the budget.
fn fire_until_impact(w: &mut World, attacker: ra_sim::Handle, victim: ra_sim::Handle) {
    let start_hp = w.units.get(victim).unwrap().health;
    w.tick(&[Command::Attack {
        unit: attacker,
        target: Target::Unit(victim),
        house: 1,
    }]);
    for _ in 0..40 {
        if w.units.get(victim).unwrap().health < start_hp {
            return;
        }
        w.tick(&[]);
    }
    assert!(
        w.units.get(victim).unwrap().health < start_hp,
        "fixture sanity: the attacker never landed a hit within budget"
    );
}

#[test]
fn base_alert_wakes_guards_within_4_cells_of_the_impact() {
    let (mut w, attacker, victim, guard_in, _guard_out) = alert_boundary_fixture();
    fire_until_impact(&mut w, attacker, victim);
    assert_eq!(
        w.units.get(guard_in).unwrap().target,
        Some(Target::Unit(attacker)),
        "a guard within 4 cells (Chebyshev) of the impact must wake and \
         target the attacker, even though it is outside the guard's own \
         acquire/sight range"
    );
    assert!(w.units.get(guard_in).unwrap().guard_target);
}

#[test]
fn base_alert_does_not_wake_a_guard_at_5_cells() {
    let (mut w, attacker, victim, _guard_in, guard_out) = alert_boundary_fixture();
    fire_until_impact(&mut w, attacker, victim);
    assert!(
        w.units.get(guard_out).unwrap().target.is_none(),
        "one cell beyond the alert radius (5 cells, Chebyshev), the sentry \
         must show no reaction at all — the boundary is `<= 4`, not `< 5` \
         loosely rounded"
    );
}

/// The alert leash: an alerted guard whose own weapon cannot reach the
/// attacker engages nothing and does not abandon its post — it must NOT path
/// toward the attacker across the map (this is what prevented the scg05ea
/// AI-vs-AI cross-map-chase stall, per the QUIRKS Q18 note).
#[test]
fn alerted_guard_out_of_its_own_weapon_range_does_not_cross_map_chase() {
    let (mut w, attacker, victim, guard_in, _guard_out) = alert_boundary_fixture();
    let post = w.units.get(guard_in).unwrap().cell();
    fire_until_impact(&mut w, attacker, victim);
    assert_eq!(
        w.units.get(guard_in).unwrap().target,
        Some(Target::Unit(attacker)),
        "fixture sanity: it was alerted"
    );
    // Next combat pass: the leash check runs against the guard's own (short)
    // weapon range, well short of the actual distance to the attacker.
    w.tick(&[]);
    let g = w.units.get(guard_in).unwrap();
    assert!(
        g.target.is_none(),
        "out of its own weapon range, the alerted guard must drop the target \
         on the very next pass rather than start closing the distance"
    );
    assert!(g.path.is_empty(), "it must never have started a chase path");
    assert_eq!(g.cell(), post, "it must never have left its post");
}
