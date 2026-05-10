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
        matches!(
            self,
            SmokeResult::Crashed { .. } | SmokeResult::Timeout
        )
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
        Err(e) => return SmokeResult::Skipped { reason: format!("current_exe: {e}") },
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
        Err(e) => return SmokeResult::Skipped { reason: format!("spawn: {e}") },
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
                return SmokeResult::Skipped { reason: format!("wait: {e}") };
            }
        }
    }
}
