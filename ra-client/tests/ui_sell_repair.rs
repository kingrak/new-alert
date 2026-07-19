//! M7.9 P1 — player sell / repair interface (scripted end-to-end drives through
//! the `AppCore` seam, DESIGN.md §4.8 layer 1). Every action goes through the
//! real `handle`/`update`/`drain_commands` path a player's mouse would drive.
//!
//! Covers:
//! - Click SELL → sell mode arms; click an **own** building → `Command::Sell`,
//!   the building is gone and the refund (`RefundPercent`) is credited exactly.
//! - Click REPAIR → repair mode arms; click an own damaged building →
//!   `Command::Repair`; on the repair cadence it heals and drains credits.
//! - **Monkey/scripted-drive safety**: in sell/repair mode a click on an *enemy*
//!   building, or empty ground, emits **no** command.
//! - Right-click and Esc cancel the mode.

mod support;

use ra_client::appcore::AppCore;
use ra_client::input::{InputEvent, Key, MouseButton};
use ra_sim::coords::CellCoord;
use ra_sim::{Command, Handle};

const CELL_PX: i32 = 24; // ICON_WIDTH — one cell edge in map pixels.

/// A `640×400` core with the econ catalog, sidebar enabled for house 1, camera
/// at the origin, an **own** PROC (house 1) near the origin and an **enemy** PROC
/// (house 2) beside it — both on-screen. Returns the core and the two handles.
fn core_with_buildings() -> (AppCore, Handle, Handle) {
    let (mut world, _mcv) = support::synthetic_world_with_econ(0x5E11_0001, 5000);
    // Own PROC at (2,2) (centre cell (3,3) -> pixel (84,84)), enemy PROC at
    // (10,2) (centre (11,3) -> pixel (276,84)); both inside the 640×400 view.
    let own = world
        .spawn_building(support::ECON_B_PROC, 1, CellCoord::new(2, 2))
        .expect("own PROC spawns");
    let enemy = world
        .spawn_building(support::ECON_B_PROC, 2, CellCoord::new(10, 2))
        .expect("enemy PROC spawns");
    let (raster, palette) = support::synthetic_fixture();
    let mut core = AppCore::with_sim(raster.clone(), *palette, world, Vec::new(), Vec::new());
    core.enable_sidebar(1, support::econ_buildables());
    core.handle(InputEvent::Resize {
        width: 640,
        height: 400,
    });
    core.set_camera(0.0, 0.0);
    (core, own, enemy)
}

/// Left-click at a viewport pixel.
fn click(core: &mut AppCore, x: i32, y: i32) {
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x,
        y,
    });
}

/// Move the cursor to a viewport pixel (so hover-tint / mode logic sees it).
fn move_to(core: &mut AppCore, x: i32, y: i32) {
    core.handle(InputEvent::MouseMoved { x, y });
}

/// Viewport pixel at the centre of a cell (camera at origin).
fn cell_px(cell: CellCoord) -> (i32, i32) {
    (
        cell.x * CELL_PX + CELL_PX / 2,
        cell.y * CELL_PX + CELL_PX / 2,
    )
}

/// SELL button centre (right edge of the sidebar header).
fn sell_button_px(core: &AppCore) -> (i32, i32) {
    (core.viewport_size().0 as i32 - 2 - 17, 5)
}
/// REPAIR button centre.
fn repair_button_px(core: &AppCore) -> (i32, i32) {
    (core.viewport_size().0 as i32 - 2 - 17, 15)
}

// ===========================================================================
// SELL
// ===========================================================================

#[test]
fn clicking_sell_button_arms_sell_mode() {
    let (mut core, _own, _enemy) = core_with_buildings();
    assert!(!core.sell_mode());
    let (bx, by) = sell_button_px(&core);
    click(&mut core, bx, by);
    assert!(core.sell_mode(), "SELL button should arm sell mode");
    // Clicking again toggles it off.
    click(&mut core, bx, by);
    assert!(!core.sell_mode());
}

#[test]
fn sell_own_building_refunds_exactly_and_removes_it() {
    let (mut core, own, _enemy) = core_with_buildings();
    let cost = core.world().buildings.get(own).unwrap().cost;
    let refund_pct = core.world().catalog.econ.refund_percent;
    let expected_refund = cost * refund_pct / 100;
    let credits_before = core.credits();

    // Arm sell mode, then click the own PROC.
    let (bx, by) = sell_button_px(&core);
    click(&mut core, bx, by);
    let (px, py) = cell_px(CellCoord::new(3, 3)); // inside the 3x3 footprint
    click(&mut core, px, py);

    // Exactly one Sell command for the own building was emitted.
    let cmds = core.drain_commands();
    assert_eq!(
        cmds,
        vec![Command::Sell {
            house: 1,
            building: own
        }],
        "a sell-mode click on an own building emits exactly one Sell"
    );

    // Apply it (a tick), then verify the building is gone and credits rose by
    // exactly the refund.
    core.update(support::TICK_MS);
    assert!(
        core.world().buildings.get(own).is_none(),
        "the sold building must be removed from the arena"
    );
    assert_eq!(
        core.credits(),
        credits_before + expected_refund,
        "refund must be exactly RefundPercent * cost"
    );
}

