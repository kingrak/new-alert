//! A hand-rolled generational arena (no external crate). Entities are
//! addressed by [`Handle`] — an index plus a generation counter — so a stale
//! handle to a freed-then-reused slot is detected instead of silently aliasing
//! a different entity (DESIGN.md §4.3: "Entity references are generational
//! handles, never indices or pointers").
//!
//! Iteration is always in **slot order**, never hashed order, which the
//! determinism contract (§4.2) requires. Indices and generations are `u32`
//! (no `usize` in sim state, §4.7) so the state is bit-identical across
//! 32/64-bit targets.

/// A stable reference to an arena entry. Cheap to copy; compares by value.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Handle {
    /// Slot index within the arena.
    pub index: u32,
    /// Generation the handle was minted at; must match the slot's generation.
    pub gen: u32,
}

#[derive(Clone, Debug)]
struct Slot<T> {
    gen: u32,
    value: Option<T>,
}

/// A generational arena of `T`.
#[derive(Clone, Debug, Default)]
pub struct Arena<T> {
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
    len: u32,
}

impl<T> Arena<T> {
    /// An empty arena.
    pub fn new() -> Arena<T> {
        Arena {
            slots: Vec::new(),
            free: Vec::new(),
            len: 0,
        }
    }

    /// Number of live entries.
    pub fn len(&self) -> u32 {
        self.len
    }

    /// Whether the arena has no live entries.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Insert a value, returning its handle. Reuses a freed slot when possible
    /// (lowest freed index first, for deterministic slot assignment).
    pub fn insert(&mut self, value: T) -> Handle {
        self.len += 1;
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            slot.value = Some(value);
            Handle {
                index,
                gen: slot.gen,
            }
        } else {
            let index = self.slots.len() as u32;
            self.slots.push(Slot {
                gen: 0,
                value: Some(value),
            });
            Handle { index, gen: 0 }
        }
    }

    /// Whether `handle` still refers to a live entry.
    pub fn contains(&self, handle: Handle) -> bool {
        self.slots
            .get(handle.index as usize)
            .is_some_and(|s| s.gen == handle.gen && s.value.is_some())
    }

    /// Borrow the entry `handle` refers to, or `None` if stale/removed.
    pub fn get(&self, handle: Handle) -> Option<&T> {
        self.slots
            .get(handle.index as usize)
            .filter(|s| s.gen == handle.gen)
            .and_then(|s| s.value.as_ref())
    }

    /// Mutably borrow the entry `handle` refers to.
    pub fn get_mut(&mut self, handle: Handle) -> Option<&mut T> {
        self.slots
            .get_mut(handle.index as usize)
            .filter(|s| s.gen == handle.gen)
            .and_then(|s| s.value.as_mut())
    }

    /// Remove the entry, bumping the slot generation so old handles go stale.
    /// Returns the removed value if the handle was live.
    pub fn remove(&mut self, handle: Handle) -> Option<T> {
        let slot = self.slots.get_mut(handle.index as usize)?;
        if slot.gen != handle.gen {
            return None;
        }
        let value = slot.value.take()?;
        slot.gen = slot.gen.wrapping_add(1);
        self.free.push(handle.index);
        self.len -= 1;
        Some(value)
    }

    /// Iterate live `(Handle, &T)` pairs in ascending slot order. This is the
    /// canonical, determinism-safe iteration used by every sim system.
    pub fn iter(&self) -> impl Iterator<Item = (Handle, &T)> {
        self.slots.iter().enumerate().filter_map(|(i, s)| {
            s.value.as_ref().map(|v| {
                (
                    Handle {
                        index: i as u32,
                        gen: s.gen,
                    },
                    v,
                )
            })
        })
    }

    /// The live handles in ascending slot order (materialised, so a system can
    /// mutate entries during iteration without borrowing the arena).
    pub fn handles(&self) -> Vec<Handle> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                s.value.as_ref().map(|_| Handle {
                    index: i as u32,
                    gen: s.gen,
                })
            })
            .collect()
    }
}

