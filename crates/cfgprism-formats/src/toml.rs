//! TOML via `toml_edit`: the crate preserves comments/whitespace/order and
//! exposes them as `decor` (exact prefix/suffix strings) and `repr` (exact
//! value spellings). This adapter maps those onto IR verbatim slices, so
//! self round-trip is byte-identical for canonical documents.
//!
//! Mapping rules:
//! - KV `key = value`: prefix = key leaf prefix; `sep = key suffix + "=" +
//!   value prefix`; scalar = `repr` verbatim (`Style::Original`); suffix =
//!   value suffix (same line). Logical comments/blank counts are extracted
//!   from the same slices for cross-format use.
//! - `[tbl]` / `[[aot]]`: prefix = table decor prefix (full gap); header is
//!   reconstructed as `[dotted.path]` / `[[dotted.path]]` with the leaf
//!   key's leaf prefix/suffix as the inner bracket spacing; suffix = table
//!   decor suffix (inline comment). Empty stored prefixes emit one automatic
//!   `"\n"` (matching `toml_edit`); the file tail is `"\n"` iff the source
//!   ends with a newline.
//! - Dotted keys (`a.b = 1`, `a . b = 1`) flatten into single entries whose
//!   key raw is rebuilt from per-level raws plus `dotted_decor` spacing.
//! - Implicit header-parents (`[a.b]`'s `a`) are transparent containers.
//! - Known limitations (see `DECISIONS.md` D13): exotic header bracket
//!   spacing beyond leaf gaps is canonicalized; parent-after-child table
//!   order is normalized; 2+ trailing newlines collapse to one.

use cfgprism_core::{
    offset_to_line_col, Doc, EmitOutput, Entry, Error, Format, Key, Node, Number, NumberKind,
    Options, Style, Trivia, Value, Warning, WarningKind,
};
use toml_edit::{Decor, DocumentMut, Item, RawString, Value as TomlValue};

/// TOML format (`*.toml`).
pub struct TomlFormat;

/// Exact text of an optional raw string (`""` when absent/default).
fn rs(raw: Option<&RawString>) -> &str {
    raw.and_then(|r| r.as_str()).unwrap_or("")
}

fn prefix(d: &Decor) -> &str {
    rs(d.prefix())
}

fn suffix(d: &Decor) -> &str {
    rs(d.suffix())
}

/// Logical `#` comments inside a gap. Gaps never contain quoted strings
/// (those live in key/value raws), so a naive `#`→EOL scan is safe.
fn extract_hash_comments(gap: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in gap.split('\n') {
        if let Some(i) = line.find('#') {
            out.push(line[i..].trim_end().to_string());
        }
    }
    out
}

/// Count interior blank lines (shared semantics with the JSON engine).
fn count_blanks(gap: &str) -> usize {
    let lines: Vec<&str> = gap.split('\n').collect();
    if lines.len() < 2 {
        return 0;
    }
    lines[1..lines.len() - 1]
        .iter()
        .filter(|l| l.trim().is_empty())
        .count()
}

fn toml_error(src: &str, e: toml_edit::TomlError) -> Error {
    let (line, col) = e
        .span()
        .map(|r| {
            let lc = offset_to_line_col(src, r.start);
            (lc.line, lc.col)
        })
        .unwrap_or((1, 1));
    let msg = e.to_string();
    let first = msg
        .lines()
        .next()
        .unwrap_or("invalid TOML")
        .trim()
        .to_string();
    Error::parse(line, col, first)
}

/// Per-level dotted-path segment: key raw + spacing around the following dot.
#[derive(Clone)]
struct PathSeg {
    raw: String,
    /// Whitespace between this key and the `.` after it.
    dot_suffix: String,
    /// Whitespace between the preceding `.` and this key.
    dot_prefix: String,
}

fn key_raw(key: &toml_edit::Key, decoded: &str) -> String {
    key.as_repr()
        .and_then(|r| r.as_raw().as_str())
        .unwrap_or(decoded)
        .to_string()
}

struct Ctx {
    order: usize,
}

impl Ctx {
    fn next(&mut self) -> usize {
        let o = self.order;
        self.order += 1;
        o
    }
}

