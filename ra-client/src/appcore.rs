//! `AppCore` — the windowless heart of the client (DESIGN.md §4.8). All UI and
//! game-view behavior lives here so every corner of it is reachable from tests
//! without a window: feed it [`InputEvent`]s, advance it with virtual time via
//! [`AppCore::update`], and read pixels back with [`AppCore::compose`]. The
//! macroquad shell is only a thin adapter over this seam.
//!
//! M2 gave it the terrain camera. M3 adds the simulation view: it owns a
//! [`ra_sim::World`], steps it at a fixed 15 Hz on **virtual** time, renders
//! units at interpolated positions with house-colour remap, and turns
//! left-drag selection + right-click into [`Command`]s that flow through the
//! deterministic pipeline. A core built with [`AppCore::new`] (terrain only)
//! has no units, so it behaves exactly as it did at M2.

use std::collections::BTreeMap;

use ra_data::house::{identity_remap, RemapTable};
use ra_formats::tmpl::ICON_WIDTH;
use ra_sim::coords::{CellCoord, Facing, WorldCoord, LEPTONS_PER_CELL};
use ra_sim::{BuildItem, GameOver, Handle, Passability, ProdKind, Target, World};

use crate::compositor::{viewport_rgba, IndexedImage, Palette, RgbaImage};
use crate::font;
use crate::input::{InputEvent, Key, MouseButton, Rect};
use crate::unit_render::{
    draw_health_bar, draw_rect_outline, draw_sprite_centered, draw_sprite_topleft, fill_rect,
    UnitSprite,
};

/// Sim commands the UI emits. Re-exported from the sim so the whole app speaks
/// one command vocabulary (DESIGN.md §4.4).
pub use ra_sim::Command;

/// Width of the build sidebar strip, in viewport pixels (§4.9 M5: "functional,
/// not pretty"). Only present when the sidebar is enabled (game mode); the M2/M3
/// terrain and combat test paths never enable it, so `compose_camera` there is
/// byte-identical to before.
pub const SIDEBAR_W: u32 = 130;
/// Row height for one sidebar buildable entry, in pixels.
const SIDEBAR_ROW_H: i32 = 22;
/// Sidebar background colour.
const SIDEBAR_BG: [u8; 3] = [24, 24, 28];
/// Gold-ore render colour (tactical overlay).
const ORE_GOLD_RGB: [u8; 3] = [196, 160, 40];
/// Gem-ore render colour.
const ORE_GEM_RGB: [u8; 3] = [70, 150, 210];

/// A single buildable entry the sidebar exposes (also the queryable surface
/// tests drive the build UI through).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarItem {
    /// The sim command payload this row builds.
    pub item: BuildItem,
    /// Short display name (e.g. `"POWR"`, `"2TNK"`).
    pub name: String,
    /// Build cost in credits.
    pub cost: i32,
    /// Whether the player can start it right now (prereqs + factory + funds).
    pub buildable: bool,
    /// Build progress in permille if this item is currently in production.
    pub progress: Option<i32>,
    /// Whether this (structure) is finished and awaiting placement.
    pub ready: bool,
}

/// The composed output of a frame — an RGBA image ready to upload as a texture.
pub type Frame = RgbaImage;

/// The simulation tick rate (`TICKS_PER_SECOND 15`, DESIGN.md §5).
pub const TICKS_PER_SECOND: u64 = 15;

/// Pixels per cell edge in the terrain raster (SHP icon size).
const CELL_PIXELS: i32 = ICON_WIDTH as i32;

/// Maximum sim ticks stepped in a single [`AppCore::update`] call. Real frames
/// advance virtual time by ~16 ms (≈¼ tick), so this only bites under a
/// pathologically large `dt`, where we deliberately drop the excess rather than
/// spin catching up thousands of ticks — a documented cap (DESIGN.md §4.8
/// structural finding). Determinism is unaffected: the same `dt` sequence
/// always produces the same tick count.
const MAX_CATCHUP_TICKS: u32 = 8;

/// Largest viewport dimension AppCore will accept from a `Resize`, per axis. An
/// unbounded resize would let a compose allocate `w*h*4` bytes without limit;
/// this caps a single frame near a quarter-gigabyte. Documented structural
/// bound requested by ra-tester.
pub const MAX_VIEWPORT_DIM: u32 = 8192;

/// Default camera scroll speed, in map pixels per second.
const DEFAULT_SCROLL_SPEED: f32 = 640.0;
/// Distance from a viewport edge (pixels) within which the pointer edge-scrolls.
const EDGE_MARGIN: i32 = 16;
/// Below this drag size (pixels) a left-release is treated as a click, not a box.
const CLICK_SLOP: i32 = 3;
/// Click-select pick radius, in map pixels.
const PICK_RADIUS: i32 = CELL_PIXELS;

/// Selection marker / drag-box colour (classic RA green).
const SELECT_RGB: [u8; 3] = [0, 255, 0];

/// An in-progress left-drag box, in viewport pixels.
#[derive(Clone, Copy, Debug)]
struct DragBox {
    start: (i32, i32),
    cur: (i32, i32),
}

/// The windowless client core: terrain raster + camera + the sim view.
pub struct AppCore {
    raster: IndexedImage,
    palette: Palette,

    // Camera position (top-left of the viewport) in map pixels. Float because
    // this is presentation state, never simulation state.
    cam_x: f32,
    cam_y: f32,
    viewport_w: u32,
    viewport_h: u32,

    // Held scroll keys.
    left: bool,
    right: bool,
    up: bool,
    down: bool,

    // Last known pointer position (viewport pixels) and whether it is inside.
    mouse_x: i32,
    mouse_y: i32,
    mouse_inside: bool,

    scroll_speed: f32,

    // --- Simulation view (M3) ---
    world: World,
    /// Unit body sprites, indexed by `Unit::type_id`.
    sprites: Vec<UnitSprite>,
    /// House-colour remap LUTs, indexed by house; falls back to identity.
    remaps: Vec<RemapTable>,

    /// Virtual time accumulated for sim stepping, in milliseconds.
    virtual_ms: u64,
    /// Fraction (0..1000) into the current tick, for render interpolation.
    tick_frac: u32,
    /// Previous-tick unit positions (handle index → coord) for interpolation.
    prev_coords: BTreeMap<u32, WorldCoord>,

    /// Currently-selected unit handle indices.
    selected: Vec<Handle>,
    /// Active left-drag selection box, if any.
    drag: Option<DragBox>,

    /// Commands queued for the next sim tick (loopback pipeline).
    pending: Vec<Command>,
    /// Commands emitted since the last [`AppCore::drain_commands`] (for the net
    /// layer / tests to observe).
    emitted: Vec<Command>,

