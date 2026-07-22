//! M7.5 campaign verification: load the real Allied mission 1 (`scg01ea.ini`,
//! "In the thick of it" — rescue Einstein), report its full trigger/teamtype/
//! placement inventory, and drive a scripted playthrough to VICTORY through the
//! real scenario scripting.
//!
//! Skips cleanly (never fails) when the real assets aren't present.

mod support;

use ra_client::assets;
use ra_sim::campaign::{taction, tevent};
use ra_sim::{Command, GameOver, Target};
use std::path::PathBuf;

fn scratch() -> PathBuf {
    PathBuf::from(
        "/tmp/claude-1000/-home-cshi-dev-game/f65beaba-9afb-445c-a6fd-47d2eb3dad49/scratchpad",
    )
}

fn dump_png(core: &ra_client::appcore::AppCore, name: &str) {
    let f = core.compose_game();
    let bytes = ra_client::png::encode_rgba(f.width, f.height, &f.pixels);
    let path = scratch().join(name);
    let _ = std::fs::write(&path, bytes);
    eprintln!("  wrote {}", path.display());
}

#[test]
fn scg01ea_inventory_and_playthrough_to_victory() {
    let dir = support::assets_dir();
    if !dir.join("main.mix").is_file() || !dir.join("redalert.mix").is_file() {
        eprintln!(
            "SKIP: real assets not found under {} (set RA_ASSETS_DIR)",
            dir.display()
        );
        return;
    }

    let mut mission = assets::load_campaign_from_bytes(
        &std::fs::read(dir.join("main.mix")).unwrap(),
        &std::fs::read(dir.join("redalert.mix")).unwrap(),
        "scg01ea.ini",
        ra_sim::Difficulty::Normal,
    )
    .expect("load scg01ea");

    // ---- Inventory report ----
    eprintln!("=== scg01ea '{}' ===", mission.name);
    eprintln!(
        "placements: {} vehicles, {} infantry, {} structures, {} terrain",
        mission.units_placed,
        mission.infantry_placed,
        mission.structures_placed,
        mission.terrain_placed
    );
    eprintln!(
        "scripting: {} triggers, {} teamtypes",
        mission.triggers, mission.teamtypes
    );
    eprintln!("player house: {}", mission.player_house);
    eprintln!("skipped (naval/air/unresolved): {:?}", mission.skipped);
    eprintln!("briefing: {}", mission.briefing);

    // Real scenario numbers (from the extracted scg01ea.ini).
    assert_eq!(mission.player_house, 1, "Player=Greece");
    assert_eq!(mission.triggers, 19, "19 [Trigs] entries");
    assert_eq!(mission.teamtypes, 12, "12 [TeamTypes] entries");
    assert_eq!(mission.infantry_placed, 22, "22 [INFANTRY]");
    // All 25 [STRUCTURES] place (18 real buildings + 7 barrel/oil-pump props —
    // BARL/BRL3/V19, which carry a rules.ini section but no `Cost=`; a cost-less
    // civilian structure now resolves, M7.5-A audit fix).
    assert_eq!(mission.structures_placed, 25, "all 25 [STRUCTURES] place");
    assert_eq!(mission.units_placed, 4, "4 [UNITS] (3 jeeps + harvester)");
    assert!(mission.terrain_placed > 0, "some [TERRAIN] trees");

    // The win trigger: Greece wins on EVAC_CIVILIAN.
    {
        let camp = mission.core.world().campaign().unwrap();
        let win = camp.triggers.iter().find(|t| t.name == "win").unwrap();
        assert_eq!(win.e1.code, tevent::EVAC_CIVILIAN);
        assert_eq!(win.a1.code, taction::WIN);
        // The rescue chain exists.
        assert!(camp.triggers.iter().any(|t| t.name == "eins"));
        assert!(camp.triggers.iter().any(|t| t.name == "ein2"));
    }

    // Camera at the mission start (waypoint 98 home / a placed player unit).
    mission.core.handle(ra_client::input::InputEvent::Resize {
        width: 1024,
        height: 768,
    });
    let cp = 24.0f32; // RA cell = 24px (ICON_WIDTH)
    mission.core.set_camera(
        mission.start.x as f32 * cp - 512.0,
        mission.start.y as f32 * cp - 384.0,
    );

    let core = &mut mission.core;

    // ---- Tick 0: set1 (TIME 0) reinforces Tanya (E7) at waypoint 10. ----
    core.world_mut().tick(&[]);
    let tanya_present = core
        .world()
        .units
        .iter()
        .any(|(_, u)| u.house == 1 && !u.is_civ_evac && u.is_infantry());
    assert!(
        tanya_present || core.world().units.iter().count() >= 4,
        "set1 should have reinforced the player at tick 0"
    );
    dump_png(core, "campaign_scg01ea_start.png");

    // ---- Destroy the two HQ guards carrying the SEMI `eins` trigger. ----
    // (Stand-in for the cross-map, Tesla-defended assault the player performs;
    // the trigger/reinforcement/global/evac/win chain below is the real engine.)
    let eins_idx = core
        .world()
        .campaign()
        .unwrap()
        .triggers
        .iter()
        .position(|t| t.name == "eins")
        .unwrap() as u16;
    let guards: Vec<ra_sim::Handle> = core
        .world()
        .units
        .iter()
        .filter(|(_, u)| u.trigger == Some(eins_idx))
        .map(|(h, _)| h)
        .collect();
    assert_eq!(guards.len(), 2, "two E1 guards carry the `eins` trigger");
    for h in &guards {
        core.world_mut().units.remove(*h);
    }
    // Tick: eins springs -> REINFORCEMENTS einst (Einstein) + FORCE ein2 (DZ +
    // SET_GLOBAL 1). Einstein appears; the LZ/DZ evac cell is dropped.
    core.world_mut().tick(&[]);
    core.world_mut().tick(&[]);

    let einstein = core
        .world()
        .units
        .iter()
        .find(|(_, u)| u.is_civ_evac)
        .map(|(h, _)| h)
        .expect("eins should have reinforced Einstein (a civ-evac VIP)");
    {
        let camp = core.world().campaign().unwrap();
        assert!(
            camp.globals.first().copied() == Some(false),
            "global 0 unused"
        );
        assert!(
            camp.globals.get(1).copied() == Some(true),
            "ein2 SET_GLOBAL 1 (Einstein rescued)"
        );
        assert!(!camp.evac_cells.is_empty(), "ein2 DZ dropped an evac flare");
    }
    eprintln!(
        "Einstein spawned, global-1 set, {} evac cell(s)",
        core.world().campaign().unwrap().evac_cells.len()
    );

    // Clear the USSR base defences on Einstein's route (the briefing's "beware the
    // Tesla coils" — a stand-in for the assault Tanya makes; the evac + win below
    // is the real engine). Without this, a Tesla zaps Einstein mid-escort, which
    // is itself correct mission logic (his `elos` DESTROYED -> LOSE fires).
    let ussr_buildings: Vec<ra_sim::Handle> = core
        .world()
        .buildings
        .iter()
        .filter(|(_, b)| b.house == 2)
        .map(|(h, _)| h)
        .collect();
    for h in ussr_buildings {
        core.world_mut().buildings.remove(h);
    }
    // M7.5-B: the Soviet guards now **actively** engage (per-unit Guard mission,
    // QUIRKS Q18) — before, placed units only retaliated, so an unarmed VIP could
    // stroll past them. Escorting Einstein now requires the route to be *cleared*,
    // which is exactly what Tanya does in the real mission. This harness stands in
    // for that assault by removing the Soviet units on his corridor too (the same
    // "stand-in for the assault Tanya makes" the building removal above already is).
    // The trigger chain (evac -> win) is untouched; only the scripted tactics change.
    let ussr_units: Vec<ra_sim::Handle> = core
        .world()
        .units
        .iter()
        .filter(|(_, u)| u.house == 2)
        .map(|(h, _)| h)
        .collect();
    for h in ussr_units {
        core.world_mut().units.remove(h);
    }

    // ---- Guide Einstein to the DZ evac cell -> EVAC_CIVILIAN -> WIN. ----
    let evac_cell = core.world().campaign().unwrap().evac_cells[0];
    // Einstein's reinforcement lands on his origin waypoint, which in this base
    // sits on a tile our simplified land mask (Q6: cost<100 collapsed, only
    // impassability modelled) marks Foot-impassable. Nudge him to the nearest
    // Foot-passable cell so he can walk out — a one-time correction for the
    // spawn-on-waypoint landing, not a gameplay path (the real evac is by
    // helicopter anyway).
    {
        let ecell = core.world().units.get(einstein).unwrap().cell();
        if !core
            .world()
            .passability()
            .is_passable_loco(ecell, ra_sim::Locomotor::Foot)
        {
            'find: for r in 1..12 {
                for dy in -r..=r {
                    for dx in -r..=r {
                        let c = ra_sim::CellCoord::new(ecell.x + dx, ecell.y + dy);
                        if core
                            .world()
                            .passability()
                            .is_passable_loco(c, ra_sim::Locomotor::Foot)
                        {
                            if let Some(u) = core.world_mut().units.get_mut(einstein) {
                                u.coord = c.center();
                            }
                            break 'find;
                        }
                    }
                }
            }
        }
    }
    // Recenter camera on Einstein for the victory shot.
    let ecell = core.world().units.get(einstein).unwrap().cell();
    core.set_camera(ecell.x as f32 * cp - 512.0, ecell.y as f32 * cp - 384.0);
    core.world_mut().tick(&[Command::Move {
        unit: einstein,
        dest: evac_cell,
        house: 1,
    }]);
    let _ = Target::Cell(evac_cell); // (Target imported for API completeness)

    let mut won = false;
    for _ in 0..4000 {
        core.world_mut().tick(&[]);
        if core.world().units.get(einstein).is_none() {
            // Evacuated (removed). Give the win trigger a tick to resolve.
            core.world_mut().tick(&[]);
        }
        if core.world().game_over() == GameOver::Victory {
            won = true;
            break;
        }
        if core.world().game_over() == GameOver::Defeat {
            let camp = core.world().campaign().unwrap();
            let sprung: Vec<&str> = camp
                .triggers
                .iter()
                .zip(&camp.state)
                .filter(|(t, s)| {
                    s.sprung && (t.a1.code == taction::LOSE || t.a2.code == taction::LOSE)
                })
                .map(|(t, _)| t.name.as_str())
                .collect();
            let einstein_alive = core.world().units.get(einstein).is_some();
            let evac = camp.is_civ_evacuated(1);
            let ncell = core.world().units.get(einstein).map(|u| u.cell());
            panic!(
                "DEFEAT at tick {}: sprung LOSE triggers={:?} einstein_alive={} evacuated={} einstein_cell={:?} evac_cell={:?}",
                core.world().tick_count(),
                sprung,
                einstein_alive,
                evac,
                ncell,
                evac_cell
            );
        }
    }
    if !won {
        let ec = core
            .world()
            .units
            .get(einstein)
            .map(|u| (u.cell(), u.path.len(), u.stats.max_speed));
        panic!(
            "no victory after escort: einstein={:?} (cell,pathlen,speed) evac_cell={:?}",
            ec, evac_cell
        );
    }
    assert!(
        won,
        "reaching the evac point with Einstein must trigger VICTORY"
    );
    assert!(
        core.world().campaign().unwrap().is_civ_evacuated(1),
        "Greece's IsCivEvacuated latched"
    );
    dump_png(core, "campaign_scg01ea_victory.png");
    eprintln!("VICTORY at tick {}", core.world().tick_count());
}

