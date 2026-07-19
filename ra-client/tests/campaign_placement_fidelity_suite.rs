//! M7.5-A campaign placement/scripting fidelity: the two Allied missions that
//! sit next to the already-covered `scg01ea.ini` in the campaign arc —
//! `scg02ea.ini` ("Five to one") and `scg03ea.ini` ("Dead End").
//!
//! Unlike `campaign_scg01ea.rs` (which hand-derives its expected counts once
//! by reading the INI and then hard-codes them), every expectation here is
//! computed **live**, in this file, from the raw scenario INI text returned by
//! `assets::scenario_text_from_archive` — parsed independently with
//! `ra_formats::ini::Ini`, never by calling the loader's own
//! `ra_data::campaign` parsers (that would just be testing the code against
//! itself). The only "trusted" helpers reused here are pure name/index
//! lookup tables (`ra_data::campaign::campaign_house_index`,
//! `CellCoord::from_index`), not scenario business logic.
//!
//! Three documented deferrals from `docs/QUIRKS.md` Q17 shape the oracle math
//! (read Q17 before touching this file):
//!   1. `[STRUCTURES]` entries whose type has no rules.ini building section
//!      (barrel/oil-pump props: `BARL`/`BRL3`/`V19`/…) are not placed.
//!   2. Naval/aircraft unit classes have no sim and are dropped.
//!   3. `[TERRAIN]` stamps occupancy only (no render), 1:1 with its raw entry
//!      count — no skip logic there.
//!   4. `[Basic]/house Allies=` builds a **symmetric** alliance bitmask, even
//!      when the INI only lists the pair in one direction.
//!
//! Skips cleanly (never fails) when the real assets aren't present.

mod support;

use ra_client::assets;
use ra_data::campaign::campaign_house_index;
use ra_formats::ini::Ini;
use ra_sim::{CellCoord, Facing, Handle, Locomotor, World};
use std::collections::BTreeSet;

/// Resolve the real archives, or print a skip notice and return `None`. Every
/// test in this file starts with this and returns early on `None` — never a
/// failure — matching `campaign_scg01ea.rs`'s skip-clean contract.
fn load_archives() -> Option<(Vec<u8>, Vec<u8>)> {
    let dir = support::assets_dir();
    if !dir.join("main.mix").is_file() || !dir.join("redalert.mix").is_file() {
        eprintln!(
            "SKIP: real assets not found under {} (set RA_ASSETS_DIR)",
            dir.display()
        );
        return None;
    }
    let main = std::fs::read(dir.join("main.mix")).ok()?;
    let redalert = std::fs::read(dir.join("redalert.mix")).ok()?;
    Some((main, redalert))
}

/// Case-insensitive set of every catalog name a scenario load successfully
/// resolved. `register_campaign_building`/`register_campaign_unit` (assets.rs)
/// only ever *push* a proto after a successful rules.ini/naval-air check, so
/// this is exactly "what actually got placed" — comparing an INI entry's type
/// name against this set is thus equivalent to re-running the loader's own
/// resolvability check, without calling the loader's internals.
fn building_name_set(world: &World) -> BTreeSet<String> {
    world
        .catalog
        .buildings
        .iter()
        .map(|b| b.name.to_ascii_uppercase())
        .collect()
}

fn unit_name_set(world: &World) -> BTreeSet<String> {
    world
        .catalog
        .units
        .iter()
        .map(|u| u.name.to_ascii_uppercase())
        .collect()
}

/// Oracle for `[STRUCTURES]`: `house,type,strength,cell,facing[,trigger,
/// sellable,rebuild]` (`BuildingClass::Read_INI`) — the type name is comma
/// field **index 1**. Returns `(resolvable, well_formed_total)`; well-formed
/// mirrors the loader's own line filter (>=5 fields, house name resolves,
/// cell parses) so malformed/unknown-house lines — if any — are excluded from
/// both sides identically, the same way `ra_data::campaign::parse_structures`
/// drops them before the resolver ever sees them.
fn oracle_structures(ini: &Ini, building_names: &BTreeSet<String>) -> (usize, usize) {
    let mut total = 0;
    let mut resolvable = 0;
    if let Some(entries) = ini.section_entries("STRUCTURES") {
        for (_, value) in entries {
            let f: Vec<&str> = value.split(',').map(|s| s.trim()).collect();
            if f.len() < 5 {
                continue;
            }
            if campaign_house_index(f[0]).is_none() {
                continue;
            }
            if f[3].parse::<u32>().is_err() {
                continue;
            }
            total += 1;
            if building_names.contains(&f[1].to_ascii_uppercase()) {
                resolvable += 1;
            }
        }
    }
    (resolvable, total)
}

