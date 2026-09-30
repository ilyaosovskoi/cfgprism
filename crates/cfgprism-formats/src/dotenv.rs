//! `.env` files: `KEY=value` lines with `export` prefixes, quotes and `#`
//! comments. Each entry keeps its verbatim line parts (`key.raw`, `sep_raw`,
//! `Style::Original` value, `suffix_raw` incl. newline), so self round-trip
//! is byte-identical; logical values/comments feed cross-format conversion.
//!
//! Supported line shapes (verified by fixtures):
//! `KEY=val`, `KEY = val`, `export KEY=val`, `KEY="dq …"`, `KEY='sq …'`,
//! `KEY=` (empty), bare `KEY`, full-line `#` comments, blank lines,
//! trailing/leading whitespace, `\n` or `\r\n` endings.

use cfgprism_core::{
    offset_to_line_col, Doc, EmitOutput, Entry, Error, Format, Key, Node, Options, Style, Trivia,
    Value, Warning, WarningKind,
};

use crate::util::{line_starts, split_lines};

/// `.env` / `*.env` files.
pub struct DotenvFormat;

/// Decode a raw value slice into its logical text.
/// Returns `(logical, verbatim_style_raw)`.
fn decode_value(raw: &str) -> (String, String) {
    let t = raw.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        (unescape_double(&t[1..t.len() - 1]), raw.to_string())
    } else if t.len() >= 2 && t.starts_with('\'') && t.ends_with('\'') {
        (t[1..t.len() - 1].to_string(), raw.to_string())
    } else {
        (t.to_string(), raw.to_string())
    }
}

fn unescape_double(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
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
            Some('$') => out.push('$'),
            Some(' ') => out.push(' '),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Split `content` into value part + inline comment.
/// `#` starts a comment only at line start or after whitespace.
fn split_inline(content: &str) -> (&str, Option<&str>) {
    let mut in_single = false;
    let mut in_double = false;
    let bytes = content.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'\\' if in_double => {
                i += 1;
            }
            b'#' if !in_single && !in_double => {
                if i == 0 || bytes[i - 1] == b' ' || bytes[i - 1] == b'\t' {
                    return (content[..i].trim_end(), Some(content[i..].trim_end()));
                }
            }
            _ => {}
        }
        i += 1;
    }
    (content.trim_end(), None)
}

/// Extract the logical key text from a verbatim key part
/// (`export FOO` → `FOO`).
fn logical_key(key_raw: &str) -> String {
    let t = key_raw.trim();
    t.strip_prefix("export")
        .map_or_else(|| t.to_string(), |r| r.trim().to_string())
}

/// Parse `.env` source into an IR document (root is always a Map).
pub fn parse_dotenv(src: &str) -> Result<Doc, Error> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut pending_prefix = String::new();
    let mut pending_leading: Vec<String> = Vec::new();
    let mut pending_blanks = 0_usize;

    // Byte offset of each RawLine within src (for error positions).
    let starts = line_starts(src);

    for (idx, line) in split_lines(src).iter().enumerate() {
        let line_off = starts[idx];
        let trimmed = line.content.trim();
        let raw_prefix_line = format!("{}{}", line.content, line.ending);
        if trimmed.is_empty() {
            pending_prefix.push_str(&raw_prefix_line);
            pending_blanks += 1;
            continue;
        }
        if trimmed.starts_with('#') {
            pending_prefix.push_str(&raw_prefix_line);
            pending_leading.push(trimmed.to_string());
            continue;
        }
        // Entry line: find `=` outside quotes.
        let eq = find_unquoted_eq(line.content);
        let Some(eq) = eq else {
            // Bare `KEY` (no `=`): value is empty, separator is empty.
            let key_raw = line.content.trim().to_string();
            let entry = Entry {
                key: Key {
                    text: logical_key(&key_raw),
                    repr: None,
                    raw: Some(key_raw),
                },
                key_trivia: Trivia {
                    leading: std::mem::take(&mut pending_leading),
                    inline: None,
                    blanks_before: std::mem::replace(&mut pending_blanks, 0),
                    prefix_raw: Some(std::mem::take(&mut pending_prefix)),
                    suffix_raw: None,
                },
                sep_raw: Some(String::new()),
                value: Node {
                    value: Value::Str(String::new()),
                    trivia: Trivia {
                        suffix_raw: Some(line.ending.to_string()),
                        ..Trivia::empty()
                    },
                    style: Style::Plain,
                    span: None,
                    anchor: None,
                    order: entries.len(),
                    open_raw: None,
                    close_raw: None,
                },
            };
            entries.push(entry);
            continue;
        };
        let key_part = &line.content[..eq];
        let after_eq = &line.content[eq + 1..];
        if logical_key(key_part).is_empty() {
            let lc = offset_to_line_col(src, line_off);
            return Err(Error::parse(lc.line, lc.col, "missing key before '='"));
        }
        // Validate key characters loosely (anything but quotes/whitespace runs).
        if logical_key(key_part).contains(['"', '\'']) {
            let lc = offset_to_line_col(src, line_off);
            return Err(Error::parse(lc.line, lc.col, "invalid key"));
        }
        let (val_part, comment) = split_inline(after_eq);
        let (logical, _) = decode_value(val_part);
        // Exact value slice: from after `=` (past left whitespace) to the end
        // of the trimmed value part; everything after goes to the suffix.
        let val_start_in_after = after_eq.len() - after_eq.trim_start().len();
        let vbegin = eq + 1 + val_start_in_after;
        let vend = (vbegin + val_part.trim_start().len()).min(line.content.len());
        let value_raw = line.content[vbegin..vend].to_string();
        let suffix_raw = format!("{}{}", &line.content[vend..], line.ending);
        let entry = Entry {
            key: Key {
                text: logical_key(key_part),
                repr: None,
                raw: Some(key_part.to_string()),
            },
            key_trivia: Trivia {
                leading: std::mem::take(&mut pending_leading),
                inline: None,
                blanks_before: std::mem::replace(&mut pending_blanks, 0),
                prefix_raw: Some(std::mem::take(&mut pending_prefix)),
                suffix_raw: None,
            },
            sep_raw: Some(line.content[eq..vbegin].to_string()),
            value: Node {
                value: Value::Str(logical),
                trivia: Trivia {
                    inline: comment.map(str::to_string),
                    suffix_raw: Some(suffix_raw),
                    ..Trivia::empty()
                },
                style: Style::Original(value_raw),
                span: None,
                anchor: None,
                order: entries.len(),
                open_raw: None,
                close_raw: None,
            },
        };
        entries.push(entry);
    }

    let mut root = Node::new(Value::Map(entries));
    root.order = 0;
    let mut doc = Doc::new(root);
    // Anything left pending is the file tail.
    doc.trailing.leading = pending_leading;
    doc.trailing.prefix_raw = Some(pending_prefix);
    Ok(doc)
}

