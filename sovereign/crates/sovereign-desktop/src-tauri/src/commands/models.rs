//! Auto-split from the former monolithic `commands.rs` (PR5). Tauri
//! command handlers grouped by concern; re-exported through
//! `commands/mod.rs` so `commands::<name>` paths in `main.rs`'s
//! `generate_handler!` stay valid.
#![allow(unused_imports)]
use super::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};
use tokio::io::AsyncWriteExt;

use crate::error::DesktopError;
use crate::state::{self, AppState, DesktopConfig};

// ─── Web Search ─────────────────────────────────────────────

#[tauri::command]
pub async fn search_web(
    state: State<'_, Arc<AppState>>,
    query: String,
    conversation_id: String,
) -> Result<MessageResponse, DesktopError> {
    let runtime = state.runtime().await?;

    // Save user message.
    let user_msg = sovereign_core::types::Message {
        id: uuid::Uuid::new_v4().to_string(),
        conversation_id: conversation_id.clone(),
        role: sovereign_core::types::Role::User,
        content: query.clone(),
        created_at: now_epoch(),
        metadata: None,
        version: now_epoch(),
    };
    runtime
        .store
        .save_message(&user_msg)
        .await
        .map_err(|e| e.to_string())?;

    // Execute search tool directly.
    let tool = runtime
        .tools
        .get("search")
        .or(runtime.tools.get("web_search"))
        .map_err(|_| DesktopError::invalid_request("Search tool is not enabled."))?;

    let params = serde_json::json!({ "query": query });
    let ctx = sovereign_core::types::ToolContext {
        conversation_id: conversation_id.clone(),
        task_id: None,
        working_directory: None,
        in_reasoning_loop: false,
        agent_session_token: None,
        turn_index: 0,
    };

    let output = tool
        .execute(&params, &ctx)
        .await
        .map_err(|e| DesktopError::upstream(format!("Web search failed: {e}")))?;

    let content = match output {
        sovereign_core::types::StepOutput::Text(t) => t,
        sovereign_core::types::StepOutput::Json(ref v) => v
            .get("answer")
            .and_then(|a| a.as_str())
            .unwrap_or("No results found.")
            .to_string(),
        sovereign_core::types::StepOutput::ReasonWithToolsResult { text, .. } => text,
        _ => "No results found.".to_string(),
    };

    // Save assistant message.
    let msg_id = uuid::Uuid::new_v4().to_string();
    let assistant_msg = sovereign_core::types::Message {
        id: msg_id.clone(),
        conversation_id,
        role: sovereign_core::types::Role::Assistant,
        content: content.clone(),
        created_at: now_epoch(),
        metadata: None,
        version: now_epoch(),
    };
    runtime
        .store
        .save_message(&assistant_msg)
        .await
        .map_err(|e| e.to_string())?;

    Ok(MessageResponse {
        message_id: msg_id,
        role: "assistant".to_string(),
        content,
        task: None,
        metadata: None,
    })
}

// ─── Model Discovery & Download ─────────────────────────────

#[derive(Serialize)]
pub struct DiscoveredModel {
    pub path: String,
    pub file_name: String,
    pub size_bytes: u64,
    pub location_label: String,
}

#[derive(Deserialize)]
pub struct DownloadRequest {
    pub url: String,
    pub file_name: String,
    /// Advertised file size from the model catalogue (models.toml
    /// / ModelSelector.svelte's EMBED_MODELS). Optional for back-
    /// compat; when present, `download_model` applies a 50% floor
    /// via `sovereign_inference::GgufExpectation::from_size_gb`
    /// so a CDN-served 200 KB HTML stub doesn't silently land at
    /// the final path as a "30 GB" model.
    #[serde(default)]
    pub size_gb: Option<f64>,
}

