//! Projectiles in flight (DESIGN.md §5 "Entity arenas: per-kind" — Bullets get
//! their own arena, mirroring the original's `Bullets` heap). A [`Bullet`] is a
//! plain integer struct: it flies straight from muzzle to a pre-computed impact
//! point at its weapon's speed, or (for a hitscan weapon) exists already at the
//! impact point and detonates the same tick. On detonation the sim applies
//! [`crate::combat::modify_damage`] to the target. Death anims and craters are a
//! deliberate M7 seam (see [`crate::world`]).

use crate::arena::Handle;
use crate::combat::{Target, WarheadProfile};
use crate::coords::{Facing, WorldCoord};
use crate::hash::Fnv1a;

/// A projectile mid-flight. Deterministic straight-line motion (no floats):
/// each tick it advances `speed` leptons toward `impact`, detonating on arrival.
#[derive(Clone, Debug)]
pub struct Bullet {
    /// Current position in leptons.
    pub pos: WorldCoord,
    /// Pre-computed impact point (already scattered if the shot was inaccurate).
    pub impact: WorldCoord,
    /// What the shot is aimed at: a live unit, a live building, or a ground cell
    /// (force-fire). Damage is applied to the entity target if it is still live
    /// at detonation; a `Cell` target detonates harmlessly (M6 seam).
    pub target: Target,
    /// Straight-flight speed in leptons per tick.
    pub speed: i32,
    /// Direction of travel (for the client's tracer/sprite; sim uses `impact`).
    pub facing: Facing,
    /// Base damage carried (`Damage=`).
    pub damage: i32,
    /// The warhead applied on detonation (armor modifier + falloff spread).
    pub warhead: WarheadProfile,
    /// `[General] MinDamage` floor passed through to `modify_damage`.
    pub min_damage: i32,
    /// `[General] MaxDamage` ceiling passed through to `modify_damage`.
    pub max_damage: i32,
    /// House that fired the shot (for future friendly-fire / scoring rules).
    pub source_house: u8,
    /// The unit that fired the shot. Used to (a) exclude the shooter from its
    /// own blast (`object != source`, `combat.cpp:203`) and (b) name the
    /// retaliation target for units the blast wakes up (`FootClass::Take_Damage`
    /// → `Assign_Target(source)`, `foot.cpp:1189`). The handle may be stale by
    /// detonation (the shooter died mid-flight); consumers check liveness.
    pub source_unit: Handle,
    /// Hitscan flag: detonate on the first bullet step, no visible flight
    /// (`MaxSpeed == MPH_LIGHT_SPEED && IsInvisible`, `bullet.cpp:787`).
    pub instant: bool,
    /// `Inviso=yes`: no projectile sprite (client renders a brief tracer).
    pub invisible: bool,
}

impl Bullet {
    /// Advance one tick along the straight line to `impact`. Returns `true` when
    /// the bullet has reached its impact point and should detonate this tick.
    /// Instant (hitscan) bullets detonate immediately without moving.
    pub fn advance(&mut self) -> bool {
        if self.instant {
            self.pos = self.impact;
            return true;
        }
        let dx = (self.impact.x.0 - self.pos.x.0) as i64;
        let dy = (self.impact.y.0 - self.pos.y.0) as i64;
        let dist2 = dx * dx + dy * dy;
        let step = self.speed.max(1) as i64;
        if dist2 <= step * step {
            // Within one step: snap to impact and detonate.
            self.pos = self.impact;
            return true;
        }
        // Partial step along the straight line (integer, deterministic).
        let dist = crate::coords::isqrt(dist2);
        let nx = self.pos.x.0 + (dx * step / dist) as i32;
        let ny = self.pos.y.0 + (dy * step / dist) as i32;
        self.pos = WorldCoord::new(nx, ny);
        false
    }

    pub(crate) fn hash_into(&self, h: &mut Fnv1a) {
        h.write_i32(self.pos.x.0);
        h.write_i32(self.pos.y.0);
        h.write_i32(self.impact.x.0);
        h.write_i32(self.impact.y.0);
        match self.target {
            Target::Unit(handle) => {
                h.write_u8(1);
                h.write_u32(handle.index);
                h.write_u32(handle.gen);
            }
            Target::Building(handle) => {
                h.write_u8(2);
                h.write_u32(handle.index);
                h.write_u32(handle.gen);
            }
            Target::Cell(c) => {
                h.write_u8(3);
                h.write_i32(c.x);
                h.write_i32(c.y);
            }
        }
        h.write_i32(self.speed);
        h.write_u8(self.facing.0);
        h.write_i32(self.damage);
        self.warhead.hash_into(h);
        h.write_i32(self.min_damage);
        h.write_i32(self.max_damage);
        h.write_u8(self.source_house);
        h.write_u32(self.source_unit.index);
        h.write_u32(self.source_unit.gen);
        h.write_u8(self.instant as u8);
        h.write_u8(self.invisible as u8);
    }
}
