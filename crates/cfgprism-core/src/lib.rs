//! cfgprism-core: intermediate representation (IR), `Format` trait,
//! warnings and position-aware errors.
//!
//! ```rust
//! use cfgprism_core::{Doc, FormatRegistry, Options};
//!
//! let doc = Doc::scalar_string("hi");
//! assert_eq!(doc.root.display_value(), "\"hi\"");
//! ```

mod doc;
mod equiv;
mod error;
mod format;
mod options;
mod warning;

pub use doc::{Anchor, Doc, Entry, Key, Node, Number, NumberKind, Style, Trivia, Value};
pub use equiv::{values_equal, values_equal_unordered};
pub use error::{offset_to_line_col, Error, ErrorKind, LineCol, LineIndex, Span};
pub use format::{convert, ConvertOutput, EmitOutput, Format, FormatRegistry};
pub use options::Options;
pub use warning::{Warning, WarningKind};
