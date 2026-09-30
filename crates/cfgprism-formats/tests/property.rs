//! Property tests: `parse → emit → parse` yields an equivalent IR and
//! emission is idempotent. Inputs are programmatically built documents (no
//! verbatim slices), so these also cover every emitter's canonical fallback.
//! Deterministic xorshift PRNG — no extra dependencies.

use cfgprism_core::{values_equal, Format};
use cfgprism_core::{Doc, Entry, Key, Node, Number, NumberKind, Options, Style, Trivia, Value};
use cfgprism_formats::{DotenvFormat, IniFormat, JsonFormat, TomlFormat};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % (n as u64)) as usize
    }

    fn pick<'a>(&mut self, opts: &'a [&'a str]) -> &'a str {
        opts[self.below(opts.len())]
    }
}

fn node_order(n: &mut Node, counter: &mut usize) {
    n.order = *counter;
    *counter += 1;
    match &mut n.value {
        Value::Array(items) => {
            for i in items {
                node_order(i, counter);
            }
        }
        Value::Map(entries) => {
            for e in entries {
                node_order(&mut e.value, counter);
            }
        }
        _ => {}
    }
}

fn json_value(rng: &mut Rng, depth: usize) -> Value {
    const STRS: &[&str] = &[
        "",
        "a",
        "hello world",
        "quote\"q",
        "back\\slash",
        "tab\tnl\ncr\r",
        "unicode-é-ß-中",
        "# not a comment",
        "trailing ",
        " leading",
    ];
    let leaf = depth == 0;
    match if leaf { rng.below(5) } else { rng.below(7) } {
        0 => Value::Null,
        1 => Value::Bool(rng.below(2) == 0),
        2 => {
            let raw = rng.pick(&["0", "1", "-42", "3.14", "-0.5", "1e3", "1000000"]);
            Value::Number(Number {
                raw: raw.to_string(),
                kind: if raw.contains(['.', 'e']) {
                    NumberKind::Float
                } else {
                    NumberKind::Int
                },
            })
        }
        3 => Value::Str(rng.pick(STRS).to_string()),
        4 => {
            let n = rng.below(4);
            Value::Array(
                (0..n)
                    .map(|_| Node::new(json_value(rng, depth.saturating_sub(1))))
                    .collect(),
            )
        }
        _ => {
            let n = rng.below(4);
            Value::Map(
                (0..n)
                    .map(|i| Entry {
                        key: Key::plain(format!("k{i}")),
                        key_trivia: Trivia::empty(),
                        sep_raw: None,
                        value: Node::new(json_value(rng, depth.saturating_sub(1))),
                    })
                    .collect(),
            )
        }
    }
}

fn check_stable(fmt: &dyn Format, doc: &Doc) {
    let opt = Options::default();
    let out1 = fmt.emit(doc, &opt).expect("emit 1");
    let doc2 = fmt.parse(&out1.text).expect("re-parse");
    assert!(
        values_equal(&doc.root, &doc2.root),
        "IR changed across round-trip for {}",
        fmt.name()
    );
    let out2 = fmt.emit(&doc2, &opt).expect("emit 2");
    assert_eq!(
        out1.text,
        out2.text,
        "emit not idempotent for {}",
        fmt.name()
    );
}

#[test]
fn json_parse_emit_parse_is_stable() {
    let fmt = JsonFormat;
    let mut rng = Rng(0x1234_5678_9abc_def0);
    for _ in 0..200 {
        let mut root = Node::new(json_value(&mut rng, 4));
        let mut c = 0;
        node_order(&mut root, &mut c);
        check_stable(&fmt, &Doc::new(root));
    }
}

#[test]
fn dotenv_parse_emit_parse_is_stable() {
    let fmt = DotenvFormat;
    let mut rng = Rng(0xdead_beef_cafe_f00d);
    const KEYS: &[&str] = &["A", "FOO", "x1", "_p", "LONGKEYNAME"];
    const VALS: &[&str] = &[
        "",
        "1",
        "abc",
        "hello world",
        "a#b",
        "q\"q",
        "it's",
        "a=b",
        " spaced ",
    ];
    for _ in 0..200 {
        let n = rng.below(5);
        let entries: Vec<Entry> = (0..n)
            .map(|i| {
                let v = rng.pick(VALS);
                // Quote when the canonical `key=value` form would not survive
                // (the quoted raw also exercises the verbatim path).
                let raw = if v.contains([' ', '#', '"', '\'', '=']) {
                    format!("\"{v}\"")
                } else {
                    v.to_string()
                };
                let mut vn = Node::new(Value::Str(v.to_string()));
                vn.style = Style::Original(raw);
                Entry {
                    key: Key::plain(KEYS[(i + rng.below(KEYS.len())) % KEYS.len()].to_string()),
                    key_trivia: Trivia::empty(),
                    sep_raw: Some("=".to_string()),
                    value: vn,
                }
            })
            .collect();
        // Deduplicate keys to keep IR comparison meaningful.
        let mut seen = std::collections::HashSet::new();
        let entries: Vec<Entry> = entries
            .into_iter()
            .filter(|e| seen.insert(e.key.text.clone()))
            .collect();
        check_stable(&fmt, &Doc::new(Node::new(Value::Map(entries))));
    }
}

