//! Scripted end-to-end drive (DESIGN.md §4.8 layer 1) for the M7.7 P6
//! two-strip scrolling sidebar: a genuine multi-step user flow — scroll a
//! column until an item that started off-screen lands at row 0, click it to
//! start production, tick until it completes, then confirm it actually
//! enters the world at a sane location — through `AppCore`'s real
//! `handle`/`update` seam, not just the single-op smoke assertions already
//! pinned in `ui_radar_cameo_f1_suite.rs`'s "Two-strip scrolling sidebar"
//! section (`two_strip_core`/`structures_go_left_column_units_go_right_column`/
//! `scrolling_the_units_column_shifts_which_row0_builds`).
//!
//! Two variants, one per column, because the two columns behave differently
//! once production completes (`ra_sim::world::finish_or_retry`,
//! `sidebar.cpp`/`factory.cpp` equivalents):
//! - **Units** (right column): a completed vehicle build has no manual
//!   placement step at all — it auto-exits onto a free cell next to the
//!   producing war factory (`find_factory_exit`/`spawn_produced_unit`).
//!   `AppCore::begin_placement`/`place_at` only ever handle
//!   `BuildItem::Building` (see `appcore.rs`'s `sidebar_click`: the `ready &&
//!   BuildItem::Building` arm is the only path into placement mode) — so
//!   there is no `InputEvent` for "place this tank"; the realistic flow ends
//!   at "tick until it spawns, assert it landed somewhere sane".
//! - **Structures** (left column): a completed building becomes
//!   `ready_building` and genuinely needs a second sidebar click (which
//!   re-enters placement mode) plus a tactical-map click to actually enter
//!   the world at a chosen cell — the flow the task brief's "issue a
//!   placement command at a valid map cell" describes literally.

mod support;

use ra_client::appcore::AppCore;
use ra_client::input::{InputEvent, MouseButton};
use ra_sim::coords::CellCoord;
use ra_sim::{
    BuildItem, BuildingProto, Catalog, Command, EconRules, MoveStats, Passability, UnitProto, World,
};

/// Pixels per cell edge (`appcore`'s private `CELL_PIXELS`/`ICON_WIDTH`) —
/// duplicated here for the same reason every other UI suite already does
/// (no public accessor; see `ui_scripted_drive.rs`'s own copy).
const CELL_PIXELS: i32 = 24;
/// `sidebar_rows_top()`'s no-radar fallback (`font::GLYPH_H * 3 + 12`) —
/// these fixtures never call `enable_radar`, so this is always the active
/// geometry. Matches `ui_scripted_drive.rs`'s `SIDEBAR_ROWS_TOP` and
/// `ui_radar_cameo_f1_suite.rs`'s `NO_RADAR_ROWS_TOP`.
const NO_RADAR_ROWS_TOP: i32 = 7 * 3 + 12;
/// One build-column width (`appcore`'s private `COLUMN_W = CAMEO_W = 64`).
const COLUMN_W: i32 = 64;
/// Per-row height with no cameo art installed (`appcore`'s private
/// `SIDEBAR_ROW_H`) — neither fixture below calls `set_cameo_art`.
const SIDEBAR_ROW_H: i32 = 22;
/// Height of the per-column scroll-button row (`appcore`'s private
/// `SCROLL_BTN_H`), reserved at the bottom of the visible-rows band.
const SCROLL_BTN_H: i32 = 14;

/// `appcore::AppCore::sidebar_visible_rows()`, replicated black-box (no
/// public accessor — same rationale as every other duplicated-geometry
/// constant in this file): how many cameo rows fit in `viewport_h` pixels
/// with the radar disabled and no cameo art installed. Used only to assert
/// the test's own fixture setup actually produces the short viewport this
/// suite depends on for its "started off-screen" claims.
fn expected_visible_rows(viewport_h: i32) -> i32 {
    let avail = viewport_h - NO_RADAR_ROWS_TOP - SCROLL_BTN_H;
    (avail / SIDEBAR_ROW_H).max(1)
}

// ---------------------------------------------------------------------
// Fixture 1: units-only sidebar (column 0 empty — incidentally also
// covering "scroll a zero-row column" for this specific realistic flow;
// the monkey suite covers that adversarially in general).
// ---------------------------------------------------------------------

