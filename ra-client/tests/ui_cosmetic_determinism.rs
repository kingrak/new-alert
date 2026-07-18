//! The §4.2 sim-vs-cosmetic split, made testable (M7). The client's cosmetic
//! layer — ore/gem art, the explosion/buildup animation layer, and the audio cue
//! queue — is derived from sim state and driven by a *virtual* clock; it must
//! never perturb the simulation. This test pins that: the SAME input timeline
//! run with the cosmetic layer fully ON (art installed, `compose_game` composited
//! and `drain_sounds` drained every frame, so effects are spawned and cues fire)
//! yields a byte-identical sim-hash chain to the layer fully OFF (art stripped,
//! nothing composed or drained).
//!
//! Skip-clean without assets, like the other real-asset UI tests.
//!
//! **M7 extension**: the real-scenario test above lets the skirmish AI run
//! freely for 300 ticks, which *may* produce combat but doesn't guarantee it
//! (and skips entirely without real assets). `combat_heavy_script_...` below
//! adds a second, always-runs (no assets needed) variant that *forces* heavy
//! combat deterministically — three armed jeeps ordered to attack a single
//! target through the real UI click seam (`support::synthetic_core_with_armed_units`,
//! the same fixture/click sequence `ui_scripted_drive.rs`'s
//! `synthetic_battle_attack_kill_and_health_bar_rendering` uses) — with fake
//! explosion art installed so the death animation, `SoundEvent::Fire`/
//! `SoundEvent::Explosion` cues, and the effect-spawning path in
//! `compose_game()` are all genuinely exercised on the "on" side, not just
//! present-but-inert.

mod support;

use std::path::{Path, PathBuf};

use ra_client::unit_render::{SpriteFrame, UnitSprite};
use ra_client::AppCore;

fn assets_dir() -> Option<PathBuf> {
    let dir = std::env::var("RA_ASSETS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../assets"));
    (dir.join("main.mix").is_file() && dir.join("redalert.mix").is_file()).then_some(dir)
}

/// Load a skirmish core, or `None` if assets/scenario are unavailable.
fn load_core(dir: &Path) -> Option<AppCore> {
    ra_client::assets::load_skirmish_from_dir(dir, "scm01ea.ini", 8000, ra_sim::Difficulty::Normal)
        .ok()
        .map(|g| g.core)
}

#[test]
fn cosmetic_layer_on_vs_off_yields_identical_sim_hashes() {
    let Some(dir) = assets_dir() else {
        eprintln!("SKIP: assets not found");
        return;
    };
    let (Some(mut on), Some(mut off)) = (load_core(&dir), load_core(&dir)) else {
        eprintln!("SKIP: scenario unavailable");
        return;
    };

    // Strip all cosmetic art from the "off" core: with no explosion/buildup art
    // installed, effect spawning no-ops; with no ore/cameo art, nothing extra is
    // drawn (and we won't draw it anyway).
    off.set_effect_art(Vec::new(), Vec::new());
    off.set_ore_art(Vec::new(), Vec::new());
    off.set_cameo_art(Vec::new());

    // Give both a real viewport so `compose_game` on the "on" core does real work.
    use ra_client::InputEvent;
    for c in [&mut on, &mut off] {
        c.handle(InputEvent::Resize {
            width: 640,
            height: 400,
        });
    }

    let mut hashes_on = Vec::new();
    let mut hashes_off = Vec::new();
    for _ in 0..300 {
        // "On": advance, then exercise the entire cosmetic pipeline.
        on.update(67);
        let _frame = on.compose_game(); // spawns/draws effects, radar, ore art
        let _cues = on.drain_sounds(); // consume queued audio cues
        hashes_on.push(on.sim_hash());

        // "Off": advance only — never compose, never drain.
        off.update(67);
        hashes_off.push(off.sim_hash());
    }

    assert_eq!(
        hashes_on, hashes_off,
        "cosmetic layer (art + anims + audio + compositing) perturbed the sim hash chain"
    );
    // Sanity: the sim actually did something over 800 ticks (not a trivial pass).
    assert!(
        hashes_on.windows(2).any(|w| w[0] != w[1]),
        "sim never advanced — determinism check was vacuous"
    );
}

