//! Adversarial-depth tests for `select_weapon` — the sim's port of
//! `TechnoClass::What_Weapon_Should_I_Use` (`techno.cpp:360`, wired up at
//! `ra-sim/src/world.rs:1328`) — covering the M7.7 Chunk A dual-weapon
//! feature (3TNK/4TNK, `docs/QUIRKS.md` Q8).
//!
//! Section 1 is a hand-derived truth table crossing every armor class
//! (`ra_sim::ARMOR_COUNT == 5`: none/wood/light/heavy/concrete) against
//! in-range, out-of-range, and — the genuinely adversarial part — the narrow
//! *differential-range* window where the primary and secondary weapons
//! disagree about whether the target is "in range" at all. It uses the REAL
//! `[4TNK]` dual weapons, extracted straight out of `assets/redalert.mix`'s
//! `rules.ini` (`ra-formats`' `radump extract ... rules.ini --in local.mix`)
//! and transcribed below — not re-derived by calling `resolve_weapon`; this
//! file's arithmetic is worked out independently in the comments, the same
//! derivation policy `damage_matrix.rs` documents.
//!
//! Real `rules.ini` facts this file's numbers come from
//! (`[4TNK] Primary=120mm Secondary=MammothTusk Armor=heavy Strength=600`):
//! ```text
//! [120mm]        Damage=40 ROF=80 Range=4.75 Speed=40 Warhead=AP
//! [MammothTusk]  Damage=75 ROF=80 Range=5    Speed=30 Warhead=HE  (Projectile=HeatSeeker, ROT=5)
//! [AP]  Spread=3 Verses=30%,75%,75%,100%,50%   (none,wood,light,heavy,concrete)
//! [HE]  Spread=6 Verses=90%,75%,60%,25%,100%
//! ```
//! `select_weapon`'s score is the *raw* `Verses` modifier (16.16 fixed,
//! `pct*65536/100`, integer-truncating — `parse_fixed_raw`'s percentage
//! branch), doubled when the target is within that weapon's own `Range=`
//! (leptons = cells*256) — **not** a shared "in range" flag: `120mm`'s range
//! (1216 leptons) and `MammothTusk`'s range (1280 leptons) differ, so there is
//! a real 64-lepton window (1217..=1280) where the secondary is in range and
//! the primary is not. That window is exactly what several cases below
//! exploit — this is real-data-driven, not a contrived scenario.
//!
//! Section 2 is a live-`World` end-to-end script: one spawned 4TNK-like unit
//! fires at a heavy-armor target (must select the 120mm primary) then, after
//! retargeting, at a none-armor target (must select the MammothTusk
//! secondary) — asserted through actual health deltas, not by peeking at an
//! internal "which weapon fired" field (`Unit` doesn't expose one; `weapon`/
//! `secondary` are static type constants, not a "current weapon").
//!
//! Self-contained: the real `rules.ini` numbers are transcribed as constants,
//! cross-checked by hand in the comments, not loaded from `assets/` at test
//! time, so nothing here skips even without real game assets present.

use ra_sim::coords::{CellCoord, Facing};
use ra_sim::world::select_weapon;
use ra_sim::{
    Command, Handle, MoveStats, Passability, Target, WarheadProfile, WeaponProfile, World,
};

// ===========================================================================
// Fixtures: the real 4TNK dual weapons, transcribed from `rules.ini`.
// ===========================================================================

/// `pct * 65536 / 100`, integer-truncating — mirrors `ra_data::combat`'s
/// `parse_fixed_raw` percentage branch exactly, worked out independently here
/// (not by calling into `ra_data`) per the repo's damage-matrix derivation
/// convention (`damage_matrix.rs`'s own `pct_to_raw`).
fn pct_to_raw(pct: i32) -> i32 {
    pct * 65536 / 100
}