#[derive(Serialize, Clone)]
pub struct DownloadProgress {
    pub file_name: String,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub percent: Option<f64>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Returns true iff `path` starts with the GGUF magic (`GGUF`, 4 bytes
/// ASCII).
///
/// Discovery scanners list every `*.gguf` file by extension, but a
/// failed download (HTML 404 page, captive-portal interstitial, Git-LFS
/// pointer) saved with a `.gguf` extension still slips through. The
/// picker would surface those as selectable options and the user would
/// land on `state::bootstrap` → `LlamaModel::load_from_file` → "null
/// result from llama cpp" with no path back. The magic-byte check is a
/// 4-byte read — much cheaper than `validate_gguf` (which also size-
/// checks) and sufficient to weed out non-GGUFs at discovery time.
fn looks_like_gguf(path: &Path) -> bool {
    use std::io::Read;
    match std::fs::File::open(path) {
        Ok(mut f) => {
            let mut buf = [0u8; 4];
            f.read_exact(&mut buf).is_ok() && &buf == b"GGUF"
        }
        Err(_) => false,
    }
}

fn scan_directory_flat(
    dir: &Path,
    label: &str,
    results: &mut Vec<DiscoveredModel>,
    seen: &mut HashSet<PathBuf>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "gguf") {
            if !looks_like_gguf(&path) {
                tracing::warn!(
                    path = %path.display(),
                    "scan_directory_flat: skipping non-GGUF (likely failed download or LFS pointer)"
                );
                continue;
            }
            if let Ok(canonical) = path.canonicalize() {
                if seen.insert(canonical.clone()) {
                    let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    results.push(DiscoveredModel {
                        path: canonical.display().to_string(),
                        file_name: path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string(),
                        size_bytes: size,
                        location_label: label.to_string(),
                    });
                }
            }
        }
    }
}

fn scan_directory_deep(
    dir: &Path,
    label: &str,
    max_depth: usize,
    results: &mut Vec<DiscoveredModel>,
    seen: &mut HashSet<PathBuf>,
) {
    if !dir.exists() {
        return;
    }
    for entry in walkdir::WalkDir::new(dir)
        .max_depth(max_depth)
        .into_iter()
        .flatten()
    {
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "gguf") {
            if !looks_like_gguf(path) {
                tracing::warn!(
                    path = %path.display(),
                    "scan_directory_deep: skipping non-GGUF (likely failed download or LFS pointer)"
                );
                continue;
            }
            if let Ok(canonical) = path.canonicalize() {
                if seen.insert(canonical.clone()) {
                    let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    results.push(DiscoveredModel {
                        path: canonical.display().to_string(),
                        file_name: path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string(),
                        size_bytes: size,
                        location_label: label.to_string(),
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod scan_tests {
    use super::looks_like_gguf;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn looks_like_gguf_accepts_real_magic() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("real.gguf");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"GGUF\x03\x00\x00\x00rest of file...").unwrap();
        assert!(looks_like_gguf(&path));
    }

    #[test]
    fn looks_like_gguf_rejects_html() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("stub.gguf");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"<!doctype html><html>404 Not Found</html>")
            .unwrap();
        assert!(!looks_like_gguf(&path));
    }

    #[test]
    fn looks_like_gguf_rejects_lfs_pointer() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("ptr.gguf");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"version https://git-lfs.github.com/spec/v1\noid sha256:abc\nsize 12345\n")
            .unwrap();
        assert!(!looks_like_gguf(&path));
    }

    #[test]
    fn looks_like_gguf_rejects_too_short_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("short.gguf");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"GG").unwrap();
        assert!(!looks_like_gguf(&path));
    }

    #[test]
    fn looks_like_gguf_rejects_missing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("missing.gguf");
        assert!(!looks_like_gguf(&path));
    }
}

#[cfg(test)]
mod delete_tests {
    use super::delete_model_blocking;
    use tempfile::tempdir;

    fn touch(path: &std::path::Path) {
        std::fs::write(path, b"GGUF\x00\x00\x00\x00").unwrap();
    }

    #[test]
    fn deletes_gguf_under_root() {
        let root = tempdir().unwrap();
        let file = root.path().join("model.gguf");
        touch(&file);
        let roots = vec![root.path().to_path_buf()];
        assert!(delete_model_blocking(&file, &[], &roots).is_ok());
        assert!(!file.exists(), "the model file should be gone");
    }

