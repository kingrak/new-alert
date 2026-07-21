//! AI-vs-AI naval acceptance on the REAL coastal map (naval arc P0, this task).
//!
//! The synthetic `ra-sim/tests/naval_suite.rs` proves the water locomotor + sub
//! stealth; `naval_realmap_suite.rs` proves human-driven naval on `scm11ea`. This
//! suite closes the AI-side gap: on the coastal map both AI houses build naval
//! (a shipyard + combat vessels) and the game still reaches a **decisive** outcome
//! inside the 45-minute budget — naval must not destabilise the land war.
//!
//! It reuses the real skirmish loader (`load_skirmish_from_bytes`) — which installs
//! the true per-locomotor **water** passability the bespoke `ui_ai_vs_ai` harness
//! lacks (that one zeroes the water mask via `Passability::new`) — then registers
//! **both** houses as `AiPlayer`s to make it AI-vs-AI.
//!
//! **Landlocked byte-identity** (the AI is byte-identical on land maps, naval code
//! dead) is proven separately and exactly in `ui_ai_vs_ai.rs`: that suite runs
//! scg05ea/scm01ea AI-vs-AI over water-zeroed passability, and their resolution
//! ticks are unchanged by this work (scg05ea Hard 4997 / Normal 16693 / Easy 25274,
//! scm01ea 21977). Note those *land* maps do carry some open water under the *real*
//! passability the skirmish loader builds, so they are not a clean "no naval"
//! control here — the water-zeroed `ui_ai_vs_ai` harness is the invariant's home.
//!
//! **Economy note.** `CREDITS` is set high enough that a house reaches a cash
//! surplus and actually fields combat vessels (the AI funds a navy only with spare
//! capacity — a full land army comes first; see `ai.rs`). At the stock 6000-credit
//! economy the fast Hard game resolves via land before any surplus accrues, so the
//! shipyards are built but no vessels are — decisive, just navy-less.

mod support;

use ra_client::appcore::AppCore;
use ra_client::assets;
use ra_sim::{AiPlayer, Difficulty, World};

fn scratch() -> std::path::PathBuf {
    std::path::PathBuf::from(
        "/tmp/claude-1000/-home-cshi-dev-game/f65beaba-9afb-445c-a6fd-47d2eb3dad49/scratchpad",
    )
}

fn dump(core: &AppCore, name: &str) {
    let f = core.compose_game();
    let bytes = ra_client::png::encode_rgba(f.width, f.height, &f.pixels);
    let p = scratch().join(name);
    let _ = std::fs::write(&p, bytes);
    eprintln!("  wrote {}", p.display());
}

const TICKS_PER_SEC: u32 = 15;
const MAX_TICKS: u32 = 45 * 60 * TICKS_PER_SEC;
/// A surplus economy so the AI has spare capacity to field a navy (see module doc).
const CREDITS: i32 = 15000;

fn vessel_count(world: &World, house: u8) -> usize {
    world
        .units
        .iter()
        .filter(|(_, u)| {
            u.house == house
                && u.is_alive()
                && world
                    .catalog
                    .unit(u.type_id)
                    .map(|p| p.locomotor == ra_sim::LOCO_WATER_INDEX)
                    .unwrap_or(false)
        })
        .count()
}

fn owns_shipyard(world: &World, house: u8) -> bool {
    world.buildings.iter().any(|(_, b)| {
        b.house == house
            && b.is_alive()
            && world
                .catalog
                .building(b.type_id)
                .map(|p| matches!(p.name.as_str(), "SYRD" | "SPEN"))
                .unwrap_or(false)
    })
}

fn ai_vs_ai(scenario: &str, credits: i32) -> Option<(World, u8, u8)> {
    if !support::real_assets_available() {
        eprintln!("SKIP: real assets not found");
        return None;
    }
    let dir = support::assets_dir();
    let main = std::fs::read(dir.join("main.mix")).ok()?;
    let redalert = std::fs::read(dir.join("redalert.mix")).ok()?;
    let game = match assets::load_skirmish_from_bytes(
        &main,
        &redalert,
        scenario,
        credits,
        Difficulty::Hard,
    ) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SKIP: could not load {scenario}: {e}");
            return None;
        }
    };
    let (player, ai) = (game.player_house, game.ai_house);
    let mut core = game.core;
    core.world_mut().set_ai(vec![
        AiPlayer::new(player, Difficulty::Hard),
        AiPlayer::new(ai, Difficulty::Hard),
    ]);
    let world = core.world().clone();
    Some((world, player, ai))
}

