//! Scenario INI semantics: the `[Map]` section (theater + playable rectangle)
//! and the decoded `[MapPack]` / `[OverlayPack]` terrain planes.
//!
//! The format-agnostic pieces (INI parsing, base64, chunked-LCW pack decode)
//! live in `ra-formats`; this module adds the RA-specific meaning on top. Ported
//! from `redalert/display.cpp` (`DisplayClass::Read_INI`), `redalert/map.cpp`
//! (`MapClass::Read_Binary`), and `redalert/overlay.cpp`
//! (`OverlayClass::Read_INI`).

use ra_formats::ini::Ini;
use ra_formats::pack::{decode_base64, decompress_pack};

/// Map width in cells (fixed in RA).
pub const MAP_CELL_W: u32 = 128;
/// Map height in cells (fixed in RA).
pub const MAP_CELL_H: u32 = 128;
/// Total cells in a map.
pub const MAP_CELL_TOTAL: u32 = MAP_CELL_W * MAP_CELL_H;

/// Overlay byte meaning "no overlay" (`OVERLAY_NONE = -1`).
pub const OVERLAY_NONE: u8 = 0xFF;

/// The three RA theaters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theater {
    /// Temperate (`.tem`, `temperat.mix`).
    Temperate,
    /// Snow (`.sno`, `snow.mix`).
    Snow,
    /// Interior (`.int`, `interior.mix`).
    Interior,
}

impl Theater {
    /// Parse a theater by its scenario `Theater=` name (case-insensitive).
    /// Defaults to temperate for unknown values, as the original does.
    pub fn from_name(name: &str) -> Theater {
        match name.to_ascii_uppercase().as_str() {
            "SNOW" => Theater::Snow,
            "INTERIOR" => Theater::Interior,
            _ => Theater::Temperate,
        }
    }

    /// The per-theater template file extension (`TEM`/`SNO`/`INT`).
    pub fn suffix(self) -> &'static str {
        match self {
            Theater::Temperate => "TEM",
            Theater::Snow => "SNO",
            Theater::Interior => "INT",
        }
    }

    /// The theater's MIX archive name (inside `main.mix`).
    pub fn mix_name(self) -> &'static str {
        match self {
            Theater::Temperate => "temperat.mix",
            Theater::Snow => "snow.mix",
            Theater::Interior => "interior.mix",
        }
    }

    /// The theater's palette file name.
    pub fn palette_name(self) -> &'static str {
        match self {
            Theater::Temperate => "temperat.pal",
            Theater::Snow => "snow.pal",
            Theater::Interior => "interior.pal",
        }
    }
}

/// One terrain cell: a template id and the icon number within that template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapCell {
    /// Template id (index into [`crate::templates`]). `0xFFFF` or `255` = none.
    pub template: u16,
    /// Icon number within the template.
    pub icon: u8,
}

/// Errors from scenario parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScenarioError {
    /// The `[Map]` section was missing.
    NoMapSection,
    /// The `[MapPack]` section was missing.
    NoMapPack,
    /// The decoded `[MapPack]` was too short to fill the map.
    ShortMapPack {
        /// How many bytes were actually decoded.
        got: usize,
    },
}

impl core::fmt::Display for ScenarioError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ScenarioError::NoMapSection => write!(f, "scenario has no [Map] section"),
            ScenarioError::NoMapPack => write!(f, "scenario has no [MapPack] section"),
            ScenarioError::ShortMapPack { got } => {
                write!(f, "decoded [MapPack] too short ({got} bytes)")
            }
        }
    }
}

impl std::error::Error for ScenarioError {}

/// A parsed scenario's terrain: theater, playable rectangle, and the full
/// 128×128 cell grid (plus overlay plane).
#[derive(Debug, Clone)]
pub struct Scenario {
    /// The theater (tileset) this map uses.
    pub theater: Theater,
    /// Playable rectangle top-left X within the 128×128 grid.
    pub map_x: u16,
    /// Playable rectangle top-left Y.
    pub map_y: u16,
    /// Playable rectangle width in cells.
    pub map_width: u16,
    /// Playable rectangle height in cells.
    pub map_height: u16,
    /// All 16384 cells, row-major (`index = y*128 + x`).
    pub cells: Vec<MapCell>,
    /// All 16384 overlay bytes, row-major; `0xFF` = none. Empty if no
    /// `[OverlayPack]` was present.
    pub overlay: Vec<u8>,
}

