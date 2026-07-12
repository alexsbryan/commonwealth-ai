// SPDX-License-Identifier: AGPL-3.0-or-later
//! Parent-side helpers for crash-isolated model smoke testing.
//!
//! The actual `LlamaBackend::init` + load + decode happens in
//! `sovereign_inference::smoketest`. This module owns:
//!
//! - The thin call-from-`main` bridge that detects the
//!   `--smoketest` flag and delegates to the library entry point.
//! - The parent-side `run_in_subprocess` helper that spawns a
//!   child copy of the desktop binary with the smoketest flag
//!   and interprets its exit status.
//!
//! The split keeps `llama-cpp-2` out of this crate's dep graph
//! while still letting the desktop binary serve as both parent
//! and smoketest worker (using `current_exe()` re-exec).

use std::process::ExitCode;
use std::time::{Duration, Instant};

use sovereign_inference::smoketest as inference_smoketest;

pub use sovereign_inference::smoketest::SMOKETEST_FLAG;

/// Inspect `argv` for the smoketest flag. When found, run the
/// smoketest (which loads a model + does a 1-token decode) and
/// return the resulting `ExitCode` for `main()` to propagate.
/// Returns `None` when the flag wasn't present and the caller
/// should proceed with normal Tauri startup.
pub fn detect_and_run(argv: &[String]) -> Option<ExitCode> {
    inference_smoketest::run_from_argv(argv)
}

/// Outcome of running the smoketest as a child of the desktop
/// process.
#[derive(Debug)]
pub enum SmokeResult {
    /// Child exited 0. Model is safe to load in-process.
    Ok,
    /// Child died from a signal (SIGSEGV most likely on the
    /// kernel-pipeline-nil bugs we're guarding against). The
    /// `signal` is the Unix signal number. Treat as "this combo
    /// crashes — fall back to a safer config".
    Crashed { signal: i32 },
    /// Child exited non-zero through a Rust error path. The
    /// model fundamentally couldn't load (missing file, corrupt
    /// gguf, etc.) — CPU fallback won't help.
    Failed { exit_code: i32 },
    /// Child didn't finish within the allowed budget. Parent
    /// killed it. Treat as "broken / hung" — fall back.
    Timeout,
    /// Couldn't spawn the child or read its status. Skip the
    /// smoketest and load in-process — don't gate the user's app
    /// on the meta-failure of our own infrastructure.
    Skipped { reason: String },
}

impl SmokeResult {
    /// True when the result indicates the GPU configuration
    /// crashed and a CPU fallback is worth trying. False for
    /// `Ok`, plain `Failed` (Rust error — won't help to retry),
    /// and `Skipped` (we don't know).
    pub fn suggests_cpu_fallback(&self) -> bool {
        matches!(self, SmokeResult::Crashed { .. } | SmokeResult::Timeout)
    }
}

impl std::fmt::Display for SmokeResult {
    /// Glassbox surface for `tracing::error!(outcome = %res, ...)`.
    /// Surfaces the diagnostic payloads that `#[derive(Debug)]`
    /// produces — but in a stable, parseable shape an operator can
    /// grep. Per ARCH_PRINCIPLES §9.1, every non-obvious
    /// fall-back decision (smoketest fail → CPU fallback) names
    /// the *reason* in the log line so support requests don't
    /// require digging into the hex dump.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SmokeResult::Ok => write!(f, "ok"),
            SmokeResult::Crashed { signal } => {
                write!(f, "crashed(signal={signal})")
            }
            SmokeResult::Failed { exit_code } => {
                write!(f, "failed(exit_code={exit_code})")
            }
            SmokeResult::Timeout => write!(f, "timeout"),
            SmokeResult::Skipped { reason } => {
                write!(f, "skipped({reason})")
            }
        }
    }
}

/// Run the smoketest as a subprocess of the current desktop
/// binary. Returns the outcome; does NOT decide what to do with
/// it — caller chooses CPU fallback / abort / surface to UI.
///
/// `timeout` should be generous: cold-loading a 5 GB GGUF over
/// Metal can take 5-15 s depending on disk and architecture.
/// 60 s is a reasonable default.
pub fn run_in_subprocess(
    model_path: &std::path::Path,
    n_gpu_layers: u32,
    n_ctx: u32,
    timeout: Duration,
) -> SmokeResult {
    use std::process::{Command, Stdio};

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            return SmokeResult::Skipped {
                reason: format!("current_exe: {e}"),
            }
        }
    };

    let mut child = match Command::new(&exe)
        .arg(SMOKETEST_FLAG)
        .arg("--model")
        .arg(model_path)
        .arg("--gpu-layers")
        .arg(n_gpu_layers.to_string())
        .arg("--ctx")
        .arg(n_ctx.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return SmokeResult::Skipped {
                reason: format!("spawn: {e}"),
            }
        }
    };

    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return SmokeResult::Ok;
                }
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    if let Some(sig) = status.signal() {
                        return SmokeResult::Crashed { signal: sig };
                    }
                }
                let code = status.code().unwrap_or(-1);
                // Non-zero exit code without a signal: Rust-side
                // failure, model load was rejected cleanly.
                return SmokeResult::Failed { exit_code: code };
            }
            Ok(None) => {
                if started.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return SmokeResult::Timeout;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => {
                return SmokeResult::Skipped {
                    reason: format!("wait: {e}"),
                };
            }
        }
    }
}

