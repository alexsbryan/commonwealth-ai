// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn corpus install <recipe.toml> --inline`: the recipe, documents and
//! all, sent in the daemon's published install (OICP v0.5 §4), as a client
//! that shares no disk with the daemon sends it. Sized before it is sent
//! against the largest install the daemon states, `max_install_bytes` (v0.6
//! §2.12), and refused by name past it: the platform's own second user of
//! §2.12. With `--wait` it polls the published progress, not this host's
//! index directory, because the daemon's disk is not assumed to be this one.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use sovereign_contracts::oicp::ingest::reasons::INSTALL_TOO_LARGE;
use sovereign_contracts::oicp::{
    features, CorpusInstallRequest, CorpusInstallResponse, CorpusProgressResponse,
    IngestDocumentState, IngestEndpoints, IngestPhase, ProviderManifest,
};

/// Install the recipe at `path` inline on the daemon at `base`. The exit code.
pub(super) async fn install_inline(
    base: &str,
    path: &Path,
    parameters: BTreeMap<String, serde_json::Value>,
    wait_secs: Option<u64>,
) -> i32 {
    let recipe_toml = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: reading {}: {e}", path.display());
            return 1;
        }
    };
    // The recipe's own id, from the one load boundary: a recipe that does not
    // load is refused here rather than after its documents cross the wire.
    let corpus_id = match corpus_engine::recipe::Recipe::from_toml(&recipe_toml) {
        Ok(r) => r.corpus.id,
        Err(e) => {
            eprintln!("error: {} does not load as a recipe: {e}", path.display());
            return 1;
        }
    };
    let http = reqwest::Client::new();
    let ingest = match manifest(&http, base)
        .await
        .and_then(|m| install_surface(&m))
    {
        Ok(i) => i,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let req = CorpusInstallRequest {
        corpus_id: corpus_id.clone(),
        parameters,
        recipe_toml: Some(recipe_toml),
        allow_lookups: false,
    };
    let body = match serde_json::to_vec(&req) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: the install request does not encode: {e}");
            return 1;
        }
    };
    if let Err(why) = fits(body.len(), ingest.max_install_bytes) {
        tracing::info!(corpus = %corpus_id, bytes = body.len(), limit = ?ingest.max_install_bytes, "corpus install --inline: refused before sending");
        eprintln!("error: {why}");
        return 1;
    }
    tracing::debug!(corpus = %corpus_id, bytes = body.len(), limit = ?ingest.max_install_bytes, "corpus install --inline: sending");
    let started = Instant::now();
    let reply = match send(&http, &join(base, &ingest.install_endpoint), body).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    println!(
        "Install {}: {corpus_id} (recipe {})",
        if reply.spawned {
            "requested"
        } else {
            "already held from this recipe"
        },
        reply.recipe_sha256.as_deref().unwrap_or("unnamed")
    );
    match wait_secs {
        None => {
            println!("Watch progress: {}", join(base, &ingest.progress_endpoint));
            0
        }
        Some(budget) => {
            let progress = join(base, &ingest.progress_endpoint);
            wait_terminal(&http, &progress, &corpus_id, budget, started).await
        }
    }
}

/// The daemon's manifest.
async fn manifest(http: &reqwest::Client, base: &str) -> Result<ProviderManifest, String> {
    http.get(join(base, "/oicp/v1/capabilities"))
        .send()
        .await
        .map_err(|e| format!("the daemon at {base} did not answer: {e}"))?
        .json()
        .await
        .map_err(|e| format!("the daemon's manifest does not decode: {e}"))
}

/// The install surface, when the daemon takes a recipe over the wire.
fn install_surface(m: &ProviderManifest) -> Result<IngestEndpoints, String> {
    let ingest = m.knowledge.as_ref().and_then(|k| k.ingest.clone());
    match ingest {
        Some(i) if m.has_feature(features::INGEST_RECIPE) => Ok(i),
        _ => Err(
            "the daemon does not advertise `ingest:recipe`, so it takes no recipe inline".into(),
        ),
    }
}

