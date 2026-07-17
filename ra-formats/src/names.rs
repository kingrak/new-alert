//! A small built-in list of well-known Red Alert filenames, used to reverse the
//! MIX entry-id hash back into a human-readable name for `radump`. This is
//! deliberately tiny — just the top-level nested archives, common palettes, and
//! a handful of representative unit/infantry shapes — not a full name database.

/// A representative sample of well-known RA filenames.
pub const KNOWN_NAMES: &[&str] = &[
    // Nested archives contained in redalert.mix / main.mix.
    "local.mix",
    "hires.mix",
    "lores.mix",
    "nchires.mix",
    "speech.mix",
    "sounds.mix",
    "russian.mix",
    "allies.mix",
    "conquer.mix",
    "general.mix",
    "movies1.mix",
    "movies2.mix",
    "scores.mix",
    "interior.mix",
    "snow.mix",
    "temperat.mix",
    "expand.mix",
    // Theater palettes and data.
    "temperat.pal",
    "snow.pal",
    "interior.pal",
    "palette.pal",
    "temperat.mrf",
    // Rules / config.
    "rules.ini",
    "conquer.ini",
    // A few representative unit / infantry / structure shapes.
    "e1.shp",
    "e2.shp",
    "e3.shp",
    "e4.shp",
    "e6.shp",
    "e7.shp",
    "dog.shp",
    "medi.shp",
    "mech.shp",
    "1tnk.shp",
    "2tnk.shp",
    "3tnk.shp",
    "4tnk.shp",
    "jeep.shp",
    "harv.shp",
    "mcv.shp",
    "apc.shp",
    "arty.shp",
    "mnly.shp",
    "truk.shp",
    "v2rl.shp",
    "ftnk.shp",
    "mgg.shp",
    "mrj.shp",
    "ss.shp",
    "dd.shp",
    "ca.shp",
    "pt.shp",
    "lst.shp",
    "heli.shp",
    "hind.shp",
    "yak.shp",
    "mig.shp",
    "tran.shp",
    "orca.shp",
    // Cursors / mouse.
    "mouse.shp",
];

/// Reverse a MIX entry id back to a known filename, if one is in the built-in
/// list.
pub fn lookup(id: u32) -> Option<&'static str> {
    KNOWN_NAMES
        .iter()
        .copied()
        .find(|&name| crate::crc::id_of(name) == id)
}
