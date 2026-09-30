//! Fuzz the YAML parser/adapter. See `fuzz_json.rs` for runner docs.
//!
//! Note: YAML event collection runs on a 256 MiB worker thread, so deep
//! inputs exercise the builder's depth guard (64) rather than the fuzzer
//! thread's stack.

#![no_main]

use cfgprism_core::{Format, Options};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if text.len() > 64 * 1024 {
        return;
    }
    let fmt = cfgprism_formats::YamlFormat;
    let Ok(doc) = fmt.parse(text) else { return };
    let Ok(out) = fmt.emit(&doc, &Options::default()) else {
        return;
    };
    assert!(
        fmt.parse(&out.text).is_ok(),
        "unstable round-trip: {text:?} -> {:?}",
        out.text
    );
});
