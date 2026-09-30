# Default PR template

## What
<!-- One paragraph: what changes and why. -->

## How verified
<!-- Commands run: fmt, clippy, tests. Paste new fixture names if any. -->

## Checklist
- [ ] `cargo fmt --all -- --check` clean
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo test --workspace` green
- [ ] No Russian text anywhere (code, docs, messages)
- [ ] `docs/DECISIONS.md` updated if a trade-off was made
