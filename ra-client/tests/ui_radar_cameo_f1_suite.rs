//! M7 UI coverage: the radar minimap, taller cameo sidebar rows, the F1
//! controls-hint overlay, and the cosmetic sound-cue queue (DESIGN.md §4.8;
//! §4.2 on the cosmetic layer never touching the sim). All four landed in
//! `ra-client/src/appcore.rs` as pure `AppCore` state/behavior — reachable
//! only through `handle`/`update`/`compose`/`compose_game`/`drain_sounds`,
//! never the macroquad shell — so every test here drives that seam exactly
//! the way `ui_scripted_drive.rs` does, black-box: the private geometry
//! functions (`radar_rect`, `radar_cell_at`, `sidebar_row_at`,
//! `sidebar_rows_top`, `sidebar_header_h`) are replicated independently
//! below from their doc comments/source rather than imported, so a test
//! failure here means the *black-box* behavior changed, not just that two
//! copies of the same formula drifted apart.
//!
//! Kept out of `ui_golden_frames.rs`/`ui_determinism.rs`/`ui_shroud_golden.rs`
//! deliberately (per the M7 task split): those pin `compose()`/`compose_game()`
//! hashes for the M2/M3 debug surface and the real-map shroud sweep and must
//! not be touched by new M7 coverage.

mod support;

use ra_client::appcore::{AppCore, SoundEvent};
use ra_client::input::{InputEvent, Key, MouseButton};
use ra_client::unit_render::{SpriteFrame, UnitSprite};
use ra_sim::coords::CellCoord;
use ra_sim::{BuildItem, Command, GameOver};

/// Pixels per cell edge, matching `appcore`'s private `CELL_PIXELS`
/// (`ICON_WIDTH`) — duplicated here for the same reason
/// `ui_scripted_drive.rs`'s own `CELL_PIXELS` already is (no public
/// accessor; a stable, documented constant).
const CELL_PIXELS: i32 = 24;

// ---------------------------------------------------------------------
// Black-box replicas of appcore's private M7 sidebar/radar geometry. Each
// mirrors the exact source (appcore.rs, `sidebar_header_h`/`radar_rect`/
// `radar_cell_at`/`sidebar_click`/`clamp_camera`) independently, rather than
// importing it, per this suite's whole point: these tests must fail if the
// *observable* geometry changes, not just if two copies of one formula
// drift apart.
// ---------------------------------------------------------------------

/// Sidebar cameo icon dimensions, matching `appcore`'s private `CAMEO_W`/
/// `CAMEO_H`.
const CAMEO_H: i32 = 48;
/// Taller sidebar row height when cameo art is installed, matching
/// `appcore`'s private `SIDEBAR_ROW_H_CAMEO` (`CAMEO_H + 12`).
const SIDEBAR_ROW_H_CAMEO: i32 = CAMEO_H + 12;
/// Radar minimap panel side length, matching `appcore`'s private
/// `RADAR_SIZE`.
const RADAR_SIZE: i32 = 120;
/// `sidebar_rows_top()`'s no-radar fallback, matching `appcore`'s
/// `font::GLYPH_H * 3 + 12` (same constant `ui_scripted_drive.rs`'s
/// `SIDEBAR_ROWS_TOP` already pins).
const NO_RADAR_ROWS_TOP: i32 = 7 * 3 + 12;

/// `appcore::AppCore::sidebar_header_h()`, replicated black-box: `2 +
/// (GLYPH_H+2) + GLYPH_H + 4` — the y0 the radar panel sits at.
fn sidebar_header_h() -> i32 {
    2 + (ra_client::font::GLYPH_H + 2) + ra_client::font::GLYPH_H + 4
}

/// `appcore::AppCore::radar_rect()`, replicated black-box: `(x0, y0, size)`
/// in viewport pixels. Only meaningful on a core that called
/// `enable_radar()` (this suite never calls it otherwise).
fn radar_rect(core: &AppCore) -> (i32, i32, i32) {
    (
        core.tactical_width() as i32 + 2,
        sidebar_header_h(),
        RADAR_SIZE,
    )
}

