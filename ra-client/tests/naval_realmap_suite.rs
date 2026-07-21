//! P0 naval end-to-end on a REAL coastal map (ra-tester, M7.18 audit).
//!
//! The synthetic `ra-sim/tests/naval_suite.rs` proves the water-locomotor and
//! submarine-stealth mechanics on a hand-built grid; the coder never verified
//! naval on real content. This suite closes that gap on `scm11ea.ini` — the
//! shipped multiplayer map with the MOST water in our archives (Snow theater,
//! 58% of its playable rect is open water; see `naval_realmap_survey.rs`). It:
//!
//!   1. proves the real SYRD shore-placement rule on real map content (placeable
//!      on a real shore, refused deep inland);
//!   2. drives REAL production of a destroyer through the economy and asserts it
//!      spawns onto a REAL water cell adjacent to the yard;
//!   3. sails that destroyer across real water and asserts it never once occupies
//!      a land cell on the REAL map;
//!   4. runs the submarine-stealth check against a real enemy submarine on the
//!      real map (hidden from a non-detector, revealed by a destroyer);
//!   5. dumps PNG evidence (ship on real water, submarine scene, shipyard).
//!
//! Skips cleanly (never fails) when the real assets are absent.

mod support;

use ra_client::appcore::AppCore;
use ra_client::assets;
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{BuildItem, Command, Handle, Target};
use std::collections::VecDeque;
use std::path::PathBuf;

const MAP: &str = "scm11ea.ini";

fn scratch() -> PathBuf {
    PathBuf::from(
        "/tmp/claude-1000/-home-cshi-dev-game/f65beaba-9afb-445c-a6fd-47d2eb3dad49/scratchpad",
    )
}

fn dump(core: &AppCore, name: &str) {
    let f = core.compose_game();
    let bytes = ra_client::png::encode_rgba(f.width, f.height, &f.pixels);
    let path = scratch().join(name);
    std::fs::write(&path, bytes).expect("write png");
    eprintln!("  wrote {}", path.display());
}

fn unit_id(core: &AppCore, name: &str) -> u32 {
    core.world()
        .catalog
        .units
        .iter()
        .position(|u| u.name.eq_ignore_ascii_case(name))
        .unwrap_or_else(|| panic!("{name} in catalog")) as u32
}

fn building_id(core: &AppCore, name: &str) -> u32 {
    core.world()
        .catalog
        .buildings
        .iter()
        .position(|b| b.name.eq_ignore_ascii_case(name))
        .unwrap_or_else(|| panic!("{name} in catalog")) as u32
}

/// Load the real coastal map into an economy game (full catalog incl. naval +
/// credits + real water passability), or `None` (skip). The econ loader is the
/// one that installs the complete buildable catalog (SS/DD/CA/SYRD/SPEN).
fn load() -> Option<assets::EconGame> {
    if !support::real_assets_available() {
        eprintln!("SKIP: real assets not found");
        return None;
    }
    match assets::load_econ_from_dir(&support::assets_dir(), MAP, 1_000_000) {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("SKIP: could not load {MAP}: {e}");
            None
        }
    }
}

/// Scan rectangle `(x0, y0, x1, y1)` (exclusive upper bound), inset from the map
/// edge — the water mask is only ever `true` inside the real playable area.
const SCAN: (i32, i32, i32, i32) = (1, 1, 127, 127);

/// A house index that owns no buildings (so a SYRD founding-placement bypasses
/// the proximity rule and its shore-adjacency rule is the only gate).
fn empty_house(core: &AppCore) -> u8 {
    let w = core.world();
    for h in 0..w.houses.len() as u8 {
        if !w.buildings.iter().any(|(_, b)| b.house == h) {
            return h;
        }
    }
    0
}

/// Whether the SYRD footprint (`fw`×`fh`) at top-left `c` sits entirely on land
/// (static-passable, unoccupied, not water) within the playable rect.
fn footprint_on_land(
    core: &AppCore,
    c: CellCoord,
    fw: i32,
    fh: i32,
    r: (i32, i32, i32, i32),
) -> bool {
    let p = core.world().passability();
    for dy in 0..fh {
        for dx in 0..fw {
            let cell = CellCoord::new(c.x + dx, c.y + dy);
            if cell.x < r.0 || cell.y < r.1 || cell.x >= r.2 || cell.y >= r.3 {
                return false;
            }
            if !p.is_static_passable(cell) || p.is_occupied(cell) || p.is_water(cell) {
                return false;
            }
        }
    }
    true
}

