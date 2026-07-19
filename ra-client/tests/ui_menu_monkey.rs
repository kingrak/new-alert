//! Menu monkey test (M7.8 coverage layer 1, DESIGN.md §4.8 layer 3 applied to
//! the pre-game `App` state machine rather than the in-game `AppCore`):
//! seeded, proptest-driven random `InputEvent`/`update()` sequences fired at
//! `App` across every reachable state, with the asset-free
//! [`support::MenuSynthFactory`]. No panic (implicit — a panic aborts the
//! test), and three structural invariants checked after *every* op:
//!
//! 1. **No invalid state transition.** "Valid" is defined exactly by the
//!    state diagram in `menu.rs`'s module doc comment — self-loops (an op
//!    that doesn't change state) are always allowed; any other (before,
//!    after) pair must be one of the diagram's documented edges. See
//!    [`valid_transition`] for the edge set, derived by reading every
//!    `self.state = ...` assignment in `App::activate`/`handle_ingame`/
//!    `handle_menu`/`update`.
//! 2. **Quit only from a documented action.** `App::quit` can only flip
//!    false->true while handling `Action::Quit`, which only exists in
//!    `items_main_menu()` — so it is only ever reachable while
//!    `state == AppState::MainMenu`. That is externally observable without
//!    an internal hook: record the state *before* each op, and if `quit`
//!    flips on during this op, assert that pre-op state was `MainMenu`.
//! 3. **The wrapped core is `Some` iff the state needs one.** `start_game`
//!    is the only place `self.core` becomes `Some`, and `quit_to_menu` (the
//!    only path back to `MainMenu` from a running game) is the only place it
//!    becomes `None` again — so `core().is_some()` must equal
//!    `state ∈ {InGame, Paused, GameOver}` after every single op, not just
//!    at designed checkpoints.

mod support;

use proptest::prelude::*;

use ra_client::compositor::IndexedImage;
use ra_client::input::{InputEvent, Key, MouseButton};
use ra_client::menu::{App, AppState, BuiltMission, CampaignEntry, CampaignFactory};
use ra_client::AppCore;
use ra_sim::campaign::{taction, tevent};
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{Campaign, MoveStats, Passability, TActionDef, TEventDef, TriggerType, World};

/// One fuzzed operation: either an `InputEvent` or a virtual-time tick, same
/// shape as `ui_monkey.rs`'s `MonkeyOp` (`AppCore::update`/`App::update`
/// isn't itself an `InputEvent`, so it's modeled as a sibling op).
#[derive(Debug, Clone, Copy)]
enum MonkeyOp {
    Event(InputEvent),
    Update(u32),
}

/// Keys relevant to the menu state machine: `Menu` (Esc, backs out / opens
/// pause) and `Confirm` (Enter, activates the focused item) drive the state
/// machine itself; `Up`/`Down` move focus in a menu state or scroll the
/// camera in `InGame`; `Left`/`Right`/`Deploy`/`Help` are pure `AppCore`
/// pass-through when `InGame` (already heavily fuzzed by `ui_monkey.rs` —
/// included here too so a menu-state transition mid-sequence can't dodge
/// them).
fn key_strategy() -> impl Strategy<Value = Key> {
    prop_oneof![
        Just(Key::Menu),
        Just(Key::Confirm),
        Just(Key::Up),
        Just(Key::Down),
        Just(Key::Left),
        Just(Key::Right),
        Just(Key::Deploy),
        Just(Key::Help),
    ]
}

fn mouse_button_strategy() -> impl Strategy<Value = MouseButton> {
    prop_oneof![Just(MouseButton::Left), Just(MouseButton::Right)]
}

