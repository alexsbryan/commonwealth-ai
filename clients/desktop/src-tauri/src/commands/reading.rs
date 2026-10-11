// SPDX-License-Identifier: AGPL-3.0-or-later
//! Auto-split from the former monolithic `commands.rs` (PR5). Tauri
//! command handlers grouped by concern; re-exported through
//! `commands/mod.rs` so `commands::<name>` paths in `main.rs`'s
//! `generate_handler!` stay valid.
use std::sync::Arc;

use tauri::State;

use crate::state::AppState;

// ─── Reading Surface ─────────────────────────────────────────────────────────
//
// Backs the desktop's glass-box reading UI. Frontend calls
// `read_get_chunk_neighbors(corpus, chunkId, radius)` after the user clicks a
// citation; the answer comes from the daemon's `reading_http` routes in BOTH
// boot modes (sv-surface D1). "Local" does not mean "a second reading
// implementation" — it means the daemon is IN-PROCESS: `state.rs` commissions
// it with THIS process's `corpus_engine` and `state_store` (`ServingCore`) and
// refuses to finish bootstrap unless it binds `client_port`, so the same four
// routes answer over loopback that an attached daemon answers over the wire.
//
// Parity is by construction and needs no mirror to keep: these commands return
// the daemon's response bytes VERBATIM — `serde_json::Value`, parsed by nobody
// on the way through — so there is no second serializer that could disagree
// with `reading_http`'s. The hand-kept `*Dto` mirrors this file used to carry
// (44 refs, "byte-compatible by comment") had already drifted when they were
// deleted: they lacked the wire types' `skip_serializing_if` attributes, so an
// in-process chunk emitted `"title": null` where the daemon's JSON omitted the
// key — the exact break the mirrors existed to prevent. The census
// (tests/reading_wire_types_census.rs) keeps both a second spelling and a
// second path out.

/// GET one of the daemon's loopback `reading_http` routes
/// (`/internal/corpus/...`, merged into the client router on `client_port`)
/// and return its body verbatim. A 404 — chunk/atom/corpus absent, or an older
/// daemon without the route — maps to `Ok(None)`, which is the shape the
/// frontend already reads.
async fn daemon_reading_get(
    base_url: &str,
    path: &str,
) -> Result<Option<serde_json::Value>, String> {
    let url = format!("{base_url}{path}");
    let resp = reqwest::Client::new()
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("daemon reading GET {url}: {e}"))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("daemon reading {url} → {status}: {body}"));
    }
    resp.json::<serde_json::Value>()
        .await
        .map(Some)
        .map_err(|e| format!("daemon reading decode {url}: {e}"))
}

#[tauri::command]
pub async fn read_get_chunk(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    chunk_id: u64,
) -> Result<Option<serde_json::Value>, String> {
    daemon_reading_get(
        &state.client_base_url(),
        &format!("/internal/corpus/{corpus_id}/chunks/{chunk_id}"),
    )
    .await
}

#[tauri::command]
pub async fn read_get_chunk_neighbors(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    chunk_id: u64,
    radius: Option<usize>,
) -> Result<Option<serde_json::Value>, String> {
    let radius = radius.unwrap_or(1).min(5);
    daemon_reading_get(
        &state.client_base_url(),
        &format!("/internal/corpus/{corpus_id}/chunks/{chunk_id}/neighbors?radius={radius}"),
    )
    .await
}

// ─── Atom Panel ──────────────────────────────────────────────────────────────
//
// Two routes back the desktop's atom panel: `read_get_atom_card` returns the
// atom card (canonical_name, description, salience, one-hop relations,
// cross-corpus bridges) and `read_get_atom_elsewhere` returns the section list
// + cross-corpus links so the user can jump to other places the atom appears.
// The section→chunk projection happens in the route
// (`reading_http`'s `resolve_sections_to_chunks`), so the desktop receives
// ready-to-click chunk_ids in both boot modes.