    // --- Build UI (M5) ---
    /// Whether the build sidebar is drawn and interactive (game mode). Off by
    /// default so terrain/combat test paths are unaffected.
    sidebar_enabled: bool,
    /// The controlled ("player") house. Selection and orders gate to it once the
    /// sidebar is enabled (fixes the M3 open question); `None` = the legacy
    /// "command whatever you select" behaviour used by the headless harnesses.
    player_house: Option<u8>,
    /// Building idle sprites, indexed by building type id.
    building_sprites: Vec<UnitSprite>,
    /// The buildable items the sidebar lists, in display order.
    buildables: Vec<BuildItem>,
    /// Active placement mode: a completed building type id awaiting a map click.
    placing: Option<u32>,
}

impl AppCore {
    /// Build a terrain-only core (no units), exactly as M2. Camera starts at the
    /// map origin with a default viewport size.
    pub fn new(raster: IndexedImage, palette: Palette) -> AppCore {
        AppCore::with_sim(
            raster,
            palette,
            World::new(Passability::all_passable(), 0),
            Vec::new(),
            Vec::new(),
        )
    }

    /// Build a core with a populated simulation: `world` already holds spawned
    /// units, `sprites` are indexed by `Unit::type_id`, and `remaps` by house.
    pub fn with_sim(
        raster: IndexedImage,
        palette: Palette,
        world: World,
        sprites: Vec<UnitSprite>,
        remaps: Vec<RemapTable>,
    ) -> AppCore {
        AppCore {
            raster,
            palette,
            cam_x: 0.0,
            cam_y: 0.0,
            viewport_w: 640,
            viewport_h: 400,
            left: false,
            right: false,
            up: false,
            down: false,
            mouse_x: -1,
            mouse_y: -1,
            mouse_inside: false,
            scroll_speed: DEFAULT_SCROLL_SPEED,
            world,
            sprites,
            remaps,
            virtual_ms: 0,
            tick_frac: 0,
            prev_coords: BTreeMap::new(),
            selected: Vec::new(),
            drag: None,
            pending: Vec::new(),
            emitted: Vec::new(),
            sidebar_enabled: false,
            player_house: None,
            building_sprites: Vec::new(),
            buildables: Vec::new(),
            placing: None,
        }
    }

    /// Install building idle sprites, indexed by building type id.
    pub fn set_building_sprites(&mut self, sprites: Vec<UnitSprite>) {
        self.building_sprites = sprites;
    }

    /// Enable the build sidebar for the controlled `player_house`, listing
    /// `buildables` (in display order). Turns on player-house gating of
    /// selection/orders.
    pub fn enable_sidebar(&mut self, player_house: u8, buildables: Vec<BuildItem>) {
        self.sidebar_enabled = true;
        self.player_house = Some(player_house);
        self.buildables = buildables;
    }

    /// The controlled house, if one is set.
    pub fn player_house(&self) -> Option<u8> {
        self.player_house
    }

    /// The current terminal game state (Ongoing / Victory / Defeat), surfaced for
    /// the overlay, the shell, and tests.
    pub fn game_over(&self) -> GameOver {
        self.world.game_over()
    }

    /// Whether the UI still accepts player orders — false once the game is over
    /// (§4.9 M6: "stops accepting orders").
    fn accepting_orders(&self) -> bool {
        self.world.game_over() == GameOver::Ongoing
    }

    /// The controlled house's spendable credits (0 if none / no house).
    pub fn credits(&self) -> i32 {
        self.player_house
            .map(|h| self.world.house_credits(h))
            .unwrap_or(0)
    }

    /// The controlled house's `(power_output, power_drain)`.
    pub fn power(&self) -> (i32, i32) {
        self.player_house
            .and_then(|h| self.world.house(h))
            .map(|hs| (hs.power_output, hs.power_drain))
            .unwrap_or((0, 0))
    }

    /// The sidebar's buildable entries, with live buildable/progress/ready
    /// state — the queryable surface tests drive the build UI through.
    pub fn sidebar_items(&self) -> Vec<SidebarItem> {
        let Some(house) = self.player_house else {
            return Vec::new();
        };
        let hs = self.world.house(house);
        self.buildables
            .iter()
            .filter_map(|&item| self.describe_buildable(house, hs, item))
            .collect()
    }

    fn describe_buildable(
        &self,
        house: u8,
        hs: Option<&ra_sim::House>,
        item: BuildItem,
    ) -> Option<SidebarItem> {
        let cat = &self.world.catalog;
        let (name, cost, prereq) = match item {
            BuildItem::Building(id) => {
                let p = cat.building(id)?;
                (p.name.clone(), p.cost, &p.prereq)
            }
            BuildItem::Unit(id) => {
                let p = cat.unit(id)?;
                (p.name.clone(), p.cost, &p.prereq)
            }
        };

        // In production?
        let (progress, ready) = match (item, hs) {
            (BuildItem::Building(id), Some(h)) => {
                let ready = h.ready_building == Some(id);
                let prog = h
                    .building_prod
                    .filter(|p| p.item == item)
                    .map(|p| p.progress_permille());
                (prog, ready)
            }
            (BuildItem::Unit(_), Some(h)) => {
                let prog = h
                    .unit_prod
                    .filter(|p| p.item == item)
                    .map(|p| p.progress_permille());
                (prog, false)
            }
            _ => (None, false),
        };

        // Buildable now? Prereqs owned + the producing factory present + funds +
        // the lane free.
        let owns = |id: u32| hs.map(|h| h.owns_building(id)).unwrap_or(false);
        let prereqs_ok = prereq.iter().all(|&id| owns(id));
        let (factory_ok, lane_free) = match item {
            BuildItem::Building(_) => {
                let yard = self
                    .world
                    .buildings
                    .iter()
                    .any(|(_, b)| b.house == house && b.is_construction_yard && b.is_alive());
                let free = hs
                    .map(|h| h.building_prod.is_none() && h.ready_building.is_none())
                    .unwrap_or(false);
                (yard, free)
            }
            BuildItem::Unit(_) => {
                let fac = self
                    .world
                    .buildings
                    .iter()
                    .any(|(_, b)| b.house == house && b.is_war_factory && b.is_alive());
                let free = hs.map(|h| h.unit_prod.is_none()).unwrap_or(false);
                (fac, free)
            }
        };
        let funds = self.world.house_credits(house) > 0;
        let buildable =
            prereqs_ok && factory_ok && lane_free && funds && progress.is_none() && !ready;

        Some(SidebarItem {
            item,
            name,
            cost,
            buildable,
            progress,
            ready,
        })
    }

