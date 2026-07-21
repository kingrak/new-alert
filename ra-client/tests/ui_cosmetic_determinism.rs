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

/// Sell/repair effects (deconstruct anim + cash/EVA/repair cues + mode cursor +
/// wrench overlay + banner) are cosmetic: firing them must not perturb the sim
/// hash chain. Both cores issue the **same** sim commands (Sell/Repair are real
/// sim input); only the "on" core composes/drains the cosmetic layer.
#[test]
fn sell_repair_cosmetic_layer_on_vs_off_yields_identical_sim_hashes() {
    use ra_client::input::InputEvent;
    use ra_client::unit_render::{SpriteFrame, UnitSprite};
    use ra_sim::coords::CellCoord;

    let build = || {
        let (mut world, _mcv) = support::synthetic_world_with_econ(0x5E11_D077, 6000);
        let a = world
            .spawn_building(support::ECON_B_PROC, 1, CellCoord::new(2, 2))
            .unwrap();
        let b = world
            .spawn_building(support::ECON_B_PROC, 1, CellCoord::new(10, 2))
            .unwrap();
        // Damage the second so repair has work; the first will be sold.
        let max = world.buildings.get(b).unwrap().max_health;
        world.buildings.get_mut(b).unwrap().health = max / 2;
        let (raster, palette) = support::synthetic_fixture();
        let mut core = AppCore::with_sim(raster.clone(), *palette, world, Vec::new(), Vec::new());
        core.enable_sidebar(1, support::econ_buildables());
        core.handle(InputEvent::Resize {
            width: 640,
            height: 400,
        });
        core.set_camera(0.0, 0.0);
        (core, a, b)
    };
    let (mut on, on_sell, on_rep) = build();
    let (mut off, off_sell, off_rep) = build();
    // "on": real (fake) buildup art so the deconstruct effect actually spawns.
    let mut buildups: Vec<Option<UnitSprite>> = vec![None; (support::ECON_B_PROC + 1) as usize];
    buildups[support::ECON_B_PROC as usize] = Some(UnitSprite {
        frames: (0..5)
            .map(|_| SpriteFrame {
                width: 24,
                height: 24,
                pixels: vec![9u8; 24 * 24],
            })
            .collect(),
    });
    on.set_effect_art(vec![fake_explosion_sprite()], buildups);

    // Identical command scripts on both cores (Repair the damaged PROC, then
    // Sell the other) — Sell/Repair are real sim input, applied to both.
    for (core, sell, rep) in [(&mut on, on_sell, on_rep), (&mut off, off_sell, off_rep)] {
        core.inject_command(ra_sim::Command::Repair {
            house: 1,
            building: rep,
        });
        core.update(support::TICK_MS);
        core.inject_command(ra_sim::Command::Sell {
            house: 1,
            building: sell,
        });
    }

    let mut h_on = Vec::new();
    let mut h_off = Vec::new();
    for _ in 0..120 {
        on.update(support::TICK_MS);
        let _ = on.compose_game();
        let _ = on.drain_sounds();
        h_on.push(on.sim_hash());

        off.update(support::TICK_MS);
        h_off.push(off.sim_hash());
    }
    assert_eq!(
        h_on, h_off,
        "sell/repair cosmetic effects perturbed the sim hash chain"
    );
    assert!(
        h_on.windows(2).any(|w| w[0] != w[1]),
        "sim never advanced — determinism check was vacuous"
    );
}

/// A fake multi-facing aircraft body sprite (32 frames) so the lifted body,
/// ground shadow, and turret paths all resolve a real frame on the "on" core.
fn fake_aircraft_sprite() -> UnitSprite {
    UnitSprite {
        frames: (0..32)
            .map(|_| SpriteFrame {
                width: 24,
                height: 24,
                pixels: vec![5u8; 24 * 24],
            })
            .collect(),
    }
}

/// A fake RROTOR.SHP: 12 frames (0..4 fast/airborne, 4..12 slow/landed).
fn fake_rotor_sprite() -> UnitSprite {
    UnitSprite {
        frames: (0..12)
            .map(|_| SpriteFrame {
                width: 24,
                height: 24,
                pixels: vec![3u8; 24 * 24],
            })
            .collect(),
    }
}

