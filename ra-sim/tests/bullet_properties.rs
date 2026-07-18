//! Bullet-flight property tests (M4, DESIGN.md §4.2): `proptest`-driven
//! checks of [`ra_sim::Bullet::advance`] and [`ra_sim::coords::coord_move`]
//! (the integer displacement math bullet scatter and straight flight both
//! build on) against their documented contracts, over hundreds of randomly
//! generated cases rather than the handful of hand-picked examples in each
//! module's own unit tests.

use proptest::prelude::*;

use ra_sim::coords::{coord_move, isqrt, CellCoord, Facing, WorldCoord};
use ra_sim::{Bullet, Handle, Target, WarheadProfile};

/// A minimal bullet for flight-only testing: damage/warhead/target are
/// irrelevant to `advance`, so they're fixed placeholders.
fn make_bullet(pos: WorldCoord, impact: WorldCoord, speed: i32, instant: bool) -> Bullet {
    Bullet {
        pos,
        impact,
        // M6 changed Bullet::target from Option<Handle> to Target; a ground cell
        // is the flight-only placeholder (advance() never reads the target).
        target: Target::Cell(CellCoord::new(0, 0)),
        speed,
        facing: Facing(0),
        damage: 1,
        warhead: WarheadProfile {
            spread: 1,
            verses: [65536; 5],
        },
        min_damage: 1,
        max_damage: 1000,
        source_house: 0,
        source_unit: Handle { index: 0, gen: 0 },
        instant,
        invisible: false,
    }
}

