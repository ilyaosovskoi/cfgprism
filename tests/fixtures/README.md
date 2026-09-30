# Golden fixtures

Layout: `tests/fixtures/<format>/*.in` (input) + `*.out` (expected output).

- Stage 1: only `json/basic.{in,out}` (JSON stub: value round-trip, order kept).
- Stage 2+: every format gets `roundtrip` fixtures (byte-identical in/out for
  self round-trip) plus `to-<other>` fixtures for cross-conversion together
  with expected warnings (see `docs/DESIGN.md` §2.4).
- The `golden` integration test in `cfgprism-formats` walks this directory;
  adding a fixture pair without code changes is enough to extend coverage.
