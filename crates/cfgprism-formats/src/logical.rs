//! Cross-format conversion: parse with the source format, emit *logical*
//! (canonical + warnings) with the target format. Same-format conversion
//! stays verbatim (`Format::emit`); every other pair goes logical.
//!
//! Shared rules (see `DECISIONS.md` D15):
//! - `<<` merge entries splice aliased-map content (missing keys only) and
//!   the marker itself is dropped (directive, not data) — no warning, no
//!   semantic loss. A literal (non-alias) `<<` key is kept as data.
//! - Duplicates: JSON-family/YAML keep all entries (valid there); strict
//!   targets (TOML/dotenv/INI) collapse last-wins with an
//!   `UnsupportedConstruct` warning.
//! - Anchors expand inline with an `AnchorExpanded` warning per anchored
//!   node (definitions and aliases alike).

use cfgprism_core::{ConvertOutput, Entry, Error, Options, Warning, WarningKind};

use crate::registry::all_formats;

/// Expand `<<` merges; optionally collapse duplicates last-wins.
/// Returned entries are clones (keys keep their text; trivia travels along).
/// Expansion recurses into nested maps/arrays, so transitive merges
/// (`<<` inside merged content) resolve too. IR trees are finite (aliases
/// are snapshots), so recursion always terminates.
pub fn expand_entries(
    entries: &[Entry],
    path: &str,
    collapse_dupes: bool,
    warnings: &mut Vec<Warning>,
) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    // key text → index in `out` (for override/collapse handling).
    let mut index: Vec<(String, usize)> = Vec::new();
    let find = |index: &[(String, usize)], key: &str| -> Option<usize> {
        index.iter().rev().find(|(k, _)| k == key).map(|(_, i)| *i)
    };
    // Pre-expand nested values first so inner `<<` markers resolve before
    // the outer level splices (transitive merges).
    let mut nested: Vec<Entry> = Vec::with_capacity(entries.len());
    for e in entries {
        let mut e = e.clone();
        e.value = expand_value(
            &e.value,
            &join_path(path, &e.key.text),
            collapse_dupes,
            warnings,
        );
        nested.push(e);
    }
    for e in &nested {
        let is_merge = e.key.text == "<<" && e.value.anchor.as_ref().is_some_and(|a| a.is_alias);
        if is_merge {
            for (k, n) in merge_sources(&e.value) {
                if find(&index, &k).is_none() {
                    // Splice a clone carrying the definition-site trivia.
                    let mut spliced = n.clone();
                    spliced.order = out.len();
                    index.push((k.clone(), out.len()));
                    out.push(Entry {
                        key: crate::key_text(&k),
                        key_trivia: n.trivia.clone(),
                        sep_raw: None,
                        value: spliced_value(spliced),
                    });
                }
            }
            continue;
        }
        if let Some(i) = find(&index, &e.key.text) {
            if collapse_dupes {
                warnings.push(Warning::new(
                    join_path(path, &e.key.text),
                    WarningKind::UnsupportedConstruct,
                    format!("duplicate key '{}' collapsed, last wins", e.key.text),
                ));
                out[i] = e.clone();
            } else {
                index.push((e.key.text.clone(), out.len()));
                out.push(e.clone());
            }
        } else {
            index.push((e.key.text.clone(), out.len()));
            out.push(e.clone());
        }
    }
    out
}

/// Recursively expand a value node (maps/arrays); scalars pass through.
fn expand_value(
    node: &cfgprism_core::Node,
    path: &str,
    collapse_dupes: bool,
    warnings: &mut Vec<Warning>,
) -> cfgprism_core::Node {
    use cfgprism_core::Value;
    match &node.value {
        Value::Map(entries) => {
            let mut n = cfgprism_core::Node::new(Value::Map(expand_entries(
                entries,
                path,
                collapse_dupes,
                warnings,
            )));
            n.anchor = node.anchor.clone();
            n.order = node.order;
            n.style = node.style.clone();
            n.trivia = node.trivia.clone();
            n
        }
        Value::Array(items) => {
            let mut n = cfgprism_core::Node::new(Value::Array(
                items
                    .iter()
                    .map(|i| expand_value(i, path, collapse_dupes, warnings))
                    .collect(),
            ));
            n.anchor = node.anchor.clone();
            n.order = node.order;
            n.style = node.style.clone();
            n.trivia = node.trivia.clone();
            n
        }
        _ => node.clone(),
    }
}

