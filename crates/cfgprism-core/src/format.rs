//! `Format` trait + registry + `convert()` pipeline.
//!
//! ```rust
//! use cfgprism_core::{Doc, EmitOutput, Format, FormatRegistry, Options};
//! # struct Echo;
//! # impl Format for Echo {
//! #   fn name(&self) -> &'static str { "echo" }
//! #   fn extensions(&self) -> &'static [&'static str] { &[] }
//! #   fn parse(&self, src: &str) -> Result<Doc, cfgprism_core::Error> { Ok(Doc::scalar_string(src)) }
//! #   fn emit(&self, doc: &Doc, _opt: &Options) -> Result<EmitOutput, cfgprism_core::Error> {
//! #     Ok(EmitOutput { text: "x".into(), warnings: vec![] })
//! #   }
//! # }
//! let r = FormatRegistry::new(vec![Box::new(Echo)]);
//! assert!(r.find("echo").is_some());
//! ```

use crate::doc::Doc;
use crate::error::Error;
use crate::options::Options;
use crate::warning::Warning;

/// Successful emission: text plus the warnings collected on the way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmitOutput {
    /// Emitted document text.
    pub text: String,
    /// Losses that occurred while emitting.
    pub warnings: Vec<Warning>,
}

/// Successful conversion: text plus warnings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvertOutput {
    /// Converted document text.
    pub text: String,
    /// Losses that occurred during conversion.
    pub warnings: Vec<Warning>,
}

/// Implemented once per format (`json`, `toml`, …).
///
/// Rules: `parse` must fail with a line:col `Error` on bad input and must
/// preserve source order in `Doc`; `emit` must never drop comments/styles
/// silently — every loss becomes a `Warning`.
pub trait Format {
    /// Canonical lowercase name (`"json"`, `"toml"`, …).
    fn name(&self) -> &'static str;
    /// File extensions (without dot) that map to this format.
    fn extensions(&self) -> &'static [&'static str];
    /// Parse source text into IR.
    fn parse(&self, src: &str) -> Result<Doc, Error>;
    /// Emit IR back to text, collecting warnings.
    fn emit(&self, doc: &Doc, opt: &Options) -> Result<EmitOutput, Error>;
}

/// Name → format lookup used by the CLI (`-f`/`-t` and extension sniffing).
pub struct FormatRegistry {
    formats: Vec<Box<dyn Format>>,
}

impl FormatRegistry {
    /// Build a registry from an explicit list (order = help order).
    pub fn new(formats: Vec<Box<dyn Format>>) -> Self {
        Self { formats }
    }

    /// Find a format by canonical name (case-insensitive).
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&dyn Format> {
        self.formats
            .iter()
            .find(|f| f.name().eq_ignore_ascii_case(name))
            .map(AsRef::as_ref)
    }

    /// All registered names in order.
    #[must_use]
    pub fn names(&self) -> Vec<&'static str> {
        self.formats.iter().map(|f| f.name()).collect()
    }
}

/// Parse with `from`, emit with `to`. Warnings from emission are returned
/// alongside the text; the caller (CLI/WASM) decides how to surface them.
pub fn convert(
    from: &dyn Format,
    to: &dyn Format,
    src: &str,
    opt: &Options,
) -> Result<ConvertOutput, Error> {
    let doc = from.parse(src)?;
    let out = to.emit(&doc, opt)?;
    Ok(ConvertOutput {
        text: out.text,
        warnings: out.warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    impl Format for Echo {
        fn name(&self) -> &'static str {
            "echo"
        }
        fn extensions(&self) -> &'static [&'static str] {
            &["echo"]
        }
        fn parse(&self, src: &str) -> Result<Doc, Error> {
            Ok(Doc::scalar_string(src))
        }
        fn emit(&self, doc: &Doc, _opt: &Options) -> Result<EmitOutput, Error> {
            let text = match &doc.root.value {
                crate::Value::Str(s) => s.clone(),
                _ => String::new(),
            };
            Ok(EmitOutput {
                text,
                warnings: Vec::new(),
            })
        }
    }

    #[test]
    fn convert_round_trips_through_registry() {
        let r = FormatRegistry::new(vec![Box::new(Echo)]);
        let from = r.find("ECHO").expect("case-insensitive lookup");
        let out = convert(from, from, "hi", &Options::default()).expect("convert");
        assert_eq!(out.text, "hi");
        assert!(out.warnings.is_empty());
    }
}