    /// Full map width in pixels.
    pub fn map_width(&self) -> u32 {
        self.raster.width
    }
    /// Full map height in pixels.
    pub fn map_height(&self) -> u32 {
        self.raster.height
    }

    /// The current viewport size in pixels.
    pub fn viewport_size(&self) -> (u32, u32) {
        (self.viewport_w, self.viewport_h)
    }

    /// Borrow the simulation world (read-only view for tests/tools).
    pub fn world(&self) -> &World {
        &self.world
    }

    /// The current sim state hash — the determinism backbone surfaced through
    /// the seam so drives can assert same-seed-twice equality.
    pub fn sim_hash(&self) -> u64 {
        self.world.state_hash()
    }

    /// Directly set the selection to `handles` (only those the player may
    /// command are kept). A deterministic selection seam for the verification
    /// harness / tests — equivalent to a box-select but independent of the
    /// camera position.
    pub fn select_units(&mut self, handles: &[Handle]) {
        self.selected = handles
            .iter()
            .copied()
            .filter(|&h| {
                self.world
                    .units
                    .get(h)
                    .map(|u| self.selectable(u.house))
                    .unwrap_or(false)
            })
            .collect();
    }

    /// The handles of currently-selected units (ascending slot order).
    pub fn selected_handles(&self) -> Vec<Handle> {
        self.world
            .units
            .handles()
            .into_iter()
            .filter(|h| self.selected.contains(h))
            .collect()
    }

    /// Handle one input event.
    pub fn handle(&mut self, ev: InputEvent) {
        match ev {
            InputEvent::KeyDown(Key::Deploy) => self.deploy_selected(),
            InputEvent::KeyUp(Key::Deploy) => {}
            InputEvent::KeyDown(k) => self.set_key(k, true),
            InputEvent::KeyUp(k) => self.set_key(k, false),
            InputEvent::MouseMoved { x, y } => {
                self.mouse_x = x;
                self.mouse_y = y;
                self.mouse_inside = true;
                if let Some(d) = &mut self.drag {
                    d.cur = (x, y);
                }
            }
            InputEvent::MouseLeft => self.mouse_inside = false,
            InputEvent::MouseDown { button, x, y } => match button {
                MouseButton::Left => {
                    // Sidebar click? (game mode, click in the right strip)
                    if self.sidebar_enabled && x >= self.tactical_width() as i32 {
                        self.sidebar_click(x, y);
                    } else if self.placing.is_some() {
                        // Placement mode: a tactical click drops the building.
                        self.place_at(x, y);
                    } else {
                        self.drag = Some(DragBox {
                            start: (x, y),
                            cur: (x, y),
                        })
                    }
                }
                MouseButton::Right => {
                    if self.placing.take().is_some() {
                        // Right-click cancels placement (keeps the ready building).
                    } else {
                        self.issue_order(x, y);
                    }
                }
            },
            InputEvent::MouseUp { button, x, y } => {
                if button == MouseButton::Left {
                    if let Some(d) = self.drag.take() {
                        self.finish_selection(d.start, (x, y));
                    }
                }
            }
            InputEvent::Resize { width, height } => {
                self.viewport_w = width.clamp(1, MAX_VIEWPORT_DIM);
                self.viewport_h = height.clamp(1, MAX_VIEWPORT_DIM);
                self.clamp_camera();
            }
        }
    }

    fn set_key(&mut self, k: Key, down: bool) {
        match k {
            Key::Left => self.left = down,
            Key::Right => self.right = down,
            Key::Up => self.up = down,
            Key::Down => self.down = down,
            Key::Deploy => {} // handled at the event layer (edge-triggered)
        }
    }

    /// Advance the camera and the simulation by `dt_ms` of virtual time.
    pub fn update(&mut self, dt_ms: u32) {
        // Camera scroll (unchanged from M2).
        let (dx, dy) = self.scroll_direction();
        if dx != 0.0 || dy != 0.0 {
            let dt = dt_ms as f32 / 1000.0;
            self.cam_x += dx * self.scroll_speed * dt;
            self.cam_y += dy * self.scroll_speed * dt;
            self.clamp_camera();
        }

        // Fixed-timestep sim stepping on virtual time.
        self.virtual_ms = self.virtual_ms.saturating_add(dt_ms as u64);
        let target = (self.virtual_ms.saturating_mul(TICKS_PER_SECOND) / 1000) as u32;
        let mut steps = 0;
        while self.world.tick_count() < target && steps < MAX_CATCHUP_TICKS {
            self.step_tick();
            steps += 1;
        }
        if self.world.tick_count() < target {
            // Giant dt: snap the clock to the current tick so we neither spin
            // nor perpetually lag (see MAX_CATCHUP_TICKS).
            self.virtual_ms = (self.world.tick_count() as u64) * 1000 / TICKS_PER_SECOND;
        }
        self.tick_frac = (self.virtual_ms.saturating_mul(TICKS_PER_SECOND) % 1000) as u32;
    }

    /// Snapshot positions for interpolation, then apply one tick's commands and
    /// run the sim's systems.
    fn step_tick(&mut self) {
        self.prev_coords.clear();
        for (h, u) in self.world.units.iter() {
            self.prev_coords.insert(h.index, u.coord);
        }
        let cmds = std::mem::take(&mut self.pending);
        self.world.tick(&cmds);
    }

    /// Unit-ish scroll direction from held keys plus pointer edge scrolling.
    fn scroll_direction(&self) -> (f32, f32) {
        let mut dx = 0.0f32;
        let mut dy = 0.0f32;
        if self.left {
            dx -= 1.0;
        }
        if self.right {
            dx += 1.0;
        }
        if self.up {
            dy -= 1.0;
        }
        if self.down {
            dy += 1.0;
        }
        // Edge scroll only over the tactical area (never from inside the
        // sidebar strip).
        let tw = self.tactical_width() as i32;
        if self.mouse_inside && self.mouse_x < tw {
            if self.mouse_x >= 0 && self.mouse_x < EDGE_MARGIN {
                dx -= 1.0;
            } else if self.mouse_x >= tw - EDGE_MARGIN {
                dx += 1.0;
            }
            if self.mouse_y >= 0 && self.mouse_y < EDGE_MARGIN {
                dy -= 1.0;
            } else if self.mouse_y >= self.viewport_h as i32 - EDGE_MARGIN {
                dy += 1.0;
            }
        }
        (dx.clamp(-1.0, 1.0), dy.clamp(-1.0, 1.0))
    }

