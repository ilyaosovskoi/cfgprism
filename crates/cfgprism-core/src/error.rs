//! Position-aware errors: every error carries `line:col`.

use std::fmt;
use thiserror::Error;

/// 1-based line/column position in the source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LineCol {
    /// 1-based line number.
    pub line: u32,
    /// 1-based column number.
    pub col: u32,
}

impl LineCol {
    /// Create a new position. Both coordinates must be >= 1.
    #[must_use]
    pub fn new(line: u32, col: u32) -> Self {
        debug_assert!(line >= 1 && col >= 1);
        Self { line, col }
    }
}

impl fmt::Display for LineCol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.col)
    }
}

/// Byte-span with start/end positions (both 1-based).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    /// Start position (inclusive).
    pub start: LineCol,
    /// End position (inclusive).
    pub end: LineCol,
}

impl Span {
    /// Single-point span.
    #[must_use]
    pub fn point(line: u32, col: u32) -> Self {
        let p = LineCol::new(line, col);
        Self { start: p, end: p }
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.start)
    }
}

/// Machine-readable error categories.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// Syntax error in the source document.
    Parse,
    /// Target document cannot be emitted (e.g. unsupported construct + strict).
    Emit,
    /// Unknown or unsupported format name.
    UnsupportedFormat,
    /// I/O failure (file not found, broken stdin, …).
    Io,
    /// One or more warnings escalated via `--strict`.
    StrictWarnings,
    /// Anything else (reserved for format-specific codes).
    Other(String),
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse => write!(f, "parse error"),
            Self::Emit => write!(f, "emit error"),
            Self::UnsupportedFormat => write!(f, "unsupported format"),
            Self::Io => write!(f, "i/o error"),
            Self::StrictWarnings => write!(f, "strict warnings"),
            Self::Other(s) => write!(f, "{s}"),
        }
    }
}

/// cfgprism error. Always carries a position when it comes from parsing.
///
/// ```rust
/// use cfgprism_core::Error;
/// let e = Error::parse(3, 7, "unexpected `}`");
/// assert_eq!(e.to_string(), "3:7: parse error: unexpected `}`");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub struct Error {
    /// Error category.
    pub kind: ErrorKind,
    /// Position (dummy 1:1 when not applicable, e.g. I/O).
    pub pos: LineCol,
    /// Human-readable message.
    pub message: String,
}

impl Error {
    /// Generic constructor.
    pub fn new(kind: ErrorKind, line: u32, col: u32, message: impl Into<String>) -> Self {
        Self {
            kind,
            pos: LineCol::new(line.max(1), col.max(1)),
            message: message.into(),
        }
    }

    /// Syntax error at `line:col`.
    pub fn parse(line: u32, col: u32, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Parse, line, col, message)
    }

    /// Emit error (position defaults to 1:1).
    pub fn emit(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Emit, 1, 1, message)
    }

    /// Unsupported format name.
    pub fn unsupported_format(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::UnsupportedFormat, 1, 1, message)
    }

    /// I/O error wrapper.
    pub fn io(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Io, 1, 1, message)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}: {}", self.pos, self.kind, self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_includes_line_col() {
        let e = Error::parse(3, 7, "unexpected `}`");
        assert_eq!(e.to_string(), "3:7: parse error: unexpected `}`");
        assert_eq!(e.pos.line, 3);
        assert_eq!(e.pos.col, 7);
    }

    #[test]
    fn unsupported_format_has_dummy_pos() {
        let e = Error::unsupported_format("no format 'xml'");
        assert!(e.to_string().starts_with("1:1: unsupported format:"));
    }
}
