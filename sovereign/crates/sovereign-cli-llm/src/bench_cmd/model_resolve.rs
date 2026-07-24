// SPDX-License-Identifier: AGPL-3.0-or-later
//! Resolve a slot alias (`primary`, `fast`, …) to the **concrete**
//! model behind it, for benchmark attribution.
//!
//! The daemon reports the alias back as the `model_id` on every
//! inference response, so a baseline captured against `primary`
//! records only `"primary"` — useless once the alias is repointed at
//! a different GGUF. This module closes that gap: it asks the daemon's
//! `/v1/models` surface which concrete model the alias currently
//! resolves to, then enriches it with human-readable identity
//! (`base_name`, `family`, `quant`) from the bundled manifest.
//!
//! Resolution surface — `GET {base_url}/v1/models` returns a `data[]`
//! where concrete rows carry `owned_by: "mesh"` and alias rows carry
//! `owned_by: "alias→<concrete-stem>"` (see
//! `commonwealth-api/src/routes_inference.rs::list_models`). So the
//! alias row *is* the resolution: strip the `alias→` prefix.
//!
//! Design: the pure [`attribution_from_models_json`] does the parse +
//! manifest enrichment and is fully unit-tested against fixture JSON;
//! [`resolve_model_attribution`] is the thin HTTP wrapper. Resolution
//! is best-effort — a bench must never abort because `/v1/models`
//! hiccuped — so the wrapper returns `None` and lets the caller record
//! an unattributed baseline (which the report rollup buckets
//! honestly) rather than a wrong one.

use std::time::Duration;

use serde_json::Value;
use sovereign_core::models_manifest::{ModelAttribution, DEFAULT_MANIFEST};

use super::lane_baseline::is_alias_marker;

/// The `alias→` marker prefix on an alias row's `owned_by`. The arrow
/// is U+2192; we also accept the ASCII `alias->` for robustness.
const ARROW_PREFIXES: [&str; 2] = ["alias\u{2192}", "alias->"];

fn is_embed_id(id: &str) -> bool {
    let lower = id.to_ascii_lowercase();
    lower.contains("embedding") || lower.contains("-embed")
}

/// Parse a `/v1/models` response body and resolve `alias` to a
/// [`ModelAttribution`]. Pure — no I/O — so it can be exercised
/// against canned JSON.
///
/// Resolution order:
///   1. The row with `id == alias`: an alias row (`owned_by ==
///      "alias→<stem>"`) resolves to its target; a concrete row
///      (`owned_by == "mesh"`) means the caller already passed a
///      concrete stem — it resolves to itself.
///   2. Fallback, ONLY when `alias` is a slot alias (`primary` /
///      `fast` / …): the first concrete non-embed row (`owned_by ==
///      "mesh"`). Covers a daemon that advertises the loaded model
///      but not the alias (e.g. a stripped harness), and BYOM setups.
///      A *concrete* stem that isn't in the list must NOT fall back —
///      picking an arbitrary listed model would mislabel the run,
///      which is strictly worse than recording it unattributed.
///
/// Returns `None` when neither yields a concrete stem — the caller
/// then records an unattributed baseline rather than guessing.
pub fn attribution_from_models_json(v: &Value, alias: &str) -> Option<ModelAttribution> {
    let arr = v.get("data").and_then(Value::as_array)?;

    // (1) The row matching the requested id: alias rows carry the
    // concrete target in `owned_by`; a concrete row IS the answer.
    let mut concrete: Option<String> = None;
    for m in arr {
        let id = m.get("id").and_then(Value::as_str).unwrap_or_default();
        if id != alias {
            continue;
        }
        if let Some(ob) = m.get("owned_by").and_then(Value::as_str) {
            for pfx in ARROW_PREFIXES {
                if let Some(stem) = ob.strip_prefix(pfx) {
                    let stem = stem.trim();
                    if !stem.is_empty() {
                        concrete = Some(stem.to_string());
                    }
                    break;
                }
            }
            if concrete.is_none() && ob == "mesh" {
                concrete = Some(id.to_string());
            }
        }
        if concrete.is_some() {
            break;
        }
    }

    // (2) Fallback — slot aliases only (see doc). A concrete stem the
    // daemon doesn't list yields None, never some other model.
    if concrete.is_none() && is_alias_marker(alias) {
        for m in arr {
            let id = m.get("id").and_then(Value::as_str).unwrap_or_default();
            let ob = m
                .get("owned_by")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if ob == "mesh" && !id.is_empty() && !is_embed_id(id) {
                concrete = Some(id.to_string());
                break;
            }
        }
    }

    let stem = concrete?;
    let mut attr = DEFAULT_MANIFEST.attribution_for_file(&stem);
    attr.alias = Some(alias.to_string());
    Some(attr)
}

