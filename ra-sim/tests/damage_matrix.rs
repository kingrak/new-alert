//! Damage-matrix table tests (M4 combat, DESIGN.md §4.2/§4.4): with the real
//! `rules.ini` parsed through `ra_data::combat`, exercise
//! [`ra_sim::modify_damage`] across all 5 armor classes for three real
//! warheads (AP/90mm, SA/M60mg, HE/155mm) at three distance regimes (point
//! blank, "near" — inside the quarter-range MinDamage floor — and "far" —
//! outside it, including one clamp-to-16 case), against **independently
//! hand-computed** expected values.
//!
//! **Derivation policy** (per the task: "don't just call the same function
//! twice"). Every expected value below is computed by hand from:
//! - the real `Verses=`/`Spread=` percentages read directly out of
//!   `rules.ini` (transcribed in the doc comment on each table, not derived
//!   by calling `resolve_weapon`), and
//! - the *documented* `Modify_Damage` constants cited in
//!   `ra_sim::combat::modify_damage`'s own doc comment (`combat.cpp:68`):
//!   modifier rounding `(damage*raw + 32768) / 65536`, no-spread divisor
//!   `PIXEL_LEPTON_W/4 == 2`, spread divisor `spread * (PIXEL_LEPTON_W/2) ==
//!   spread*5`, the `Bound(distance, 0, 16)` clamp, the `distance < 4 ->
//!   max(damage, MinDamage)` floor, and the final `min(damage, MaxDamage)`.
//!
//! This test only *loads* the weapon defs from the real archive (so a drift
//! in `rules.ini`'s actual values — e.g. someone re-balances `[AP].Verses` —
//! fails loudly here rather than silently validating stale hardcoded stats);
//! the arithmetic that turns "base damage + armor + distance" into "final
//! damage" is worked out independently in this file's comments, not by
//! calling `resolve_weapon` -> `modify_damage` -> `modify_damage` again.
//!
//! Skips cleanly (never fails) without the real assets, per repo policy.

use std::path::PathBuf;

use ra_data::combat::{armor_index, resolve_unit_combat};
use ra_formats::ini::Ini;
use ra_formats::mix::MixArchive;
use ra_sim::modify_damage;

fn assets_dir() -> PathBuf {
    std::env::var("RA_ASSETS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../assets"))
}

/// Load the real `rules.ini` from `redalert.mix` -> `local.mix`, or `None`
/// (skip) if the archive isn't present.
fn load_rules() -> Option<Ini> {
    let dir = assets_dir();
    if !dir.join("redalert.mix").is_file() {
        eprintln!(
            "SKIP: real assets not found under {} (set RA_ASSETS_DIR or copy \
             redalert.mix into assets/ to run this test)",
            dir.display()
        );
        return None;
    }
    let bytes = std::fs::read(dir.join("redalert.mix")).ok()?;
    let redalert = MixArchive::parse(&bytes).ok()?;
    let local = redalert.open_nested("local.mix").ok()?;
    let rules_bytes = local.get("rules.ini")?;
    Some(Ini::parse(&String::from_utf8_lossy(rules_bytes)))
}

/// General bounds used throughout: `rules.ini`'s `[General] MinDamage=1
/// MaxDamage=1000` (verified below in `general_bounds_are_the_expected_ones`,
/// same values every existing combat test in the repo already assumes).
const MIN_DAMAGE: i32 = 1;
const MAX_DAMAGE: i32 = 1000;

/// Confirms the assumption the hand-derivations below are built on: real
/// `[General]` bounds. If `rules.ini` ever changes these, this test (not the
/// silent hardcoded constants above) is where it shows up.
#[test]
fn general_bounds_are_the_expected_ones() {
    let Some(rules) = load_rules() else { return };
    assert_eq!(rules.get_int("General", "MinDamage"), Some(1));
    assert_eq!(rules.get_int("General", "MaxDamage"), Some(1000));
}