/// Whether a body of `len` bytes is within the stated limit; past it, the
/// refusal naming both numbers. A daemon that states none is sent to, and its
/// own `413`, if any, is reported (§2.12, "Unstated").
fn fits(len: usize, max_install_bytes: Option<u64>) -> Result<(), String> {
    match max_install_bytes {
        Some(max) if len as u64 > max => Err(format!(
            "the install is {len} bytes and the daemon reads at most {max} \
             (knowledge.ingest.max_install_bytes); not sent. Install a library \
             this large from files on a disk the daemon reads"
        )),
        _ => Ok(()),
    }
}

/// POST the body; a refusal is named by its status and the daemon's reason.
async fn send(
    http: &reqwest::Client,
    url: &str,
    body: Vec<u8>,
) -> Result<CorpusInstallResponse, String> {
    let resp = http
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("the daemon answered {status} and its body did not read: {e}"))?;
    if status == reqwest::StatusCode::PAYLOAD_TOO_LARGE {
        let named = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .filter(|v| v["error"] == INSTALL_TOO_LARGE);
        tracing::info!(
            named = named.is_some(),
            "corpus install --inline: refused 413"
        );
        return Err(match named {
            Some(v) => format!(
                "the daemon refused the install as too large: it reads at most {} bytes",
                v["max_install_bytes"]
            ),
            None => format!("the daemon refused the install as too large, naming no limit: {text}"),
        });
    }
    if !status.is_success() {
        tracing::info!(%status, "corpus install --inline: refused");
        return Err(format!("the daemon refused the install ({status}): {text}"));
    }
    serde_json::from_str(&text)
        .map_err(|e| format!("the install reply does not decode: {e}: {text}"))
}

/// Poll the published progress until `corpus_id` is complete or failed, or
/// the budget runs out; the time is counted from the request.
async fn wait_terminal(
    http: &reqwest::Client,
    progress: &str,
    corpus_id: &str,
    budget_secs: u64,
    started: Instant,
) -> i32 {
    let budget = Duration::from_secs(budget_secs);
    let mut last = "not listed".to_string();
    while started.elapsed() < budget {
        let snapshot = match http.get(progress).send().await {
            Ok(r) => r
                .json::<CorpusProgressResponse>()
                .await
                .map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        match snapshot {
            Ok(s) => match s.progress.get(corpus_id) {
                Some(entry) if entry.phase == IngestPhase::Complete => {
                    let failed: Vec<_> = entry
                        .documents
                        .iter()
                        .filter_map(|d| match &d.state {
                            IngestDocumentState::Failed { reason } => Some(format!(
                                "{}: {reason}",
                                d.source
                                    .as_deref()
                                    .unwrap_or("(a document the extractor did not name)")
                            )),
                            IngestDocumentState::Ready { .. } => None,
                        })
                        .collect();
                    let ready = entry.documents.len() - failed.len();
                    tracing::debug!(
                        corpus = corpus_id,
                        ready,
                        failed = failed.len(),
                        "corpus install --inline: complete"
                    );
                    println!(
                        "✓ installed: {corpus_id} after {:.1}s, {ready} document(s) ready, {} failed",
                        started.elapsed().as_secs_f64(),
                        failed.len()
                    );
                    for f in &failed {
                        println!("  failed: {f}");
                    }
                    return 0;
                }
                Some(entry) if entry.phase == IngestPhase::Failed => {
                    tracing::info!(corpus = corpus_id, "corpus install --inline: failed");
                    eprintln!(
                        "error: the install of {corpus_id} failed: {}",
                        entry.detail.as_deref().unwrap_or("no detail")
                    );
                    return 1;
                }
                Some(entry) => last = format!("{:?}", entry.phase).to_lowercase(),
                None => last = "not listed".into(),
            },
            Err(e) => last = format!("progress unread: {e}"),
        }
        tracing::debug!(corpus = corpus_id, state = %last, "corpus install --inline: polled");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    eprintln!("error: {corpus_id} is still {last} after {budget_secs}s; raise --wait=SECS");
    1
}

/// An origin-relative endpoint (`/oicp/…`) on `base`, or an absolute URL.
fn join(base: &str, path: &str) -> String {
    if path.starts_with("http://") || path.starts_with("https://") {
        return path.to_string();
    }
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

#[cfg(test)]
#[path = "inline_install_tests.rs"]
mod tests;