#[tauri::command]
pub async fn read_get_atom_card(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    atom_id: String,
) -> Result<Option<serde_json::Value>, String> {
    daemon_reading_get(
        &state.client_base_url(),
        &format!("/internal/corpus/{corpus_id}/atoms/{atom_id}"),
    )
    .await
}

#[tauri::command]
pub async fn read_get_atom_elsewhere(
    state: State<'_, Arc<AppState>>,
    corpus_id: String,
    atom_id: String,
) -> Result<Option<serde_json::Value>, String> {
    daemon_reading_get(
        &state.client_base_url(),
        &format!("/internal/corpus/{corpus_id}/atoms/{atom_id}/elsewhere"),
    )
    .await
}

// ─── Stored texts ────────────────────────────────────────────────────────────
//
// A verified quote in an answer opens at its address in a stored text
// (ADDRESSED_TEXT §5.2): the daemon's published `GET {text_endpoint}/{sha}`
// (OICP v0.5 §2.2), its `TextSlice` returned verbatim like every read above.
// Two differences from the chunk reads, both from the spec: the endpoint is
// the one the daemon's manifest advertises under `evidence:text`, and a
// refusal is not "absent" — a 404 names why (`text not held`, `texts not
// stored`, `text not stored`) and that reason reaches the reader by name.

/// The stored text named `text_sha256`, cut to `[start, end)` (code points)
/// with `context` either side, as the daemon at `base_url` serves it.
async fn daemon_text_slice(
    base_url: &str,
    text_sha256: &str,
    start: Option<u64>,
    end: Option<u64>,
    context: Option<u32>,
    corpus: Option<&str>,
) -> Result<serde_json::Value, String> {
    let client = reqwest::Client::new();
    let manifest_url = format!("{base_url}/oicp/v1/capabilities");
    let manifest: serde_json::Value = client
        .get(&manifest_url)
        .send()
        .await
        .map_err(|e| format!("daemon manifest GET {manifest_url}: {e}"))?
        .json()
        .await
        .map_err(|e| format!("daemon manifest decode {manifest_url}: {e}"))?;
    let advertised = manifest["features"]
        .as_array()
        .is_some_and(|f| f.iter().any(|x| x == "evidence:text"));
    let endpoint = manifest["knowledge"]["evidence"]["text_endpoint"].as_str();
    let Some(endpoint) = endpoint.filter(|_| advertised) else {
        tracing::info!(%base_url, "reading: the daemon advertises no evidence:text; a quote cannot be opened at its address");
        return Err(
            "this daemon does not serve stored texts (no evidence:text in its manifest)".into(),
        );
    };
    let mut query: Vec<(&str, String)> = Vec::new();
    if let Some(v) = start {
        query.push(("start", v.to_string()));
    }
    if let Some(v) = end {
        query.push(("end", v.to_string()));
    }
    if let Some(v) = context {
        query.push(("context", v.to_string()));
    }
    if let Some(v) = corpus {
        query.push(("corpus", v.to_string()));
    }
    let url = format!("{base_url}{endpoint}/{text_sha256}");
    let resp = client
        .get(&url)
        .query(&query)
        .send()
        .await
        .map_err(|e| format!("daemon text GET {url}: {e}"))?;
    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("daemon text decode {url} ({status}): {e}"))?;
    if !status.is_success() {
        let Some(reason) = body["error"].as_str() else {
            tracing::debug!(%url, %status, "reading: the text read was refused with no reason");
            return Err(format!("daemon text {url} → {status}, no reason named"));
        };
        tracing::debug!(%url, %status, reason, "reading: the text read was refused");
        return Err(reason.to_string());
    }
    tracing::debug!(%url, "reading: a stored text read at its address");
    Ok(body)
}

