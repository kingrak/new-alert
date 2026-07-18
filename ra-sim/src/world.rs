//! `World` — the single explicit game-state value (DESIGN.md §3.2, §4.2) and
//! the command pipeline that is the *only* way to mutate it (§4.4).
//!
//! Each tick runs a fixed, explicit sequence of systems — **commands, then
//! movement** — over arenas iterated in slot order, with one seeded RNG owned
//! here. At the end of a tick the whole mutable state is folded into a 64-bit
//! FNV-1a hash (§4.2): the hash chain is the determinism backbone, asserted in
//! replays and multiplayer alike.

use crate::arena::{Arena, Handle};
use crate::bullet::Bullet;
use crate::combat::{aligned_to_fire, modify_damage, Target};
use crate::coords::{coord_move, isqrt, leptons_distance, CellCoord, Facing, WorldCoord};
use crate::hash::Fnv1a;
use crate::path::{find_path, Passability};
use crate::rng::RandomLcg;
use crate::unit::{MoveStats, Unit};

/// A player order. Every command carries the **issuing house** explicitly
/// (§4.6): ownership is validated by the sim, never inferred from a connection,
/// so the same schema serves single-player, LAN, and relay play.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Order `unit` to move to `dest` (a cell). Ignored if the issuing house
    /// does not own the unit, the unit is stale, or no path exists. Clears any
    /// attack target (a move order overrides an attack, like the original).
    Move {
        /// The unit to move.
        unit: Handle,
        /// Destination cell.
        dest: CellCoord,
        /// House issuing the order (must own `unit`).
        house: u8,
    },
    /// Order `unit` to halt where it is (and stop attacking).
    Stop {
        /// The unit to stop.
        unit: Handle,
        /// House issuing the order (must own `unit`).
        house: u8,
    },
    /// Order `unit` to attack `target` — an enemy unit handle, or a ground cell
    /// for force-fire. Ignored if the issuing house does not own `unit`, the
    /// unit is stale, or the unit has no weapon. The unit will approach until in
    /// range, aim, and fire on ROF cadence. This is the TarCom assignment.
    Attack {
        /// The attacking unit (must belong to `house` and be armed).
        unit: Handle,
        /// What to shoot at.
        target: Target,
        /// House issuing the order (must own `unit`).
        house: u8,
    },
}

/// The complete simulation state. Fields are plain and serialisable; there are
/// no back-pointers, no `HashMap` iteration, no floats.
#[derive(Clone, Debug)]
pub struct World {
    /// Live movable units, addressed by generational handle.
    pub units: Arena<Unit>,
    /// Projectiles in flight (their own arena, per §5's per-kind arena plan).
    pub bullets: Arena<Bullet>,
    /// Map passability grid (derived from terrain by `ra-data`).
    passable: Passability,
    /// The sim RNG, seeded and owned here.
    rng: RandomLcg,
    /// The current tick number (advances once per [`World::tick`]).
    tick_count: u32,
}

impl World {
    /// Create a world over a passability grid, seeding the sim RNG.
    pub fn new(passable: Passability, seed: u32) -> World {
        World {
            units: Arena::new(),
            bullets: Arena::new(),
            passable,
            rng: RandomLcg::new(seed),
            tick_count: 0,
        }
    }

    /// The current tick number.
    pub fn tick_count(&self) -> u32 {
        self.tick_count
    }

    /// Borrow the passability grid.
    pub fn passability(&self) -> &Passability {
        &self.passable
    }

    /// Read-only view of the sim RNG seed (also folded into the state hash).
    pub fn rng_seed(&self) -> u32 {
        self.rng.seed()
    }

    /// Spawn a unit at a cell, returning its handle.
    pub fn spawn_unit(
        &mut self,
        type_id: u32,
        house: u8,
        cell: CellCoord,
        facing: Facing,
        health: u16,
        stats: MoveStats,
    ) -> Handle {
        self.units
            .insert(Unit::new(type_id, house, cell, facing, health, stats))
    }