/// Whether the SYRD footprint's 8-neighbour ring touches open water.
fn ring_touches_water(core: &AppCore, c: CellCoord, fw: i32, fh: i32) -> bool {
    let p = core.world().passability();
    for y in (c.y - 1)..=(c.y + fh) {
        for x in (c.x - 1)..=(c.x + fw) {
            if p.is_water(CellCoord::new(x, y)) {
                return true;
            }
        }
    }
    false
}

/// A far-away water cell reachable from `start` over water only (BFS on the
/// water mask, within the playable rect) — the farthest reachable cell found.
fn farthest_reachable_water(
    core: &AppCore,
    start: CellCoord,
    r: (i32, i32, i32, i32),
) -> CellCoord {
    let p = core.world().passability();
    let mut seen = std::collections::HashSet::new();
    let mut q = VecDeque::new();
    q.push_back(start);
    seen.insert((start.x, start.y));
    let mut far = start;
    let mut far_d = 0i32;
    while let Some(c) = q.pop_front() {
        let d = (c.x - start.x).abs() + (c.y - start.y).abs();
        if d > far_d {
            far_d = d;
            far = c;
        }
        for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            let n = CellCoord::new(c.x + dx, c.y + dy);
            if n.x < r.0 || n.y < r.1 || n.x >= r.2 || n.y >= r.3 {
                continue;
            }
            if p.is_water(n) && seen.insert((n.x, n.y)) {
                q.push_back(n);
            }
        }
    }
    far
}

// ---------------------------------------------------------------------------

