//! M7.21 — production cancel + hold through the real input seam.
//!
//! The player-reported must-have (cycle brief): a naval yard built on a
//! landlocked base completes, can never be placed, and used to wedge the
//! sidebar lane and the money forever — `AppCore::cancel_production` existed
//! with zero callers. These tests prove the **wiring**, not the method: every
//! cancel/hold here goes through `AppCore::handle` with real `InputEvent`s at
//! cameo pixel coordinates (the M7.21 P0 right-click route), so unwiring the
//! sidebar right-click path makes them fail (revert-sensitivity, P3e).
//!
//! Original click table being exercised (`StripClass::SelectClass::Action`,
//! SIDEBAR.CPP:2160-2256): right-click on an actively-building cameo →
//! SUSPEND ("on hold"); right-click on a suspended or completed cameo →
//! ABANDON with refund ("canceled", `FactoryClass::Abandon`,
//! FACTORY.CPP:481); left-click on a suspended cameo → PRODUCE resume
//! ("building"); left-click on an actively-building cameo → the
//! "unable to comply" scold.

mod support;

use ra_client::appcore::{AppCore, SoundEvent};
use ra_client::input::{InputEvent, MouseButton};
use ra_sim::coords::CellCoord;
use ra_sim::{BuildItem, BuildingProto, Catalog, Command, EconRules, Passability, ProdKind, World};

// Building type ids in the local fixture catalog.
const B_FACT: u32 = 0; // construction yard
const B_POWR: u32 = 1; // power plant, cost 30
const B_SYRD: u32 = 2; // naval yard, cost 150 — shore-placement rule applies by name

const INITIAL_CREDITS: i32 = 1000;

// Sidebar geometry (appcore privates, replicated black-box exactly as
// `ui_sidebar_scripted_drive.rs` / `ui_radar_cameo_f1_suite.rs` do).
const SIDEBAR_W: i32 = 130;
const NO_RADAR_ROWS_TOP: i32 = 7 * 3 + 12;
const SIDEBAR_ROW_H: i32 = 22;

fn bproto(name: &str, w: u8, h: u8, cost: i32, prereq: Vec<u32>, cy: bool) -> BuildingProto {
    BuildingProto {
        is_barracks: false,
        name: name.to_string(),
        foot_w: w,
        foot_h: h,
        max_health: 400,
        armor: 0,
        power: 0,
        cost,
        prereq,
        is_refinery: false,
        is_construction_yard: cy,
        is_war_factory: false,
        free_harvester_unit: None,
        sight: 4,
        sprite_id: 0,
        weapon: None,
        has_turret: false,
        charges: false,
        is_wall: false,
        storage: 0,
    }
}

/// A landlocked base: an all-land map (no water cell anywhere), a standing
/// construction yard for house 1, and a sidebar offering POWR and SYRD.
/// The naval yard is buildable (the *factory* requirement for structures is
/// the construction yard) but never placeable (`footprint_placeable`'s
/// shore rule: a SYRD needs adjacent water) — the user's exact trap.
fn landlocked_core() -> AppCore {
    let mut world = World::new(Passability::all_passable(), 0x0521_0007);
    world.set_catalog(Catalog {
        buildings: vec![
            bproto("FACT", 3, 3, 100, vec![], true),
            bproto("POWR", 2, 2, 30, vec![B_FACT], false),
            bproto("SYRD", 3, 3, 150, vec![B_FACT], false),
        ],
        units: vec![],
        econ: EconRules::default(),
    });
    world.init_houses(2, INITIAL_CREDITS);
    world
        .spawn_building(B_FACT, 1, CellCoord::new(20, 20))
        .unwrap();

    let (raster, palette) = support::synthetic_fixture();
    let mut core = AppCore::with_sim(raster.clone(), *palette, world, Vec::new(), Vec::new());
    core.enable_sidebar(
        1,
        vec![BuildItem::Building(B_POWR), BuildItem::Building(B_SYRD)],
    );
    core.handle(InputEvent::Resize {
        width: 900,
        height: 100,
    });
    core
}

/// Pixel coordinates of the structures-column cameo for `row` (0-based) under
/// [`landlocked_core`]'s 900x100 viewport (no radar, no cameo art).
fn cameo_xy(core: &AppCore, row: i32) -> (i32, i32) {
    let tw = core.tactical_width() as i32;
    assert_eq!(tw, 900 - SIDEBAR_W, "fixture viewport geometry changed");
    (tw + 10, NO_RADAR_ROWS_TOP + row * SIDEBAR_ROW_H + 5)
}

