// SPDX-License-Identifier: AGPL-3.0-or-later
//! The conformance checks.
//!
//! Split into two halves: **pure validators** over an already-fetched
//! `ProviderManifest` (unit-tested here, no network), and the **async runners**
//! that make the HTTP calls and turn results into [`Check`]s. Each runner is
//! gated: a check for an un-advertised feature returns `Skip`, never `Fail`.

use oicp_types::{features, ProviderManifest};
use serde_json::json;

use crate::args::{self, Args};
use crate::report::{Check, Level};

/// A thin HTTP client bound to one host + optional bearer.
pub struct Host {
    base: String,
    token: Option<String>,
    client: reqwest::Client,
}

impl Host {
    pub fn new(base: &str, token: Option<String>) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            token,
            client: reqwest::Client::new(),
        }
    }

    fn req(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        let mut r = self.client.request(method, url);
        if let Some(t) = &self.token {
            r = r.bearer_auth(t);
        }
        r
    }

    /// Join an origin-relative endpoint (`/oicp/…`) or pass an absolute URL.
    fn url(&self, path: &str) -> String {
        if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else if let Some(rest) = path.strip_prefix('/') {
            format!("{}/{}", self.base, rest)
        } else {
            format!("{}/{}", self.base, path)
        }
    }
}

// ── Pure validators (unit-tested, no network) ────────────────────────────────

/// §4 manifest invariants: every claim's affinity is in `[0,1]` and its
/// `max_context` does not exceed the model's `context_tokens`.
pub fn manifest_invariant_failures(m: &ProviderManifest) -> Vec<String> {
    let mut f = Vec::new();
    if m.oicp_version.is_empty() {
        f.push("oicp_version is empty".to_string());
    }
    for model in &m.models {
        for (i, claim) in model.claims.iter().enumerate() {
            if !(0.0..=1.0).contains(&claim.affinity) {
                f.push(format!(
                    "model `{}` claim #{i}: affinity {} outside [0,1]",
                    model.id, claim.affinity
                ));
            }
            if claim.max_context > model.context_tokens {
                f.push(format!(
                    "model `{}` claim #{i}: max_context {} exceeds context_tokens {}",
                    model.id, claim.max_context, model.context_tokens
                ));
            }
        }
    }
    f
}

/// §2 feature invariants: every advertised feature is registered or a
/// well-formed `x:` extension, and `ingest:v1` iff `knowledge.ingest`.
pub fn feature_failures(m: &ProviderManifest) -> Vec<String> {
    let mut f = Vec::new();
    for feat in &m.features {
        if !features::is_valid(feat) {
            f.push(format!(
                "feature `{feat}` is neither registered nor a well-formed `x:` extension"
            ));
        }
    }
    let advertises_ingest = m.has_feature(features::INGEST_V1);
    let has_ingest_section = m
        .knowledge
        .as_ref()
        .and_then(|k| k.ingest.as_ref())
        .is_some();
    if advertises_ingest != has_ingest_section {
        f.push(format!(
            "ingest:v1 feature ({advertises_ingest}) must co-occur with knowledge.ingest \
             ({has_ingest_section})"
        ));
    }
    if m.has_feature(features::INGEST_RECIPE_TEST) {
        let has_test = m
            .knowledge
            .as_ref()
            .and_then(|k| k.ingest.as_ref())
            .and_then(|i| i.test_endpoint.as_ref())
            .is_some();
        if !has_test {
            f.push(
                "ingest:recipe_test feature requires knowledge.ingest.test_endpoint".to_string(),
            );
        }
    }
    f
}

// ── The run ──────────────────────────────────────────────────────────────────

