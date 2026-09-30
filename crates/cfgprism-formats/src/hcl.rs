//! Simplified HCL (HashiCorp Configuration Language): attributes
//! (`key = value`), nested blocks (`type "label" { … }`), `#`/`//`/`/* */`
//! comments. Values: strings (with escapes; `${…}` kept literal —
//! interpolation is out of scope), numbers, bools, `null`, lists and
//! objects (`,` or newline separated).
//!
//! Deliberately NOT supported, as positioned errors (never misparsed):
//! heredocs (`<<`), `for`/`if` expressions, function calls, variable
//! references, `(…)` keys and type annotations. Real-world modules using
//! those fail loudly with line:col instead of converting wrongly.
//! See `DECISIONS.md` D17 for why this is an own parser rather than
//! `hcl-edit` (simplified scope, byte round-trip through the IR).
//!
//! Model: attributes are KV entries; blocks nest by label
//! (`resource "aws_x" {…}` → `resource` → `aws_x` → body). Block headers
//! live verbatim in the node's `open_raw`; object values use `Flow` style,
//! so verbatim and logical emission stay unambiguous.

use cfgprism_core::{
    offset_to_line_col, Doc, EmitOutput, Entry, Error, Format, Key, Node, Number, NumberKind,
    Options, Style, Trivia, Value, Warning, WarningKind,
};

/// HCL / `*.hcl` / `*.tf` / `*.tfvars` files (simplified subset).
pub struct HclFormat;

/// Nesting cap (same reasoning as the JSON engine).
const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Ident(String),
    Str(String),
    Num,
    True,
    False,
    Null,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Eq,
    Colon,
    Comma,
    LParen,
    Heredoc,
}

struct Lexed {
    kind: Tok,
    start: usize,
    end: usize,
}

struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    fn err(&self, at: usize, msg: impl Into<String>) -> Error {
        let lc = offset_to_line_col(self.src, at);
        Error::parse(lc.line, lc.col, msg)
    }

    fn skip_gap(&mut self) {
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if matches!(b, b' ' | b'\t' | b'\n' | b'\r') {
                self.pos += 1;
                continue;
            }
            if self.src[self.pos..].starts_with("//") {
                while self.pos < self.bytes.len() && self.bytes[self.pos] != b'\n' {
                    self.pos += 1;
                }
                continue;
            }
            if self.src[self.pos..].starts_with("/*") {
                match self.src[self.pos + 2..].find("*/") {
                    Some(k) => {
                        self.pos += 2 + k + 2;
                        continue;
                    }
                    None => break, // reported later with position
                }
            }
            if b == b'#' {
                while self.pos < self.bytes.len() && self.bytes[self.pos] != b'\n' {
                    self.pos += 1;
                }
                continue;
            }
            break;
        }
    }

    fn lex_all(mut self) -> Result<Vec<Lexed>, Error> {
        let mut toks = Vec::new();
        loop {
            self.skip_gap();
            if self.pos >= self.bytes.len() {
                break;
            }
            let start = self.pos;
            let b = self.bytes[self.pos];
            let kind = match b {
                b'{' => Tok::LBrace,
                b'}' => Tok::RBrace,
                b'[' => Tok::LBracket,
                b']' => Tok::RBracket,
                b'=' => Tok::Eq,
                b':' => Tok::Colon,
                b',' => Tok::Comma,
                b'(' => Tok::LParen,
                b'"' => {
                    let text = self.lex_string()?;
                    toks.push(Lexed {
                        kind: Tok::Str(text),
                        start,
                        end: self.pos,
                    });
                    continue;
                }
                b'<' if self.src[self.pos..].starts_with("<<") => Tok::Heredoc,
                _ => {
                    if b.is_ascii_alphabetic() || b == b'_' {
                        let (kind, end) = self.lex_word()?;
                        toks.push(Lexed { kind, start, end });
                        self.pos = end;
                        continue;
                    }
                    if b.is_ascii_digit() || b == b'-' || b == b'+' || b == b'.' {
                        self.lex_number()?;
                        toks.push(Lexed {
                            kind: Tok::Num,
                            start,
                            end: self.pos,
                        });
                        continue;
                    }
                    return Err(self.err(start, format!("unexpected character '{}'", b as char)));
                }
            };
            self.pos += 1;
            toks.push(Lexed {
                kind,
                start,
                end: self.pos,
            });
        }
        Ok(toks)
    }

    fn lex_string(&mut self) -> Result<String, Error> {
        let start = self.pos;
        self.pos += 1; // opening quote
        let mut out = String::new();
        loop {
            if self.pos >= self.bytes.len() {
                return Err(self.err(start, "unterminated string"));
            }
            let b = self.bytes[self.pos];
            if b == b'"' {
                self.pos += 1;
                return Ok(out);
            }
            if b == b'\\' {
                self.pos += 1;
                if self.pos >= self.bytes.len() {
                    return Err(self.err(start, "unterminated string"));
                }
                match self.bytes[self.pos] {
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'u' => {
                        if self.pos + 4 >= self.bytes.len() {
                            return Err(self.err(self.pos, "truncated unicode escape"));
                        }
                        let hex = &self.src[self.pos + 1..self.pos + 5];
                        match u32::from_str_radix(hex, 16).ok().and_then(char::from_u32) {
                            Some(c) => {
                                out.push(c);
                                self.pos += 4;
                            }
                            None => return Err(self.err(self.pos, "invalid unicode escape")),
                        }
                    }
                    // `${…}` interpolation is out of scope: kept literal.
                    // Any other escape survives verbatim-ish (backslash kept).
                    _ => {
                        out.push('\\');
                        out.push(self.bytes[self.pos] as char);
                    }
                }
                self.pos += 1;
                continue;
            }
            if b == b'\n' || b < 0x20 {
                return Err(self.err(self.pos, "unescaped control in string (use \\n)"));
            }
            let ch = self.src[self.pos..]
                .chars()
                .next()
                .ok_or_else(|| self.err(self.pos, "invalid UTF-8 in string"))?;
            out.push(ch);
            self.pos += ch.len_utf8();
        }
    }

    fn lex_word(&mut self) -> Result<(Tok, usize), Error> {
        let start = self.pos;
        while self.pos < self.bytes.len() {
            let c = self.bytes[self.pos];
            if c.is_ascii_alphanumeric() || c == b'_' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let word = &self.src[start..self.pos];
        let kind = match word {
            "true" => Tok::True,
            "false" => Tok::False,
            "null" => Tok::Null,
            _ => Tok::Ident(word.to_string()),
        };
        Ok((kind, self.pos))
    }

    fn lex_number(&mut self) -> Result<(), Error> {
        let start = self.pos;
        let b = self.bytes;
        if b.get(self.pos) == Some(&b'-') || b.get(self.pos) == Some(&b'+') {
            self.pos += 1;
        }
        let mut digits = 0;
        while self.pos < b.len() && b[self.pos].is_ascii_digit() {
            self.pos += 1;
            digits += 1;
        }
        if b.get(self.pos) == Some(&b'.') {
            self.pos += 1;
            while self.pos < b.len() && b[self.pos].is_ascii_digit() {
                self.pos += 1;
                digits += 1;
            }
        }
        if b.get(self.pos) == Some(&b'e') || b.get(self.pos) == Some(&b'E') {
            self.pos += 1;
            if b.get(self.pos) == Some(&b'-') || b.get(self.pos) == Some(&b'+') {
                self.pos += 1;
            }
            let e0 = self.pos;
            while self.pos < b.len() && b[self.pos].is_ascii_digit() {
                self.pos += 1;
            }
            if e0 == self.pos {
                return Err(self.err(start, "invalid exponent"));
            }
        }
        if digits == 0 {
            return Err(self.err(start, "invalid number"));
        }
        if self.pos < b.len() {
            let c = b[self.pos];
            if c.is_ascii_alphanumeric() || c == b'_' {
                return Err(self.err(self.pos, "invalid number suffix"));
            }
        }
        Ok(())
    }
}

