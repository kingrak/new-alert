//! M7 item 2's last bullet ("force-fire at ground area-denial end-to-end
//! through AppCore") — and a **structural finding** instead of the requested
//! test, reported here rather than worked around.
//!
//! `ra-sim`'s `Command::Attack { target: Target::Cell(_), .. }` is real
//! force-fire: as of M7, `explosion_damage` gives it genuine ground-blast
//! splash damage (`splash_suite.rs` covers the sim side end-to-end via
//! `World::tick`). But **`AppCore` has no way to emit that command.**
//! `AppCore::issue_order` (`appcore.rs`, right-click handling) only ever
//! produces two outcomes for a right-click with units selected:
//! - a live enemy unit/building under the cursor -> `Command::Attack { target:
//!   Target::Unit(_) | Target::Building(_), .. }`
//! - anything else (including empty passable ground) -> `Command::Move`
//!
//! There is no modifier key, no separate "force-fire" input mode, and no
//! `Key`/`InputEvent` variant that could route a click to `Target::Cell` — the
//! `Key` enum (`input.rs`) only has `Left/Right/Up/Down/Deploy/Help`, and
//! `MouseButton` only `Left/Right`. So a real player, and any UI-level test,
//! can select an armed unit and right-click open ground a thousand times and
//! will only ever get `Move` orders. Ground-targeted splash — a real M7
//! gameplay feature now (area denial, pre-firing a chokepoint, clearing a
//! blob of infantry that scattered off a direct target) — is **sim-reachable
//! but UI-unreachable**, which is exactly the review-blocking defect class
//! DESIGN.md §4.8 calls out ("if a behavior can't be reached through
//! handle/update, it's a review-blocking defect").
//!
//! `right_click_on_empty_ground_never_emits_a_force_fire_attack` below pins
//! the current (gap) behavior as a regression marker: it should start
//! **failing** the moment ra-coder adds a real force-fire affordance (a
//! modifier key, a mode toggle, whatever), at which point this file's charter
//! is satisfied by deleting the pin and writing the real scripted-drive test
//! the M7 task asked for.
//!
//! `compose_game_renders_a_post_ground_splash_world_without_panicking` is the
//! closest honest substitute for "through AppCore": it runs a real
//! `Command::Attack { target: Target::Cell(_) }` directly against a `World`
//! (proving the *sim* side works, already covered by `splash_suite.rs`), then
//! hands the **already-mutated, post-splash** `World` to `AppCore::with_sim`
//! and drives `compose_game()` over it — confirming the client can at least
//! *render* the aftermath of ground splash without panicking, since it cannot
//! be asked to *cause* it.

mod support;

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    Command, MoveStats, Passability, Target, WarheadProfile, WeaponProfile, World, ARMOR_COUNT,
};

#[test]
fn right_click_on_empty_ground_never_emits_a_force_fire_attack() {
    let (mut core, jeeps, _target) = support::synthetic_core_with_armed_units(0xF0F0_ACE0);
    core.handle(ra_client::input::InputEvent::Resize {
        width: 480,
        height: 320,
    });
    core.set_camera(0.0, 0.0);

    // Select the jeeps, then right-click a patch of open ground with nothing
    // on it at all (well away from the target/other units).
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
    assert_eq!(core.selected_handles().len(), 3, "sanity: 3 jeeps selected");

    // Cell (18,5): empty ground, well away from the jeeps/target (all at row
    // 10), still inside the 480x320 viewport (CELL_PIXELS=24).
    let empty_x = 18 * 24 + 12;
    let empty_y = 5 * 24 + 12;
    core.handle(ra_client::input::InputEvent::MouseDown {
        button: ra_client::input::MouseButton::Right,
        x: empty_x,
        y: empty_y,
    });
    let emitted = core.drain_commands();
    assert_eq!(emitted.len(), 3, "one order per selected jeep");
    for cmd in &emitted {
        match *cmd {
            Command::Move { unit, .. } => {
                assert!(jeeps.contains(&unit));
            }
            other => panic!(
                "expected only Move (the UI has no force-fire affordance), got {other:?} -- \
                 if this now fails because ra-coder added ground force-fire to the UI, that's \
                 the fix landing: delete this pin and write the real scripted force-fire test"
            ),
        }
    }
}

fn splash_test_weapon() -> WeaponProfile {
    let mut verses = [0i32; ARMOR_COUNT];
    for v in verses.iter_mut() {
        *v = 65536; // 100%
    }
    WeaponProfile {
        damage: 50,
        rof: 60_000,
        range: 3000,
        proj_speed: 999,
        proj_rot: 0,
        invisible: true,
        instant: true,
        warhead: WarheadProfile { spread: 3, verses },
        warhead_ap: false, // non-AP: deterministic impact, no scatter RNG
        arcing: false,
        ballistic_scatter: 256,
        homing_scatter: 512,
        min_damage: 1,
        max_damage: 1000,
    }
}

#[test]
fn compose_game_renders_a_post_ground_splash_world_without_panicking() {
    // Build a plain World (not through AppCore -- there's no UI path to drive
    // this), force-fire at an empty cell with a bystander nearby, and confirm
    // the blast actually landed (sim-side correctness is `splash_suite.rs`'s
    // job; this is a sanity check that the scenario is real).
    let mut world = World::new(Passability::all_passable(), 0x5A5A_5A5A);
    let atk = world.spawn_unit(
        0,
        1,
        CellCoord::new(10, 10),
        Facing(64),
        400,
        MoveStats {
            max_speed: 40,
            rot: 10,
        },
    );
    world.set_unit_combat(atk, 0, Some(splash_test_weapon()), true);
    let bystander = world.spawn_unit(
        0,
        2,
        CellCoord::new(13, 10),
        Facing(0),
        400,
        MoveStats {
            max_speed: 40,
            rot: 10,
        },
    );
    let before = world.units.get(bystander).unwrap().health;
    world.tick(&[Command::Attack {
        unit: atk,
        target: Target::Cell(CellCoord::new(12, 10)),
        house: 1,
    }]);
    assert!(
        world.units.get(bystander).unwrap().health < before,
        "sanity: the ground force-fire should have splashed the nearby bystander"
    );

    // Hand the already-mutated World to a fresh AppCore and render it.
    let (raster, palette) = support::synthetic_fixture();
    let mut core =
        ra_client::AppCore::with_sim(raster.clone(), *palette, world, Vec::new(), Vec::new());
    core.handle(ra_client::input::InputEvent::Resize {
        width: 480,
        height: 320,
    });
    core.set_camera(0.0, 0.0);
    let _frame1 = core.compose_game();
    core.update(67);
    let _frame2 = core.compose_game();
    // No panic is the whole assertion here; also sanity-check the frame is
    // the requested size.
    assert_eq!(_frame2.width, 480);
    assert_eq!(_frame2.height, 320);
}