#[test]
fn sell_mode_ignores_enemy_buildings_and_empty_ground() {
    let (mut core, _own, enemy) = core_with_buildings();
    let (bx, by) = sell_button_px(&core);
    click(&mut core, bx, by);
    assert!(core.sell_mode());

    // Click the ENEMY building — must emit nothing.
    let (ex, ey) = cell_px(CellCoord::new(11, 3));
    click(&mut core, ex, ey);
    assert!(
        core.drain_commands().is_empty(),
        "selling must never target an enemy building"
    );
    assert!(
        core.world().buildings.get(enemy).is_some(),
        "enemy building must be untouched"
    );

    // Click empty ground — also nothing.
    let (gx, gy) = cell_px(CellCoord::new(30, 12));
    click(&mut core, gx, gy);
    assert!(
        core.drain_commands().is_empty(),
        "selling empty ground emits nothing"
    );
}

#[test]
fn right_click_and_esc_cancel_sell_mode() {
    let (mut core, _own, _enemy) = core_with_buildings();
    let (bx, by) = sell_button_px(&core);
    click(&mut core, bx, by);
    assert!(core.sell_mode());
    // Right-click in the tactical area cancels the mode (and emits no order).
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Right,
        x: 100,
        y: 100,
    });
    assert!(!core.sell_mode(), "right-click cancels sell mode");
    assert!(core.drain_commands().is_empty());

    // Re-arm, then Esc cancels.
    click(&mut core, bx, by);
    assert!(core.sell_mode());
    core.handle(InputEvent::KeyDown(Key::Menu));
    assert!(!core.sell_mode(), "Esc cancels sell mode");
}

// ===========================================================================
// REPAIR
// ===========================================================================

#[test]
fn repair_own_damaged_building_heals_and_drains_credits() {
    let (mut core, own, _enemy) = core_with_buildings();
    // Damage the PROC to 1/4 health.
    let max = core.world().buildings.get(own).unwrap().max_health;
    let damaged = max / 4;
    core.world_mut().buildings.get_mut(own).unwrap().health = damaged;

    // Arm repair mode, click the PROC.
    let (bx, by) = repair_button_px(&core);
    click(&mut core, bx, by);
    assert!(core.repair_mode(), "REPAIR button arms repair mode");
    let (px, py) = cell_px(CellCoord::new(3, 3));
    click(&mut core, px, py);
    assert_eq!(
        core.drain_commands(),
        vec![Command::Repair {
            house: 1,
            building: own
        }],
        "repair-mode click on own building emits exactly one Repair"
    );

    let credits_before = core.credits();
    // Tick through several repair cadences; health must climb, credits fall.
    for _ in 0..80 {
        core.update(support::TICK_MS);
    }
    let after = core.world().buildings.get(own).unwrap().health;
    assert!(
        after > damaged,
        "repair should heal the building over time ({damaged} -> {after})"
    );
    assert!(
        core.credits() < credits_before,
        "repair must drain credits while healing"
    );
}

#[test]
fn repair_mode_ignores_enemy_buildings() {
    let (mut core, _own, _enemy) = core_with_buildings();
    let (bx, by) = repair_button_px(&core);
    click(&mut core, bx, by);
    assert!(core.repair_mode());
    let (ex, ey) = cell_px(CellCoord::new(11, 3));
    click(&mut core, ex, ey);
    assert!(
        core.drain_commands().is_empty(),
        "repair must never target an enemy building"
    );
}

// ===========================================================================
// Determinism (same script twice → identical hash chain)
// ===========================================================================

#[test]
fn sell_script_is_deterministic() {
    let run = || {
        let (mut core, _own, _enemy) = core_with_buildings();
        let (bx, by) = sell_button_px(&core);
        move_to(&mut core, bx, by);
        click(&mut core, bx, by);
        let (px, py) = cell_px(CellCoord::new(3, 3));
        move_to(&mut core, px, py);
        click(&mut core, px, py);
        let mut hashes = Vec::new();
        for _ in 0..10 {
            core.update(support::TICK_MS);
            hashes.push(core.sim_hash());
        }
        hashes
    };
    assert_eq!(
        run(),
        run(),
        "same sell script twice => identical hash chain"
    );
}
