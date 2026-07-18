//! Scripted end-to-end drive (DESIGN.md §4.8 layer 1) — the first real one
//! for M3's simulation features: "box-select 3 jeeps, right-click an empty
//! cell, step N ticks" through `AppCore`'s real `handle`/`update` seam,
//! asserting the whole pipeline behaves: `Move` commands are emitted, only
//! the selected (same-house) units are ordered, an unselected different-
//! house unit never moves, the selected units actually converge toward the
//! destination, and the hash chain produced is stable across independent
//! reruns of the identical script.
//!
//! Synthetic variant always runs (`support::synthetic_core_with_units`); a
//! real-scenario variant drives scg01ea's actual 3 JEEPs + 1 HARV and skips
//! cleanly without the real assets.

mod support;

use ra_sim::coords::CellCoord;
use ra_sim::Command;

const SEED: u32 = 0x5C21_D07E;

/// The synthetic fixture's three jeeps sit at cells (10,10),(12,10),(14,10)
/// (see `support::synthetic_unit_cell`); a box comfortably covering that
/// row, in viewport pixels, with the camera at the origin.
const SYNTHETIC_SELECT_BOX: ((i32, i32), (i32, i32)) = ((0, 0), (500, 400));
const SYNTHETIC_DEST: CellCoord = CellCoord { x: 4, y: 3 };

#[test]
fn synthetic_box_select_move_and_converge() {
    let (mut core, jeeps) = support::synthetic_core_with_units(SEED);
    let (hashes, emitted) = support::run_select_and_move_script(
        &mut core,
        (0.0, 0.0),
        (320, 240),
        SYNTHETIC_SELECT_BOX,
        SYNTHETIC_DEST,
        3,   // warm-up ticks before selecting
        200, // settle ticks after ordering (plenty to arrive)
    );

    // The box-select must have picked up exactly the 3 same-house jeeps
    // (not the house-2 unit at (60,60), well outside the box).
    assert_eq!(
        core.selected_handles().len(),
        3,
        "box-select should pick exactly the 3 jeeps"
    );

    // Every selected unit gets a well-formed Move: valid handle, ownership
    // (house) matches the unit it addresses, correct destination.
    assert_eq!(
        emitted.len(),
        3,
        "right-click with 3 selected units should emit 3 Move commands"
    );
    let mut moved_handles = std::collections::BTreeSet::new();
    for cmd in &emitted {
        match *cmd {
            Command::Move { unit, dest, house } => {
                assert_eq!(dest, SYNTHETIC_DEST);
                let owner = core
                    .world()
                    .units
                    .get(unit)
                    .expect("emitted command should reference a live unit")
                    .house;
                assert_eq!(
                    house, owner,
                    "command house must match the unit's real owner"
                );
                assert_eq!(house, 1, "only house-1 jeeps should have been selected");
                moved_handles.insert(unit.index);
            }
            other => panic!("expected only Move commands, got {other:?}"),
        }
    }
    assert_eq!(
        moved_handles,
        jeeps.iter().map(|h| h.index).collect(),
        "emitted commands should address exactly the 3 spawned jeeps"
    );

    // Convergence: every jeep ended up at (or adjacent to, since 3 units
    // can't all occupy one cell center exactly if paths interleave — but on
    // an open map with plenty of settle ticks they should all reach the
    // destination cell exactly) the destination.
    for h in &jeeps {
        let u = core.world().units.get(*h).unwrap();
        assert!(!u.is_moving(), "jeep should have finished its path by now");
        assert_eq!(
            u.cell(),
            SYNTHETIC_DEST,
            "jeep did not converge to the destination"
        );
    }

    // The house-2 unit must never have been touched: still at its spawn
    // cell, still idle.
    let untouched = core
        .world()
        .units
        .handles()
        .into_iter()
        .find(|h| !jeeps.contains(h))
        .expect("house-2 witness unit should still exist");
    let u = core.world().units.get(untouched).unwrap();
    assert_eq!(u.house, 2);
    assert_eq!(
        u.cell(),
        CellCoord::new(60, 60),
        "unselected unit must not have moved"
    );
    assert!(!u.is_moving());

    // Hash-chain stability: rerunning the identical script from a fresh core
    // reproduces the identical per-tick hash chain.
    let (mut core2, _) = support::synthetic_core_with_units(SEED);
    let (hashes2, _) = support::run_select_and_move_script(
        &mut core2,
        (0.0, 0.0),
        (320, 240),
        SYNTHETIC_SELECT_BOX,
        SYNTHETIC_DEST,
        3,
        200,
    );
    assert_eq!(
        hashes, hashes2,
        "identical script reruns must give identical hash chains"
    );
}