/// Convert one `(key, item)` pair; dotted tables flatten into several entries.
fn convert_item(
    ctx: &mut Ctx,
    key_text: &str,
    key: &toml_edit::Key,
    item: &Item,
    parent: &[PathSeg],
    is_first: bool,
) -> Result<Vec<Entry>, Error> {
    match item {
        Item::None => Ok(Vec::new()),
        Item::Value(v) => {
            let pre_gap = prefix(key.leaf_decor());
            let vpre = prefix(v.decor()).to_string();
            let mut value = convert_value(ctx, v)?;
            value.trivia.prefix_raw = None; // sep covers the key→value gap
            let path_raw = render_path(parent, key_text, key);
            Ok(vec![Entry {
                key: Key {
                    text: dotted_text(parent, key_text),
                    repr: None,
                    raw: Some(path_raw),
                },
                key_trivia: trivia_of(pre_gap, is_first),
                sep_raw: Some(format!("{}={}", suffix(key.leaf_decor()), vpre)),
                value,
            }])
        }
        Item::Table(t) => {
            if t.is_implicit() && t.is_dotted() {
                // Dotted-key parent (`a.b = 1`): flatten each child with an
                // extended path. The head gap lives on the LEAF key.
                let seg = PathSeg {
                    raw: key_raw(key, key_text),
                    dot_suffix: suffix(key.dotted_decor()).to_string(),
                    dot_prefix: prefix(key.dotted_decor()).to_string(),
                };
                let mut out = Vec::new();
                let mut sub: Vec<(&str, &Item)> = Vec::new();
                for (sk, sitem) in t.iter() {
                    sub.push((sk, sitem));
                }
                for (i, (sk, sitem)) in sub.iter().enumerate() {
                    let (skk, _) = t.get_key_value(sk).expect("iterated key exists");
                    let mut parent2 = parent.to_vec();
                    parent2.push(PathSeg {
                        raw: seg.raw.clone(),
                        dot_suffix: seg.dot_suffix.clone(),
                        dot_prefix: seg.dot_prefix.clone(),
                    });
                    let mut es = convert_item(ctx, sk, skk, sitem, &parent2, is_first && i == 0)?;
                    out.append(&mut es);
                }
                return Ok(out);
            }
            // Real table: explicit header or transparent implicit parent.
            let table_prefix = prefix(t.decor());
            if t.is_implicit() {
                // Transparent (`[a.b]`'s `a`): no header of its own.
                let mut out = Vec::new();
                for (sk, sitem) in t.iter() {
                    let (skk, _) = t.get_key_value(sk).expect("iterated key exists");
                    let seg = PathSeg {
                        raw: key_raw(key, key_text),
                        dot_suffix: suffix(key.dotted_decor()).to_string(),
                        dot_prefix: prefix(key.dotted_decor()).to_string(),
                    };
                    let mut parent2 = parent.to_vec();
                    parent2.push(seg);
                    let mut es = convert_item(ctx, sk, skk, sitem, &parent2, false)?;
                    out.append(&mut es);
                }
                // Transparent wrappers keep no slices of their own; the gap
                // before the first real header lives on that header.
                let _ = table_prefix;
                return Ok(out);
            }
            // Explicit `[path]` header.
            let header = render_header(parent, key_text, key, false);
            let mut children = Vec::new();
            for (sk, sitem) in t.iter() {
                let (skk, _) = t.get_key_value(sk).expect("iterated key exists");
                let mut es = convert_item(ctx, sk, skk, sitem, &[], false)?;
                children.append(&mut es);
            }
            let mut map = Node::new(Value::Map(children));
            map.open_raw = Some(header);
            map.order = ctx.next();
            map.trivia.suffix_raw = Some(suffix(t.decor()).to_string());
            if let Some(c) = inline_of(suffix(t.decor())) {
                map.trivia.inline = Some(c);
            }
            Ok(vec![Entry {
                key: Key {
                    text: key_text.to_string(),
                    repr: None,
                    raw: Some(key_raw(key, key_text)),
                },
                key_trivia: trivia_of(table_prefix, is_first),
                sep_raw: None,
                value: map,
            }])
        }
        Item::ArrayOfTables(aot) => {
            let mut elements = Vec::new();
            for inner in aot.iter() {
                let header = render_header(parent, key_text, key, true);
                let mut children = Vec::new();
                for (sk, sitem) in inner.iter() {
                    let (skk, _) = inner.get_key_value(sk).expect("iterated key exists");
                    let mut es = convert_item(ctx, sk, skk, sitem, &[], false)?;
                    children.append(&mut es);
                }
                let mut map = Node::new(Value::Map(children));
                map.open_raw = Some(header);
                map.order = ctx.next();
                map.trivia.suffix_raw = Some(suffix(inner.decor()).to_string());
                if let Some(c) = inline_of(suffix(inner.decor())) {
                    map.trivia.inline = Some(c);
                }
                map.trivia.prefix_raw = Some(prefix(inner.decor()).to_string());
                fill_logical(&mut map.trivia, prefix(inner.decor()));
                elements.push(map);
            }
            Ok(vec![Entry {
                key: Key {
                    text: key_text.to_string(),
                    repr: None,
                    raw: Some(key_raw(key, key_text)),
                },
                key_trivia: trivia_of(aot_first_prefix(aot), is_first),
                sep_raw: None,
                value: {
                    let mut arr = Node::new(Value::Array(elements));
                    arr.order = ctx.next();
                    arr
                },
            }])
        }
    }
}

