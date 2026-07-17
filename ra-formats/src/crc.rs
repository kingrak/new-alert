//! The Westwood filename → entry-id hash.
//!
//! MIX archives index their entries by a 32-bit hash of the (upper-cased)
//! filename rather than by name. This is *not* a true CRC — Westwood called it
//! one but it is a custom rolling hash (`Calculate_CRC` /
//! `common/crc.cpp` `CRCEngine` in the reference source). Both Tiberian Dawn and
//! Red Alert use this same "classic" variant.
//!
//! Algorithm (per `common/crc.cpp`):
//! - Upper-case the name (ASCII).
//! - Process the bytes in little-endian 4-byte groups; the final partial group
//!   is zero-padded in its high bytes.
//! - For each group: `crc = rotate_left(crc, 1) + group`.

/// Compute the Westwood MIX entry id for a filename.
///
/// The name is upper-cased as ASCII (matching the original's `strupr`); any
/// directory component should already be stripped by the caller.
pub fn id_of(name: &str) -> u32 {
    let upper: Vec<u8> = name.bytes().map(|b| b.to_ascii_uppercase()).collect();
    id_of_bytes(&upper)
}

/// Compute the hash over raw bytes that are assumed to already be upper-cased.
pub fn id_of_bytes(bytes: &[u8]) -> u32 {
    let mut crc: u32 = 0;
    let mut i = 0;
    let len = bytes.len();
    while i < len {
        // Assemble one little-endian 32-bit group, zero-padding a short tail.
        let mut group: u32 = 0;
        for j in 0..4 {
            let idx = i + j;
            if idx < len {
                group |= (bytes[idx] as u32) << (8 * j);
            }
        }
        crc = crc.rotate_left(1).wrapping_add(group);
        i += 4;
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_zero() {
        assert_eq!(id_of(""), 0);
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(id_of("local.mix"), id_of("LOCAL.MIX"));
        assert_eq!(id_of("Temperat.pal"), id_of("TEMPERAT.PAL"));
    }

    /// Known filename -> entry-id pairs, cross-checked against `radump list`
    /// output on the real `redalert.mix` / `main.mix` archives (see
    /// `tests/golden_assets.rs`). These pairs are just filenames and their
    /// hash, not extracted game content, so they are safe to commit even
    /// though they were discovered by inspecting the real assets.
    #[test]
    fn known_filename_ids_redalert_mix() {
        let cases: &[(&str, u32)] = &[
            ("nchires.mix", 0x821F_E12A),
            ("local.mix", 0x97A7_9A21),
            ("hires.mix", 0xA7E3_821F),
            ("lores.mix", 0xA7E3_9A2F),
            ("speech.mix", 0xAF72_2A1C),
        ];
        for &(name, expected) in cases {
            assert_eq!(id_of(name), expected, "id_of({name:?})");
        }
    }

    #[test]
    fn known_filename_ids_main_mix() {
        let cases: &[(&str, u32)] = &[
            ("movies1.mix", 0x8214_2D0C),
            ("conquer.mix", 0xA236_1104),
            ("russian.mix", 0xAA42_2128),
            ("allies.mix", 0xBF8E_2FD8),
            ("sounds.mix", 0xD3B2_3C1E),
            ("scores.mix", 0xE39A_0C20),
            ("snow.mix", 0x06E7_E9D4),
            ("interior.mix", 0x1239_18F7),
            ("temperat.mix", 0x4201_0709),
            ("general.mix", 0x7229_E10E),
        ];
        for &(name, expected) in cases {
            assert_eq!(id_of(name), expected, "id_of({name:?})");
        }
    }
}
