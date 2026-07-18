//! `ra-formats` — pure parsers for the Command & Conquer: Red Alert (1996) data
//! formats. No game knowledge, no I/O policy: every parser operates over a
//! borrowed byte slice (`&[u8]`) so the crate stays I/O-agnostic and fuzzable.
//!
//! See `docs/DESIGN.md` §4.1 (crate layout) and milestone M1 (§4.7).
//!
//! Modules:
//! - [`mix`]    — MIX archives (plain + RA flagged/encrypted headers, nesting).
//! - [`cps`]    — CPS full-screen images (LCW; `PALETTE.CPS` remap source).
//! - [`crc`]    — the Westwood filename → entry-id hash.
//! - [`crypto`] — the Westwood public-key scheme + Blowfish (header decryption).
//! - [`pal`]    — 6-bit VGA palettes expanded to 8-bit RGB.
//! - [`shp`]    — SHP unit shapes (Format80 / LCW and Format40 / XOR-delta).
//! - [`tmpl`]   — theater template (icon/tileset) files (`.tem`/`.sno`/`.int`).
//! - [`codec`]  — the shared LCW and XOR-delta byte codecs.
//! - [`pack`]   — base64 + chunked-LCW "pack" decoding (`[MapPack]` blocks).
//! - [`ini`]    — a small case-insensitive INI reader.
//! - [`names`]  — a small built-in list of well-known RA filenames.

pub mod codec;
pub mod cps;
pub mod crc;
pub mod crypto;
pub mod ini;
pub mod mix;
pub mod names;
pub mod pack;
pub mod pal;
pub mod shp;
pub mod tmpl;

/// Error type shared by the format parsers.
///
/// Parsers never panic on malformed input; they return one of these instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatError {
    /// The input ended before a required field could be read.
    UnexpectedEof {
        /// What the parser was trying to read when it ran out of bytes.
        context: &'static str,
    },
    /// A structural field held a value the parser cannot handle.
    Invalid {
        /// Human-readable description of what was wrong.
        reason: &'static str,
    },
}

impl core::fmt::Display for FormatError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FormatError::UnexpectedEof { context } => {
                write!(f, "unexpected end of input while reading {context}")
            }
            FormatError::Invalid { reason } => write!(f, "invalid data: {reason}"),
        }
    }
}

impl std::error::Error for FormatError {}

/// Convenience result alias for the parsers.
pub type Result<T> = core::result::Result<T, FormatError>;