/// Byte offset of the first unquoted `=` in a line, if any.
fn find_unquoted_eq(line: &str) -> Option<usize> {
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if in_double => escaped = true,
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '=' if !in_single && !in_double => return Some(i),
            _ => {}
        }
    }
    None
}

/// Emit a dotenv document: verbatim when slices exist, canonical otherwise
/// (with `CommentDropped` warnings for homeless comments).
pub fn emit_dotenv(doc: &Doc, _opt: &Options) -> Result<EmitOutput, Error> {
    let Value::Map(entries) = &doc.root.value else {
        return Err(Error::emit("dotenv root must be a map"));
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
                    "dotenv values must be strings (key '{}')",
                    e.key.text
                )));
            }
        }
        if let Some(s) = e.value.trivia.suffix_raw.as_deref() {
            out.push_str(s);
        } else {
            if e.value.trivia.inline.is_some() {
                warnings.push(Warning::new(
                    &e.key.text,
                    WarningKind::CommentDropped,
                    "inline comment has no verbatim slice; dropped",
                ));
            }
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

impl Format for DotenvFormat {
    fn name(&self) -> &'static str {
        "dotenv"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["env"]
    }

    fn parse(&self, src: &str) -> Result<Doc, Error> {
        parse_dotenv(src)
    }

    fn emit(&self, doc: &Doc, opt: &Options) -> Result<EmitOutput, Error> {
        emit_dotenv(doc, opt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgprism_core::values_equal;

    fn roundtrip(src: &str) -> Doc {
        let fmt = DotenvFormat;
        let doc = fmt.parse(src).expect("parse");
        let out = fmt.emit(&doc, &Options::default()).expect("emit");
        assert_eq!(out.text, src, "byte round-trip failed");
        assert!(out.warnings.is_empty());
        fmt.parse(&out.text).expect("re-parse")
    }

    #[test]
    fn kitchen_sink_round_trip() {
        let src = "# head\n\nexport FOO=bar # inline\nEMPTY=\nDQ=\"a\\n b\" # c\nSQ='x#y'\nHASH=abc#def\nBARE\nCRLF=1\r\n";
        let a = DotenvFormat.parse(src).expect("parse");
        let b = roundtrip(src);
        assert!(values_equal(&a.root, &b.root));
        assert_eq!(
            a.root_keys_in_order(),
            vec!["FOO", "EMPTY", "DQ", "SQ", "HASH", "BARE", "CRLF"]
        );
        assert_eq!(
            a.root.get("FOO").expect("FOO").trivia.inline.as_deref(),
            Some("# inline")
        );
        assert_eq!(
            a.root.get("DQ").expect("DQ").value,
            Value::Str("a\n b".to_string())
        );
        assert_eq!(
            a.root.get("HASH").expect("HASH").value,
            Value::Str("abc#def".to_string())
        );
    }

    #[test]
    fn tail_comments_preserved() {
        let src = "A=1\n# tail\n\n";
        roundtrip(src);
    }

    #[test]
    fn missing_key_is_an_error_with_position() {
        let err = DotenvFormat.parse("A=1\n=2\n").expect_err("must fail");
        assert_eq!(err.pos.line, 2);
    }
}
