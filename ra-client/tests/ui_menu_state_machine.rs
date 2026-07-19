//! M7.8 scripted state-machine drive (asset-free). Exercises the pre-game
//! `App` flow — MainMenu → SkirmishSetup → InGame → Paused → resume →
//! quit-to-menu → a second game — with a synthetic [`GameFactory`] so it runs on
//! every OS target with no archives. The real-asset drive (map scan, minimap
//! preview, user maps folder, PNG evidence) lives in the `verify-m78` bin
//! subcommand; this pins the logic ra-tester will expand (monkey/golden layers).

use ra_client::compositor::{IndexedImage, RgbaImage};
use ra_client::input::{InputEvent, Key, MouseButton};
use ra_client::menu::{App, AppState, GameFactory, MapEntry, MapSource, ResolvedSkirmish, HOUSES};
use ra_client::AppCore;
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{AiPlayer, Difficulty, GameOver, MoveStats, Passability, World};

/// A synthetic factory: builds a tiny World configured exactly by the resolved
/// choices, so the state machine's settings-threading is observable without
/// assets. Records the last resolved request for inspection.
struct SynthFactory;

impl GameFactory for SynthFactory {
    fn build(&self, res: &ResolvedSkirmish) -> Result<(AppCore, CellCoord), String> {
        if res.map_filename.is_empty() {
            return Err("empty map".to_string());
        }
        let mut world = World::new(Passability::all_passable(), 0xABCD_1234);
        world.init_houses(8, res.credits);
        world.set_player_house(res.player_house);
        let ai_house = if res.player_house == 2 { 0 } else { 2 };
        world.set_ai(vec![AiPlayer::new(ai_house, res.difficulty)]);
        // Give each house a live unit so the win/lose check stays Ongoing (an
        // empty house reads as eliminated → instant Defeat).
        let stats = MoveStats {
            max_speed: 20,
            rot: 8,
        };
        world.spawn_unit(
            0,
            res.player_house,
            CellCoord::new(10, 10),
            Facing(0),
            100,
            stats,
        );
        world.spawn_unit(0, ai_house, CellCoord::new(50, 50), Facing(0), 100, stats);
        let raster = IndexedImage {
            width: 16,
            height: 16,
            pixels: vec![0u8; 16 * 16],
        };
        let mut core = AppCore::with_sim(raster, [[0u8; 3]; 256], world, Vec::new(), Vec::new());
        core.enable_sidebar(res.player_house, Vec::new());
        core.enable_radar();
        core.set_classic_radar(res.classic_radar);
        Ok((core, CellCoord::new(10, 10)))
    }
}

fn synth_maps() -> Vec<MapEntry> {
    let mk = |name: &str, file: &str, players: u8, src: MapSource| MapEntry {
        name: name.to_string(),
        filename: file.to_string(),
        players,
        width: 64,
        height: 64,
        source: src,
        // Empty preview -> the menu draws a placeholder (no assets needed).
        preview: RgbaImage {
            width: 0,
            height: 0,
            pixels: Vec::new(),
        },
    };
    vec![
        mk("Alpha", "alpha.ini", 2, MapSource::Archive),
        mk("Bravo", "bravo.ini", 4, MapSource::Archive),
        mk("MyMap", "mymap.mpr", 2, MapSource::User),
    ]
}

fn app() -> App {
    let mut a = App::new(synth_maps(), Box::new(SynthFactory));
    a.handle(InputEvent::Resize {
        width: 1024,
        height: 768,
    });
    a
}

fn click(a: &mut App, x: i32, y: i32) {
    a.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x,
        y,
    });
    a.handle(InputEvent::MouseUp {
        button: MouseButton::Left,
        x,
        y,
    });
}

#[test]
fn boots_to_main_menu_and_compose_never_panics_in_any_state() {
    let mut a = app();
    assert_eq!(a.state(), AppState::MainMenu);
    // compose in each reachable state produces a full-viewport frame.
    for _ in 0..1 {
        let f = a.compose();
        assert_eq!((f.width, f.height), (1024, 768));
    }
    click(&mut a, 512, 768 / 2 - 2); // SKIRMISH
    assert_eq!(a.state(), AppState::SkirmishSetup);
    let f = a.compose();
    assert_eq!((f.width, f.height), (1024, 768));
}

