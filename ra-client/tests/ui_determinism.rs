//! Client-level determinism suite (DESIGN.md §4.2, §4.8) — the `AppCore`
//! mirror of `ra-sim/tests/determinism.rs`'s sim-level suite. Same claim,
//! driven through the real UI seam instead of `World` directly: identical
//! `InputEvent` scripts on identical virtual time must produce identical
//! `sim_hash()` chains, tick for tick, all the way from box-select and
//! right-click through to the sim ticks those commands land in.
//!
//! Synthetic variant always runs; a real-scenario variant drives scg01ea's 4
//! real starting units and skips cleanly without the real assets.

mod support;

use ra_sim::coords::CellCoord;

const SEED: u32 = 0xFACE_B00C;
const SELECT_BOX: ((i32, i32), (i32, i32)) = ((0, 0), (500, 400));
const DEST: CellCoord = CellCoord { x: 1, y: 25 };

/// Same-seed-twice, synthetic: two independently-built cores driven through
/// the identical script must produce identical `sim_hash()` at every single
/// tick, not just at the end.
#[test]
fn synthetic_same_script_twice_identical_hash_chain_every_tick() {
    let (mut a, _) = support::synthetic_core_with_units(SEED);
    let (mut b, _) = support::synthetic_core_with_units(SEED);

    let (chain_a, emitted_a) = support::run_select_and_move_script(
        &mut a,
        (0.0, 0.0),
        (320, 240),
        SELECT_BOX,
        DEST,
        4,
        150,
    );
    let (chain_b, emitted_b) = support::run_select_and_move_script(
        &mut b,
        (0.0, 0.0),
        (320, 240),
        SELECT_BOX,
        DEST,
        4,
        150,
    );

    assert!(!chain_a.is_empty());
    assert_eq!(chain_a.len(), chain_b.len());
    for (t, (ha, hb)) in chain_a.iter().zip(&chain_b).enumerate() {
        assert_eq!(ha, hb, "hash chains diverge at tick index {t}");
    }
    assert_eq!(
        emitted_a, emitted_b,
        "identical script should emit identical commands"
    );
    assert_eq!(a.sim_hash(), b.sim_hash());
}

/// A second, independently-composed script (different seed, different
/// destination, a longer idle warm-up before selecting) — guards against the
/// first test's specific script shape accidentally being the only thing that
/// replays cleanly.
#[test]
fn synthetic_second_script_shape_also_replays_identically() {
    const SEED2: u32 = 0x0BAD_F00D;
    const DEST2: CellCoord = CellCoord { x: 20, y: 1 };

    let (mut a, _) = support::synthetic_core_with_units(SEED2);
    let (mut b, _) = support::synthetic_core_with_units(SEED2);

    let (chain_a, _) = support::run_select_and_move_script(
        &mut a,
        (0.0, 0.0),
        (256, 200),
        SELECT_BOX,
        DEST2,
        11,
        90,
    );
    let (chain_b, _) = support::run_select_and_move_script(
        &mut b,
        (0.0, 0.0),
        (256, 200),
        SELECT_BOX,
        DEST2,
        11,
        90,
    );

    assert_eq!(chain_a, chain_b);
}

/// A run with no input at all (just virtual-time ticks) must also be
/// hash-stable across reruns — isolates the pure sim-stepping path from the
/// selection/command machinery exercised above.
#[test]
fn synthetic_idle_ticks_only_identical_hash_chain() {
    let (mut a, _) = support::synthetic_core_with_units(0x1111_2222);
    let (mut b, _) = support::synthetic_core_with_units(0x1111_2222);
    a.handle(ra_client::input::InputEvent::Resize {
        width: 320,
        height: 240,
    });
    b.handle(ra_client::input::InputEvent::Resize {
        width: 320,
        height: 240,
    });
    let mut chain_a = Vec::new();
    let mut chain_b = Vec::new();
    for _ in 0..120 {
        a.update(support::TICK_MS);
        b.update(support::TICK_MS);
        chain_a.push(a.sim_hash());
        chain_b.push(b.sim_hash());
    }
    assert_eq!(chain_a, chain_b);
}

// ---------------------------------------------------------------------
// Real-scenario variant.
// ---------------------------------------------------------------------

#[test]
fn real_scg01ea_same_script_twice_identical_hash_chain_every_tick() {
    let (Some(game_a), Some(game_b)) = (support::load_real_game(), support::load_real_game())
    else {
        return;
    };
    let mut core_a = game_a.core;
    let mut core_b = game_b.core;
    assert_eq!(game_a.spawned.len(), 4);
    assert_eq!(game_b.spawned.len(), 4);

    // Same fixed camera/viewport/select-box/destination for both — this test
    // is about hash-chain reproducibility, not about picking a clever
    // destination (see `ui_scripted_drive.rs` for the movement-behavior
    // assertions over the real scenario).
    let cam = (63.0 * 24.0 - 400.0, 50.0 * 24.0 - 300.0);
    let dest = CellCoord::new(69, 50); // known-passable: a real JEEP's own spawn cell

    let (chain_a, emitted_a) = support::run_select_and_move_script(
        &mut core_a,
        cam,
        (800, 600),
        ((0, 0), (799, 599)),
        dest,
        3,
        120,
    );
    let (chain_b, emitted_b) = support::run_select_and_move_script(
        &mut core_b,
        cam,
        (800, 600),
        ((0, 0), (799, 599)),
        dest,
        3,
        120,
    );

    assert_eq!(chain_a.len(), chain_b.len());
    for (t, (ha, hb)) in chain_a.iter().zip(&chain_b).enumerate() {
        assert_eq!(ha, hb, "real scg01ea hash chains diverge at tick index {t}");
    }
    assert_eq!(emitted_a, emitted_b);
}