/// A war factory already standing (built directly, bypassing the MCV-deploy
/// dance — irrelevant to this suite's actual subject, already covered by
/// `ui_scripted_drive.rs`'s `synthetic_deploy_via_key_event_creates_construction_yard`)
/// plus `n_units` buildable vehicle types, all with no power draw so
/// production always runs at full speed (no low-power multiplier to budget
/// tick counts around). Each unit proto's `sprite_id` is set to its own
/// catalog index (not left at a shared `0`) so a spawned `Unit::type_id`
/// (`world.rs`'s `spawn_produced_unit` uses `proto.sprite_id`, not the
/// catalog id, as the spawned type) can still be matched back to the
/// `BuildItem::Unit` index that was ordered. Returns the core and the war
/// factory's cell (for the "spawned near its factory" assertion).
fn units_overflow_core(seed: u32, n_units: usize) -> (AppCore, CellCoord) {
    let fact = BuildingProto {
        is_barracks: false,
        name: "FACT".into(),
        foot_w: 3,
        foot_h: 3,
        max_health: 400,
        armor: 0,
        power: 0,
        cost: 100,
        prereq: vec![],
        is_refinery: false,
        is_construction_yard: true,
        is_war_factory: false,
        free_harvester_unit: None,
        sight: 4,
        sprite_id: 0,
        weapon: None,
        has_turret: false,
        charges: false,
        is_wall: false,
        storage: 0,
    };
    let weap = BuildingProto {
        is_barracks: false,
        name: "WEAP".into(),
        foot_w: 3,
        foot_h: 3,
        max_health: 400,
        armor: 0,
        power: 0, // no power draw -> full build speed, no throttle math needed
        cost: 60,
        prereq: vec![],
        is_refinery: false,
        is_construction_yard: false,
        is_war_factory: true,
        free_harvester_unit: None,
        sight: 4,
        sprite_id: 0,
        weapon: None,
        has_turret: false,
        charges: false,
        is_wall: false,
        storage: 0,
    };
    // `sprite_id` (not the catalog index) becomes the spawned `Unit::type_id`
    // (`world.rs`'s `spawn_produced_unit`: `world.spawn_unit(proto.sprite_id,
    // ...)`) -- set it to the catalog index `i` so a spawned unit can be
    // identified by the same id the sidebar/`BuildItem::Unit` uses.
    let uproto = |i: usize, name: String| UnitProto {
        is_infantry: false,
        locomotor: 1,
        name,
        sprite_id: i as u32,
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
    let units: Vec<UnitProto> = (0..n_units).map(|i| uproto(i, format!("U{i}"))).collect();

    let mut world = World::new(Passability::all_passable(), seed);
    world.set_catalog(Catalog {
        buildings: vec![fact, weap],
        units,
        econ: EconRules::default(),
    });
    world.init_houses(3, 5000);

    let yard_cell = CellCoord::new(20, 20);
    let weap_cell = CellCoord::new(24, 20);
    world.spawn_building(0, 1, yard_cell).unwrap();
    world.spawn_building(1, 1, weap_cell).unwrap();

    let (raster, palette) = support::synthetic_fixture();
    let mut core = AppCore::with_sim(raster.clone(), *palette, world, Vec::new(), Vec::new());
    let buildables: Vec<BuildItem> = (0..n_units).map(|i| BuildItem::Unit(i as u32)).collect();
    core.enable_sidebar(1, buildables);
    (core, weap_cell)
}

#[test]
fn scroll_offscreen_unit_builds_and_auto_spawns_near_the_factory() {
    // 10 units, a viewport short enough that only 2 rows fit -> the units
    // column overflows and the target (index 6) genuinely starts off-screen.
    let (mut core, weap_cell) = units_overflow_core(0x51DE_0001, 10);
    core.handle(InputEvent::Resize {
        width: 900,
        height: 100,
    });
    let tw = core.tactical_width() as i32;
    assert_eq!(
        expected_visible_rows(100),
        2,
        "test setup: this viewport height must yield exactly 2 visible rows"
    );
    assert_eq!(core.sidebar_scroll(1), 0, "starts un-scrolled");
    assert_eq!(
        core.sidebar_scroll(0),
        0,
        "structures column (empty) starts un-scrolled too"
    );

    let ux = tw + 1 + COLUMN_W + 10;
    let y0 = NO_RADAR_ROWS_TOP + 5;

    // Sanity (read-only -- no click, so no command gets queued into `pending`
    // ahead of the real one below): before scrolling, the flat sidebar list's
    // first entry is unit 0, not our target -- proves the target genuinely
    // starts off-screen rather than happening to already be visible. The
    // structures column is empty here, so `sidebar_items()`'s flat order is
    // exactly the units column's order (see `which_column`/`column_items`'s
    // docs on the flat-index convention).
    let items = core.sidebar_items();
    assert_eq!(
        items.first().map(|i| i.name.as_str()),
        Some("U0"),
        "test setup: the first sidebar entry pre-scroll should be unit 0"
    );

    // Scroll the units column down until unit 6 (started off-screen, since
    // only rows 0-1 -- units 0-1 -- were visible) is at row 0.
    const TARGET: u32 = 6;
    for _ in 0..TARGET {
        core.handle(InputEvent::SidebarScroll {
            column: 1,
            up: false,
        });
    }
    assert_eq!(core.sidebar_scroll(1), TARGET as usize);

    // Click row 0: now unit 6.
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x: ux,
        y: y0,
    });
    let emitted = core.drain_commands();
    assert_eq!(
        emitted.len(),
        1,
        "the click should emit exactly one command"
    );
    match emitted[0] {
        Command::StartProduction { house, item } => {
            assert_eq!(house, 1);
            assert_eq!(
                item,
                BuildItem::Unit(TARGET),
                "row 0 after scrolling by {TARGET} should build the unit that scrolled into view"
            );
        }
        other => panic!("expected StartProduction, got {other:?}"),
    }

    // Drive ticks until unit 6 spawns into the world (no manual placement
    // step exists for vehicles -- see the module doc).
    let mut spawned = None;
    for _ in 0..500 {
        core.update(67);
        if let Some((h, u)) = core
            .world()
            .units
            .iter()
            .find(|(_, u)| u.house == 1 && u.type_id == TARGET)
        {
            spawned = Some((h, u.coord.cell()));
            break;
        }
    }
    let (_, spawn_cell) = spawned
        .unwrap_or_else(|| panic!("unit {TARGET} should have spawned within the tick budget"));

    // "Placed" sanely: near the war factory it exited from, not at some
    // default/error location like (0,0).
    let dx = (spawn_cell.x - weap_cell.x).abs();
    let dy = (spawn_cell.y - weap_cell.y).abs();
    assert!(
        dx <= 6 && dy <= 6,
        "the spawned unit should exit near its war factory ({weap_cell:?}), landed at \
         {spawn_cell:?} instead"
    );
}

