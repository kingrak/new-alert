//! Naval depth suite (ra-tester, M7.18 audit). Exhaustive adversarial coverage
//! the coder handed off from the acceptance smoke in `naval_suite.rs`:
//!
//!   * ZERO-RE-PIN audit pins — a surface vessel (and a detector) append **no**
//!     hash bytes; only a submarine's `submerged`/`recloak` do (tag 0x34,
//!     `unit.rs:756`), so every non-submarine world is byte-identical.
//!   * Submarine stealth depth — explicit-attack rejection, the ~5-cell detector
//!     boundary (`SUB_DETECT_RANGE = 0x0500`, `world.rs:4290`), the
//!     surface/recloak-grace/re-submerge FSM (`SUB_RECLOAK_TICKS = 45`,
//!     `vessel.cpp:2044`), area-splash bypassing cloak, two-sub independence, and
//!     same-seed determinism of the stealth FSM.
//!   * Water-locomotor depth — ground never enters water (the mirror of the ship
//!     case in `naval_suite.rs`), one-vessel-per-cell occupancy, and ship/ground
//!     cell-exclusivity.
//!   * Naval combat — a destroyer engaging a submarine and a ground target, a
//!     long-range cruiser bombard, and determinism with vessels + combat.

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    Command, Handle, MoveStats, Passability, Target, WarheadProfile, WeaponProfile, World,
};

const W: i32 = 24;
const H: i32 = 16;

