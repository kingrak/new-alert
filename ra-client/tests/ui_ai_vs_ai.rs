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
use ra_sim::{AiPlayer, AiProfile, Difficulty, Handicap, OreField, Passability, World};

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

// ===========================================================================
// Building-count utilities (M7.11 audit backfill — QUIRKS Q20's "building-
// runaway fix"): the rubber-band building cap `max(self, avg_enemy+10)`
// (house.cpp:5010) was a positive-feedback loop between two symmetric bases
// that, pre-fix, spammed hundreds of power plants and walled a base in (units
// could no longer path out to attack — an eternal stalemate). These utilities
// sample each house's live building count over the course of an AI-vs-AI run
// so a regression of that class shows up as a numeric bound violation, not
// only as "the game never resolves."
// ===========================================================================

/// Count of `house`'s currently-alive buildings.
fn building_count(world: &World, house: u8) -> usize {
    world
        .buildings
        .iter()
        .filter(|(_, b)| b.house == house && b.is_alive())
        .count()
}

/// [`drive_to_decisive`], additionally sampling both houses' live building
/// counts every `sample_every` ticks. Returns the per-house time series (one
/// entry per sample, oldest first) alongside the terminal `(tick, Outcome)` —
/// or `None` in that slot if the game never resolved within `max_ticks` (the
/// partial series is still returned).
fn drive_to_decisive_sampling(
    mut game: AiVsAiGame,
    max_ticks: u32,
    sample_every: u32,
) -> (Option<(u32, Outcome)>, Vec<usize>, Vec<usize>) {
    let mut series_a = Vec::new();
    let mut series_b = Vec::new();
    for t in 0..max_ticks {
        game.world.tick(&[]);
        if t % sample_every.max(1) == 0 {
            series_a.push(building_count(&game.world, game.house_a));
            series_b.push(building_count(&game.world, game.house_b));
        }
        let a_alive = game.world.house_alive(game.house_a);
        let b_alive = game.world.house_alive(game.house_b);
        match (a_alive, b_alive) {
            (true, false) => return (Some((t, Outcome::HouseAWins)), series_a, series_b),
            (false, true) => return (Some((t, Outcome::HouseBWins)), series_a, series_b),
            (false, false) => return (Some((t, Outcome::BothEliminated)), series_a, series_b),
            (true, true) => {}
        }
    }
    (None, series_a, series_b)
}

