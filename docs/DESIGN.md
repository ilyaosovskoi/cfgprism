# cfgprism — DESIGN (Stage 0. Reconnaissance)

> Status: Stage 0 done, no code. This document is the single source of truth
> for parser choices and architecture. Any conflict with the task brief is
> resolved via `docs/DECISIONS.md`.

## 0. Project goal (reminder)

A config converter between formats that **preserves comments, key order and
formatting** wherever the target format can express them, and **warns
explicitly** (`Warning { path, kind, message }` → stderr, `--strict` turns
warnings into errors) wherever loss is unavoidable. The opposite of `yq` /
`dasel`, which silently normalize or drop trivia.

## 1. What was studied

### 1.1. yq (mikefarah/yq, Go)

- Parser: `gopkg.in/yaml.v3` (with `goccy/go-yaml` experiments). `yaml.v3`
  stores `HeadComment/LineComment/FootComment`, styles, anchors/tags — which
  is why `yq eval -i` keeps comments on **in-place updates**.
- Weaknesses for our task:
  1. Conversion (`-o=json`) **silently drops** comments/styles/anchors.
     No warnings, no `--strict`.
  2. `yq` is a query processor (jq syntax), not a converter with an IR.
     Its "decode into Node → re-encode" model normalizes whitespace,
     ordering under `sort_keys`, etc.
  3. Known whitespace/comment bugs (issues #718, CRLF #1871) bottom out in
     `go-yaml` limitations.
- Takeaway: this confirms the cfgprism niche — not competing on jq syntax,
  but on **honest cross-conversion with warnings**. From yq we borrow UX
  ideas only (`-o`, `eval` concepts are out of scope; we need `convert`).

### 1.2. dasel (TomWright/dasel v3, Go)

- Architecture: every format parses into a **generic model**
  (`model.Value`); trivia **is discarded on input**. The man page states it
  directly: "Comments in YAML and TOML files are discarded when writing due
  to parser limitations".
- Strengths: wide format matrix (JSON/YAML/TOML/XML/CSV/HCL/INI/KDL), one
  selector language, handy CLI reference (`-i`/`-o`, stdin/stdout).
- Takeaway: dasel is the anti-example for our IR. Our IR must store trivia
  from day one or we repeat its losses. We copy its format matrix and
  `-i`/`-o` flags as a UX landmark.

### 1.3. TOML: `toml_edit` vs `taplo`

| Criterion | `toml_edit` (toml-rs) | `taplo` (tamasfe) |
|---|---|---|
| Trivia | Yes: comments, whitespace, relative order; `DocumentMut::to_string()` gives byte round-trip | Yes: full rowan green-tree, every character preserved |
| Error positions | Yes: `TomlError::span()`, line:col computable | Yes: offsets + lengths |
| Dependency weight | Light, no rowan/logos | Heavy: `rowan, logos, itertools, tracing…`, ~160K SLoC transitively |
| Spec | Current, spec-1.1.0, MSRV 1.85, 300M+ downloads | Current, but LSP/formatter/schema-oriented |
| Converter API fit | `DocumentMut/Item/Table/Value + decor/repr` maps directly onto IR Trivia/Style | DOM + syntax tree — powerful but overkill |

**Decision: `toml_edit`.** Reasons: byte round-trip out of the box, minimal
dependency tree (matters for "one static binary" and WASM), and an API whose
`decor`/`repr` map 1:1 onto our `Trivia`+`Style`. `taplo` rejected: a rowan
tree is overhead for a converter; reconsider only if we ever need LSP /
formatting. Known `toml_edit` losses (dotted-key order, reordered scattered
`[tables]` — see the crate README) are recorded as known losses and covered
by warnings/tests.

### 1.4. YAML: `yaml-rust2` / `saphyr` / `serde-saphyr` / `granit-parser`

Verified via docs.rs and sources:

- `yaml-rust2` (a `yaml-rust` fork, libyaml heritage): parses into
  `Yaml::Hash` / `Array`, **losing comments, scalar styles, anchors (except
  alias resolution) and original quoting**. `YamlEmitter` normalizes output.
  Low-level `Event`s exist but carry no spans or trivia. Status: **basic
  maintenance only**; new features go to `saphyr`.
- `saphyr` + `saphyr-parser`: fully YAML 1.2 compliant, faster and more
  correct, but same model — **no comments**. `saphyr-parser` yields an
  `Event` stream (styles, anchors/aliases, tags included) but no comments
  and no exact spans for round-trip.
- `serde-saphyr` (on `granit-parser`, a saphyr fork): the only one that
  raised comments — the `Commented<T>` wrapper captures/emits inline
  comments, plus `Budget`/spans. But: **freestanding comments are not
  captured** ("use granit-parser directly"), flow contexts suppress
  comments, attachment semantics are fragile.
- Takeaway: **there is no `toml_edit`-grade trivia-preserving YAML crate in
  Rust** (no `hcl-edit`/`kdl` equivalent for YAML exists).

**Decision: our own YAML layer on top of `saphyr-parser`.**
`saphyr-parser` is the event source (mappings/sequences/scalars, styles,
anchors/aliases, tags, multi-doc boundaries) as a compliant 1.2 parser. On
top of it, our own trivia collector: a source pre-scan gathers `#`
comments, blank lines, indentation, block styles (`|`/`>`), flow styles
(`{}`/`[]`) and quoting, attaching them to IR nodes by line. The "just take
`yaml-rust2`" alternative is rejected: same trivia loss with worse
compliance and less activity. `unsafe-libyaml` is rejected: a C dependency
breaks the static binary and WASM. This is the riskiest part of the project,
so Stage 3 is dedicated to YAML plus a dedicated Norway-test set.

### 1.5. JSON / JSONC / JSON5

- `serde_json`: fast but **comment-free**; order only with the
  `preserve_order` feature (IndexMap). Unfit for round-trip as-is.
- `jsonc-parser` (dprint): parses JSONC into `JsonValue` **and** into a
  CST/AST with `comments: true, tokens: true`, including spans. Closest to
  what we need.
- `json5-rs` (callum-oakley), `serde_json5` (google fork), `json-five-rs`:
  all **serde-oriented, trivia-dropping**. `json5format` (google): formats
  JSON5 **while keeping contextual line/block comments** — useful as an
  emitter reference, not as an IR parser.
- **Decision:**
  - JSON (strict): our own thin parser inspired by the `jsonc-parser` API,
    or `jsonc-parser` itself with tokens/comments enabled; finalized by a
    benchmark in Stage 2. Default: **our own hand-rolled recursive-descent**
    to guarantee byte round-trip and spans without CST-API surprises.
  - JSONC/JSON5: our own parser (JSON extension: `//`, `/* */`, trailing
    commas, unquoted keys, single quotes, hex, multiline strings, `+` sign,
    `.5`). Reason: no trivia-preserving JSON5 crate exists in Rust; the
    grammar is small, writing it is cheaper than forking.

### 1.6. `.env` / INI / Properties

- `.env`: `dotenv`/`dotenvy` are loaders into `env` (lose
  comments/order/quoting/`export`); `dotenv-parser` is abandoned (5+ years,
  ISC, BTreeMap only). The `.env` grammar is trivial (lines of `KEY=VAL`,
  `export`, `#`, quotes, continuations).
  **Decision: our own line-based parser** (~200 lines), trivia included.
- INI: `rust-ini`, `configparser` drop trivia/section order and disagree on
  duplicates; none guarantees round-trip.
  **Decision: our own parser** (sections, `key=value|key:value`, `;`/`#`
  comments, continuations). Cheaper than fixing someone else's.
- Properties (Java): no mature trivia crate exists.
  **Decision: our own parser** (same class as INI/`.env`).

### 1.7. Stage 5 (HCL / KDL / RON)

- **HCL: `hcl-edit` (+ `hcl-rs`).** `hcl-edit` is literally "to HCL what
  `toml_edit` is to TOML": keeps whitespace/comments, API inspired by
  `toml_edit`. `hcl-rs` on top adds serde + expression/template eval.
  Limitation: native-syntax expressions (`for`, functions, `${}`) map into
  the IR as opaque scalars + `ExprOpaque` warning. Stage 5 scope is
  "simplified HCL" (attributes/blocks/literals); full eval is out of scope.
- **KDL: `kdl` (kdl-rs, v2).** Document-oriented, "`toml_edit` for KDL":
  keeps formatting/whitespace/comments, byte round-trip out of the box,
  v1/v2 plus conversion between them. `knus` rejected: serde-derive based,
  drops trivia. **Decision: `kdl`.**
- **RON: `ron` (ron-rs).** Handles comments/trailing commas/enums but is
  **not trivia-preserving** (no spans/round-trip API); `ron2` (AST-based,
  2026) is immature (single-digit stars, unstable API). **Decision: `ron`
  for values + our own comment pre-lexer attached to the IR; byte round-trip
  for RON is NOT guaranteed** (recorded as `round-trip: partial` in the
  support matrix). Migrate to `ron2` if it stabilizes.

## 2. Architecture

### 2.1. Crates

```text
cfgprism-core    — IR (Node/Doc/Trivia/Style/Span), Format trait, Warning, convert()
cfgprism-formats — one module per format: json.rs, jsonc.rs/json5.rs, toml.rs,
                   dotenv.rs, ini.rs, (stage 3) yaml.rs,
                   (stage 5) hcl.rs, properties.rs, kdl.rs, ron.rs
cfgprism-cli     — the `cfgprism convert <in> [-f FROM] -t TO [-o OUT]` binary
cfgprism-wasm    — wasm-bindgen wrapper for the web demo (stage 7)
```

Dependency rules: `formats → core`, `cli → core+formats`,
`wasm → core+formats`. The core **knows nothing** about concrete formats —
otherwise external contributors could not add formats "without touching the
core" (a Stage 5 requirement).

### 2.2. IR

```rust
struct Doc  { root: Node, trailing: Trivia, … }
struct Node {
  key: Option<Key>,        // for map entries; keeps the original key repr
  value: Value,            // Null|Bool|Num|Str|Array|Map
  order: usize,            // source order (IndexMap invariant)
  trivia: Trivia,          // leading comments, inline comment, blanks_before, trailing
  style: Style,            // Quoted{single|double}|Plain|Block{literal|folded}|Flow{…}|Original(repr)
  span: Option<Span>,      // line:col start/end in the source
  anchor: Option<Anchor>,  // YAML anchors/aliases (None outside YAML)
}
struct Warning { path: String /* JSON-pointer-ish */, kind: WarningKind, message: String }
enum WarningKind { CommentDropped | AnchorExpanded | StyleNormalized | TypeCoerced
                 | KeyReordered | LossyNumber | UnsupportedConstruct | … }
trait Format {
  fn name(&self) -> &'static str;
  fn parse(&self, src: &str) -> Result<Doc, Error>;   // Error always carries line:col
  fn emit(&self, doc: &Doc, opt: &Options) -> Result<String, Error>;
}
fn convert(doc: &Doc, from: &dyn Format, to: &dyn Format) -> (String, Vec<Warning>)
```

- Order: `Map` always has `IndexMap` semantics; there is no sorting (except
  an explicit emitter option that must produce a `KeyReordered` warning).
- Numbers/dates: store **both** the typed value **and** the original repr
  (`Original`), so `01` vs `1`, `0o17`, `no` vs `"no"` and dates are never
  silently rewritten. Ambiguities produce a `TypeCoerced` warning.
- `trivia` is a first-class IR citizen, not second-class: any format that
  cannot express the receiver's trivia must return a warning, never drop it
  silently. Strict JSON is the canonical example: comments become
  `CommentDropped` unless the target is JSONC/5.

### 2.3. Conversion and warnings

Stage 4 matrix: golden tests for every `(from, to)` pair plus a "what is
lost" table. The warning taxonomy is fixed in the core (`WarningKind` is
`non_exhaustive` so formats can extend it, but the base kinds are stable for
`--strict` and the web demo). `--strict` = any warning → `exit != 0` + text
on stderr.