// ---------------------------------------------------------------------
// Fixture 2: structures-only sidebar (column 1 empty), so the flow ends
// with a genuine placement click, matching the task brief's literal
// "issue a placement command at a valid map cell" step.
// ---------------------------------------------------------------------

fn structures_overflow_core(seed: u32, n_extra: usize) -> (AppCore, CellCoord) {
    let fact = BuildingProto {
        is_barracks: false,
        name: "FACT".into(),
        foot_w: 3,
        foot_h: 3,
        max_health: 400,
        armor: 0,
        power: 0,
        cost: 100,
        prereq: vec![],
        is_refinery: false,
        is_construction_yard: true,
        is_war_factory: false,
        free_harvester_unit: None,
        sight: 4,
        sprite_id: 0,
        weapon: None,
        has_turret: false,
        charges: false,
        is_wall: false,
        storage: 0,
    };
    let extra = |name: String| BuildingProto {
        is_barracks: false,
        name,
        foot_w: 2,
        foot_h: 2,
        max_health: 400,
        armor: 0,
        power: 0,
        cost: 10,
        prereq: vec![],
        is_refinery: false,
        is_construction_yard: false,
        is_war_factory: false,
        free_harvester_unit: None,
        sight: 4,
        sprite_id: 0,
        weapon: None,
        has_turret: false,
        charges: false,
        is_wall: false,
        storage: 0,
    };
    let mut buildings = vec![fact];
    buildings.extend((0..n_extra).map(|i| extra(format!("S{i}"))));

    let mut world = World::new(Passability::all_passable(), seed);
    world.set_catalog(Catalog {
        buildings,
        units: vec![],
        econ: EconRules::default(),
    });
    world.init_houses(3, 5000);

    let yard_cell = CellCoord::new(20, 20);
    world.spawn_building(0, 1, yard_cell).unwrap();

    let (raster, palette) = support::synthetic_fixture();
    let mut core = AppCore::with_sim(raster.clone(), *palette, world, Vec::new(), Vec::new());
    let buildables: Vec<BuildItem> = (0..n_extra)
        .map(|i| BuildItem::Building(i as u32 + 1)) // +1: id 0 is FACT, not buildable
        .collect();
    core.enable_sidebar(1, buildables);
    (core, yard_cell)
}