// ── Verdict cache ───────────────────────────────────────────────────
//
// The probe exists to catch NEW (model × GPU backend) combos that
// SIGSEGV in-process — but it fully loads the fast-slot GGUF in the
// child every boot (~3-6s on a 9B-class model), and the answer for an
// unchanged combo never changes. Cache passing verdicts keyed by the
// exact inputs that could change the answer: model file identity
// (path + size + mtime), gpu_layers, probe ctx, and the app version
// (a llama.cpp bump can change crash behaviour). Any change → full
// re-probe. Failures are deliberately NOT cached: a crashed combo
// re-probes every boot so a fixed driver gets the GPU path back
// without the user knowing a cache exists.
//
// Env: `SOVEREIGN_SMOKETEST_CACHE=0` disables (probe every boot).

const VERDICT_SCHEMA: u32 = 1;

#[derive(serde::Serialize, serde::Deserialize)]
struct VerdictFile {
    schema_version: u32,
    /// model path → the passing configuration.
    entries: std::collections::HashMap<String, OkVerdict>,
}

#[derive(serde::Serialize, serde::Deserialize, PartialEq)]
struct OkVerdict {
    size_bytes: u64,
    mtime_ms: u64,
    gpu_layers: u32,
    n_ctx: u32,
    app_version: String,
    passed_at_unix: u64,
}

fn verdict_cache_path() -> Option<std::path::PathBuf> {
    if std::env::var("SOVEREIGN_SMOKETEST_CACHE").as_deref() == Ok("0") {
        return None;
    }
    dirs::home_dir().map(|h| h.join(".sovereign").join("smoketest-cache.json"))
}

fn current_verdict(
    model_path: &std::path::Path,
    n_gpu_layers: u32,
    n_ctx: u32,
) -> Option<OkVerdict> {
    let meta = std::fs::metadata(model_path).ok()?;
    let mtime_ms = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    Some(OkVerdict {
        size_bytes: meta.len(),
        mtime_ms,
        gpu_layers: n_gpu_layers,
        n_ctx,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        passed_at_unix: 0, // not part of equality below
    })
}

/// True when this exact (model file, gpu_layers, n_ctx, app version)
/// already passed a probe — safe to skip the child process.
pub fn cached_ok(model_path: &std::path::Path, n_gpu_layers: u32, n_ctx: u32) -> bool {
    let Some(path) = verdict_cache_path() else {
        return false;
    };
    let Some(want) = current_verdict(model_path, n_gpu_layers, n_ctx) else {
        return false;
    };
    let Some(file) = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice::<VerdictFile>(&b).ok())
    else {
        return false;
    };
    if file.schema_version != VERDICT_SCHEMA {
        return false;
    }
    file.entries
        .get(&model_path.display().to_string())
        .map(|got| {
            got.size_bytes == want.size_bytes
                && got.mtime_ms == want.mtime_ms
                && got.gpu_layers == want.gpu_layers
                && got.n_ctx == want.n_ctx
                && got.app_version == want.app_version
        })
        .unwrap_or(false)
}

/// Record a passing probe. Best-effort — a write failure costs one
/// re-probe next boot, never the boot itself.
pub fn record_ok(model_path: &std::path::Path, n_gpu_layers: u32, n_ctx: u32) {
    let Some(path) = verdict_cache_path() else {
        return;
    };
    let Some(mut v) = current_verdict(model_path, n_gpu_layers, n_ctx) else {
        return;
    };
    v.passed_at_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut file = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice::<VerdictFile>(&b).ok())
        .filter(|f| f.schema_version == VERDICT_SCHEMA)
        .unwrap_or(VerdictFile {
            schema_version: VERDICT_SCHEMA,
            entries: std::collections::HashMap::new(),
        });
    file.entries.insert(model_path.display().to_string(), v);
    let write = || -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_vec(&file).map_err(std::io::Error::other)?,
        )?;
        std::fs::rename(&tmp, &path)
    };
    if let Err(e) = write() {
        tracing::warn!(error = %e, "smoketest: verdict cache write failed (next boot re-probes)");
    }
}

#[cfg(test)]
mod verdict_cache_tests {
    use super::*;

    fn with_cache_env<T>(dir: &std::path::Path, f: impl FnOnce() -> T) -> T {
        // Route the cache into a tempdir via HOME — the path helper
        // derives from home_dir(). Serialise on the crate-wide HOME lock
        // (NOT a private one): crash_report's tests mutate HOME too, and a
        // per-module lock lets them race us. See `crate::test_support`.
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
    fn unknown_model_misses_then_recorded_pass_hits() {
        let tmp = tempfile::tempdir().unwrap();
        let model = tmp.path().join("model.gguf");
        std::fs::write(&model, b"weights").unwrap();

        with_cache_env(tmp.path(), || {
            assert!(!cached_ok(&model, 99, 2048), "no verdict yet");
            record_ok(&model, 99, 2048);
            assert!(cached_ok(&model, 99, 2048), "recorded pass must hit");
            // Different config → miss.
            assert!(!cached_ok(&model, 0, 2048), "gpu_layers change re-probes");
            assert!(!cached_ok(&model, 99, 512), "ctx change re-probes");
        });
    }

    #[test]
    fn model_file_change_invalidates() {
        let tmp = tempfile::tempdir().unwrap();
        let model = tmp.path().join("model.gguf");
        std::fs::write(&model, b"weights").unwrap();

        with_cache_env(tmp.path(), || {
            record_ok(&model, 99, 2048);
            assert!(cached_ok(&model, 99, 2048));
            // Replace the file (size changes; mtime too).
            std::fs::write(&model, b"different weights entirely").unwrap();
            assert!(
                !cached_ok(&model, 99, 2048),
                "changed GGUF must force a fresh probe"
            );
        });
    }
}