/// `appcore::AppCore::radar_cell_at(x, y)`, replicated black-box: the map
/// cell a radar-panel viewport pixel corresponds to, or `None` if `(x, y)`
/// misses the panel.
fn expected_radar_cell(core: &AppCore, x: i32, y: i32) -> Option<CellCoord> {
    let (rx, ry, size) = radar_rect(core);
    if x < rx || x >= rx + size || y < ry || y >= ry + size {
        return None;
    }
    let mw = (core.map_width() as i32 / CELL_PIXELS).max(1) as i64;
    let mh = (core.map_height() as i32 / CELL_PIXELS).max(1) as i64;
    let cx = (x - rx) as i64 * mw / size as i64;
    let cy = (y - ry) as i64 * mh / size as i64;
    Some(CellCoord::new(cx as i32, cy as i32))
}

/// `appcore::AppCore::sidebar_click`'s radar-jump math plus
/// `AppCore::clamp_camera`, replicated black-box: the `(cam_x, cam_y)` a
/// radar click on `cell` should leave the camera at.
fn expected_camera_after_radar_jump(core: &AppCore, cell: CellCoord) -> (f32, f32) {
    let (_, viewport_h) = core.viewport_size();
    let px = (cell.x * CELL_PIXELS - core.tactical_width() as i32 / 2) as f32;
    let py = (cell.y * CELL_PIXELS - viewport_h as i32 / 2) as f32;
    let max_x = (core.map_width() as f32 - core.tactical_width() as f32).max(0.0);
    let max_y = (core.map_height() as f32 - viewport_h as f32).max(0.0);
    (px.clamp(0.0, max_x), py.clamp(0.0, max_y))
}

// ---------------------------------------------------------------------
// 1. Radar click-to-jump (scripted).
// ---------------------------------------------------------------------

#[test]
fn radar_click_jumps_camera_to_expected_cell() {
    let (mut core, _mcv) = support::synthetic_core_with_econ_radar_cameo(0x7ADA_0001, 2000);
    core.handle(InputEvent::Resize {
        width: 900,
        height: 600,
    });
    core.set_camera(0.0, 0.0);

    let (rx, ry, _) = radar_rect(&core);
    let (cx, cy) = (rx + 40, ry + 70);
    let cell = expected_radar_cell(&core, cx, cy)
        .expect("test setup: this click should land inside the radar panel");
    let (want_x, want_y) = expected_camera_after_radar_jump(&core, cell);

    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x: cx,
        y: cy,
    });

    let r = core.camera_rect();
    assert_eq!(r.x, want_x.round() as i64);
    assert_eq!(r.y, want_y.round() as i64);
    assert!(
        core.drain_commands().is_empty(),
        "a radar click must never emit a sim command"
    );
}

#[test]
fn radar_click_boundary_pixels_both_map_inside_the_panel() {
    let (mut core, _mcv) = support::synthetic_core_with_econ_radar_cameo(0x7ADA_0002, 2000);
    core.handle(InputEvent::Resize {
        width: 900,
        height: 600,
    });
    let (rx, ry, size) = radar_rect(&core);

    // Top-left corner pixel (x0, y0).
    core.set_camera(500.0, 500.0);
    let cell_tl =
        expected_radar_cell(&core, rx, ry).expect("(x0,y0) should map inside the radar panel");
    let (want_x, want_y) = expected_camera_after_radar_jump(&core, cell_tl);
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x: rx,
        y: ry,
    });
    let r = core.camera_rect();
    assert_eq!((r.x, r.y), (want_x.round() as i64, want_y.round() as i64));
    assert_ne!(
        (r.x, r.y),
        (500, 500),
        "a jump must actually have happened at the top-left corner pixel"
    );

    // Bottom-right corner pixel (x0+size-1, y0+size-1).
    core.set_camera(0.0, 0.0);
    let (bx, by) = (rx + size - 1, ry + size - 1);
    let cell_br = expected_radar_cell(&core, bx, by)
        .expect("(x0+size-1,y0+size-1) should map inside the radar panel");
    let (want_x2, want_y2) = expected_camera_after_radar_jump(&core, cell_br);
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x: bx,
        y: by,
    });
    let r2 = core.camera_rect();
    assert_eq!(
        (r2.x, r2.y),
        (want_x2.round() as i64, want_y2.round() as i64)
    );
    assert_ne!(
        (r2.x, r2.y),
        (0, 0),
        "a jump must actually have happened at the bottom-right corner pixel"
    );
}

