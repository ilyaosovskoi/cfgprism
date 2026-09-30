//! KDL (v2) via the `kdl` crate: document-oriented parsing with trivia.
//!
//! Mapping (documented, reversible): each document node becomes a root-map
//! entry keyed by node name; the value is `Null` for bare nodes (`debug;`),
//! the scalar for single-argument nodes (`a 1` → `{"a": 1}`), otherwise a
//! Map with `"args": […]` (positional, when present), properties inline,
//! and `"children": {…}` (when present).
//!
//! A property literally named `args`/`children` alongside positional args
//! or child nodes is an explicit `UnsupportedConstruct` error (no silent
//! collision). `(type)` annotations are rejected the same way (outside the
//! stage-5 subset); v1 documents are retried with the v1 parser.
//!
//! Round-trip is canonical, not byte-verbatim: values, comments (leading
//! trivia + same-line remainders via spans) and key order survive; layout
//! is normalized. Slashdashed (`/-`) lines travel as comments.

use cfgprism_core::{
    offset_to_line_col, Doc, EmitOutput, Entry, Error, Format, Key, Node, Number, NumberKind,
    Options, Trivia, Value, Warning, WarningKind,
};

/// KDL format (`*.kdl`, v2 with v1 fallback).
pub struct KdlFormat;

const MAX_DEPTH: usize = 64;

/// Reserved mapping keys (see module docs).
const ARGS_KEY: &str = "args";
const CHILDREN_KEY: &str = "children";

fn kdl_error(src: &str, e: kdl::KdlError) -> Error {
    let first = e.diagnostics.first();
    let message = first
        .and_then(|d| d.message.clone())
        .unwrap_or_else(|| "invalid KDL".to_string());
    match first {
        Some(d) => {
            // Diagnostic offsets count chars; translate to bytes.
            let byte = src
                .char_indices()
                .nth(d.span.offset())
                .map(|(i, _)| i)
                .unwrap_or(src.len());
            let lc = offset_to_line_col(src, byte.min(src.len()));
            Error::parse(lc.line, lc.col, message)
        }
        None => Error::parse(1, 1, message),
    }
}

/// Logical `//`/`/* */` comments in a trivia slice (slashdash lines count
/// as comments: their text is preserved, the disabled-node meaning is not).
fn extract_comments(trivia: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = trivia.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if trivia[i..].starts_with("//") {
            let end = trivia[i..]
                .find('\n')
                .map(|k| i + k)
                .unwrap_or(trivia.len());
            out.push(trivia[i..end].trim_end().to_string());
            i = end;
        } else if trivia[i..].starts_with("/*") {
            match trivia[i + 2..].find("*/") {
                Some(k) => {
                    out.push(trivia[i..i + 2 + k + 2].to_string());
                    i += 2 + k + 2;
                }
                None => break,
            }
        } else if trivia[i..].starts_with("/-") {
            let end = trivia[i..]
                .find('\n')
                .map(|k| i + k)
                .unwrap_or(trivia.len());
            out.push(trivia[i..end].trim_end().to_string());
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

fn count_blanks(trivia: &str) -> usize {
    let lines: Vec<&str> = trivia.split('\n').collect();
    if lines.len() < 2 {
        return 0;
    }
    lines[1..lines.len() - 1]
        .iter()
        .filter(|l| l.trim().is_empty())
        .count()
}

struct Ctx<'a> {
    src: &'a str,
    order: usize,
    depth: usize,
}

impl Ctx<'_> {
    fn next_order(&mut self) -> usize {
        let o = self.order;
        self.order += 1;
        o
    }

    fn err(&self, msg: impl Into<String>) -> Error {
        Error::parse(1, 1, msg)
    }
}

