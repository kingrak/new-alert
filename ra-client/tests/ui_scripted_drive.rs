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