/// Run every applicable check. `manifest` is fetched once up front (a `None`
/// means the host has no OICP surface — `manifest.schema` fails and the rest
/// skip). Returns the checks in a stable, most-foundational-first order.
pub async fn run_all(host: &Host, args: &Args) -> Vec<Check> {
    let mut out = Vec::new();

    // manifest.schema — the load-bearing one: fetch + deserialize via oicp-types.
    let manifest = match fetch_manifest(host).await {
        Ok(m) => {
            let inv = manifest_invariant_failures(&m);
            if inv.is_empty() {
                out.push(Check::pass(
                    "manifest.schema",
                    Level::Must,
                    format!(
                        "{} model(s), oicp_version {}",
                        m.models.len(),
                        m.oicp_version
                    ),
                ));
            } else {
                out.push(Check::fail("manifest.schema", Level::Must, inv.join("; ")));
            }
            Some(m)
        }
        Err(e) => {
            out.push(Check::fail(
                "manifest.schema",
                Level::Must,
                format!("could not fetch/parse /oicp/v1/capabilities: {e}"),
            ));
            None
        }
    };

    let Some(m) = manifest else {
        // Without a manifest nothing else is applicable.
        for id in [
            "manifest.features",
            "manifest.embed_model",
            "inference.baseline",
        ] {
            out.push(Check::skip(id, Level::Must, "no manifest"));
        }
        return out;
    };

    // manifest.features
    let ff = feature_failures(&m);
    out.push(if ff.is_empty() {
        Check::pass(
            "manifest.features",
            Level::Must,
            if m.features.is_empty() {
                "no features advertised (v0.3 host)".to_string()
            } else {
                m.features.join(", ")
            },
        )
    } else {
        Check::fail("manifest.features", Level::Must, ff.join("; "))
    });

    // manifest.embed_model
    out.push(match &m.knowledge {
        None => Check::skip("manifest.embed_model", Level::Should, "no knowledge plane"),
        Some(k) if k.embed_model.is_some() => Check::pass(
            "manifest.embed_model",
            Level::Should,
            "embed_model populated",
        ),
        Some(_) => Check::fail(
            "manifest.embed_model",
            Level::Should,
            "v0.4 host with a knowledge plane MUST advertise knowledge.embed_model",
        ),
    });

    // inference.baseline + model_identity
    out.extend(check_inference(host, &m).await);

    // constraint.* (feature-gated)
    out.push(check_constraint_json_object(host, &m).await);
    out.push(check_constraint_json_schema(host, &m).await);
    out.push(check_constraint_lark(host, &m).await);

    // embed.bitcompat (gated on an advertised embed model)
    out.push(check_embed_bitcompat(host, &m).await);

    // knowledge.search (gated on a knowledge plane)
    out.push(check_knowledge_search(host, &m).await);

    // ingest.state_machine (gated on ingest:v1 + --fixture-recipe)
    out.push(check_ingest_state_machine(host, &m, args).await);

    // auth.posture (gated on a non-loopback host)
    out.push(check_auth_posture(host, &m, args).await);

    out
}

async fn fetch_manifest(host: &Host) -> Result<ProviderManifest, String> {
    let url = host.url("/oicp/v1/capabilities");
    let resp = host
        .req(reqwest::Method::GET, &url)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    resp.json::<ProviderManifest>()
        .await
        .map_err(|e| e.to_string())
}

/// POST a chat completion, returning the raw JSON body.
async fn chat(host: &Host, body: serde_json::Value) -> Result<serde_json::Value, String> {
    let url = host.url("/v1/chat/completions");
    let resp = host
        .req(reqwest::Method::POST, &url)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("HTTP {status}: {v}"));
    }
    Ok(v)
}

fn first_model(m: &ProviderManifest) -> Option<&str> {
    m.models.first().map(|x| x.id.as_str())
}

fn content(v: &serde_json::Value) -> Option<&str> {
    v.get("choices")?
        .get(0)?
        .get("message")?
        .get("content")?
        .as_str()
}