fn click(core: &mut AppCore, button: MouseButton, x: i32, y: i32) {
    core.handle(InputEvent::MouseDown { button, x, y });
    core.handle(InputEvent::MouseUp { button, x, y });
}

/// Advance one sim tick (TICKS_PER_SECOND = 15 → 67 ms of virtual time).
fn tick(core: &mut AppCore) {
    core.update(67);
}

// ===========================================================================
// P3b — the user's exact scenario: landlocked base, naval yard built to
// completion, unplaceable; right-click on the cameo → full refund, lane
// free, sidebar starts something else.
// ===========================================================================

#[test]
fn landlocked_naval_yard_right_click_cancels_full_refund_and_frees_the_lane() {
    let mut core = landlocked_core();
    let (x, y_syrd) = cameo_xy(&core, 1);
    let (_, y_powr) = cameo_xy(&core, 0);

    // Start the naval yard from its cameo.
    click(&mut core, MouseButton::Left, x, y_syrd);
    let cmds = core.drain_commands();
    assert_eq!(
        cmds,
        vec![Command::StartProduction {
            house: 1,
            item: BuildItem::Building(B_SYRD),
        }],
        "the cameo click should start SYRD production"
    );

    // Build it to completion.
    let mut ready = false;
    for _ in 0..600 {
        tick(&mut core);
        if core.world().house(1).unwrap().ready_building == Some(B_SYRD) {
            ready = true;
            break;
        }
    }
    assert!(ready, "the naval yard should finish building");
    assert_eq!(
        core.world().house_credits(1),
        INITIAL_CREDITS - 150,
        "the full cost was drained during the build"
    );

    // Landlocked: there is genuinely no legal placement cell anywhere (the
    // shore rule — a naval yard needs adjacent water; this map has none).
    let unplaceable = !(0..40).any(|cy| {
        (0..40).any(|cx| {
            core.world()
                .can_place_building(1, B_SYRD, CellCoord::new(cx, cy))
        })
    });
    assert!(unplaceable, "test setup: the SYRD must be unplaceable");

    // A left-click on the ready cameo enters placement mode (the stuck
    // state the player was trapped in)...
    click(&mut core, MouseButton::Left, x, y_syrd);
    assert_eq!(core.placing(), Some(B_SYRD));
    core.drain_commands();
    core.drain_sounds();

    // ...and the M7.21 right-click resolves it: abandon, full refund,
    // placement mode dropped, EVA "Canceled".
    click(&mut core, MouseButton::Right, x, y_syrd);
    assert_eq!(
        core.drain_commands(),
        vec![Command::CancelProduction {
            house: 1,
            kind: ProdKind::Building,
        }],
        "the cameo right-click should cancel the ready building"
    );
    assert_eq!(core.placing(), None, "placement mode must be dropped");
    assert!(
        core.drain_sounds().contains(&SoundEvent::Canceled),
        "EVA 'Canceled' (VOX_CANCELED/CANCLD1) should be queued"
    );
    tick(&mut core);
    let hs = core.world().house(1).unwrap();
    assert_eq!(hs.ready_building, None, "ready slot cleared");
    assert!(hs.building_prod.is_none(), "lane free");
    assert_eq!(
        core.world().house_credits(1),
        INITIAL_CREDITS,
        "a completed-but-unplaced building refunds its full cost"
    );

    // The freed lane starts a new item straight from the sidebar.
    click(&mut core, MouseButton::Left, x, y_powr);
    assert_eq!(
        core.drain_commands(),
        vec![Command::StartProduction {
            house: 1,
            item: BuildItem::Building(B_POWR),
        }],
        "the sidebar must be able to start a new item after the cancel"
    );
    tick(&mut core);
    assert_eq!(
        core.world().house(1).unwrap().building_prod.map(|p| p.item),
        Some(BuildItem::Building(B_POWR)),
        "the new build genuinely started"
    );
}

// ===========================================================================
// P3c — mid-build hold/cancel through real InputEvents at cameo pixels:
// right-click holds, left-click resumes, right-click twice cancels with an
// exact spent-so-far refund. Proves the routing in `AppCore::handle`, not
// `cancel_production()` the method.
// ===========================================================================