/// 4TNK's real primary: `[120mm]` (`Damage=40 ROF=80 Range=4.75 Speed=40
/// Warhead=AP`) + `[AP]` (`Spread=3 Verses=30%,75%,75%,100%,50%`).
/// `proj_speed = scale_to_256(40) = 40*256/100 = 102` — the same `Speed=40`
/// 2TNK's 90mm uses, cross-checked against `ra_data::combat`'s own
/// `scale_to_256_matches_engine` test (`scale_to_256(40) == 102`) and
/// `world.rs`'s `ninety_mm()` test fixture.
fn real_120mm() -> WeaponProfile {
    WeaponProfile {
        damage: 40,
        rof: 80,
        range: 1216, // 4.75 * 256
        proj_speed: 102,
        proj_rot: 0, // [Cannon] projectile: no ROT= (straight flight)
        invisible: false,
        instant: false,
        warhead: WarheadProfile {
            spread: 3,
            verses: [
                pct_to_raw(30),  // none:     30*65536/100 = 19660 (trunc from 19660.8)
                pct_to_raw(75),  // wood:     75*65536/100 = 49152 (exact)
                pct_to_raw(75),  // light:    49152
                pct_to_raw(100), // heavy:    65536 (exact)
                pct_to_raw(50),  // concrete: 32768 (exact)
            ],
        },
        warhead_ap: true,
        arcing: false,
        ballistic_scatter: 256,
        homing_scatter: 512,
        min_damage: 1,
        max_damage: 1000,
    }
}

/// 4TNK's real secondary: `[MammothTusk]` (`Damage=75 ROF=80 Range=5
/// Speed=30 Warhead=HE`, `Projectile=HeatSeeker` which carries `ROT=5`) +
/// `[HE]` (`Spread=6 Verses=90%,75%,60%,25%,100%`).
/// `proj_speed = scale_to_256(30) = 30*256/100 = 76.8 -> 76` (integer
/// truncation, same formula `real_120mm` uses).
fn real_mammoth_tusk() -> WeaponProfile {
    WeaponProfile {
        damage: 75,
        rof: 80,
        range: 1280, // 5 * 256
        proj_speed: 76,
        proj_rot: 5, // [HeatSeeker] ROT=5 (homing missile)
        invisible: false,
        instant: false,
        warhead: WarheadProfile {
            spread: 6,
            verses: [
                pct_to_raw(90),  // none:     90*65536/100 = 58982 (trunc from 58982.4)
                pct_to_raw(75),  // wood:     49152
                pct_to_raw(60),  // light:    60*65536/100 = 39321 (trunc from 39321.6)
                pct_to_raw(25),  // heavy:    25*65536/100 = 16384 (exact)
                pct_to_raw(100), // concrete: 65536 (exact)
            ],
        },
        warhead_ap: false,
        arcing: false,
        ballistic_scatter: 256,
        homing_scatter: 512,
        min_damage: 1,
        max_damage: 1000,
    }
}

/// Sanity-pins the transcribed literals against the by-hand comments above,
/// so a transcription slip fails loudly here instead of silently poisoning
/// every truth-table case below.
#[test]
fn real_weapon_fixtures_match_hand_transcription() {
    let p = real_120mm();
    assert_eq!(p.warhead.verses, [19660, 49152, 49152, 65536, 32768]);
    assert_eq!(p.proj_speed, 102);
    assert_eq!(p.range, 1216);

    let s = real_mammoth_tusk();
    assert_eq!(s.warhead.verses, [58982, 49152, 39321, 16384, 65536]);
    assert_eq!(s.proj_speed, 76);
    assert_eq!(s.range, 1280);
}

// ===========================================================================
// Section 1: hand-derived truth table.
//   dist <= 1216            -> both weapons in range (doubling applies equally)
//   1217 <= dist <= 1280    -> only MammothTusk in range (the differential window)
//   dist > 1280             -> neither weapon in range (doubling applies to neither)
// ===========================================================================

const NONE_ARMOR: u8 = 0;
const WOOD: u8 = 1;
const LIGHT: u8 = 2;
const HEAVY: u8 = 3;
const CONCRETE: u8 = 4;

