//! Tauri commands powering the Settings → Imports tab.
//!
//! v1 ships the **Anthropic** path only: the user picks the
//! `data-<uuid>-…batch-0000.zip` Anthropic produces from
//! Settings → Privacy → Export data; this module unpacks the
//! `conversations.json` entry into the canonical landing path
//! (`~/.sovereign/conversations/conversations.json`), counts
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

const DAEMON_INTERNAL_URL: &str = "http://127.0.0.1:9742";

/// Anthropic-export landing zone. Matches the `[acquire] path`
/// declared by `sovereign-recipes/conversations-anthropic/recipe.toml`.
/// Resolved at runtime so the Imports tab works regardless of where
/// `~` resolves on the host.
const CANONICAL_REL_DIR: &str = ".sovereign/conversations";
const CANONICAL_FILE: &str = "conversations.json";

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
pub struct ImportAnthropicZipRequest {
    pub zip_path: PathBuf,
    /// `true` ⇒ wipe any existing
    /// `~/.sovereign/indexes/conversations-anthropic/` directory
    /// before posting the install. The UI sends this on the second
    /// invocation after the user confirms the destructive prompt.
    #[serde(default)]
    pub reset_partial: bool,
}

/// Tauri command: unpack the Anthropic export the user picked,
/// land its `conversations.json` at the canonical path the
/// `conversations-anthropic` recipe reads from, and kick off the
/// daemon install. The progress + ETA UX after this returns is
/// driven by the existing `corpus-progress` event stream — this
/// command is just the entry hop.
#[tauri::command]
pub async fn import_anthropic_zip(
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
    request: ImportAnthropicZipRequest,
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
                "Imports expects an Anthropic export .zip; got {}",
                zip_path
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("<no extension>")
            ));
        }
    }

    let canonical_path = canonical_landing_path()?;
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
        count_messages_in_file(&extracted_bytes.canonical_path).unwrap_or_else(|e| {
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

    let corpus_id = "conversations-anthropic".to_string();

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
    let index_dir = conversations_anthropic_index_dir()?;
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
    let url = format!("{DAEMON_INTERNAL_URL}/internal/corpus/install");
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
            "Couldn't reach Sovereign. Make sure the daemon is running \
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
            "Sovereign rejected the import (HTTP {status}). Check the \
             daemon logs at ~/.sovereign/logs/daemon.out for details."
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

/// Resolve the canonical on-disk index dir for the
/// `conversations-anthropic` corpus. Mirrors the path
/// `~/.sovereign/indexes/<corpus_id>` the daemon's `CorpusEngine`
/// uses by convention (see `state.rs::build_app_state`).
fn conversations_anthropic_index_dir() -> Result<PathBuf, String> {
    let home =
        dirs::home_dir().ok_or_else(|| "HOME is not set; cannot resolve index dir".to_string())?;
    Ok(home
        .join(".sovereign")
        .join("indexes")
        .join("conversations-anthropic"))
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

fn canonical_landing_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir()
        .ok_or_else(|| "HOME is not set; cannot resolve ~/.sovereign/conversations/".to_string())?;
    let dir = home.join(CANONICAL_REL_DIR);
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
                "no `{CANONICAL_FILE}` entry inside {}; is this an Anthropic export?",
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

/// Counts the total `chat_messages` across every conversation in
/// the export. Reads the file in chunks rather than slurping it
/// whole — a Claude power-user export is easily 100+ MB.
///
/// "Best effort": counts occurrences of `"sender"` field markers,
/// which appear exactly once per message in the Anthropic schema
/// (per `corpus-engine/src/extractors/anthropic_export.rs`). Wrong
/// by at most a handful when the dataset embeds the word "sender"
/// inside message text — the pre-flight ETA is a `±30%` band
/// anyway, so a few stray matches don't matter.
fn count_messages_in_file(path: &Path) -> Result<u64, String> {
    let file = fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut reader = std::io::BufReader::new(file);
    let needle = b"\"sender\"";
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

#[cfg(test)]
mod tests {
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
        assert_eq!(count_messages_in_file(&path).unwrap(), 3);
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
