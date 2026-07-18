//! Property-based "never panics" tests for `ra-data`'s scenario pipeline.
//!
//! `ra-formats`' own `property_no_panic.rs` fuzzes each stage in isolation
//! (INI parsing, base64, chunked-LCW). This suite fuzzes the *composition* —
//! `Scenario::parse`, which chains INI parsing, `[MapPack]`/`[OverlayPack]`
//! reassembly, base64, and LCW decompression, then indexes the result by
//! `(x, y)` — so a bug that only appears at the seams between stages (e.g. a
//! length assumption that holds per-stage but not end-to-end) still gets
//! exercised.

use proptest::prelude::*;

use ra_data::scenario::Scenario;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn scenario_parse_never_panics(text in ".{0,4096}") {
        // Most arbitrary text won't even have a [Map] section, let alone a
        // decodable [MapPack] — that's fine, `Err` is the expected outcome
        // for almost every input. The property is just "never panics".
        let _ = Scenario::parse(&text);
    }

    #[test]
    fn scenario_parse_never_panics_on_raw_bytes(
        data in proptest::collection::vec(any::<u8>(), 0..4096),
    ) {
        // Same property via lossy UTF-8 conversion, matching the real
        // ingestion path (`ra_client::assets::load_from_bytes` reads INI
        // bytes out of a MIX entry with `String::from_utf8_lossy`).
        let text = String::from_utf8_lossy(&data);
        let _ = Scenario::parse(&text);
    }

    #[test]
    fn scenario_cell_lookup_never_panics_out_of_range(
        text in ".{0,2048}",
        x in any::<u32>(),
        y in any::<u32>(),
    ) {
        // Whether or not parsing succeeds, any (x, y) — including wildly
        // out-of-range ones — must be a safe, bounds-checked lookup.
        if let Ok(scen) = Scenario::parse(&text) {
            let _ = scen.cell(x, y);
            let _ = scen.overlay_at(x, y);
        }
    }
}