#[test]
fn radar_click_one_pixel_outside_each_edge_does_not_jump() {
    let (mut core, _mcv) = support::synthetic_core_with_econ_radar_cameo(0x7ADA_0003, 2000);
    core.handle(InputEvent::Resize {
        width: 900,
        height: 600,
    });
    let (rx, ry, size) = radar_rect(&core);

    // Each point is exactly one pixel outside one edge of the panel, still
    // well above `sidebar_rows_top()` (146 with the radar enabled) so a miss
    // here can't accidentally be reinterpreted as a buildable-row hit.
    let outside_points = [
        (rx - 1, ry),    // 1px left of the panel
        (rx + size, ry), // 1px right of the panel
        (rx, ry - 1),    // 1px above the panel
        (rx, ry + size), // 1px below the panel
    ];
    for &(x, y) in &outside_points {
        assert_eq!(
            expected_radar_cell(&core, x, y),
            None,
            "test setup: ({x},{y}) should be just outside the radar panel"
        );
        core.set_camera(321.0, 654.0);
        core.handle(InputEvent::MouseDown {
            button: MouseButton::Left,
            x,
            y,
        });
        let r = core.camera_rect();
        assert_eq!(
            (r.x, r.y),
            (321, 654),
            "a click just outside the radar panel at ({x},{y}) must not move the camera"
        );
        assert!(core.drain_commands().is_empty());
    }
}

/// A radar-enabled, sidebar-enabled core that resolves to `GameOver::Victory`
/// within a couple of ticks (house 2 starts with no assets at all). See
/// `ui_gameover.rs`'s `gameover_fixture`/module doc (structural finding 2):
/// `AppCore` has no `world_mut()`, so elimination must be pre-arranged on the
/// raw `World` before `AppCore::with_sim` wraps it — `game_over()` still
/// resolves for real, inside a real `tick()`, the first time `update()` runs.
fn radar_victory_fixture(seed: u32) -> AppCore {
    use ra_sim::coords::Facing;
    use ra_sim::{
        AiPlayer, BuildingProto, Catalog, Difficulty, EconRules, MoveStats, Passability, World,
    };

    let (raster, palette) = support::synthetic_fixture();
    let mut world = World::new(Passability::all_passable(), seed);
    world.set_catalog(Catalog {
        buildings: vec![BuildingProto {
            is_barracks: false,
            name: "HUT".to_string(),
            foot_w: 1,
            foot_h: 1,
            max_health: 100,
            armor: 0,
            power: 0,
            cost: 10,
            prereq: vec![],
            is_refinery: false,
            is_construction_yard: false,
            is_war_factory: false,
            free_harvester_unit: None,
            sight: 2,
            sprite_id: 0,
            weapon: None,
            has_turret: false,
            charges: false,
            is_wall: false,
        }],
        units: vec![],
        econ: EconRules::default(),
    });
    world.init_houses(3, 1000);
    world.spawn_building(0, 1, CellCoord::new(20, 20)).unwrap();
    world.spawn_unit(
        0,
        1,
        CellCoord::new(25, 25),
        Facing(0),
        100,
        MoveStats {
            max_speed: 20,
            rot: 8,
        },
    );
    // House 2 (AI) starts with nothing: house 1 wins the moment
    // `update_game_over` first runs.
    world.set_player_house(1);
    world.set_ai(vec![AiPlayer::new(2, Difficulty::Normal)]);

    let mut core = AppCore::with_sim(raster.clone(), *palette, world, Vec::new(), Vec::new());
    core.enable_sidebar(1, Vec::new());
    core.enable_radar();
    core
}

#[test]
fn radar_click_still_jumps_camera_after_game_over_navigation_only() {
    let mut core = radar_victory_fixture(0x7ADA_0004);
    core.handle(InputEvent::Resize {
        width: 900,
        height: 600,
    });
    for _ in 0..3 {
        core.update(67);
    }
    assert_eq!(
        core.game_over(),
        GameOver::Victory,
        "test setup: should be terminal"
    );
    core.drain_commands();

    let (rx, ry, _) = radar_rect(&core);
    let (cx, cy) = (rx + 30, ry + 30);
    let cell =
        expected_radar_cell(&core, cx, cy).expect("test setup: click should land inside the panel");
    let (want_x, want_y) = expected_camera_after_radar_jump(&core, cell);

    core.set_camera(999.0, 999.0);
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x: cx,
        y: cy,
    });
    let r = core.camera_rect();
    assert_eq!(
        (r.x, r.y),
        (want_x.round() as i64, want_y.round() as i64),
        "radar click-to-jump must work even once the game is over (navigation only)"
    );
    assert!(
        core.drain_commands().is_empty(),
        "a radar click must never emit a sim command, game over or not"
    );
}

