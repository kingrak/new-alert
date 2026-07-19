//! Combat rules read from `rules.ini` (DESIGN.md §3.8: rules.ini is the single
//! source of stats). This module parses the `[WeaponName]`, `[WarheadName]`, and
//! `[Projectile]` sections plus the `[General]` damage/scatter bounds, and
//! resolves a unit's `Primary=`/`Armor=` into the plain numbers the sim needs.
//!
//! It produces [`UnitCombat`] / [`WeaponDef`] as plain integers; `ra-client`
//! maps those onto `ra_sim::WeaponProfile` at spawn time, exactly as it maps
//! [`crate::rules::UnitStats`] onto `ra_sim::MoveStats` (the crate-layer split
//! keeps `ra-sim` from depending on the INI layer — §4.1).
//!
//! **Fidelity notes** (ported semantics):
//! - `Verses=` is a comma list of percentages stored as raw 16.16 `fixed`
//!   (`warhead.cpp` Read_INI): `100%` → 65536, `50%` → 32768.
//! - `Range=` is in cells (`Get_Lepton`, `ccini.cpp:276`): leptons = round(cells×256).
//! - `Speed=` is a 0..100 value scaled to a 0..255 leptons/tick MPH
//!   (`_Scale_To_256`, `ccini.cpp:246`); `255` is `MPH_LIGHT_SPEED`.
//! - A projectile with `Inviso=yes` **and** MPH 255 hits instantly
//!   (`bullet.cpp:787`) — the M60mg machine gun; the tank cannons fly straight.

use ra_formats::ini::Ini;

/// Number of armor classes, matching `ra_sim::combat::ARMOR_COUNT`.
pub const ARMOR_COUNT: usize = 5;

/// `MPH_LIGHT_SPEED` (`defines.h:1116`): the top speed value; an invisible
/// projectile at this speed is a hitscan weapon.
const MPH_LIGHT_SPEED: i32 = 255;

/// A unit's resolved combat data (plain numbers; the client lifts it into the
/// sim's `WeaponProfile`). `weapon` is `None` for unarmed units (e.g. HARV).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnitCombat {
    /// Armor class index (0=none … 3=steel/"heavy" … 4=concrete).
    pub armor: u8,
    /// Whether the unit aims an independent turret (vs rotating its whole body).
    pub has_turret: bool,
    /// The resolved primary weapon, if the unit is armed.
    pub weapon: Option<WeaponDef>,
    /// The resolved `Secondary=` weapon, if any (e.g. the mammoth tank's
    /// anti-infantry/air MammothTusk missiles alongside its 120mm cannon). The
    /// sim picks primary vs. secondary per target armor
    /// (`TechnoClass::What_Weapon_Should_I_Use`, `techno.cpp:360`).
    pub secondary: Option<WeaponDef>,
}

/// A fully-resolved weapon (weapon + warhead + projectile + general bounds),
/// carrying every number `ra_sim::WeaponProfile` needs. Fields mirror it 1:1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeaponDef {
    /// `Damage=` (base explosive load).
    pub damage: i32,
    /// `ROF=` rearm cooldown, ticks.
    pub rof: u16,
    /// `Range=` in leptons (cells × 256).
    pub range: i32,
    /// Projectile speed in leptons/tick (MPH; used for straight flight).
    pub proj_speed: i32,
    /// Projectile `ROT` (homing rate; 0 = dumb-fire straight).
    pub proj_rot: u8,
    /// Projectile `Inviso=yes` (no sprite).
    pub invisible: bool,
    /// Hitscan: invisible AND at light speed.
    pub instant: bool,
    /// Projectile `Arcing=yes` (grenade/artillery). False for starter weapons.
    pub arcing: bool,
    /// Whether the warhead is `AP` (drives ground/infantry inaccuracy scatter).
    pub warhead_ap: bool,
    /// Warhead `Spread=`.
    pub spread: i32,
    /// Warhead `Verses=` per-armor modifiers, raw 16.16.
    pub verses: [i32; ARMOR_COUNT],
    /// `[General] BallisticScatter`, leptons.
    pub ballistic_scatter: i32,
    /// `[General] HomingScatter`, leptons.
    pub homing_scatter: i32,
    /// `[General] MinDamage`.
    pub min_damage: i32,
    /// `[General] MaxDamage`.
    pub max_damage: i32,
}

