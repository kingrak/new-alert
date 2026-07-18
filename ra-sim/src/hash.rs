//! A tiny hand-rolled FNV-1a 64-bit hasher — the determinism backbone
//! (DESIGN.md §4.2: "Per-tick state hash … computed always"). Every mutable
//! field of [`crate::World`] is folded in, in a fixed order, at the end of each
//! tick. Same seed + same commands must produce the same hash chain on every
//! OS and CPU, so the hasher itself must be width-independent: it consumes
//! bytes and fixed-width little-endian integers only, never a `usize`.

/// FNV-1a 64-bit offset basis.
const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime.
const PRIME: u64 = 0x0000_0100_0000_01B3;

/// An incremental FNV-1a hasher.
#[derive(Clone, Copy, Debug)]
pub struct Fnv1a {
    state: u64,
}

impl Default for Fnv1a {
    fn default() -> Fnv1a {
        Fnv1a::new()
    }
}

impl Fnv1a {
    /// A fresh hasher seeded with the FNV offset basis.
    pub fn new() -> Fnv1a {
        Fnv1a {
            state: OFFSET_BASIS,
        }
    }

    /// Fold in one byte.
    pub fn write_u8(&mut self, b: u8) {
        self.state ^= b as u64;
        self.state = self.state.wrapping_mul(PRIME);
    }

    /// Fold in a byte slice.
    pub fn write_bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u8(b);
        }
    }

    /// Fold in a `u16` (little-endian).
    pub fn write_u16(&mut self, v: u16) {
        self.write_bytes(&v.to_le_bytes());
    }

    /// Fold in a `u32` (little-endian).
    pub fn write_u32(&mut self, v: u32) {
        self.write_bytes(&v.to_le_bytes());
    }

    /// Fold in an `i32` (little-endian two's complement).
    pub fn write_i32(&mut self, v: i32) {
        self.write_bytes(&v.to_le_bytes());
    }

    /// Fold in a `u64` (little-endian).
    pub fn write_u64(&mut self, v: u64) {
        self.write_bytes(&v.to_le_bytes());
    }

    /// The current 64-bit digest.
    pub fn finish(self) -> u64 {
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_known_fnv1a() {
        // FNV-1a of the empty input is the offset basis.
        assert_eq!(Fnv1a::new().finish(), OFFSET_BASIS);
        // FNV-1a("a") = 0xaf63dc4c8601ec8c (well-known reference vector).
        let mut h = Fnv1a::new();
        h.write_u8(b'a');
        assert_eq!(h.finish(), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn order_sensitive() {
        let mut a = Fnv1a::new();
        a.write_u32(1);
        a.write_u32(2);
        let mut b = Fnv1a::new();
        b.write_u32(2);
        b.write_u32(1);
        assert_ne!(a.finish(), b.finish());
    }
}