async fn check_inference(host: &Host, m: &ProviderManifest) -> Vec<Check> {
    let Some(model) = first_model(m) else {
        return vec![
            Check::skip(
                "inference.baseline",
                Level::Must,
                "manifest lists no models",
            ),
            Check::skip(
                "inference.model_identity",
                Level::Must,
                "manifest lists no models",
            ),
        ];
    };
    let body = json!({
        "model": model,
        "messages": [{"role": "user", "content": "Reply with the single word: ok"}],
        "max_tokens": 16,
        "temperature": 0.0,
    });
    match chat(host, body).await {
        Err(e) => vec![
            Check::fail("inference.baseline", Level::Must, e),
            Check::skip(
                "inference.model_identity",
                Level::Must,
                "baseline call failed",
            ),
        ],
        Ok(v) => {
            let baseline = if content(&v).is_some() {
                Check::pass(
                    "inference.baseline",
                    Level::Must,
                    "chat completion returned content",
                )
            } else {
                Check::fail(
                    "inference.baseline",
                    Level::Must,
                    format!("no message content: {v}"),
                )
            };
            // model_identity: response `model` must be a manifest id; if the
            // host advertises model_fingerprint, its meta must echo one.
            let resp_model = v.get("model").and_then(|x| x.as_str());
            let ids: Vec<&str> = m.models.iter().map(|x| x.id.as_str()).collect();
            let identity = match resp_model {
                Some(rm) if ids.contains(&rm) => {
                    if m.has_feature(features::MODEL_FINGERPRINT) {
                        let echoed = v
                            .get("oicp")
                            .and_then(|o| o.get("model_fingerprint"))
                            .and_then(|f| f.as_str())
                            .is_some();
                        if echoed {
                            Check::pass(
                                "inference.model_identity",
                                Level::Must,
                                format!("model `{rm}` ∈ manifest; fingerprint echoed"),
                            )
                        } else {
                            Check::fail(
                                "inference.model_identity",
                                Level::Must,
                                "model_fingerprint advertised but not echoed in response meta",
                            )
                        }
                    } else {
                        Check::pass(
                            "inference.model_identity",
                            Level::Must,
                            format!("response model `{rm}` ∈ manifest ids"),
                        )
                    }
                }
                Some(rm) => Check::fail(
                    "inference.model_identity",
                    Level::Must,
                    format!("response model `{rm}` is not in the manifest"),
                ),
                None => Check::fail(
                    "inference.model_identity",
                    Level::Must,
                    "response omitted the resolved `model` id",
                ),
            };
            vec![baseline, identity]
        }
    }
}

async fn check_constraint_json_object(host: &Host, m: &ProviderManifest) -> Check {
    let id = "constraint.json_object";
    // json_object is OpenAI baseline — probe it even when not explicitly
    // advertised, but only FAIL when the host claims it.
    let advertised = m.has_feature(features::CONSTRAINT_JSON_OBJECT);
    let Some(model) = first_model(m) else {
        return Check::skip(id, Level::Feature, "no models");
    };
    let body = json!({
        "model": model,
        "messages": [{"role": "user", "content": "Return a JSON object with key ok=true."}],
        "response_format": {"type": "json_object"},
        "max_tokens": 64,
        "temperature": 0.0,
    });
    match chat(host, body).await {
        Ok(v) => {
            match content(&v).and_then(|c| serde_json::from_str::<serde_json::Value>(c).ok()) {
                Some(j) if j.is_object() => {
                    Check::pass(id, Level::Feature, "output is a JSON object")
                }
                _ if advertised => Check::fail(
                    id,
                    Level::Feature,
                    "advertised but output was not a JSON object",
                ),
                _ => Check::skip(
                    id,
                    Level::Feature,
                    "not advertised; output not valid JSON (allowed)",
                ),
            }
        }
        Err(e) if advertised => Check::fail(id, Level::Feature, e),
        Err(_) => Check::skip(id, Level::Feature, "not advertised; call errored (allowed)"),
    }
}

async fn check_constraint_json_schema(host: &Host, m: &ProviderManifest) -> Check {
    let id = "constraint.json_schema";
    if !m.has_feature(features::CONSTRAINT_JSON_SCHEMA) {
        return Check::skip(id, Level::Feature, "not advertised");
    }
    let Some(model) = first_model(m) else {
        return Check::skip(id, Level::Feature, "no models");
    };
    // Fixed probe schema: {answer: string}. Hand-checked (no jsonschema crate).
    let schema = json!({
        "type": "object",
        "properties": {"answer": {"type": "string"}},
        "required": ["answer"],
        "additionalProperties": false,
    });
    let body = json!({
        "model": model,
        "messages": [{"role": "user", "content": "Put the word ok in the answer field."}],
        "response_format": {"type": "json_schema", "json_schema": {"name": "probe", "schema": schema}},
        "max_tokens": 64,
        "temperature": 0.0,
    });
    match chat(host, body).await {
        Ok(v) => {
            match content(&v).and_then(|c| serde_json::from_str::<serde_json::Value>(c).ok()) {
                Some(j)
                    if j.get("answer").and_then(|a| a.as_str()).is_some()
                        && j.as_object().map(|o| o.len() == 1).unwrap_or(false) =>
                {
                    Check::pass(id, Level::Feature, "output conforms to the probe schema")
                }
                Some(j) => Check::fail(
                    id,
                    Level::Feature,
                    format!("output violates probe schema: {j}"),
                ),
                None => Check::fail(id, Level::Feature, "output was not valid JSON"),
            }
        }
        Err(e) => Check::fail(id, Level::Feature, e),
    }
}