#[test]
fn synthetic_click_with_nothing_selected_emits_nothing() {
    // A right-click with no prior selection must be a pure no-op: no
    // commands, no state change (regression guard for the "selected.is_empty
    // -> return early" path in AppCore::issue_move now that units exist to
    // select in the first place).
    let (mut core, jeeps) = support::synthetic_core_with_units(SEED);
    core.handle(ra_client::input::InputEvent::Resize {
        width: 320,
        height: 240,
    });
    core.handle(ra_client::input::InputEvent::MouseDown {
        button: ra_client::input::MouseButton::Right,
        x: 50,
        y: 50,
    });
    assert!(core.drain_commands().is_empty());
    for h in &jeeps {
        assert!(!core.world().units.get(*h).unwrap().is_moving());
    }
}

// ---------------------------------------------------------------------
// Battle script (M4, new): select tank(s) -> attack enemy -> step to kill ->
// assert health-bar rendering changes and the post-death frame no longer
// draws the target. Complements the M3 move-only script above; uses
// `support::synthetic_core_with_armed_units` (armed jeeps + a nearby
// unarmed house-2 target — see that helper's docs on why the M3 unarmed
// fixture can never generate an `Attack` at all).
// ---------------------------------------------------------------------

/// Pixels per cell edge, matching `ra_client::appcore`'s private
/// `CELL_PIXELS` (== `ICON_WIDTH`) — duplicated here for the same reason
/// `support::run_select_and_move_script` already duplicates it (no public
/// accessor, and it's a stable, documented constant).
const CELL_PIXELS: i32 = 24;

/// The exact backing-rectangle pixel `draw_health_bar` always paints black
/// (`fill_rect(dst, x0-1, y0-1, x1+1, y1+1, [0,0,0])`, unconditionally,
/// regardless of the health fraction) for a unit centred at `(sx, sy)` —
/// `(sx - CELL_PIXELS/2 - 1, sy - CELL_PIXELS/2 - 5)` inclusive is inside
/// that rect for every case (`width.max(4)` keeps `x0` no further right than
/// `sx - 2`). Used as an unambiguous "was a health bar drawn for the unit
/// centred here, this frame" probe, independent of whatever sprite/terrain
/// pixels would otherwise be there.
fn health_bar_backing_pixel(frame: &ra_client::compositor::RgbaImage, sx: i32, sy: i32) -> [u8; 4] {
    let x = sx - CELL_PIXELS / 2 - 1;
    let y = sy - CELL_PIXELS / 2 - 5;
    let idx = ((y as u32 * frame.width + x as u32) * 4) as usize;
    [
        frame.pixels[idx],
        frame.pixels[idx + 1],
        frame.pixels[idx + 2],
        frame.pixels[idx + 3],
    ]
}

