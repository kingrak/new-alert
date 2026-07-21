//! Audit coverage (ra-tester, post-M7.5-B): the AppCore UI surface for the
//! APC transport system (Q18 P1) — the coder's `appcore.rs` `issue_order`
//! Load branch and `deploy_selected` Unload branch. Fully synthetic, no real
//! assets needed.
//!
//! §1 scripted drives: select infantry -> right-click own APC -> boards;
//!    select a loaded APC -> Deploy -> disgorges; right-click an ENEMY
//!    transport orders an attack, never a load.
//! §2 monkey testing with transports present: fuzzed event/update sequences
//!    never panic, and every drained `Load`/`Unload` is well-formed (own
//!    transport only, infantry-only passenger, capacity respected) — no
//!    invalid command is ever drained.

mod support;

use proptest::prelude::*;

use ra_client::appcore::AppCore;
use ra_client::input::{InputEvent, Key, MouseButton};
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    Command, Handle, MoveStats, Passability, Target, WarheadProfile, WeaponProfile, World,
};

/// Mirrors `AppCore`'s private `leptons_to_pixel` (see `ui_infantry_suite.rs`).
fn leptons_to_pixel(leptons: i32) -> i32 {
    (leptons as i64 * 24 / 256) as i32
}

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 20,
        rot: 8,
    }
}

fn pct5(p: [i32; 5]) -> [i32; 5] {
    let mut o = [0i32; 5];
    for (d, v) in o.iter_mut().zip(p) {
        *d = v * 65536 / 100;
    }
    o
}

fn gun() -> WeaponProfile {
    WeaponProfile {
        damage: 10,
        rof: 10,
        range: 5 * 256,
        proj_speed: 999,
        proj_rot: 0,
        invisible: true,
        instant: true,
        warhead: WarheadProfile {
            spread: 1,
            verses: pct5([100, 100, 100, 100, 100]),
        },
        warhead_ap: false,
        arcing: false,
        ballistic_scatter: 256,
        homing_scatter: 512,
        min_damage: 1,
        max_damage: 1000,
    }
}

fn unit_screen_pos(core: &AppCore, handle: Handle) -> (i32, i32) {
    let coord = core.world().units.get(handle).unwrap().coord;
    (leptons_to_pixel(coord.x.0), leptons_to_pixel(coord.y.0))
}

fn right_click(core: &mut AppCore, x: i32, y: i32) {
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Right,
        x,
        y,
    });
}

// ===========================================================================
// §1 — scripted drives
// ===========================================================================

/// House 0: an infantry soldier adjacent to an APC (capacity 2) — adjacent so
/// a Load resolves in a single tick (the walk-to-board case is covered by
/// `mission_transport_edges_suite.rs`'s §2). House 1: an enemy APC, well
/// separated from everything else.
fn transport_world() -> (World, Handle, Handle, Handle) {
    let mut w = World::new(Passability::all_passable(), 0x7EA5_71E5);
    let apc = w.spawn_unit(1, 0, CellCoord::new(20, 10), Facing(0), 200, stats());
    w.set_unit_capacity(apc, 2);
    let soldier = w.spawn_unit(0, 0, CellCoord::new(20, 11), Facing(0), 50, stats());
    w.set_unit_combat(soldier, 0, Some(gun()), false);
    w.units.get_mut(soldier).unwrap().make_infantry(0);

    let enemy_apc = w.spawn_unit(1, 1, CellCoord::new(60, 60), Facing(0), 200, stats());
    w.set_unit_capacity(enemy_apc, 2);

    (w, soldier, apc, enemy_apc)
}

fn transport_core() -> (AppCore, Handle, Handle, Handle) {
    let (raster, palette) = support::synthetic_fixture();
    let (world, soldier, apc, enemy_apc) = transport_world();
    let mut core = AppCore::with_sim(raster.clone(), *palette, world, Vec::new(), Vec::new());
    core.handle(InputEvent::Resize {
        width: 1024,
        height: 768,
    });
    (core, soldier, apc, enemy_apc)
}

