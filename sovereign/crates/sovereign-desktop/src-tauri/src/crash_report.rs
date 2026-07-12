// SPDX-License-Identifier: AGPL-3.0-or-later
//! Durable, local-first crash & panic records.
//!
//! svrnmesh is decentralized: there is no central error pipeline, so a crash on
//! a user's machine has to be *captured where it happens* and made accessible
//! there — a structured record the user can view and, with one click, submit.
//! This module owns the record format and its on-disk store
//! (`~/.sovereign/crashes/*.json`), plus the two capture points:
//!
//! 1. **Rust panics** — [`install_panic_hook`] chains a hook that writes a
//!    record (with a backtrace) before delegating to the default hook.
//! 2. **Native crashes** — a model load/decode that SIGSEGVs runs inside the
//!    crash-isolation subprocess (`crate::smoketest`); the parent survives and
//!    calls [`record_native_crash`] with the signal + captured stderr.
//!
//! Everything here is **best-effort**: capturing a crash must never itself
//! panic or block boot. Write failures are logged and swallowed.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// Bump when the on-disk shape changes incompatibly.
pub const CRASH_SCHEMA: u32 = 1;

/// How the crash was captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CrashKind {
    /// A Rust panic caught by the process-wide hook.
    Panic,
    /// A native crash (SIGSEGV/SIGABRT) observed in the model-probe subprocess.
    NativeCrash,
}

/// One captured crash. Serialized to `~/.sovereign/crashes/<id>.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrashRecord {
    pub schema_version: u32,
    /// Sortable unique id (`<unix_millis>-<seq>`).
    pub id: String,
    pub captured_at_unix: u64,
    pub kind: CrashKind,
    pub app_version: String,
    pub os: String,
    pub cpu_arch: String,
    /// One-line, human-readable summary (shown in the list).
    pub summary: String,
    /// Backtrace (panic) or captured subprocess stderr tail (native crash).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Chat model in play, when known — the usual culprit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_arch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_layers: Option<u32>,
    /// Unix signal number, for native crashes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
}

static SEQ: AtomicU64 = AtomicU64::new(0);

fn now_unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl CrashRecord {
    /// Build a record with the common environment fields filled in.
    pub fn new(kind: CrashKind, summary: impl Into<String>) -> Self {
        let millis = now_unix_millis();
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        CrashRecord {
            schema_version: CRASH_SCHEMA,
            id: format!("{millis}-{seq}"),
            captured_at_unix: millis / 1000,
            kind,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            os: std::env::consts::OS.to_string(),
            cpu_arch: std::env::consts::ARCH.to_string(),
            summary: summary.into(),
            detail: None,
            model_path: None,
            model_arch: None,
            gpu_layers: None,
            signal: None,
        }
    }
}

/// `~/.sovereign/crashes`, creating it if needed. `None` when there's no home
/// dir (a headless/misconfigured environment) — capture degrades to logging.
pub fn crashes_dir() -> Option<PathBuf> {
    let dir = dirs::home_dir()?.join(".sovereign").join("crashes");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        tracing::warn!(error = %e, dir = %dir.display(), "crash_report: could not create crashes dir");
        return None;
    }
    Some(dir)
}

/// Persist a record. Best-effort: logs and returns `false` on any failure so a
/// capture site can stay a one-liner that never itself fails.
pub fn write_record(rec: &CrashRecord) -> bool {
    let Some(dir) = crashes_dir() else {
        // Still surface the crash somewhere.
        tracing::error!(summary = %rec.summary, kind = ?rec.kind, "crash captured (no store available)");
        return false;
    };
    let path = dir.join(format!("{}.json", rec.id));
    let json = match serde_json::to_vec_pretty(rec) {
        Ok(j) => j,
        Err(e) => {
            tracing::error!(error = %e, "crash_report: serialize failed");
            return false;
        }
    };
    // Atomic-ish: write tmp then rename, so a reader never sees a half file.
    let tmp = path.with_extension("json.tmp");
    let ok = std::fs::write(&tmp, &json)
        .and_then(|_| std::fs::rename(&tmp, &path))
        .is_ok();
    if ok {
        tracing::error!(id = %rec.id, kind = ?rec.kind, summary = %rec.summary, path = %path.display(), "crash captured");
    } else {
        tracing::error!(id = %rec.id, "crash_report: write failed");
    }
    ok
}