fn kdl_value_to_ir(v: &kdl::KdlValue) -> Value {
    match v {
        kdl::KdlValue::String(s) => Value::Str(s.clone()),
        kdl::KdlValue::Integer(i) => Value::Number(Number {
            raw: i.to_string(),
            kind: NumberKind::Int,
        }),
        kdl::KdlValue::Float(f) => Value::Number(Number {
            raw: float_raw(*f),
            kind: NumberKind::Float,
        }),
        kdl::KdlValue::Bool(b) => Value::Bool(*b),
        kdl::KdlValue::Null => Value::Null,
    }
}

fn float_raw(f: f64) -> String {
    if f.is_nan() {
        "#nan".to_string()
    } else if f.is_infinite() {
        if f > 0.0 {
            "#inf".to_string()
        } else {
            "#-inf".to_string()
        }
    } else {
        format!("{f}")
    }
}

/// Same-line remainder after a node's span end (inline comment source).
fn inline_after(src: &str, end: usize) -> Option<String> {
    if end >= src.len() {
        return None;
    }
    if end > 0 && src.as_bytes()[end - 1] == b'\n' {
        return None;
    }
    let rest = &src[end..];
    let line = match rest.find('\n') {
        Some(i) => &rest[..i],
        None => rest,
    };
    // First // or /* outside... node text already ended; naive scan is safe
    // for the remainder (no strings can start here except... `//` in what
    // follows on the line is a comment by KDL rules).
    if let Some(i) = line.find("//") {
        return Some(line[i..].trim_end().to_string());
    }
    if let Some(i) = line.find("/*") {
        return Some(line[i..].trim_end().to_string());
    }
    None
}

fn node_to_entry(ctx: &mut Ctx, node: &kdl::KdlNode) -> Result<Entry, Error> {
    if ctx.depth > MAX_DEPTH {
        return Err(ctx.err("nesting too deep (limit 64)"));
    }
    if node.ty().is_some() {
        return Err(ctx.err("type annotations are outside the stage-5 KDL subset"));
    }
    let name = node.name().to_string();
    let leading = node.format().map(|f| f.leading.as_str()).unwrap_or("");
    let mut key_trivia = Trivia::empty();
    key_trivia.leading = extract_comments(leading);
    key_trivia.blanks_before = count_blanks(leading);
    // Inline comment: same-line remainder after the node span.
    let span = node.span();
    let node_end = span.offset() + span.len();
    let inline = inline_after(ctx.src, node_end);

    // Split entries into positional args and properties.
    let mut args: Vec<Node> = Vec::new();
    let mut props: Vec<Entry> = Vec::new();
    for e in node.entries() {
        let mut vn = Node::new(kdl_value_to_ir(e.value()));
        vn.order = ctx.next_order();
        match e.name() {
            None => args.push(vn),
            Some(k) => {
                let key = k.to_string();
                props.push(Entry {
                    key: Key::plain(key),
                    key_trivia: Trivia::empty(),
                    sep_raw: None,
                    value: vn,
                });
            }
        }
    }
    let has_children = node.children().is_some_and(|c| !c.nodes().is_empty());
    let value = if args.is_empty() && props.is_empty() && !has_children {
        Value::Null
    } else if args.len() == 1 && props.is_empty() && !has_children {
        args.pop().map(|n| n.value).unwrap_or(Value::Null)
    } else {
        let mut map: Vec<Entry> = Vec::new();
        if !args.is_empty() {
            if props.iter().any(|e| e.key.text == ARGS_KEY) {
                return Err(ctx.err(format!(
                    "property '{ARGS_KEY}' collides with positional args (node '{name}')"
                )));
            }
            map.push(Entry {
                key: Key::plain(ARGS_KEY.to_string()),
                key_trivia: Trivia::empty(),
                sep_raw: None,
                value: Node::new(Value::Array(args)),
            });
        }
        map.extend(props);
        if has_children {
            if map.iter().any(|e| e.key.text == CHILDREN_KEY) {
                return Err(ctx.err(format!(
                    "property '{CHILDREN_KEY}' collides with child nodes (node '{name}')"
                )));
            }
            ctx.depth += 1;
            let mut kids: Vec<Entry> = Vec::new();
            for kid in node.children().expect("checked").nodes() {
                kids.push(node_to_entry(ctx, kid)?);
            }
            ctx.depth -= 1;
            map.push(Entry {
                key: Key::plain(CHILDREN_KEY.to_string()),
                key_trivia: Trivia::empty(),
                sep_raw: None,
                value: Node::new(Value::Map(kids)),
            });
        }
        Value::Map(map)
    };
    let mut vnode = Node::new(value);
    vnode.order = ctx.next_order();
    if let Some(c) = inline {
        vnode.trivia.inline = Some(c);
    }
    Ok(Entry {
        key: Key::plain(name),
        key_trivia,
        sep_raw: None,
        value: vnode,
    })
}