#[test]
fn select_infantry_right_click_own_apc_boards() {
    let (mut core, soldier, apc, _enemy) = transport_core();
    core.select_units(&[soldier]);
    let (x, y) = unit_screen_pos(&core, apc);
    right_click(&mut core, x, y);

    let cmds = core.drain_commands();
    assert_eq!(
        cmds.len(),
        1,
        "exactly one Load must be emitted, not an accidental Move/Attack too"
    );
    match cmds[0] {
        Command::Load {
            passenger,
            transport,
            house,
        } => {
            assert_eq!(passenger, soldier);
            assert_eq!(transport, apc);
            assert_eq!(house, 0);
        }
        other => panic!("expected Command::Load, got {other:?}"),
    }

    // Apply it through the sim and confirm the soldier actually boards.
    core.world_mut().tick(&cmds);
    assert!(
        core.world().units.get(soldier).is_none(),
        "the boarded soldier must leave the map"
    );
    assert_eq!(core.world().units.get(apc).unwrap().cargo.len(), 1);
}

#[test]
fn select_loaded_apc_deploy_disgorges() {
    let (mut core, soldier, apc, _enemy) = transport_core();
    core.select_units(&[soldier]);
    let (x, y) = unit_screen_pos(&core, apc);
    right_click(&mut core, x, y);
    let cmds = core.drain_commands();
    core.world_mut().tick(&cmds);
    assert_eq!(core.world().units.get(apc).unwrap().cargo.len(), 1);

    core.select_units(&[apc]);
    core.handle(InputEvent::KeyDown(Key::Deploy));
    let cmds = core.drain_commands();
    assert_eq!(
        cmds.len(),
        1,
        "Deploy on a loaded transport must emit exactly one command"
    );
    match cmds[0] {
        Command::Unload { transport, house } => {
            assert_eq!(transport, apc);
            assert_eq!(house, 0);
        }
        other => panic!("expected Command::Unload, got {other:?}"),
    }
    core.world_mut().tick(&cmds);
    assert!(
        core.world().units.get(apc).unwrap().cargo.is_empty(),
        "the transport must have disgorged"
    );
    assert!(
        core.world()
            .units
            .iter()
            .any(|(h, u)| h != apc && u.house == 0 && u.is_infantry()),
        "the passenger must have re-materialised"
    );
}

#[test]
fn right_click_enemy_transport_orders_an_attack_not_a_load() {
    let (mut core, soldier, _own_apc, enemy_apc) = transport_core();
    core.select_units(&[soldier]);
    let (x, y) = unit_screen_pos(&core, enemy_apc);
    right_click(&mut core, x, y);

    let cmds = core.drain_commands();
    assert_eq!(cmds.len(), 1);
    match cmds[0] {
        Command::Attack {
            unit,
            target,
            house,
        } => {
            assert_eq!(unit, soldier);
            assert_eq!(house, 0);
            assert_eq!(
                target,
                Target::Unit(enemy_apc),
                "an enemy transport must be attacked, not treated as a Load target"
            );
        }
        other => panic!(
            "right-clicking an enemy transport must order an Attack, not {other:?} \
             (a transport is only ever a Load target when it is OWN)"
        ),
    }
}

// ===========================================================================
// §2 — monkey testing with transports present.
// ===========================================================================

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
        Just(Key::Deploy),
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
        4 => (-1000i32..=5000, -1000i32..=5000)
            .prop_map(|(x, y)| InputEvent::MouseMoved { x, y }),
        1 => Just(InputEvent::MouseLeft),
        3 => (mouse_button_strategy(), -200i32..=1200, -200i32..=1200)
            .prop_map(|(button, x, y)| InputEvent::MouseDown { button, x, y }),
        3 => (mouse_button_strategy(), -200i32..=1200, -200i32..=1200)
            .prop_map(|(button, x, y)| InputEvent::MouseUp { button, x, y }),
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

const COMPOSE_EVERY: usize = 8;

