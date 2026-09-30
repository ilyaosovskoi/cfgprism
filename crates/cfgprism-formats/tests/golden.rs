//! Golden-file tests: every `tests/fixtures/<fmt>/*.in` must convert to
//! `*.out` through the matching format with default options.
//!
//! For self round-trip fixtures `.out` is byte-identical to `.in`; the
//! format set grows every stage (Stage 4 adds cross-format pairs).

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

    let mut count = 0;
    let mut fmt_dirs: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("read fixtures")
        .map(|e| e.expect("dir entry").path())
        .filter(|p| p.is_dir())
        .collect();
    fmt_dirs.sort();
    assert!(!fmt_dirs.is_empty(), "no format dirs found");

    for fmt_dir in fmt_dirs {
        let fmt_name = fmt_dir
            .file_name()
            .expect("dir name")
            .to_string_lossy()
            .to_string();
        let Some(fmt) = registry.find(&fmt_name) else {
            panic!("fixture dir for unregistered format: {fmt_name}");
        };
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&fmt_dir)
            .expect("read format fixtures")
            .map(|e| e.expect("dir entry").path())
            .filter(|p| p.extension().is_some_and(|e| e == "in"))
            .collect();
        entries.sort();
        assert!(!entries.is_empty(), "no .in fixtures in {fmt_name}");
        for input in entries {
            let expected = input.with_extension("out");
            assert!(expected.is_file(), "missing {}", expected.display());
            let src = std::fs::read_to_string(&input).expect("read .in");
            let want = std::fs::read_to_string(&expected).expect("read .out");
            let doc = fmt.parse(&src).expect("parse .in");
            let out = fmt.emit(&doc, &Options::default()).expect("emit");
            assert_eq!(out.text, want, "mismatch for {}", input.display());
            assert!(out.warnings.is_empty(), "warnings for {}", input.display());
            // Re-parse stability: parse → emit → parse must agree.
            let doc2 = fmt.parse(&out.text).expect("re-parse");
            assert!(
                cfgprism_core::values_equal(&doc.root, &doc2.root),
                "unstable IR for {}",
                input.display()
            );
            count += 1;
        }
    }
    assert!(count >= 6, "expected at least 6 fixtures, got {count}");
    println!("golden fixtures passed: {count}");
}
