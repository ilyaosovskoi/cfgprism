//! Cross-format matrix: every `(from, to)` pair over shared sample documents.
//!
//! Rules under test (see `DECISIONS.md` D15):
//! - Every pair either converts cleanly or fails with a precise error
//!   (nesting into dotenv, deep nesting into INI); nothing silently drops.
//! - `values_equal` holds for all `Ok` pairs (order-sensitive), except
//!   TOML/INI targets on order-violating input (`values_equal_unordered` +
//!   `KeyReordered`). dotenv/INI targets stringify scalars (compared
//!   against the stringified expectation).
//! - Required warnings are asserted per pair class; a set of clean pairs
//!   must warn nothing at all.

use cfgprism_core::{
    values_equal, values_equal_unordered, Anchor, Doc, Entry, Key, Node, Number, NumberKind,
    Options, Trivia, Value, WarningKind,
};
use cfgprism_formats::{all_formats, convert_text};

const FORMATS: &[&str] = &["json", "jsonc", "json5", "toml", "yaml", "dotenv", "ini"];

fn opt() -> Options {
    Options::default()
}

fn scalar_entries() -> Vec<Entry> {
    vec![
        entry("z", Value::Number(num("1"))),
        entry("a", Value::Bool(true)),
        entry("m", Value::Str("hi".to_string())),
    ]
}

fn num(raw: &str) -> Number {
    Number {
        raw: raw.to_string(),
        kind: NumberKind::Int,
    }
}

fn entry(key: &str, value: Value) -> Entry {
    Entry {
        key: Key::plain(key.to_string()),
        key_trivia: Trivia::empty(),
        sep_raw: None,
        value: Node::new(value),
    }
}

fn commented(entries: Vec<Entry>) -> Vec<Entry> {
    entries
        .into_iter()
        .map(|mut e| {
            e.key_trivia.leading = vec![format!("# about {}", e.key.text)];
            if e.key.text == "z" {
                e.value.trivia.inline = Some("# inline z".to_string());
            }
            e
        })
        .collect()
}

/// Flat scalars, ordered z/a/m.
fn doc_bare() -> Doc {
    Doc::new(Node::new(Value::Map(scalar_entries())))
}

/// Flat scalars with comments on every entry.
fn doc_commented() -> Doc {
    Doc::new(Node::new(Value::Map(commented(scalar_entries()))))
}

/// Anchor definition + alias (values already cloned, as parsers produce).
fn doc_anchored() -> Doc {
    let mut def = Node::new(Value::Number(num("1")));
    def.anchor = Some(Anchor {
        name: "x".to_string(),
        is_alias: false,
    });
    let mut alias = Node::new(Value::Number(num("1")));
    alias.anchor = Some(Anchor {
        name: "x".to_string(),
        is_alias: true,
    });
    Doc::new(Node::new(Value::Map(vec![
        Entry {
            key: Key::plain("x".to_string()),
            key_trivia: Trivia::empty(),
            sep_raw: None,
            value: def,
        },
        Entry {
            key: Key::plain("y".to_string()),
            key_trivia: Trivia::empty(),
            sep_raw: None,
            value: alias,
        },
    ])))
}

/// Nested maps/arrays/empty containers.
fn doc_nested() -> Doc {
    Doc::new(Node::new(Value::Map(vec![
        entry("z", Value::Number(num("1"))),
        Entry {
            key: Key::plain("t".to_string()),
            key_trivia: Trivia::empty(),
            sep_raw: None,
            value: Node::new(Value::Map(vec![
                entry(
                    "a",
                    Value::Array(vec![
                        Node::new(Value::Number(num("1"))),
                        Node::new(Value::Number(num("2"))),
                    ]),
                ),
                entry("s", Value::Str("x".to_string())),
            ])),
        },
        entry("e", Value::Array(vec![])),
    ])))
}

/// Merge entry (`<<: *base`) over a base map.
fn doc_merge() -> Doc {
    let base = Node::new(Value::Map(vec![
        entry("timeout", Value::Number(num("30"))),
        entry("retries", Value::Number(num("3"))),
    ]));
    let mut alias = Node::new(Value::Map(vec![
        entry("timeout", Value::Number(num("30"))),
        entry("retries", Value::Number(num("3"))),
    ]));
    alias.anchor = Some(Anchor {
        name: "base".to_string(),
        is_alias: true,
    });
    Doc::new(Node::new(Value::Map(vec![
        Entry {
            key: Key::plain("base".to_string()),
            key_trivia: Trivia::empty(),
            sep_raw: None,
            value: base,
        },
        Entry {
            key: Key::plain("svc".to_string()),
            key_trivia: Trivia::empty(),
            sep_raw: None,
            value: Node::new(Value::Map(vec![
                Entry {
                    key: Key::plain("<<".to_string()),
                    key_trivia: Trivia::empty(),
                    sep_raw: None,
                    value: alias,
                },
                entry("name", Value::Str("api".to_string())),
            ])),
        },
    ])))
}