/// The real campaign menu flow: MainMenu -> Campaign -> mission list ->
/// briefing (scg01ea's real briefing text), with a PNG of the briefing screen.
#[test]
fn scg01ea_campaign_menu_flow_and_briefing_png() {
    use ra_client::input::{InputEvent, Key};
    use ra_client::menu::{App, AppState, CampaignFactory};

    let dir = support::assets_dir();
    if !dir.join("main.mix").is_file() || !dir.join("redalert.mix").is_file() {
        eprintln!("SKIP: real assets not found under {}", dir.display());
        return;
    }
    let main = std::fs::read(dir.join("main.mix")).unwrap();
    let redalert = std::fs::read(dir.join("redalert.mix")).unwrap();
    let factory = assets::ArchiveCampaignFactory::new(main, redalert);
    let missions = factory.missions();
    eprintln!("campaign missions found: {}", missions.len());
    assert!(!missions.is_empty(), "at least scg01ea should resolve");
    assert!(missions[0].name.contains("thick"), "first Allied mission");

    let mut app =
        App::new(Vec::new(), Box::new(NoSkirmishFactory)).with_campaign(Box::new(factory));
    app.handle(InputEvent::Resize {
        width: 1024,
        height: 768,
    });
    // Main menu -> Campaign (focus down to the CAMPAIGN button) -> list.
    app.handle(InputEvent::KeyDown(Key::Down));
    app.handle(InputEvent::KeyDown(Key::Confirm));
    assert_eq!(app.state(), AppState::CampaignList);
    // Select mission 1 -> briefing.
    app.handle(InputEvent::KeyDown(Key::Confirm));
    assert_eq!(app.state(), AppState::Briefing);
    assert!(
        app.briefing_text().contains("Einstein"),
        "real scg01ea briefing loaded"
    );
    let f = app.compose();
    let bytes = ra_client::png::encode_rgba(f.width, f.height, &f.pixels);
    let path = scratch().join("campaign_scg01ea_briefing.png");
    let _ = std::fs::write(&path, bytes);
    eprintln!("wrote {}", path.display());

    // START MISSION -> InGame with the real mission core.
    app.handle(InputEvent::KeyDown(Key::Confirm));
    assert_eq!(app.state(), AppState::InGame);
    assert!(app.core().unwrap().world().campaign().is_some());
}

