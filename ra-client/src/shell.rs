//! The macroquad shell — a thin adapter over [`AppCore`] (DESIGN.md §4.8). Its
//! only jobs: translate real device input into [`InputEvent`]s, tick
//! [`AppCore::update`] with the real frame time, and upload
//! [`AppCore::compose_camera`] as a texture. No game/UI behavior lives here, so
//! everything the shell drives is equally reachable from headless tests.
//!
//! Built only with the `window` feature; the headless `--dump` path never
//! touches macroquad.

use macroquad::prelude::*;

use crate::appcore::{AppCore, SoundEvent};
use crate::input::{InputEvent, Key, MouseButton};

/// Playback volume for cosmetic sound cues (0.0..=1.0). Kept modest so EVA lines
/// and SFX layer without clipping.
#[cfg(feature = "audio")]
const SOUND_VOLUME: f32 = 0.5;

/// Run the windowed viewer. If `smoke_seconds` is `Some(n)`, the window exits
/// automatically after roughly `n` seconds of virtual frame time — a headless
/// CI smoke path (Linux + xvfb) that boots the real shell without needing a
/// human to close it (DESIGN.md §4.8 layer 5). `None` runs until closed.
///
/// `sounds` is the decoded WAV sound bank (event → WAV bytes). It is ignored
/// unless the `audio` feature is on; even then, a failed decode/device is a
/// silent skip — audio never crashes the game.
pub fn run_window(core: AppCore, smoke_seconds: Option<f32>, sounds: Vec<(SoundEvent, Vec<u8>)>) {
    let conf = Conf {
        window_title: "new-alert — M7 polish".to_string(),
        window_width: 1024,
        window_height: 768,
        high_dpi: false,
        ..Default::default()
    };
    macroquad::Window::from_config(conf, amain(core, smoke_seconds, sounds));
}

const ARROWS: [(KeyCode, Key); 4] = [
    (KeyCode::Left, Key::Left),
    (KeyCode::Right, Key::Right),
    (KeyCode::Up, Key::Up),
    (KeyCode::Down, Key::Down),
];

async fn amain(mut core: AppCore, smoke_seconds: Option<f32>, sounds: Vec<(SoundEvent, Vec<u8>)>) {
    let mut last_size = (0u32, 0u32);
    let mut last_mouse = (f32::NAN, f32::NAN);
    let mut elapsed = 0.0f32;

    // Load the sound bank into the audio device (best-effort; any failure is a
    // silent skip so audio can never crash the game).
    #[cfg(feature = "audio")]
    let sound_bank: Vec<(SoundEvent, macroquad::audio::Sound)> = {
        let mut m = Vec::new();
        for (ev, wav) in &sounds {
            if let Ok(s) = macroquad::audio::load_sound_from_bytes(wav).await {
                m.push((*ev, s));
            }
        }
        m
    };
    #[cfg(not(feature = "audio"))]
    let _ = &sounds;

    // Show the controls hint briefly at boot; hidden after this many seconds of
    // real time (F1 toggles it thereafter).
    core.set_help_visible(true);
    let mut intro_help_hidden = false;
    const INTRO_HELP_SECONDS: f32 = 6.0;

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

        // Deploy the selected MCV (M5): the 'D' key, edge-triggered.
        if is_key_pressed(KeyCode::D) {
            core.handle(InputEvent::KeyDown(Key::Deploy));
            core.handle(InputEvent::KeyUp(Key::Deploy));
        }

        // Toggle the controls-hint overlay (M7): F1, edge-triggered.
        if is_key_pressed(KeyCode::F1) {
            core.handle(InputEvent::KeyDown(Key::Help));
            core.handle(InputEvent::KeyUp(Key::Help));
        }

        if is_key_pressed(KeyCode::Escape) {
            break;
        }

        // --- tick with real frame time (virtual-time API, real dt here) ---
        let dt = get_frame_time();
        elapsed += dt;
        // Auto-hide the intro controls hint once, after the intro window.
        if !intro_help_hidden && elapsed >= INTRO_HELP_SECONDS {
            core.set_help_visible(false);
            intro_help_hidden = true;
        }
        if let Some(limit) = smoke_seconds {
            if elapsed >= limit {
                break;
            }
        }
        let dt_ms = (dt * 1000.0) as u32;
        core.update(dt_ms);

        // --- play any queued sound cues (feature-gated; always drained so the
        //     queue can't grow unbounded even in a no-audio build) ---
        let cues = core.drain_sounds();
        #[cfg(feature = "audio")]
        for ev in cues {
            if let Some((_, sound)) = sound_bank.iter().find(|(e, _)| *e == ev) {
                macroquad::audio::play_sound(
                    sound,
                    macroquad::audio::PlaySoundParams {
                        looped: false,
                        volume: SOUND_VOLUME,
                    },
                );
            }
        }
        #[cfg(not(feature = "audio"))]
        let _ = cues;

        // --- compose and upload ---
        let frame = core.compose_camera();
        let tex = Texture2D::from_rgba8(frame.width as u16, frame.height as u16, &frame.pixels);
        tex.set_filter(FilterMode::Nearest);
        clear_background(BLACK);
        draw_texture(&tex, 0.0, 0.0, WHITE);

        next_frame().await;
    }
}
