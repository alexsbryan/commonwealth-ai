// SPDX-License-Identifier: AGPL-3.0-or-later
//! Host detection — what the endpoint at `--base-url` can do, DISCOVERED.
//!
//! `docs/CODE_TOOLING_BOUNDARY.md` §5.3: capability is detected, never
//! configured (rule 1); every degradation is printed (rule 2); a host that
//! only works against our own daemon is a bug (rule 3). The baseline any
//! frontend must satisfy is `POST /v1/embeddings` + `GET /v1/models`; an OICP
//! host additionally answers `GET /oicp/v1/capabilities`, and that is the only
//! thing the probe treats differently — it says so.

use anyhow::{bail, Context, Result};
use serde_json::Value;

/// What was learned about the endpoint before serving anything.
#[derive(Debug, Clone)]
pub struct HostProfile {
    pub base_url: String,
    pub embeddings_url: String,
    pub embed_model: String,
    /// Dimensionality of one probe embedding — checked against every served
    /// index so a mismatch is announced at boot, not discovered as an empty
    /// vector leg on the first query (§18.4: validate the instrument first).
    pub embed_dims: usize,
    pub kind: HostKind,
}

#[derive(Debug, Clone)]
pub enum HostKind {
    /// `GET <root>/oicp/v1/capabilities` answered 200.
    Oicp { capabilities: Value },
    /// It did not (404, connection refused, non-JSON) — the OpenAI-compatible
    /// baseline, which is the case the whole binary exists for.
    Baseline { reason: String },
}

impl HostKind {
    pub fn label(&self) -> &'static str {
        match self {
            HostKind::Oicp { .. } => "oicp",
            HostKind::Baseline { .. } => "baseline (OpenAI-compatible)",
        }
    }
}

pub async fn probe(base_url: &str, embed_model: Option<String>) -> Result<HostProfile> {
    let base = base_url.trim_end_matches('/').to_string();
    let root = base.strip_suffix("/v1").unwrap_or(&base).to_string();
    let client = reqwest::Client::new();

    // 1. Capability — detected, never configured.
    let cap_url = format!("{root}/oicp/v1/capabilities");
    let kind = match client.get(&cap_url).send().await {
        Ok(r) if r.status().is_success() => match r.json::<Value>().await {
            Ok(capabilities) => HostKind::Oicp { capabilities },
            Err(e) => HostKind::Baseline {
                reason: format!("{cap_url} answered 200 but not JSON ({e})"),
            },
        },
        Ok(r) => HostKind::Baseline {
            reason: format!("{cap_url} returned {}", r.status()),
        },
        Err(e) => HostKind::Baseline {
            reason: format!("{cap_url}: {e}"),
        },
    };
    match &kind {
        HostKind::Oicp { capabilities } => eprintln!(
            "corpus-mcp: host {base}: OICP capabilities detected ({} top-level keys)",
            capabilities.as_object().map(|o| o.len()).unwrap_or(0)
        ),
        HostKind::Baseline { reason } => {
            eprintln!("corpus-mcp: host {base}: baseline OpenAI-compatible path — {reason}")
        }
    }

    // 2. The embedding model id — from the flag, else from what the host
    //    says it serves. Absence is refused, never defaulted (§18.3).
    let models_url = format!("{base}/models");
    let embed_model = match embed_model {
        Some(m) => m,
        None => {
            let listed = client
                .get(&models_url)
                .send()
                .await
                .with_context(|| format!("GET {models_url}"))?
                .json::<Value>()
                .await
                .with_context(|| format!("GET {models_url}: not JSON"))?;
            let first = listed["data"]
                .as_array()
                .and_then(|d| d.first())
                .and_then(|m| m["id"].as_str())
                .map(str::to_string);
            match first {
                Some(id) => id,
                None => bail!(
                    "{models_url} lists no models, so there is no embedding model id to \
                     send; pass --embed-model <id>"
                ),
            }
        }
    };

    // 3. One probe embedding: proves the endpoint works and learns its width.
    let embeddings_url = format!("{base}/embeddings");
    let resp = client
        .post(&embeddings_url)
        .json(&serde_json::json!({ "input": "corpus-mcp probe", "model": embed_model }))
        .send()
        .await
        .with_context(|| format!("POST {embeddings_url}"))?;
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .with_context(|| format!("POST {embeddings_url}: {status}, body not JSON"))?;
    if !status.is_success() {
        bail!("POST {embeddings_url} returned {status}: {body}");
    }
    let embed_dims = body["data"][0]["embedding"]
        .as_array()
        .map(|a| a.len())
        .with_context(|| format!("POST {embeddings_url}: no data[0].embedding in {body}"))?;
    if embed_dims == 0 {
        bail!("POST {embeddings_url} returned an empty embedding for model `{embed_model}`");
    }
    eprintln!(
        "corpus-mcp: embeddings via {embeddings_url}, model `{embed_model}`, {embed_dims}-d ({})",
        kind.label()
    );

    Ok(HostProfile {
        base_url: base,
        embeddings_url,
        embed_model,
        embed_dims,
        kind,
    })
}