/// All stored records, newest first. Unreadable files are skipped.
pub fn list_records() -> Vec<CrashRecord> {
    let Some(dir) = crashes_dir() else {
        return Vec::new();
    };
    let mut out: Vec<CrashRecord> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
        .filter_map(|e| std::fs::read(e.path()).ok())
        .filter_map(|b| serde_json::from_slice::<CrashRecord>(&b).ok())
        .collect();
    out.sort_by(|a, b| b.captured_at_unix.cmp(&a.captured_at_unix).then(b.id.cmp(&a.id)));
    out
}

/// Read one record by id.
pub fn read_record(id: &str) -> Option<CrashRecord> {
    let dir = crashes_dir()?;
    let bytes = std::fs::read(dir.join(format!("{id}.json"))).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Delete one record by id (best-effort). Returns whether a file was removed.
pub fn delete_record(id: &str) -> bool {
    let Some(dir) = crashes_dir() else {
        return false;
    };
    std::fs::remove_file(dir.join(format!("{id}.json"))).is_ok()
}

/// Record a native crash observed in the model-probe subprocess.
pub fn record_native_crash(
    summary: impl Into<String>,
    model_path: Option<String>,
    model_arch: Option<String>,
    gpu_layers: Option<u32>,
    signal: Option<i32>,
    stderr_tail: Option<String>,
) {
    let mut rec = CrashRecord::new(CrashKind::NativeCrash, summary);
    rec.model_path = model_path;
    rec.model_arch = model_arch;
    rec.gpu_layers = gpu_layers;
    rec.signal = signal;
    rec.detail = stderr_tail;
    write_record(&rec);
}

/// Install a process-wide panic hook that captures a [`CrashRecord`] (with a
/// backtrace) then chains to the previously-installed hook (so the default
/// logging/abort behaviour is preserved). Call once, early in `main`.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Keep this defensive — we're already unwinding.
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic (non-string payload)".to_string());
        let loc = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown location>".to_string());
        let mut rec = CrashRecord::new(CrashKind::Panic, format!("panic at {loc}: {msg}"));
        rec.detail = Some(std::backtrace::Backtrace::force_capture().to_string());
        write_record(&rec);
        previous(info);
    }));
}

// ── Tauri command surface (in-app Diagnostics) ───────────────────────────

/// List captured crash records, newest first — the in-app Diagnostics view.
#[tauri::command]
pub fn list_crash_records() -> Vec<CrashRecord> {
    list_records()
}

/// Full detail for one record (backtrace / stderr).
#[tauri::command]
pub fn read_crash_record(id: String) -> Option<CrashRecord> {
    read_record(&id)
}

/// Discard a record the user doesn't want to keep or share.
#[tauri::command]
pub fn delete_crash_record(id: String) -> bool {
    delete_record(&id)
}

/// Result of [`export_crash_record`].
#[derive(Debug, Clone, Serialize)]
pub struct ExportedCrashRecord {
    /// Path to the redacted, human-readable report written to the Desktop.
    pub report_path: String,
    /// GitHub Issues URL the frontend hands to `tauri-plugin-shell::open`.
    pub issues_url: String,
}