/// Table-before-value (forces TOML/INI reordering).
fn doc_reorder() -> Doc {
    Doc::new(Node::new(Value::Map(vec![
        Entry {
            key: Key::plain("t".to_string()),
            key_trivia: Trivia::empty(),
            sep_raw: None,
            value: Node::new(Value::Map(vec![entry("x", Value::Number(num("1")))])),
        },
        entry("v", Value::Number(num("2"))),
    ])))
}

/// Stringify all scalars (dotenv/INI target expectation). Node metadata
/// (anchors for merge detection, order, trivia) is preserved; only value
/// kinds are mapped.
fn stringified(node: &Node) -> Node {
    let value = match &node.value {
        Value::Null => Value::Str(String::new()),
        Value::Bool(b) => Value::Str(b.to_string()),
        Value::Number(n) => Value::Str(n.raw.clone()),
        Value::Str(s) => Value::Str(s.clone()),
        Value::Datetime(d) => Value::Str(d.clone()),
        Value::Array(items) => Value::Array(items.iter().map(stringified).collect()),
        Value::Map(entries) => Value::Map(
            entries
                .iter()
                .map(|e| Entry {
                    key: e.key.clone(),
                    key_trivia: e.key_trivia.clone(),
                    sep_raw: e.sep_raw.clone(),
                    value: stringified(&e.value),
                })
                .collect(),
        ),
        _ => Value::Null,
    };
    let mut out = Node::new(value);
    out.anchor = node.anchor.clone();
    out.order = node.order;
    out.trivia = node.trivia.clone();
    out.style = node.style.clone();
    out
}

/// Render `doc` into `fmt` via the logical emitter (canonical input text).
fn render(fmt_name: &str, doc: &Doc) -> String {
    let registry = all_formats();
    let fmt = registry.find(fmt_name).expect("registered");
    fmt.emit_logical(doc, &opt()).expect("render").text
}

fn has_kind(warnings: &[cfgprism_core::Warning], kind: &WarningKind) -> bool {
    warnings
        .iter()
        .any(|w| std::mem::discriminant(&w.kind) == std::mem::discriminant(kind))
}

/// (doc-name, target) pairs that must FAIL with a precise error:
/// dotenv takes only flat maps; INI takes at most one section level and no
/// arrays. Everything else converts.
fn must_fail(doc: &str, to: &str) -> bool {
    match to {
        "dotenv" => matches!(doc, "nested" | "reorder" | "merge"),
        "ini" => doc == "nested",
        _ => false,
    }
}

#[test]
fn matrix_all_pairs() {
    let docs: &[(&str, Doc)] = &[
        ("bare", doc_bare()),
        ("commented", doc_commented()),
        ("anchored", doc_anchored()),
        ("nested", doc_nested()),
        ("merge", doc_merge()),
        ("reorder", doc_reorder()),
    ];
    let mut count = 0;
    for (doc_name, doc) in docs {
        for from in FORMATS {
            // dotenv cannot even *render* nested input; skip generation.
            let src = match render_checked(from, doc) {
                Some(s) => s,
                None => continue,
            };
            for to in FORMATS {
                let res = convert_text(from, to, &src, &opt());
                if must_fail(doc_name, to) {
                    assert!(res.is_err(), "{from}->{to} on {doc_name} must fail, got Ok");
                    continue;
                }
                let out = res.unwrap_or_else(|e| panic!("{from}->{to} on {doc_name} failed: {e}"));
                // Re-parse with the target format and compare IR.
                let registry = all_formats();
                let reparsed = registry
                    .find(to)
                    .expect("registered")
                    .parse(&out.text)
                    .unwrap_or_else(|e| {
                        panic!("re-parse {to} failed ({from}->{to} {doc_name}): {e}")
                    });
                // Untyped formats (dotenv/INI) stringify on *both* ends:
                // their inputs already lost types at render time.
                let untyped =
                    *from == "dotenv" || *from == "ini" || *to == "dotenv" || *to == "ini";
                let mut expect = if untyped {
                    stringified(&doc.root)
                } else {
                    doc.root.clone()
                };
                // `<<` markers never survive logical rendering (inputs are
                // rendered logically): expand for comparison. Duplicates are
                // kept (none exist in the samples); strict targets compare
                // unordered below.
                let expanded = match &expect.value {
                    Value::Map(entries) => Some(cfgprism_formats::logical::expand_entries(
                        entries,
                        "",
                        false,
                        &mut Vec::new(),
                    )),
                    _ => None,
                };
                if let Some(flat) = expanded {
                    expect = Node::new(Value::Map(flat));
                }
                // TOML/INI legitimately reorder (values before tables); once
                // reordered anywhere in the chain, compare unordered.
                let reorder_prone =
                    *from == "toml" || *from == "ini" || *to == "toml" || *to == "ini";
                if reorder_prone {
                    assert!(
                        values_equal_unordered(&expect, &reparsed.root),
                        "{from}->{to} on {doc_name}: unordered mismatch"
                    );
                } else {
                    assert!(
                        values_equal(&expect, &reparsed.root),
                        "{from}->{to} on {doc_name}: mismatch"
                    );
                }
                assert_no_marker_leak(to, &out.text, doc_name, from);
                count += 1;
            }
        }
    }
    assert!(count > 150, "matrix too small: {count}");
    println!("matrix pairs passed: {count}");
}

