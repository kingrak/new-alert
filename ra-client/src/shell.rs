//! The macroquad shell — a thin adapter over [`AppCore`] (DESIGN.md §4.8). Its
//! only jobs: translate real device input into [`InputEvent`]s, tick
//! [`AppCore::update`] with the real frame time, and upload
//! [`AppCore::compose_camera`] as a texture. No game/UI behavior lives here, so
//! everything the shell drives is equally reachable from headless tests.
//!
//! Built only with the `window` feature; the headless `--dump` path never
//! touches macroquad.

use macroquad::prelude::*;

use crate::appcore::AppCore;
use crate::input::{InputEvent, Key, MouseButton};

/// Run the windowed viewer. If `smoke_seconds` is `Some(n)`, the window exits
/// automatically after roughly `n` seconds of virtual frame time — a headless
/// CI smoke path (Linux + xvfb) that boots the real shell without needing a
/// human to close it (DESIGN.md §4.8 layer 5). `None` runs until closed.
pub fn run_window(core: AppCore, smoke_seconds: Option<f32>) {
    let conf = Conf {
        window_title: "new-alert — M3 units".to_string(),
        window_width: 1024,
        window_height: 768,
        high_dpi: false,
        ..Default::default()
    };
    macroquad::Window::from_config(conf, amain(core, smoke_seconds));
}

const ARROWS: [(KeyCode, Key); 4] = [
    (KeyCode::Left, Key::Left),
    (KeyCode::Right, Key::Right),
    (KeyCode::Up, Key::Up),
    (KeyCode::Down, Key::Down),
];

async fn amain(mut core: AppCore, smoke_seconds: Option<f32>) {
    let mut last_size = (0u32, 0u32);
    let mut last_mouse = (f32::NAN, f32::NAN);
    let mut elapsed = 0.0f32;

    loop {
        // --- translate input -> InputEvent ---
        let (sw, sh) = (screen_width() as u32, screen_height() as u32);
        if (sw, sh) != last_size {
            last_size = (sw, sh);
            core.handle(InputEvent::Resize {
                width: sw,
                height: sh,
            });
        }

        for (code, key) in ARROWS {
            if is_key_pressed(code) {
                core.handle(InputEvent::KeyDown(key));
            }
            if is_key_released(code) {
                core.handle(InputEvent::KeyUp(key));
            }
        }

        let (mx, my) = mouse_position();
        if (mx, my) != last_mouse {
            last_mouse = (mx, my);
            core.handle(InputEvent::MouseMoved {
                x: mx as i32,
                y: my as i32,
            });
        }

        for (mqbtn, button) in [
            (macroquad::input::MouseButton::Left, MouseButton::Left),
            (macroquad::input::MouseButton::Right, MouseButton::Right),
        ] {
            if is_mouse_button_pressed(mqbtn) {
                core.handle(InputEvent::MouseDown {
                    button,
                    x: mx as i32,
                    y: my as i32,
                });
            }
            if is_mouse_button_released(mqbtn) {
                core.handle(InputEvent::MouseUp {
                    button,
                    x: mx as i32,
                    y: my as i32,
                });
            }
        }

        if is_key_pressed(KeyCode::Escape) {
            break;
        }

        // --- tick with real frame time (virtual-time API, real dt here) ---
        let dt = get_frame_time();
        elapsed += dt;
        if let Some(limit) = smoke_seconds {
            if elapsed >= limit {
                break;
            }
        }
        let dt_ms = (dt * 1000.0) as u32;
        core.update(dt_ms);

        // --- compose and upload ---
        let frame = core.compose_camera();
        let tex = Texture2D::from_rgba8(frame.width as u16, frame.height as u16, &frame.pixels);
        tex.set_filter(FilterMode::Nearest);
        clear_background(BLACK);
        draw_texture(&tex, 0.0, 0.0, WHITE);

        next_frame().await;
    }
}
