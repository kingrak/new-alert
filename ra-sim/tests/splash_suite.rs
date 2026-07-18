//! M7 item 2 — splash/area damage suite (ra-tester charter). `explosion_damage`
//! (`ra-sim/src/world.rs`, port of `Explosion_Damage`, `combat.cpp:162`) is
//! exercised end-to-end through `World`'s public API (`tick`/`Command::Attack`),
//! not by calling the function directly, so these tests also validate the
//! `run_bullets` → `explosion_damage` wiring and the `fire`/scatter interaction
//! at the boundary (accurate unit/building shots draw no RNG; only an AP shot
//! force-fired at a ground cell scatters, `fire`'s `inaccurate` branch).
//!
//! Every damage number here is hand-derived from [`crate::combat::modify_damage`]
//! (docented at `ra-sim/src/combat.rs`): `mod = (base*Verses[armor]+32768)/65536`
//! (`Verses=` as a raw 16.16 fixed, truncating `pct5` below matches the
//! production `ra-data` loader's own truncating conversion), then
//! `falloff = clamp(raw_distance / (Spread*5), 0, 16)`, `damage = mod / falloff`
//! (skipped when `falloff == 0`, i.e. inside one Spread-scaled "cell" of the
//! blast centre — the original's point-blank zone), floored to `MinDamage` when
//! `falloff < 4`, capped at `MaxDamage`. `raw_distance` itself is the octagonal
//! `leptons_distance` approximation (`coords.rs`): `max+min/2`, not Euclidean.
//!
//! Uses its own minimal fixture catalog/weapon builders, independent of
//! `world.rs`'s colocated tests and of `building_combat_economy_edges.rs`'s
//! (per the house convention of not reaching into another file's private test
//! module). The colocated tests in `world.rs` already cover the *basic*
//! single-bystander force-fire case, idle-unit retaliation, and
//! order-not-hijacked — not duplicated here; this file adds the harder edges:
//! multiple bystanders at independently hand-computed distances, the
//! armor-matrix cross-section of a single blast, source self-exclusion,
//! ally (non-source) friendly fire, the building direct-hit rule, and the
//! "does a bystander caught in someone else's blast still retaliate" wrinkle.

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    Command, Handle, MoveStats, Passability, Target, WarheadProfile, WeaponProfile, World,
    WorldCoord,
};

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 40,
        rot: 10,
    }
}

fn world() -> World {
    World::new(Passability::all_passable(), 0xBEEF_CAFE)
}

/// Truncating percent -> raw 16.16 fixed, matching `ra-data`'s `Verses=` loader
/// (and every other test fixture in this repo that hand-builds a
/// `WarheadProfile`, e.g. `ra-sim/src/world.rs`'s colocated `pct5`).
fn pct5(p: [i32; 5]) -> [i32; 5] {
    let mut o = [0i32; 5];
    for (d, v) in o.iter_mut().zip(p) {
        *d = v * 65536 / 100;
    }
    o
}

/// An AP-warhead weapon shaped like the real 90mm (`Damage=30, Spread=3,
/// Verses=30%,75%,75%,100%,50%`, same table `damage_matrix.rs` validates
/// against real `rules.ini`), but `instant=true` so a single `Attack` command
/// always resolves within the same tick it's issued — no projectile-flight
/// ticks to count, which keeps every test's tick budget trivial and its
/// timing assertion-free. `range` is generous so approach is never a factor.
fn ap_90mm_instant() -> WeaponProfile {
    WeaponProfile {
        damage: 30,
        rof: 60_000,
        range: 3000,
        proj_speed: 999,
        proj_rot: 0,
        invisible: true,
        instant: true,
        warhead: WarheadProfile {
            spread: 3,
            verses: pct5([30, 75, 75, 100, 50]),
        },
        warhead_ap: true,
        arcing: false,
        ballistic_scatter: 256,
        homing_scatter: 512,
        min_damage: 1,
        max_damage: 1000,
    }
}

/// A non-AP weapon (`warhead_ap=false`): `fire`'s `inaccurate` gate
/// (`warhead.warhead_ap && is_ground`) is false regardless of the target kind,
/// so a force-fire at a `Target::Cell` lands at the cell's exact centre with
/// **no scatter RNG draw** — the deterministic-impact-point tool this suite
/// needs to hand-place bystanders precisely. Same base/spread/verses shape as
/// [`ap_90mm_instant`] otherwise, so the two are damage-comparable.
fn he_ground_instant() -> WeaponProfile {
    let mut w = ap_90mm_instant();
    w.warhead_ap = false;
    w
}

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

