//! Java `.properties` files: `key = value` / `key: value` / `key value`
//! entries, `#` and `!` comments, `\`-continuations and `\uXXXX` escapes.
//!
//! Model: flat string map (like dotenv, with richer separators/escapes).
//! Every line keeps its verbatim slices, so self round-trip is
//! byte-identical; duplicate keys are preserved in order (Java semantics
//! are last-wins, applied by readers).

use cfgprism_core::{
    offset_to_line_col, Doc, EmitOutput, Entry, Error, Format, Key, Node, Options, Style, Trivia,
    Value, Warning, WarningKind,
};

use crate::util::{line_starts, split_lines};

/// Java properties / `*.properties` files.
pub struct PropertiesFormat;

/// Decode escapes (`\uXXXX`, `\n`, `\t`, `\r`, `\\`, `\"`, `\'`, `\ `,
/// `\#`, `\!`, `\=`, `\:` and line continuations are handled by the caller).
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('\'') => out.push('\''),
            Some(' ') => out.push(' '),
            Some('#') => out.push('#'),
            Some('!') => out.push('!'),
            Some('=') => out.push('='),
            Some(':') => out.push(':'),
            Some('u') => {
                let hex: String = it.by_ref().take(4).collect();
                match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    Some(ch) => out.push(ch),
                    None => {
                        out.push_str("\\u");
                        out.push_str(&hex);
                    }
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Find the key/value split: first unescaped `=`, `:` or whitespace run.
/// Returns `(key_end, value_start)` byte offsets into the line.
fn find_split(line: &str) -> Option<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'=' | b':' => {
                // Value starts after the separator plus one whitespace run.
                let mut j = i + 1;
                while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t' || bytes[j] == 0x0c)
                {
                    j += 1;
                }
                return Some((i, j));
            }
            b' ' | b'\t' | b'\x0c' => {
                // Whitespace separator: value starts after the run, and an
                // immediately following `=`/`:` is also consumed.
                let mut j = i;
                while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t' || bytes[j] == 0x0c)
                {
                    j += 1;
                }
                if bytes.get(j) == Some(&b'=') || bytes.get(j) == Some(&b':') {
                    j += 1;
                    while j < bytes.len()
                        && (bytes[j] == b' ' || bytes[j] == b'\t' || bytes[j] == 0x0c)
                    {
                        j += 1;
                    }
                }
                return Some((i, j));
            }
            _ => i += 1,
        }
    }
    None
}

/// Pending trivia accumulator.
#[derive(Default)]
struct Pending {
    raw: String,
    leading: Vec<String>,
    blanks: usize,
}

impl Pending {
    fn take(&mut self) -> (String, Vec<String>, usize) {
        (
            std::mem::take(&mut self.raw),
            std::mem::take(&mut self.leading),
            std::mem::replace(&mut self.blanks, 0),
        )
    }
}

