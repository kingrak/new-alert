//! Shared test support for `ra-client`'s UI test suites (DESIGN.md §4.8).
//! Not a test binary itself (`tests/support/mod.rs`, not `tests/support.rs`)
//! — Cargo only auto-discovers direct children of `tests/` as test targets,
//! so each suite pulls this in with `mod support;`.
//!
//! Each `tests/*.rs` file is compiled as its own separate crate, and not
//! every suite uses every helper here — `#![allow(dead_code)]` avoids a
//! per-binary "unused function" warning for the ones a given suite doesn't
//! need (which `-D warnings` would otherwise turn into a build failure).
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::OnceLock;

use ra_client::appcore::AppCore;
use ra_client::assets::{self, LoadedGame};
use ra_client::compositor::{IndexedImage, Palette};
use ra_client::input::{InputEvent, MouseButton};
use ra_client::terrain::{self, TileSet};
use ra_data::scenario::{MapCell, Scenario, Theater, MAP_CELL_H, MAP_CELL_W};
use ra_data::templates;
use ra_formats::tmpl::{Template, ICON_WIDTH};
use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{Command, Handle, MoveStats, World};

/// Tiny dependency-free FNV-1a 64-bit hash — same algorithm used by the
/// `ra-formats` and `ra-data` golden tests, reimplemented here rather than
/// shared across crates (it's a test-only utility, not worth a dependency).
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

/// Resolve the assets directory: `RA_ASSETS_DIR` env var if set, else
/// `<crate>/../assets`.
pub fn assets_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("RA_ASSETS_DIR") {
        PathBuf::from(dir)
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../assets")
    }
}

/// Whether both real archives needed to load a scenario are present.
pub fn real_assets_available() -> bool {
    let dir = assets_dir();
    dir.join("main.mix").is_file() && dir.join("redalert.mix").is_file()
}

/// Load the M2 reference scenario (`scg01ea.ini`, Snow theater) into an
/// `AppCore` from the real assets, or print a skip notice and return `None`.
pub fn load_real_core() -> Option<AppCore> {
    if !real_assets_available() {
        eprintln!(
            "SKIP: real assets not found under {} (set RA_ASSETS_DIR or copy \
             main.mix/redalert.mix into assets/ to run this test)",
            assets_dir().display()
        );
        return None;
    }
    let loaded = assets::load_from_dir(&assets_dir(), "scg01ea.ini")
        .expect("scg01ea.ini should load from the real assets");
    Some(loaded.into_appcore())
}

/// Build a hand-rolled RA-layout template file (see `ra_formats::tmpl`
/// module docs for the header layout) with `count` icons, each a solid
/// `ICON_WIDTH`x`ICON_HEIGHT` block of a distinct, deterministic palette
/// index (`1..=count`, so index 0 never appears from a drawn tile — makes
/// "was this cell actually drawn" trivial to assert on). All icons are
/// opaque (no transparent index-0 punch-through), and the icon-number to
/// image-index map is the identity, i.e. no deduplication.
fn build_hand_template(count: u16) -> Vec<u8> {
    use ra_formats::tmpl::{ICON_HEIGHT, ICON_WIDTH};
    let icon_size = ICON_WIDTH * ICON_HEIGHT;

    let header_len = 0x28u32; // RA layout: Map field ends at 0x28.
    let icons_off = header_len;
    let icons_len = icon_size as u32 * count as u32;
    let map_off = icons_off + icons_len;
    let map_len = count as u32;
    let trans_off = map_off + map_len;
    let trans_len = count as u32;
    let colormap_off = trans_off + trans_len;
    let colormap_len = count as u32;

    let mut out = Vec::new();
    out.extend_from_slice(&(ICON_WIDTH as u16).to_le_bytes()); // 0x00 width
    out.extend_from_slice(&(ICON_HEIGHT as u16).to_le_bytes()); // 0x02 height
    out.extend_from_slice(&count.to_le_bytes()); // 0x04 count
    out.extend_from_slice(&0u16.to_le_bytes()); // 0x06 Allocated
    out.extend_from_slice(&1u16.to_le_bytes()); // 0x08 MapWidth
    out.extend_from_slice(&1u16.to_le_bytes()); // 0x0A MapHeight
    out.extend_from_slice(&(colormap_off + colormap_len).to_le_bytes()); // 0x0C Size (RA disc.)
    out.extend_from_slice(&icons_off.to_le_bytes()); // 0x10 Icons
    out.extend_from_slice(&0u32.to_le_bytes()); // 0x14 Palettes
    out.extend_from_slice(&0u32.to_le_bytes()); // 0x18 Remaps
    out.extend_from_slice(&trans_off.to_le_bytes()); // 0x1C TransFlag
    out.extend_from_slice(&colormap_off.to_le_bytes()); // 0x20 ColorMap
    out.extend_from_slice(&map_off.to_le_bytes()); // 0x24 Map
    assert_eq!(out.len() as u32, header_len);

    for i in 0..count {
        let fill = (i as u8).wrapping_add(1); // 1..=count, never 0
        out.extend(std::iter::repeat_n(fill, icon_size));
    }
    for i in 0..count {
        out.push(i as u8); // identity map: icon i -> image i
    }
    out.extend(std::iter::repeat_n(0u8, count as usize)); // trans flags: all opaque
    out.extend(std::iter::repeat_n(0u8, count as usize)); // colormap: unused by tests

    out
}

