//! Logical IR equivalence: compare values + key order, ignoring trivia,
//! verbatim slices, spans, styles and anchors.
//!
//! Used by property tests (`parse → emit → parse` stability) and by the
//! cross-format matrix (Stage 4). Numbers compare numerically when possible
//! (`0x10` ≡ `16`), because spellings legitimately differ across formats.
//!
//! ```rust
//! use cfgprism_core::{Node, Value, values_equal};
//! let a = Node::new(Value::Bool(true));
//! let b = Node::new(Value::Bool(true));
//! assert!(values_equal(&a, &b));
//! ```

use crate::doc::{Entry, Node, Value};

/// `true` when two nodes carry the same logical value.
/// Maps compare key-by-key **in order**; everything rendering-related
/// (trivia, `*_raw`, spans, styles, anchors, `order`) is ignored.
#[must_use]
pub fn values_equal(a: &Node, b: &Node) -> bool {
    values_equal_inner(&a.value, &b.value)
}

fn values_equal_inner(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Number(x), Value::Number(y)) => numbers_equal(&x.raw, &y.raw),
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Datetime(x), Value::Datetime(y)) => x == y,
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| values_equal(p, q))
        }
        (Value::Map(x), Value::Map(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y.iter())
                    .all(|(p, q)| p.key.text == q.key.text && values_equal(&p.value, &q.value))
        }
        _ => false,
    }
}

/// `true` when two nodes carry the same logical value, ignoring map entry
/// order (arrays stay ordered). Used for targets that legitimately reorder
/// (TOML sections after values). See [`values_equal`] for the ordered form.
#[must_use]
pub fn values_equal_unordered(a: &Node, b: &Node) -> bool {
    values_unordered_inner(&a.value, &b.value)
}

fn values_unordered_inner(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| values_equal(p, q))
        }
        (Value::Map(x), Value::Map(y)) => {
            if x.len() != y.len() {
                return false;
            }
            let mut ys: Vec<&Entry> = y.iter().collect();
            for e in x {
                let Some(pos) = ys.iter().position(|o| o.key.text == e.key.text) else {
                    return false;
                };
                let other = ys.remove(pos);
                if !values_equal(&e.value, &other.value) {
                    return false;
                }
            }
            true
        }
        _ => values_equal_inner(a, b),
    }
}

/// Numeric-aware number comparison: equal integers/floats match even when
/// spelled differently (`0x10` vs `16`, `1.0` vs `1e0`); non-numeric spellings
/// (kept for exotic radixes) fall back to raw equality.
fn numbers_equal(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    if let (Some(x), Some(y)) = (parse_num(a), parse_num(b)) {
        return x == y || (x.is_nan() && y.is_nan());
    }
    false
}

fn parse_num(raw: &str) -> Option<f64> {
    let r = raw.trim();
    if let Some(hex) = r
        .strip_prefix("0x")
        .or_else(|| r.strip_prefix("0X"))
        .or_else(|| r.strip_prefix("+0x"))
        .or_else(|| r.strip_prefix("+0X"))
    {
        return i64::from_str_radix(hex, 16).ok().map(|v| v as f64);
    }
    if let Some(oct) = r
        .strip_prefix("0o")
        .or_else(|| r.strip_prefix("0O"))
        .or_else(|| r.strip_prefix("+0o"))
    {
        return i64::from_str_radix(oct, 8).ok().map(|v| v as f64);
    }
    if r.eq_ignore_ascii_case("inf")
        || r.eq_ignore_ascii_case("+inf")
        || r.eq_ignore_ascii_case("infinity")
        || r.eq_ignore_ascii_case("+infinity")
    {
        return Some(f64::INFINITY);
    }
    if r.eq_ignore_ascii_case("-inf") || r.eq_ignore_ascii_case("-infinity") {
        return Some(f64::NEG_INFINITY);
    }
    if r.eq_ignore_ascii_case("nan") {
        // NaN != NaN by definition; treat textual NaN as equal for IR purposes.
        return Some(f64::NAN);
    }
    r.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{Number, NumberKind};

    fn num(raw: &str) -> Node {
        Node::new(Value::Number(Number {
            raw: raw.to_string(),
            kind: NumberKind::Int,
        }))
    }

    #[test]
    fn hex_equals_decimal() {
        assert!(values_equal(&num("0x10"), &num("16")));
    }

    #[test]
    fn different_numbers_differ() {
        assert!(!values_equal(&num("16"), &num("17")));
    }

    #[test]
    fn nan_text_equals_itself_but_nothing_else() {
        assert!(values_equal(&num("NaN"), &num("nan")));
        assert!(!values_equal(&num("NaN"), &num("1")));
    }
}
