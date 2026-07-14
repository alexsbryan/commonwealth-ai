// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-user private corpus uploads for the multi-tenant hub (A3b-2).
//!
//! A file uploaded by a tenant becomes a `Private { owner }` corpus. That
//! requires TWO artifacts under ONE identical `corpus_id`:
//!
//!   1. A real LanceDB index (so `CorpusEngine::installed_indexes()` returns
//!      it and the retrieval pipeline can search its chunks), built by
//!      `CorpusEngine::ingest` from an inline `local_file` recipe; and
//!   2. A `CorpusState` row stamped `Private { owner }`, so the per-principal
//!      retrieval ceiling (`ConversationContext::corpus_ceiling`, enforced as
//!      "Filter 5" in `runtime/retrieval.rs`) admits the corpus for its owner
//!      and excludes it from every other tenant.
//!
//! BOTH are required: the ceiling is an allow-list of `CorpusState` corpus_ids
//! intersected with the on-disk index set, so an index with no row is
//! unsearchable even by its owner, and a row with no index is empty. This is
//! the ingest half of A3b; the enforcement half (the ceiling) shipped in
//! A3b-1. Together they deliver the "per-user private uploads, isolated"
//! layer of the SaaS hub.
//!
//! The recipe declares no `[enrichment]` block, so the upload is a plain
//! Knowledge corpus (chunk index only — no atlas/RAPTOR). That is deliberate:
//! it keeps ingest cheap AND keeps private uploads off the enrichment-driven
//! retrieval boosts entirely, so the chunk-search ceiling is the whole story.

use std::path::Path;
use std::sync::Arc;

use axum::extract::Extension;
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::post;
use axum::Router;

use corpus_engine::{CorpusEngine, CorpusSpec, Recipe};
use sovereign_core::runtime::Runtime;
use sovereign_core::traits::StateStore;
use sovereign_core::types::{CorpusState, CorpusVisibility};

use crate::auth::TenantId;

// ─── Request / Response ──────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct CorpusUploadRequest {
    /// Absolute path to the file on the server's filesystem.
    pub file_path: String,
    /// Human-facing corpus name. The owner-namespaced, filesystem-safe
    /// `corpus_id` is derived from it (`user:<tenant>:<slug>`).
    pub name: String,
}

#[derive(serde::Serialize)]
pub struct CorpusUploadResponse {
    pub corpus: CorpusState,
}

#[derive(serde::Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

type ApiResult<T> = Result<Json<T>, (StatusCode, Json<ErrorResponse>)>;

fn api_error(status: StatusCode, msg: &str) -> (StatusCode, Json<ErrorResponse>) {
    (
        status,
        Json(ErrorResponse {
            error: msg.to_string(),
        }),
    )
}

// ─── corpus_id derivation ────────────────────────────────────

/// Owner-namespaced corpus id: `user:<owner>:<slug(name)>`.
///
/// The `owner` segment is the raw tenant id (operator-controlled, assumed
/// path-safe) so one tenant's uploads never collide with another's or with
/// shared `Org` corpora. Only the untrusted user-supplied `name` is slugified
/// — that also neutralises any path-traversal attempt in `name`, since every
/// non-alphanumeric character (including `/` and `.`) collapses to `-`.
///
/// Re-uploading under the same name yields the same id (update-by-name).
pub fn private_corpus_id(owner: &str, name: &str) -> String {
    format!("user:{}:{}", owner, slugify(name))
}

/// Lowercase, alphanumeric-or-dash slug; runs of separators collapse to a
/// single `-`, leading/trailing dashes are trimmed. Empty input (or input
/// with no alphanumerics) falls back to `corpus` so the id is always valid.
fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "corpus".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Pick the recipe extractor by file extension. Markdown for `.md`/`.markdown`
/// (which also handles plain prose), plaintext for everything else. v1 covers
/// text documents; binary formats (PDF, etc.) are a follow-on — they need the
/// staging/extraction the desktop's `LocalCorpusManager` does.
fn extract_type_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("md") | Some("markdown") => "markdown",
        _ => "plaintext",
    }
}

