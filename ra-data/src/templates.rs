//! Terrain template catalog — the mapping from a scenario cell's template id to
//! the base filename (per theater it gets a `.tem`/`.sno`/`.int` extension) and
//! the set of theaters that template is legal in.
//!
//! This table is a mechanical port of the original engine's `TemplateTypeClass`
//! catalog. The row index *is* the template id: the original allocates its
//! template type-classes in exact `TemplateType`-enum order and indexes them by
//! that enum value (`redalert/cdata.cpp` `Init_Heap`/`As_Reference`), so row `i`
//! here corresponds to `TemplateType == i`. Generated from the `TemplateType`
//! enum (`redalert/defines.h`) crossed with the declarations in
//! `redalert/cdata.cpp`; see `docs/DESIGN.md` §4.8 M2.
//!
//! Per-icon land types are NOT here — the original derives them at runtime from
//! each iconset file's control map, so they belong with the parsed template
//! (see `ra_formats::tmpl`), not this static table.

/// Theater legality flag: temperate (`.tem`).
pub const THEATER_TEMPERATE: u8 = 0b001;
/// Theater legality flag: snow (`.sno`).
pub const THEATER_SNOW: u8 = 0b010;
/// Theater legality flag: interior (`.int`).
pub const THEATER_INTERIOR: u8 = 0b100;

/// The clear-terrain template id (`CLEAR1`). A scenario cell whose template is
/// this, [`TEMPLATE_NONE`], or the legacy value 255 renders as clear terrain
/// with a position-scrambled icon (see `ra-client` compositing).
pub const TEMPLATE_CLEAR1: u16 = 0;
/// Sentinel stored in a scenario cell that has no template set.
pub const TEMPLATE_NONE: u16 = 0xFFFF;

