//! JSON5 format (ECMAScript 5.1 literal grammar): comments, trailing commas,
//! single-quoted and unquoted keys, hex numbers, leading `+`, `.5` / `5.`,
//! `Infinity` / `NaN`, backslash line continuations.
//! The lossless engine lives in [`crate::json`]; this module only wires the
//! dialect into [`cfgprism_core::Format`].

use cfgprism_core::{Doc, EmitOutput, Error, Format, Options};

use crate::json::{emit_json, parse_json, JSON5_DIALECT};

/// JSON5 (`*.json5`).
pub struct Json5Format;

impl Format for Json5Format {
    fn name(&self) -> &'static str {
        "json5"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["json5"]
    }

    fn parse(&self, src: &str) -> Result<Doc, Error> {
        parse_json(src, JSON5_DIALECT)
    }

    fn emit(&self, doc: &Doc, opt: &Options) -> Result<EmitOutput, Error> {
        emit_json(doc, opt)
    }
}