/// Spawn a bystander at an exact `WorldCoord` (not merely a cell) — direct
/// `coord` placement (the field is public) is how this suite achieves
/// hand-computable sub-cell distances instead of being limited to the ~256/384
/// -lepton distances that land exactly on cell centres.
fn spawn_bystander_at(w: &mut World, house: u8, coord: WorldCoord, armor: u8, hp: u16) -> Handle {
    let h = w.spawn_unit(0, house, coord.cell(), Facing(0), hp, stats());
    w.set_unit_combat(h, armor, None, false);
    if let Some(u) = w.units.get_mut(h) {
        u.coord = coord;
    }
    h
}

/// Spawn an idle, armed bystander (for the "does a splashed non-target
/// retaliate" case) at an exact `WorldCoord`.
fn spawn_armed_bystander_at(
    w: &mut World,
    house: u8,
    coord: WorldCoord,
    weapon: WeaponProfile,
    hp: u16,
) -> Handle {
    let h = w.spawn_unit(0, house, coord.cell(), Facing(0), hp, stats());
    w.set_unit_combat(h, 0, Some(weapon), true);
    if let Some(u) = w.units.get_mut(h) {
        u.coord = coord;
    }
    h
}

// ===========================================================================
// 1. Multiple bystanders at independently hand-computed distances.
// ===========================================================================

/// One shot, one addressed primary target, three bystanders east of the
/// impact point at hand-picked lepton offsets, all armor class 0 (none) so
/// distance is the only varying input. Impact = the primary target's coord at
/// fire time (`Target::Unit` is always "accurate", `fire`'s `is_ground` is
/// false for it, so no scatter regardless of `warhead_ap` — the primary
/// target itself always takes a `distance == 0` direct hit).
///
/// Derivation (base 30, Verses\[none\]=30% -> raw 30*65536/100=19660,
/// mod=(30*19660+32768)/65536=9, Spread=3 -> falloff divisor = raw_distance/15,
/// clamped 0..16, floored to MinDamage=1 below divisor 4):
/// - offset 10 leptons: divisor 10/15=0 -> falloff skipped entirely (the
///   "inside one Spread-cell" point-blank zone) -> damage stays the *undivided*
///   9 (floor doesn't reduce it: 9 >= 1).
/// - offset 40 leptons: divisor 40/15=2 -> damage = 9/2 = 4 (floor: max(4,1)=4).
/// - offset 100 leptons: divisor 100/15=6 -> damage = 9/6 = 1 (not < 4, no floor
///   change).
/// - offset 300/200 (dx=300,dy=200): true octagonal distance = 300+200/2 = 400,
///   at or past `EXPLOSION_RANGE` (384) -> excluded entirely (0 damage), even
///   though its cell is still within the impact cell's 3x3 neighbourhood
///   (confirms the 384-lepton radius check is a real distance cutoff, not just
///   the coarser cell-neighbourhood prefilter).
#[test]
fn splash_hits_multiple_bystanders_at_independently_hand_computed_distances() {
    let mut w = world();
    let atk = spawn_attacker(
        &mut w,
        1,
        CellCoord::new(10, 10),
        Facing(64),
        ap_90mm_instant(),
    );
    let primary = w.spawn_unit(0, 2, CellCoord::new(11, 10), Facing(0), 400, stats());
    let impact = w.units.get(primary).unwrap().coord;

    let east = |dx: i32, dy: i32| WorldCoord::new(impact.x.raw() + dx, impact.y.raw() + dy);
    let near_pointblank = spawn_bystander_at(&mut w, 3, east(10, 0), 0, 400);
    let mid = spawn_bystander_at(&mut w, 3, east(40, 0), 0, 400);
    let far = spawn_bystander_at(&mut w, 3, east(100, 0), 0, 400);
    let out_of_range = spawn_bystander_at(&mut w, 3, east(300, 200), 0, 400);

    let before = |w: &World, h: Handle| w.units.get(h).unwrap().health;
    let (hp0, hp1, hp2, hp3, hp4) = (
        before(&w, primary),
        before(&w, near_pointblank),
        before(&w, mid),
        before(&w, far),
        before(&w, out_of_range),
    );

    w.tick(&[Command::Attack {
        unit: atk,
        target: Target::Unit(primary),
        house: 1,
    }]);

    assert_eq!(
        hp0 - w.units.get(primary).unwrap().health,
        9,
        "primary target: direct hit (distance 0) takes the undivided armor-modified damage"
    );
    assert_eq!(
        hp1 - w.units.get(near_pointblank).unwrap().health,
        9,
        "10-lepton bystander: inside the point-blank zone, undivided damage"
    );
    assert_eq!(
        hp2 - w.units.get(mid).unwrap().health,
        4,
        "40-lepton bystander: mod 9 / falloff-divisor 2 = 4"
    );
    assert_eq!(
        hp3 - w.units.get(far).unwrap().health,
        1,
        "100-lepton bystander: mod 9 / falloff-divisor 6 = 1"
    );
    assert_eq!(
        hp4,
        w.units.get(out_of_range).unwrap().health,
        "400-lepton bystander is beyond EXPLOSION_RANGE (384): untouched"
    );
}

