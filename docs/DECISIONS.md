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

## D11. IR verbatim slices + entry-trivia convention — 2026-09-30

- Context: Stage 2 needs byte-identical self round-trip through the IR
  (not around it — retaining the source text would make goldens vacuous).
- Decision: `Trivia.prefix_raw/suffix_raw`, `Key.raw`, `Entry.sep_raw`,
  `Node.open_raw/close_raw`, scalar `Style::Original`, `Value::Datetime`.
  Same-format emitters concatenate stored slices; cross-format emitters
  ignore every `*_raw` field. Convention: pre-entry comments live on
  `key_trivia.leading`, inline comments on `value.trivia.inline` (array
  items use their own `trivia`); documented on `Entry`.
- Rationale: one uniform mechanism for JSON gaps, TOML decor, and
  line-based formats; logical comments stay reachable for conversion.
- Alternatives: source retention + spans (rejected — goldens would prove
  nothing), per-format hidden CSTs (rejected — splits the architecture).

## D12. JSON gap-partition scheme + MAX_DEPTH 64 — 2026-09-30

- Context: byte round-trip needs every gap byte owned by exactly one slice.
- Decision: commas/colons attach to the following sibling's prefix or the
  preceding value's suffix (split at the first newline); trailing commas
  fold into `close_raw`. Nesting cap 64 (not serde_json's 128): debug
  builds cost two ~8 KiB frames per bracket level, so 128 overflows 2 MiB
  test-thread stacks before the guard trips — found by bisecting, fixed by
  measuring. Fuzz-safety holds on all stacks.
- Alternatives: deeper cap with iterative parsing (rejected — 64 levels is
  plenty for human configs and the error is clean).

## D13. TOML adapter storage model (probed, not assumed) — 2026-09-30

- Context: `toml_edit` decor placement is undocumented; guessing caused
  five distinct round-trip bugs.
- Decision: probed the model (throwaway `tests/probe.rs`, deleted after):
  value suffixes are same-line-only; key/table prefixes exclude the first
  structural newline (emitter adds it back — universal `emit_gap` rule);
  head gaps live on first-key/leaf prefixes; dotted keys flatten to
  single entries rebuilt from `dotted_decor` spacing; implicit
  header-parents are transparent; file tail (trailing comments/blanks,
  invisible to the public API) is recovered by a backward source scan.
  Known limitations: exotic header bracket spacing beyond leaf gaps is
  canonicalized; parent-after-child table order normalizes (both match
  upstream `toml_edit` behavior); CRLF is rejected by the parser.
- Rationale: every rule is probe-verified; fixtures cover each shape.
- Alternatives: emitting via `toml_edit` itself (rejected — bypasses the
  IR and teaches nothing about conversion).

## D14. YAML: saphyr spanned events + own gap layer + own 1.2 resolver — 2026-09-30

- Context: D2 named `saphyr-parser` as the event source but left open how
  far it reaches (comments? anchor names? verbatim slices?).
- Decision: probed `saphyr-parser 0.0.12` (throwaway tests, deleted after)
  and built on what's solid — `SpannedEventReceiver` with byte-index
  markers, exact scalar/alias/bracket spans, numeric anchor ids, core tags:
  - Events own *structure* (out-of-subset input becomes a positioned
    error, never a misparse); an own gap layer owns *trivia + verbatim
    slices* (prefix/key/sep/value/suffix partition, pure concatenation on
    emit, zero auto-whitespace); an own resolver implements YAML 1.2 core
    (Norway `no`/`yes`/`on`/`off`, `1_000`, sexagesimal `12:34` and all
    timestamps are strings; `0o17`/`0xFF` are ints; quoted scalars are
    always strings; `!!` core tags honored).
  - Anchor names: alias spans include `*name` verbatim; definitions found
    by same-line backward scan with separator-gap fallback. Aliases clone
    the anchored subtree at build time (recursive anchors fail cleanly as
    "unknown anchor" instead of hanging).
  - Flow collections included (exact bracket spans made them ~40 lines,
    a bonus beyond the brief's subset): commas live in next-prefixes,
    same-line close-gap comments attach to the last child.
  - Explicit `---`/`...` preserved verbatim; second document → positioned
    error (never silent loss). Non-core `!` tags, complex `?` keys and
    anchors on keys → positioned errors. Duplicate keys are both kept
    verbatim. `<<` merge keys stay literal entries (alias value cloned);
    merge expansion happens at cross-conversion (Stage 4), not at build.
  - Empty/comment-only documents → Null root with the whole source as tail.
  - Canonical fallback emitter for programmatic docs: conservative quoter
    (multiline always double-quoted with escapes — literal chomping is not
    IR-stable), empty containers inline, anchors expand with warnings.
- Rationale: every structural/spans claim probe-verified; fixtures cover
  basic/anchors, multiline/chomping, flow, Norway matrix, merges, markers.
- Alternatives: own line-based YAML parser (rejected — saphyr turns
  out-of-subset input into precise errors instead of misparses); source
  retention (rejected per D11).

## D15. Cross-conversion rules (logical emitters) — 2026-09-30

- Context: Stage 4 needs every pair to convert or fail precisely, never
  silently drop. Same-format stays verbatim; cross pairs go logical.
- Decision (`formats::convert_text` + `Format::emit_logical`):
  - `<<` merges splice (missing keys only, transitive) and the marker is
    dropped without warning (directive, not data); literal non-alias `<<`
    keys are kept. Expansion recurses, so `json->json` on merge input is
    stable. Anchor nodes expand with `AnchorExpanded` per node.
  - Duplicates: JSON-family/YAML keep all entries (valid there);
    TOML/dotenv/INI collapse last-wins + `UnsupportedConstruct`.
  - Comment markers are normalized per target (`#`↔`//` via
    `restyle_comment`); without it YAML `#` comments produce invalid JSONC
    (caught by the matrix, fixed the same day).
  - TOML/INI reorder values-before-tables/sections + `KeyReordered`;
    dotted-looking keys are quoted (`"a.b"`) so they never reinterpret.
  - Unrepresentable is an error, not a warning: nesting into dotenv,
    deep nesting/arrays into INI, `null` into TOML, invalid key names.
    Stringify (bool/number/datetime) into dotenv/INI is silent by
    convention (untyped targets), like `configparser`.
  - Datetime: `TypeCoerced` into JSON-family/YAML (re-parses as string),
    kept raw into TOML, stringified silently into dotenv/INI.
  - Verbatim fallbacks (programmatic IR) warn `AnchorExpanded` when anchor
    syntax cannot survive (JSON pretty, TOML sliceless, dotenv/INI loops);
    YAML routes programmatic docs to the canonical emitter (already warns).
- Rationale: the 7×7 matrix (190+ pairs incl. 6 sample docs) asserts
  `values_equal` (unordered where reordering is legal, stringified for
  untyped ends) plus required-warning sets and clean-pair silence.
- Alternatives: warning on every canonical normalization (rejected —
  canonical output is expected on conversion; warnings mean real loss).

## D16. Performance, fuzzing, hostile inputs — 2026-09-30

- Context: quality bars demand 5 MB < 1 s and fuzz coverage; adversarial
  tests found two real defects.
- Decision:
  - O(n²) span positions: per-node `offset_to_line_col` rescans hung 5 MB
    YAML forever. Fixed with `core::LineIndex` (one pass + binary search).
    Measured after the fix (release): 5 MB json→json 269 ms, yaml→yaml
    0.8 s, toml→toml 0.4 s — all byte-identical, all < 1 s.
  - `saphyr-parser` recurses per level and aborts past ~5k depth on small
    stacks (bisected: saphyr alone, not our builder). Event collection now
    runs on a scoped 256 MiB-stack worker thread (virtual, ~free); our own
    builder guard (64) still bounds IR. 20k-deep input errors cleanly in
    0.03 s. wasm32 runs inline (no threads; demo inputs are tiny).
  - Fuzzing: `fuzz/` cargo-fuzz targets (json/toml/yaml, assert re-parse
    stability) + nightly CI job (weekly schedule + parser changes). No
    nightly locally, so targets are CI-gated; deterministic
    `tests/adversarial.rs` (deep/truncated/hostile inputs, no-panic) runs
    on stable in the main suite.
  - JSON nesting cap 64 (D12) holds: 100 valid levels convert, 200 error
    cleanly on 2 MiB test stacks.
- Rationale: measured, not assumed — every number above reproduces via
  `cargo test --release -p cfgprism-formats` and the criterion benches.
- Alternatives: indent pre-scan for YAML depth (rejected — block-scalar
  false positives, duplicates lexing); iterative saphyr use (impossible —
  their recursion).

## D17. Stage-5 format scopes: own HCL parser, KDL/RON canonical — 2026-09-30

- Context: D6 named `hcl-edit`/`hcl-rs`; KDL/RON fidelity was open.
- Decision:
  - HCL: own simplified parser (supersedes D6). Rationale: the brief
    scopes HCL as "simplified" (attributes, blocks, JSON-ish values);
    mapping `hcl-edit` decor onto IR verbatim slices costs as much as a
    purpose-built parser with none of the round-trip control. Blocks nest
    by label (`resource "x" {…}` → `resource` → `x`), headers stay verbatim
    in `open_raw`, objects use `Flow` style so verbatim/logical emission
    never confuses them. Expressions, variable refs, calls, heredocs,
    `(…)` keys and type-level features are positioned errors, never
    misparses; `${…}` passes through literally (documented).
  - KDL: `kdl` 6.5.0 pinned (`=6.5.0`: 6.6+ needs rustc 1.95, we run 1.94;
    revisit on toolchain bump) with `v1-fallback`. Canonical — not byte —
    round-trip: values, comments (leading trivia + same-line remainders via
    spans; `/-` travels as comments) and order survive; layout normalizes.
    Node mapping: bare→null, single-arg→scalar, else
    `{args, props…, children}` with explicit `args`/`children` collision
    errors and `(type)` rejection. Deep-input aborts inside the crate, so
    parsing runs on a 256 MiB worker thread (same cure as YAML/D16; the
    document is owned, so plain spawn works).
  - RON: `ron` (+ `indexmap` feature: default maps sort keys!) for values,
    own comment pre-lexer hoisted to the head (no spans exist). Canonical
    round-trip; struct form for ident-safe keys, map form otherwise;
    `Some` unwraps, struct/enum names drop (the `Value` model), chars
    become strings. All documented in the module header.
  - Properties: own line parser (Java rules: `=`/`:`/whitespace
    separators, `\` continuations, `\uXXXX`); byte-verbatim like dotenv.
- Rationale: each format is one module + registry lines + fixtures; core
  untouched (CONTRIBUTING.md documents the 30-minute recipe + PR template).
- Alternatives: `hcl-edit` integration (rejected — decor mapping without
  round-trip control); KDL byte-verbatim via spans (deferred — the `}`
  gap has no dedicated slice; follow-up); `ron2` (rejected per D8).

## D18. Stage 6/7 DX choices — 2026-09-30

- Context: sync checks, releases and the web demo need concrete shapes.
- Decision:
  - `cfgprism check A B…` compares IR with `values_equal` (ordered by
    default, `--unordered` for reordering targets), prints the first
    differing path, exits 1 on mismatch. `--as PATH,FORMAT` overrides
    detection. The composite `action/action.yml` (`uses:
    ilyaosovskoi/cfgprism/action`) installs a release binary (cargo-install
    fallback) and runs it; `.pre-commit-hooks.yaml` exposes the same as a
    `system` hook.
  - Releases via cargo-dist (5 targets, shell/powershell/npm/homebrew
    installers) on `v*` tags; Homebrew/Scoop/npm have `packaging/`
    templates with `FIXME`s until the first release exists. `cargo install`
    from git works today.
  - Demo SVG in README is a static illustration (no recording toolchain in
    this environment), labeled by context, not passed off as a recording.
  - Web demo: hand-written `web/` page (no framework), wasm-pack `web`
    target, `web/pkg/` git-ignored and rebuilt by the Pages workflow
    (which also node smoke-tests the bundle). State = `#from=&to=&src=`
    base64url; wasm32 runs YAML/KDL collection inline (no threads).
- Rationale: everything above is verified — `check` by CLI tests, the
  Action by reading (runs on first use), the WASM bundle by a node smoke
  test locally and in CI, Pages enabled via API.
- Alternatives: committing built `web/pkg` (rejected — generated output,
  CI rebuilds); trunk/bundlers (rejected — zero-dependency page).