/// Parse KDL source into an IR document (v2, with v1 fallback via the
/// crate's `v1-fallback` feature: v1 documents parse transparently).
///
/// The `kdl` parser recurses per nesting level and aborts small stacks on
/// hostile input (`{{{{…` — found by the adversarial suite), so parsing
/// runs on a worker thread with a 256 MiB stack (virtual, ~free). The
/// parsed document is owned, hence movable across threads. Our own depth
/// (64) still bounds the IR walk afterwards. On wasm32 threads do not
/// exist, so parsing runs inline there (demo inputs are small).
pub fn parse_kdl(src: &str) -> Result<Doc, Error> {
    #[cfg(not(target_arch = "wasm32"))]
    let kdoc: kdl::KdlDocument = {
        let owned = src.to_string();
        std::thread::Builder::new()
            .name("cfgprism-kdl-parse".to_string())
            .stack_size(256 << 20)
            .spawn(move || owned.parse::<kdl::KdlDocument>())
            .map_err(|e| Error::io(format!("cannot spawn KDL worker thread: {e}")))?
            .join()
            .map_err(|_| Error::emit("KDL parser failed internally on hostile input"))?
            .map_err(|e| kdl_error(src, e))?
    };
    #[cfg(target_arch = "wasm32")]
    let kdoc: kdl::KdlDocument = src.parse().map_err(|e| kdl_error(src, e))?;
    let mut ctx = Ctx {
        src,
        order: 0,
        depth: 0,
    };
    let mut entries = Vec::new();
    for node in kdoc.nodes() {
        entries.push(node_to_entry(&mut ctx, node)?);
    }
    let mut root = Node::new(Value::Map(entries));
    root.order = 0;
    let mut doc = Doc::new(root);
    // File tail: everything after the last node span.
    let mut end = 0;
    for node in kdoc.nodes() {
        let s = node.span();
        end = end.max(s.offset() + s.len());
    }
    let tail = src.get(end..).unwrap_or("").to_string();
    doc.trailing.leading = extract_comments(&tail);
    doc.trailing.prefix_raw = Some(tail);
    Ok(doc)
}

/// Emit a KDL document canonically (values + comments + order; layout
/// normalized). Used for both same-format output and logical conversion:
/// KDL round-trip is canonical by design (see module docs).
pub fn emit_kdl(doc: &Doc, opt: &Options) -> Result<EmitOutput, Error> {
    let Value::Map(entries) = &doc.root.value else {
        return Err(Error::emit("kdl root must be a map"));
    };
    let mut warnings = Vec::new();
    let flat = crate::logical::expand_entries(entries, "", false, &mut warnings);
    let mut out = String::new();
    for c in &doc.root.trivia.leading {
        for line in crate::logical::restyle_comment(c, "//") {
            out.push_str(&line);
            out.push('\n');
        }
    }
    for e in &flat {
        emit_node(&mut out, &mut warnings, "", e, 0, opt.indent.max(1))?;
    }
    for c in &doc.trailing.leading {
        for line in crate::logical::restyle_comment(c, "//") {
            out.push_str(&line);
            out.push('\n');
        }
    }
    Ok(EmitOutput {
        text: out,
        warnings,
    })
}

