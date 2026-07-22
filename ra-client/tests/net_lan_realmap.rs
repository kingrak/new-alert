//! M8-B proof test (b, real-map half): the M8-A real-scenario AI-vs-AI
//! lockstep match (`net_lockstep_realmap.rs`) re-run with the in-process
//! `PairTransport` swapped for two real **UDP** `LanTransport` endpoints on
//! 127.0.0.1 (OS-assigned ports — never fixed). Same architecture: each
//! instance owns its own `World` and steps ONLY its own house's `AiPlayer`
//! externally; the only way either house acts in either world is via
//! commands that crossed the socket with the input-delay stamp
//! (QUEUE.CPP:2526). Hash chains must stay identical the whole way.
//!
//! Budgets per the M7.20 lesson: bounded ticks + a hard wall-clock guard,
//! with a minimum-progress floor so the run cannot silently go vacuous.

mod support;

use std::net::UdpSocket;
use std::time::Instant;

use ra_client::assets::{self, build_content, load_from_bytes};
use ra_data::house::{house_from_name, HOUSE_COUNT};
use ra_formats::ini::Ini;
use ra_formats::mix::MixArchive;
use ra_net::{CommandTransport, LanTransport, PollResult, DEFAULT_INPUT_DELAY};
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{AiPlayer, Command, Difficulty, OreField, Passability, RandomLcg, World};

const CREDITS: i32 = 6000;
const SCENARIO: &str = "scg05ea.ini";
/// Bounded tick budget: 4 sim-minutes at 15 ticks/s — both AIs deploy,
/// build up, and produce/move units, all through real sockets. (Half the
/// PairTransport run's budget: that test already proves the long haul; this
/// one proves the socketed medium.)
const MAX_TICKS: u32 = 3600;
/// Hard wall-clock guard: bail with the identity already proven for every
/// completed tick rather than hang.
const WALL_SECS: u64 = 180;
const MIN_TICKS: u32 = 600;

struct LockstepGame {
    world: World,
    house_a: u8,
    house_b: u8,
}

/// Verbatim from `net_lockstep_realmap.rs` (the established loader-copy
/// pattern): a controller-free world — no `set_ai`, no designated player.
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

    let mcv_proto = content.catalog.units[0].clone();
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

/// Verbatim from `net_lockstep_realmap.rs` / `ui_ai_vs_ai.rs`.
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

/// Two directly-connected UDP endpoints on loopback, OS-assigned ports.
fn udp_pair(seat_a: u8, seat_b: u8) -> (LanTransport, LanTransport) {
    let sa = UdpSocket::bind("127.0.0.1:0").expect("bind");
    let sb = UdpSocket::bind("127.0.0.1:0").expect("bind");
    let aa = sa.local_addr().unwrap();
    let ab = sb.local_addr().unwrap();
    let ta = LanTransport::new(sa, ab, seat_a, seat_b, DEFAULT_INPUT_DELAY, true).unwrap();
    let tb = LanTransport::new(sb, aa, seat_b, seat_a, DEFAULT_INPUT_DELAY, false).unwrap();
    (ta, tb)
}

