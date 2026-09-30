//! IR: tree nodes with attached "trivia" (comments, blank lines, style,
//! original key order and source spans).
//!
//! Design notes (see `docs/DESIGN.md`):
//! - Map entries are stored in a `Vec` — insertion order IS the source order.
//!   No sorting happens implicitly; an explicit sort must emit a warning.
//! - Numbers/strings keep their original lexical representation (`raw` / `repr`)
//!   so that `01` vs `1` or `no` vs `"no"` are never silently normalized.
//! - Byte round-trip: parsers may fill the `*_raw` fields with verbatim
//!   source slices. A same-format emitter reproduces bytes by concatenating
//!   `prefix_raw + key.raw + sep_raw + value …`; cross-format emitters ignore
//!   every `*_raw` field and use logical values plus `Warning`s instead.

use crate::error::Span;

/// Whitespace/comments attached to a node.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Trivia {
    /// Full-line comments above the node (raw comment text, e.g. `"# hello"`).
    pub leading: Vec<String>,
    /// Inline comment on the same line (e.g. `key = 1 # comment` → `"# comment"`).
    pub inline: Option<String>,
    /// Blank lines directly before the node (0+).
    pub blanks_before: usize,
    /// Verbatim source bytes preceding this node (indent, blank lines,
    /// comments). Same-format emitters reproduce it as-is.
    pub prefix_raw: Option<String>,
    /// Verbatim source bytes following the value on the same line
    /// (e.g. `" # comment"`). Same-format emitters reproduce it as-is.
    pub suffix_raw: Option<String>,
}

impl Trivia {
    /// Empty trivia (no comments, no blank lines, no verbatim slices).
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// `true` when the node carries no *logical* comment or blank line
    /// (verbatim slices are ignored: they are a rendering detail).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.leading.is_empty() && self.inline.is_none() && self.blanks_before == 0
    }

    /// Split a gap of whitespace/comments at the first newline.
    /// Returns `(same_line_head, rest_from_newline)`.
    #[must_use]
    pub fn split_gap(gap: &str) -> (&str, &str) {
        match gap.find('\n') {
            Some(i) => (&gap[..i], &gap[i..]),
            None => (gap, ""),
        }
    }
}

/// How a scalar was written in the source document.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Style {
    /// No information (e.g. constructed programmatically).
    #[default]
    Unknown,
    /// Bare value (`true`, `42`, `hello`).
    Plain,
    /// `"double quoted"`.
    DoubleQuoted,
    /// `'single quoted'`.
    SingleQuoted,
    /// YAML `|` literal block.
    Literal,
    /// YAML `>` folded block.
    Folded,
    /// Inline flow (`{a: 1}`, `[1, 2]`).
    Flow,
    /// Original lexical representation, reproduced verbatim (e.g. `0xdecaf`).
    Original(String),
}

/// Map key with its original representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    /// Logical key text.
    pub text: String,
    /// How the key was written (`"quoted"` vs `plain`), if known.
    pub repr: Option<String>,
    /// Verbatim source slice of the key (e.g. `"'a b'"`, `"export FOO"`).
    pub raw: Option<String>,
}

impl Key {
    /// Plain key (no quoting info).
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            repr: None,
            raw: None,
        }
    }
}

/// Number that keeps both value and original spelling.
#[derive(Debug, Clone, PartialEq)]
pub struct Number {
    /// Original lexical form (`"01"`, `"0o17"`, `"1.0"`).
    pub raw: String,
    /// Logical kind.
    pub kind: NumberKind,
}

/// Logical number kind (no silent coercion: the emitter decides via `raw`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum NumberKind {
    /// Signed integer.
    Int,
    /// Floating point.
    Float,
}

/// YAML anchor/alias attachment (`None` outside YAML).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
    /// Anchor name (without `&`/`*`).
    pub name: String,
    /// `true` for alias (`*name`), `false` for definition (`&name`).
    pub is_alias: bool,
}

/// A map entry: key + value node. The node's own `key` field is `None`;
/// the key lives here so trivia can attach to either side later.
///
/// Convention (uniform across formats): comments *before* the entry live on
/// `key_trivia.leading`; the *inline* (same-line) comment lives on
/// `value.trivia.inline`. Array items have no key, so both live on the item's
/// own `trivia`.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// Entry key.
    pub key: Key,
    /// Key trivia (comments on the key line before the value, if any).
    pub key_trivia: Trivia,
    /// Verbatim bytes between key end and value start (e.g. `": "`, `" = "`).
    pub sep_raw: Option<String>,
    /// Value node.
    pub value: Node,
}

/// Scalar/collection value.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Value {
    /// `null` / `~` / missing.
    Null,
    /// Boolean.
    Bool(bool),
    /// Number (keeps raw spelling).
    Number(Number),
    /// String.
    Str(String),
    /// TOML/YAML datetime, kept as raw text (e.g. `"1979-05-27T07:32:00Z"`).
    /// Targets without datetimes stringify it with a `TypeCoerced` warning.
    Datetime(String),
    /// Ordered list.
    Array(Vec<Node>),
    /// Ordered map — `Vec` order is canonical.
    Map(Vec<Entry>),
}