struct Parser<'a> {
    src: &'a str,
    toks: Vec<Lexed>,
    pos: usize,
    last_end: usize,
    order: usize,
}

impl<'a> Parser<'a> {
    fn gap(&self, to: usize) -> &'a str {
        &self.src[self.last_end..to]
    }

    fn next_order(&mut self) -> usize {
        let o = self.order;
        self.order += 1;
        o
    }

    fn peek(&self) -> Option<&Lexed> {
        self.toks.get(self.pos)
    }

    fn bump(&mut self) -> Option<Lexed> {
        // Lexed contains Strings; rebuild cheaply via index (no Clone derive
        // needed on Tok::Ident payloads beyond what we copy on use).
        let t = self.toks.get(self.pos)?;
        let out = Lexed {
            kind: t.kind.clone(),
            start: t.start,
            end: t.end,
        };
        self.last_end = t.end;
        self.pos += 1;
        Some(out)
    }

    fn err(&self, at: usize, msg: impl Into<String>) -> Error {
        let lc = offset_to_line_col(self.src, at);
        Error::parse(lc.line, lc.col, msg)
    }

    fn eof_err(&self) -> Error {
        let lc = offset_to_line_col(self.src, self.src.len());
        Error::parse(lc.line, lc.col, "unexpected end of input")
    }

    /// Logical `#`/`//` comments in a gap slice.
    fn gap_comments(slice: &str) -> Vec<String> {
        let mut out = Vec::new();
        for line in slice.split('\n') {
            let t = line.trim();
            if let Some(i) = t.find('#') {
                out.push(t[i..].trim_end_matches('\r').to_string());
                continue;
            }
            if let Some(i) = t.find("//") {
                out.push(t[i..].trim_end_matches('\r').to_string());
            }
        }
        out
    }

    fn tok_name(kind: &Tok) -> &'static str {
        match kind {
            Tok::Ident(_) => "an identifier",
            Tok::Str(_) => "a string",
            Tok::Num => "a number",
            Tok::True | Tok::False => "a boolean",
            Tok::Null => "null",
            Tok::LBrace => "'{'",
            Tok::RBrace => "'}'",
            Tok::LBracket => "'['",
            Tok::RBracket => "']'",
            Tok::Eq => "'='",
            Tok::Colon => "':'",
            Tok::Comma => "','",
            Tok::LParen => "'('",
            Tok::Heredoc => "a heredoc",
        }
    }

    /// Parse a body (document or block): attributes and blocks until
    /// `}`/EOF. Returns entries in source order. Owns prefix discipline:
    /// each item's prefix runs from the previous token end.
    fn parse_body(&mut self, depth: usize) -> Result<Vec<Entry>, Error> {
        if depth > MAX_DEPTH {
            return Err(self.eof_err());
        }
        let mut entries = Vec::new();
        loop {
            let t = match self.peek() {
                None => break,
                Some(t) => t,
            };
            if matches!(t.kind, Tok::RBrace) {
                break;
            }
            let start = t.start;
            let prefix = self.gap(start).to_string();
            let mut entry = self.parse_item(depth + 1)?;
            entry.key_trivia.prefix_raw = Some(prefix.clone());
            fill_key_logical(&mut entry.key_trivia, &prefix);
            entries.push(entry);
        }
        Ok(entries)
    }

    /// One attribute (`key = value`) or block (`type "label" { … }`).
    fn parse_item(&mut self, depth: usize) -> Result<Entry, Error> {
        if depth > MAX_DEPTH {
            return Err(self.eof_err());
        }
        let src_len = self.src.len();
        let kt = self.peek().cloned_tok().ok_or_else(|| self.eof_err())?;
        let (key_text, key_raw) = match &kt.kind {
            Tok::Ident(s) => (s.clone(), self.src[kt.start..kt.end].to_string()),
            Tok::Str(s) => (s.clone(), self.src[kt.start..kt.end].to_string()),
            _ => {
                return Err(self.err(
                    kt.start,
                    format!(
                        "expected attribute or block, got {}",
                        Self::tok_name(&kt.kind)
                    ),
                ));
            }
        };
        self.bump();
        // Attribute (`=`) or block (labels + `{`)?
        let after_key = self.peek().cloned_tok().ok_or_else(|| self.eof_err())?;
        if matches!(after_key.kind, Tok::Eq) {
            self.bump();
            let vstart = self.peek().map(|t| t.start).ok_or_else(|| self.eof_err())?;
            // gap(key→=) + "=" + gap(=→value).
            let sep = format!(
                "{}{}{}",
                &self.src[kt.end..after_key.start],
                "=",
                &self.src[after_key.end..vstart]
            );
            let mut value = self.parse_value(depth)?;
            value.trivia.prefix_raw = None; // sep owns the key→value gap
                                            // The suffix runs to the next token or the line end, whichever
                                            // comes first; anything else on the line is an error (checked
                                            // before claiming the remainder, so positions stay sane).
            let vend = self.last_end;
            let next_start = self.peek().map(|t| t.start).unwrap_or(src_len);
            let line_end = self.src[vend..]
                .find('\n')
                .map(|i| vend + i)
                .unwrap_or(src_len);
            let send = next_start.min(line_end);
            let suffix = self.src[vend..send].to_string();
            Self::fill_suffix(&mut value, &suffix);
            value.trivia.suffix_raw = Some(suffix);
            self.last_end = send;
            // Attribute values must end the line (or meet `}`/EOF).
            self.check_value_end()?;
            return Ok(Entry {
                key: Key {
                    text: key_text,
                    repr: None,
                    raw: Some(key_raw),
                },
                key_trivia: Trivia::empty(), // prefix attached by the caller
                sep_raw: Some(sep),
                value,
            });
        }
        // Block: labels then `{` body `}`.
        let mut label_spans: Vec<(String, usize, usize)> = Vec::new();
        while !matches!(self.peek().map(|t| &t.kind), Some(Tok::LBrace) | None) {
            let lt = self.peek().cloned_tok().ok_or_else(|| self.eof_err())?;
            match &lt.kind {
                Tok::Ident(s) | Tok::Str(s) => {
                    label_spans.push((s.clone(), lt.start, lt.end));
                    self.bump();
                }
                _ => {
                    return Err(self.err(
                        lt.start,
                        format!(
                            "expected block label or '{{', got {}",
                            Self::tok_name(&lt.kind)
                        ),
                    ));
                }
            }
        }
        let open_tok = self.peek().cloned_tok().ok_or_else(|| self.eof_err())?;
        if !matches!(open_tok.kind, Tok::LBrace) {
            return Err(self.err(
                open_tok.start,
                "expected '=' for an attribute or '{' for a block".to_string(),
            ));
        }
        self.bump();
        let children = self.parse_body(depth)?;
        let close = self.peek().cloned_tok().ok_or_else(|| self.eof_err())?;
        if !matches!(close.kind, Tok::RBrace) {
            return Err(self.err(
                close.start,
                format!("expected '}}', got {}", Self::tok_name(&close.kind)),
            ));
        }
        // Capture the gap BEFORE consuming `}` (bump moves last_end).
        let pre = self.last_end;
        self.bump();
        // Header verbatim: key through `{` (labels keep their spelling).
        let open_raw = self.src[kt.start..open_tok.end].to_string();
        // Close verbatim: gap since last child through `}`.
        let close_raw = self.src[pre..close.end].to_string();
        let mut map = Node::new(Value::Map(children));
        map.open_raw = Some(open_raw);
        map.close_raw = Some(close_raw);
        map.order = self.next_order();
        // Same-line remainder after `}` belongs to the block suffix
        // (bounded by the next token, like attribute values).
        let cend = close.end;
        let next_start = self.peek().map(|t| t.start).unwrap_or(src_len);
        let line_end = self.src[cend..]
            .find('\n')
            .map(|i| cend + i)
            .unwrap_or(src_len);
        let send = next_start.min(line_end);
        let suffix = self.src[cend..send].to_string();
        Self::fill_suffix(&mut map, &suffix);
        map.trivia.suffix_raw = Some(suffix.clone());
        self.last_end = send;
        // Like attributes, blocks must end the line (or meet `}`/EOF).
        self.check_value_end()?;
        // Nest by label for JSON-natural conversion.
        let node = nest_labels(label_spans, map, self)?;
        Ok(Entry {
            key: Key {
                text: key_text,
                repr: None,
                raw: Some(key_raw),
            },
            key_trivia: Trivia::empty(), // prefix attached by finish pass
            sep_raw: None,
            value: node,
        })
    }

    /// Attach a same-line suffix (logical inline comment included).
    fn fill_suffix(node: &mut Node, suffix: &str) {
        let mut comments = Self::gap_comments(suffix);
        if !comments.is_empty() {
            node.trivia.inline = Some(comments.remove(0));
            node.trivia.leading.extend(comments);
        }
    }

    /// After an attribute value, only a newline, `}` or EOF may follow.
    fn check_value_end(&mut self) -> Result<(), Error> {
        let gap_start = self.last_end;
        match self.peek() {
            None => Ok(()),
            Some(t) if matches!(t.kind, Tok::RBrace) => Ok(()),
            Some(t) => {
                let gap = &self.src[gap_start..t.start];
                if gap.contains('\n') {
                    Ok(())
                } else {
                    Err(self.err(
                        t.start,
                        format!("expected newline, got {}", Self::tok_name(&t.kind)),
                    ))
                }
            }
        }
    }

    fn parse_value(&mut self, depth: usize) -> Result<Node, Error> {
        if depth > MAX_DEPTH {
            return Err(self.eof_err());
        }
        let t = self.peek().cloned_tok().ok_or_else(|| self.eof_err())?;
        match t.kind {
            Tok::Str(s) => {
                self.bump();
                let raw = self.src[t.start..t.end].to_string();
                let mut n = Node::new(Value::Str(s));
                n.style = Style::Original(raw);
                n.order = self.next_order();
                Ok(n)
            }
            Tok::Num => {
                self.bump();
                let raw = self.src[t.start..t.end].to_string();
                let kind = if raw.contains(['.', 'e', 'E']) {
                    NumberKind::Float
                } else {
                    NumberKind::Int
                };
                let mut n = Node::new(Value::Number(Number {
                    raw: raw.clone(),
                    kind,
                }));
                n.style = Style::Original(raw);
                n.order = self.next_order();
                Ok(n)
            }
            Tok::True | Tok::False => {
                self.bump();
                let raw = self.src[t.start..t.end].to_string();
                let mut n = Node::new(Value::Bool(matches!(t.kind, Tok::True)));
                n.style = Style::Original(raw);
                n.order = self.next_order();
                Ok(n)
            }
            Tok::Null => {
                self.bump();
                let raw = self.src[t.start..t.end].to_string();
                let mut n = Node::new(Value::Null);
                n.style = Style::Original(raw);
                n.order = self.next_order();
                Ok(n)
            }
            Tok::LBracket => self.parse_list(depth),
            Tok::LBrace => self.parse_object(depth),
            Tok::Ident(s) => Err(self.err(
                t.start,
                format!("expressions and references ('{s}') are outside the simplified HCL subset"),
            )),
            Tok::Heredoc => Err(self.err(
                t.start,
                "heredocs ('<<') are outside the simplified HCL subset",
            )),
            Tok::LParen => Err(self.err(
                t.start,
                "function calls are outside the simplified HCL subset",
            )),
            other => Err(self.err(
                t.start,
                format!("unexpected {} as a value", Self::tok_name(&other)),
            )),
        }
    }

    fn parse_list(&mut self, depth: usize) -> Result<Node, Error> {
        let open = self.bump().ok_or_else(|| self.eof_err())?;
        let mut items = Vec::new();
        // Empty list (modulo gap).
        if let Some(t) = self.peek() {
            if matches!(t.kind, Tok::RBracket) {
                let close = self.bump().expect("peeked");
                let raw = self.src[open.start..close.end].to_string();
                let mut n = Node::new(Value::Array(items));
                n.style = Style::Flow;
                n.open_raw = Some(raw);
                n.order = self.next_order();
                return Ok(n);
            }
        } else {
            return Err(self.eof_err());
        }
        let first_start = self.peek().map(|t| t.start).ok_or_else(|| self.eof_err())?;
        let open_raw = self.src[open.start..first_start].to_string();
        let open_gap = self.src[open.end..first_start].to_string();
        let mut first = self.parse_value(depth + 1)?;
        first.trivia.prefix_raw = None; // opener owns bracket+gap
        Self::fill_item_leading(&mut first, &open_gap);
        items.push(first);
        loop {
            let t = self.peek().cloned_tok().ok_or_else(|| self.eof_err())?;
            match t.kind {
                Tok::Comma => {
                    let comma = self.bump().expect("peeked");
                    let nxt = self.peek().cloned_tok().ok_or_else(|| self.eof_err())?;
                    if matches!(nxt.kind, Tok::RBracket) {
                        // Trailing comma folds into close (verbatim).
                        let close = self.bump().expect("peeked");
                        let mut n = Node::new(Value::Array(std::mem::take(&mut items)));
                        n.style = Style::Flow;
                        n.open_raw = Some(open_raw);
                        n.close_raw = Some(self.src[comma.start..close.end].to_string());
                        n.order = self.next_order();
                        return Ok(n);
                    }
                    let gap = self.src[comma.end..nxt.start].to_string();
                    let mut item = self.parse_value(depth + 1)?;
                    // Verbatim prefix keeps an optional preceding comma.
                    item.trivia.prefix_raw = Some(format!(",{gap}"));
                    Self::fill_item_leading(&mut item, &gap);
                    items.push(item);
                }
                Tok::RBracket => {
                    // Capture the gap BEFORE consuming `]` (bump moves last_end).
                    let close_gap = self.src[self.last_end..t.start].to_string();
                    Self::attach_close_comment(&mut items, &close_gap);
                    let _close = self.bump().expect("peeked");
                    let mut n = Node::new(Value::Array(std::mem::take(&mut items)));
                    n.style = Style::Flow;
                    n.open_raw = Some(open_raw);
                    n.close_raw = Some(format!("{close_gap}]"));
                    n.order = self.next_order();
                    return Ok(n);
                }
                _ => {
                    // Newline-separated values (no comma): the item starts a
                    // fresh gap; the previous item needs no comma.
                    let gap = self.src[self.last_end..t.start].to_string();
                    if !gap.contains('\n') {
                        return Err(self.err(
                            t.start,
                            format!("expected ',' or ']', got {}", Self::tok_name(&t.kind)),
                        ));
                    }
                    let mut item = self.parse_value(depth + 1)?;
                    item.trivia.prefix_raw = Some(gap.clone());
                    Self::fill_item_leading(&mut item, &gap);
                    items.push(item);
                }
            }
        }
    }

    /// Logical leading trivia from a gap slice.
    fn fill_item_leading(node: &mut Node, gap: &str) {
        node.trivia.leading = Self::gap_comments(gap);
        node.trivia.blanks_before = count_blanks(gap);
    }

    /// Same-line head comment of a close gap belongs to the last child.
    fn attach_close_comment(items: &mut [Node], close_gap: &str) {
        let head = close_gap.split('\n').next().unwrap_or("");
        if let Some(last) = items.last_mut() {
            if last.trivia.inline.is_none() {
                last.trivia.inline = first_comment_in(head);
            }
        }
    }

    fn parse_object(&mut self, depth: usize) -> Result<Node, Error> {
        let open = self.bump().ok_or_else(|| self.eof_err())?;
        let mut entries = Vec::new();
        if let Some(t) = self.peek() {
            if matches!(t.kind, Tok::RBrace) {
                let close = self.bump().expect("peeked");
                let raw = self.src[open.start..close.end].to_string();
                let mut n = Node::new(Value::Map(entries));
                n.style = Style::Flow;
                n.open_raw = Some(raw);
                n.order = self.next_order();
                return Ok(n);
            }
        } else {
            return Err(self.eof_err());
        }
        let first_start = self.peek().map(|t| t.start).ok_or_else(|| self.eof_err())?;
        let open_raw = self.src[open.start..first_start].to_string();
        let open_gap = self.src[open.end..first_start].to_string();
        entries.push(self.parse_object_entry(depth + 1, None, &open_gap)?);
        loop {
            let t = self.peek().cloned_tok().ok_or_else(|| self.eof_err())?;
            match t.kind {
                Tok::Comma => {
                    let comma = self.bump().expect("peeked");
                    let nxt = self.peek().cloned_tok().ok_or_else(|| self.eof_err())?;
                    if matches!(nxt.kind, Tok::RBrace) {
                        let close = self.bump().expect("peeked");
                        let close_gap = self.src[comma.start..close.start].to_string();
                        Self::attach_close_entry(&mut entries, &close_gap);
                        let mut n = Node::new(Value::Map(std::mem::take(&mut entries)));
                        n.style = Style::Flow;
                        n.open_raw = Some(open_raw);
                        n.close_raw = Some(self.src[comma.start..close.end].to_string());
                        n.order = self.next_order();
                        return Ok(n);
                    }
                    let gap = self.src[comma.end..nxt.start].to_string();
                    // Verbatim prefix keeps the comma; logical trivia comes
                    // from the gap after it.
                    let mut entry = self.parse_object_entry(depth + 1, None, &gap)?;
                    entry.key_trivia.prefix_raw = Some(format!(",{gap}"));
                    entries.push(entry);
                }
                Tok::RBrace => {
                    let close_gap = self.src[self.last_end..t.start].to_string();
                    Self::attach_close_entry(&mut entries, &close_gap);
                    let _close = self.bump().expect("peeked");
                    let mut n = Node::new(Value::Map(std::mem::take(&mut entries)));
                    n.style = Style::Flow;
                    n.open_raw = Some(open_raw);
                    n.close_raw = Some(format!("{close_gap}}}"));
                    n.order = self.next_order();
                    return Ok(n);
                }
                _ => {
                    // Newline-separated entries without commas.
                    let gap = self.src[self.last_end..t.start].to_string();
                    if !gap.contains('\n') {
                        return Err(self.err(
                            t.start,
                            format!("expected ',' or '}}', got {}", Self::tok_name(&t.kind)),
                        ));
                    }
                    entries.push(self.parse_object_entry(depth + 1, Some(gap.clone()), &gap)?);
                }
            }
        }
    }

    /// Same-line head comment of an object close gap belongs to the last value.
    fn attach_close_entry(entries: &mut [Entry], close_gap: &str) {
        let head = close_gap.split('\n').next().unwrap_or("");
        if let Some(last) = entries.last_mut() {
            if last.value.trivia.inline.is_none() {
                last.value.trivia.inline = first_comment_in(head);
            }
        }
    }

    fn parse_object_entry(
        &mut self,
        depth: usize,
        prefix: Option<String>,
        logical_gap: &str,
    ) -> Result<Entry, Error> {
        let kt = self.peek().cloned_tok().ok_or_else(|| self.eof_err())?;
        let (text, raw_key) = match &kt.kind {
            Tok::Ident(s) | Tok::Str(s) => (s.clone(), self.src[kt.start..kt.end].to_string()),
            _ => {
                return Err(self.err(
                    kt.start,
                    format!("expected object key, got {}", Self::tok_name(&kt.kind)),
                ));
            }
        };
        self.bump();
        // `=` or `:` separator.
        let sept = self.peek().cloned_tok().ok_or_else(|| self.eof_err())?;
        let is_eq = matches!(sept.kind, Tok::Eq);
        let is_colon_sep = self.src.get(sept.start..sept.end) == Some(":");
        if !is_eq && !is_colon_sep {
            return Err(self.err(
                sept.start,
                format!("expected '=' or ':', got {}", Self::tok_name(&sept.kind)),
            ));
        }
        self.bump();
        let vstart = self.peek().map(|t| t.start).ok_or_else(|| self.eof_err())?;
        let sep = format!(
            "{}{}{}",
            &self.src[kt.end..sept.start],
            &self.src[sept.start..sept.end],
            &self.src[sept.end..vstart]
        );
        let mut value = self.parse_value(depth)?;
        value.trivia.prefix_raw = None;
        let mut key_trivia = Trivia::empty();
        if let Some(g) = prefix {
            key_trivia.prefix_raw = Some(g);
        }
        key_trivia.leading = Self::gap_comments(logical_gap);
        key_trivia.blanks_before = count_blanks(logical_gap);
        Ok(Entry {
            key: Key {
                text,
                repr: None,
                raw: Some(raw_key),
            },
            key_trivia,
            sep_raw: Some(sep),
            value,
        })
    }
}

