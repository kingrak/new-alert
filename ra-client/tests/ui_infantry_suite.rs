//! Infantry UI suite (M7.6 test-plan item 6, "UI" bullet): frame-band
//! correctness for the E1/E2/E3 Do-table renderer, small-target click
//! selection tolerance, and monkey testing with infantry present.
//!
//! Fully synthetic — no real assets needed (a fake 2-frame-band `UnitSprite`
//! is explicitly endorsed by the task brief), so nothing here is skip-gated.
//! Structured in four sections mirroring the task's four coverage bullets:
//!
//! 1. Pure-function frame-band correctness for
//!    `ra_client::unit_render::infantry_frame` (idle/walk/fire distinctness,
//!    facing table, stage wraparound) — no `AppCore` needed.
//! 2. End-to-end frame-band correctness through `AppCore::compose`: a
//!    synthetic one-infantry world with a fake 2-band sprite, asserting idle
//!    and walking genuinely composite different pixels.
//! 3. Small-target click-selection hit-testing.
//!
//!    **Finding (flagged for ra-coder, not fixed here — read before
//!    extending this section).** `ra-client/src/appcore.rs` draws a smaller
//!    selection ring/health-bar for infantry than for vehicles
//!    (`marker_half = CELL_PIXELS/4` = 6px for infantry vs `CELL_PIXELS/2` =
//!    12px for vehicles, in `draw_units`), and as of **M7.7 P0d the click
//!    hit-test now matches**: both `finish_selection`'s click path and
//!    `unit_at_map` (the right-click attack-target picker) scale the pick
//!    radius via `pick_radius(is_infantry)` — `CELL_PIXELS/2` = 12px for
//!    infantry, the full `CELL_PIXELS` = 24px for vehicles/others. So a click
//!    a whole cell away from a soldier no longer grabs it; the click tolerance
//!    tracks the visible footprint. The tests below pin the post-P0d
//!    behaviour, citing the exact constants involved.
//! 4. Monkey testing (seeded proptest) over a mixed vehicle+infantry
//!    fixture: no panic, every drained command well-formed (live unit,
//!    correct owning house) — mirrors `ui_monkey.rs`'s units-variant
//!    invariant, copied here rather than imported since this file must not
//!    edit `ui_monkey.rs`.

mod support;

use proptest::prelude::*;

use ra_client::appcore::{AppCore, Frame};
use ra_client::input::{InputEvent, Key, MouseButton};
use ra_client::unit_render::{infantry_facing_index, infantry_frame, InfAction, InfantryAnim};
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{Command, Handle, MoveStats, Passability, Unit, World};

// ===========================================================================
// Section 1 — pure-function frame-band correctness (`infantry_frame`).
// No `AppCore` involved; these test `ra_client::unit_render` directly.
// ===========================================================================

/// A handful of facings cross-checked against the `HUMAN_SHAPE` table
/// transcribed in `unit_render.rs` (already verified byte-for-byte against
/// `infantry.cpp:91` per that file's doc comment — this just exercises the
/// public `infantry_facing_index` wrapper at a few representative octants,
/// including the wrap-around case near 256).
#[test]
fn infantry_facing_index_matches_human_shape_table() {
    // (facing, expected sprite-facing 0..8). Cardinal directions plus two
    // off-cardinal checks to catch an off-by-one in the Dir_To_32 shift.
    let cases: &[(u8, usize)] = &[
        (0, 0),   // due north
        (32, 7),  // NNE-ish
        (64, 6),  // due east
        (160, 3), // SSW-ish
        (128, 4), // due south
        (192, 2), // due west
        (224, 1), // NNW-ish
        (252, 0), // wraps back to north
    ];
    for &(facing, expected) in cases {
        assert_eq!(
            infantry_facing_index(Facing(facing)),
            expected,
            "facing {facing} should map to sprite-facing {expected}"
        );
    }
}