/// Prefix gap before the first AoT element doubles as the entry prefix.
fn aot_first_prefix(aot: &toml_edit::ArrayOfTables) -> &str {
    aot.iter().next().map(|t| prefix(t.decor())).unwrap_or("")
}

/// Render `[dotted.path]` / `[[dotted.path]]`, or the flattened dotted key.
/// Bracket spacing comes from the leaf key's leaf decor
/// (probed: `[ a.b ]` stores `" "`/`" "` there).
fn render_header(parent: &[PathSeg], key_text: &str, key: &toml_edit::Key, is_aot: bool) -> String {
    let path = render_path(parent, key_text, key);
    let (pre, suf) = leaf_bracket_gaps(key);
    if is_aot {
        format!("[[{pre}{path}{suf}]]")
    } else {
        format!("[{pre}{path}{suf}]")
    }
}

fn leaf_bracket_gaps(key: &toml_edit::Key) -> (String, String) {
    (
        prefix(key.leaf_decor()).to_string(),
        suffix(key.leaf_decor()).to_string(),
    )
}

/// Full dotted path raw: parent segs + this key.
fn render_path(parent: &[PathSeg], key_text: &str, key: &toml_edit::Key) -> String {
    let mut out = String::new();
    for seg in parent {
        out.push_str(&seg.raw);
        out.push_str(&seg.dot_suffix);
        out.push('.');
        out.push_str(&seg.dot_prefix);
    }
    // Own leading-dot gap: this key's dotted prefix ("" at top level).
    let own_pre = if parent.is_empty() {
        String::new()
    } else {
        prefix(key.dotted_decor()).to_string()
    };
    out.push_str(&own_pre);
    out.push_str(&key_raw(key, key_text));
    out
}

/// Logical dotted text (`a.b.c`), spacing stripped.
fn dotted_text(parent: &[PathSeg], key_text: &str) -> String {
    let mut parts: Vec<&str> = parent.iter().map(|s| s.raw.as_str()).collect();
    parts.push(key_text);
    // Raws may be quoted ('a'); strip one layer of quotes for logical text.
    parts
        .iter()
        .map(|p| unquote_key(p))
        .collect::<Vec<_>>()
        .join(".")
}

fn unquote_key(raw: &str) -> String {
    let t = raw.trim();
    if t.len() >= 2
        && ((t.starts_with('"') && t.ends_with('"')) || (t.starts_with('\'') && t.ends_with('\'')))
    {
        // Best-effort unescape for logical use; verbatim stays in `raw`.
        t[1..t.len() - 1].to_string()
    } else {
        t.to_string()
    }
}

fn trivia_of(gap: &str, _is_first: bool) -> Trivia {
    let mut t = Trivia::empty();
    t.leading = extract_hash_comments(gap);
    t.blanks_before = count_blanks(gap);
    t.prefix_raw = Some(gap.to_string());
    t
}

fn fill_logical(t: &mut Trivia, gap: &str) {
    t.leading = extract_hash_comments(gap);
    t.blanks_before = count_blanks(gap);
}

fn inline_of(same_line: &str) -> Option<String> {
    if let Some(i) = same_line.find('#') {
        return Some(same_line[i..].trim_end().to_string());
    }
    None
}

