//! `AppCore` — the windowless heart of the client (DESIGN.md §4.8). All UI
//! behavior lives here so every corner of it is reachable from tests without a
//! window: feed it [`InputEvent`]s, advance it with virtual time via
//! [`AppCore::update`], and read pixels back with [`AppCore::compose`]. The
//! macroquad shell is only an adapter over this seam.
//!
//! For M2 the only behavior is the terrain camera: arrow-key and screen-edge
//! scrolling over a pre-rasterized indexed-color map, clamped to the map bounds.

use crate::compositor::{viewport_rgba, IndexedImage, Palette, RgbaImage};
use crate::input::{InputEvent, Key, Rect};

/// The composed output of a frame — an RGBA image ready to upload as a texture.
pub type Frame = RgbaImage;

/// Sim commands the UI would emit. None exist yet at M2 (no sim), so this is
/// intentionally uninhabited; [`AppCore::drain_commands`] always yields empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {}

/// Default camera scroll speed, in map pixels per second.
const DEFAULT_SCROLL_SPEED: f32 = 640.0;
/// Distance from a viewport edge (pixels) within which the pointer edge-scrolls.
const EDGE_MARGIN: i32 = 16;

/// The windowless client core: owns the terrain raster, palette, and camera.
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
}

impl AppCore {
    /// Build a core over a pre-rasterized indexed terrain image and its palette.
    /// The camera starts at the map origin with a default viewport size.
    pub fn new(raster: IndexedImage, palette: Palette) -> AppCore {
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

    /// Handle one input event.
    pub fn handle(&mut self, ev: InputEvent) {
        match ev {
            InputEvent::KeyDown(k) => self.set_key(k, true),
            InputEvent::KeyUp(k) => self.set_key(k, false),
            InputEvent::MouseMoved { x, y } => {
                self.mouse_x = x;
                self.mouse_y = y;
                self.mouse_inside = true;
            }
            InputEvent::MouseLeft => self.mouse_inside = false,
            InputEvent::Resize { width, height } => {
                self.viewport_w = width.max(1);
                self.viewport_h = height.max(1);
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

    /// Advance the camera by `dt_ms` milliseconds of virtual time.
    pub fn update(&mut self, dt_ms: u32) {
        let (dx, dy) = self.scroll_direction();
        if dx != 0.0 || dy != 0.0 {
            let dt = dt_ms as f32 / 1000.0;
            self.cam_x += dx * self.scroll_speed * dt;
            self.cam_y += dy * self.scroll_speed * dt;
            self.clamp_camera();
        }
    }

    /// Unit-ish scroll direction from held keys plus pointer edge scrolling.
    /// Each axis is clamped to [-1, 1]; diagonal scroll is intentionally faster,
    /// matching the original's per-axis edge scroll.
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

    /// Clamp the camera so the viewport stays within the map. If the viewport is
    /// larger than the map on an axis, the camera pins to 0 on that axis.
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

    /// Composite an arbitrary map-space rectangle to RGBA. Pure: no camera state
    /// is read, so tests can sweep the whole map independent of the camera.
    pub fn compose(&self, viewport: Rect) -> Frame {
        viewport_rgba(
            &self.raster,
            &self.palette,
            viewport.x,
            viewport.y,
            viewport.width,
            viewport.height,
        )
    }

    /// Composite at the current camera position (shell convenience).
    pub fn compose_camera(&self) -> Frame {
        self.compose(self.camera_rect())
    }

    /// Drain queued sim commands. Always empty at M2 (no sim yet).
    pub fn drain_commands(&mut self) -> Vec<Command> {
        Vec::new()
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
                                           // Releasing and pressing left returns to origin and clamps at 0.
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
    fn no_commands_at_m2() {
        let mut c = core();
        assert!(c.drain_commands().is_empty());
    }
}
