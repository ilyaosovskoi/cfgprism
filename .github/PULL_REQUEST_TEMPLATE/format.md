# New format PR template

Use this template for `Format request` follow-ups (`?template=format.md`).

## Format
<!-- Name, file extensions, spec/grammar link. -->

## Mapping to the IR
<!-- How do its values map (tables? anchors? dates? null? duplicate keys?).
What becomes `args`/`children`-style sentinels, if any? -->

## Verbatim or canonical?
<!-- Byte-identical round-trip (which slices) or canonical output (why not
verbatim)? If canonical: what normalizes (layout? quoting? comment
placement?) and why is it acceptable? -->

## Loss table (also add to the README matrix)
<!-- e.g. comments: preserved | dropped+warning | error; anchors: …;
nesting limits; unrepresentable constructs with example inputs. -->

## Tests
<!-- Fixture files added under tests/fixtures/<name>/: -->
<!-- - [ ] unit tests (round-trip, error positions) -->
<!-- - [ ] golden fixtures -->
<!-- - [ ] adversarial cases in tests/adversarial.rs -->

## Left out (file `good first issue`s for these)
<!-- e.g. flow syntax, v1 fallback, datetime support, byte-verbatim. -->
