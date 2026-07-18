//! Monkey UI tests (DESIGN.md §4.8 layer 3): seeded, proptest-driven random
//! `InputEvent`/`update()` interleavings. `AppCore` must never panic and
//! `drain_commands()` must never yield anything invalid.
//!
//! **Terrain-only variants** (`synthetic_monkey_never_panics`,
//! `real_scg01ea_monkey_never_panics`): no units exist in these fixtures, so
//! `drain_commands()` must stay empty after every single op — that was the
//! whole story at M2 when `Command` was uninhabited, and remains true now
//! (M3) simply because there is nothing to select.
//!
//! **Units variants** (`synthetic_monkey_with_units_never_panics`,
//! `real_scg01ea_monkey_with_units_never_panics`, new at M3): fixtures that
//! *do* have live, ownable units, so `MouseDown`/`MouseUp` (added to the
//! event strategy below — absent at M2, when there was nothing to click)
//! actually exercise box/click selection and move-order issuing under fuzzed
//! sequencing. The invariant these check is not "commands are always empty"
//! but "every drained command is well-formed": it addresses a still-live
//! unit, and its `house` field matches that unit's real owner — i.e.
//! selection + ownership scoping never let a bogus or cross-house order
//! through, regardless of how box-select/click/drag events are interleaved.
//!
//! Two asset variants of each: synthetic (always runs, rebuilds a fresh
//! cheap core per case — proptest's normal shrinking-friendly pattern) and
//! real `scg01ea` (skips cleanly when assets are absent; loads the scenario
//! once and reuses it across cases via a `RefCell`, since re-parsing ~480MB
//! of MIX archives thousands of times would dominate runtime for no extra
//! coverage — the property under test is `AppCore`'s event handling, not the
//! asset loader, which already has its own golden tests).

mod support;

use std::cell::RefCell;

use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestRunnerConfig, TestRunner};

use ra_client::appcore::AppCore;
use ra_client::input::{InputEvent, Key, MouseButton};
use ra_sim::Command;

/// One fuzzed operation: either an `InputEvent` or a virtual-time tick.
/// `AppCore::update` isn't itself part of `InputEvent`, so the monkey
/// sequence models it as a sibling op, matching how the real shell
/// interleaves `handle` calls with per-frame `update` calls.
#[derive(Debug, Clone, Copy)]
enum MonkeyOp {
    Event(InputEvent),
    Update(u32),
}

fn key_strategy() -> impl Strategy<Value = Key> {
    prop_oneof![
        Just(Key::Left),
        Just(Key::Right),
        Just(Key::Up),
        Just(Key::Down),
    ]
}

fn mouse_button_strategy() -> impl Strategy<Value = MouseButton> {
    prop_oneof![Just(MouseButton::Left), Just(MouseButton::Right)]
}

fn event_strategy() -> impl Strategy<Value = InputEvent> {
    prop_oneof![
        3 => key_strategy().prop_map(InputEvent::KeyDown),
        3 => key_strategy().prop_map(InputEvent::KeyUp),
        // Wide enough to cover negative and far-outside-viewport coordinates
        // (both should be handled as "not near an edge", never panic).
        4 => (-1000i32..=5000, -1000i32..=5000)
            .prop_map(|(x, y)| InputEvent::MouseMoved { x, y }),
        1 => Just(InputEvent::MouseLeft),
        // MouseDown/MouseUp, both buttons, deliberately unpaired/arbitrarily
        // ordered by the surrounding op sequence (a bare MouseUp with no
        // prior MouseDown, two MouseDowns in a row, a button released other
        // than the one pressed, ...) — selection/order issuing must survive
        // every ordering, not just the well-formed drag sequences the
        // scripted-drive suite uses.
        3 => (mouse_button_strategy(), -200i32..=1200, -200i32..=1200)
            .prop_map(|(button, x, y)| InputEvent::MouseDown { button, x, y }),
        3 => (mouse_button_strategy(), -200i32..=1200, -200i32..=1200)
            .prop_map(|(button, x, y)| InputEvent::MouseUp { button, x, y }),
        // Deliberately bounded small (see module docs on the Resize op):
        // this suite fuzzes event *sequencing*, not allocation size: an
        // unbounded Resize would make compose() cost dominate runtime
        // without adding sequencing coverage. Unbounded-viewport allocation
        // safety is tracked separately (see final report: structural
        // finding for ra-coder, not exercised destructively here).
        2 => (1u32..=200, 1u32..=200)
            .prop_map(|(width, height)| InputEvent::Resize { width, height }),
    ]
}

fn op_strategy() -> impl Strategy<Value = MonkeyOp> {
    prop_oneof![
        5 => event_strategy().prop_map(MonkeyOp::Event),
        2 => (0u32..=20_000).prop_map(MonkeyOp::Update),
    ]
}

fn ops_strategy(len: std::ops::Range<usize>) -> impl Strategy<Value = Vec<MonkeyOp>> {
    proptest::collection::vec(op_strategy(), len)
}

/// How often (in ops) to also call `compose_camera()` during a sequence.
/// `drain_commands()` is checked after *every* op (it's O(1) — `Command` is
/// uninhabited, so draining is just "return an empty Vec"), but `compose()`
/// is O(viewport area); composing after every single op in a
/// thousands-of-events run would make runtime dominated by pixel-fill work
/// rather than event-sequencing coverage. Sampling still guarantees compose
/// is exercised at many distinct points in every sequence (a case with 200
/// ops still composes ~25 times) without that blowup.
const COMPOSE_EVERY: usize = 8;