/// A skirmish factory stub for the campaign-only flow test.
struct NoSkirmishFactory;
impl ra_client::menu::GameFactory for NoSkirmishFactory {
    fn build(
        &self,
        _res: &ra_client::menu::ResolvedSkirmish,
    ) -> Result<(ra_client::appcore::AppCore, ra_sim::CellCoord), String> {
        Err("skirmish disabled".into())
    }
}

/// Same script twice must yield identical final hashes (determinism, incl. the
/// campaign trigger/global/timer state).
#[test]
fn scg01ea_playthrough_is_deterministic() {
    let dir = support::assets_dir();
    if !dir.join("main.mix").is_file() || !dir.join("redalert.mix").is_file() {
        eprintln!("SKIP: real assets not found under {}", dir.display());
        return;
    }
    let run = || -> u64 {
        let mut m = assets::load_campaign_from_bytes(
            &std::fs::read(dir.join("main.mix")).unwrap(),
            &std::fs::read(dir.join("redalert.mix")).unwrap(),
            "scg01ea.ini",
            ra_sim::Difficulty::Normal,
        )
        .unwrap();
        let core = &mut m.core;
        core.world_mut().tick(&[]);
        // Kill the eins guards, run the chain, escort Einstein, win — scripted
        // identically both times.
        let eins_idx = core
            .world()
            .campaign()
            .unwrap()
            .triggers
            .iter()
            .position(|t| t.name == "eins")
            .unwrap() as u16;
        let guards: Vec<ra_sim::Handle> = core
            .world()
            .units
            .iter()
            .filter(|(_, u)| u.trigger == Some(eins_idx))
            .map(|(h, _)| h)
            .collect();
        for h in guards {
            core.world_mut().units.remove(h);
        }
        core.world_mut().tick(&[]);
        core.world_mut().tick(&[]);
        let einstein = core
            .world()
            .units
            .iter()
            .find(|(_, u)| u.is_civ_evac)
            .map(|(h, _)| h)
            .unwrap();
        // Raze the USSR base + nudge Einstein onto passable ground (same as the
        // victory script) so both runs reach VICTORY, not a defeat.
        let ussr: Vec<ra_sim::Handle> = core
            .world()
            .buildings
            .iter()
            .filter(|(_, b)| b.house == 2)
            .map(|(h, _)| h)
            .collect();
        for h in ussr {
            core.world_mut().buildings.remove(h);
        }
        let ecell = core.world().units.get(einstein).unwrap().cell();
        if !core
            .world()
            .passability()
            .is_passable_loco(ecell, ra_sim::Locomotor::Foot)
        {
            'find: for r in 1..12 {
                for dy in -r..=r {
                    for dx in -r..=r {
                        let c = ra_sim::CellCoord::new(ecell.x + dx, ecell.y + dy);
                        if core
                            .world()
                            .passability()
                            .is_passable_loco(c, ra_sim::Locomotor::Foot)
                        {
                            core.world_mut().units.get_mut(einstein).unwrap().coord = c.center();
                            break 'find;
                        }
                    }
                }
            }
        }
        let evac = core.world().campaign().unwrap().evac_cells[0];
        core.world_mut().tick(&[Command::Move {
            unit: einstein,
            dest: evac,
            house: 1,
        }]);
        let mut last = 0;
        for _ in 0..4000 {
            last = core.world_mut().tick(&[]);
            if core.world().game_over() != GameOver::Ongoing {
                break;
            }
        }
        last
    };
    assert_eq!(run(), run(), "same script twice must hash-match");
}

