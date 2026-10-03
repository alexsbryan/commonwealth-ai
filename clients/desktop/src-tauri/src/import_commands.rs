// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tauri commands powering the Settings → Imports tab.
//!
//! v1 ships the **Anthropic** path only: the user picks the
//! `data-<uuid>-…batch-0000.zip` Anthropic produces from
//! Settings → Privacy → Export data; this module unpacks the
//! `conversations.json` entry into the canonical landing path
//! (`~/.svrnmesh/conversations/conversations.json`), counts
//! messages for a pre-flight ETA, and posts to the daemon's
//! `/internal/corpus/install` so the existing
//! `conversations-anthropic` recipe drives ingest. The progress
//! stream is already wired (`corpus-progress` Tauri event); the
//! ImportsTab subscribes to it and renders the live ETA.
//!
//! ChatGPT + Gemini paths are deferred (SYSTEM_OVERVIEW §10.1).
//! The seam to add them is the `source` discriminator on
//! `ImportStartResponse` plus a sibling extractor + recipe — the
//! progress + Atlas-View grouping infrastructure is source-agnostic.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const CANONICAL_FILE: &str = "conversations.json";

/// Which chat vendor's export we're importing. The import flow is
/// byte-for-byte identical across vendors — pick the zip, land
/// `conversations.json`, post the install — so only these per-source
/// bindings differ. Everything downstream (chunking, conversational
/// enrichment, Atlas-View "Conversations" grouping) keys on the
/// recipe's chunker/domain/`[display] category`, NOT the corpus id, so
/// adding a vendor is just another arm here plus its extractor + recipe
/// (`SYSTEM_OVERVIEW §10.1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportSource {
    Anthropic,
    Chatgpt,
}

impl ImportSource {
    /// Corpus id == recipe id installed for this source.
    fn corpus_id(self) -> &'static str {
        match self {
            ImportSource::Anthropic => "conversations-anthropic",
            ImportSource::Chatgpt => "conversations-chatgpt",
        }
    }

    /// Landing dir (relative to the per-user root) the matching recipe's
    /// `[acquire]` path reads from. Resolved at runtime so the tab works
    /// regardless of where the root resolves. SEPARATE dirs per vendor
    /// because both name the export file `conversations.json` — a shared
    /// dir would clobber.
    fn canonical_rel_dir(self) -> &'static str {
        match self {
            ImportSource::Anthropic => "conversations",
            ImportSource::Chatgpt => "conversations-chatgpt",
        }
    }

    /// Per-message byte marker for the best-effort pre-flight count.
    /// Anthropic emits one `"sender"` per message; ChatGPT one
    /// `"author"` per message node. Off by at most a handful when the
    /// token appears inside message text — the ETA is a ±30% band.
    fn count_needle(self) -> &'static [u8] {
        match self {
            ImportSource::Anthropic => b"\"sender\"",
            ImportSource::Chatgpt => b"\"author\"",
        }
    }

    /// Human label for picker/error copy.
    fn display_name(self) -> &'static str {
        match self {
            ImportSource::Anthropic => "Anthropic",
            ImportSource::Chatgpt => "ChatGPT",
        }
    }
}

/// Seconds per message — baked benchmark constant from one
/// calibration run of the conversation_atlas pipeline against the
/// user's own 90 MB Anthropic export (~10K messages, single primary
/// chat slot, M-series). The pre-flight ETA the UI shows is
/// `total_messages * SECONDS_PER_MESSAGE` displayed as a `±30%`
/// band. If the constant drifts (model swap, pipeline-phase change),
/// the live ETA derived from streaming progress corrects within ~60s
/// of warmup — the band gives us slack for that.
const SECONDS_PER_MESSAGE: f64 = 0.4;

/// Outcome of [`import_anthropic_zip`]. The two-variant tagged
/// shape lets the UI distinguish "install dispatched, watch the
/// progress channel" from "a partial corpus exists, confirm a
/// reset before we proceed."
///
/// Partial state shows up after a daemon crash mid-ingest or after
/// a chunker fix lands while an old import was in flight (the
/// concrete trigger for the destructive-confirm flow: the
/// pre-1500-char-cap import left rows whose embeddings are
/// truncated; resuming would interleave them with new-shape rows
/// that the new chunker emits). The UI surfaces a banner with a
/// "Reset and re-import" button that re-invokes this command with
/// `reset_partial = true`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImportStartResponse {
    /// Install POST accepted; the ImportsTab subscribes to
    /// `corpusProgressStore.byId[corpus_id]` from here.
    Started {
        corpus_id: String,
        total_messages: u64,
        estimated_minutes: f64,
        /// Where the canonical `conversations.json` landed. Surfaced
        /// for glassbox UX so an operator can verify the move
        /// without trusting the toast.
        canonical_path: String,
    },
    /// An existing partial index was detected; install was NOT
    /// dispatched. The user must explicitly confirm a reset (re-
    /// invoke with `reset_partial: true`) or cancel.
    PartialIndexExists {
        corpus_id: String,
        index_path: String,
        /// Total messages parsed from the freshly-extracted
        /// `conversations.json`. Forwarded so the UI can render the
        /// pre-flight estimate alongside the confirmation banner —
        /// the user sees "you'll re-embed N messages" before
        /// clicking through.
        total_messages: u64,
        estimated_minutes: f64,
        canonical_path: String,
    },
}