    /// Width of the tactical (map) area in viewport pixels — the full viewport
    /// unless the build sidebar is enabled, in which case the sidebar strip is
    /// reserved on the right.
    pub fn tactical_width(&self) -> u32 {
        if self.sidebar_enabled {
            self.viewport_w.saturating_sub(SIDEBAR_W).max(1)
        } else {
            self.viewport_w
        }
    }

    /// Clamp the camera so the tactical viewport stays within the map.
    fn clamp_camera(&mut self) {
        let max_x = (self.raster.width as f32 - self.tactical_width() as f32).max(0.0);
        let max_y = (self.raster.height as f32 - self.viewport_h as f32).max(0.0);
        self.cam_x = self.cam_x.clamp(0.0, max_x);
        self.cam_y = self.cam_y.clamp(0.0, max_y);
    }

    /// Directly set the camera top-left (map pixels); clamped. For tests/CLI.
    pub fn set_camera(&mut self, x: f32, y: f32) {
        self.cam_x = x;
        self.cam_y = y;
        self.clamp_camera();
    }

    /// The clamped tactical viewport rectangle at the current camera position.
    /// Its width excludes the sidebar strip when the sidebar is enabled, so
    /// viewport→map click mapping stays correct for the visible tactical area.
    pub fn camera_rect(&self) -> Rect {
        Rect {
            x: self.cam_x.round() as i64,
            y: self.cam_y.round() as i64,
            width: self.tactical_width(),
            height: self.viewport_h,
        }
    }

    /// Composite an arbitrary map-space rectangle to RGBA: terrain, then units
    /// at interpolated positions with their house remap, then selection markers.
    /// Pure over `self` — camera state is not read, so tests can sweep the whole
    /// map independently of the camera.
    pub fn compose(&self, viewport: Rect) -> Frame {
        let mut frame = viewport_rgba(
            &self.raster,
            &self.palette,
            viewport.x,
            viewport.y,
            viewport.width,
            viewport.height,
        );
        self.draw_units(&mut frame, viewport);
        frame
    }

    /// Composite at the current camera position, plus the transient drag-select
    /// box (which is viewport-relative, so it only belongs on the camera view).
    /// In game mode (sidebar enabled) this delegates to [`AppCore::compose_game`].
    pub fn compose_camera(&self) -> Frame {
        if self.sidebar_enabled {
            return self.compose_game();
        }
        let rect = self.camera_rect();
        let mut frame = self.compose(rect);
        if let Some(d) = &self.drag {
            draw_rect_outline(
                &mut frame, d.start.0, d.start.1, d.cur.0, d.cur.1, SELECT_RGB,
            );
        }
        frame
    }

    /// The full game view: a `viewport_w × viewport_h` frame with the tactical
    /// map (terrain + ore + buildings + units) on the left, the build sidebar on
    /// the right, plus the placement preview and drag box. Used by the windowed
    /// shell and the M5 economy verification.
    pub fn compose_game(&self) -> Frame {
        let tw = self.tactical_width();
        // Tactical area: render terrain + entities for the camera rect into a
        // full-viewport-sized frame (its right strip stays black, then the
        // sidebar overpaints it).
        let cam = self.camera_rect();
        let mut frame = viewport_rgba(
            &self.raster,
            &self.palette,
            cam.x,
            cam.y,
            self.viewport_w,
            self.viewport_h,
        );
        self.draw_ore(&mut frame, cam);
        self.draw_buildings(&mut frame, cam);
        // Units draw over terrain/buildings; anything spilling into the sidebar
        // strip is overpainted by `draw_sidebar` below.
        self.draw_units(&mut frame, cam);
        // Shroud: paint unexplored cells black, hiding whatever sits under them.
        self.draw_shroud(&mut frame, cam);
        self.draw_placement_preview(&mut frame, cam, tw);
        if let Some(d) = &self.drag {
            draw_rect_outline(
                &mut frame, d.start.0, d.start.1, d.cur.0, d.cur.1, SELECT_RGB,
            );
        }
        self.draw_sidebar(&mut frame);
        self.draw_game_over(&mut frame);
        frame
    }

    /// Paint a solid-black overlay over cells the player house has not explored
    /// (M6 shroud). No-op when the shroud is disabled (non-skirmish worlds), so
    /// terrain/econ views are unchanged. Only the tactical strip is shrouded.
    fn draw_shroud(&self, frame: &mut RgbaImage, cam: Rect) {
        if !self.world.shroud.is_enabled() {
            return;
        }
        let house = self.player_house.unwrap_or(0);
        let tw = self.tactical_width() as i32;
        let cx0 = (cam.x.div_euclid(CELL_PIXELS as i64)) as i32 - 1;
        let cy0 = (cam.y.div_euclid(CELL_PIXELS as i64)) as i32 - 1;
        let cx1 = cx0 + (tw / CELL_PIXELS) + 3;
        let cy1 = cy0 + (self.viewport_h as i32 / CELL_PIXELS) + 3;
        for cy in cy0..cy1 {
            for cx in cx0..cx1 {
                let c = CellCoord::new(cx, cy);
                if self.world.shroud.is_explored(house, c) {
                    continue;
                }
                let px = (cx * CELL_PIXELS) as i64 - cam.x;
                let py = (cy * CELL_PIXELS) as i64 - cam.y;
                // Clip the black fill to the tactical strip (leave the sidebar).
                let x0 = (px as i32).max(0);
                let x1 = ((px + CELL_PIXELS as i64) as i32).min(tw);
                let y0 = (py as i32).max(0);
                let y1 = ((py + CELL_PIXELS as i64) as i32).min(self.viewport_h as i32);
                if x1 > x0 && y1 > y0 {
                    fill_rect(frame, x0, y0, x1 - 1, y1 - 1, [0, 0, 0]);
                }
            }
        }
    }

    /// Draw the centred VICTORY / DEFEAT banner once the skirmish resolves.
    fn draw_game_over(&self, frame: &mut RgbaImage) {
        let (text, rgb) = match self.world.game_over() {
            GameOver::Ongoing => return,
            GameOver::Victory => ("VICTORY", [120, 240, 120]),
            GameOver::Defeat => ("DEFEAT", [240, 90, 90]),
        };
        let scale = 6;
        let tw = font::text_width(text) * scale;
        let th = font::GLYPH_H * scale;
        let cx = (self.tactical_width() as i32 - tw) / 2;
        let cy = (self.viewport_h as i32 - th) / 2;
        // Dim backing band for legibility.
        fill_rect(
            frame,
            0,
            cy - 12,
            self.tactical_width() as i32 - 1,
            cy + th + 12,
            [12, 12, 16],
        );
        font::draw_text_scaled(frame, cx, cy, text, rgb, scale);
    }