/// Resolve `alias` against the running daemon at `base_url` (no
/// trailing `/v1`). Best-effort: returns `None` on any transport or
/// shape failure, logging a warning, so a long bench is never aborted
/// by a resolution hiccup — an unattributed baseline is strictly
/// better than a mislabelled one.
pub async fn resolve_model_attribution(base_url: &str, alias: &str) -> Option<ModelAttribution> {
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[bench] WARN: model-resolve http client build failed: {e}");
            return None;
        }
    };
    let resp = match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            eprintln!(
                "[bench] WARN: GET {url} returned {} — recording unattributed baseline",
                r.status()
            );
            return None;
        }
        Err(e) => {
            eprintln!("[bench] WARN: GET {url} failed: {e} — recording unattributed baseline");
            return None;
        }
    };
    let body: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[bench] WARN: parse {url}: {e} — recording unattributed baseline");
            return None;
        }
    };
    let attr = attribution_from_models_json(&body, alias);
    if attr.is_none() {
        eprintln!("[bench] WARN: /v1/models had no concrete model for alias '{alias}' — recording unattributed baseline");
    }
    attr
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn models_response() -> Value {
        // Mirrors the real daemon shape + the in-repo fixture at
        // enrich_cmd/inference_client.rs:1228.
        json!({
            "object": "list",
            "data": [
                {"id": "Qwen3.5-4B-Q4_K_M", "owned_by": "mesh"},
                {"id": "Qwen3-Embedding-0.6B-Q8_0", "owned_by": "mesh"},
                {"id": "primary", "owned_by": "alias\u{2192}Qwen3.5-4B-Q4_K_M"},
                {"id": "commonwealth/primary", "owned_by": "alias\u{2192}Qwen3.5-4B-Q4_K_M"},
                {"id": "embed", "owned_by": "alias\u{2192}Qwen3-Embedding-0.6B-Q8_0"}
            ]
        })
    }

    #[test]
    fn resolves_primary_alias_to_concrete_and_enriches() {
        let a = attribution_from_models_json(&models_response(), "primary")
            .expect("primary alias must resolve");
        assert_eq!(a.file_stem, "Qwen3.5-4B-Q4_K_M");
        // Enriched from the bundled manifest.
        assert_eq!(a.base_name.as_deref(), Some("Qwen3.5-4B"));
        assert_eq!(a.quant.as_deref(), Some("Q4_K_M"));
        assert_eq!(a.alias.as_deref(), Some("primary"));
    }

    #[test]
    fn falls_back_to_first_concrete_non_embed_when_alias_absent() {
        // A response with no alias rows — resolve the loaded chat model.
        let v = json!({"data": [
            {"id": "Qwen3-Embedding-0.6B-Q8_0", "owned_by": "mesh"},
            {"id": "Qwen3.5-4B-Q4_K_M", "owned_by": "mesh"}
        ]});
        let a = attribution_from_models_json(&v, "primary").expect("fallback must resolve");
        assert_eq!(a.file_stem, "Qwen3.5-4B-Q4_K_M");
        assert_eq!(a.alias.as_deref(), Some("primary"));
    }

    #[test]
    fn concrete_stem_resolves_to_itself_not_first_row() {
        // The user passed a concrete stem via --chat-model. It must
        // resolve to ITSELF, never to whichever concrete row happens
        // to come first (here the 4B is listed before it).
        let v = json!({"data": [
            {"id": "Qwen3.5-4B-Q4_K_M", "owned_by": "mesh"},
            {"id": "Qwopus3.5-4B-Q6_K", "owned_by": "mesh"}
        ]});
        let a = attribution_from_models_json(&v, "Qwopus3.5-4B-Q6_K")
            .expect("concrete stem must resolve");
        assert_eq!(a.file_stem, "Qwopus3.5-4B-Q6_K");
        assert_eq!(a.alias.as_deref(), Some("Qwopus3.5-4B-Q6_K"));
    }

    #[test]
    fn unknown_concrete_stem_is_none_never_another_model() {
        // A concrete stem the daemon doesn't list must NOT fall back
        // to the first listed model — unattributed beats mislabelled.
        let v = json!({"data": [
            {"id": "Qwen3.5-4B-Q4_K_M", "owned_by": "mesh"}
        ]});
        assert!(attribution_from_models_json(&v, "SomeOther-13B-Q8_0").is_none());
    }

    #[test]
    fn none_when_only_embed_present() {
        let v = json!({"data": [
            {"id": "Qwen3-Embedding-0.6B-Q8_0", "owned_by": "mesh"}
        ]});
        assert!(attribution_from_models_json(&v, "primary").is_none());
    }

    #[test]
    fn none_on_missing_data_array() {
        assert!(attribution_from_models_json(&json!({}), "primary").is_none());
    }
}