/// Parse an ASCII fixed-point number into raw 16.16, matching `fixed(char const*)`
/// (`common/fixed.cpp:82`): a trailing `%` means `value/100`; a decimal point
/// splits whole and fractional parts. Non-numeric input yields 0.
fn parse_fixed_raw(s: &str) -> i64 {
    let s = s.trim();
    // Percentage form: atoi(digits) * 65536 / 100.
    if let Some(pos) = s.find('%') {
        let digits: i64 = s[..pos].trim().parse().unwrap_or(0);
        return digits * 65536 / 100;
    }
    // Decimal form: whole + fraction.
    let (whole_str, frac_str) = match s.split_once('.') {
        Some((w, f)) => (w, f),
        None => (s, ""),
    };
    let whole: i64 = whole_str.trim().parse().unwrap_or(0);
    let mut raw = whole * 65536;
    // Fraction: leading run of digits, value * 65536 / 10^len.
    let frac_digits: String = frac_str
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if !frac_digits.is_empty() {
        let frac: i64 = frac_digits.parse().unwrap_or(0);
        let base: i64 = 10i64.pow(frac_digits.len() as u32);
        raw += frac * 65536 / base;
    }
    raw
}

/// Round a raw 16.16 value multiplied by `mult` to an integer, matching
/// `fixed::operator*(int)` then `operator unsigned` (round-to-nearest).
fn fixed_mul_int_round(raw: i64, mult: i64) -> i32 {
    (((raw * mult) + 32768) / 65536) as i32
}

/// `Get_Lepton`: read a cells value (fixed) and convert to leptons (× 256).
fn get_lepton(ini: &Ini, section: &str, key: &str, default_leptons: i32) -> i32 {
    match ini.get(section, key) {
        Some(v) => fixed_mul_int_round(parse_fixed_raw(v), 256),
        None => default_leptons,
    }
}

/// `_Scale_To_256`: clamp `val` to 0..100 then scale to 0..255 (leptons/tick).
fn scale_to_256(val: i32) -> i32 {
    let val = val.clamp(0, 100);
    (val * 256 / 100).min(255)
}

/// Map an `Armor=` name to its class index (`ArmorName`, `const.cpp:138`).
pub fn armor_index(name: &str) -> u8 {
    match name.trim().to_ascii_lowercase().as_str() {
        "none" => 0,
        "wood" => 1,
        "light" => 2,
        "heavy" => 3,
        "concrete" => 4,
        _ => 0,
    }
}

/// Read a bool INI entry the way the engine does (`yes`/`true`/`1`).
fn get_bool(ini: &Ini, section: &str, key: &str) -> bool {
    matches!(
        ini.get(section, key).map(|v| v.trim().to_ascii_lowercase()),
        Some(ref v) if v == "yes" || v == "true" || v == "1"
    )
}

/// Parse a `Verses=` percentage list into raw-16.16 per-armor modifiers.
/// Missing/short lists default each entry to 100% (`warhead.cpp` default).
fn parse_verses(s: &str) -> [i32; ARMOR_COUNT] {
    let mut out = [65536i32; ARMOR_COUNT]; // 100%
    for (slot, tok) in out.iter_mut().zip(s.split(',')) {
        *slot = parse_fixed_raw(tok) as i32;
    }
    out
}

/// Resolve the combat data for unit type `name` from `rules`. Returns `None`
/// only if the unit section is absent; an armed unit with a missing/garbled
/// weapon resolves to `weapon = None` (unarmed) rather than failing.
pub fn resolve_unit_combat(rules: &Ini, name: &str) -> Option<UnitCombat> {
    if !rules.has_section(name) {
        return None;
    }
    let armor = armor_index(rules.get(name, "Armor").unwrap_or("none"));
    let weapon = rules
        .get(name, "Primary")
        .and_then(|w| resolve_weapon(rules, w));
    let secondary = rules
        .get(name, "Secondary")
        .and_then(|w| resolve_weapon(rules, w));
    Some(UnitCombat {
        armor,
        has_turret: turret_equipped(name),
        weapon,
        secondary,
    })
}

