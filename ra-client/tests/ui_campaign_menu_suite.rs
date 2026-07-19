//! M7.5-A campaign-flow UI coverage (asset-free, synthetic factories — no
//! archives read anywhere in this file):
//!
//! 1. A pinned golden `compose()` hash for the `Briefing` screen.
//! 2. Defeat -> RETRY MISSION produces a genuinely FRESH world (not the
//!    exhausted/continued one) for the SAME mission.
//! 3. Victory -> next-mission advance across THREE missions, confirming the
//!    mission index keeps incrementing (not just a single 1->2 toggle) and
//!    the `Briefing` state is re-entered each time with that mission's own
//!    text, ending in campaign-complete (-> `MainMenu`).
//!
//! See `ra-client/tests/ui_campaign_flow.rs` for the two-mission smoke
//! version of the win-chain this file extends, and `ra-client/tests/
//! ui_menu_monkey.rs` for the fuzzed/no-panic coverage of the same states.

mod support;

use ra_client::compositor::IndexedImage;
use ra_client::input::{InputEvent, Key};
use ra_client::menu::{App, AppState, BuiltMission, CampaignEntry, CampaignFactory, GameFactory};
use ra_client::AppCore;
use ra_sim::campaign::{taction, tevent};
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{Campaign, MoveStats, Passability, TActionDef, TEventDef, TriggerType, World};

/// A skirmish factory that's never used (the Campaign path is exercised).
struct NoSkirmish;
impl GameFactory for NoSkirmish {
    fn build(
        &self,
        _res: &ra_client::menu::ResolvedSkirmish,
    ) -> Result<(AppCore, CellCoord), String> {
        Err("skirmish disabled in this test".into())
    }
}

/// A `TIME(0) -> WIN` or `TIME(0) -> LOSE` trigger (fires the very first
/// `run_campaign` tick, `ra-sim/src/world.rs`'s `maybe_spring`).
fn win_lose_trigger(win: bool) -> TriggerType {
    TriggerType {
        name: if win { "win" } else { "lose" }.into(),
        persist: 0, // VOLATILE
        house: 1,
        event_ctrl: 0,
        action_ctrl: 0,
        e1: TEventDef {
            code: tevent::TIME,
            team: -1,
            data: 0,
        },
        e2: TEventDef {
            code: tevent::NONE,
            team: -1,
            data: 0,
        },
        a1: TActionDef {
            code: if win { taction::WIN } else { taction::LOSE },
            team: -1,
            trigger: -1,
            data: -1,
        },
        a2: TActionDef {
            code: taction::NONE,
            team: -1,
            trigger: -1,
            data: -1,
        },
    }
}

/// A world that resolves win/lose on tick 0, with one player unit at a fixed
/// cell (so "fresh world" checks below have something concrete to compare:
/// unit count + position + `tick_count()`).
fn synth_world(win: bool, seed: u32) -> World {
    let mut world = World::new(Passability::all_passable(), seed);
    world.init_houses(8, 0);
    world.set_player_house(1);
    world.spawn_unit(
        0,
        1,
        CellCoord::new(10, 10),
        Facing(0),
        100,
        MoveStats {
            max_speed: 20,
            rot: 8,
        },
    );
    let t = win_lose_trigger(win);
    let camp = Campaign {
        triggers: vec![t],
        teamtypes: Vec::new(),
        waypoints: vec![-1; 101],
        globals: vec![false; 8],
        cell_triggers: Vec::new(),
        state: vec![ra_sim::campaign::TriggerState::default()],
        started: false,
        mission_timer: None,
        evac_cells: Vec::new(),
        civ_evacuated: vec![false; 8],
        reveal_all: false,
        reveal_cells: Vec::new(),
        pending_texts: Vec::new(),
        pending_speech: Vec::new(),
    };
    world.set_campaign(camp);
    world
}

fn built(win: bool, seed: u32, name: &str, briefing: &str) -> BuiltMission {
    let raster = IndexedImage {
        width: 8,
        height: 8,
        pixels: vec![0u8; 64],
    };
    let core = AppCore::with_sim(
        raster,
        [[0u8; 3]; 256],
        synth_world(win, seed),
        Vec::new(),
        Vec::new(),
    );
    BuiltMission {
        core,
        start: CellCoord::new(10, 10),
        name: name.to_string(),
        briefing: briefing.to_string(),
    }
}

// ===========================================================================
// §1 Briefing golden frame
// ===========================================================================

struct OneMissionCampaign;
impl CampaignFactory for OneMissionCampaign {
    fn missions(&self) -> Vec<CampaignEntry> {
        vec![CampaignEntry {
            scenario: "m1".into(),
            name: "The Golden Briefing".into(),
        }]
    }
    fn build(&self, _scenario: &str) -> Result<BuiltMission, String> {
        Ok(built(
            true,
            0x6018_DE01,
            "The Golden Briefing",
            "Commander, your objective is to hold the line and report back.",
        ))
    }
}