// ---------------------------------------------------------------------
// 2. Cameo row hit-testing with taller rows.
// ---------------------------------------------------------------------

/// A minimal, deterministic stand-in for a decoded cameo icon (see
/// `support::fake_cameo_sprite`, which this mirrors — duplicated rather than
/// exported since it's a two-line literal, not worth widening `support`'s
/// public surface for).
fn fake_cameo_sprite(index: u8) -> UnitSprite {
    const CAMEO_W: u32 = 64;
    const CAMEO_H: u32 = 48;
    UnitSprite {
        frames: vec![SpriteFrame {
            width: CAMEO_W,
            height: CAMEO_H,
            pixels: vec![index.max(1); (CAMEO_W * CAMEO_H) as usize],
        }],
    }
}

/// A tiny catalog with three units, no prereqs beyond a war factory, so all
/// three sidebar rows are simultaneously `buildable` on a fresh core —
/// letting each row-hit-testing case below independently confirm the row it
/// clicked via the specific `StartProduction` item it gets back, instead of
/// fighting `econ_catalog`'s POWR->PROC->WEAP prereq chain (irrelevant to a
/// pure "(x, y) maps to which row index" question). The radar is left
/// disabled here (unlike `support::synthetic_core_with_econ_radar_cameo`) so
/// `sidebar_rows_top()` stays at the simpler no-radar fallback
/// (`NO_RADAR_ROWS_TOP`) — this test is about row *height*, not the radar
/// panel, which item 1 above already covers.
fn cameo_row_core(seed: u32) -> AppCore {
    use ra_sim::{BuildingProto, Catalog, EconRules, MoveStats, Passability, UnitProto, World};

    let bproto = |name: &str, wf: bool| BuildingProto {
        is_barracks: false,
        name: name.to_string(),
        foot_w: 2,
        foot_h: 2,
        max_health: 400,
        armor: 0,
        power: 0,
        cost: 10,
        prereq: vec![],
        is_refinery: false,
        is_construction_yard: false,
        is_war_factory: wf,
        free_harvester_unit: None,
        sight: 4,
        sprite_id: 0,
        weapon: None,
        has_turret: false,
        charges: false,
        is_wall: false,
    };
    let uproto = |name: &str| UnitProto {
        is_infantry: false,
        locomotor: 1,
        name: name.to_string(),
        sprite_id: 0,
        max_health: 100,
        stats: MoveStats {
            max_speed: 20,
            rot: 8,
        },
        armor: 0,
        weapon: None,
        secondary: None,
        has_turret: false,
        is_harvester: false,
        deploys_to: None,
        cost: 10,
        prereq: vec![],
        sight: 2,
    };

    let mut world = World::new(Passability::all_passable(), seed);
    world.set_catalog(Catalog {
        buildings: vec![bproto("WEAP", true)],
        units: vec![uproto("AAA"), uproto("BBB"), uproto("CCC")],
        econ: EconRules::default(),
    });
    world.init_houses(3, 5000);
    world.spawn_building(0, 1, CellCoord::new(20, 20)).unwrap();

    let (raster, palette) = support::synthetic_fixture();
    let mut core = AppCore::with_sim(raster.clone(), *palette, world, Vec::new(), Vec::new());
    core.enable_sidebar(
        1,
        vec![BuildItem::Unit(0), BuildItem::Unit(1), BuildItem::Unit(2)],
    );
    core.set_cameo_art(vec![
        Some(fake_cameo_sprite(1)),
        Some(fake_cameo_sprite(2)),
        Some(fake_cameo_sprite(3)),
    ]);
    core
}

/// One build-column width (`appcore`'s private `COLUMN_W = CAMEO_W = 64`).
const COLUMN_W: i32 = 64;

/// A sidebar-strip x coordinate for `core`'s current viewport. The fixture's
/// buildables are all `BuildItem::Unit`s, which live in the **right** strip
/// (column 1) of the two-strip sidebar (M7.7 P6 / `Which_Column`), so this
/// points into column 1.
fn sidebar_x(core: &AppCore) -> i32 {
    core.tactical_width() as i32 + 1 + COLUMN_W + 10
}