// ===========================================================================
// 2. Armor-matrix interaction: same blast, same distance, different armor.
// ===========================================================================

/// One shot, five bystanders at the *same* 40-lepton offset (falloff divisor
/// 2, matching the `mid` case above) but each a different armor class —
/// isolates the `Verses=` modifier as the only varying input.
///
/// Derivation (base 30, divisor 2, floor MinDamage=1 below divisor 4 — inert
/// here since every value already clears 1):
/// - none   (30%): mod (30*19660+32768)/65536=9  -> 9/2=4
/// - wood   (75%): mod (30*49152+32768)/65536=23 -> 23/2=11
/// - light  (75%): same modifier as wood -> 23/2=11
/// - heavy  (100%): mod (30*65536+32768)/65536=30 -> 30/2=15
/// - concrete (50%): mod (30*32768+32768)/65536=15 -> 15/2=7
#[test]
fn splash_armor_matrix_same_blast_same_distance_different_armor() {
    let mut w = world();
    let atk = spawn_attacker(
        &mut w,
        1,
        CellCoord::new(10, 10),
        Facing(64),
        ap_90mm_instant(),
    );
    let primary = w.spawn_unit(0, 2, CellCoord::new(11, 10), Facing(0), 400, stats());
    let impact = w.units.get(primary).unwrap().coord;
    let east = |dx: i32| WorldCoord::new(impact.x.raw() + dx, impact.y.raw());

    let armors = [0u8, 1, 2, 3, 4];
    let expected = [4u16, 11, 11, 15, 7];
    let bystanders: Vec<Handle> = armors
        .iter()
        .map(|&a| spawn_bystander_at(&mut w, 3, east(40), a, 400))
        .collect();
    let before: Vec<u16> = bystanders
        .iter()
        .map(|&h| w.units.get(h).unwrap().health)
        .collect();

    w.tick(&[Command::Attack {
        unit: atk,
        target: Target::Unit(primary),
        house: 1,
    }]);

    for (i, &h) in bystanders.iter().enumerate() {
        let after = w.units.get(h).unwrap().health;
        assert_eq!(
            before[i] - after,
            expected[i],
            "armor class {} (index {i}) expected {} splash damage",
            armors[i],
            expected[i]
        );
    }
}

// ===========================================================================
// 3. Source exclusion.
// ===========================================================================

/// The firing unit is never damaged by its own blast, even when the shot
/// lands close enough that the attacker's own cell is well inside the
/// EXPLOSION_RANGE — `explosion_damage`'s `if h == source { continue; }` is an
/// unconditional identity check, not a distance-based exemption.
#[test]
fn splash_never_damages_the_firing_unit_itself() {
    let mut w = world();
    // Attacker fires point-blank at an adjacent enemy: the blast's 3x3
    // neighbourhood and 384-lepton radius both fully cover the attacker's own
    // cell (256 leptons away).
    let atk = spawn_attacker(
        &mut w,
        1,
        CellCoord::new(10, 10),
        Facing(64),
        ap_90mm_instant(),
    );
    let target = w.spawn_unit(0, 2, CellCoord::new(11, 10), Facing(0), 400, stats());
    let atk_hp_before = w.units.get(atk).unwrap().health;

    // Fire several times (rof is huge so in practice this is one shot per
    // tick's worth of ticks anyway; loop just to be robust to timing).
    for _ in 0..5 {
        w.tick(&[Command::Attack {
            unit: atk,
            target: Target::Unit(target),
            house: 1,
        }]);
    }

    assert_eq!(
        w.units.get(atk).unwrap().health,
        atk_hp_before,
        "the firing unit must never take damage from its own blast"
    );
}