/// One catalog row: `(base_filename, theater_flags)`.
pub type TemplateInfo = (&'static str, u8);

/// Template catalog indexed by template id. Row `i` is `TemplateType == i`.
pub static TEMPLATES: &[TemplateInfo] = &[
    ("CLEAR1", 0b111),   // 0 TEMPLATE_CLEAR1
    ("W1", 0b011),       // 1 TEMPLATE_WATER
    ("W2", 0b011),       // 2 TEMPLATE_WATER2
    ("SH01", 0b011),     // 3 TEMPLATE_SHORE01
    ("SH02", 0b011),     // 4 TEMPLATE_SHORE02
    ("SH03", 0b011),     // 5 TEMPLATE_SHORE03
    ("SH04", 0b011),     // 6 TEMPLATE_SHORE04
    ("SH05", 0b011),     // 7 TEMPLATE_SHORE05
    ("SH06", 0b011),     // 8 TEMPLATE_SHORE06
    ("SH07", 0b011),     // 9 TEMPLATE_SHORE07
    ("SH08", 0b011),     // 10 TEMPLATE_SHORE08
    ("SH09", 0b011),     // 11 TEMPLATE_SHORE09
    ("SH10", 0b011),     // 12 TEMPLATE_SHORE10
    ("SH11", 0b011),     // 13 TEMPLATE_SHORE11
    ("SH12", 0b011),     // 14 TEMPLATE_SHORE12
    ("SH13", 0b011),     // 15 TEMPLATE_SHORE13
    ("SH14", 0b011),     // 16 TEMPLATE_SHORE14
    ("SH15", 0b011),     // 17 TEMPLATE_SHORE15
    ("SH16", 0b011),     // 18 TEMPLATE_SHORE16
    ("SH17", 0b011),     // 19 TEMPLATE_SHORE17
    ("SH18", 0b011),     // 20 TEMPLATE_SHORE18
    ("SH19", 0b011),     // 21 TEMPLATE_SHORE19
    ("SH20", 0b011),     // 22 TEMPLATE_SHORE20
    ("SH21", 0b011),     // 23 TEMPLATE_SHORE21
    ("SH22", 0b011),     // 24 TEMPLATE_SHORE22
    ("SH23", 0b011),     // 25 TEMPLATE_SHORE23
    ("SH24", 0b011),     // 26 TEMPLATE_SHORE24
    ("SH25", 0b011),     // 27 TEMPLATE_SHORE25
    ("SH26", 0b011),     // 28 TEMPLATE_SHORE26
    ("SH27", 0b011),     // 29 TEMPLATE_SHORE27
    ("SH28", 0b011),     // 30 TEMPLATE_SHORE28
    ("SH29", 0b011),     // 31 TEMPLATE_SHORE29
    ("SH30", 0b011),     // 32 TEMPLATE_SHORE30
    ("SH31", 0b011),     // 33 TEMPLATE_SHORE31
    ("SH32", 0b011),     // 34 TEMPLATE_SHORE32
    ("SH33", 0b011),     // 35 TEMPLATE_SHORE33
    ("SH34", 0b011),     // 36 TEMPLATE_SHORE34
    ("SH35", 0b011),     // 37 TEMPLATE_SHORE35
    ("SH36", 0b011),     // 38 TEMPLATE_SHORE36
    ("SH37", 0b011),     // 39 TEMPLATE_SHORE37
    ("SH38", 0b011),     // 40 TEMPLATE_SHORE38
    ("SH39", 0b011),     // 41 TEMPLATE_SHORE39
    ("SH40", 0b011),     // 42 TEMPLATE_SHORE40
    ("SH41", 0b011),     // 43 TEMPLATE_SHORE41
    ("SH42", 0b011),     // 44 TEMPLATE_SHORE42
    ("SH43", 0b011),     // 45 TEMPLATE_SHORE43
    ("SH44", 0b011),     // 46 TEMPLATE_SHORE44
    ("SH45", 0b011),     // 47 TEMPLATE_SHORE45
    ("SH46", 0b011),     // 48 TEMPLATE_SHORE46
    ("SH47", 0b011),     // 49 TEMPLATE_SHORE47
    ("SH48", 0b011),     // 50 TEMPLATE_SHORE48
    ("SH49", 0b011),     // 51 TEMPLATE_SHORE49
    ("SH50", 0b011),     // 52 TEMPLATE_SHORE50
    ("SH51", 0b011),     // 53 TEMPLATE_SHORE51
    ("SH52", 0b011),     // 54 TEMPLATE_SHORE52
    ("SH53", 0b011),     // 55 TEMPLATE_SHORE53
    ("SH54", 0b011),     // 56 TEMPLATE_SHORE54
    ("SH55", 0b011),     // 57 TEMPLATE_SHORE55
    ("SH56", 0b011),     // 58 TEMPLATE_SHORE56
    ("WC01", 0b011),     // 59 TEMPLATE_SHORECLIFF01
    ("WC02", 0b011),     // 60 TEMPLATE_SHORECLIFF02
    ("WC03", 0b011),     // 61 TEMPLATE_SHORECLIFF03
    ("WC04", 0b011),     // 62 TEMPLATE_SHORECLIFF04
    ("WC05", 0b011),     // 63 TEMPLATE_SHORECLIFF05
    ("WC06", 0b011),     // 64 TEMPLATE_SHORECLIFF06
    ("WC07", 0b011),     // 65 TEMPLATE_SHORECLIFF07
    ("WC08", 0b011),     // 66 TEMPLATE_SHORECLIFF08
    ("WC09", 0b011),     // 67 TEMPLATE_SHORECLIFF09
    ("WC10", 0b011),     // 68 TEMPLATE_SHORECLIFF10
    ("WC11", 0b011),     // 69 TEMPLATE_SHORECLIFF11
    ("WC12", 0b011),     // 70 TEMPLATE_SHORECLIFF12
    ("WC13", 0b011),     // 71 TEMPLATE_SHORECLIFF13
    ("WC14", 0b011),     // 72 TEMPLATE_SHORECLIFF14
    ("WC15", 0b011),     // 73 TEMPLATE_SHORECLIFF15
    ("WC16", 0b011),     // 74 TEMPLATE_SHORECLIFF16
    ("WC17", 0b011),     // 75 TEMPLATE_SHORECLIFF17
    ("WC18", 0b011),     // 76 TEMPLATE_SHORECLIFF18
    ("WC19", 0b011),     // 77 TEMPLATE_SHORECLIFF19
    ("WC20", 0b011),     // 78 TEMPLATE_SHORECLIFF20
    ("WC21", 0b011),     // 79 TEMPLATE_SHORECLIFF21
    ("WC22", 0b011),     // 80 TEMPLATE_SHORECLIFF22
    ("WC23", 0b011),     // 81 TEMPLATE_SHORECLIFF23
    ("WC24", 0b011),     // 82 TEMPLATE_SHORECLIFF24
    ("WC25", 0b011),     // 83 TEMPLATE_SHORECLIFF25
    ("WC26", 0b011),     // 84 TEMPLATE_SHORECLIFF26
    ("WC27", 0b011),     // 85 TEMPLATE_SHORECLIFF27
    ("WC28", 0b011),     // 86 TEMPLATE_SHORECLIFF28
    ("WC29", 0b011),     // 87 TEMPLATE_SHORECLIFF29
    ("WC30", 0b011),     // 88 TEMPLATE_SHORECLIFF30
    ("WC31", 0b011),     // 89 TEMPLATE_SHORECLIFF31
    ("WC32", 0b011),     // 90 TEMPLATE_SHORECLIFF32
    ("WC33", 0b011),     // 91 TEMPLATE_SHORECLIFF33
    ("WC34", 0b011),     // 92 TEMPLATE_SHORECLIFF34
    ("WC35", 0b011),     // 93 TEMPLATE_SHORECLIFF35
    ("WC36", 0b011),     // 94 TEMPLATE_SHORECLIFF36
    ("WC37", 0b011),     // 95 TEMPLATE_SHORECLIFF37
    ("WC38", 0b011),     // 96 TEMPLATE_SHORECLIFF38
    ("B1", 0b011),       // 97 TEMPLATE_BOULDER1
    ("B2", 0b011),       // 98 TEMPLATE_BOULDER2
    ("B3", 0b011),       // 99 TEMPLATE_BOULDER3
    ("B4", 0b011),       // 100 TEMPLATE_BOULDER4
    ("B5", 0b011),       // 101 TEMPLATE_BOULDER5
    ("B6", 0b011),       // 102 TEMPLATE_BOULDER6
    ("P01", 0b011),      // 103 TEMPLATE_PATCH01
    ("P02", 0b011),      // 104 TEMPLATE_PATCH02
    ("P03", 0b011),      // 105 TEMPLATE_PATCH03
    ("P04", 0b011),      // 106 TEMPLATE_PATCH04
    ("P07", 0b011),      // 107 TEMPLATE_PATCH07
    ("P08", 0b011),      // 108 TEMPLATE_PATCH08
    ("P13", 0b011),      // 109 TEMPLATE_PATCH13
    ("P14", 0b011),      // 110 TEMPLATE_PATCH14
    ("P15", 0b011),      // 111 TEMPLATE_PATCH15
    ("RV01", 0b011),     // 112 TEMPLATE_RIVER01
    ("RV02", 0b011),     // 113 TEMPLATE_RIVER02
    ("RV03", 0b011),     // 114 TEMPLATE_RIVER03
    ("RV04", 0b011),     // 115 TEMPLATE_RIVER04
    ("RV05", 0b011),     // 116 TEMPLATE_RIVER05
    ("RV06", 0b011),     // 117 TEMPLATE_RIVER06
    ("RV07", 0b011),     // 118 TEMPLATE_RIVER07
    ("RV08", 0b011),     // 119 TEMPLATE_RIVER08
    ("RV09", 0b011),     // 120 TEMPLATE_RIVER09
    ("RV10", 0b011),     // 121 TEMPLATE_RIVER10
    ("RV11", 0b011),     // 122 TEMPLATE_RIVER11
    ("RV12", 0b011),     // 123 TEMPLATE_RIVER12
    ("RV13", 0b011),     // 124 TEMPLATE_RIVER13
    ("FALLS1", 0b011),   // 125 TEMPLATE_FALLS1
    ("FALLS1A", 0b011),  // 126 TEMPLATE_FALLS1A
    ("FALLS2", 0b011),   // 127 TEMPLATE_FALLS2
    ("FALLS2A", 0b011),  // 128 TEMPLATE_FALLS2A
    ("FORD1", 0b011),    // 129 TEMPLATE_FORD1
    ("FORD2", 0b011),    // 130 TEMPLATE_FORD2
    ("BRIDGE1", 0b011),  // 131 TEMPLATE_BRIDGE1
    ("BRIDGE1D", 0b011), // 132 TEMPLATE_BRIDGE1D
    ("BRIDGE2", 0b011),  // 133 TEMPLATE_BRIDGE2
    ("BRIDGE2D", 0b011), // 134 TEMPLATE_BRIDGE2D
    ("S01", 0b011),      // 135 TEMPLATE_SLOPE01
    ("S02", 0b011),      // 136 TEMPLATE_SLOPE02
    ("S03", 0b011),      // 137 TEMPLATE_SLOPE03
    ("S04", 0b011),      // 138 TEMPLATE_SLOPE04
    ("S05", 0b011),      // 139 TEMPLATE_SLOPE05
    ("S06", 0b011),      // 140 TEMPLATE_SLOPE06
    ("S07", 0b011),      // 141 TEMPLATE_SLOPE07
    ("S08", 0b011),      // 142 TEMPLATE_SLOPE08
    ("S09", 0b011),      // 143 TEMPLATE_SLOPE09
    ("S10", 0b011),      // 144 TEMPLATE_SLOPE10
    ("S11", 0b011),      // 145 TEMPLATE_SLOPE11
    ("S12", 0b011),      // 146 TEMPLATE_SLOPE12
    ("S13", 0b011),      // 147 TEMPLATE_SLOPE13
    ("S14", 0b011),      // 148 TEMPLATE_SLOPE14
    ("S15", 0b011),      // 149 TEMPLATE_SLOPE15
    ("S16", 0b011),      // 150 TEMPLATE_SLOPE16
    ("S17", 0b011),      // 151 TEMPLATE_SLOPE17
    ("S18", 0b011),      // 152 TEMPLATE_SLOPE18
    ("S19", 0b011),      // 153 TEMPLATE_SLOPE19
    ("S20", 0b011),      // 154 TEMPLATE_SLOPE20
    ("S21", 0b011),      // 155 TEMPLATE_SLOPE21
    ("S22", 0b011),      // 156 TEMPLATE_SLOPE22
    ("S23", 0b011),      // 157 TEMPLATE_SLOPE23
    ("S24", 0b011),      // 158 TEMPLATE_SLOPE24
    ("S25", 0b011),      // 159 TEMPLATE_SLOPE25
    ("S26", 0b011),      // 160 TEMPLATE_SLOPE26
    ("S27", 0b011),      // 161 TEMPLATE_SLOPE27
    ("S28", 0b011),      // 162 TEMPLATE_SLOPE28
    ("S29", 0b011),      // 163 TEMPLATE_SLOPE29
    ("S30", 0b011),      // 164 TEMPLATE_SLOPE30
    ("S31", 0b011),      // 165 TEMPLATE_SLOPE31
    ("S32", 0b011),      // 166 TEMPLATE_SLOPE32
    ("S33", 0b011),      // 167 TEMPLATE_SLOPE33
    ("S34", 0b011),      // 168 TEMPLATE_SLOPE34
    ("S35", 0b011),      // 169 TEMPLATE_SLOPE35
    ("S36", 0b011),      // 170 TEMPLATE_SLOPE36
    ("S37", 0b011),      // 171 TEMPLATE_SLOPE37
    ("S38", 0b011),      // 172 TEMPLATE_SLOPE38
    ("D01", 0b011),      // 173 TEMPLATE_ROAD01
    ("D02", 0b011),      // 174 TEMPLATE_ROAD02
    ("D03", 0b011),      // 175 TEMPLATE_ROAD03
    ("D04", 0b011),      // 176 TEMPLATE_ROAD04
    ("D05", 0b011),      // 177 TEMPLATE_ROAD05
    ("D06", 0b011),      // 178 TEMPLATE_ROAD06
    ("D07", 0b011),      // 179 TEMPLATE_ROAD07
    ("D08", 0b011),      // 180 TEMPLATE_ROAD08
    ("D09", 0b011),      // 181 TEMPLATE_ROAD09
    ("D10", 0b011),      // 182 TEMPLATE_ROAD10
    ("D11", 0b011),      // 183 TEMPLATE_ROAD11
    ("D12", 0b011),      // 184 TEMPLATE_ROAD12
    ("D13", 0b011),      // 185 TEMPLATE_ROAD13
    ("D14", 0b011),      // 186 TEMPLATE_ROAD14
    ("D15", 0b011),      // 187 TEMPLATE_ROAD15
    ("D16", 0b011),      // 188 TEMPLATE_ROAD16
    ("D17", 0b011),      // 189 TEMPLATE_ROAD17
    ("D18", 0b011),      // 190 TEMPLATE_ROAD18
    ("D19", 0b011),      // 191 TEMPLATE_ROAD19
    ("D20", 0b011),      // 192 TEMPLATE_ROAD20
    ("D21", 0b011),      // 193 TEMPLATE_ROAD21
    ("D22", 0b011),      // 194 TEMPLATE_ROAD22
    ("D23", 0b011),      // 195 TEMPLATE_ROAD23
    ("D24", 0b011),      // 196 TEMPLATE_ROAD24
    ("D25", 0b011),      // 197 TEMPLATE_ROAD25
    ("D26", 0b011),      // 198 TEMPLATE_ROAD26
    ("D27", 0b011),      // 199 TEMPLATE_ROAD27
    ("D28", 0b011),      // 200 TEMPLATE_ROAD28
    ("D29", 0b011),      // 201 TEMPLATE_ROAD29
    ("D30", 0b011),      // 202 TEMPLATE_ROAD30
    ("D31", 0b011),      // 203 TEMPLATE_ROAD31
    ("D32", 0b011),      // 204 TEMPLATE_ROAD32
    ("D33", 0b011),      // 205 TEMPLATE_ROAD33
    ("D34", 0b011),      // 206 TEMPLATE_ROAD34
    ("D35", 0b011),      // 207 TEMPLATE_ROAD35
    ("D36", 0b011),      // 208 TEMPLATE_ROAD36
    ("D37", 0b011),      // 209 TEMPLATE_ROAD37
    ("D38", 0b011),      // 210 TEMPLATE_ROAD38
    ("D39", 0b011),      // 211 TEMPLATE_ROAD39
    ("D40", 0b011),      // 212 TEMPLATE_ROAD40
    ("D41", 0b011),      // 213 TEMPLATE_ROAD41
    ("D42", 0b011),      // 214 TEMPLATE_ROAD42
    ("D43", 0b011),      // 215 TEMPLATE_ROAD43
    ("RF01", 0b011),     // 216 TEMPLATE_ROUGH01
    ("RF02", 0b011),     // 217 TEMPLATE_ROUGH02
    ("RF03", 0b011),     // 218 TEMPLATE_ROUGH03
    ("RF04", 0b011),     // 219 TEMPLATE_ROUGH04
    ("RF05", 0b011),     // 220 TEMPLATE_ROUGH05
    ("RF06", 0b011),     // 221 TEMPLATE_ROUGH06
    ("RF07", 0b011),     // 222 TEMPLATE_ROUGH07
    ("RF08", 0b011),     // 223 TEMPLATE_ROUGH08
    ("RF09", 0b011),     // 224 TEMPLATE_ROUGH09
    ("RF10", 0b011),     // 225 TEMPLATE_ROUGH10
    ("RF11", 0b011),     // 226 TEMPLATE_ROUGH11
    ("D44", 0b011),      // 227 TEMPLATE_ROAD44
    ("D45", 0b011),      // 228 TEMPLATE_ROAD45
    ("RV14", 0b011),     // 229 TEMPLATE_RIVER14
    ("RV15", 0b011),     // 230 TEMPLATE_RIVER15
    ("RC01", 0b011),     // 231 TEMPLATE_RIVERCLIFF01
    ("RC02", 0b011),     // 232 TEMPLATE_RIVERCLIFF02
    ("RC03", 0b011),     // 233 TEMPLATE_RIVERCLIFF03
    ("RC04", 0b011),     // 234 TEMPLATE_RIVERCLIFF04
    ("BR1A", 0b011),     // 235 TEMPLATE_BRIDGE_1A
    ("BR1B", 0b011),     // 236 TEMPLATE_BRIDGE_1B
    ("BR1C", 0b011),     // 237 TEMPLATE_BRIDGE_1C
    ("BR2A", 0b011),     // 238 TEMPLATE_BRIDGE_2A
    ("BR2B", 0b011),     // 239 TEMPLATE_BRIDGE_2B
    ("BR2C", 0b011),     // 240 TEMPLATE_BRIDGE_2C
    ("BR3A", 0b011),     // 241 TEMPLATE_BRIDGE_3A
    ("BR3B", 0b011),     // 242 TEMPLATE_BRIDGE_3B
    ("BR3C", 0b011),     // 243 TEMPLATE_BRIDGE_3C
    ("BR3D", 0b011),     // 244 TEMPLATE_BRIDGE_3D
    ("BR3E", 0b011),     // 245 TEMPLATE_BRIDGE_3E
    ("BR3F", 0b011),     // 246 TEMPLATE_BRIDGE_3F
    ("F01", 0b011),      // 247 TEMPLATE_F01
    ("F02", 0b011),      // 248 TEMPLATE_F02
    ("F03", 0b011),      // 249 TEMPLATE_F03
    ("F04", 0b011),      // 250 TEMPLATE_F04
    ("F05", 0b011),      // 251 TEMPLATE_F05
    ("F06", 0b011),      // 252 TEMPLATE_F06
    ("ARRO0001", 0b100), // 253 TEMPLATE_ARRO0001
    ("ARRO0002", 0b100), // 254 TEMPLATE_ARRO0002
    ("ARRO0003", 0b100), // 255 TEMPLATE_ARRO0003
    ("ARRO0004", 0b100), // 256 TEMPLATE_ARRO0004
    ("ARRO0005", 0b100), // 257 TEMPLATE_ARRO0005
    ("ARRO0006", 0b100), // 258 TEMPLATE_ARRO0006
    ("ARRO0007", 0b100), // 259 TEMPLATE_ARRO0007
    ("ARRO0008", 0b100), // 260 TEMPLATE_ARRO0008
    ("ARRO0009", 0b100), // 261 TEMPLATE_ARRO0009
    ("ARRO0010", 0b100), // 262 TEMPLATE_ARRO0010
    ("ARRO0011", 0b100), // 263 TEMPLATE_ARRO0011
    ("ARRO0012", 0b100), // 264 TEMPLATE_ARRO0012
    ("ARRO0013", 0b100), // 265 TEMPLATE_ARRO0013
    ("ARRO0014", 0b100), // 266 TEMPLATE_ARRO0014
    ("ARRO0015", 0b100), // 267 TEMPLATE_ARRO0015
    ("FLOR0001", 0b100), // 268 TEMPLATE_FLOR0001
    ("FLOR0002", 0b100), // 269 TEMPLATE_FLOR0002
    ("FLOR0003", 0b100), // 270 TEMPLATE_FLOR0003
    ("FLOR0004", 0b100), // 271 TEMPLATE_FLOR0004
    ("FLOR0005", 0b100), // 272 TEMPLATE_FLOR0005
    ("FLOR0006", 0b100), // 273 TEMPLATE_FLOR0006
    ("FLOR0007", 0b100), // 274 TEMPLATE_FLOR0007
    ("GFLR0001", 0b100), // 275 TEMPLATE_GFLR0001
    ("GFLR0002", 0b100), // 276 TEMPLATE_GFLR0002
    ("GFLR0003", 0b100), // 277 TEMPLATE_GFLR0003
    ("GFLR0004", 0b100), // 278 TEMPLATE_GFLR0004
    ("GFLR0005", 0b100), // 279 TEMPLATE_GFLR0005
    ("GSTR0001", 0b100), // 280 TEMPLATE_GSTR0001
    ("GSTR0002", 0b100), // 281 TEMPLATE_GSTR0002
    ("GSTR0003", 0b100), // 282 TEMPLATE_GSTR0003
    ("GSTR0004", 0b100), // 283 TEMPLATE_GSTR0004
    ("GSTR0005", 0b100), // 284 TEMPLATE_GSTR0005
    ("GSTR0006", 0b100), // 285 TEMPLATE_GSTR0006
    ("GSTR0007", 0b100), // 286 TEMPLATE_GSTR0007
    ("GSTR0008", 0b100), // 287 TEMPLATE_GSTR0008
    ("GSTR0009", 0b100), // 288 TEMPLATE_GSTR0009
    ("GSTR0010", 0b100), // 289 TEMPLATE_GSTR0010
    ("GSTR0011", 0b100), // 290 TEMPLATE_GSTR0011
    ("LWAL0001", 0b100), // 291 TEMPLATE_LWAL0001
    ("LWAL0002", 0b100), // 292 TEMPLATE_LWAL0002
    ("LWAL0003", 0b100), // 293 TEMPLATE_LWAL0003
    ("LWAL0004", 0b100), // 294 TEMPLATE_LWAL0004
    ("LWAL0005", 0b100), // 295 TEMPLATE_LWAL0005
    ("LWAL0006", 0b100), // 296 TEMPLATE_LWAL0006
    ("LWAL0007", 0b100), // 297 TEMPLATE_LWAL0007
    ("LWAL0008", 0b100), // 298 TEMPLATE_LWAL0008
    ("LWAL0009", 0b100), // 299 TEMPLATE_LWAL0009
    ("LWAL0010", 0b100), // 300 TEMPLATE_LWAL0010
    ("LWAL0011", 0b100), // 301 TEMPLATE_LWAL0011
    ("LWAL0012", 0b100), // 302 TEMPLATE_LWAL0012
    ("LWAL0013", 0b100), // 303 TEMPLATE_LWAL0013
    ("LWAL0014", 0b100), // 304 TEMPLATE_LWAL0014
    ("LWAL0015", 0b100), // 305 TEMPLATE_LWAL0015
    ("LWAL0016", 0b100), // 306 TEMPLATE_LWAL0016
    ("LWAL0017", 0b100), // 307 TEMPLATE_LWAL0017
    ("LWAL0018", 0b100), // 308 TEMPLATE_LWAL0018
    ("LWAL0019", 0b100), // 309 TEMPLATE_LWAL0019
    ("LWAL0020", 0b100), // 310 TEMPLATE_LWAL0020
    ("LWAL0021", 0b100), // 311 TEMPLATE_LWAL0021
    ("LWAL0022", 0b100), // 312 TEMPLATE_LWAL0022
    ("LWAL0023", 0b100), // 313 TEMPLATE_LWAL0023
    ("LWAL0024", 0b100), // 314 TEMPLATE_LWAL0024
    ("LWAL0025", 0b100), // 315 TEMPLATE_LWAL0025
    ("LWAL0026", 0b100), // 316 TEMPLATE_LWAL0026
    ("LWAL0027", 0b100), // 317 TEMPLATE_LWAL0027
    ("STRP0001", 0b100), // 318 TEMPLATE_STRP0001
    ("STRP0002", 0b100), // 319 TEMPLATE_STRP0002
    ("STRP0003", 0b100), // 320 TEMPLATE_STRP0003
    ("STRP0004", 0b100), // 321 TEMPLATE_STRP0004
    ("STRP0005", 0b100), // 322 TEMPLATE_STRP0005
    ("STRP0006", 0b100), // 323 TEMPLATE_STRP0006
    ("STRP0007", 0b100), // 324 TEMPLATE_STRP0007
    ("STRP0008", 0b100), // 325 TEMPLATE_STRP0008
    ("STRP0009", 0b100), // 326 TEMPLATE_STRP0009
    ("STRP0010", 0b100), // 327 TEMPLATE_STRP0010
    ("STRP0011", 0b100), // 328 TEMPLATE_STRP0011
    ("WALL0001", 0b100), // 329 TEMPLATE_WALL0001
    ("WALL0002", 0b100), // 330 TEMPLATE_WALL0002
    ("WALL0003", 0b100), // 331 TEMPLATE_WALL0003
    ("WALL0004", 0b100), // 332 TEMPLATE_WALL0004
    ("WALL0005", 0b100), // 333 TEMPLATE_WALL0005
    ("WALL0006", 0b100), // 334 TEMPLATE_WALL0006
    ("WALL0007", 0b100), // 335 TEMPLATE_WALL0007
    ("WALL0008", 0b100), // 336 TEMPLATE_WALL0008
    ("WALL0009", 0b100), // 337 TEMPLATE_WALL0009
    ("WALL0010", 0b100), // 338 TEMPLATE_WALL0010
    ("WALL0011", 0b100), // 339 TEMPLATE_WALL0011
    ("WALL0012", 0b100), // 340 TEMPLATE_WALL0012
    ("WALL0013", 0b100), // 341 TEMPLATE_WALL0013
    ("WALL0014", 0b100), // 342 TEMPLATE_WALL0014
    ("WALL0015", 0b100), // 343 TEMPLATE_WALL0015
    ("WALL0016", 0b100), // 344 TEMPLATE_WALL0016
    ("WALL0017", 0b100), // 345 TEMPLATE_WALL0017
    ("WALL0018", 0b100), // 346 TEMPLATE_WALL0018
    ("WALL0019", 0b100), // 347 TEMPLATE_WALL0019
    ("WALL0020", 0b100), // 348 TEMPLATE_WALL0020
    ("WALL0021", 0b100), // 349 TEMPLATE_WALL0021
    ("WALL0022", 0b100), // 350 TEMPLATE_WALL0022
    ("WALL0023", 0b100), // 351 TEMPLATE_WALL0023
    ("WALL0024", 0b100), // 352 TEMPLATE_WALL0024
    ("WALL0025", 0b100), // 353 TEMPLATE_WALL0025
    ("WALL0026", 0b100), // 354 TEMPLATE_WALL0026
    ("WALL0027", 0b100), // 355 TEMPLATE_WALL0027
    ("WALL0028", 0b100), // 356 TEMPLATE_WALL0028
    ("WALL0029", 0b100), // 357 TEMPLATE_WALL0029
    ("WALL0030", 0b100), // 358 TEMPLATE_WALL0030
    ("WALL0031", 0b100), // 359 TEMPLATE_WALL0031
    ("WALL0032", 0b100), // 360 TEMPLATE_WALL0032
    ("WALL0033", 0b100), // 361 TEMPLATE_WALL0033
    ("WALL0034", 0b100), // 362 TEMPLATE_WALL0034
    ("WALL0035", 0b100), // 363 TEMPLATE_WALL0035
    ("WALL0036", 0b100), // 364 TEMPLATE_WALL0036
    ("WALL0037", 0b100), // 365 TEMPLATE_WALL0037
    ("WALL0038", 0b100), // 366 TEMPLATE_WALL0038
    ("WALL0039", 0b100), // 367 TEMPLATE_WALL0039
    ("WALL0040", 0b100), // 368 TEMPLATE_WALL0040
    ("WALL0041", 0b100), // 369 TEMPLATE_WALL0041
    ("WALL0042", 0b100), // 370 TEMPLATE_WALL0042
    ("WALL0043", 0b100), // 371 TEMPLATE_WALL0043
    ("WALL0044", 0b100), // 372 TEMPLATE_WALL0044
    ("WALL0045", 0b100), // 373 TEMPLATE_WALL0045
    ("WALL0046", 0b100), // 374 TEMPLATE_WALL0046
    ("WALL0047", 0b100), // 375 TEMPLATE_WALL0047
    ("WALL0048", 0b100), // 376 TEMPLATE_WALL0048
    ("WALL0049", 0b100), // 377 TEMPLATE_WALL0049
    ("BRIDGE1H", 0b011), // 378 TEMPLATE_BRIDGE1H
    ("BRIDGE2H", 0b011), // 379 TEMPLATE_BRIDGE2H
    ("BR1X", 0b011),     // 380 TEMPLATE_BRIDGE_1AX
    ("BR2X", 0b011),     // 381 TEMPLATE_BRIDGE_2AX
    ("BRIDGE1X", 0b011), // 382 TEMPLATE_BRIDGE1X
    ("BRIDGE2X", 0b011), // 383 TEMPLATE_BRIDGE2X
    ("XTRA0001", 0b100), // 384 TEMPLATE_XTRA0001
    ("XTRA0002", 0b100), // 385 TEMPLATE_XTRA0002
    ("XTRA0003", 0b100), // 386 TEMPLATE_XTRA0003
    ("XTRA0004", 0b100), // 387 TEMPLATE_XTRA0004
    ("XTRA0005", 0b100), // 388 TEMPLATE_XTRA0005
    ("XTRA0006", 0b100), // 389 TEMPLATE_XTRA0006
    ("XTRA0007", 0b100), // 390 TEMPLATE_XTRA0007
    ("XTRA0008", 0b100), // 391 TEMPLATE_XTRA0008
    ("XTRA0009", 0b100), // 392 TEMPLATE_XTRA0009
    ("XTRA0010", 0b100), // 393 TEMPLATE_XTRA0010
    ("XTRA0011", 0b100), // 394 TEMPLATE_XTRA0011
    ("XTRA0012", 0b100), // 395 TEMPLATE_XTRA0012
    ("XTRA0013", 0b100), // 396 TEMPLATE_XTRA0013
    ("XTRA0014", 0b100), // 397 TEMPLATE_XTRA0014
    ("XTRA0015", 0b100), // 398 TEMPLATE_XTRA0015
    ("XTRA0016", 0b100), // 399 TEMPLATE_XTRA0016
    ("HILL01", 0b001),   // 400 TEMPLATE_HILL01
];

/// Look up a template's base filename and theater flags by id.
pub fn template(id: u16) -> Option<TemplateInfo> {
    TEMPLATES.get(id as usize).copied()
}

/// Build a template's per-theater filename, e.g. id 3 + `"SNO"` -> `"SH01.SNO"`.
/// Returns `None` if the id is unknown.
pub fn template_filename(id: u16, theater_suffix: &str) -> Option<String> {
    template(id).map(|(base, _)| format!("{base}.{theater_suffix}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_all_templates() {
        // The original defines 401 templates (with FIXIT_ANTS).
        assert_eq!(TEMPLATES.len(), 401);
    }

    #[test]
    fn clear_is_zero() {
        assert_eq!(TEMPLATES[0].0, "CLEAR1");
        assert_eq!(TEMPLATE_CLEAR1, 0);
    }

    #[test]
    fn known_ids() {
        assert_eq!(template(1).unwrap().0, "W1"); // TEMPLATE_WATER
        assert_eq!(template(3).unwrap().0, "SH01"); // TEMPLATE_SHORE01
        assert_eq!(template_filename(3, "SNO").unwrap(), "SH01.SNO");
    }

    #[test]
    fn every_row_has_a_nonempty_name_and_at_least_one_theater() {
        // Sanity over the whole generated catalog, no assets needed: every
        // row must have a real base filename and be legal in at least one
        // theater (a template legal in zero theaters would be dead data,
        // most likely a transcription slip from the source enum).
        for (i, &(name, flags)) in TEMPLATES.iter().enumerate() {
            assert!(!name.is_empty(), "row {i} has an empty base filename");
            assert_ne!(flags, 0, "row {i} ('{name}') is legal in zero theaters");
            assert_eq!(
                flags & !(THEATER_TEMPERATE | THEATER_SNOW | THEATER_INTERIOR),
                0,
                "row {i} ('{name}') has unknown theater flag bits set: {flags:#b}"
            );
        }
    }

    #[test]
    fn out_of_range_id_is_none() {
        assert!(template(401).is_none());
        assert!(template(u16::MAX).is_none());
        assert!(template_filename(401, "TEM").is_none());
    }
}