/// Merge source entries: the aliased map itself, or each map in an aliased
/// array (`<<: [*a, *b]`). Non-map sources contribute nothing.
fn merge_sources(node: &cfgprism_core::Node) -> Vec<(String, cfgprism_core::Node)> {
    use cfgprism_core::Value;
    match &node.value {
        Value::Map(entries) => entries
            .iter()
            .map(|e| (e.key.text.clone(), e.value.clone()))
            .collect(),
        Value::Array(items) => {
            let mut out = Vec::new();
            for it in items {
                if let Value::Map(entries) = &it.value {
                    for e in entries {
                        out.push((e.key.text.clone(), e.value.clone()));
                    }
                }
            }
            out
        }
        _ => Vec::new(),
    }
}

/// The spliced value node: anchor metadata cleared (expansion is reported
/// by the emitters, not here) but trivia kept.
fn spliced_value(mut n: cfgprism_core::Node) -> cfgprism_core::Node {
    n.anchor = None;
    n
}

fn join_path(base: &str, key: &str) -> String {
    if base.is_empty() {
        key.to_string()
    } else {
        format!("{base}.{key}")
    }
}

/// Rewrite a stored comment's marker for the target format (`#` or `//`).
/// Stored comments keep their source marker (`#`, `//`, `/* */`, `;`);
/// emitting them verbatim could produce invalid output (`#` is not a JSONC
/// comment). Returns output lines (block comments unfold).
pub fn restyle_comment(comment: &str, marker: &str) -> Vec<String> {
    let t = comment.trim();
    // Single-line block comment: `/* x */` → inner text.
    if t.starts_with("/*") {
        let inner = t
            .strip_prefix("/*")
            .unwrap_or(t)
            .strip_suffix("*/")
            .unwrap_or(t.strip_prefix("/*").unwrap_or(t));
        let mut out = Vec::new();
        for line in inner.split('\n') {
            let line = line
                .trim()
                .strip_prefix('*')
                .map(str::trim)
                .unwrap_or(line.trim());
            if line.is_empty() {
                continue;
            }
            out.push(format!("{marker} {line}"));
        }
        if out.is_empty() {
            out.push(marker.to_string());
        }
        return out;
    }
    // Line comment: swap the marker, keep the exact remainder.
    // (`!` is Java-properties style; `;` is INI style.)
    for m in ["//", "#", ";", "!"] {
        if let Some(rest) = t.strip_prefix(m) {
            if rest.is_empty() {
                return vec![marker.to_string()];
            }
            return vec![format!("{marker}{rest}")];
        }
    }
    // No recognizable marker: prefix one.
    vec![format!("{marker} {t}")]
}

/// Convert `src` from `from` to `to`.
///
/// Same-format pairs use the verbatim emitter (byte round-trip); all other
/// pairs parse into the IR and emit logical canonical output with warnings.
pub fn convert_text(
    from: &str,
    to: &str,
    src: &str,
    opt: &Options,
) -> Result<ConvertOutput, Error> {
    let registry = all_formats();
    let from_fmt = registry
        .find(from)
        .ok_or_else(|| Error::unsupported_format(format!("unsupported source format '{from}'")))?;
    let to_fmt = registry
        .find(to)
        .ok_or_else(|| Error::unsupported_format(format!("unsupported target format '{to}'")))?;
    let doc = from_fmt.parse(src)?;
    if from.eq_ignore_ascii_case(to) {
        let out = to_fmt.emit(&doc, opt)?;
        return Ok(ConvertOutput {
            text: out.text,
            warnings: out.warnings,
        });
    }
    let out = to_fmt.emit_logical(&doc, opt)?;
    Ok(ConvertOutput {
        text: out.text,
        warnings: out.warnings,
    })
}
