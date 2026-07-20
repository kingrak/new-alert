//! M7.5-C P0 UI coverage (asset-free): the briefing screen's Easy/Normal/Hard
//! selector drives the campaign difficulty threaded into the `CampaignFactory`
//! (the same "factory config" path the skirmish setup uses for its difficulty),
//! and defaults to Normal.

mod support;

use ra_client::compositor::IndexedImage;
use ra_client::input::{InputEvent, Key};
use ra_client::menu::{App, AppState, BuiltMission, CampaignEntry, CampaignFactory, GameFactory};
use ra_client::AppCore;
use ra_sim::campaign::{taction, tevent, TriggerState};
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    Campaign, Difficulty, MoveStats, Passability, TActionDef, TEventDef, TriggerType, World,
};
use std::cell::Cell;
use std::rc::Rc;

struct NoSkirmish;
impl GameFactory for NoSkirmish {
    fn build(
        &self,
        _res: &ra_client::menu::ResolvedSkirmish,
    ) -> Result<(AppCore, CellCoord), String> {
        Err("skirmish disabled".into())
    }
}

fn synth_world() -> World {
    let mut world = World::new(Passability::all_passable(), 0x1234);
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
    world.set_campaign(Campaign {
        triggers: Vec::new(),
        teamtypes: Vec::new(),
        waypoints: vec![-1; 101],
        globals: vec![false; 8],
        cell_triggers: Vec::new(),
        state: Vec::new(),
        started: false,
        mission_timer: None,
        evac_cells: Vec::new(),
        civ_evacuated: vec![false; 8],
        reveal_all: false,
        reveal_cells: Vec::new(),
        pending_texts: Vec::new(),
        pending_speech: Vec::new(),
    });
    world
}

/// Records every difficulty the App asks it to build with.
struct RecordingCampaign {
    built_with: Rc<Cell<Option<Difficulty>>>,
}
impl CampaignFactory for RecordingCampaign {
    fn missions(&self) -> Vec<CampaignEntry> {
        vec![CampaignEntry {
            scenario: "m1.ini".into(),
            name: "Mission One".into(),
        }]
    }
    fn build(&self, _scenario: &str, difficulty: Difficulty) -> Result<BuiltMission, String> {
        self.built_with.set(Some(difficulty));
        let core = AppCore::with_sim(
            IndexedImage {
                width: 8,
                height: 8,
                pixels: vec![0u8; 64],
            },
            [[0u8; 3]; 256],
            synth_world(),
            Vec::new(),
            Vec::new(),
        );
        Ok(BuiltMission {
            core,
            start: CellCoord::new(10, 10),
            name: "Mission One".into(),
            briefing: "brief".into(),
        })
    }
}

#[test]
fn briefing_difficulty_selector_drives_the_factory_config() {
    let built_with = Rc::new(Cell::new(None));
    let mut app =
        App::new(Vec::new(), Box::new(NoSkirmish)).with_campaign(Box::new(RecordingCampaign {
            built_with: built_with.clone(),
        }));
    app.handle(InputEvent::Resize {
        width: 1024,
        height: 768,
    });

    // Main menu -> campaign list -> briefing (mission 0).
    app.handle(InputEvent::KeyDown(Key::Down)); // focus CAMPAIGN
    app.handle(InputEvent::KeyDown(Key::Confirm)); // -> CampaignList
    app.handle(InputEvent::KeyDown(Key::Confirm)); // select mission -> Briefing
    assert_eq!(app.state(), AppState::Briefing);
    // Default is Normal (a neutral no-op handicap).
    assert_eq!(app.campaign_difficulty(), Difficulty::Normal);

    // Item order on the briefing screen is [START(0), BACK(1), EASY(2), NORMAL(3),
    // HARD(4)]. Move focus to HARD (Down x4) and confirm to select it.
    for _ in 0..4 {
        app.handle(InputEvent::KeyDown(Key::Down));
    }
    app.handle(InputEvent::KeyDown(Key::Confirm)); // SetCampaignDifficulty(Hard)
    assert_eq!(
        app.state(),
        AppState::Briefing,
        "selecting difficulty stays on briefing"
    );
    assert_eq!(app.campaign_difficulty(), Difficulty::Hard);

    // Focus wraps Down (HARD -> START) and Confirm starts the mission — the factory
    // must be handed the selected Hard difficulty.
    app.handle(InputEvent::KeyDown(Key::Down)); // focus back to START
    app.handle(InputEvent::KeyDown(Key::Confirm)); // START MISSION
    assert_eq!(app.state(), AppState::InGame);
    assert_eq!(
        built_with.get(),
        Some(Difficulty::Hard),
        "the mission was built with the selected Hard difficulty"
    );
}

