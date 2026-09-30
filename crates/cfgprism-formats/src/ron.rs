//! RON (Rusty Object Notation) via the `ron` crate: structs, maps, lists,
//! strings/chars, numbers, bools, options, units.
//!
//! Mapping: RON structs `(k: v)` and maps `{"k": v}` both become IR maps;
//! emission prefers struct form for identifier-safe keys, map form
//! otherwise. `Some(x)` unwraps to `x`, `None`/`()` become null, chars
//! become single-character strings, struct/enum *names* are dropped (the
//! `ron::Value` model does not carry them — documented in D17).
//!
//! Round-trip is canonical, not byte-verbatim: `ron` exposes no spans, so
//! comments are pre-lexed (`//`, `/* */`, plus `#!` extension lines kept
//! verbatim) and hoisted to the document head on output. Values, key order
//! and comment text survive; layout and comment placement normalize.

use cfgprism_core::{
    Doc, EmitOutput, Entry, Error, Format, Key, Node, Number, NumberKind, Options, Trivia, Value,
    Warning, WarningKind,
};

/// RON format (`*.ron`, canonical round-trip).
pub struct RonFormat;

/// Pre-lexed comment lines (in source order).
fn lex_comments(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = src.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    let mut in_char = false;
    let mut escaped = false;
    let mut line_has_code = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
                line_has_code = true;
            }
            i += 1;
            continue;
        }
        if in_char {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'\'' {
                in_char = false;
                line_has_code = true;
            }
            i += 1;
            continue;
        }
        match b {
            b'"' => {
                in_string = true;
                line_has_code = true;
                i += 1;
            }
            b'\'' => {
                in_char = true;
                line_has_code = true;
                i += 1;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                // `//` comment: take the whole line, but only as a hoisted
                // comment when the line holds no code before it.
                let mut j = i;
                while j < bytes.len() && bytes[j] != b'\n' {
                    j += 1;
                }
                if !line_has_code {
                    out.push(src[i..j].trim_end().to_string());
                } else {
                    // Trailing comment: still hoisted (placement normalizes).
                    out.push(src[i..j].trim_end().to_string());
                }
                i = j;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                match src[i + 2..].find("*/") {
                    Some(k) => {
                        let end = i + 2 + k + 2;
                        out.push(src[i..end].to_string());
                        i = end;
                    }
                    None => {
                        // Unterminated: the RON parser reports it; stop here.
                        break;
                    }
                }
            }
            b'\n' => {
                line_has_code = false;
                i += 1;
            }
            b' ' | b'\t' | b'\r' => {
                i += 1;
            }
            _ => {
                line_has_code = true;
                i += 1;
            }
        }
    }
    // `#!` extension lines are kept verbatim (they are not comments, but
    // dropping them would lose text; they re-emit as-is at the head).
    let mut with_ext: Vec<String> = Vec::new();
    for line in src.lines() {
        let t = line.trim_start();
        if t.starts_with("#!") {
            with_ext.push(t.to_string());
        }
    }
    with_ext.extend(out);
    with_ext
}

fn ron_error(e: ron::error::SpannedError) -> Error {
    let msg = e.code.to_string();
    let first = msg
        .lines()
        .next()
        .unwrap_or("invalid RON")
        .trim()
        .to_string();
    Error::parse(
        (e.position.line as u32).max(1),
        (e.position.col as u32).max(1),
        first,
    )
}

fn value_to_ir(v: &ron::Value) -> Result<Value, Error> {
    match v {
        ron::Value::Bool(b) => Ok(Value::Bool(*b)),
        ron::Value::Char(c) => Ok(Value::Str(c.to_string())),
        ron::Value::Number(n) => match n {
            ron::Number::Integer(i) => Ok(Value::Number(Number {
                raw: i.to_string(),
                kind: NumberKind::Int,
            })),
            ron::Number::Float(f) => {
                let v = f.get();
                Ok(Value::Number(Number {
                    raw: float_raw(v),
                    kind: NumberKind::Float,
                }))
            }
        },
        ron::Value::String(s) => Ok(Value::Str(s.clone())),
        ron::Value::Option(None) => Ok(Value::Null),
        ron::Value::Option(Some(inner)) => value_to_ir(inner),
        ron::Value::Unit => Ok(Value::Null),
        ron::Value::Seq(items) => {
            let mut out = Vec::new();
            for i in items {
                out.push(Node::new(value_to_ir(i)?));
            }
            Ok(Value::Array(out))
        }
        ron::Value::Map(entries) => {
            let mut out = Vec::new();
            for (k, v) in entries.iter() {
                out.push(Entry {
                    key: Key::plain(ron_key_text(k)?),
                    key_trivia: Trivia::empty(),
                    sep_raw: None,
                    value: Node::new(value_to_ir(v)?),
                });
            }
            Ok(Value::Map(out))
        }
    }
}