/// Parse properties source into an IR document (root is always a Map).
pub fn parse_properties(src: &str) -> Result<Doc, Error> {
    let starts = line_starts(src);
    let lines = split_lines(src);
    let mut entries: Vec<Entry> = Vec::new();
    let mut pending = Pending::default();
    let mut idx = 0;
    while idx < lines.len() {
        let line = &lines[idx];
        let line_off = starts[idx];
        let trimmed = line.content.trim();
        let raw_line = format!("{}{}", line.content, line.ending);
        if trimmed.is_empty() {
            pending.raw.push_str(&raw_line);
            pending.blanks += 1;
            idx += 1;
            continue;
        }
        if trimmed.starts_with('#') || trimmed.starts_with('!') {
            pending.raw.push_str(&raw_line);
            pending.leading.push(trimmed.to_string());
            idx += 1;
            continue;
        }
        // Entry line, possibly with `\` continuations.
        let mut content = line.content.to_string();
        let mut ending = line.ending.to_string();
        let mut j = idx;
        while ends_with_continuation(&content) {
            j += 1;
            if j >= lines.len() {
                let lc = offset_to_line_col(src, line_off);
                return Err(Error::parse(lc.line, lc.col, "dangling line continuation"));
            }
            // Strip the backslash + line break; leading whitespace of the
            // next line is insignificant.
            content.truncate(content.len() - 1);
            content.push_str(lines[j].content.trim_start());
            ending = lines[j].ending.to_string();
        }
        let full_end_offset = starts[j] + lines[j].content.len() + lines[j].ending.len();
        let _ = full_end_offset;
        let Some((kend, vstart)) = find_split(&content) else {
            // Bare key (no separator): empty value.
            let (praw, plead, pblanks) = pending.take();
            entries.push(Entry {
                key: Key {
                    text: unescape(content.trim()),
                    repr: None,
                    raw: Some(content.clone()),
                },
                key_trivia: Trivia {
                    leading: plead,
                    inline: None,
                    blanks_before: pblanks,
                    prefix_raw: Some(praw),
                    suffix_raw: None,
                },
                sep_raw: Some(String::new()),
                value: value_node(String::new(), String::new(), ending, entries.len()),
            });
            idx = j + 1;
            continue;
        };
        let key_verbatim = content[..kend].to_string();
        let sep_verbatim = content[kend..vstart].to_string();
        // Logical value from the joined content (continuations resolved);
        // verbatim value from the raw physical lines (byte round-trip).
        let logical = unescape(content[vstart..].trim_end());
        let val_verbatim = if j == idx {
            content[vstart..].to_string()
        } else {
            let mut raw = line.content[vstart..].to_string();
            raw.push_str(line.ending);
            for (n, cont) in lines.iter().enumerate().skip(idx + 1).take(j - idx) {
                raw.push_str(cont.content);
                if n < j {
                    raw.push_str(cont.ending);
                }
            }
            raw
        };
        let (praw, plead, pblanks) = pending.take();
        entries.push(Entry {
            key: Key {
                text: unescape(key_verbatim.trim()),
                repr: None,
                raw: Some(key_verbatim),
            },
            key_trivia: Trivia {
                leading: plead,
                inline: None,
                blanks_before: pblanks,
                prefix_raw: Some(praw),
                suffix_raw: None,
            },
            sep_raw: Some(sep_verbatim),
            value: value_node(logical, val_verbatim, ending, entries.len()),
        });
        idx = j + 1;
    }
    let mut root = Node::new(Value::Map(entries));
    root.order = 0;
    let mut doc = Doc::new(root);
    let (praw, plead, _) = pending.take();
    doc.trailing.leading = plead;
    doc.trailing.prefix_raw = Some(praw);
    Ok(doc)
}

fn value_node(logical: String, raw: String, ending: String, order: usize) -> Node {
    Node {
        value: Value::Str(logical),
        trivia: Trivia {
            suffix_raw: Some(ending),
            ..Trivia::empty()
        },
        style: Style::Original(raw),
        span: None,
        anchor: None,
        order,
        open_raw: None,
        close_raw: None,
    }
}

/// Odd number of trailing backslashes = continuation.
fn ends_with_continuation(content: &str) -> bool {
    let mut n = 0;
    for c in content.chars().rev() {
        if c == '\\' {
            n += 1;
        } else {
            break;
        }
    }
    n % 2 == 1
}

/// Emit a properties document: verbatim when slices exist, canonical
/// otherwise (with `CommentDropped` warnings for homeless comments).
pub fn emit_properties(doc: &Doc, _opt: &Options) -> Result<EmitOutput, Error> {
    let Value::Map(entries) = &doc.root.value else {
        return Err(Error::emit("properties root must be a map"));
    };
    let mut warnings = Vec::new();
    let mut out = doc.root.trivia.prefix_raw.clone().unwrap_or_default();
    for e in entries {
        if let Some(p) = e.key_trivia.prefix_raw.as_deref() {
            out.push_str(p);
        } else {
            emit_logical_prefix(&mut out, &e.key_trivia);
        }
        out.push_str(e.key.raw.as_deref().unwrap_or(&e.key.text));
        out.push_str(e.sep_raw.as_deref().unwrap_or("="));
        match &e.value.value {
            Value::Str(s) => {
                if let Style::Original(raw) = &e.value.style {
                    out.push_str(raw);
                } else {
                    out.push_str(s);
                    if !e.value.trivia.is_empty() {
                        warnings.push(Warning::new(
                            &e.key.text,
                            WarningKind::CommentDropped,
                            "comment has no verbatim slice in this output; dropped",
                        ));
                    }
                }
            }
            _ => {
                return Err(Error::emit(format!(
                    "properties values must be strings (key '{}')",
                    e.key.text
                )));
            }
        }
        if let Some(s) = e.value.trivia.suffix_raw.as_deref() {
            out.push_str(s);
        } else {
            out.push('\n');
        }
    }
    if let Some(tail) = doc.trailing.prefix_raw.as_deref() {
        out.push_str(tail);
    } else {
        emit_logical_prefix(&mut out, &doc.trailing);
    }
    Ok(EmitOutput {
        text: out,
        warnings,
    })
}