/// Escape a value for embedding inside a TOML basic (double-quoted) string.
fn toml_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Build the inline recipe TOML for a single uploaded file. No `[enrichment]`
/// block ⇒ a plain Knowledge corpus (chunk index only). Mirrors the proven
/// `local_file` recipe shape that `LocalCorpusManager` generates, minus the
/// JSONL pre-extraction (we read the raw file directly) and `personal_scope`
/// (isolation here is the `CorpusState` ceiling, not the engine flag).
fn private_corpus_recipe_toml(corpus_id: &str, name: &str, file_path: &Path) -> String {
    format!(
        r#"[corpus]
id = "{id}"
name = "{name}"

[acquire]
type = "local_file"
path = "{path}"

[extract]
type = "{extract}"

[chunk]
type = "paragraph"
max_chars = 2048
overlap_chars = 256

[index]
fts = true
vector = true
"#,
        id = corpus_id,
        name = toml_escape(name),
        path = toml_escape(&file_path.display().to_string()),
        extract = extract_type_for(file_path),
    )
}

use sovereign_core::time::unix_now as unix_secs;

// ─── Core ingest (testable without a full Runtime) ───────────

/// Ingest `file_path` into a `Private { owner }` corpus and persist both
/// artifacts (LanceDB index + `CorpusState`) under one identical `corpus_id`.
/// Returns the persisted `CorpusState`.
///
/// This is the load-bearing seam of A3b-2: the `Private { owner }` visibility
/// stamp is what couples a brand-new index to the A3b-1 retrieval ceiling.
/// Get it wrong (`Org`, or a mismatched owner) and the upload leaks to every
/// tenant; omit the `CorpusState` write and the owner can't search their own
/// upload. Kept as a free function so it is exercised directly by the
/// isolation test, no HTTP stack required.
pub async fn ingest_private_corpus(
    engine: &CorpusEngine,
    store: &dyn StateStore,
    file_path: &Path,
    owner: &str,
    name: &str,
) -> Result<CorpusState, String> {
    if !file_path.exists() {
        return Err(format!("File not found: {}", file_path.display()));
    }

    let corpus_id = private_corpus_id(owner, name);

    // 1. Build the LanceDB index via the live engine (embed_fn + model are
    //    already baked into the server's engine; the recipe is inline so no
    //    registry lookup happens).
    let toml = private_corpus_recipe_toml(&corpus_id, name, file_path);
    let recipe = Recipe::from_toml(&toml).map_err(|e| format!("recipe build failed: {e}"))?;
    let result = engine
        .ingest(&CorpusSpec::Inline(Box::new(recipe)), None)
        .await
        .map_err(|e| format!("ingest failed: {e}"))?;

    // 2. THE SEAM — stamp the `Private { owner }` ceiling row. `owner` is the
    //    raw tenant id, matched verbatim against the conversation principal in
    //    `build_context` (owner == principal), so it MUST equal the tenant id,
    //    not a slug.
    let now = unix_secs();
    let state = CorpusState {
        corpus_id: corpus_id.clone(),
        installed_at: now,
        source_date: chrono::Utc::now().format("%Y-%m-%d").to_string(),
        chunks_count: result.chunks_created as i64,
        index_size_mb: (result.index_size_bytes / 1_000_000) as i64,
        last_updated: now,
        version: now,
        deleted_at: None,
        vector_index_ready: true,
        visibility: CorpusVisibility::Private {
            owner: owner.to_string(),
        },
    };
    store
        .save_corpus_state(&state)
        .await
        .map_err(|e| format!("save_corpus_state failed: {e}"))?;

    tracing::info!(
        target: "corpus.upload",
        corpus_id = %corpus_id,
        owner = %owner,
        chunks = result.chunks_created,
        "corpus.upload: private corpus ingested + ceiling row written"
    );

    Ok(state)
}

// ─── Router ──────────────────────────────────────────────────

pub fn corpus_upload_router() -> Router {
    Router::new().route("/v1/corpora/upload", post(upload_private_corpus))
}