/// M7.17 audit (ra-tester): aircraft rendering — altitude-lift, ground shadow,
/// spinning/idle rotor, AA tracer aim-lift, and the LIFTED crash fireball — is
/// entirely cosmetic and must not perturb the sim. The SAME input timeline run
/// with the full aircraft cosmetic layer ON (aircraft/rotor art installed,
/// `compose_game` composited and sounds drained every frame — so the lifted
/// body, shadow, rotor stage, and crash-explosion effect are genuinely
/// exercised) must yield a byte-identical sim-hash chain to the layer OFF.
///
/// Always runs (no assets): a synthetic world with two helicopters — one
/// airborne (downed by an enemy AA gun → lifted crash fireball on the "on"
/// side) and one docked/landed (idle-rotor branch) — plus the AA gun that
/// tracers up at the flying craft.
#[test]
fn aircraft_render_cosmetic_layer_on_vs_off_yields_identical_sim_hashes() {
    use ra_client::input::InputEvent;
    use ra_sim::coords::{CellCoord, Facing};
    use ra_sim::{
        BuildingProto, Catalog, EconRules, MoveStats, Passability, WarheadProfile, WeaponProfile,
        World,
    };

    fn weapon(damage: i32, rof: u16, range: i32) -> WeaponProfile {
        WeaponProfile {
            damage,
            rof,
            range,
            proj_speed: 255,
            proj_rot: 0,
            invisible: false, // visible tracer → exercises the AA aim-lift render
            instant: false,
            warhead: WarheadProfile {
                spread: 1000,
                verses: [65536; 5],
            },
            warhead_ap: false,
            arcing: false,
            ballistic_scatter: 256,
            homing_scatter: 512,
            min_damage: 1,
            max_damage: 1000,
        }
    }
    fn bproto(name: &str, w: u8, h: u8, wpn: Option<WeaponProfile>) -> BuildingProto {
        BuildingProto {
            name: name.to_string(),
            foot_w: w,
            foot_h: h,
            max_health: 400,
            armor: 0,
            power: 0,
            cost: 400,
            prereq: vec![],
            is_refinery: false,
            is_construction_yard: false,
            is_war_factory: false,
            is_barracks: false,
            free_harvester_unit: None,
            sight: 6,
            sprite_id: 0,
            weapon: wpn,
            has_turret: false,
            charges: false,
            is_wall: false,
            storage: 0,
        }
    }

    let build = || {
        let mut world = World::new(Passability::all_passable(), 0xA112_C0DE);
        world.set_catalog(Catalog {
            buildings: vec![
                bproto("HPAD", 2, 2, None),
                bproto("AGUN", 1, 2, Some(weapon(20, 12, 4096))),
            ],
            units: vec![],
            econ: EconRules::default(),
        });
        world.init_houses(3, 1000);
        let air_stats = MoveStats {
            max_speed: 40,
            rot: 18,
        };
        // Enemy AA gun that downs the airborne heli.
        world.spawn_building(1, 2, CellCoord::new(20, 20)).unwrap();
        // Airborne heli (house 1) hovering in range — gets shot down (crash lift).
        let flyer = world.spawn_unit(0, 1, CellCoord::new(23, 20), Facing(0), 40, air_stats);
        world.set_unit_combat(flyer, 0, Some(weapon(0, 200, 1)), false);
        world.units.get_mut(flyer).unwrap().make_aircraft(6);
        // Docked heli (house 1) parked on a pad far away (idle-rotor branch).
        let pad = world.spawn_building(0, 1, CellCoord::new(40, 40)).unwrap();
        let parked = world.spawn_unit(0, 1, CellCoord::new(40, 40), Facing(0), 200, air_stats);
        world.set_unit_combat(parked, 0, Some(weapon(0, 200, 1)), false);
        {
            let u = world.units.get_mut(parked).unwrap();
            u.make_aircraft(6);
            u.altitude = 0;
            u.home = Some(pad);
        }
        let (raster, palette) = support::synthetic_fixture();
        let mut core = AppCore::with_sim(
            raster.clone(),
            *palette,
            world,
            vec![fake_aircraft_sprite()],
            Vec::new(),
        );
        core.handle(InputEvent::Resize {
            width: 640,
            height: 400,
        });
        core.set_camera(0.0, 0.0);
        core
    };

    let mut on = build();
    let mut off = build();

    // "on": full aircraft cosmetic layer — rotor art + crash-explosion art.
    on.set_rotor_art(Some(fake_rotor_sprite()));
    on.set_effect_art(vec![fake_explosion_sprite()], Vec::new());
    // "off": strip every cosmetic input.
    off.set_rotor_art(None);
    off.set_effect_art(Vec::new(), Vec::new());
    off.set_ore_art(Vec::new(), Vec::new());
    off.set_cameo_art(Vec::new());

    let mut h_on = Vec::new();
    let mut h_off = Vec::new();
    for _ in 0..400 {
        on.update(67);
        let _frame = on.compose_game(); // lifts body/shadow/rotor; spawns crash fireball
        let _cues = on.drain_sounds();
        h_on.push(on.sim_hash());

        off.update(67);
        h_off.push(off.sim_hash());
    }

    assert_eq!(
        h_on, h_off,
        "aircraft rendering (altitude lift + shadow + rotor + crash fireball + AA \
         tracer aim-lift) perturbed the sim hash chain"
    );
    assert!(
        h_on.windows(2).any(|w| w[0] != w[1]),
        "sim never advanced — the aircraft cosmetic-determinism check was vacuous"
    );
}
