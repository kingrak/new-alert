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
        arcing: false,
        height: 0,
        riser: 0,
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
        // Speed 1 is now included: the old speed==1 non-axis-aligned stall
        // (`regression_examples::speed_one_bullet_makes_progress_and_detonates`)
        // was fixed in M7.7 — `Bullet::advance` forces a 1-lepton step when the
        // truncated per-axis division underflows to zero, so a bullet always
        // makes progress and this property holds at every speed.
        speed in 1i32..=255,
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
    /// **FIXED in M7.7 (P1 arcing pass).** `Bullet::advance` now guarantees
    /// forward progress: when the truncated per-axis step underflows to zero
    /// net movement, it forces a 1-lepton step along the dominant axis, so a
    /// bullet can never stall. Un-ignored; the shot detonates at impact.
    #[test]
    fn speed_one_bullet_makes_progress_and_detonates() {
        let start = WorldCoord::new(0, 0);
        let impact = WorldCoord::new(3, 3);
        let b = make_bullet(start, impact, 1, false);
        // Detonates within a generous cap (distance ~4 at speed 1); the old
        // truncation bug never detonated (fly_to_detonation's cap would panic).
        let (ticks, pos) = fly_to_detonation(b, 20);
        assert!(ticks <= 20);
        assert_eq!(pos, impact);
    }
}

// ===========================================================================
// Arcing (ballistic lob) flight — M7.7 P1. Smoke coverage; ra-tester owns the
// full property suite.
// ===========================================================================

/// Build an arcing bullet the way `world::fire` does (`bullet.cpp:809/838`):
/// distance-scaled horizontal speed and a launch `riser` sized to the parabola.
fn make_arcing(start: WorldCoord, impact: WorldCoord, base_speed: i32) -> Bullet {
    let dx = (impact.x.0 - start.x.0) as i64;
    let dy = (impact.y.0 - start.y.0) as i64;
    let d = isqrt(dx * dx + dy * dy) as i32;
    let speed = (base_speed + d / 32).max(25);
    let riser = (((d / 2) / (speed + 1)) * ra_sim::bullet::GRAVITY).max(10);
    let mut b = make_bullet(start, impact, speed, false);
    b.arcing = true;
    b.height = 1;
    b.riser = riser;
    b
}

#[test]
fn arcing_shell_rises_then_falls_and_detonates_at_impact() {
    let start = WorldCoord::new(0, 0);
    let impact = WorldCoord::new(6 * 256, 2 * 256); // a long lob
    let mut b = make_arcing(start, impact, 12); // 155mm-ish base speed
    let mut max_h = 0;
    let mut prev_h = b.height;
    let mut rose = false;
    let mut fell = false;
    let mut detonated_at = None;
    for t in 0..200 {
        let det = b.advance();
        max_h = max_h.max(b.height);
        if b.height > prev_h {
            rose = true;
        }
        if rose && b.height < prev_h {
            fell = true;
        }
        prev_h = b.height;
        if det {
            detonated_at = Some(t);
            break;
        }
    }
    assert!(rose, "the shell should have gained height (arced up)");
    assert!(fell, "the shell should have lost height on the way down");
    assert!(
        max_h > 128,
        "a long lob should reach a visibly-arced height (~half a cell+)"
    );
    assert!(detonated_at.is_some(), "the shell must detonate");
    assert_eq!(b.pos, impact, "it detonates at the impact point");
    assert_eq!(b.height, 0, "height is zeroed at detonation (landed)");
}

// ===========================================================================
// Adversarial-depth proptests (M7.7 Chunk A, ra-tester charter): generalize
// the arcing example above into a property over the whole realistic
// coordinate/speed space, confirm non-arcing bullets are provably unaffected,
// and hammer the speed-one/stall fix far beyond the single regression example
// in `regression_examples` above.
// ===========================================================================