trait ClonedTok {
    fn cloned_tok(&self) -> Option<Lexed>;
}

impl ClonedTok for Option<&Lexed> {
    fn cloned_tok(&self) -> Option<Lexed> {
        self.map(|t| Lexed {
            kind: t.kind.clone(),
            start: t.start,
            end: t.end,
        })
    }
}

/// First `#`/`//` comment in a same-line head slice, if any.
fn first_comment_in(head: &str) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    for marker in ["#", "//"] {
        if let Some(i) = head.find(marker) {
            let text = head[i..].trim_end().to_string();
            if best.as_ref().is_none_or(|(j, _)| i < *j) {
                best = Some((i, text));
            }
        }
    }
    best.map(|(_, s)| s)
}

/// Nest a block body under its labels: `type "l1" "l2" { body }` becomes
/// `type` → `l1` → `l2` → body (each level a Map entry). Wrapper levels get
/// an empty `open_raw` as a transparency marker (the real header lives on
/// the innermost body node); the emitter passes them through.
fn nest_labels(
    labels: Vec<(String, usize, usize)>,
    body: Node,
    parser: &mut Parser<'_>,
) -> Result<Node, Error> {
    let mut node = body;
    for (text, s, e) in labels.into_iter().rev() {
        let raw = parser.src[s..e].to_string();
        let mut map = Node::new(Value::Map(vec![Entry {
            key: Key {
                text,
                repr: None,
                raw: Some(raw),
            },
            key_trivia: Trivia::empty(),
            sep_raw: None,
            value: node,
        }]));
        map.order = parser.next_order();
        map.open_raw = Some(String::new());
        node = map;
    }
    Ok(node)
}