/// POST /v1/corpora/upload
async fn upload_private_corpus(
    Extension(runtime): Extension<Arc<Runtime>>,
    Extension(tenant): Extension<TenantId>,
    Json(body): Json<CorpusUploadRequest>,
) -> ApiResult<CorpusUploadResponse> {
    let engine = runtime.corpus_engine.as_deref().ok_or_else(|| {
        api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "corpus engine is not configured on this server",
        )
    })?;

    let state = ingest_private_corpus(
        engine,
        runtime.store.as_ref(),
        Path::new(&body.file_path),
        &tenant.0,
        &body.name,
    )
    .await
    .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, &e))?;

    Ok(Json(CorpusUploadResponse { corpus: state }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use corpus_engine::EmbedFn;
    // `get_corpus_state` is declared on `CorpusStateStore`; bring it into
    // scope so it resolves on the concrete `SqliteStateStore` below.
    use sovereign_core::traits::CorpusStateStore;
    use sovereign_store::sqlite::SqliteStateStore;

    fn mock_embed_fn() -> EmbedFn {
        Arc::new(|text: &str| {
            // A deterministic, non-zero vector. Dimensionality and exact
            // values are irrelevant to the isolation proof — we only need
            // ingest to produce a real on-disk index.
            let len = text.len() as f32;
            let v: Vec<f32> = (0..8).map(|i| (len + i as f32) / 100.0).collect();
            Box::pin(async move { Ok(v) })
        })
    }

    #[test]
    fn slugify_and_id_are_path_safe_and_owner_namespaced() {
        assert_eq!(slugify("My Notes!"), "my-notes");
        assert_eq!(slugify("../../etc/passwd"), "etc-passwd");
        assert_eq!(slugify("   "), "corpus");
        // Owner stays raw (it must match the principal verbatim); only the
        // untrusted name is slugified.
        assert_eq!(
            private_corpus_id("alice", "My Notes"),
            "user:alice:my-notes"
        );
        assert!(!private_corpus_id("alice", "../escape").contains('/'));
    }

    /// The A3b-2 end-to-end proof: an upload becomes a `Private { owner }`
    /// corpus with BOTH artifacts under one id, and the A3b-1 ceiling then
    /// isolates it — its owner retrieves over it, no other tenant can.
    #[tokio::test]
    async fn upload_becomes_private_corpus_isolated_from_other_tenants() {
        let tmp = tempfile::tempdir().unwrap();
        let recipes = tmp.path().join("recipes");
        std::fs::create_dir_all(&recipes).unwrap();
        let engine = CorpusEngine::new(recipes, tmp.path().join("indexes"), mock_embed_fn())
            .with_embedding_model("test-mock");
        let store = SqliteStateStore::open_in_memory().unwrap();

        // Alice uploads a file carrying a secret token.
        let file = tmp.path().join("alice-notes.txt");
        std::fs::write(
            &file,
            "Project Aurora kickoff.\n\nThe staging passphrase is SWORDFISH-7788.\n\n\
             Budget review is scheduled for the third quarter.\n\n\
             The vendor shortlist has three finalists.\n\n\
             Security review must precede any public launch.\n",
        )
        .unwrap();

        let state = ingest_private_corpus(&engine, &store, &file, "alice", "Aurora Notes")
            .await
            .expect("ingest must succeed");

        // Artifact 1 — a real index the engine can enumerate (so retrieval
        // can search it).
        assert_eq!(state.corpus_id, "user:alice:aurora-notes");
        let indexes = engine.installed_indexes().await.unwrap();
        assert!(
            indexes.iter().any(|i| i.corpus_id == state.corpus_id),
            "ingest must produce a searchable LanceDB index; got {:?}",
            indexes.iter().map(|i| &i.corpus_id).collect::<Vec<_>>()
        );

        // Artifact 2 — a Private{owner} CorpusState keyed on the SAME id.
        let expected_vis = CorpusVisibility::Private {
            owner: "alice".to_string(),
        };
        assert_eq!(state.visibility, expected_vis);
        let persisted = store.get_corpus_state(&state.corpus_id).await.unwrap();
        assert_eq!(persisted.visibility, expected_vis);

        // The loop closes through the A3b-1 ceiling. Alice retrieves over her
        // upload…
        let alice_ctx =
            sovereign_core::context::build_context(&store, "alice:c", "passphrase", Some("alice"))
                .await
                .unwrap();
        assert!(
            alice_ctx
                .corpus_ceiling
                .as_ref()
                .unwrap()
                .contains(&state.corpus_id),
            "owner's ceiling must include her own upload"
        );

        // …and Bob — a different tenant on the same hub — never sees it, on
        // the default chat path (no enabled_corpora selection at all).
        let bob_ctx =
            sovereign_core::context::build_context(&store, "bob:c", "passphrase", Some("bob"))
                .await
                .unwrap();
        assert!(
            !bob_ctx
                .corpus_ceiling
                .as_ref()
                .unwrap()
                .contains(&state.corpus_id),
            "ISOLATION LEAK: Bob's retrieval ceiling includes Alice's private upload: {:?}",
            bob_ctx.corpus_ceiling
        );
    }
}
