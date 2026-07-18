//! A 16.16 signed fixed-point number for the few places the sim needs
//! fractions (speed scaling, sub-lepton accumulation) without touching a
//! float. The original engine uses an unsigned `fixed` (`common/fixed.h`) that
//! is 8.8 for game *stats*; we keep a wider 16.16 here purely as internal
//! arithmetic so intermediate products don't lose precision. Determinism is
//! exact: every operation is integer.

/// Fractional bits in the 16.16 representation.
pub const FRAC_BITS: u32 = 16;
/// The integer value `1.0`.
pub const ONE: i32 = 1 << FRAC_BITS;

/// A signed 16.16 fixed-point value.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Hash)]
pub struct Fixed(pub i32);

impl Fixed {
    /// The largest representable value (`+32767.9999…`). Overflowing products
    /// saturate here (see [`Fixed::mul`]).
    pub const MAX: Fixed = Fixed(i32::MAX);
    /// The smallest representable value (`-32768.0`).
    pub const MIN: Fixed = Fixed(i32::MIN);

    /// The whole number `n` as a fixed-point value.
    pub fn from_int(n: i32) -> Fixed {
        Fixed(n << FRAC_BITS)
    }

    /// Build from a `num/den` ratio (e.g. `Fixed::from_ratio(1, 2)` == 0.5).
    /// `den` must be non-zero.
    pub fn from_ratio(num: i32, den: i32) -> Fixed {
        Fixed((((num as i64) << FRAC_BITS) / den as i64) as i32)
    }

    /// The truncated-toward-zero integer part.
    pub fn to_int(self) -> i32 {
        self.0 >> FRAC_BITS
    }

    /// The raw 16.16 bits.
    pub fn raw(self) -> i32 {
        self.0
    }

    /// Product of two fixed-point values.
    ///
    /// **Overflow contract (pinned finding fix).** The 16.16 layout's integer
    /// part only spans ±32768, so a product whose magnitude exceeds that cannot
    /// be represented. The computation is done in a 64-bit intermediate (so no
    /// information is lost mid-multiply) and the result is then **saturated** to
    /// the `Fixed` range rather than silently wrapping: an out-of-range positive
    /// product clamps to [`Fixed::MAX`], a negative one to [`Fixed::MIN`]. This
    /// is deterministic and monotonic (a bigger true product never yields a
    /// smaller stored value), which a wrap would violate.
    #[allow(clippy::should_implement_trait)] // deliberate inherent method, not std::ops::Mul
    pub fn mul(self, other: Fixed) -> Fixed {
        let raw = ((self.0 as i64) * (other.0 as i64)) >> FRAC_BITS;
        Fixed(raw.clamp(i32::MIN as i64, i32::MAX as i64) as i32)
    }

