//! Peer-to-peer model and corpus index transfer endpoints.
//!
//! `model_transfer` is reserved for future use (returns
//! `NOT_IMPLEMENTED` today). `index_serve` and `index_transfer` are
//! the pull and push halves of inter-node corpus index movement; they
//! are intentionally split rather than overloaded onto a single POST,
//! because doing so previously masked an empty-body bug in
//! `coordinate_merge` that silently degraded merges to "merge with
//! available shards."

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;

use crate::state::AppState;

/// POST /internal/model/transfer — peer-to-peer model file transfer.
pub async fn model_transfer(
    State(_state): State<AppState>,
    Json(_payload): Json<serde_json::Value>,
) -> StatusCode {
    StatusCode::NOT_IMPLEMENTED
}

/// GET /internal/index/serve — peer pulls our partition-of-self for a corpus.
///
/// Companion to `/internal/index/transfer`'s upload semantics: this
/// endpoint is the *server* side of the merge-leader's pull. The
/// caller specifies `X-Corpus-Id`; we tar the on-disk
/// `<index_dir>/<corpus_id>-partition-<self_node_id>/` and stream
/// the bytes back as the response body.
///
/// Why this is a distinct endpoint from `index_transfer`: that route
/// reads the request body (upload), this route writes the response
/// body (download). Trying to overload one POST endpoint with both
/// directions is what produced the original silent-failure bug — the
/// puller did POST-without-body, the server tried to read an empty
/// body as a tarball, and `coordinate_merge` quietly degraded to
/// "merge with available shards" (i.e. just our own partition).
///
/// Returns 404 when the partition dir doesn't exist (the peer
/// hasn't ingested this corpus, or never participated in a queue
/// for it). 503 when no corpus engine is wired (standalone mode).
pub async fn index_serve(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<axum::response::Response, (StatusCode, Json<serde_json::Value>)> {
    use axum::response::IntoResponse;

    let corpus_id = match headers.get("X-Corpus-Id").and_then(|v| v.to_str().ok()) {
        Some(id) => id.to_string(),
        None => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "missing X-Corpus-Id header"})),
            ));
        }
    };
    let engine = match &state.inner.corpus_engine {
        Some(e) => e.clone(),
        None => {
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "no corpus engine on this node"})),
            ));
        }
    };
    let self_node_id = *state.inner.self_node_id_swap.load_full().as_ref();
    let partition_path = engine
        .index_dir()
        .join(format!("{corpus_id}-partition-{self_node_id}"));
    if !partition_path.exists() {
        // 404: nothing to serve. The puller logs and continues with
        // whatever shards it does manage to fetch.
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!(
                    "no partition-of-self for corpus '{corpus_id}' on this node"
                ),
            })),
        ));
    }
    // Tar to a temp file, read into memory, return as octet-stream.
    // For typical corpus partitions (10s of MB to a few GB) full-buffer
    // is fine; if that ever changes we'd switch to a streaming body.
    let tar_path = engine
        .index_dir()
        .join(format!(".{corpus_id}-partition-{self_node_id}.serve.tar"));
    // Tar the *contents* of the partition dir (`.`), not the dir
    // itself — the puller extracts straight into its
    // `<corpus>-partition-<peer>/` dest dir, and a top-level
    // wrapper would produce a nested
    // `dest_dir/<corpus>-partition-<peer>/_corpus_meta.json`
    // that `merge_partitions` doesn't recognize as a shard.
    let tar_status = std::process::Command::new("tar")
        .args([
            "cf",
            &tar_path.to_string_lossy(),
            "-C",
            &partition_path.to_string_lossy(),
            ".",
        ])
        .status();
    match tar_status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            let _ = std::fs::remove_file(&tar_path);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("tar exited with {s}")})),
            ));
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tar_path);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("tar spawn: {e}")})),
            ));
        }
    }
    let tar_bytes = match std::fs::read(&tar_path) {
        Ok(b) => b,
        Err(e) => {
            let _ = std::fs::remove_file(&tar_path);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("read tar: {e}")})),
            ));
        }
    };
    let _ = std::fs::remove_file(&tar_path);
    let bytes_len = tar_bytes.len();
    tracing::info!(
        corpus = %corpus_id,
        bytes = bytes_len,
        "index_serve: tarred and returning partition-of-self"
    );
    Ok((
        StatusCode::OK,
        [
            ("content-type", "application/octet-stream"),
            ("x-bytes", &bytes_len.to_string()),
        ],
        tar_bytes,
    )
        .into_response())
}

