//! M7.7 Chunk C: DOME radar gating across power-state **transitions**
//! (`ra-tester` charter). `AppCore::has_radar()` (`appcore.rs:1703-1732`) gates
//! the sidebar radar minimap on owning a **live, powered** radar dome (DOME)
//! when the catalog models one at all — this landed in Chunk C with only a
//! start/end-state smoke check in the `ra-client` verification binary
//! (`cmd_verify_m77c` in `src/bin/ra-client.rs`), never as an automated test.
//! This file drives every transition explicitly: no DOME -> DOME built +
//! powered -> power cut -> power restored -> DOME sold, asserting the
//! radar-panel-presence signal at **each** step, not just first/last.
//!
//! Deliberately a separate file from `ui_radar_cameo_f1_suite.rs`: that
//! suite's fixtures (`support::synthetic_core_with_econ_radar_cameo` et al.)
//! use a catalog with **no** `DOME` building at all, which per `has_radar()`'s
//! own documented fallback ("a catalog with no DOME concept modeled keeps the
//! radar always-on") makes them structurally unable to exercise this gate —
//! this file needs its own catalog that actually models a DOME.
//!
//! Mirrors that suite's established black-box conventions (geometry constants
//! duplicated independently from `appcore.rs`'s private layout rather than
//! imported, per that file's own stated rationale: a test failure here should
//! mean the *observable* behavior changed, not that two copies of a formula
//! drifted apart) for the compose/click proxy checks, while using the public
//! `AppCore::has_radar()` query as the primary oracle at every step.

mod support;

use ra_client::appcore::AppCore;
use ra_client::input::{InputEvent, MouseButton};
use ra_sim::coords::CellCoord;
use ra_sim::{BuildingProto, Catalog, Command, EconRules, Passability, World};

// Building type ids for this file's minimal catalog.
const B_POWR: u32 = 0; // power plant, 2x2, +100 output, cost 30
const B_DOME: u32 = 1; // radar dome, 2x2, -40 drain (consumes power), cost 100

/// Radar minimap panel side length, matching `appcore`'s private `RADAR_SIZE`
/// (also independently duplicated by `ui_radar_cameo_f1_suite.rs`).
const RADAR_SIZE: i32 = 120;

fn bproto(name: &str, power: i32, cost: i32) -> BuildingProto {
    BuildingProto {
        is_barracks: false,
        name: name.to_string(),
        foot_w: 2,
        foot_h: 2,
        max_health: 400,
        armor: 0,
        power,
        cost,
        prereq: vec![],
        is_refinery: false,
        is_construction_yard: false,
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

/// A radar-enabled, sidebar-enabled core over a catalog that actually models
/// a DOME (ids fixed: `B_POWR=0`, `B_DOME=1`), with no buildings placed yet.
fn dome_fixture(seed: u32) -> AppCore {
    let (raster, palette) = support::synthetic_fixture();
    let mut world = World::new(Passability::all_passable(), seed);
    world.set_catalog(Catalog {
        buildings: vec![bproto("POWR", 100, 30), bproto("DOME", -40, 100)],
        units: vec![],
        econ: EconRules::default(),
    });
    world.init_houses(3, 1000);

    let mut core = AppCore::with_sim(raster.clone(), *palette, world, Vec::new(), Vec::new());
    core.enable_sidebar(1, Vec::new());
    core.enable_radar();
    core.handle(InputEvent::Resize {
        width: 900,
        height: 600,
    });
    core
}

/// `appcore::AppCore::sidebar_header_h()`, replicated black-box (see this
/// file's module doc for why it's duplicated rather than imported).
fn sidebar_header_h() -> i32 {
    2 + (ra_client::font::GLYPH_H + 2) + ra_client::font::GLYPH_H + 4
}

/// The radar panel's viewport rectangle, IF it were present — independent of
/// whatever `has_radar()` currently says, so it can be used as a fixed click
/// target across every transition in this file.
fn radar_rect(core: &AppCore) -> (i32, i32, i32) {
    (
        core.tactical_width() as i32 + 2,
        sidebar_header_h(),
        RADAR_SIZE,
    )
}

/// Behavioral proxy for "is the radar panel actually present, as observed
/// through user interaction": click inside where the panel would be and see
/// whether the camera jumps. Complements the direct `has_radar()` assertion
/// with an end-to-end check that the internal flag has a real, observable
/// consequence at this exact transition.
fn radar_click_moves_camera(core: &mut AppCore) -> bool {
    let (rx, ry, _) = radar_rect(core);
    core.set_camera(555.0, 555.0);
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x: rx + 10,
        y: ry + 10,
    });
    let r = core.camera_rect();
    core.drain_commands(); // a radar click never emits a sim command either way
    (r.x, r.y) != (555, 555)
}

