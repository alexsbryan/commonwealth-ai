// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where setup's two kinds of output go.
//!
//! The wizard has always narrated to a human on stdout. `--json` gives it a
//! SECOND audience — a first-run client that spawned this process and parses
//! its stdout (`sovereign_contracts::daemon_wire::SetupProgressLine`) — and
//! the two cannot share a stream: a parser that has to skip lines it does not
//! recognise is one banner change away from silently losing an event.
//!
//! So `--json` makes stdout structured and moves the narration to stderr.
//! That is ONE decision, taken here, rather than a `if json` at each of the
//! ~110 `println!` sites: `say!` is the narration, `line` is the wire, and a
//! new narration line cannot land on the wrong stream by forgetting a guard
//! (ARCH principle 10).

use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};

use sovereign_contracts::daemon_wire::{SetupProgressLine, SetupProgressPhase};

/// Set once, at the top of `run_setup`, from `--json`. A process-global
/// rather than a threaded parameter because the narration sites are spread
/// across six modules and several `tokio::join!` arms, and a flag that can be
/// *missed* at one site is the failure this module exists to prevent.
static JSON_MODE: AtomicBool = AtomicBool::new(false);

/// Declare this run's stdout to be the structured stream.
pub(super) fn set_json_mode(on: bool) {
    JSON_MODE.store(on, Ordering::Relaxed);
}

/// Is stdout reserved for [`SetupProgressLine`]s?
pub(super) fn json_mode() -> bool {
    JSON_MODE.load(Ordering::Relaxed)
}

/// Human narration: stdout normally, stderr when stdout is the wire.
macro_rules! say {
    ($($arg:tt)*) => {
        if $crate::setup_cmd::emit::json_mode() {
            eprintln!($($arg)*);
        } else {
            println!($($arg)*);
        }
    };
}
pub(super) use say;

/// Where delivered lines go when a test has asked to read them.
///
/// NOT `#[cfg(test)]`, deliberately: a capture that only exists under `cfg`
/// means the tested path and the shipped path are different code, which is
/// the shape ARCH principle 5 refuses. Production installs no capture, so
/// this costs one uncontended lock per emitted line — and lines are emitted
/// at most every 250 ms.
static CAPTURE: std::sync::Mutex<Option<Vec<String>>> = std::sync::Mutex::new(None);

/// Start collecting delivered lines instead of writing them, and drop
/// anything a previous capture left behind.
pub(super) fn start_capture() {
    *CAPTURE.lock().expect("capture mutex") = Some(Vec::new());
}

/// Stop collecting and return what was delivered since [`start_capture`].
pub(super) fn take_capture() -> Vec<String> {
    CAPTURE
        .lock()
        .expect("capture mutex")
        .take()
        .unwrap_or_default()
}

/// Write one progress line to stdout and flush it.
///
/// Flushed per line on purpose: the reader is a parent process reading a pipe
/// as the run proceeds, and a block-buffered stdout would deliver the whole
/// narration at exit — which is the same as not having it. A no-op outside
/// `--json`, so every call site is unconditional.
pub(super) fn line(l: &SetupProgressLine) {
    if !json_mode() {
        return;
    }
    let Ok(json) = serde_json::to_string(l) else {
        // Serializing a struct of primitives cannot fail; if it somehow did,
        // dropping the line is better than aborting a download that is
        // already running, and the narration on stderr still says where we
        // are.
        return;
    };
    if let Some(buf) = CAPTURE.lock().expect("capture mutex").as_mut() {
        buf.push(json);
        return;
    }
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{json}");
    let _ = out.flush();
}

/// A phase announcement with no numbers attached.
pub(super) fn narrate(phase: SetupProgressPhase, message: impl Into<String>) {
    line(&SetupProgressLine::narration(phase, message));
}

/// The terminal success line. `config_path` is what the caller should read
/// next — the client relaunches against it.
pub(super) fn done(config_path: &std::path::Path) {
    let mut l = SetupProgressLine::narration(SetupProgressPhase::Done, "Setup complete.");
    l.config_path = Some(config_path.display().to_string());
    line(&l);
}

/// The terminal failure line. The exit code is still the verdict; this
/// carries the reason a UI can show.
pub(super) fn failed(error: &str) {
    let mut l = SetupProgressLine::narration(SetupProgressPhase::Failed, error.to_string());
    l.error = Some(error.to_string());
    line(&l);
}

/// One download's progress, with an ETA from a 4-sample rolling rate.
///
/// The sampler is here rather than at the call site because the CLI's
/// stderr bar and the JSON stream are two renderings of ONE download, and a
/// second rate estimator would be a second answer to "how long left".
pub(super) struct DownloadNarrator {
    phase: SetupProgressPhase,
    message: String,
    file: String,
    samples: std::sync::Mutex<Vec<(std::time::Instant, u64)>>,
}

impl DownloadNarrator {
    /// Start narrating `file` under `phase`.
    pub(super) fn new(phase: SetupProgressPhase, message: &str, file: &str) -> Self {
        let me = Self {
            phase,
            message: message.to_string(),
            file: file.to_string(),
            samples: std::sync::Mutex::new(Vec::with_capacity(8)),
        };
        me.emit(0, None);
        me
    }

    /// Report `done` of `total` bytes.
    pub(super) fn emit(&self, done: u64, total: Option<u64>) {
        if !json_mode() {
            return;
        }
        let eta_seconds = self.eta(done, total);
        let fraction =
            total.and_then(|t| (t > 0).then(|| (done as f64 / t as f64).clamp(0.0, 1.0)));
        line(&SetupProgressLine {
            phase: self.phase,
            message: self.message.clone(),
            file: Some(self.file.clone()),
            downloaded: Some(done),
            total,
            fraction,
            eta_seconds,
            config_path: None,
            error: None,
        });
    }

    fn eta(&self, done: u64, total: Option<u64>) -> Option<u64> {
        let total = total?;
        let mut s = self.samples.lock().ok()?;
        s.push((std::time::Instant::now(), done));
        if s.len() > 4 {
            let drop = s.len() - 4;
            s.drain(..drop);
        }
        if s.len() < 2 {
            return None;
        }
        let (t0, b0) = s[0];
        let (t1, b1) = s[s.len() - 1];
        let dt = t1.duration_since(t0).as_secs_f64();
        let db = b1.saturating_sub(b0) as f64;
        if dt <= 0.0 || db <= 0.0 {
            return None;
        }
        let rate = db / dt;
        (rate > 0.0).then(|| (total.saturating_sub(done) as f64 / rate).round() as u64)
    }
}
