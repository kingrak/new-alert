//! Golden-frame UI tests (DESIGN.md §4.8 layer 4): pinned `compose()` RGBA
//! hashes for fixed scenario+viewport combos. Compositing is integer-only
//! (palette-index lookup + copy), so these are tolerance-free regression
//! pins, not approximate-match tests — any change in the pinned hash means
//! either a real compositing bug or a deliberate rendering change that
//! should update the pin with a comment explaining why.
//!
//! Real-scenario goldens skip cleanly when the assets are absent (same
//! policy as `ra-formats`/`ra-data`'s golden tests). The synthetic-map
//! goldens have no such dependency and always run — they don't validate
//! anything about the real game's art, only that `AppCore`/`compositor`
//! keep producing byte-identical output for a fixed hand-built input.

mod support;

use ra_client::input::Rect;

#[test]
fn synthetic_full_map_frame_hash() {
    let core = support::synthetic_core();
    let frame = core.compose(Rect {
        x: 0,
        y: 0,
        width: 96,
        height: 96,
    });
    assert_eq!(frame.width, 96);
    assert_eq!(frame.height, 96);
    // Regression pin: the synthetic fixture's 4x4-icon repeating pattern
    // covers exactly one period in a 96x96 (4 cells * 24px) rect starting at
    // the origin, so this hash also implicitly locks in the hand-built
    // template's pixel values and the `Clear_Icon` scramble formula.
    assert_eq!(
        support::fnv1a(&frame.pixels),
        0xabb4_98b7_e364_af25,
        "synthetic full-pattern-period frame hash changed"
    );
}

#[test]
fn synthetic_offset_viewport_frame_hash() {
    let core = support::synthetic_core();
    // An off-origin, non-cell-aligned viewport spanning a partial pattern
    // period on each axis, so it also exercises the mid-tile clipping path.
    let frame = core.compose(Rect {
        x: 10,
        y: 37,
        width: 200,
        height: 150,
    });
    assert_eq!(frame.width, 200);
    assert_eq!(frame.height, 150);
    assert_eq!(
        support::fnv1a(&frame.pixels),
        0x6409_b140_f465_6405,
        "synthetic offset-viewport frame hash changed"
    );
}

#[test]
fn synthetic_camera_default_frame_hash() {
    // Through AppCore's camera path (compose_camera at the default 640x400
    // viewport, origin position) rather than a raw compose(Rect) — pins the
    // exact combination the macroquad shell actually uses on startup.
    let core = support::synthetic_core();
    let frame = core.compose_camera();
    assert_eq!(frame.width, 640);
    assert_eq!(frame.height, 400);
    assert_eq!(
        support::fnv1a(&frame.pixels),
        0x8b67_fc8a_9889_7425,
        "synthetic default-camera frame hash changed"
    );
}

#[test]
fn scg01ea_origin_default_viewport_frame_hash() {
    let Some(core) = support::load_real_core() else {
        return;
    };
    let frame = core.compose(Rect {
        x: 0,
        y: 0,
        width: 640,
        height: 400,
    });
    assert_eq!(frame.width, 640);
    assert_eq!(frame.height, 400);
    // Pinned from the current decoder+compositor output (regression pin,
    // derived once via a throwaway probe against the real asset — same
    // caveat as every other golden hash in this repo: not independently
    // re-verified against a second implementation).
    assert_eq!(
        support::fnv1a(&frame.pixels),
        0xee0b_68eb_8edb_b18d,
        "scg01ea origin/640x400 frame hash changed"
    );
}

#[test]
fn scg01ea_playable_rect_frame_hash() {
    let Some(core) = support::load_real_core() else {
        return;
    };
    // The exact playable rect (DESIGN.md M2 reference scenario notes):
    // x=49 y=45 w=30 h=36 cells * 24px.
    let frame = core.compose(Rect {
        x: 49 * 24,
        y: 45 * 24,
        width: 30 * 24,
        height: 36 * 24,
    });
    assert_eq!(frame.width, 720);
    assert_eq!(frame.height, 864);
    assert_eq!(
        support::fnv1a(&frame.pixels),
        0xed75_807c_008f_a315,
        "scg01ea playable-rect frame hash changed"
    );
}

#[test]
fn scg01ea_tiny_viewport_at_playable_origin_frame_hash() {
    let Some(core) = support::load_real_core() else {
        return;
    };
    let frame = core.compose(Rect {
        x: 49 * 24,
        y: 45 * 24,
        width: 8,
        height: 8,
    });
    assert_eq!(frame.width, 8);
    assert_eq!(frame.height, 8);
    assert_eq!(
        support::fnv1a(&frame.pixels),
        0xb282_4bf1_9d14_2dae,
        "scg01ea tiny-viewport frame hash changed"
    );
}

#[test]
fn scg01ea_frame_is_stable_across_repeat_compose() {
    let Some(core) = support::load_real_core() else {
        return;
    };
    let rect = Rect {
        x: 49 * 24,
        y: 45 * 24,
        width: 640,
        height: 480,
    };
    let a = core.compose(rect);
    let b = core.compose(rect);
    assert_eq!(a.pixels, b.pixels, "compose() is not deterministic");
}
