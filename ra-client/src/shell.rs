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
use crate::input::{InputEvent, Key};

/// Run the windowed terrain viewer until the window is closed.
pub fn run_window(core: AppCore) {
    let conf = Conf {
        window_title: "new-alert — M2 terrain".to_string(),
        window_width: 1024,
        window_height: 768,
        high_dpi: false,
        ..Default::default()
    };
    macroquad::Window::from_config(conf, amain(core));
}

const ARROWS: [(KeyCode, Key); 4] = [
    (KeyCode::Left, Key::Left),
    (KeyCode::Right, Key::Right),
    (KeyCode::Up, Key::Up),
    (KeyCode::Down, Key::Down),
];

async fn amain(mut core: AppCore) {
    let mut last_size = (0u32, 0u32);
    let mut last_mouse = (f32::NAN, f32::NAN);

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

        if is_key_pressed(KeyCode::Escape) {
            break;
        }

        // --- tick with real frame time (virtual-time API, real dt here) ---
        let dt_ms = (get_frame_time() * 1000.0) as u32;
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
