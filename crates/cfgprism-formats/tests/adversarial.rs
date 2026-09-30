//! Adversarial robustness: hostile inputs must yield `Err`, never a panic,
//! hang or abort. Deterministic (fixed corpus) so it runs on stable Rust;
//! randomized fuzzing lives in `fuzz/` (cargo-fuzz, nightly CI).
//!
//! The corpus covers: deep nesting, huge scalars, truncated documents,
//! hostile bytes/UTF-8 edges, pathological comments and anchors.

use cfgprism_core::Options;
use cfgprism_formats::all_formats;

fn no_panic(from: &str, src: &str) {
    let registry = all_formats();
    let fmt = registry.find(from).expect("registered");
    // Parse must not panic; on success, emit + re-parse must not panic either.
    let doc = match fmt.parse(src) {
        Ok(d) => d,
        Err(e) => {
            assert!(!e.to_string().is_empty());
            assert!(e.pos.line >= 1 && e.pos.col >= 1);
            return;
        }
    };
    if let Ok(out) = fmt.emit(&doc, &Options::default()) {
        let _ = fmt.parse(&out.text);
    }
    // Logical emission must not panic either (errors are fine).
    let _ = fmt.emit_logical(&doc, &Options::default());
}

#[test]
fn adversarial_json() {
    let cases = [
        "[".repeat(10_000),
        "{".repeat(10_000),
        format!("{}1{}", "[".repeat(500), "]".repeat(500)),
        format!("{{\"a\":{{\"b\":{}}}}}", "[".repeat(0)),
        "\"".to_string() + &"a".repeat(1_000_000) + "\"",
        "\"\\uD800\"".to_string(),
        "\"\\uD800\\uD800\"".to_string(),
        "\"\\uDC00\"".to_string(),
        "\"unterminated".to_string(),
        "[1,".to_string(),
        "{\"a\":".to_string(),
        "[01]".to_string(),
        "[+1]".to_string(),
        "[.5]".to_string(),
        "[5.]".to_string(),
        "[NaN]".to_string(),
        "[Infinity]".to_string(),
        "nul".to_string(),
        "\u{FEFF}{\"a\": 1}".to_string(),
        "{}\u{FEFF}".to_string(),
    ];
    for (i, src) in cases.iter().enumerate() {
        no_panic("json", src);
        no_panic("jsonc", src);
        no_panic("json5", src);
        let _ = i;
    }
    // Comment bombs for the comment dialects.
    for src in [
        "/*".to_string() + &"a".repeat(100_000),
        "//".to_string() + &"b".repeat(100_000),
        "{/*".to_string() + &"c".repeat(100_000),
    ] {
        no_panic("jsonc", &src);
        no_panic("json5", &src);
    }
}

#[test]
fn adversarial_toml() {
    let cases = [
        "[".repeat(10_000),
        "a = ".to_string() + &"[1,".repeat(5_000),
        "a = \"".to_string() + &"x".repeat(1_000_000),
        "a = ".to_string(),
        "[unclosed".to_string(),
        "[[[[a]]]]".to_string(),
        "a.b.c.d.e = 1\n".repeat(2_000),
        "a = 0x".to_string(),
        "a = 1979-13-45T99:99:99Z".to_string(),
    ];
    for src in &cases {
        no_panic("toml", src);
    }
}

#[test]
fn adversarial_yaml() {
    let deep = (0..500).map(|i| format!("k{i}:\n  ")).collect::<String>() + "1\n";
    let cases = [
        deep,
        ": \n".repeat(10_000),
        "- ".to_string() + &"- ".repeat(5_000),
        "&a ".to_string() + &"[1, ".repeat(2_000),
        "a: \"".to_string() + &"x".repeat(1_000_000),
        "a: |\n".to_string() + &"  x\n".repeat(100_000),
        "\tkey: value\n".to_string(),
        "a: 1\n---\n".to_string() + &"b: 2\n---\n".repeat(1_000),
        "a: !".to_string() + &"x".repeat(100_000),
        "a: &".to_string() + &"x".repeat(100_000),
        "*undefined\n".to_string(),
        "a: *undefined\n".to_string(),
    ];
    for src in &cases {
        no_panic("yaml", src);
    }
}

#[test]
fn adversarial_dotenv_ini() {
    let cases = [
        "A=".to_string() + &"x".repeat(1_000_000),
        "A=\"".to_string() + &"y".repeat(500_000),
        "=".repeat(100_000),
        "[sec]\n".to_string() + &"k = v\n".repeat(50_000),
        "[".to_string() + &"s".repeat(100_000),
        "k = ".to_string() + &"v ".repeat(100_000),
    ];
    for src in &cases {
        no_panic("dotenv", src);
        no_panic("ini", src);
    }
}