/// Both weapons comfortably in range (dist <= 1216, the smaller of the two).
const DIST_BOTH_IN_RANGE: i32 = 500;
/// Both weapons comfortably out of range (dist > 1280, the larger of the two).
const DIST_BOTH_OUT_OF_RANGE: i32 = 5000;
/// Inside the differential window (1216 < dist <= 1280): MammothTusk (range
/// 1280) is in range and doubled; 120mm (range 1216) is not.
const DIST_ONLY_SECONDARY_IN_RANGE: i32 = 1250;

#[test]
fn primary_wins_by_verses_alone_regardless_of_range() {
    // armor=heavy(3): P raw=65536 (100%), S raw=16384 (25%). P >> S at every
    // multiplier, so doubling never changes the winner here.
    //   in-range:      P=65536*2=131072  S=16384*2=32768   -> P
    //   out-of-range:  P=65536           S=16384           -> P
    let p = real_120mm();
    let s = real_mammoth_tusk();
    for dist in [DIST_BOTH_IN_RANGE, DIST_BOTH_OUT_OF_RANGE] {
        let w = select_weapon(p, Some(s), HEAVY, dist);
        assert_eq!(
            w.warhead.verses, p.warhead.verses,
            "vs heavy at dist={dist}: 120mm/AP must win on verses alone"
        );
    }
}

#[test]
fn secondary_wins_by_verses_alone_regardless_of_range() {
    // armor=none(0): P raw=19660 (30%), S raw=58982 (90%). S >> P at every
    // multiplier.
    //   in-range:      P=19660*2=39320   S=58982*2=117964  -> S
    //   out-of-range:  P=19660           S=58982           -> S
    let p = real_120mm();
    let s = real_mammoth_tusk();
    for dist in [DIST_BOTH_IN_RANGE, DIST_BOTH_OUT_OF_RANGE] {
        let w = select_weapon(p, Some(s), NONE_ARMOR, dist);
        assert_eq!(
            w.warhead.verses, s.warhead.verses,
            "vs none at dist={dist}: MammothTusk/HE must win on verses alone"
        );
    }
}

#[test]
fn secondary_wins_at_concrete_armor_regardless_of_range() {
    // armor=concrete(4): P raw=32768 (50%), S raw=65536 (100%) — HE's
    // strength against structures/concrete; a second "stable secondary win"
    // case with a different margin shape than armor=none's.
    //   in-range:      P=32768*2=65536   S=65536*2=131072  -> S
    //   out-of-range:  P=32768           S=65536           -> S
    let p = real_120mm();
    let s = real_mammoth_tusk();
    for dist in [DIST_BOTH_IN_RANGE, DIST_BOTH_OUT_OF_RANGE] {
        let w = select_weapon(p, Some(s), CONCRETE, dist);
        assert_eq!(
            w.warhead.verses, s.warhead.verses,
            "vs concrete at dist={dist}: MammothTusk/HE must win on verses alone"
        );
    }
}

#[test]
fn exact_tie_primary_wins_both_in_range_and_out_of_range() {
    // armor=wood(1): P raw=49152 (75%), S raw=49152 (75%) — genuinely equal
    // in real rules.ini (120mm and MammothTusk happen to share Verses[wood]).
    // Doubling scales both sides identically whenever they share the same
    // in-range status, so the tie survives both regimes:
    //   in-range:      P=49152*2=98304   S=49152*2=98304   -> TIE -> primary
    //   out-of-range:  P=49152           S=49152           -> TIE -> primary
    // `select_weapon` requires the secondary to *strictly* outscore the
    // primary (`if score(&sec) > score(&primary)`), so an exact tie must
    // resolve to the primary.
    let p = real_120mm();
    let s = real_mammoth_tusk();
    assert_eq!(
        p.warhead.verses[WOOD as usize], s.warhead.verses[WOOD as usize],
        "sanity: this test's premise is that AP and HE truly tie at armor=wood"
    );
    for dist in [DIST_BOTH_IN_RANGE, DIST_BOTH_OUT_OF_RANGE] {
        let w = select_weapon(p, Some(s), WOOD, dist);
        assert_eq!(
            w.warhead.verses, p.warhead.verses,
            "exact tie at dist={dist} must resolve to the primary (strictly-outscores rule)"
        );
    }
}