/// Parse HCL source into an IR document (root is always a Map).
pub fn parse_hcl(src: &str) -> Result<Doc, Error> {
    let toks = Lexer {
        src,
        bytes: src.as_bytes(),
        pos: 0,
    }
    .lex_all()?;
    // Unterminated block comments surface here with position.
    if let Some(i) = last_unterminated_comment(src, &toks) {
        let lc = offset_to_line_col(src, i);
        return Err(Error::parse(lc.line, lc.col, "unterminated block comment"));
    }
    let mut p = Parser {
        src,
        toks,
        pos: 0,
        last_end: 0,
        order: 0,
    };
    let entries = p.parse_body(0)?;
    // A stray `}` at top level is an error, not an empty body end.
    if let Some(t) = p.peek() {
        return Err(p.err(t.start, format!("unexpected {}", Parser::tok_name(&t.kind))));
    }
    let mut root = Node::new(Value::Map(entries));
    root.order = 0;
    let mut doc = Doc::new(root);
    doc.trailing.prefix_raw = Some(p.gap(src.len()).to_string());
    Ok(doc)
}

fn fill_key_logical(t: &mut Trivia, prefix: &str) {
    t.leading = extract_hash_comments(prefix);
    t.blanks_before = count_blanks(prefix);
}

/// `#`/`//` comments in a gap (no strings live in gaps: quoted spans are
/// tokens, so a naive scan is safe).
fn extract_hash_comments(gap: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in gap.split('\n') {
        let rest = line;
        // Skip `/* */` pairs naively: any `#` or `//` outside them counts.
        // Gaps here are small; a simple scan suffices.
        let mut i = 0;
        let bytes = rest.as_bytes();
        while i < bytes.len() {
            if rest[i..].starts_with("/*") {
                match rest[i + 2..].find("*/") {
                    Some(k) => {
                        i += 2 + k + 2;
                        continue;
                    }
                    None => break,
                }
            }
            if rest[i..].starts_with("//") || bytes[i] == b'#' {
                out.push(rest[i..].trim_end().to_string());
                break;
            }
            i += 1;
        }
        let _ = rest;
    }
    out
}

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