/// One row of the damage matrix: base weapon damage + warhead spread/verses
/// (as read from `rules.ini`, asserted below) crossed with an armor index and
/// a distance, mapped to a hand-derived expected `modify_damage` output.
struct Case {
    /// Human label for failure messages.
    label: &'static str,
    weapon_section: &'static str,
    warhead_section: &'static str,
    base_damage: i32,
    /// `Verses=` percentages in armor order [none, wood, light, heavy, concrete].
    verses_pct: [i32; 5],
    spread: i32,
    /// (distance leptons, armor index, expected damage).
    rows: &'static [(i32, u8, i32)],
}

/// Convert a `Verses=` percentage to the raw 16.16 `fixed` `ra_data::combat`
/// stores (`parse_fixed_raw`'s percentage branch: `digits * 65536 / 100`,
/// integer-truncating division — mirrored here independently).
fn pct_to_raw(pct: i32) -> i32 {
    pct * 65536 / 100
}

/// The hand worked-out arithmetic behind every `(distance, armor, expected)`
/// row below (see the module doc comment for the constants cited).
///
/// **AP / 90mm** (`[90mm] Damage=30`, `[AP] Spread=3 Verses=30%,75%,75%,100%,50%`):
/// modifier-only damage (round((30*raw+32768)/65536)) per armor
/// [none,wood,light,heavy,concrete] = [9, 23, 23, 30, 15]:
///   - none:  30*19660=589800,  +32768=622568,  /65536 = 9  (589800=30*30%raw)
///   - wood:  30*49152=1474560, +32768=1507328, /65536 = 23
///   - light: same modifier as wood (75%) = 23
///   - heavy: 30*65536=1966080, +32768=1998848, /65536 = 30
///   - concrete: 30*32768=983040, +32768=1015808, /65536 = 15
///
/// Falloff divisor = spread*5 = 15.
///   - distance=0   -> computed=0,  distance<4 floor(., 1) -> unchanged: [9,23,23,30,15]
///   - distance=30  -> computed=30/15=2 <4 -> damage/2 then floor(.,1): [4,11,11,15,7]
///     (9/2=4, 23/2=11, 23/2=11, 30/2=15, 15/2=7; all already >=1, floor is a no-op)
///   - distance=200 -> computed=200/15=13 >=4, no floor: [0,1,1,2,1]
///     (9/13=0, 23/13=1, 23/13=1, 30/13=2, 15/13=1)
fn ap_case() -> Case {
    Case {
        label: "AP/90mm",
        weapon_section: "90mm",
        warhead_section: "AP",
        base_damage: 30,
        verses_pct: [30, 75, 75, 100, 50],
        spread: 3,
        rows: &[
            (0, 0, 9),
            (0, 1, 23),
            (0, 2, 23),
            (0, 3, 30),
            (0, 4, 15),
            (30, 0, 4),
            (30, 1, 11),
            (30, 2, 11),
            (30, 3, 15),
            (30, 4, 7),
            (200, 0, 0),
            (200, 1, 1),
            (200, 2, 1),
            (200, 3, 2),
            (200, 4, 1),
        ],
    }
}