#[test]
fn differential_range_flips_tie_to_secondary_win() {
    // armor=wood(1), dist=1250: only MammothTusk (range 1280) is in range;
    // 120mm (range 1216) is not.
    //   P (out of range, undoubled) = 49152
    //   S (in range, doubled)       = 49152*2 = 98304   -> S wins
    // The SAME verses that tie at equal multiplier (previous test) flip to a
    // secondary win purely because of the differential range window — the
    // in-range doubling changing the outcome, not the verses.
    let p = real_120mm();
    let s = real_mammoth_tusk();
    let w = select_weapon(p, Some(s), WOOD, DIST_ONLY_SECONDARY_IN_RANGE);
    assert_eq!(
        w.warhead.verses, s.warhead.verses,
        "differential in-range doubling must flip the wood-armor tie to the secondary"
    );
}

#[test]
fn differential_range_flips_primary_win_to_secondary_win() {
    // armor=light(2): P raw=49152 (75%), S raw=39321 (60%) — primary wins on
    // verses alone at equal multiplier:
    //   in-range:      P=49152*2=98304   S=39321*2=78642   -> P
    //   out-of-range:  P=49152           S=39321            -> P
    // But at dist=1250 (differential window, only S in range):
    //   P (out of range, undoubled) = 49152
    //   S (in range, doubled)       = 39321*2 = 78642       -> S wins!
    // A genuine order-flip: the *weaker* weapon (by verses) wins purely
    // because it alone is in range.
    let p = real_120mm();
    let s = real_mammoth_tusk();

    // Baseline (primary wins) at equal-multiplier distances first.
    for dist in [DIST_BOTH_IN_RANGE, DIST_BOTH_OUT_OF_RANGE] {
        let w = select_weapon(p, Some(s), LIGHT, dist);
        assert_eq!(
            w.warhead.verses, p.warhead.verses,
            "baseline at dist={dist}: primary should win"
        );
    }

    // The flip.
    let w = select_weapon(p, Some(s), LIGHT, DIST_ONLY_SECONDARY_IN_RANGE);
    assert_eq!(
        w.warhead.verses, s.warhead.verses,
        "differential in-range doubling must flip armor=light to the secondary"
    );
}

#[test]
fn differential_range_window_boundaries_are_exact() {
    // armor=light(2), probing the exact edges of the differential window
    // (120mm range=1216, MammothTusk range=1280; `dist <= range` is
    // inclusive on both sides — `select_weapon`'s `dist <= w.range`):
    //   dist=1216: both in range (1216<=1216 and 1216<=1280)  -> P (98304 vs 78642)
    //   dist=1217: only S in range (1217>1216, 1217<=1280)    -> S (49152 vs 78642)
    //   dist=1280: only S in range, at S's own exact boundary -> S (49152 vs 78642)
    //   dist=1281: neither in range (1281>1216 and 1281>1280) -> P (49152 vs 39321)
    let p = real_120mm();
    let s = real_mammoth_tusk();
    let cases: [(i32, bool); 4] = [
        (1216, true),  // primary wins
        (1217, false), // secondary wins
        (1280, false), // secondary wins
        (1281, true),  // primary wins
    ];
    for (dist, primary_should_win) in cases {
        let w = select_weapon(p, Some(s), LIGHT, dist);
        let expected = if primary_should_win {
            p.warhead.verses
        } else {
            s.warhead.verses
        };
        assert_eq!(
            w.warhead.verses,
            expected,
            "dist={dist}: expected {} to win",
            if primary_should_win {
                "primary"
            } else {
                "secondary"
            }
        );
    }
}

#[test]
fn no_secondary_always_returns_primary() {
    // Single-weapon units (no Secondary=) must always get the primary,
    // regardless of armor or range — checked at both a "secondary would
    // clearly win if present" armor (none) and a "primary wins anyway" armor
    // (heavy), and at both an in-range and an out-of-range distance.
    let p = real_120mm();
    for armor in [NONE_ARMOR, HEAVY] {
        for dist in [DIST_BOTH_IN_RANGE, DIST_BOTH_OUT_OF_RANGE] {
            let w = select_weapon(p, None, armor, dist);
            assert_eq!(
                w.warhead.verses, p.warhead.verses,
                "armor={armor} dist={dist}: no secondary must always yield the primary"
            );
        }
    }
}