/// Locate an unterminated `/*` the lexer stopped at (if any).
fn last_unterminated_comment(src: &str, toks: &[Lexed]) -> Option<usize> {
    let end = toks.last().map(|t| t.end).unwrap_or(0);
    let rest = &src[end..];
    // If the remainder holds an opener without a closer, it is unterminated
    // (the lexer breaks instead of producing a bogus token).
    match rest.find("/*") {
        Some(i) if rest[i..].find("*/").is_none() => Some(end + i),
        _ => None,
    }
}

/// Emit an HCL document: verbatim when slices exist, canonical otherwise.
pub fn emit_hcl(doc: &Doc, _opt: &Options) -> Result<EmitOutput, Error> {
    let Value::Map(entries) = &doc.root.value else {
        return Err(Error::emit("hcl root must be a map"));
    };
    let mut warnings = Vec::new();
    let mut out = doc.root.trivia.prefix_raw.clone().unwrap_or_default();
    for e in entries {
        emit_entry(&mut out, &mut warnings, "", e)?;
    }
    if let Some(tail) = doc.trailing.prefix_raw.as_deref() {
        out.push_str(tail);
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
) -> Result<(), Error> {
    // Prefix renders exactly once here; bodies below never touch it.
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
    }
    match &e.value.value {
        Value::Map(children) if is_block(&e.value) => {
            // Transparent label wrappers (empty open marker) pass through;
            // the real header lives on the innermost body node.
            if e.value.open_raw.as_deref() == Some("") {
                let path2 = join_path(path, &e.key.text);
                for c in children {
                    emit_entry(out, warnings, &path2, c)?;
                }
                return Ok(());
            }
            // Block: verbatim header + children + verbatim close.
            let open = e
                .value
                .open_raw
                .as_deref()
                .ok_or_else(|| Error::emit(format!("block '{}' has no header", e.key.text)))?;
            out.push_str(open);
            let path2 = join_path(path, &e.key.text);
            for c in children {
                emit_entry(out, warnings, &path2, c)?;
            }
            if let Some(close) = e.value.close_raw.as_deref() {
                out.push_str(close);
            } else {
                out.push('\n');
                out.push('}');
            }
            // Block suffix (same-line comment after `}`) is stored on the
            // node; print it when present.
            if let Some(s) = e.value.trivia.suffix_raw.as_deref() {
                out.push_str(s);
            }
            Ok(())
        }
        _ => {
            out.push_str(e.key.raw.as_deref().unwrap_or(&e.key.text));
            out.push_str(e.sep_raw.as_deref().unwrap_or(" = "));
            emit_scalar(out, warnings, &e.key.text, &e.value)?;
            if let Some(s) = e.value.trivia.suffix_raw.as_deref() {
                out.push_str(s);
            } else {
                out.push('\n');
            }
            Ok(())
        }
    }
}