#[test]
fn cameo_row_click_selects_correct_buildable_index_row0() {
    let mut core = cameo_row_core(0xCA3E_0001);
    core.handle(InputEvent::Resize {
        width: 500,
        height: 500,
    });
    let x = sidebar_x(&core);
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x,
        y: NO_RADAR_ROWS_TOP + 5,
    });
    let emitted = core.drain_commands();
    assert_eq!(emitted.len(), 1);
    match emitted[0] {
        Command::StartProduction { house, item } => {
            assert_eq!(house, 1);
            assert_eq!(
                item,
                BuildItem::Unit(0),
                "row 0 should start the first buildable (AAA)"
            );
        }
        other => panic!("expected StartProduction, got {other:?}"),
    }
}

#[test]
fn cameo_row_click_selects_correct_buildable_index_row1() {
    let mut core = cameo_row_core(0xCA3E_0002);
    core.handle(InputEvent::Resize {
        width: 500,
        height: 500,
    });
    let x = sidebar_x(&core);
    let y = NO_RADAR_ROWS_TOP + SIDEBAR_ROW_H_CAMEO + 5;
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x,
        y,
    });
    let emitted = core.drain_commands();
    assert_eq!(emitted.len(), 1);
    match emitted[0] {
        Command::StartProduction { house, item } => {
            assert_eq!(house, 1);
            assert_eq!(
                item,
                BuildItem::Unit(1),
                "row 1 should start the second buildable (BBB)"
            );
        }
        other => panic!("expected StartProduction, got {other:?}"),
    }
}

#[test]
fn cameo_row_click_selects_correct_buildable_index_row2() {
    let mut core = cameo_row_core(0xCA3E_0003);
    core.handle(InputEvent::Resize {
        width: 500,
        height: 500,
    });
    let x = sidebar_x(&core);
    let y = NO_RADAR_ROWS_TOP + 2 * SIDEBAR_ROW_H_CAMEO + 5;
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x,
        y,
    });
    let emitted = core.drain_commands();
    assert_eq!(emitted.len(), 1);
    match emitted[0] {
        Command::StartProduction { house, item } => {
            assert_eq!(house, 1);
            assert_eq!(
                item,
                BuildItem::Unit(2),
                "row 2 should start the third buildable (CCC)"
            );
        }
        other => panic!("expected StartProduction, got {other:?}"),
    }
}

#[test]
fn cameo_row_boundary_one_pixel_above_top_is_no_hit() {
    let mut core = cameo_row_core(0xCA3E_0004);
    core.handle(InputEvent::Resize {
        width: 500,
        height: 500,
    });
    let x = sidebar_x(&core);
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x,
        y: NO_RADAR_ROWS_TOP - 1,
    });
    assert!(
        core.drain_commands().is_empty(),
        "1px above the first row's top edge must not hit row 0"
    );
}

#[test]
fn cameo_row_boundary_between_row0_and_row1_lands_in_row1_not_row0() {
    let mut core = cameo_row_core(0xCA3E_0005);
    core.handle(InputEvent::Resize {
        width: 500,
        height: 500,
    });
    let x = sidebar_x(&core);
    // Exactly the y where row 0 ends and row 1 begins.
    let y = NO_RADAR_ROWS_TOP + SIDEBAR_ROW_H_CAMEO;
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x,
        y,
    });
    let emitted = core.drain_commands();
    assert_eq!(emitted.len(), 1);
    match emitted[0] {
        Command::StartProduction { item, .. } => {
            assert_eq!(
                item,
                BuildItem::Unit(1),
                "the row0/row1 boundary pixel must land in row 1 (BBB), not row 0 (AAA) — an \
                 off-by-one here would start AAA instead"
            );
        }
        other => panic!("expected StartProduction, got {other:?}"),
    }
}

#[test]
fn cameo_row_click_past_last_item_is_a_noop_not_a_panic() {
    let mut core = cameo_row_core(0xCA3E_0006);
    core.handle(InputEvent::Resize {
        width: 500,
        height: 500,
    });
    let x = sidebar_x(&core);
    // Row index 3: one past the last real item (indices 0..=2).
    let y = NO_RADAR_ROWS_TOP + 3 * SIDEBAR_ROW_H_CAMEO + 5;
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x,
        y,
    });
    assert!(
        core.drain_commands().is_empty(),
        "a click past the last sidebar row must be a no-op"
    );
    // The render path must not panic either.
    let _ = core.compose_game();
}

// ---------------------------------------------------------------------
// 3. F1 help overlay: pixel presence/absence, no leak into compose().
// ---------------------------------------------------------------------

