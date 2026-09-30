//! Lossless JSON-family engine: lexer + parser + emitter shared by the
//! `json` (strict), `jsonc` and `json5` formats.
//!
//! Round-trip model: every gap between tokens (whitespace, newlines,
//! comments) is captured verbatim into `*_raw` IR fields, scalars keep
//! `Style::Original` spellings, and same-format emission concatenates the
//! stored slices — byte-identical by construction. Logical comments are
//! extracted from the same slices so cross-format conversion (Stage 4) can
//! carry them with explicit warnings.
//!
//! Note: [`emit_json`] reproduces the source dialect verbatim. Emitting into
//! strict JSON from a comment-carrying IR (cross-format, Stage 4) needs a
//! separate logical emitter that drops comments with `CommentDropped`
//! warnings — verbatim mode must not be used for that direction.

use cfgprism_core::{
    offset_to_line_col, Doc, EmitOutput, Entry, Error, Format, Key, Node, Number, NumberKind,
    Options, Style, Trivia, Value, Warning, WarningKind,
};

/// Parsing/emitting dialect knobs. Strict JSON is the all-`false` case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonDialect {
    /// `//` and `/* */` comments allowed.
    pub comments: bool,
    /// `[1, 2,]` / `{"a": 1,}` allowed.
    pub trailing_commas: bool,
    /// `'single quoted'` strings allowed.
    pub single_quotes: bool,
    /// `{unquoted: 1}` keys allowed.
    pub unquoted_keys: bool,
    /// `0x…` numbers allowed.
    pub hex_numbers: bool,
    /// Leading `+` on numbers allowed.
    pub plus_sign: bool,
    /// `.5` numbers allowed.
    pub leading_dot: bool,
    /// `5.` numbers allowed.
    pub trailing_dot: bool,
    /// `Infinity` / `NaN` (any case? no — exact case) allowed.
    pub inf_nan: bool,
    /// Backslash-newline inside strings allowed.
    pub line_continuation: bool,
    /// U+2028/U+2029 count as whitespace.
    pub unicode_whitespace: bool,
}

/// Strict JSON (RFC 8259): no comments, no trailing commas, double quotes only.
pub const STRICT_DIALECT: JsonDialect = JsonDialect {
    comments: false,
    trailing_commas: false,
    single_quotes: false,
    unquoted_keys: false,
    hex_numbers: false,
    plus_sign: false,
    leading_dot: false,
    trailing_dot: false,
    inf_nan: false,
    line_continuation: false,
    unicode_whitespace: false,
};

/// JSONC: JSON + comments + trailing commas.
pub const JSONC_DIALECT: JsonDialect = JsonDialect {
    comments: true,
    trailing_commas: true,
    ..STRICT_DIALECT
};

/// JSON5 (ECMAScript 5.1 literal grammar): everything on.
pub const JSON5_DIALECT: JsonDialect = JsonDialect {
    comments: true,
    trailing_commas: true,
    single_quotes: true,
    unquoted_keys: true,
    hex_numbers: true,
    plus_sign: true,
    leading_dot: true,
    trailing_dot: true,
    inf_nan: true,
    line_continuation: true,
    unicode_whitespace: true,
};

/// Hard nesting cap: input-driven stack overflow must be impossible,
/// including under fuzzing and on 2 MiB threads in debug builds (where a
/// bracket level costs two frames of several KiB each). 64 levels is plenty
/// for human-written configs; deeper machine output gets a clean error.
const MAX_DEPTH: usize = 64;

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum TokKind {
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Colon,
    Comma,
    Str,
    Num,
    True,
    False,
    Null,
    Ident,
    InfNan,
}

#[derive(Debug, Clone)]
struct Tok {
    kind: TokKind,
    start: usize,
    end: usize,
    /// Decoded payload for `Str` (and empty otherwise).
    text: String,
}

struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    dialect: JsonDialect,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str, dialect: JsonDialect) -> Self {
        Self {
            src,
            bytes: src.as_bytes(),
            pos: 0,
            dialect,
        }
    }

    fn err(&self, at: usize, msg: impl Into<String>) -> Error {
        let lc = offset_to_line_col(self.src, at);
        Error::parse(lc.line, lc.col, msg)
    }

    fn is_ws(&self, b: u8) -> bool {
        matches!(b, b' ' | b'\t' | b'\n' | b'\r')
    }

    fn skip_gap(&mut self) -> Result<(), Error> {
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if self.is_ws(b) {
                self.pos += 1;
                continue;
            }
            if self.dialect.unicode_whitespace
                && self.src[self.pos..].starts_with(['\u{2028}', '\u{2029}'])
            {
                self.pos += '\u{2028}'.len_utf8();
                continue;
            }
            // BOM only counts as whitespace at offset 0.
            if self.pos == 0 && self.src.starts_with('\u{FEFF}') {
                self.pos += '\u{FEFF}'.len_utf8();
                continue;
            }
            // Comments are gap content (kept verbatim in the source slice;
            // the parser validates/extracts them). Only for dialects with
            // `comments`; otherwise the gap check reports them as errors.
            if self.dialect.comments && b == b'/' {
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
                        None => {
                            return Err(self.err(self.pos, "unterminated block comment"));
                        }
                    }
                }
            }
            break;
        }
        Ok(())
    }

    fn lex_all(mut self) -> Result<Vec<Tok>, Error> {
        let mut toks = Vec::new();
        loop {
            self.skip_gap()?;
            if self.pos >= self.bytes.len() {
                break;
            }
            let start = self.pos;
            let b = self.bytes[self.pos];
            let kind = match b {
                b'{' => TokKind::LBrace,
                b'}' => TokKind::RBrace,
                b'[' => TokKind::LBracket,
                b']' => TokKind::RBracket,
                b':' => TokKind::Colon,
                b',' => TokKind::Comma,
                b'"' => {
                    let text = self.lex_string(b'"')?;
                    toks.push(Tok {
                        kind: TokKind::Str,
                        start,
                        end: self.pos,
                        text,
                    });
                    continue;
                }
                b'\'' if self.dialect.single_quotes => {
                    let text = self.lex_string(b'\'')?;
                    toks.push(Tok {
                        kind: TokKind::Str,
                        start,
                        end: self.pos,
                        text,
                    });
                    continue;
                }
                _ => {
                    if b == b'\'' {
                        return Err(self.err(start, "single-quoted strings need JSON5"));
                    }
                    if b.is_ascii_digit()
                        || b == b'-'
                        || b == b'+'
                        || (b == b'.' && self.dialect.leading_dot)
                    {
                        let (kind, text) = self.lex_number_or_inf()?;
                        toks.push(Tok {
                            kind,
                            start,
                            end: self.pos,
                            text,
                        });
                        continue;
                    }
                    if b.is_ascii_alphabetic() || b == b'_' || b == b'$' {
                        let (kind, text) = self.lex_word()?;
                        toks.push(Tok {
                            kind,
                            start,
                            end: self.pos,
                            text,
                        });
                        continue;
                    }
                    return Err(self.err(start, format!("unexpected character '{}'", b as char)));
                }
            };
            self.pos += 1;
            toks.push(Tok {
                kind,
                start,
                end: self.pos,
                text: String::new(),
            });
        }
        Ok(toks)
    }

    /// Lex a quoted string starting at the quote; `self.pos` points at it.
    fn lex_string(&mut self, quote: u8) -> Result<String, Error> {
        let start = self.pos;
        debug_assert_eq!(self.bytes[self.pos], quote);
        self.pos += 1;
        let mut out = String::new();
        loop {
            if self.pos >= self.bytes.len() {
                return Err(self.err(start, "unterminated string"));
            }
            let b = self.bytes[self.pos];
            if b == quote {
                self.pos += 1;
                return Ok(out);
            }
            if b == b'\\' {
                let esc_at = self.pos;
                self.pos += 1;
                if self.pos >= self.bytes.len() {
                    return Err(self.err(start, "unterminated string"));
                }
                let e = self.bytes[self.pos];
                match e {
                    b'"' => {
                        out.push('"');
                        self.pos += 1;
                    }
                    b'\'' => {
                        out.push('\'');
                        self.pos += 1;
                    }
                    b'\\' => {
                        out.push('\\');
                        self.pos += 1;
                    }
                    b'/' => {
                        out.push('/');
                        self.pos += 1;
                    }
                    b'b' => {
                        out.push('\u{8}');
                        self.pos += 1;
                    }
                    b'f' => {
                        out.push('\u{c}');
                        self.pos += 1;
                    }
                    b'n' => {
                        out.push('\n');
                        self.pos += 1;
                    }
                    b'r' => {
                        out.push('\r');
                        self.pos += 1;
                    }
                    b't' => {
                        out.push('\t');
                        self.pos += 1;
                    }
                    b'u' => {
                        self.pos += 1;
                        let cp = self.lex_hex4()?;
                        // Surrogate pair?
                        if (0xD800..0xDC00).contains(&cp) {
                            if self.bytes.get(self.pos) == Some(&b'\\')
                                && self.bytes.get(self.pos + 1) == Some(&b'u')
                            {
                                self.pos += 2;
                                let lo = self.lex_hex4()?;
                                if (0xDC00..0xE000).contains(&lo) {
                                    let c = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                                    out.push(char::from_u32(c).ok_or_else(|| {
                                        self.err(esc_at, "invalid surrogate pair")
                                    })?);
                                    continue;
                                }
                                return Err(self.err(esc_at, "invalid low surrogate"));
                            }
                            return Err(self.err(esc_at, "lone high surrogate"));
                        }
                        if (0xDC00..0xE000).contains(&cp) {
                            return Err(self.err(esc_at, "lone low surrogate"));
                        }
                        out.push(
                            char::from_u32(cp)
                                .ok_or_else(|| self.err(esc_at, "invalid unicode escape"))?,
                        );
                    }
                    b'\n' if self.dialect.line_continuation => {
                        self.pos += 1; // line continuation: contributes nothing
                    }
                    b'\r' if self.dialect.line_continuation => {
                        self.pos += 1;
                        if self.bytes.get(self.pos) == Some(&b'\n') {
                            self.pos += 1;
                        }
                    }
                    _ => {
                        return Err(self.err(esc_at, format!("invalid escape '\\{}'", e as char)));
                    }
                }
                continue;
            }
            // Raw characters: reject ASCII controls (and raw newlines) everywhere.
            if b < 0x20 {
                return Err(self.err(self.pos, "unescaped control character in string"));
            }
            // Decode one UTF-8 char.
            let ch = self.src[self.pos..]
                .chars()
                .next()
                .ok_or_else(|| self.err(self.pos, "invalid UTF-8 in string"))?;
            out.push(ch);
            self.pos += ch.len_utf8();
        }
    }

    fn lex_hex4(&mut self) -> Result<u32, Error> {
        if self.pos + 4 > self.bytes.len() {
            return Err(self.err(self.pos, "truncated unicode escape"));
        }
        let s = &self.src[self.pos..self.pos + 4];
        let v =
            u32::from_str_radix(s, 16).map_err(|_| self.err(self.pos, "invalid unicode escape"))?;
        self.pos += 4;
        Ok(v)
    }

    /// Lex a number or Infinity/NaN starting at `self.pos`.
    fn lex_number_or_inf(&mut self) -> Result<(TokKind, String), Error> {
        let start = self.pos;
        let rest = &self.src[start..];
        // Infinity / NaN (exact case, optional sign).
        for word in ["Infinity", "NaN"] {
            for sign in ["", "+", "-"] {
                let lit = format!("{sign}{word}");
                if rest.starts_with(&lit)
                    && !rest[lit.len()..]
                        .chars()
                        .next()
                        .is_some_and(|c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$')
                {
                    if !self.dialect.inf_nan {
                        return Err(self.err(start, format!("{lit} needs JSON5")));
                    }
                    self.pos += lit.len();
                    return Ok((TokKind::InfNan, String::new()));
                }
            }
        }
        // Numeric literal scan.
        let mut i = start;
        let b = self.bytes;
        if b.get(i) == Some(&b'-') || b.get(i) == Some(&b'+') {
            if b[i] == b'+' && !self.dialect.plus_sign {
                return Err(self.err(start, "leading '+' needs JSON5"));
            }
            i += 1;
        }
        // Hex.
        if rest[i - start..].starts_with("0x") || rest[i - start..].starts_with("0X") {
            if !self.dialect.hex_numbers {
                return Err(self.err(start, "hex numbers need JSON5"));
            }
            i += 2;
            let d0 = i;
            while i < b.len() && b[i].is_ascii_hexdigit() {
                i += 1;
            }
            if d0 == i {
                return Err(self.err(start, "invalid hex number"));
            }
            self.pos = i;
            self.check_num_trailer()?;
            return Ok((TokKind::Num, String::new()));
        }
        // Decimal: int part.
        let mut digits = 0;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            digits += 1;
        }
        // Fraction.
        let mut is_float = false;
        if b.get(i) == Some(&b'.') {
            // `5.` needs trailing_dot; `.5` handled by digits==0 + leading_dot.
            let after = i + 1;
            let mut frac = 0;
            let mut j = after;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
                frac += 1;
            }
            if digits == 0 && frac == 0 {
                return Err(self.err(start, "invalid number"));
            }
            if digits == 0 && !self.dialect.leading_dot {
                return Err(self.err(start, "leading-dot numbers need JSON5"));
            }
            if frac == 0 && !self.strict_dot_ok(digits) {
                return Err(self.err(start, "trailing-dot numbers need JSON5"));
            }
            is_float = true;
            i = j;
        } else if digits == 0 {
            return Err(self.err(start, "invalid number"));
        }
        // Exponent.
        if b.get(i) == Some(&b'e') || b.get(i) == Some(&b'E') {
            is_float = true;
            i += 1;
            if b.get(i) == Some(&b'-') || b.get(i) == Some(&b'+') {
                i += 1;
            }
            let e0 = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            if e0 == i {
                return Err(self.err(start, "invalid exponent"));
            }
        }
        // Strict leading-zero check: `01` is invalid JSON.
        let int_part: &str = {
            let s = &self.src[start..i];
            let t = s.trim_start_matches(['+', '-']);
            let t = t.split(['.', 'e', 'E']).next().unwrap_or(t);
            t
        };
        if !self.dialect.hex_numbers
            && !self.dialect.leading_dot
            && int_part.len() > 1
            && int_part.starts_with('0')
        {
            return Err(self.err(start, "leading zeros are not allowed"));
        }
        self.pos = i;
        self.check_num_trailer()?;
        let _ = is_float;
        Ok((TokKind::Num, String::new()))
    }

    fn strict_dot_ok(&self, digits: usize) -> bool {
        // Strict JSON never allows `5.`; JSON5 always does.
        let _ = digits;
        self.dialect.trailing_dot
    }

    /// A number must not be glued to an identifier char (`123abc`).
    fn check_num_trailer(&self) -> Result<(), Error> {
        if self.pos < self.bytes.len() {
            let c = self.bytes[self.pos];
            if c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c == b'.' {
                return Err(self.err(self.pos, "invalid number suffix"));
            }
        }
        Ok(())
    }

    /// Lex `true`/`false`/`null` or (JSON5) an unquoted identifier.
    fn lex_word(&mut self) -> Result<(TokKind, String), Error> {
        let start = self.pos;
        while self.pos < self.bytes.len() {
            let c = self.bytes[self.pos];
            if c.is_ascii_alphanumeric() || c == b'_' || c == b'$' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let word = &self.src[start..self.pos];
        let kind = match word {
            "true" => TokKind::True,
            "false" => TokKind::False,
            "null" => TokKind::Null,
            _ => {
                if !self.dialect.unquoted_keys {
                    return Err(self.err(start, format!("unexpected identifier '{word}'")));
                }
                // Validate identifier shape: must not start with a digit.
                if word.starts_with(|c: char| c.is_ascii_digit()) {
                    return Err(self.err(start, format!("invalid identifier '{word}'")));
                }
                TokKind::Ident
            }
        };
        Ok((kind, word.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Comment extraction (logical trivia from verbatim gaps)
// ---------------------------------------------------------------------------

/// Pull `//…` and `/*…*/` comments out of a gap; returns them in order.
/// Only called for dialects with `comments`; strict gaps are whitespace-only
/// (validated separately).
fn extract_comments(gap: &str) -> Vec<String> {
    let mut out = Vec::new();
    let b = gap.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
            let end = gap[i..].find('\n').map(|k| i + k).unwrap_or(gap.len());
            out.push(gap[i..end].trim_end().to_string());
            i = end;
        } else if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            match gap[i + 2..].find("*/") {
                Some(k) => {
                    out.push(gap[i..i + 2 + k + 2].to_string());
                    i += 2 + k + 2;
                }
                None => break, // unterminated: parser reports it later
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Count blank (whitespace-only) lines strictly inside a gap: the first
/// fragment shares a line with the previous token and the last fragment is
/// the next line's indent, so only interior fragments count.
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

/// Validate a strict-JSON gap: whitespace only.
fn check_strict_gap(
    src: &str,
    gap_start: usize,
    gap: &str,
    dialect: JsonDialect,
) -> Result<(), Error> {
    if dialect.comments {
        // Consume `//`/`/* */` comments, validating the text between them
        // as plain whitespace. `base` tracks the byte offset of `rest`
        // so error positions stay exact.
        let mut rest = gap;
        let mut base = gap_start;
        loop {
            match rest.find('/') {
                None => return check_gap_chars(src, base, rest),
                Some(k) => {
                    check_gap_chars(src, base, &rest[..k])?;
                    let after = &rest[k..];
                    if after.starts_with("//") {
                        let end = after.find('\n').map(|e| k + e).unwrap_or(rest.len());
                        base += end;
                        rest = &rest[end..];
                    } else if after.starts_with("/*") {
                        match after.find("*/") {
                            Some(e) => {
                                let skip = k + e + 2;
                                base += skip;
                                rest = &rest[skip..];
                            }
                            None => {
                                let lc = offset_to_line_col(src, base + k);
                                return Err(Error::parse(
                                    lc.line,
                                    lc.col,
                                    "unterminated block comment",
                                ));
                            }
                        }
                    } else {
                        let lc = offset_to_line_col(src, base + k);
                        return Err(Error::parse(lc.line, lc.col, "unexpected character '/'"));
                    }
                }
            }
        }
    }
    check_gap_chars(src, gap_start, gap)
}

fn check_gap_chars(src: &str, gap_start: usize, gap: &str) -> Result<(), Error> {
    for (off, ch) in gap.char_indices() {
        if !matches!(ch, ' ' | '\t' | '\n' | '\r') {
            let lc = offset_to_line_col(src, gap_start + off);
            return Err(Error::parse(
                lc.line,
                lc.col,
                format!("unexpected character '{ch}'"),
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct Parser<'a> {
    src: &'a str,
    toks: Vec<Tok>,
    pos: usize,
    /// End offset of the previously consumed token (gap tracking).
    last_end: usize,
    dialect: JsonDialect,
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

    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn bump(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned()?;
        self.last_end = t.end;
        self.pos += 1;
        Some(t)
    }

    fn err(&self, at: usize, msg: impl Into<String>) -> Error {
        let lc = offset_to_line_col(self.src, at);
        Error::parse(lc.line, lc.col, msg)
    }

    fn eof_err(&self) -> Error {
        let lc = offset_to_line_col(self.src, self.src.len());
        Error::parse(lc.line, lc.col, "unexpected end of input")
    }

    /// Attach logical trivia parsed from verbatim slices.
    fn fill_trivia(&self, node: &mut Node, prefix: Option<&str>, suffix: Option<&str>) {
        if !self.dialect.comments {
            if let Some(p) = prefix {
                node.trivia.blanks_before = count_blanks(p);
            }
            return;
        }
        if let Some(p) = prefix {
            node.trivia.leading = extract_comments(p);
            node.trivia.blanks_before = count_blanks(p);
        }
        if let Some(s) = suffix {
            let mut comments = extract_comments(s);
            if !comments.is_empty() {
                node.trivia.inline = Some(comments.remove(0));
                node.trivia.leading.extend(comments);
            }
        }
    }

    fn parse_value(&mut self, depth: usize) -> Result<Node, Error> {
        if depth > MAX_DEPTH {
            return Err(self.eof_err());
        }
        let t = self.peek().cloned().ok_or_else(|| self.eof_err())?;
        match t.kind {
            TokKind::LBrace => self.parse_map(depth),
            TokKind::LBracket => self.parse_array(depth),
            TokKind::Str => {
                self.bump();
                let raw = self.src[t.start..t.end].to_string();
                let mut n = Node::new(Value::Str(t.text));
                n.style = Style::Original(raw);
                n.order = self.next_order();
                Ok(n)
            }
            TokKind::Num => {
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
            TokKind::InfNan => {
                self.bump();
                let raw = self.src[t.start..t.end].to_string();
                let mut n = Node::new(Value::Number(Number {
                    raw: raw.clone(),
                    kind: NumberKind::Float,
                }));
                n.style = Style::Original(raw);
                n.order = self.next_order();
                Ok(n)
            }
            TokKind::True | TokKind::False => {
                self.bump();
                let raw = self.src[t.start..t.end].to_string();
                let mut n = Node::new(Value::Bool(t.kind == TokKind::True));
                n.style = Style::Original(raw);
                n.order = self.next_order();
                Ok(n)
            }
            TokKind::Null => {
                self.bump();
                let raw = self.src[t.start..t.end].to_string();
                let mut n = Node::new(Value::Null);
                n.style = Style::Original(raw);
                n.order = self.next_order();
                Ok(n)
            }
            TokKind::Ident => {
                // Bare `Infinity`/`NaN` values (signed forms are lexed as
                // numbers). As keys they stay identifiers (see parse_entry).
                if self.dialect.inf_nan && (t.text == "Infinity" || t.text == "NaN") {
                    self.bump();
                    let raw = self.src[t.start..t.end].to_string();
                    let mut n = Node::new(Value::Number(Number {
                        raw: raw.clone(),
                        kind: NumberKind::Float,
                    }));
                    n.style = Style::Original(raw);
                    n.order = self.next_order();
                    Ok(n)
                } else {
                    Err(self.err(t.start, format!("unexpected identifier '{}'", t.text)))
                }
            }
            _ => Err(self.err(t.start, format!("unexpected {}", tok_name(&t.kind)))),
        }
    }

    fn parse_array(&mut self, depth: usize) -> Result<Node, Error> {
        let open = self.bump().ok_or_else(|| self.eof_err())?;
        let mut items = Vec::new();
        // Gap right after `[`, before first item or `]`.
        let after_open = self.peek().map(|t| (t.start, t.kind.clone()));
        match after_open {
            Some((_, TokKind::RBracket)) => {
                let close = self.bump().expect("peeked");
                let raw = self.src[open.start..close.end].to_string();
                check_strict_gap(
                    self.src,
                    open.end,
                    &self.src[open.end..close.start],
                    self.dialect,
                )?;
                let mut n = Node::new(Value::Array(items));
                n.style = Style::Flow;
                n.open_raw = Some(raw);
                n.order = self.next_order();
                return Ok(n);
            }
            None => return Err(self.eof_err()),
            _ => {}
        }
        // Non-empty: open_raw = "[" + gap.
        let first_start = self.peek().map(|t| t.start).ok_or_else(|| self.eof_err())?;
        let open_gap = self.gap(first_start);
        check_strict_gap(self.src, self.last_end, open_gap, self.dialect)?;
        let open_raw = format!("[{open_gap}");
        // First item (prefix covered by open_raw).
        let mut first = self.parse_value(depth + 1)?;
        if self.dialect.comments {
            first.trivia.leading = extract_comments(open_gap);
        }
        first.trivia.blanks_before = count_blanks(open_gap);
        first.trivia.prefix_raw = None;
        items.push(first);
        // Rest: `,` item | `,`? `]`.
        loop {
            let t = self.peek().cloned().ok_or_else(|| self.eof_err())?;
            match t.kind {
                TokKind::Comma => {
                    // Capture value→comma gap BEFORE consuming the comma.
                    let gap1 = self.src[self.last_end..t.start].to_string();
                    check_strict_gap(self.src, self.last_end, &gap1, self.dialect)?;
                    let comma = self.bump().expect("peeked");
                    // Trailing comma?
                    let nxt = self.peek().cloned().ok_or_else(|| self.eof_err())?;
                    if nxt.kind == TokKind::RBracket {
                        if !self.dialect.trailing_commas {
                            return Err(self.err(comma.start, "trailing comma needs JSONC/JSON5"));
                        }
                        let close = self.bump().expect("peeked");
                        let tail_gap = &self.src[comma.end..close.start];
                        check_strict_gap(self.src, comma.end, tail_gap, self.dialect)?;
                        let (head, tail) = Trivia::split_gap(tail_gap);
                        // head (same line as comma) extends previous suffix.
                        let prev = items.last_mut().expect("non-empty");
                        let mut suf = gap1;
                        suf.push(',');
                        suf.push_str(head);
                        if self.dialect.comments {
                            let mut cs = extract_comments(head);
                            if !cs.is_empty() {
                                prev.trivia.inline = Some(cs.remove(0));
                            }
                        }
                        prev.trivia.suffix_raw = Some(suf);
                        let mut n = Node::new(Value::Array(items));
                        n.style = Style::Flow;
                        n.open_raw = Some(open_raw);
                        n.close_raw = Some(format!("{tail}]"));
                        n.order = self.next_order();
                        return Ok(n);
                    }
                    // Regular `, value`: gap after comma.
                    let vstart = nxt.start;
                    let gap2 = self.gap(vstart);
                    check_strict_gap(self.src, self.last_end, gap2, self.dialect)?;
                    let (head, tail) = Trivia::split_gap(gap2);
                    // head extends previous suffix (with comma).
                    let prev = items.last_mut().expect("non-empty");
                    let mut suf = gap1;
                    suf.push(',');
                    suf.push_str(head);
                    if self.dialect.comments {
                        let mut cs = extract_comments(head);
                        if !cs.is_empty() {
                            prev.trivia.inline = Some(cs.remove(0));
                        }
                    }
                    prev.trivia.suffix_raw = Some(suf);
                    let mut item = self.parse_value(depth + 1)?;
                    item.trivia.prefix_raw = Some(tail.to_string());
                    self.fill_trivia(&mut item, Some(tail), None);
                    items.push(item);
                }
                TokKind::RBracket => {
                    // Gap BEFORE consuming `]`: src is Copied out so the
                    // slice outlives the bump.
                    let src = self.src;
                    let gap = &src[self.last_end..t.start];
                    check_strict_gap(src, self.last_end, gap, self.dialect)?;
                    let (head, tail) = Trivia::split_gap(gap);
                    let head = head.to_string();
                    let tail = tail.to_string();
                    let close = self.bump().expect("peeked");
                    let prev = items.last_mut().expect("non-empty");
                    if !head.is_empty() {
                        prev.trivia.suffix_raw = Some(head);
                        if self.dialect.comments {
                            let mut cs =
                                extract_comments(prev.trivia.suffix_raw.as_deref().unwrap_or(""));
                            if !cs.is_empty() {
                                prev.trivia.inline = Some(cs.remove(0));
                            }
                        }
                    }
                    let mut n = Node::new(Value::Array(items));
                    n.style = Style::Flow;
                    n.open_raw = Some(open_raw);
                    n.close_raw = Some(format!("{tail}]"));
                    n.order = self.next_order();
                    let _ = close;
                    return Ok(n);
                }
                _ => {
                    return Err(self.err(
                        t.start,
                        format!("expected ',' or ']', got {}", tok_name(&t.kind)),
                    ))
                }
            }
        }
    }

    fn parse_map(&mut self, depth: usize) -> Result<Node, Error> {
        let open = self.bump().ok_or_else(|| self.eof_err())?;
        let mut entries = Vec::new();
        match self.peek().map(|t| (t.start, t.kind.clone())) {
            Some((_, TokKind::RBrace)) => {
                let close = self.bump().expect("peeked");
                check_strict_gap(
                    self.src,
                    open.end,
                    &self.src[open.end..close.start],
                    self.dialect,
                )?;
                let raw = self.src[open.start..close.end].to_string();
                let mut n = Node::new(Value::Map(entries));
                n.style = Style::Flow;
                n.open_raw = Some(raw);
                n.order = self.next_order();
                return Ok(n);
            }
            None => return Err(self.eof_err()),
            _ => {}
        }
        let first_start = self.peek().map(|t| t.start).ok_or_else(|| self.eof_err())?;
        let open_gap = self.gap(first_start);
        check_strict_gap(self.src, self.last_end, open_gap, self.dialect)?;
        let open_raw = format!("{{{open_gap}");
        entries.push(self.parse_entry(depth + 1, None)?);
        // Fill first entry logical trivia from open gap.
        {
            let first = entries.last_mut().expect("one entry");
            if self.dialect.comments {
                first.key_trivia.leading = extract_comments(open_gap);
            }
            first.key_trivia.blanks_before = count_blanks(open_gap);
            first.key_trivia.prefix_raw = None;
        }
        loop {
            let t = self.peek().cloned().ok_or_else(|| self.eof_err())?;
            match t.kind {
                TokKind::Comma => {
                    // Capture value→comma gap BEFORE consuming the comma.
                    let gap1 = self.src[self.last_end..t.start].to_string();
                    check_strict_gap(self.src, self.last_end, &gap1, self.dialect)?;
                    let comma = self.bump().expect("peeked");
                    let nxt = self.peek().cloned().ok_or_else(|| self.eof_err())?;
                    if nxt.kind == TokKind::RBrace {
                        if !self.dialect.trailing_commas {
                            return Err(self.err(comma.start, "trailing comma needs JSONC/JSON5"));
                        }
                        let close = self.bump().expect("peeked");
                        let tail_gap = &self.src[comma.end..close.start];
                        check_strict_gap(self.src, comma.end, tail_gap, self.dialect)?;
                        let (head, tail) = Trivia::split_gap(tail_gap);
                        let prev = entries.last_mut().expect("non-empty");
                        let mut suf = gap1;
                        suf.push(',');
                        suf.push_str(head);
                        if self.dialect.comments {
                            let mut cs = extract_comments(head);
                            if !cs.is_empty() {
                                prev.value.trivia.inline = Some(cs.remove(0));
                            }
                        }
                        prev.value.trivia.suffix_raw = Some(suf);
                        let mut n = Node::new(Value::Map(entries));
                        n.style = Style::Flow;
                        n.open_raw = Some(open_raw);
                        n.close_raw = Some(format!("{tail}}}"));
                        n.order = self.next_order();
                        return Ok(n);
                    }
                    let vstart = nxt.start;
                    let gap2 = self.gap(vstart);
                    check_strict_gap(self.src, self.last_end, gap2, self.dialect)?;
                    let (head, tail) = Trivia::split_gap(gap2);
                    let prev = entries.last_mut().expect("non-empty");
                    let mut suf = gap1;
                    suf.push(',');
                    suf.push_str(head);
                    if self.dialect.comments {
                        let mut cs = extract_comments(head);
                        if !cs.is_empty() {
                            prev.value.trivia.inline = Some(cs.remove(0));
                        }
                    }
                    prev.value.trivia.suffix_raw = Some(suf);
                    entries.push(self.parse_entry(depth + 1, Some(tail))?);
                }
                TokKind::RBrace => {
                    let src = self.src;
                    let gap = &src[self.last_end..t.start];
                    check_strict_gap(src, self.last_end, gap, self.dialect)?;
                    let (head, tail) = Trivia::split_gap(gap);
                    let head = head.to_string();
                    let tail = tail.to_string();
                    let close = self.bump().expect("peeked");
                    let prev = entries.last_mut().expect("non-empty");
                    if !head.is_empty() {
                        prev.value.trivia.suffix_raw = Some(head);
                        if self.dialect.comments {
                            let mut cs = extract_comments(
                                prev.value.trivia.suffix_raw.as_deref().unwrap_or(""),
                            );
                            if !cs.is_empty() {
                                prev.value.trivia.inline = Some(cs.remove(0));
                            }
                        }
                    }
                    let mut n = Node::new(Value::Map(entries));
                    n.style = Style::Flow;
                    n.open_raw = Some(open_raw);
                    n.close_raw = Some(format!("{tail}}}"));
                    n.order = self.next_order();
                    let _ = close;
                    return Ok(n);
                }
                _ => {
                    return Err(self.err(
                        t.start,
                        format!("expected ',' or '}}', got {}", tok_name(&t.kind)),
                    ))
                }
            }
        }
    }

    /// Parse one `"key" : value` entry. `prefix_tail` is the tail part of the
    /// gap after the preceding comma/open (already validated); `None` means
    /// the entry directly follows the container opening gap.
    fn parse_entry(&mut self, depth: usize, prefix_tail: Option<&str>) -> Result<Entry, Error> {
        let kt = self.peek().cloned().ok_or_else(|| self.eof_err())?;
        let (text, raw_key, style) = match kt.kind {
            TokKind::Str => {
                self.bump();
                (
                    kt.text.clone(),
                    self.src[kt.start..kt.end].to_string(),
                    Style::DoubleQuoted,
                )
            }
            TokKind::Ident => {
                self.bump();
                (
                    kt.text.clone(),
                    self.src[kt.start..kt.end].to_string(),
                    Style::Plain,
                )
            }
            _ => {
                return Err(self.err(
                    kt.start,
                    format!("expected object key, got {}", tok_name(&kt.kind)),
                ))
            }
        };
        // Strict JSON: keys must be double-quoted strings (lexer already
        // rejects single quotes; Ident needs unquoted_keys).
        if kt.kind == TokKind::Str
            && self.src.as_bytes()[kt.start] == b'\''
            && !self.dialect.single_quotes
        {
            return Err(self.err(kt.start, "single-quoted strings need JSON5"));
        }
        let colon = self.peek().cloned().ok_or_else(|| self.eof_err())?;
        if colon.kind != TokKind::Colon {
            return Err(self.err(
                colon.start,
                format!("expected ':', got {}", tok_name(&colon.kind)),
            ));
        }
        self.bump();
        let vstart = self.peek().map(|t| t.start).ok_or_else(|| self.eof_err())?;
        let sep_gap = self.gap(vstart);
        // sep = gap(key→colon) + ":" + gap(colon→value).
        let pre_gap = &self.src[kt.end..colon.start];
        check_strict_gap(self.src, kt.end, pre_gap, self.dialect)?;
        check_strict_gap(self.src, colon.end, sep_gap, self.dialect)?;
        let sep_raw = format!("{pre_gap}:{sep_gap}");
        let mut value = self.parse_value(depth)?;
        value.trivia.prefix_raw = None; // sep covers the gap; keep logical only
        if self.dialect.comments {
            let cs = extract_comments(sep_gap);
            if !cs.is_empty() {
                value.trivia.leading.extend(cs);
            }
            let cs2 = extract_comments(pre_gap);
            if !cs2.is_empty() {
                value.trivia.leading.extend(cs2);
            }
        }
        let mut key_trivia = Trivia::empty();
        if let Some(tail) = prefix_tail {
            key_trivia.prefix_raw = Some(tail.to_string());
            if self.dialect.comments {
                key_trivia.leading = extract_comments(tail);
                key_trivia.blanks_before = count_blanks(tail);
            } else {
                key_trivia.blanks_before = count_blanks(tail);
            }
        }
        let _ = style;
        Ok(Entry {
            key: Key {
                text,
                repr: None,
                raw: Some(raw_key),
            },
            key_trivia,
            sep_raw: Some(sep_raw),
            value,
        })
    }
}

fn tok_name(k: &TokKind) -> &'static str {
    match k {
        TokKind::LBrace => "'{'",
        TokKind::RBrace => "'}'",
        TokKind::LBracket => "'['",
        TokKind::RBracket => "']'",
        TokKind::Colon => "':'",
        TokKind::Comma => "','",
        TokKind::Str => "a string",
        TokKind::Num => "a number",
        TokKind::True | TokKind::False => "a boolean",
        TokKind::Null => "null",
        TokKind::Ident => "an identifier",
        TokKind::InfNan => "Infinity/NaN",
    }
}

/// Parse `src` with the given dialect into an IR document.
pub fn parse_json(src: &str, dialect: JsonDialect) -> Result<Doc, Error> {
    let toks = Lexer::new(src, dialect).lex_all()?;
    let mut p = Parser {
        src,
        toks,
        pos: 0,
        last_end: 0,
        dialect,
        order: 0,
    };
    if p.peek().is_none() {
        let lc = offset_to_line_col(src, src.len());
        return Err(Error::parse(lc.line, lc.col, "empty document"));
    }
    let first_start = p.peek().map(|t| t.start).unwrap_or(0);
    let head = p.gap(first_start);
    check_strict_gap(src, 0, head, dialect)?;
    let mut root = p.parse_value(0)?;
    root.trivia.prefix_raw = Some(head.to_string());
    if dialect.comments {
        root.trivia.leading = extract_comments(head);
    }
    root.trivia.blanks_before = count_blanks(head);
    // Trailing gap.
    if p.peek().is_some() {
        let t = p.peek().cloned().expect("peeked");
        return Err(p.err(t.start, "unexpected trailing content"));
    }
    let tail = &src[p.last_end..];
    check_strict_gap(src, p.last_end, tail, dialect)?;
    let (shead, stail) = Trivia::split_gap(tail);
    if !shead.is_empty() {
        root.trivia.suffix_raw = Some(shead.to_string());
    }
    let mut doc = Doc::new(root);
    doc.trailing.prefix_raw = Some(stail.to_string());
    if dialect.comments {
        let mut cs = extract_comments(shead);
        if !cs.is_empty() {
            doc.root.trivia.inline = Some(cs.remove(0));
            doc.trailing.leading.extend(cs);
        }
        doc.trailing.leading.extend(extract_comments(stail));
    }
    Ok(doc)
}

// ---------------------------------------------------------------------------
// Emitter
// ---------------------------------------------------------------------------

/// Escape a string as strict-JSON `"…"`.
pub fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
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

struct Emitter<'a> {
    opt: &'a Options,
    warnings: Vec<Warning>,
}

impl<'a> Emitter<'a> {
    /// Logical fallback for a scalar without verbatim spelling.
    fn pretty_scalar(&mut self, path: &str, node: &Node) -> Result<String, Error> {
        // Reached only without a verbatim spelling (programmatic docs);
        // anchor syntax cannot survive here.
        if let Some(a) = &node.anchor {
            self.warnings.push(Warning::new(
                path,
                WarningKind::AnchorExpanded,
                format!("anchor '{}' expanded (no verbatim spelling)", a.name),
            ));
        }
        match &node.value {
            Value::Null => Ok("null".to_string()),
            Value::Bool(b) => Ok(b.to_string()),
            Value::Number(n) => Ok(n.raw.clone()),
            Value::Str(s) => Ok(escape_json(s)),
            Value::Datetime(d) => {
                self.warnings.push(Warning::new(
                    path,
                    WarningKind::TypeCoerced,
                    "datetime has no JSON representation; emitted as string",
                ));
                Ok(escape_json(d))
            }
            _ => Err(Error::emit("pretty_scalar called on container")),
        }
    }

    fn scalar_text(&mut self, path: &str, node: &Node) -> Result<String, Error> {
        if let Style::Original(raw) = &node.style {
            return Ok(raw.clone());
        }
        // Containers never reach here.
        self.pretty_scalar(path, node)
    }

    fn emit_node(&mut self, path: &str, node: &Node, level: usize) -> Result<String, Error> {
        match &node.value {
            Value::Array(_) | Value::Map(_) => self.emit_container(path, node, level),
            _ => self.scalar_text(path, node),
        }
    }

    fn emit_container(&mut self, path: &str, node: &Node, level: usize) -> Result<String, Error> {
        let Some(open) = node.open_raw.as_deref() else {
            return self.pretty_container(path, node, level);
        };
        let mut out = String::from(open);
        match &node.value {
            Value::Array(items) => {
                for item in items {
                    let p = item.trivia.prefix_raw.as_deref().unwrap_or("");
                    out.push_str(p);
                    out.push_str(&self.emit_node(path, item, level + 1)?);
                    let s = item.trivia.suffix_raw.as_deref().unwrap_or("");
                    out.push_str(s);
                }
            }
            Value::Map(entries) => {
                for e in entries {
                    let p = e.key_trivia.prefix_raw.as_deref().unwrap_or("");
                    out.push_str(p);
                    out.push_str(e.key.raw.as_deref().unwrap_or(&e.key.text));
                    out.push_str(e.sep_raw.as_deref().unwrap_or(": "));
                    out.push_str(&self.emit_node(
                        &join_path(path, &e.key.text),
                        &e.value,
                        level + 1,
                    )?);
                    let s = e.value.trivia.suffix_raw.as_deref().unwrap_or("");
                    out.push_str(s);
                }
            }
            _ => {}
        }
        // Empty containers: open_raw holds the whole `[]`/`{}`.
        if node.close_raw.is_none() && !is_empty_container(node) {
            // Verbatim open but missing close: degenerate programmatic doc;
            // close canonically.
            out.push_str(if matches!(node.value, Value::Array(_)) {
                "]"
            } else {
                "}"
            });
        } else if let Some(close) = node.close_raw.as_deref() {
            out.push_str(close);
        }
        Ok(out)
    }

    /// Canonical pretty fallback for programmatic docs (no verbatim slices).
    /// Drops logical comments with explicit warnings — never silently.
    fn pretty_container(&mut self, path: &str, node: &Node, level: usize) -> Result<String, Error> {
        self.warn_trivia(path, &node.trivia);
        // Reached only without verbatim slices (programmatic docs).
        if let Some(a) = &node.anchor {
            self.warnings.push(Warning::new(
                path,
                WarningKind::AnchorExpanded,
                format!("anchor '{}' expanded (no verbatim spelling)", a.name),
            ));
        }
        let pad = " ".repeat(self.opt.indent.max(1) * level);
        let pad_in = " ".repeat(self.opt.indent.max(1) * (level + 1));
        match &node.value {
            Value::Array(items) => {
                if items.is_empty() {
                    return Ok("[]".to_string());
                }
                let mut out = String::from("[\n");
                for (i, item) in items.iter().enumerate() {
                    self.warn_trivia(path, &item.trivia);
                    out.push_str(&pad_in);
                    out.push_str(&self.emit_node(path, item, level + 1)?);
                    if i + 1 < items.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&pad);
                out.push(']');
                Ok(out)
            }
            Value::Map(entries) => {
                if entries.is_empty() {
                    return Ok("{}".to_string());
                }
                let mut out = String::from("{\n");
                for (i, e) in entries.iter().enumerate() {
                    self.warn_trivia(path, &e.key_trivia);
                    self.warn_trivia(path, &e.value.trivia);
                    out.push_str(&pad_in);
                    out.push_str(&escape_json(&e.key.text));
                    out.push_str(": ");
                    out.push_str(&self.emit_node(
                        &join_path(path, &e.key.text),
                        &e.value,
                        level + 1,
                    )?);
                    if i + 1 < entries.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&pad);
                out.push('}');
                Ok(out)
            }
            _ => Err(Error::emit("pretty_container called on scalar")),
        }
    }

    fn warn_trivia(&mut self, path: &str, t: &Trivia) {
        if !t.leading.is_empty() || t.inline.is_some() {
            self.warnings.push(Warning::new(
                path,
                WarningKind::CommentDropped,
                "comment has no verbatim slice in this output; dropped",
            ));
        }
    }
}

fn is_empty_container(node: &Node) -> bool {
    match &node.value {
        Value::Array(items) => items.is_empty(),
        Value::Map(entries) => entries.is_empty(),
        _ => false,
    }
}

fn join_path(base: &str, key: &str) -> String {
    if base.is_empty() {
        key.to_string()
    } else {
        format!("{base}.{key}")
    }
}

/// Emit `doc` as JSON-family text: verbatim when slices exist, canonical
/// pretty fallback (with warnings) for programmatic documents.
pub fn emit_json(doc: &Doc, opt: &Options) -> Result<EmitOutput, Error> {
    let mut e = Emitter {
        opt,
        warnings: Vec::new(),
    };
    let mut out = doc.root.trivia.prefix_raw.clone().unwrap_or_default();
    // Verbatim root?
    let root_text = if doc.root.open_raw.is_some()
        || matches!(doc.root.value, Value::Array(_) | Value::Map(_)) && doc.root.open_raw.is_none()
    {
        // Container without slices → pretty path handles it.
        e.emit_node("", &doc.root, 0)?
    } else {
        e.scalar_text("", &doc.root)?
    };
    out.push_str(&root_text);
    if let Some(s) = doc.root.trivia.suffix_raw.as_deref() {
        out.push_str(s);
    } else if !doc.root.trivia.is_empty() && doc.root.open_raw.is_none() {
        e.warn_trivia("", &doc.root.trivia);
    }
    if let Some(tail) = doc.trailing.prefix_raw.as_deref() {
        out.push_str(tail);
    }
    // Warn on logical comments that had no verbatim slice in verbatim mode?
    // Verbatim mode keeps everything; only the pretty fallback warns.
    Ok(EmitOutput {
        text: out,
        warnings: e.warnings,
    })
}

/// Strict JSON format (RFC 8259).
pub struct JsonFormat;

impl Format for JsonFormat {
    fn name(&self) -> &'static str {
        "json"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["json"]
    }

    fn parse(&self, src: &str) -> Result<Doc, Error> {
        parse_json(src, STRICT_DIALECT)
    }

    fn emit(&self, doc: &Doc, opt: &Options) -> Result<EmitOutput, Error> {
        emit_json(doc, opt)
    }

    fn emit_logical(&self, doc: &Doc, opt: &Options) -> Result<EmitOutput, Error> {
        emit_json_logical(doc, opt, JsonComments::Strip, false)
    }
}

// ---------------------------------------------------------------------------
// Logical (canonical + warnings) emission for cross-format conversion.
// ---------------------------------------------------------------------------

/// Comment policy for logical JSON output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonComments {
    /// Strict JSON: comments cannot be represented — dropped with warnings.
    Strip,
    /// JSONC/JSON5: comments preserved verbatim (markers travel in the IR).
    Keep,
}

/// Canonical logical emitter: verbatim slices ignored, values re-rendered,
/// losses reported. Used for every cross-format pair targeting JSON-family.
pub fn emit_json_logical(
    doc: &Doc,
    opt: &Options,
    comments: JsonComments,
    unquoted_keys: bool,
) -> Result<EmitOutput, Error> {
    let mut w = LogicalEmitter {
        indent: opt.indent.max(1),
        comments,
        unquoted_keys,
        warnings: Vec::new(),
    };
    let mut out = String::new();
    w.emit_trivia_top(&mut out, &doc.root.trivia, 0);
    out.push_str(&w.value(&doc.root, "", 0)?);
    out.push('\n');
    w.emit_trailing(&mut out, &doc.trailing);
    Ok(EmitOutput {
        text: out,
        warnings: w.warnings,
    })
}

struct LogicalEmitter {
    indent: usize,
    comments: JsonComments,
    unquoted_keys: bool,
    warnings: Vec<Warning>,
}

impl LogicalEmitter {
    fn pad(&self, level: usize) -> String {
        " ".repeat(self.indent * level)
    }

    fn warn_anchor(&mut self, path: &str, node: &Node) {
        if let Some(a) = &node.anchor {
            self.warnings.push(Warning::new(
                path,
                WarningKind::AnchorExpanded,
                format!("anchor '{}' expanded (JSON has no anchors)", a.name),
            ));
        }
    }

    /// Leading comments of a node: emit (Keep) or warn once (Strip).
    fn leading(&mut self, out: &mut String, path: &str, t: &Trivia, level: usize) {
        if t.leading.is_empty() {
            return;
        }
        match self.comments {
            JsonComments::Keep => {
                for c in &t.leading {
                    for line in crate::logical::restyle_comment(c, "//") {
                        out.push_str(&self.pad(level));
                        out.push_str(&line);
                        out.push('\n');
                    }
                }
            }
            JsonComments::Strip => {
                self.warnings.push(Warning::new(
                    path,
                    WarningKind::CommentDropped,
                    format!(
                        "{} comment(s) dropped (strict JSON has no comments)",
                        t.leading.len()
                    ),
                ));
            }
        }
    }

    fn inline_suffix(&mut self, path: &str, t: &Trivia) -> String {
        match (&t.inline, self.comments) {
            (Some(c), JsonComments::Keep) => {
                let line = crate::logical::restyle_comment(c, "//").join(" ");
                format!(" {line}")
            }
            (Some(_), JsonComments::Strip) => {
                self.warnings.push(Warning::new(
                    path,
                    WarningKind::CommentDropped,
                    "inline comment dropped (strict JSON has no comments)",
                ));
                String::new()
            }
            (None, _) => String::new(),
        }
    }

    fn emit_trivia_top(&mut self, out: &mut String, t: &Trivia, level: usize) {
        self.leading(out, "", t, level);
    }

    fn emit_trailing(&mut self, out: &mut String, t: &Trivia) {
        // Trailing file comments behave like top-level leading ones.
        self.leading(out, "", t, 0);
        if t.inline.is_some() {
            let s = self.inline_suffix("", t);
            if !s.is_empty() {
                out.push_str(s.trim_start());
                out.push('\n');
            }
        }
    }

    fn value(&mut self, node: &Node, path: &str, level: usize) -> Result<String, Error> {
        self.warn_anchor(path, node);
        match &node.value {
            Value::Null => Ok("null".to_string()),
            Value::Bool(b) => Ok(b.to_string()),
            Value::Number(n) => Ok(n.raw.clone()),
            Value::Str(s) => Ok(escape_json(s)),
            Value::Datetime(d) => {
                self.warnings.push(Warning::new(
                    path,
                    WarningKind::TypeCoerced,
                    "datetime has no JSON representation; emitted as string",
                ));
                Ok(escape_json(d))
            }
            Value::Array(items) => {
                if items.is_empty() {
                    return Ok("[]".to_string());
                }
                let mut out = String::from("[\n");
                for (i, item) in items.iter().enumerate() {
                    self.leading(&mut out, path, &item.trivia, level + 1);
                    out.push_str(&self.pad(level + 1));
                    out.push_str(&self.value(item, path, level + 1)?);
                    if i + 1 < items.len() {
                        out.push(',');
                    }
                    out.push_str(&self.inline_suffix(path, &item.trivia));
                    out.push('\n');
                }
                out.push_str(&self.pad(level));
                out.push(']');
                Ok(out)
            }
            Value::Map(entries) => {
                let flat = crate::logical::expand_entries(entries, path, false, &mut self.warnings);
                if flat.is_empty() {
                    return Ok("{}".to_string());
                }
                let mut out = String::from("{\n");
                for (i, e) in flat.iter().enumerate() {
                    let epath = join_path(path, &e.key.text);
                    self.leading(&mut out, &epath, &e.key_trivia, level + 1);
                    out.push_str(&self.pad(level + 1));
                    out.push_str(&self.key(&e.key.text));
                    out.push_str(": ");
                    out.push_str(&self.value(&e.value, &epath, level + 1)?);
                    if i + 1 < flat.len() {
                        out.push(',');
                    }
                    out.push_str(&self.inline_suffix(&epath, &e.value.trivia));
                    out.push('\n');
                }
                out.push_str(&self.pad(level));
                out.push('}');
                Ok(out)
            }
            _ => Err(Error::emit("unexpected value kind in JSON output")),
        }
    }

    fn key(&self, text: &str) -> String {
        if self.unquoted_keys && is_json5_ident(text) {
            text.to_string()
        } else {
            escape_json(text)
        }
    }
}

fn is_json5_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$' => {}
        _ => return false,
    }
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgprism_core::values_equal;

    fn roundtrip(src: &str, dialect: JsonDialect) -> Doc {
        let doc = parse_json(src, dialect).expect("parse");
        let out = emit_json(&doc, &Options::default()).expect("emit");
        assert_eq!(out.text, src, "byte round-trip failed");
        assert!(out.warnings.is_empty());
        parse_json(&out.text, dialect).expect("re-parse")
    }

    #[test]
    fn strict_round_trip() {
        let src = "{\n  \"z\": 1,\n  \"a\": [true, null, \"s\\u00e9\"],\n  \"m\": {}\n}\n";
        let a = parse_json(src, STRICT_DIALECT).expect("parse");
        let b = roundtrip(src, STRICT_DIALECT);
        assert!(values_equal(&a.root, &b.root));
        assert_eq!(a.root_keys_in_order(), vec!["z", "a", "m"]);
    }

    #[test]
    fn strict_rejects_comments_and_trailing_commas() {
        assert!(parse_json("{/*c*/}", STRICT_DIALECT).is_err());
        assert!(parse_json("{\"a\":1,}", STRICT_DIALECT).is_err());
        assert!(parse_json("{'a':1}", STRICT_DIALECT).is_err());
        assert!(parse_json("[01]", STRICT_DIALECT).is_err());
    }

    #[test]
    fn jsonc_round_trip_with_comments() {
        let src =
            "// head\n{\n  // about a\n  \"a\": 1, // inline\n  \"b\": [1, 2,], /* tail */\n}\n";
        let doc = roundtrip(src, JSONC_DIALECT);
        // Pre-key comments live on the entry's key trivia (uniform across
        // all formats); inline comments live on the value node.
        let Value::Map(entries) = &doc.root.value else {
            panic!("root must be a map");
        };
        let a_entry = entries.iter().find(|e| e.key.text == "a").expect("a entry");
        assert!(a_entry
            .key_trivia
            .leading
            .iter()
            .any(|c| c.contains("about a")));
        assert_eq!(a_entry.value.trivia.inline.as_deref(), Some("// inline"));
        assert!(doc.root.trivia.leading.iter().any(|c| c.contains("head")));
    }

    #[test]
    fn json5_round_trip_kitchen_sink() {
        let src = "{unquoted: 'sq', hex: 0xdecaf, plus: +1, dot: .5, tdot: 5., inf: Infinity, nan: NaN,}\n";
        let doc = roundtrip(src, JSON5_DIALECT);
        // 0xdecaf == 912559; numeric-aware comparison bridges the spelling.
        assert!(values_equal(
            doc.root.get("hex").expect("hex"),
            &Node::new(Value::Number(Number {
                raw: "912559".to_string(),
                kind: NumberKind::Int,
            }))
        ));
        assert_eq!(
            doc.root.get("unquoted").expect("unquoted").value,
            Value::Str("sq".to_string())
        );
    }

    #[test]
    fn errors_carry_line_col() {
        let err = parse_json("{\n  \"a\": bad\n}", STRICT_DIALECT).expect_err("must fail");
        assert_eq!((err.pos.line, err.pos.col), (2, 8));
    }

    #[test]
    fn deep_nesting_is_rejected_not_crashed() {
        let src = format!("{}[]", "[".repeat(200));
        assert!(parse_json(&src, STRICT_DIALECT).is_err());
        // The guard (64) also holds on small stacks: 100 valid levels must
        // fail cleanly rather than overflow a 2 MiB test-thread stack.
        let deep = format!("{}1{}", "[".repeat(100), "]".repeat(100));
        assert!(parse_json(&deep, STRICT_DIALECT).is_err());
        let ok = format!("{}1{}", "[".repeat(60), "]".repeat(60));
        assert!(parse_json(&ok, STRICT_DIALECT).is_ok());
    }

    #[test]
    fn programmatic_doc_pretty_prints() {
        let mut root = Node::new(Value::Map(vec![Entry {
            key: Key::plain("b"),
            key_trivia: Trivia::empty(),
            sep_raw: None,
            value: Node::new(Value::Bool(true)),
        }]));
        root.order = 0;
        let doc = Doc::new(root);
        let out = emit_json(&doc, &Options::default()).expect("emit");
        assert_eq!(out.text, "{\n  \"b\": true\n}");
    }
}