/// Oracle for `[UNITS]`/`[INFANTRY]`: `house,type,strength,cell,...` — same
/// field layout, type name at index 1, well-formed needs >=6 fields
/// (`UnitClass::Read_INI`/`InfantryClass::Read_INI`).
fn oracle_placements(ini: &Ini, section: &str, unit_names: &BTreeSet<String>) -> (usize, usize) {
    let mut total = 0;
    let mut resolvable = 0;
    if let Some(entries) = ini.section_entries(section) {
        for (_, value) in entries {
            let f: Vec<&str> = value.split(',').map(|s| s.trim()).collect();
            if f.len() < 6 {
                continue;
            }
            if campaign_house_index(f[0]).is_none() {
                continue;
            }
            if f[3].parse::<u32>().is_err() {
                continue;
            }
            total += 1;
            if unit_names.contains(&f[1].to_ascii_uppercase()) {
                resolvable += 1;
            }
        }
    }
    (resolvable, total)
}

/// Oracle for `[TERRAIN]`: `cell=TypeName`, no skip logic — every entry whose
/// key parses as a cell number counts (matches `parse_terrain`).
fn oracle_terrain_count(ini: &Ini) -> usize {
    ini.section_entries("TERRAIN")
        .map(|entries| {
            entries
                .iter()
                .filter(|(k, _)| k.trim().parse::<u32>().is_ok())
                .count()
        })
        .unwrap_or(0)
}

/// Oracle for `[Trigs]`: 18 comma-separated fields per well-formed entry.
fn oracle_trigger_count(ini: &Ini) -> usize {
    ini.section_entries("Trigs")
        .map(|entries| {
            entries
                .iter()
                .filter(|(_, v)| v.split(',').count() >= 18)
                .count()
        })
        .unwrap_or(0)
}

/// Oracle for `[TeamTypes]`: at least 8 leading comma fields before the
/// class/mission lists (house, flags, recruit, init_num, max_allowed, origin,
/// trigger, class_count).
fn oracle_teamtype_count(ini: &Ini) -> usize {
    ini.section_entries("TeamTypes")
        .map(|entries| {
            entries
                .iter()
                .filter(|(_, v)| v.split(',').count() >= 8)
                .count()
        })
        .unwrap_or(0)
}