fn ship_stats() -> MoveStats {
    MoveStats {
        max_speed: 60,
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

/// Full-damage instant weapon, `range` in leptons.
fn weapon(range: i32) -> WeaponProfile {
    WeaponProfile {
        damage: 50,
        rof: 20,
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

/// All-water grid (no land) — the open sea.
fn open_sea() -> Passability {
    let n = (W * H) as usize;
    let none = vec![false; n];
    let all = vec![true; n];
    Passability::per_locomotor_water(W, H, none.clone(), none.clone(), none, all)
}

/// Water everywhere except a land strip along the bottom two rows (a shore), so a
/// ground unit has land to stand on and water to (illegally) be ordered onto.
fn shore_grid() -> Passability {
    let n = (W * H) as usize;
    let is_land = |_x: i32, y: i32| y >= H - 2;
    let mut water = vec![false; n];
    let mut ground = vec![false; n];
    for y in 0..H {
        for x in 0..W {
            let i = (y * W + x) as usize;
            let land = is_land(x, y);
            water[i] = !land;
            ground[i] = land;
        }
    }
    Passability::per_locomotor_water(W, H, ground.clone(), ground.clone(), ground, water)
}

fn spawn_vessel(
    w: &mut World,
    house: u8,
    cell: CellCoord,
    wpn: Option<WeaponProfile>,
    is_sub: bool,
    is_det: bool,
) -> Handle {
    let h = w.spawn_unit(0, house, cell, Facing(0), 400, ship_stats());
    w.set_unit_combat(h, 0, wpn, false);
    w.units.get_mut(h).unwrap().make_vessel(is_sub, is_det);
    h
}

// ===========================================================================
// Zero-re-pin audit pins
// ===========================================================================

/// A **surface** vessel folds no naval bytes: a world with a unit turned into a
/// surface vessel (Water locomotor, non-submarine) hashes identically to the same
/// world with that unit left a plain ground vehicle at the same coord. Proves the
/// naval arc added zero hash churn for every non-submarine unit (`unit.rs:752` —
/// the 0x34 block is gated on `is_submarine`, and `locomotor`/`is_detector` are
/// unhashed type constants).
#[test]
fn surface_vessel_and_detector_append_zero_hash_bytes() {
    let cell = CellCoord::new(5, 5);
    let plain = {
        let mut w = World::new(open_sea(), 0xD00D_0000);
        w.init_houses(2, 0);
        w.spawn_unit(0, 0, cell, Facing(0), 400, ship_stats());
        w.state_hash()
    };
    let surface_dd = {
        let mut w = World::new(open_sea(), 0xD00D_0000);
        w.init_houses(2, 0);
        let h = w.spawn_unit(0, 0, cell, Facing(0), 400, ship_stats());
        w.units.get_mut(h).unwrap().make_vessel(false, true); // DD: detector, not sub
        w.state_hash()
    };
    assert_eq!(
        plain, surface_dd,
        "a surface vessel/detector must add zero hash bytes vs a plain vehicle"
    );
}

/// A **submarine** DOES fold bytes (the 0x34 block): flipping `submerged` changes
/// the hash, so the stealth state is deterministically tracked; two subs each
/// contribute independently.
#[test]
fn submarine_state_changes_hash() {
    let cell = CellCoord::new(5, 5);
    let base = {
        let mut w = World::new(open_sea(), 0xD00D_0001);
        w.init_houses(2, 0);
        spawn_vessel(&mut w, 0, cell, None, true, false); // spawns submerged
        w.state_hash()
    };
    let surfaced = {
        let mut w = World::new(open_sea(), 0xD00D_0001);
        w.init_houses(2, 0);
        let h = spawn_vessel(&mut w, 0, cell, None, true, false);
        w.units.get_mut(h).unwrap().submerged = false; // force surfaced
        w.state_hash()
    };
    assert_ne!(
        base, surfaced,
        "a submarine's submerged flag must be folded into the hash"
    );
}

// ===========================================================================
// Submarine stealth depth
// ===========================================================================

/// A submerged enemy submarine cannot be **explicitly** targeted (force-clicked)
/// by a non-detector: `Command::Attack` on it is rejected, the observer's target
/// stays `None` (`world.rs:2150` is_hidden_submarine gate).
#[test]
fn hidden_submarine_rejects_explicit_attack_from_non_detector() {
    let mut w = World::new(open_sea(), 0xD00D_0002);
    w.init_houses(2, 0);
    let sub = spawn_vessel(&mut w, 1, CellCoord::new(10, 8), None, true, false);
    let ship = spawn_vessel(
        &mut w,
        0,
        CellCoord::new(8, 8),
        Some(weapon(0x0800)),
        false,
        false,
    );

    assert!(w.units.get(sub).unwrap().submerged);
    w.tick(&[Command::Attack {
        unit: ship,
        target: Target::Unit(sub),
        house: 0,
    }]);
    assert!(
        w.units.get(ship).unwrap().target.is_none(),
        "an explicit attack on a hidden submarine must be rejected"
    );
    // And it never auto-acquires it either, over time.
    for _ in 0..20 {
        w.tick(&[]);
        assert!(w.units.get(ship).unwrap().target.is_none());
    }
}

/// The detector boundary is ~5 cells (`SUB_DETECT_RANGE = 0x0500`): a destroyer
/// exactly 5 cells away reveals the sub (an allied ship acquires it), but a
/// destroyer 6 cells away does not.
#[test]
fn detector_reveals_within_five_cells_not_beyond() {
    // A submerged enemy sub, and a friendly armed non-detector patrol adjacent to
    // it (so once revealed it acquires within its own weapon range).
    let scene = |dd_dx: i32| -> bool {
        let mut w = World::new(open_sea(), 0xD00D_0003);
        w.init_houses(2, 0);
        let sub = spawn_vessel(&mut w, 1, CellCoord::new(12, 8), None, true, false);
        let patrol = spawn_vessel(
            &mut w,
            0,
            CellCoord::new(11, 8),
            Some(weapon(0x0400)),
            false,
            false,
        );
        // Detector destroyer `dd_dx` cells to the right of the sub.
        let _dd = spawn_vessel(
            &mut w,
            0,
            CellCoord::new(12 + dd_dx, 8),
            Some(weapon(0x0400)),
            false,
            true,
        );
        for _ in 0..10 {
            w.tick(&[]);
            if matches!(w.units.get(patrol).unwrap().target, Some(Target::Unit(t)) if t == sub) {
                return true;
            }
        }
        false
    };
    assert!(
        scene(5),
        "a detector exactly 5 cells (0x0500) away must reveal the submarine"
    );
    assert!(
        !scene(6),
        "a detector 6 cells away is beyond 0x0500 and must NOT reveal the submarine"
    );
}

/// Surface/recloak-grace/re-submerge FSM (`run_submarines`, `vessel.cpp:2044`):
/// a sub with a target surfaces immediately (recloak reset to 45); after it loses
/// the target it stays surfaced through the ~45-tick grace window and then
/// re-submerges.
#[test]
fn submarine_surface_grace_resubmerge_fsm() {
    let mut w = World::new(open_sea(), 0xD00D_0004);
    w.init_houses(2, 0);
    // Armed sub, and a far enemy well OUT of the sub's weapon range so it is never
    // guard-auto-acquired (only an explicit order gives it a target).
    let sub = spawn_vessel(
        &mut w,
        0,
        CellCoord::new(4, 8),
        Some(weapon(0x0300)),
        true,
        false,
    );
    let enemy = spawn_vessel(&mut w, 1, CellCoord::new(20, 8), None, false, false);
    assert!(w.units.get(sub).unwrap().submerged, "sub starts submerged");

    // Explicit attack → surfaces this very tick (FSM runs before combat).
    w.tick(&[Command::Attack {
        unit: sub,
        target: Target::Unit(enemy),
        house: 0,
    }]);
    assert!(
        !w.units.get(sub).unwrap().submerged,
        "a sub with a target must be surfaced"
    );
    assert_eq!(
        w.units.get(sub).unwrap().recloak,
        45,
        "recloak grace resets to SUB_RECLOAK_TICKS while it holds a target"
    );

    // Halt (drop the target). It must stay surfaced through the grace window...
    w.tick(&[Command::Stop {
        unit: sub,
        house: 0,
    }]);
    for t in 0..40 {
        assert!(
            !w.units.get(sub).unwrap().submerged,
            "sub re-submerged too early (tick {t} of grace)"
        );
        w.tick(&[]);
    }
    // ...and re-submerge once the grace expires.
    let mut resubmerged = false;
    for _ in 0..20 {
        if w.units.get(sub).unwrap().submerged {
            resubmerged = true;
            break;
        }
        w.tick(&[]);
    }
    assert!(
        resubmerged,
        "sub never re-submerged after the recloak grace expired"
    );
}

/// Cloak blocks *targeting*, not *area splash*: a submerged sub caught in a
/// force-fire blast still takes damage (`explosion_damage` has no cloak check —
/// matching a depth-charge/area weapon hitting a submerged hull).
#[test]
fn submerged_submarine_still_hit_by_area_splash() {
    let mut w = World::new(open_sea(), 0xD00D_0005);
    w.init_houses(2, 0);
    let sub_cell = CellCoord::new(10, 8);
    let sub = spawn_vessel(&mut w, 1, sub_cell, None, true, false);
    // A non-detector attacker cannot click the sub, but CAN force-fire its cell.
    let ship = spawn_vessel(
        &mut w,
        0,
        CellCoord::new(8, 8),
        Some(weapon(0x0800)),
        false,
        false,
    );
    let hp0 = w.units.get(sub).unwrap().health;
    assert!(w.units.get(sub).unwrap().submerged);

    w.tick(&[Command::Attack {
        unit: ship,
        target: Target::Cell(sub_cell), // force-fire the water the sub hides in
        house: 0,
    }]);
    for _ in 0..30 {
        w.tick(&[]);
        if w.units.get(sub).map(|u| u.health).unwrap_or(0) < hp0 {
            break;
        }
    }
    let hp1 = w.units.get(sub).map(|u| u.health).unwrap_or(0);
    assert!(
        hp1 < hp0,
        "a submerged sub must still take area/splash damage from a force-fire blast"
    );
    assert!(
        w.units.get(sub).map(|u| u.submerged).unwrap_or(true),
        "force-firing the cell must not itself surface the sub (it has no target)"
    );
}

/// Two submarines are independent: engaging one (giving it a target so it
/// surfaces) leaves the other submerged.
#[test]
fn two_submarines_are_independent() {
    let mut w = World::new(open_sea(), 0xD00D_0006);
    w.init_houses(2, 0);
    let a = spawn_vessel(
        &mut w,
        0,
        CellCoord::new(4, 8),
        Some(weapon(0x0300)),
        true,
        false,
    );
    let b = spawn_vessel(&mut w, 0, CellCoord::new(6, 8), None, true, false);
    let enemy = spawn_vessel(&mut w, 1, CellCoord::new(18, 8), None, false, false);

    w.tick(&[Command::Attack {
        unit: a,
        target: Target::Unit(enemy),
        house: 0,
    }]);
    assert!(
        !w.units.get(a).unwrap().submerged,
        "sub A surfaced to engage"
    );
    assert!(
        w.units.get(b).unwrap().submerged,
        "sub B must stay submerged — subs are independent"
    );
}

/// The stealth FSM is deterministic: same seed + same script twice → identical
/// per-tick hash chains (with subs surfacing/re-submerging).
#[test]
fn stealth_fsm_deterministic_same_seed_twice() {
    let run = || -> Vec<u64> {
        let mut w = World::new(open_sea(), 0xD00D_0007);
        w.init_houses(2, 0);
        let sub = spawn_vessel(
            &mut w,
            0,
            CellCoord::new(4, 8),
            Some(weapon(0x0300)),
            true,
            false,
        );
        let enemy = spawn_vessel(&mut w, 1, CellCoord::new(20, 8), None, false, false);
        let mut hs = vec![w.tick(&[Command::Attack {
            unit: sub,
            target: Target::Unit(enemy),
            house: 0,
        }])];
        hs.push(w.tick(&[Command::Stop {
            unit: sub,
            house: 0,
        }]));
        for _ in 0..60 {
            hs.push(w.tick(&[]));
        }
        hs
    };
    assert_eq!(run(), run(), "the stealth FSM diverged on identical replay");
}

// ===========================================================================
// Water-locomotor depth
// ===========================================================================

/// The mirror of `naval_suite`'s ship-never-onto-land: a **ground** unit ordered
/// onto water never moves there (water is impassable to every ground locomotor).
#[test]
fn ground_unit_never_enters_water() {
    let mut w = World::new(shore_grid(), 0xD00D_0008);
    w.init_houses(2, 0);
    // A ground vehicle on the land strip (bottom rows).
    let land_cell = CellCoord::new(10, H - 1);
    let tank = w.spawn_unit(0, 0, land_cell, Facing(0), 400, ship_stats());
    // (default Track locomotor — no make_vessel)
    assert!(!w.passability().is_water(land_cell));

    let water_goal = CellCoord::new(10, 2);
    w.tick(&[Command::Move {
        unit: tank,
        dest: water_goal,
        house: 0,
    }]);
    for _ in 0..60 {
        w.tick(&[]);
        let c = w.units.get(tank).unwrap().cell();
        assert!(
            !w.passability().is_water(c),
            "a ground unit stepped onto water at {c:?}"
        );
    }
}

/// One vessel per cell: two ships ordered to converge never occupy the same cell
/// (vehicle occupancy applies to the Water locomotor).
#[test]
fn one_vessel_per_cell_occupancy() {
    let mut w = World::new(open_sea(), 0xD00D_0009);
    w.init_houses(1, 0);
    let a = spawn_vessel(&mut w, 0, CellCoord::new(6, 8), None, false, false);
    let b = spawn_vessel(&mut w, 0, CellCoord::new(14, 8), None, false, false);
    let meet = CellCoord::new(10, 8);
    w.tick(&[
        Command::Move {
            unit: a,
            dest: meet,
            house: 0,
        },
        Command::Move {
            unit: b,
            dest: meet,
            house: 0,
        },
    ]);
    for _ in 0..120 {
        w.tick(&[]);
        let ca = w.units.get(a).unwrap().cell();
        let cb = w.units.get(b).unwrap().cell();
        assert_ne!(ca, cb, "two vessels occupied the same cell {ca:?}");
    }
}

/// Ship/ground cell-exclusivity: on a shore grid the water cells are passable to
/// a vessel and impassable to ground, and vice versa — no cell admits both kinds.
#[test]
fn water_and_land_cells_are_kind_exclusive() {
    let w = World::new(shore_grid(), 0xD00D_000A);
    let p = w.passability();
    use ra_sim::coords::Locomotor;
    let mut water_cells = 0;
    let mut land_cells = 0;
    for y in 0..H {
        for x in 0..W {
            let c = CellCoord::new(x, y);
            let is_w = p.is_passable_loco(c, Locomotor::Water);
            let is_g = p.is_passable_loco(c, Locomotor::Track);
            assert!(
                !(is_w && is_g),
                "cell {c:?} admits BOTH a vessel and a ground unit"
            );
            if is_w {
                water_cells += 1;
            }
            if is_g {
                land_cells += 1;
            }
        }
    }
    assert!(
        water_cells > 0 && land_cells > 0,
        "grid must have both kinds"
    );
}

// ===========================================================================
// Naval combat
// ===========================================================================

/// A destroyer (detector) destroys a submerged enemy submarine (its own detector
/// capability reveals it) and, separately, a ground target — engaging both
/// classes.
#[test]
fn destroyer_engages_submarine_and_ground_target() {
    // vs submarine
    {
        let mut w = World::new(open_sea(), 0xD00D_000B);
        w.init_houses(2, 0);
        let sub = spawn_vessel(&mut w, 1, CellCoord::new(11, 8), None, true, false);
        let _dd = spawn_vessel(
            &mut w,
            0,
            CellCoord::new(9, 8),
            Some(weapon(0x0600)),
            false,
            true,
        );
        let mut killed = false;
        for _ in 0..200 {
            w.tick(&[]);
            if !w.units.contains(sub) {
                killed = true;
                break;
            }
        }
        assert!(killed, "a destroyer must be able to sink a submarine");
    }
    // vs ground target on the shore
    {
        let mut w = World::new(shore_grid(), 0xD00D_000C);
        w.init_houses(2, 0);
        let ground = w.spawn_unit(
            0,
            1,
            CellCoord::new(10, H - 1),
            Facing(0),
            200,
            ship_stats(),
        );
        w.set_unit_combat(ground, 0, None, false);
        let dd = spawn_vessel(
            &mut w,
            0,
            CellCoord::new(10, H - 4),
            Some(weapon(0x0800)),
            false,
            true,
        );
        let mut hit = false;
        w.tick(&[Command::Attack {
            unit: dd,
            target: Target::Unit(ground),
            house: 0,
        }]);
        for _ in 0..200 {
            w.tick(&[]);
            if w.units.get(ground).map(|u| u.health).unwrap_or(0) < 200 {
                hit = true;
                break;
            }
        }
        assert!(hit, "a destroyer must be able to shell a ground target");
    }
}

/// A cruiser (CA) bombards from long range: an 8-inch gun (long range weapon)
/// damages a target ~8 cells away that a short-range unit could never reach.
#[test]
fn cruiser_long_range_bombard() {
    let mut w = World::new(open_sea(), 0xD00D_000D);
    w.init_houses(2, 0);
    let target = spawn_vessel(&mut w, 1, CellCoord::new(18, 8), None, false, false);
    // CA at (10,8): 8 cells away. Long range 0x0A00 (10 cells) reaches it.
    let ca = spawn_vessel(
        &mut w,
        0,
        CellCoord::new(10, 8),
        Some(weapon(0x0A00)),
        false,
        true,
    );
    let hp0 = w.units.get(target).unwrap().health;
    w.tick(&[Command::Attack {
        unit: ca,
        target: Target::Unit(target),
        house: 0,
    }]);
    let mut bombarded = false;
    let start = w.units.get(ca).unwrap().cell();
    for _ in 0..120 {
        w.tick(&[]);
        if w.units.get(target).map(|u| u.health).unwrap_or(0) < hp0 {
            bombarded = true;
            break;
        }
    }
    assert!(bombarded, "a long-range cruiser must hit a distant target");
    // It bombarded from afar rather than closing to point-blank.
    let end = w.units.get(ca).unwrap().cell();
    assert!(
        (end.x - start.x).abs() <= 3,
        "a long-range bombard should not require closing the whole distance"
    );
}

/// Determinism with vessels + live combat: same seed twice → identical hashes.
#[test]
fn determinism_vessels_plus_combat() {
    let run = || -> Vec<u64> {
        let mut w = World::new(open_sea(), 0xD00D_000E);
        w.init_houses(2, 0);
        let dd = spawn_vessel(
            &mut w,
            0,
            CellCoord::new(6, 8),
            Some(weapon(0x0600)),
            false,
            true,
        );
        let _sub = spawn_vessel(&mut w, 1, CellCoord::new(12, 8), None, true, false);
        let ca = spawn_vessel(
            &mut w,
            0,
            CellCoord::new(6, 10),
            Some(weapon(0x0A00)),
            false,
            true,
        );
        let mut hs = vec![w.tick(&[Command::Move {
            unit: dd,
            dest: CellCoord::new(10, 8),
            house: 0,
        }])];
        let _ = ca;
        for _ in 0..80 {
            hs.push(w.tick(&[]));
        }
        hs
    };
    assert_eq!(run(), run(), "vessels+combat diverged on identical replay");
}
