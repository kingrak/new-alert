//! `World` — the single explicit game-state value (DESIGN.md §3.2, §4.2) and
//! the command pipeline that is the *only* way to mutate it (§4.4).
//!
//! Each tick runs a fixed, explicit sequence of systems — **commands, then
//! movement** — over arenas iterated in slot order, with one seeded RNG owned
//! here. At the end of a tick the whole mutable state is folded into a 64-bit
//! FNV-1a hash (§4.2): the hash chain is the determinism backbone, asserted in
//! replays and multiplayer alike.

use crate::arena::{Arena, Handle};
use crate::coords::{isqrt, CellCoord, Facing, WorldCoord};
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
    /// does not own the unit, the unit is stale, or no path exists.
    Move {
        /// The unit to move.
        unit: Handle,
        /// Destination cell.
        dest: CellCoord,
        /// House issuing the order (must own `unit`).
        house: u8,
    },
    /// Order `unit` to halt where it is.
    Stop {
        /// The unit to stop.
        unit: Handle,
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
        h.finish()
    }
}

/// Apply one tick's worth of systems to `world`, in the canonical fixed order:
/// **(1) commands, (2) movement**. This is the single mutation entry point for
/// the sim (§4.4). `tick` must equal the world's current tick.
pub fn apply(world: &mut World, tick: u32, commands: &[Command]) {
    debug_assert_eq!(
        tick, world.tick_count,
        "commands applied to the wrong tick (replay/order bug)"
    );

    // System 1: commands. Applied in the given (canonical) order.
    for &cmd in commands {
        apply_command(world, cmd);
    }

    // System 2: movement.
    move_units(world);

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
                }
            }
        }
        Command::Stop { unit, house } => {
            if let Some(u) = world.units.get_mut(unit) {
                if u.house == house {
                    u.path.clear();
                    u.dest = None;
                }
            }
        }
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