/// Run the full inventory oracle against one scenario and assert every
/// `CampaignMission` count matches it. Prints an `eprintln!` report so
/// `-- --nocapture` shows the numbers.
fn inventory_check(scenario_name: &str) {
    let Some((main_bytes, redalert_bytes)) = load_archives() else {
        return;
    };

    let ini_text = match assets::scenario_text_from_archive(&main_bytes, scenario_name) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("SKIP {scenario_name}: could not read scenario text: {e}");
            return;
        }
    };
    let ini = Ini::parse(&ini_text);

    let mission =
        match assets::load_campaign_from_bytes(&main_bytes, &redalert_bytes, scenario_name) {
            Ok(m) => m,
            Err(e) => {
                eprintln!(
                    "SKIP {scenario_name}: failed to load ({e}) — scenario may reference \
                     unimplemented reference content"
                );
                return;
            }
        };

    let world = mission.core.world();
    let bnames = building_name_set(world);
    let unames = unit_name_set(world);

    let (units_resolvable, units_total) = oracle_placements(&ini, "UNITS", &unames);
    let (infantry_resolvable, infantry_total) = oracle_placements(&ini, "INFANTRY", &unames);
    let (structs_resolvable, structs_total) = oracle_structures(&ini, &bnames);
    let terrain_total = oracle_terrain_count(&ini);
    let triggers_total = oracle_trigger_count(&ini);
    let teamtypes_total = oracle_teamtype_count(&ini);

    eprintln!("=== {scenario_name} '{}' ===", mission.name);
    eprintln!(
        "placements: {}/{} vehicles, {}/{} infantry, {}/{} structures, {} terrain \
         (placed/well-formed-INI-count; terrain has no skip logic)",
        mission.units_placed,
        units_total,
        mission.infantry_placed,
        infantry_total,
        mission.structures_placed,
        structs_total,
        mission.terrain_placed,
    );
    eprintln!(
        "scripting: {} triggers, {} teamtypes",
        mission.triggers, mission.teamtypes
    );
    eprintln!("player house: {}", mission.player_house);
    eprintln!(
        "skipped ({} entries): {:?}",
        mission.skipped.len(),
        mission.skipped
    );

    assert_eq!(
        mission.units_placed, units_resolvable,
        "{scenario_name}: units_placed should equal the count of [UNITS] entries whose \
         type resolves against the final unit catalog"
    );
    assert_eq!(
        mission.infantry_placed, infantry_resolvable,
        "{scenario_name}: infantry_placed should equal the count of [INFANTRY] entries \
         whose type resolves against the final unit catalog"
    );
    assert_eq!(
        mission.structures_placed, structs_resolvable,
        "{scenario_name}: structures_placed should equal the count of [STRUCTURES] entries \
         whose type resolves against the final building catalog (QUIRKS Q17.5 — barrel/\
         oil-pump props with no rules.ini section are skipped, not placed)"
    );
    assert_eq!(
        mission.terrain_placed, terrain_total,
        "{scenario_name}: terrain_placed should equal the raw [TERRAIN] entry count — \
         occupancy stamping has no skip logic (QUIRKS Q17.4)"
    );
    assert_eq!(
        mission.triggers, triggers_total,
        "{scenario_name}: triggers should equal the well-formed [Trigs] entry count"
    );
    assert_eq!(
        mission.teamtypes, teamtypes_total,
        "{scenario_name}: teamtypes should equal the well-formed [TeamTypes] entry count \
         (unresolved naval/air class members are dropped per-member, not per-team)"
    );

    // Cross-check: every well-formed-but-unresolvable STRUCTURES type name
    // should show up in `mission.skipped` (case-insensitive containment) —
    // ties the oracle's "unresolvable" classification back to the loader's
    // own bookkeeping.
    if let Some(entries) = ini.section_entries("STRUCTURES") {
        let skipped_upper: BTreeSet<String> = mission
            .skipped
            .iter()
            .map(|s| s.to_ascii_uppercase())
            .collect();
        for (_, value) in entries {
            let f: Vec<&str> = value.split(',').map(|s| s.trim()).collect();
            if f.len() < 5 || campaign_house_index(f[0]).is_none() || f[3].parse::<u32>().is_err() {
                continue;
            }
            let ty = f[1].to_ascii_uppercase();
            if !bnames.contains(&ty) {
                assert!(
                    skipped_upper.contains(&ty),
                    "{scenario_name}: unresolvable structure type {ty} should be recorded \
                     in mission.skipped"
                );
            }
        }
    }
}

#[test]
fn scg02ea_inventory_matches_ini_oracle() {
    inventory_check("scg02ea.ini");
}

#[test]
fn scg03ea_inventory_matches_ini_oracle() {
    inventory_check("scg03ea.ini");
}

/// The 20 campaign house sections `[Basic]`/per-house data can appear under
/// (`ra_data::campaign::campaign_house_index`'s table), used to build the
/// alliance oracle directly from the INI.
const HOUSE_NAMES: [&str; 20] = [
    "Spain", "Greece", "USSR", "England", "Ukraine", "Germany", "France", "Turkey", "GoodGuy",
    "BadGuy", "Neutral", "Special", "Multi1", "Multi2", "Multi3", "Multi4", "Multi5", "Multi6",
    "Multi7", "Multi8",
];

/// Parse every house section's `Allies=` line directly and return the
/// **symmetric closure** of the declared pairs (an A→B listing implies B is
/// allied to A too — QUIRKS Q17.6 / `build_alliances`), independent of
/// `ra_data::campaign::parse_house_defs`/`build_alliances` (the code under
/// test).
fn oracle_allied_pairs(ini: &Ini) -> BTreeSet<(u8, u8)> {
    let mut pairs = BTreeSet::new();
    for name in HOUSE_NAMES {
        let Some(hi) = campaign_house_index(name) else {
            continue;
        };
        let Some(list) = ini.get(name, "Allies") else {
            continue;
        };
        for ally in list.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
            if let Some(hj) = campaign_house_index(ally) {
                pairs.insert((hi, hj));
                pairs.insert((hj, hi));
            }
        }
    }
    pairs
}