/// Idle, walking, and firing must pick genuinely different SHP frame indices
/// for the same facing — that's the entire point of the Do-table band split
/// (`infantry.cpp:524-543`).
#[test]
fn idle_walk_fire_pick_distinct_frame_indices_same_facing() {
    let anim = InfantryAnim::for_name("E1"); // 8-frame fire cycle
    let facing = Facing(0); // sprite-facing 0

    let idle = infantry_frame(&anim, facing, InfAction::Idle, 0);
    let walk = infantry_frame(&anim, facing, InfAction::Walk, 0);
    let fire = infantry_frame(&anim, facing, InfAction::Fire, 0);

    assert_eq!(idle, 0, "idle band is frame `facing` (band 0, count 1)");
    assert_eq!(walk, 16, "walk band starts at frame 16 (DO_WALK)");
    assert_eq!(fire, 64, "E1 fire band starts at frame 64 (DO_FIRE_WEAPON)");
    assert_ne!(idle, walk);
    assert_ne!(walk, fire);
    assert_ne!(idle, fire);
}

/// Facing changes the index the way `HUMAN_SHAPE` dictates: idle frame IS the
/// sprite-facing index, so due-north vs due-east idle frames must differ by
/// exactly the `HUMAN_SHAPE` delta (0 vs 6).
#[test]
fn facing_changes_idle_frame_per_human_shape() {
    let anim = InfantryAnim::for_name("E1");
    let north = infantry_frame(&anim, Facing(0), InfAction::Idle, 0);
    let east = infantry_frame(&anim, Facing(64), InfAction::Idle, 0);
    assert_eq!(north, 0);
    assert_eq!(east, 6);
    assert_ne!(north, east);
}

/// Walk stage wraps modulo 6 (`DO_WALK`'s 6-frame cycle): stage 6 must equal
/// stage 0, and the frames in between must all be distinct.
#[test]
fn walk_stage_wraps_modulo_six() {
    let anim = InfantryAnim::for_name("E1");
    let facing = Facing(0);
    let stage0 = infantry_frame(&anim, facing, InfAction::Walk, 0);
    let stage6 = infantry_frame(&anim, facing, InfAction::Walk, 6);
    assert_eq!(
        stage0, stage6,
        "stage must wrap modulo the 6-frame walk cycle"
    );

    let frames: Vec<usize> = (0..6)
        .map(|s| infantry_frame(&anim, facing, InfAction::Walk, s))
        .collect();
    let mut sorted = frames.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        frames.len(),
        "all 6 walk-cycle frames should be distinct: {frames:?}"
    );
}

/// E1/E3 share the 8-frame fire cycle; E2's grenade throw is a 20-frame
/// cycle (`idata.cpp:178/202/226`, transcribed in `InfantryAnim::for_name`).
/// At a stage beyond 8 the two cycles must diverge (E1 has already wrapped,
/// E2 has not), proving the client actually uses the per-type `fire_count`
/// rather than a hardcoded 8.
#[test]
fn e1_and_e2_fire_cycles_diverge_at_long_stage() {
    let e1 = InfantryAnim::for_name("E1");
    let e2 = InfantryAnim::for_name("E2");
    assert_eq!(e1.fire_count, 8);
    assert_eq!(e2.fire_count, 20);

    let facing = Facing(0);
    let stage = 15u32; // 15 % 8 = 7, 15 % 20 = 15 -> must differ
    let e1_frame = infantry_frame(&e1, facing, InfAction::Fire, stage);
    let e2_frame = infantry_frame(&e2, facing, InfAction::Fire, stage);
    assert_eq!(e1_frame, 64 + 7);
    assert_eq!(e2_frame, 64 + 15);
    assert_ne!(
        e1_frame, e2_frame,
        "E1's 8-frame cycle and E2's 20-frame cycle must diverge past stage 8"
    );

    // E1 wraps back to its stage-0 frame at stage 8; E2 does not (its cycle
    // is 20 long), so the two anims' "same stage" frame stays apart there too.
    let e1_s8 = infantry_frame(&e1, facing, InfAction::Fire, 8);
    let e1_s0 = infantry_frame(&e1, facing, InfAction::Fire, 0);
    assert_eq!(e1_s8, e1_s0, "E1's 8-frame fire cycle wraps at stage 8");
    let e2_s8 = infantry_frame(&e2, facing, InfAction::Fire, 8);
    let e2_s0 = infantry_frame(&e2, facing, InfAction::Fire, 0);
    assert_ne!(
        e2_s8, e2_s0,
        "E2's 20-frame fire cycle has not wrapped at stage 8"
    );
}

