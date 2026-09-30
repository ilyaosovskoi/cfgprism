//! Format registry + extension sniffing.
//!
//! Format set is intentionally tiny in stage 1. Stage 2/3/5 extend
//! `all_formats()` — the CLI and detection logic below do not change.

use cfgprism_core::FormatRegistry;

use crate::JsonStubFormat;

/// Build the registry with every supported format (stage-1 set).
#[must_use]
pub fn all_formats() -> FormatRegistry {
    FormatRegistry::new(vec![JsonStubFormat::boxed()])
}

/// Canonical supported names in help order.
#[must_use]
pub fn supported_names() -> Vec<&'static str> {
    all_formats().names()
}

/// Guess the canonical format name from a file path extension.
///
/// Returns `None` when there is no extension or it is unknown.
/// Matching is case-insensitive; `stdin`/`-` never sniff.
#[must_use]
pub fn detect_format(path: &str) -> Option<&'static str> {
    let file = path.rsplit('/').next().unwrap_or(path);
    let ext = file.rsplit('.').next()?;
    if !file.contains('.') || ext.is_empty() {
        return None;
    }
    // Keep in sync with `Format::extensions()` of every registered format.
    // (Registry lookup by extension arrives with stage 2 when >1 format exists.)
    match ext.to_ascii_lowercase().as_str() {
        "json" => Some("json"),
        // Reserved for upcoming stages — detected but reported as unsupported
        // with a precise error instead of "cannot detect". See docs/DESIGN.md.
        "jsonc" | "json5" | "toml" | "env" | "ini" | "yaml" | "yml" | "hcl" | "properties"
        | "kdl" | "ron" => None,
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_json_by_extension() {
        assert_eq!(detect_format("cfg.json"), Some("json"));
        assert_eq!(detect_format("dir/CFG.JSON"), Some("json"));
    }

    #[test]
    fn unknown_or_missing_extension_is_none() {
        assert_eq!(detect_format("noext"), None);
        assert_eq!(detect_format("cfg.xml"), None);
        assert_eq!(detect_format("-"), None);
    }
}