// ===========================================================================
// 4. Friendly fire: full, except the exact source.
// ===========================================================================

/// An ally of the firing unit (same house, but a *different* handle) standing
/// at the same distance as an enemy bystander takes **identical** splash
/// damage — only the literal source handle is immune, not the whole house.
/// This matches the original exactly (`object != source`, `combat.cpp:203`,
/// spares only the firer) and is intentionally not softened; see QUIRKS.md Q4.
#[test]
fn splash_is_full_friendly_fire_except_the_exact_source() {
    let mut w = world();
    let atk = spawn_attacker(
        &mut w,
        1,
        CellCoord::new(10, 10),
        Facing(64),
        ap_90mm_instant(),
    );
    let primary = w.spawn_unit(0, 2, CellCoord::new(11, 10), Facing(0), 400, stats());
    let impact = w.units.get(primary).unwrap().coord;
    let east = |dx: i32| WorldCoord::new(impact.x.raw() + dx, impact.y.raw());

    // Same house as the attacker (1) and same house as the enemy bystander
    // (2), both at the identical 40-lepton offset, armor none.
    let ally = spawn_bystander_at(&mut w, 1, east(40), 0, 400);
    let enemy = spawn_bystander_at(&mut w, 2, east(-40), 0, 400);
    let (ally_before, enemy_before) = (
        w.units.get(ally).unwrap().health,
        w.units.get(enemy).unwrap().health,
    );

    w.tick(&[Command::Attack {
        unit: atk,
        target: Target::Unit(primary),
        house: 1,
    }]);

    let ally_dmg = ally_before - w.units.get(ally).unwrap().health;
    let enemy_dmg = enemy_before - w.units.get(enemy).unwrap().health;
    assert_eq!(
        ally_dmg, 4,
        "ally at 40 leptons takes the same splash math as anyone else"
    );
    assert_eq!(
        ally_dmg, enemy_dmg,
        "an ally (not the source) and an enemy at the same distance take identical splash damage"
    );
}

// ===========================================================================
// 5. Building direct-hit rule.
// ===========================================================================

/// A building whose footprint *covers* the impact cell takes a direct hit at
/// distance 0 (`combat.cpp:230`) — even when the impact point is nowhere near
/// its `center_cell()`, i.e. an off-centre hit on a multi-cell building is
/// just as "direct" as a dead-centre one. A second building of the same type
/// that does *not* cover the impact cell, but is near enough to be in range,
/// takes the ordinary distance-falloff damage instead — dramatically less.
///
/// Force-fired (`Target::Cell`) with [`he_ground_instant`] (non-AP: no scatter)
/// so the impact point is exactly the target cell's centre, deterministically.
///
/// Setup: `building1` is a 3x3 at cell (20,20)-(22,22) (centre (21,21));
/// impact cell = (20,20), the building's *corner*, not its centre — covered,
/// so distance 0. `building2` is a 1x1 at cell (20,19), directly north of the
/// impact cell (in the 3x3 neighbourhood, but not covering it): centre
/// (20,19).center(); `leptons_distance(impact, that centre)` = dy(256) +
/// dx(0)/2 = 256.
///
/// Derivation (base 30, armor heavy/100%, mod = 30, Spread 3):
/// - building1 (direct hit, distance 0): falloff skipped -> 30 (full).
/// - building2 (distance 256): falloff divisor 256/15=17->clamp 16 -> 30/16=1.
#[test]
fn building_covering_impact_cell_takes_direct_hit_regardless_of_center_distance() {
    let mut w = world();
    w.set_catalog(splash_building_catalog());
    let atk = spawn_attacker(
        &mut w,
        1,
        CellCoord::new(20, 17),
        Facing(128),
        he_ground_instant(),
    );
    let b1 = w.spawn_building(0, 2, CellCoord::new(20, 20)).unwrap();
    let b2 = w.spawn_building(1, 2, CellCoord::new(20, 19)).unwrap();
    let (hp1_before, hp2_before) = (
        w.buildings.get(b1).unwrap().health,
        w.buildings.get(b2).unwrap().health,
    );

    w.tick(&[Command::Attack {
        unit: atk,
        target: Target::Cell(CellCoord::new(20, 20)),
        house: 1,
    }]);

    let dmg1 = hp1_before - w.buildings.get(b1).unwrap().health;
    let dmg2 = hp2_before - w.buildings.get(b2).unwrap().health;
    assert_eq!(
        dmg1, 30,
        "the building covering the impact cell must take the full undivided direct hit"
    );
    assert_eq!(
        dmg2, 1,
        "the nearby non-covering building only gets the ordinary distance-falloff damage"
    );
    assert!(
        dmg1 > dmg2,
        "direct hit must clearly outweigh a mere-nearby hit on the same building type"
    );
}