/// Convert a TOML value into a complete IR node (verbatim slices included).
/// The caller clears `prefix_raw` for KV positions (the separator covers the
/// key→value gap).
fn convert_value(ctx: &mut Ctx, v: &TomlValue) -> Result<Node, Error> {
    let pre = prefix(v.decor()).to_string();
    let suf = suffix(v.decor()).to_string();
    let raw_of = |r: Option<&toml_edit::Repr>| -> Option<String> {
        r.and_then(|x| x.as_raw().as_str()).map(str::to_string)
    };
    let mut node = match v {
        TomlValue::String(f) => {
            let raw = raw_of(f.as_repr()).unwrap_or_else(|| format!("\"{}\"", f.value()));
            let mut n = Node::new(Value::Str(f.value().clone()));
            n.style = Style::Original(raw);
            n
        }
        TomlValue::Integer(f) => {
            let raw = raw_of(f.as_repr()).unwrap_or_else(|| f.value().to_string());
            let mut n = Node::new(Value::Number(Number {
                raw: raw.clone(),
                kind: NumberKind::Int,
            }));
            n.style = Style::Original(raw);
            n
        }
        TomlValue::Float(f) => {
            let raw = raw_of(f.as_repr()).unwrap_or_else(|| f.value().to_string());
            let mut n = Node::new(Value::Number(Number {
                raw: raw.clone(),
                kind: NumberKind::Float,
            }));
            n.style = Style::Original(raw);
            n
        }
        TomlValue::Boolean(f) => {
            let raw = raw_of(f.as_repr()).unwrap_or_else(|| f.value().to_string());
            let mut n = Node::new(Value::Bool(*f.value()));
            n.style = Style::Original(raw);
            n
        }
        TomlValue::Datetime(f) => {
            let raw = raw_of(f.as_repr()).unwrap_or_else(|| "<datetime>".to_string());
            let mut n = Node::new(Value::Datetime(raw.clone()));
            n.style = Style::Original(raw);
            n
        }
        TomlValue::Array(a) => {
            let mut items = Vec::new();
            for el in a.iter() {
                items.push(convert_value(ctx, el)?);
            }
            let mut n = Node::new(Value::Array(items));
            n.style = Style::Flow;
            if n.is_empty_container() {
                n.open_raw = Some("[]".to_string());
            } else {
                n.open_raw = Some("[".to_string());
                let trailing = a.trailing().as_str().unwrap_or("").to_string();
                n.close_raw = Some(format!(
                    "{}{trailing}]",
                    if a.trailing_comma() { "," } else { "" }
                ));
            }
            n
        }
        TomlValue::InlineTable(t) => {
            let mut entries = Vec::new();
            for (sk, sv) in t.iter() {
                let (skk, _) = t.get_key_value(sk).expect("iterated key exists");
                let mut vn = convert_value(ctx, sv)?;
                vn.trivia.prefix_raw = None; // sep covers the gap
                entries.push(Entry {
                    key: Key {
                        text: sk.to_string(),
                        repr: None,
                        raw: Some(key_raw(skk, sk)),
                    },
                    key_trivia: {
                        let mut kt = Trivia::empty();
                        let gp = prefix(skk.leaf_decor());
                        kt.prefix_raw = Some(gp.to_string());
                        fill_logical(&mut kt, gp);
                        kt
                    },
                    sep_raw: Some(format!(
                        "{}={}",
                        suffix(skk.leaf_decor()),
                        prefix(sv.decor())
                    )),
                    value: vn,
                });
            }
            let mut n = Node::new(Value::Map(entries));
            n.style = Style::Flow;
            if n.is_empty_container() {
                n.open_raw = Some("{}".to_string());
            } else {
                n.open_raw = Some("{".to_string());
                n.close_raw = Some("}".to_string());
            }
            n
        }
    };
    node.trivia.prefix_raw = Some(pre.clone());
    node.trivia.suffix_raw = Some(suf.clone());
    fill_logical(&mut node.trivia, &pre);
    if let Some(c) = inline_of(&suf) {
        node.trivia.inline = Some(c);
    }
    node.order = ctx.next();
    Ok(node)
}

/// Verbatim tail bytes: everything after the last item's same-line suffix.
/// Computed from the source by chopping trailing blank / full-comment lines
/// (`toml_edit` preserves them on output but exposes them in no accessible
/// slice). Safe against multiline strings/arrays: the scan stops at the
/// first trailing line carrying other content, which is necessarily the
/// last item's own closing line.
fn source_tail(src: &str) -> &str {
    let mut end = src.len();
    // Each iteration either breaks or strictly decreases `end`, so this
    // always terminates.
    while end > 0 {
        let rest = &src[..end];
        let (head_len, last) = match rest.rfind('\n') {
            Some(i) => (i, &rest[i + 1..]),
            None => (0, rest),
        };
        let t = last.trim();
        if !(t.is_empty() || t.starts_with('#')) {
            break;
        }
        // Consume `last` together with its preceding newline (which is the
        // previous line's terminator, hence part of the tail region).
        end = head_len;
    }
    &src[end..]
}

/// Parse TOML source into an IR document.
pub fn parse_toml(src: &str) -> Result<Doc, Error> {
    let doc: DocumentMut = src.parse().map_err(|e| toml_error(src, e))?;
    let mut ctx = Ctx { order: 0 };
    let mut entries: Vec<Entry> = Vec::new();
    // Snapshot iteration order (borrow discipline).
    let names: Vec<String> = doc.iter().map(|(k, _)| k.to_string()).collect();
    for (i, name) in names.iter().enumerate() {
        let (key, item) = doc.get_key_value(name).expect("iterated key exists");
        let item_clone = item.clone();
        let mut es = convert_item(&mut ctx, name, key, &item_clone, &[], i == 0)?;
        entries.append(&mut es);
    }
    let mut root = Node::new(Value::Map(entries));
    root.order = 0;
    let mut out = Doc::new(root);
    let tail = source_tail(src);
    out.trailing.prefix_raw = Some(tail.to_string());
    out.trailing.leading = extract_hash_comments(tail);
    Ok(out)
}

/// Emit a TOML document from the IR.
pub fn emit_toml(doc: &Doc, _opt: &Options) -> Result<EmitOutput, Error> {
    let Value::Map(entries) = &doc.root.value else {
        return Err(Error::emit("toml root must be a map"));
    };
    let mut warnings = Vec::new();
    let mut out = String::new();
    let mut first = true;
    for e in entries {
        emit_entry(&mut out, &mut warnings, &[], e, &mut first)?;
    }
    if let Some(tail) = doc.trailing.prefix_raw.as_deref() {
        out.push_str(tail);
    }
    // No fallback newline: programmatic docs terminate their last line via
    // the value-suffix rule, so appending another `\n` here would double it
    // after re-parsing (idempotence). A document that must end with a
    // newline carries it in `trailing.prefix_raw`.
    Ok(EmitOutput {
        text: out,
        warnings,
    })
}