    /// Attach resolved combat stats (armor, weapon, turret) to an already-spawned
    /// unit. Separate from [`World::spawn_unit`] so movement-only callers and
    /// tests are unaffected; the client calls this right after spawning.
    pub fn set_unit_combat(
        &mut self,
        unit: Handle,
        armor: u8,
        weapon: Option<crate::combat::WeaponProfile>,
        has_turret: bool,
    ) {
        if let Some(u) = self.units.get_mut(unit) {
            u.set_combat(armor, weapon, has_turret);
        }
    }

    /// Set a spawned unit's maximum strength (for the client's health bar).
    pub fn set_unit_max_health(&mut self, unit: Handle, max_health: u16) {
        if let Some(u) = self.units.get_mut(unit) {
            u.set_max_health(max_health);
        }
    }

    /// Advance one tick: apply `commands` (in order), run movement, then return
    /// the post-tick state hash. This is the function replays and the lockstep
    /// net layer drive; the returned hash is chained and compared.
    pub fn tick(&mut self, commands: &[Command]) -> u64 {
        apply(self, self.tick_count, commands);
        self.state_hash()
    }

    /// Fold all mutable state into a 64-bit hash, in a fixed field order.
    pub fn state_hash(&self) -> u64 {
        let mut h = Fnv1a::new();
        h.write_u32(self.tick_count);
        h.write_u32(self.rng.seed());
        h.write_u32(self.units.len());
        for (handle, unit) in self.units.iter() {
            h.write_u32(handle.index);
            h.write_u32(handle.gen);
            unit.hash_into(&mut h);
        }
        h.write_u32(self.bullets.len());
        for (handle, bullet) in self.bullets.iter() {
            h.write_u32(handle.index);
            h.write_u32(handle.gen);
            bullet.hash_into(&mut h);
        }
        h.finish()
    }
}

/// Apply one tick's worth of systems to `world`, in the canonical fixed order:
///
/// 1. **commands** — apply player/AI orders (in the given canonical order),
/// 2. **combat** — targeting, turret/body rotation, and firing (spawns bullets,
///    consumes the sim RNG on inaccurate shots — see [`run_combat`]),
/// 3. **movement** — advance units along their paths,
/// 4. **bullets** — advance projectiles, detonate, apply damage, remove the dead.
///
/// This fixed, explicit order is itself a determinism requirement (§4.2). This
/// is the single mutation entry point for the sim (§4.4). `tick` must equal the
/// world's current tick.
pub fn apply(world: &mut World, tick: u32, commands: &[Command]) {
    debug_assert_eq!(
        tick, world.tick_count,
        "commands applied to the wrong tick (replay/order bug)"
    );

    // System 1: commands. Applied in the given (canonical) order.
    for &cmd in commands {
        apply_command(world, cmd);
    }

    // System 2: combat (targeting + rotation + firing).
    run_combat(world);

    // System 3: movement.
    move_units(world);

    // System 4: bullets (flight + detonation + damage + death).
    run_bullets(world);

    world.tick_count = world.tick_count.wrapping_add(1);
}

/// Validate and enact a single command.
fn apply_command(world: &mut World, cmd: Command) {
    match cmd {
        Command::Move { unit, dest, house } => {
            // Ownership check (§4.6): silently ignore orders for units the
            // issuing house does not own, or stale handles.
            let start = match world.units.get(unit) {
                Some(u) if u.house == house => u.cell(),
                _ => return,
            };
            if let Some(path) = find_path(&world.passable, start, dest) {
                if let Some(u) = world.units.get_mut(unit) {
                    u.path = path;
                    u.dest = Some(dest);
                    u.target = None; // a move order overrides an attack
                }
            }
        }
        Command::Stop { unit, house } => {
            if let Some(u) = world.units.get_mut(unit) {
                if u.house == house {
                    u.path.clear();
                    u.dest = None;
                    u.target = None;
                }
            }
        }
        Command::Attack {
            unit,
            target,
            house,
        } => {
            // Reject the order up front for unowned/stale/unarmed units, and for
            // targeting oneself. Otherwise store the TarCom; `run_combat` drives
            // the approach/aim/fire each tick.
            let ok = match world.units.get(unit) {
                Some(u) => u.house == house && u.weapon.is_some(),
                None => false,
            };
            if !ok {
                return;
            }
            if let Target::Unit(t) = target {
                if t == unit || !world.units.contains(t) {
                    return;
                }
            }
            if let Some(u) = world.units.get_mut(unit) {
                u.target = Some(target);
                // Clear a stale movement destination; approach is driven by the
                // combat system toward the target, not a prior move order.
                u.dest = None;
                u.path.clear();
            }
        }
    }
}

