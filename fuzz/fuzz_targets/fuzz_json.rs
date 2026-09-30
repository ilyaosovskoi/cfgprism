//! Fuzz the JSON-family parsers: no panics/hangs, and emit output
//! re-parses (round-trip stability). Run with cargo-fuzz (nightly):
//! `cargo +nightly fuzz run fuzz_json -- -max_total_time=120`.

#![no_main]

use cfgprism_core::{Format, Options};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    // Cap input size: the depth guard (64) bounds stacks, but sponge inputs
    // waste fuzzer time.
    if text.len() > 64 * 1024 {
        return;
    }
    let formats: [&dyn Format; 3] = [
        &cfgprism_formats::JsonFormat,
        &cfgprism_formats::JsoncFormat,
        &cfgprism_formats::Json5Format,
    ];
    for fmt in formats {
        let Ok(doc) = fmt.parse(text) else { continue };
        let Ok(out) = fmt.emit(&doc, &Options::default()) else {
            continue;
        };
        // Stability: well-formed output must re-parse. Any failure here is
        // an emitter bug worth a regression fixture.
        assert!(
            fmt.parse(&out.text).is_ok(),
            "unstable round-trip for {}: {text:?} -> {:?}",
            fmt.name(),
            out.text
        );
    }
});