async fn check_constraint_lark(host: &Host, m: &ProviderManifest) -> Check {
    let id = "constraint.lark";
    if !m.has_feature(features::CONSTRAINT_LARK) {
        return Check::skip(id, Level::Feature, "not advertised");
    }
    let Some(model) = first_model(m) else {
        return Check::skip(id, Level::Feature, "no models");
    };
    let body = json!({
        "model": model,
        "messages": [{"role": "user", "content": "Answer yes or no: is water wet?"}],
        "lark_grammar": "start: \"yes\" | \"no\"",
        "max_tokens": 8,
        "temperature": 0.0,
    });
    match chat(host, body).await {
        Ok(v) => match content(&v).map(|c| c.trim()) {
            Some(c) if c == "yes" || c == "no" => Check::pass(
                id,
                Level::Feature,
                format!("output `{c}` ∈ grammar language"),
            ),
            Some(c) => Check::fail(
                id,
                Level::Feature,
                format!("output `{c}` not in {{yes,no}}"),
            ),
            None => Check::fail(id, Level::Feature, "no content"),
        },
        Err(e) => Check::fail(id, Level::Feature, e),
    }
}

async fn check_embed_bitcompat(host: &Host, m: &ProviderManifest) -> Check {
    let id = "embed.bitcompat";
    let Some(embed) = m.knowledge.as_ref().and_then(|k| k.embed_model.as_ref()) else {
        return Check::skip(id, Level::Feature, "no embed model advertised");
    };
    let url = host.url("/v1/embeddings");
    let body = json!({"model": embed.model_id, "input": "conformance probe input"});
    let call = || async {
        host.req(reqwest::Method::POST, &url)
            .json(&body)
            .send()
            .await
            .map_err(|e| e.to_string())?
            .json::<serde_json::Value>()
            .await
            .map_err(|e| e.to_string())
    };
    let (a, b) = match (call().await, call().await) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => return Check::fail(id, Level::Feature, e),
    };
    let vec_of = |v: &serde_json::Value| -> Option<Vec<f64>> {
        v.get("data")?
            .get(0)?
            .get("embedding")?
            .as_array()?
            .iter()
            .map(|x| x.as_f64())
            .collect()
    };
    let (Some(va), Some(vb)) = (vec_of(&a), vec_of(&b)) else {
        return Check::fail(id, Level::Feature, "response had no embedding vector");
    };
    if va.len() != embed.dimensions {
        return Check::fail(
            id,
            Level::Feature,
            format!(
                "vector len {} != advertised dimensions {}",
                va.len(),
                embed.dimensions
            ),
        );
    }
    if va != vb {
        return Check::fail(
            id,
            Level::Feature,
            "same input produced different vectors (not deterministic)",
        );
    }
    // Server-normalized embeddings must be unit length.
    if matches!(
        embed.normalization,
        oicp_types::NormalizationStrategy::Server
    ) {
        let norm: f64 = va.iter().map(|x| x * x).sum::<f64>().sqrt();
        if (norm - 1.0).abs() > 0.02 {
            return Check::fail(
                id,
                Level::Feature,
                format!("server-normalized but ‖v‖ = {norm:.4} (expected ≈1)"),
            );
        }
    }
    Check::pass(
        id,
        Level::Feature,
        format!("deterministic, {}-dim", va.len()),
    )
}