/// Resolve a `[WeaponName]` section (and its warhead + projectile) into a
/// [`WeaponDef`]. Returns `None` if the weapon section is missing or has no
/// `Damage`.
pub fn resolve_weapon(rules: &Ini, weapon_name: &str) -> Option<WeaponDef> {
    if !rules.has_section(weapon_name) {
        return None;
    }
    let damage = rules.get_int(weapon_name, "Damage")? as i32;
    let rof = rules
        .get_int(weapon_name, "ROF")
        .unwrap_or(0)
        .clamp(0, u16::MAX as i64) as u16;
    let range = get_lepton(rules, weapon_name, "Range", 0);
    let speed_raw = rules.get_int(weapon_name, "Speed").unwrap_or(0) as i32;
    let proj_speed = scale_to_256(speed_raw);

    let warhead_name = rules.get(weapon_name, "Warhead").unwrap_or("HE");
    let proj_name = rules.get(weapon_name, "Projectile").unwrap_or("Invisible");

    // Warhead.
    let (spread, verses) = if rules.has_section(warhead_name) {
        let spread = rules.get_int(warhead_name, "Spread").unwrap_or(1) as i32;
        let verses = rules
            .get(warhead_name, "Verses")
            .map(parse_verses)
            .unwrap_or([65536; ARMOR_COUNT]);
        (spread, verses)
    } else {
        (1, [65536; ARMOR_COUNT])
    };
    let warhead_ap = warhead_name.trim().eq_ignore_ascii_case("AP");

    // Projectile flags (defaults from the BulletTypeClass ctor: not invisible,
    // not arcing, ROT 0 — `bbdata.cpp`).
    let (invisible, arcing, proj_rot) = if rules.has_section(proj_name) {
        (
            get_bool(rules, proj_name, "Inviso"),
            get_bool(rules, proj_name, "Arcing"),
            rules.get_int(proj_name, "ROT").unwrap_or(0).clamp(0, 255) as u8,
        )
    } else {
        (false, false, 0)
    };
    let instant = invisible && proj_speed == MPH_LIGHT_SPEED;

    // General damage/scatter bounds.
    let min_damage = rules.get_int("General", "MinDamage").unwrap_or(1) as i32;
    let max_damage = rules.get_int("General", "MaxDamage").unwrap_or(1000) as i32;
    let ballistic_scatter = get_lepton(rules, "General", "BallisticScatter", 256);
    let homing_scatter = get_lepton(rules, "General", "HomingScatter", 512);

    Some(WeaponDef {
        damage,
        rof,
        range,
        proj_speed,
        proj_rot,
        invisible,
        instant,
        arcing,
        warhead_ap,
        spread,
        verses,
        ballistic_scatter,
        homing_scatter,
        min_damage,
        max_damage,
    })
}