#[derive(Debug, Clone, Deserialize)]
pub struct ImportConversationsZipRequest {
    pub zip_path: PathBuf,
    /// `true` ⇒ wipe any existing
    /// `~/.svrnmesh/indexes/<corpus_id>/` directory before posting the
    /// install. The UI sends this on the second invocation after the
    /// user confirms the destructive prompt.
    #[serde(default)]
    pub reset_partial: bool,
}

/// Tauri command: import the user's **Claude (Anthropic)** chat export.
/// Thin wrapper over [`run_conversation_import`] — see it for the flow.
#[tauri::command]
pub async fn import_anthropic_zip(
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
    request: ImportConversationsZipRequest,
) -> Result<ImportStartResponse, String> {
    run_conversation_import(ImportSource::Anthropic, app_handle, state, request).await
}

/// Tauri command: import the user's **ChatGPT (OpenAI)** chat export.
/// Thin wrapper over [`run_conversation_import`] — see it for the flow.
#[tauri::command]
pub async fn import_chatgpt_zip(
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
    request: ImportConversationsZipRequest,
) -> Result<ImportStartResponse, String> {
    run_conversation_import(ImportSource::Chatgpt, app_handle, state, request).await
}

/// Unpack the chat export the user picked, land its `conversations.json`
/// at the canonical path the matching recipe reads from, and kick off
/// the daemon install. The progress + ETA UX after this returns is
/// driven by the existing `corpus-progress` event stream — this is just
/// the entry hop. Vendor-neutral: all per-source differences are behind
/// [`ImportSource`].
async fn run_conversation_import(
    source: ImportSource,
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
    request: ImportConversationsZipRequest,
) -> Result<ImportStartResponse, String> {
    let zip_path = request.zip_path;
    if !zip_path.exists() {
        return Err(format!(
            "selected file does not exist: {}",
            zip_path.display()
        ));
    }
    match zip_path.extension().and_then(|s| s.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("zip") => {}
        _ => {
            return Err(format!(
                "Imports expects a {} export .zip; got {}",
                source.display_name(),
                zip_path
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("<no extension>")
            ));
        }
    }

    let canonical_path = canonical_landing_path(source)?;
    let extracted_bytes =
        tokio::task::spawn_blocking(move || unpack_conversations_json(&zip_path, &canonical_path))
            .await
            .map_err(|e| format!("unpack task panicked: {e}"))??;

    tracing::info!(
        target: "imports",
        canonical_path = %extracted_bytes.canonical_path.display(),
        archive_bytes = extracted_bytes.archive_entry_bytes,
        "imports: zip unpacked"
    );

    let total_messages =
        count_messages_in_file(&extracted_bytes.canonical_path, source.count_needle())
            .unwrap_or_else(|e| {
                // Counting is best-effort. We have the file at the
                // canonical path either way; the ETA just degrades to
                // "we don't know" rather than blocking the install.
                tracing::warn!(
                    target: "imports",
                    error = %e,
                    "imports: message-count probe failed — ETA will degrade",
                );
                0
            });

    let estimated_minutes = if total_messages > 0 {
        (total_messages as f64 * SECONDS_PER_MESSAGE / 60.0).max(0.5)
    } else {
        0.0
    };

    let corpus_id = source.corpus_id().to_string();

    // Early-return when the daemon is already ingesting this corpus.
    // Two reasons:
    //   1. The status poller's `install_progress` map carries any
    //      in-flight phase the daemon has reported. If it's non-
    //      terminal, posting `/internal/corpus/install` again is at
    //      best a no-op (daemon logs "already active — not spawning
    //      a second task") and at worst races our optimistic
    //      `downloading 0%` event over the real phase the daemon
    //      is in.
    //   2. The user clicked Import after a prior session left the
    //      daemon mid-ingest. The Auto-Resume path (see
    //      `importsStore.hydrateFromDaemon`) covers the App-Start
    //      case; this branch covers the still-Tab-Open case where
    //      a stale picker UI led the user to click Import a second
    //      time.
    {
        let map = state.install_progress.read().await;
        if let Some(p) = map.get(&corpus_id) {
            if p.phase != "complete" && p.phase != "failed" {
                tracing::info!(
                    target: "imports",
                    corpus_id = %corpus_id,
                    current_phase = %p.phase,
                    "imports: install request short-circuited — daemon already ingesting"
                );
                return Ok(ImportStartResponse::Started {
                    corpus_id: corpus_id.clone(),
                    total_messages: total_messages as u64,
                    estimated_minutes,
                    canonical_path: extracted_bytes.canonical_path.display().to_string(),
                });
            }
        }
    }

    // Destructive-confirm flow. The conversation-anthropic chunker
    // fix (threaded_turns soft cap, 2026-05-18) means any
    // pre-existing partial index carries embeddings computed on
    // oversized truncated chunks. Resuming would interleave them
    // with new-shape rows; the only way to a clean result is to
    // wipe the partial dir. We require an explicit confirmation
    // to do that — `reset_partial: true` on the request.
    let index_dir = conversations_index_dir(source)?;
    if index_dir.exists() && index_has_content(&index_dir) {
        if !request.reset_partial {
            return Ok(ImportStartResponse::PartialIndexExists {
                corpus_id,
                index_path: index_dir.display().to_string(),
                total_messages: total_messages as u64,
                estimated_minutes,
                canonical_path: extracted_bytes.canonical_path.display().to_string(),
            });
        }
        // Confirmed reset. Wipe the dir; surface a tracing event so
        // a forensic look later can correlate the destructive op
        // with the user's confirmation click.
        tracing::warn!(
            target: "imports",
            index_dir = %index_dir.display(),
            "imports: removing partial index on user-confirmed reset"
        );
        if let Err(e) = std::fs::remove_dir_all(&index_dir) {
            return Err(format!(
                "could not reset partial index at {}: {e}",
                index_dir.display()
            ));
        }
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;
    let daemon = state.internal_base_url();
    let url = format!("{daemon}/internal/corpus/install");
    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "corpus_id": corpus_id,
            "parameters": serde_json::Map::<String, serde_json::Value>::new(),
        }))
        .send()
        .await
        .map_err(|e| {
            // Friendly copy for the common case (daemon not running
            // / port unreachable / TLS handshake denied). Raw reqwest
            // errors are noisy and intimidating in a settings tab.
            tracing::warn!(
                target: "imports",
                error = %e,
                "imports: POST /internal/corpus/install failed"
            );
            "Couldn't reach svrnmesh. Make sure the daemon is running \
             (try `sovereign daemon start`) and click Import again."
                .to_string()
        })?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        tracing::warn!(
            target: "imports",
            status = %status,
            body = %body,
            "imports: daemon /internal/corpus/install returned non-success"
        );
        return Err(format!(
            "svrnmesh rejected the import (HTTP {status}). Check the \
             daemon logs at ~/.svrnmesh/logs/daemon.out for details."
        ));
    }

    // Mirror the recipe-install optimistic UI flip — drop a
    // "downloading 0%" frame into the store + emit the event so the
    // ImportsTab's progress card has something to render immediately,
    // rather than blanking until the daemon's first real phase
    // arrives.
    let initial = crate::commands::CorpusProgressPayload {
        corpus_id: corpus_id.clone(),
        phase: "downloading".into(),
        percent: 0.0,
        chunks_processed: 0,
        message: Some("Starting…".into()),
        ..Default::default()
    };
    if let Ok(mut map) = state.install_progress.try_write() {
        map.insert(corpus_id.clone(), initial.clone());
    }
    use tauri::Emitter;
    let _ = app_handle.emit("corpus-progress", initial);

    tracing::info!(
        target: "imports",
        corpus_id = %corpus_id,
        total_messages,
        estimated_minutes,
        "imports: install dispatched"
    );

    Ok(ImportStartResponse::Started {
        corpus_id,
        total_messages: total_messages as u64,
        estimated_minutes,
        canonical_path: extracted_bytes.canonical_path.display().to_string(),
    })
}

