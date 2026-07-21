//! Naval arc P0 — acceptance smoke coverage (ra-coder). Proves the water
//! locomotor (ships path over water only, never onto land), submarine stealth
//! (a submerged sub is hidden from a non-detector enemy but visible to a
//! destroyer/detector), and determinism with vessels — all headless. Exhaustive
//! adversarial coverage (shipyard production end-to-end, multi-sub retarget,
//! recloak-grace timing, AI naval) is handed to ra-tester; this is the minimal
//! proof the mechanics run and match the reference cited in
//! `ra-sim/src/world.rs` (`vessel.cpp` cloak/`SPEED_FLOAT`).

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    BuildingProto, Catalog, Command, Handle, MoveStats, Passability, Target, WarheadProfile,
    WeaponProfile, World,
};

const W: i32 = 20;
const H: i32 = 12;

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

/// Full-damage instant weapon vs none-armor, range in leptons.
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

/// A grid that is water everywhere except a small land wall at x=10, rows 5..=7
/// (so a ship routing east-to-west must go around it and never onto land).
fn sea_grid() -> Passability {
    let n = (W * H) as usize;
    let is_land = |x: i32, y: i32| x == 10 && (5..=7).contains(&y);
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
    let u = w.units.get_mut(h).unwrap();
    u.make_vessel(is_sub, is_det);
    h
}

/// A ship routes over water only — it reaches a water goal (around a land wall)
/// and never enters a land cell; ordered onto land it does not move there.
#[test]
fn ship_paths_over_water_only_never_onto_land() {
    let mut w = World::new(sea_grid(), 0x5CA7_0000);
    w.init_houses(2, 0);
    let start = CellCoord::new(2, 6);
    let goal = CellCoord::new(17, 6);
    let v = spawn_vessel(&mut w, 0, start, None, false, false);

    w.tick(&[Command::Move {
        unit: v,
        dest: goal,
        house: 0,
    }]);

    let mut reached = false;
    for _ in 0..400 {
        // Every cell the ship ever occupies must be water (never the land wall).
        let c = w.units.get(v).unwrap().cell();
        assert!(
            w.passability().is_water(c),
            "ship stepped onto a non-water cell {c:?}"
        );
        assert!(
            !(c.x == 10 && (5..=7).contains(&c.y)),
            "ship entered the land wall at {c:?}"
        );
        if c == goal {
            reached = true;
            break;
        }
        w.tick(&[]);
    }
    assert!(reached, "ship never reached the water goal");

    // Ordered onto a land cell: find_path fails (land is impassable to Water), so
    // the ship does not move there.
    let land = CellCoord::new(10, 6);
    w.tick(&[Command::Move {
        unit: v,
        dest: land,
        house: 0,
    }]);
    for _ in 0..30 {
        w.tick(&[]);
        let c = w.units.get(v).unwrap().cell();
        assert_ne!(c, land, "ship illegally moved onto land");
        assert!(w.passability().is_water(c));
    }
}

/// A submerged submarine is invisible to a non-detector enemy (never auto-
/// acquired), but a destroyer (detector) reveals it so an allied ship acquires it.
#[test]
fn submarine_stealth_hidden_from_non_detector_visible_to_destroyer() {
    let mut w = World::new(sea_grid(), 0x5CA7_0001);
    w.init_houses(2, 0);

    // Enemy (house 1) submarine, unarmed so it never surfaces to fire — it stays
    // submerged and is purely the target.
    let sub = spawn_vessel(&mut w, 1, CellCoord::new(6, 6), None, true, false);
    // Friendly (house 0) armed **non-detector** patrol ship two cells away, in
    // weapon range of the sub, on Guard (auto-acquire) — the observer.
    let patrol = spawn_vessel(
        &mut w,
        0,
        CellCoord::new(4, 6),
        Some(weapon(0x0500)),
        false,
        false,
    );

    // Without a detector, the submerged sub is hidden: the patrol never targets it.
    for _ in 0..30 {
        w.tick(&[]);
        assert!(
            w.units.get(patrol).unwrap().target.is_none(),
            "a non-detector acquired a submerged submarine"
        );
        assert!(
            w.units.get(sub).unwrap().submerged,
            "the unengaged submarine surfaced"
        );
    }

    // Add a friendly destroyer (detector) next to the sub. Now it is revealed and
    // the patrol (or the destroyer) acquires it.
    let _dd = spawn_vessel(
        &mut w,
        0,
        CellCoord::new(6, 8),
        Some(weapon(0x0500)),
        false,
        true,
    );
    let mut acquired = false;
    for _ in 0..30 {
        w.tick(&[]);
        let patrol_sees = matches!(
            w.units.get(patrol).unwrap().target,
            Some(Target::Unit(t)) if t == sub
        );
        let dd_sees = matches!(
            w.units.get(_dd).unwrap().target,
            Some(Target::Unit(t)) if t == sub
        );
        if patrol_sees || dd_sees {
            acquired = true;
            break;
        }
    }
    assert!(
        acquired,
        "a destroyer did not reveal the submarine to its allies"
    );
}

/// Same seed, same script, twice → identical per-tick hashes, with vessels
/// (including a submarine) present.
#[test]
fn determinism_with_vessels() {
    let run = || -> Vec<u64> {
        let mut w = World::new(sea_grid(), 0x5CA7_0002);
        w.init_houses(2, 0);
        let v = spawn_vessel(
            &mut w,
            0,
            CellCoord::new(2, 2),
            Some(weapon(0x0500)),
            false,
            false,
        );
        let _sub = spawn_vessel(&mut w, 1, CellCoord::new(15, 6), None, true, false);
        let mut hs = Vec::new();
        hs.push(w.tick(&[Command::Move {
            unit: v,
            dest: CellCoord::new(15, 9),
            house: 0,
        }]));
        for _ in 0..60 {
            hs.push(w.tick(&[]));
        }
        hs
    };
    assert_eq!(run(), run(), "vessel sim diverged on identical replay");
}

/// The naval-yard shore-placement rule: SYRD is placeable on a shore (a land
/// footprint with at least one adjacent water cell) and refused inland.
#[test]
fn shipyard_requires_adjacent_water() {
    // Land everywhere except a single water cell at (5,5). Ground masks all true.
    let n = (W * H) as usize;
    let mut water = vec![false; n];
    water[(5 * W + 5) as usize] = true;
    let ground = vec![true; n];
    let grid =
        Passability::per_locomotor_water(W, H, ground.clone(), ground.clone(), ground, water);

    let mut w = World::new(grid, 0x5CA7_0003);
    w.init_houses(2, 0);
    let mut cat = Catalog::new();
    cat.buildings.push(BuildingProto {
        name: "SYRD".to_string(),
        foot_w: 3,
        foot_h: 2,
        max_health: 1000,
        armor: 0,
        power: 0,
        cost: 2000,
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
    });
    w.set_catalog(cat);

    // A footprint whose adjacency ring includes the water cell (5,5): top-left
    // (3,4) → covers x3..5,y4..5, ring includes (5,5). Placeable.
    assert!(
        w.can_place_building(0, 0, CellCoord::new(3, 4)),
        "SYRD on a shore (adjacent to water) should be placeable"
    );
    // Far inland (no water anywhere near): refused.
    assert!(
        !w.can_place_building(0, 0, CellCoord::new(15, 9)),
        "SYRD inland (no adjacent water) must be refused"
    );
}