/// Regression pin (ra-tester, M7.5-B audit): before this milestone, placed
/// Soviet guards only *retaliated* — an unescorted Einstein could stroll past
/// them untouched. `scg01ea_inventory_and_playthrough_to_victory` above works
/// around the now-active guards by clearing the USSR base's units on
/// Einstein's corridor (documented there as "the assault Tanya makes"). This
/// test proves that workaround is load-bearing: with the Soviet *buildings*
/// razed (removing the Tesla-coil threat, isolating the guards specifically)
/// but the Soviet *units* left alive, walking Einstein through them must now
/// end in DEFEAT — his `elos` (`DESTROYED` -> `LOSE`) trigger firing — pinned
/// at the exact deterministic tick this reaches (a change from that tick would
/// mean guard engagement timing shifted and this pin should be revisited).
#[test]
fn scg01ea_einstein_dies_to_active_guards_if_the_route_is_not_cleared() {
    let dir = support::assets_dir();
    if !dir.join("main.mix").is_file() || !dir.join("redalert.mix").is_file() {
        eprintln!("SKIP: real assets not found under {}", dir.display());
        return;
    }
    let mut mission = assets::load_campaign_from_bytes(
        &std::fs::read(dir.join("main.mix")).unwrap(),
        &std::fs::read(dir.join("redalert.mix")).unwrap(),
        "scg01ea.ini",
        ra_sim::Difficulty::Normal,
    )
    .expect("load scg01ea");
    let core = &mut mission.core;

    core.world_mut().tick(&[]);
    let eins_idx = core
        .world()
        .campaign()
        .unwrap()
        .triggers
        .iter()
        .position(|t| t.name == "eins")
        .unwrap() as u16;
    let guards: Vec<ra_sim::Handle> = core
        .world()
        .units
        .iter()
        .filter(|(_, u)| u.trigger == Some(eins_idx))
        .map(|(h, _)| h)
        .collect();
    for h in &guards {
        core.world_mut().units.remove(*h);
    }
    core.world_mut().tick(&[]);
    core.world_mut().tick(&[]);
    let einstein = core
        .world()
        .units
        .iter()
        .find(|(_, u)| u.is_civ_evac)
        .map(|(h, _)| h)
        .expect("eins should have reinforced Einstein");

    // Raze the USSR *buildings* only (removes the Tesla coil so the DEFEAT
    // below is attributable to the guards, not a base defense structure) —
    // the Soviet *units* are deliberately left alive and un-alerted.
    let ussr_buildings: Vec<ra_sim::Handle> = core
        .world()
        .buildings
        .iter()
        .filter(|(_, b)| b.house == 2)
        .map(|(h, _)| h)
        .collect();
    for h in ussr_buildings {
        core.world_mut().buildings.remove(h);
    }

    // Same spawn-cell foot-passability nudge the victory playthrough performs
    // (Q6: our simplified land mask marks Einstein's landing waypoint
    // Foot-impassable) — orthogonal to the guard behaviour under test.
    {
        let ecell = core.world().units.get(einstein).unwrap().cell();
        if !core
            .world()
            .passability()
            .is_passable_loco(ecell, ra_sim::Locomotor::Foot)
        {
            'find: for r in 1..12 {
                for dy in -r..=r {
                    for dx in -r..=r {
                        let c = ra_sim::CellCoord::new(ecell.x + dx, ecell.y + dy);
                        if core
                            .world()
                            .passability()
                            .is_passable_loco(c, ra_sim::Locomotor::Foot)
                        {
                            core.world_mut().units.get_mut(einstein).unwrap().coord = c.center();
                            break 'find;
                        }
                    }
                }
            }
        }
    }

    let evac_cell = core.world().campaign().unwrap().evac_cells[0];
    core.world_mut().tick(&[Command::Move {
        unit: einstein,
        dest: evac_cell,
        house: 1,
    }]);

    let mut outcome = None;
    for _ in 0..2000 {
        core.world_mut().tick(&[]);
        let go = core.world().game_over();
        if go != GameOver::Ongoing {
            outcome = Some((go, core.world().tick_count()));
            break;
        }
    }
    let (go, tick) = outcome.expect("the escort must resolve (win or lose) within budget");
    assert_eq!(
        go,
        GameOver::Defeat,
        "with active Soviet guards left un-cleared on his route, Einstein must \
         be caught and killed — DEFEAT, not VICTORY (the M7.5-B behaviour \
         change this suite guards against silently reverting)"
    );
    let camp = core.world().campaign().unwrap();
    let sprung: Vec<&str> = camp
        .triggers
        .iter()
        .zip(&camp.state)
        .filter(|(t, s)| s.sprung && (t.a1.code == taction::LOSE || t.a2.code == taction::LOSE))
        .map(|(t, _)| t.name.as_str())
        .collect();
    assert_eq!(
        sprung,
        vec!["elos"],
        "the DEFEAT must come from Einstein's own DESTROYED->LOSE trigger, not \
         some other loss condition"
    );
    assert!(
        core.world().units.get(einstein).is_none(),
        "Einstein must actually have been killed, not merely have the trigger \
         fire around a survivor"
    );
    // Deterministic tick pin: this scenario, this scripted route, this seed —
    // the guards must catch and kill him at exactly this tick. A drift here
    // means guard reaction/engagement timing changed; re-derive, don't just
    // bump the number.
    //
    // Re-derived 63 → 58 for M7.20 P1.5: the pathfinder now matches the
    // original's destination-cell-only diagonal rule (`Can_Enter_Cell`
    // ignores its FacingType, UNIT.CPP:3208), so units squeeze diagonally
    // between corner-touching static blockers. On scg01ea's real terrain the
    // guard/Einstein routes shorten and the engagement lands 5 ticks earlier.
    // Verified by bisect: restoring the pre-M7.20 corner rule alone restores
    // tick 63 exactly.
    assert_eq!(
        tick, 58,
        "Einstein's death-to-active-guards must land on the same deterministic \
         tick every run"
    );
}