/// Separator between items. Probed `toml_edit` storage rule: a stored
/// prefix excludes the first (structural) newline of the gap, so every
/// non-first item emits `"\n"` plus its stored prefix verbatim; the very
/// first item emits its prefix (file head) as-is.
fn emit_gap(out: &mut String, prefix_raw: Option<&str>, first: &mut bool) {
    if *first {
        if let Some(p) = prefix_raw {
            out.push_str(p);
        }
        *first = false;
        return;
    }
    out.push('\n');
    if let Some(p) = prefix_raw {
        out.push_str(p);
    }
    *first = false;
}

fn emit_entry(
    out: &mut String,
    warnings: &mut Vec<Warning>,
    path: &[String],
    e: &Entry,
    first: &mut bool,
) -> Result<(), Error> {
    // Inline tables (`{…}`) are Maps too, but they render through the scalar
    // path (verbatim `open/close`), not as `[header]` tables.
    let is_inline_map = matches!(&e.value.value, Value::Map(_))
        && e.value
            .open_raw
            .as_deref()
            .is_some_and(|o| o.starts_with('{'));
    match &e.value.value {
        Value::Map(children) if !is_inline_map => {
            // Without a header gap the anchor syntax (if any) is lost;
            // parsed tables always carry their prefix slice.
            if e.key_trivia.prefix_raw.is_none() {
                if let Some(a) = &e.value.anchor {
                    warnings.push(Warning::new(
                        e.key.text.clone(),
                        WarningKind::AnchorExpanded,
                        format!("anchor '{}' expanded (no verbatim spelling)", a.name),
                    ));
                }
            }
            let Some(open) = e.value.open_raw.as_deref() else {
                // Transparent implicit parent: children inline, no header.
                for c in children {
                    emit_entry(out, warnings, path, c, first)?;
                }
                return Ok(());
            };
            emit_gap(out, e.key_trivia.prefix_raw.as_deref(), first);
            out.push_str(open);
            if let Some(s) = e.value.trivia.suffix_raw.as_deref() {
                out.push_str(s);
            } else if e.value.trivia.inline.is_some() {
                warnings.push(Warning::new(
                    e.key.text.clone(),
                    WarningKind::CommentDropped,
                    "table inline comment has no verbatim slice; dropped",
                ));
            }
            let mut path2 = path.to_vec();
            path2.push(e.key.text.clone());
            // Newline after the header comes from the first child's gap
            // via the auto-newline rule: inner contexts never start "first".
            let mut inner_first = false;
            // Trick: reuse emit_gap by threading a flag that starts "first"
            // so an empty child prefix yields exactly one "\n".
            for c in children {
                emit_entry(out, warnings, &path2, c, &mut inner_first)?;
            }
            let _ = path2;
            Ok(())
        }
        Value::Array(elements)
            if elements.first().is_some_and(|n| {
                matches!(n.value, Value::Map(_))
                    && n.open_raw.as_deref().is_some_and(|o| o.starts_with('['))
            }) =>
        {
            // Array of tables.
            for el in elements {
                let Value::Map(children) = &el.value else {
                    return Err(Error::emit("mixed array-of-tables"));
                };
                if el.trivia.prefix_raw.is_none() {
                    if let Some(a) = &el.anchor {
                        warnings.push(Warning::new(
                            e.key.text.clone(),
                            WarningKind::AnchorExpanded,
                            format!("anchor '{}' expanded (no verbatim spelling)", a.name),
                        ));
                    }
                }
                let Some(open) = el.open_raw.as_deref() else {
                    return Err(Error::emit("array-of-tables element without header"));
                };
                emit_gap(out, el.trivia.prefix_raw.as_deref(), first);
                out.push_str(open);
                if let Some(s) = el.trivia.suffix_raw.as_deref() {
                    out.push_str(s);
                }
                let mut path2 = path.to_vec();
                path2.push(e.key.text.clone());
                let mut inner_first = false;
                for c in children {
                    emit_entry(out, warnings, &path2, c, &mut inner_first)?;
                }
            }
            Ok(())
        }
        _ => {
            // Plain KV (or dotted-flattened KV).
            emit_gap(out, e.key_trivia.prefix_raw.as_deref(), first);
            // Anchors survive only inside verbatim slices; without a
            // separator slice the syntax is lost.
            if e.sep_raw.is_none() {
                if let Some(a) = &e.value.anchor {
                    warnings.push(Warning::new(
                        e.key.text.clone(),
                        WarningKind::AnchorExpanded,
                        format!("anchor '{}' expanded (no verbatim spelling)", a.name),
                    ));
                }
            }
            out.push_str(e.key.raw.as_deref().unwrap_or(&e.key.text));
            out.push_str(e.sep_raw.as_deref().unwrap_or(" = "));
            emit_scalar(out, warnings, &e.key.text, &e.value)?;
            if let Some(s) = e.value.trivia.suffix_raw.as_deref() {
                out.push_str(s);
            } else {
                if e.value.trivia.inline.is_some() {
                    warnings.push(Warning::new(
                        e.key.text.clone(),
                        WarningKind::CommentDropped,
                        "inline comment has no verbatim slice; dropped",
                    ));
                }
                out.push('\n');
            }
            Ok(())
        }
    }
}

