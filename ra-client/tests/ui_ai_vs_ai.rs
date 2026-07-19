//! Real-map (skip-clean) AI-vs-AI coverage for M6's skirmish AI — the
//! headless half of "AI determinism + behavior" that needs an actual
//! scenario (real terrain, real rules.ini catalog, real house layout) rather
//! than the synthetic fixture in `ra-sim/tests/ai_suite.rs`.
//!
//! `assets::load_skirmish_from_bytes` always leaves one house
//! human-controlled (`SkirmishGame::player_house`), so it can't drive a fully
//! headless AI-vs-AI game. [`load_ai_vs_ai_from_bytes`] below is a near-copy
//! of it with exactly one change — both houses get an `AiPlayer` — returning
//! the raw `World` (no `AppCore`, no rendering) so the game can be driven
//! with `world.tick(&[])` alone. This mirrors the "duplicate a `pub fn`
//! loader, change one field" pattern `ui_economy_determinism.rs`'s
//! `load_econ_from_bytes_growth_disabled` already established.

mod support;

use ra_client::assets::{self, build_content, load_from_bytes};
use ra_data::house::{house_from_name, HOUSE_COUNT};
use ra_formats::ini::Ini;
use ra_formats::mix::MixArchive;
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{AiPlayer, Difficulty, OreField, Passability, World};

const CREDITS: i32 = 6000;
/// Sim tick rate (DESIGN.md): 15 ticks/second.
const TICKS_PER_SEC: u32 = 15;

struct AiVsAiGame {
    world: World,
    house_a: u8,
    house_b: u8,
}

/// A near-duplicate of `assets::load_skirmish_from_bytes`, with one
/// deliberate difference: BOTH houses get an [`AiPlayer`] instead of leaving
/// one human-controlled, so the returned `World` needs no `AppCore` and can
/// be driven headless. Uses only `pub fn`s of `ra-client`/`ra-sim`/
/// `ra-data`/`ra-formats` (no rendering/sprite/remap plumbing is needed for a
/// headless sim-only game, so that part of the original loader is dropped).
fn load_ai_vs_ai_from_bytes(
    main_bytes: &[u8],
    redalert_bytes: &[u8],
    scenario_name: &str,
    starting_credits: i32,
    diff_a: Difficulty,
    diff_b: Difficulty,
) -> Result<AiVsAiGame, Box<dyn std::error::Error>> {
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

    // House A / house B: the same "[Basic] Player, else a distinct second
    // house" rule `load_skirmish_from_bytes` uses to pick its (single) AI
    // house — except BOTH become AI-controlled here.
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
    // Ore growth on, matching the rules.ini stock default a real skirmish
    // boots with. `ore_growth_flags`/`ini_bool` (which read the actual
    // `OreGrows=`/`OreSpreads=` keys) are private to `ra_client::assets`, so
    // this duplicate hardcodes the stock default rather than reading it —
    // same limitation `ui_economy_determinism.rs` flags on its own loader
    // duplicate. Immaterial here: this suite doesn't assert anything about
    // ore growth, only that the AI-vs-AI game resolves.
    world.set_ore_growth(true, true);
    // Per-house difficulty (M7.9 P2a): `set_catalog` above carried the
    // difficulty stat-handicap table, and `set_ai` copies each house's handicap
    // onto it — so an asymmetric (Hard vs Easy) game genuinely handicaps the two
    // sides differently at the combat/movement/production sites.
    world.set_ai(vec![
        AiPlayer::new(house_a, diff_a),
        AiPlayer::new(house_b, diff_b),
    ]);

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

    Ok(AiVsAiGame {
        world,
        house_a,
        house_b,
    })
}

/// Two well-separated, **land-connected** starts for a headless AI-vs-AI
/// game: house A takes the scenario's first `[Waypoints]` multiplayer start
/// (every skirmish map defines several, for exactly this purpose); house B
/// takes the farthest *BFS-reachable* waypoint from A over passable terrain
/// (falling back to the farthest BFS-reachable cell at all if no other
/// waypoint qualifies), each snapped to a legal base cell via the public
/// `find_base_start`.
///
/// The BFS-connectivity requirement matters: an earlier version of this
/// helper picked the raw farthest-apart waypoint *pair* with no reachability
/// check, which on `scm01ea.ini` landed house B's start on a landmass that
/// house A's ground forces could not fully reach — a self-inflicted "naval
/// trap" (the exact failure mode `ra-client::assets::pick_two_starts`'s own
/// doc comment warns about), not an AI logic bug. This mirrors that private
/// helper's BFS-reachability guarantee using only public API
/// (`Passability::is_passable`), without needing access to it.
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

    // BFS the passable component connected to `a`.
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
            // No other waypoint is BFS-reachable: fall back to the farthest
            // reachable cell overall, guaranteeing the two starts are at
            // least on the same landmass.
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

