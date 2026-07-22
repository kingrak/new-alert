//! M8-A proof test (c): real-scenario lockstep — an AI-vs-AI match on a real
//! map (`scg05ea.ini`, real terrain/rules/houses) where each AI's commands
//! flow through `ra-net`'s [`PairTransport`] *as if it were a remote player*,
//! and the two independent game instances must stay hash-chain-identical the
//! whole way.
//!
//! Architecture under test (DESIGN.md §4.6): each instance owns its own
//! `World` (built from the same bytes → byte-identical start) and steps ONLY
//! its own house's `AiPlayer` — externally, against its local world, with its
//! own command RNG — submitting the resulting commands through the transport.
//! Neither world has an installed (`set_ai`) controller, so the *only* way
//! either house acts in either world is via commands that crossed the
//! lockstep pipeline with the input-delay stamp (queue.cpp:2526). This is
//! exactly the remote-player shape M8-B's LAN transport will drive.
//!
//! The world loader is a near-copy of `ui_ai_vs_ai.rs`'s
//! `load_ai_vs_ai_from_bytes` (itself the established "duplicate a `pub fn`
//! loader, change one field" pattern) with one deliberate difference: no
//! `world.set_ai(..)` — both controllers live outside the worlds.
//!
//! Budgets follow the M7.20 lesson: bounded tick budget plus a hard
//! wall-clock guard so this can never hang a suite run; non-vacuity asserts
//! (M7.19 lesson) prove commands flowed and bases actually grew before the
//! identity claim counts.

mod support;

use std::time::Instant;

use ra_client::assets::{self, build_content, load_from_bytes};
use ra_data::house::{house_from_name, HOUSE_COUNT};
use ra_formats::ini::Ini;
use ra_formats::mix::MixArchive;
use ra_net::{CommandTransport, PairTransport, PollResult, DEFAULT_INPUT_DELAY};
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{AiPlayer, Command, Difficulty, OreField, Passability, RandomLcg, World};

const CREDITS: i32 = 6000;
const SCENARIO: &str = "scg05ea.ini";
/// Bounded tick budget: 8 sim-minutes at 15 ticks/s — enough for both AIs to
/// deploy their MCVs, build up a base, and produce and move units, all
/// through the transport (measured ~4s of wall time per 1800 ticks in debug).
const MAX_TICKS: u32 = 7200;
/// Hard wall-clock guard (M7.20): bail — with the identity already proven for
/// every completed tick — rather than hang, but require enough progress that
/// the run cannot silently become vacuous.
const WALL_SECS: u64 = 240;
const MIN_TICKS: u32 = 600;

struct LockstepGame {
    world: World,
    house_a: u8,
    house_b: u8,
}

/// Near-copy of `ui_ai_vs_ai.rs::load_ai_vs_ai_from_bytes`, with one
/// deliberate difference: **no `set_ai`** — the returned `World` contains no
/// controllers at all, so it only ever changes through `tick(&commands)`.
fn load_headless_world(
    main_bytes: &[u8],
    redalert_bytes: &[u8],
    scenario_name: &str,
    starting_credits: i32,
) -> Result<LockstepGame, Box<dyn std::error::Error>> {
    let main = MixArchive::parse(main_bytes)?;
    let redalert = MixArchive::parse(redalert_bytes)?;

    let loaded = load_from_bytes(main_bytes, redalert_bytes, scenario_name)?;
    let scenario = loaded.scenario;

    let general = main.open_nested("general.mix")?;
    let ini_bytes = general
        .get(scenario_name)
        .ok_or_else(|| format!("scenario '{scenario_name}' not found"))?;
    let scen_ini = Ini::parse(&String::from_utf8_lossy(ini_bytes));

    let local = redalert.open_nested("local.mix")?;
    let rules = Ini::parse(&String::from_utf8_lossy(
        local.get("rules.ini").ok_or("rules.ini not found")?,
    ));

    let house_a = scen_ini
        .get("Basic", "Player")
        .and_then(house_from_name)
        .unwrap_or(1);
    let house_b = if house_a == 2 { 0 } else { 2 };

    let conquer = main.open_nested("conquer.mix")?;
    let content = build_content(&rules, &conquer, None)?;

    let passable = ra_data::passability::build(&scenario);
    let grid = Passability::new(128, 128, passable);
    let mut world = World::new(grid, 0x1234_5678);
    world.set_catalog(content.catalog.clone());
    world.init_houses(HOUSE_COUNT, starting_credits);
    world.set_ore(OreField::from_overlay(128, 128, &scenario.overlay));
    world.enable_shroud();
    world.set_ore_growth(true, true);

    let (start_a, start_b) = two_starts(&world, &scen_ini);

    let mcv_proto = content.catalog.units[0].clone(); // U_MCV, per assets.rs's own convention
    let spawn_mcv = |world: &mut World, house: u8, cell: CellCoord| {
        let h = world.spawn_unit(
            mcv_proto.sprite_id,
            house,
            cell,
            Facing(0),
            mcv_proto.max_health,
            mcv_proto.stats,
        );
        world.set_unit_max_health(h, mcv_proto.max_health);
        world.set_unit_combat(h, mcv_proto.armor, mcv_proto.weapon, mcv_proto.has_turret);
        world.set_unit_sight(h, mcv_proto.sight);
    };
    spawn_mcv(&mut world, house_a, start_a);
    spawn_mcv(&mut world, house_b, start_b);

    Ok(LockstepGame {
        world,
        house_a,
        house_b,
    })
}

