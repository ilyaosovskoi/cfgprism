//! YAML subset: block/flow mappings and sequences, plain/quoted/literal/
//! folded scalars, comments, anchors/aliases, explicit `---`/`...` markers.
//!
//! Architecture (see `DECISIONS.md` D14): `saphyr-parser` spanned events are
//! the source of truth for *structure* (anything outside the subset becomes
//! a positioned error, never a misparse); an own gap layer owns *trivia and
//! verbatim slices*; scalar resolution follows YAML 1.2 core (`no`/`yes`
//! are strings, `0o17` is octal, timestamps are strings).
//!
//! Byte round-trip: every gap byte is owned by exactly one `*_raw` slice —
//! `prefix` (up to node start, incl. indent/markers), key span slices,
//! `sep` (key end to value start, incl. `:`, anchors, tags, block headers),
//! value span slices (quoted spans include quotes; block spans cover the
//! content), same-line suffixes. Block collections need no open/close
//! (children prefixes cover layout); flow collections capture brackets via
//! event spans. Emission is pure concatenation — no auto-whitespace.
//!
//! Explicitly out of subset (positioned errors): multi-document streams,
//! non-core `!` tags, complex (`?`) keys, anchors on keys.

use std::collections::HashMap;

use cfgprism_core::{
    offset_to_line_col, Anchor, Doc, EmitOutput, Entry, Error, Format, Key, Node, Number,
    NumberKind, Options, Style, Trivia, Value, Warning, WarningKind,
};
use saphyr_parser::{Event, Parser, ScalarStyle, SpannedEventReceiver};

/// YAML format (`*.yaml`, `*.yml`).
pub struct YamlFormat;

/// Builder nesting cap (mirrors the JSON engine's reasoning).
const MAX_DEPTH: usize = 64;

type ByteSpan = (usize, usize);

struct RawEv<'a> {
    ev: Event<'a>,
    span: ByteSpan,
}

struct Collector<'a> {
    evs: Vec<RawEv<'a>>,
}

impl<'a> SpannedEventReceiver<'a> for Collector<'a> {
    fn on_event(&mut self, ev: Event<'a>, span: saphyr_parser::Span) {
        self.evs.push(RawEv {
            ev,
            span: (span.start.index(), span.end.index()),
        });
    }
}

/// A built value plus its byte extent and anchor id (0 = none).
/// Anchor *names* are resolved by the parent context, which owns the gap
/// slice holding the `&name` (separator for map values, prefix for sequence
/// items and the root).
struct Built {
    node: Node,
    span: ByteSpan,
    anchor_id: usize,
}

struct Builder<'a> {
    src: &'a str,
    evs: Vec<RawEv<'a>>,
    pos: usize,
    /// Anchor id → defined node snapshot (for alias cloning).
    anchors: HashMap<usize, Node>,
    /// Anchor id → source name.
    anchor_names: HashMap<usize, String>,
    order: usize,
    /// Byte offset where the next prefix slice starts.
    cursor: usize,
    /// Shared line index for O(log n) span positions (see core docs).
    lines: cfgprism_core::LineIndex,
}