/// Fire-band facing stride also differs per type (`Jump`: 8 for E1/E3, 20 for
/// E2) — a non-zero facing must offset the two anims' fire frames by their
/// respective jump, not a shared constant.
#[test]
fn fire_facing_jump_differs_e1_vs_e2() {
    let e1 = InfantryAnim::for_name("E1");
    let e2 = InfantryAnim::for_name("E2");
    // Facing 64 (east) -> sprite-facing 6.
    let facing = Facing(64);
    let e1_frame = infantry_frame(&e1, facing, InfAction::Fire, 0);
    let e2_frame = infantry_frame(&e2, facing, InfAction::Fire, 0);
    assert_eq!(e1_frame, 64 + 6 * 8);
    assert_eq!(e2_frame, 64 + 6 * 20);
    assert_ne!(e1_frame, e2_frame);
}

// ===========================================================================
// Section 2 — end-to-end frame-band correctness through `AppCore::compose`.
// A synthetic one-infantry world with a fake 2-frame-band `UnitSprite`
// (idle band = one solid colour, walk band = a different solid colour),
// proving idle vs walking genuinely composite different pixels, not just
// different indices in isolation.
// ===========================================================================

/// Mirrors `AppCore`'s private `leptons_to_pixel` (`CELL_PIXELS` =
/// `ICON_WIDTH` = 24, `LEPTONS_PER_CELL` = 256) so this file can compute the
/// same screen position `draw_units`/`finish_selection` use, without that
/// helper being exported.
fn leptons_to_pixel(leptons: i32) -> i32 {
    (leptons as i64 * 24 / 256) as i32
}

/// Palette index painted into the idle band's frames (0..8, one per
/// sprite-facing) of the synthetic sprite used by [`frame_band_world`].
const IDLE_PAL_IDX: u8 = 200;
/// Palette index painted into the walk band's frames (16..64, 6 per
/// sprite-facing) of the same synthetic sprite. Both indices are well clear
/// of the `1..=16` range `support::synthetic_fixture`'s hand-built terrain
/// template actually paints, so a colour match can only come from the unit
/// sprite, never the background terrain.
const WALK_PAL_IDX: u8 = 210;

/// Build a fake 2-frame-band `UnitSprite`: frames 0..8 (the idle band, one
/// per sprite-facing) are a solid [`IDLE_PAL_IDX`] block; frames 16..64 (the
/// walk band, 6 frames per sprite-facing) are a solid [`WALK_PAL_IDX`]
/// block; everything else (the unused 8..16 gap) is transparent (index 0).
/// "Synthetic fixture with fake 2-frame bands" is explicitly endorsed by the
/// task brief for this coverage item.
fn fake_infantry_sprite() -> ra_client::unit_render::UnitSprite {
    use ra_client::unit_render::{SpriteFrame, UnitSprite};
    let solid = |idx: u8| SpriteFrame {
        width: 8,
        height: 8,
        pixels: vec![idx; 64],
    };
    let mut frames = Vec::new();
    for i in 0..64usize {
        frames.push(if i < 8 {
            solid(IDLE_PAL_IDX)
        } else if (16..64).contains(&i) {
            solid(WALK_PAL_IDX)
        } else {
            solid(0) // transparent gap, never indexed by idle/walk math
        });
    }
    UnitSprite { frames }
}

