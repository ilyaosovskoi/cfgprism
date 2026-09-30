//! JSONC format: strict JSON plus `//`/`/* */` comments and trailing commas.
//! The lossless engine lives in [`crate::json`]; this module only wires the
//! dialect into [`cfgprism_core::Format`].

use cfgprism_core::{Doc, EmitOutput, Error, Format, Options};

use crate::json::{emit_json, emit_json_logical, parse_json, JsonComments, JSONC_DIALECT};

/// JSON with comments (VS Code `*.jsonc`, `tsconfig`-style).
pub struct JsoncFormat;

impl Format for JsoncFormat {
    fn name(&self) -> &'static str {
        "jsonc"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["jsonc"]
    }

    fn parse(&self, src: &str) -> Result<Doc, Error> {
        parse_json(src, JSONC_DIALECT)
    }

    fn emit(&self, doc: &Doc, opt: &Options) -> Result<EmitOutput, Error> {
        emit_json(doc, opt)
    }

    fn emit_logical(&self, doc: &Doc, opt: &Options) -> Result<EmitOutput, Error> {
        emit_json_logical(doc, opt, JsonComments::Keep, false)
    }
}