/// Two well-separated, land-connected starts (verbatim from `ui_ai_vs_ai.rs`,
/// see its doc comment for the BFS-reachability rationale).
fn two_starts(world: &World, scen_ini: &Ini) -> (CellCoord, CellCoord) {
    let mut waypoints: Vec<CellCoord> = scen_ini
        .section_entries("Waypoints")
        .map(|e| {
            let mut v: Vec<(u32, u32)> = e
                .iter()
                .filter_map(|(k, v)| Some((k.parse::<u32>().ok()?, v.parse::<u32>().ok()?)))
                .filter(|(idx, _)| *idx < 8)
                .collect();
            v.sort_by_key(|(idx, _)| *idx);
            v.into_iter()
                .map(|(_, cell)| CellCoord::from_index(cell))
                .collect()
        })
        .unwrap_or_default();
    waypoints.dedup();

    let passable = world.passability();
    let ore = &world.ore;
    let (w, h) = (passable.width(), passable.height());

    let a_seed = waypoints
        .first()
        .copied()
        .unwrap_or_else(|| CellCoord::new(w / 2, h / 2));
    let a = assets::find_base_start(passable, ore, a_seed).0;

    let idx = |c: CellCoord| (c.y * w + c.x) as usize;
    let mut seen = vec![false; (w * h) as usize];
    let mut queue = std::collections::VecDeque::new();
    if passable.is_passable(a) {
        seen[idx(a)] = true;
        queue.push_back(a);
    }
    while let Some(c) = queue.pop_front() {
        for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            let n = CellCoord::new(c.x + dx, c.y + dy);
            if n.x < 0 || n.y < 0 || n.x >= w || n.y >= h {
                continue;
            }
            if !seen[idx(n)] && passable.is_passable(n) {
                seen[idx(n)] = true;
                queue.push_back(n);
            }
        }
    }

    let key = |c: CellCoord| -> i64 {
        let dx = (c.x - a.x) as i64;
        let dy = (c.y - a.y) as i64;
        dx * dx + dy * dy
    };
    let b_seed = waypoints
        .iter()
        .copied()
        .filter(|&c| c.x >= 0 && c.y >= 0 && c.x < w && c.y < h && seen[idx(c)])
        .max_by_key(|&c| key(c));
    let b = match b_seed {
        Some(c) => assets::find_base_start(passable, ore, c).0,
        None => {
            let mut best: Option<CellCoord> = None;
            for y in 0..h {
                for x in 0..w {
                    let c = CellCoord::new(x, y);
                    if seen[idx(c)] && best.map(|b| key(c) > key(b)).unwrap_or(true) {
                        best = Some(c);
                    }
                }
            }
            best.map(|c| assets::find_base_start(passable, ore, c).0)
                .unwrap_or(a)
        }
    };
    (a, b)
}

fn building_count(world: &World, house: u8) -> usize {
    world
        .buildings
        .iter()
        .filter(|(_, b)| b.house == house && b.is_alive())
        .count()
}