fn emit_scalar(
    out: &mut String,
    warnings: &mut Vec<Warning>,
    path: &str,
    node: &Node,
) -> Result<(), Error> {
    if let Style::Original(raw) = &node.style {
        out.push_str(raw);
        return Ok(());
    }
    match &node.value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(&b.to_string()),
        Value::Number(n) => out.push_str(&n.raw),
        Value::Str(s) => out.push_str(&format!("\"{s}\"")),
        Value::Datetime(d) => out.push_str(d),
        Value::Array(items) => {
            if let Some(open) = node.open_raw.as_deref() {
                // Verbatim (empty containers hold the whole `"[]"`).
                out.push_str(open);
                for (i, it) in items.iter().enumerate() {
                    if let Some(p) = it.trivia.prefix_raw.as_deref() {
                        out.push_str(p);
                    }
                    emit_scalar(out, warnings, path, it)?;
                    if let Some(s) = it.trivia.suffix_raw.as_deref() {
                        out.push_str(s);
                    }
                    if i + 1 < items.len() {
                        out.push(',');
                    }
                }
                if let Some(close) = node.close_raw.as_deref() {
                    out.push_str(close);
                }
            } else {
                // Canonical fallback for programmatic docs.
                out.push('[');
                for (i, it) in items.iter().enumerate() {
                    emit_scalar(out, warnings, path, it)?;
                    if i + 1 < items.len() {
                        out.push_str(", ");
                    }
                }
                out.push(']');
                warn_trivia(warnings, path, &node.trivia);
            }
        }
        Value::Map(entries) => {
            // Inline table (real tables never reach scalar position).
            if let Some(open) = node.open_raw.as_deref() {
                out.push_str(open);
                for (i, e) in entries.iter().enumerate() {
                    if let Some(p) = e.key_trivia.prefix_raw.as_deref() {
                        out.push_str(p);
                    }
                    out.push_str(e.key.raw.as_deref().unwrap_or(&e.key.text));
                    out.push_str(e.sep_raw.as_deref().unwrap_or(" = "));
                    emit_scalar(out, warnings, &e.key.text, &e.value)?;
                    if let Some(s) = e.value.trivia.suffix_raw.as_deref() {
                        out.push_str(s);
                    }
                    if i + 1 < entries.len() {
                        out.push(',');
                    }
                }
                if let Some(close) = node.close_raw.as_deref() {
                    out.push_str(close);
                }
            } else {
                out.push('{');
                for (i, e) in entries.iter().enumerate() {
                    out.push_str(&e.key.text);
                    out.push_str(" = ");
                    emit_scalar(out, warnings, &e.key.text, &e.value)?;
                    if i + 1 < entries.len() {
                        out.push_str(", ");
                    }
                }
                out.push('}');
                warn_trivia(warnings, path, &node.trivia);
            }
        }
        _ => {
            return Err(Error::emit("unexpected value kind in TOML output"));
        }
    }
    Ok(())
}

fn warn_trivia(warnings: &mut Vec<Warning>, path: &str, t: &Trivia) {
    if !t.leading.is_empty() || t.inline.is_some() {
        warnings.push(Warning::new(
            path,
            WarningKind::CommentDropped,
            "comment has no verbatim slice in this output; dropped",
        ));
    }
}

impl Format for TomlFormat {
    fn name(&self) -> &'static str {
        "toml"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["toml"]
    }

    fn parse(&self, src: &str) -> Result<Doc, Error> {
        parse_toml(src)
    }

    fn emit(&self, doc: &Doc, opt: &Options) -> Result<EmitOutput, Error> {
        emit_toml(doc, opt)
    }

    fn emit_logical(&self, doc: &Doc, opt: &Options) -> Result<EmitOutput, Error> {
        emit_toml_logical(doc, opt)
    }
}

// ---------------------------------------------------------------------------
// Logical (canonical + warnings) emission for cross-format conversion.
// ---------------------------------------------------------------------------

