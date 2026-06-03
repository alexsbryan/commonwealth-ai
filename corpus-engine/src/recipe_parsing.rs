//! TOML parsing + parameter validation + serde error rewriting —
//! extracted out of `crate::recipe`.
//!
//! Pure helper free fns. `Recipe::from_toml` and
//! `Recipe::resolve_parameters` (still in `recipe.rs`) call into here.
//! Behaviour-preserving — same diagnostics, same arms, same wording.

use crate::error::{Error, Result};
use crate::recipe::{ParameterKind, ParameterValue, MAX_SCHEMA_VERSION};

pub(crate) fn empty_value(kind: &ParameterKind) -> ParameterValue {
    match kind {
        ParameterKind::String | ParameterKind::Date => ParameterValue::String(String::new()),
        ParameterKind::Int => ParameterValue::Int(0),
        ParameterKind::List => ParameterValue::List(Vec::new()),
    }
}

pub(crate) fn parameter_value_from_toml(
    name: &str,
    kind: &ParameterKind,
    v: toml::Value,
) -> Result<ParameterValue> {
    match (kind, v) {
        (ParameterKind::String, toml::Value::String(s)) => Ok(ParameterValue::String(s)),
        (ParameterKind::Int, toml::Value::Integer(i)) => Ok(ParameterValue::Int(i)),
        (ParameterKind::Int, toml::Value::String(s)) => {
            s.parse::<i64>().map(ParameterValue::Int).map_err(|e| {
                Error::InvalidInput(format!("parameter `{name}` is not an integer: {s} ({e})"))
            })
        }
        (ParameterKind::Date, toml::Value::String(s)) => {
            if !is_iso_date(&s) {
                return Err(Error::InvalidInput(format!(
                    "parameter `{name}` is not an ISO-8601 date (YYYY-MM-DD): {s}"
                )));
            }
            Ok(ParameterValue::Date(s))
        }
        (ParameterKind::List, toml::Value::Array(arr)) => {
            let mut items = Vec::with_capacity(arr.len());
            for item in arr {
                match item {
                    toml::Value::String(s) => items.push(s),
                    other => {
                        return Err(Error::InvalidInput(format!(
                            "parameter `{name}` list entries must be strings, got: {other:?}"
                        )))
                    }
                }
            }
            Ok(ParameterValue::List(items))
        }
        // Convenience: comma-separated string for list parameters.
        // The CLI prompt yields one string; the desktop form yields
        // a true array. Both should work.
        (ParameterKind::List, toml::Value::String(s)) => {
            let items = s
                .split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect();
            Ok(ParameterValue::List(items))
        }
        (kind, other) => Err(Error::InvalidInput(format!(
            "parameter `{name}` expected {kind:?}, got TOML value: {other:?}"
        ))),
    }
}

/// Refuse recipes whose declared `schema_version` is higher than
/// the engine knows. See [`MAX_SCHEMA_VERSION`].
pub(crate) fn check_schema_version(v: u32) -> Result<()> {
    if v > MAX_SCHEMA_VERSION {
        return Err(Error::Recipe(format!(
            "recipe declares schema_version = {v} but this engine \
             supports schema_version <= {MAX_SCHEMA_VERSION}. \
             The recipe was authored against a newer engine; \
             upgrade `corpus-engine` to load it."
        )));
    }
    Ok(())
}