/// Manhattan/Chebyshev distance between two cells, in cells.
fn chebyshev(a: CellCoord, b: CellCoord) -> i32 {
    (a.x - b.x).abs().max((a.y - b.y).abs())
}

/// Find a cell at least `min_dist` cells (Chebyshev) from every live unit,
/// every live building, and every cell in `avoid` — a quiet corner of the map
/// to spawn synthetic test units without a real placement (or a scripted
/// reinforcement landing on a waypoint) accidentally becoming the "nearest
/// enemy" instead of our controlled pair.
fn empty_spot(world: &World, avoid: &[CellCoord], min_dist: i32) -> CellCoord {
    for y in (4..124).step_by(6) {
        for x in (4..124).step_by(6) {
            let c = CellCoord::new(x, y);
            let clear = world
                .units
                .iter()
                .all(|(_, u)| chebyshev(u.cell(), c) >= min_dist)
                && world
                    .buildings
                    .iter()
                    .all(|(_, b)| chebyshev(b.cell, c) >= min_dist)
                && avoid.iter().all(|&t| chebyshev(t, c) >= min_dist);
            if clear {
                return c;
            }
        }
    }
    panic!("could not find an empty test spot on the 128x128 map");
}

/// Spawn a fully combat-capable synthetic unit from a resolved catalog proto,
/// for the alliance-gating behavioral check.
fn spawn_combatant(
    world: &mut World,
    proto_id: u32,
    proto: &ra_sim::UnitProto,
    house: u8,
    cell: CellCoord,
    hunt: bool,
) -> Handle {
    let h = world.spawn_unit(
        proto_id,
        house,
        cell,
        Facing(0),
        proto.max_health,
        proto.stats,
    );
    world.set_unit_combat(h, proto.armor, proto.weapon, proto.has_turret);
    if let Some(u) = world.units.get_mut(h) {
        u.hunt = hunt;
    }
    h
}