/// A distinguishable, deterministic 256-entry palette (not the flat test
/// palettes used by `compositor`'s own unit tests, so sweep/monkey output
/// visibly varies pixel-to-pixel, catching index-mixups a flat palette
/// couldn't).
fn synthetic_palette() -> Palette {
    let mut pal = [[0u8; 3]; 256];
    for (i, entry) in pal.iter_mut().enumerate() {
        *entry = [i as u8, 255u8.wrapping_sub(i as u8), 128];
    }
    pal
}

/// Memoized [`synthetic_raster_and_palette`]: rasterizing walks all 16384
/// cells through a hashmap lookup, which is cheap once but adds up when a
/// test needs a fresh core per proptest case (dozens to hundreds of times).
/// Callers that need their own mutable `AppCore` should clone the raster
/// (`IndexedImage` is a plain `Vec<u8>` wrapper — cloning is a memcpy, far
/// cheaper than re-rasterizing) rather than calling the unmemoized builder
/// repeatedly.
pub fn synthetic_fixture() -> &'static (IndexedImage, Palette) {
    static FIXTURE: OnceLock<(IndexedImage, Palette)> = OnceLock::new();
    FIXTURE.get_or_init(synthetic_raster_and_palette)
}

/// Build a synthetic-map `AppCore` that needs no real assets: a hand-built
/// 16-icon RA-layout `Template` (see [`build_hand_template`]) installed as
/// the `CLEAR1` template, applied via the real `terrain::rasterize` pipeline
/// over an all-"clear" 128x128 `Scenario`. Because clear cells resolve
/// through `Clear_Icon`'s `(x&3) | ((y&3)<<2)` scramble, this produces a
/// full 3072x3072 raster tiled with a deterministic 4x4-icon repeating
/// pattern — a genuinely hand-built map + hand-built template exercised
/// through the same `Scenario` -> `TileSet` -> `rasterize` -> `AppCore` path
/// the real asset loader uses, just with synthetic inputs so it always runs.
pub fn synthetic_core() -> AppCore {
    let (raster, palette) = synthetic_fixture();
    AppCore::new(raster.clone(), *palette)
}

/// The raw pieces behind [`synthetic_core`], exposed separately so tests can
/// also sanity-check the rasterized image directly (not just through
/// `AppCore`).
pub fn synthetic_raster_and_palette() -> (IndexedImage, Palette) {
    let template_bytes = build_hand_template(16);
    let template = Template::parse(&template_bytes).expect("hand-built template should parse");

    let mut tiles = TileSet::new();
    tiles.insert(templates::TEMPLATE_CLEAR1, template);

    let total = (MAP_CELL_W * MAP_CELL_H) as usize;
    let cells = vec![
        MapCell {
            template: 0xFFFF, // "no template" -> resolved as clear terrain
            icon: 0,
        };
        total
    ];
    let scenario = Scenario {
        theater: Theater::Snow, // arbitrary; unused by rasterize
        map_x: 0,
        map_y: 0,
        map_width: 4,
        map_height: 4,
        cells,
        overlay: Vec::new(),
    };

    let raster = terrain::rasterize(&scenario, &tiles);
    (raster, synthetic_palette())
}

/// Cell of the `n`th synthetic "jeep" spawned by [`synthetic_core_with_units`]
/// / [`synthetic_world_with_units`], exposed so scripted-drive tests can
/// address a specific unit's start position without hardcoding the layout
/// twice.
pub fn synthetic_unit_cell(n: i32) -> CellCoord {
    CellCoord::new(10 + n * 2, 10)
}