impl Scenario {
    /// Parse a scenario INI's terrain data.
    pub fn parse(text: &str) -> Result<Scenario, ScenarioError> {
        let ini = Ini::parse(text);
        Self::from_ini(&ini)
    }

    /// Parse from an already-parsed INI.
    pub fn from_ini(ini: &Ini) -> Result<Scenario, ScenarioError> {
        if !ini.has_section("Map") {
            return Err(ScenarioError::NoMapSection);
        }
        let theater = Theater::from_name(ini.get("Map", "Theater").unwrap_or("TEMPERATE"));
        let map_x = ini.get_int("Map", "X").unwrap_or(1).clamp(0, 127) as u16;
        let map_y = ini.get_int("Map", "Y").unwrap_or(1).clamp(0, 127) as u16;
        let map_width = ini.get_int("Map", "Width").unwrap_or(126).clamp(0, 128) as u16;
        let map_height = ini.get_int("Map", "Height").unwrap_or(126).clamp(0, 128) as u16;
        let new_ini_format = ini.get_int("Basic", "NewINIFormat").unwrap_or(0);

        let cells = decode_mappack(ini, new_ini_format)?;
        let overlay = decode_overlaypack(ini);

        Ok(Scenario {
            theater,
            map_x,
            map_y,
            map_width,
            map_height,
            cells,
            overlay,
        })
    }

    /// The cell at (x, y), or a clear cell if out of bounds.
    pub fn cell(&self, x: u32, y: u32) -> MapCell {
        if x >= MAP_CELL_W || y >= MAP_CELL_H {
            return MapCell {
                template: 0xFFFF,
                icon: 0,
            };
        }
        self.cells[(y * MAP_CELL_W + x) as usize]
    }

    /// The overlay byte at (x, y), or `0xFF` (none) if absent/out of bounds.
    pub fn overlay_at(&self, x: u32, y: u32) -> u8 {
        if self.overlay.is_empty() || x >= MAP_CELL_W || y >= MAP_CELL_H {
            return OVERLAY_NONE;
        }
        self.overlay[(y * MAP_CELL_W + x) as usize]
    }
}

/// Base64-decode and LCW-decompress a numbered pack section, e.g. `[MapPack]`.
fn decode_pack_section(ini: &Ini, section: &str) -> Option<Vec<u8>> {
    let text = ini.concat_block(section)?;
    let packed = decode_base64(text.as_bytes());
    Some(decompress_pack(&packed))
}

fn decode_mappack(ini: &Ini, new_ini_format: i64) -> Result<Vec<MapCell>, ScenarioError> {
    let raw = decode_pack_section(ini, "MapPack").ok_or(ScenarioError::NoMapPack)?;
    let total = MAP_CELL_TOTAL as usize;

    let mut cells = vec![
        MapCell {
            template: 0xFFFF,
            icon: 0,
        };
        total
    ];

    if new_ini_format >= 3 {
        // Planar: 16384 u16 template ids, then 16384 u8 icon numbers.
        let need = total * 2 + total;
        if raw.len() < need {
            return Err(ScenarioError::ShortMapPack { got: raw.len() });
        }
        for (i, cell) in cells.iter_mut().enumerate() {
            cell.template = u16::from_le_bytes([raw[i * 2], raw[i * 2 + 1]]);
        }
        let icon_base = total * 2;
        for (i, cell) in cells.iter_mut().enumerate() {
            cell.icon = raw[icon_base + i];
        }
    } else {
        // Legacy interleaved: [u16 template][u8 icon] per cell.
        let need = total * 3;
        if raw.len() < need {
            return Err(ScenarioError::ShortMapPack { got: raw.len() });
        }
        for (i, cell) in cells.iter_mut().enumerate() {
            let b = i * 3;
            cell.template = u16::from_le_bytes([raw[b], raw[b + 1]]);
            cell.icon = raw[b + 2];
        }
    }
    Ok(cells)
}