/// Shared tick-count bound: the fastest a bullet can possibly cover
/// `dist` leptons at `speed` leptons/tick is `ceil(dist/speed)` ticks, and
/// `2*ceil(dist/speed) + 8` is the same generous, empirically-verified-safe
/// upper bound `straight_bullet_detonates_exactly_once_within_bound` above
/// already establishes over this file's coordinate range (`-2000..=2000` per
/// corner, i.e. distances up to ~5657 leptons) and the full `speed in
/// 1..=255` range — reused here rather than re-derived, since arcing's
/// height/riser integration never touches the horizontal `pos` stepping in
/// `Bullet::advance` (it runs before, and independently of, the dx/dy/step
/// math), so straight and arcing flights share the exact same tick-count
/// envelope for the same distance/speed.
fn tick_bound(dist: i64, speed: i32) -> (i64, i64) {
    let expected_min = if dist == 0 {
        1
    } else {
        (dist + speed as i64 - 1) / speed as i64
    };
    (expected_min, expected_min * 2 + 8)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// **Generalizes `arcing_shell_rises_then_falls_and_detonates_at_impact`
    /// into a property** over arbitrary start/impact/base_speed: every
    /// arcing shot (a) traces a genuine rise-then-fall arc — except the
    /// single-tick degenerate case (the whole flight fits within one step,
    /// handled explicitly below, not skipped), (b) detonates with `pos ==
    /// impact` bit-exact (no drift), and (c) does so within the same
    /// tick-count envelope proven for straight bullets (`tick_bound` above),
    /// derived from `distance/speed`, not a magic constant. `base_speed in
    /// 8..=64` covers every real arcing weapon in `rules.ini` (`Grenade`
    /// `Speed=5` -> `scale_to_256(5)==12`, `155mm` `Speed=12` ->
    /// `scale_to_256(12)==30`) with adversarial margin on both sides.
    #[test]
    fn arcing_bullet_rises_then_falls_and_impacts_exactly(
        sx in -2000i32..=2000, sy in -2000i32..=2000,
        ix in -2000i32..=2000, iy in -2000i32..=2000,
        base_speed in 8i32..=64,
    ) {
        let start = WorldCoord::new(sx, sy);
        let impact = WorldCoord::new(ix, iy);
        let dx = (impact.x.0 - start.x.0) as i64;
        let dy = (impact.y.0 - start.y.0) as i64;
        let d = isqrt(dx * dx + dy * dy);

        let mut b = make_arcing(start, impact, base_speed);
        let (expected_min, cap) = tick_bound(d, b.speed);

        let mut max_h = 0i32;
        let mut prev_h = b.height;
        let mut rose = false;
        let mut fell = false;
        let mut detonated_at = None;
        for t in 1..=(cap + 16) {
            let det = b.advance();
            max_h = max_h.max(b.height);
            if b.height > prev_h {
                rose = true;
            }
            if rose && b.height < prev_h {
                fell = true;
            }
            prev_h = b.height;
            if det {
                detonated_at = Some(t);
                break;
            }
        }

        prop_assert!(
            detonated_at.is_some(),
            "arcing bullet never detonated within {} ticks (cap {cap}, d={d}, speed={})",
            cap + 16,
            b.speed
        );
        let ticks = detonated_at.unwrap();
        prop_assert_eq!(b.pos, impact, "detonates exactly at the impact point, no drift");
        prop_assert_eq!(b.height, 0, "height is zeroed on the detonating tick");
        prop_assert!(
            ticks >= expected_min,
            "detonated in {ticks} ticks, faster than the {expected_min}-tick lower bound \
             (d={d}, speed={})",
            b.speed
        );
        prop_assert!(
            ticks <= cap,
            "arcing flight took {ticks} ticks, exceeding the generous cap {cap} (d={d}, speed={})",
            b.speed
        );

        if ticks == 1 {
            // Degenerate single-tick case: this is NOT limited to literal
            // zero distance (originally assumed, and wrong — caught by this
            // very proptest during authoring: `d=48, base_speed=48` also
            // detonates in 1 tick, since `speed = (48 + 48/32).max(25) = 49`
            // makes the whole ~48-lepton flight fit within one step). Whenever
            // `dist2 <= step*step` is *already* true on the very first call
            // (a real reachable case: e.g. a short-range arcing lob, a
            // force-fire at the shooter's own cell, or a homing/self-target
            // edge case in `world::fire`), `advance()`'s snap-to-impact/detonate
            // branch fires on that same call — *after* the arcing block has
            // already run once and bumped `height` up by `riser`, but the very
            // same call's snap branch unconditionally resets `height` to 0
            // before returning (`bullet.rs:83-99`), regardless of how large
            // that bump was. From the outside (only observing `height` *after*
            // each `advance()` call, as this loop does) the shell is never
            // seen airborne: it launches and detonates in the same single
            // tick. This is the *correct* ported behavior, not a bug — a shot
            // that lands within one step is too close to visibly arc — so
            // `rose`/`fell` are deliberately NOT required here, only
            // exact-impact and single-tick timing.
        } else {
            prop_assert!(rose, "a non-degenerate arcing shot must gain height (d={d})");
            prop_assert!(
                fell,
                "a non-degenerate arcing shot must lose height on the way down (d={d})"
            );
            prop_assert!(
                max_h > 0,
                "a non-degenerate arcing shot must reach a height above 0 (d={d})"
            );
        }
    }

    /// **Non-arcing bullets are unaffected**: extends the existing
    /// non-arcing coverage (`straight_bullet_detonates_exactly_once_within_bound`
    /// above, which never inspects `height`) with an explicit assertion that
    /// `height` stays exactly 0 for the *entire* flight of a non-arcing
    /// bullet, over the same start/impact/speed space the arcing property
    /// above uses — proving the M7.7 arcing addition is fully opt-in and
    /// doesn't leak into the pre-existing straight-flight path.
    #[test]
    fn non_arcing_bullet_height_stays_zero_and_hits_impact(
        sx in -2000i32..=2000, sy in -2000i32..=2000,
        ix in -2000i32..=2000, iy in -2000i32..=2000,
        speed in 1i32..=255,
    ) {
        let start = WorldCoord::new(sx, sy);
        let impact = WorldCoord::new(ix, iy);
        let dx = (impact.x.0 - start.x.0) as i64;
        let dy = (impact.y.0 - start.y.0) as i64;
        let d = isqrt(dx * dx + dy * dy);
        let (_, cap) = tick_bound(d, speed);

        let mut b = make_bullet(start, impact, speed, false);
        prop_assert_eq!(b.height, 0);
        let mut ticks = 0i64;
        loop {
            ticks += 1;
            let det = b.advance();
            prop_assert_eq!(b.height, 0, "a non-arcing bullet's height must never leave 0");
            if det {
                break;
            }
            prop_assert!(
                ticks <= cap + 16,
                "non-arcing bullet still in flight after {ticks} ticks (cap {cap}, d={d}, speed={speed})"
            );
        }
        prop_assert_eq!(b.pos, impact);
    }
}

