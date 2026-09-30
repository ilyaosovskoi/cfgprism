//! Golden-file test: every `tests/fixtures/<fmt>/*.in` must convert to `*.out`.
//!
//! Stage 1: only `json/basic` exists and goes through the JSON stub with
//! default options. Later stages extend the walker (target format from the
//! fixture name, warnings assertion sidecar files).

use std::path::PathBuf;

use cfgprism_core::Options;
use cfgprism_formats::all_formats;

fn fixtures_dir() -> PathBuf {
    // crates/cfgprism-formats -> workspace root -> tests/fixtures
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
}

#[test]
fn golden_files_match() {
    let dir = fixtures_dir();
    assert!(dir.is_dir(), "missing fixtures dir: {}", dir.display());
    let registry = all_formats();
    let json = registry.find("json").expect("json registered");

    let mut count = 0;
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir.join("json"))
        .expect("read json fixtures")
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "in"))
        .collect();
    entries.sort();
    assert!(!entries.is_empty(), "no .in fixtures found");

    for input in entries {
        let expected = input.with_extension("out");
        assert!(expected.is_file(), "missing {}", expected.display());
        let src = std::fs::read_to_string(&input).expect("read .in");
        let want = std::fs::read_to_string(&expected).expect("read .out");
        let doc = json.parse(&src).expect("parse .in");
        // Order must survive parse (stage-1 invariant).
        if input.file_stem().is_some_and(|s| s == "basic") {
            assert_eq!(
                doc.root_keys_in_order(),
                vec!["z".to_string(), "a".to_string(), "m".to_string()]
            );
        }
        let out = json.emit(&doc, &Options::default()).expect("emit");
        assert_eq!(out.text, want, "mismatch for {}", input.display());
        assert!(out.warnings.is_empty());
        count += 1;
    }
    assert!(count >= 1);
}