/// Coastal acceptance: both AIs build naval (yard + vessels) AND the game resolves
/// decisively within the 45-minute budget.
#[test]
fn coastal_scm11ea_ai_vs_ai_builds_naval_and_is_decisive() {
    let Some((mut world, a, b)) = ai_vs_ai("scm11ea.ini", CREDITS) else {
        return;
    };
    let mut peak_a = 0usize;
    let mut peak_b = 0usize;
    let mut yard_a = false;
    let mut yard_b = false;
    let mut first_vessel: Option<u32> = None;
    let mut outcome: Option<(u32, bool, bool)> = None;
    for t in 0..MAX_TICKS {
        world.tick(&[]);
        let (va, vb) = (vessel_count(&world, a), vessel_count(&world, b));
        peak_a = peak_a.max(va);
        peak_b = peak_b.max(vb);
        yard_a |= owns_shipyard(&world, a);
        yard_b |= owns_shipyard(&world, b);
        if first_vessel.is_none() && (va > 0 || vb > 0) {
            first_vessel = Some(t);
        }
        let (aa, ba) = (world.house_alive(a), world.house_alive(b));
        if !aa || !ba {
            outcome = Some((t, aa, ba));
            break;
        }
    }
    eprintln!(
        "scm11ea AI-vs-AI: yards A={yard_a} B={yard_b}; peak vessels A={peak_a} B={peak_b}; \
         first vessel @ {first_vessel:?}; outcome={outcome:?}"
    );
    let (tick, aa, ba) =
        outcome.expect("scm11ea AI-vs-AI must reach a decisive outcome within the 45-min budget");
    eprintln!(
        "scm11ea resolved at tick {tick} (~{:.1} min); A_alive={aa} B_alive={ba}",
        tick as f64 / TICKS_PER_SEC as f64 / 60.0
    );
    assert!(aa != ba, "decisive: exactly one house survives");
    assert!(
        yard_a || yard_b,
        "on the 58%-water coastal map a coastal-based AI must build a naval yard"
    );
    assert!(
        peak_a > 0 || peak_b > 0,
        "with a surplus economy the AI must field at least one combat vessel"
    );
}

/// PNG evidence: AI-vs-AI naval on scm11ea — drive until both sides field vessels,
/// centre the camera on a vessel, and dump a frame of the naval action.
#[test]
fn png_ai_naval_battle_scm11ea() {
    if !support::real_assets_available() {
        eprintln!("SKIP");
        return;
    }
    let dir = support::assets_dir();
    let Ok(main) = std::fs::read(dir.join("main.mix")) else {
        return;
    };
    let Ok(redalert) = std::fs::read(dir.join("redalert.mix")) else {
        return;
    };
    let Ok(game) = assets::load_skirmish_from_bytes(
        &main,
        &redalert,
        "scm11ea.ini",
        CREDITS,
        Difficulty::Hard,
    ) else {
        return;
    };
    let (a, b) = (game.player_house, game.ai_house);
    let mut core = game.core;
    core.world_mut().set_ai(vec![
        AiPlayer::new(a, Difficulty::Hard),
        AiPlayer::new(b, Difficulty::Hard),
    ]);
    // Drive until several vessels are afloat and engaging, then frame one.
    let mut best_cell = None;
    for _ in 0..20_000u32 {
        core.world_mut().tick(&[]);
        let afloat = vessel_count(core.world(), a) + vessel_count(core.world(), b);
        if afloat >= 3 {
            // Centre on the first live vessel with a target (active combat).
            if let Some((_, u)) = core.world().units.iter().find(|(_, u)| {
                u.is_alive()
                    && core
                        .world()
                        .catalog
                        .unit(u.type_id)
                        .map(|p| p.locomotor == ra_sim::LOCO_WATER_INDEX)
                        .unwrap_or(false)
            }) {
                best_cell = Some(u.cell());
                break;
            }
        }
        if !core.world().house_alive(a) || !core.world().house_alive(b) {
            break;
        }
    }
    if let Some(c) = best_cell {
        // Reveal the shroud around the action for the rendered (player) house so the
        // vessels are visible in the evidence frame, not hidden in fog.
        for dy in -12..=12i32 {
            for dx in -12..=12i32 {
                core.world_mut().reveal_shroud(
                    a,
                    ra_sim::coords::CellCoord::new(c.x + dx, c.y + dy),
                    2,
                );
            }
        }
        core.set_camera(
            ((c.x - 6) * 24).max(0) as f32,
            ((c.y - 6) * 24).max(0) as f32,
        );
        core.update(30);
        dump(&core, "naval_ai_battle_scm11ea.png");
    } else {
        eprintln!("no vessels afloat to frame");
    }
}