impl<'a> Builder<'a> {
    fn peek_kind(&self) -> &'static str {
        match self.evs.get(self.pos).map(|r| &r.ev) {
            Some(Event::Scalar(_, _, _, _)) => "scalar",
            Some(Event::Alias(_)) => "alias",
            Some(Event::SequenceStart(_, _)) => "sequence",
            Some(Event::MappingStart(_, _)) => "mapping",
            Some(Event::SequenceEnd) => "sequence end",
            Some(Event::MappingEnd) => "mapping end",
            Some(Event::DocumentStart(_)) => "document start",
            Some(Event::DocumentEnd) => "document end",
            Some(Event::StreamStart) => "stream start",
            Some(Event::StreamEnd) => "stream end",
            Some(Event::Nothing) => "nothing",
            None => "end of input",
        }
    }

    fn err_at(&self, span: ByteSpan, msg: impl Into<String>) -> Error {
        let lc = offset_to_line_col(self.src, span.0.min(self.src.len()));
        Error::parse(lc.line, lc.col, msg)
    }

    fn next_order(&mut self) -> usize {
        let o = self.order;
        self.order += 1;
        o
    }

    fn peek_span(&self) -> Result<ByteSpan, Error> {
        self.evs.get(self.pos).map(|r| r.span).ok_or_else(|| {
            let lc = offset_to_line_col(self.src, self.src.len());
            Error::parse(lc.line, lc.col, "unexpected end of input")
        })
    }

    /// Source slice (spans are trusted, but fuzz input must never panic us).
    fn slice(&self, span: ByteSpan) -> Result<&'a str, Error> {
        let (s, e) = span;
        if s <= e
            && e <= self.src.len()
            && self.src.is_char_boundary(s)
            && self.src.is_char_boundary(e)
        {
            Ok(&self.src[s..e])
        } else {
            Err(self.err_at(span, "parser produced an invalid span"))
        }
    }

    /// Same-line remainder after `end` (no newline crossing). When `end`
    /// sits at a line start (block scalar tails), the remainder is empty —
    /// the following prefix owns that line.
    fn same_line_suffix(&self, end: usize) -> &'a str {
        if end >= self.src.len() {
            return "";
        }
        if end > 0 && self.src.as_bytes()[end - 1] == b'\n' {
            return "";
        }
        // Strip a trailing `\r` (CRLF): it belongs to the line ending,
        // which the next prefix owns.
        let rest = &self.src[end..];
        match rest.find('\n') {
            Some(i) => rest[..i].trim_end_matches('\r'),
            None => rest.trim_end_matches('\r'),
        }
    }

    /// Logical `#` comments in a gap slice. Gaps never contain quoted
    /// strings (quoted spans include their quotes), so a naive scan is safe.
    fn gap_comments(slice: &str) -> Vec<String> {
        let mut out = Vec::new();
        for line in slice.split('\n') {
            if let Some(i) = line.find('#') {
                out.push(line[i..].trim_end_matches('\r').trim_end().to_string());
            }
        }
        out
    }

    fn gap_blanks(slice: &str) -> usize {
        let lines: Vec<&str> = slice.split('\n').collect();
        if lines.len() < 2 {
            return 0;
        }
        lines[1..lines.len() - 1]
            .iter()
            .filter(|l| l.trim().trim_matches('\r').is_empty())
            .count()
    }

    fn fill_trivia(node: &mut Node, prefix: Option<&str>, suffix: Option<&str>) {
        if let Some(p) = prefix {
            node.trivia.leading = Self::gap_comments(p);
            node.trivia.blanks_before = Self::gap_blanks(p);
        }
        if let Some(s) = suffix {
            let mut cs = Self::gap_comments(s);
            if !cs.is_empty() {
                node.trivia.inline = Some(cs.remove(0));
                node.trivia.leading.extend(cs);
            }
        }
    }

    /// Take the current Scalar / Alias payload without cloning the world.
    /// Events borrow the source; the few owned Strings below are names and
    /// small texts, not the document.
    fn bump(&mut self) -> Result<(OwnedEv, ByteSpan), Error> {
        let r = self.evs.get(self.pos).ok_or_else(|| {
            let lc = offset_to_line_col(self.src, self.src.len());
            Error::parse(lc.line, lc.col, "unexpected end of input")
        })?;
        let span = r.span;
        let owned = match &r.ev {
            Event::Scalar(v, s, a, t) => OwnedEv::Scalar(
                v.to_string(),
                *s,
                *a,
                t.as_ref()
                    .map(|tag| (tag.handle.clone(), tag.suffix.clone())),
            ),
            Event::Alias(id) => OwnedEv::Alias(*id),
            Event::SequenceStart(a, t) => OwnedEv::SeqStart(
                *a,
                t.as_ref()
                    .map(|tag| (tag.handle.clone(), tag.suffix.clone())),
            ),
            Event::MappingStart(a, t) => OwnedEv::MapStart(
                *a,
                t.as_ref()
                    .map(|tag| (tag.handle.clone(), tag.suffix.clone())),
            ),
            Event::SequenceEnd => OwnedEv::SeqEnd,
            Event::MappingEnd => OwnedEv::MapEnd,
            Event::DocumentStart(_) => OwnedEv::DocStart,
            Event::DocumentEnd => OwnedEv::DocEnd,
            Event::StreamStart => OwnedEv::StreamStart,
            Event::StreamEnd => OwnedEv::StreamEnd,
            Event::Nothing => OwnedEv::Nothing,
        };
        self.pos += 1;
        Ok((owned, span))
    }

    fn check_tag(
        &self,
        tag: &Option<(String, String)>,
        span: ByteSpan,
    ) -> Result<Option<String>, Error> {
        let Some((handle, suffix)) = tag else {
            return Ok(None);
        };
        if handle == "tag:yaml.org,2002:" {
            return Ok(Some(suffix.clone()));
        }
        Err(self.err_at(
            span,
            format!("explicit tag '!{suffix}' is outside the stage-3 subset"),
        ))
    }

    /// YAML 1.2 core-schema plain resolution. Quoted/literal/folded styles
    /// always yield strings; only plain scalars are resolved.
    fn resolve_scalar(
        &self,
        text: &str,
        style: ScalarStyle,
        core_tag: Option<String>,
        span: ByteSpan,
    ) -> Result<Value, Error> {
        if !matches!(style, ScalarStyle::Plain) {
            return Ok(Value::Str(text.to_string()));
        }
        if let Some(tag) = core_tag {
            return match tag.as_str() {
                "str" => Ok(Value::Str(text.to_string())),
                "null" => match text {
                    "" | "~" | "null" | "Null" | "NULL" => Ok(Value::Null),
                    _ => Err(self.err_at(span, format!("!!null with value {text:?}"))),
                },
                "bool" => match text {
                    "true" | "True" | "TRUE" => Ok(Value::Bool(true)),
                    "false" | "False" | "FALSE" => Ok(Value::Bool(false)),
                    _ => Err(self.err_at(span, format!("!!bool with value {text:?}"))),
                },
                "int" => {
                    if parse_yaml_int(text).is_some() {
                        Ok(Value::Number(Number {
                            raw: text.to_string(),
                            kind: NumberKind::Int,
                        }))
                    } else {
                        Err(self.err_at(span, format!("!!int with value {text:?}")))
                    }
                }
                "float" => {
                    if parse_yaml_float(text).is_some() {
                        Ok(Value::Number(Number {
                            raw: text.to_string(),
                            kind: NumberKind::Float,
                        }))
                    } else {
                        Err(self.err_at(span, format!("!!float with value {text:?}")))
                    }
                }
                _ => Err(self.err_at(span, format!("tag '!!{tag}' is not supported"))),
            };
        }
        Ok(resolve_plain(text))
    }

    /// Register an anchor definition on a freshly built node. The `&name`
    /// lives in `gap` (separator/prefix slice) or, for scalars, right before
    /// the span on the same line.
    fn register_anchor(
        &mut self,
        node: &mut Node,
        anchor_id: usize,
        span: ByteSpan,
        gap: &str,
    ) -> Result<(), Error> {
        if anchor_id == 0 {
            return Ok(());
        }
        let name = self.scan_anchor(span.0, gap)?;
        self.anchors.insert(anchor_id, node.clone());
        self.anchor_names.insert(anchor_id, name.clone());
        node.anchor = Some(Anchor {
            name,
            is_alias: false,
        });
        Ok(())
    }

    fn scan_anchor(&self, from: usize, gap: &str) -> Result<String, Error> {
        // 1. Same-line backward scan (scalar anchors: `key: &a value`).
        let from = from.min(self.src.len());
        let line_start = self.src[..from].rfind('\n').map(|i| i + 1).unwrap_or(0);
        if let Some(name) = anchor_at_end(&self.src[line_start..from]) {
            return Ok(name);
        }
        // 2. Search the enclosing gap with comments stripped (block
        // collections: the anchor sits lines above the span; flow opens).
        // Gaps hold no quoted strings (quoted spans include quotes).
        let mut code = String::new();
        for ln in gap.split('\n') {
            match ln.find('#') {
                Some(i) => code.push_str(&ln[..i]),
                None => code.push_str(ln),
            }
            code.push('\n');
        }
        if let Some(i) = code.find('&') {
            let name: String = code[i + 1..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .collect();
            if !name.is_empty() {
                return Ok(name);
            }
        }
        Err(self.err_at((from, from), "anchor definition not found"))
    }

    fn build_node(&mut self, depth: usize) -> Result<Built, Error> {
        if depth > MAX_DEPTH {
            return Err(self.err_at(
                self.peek_span().unwrap_or((0, 0)),
                "nesting too deep (limit 64)",
            ));
        }
        // Flow detection: a non-empty start span on `[`/`{` means flow.
        let (span, is_flow) = {
            let r = self.evs.get(self.pos).ok_or_else(|| {
                let lc = offset_to_line_col(self.src, self.src.len());
                Error::parse(lc.line, lc.col, "unexpected end of input")
            })?;
            let flow = matches!(r.ev, Event::SequenceStart(_, _) | Event::MappingStart(_, _))
                && r.span.0 != r.span.1
                && matches!(self.src.get(r.span.0..r.span.0 + 1), Some("[") | Some("{"));
            (r.span, flow)
        };
        match self.peek_kind() {
            "scalar" => self.build_scalar(),
            "alias" => self.build_alias(),
            "sequence" => {
                if is_flow {
                    self.build_flow_seq(depth)
                } else {
                    self.build_block_seq(depth)
                }
            }
            "mapping" => {
                if is_flow {
                    self.build_flow_map(depth)
                } else {
                    self.build_block_map(depth)
                }
            }
            other => Err(self.err_at(span, format!("unexpected {other} as a value"))),
        }
    }

    fn build_scalar(&mut self) -> Result<Built, Error> {
        let (ev, span) = self.bump()?;
        let OwnedEv::Scalar(text, style, anchor_id, tag) = ev else {
            return Err(self.err_at(span, "expected scalar"));
        };
        let core_tag = self.check_tag(&tag, span)?;
        let value = self.resolve_scalar(&text, style, core_tag, span)?;
        let raw = self.slice(span)?.to_string();
        let mut node = Node::new(value);
        node.style = Style::Original(raw);
        node.order = self.next_order();
        node.span = Some(self.span_of(span));
        let suffix = self.same_line_suffix(span.1).to_string();
        Self::fill_trivia(&mut node, None, Some(&suffix));
        node.trivia.suffix_raw = Some(suffix);
        self.cursor = span.1;
        Ok(Built {
            node,
            span,
            anchor_id,
        })
    }

    fn build_alias(&mut self) -> Result<Built, Error> {
        let (ev, span) = self.bump()?;
        let OwnedEv::Alias(id) = ev else {
            return Err(self.err_at(span, "expected alias"));
        };
        let target = self
            .anchors
            .get(&id)
            .cloned()
            .ok_or_else(|| self.err_at(span, "alias of an unknown or recursive anchor"))?;
        let name = self.anchor_names.get(&id).cloned().unwrap_or_default();
        let raw = self.slice(span)?.to_string();
        if !raw.starts_with('*') {
            return Err(self.err_at(span, "invalid alias span"));
        }
        let mut node = Node::new(target.value);
        node.style = Style::Original(raw);
        node.order = self.next_order();
        node.span = Some(self.span_of(span));
        let suffix = self.same_line_suffix(span.1).to_string();
        Self::fill_trivia(&mut node, None, Some(&suffix));
        node.trivia.suffix_raw = Some(suffix);
        node.anchor = Some(Anchor {
            name,
            is_alias: true,
        });
        self.cursor = span.1;
        Ok(Built {
            node,
            span,
            anchor_id: 0,
        })
    }

    fn build_block_seq(&mut self, depth: usize) -> Result<Built, Error> {
        let (ev, span) = self.bump()?;
        let OwnedEv::SeqStart(anchor_id, tag) = ev else {
            return Err(self.err_at(span, "expected sequence"));
        };
        self.check_tag(&tag, span)?;
        let seq_start = span.0;
        // Block collections have no opener: the first prefix must start at
        // the collection span (the `-` position), right after the parent's
        // separator gap.
        self.cursor = seq_start;
        let mut items: Vec<Node> = Vec::new();
        loop {
            match self.peek_kind() {
                "sequence end" => {
                    let (_, espan) = self.bump()?;
                    let _ = espan;
                    let end = self.cursor;
                    let mut n = Node::new(Value::Array(items));
                    n.style = Style::Unknown;
                    n.order = self.next_order();
                    n.span = Some(self.span_of((seq_start, end)));
                    return Ok(Built {
                        node: n,
                        span: (seq_start, end),
                        anchor_id,
                    });
                }
                "mapping end" | "document end" | "stream end" | "end of input" => {
                    return Err(self.err_at(
                        self.peek_span().unwrap_or((seq_start, seq_start)),
                        "unterminated sequence",
                    ));
                }
                _ => {}
            }
            // Item prefix owns everything since the previous token (`- ` etc).
            let hspan = self.peek_span()?;
            let prefix = self.slice((self.cursor, hspan.0))?.to_string();
            let mut built = self.build_node(depth + 1)?;
            built.node.trivia.prefix_raw = Some(prefix.clone());
            Self::fill_trivia(&mut built.node, Some(&prefix), None);
            if matches!(built.node.value, Value::Array(_) | Value::Map(_))
                && built.node.open_raw.is_none()
            {
                // Block collection item: no suffix of its own (the following
                // prefix owns the line remainder).
                built.node.trivia.suffix_raw = None;
            }
            // Parent-level anchor registration (the `&name` sits in prefix).
            let gap = prefix.clone();
            let ispan = built.span;
            let id = built.anchor_id;
            self.register_anchor(&mut built.node, id, ispan, &gap)?;
            // Advance past the item's line remainder.
            self.cursor = ispan.1 + self.same_line_suffix(ispan.1).len();
            // Block collections already advanced the cursor past content.
            self.cursor = self.cursor.max(cursor_after(&built.node, ispan.1));
            items.push(built.node);
        }
    }

    fn build_block_map(&mut self, depth: usize) -> Result<Built, Error> {
        let (ev, span) = self.bump()?;
        let OwnedEv::MapStart(anchor_id, tag) = ev else {
            return Err(self.err_at(span, "expected mapping"));
        };
        self.check_tag(&tag, span)?;
        let map_start = span.0;
        // Same cursor rule as block sequences (see above).
        self.cursor = map_start;
        let mut entries: Vec<Entry> = Vec::new();
        loop {
            match self.peek_kind() {
                "mapping end" => {
                    let (_, espan) = self.bump()?;
                    let _ = espan;
                    let end = self.cursor;
                    let mut n = Node::new(Value::Map(entries));
                    n.style = Style::Unknown;
                    n.order = self.next_order();
                    n.span = Some(self.span_of((map_start, end)));
                    return Ok(Built {
                        node: n,
                        span: (map_start, end),
                        anchor_id,
                    });
                }
                "sequence end" | "document end" | "stream end" | "end of input" => {
                    return Err(self.err_at(
                        self.peek_span().unwrap_or((map_start, map_start)),
                        "unterminated mapping",
                    ));
                }
                _ => {}
            }
            entries.push(self.build_entry(depth + 1)?);
        }
    }

    /// One `key: value` entry in a block map.
    fn build_entry(&mut self, depth: usize) -> Result<Entry, Error> {
        let key_span = self.peek_span()?;
        let prefix = self.slice((self.cursor, key_span.0))?.to_string();
        let (kev, kspan) = self.bump()?;
        let OwnedEv::Scalar(ktext, kstyle, kanchor, ktag) = kev else {
            return Err(self.err_at(
                key_span,
                "complex mapping keys are outside the stage-3 subset",
            ));
        };
        if kanchor != 0 {
            return Err(self.err_at(key_span, "anchors on mapping keys are not supported"));
        }
        if ktag.is_some() {
            return Err(self.err_at(key_span, "tagged mapping keys are not supported"));
        }
        if ktext.is_empty() && kspan.0 == kspan.1 {
            return Err(self.err_at(key_span, "empty mapping key"));
        }
        let kvalue = self.resolve_scalar(&ktext, kstyle, None, kspan)?;
        let key_text = match kvalue {
            Value::Str(s) => s,
            Value::Null => return Err(self.err_at(key_span, "null mapping key")),
            _ => self.slice(kspan)?.trim().to_string(),
        };
        let key_raw = self.slice(kspan)?.to_string();
        let mut built = self.build_node(depth)?;
        let sep = self.slice((kspan.1, built.span.0))?.to_string();
        Self::fill_trivia(&mut built.node, Some(&sep), None);
        built.node.trivia.prefix_raw = None; // sep owns the gap
        self.register_anchor(&mut built.node, built.anchor_id, built.span, &sep)?;
        // Suffix: same-line remainder. Scalars/aliases set it during their
        // build (same value, idempotent); flow values need it here (their
        // builders leave it unset); block collections sit at a line end.
        let suf = self.same_line_suffix(built.span.1).to_string();
        Self::fill_trivia(&mut built.node, None, Some(&suf));
        built.node.trivia.suffix_raw = Some(suf);
        let mut key_trivia = Trivia::empty();
        key_trivia.prefix_raw = Some(prefix.clone());
        Self::fill_key_trivia(&mut key_trivia, &prefix);
        // Resume after the value's line remainder (block collections already
        // sit at a line end; scalars/aliases need the advance).
        self.cursor = (built.span.1 + self.same_line_suffix(built.span.1).len()).max(self.cursor);
        Ok(Entry {
            key: Key {
                text: key_text,
                repr: None,
                raw: Some(key_raw),
            },
            key_trivia,
            sep_raw: Some(sep),
            value: built.node,
        })
    }

    fn fill_key_trivia(t: &mut Trivia, prefix: &str) {
        t.leading = Self::gap_comments(prefix);
        t.blanks_before = Self::gap_blanks(prefix);
    }

    fn build_flow_seq(&mut self, depth: usize) -> Result<Built, Error> {
        let (ev, span) = self.bump()?;
        let OwnedEv::SeqStart(anchor_id, tag) = ev else {
            return Err(self.err_at(span, "expected sequence"));
        };
        self.check_tag(&tag, span)?;
        let open_start = span.0;
        let mut items: Vec<Node> = Vec::new();
        let mut open_raw: Option<String> = None;
        loop {
            match self.peek_kind() {
                "sequence end" => {
                    let (_, espan) = self.bump()?;
                    let close_gap = self.slice((self.cursor, espan.0))?.to_string();
                    let (head, _) = split_flow_gap(&close_gap);
                    if let Some(last) = items.last_mut() {
                        if last.trivia.inline.is_none() {
                            last.trivia.inline = first_comment(head);
                        }
                    }
                    let close_raw = self.slice((self.cursor, espan.1))?.to_string();
                    let mut n = Node::new(Value::Array(items));
                    n.style = Style::Flow;
                    n.order = self.next_order();
                    n.span = Some(self.span_of((open_start, espan.1)));
                    match open_raw.take() {
                        Some(o) => {
                            n.open_raw = Some(o);
                            n.close_raw = Some(close_raw);
                        }
                        None => {
                            n.open_raw = Some(self.slice((open_start, espan.1))?.to_string());
                        }
                    }
                    self.cursor = espan.1;
                    let gap = n.open_raw.clone().unwrap_or_default();
                    self.register_anchor(&mut n, anchor_id, (open_start, espan.1), &gap)?;
                    return Ok(Built {
                        node: n,
                        span: (open_start, espan.1),
                        anchor_id: 0,
                    });
                }
                "mapping end" | "document end" | "stream end" | "end of input" => {
                    return Err(self.err_at(
                        self.peek_span().unwrap_or((open_start, open_start)),
                        "unterminated flow sequence",
                    ));
                }
                _ => {}
            }
            let hspan = self.peek_span()?;
            let gap = self.slice((self.cursor, hspan.0))?.to_string();
            let ogap = self.slice((open_start, hspan.0))?.to_string();
            if open_raw.is_none() {
                open_raw = Some(ogap.clone());
            }
            let mut built = self.build_node(depth + 1)?;
            // Flow: the opener owns bracket+gap, so the first prefix is
            // empty; logical comments still come from the open gap.
            let first = items.is_empty();
            if first {
                built.node.trivia.prefix_raw = Some(String::new());
                Self::fill_trivia(&mut built.node, Some(&ogap), None);
            } else {
                // Verbatim prefix keeps the comma; logically the same-line
                // head belongs to the previous item's inline comment.
                let (head, tail) = split_flow_gap(&gap);
                if let Some(prev) = items.last_mut() {
                    if prev.trivia.inline.is_none() {
                        prev.trivia.inline = first_comment(head);
                    }
                }
                built.node.trivia.prefix_raw = Some(gap.clone());
                Self::fill_trivia(&mut built.node, Some(tail), None);
                built.node.trivia.leading = Self::gap_comments(tail);
                built.node.trivia.blanks_before = Self::gap_blanks(tail);
            }
            // Flow internals never own line suffixes (see flow entries).
            built.node.trivia.suffix_raw = None;
            built.node.trivia.inline = None;
            let ispan = built.span;
            let id = built.anchor_id;
            self.register_anchor(&mut built.node, id, ispan, &gap)?;
            self.cursor = ispan.1;
            items.push(built.node);
        }
    }

    fn build_flow_map(&mut self, depth: usize) -> Result<Built, Error> {
        let (ev, span) = self.bump()?;
        let OwnedEv::MapStart(anchor_id, tag) = ev else {
            return Err(self.err_at(span, "expected mapping"));
        };
        self.check_tag(&tag, span)?;
        let open_start = span.0;
        let mut entries: Vec<Entry> = Vec::new();
        let mut open_raw: Option<String> = None;
        loop {
            match self.peek_kind() {
                "mapping end" => {
                    let (_, espan) = self.bump()?;
                    let close_gap = self.slice((self.cursor, espan.0))?.to_string();
                    let (head, _) = split_flow_gap(&close_gap);
                    if let Some(last) = entries.last_mut() {
                        if last.value.trivia.inline.is_none() {
                            last.value.trivia.inline = first_comment(head);
                        }
                    }
                    let close_raw = self.slice((self.cursor, espan.1))?.to_string();
                    let mut n = Node::new(Value::Map(entries));
                    n.style = Style::Flow;
                    n.order = self.next_order();
                    n.span = Some(self.span_of((open_start, espan.1)));
                    match open_raw.take() {
                        Some(o) => {
                            n.open_raw = Some(o);
                            n.close_raw = Some(close_raw);
                        }
                        None => {
                            n.open_raw = Some(self.slice((open_start, espan.1))?.to_string());
                        }
                    }
                    self.cursor = espan.1;
                    let gap = n.open_raw.clone().unwrap_or_default();
                    self.register_anchor(&mut n, anchor_id, (open_start, espan.1), &gap)?;
                    return Ok(Built {
                        node: n,
                        span: (open_start, espan.1),
                        anchor_id: 0,
                    });
                }
                "sequence end" | "document end" | "stream end" | "end of input" => {
                    return Err(self.err_at(
                        self.peek_span().unwrap_or((open_start, open_start)),
                        "unterminated flow mapping",
                    ));
                }
                _ => {}
            }
            let hspan = self.peek_span()?;
            let gap = self.slice((self.cursor, hspan.0))?.to_string();
            let ogap = self.slice((open_start, hspan.0))?.to_string();
            let first = entries.is_empty();
            if open_raw.is_none() {
                open_raw = Some(ogap.clone());
            }
            if !first {
                // Same-line head of the gap belongs to the previous entry's
                // inline comment (the comma stays in the verbatim prefix).
                let (head, _) = split_flow_gap(&gap);
                if let Some(prev) = entries.last_mut() {
                    if prev.value.trivia.inline.is_none() {
                        prev.value.trivia.inline = first_comment(head);
                    }
                }
            }
            // First prefix is empty (the opener owns bracket+gap).
            let prefix = if first { String::new() } else { gap };
            let logical_gap = ogap;
            entries.push(self.build_flow_entry(depth + 1, prefix, first.then_some(logical_gap))?);
        }
    }

    fn build_flow_entry(
        &mut self,
        depth: usize,
        prefix: String,
        logical_gap: Option<String>,
    ) -> Result<Entry, Error> {
        let (kev, kspan) = self.bump()?;
        let OwnedEv::Scalar(ktext, kstyle, kanchor, ktag) = kev else {
            return Err(self.err_at(
                self.peek_span().unwrap_or((0, 0)),
                "complex mapping keys are outside the stage-3 subset",
            ));
        };
        if kanchor != 0 {
            return Err(self.err_at(kspan, "anchors on mapping keys are not supported"));
        }
        if ktag.is_some() {
            return Err(self.err_at(kspan, "tagged mapping keys are not supported"));
        }
        let kvalue = self.resolve_scalar(&ktext, kstyle, None, kspan)?;
        let key_text = match kvalue {
            Value::Str(s) => s,
            Value::Null => return Err(self.err_at(kspan, "null mapping key")),
            _ => self.slice(kspan)?.trim().to_string(),
        };
        let key_raw = self.slice(kspan)?.to_string();
        let mut built = self.build_node(depth)?;
        let sep = self.slice((kspan.1, built.span.0))?.to_string();
        built.node.trivia.prefix_raw = None;
        // Flow internals never own line suffixes (commas live in prefixes);
        // the scalar builder's line remainder is stale here — clear it.
        built.node.trivia.suffix_raw = None;
        built.node.trivia.inline = None;
        self.register_anchor(&mut built.node, built.anchor_id, built.span, &sep)?;
        let mut key_trivia = Trivia::empty();
        key_trivia.prefix_raw = Some(prefix);
        let logical: String = logical_gap
            .as_deref()
            .unwrap_or(key_trivia.prefix_raw.as_deref().unwrap_or(""))
            .to_string();
        Self::fill_key_trivia(&mut key_trivia, &logical);
        self.cursor = built.span.1;
        Ok(Entry {
            key: Key {
                text: key_text,
                repr: None,
                raw: Some(key_raw),
            },
            key_trivia,
            sep_raw: Some(sep),
            value: built.node,
        })
    }
}