### 2.4. Testing (groundwork for Stages 2–4)

- `tests/fixtures/<format>/*.in` + `*.out` — golden files; self round-trip
  "byte-identical" for Stage 2 (except documented `toml_edit` exceptions:
  dotted keys / reordered scattered tables).
- Stage 3: Norway problem (`no`/`on`/`off`/`yes` as strings vs bools),
  `0o` numbers, dates/timestamps, sexagesimal, merge keys `<<`, multi-doc
  (`---`/`...`) — one fixture + warning assertion per case.
- Property: `parse → emit → parse ≡ IR` (span-free equivalence).
- Fuzz (`cargo-fuzz`): JSON/TOML/YAML parsers — only our own code plus
  adapters; `toml_edit`/`saphyr-parser` are fuzzed through our wrappers
  (catching panics in trivia attachment).
- Bench: 5 MB < 1 s. Groundwork: zero-copy where cheap, but never at the
  cost of trivia correctness; measured in Stage 2.

## 3. Decisions (short form; details in DECISIONS.md D1–D10)

| # | Decision | Rationale (one line) |
|---|---|---|
| D1 | TOML — `toml_edit` | byte round-trip + decor/repr + light deps; `taplo` is overhead |
| D2 | YAML — `saphyr-parser` + own trivia collector | no ready trivia-YAML in Rust; need a compliant event source |
| D3 | JSON — own hand-rolled parser (`jsonc-parser` API as reference) | need byte round-trip + spans; serde_json does not provide them |
| D4 | JSONC/JSON5 — own JSON-extension parser | trivia-preserving JSON5 does not exist in Rust |
| D5 | `.env`/INI/Properties — own line parsers | existing ones drop trivia or are abandoned; grammars are trivial |
| D6 | HCL — `hcl-edit`/`hcl-rs`, "simplified" scope | the only trivia-HCL; expressions → opaque + warning |
| D7 | KDL — `kdl` (kdl-rs) | the only trivia-KDL ("toml_edit for KDL") |
| D8 | RON — `ron` + own comment lexer; round-trip partial | `ron2` is immature; honest limitation beats faked round-trip |
| D9 | MSRV 1.82 | `Option::is_none_or` needs it; modern enough for dists/CI |
| D10 | `serde_json/preserve_order` in the stage-1 stub | default BTreeMap backend silently sorts keys — exactly the loss we fight |