/// A single IR node: value + trivia + style + span + optional anchor.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    /// Value payload.
    pub value: Value,
    /// Attached comments/blank lines (+ verbatim slices).
    pub trivia: Trivia,
    /// Source style hint (`Original` reproduces the scalar verbatim).
    pub style: Style,
    /// Source span, if parsed from text.
    pub span: Option<Span>,
    /// YAML anchor/alias, if any.
    pub anchor: Option<Anchor>,
    /// Source order index (0-based, assigned at parse time).
    pub order: usize,
    /// Verbatim container opening: bracket plus following gap
    /// (e.g. `"{\n  "`, `"["`, `"[s] ; c\n"` for INI sections).
    /// `None` for scalars or programmatic nodes.
    pub open_raw: Option<String>,
    /// Verbatim container closing: gap plus bracket
    /// (e.g. `"\n}"`, `"]"`). `None` for scalars or programmatic nodes.
    /// For empty containers `open_raw` holds the whole `"[]"`/`"{}"`.
    pub close_raw: Option<String>,
}

impl Node {
    /// Bare node with empty trivia and unknown style.
    pub fn new(value: Value) -> Self {
        Self {
            value,
            trivia: Trivia::empty(),
            style: Style::Unknown,
            span: None,
            anchor: None,
            order: 0,
            open_raw: None,
            close_raw: None,
        }
    }

    /// `true` for `Value::Map`.
    #[must_use]
    pub fn is_map(&self) -> bool {
        matches!(self.value, Value::Map(_))
    }

    /// Look up a map entry by key text (linear scan preserves order semantics).
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Node> {
        match &self.value {
            Value::Map(entries) => entries.iter().find(|e| e.key.text == key).map(|e| &e.value),
            _ => None,
        }
    }

    /// `true` for empty arrays/maps (verbatim emitters store the whole
    /// `"[]"`/`"{}"` in `open_raw` for these).
    #[must_use]
    pub fn is_empty_container(&self) -> bool {
        match &self.value {
            Value::Array(items) => items.is_empty(),
            Value::Map(entries) => entries.is_empty(),
            _ => false,
        }
    }

    /// Short human-readable value summary for tests/docs.
    /// Not a serializer — strings are shown quoted, maps/arrays by length.
    #[must_use]
    pub fn display_value(&self) -> String {
        match &self.value {
            Value::Null => "null".to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => n.raw.clone(),
            Value::Str(s) => format!("\"{s}\""),
            Value::Datetime(d) => format!("<datetime {d}>"),
            Value::Array(items) => format!("[{} items]", items.len()),
            Value::Map(entries) => format!("{{{} keys}}", entries.len()),
        }
    }
}

/// Top-level document: root node + file-level trailing trivia.
#[derive(Debug, Clone, PartialEq)]
pub struct Doc {
    /// Root node (usually a map, but any value is legal).
    pub root: Node,
    /// Comments/blank lines after the last node.
    /// `trailing.prefix_raw` holds the verbatim tail bytes when known.
    pub trailing: Trivia,
}

impl Doc {
    /// Document from a root node.
    pub fn new(root: Node) -> Self {
        Self {
            root,
            trailing: Trivia::empty(),
        }
    }

    /// Scalar-string document (helper for doctests).
    #[must_use]
    pub fn scalar_string(text: &str) -> Self {
        Self::new(Node::new(Value::Str(text.to_string())))
    }

    /// Key texts of a root map in order (empty for non-maps).
    #[must_use]
    pub fn root_keys_in_order(&self) -> Vec<String> {
        match &self.root.value {
            Value::Map(entries) => entries.iter().map(|e| e.key.text.clone()).collect(),
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map_doc(keys: &[&str]) -> Doc {
        let entries = keys
            .iter()
            .enumerate()
            .map(|(i, k)| Entry {
                key: Key::plain(*k),
                key_trivia: Trivia::empty(),
                sep_raw: None,
                value: {
                    let mut n = Node::new(Value::Number(Number {
                        raw: i.to_string(),
                        kind: NumberKind::Int,
                    }));
                    n.order = i;
                    n
                },
            })
            .collect();
        Doc::new(Node::new(Value::Map(entries)))
    }

    #[test]
    fn map_preserves_insertion_order() {
        let doc = map_doc(&["z", "a", "m"]);
        assert_eq!(doc.root_keys_in_order(), vec!["z", "a", "m"]);
        assert_eq!(doc.root.get("a").unwrap().display_value(), "1");
    }

    #[test]
    fn trivia_empty_by_default() {
        let n = Node::new(Value::Null);
        assert!(n.trivia.is_empty());
    }

    #[test]
    fn number_keeps_raw_spelling() {
        let n = Node::new(Value::Number(Number {
            raw: "01".to_string(),
            kind: NumberKind::Int,
        }));
        assert_eq!(n.display_value(), "01");
    }

    #[test]
    fn split_gap_splits_at_first_newline() {
        assert_eq!(Trivia::split_gap(" // c\n  "), (" // c", "\n  "));
        assert_eq!(Trivia::split_gap("  "), ("  ", ""));
    }
}