/// (1)+(2)+(3): place a real shipyard on scm11ea's shore, produce a destroyer
/// through the real economy, and sail it across real water — asserting the
/// destroyer only ever occupies water cells (never land) on the REAL map.
#[test]
fn destroyer_produced_at_real_shipyard_sails_real_water_only() {
    let Some(g) = load() else { return };
    let r = SCAN;
    let mut core = g.core;

    let syrd = building_id(&core, "SYRD");
    let dd = unit_id(&core, "DD");
    let (fw, fh) = {
        let p = core.world().catalog.building(syrd).unwrap();
        (p.foot_w as i32, p.foot_h as i32)
    };
    let house = empty_house(&core);

    // --- (1) Shore-placement rule on real map content ---
    // A real shore: SYRD footprint all on land, ring touches real water, and the
    // game's own `can_place_building` accepts it (founding case → proximity is
    // bypassed, so the naval shore-adjacency rule is the deciding gate).
    let mut shore: Option<CellCoord> = None;
    let mut inland: Option<CellCoord> = None;
    'scan: for y in r.1..r.3 - fh {
        for x in r.0..r.2 - fw {
            let c = CellCoord::new(x, y);
            if !footprint_on_land(&core, c, fw, fh, r) {
                continue;
            }
            let placeable = core.world().can_place_building(house, syrd, c);
            if ring_touches_water(&core, c, fw, fh) {
                if placeable && shore.is_none() {
                    shore = Some(c);
                }
            } else if !placeable && inland.is_none() {
                // Land footprint with NO water in its ring → the naval rule must
                // refuse it even though footprint+proximity would otherwise pass.
                inland = Some(c);
            }
            if shore.is_some() && inland.is_some() {
                break 'scan;
            }
        }
    }
    let shore = shore.expect("scm11ea should have at least one placeable SYRD shore cell");
    let inland = inland.expect("scm11ea should have at least one inland (no-water-ring) land cell");
    assert!(
        core.world().can_place_building(house, syrd, shore),
        "SYRD must be placeable on a real shore"
    );
    assert!(
        !core.world().can_place_building(house, syrd, inland),
        "SYRD must be refused on real inland (no adjacent water)"
    );
    eprintln!("real SYRD shore={shore:?} inland-refused={inland:?} (footprint {fw}x{fh})");

    // --- (2) Real production: place the yard, fund the house, build a DD ---
    core.world_mut().set_house_credits(house, 1_000_000);
    core.world_mut()
        .spawn_building(syrd, house, shore)
        .expect("place SYRD on shore");
    // Reveal a generous disc so the shipyard + spawned ship show in the frame.
    for dy in -6..12 {
        for dx in -6..12 {
            core.world_mut()
                .reveal_shroud(house, CellCoord::new(shore.x + dx, shore.y + dy), 8);
        }
    }

    let before: std::collections::HashSet<Handle> =
        core.world().units.iter().map(|(h, _)| h).collect();
    core.world_mut().tick(&[Command::StartProduction {
        house,
        item: BuildItem::Unit(dd),
    }]);
    let mut ship: Option<Handle> = None;
    for _ in 0..40_000 {
        core.world_mut().tick(&[]);
        if let Some((h, _)) = core
            .world()
            .units
            .iter()
            .find(|(h, u)| !before.contains(h) && u.house == house && u.is_vessel())
        {
            ship = Some(h);
            break;
        }
    }
    let ship = ship.expect("the naval yard should produce a destroyer");
    let spawn_cell = core.world().units.get(ship).unwrap().cell();
    assert!(
        core.world().passability().is_water(spawn_cell),
        "produced destroyer must spawn onto a REAL water cell, got {spawn_cell:?}"
    );
    // Adjacent to the yard footprint ring.
    assert!(
        (spawn_cell.x >= shore.x - 1 && spawn_cell.x <= shore.x + fw)
            && (spawn_cell.y >= shore.y - 1 && spawn_cell.y <= shore.y + fh),
        "destroyer must spawn in the yard's adjacency ring, got {spawn_cell:?} vs yard {shore:?}"
    );
    eprintln!("destroyer spawned at real water cell {spawn_cell:?}");

    // PNG evidence: the destroyer sitting on real water beside its shipyard.
    core.set_camera(
        ((shore.x - 6) * 24).max(0) as f32,
        ((shore.y - 6) * 24).max(0) as f32,
    );
    core.update(60);
    dump(&core, "naval_real_shipyard_destroyer.png");

    // --- (3) Sail across real water, never onto land ---
    let goal = farthest_reachable_water(&core, spawn_cell, r);
    assert!(
        (goal.x - spawn_cell.x).abs() + (goal.y - spawn_cell.y).abs() >= 6,
        "expected a non-trivial water voyage on scm11ea (goal {goal:?} too close to {spawn_cell:?})"
    );
    core.world_mut().tick(&[Command::Move {
        unit: ship,
        dest: goal,
        house,
    }]);
    let mut reached = false;
    let mut max_d = 0i32;
    for _ in 0..4000 {
        let c = core.world().units.get(ship).unwrap().cell();
        // INVARIANT: every cell the destroyer ever occupies is real water.
        assert!(
            core.world().passability().is_water(c),
            "destroyer stepped onto a non-water cell {c:?} on the real map"
        );
        let d = (c.x - spawn_cell.x).abs() + (c.y - spawn_cell.y).abs();
        max_d = max_d.max(d);
        if c == goal {
            reached = true;
            break;
        }
        core.world_mut().tick(&[]);
    }
    assert!(
        reached || max_d >= 6,
        "destroyer made no meaningful headway across the water (max manhattan {max_d})"
    );
    eprintln!(
        "destroyer sailed real water: start {spawn_cell:?} -> goal {goal:?}, reached={reached}, \
         max_dist={max_d}"
    );

    // Reveal + reframe on the ship's mid-voyage position and dump the "ship on
    // real open water" evidence.
    let pos = core.world().units.get(ship).unwrap().cell();
    for dy in -6..8 {
        for dx in -6..8 {
            core.world_mut()
                .reveal_shroud(house, CellCoord::new(pos.x + dx, pos.y + dy), 8);
        }
    }
    core.set_camera(
        ((pos.x - 6) * 24).max(0) as f32,
        ((pos.y - 6) * 24).max(0) as f32,
    );
    core.update(60);
    dump(&core, "naval_real_ship_on_water.png");
}