/// Deepest consumed offset for cursor bookkeeping (block collections may
/// have advanced the cursor past the returned span already).
fn cursor_after(node: &Node, span_end: usize) -> usize {
    let _ = node;
    span_end
}

/// Owned event payload (events borrow the source; the builder cursor needs
/// owned control data to satisfy the borrow checker).
#[derive(Debug)]
enum OwnedEv {
    Scalar(String, ScalarStyle, usize, Option<(String, String)>),
    Alias(usize),
    SeqStart(usize, Option<(String, String)>),
    MapStart(usize, Option<(String, String)>),
    SeqEnd,
    MapEnd,
    DocStart,
    DocEnd,
    StreamStart,
    StreamEnd,
    Nothing,
}

/// Split a flow gap at the first newline: `(same_line_head, rest)`.
/// The head (with the comma) belongs to the previous item's line.
fn split_flow_gap(gap: &str) -> (&str, &str) {
    match gap.find('\n') {
        Some(i) => (&gap[..i], &gap[i..]),
        None => (gap, ""),
    }
}

/// First `#` comment in a slice, if any.
fn first_comment(slice: &str) -> Option<String> {
    for line in slice.split('\n') {
        if let Some(i) = line.find('#') {
            return Some(line[i..].trim_end_matches('\r').trim_end().to_string());
        }
    }
    None
}