/// Resolve the canonical on-disk index dir for `source`'s corpus.
/// Mirrors the path `~/.svrnmesh/indexes/<corpus_id>` the daemon's
/// `CorpusEngine` uses by convention (see `state.rs::build_app_state`).
fn conversations_index_dir(source: ImportSource) -> Result<PathBuf, String> {
    Ok(sovereign_contracts::rebrand::svrnmesh_root()
        .join("indexes")
        .join(source.corpus_id()))
}

/// `true` ⇔ `dir` looks like a real partial-or-complete index (has
/// a meta file or any LanceDB artifact). Empty placeholder dirs
/// don't count — the daemon sometimes creates parent dirs ahead of
/// the first write and we don't want to trip the destructive-confirm
/// banner on an empty shell.
fn index_has_content(dir: &Path) -> bool {
    if dir.join("_corpus_meta.json").exists() {
        return true;
    }
    if dir.join("chunks.lance").exists() {
        return true;
    }
    // Fallback — any visible child indicates state worth confirming.
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let name = entry.file_name();
            let s = name.to_string_lossy();
            if !s.starts_with('.') {
                return true;
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Result of streaming the `conversations.json` entry out of the
/// user-picked archive. Exposed for the tracing event so an operator
/// can correlate "user clicked import" with "N bytes landed."
#[derive(Debug)]
struct ExtractedEntry {
    canonical_path: PathBuf,
    archive_entry_bytes: u64,
}

fn canonical_landing_path(source: ImportSource) -> Result<PathBuf, String> {
    let dir = sovereign_contracts::rebrand::svrnmesh_root().join(source.canonical_rel_dir());
    fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    Ok(dir.join(CANONICAL_FILE))
}

/// Stream the `conversations.json` entry out of the zip and land it
/// at `dest`. Atomic rename via a `.tmp` sibling so a partial copy
/// doesn't poison the canonical path; existing canonical file gets
/// rotated to `conversations.json.bak-<unix_ts>` so re-importing
/// doesn't silently overwrite a prior import.
///
/// Returns the resolved destination plus the entry's uncompressed
/// byte length (for the glassbox tracing event).
fn unpack_conversations_json(zip_path: &Path, dest: &Path) -> Result<ExtractedEntry, String> {
    let file = fs::File::open(zip_path).map_err(|e| format!("open {}: {e}", zip_path.display()))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("read zip {}: {e}", zip_path.display()))?;

    // Locate the first entry whose path ends in `conversations.json`.
    // Anthropic ships the export either at the archive root or under
    // one nesting level (`data-<uuid>-batch-0000/`); accept both
    // without requiring the user to know which they have.
    let entry_index = (0..archive.len())
        .find(|i| {
            archive
                .by_index_raw(*i)
                .ok()
                .and_then(|e| {
                    let name = e.name().to_string();
                    let leaf = Path::new(&name)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("")
                        .to_string();
                    if leaf.eq_ignore_ascii_case(CANONICAL_FILE) {
                        Some(())
                    } else {
                        None
                    }
                })
                .is_some()
        })
        .ok_or_else(|| {
            format!(
                "no `{CANONICAL_FILE}` entry inside {}; is this a chat export?",
                zip_path.display()
            )
        })?;

    // Rotate any prior canonical file out of the way so the
    // re-import is non-destructive. Operator can see the prior copy
    // at .bak-<ts> if they want to compare.
    if dest.exists() {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let backup = dest.with_file_name(format!("{CANONICAL_FILE}.bak-{ts}"));
        fs::rename(dest, &backup)
            .map_err(|e| format!("rotate prior canonical to {}: {e}", backup.display()))?;
    }

    let tmp_path = dest.with_extension("json.tmp");
    let mut entry = archive
        .by_index(entry_index)
        .map_err(|e| format!("read zip entry: {e}"))?;
    let archive_entry_bytes = entry.size();
    {
        let mut tmp_file = fs::File::create(&tmp_path)
            .map_err(|e| format!("create {}: {e}", tmp_path.display()))?;
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = entry
                .read(&mut buf)
                .map_err(|e| format!("read zip entry body: {e}"))?;
            if n == 0 {
                break;
            }
            tmp_file
                .write_all(&buf[..n])
                .map_err(|e| format!("write {}: {e}", tmp_path.display()))?;
        }
        tmp_file
            .sync_all()
            .map_err(|e| format!("fsync {}: {e}", tmp_path.display()))?;
    }
    fs::rename(&tmp_path, dest)
        .map_err(|e| format!("rename {} -> {}: {e}", tmp_path.display(), dest.display()))?;

    Ok(ExtractedEntry {
        canonical_path: dest.to_path_buf(),
        archive_entry_bytes,
    })
}