/// A tiny, deterministic stand-in for a decoded `FBALL1.SHP` explosion frame
/// (mirrors `support.rs`'s private `fake_cameo_sprite`, reimplemented here
/// since it isn't `pub`): a single non-zero-indexed frame, so the effect
/// actually paints something instead of being installed-but-invisible.
fn fake_explosion_sprite() -> UnitSprite {
    UnitSprite {
        frames: vec![SpriteFrame {
            width: 24,
            height: 24,
            pixels: vec![7u8; 24 * 24],
        }],
    }
}

#[test]
fn combat_heavy_script_cosmetic_layer_on_vs_off_yields_identical_sim_hashes() {
    let seed = 0xC0BA_7000;
    let mut on = {
        let (raster, palette) = support::synthetic_fixture();
        let (world, _jeeps, _target) = support::synthetic_world_with_armed_units(seed);
        ra_client::AppCore::with_sim(raster.clone(), *palette, world, Vec::new(), Vec::new())
    };
    let mut off = {
        let (raster, palette) = support::synthetic_fixture();
        let (world, _jeeps, _target) = support::synthetic_world_with_armed_units(seed);
        ra_client::AppCore::with_sim(raster.clone(), *palette, world, Vec::new(), Vec::new())
    };

    // "on": real (fake but non-empty) explosion art installed, so death
    // triggers an actual animated effect, not a no-op.
    on.set_effect_art(vec![fake_explosion_sprite()], Vec::new());
    // "off": no cosmetic art at all (matches the baseline test above).
    off.set_effect_art(Vec::new(), Vec::new());

    use ra_client::input::{InputEvent, Key, MouseButton};
    const CELL_PIXELS: i32 = 24;
    for c in [&mut on, &mut off] {
        c.handle(InputEvent::Resize {
            width: 480,
            height: 320,
        });
        c.set_camera(0.0, 0.0);
    }

    // Box-select the 3 jeeps and right-click the target (identical script on
    // both cores — same seed, same fixture layout as
    // `ui_scripted_drive.rs`'s `synthetic_battle_attack_kill_and_health_bar_rendering`).
    let target_sx = 16 * CELL_PIXELS + CELL_PIXELS / 2;
    let target_sy = 10 * CELL_PIXELS + CELL_PIXELS / 2;
    for c in [&mut on, &mut off] {
        c.handle(InputEvent::MouseDown {
            button: MouseButton::Left,
            x: 0,
            y: 0,
        });
        c.handle(InputEvent::MouseMoved { x: 370, y: 280 });
        c.handle(InputEvent::MouseUp {
            button: MouseButton::Left,
            x: 370,
            y: 280,
        });
        assert_eq!(c.selected_handles().len(), 3, "sanity: 3 jeeps selected");
        c.handle(InputEvent::MouseDown {
            button: MouseButton::Right,
            x: target_sx,
            y: target_sy,
        });
    }

    // Also toggle the F1 overlay and issue a stray key mid-run on the "on"
    // core only, to confirm even unrelated UI/cosmetic churn never perturbs
    // the hash chain (F1 is purely `compose_game()`-side presentation).
    on.handle(InputEvent::KeyDown(Key::Help));
    on.handle(InputEvent::KeyUp(Key::Help));

    let mut hashes_on = Vec::new();
    let mut hashes_off = Vec::new();
    let mut saw_explosion_cue = false;
    let mut saw_fire_cue = false;
    for i in 0..400 {
        on.update(67);
        let _frame = on.compose_game(); // spawns/draws the death-explosion effect
        for cue in on.drain_sounds() {
            match cue {
                ra_client::appcore::SoundEvent::Explosion => saw_explosion_cue = true,
                ra_client::appcore::SoundEvent::Fire => saw_fire_cue = true,
                _ => {}
            }
        }
        hashes_on.push(on.sim_hash());

        off.update(67);
        hashes_off.push(off.sim_hash());

        // Re-toggle F1 on the "on" core periodically: more cosmetic churn.
        if i % 97 == 0 {
            on.handle(InputEvent::KeyDown(Key::Help));
            on.handle(InputEvent::KeyUp(Key::Help));
        }
    }

    assert_eq!(
        hashes_on, hashes_off,
        "a combat-heavy script (fire + a kill + explosion anim + sound cues + F1 churn) \
         must not perturb the sim hash chain vs. the same script with cosmetics off"
    );
    assert!(
        saw_fire_cue,
        "sanity: this script should have fired at least one shot"
    );
    assert!(
        saw_explosion_cue,
        "sanity: this script should have killed the unarmed target (explosion cue)"
    );
}