/// Adversarial start/impact deltas for the speed-one stall-fix hammer below:
/// near-45-degree diagonals, near-axis-but-not-quite angles, very short
/// (1-2 lepton) hops, and very long hauls — the angle/distance regimes most
/// likely to trigger the old truncation stall (`Bullet::advance`'s
/// `dx*step/dist`/`dy*step/dist` both flooring to 0 on the same tick).
fn adversarial_delta() -> impl Strategy<Value = (i32, i32)> {
    prop_oneof![
        // Near-45-degree: both axes almost equal magnitude.
        (1i32..=3000, -3i32..=3, any::<bool>(), any::<bool>()).prop_map(|(m, jitter, fx, fy)| {
            let dx = if fx { m } else { -m };
            let dy_mag = (m + jitter).max(1);
            let dy = if fy { dy_mag } else { -dy_mag };
            (dx, dy)
        }),
        // Near-axis-aligned but not quite: one axis dominant, the other tiny.
        (10i32..=4000, 1i32..=5, any::<bool>(), any::<bool>()).prop_map(|(big, small, fx, fy)| {
            let dx = if fx { big } else { -big };
            let dy = if fy { small } else { -small };
            (dx, dy)
        }),
        // Very short hops (1-2 leptons per axis, non-axis-aligned).
        (1i32..=2, 1i32..=2, any::<bool>(), any::<bool>()).prop_map(|(dx0, dy0, fx, fy)| {
            let dx = if fx { dx0 } else { -dx0 };
            let dy = if fy { dy0 } else { -dy0 };
            (dx, dy)
        }),
        // Very long, generic diagonal hauls.
        (1i32..=8000, 1i32..=8000, any::<bool>(), any::<bool>()).prop_map(|(dx0, dy0, fx, fy)| {
            let dx = if fx { dx0 } else { -dx0 };
            let dy = if fy { dy0 } else { -dy0 };
            (dx, dy)
        }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(768))]

    /// **Aggressive hammer on the M7.7 speed-one/stall fix.** For the FULL
    /// `speed in 1..=255` range (not narrowed to just 1, to also confirm the
    /// fix didn't regress any other speed) crossed with adversarial angles
    /// (near-45°, near-axis, very short, very long — see `adversarial_delta`),
    /// this proves the STRONGER per-tick invariant that makes a stall
    /// structurally impossible, not just empirically absent: squared
    /// distance-to-impact is monotonically, strictly decreasing every tick
    /// that doesn't detonate.
    ///
    /// This follows directly from reading `Bullet::advance` (`bullet.rs:90-115`):
    /// outside the forced single-lepton fallback, each axis's truncated step
    /// `dx*step/dist` shares `dx`'s sign and has magnitude `<= |dx|` (because
    /// `step <= dist` whenever `dist2 > step*step`, the only branch that
    /// reaches this code), so the remaining delta on each axis is
    /// non-increasing in magnitude and never changes sign — hence squared
    /// distance is non-increasing. The forced fallback (both truncated steps
    /// land on exactly 0) moves exactly 1 lepton along whichever axis still
    /// has nonzero remaining delta, which strictly shrinks that axis's
    /// contribution. So `dist2` can never fail to shrink on a tick that
    /// doesn't detonate — that is precisely what "no stall" means, verified
    /// here on every single tick, not just at the end of flight.
    ///
    /// The tick-count *cap* (not the progress invariant above, which is
    /// exact) needs the L1 bound, not the Euclidean one: at `speed == 1` and
    /// non-axis-aligned `(dx, dy)`, `|dx| < dist` and `|dy| < dist` strictly
    /// (`dist == isqrt(dx² + dy²) >= max(|dx|, |dy|)`, with equality only on
    /// an axis), so *both* natural per-axis steps `dx*1/dist`/`dy*1/dist`
    /// truncate to 0 on *every* tick — the forced fallback fires the entire
    /// flight, always moving the single larger-magnitude axis by exactly 1
    /// lepton. That converges in exactly `|dx| + |dy|` ticks (each tick
    /// retires one lepton of remaining L1 distance), which for a near-45°
    /// diagonal is up to `sqrt(2)` times the Euclidean distance `d` — e.g.
    /// `dx = dy = -59` takes `59 + 59 = 118` ticks, not `isqrt(59²·2) = 83`.
    /// Higher speeds only add larger natural per-axis steps on top, so this
    /// L1 bound safely covers the whole `speed in 1..=255` range too.
    #[test]
    fn stall_fix_holds_every_tick_across_full_speed_range(
        sx in -500i32..=500, sy in -500i32..=500,
        (dx, dy) in adversarial_delta(),
        speed in 1i32..=255,
    ) {
        let start = WorldCoord::new(sx, sy);
        let impact = WorldCoord::new(sx + dx, sy + dy);
        let mut b = make_bullet(start, impact, speed, false);

        let dist2_to_impact = |b: &Bullet| -> i64 {
            let ddx = (b.impact.x.0 - b.pos.x.0) as i64;
            let ddy = (b.impact.y.0 - b.pos.y.0) as i64;
            ddx * ddx + ddy * ddy
        };

        let d = isqrt((dx as i64) * (dx as i64) + (dy as i64) * (dy as i64));
        // L1 (Manhattan) bound, not Euclidean `d` — see the doc comment above
        // for why: at speed 1 the worst case retires exactly one lepton of
        // *L1* remaining distance per tick, which can be up to sqrt(2)x `d`
        // on a near-45° diagonal. +32 covers small-distance rounding and the
        // final detonating tick.
        let cap = (dx as i64).abs() + (dy as i64).abs() + 32;

        let mut ticks = 0i64;
        loop {
            ticks += 1;
            let before = dist2_to_impact(&b);
            let det = b.advance();
            if det {
                prop_assert_eq!(b.pos, impact, "must detonate exactly at impact, no drift");
                break;
            }
            let after = dist2_to_impact(&b);
            prop_assert!(
                after < before,
                "no forward progress at tick {ticks}: dist2 {before} -> {after} \
                 (start={start:?} impact={impact:?} speed={speed})"
            );
            prop_assert!(
                ticks <= cap,
                "bullet still in flight after {ticks} ticks (cap {cap}, d={d}, speed={speed}) \
                 — possible stall regression"
            );
        }
    }
}
