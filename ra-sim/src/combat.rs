//! Combat data and math for the sim (DESIGN.md §4.4, M4). This module owns the
//! *values* a unit carries into a fight (resolved from rules.ini by `ra-data`,
//! then stored on the unit like [`crate::unit::MoveStats`]) and the *pure*
//! damage arithmetic ported from the original `Modify_Damage`. The stateful
//! systems — targeting, turret rotation, firing, bullet flight, death — live in
//! [`crate::world`], which is the only place `World` is mutated (§4.4).
//!
//! Everything here is integer/fixed-point and deterministic: no floats, no
//! wall-clock, no OS randomness (§4.2). The sim RNG is consumed only where the
//! original consumes its sync RNG — see [`crate::world`]'s bullet-scatter path.

use crate::arena::Handle;
use crate::coords::{CellCoord, Facing};
use crate::hash::Fnv1a;

/// Number of armor classes (`ARMOR_COUNT`, `defines.h:2753`): none, wood,
/// aluminum ("light"), steel ("heavy"), concrete — in that fixed order, which
/// is also the order of a warhead's `Verses=` list.
pub const ARMOR_COUNT: usize = 5;

/// Leptons per screen pixel (`PIXEL_LEPTON_W = ICON_LEPTON_W / ICON_PIXEL_W =
/// 256 / 24 = 10`, `display.h:52`). Load-bearing in the damage-falloff divisor.
pub const PIXEL_LEPTON_W: i32 = 10;

/// What a unit is trying to shoot. A [`Handle`] tracks a live enemy unit; a
/// [`CellCoord`] is a **force-fire** ground target (§ task: "target = unit
/// handle or cell for force-fire").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// An enemy unit, addressed by generational handle (goes stale on death).
    Unit(Handle),
    /// A ground cell (force-fire). Always "in range" of being aimed at.
    Cell(CellCoord),
}

/// A warhead's damage character: how it scales against each armor class and how
/// fast its damage falls off with distance. Ported from `WarheadTypeClass`
/// (`warhead.h`); `verses` holds the `Verses=` matrix as raw 16.16 modifiers
/// (`fixed`), so `100%` is `65536`, `50%` is `32768` (`warhead.cpp` Read_INI).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WarheadProfile {
    /// `Spread=` — larger spreads damage over a wider radius (slower falloff).
    pub spread: i32,
    /// Per-armor damage modifiers, raw 16.16 (`Modifier[ARMOR_COUNT]`).
    pub verses: [i32; ARMOR_COUNT],
}

impl WarheadProfile {
    pub(crate) fn hash_into(&self, h: &mut Fnv1a) {
        h.write_i32(self.spread);
        for v in &self.verses {
            h.write_i32(*v);
        }
    }
}

/// A unit's resolved weapon — the numbers it needs to fight, copied onto the
/// unit at spawn so a tick never reaches back into a type table (mirrors
/// [`crate::unit::MoveStats`]). Assembled by `ra-data` from the `[WeaponName]`,
/// `[WarheadName]`, and `[Projectile]` rules.ini sections plus the `[General]`
/// damage bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WeaponProfile {
    /// Base damage (`Damage=`, the projectile's explosive load).
    pub damage: i32,
    /// Rate-of-fire cooldown in ticks (`ROF=`); the rearm timer after a shot.
    pub rof: u16,
    /// Maximum range in leptons (`Range=` cells × 256).
    pub range: i32,
    /// Projectile speed in leptons/tick for straight flight (`Speed=` → MPH).
    pub proj_speed: i32,
    /// Projectile turn rate (`ROT`): non-zero means a homing missile (loosens
    /// the firing-alignment tolerance). Zero for all starter weapons.
    pub proj_rot: u8,
    /// `Inviso=yes`: the projectile draws no sprite (client shows a tracer).
    pub invisible: bool,
    /// True when the projectile hits instantly with no visible flight —
    /// `MaxSpeed == MPH_LIGHT_SPEED (255) && IsInvisible` (`bullet.cpp:787`).
    /// The M60mg machine gun (Speed=100 → MPH 255, Invisible) is such a weapon;
    /// the tank cannons (Speed=40) are not and fly a straight bullet.
    pub instant: bool,
    /// The warhead attached to this weapon's projectile.
    pub warhead: WarheadProfile,
    /// True if the warhead is armor-piercing (`Warhead=AP`). AP shots at a
    /// ground cell or at infantry are inherently inaccurate and scatter
    /// (`bullet.cpp:763`), which is where combat consumes the sim RNG.
    pub warhead_ap: bool,
    /// `IsArcing` projectile (grenade/artillery). Selects the homing-scatter
    /// branch. False for every starter weapon (all flat-trajectory).
    pub arcing: bool,
    /// `[General] BallisticScatter` in leptons (default 256 = 1 cell) — the
    /// scatter cap for inaccurate flat projectiles.
    pub ballistic_scatter: i32,
    /// `[General] HomingScatter` in leptons (default 512) — cap for arcing shots.
    pub homing_scatter: i32,
    /// `[General] MinDamage` (default 1) — the damage floor at close range.
    pub min_damage: i32,
    /// `[General] MaxDamage` (default 1000) — the per-shot damage ceiling.
    pub max_damage: i32,
}