/// A Map node is a *block* unless it carries Flow style (object value).
fn is_block(node: &Node) -> bool {
    !matches!(node.style, Style::Flow)
}

fn join_path(base: &str, key: &str) -> String {
    if base.is_empty() {
        key.to_string()
    } else {
        format!("{base}.{key}")
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
        Value::Str(s) => out.push_str(&hcl_string(s)),
        Value::Datetime(d) => {
            warnings.push(Warning::new(
                path,
                WarningKind::TypeCoerced,
                "datetime has no HCL representation; emitted as string",
            ));
            out.push_str(&hcl_string(d));
        }
        Value::Array(items) => {
            if let Some(open) = node.open_raw.as_deref() {
                out.push_str(open);
                for item in items {
                    out.push_str(item.trivia.prefix_raw.as_deref().unwrap_or(""));
                    emit_scalar(out, warnings, path, item)?;
                    out.push_str(item.trivia.suffix_raw.as_deref().unwrap_or(""));
                }
                if let Some(close) = node.close_raw.as_deref() {
                    out.push_str(close);
                }
            } else {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    emit_scalar(out, warnings, path, item)?;
                    if i + 1 < items.len() {
                        out.push_str(", ");
                    }
                }
                out.push(']');
            }
        }
        Value::Map(entries) => {
            // Inline object (Flow style guaranteed by the parser for `{…}`;
            // programmatic Maps without Flow render as blocks elsewhere).
            if let Some(open) = node.open_raw.as_deref() {
                out.push_str(open);
                for en in entries {
                    out.push_str(en.key_trivia.prefix_raw.as_deref().unwrap_or(""));
                    out.push_str(en.key.raw.as_deref().unwrap_or(&en.key.text));
                    out.push_str(en.sep_raw.as_deref().unwrap_or(" = "));
                    emit_scalar(out, warnings, &en.key.text, &en.value)?;
                    out.push_str(en.value.trivia.suffix_raw.as_deref().unwrap_or(""));
                }
                if let Some(close) = node.close_raw.as_deref() {
                    out.push_str(close);
                }
            } else {
                out.push('{');
                for (i, en) in entries.iter().enumerate() {
                    out.push_str(&en.key.text);
                    out.push_str(" = ");
                    emit_scalar(out, warnings, &en.key.text, &en.value)?;
                    if i + 1 < entries.len() {
                        out.push_str(", ");
                    }
                }
                out.push('}');
            }
        }
        _ => {
            return Err(Error::emit("unexpected value kind in HCL output"));
        }
    }
    Ok(())
}