/// One infantry unit (type_id 0, house 1, facing north) in an otherwise
/// empty synthetic world, wrapped in an `AppCore` with `fake_infantry_sprite`
/// installed for type_id 0 and E1's `InfantryAnim` layout.
fn frame_band_core() -> (AppCore, Handle) {
    let (raster, palette) = support::synthetic_fixture();
    let mut world = World::new(Passability::all_passable(), 0xF1A3_B00D);
    let stats = MoveStats {
        max_speed: 20,
        rot: 8,
    };
    let handle = world.spawn_unit(0, 1, CellCoord::new(10, 10), Facing(0), 50, stats);
    world.set_unit_max_health(handle, 50);
    world.set_unit_combat(handle, 0, None, false);
    world
        .units
        .get_mut(handle)
        .expect("just-spawned unit is live")
        .make_infantry(0);

    let mut core = AppCore::with_sim(
        raster.clone(),
        *palette,
        world,
        vec![fake_infantry_sprite()],
        Vec::new(),
    );
    core.set_infantry_anim(vec![Some(InfantryAnim::for_name("E1"))]);
    (core, handle)
}

/// Count pixels in `frame` matching palette index `idx`'s colour exactly.
fn count_pal_pixels(frame: &Frame, palette: &ra_client::compositor::Palette, idx: u8) -> usize {
    let [r, g, b] = palette[idx as usize];
    frame
        .pixels
        .chunks_exact(4)
        .filter(|px| px[0] == r && px[1] == g && px[2] == b)
        .count()
}

/// A generous viewport (map-pixel space) centred on the unit's spawn
/// position, wide enough to still contain it after a few ticks of southward
/// drift toward a far-away destination.
fn frame_band_viewport(core: &AppCore, handle: Handle) -> ra_client::input::Rect {
    let coord = core.world().units.get(handle).unwrap().coord;
    let px = leptons_to_pixel(coord.x.0);
    let py = leptons_to_pixel(coord.y.0);
    ra_client::input::Rect {
        x: (px - 64).max(0) as i64,
        y: (py - 64).max(0) as i64,
        width: 160,
        height: 220,
    }
}

#[test]
fn idle_infantry_composes_the_idle_band_colour_only() {
    let (core, handle) = frame_band_core();
    let (_, palette) = support::synthetic_fixture();
    let viewport = frame_band_viewport(&core, handle);

    assert!(
        !core.world().units.get(handle).unwrap().is_moving(),
        "freshly spawned unit should be idle"
    );
    let frame = core.compose(viewport);
    assert!(
        count_pal_pixels(&frame, palette, IDLE_PAL_IDX) > 0,
        "idle compose should paint at least one idle-band pixel"
    );
    assert_eq!(
        count_pal_pixels(&frame, palette, WALK_PAL_IDX),
        0,
        "idle compose must not paint any walk-band pixel"
    );
}

#[test]
fn walking_infantry_composes_a_genuinely_different_frame_than_idle() {
    let (mut core, handle) = frame_band_core();
    let (_, palette) = support::synthetic_fixture();
    let viewport = frame_band_viewport(&core, handle);

    let idle_frame = core.compose(viewport);

    // Order a long move so the unit is still mid-path (not yet arrived)
    // after a handful of ticks.
    core.inject_command(Command::Move {
        unit: handle,
        dest: CellCoord::new(10, 70),
        house: 1,
    });
    let mut ticks = 0;
    loop {
        core.update(67); // ~1 tick (TICKS_PER_SECOND = 15)
        ticks += 1;
        if core.world().units.get(handle).unwrap().is_moving() {
            break;
        }
        assert!(
            ticks < 30,
            "unit never entered is_moving() after a Move order; pathfinding/command \
             pipeline may be broken"
        );
    }

    let walk_frame = core.compose(viewport);
    assert_ne!(
        idle_frame.pixels, walk_frame.pixels,
        "idle and walking composed frames must differ pixel-for-pixel"
    );
    assert_eq!(
        count_pal_pixels(&idle_frame, palette, IDLE_PAL_IDX),
        count_pal_pixels(&idle_frame, palette, IDLE_PAL_IDX).max(1),
    );
    assert!(
        count_pal_pixels(&walk_frame, palette, WALK_PAL_IDX) > 0,
        "walking compose should paint at least one walk-band pixel"
    );
    assert_eq!(
        count_pal_pixels(&walk_frame, palette, IDLE_PAL_IDX),
        0,
        "walking compose must not paint the idle-band colour"
    );
}