impl WeaponProfile {
    pub(crate) fn hash_into(&self, h: &mut Fnv1a) {
        h.write_i32(self.damage);
        h.write_u16(self.rof);
        h.write_i32(self.range);
        h.write_i32(self.proj_speed);
        h.write_u8(self.proj_rot);
        h.write_u8(self.invisible as u8);
        h.write_u8(self.instant as u8);
        self.warhead.hash_into(h);
        h.write_u8(self.warhead_ap as u8);
        h.write_u8(self.arcing as u8);
        h.write_i32(self.ballistic_scatter);
        h.write_i32(self.homing_scatter);
        h.write_i32(self.min_damage);
        h.write_i32(self.max_damage);
    }
}

/// Whether the turret/body is aimed closely enough at `dir` to fire. Port of
/// `UnitClass::Can_Fire`'s alignment gate (`unit.cpp:4360`): the facing
/// difference must be `< 8` binary-angle units; a homing projectile
/// (`proj_rot != 0`) quarters the difference first, so it fires much sooner.
pub fn aligned_to_fire(current: Facing, dir: Facing, proj_rot: u8) -> bool {
    let mut diff = current.difference(dir).abs();
    if proj_rot != 0 {
        diff >>= 2;
    }
    diff < 8
}

/// Port of `Modify_Damage` (`combat.cpp:68`). Applies the warhead-vs-armor
/// modifier, then the distance falloff with the original's **hardcoded**
/// divisor and clamp, then the `[General]` Min/Max damage bounds. `distance` is
/// in leptons from the point of impact to the object being damaged.
///
/// The constants are the original's, kept verbatim:
/// - `PIXEL_LEPTON_W / 4 == 2` (no-spread divisor) and
///   `SpreadFactor * (PIXEL_LEPTON_W / 2) == SpreadFactor * 5` (`combat.cpp:108-112`),
/// - the `Bound(distance, 0, 16)` clamp (`combat.cpp:113`),
/// - the `distance < 4 → max(damage, MinDamage)` close-range floor
///   (`combat.cpp:123`), and the final `min(damage, MaxDamage)` (`combat.cpp:127`).
///
/// Healing (negative damage) is out of scope for M4; a negative `damage` is
/// returned unmodified only at point-blank, matching the pre-CS branch.
pub fn modify_damage(
    mut damage: i32,
    warhead: &WarheadProfile,
    armor: u8,
    mut distance: i32,
    min_damage: i32,
    max_damage: i32,
) -> i32 {
    if damage == 0 {
        return 0;
    }

    // Negative damage (heal) is applied full strength only at point-blank
    // against unarmored targets (combat.cpp:83-96, pre-FIXIT_CSII branch).
    if damage < 0 {
        if distance < 0x008 && armor == 0 {
            return damage;
        }
        return 0;
    }

    // Warhead-vs-armor modifier: damage * Modifier[armor], where Modifier is a
    // raw 16.16 fixed. `fixed::operator unsigned` rounds to nearest:
    // (damage*raw + 32768) / 65536 (combat.cpp:101, fixed.h operator unsigned).
    let modifier = warhead.verses[armor as usize] as i64;
    damage = (((damage as i64) * modifier + 32768) / 65536) as i32;

    // Distance falloff (combat.cpp:106-125).
    if damage != 0 {
        if warhead.spread == 0 {
            distance /= PIXEL_LEPTON_W / 4; // == 2
        } else {
            distance /= warhead.spread * (PIXEL_LEPTON_W / 2); // == spread * 5
        }
        distance = distance.clamp(0, 16);
        if distance != 0 {
            damage /= distance;
        }
        // Below quarter-range the shot always does at least MinDamage.
        if distance < 4 {
            damage = damage.max(min_damage);
        }
    }

    damage.min(max_damage)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AP warhead (2TNK's 90mm) `Verses` from rules.ini, as raw 16.16.
    fn ap() -> WarheadProfile {
        WarheadProfile {
            spread: 3,
            verses: pct5([30, 75, 75, 100, 50]),
        }
    }
    /// SA warhead (JEEP's M60mg) `Verses`.
    fn sa() -> WarheadProfile {
        WarheadProfile {
            spread: 3,
            verses: pct5([100, 50, 60, 25, 25]),
        }
    }
    /// Percentages → raw 16.16 the same way `fixed("NN%")` does: NN*65536/100.
    fn pct5(p: [i32; 5]) -> [i32; 5] {
        let mut out = [0i32; 5];
        for (o, v) in out.iter_mut().zip(p) {
            *o = v * 65536 / 100;
        }
        out
    }

    #[test]
    fn ap_vs_heavy_at_point_blank() {
        // 90mm base 30, AP vs steel/"heavy" (armor 3) = 100% → 30, no falloff.
        assert_eq!(modify_damage(30, &ap(), 3, 0, 1, 1000), 30);
    }

    #[test]
    fn ap_vs_light_rounds_to_nearest() {
        // 30 * 75% = 22.5 → rounds up to 23 (fixed operator unsigned adds 0.5).
        assert_eq!(modify_damage(30, &ap(), 2, 0, 1, 1000), 23);
    }

    #[test]
    fn sa_vs_heavy_is_quarter() {
        // M60mg base 15, SA vs steel = 25% → 3.75 → rounds to 4.
        assert_eq!(modify_damage(15, &sa(), 3, 0, 1, 1000), 4);
    }

    #[test]
    fn falloff_reduces_at_distance() {
        // AP vs steel, base 30, impact 200 leptons away, spread 3:
        // distance = 200 / (3*5) = 13 → damage = 30/13 = 2 (integer),
        // distance >= 4 so no MinDamage floor.
        assert_eq!(modify_damage(30, &ap(), 3, 200, 1, 1000), 2);
    }

    #[test]
    fn close_range_floor_applies() {
        // Small but nonzero modified damage at distance < 4 → floored to
        // MinDamage. 3 * 50% = 2 (nonzero), then max(2, MinDamage=5) = 5.
        let weak = WarheadProfile {
            spread: 100,
            verses: pct5([50, 50, 50, 50, 50]),
        };
        assert_eq!(modify_damage(3, &weak, 0, 0, 5, 1000), 5);
    }

    #[test]
    fn modifier_rounding_to_zero_yields_zero_no_floor() {
        // If the warhead modifier rounds the damage to 0, the MinDamage floor
        // does NOT rescue it (the `if (damage)` block is skipped) — faithful to
        // combat.cpp. 1 * 1% rounds to 0.
        let tiny = WarheadProfile {
            spread: 100,
            verses: pct5([1, 1, 1, 1, 1]),
        };
        assert_eq!(modify_damage(1, &tiny, 0, 0, 5, 1000), 0);
    }

    #[test]
    fn max_damage_caps() {
        assert_eq!(modify_damage(5000, &ap(), 3, 0, 1, 1000), 1000);
    }

    #[test]
    fn zero_damage_is_zero() {
        assert_eq!(modify_damage(0, &ap(), 3, 0, 1, 1000), 0);
    }

    #[test]
    fn alignment_gate() {
        // Non-homing: must be within 8 binary-angle units.
        assert!(aligned_to_fire(Facing(0), Facing(7), 0));
        assert!(!aligned_to_fire(Facing(0), Facing(8), 0));
        // Homing quarters the difference: 8>>2 = 2 < 8 → fires.
        assert!(aligned_to_fire(Facing(0), Facing(8), 20));
    }
}