// ===========================================================================
// Depth (ra-tester): difficulty threading through defeat-retry and
// victory-advance. `App::campaign_difficulty` is a single field on `App`,
// persisted across the whole App lifetime and read fresh by every
// `f.build(...)` call site (`goto_briefing`/`start_mission`, `menu.rs`); it is
// never reset by `on_continue`/`quit_to_menu`/`start_mission`. These tests pin
// that behaviour end-to-end through the real input-driven flow rather than
// just reading the source.
// ===========================================================================

/// A world that resolves to DEFEAT on the very next tick (a `TIME`-0 `LOSE`
/// trigger), for driving the `GameOver -> RetryMission -> InGame` edge.
fn defeat_world() -> World {
    let mut world = synth_world();
    let t = TriggerType {
        name: "lose".into(),
        persist: 0,
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
            code: taction::LOSE,
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
    };
    let mut camp = world.campaign().unwrap().clone();
    camp.triggers = vec![t];
    camp.state = vec![TriggerState::default()];
    world.set_campaign(camp);
    world
}

/// A world that resolves to VICTORY on the very next tick, for driving the
/// `GameOver -> Continue -> Briefing` (next mission) edge.
fn victory_world() -> World {
    let mut world = synth_world();
    let t = TriggerType {
        name: "win".into(),
        persist: 0,
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
            code: taction::WIN,
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
    };
    let mut camp = world.campaign().unwrap().clone();
    camp.triggers = vec![t];
    camp.state = vec![TriggerState::default()];
    world.set_campaign(camp);
    world
}

/// Records every difficulty the App asks it to build with, across every call
/// (a `Vec`, not just the last one). Single mission, always defeats — for
/// driving the retry edge.
struct AlwaysDefeatCampaign {
    built_with: Rc<std::cell::RefCell<Vec<(String, Difficulty)>>>,
}
impl CampaignFactory for AlwaysDefeatCampaign {
    fn missions(&self) -> Vec<CampaignEntry> {
        vec![CampaignEntry {
            scenario: "m1".into(),
            name: "Mission One (always loses)".into(),
        }]
    }
    fn build(&self, scenario: &str, difficulty: Difficulty) -> Result<BuiltMission, String> {
        self.built_with
            .borrow_mut()
            .push((scenario.to_string(), difficulty));
        let core = AppCore::with_sim(
            IndexedImage {
                width: 8,
                height: 8,
                pixels: vec![0u8; 64],
            },
            [[0u8; 3]; 256],
            defeat_world(),
            Vec::new(),
            Vec::new(),
        );
        Ok(BuiltMission {
            core,
            start: CellCoord::new(10, 10),
            name: scenario.to_string(),
            briefing: format!("brief {scenario}"),
        })
    }
}