fn emit_node(
    out: &mut String,
    warnings: &mut Vec<Warning>,
    path: &str,
    e: &Entry,
    level: usize,
    indent: usize,
) -> Result<(), Error> {
    let epath = if path.is_empty() {
        e.key.text.clone()
    } else {
        format!("{path}.{}", e.key.text)
    };
    if let Some(a) = &e.value.anchor {
        warnings.push(Warning::new(
            &epath,
            WarningKind::AnchorExpanded,
            format!("anchor '{}' expanded (KDL has no anchors)", a.name),
        ));
    }
    let pad = " ".repeat(indent * level);
    for _ in 0..e.key_trivia.blanks_before {
        out.push('\n');
    }
    for c in &e.key_trivia.leading {
        for line in crate::logical::restyle_comment(c, "//") {
            out.push_str(&pad);
            out.push_str(&line);
            out.push('\n');
        }
    }
    out.push_str(&pad);
    out.push_str(&kdl_ident(&e.key.text));
    emit_node_value(out, warnings, &epath, &e.value, level, indent)?;
    if let Some(c) = &e.value.trivia.inline {
        out.push(' ');
        out.push_str(&crate::logical::restyle_comment(c, "//").join(" "));
    }
    out.push('\n');
    Ok(())
}

fn emit_node_value(
    out: &mut String,
    warnings: &mut Vec<Warning>,
    path: &str,
    node: &Node,
    level: usize,
    indent: usize,
) -> Result<(), Error> {
    match &node.value {
        Value::Null => Ok(()),
        Value::Bool(b) => {
            out.push(' ');
            out.push_str(&b.to_string());
            Ok(())
        }
        Value::Number(n) => {
            out.push(' ');
            out.push_str(&n.raw);
            Ok(())
        }
        Value::Str(s) => {
            out.push(' ');
            out.push_str(&kdl_string(s));
            Ok(())
        }
        Value::Datetime(d) => {
            warnings.push(Warning::new(
                path,
                WarningKind::TypeCoerced,
                "datetime has no KDL representation; emitted as string",
            ));
            out.push(' ');
            out.push_str(&kdl_string(d));
            Ok(())
        }
        Value::Array(items) => {
            // Positional args (nested containers are unrepresentable).
            for it in items {
                if matches!(it.value, Value::Array(_) | Value::Map(_)) {
                    return Err(Error::emit(format!(
                        "nested values have no KDL argument representation ({path})"
                    )));
                }
                out.push(' ');
                out.push_str(&kdl_scalar(it)?);
            }
            Ok(())
        }
        Value::Map(entries) => {
            // Reversible split: `args`/`children` keys or plain props?
            // A map that came from KDL carries them; foreign maps are props
            // with a possible `children` sub-block.
            let flat = crate::logical::expand_entries(entries, path, false, warnings);
            let mut args: &[Node] = &[];
            let mut children: Option<&Vec<Entry>> = None;
            let mut props: Vec<&Entry> = Vec::new();
            for en in &flat {
                match en.key.text.as_str() {
                    ARGS_KEY => {
                        if let Value::Array(items) = &en.value.value {
                            args = items;
                        } else {
                            props.push(en);
                        }
                    }
                    CHILDREN_KEY => {
                        if let Value::Map(kids) = &en.value.value {
                            children = Some(kids);
                        } else {
                            props.push(en);
                        }
                    }
                    _ => props.push(en),
                }
            }
            for a in args {
                out.push(' ');
                out.push_str(&kdl_scalar(a)?);
            }
            for p in props {
                out.push(' ');
                out.push_str(&kdl_ident(&p.key.text));
                out.push('=');
                out.push_str(&kdl_scalar(&p.value)?);
            }
            if let Some(kids) = children {
                out.push_str(" {\n");
                for k in kids {
                    emit_node(out, warnings, path, k, level + 1, indent)?;
                }
                out.push_str(&" ".repeat(indent * level));
                out.push('}');
            }
            Ok(())
        }
        _ => Err(Error::emit("unexpected value kind in KDL output")),
    }
}