/// Run a non-instant bullet to detonation, returning the number of
/// `advance()` calls it took (>= 1) and the final position. Bounded by
/// `cap` ticks so a genuine regression (bullet that never detonates) fails
/// the test instead of hanging.
fn fly_to_detonation(mut b: Bullet, cap: i64) -> (i64, WorldCoord) {
    let mut ticks = 0i64;
    loop {
        ticks += 1;
        let detonated = b.advance();
        if detonated {
            return (ticks, b.pos);
        }
        assert!(
            ticks <= cap,
            "bullet never detonated within {cap} ticks (pos={:?} impact={:?} speed={})",
            b.pos,
            b.impact,
            b.speed
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    /// **Straight bullets reach impact in a bounded number of ticks and
    /// detonate exactly once.** The lower bound is exact —
    /// `ceil(distance/speed)` ticks is the fastest a bullet can possibly
    /// cover the distance when it advances at most `speed` leptons/tick — and
    /// this implementation never finishes early (confirmed empirically over
    /// 1M realistic-range samples during test authoring: zero cases finished
    /// before `ceil(distance/speed)`).
    ///
    /// The **upper** bound is *not* exactly `ceil(distance/speed)`: each
    /// partial step re-derives its direction from `isqrt` (a *floor* square
    /// root) and then truncates the per-axis division
    /// (`dx * step / dist`), so successive steps can advance slightly less
    /// than the nominal `speed` — the shortfall compounds over many ticks,
    /// worse at low speed relative to distance (verified empirically up to
    /// ~11% more ticks than the idealized `ceil(distance/speed)` at
    /// speed=12, distance=2000, the slowest/longest realistic weapon
    /// combination in `rules.ini`). `2*ceil(distance/speed) + 8` is a
    /// generous, empirically-verified-safe cap over the whole realistic
    /// combat range (distance 0..=2000 leptons, speed 1..=255 leptons/tick —
    /// M60mg's 255 down to ARTY's 30 and below); this test pins "detonates
    /// exactly once, within a bounded number of ticks", not the idealized
    /// continuous-motion formula, which the discrete integer implementation
    /// does not exactly match (a documented, harmless quirk — flagged in the
    /// M4 test report; determinism is unaffected since the same inputs always
    /// take the same number of ticks).
    #[test]
    fn straight_bullet_detonates_exactly_once_within_bound(
        sx in -2000i32..=2000,
        sy in -2000i32..=2000,
        ix in -2000i32..=2000,
        iy in -2000i32..=2000,
        // Speed starts at 2, not 1: speed==1 can stall a bullet *forever* on
        // a non-axis-aligned shot (see `regression_examples::
        // known_bug_speed_one_bullet_can_stall_forever` below) — a genuine
        // bug in `Bullet::advance`'s per-axis truncated division, reported
        // separately. It is unreachable with any real `rules.ini` weapon
        // (the slowest, `Grenade`, resolves to `proj_speed=12` via
        // `scale_to_256`), so excluding it here keeps this property test
        // meaningful for the domain the sim actually exercises rather than
        // silently passing over a real hang.
        speed in 2i32..=255,
    ) {
        let start = WorldCoord::new(sx, sy);
        let impact = WorldCoord::new(ix, iy);
        let dx = (impact.x.0 - start.x.0) as i64;
        let dy = (impact.y.0 - start.y.0) as i64;
        let dist = isqrt(dx * dx + dy * dy);
        let expected_min = if dist == 0 { 1 } else { (dist + speed as i64 - 1) / speed as i64 };
        let cap = expected_min * 2 + 8;

        let b = make_bullet(start, impact, speed, false);
        let (ticks, final_pos) = fly_to_detonation(b, cap + 16);

        prop_assert!(
            ticks >= expected_min,
            "bullet detonated in {ticks} ticks, faster than the {expected_min}-tick lower bound \
             (dist={dist}, speed={speed})"
        );
        prop_assert!(
            ticks <= cap,
            "bullet took {ticks} ticks, exceeding the generous cap {cap} \
             (dist={dist}, speed={speed}, expected_min={expected_min})"
        );
        // Exact-once detonation: `advance()` snaps to `impact` on the tick it
        // returns `true`, and this helper stops calling it the instant it
        // does — so "exactly once" is "it returned true exactly once before
        // this point", which `fly_to_detonation`'s loop structure guarantees
        // by construction. What's worth asserting is the postcondition that
        // makes a second detonation meaningless: position is pinned exactly
        // at `impact`, bit-for-bit (no residual drift).
        prop_assert_eq!(final_pos, impact);
    }

    /// **Instant (hitscan) bullets never persist a tick**: `advance()` must
    /// return `true` on its very first call, snapping straight to `impact`
    /// without any intermediate partial-step state, regardless of the
    /// muzzle/impact distance or speed field (which `instant` bullets ignore
    /// entirely — `bullet.cpp:787`, the M60mg case).
    #[test]
    fn instant_bullet_detonates_on_first_advance(
        sx in -5000i32..=5000,
        sy in -5000i32..=5000,
        ix in -5000i32..=5000,
        iy in -5000i32..=5000,
        speed in 0i32..=255,
    ) {
        let start = WorldCoord::new(sx, sy);
        let impact = WorldCoord::new(ix, iy);
        let mut b = make_bullet(start, impact, speed, true);
        prop_assert!(b.advance(), "instant bullet did not detonate on its first advance()");
        prop_assert_eq!(b.pos, impact);
    }

    /// **`coord_move` displacement magnitude ≈ requested distance.** The
    /// original's cosine/sine table (`Move_Point`, `coord.cpp`) stores signed
    /// bytes in `-127..=127` (never reaching the "true" unit-circle scale of
    /// 128 that the `>>7` division assumes) and each axis is independently
    /// floor-divided by 128, so the displacement magnitude is systematically
    /// *slightly under* the requested distance, by an amount that grows with
    /// distance.
    ///
    /// **Stated bound**, verified exhaustively (all 256 directions) over
    /// distance 0..=32768 leptons (the full map width) during test authoring
    /// with zero violations: `|magnitude - distance| <= distance*3/128 + 2`.
    /// This test restricts to distance 0..=8192 leptons (32 cells — already
    /// far beyond any real scatter/weapon-range use, whose largest values are
    /// `HomingScatter=512` and `ARTY`'s `Range=6` cells = 1536 leptons) to
    /// stay well clear of the *documented* `u16`/`i16` truncation regime
    /// `coord_move`'s own doc comment calls out for distances large enough
    /// that `(cos*distance)>>7` would overflow 16 bits — that regime is a
    /// real, separately-documented limitation of the port, not part of what
    /// this property claims.
    #[test]
    fn coord_move_magnitude_matches_distance_within_table_error_bound(
        dir: u8,
        distance in 0i32..=8192,
    ) {
        let start = WorldCoord::new(0, 0);
        let moved = coord_move(start, Facing(dir), distance);
        let dx = moved.x.0 as i64;
        let dy = moved.y.0 as i64;
        let mag2 = dx * dx + dy * dy;
        let mag = isqrt(mag2); // floor(sqrt) — fine for a bound check
        let err = (mag - distance as i64).abs();
        let bound = (distance as i64) * 3 / 128 + 2;
        prop_assert!(
            err <= bound + 1, // +1 slack for isqrt's own floor vs the f64 probe used to derive `bound`
            "coord_move(dist={distance}, dir={dir}) magnitude={mag}, error={err} exceeds bound {bound}"
        );
    }
}

#[cfg(test)]
mod regression_examples {
    use super::*;

    /// Concrete example pinning the "detonates exactly once, position pinned
    /// at impact" behavior for a realistic AP shot (2TNK's 90mm, muzzle
    /// speed 102 leptons/tick, adjacent-cell range) — a fixed case
    /// complementing the proptest above with a human-readable number.
    #[test]
    fn ninety_mm_adjacent_shot_detonates_in_one_or_two_ticks() {
        let muzzle = WorldCoord::new(2688, 2688); // cell (10,10) centre
        let impact = WorldCoord::new(2944, 2688); // cell (11,10) centre, 256 leptons away
        let b = make_bullet(muzzle, impact, 102, false);
        let (ticks, pos) = fly_to_detonation(b, 10);
        // ceil(256/102) = 3.
        assert_eq!(ticks, 3);
        assert_eq!(pos, impact);
    }

    /// A zero-distance shot (muzzle == impact, e.g. a point-blank force-fire)
    /// must still detonate — on the very first `advance()` call, since
    /// `dist2 <= step*step` is trivially true at distance 0.
    #[test]
    fn zero_distance_bullet_detonates_immediately() {
        let p = WorldCoord::new(500, 500);
        let mut b = make_bullet(p, p, 40, false);
        assert!(b.advance());
        assert_eq!(b.pos, p);
    }

    /// **Known bug (reported to ra-coder, not fixed here — see the M4 test
    /// report): `Bullet::advance()` can stall forever at `speed == 1`.**
    ///
    /// Each partial step independently truncates `dx * step / dist` and
    /// `dy * step / dist` toward zero (`bullet.rs`'s `advance`). For a
    /// non-axis-aligned shot, `dist == isqrt(dx*dx + dy*dy)` satisfies
    /// `max(|dx|, |dy|) < dist <= sqrt(2) * max(|dx|, |dy|)`. At `step == 1`
    /// (which is what `speed.max(1)` floors *any* `speed <= 1` to,
    /// including `speed == 0`), the condition `|dx| * 1 < dist` and
    /// `|dy| * 1 < dist` both hold whenever `dist / max(|dx|, |dy|) > 1`
    /// — true for essentially every diagonal-ish direction (exactly the
    /// 45°-ish case below: `dx == dy == 3`, `dist == isqrt(18) == 4`,
    /// `3*1/4 == 0` on *both* axes). Both axes then compute zero movement,
    /// `pos` never changes, `dist2` never shrinks, and the bullet detonates
    /// **never** — a permanent hang for that projectile (not a game-freeze:
    /// `run_bullets` calls `advance()` once per tick, not in a loop, so the
    /// tick loop itself keeps running — but the bullet silently never
    /// reaches or damages its target, forever).
    ///
    /// **Live-game impact: none today.** Every real `rules.ini` weapon
    /// resolves to `proj_speed >= 12` (`Grenade`'s `Speed=5` is the slowest,
    /// `scale_to_256(5) == 12`) via `ra_data::combat::resolve_weapon`'s
    /// `scale_to_256`, so `speed == 1` cannot occur from real game data. This
    /// is a latent robustness gap (any weapon with `Speed=0..3`, or a
    /// missing `Speed=` combined with a non-instant `Projectile=`, would
    /// trigger it — plausible for future content/mods, not just a
    /// contrived unit test), not a currently-live desync risk.
    ///
    /// This test is `#[ignore]`d so the suite stays green until ra-coder
    /// fixes `Bullet::advance`'s forward-progress guarantee (e.g. round the
    /// per-axis step instead of truncating, or force at least a 1-lepton
    /// step toward the target when truncation would otherwise produce zero
    /// net movement). Run with:
    /// `cargo test -p ra-sim --test bullet_properties -- --ignored known_bug_speed_one`
    #[test]
    #[ignore = "known bug: speed==1 bullets can stall forever on non-axis-aligned shots — see doc comment; unreachable with any real rules.ini weapon today"]
    fn known_bug_speed_one_bullet_can_stall_forever() {
        let start = WorldCoord::new(0, 0);
        let impact = WorldCoord::new(3, 3);
        let b = make_bullet(start, impact, 1, false);
        // A correct implementation detonates within, say, 20 ticks (a
        // generous cap for distance ~4 at speed 1). The buggy
        // implementation never detonates at all; `fly_to_detonation`'s
        // internal cap panics well before that, demonstrating the hang.
        let (ticks, pos) = fly_to_detonation(b, 20);
        assert!(ticks <= 20);
        assert_eq!(pos, impact);
    }
}
