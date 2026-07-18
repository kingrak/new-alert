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
use ra_sim::{Command, Target};

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
        // M7: F1 toggles the controls-hint overlay (`show_help`); fuzzing it
        // in exercises `draw_help_overlay` at arbitrary points in a sequence
        // alongside everything else, same as any other key.
        Just(Key::Help),
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
            // M4: an attack order also carries the issuing unit + house; the same
            // liveness/ownership invariant applies.
            Command::Attack { unit, house, .. } => (unit, house),
            // M5: deploy addresses a unit + house (same invariant); the
            // production/placement commands do not address a unit, and the monkey
            // never enables the sidebar so they cannot be emitted here anyway.
            Command::Deploy { unit, house } => (unit, house),
            Command::StartProduction { .. }
            | Command::PlaceBuilding { .. }
            | Command::CancelProduction { .. }
            | Command::Sell { .. } => continue,
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

// ---------------------------------------------------------------------
// Armed-units variant (M4, new): both the existing units-variants above use
// `synthetic_world_with_units`/`load_real_game`, whose *synthetic* fixture
// has no armed unit at all — `AppCore::issue_order` only emits `Attack` for
// an armed selected unit (unarmed ones are silently skipped), so
// `synthetic_monkey_with_units_never_panics` can never actually generate an
// `Attack` command; only the assets-gated real-scenario variant (JEEPs with
// a real M60mg) ever could, and that one skips cleanly in CI without assets.
// That leaves `Attack`'s well-formedness genuinely unexercised by any
// always-run suite. `support::synthetic_world_with_armed_units` closes that
// gap: armed house-1 jeeps within immediate range of a house-2 target, so
// fuzzed click sequences can and do produce `Attack` orders without needing
// real assets. Same invariant as `apply_op_with_units`, plus an explicit
// check that no drained `Attack` ever targets a unit of the *same* house as
// the issuing unit (self/friendly-fire targeting must be structurally
// impossible via the click path — `AppCore::issue_order` only treats a
// different-house unit as "enemy").
// ---------------------------------------------------------------------

fn apply_op_with_armed_units(core: &mut AppCore, op: MonkeyOp, index: usize) {
    match op {
        MonkeyOp::Event(ev) => core.handle(ev),
        MonkeyOp::Update(dt) => core.update(dt),
    }
    for cmd in core.drain_commands() {
        let (unit, house) = match cmd {
            Command::Move { unit, house, .. } => (unit, house),
            Command::Stop { unit, house } => (unit, house),
            Command::Attack { unit, house, .. } => (unit, house),
            Command::Deploy { unit, house } => (unit, house),
            Command::StartProduction { .. }
            | Command::PlaceBuilding { .. }
            | Command::CancelProduction { .. }
            | Command::Sell { .. } => continue,
        };
        let owner = core.world().units.get(unit).unwrap_or_else(|| {
            panic!("drained {cmd:?} addresses a handle that isn't live in the world")
        });
        assert_eq!(
            house, owner.house,
            "drained {cmd:?} has house {house}, but the unit it addresses is owned by house {}",
            owner.house
        );
        if let Command::Attack {
            target: Target::Unit(target),
            house,
            ..
        } = cmd
        {
            if let Some(target_unit) = core.world().units.get(target) {
                assert_ne!(
                    target_unit.house, house,
                    "drained {cmd:?} orders house {house} to attack its own unit \
                     (target is also house {house}) — self/friendly targeting must be \
                     structurally unreachable through the click path"
                );
            }
            // A stale/dead target handle is fine (the unit may have died
            // between the click and the drain in a fuzzed sequence); only a
            // *live* same-house target would be the structural bug.
        }
    }
    if index.is_multiple_of(COMPOSE_EVERY) {
        let frame = core.compose_camera();
        let (vw, vh) = core.viewport_size();
        assert_eq!(frame.width, vw);
        assert_eq!(frame.height, vh);
    }
}

