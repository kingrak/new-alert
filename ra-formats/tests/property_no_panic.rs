//! Property-based "never panics" tests over arbitrary / truncated bytes for
//! every parser and codec in this crate's public API. These are robustness
//! properties, not correctness ones: malformed input must produce a `Result`
//! (or, for the codecs, a best-effort partial decode), never a panic.
//!
//! `proptest` is a justified dev-dependency here (see `Cargo.toml`): the
//! crate's stated design goal is "trivially auditable and fuzzable", and
//! these tests are exactly that fuzzing harness, wired into `cargo test`.
//!
//! A couple of tests bound the SHP width/height product before decoding.
//! `Shp::parse` itself is always safe (frame/table sizes are bounded by a
//! `u16` count), but `decode_frame` allocates a `width * height`-byte buffer
//! per frame from attacker-controlled `u16` fields — unbounded that's a
//! ~4 GiB allocation, which aborts the process (an allocator abort, not a
//! `panic!`) rather than failing a single test. Bounding it here is a test
//! harness precaution, not evidence the parser is safe against that input;
//! see the final report for this noted as a structural finding for ra-coder.

use proptest::prelude::*;

use ra_formats::codec::{apply_xor_delta, lcw_decompress};
use ra_formats::ini::Ini;
use ra_formats::mix::MixArchive;
use ra_formats::pack::{decode_base64, decompress_pack};
use ra_formats::shp::Shp;
use ra_formats::tmpl::Template;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn mix_parse_never_panics(data in proptest::collection::vec(any::<u8>(), 0..4096)) {
        // Entry counts are always read as u16, so a successful parse can
        // never demand more than 65535 * 12 bytes of index — bounded, no
        // special-casing needed here.
        if let Ok(mix) = MixArchive::parse(&data) {
            let _ = mix.data_start();
            for e in mix.entries() {
                // Must never panic even when offset/size point outside the
                // buffer; a corrupt entry yields `None`.
                let _ = mix.entry_bytes(e);
            }
        }
    }

    #[test]
    fn mix_nested_open_never_panics(data in proptest::collection::vec(any::<u8>(), 0..4096)) {
        if let Ok(mix) = MixArchive::parse(&data) {
            for e in mix.entries() {
                if let Some(bytes) = mix.entry_bytes(e) {
                    // Re-parsing arbitrary entry bytes as a nested archive
                    // must not panic either.
                    let _ = MixArchive::parse(bytes);
                }
            }
        }
    }

    #[test]
    fn shp_parse_never_panics(data in proptest::collection::vec(any::<u8>(), 0..4096)) {
        // Header + frame table parsing only ever allocates a `Vec` bounded
        // by a u16 frame count (<= 65535 * 8 bytes), so this is always safe.
        let _ = Shp::parse(&data);
    }

    #[test]
    fn shp_decode_bounded_never_panics(
        data in proptest::collection::vec(any::<u8>(), 14..4096),
        idx in 0usize..8,
    ) {
        if let Ok(shp) = Shp::parse(&data) {
            let hdr = shp.header();
            let pixel_count = hdr.width as usize * hdr.height as usize;
            // See module docs: bound the allocation ourselves rather than
            // asking the parser to.
            prop_assume!(pixel_count <= 1_000_000);
            if shp.frame_count() > 0 {
                let bounded_idx = idx % shp.frame_count();
                let _ = shp.decode_frame(bounded_idx);
                let _ = shp.decode_all();
            }
        }
    }

    #[test]
    fn shp_decode_out_of_range_is_err_not_panic(
        data in proptest::collection::vec(any::<u8>(), 14..512),
        idx in any::<usize>(),
    ) {
        // Out-of-range indices must be rejected before any allocation
        // happens, regardless of header width/height, so no bounding needed.
        if let Ok(shp) = Shp::parse(&data) {
            if idx >= shp.frame_count() {
                prop_assert!(shp.decode_frame(idx).is_err());
            }
        }
    }

    #[test]
    fn lcw_decompress_never_panics(
        src in proptest::collection::vec(any::<u8>(), 0..2048),
        out_len in 0usize..4096,
    ) {
        let mut out = vec![0u8; out_len];
        let written = lcw_decompress(&src, &mut out);
        prop_assert!(written <= out_len);
    }

    #[test]
    fn xor_delta_never_panics(
        dst_len in 0usize..4096,
        delta in proptest::collection::vec(any::<u8>(), 0..2048),
    ) {
        let mut dst = vec![0u8; dst_len];
        apply_xor_delta(&mut dst, &delta);
    }

    #[test]
    fn tmpl_parse_never_panics(data in proptest::collection::vec(any::<u8>(), 0..4096)) {
        // Header fields are bounded u16/u32 reads; `icon()` is only exercised
        // here through the safe getters (count/width/height), never with an
        // attacker-chosen index, since that's covered by the bounded test
        // below. `Template::parse` itself must never panic or allocate
        // beyond the input length (it copies `data` verbatim into `raw`).
        if let Ok(t) = Template::parse(&data) {
            let _ = t.width();
            let _ = t.height();
            let _ = t.count();
            let _ = t.color_map();
        }
    }

    #[test]
    fn tmpl_icon_lookup_never_panics(
        data in proptest::collection::vec(any::<u8>(), 0..4096),
        idx in 0usize..300,
    ) {
        // `icon()` indexes into `raw` via checked `get()` calls, so any index
        // (in- or out-of-range) must return `Option`, never panic.
        if let Ok(t) = Template::parse(&data) {
            let _ = t.icon(idx);
        }
    }

    #[test]
    fn base64_decode_never_panics(data in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let out = decode_base64(&data);
        // Every 6 input bits contributes at most... in the worst case (all
        // alphabet bytes) output is bounded by input length.
        prop_assert!(out.len() <= data.len());
    }

    #[test]
    fn decompress_pack_never_panics(data in proptest::collection::vec(any::<u8>(), 0..8192)) {
        let _ = decompress_pack(&data);
    }

    #[test]
    fn base64_then_pack_never_panics(data in proptest::collection::vec(any::<u8>(), 0..8192)) {
        // The exact pipeline `ra_data::scenario` runs over a scenario's
        // `[MapPack]`/`[OverlayPack]` text: base64-decode then chunked-LCW
        // decompress. Must never panic regardless of what garbage the
        // "base64" text actually contains.
        let packed = decode_base64(&data);
        let _ = decompress_pack(&packed);
    }

    #[test]
    fn ini_parse_never_panics(text in ".{0,4096}") {
        // Arbitrary (possibly non-ASCII, control-character-laden) text must
        // parse into *some* `Ini` without panicking; every lookup used below
        // is itself bounds-checked.
        let ini = Ini::parse(&text);
        let _ = ini.has_section("Map");
        let _ = ini.get("Map", "Theater");
        let _ = ini.get_int("Map", "X");
        let _ = ini.section_entries("MapPack");
        let _ = ini.concat_block("MapPack");
    }

    #[test]
    fn ini_parse_never_panics_on_raw_bytes(data in proptest::collection::vec(any::<u8>(), 0..4096)) {
        // Same property, but starting from arbitrary bytes lossily converted
        // to UTF-8 (the actual path `ra_client::assets` uses for MIX-sourced
        // INI text via `String::from_utf8_lossy`), so invalid UTF-8 sequences
        // and replacement characters are exercised too.
        let text = String::from_utf8_lossy(&data);
        let ini = Ini::parse(&text);
        let _ = ini.get_int("Basic", "NewINIFormat");
    }
}