/// Apply one op to `core` and check the invariants that must hold after
/// every op: no panic (implicit — a panic aborts the test) and an empty
/// command drain. `compose_camera()` is additionally exercised every
/// [`COMPOSE_EVERY`]th op (see its docs).
fn apply_op(core: &mut AppCore, op: MonkeyOp, index: usize) {
    match op {
        MonkeyOp::Event(ev) => core.handle(ev),
        MonkeyOp::Update(dt) => core.update(dt),
    }
    let cmds = core.drain_commands();
    assert!(
        cmds.is_empty(),
        "drain_commands() yielded {} command(s), but Command is uninhabited at M2",
        cmds.len()
    );
    if index.is_multiple_of(COMPOSE_EVERY) {
        let frame = core.compose_camera();
        let (vw, vh) = core.viewport_size();
        assert_eq!(frame.width, vw);
        assert_eq!(frame.height, vh);
    }
}

/// Apply a whole op sequence, starting from a small viewport so the default
/// compose cost (before any fuzzed `Resize` op changes it) stays cheap —
/// `Resize` itself is already bounded (see `event_strategy`), this just
/// avoids paying for the *unfuzzed* default 640x400 viewport on every one of
/// thousands of sampled composes.
fn apply_ops(core: &mut AppCore, ops: &[MonkeyOp]) {
    core.handle(InputEvent::Resize {
        width: 64,
        height: 48,
    });
    for (i, &op) in ops.iter().enumerate() {
        apply_op(core, op, i);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Synthetic variant: always runs. Each case gets a fresh `AppCore` over
    /// a *cloned* pre-rasterized map (rasterizing involves a per-cell
    /// hashmap lookup over all 16384 cells — cheap once, but real overhead
    /// at proptest's case-count multiplier; cloning the resulting raster
    /// buffer is a plain memcpy) and replays 200-800 ops against it —
    /// proptest's normal shrinking-friendly pattern, so a failure here gets
    /// a minimal repro for free, persisted to
    /// `ui_monkey.proptest-regressions` on failure. 64 cases * up to 800 ops
    /// is tens of thousands of events overall.
    #[test]
    fn synthetic_monkey_never_panics(ops in ops_strategy(200..800)) {
        let (raster, palette) = support::synthetic_fixture();
        let mut core = AppCore::new(raster.clone(), *palette);
        apply_ops(&mut core, &ops);
    }
}

#[test]
fn real_scg01ea_monkey_never_panics() {
    let Some(core) = support::load_real_core() else {
        return;
    };
    let core = RefCell::new(core);

    let mut runner = TestRunner::new(ProptestRunnerConfig {
        cases: 20,
        ..ProptestRunnerConfig::default()
    });
    let strategy = ops_strategy(50..300);
    let result = runner.run(&strategy, |ops| {
        let mut core = core.borrow_mut();
        apply_ops(&mut core, &ops);
        Ok(())
    });
    result.expect("real-asset monkey sequence should never panic or yield a command");
}

// ---------------------------------------------------------------------
// Units variants (new at M3): fixtures with live, ownable units, so
// MouseDown/MouseUp actually drive selection + move-order issuing. The
// invariant is "every drained command is well-formed", not "always empty".
// ---------------------------------------------------------------------

/// Apply one op to `core` and check the M3 invariants: no panic (implicit),
/// and every command drained after this op addresses a still-live unit whose
/// `house` field matches that unit's real owner. `compose_camera()` is
/// sampled the same way [`apply_op`] does.
fn apply_op_with_units(core: &mut AppCore, op: MonkeyOp, index: usize) {
    match op {
        MonkeyOp::Event(ev) => core.handle(ev),
        MonkeyOp::Update(dt) => core.update(dt),
    }
    for cmd in core.drain_commands() {
        let (unit, house) = match cmd {
            Command::Move { unit, house, .. } => (unit, house),
            Command::Stop { unit, house } => (unit, house),
        };
        let owner = core.world().units.get(unit).unwrap_or_else(|| {
            panic!("drained {cmd:?} addresses a handle that isn't live in the world")
        });
        assert_eq!(
            house, owner.house,
            "drained {cmd:?} has house {house}, but the unit it addresses is owned by house {}",
            owner.house
        );
    }
    if index.is_multiple_of(COMPOSE_EVERY) {
        let frame = core.compose_camera();
        let (vw, vh) = core.viewport_size();
        assert_eq!(frame.width, vw);
        assert_eq!(frame.height, vh);
    }
}

fn apply_ops_with_units(core: &mut AppCore, ops: &[MonkeyOp]) {
    core.handle(InputEvent::Resize {
        width: 64,
        height: 48,
    });
    for (i, &op) in ops.iter().enumerate() {
        apply_op_with_units(core, op, i);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Synthetic units variant: a fresh `AppCore` with the 3-jeep-plus-one-
    /// house-2-unit fixture (`support::synthetic_core_with_units`) per case,
    /// fuzzed the same way as the terrain-only variant above but checked
    /// against the well-formedness invariant instead of emptiness.
    #[test]
    fn synthetic_monkey_with_units_never_panics(ops in ops_strategy(200..800)) {
        let (mut core, _jeeps) = support::synthetic_core_with_units(0x0FF1_CE42);
        apply_ops_with_units(&mut core, &ops);
    }
}

#[test]
fn real_scg01ea_monkey_with_units_never_panics() {
    let Some(game) = support::load_real_game() else {
        return;
    };
    let core = RefCell::new(game.core);

    let mut runner = TestRunner::new(ProptestRunnerConfig {
        cases: 20,
        ..ProptestRunnerConfig::default()
    });
    let strategy = ops_strategy(50..300);
    let result = runner.run(&strategy, |ops| {
        let mut core = core.borrow_mut();
        apply_ops_with_units(&mut core, &ops);
        Ok(())
    });
    result
        .expect("real-asset units monkey sequence should never panic or yield a malformed command");
}