/// Minimal catalog for the building direct-hit test: type 0 is a 3x3 (the
/// off-centre-hit building, `b1`), type 1 is a 1x1 (the nearby-but-not-covering
/// comparison building, `b2`) — both armor heavy (index 3, matches
/// `ap_90mm_instant`/`he_ground_instant`'s `Verses[3]=100%`) and otherwise
/// identical, so the only variable between them is footprint coverage of the
/// impact cell.
fn splash_building_catalog() -> ra_sim::Catalog {
    use ra_sim::{BuildingProto, EconRules};
    let proto = |w: u8, h: u8| BuildingProto {
        is_barracks: false,
        name: "PAD".to_string(),
        foot_w: w,
        foot_h: h,
        max_health: 500,
        armor: 3,
        power: 0,
        cost: 10,
        prereq: vec![],
        is_refinery: false,
        is_construction_yard: false,
        is_war_factory: false,
        free_harvester_unit: None,
        sight: 1,
        sprite_id: 0,
    };
    ra_sim::Catalog {
        buildings: vec![proto(3, 3), proto(1, 1)],
        units: vec![],
        econ: EconRules::default(),
    }
}

// ===========================================================================
// 6. Splash-survivor retaliation against the shooter.
// ===========================================================================

/// **Behavior pinned, flagged for ra-coder's confirmation.** `explosion_damage`
/// calls `assign_retaliation` for *every* surviving unit hit by a blast whose
/// house differs from the source's — not only the unit the shot was actually
/// addressed to (`Target::Unit`/`Target::Building`/`Target::Cell`). So an idle,
/// armed bystander who merely happens to be caught in someone else's splash
/// (never the addressed target) turns and targets the shooter too, exactly as
/// if it had been the primary target. This is a real, observable consequence
/// of splash + retaliation interacting (`source_unit` threading, item 2+3 in
/// the M7 charter) — pinned here as current behavior. Whether "anyone winged
/// by a blast joins the fight" is intended (vs. "only the addressed target
/// retaliates") is a design call for ra-coder; flagging it explicitly rather
/// than silently asserting it as obviously correct.
///
/// (The addressed-target-retaliates case is already covered by
/// `world.rs`'s colocated `idle_unit_retaliates_against_its_attacker` — not
/// duplicated here.)
#[test]
fn splash_wakes_a_non_addressed_bystander_to_retaliate_against_the_shooter() {
    let mut w = world();
    let atk = spawn_attacker(
        &mut w,
        1,
        CellCoord::new(10, 10),
        Facing(64),
        ap_90mm_instant(),
    );
    let primary = w.spawn_unit(0, 2, CellCoord::new(11, 10), Facing(0), 400, stats());
    let impact = w.units.get(primary).unwrap().coord;
    let east = |dx: i32| WorldCoord::new(impact.x.raw() + dx, impact.y.raw());

    // Idle, armed bystander at 40 leptons (a non-lethal splash hit, per the
    // armor-matrix derivation above: 4 damage against armor none), never the
    // addressed target.
    let bystander = spawn_armed_bystander_at(&mut w, 3, east(40), ap_90mm_instant(), 400);
    assert!(
        w.units.get(bystander).unwrap().target.is_none(),
        "sanity: bystander starts idle"
    );

    w.tick(&[Command::Attack {
        unit: atk,
        target: Target::Unit(primary),
        house: 1,
    }]);

    assert_eq!(
        w.units.get(bystander).unwrap().target,
        Some(Target::Unit(atk)),
        "a surviving bystander caught in someone else's blast retaliates against the shooter, \
         even though it was never the addressed target"
    );
}