/// Coordinates wide enough to cover every button rect at the default
/// 1024x768 viewport *and* every fuzzed-small `Resize` target below, plus
/// clearly-out-of-bounds negative/overshoot values (menu hit-testing must
/// reject those cleanly, not panic).
fn event_strategy() -> impl Strategy<Value = InputEvent> {
    prop_oneof![
        3 => key_strategy().prop_map(InputEvent::KeyDown),
        3 => key_strategy().prop_map(InputEvent::KeyUp),
        3 => (-300i32..=1400, -300i32..=1400)
            .prop_map(|(x, y)| InputEvent::MouseMoved { x, y }),
        1 => Just(InputEvent::MouseLeft),
        4 => (mouse_button_strategy(), -300i32..=1400, -300i32..=1400)
            .prop_map(|(button, x, y)| InputEvent::MouseDown { button, x, y }),
        4 => (mouse_button_strategy(), -300i32..=1400, -300i32..=1400)
            .prop_map(|(button, x, y)| InputEvent::MouseUp { button, x, y }),
        // Bounded small for compose-cost reasons, same rationale as every
        // other UI monkey suite's Resize range (`ui_monkey.rs`).
        2 => (1u32..=400, 1u32..=400)
            .prop_map(|(width, height)| InputEvent::Resize { width, height }),
        // The brief's "wheel" op: `App::handle_ingame` forwards any
        // non-Menu/Confirm event straight to the wrapped core, so this only
        // does anything once a game is running, but is fuzzed unconditionally
        // like everything else (menu states must silently ignore it too).
        2 => (prop_oneof![Just(0u8), Just(1u8)], any::<bool>())
            .prop_map(|(column, up)| InputEvent::SidebarScroll { column, up }),
    ]
}

fn op_strategy() -> impl Strategy<Value = MonkeyOp> {
    prop_oneof![
        6 => event_strategy().prop_map(MonkeyOp::Event),
        1 => (0u32..=5_000).prop_map(MonkeyOp::Update),
    ]
}

fn ops_strategy(len: std::ops::Range<usize>) -> impl Strategy<Value = Vec<MonkeyOp>> {
    proptest::collection::vec(op_strategy(), len)
}

/// The state-transition graph documented in `menu.rs`'s module doc comment,
/// as concrete (before, after) edges — see the `menu.rs` ASCII diagram for the
/// original (pre-campaign) skirmish half:
///
/// ```text
///   MainMenu ──Skirmish──▶ SkirmishSetup ──Start──▶ InGame ──Esc──▶ Paused
///      ▲                        │  ▲                   │  │            │
///      └────Quit-to-menu────────┘  └──Back────────────┘  │        Resume│
///      ▲                                                  ▼              │
///      └───────────Continue──────────── GameOver ◀──Victory/Defeat──────┘
/// ```
///
/// Cross-checked directly against `App::activate`'s match arms plus the two
/// `Key::Menu` special cases in `handle_ingame`/`handle_menu` and the
/// automatic `InGame -> GameOver` transition in `App::update`. A self-loop
/// (`before == after`) is always valid and handled by the caller before this
/// is consulted.
///
/// The campaign-flow edges (added for M7.5-A coverage, `menu.rs` line refs as
/// of this writing):
/// - `(MainMenu, CampaignList)` — `Action::GotoCampaign` (`activate`, ~line 550).
/// - `(CampaignList, MainMenu)` — `Action::BackToMenu`, or `Key::Menu` backing
///   out of `CampaignList` (`handle_menu`, ~line 493).
/// - `(CampaignList, Briefing)` — `Action::SelectMission(i)` ->
///   `goto_briefing` (~line 560/575).
/// - `(Briefing, CampaignList)` — `Action::GotoCampaign` ("BACK" item in
///   `items_briefing`, ~line 1023), or `Key::Menu` backing out of `Briefing`
///   (`handle_menu`, ~line 496).
/// - `(Briefing, InGame)` — `Action::StartMission` -> `start_mission` (~line
///   561/601).
/// - `(GameOver, InGame)` — `Action::RetryMission` -> `start_mission(campaign_
///   current)` (~line 562), only offered on a campaign Defeat (`items_
///   gameover`, ~line 1194).
/// - `(GameOver, Briefing)` — `Action::Continue` -> `on_continue`, when
///   `in_campaign && victory` and another mission remains (~line 645-663).
fn valid_transition(before: AppState, after: AppState) -> bool {
    use AppState::*;
    matches!(
        (before, after),
        (MainMenu, SkirmishSetup)   // GotoSkirmish
            | (SkirmishSetup, MainMenu) // BackToMenu, or Esc backing out
            | (SkirmishSetup, InGame)   // StartGame succeeds (always does: map list is non-empty and config.map is kept in-range by select_map/Default)
            | (InGame, Paused)          // Esc (Key::Menu) in handle_ingame
            | (InGame, GameOver)        // automatic, inside App::update, when core.game_over() != Ongoing
            | (Paused, InGame)          // Resume, or Esc backing out
            | (Paused, MainMenu)        // QuitToMenu
            | (GameOver, MainMenu) // Continue (defeat, or victory + campaign complete, or skirmish)
            // --- Campaign flow (M7.5-A) ---
            | (MainMenu, CampaignList)  // GotoCampaign
            | (CampaignList, MainMenu)  // BackToMenu, or Esc backing out
            | (CampaignList, Briefing)  // SelectMission(i)
            | (Briefing, CampaignList)  // GotoCampaign ("BACK"), or Esc backing out
            | (Briefing, InGame)        // StartMission
            | (GameOver, InGame)        // RetryMission (campaign defeat only)
            | (GameOver, Briefing) // Continue after a campaign victory, more missions remain
    )
}