/// Build a `World` over an all-passable synthetic grid with a small, fixed
/// population: three house-1 "jeeps" in a row (stand-ins for the real
/// scg01ea JEEPs — M3 has no real unit catalog loaded here, just `MoveStats`
/// shaped like one) plus one house-2 unit off to the side, so
/// selection/ownership tests have both a same-house group to select and a
/// different-house unit that must never be swept in by mistake. Returns the
/// world plus the three house-1 handles in spawn order.
pub fn synthetic_world_with_units(seed: u32) -> (World, Vec<Handle>) {
    let mut world = World::new(ra_sim::Passability::all_passable(), seed);
    let jeep_stats = MoveStats {
        max_speed: 25, // Speed=10 -> 10*256/100
        rot: 10,
    };
    let mut jeeps = Vec::new();
    for i in 0..3i32 {
        let h = world.spawn_unit(0, 1, synthetic_unit_cell(i), Facing(0), 256, jeep_stats);
        jeeps.push(h);
    }
    // A house-2 unit well away from the jeeps and from any destination the
    // scripted-drive tests click on, so it's a reliable "must not move"
    // witness for ownership-scoped orders.
    world.spawn_unit(
        1,
        2,
        CellCoord::new(60, 60),
        Facing(0),
        256,
        MoveStats {
            max_speed: 20,
            rot: 8,
        },
    );
    (world, jeeps)
}

/// [`synthetic_core`] plus the unit population from
/// [`synthetic_world_with_units`], wrapped in an `AppCore`. No sprites are
/// installed (unit bodies won't draw, but `compose`'s sprite lookup already
/// tolerates a missing sprite by design — see `AppCore::draw_units` — so
/// this still exercises every non-rendering unit code path: selection,
/// ownership, command emission, movement, hashing).
pub fn synthetic_core_with_units(seed: u32) -> (AppCore, Vec<Handle>) {
    let (raster, palette) = synthetic_fixture();
    let (world, jeeps) = synthetic_world_with_units(seed);
    let core = AppCore::with_sim(raster.clone(), *palette, world, Vec::new(), Vec::new());
    (core, jeeps)
}

/// A hand-built weapon profile shaped like 2TNK's real 90mm cannon (AP,
/// Damage 30, ROF 50, Range 4.75 cells, Speed 40 -> 102 leptons/tick,
/// non-instant) — the same numbers `ra_sim::world`'s own combat unit tests
/// and `ra-sim/tests/firing_fsm.rs` use, so a UI-level battle exercises
/// realistic pacing (a shot roughly every 50 ticks, several ticks of
/// straight bullet flight) rather than an instant one-shot kill.
fn synthetic_ninety_mm() -> ra_sim::WeaponProfile {
    fn pct5(p: [i32; 5]) -> [i32; 5] {
        let mut o = [0i32; 5];
        for (d, v) in o.iter_mut().zip(p) {
            *d = v * 65536 / 100;
        }
        o
    }
    ra_sim::WeaponProfile {
        damage: 30,
        rof: 50,
        range: 1216,
        proj_speed: 102,
        proj_rot: 0,
        invisible: false,
        instant: false,
        warhead: ra_sim::WarheadProfile {
            spread: 3,
            verses: pct5([30, 75, 75, 100, 50]),
        },
        warhead_ap: true,
        arcing: false,
        ballistic_scatter: 256,
        homing_scatter: 512,
        min_damage: 1,
        max_damage: 1000,
    }
}

/// Like [`synthetic_world_with_units`], but every house-1 "jeep" is armed
/// (a 90mm-shaped weapon, see [`synthetic_ninety_mm`]) and the house-2
/// witness unit sits close enough (2 cells from the nearest jeep, well
/// inside the weapon's 4.75-cell range) that an `Attack` order issued
/// through `AppCore`'s real click path can actually converge into gunfire
/// within a modest tick budget — unlike the unarmed
/// [`synthetic_world_with_units`], which can never emit a well-formed
/// `Attack` at all (`AppCore::issue_order` only orders *armed* selected
/// units to attack), leaving that whole `Command` variant unexercised by
/// any always-run (no-real-assets) UI suite. Returns the world, the 3
/// house-1 jeep handles, and the house-2 target handle.
pub fn synthetic_world_with_armed_units(seed: u32) -> (World, Vec<Handle>, Handle) {
    let mut world = World::new(ra_sim::Passability::all_passable(), seed);
    let jeep_stats = MoveStats {
        max_speed: 25,
        rot: 10,
    };
    let mut jeeps = Vec::new();
    for i in 0..3i32 {
        let h = world.spawn_unit(0, 1, synthetic_unit_cell(i), Facing(0), 400, jeep_stats);
        world.set_unit_combat(h, 3 /* heavy */, Some(synthetic_ninety_mm()), true);
        jeeps.push(h);
    }
    // 2 cells east of the third jeep (synthetic_unit_cell(2) = (14,10)):
    // close enough to be in range almost immediately.
    let target = world.spawn_unit(
        1,
        2,
        CellCoord::new(16, 10),
        Facing(0),
        150,
        MoveStats {
            max_speed: 20,
            rot: 8,
        },
    );
    world.set_unit_combat(target, 3, None, false); // unarmed — a pure target
    (world, jeeps, target)
}

