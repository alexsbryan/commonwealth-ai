// SPDX-License-Identifier: AGPL-3.0-or-later
//! Canonical-index pull client.
//!
//! Phase 6 of the resilience track. Pairs with the
//! `GET /internal/corpus/canonical/{id}` endpoint in
//! `commonwealth-api`. Given a peer URL and a corpus id, this module
//! streams the peer's tar+zstd canonical, unpacks into a temp dir,
//! validates the content fingerprint against the
//! `X-Canonical-Fingerprint` response header, and atomically
//! renames the temp dir into `<index_dir>/<corpus_id>/`.
//!
//! ## Atomicity
//!
//! Three-stage rename pattern:
//!
//! 1. Untar into `<index_dir>/<corpus_id>.pulling.<rand>/`.
//! 2. Open the freshly-unpacked dir, recompute its fingerprint,
//!    compare to the header. Mismatch → wipe + error.
//! 3. Match → `std::fs::rename(temp, canonical)`. POSIX rename is
//!    atomic for same-filesystem dirs. The receiver removed any
//!    prior canonical (via `corpus remove --canonical-only`)
//!    before kicking off the pull, so the rename target is
//!    expected to be absent. If something raced and the canonical
//!    appeared in the meantime, we leave the temp in place and
//!    surface a clear error rather than overwriting.
//!
//! ## Auth
//!
//! For now this uses bare HTTP against the peer's `:9742` (the
//! internal mesh port). The same endpoint shape that
//! `/internal/knowledge/search` and friends use; mesh peer
//! authentication is the same. The receiver doesn't gate on auth
//! itself — the loopback guard on the responder is the load-
//! bearing check.
//!
//! ## Memory
//!
//! Streaming the response body through `reqwest::Response::bytes_stream()`
//! and feeding it to a `tokio_util::io::StreamReader` keeps the
//! transfer bounded by the network buffer (default 8 KiB) plus
//! whatever zstd's frame state needs. A 12 GB Wikipedia canonical
//! flows through without sitting in RAM.

use std::path::{Path, PathBuf};

use corpus_engine::canonical_sync;
use corpus_engine::index::CorpusIndex;

/// Result of a successful pull. Lets callers log throughput,
/// confirm the fingerprint match, and decide whether to emit a
/// "pulled" gossip event.
#[derive(Debug, Clone)]
pub struct CanonicalPullReport {
    pub corpus_id: String,
    /// The URL we ultimately succeeded on (some peers publish
    /// multiple addresses — LAN, Tailscale, IPv6 — and we try each
    /// in turn). Useful for log analysis when one address path is
    /// systematically broken.
    pub peer_url: String,
    pub fingerprint: String,
    pub bytes_uncompressed: u64,
    pub canonical_path: PathBuf,
}

/// Errors specific to the pull path. The transport `String` form
/// keeps callers simple — the puller is best-effort and a string is
/// what they log.
#[derive(Debug, thiserror::Error)]
pub enum PullError {
    #[error("peer responded {status}: {body}")]
    PeerHttpError { status: u16, body: String },

    #[error("peer did not advertise canonical_fingerprint header")]
    NoFingerprintHeader,

    #[error("fingerprint mismatch: expected {expected}, got {actual}")]
    FingerprintMismatch { expected: String, actual: String },

    #[error(
        "destination already exists: {0} (run `sovereign corpus remove \
        --canonical-only` first)"
    )]
    DestinationExists(PathBuf),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("transport: {0}")]
    Transport(String),

    #[error("engine: {0}")]
    Engine(String),
}