#[test]
fn help_overlay_starts_hidden_and_f1_toggles_it() {
    let mut core = support::synthetic_core();
    core.handle(InputEvent::Resize {
        width: 320,
        height: 240,
    });
    assert!(
        !core.help_visible(),
        "AppCore::with_sim initializes show_help to false"
    );

    core.handle(InputEvent::KeyDown(Key::Help));
    core.handle(InputEvent::KeyUp(Key::Help));
    assert!(core.help_visible(), "F1 down should toggle the overlay on");

    core.handle(InputEvent::KeyDown(Key::Help));
    core.handle(InputEvent::KeyUp(Key::Help));
    assert!(
        !core.help_visible(),
        "a second F1 down should toggle it back off"
    );
}

#[test]
fn help_overlay_changes_compose_game_pixels_but_never_leaks_into_compose() {
    let mut core = support::synthetic_core();
    core.handle(InputEvent::Resize {
        width: 320,
        height: 240,
    });
    core.set_camera(10.0, 10.0);

    assert!(!core.help_visible());
    let game_off = core.compose_game();
    let raw_off = core.compose(core.camera_rect());

    core.set_help_visible(true);
    let game_on = core.compose_game();
    let raw_on = core.compose(core.camera_rect());

    assert_ne!(
        support::fnv1a(&game_off.pixels),
        support::fnv1a(&game_on.pixels),
        "toggling F1 must visibly change compose_game()'s output"
    );
    assert_eq!(
        support::fnv1a(&raw_off.pixels),
        support::fnv1a(&raw_on.pixels),
        "compose() (the debug/raw-terrain surface the M2/M3 goldens pin) must be byte-identical \
         whether help is visible or not — the F1 overlay must only ever paint into \
         compose_game()"
    );

    // Toggling back off should reproduce the exact off-state frame — no
    // side effect beyond the visibility flag itself.
    core.set_help_visible(false);
    let game_off2 = core.compose_game();
    assert_eq!(
        support::fnv1a(&game_off.pixels),
        support::fnv1a(&game_off2.pixels),
        "toggling help off again should reproduce the exact off-state frame"
    );
}

// ---------------------------------------------------------------------
// 4. SoundEvent queue: same-script-twice determinism.
// ---------------------------------------------------------------------

/// Battle script: box-select the 3 armed jeeps, right-click the unarmed
/// target (an `Attack` order), step until the target dies. Reliably produces
/// `Fire` (the jeeps' cannons), `Explosion` (the target's death), and
/// `Select` (the box-select itself) cues. Mirrors
/// `ui_scripted_drive.rs`'s `synthetic_battle_attack_kill_and_health_bar_rendering`
/// script (same fixture, same click coordinates), reusing an
/// already-pinned-reliable combat pacing rather than inventing a new one.
fn run_battle_sound_script(seed: u32) -> Vec<SoundEvent> {
    let (mut core, _jeeps, target) = support::synthetic_core_with_armed_units(seed);
    core.handle(InputEvent::Resize {
        width: 480,
        height: 320,
    });
    core.set_camera(0.0, 0.0);

    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x: 0,
        y: 0,
    });
    core.handle(InputEvent::MouseMoved { x: 370, y: 280 });
    core.handle(InputEvent::MouseUp {
        button: MouseButton::Left,
        x: 370,
        y: 280,
    });

    let target_sx = 16 * CELL_PIXELS + CELL_PIXELS / 2;
    let target_sy = 10 * CELL_PIXELS + CELL_PIXELS / 2;
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Right,
        x: target_sx,
        y: target_sy,
    });

    let mut sounds = Vec::new();
    for _ in 0..600 {
        core.update(support::TICK_MS);
        sounds.extend(core.drain_sounds());
        if !core.world().units.contains(target) {
            break;
        }
    }
    // A few settling ticks so any straggler cue queued the same tick as
    // death is captured by a subsequent drain too.
    for _ in 0..5 {
        core.update(support::TICK_MS);
        sounds.extend(core.drain_sounds());
    }
    sounds
}

#[test]
fn battle_sound_sequence_is_deterministic_across_reruns_and_covers_fire_explosion_select() {
    let seed = 0x50D0_0001;
    let run1 = run_battle_sound_script(seed);
    let run2 = run_battle_sound_script(seed);
    assert_eq!(
        run1, run2,
        "identical script reruns must give identical sound-event sequences"
    );

    assert!(
        run1.contains(&SoundEvent::Select),
        "box-select should have queued a Select cue"
    );
    assert!(
        run1.contains(&SoundEvent::Fire),
        "the armed jeeps firing should have queued a Fire cue"
    );
    assert!(
        run1.contains(&SoundEvent::Explosion),
        "the target's death should have queued an Explosion cue"
    );
}