/// Double-quoted HCL string (`${…}` passes through literally by design).
fn hcl_string(s: &str) -> String {
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

impl Format for HclFormat {
    fn name(&self) -> &'static str {
        "hcl"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["hcl", "tf", "tfvars"]
    }

    fn parse(&self, src: &str) -> Result<Doc, Error> {
        parse_hcl(src)
    }

    fn emit(&self, doc: &Doc, opt: &Options) -> Result<EmitOutput, Error> {
        emit_hcl(doc, opt)
    }

    fn emit_logical(&self, doc: &Doc, opt: &Options) -> Result<EmitOutput, Error> {
        emit_hcl_logical(doc, opt)
    }
}

// ---------------------------------------------------------------------------
// Logical (canonical + warnings) emission for cross-format conversion.
// ---------------------------------------------------------------------------

/// Canonical HCL: attributes for scalars/lists/inline objects, blocks for
/// nested maps (Flow maps stay `{…}` objects). Comments preserved.
pub fn emit_hcl_logical(doc: &Doc, _opt: &Options) -> Result<EmitOutput, Error> {
    let Value::Map(entries) = &doc.root.value else {
        return Err(Error::emit("hcl root must be a map"));
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
        emit_logical_entry(&mut out, &mut warnings, "", e)?;
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

fn emit_logical_entry(
    out: &mut String,
    warnings: &mut Vec<Warning>,
    path: &str,
    e: &Entry,
) -> Result<(), Error> {
    let epath = join_path(path, &e.key.text);
    if let Some(a) = &e.value.anchor {
        warnings.push(Warning::new(
            &epath,
            WarningKind::AnchorExpanded,
            format!("anchor '{}' expanded (HCL has no anchors)", a.name),
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
    match &e.value.value {
        Value::Map(children) if is_block(&e.value) => {
            // Block with a bare (possibly quoted) type name.
            out.push_str(&hcl_ident(&e.key.text));
            out.push_str(" {\n");
            let flat = crate::logical::expand_entries(children, &epath, true, warnings);
            for c in &flat {
                emit_logical_entry_indented(out, warnings, &epath, c, 1)?;
            }
            out.push_str("}\n");
            Ok(())
        }
        _ => {
            out.push_str(&hcl_ident(&e.key.text));
            out.push_str(" = ");
            out.push_str(&emit_logical_value(warnings, &epath, &e.value)?);
            if let Some(c) = &e.value.trivia.inline {
                out.push(' ');
                out.push_str(&crate::logical::restyle_comment(c, "#").join(" "));
            }
            out.push('\n');
            Ok(())
        }
    }
}

fn emit_logical_entry_indented(
    out: &mut String,
    warnings: &mut Vec<Warning>,
    path: &str,
    e: &Entry,
    level: usize,
) -> Result<(), Error> {
    // Render at level 0, then indent every line (canonical blocks are
    // single-line-headed, so a prefix indent suffices per line).
    let mut tmp = String::new();
    emit_logical_entry(&mut tmp, warnings, path, e)?;
    let pad = "  ".repeat(level);
    for line in tmp.split_inclusive('\n') {
        if line.trim().is_empty() {
            out.push_str(line);
        } else {
            out.push_str(&pad);
            out.push_str(line);
        }
    }
    Ok(())
}

fn emit_logical_value(
    warnings: &mut Vec<Warning>,
    path: &str,
    node: &Node,
) -> Result<String, Error> {
    match &node.value {
        Value::Null => Ok("null".to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Number(n) => Ok(n.raw.clone()),
        Value::Str(s) => Ok(hcl_string(s)),
        Value::Datetime(d) => {
            warnings.push(Warning::new(
                path,
                WarningKind::TypeCoerced,
                "datetime has no HCL representation; emitted as string",
            ));
            Ok(hcl_string(d))
        }
        Value::Array(items) => {
            let mut parts = Vec::new();
            for it in items {
                if let Some(a) = &it.anchor {
                    warnings.push(Warning::new(
                        path,
                        WarningKind::AnchorExpanded,
                        format!("anchor '{}' expanded (HCL has no anchors)", a.name),
                    ));
                }
                parts.push(emit_logical_value(warnings, path, it)?);
            }
            Ok(format!("[{}]", parts.join(", ")))
        }
        Value::Map(entries) => {
            // Inline object form (blocks cannot nest in values).
            let flat = crate::logical::expand_entries(entries, path, true, warnings);
            let mut parts = Vec::new();
            for e in &flat {
                parts.push(format!(
                    "{} = {}",
                    hcl_ident(&e.key.text),
                    emit_logical_value(warnings, &join_path(path, &e.key.text), &e.value)?
                ));
            }
            Ok(format!("{{{}}}", parts.join(", ")))
        }
        _ => Err(Error::emit("unexpected value kind in HCL output")),
    }
}

/// Bare HCL identifier when possible, quoted otherwise.
fn hcl_ident(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return hcl_string(text),
    }
    if text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        text.to_string()
    } else {
        hcl_string(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgprism_core::values_equal;

    fn roundtrip(src: &str) -> Doc {
        let fmt = HclFormat;
        let doc = fmt.parse(src).expect("parse");
        let out = fmt.emit(&doc, &Options::default()).expect("emit");
        assert_eq!(out.text, src, "byte round-trip failed");
        assert!(out.warnings.is_empty());
        fmt.parse(&out.text).expect("re-parse")
    }

    #[test]
    fn attrs_blocks_comments_round_trip() {
        let src = "# head\nname = \"svc\" # inline\ncount = 3\nenabled = true\nports = [8000, 8001]\nobj = {a = 1}\n\nserver \"web\" {\n  host = \"x\" # c\n  nested {\n    deep = 1\n  }\n}\n";
        let a = HclFormat.parse(src).expect("parse");
        let b = roundtrip(src);
        assert!(values_equal(&a.root, &b.root));
        assert_eq!(
            a.root_keys_in_order(),
            vec!["name", "count", "enabled", "ports", "obj", "server"]
        );
    }

    #[test]
    fn expressions_are_positioned_errors() {
        for src in [
            "a = var.x\n",
            "a = foo(1)\n",
            "a = <<EOT\nx\nEOT\n",
            "a = [for x in y : x]\n",
        ] {
            assert!(HclFormat.parse(src).is_err(), "{src:?} must fail");
        }
    }

    #[test]
    fn attr_must_end_the_line() {
        assert!(HclFormat.parse("a = 1 b = 2\n").is_err());
    }
}