#[test]
fn synthetic_battle_attack_kill_and_health_bar_rendering() {
    let seed = 0xBA77_1E00;
    let (mut core, jeeps, target) = support::synthetic_core_with_armed_units(seed);
    // Tall enough to actually include row-10 units (pixel y=252): a 320x240
    // viewport at the origin would clip them below the visible area.
    core.handle(ra_client::input::InputEvent::Resize {
        width: 480,
        height: 320,
    });
    core.set_camera(0.0, 0.0);
    assert_eq!(jeeps.len(), 3);

    // Target sits at cell (16,10) (see `support::synthetic_world_with_armed_units`);
    // screen position with the camera at the origin.
    let target_sx = 16 * CELL_PIXELS + CELL_PIXELS / 2;
    let target_sy = 10 * CELL_PIXELS + CELL_PIXELS / 2;

    // Before any order: unselected, undamaged — no health bar for the
    // target, so its backing pixel must not be the bar's black.
    let frame_before = core.compose(core.camera_rect());
    let pixel_before = health_bar_backing_pixel(&frame_before, target_sx, target_sy);
    assert_ne!(
        pixel_before,
        [0, 0, 0, 255],
        "target's health-bar pixel is already black before any damage — test fixture assumption \
         broken (terrain there happens to be black), pick a different probe pixel"
    );

    // Box-select the 3 jeeps only (their cells (10,10)-(14,10) map to pixels
    // ~(240,240)-(348,264); a box out to x=370 stays short of the target at
    // x=396, matching `support`'s armed-fixture layout).
    core.handle(ra_client::input::InputEvent::MouseDown {
        button: ra_client::input::MouseButton::Left,
        x: 0,
        y: 0,
    });
    core.handle(ra_client::input::InputEvent::MouseMoved { x: 370, y: 280 });
    core.handle(ra_client::input::InputEvent::MouseUp {
        button: ra_client::input::MouseButton::Left,
        x: 370,
        y: 280,
    });
    assert_eq!(
        core.selected_handles().len(),
        3,
        "box-select should pick up exactly the 3 armed jeeps, not the target"
    );

    // Right-click the target: an Attack order for every armed selected unit.
    core.handle(ra_client::input::InputEvent::MouseDown {
        button: ra_client::input::MouseButton::Right,
        x: target_sx,
        y: target_sy,
    });
    let emitted = core.drain_commands();
    assert_eq!(emitted.len(), 3, "all 3 armed jeeps should attack");
    for cmd in &emitted {
        match *cmd {
            Command::Attack {
                unit,
                target: t,
                house,
            } => {
                assert_eq!(house, 1);
                assert!(jeeps.contains(&unit));
                assert_eq!(t, ra_sim::Target::Unit(target));
            }
            other => panic!("expected only Attack commands, got {other:?}"),
        }
    }

    // Step until the target has taken damage (still alive).
    let mut damaged = false;
    for _ in 0..60 {
        core.update(67);
        if core
            .world()
            .units
            .get(target)
            .map(|u| u.health)
            .unwrap_or(0)
            < 150
        {
            damaged = true;
            break;
        }
    }
    assert!(damaged, "target never took damage");
    let frame_damaged = core.compose(core.camera_rect());

    let pixel_damaged = health_bar_backing_pixel(&frame_damaged, target_sx, target_sy);
    assert_eq!(
        pixel_damaged,
        [0, 0, 0, 255],
        "a damaged, unselected unit should now be drawing a health bar (black backing pixel)"
    );

    // Step until the target dies (arena removal).
    let mut died = false;
    for _ in 0..500 {
        core.update(67);
        if !core.world().units.contains(target) {
            died = true;
            break;
        }
    }
    assert!(died, "target should have been destroyed");
    // A few more idle-ish updates so any transient muzzle-flash frame from
    // the instant the killing shot landed has passed (the jeeps' TarCom
    // clears the tick after the target dies — `run_combat`'s stale-target
    // handling — so this is just settling, not masking a real signal).
    for _ in 0..5 {
        core.update(67);
    }
    let frame_after_death = core.compose(core.camera_rect());
    let pixel_after_death = health_bar_backing_pixel(&frame_after_death, target_sx, target_sy);
    assert_ne!(
        pixel_after_death,
        [0, 0, 0, 255],
        "post-death frame must no longer draw the target's health bar"
    );
    assert_eq!(
        pixel_after_death, pixel_before,
        "post-death, the target's former position should look exactly like it did before combat \
         (nothing left to draw there — the unit is gone from the arena)"
    );
}

// ---------------------------------------------------------------------
// Real-scenario variant: scg01ea's actual starting units.
// ---------------------------------------------------------------------