/// Counts the messages across every conversation in the export by
/// scanning for `needle` — a per-message field marker the caller picks
/// per source (`"sender"` for Anthropic, `"author"` for ChatGPT; see
/// [`ImportSource::count_needle`]). Reads the file in chunks rather than
/// slurping it whole — a power-user export is easily 100+ MB.
///
/// "Best effort": the marker appears ~once per message in each vendor's
/// schema. Wrong by at most a handful when the token appears inside
/// message text — the pre-flight ETA is a `±30%` band anyway, so a few
/// stray matches don't matter.
fn count_messages_in_file(path: &Path, needle: &[u8]) -> Result<u64, String> {
    let file = fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut reader = std::io::BufReader::new(file);
    let mut total: u64 = 0;
    let mut carry: Vec<u8> = Vec::with_capacity(needle.len() - 1);
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        // Concatenate the carry from the prior chunk so we don't
        // miss a needle straddling a buffer boundary.
        let mut window: Vec<u8> = Vec::with_capacity(carry.len() + n);
        window.extend_from_slice(&carry);
        window.extend_from_slice(&buf[..n]);

        let mut i = 0;
        while i + needle.len() <= window.len() {
            if &window[i..i + needle.len()] == needle {
                total += 1;
                i += needle.len();
            } else {
                i += 1;
            }
        }
        // Preserve the tail (needle.len() - 1 bytes) for the next pass.
        let keep = needle.len() - 1;
        if window.len() > keep {
            carry.clear();
            carry.extend_from_slice(&window[window.len() - keep..]);
        } else {
            carry = window;
        }
    }
    Ok(total)
}

