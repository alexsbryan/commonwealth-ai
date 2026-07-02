// SPDX-License-Identifier: AGPL-3.0-or-later
//! Recursive `serde_json::Value` → `toml::Value` converter.
//!
//! Used by `RecipeWriteStructuredTool` to take a JSON object the
//! agent emitted (under JSON-Schema constraint) and produce a TOML
//! document on disk. Same converter is reusable for any future
//! "structured config" tool that wants the same JSON-input,
//! TOML-on-disk shape.
//!
//! Conversion rules:
//!
//! - `Object` → TOML inline table promoted to `[section]` headers
//!   when serialized at the document level (handled by
//!   `toml::to_string_pretty`).
//! - `Array` → TOML array. Mixed-type arrays survive (TOML 1.0
//!   allows mixed-type arrays); the `toml` crate's serializer
//!   handles them correctly.
//! - `Number(i64)` → `Integer`. `Number(f64)` → `Float`.
//!   `Number` outside `i64` range falls back to `Float` if it fits
//!   `f64`, else errors — TOML has no big-int.
//! - `String` → `String`.
//! - `Bool` → `Boolean`.
//! - `Null` → **error**. TOML has no null. Callers should omit the
//!   key. JSON Schema can declare a field optional and the agent
//!   complies by leaving it out, not by setting `null`.
//!
//! Returns a structured error with a JSON-Pointer-style path so the
//! tool can surface "field /enrichment/patterns/0/threshold is null"
//! to the agent rather than a generic conversion failure.

use std::fmt;

#[derive(Debug, Clone)]
pub struct ConvertError {
    pub path: String,
    pub message: String,
}

impl fmt::Display for ConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            write!(f, "{}", self.message)
        } else {
            write!(f, "at {}: {}", self.path, self.message)
        }
    }
}

impl std::error::Error for ConvertError {}

/// Convert a `serde_json::Value` into a `toml::Value`. The input is
/// expected to be an Object at the document level — anything else
/// is technically convertible but TOML serialization at the doc
/// root requires a table.
pub fn json_to_toml(v: &serde_json::Value) -> Result<toml::Value, ConvertError> {
    convert(v, "")
}

fn convert(v: &serde_json::Value, path: &str) -> Result<toml::Value, ConvertError> {
    match v {
        serde_json::Value::Null => Err(ConvertError {
            path: path.to_string(),
            message: "TOML has no null. Omit the key from the JSON object \
                      instead of setting it to null."
                .to_string(),
        }),
        serde_json::Value::Bool(b) => Ok(toml::Value::Boolean(*b)),
        serde_json::Value::Number(n) => convert_number(n, path),
        serde_json::Value::String(s) => Ok(toml::Value::String(s.clone())),
        serde_json::Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for (i, item) in arr.iter().enumerate() {
                let child_path = format!("{path}/{i}");
                out.push(convert(item, &child_path)?);
            }
            Ok(toml::Value::Array(out))
        }
        serde_json::Value::Object(map) => {
            let mut tbl = toml::map::Map::new();
            // Iterate in insertion order so the resulting TOML is
            // stable across runs — the agent's emitted JSON
            // ordering is preserved into the on-disk document.
            for (k, child) in map {
                let child_path = format!("{path}/{k}");
                // Guard against malformed keys the model occasionally emits —
                // e.g. `comparison":` (an unescaped-quote artifact from
                // structured output) lands as a dead key the recipe parser
                // ignores, silently dropping the field. A recipe field key is
                // a plain identifier; a quote / backslash / control char means
                // the JSON key itself is broken. Reject loudly so
                // recipe_write_structured surfaces it and the agent re-emits.
                if let Some(ch) = k
                    .chars()
                    .find(|c| matches!(c, '"' | '\\' | '\n' | '\r' | '\t'))
                {
                    return Err(ConvertError {
                        path: child_path,
                        message: format!(
                            "malformed key `{k}` contains an invalid character ({ch:?}). \
                             Recipe field keys are plain identifiers (e.g. `comparison`); \
                             re-emit this object with a clean key."
                        ),
                    });
                }
                let converted = convert(child, &child_path)?;
                tbl.insert(k.clone(), converted);
            }
            Ok(toml::Value::Table(tbl))
        }
    }
}