fn kdl_scalar(node: &Node) -> Result<String, Error> {
    match &node.value {
        Value::Null => Ok("null".to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Number(n) => Ok(n.raw.clone()),
        Value::Str(s) => Ok(kdl_string(s)),
        Value::Datetime(d) => Ok(kdl_string(d)),
        Value::Array(_) | Value::Map(_) => Err(Error::emit(
            "nested values have no KDL scalar representation",
        )),
        _ => Err(Error::emit("unexpected value kind in KDL output")),
    }
}

/// Bare KDL identifier when possible, quoted otherwise.
fn kdl_ident(text: &str) -> String {
    let mut chars = text.chars();
    let bare_head = matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_');
    let bare_rest = text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '$' | '/'));
    // Keywords and number-likes must stay quoted.
    let reserved = matches!(
        text,
        "true" | "false" | "null" | "inf" | "#inf" | "#-inf" | "#nan"
    ) || text.parse::<f64>().is_ok() && !text.is_empty();
    if bare_head && bare_rest && !reserved && !text.starts_with(|c: char| c.is_ascii_digit()) {
        text.to_string()
    } else {
        kdl_string(text)
    }
}

fn kdl_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{{{:04X}}}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

impl Format for KdlFormat {
    fn name(&self) -> &'static str {
        "kdl"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["kdl"]
    }

    fn parse(&self, src: &str) -> Result<Doc, Error> {
        parse_kdl(src)
    }

    fn emit(&self, doc: &Doc, opt: &Options) -> Result<EmitOutput, Error> {
        // Canonical by design (see module docs): same function serves both.
        emit_kdl(doc, opt)
    }

    fn emit_logical(&self, doc: &Doc, opt: &Options) -> Result<EmitOutput, Error> {
        emit_kdl(doc, opt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgprism_core::values_equal;

    /// Canonical round-trip: re-parse the canonical output and compare IR
    /// (layout normalizes by design; values/comments/order survive).
    fn roundtrip(src: &str) -> Doc {
        let fmt = KdlFormat;
        let doc = fmt.parse(src).expect("parse");
        let out = fmt.emit(&doc, &Options::default()).expect("emit");
        assert!(out.warnings.is_empty());
        fmt.parse(&out.text).expect("re-parse")
    }

    #[test]
    fn nodes_args_props_children_round_trip() {
        let src = "// head\nserver \"x\" port=8080 {\n  /* multi */\n  route \"/a\"\n}\ndebug\nanswer 42\n";
        let a = KdlFormat.parse(src).expect("parse");
        let b = roundtrip(src);
        assert!(values_equal(&a.root, &b.root));
        // Single-arg node maps to its scalar.
        assert_eq!(
            a.root.get("answer").expect("answer").value,
            Value::Number(Number {
                raw: "42".to_string(),
                kind: NumberKind::Int
            })
        );
        // Bare node maps to null.
        assert_eq!(a.root.get("debug").expect("debug").value, Value::Null);
    }

    #[test]
    fn v1_documents_parse() {
        let doc = KdlFormat
            .parse("node 1 2.5 \"s\" true false null\n")
            .expect("parse");
        let node = doc.root.get("node").expect("node");
        let Value::Map(map) = &node.value else {
            panic!("node must map args");
        };
        assert_eq!(map.len(), 1);
        assert_eq!(map[0].key.text, "args");
        let Value::Array(items) = &map[0].value.value else {
            panic!("args must be an array");
        };
        assert_eq!(items.len(), 6);
        assert!(matches!(items[0].value, Value::Number(_)));
        assert!(matches!(items[1].value, Value::Number(_)));
        assert_eq!(items[2].value, Value::Str("s".to_string()));
        assert_eq!(items[3].value, Value::Bool(true));
        assert_eq!(items[4].value, Value::Bool(false));
        assert_eq!(items[5].value, Value::Null);
    }
}