/// Translate a serde TOML parse error into something actionable
/// for the recipe author. Three rewrite passes, in order:
///
/// 1. **Deprecation aliases** (e.g. `api_paginated` → `http_api`):
///    name the replacement so the user doesn't reverse-engineer
///    the rename from a generic "unknown variant" message.
/// 2. **Missing required fields**: rephrase `missing field 'X'`
///    in plain language and, when the field is a section we know
///    well, list valid `type` values inline. The default serde
///    message points the caret at line 1 even when the issue is
///    "the section doesn't exist anywhere" — that's misleading and
///    the rewrite drops it.
/// 3. **Unknown enum variants**: name the field path that the
///    bad value was assigned to, when the parse error carries
///    enough position info to recover it. The default message
///    quotes the bad value but not the field, so a recipe with
///    `[acquire.follow] document_format = "pdf"` reads as just
///    "unknown variant 'pdf'" with no field hint.
///
/// Falls through to the raw serde message when no rewrite
/// applies — better to surface the technical error than to
/// invent a "helpful" rewrite that misdescribes the failure.
pub(crate) fn translate_parse_error(e: toml::de::Error) -> Error {
    const DEPRECATIONS: &[(&str, &str, &str)] = &[
        // (deprecated_name, replacement, since)
        (
            "api_paginated",
            "http_api",
            "PR1 — recipe-authoring platform",
        ),
    ];
    let raw = e.to_string();

    // 1. Deprecation aliases — keep first so a deprecated variant
    //    name takes precedence over the generic "unknown variant"
    //    rewrite below.
    for (old, new, since) in DEPRECATIONS {
        if raw.contains(old) {
            return Error::Recipe(format!(
                "recipe references the removed acquirer/extractor type \
                 `{old}`. Migrate to `{new}` (replaced in {since}). \
                 See SYSTEM_OVERVIEW.md §3.10. Underlying parse error: {raw}"
            ));
        }
    }

    // 2. Missing required field — `missing field \`X\`` (single
    //    backticks in serde's output).
    if let Some(field) = extract_missing_field(&raw) {
        return Error::Recipe(rewrite_missing_field(&field, &raw));
    }

    // 3. Unknown variant — `unknown variant \`X\`, expected one of …`
    if let Some((bad_value, allowed)) = extract_unknown_variant(&raw) {
        return Error::Recipe(rewrite_unknown_variant(&bad_value, &allowed, &raw));
    }

    Error::Recipe(raw)
}

/// Pull the field name out of a serde `missing field \`X\`` message.
pub(crate) fn extract_missing_field(raw: &str) -> Option<String> {
    let anchor = "missing field `";
    let start = raw.find(anchor)? + anchor.len();
    let rest = &raw[start..];
    let end = rest.find('`')?;
    Some(rest[..end].to_string())
}

/// Pull `(bad_value, allowed_csv)` out of a serde
/// `unknown variant \`X\`, expected one of \`a\`, \`b\`, …` message.
pub(crate) fn extract_unknown_variant(raw: &str) -> Option<(String, String)> {
    let var_anchor = "unknown variant `";
    let var_start = raw.find(var_anchor)? + var_anchor.len();
    let after_var = &raw[var_start..];
    let var_end = after_var.find('`')?;
    let bad_value = after_var[..var_end].to_string();
    // Allowed list: everything between "expected one of " and the
    // end of the line / next backtick-free run. Serde emits the
    // list with backticks; surface it as plain CSV.
    let allowed_anchor = "expected one of ";
    let allowed_start = raw.find(allowed_anchor)? + allowed_anchor.len();
    let allowed_chunk = &raw[allowed_start..];
    let allowed_end = allowed_chunk.find('\n').unwrap_or(allowed_chunk.len());
    let allowed = allowed_chunk[..allowed_end].replace('`', "");
    Some((bad_value, allowed))
}

