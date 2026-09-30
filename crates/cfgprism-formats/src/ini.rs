//! INI files: `[section]` headers, `key = value` / `key: value` entries,
//! `;` and `#` comments, blank lines, and indented continuation lines.
//!
//! Model: the document root is a Map. Global entries (before the first
//! section) are plain KV entries; each section is an entry whose value is a
//! nested Map with `open_raw` holding the verbatim header line. Duplicate
//! keys and duplicate sections are preserved in order (the `Vec` model
//! allows them); cross-format conversion reports them (Stage 4).
//!
//! Every line keeps its verbatim slices, so self round-trip is
//! byte-identical.

use cfgprism_core::{
    offset_to_line_col, Doc, EmitOutput, Entry, Error, Format, Key, Node, Options, Style, Trivia,
    Value, Warning, WarningKind,
};

use crate::util::{line_starts, split_lines};

/// INI / `*.ini` / `*.cfg` / `*.conf` files.
pub struct IniFormat;

/// A `[section]` header: `(verbatim_inner_name, inline_comment)`.
/// The header may carry a trailing `;`/`#` comment; anything else after `]`
/// is rejected by the caller.
fn section_header(trimmed: &str) -> Option<(&str, Option<&str>)> {
    let inner = trimmed.strip_prefix('[')?;
    let close = inner.find(']')?;
    let (name, after) = (&inner[..close], inner[close + 1..].trim());
    if name.trim().is_empty() {
        return None;
    }
    if after.is_empty() {
        return Some((name, None));
    }
    if after.starts_with([';', '#']) {
        return Some((name, Some(after)));
    }
    None
}

/// Find the separator (`=` or `:`) outside quotes. Returns byte index.
fn find_sep(line: &str) -> Option<usize> {
    let mut in_single = false;
    let mut in_double = false;
    for (i, c) in line.char_indices() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '=' | ':' if !in_single && !in_double => return Some(i),
            _ => {}
        }
    }
    None
}