/// [`synthetic_core`] plus [`synthetic_world_with_armed_units`], wrapped in
/// an `AppCore`. Companion to [`synthetic_core_with_units`] for combat
/// coverage (see that helper's docs on why sprites aren't needed).
pub fn synthetic_core_with_armed_units(seed: u32) -> (AppCore, Vec<Handle>, Handle) {
    let (raster, palette) = synthetic_fixture();
    let (world, jeeps, target) = synthetic_world_with_armed_units(seed);
    let core = AppCore::with_sim(raster.clone(), *palette, world, Vec::new(), Vec::new());
    (core, jeeps, target)
}

/// Load the M2/M3 reference scenario (`scg01ea.ini`) as a fully playable
/// [`LoadedGame`] (terrain + real spawned units) from the real assets, or
/// print a skip notice and return `None`. Companion to [`load_real_core`]
/// (terrain-only); this one drives `ra_client::assets::load_game_from_dir`,
/// so it needs `redalert.mix` (rules.ini, PALETTE.CPS) in addition to
/// `main.mix`.
pub fn load_real_game() -> Option<LoadedGame> {
    if !real_assets_available() {
        eprintln!(
            "SKIP: real assets not found under {} (set RA_ASSETS_DIR or copy \
             main.mix/redalert.mix into assets/ to run this test)",
            assets_dir().display()
        );
        return None;
    }
    Some(
        assets::load_game_from_dir(&assets_dir(), "scg01ea.ini")
            .expect("scg01ea.ini should load as a playable game from the real assets"),
    )
}

/// Pixels per cell edge (same constant `AppCore`/`ra-client`'s binary use).
const CELL_PIXELS: i32 = ICON_WIDTH as i32;

/// Virtual-time step that advances `AppCore` by ≈ one 15 Hz sim tick
/// (`1000 / TICKS_PER_SECOND`, rounded up so repeated calls don't fall
/// slightly behind and occasionally skip a tick).
pub const TICK_MS: u32 = 67;

/// Drive a "box-select the units inside this viewport rectangle, right-click
/// this destination cell, step some ticks" script through `core`'s real
/// `handle`/`update` seam (DESIGN.md §4.8 layer 1) and return the per-`
/// update()`-call `sim_hash()` chain (`warmup_ticks` idle steps first, then
/// `settle_ticks` after the order is issued) plus whatever `Command`s the
/// right-click emitted. Shared by the scripted end-to-end drive
/// (`ui_scripted_drive.rs`) and the client-level determinism suite
/// (`ui_determinism.rs`) so both exercise exactly the same input sequence —
/// one as a behavior assertion, the other as a same-script-twice hash
/// comparison.
pub fn run_select_and_move_script(
    core: &mut AppCore,
    camera: (f32, f32),
    viewport: (u32, u32),
    select_corners: ((i32, i32), (i32, i32)),
    dest_cell: CellCoord,
    warmup_ticks: u32,
    settle_ticks: u32,
) -> (Vec<u64>, Vec<Command>) {
    core.handle(InputEvent::Resize {
        width: viewport.0,
        height: viewport.1,
    });
    core.set_camera(camera.0, camera.1);

    let mut hashes = Vec::new();
    for _ in 0..warmup_ticks {
        core.update(TICK_MS);
        hashes.push(core.sim_hash());
    }

    let (s, e) = select_corners;
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Left,
        x: s.0,
        y: s.1,
    });
    core.handle(InputEvent::MouseMoved { x: e.0, y: e.1 });
    core.handle(InputEvent::MouseUp {
        button: MouseButton::Left,
        x: e.0,
        y: e.1,
    });

    let r = core.camera_rect();
    let dest_vx = (dest_cell.x * CELL_PIXELS) as i64 + CELL_PIXELS as i64 / 2 - r.x;
    let dest_vy = (dest_cell.y * CELL_PIXELS) as i64 + CELL_PIXELS as i64 / 2 - r.y;
    core.handle(InputEvent::MouseDown {
        button: MouseButton::Right,
        x: dest_vx as i32,
        y: dest_vy as i32,
    });
    let emitted = core.drain_commands();

    for _ in 0..settle_ticks {
        core.update(TICK_MS);
        hashes.push(core.sim_hash());
    }
    (hashes, emitted)
}