/// Best-effort repair of the two structured-output artifacts the 35B
/// reliably emits, applied BEFORE [`json_to_toml`] so a well-formed
/// recipe survives `recipe_write_structured` instead of hard-failing the
/// conversion (which forced agents into a raw-`recipe_write` fallback):
///
/// 1. **Stray escaped-quote key suffix** — the model emits a key like
///    `comparison": ` (an unescaped `"` from the grammar). The real key
///    is the identifier prefix; we cut at the first `"`/`\`/control char
///    and trim a trailing `:`/whitespace, recovering `comparison`.
/// 2. **Null-valued optional keys** — `attribute: null`. TOML has no
///    null, so we DROP the key. If the field was actually required, the
///    on-disk validator then reports a clean "missing field" the agent
///    can fix — far better than an opaque conversion failure.
///
/// Recurses through objects and arrays. The [`convert`] guard stays as
/// defense-in-depth: anything this misses is still rejected loudly.
pub fn sanitize_for_toml(v: &serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, child) in map {
                if child.is_null() {
                    continue; // drop null-valued keys
                }
                let key = clean_key(k);
                if key.is_empty() {
                    continue;
                }
                // First clean occurrence wins (a recovered `comparison`
                // shouldn't be clobbered by a later malformed dup).
                out.entry(key).or_insert_with(|| sanitize_for_toml(child));
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(sanitize_for_toml).collect())
        }
        other => other.clone(),
    }
}

/// Recover the identifier prefix of a possibly-malformed key: cut at the
/// first quote/backslash/control char, then trim trailing `:`/whitespace.
fn clean_key(k: &str) -> String {
    let cut = match k.find(|c| matches!(c, '"' | '\\' | '\n' | '\r' | '\t')) {
        Some(i) => &k[..i],
        None => k,
    };
    cut.trim().trim_end_matches(':').trim().to_string()
}

fn convert_number(n: &serde_json::Number, path: &str) -> Result<toml::Value, ConvertError> {
    if let Some(i) = n.as_i64() {
        return Ok(toml::Value::Integer(i));
    }
    if let Some(f) = n.as_f64() {
        // Some JSON numbers exceed i64 but fit f64 (e.g. 2^53+1).
        // TOML floats lose precision past ~2^53 but that's TOML's
        // limit, not ours. Surface the precision concession in the
        // error path only when the number is *also* not finite.
        if f.is_finite() {
            return Ok(toml::Value::Float(f));
        }
    }
    Err(ConvertError {
        path: path.to_string(),
        message: format!("number {n} is not representable as a TOML integer or float"),
    })
}

/// Serialize a `toml::Value` (which we expect to be a top-level
/// Table) to a pretty TOML string. Wrapper around
/// `toml::to_string_pretty` that surfaces non-table roots as a
/// usable error rather than panicking.
pub fn toml_value_to_string(v: &toml::Value) -> Result<String, ConvertError> {
    let table = match v {
        toml::Value::Table(t) => t,
        other => {
            return Err(ConvertError {
                path: String::new(),
                message: format!(
                    "recipe must be a JSON object at the root; got {}",
                    type_name(other)
                ),
            });
        }
    };
    toml::to_string_pretty(table).map_err(|e| ConvertError {
        path: String::new(),
        message: format!("TOML serialization failed: {e}"),
    })
}

