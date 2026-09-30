// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one client of the daemon's `/internal/corpus/install`, and the one
//! `--param` value convention it is fed with. Two programs' CLIs call it:
//! ingest's `corpus install` and svrn's `workflow run <recipe-id>`, so it
//! lives in the CLI leaf both link (moved from sovereign-cli-llm
//! `corpus_cmd/inventory.rs` by pb-cli-llm-ingest-move; one client, two
//! callers).

use std::collections::BTreeMap;

use sovereign_contracts::setup_config::internal_daemon_base;

/// POST an install request to the running daemon's `/internal/corpus/install`
/// endpoint and report the outcome. The daemon owns the actual ingest task; this
/// is the thin, fire-and-forget client. Shared by `corpus install` and a
/// `workflow run <recipe-id>` dispatch so both delegate to the *same* install path
/// (surface-unify, don't deep-collapse — one client, two callers).
pub async fn submit_install_request(id: &str, params: BTreeMap<String, serde_json::Value>) -> i32 {
    let url = format!("{}/internal/corpus/install", internal_daemon_base());
    let body = serde_json::json!({
        "corpus_id": id,
        "parameters": params,
    });
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to build HTTP client: {e}");
            return 1;
        }
    };
    match client.post(&url).json(&body).send().await {
        Ok(resp) if resp.status().is_success() => {
            // The endpoint is fire-and-forget, but its 200 body carries a
            // `spawned` flag: true = a new ingest task started, false = an
            // ingest for this corpus was already in flight (idempotent
            // no-op). Distinguish them so an already-running corpus doesn't
            // read as a fresh "Install requested" — the daemon returns 4xx
            // (handled below) for genuine failures, so a 200 here is never
            // a silent error, only "started" vs "already going".
            let body_text = resp.text().await.unwrap_or_default();
            let spawned = serde_json::from_str::<serde_json::Value>(&body_text)
                .ok()
                .and_then(|v| v.get("spawned").and_then(|s| s.as_bool()));
            match spawned {
                Some(false) => {
                    println!("Already in progress: {id} (ingest already running — not re-spawned)");
                }
                _ => {
                    // spawned:true, or a body we couldn't parse — treat as
                    // a fresh request and still show the raw body for
                    // observability.
                    println!("Install requested: {id}");
                    if !body_text.is_empty() {
                        println!("{body_text}");
                    }
                }
            }
            println!("Watch progress: svrn corpus status");
            0
        }
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            eprintln!("Daemon rejected install ({status}): {body}");
            1
        }
        Err(e) => {
            eprintln!(
                "Failed to contact daemon at {url}: {e}\n\n\
                 Is `svrn daemon` running? Try: svrn daemon status"
            );
            1
        }
    }
}

/// Shape a single `--param`/`--params` *value* into JSON: a comma-bearing value
/// becomes an array of trimmed, non-empty strings; otherwise a single trimmed
/// string. The daemon coerces strings → ints/dates per the recipe's declared
/// `ParameterKind`, so the CLI only shapes the JSON. Shared by `corpus install`'s
/// `--param` parser and the `workflow run <recipe-id>` param conversion so the one
/// `--param` convention behaves identically across both surfaces.
pub fn param_json_value(value: &str) -> serde_json::Value {
    if value.contains(',') {
        let items: Vec<serde_json::Value> = value
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(serde_json::Value::String)
            .collect();
        serde_json::Value::Array(items)
    } else {
        serde_json::Value::String(value.trim().to_string())
    }
}