/// Canonical logical TOML emitter: values before tables (reordering warned),
/// comments preserved, anchors expanded, dotted-quoted keys as needed.
pub fn emit_toml_logical(doc: &Doc, _opt: &Options) -> Result<EmitOutput, Error> {
    let Value::Map(entries) = &doc.root.value else {
        return Err(Error::emit("toml root must be a map"));
    };
    let mut warnings = Vec::new();
    let mut out = String::new();
    for c in &doc.root.trivia.leading {
        out.push_str(c);
        out.push('\n');
    }
    emit_level(&mut out, &mut warnings, entries, &[], true)?;
    for c in &doc.trailing.leading {
        out.push_str(c);
        out.push('\n');
    }
    Ok(EmitOutput {
        text: out,
        warnings,
    })
}

fn dotted_path(path: &[String]) -> String {
    path.iter()
        .map(|s| toml_key(s).to_string())
        .collect::<Vec<_>>()
        .join(".")
}

/// Bare TOML key when possible, double-quoted otherwise.
fn toml_key(text: &str) -> String {
    if !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        text.to_string()
    } else {
        toml_basic_string(text)
    }
}

/// Double-quoted TOML basic string with escapes.
fn toml_basic_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn warn_anchor(warnings: &mut Vec<Warning>, path: &str, node: &Node) {
    if let Some(a) = &node.anchor {
        warnings.push(Warning::new(
            path,
            WarningKind::AnchorExpanded,
            format!("anchor '{}' expanded (TOML has no anchors)", a.name),
        ));
    }
}

fn emit_trivia_full(out: &mut String, t: &Trivia) {
    for _ in 0..t.blanks_before {
        out.push('\n');
    }
    for c in &t.leading {
        for line in crate::logical::restyle_comment(c, "#") {
            out.push_str(&line);
            out.push('\n');
        }
    }
}

fn emit_inline(out: &mut String, t: &Trivia) {
    if let Some(c) = &t.inline {
        out.push(' ');
        out.push_str(&crate::logical::restyle_comment(c, "#").join(" "));
    }
}

/// A value position classification for TOML ordering (values precede
/// tables) and array-of-tables detection.
#[derive(PartialEq, Eq)]
enum TomlSlot {
    Value,
    Table,
    AoT,
}

fn classify(node: &Node) -> TomlSlot {
    match &node.value {
        Value::Map(_) => TomlSlot::Table,
        Value::Array(items)
            if !items.is_empty() && items.iter().all(|i| matches!(i.value, Value::Map(_))) =>
        {
            TomlSlot::AoT
        }
        _ => TomlSlot::Value,
    }
}

fn emit_level(
    out: &mut String,
    warnings: &mut Vec<Warning>,
    entries: &[Entry],
    path: &[String],
    is_root: bool,
) -> Result<(), Error> {
    let here = dotted_path(path);
    let flat = crate::logical::expand_entries(entries, &here, true, warnings);
    // Partition preserving relative order; values must precede tables.
    let mut values: Vec<&Entry> = Vec::new();
    let mut tables: Vec<&Entry> = Vec::new();
    let mut seen_table = false;
    let mut reordered = false;
    for e in &flat {
        match classify(&e.value) {
            TomlSlot::Value => {
                values.push(e);
                if seen_table {
                    reordered = true;
                }
            }
            TomlSlot::Table | TomlSlot::AoT => {
                tables.push(e);
                seen_table = true;
            }
        }
    }
    // Mixed arrays (some map elements) are invalid TOML: detect now for a
    // precise error instead of corrupt output.
    for e in &flat {
        if let Value::Array(items) = &e.value.value {
            if !items.is_empty()
                && items.iter().any(|i| matches!(i.value, Value::Map(_)))
                && !items.iter().all(|i| matches!(i.value, Value::Map(_)))
            {
                return Err(Error::emit(format!(
                    "mixed arrays cannot be represented in TOML (key '{}')",
                    join_logical_path(&here, &e.key.text)
                )));
            }
        }
    }
    if reordered {
        warnings.push(Warning::new(
            here.clone(),
            WarningKind::KeyReordered,
            "keys reordered: TOML values must precede tables",
        ));
    }
    for e in values {
        emit_logical_kv(out, warnings, &here, e)?;
    }
    // Blank separator between values and tables is conventional; the stored
    // blanks/comments of the first table still render below.
    for e in tables {
        emit_logical_table(out, warnings, path, e, is_root)?;
    }
    let _ = is_root;
    Ok(())
}

fn join_logical_path(base: &str, key: &str) -> String {
    if base.is_empty() {
        key.to_string()
    } else {
        format!("{base}.{key}")
    }
}

fn emit_logical_kv(
    out: &mut String,
    warnings: &mut Vec<Warning>,
    here: &str,
    e: &Entry,
) -> Result<(), Error> {
    let path = join_logical_path(here, &e.key.text);
    warn_anchor(warnings, &path, &e.value);
    emit_trivia_full(out, &e.key_trivia);
    out.push_str(&toml_key(&e.key.text));
    out.push_str(" = ");
    let rendered = emit_logical_value(warnings, &path, &e.value)?;
    out.push_str(&rendered);
    emit_inline(out, &e.value.trivia);
    out.push('\n');
    Ok(())
}