/// Anchor name at the end of `line` (`&name` with optional trailing spaces).
fn anchor_at_end(line: &str) -> Option<String> {
    let t = line.trim_end_matches([' ', '\t', '\r']);
    let amp = t.rfind('&')?;
    if amp > 0 {
        let prev = t.as_bytes()[amp - 1];
        if prev != b' ' && prev != b'\t' {
            return None;
        }
    }
    let name: String = t[amp + 1..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    if name.is_empty() || t.len() != amp + 1 + name.len() {
        return None;
    }
    Some(name)
}

impl Builder<'_> {
    fn span_of(&self, span: ByteSpan) -> cfgprism_core::Span {
        let len = self.src.len();
        let s = self.lines.line_col(span.0.min(len));
        let e = self.lines.line_col(span.1.min(len));
        cfgprism_core::Span { start: s, end: e }
    }
}

/// YAML 1.2 core-schema plain scalar resolution.
fn resolve_plain(text: &str) -> Value {
    if text.is_empty() {
        return Value::Null;
    }
    match text {
        "~" | "null" | "Null" | "NULL" => return Value::Null,
        "true" | "True" | "TRUE" => return Value::Bool(true),
        "false" | "False" | "FALSE" => return Value::Bool(false),
        _ => {}
    }
    if parse_yaml_int(text).is_some() {
        return Value::Number(Number {
            raw: text.to_string(),
            kind: NumberKind::Int,
        });
    }
    if parse_yaml_float(text).is_some() {
        return Value::Number(Number {
            raw: text.to_string(),
            kind: NumberKind::Float,
        });
    }
    Value::Str(text.to_string())
}