/// A decisive outcome for a headless AI-vs-AI game, determined directly from
/// [`World::house_alive`] rather than [`World::game_over`]/`GameOver`.
///
/// **Why not `game_over()`:** its Victory branch is
/// `world.ai.iter().all(|a| !world.house_alive(a.house))` — every house
/// *registered as an AI* must be eliminated. That is correct for a normal
/// skirmish (`load_skirmish_from_bytes` only ever registers the *opponent* as
/// an `AiPlayer`; the tracked player house never appears in `world.ai`), but
/// this suite's [`load_ai_vs_ai_from_bytes`] registers **both** houses as
/// `AiPlayer`s. With house A set as `player_house` (so `Defeat` can still be
/// observed), house A's own entry in `world.ai` means the Victory branch can
/// never fire while house A is alive and winning — confirmed empirically: a
/// run where house B's MCV died at tick ~6900 (fully eliminated, 0 buildings)
/// left `game_over()` at `Ongoing` through the entire 20-simulated-minute
/// probe. This is a real coupling between `set_ai`/`set_player_house` worth
/// flagging (see the ra-tester report), not something this test suite works
/// around by calling into anything private — `house_alive` is public and is
/// exactly the primitive `game_over()` itself is built on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    HouseAWins,
    HouseBWins,
    BothEliminated,
}

/// Drive an AI-vs-AI game headlessly until one house is eliminated. Returns
/// `Some((tick, outcome))` the moment that happens, or `None` if neither
/// house is ever eliminated within `max_ticks`.
fn drive_to_decisive(mut game: AiVsAiGame, max_ticks: u32) -> Option<(u32, Outcome)> {
    for t in 0..max_ticks {
        game.world.tick(&[]);
        let a_alive = game.world.house_alive(game.house_a);
        let b_alive = game.world.house_alive(game.house_b);
        match (a_alive, b_alive) {
            (true, false) => return Some((t, Outcome::HouseAWins)),
            (false, true) => return Some((t, Outcome::HouseBWins)),
            (false, false) => return Some((t, Outcome::BothEliminated)),
            (true, true) => {}
        }
    }
    None
}

/// Run one scenario end to end at Hard-vs-Hard and assert it resolves.
fn assert_decisive(scenario: &str, max_ticks: u32) {
    assert_decisive_at(scenario, max_ticks, Difficulty::Hard);
}

/// Run one scenario end to end with both AIs at `difficulty` and assert it
/// reaches a clean decisive outcome within `max_ticks` (M7.10 acceptance:
/// AI-vs-AI must be decisive at *every* difficulty, not just Hard).
fn assert_decisive_at(scenario: &str, max_ticks: u32, difficulty: Difficulty) {
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
    let game = load_ai_vs_ai_from_bytes(
        &main_bytes,
        &redalert_bytes,
        scenario,
        CREDITS,
        difficulty,
        difficulty,
    )
    .unwrap_or_else(|e| panic!("{scenario}: failed to load an AI-vs-AI game: {e}"));
    let house_a = game.house_a;
    let house_b = game.house_b;

    let (tick, outcome) = drive_to_decisive(game, max_ticks).unwrap_or_else(|| {
        panic!(
            "{scenario}: AI-vs-AI game (house {house_a} vs house {house_b}) never reached a \
             decisive outcome within {max_ticks} ticks (~{:.1} sim-minutes) — either the AI \
             is stuck (never builds/attacks to a conclusion on this map) or the budget is too \
             small; this is a real finding either way, not a flaky test",
            max_ticks as f64 / TICKS_PER_SEC as f64 / 60.0
        )
    });
    eprintln!(
        "{scenario}: resolved at tick {tick} (~{:.1} sim-minutes), outcome={outcome:?}",
        tick as f64 / TICKS_PER_SEC as f64 / 60.0
    );
    assert_ne!(
        outcome,
        Outcome::BothEliminated,
        "{scenario}: both houses were eliminated on the same tick ({tick}) — not a clean \
         decisive outcome"
    );
}