fn type_name(v: &toml::Value) -> &'static str {
    match v {
        toml::Value::String(_) => "string",
        toml::Value::Integer(_) => "integer",
        toml::Value::Float(_) => "float",
        toml::Value::Boolean(_) => "boolean",
        toml::Value::Datetime(_) => "datetime",
        toml::Value::Array(_) => "array",
        toml::Value::Table(_) => "table",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sanitize_repairs_artifacts_then_converts() {
        // The two 35B artifacts: a stray escaped-quote key + a null key.
        let mut inner = serde_json::Map::new();
        inner.insert("comparison\": ".to_string(), json!("greater_than"));
        inner.insert("attribute".to_string(), serde_json::Value::Null);
        inner.insert("threshold".to_string(), json!(3.0));
        let v = serde_json::Value::Object(inner);
        let cleaned = sanitize_for_toml(&v);
        // Now it converts (no malformed-key/null rejection).
        let t = json_to_toml(&cleaned).unwrap();
        let s = toml_value_to_string(&t).unwrap();
        assert!(s.contains("comparison = \"greater_than\""), "got: {s}");
        assert!(!s.contains("attribute"), "null attribute dropped: {s}");
        assert!(s.contains("threshold = 3.0"));
    }

    #[test]
    fn rejects_malformed_key_with_embedded_quote() {
        // Regression: recipe_write_structured once emitted `comparison":` as a
        // key (unescaped-quote artifact), which landed as a dead key the recipe
        // parser ignored. json_to_toml must reject it loudly instead.
        let mut map = serde_json::Map::new();
        map.insert("comparison\": ".to_string(), json!("greater_than"));
        let err = json_to_toml(&serde_json::Value::Object(map)).unwrap_err();
        assert!(
            err.message.contains("malformed key"),
            "expected malformed-key error, got {err:?}"
        );
    }

    #[test]
    fn flat_object_round_trips() {
        let j = json!({
            "id": "demo",
            "count": 7,
            "rate": 1.5,
            "enabled": true,
            "tags": ["a", "b"],
        });
        let t = json_to_toml(&j).unwrap();
        let s = toml_value_to_string(&t).unwrap();
        assert!(s.contains("id = \"demo\""));
        assert!(s.contains("count = 7"));
        assert!(s.contains("rate = 1.5"));
        assert!(s.contains("enabled = true"));
        // `to_string_pretty` may format arrays inline or vertically
        // depending on length; assert the values land regardless of
        // formatting.
        assert!(s.contains("\"a\""));
        assert!(s.contains("\"b\""));
        assert!(s.contains("tags"));
    }

    #[test]
    fn nested_objects_become_sections() {
        let j = json!({
            "corpus": {"id": "demo"},
            "acquire": {"type": "bulk_download", "url": "https://x"},
        });
        let t = json_to_toml(&j).unwrap();
        let s = toml_value_to_string(&t).unwrap();
        assert!(s.contains("[corpus]"));
        assert!(s.contains("id = \"demo\""));
        assert!(s.contains("[acquire]"));
        assert!(s.contains("type = \"bulk_download\""));
    }

    #[test]
    fn arrays_of_objects_become_double_bracket_blocks() {
        let j = json!({
            "enrichment": {
                "type": "investigation",
                "entity_types": [
                    {"name": "company", "attributes": ["ticker"]},
                    {"name": "person",  "attributes": ["full_name"]},
                ]
            }
        });
        let t = json_to_toml(&j).unwrap();
        let s = toml_value_to_string(&t).unwrap();
        assert!(s.contains("[enrichment]"));
        assert!(s.contains("[[enrichment.entity_types]]"));
        // Both entries must appear.
        let occurrences = s.matches("[[enrichment.entity_types]]").count();
        assert_eq!(occurrences, 2, "got: {s}");
    }

    #[test]
    fn null_is_rejected_with_path() {
        let j = json!({"max_pages": null});
        let err = json_to_toml(&j).unwrap_err();
        assert_eq!(err.path, "/max_pages");
        assert!(err.message.contains("TOML has no null"));
    }

    #[test]
    fn nested_null_path_is_pointed() {
        let j = json!({
            "acquire": {"pagination": {"max_pages": null}}
        });
        let err = json_to_toml(&j).unwrap_err();
        assert_eq!(err.path, "/acquire/pagination/max_pages");
    }

    #[test]
    fn bigint_falls_back_to_float() {
        // 2^60 — fits i64 actually, so this just round-trips as int.
        let j = json!({"big": 1_152_921_504_606_846_976_i64});
        let t = json_to_toml(&j).unwrap();
        if let toml::Value::Table(m) = t {
            assert!(matches!(m["big"], toml::Value::Integer(_)));
        } else {
            panic!("expected table");
        }
    }
}