/// Pull `corpus_id` from one of `peer_urls` and place the canonical
/// at `<index_dir>/<corpus_id>/`. Each URL is the BASE — e.g.
/// `http://100.64.0.2:9742`. Tries each URL in turn, falling
/// through on connection failure (`reqwest::Error::is_connect()` or
/// timeout); HTTP-level errors (404, 403) abort early without
/// trying the next URL since they reflect server policy, not
/// reachability. Returns the first URL that successfully opens a
/// response.
///
/// **Why a list, not a single URL.** Peers gossip multiple
/// addresses (LAN IP, Tailscale CGNAT, IPv6 ULA). The "right" one
/// depends on the network topology between the puller and the
/// pusher; a fresh-boot node on a VPN can't reach the LAN address,
/// while a same-LAN node may not have Tailscale. Picking just one
/// up-front means systematic pull failures whenever the topology
/// changes. Trying all in turn is cheap (each attempt is bounded
/// by `connect_timeout`) and converges to the working address.
///
/// The `expected_fingerprint` argument is the value the caller
/// learned from gossip; if `None`, we accept whatever the peer
/// advertises in the response header (manual `corpus pull` flow
/// where the user trusts the source). If `Some`, it must match
/// both the header and the recomputed fingerprint after unpack.
///
/// Returns a `CanonicalPullReport` on success. On failure, the
/// temp dir is removed and no canonical is created.
pub async fn pull_canonical_from_peer(
    peer_urls: &[String],
    corpus_id: &str,
    index_dir: &Path,
    expected_fingerprint: Option<&str>,
) -> Result<CanonicalPullReport, PullError> {
    if peer_urls.is_empty() {
        return Err(PullError::Transport(
            "no peer addresses supplied".to_string(),
        ));
    }
    let canonical_path = index_dir.join(corpus_id);
    if canonical_path.exists() {
        return Err(PullError::DestinationExists(canonical_path));
    }
    std::fs::create_dir_all(index_dir)?;

    // Resolve a unique temp dir under the same parent as the final
    // canonical. Same-filesystem rename is what makes the final
    // step atomic. We can't use `tempfile::tempdir_in` because we
    // need the path to be deterministic across the streaming task
    // and the rename — instead, mint an explicit name and clean up
    // ourselves on error.
    let suffix: u128 = rand::random();
    let temp_path = index_dir.join(format!("{corpus_id}.pulling.{suffix:032x}"));
    if temp_path.exists() {
        // Astronomically unlikely — but if it did happen, refuse
        // to overwrite somebody's in-flight pull.
        return Err(PullError::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("pull temp already exists: {}", temp_path.display()),
        )));
    }

    let client = reqwest::Client::builder()
        // No timeout on the body stream — Wikipedia canonical pulls
        // can take many minutes on a slow link. The connect timeout
        // is what catches dead peers fast.
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| PullError::Transport(format!("client build: {e}")))?;

    // Try each candidate peer URL in turn until one yields a
    // successful HTTP response. Connection-level failures (refused,
    // timeout, no route) advance to the next URL. HTTP-level errors
    // (404, 403, 500) are server-side policy and abort immediately
    // — trying another address won't change the answer.
    let mut chosen_url: Option<String> = None;
    let mut chosen_resp: Option<reqwest::Response> = None;
    let mut last_transport_error: Option<String> = None;
    for base in peer_urls {
        let url = format!(
            "{}/internal/corpus/canonical/{}",
            base.trim_end_matches('/'),
            corpus_id
        );
        tracing::info!(
            corpus_id,
            peer = base,
            url = %url,
            temp = %temp_path.display(),
            "canonical_pull: attempting"
        );
        match client.get(&url).send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    chosen_url = Some(base.clone());
                    chosen_resp = Some(resp);
                    break;
                }
                // HTTP-level error — surface to caller without
                // trying the next URL. Same corpus, same server
                // policy.
                let body = resp
                    .text()
                    .await
                    .unwrap_or_else(|_| "<unreadable>".to_string());
                return Err(PullError::PeerHttpError {
                    status: status.as_u16(),
                    body,
                });
            }
            Err(e) => {
                // Connection-level failure: refused, timeout, no
                // route, DNS fail. Move on to the next URL.
                tracing::info!(
                    corpus_id,
                    peer = base,
                    error = %e,
                    "canonical_pull: address unreachable, trying next"
                );
                last_transport_error = Some(format!("{base}: {e}"));
                continue;
            }
        }
    }

    let (peer_url, resp) = match (chosen_url, chosen_resp) {
        (Some(u), Some(r)) => (u, r),
        _ => {
            return Err(PullError::Transport(format!(
                "all {} peer addresses unreachable; last: {}",
                peer_urls.len(),
                last_transport_error.unwrap_or_else(|| "n/a".to_string())
            )));
        }
    };

    tracing::info!(
        corpus_id,
        peer = %peer_url,
        "canonical_pull: connection established, streaming body"
    );

    let advertised_fp = resp
        .headers()
        .get("x-canonical-fingerprint")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let advertised_fp = match advertised_fp {
        Some(s) if !s.is_empty() => s,
        _ => return Err(PullError::NoFingerprintHeader),
    };
    if let Some(expected) = expected_fingerprint {
        if expected != advertised_fp {
            return Err(PullError::FingerprintMismatch {
                expected: expected.to_string(),
                actual: advertised_fp,
            });
        }
    }

    // Stream body → SyncIoBridge → unpack on a blocking task.
    let stream = resp.bytes_stream().map_err(std::io::Error::other);
    use futures::TryStreamExt;
    let async_reader = tokio_util::io::StreamReader::new(stream);
    let sync_reader = tokio_util::io::SyncIoBridge::new(async_reader);

    let temp_for_unpack = temp_path.clone();
    let unpack_result = tokio::task::spawn_blocking(move || {
        canonical_sync::unpack_canonical(sync_reader, &temp_for_unpack)
    })
    .await
    .map_err(|e| PullError::Engine(format!("unpack join: {e}")))?
    .map_err(|e| PullError::Engine(format!("unpack: {e}")))?;
    let bytes_uncompressed = unpack_result;

    // Recompute the fingerprint over the unpacked canonical and
    // verify the peer didn't lie / corruption didn't occur.
    let recomputed = match CorpusIndex::open(&temp_path).await {
        Ok(idx) => match idx.compute_canonical_fingerprint().await {
            Ok(fp) => fp,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&temp_path);
                return Err(PullError::Engine(format!("recompute: {e}")));
            }
        },
        Err(e) => {
            let _ = std::fs::remove_dir_all(&temp_path);
            return Err(PullError::Engine(format!("open after unpack: {e}")));
        }
    };
    if recomputed != advertised_fp {
        let _ = std::fs::remove_dir_all(&temp_path);
        return Err(PullError::FingerprintMismatch {
            expected: advertised_fp,
            actual: recomputed,
        });
    }

    // Final atomic rename. If a concurrent process placed a
    // canonical here in the meantime, refuse rather than overwrite.
    if canonical_path.exists() {
        let _ = std::fs::remove_dir_all(&temp_path);
        return Err(PullError::DestinationExists(canonical_path));
    }
    std::fs::rename(&temp_path, &canonical_path).map_err(|e| {
        // Best-effort cleanup; on a same-filesystem rename this
        // generally only fails if the destination got created
        // under us. We can't easily recover the temp at this point
        // but logging it gives the operator a hint.
        tracing::warn!(
            corpus_id,
            temp = %temp_path.display(),
            error = %e,
            "canonical_pull: final rename failed; temp left in place"
        );
        PullError::Io(e)
    })?;

    Ok(CanonicalPullReport {
        corpus_id: corpus_id.to_string(),
        peer_url,
        fingerprint: advertised_fp,
        bytes_uncompressed,
        canonical_path,
    })
}
