# Contributing to cfgprism

## Add a new format in ~30 minutes (no core changes needed)

Adding a format never touches `cfgprism-core`. You only add one module in
`cfgprism-formats` plus fixtures. The pattern below is complete.

### 1. Create `crates/cfgprism-formats/src/<name>.rs`

```rust
use cfgprism_core::{Doc, EmitOutput, Error, Format, Options};

pub struct MyFormat;

impl Format for MyFormat {
    fn name(&self) -> &'static str { "myfmt" }
    fn extensions(&self) -> &'static [&'static str] { &["mf", "myfmt"] }
    fn parse(&self, src: &str) -> Result<Doc, Error> { /* ... */ }
    fn emit(&self, doc: &Doc, opt: &Options) -> Result<EmitOutput, Error> { /* ... */ }
    fn emit_logical(&self, doc: &Doc, opt: &Options) -> Result<EmitOutput, Error> { /* ... */ }
}
```

### 2. Register it (two lines each)

- `lib.rs`: `pub mod myfmt;` + `pub use myfmt::MyFormat;`
- `registry.rs`: `Box::new(MyFormat)` in `all_formats()` + extension arms in
  `detect_format()` + one line in the `detects_known_extensions` test.

### 3. IR rules (the important part)

- Maps are `Vec<Entry>` — insertion order **is** the source order. Never sort.
- Comments: pre-entry → `key_trivia.leading`, same-line → `value.trivia.inline`.
  Array items use their own `trivia` (no keys exist).
- Scalars keep verbatim spellings (`Style::Original`, `Number.raw`).
- `emit` (same format): reproduce bytes from `*_raw` slices, or go canonical
  and document it (like KDL/RON do — see their module docs).
- `emit_logical` (cross pairs): canonical output, `expand_entries` for `<<`
  merges, `restyle_comment` for markers, warnings for every loss:
  `CommentDropped`, `AnchorExpanded`, `TypeCoerced`, `KeyReordered`,
  `UnsupportedConstruct`. Unrepresentable input is an `Error`, never silent.
- Every parse error carries `line:col` (`offset_to_line_col` or equivalent).
- No `unwrap()`/`expect()` in library code (CI enforces it); no panics on
  hostile input — add your format to `tests/adversarial.rs`.

### 4. Fixtures and tests

- `tests/fixtures/<name>/basic.in` (+ `.out`; identical for byte-verbatim
  formats, canonical for normalizing ones like KDL/RON). The `golden` test
  picks them up automatically — no harness changes.
- Unit tests in the module: round-trip, error positions, one hostile case.
- Run before opening the PR: `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`.

### 5. Open the PR with the format template

Use the *Format* PR template (`.github/PULL_REQUEST_TEMPLATE/format.md`):
what the format is, verbatim-vs-canonical choice, warning table additions
for the README matrix, and the `good first issue` follow-ups you left out.

## Other contributions

- Bug reports: include input, expected and actual output (see issue templates).
- Decisions with trade-offs go to `docs/DECISIONS.md` (append-only, D-numbered).
- Keep it English everywhere (docs, code, comments, messages).

## Releasing (maintainers)

1. Bump `version` in the root `Cargo.toml`, update `CHANGELOG.md` (create it
   on first release), commit.
2. Push a tag: `git tag vX.Y.Z && git push origin vX.Y.Z` — the `release`
   workflow (cargo-dist, 5 targets) builds, uploads artifacts and drafts
   the GitHub release.
3. Fill `packaging/homebrew/cfgprism.rb` (`FIXME` version/sha256) and copy it
   to the tap repo; update `packaging/scoop/cfgprism.json` (`FIXME` hash);
   `cargo-dist` publishes the npm wrapper automatically (`NODE_AUTH_TOKEN`
   secret required).
4. Verify: `cargo install cfgprism@X.Y.Z`, `brew install`, `npx cfgprism`.