/// One-click "share this crash": write a **redacted**, human-readable copy of a
/// record to the user's Desktop and return its path + the GitHub Issues URL.
/// Nothing is uploaded — the user reads the file (every byte visible) and
/// attaches it to an issue they open. Mirrors [`crate::crash_bundle`]'s
/// daemon-crash flow: local-first, user-initiated, no surprise egress.
#[tauri::command]
pub fn export_crash_record(id: String) -> Result<ExportedCrashRecord, String> {
    let rec = read_record(&id).ok_or_else(|| format!("crash record {id} not found"))?;
    let dir = crate::crash_bundle::desktop_dir()
        .ok_or_else(|| "could not resolve a directory to write the report".to_string())?;
    let path = dir.join(format!("svrnmesh-crash-{id}.md"));
    std::fs::write(&path, render_shareable_markdown(&rec))
        .map_err(|e| format!("failed to write crash report: {e}"))?;
    Ok(ExportedCrashRecord {
        report_path: path.display().to_string(),
        issues_url: crate::crash_bundle::issues_url(),
    })
}

/// Render a record as markdown for sharing, redacting the model path to its
/// basename (no home dir / absolute paths leak).
fn render_shareable_markdown(rec: &CrashRecord) -> String {
    fn basename(p: &str) -> &str {
        std::path::Path::new(p)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(p)
    }
    let mut s = String::new();
    s.push_str("# svrnmesh crash report\n\n");
    s.push_str(&format!("- kind: {:?}\n", rec.kind));
    s.push_str(&format!("- captured_at_unix: {}\n", rec.captured_at_unix));
    s.push_str(&format!("- app_version: {}\n", rec.app_version));
    s.push_str(&format!("- os / cpu_arch: {} / {}\n", rec.os, rec.cpu_arch));
    if let Some(m) = &rec.model_path {
        s.push_str(&format!("- model: {}\n", basename(m)));
    }
    if let Some(a) = &rec.model_arch {
        s.push_str(&format!("- model_arch: {a}\n"));
    }
    if let Some(g) = rec.gpu_layers {
        s.push_str(&format!("- gpu_layers: {g}\n"));
    }
    if let Some(sig) = rec.signal {
        s.push_str(&format!("- signal: {sig}\n"));
    }
    s.push_str(&format!("\n## Summary\n\n{}\n", rec.summary));
    if let Some(d) = &rec.detail {
        s.push_str(&format!("\n## Detail\n\n```\n{d}\n```\n"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    // Route the store into a tempdir via HOME (crashes_dir derives from it).
    fn with_home<T>(dir: &std::path::Path, f: impl FnOnce() -> T) -> T {
        // Crate-wide HOME lock, shared with smoketest's cache tests — env is
        // process-global, so a per-module mutex is no real guard. See
        // `crate::test_support`.
        let _g = crate::test_support::HOME_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let old = std::env::var_os("HOME");
        std::env::set_var("HOME", dir);
        let out = f();
        match old {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        out
    }

    #[test]
    fn write_list_read_delete_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        with_home(tmp.path(), || {
            let mut rec = CrashRecord::new(CrashKind::NativeCrash, "boom on qwen35");
            rec.model_arch = Some("qwen35".into());
            rec.signal = Some(11);
            assert!(write_record(&rec));

            let all = list_records();
            assert_eq!(all.len(), 1);
            assert_eq!(all[0].summary, "boom on qwen35");
            assert_eq!(all[0].signal, Some(11));

            let got = read_record(&rec.id).expect("record readable by id");
            assert_eq!(got.model_arch.as_deref(), Some("qwen35"));

            assert!(delete_record(&rec.id));
            assert!(list_records().is_empty());
        });
    }

    #[test]
    fn ids_are_unique_and_sorted_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        with_home(tmp.path(), || {
            for i in 0..3 {
                write_record(&CrashRecord::new(CrashKind::Panic, format!("p{i}")));
            }
            let all = list_records();
            assert_eq!(all.len(), 3, "three distinct ids");
            // Strictly non-increasing capture order (newest first).
            for w in all.windows(2) {
                assert!(w[0].id >= w[1].id);
            }
        });
    }
}