/// Canonical text for a RON map key (strings verbatim, scalars canonical).
fn ron_key_text(k: &ron::Value) -> Result<String, Error> {
    match k {
        ron::Value::String(s) => Ok(s.clone()),
        ron::Value::Char(c) => Ok(c.to_string()),
        ron::Value::Bool(b) => Ok(b.to_string()),
        ron::Value::Number(ron::Number::Integer(i)) => Ok(i.to_string()),
        ron::Value::Number(ron::Number::Float(f)) => Ok(float_raw(f.get())),
        _ => Err(Error::emit("complex RON map keys are not supported")),
    }
}

fn float_raw(f: f64) -> String {
    if f.is_nan() {
        "nan".to_string()
    } else if f.is_infinite() {
        if f > 0.0 {
            "inf".to_string()
        } else {
            "-inf".to_string()
        }
    } else {
        format!("{f}")
    }
}

/// Parse RON source into an IR document (comments hoisted to the head).
pub fn parse_ron(src: &str) -> Result<Doc, Error> {
    let comments = lex_comments(src);
    let value: ron::Value = ron::Options::default().from_str(src).map_err(ron_error)?;
    let mut root = Node::new(value_to_ir(&value)?);
    root.order = 0;
    let mut doc = Doc::new(root);
    doc.root.trivia.leading = comments;
    Ok(doc)
}

/// Emit a RON document canonically (values + hoisted comments + order).
/// Used for both same-format output and logical conversion: RON round-trip
/// is canonical by design (see module docs).
pub fn emit_ron(doc: &Doc, _opt: &Options) -> Result<EmitOutput, Error> {
    let mut warnings = Vec::new();
    let flat = match &doc.root.value {
        Value::Map(entries) => crate::logical::expand_entries(entries, "", false, &mut warnings),
        _ => {
            return Err(Error::emit(
                "ron root must be a map (use a top-level struct or map)",
            ));
        }
    };
    let mut out = String::new();
    for c in &doc.root.trivia.leading {
        for line in crate::logical::restyle_comment(c, "//") {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out.push('(');
    if flat.is_empty() {
        out.push(')');
    } else {
        out.push('\n');
        for (i, e) in flat.iter().enumerate() {
            emit_entry(&mut out, &mut warnings, &e.key.text, e, 1)?;
            if i + 1 < flat.len() {
                out.push(',');
            }
            out.push('\n');
        }
        out.push(')');
    }
    for c in &doc.trailing.leading {
        for line in crate::logical::restyle_comment(c, "//") {
            out.push_str(&line);
            out.push('\n');
        }
    }
    // Trailing newline convention for text files.
    if !out.ends_with('\n') {
        out.push('\n');
    }
    Ok(EmitOutput {
        text: out,
        warnings,
    })
}

fn emit_entry(
    out: &mut String,
    warnings: &mut Vec<Warning>,
    path: &str,
    e: &Entry,
    level: usize,
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
            format!("anchor '{}' expanded (RON has no anchors)", a.name),
        ));
    }
    for _ in 0..e.key_trivia.blanks_before {
        out.push('\n');
    }
    for c in &e.key_trivia.leading {
        for line in crate::logical::restyle_comment(c, "//") {
            out.push_str(&"  ".repeat(level));
            out.push_str(&line);
            out.push('\n');
        }
    }
    out.push_str(&"  ".repeat(level));
    out.push_str(&ron_key(&e.key.text));
    out.push_str(": ");
    out.push_str(&emit_value(warnings, &epath, &e.value)?);
    if let Some(c) = &e.value.trivia.inline {
        out.push(' ');
        out.push_str(&crate::logical::restyle_comment(c, "//").join(" "));
    }
    Ok(())
}