fn parse_yaml_int(text: &str) -> Option<()> {
    let t = text.strip_prefix(['+', '-']).unwrap_or(text);
    if t.is_empty() {
        return None;
    }
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return (!hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit())).then_some(());
    }
    if let Some(oct) = t.strip_prefix("0o").or_else(|| t.strip_prefix("0O")) {
        return (!oct.is_empty() && oct.bytes().all(|b| matches!(b, b'0'..=b'7'))).then_some(());
    }
    t.bytes().all(|b| b.is_ascii_digit()).then_some(())
}

fn parse_yaml_float(text: &str) -> Option<()> {
    let t = text.strip_prefix(['+', '-']).unwrap_or(text);
    for special in [".inf", ".Inf", ".INF", ".nan", ".NaN", ".NAN"] {
        if t == special {
            return Some(());
        }
    }
    // `5`, `5.`, `.5`, `5e3`, `5.0e-2` — bare ints are checked first.
    let (mantissa, exp) = match t.split_once(['e', 'E']) {
        Some((m, e)) => {
            if e.is_empty() {
                return None;
            }
            let e = e.strip_prefix(['+', '-']).unwrap_or(e);
            if e.is_empty() || !e.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            (m, true)
        }
        None => (t, false),
    };
    if mantissa.is_empty() {
        return None;
    }
    match mantissa.split_once('.') {
        Some((a, b)) => {
            if a.is_empty() && b.is_empty() {
                return None;
            }
            if !a.bytes().all(|c| c.is_ascii_digit()) || !b.bytes().all(|c| c.is_ascii_digit()) {
                return None;
            }
            Some(())
        }
        None => {
            if exp && mantissa.bytes().all(|c| c.is_ascii_digit()) {
                Some(())
            } else {
                None
            }
        }
    }
}

/// `saphyr-parser` recurses per nesting level and overflows fixed-size
/// stacks on hostile depth (~5k `- ` levels abort a 2 MiB test stack, found
/// by the adversarial suite; debug frames run ~8 KiB/level). The event load
/// therefore runs on a worker thread with a 256 MiB stack — virtual address
/// space, committed on demand, so the cost is ~zero for normal inputs.
/// [`Builder`] depth (64) still bounds our own recursion afterwards, so IR
/// and emission never see hostile depth. On wasm32 threads do not exist, so
/// collection runs inline there (web-demo inputs are small; noted in the
/// stage-7 docs).
fn collect_events<'a>(src: &'a str) -> Result<Vec<RawEv<'a>>, Error> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut out: Option<Result<Vec<RawEv<'a>>, Error>> = None;
        std::thread::scope(|scope| {
            let handle = std::thread::Builder::new()
                .name("cfgprism-yaml-load".to_string())
                .stack_size(256 << 20)
                .spawn_scoped(scope, || run_load(src))
                .map_err(|e| Error::io(format!("cannot spawn YAML worker thread: {e}")))?;
            out = Some(
                handle
                    .join()
                    .map_err(|_| Error::emit("YAML parser failed internally on hostile input"))?,
            );
            Ok::<(), Error>(())
        })?;
        out.unwrap_or(Err(Error::io("YAML worker thread did not run")))
    }
    #[cfg(target_arch = "wasm32")]
    {
        run_load(src)
    }
}