// ---------------------------------------------------------------------------
// Email archive import (mbox / maildir / .eml folder)
// ---------------------------------------------------------------------------
//
// Same lifecycle as the conversation imports (short-circuit → destructive
// confirm → POST install → optimistic progress frame), with two deliberate
// divergences:
//
//   1. NO staging copy. The `email-archive` recipe declares a required
//      `path` parameter and reads the mailbox IN PLACE — the picked path
//      travels through the install POST's `parameters` map and is
//      interpolated into the recipe's `[acquire] path = "{path}"` at
//      acquire time (`ingest_factories::render_against_parameters`).
//   2. NO auto-enrichment downstream. The recipe ships `[enrichment]`
//      off (LLM-bound, hours on a big mailbox); the frontend store for
//      this corpus completes at ingest instead of chaining
//      `enrich_build_async`.

/// The bundled recipe this import drives. Its `scope = "local"` makes
/// the privacy promise structural: never advertised, never replicated,
/// never federated-queried.
pub const EMAIL_CORPUS_ID: &str = "email-archive";

/// Embed-bound estimate (no enrichment pass runs on this path). The
/// live ETA refines it as soon as real phases stream.
const EMAIL_SECONDS_PER_MESSAGE: f64 = 0.05;

#[derive(Debug, Clone, Deserialize)]
pub struct ImportEmailArchiveRequest {
    /// The user-picked mailbox: a Takeout/Apple-Mail `.mbox` file, a
    /// Thunderbird mbox store (no extension), a maildir root, a folder
    /// of `.eml` files, or a single `.eml`.
    pub path: PathBuf,
    /// Explicit user confirmation to wipe a pre-existing partial index.
    #[serde(default)]
    pub reset_partial: bool,
}

#[tauri::command]
pub async fn import_email_archive(
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
    request: ImportEmailArchiveRequest,
) -> Result<ImportStartResponse, String> {
    run_email_import(app_handle, state, request).await
}

async fn run_email_import(
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
    request: ImportEmailArchiveRequest,
) -> Result<ImportStartResponse, String> {
    let source_path = request.path.clone();
    validate_email_source(&source_path)?;

    let corpus_id = EMAIL_CORPUS_ID.to_string();

    // Preflight count — best-effort, like the conversation imports'
    // needle scan. mbox counts postmarks; a folder counts files.
    let count_path = source_path.clone();
    let total_messages = match tokio::task::spawn_blocking(move || count_emails(&count_path)).await
    {
        Ok(Ok(n)) => n,
        Ok(Err(e)) => {
            tracing::warn!(target: "imports", error = %e, "imports: email preflight count failed");
            0
        }
        Err(e) => {
            tracing::warn!(target: "imports", error = %e, "imports: email preflight count join error");
            0
        }
    };
    let estimated_minutes = if total_messages > 0 {
        (total_messages as f64 * EMAIL_SECONDS_PER_MESSAGE / 60.0).max(0.5)
    } else {
        0.0
    };

    // Already-ingesting short-circuit — same reasoning as the
    // conversation flow (see the long comment above).
    {
        let map = state.install_progress.read().await;
        if let Some(p) = map.get(&corpus_id) {
            if p.phase != "complete" && p.phase != "failed" {
                tracing::info!(
                    target: "imports",
                    corpus_id = %corpus_id,
                    current_phase = %p.phase,
                    "imports: email install short-circuited — daemon already ingesting"
                );
                return Ok(ImportStartResponse::Started {
                    corpus_id: corpus_id.clone(),
                    total_messages,
                    estimated_minutes,
                    canonical_path: source_path.display().to_string(),
                });
            }
        }
    }

    // Destructive-confirm flow, identical gate to the conversation
    // imports — keyed purely on the on-disk index dir.
    let index_dir = email_index_dir()?;
    if index_dir.exists() && index_has_content(&index_dir) {
        if !request.reset_partial {
            return Ok(ImportStartResponse::PartialIndexExists {
                corpus_id,
                index_path: index_dir.display().to_string(),
                total_messages,
                estimated_minutes,
                canonical_path: source_path.display().to_string(),
            });
        }
        tracing::warn!(
            target: "imports",
            index_dir = %index_dir.display(),
            "imports: removing partial email index on user-confirmed reset"
        );
        if let Err(e) = std::fs::remove_dir_all(&index_dir) {
            return Err(format!(
                "could not reset partial index at {}: {e}",
                index_dir.display()
            ));
        }
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("build daemon client: {e}"))?;
    let daemon = state.internal_base_url();
    let url = format!("{daemon}/internal/corpus/install");
    let mut parameters = serde_json::Map::new();
    parameters.insert(
        "path".to_string(),
        serde_json::Value::String(source_path.display().to_string()),
    );
    let resp = client
        .post(&url)
        .json(&serde_json::json!({
            "corpus_id": corpus_id,
            "parameters": parameters,
        }))
        .send()
        .await
        .map_err(|e| {
            tracing::warn!(
                target: "imports",
                error = %e,
                "imports: POST /internal/corpus/install (email) failed"
            );
            "Couldn't reach svrnmesh. Make sure the daemon is running \
             (try `sovereign daemon start`) and click Import again."
                .to_string()
        })?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        tracing::warn!(
            target: "imports",
            status = %status,
            body = %body,
            "imports: daemon /internal/corpus/install (email) returned non-success"
        );
        return Err(format!(
            "svrnmesh rejected the import (HTTP {status}). Check the \
             daemon logs at ~/.svrnmesh/logs/daemon.out for details."
        ));
    }

    let initial = crate::commands::CorpusProgressPayload {
        corpus_id: corpus_id.clone(),
        phase: "downloading".into(),
        percent: 0.0,
        chunks_processed: 0,
        message: Some("Starting…".into()),
        ..Default::default()
    };
    if let Ok(mut map) = state.install_progress.try_write() {
        map.insert(corpus_id.clone(), initial.clone());
    }
    use tauri::Emitter;
    let _ = app_handle.emit("corpus-progress", initial);

    tracing::info!(
        target: "imports",
        corpus_id = %corpus_id,
        total_messages,
        estimated_minutes,
        source = %source_path.display(),
        "imports: email install dispatched"
    );

    Ok(ImportStartResponse::Started {
        corpus_id,
        total_messages,
        estimated_minutes,
        canonical_path: source_path.display().to_string(),
    })
}