/// The full transition sequence: no DOME -> DOME built + powered -> power
/// cut -> power restored -> DOME sold, asserting `has_radar()` (the direct
/// public oracle) and the click-behavior proxy at **every** step.
#[test]
fn radar_panel_flips_correctly_across_every_dome_power_transition() {
    let mut core = dome_fixture(0xD0E5_0001);
    let house = 1u8;

    // --- 1. No DOME yet: radar absent, even with the sidebar radar enabled. ---
    assert!(!core.has_radar(), "no DOME owned yet: radar must be absent");
    assert!(
        !radar_click_moves_camera(&mut core),
        "a click where the panel would be must not jump the camera when absent"
    );

    // --- 2. Build a POWR (for power) and a DOME: radar becomes present. ---
    core.world_mut()
        .spawn_building(B_POWR, house, CellCoord::new(10, 10));
    let dome = core
        .world_mut()
        .spawn_building(B_DOME, house, CellCoord::new(14, 10))
        .expect("DOME should place");
    core.update(67);
    assert!(
        !core.world().house(house).unwrap().low_power(),
        "test setup: 100 output vs 40 drain should not be low-power"
    );
    assert!(
        core.has_radar(),
        "a live, powered DOME should turn the radar panel on"
    );
    assert!(
        radar_click_moves_camera(&mut core),
        "with the panel present, a click inside it must jump the camera"
    );

    // --- 3. Cut power (simulate the power plant being knocked out): radar
    // flips back off even though the DOME itself is still alive. ---
    let output = core.world().house(house).unwrap().power_output;
    core.world_mut().houses[house as usize].power_drain = output + 500;
    core.update(67);
    assert!(
        core.world().house(house).unwrap().low_power(),
        "test setup: drain now far exceeds output"
    );
    assert!(
        !core.has_radar(),
        "a DOME with insufficient power must NOT keep the radar panel active"
    );
    assert!(
        !radar_click_moves_camera(&mut core),
        "with the panel absent again (power cut), a click must not jump the camera"
    );

    // --- 4. Restore power: radar flips back on (not a one-way/sticky gate). ---
    core.world_mut().houses[house as usize].power_drain = 40;
    core.update(67);
    assert!(!core.world().house(house).unwrap().low_power());
    assert!(
        core.has_radar(),
        "restoring power should re-activate the radar panel, not leave it stuck off"
    );
    assert!(radar_click_moves_camera(&mut core));

    // --- 5. Sell the DOME: radar absent again, even though power is fine. ---
    core.inject_command(Command::Sell {
        house,
        building: dome,
    });
    core.update(67);
    assert!(
        !core.world().buildings.contains(dome),
        "test setup: the DOME should actually be sold"
    );
    assert!(
        !core.world().house(house).unwrap().low_power(),
        "test setup: power is still fine post-sale (only the DOME's drain is gone)"
    );
    assert!(
        !core.has_radar(),
        "selling the only DOME must turn the radar panel back off"
    );
    assert!(
        !radar_click_moves_camera(&mut core),
        "with the panel absent (DOME sold), a click must not jump the camera"
    );
}

/// A house that never enables the sidebar radar at all (`enable_radar()`
/// never called) stays radar-absent regardless of DOME/power state — the
/// `radar_enabled` flag gates everything else (`appcore.rs:1704-1706`).
/// Isolated from the main transition test so a regression in this
/// independent gate isn't masked by that test's later assertions.
#[test]
fn radar_stays_absent_without_enable_radar_even_with_a_powered_dome() {
    let (raster, palette) = support::synthetic_fixture();
    let mut world = World::new(Passability::all_passable(), 0xD0E5_0002);
    world.set_catalog(Catalog {
        buildings: vec![bproto("POWR", 100, 30), bproto("DOME", -40, 100)],
        units: vec![],
        econ: EconRules::default(),
    });
    world.init_houses(3, 1000);
    world.spawn_building(B_POWR, 1, CellCoord::new(10, 10));
    world.spawn_building(B_DOME, 1, CellCoord::new(14, 10));

    let mut core = AppCore::with_sim(raster.clone(), *palette, world, Vec::new(), Vec::new());
    core.enable_sidebar(1, Vec::new());
    // Deliberately no `core.enable_radar()`.
    core.handle(InputEvent::Resize {
        width: 900,
        height: 600,
    });

    assert!(
        !core.has_radar(),
        "PIN: a live, powered DOME alone is not enough — the sidebar radar \
         must also be explicitly enabled"
    );
}