#[test]
fn toml_parse_emit_parse_is_stable() {
    let fmt = TomlFormat;
    let mut rng = Rng(0x0bad_c0de_5eed);
    const SAFE_STRS: &[&str] = &["", "abc", "hello world", "x y z", " under "];
    for _ in 0..200 {
        let scalar = |rng: &mut Rng| -> Node {
            match rng.below(5) {
                0 => Node::new(Value::Number(Number {
                    raw: rng.pick(&["0", "7", "-3", "2.5", "-0.25"]).to_string(),
                    kind: NumberKind::Int,
                })),
                1 => Node::new(Value::Bool(rng.below(2) == 0)),
                2 => Node::new(Value::Str(rng.pick(SAFE_STRS).to_string())),
                3 => Node::new(Value::Datetime("1979-05-27T07:32:00Z".to_string())),
                _ => Node::new(Value::Array(
                    (0..rng.below(3))
                        .map(|_| {
                            Node::new(Value::Number(Number {
                                raw: rng.below(100).to_string(),
                                kind: NumberKind::Int,
                            }))
                        })
                        .collect(),
                )),
            }
        };
        // Scalars first (TOML order discipline), then subtables with headers.
        let mut entries: Vec<Entry> = (0..rng.below(4))
            .map(|i| Entry {
                key: Key::plain(format!("v{i}")),
                key_trivia: Trivia::empty(),
                sep_raw: None,
                value: scalar(&mut rng),
            })
            .collect();
        for i in 0..rng.below(3) {
            let children: Vec<Entry> = (0..rng.below(3))
                .map(|j| Entry {
                    key: Key::plain(format!("f{j}")),
                    key_trivia: Trivia::empty(),
                    sep_raw: None,
                    value: scalar(&mut rng),
                })
                .collect();
            let mut map = Node::new(Value::Map(children));
            map.open_raw = Some(format!("[t{i}]"));
            entries.push(Entry {
                key: Key::plain(format!("t{i}")),
                key_trivia: Trivia::empty(),
                sep_raw: None,
                value: map,
            });
        }
        let mut root = Node::new(Value::Map(entries));
        let mut c = 0;
        node_order(&mut root, &mut c);
        check_stable(&fmt, &Doc::new(root));
    }
}

#[test]
fn ini_parse_emit_parse_is_stable() {
    let fmt = IniFormat;
    let mut rng = Rng(0xfeed_face_1234);
    const SAFE: &[&str] = &["", "abc", "hello world", "x"];
    // Note: INI logical values trim surrounding whitespace (like
    // configparser), so edge spaces are verbatim-stable but not
    // IR-stable; the generator avoids them.
    for _ in 0..200 {
        let kv = |rng: &mut Rng, i: usize| {
            let text = rng.pick(SAFE).to_string();
            let mut vn = Node::new(Value::Str(text.clone()));
            vn.style = Style::Original(text);
            Entry {
                key: Key {
                    text: format!("k{i}"),
                    repr: None,
                    raw: Some(format!("k{i}")),
                },
                key_trivia: Trivia::empty(),
                sep_raw: Some(" = ".to_string()),
                value: vn,
            }
        };
        let mut entries: Vec<Entry> = (0..rng.below(3)).map(|i| kv(&mut rng, i)).collect();
        for i in 0..rng.below(3) {
            let children: Vec<Entry> = (0..rng.below(3)).map(|j| kv(&mut rng, j)).collect();
            entries.push(Entry {
                key: Key::plain(format!("s{i}")),
                key_trivia: Trivia::empty(),
                sep_raw: None,
                value: Node::new(Value::Map(children)),
            });
        }
        let mut root = Node::new(Value::Map(entries));
        let mut c = 0;
        node_order(&mut root, &mut c);
        check_stable(&fmt, &Doc::new(root));
    }
}
