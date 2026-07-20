//! M7.5 P3: asset-free drive of the campaign menu flow — MainMenu → Campaign
//! button → mission list → briefing (text shown) → play → Victory → next
//! mission → … → campaign complete → MainMenu. Uses a synthetic
//! [`CampaignFactory`] whose missions win on the first tick (a trivial
//! `TIME 0 → WIN` trigger), so the whole state machine runs with no archives.

use ra_client::compositor::IndexedImage;
use ra_client::input::{InputEvent, MouseButton};
use ra_client::menu::{
    App, AppState, BuiltMission, CampaignEntry, CampaignFactory, GameFactory, ResolvedSkirmish,
};
use ra_client::AppCore;
use ra_sim::campaign::{taction, tevent};
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{Campaign, MoveStats, Passability, TActionDef, TEventDef, TriggerType, World};

/// A skirmish factory that's never used here (the Campaign path is exercised).
struct NoSkirmish;
impl GameFactory for NoSkirmish {
    fn build(&self, _res: &ResolvedSkirmish) -> Result<(AppCore, CellCoord), String> {
        Err("skirmish disabled in this test".into())
    }
}

/// Two synthetic missions; each builds a World that wins on tick 0.
struct SynthCampaign;

fn win_trigger() -> TriggerType {
    TriggerType {
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
    }
}

fn synth_campaign_world() -> World {
    let mut world = World::new(Passability::all_passable(), 0x1234);
    world.init_houses(8, 0);
    world.set_player_house(1);
    // A live player unit so the player isn't instantly "eliminated".
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
    let camp = Campaign {
        triggers: vec![win_trigger()],
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

impl CampaignFactory for SynthCampaign {
    fn missions(&self) -> Vec<CampaignEntry> {
        vec![
            CampaignEntry {
                scenario: "m1".into(),
                name: "In the thick of it".into(),
            },
            CampaignEntry {
                scenario: "m2".into(),
                name: "Five past midnight".into(),
            },
        ]
    }
    fn build(
        &self,
        scenario: &str,
        _difficulty: ra_sim::Difficulty,
    ) -> Result<BuiltMission, String> {
        let raster = IndexedImage {
            width: 8,
            height: 8,
            pixels: vec![0u8; 64],
        };
        let core = AppCore::with_sim(
            raster,
            [[0u8; 3]; 256],
            synth_campaign_world(),
            Vec::new(),
            Vec::new(),
        );
        Ok(BuiltMission {
            core,
            start: CellCoord::new(10, 10),
            name: if scenario == "m1" {
                "In the thick of it"
            } else {
                "Five past midnight"
            }
            .into(),
            briefing: format!("Briefing for {scenario}: rescue the scientist and win."),
        })
    }
}

/// Left-click at a point (mouse hit-testing path, complements keyboard focus).
fn click(app: &mut App, x: i32, y: i32) {
    app.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x,
        y,
    });
}

#[test]
fn campaign_flow_main_menu_to_complete() {
    let mut app = App::new(Vec::new(), Box::new(NoSkirmish)).with_campaign(Box::new(SynthCampaign));
    app.handle(InputEvent::Resize {
        width: 1024,
        height: 768,
    });
    assert_eq!(app.state(), AppState::MainMenu);
    assert_eq!(app.campaign_missions().len(), 2);

    // Enter the campaign list via the keyboard focus path (robust to layout).
    // Focus starts at SKIRMISH (0); Down -> CAMPAIGN (1); Confirm.
    app.handle(InputEvent::KeyDown(ra_client::input::Key::Down));
    app.handle(InputEvent::KeyDown(ra_client::input::Key::Confirm));
    assert_eq!(
        app.state(),
        AppState::CampaignList,
        "Campaign button opens the list"
    );

    // Select the first mission (focus 0 = mission 1) -> briefing.
    app.handle(InputEvent::KeyDown(ra_client::input::Key::Confirm));
    assert_eq!(app.state(), AppState::Briefing);
    assert!(
        app.briefing_text().contains("rescue"),
        "briefing text loaded: {:?}",
        app.briefing_text()
    );

    // Compose the briefing (smoke: no panic, right size).
    let f = app.compose();
    assert_eq!((f.width, f.height), (1024, 768));

    // START MISSION (focus 0 = START) -> InGame.
    app.handle(InputEvent::KeyDown(ra_client::input::Key::Confirm));
    assert_eq!(app.state(), AppState::InGame);

    // One update ticks the sim: the TIME-0 WIN trigger fires -> Victory ->
    // GameOver screen.
    app.update(100);
    assert_eq!(app.state(), AppState::GameOver);
    assert_eq!(app.core().unwrap().game_over(), ra_sim::GameOver::Victory);

    // Continue -> advances to mission 2's briefing.
    app.handle(InputEvent::KeyDown(ra_client::input::Key::Confirm));
    assert_eq!(
        app.state(),
        AppState::Briefing,
        "victory advances to next mission"
    );
    assert!(app.briefing_text().contains("m2"));

    // Play + win mission 2.
    app.handle(InputEvent::KeyDown(ra_client::input::Key::Confirm)); // START
    assert_eq!(app.state(), AppState::InGame);
    app.update(100);
    assert_eq!(app.state(), AppState::GameOver);

    // Continue past the last mission -> campaign complete -> MainMenu.
    app.handle(InputEvent::KeyDown(ra_client::input::Key::Confirm));
    assert_eq!(
        app.state(),
        AppState::MainMenu,
        "completing the last mission returns to menu"
    );
}

/// Mouse-driven variant: the Campaign button and BACK are reachable by clicking.
#[test]
fn campaign_list_reachable_by_mouse_and_back() {
    let mut app = App::new(Vec::new(), Box::new(NoSkirmish)).with_campaign(Box::new(SynthCampaign));
    app.handle(InputEvent::Resize {
        width: 1024,
        height: 768,
    });
    // Click the CAMPAIGN button (second main-menu button, centered).
    let cx = 1024 / 2;
    // Main-menu buttons start at vh/2-20 with gap 50; button 1 (CAMPAIGN) center.
    let y = 768 / 2 - 20 + 50 + 18;
    click(&mut app, cx, y);
    assert_eq!(app.state(), AppState::CampaignList);
    // ESC backs out to the main menu.
    app.handle(InputEvent::KeyDown(ra_client::input::Key::Menu));
    assert_eq!(app.state(), AppState::MainMenu);
}
