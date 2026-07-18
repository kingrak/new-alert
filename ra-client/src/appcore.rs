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

use std::collections::{BTreeMap, BTreeSet};

use ra_data::house::{identity_remap, RemapTable};
use ra_formats::tmpl::ICON_WIDTH;
use ra_sim::coords::{CellCoord, WorldCoord, LEPTONS_PER_CELL};
use ra_sim::{Handle, Passability, World};

use crate::compositor::{viewport_rgba, IndexedImage, Palette, RgbaImage};
use crate::input::{InputEvent, Key, MouseButton, Rect};
use crate::unit_render::{draw_rect_outline, draw_sprite_centered, UnitSprite};

/// Sim commands the UI emits. Re-exported from the sim so the whole app speaks
/// one command vocabulary (DESIGN.md §4.4).
pub use ra_sim::Command;

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
    selected: BTreeSet<u32>,
    /// Active left-drag selection box, if any.
    drag: Option<DragBox>,

    /// Commands queued for the next sim tick (loopback pipeline).
    pending: Vec<Command>,
    /// Commands emitted since the last [`AppCore::drain_commands`] (for the net
    /// layer / tests to observe).
    emitted: Vec<Command>,
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
            selected: BTreeSet::new(),
            drag: None,
            pending: Vec::new(),
            emitted: Vec::new(),
        }
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

    /// The handles of currently-selected units (ascending slot order).
    pub fn selected_handles(&self) -> Vec<Handle> {
        self.world
            .units
            .handles()
            .into_iter()
            .filter(|h| self.selected.contains(&h.index))
            .collect()
    }

    /// Handle one input event.
    pub fn handle(&mut self, ev: InputEvent) {
        match ev {
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
                    self.drag = Some(DragBox {
                        start: (x, y),
                        cur: (x, y),
                    })
                }
                MouseButton::Right => self.issue_move(x, y),
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
        if self.mouse_inside {
            if self.mouse_x >= 0 && self.mouse_x < EDGE_MARGIN {
                dx -= 1.0;
            } else if self.mouse_x >= self.viewport_w as i32 - EDGE_MARGIN {
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

    /// Clamp the camera so the viewport stays within the map.
    fn clamp_camera(&mut self) {
        let max_x = (self.raster.width as f32 - self.viewport_w as f32).max(0.0);
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

    /// The clamped viewport rectangle at the current camera position.
    pub fn camera_rect(&self) -> Rect {
        Rect {
            x: self.cam_x.round() as i64,
            y: self.cam_y.round() as i64,
            width: self.viewport_w,
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
    pub fn compose_camera(&self) -> Frame {
        let rect = self.camera_rect();
        let mut frame = self.compose(rect);
        if let Some(d) = &self.drag {
            draw_rect_outline(
                &mut frame, d.start.0, d.start.1, d.cur.0, d.cur.1, SELECT_RGB,
            );
        }
        frame
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

            if let Some(sprite) = self.sprites.get(unit.type_id as usize) {
                if let Some(sframe) = sprite.frame_for(unit.facing) {
                    let remap = self
                        .remaps
                        .get(unit.house as usize)
                        .copied()
                        .unwrap_or_else(identity_remap);
                    draw_sprite_centered(frame, sx, sy, sframe, &remap, &self.palette);
                }
            }

            if self.selected.contains(&h.index) {
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
            // Click: pick the nearest unit within PICK_RADIUS.
            let mut best: Option<(i64, u32)> = None;
            for (h, unit) in self.world.units.iter() {
                let px = leptons_to_pixel(unit.coord.x.0) as i64;
                let py = leptons_to_pixel(unit.coord.y.0) as i64;
                let d2 = (px - sx0) * (px - sx0) + (py - sy0) * (py - sy0);
                if d2 <= (PICK_RADIUS as i64) * (PICK_RADIUS as i64)
                    && best.map(|(bd, _)| d2 < bd).unwrap_or(true)
                {
                    best = Some((d2, h.index));
                }
            }
            if let Some((_, idx)) = best {
                self.selected.insert(idx);
            }
            return;
        }

        let (xa, xb) = (sx0.min(sx1), sx0.max(sx1));
        let (ya, yb) = (sy0.min(sy1), sy0.max(sy1));
        for (h, unit) in self.world.units.iter() {
            let px = leptons_to_pixel(unit.coord.x.0) as i64;
            let py = leptons_to_pixel(unit.coord.y.0) as i64;
            if px >= xa && px <= xb && py >= ya && py <= yb {
                self.selected.insert(h.index);
            }
        }
    }

    /// Issue a move order for every selected unit toward the clicked cell.
    fn issue_move(&mut self, x: i32, y: i32) {
        if self.selected.is_empty() {
            return;
        }
        let (mx, my) = self.viewport_to_map(x, y);
        let dest = CellCoord::new(
            (mx / CELL_PIXELS as i64) as i32,
            (my / CELL_PIXELS as i64) as i32,
        );
        // Collect live selected handles first (borrow discipline).
        let orders: Vec<(Handle, u8)> = self
            .world
            .units
            .handles()
            .into_iter()
            .filter(|h| self.selected.contains(&h.index))
            .filter_map(|h| self.world.units.get(h).map(|u| (h, u.house)))
            .collect();
        for (unit, house) in orders {
            let cmd = Command::Move { unit, dest, house };
            self.pending.push(cmd);
            self.emitted.push(cmd);
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