async fn check_knowledge_search(host: &Host, m: &ProviderManifest) -> Check {
    let id = "knowledge.search";
    let Some(k) = m.knowledge.as_ref() else {
        return Check::skip(id, Level::Feature, "no knowledge plane");
    };
    let url = host.url(&k.search_endpoint);
    let body = json!({"query": "conformance probe", "limit": 1});
    let resp = match host
        .req(reqwest::Method::POST, &url)
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Check::fail(id, Level::Feature, e.to_string()),
    };
    if !resp.status().is_success() {
        return Check::fail(id, Level::Feature, format!("HTTP {}", resp.status()));
    }
    let v: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return Check::fail(id, Level::Feature, e.to_string()),
    };
    // Shape: a `results` (or `chunks`) array, and any `corpora_searched` must be
    // a subset of the advertised corpora.
    let has_results = v.get("results").map(|x| x.is_array()).unwrap_or(false)
        || v.get("chunks").map(|x| x.is_array()).unwrap_or(false);
    if !has_results {
        return Check::fail(id, Level::Feature, format!("no results/chunks array: {v}"));
    }
    if let Some(searched) = v.get("corpora_searched").and_then(|x| x.as_array()) {
        let known: Vec<&str> = k.corpora.iter().map(|c| c.id.as_str()).collect();
        for c in searched {
            if let Some(cid) = c.as_str() {
                if !known.contains(&cid) {
                    return Check::fail(
                        id,
                        Level::Feature,
                        format!("searched corpus `{cid}` is not in the manifest"),
                    );
                }
            }
        }
    }
    Check::pass(
        id,
        Level::Feature,
        "search returned a well-shaped result set",
    )
}

