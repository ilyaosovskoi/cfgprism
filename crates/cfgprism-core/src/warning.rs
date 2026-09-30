//! Explicit loss warnings: the converter never drops information silently.
//!
//! ```rust
//! use cfgprism_core::{Warning, WarningKind};
//! let w = Warning::new("a.b", WarningKind::CommentDropped, "plain JSON drops comments");
//! assert_eq!(w.to_string(), "[comment-dropped] a.b: plain JSON drops comments");
//! ```

use std::fmt;

/// Machine-readable loss categories. `#[non_exhaustive]` so formats can grow
/// the taxonomy without breaking the core; matching code must use a wildcard.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum WarningKind {
    /// A comment could not be represented in the target format.
    CommentDropped,
    /// A YAML anchor/alias was expanded (duplicated) into the target.
    AnchorExpanded,
    /// Quoting/block/flow style was normalized.
    StyleNormalized,
    /// A scalar type was coerced (`no` -> `false`, date -> string, …).
    TypeCoerced,
    /// Key order changed (only when explicitly requested).
    KeyReordered,
    /// Numeric precision or radix was lost.
    LossyNumber,
    /// Target format cannot express the construct at all.
    UnsupportedConstruct,
}

impl WarningKind {
    /// Stable kebab-case code for CLI/WASM output.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::CommentDropped => "comment-dropped",
            Self::AnchorExpanded => "anchor-expanded",
            Self::StyleNormalized => "style-normalized",
            Self::TypeCoerced => "type-coerced",
            Self::KeyReordered => "key-reordered",
            Self::LossyNumber => "lossy-number",
            Self::UnsupportedConstruct => "unsupported-construct",
        }
    }
}

impl fmt::Display for WarningKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code())
    }
}

/// One loss event: JSON-pointer-ish path + kind + human message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    /// Document path (`""` for root, `a.b[0]` otherwise).
    pub path: String,
    /// Loss category.
    pub kind: WarningKind,
    /// Human-readable detail.
    pub message: String,
}

impl Warning {
    /// Create a warning.
    pub fn new(path: impl Into<String>, kind: WarningKind, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = if self.path.is_empty() {
            "<root>".to_string()
        } else {
            self.path.clone()
        };
        write!(f, "[{}] {}: {}", self.kind, path, self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_format() {
        let w = Warning::new(
            "a.b",
            WarningKind::CommentDropped,
            "plain JSON drops comments",
        );
        assert_eq!(
            w.to_string(),
            "[comment-dropped] a.b: plain JSON drops comments"
        );
    }
}