    /// Interpolated render coordinate of a unit this frame.
    fn render_coord(&self, h: Handle, cur: WorldCoord) -> WorldCoord {
        match self.prev_coords.get(&h.index) {
            Some(prev) => {
                let f = self.tick_frac as i64;
                let x = prev.x.0 as i64 + (cur.x.0 as i64 - prev.x.0 as i64) * f / 1000;
                let y = prev.y.0 as i64 + (cur.y.0 as i64 - prev.y.0 as i64) * f / 1000;
                WorldCoord::new(x as i32, y as i32)
            }
            None => cur,
        }
    }

    /// Draw all units (and selection markers) whose sprite overlaps `viewport`.
    fn draw_units(&self, frame: &mut RgbaImage, viewport: Rect) {
        for (h, unit) in self.world.units.iter() {
            let coord = self.render_coord(h, unit.coord);
            let map_px = leptons_to_pixel(coord.x.0);
            let map_py = leptons_to_pixel(coord.y.0);
            let sx = (map_px as i64 - viewport.x) as i32;
            let sy = (map_py as i64 - viewport.y) as i32;

            let remap = self
                .remaps
                .get(unit.house as usize)
                .copied()
                .unwrap_or_else(identity_remap);

            if let Some(sprite) = self.sprites.get(unit.type_id as usize) {
                // Body sprite.
                if let Some(sframe) = sprite.frame_for(unit.facing) {
                    draw_sprite_centered(frame, sx, sy, sframe, &remap, &self.palette);
                }
                // Turret overlay (turreted vehicles whose SHP has turret frames).
                if unit.has_turret {
                    if let Some(tframe) = sprite.turret_frame_for(unit.turret_facing) {
                        draw_sprite_centered(frame, sx, sy, tframe, &remap, &self.palette);
                    }
                }
            }

            // Muzzle flash: a brief bright spot at the barrel tip the tick(s)
            // right after a shot (arm just reset to ROF). Covers hitscan
            // weapons whose bullet never persists in the arena.
            if let Some(w) = &unit.weapon {
                if unit.has_target() && unit.arm + 2 >= w.rof && unit.arm != 0 {
                    let aim = if unit.has_turret {
                        unit.turret_facing
                    } else {
                        unit.facing
                    };
                    let (fx, fy) = offset_pixels(sx, sy, aim, CELL_PIXELS / 2);
                    fill_rect(frame, fx - 1, fy - 1, fx + 1, fy + 1, [255, 230, 120]);
                }
            }

            let selected = self.selected.contains(&h);
            if selected {
                let half = CELL_PIXELS / 2;
                draw_rect_outline(
                    frame,
                    sx - half,
                    sy - half,
                    sx + half,
                    sy + half,
                    SELECT_RGB,
                );
            }
            // Health bar on selected or damaged units.
            if selected || unit.health < unit.max_health {
                draw_health_bar(
                    frame,
                    sx,
                    sy - CELL_PIXELS / 2 - 4,
                    CELL_PIXELS,
                    unit.health_permille(),
                );
            }
        }

        self.draw_bullets(frame, viewport);
    }

    /// Draw projectiles in flight as short bright tracers. Invisible/hitscan
    /// projectiles are represented by the muzzle flash instead, so they are
    /// skipped here.
    fn draw_bullets(&self, frame: &mut RgbaImage, viewport: Rect) {
        for (_h, b) in self.world.bullets.iter() {
            if b.invisible {
                continue;
            }
            let px = (leptons_to_pixel(b.pos.x.0) as i64 - viewport.x) as i32;
            let py = (leptons_to_pixel(b.pos.y.0) as i64 - viewport.y) as i32;
            // Small tracer: a couple of pixels back along the flight direction.
            let (tx, ty) = offset_pixels(px, py, Facing(b.facing.0.wrapping_add(128)), 4);
            draw_line(frame, tx, ty, px, py, [255, 240, 160]);
            fill_rect(frame, px - 1, py - 1, px + 1, py + 1, [255, 255, 200]);
        }
    }

    /// Convert a viewport pixel to a map-pixel position at the current camera.
    fn viewport_to_map(&self, x: i32, y: i32) -> (i64, i64) {
        let r = self.camera_rect();
        (r.x + x as i64, r.y + y as i64)
    }

    /// Finish a left-drag: box-select owned units inside the rectangle, or on a
    /// near-zero drag treat it as a click that picks the nearest unit.
    fn finish_selection(&mut self, start: (i32, i32), end: (i32, i32)) {
        self.selected.clear();
        let (sx0, sy0) = self.viewport_to_map(start.0, start.1);
        let (sx1, sy1) = self.viewport_to_map(end.0, end.1);

        if (end.0 - start.0).abs() <= CLICK_SLOP && (end.1 - start.1).abs() <= CLICK_SLOP {
            // Click: pick the nearest selectable unit within PICK_RADIUS.
            let mut best: Option<(i64, Handle)> = None;
            for (h, unit) in self.world.units.iter() {
                if !self.selectable(unit.house) {
                    continue;
                }
                let px = leptons_to_pixel(unit.coord.x.0) as i64;
                let py = leptons_to_pixel(unit.coord.y.0) as i64;
                let d2 = (px - sx0) * (px - sx0) + (py - sy0) * (py - sy0);
                if d2 <= (PICK_RADIUS as i64) * (PICK_RADIUS as i64)
                    && best.map(|(bd, _)| d2 < bd).unwrap_or(true)
                {
                    best = Some((d2, h));
                }
            }
            if let Some((_, handle)) = best {
                self.selected.push(handle);
            }
            return;
        }

        let (xa, xb) = (sx0.min(sx1), sx0.max(sx1));
        let (ya, yb) = (sy0.min(sy1), sy0.max(sy1));
        for (h, unit) in self.world.units.iter() {
            if !self.selectable(unit.house) {
                continue;
            }
            let px = leptons_to_pixel(unit.coord.x.0) as i64;
            let py = leptons_to_pixel(unit.coord.y.0) as i64;
            if px >= xa && px <= xb && py >= ya && py <= yb {
                self.selected.push(h);
            }
        }
    }

    /// Whether the controlled player may select/command a unit of `house`. With
    /// no controlled house set (headless harnesses), everything is selectable —
    /// the legacy behaviour; once a player house is set (game mode) it gates to
    /// exactly that house.
    fn selectable(&self, house: u8) -> bool {
        match self.player_house {
            Some(ph) => ph == house,
            None => true,
        }
    }