/// Apply one op and check every invariant that must hold afterward.
fn apply_op(a: &mut ra_client::menu::App, op: MonkeyOp, index: usize) {
    let state_before = a.state();
    let quit_before = a.quit_requested();

    match op {
        MonkeyOp::Event(ev) => a.handle(ev),
        MonkeyOp::Update(dt) => a.update(dt),
    }

    let state_after = a.state();
    if state_before != state_after {
        assert!(
            valid_transition(state_before, state_after),
            "op #{index} ({op:?}) made an undocumented transition {state_before:?} -> {state_after:?}"
        );
    }

    let quit_after = a.quit_requested();
    if quit_after && !quit_before {
        assert_eq!(
            state_before,
            AppState::MainMenu,
            "op #{index} ({op:?}) set the quit flag, but the pre-op state was {state_before:?}, \
             not MainMenu — Action::Quit only exists in items_main_menu()"
        );
    }
    // The quit flag is sticky: once set, fuzzed input must never clear it
    // (there is no "un-quit" action anywhere in `App`).
    if quit_before {
        assert!(
            quit_after,
            "op #{index} ({op:?}) cleared an already-set quit flag"
        );
    }

    let core_present = a.core().is_some();
    let needs_core = matches!(
        state_after,
        AppState::InGame | AppState::Paused | AppState::GameOver
    );
    assert_eq!(
        core_present, needs_core,
        "op #{index} ({op:?}): state {state_after:?} but core().is_some() == {core_present}"
    );

    // drain_commands/drain_sounds must never panic in any state (empty is
    // fine and expected — this suite never sets up armed/econ fixtures, see
    // `ui_monkey.rs` for command well-formedness fuzzing).
    let _ = a.drain_commands();
    let _ = a.drain_sounds();

    // Sample compose() periodically in every state (menu screens included) —
    // must always produce a full-viewport frame, never panic.
    if index.is_multiple_of(4) {
        let f = a.compose();
        // `App::blank`/`compose_*` always size to the current viewport, which
        // only ever changes via `Resize` (clamped to [1, 8192] by `handle`).
        assert!(
            f.width >= 1 && f.height >= 1,
            "op #{index}: zero-sized frame"
        );
    }
}

