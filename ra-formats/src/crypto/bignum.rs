//! Minimal fixed-size unsigned big integer, just enough for the Westwood
//! public-key decrypt (modular exponentiation of ~320-bit numbers with a fixed
//! public exponent of 65537).
//!
//! Intentionally simple and dependency-free: modular multiplication is done by
//! binary double-and-add so there is no big-integer division to get wrong. The
//! numbers are tiny (≤ 320 bits) and this runs only a handful of times when a
//! MIX header is opened, so the O(bits) inner loop is irrelevant to performance.
//!
//! Limbs are little-endian (`limbs[0]` is least significant). 512 bits of
//! capacity comfortably holds the 319-bit modulus and the `2·a` intermediates.

const LIMBS: usize = 16; // 16 × 32 bits = 512 bits capacity.

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Big {
    limbs: [u32; LIMBS],
}

impl Big {
    pub(crate) const ZERO: Big = Big { limbs: [0; LIMBS] };

    fn one() -> Big {
        let mut b = Big::ZERO;
        b.limbs[0] = 1;
        b
    }

    /// Build from big-endian bytes (most significant byte first).
    pub(crate) fn from_be_bytes(bytes: &[u8]) -> Big {
        let mut b = Big::ZERO;
        // Walk bytes from least to most significant.
        for (i, &byte) in bytes.iter().rev().enumerate() {
            let limb = i / 4;
            let shift = (i % 4) * 8;
            if limb < LIMBS {
                b.limbs[limb] |= (byte as u32) << shift;
            }
        }
        b
    }

    /// Build from little-endian bytes (least significant byte first).
    pub(crate) fn from_le_bytes(bytes: &[u8]) -> Big {
        let mut b = Big::ZERO;
        for (i, &byte) in bytes.iter().enumerate() {
            let limb = i / 4;
            let shift = (i % 4) * 8;
            if limb < LIMBS {
                b.limbs[limb] |= (byte as u32) << shift;
            }
        }
        b
    }

    /// Write the low `n` bytes as little-endian.
    pub(crate) fn to_le_bytes(self, n: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let limb = i / 4;
            let shift = (i % 4) * 8;
            out.push((self.limbs[limb] >> shift) as u8);
        }
        out
    }

    fn bit(self, i: usize) -> bool {
        (self.limbs[i / 32] >> (i % 32)) & 1 == 1
    }

    /// `self >= other` (unsigned).
    fn ge(self, other: &Big) -> bool {
        for i in (0..LIMBS).rev() {
            if self.limbs[i] != other.limbs[i] {
                return self.limbs[i] > other.limbs[i];
            }
        }
        true
    }

    /// Wrapping add (any carry out of the top limb is dropped; callers keep
    /// operands small enough that this never happens).
    fn add(self, other: &Big) -> Big {
        let mut out = Big::ZERO;
        let mut carry: u64 = 0;
        for i in 0..LIMBS {
            let sum = self.limbs[i] as u64 + other.limbs[i] as u64 + carry;
            out.limbs[i] = sum as u32;
            carry = sum >> 32;
        }
        out
    }

    /// Wrapping subtract (assumes `self >= other`).
    fn sub(self, other: &Big) -> Big {
        let mut out = Big::ZERO;
        let mut borrow: i64 = 0;
        for i in 0..LIMBS {
            let diff = self.limbs[i] as i64 - other.limbs[i] as i64 - borrow;
            if diff < 0 {
                out.limbs[i] = (diff + (1i64 << 32)) as u32;
                borrow = 1;
            } else {
                out.limbs[i] = diff as u32;
                borrow = 0;
            }
        }
        out
    }

    fn shl1(self) -> Big {
        let mut out = Big::ZERO;
        let mut carry: u32 = 0;
        for i in 0..LIMBS {
            out.limbs[i] = (self.limbs[i] << 1) | carry;
            carry = self.limbs[i] >> 31;
        }
        out
    }

    /// `(self + other) mod m`, assuming both operands are already `< m`.
    fn add_mod(self, other: &Big, m: &Big) -> Big {
        let s = self.add(other);
        if s.ge(m) {
            s.sub(m)
        } else {
            s
        }
    }

    /// `(self * 2) mod m`, assuming `self < m`.
    fn dbl_mod(self, m: &Big) -> Big {
        let d = self.shl1();
        if d.ge(m) {
            d.sub(m)
        } else {
            d
        }
    }

    /// `self mod m`, for arbitrary `self` (Horner over the bits).
    fn rem(self, m: &Big) -> Big {
        let one = Big::one();
        let mut r = Big::ZERO;
        for i in (0..LIMBS * 32).rev() {
            r = r.dbl_mod(m);
            if self.bit(i) {
                r = r.add_mod(&one, m);
            }
        }
        r
    }

    /// `(self * other) mod m`, assuming both operands are already `< m`.
    fn mul_mod(self, other: &Big, m: &Big) -> Big {
        let mut result = Big::ZERO;
        let mut a = self;
        let mut b = *other;
        while b != Big::ZERO {
            if b.limbs[0] & 1 == 1 {
                result = result.add_mod(&a, m);
            }
            a = a.dbl_mod(m);
            b = b.shr1();
        }
        result
    }

    fn shr1(self) -> Big {
        let mut out = Big::ZERO;
        let mut carry: u32 = 0;
        for i in (0..LIMBS).rev() {
            out.limbs[i] = (self.limbs[i] >> 1) | carry;
            carry = self.limbs[i] << 31;
        }
        out
    }

    /// `self^exp mod m`.
    pub(crate) fn pow_mod(self, mut exp: u32, m: &Big) -> Big {
        let mut result = Big::one();
        let mut base = self.rem(m);
        while exp != 0 {
            if exp & 1 == 1 {
                result = result.mul_mod(&base, m);
            }
            base = base.mul_mod(&base, m);
            exp >>= 1;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_modexp() {
        // 4^13 mod 497 == 445 (classic RSA textbook example).
        let base = Big::from_le_bytes(&[4]);
        let m = Big::from_le_bytes(&[497u16 as u8, (497u16 >> 8) as u8]);
        let r = base.pow_mod(13, &m);
        assert_eq!(r.to_le_bytes(2), vec![445u16 as u8, (445u16 >> 8) as u8]);
    }

    #[test]
    fn rem_reduces() {
        let x = Big::from_le_bytes(&[10]);
        let m = Big::from_le_bytes(&[7]);
        assert_eq!(x.rem(&m).to_le_bytes(1), vec![3]);
    }
}