async fn check_ingest_state_machine(host: &Host, m: &ProviderManifest, args: &Args) -> Check {
    let id = "ingest.state_machine";
    if !m.has_feature(features::INGEST_V1) {
        return Check::skip(id, Level::Feature, "ingest:v1 not advertised");
    }
    let Some(recipe) = args.fixture_recipe.as_ref() else {
        return Check::skip(id, Level::Feature, "no --fixture-recipe supplied");
    };
    let Some(ing) = m.knowledge.as_ref().and_then(|k| k.ingest.as_ref()) else {
        return Check::fail(
            id,
            Level::Feature,
            "ingest:v1 advertised but no knowledge.ingest",
        );
    };
    let install_url = host.url(&ing.install_endpoint);
    let body = json!({"corpus_id": recipe, "parameters": {}});
    let first = match host
        .req(reqwest::Method::POST, &install_url)
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return Check::fail(id, Level::Feature, e.to_string()),
    };
    if !first.status().is_success() {
        return Check::fail(
            id,
            Level::Feature,
            format!("install HTTP {}", first.status()),
        );
    }
    // Re-POST must be idempotent: spawned=false (already in flight / installed).
    let second: serde_json::Value = match host
        .req(reqwest::Method::POST, &install_url)
        .json(&body)
        .send()
        .await
        .and_then(|r| r.error_for_status())
    {
        Ok(r) => r.json().await.unwrap_or_else(|_| json!({})),
        Err(e) => return Check::fail(id, Level::Feature, format!("re-install: {e}")),
    };
    if second.get("spawned").and_then(|s| s.as_bool()) == Some(true) {
        return Check::fail(
            id,
            Level::Feature,
            "re-POST of an in-flight/installed corpus returned spawned=true (not idempotent)",
        );
    }
    // Poll progress until a terminal phase (bounded).
    let progress_url = host.url(&ing.progress_endpoint);
    for _ in 0..30 {
        let snap: serde_json::Value =
            match host.req(reqwest::Method::GET, &progress_url).send().await {
                Ok(r) => r.json().await.unwrap_or_else(|_| json!({})),
                Err(e) => return Check::fail(id, Level::Feature, e.to_string()),
            };
        if let Some(entry) = snap.get("progress").and_then(|p| p.get(recipe.as_str())) {
            match entry.get("phase").and_then(|p| p.as_str()) {
                Some("complete") => {
                    return Check::pass(id, Level::Feature, "install → progress → complete")
                }
                Some("failed") => {
                    return Check::fail(id, Level::Feature, format!("ingest failed: {entry}"))
                }
                _ => {}
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    Check::pass(
        id,
        Level::Feature,
        "install accepted + idempotent (did not reach terminal in 30s)",
    )
}

async fn check_auth_posture(host: &Host, m: &ProviderManifest, args: &Args) -> Check {
    let id = "auth.posture";
    if args::is_loopback(&args.host) {
        return Check::skip(
            id,
            Level::Feature,
            "loopback host is unauthenticated by posture",
        );
    }
    // A bogus bearer must be rejected on inference; the manifest stays reachable.
    let bogus = Host::new(&host.base, Some("definitely-not-a-valid-token".to_string()));
    let Some(model) = first_model(m) else {
        return Check::skip(id, Level::Feature, "no models to probe");
    };
    let body =
        json!({"model": model, "messages": [{"role": "user", "content": "hi"}], "max_tokens": 1});
    let url = bogus.url("/v1/chat/completions");
    match bogus
        .req(reqwest::Method::POST, &url)
        .json(&body)
        .send()
        .await
    {
        Ok(r) if r.status() == 401 || r.status() == 403 => Check::pass(
            id,
            Level::Feature,
            format!("bogus bearer rejected ({})", r.status()),
        ),
        Ok(r) => Check::fail(
            id,
            Level::Feature,
            format!(
                "bogus bearer NOT rejected on inference (HTTP {})",
                r.status()
            ),
        ),
        Err(e) => Check::fail(id, Level::Feature, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oicp_types::{
        CapabilityClaim, CapabilityHint, IngestEndpoints, KnowledgeManifest, LatencyClass,
        ModelStatus, ProviderModel,
    };

    fn model(id: &str, ctx: u32, claims: Vec<CapabilityClaim>) -> ProviderModel {
        ProviderModel {
            id: id.into(),
            base_model: None,
            quantization: None,
            context_tokens: ctx,
            status: ModelStatus {
                available: true,
                loaded: true,
                estimated_tokens_per_sec: None,
                estimated_ttft_ms: None,
                estimated_load_time_sec: None,
            },
            size_gb: None,
            claims,
            fingerprint: None,
        }
    }

    fn claim(affinity: f32, max_context: u32) -> CapabilityClaim {
        CapabilityClaim {
            hint: CapabilityHint::general(),
            latency_class: LatencyClass::Normal,
            max_context,
            max_output: 512,
            affinity,
        }
    }

    #[test]
    fn clean_manifest_has_no_invariant_failures() {
        let m = ProviderManifest::new(vec![model("m", 8192, vec![claim(0.9, 8000)])]);
        assert!(manifest_invariant_failures(&m).is_empty());
    }

    #[test]
    fn affinity_out_of_range_is_flagged() {
        let m = ProviderManifest::new(vec![model("m", 8192, vec![claim(1.5, 8000)])]);
        assert_eq!(manifest_invariant_failures(&m).len(), 1);
    }

    #[test]
    fn claim_context_exceeding_model_is_flagged() {
        let m = ProviderManifest::new(vec![model("m", 8192, vec![claim(0.5, 9000)])]);
        let f = manifest_invariant_failures(&m);
        assert_eq!(f.len(), 1);
        assert!(f[0].contains("exceeds context_tokens"));
    }

    #[test]
    fn unknown_feature_is_flagged_but_x_prefix_is_ok() {
        let mut m = ProviderManifest::new(vec![model("m", 8192, vec![])]);
        m.features = vec!["x:custom-thing".into(), "not-a-real-feature".into()];
        let f = feature_failures(&m);
        assert_eq!(f.len(), 1);
        assert!(f[0].contains("not-a-real-feature"));
    }

    #[test]
    fn ingest_feature_must_match_ingest_section() {
        // ingest:v1 advertised but no knowledge.ingest → failure.
        let mut m = ProviderManifest::new(vec![model("m", 8192, vec![])]);
        m.features = vec![features::INGEST_V1.into()];
        assert_eq!(feature_failures(&m).len(), 1);

        // Now add the section → consistent.
        m.knowledge = Some(KnowledgeManifest {
            corpora: vec![],
            search_endpoint: "/v1/knowledge/search".into(),
            embed_model: None,
            ingest: Some(IngestEndpoints {
                install_endpoint: "/oicp/v1/corpus/install".into(),
                progress_endpoint: "/oicp/v1/corpus/progress".into(),
                test_endpoint: None,
            }),
        });
        assert!(feature_failures(&m).is_empty());
    }
}
