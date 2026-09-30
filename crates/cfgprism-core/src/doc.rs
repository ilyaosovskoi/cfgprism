//! IR: tree nodes with attached "trivia" (comments, blank lines, style,
//! original key order and source spans).
//!
//! Design notes (see `docs/DESIGN.md`):
//! - Map entries are stored in a `Vec` — insertion order IS the source order.
//!   No sorting happens implicitly; an explicit sort must emit a warning.
//! - Numbers/strings keep their original lexical representation (`raw` / `repr`)
//!   so that `01` vs `1` or `no` vs `"no"` are never silently normalized.

use crate::error::Span;

/// Whitespace/comments attached to a node.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Trivia {
    /// Full-line comments above the node (without the `#` marker handling —
    /// each entry is the raw comment text, e.g. `"# hello"`).
    pub leading: Vec<String>,
    /// Inline comment on the same line (e.g. `key = 1 # comment`).
    pub inline: Option<String>,
    /// Blank lines directly before the node (0+).
    pub blanks_before: usize,
}

impl Trivia {
    /// Empty trivia (no comments, no blank lines).
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// `true` when the node carries any comment or blank line.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.leading.is_empty() && self.inline.is_none() && self.blanks_before == 0
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
    /// Original lexical representation preserved verbatim (e.g. `0xdecaf`).
    Original(String),
}

/// Map key with its original representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    /// Logical key text.
    pub text: String,
    /// How the key was written (`"quoted"` vs `plain`), if known.
    pub repr: Option<String>,
}

impl Key {
    /// Plain key (no quoting info).
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            repr: None,
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
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// Entry key.
    pub key: Key,
    /// Key trivia (comments on the key line before the value, if any).
    pub key_trivia: Trivia,
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
    /// Attached comments/blank lines.
    pub trivia: Trivia,
    /// Source style hint.
    pub style: Style,
    /// Source span, if parsed from text.
    pub span: Option<Span>,
    /// YAML anchor/alias, if any.
    pub anchor: Option<Anchor>,
    /// Source order index (0-based, assigned at parse time).
    pub order: usize,
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

    /// Short human-readable value summary for tests/docs.
    /// Not a serializer — strings are shown quoted, maps/arrays by length.
    #[must_use]
    pub fn display_value(&self) -> String {
        match &self.value {
            Value::Null => "null".to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => n.raw.clone(),
            Value::Str(s) => format!("\"{s}\""),
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
    /// Trailing comments/blank lines after the last node.
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
}