/// Combat system: for each unit (in slot order) decrement its rearm timer,
/// rotate its turret/body toward its target, approach if out of range, and fire
/// when aimed and rearmed. Ported from `UnitClass::Rotation_AI` +
/// `Firing_AI` + `Can_Fire` (`unit.cpp`). Iterating in slot order keeps the
/// sim-RNG draw sequence (bullet scatter) deterministic.
fn run_combat(world: &mut World) {
    for handle in world.units.handles() {
        // Decrement the rearm countdown regardless of whether we fire.
        if let Some(u) = world.units.get_mut(handle) {
            if u.arm > 0 {
                u.arm -= 1;
            }
        }

        // Snapshot what we need without holding a borrow across the RNG draw.
        let (weapon, coord, turret, body, has_turret, rot, target) = match world.units.get(handle) {
            Some(u) => match (u.target, u.weapon) {
                (Some(t), Some(w)) => (
                    w,
                    u.coord,
                    u.turret_facing,
                    u.facing,
                    u.has_turret,
                    u.stats.rot,
                    t,
                ),
                _ => continue,
            },
            None => continue,
        };

        // Resolve the target's current aim point; drop stale/dead unit targets.
        let target_coord = match target {
            Target::Unit(t) => match world.units.get(t) {
                Some(tu) if tu.is_alive() => tu.coord,
                _ => {
                    if let Some(u) = world.units.get_mut(handle) {
                        u.target = None;
                    }
                    continue;
                }
            },
            Target::Cell(c) => c.center(),
        };

        // Desired aim direction toward the target.
        let desired = Facing::toward(coord, target_coord);

        // Rotate turret (turreted) or body (turretless) toward the target.
        if let Some(desired) = desired {
            if let Some(u) = world.units.get_mut(handle) {
                if has_turret {
                    u.turret_facing = u.turret_facing.rotate_toward(desired, rot.wrapping_add(1));
                } else {
                    u.facing = u.facing.rotate_toward(desired, rot.wrapping_add(1));
                    u.turret_facing = u.facing;
                }
            }
        }

        // Range check uses the original's octagonal `Distance` metric.
        let dist = leptons_distance(coord, target_coord);
        let in_range = dist <= weapon.range;

        if !in_range {
            // Approach: path toward the target's cell if we aren't already.
            let goal = target_coord.cell();
            let need_path = world
                .units
                .get(handle)
                .map(|u| u.path.is_empty() || u.dest != Some(goal))
                .unwrap_or(false);
            if need_path {
                if let Some(path) = find_path(&world.passable, coord.cell(), goal) {
                    if let Some(u) = world.units.get_mut(handle) {
                        u.path = path;
                        u.dest = Some(goal);
                    }
                }
            }
            continue;
        }

        // In range: hold position (stop approaching) and try to fire.
        if let Some(u) = world.units.get_mut(handle) {
            u.path.clear();
            u.dest = None;
        }

        let aim = if has_turret { turret } else { body };
        let arm_ready = world.units.get(handle).map(|u| u.arm == 0).unwrap_or(false);
        let aligned = desired
            .map(|d| aligned_to_fire(aim, d, weapon.proj_rot))
            .unwrap_or(true);

        if arm_ready && aligned {
            fire(world, handle, coord, aim, target, target_coord, &weapon);
            if let Some(u) = world.units.get_mut(handle) {
                u.arm = weapon.rof;
            }
        }
    }
}