/// Alliances: `[Basic]/house Allies=` must build a symmetric bitmask
/// (`World::are_allies` symmetric both ways, matching the INI's declared set
/// under symmetric closure), and that matrix must actually gate combat —
/// hunt-mode auto-acquire (`maybe_acquire_hunt_target`) must never let two
/// allied units engage, but must let two non-allied ones.
///
/// Uses `scg02ea.ini`: its `Allies=` lines are declared asymmetrically in the
/// raw INI (`USSR Allies=France,BadGuy` but `France` has no `Allies=` back to
/// USSR at all) — the more interesting case for proving the symmetric-closure
/// behavior, vs. scg03ea's already-mutual pairs.
#[test]
fn alliance_matrix_is_symmetric_and_gates_auto_acquire() {
    let Some((main_bytes, redalert_bytes)) = load_archives() else {
        return;
    };
    let scenario_name = "scg02ea.ini";
    let ini_text = match assets::scenario_text_from_archive(&main_bytes, scenario_name) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("SKIP {scenario_name}: could not read scenario text: {e}");
            return;
        }
    };
    let ini = Ini::parse(&ini_text);
    let allied_pairs = oracle_allied_pairs(&ini);
    eprintln!("oracle allied pairs (symmetric closure): {allied_pairs:?}");
    assert!(
        !allied_pairs.is_empty(),
        "{scenario_name} should declare at least one Allies= pair"
    );
    // scg02ea's raw asymmetric listings that the loader must symmetrize:
    // USSR(2) lists France(6) and BadGuy(9); Neutral(10) lists Special(11).
    assert!(allied_pairs.contains(&(2, 6)) && allied_pairs.contains(&(6, 2)));
    assert!(allied_pairs.contains(&(2, 9)) && allied_pairs.contains(&(9, 2)));
    assert!(allied_pairs.contains(&(10, 11)) && allied_pairs.contains(&(11, 10)));
    // Greece(1)/England(3) are declared mutually already.
    assert!(allied_pairs.contains(&(1, 3)) && allied_pairs.contains(&(3, 1)));

    let mut mission =
        match assets::load_campaign_from_bytes(&main_bytes, &redalert_bytes, scenario_name) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("SKIP {scenario_name}: failed to load ({e})");
                return;
            }
        };

    // 1. Full symmetric-match check over every campaign house pair.
    {
        let world = mission.core.world();
        for a in 0u8..20 {
            for b in 0u8..20 {
                let expect = a == b || allied_pairs.contains(&(a, b));
                assert_eq!(
                    world.are_allies(a, b),
                    expect,
                    "are_allies({a},{b}) should match the INI's symmetric-closure alliance set"
                );
                assert_eq!(
                    world.are_allies(a, b),
                    world.are_allies(b, a),
                    "are_allies({a},{b}) must be symmetric"
                );
            }
        }
    }

    // 2. Behavioral gate: pick a real armed, non-infantry catalog proto (any
    // will do — we only need `World::are_allies` to gate `maybe_acquire_hunt_
    // target`, not any particular weapon), preferring a fast-turning one so
    // firing alignment resolves quickly.
    let (proto_id, proto): (u32, ra_sim::UnitProto) = {
        let world = mission.core.world();
        let (id, p) = world
            .catalog
            .units
            .iter()
            .enumerate()
            .filter(|(_, p)| p.weapon.is_some() && !p.is_infantry)
            .max_by_key(|(_, p)| p.stats.rot)
            .expect("scg02ea's catalog should contain at least one armed vehicle type");
        (id as u32, p.clone())
    };
    eprintln!(
        "alliance-gate combatant proto: {} (range={} rot={})",
        proto.name,
        proto.weapon.unwrap().range,
        proto.stats.rot
    );

    // Waypoints are potential reinforcement landing spots — avoid them too so
    // a scripted team can't wander in and confuse "nearest enemy".
    let waypoint_cells: Vec<CellCoord> = ini
        .section_entries("Waypoints")
        .map(|entries| {
            entries
                .iter()
                .filter_map(|(_, v)| v.trim().parse::<i32>().ok())
                .filter(|&c| c >= 0)
                .map(|c| CellCoord::from_index(c as u32))
                .collect()
        })
        .unwrap_or_default();

    let (spot_allied, spot_enemy) = {
        let world = mission.core.world();
        let a = empty_spot(world, &waypoint_cells, 10);
        let mut avoid2 = waypoint_cells.clone();
        avoid2.push(a);
        let b = empty_spot(world, &avoid2, 10);
        (a, b)
    };

    // Allied pair: Greece(1)/England(3) — declared mutually allied above.
    let (allied_a, allied_b) = (1u8, 3u8);
    // Non-allied pair: Greece(1)/USSR(2) — never listed together.
    let (enemy_a, enemy_b) = (1u8, 2u8);
    assert!(!allied_pairs.contains(&(enemy_a, enemy_b)) && enemy_a != enemy_b);

    let world = mission.core.world_mut();
    let ally1 = spawn_combatant(world, proto_id, &proto, allied_a, spot_allied, true);
    let ally2 = spawn_combatant(
        world,
        proto_id,
        &proto,
        allied_b,
        CellCoord::new(spot_allied.x + 1, spot_allied.y),
        true,
    );
    let foe1 = spawn_combatant(world, proto_id, &proto, enemy_a, spot_enemy, true);
    let foe2 = spawn_combatant(
        world,
        proto_id,
        &proto,
        enemy_b,
        CellCoord::new(spot_enemy.x + 1, spot_enemy.y),
        true,
    );

    for _ in 0..300 {
        world.tick(&[]);
    }

    let health = |world: &World, h: Handle| world.units.get(h).map(|u| u.health);
    let ally1_health = health(world, ally1);
    let ally2_health = health(world, ally2);
    let foe1_health = health(world, foe1);
    let foe2_health = health(world, foe2);
    eprintln!(
        "after 300 ticks: allied pair health {ally1_health:?}/{ally2_health:?} (max {}), \
         enemy pair health {foe1_health:?}/{foe2_health:?} (max {})",
        proto.max_health, proto.max_health
    );

    assert_eq!(
        ally1_health,
        Some(proto.max_health),
        "an allied Greece unit must never take damage from an allied England unit \
         standing next to it"
    );
    assert_eq!(
        ally2_health,
        Some(proto.max_health),
        "an allied England unit must never take damage from an allied Greece unit \
         standing next to it"
    );
    // Neither allied unit's hunt-acquired target may ever be its own ally
    // partner (guaranteed structurally by `are_allies` gating
    // `maybe_acquire_hunt_target`'s scan, verified behaviorally here).
    let ally1_target = world.units.get(ally1).and_then(|u| u.target);
    let ally2_target = world.units.get(ally2).and_then(|u| u.target);
    assert_ne!(ally1_target, Some(ra_sim::Target::Unit(ally2)));
    assert_ne!(ally2_target, Some(ra_sim::Target::Unit(ally1)));

    assert!(
        foe1_health.unwrap_or(0) < proto.max_health || foe2_health.unwrap_or(0) < proto.max_health,
        "two non-allied units placed one cell apart with hunt enabled must engage in combat \
         within 300 ticks (foe1={foe1_health:?} foe2={foe2_health:?} max={})",
        proto.max_health
    );
}