fn decode_overlaypack(ini: &Ini) -> Vec<u8> {
    let total = MAP_CELL_TOTAL as usize;
    match decode_pack_section(ini, "OverlayPack") {
        Some(mut raw) if raw.len() >= total => {
            raw.truncate(total);
            raw
        }
        Some(mut raw) => {
            raw.resize(total, OVERLAY_NONE);
            raw
        }
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theater_mapping() {
        assert_eq!(Theater::from_name("snow"), Theater::Snow);
        assert_eq!(Theater::from_name("bogus"), Theater::Temperate);
        assert_eq!(Theater::Snow.suffix(), "SNO");
        assert_eq!(Theater::Snow.palette_name(), "snow.pal");
    }

    #[test]
    fn missing_map_section_errors() {
        assert!(matches!(
            Scenario::parse("[Basic]\nName=x\n"),
            Err(ScenarioError::NoMapSection)
        ));
    }

    #[test]
    fn missing_mappack_errors() {
        let r = Scenario::parse("[Map]\nTheater=SNOW\nX=1\nY=1\nWidth=2\nHeight=2\n");
        assert!(matches!(r, Err(ScenarioError::NoMapPack)));
    }

    /// Build a tiny NewINIFormat=3 scenario end to end: MapPack encodes a full
    /// planar buffer where cell 0 = template 5 icon 7, everything else clear.
    #[test]
    fn decodes_planar_mappack() {
        use ra_formats::codec; // for a hand-built LCW chunk via the pack format
        let total = MAP_CELL_TOTAL as usize;

        // Build the 49152-byte plaintext.
        let mut plain = vec![0u8; total * 3];
        // cell 0 template = 5 (u16 LE at bytes 0..2)
        plain[0] = 5;
        plain[1] = 0;
        // all other templates 0xFFFF so they read as clear
        for i in 1..total {
            plain[i * 2] = 0xFF;
            plain[i * 2 + 1] = 0xFF;
        }
        // cell 0 icon = 7
        plain[total * 2] = 7;

        // LCW-compress is not available (only decompress), so emit the plaintext
        // as an LCW "medium copy from source" stream in <=63-byte runs, wrapped
        // in one pack chunk. Verify the chunk decodes back with the real codec.
        let mut lcw = Vec::new();
        for chunk in plain.chunks(63) {
            lcw.push(0x80 | (chunk.len() as u8)); // medium copy-from-source
            lcw.extend_from_slice(chunk);
        }
        lcw.push(0x80); // end
        let mut check = vec![0u8; plain.len()];
        codec::lcw_decompress(&lcw, &mut check);
        assert_eq!(check, plain);

        let mut pack = Vec::new();
        pack.extend_from_slice(&(lcw.len() as u16).to_le_bytes());
        pack.extend_from_slice(&(plain.len() as u16).to_le_bytes());
        pack.extend_from_slice(&lcw);

        // base64-encode the pack the simple way, then wrap in an INI.
        let b64 = base64_encode(&pack);
        let mut ini = String::from("[Basic]\nNewINIFormat=3\n[Map]\nTheater=SNOW\nX=1\nY=1\nWidth=4\nHeight=4\n[MapPack]\n");
        for (i, line) in b64.as_bytes().chunks(70).enumerate() {
            ini.push_str(&format!(
                "{}={}\n",
                i + 1,
                std::str::from_utf8(line).unwrap()
            ));
        }

        let scen = Scenario::parse(&ini).unwrap();
        assert_eq!(scen.theater, Theater::Snow);
        assert_eq!(scen.map_width, 4);
        assert_eq!(
            scen.cell(0, 0),
            MapCell {
                template: 5,
                icon: 7
            }
        );
        assert_eq!(scen.cell(1, 0).template, 0xFFFF);
    }

    // Minimal standard base64 encoder for the test above only.
    fn base64_encode(data: &[u8]) -> String {
        const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for c in data.chunks(3) {
            let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
            let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
            out.push(A[(n >> 18) as usize & 63] as char);
            out.push(A[(n >> 12) as usize & 63] as char);
            out.push(if c.len() > 1 {
                A[(n >> 6) as usize & 63] as char
            } else {
                '='
            });
            out.push(if c.len() > 2 {
                A[n as usize & 63] as char
            } else {
                '='
            });
        }
        out
    }
}