// ===========================================================================
// Section 3 — small-target click-selection hit-testing.
// See the module doc's "Finding" above: the click hit-test uses a single
// `PICK_RADIUS = CELL_PIXELS` (24px) constant regardless of `is_infantry`,
// even though the *visual* selection marker is smaller for infantry
// (`marker_half` 6px vs 12px). The tests below pin the actual behaviour.
// ===========================================================================

/// `PICK_RADIUS` from `ra-client/src/appcore.rs` (`const PICK_RADIUS: i32 =
/// CELL_PIXELS;`, `CELL_PIXELS = ICON_WIDTH = 24`). Not exported, so
/// transcribed here with its source cited — if `appcore.rs` ever changes this
/// constant, this suite's distance-24/30 assertions below should be revisited
/// alongside it.
const PICK_RADIUS: i32 = 24;
/// Infantry visual selection-marker half-width (`marker_half = CELL_PIXELS /
/// 4` in `draw_units`'s `is_inf` branch) — for documentation/contrast only;
/// it does **not** gate the click hit-test (see the Finding above).
const INFANTRY_MARKER_HALF: i32 = 6;
/// Vehicle visual selection-marker half-width (`marker_half = CELL_PIXELS /
/// 2`) — likewise for contrast only.
const VEHICLE_MARKER_HALF: i32 = 12;

fn selection_fixture() -> (World, Handle, Handle) {
    let mut world = World::new(Passability::all_passable(), 0x005E_1EC7);
    let stationary = MoveStats {
        max_speed: 0,
        rot: 0,
    };

    // Infantry at cell (10,10), sub-cell spot 1 (NW quadrant) rather than the
    // cell centre, so it sits at a genuinely different pixel than a
    // cell-centred vehicle would.
    let inf = world.spawn_unit(0, 1, CellCoord::new(10, 10), Facing(0), 50, stationary);
    world.set_unit_max_health(inf, 50);
    world
        .units
        .get_mut(inf)
        .expect("just-spawned unit is live")
        .make_infantry(1);

    // Vehicle at cell (14,10) centre — far enough from the infantry (~100px)
    // that clicks near one never accidentally land within PICK_RADIUS of the
    // other.
    let veh = world.spawn_unit(1, 1, CellCoord::new(14, 10), Facing(0), 100, stationary);
    world.set_unit_max_health(veh, 100);

    (world, inf, veh)
}

fn selection_core() -> (AppCore, Handle, Handle) {
    let (raster, palette) = support::synthetic_fixture();
    let (world, inf, veh) = selection_fixture();
    let core = AppCore::with_sim(raster.clone(), *palette, world, Vec::new(), Vec::new());
    (core, inf, veh)
}

/// The unit's true on-screen pixel position (camera at the default origin,
/// so map pixels == viewport pixels).
fn unit_screen_pos(core: &AppCore, handle: Handle) -> (i32, i32) {
    let coord = core.world().units.get(handle).unwrap().coord;
    (leptons_to_pixel(coord.x.0), leptons_to_pixel(coord.y.0))
}

/// A "click" (as opposed to a drag/box-select): `MouseDown` then `MouseUp` at
/// the identical point, well within `finish_selection`'s `CLICK_SLOP`.
fn click(core: &mut AppCore, x: i32, y: i32) {
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x,
        y,
    });
    core.handle(InputEvent::MouseUp {
        button: MouseButton::Left,
        x,
        y,
    });
}

#[test]
fn click_exactly_on_infantry_true_position_selects_it() {
    let (mut core, inf, _veh) = selection_core();
    let (x, y) = unit_screen_pos(&core, inf);
    click(&mut core, x, y);
    assert_eq!(core.selected_handles(), vec![inf]);
}

#[test]
fn click_beyond_pick_radius_selects_nothing() {
    let (mut core, inf, _veh) = selection_core();
    let (x, y) = unit_screen_pos(&core, inf);
    // Distance 30 > PICK_RADIUS (24): must not select.
    click(&mut core, x + PICK_RADIUS + 6, y);
    assert!(
        core.selected_handles().is_empty(),
        "a click 6px beyond PICK_RADIUS must not select the infantry"
    );
}