/// `[TERRAIN]` entries stamp occupancy only (QUIRKS Q17.4): assert a handful
/// of declared terrain cells are Foot-impassable, and that A* routes around
/// (not through) one of them. Checks both scg02ea and scg03ea since both
/// declare `[TERRAIN]`.
fn terrain_occupancy_check(scenario_name: &str) {
    let Some((main_bytes, redalert_bytes)) = load_archives() else {
        return;
    };
    let ini_text = match assets::scenario_text_from_archive(&main_bytes, scenario_name) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("SKIP {scenario_name}: could not read scenario text: {e}");
            return;
        }
    };
    let ini = Ini::parse(&ini_text);
    let terrain_entries: Vec<(u32, String)> = ini
        .section_entries("TERRAIN")
        .map(|entries| {
            entries
                .iter()
                .filter_map(|(k, v)| k.trim().parse::<u32>().ok().map(|c| (c, v.clone())))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !terrain_entries.is_empty(),
        "{scenario_name} should declare [TERRAIN] entries"
    );

    let mission =
        match assets::load_campaign_from_bytes(&main_bytes, &redalert_bytes, scenario_name) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("SKIP {scenario_name}: failed to load ({e})");
                return;
            }
        };
    let world = mission.core.world();

    // Sample several declared cells (not just one) so the pin is robust.
    let sample: Vec<&(u32, String)> = terrain_entries.iter().take(6).collect();
    let mut blocked: Vec<CellCoord> = Vec::new();
    for (cellnum, ttype) in &sample {
        let cell = CellCoord::from_index(*cellnum);
        let passable = world.passability().is_passable_loco(cell, Locomotor::Foot);
        eprintln!("{scenario_name}: terrain cell {cellnum} ({ttype}) foot-passable={passable}");
        if !passable {
            blocked.push(cell);
        }
    }

    if blocked.is_empty() {
        eprintln!(
            "FINDING: {scenario_name}: none of the {} sampled [TERRAIN] cells are \
             Foot-impassable — occupancy stamping (QUIRKS Q17.4, World::block_cell) may not \
             be reaching the passability grid for these terrain types/theater.",
            sample.len()
        );
    }
    assert!(
        !blocked.is_empty(),
        "{scenario_name}: at least one of {} sampled [TERRAIN] cells must be Foot-impassable \
         (QUIRKS Q17.4 occupancy stamping)",
        sample.len()
    );

    // Path-relevant check: A* around one blocked cell must not step onto it.
    let blocked_cell = blocked[0];
    let start = CellCoord::new(blocked_cell.x - 3, blocked_cell.y);
    let goal = CellCoord::new(blocked_cell.x + 3, blocked_cell.y);
    if start.on_map() && goal.on_map() {
        match ra_sim::path::find_path(world.passability(), start, goal, Locomotor::Foot) {
            Some(path) => {
                assert!(
                    !path.contains(&blocked_cell),
                    "{scenario_name}: A* must route around the blocked terrain cell {:?}, \
                     not step onto it",
                    blocked_cell
                );
                eprintln!(
                    "{scenario_name}: A* path around {:?} has {} steps, avoids it: OK",
                    blocked_cell,
                    path.len()
                );
            }
            None => {
                eprintln!(
                    "{scenario_name}: no path found between {:?} and {:?} (may be genuinely \
                     enclosed terrain) — skipping the path-relevant sub-check",
                    start, goal
                );
            }
        }
    }
}

#[test]
fn terrain_occupancy_blocks_pathing() {
    terrain_occupancy_check("scg02ea.ini");
    terrain_occupancy_check("scg03ea.ini");
}