/// Compose a plain-language explanation for a missing required key,
/// and inline the valid `type` values when the missing field names
/// a section whose `type` enum we know up-front. The known sections
/// stay narrow on purpose — better to fall back to the raw serde
/// message than to give wrong "valid types" guidance.
fn rewrite_missing_field(field: &str, raw: &str) -> String {
    match field {
        "acquire" => format!(
            "Recipe is missing the `[acquire]` section. Every recipe needs \
             one. Add it with `type = \"...\"` (one of: bulk_download | \
             http_api | web_crawl | local_file | huggingface_dataset). \
             Underlying parser error: {raw}"
        ),
        "extract" => format!(
            "Recipe is missing the `[extract]` section. Add it with \
             `type = \"...\"` (one of: plaintext | html | html_sections | \
             json | jsonl | csv | parquet | mediawiki_xml | \
             stackexchange_xml | wikipedia_jsonl | wikipedia_structured | \
             wikipedia_catalog | wikipedia_api_article | gutenberg_catalog \
             | code | markdown). Underlying parser error: {raw}"
        ),
        "chunk" => format!(
            "Recipe is missing the `[chunk]` section. Add it with \
             `type = \"...\"` (one of: paragraph | sentence | fixed | \
             semantic | passthrough). Underlying parser error: {raw}"
        ),
        "corpus" => format!(
            "Recipe is missing the `[corpus]` section. Every recipe needs \
             one with at least `id = \"...\"` and `name = \"...\"`. \
             Underlying parser error: {raw}"
        ),
        "type" => format!(
            "A section is missing its required `type` field. Look at the \
             TOML caret below to see which section. Each acquirer / \
             extractor / chunker / pattern needs an explicit `type = \
             \"...\"`. Underlying parser error: {raw}"
        ),
        "base_url" => format!(
            "An `[acquire]` block with `type = \"http_api\"` is missing \
             `base_url`. Add `base_url = \"https://api.example.com\"`. \
             Underlying parser error: {raw}"
        ),
        "id" | "name" => format!(
            "The `[corpus]` section is missing required field `{field}`. \
             Both `id` (stable identifier) and `name` (display name) are \
             required. Underlying parser error: {raw}"
        ),
        "document_path" => format!(
            "An `[extract]` block with `type = \"json\"` is missing \
             `document_path`. Set it to a JSONPath that selects the \
             documents array (e.g. `$.results[*]`). Underlying parser \
             error: {raw}"
        ),
        "content_field" => format!(
            "An `[extract]` block is missing `content_field` — the name \
             of the JSON field on each matched object that holds the \
             document body text. Underlying parser error: {raw}"
        ),
        _ => format!(
            "Recipe is missing required field `{field}`. Add it to the \
             section the parser caret points at below. Underlying parser \
             error: {raw}"
        ),
    }
}

/// Compose a plain-language explanation for an unknown enum value,
/// naming the field path when the parse error carries enough span
/// info for us to recover it. Serde's default points the caret at
/// the assignment but doesn't quote the field name in the error
/// text, which makes the message read as just "unknown variant 'X'".
fn rewrite_unknown_variant(bad_value: &str, allowed: &str, raw: &str) -> String {
    let field_hint = extract_field_from_span(raw, bad_value);
    let field_phrase = match field_hint.as_deref() {
        Some(f) => format!("field `{f}`"),
        None => "a field".to_string(),
    };
    format!(
        "{field_phrase} got `{bad_value}` but allowed values are: \
         {allowed}. Underlying parser error: {raw}"
    )
}

/// Best-effort extraction of `key` from a serde TOML error span
/// containing `key = "<bad_value>"` or similar. Walks the message
/// line-by-line looking for an `=` neighbouring the bad value.
fn extract_field_from_span(raw: &str, bad_value: &str) -> Option<String> {
    for line in raw.lines() {
        let trimmed = line.trim_start_matches(|c: char| c.is_ascii_digit() || c == '|' || c == ' ');
        if !trimmed.contains(bad_value) || !trimmed.contains('=') {
            continue;
        }
        let key_part = trimmed.split('=').next()?.trim();
        if !key_part.is_empty()
            && key_part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
        {
            return Some(key_part.to_string());
        }
    }
    None
}

/// Lexical ISO-8601 calendar-date check (`YYYY-MM-DD`). We don't
/// validate semantic correctness (e.g. February 30) here — that's
/// the caller's job. This function exists so the recipe schema
/// doesn't gain a dependency on `chrono` purely for parameter
/// validation.
pub(crate) fn is_iso_date(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[..4].iter().all(|c| c.is_ascii_digit())
        && bytes[5..7].iter().all(|c| c.is_ascii_digit())
        && bytes[8..].iter().all(|c| c.is_ascii_digit())
}