    /// Right-click order: if the click lands on an **enemy** unit, every
    /// selected owned unit attacks it; otherwise it is a move to the clicked
    /// cell. Ownership: you cannot attack your own units (ground force-fire is
    /// deferred — the task's simplification). Emits `Command::Attack` or
    /// `Command::Move` through the deterministic pipeline.
    fn issue_order(&mut self, x: i32, y: i32) {
        if self.selected.is_empty() || !self.accepting_orders() {
            return;
        }
        let (mx, my) = self.viewport_to_map(x, y);
        // Collect live selected handles + houses first (borrow discipline).
        let orders: Vec<(Handle, u8)> = self
            .world
            .units
            .handles()
            .into_iter()
            .filter(|h| self.selected.contains(h))
            .filter_map(|h| self.world.units.get(h).map(|u| (h, u.house)))
            .collect();
        if orders.is_empty() {
            return;
        }
        // The controlling house is that of the selected units (single-house
        // player). A click on a unit of a different house is an attack order.
        let player_house = orders[0].1;
        let picked = self.unit_at_map(mx, my);
        let enemy = picked.and_then(|h| {
            self.world
                .units
                .get(h)
                .filter(|u| u.house != player_house)
                .map(|_| h)
        });
        // An enemy building under the cursor is an attack target too (M6). Own
        // buildings are ignored here — a sell mode over own buildings is deferred
        // (noted): the sim already has `Command::Sell`, but no UI affordance yet.
        let enemy_target = enemy.map(Target::Unit).or_else(|| {
            self.enemy_building_at_map(mx, my, player_house)
                .map(Target::Building)
        });

        if let Some(target) = enemy_target {
            for (unit, house) in orders {
                // Only armed units get an attack order; unarmed selected units
                // are simply left idle (the sim also rejects unarmed attackers).
                let armed = self
                    .world
                    .units
                    .get(unit)
                    .map(|u| u.weapon.is_some())
                    .unwrap_or(false);
                if !armed {
                    continue;
                }
                let cmd = Command::Attack {
                    unit,
                    target,
                    house,
                };
                self.pending.push(cmd);
                self.emitted.push(cmd);
            }
        } else {
            let dest = CellCoord::new(
                (mx / CELL_PIXELS as i64) as i32,
                (my / CELL_PIXELS as i64) as i32,
            );
            for (unit, house) in orders {
                let cmd = Command::Move { unit, dest, house };
                self.pending.push(cmd);
                self.emitted.push(cmd);
            }
        }
    }

    /// The unit whose sprite is nearest a map-pixel point within the pick
    /// radius, if any (slot order breaks ties deterministically).
    fn unit_at_map(&self, mx: i64, my: i64) -> Option<Handle> {
        let mut best: Option<(i64, Handle)> = None;
        for (h, unit) in self.world.units.iter() {
            let px = leptons_to_pixel(unit.coord.x.0) as i64;
            let py = leptons_to_pixel(unit.coord.y.0) as i64;
            let d2 = (px - mx) * (px - mx) + (py - my) * (py - my);
            if d2 <= (PICK_RADIUS as i64) * (PICK_RADIUS as i64)
                && best.map(|(bd, _)| d2 < bd).unwrap_or(true)
            {
                best = Some((d2, h));
            }
        }
        best.map(|(_, h)| h)
    }

    /// The enemy building whose footprint covers a map-pixel point, if any (for
    /// the enemy-building attack click). Own buildings return `None`.
    fn enemy_building_at_map(&self, mx: i64, my: i64, player_house: u8) -> Option<Handle> {
        let cell = CellCoord::new(
            (mx.div_euclid(CELL_PIXELS as i64)) as i32,
            (my.div_euclid(CELL_PIXELS as i64)) as i32,
        );
        self.world
            .buildings
            .iter()
            .find(|(_, b)| b.house != player_house && b.is_alive() && b.covers(cell))
            .map(|(h, _)| h)
    }

    // ---- Build UI actions (public so tests / the verification drive them) ----

    /// Queue a command into the loopback pipeline and record it as emitted.
    fn emit(&mut self, cmd: Command) {
        self.pending.push(cmd);
        self.emitted.push(cmd);
    }

    /// Deploy the currently-selected MCV (if any) into a construction yard.
    pub fn deploy_selected(&mut self) {
        if !self.accepting_orders() {
            return;
        }
        let Some(house) = self.player_house.or(Some(0)) else {
            return;
        };
        // Find a selected unit that is an MCV (a proto with `deploys_to`).
        let mcv = self
            .world
            .units
            .handles()
            .into_iter()
            .filter(|h| self.selected.contains(h))
            .find(|&h| {
                self.world
                    .units
                    .get(h)
                    .map(|u| self.is_mcv(u.type_id))
                    .unwrap_or(false)
            });
        if let Some(unit) = mcv {
            self.emit(Command::Deploy { unit, house });
        }
    }

    /// Whether unit sprite `type_id` belongs to a deployable MCV proto.
    fn is_mcv(&self, type_id: u32) -> bool {
        self.world
            .catalog
            .units
            .iter()
            .any(|p| p.sprite_id == type_id && p.deploys_to.is_some())
    }

    /// Start producing `item` for the controlled house (validated by the sim).
    pub fn start_production(&mut self, item: BuildItem) {
        if let Some(house) = self.player_house {
            self.emit(Command::StartProduction { house, item });
        }
    }

    /// Cancel the controlled house's production of `kind`.
    pub fn cancel_production(&mut self, kind: ProdKind) {
        if let Some(house) = self.player_house {
            self.emit(Command::CancelProduction { house, kind });
        }
    }

    /// Enter placement mode for a ready building type id (green/red preview
    /// follows the cursor until a map click).
    pub fn begin_placement(&mut self, building_id: u32) {
        self.placing = Some(building_id);
    }

    /// Whether a building is currently being placed.
    pub fn placing(&self) -> Option<u32> {
        self.placing
    }

    /// The footprint top-left cell for a tactical viewport pixel.
    fn cell_at_viewport(&self, x: i32, y: i32) -> CellCoord {
        let (mx, my) = self.viewport_to_map(x, y);
        CellCoord::new(
            (mx.div_euclid(CELL_PIXELS as i64)) as i32,
            (my.div_euclid(CELL_PIXELS as i64)) as i32,
        )
    }