fn run_load(src: &str) -> Result<Vec<RawEv<'_>>, Error> {
    let mut collector = Collector { evs: Vec::new() };
    let mut parser = Parser::new_from_str(src);
    parser.load(&mut collector, true).map_err(|e| {
        let m = e.marker();
        Error::parse(
            m.line().max(1) as u32,
            (m.col() + 1).max(1) as u32,
            e.to_string()
                .lines()
                .next()
                .unwrap_or("invalid YAML")
                .trim()
                .to_string(),
        )
    })?;
    Ok(collector.evs)
}

/// Parse YAML source into an IR document.
pub fn parse_yaml(src: &str) -> Result<Doc, Error> {
    let collector = collect_events(src)?;
    let doc_pos = collector
        .iter()
        .position(|r| matches!(r.ev, Event::DocumentStart(_)));
    let Some(doc_pos) = doc_pos else {
        // No document (empty / comment-only source): null root.
        let mut doc = Doc::new(Node::new(Value::Null));
        doc.trailing.prefix_raw = Some(src.to_string());
        doc.trailing.leading = Builder::gap_comments(src);
        return Ok(doc);
    };
    let lines = cfgprism_core::LineIndex::new(src);
    let mut b = Builder {
        src,
        evs: collector,
        lines,
        pos: doc_pos + 1, // consume DocumentStart
        anchors: HashMap::new(),
        anchor_names: HashMap::new(),
        order: 0,
        cursor: 0,
    };
    // Root value starts at the first content event; head owns everything
    // before it (`---`, comments, blank lines).
    let content_span = b.peek_span()?;
    let head = b.slice((0, content_span.0))?.to_string();
    b.cursor = content_span.0;
    let mut built = b.build_node(0)?;
    built.node.trivia.prefix_raw = Some(head.clone());
    Builder::fill_trivia(&mut built.node, Some(&head), None);
    // Root-level anchor definitions (`&a` before a block root).
    let head_gap = head.clone();
    let rspan = built.span;
    let rid = built.anchor_id;
    b.register_anchor(&mut built.node, rid, rspan, &head_gap)?;
    // A second document is outside the subset — an explicit, positioned
    // error beats silently dropping it.
    for r in b.evs.iter().skip(b.pos) {
        if let Event::DocumentStart(_) = r.ev {
            return Err(b.err_at(r.span, "multi-document YAML is outside the stage-3 subset"));
        }
    }
    let tail = b.slice((b.cursor, src.len()))?.to_string();
    let mut doc = Doc::new(built.node);
    doc.trailing.prefix_raw = Some(tail.clone());
    doc.trailing.leading = Builder::gap_comments(&tail);
    Ok(doc)
}

/// Emit a YAML document: verbatim concatenation when slices exist,
/// canonical block fallback (with warnings) for programmatic documents.
pub fn emit_yaml(doc: &Doc, opt: &Options) -> Result<EmitOutput, Error> {
    let mut e = YamlEmitter {
        indent: opt.indent.max(1),
        warnings: Vec::new(),
    };
    let out = if doc.root.trivia.prefix_raw.is_none() && root_has_no_slices(&doc.root) {
        e.emit_canonical_root(&doc.root)?
    } else {
        let mut text = doc.root.trivia.prefix_raw.clone().unwrap_or_default();
        text.push_str(&e.emit_node(&doc.root, "")?);
        if let Some(s) = doc.root.trivia.suffix_raw.as_deref() {
            text.push_str(s);
        }
        text
    };
    let mut out_with_tail = out;
    if let Some(tail) = doc.trailing.prefix_raw.as_deref() {
        out_with_tail.push_str(tail);
    }
    Ok(EmitOutput {
        text: out_with_tail,
        warnings: e.warnings,
    })
}

fn root_has_no_slices(node: &Node) -> bool {
    match &node.value {
        Value::Array(items) => {
            node.open_raw.is_none()
                && items.iter().all(|i| {
                    i.trivia.prefix_raw.is_none()
                        && i.trivia.suffix_raw.is_none()
                        && root_has_no_slices(i)
                })
        }
        Value::Map(entries) => {
            node.open_raw.is_none()
                && entries.iter().all(|en| {
                    en.key.raw.is_none()
                        && en.sep_raw.is_none()
                        && en.key_trivia.prefix_raw.is_none()
                        && root_has_no_slices(&en.value)
                })
        }
        _ => node.style_as_original().is_none(),
    }
}

trait StyleExt {
    fn style_as_original(&self) -> Option<&str>;
}

impl StyleExt for Node {
    fn style_as_original(&self) -> Option<&str> {
        match &self.style {
            Style::Original(raw) => Some(raw),
            _ => None,
        }
    }
}

struct YamlEmitter {
    indent: usize,
    warnings: Vec<Warning>,
}

impl YamlEmitter {
    fn warn(&mut self, path: &str, kind: WarningKind, msg: impl Into<String>) {
        self.warnings.push(Warning::new(path, kind, msg));
    }

    fn emit_node(&mut self, node: &Node, path: &str) -> Result<String, Error> {
        // Verbatim scalar spellings (plain/quoted/block content, aliases)
        // always win over value-kind dispatch: an alias of a map still
        // renders as `*name`.
        if let Some(raw) = node.style_as_original() {
            return Ok(raw.to_string());
        }
        match &node.value {
            Value::Array(_) | Value::Map(_) if node.open_raw.is_some() || is_flow(node) => {
                self.emit_flow_container(node, path)
            }
            Value::Array(items) => {
                // Block sequence, verbatim: prefixes own `- ` and layout.
                let mut out = String::new();
                for item in items {
                    out.push_str(item.trivia.prefix_raw.as_deref().unwrap_or(""));
                    out.push_str(&self.emit_node(item, path)?);
                    if prints_suffix(item) {
                        out.push_str(item.trivia.suffix_raw.as_deref().unwrap_or(""));
                    }
                }
                Ok(out)
            }
            Value::Map(entries) => {
                let mut out = String::new();
                for en in entries {
                    out.push_str(en.key_trivia.prefix_raw.as_deref().unwrap_or(""));
                    out.push_str(en.key.raw.as_deref().unwrap_or(&en.key.text));
                    out.push_str(en.sep_raw.as_deref().unwrap_or(": "));
                    out.push_str(&self.emit_node(&en.value, &join_path(path, &en.key.text))?);
                    if prints_suffix(&en.value) {
                        out.push_str(en.value.trivia.suffix_raw.as_deref().unwrap_or(""));
                    }
                }
                Ok(out)
            }
            _ => {
                if let Some(raw) = node.style_as_original() {
                    Ok(raw.to_string())
                } else {
                    Ok(yaml_scalar_fallback(&node.value))
                }
            }
        }
    }

