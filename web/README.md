# Web demo (`web/`)

Single static page (two panes, format selects, warnings, copy-as-link with
URL-hash state). All conversion runs in-browser via the `cfgprism-wasm`
build — no backend.

## Develop locally

```sh
# needs the rustup toolchain (wasm-pack ignores Homebrew rust):
export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
wasm-pack build crates/cfgprism-wasm --target web --out-dir ../../web/pkg --out-name cfgprism
python3 -m http.server --directory web 8000
# open http://localhost:8000
```

`web/pkg/` is generated (git-ignored). CI rebuilds it on every `main` push
and deploys to GitHub Pages (`.github/workflows/pages.yml`), including a
node smoke test of the bundle.

## Notes and limits

- wasm32 has no threads: YAML/KDL event collection runs inline there (see
  D16). Demo inputs are small; pasting megabytes will be slow — same as CLI.
- State encoding: `#from=&to=&src=` with base64url UTF-8 (no compression;
  huge inputs make huge URLs — by design, share small repros).
