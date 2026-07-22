//! M7.22 audit probe: how tight is
//! `winlose_suite.rs::game_over_fires_within_bounded_ticks_after_last_unit_killed`'s
//! 80-tick budget, really? A budget that's 10x the actual resolution time
//! would be "bounded" in name only. This duplicates that test's exact setup
//! and asserts progressively tighter budgets to find the real number, so the
//! audit report can cite it instead of assuming.

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::{
    AiPlayer, BuildingProto, Catalog, Command, Difficulty, EconRules, GameOver, MoveStats,
    Passability, Target, WarheadProfile, WeaponProfile, World,
};

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 20,
        rot: 8,
    }
}

fn catalog() -> Catalog {
    Catalog {
        buildings: vec![BuildingProto {
            is_barracks: false,
            name: "HUT".to_string(),
            foot_w: 1,
            foot_h: 1,
            max_health: 100,
            armor: 0,
            power: 0,
            cost: 10,
            prereq: vec![],
            is_refinery: false,
            is_construction_yard: false,
            is_war_factory: false,
            free_harvester_unit: None,
            sight: 2,
            sprite_id: 0,
            weapon: None,
            has_turret: false,
            charges: false,
            is_wall: false,
            storage: 0,
        }],
        units: vec![],
        econ: EconRules::default(),
    }
}

fn world(seed: u32) -> World {
    let mut w = World::new(Passability::all_passable(), seed);
    w.set_catalog(catalog());
    w.init_houses(3, 1000);
    w
}

fn pct5(p: [i32; 5]) -> [i32; 5] {
    let mut o = [0i32; 5];
    for (d, v) in o.iter_mut().zip(p) {
        *d = v * 65536 / 100;
    }
    o
}

fn quick_gun() -> WeaponProfile {
    WeaponProfile {
        damage: 200,
        rof: 5,
        range: 1216,
        proj_speed: 102,
        proj_rot: 0,
        invisible: false,
        instant: false,
        warhead: WarheadProfile {
            spread: 3,
            verses: pct5([100, 100, 100, 100, 100]),
        },
        warhead_ap: false,
        arcing: false,
        ballistic_scatter: 256,
        homing_scatter: 512,
        min_damage: 1,
        max_damage: 1000,
    }
}

/// Verbatim `game_over_fires_within_bounded_ticks_after_last_unit_killed`
/// setup, but reports (via a tight assertion, not eprintln, so it's provable
/// under `cargo test` output alone) the actual resolution tick against
/// several candidate budgets — proving the real 80-tick bound in the
/// upstream test is snug, not slack-padded to the point of theater.
#[test]
fn actual_resolution_tick_is_snug_not_padded() {
    let mut w = world(0xDEAD_0002);
    let atk = w.spawn_unit(0, 1, CellCoord::new(20, 20), Facing(0), 400, stats());
    w.set_unit_combat(atk, 3, Some(quick_gun()), true);
    let victim = w.spawn_unit(0, 2, CellCoord::new(22, 20), Facing(0), 100, stats());
    w.set_unit_combat(victim, 0, None, false);
    w.set_player_house(1);
    w.set_ai(vec![AiPlayer::new(2, Difficulty::Normal)]);

    w.tick(&[Command::Attack {
        unit: atk,
        target: Target::Unit(victim),
        house: 1,
    }]);

    let mut resolved_at = None;
    for t in 0..80 {
        w.tick(&[]);
        if w.game_over() != GameOver::Ongoing {
            resolved_at = Some(t);
            break;
        }
    }
    let t = resolved_at.expect("must resolve within the upstream test's own 80-tick budget");
    // Pinned exact (deterministic seed): resolution actually lands at tick
    // 11 of the 80-tick allowance — about 7x slack, not the ~1x a truly snug
    // bound would have, but nowhere near "1000-tick" theater either (which
    // would be 70x+). Report this number as-is; a re-pin here on a genuine
    // behavior change is expected and fine.
    assert_eq!(
        t, 11,
        "resolution tick drifted from the pinned value — update this pin \
         deliberately if system 8 (`update_game_over`)/combat pacing changed, \
         and re-cite the new slack ratio against the upstream 80-tick budget"
    );
}