/// Proof (b, real map): the full lockstep protocol on a real scenario over
/// real UDP sockets — per-tick hash-chain identity between the instances.
#[test]
fn real_scenario_ai_vs_ai_lan_lockstep_hash_identical() {
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

    let mut ai_a = AiPlayer::new(house_a, Difficulty::Normal);
    let mut ai_b = AiPlayer::new(house_b, Difficulty::Normal);
    let mut rng_a = RandomLcg::new(0xA11C_E501);
    let mut rng_b = RandomLcg::new(0xB0B5_1DE5);

    let (mut tp_a, mut tp_b) = udp_pair(house_a, house_b);

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

        let mut bundle_a = None;
        let mut bundle_b = None;
        let mut spins = 0u32;
        while bundle_a.is_none() || bundle_b.is_none() {
            if bundle_a.is_none() {
                match tp_a.poll(t) {
                    PollResult::Ready(x) => bundle_a = Some(x),
                    PollResult::Waiting => {}
                    PollResult::Desync(d) => panic!("instance A desynced: {d:?}"),
                    PollResult::ConnectionLost(l) => panic!("instance A lost its peer: {l:?}"),
                }
            } else {
                tp_a.service();
            }
            if bundle_b.is_none() {
                match tp_b.poll(t) {
                    PollResult::Ready(x) => bundle_b = Some(x),
                    PollResult::Waiting => {}
                    PollResult::Desync(d) => panic!("instance B desynced: {d:?}"),
                    PollResult::ConnectionLost(l) => panic!("instance B lost its peer: {l:?}"),
                }
            } else {
                tp_b.service();
            }
            spins += 1;
            assert!(spins < 500_000, "lockstep deadlock at tick {t}");
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
        "{SCENARIO}: {ticks_run} LAN lockstep ticks in {:.1}s; {submitted_a} cmds from A(house {house_a}), \
         {submitted_b} from B(house {house_b}); buildings A={}, B={}; decode errors {}+{}",
        start.elapsed().as_secs_f64(),
        building_count(&world_a, house_a),
        building_count(&world_a, house_b),
        tp_a.decode_errors(),
        tp_b.decode_errors(),
    );

    // Non-vacuity (M7.19): the game must have actually happened, through the
    // sockets.
    assert!(
        ticks_run >= MIN_TICKS,
        "only {ticks_run} ticks completed before the wall-clock guard — too little to be a \
         meaningful proof (budget/hardware problem, not a determinism verdict)"
    );
    assert!(
        submitted_a > 10 && submitted_b > 10,
        "AIs submitted too few commands (A={submitted_a}, B={submitted_b}) — commands did not flow"
    );
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
    assert_eq!(
        tp_a.decode_errors(),
        0,
        "clean sockets must decode all traffic"
    );
    assert_eq!(tp_b.decode_errors(), 0);
    assert!(tp_a.desync().is_none() && tp_b.desync().is_none());
    assert!(tp_a.connection_lost().is_none() && tp_b.connection_lost().is_none());
}

/// M8-C P2a — **real-map mid-game snapshot round-trip.** Load an actual
/// scenario, install two skirmish AIs so a genuine mid-game develops (buildings,
/// production, units, combat, ore growth, shroud — the full state a snapshot must
/// capture), then `save_snapshot` → `load_snapshot` and prove the loaded world
/// produces the byte-identical hash chain for 200 further ticks. Skips cleanly
/// when the real assets are absent.
#[test]
fn realmap_midgame_snapshot_roundtrip() {
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

    let game = load_headless_world(&main_bytes, &redalert_bytes, SCENARIO, CREDITS)
        .expect("headless world should load");
    let mut world = game.world;
    world.set_player_house(game.house_a);
    world.set_ai(vec![
        AiPlayer::new(game.house_a, Difficulty::Normal),
        AiPlayer::new(game.house_b, Difficulty::Normal),
    ]);

    // Advance to a rich mid-game state.
    let warmup = 400u32;
    for _ in 0..warmup {
        world.tick(&[]);
    }

    // Round-trip the snapshot against this peer's own shared catalog + map.
    let bytes = world.save_snapshot();
    eprintln!(
        "realmap mid-game snapshot: {} bytes at tick {}",
        bytes.len(),
        world.tick_count()
    );
    let mut loaded =
        World::load_snapshot(&bytes, world.catalog().clone(), world.passability().clone())
            .expect("snapshot must load");
    assert_eq!(
        world.state_hash(),
        loaded.state_hash(),
        "loaded real-map world must hash-match at the snapshot tick"
    );

    // Hash-chain identity for 200 further ticks (AI keeps running on both).
    let mut chain = Vec::new();
    for t in 0..200u32 {
        let h0 = world.tick(&[]);
        let h1 = loaded.tick(&[]);
        assert_eq!(
            h0, h1,
            "real-map hash chain diverged {t} ticks after resume"
        );
        chain.push(h0);
    }
    let distinct: std::collections::BTreeSet<u64> = chain.iter().copied().collect();
    assert!(
        distinct.len() > 50,
        "post-snapshot chain suspiciously static ({} distinct of 200)",
        distinct.len()
    );
}