fn apply_ops(a: &mut ra_client::menu::App, ops: &[MonkeyOp]) {
    for (i, &op) in ops.iter().enumerate() {
        apply_op(a, op, i);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Fresh `App` per case (cheap: a tiny synthetic map list, no game
    /// started until/unless the fuzzed sequence itself reaches `StartGame`),
    /// fuzzed for 150-500 ops — tens of thousands of events across the run.
    #[test]
    fn menu_monkey_never_panics_or_transitions_invalidly(ops in ops_strategy(150..500)) {
        let mut a = support::menu_app();
        apply_ops(&mut a, &ops);
    }
}

// ===========================================================================
// M7.5-A: monkey coverage extended into CampaignList/Briefing.
//
// `support::menu_app()` never installs a `CampaignFactory`, so the CAMPAIGN
// button is disabled (`items_main_menu`, `menu.rs` ~line 972-976) and the
// monkey above can never reach `CampaignList`/`Briefing`/the campaign-flavored
// `GameOver` items (`RetryMission`). This second monkey attaches a tiny
// 2-mission synthetic campaign so the SAME fuzzed-op machinery above also
// walks the campaign states — asserting the identical invariants (no panic,
// `valid_transition`-only state changes, `core().is_some()` gating,
// `compose()` never zero-sized).
// ===========================================================================

fn win_lose_trigger(win: bool) -> TriggerType {
    TriggerType {
        name: if win { "win" } else { "lose" }.into(),
        persist: 0, // VOLATILE
        house: 1,
        event_ctrl: 0, // ONLY
        action_ctrl: 0,
        e1: TEventDef {
            code: tevent::TIME,
            team: -1,
            data: 0,
        },
        e2: TEventDef {
            code: tevent::NONE,
            team: -1,
            data: 0,
        },
        a1: TActionDef {
            code: if win { taction::WIN } else { taction::LOSE },
            team: -1,
            trigger: -1,
            data: -1,
        },
        a2: TActionDef {
            code: taction::NONE,
            team: -1,
            trigger: -1,
            data: -1,
        },
    }
}

/// A world that resolves on tick 0: victory (mission 1) or defeat (mission 2)
/// — see [`MonkeyCampaign`]. Mirrors `ui_campaign_flow.rs`'s
/// `synth_campaign_world`.
fn monkey_campaign_world(win: bool) -> World {
    let mut world = World::new(Passability::all_passable(), 0x5EED_1234);
    world.init_houses(8, 0);
    world.set_player_house(1);
    world.spawn_unit(
        0,
        1,
        CellCoord::new(10, 10),
        Facing(0),
        100,
        MoveStats {
            max_speed: 20,
            rot: 8,
        },
    );
    let t = win_lose_trigger(win);
    let camp = Campaign {
        triggers: vec![t],
        teamtypes: Vec::new(),
        waypoints: vec![-1; 101],
        globals: vec![false; 8],
        cell_triggers: Vec::new(),
        state: vec![ra_sim::campaign::TriggerState::default()],
        started: false,
        mission_timer: None,
        evac_cells: Vec::new(),
        civ_evacuated: vec![false; 8],
        reveal_all: false,
        reveal_cells: Vec::new(),
        pending_texts: Vec::new(),
        pending_speech: Vec::new(),
    };
    world.set_campaign(camp);
    world
}

/// Mission 1 always wins on tick 0 (exercises the victory->next-briefing
/// `GameOver -> Briefing` edge); mission 2 always loses on tick 0 (exercises
/// the defeat `RETRY MISSION` -> `GameOver -> InGame` edge). Deterministic by
/// design so the monkey's random `Update` timings don't matter — one
/// `update()` past `dt=0` is enough for either trigger to resolve.
struct MonkeyCampaign;
impl CampaignFactory for MonkeyCampaign {
    fn missions(&self) -> Vec<CampaignEntry> {
        vec![
            CampaignEntry {
                scenario: "m1".into(),
                name: "Monkey One".into(),
            },
            CampaignEntry {
                scenario: "m2".into(),
                name: "Monkey Two".into(),
            },
        ]
    }
    fn build(&self, scenario: &str) -> Result<BuiltMission, String> {
        let win = scenario == "m1";
        let raster = IndexedImage {
            width: 8,
            height: 8,
            pixels: vec![0u8; 64],
        };
        let core = AppCore::with_sim(
            raster,
            [[0u8; 3]; 256],
            monkey_campaign_world(win),
            Vec::new(),
            Vec::new(),
        );
        Ok(BuiltMission {
            core,
            start: CellCoord::new(10, 10),
            name: if win { "Monkey One" } else { "Monkey Two" }.into(),
            briefing: format!("Briefing for {scenario}."),
        })
    }
}

fn menu_app_with_campaign() -> App {
    let mut a = App::new(Vec::new(), Box::new(support::MenuSynthFactory))
        .with_campaign(Box::new(MonkeyCampaign));
    a.handle(InputEvent::Resize {
        width: 1024,
        height: 768,
    });
    a
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Same fuzzed-op machinery, campaign-enabled `App` — reaches
    /// `CampaignList`/`Briefing` (and, when the randomly-focused mission is
    /// "Monkey Two", the defeat `RetryMission` edge) with no panic and no
    /// undocumented transition.
    #[test]
    fn campaign_menu_monkey_never_panics_or_transitions_invalidly(ops in ops_strategy(150..500)) {
        let mut a = menu_app_with_campaign();
        apply_ops(&mut a, &ops);
    }
}