/// Econ script: select + deploy the starter MCV. Reliably produces `Select`
/// (the click) and `ConstructionComplete` (the fresh construction yard is a
/// *player* building appearing where none was before — see
/// `AppCore::step_tick`'s `player_built` check).
fn run_econ_deploy_sound_script(seed: u32) -> Vec<SoundEvent> {
    let (mut core, _mcv) = support::synthetic_core_with_econ(seed, 2000);
    core.handle(InputEvent::Resize {
        width: 900,
        height: 600,
    });
    let mcv_cell = support::econ_mcv_cell();
    let cam = (
        (mcv_cell.x * CELL_PIXELS) as f32 - 300.0,
        (mcv_cell.y * CELL_PIXELS) as f32 - 300.0,
    );
    core.set_camera(cam.0, cam.1);
    let (sx, sy) = (
        mcv_cell.x * CELL_PIXELS + CELL_PIXELS / 2 - cam.0 as i32,
        mcv_cell.y * CELL_PIXELS + CELL_PIXELS / 2 - cam.1 as i32,
    );
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x: sx,
        y: sy,
    });
    core.handle(InputEvent::MouseUp {
        button: MouseButton::Left,
        x: sx,
        y: sy,
    });
    core.handle(InputEvent::KeyDown(Key::Deploy));
    core.handle(InputEvent::KeyUp(Key::Deploy));

    let mut sounds = Vec::new();
    for _ in 0..10 {
        core.update(support::TICK_MS);
        sounds.extend(core.drain_sounds());
    }
    sounds
}

#[test]
fn econ_deploy_sound_sequence_is_deterministic_across_reruns_and_covers_construction_complete() {
    let seed = 0x50D0_0002;
    let run1 = run_econ_deploy_sound_script(seed);
    let run2 = run_econ_deploy_sound_script(seed);
    assert_eq!(
        run1, run2,
        "identical script reruns must give identical sound-event sequences"
    );

    assert!(
        run1.contains(&SoundEvent::Select),
        "clicking the MCV should have queued a Select cue"
    );
    assert!(
        run1.contains(&SoundEvent::ConstructionComplete),
        "deploying into a construction yard should have queued a ConstructionComplete cue"
    );
}

#[test]
fn drain_sounds_empties_the_queue() {
    let (mut core, _mcv) = support::synthetic_core_with_econ(0x50D0_0003, 2000);
    core.handle(InputEvent::Resize {
        width: 900,
        height: 600,
    });
    let mcv_cell = support::econ_mcv_cell();
    let (sx, sy) = (
        mcv_cell.x * CELL_PIXELS + CELL_PIXELS / 2,
        mcv_cell.y * CELL_PIXELS + CELL_PIXELS / 2,
    );
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x: sx,
        y: sy,
    });
    core.handle(InputEvent::MouseUp {
        button: MouseButton::Left,
        x: sx,
        y: sy,
    });
    assert!(
        !core.drain_sounds().is_empty(),
        "test setup: the click-select should have queued a Select cue"
    );
    assert!(
        core.drain_sounds().is_empty(),
        "a second immediate drain must return nothing new"
    );
}

// ===========================================================================
// Two-strip scrolling sidebar (M7.7 P6) — smoke coverage. Full coverage
// (monkey over scroll events, per-column overflow, resize re-clamp) is
// ra-tester's to build out; these pin the core contract.
// ===========================================================================