/// Progress-monotonicity helper (added for future AI-vs-AI regression tests,
/// not just the building-count-bounded pin below): true if `series`, viewed
/// through every trailing window of `window` consecutive samples, never
/// increases — i.e. no window's last sample exceeds its first. Intended use:
/// pin that a *losing* AI's building count (or any other "amount of stuff
/// left" metric) is on a one-way path down once its endgame has begun
/// (fire-sale/all-hunt, QUIRKS Q16), rather than fluctuating back upward,
/// which would indicate its production was not actually eliminated.
/// Vacuously true if `series` is shorter than `window`, or `window` is 0.
fn is_non_increasing_over_window(series: &[usize], window: usize) -> bool {
    if window == 0 || series.len() < window {
        return true;
    }
    series
        .windows(window)
        .all(|w| w.first().copied().unwrap_or(0) >= w.last().copied().unwrap_or(0))
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

/// **M7.14 audit P0 — new-vs-old A/B (retire the standing acceptance gap).**
/// M7.14 replaced the old fixed-priority AI *in place*, so "the ratio/IQ Expert AI
/// beats the pre-M7.14 fixed-ladder AI" was never proven. This is the honest
/// head-to-head: [`AiProfile::Expert`] (the shipping ratio-driven policy) vs
/// [`AiProfile::Legacy`] (the verbatim pre-M7.14 fixed ladder, git `9155fce^`), at
/// **equal handicap** (both Normal — no difficulty bias tips the scale), on both
/// real scenarios, in both orientations (which side is Expert is swapped, so each
/// policy plays from each start). Everything else — combat, movement, teams,
/// economic reflexes — is identical between the two profiles, so the race isolates
/// exactly the M7.14 delta: ratio-driven base composition + IQ-gated economy.
///
/// ## HONEST FINDING (reported, not tuned away)
///
/// **Expert does NOT reliably beat Legacy on these maps.** Measured record (equal
/// Normal handicap):
///
/// | scenario | Expert=A          | Expert=B          |
/// |----------|-------------------|-------------------|
/// | scg05ea  | B wins (Legacy)   | B wins (Expert)   |
/// | scm01ea  | B wins (Legacy)   | B wins (Expert)   |
///
/// **All four games are won by house B, whichever policy sits there** — the winner
/// is decided by the **starting seat**, not the AI policy. Each policy wins exactly
/// when it holds house B (2/4 apiece): a perfectly symmetric 1-1 on each scenario,
/// i.e. the profile has *no measurable effect* on the outcome. The two policies are
/// at **near-parity**: M7.14 was a *fidelity + self-limiting* change (matching the
/// original's ratio system, bounding base growth), sharing all combat/team/economy
/// code with the M7.10/M7.11-tuned Legacy — it did not make the AI raw-stronger, so
/// the map's start asymmetry dominates the sub-threshold policy difference.
///
/// Per the brief ("if Expert does NOT reliably beat Legacy, report it honestly, do
/// not tune the test to pass") this test therefore asserts only the **defensible,
/// true** invariants — every A/B game resolves *decisively* (both policies are
/// competent; no stall, no mutual annihilation) and Expert is **not strictly
/// dominated** (it does not lose all four) — and prints the full per-orientation
/// record for the report. It deliberately does **not** assert an Expert sweep,
/// because that would be false.
#[test]
fn real_expert_vs_legacy_ai_ab_record() {
    if !support::real_assets_available() {
        eprintln!("SKIP: real assets not found (Expert-vs-Legacy A/B)");
        return;
    }
    let dir = support::assets_dir();
    let main_bytes = std::fs::read(dir.join("main.mix")).expect("main.mix");
    let redalert_bytes = std::fs::read(dir.join("redalert.mix")).expect("redalert.mix");

    // Load a Normal-vs-Normal game, then reinstall the two houses' controllers with
    // the requested profiles (both stay Normal → equal handicap). `set_ai` re-copies
    // each house's Normal (neutral) handicap and MaxIQ, so the only difference
    // between the two runs is the base-building/economy profile.
    let run = |scenario: &str, prof_a: AiProfile, prof_b: AiProfile| -> Option<(u32, Outcome)> {
        let g = load_ai_vs_ai_from_bytes(
            &main_bytes,
            &redalert_bytes,
            scenario,
            CREDITS,
            Difficulty::Normal,
            Difficulty::Normal,
        )
        .unwrap_or_else(|e| panic!("failed to load Expert-vs-Legacy game: {e}"));
        let (mut world, house_a, house_b) = (g.world, g.house_a, g.house_b);
        world.set_ai(vec![
            AiPlayer::new(house_a, Difficulty::Normal).with_profile(prof_a),
            AiPlayer::new(house_b, Difficulty::Normal).with_profile(prof_b),
        ]);
        drive_to_decisive(
            AiVsAiGame {
                world,
                house_a,
                house_b,
            },
            MAX_TICKS,
        )
    };

    let mut expert_wins = 0u32;
    let mut games = 0u32;
    for scenario in ["scg05ea.ini", "scm01ea.ini"] {
        // Orientation 1: A=Expert, B=Legacy → Expert won iff HouseAWins.
        // Orientation 2: A=Legacy, B=Expert → Expert won iff HouseBWins.
        for (i, (pa, pb, expert_is_a)) in [
            (AiProfile::Expert, AiProfile::Legacy, true),
            (AiProfile::Legacy, AiProfile::Expert, false),
        ]
        .into_iter()
        .enumerate()
        {
            let (tick, outcome) = run(scenario, pa, pb).unwrap_or_else(|| {
                panic!(
                    "Expert-vs-Legacy on {scenario} (orientation {i}) never resolved within \
                     {MAX_TICKS} ticks (~45 min) — both policies are M7.10/M7.11-tuned to be \
                     decisive, so a stall here is a real regression"
                )
            });
            // A/B games must still be decisive (the P1 repair-throttle change must
            // not reintroduce a starvation stall / mutual annihilation).
            assert_ne!(
                outcome,
                Outcome::BothEliminated,
                "{scenario} orientation {i}: both houses eliminated same tick — not decisive"
            );
            let ew = matches!(
                (outcome, expert_is_a),
                (Outcome::HouseAWins, true) | (Outcome::HouseBWins, false)
            );
            expert_wins += ew as u32;
            games += 1;
            eprintln!(
                "EXPERT-vs-LEGACY {scenario} (Expert={}): resolved tick {tick} (~{:.1} min), \
                 outcome={outcome:?}, expert_won={ew}",
                if expert_is_a { "A" } else { "B" },
                tick as f64 / TICKS_PER_SEC as f64 / 60.0
            );
        }
    }
    eprintln!(
        "EXPERT-vs-LEGACY A/B summary: Expert won {expert_wins}/{games} games at equal handicap \
         (near-parity — the winner is start-seat-dominated, see the module doc comment)."
    );
    // Defensible, true invariant: Expert is not *strictly dominated* by the old AI
    // (it does not lose every game). It is NOT asserted to sweep — the honest
    // finding is near-parity, documented above.
    assert!(
        expert_wins >= 1,
        "Expert should not be strictly dominated by the Legacy AI (won {expert_wins}/{games})"
    );
}

// ===========================================================================
// Audit addendum (ra-tester, post-M7.9/M7.10): pin the exact difficulty
// handicap *values* the real `redalert.mix` rules.ini loads into
// `Catalog::econ.difficulty`, confirming both (a) the numbers match the real
// `[Easy]`/`[Normal]`/`[Difficult]` sections (extracted from the actual asset
// — ground truth, not the brief) and (b) the label -> section **inversion**
// QUIRKS Q15 documents: our `Difficulty::Hard` reads rules.ini's `[Easy]`
// (buffed) section, `Difficulty::Easy` reads `[Difficult]` (nerfed), and
// `Difficulty::Normal` is the untouched `[Normal]` (neutral).
//
// Real rules.ini values (verified via `radump extract redalert.mix rules.ini
// --in local.mix`):
//   [Easy]:      FirePower=1.2 Armor=1.2 ROF=.8  Groundspeed=1.2 Cost=.8  BuildTime=.8
//   [Normal]:    all 1.0
//   [Difficult]: FirePower=.8  Armor=.8  ROF=1.2 Groundspeed=.8  Cost=1.0 BuildTime=1.0
//
// Parsed as raw 16.16 via `ra_data::combat::parse_fixed_raw` (`.8` -> `52428`,
// `1.2` -> `78643`, `1.0`/`1` -> `65536`).
#[test]
fn real_difficulty_handicap_table_matches_rules_ini_with_the_documented_inversion() {
    if !support::real_assets_available() {
        eprintln!("SKIP: real assets not found (difficulty handicap table pin)");
        return;
    }
    const BIAS_08: i32 = 52428;
    const BIAS_12: i32 = 78643;
    const NEUTRAL: i32 = 65536;

    let dir = support::assets_dir();
    let main_bytes = std::fs::read(dir.join("main.mix")).expect("main.mix");
    let redalert_bytes = std::fs::read(dir.join("redalert.mix")).expect("redalert.mix");
    let game = load_ai_vs_ai_from_bytes(
        &main_bytes,
        &redalert_bytes,
        "scg05ea.ini",
        CREDITS,
        Difficulty::Normal,
        Difficulty::Normal,
    )
    .unwrap_or_else(|e| panic!("failed to load a game to inspect its catalog: {e}"));

    let table = game.world.catalog.econ.difficulty;

    // Difficulty::Easy (index 0) -> rules.ini `[Difficult]` (nerfed FirePower/
    // Armor/Groundspeed, buffed-vs-original ROF; Cost/BuildTime *unchanged*
    // from neutral in the real asset — confirmed, not assumed).
    let easy = table[Difficulty::Easy as usize];
    let want_easy = Handicap {
        firepower: BIAS_08,
        armor: BIAS_08,
        rof: BIAS_12,
        groundspeed: BIAS_08,
        cost: NEUTRAL,
        build_time: NEUTRAL,
    };
    assert_eq!(
        easy, want_easy,
        "Difficulty::Easy must load rules.ini's [Difficult] section exactly"
    );

    // Difficulty::Normal (index 1) -> rules.ini `[Normal]`, all neutral.
    let normal = table[Difficulty::Normal as usize];
    assert_eq!(
        normal,
        Handicap::default(),
        "Difficulty::Normal must be the all-1.0 neutral handicap"
    );

    // Difficulty::Hard (index 2) -> rules.ini `[Easy]` (the inversion: a "Hard"
    // AI opponent is the *player's* easy-mode buffs, QUIRKS Q15).
    let hard = table[Difficulty::Hard as usize];
    let want_hard = Handicap {
        firepower: BIAS_12,
        armor: BIAS_12,
        rof: BIAS_08,
        groundspeed: BIAS_12,
        cost: BIAS_08,
        build_time: BIAS_08,
    };
    assert_eq!(
        hard, want_hard,
        "Difficulty::Hard must load rules.ini's [Easy] section exactly (the inversion)"
    );

    // The inversion is total: Hard and Easy must not coincide on any field a
    // real rules.ini biases (a regression here would silently un-invert it).
    assert_ne!(hard, easy, "Hard and Easy handicaps must differ");
}

// ===========================================================================
// M7.11 audit backfill: building-count-bounded (QUIRKS Q20 spare-power-
// runaway regression pin) + a pure sanity check for the progress-
// monotonicity helper it exercises.
// ===========================================================================

/// Pure sanity check for [`is_non_increasing_over_window`] — no assets
/// needed, so this always runs and guards the helper itself before any
/// asset-gated test trusts it.
#[test]
fn non_increasing_window_helper_catches_an_uptick_and_ignores_short_series() {
    assert!(is_non_increasing_over_window(&[10, 9, 9, 7, 5], 3));
    // The 9 -> 11 uptick falls inside a window of 3: caught.
    assert!(!is_non_increasing_over_window(&[10, 9, 9, 11, 5], 3));
    // Same series, but the window is longer than the series: vacuously true.
    assert!(is_non_increasing_over_window(&[10, 9, 9, 11, 5], 10));
    assert!(is_non_increasing_over_window(&[], 3));
    assert!(is_non_increasing_over_window(&[5], 0));
    // A flat series never "increases".
    assert!(is_non_increasing_over_window(&[4, 4, 4, 4], 2));
}

/// M7.11 regression pin: the spare-power-runaway building cap bug (QUIRKS
/// Q20 — the `max(self, avg_enemy+10)` rubber-band positive-feedback loop
/// that, pre-fix, spammed hundreds of power plants and walled a base in) must
/// not come back. A symmetric (same-difficulty) AI-vs-AI game's live building
/// count, sampled throughout the run, must stay under a sane cap — generous
/// enough for any legitimate base, nowhere close to the "hundreds" the
/// pre-fix bug produced. Also exercises [`is_non_increasing_over_window`]
/// against the loser's tail: once a house is in its terminal collapse, its
/// building count should only go down.
#[test]
fn real_symmetric_ai_vs_ai_building_count_stays_bounded() {
    if !support::real_assets_available() {
        eprintln!("SKIP: real assets not found (building-count-bounded regression pin)");
        return;
    }
    // A real base in these scenarios peaks in the teens/twenties of
    // buildings (see the eprintln! below for observed peaks); 60 is a
    // generous cap that is nowhere near the "hundreds" the pre-fix rubber-
    // band bug produced, while still catching any reoccurrence early.
    const SANE_CAP: usize = 60;
    let dir = support::assets_dir();
    let main_bytes = std::fs::read(dir.join("main.mix")).expect("main.mix");
    let redalert_bytes = std::fs::read(dir.join("redalert.mix")).expect("redalert.mix");
    let game = load_ai_vs_ai_from_bytes(
        &main_bytes,
        &redalert_bytes,
        "scg05ea.ini",
        CREDITS,
        Difficulty::Hard,
        Difficulty::Hard,
    )
    .unwrap_or_else(|e| panic!("failed to load a symmetric Hard-vs-Hard game: {e}"));

    let (resolved, series_a, series_b) =
        drive_to_decisive_sampling(game, MAX_TICKS, TICKS_PER_SEC * 5);
    let (tick, outcome) = resolved.unwrap_or_else(|| {
        panic!("symmetric Hard-vs-Hard scg05ea never resolved within {MAX_TICKS} ticks")
    });
    let max_a = series_a.iter().copied().max().unwrap_or(0);
    let max_b = series_b.iter().copied().max().unwrap_or(0);
    eprintln!(
        "symmetric building-count run: resolved tick {tick} outcome={outcome:?}, peak building \
         counts A={max_a} B={max_b} (cap {SANE_CAP}, {} samples each)",
        series_a.len()
    );
    assert!(
        max_a <= SANE_CAP,
        "house A's building count peaked at {max_a}, exceeding the sane cap of {SANE_CAP} — \
         the M7.11 spare-power-runaway fix (QUIRKS Q20) may have regressed"
    );
    assert!(
        max_b <= SANE_CAP,
        "house B's building count peaked at {max_b}, exceeding the sane cap of {SANE_CAP} — \
         the M7.11 spare-power-runaway fix (QUIRKS Q20) may have regressed"
    );

    // Progress-monotonicity: the loser's building count should trend only
    // downward over its terminal collapse (the last few samples before
    // elimination), never tick back upward. Only the *tail* is checked, not
    // the whole run — early-game economy growth legitimately raises the
    // count, so `is_non_increasing_over_window` is applied with a pairwise
    // window (2) over just the trailing samples, not the full series.
    let loser_series = match outcome {
        Outcome::HouseAWins => &series_b,
        Outcome::HouseBWins => &series_a,
        Outcome::BothEliminated => return, // no single loser to check
    };
    let tail_len = loser_series.len().min(5);
    let tail = &loser_series[loser_series.len() - tail_len..];
    assert!(
        is_non_increasing_over_window(tail, 2),
        "the loser's building count should trend only downward across its terminal-collapse \
         tail (last {tail_len} samples before elimination), not tick back upward: {tail:?}"
    );
}
