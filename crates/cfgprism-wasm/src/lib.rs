//! WASM bindings (stage-1 skeleton; full web demo arrives in stage 7).
//!
//! Exposes `convert_text(from, to, src)` returning a JSON string
//! `{"text": ..., "warnings": [...]}` or throwing a JS error with the
//! `line:col` message. No network calls; pure in-browser conversion.

use cfgprism_core::Options;
use wasm_bindgen::prelude::*;

/// Convert `src` from `from` to `to`. Returns JSON `{"text","warnings"}`.
///
/// # Errors
/// Throws a JS string on unknown format / parse / emit failure.
#[wasm_bindgen]
pub fn convert_text(from: &str, to: &str, src: &str) -> Result<String, JsValue> {
    let out = cfgprism_formats::convert_text(from, to, src, &Options::default())
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    let warnings: Vec<String> = out.warnings.iter().map(ToString::to_string).collect();
    serde_json::to_string(&serde_json::json!({"text": out.text, "warnings": warnings}))
        .map_err(|e| JsValue::from_str(&e.to_string()))
}