#[tauri::command]
pub async fn read_text_slice(
    state: State<'_, Arc<AppState>>,
    text_sha256: String,
    start: Option<u64>,
    end: Option<u64>,
    context: Option<u32>,
    corpus: Option<String>,
) -> Result<serde_json::Value, String> {
    daemon_text_slice(
        &state.client_base_url(),
        &text_sha256,
        start,
        end,
        context,
        corpus.as_deref(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::daemon_text_slice;
    use axum::extract::{Path, Query};
    use axum::http::StatusCode;
    use axum::routing::get;
    use axum::{Json, Router};
    use serde_json::{json, Value};
    use std::collections::HashMap;

    const SHA: &str = "fc119db33120843e339028e353a38bcb6eabf74833e5714d5f722b502fb015a2";

    /// A v0.5 host fake: the manifest names its text endpoint, the endpoint
    /// answers the spec's `TextSlice` for the one text it holds and a named
    /// refusal for any other, and echoes the range it was asked for.
    async fn fake_host(advertise: bool) -> String {
        let features = if advertise {
            json!(["evidence:text"])
        } else {
            json!([])
        };
        let manifest = json!({
            "oicp_version": "0.4.0",
            "models": [],
            "features": features,
            "knowledge": {
                "corpora": [],
                "search_endpoint": "/v1/knowledge/search",
                "evidence": { "text_endpoint": "/evidence/text" }
            }
        });
        let app = Router::new()
            .route(
                "/oicp/v1/capabilities",
                get(move || {
                    let m = manifest.clone();
                    async move { Json(m) }
                }),
            )
            .route(
                "/evidence/text/{sha}",
                get(
                    |Path(sha): Path<String>, Query(q): Query<HashMap<String, String>>| async move {
                        if sha == "1".repeat(64) {
                            return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({})));
                        }
                        if sha != SHA {
                            return (StatusCode::NOT_FOUND, Json(json!({"error": "text not held"})));
                        }
                        let n = |k: &str| q.get(k).and_then(|v| v.parse::<u64>().ok());
                        (
                            StatusCode::OK,
                            Json(json!({
                                "document": {
                                    "text_sha256": SHA,
                                    "extractor": "plain_text@test",
                                    "source": {"id": "okafor2019", "sha256": SHA}
                                },
                                "start": n("start"),
                                "end": n("end"),
                                "text": "persuaded",
                                "before": format!("context={}", q.get("context").cloned().unwrap_or_default()),
                                "after": format!("corpus={}", q.get("corpus").cloned().unwrap_or_default())
                            })),
                        )
                    },
                ),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn a_quote_is_read_at_its_address_through_the_advertised_endpoint() {
        let base = fake_host(true).await;
        let slice: Value =
            daemon_text_slice(&base, SHA, Some(122), Some(131), Some(32), Some("fixture"))
                .await
                .expect("the held text reads");
        assert_eq!(slice["text"], "persuaded");
        assert_eq!(
            (slice["start"].as_u64(), slice["end"].as_u64()),
            (Some(122), Some(131))
        );
        assert_eq!(slice["before"], "context=32");
        assert_eq!(slice["after"], "corpus=fixture");
        assert_eq!(slice["document"]["text_sha256"], SHA);
    }

    #[tokio::test]
    async fn a_refusal_reaches_the_reader_by_its_published_reason() {
        let base = fake_host(true).await;
        let err = daemon_text_slice(&base, &"0".repeat(64), None, None, None, None)
            .await
            .unwrap_err();
        assert_eq!(err, "text not held");
    }

    #[tokio::test]
    async fn a_refusal_without_a_reason_names_its_status() {
        let base = fake_host(true).await;
        let err = daemon_text_slice(&base, &"1".repeat(64), None, None, None, None)
            .await
            .unwrap_err();
        assert!(
            err.contains("503") && err.contains("no reason named"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn a_daemon_without_evidence_text_is_named_not_guessed() {
        let base = fake_host(false).await;
        let err = daemon_text_slice(&base, SHA, None, None, None, None)
            .await
            .unwrap_err();
        assert!(err.contains("no evidence:text"), "{err}");
    }
}
