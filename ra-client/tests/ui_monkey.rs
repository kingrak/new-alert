//! Monkey UI tests (DESIGN.md §4.8 layer 3): seeded, proptest-driven random
//! `InputEvent`/`update()` interleavings. `AppCore` must never panic and
//! `drain_commands()` must never yield anything invalid — currently
//! `Command` is uninhabited (no sim exists yet at M2), so the only possible
//! valid drain is empty; this suite asserts that stays true after every
//! single op, not just at the end of a run.
//!
//! Two variants: synthetic (always runs, rebuilds a fresh cheap synthetic
//! core per case — proptest's normal shrinking-friendly pattern) and real
//! `scg01ea` (skips cleanly when assets are absent; loads the scenario once
//! and reuses it across cases via a `RefCell`, since re-parsing ~480MB of
//! MIX archives thousands of times would dominate runtime for no extra
//! coverage — the property under test is `AppCore`'s event handling, not the
//! asset loader, which already has its own golden tests).

mod support;

use std::cell::RefCell;

use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestRunnerConfig, TestRunner};

use ra_client::appcore::AppCore;
use ra_client::input::{InputEvent, Key};

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

fn event_strategy() -> impl Strategy<Value = InputEvent> {
    prop_oneof![
        3 => key_strategy().prop_map(InputEvent::KeyDown),
        3 => key_strategy().prop_map(InputEvent::KeyUp),
        // Wide enough to cover negative and far-outside-viewport coordinates
        // (both should be handled as "not near an edge", never panic).
        4 => (-1000i32..=5000, -1000i32..=5000)
            .prop_map(|(x, y)| InputEvent::MouseMoved { x, y }),
        1 => Just(InputEvent::MouseLeft),
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