/// Spawn one projectile from `shooter` at `target`. Computes the impact point,
/// applying the original's inaccuracy scatter for AP shots at ground/infantry —
/// the one combat path that consumes the sim RNG (`bullet.cpp:763-782`,
/// `Random_Pick(0, scatterdist)` = `Scen.RandomNumber`, the sync RNG). Accurate
/// shots (any shot at a vehicle) draw no RNG, exactly as the original.
#[allow(clippy::too_many_arguments)]
fn fire(
    world: &mut World,
    shooter: Handle,
    muzzle: WorldCoord,
    aim: Facing,
    target: Target,
    target_coord: WorldCoord,
    weapon: &crate::combat::WeaponProfile,
) {
    let source_house = world.units.get(shooter).map(|u| u.house).unwrap_or(0);

    // Direction the projectile is launched (toward the target; non-homing
    // bullets fire straight at it — `bullet.cpp:751`).
    let dir = Facing::toward(muzzle, target_coord).unwrap_or(aim);

    // Inaccuracy: an AP warhead trained on a ground cell (or infantry — none in
    // M4) scatters. Flat (non-arcing) projectiles use the ballistic branch.
    let is_ground = matches!(target, Target::Cell(_));
    let inaccurate = weapon.warhead_ap && is_ground;
    let impact = if inaccurate && !weapon.arcing {
        // scatterdist = (Distance/16) - 0x40, capped at BallisticScatter, >= 0.
        let d = leptons_distance(muzzle, target_coord);
        let mut scatterdist = (d / 16) - 0x40;
        scatterdist = scatterdist.min(weapon.ballistic_scatter).max(0);
        // Genuine sim-RNG draw (skips the draw when scatterdist == 0, matching
        // RandomClass::operator()(min,max) returning min without a draw).
        let offset = world.rng.range(0, scatterdist);
        coord_move(target_coord, dir, offset)
    } else {
        target_coord
    };

    let target_handle = match target {
        Target::Unit(t) => Some(t),
        Target::Cell(_) => None,
    };

    let bullet = Bullet {
        pos: if weapon.instant { impact } else { muzzle },
        impact,
        target: target_handle,
        speed: weapon.proj_speed,
        facing: dir,
        damage: weapon.damage,
        warhead: weapon.warhead,
        min_damage: weapon.min_damage,
        max_damage: weapon.max_damage,
        source_house,
        instant: weapon.instant,
        invisible: weapon.invisible,
    };
    world.bullets.insert(bullet);
}

/// Bullet system: advance every projectile; on detonation apply damage to its
/// target (with distance falloff from the actual impact point) and remove any
/// unit whose health reaches zero. Processed in slot order.
///
/// **Death seam (M4 → M7).** A unit at zero health is removed from the arena
/// here. Death animations, wreck/crater smudges, and score/credit effects are a
/// deliberate later-milestone TODO — the removal point is the single seam they
/// will hook.
fn run_bullets(world: &mut World) {
    let mut dead: Vec<Handle> = Vec::new();
    for handle in world.bullets.handles() {
        let detonated = match world.bullets.get_mut(handle) {
            Some(b) => b.advance(),
            None => continue,
        };
        if !detonated {
            continue;
        }
        // Detonate: pull the bullet out and apply its damage.
        if let Some(b) = world.bullets.remove(handle) {
            if let Some(t) = b.target {
                if let Some(tu) = world.units.get(t) {
                    if tu.is_alive() {
                        let distance = leptons_distance(b.impact, tu.coord);
                        let dmg = modify_damage(
                            b.damage,
                            &b.warhead,
                            tu.armor,
                            distance,
                            b.min_damage,
                            b.max_damage,
                        );
                        if let Some(tu) = world.units.get_mut(t) {
                            tu.health = tu.health.saturating_sub(dmg.max(0) as u16);
                            if tu.health == 0 && !dead.contains(&t) {
                                dead.push(t);
                            }
                        }
                    }
                }
            }
            // Force-fire at an empty cell (no unit target) detonates harmlessly.
            // Area/splash damage to bystanders is a documented M7 TODO.
        }
    }
    // Remove the dead (their handles go stale; attackers drop the target next
    // tick via the stale-handle check in `run_combat`).
    for h in dead {
        world.units.remove(h);
    }
}