    /// Attempt to place the building being placed at the clicked tactical pixel.
    /// Emits `PlaceBuilding` only when the spot is valid, so an errant click
    /// keeps placement mode active for a retry.
    fn place_at(&mut self, x: i32, y: i32) {
        if !self.accepting_orders() {
            return;
        }
        let (Some(building), Some(house)) = (self.placing, self.player_house) else {
            return;
        };
        let cell = self.cell_at_viewport(x, y);
        if self.world.can_place_building(house, building, cell) {
            self.emit(Command::PlaceBuilding {
                house,
                building,
                cell,
            });
            self.placing = None;
        }
    }

    /// Directly place a ready building (for tests / the verification harness).
    pub fn place_building(&mut self, building: u32, cell: CellCoord) {
        if let Some(house) = self.player_house {
            self.emit(Command::PlaceBuilding {
                house,
                building,
                cell,
            });
            self.placing = None;
        }
    }

    /// Y offset (px) where the buildable rows begin (below the readout header).
    fn sidebar_rows_top(&self) -> i32 {
        font::GLYPH_H * 3 + 12
    }

    /// The buildable index for a sidebar viewport point, if it lands on a row.
    fn sidebar_row_at(&self, x: i32, y: i32) -> Option<usize> {
        if x < self.tactical_width() as i32 {
            return None;
        }
        let top = self.sidebar_rows_top();
        if y < top {
            return None;
        }
        let idx = ((y - top) / SIDEBAR_ROW_H) as usize;
        let items = self.sidebar_items();
        if idx < items.len() {
            Some(idx)
        } else {
            None
        }
    }

    /// Handle a left-click inside the sidebar strip: ready buildings enter
    /// placement mode, buildable rows start production.
    fn sidebar_click(&mut self, x: i32, y: i32) {
        if !self.accepting_orders() {
            return;
        }
        let Some(idx) = self.sidebar_row_at(x, y) else {
            return;
        };
        let items = self.sidebar_items();
        let item = &items[idx];
        if item.ready {
            if let BuildItem::Building(id) = item.item {
                self.begin_placement(id);
            }
        } else if item.buildable {
            let it = item.item;
            self.start_production(it);
        }
    }

    // ---- Build UI rendering ----

    /// Draw ore cells as small coloured squares over the tactical terrain.
    fn draw_ore(&self, frame: &mut RgbaImage, cam: Rect) {
        let ore = &self.world.ore;
        // Iterate only the visible cell band.
        let cx0 = (cam.x.div_euclid(CELL_PIXELS as i64)) as i32 - 1;
        let cy0 = (cam.y.div_euclid(CELL_PIXELS as i64)) as i32 - 1;
        let cx1 = cx0 + (self.tactical_width() as i32 / CELL_PIXELS) + 3;
        let cy1 = cy0 + (self.viewport_h as i32 / CELL_PIXELS) + 3;
        for cy in cy0..cy1 {
            for cx in cx0..cx1 {
                let c = CellCoord::new(cx, cy);
                let cell = ore.at(c);
                if cell.bails == 0 {
                    continue;
                }
                let rgb = if cell.gem { ORE_GEM_RGB } else { ORE_GOLD_RGB };
                let px = (cx * CELL_PIXELS) as i64 - cam.x;
                let py = (cy * CELL_PIXELS) as i64 - cam.y;
                // Denser ore = larger patch (2..CELL_PIXELS-ish).
                let inset = (CELL_PIXELS - 4 - (cell.bails as i32).min(CELL_PIXELS - 6)).max(2) / 2;
                fill_rect(
                    frame,
                    px as i32 + inset,
                    py as i32 + inset,
                    px as i32 + CELL_PIXELS - inset,
                    py as i32 + CELL_PIXELS - inset,
                    rgb,
                );
            }
        }
    }

    /// Draw all buildings from their SHP idle frame (top-left anchored).
    fn draw_buildings(&self, frame: &mut RgbaImage, cam: Rect) {
        for (_h, b) in self.world.buildings.iter() {
            let remap = self
                .remaps
                .get(b.house as usize)
                .copied()
                .unwrap_or_else(identity_remap);
            let px = (b.cell.x * CELL_PIXELS) as i64 - cam.x;
            let py = (b.cell.y * CELL_PIXELS) as i64 - cam.y;
            let drawn = self.building_sprites.get(b.type_id as usize).and_then(|s| {
                s.frames.first().map(|f| {
                    draw_sprite_topleft(frame, px as i32, py as i32, f, &remap, &self.palette);
                })
            });
            if drawn.is_none() {
                // No sprite: fall back to a filled footprint so it is visible.
                fill_rect(
                    frame,
                    px as i32,
                    py as i32,
                    px as i32 + b.foot_w as i32 * CELL_PIXELS,
                    py as i32 + b.foot_h as i32 * CELL_PIXELS,
                    [90, 90, 110],
                );
            }
            // Damage bar.
            if b.health < b.max_health {
                draw_health_bar(
                    frame,
                    px as i32 + b.foot_w as i32 * CELL_PIXELS / 2,
                    py as i32 - 4,
                    b.foot_w as i32 * CELL_PIXELS,
                    b.health_permille(),
                );
            }
        }
    }

    /// Draw the green/red footprint preview while placing a building.
    fn draw_placement_preview(&self, frame: &mut RgbaImage, cam: Rect, tw: u32) {
        let (Some(building), Some(house)) = (self.placing, self.player_house) else {
            return;
        };
        if !self.mouse_inside || self.mouse_x >= tw as i32 {
            return;
        }
        let Some(proto) = self.world.catalog.building(building) else {
            return;
        };
        let cell = self.cell_at_viewport(self.mouse_x, self.mouse_y);
        let ok = self.world.can_place_building(house, building, cell);
        let rgb = if ok { [0, 220, 0] } else { [220, 0, 0] };
        for dy in 0..proto.foot_h as i32 {
            for dx in 0..proto.foot_w as i32 {
                let px = ((cell.x + dx) * CELL_PIXELS) as i64 - cam.x;
                let py = ((cell.y + dy) * CELL_PIXELS) as i64 - cam.y;
                draw_rect_outline(
                    frame,
                    px as i32,
                    py as i32,
                    px as i32 + CELL_PIXELS - 1,
                    py as i32 + CELL_PIXELS - 1,
                    rgb,
                );
            }
        }
    }