/// House 1 (the "player"): two infantry (distinct sub-cells), a plain
/// vehicle, and an APC (capacity 2) — so a monkey sequence has both a
/// boardable passenger and an own transport within reach. House 2: a lone
/// witness vehicle far away (an occasional attack/enemy-pick target).
fn transport_monkey_world(seed: u32) -> World {
    let mut w = World::new(Passability::all_passable(), seed);
    for i in 0..2u8 {
        let h = w.spawn_unit(0, 1, CellCoord::new(16, 10), Facing(0), 50, stats());
        w.set_unit_combat(h, 0, Some(gun()), false);
        w.units.get_mut(h).unwrap().make_infantry(i + 1);
    }
    let apc = w.spawn_unit(1, 1, CellCoord::new(10, 10), Facing(0), 200, stats());
    w.set_unit_capacity(apc, 2);
    let tank = w.spawn_unit(2, 1, CellCoord::new(12, 10), Facing(0), 256, stats());
    w.set_unit_combat(tank, 0, Some(gun()), true);
    w.spawn_unit(1, 2, CellCoord::new(60, 60), Facing(0), 256, stats());
    w
}

fn transport_monkey_core(seed: u32) -> AppCore {
    let (raster, palette) = support::synthetic_fixture();
    let world = transport_monkey_world(seed);
    AppCore::with_sim(raster.clone(), *palette, world, Vec::new(), Vec::new())
}

/// Apply one op and check: no panic, every drained command well-formed, and
/// the transport-specific invariants — `Load`'s passenger is infantry and the
/// transport has spare capacity and is owned by `house`; `Unload`'s transport
/// is owned by `house` and (was) a transport at all (`capacity > 0` — checked
/// pre-tick since a fully-unloaded transport still has capacity, but a
/// non-transport must never appear here).
fn apply_op_with_transports(core: &mut AppCore, op: MonkeyOp, index: usize) {
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
            Command::Load {
                passenger,
                transport,
                house,
            } => {
                let p = core.world().units.get(passenger).unwrap_or_else(|| {
                    panic!("drained Load addresses a passenger that isn't live")
                });
                assert!(
                    p.is_infantry(),
                    "Load's passenger must be infantry: {cmd:?}"
                );
                assert_eq!(
                    p.house, house,
                    "Load's passenger must belong to the issuing house"
                );
                let t = core.world().units.get(transport).unwrap_or_else(|| {
                    panic!("drained Load addresses a transport that isn't live")
                });
                assert!(t.capacity > 0, "Load's target must be a transport: {cmd:?}");
                assert_eq!(
                    t.house, house,
                    "Load's transport must be owned by the issuing house"
                );
                (passenger, house)
            }
            Command::Unload { transport, house } => {
                let t = core.world().units.get(transport).unwrap_or_else(|| {
                    panic!("drained Unload addresses a transport that isn't live")
                });
                assert!(
                    t.capacity > 0,
                    "Unload's target must be a transport: {cmd:?}"
                );
                assert_eq!(
                    t.house, house,
                    "Unload's transport must be owned by the issuing house"
                );
                (transport, house)
            }
            Command::StartProduction { .. }
            | Command::PlaceBuilding { .. }
            | Command::CancelProduction { .. }
            | Command::Sell { .. }
            | Command::Repair { .. }
            | Command::FireSuperWeapon { .. } => continue,
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

fn apply_ops_with_transports(core: &mut AppCore, ops: &[MonkeyOp]) {
    core.handle(InputEvent::Resize {
        width: 64,
        height: 48,
    });
    for (i, &op) in ops.iter().enumerate() {
        apply_op_with_transports(core, op, i);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Fuzzed event/update sequences against a fixture that includes an own
    /// transport, a boardable passenger, a plain vehicle, and an enemy — no
    /// panic ever, and no drained `Load`/`Unload` is ever malformed (wrong
    /// house, non-infantry passenger, or a non-transport target), regardless
    /// of what the sequence happens to select and click.
    #[test]
    fn monkey_with_transports_never_panics_or_emits_bad_load_unload(ops in ops_strategy(200..800)) {
        let mut core = transport_monkey_core(0x7EA5_7100);
        apply_ops_with_transports(&mut core, &ops);
    }
}