/// Split off an inline comment (`;` or `#` after whitespace / at start,
/// outside quotes).
fn split_inline(content: &str) -> (&str, Option<&str>) {
    let mut in_single = false;
    let mut in_double = false;
    let bytes = content.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b';' | b'#' if !in_single && !in_double => {
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

fn is_comment_line(trimmed: &str) -> bool {
    trimmed.starts_with(';') || trimmed.starts_with('#')
}

/// Pending trivia accumulator: blank/comment lines before the next entry.
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

/// A section whose entries are still being collected.
struct OpenSection {
    key: Key,
    key_trivia: Trivia,
    open_raw: String,
    inline: Option<String>,
    entries: Vec<Entry>,
}

/// Where an indented continuation line attaches.
#[derive(Clone, Copy)]
enum ContTarget {
    Root,
    Section,
}

/// Parse INI source into an IR document.
pub fn parse_ini(src: &str) -> Result<Doc, Error> {
    let starts = line_starts(src);
    let lines = split_lines(src);
    let mut root_entries: Vec<Entry> = Vec::new();
    let mut current: Option<OpenSection> = None;
    let mut pending = Pending::default();
    let mut cont: Option<ContTarget> = None;

    for (idx, line) in lines.iter().enumerate() {
        let line_off = starts[idx];
        let trimmed = line.content.trim();
        let raw_line = format!("{}{}", line.content, line.ending);
        let indented = line.content.starts_with(' ') || line.content.starts_with('\t');

        // Continuation: indented, non-comment, separator-free line right
        // after an entry (blank lines and headers break the chain).
        if indented
            && cont.is_some()
            && !trimmed.is_empty()
            && !is_comment_line(trimmed)
            && section_header(trimmed).is_none()
            && find_sep(line.content).is_none()
        {
            let entries = match (&mut current, cont) {
                (Some(sec), Some(ContTarget::Section)) => &mut sec.entries,
                (_, Some(ContTarget::Root)) => &mut root_entries,
                _ => &mut root_entries,
            };
            if let Some(last) = entries.last_mut() {
                if let Value::Str(s) = &mut last.value.value {
                    s.push('\n');
                    s.push_str(trimmed);
                }
                let suf = last.value.trivia.suffix_raw.take().unwrap_or_default();
                last.value.trivia.suffix_raw = Some(format!("{suf}{raw_line}"));
                continue;
            }
        }

        if trimmed.is_empty() {
            pending.raw.push_str(&raw_line);
            pending.blanks += 1;
            cont = None;
            continue;
        }
        if is_comment_line(trimmed) {
            pending.raw.push_str(&raw_line);
            pending.leading.push(trimmed.to_string());
            cont = None;
            continue;
        }
        if let Some((name, inline)) = section_header(trimmed) {
            flush_section(&mut root_entries, &mut current);
            let (praw, plead, pblanks) = pending.take();
            current = Some(OpenSection {
                key: Key {
                    text: name.trim().to_string(),
                    repr: None,
                    raw: Some(name.to_string()),
                },
                key_trivia: Trivia {
                    leading: plead,
                    inline: None,
                    blanks_before: pblanks,
                    prefix_raw: Some(praw),
                    suffix_raw: None,
                },
                open_raw: raw_line,
                inline: inline.map(str::to_string),
                entries: Vec::new(),
            });
            cont = None;
            continue;
        }
        // Entry line.
        let Some(sep_idx) = find_sep(line.content) else {
            // Could be `[section] garbage` — report precisely.
            let lc = offset_to_line_col(src, line_off);
            if line.content.trim_start().starts_with('[') {
                return Err(Error::parse(lc.line, lc.col, "invalid section header"));
            }
            return Err(Error::parse(
                lc.line,
                lc.col,
                "expected 'key = value' or '[section]'",
            ));
        };
        let key_part = &line.content[..sep_idx];
        let after_sep = &line.content[sep_idx + 1..];
        // key.raw keeps leading indent but not trailing space (which belongs
        // to the separator); sep covers key-end → value-start verbatim.
        let key_text = key_part.trim();
        if key_text.is_empty() {
            let lc = offset_to_line_col(src, line_off);
            return Err(Error::parse(
                lc.line,
                lc.col,
                "missing key before separator",
            ));
        }
        let key_trailing_ws = key_part.len() - key_part.trim_end().len();
        let (val_part, comment) = split_inline(after_sep);
        let vbegin_off = sep_idx + 1 + (after_sep.len() - after_sep.trim_start().len());
        let vend_off = (vbegin_off + val_part.trim_start().len()).min(line.content.len());
        let value_raw = line.content[vbegin_off..vend_off].to_string();
        let suffix_raw = format!("{}{}", &line.content[vend_off..], line.ending);
        let (praw, plead, pblanks) = pending.take();
        let entry = Entry {
            key: Key {
                text: key_text.to_string(),
                repr: None,
                raw: Some(key_part.trim_end().to_string()),
            },
            key_trivia: Trivia {
                leading: plead,
                inline: None,
                blanks_before: pblanks,
                prefix_raw: Some(praw),
                suffix_raw: None,
            },
            sep_raw: Some(line.content[key_part.len() - key_trailing_ws..vbegin_off].to_string()),
            value: Node {
                value: Value::Str(val_part.trim().to_string()),
                trivia: Trivia {
                    inline: comment.map(str::to_string),
                    suffix_raw: Some(suffix_raw),
                    ..Trivia::empty()
                },
                style: Style::Original(value_raw),
                span: None,
                anchor: None,
                order: 0,
                open_raw: None,
                close_raw: None,
            },
        };
        let in_section = current.is_some();
        match &mut current {
            Some(sec) => sec.entries.push(entry),
            None => root_entries.push(entry),
        }
        cont = Some(if in_section {
            ContTarget::Section
        } else {
            ContTarget::Root
        });
    }
    flush_section(&mut root_entries, &mut current);

    let mut root = Node::new(Value::Map(root_entries));
    root.order = 0;
    let mut doc = Doc::new(root);
    let (praw, plead, _) = pending.take();
    doc.trailing.leading = plead;
    doc.trailing.prefix_raw = Some(praw);
    Ok(doc)
}

fn flush_section(root: &mut Vec<Entry>, current: &mut Option<OpenSection>) {
    if let Some(sec) = current.take() {
        let mut map = Node::new(Value::Map(sec.entries));
        map.open_raw = Some(sec.open_raw);
        if let Some(c) = sec.inline {
            map.trivia.inline = Some(c);
        }
        let order = root.len();
        map.order = order;
        root.push(Entry {
            key: sec.key,
            key_trivia: sec.key_trivia,
            sep_raw: None,
            value: map,
        });
    }
}

/// Emit an INI document: verbatim when slices exist, canonical otherwise.
pub fn emit_ini(doc: &Doc, _opt: &Options) -> Result<EmitOutput, Error> {
    let Value::Map(entries) = &doc.root.value else {
        return Err(Error::emit("ini root must be a map"));
    };
    let mut warnings = Vec::new();
    let mut out = doc.root.trivia.prefix_raw.clone().unwrap_or_default();
    for e in entries {
        emit_entry(&mut out, &mut warnings, "", e)?;
    }
    if let Some(tail) = doc.trailing.prefix_raw.as_deref() {
        out.push_str(tail);
    } else {
        for c in &doc.trailing.leading {
            out.push_str(c);
            out.push('\n');
        }
    }
    Ok(EmitOutput {
        text: out,
        warnings,
    })
}

/// Emit one entry: verbatim/logical prefix, then section or KV body.
fn emit_entry(
    out: &mut String,
    warnings: &mut Vec<Warning>,
    path: &str,
    e: &Entry,
) -> Result<(), Error> {
    if let Some(p) = e.key_trivia.prefix_raw.as_deref() {
        out.push_str(p);
    } else {
        for _ in 0..e.key_trivia.blanks_before {
            out.push('\n');
        }
        for c in &e.key_trivia.leading {
            out.push_str(c);
            out.push('\n');
        }
        if e.key_trivia.inline.is_some() {
            warnings.push(Warning::new(
                if path.is_empty() {
                    e.key.text.clone()
                } else {
                    path.to_string()
                },
                WarningKind::CommentDropped,
                "inline comment has no verbatim slice in this output; dropped",
            ));
        }
    }
    if let Value::Map(children) = &e.value.value {
        if let Some(open) = e.value.open_raw.as_deref() {
            out.push_str(open);
        } else {
            out.push('[');
            out.push_str(&e.key.text);
            out.push_str("]\n");
            if e.value.trivia.inline.is_some() {
                warnings.push(Warning::new(
                    &e.key.text,
                    WarningKind::CommentDropped,
                    "section inline comment has no verbatim slice; dropped",
                ));
            }
        }
        for c in children {
            // Child prefix was not emitted yet (the section header path
            // above only wrote the header line itself).
            if let Some(p) = c.key_trivia.prefix_raw.as_deref() {
                out.push_str(p);
            } else {
                for _ in 0..c.key_trivia.blanks_before {
                    out.push('\n');
                }
                for line in &c.key_trivia.leading {
                    out.push_str(line);
                    out.push('\n');
                }
                if c.key_trivia.inline.is_some() {
                    warnings.push(Warning::new(
                        format!("{}.{}", e.key.text, c.key.text),
                        WarningKind::CommentDropped,
                        "inline comment has no verbatim slice; dropped",
                    ));
                }
            }
            emit_kv(out, warnings, &e.key.text, c)?;
        }
        return Ok(());
    }
    emit_kv(out, warnings, path, e)
}

/// Emit a plain `key sep value suffix` body (prefix handled by the caller).
fn emit_kv(
    out: &mut String,
    warnings: &mut Vec<Warning>,
    path: &str,
    e: &Entry,
) -> Result<(), Error> {
    out.push_str(e.key.raw.as_deref().unwrap_or(&e.key.text));
    out.push_str(e.sep_raw.as_deref().unwrap_or(" = "));
    let Value::Str(s) = &e.value.value else {
        return Err(Error::emit(format!(
            "ini value must be a string (key '{}')",
            e.key.text
        )));
    };
    if let Style::Original(raw) = &e.value.style {
        out.push_str(raw);
    } else {
        out.push_str(s);
    }
    if let Some(suf) = e.value.trivia.suffix_raw.as_deref() {
        out.push_str(suf);
    } else {
        if e.value.trivia.inline.is_some() {
            warnings.push(Warning::new(
                if path.is_empty() {
                    e.key.text.clone()
                } else {
                    format!("{path}.{}", e.key.text)
                },
                WarningKind::CommentDropped,
                "inline comment has no verbatim slice; dropped",
            ));
        }
        out.push('\n');
    }
    Ok(())
}

impl Format for IniFormat {
    fn name(&self) -> &'static str {
        "ini"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["ini", "cfg", "conf"]
    }

    fn parse(&self, src: &str) -> Result<Doc, Error> {
        parse_ini(src)
    }

    fn emit(&self, doc: &Doc, opt: &Options) -> Result<EmitOutput, Error> {
        emit_ini(doc, opt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgprism_core::values_equal;

    fn roundtrip(src: &str) -> Doc {
        let fmt = IniFormat;
        let doc = fmt.parse(src).expect("parse");
        let out = fmt.emit(&doc, &Options::default()).expect("emit");
        assert_eq!(out.text, src, "byte round-trip failed");
        assert!(out.warnings.is_empty());
        fmt.parse(&out.text).expect("re-parse")
    }

    #[test]
    fn sections_comments_continuations_round_trip() {
        let src = "; global comment\nkey = val ; inline\n\n[sec] ; head\n# about a\na: 1\nmulti = one\n  two\n[sec]\n# second block\ndup = 1\n";
        let a = IniFormat.parse(src).expect("parse");
        let b = roundtrip(src);
        assert!(values_equal(&a.root, &b.root));
        assert_eq!(
            a.root.get("key").expect("global").value,
            Value::Str("val".to_string())
        );
        let sec = a.root.get("sec").expect("sec");
        assert_eq!(
            sec.get("multi").expect("multi").value,
            Value::Str("one\ntwo".to_string())
        );
        // Pre-key comments live on entry key trivia (see json.rs convention).
        let Value::Map(sec_entries) = &sec.value else {
            panic!("sec must be a map");
        };
        let a_entry = sec_entries
            .iter()
            .find(|e| e.key.text == "a")
            .expect("a entry");
        assert_eq!(a_entry.key_trivia.leading, vec!["# about a".to_string()]);
    }

    #[test]
    fn garbage_line_is_an_error_with_position() {
        let err = IniFormat
            .parse("[s]\nno separator here\n")
            .expect_err("must fail");
        assert_eq!(err.pos.line, 2);
    }

    #[test]
    fn bad_header_is_an_error() {
        assert!(IniFormat.parse("[s] garbage\n").is_err());
    }
}