    /// Draw the build sidebar: credits + power header, then buildable rows with
    /// cost, a build-progress bar, or a READY tag.
    fn draw_sidebar(&self, frame: &mut RgbaImage) {
        let x0 = self.tactical_width() as i32;
        let w = self.viewport_w as i32;
        // Background panel.
        fill_rect(frame, x0, 0, w - 1, self.viewport_h as i32 - 1, SIDEBAR_BG);

        let pad = 4;
        let tx = x0 + pad;
        // Header: credits + power.
        let credits = self.credits();
        let (out, drain) = self.power();
        font::draw_text(frame, tx, 2, &format!("$ {credits}"), [240, 220, 80]);
        let low = drain > out;
        let pcol = if low { [230, 80, 80] } else { [120, 220, 120] };
        font::draw_text(
            frame,
            tx,
            2 + font::GLYPH_H + 2,
            &format!("PWR {out}/{drain}"),
            pcol,
        );

        // Buildable rows.
        let items = self.sidebar_items();
        let mut ry = self.sidebar_rows_top();
        for item in &items {
            let row_bg = if item.ready {
                [30, 70, 30]
            } else if item.buildable {
                [40, 40, 52]
            } else {
                [30, 30, 34]
            };
            fill_rect(frame, x0 + 2, ry, w - 3, ry + SIDEBAR_ROW_H - 2, row_bg);
            let name_col = if item.buildable || item.progress.is_some() || item.ready {
                [230, 230, 230]
            } else {
                [110, 110, 120]
            };
            font::draw_text(frame, tx, ry + 2, &item.name, name_col);
            font::draw_text(
                frame,
                tx,
                ry + 2 + font::GLYPH_H + 1,
                &format!("${}", item.cost),
                [180, 180, 140],
            );
            // Progress bar / ready tag on the right of the row.
            if item.ready {
                font::draw_text(frame, tx + 40, ry + 2, "READY", [120, 240, 120]);
            } else if let Some(pm) = item.progress {
                let bx0 = x0 + 44;
                let bx1 = w - 4;
                fill_rect(frame, bx0, ry + 3, bx1, ry + 9, [20, 20, 24]);
                let fill = bx0 + (bx1 - bx0) * pm / 1000;
                fill_rect(frame, bx0, ry + 3, fill, ry + 9, [80, 160, 240]);
                font::draw_text(
                    frame,
                    bx0,
                    ry + 11,
                    &format!("{}%", pm / 10),
                    [200, 200, 210],
                );
            }
            ry += SIDEBAR_ROW_H;
        }
    }

    /// Drain queued sim commands emitted since the last call (for the transport
    /// / tests). Terrain-only cores never emit any.
    pub fn drain_commands(&mut self) -> Vec<Command> {
        std::mem::take(&mut self.emitted)
    }
}

/// Convert a lepton coordinate to a terrain-raster pixel coordinate
/// (`CELL_PIXELS` per `LEPTONS_PER_CELL`).
fn leptons_to_pixel(leptons: i32) -> i32 {
    (leptons as i64 * CELL_PIXELS as i64 / LEPTONS_PER_CELL as i64) as i32
}

/// Offset a pixel point by `dist` pixels along a binary-angle facing (for muzzle
/// tips / tracer tails). Uses the sim's own [`ra_sim::coords::coord_move`] on a
/// scaled lepton point so the direction matches the sim exactly — this is
/// presentation only, never fed back into the sim.
fn offset_pixels(x: i32, y: i32, dir: Facing, dist: i32) -> (i32, i32) {
    let leptons = dist * LEPTONS_PER_CELL / CELL_PIXELS;
    let moved = ra_sim::coords::coord_move(WorldCoord::new(0, 0), dir, leptons);
    (
        x + leptons_to_pixel(moved.x.0),
        y + leptons_to_pixel(moved.y.0),
    )
}

/// Draw a bright line between two pixel points (Bresenham), clipped to `dst`.
fn draw_line(dst: &mut RgbaImage, x0: i32, y0: i32, x1: i32, y1: i32, rgb: [u8; 3]) {
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let (mut x, mut y) = (x0, y0);
    loop {
        if x >= 0 && y >= 0 && (x as u32) < dst.width && (y as u32) < dst.height {
            let di = ((y as u32 * dst.width + x as u32) * 4) as usize;
            dst.pixels[di] = rgb[0];
            dst.pixels[di + 1] = rgb[1];
            dst.pixels[di + 2] = rgb[2];
            dst.pixels[di + 3] = 255;
        }
        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core() -> AppCore {
        // 100x80 px map (not cell-aligned; the camera math is pixel-based).
        let raster = IndexedImage::filled(100, 80, 7);
        let mut pal = [[0u8; 3]; 256];
        pal[7] = [10, 20, 30];
        let mut c = AppCore::new(raster, pal);
        c.handle(InputEvent::Resize {
            width: 40,
            height: 30,
        });
        c
    }

    #[test]
    fn arrow_scroll_moves_and_clamps() {
        let mut c = core();
        c.handle(InputEvent::KeyDown(Key::Right));
        c.update(1000); // 1s * 640px/s, but clamped to map-viewport = 60
        assert_eq!(c.camera_rect().x, 60); // 100 - 40
        c.handle(InputEvent::KeyUp(Key::Right));
        c.handle(InputEvent::KeyDown(Key::Left));
        c.update(1000);
        assert_eq!(c.camera_rect().x, 0);
    }

    #[test]
    fn edge_scroll_triggers_on_margin() {
        let mut c = core();
        c.handle(InputEvent::MouseMoved { x: 2, y: 15 }); // near left edge
        c.update(100);
        assert_eq!(c.camera_rect().x, 0); // already at left, clamped
        c.handle(InputEvent::MouseMoved { x: 39, y: 15 }); // near right edge
        c.update(1000);
        assert_eq!(c.camera_rect().x, 60);
    }

    #[test]
    fn compose_size_matches_viewport() {
        let c = core();
        let f = c.compose(Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 30,
        });
        assert_eq!(f.width, 40);
        assert_eq!(f.height, 30);
        assert_eq!(f.pixels.len(), 40 * 30 * 4);
        assert_eq!(&f.pixels[0..4], &[10, 20, 30, 255]);
    }

    #[test]
    fn compose_is_deterministic() {
        let c = core();
        let r = Rect {
            x: 5,
            y: 5,
            width: 40,
            height: 30,
        };
        assert_eq!(c.compose(r).pixels, c.compose(r).pixels);
    }

    #[test]
    fn no_commands_without_units() {
        let mut c = core();
        // Right-clicking with nothing selected emits nothing.
        c.handle(InputEvent::MouseDown {
            button: MouseButton::Right,
            x: 10,
            y: 10,
        });
        assert!(c.drain_commands().is_empty());
    }

    #[test]
    fn resize_is_bounded() {
        let mut c = core();
        c.handle(InputEvent::Resize {
            width: 100_000,
            height: 100_000,
        });
        let (w, h) = c.viewport_size();
        assert!(w <= MAX_VIEWPORT_DIM && h <= MAX_VIEWPORT_DIM);
    }
}