/// Proof test (c): full lockstep protocol on a real scenario, both AIs as
/// remote players, per-tick hash-chain identity between the two instances.
#[test]
fn real_scenario_ai_vs_ai_lockstep_hash_identical() {
    if !support::real_assets_available() {
        eprintln!(
            "SKIP: real assets not found under {} (set RA_ASSETS_DIR or copy \
             main.mix/redalert.mix into assets/ to run this test)",
            support::assets_dir().display()
        );
        return;
    }
    let dir = support::assets_dir();
    let main_bytes = std::fs::read(dir.join("main.mix")).expect("main.mix should read");
    let redalert_bytes = std::fs::read(dir.join("redalert.mix")).expect("redalert.mix should read");

    // Two independent instances from the same bytes: byte-identical starts.
    let ga = load_headless_world(&main_bytes, &redalert_bytes, SCENARIO, CREDITS)
        .unwrap_or_else(|e| panic!("{SCENARIO}: failed to load instance A: {e}"));
    let gb = load_headless_world(&main_bytes, &redalert_bytes, SCENARIO, CREDITS)
        .unwrap_or_else(|e| panic!("{SCENARIO}: failed to load instance B: {e}"));
    let (house_a, house_b) = (ga.house_a, ga.house_b);
    let mut world_a = ga.world;
    let mut world_b = gb.world;
    assert_eq!(
        world_a.state_hash(),
        world_b.state_hash(),
        "instances must start byte-identical"
    );

    // Each instance runs ONLY its own house's controller, externally, with its
    // own command RNG (the remote-player shape; the sim RNG inside each world
    // is never touched by the controllers).
    let mut ai_a = AiPlayer::new(house_a, Difficulty::Normal);
    let mut ai_b = AiPlayer::new(house_b, Difficulty::Normal);
    let mut rng_a = RandomLcg::new(0xA11C_E501);
    let mut rng_b = RandomLcg::new(0xB0B5_1DE5);

    let (mut tp_a, mut tp_b) = PairTransport::pair(house_a, house_b, DEFAULT_INPUT_DELAY, None);

    let start = Instant::now();
    let mut submitted_a = 0u64;
    let mut submitted_b = 0u64;
    let mut chain: Vec<u64> = Vec::new();
    let mut cmds: Vec<Command> = Vec::new();
    let mut ticks_run = 0u32;
    for t in 0..MAX_TICKS {
        if start.elapsed().as_secs() >= WALL_SECS {
            eprintln!(
                "[guard] wall-clock guard hit at tick {t} — bailing (identity proven so far)"
            );
            break;
        }
        // Local input generation, per instance, against the local world only.
        cmds.clear();
        ai_a.step(&world_a, &mut rng_a, &mut cmds);
        submitted_a += cmds.len() as u64;
        for &c in &cmds {
            tp_a.submit(c);
        }
        cmds.clear();
        ai_b.step(&world_b, &mut rng_b, &mut cmds);
        submitted_b += cmds.len() as u64;
        for &c in &cmds {
            tp_b.submit(c);
        }

        // Lockstep: poll both endpoints to Ready (clean link — no stalls
        // expected beyond the barrier's own bookkeeping), apply, exchange
        // hashes.
        let mut bundle_a = None;
        let mut bundle_b = None;
        let mut spins = 0u32;
        while bundle_a.is_none() || bundle_b.is_none() {
            if bundle_a.is_none() {
                match tp_a.poll(t) {
                    PollResult::Ready(x) => bundle_a = Some(x),
                    PollResult::Waiting => {}
                    PollResult::Desync(d) => panic!("instance A desynced: {d:?}"),
                }
            }
            if bundle_b.is_none() {
                match tp_b.poll(t) {
                    PollResult::Ready(x) => bundle_b = Some(x),
                    PollResult::Waiting => {}
                    PollResult::Desync(d) => panic!("instance B desynced: {d:?}"),
                }
            }
            spins += 1;
            assert!(spins < 100_000, "lockstep deadlock at tick {t}");
        }
        let bundle_a = bundle_a.unwrap();
        let bundle_b = bundle_b.unwrap();
        assert_eq!(bundle_a, bundle_b, "bundle mismatch at tick {t}");

        let ha = world_a.tick(&bundle_a.flatten());
        let hb = world_b.tick(&bundle_b.flatten());
        assert_eq!(
            ha, hb,
            "hash chains diverged at tick {t} (after {submitted_a}+{submitted_b} commands)"
        );
        tp_a.report_hash(t, ha);
        tp_b.report_hash(t, hb);
        chain.push(ha);
        ticks_run = t + 1;
    }

    eprintln!(
        "{SCENARIO}: {ticks_run} lockstep ticks in {:.1}s; {submitted_a} cmds from A(house {house_a}), \
         {submitted_b} from B(house {house_b}); buildings A={}, B={}",
        start.elapsed().as_secs_f64(),
        building_count(&world_a, house_a),
        building_count(&world_a, house_b),
    );

    // Non-vacuity (M7.19): the game must have actually happened.
    assert!(
        ticks_run >= MIN_TICKS,
        "only {ticks_run} ticks completed before the wall-clock guard — too little to be a \
         meaningful proof (budget/hardware problem, not a determinism verdict)"
    );
    assert!(
        submitted_a > 10 && submitted_b > 10,
        "AIs submitted too few commands (A={submitted_a}, B={submitted_b}) — commands did not flow"
    );
    // Both houses must have deployed and built: base growth visible in BOTH
    // worlds (they are hash-identical, but check each independently anyway).
    for (name, w) in [("A", &world_a), ("B", &world_b)] {
        assert!(
            building_count(w, house_a) >= 2 && building_count(w, house_b) >= 2,
            "instance {name}: bases did not grow (house {house_a}: {}, house {house_b}: {}) — \
             the AIs' commands cannot have applied",
            building_count(w, house_a),
            building_count(w, house_b),
        );
    }
    let distinct: std::collections::BTreeSet<u64> = chain.iter().copied().collect();
    assert!(
        distinct.len() > 100,
        "hash chain suspiciously static ({} distinct of {})",
        distinct.len(),
        chain.len()
    );
    assert!(tp_a.desync().is_none() && tp_b.desync().is_none());
}
