//! Pure sequencing helpers (SERVER-DESIGN.md §6): hash arbitration and the
//! tick-window sanity check. These are the deterministic decision functions the
//! session sequencer (`session.rs`) calls; keeping them pure and side-effect-free
//! makes the arbitration rule testable in isolation.

use std::collections::BTreeMap;

use ra_net::SeatId;

/// The arbitrated (winning) hash for a tick, given every reporting seat's hash
/// (SERVER-DESIGN.md §6.3): the **largest equal-hash group** wins; ties are
/// broken by the group containing the **lowest seat id**, which makes the lowest
/// seat the 2-player tiebreak (degenerating to M8-C host-authoritative). Returns
/// `None` only for an empty report set.
///
/// The server holds no truth of its own — it cannot compute a world hash. It
/// arbitrates *agreement*: this is a pure function of the reported hashes.
pub fn arbitrate(reports: &[(SeatId, u64)]) -> Option<u64> {
    if reports.is_empty() {
        return None;
    }
    // hash -> (count, lowest seat reporting it). BTreeMap keeps iteration order
    // deterministic (by hash), so ties resolve identically on every run.
    let mut groups: BTreeMap<u64, (usize, SeatId)> = BTreeMap::new();
    for &(seat, hash) in reports {
        let e = groups.entry(hash).or_insert((0, SeatId::MAX));
        e.0 += 1;
        e.1 = e.1.min(seat);
    }
    let mut best: Option<(u64, usize, SeatId)> = None;
    for (&hash, &(count, min_seat)) in &groups {
        let take = match best {
            None => true,
            // Larger group wins; on a count tie the lower min-seat group wins.
            Some((_, bc, bs)) => count > bc || (count == bc && min_seat < bs),
        };
        if take {
            best = Some((hash, count, min_seat));
        }
    }
    best.map(|(h, _, _)| h)
}

/// Whether a client-stamped execution tick is within the acceptance window
/// `[current, current + max_ahead]` (SERVER-DESIGN.md §7.4). A stamp far in the
/// past is a stale redundant carry (harmless, ignored elsewhere); a stamp far in
/// the future is malformed/abusive and is rejected here.
pub fn tick_in_window(tick: u32, current: u32, max_ahead: u32) -> bool {
    tick >= current && tick <= current.saturating_add(max_ahead)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_agree_returns_that_hash() {
        assert_eq!(arbitrate(&[(1, 0xAB), (2, 0xAB), (3, 0xAB)]), Some(0xAB));
    }

    #[test]
    fn majority_wins() {
        // Two seats say 0xAA, one says 0xBB → 0xAA.
        assert_eq!(arbitrate(&[(1, 0xAA), (2, 0xBB), (3, 0xAA)]), Some(0xAA));
    }

    #[test]
    fn two_player_tie_breaks_to_lowest_seat() {
        // 1v1 disagreement: each hash has count 1; the lower seat (1) wins —
        // host-authoritative, matching M8-C.
        assert_eq!(arbitrate(&[(1, 0xAA), (2, 0xBB)]), Some(0xAA));
        assert_eq!(arbitrate(&[(2, 0xBB), (1, 0xAA)]), Some(0xAA));
    }

    #[test]
    fn three_way_all_different_lowest_seat_wins() {
        assert_eq!(arbitrate(&[(3, 0x30), (1, 0x10), (2, 0x20)]), Some(0x10));
    }

    #[test]
    fn empty_is_none() {
        assert_eq!(arbitrate(&[]), None);
    }

    #[test]
    fn window_bounds() {
        assert!(tick_in_window(10, 10, 5));
        assert!(tick_in_window(15, 10, 5));
        assert!(!tick_in_window(16, 10, 5));
        assert!(!tick_in_window(9, 10, 5));
        assert!(tick_in_window(u32::MAX, u32::MAX - 1, 5)); // saturating add
    }
}
