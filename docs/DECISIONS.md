# docs/DECISIONS.md — decision log (append-only)

Entry format: `## Dn. <title> — <date>` + Context / Decision / Rationale /
Alternatives. Ambiguities in the brief are resolved here, not in chat.

## D1. TOML — `toml_edit`, not `taplo` — 2026-09-30

- Context: need trivia (comments/whitespace/order) + byte round-trip + line:col.
- Decision: `toml_edit` (toml-rs, spec-1.1.0).
- Rationale: `DocumentMut::to_string()` round-trips out of the box;
  `decor`/`repr` map directly onto IR Trivia/Style; light dependencies
  (matters for the static binary and WASM).
- Alternatives: `taplo` (rowan green-tree, full fidelity + formatter/LSP, but
  a heavy dependency tree ~160K SLoC transitively) — rejected as overhead.
  Known `toml_edit` losses (dotted-key order, scattered-table reorder) are
  covered by warnings and tests instead of being hushed up.

## D2. YAML — `saphyr-parser` + own trivia collector — 2026-09-30

- Context: the brief demands YAML round-trip without losses + anchors/aliases
  + comments.
- Decision: event source `saphyr-parser` (YAML 1.2 compliant, active;
  `yaml-rust2` is maintenance-only) + our own trivia scanner
  (comments/blank lines/styles/quotes/block/flow) attached by line.
- Rationale: no ready trivia-preserving YAML crate exists in Rust (checked
  `yaml-rust2`, `saphyr`, `serde-saphyr`/`granit-parser`: they either drop
  comments or catch only `Commented<T>`, never freestanding ones). Writing a
  YAML parser from scratch costs more than a collector over compliant events.
- Alternatives: `yaml-rust2` (same flaw + worse compliance),
  `unsafe-libyaml` (C dependency, breaks the static build and WASM) —
  rejected.

## D3. JSON (strict) — own hand-rolled parser — 2026-09-30

- Context: need JSON-to-itself byte round-trip + spans + order preservation.
- Decision: our own recursive-descent (API modeled on `jsonc-parser` with
  `comments`/`tokens`), finalized by a benchmark in Stage 2.
- Rationale: `serde_json` drops comments/whitespace (order only with
  `preserve_order`); `jsonc-parser` is closest but we want round-trip control
  without CST-API surprises.
- Alternatives: using `jsonc-parser` directly — kept as fallback if ours
  loses on fuzz/bench.

## D4. JSONC/JSON5 — own JSON-extension parser — 2026-09-30

- Context: JSONC/JSON5 are JSON supersets (comments, trailing commas,
  unquoted keys, single quotes, hex, multiline).
- Decision: one parser with a dialect flag; trivia from D3 + extensions.
- Rationale: no trivia-preserving JSON5 crate exists in Rust
  (`json5-rs`/`serde_json5` are serde-only and drop trivia; `json5format` is
  a formatter only). The grammar is small — writing it is cheapest.
- Alternatives: forking `json5format` — rejected (not an IR parser).

## D5. `.env` / INI / Properties — own line parsers — 2026-09-30

- Context: the brief demands byte round-trip for simple line formats.
- Decision: three small hand-written parsers (~200 lines each).
- Rationale: `dotenv`/`dotenvy` are loaders (lose comments/quotes/`export`);
  `dotenv-parser` abandoned 5+ years; `rust-ini`/`configparser` do not
  guarantee round-trip. Trivial grammars — cheaper to write than to fix.

## D6. HCL — `hcl-edit`/`hcl-rs`, "simplified HCL" scope — 2026-09-30

- Context: Stage 5 demands HCL; full HCL includes expressions/templates/eval.
- Decision: parsing/round-trip via `hcl-edit` ("toml_edit for HCL"); values
  via `hcl-rs`; scope — attributes/blocks/literals; `for`/functions/`${}`
  map into the IR as opaque scalars + `WarningKind::UnsupportedConstruct`.
- Rationale: the only trivia-preserving HCL in the ecosystem; full eval is a
  separate project, out of converter scope.
- Alternatives: writing our own HCL parser — rejected (expensive, worse
  compliance with go-hcl).

## D7. KDL — `kdl` (kdl-rs v2) — 2026-09-30

- Context: need trivia-KDL.
- Decision: the `kdl` crate (document-oriented, keeps formatting/comments,
  byte round-trip, v1/v2).
- Rationale: literally `toml_edit` for KDL; `knus` rejected — serde-derive
  based, drops trivia.

## D8. RON — `ron` + own comment lexer; round-trip partial only — 2026-09-30

- Context: RON is required in Stage 5, but trivia-RON does not exist in Rust.
- Decision: values via `ron`, comments via our own pre-lexer attached to the
  IR; byte round-trip for RON is NOT promised, the README matrix will show
  `round-trip: partial` + a normalization warning.
- Rationale: `ron2` (AST-based, 2026) is immature (single-digit stars,
  unstable API). An honest limitation beats a faked round-trip.
- Alternatives: waiting/migrating to `ron2` once stable — recorded as
  follow-up.

## D9. MSRV 1.82 — 2026-09-30

- Context: clippy's `incompatible_msrv` lint flags `Option::is_none_or`
  (used in the CLI stdin detection) against the workspace `rust-version =
  "1.75"`.
- Decision: raise workspace MSRV to 1.82 instead of rewriting the code.
- Rationale: `is_none_or`/`is_some_and` read better than manual matches;
  1.82 is widely available (CI images, distros, rustup) and still
  conservative; the lint stays green with `-D warnings`.
- Alternatives: avoid `is_none_or` to keep 1.75 — rejected, not worth the
  readability cost for a 2026 project.

## D10. `serde_json/preserve_order` in the stage-1 JSON stub — 2026-09-30

- Context: the CLI test `convert_json_file_to_json_stdout` failed: `{"b":
  1, "a": 2}` came out as `{"a": 2, "b": 1}`. Root cause: `serde_json::Map`
  is a BTreeMap by default and silently sorts keys.
- Decision: enable `preserve_order` (IndexMap backend) for `serde_json` in
  `cfgprism-formats` (and `cfgprism-wasm` for unification).
- Rationale: silent key re-sorting is exactly the loss class cfgprism exists
  to fight; the stub must already preserve order even before Stage 2 brings
  real trivia. Caught by test, fixed by feature flag, recorded here.