/// **M7.7 P0d: infantry click tolerance now tracks the visual footprint.** A
/// click at distance 20px from the infantry's true position — outside the new
/// infantry pick radius (`CELL_PIXELS/2` = 12px) though still within the
/// full-cell `PICK_RADIUS` (24px) — no longer selects the infantryman. (Before
/// P0d a full-cell hitbox let a click a whole cell away grab a soldier; that
/// old behaviour is what this test used to pin, now flipped.)
#[test]
fn click_beyond_infantry_pick_radius_but_within_full_radius_no_longer_selects() {
    let (mut core, inf, _veh) = selection_core();
    let (x, y) = unit_screen_pos(&core, inf);
    let distance = 20;
    assert!(distance > INFANTRY_MARKER_HALF);
    assert!(distance > VEHICLE_MARKER_HALF);
    assert!(distance <= PICK_RADIUS);
    assert!(distance > PICK_RADIUS / 2); // beyond the halved infantry radius

    click(&mut core, x + distance, y);
    assert!(
        core.selected_handles().is_empty(),
        "P0d: an infantry click hitbox is now CELL_PIXELS/2 (12px), so a 20px-away \
         click must not select the soldier"
    );
}

/// Companion to the above: a click *within* the halved infantry radius (12px)
/// still selects — the tolerance shrank, it did not vanish.
#[test]
fn click_within_infantry_pick_radius_still_selects() {
    let (mut core, inf, _veh) = selection_core();
    let (x, y) = unit_screen_pos(&core, inf);
    let distance = PICK_RADIUS / 2 - 2; // 10px, inside the 12px infantry radius
    click(&mut core, x + distance, y);
    assert_eq!(core.selected_handles(), vec![inf]);
}

/// Same shared-tolerance behaviour holds symmetrically for a vehicle: a
/// click 20px away (outside its own 12px marker, but within PICK_RADIUS)
/// also selects it. Included to show the mismatch is not infantry-specific
/// special-casing gone wrong — it is simply the *absence* of any
/// `is_infantry` branch in the click hit-test at all.
#[test]
fn click_within_pick_radius_but_outside_vehicle_visual_marker_still_selects_vehicle() {
    let (mut core, _inf, veh) = selection_core();
    let (x, y) = unit_screen_pos(&core, veh);
    let distance = 20;
    assert!(distance > VEHICLE_MARKER_HALF);
    assert!(distance <= PICK_RADIUS);

    click(&mut core, x + distance, y);
    assert_eq!(core.selected_handles(), vec![veh]);
}

#[test]
fn click_beyond_pick_radius_selects_nothing_for_vehicle_either() {
    let (mut core, _inf, veh) = selection_core();
    let (x, y) = unit_screen_pos(&core, veh);
    click(&mut core, x + PICK_RADIUS + 6, y);
    assert!(core.selected_handles().is_empty());
}

/// A tiny 2px drag (within `CLICK_SLOP` = 3) is still treated as a click, not
/// a box-select — same PICK_RADIUS-gated behaviour as an exact click.
#[test]
fn tiny_drag_within_click_slop_is_still_treated_as_a_click() {
    let (mut core, inf, _veh) = selection_core();
    let (x, y) = unit_screen_pos(&core, inf);
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x,
        y,
    });
    core.handle(InputEvent::MouseUp {
        button: MouseButton::Left,
        x: x + 2,
        y: y + 2,
    });
    assert_eq!(core.selected_handles(), vec![inf]);
}

/// Box-select (drag) has no radius/marker distinction at all — any unit
/// whose exact pixel position falls inside the dragged rectangle is
/// selected, infantry and vehicle alike.
#[test]
fn box_select_drag_includes_infantry_and_vehicle_alike() {
    let (mut core, inf, veh) = selection_core();
    let (ix, iy) = unit_screen_pos(&core, inf);
    let (vx, vy) = unit_screen_pos(&core, veh);
    let (x0, y0) = (ix.min(vx) - 10, iy.min(vy) - 10);
    let (x1, y1) = (ix.max(vx) + 10, iy.max(vy) + 10);

    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x: x0,
        y: y0,
    });
    core.handle(InputEvent::MouseUp {
        button: MouseButton::Left,
        x: x1,
        y: y1,
    });
    let mut selected = core.selected_handles();
    selected.sort_by_key(|h| h.index);
    let mut expected = vec![inf, veh];
    expected.sort_by_key(|h| h.index);
    assert_eq!(selected, expected);
}