fn emit_logical_prefix(out: &mut String, t: &Trivia) {
    for _ in 0..t.blanks_before {
        out.push('\n');
    }
    for c in &t.leading {
        out.push_str(c);
        out.push('\n');
    }
}

impl Format for PropertiesFormat {
    fn name(&self) -> &'static str {
        "properties"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["properties"]
    }

    fn parse(&self, src: &str) -> Result<Doc, Error> {
        parse_properties(src)
    }

    fn emit(&self, doc: &Doc, opt: &Options) -> Result<EmitOutput, Error> {
        emit_properties(doc, opt)
    }

    fn emit_logical(&self, doc: &Doc, opt: &Options) -> Result<EmitOutput, Error> {
        emit_properties_logical(doc, opt)
    }
}

// ---------------------------------------------------------------------------
// Logical (canonical + warnings) emission for cross-format conversion.
// ---------------------------------------------------------------------------

/// Canonical properties: flat string map; nesting and invalid content are
/// precise errors; other scalars stringify.
pub fn emit_properties_logical(doc: &Doc, _opt: &Options) -> Result<EmitOutput, Error> {
    let Value::Map(entries) = &doc.root.value else {
        return Err(Error::emit("properties root must be a map"));
    };
    let mut warnings = Vec::new();
    let flat = crate::logical::expand_entries(entries, "", true, &mut warnings);
    let mut out = String::new();
    for c in &doc.root.trivia.leading {
        for line in crate::logical::restyle_comment(c, "#") {
            out.push_str(&line);
            out.push('\n');
        }
    }
    for e in &flat {
        if let Some(a) = &e.value.anchor {
            warnings.push(Warning::new(
                &e.key.text,
                WarningKind::AnchorExpanded,
                format!("anchor '{}' expanded (properties have no anchors)", a.name),
            ));
        }
        for _ in 0..e.key_trivia.blanks_before {
            out.push('\n');
        }
        for c in &e.key_trivia.leading {
            for line in crate::logical::restyle_comment(c, "#") {
                out.push_str(&line);
                out.push('\n');
            }
        }
        out.push_str(&escape_key(&e.key.text));
        out.push('=');
        out.push_str(&prop_string(&e.value)?);
        if let Some(c) = &e.value.trivia.inline {
            out.push(' ');
            out.push_str(&crate::logical::restyle_comment(c, "#").join(" "));
        }
        out.push('\n');
    }
    for c in &doc.trailing.leading {
        for line in crate::logical::restyle_comment(c, "#") {
            out.push_str(&line);
            out.push('\n');
        }
    }
    Ok(EmitOutput {
        text: out,
        warnings,
    })
}

fn escape_key(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for c in key.chars() {
        match c {
            ' ' => out.push_str("\\ "),
            '=' => out.push_str("\\="),
            ':' => out.push_str("\\:"),
            '#' => out.push_str("\\#"),
            '!' => out.push_str("\\!"),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

fn prop_string(node: &Node) -> Result<String, Error> {
    match &node.value {
        Value::Null => Ok(String::new()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Number(n) => Ok(n.raw.clone()),
        Value::Str(s) => Ok(escape_value(s)),
        Value::Datetime(d) => Ok(escape_value(d)),
        Value::Array(_) | Value::Map(_) => Err(Error::emit(
            "nested values have no properties representation",
        )),
        _ => Err(Error::emit("unexpected value kind in properties output")),
    }
}

fn escape_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgprism_core::values_equal;

    fn roundtrip(src: &str) -> Doc {
        let fmt = PropertiesFormat;
        let doc = fmt.parse(src).expect("parse");
        let out = fmt.emit(&doc, &Options::default()).expect("emit");
        assert_eq!(out.text, src, "byte round-trip failed");
        assert!(out.warnings.is_empty());
        fmt.parse(&out.text).expect("re-parse")
    }

    #[test]
    fn separators_escapes_continuations_round_trip() {
        let src = "# head\n! bang comment\n\nkey = value\ncolon: v\nspace v2\nescaped\\ key = a\\nb\nuni = caf\\u00e9\ntrail = one \\\n  two\nempty=\nbare\n";
        let a = PropertiesFormat.parse(src).expect("parse");
        let b = roundtrip(src);
        assert!(values_equal(&a.root, &b.root));
        assert_eq!(
            a.root.get("uni").expect("uni").value,
            Value::Str("café".to_string())
        );
        assert_eq!(
            a.root.get("trail").expect("trail").value,
            Value::Str("one two".to_string())
        );
        assert_eq!(
            a.root.get("escaped key").expect("escaped").value,
            Value::Str("a\nb".to_string())
        );
    }
}