#[test]
fn cameo_right_click_holds_then_cancels_mid_build_via_real_input_events() {
    let mut core = landlocked_core();
    let (x, y_powr) = cameo_xy(&core, 0);

    // Start POWR and run a few ticks of installments.
    click(&mut core, MouseButton::Left, x, y_powr);
    assert!(
        core.drain_sounds().contains(&SoundEvent::BuildingStarted),
        "EVA 'Building' (VOX_BUILDING/ABLDGIN1) acknowledges the start"
    );
    for _ in 0..5 {
        tick(&mut core);
    }
    core.drain_commands();
    let spent = core.world().house(1).unwrap().building_prod.unwrap().spent;
    assert!(spent > 0, "test setup: installments must have been paid");

    // Left-click while actively building: the scold, and no command.
    click(&mut core, MouseButton::Left, x, y_powr);
    assert!(
        core.drain_commands().is_empty(),
        "clicking an in-progress cameo must not emit a command"
    );
    assert!(
        core.drain_sounds().contains(&SoundEvent::NoFactory),
        "EVA scold (VOX_NO_FACTORY/PROGRES1) for the busy lane"
    );

    // Right-click #1: hold (the original's SUSPEND stage).
    click(&mut core, MouseButton::Right, x, y_powr);
    assert_eq!(
        core.drain_commands(),
        vec![Command::HoldProduction {
            house: 1,
            kind: ProdKind::Building,
        }]
    );
    assert!(
        core.drain_sounds().contains(&SoundEvent::OnHold),
        "EVA 'On hold' (VOX_SUSPENDED/ONHOLD1) should be queued"
    );
    tick(&mut core);
    assert!(
        core.world().house(1).unwrap().building_prod.unwrap().paused,
        "the sim lane must be suspended"
    );
    let held_item = core
        .sidebar_items()
        .into_iter()
        .find(|i| i.item == BuildItem::Building(B_POWR))
        .unwrap();
    assert!(held_item.paused, "the sidebar row must report on-hold");

    // Frozen while held.
    let frozen = core.world().house(1).unwrap().building_prod.unwrap();
    for _ in 0..20 {
        tick(&mut core);
    }
    let after = core.world().house(1).unwrap().building_prod.unwrap();
    assert_eq!(after.progress, frozen.progress, "no progress while held");
    assert_eq!(after.spent, frozen.spent, "no spending while held");

    // Left-click on the held cameo: resume (PRODUCE re-issue).
    click(&mut core, MouseButton::Left, x, y_powr);
    assert_eq!(
        core.drain_commands(),
        vec![Command::StartProduction {
            house: 1,
            item: BuildItem::Building(B_POWR),
        }],
        "left-click on a held cameo re-issues PRODUCE to resume"
    );
    assert!(
        core.drain_sounds().contains(&SoundEvent::BuildingStarted),
        "EVA 'Building' acknowledges the resume"
    );
    tick(&mut core);
    assert!(
        !core.world().house(1).unwrap().building_prod.unwrap().paused,
        "the lane must be running again"
    );

    // Right-click twice: hold, then abandon with an exact refund.
    let credits_before = core.world().house_credits(1);
    let spent_now = core.world().house(1).unwrap().building_prod.unwrap().spent;
    click(&mut core, MouseButton::Right, x, y_powr);
    tick(&mut core);
    core.drain_sounds();
    click(&mut core, MouseButton::Right, x, y_powr);
    let cmds = core.drain_commands();
    assert_eq!(
        cmds,
        vec![
            Command::HoldProduction {
                house: 1,
                kind: ProdKind::Building,
            },
            Command::CancelProduction {
                house: 1,
                kind: ProdKind::Building,
            },
        ],
        "hold then cancel, exactly the original's two-stage right-click"
    );
    assert!(
        core.drain_sounds().contains(&SoundEvent::Canceled),
        "EVA 'Canceled' on the second right-click"
    );
    tick(&mut core);
    assert!(
        core.world().house(1).unwrap().building_prod.is_none(),
        "lane cleared"
    );
    assert_eq!(
        core.world().house_credits(1),
        credits_before + spent_now,
        "cancel refunds exactly the spent-so-far amount"
    );
}