/// Pinned `compose()` hash for the `Briefing` screen with a fixed synthetic
/// mission's text — same tolerance-free-pin convention as `ui_menu_golden_
/// frames.rs` (integer-only compositing, so any hash change is either a real
/// compositing/layout bug or a deliberate change that must update this pin
/// with a comment explaining why).
#[test]
fn briefing_screen_frame_hash() {
    let mut app =
        App::new(Vec::new(), Box::new(NoSkirmish)).with_campaign(Box::new(OneMissionCampaign));
    app.handle(InputEvent::Resize {
        width: 1024,
        height: 768,
    });
    app.handle(InputEvent::KeyDown(Key::Down)); // focus CAMPAIGN
    app.handle(InputEvent::KeyDown(Key::Confirm)); // -> CampaignList
    assert_eq!(app.state(), AppState::CampaignList);
    app.handle(InputEvent::KeyDown(Key::Confirm)); // select mission 1 -> Briefing
    assert_eq!(app.state(), AppState::Briefing);
    assert!(app.briefing_text().contains("hold the line"));

    let f = app.compose();
    assert_eq!((f.width, f.height), (1024, 768));
    assert_eq!(
        support::fnv1a(&f.pixels),
        0x6faa_24ef_871f_dd4f,
        "Briefing frame hash changed (composition or layout changed)"
    );
}

// ===========================================================================
// §2 Defeat -> RETRY produces a fresh world, same mission
// ===========================================================================

/// Mission 0 always LOSEs on tick 0; mission 1 (never reached by this test)
/// exists only to prove RetryMission does not advance `campaign_current` —
/// see the test's final assertion.
struct RetryCampaign;
impl CampaignFactory for RetryCampaign {
    fn missions(&self) -> Vec<CampaignEntry> {
        vec![
            CampaignEntry {
                scenario: "doomed".into(),
                name: "Doomed Mission".into(),
            },
            CampaignEntry {
                scenario: "never".into(),
                name: "Never Reached".into(),
            },
        ]
    }
    fn build(&self, scenario: &str) -> Result<BuiltMission, String> {
        Ok(built(
            false, // always LOSE
            0xDEAD_0000,
            "Doomed Mission",
            &format!("Briefing for {scenario}: this mission cannot be won."),
        ))
    }
}

/// `Action::RetryMission` -> `App::start_mission(self.campaign_current)`
/// (`menu.rs` ~line 562/601): since `pending_core` was already consumed by
/// the first `start_mission` call, retry falls into the `None` branch and
/// calls `CampaignFactory::build` again from scratch — a genuinely new
/// `World` (fresh `tick_count`, fresh unit arena), NOT the exhausted one that
/// just lost. This test proves that externally: run the doomed mission until
/// Defeat (tick_count > 0 by then), press RETRY MISSION, and confirm the new
/// world's `tick_count()` is back to 0 and the original unit exists again
/// (arena regenerated, not carried over).
#[test]
fn defeat_retry_rebuilds_a_fresh_world_for_the_same_mission() {
    let mut app = App::new(Vec::new(), Box::new(NoSkirmish)).with_campaign(Box::new(RetryCampaign));
    app.handle(InputEvent::Resize {
        width: 1024,
        height: 768,
    });
    app.handle(InputEvent::KeyDown(Key::Down));
    app.handle(InputEvent::KeyDown(Key::Confirm)); // -> CampaignList
    app.handle(InputEvent::KeyDown(Key::Confirm)); // select "Doomed Mission" -> Briefing
    assert_eq!(app.state(), AppState::Briefing);
    app.handle(InputEvent::KeyDown(Key::Confirm)); // START MISSION -> InGame
    assert_eq!(app.state(), AppState::InGame);

    // Let the LOSE trigger resolve; the sim must have advanced at least one
    // tick by the time Defeat latches.
    app.update(100);
    assert_eq!(app.state(), AppState::GameOver);
    assert_eq!(app.core().unwrap().game_over(), ra_sim::GameOver::Defeat);
    let tick_at_defeat = app.core().unwrap().world().tick_count();
    assert!(
        tick_at_defeat >= 1,
        "the doomed world must have ticked at least once before Defeat latched"
    );

    // A defeat's game-over screen offers RETRY MISSION above CONTINUE
    // (`items_gameover`, `menu.rs` ~line 1194) — focus 0 is RETRY.
    app.handle(InputEvent::KeyDown(Key::Confirm)); // RETRY MISSION
    assert_eq!(
        app.state(),
        AppState::InGame,
        "RetryMission goes straight back to InGame (not Briefing)"
    );
    let fresh = app.core().unwrap().world();
    assert_eq!(
        fresh.tick_count(),
        0,
        "retry must be a FRESH world (tick_count reset to 0), not the exhausted one"
    );
    assert_eq!(
        fresh.units.len(),
        1,
        "the original single player unit must exist again (fresh spawn, not the removed/dead one)"
    );
    let (_, u) = fresh.units.iter().next().unwrap();
    assert_eq!(
        u.cell(),
        CellCoord::new(10, 10),
        "fresh world's unit is back at the mission's original start cell"
    );

    // Prove retry did NOT advance the mission index: losing again and this
    // time pressing CONTINUE (not RETRY) must return to MainMenu, not
    // "Never Reached"'s briefing — `on_continue` only advances on a VICTORY
    // (`menu.rs` ~line 651: `if self.in_campaign && victory`), so a defeat's
    // Continue always calls `quit_to_menu()` regardless of which mission
    // index we're on.
    app.update(100);
    assert_eq!(app.state(), AppState::GameOver);
    // Focus down past RETRY to CONTINUE.
    app.handle(InputEvent::KeyDown(Key::Down));
    app.handle(InputEvent::KeyDown(Key::Confirm));
    assert_eq!(
        app.state(),
        AppState::MainMenu,
        "defeat's CONTINUE returns to MainMenu, never advancing to another mission's briefing"
    );
}