/// Synthetic pair (not tied to any real unit — clearly labeled) exercising
/// the *opposite* differential-range flip direction from the real-4TNK cases
/// above: the real 120mm/MammothTusk pair only ever lets the *secondary*
/// benefit from a range advantage the *primary* lacks (120mm's range, 1216,
/// is the smaller of the two), so it cannot demonstrate a primary winning
/// *only* because it, not the secondary, is in range. Hand-picked round
/// numbers make that direction reachable:
///   primary:   verses[none]=40000 (raw), range=2000
///   secondary: verses[none]=50000 (raw), range=100
/// At equal multiplier the secondary always wins on verses alone (50000 >
/// 40000, doubled or not). But at dist=150 (primary in its own 2000-range,
/// secondary out of its 100-range):
///   P (in range, doubled)       = 40000*2 = 80000
///   S (out of range, undoubled) = 50000                -> P wins
/// — the mirror image of `differential_range_flips_primary_win_to_secondary_win`.
#[test]
fn synthetic_reverse_flip_primary_wins_only_because_in_range() {
    let base = real_120mm(); // borrow a realistic shape; override only what's under test
    let mut p = base;
    p.warhead.verses = [40000; 5];
    p.range = 2000;
    let mut s = base;
    s.warhead.verses = [50000; 5];
    s.range = 100;

    // Baseline: secondary wins on verses alone regardless of equal doubling
    // (dist=50, both in range: P=80000, S=100000 -> S).
    let w = select_weapon(p, Some(s), NONE_ARMOR, 50);
    assert_eq!(
        w.warhead.verses, s.warhead.verses,
        "baseline: secondary should win on verses alone"
    );

    // The reverse flip: only the primary is in range.
    let w = select_weapon(p, Some(s), NONE_ARMOR, 150);
    assert_eq!(
        w.warhead.verses, p.warhead.verses,
        "dist=150: only the primary is in range (range=2000 vs secondary's range=100), \
         so it must win despite weaker verses (40000 doubled=80000 > 50000 undoubled)"
    );
}

// ===========================================================================
// Section 2: live-World end-to-end — one 4TNK-like unit engages two
// different-armor targets in sequence, and its *damage numbers* (not an
// internal "current weapon" field — none exists; `Unit::weapon`/`secondary`
// are static type constants, not a per-shot "which one fired") prove which
// weapon actually fired.
// ===========================================================================

fn stats() -> MoveStats {
    MoveStats {
        max_speed: 25,
        rot: 10,
    }
}

fn world() -> World {
    // Combat-only scenario: no economy/production involved, so (matching
    // `world.rs`'s own colocated combat tests) `init_houses` is not called.
    World::new(Passability::all_passable(), 0xF00D_4747)
}

/// Spawn a 4TNK-like dual-weapon attacker: real primary (120mm/AP) + real
/// secondary (MammothTusk/HE), armor=heavy (4TNK's own `Armor=heavy`),
/// turreted (4TNK is turret-equipped per `ra_data::combat::turret_equipped`).
fn spawn_4tnk_like(w: &mut World, house: u8, cell: CellCoord) -> Handle {
    let h = w.spawn_unit(0, house, cell, Facing(0), 600, stats()); // Strength=600
    w.set_unit_combat(h, 3 /* heavy */, Some(real_120mm()), true);
    w.set_unit_secondary(h, Some(real_mammoth_tusk()));
    h
}

/// Spawn an unarmed target at the given armor class (no weapon: isolates the
/// attacker's damage numbers from any return fire, the same simplification
/// `world.rs`'s own `spawn_tank`/HARV-like fixtures use).
fn spawn_target(w: &mut World, house: u8, cell: CellCoord, armor: u8, hp: u16) -> Handle {
    let h = w.spawn_unit(0, house, cell, Facing(0), hp, stats());
    w.set_unit_combat(h, armor, None, false);
    h
}

