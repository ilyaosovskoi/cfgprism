//! Conversion benchmarks: a ~5 MB document per format must convert in
//! well under one second (project quality bar).
//!
//! Run: `cargo bench -p cfgprism-formats` (criterion reports in
//! `target/criterion`). CI asserts the bar via the `bench-quick` check
//! below running a single measured iteration? No — benches are
//! informational in CI; the hard gate is the `converts_5mb_fast_enough`
//! unit test in `tests/bench_gate.rs`.

use cfgprism_core::Options;
use cfgprism_formats::convert_text;
use criterion::{criterion_group, criterion_main, Criterion};
use std::hint::black_box;

/// Deterministic ~5 MB JSON array of flat objects with comments-free content.
fn big_json(n: usize) -> String {
    let mut s = String::from("[\n");
    for i in 0..n {
        s.push_str(&format!(
            "  {{\"id\": {i}, \"name\": \"service-{i}\", \"enabled\": {}, \"tags\": [\"a\", \"b\"]}}{}\n",
            i % 2 == 0,
            if i + 1 < n { "," } else { "" }
        ));
    }
    s.push(']');
    s
}

fn big_toml(n: usize) -> String {
    let mut s = String::from("# generated\n");
    for i in 0..n {
        s.push_str(&format!(
            "[service{i}]\nid = {i}\nname = \"service-{i}\"\nenabled = {}\n",
            i % 2 == 0
        ));
    }
    s
}

fn big_yaml(n: usize) -> String {
    let mut s = String::from("# generated\n");
    for i in 0..n {
        s.push_str(&format!(
            "service{i}:\n  id: {i}\n  name: service-{i}\n  enabled: {}\n",
            i % 2 == 0
        ));
    }
    s
}

fn bench_convert(c: &mut Criterion) {
    let opt = Options::default();
    // Calibrated so each document is ~5 MB.
    let json = big_json(65_000);
    let toml = big_toml(80_000);
    let yaml = big_yaml(80_000);
    eprintln!(
        "sizes: json={} toml={} yaml={}",
        json.len(),
        toml.len(),
        yaml.len()
    );
    assert!(json.len() > 4_500_000, "json fixture too small");
    let mut group = c.benchmark_group("convert-5mb");
    group.sample_size(10);
    group.bench_function("json-to-json", |b| {
        b.iter(|| convert_text("json", "json", black_box(&json), &opt).expect("convert"))
    });
    group.bench_function("json-to-yaml", |b| {
        b.iter(|| convert_text("json", "yaml", black_box(&json), &opt).expect("convert"))
    });
    group.bench_function("toml-to-toml", |b| {
        b.iter(|| convert_text("toml", "toml", black_box(&toml), &opt).expect("convert"))
    });
    group.bench_function("toml-to-json", |b| {
        b.iter(|| convert_text("toml", "json", black_box(&toml), &opt).expect("convert"))
    });
    group.bench_function("yaml-to-yaml", |b| {
        b.iter(|| convert_text("yaml", "yaml", black_box(&yaml), &opt).expect("convert"))
    });
    group.bench_function("yaml-to-json", |b| {
        b.iter(|| convert_text("yaml", "json", black_box(&yaml), &opt).expect("convert"))
    });
    group.finish();
}

criterion_group!(benches, bench_convert);
criterion_main!(benches);