#[test]
fn real_scg01ea_box_select_move_and_converge() {
    let Some(game) = support::load_real_game() else {
        return;
    };
    let mut core = game.core;
    let spawned = game.spawned;
    assert_eq!(
        spawned.len(),
        4,
        "scg01ea should spawn its 4 real starting units"
    );

    // Camera centred on the units (mirrors `ra-client`'s own `sim`
    // subcommand), viewport large enough to cover all 4 real spawn cells in
    // one box-select.
    let (mut sx, mut sy) = (0i64, 0i64);
    for s in &spawned {
        sx += s.cell.x as i64;
        sy += s.cell.y as i64;
    }
    let n = spawned.len() as i64;
    let (cx_cell, cy_cell) = (sx / n, sy / n);
    let (vw, vh) = (800u32, 600u32);
    let cam = (
        (cx_cell * 24) as f32 - vw as f32 / 2.0,
        (cy_cell * 24) as f32 - vh as f32 / 2.0,
    );

    let dest = pick_real_destination(&core, CellCoord::new(cx_cell as i32, cy_cell as i32));
    let (_hashes, emitted) = support::run_select_and_move_script(
        &mut core,
        cam,
        (vw, vh),
        ((0, 0), (vw as i32 - 1, vh as i32 - 1)),
        dest,
        2,
        160,
    );

    assert_eq!(
        core.selected_handles().len(),
        4,
        "box-select should pick up all 4 real units"
    );
    assert_eq!(
        emitted.len(),
        4,
        "issuing a move to 4 selected units should emit 4 Move commands"
    );
    for cmd in &emitted {
        let Command::Move {
            unit,
            dest: d,
            house,
        } = *cmd
        else {
            panic!("expected only Move commands");
        };
        assert_eq!(d, dest);
        let owner = core.world().units.get(unit).unwrap().house;
        assert_eq!(house, owner);
    }

    let mut moved = 0;
    for s in &spawned {
        let u = core.world().units.get(s.handle).unwrap();
        if u.cell() != s.cell {
            moved += 1;
        }
    }
    assert!(
        moved > 0,
        "at least some real units should have moved toward the destination"
    );
}

/// Same passable-cell scan `ra-client`'s own `sim` subcommand uses, kept
/// independent here (test-only) rather than imported, matching this repo's
/// existing pattern of small test-only utilities not sharing code with the
/// production binary they're testing.
fn pick_real_destination(core: &ra_client::appcore::AppCore, anchor: CellCoord) -> CellCoord {
    let grid = core.world().passability();
    let candidates = [
        (6, 0),
        (-6, 0),
        (0, 6),
        (0, -6),
        (4, 4),
        (-4, 4),
        (4, -4),
        (-4, -4),
        (3, 0),
        (0, 3),
    ];
    for (dx, dy) in candidates {
        let c = CellCoord::new(anchor.x + dx, anchor.y + dy);
        if grid.is_passable(c) {
            return c;
        }
    }
    anchor
}

// ---------------------------------------------------------------------
// Real-asset battle variant: a real 2TNK vs a real HARV, real rules.ini
// weapon/armor stats, driven through the same click path. Skips cleanly
// without assets.
// ---------------------------------------------------------------------