    #[test]
    fn rejects_non_gguf() {
        let root = tempdir().unwrap();
        let file = root.path().join("notes.txt");
        touch(&file);
        let roots = vec![root.path().to_path_buf()];
        let err = delete_model_blocking(&file, &[], &roots).unwrap_err();
        assert!(err.contains(".gguf"));
        assert!(file.exists(), "a non-gguf must never be deleted");
    }

    #[test]
    fn rejects_file_outside_roots() {
        let root = tempdir().unwrap();
        let elsewhere = tempdir().unwrap();
        let file = elsewhere.path().join("model.gguf");
        touch(&file);
        let roots = vec![root.path().to_path_buf()];
        let err = delete_model_blocking(&file, &[], &roots).unwrap_err();
        assert!(err.contains("outside"));
        assert!(file.exists(), "a file outside the known roots must never be deleted");
    }

    #[test]
    fn rejects_assigned_model() {
        let root = tempdir().unwrap();
        let file = root.path().join("primary.gguf");
        touch(&file);
        let roots = vec![root.path().to_path_buf()];
        let assigned = vec![file.clone()];
        let err = delete_model_blocking(&file, &assigned, &roots).unwrap_err();
        assert!(err.contains("assigned"));
        assert!(file.exists(), "a slot-assigned model must never be deleted");
    }
}

/// Returns `Some(bytes)` for an existing file path, `None` if the path
/// is empty or the file does not exist. Errors are reserved for genuine
/// IO failures (permissions etc.) so the UI can distinguish "user
/// hasn't picked this slot yet" from "the file vanished" — both render
/// as "—" but only the second is worth a console warning. Used by the
/// Settings → Models budget meter to estimate peak memory before the
/// user saves a combination that would crash the daemon at load time.
#[tauri::command]
pub async fn model_file_size(path: String) -> Result<Option<u64>, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let p = PathBuf::from(trimmed);
    tokio::task::spawn_blocking(move || match std::fs::metadata(&p) {
        Ok(meta) if meta.is_file() => Ok(Some(meta.len())),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("stat failed: {e}")),
    })
    .await
    .map_err(|e| format!("model_file_size join error: {e}"))?
}

#[tauri::command]
pub async fn scan_for_models() -> Result<Vec<DiscoveredModel>, String> {
    // Run filesystem scanning on a blocking thread.
    tokio::task::spawn_blocking(|| {
        let mut results = Vec::new();
        let mut seen = HashSet::new();
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));

        // Priority 1: Sovereign models directory
        let sovereign_models = home.join(".sovereign").join("models");
        scan_directory_flat(
            &sovereign_models,
            "Sovereign Models",
            &mut results,
            &mut seen,
        );

        // Priority 2: Local models/ directory
        let local_models = std::env::current_dir().unwrap_or_default().join("models");
        scan_directory_flat(&local_models, "Local Models", &mut results, &mut seen);

        // Priority 3: HuggingFace cache (deep scan, GGUF files nested in snapshots)
        let hf_cache = home.join(".cache").join("huggingface").join("hub");
        scan_directory_deep(&hf_cache, "HuggingFace Cache", 5, &mut results, &mut seen);

        // Priority 4: Downloads folder
        let downloads = home.join("Downloads");
        scan_directory_flat(&downloads, "Downloads", &mut results, &mut seen);

        Ok(results)
    })
    .await
    .map_err(|e| format!("Scan failed: {e}"))?
}

/// The directories the model scanner walks — and therefore the ONLY
/// places `delete_model` will remove a file from. Kept in lock-step with
/// `scan_for_models` above so anything the user can see, they can delete,
/// and nothing outside these roots can ever be targeted.
fn model_scan_roots() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    vec![
        home.join(".sovereign").join("models"),
        std::env::current_dir().unwrap_or_default().join("models"),
        home.join(".cache").join("huggingface").join("hub"),
        home.join("Downloads"),
    ]
}

