//! Stage-1 JSON stub: parses `serde_json::Value` into IR and emits it back.
//!
//! Explicitly LOSSY w.r.t. trivia (comments/whitespace/styles are not part of
//! strict JSON, so nothing is lost *yet*): order is preserved, numbers keep
//! their raw spelling for the round trip. Stage 2 replaces this module with
//! a trivia-preserving JSON/JSONC/JSON5 implementation.

use cfgprism_core::{
    Doc, EmitOutput, Entry, Error, Format, Key, Node, Number, NumberKind, Options, Style, Trivia,
    Value,
};

/// JSON format (stage-1 stub).
pub struct JsonStubFormat;

impl JsonStubFormat {
    /// Shared instance helper.
    #[must_use]
    pub fn boxed() -> Box<dyn Format> {
        Box::new(Self)
    }

    fn convert_value(v: &serde_json::Value, order: &mut usize) -> Node {
        let next = |order: &mut usize| {
            let o = *order;
            *order += 1;
            o
        };
        match v {
            serde_json::Value::Null => Node {
                value: Value::Null,
                trivia: Trivia::empty(),
                style: Style::Plain,
                span: None,
                anchor: None,
                order: next(order),
            },
            serde_json::Value::Bool(b) => Node {
                value: Value::Bool(*b),
                trivia: Trivia::empty(),
                style: Style::Plain,
                span: None,
                anchor: None,
                order: next(order),
            },
            serde_json::Value::Number(n) => Node {
                value: Value::Number(Number {
                    raw: n.to_string(),
                    kind: if n.is_f64() {
                        NumberKind::Float
                    } else {
                        NumberKind::Int
                    },
                }),
                trivia: Trivia::empty(),
                style: Style::Plain,
                span: None,
                anchor: None,
                order: next(order),
            },
            serde_json::Value::String(s) => Node {
                value: Value::Str(s.clone()),
                trivia: Trivia::empty(),
                style: Style::DoubleQuoted,
                span: None,
                anchor: None,
                order: next(order),
            },
            serde_json::Value::Array(items) => {
                let o = next(order);
                let nodes = items
                    .iter()
                    .map(|i| Self::convert_value(i, order))
                    .collect();
                Node {
                    value: Value::Array(nodes),
                    trivia: Trivia::empty(),
                    style: Style::Flow,
                    span: None,
                    anchor: None,
                    order: o,
                }
            }
            serde_json::Value::Object(map) => {
                let o = next(order);
                let entries = map
                    .iter()
                    .map(|(k, v)| Entry {
                        key: Key::plain(k.clone()),
                        key_trivia: Trivia::empty(),
                        value: Self::convert_value(v, order),
                    })
                    .collect();
                Node {
                    value: Value::Map(entries),
                    trivia: Trivia::empty(),
                    style: Style::Flow,
                    span: None,
                    anchor: None,
                    order: o,
                }
            }
        }
    }

    fn emit_value(node: &Node, indent: usize, level: usize) -> Result<String, Error> {
        match &node.value {
            Value::Null => Ok("null".to_string()),
            Value::Bool(b) => Ok(b.to_string()),
            Value::Number(n) => Ok(n.raw.clone()),
            Value::Str(s) => Ok(serde_json::to_string(s)
                .map_err(|e| Error::emit(format!("cannot quote string: {e}")))?),
            Value::Array(items) => {
                if items.is_empty() {
                    return Ok("[]".to_string());
                }
                let pad_outer = " ".repeat(indent * level);
                let pad_inner = " ".repeat(indent * (level + 1));
                let mut out = String::from("[\n");
                for (i, item) in items.iter().enumerate() {
                    out.push_str(&pad_inner);
                    out.push_str(&Self::emit_value(item, indent, level + 1)?);
                    if i + 1 < items.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&pad_outer);
                out.push(']');
                Ok(out)
            }
            Value::Map(entries) => {
                if entries.is_empty() {
                    return Ok("{}".to_string());
                }
                let pad_outer = " ".repeat(indent * level);
                let pad_inner = " ".repeat(indent * (level + 1));
                let mut out = String::from("{\n");
                for (i, e) in entries.iter().enumerate() {
                    let key = serde_json::to_string(&e.key.text)
                        .map_err(|err| Error::emit(format!("cannot quote key: {err}")))?;
                    out.push_str(&pad_inner);
                    out.push_str(&key);
                    out.push_str(": ");
                    out.push_str(&Self::emit_value(&e.value, indent, level + 1)?);
                    if i + 1 < entries.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&pad_outer);
                out.push('}');
                Ok(out)
            }
            _ => Err(Error::emit("unsupported value for JSON stub")),
        }
    }
}

impl Format for JsonStubFormat {
    fn name(&self) -> &'static str {
        "json"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["json"]
    }

    fn parse(&self, src: &str) -> Result<Doc, Error> {
        let v: serde_json::Value = serde_json::from_str(src).map_err(|e| {
            Error::parse(
                u32::try_from(e.line()).unwrap_or(1).max(1),
                u32::try_from(e.column()).unwrap_or(1).max(1),
                format!("invalid JSON: {e}"),
            )
        })?;
        let mut order = 0_usize;
        let root = Self::convert_value(&v, &mut order);
        Ok(Doc::new(root))
    }

    fn emit(&self, doc: &Doc, opt: &Options) -> Result<EmitOutput, Error> {
        let indent = opt.indent.max(1);
        let mut text = Self::emit_value(&doc.root, indent, 0)?;
        text.push('\n');
        Ok(EmitOutput {
            text,
            warnings: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_preserves_key_order() {
        let fmt = JsonStubFormat;
        let doc = fmt
            .parse(r#"{"z": 1, "a": 2, "m": 3}"#)
            .expect("valid JSON");
        assert_eq!(doc.root_keys_in_order(), vec!["z", "a", "m"]);
    }

    #[test]
    fn json_error_carries_line_col() {
        let fmt = JsonStubFormat;
        let err = fmt.parse("{bad}").expect_err("must fail");
        assert!(err.pos.line >= 1 && err.pos.col >= 1, "{err}");
    }

    #[test]
    fn json_emit_round_trip_values() {
        let fmt = JsonStubFormat;
        let doc = fmt.parse(r#"{"b": [1, 2]}"#).expect("valid");
        let out = fmt.emit(&doc, &Options::default()).expect("emit");
        assert!(out.text.contains("\"b\""));
        assert!(out.warnings.is_empty());
    }
}