    fn emit_flow_container(&mut self, node: &Node, path: &str) -> Result<String, Error> {
        let Some(open) = node.open_raw.as_deref() else {
            return self.emit_canonical(node, path, 0);
        };
        let mut out = String::from(open);
        match &node.value {
            Value::Array(items) => {
                for item in items {
                    out.push_str(item.trivia.prefix_raw.as_deref().unwrap_or(""));
                    out.push_str(&self.emit_node(item, path)?);
                }
            }
            Value::Map(entries) => {
                for en in entries {
                    out.push_str(en.key_trivia.prefix_raw.as_deref().unwrap_or(""));
                    out.push_str(en.key.raw.as_deref().unwrap_or(&en.key.text));
                    out.push_str(en.sep_raw.as_deref().unwrap_or(": "));
                    out.push_str(&self.emit_node(&en.value, &join_path(path, &en.key.text))?);
                }
            }
            _ => {}
        }
        if let Some(close) = node.close_raw.as_deref() {
            out.push_str(close);
        }
        Ok(out)
    }

    /// Canonical block emitter for programmatic documents and cross-format
    /// output. Comments are preserved (leading lines, inline suffixes);
    /// anchors expand inline with a warning; `<<` merges splice.
    fn emit_canonical(&mut self, node: &Node, path: &str, level: usize) -> Result<String, Error> {
        if let Some(a) = &node.anchor {
            self.warn(
                path,
                WarningKind::AnchorExpanded,
                format!("anchor '{}' expanded in canonical output", a.name),
            );
        }
        // Note: no generic trivia warning here. Every trivia slot has an
        // owner that either emits it (parents handle keys/items/values) or
        // warns explicitly (key inline comments).
        let pad = " ".repeat(self.indent * level);
        match &node.value {
            Value::Null => Ok("null".to_string()),
            Value::Bool(b) => Ok(b.to_string()),
            Value::Number(n) => Ok(n.raw.clone()),
            Value::Str(s) => Ok(yaml_plain_or_quoted(s, 0)),
            Value::Datetime(d) => Ok(yaml_plain_or_quoted(d, 0)),
            Value::Array(items) => {
                if items.is_empty() {
                    return Ok("[]".to_string());
                }
                let mut out = String::new();
                for item in items {
                    self.emit_blanks_leading(&mut out, path, &item.trivia, &pad);
                    if is_container(item) {
                        let child = self.emit_canonical(item, path, level + 1)?;
                        if child == "{}" || child == "[]" {
                            // Empty containers stay inline.
                            out.push_str(&format!("{pad}- {child}"));
                        } else {
                            out.push_str(&format!("{pad}-"));
                            out.push_str(&self.inline_of(&item.trivia));
                            out.push('\n');
                            for line in child.lines() {
                                out.push_str(&format!(
                                    "{}{}\n",
                                    " ".repeat(self.indent * (level + 1)),
                                    line
                                ));
                            }
                            continue;
                        }
                    } else {
                        let inline = self.emit_canonical(item, path, 0)?;
                        // Multiline scalars render as indented literal blocks.
                        let inline = indent_literal(&inline, self.indent * (level + 1));
                        out.push_str(&format!("{pad}- {inline}"));
                    }
                    out.push_str(&self.inline_of(&item.trivia));
                    out.push('\n');
                }
                Ok(out.trim_end().to_string())
            }
            Value::Map(entries) => {
                if entries.is_empty() {
                    return Ok("{}".to_string());
                }
                let flat = crate::logical::expand_entries(entries, path, false, &mut self.warnings);
                let mut out = String::new();
                for en in &flat {
                    let epath = join_path(path, &en.key.text);
                    self.emit_blanks_leading(&mut out, &epath, &en.key_trivia, &pad);
                    // Value leading (separator-gap comments, merge clones)
                    // has no line of its own; keep it above the key.
                    self.emit_blanks_leading(&mut out, &epath, &en.value.trivia, &pad);
                    if en.key_trivia.inline.is_some() {
                        self.warn(
                            &epath,
                            WarningKind::CommentDropped,
                            "key inline comment has no canonical position; dropped",
                        );
                    }
                    let key = yaml_plain_or_quoted(&en.key.text, 0);
                    if is_container(&en.value) {
                        let child = self.emit_canonical(&en.value, &epath, level + 1)?;
                        if child == "{}" || child == "[]" {
                            // Empty containers stay inline.
                            out.push_str(&format!("{pad}{key}: {child}"));
                        } else {
                            out.push_str(&format!("{pad}{key}:"));
                            out.push_str(&self.inline_of(&en.value.trivia));
                            out.push_str(&format!("\n{child}\n"));
                            continue;
                        }
                    } else {
                        let val = self.emit_canonical(&en.value, &epath, 0)?;
                        let val = indent_literal(&val, self.indent * (level + 1));
                        out.push_str(&format!("{pad}{key}: {val}"));
                    }
                    out.push_str(&self.inline_of(&en.value.trivia));
                    out.push('\n');
                }
                Ok(out.trim_end().to_string())
            }
            _ => Err(Error::emit(
                "unexpected value kind in canonical YAML output",
            )),
        }
    }

    /// Blank lines + full-line comments at the current indent.
    /// Markers are normalized to `#` (source markers like `//` are invalid
    /// in YAML).
    fn emit_blanks_leading(&mut self, out: &mut String, path: &str, t: &Trivia, pad: &str) {
        for _ in 0..t.blanks_before {
            out.push('\n');
        }
        for c in &t.leading {
            for line in crate::logical::restyle_comment(c, "#") {
                out.push_str(pad);
                out.push_str(&line);
                out.push('\n');
            }
        }
        let _ = path;
    }

    /// ` # comment` suffix (or empty).
    fn inline_of(&self, t: &Trivia) -> String {
        match &t.inline {
            Some(c) => format!(" {}", crate::logical::restyle_comment(c, "#").join(" ")),
            None => String::new(),
        }
    }

    fn emit_canonical_root(&mut self, root: &Node) -> Result<String, Error> {
        let mut out = self.emit_canonical(root, "", 0)?;
        out.push('\n');
        Ok(out)
    }
}

/// Re-indent a (possibly literal-block) canonical scalar under its parent.
fn indent_literal(s: &str, indent: usize) -> String {
    let mut lines = s.split('\n');
    let Some(first) = lines.next() else {
        return String::new();
    };
    let mut out = first.to_string();
    for line in lines {
        out.push('\n');
        out.push_str(&" ".repeat(indent));
        out.push_str(line);
    }
    out
}

fn is_flow(node: &Node) -> bool {
    node.style == Style::Flow
        || node
            .open_raw
            .as_deref()
            .is_some_and(|o| o.starts_with('[') || o.starts_with('{'))
}

fn is_container(node: &Node) -> bool {
    matches!(node.value, Value::Array(_) | Value::Map(_))
}

/// Block collections own no suffix (following prefixes do); scalars,
/// aliases and flow containers print theirs.
fn prints_suffix(node: &Node) -> bool {
    !is_container(node) || node.open_raw.is_some()
}

fn join_path(base: &str, key: &str) -> String {
    if base.is_empty() {
        key.to_string()
    } else {
        format!("{base}.{key}")
    }
}

/// Canonical scalar for fallback emission (never used verbatim).
fn yaml_scalar_fallback(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.raw.clone(),
        Value::Str(s) => yaml_plain_or_quoted(s, 0),
        Value::Datetime(d) => yaml_plain_or_quoted(d, 0),
        Value::Array(_) | Value::Map(_) => "<container>".to_string(),
        _ => "<other>".to_string(),
    }
}