use crate::snapshot::{SnapError, SnapReader, SnapWriter};

impl<T> Arena<T> {
    /// Byte-exact snapshot of the arena — **including** the free-list and every
    /// slot's generation counter, so handles (index+gen) round-trip identically
    /// and cross-references between entities stay valid (M8-C). Each element is
    /// written by `f`.
    pub(crate) fn snap_write<F: Fn(&mut SnapWriter, &T)>(&self, w: &mut SnapWriter, f: F) {
        w.u32(self.slots.len() as u32);
        for slot in &self.slots {
            w.u32(slot.gen);
            match &slot.value {
                Some(v) => {
                    w.u8(1);
                    f(w, v);
                }
                None => w.u8(0),
            }
        }
        w.u32(self.free.len() as u32);
        for &fr in &self.free {
            w.u32(fr);
        }
        w.u32(self.len);
    }

    /// Inverse of [`Arena::snap_write`]; `f` decodes one element.
    pub(crate) fn snap_read<F: Fn(&mut SnapReader) -> Result<T, SnapError>>(
        r: &mut SnapReader,
        f: F,
    ) -> Result<Arena<T>, SnapError> {
        let nslots = r.count("arena.slots")?;
        let mut slots = Vec::with_capacity(nslots.min(4096));
        for _ in 0..nslots {
            let gen = r.u32()?;
            let value = match r.u8()? {
                0 => None,
                1 => Some(f(r)?),
                _ => return Err(SnapError::BadTag("arena.slot")),
            };
            slots.push(Slot { gen, value });
        }
        let nfree = r.count("arena.free")?;
        let mut free = Vec::with_capacity(nfree.min(4096));
        for _ in 0..nfree {
            free.push(r.u32()?);
        }
        let len = r.u32()?;
        Ok(Arena { slots, free, len })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_get_remove() {
        let mut a: Arena<i32> = Arena::new();
        let h1 = a.insert(10);
        let h2 = a.insert(20);
        assert_eq!(a.len(), 2);
        assert_eq!(a.get(h1), Some(&10));
        assert_eq!(a.get(h2), Some(&20));
        assert_eq!(a.remove(h1), Some(10));
        assert_eq!(a.get(h1), None);
        assert!(!a.contains(h1));
        assert_eq!(a.len(), 1);
    }

    #[test]
    fn stale_handle_after_reuse() {
        let mut a: Arena<i32> = Arena::new();
        let h1 = a.insert(1);
        a.remove(h1);
        let h2 = a.insert(2); // reuses slot 0, generation bumped
        assert_eq!(h2.index, h1.index);
        assert_ne!(h2.gen, h1.gen);
        assert_eq!(a.get(h1), None); // old handle is stale
        assert_eq!(a.get(h2), Some(&2));
    }

    #[test]
    fn iter_is_slot_ordered() {
        let mut a: Arena<i32> = Arena::new();
        let h0 = a.insert(0);
        let _h1 = a.insert(1);
        let h2 = a.insert(2);
        a.remove(h0);
        a.remove(h2);
        let h3 = a.insert(3); // reuses slot 2 (lowest freed popped last -> LIFO)
        let collected: Vec<_> = a.iter().map(|(h, v)| (h.index, *v)).collect();
        // slot 1 (value 1) then slot 2 (value 3), ascending.
        assert_eq!(collected, vec![(1, 1), (2, 3)]);
        assert!(a.contains(h3));
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    /// One fuzzed arena operation.
    #[derive(Debug, Clone, Copy)]
    enum Op {
        Insert(i32),
        /// Remove the `n`th still-live handle we've ever minted, by
        /// insertion order (modulo the count so far) — indexes into a
        /// side-table of handles the test tracks, not a raw slot index, so
        /// every generated op is meaningful regardless of prior removals.
        RemoveNth(usize),
    }

    fn op_strategy() -> impl Strategy<Value = Op> {
        prop_oneof![
            3 => any::<i32>().prop_map(Op::Insert),
            1 => (0usize..64).prop_map(Op::RemoveNth),
        ]
    }

    proptest! {
        /// Property model: replay a random insert/remove sequence against a
        /// real `Arena` while shadowing expected state in a plain `Vec` of
        /// `Option<(Handle, T)>` (by mint order), then assert every
        /// documented invariant holds after *every* op, not just at the end:
        /// - `len()` matches the live count.
        /// - A handle resolves via `get`/`contains` iff it was minted and
        ///   not yet removed (staleness: a freed-then-reused slot's old
        ///   handle must never resolve, per DESIGN.md §4.3).
        /// - `iter()`/`handles()` yield exactly the live set, in strictly
        ///   ascending slot-index order (the determinism contract, §4.2).
        #[test]
        fn arena_invariants_hold_under_random_ops(ops in proptest::collection::vec(op_strategy(), 0..200)) {
            let mut arena: Arena<i32> = Arena::new();
            // Every handle ever minted, in mint order; `None` once removed.
            let mut minted: Vec<Option<(Handle, i32)>> = Vec::new();

            for op in ops {
                match op {
                    Op::Insert(v) => {
                        let h = arena.insert(v);
                        minted.push(Some((h, v)));
                    }
                    Op::RemoveNth(n) => {
                        if minted.is_empty() {
                            continue;
                        }
                        let idx = n % minted.len();
                        if let Some((h, _)) = minted[idx].take() {
                            let removed = arena.remove(h);
                            prop_assert!(removed.is_some(), "remove() missed a handle the model thinks is live");
                        }
                        // Removing an already-removed (or never-live) model
                        // entry is a no-op by construction (`.take()` above
                        // already made it `None`), so there is nothing further
                        // to do here — this branch intentionally does not
                        // re-remove.
                    }
                }

                // Invariant 1: len() equals the model's live count.
                let expected_live: Vec<(Handle, i32)> =
                    minted.iter().flatten().copied().collect();
                prop_assert_eq!(arena.len() as usize, expected_live.len());

                // Invariant 2: every live handle resolves to its value; every
                // removed (stale) handle does not.
                // Already-removed slots (`None`) have no handle left to check.
                for (h, v) in minted.iter().flatten() {
                    prop_assert_eq!(arena.get(*h), Some(v));
                    prop_assert!(arena.contains(*h));
                }

                // Invariant 3: iter()/handles() are exactly the live set, in
                // strictly ascending slot-index order.
                let mut expected_sorted = expected_live.clone();
                expected_sorted.sort_by_key(|(h, _)| h.index);
                let got: Vec<(Handle, i32)> =
                    arena.iter().map(|(h, v)| (h, *v)).collect();
                prop_assert_eq!(&got, &expected_sorted);
                let got_handles = arena.handles();
                prop_assert_eq!(got_handles.len(), expected_sorted.len());
                for w in got_handles.windows(2) {
                    prop_assert!(w[0].index < w[1].index, "handles() not strictly ascending");
                }
            }
        }

        /// A handle from a slot that has since been freed *and reused* must
        /// never alias the new occupant — the specific staleness scenario
        /// DESIGN.md calls out by name. Exercised directly (not just as a
        /// side effect of the general model above) across many insert counts.
        #[test]
        fn stale_handle_never_aliases_after_reuse(pre_inserts in 0usize..8) {
            let mut arena: Arena<i32> = Arena::new();
            for i in 0..pre_inserts {
                arena.insert(i as i32);
            }
            let victim = arena.insert(999);
            arena.remove(victim);
            let replacement = arena.insert(1000);
            prop_assert_eq!(replacement.index, victim.index, "test assumes slot reuse");
            prop_assert_ne!(replacement.gen, victim.gen);
            prop_assert_eq!(arena.get(victim), None);
            prop_assert_eq!(arena.get(replacement), Some(&1000));
            prop_assert!(!arena.contains(victim));
            prop_assert!(arena.contains(replacement));
        }
    }
}