/// Tick until `target`'s health first drops below `before`, returning the
/// exact size of that single hit. A hard test failure (not a silent skip) if
/// the target dies without ever being observed to drop (shouldn't happen
/// with the generous hitpoints used below) or if it never takes damage
/// within `cap` ticks — the latter is exactly the kind of "weapon selection
/// silently broke" failure this test exists to catch.
fn tick_until_hit(w: &mut World, target: Handle, before: u16, cap: u32) -> u16 {
    for _ in 0..cap {
        w.tick(&[]);
        match w.units.get(target) {
            Some(u) if u.health < before => return before - u.health,
            Some(_) => continue,
            None => panic!("target died before a single hit could be measured"),
        }
    }
    panic!("target took no damage within {cap} ticks — the attacker never fired");
}

#[test]
fn mammoth_tank_dual_weapon_selects_by_target_armor_via_damage_numbers() {
    let mut w = world();
    let atk = spawn_4tnk_like(&mut w, 1, CellCoord::new(10, 10));

    // --- Phase 1: engage a heavy-armor target -> must select 120mm/AP. ---
    // Hand-derived expected damage (direct unit hit: `world::fire`'s
    // `inaccurate` scatter branch only triggers for `Target::Cell`
    // force-fires, never `Target::Unit`, so `impact == target_coord` exactly
    // and the post-detonation `leptons_distance(impact, coord)` is 0 for a
    // stationary target — `modify_damage`'s falloff branch is then skipped
    // entirely, matching `infantry_combat_suite.rs`'s identical JEEP-vs-E1
    // reasoning):
    //   modifier damage = round((40*65536 + 32768) / 65536)
    //                    = (2621440 + 32768) / 65536 = 2654208 / 65536 = 40  (100% verses)
    let heavy_target = spawn_target(&mut w, 2, CellCoord::new(11, 10), HEAVY, 1000);
    let before = w.units.get(heavy_target).unwrap().health;
    w.tick(&[Command::Attack {
        unit: atk,
        target: Target::Unit(heavy_target),
        house: 1,
    }]);
    let dmg1 = tick_until_hit(&mut w, heavy_target, before, 400);
    assert_eq!(
        dmg1, 40,
        "vs heavy armor the 4TNK must fire its 120mm primary for exactly 40 damage \
         (AP Verses[heavy]=100%, matching docs/QUIRKS.md Q8's cited Damage-40 case)"
    );

    // --- Phase 2: retarget to a none-armor target -> must select
    // MammothTusk/HE instead. Hand-derived expected damage:
    //   modifier damage = round((75*58982 + 32768) / 65536)
    //                    = (4423650 + 32768) / 65536 = 4456418 / 65536 = 67
    //     (65536*67 = 4390912; remainder 65506 < 65536, so the integer
    //     division floors to exactly 67 — a genuinely different number from
    //     Phase 1's 40, proving a *different weapon* fired, not just a
    //     different armor modifier on the same one: the only other two
    //     (weapon, armor) combinations, 120mm-vs-none and MammothTusk-vs-heavy,
    //     hand-compute to 12 and 19 respectively — nothing else lands on 40
    //     or 67, so these two numbers alone discriminate the weapon choice.)
    let none_target = spawn_target(&mut w, 2, CellCoord::new(9, 10), NONE_ARMOR, 1000);
    let before2 = w.units.get(none_target).unwrap().health;
    w.tick(&[Command::Attack {
        unit: atk,
        target: Target::Unit(none_target),
        house: 1,
    }]);
    let dmg2 = tick_until_hit(&mut w, none_target, before2, 400);
    assert_eq!(
        dmg2, 67,
        "vs none armor the 4TNK must fire its MammothTusk secondary for exactly 67 damage \
         (HE Verses[none]=90%, matching docs/QUIRKS.md Q8's cited Damage-75 weapon)"
    );

    assert_ne!(
        dmg1, dmg2,
        "sanity: the two shots must be visibly different weapons"
    );
}