/// Advance every moving unit along its path by up to its per-tick speed,
/// rotating its facing toward the heading. Units are processed in slot order.
fn move_units(world: &mut World) {
    for handle in world.units.handles() {
        let Some(unit) = world.units.get_mut(handle) else {
            continue;
        };
        if unit.path.is_empty() {
            continue;
        }

        // Rotate toward the next waypoint before translating.
        let target = unit.path[0].center();
        if let Some(desired) = Facing::toward(unit.coord, target) {
            unit.facing = unit
                .facing
                .rotate_toward(desired, unit.stats.rot.wrapping_add(1));
        }

        // Consume this tick's movement budget, possibly across several short
        // waypoints (robust even if a future unit is faster than one cell/tick).
        let mut budget = unit.stats.max_speed;
        while budget > 0 && !unit.path.is_empty() {
            let target = unit.path[0].center();
            let dx = (target.x.0 - unit.coord.x.0) as i64;
            let dy = (target.y.0 - unit.coord.y.0) as i64;
            let dist = isqrt(dx * dx + dy * dy) as i32;
            if dist <= budget {
                // Reached this waypoint exactly.
                unit.coord = target;
                budget -= dist.max(0);
                unit.path.remove(0);
                if unit.path.is_empty() {
                    unit.dest = None;
                }
            } else {
                // Advance a partial step along the straight line to the target.
                let nx = unit.coord.x.0 + (dx * budget as i64 / dist as i64) as i32;
                let ny = unit.coord.y.0 + (dy * budget as i64 / dist as i64) as i32;
                unit.coord = WorldCoord::new(nx, ny);
                budget = 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats() -> MoveStats {
        // JEEP-like: Speed=10 -> 25 leptons/tick, ROT=10.
        MoveStats {
            max_speed: 25,
            rot: 10,
        }
    }

    fn world() -> World {
        World::new(Passability::all_passable(), 0x1234)
    }

    // --- Combat test helpers (real rules.ini values for the starter weapons) ---
    use crate::combat::{Target, WarheadProfile, WeaponProfile};

    fn pct5(p: [i32; 5]) -> [i32; 5] {
        let mut o = [0i32; 5];
        for (d, v) in o.iter_mut().zip(p) {
            *d = v * 65536 / 100;
        }
        o
    }

    /// 2TNK's 90mm cannon (AP, Damage 30, ROF 50, Range 4.75 cells, Speed 40).
    fn ninety_mm() -> WeaponProfile {
        WeaponProfile {
            damage: 30,
            rof: 50,
            range: 1216, // 4.75 * 256
            proj_speed: 102,
            proj_rot: 0,
            invisible: false,
            instant: false,
            warhead: WarheadProfile {
                spread: 3,
                verses: pct5([30, 75, 75, 100, 50]),
            },
            warhead_ap: true,
            arcing: false,
            ballistic_scatter: 256,
            homing_scatter: 512,
            min_damage: 1,
            max_damage: 1000,
        }
    }

    /// JEEP's M60mg (SA, Damage 15, Range 4, invisible + light-speed = instant).
    fn m60mg() -> WeaponProfile {
        WeaponProfile {
            damage: 15,
            rof: 20,
            range: 1024,
            proj_speed: 255,
            proj_rot: 0,
            invisible: true,
            instant: true,
            warhead: WarheadProfile {
                spread: 3,
                verses: pct5([100, 50, 60, 25, 25]),
            },
            warhead_ap: false,
            arcing: false,
            ballistic_scatter: 256,
            homing_scatter: 512,
            min_damage: 1,
            max_damage: 1000,
        }
    }

    fn spawn_tank(w: &mut World, house: u8, cell: CellCoord, hp: u16) -> Handle {
        let h = w.spawn_unit(0, house, cell, Facing(0), hp, stats());
        w.set_unit_combat(h, 3 /*heavy=steel*/, Some(ninety_mm()), true);
        h
    }

    #[test]
    fn tank_kills_adjacent_enemy_with_expected_shot_count() {
        // 2TNK (90mm, 30 dmg vs steel) vs a 600-hp heavy target one cell away.
        let mut w = world();
        let atk = spawn_tank(&mut w, 1, CellCoord::new(10, 10), 400);
        let tgt = w.spawn_unit(0, 2, CellCoord::new(11, 10), Facing(0), 600, stats());
        w.set_unit_combat(tgt, 3, None, false); // unarmed heavy (HARV-like)

        w.tick(&[Command::Attack {
            unit: atk,
            target: Target::Unit(tgt),
            house: 1,
        }]);
        // Run until the target dies or a generous timeout.
        let mut ticks = 0;
        while w.units.contains(tgt) && ticks < 2000 {
            w.tick(&[]);
            ticks += 1;
        }
        assert!(!w.units.contains(tgt), "target should have been destroyed");
        // 600 hp / 30 dmg = 20 shots. Rearm is ROF=50 ticks between shots, so
        // the kill lands on the 20th shot ~ 19*50 ticks after the first.
        // Sanity-bound the timing rather than pin it exactly.
        assert!(
            (900..1100).contains(&ticks),
            "unexpected time-to-kill: {ticks} ticks"
        );
    }

    #[test]
    fn attack_needs_ownership_and_a_weapon() {
        let mut w = world();
        let armed = spawn_tank(&mut w, 1, CellCoord::new(5, 5), 400);
        let tgt = w.spawn_unit(0, 2, CellCoord::new(6, 5), Facing(0), 100, stats());
        // Wrong house: ignored.
        w.tick(&[Command::Attack {
            unit: armed,
            target: Target::Unit(tgt),
            house: 99,
        }]);
        assert!(!w.units.get(armed).unwrap().has_target());
        // Unarmed attacker: ignored.
        let unarmed = w.spawn_unit(0, 1, CellCoord::new(5, 6), Facing(0), 400, stats());
        w.tick(&[Command::Attack {
            unit: unarmed,
            target: Target::Unit(tgt),
            house: 1,
        }]);
        assert!(!w.units.get(unarmed).unwrap().has_target());
    }

    #[test]
    fn force_fire_at_cell_consumes_sim_rng_when_scattering() {
        // A tank force-firing an AP shot at a distant ground cell scatters, which
        // draws the sync RNG (the one genuine combat RNG path). The seed must
        // therefore advance across the shot.
        let mut w = world();
        // Place attacker and force-fire target ~4.5 cells apart (in range, and
        // far enough that scatterdist > 0 so a draw actually happens).
        let atk = spawn_tank(&mut w, 1, CellCoord::new(10, 10), 400);
        let cell = CellCoord::new(14, 12); // ~4.5 cells => distance > 1024 leptons
        let seed_before = w.rng_seed();
        w.tick(&[Command::Attack {
            unit: atk,
            target: Target::Cell(cell),
            house: 1,
        }]);
        // Give the turret time to align and fire once.
        let mut fired = false;
        for _ in 0..80 {
            let seed = w.rng_seed();
            w.tick(&[]);
            if w.rng_seed() != seed {
                fired = true;
                break;
            }
        }
        assert!(fired, "force-fire never drew the sim RNG (no scatter)");
        assert_ne!(seed_before, w.rng_seed(), "sim RNG did not advance");
    }

    #[test]
    fn unit_target_shot_is_accurate_no_rng() {
        // A shot at a *vehicle* is accurate — it must NOT draw the sim RNG.
        let mut w = world();
        let atk = spawn_tank(&mut w, 1, CellCoord::new(10, 10), 400);
        let tgt = w.spawn_unit(0, 2, CellCoord::new(11, 10), Facing(0), 600, stats());
        w.set_unit_combat(tgt, 3, None, false);
        w.tick(&[Command::Attack {
            unit: atk,
            target: Target::Unit(tgt),
            house: 1,
        }]);
        let seed = w.rng_seed();
        // Step through at least one full shot (ROF 50) — target loses health.
        for _ in 0..60 {
            w.tick(&[]);
        }
        assert!(
            w.units.get(tgt).unwrap().health < 600,
            "target took no damage"
        );
        assert_eq!(
            seed,
            w.rng_seed(),
            "accurate vehicle shot must not draw RNG"
        );
    }

    #[test]
    fn instant_weapon_hits_same_tick_as_fire() {
        // M60mg is a hitscan weapon: the bullet detonates the tick it is created.
        let mut w = world();
        let jeep = w.spawn_unit(0, 1, CellCoord::new(10, 10), Facing(0), 150, stats());
        w.set_unit_combat(jeep, 2, Some(m60mg()), true);
        let tgt = w.spawn_unit(0, 2, CellCoord::new(11, 10), Facing(0), 600, stats());
        w.set_unit_combat(tgt, 3, None, false);
        w.tick(&[Command::Attack {
            unit: jeep,
            target: Target::Unit(tgt),
            house: 1,
        }]);
        let start_hp = w.units.get(tgt).unwrap().health;
        for _ in 0..40 {
            w.tick(&[]);
            // No bullet should ever linger for a hitscan weapon.
            assert!(
                w.bullets.is_empty(),
                "instant weapon left a bullet in flight"
            );
        }
        // SA vs steel = 25% of 15 => 4 dmg/shot; several shots landed.
        assert!(w.units.get(tgt).unwrap().health < start_hp);
    }

    #[test]
    fn attack_is_deterministic_hash_chain() {
        let script = |w: &mut World| -> Vec<u64> {
            let atk = spawn_tank(w, 1, CellCoord::new(8, 8), 400);
            let tgt = w.spawn_unit(0, 2, CellCoord::new(13, 10), Facing(0), 600, stats());
            w.set_unit_combat(tgt, 3, None, false);
            let mut hs = Vec::new();
            hs.push(w.tick(&[Command::Attack {
                unit: atk,
                target: Target::Cell(CellCoord::new(13, 10)),
                house: 1,
            }]));
            for _ in 0..120 {
                hs.push(w.tick(&[]));
            }
            hs
        };
        let mut a = world();
        let mut b = world();
        assert_eq!(script(&mut a), script(&mut b));
    }

    #[test]
    fn move_command_paths_and_advances() {
        let mut w = world();
        let h = w.spawn_unit(0, 1, CellCoord::new(5, 5), Facing(0), 256, stats());
        w.tick(&[Command::Move {
            unit: h,
            dest: CellCoord::new(5, 10),
            house: 1,
        }]);
        let u = w.units.get(h).unwrap();
        assert!(u.is_moving(), "unit should have a path");
        assert!(u.dest.is_some());
        // It should have advanced south (larger y) toward the goal.
        assert!(u.coord.y.0 > CellCoord::new(5, 5).center().y.0);
    }

    #[test]
    fn unit_eventually_arrives() {
        let mut w = world();
        let h = w.spawn_unit(0, 1, CellCoord::new(5, 5), Facing(0), 256, stats());
        w.tick(&[Command::Move {
            unit: h,
            dest: CellCoord::new(8, 9),
            house: 1,
        }]);
        for _ in 0..500 {
            if !w.units.get(h).unwrap().is_moving() {
                break;
            }
            w.tick(&[]);
        }
        let u = w.units.get(h).unwrap();
        assert!(!u.is_moving(), "unit never finished its path");
        assert_eq!(u.cell(), CellCoord::new(8, 9));
        assert!(u.dest.is_none());
    }

    #[test]
    fn wrong_house_command_is_ignored() {
        let mut w = world();
        let h = w.spawn_unit(0, 1, CellCoord::new(5, 5), Facing(0), 256, stats());
        w.tick(&[Command::Move {
            unit: h,
            dest: CellCoord::new(5, 10),
            house: 2, // not the owner
        }]);
        assert!(!w.units.get(h).unwrap().is_moving());
    }

    #[test]
    fn stop_clears_path() {
        let mut w = world();
        let h = w.spawn_unit(0, 1, CellCoord::new(5, 5), Facing(0), 256, stats());
        w.tick(&[Command::Move {
            unit: h,
            dest: CellCoord::new(5, 20),
            house: 1,
        }]);
        assert!(w.units.get(h).unwrap().is_moving());
        w.tick(&[Command::Stop { unit: h, house: 1 }]);
        assert!(!w.units.get(h).unwrap().is_moving());
    }

    #[test]
    fn same_seed_and_commands_give_same_hash_chain() {
        let script = |w: &mut World| -> Vec<u64> {
            let h = w.spawn_unit(0, 1, CellCoord::new(3, 3), Facing(0), 256, stats());
            let mut hashes = Vec::new();
            hashes.push(w.tick(&[Command::Move {
                unit: h,
                dest: CellCoord::new(20, 15),
                house: 1,
            }]));
            for _ in 0..60 {
                hashes.push(w.tick(&[]));
            }
            hashes
        };
        let mut a = world();
        let mut b = world();
        assert_eq!(script(&mut a), script(&mut b));
        assert_eq!(a.state_hash(), b.state_hash());
    }

    #[test]
    fn hash_changes_when_state_changes() {
        let mut w = world();
        let empty = w.state_hash();
        w.spawn_unit(0, 1, CellCoord::new(3, 3), Facing(0), 256, stats());
        assert_ne!(empty, w.state_hash());
    }
}