/// POST /internal/index/transfer — peer-to-peer corpus index transfer.
///
/// Receives a tar stream of a corpus shard directory.  The body is the
/// raw tar bytes; the corpus ID is in the `X-Corpus-Id` request header.
///
/// Protocol:
/// 1. Stream body to `<index_dir>/.incoming/<corpus_id>.tar`
/// 2. Untar to `<index_dir>/.incoming/<corpus_id>/`
/// 3. Verify `_corpus_meta.json` exists in the unpacked directory
/// 4. Atomic rename from `.incoming/<corpus_id>` to `indexes/<corpus_id>`
///
/// On crash during steps 1-3 the `.incoming/` dir is left dirty — the
/// daemon cleans it on next startup.  Step 4 is atomic on POSIX systems
/// so a completed merge can never see a partially-written index.
pub async fn index_transfer(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> (StatusCode, Json<serde_json::Value>) {
    let corpus_id = match headers.get("X-Corpus-Id").and_then(|v| v.to_str().ok()) {
        Some(id) => id.to_string(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "missing X-Corpus-Id header"})),
            );
        }
    };

    let engine = match &state.inner.corpus_engine {
        Some(e) => e.clone(),
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": "no corpus engine on this node"})),
            );
        }
    };

    let index_dir = engine.index_dir().to_path_buf();
    let incoming_dir = index_dir.join(".incoming");
    let tarball_path = incoming_dir.join(format!("{corpus_id}.tar"));
    let unpack_path = incoming_dir.join(&corpus_id);

    if let Err(e) = std::fs::create_dir_all(&incoming_dir) {
        tracing::error!(error = %e, "index_transfer: failed to create .incoming dir");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        );
    }

    // Write tarball to disk.
    if let Err(e) = std::fs::write(&tarball_path, &body) {
        tracing::error!(error = %e, "index_transfer: failed to write tarball");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        );
    }

    // Untar.
    if let Err(e) = std::fs::create_dir_all(&unpack_path) {
        tracing::error!(error = %e, "index_transfer: failed to create unpack dir");
        let _ = std::fs::remove_file(&tarball_path);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        );
    }
    let tar_status = std::process::Command::new("tar")
        .args([
            "xf",
            &tarball_path.to_string_lossy(),
            "-C",
            &unpack_path.to_string_lossy(),
        ])
        .status();
    let _ = std::fs::remove_file(&tarball_path);
    match tar_status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            let _ = std::fs::remove_dir_all(&unpack_path);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("tar exited with {s}")})),
            );
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&unpack_path);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            );
        }
    }

    // Verify the unpacked directory has _corpus_meta.json.
    if !unpack_path.join("_corpus_meta.json").exists() {
        tracing::error!(
            corpus = %corpus_id,
            path = %unpack_path.display(),
            "index_transfer: unpacked shard is missing _corpus_meta.json"
        );
        let _ = std::fs::remove_dir_all(&unpack_path);
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error": "shard missing _corpus_meta.json"})),
        );
    }

    // Atomic rename to final location.
    let final_path = index_dir.join(&corpus_id);
    if let Err(e) = std::fs::rename(&unpack_path, &final_path) {
        tracing::error!(
            error = %e,
            from = %unpack_path.display(),
            to = %final_path.display(),
            "index_transfer: failed to rename to final path"
        );
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        );
    }

    let bytes = body.len() as u64;
    // Phase C2: if the unpacked corpus carries an atlas dir, read
    // its fingerprint + atom counts and include them in the
    // response so the puller can validate against the gossiped
    // advertisement. No body re-write — atlas state lives under
    // `<corpus>/atlas/` and was already inside the transferred tar.
    //
    // We also drop the receiver's stale `_summary.json` (the
    // puller's pre-existing summary, if any, was pre-rename and
    // now matches no atoms.json on disk) so the next gossip round
    // recomputes against the just-pulled atoms.json. The embeddings
    // cache (`atoms.embeddings.bin`) self-invalidates via the
    // header-mismatch check in `read_atlas_embeddings` when the
    // pulled atoms_content_hash or embed_model differs.
    let atlas_dir = final_path.join("atlas");
    let _ = std::fs::remove_file(atlas_dir.join("_summary.json"));
    let atlas_summary = corpus_engine::enrichment::atlas::read_or_compute_atlas_summary(&atlas_dir)
        .ok()
        .flatten();
    let atlas_meta = match atlas_summary {
        Some(s) => serde_json::json!({
            "atom_count": s.atom_count,
            "tier2_count": s.tier2_count,
            "fingerprint": s.fingerprint,
        }),
        None => serde_json::json!(null),
    };
    tracing::info!(
        corpus = %corpus_id,
        bytes,
        path = %final_path.display(),
        atlas_present = !atlas_meta.is_null(),
        "index_transfer: shard installed successfully"
    );

    // Self-heal hook for `mutable_merge` corpora (the alignment
    // recipe). project() rechecks the policy on the unpacked
    // partition's `_corpus_meta.json` and is a no-op for every
    // classic corpus, so this is safe to invoke unconditionally on
    // the receive path. The transferred dir IS a partition (not the
    // canonical), but it has the same _corpus_meta + chunks layout,
    // so the projector reads it fine — this lets a peer's edits land
    // on disk even before the canonical merge runs.
    if let Some(home) = dirs::home_dir() {
        match corpus_engine::alignment_projector::project(&final_path, &home).await {
            Ok(p) => {
                if p.wrote > 0 || p.skipped_local_newer > 0 {
                    tracing::info!(
                        corpus = %corpus_id,
                        wrote = p.wrote,
                        skipped_local_newer = p.skipped_local_newer,
                        skipped_unsafe_path = p.skipped_unsafe_path,
                        "index_transfer: alignment projection complete"
                    );
                }
            }
            Err(e) => tracing::warn!(
                corpus = %corpus_id,
                error = %e,
                "index_transfer: alignment projection failed; transfer stands"
            ),
        }
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "corpus_id": corpus_id,
            "bytes_received": bytes,
            "path": final_path.to_string_lossy(),
            "atlas": atlas_meta,
        })),
    )
}