/// Retry after a campaign defeat must rebuild the mission with the **same**
/// difficulty the player selected on the briefing screen — the App never
/// silently resets to Normal on a retry.
#[test]
fn retry_after_defeat_keeps_the_selected_difficulty() {
    let built_with = Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut app =
        App::new(Vec::new(), Box::new(NoSkirmish)).with_campaign(Box::new(AlwaysDefeatCampaign {
            built_with: built_with.clone(),
        }));
    app.handle(InputEvent::Resize {
        width: 1024,
        height: 768,
    });

    // Main menu -> campaign list -> briefing (mission 0).
    app.handle(InputEvent::KeyDown(Key::Down));
    app.handle(InputEvent::KeyDown(Key::Confirm));
    app.handle(InputEvent::KeyDown(Key::Confirm));
    assert_eq!(app.state(), AppState::Briefing);

    // Select HARD, then start the (always-losing) mission.
    for _ in 0..4 {
        app.handle(InputEvent::KeyDown(Key::Down));
    }
    app.handle(InputEvent::KeyDown(Key::Confirm)); // SetCampaignDifficulty(Hard)
    assert_eq!(app.campaign_difficulty(), Difficulty::Hard);
    app.handle(InputEvent::KeyDown(Key::Down)); // wrap to START
    app.handle(InputEvent::KeyDown(Key::Confirm)); // START MISSION
    assert_eq!(app.state(), AppState::InGame);

    // Let the scripted LOSE trigger resolve -> GameOver.
    for _ in 0..10 {
        app.update(70);
        if app.state() == AppState::GameOver {
            break;
        }
    }
    assert_eq!(
        app.state(),
        AppState::GameOver,
        "the defeat trigger must resolve"
    );

    // RETRY MISSION: campaign-difficulty selector doesn't even appear on
    // GameOver, so nothing could have changed it -- it must still read Hard,
    // and the rebuilt mission must have been built with Hard.
    assert_eq!(
        app.campaign_difficulty(),
        Difficulty::Hard,
        "difficulty unchanged across defeat"
    );
    app.handle(InputEvent::KeyDown(Key::Confirm)); // RETRY MISSION (first/only item on a campaign defeat)
    assert_eq!(app.state(), AppState::InGame, "retry re-enters the mission");
    assert_eq!(
        app.campaign_difficulty(),
        Difficulty::Hard,
        "difficulty unchanged across retry"
    );

    // The App made several build calls: the initial `goto_briefing` at the
    // default Normal, one eager rebuild when HARD was selected
    // (`set_campaign_difficulty`, `menu.rs`), and the retry. Every build is
    // mission 0 ("m1"); what matters here is that everything from the moment
    // Hard was selected onward — the pre-defeat build *and* the post-defeat
    // retry build — used Hard, not that the very first (pre-selection,
    // still-Normal) build somehow retroactively did too.
    let calls = built_with.borrow();
    assert!(
        calls.len() >= 3,
        "expected >= 3 builds (initial Normal + Hard-select + retry), got {calls:?}"
    );
    assert!(
        calls.iter().all(|(s, _)| s == "m1"),
        "every build must be mission 0: {calls:?}"
    );
    assert_eq!(
        calls[0].1,
        Difficulty::Normal,
        "the very first briefing build (before any selection) is the App default: {calls:?}"
    );
    assert!(
        calls[1..].iter().all(|(_, d)| *d == Difficulty::Hard),
        "every build from the moment Hard was selected onward (incl. the retry) must be Hard: {calls:?}"
    );
    let retry_call = calls.last().unwrap();
    assert_eq!(
        retry_call.1,
        Difficulty::Hard,
        "the retry-triggered rebuild specifically must be Hard: {calls:?}"
    );
}