/// (4): submarine stealth on the real map — a submerged enemy submarine is hidden
/// from a non-detector patrol ship, but a friendly destroyer reveals it.
#[test]
fn submarine_stealth_on_real_map() {
    let Some(g) = load() else { return };
    let r = SCAN;
    let mut core = g.core;

    let ss = unit_id(&core, "SS");
    let dd = unit_id(&core, "DD");
    let (ss_sprite, ss_hp, ss_stats) = {
        let u = core.world().catalog.unit(ss).unwrap();
        (u.sprite_id, u.max_health, u.stats)
    };
    let (dd_sprite, dd_hp, dd_stats, dd_wpn) = {
        let u = core.world().catalog.unit(dd).unwrap();
        (u.sprite_id, u.max_health, u.stats, u.weapon)
    };

    // Two mutually non-allied houses on the real map.
    let (own, enemy) = {
        let w = core.world();
        let n = w.houses.len() as u8;
        let mut pair = None;
        'p: for a in 0..n {
            for b in 0..n {
                if a != b && !w.are_allies(a, b) {
                    pair = Some((a, b));
                    break 'p;
                }
            }
        }
        pair.expect("two non-allied houses on the real map")
    };

    // A run of contiguous water cells in the playable rect to stage the scene on.
    let p = core.world().passability();
    let mut base: Option<CellCoord> = None;
    'find: for y in r.1..r.3 {
        for x in r.0..r.2 - 4 {
            if (0..5).all(|k| p.is_water(CellCoord::new(x + k, y))) {
                base = Some(CellCoord::new(x, y));
                break 'find;
            }
        }
    }
    let base = base.expect("scm11ea should have a 5-cell run of open water");

    // Enemy submarine (submerged, unarmed → never surfaces) as the target.
    let sub_cell = CellCoord::new(base.x + 2, base.y);
    let sub = core
        .world_mut()
        .spawn_unit(ss_sprite, enemy, sub_cell, Facing(0), ss_hp, ss_stats);
    core.world_mut()
        .units
        .get_mut(sub)
        .unwrap()
        .make_vessel(true, false);

    // Friendly armed NON-detector patrol ship next to the sub (a real DD hull,
    // but forced to a plain non-detector vessel so it cannot itself reveal subs).
    let patrol = core
        .world_mut()
        .spawn_unit(dd_sprite, own, base, Facing(0), dd_hp, dd_stats);
    core.world_mut().set_unit_combat(patrol, 0, dd_wpn, false);
    core.world_mut()
        .units
        .get_mut(patrol)
        .unwrap()
        .make_vessel(false, false);

    // Without a detector, the submerged sub is hidden: the patrol never targets it.
    for _ in 0..30 {
        core.world_mut().tick(&[]);
        assert!(
            core.world().units.get(patrol).unwrap().target.is_none(),
            "a non-detector acquired a submerged submarine on the real map"
        );
        assert!(
            core.world().units.get(sub).unwrap().submerged,
            "the unengaged submarine surfaced"
        );
    }

    // PNG evidence: the submarine scene on real water (sub submerged near patrol).
    for dy in -6..8 {
        for dx in -6..8 {
            core.world_mut()
                .reveal_shroud(own, CellCoord::new(base.x + dx, base.y + dy), 8);
        }
    }
    core.set_camera(
        ((base.x - 5) * 24).max(0) as f32,
        ((base.y - 5) * 24).max(0) as f32,
    );
    core.update(60);
    dump(&core, "naval_real_submarine.png");

    // Add a friendly DESTROYER (real detector) beside the sub → it is revealed and
    // an allied ship acquires it.
    let det_cell = CellCoord::new(base.x + 3, base.y);
    let det = core
        .world_mut()
        .spawn_unit(dd_sprite, own, det_cell, Facing(0), dd_hp, dd_stats);
    core.world_mut().set_unit_combat(det, 0, dd_wpn, false);
    core.world_mut()
        .units
        .get_mut(det)
        .unwrap()
        .make_vessel(false, true);

    let mut acquired = false;
    for _ in 0..30 {
        core.world_mut().tick(&[]);
        let sees = |h| {
            matches!(
                core.world().units.get(h).and_then(|u| u.target),
                Some(Target::Unit(t)) if t == sub
            )
        };
        if sees(patrol) || sees(det) {
            acquired = true;
            break;
        }
    }
    assert!(
        acquired,
        "a real destroyer (detector) did not reveal the submarine to its allies on the real map"
    );
}
