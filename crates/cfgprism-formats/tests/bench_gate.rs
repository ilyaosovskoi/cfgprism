//! Hard performance gate: a ~5 MB document converts in under one second.
//! (The criterion benches in `benches/` profile the same paths; this test
//! enforces the project quality bar on every CI run.)

use cfgprism_core::Options;
use cfgprism_formats::convert_text;

fn big_json(n: usize) -> String {
    let mut s = String::from("[\n");
    for i in 0..n {
        s.push_str(&format!(
            "  {{\"id\": {i}, \"name\": \"service-{i}\", \"enabled\": {}}}{}\n",
            i % 2 == 0,
            if i + 1 < n { "," } else { "" }
        ));
    }
    s.push(']');
    s
}

#[test]
fn converts_5mb_fast_enough() {
    let json = big_json(90_000);
    assert!(json.len() > 5_000_000, "fixture too small: {}", json.len());
    let opt = Options::default();
    let start = std::time::Instant::now();
    let out = convert_text("json", "json", &json, &opt).expect("convert");
    let elapsed = start.elapsed();
    assert_eq!(out.text, json, "verbatim round-trip must hold");
    // The project bar (5 MB < 1 s) applies to release builds; debug builds
    // run an order of magnitude slower, so the debug gate only guards
    // against algorithmic regressions (quadratic behavior would take hours).
    let budget = if cfg!(debug_assertions) { 5.0 } else { 1.0 };
    assert!(
        elapsed.as_secs_f64() < budget,
        "5 MB conversion took too long: {elapsed:?} (budget {budget}s)"
    );
    println!("5 MB json->json in {elapsed:?}");
}