/// **SA / M60mg** (`[M60mg] Damage=15`, `[SA] Spread=3
/// Verses=100%,50%,60%,25%,25%`): modifier-only damage per armor
/// [none,wood,light,heavy,concrete] = [15, 8, 9, 4, 4]:
///   - none:  15*65536=983040,  +32768=1015808, /65536=15
///   - wood:  15*32768=491520,  +32768=524288,  /65536=8
///   - light: 15*39321=589815,  +32768=622583,  /65536=9  (60%raw = 60*65536/100 = 39321)
///   - heavy: 15*16384=245760,  +32768=278528,  /65536=4  (25%raw=16384; the existing
///     `sa_vs_heavy_is_quarter` unit test in `ra_sim::combat` pins this same 4,
///     cross-checking this derivation against a second, independently-written
///     assertion already in the tree)
///   - concrete: same modifier as heavy (25%) = 4
///
/// Falloff divisor = spread*5 = 15 (same as AP, same Spread=3).
///   - distance=0   -> unchanged (floor is a no-op, all >=1): [15,8,9,4,4]
///   - distance=30  -> computed=2<4 -> damage/2 floor(.,1): [7,4,4,2,2]
///     (15/2=7, 8/2=4, 9/2=4 [4.5 truncates down], 4/2=2, 4/2=2)
///   - distance=200 -> computed=13>=4, no floor: [1,0,0,0,0]
///     (15/13=1, 8/13=0, 9/13=0, 4/13=0, 4/13=0)
fn sa_case() -> Case {
    Case {
        label: "SA/M60mg",
        weapon_section: "M60mg",
        warhead_section: "SA",
        base_damage: 15,
        verses_pct: [100, 50, 60, 25, 25],
        spread: 3,
        rows: &[
            (0, 0, 15),
            (0, 1, 8),
            (0, 2, 9),
            (0, 3, 4),
            (0, 4, 4),
            (30, 0, 7),
            (30, 1, 4),
            (30, 2, 4),
            (30, 3, 2),
            (30, 4, 2),
            (200, 0, 1),
            (200, 1, 0),
            (200, 2, 0),
            (200, 3, 0),
            (200, 4, 0),
        ],
    }
}

/// **HE / 155mm** (`[155mm] Damage=150`, `[HE] Spread=6
/// Verses=90%,75%,60%,25%,100%`): modifier-only damage per armor
/// [none,wood,light,heavy,concrete] = [135, 113, 90, 38, 150]:
///   - none:  150*58982=8847300,  +32768=8880068,  /65536=135 (90%raw=90*65536/100=58982)
///   - wood:  150*49152=7372800,  +32768=7405568,  /65536=113 (exact: 65536*113=7405568)
///   - light: 150*39321=5898150,  +32768=5930918,  /65536=90  (60%raw=39321)
///   - heavy: 150*16384=2457600,  +32768=2490368,  /65536=38  (exact: 65536*38=2490368)
///   - concrete: 150*65536=9830400,+32768=9863168,  /65536=150 (100%raw, exact base+0.5)
///
/// Falloff divisor = spread*5 = 30 (HE's own Spread=6, twice AP/SA's).
///   - distance=90  -> computed=90/30=3<4 -> damage/3 floor(.,1): [45,37,30,12,50]
///     (135/3=45, 113/3=37 [37.67->37], 90/3=30, 38/3=12 [12.67->12], 150/3=50)
///   - distance=600 -> computed=600/30=20, clamp to 16 (Bound(.,0,16)), >=4 no floor:
///     [8,7,5,2,9] (135/16=8, 113/16=7, 90/16=5, 38/16=2, 150/16=9) — this row is the
///     suite's clamp-to-16 case (distance/divisor alone would be 20, but the
///     `Bound` clamp caps the divisor at 16, giving *more* damage than the
///     unclamped divisor would, which is the whole point of pinning it).
fn he_case() -> Case {
    Case {
        label: "HE/155mm",
        weapon_section: "155mm",
        warhead_section: "HE",
        base_damage: 150,
        verses_pct: [90, 75, 60, 25, 100],
        spread: 6,
        rows: &[
            (0, 0, 135),
            (0, 1, 113),
            (0, 2, 90),
            (0, 3, 38),
            (0, 4, 150),
            (90, 0, 45),
            (90, 1, 37),
            (90, 2, 30),
            (90, 3, 12),
            (90, 4, 50),
            (600, 0, 8),
            (600, 1, 7),
            (600, 2, 5),
            (600, 3, 2),
            (600, 4, 9),
        ],
    }
}