/// `~/.svrnmesh/indexes/email-archive` — same convention as
/// [`conversations_index_dir`].
fn email_index_dir() -> Result<PathBuf, String> {
    Ok(sovereign_contracts::rebrand::svrnmesh_root()
        .join("indexes")
        .join(EMAIL_CORPUS_ID))
}

/// Friendly pre-validation: the path must exist, a folder must not be
/// empty, and a file must look like mail (an mbox postmark or an
/// RFC-5322 header line) — content, not extension, so Thunderbird's
/// extensionless stores pass and a mispicked PDF fails with a clear
/// message instead of a cryptic ingest error later.
fn validate_email_source(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err(format!(
            "That path doesn't exist: {}. Pick the exported .mbox file \
             (Gmail Takeout / Apple Mail) or a mail folder.",
            path.display()
        ));
    }
    if path.is_dir() {
        let has_child = std::fs::read_dir(path)
            .map(|rd| {
                rd.flatten()
                    .any(|e| !e.file_name().to_string_lossy().starts_with('.'))
            })
            .unwrap_or(false);
        if !has_child {
            return Err(format!(
                "That folder is empty: {}. Point at a maildir root or a \
                 folder of .eml files.",
                path.display()
            ));
        }
        return Ok(());
    }
    let mut head = [0u8; 1024];
    let n = std::fs::File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let head = &head[..n];
    if head.starts_with(b"From ") {
        return Ok(()); // mbox postmark
    }
    // Single RFC-5322 message: first line is `Header-Name: value`.
    let first_line = head.split(|b| *b == b'\n').next().unwrap_or(head);
    let looks_rfc5322 = first_line
        .iter()
        .position(|b| *b == b':')
        .map(|colon| {
            colon > 0
                && first_line[..colon]
                    .iter()
                    .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
        })
        .unwrap_or(false);
    if looks_rfc5322 {
        return Ok(());
    }
    Err(format!(
        "{} doesn't look like an email archive. Expected an mbox export \
         (Gmail Takeout, Apple Mail) or an .eml message — got neither an \
         mbox `From ` postmark nor an email header.",
        path.display()
    ))
}

/// Best-effort message count for the preflight ETA. mbox: one per
/// unescaped `From ` postmark, streamed so a multi-gigabyte Takeout
/// costs one pass and no memory. Folder: visible files, recursively.
/// Single message file: 1.
fn count_emails(path: &Path) -> std::io::Result<u64> {
    fn count_dir(dir: &Path, n: &mut u64) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)?.flatten() {
            let p = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || name == "Thumbs.db" {
                continue;
            }
            if p.is_dir() {
                count_dir(&p, n)?;
            } else {
                *n += 1;
            }
        }
        Ok(())
    }

    if path.is_dir() {
        let mut n = 0u64;
        count_dir(path, &mut n)?;
        return Ok(n);
    }
    use std::io::BufRead;
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file);
    let mut line: Vec<u8> = Vec::new();
    let mut postmarks = 0u64;
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        if line.starts_with(b"From ") {
            postmarks += 1;
        }
    }
    // Not an mbox (no postmarks) but it validated as mail → one message.
    Ok(postmarks.max(1))
}