/// `<<` markers and anchor syntax must never leak into logical output.
/// (Same-format pairs are verbatim by design and keep everything.)
fn assert_no_marker_leak(to: &str, text: &str, doc: &str, from: &str) {
    if from == to {
        return;
    }
    if doc == "merge" || doc == "anchored" {
        assert!(
            !text.contains("<<"),
            "{from}->{to}: merge marker leaked into output"
        );
        if to == "json" || to == "jsonc" || to == "json5" || to == "toml" {
            assert!(
                !text.contains('*'),
                "{from}->{to}: alias marker leaked into output"
            );
        }
    }
}
/// Render input, returning None when the source format cannot represent it
/// (flat-only dotenv, no-array/shallow INI).
fn render_checked(from: &str, doc: &Doc) -> Option<String> {
    let registry = all_formats();
    registry
        .find(from)?
        .emit_logical(doc, &opt())
        .ok()
        .map(|o| o.text)
}

#[test]
fn matrix_required_warnings() {
    // Comments into strict JSON must warn; comment-preserving targets must not.
    let commented_src = render("yaml", &doc_commented());
    let out = convert_text("yaml", "json", &commented_src, &opt()).expect("convert");
    assert!(has_kind(&out.warnings, &WarningKind::CommentDropped));
    for to in ["jsonc", "json5", "toml", "yaml", "dotenv", "ini"] {
        let out = convert_text("yaml", to, &commented_src, &opt())
            .unwrap_or_else(|e| panic!("yaml->{to} failed: {e}"));
        assert!(
            !has_kind(&out.warnings, &WarningKind::CommentDropped),
            "yaml->{to} wrongly dropped comments"
        );
    }
    // Anchors expand with warnings everywhere except same-format verbatim.
    // NOTE: the input must carry real anchor *syntax* (logical rendering
    // expands eagerly), so it is literal YAML here, not render() output.
    let anchored_src = "x: &x 1\ny: *x\n";
    for to in ["json", "toml", "dotenv", "ini"] {
        let out = convert_text("yaml", to, anchored_src, &opt())
            .unwrap_or_else(|e| panic!("yaml->{to} failed: {e}"));
        assert!(
            has_kind(&out.warnings, &WarningKind::AnchorExpanded),
            "yaml->{to} missing AnchorExpanded"
        );
    }
    // Same-format yaml stays verbatim: anchors kept, silence.
    let out = convert_text("yaml", "yaml", anchored_src, &opt()).expect("yaml->yaml");
    assert!(out.warnings.is_empty());
    assert!(out.text.contains("*x"), "alias lost in verbatim");
    // Reordering warns on TOML/INI.
    let reorder_src = render("json", &doc_reorder());
    for to in ["toml", "ini"] {
        let out = convert_text("json", to, &reorder_src, &opt())
            .unwrap_or_else(|e| panic!("json->{to} failed: {e}"));
        assert!(
            has_kind(&out.warnings, &WarningKind::KeyReordered),
            "json->{to} missing KeyReordered"
        );
    }
    // Clean pairs warn nothing.
    let bare_src = render("json", &doc_bare());
    for to in FORMATS {
        let out = convert_text("json", to, &bare_src, &opt())
            .unwrap_or_else(|e| panic!("json->{to} failed: {e}"));
        assert!(
            out.warnings.is_empty(),
            "json->{to} warned: {:?}",
            out.warnings
        );
    }
}

#[test]
fn matrix_datetime_and_null() {
    let dt = Doc::new(Node::new(Value::Map(vec![entry(
        "d",
        Value::Datetime("1979-05-27T07:32:00Z".to_string()),
    )])));
    let src = render("toml", &dt);
    // JSON family: coerced to string with a warning.
    for to in ["json", "jsonc", "json5"] {
        let out = convert_text("toml", to, &src, &opt())
            .unwrap_or_else(|e| panic!("toml->{to} failed: {e}"));
        assert!(
            has_kind(&out.warnings, &WarningKind::TypeCoerced),
            "toml->{to}"
        );
        assert!(out.text.contains("\"1979-05-27T07:32:00Z\""), "toml->{to}");
    }
    // TOML keeps it silently; dotenv/INI stringify silently.
    for to in ["toml", "dotenv", "ini"] {
        let out = convert_text("toml", to, &src, &opt())
            .unwrap_or_else(|e| panic!("toml->{to} failed: {e}"));
        assert!(out.warnings.is_empty(), "toml->{to}: {:?}", out.warnings);
    }
    // Null has no TOML representation: precise error, not silent loss.
    let null = Doc::new(Node::new(Value::Map(vec![entry("n", Value::Null)])));
    let nsrc = render("json", &null);
    assert!(convert_text("json", "toml", &nsrc, &opt()).is_err());
    // ...but YAML/JSON round-trip it natively.
    for to in ["json", "yaml"] {
        let out = convert_text("json", to, &nsrc, &opt())
            .unwrap_or_else(|e| panic!("json->{to} failed: {e}"));
        assert!(out.warnings.is_empty());
    }
}