/// Quote a string for canonical YAML output when plain style is unsafe.
/// `indent` pads literal-block continuation lines (already includes the
/// parent offset).
fn yaml_plain_or_quoted(s: &str, _indent: usize) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    if s.contains('\n') {
        // Double-quoted with escapes: the only multiline form that is
        // IR-stable for arbitrary text (literal chomping would normalize
        // trailing newlines).
        return yaml_double_quoted(s);
    }
    if yaml_needs_quotes(s) {
        return format!("'{}'", s.replace('\'', "''"));
    }
    s.to_string()
}

/// Double-quote with YAML escapes.
fn yaml_double_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn yaml_needs_quotes(s: &str) -> bool {
    if s.trim() != s {
        return true;
    }
    for tok in ["- ", "? ", ": "] {
        if s.starts_with(tok) {
            return true;
        }
    }
    if s == "-" || s == "?" || s == ":" {
        return true;
    }
    // Conservative: any structural character forces quotes (re-parse stays
    // a plain string either way, so this is always IR-stable).
    if s.chars().any(|c| {
        matches!(
            c,
            ':' | '#'
                | '['
                | ']'
                | '{'
                | '}'
                | ','
                | '&'
                | '*'
                | '?'
                | '|'
                | '<'
                | '>'
                | '='
                | '!'
                | '%'
                | '@'
                | '`'
                | '"'
                | '\''
        )
    }) {
        return true;
    }
    // Values that would resolve to non-strings.
    if resolve_plain(s) != Value::Str(s.to_string()) {
        return true;
    }
    false
}

impl Format for YamlFormat {
    fn name(&self) -> &'static str {
        "yaml"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["yaml", "yml"]
    }

    fn parse(&self, src: &str) -> Result<Doc, Error> {
        parse_yaml(src)
    }

    fn emit(&self, doc: &Doc, opt: &Options) -> Result<EmitOutput, Error> {
        emit_yaml(doc, opt)
    }

    fn emit_logical(&self, doc: &Doc, opt: &Options) -> Result<EmitOutput, Error> {
        emit_yaml_logical(doc, opt)
    }
}

/// Logical YAML output: canonical block form with comments preserved and
/// merge keys expanded (used for every cross-format pair targeting YAML).
pub fn emit_yaml_logical(doc: &Doc, opt: &Options) -> Result<EmitOutput, Error> {
    let mut e = YamlEmitter {
        indent: opt.indent.max(1),
        warnings: Vec::new(),
    };
    let mut out = String::new();
    for c in &doc.root.trivia.leading {
        for line in crate::logical::restyle_comment(c, "#") {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out.push_str(&e.emit_canonical(&doc.root, "", 0)?);
    if !is_container(&doc.root) {
        if let Some(c) = &doc.root.trivia.inline {
            out.push(' ');
            out.push_str(&crate::logical::restyle_comment(c, "#").join(" "));
        }
    } else if let Some(c) = &doc.root.trivia.inline {
        // Nowhere to hang a root-container inline comment; keep it as a
        // trailing line rather than dropping it.
        out.push('\n');
        out.push_str(&crate::logical::restyle_comment(c, "#").join(" "));
    }
    out.push('\n');
    for c in &doc.trailing.leading {
        for line in crate::logical::restyle_comment(c, "#") {
            out.push_str(&line);
            out.push('\n');
        }
    }
    Ok(EmitOutput {
        text: out,
        warnings: e.warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgprism_core::values_equal;

    fn roundtrip(src: &str) -> Doc {
        let fmt = YamlFormat;
        let doc = fmt.parse(src).expect("parse");
        let out = fmt.emit(&doc, &Options::default()).expect("emit");
        assert_eq!(out.text, src, "byte round-trip failed");
        assert!(out.warnings.is_empty());
        fmt.parse(&out.text).expect("re-parse")
    }

    #[test]
    fn basic_maps_lists_comments_anchors() {
        let src = "# head\nkey: &anc value # inline\nlist:\n  - one\n  - *anc\nnum: 42\nflag: true\nnothing: ~\n";
        let a = YamlFormat.parse(src).expect("parse");
        let b = roundtrip(src);
        assert!(values_equal(&a.root, &b.root));
        let v = a.root.get("key").expect("key");
        assert_eq!(v.value, Value::Str("value".to_string()));
        assert_eq!(v.anchor.as_ref().map(|x| x.name.as_str()), Some("anc"));
        assert_eq!(v.trivia.inline.as_deref(), Some("# inline"));
        let list = a.root.get("list").expect("list");
        assert_eq!(list.display_value(), "[2 items]");
    }

    #[test]
    fn norway_and_numbers() {
        let src = "a: no\nb: yes\nc: on\nd: off\ne: 'yes'\nf: \"42\"\ng: 0o17\nh: 0x1F\ni: 1_000\nj: 12:34\nk: 2024-01-01\nl: .inf\nm: 5.\nn: .5\n";
        let doc = roundtrip(src);
        let get = |k: &str| doc.root.get(k).expect(k).value.clone();
        // Norway problem: 1.2 strings, not bools.
        assert_eq!(get("a"), Value::Str("no".to_string()));
        assert_eq!(get("b"), Value::Str("yes".to_string()));
        assert_eq!(get("c"), Value::Str("on".to_string()));
        assert_eq!(get("d"), Value::Str("off".to_string()));
        assert_eq!(get("e"), Value::Str("yes".to_string()));
        // Quoted scalars are always strings... except explicit tags. Plain
        // `"42"` would resolve, but quotes force string.
        assert_eq!(get("f"), Value::Str("42".to_string()));
        assert!(matches!(get("g"), Value::Number(_)));
        assert!(matches!(get("h"), Value::Number(_)));
        // 1_000 is not 1.2-core int → string; 12:34 is not sexagesimal → string.
        assert_eq!(get("i"), Value::Str("1_000".to_string()));
        assert_eq!(get("j"), Value::Str("12:34".to_string()));
        // Dates are strings in YAML 1.2 core.
        assert_eq!(get("k"), Value::Str("2024-01-01".to_string()));
        assert!(matches!(get("l"), Value::Number(_)));
        assert!(matches!(get("m"), Value::Number(_)));
        assert!(matches!(get("n"), Value::Number(_)));
    }

    #[test]
    fn multiline_blocks() {
        let src = "lit: |\n  line1\n  line2\nfold: >\n  folded\n  text\nkeep: |+\n  kept\n\nstrip: |-\n  stripped\n";
        let doc = roundtrip(src);
        assert_eq!(
            doc.root.get("lit").expect("lit").value,
            Value::Str("line1\nline2\n".to_string())
        );
        assert_eq!(
            doc.root.get("fold").expect("fold").value,
            Value::Str("folded text\n".to_string())
        );
    }

    #[test]
    fn explicit_markers_round_trip() {
        roundtrip("---\na: 1\n...\n");
    }

    #[test]
    fn multidoc_is_an_error() {
        let err = YamlFormat
            .parse("a: 1\n---\nb: 2\n")
            .expect_err("must fail");
        assert!(err.message.contains("multi-document"), "{err}");
        assert_eq!(err.pos.line, 2);
    }

    #[test]
    fn deep_nesting_is_rejected_not_crashed() {
        let mut src = String::new();
        for i in 0..200 {
            src.push_str(&format!("k{i}:\n  "));
        }
        src.push_str("1\n");
        assert!(YamlFormat.parse(&src).is_err());
    }
}