fn emit_value(warnings: &mut Vec<Warning>, path: &str, node: &Node) -> Result<String, Error> {
    if let Some(a) = &node.anchor {
        warnings.push(Warning::new(
            path,
            WarningKind::AnchorExpanded,
            format!("anchor '{}' expanded (RON has no anchors)", a.name),
        ));
    }
    match &node.value {
        Value::Null => Ok("None".to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Number(n) => Ok(n.raw.clone()),
        Value::Str(s) => Ok(ron_string(s)),
        Value::Datetime(d) => {
            warnings.push(Warning::new(
                path,
                WarningKind::TypeCoerced,
                "datetime has no RON representation; emitted as string",
            ));
            Ok(ron_string(d))
        }
        Value::Array(items) => {
            let mut parts = Vec::new();
            for it in items {
                parts.push(emit_value(warnings, path, it)?);
            }
            Ok(format!("[{}]", parts.join(", ")))
        }
        Value::Map(entries) => {
            let flat = crate::logical::expand_entries(entries, path, false, warnings);
            // Struct form for identifier-safe keys, map form otherwise.
            let struct_form = flat.iter().all(|e| is_ron_ident(&e.key.text));
            if struct_form {
                let mut s = String::from("(");
                for (i, e) in flat.iter().enumerate() {
                    s.push_str(&e.key.text);
                    s.push_str(": ");
                    s.push_str(&emit_value(
                        warnings,
                        &format!("{path}.{}", e.key.text),
                        &e.value,
                    )?);
                    if i + 1 < flat.len() {
                        s.push_str(", ");
                    }
                }
                s.push(')');
                Ok(s)
            } else {
                let mut s = String::from("{");
                for (i, e) in flat.iter().enumerate() {
                    s.push_str(&ron_string(&e.key.text));
                    s.push_str(": ");
                    s.push_str(&emit_value(
                        warnings,
                        &format!("{path}.{}", e.key.text),
                        &e.value,
                    )?);
                    if i + 1 < flat.len() {
                        s.push_str(", ");
                    }
                }
                s.push('}');
                Ok(s)
            }
        }
        _ => Err(Error::emit("unexpected value kind in RON output")),
    }
}

fn is_ron_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn ron_key(text: &str) -> String {
    if is_ron_ident(text) {
        text.to_string()
    } else {
        ron_string(text)
    }
}

fn ron_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

impl Format for RonFormat {
    fn name(&self) -> &'static str {
        "ron"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["ron"]
    }

    fn parse(&self, src: &str) -> Result<Doc, Error> {
        parse_ron(src)
    }

    fn emit(&self, doc: &Doc, opt: &Options) -> Result<EmitOutput, Error> {
        // Canonical by design (see module docs): same function serves both.
        emit_ron(doc, opt)
    }

    fn emit_logical(&self, doc: &Doc, opt: &Options) -> Result<EmitOutput, Error> {
        emit_ron(doc, opt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgprism_core::values_equal;

    /// Canonical round-trip: re-parse the canonical output and compare IR.
    fn roundtrip(src: &str) -> Doc {
        let fmt = RonFormat;
        let doc = fmt.parse(src).expect("parse");
        let out = fmt.emit(&doc, &Options::default()).expect("emit");
        assert!(out.warnings.is_empty());
        fmt.parse(&out.text).expect("re-parse")
    }

    #[test]
    fn structs_maps_options_round_trip() {
        let src = "// head\nConfig(\n  debug: true,\n  name: \"x\",\n  maybe: None,\n  list: [1, \"two\"],\n)\n";
        let a = RonFormat.parse(src).expect("parse");
        let b = roundtrip(src);
        assert!(values_equal(&a.root, &b.root));
        // Struct names are dropped (documented): Config(…) reads as a map.
        assert!(a.root.get("debug").is_some());
        assert_eq!(a.root.get("maybe").expect("maybe").value, Value::Null);
    }

    #[test]
    fn errors_carry_position() {
        let err = RonFormat.parse("(a: ").expect_err("must fail");
        assert!(err.pos.line >= 1 && err.pos.col >= 1, "{err}");
    }
}
