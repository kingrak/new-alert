//! M7.5-C P0 UI coverage (asset-free): the briefing screen's Easy/Normal/Hard
//! selector drives the campaign difficulty threaded into the `CampaignFactory`
//! (the same "factory config" path the skirmish setup uses for its difficulty),
//! and defaults to Normal.

mod support;

use ra_client::compositor::IndexedImage;
use ra_client::input::{InputEvent, Key};
use ra_client::menu::{App, AppState, BuiltMission, CampaignEntry, CampaignFactory, GameFactory};
use ra_client::AppCore;
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{Campaign, Difficulty, MoveStats, Passability, World};
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