#[test]
fn real_battle_attack_kill_and_health_bar_rendering() {
    if !support::real_assets_available() {
        eprintln!(
            "SKIP: real assets not found under {} (set RA_ASSETS_DIR or copy \
             main.mix/redalert.mix into assets/ to run this test)",
            support::assets_dir().display()
        );
        return;
    }
    let setup = match ra_client::assets::load_battle_from_dir(&support::assets_dir(), "scg01ea.ini")
    {
        Ok(s) => s,
        Err(e) => panic!("real battle setup should load from present assets: {e}"),
    };
    let ra_client::assets::BattleSetup {
        mut core,
        attacker,
        attacker_cell,
        target,
        target_cell,
        weapon,
        target_max_hp,
        ..
    } = setup;
    assert!(weapon.warhead_ap, "2TNK's 90mm should be AP, per rules.ini");

    // Camera framing both combatants with margin above for the health bar.
    let cam_x = ((attacker_cell.x - 3) * CELL_PIXELS) as f32;
    let cam_y = ((attacker_cell.y - 3) * CELL_PIXELS) as f32;
    core.handle(ra_client::input::InputEvent::Resize {
        width: 320,
        height: 240,
    });
    core.set_camera(cam_x, cam_y);

    let target_sx = (target_cell.x * CELL_PIXELS + CELL_PIXELS / 2) as i64 - cam_x as i64;
    let target_sy = (target_cell.y * CELL_PIXELS + CELL_PIXELS / 2) as i64 - cam_y as i64;

    let frame_before = core.compose(core.camera_rect());
    let pixel_before = health_bar_backing_pixel(&frame_before, target_sx as i32, target_sy as i32);
    assert_ne!(
        pixel_before,
        [0, 0, 0, 255],
        "target's health-bar pixel is already black before any damage"
    );

    // Click-select the attacker directly (a single unit, not a box).
    let attacker_sx = (attacker_cell.x * CELL_PIXELS + CELL_PIXELS / 2) as i64 - cam_x as i64;
    let attacker_sy = (attacker_cell.y * CELL_PIXELS + CELL_PIXELS / 2) as i64 - cam_y as i64;
    core.handle(ra_client::input::InputEvent::MouseDown {
        button: ra_client::input::MouseButton::Left,
        x: attacker_sx as i32,
        y: attacker_sy as i32,
    });
    core.handle(ra_client::input::InputEvent::MouseUp {
        button: ra_client::input::MouseButton::Left,
        x: attacker_sx as i32,
        y: attacker_sy as i32,
    });
    assert_eq!(core.selected_handles(), vec![attacker]);

    core.handle(ra_client::input::InputEvent::MouseDown {
        button: ra_client::input::MouseButton::Right,
        x: target_sx as i32,
        y: target_sy as i32,
    });
    let emitted = core.drain_commands();
    assert_eq!(emitted.len(), 1);
    match emitted[0] {
        Command::Attack {
            unit,
            target: t,
            house,
        } => {
            assert_eq!(unit, attacker);
            assert_eq!(t, ra_sim::Target::Unit(target));
            assert_eq!(house, core.world().units.get(attacker).unwrap().house);
        }
        other => panic!("expected an Attack command, got {other:?}"),
    }

    // Bound the wait generously: shots-to-kill * ROF * a safety factor, plus
    // approach time (attacker/target are already 3 cells apart, inside
    // 90mm's 4.75-cell range, so no approach is actually needed, but the
    // margin costs nothing).
    let shots_to_kill = (target_max_hp as i64 / weapon.damage.max(1) as i64) + 2;
    let bound_ticks = (shots_to_kill * weapon.rof as i64 * 2 + 200) as u32;

    let mut damaged = false;
    let mut hp_at_damage = target_max_hp;
    for _ in 0..bound_ticks {
        core.update(67);
        if let Some(u) = core.world().units.get(target) {
            if u.health < target_max_hp {
                damaged = true;
                hp_at_damage = u.health;
                break;
            }
        } else {
            break; // died before we ever observed a partial-damage frame
        }
    }
    if damaged {
        let frame_damaged = core.compose(core.camera_rect());
        let pixel_damaged =
            health_bar_backing_pixel(&frame_damaged, target_sx as i32, target_sy as i32);
        assert_eq!(
            pixel_damaged,
            [0, 0, 0, 255],
            "a damaged, unselected real unit should now be drawing a health bar \
             (health {hp_at_damage}/{target_max_hp})"
        );
    }

    let mut died = false;
    for _ in 0..bound_ticks {
        core.update(67);
        if !core.world().units.contains(target) {
            died = true;
            break;
        }
    }
    assert!(
        died,
        "real target should have been destroyed within {bound_ticks} ticks"
    );
    for _ in 0..5 {
        core.update(67);
    }
    let frame_after_death = core.compose(core.camera_rect());
    let pixel_after_death =
        health_bar_backing_pixel(&frame_after_death, target_sx as i32, target_sy as i32);
    assert_ne!(
        pixel_after_death,
        [0, 0, 0, 255],
        "post-death frame must no longer draw the real target's health bar"
    );
}