/// Delete a GGUF model file from disk to reclaim space. Destructive, so
/// it is guarded three ways: (1) the path must be a real `.gguf` file;
/// (2) it must resolve (canonicalised, symlinks followed) to somewhere
/// under a known model-scan root — this command can never be coaxed into
/// deleting an arbitrary file; (3) it must NOT be the file currently
/// assigned to any chat slot, since deleting an in-use model would break
/// the next runtime build. The UI additionally requires a two-click
/// confirm. Returns a human-readable error (surfaced to the user) on any
/// guard failure.
#[tauri::command]
pub async fn delete_model(
    state: State<'_, Arc<AppState>>,
    path: String,
) -> Result<(), String> {
    let target = PathBuf::from(path.trim());
    // Snapshot the assigned slot paths for the in-use guard (cheap clone).
    let assigned: Vec<PathBuf> = {
        let cfg = state.config.read().await;
        [
            Some(cfg.model_path.clone()),
            cfg.primary_model_path.clone(),
            cfg.embed_model_path.clone(),
            cfg.code_model_path.clone(),
        ]
        .into_iter()
        .flatten()
        .collect()
    };
    let roots = model_scan_roots();
    tokio::task::spawn_blocking(move || delete_model_blocking(&target, &assigned, &roots))
        .await
        .map_err(|e| format!("delete_model join error: {e}"))?
}

fn delete_model_blocking(
    target: &Path,
    assigned: &[PathBuf],
    roots: &[PathBuf],
) -> Result<(), String> {
    // (1) .gguf only.
    let is_gguf = target
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("gguf"));
    if !is_gguf {
        return Err("Refusing to delete: not a .gguf model file.".into());
    }
    let canonical =
        std::fs::canonicalize(target).map_err(|e| format!("Cannot resolve that path: {e}"))?;
    if !canonical.is_file() {
        return Err("Refusing to delete: not a regular file.".into());
    }
    // (2) Must live under a known model-scan root.
    let under_root = roots
        .iter()
        .filter_map(|r| std::fs::canonicalize(r).ok())
        .any(|r| canonical.starts_with(&r));
    if !under_root {
        return Err("Refusing to delete: that file is outside the known model folders.".into());
    }
    // (3) Must not be assigned to a slot.
    for a in assigned {
        if std::fs::canonicalize(a)
            .map(|ac| ac == canonical)
            .unwrap_or(false)
        {
            return Err(
                "This model is assigned to a slot. Clear it in Settings → Models first, \
                 then delete."
                    .into(),
            );
        }
    }
    std::fs::remove_file(&canonical).map_err(|e| format!("Delete failed: {e}"))?;
    Ok(())
}