/// Whether a unit type aims an independent turret. This flag is **not** in
/// rules.ini in the original (it lives in `udata.cpp`'s `is_turret_equipped`
/// ctor arg), so per DESIGN.md §3.8 it is a small named-capability table here
/// rather than 410 scattered type checks.
///
/// The list is the **authoritative** set from `udata.cpp` (verified ctor arg):
/// only the four battle tanks, the armed jeep, and the phase transport carry a
/// combat turret. **Everything else aims by rotating its whole body** — this
/// deliberately includes every infantry type, the V2/artillery launchers, the
/// APC, the minelayer/truck, and the specialty tanks (Tesla/Chrono/MAD), each
/// of which is `is_turret_equipped=false` in the original. The old `_ => armed`
/// default was wrong (it turreted infantry and the ARTY/APC); this closes that
/// gap (M7.7 P0c) so an infantryman correctly resolves `has_turret=false`.
pub fn turret_equipped(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_uppercase().as_str(),
        "1TNK" | "2TNK" | "3TNK" | "4TNK" | "JEEP" | "STNK"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed real rules.ini values for the starter units and their weapons.
    fn rules() -> Ini {
        Ini::parse(
            "[General]\nMinDamage=1\nMaxDamage=1000\nBallisticScatter=1.0\nHomingScatter=2.0\n\
             [2TNK]\nPrimary=90mm\nArmor=heavy\nStrength=400\nSpeed=8\nROT=5\n\
             [1TNK]\nPrimary=75mm\nArmor=heavy\nSpeed=9\n\
             [JEEP]\nPrimary=M60mg\nArmor=light\nSpeed=10\n\
             [HARV]\nArmor=heavy\nSpeed=6\n\
             [90mm]\nDamage=30\nROF=50\nRange=4.75\nProjectile=Cannon\nSpeed=40\nWarhead=AP\n\
             [75mm]\nDamage=25\nROF=40\nRange=4\nProjectile=Cannon\nSpeed=40\nWarhead=AP\n\
             [M60mg]\nDamage=15\nROF=20\nRange=4\nProjectile=Invisible\nSpeed=100\nWarhead=SA\n\
             [Cannon]\nImage=120MM\n\
             [Invisible]\nInviso=yes\nImage=none\n\
             [AP]\nSpread=3\nVerses=30%,75%,75%,100%,50%\n\
             [SA]\nSpread=3\nVerses=100%,50%,60%,25%,25%\n",
        )
    }

    #[test]
    fn parses_percent_and_decimal_fixed() {
        assert_eq!(parse_fixed_raw("100%"), 65536);
        assert_eq!(parse_fixed_raw("50%"), 32768);
        assert_eq!(parse_fixed_raw("25%"), 16384);
        // 4.75 cells * 256 = 1216 leptons.
        assert_eq!(fixed_mul_int_round(parse_fixed_raw("4.75"), 256), 1216);
        assert_eq!(fixed_mul_int_round(parse_fixed_raw("4"), 256), 1024);
    }

    #[test]
    fn scale_to_256_matches_engine() {
        assert_eq!(scale_to_256(40), 102);
        assert_eq!(scale_to_256(100), 255); // MPH_LIGHT_SPEED
        assert_eq!(scale_to_256(0), 0);
    }

    #[test]
    fn resolves_2tnk_90mm() {
        let r = rules();
        let c = resolve_unit_combat(&r, "2TNK").unwrap();
        assert_eq!(c.armor, 3); // heavy = steel
        assert!(c.has_turret);
        let w = c.weapon.unwrap();
        assert_eq!(w.damage, 30);
        assert_eq!(w.rof, 50);
        assert_eq!(w.range, 1216);
        assert_eq!(w.proj_speed, 102);
        assert!(!w.instant && !w.invisible);
        assert!(w.warhead_ap);
        assert_eq!(w.spread, 3);
        // Verses AP: [30,75,75,100,50]% as raw 16.16.
        assert_eq!(w.verses, [19660, 49152, 49152, 65536, 32768]);
    }

    #[test]
    fn resolves_m60mg_as_instant_invisible() {
        let r = rules();
        let c = resolve_unit_combat(&r, "JEEP").unwrap();
        assert_eq!(c.armor, 2); // light = aluminum
        let w = c.weapon.unwrap();
        assert!(w.invisible);
        assert!(w.instant); // Speed 100 -> MPH 255 && invisible
        assert!(!w.warhead_ap);
        assert_eq!(w.verses[3], 16384); // 25% vs steel
    }

    #[test]
    fn harvester_is_unarmed_turretless() {
        let r = rules();
        let c = resolve_unit_combat(&r, "HARV").unwrap();
        assert!(c.weapon.is_none());
        assert!(!c.has_turret);
        assert_eq!(c.armor, 3);
    }

    #[test]
    fn general_bounds_and_scatter() {
        let r = rules();
        let w = resolve_unit_combat(&r, "2TNK").unwrap().weapon.unwrap();
        assert_eq!(w.min_damage, 1);
        assert_eq!(w.max_damage, 1000);
        assert_eq!(w.ballistic_scatter, 256); // 1.0 cell
        assert_eq!(w.homing_scatter, 512); // 2.0 cells
    }
}