/// For each of the three real warheads: (1) confirm `rules.ini` still reads
/// back the exact `Damage=`/`Spread=`/`Verses=` values the hand derivation in
/// this file's comments assumes (catches silent rebalancing), then (2) run
/// every `(distance, armor, expected)` row through `modify_damage` and assert
/// it matches the independently hand-computed value.
#[test]
fn damage_matrix_matches_hand_computed_values() {
    let Some(rules) = load_rules() else { return };

    for case in [ap_case(), sa_case(), he_case()] {
        // Step 1: the real rules.ini values must match what the derivation
        // comments assume, or the hand math above is stale.
        assert_eq!(
            rules.get_int(case.weapon_section, "Damage"),
            Some(case.base_damage as i64),
            "{}: Damage= drifted from the hand-derivation's assumption",
            case.label
        );
        assert_eq!(
            rules.get_int(case.warhead_section, "Spread"),
            Some(case.spread as i64),
            "{}: Spread= drifted",
            case.label
        );
        let verses_str = rules
            .get(case.warhead_section, "Verses")
            .unwrap_or_else(|| panic!("{}: Verses= missing", case.label));
        let parsed_pct: Vec<i32> = verses_str
            .split(',')
            .map(|tok| {
                let tok = tok.trim();
                tok.trim_end_matches('%').parse::<i32>().unwrap()
            })
            .collect();
        assert_eq!(
            parsed_pct,
            case.verses_pct.to_vec(),
            "{}: Verses= percentages drifted",
            case.label
        );

        // Step 2: build the warhead profile from the *hand-transcribed*
        // percentages (not from `resolve_weapon`, so this is not "call the
        // function, then call it again") and check every row.
        let verses: [i32; 5] = std::array::from_fn(|i| pct_to_raw(case.verses_pct[i]));
        let warhead = ra_sim::WarheadProfile {
            spread: case.spread,
            verses,
        };
        for &(distance, armor, expected) in case.rows {
            let got = modify_damage(
                case.base_damage,
                &warhead,
                armor,
                distance,
                MIN_DAMAGE,
                MAX_DAMAGE,
            );
            assert_eq!(
                got, expected,
                "{}: armor={armor} distance={distance} — modify_damage returned {got}, \
                 hand-derivation expected {expected}",
                case.label
            );
        }
    }
}

/// A second, independent cross-check for the same three warheads: resolve
/// the *actual* `ra_data::resolve_unit_combat` path for a unit that carries
/// each weapon (2TNK/90mm, JEEP/M60mg, ARTY/155mm) and confirm the resolved
/// `verses`/`spread`/`damage` numbers agree with the hand-transcribed table
/// above — i.e. the production parsing path and this test's manual
/// transcription describe the same weapon, so `damage_matrix_matches_hand_computed_values`
/// is really exercising the real weapons, not a look-alike.
#[test]
fn resolved_unit_combat_matches_hand_transcribed_tables() {
    let Some(rules) = load_rules() else { return };

    let checks: [(&str, Case); 3] = [
        ("2TNK", ap_case()),
        ("JEEP", sa_case()),
        ("ARTY", he_case()),
    ];
    for (unit, case) in checks {
        let combat = resolve_unit_combat(&rules, unit)
            .unwrap_or_else(|| panic!("{unit} should resolve from real rules.ini"));
        let weapon = combat
            .weapon
            .unwrap_or_else(|| panic!("{unit} should be armed"));
        assert_eq!(weapon.damage, case.base_damage, "{unit} base damage");
        assert_eq!(weapon.spread, case.spread, "{unit} warhead spread");
        let expected_verses: [i32; 5] = std::array::from_fn(|i| pct_to_raw(case.verses_pct[i]));
        assert_eq!(weapon.verses, expected_verses, "{unit} verses matrix");
    }
}

/// `armor_index` sanity check (used by the loader to map `Armor=` to the
/// column this suite indexes by): the fixed order the whole matrix depends
/// on is none=0, wood=1, light=2, heavy=3, concrete=4.
#[test]
fn armor_index_order_matches_matrix_columns() {
    assert_eq!(armor_index("none"), 0);
    assert_eq!(armor_index("wood"), 1);
    assert_eq!(armor_index("light"), 2);
    assert_eq!(armor_index("heavy"), 3);
    assert_eq!(armor_index("concrete"), 4);
}