#[cfg(test)]
mod tests {

    // ── email import helpers ─────────────────────────────────────

    #[test]
    fn count_emails_counts_mbox_postmarks_not_escaped_ones() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("takeout.mbox");
        std::fs::write(
            &p,
            b"From a@x Mon Jun 1 10:00:00 2026\nFrom: a@x\n\nbody\n>From escaped line\n\nFrom b@x Mon Jun 1 11:00:00 2026\nFrom: b@x\n\nbody two\n",
        )
        .unwrap();
        assert_eq!(count_emails(&p).unwrap(), 2);
    }

    #[test]
    fn count_emails_single_message_file_is_one() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("one.eml");
        std::fs::write(&p, b"From: a@x\nSubject: hi\n\nbody\n").unwrap();
        assert_eq!(count_emails(&p).unwrap(), 1);
    }

    #[test]
    fn count_emails_folder_counts_visible_files_recursively() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("cur");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(dir.path().join("a.eml"), b"From: a@x\n\nb\n").unwrap();
        std::fs::write(sub.join("b.eml"), b"From: b@x\n\nb\n").unwrap();
        std::fs::write(dir.path().join(".hidden"), b"x").unwrap();
        assert_eq!(count_emails(dir.path()).unwrap(), 2);
    }

    #[test]
    fn validate_email_source_accepts_mbox_eml_and_folders() {
        let dir = tempfile::tempdir().unwrap();
        let mbox = dir.path().join("m.mbox");
        std::fs::write(&mbox, b"From a@x Mon Jun 1 10:00:00 2026\nFrom: a@x\n\nb\n").unwrap();
        assert!(validate_email_source(&mbox).is_ok());
        let eml = dir.path().join("no_extension");
        std::fs::write(&eml, b"Received: by mail\nFrom: a@x\n\nb\n").unwrap();
        assert!(validate_email_source(&eml).is_ok());
        assert!(validate_email_source(dir.path()).is_ok()); // non-empty folder
    }

    #[test]
    fn validate_email_source_rejects_non_mail_and_missing() {
        let dir = tempfile::tempdir().unwrap();
        let pdf = dir.path().join("report.pdf");
        std::fs::write(&pdf, b"%PDF-1.7 not mail at all").unwrap();
        let err = validate_email_source(&pdf).unwrap_err();
        assert!(err.contains("doesn't look like an email archive"), "{err}");
        let missing = dir.path().join("nope.mbox");
        let err = validate_email_source(&missing).unwrap_err();
        assert!(err.contains("doesn't exist"), "{err}");
        let empty = dir.path().join("emptydir");
        std::fs::create_dir_all(&empty).unwrap();
        let err = validate_email_source(&empty).unwrap_err();
        assert!(err.contains("empty"), "{err}");
    }

    #[test]
    fn email_index_dir_tracks_corpus_id() {
        let d = email_index_dir().unwrap();
        // Asserted against the SSOT rather than a literal brand token: the
        // root is `~/.svrnmesh` on a migrated machine and `~/.sovereign` on
        // one that has not migrated, so pinning either spelling makes this
        // test pass or fail on the machine, not on the code.
        assert_eq!(
            d,
            sovereign_contracts::rebrand::svrnmesh_root()
                .join("indexes")
                .join("email-archive")
        );
    }
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn build_test_zip(dir: &Path, payload: &[u8]) -> PathBuf {
        let zip_path = dir.join("export.zip");
        let file = fs::File::create(&zip_path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        // Match the Anthropic-nested layout — one folder above
        // conversations.json.
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer
            .start_file("data-deadbeef-batch-0000/conversations.json", options)
            .unwrap();
        writer.write_all(payload).unwrap();
        writer.finish().unwrap();
        zip_path
    }

    #[test]
    fn unpack_finds_nested_conversations_json() {
        let tmp = TempDir::new().unwrap();
        let payload = br#"[{"chat_messages":[{"sender":"human","text":"hi"},{"sender":"assistant","text":"hello"}]}]"#;
        let zip_path = build_test_zip(tmp.path(), payload);
        let dest = tmp.path().join("landing").join("conversations.json");
        fs::create_dir_all(dest.parent().unwrap()).unwrap();

        let result = unpack_conversations_json(&zip_path, &dest).unwrap();
        assert!(result.canonical_path.exists());
        assert_eq!(fs::read(&dest).unwrap(), payload);
        assert_eq!(result.archive_entry_bytes, payload.len() as u64);
    }

    #[test]
    fn unpack_rotates_existing_canonical_file() {
        let tmp = TempDir::new().unwrap();
        let payload = br#"[{"chat_messages":[{"sender":"human","text":"new"}]}]"#;
        let zip_path = build_test_zip(tmp.path(), payload);
        let landing_dir = tmp.path().join("landing");
        fs::create_dir_all(&landing_dir).unwrap();
        let dest = landing_dir.join("conversations.json");
        fs::write(&dest, b"prior contents").unwrap();

        unpack_conversations_json(&zip_path, &dest).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), payload);
        // Prior content survives at a .bak-<ts> sibling.
        let entries: Vec<_> = fs::read_dir(&landing_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(
            entries
                .iter()
                .any(|n| n.starts_with("conversations.json.bak-")),
            "prior canonical must rotate to .bak-<ts>; entries={entries:?}"
        );
    }

    #[test]
    fn unpack_rejects_zip_without_conversations_json() {
        let tmp = TempDir::new().unwrap();
        let zip_path = tmp.path().join("decoy.zip");
        let mut writer = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        writer.start_file("readme.txt", options).unwrap();
        writer.write_all(b"not an export").unwrap();
        writer.finish().unwrap();

        let dest = tmp.path().join("landing.json");
        let err = unpack_conversations_json(&zip_path, &dest).unwrap_err();
        assert!(
            err.contains("conversations.json"),
            "error must name the missing entry: {err}"
        );
    }

    #[test]
    fn count_messages_counts_sender_markers() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("conversations.json");
        let payload = br#"[
          {"chat_messages":[
            {"sender":"human","text":"a"},
            {"sender":"assistant","text":"b"}
          ]},
          {"chat_messages":[
            {"sender":"human","text":"c"}
          ]}
        ]"#;
        fs::write(&path, payload).unwrap();
        assert_eq!(
            count_messages_in_file(&path, ImportSource::Anthropic.count_needle()).unwrap(),
            3
        );
    }

    #[test]
    fn count_messages_counts_author_markers_chatgpt() {
        // ChatGPT message nodes each carry one `"author"` key; the root
        // node (message: null) and conversation scalars do not.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("conversations.json");
        let payload = br#"[{
          "title": "t", "current_node": "u",
          "mapping": {
            "root": {"id": "root", "parent": null, "message": null},
            "u": {"id": "u", "parent": "root", "message": {
              "author": {"role": "user"}, "content": {"content_type": "text", "parts": ["hi"]}
            }},
            "a": {"id": "a", "parent": "u", "message": {
              "author": {"role": "assistant"}, "content": {"content_type": "text", "parts": ["yo"]}
            }}
          }
        }]"#;
        fs::write(&path, payload).unwrap();
        assert_eq!(
            count_messages_in_file(&path, ImportSource::Chatgpt.count_needle()).unwrap(),
            2
        );
    }

    #[test]
    fn import_source_bindings_are_distinct() {
        // The two sources must never collide on corpus id or landing
        // dir — both vendors name the export file `conversations.json`,
        // so a shared dir would clobber on re-import across vendors.
        assert_eq!(
            ImportSource::Anthropic.corpus_id(),
            "conversations-anthropic"
        );
        assert_eq!(ImportSource::Chatgpt.corpus_id(), "conversations-chatgpt");
        assert_ne!(
            ImportSource::Anthropic.canonical_rel_dir(),
            ImportSource::Chatgpt.canonical_rel_dir()
        );
        assert_eq!(ImportSource::Chatgpt.count_needle(), b"\"author\"");
    }

    #[test]
    fn index_dir_tracks_source_corpus_id() {
        // Smoke: the index dir resolves under the source's corpus id.
        let anthropic = conversations_index_dir(ImportSource::Anthropic).unwrap();
        let chatgpt = conversations_index_dir(ImportSource::Chatgpt).unwrap();
        assert!(anthropic.ends_with("conversations-anthropic"));
        assert!(chatgpt.ends_with("conversations-chatgpt"));
    }

    #[test]
    fn index_has_content_recognises_meta_file() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("conversations-anthropic");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("_corpus_meta.json"), b"{\"corpus_id\":\"x\"}").unwrap();
        assert!(index_has_content(&dir));
    }

    #[test]
    fn index_has_content_recognises_lancedb_dir() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("conversations-anthropic");
        fs::create_dir_all(dir.join("chunks.lance")).unwrap();
        assert!(index_has_content(&dir));
    }

    #[test]
    fn index_has_content_rejects_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("conversations-anthropic");
        fs::create_dir_all(&dir).unwrap();
        assert!(!index_has_content(&dir));
    }

    #[test]
    fn index_has_content_rejects_hidden_files_only() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("conversations-anthropic");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(".DS_Store"), b"junk").unwrap();
        assert!(!index_has_content(&dir));
    }
}