/// Records every difficulty the App asks it to build with. Mission 0 ("w1")
/// always wins; mission 1 ("w2") just needs to build successfully (its own
/// resolution is irrelevant to this test).
struct AlwaysWinCampaign {
    built_with: Rc<std::cell::RefCell<Vec<(String, Difficulty)>>>,
}
impl CampaignFactory for AlwaysWinCampaign {
    fn missions(&self) -> Vec<CampaignEntry> {
        vec![
            CampaignEntry {
                scenario: "w1".into(),
                name: "Mission One (always wins)".into(),
            },
            CampaignEntry {
                scenario: "w2".into(),
                name: "Mission Two".into(),
            },
        ]
    }
    fn build(&self, scenario: &str, difficulty: Difficulty) -> Result<BuiltMission, String> {
        self.built_with
            .borrow_mut()
            .push((scenario.to_string(), difficulty));
        let world = if scenario == "w1" {
            victory_world()
        } else {
            synth_world()
        };
        let core = AppCore::with_sim(
            IndexedImage {
                width: 8,
                height: 8,
                pixels: vec![0u8; 64],
            },
            [[0u8; 3]; 256],
            world,
            Vec::new(),
            Vec::new(),
        );
        Ok(BuiltMission {
            core,
            start: CellCoord::new(10, 10),
            name: scenario.to_string(),
            briefing: format!("brief {scenario}"),
        })
    }
}

/// Victory-advance to the next mission must carry the **same** difficulty
/// forward — the briefing for mission 2 is built with the difficulty selected
/// back on mission 1's briefing, not reset to Normal.
#[test]
fn victory_advance_to_next_mission_keeps_the_selected_difficulty() {
    let built_with = Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut app =
        App::new(Vec::new(), Box::new(NoSkirmish)).with_campaign(Box::new(AlwaysWinCampaign {
            built_with: built_with.clone(),
        }));
    app.handle(InputEvent::Resize {
        width: 1024,
        height: 768,
    });

    // Main menu -> campaign list -> briefing (mission 0, "w1").
    app.handle(InputEvent::KeyDown(Key::Down));
    app.handle(InputEvent::KeyDown(Key::Confirm));
    app.handle(InputEvent::KeyDown(Key::Confirm));
    assert_eq!(app.state(), AppState::Briefing);

    // Select EASY this time, to prove it's not just "Hard survives by luck".
    for _ in 0..2 {
        app.handle(InputEvent::KeyDown(Key::Down));
    }
    app.handle(InputEvent::KeyDown(Key::Confirm)); // SetCampaignDifficulty(Easy)
    assert_eq!(app.campaign_difficulty(), Difficulty::Easy);
    app.handle(InputEvent::KeyDown(Key::Down));
    app.handle(InputEvent::KeyDown(Key::Down));
    app.handle(InputEvent::KeyDown(Key::Down)); // wrap back to START (EASY -> ... -> START)
    app.handle(InputEvent::KeyDown(Key::Confirm)); // START MISSION

    // If the focus math above landed anywhere else, StartMission wouldn't have
    // fired -- guard explicitly rather than silently no-op-ing the test.
    assert_eq!(app.state(), AppState::InGame, "mission 0 must have started");
    assert_eq!(app.campaign_difficulty(), Difficulty::Easy);

    for _ in 0..10 {
        app.update(70);
        if app.state() == AppState::GameOver {
            break;
        }
    }
    assert_eq!(
        app.state(),
        AppState::GameOver,
        "the win trigger must resolve GameOver"
    );
    assert_eq!(
        app.core().map(|c| c.game_over()),
        Some(ra_sim::GameOver::Victory),
        "must actually be a Victory, not a coincidental Defeat"
    );

    // Continue -> advances to mission 1's briefing (still Easy).
    app.handle(InputEvent::KeyDown(Key::Confirm)); // CONTINUE
    assert_eq!(
        app.state(),
        AppState::Briefing,
        "victory advances to the next mission's briefing"
    );
    assert_eq!(
        app.campaign_difficulty(),
        Difficulty::Easy,
        "difficulty unchanged across the victory-advance"
    );

    // Start mission 1 too -- it must also be built at Easy.
    app.handle(InputEvent::KeyDown(Key::Confirm)); // START MISSION (mission 1, default focus 0)
    assert_eq!(app.state(), AppState::InGame);

    let calls = built_with.borrow();
    assert_eq!(
        calls.iter().find(|(s, _)| s == "w2").map(|(_, d)| *d),
        Some(Difficulty::Easy),
        "mission 1 (post-victory-advance) must be built at the Easy difficulty carried over from mission 0: {calls:?}"
    );
}
