//! `cfgprism-formats`: one module per format, all behind `trait Format`.
//!
//! Stage 2 set: `json` (strict), `jsonc`, `json5` (lossless in-house engine),
//! `toml` (via `toml_edit`), `dotenv` and `ini` (line-based in-house
//! parsers). Every format round-trips to itself byte-identically (within the
//! documented limitations); anything unrepresentable becomes a `Warning`,
//! never a silent drop.
//!
//! ```rust
//! use cfgprism_formats::{all_formats, detect_format};
//! assert_eq!(detect_format("cfg.json"), Some("json"));
//! assert_eq!(detect_format("cfg.toml"), Some("toml"));
//! assert_eq!(detect_format(".env"), Some("dotenv"));
//! assert_eq!(detect_format("a.yaml"), Some("yaml"));
//! assert_eq!(detect_format("noext"), None);
//! assert!(all_formats().find("json5").is_some());
//! ```

pub mod dotenv;
pub mod ini;
pub mod json;
pub mod json5;
pub mod jsonc;
pub mod logical;
pub mod toml;
pub mod util;
pub mod yaml;

mod registry;

pub use dotenv::DotenvFormat;
pub use ini::IniFormat;
pub use json::JsonFormat;
pub use json5::Json5Format;
pub use jsonc::JsoncFormat;
pub use logical::convert_text;
pub use registry::{all_formats, detect_format, supported_names};
pub use toml::TomlFormat;
pub use yaml::YamlFormat;

/// Plain IR key (canonical logical output builds keys from text).
pub(crate) fn key_text(text: &str) -> cfgprism_core::Key {
    cfgprism_core::Key::plain(text.to_string())
}