/// Generous 45-sim-minute safety-cap budget (15 ticks/s per DESIGN.md). Two
/// Hard AIs (shortest attack cadence, smallest force requirement) on a real
/// map with real build costs. `drive_to_decisive` returns as soon as the game
/// resolves, so a larger cap costs nothing in the (normal) case where the
/// game resolves well under it -- observed resolving ticks in local runs were
/// ~11.3k (scg05ea, ~12.5 sim-min) and ~22.0k (scm01ea, ~24.5 sim-min); see
/// the ra-tester report for the full story (this budget only converges at
/// all after a same-session ai.rs fix -- see that module's `step` doc
/// comment).
const MAX_TICKS: u32 = 45 * 60 * TICKS_PER_SEC;

#[test]
fn real_scg05ea_ai_vs_ai_reaches_a_decisive_outcome() {
    assert_decisive("scg05ea.ini", MAX_TICKS);
}

#[test]
fn real_scm01ea_ai_vs_ai_reaches_a_decisive_outcome() {
    assert_decisive("scm01ea.ini", MAX_TICKS);
}

/// M7.10 acceptance — decisive at **every** difficulty (not just Hard), on
/// `scg05ea` (the faster-resolving of the two maps). Easy-vs-Easy and
/// Normal-vs-Normal must both reach a clean win, not stall.
#[test]
fn real_scg05ea_ai_vs_ai_decisive_at_easy() {
    assert_decisive_at("scg05ea.ini", MAX_TICKS, Difficulty::Easy);
}

#[test]
fn real_scg05ea_ai_vs_ai_decisive_at_normal() {
    assert_decisive_at("scg05ea.ini", MAX_TICKS, Difficulty::Normal);
}

/// **M7.9 P2a showcase — Hard must reliably beat Easy.** With the difficulty
/// stat handicaps wired in (firepower/armor/ROF/groundspeed/cost/build-time,
/// house-scoped from rules.ini's `[Easy]/[Difficult]` sections), a `Hard` AI
/// out-damages, out-produces and out-manoeuvres an `Easy` one. To prove the
/// *difficulty* decides it — not the map's start positions — we run the **same
/// map twice with the sides swapped** and require the Hard house to win **both**
/// times. "Reliably" = start-independent.
#[test]
fn real_hard_ai_reliably_beats_easy_ai() {
    if !support::real_assets_available() {
        eprintln!("SKIP: real assets not found (Hard-vs-Easy showcase)");
        return;
    }
    let dir = support::assets_dir();
    let main_bytes = std::fs::read(dir.join("main.mix")).expect("main.mix");
    let redalert_bytes = std::fs::read(dir.join("redalert.mix")).expect("redalert.mix");
    let scenario = "scg05ea.ini";

    // Orientation 1: house A = Hard, house B = Easy → expect A wins.
    // Orientation 2: house A = Easy, house B = Hard → expect B wins.
    for (diff_a, diff_b, hard_is_a) in [
        (Difficulty::Hard, Difficulty::Easy, true),
        (Difficulty::Easy, Difficulty::Hard, false),
    ] {
        let game = load_ai_vs_ai_from_bytes(
            &main_bytes,
            &redalert_bytes,
            scenario,
            CREDITS,
            diff_a,
            diff_b,
        )
        .unwrap_or_else(|e| panic!("failed to load Hard-vs-Easy game: {e}"));

        let (tick, outcome) = drive_to_decisive(game, MAX_TICKS).unwrap_or_else(|| {
            panic!("Hard-vs-Easy on {scenario} never resolved within {MAX_TICKS} ticks")
        });
        let hard_won = matches!(
            (outcome, hard_is_a),
            (Outcome::HouseAWins, true) | (Outcome::HouseBWins, false)
        );
        eprintln!(
            "Hard-vs-Easy ({}): resolved at tick {tick} (~{:.1} min), outcome={outcome:?}, hard_won={hard_won}",
            if hard_is_a { "Hard=A" } else { "Hard=B" },
            tick as f64 / TICKS_PER_SEC as f64 / 60.0
        );
        assert!(
            hard_won,
            "the Hard AI must beat the Easy AI (orientation hard_is_a={hard_is_a}, outcome={outcome:?}) \
             — difficulty stat handicaps should decide it regardless of start position"
        );
    }
}