fn apply_ops_with_armed_units(core: &mut AppCore, ops: &[MonkeyOp]) {
    core.handle(InputEvent::Resize {
        width: 64,
        height: 48,
    });
    for (i, &op) in ops.iter().enumerate() {
        apply_op_with_armed_units(core, op, i);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Always-run: fuzzed event sequences against the armed synthetic
    /// fixture. No panic, every drained command well-formed, and no
    /// self-targeting `Attack` ever slips through.
    #[test]
    fn synthetic_monkey_with_armed_units_never_panics(ops in ops_strategy(200..800)) {
        let (mut core, _jeeps, _target) = support::synthetic_core_with_armed_units(0xA24E_D000);
        apply_ops_with_armed_units(&mut core, &ops);
    }
}

// ---------------------------------------------------------------------
// Econ/sidebar variant (M5, new): the units/armed-units variants above never
// call `enable_sidebar`, so none of them can generate a sidebar-area click
// or drive placement mode -- `StartProduction`/`PlaceBuilding`/
// `CancelProduction` are structurally unreachable from any monkey run so
// far (see the `continue` arms in `apply_op_with_units`/
// `apply_op_with_armed_units` above). This variant closes that gap:
// `support::synthetic_core_with_econ` has the sidebar enabled for house 1,
// and the event strategy below adds `Key::Deploy` plus mouse coordinates
// biased to land in the sidebar strip (not just the tactical area). A
// deterministic prefix (deploy the MCV, start POWR) runs before the fuzzed
// suffix so production has something to chew on within the tick budget --
// otherwise "click a ready row" would depend on winning two separate
// lotteries (randomly finish a multi-tick build *and* randomly click the
// right pixel) before placement mode is ever reachable at all.
// ---------------------------------------------------------------------

/// Like [`key_strategy`] plus `Key::Deploy` (M5). Also carries `Key::Help`
/// (M7) — this is the only monkey fixture with the build sidebar enabled at
/// all (see the econ fixture swap below), so it's the only variant where F1
/// toggling can co-occur with a fuzzed radar/cameo sidebar click in the same
/// sequence.
fn econ_key_strategy() -> impl Strategy<Value = Key> {
    prop_oneof![
        Just(Key::Left),
        Just(Key::Right),
        Just(Key::Up),
        Just(Key::Down),
        Just(Key::Deploy),
        Just(Key::Help),
    ]
}

/// Like [`event_strategy`], but the mouse-position ranges are widened/biased
/// so a meaningful fraction of `MouseDown`/`MouseUp`/`MouseMoved` events land
/// inside the sidebar strip (`x >= tactical_width()`) rather than only the
/// tactical area -- the general-purpose strategy's `x` range was sized for a
/// map click, not a ~130px-wide UI panel off to the side of a much wider
/// viewport.
fn econ_event_strategy() -> impl Strategy<Value = InputEvent> {
    prop_oneof![
        3 => econ_key_strategy().prop_map(InputEvent::KeyDown),
        3 => econ_key_strategy().prop_map(InputEvent::KeyUp),
        3 => (-1000i32..=5000, -1000i32..=5000)
            .prop_map(|(x, y)| InputEvent::MouseMoved { x, y }),
        1 => Just(InputEvent::MouseLeft),
        // General tactical-area-ish coordinates (as the other suites use)...
        2 => (mouse_button_strategy(), -200i32..=1200, -200i32..=1200)
            .prop_map(|(button, x, y)| InputEvent::MouseDown { button, x, y }),
        2 => (mouse_button_strategy(), -200i32..=1200, -200i32..=1200)
            .prop_map(|(button, x, y)| InputEvent::MouseUp { button, x, y }),
        // ...plus a dedicated sidebar-biased range: x anchored past a small
        // viewport's tactical width, y spanning the header + several rows
        // (see `SIDEBAR_ROWS_TOP`/row height in `ui_scripted_drive.rs`).
        3 => (mouse_button_strategy(), 400i32..=900, -20i32..=200)
            .prop_map(|(button, x, y)| InputEvent::MouseDown { button, x, y }),
        3 => (mouse_button_strategy(), 400i32..=900, -20i32..=200)
            .prop_map(|(button, x, y)| InputEvent::MouseUp { button, x, y }),
        // Deliberately bounded small, exactly like `event_strategy`'s Resize
        // op above (this suite fuzzes sequencing, not allocation size/compose
        // cost -- an early, much wider range here made this suite ~200x
        // slower than its siblings for no extra sequencing coverage).
        1 => (1u32..=200, 1u32..=200)
            .prop_map(|(width, height)| InputEvent::Resize { width, height }),
    ]
}

fn econ_op_strategy() -> impl Strategy<Value = MonkeyOp> {
    prop_oneof![
        5 => econ_event_strategy().prop_map(MonkeyOp::Event),
        2 => (0u32..=20_000).prop_map(MonkeyOp::Update),
    ]
}

fn econ_ops_strategy(len: std::ops::Range<usize>) -> impl Strategy<Value = Vec<MonkeyOp>> {
    proptest::collection::vec(econ_op_strategy(), len)
}

/// Apply one op to the econ `core` and check: no panic (implicit); every
/// drained command well-formed (live handle where one is addressed, `house`
/// matches the real owner); every M5 command's house is exactly the
/// controlled house 1 (there is no click path that can address house 2 --
/// the sidebar/deploy/placement surface is unconditionally bound to
/// `player_house`, so this is the "no command for enemy house" invariant);
/// and `PlaceBuilding` never names a building type other than the one that
/// was `ready_building` *before* this op ran (a `PlaceBuilding` can only be
/// emitted from placement mode, which only `begin_placement` enters, which
/// only `sidebar_click` calls, and only for a row already reporting
/// `ready` -- "no placement without a completed building").
fn apply_op_with_econ(core: &mut AppCore, op: MonkeyOp, index: usize) {
    let ready_before = core.world().house(1).and_then(|h| h.ready_building);

    match op {
        MonkeyOp::Event(ev) => core.handle(ev),
        MonkeyOp::Update(dt) => core.update(dt),
    }

    for cmd in core.drain_commands() {
        match cmd {
            Command::Move { unit, house, .. }
            | Command::Stop { unit, house }
            | Command::Attack { unit, house, .. }
            | Command::Deploy { unit, house } => {
                assert_eq!(
                    house, 1,
                    "drained {cmd:?} was not issued for the controlled house"
                );
                let owner = core.world().units.get(unit).unwrap_or_else(|| {
                    panic!("drained {cmd:?} addresses a handle that isn't live in the world")
                });
                assert_eq!(house, owner.house);
            }
            Command::StartProduction { house, .. }
            | Command::CancelProduction { house, .. }
            | Command::Sell { house, .. } => {
                assert_eq!(
                    house, 1,
                    "drained {cmd:?} was not issued for the controlled house"
                );
            }
            Command::PlaceBuilding {
                house, building, ..
            } => {
                assert_eq!(
                    house, 1,
                    "drained {cmd:?} was not issued for the controlled house"
                );
                assert_eq!(
                    Some(building),
                    ready_before,
                    "drained {cmd:?}, but building {building} was not the ready building \
                     just before this op (ready was {ready_before:?}) -- placement without a \
                     completed building"
                );
            }
        }
    }
    if index.is_multiple_of(COMPOSE_EVERY) {
        let frame = core.compose_camera();
        let (vw, vh) = core.viewport_size();
        assert_eq!(frame.width, vw);
        assert_eq!(frame.height, vh);
    }
}

/// Deterministic prefix: select + deploy the starter MCV, then start POWR
/// production directly (bypassing sidebar-click luck) -- see the module docs
/// above on why the fuzzed suffix alone can't reliably reach placement mode.
fn econ_prefix(core: &mut AppCore) {
    let mcv_cell = support::econ_mcv_cell();
    const CELL_PIXELS: i32 = 24;
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
    core.handle(InputEvent::KeyDown(Key::Deploy));
    core.handle(InputEvent::KeyUp(Key::Deploy));
    for _ in 0..5 {
        core.update(67);
    }
    core.drain_commands(); // the prefix's own commands are trusted by construction
    core.start_production(ra_sim::BuildItem::Building(support::ECON_B_POWR));
    core.drain_commands();
}

fn apply_ops_with_econ(core: &mut AppCore, ops: &[MonkeyOp]) {
    core.handle(InputEvent::Resize {
        width: 640,
        height: 400,
    });
    econ_prefix(core);
    for (i, &op) in ops.iter().enumerate() {
        apply_op_with_econ(core, op, i);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Always-run: fuzzed event sequences (including sidebar-area clicks and
    /// placement-mode drives) against the econ synthetic fixture. No panic,
    /// every drained command well-formed and scoped to the controlled house,
    /// and no placement without a completed building.
    ///
    /// M7: the fixture now also has the radar minimap and per-row cameo art
    /// enabled (`support::synthetic_core_with_econ_radar_cameo` rather than
    /// the plain `synthetic_core_with_econ`) — this is the only monkey
    /// fixture with the sidebar enabled, and `sidebar_click` only reaches the
    /// radar-jump / taller-cameo-row code paths when the sidebar is on (see
    /// that helper's doc comment), so it's the only place those paths can be
    /// fuzzed at all. The existing invariants below (well-formed commands,
    /// scoped to the controlled house, no placement without a ready
    /// building) must keep holding with radar clicks and taller rows mixed
    /// into the same sequences — a radar click never emits a `Command` at
    /// all, so it is invisible to those checks by construction, and a taller
    /// row only changes the sidebar's y-geometry, not which item a given hit
    /// resolves to.
    #[test]
    fn synthetic_monkey_with_econ_never_panics(ops in econ_ops_strategy(200..800)) {
        let (mut core, _mcv) = support::synthetic_core_with_econ_radar_cameo(0xE58E_C0E1, 5000);
        apply_ops_with_econ(&mut core, &ops);
    }
}