/// A sidebar fixture with one structure (column 0) and many units (column 1),
/// plus cameo art, so column 1 overflows a short viewport and must scroll.
fn two_strip_core(seed: u32, n_units: usize) -> AppCore {
    use ra_sim::{BuildingProto, Catalog, EconRules, MoveStats, Passability, UnitProto, World};
    let bproto = BuildingProto {
        is_barracks: false,
        name: "WEAP".into(),
        foot_w: 2,
        foot_h: 2,
        max_health: 400,
        armor: 0,
        power: 0,
        cost: 10,
        prereq: vec![],
        is_refinery: false,
        // A construction yard so both the structure row (needs a yard present to
        // be buildable) and the unit rows (need the war factory) are clickable.
        is_construction_yard: true,
        is_war_factory: true,
        free_harvester_unit: None,
        sight: 4,
        sprite_id: 0,
        weapon: None,
        has_turret: false,
        charges: false,
        is_wall: false,
    };
    let uproto = |name: String| UnitProto {
        is_infantry: false,
        locomotor: 1,
        name,
        sprite_id: 0,
        max_health: 100,
        stats: MoveStats {
            max_speed: 20,
            rot: 8,
        },
        armor: 0,
        weapon: None,
        secondary: None,
        has_turret: false,
        is_harvester: false,
        deploys_to: None,
        cost: 10,
        prereq: vec![],
        sight: 2,
    };
    let units: Vec<UnitProto> = (0..n_units).map(|i| uproto(format!("U{i}"))).collect();
    let mut world = World::new(Passability::all_passable(), seed);
    world.set_catalog(Catalog {
        buildings: vec![bproto],
        units,
        econ: EconRules::default(),
    });
    world.init_houses(3, 5000);
    world.spawn_building(0, 1, CellCoord::new(20, 20)).unwrap();

    let (raster, palette) = support::synthetic_fixture();
    let mut core = AppCore::with_sim(raster.clone(), *palette, world, Vec::new(), Vec::new());
    let mut buildables = vec![BuildItem::Building(0)];
    buildables.extend((0..n_units).map(|i| BuildItem::Unit(i as u32)));
    core.enable_sidebar(1, buildables);
    core.set_cameo_art(
        (0..=n_units)
            .map(|i| Some(fake_cameo_sprite(i as u8 + 1)))
            .collect(),
    );
    // Short viewport so the units column cannot fit all its rows.
    core.handle(InputEvent::MouseLeft);
    core
}

fn only_start(core: &mut AppCore) -> Option<BuildItem> {
    let cmds = core.drain_commands();
    match cmds.as_slice() {
        [Command::StartProduction { item, .. }] => Some(*item),
        _ => None,
    }
}

#[test]
fn structures_go_left_column_units_go_right_column() {
    let mut core = two_strip_core(0x5B01_0001, 3);
    core.handle(InputEvent::Resize {
        width: 500,
        height: 500,
    });
    let top = NO_RADAR_ROWS_TOP + 5;
    let tw = core.tactical_width() as i32;
    // Column 0 (left) row 0 → the structure.
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x: tw + 1 + 10,
        y: top,
    });
    assert_eq!(only_start(&mut core), Some(BuildItem::Building(0)));
    // Column 1 (right) row 0 → the first unit.
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x: tw + 1 + COLUMN_W + 10,
        y: top,
    });
    assert_eq!(only_start(&mut core), Some(BuildItem::Unit(0)));
}

#[test]
fn scrolling_the_units_column_shifts_which_row0_builds() {
    // Many units + a short viewport → column 1 overflows and scrolls.
    let mut core = two_strip_core(0x5B01_0002, 12);
    core.handle(InputEvent::Resize {
        width: 500,
        height: 260,
    });
    assert_eq!(core.sidebar_scroll(1), 0, "starts un-scrolled");
    let tw = core.tactical_width() as i32;
    let ux = tw + 1 + COLUMN_W + 10;
    let y0 = NO_RADAR_ROWS_TOP + 5;

    // Row 0 builds unit 0 before scrolling.
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x: ux,
        y: y0,
    });
    assert_eq!(only_start(&mut core), Some(BuildItem::Unit(0)));

    // Scroll the units column (col 1) down twice.
    core.handle(InputEvent::SidebarScroll {
        column: 1,
        up: false,
    });
    core.handle(InputEvent::SidebarScroll {
        column: 1,
        up: false,
    });
    assert_eq!(core.sidebar_scroll(1), 2);

    // Row 0 now builds unit 2 (the window slid down by two).
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x: ux,
        y: y0,
    });
    assert_eq!(only_start(&mut core), Some(BuildItem::Unit(2)));

    // Scrolling up past the top clamps at 0.
    for _ in 0..10 {
        core.handle(InputEvent::SidebarScroll {
            column: 1,
            up: true,
        });
    }
    assert_eq!(core.sidebar_scroll(1), 0);

    // Column 0 (one structure) never scrolls.
    core.handle(InputEvent::SidebarScroll {
        column: 0,
        up: false,
    });
    assert_eq!(
        core.sidebar_scroll(0),
        0,
        "a non-overflowing column stays put"
    );
}