// ===========================================================================
// Section 4 — monkey testing with infantry present (DESIGN.md §4.8 layer 3).
// Self-contained copy of `ui_monkey.rs`'s units-variant strategy/invariant
// (that file is off-limits to edit for this suite), run over a fixture that
// mixes vehicles AND infantry so infantry-specific click/selection/move code
// paths are exercised by fuzzed sequencing, not just the hand-picked cases
// in Section 3.
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

/// Same sampling cadence as `ui_monkey.rs`'s `COMPOSE_EVERY`: compose is
/// O(viewport area), so it is sampled rather than run after every op.
const COMPOSE_EVERY: usize = 8;

/// Build a world mixing vehicles and infantry: two house-1 vehicles, two
/// house-1 infantry (distinct sub-cell spots so they don't stack), and one
/// house-2 witness vehicle well away from the others.
fn mixed_world(seed: u32) -> World {
    let mut world = World::new(Passability::all_passable(), seed);
    let veh_stats = MoveStats {
        max_speed: 25,
        rot: 10,
    };
    let inf_stats = MoveStats {
        max_speed: 14,
        rot: 5,
    };
    for i in 0..2i32 {
        world.spawn_unit(
            0,
            1,
            CellCoord::new(10 + i * 2, 10),
            Facing(0),
            256,
            veh_stats,
        );
    }
    for i in 0..2u8 {
        let h = world.spawn_unit(1, 1, CellCoord::new(16, 10), Facing(0), 50, inf_stats);
        world.set_unit_max_health(h, 50);
        let unit: &mut Unit = world.units.get_mut(h).expect("just-spawned unit is live");
        unit.make_infantry(i + 1); // spots 1, 2: distinct sub-cells
    }
    world.spawn_unit(0, 2, CellCoord::new(60, 60), Facing(0), 256, veh_stats);
    world
}

fn mixed_core(seed: u32) -> AppCore {
    let (raster, palette) = support::synthetic_fixture();
    let world = mixed_world(seed);
    AppCore::with_sim(raster.clone(), *palette, world, Vec::new(), Vec::new())
}

/// Apply one op and check the invariant: no panic (implicit — a panic aborts
/// the test), and every command drained after this op addresses a still-live
/// unit whose `house` matches that unit's real owner. Identical logic to
/// `ui_monkey.rs`'s `apply_op_with_units`, copied rather than imported.
fn apply_op_with_mixed_units(core: &mut AppCore, op: MonkeyOp, index: usize) {
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
                passenger, house, ..
            } => (passenger, house),
            Command::Unload { transport, house } => (transport, house),
            Command::StartProduction { .. }
            | Command::PlaceBuilding { .. }
            | Command::CancelProduction { .. }
            | Command::HoldProduction { .. }
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

fn apply_ops_with_mixed_units(core: &mut AppCore, ops: &[MonkeyOp]) {
    core.handle(InputEvent::Resize {
        width: 64,
        height: 48,
    });
    for (i, &op) in ops.iter().enumerate() {
        apply_op_with_mixed_units(core, op, i);
    }
}

proptest! {
    // Same case count as `ui_monkey.rs` (64) — matching precedent rather
    // than inventing a larger number that would slow CI.
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Fuzzed event/update sequences against a mixed vehicle+infantry
    /// fixture: no panic, and every drained command is well-formed
    /// (addresses a live unit, house matches the real owner) regardless of
    /// whether the sequence happens to select/move/order infantry, a
    /// vehicle, or both.
    #[test]
    fn monkey_with_infantry_never_panics_or_emits_bad_commands(ops in ops_strategy(200..800)) {
        let mut core = mixed_core(0x1F5A_717A);
        apply_ops_with_mixed_units(&mut core, &ops);
    }
}