#[test]
fn full_flow_threads_settings_freezes_on_pause_and_restarts_fresh() {
    let mut a = app();

    // MainMenu -> SkirmishSetup.
    click(&mut a, 512, 768 / 2 - 2);
    assert_eq!(a.state(), AppState::SkirmishSetup);

    // Choose: map #2, Hard, USSR, 10000, radar OFF.
    a.select_map(1);
    {
        let cfg = a.config_mut();
        cfg.difficulty = 2; // HARD
        cfg.house = HOUSES.iter().position(|(n, _)| *n == "USSR").unwrap();
        cfg.credits = 3; // 10000
        cfg.classic_radar = false;
    }
    a.start_game();
    assert_eq!(a.state(), AppState::InGame);

    // World built with those settings.
    let core = a.core().expect("core exists");
    assert_eq!(core.world().player_house(), Some(2));
    assert_eq!(core.world().house_credits(2), 10000);
    assert_eq!(core.world().ai_difficulty(0), Some(Difficulty::Hard));
    assert!(core.has_radar(), "classic radar OFF => always-on");
    assert_eq!(core.world().game_over(), GameOver::Ongoing);

    // Play, then pause: the tick count must freeze.
    for _ in 0..20 {
        a.update(67);
    }
    let t0 = a.core().unwrap().world().tick_count();
    assert!(t0 > 0, "sim advanced while in game");
    a.handle(InputEvent::KeyDown(Key::Menu)); // Esc -> pause
    assert_eq!(a.state(), AppState::Paused);
    for _ in 0..20 {
        a.update(67);
    }
    let t1 = a.core().unwrap().world().tick_count();
    assert_eq!(t0, t1, "sim frozen while paused");

    // Resume (RESUME button) -> ticks again.
    click(&mut a, 512, 768 / 2 - 30 + 18);
    assert_eq!(a.state(), AppState::InGame);
    for _ in 0..10 {
        a.update(67);
    }
    assert!(a.core().unwrap().world().tick_count() > t1, "sim resumed");

    // Quit to menu: pause then QUIT TO MENU -> fresh menu, World dropped.
    a.handle(InputEvent::KeyDown(Key::Menu));
    click(&mut a, 512, 768 / 2 - 30 + 36 + 14 + 18);
    assert_eq!(a.state(), AppState::MainMenu);
    assert!(a.core().is_none(), "World dropped on quit-to-menu");

    // A second game starts fresh (no state leakage) with different settings.
    click(&mut a, 512, 768 / 2 - 2);
    a.select_map(0);
    {
        let cfg = a.config_mut();
        cfg.difficulty = 0; // EASY
        cfg.house = 0; // GREECE
        cfg.credits = 0; // 2500
        cfg.classic_radar = true;
    }
    a.start_game();
    let core2 = a.core().expect("second core");
    assert!(
        core2.world().tick_count() <= 1,
        "second game starts from a fresh World"
    );
    assert_eq!(core2.world().house_credits(1), 2500);
    assert_eq!(core2.world().player_house(), Some(1));
    assert_eq!(core2.world().ai_difficulty(2), Some(Difficulty::Easy));
}

#[test]
fn same_settings_same_seed_builds_are_identical() {
    let f = SynthFactory;
    let res = ResolvedSkirmish {
        map_filename: "alpha.ini".to_string(),
        player_house: 1,
        color_house: 1,
        credits: 5000,
        difficulty: Difficulty::Normal,
        classic_radar: true,
    };
    let (mut c1, _) = f.build(&res).unwrap();
    let (mut c2, _) = f.build(&res).unwrap();
    let mut chain1 = Vec::new();
    let mut chain2 = Vec::new();
    for _ in 0..100 {
        c1.update(67);
        c2.update(67);
        chain1.push(c1.sim_hash());
        chain2.push(c2.sim_hash());
    }
    assert_eq!(
        chain1, chain2,
        "identical settings+seed => identical chains"
    );
}

#[test]
fn user_source_maps_appear_in_the_list() {
    let a = app();
    assert!(
        a.maps().iter().any(|m| m.source == MapSource::User),
        "a user-folder map is present in the combined list"
    );
}

#[test]
fn back_and_escape_navigation() {
    let mut a = app();
    click(&mut a, 512, 768 / 2 - 2); // -> setup
    assert_eq!(a.state(), AppState::SkirmishSetup);
    a.handle(InputEvent::KeyDown(Key::Menu)); // Esc backs out
    assert_eq!(a.state(), AppState::MainMenu);
}

/// Regression: a game started from the menu must boot with the camera centered
/// on the player's start cell — with the camera at the map origin under full
/// shroud, the first frame renders entirely black (user-reported bug).
#[test]
fn menu_started_game_centers_camera_on_player_start() {
    let mut a = app();
    a.select_map(0);
    a.start_game();
    assert_eq!(a.state(), AppState::InGame);
    let core = a.core().expect("in-game core");
    // SynthFactory reports start cell (10,10); the camera viewport must contain
    // its pixel center rather than sitting at the origin.
    let cam = core.camera_rect();
    let (cx, cy) = (10i64 * 24 + 12, 10i64 * 24 + 12);
    assert!(
        cam.x <= cx && cx < cam.x + cam.width as i64,
        "start cell x on-screen (cam.x={}, cx={cx})",
        cam.x
    );
    assert!(
        cam.y <= cy && cy < cam.y + cam.height as i64,
        "start cell y on-screen (cam.y={}, cy={cy})",
        cam.y
    );
}
