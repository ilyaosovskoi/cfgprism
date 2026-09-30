//! Format registry + extension sniffing.
//!
//! `all_formats()` grows every stage; the CLI and detection logic do not change.

use cfgprism_core::FormatRegistry;

use crate::{
    DotenvFormat, IniFormat, Json5Format, JsonFormat, JsoncFormat, TomlFormat, YamlFormat,
};

/// Build the registry with every supported format (help order).
#[must_use]
pub fn all_formats() -> FormatRegistry {
    FormatRegistry::new(vec![
        Box::new(JsonFormat),
        Box::new(JsoncFormat),
        Box::new(Json5Format),
        Box::new(TomlFormat),
        Box::new(YamlFormat),
        Box::new(DotenvFormat),
        Box::new(IniFormat),
    ])
}

/// Canonical supported names in help order.
#[must_use]
pub fn supported_names() -> Vec<&'static str> {
    all_formats().names()
}

/// Guess the canonical format name from a file path.
///
/// Extension match is case-insensitive; additionally the basename `.env`
/// maps to `dotenv`. Returns `None` when nothing matches (`-`, stdin and
/// unknown extensions). Reserved future extensions (hcl/properties/kdl/ron)
/// return `None` for now so the CLI reports "cannot detect" with a
/// `-f/--from` hint instead of a misleading guess.
#[must_use]
pub fn detect_format(path: &str) -> Option<&'static str> {
    let file = path.rsplit('/').next().unwrap_or(path);
    if file.eq_ignore_ascii_case(".env") {
        return Some("dotenv");
    }
    let ext = file.rsplit('.').next()?;
    if !file.contains('.') || ext.is_empty() {
        return None;
    }
    match ext.to_ascii_lowercase().as_str() {
        "json" => Some("json"),
        "jsonc" => Some("jsonc"),
        "json5" => Some("json5"),
        "toml" => Some("toml"),
        "yaml" | "yml" => Some("yaml"),
        "env" => Some("dotenv"),
        "ini" | "cfg" | "conf" => Some("ini"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_known_extensions() {
        assert_eq!(detect_format("cfg.json"), Some("json"));
        assert_eq!(detect_format("dir/CFG.JSON"), Some("json"));
        assert_eq!(detect_format("a.jsonc"), Some("jsonc"));
        assert_eq!(detect_format("a.json5"), Some("json5"));
        assert_eq!(detect_format("Cargo.toml"), Some("toml"));
        assert_eq!(detect_format("app.env"), Some("dotenv"));
        assert_eq!(detect_format(".env"), Some("dotenv"));
        assert_eq!(detect_format("setup.ini"), Some("ini"));
    }

    #[test]
    fn unknown_or_missing_extension_is_none() {
        assert_eq!(detect_format("noext"), None);
        assert_eq!(detect_format("cfg.xml"), None);
        assert_eq!(detect_format("config.yaml"), Some("yaml"));
        assert_eq!(detect_format("-"), None);
    }
}
