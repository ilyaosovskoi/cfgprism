//! `cfgprism-formats`: one module per format, all behind `trait Format`.
//!
//! Stage 1: only a `json` stub (value-preserving, trivia NOT yet preserved —
//! that is Stage 2) plus the format registry / extension sniffing used by
//! the CLI. Every other name resolves to a precise
//! `unsupported format` error, never a panic.
//!
//! ```rust
//! use cfgprism_formats::{all_formats, detect_format};
//! assert_eq!(detect_format("cfg.json"), Some("json"));
//! assert_eq!(detect_format("noext"), None);
//! assert!(all_formats().find("json").is_some());
//! ```

mod json_stub;
mod registry;

pub use json_stub::JsonStubFormat;
pub use registry::{all_formats, detect_format, supported_names};