#[tauri::command]
pub async fn download_model(
    app_handle: tauri::AppHandle,
    request: DownloadRequest,
) -> Result<String, String> {
    let home = dirs::home_dir().ok_or("Cannot determine home directory")?;
    let models_dir = home.join(".sovereign").join("models");
    std::fs::create_dir_all(&models_dir)
        .map_err(|e| format!("Failed to create models directory: {e}"))?;

    let dest = models_dir.join(&request.file_name);
    let expected = match request.size_gb {
        Some(gb) => sovereign_inference::GgufExpectation::from_size_gb(gb),
        None => sovereign_inference::GgufExpectation::unknown(),
    };

    // Validate any pre-existing file at the destination. A stub
    // from a previous bad download (HTML error page, truncated
    // stream) must be deleted — the old early-return-on-exists
    // behaviour locked users into re-running setup from a clean
    // slate. Now we just re-download whatever's invalid.
    if dest.exists() {
        match sovereign_inference::validate_gguf(&dest, &expected) {
            Ok(()) => {
                let size = dest.metadata().map(|m| m.len()).unwrap_or(0);
                let _ = app_handle.emit(
                    "download-progress",
                    DownloadProgress {
                        file_name: request.file_name,
                        downloaded_bytes: size,
                        total_bytes: Some(size),
                        percent: Some(100.0),
                        status: "complete".to_string(),
                        error: None,
                    },
                );
                return Ok(dest.display().to_string());
            }
            Err(e) => {
                tracing::warn!(
                    path = %dest.display(),
                    reason = %e,
                    "download_model: existing file failed validation, redownloading"
                );
                let _ = std::fs::remove_file(&dest);
            }
        }
    }

    let part_path = models_dir.join(format!("{}.part", &request.file_name));

    // Build the request with optional HF_TOKEN bearer auth.
    // Authenticated HF requests bypass anonymous rate-limits and
    // the CDN's bot-detection paths that return HTML.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60 * 60))
        .build()
        .map_err(|e| format!("Build http client: {e}"))?;
    let mut req = client.get(&request.url);
    if let Ok(tok) = std::env::var("HF_TOKEN") {
        if !tok.is_empty() {
            req = req.bearer_auth(tok);
        }
    }
    let response = req
        .send()
        .await
        .map_err(|e| format!("Download request failed: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("Download failed: HTTP {}", response.status()));
    }

    // Pre-stream content-type sniff. When HuggingFace returns an
    // error page (rate limit, bot detection, gated repo), the
    // body is HTML or JSON — catch it before streaming MB of
    // garbage to disk. The post-stream `validate_gguf` check
    // backstops this for cases where the server lies about
    // content-type.
    if let Some(ct) = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    {
        let lower = ct.to_ascii_lowercase();
        if lower.starts_with("text/") || lower.starts_with("application/json") {
            return Err(format!(
                "HuggingFace returned content-type={ct} for {} — likely \
                 bot-detection, rate limiting, or a gated-repo login page. \
                 Set HF_TOKEN and retry, or try a different model.",
                request.url
            ));
        }
    }

    let total_bytes = response.content_length();
    let mut downloaded: u64 = 0;
    let mut file = tokio::fs::File::create(&part_path)
        .await
        .map_err(|e| format!("Failed to create file: {e}"))?;

    let mut stream = response.bytes_stream();

    let mut last_emit: u64 = 0;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Download error: {e}"))?;
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("Write error: {e}"))?;
        downloaded += chunk.len() as u64;

        // Emit progress every ~200KB.
        if downloaded - last_emit >= 200_000 {
            last_emit = downloaded;
            let _ = app_handle.emit(
                "download-progress",
                DownloadProgress {
                    file_name: request.file_name.clone(),
                    downloaded_bytes: downloaded,
                    total_bytes,
                    percent: total_bytes.map(|t| (downloaded as f64 / t as f64) * 100.0),
                    status: "downloading".to_string(),
                    error: None,
                },
            );
        }
    }

    file.flush()
        .await
        .map_err(|e| format!("Flush error: {e}"))?;
    drop(file);

    // Post-stream validation. Covers CDN responses that advertised
    // `application/octet-stream` but delivered HTML, silent TCP
    // resets mid-body, and any other way a response can look
    // successful but not actually be a GGUF. On failure we delete
    // the `.part` so a retry starts clean rather than resuming a
    // partial bogus file.
    if let Err(e) = sovereign_inference::validate_gguf(&part_path, &expected) {
        let _ = tokio::fs::remove_file(&part_path).await;
        let msg = format!("download validation failed: {e}");
        let _ = app_handle.emit(
            "download-progress",
            DownloadProgress {
                file_name: request.file_name.clone(),
                downloaded_bytes: downloaded,
                total_bytes,
                percent: None,
                status: "error".to_string(),
                error: Some(msg.clone()),
            },
        );
        return Err(msg);
    }

    // Rename .part to final.
    tokio::fs::rename(&part_path, &dest)
        .await
        .map_err(|e| format!("Failed to finalize download: {e}"))?;

    let _ = app_handle.emit(
        "download-progress",
        DownloadProgress {
            file_name: request.file_name,
            downloaded_bytes: downloaded,
            total_bytes: Some(downloaded),
            percent: Some(100.0),
            status: "complete".to_string(),
            error: None,
        },
    );

    Ok(dest.display().to_string())
}