fn emit_logical_value(
    warnings: &mut Vec<Warning>,
    path: &str,
    node: &Node,
) -> Result<String, Error> {
    match &node.value {
        Value::Null => Err(Error::emit(format!(
            "null has no TOML representation ({path})"
        ))),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Number(n) => Ok(n.raw.clone()),
        Value::Str(s) => Ok(toml_basic_string(s)),
        Value::Datetime(d) => Ok(d.clone()),
        Value::Array(items) => {
            let mut parts = Vec::new();
            for it in items {
                warn_anchor(warnings, path, it);
                parts.push(emit_logical_value(warnings, path, it)?);
            }
            Ok(format!("[{}]", parts.join(", ")))
        }
        Value::Map(entries) => {
            // Inline table (array-nested or explicit inline position).
            let flat = crate::logical::expand_entries(entries, path, true, warnings);
            let mut parts = Vec::new();
            for e in &flat {
                warn_anchor(warnings, &join_logical_path(path, &e.key.text), &e.value);
                parts.push(format!(
                    "{} = {}",
                    toml_key(&e.key.text),
                    emit_logical_value(warnings, &join_logical_path(path, &e.key.text), &e.value)?
                ));
            }
            Ok(format!("{{{}}}", parts.join(", ")))
        }
        _ => Err(Error::emit("unexpected value kind in TOML output")),
    }
}

fn emit_logical_table(
    out: &mut String,
    warnings: &mut Vec<Warning>,
    path: &[String],
    e: &Entry,
    _is_root: bool,
) -> Result<(), Error> {
    let here = dotted_path(path);
    let tpath = join_logical_path(&here, &e.key.text);
    warn_anchor(warnings, &tpath, &e.value);
    let mut segs = path.to_vec();
    segs.push(e.key.text.clone());
    match &e.value.value {
        Value::Map(children) => {
            emit_trivia_full(out, &e.key_trivia);
            out.push_str(&format!("[{}]", dotted_path(&segs)));
            emit_inline(out, &e.value.trivia);
            out.push('\n');
            emit_level(out, warnings, children, &segs, false)?;
            Ok(())
        }
        Value::Array(elements) => {
            for el in elements {
                let Value::Map(children) = &el.value else {
                    return Err(Error::emit(format!("invalid array-of-tables at {tpath}")));
                };
                warn_anchor(warnings, &tpath, el);
                emit_trivia_full(out, &e.key_trivia);
                out.push_str(&format!("[[{}]]", dotted_path(&segs)));
                emit_inline(out, &el.trivia);
                out.push('\n');
                emit_level(out, warnings, children, &segs, false)?;
            }
            Ok(())
        }
        _ => Err(Error::emit(format!(
            "internal: non-table in table slot ({tpath})"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgprism_core::values_equal;

    fn roundtrip(src: &str) -> Doc {
        let fmt = TomlFormat;
        let doc = fmt.parse(src).expect("parse");
        let out = fmt.emit(&doc, &Options::default()).expect("emit");
        assert_eq!(out.text, src, "byte round-trip failed");
        assert!(out.warnings.is_empty());
        fmt.parse(&out.text).expect("re-parse")
    }

    #[test]
    fn basic_values_and_comments() {
        let src = "# head\n\"hello\" = 'toml!' # c1\nbare = 1\nflt = 1.0\nt = true\ndt = 1979-05-27T07:32:00Z\n";
        let a = TomlFormat.parse(src).expect("parse");
        let b = roundtrip(src);
        assert!(values_equal(&a.root, &b.root));
        assert_eq!(a.root.get("bare").expect("bare").trivia.inline, None);
    }

    #[test]
    fn tables_arrays_dotted_inline() {
        let src = "top = 1\n['a'.b]\nx = [1, 2, # inner\n3]\ntbl = {a = 1}\n[[bin]]\nname = \"n\"\n[[bin]]\nname = \"m\"\n";
        let a = TomlFormat.parse(src).expect("parse");
        let b = roundtrip(src);
        assert!(values_equal(&a.root, &b.root));
    }

    #[test]
    fn dotted_keys_and_spaced_header() {
        let src = "# c\na . b = 1\n[ c.d ]\nx = 1\n";
        roundtrip(src);
    }

    #[test]
    fn errors_carry_line_col() {
        let err = TomlFormat.parse("a = 1\nb = \n").expect_err("must fail");
        assert!(err.pos.line >= 1 && err.pos.col >= 1, "{err}");
        assert!(
            err.to_string().starts_with("2:") || err.to_string().contains("2:"),
            "{err}"
        );
    }
}