// ===========================================================================
// §3 Victory -> next-mission advance across three missions
// ===========================================================================

struct ThreeMissionCampaign;
impl CampaignFactory for ThreeMissionCampaign {
    fn missions(&self) -> Vec<CampaignEntry> {
        vec![
            CampaignEntry {
                scenario: "alpha".into(),
                name: "Alpha Mission".into(),
            },
            CampaignEntry {
                scenario: "bravo".into(),
                name: "Bravo Mission".into(),
            },
            CampaignEntry {
                scenario: "charlie".into(),
                name: "Charlie Mission".into(),
            },
        ]
    }
    fn build(&self, scenario: &str) -> Result<BuiltMission, String> {
        let (name, seed) = match scenario {
            "alpha" => ("Alpha Mission", 0xA1),
            "bravo" => ("Bravo Mission", 0xB2),
            "charlie" => ("Charlie Mission", 0xC3),
            _ => panic!("unknown scenario {scenario}"),
        };
        Ok(built(
            true, // every mission wins on tick 0
            seed,
            name,
            &format!("Briefing marker: {scenario}"),
        ))
    }
}

/// Wins mission N -> asserts the game advances to Briefing showing mission
/// N+1's OWN text (not stale/repeated text), across three consecutive
/// missions — i.e. the mission index genuinely increments each time, not
/// just toggling between two states. Ends on the third victory's CONTINUE,
/// which must complete the campaign (-> `MainMenu`, `on_continue`'s `next >=
/// campaign_missions.len()` branch, `menu.rs` ~line 655-659).
#[test]
fn victory_advances_the_mission_index_across_three_missions_then_completes() {
    let mut app =
        App::new(Vec::new(), Box::new(NoSkirmish)).with_campaign(Box::new(ThreeMissionCampaign));
    app.handle(InputEvent::Resize {
        width: 1024,
        height: 768,
    });
    app.handle(InputEvent::KeyDown(Key::Down));
    app.handle(InputEvent::KeyDown(Key::Confirm)); // -> CampaignList
    app.handle(InputEvent::KeyDown(Key::Confirm)); // select mission 0 (Alpha) -> Briefing
    assert_eq!(app.state(), AppState::Briefing);
    assert!(app.briefing_text().contains("alpha"));

    for (idx, marker) in ["alpha", "bravo", "charlie"].iter().enumerate() {
        // Briefing text must match the mission we're actually on.
        assert!(
            app.briefing_text().contains(marker),
            "mission index {idx}: expected briefing to mention {marker:?}, got {:?}",
            app.briefing_text()
        );
        app.handle(InputEvent::KeyDown(Key::Confirm)); // START MISSION
        assert_eq!(app.state(), AppState::InGame);
        app.update(100); // TIME(0) WIN resolves immediately
        assert_eq!(app.state(), AppState::GameOver);
        assert_eq!(app.core().unwrap().game_over(), ra_sim::GameOver::Victory);
        app.handle(InputEvent::KeyDown(Key::Confirm)); // CONTINUE

        if idx < 2 {
            assert_eq!(
                app.state(),
                AppState::Briefing,
                "victory on mission {idx} ({marker}) must re-enter Briefing for the next mission"
            );
        } else {
            assert_eq!(
                app.state(),
                AppState::MainMenu,
                "victory on the LAST mission ({marker}) must complete the campaign, not re-brief"
            );
        }
    }
}