## 4. Risks and what Stage 0 could not resolve

1. **The YAML trivia collector is the main risk.** Exact comment attachment
   (head/line/foot), indent edge cases, flow vs block, multi-doc headers —
   will take iterations in Stage 3. Mitigation: subset-first
   (mappings/lists/scalars/comments/multilines/anchors); everything else is
   an explicit `UnsupportedConstruct` + warning, never silence.
2. **WASM (Stage 7):** `toml_edit`, `saphyr-parser`, `kdl`, `hcl-edit` are
   pure Rust and should build for `wasm32-unknown-unknown`; `ron` and our
   own code too. Low risk, but verify in Stage 1 via
   `cargo check --target wasm32-unknown-unknown` for core.
3. **Licenses:** the project itself is MIT. All chosen parser crates are MIT
   or MIT/Apache-2.0 dual-licensed, except `kdl` (Apache-2.0) — depending on
   it from MIT code is fine; binary distributions must keep its NOTICE.
4. **5 MB < 1 s:** not measured in Stage 0 (no code). All chosen parsers are
   linear/predictive; our own JSON/YAML will be single-pass. Verified by a
   bench in Stage 2.

## 5. Stage 1 groundwork (code starts with the next stage)

Workspace + CI (fmt/clippy/test × linux/mac/windows) + IR + `trait Format`
+ `Warning` + `convert <in> [-f FROM] -t TO [-o OUT] [--strict]` CLI (+
extension sniffing, stdin), golden skeleton `tests/fixtures/*`,
`cargo-fuzz`/`criterion` stubs, `wasm` check. The first format (JSON stub)
exists only to drive the pipeline, with no round-trip claims (that is
Stage 2).
