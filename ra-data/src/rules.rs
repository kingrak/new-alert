//! Unit stats read from `rules.ini` — the single source of unit numbers
//! (DESIGN.md §3.8). `rules.ini` lives inside `redalert.mix → local.mix`; this
//! module only needs the already-decoded INI text.
//!
//! We read the handful of fields M3 movement needs — `Speed`, `ROT`, and
//! `Strength` — for the starter vehicles (2TNK, 1TNK, JEEP, …). Behaviour and
//! the rest of the stat block arrive with later milestones.
//!
//! **Speed semantics** (ported from `CCINIClass::Get_MPHType`, `ccini.cpp`, and
//! `TechnoTypeClass::Read_INI`, `techno.cpp:7064`): the INI `Speed=` value is a
//! 0..100 number where 100 means "256 leptons per tick" (a whole cell per game
//! frame). So `max_speed_leptons = Speed * 256 / 100`.

use ra_formats::ini::Ini;

/// The movement-relevant stats of one unit type, as read from `rules.ini`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnitStats {
    /// Raw `Speed=` value (0..100). Convert with [`UnitStats::max_speed_leptons`].
    pub speed: i32,
    /// Raw `ROT=` value (rate of turn, binary-angle units per tick before the
    /// original's `+1`).
    pub rot: u8,
    /// `Strength=` (max health) — carried so scenario health percentages resolve.
    pub strength: i32,
    /// `Sight=` range in cells (`techno.cpp:7062`) — how far this unit reveals the
    /// shroud (M6). Defaults to 2 (the embedded `udata.cpp` default) when absent.
    pub sight: u8,
}

impl UnitStats {
    /// Top speed in leptons per tick (256 = one cell per tick). `Speed * 256/100`.
    pub fn max_speed_leptons(&self) -> i32 {
        self.speed * 256 / 100
    }
}

/// Read one unit type's stats from a parsed `rules.ini` by its section name
/// (e.g. `"2TNK"`). Returns `None` if the section is absent or has no `Speed=`
/// (i.e. it is not a self-propelled unit we can move at M3).
pub fn unit_stats(ini: &Ini, name: &str) -> Option<UnitStats> {
    if !ini.has_section(name) {
        return None;
    }
    let speed = ini.get_int(name, "Speed")? as i32;
    let rot = ini.get_int(name, "ROT").unwrap_or(0).clamp(0, 255) as u8;
    let strength = ini.get_int(name, "Strength").unwrap_or(0) as i32;
    let sight = ini.get_int(name, "Sight").unwrap_or(2).clamp(0, 10) as u8;
    Some(UnitStats {
        speed,
        rot,
        strength,
        sight,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Ini {
        // Trimmed real values (see rules.ini): medium tank, light tank, jeep.
        Ini::parse(
            "[2TNK]\nSpeed=8\nROT=5\nStrength=400\n\
             [1TNK]\nSpeed=9\nROT=5\nStrength=300\n\
             [JEEP]\nSpeed=10\nROT=10\nStrength=150\n\
             [BARL]\nStrength=1\n", // a non-moving thing: no Speed
        )
    }

    #[test]
    fn reads_starter_units() {
        let ini = rules();
        let tnk = unit_stats(&ini, "2TNK").unwrap();
        assert_eq!(tnk.speed, 8);
        assert_eq!(tnk.rot, 5);
        assert_eq!(tnk.strength, 400);
        // Speed 8 -> 8*256/100 = 20 leptons/tick.
        assert_eq!(tnk.max_speed_leptons(), 20);

        let jeep = unit_stats(&ini, "JEEP").unwrap();
        assert_eq!(jeep.max_speed_leptons(), 25); // 10*256/100
        assert_eq!(jeep.rot, 10);
    }

    #[test]
    fn case_insensitive_lookup() {
        let ini = rules();
        assert!(unit_stats(&ini, "jeep").is_some());
    }

    #[test]
    fn missing_or_speedless_is_none() {
        let ini = rules();
        assert!(unit_stats(&ini, "NOPE").is_none());
        assert!(unit_stats(&ini, "BARL").is_none()); // no Speed field
    }
}