#[test]
fn scroll_offscreen_structure_builds_and_places_via_real_click() {
    // 5 extra structures, a viewport short enough for 2 visible rows -> the
    // structures column overflows; the target (index 3, id 4) starts
    // off-screen.
    let (mut core, yard_cell) = structures_overflow_core(0x51DE_0002, 5);
    core.handle(InputEvent::Resize {
        width: 900,
        height: 100,
    });
    let tw = core.tactical_width() as i32;
    assert_eq!(
        expected_visible_rows(100),
        2,
        "test setup: this viewport height must yield exactly 2 visible rows"
    );

    let cx = tw + 1 + 10; // column 0 (structures)
    let y0 = NO_RADAR_ROWS_TOP + 5;

    // Sanity (read-only, see the units test's twin comment on why a click
    // isn't used here): row 0 pre-scroll is building id 1 (S0), not our
    // target. The units column is empty, so the flat list is exactly the
    // structures column's order.
    let items = core.sidebar_items();
    assert_eq!(
        items.first().map(|i| i.item),
        Some(BuildItem::Building(1)),
        "test setup: the first sidebar entry pre-scroll should be building 1 (S0)"
    );

    // Scroll the structures column (col 0) down by 3 -> row 0 becomes index
    // 3 (id 4, "S3"), which started off-screen (only ids 1-2 were visible).
    const TARGET_ID: u32 = 4;
    for _ in 0..3 {
        core.handle(InputEvent::SidebarScroll {
            column: 0,
            up: false,
        });
    }
    assert_eq!(core.sidebar_scroll(0), 3);
    // Units column (empty) never had anywhere to scroll.
    assert_eq!(core.sidebar_scroll(1), 0);

    // Click row 0: now building id 4.
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x: cx,
        y: y0,
    });
    let emitted = core.drain_commands();
    assert_eq!(emitted.len(), 1);
    match emitted[0] {
        Command::StartProduction { house, item } => {
            assert_eq!(house, 1);
            assert_eq!(item, BuildItem::Building(TARGET_ID));
        }
        other => panic!("expected StartProduction, got {other:?}"),
    }

    // Drive ticks until it's ready to place.
    let mut ready = false;
    for _ in 0..500 {
        core.update(67);
        if core.world().house(1).and_then(|h| h.ready_building) == Some(TARGET_ID) {
            ready = true;
            break;
        }
    }
    assert!(ready, "building {TARGET_ID} should finish within budget");
    core.drain_commands();

    // Click the SAME sidebar row again: it now reports `ready`, so this real
    // click (not a `begin_placement()` API shortcut) re-enters placement
    // mode, exactly as a player clicking their finished building would.
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x: cx,
        y: y0,
    });
    assert!(
        core.drain_commands().is_empty(),
        "entering placement mode emits no command"
    );
    assert_eq!(
        core.placing(),
        Some(TARGET_ID),
        "clicking a ready row should enter placement mode for it"
    );

    // Position the camera so a cell touching the yard's east edge (valid:
    // adjacent to an owned building, footprint clear -- the same proximity
    // shape `ui_scripted_drive.rs`'s placement test already proved valid for
    // a 3x3 yard + 2x2 building) is on-screen, then click it.
    let valid_cell = CellCoord::new(yard_cell.x + 3, yard_cell.y);
    let cam = (
        (yard_cell.x * CELL_PIXELS) as f32 - 40.0,
        (yard_cell.y * CELL_PIXELS) as f32 - 40.0,
    );
    core.set_camera(cam.0, cam.1);
    let (vx, vy) = (
        valid_cell.x * CELL_PIXELS + CELL_PIXELS / 2 - cam.0 as i32,
        valid_cell.y * CELL_PIXELS + CELL_PIXELS / 2 - cam.1 as i32,
    );
    assert!(
        vx < tw,
        "test setup: the placement click must land in the tactical area, not the sidebar"
    );
    core.handle(InputEvent::MouseMoved { x: vx, y: vy });
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x: vx,
        y: vy,
    });
    let emitted = core.drain_commands();
    assert_eq!(emitted.len(), 1);
    match emitted[0] {
        Command::PlaceBuilding {
            house,
            building,
            cell,
        } => {
            assert_eq!(house, 1);
            assert_eq!(building, TARGET_ID);
            assert_eq!(cell, valid_cell);
        }
        other => panic!("expected PlaceBuilding, got {other:?}"),
    }
    assert_eq!(core.placing(), None, "placement mode clears on success");

    // Settle a couple of ticks, then confirm the building genuinely exists
    // in the world at the clicked cell -- not just that a command was
    // emitted and trusted blindly.
    for _ in 0..3 {
        core.update(67);
    }
    assert!(
        core.world().buildings.iter().any(|(_, b)| b.house == 1
            && b.type_id == TARGET_ID
            && b.cell == valid_cell
            && b.is_alive()),
        "building {TARGET_ID} should be alive in the world at {valid_cell:?} after placement"
    );
}