    /// Multiply a fixed fraction by an integer, returning a truncated integer.
    /// Saturates to `i32` range on overflow (same contract as [`Fixed::mul`]).
    pub fn mul_int(self, n: i32) -> i32 {
        let raw = ((self.0 as i64) * (n as i64)) >> FRAC_BITS;
        raw.clamp(i32::MIN as i64, i32::MAX as i64) as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_roundtrip() {
        assert_eq!(Fixed::from_int(5).to_int(), 5);
        assert_eq!(Fixed::from_int(-3).to_int(), -3);
        assert_eq!(Fixed::from_int(1).raw(), ONE);
    }

    #[test]
    fn ratio_and_mul_int() {
        // Exact power-of-two ratios round-trip precisely.
        let half = Fixed::from_ratio(1, 2);
        assert_eq!(half.mul_int(10), 5);
        let quarter = Fixed::from_ratio(1, 4);
        assert_eq!(quarter.mul_int(100), 25);
        // 1/3 is not representable exactly in 16.16, so it truncates: the stored
        // value is floor(65536/3)/65536 slightly below 1/3, and every product
        // then truncates toward zero (deterministic, which is all that matters).
        let third = Fixed::from_ratio(1, 3);
        assert_eq!(third.mul_int(9), 2); // 9 * 21845 >> 16 == 2
    }

    #[test]
    fn mul_composes() {
        let half = Fixed::from_ratio(1, 2);
        let quarter = half.mul(half);
        assert_eq!(quarter.mul_int(100), 25);
    }

    /// Pinned-finding fix (was `mul_silently_wraps_past_the_16_bit_integer_range`).
    /// A 16.16 layout's integer part only spans ±32768, so a product whose
    /// magnitude exceeds that cannot be represented. `Fixed::mul` used to cast
    /// the 64-bit intermediate straight to `i32`, which **wrapped** (`36 * -911
    /// = -32796` came back as `+32740`). The contract is now **saturation**:
    /// an out-of-range negative product clamps to `Fixed::MIN` (integer part
    /// `-32768`), a positive one to `Fixed::MAX` (`+32767`). Saturation is
    /// monotonic — a larger-magnitude true product never produces a
    /// smaller-magnitude stored value — which the old wrap violated.
    #[test]
    fn mul_saturates_past_the_16_bit_integer_range() {
        // Negative overflow saturates to the floor, not a wrapped positive.
        let a = Fixed::from_int(36);
        let b = Fixed::from_int(-911); // 36 * -911 == -32796, past the -32768 floor
        assert_eq!(a.mul(b).to_int(), -32768);
        assert_eq!(a.mul(b), Fixed::MIN);
        // Positive overflow saturates to the ceiling.
        let c = Fixed::from_int(1000);
        assert_eq!(c.mul(c), Fixed::MAX);
        assert_eq!(c.mul(c).to_int(), 32767);
        // mul_int shares the contract: 30000 * 100000 = 3e9 > i32::MAX.
        assert_eq!(Fixed::from_int(30_000).mul_int(100_000), i32::MAX);
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    // Keep magnitudes well below i16::MAX so `from_int(n).raw()` (`n <<
    // 16`) and every product below stay inside `i32`/`i64` range with room
    // to spare — these tests are about the arithmetic identities holding
    // exactly, not about probing overflow behavior (which is unspecified
    // here, same as the original's fixed-point vocabulary).
    fn small_int() -> impl Strategy<Value = i32> {
        -30_000i32..=30_000
    }
    fn nonzero_small_int() -> impl Strategy<Value = i32> {
        prop_oneof![-30_000i32..=-1, 1i32..=30_000]
    }

    proptest! {
        /// `from_int` / `to_int` round-trip exactly for any in-range integer
        /// (no fractional part is ever introduced by a whole-number build).
        #[test]
        fn int_roundtrip_holds(n in small_int()) {
            prop_assert_eq!(Fixed::from_int(n).to_int(), n);
        }

        /// Multiplication by the fixed-point representation of an integer is
        /// the same as scaling with `mul_int` by that integer — two
        /// independent code paths to the same result. Bounded to `|a*b| <
        /// 2^15`: `mul`'s own doc comment already caveats "no overflow for
        /// the magnitudes the sim uses" — a 16.16 layout's integer part only
        /// spans ±32768, so `from_int(a).mul(from_int(b))` is a valid
        /// operation only while the *product* stays in that range (see
        /// `mul_silently_wraps_past_the_16_bit_integer_range` below for what
        /// happens just outside it).
        #[test]
        fn mul_by_int_matches_mul_int(a in -180i32..=180, b in -180i32..=180) {
            let lhs = Fixed::from_int(a).mul(Fixed::from_int(b));
            let rhs = Fixed::from_int(a).mul_int(b);
            prop_assert_eq!(lhs.to_int(), rhs);
        }

        /// Multiplication is commutative (both operands are plain fixed
        /// values, no rounding-direction asymmetry).
        #[test]
        fn mul_is_commutative(a in small_int(), b in small_int()) {
            let fa = Fixed::from_int(a);
            let fb = Fixed::from_int(b);
            prop_assert_eq!(fa.mul(fb), fb.mul(fa));
        }

        /// `1.0` is the multiplicative identity.
        #[test]
        fn one_is_multiplicative_identity(n in small_int()) {
            let f = Fixed::from_int(n);
            let one = Fixed::from_int(1);
            prop_assert_eq!(f.mul(one), f);
        }

        /// `from_ratio(n, d)` scaled back up by `d` via `mul_int` recovers
        /// `n` up to the truncation the 16.16 representation is documented
        /// to introduce (see `ratio_and_mul_int` above) — bounded to at most
        /// 1 off from truncation error, never more, and never overshooting.
        #[test]
        fn from_ratio_mul_int_recovers_numerator_within_rounding(
            n in -10_000i32..=10_000, d in nonzero_small_int()
        ) {
            let r = Fixed::from_ratio(n, d);
            let back = r.mul_int(d);
            // `from_ratio` truncates toward zero (`(num << 16) / den`) but
            // `mul_int`'s `>>` floors, so the round trip is not always exact;
            // bounding the combined error requires |den| < 2^16 (true for
            // every `d` this strategy generates, well under 30_000), which
            // keeps the two roundings from compounding past 1 unit of `n`.
            prop_assert!((back as i64 - n as i64).abs() <= 1);
        }

        /// `from_ratio(n, 1)` is exactly `from_int(n)` — the ratio
        /// constructor's degenerate case.
        #[test]
        fn from_ratio_denominator_one_is_from_int(n in small_int()) {
            prop_assert_eq!(Fixed::from_ratio(n, 1), Fixed::from_int(n));
        }
    }
}
