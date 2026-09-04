// SPDX-License-Identifier: AGPL-3.0-or-later
//! Enrichment build orchestration for non-CLI frontends.
//!
//! Spawns `sovereign-cli enrich <subcommand>` as a subprocess,
//! parses its stdout banners into typed `EnrichProgress` events,
//! and forwards them to a caller-supplied callback. Lets the
//! desktop app drive the same flow the CLI exposes with a real-
//! time progress UI while keeping the orchestration one source of
//! truth.
//!
//! Why not link the CLI crate directly? This crate cannot: it sits in the
//! capabilities layer and `sovereign-cli-llm` is a host (ARCH_LAYERS —
//! hosts are terminal). Since ontology-v1 P0.4 the daemon, which may link
//! the host, does exactly that and installs the result as
//! `local_corpus::watched::enrich::AtlasBuildRunner`; the driver prefers it
//! and falls back to this subprocess runner only where no builder was
//! installed (a dev box with the CLI on PATH). The parsed event shape is
//! the same type the CLI emits internally, so both paths feed one sink.
//!
//! Event mapping (CLI banner → `EnrichProgress`):
//!
//! ```text
//! === enrich build — <corpus> ===             → BuildStart
//! ─── [<ord>/<total>] <step> ───              → StepStart
//! [<i>/<total>] <chapter>… <n> q              → ChapterProgress
//! [<i>/<total>] <chapter>… FAILED: <reason>   → ChapterFailed
//! === build complete — <corpus> ===           → Complete
//! (non-zero exit)                             → Aborted
//! ```
//!
//! Anything that doesn't match the banner shape is passed through
//! to stderr verbatim — we don't try to pretend we understand
//! future CLI output changes.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use corpus_engine::enrichment::pipeline::{progress::wire, BuildStep, EnrichProgress};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// Caller-supplied callback that receives each parsed event.
///
/// Boxed + `Send + Sync + 'static` so a Tauri command can clone it
/// into a spawned task and emit events on a channel without having
/// to thread a channel sender through every internal API.
pub type EnrichProgressFn = Arc<dyn Fn(EnrichProgress) + Send + Sync + 'static>;

/// Shared atomic flag a caller flips to request cancellation of
/// an in-flight build. `run_enrich_build` polls this between
/// stdout reads and kills the subprocess when the flag goes true.
///
/// `Arc<AtomicBool>` rather than a channel so a caller can scope
/// one flag to one job and drop it on completion without worrying
/// about outstanding senders. Cheap to clone, cheap to check.
pub type CancellationFlag = Arc<AtomicBool>;

/// Exit code of a build stopped through a [`CancellationFlag`] — re-exported
/// at its historical path. The constant itself moved DOWN to
/// `sovereign_contracts::launch` (order ei-5a-build-cut) so the enrichment
/// orchestrator can return it without linking this crate's tool stack; the
/// flag stays here, beside the driver that fires it. One constant, two names
/// that resolve to it (ARCH §10.6).
pub use sovereign_contracts::launch::EXIT_CANCELLED;

/// Create a fresh cancellation flag in the non-cancelled state.
/// Pair with `fire_cancellation` from a separate task (or Tauri
/// command) to request cancellation.
pub fn new_cancellation_flag() -> CancellationFlag {
    Arc::new(AtomicBool::new(false))
}

/// Request cancellation on an existing flag. Idempotent — two
/// calls in quick succession still result in a single subprocess
/// kill.
pub fn fire_cancellation(flag: &CancellationFlag) {
    flag.store(true, Ordering::SeqCst);
}

/// Outcome of a `run_enrich_build` invocation. Returned on either
/// success or failure; the `exit_code` is the ultimate truth
/// (parser-derived events are best-effort).
#[derive(Debug, Clone)]
pub struct EnrichBuildOutcome {
    pub corpus_id: String,
    pub exit_code: i32,
    /// Lines that carried the progress prefix and would NOT decode. Empty on
    /// a well-formed run, and a genuine wire fault when it is not — this is no
    /// longer "every line a regex did not recognise", which under the banner
    /// parser meant ordinary human output landed here. Surfaced in the error
    /// panel when the exit code is non-zero.
    pub unrecognised_lines: Vec<String>,
    /// Whether the child spoke the typed progress wire at all.
    pub wire: ProgressWire,
    /// `true` when the subprocess was killed via the
    /// cancellation flag rather than exiting on its own. The
    /// desktop layer uses this to suppress a misleading
    /// `Aborted` event (cancellation already emits a dedicated
    /// terminal event).
    pub cancelled: bool,
}

/// Whether the spawned CLI spoke the typed progress wire.
///
/// A build with no progress and a build whose progress we could not HEAR look
/// identical to a UI, and only one of them is a working build — so the
/// difference is a value the caller receives rather than an absence it infers
/// (ARCH §18.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressWire {
    /// Events arrived as `@progress` lines, as asked.
    Spoken {
        /// How many decoded.
        events: usize,
    },
    /// The child ran and emitted none. Almost always an older `sovereign-cli`
    /// resolved from `$PATH` that predates
    /// `SOVEREIGN_ENRICH_PROGRESS` — it printed banners to a reader that no
    /// longer parses them, so the build itself is fine and the progress panel
    /// will not move.
    Silent,
}

/// Resolve the path to the `sovereign-cli` binary the runner should
/// spawn. Honours `$SOVEREIGN_CLI` first (for ops who installed it
/// somewhere non-standard), then walks a deterministic ladder of
/// candidates so the desktop app works whether launched from a
/// terminal (full `$PATH`) or from Finder (minimal `$PATH`):
///
/// 1. `$SOVEREIGN_CLI` env var (explicit override).
/// 2. Sibling of the current executable — covers a packaged install
///    where `sovereign-desktop` and `sovereign-cli` ship in the same
///    bin dir.
/// 3. `which sovereign-cli` (canonical PATH lookup).
/// 4. `which sovereign` (the `~/.local/bin/sovereign` symlink the
///    `sovereign-cli` README installs).
/// 5. Workspace-relative `target/release/sovereign-cli` /
///    `target/debug/sovereign-cli` ascended from `current_exe()` —
///    dev-mode fallback when the desktop was `cargo run`'d from the
///    workspace.
///
/// Returns `None` only when every candidate is missing; callers
/// surface that as a clear "sovereign-cli not installed" error with
/// remediation steps rather than the kernel's bare `ENOENT`.
pub fn resolve_sovereign_cli() -> Option<PathBuf> {
    if let Ok(v) = std::env::var("SOVEREIGN_CLI") {
        let p = PathBuf::from(v);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            for name in ["sovereign-cli", "sovereign"] {
                let sibling = parent.join(name);
                if sibling.is_file() {
                    return Some(sibling);
                }
            }
            // Dev mode: `cargo run -p sovereign-desktop` puts the
            // exe at `target/debug/sovereign-desktop`; ascend to the
            // workspace root and look for `target/{release,debug}/
            // sovereign-cli`.
            let mut anc = parent;
            for _ in 0..6 {
                for profile in ["release", "debug"] {
                    let cand = anc.join("target").join(profile).join("sovereign-cli");
                    if cand.is_file() {
                        return Some(cand);
                    }
                }
                match anc.parent() {
                    Some(up) => anc = up,
                    None => break,
                }
            }
        }
    }
    for name in ["sovereign-cli", "sovereign"] {
        if let Ok(p) = which_on_path(name) {
            return Some(p);
        }
    }
    None
}

/// Minimal `which`-style lookup against `$PATH`. Returns the first
/// executable file matching `name`. Hand-rolled rather than pulling
/// the `which` crate just for two call sites — the loop is six lines.
fn which_on_path(name: &str) -> std::io::Result<PathBuf> {
    let path = std::env::var_os("PATH")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "PATH unset"))?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(name);
        if cand.is_file() {
            return Ok(cand);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("{name} not on PATH"),
    ))
}

/// Configuration for a build invocation.
///
/// `cli_path` overrides the default binary lookup (which uses
/// [`resolve_sovereign_cli`]). Tests point it at a fixture binary;
/// the desktop passes `None` to let the resolver walk its candidate
/// ladder.
#[derive(Debug, Clone, Default)]
pub struct EnrichBuildConfig {
    pub cli_path: Option<PathBuf>,
    /// Extra CLI flags passed after the corpus id. Use
    /// `["--chapters".into(), "sec_0001,sec_0002".into()]` for a
    /// subset run or `["--full".into()]` to force the default.
    pub extra_args: Vec<String>,
    /// Optional cancellation flag. When `Some`, the runner polls
    /// it after each stdout line; on `true` it kills the child
    /// subprocess and returns with `cancelled = true`. Callers
    /// that never cancel pass `None`.
    pub cancel: Option<CancellationFlag>,
}

/// Run `sovereign-cli enrich build <corpus>` as a subprocess,
/// stream its progress events through `progress`, and resolve
/// with the exit code once the subprocess exits.
///
/// The spawned process inherits stderr so the UI console still
/// gets the original diagnostic output verbatim; only stdout is
/// intercepted for parsing. This keeps the CLI's richer error
/// messages (e.g. the `! <N> drop(s) — see <path>` line after
/// atlas-resolve) visible to operators even when the parser
/// hasn't promoted that line into a structured event yet.
pub async fn run_enrich_build(
    corpus_id: &str,
    config: EnrichBuildConfig,
    progress: Option<EnrichProgressFn>,
) -> std::io::Result<EnrichBuildOutcome> {
    let bin = match config.cli_path.clone() {
        Some(p) => p,
        None => resolve_sovereign_cli().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "sovereign-cli not found. Tried $SOVEREIGN_CLI, the desktop's \
                 sibling-binary dir, $PATH (`sovereign-cli` and `sovereign`), \
                 and the workspace `target/{release,debug}/` paths. Build via \
                 `cargo build --release -p sovereign-cli` and symlink it onto \
                 $PATH, or set `SOVEREIGN_CLI=/abs/path/to/sovereign-cli`.",
            )
        })?,
    };

    let mut cmd = Command::new(&bin);
    cmd.arg("enrich")
        .arg("build")
        .arg(corpus_id)
        .args(&config.extra_args)
        // Ask for typed events instead of banners. An older binary that does
        // not know this name ignores it and prints banners, which this reader
        // reports as `ProgressWire::Silent` rather than mis-parsing.
        .env(wire::REQUEST_ENV, wire::REQUEST_VALUE)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    let mut child = cmd.spawn()?;
    let stdout = child
        .stdout
        .take()
        .expect("stdout piped above — take() should succeed");
    let mut lines = BufReader::new(stdout).lines();

    let mut state = ParserState::new();
    let mut cancelled = false;

    // Main line-reader loop. When cancellation is requested, race
    // `lines.next_line()` against the flag — polling between reads
    // is enough granularity here because Phase-1 per-chapter LLM
    // calls are the expensive units, and the CLI emits at least
    // one stdout line per chapter.
    loop {
        if let Some(flag) = &config.cancel {
            if flag.load(Ordering::SeqCst) {
                // Caller asked us to stop. Kill the child; the
                // next `next_line()` would block until the child
                // emits another line, which a killed subprocess
                // won't do. `start_kill` sends SIGKILL on *nix
                // (tokio::process maps the term); the wait below
                // reaps it.
                let _ = child.start_kill();
                cancelled = true;
                break;
            }
        }
        match lines.next_line().await? {
            Some(line) => match state.ingest(&line) {
                Some(evt) => {
                    if let Some(cb) = progress.as_ref() {
                        cb(evt);
                    }
                }
                // Not an event: the child's human output. Forward it so a log
                // tail still shows what the subprocess said — an event line
                // would only be JSON noise beside the typed callback.
                None => println!("{line}"),
            },
            None => break, // stdout closed; subprocess has exited
        }
    }

    let status = child.wait().await?;
    let exit_code = status.code().unwrap_or(-1);

    if cancelled {
        // Emit the typed `Cancelled` terminal event — distinct
        // from `Aborted` (real failure) and `SpawnFailed` (never
        // started) so the UI can render "Cancelled" without
        // string-sniffing the message. `at_step` captures what
        // was running when cancel fired so the UI can show
        // "Cancelled mid-extract" if it wants.
        if let Some(cb) = progress.as_ref() {
            cb(EnrichProgress::Cancelled {
                corpus_id: corpus_id.to_string(),
                at_step: state.current_step,
            });
        }
    } else if !state.complete_emitted && exit_code != 0 {
        // Subprocess exited non-zero without the parser seeing a
        // Complete banner — the CLI either printed StepFailed +
        // Aborted already (in which case the callback already
        // got them) or exited before a banner could fire (in
        // which case we synthesise Aborted here attributed to
        // the current step).
        if let Some(cb) = progress.as_ref() {
            let step = state.current_step.unwrap_or(BuildStep::Report);
            cb(EnrichProgress::Aborted {
                corpus_id: corpus_id.to_string(),
                failed_step: step,
                exit_code,
            });
        }
    }

    let wire = if state.events == 0 {
        tracing::warn!(
            bin = %bin.display(),
            "enrich build: the spawned CLI emitted no typed progress events — \
             it predates `{}` and the progress panel will not advance. The \
             build itself is unaffected; its exit code is the truth.",
            wire::REQUEST_ENV,
        );
        ProgressWire::Silent
    } else {
        ProgressWire::Spoken {
            events: state.events,
        }
    };

    Ok(EnrichBuildOutcome {
        corpus_id: corpus_id.to_string(),
        exit_code,
        unrecognised_lines: state.unrecognised,
        cancelled,
        wire,
    })
}

/// Reads the child's typed progress wire.
///
/// # What this replaced, and why it is smaller
///
/// Until 2026-08-26 this was a nine-function REGEX PARSER over the CLI's human
/// banners — `=== enrich build — <corpus> ===`, `─── [3/9] extract ───`,
/// `[4/12] sec_0004… 7 q` — plus an `is_noise` allowlist of banner decorations
/// and a `classify_reason` that keyword-matched free text back into a
/// `failure_kind` the child had already computed as an enum and thrown away.
///
/// Every one of those was a promise about someone else's prose. TOPOLOGY §9.3
/// names the failure: reword a banner for a human and the desktop's progress
/// panel silently stops advancing, with no compiler and no test in between.
/// The events were `Serialize` and tagged from the day they were written; only
/// the rendering was missing. Now the child encodes and this decodes, through
/// the one declaration in `corpus_engine::…::progress::wire`.
///
/// What remains is state the WIRE cannot carry because it is the reader's, not
/// the writer's: which step is in flight when a cancel arrives, and whether a
/// terminal event was already seen so a clean exit does not synthesise a
/// second one.
pub struct ParserState {
    /// Tracks the step currently running, so `Cancelled` and a synthesised
    /// `Aborted` can name where the build was.
    current_step: Option<BuildStep>,
    /// Set when a terminal event arrives so the wrapper does not re-emit
    /// `Aborted` on a clean exit.
    complete_emitted: bool,
    /// How many events decoded. Zero means the child never spoke the wire —
    /// see [`ProgressWire`].
    events: usize,
    /// Lines that carried the progress prefix and would not decode. A real
    /// wire fault, and empty on every well-formed run — unlike the banner
    /// parser's version of this field, which collected ordinary human output.
    unrecognised: Vec<String>,
}

impl ParserState {
    /// A reader for one build.
    ///
    /// Takes no corpus id: every event carries its own, because the wire was
    /// designed for a UI rendering several concurrent builds. The banner
    /// parser had to hold one because a banner mentions the corpus once, at
    /// the top, and every later line had to be attributed by memory.
    pub fn new() -> Self {
        Self {
            current_step: None,
            complete_emitted: false,
            events: 0,
            unrecognised: Vec::new(),
        }
    }

    /// Consume one line of child stdout. `Some(event)` when the line is one;
    /// `None` for the child's human output, which the caller forwards to its
    /// own stdout rather than discarding.
    pub fn ingest(&mut self, line: &str) -> Option<EnrichProgress> {
        let evt = match wire::decode(line) {
            Some(evt) => evt,
            None => {
                // Prefixed but undecodable is a fault; unprefixed is prose.
                if line.trim_start().starts_with(wire::PREFIX) {
                    self.unrecognised.push(line.trim().to_string());
                }
                return None;
            }
        };
        self.events += 1;
        match &evt {
            EnrichProgress::StepStart { step, .. } => self.current_step = Some(*step),
            EnrichProgress::Complete { .. }
            | EnrichProgress::Aborted { .. }
            | EnrichProgress::Cancelled { .. } => self.complete_emitted = true,
            _ => {}
        }
        Some(evt)
    }
}

impl Default for ParserState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    // The e2e test lock intentionally spans awaits: the guard serializes
    // whole test bodies (they spawn fake-CLI subprocesses sharing env), and
    // each #[tokio::test] owns its runtime, so a contending sibling parks a
    // thread — serialization, never deadlock (P0.3 lock audit, 2026-07-12).
    #![allow(clippy::await_holding_lock)]

    use super::*;

    /// The wire round-trips every variant the reader acts on.
    ///
    /// Named failing input (ARCH §18.1): change the `#[serde(tag)]` name or a
    /// field on `EnrichProgress` and this reds, because both halves read the
    /// one declaration. Under the banner parser the equivalent input — someone
    /// rewording `─── [3/9] extract ───` — reddened nothing at all.
    #[test]
    fn every_event_survives_the_wire() {
        let cases = vec![
            EnrichProgress::BuildStart {
                corpus_id: "bk".into(),
                pipeline_id: "atlas".into(),
                steps: vec![BuildStep::Seed, BuildStep::Extract, BuildStep::Report],
                auto_skipped: vec![BuildStep::Configure],
            },
            EnrichProgress::StepStart {
                corpus_id: "bk".into(),
                step: BuildStep::Extract,
                ordinal: 2,
                total: 3,
            },
            EnrichProgress::ChapterProgress {
                corpus_id: "bk".into(),
                chapter_id: "sec_0007".into(),
                index: 2,
                total: 5,
                question_count: Some(3),
            },
            EnrichProgress::ChapterFailed {
                corpus_id: "bk".into(),
                chapter_id: "sec_0001".into(),
                failure_kind: "think_truncated".into(),
                reason: "<think> truncated: parse error at EOF".into(),
            },
            EnrichProgress::Complete {
                corpus_id: "bk".into(),
                steps_completed: 3,
            },
        ];
        for evt in &cases {
            let line = wire::encode(evt);
            assert!(line.starts_with(wire::PREFIX), "no prefix: {line}");
            let back = wire::decode(&line).expect("decodes");
            assert_eq!(
                format!("{back:?}"),
                format!("{evt:?}"),
                "round-trip changed the event"
            );
        }
    }

    /// `failure_kind` arrives TYPED and is not re-derived from prose.
    ///
    /// The banner parser had a `classify_reason` that keyword-matched the
    /// free-text reason back into a kind the child had already computed as an
    /// enum and then discarded — so `<think> truncated: parse error at EOF`
    /// was classified by which substring the `if` chain tested first. Deleted:
    /// the kind now travels.
    #[test]
    fn failure_kind_travels_rather_than_being_guessed() {
        let mut p = ParserState::new();
        let line = wire::encode(&EnrichProgress::ChapterFailed {
            corpus_id: "bk".into(),
            chapter_id: "sec_0001".into(),
            // Prose that the old keyword matcher would have called
            // `think_truncated`; the child says `parse_drift` and the child is
            // the one that knows.
            failure_kind: "parse_drift".into(),
            reason: "<think> truncated: parse error at EOF".into(),
        });
        match p.ingest(&line) {
            Some(EnrichProgress::ChapterFailed { failure_kind, .. }) => {
                assert_eq!(failure_kind, "parse_drift");
            }
            other => panic!("expected ChapterFailed, got {other:?}"),
        }
    }

    #[test]
    fn reader_tracks_the_step_in_flight_and_the_terminal_event() {
        let mut p = ParserState::new();
        assert!(p.current_step.is_none());

        p.ingest(&wire::encode(&EnrichProgress::StepStart {
            corpus_id: "bk".into(),
            step: BuildStep::Extract,
            ordinal: 2,
            total: 3,
        }));
        assert_eq!(p.current_step, Some(BuildStep::Extract));
        assert!(!p.complete_emitted);

        p.ingest(&wire::encode(&EnrichProgress::Complete {
            corpus_id: "bk".into(),
            steps_completed: 3,
        }));
        assert!(p.complete_emitted);
        assert_eq!(p.events, 2);
    }

    /// Human output is passed through, not collected as a parse failure.
    ///
    /// The banner parser kept an `is_noise` allowlist of decorations it had to
    /// recognise in order NOT to report them — `pipeline \`…\``, `✓ `, `! `,
    /// `Next:` — which meant a new decoration became a spurious "unrecognised
    /// line" in an error panel. Only a line that claims to be an event and
    /// then is not is a fault now.
    #[test]
    fn prose_is_prose_and_only_a_broken_event_is_a_fault() {
        let mut p = ParserState::new();
        for prose in [
            "pipeline `atlas` loaded",
            "✓ 12 entity atom(s), 22 claim(s)",
            "! 3 drop(s) — see /tmp/run.json",
            "Next: svrn enrich report bk",
            "",
        ] {
            assert!(p.ingest(prose).is_none(), "treated as an event: {prose}");
        }
        assert!(p.unrecognised.is_empty(), "{:?}", p.unrecognised);
        assert_eq!(p.events, 0);

        assert!(p.ingest("@progress {\"kind\":\"not_a_variant\"}").is_none());
        assert_eq!(p.unrecognised.len(), 1);
    }

    mod e2e {
        use super::*;
        use std::os::unix::fs::PermissionsExt;
        use std::path::{Path, PathBuf};
        use std::sync::{Arc, Mutex};

        /// Write an executable shell script to `dir` that will
        /// serve as the fake `sovereign-cli`. Tests customise the
        /// body per scenario.
        fn write_fake_cli(dir: &Path, script_body: &str) -> PathBuf {
            let path = dir.join("fake-sovereign-cli.sh");
            // Shebang + script body. Every test exits the script
            // explicitly so exit codes are deterministic.
            let contents = format!("#!/bin/sh\n{script_body}\n");
            std::fs::write(&path, contents).unwrap();
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
            path
        }

        /// Per-test-module serialisation. Without this, parallel
        /// `cargo test` workers can race fork+exec on the freshly-
        /// written `fake-sovereign-cli.sh` and hit ETXTBSY: a sibling
        /// worker has the file's path open in its fd table at the
        /// moment a forked child reaches the `execve` of *this*
        /// worker's just-written script. The kernel treats "any
        /// process holds the inode open for write" as a write-busy
        /// condition for exec. Tempdirs differ per test, but the
        /// fork+exec window crosses fd tables. Serialising the e2e
        /// path eliminates the race without dropping `cargo test`'s
        /// parallelism for the rest of the file.
        fn e2e_test_lock() -> std::sync::MutexGuard<'static, ()> {
            static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
            LOCK.get_or_init(|| std::sync::Mutex::new(()))
                .lock()
                .unwrap_or_else(|p| p.into_inner())
        }

        /// Collect every EnrichProgress callback into a shared
        /// Vec. Returns the callback + the Vec handle.
        fn event_collector() -> (EnrichProgressFn, Arc<Mutex<Vec<EnrichProgress>>>) {
            let collected: Arc<Mutex<Vec<EnrichProgress>>> = Arc::new(Mutex::new(Vec::new()));
            let collected_c = collected.clone();
            let cb: EnrichProgressFn = Arc::new(move |evt: EnrichProgress| {
                collected_c.lock().unwrap().push(evt);
            });
            (cb, collected)
        }

        fn event_kinds(events: &[EnrichProgress]) -> Vec<&'static str> {
            events
                .iter()
                .map(|e| match e {
                    EnrichProgress::BuildStart { .. } => "build_start",
                    EnrichProgress::StepStart { .. } => "step_start",
                    EnrichProgress::ChapterProgress { .. } => "chapter_progress",
                    EnrichProgress::ChapterFailed { .. } => "chapter_failed",
                    EnrichProgress::StepDone { .. } => "step_done",
                    EnrichProgress::StepFailed { .. } => "step_failed",
                    EnrichProgress::Complete { .. } => "complete",
                    EnrichProgress::Aborted { .. } => "aborted",
                    EnrichProgress::SpawnFailed { .. } => "spawn_failed",
                    EnrichProgress::Cancelled { .. } => "cancelled",
                })
                .collect()
        }

        #[tokio::test]
        async fn e2e_happy_path_emits_build_start_and_complete() {
            let _guard = e2e_test_lock();
            // Minimal happy-path script: emit the start banner
            // + two step-starts + complete, on the typed wire, with human
            // banners interleaved exactly as the real CLI would NOT emit them
            // — they are here on purpose, to prove prose beside events is
            // passed through and never mistaken for one.
            let tmp = tempfile::tempdir().unwrap();
            let cli = write_fake_cli(
                tmp.path(),
                r#"
echo "=== enrich build — bk ==="
echo '@progress {"kind":"build_start","corpus_id":"bk","pipeline_id":"atlas","steps":["seed","report"],"auto_skipped":[]}'
echo '@progress {"kind":"step_start","corpus_id":"bk","step":"seed","ordinal":1,"total":2}'
echo "· seeding entities"
echo '@progress {"kind":"step_start","corpus_id":"bk","step":"report","ordinal":2,"total":2}'
echo '@progress {"kind":"complete","corpus_id":"bk","steps_completed":2}'
exit 0
"#,
            );
            let (cb, collected) = event_collector();
            let outcome = run_enrich_build(
                "bk",
                EnrichBuildConfig {
                    cli_path: Some(cli),
                    extra_args: vec![],
                    cancel: None,
                },
                Some(cb),
            )
            .await
            .expect("happy path should not fail to spawn");

            assert_eq!(outcome.exit_code, 0);
            assert!(!outcome.cancelled);
            assert!(
                outcome.unrecognised_lines.is_empty(),
                "unrecognised lines on happy path: {:?}",
                outcome.unrecognised_lines
            );
            assert!(
                matches!(outcome.wire, ProgressWire::Spoken { events: 4 }),
                "expected 4 events on the wire, got {:?}",
                outcome.wire
            );
            let events = collected.lock().unwrap();
            let kinds = event_kinds(&events);
            // Exactly ONE build_start. The banner parser re-emitted it once
            // per planned-step line because a banner cannot carry a list;
            // the wire can, so the UI stops seeing a total that grows.
            assert_eq!(
                kinds.iter().filter(|k| **k == "build_start").count(),
                1,
                "expected exactly one build_start: {kinds:?}"
            );
            assert_eq!(
                kinds.iter().filter(|k| **k == "step_start").count(),
                2,
                "expected 2 step_start events"
            );
            assert_eq!(kinds.last().copied(), Some("complete"));
        }

        #[tokio::test]
        async fn e2e_nonzero_exit_without_complete_banner_synthesizes_aborted() {
            let _guard = e2e_test_lock();
            // Script emits one step_start then exits 1 WITHOUT
            // printing a complete banner. The library should
            // synthesize `Aborted` so the UI's state machine
            // transitions terminally.
            let tmp = tempfile::tempdir().unwrap();
            let cli = write_fake_cli(
                tmp.path(),
                r#"
echo '@progress {"kind":"build_start","corpus_id":"bk","pipeline_id":"atlas","steps":["seed","extract","report"],"auto_skipped":[]}'
echo '@progress {"kind":"step_start","corpus_id":"bk","step":"seed","ordinal":1,"total":3}'
exit 1
"#,
            );
            let (cb, collected) = event_collector();
            let outcome = run_enrich_build(
                "bk",
                EnrichBuildConfig {
                    cli_path: Some(cli),
                    extra_args: vec![],
                    cancel: None,
                },
                Some(cb),
            )
            .await
            .expect("spawn should still succeed; subprocess just exits 1");

            assert_eq!(outcome.exit_code, 1);
            assert!(!outcome.cancelled);
            let events = collected.lock().unwrap();
            let kinds = event_kinds(&events);
            assert_eq!(
                kinds.last().copied(),
                Some("aborted"),
                "expected Aborted as the terminal event; kinds: {kinds:?}"
            );
            // Aborted should attribute to Seed — the last step
            // that was running when the subprocess exited.
            let aborted = events
                .iter()
                .rev()
                .find_map(|e| match e {
                    EnrichProgress::Aborted { failed_step, .. } => Some(*failed_step),
                    _ => None,
                })
                .expect("Aborted event present");
            assert_eq!(aborted, BuildStep::Seed);
        }

        #[tokio::test]
        async fn e2e_cancellation_kills_subprocess_and_emits_cancelled() {
            let _guard = e2e_test_lock();
            // Script streams a banner line, sleeps long enough
            // for the test to flip the cancel flag, then would
            // print more. The cancel should fire between the
            // first stdout line and the sleep's completion,
            // killing the child and yielding a `Cancelled`
            // terminal event.
            let tmp = tempfile::tempdir().unwrap();
            let cli = write_fake_cli(
                tmp.path(),
                r#"
echo '@progress {"kind":"build_start","corpus_id":"bk","pipeline_id":"atlas","steps":["seed","report"],"auto_skipped":[]}'
echo '@progress {"kind":"step_start","corpus_id":"bk","step":"seed","ordinal":1,"total":2}'
# Sleep long enough that the test has time to flip the
# cancellation flag. `sleep 5` is well past the test's
# expected runtime; if the kill doesn't land, the test hangs
# until tokio's default test timeout.
sleep 5
echo '@progress {"kind":"complete","corpus_id":"bk","steps_completed":2}'
exit 0
"#,
            );
            let (cb, collected) = event_collector();
            let cancel = new_cancellation_flag();
            let cancel_for_task = cancel.clone();
            let cli_for_task = cli.clone();
            // Drive the build in a background task so we can
            // flip the flag from the main test task after a
            // short delay. Without this split we'd block on
            // `run_enrich_build.await` before we could ever
            // cancel.
            let handle = tokio::spawn(async move {
                run_enrich_build(
                    "bk",
                    EnrichBuildConfig {
                        cli_path: Some(cli_for_task),
                        extra_args: vec![],
                        cancel: Some(cancel_for_task),
                    },
                    Some(cb),
                )
                .await
            });
            // Give the subprocess time to print its first few
            // lines (parser reacts to each). 150ms is plenty on
            // any developer machine — the script hasn't hit
            // `sleep 5` yet, but has emitted at least the build
            // banner + one step_start.
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            fire_cancellation(&cancel);

            let outcome = handle.await.unwrap().expect("cancel path");
            assert!(outcome.cancelled, "outcome.cancelled must be true");
            let events = collected.lock().unwrap();
            let kinds = event_kinds(&events);
            assert_eq!(
                kinds.last().copied(),
                Some("cancelled"),
                "terminal event should be Cancelled; got: {kinds:?}"
            );
        }

        #[tokio::test]
        async fn e2e_spawn_error_when_binary_does_not_exist() {
            let _guard = e2e_test_lock();
            // Point `cli_path` at a nonexistent file — the
            // spawn itself fails, bubbling up as Err. The
            // library doesn't synthesize SpawnFailed here
            // (that's the desktop layer's job after seeing the
            // Err); it just returns the io::Error.
            let outcome = run_enrich_build(
                "bk",
                EnrichBuildConfig {
                    cli_path: Some(PathBuf::from("/no/such/binary/here")),
                    extra_args: vec![],
                    cancel: None,
                },
                None,
            )
            .await;
            assert!(outcome.is_err(), "expected spawn error, got Ok");
        }

        /// An OLDER `sovereign-cli` — banners only, no wire — is reported, not
        /// mistaken for a build with nothing to say.
        ///
        /// Named failing input (ARCH §18.1). `resolve_sovereign_cli` walks
        /// four ladders and can land on a binary from `$PATH` that predates
        /// `SOVEREIGN_ENRICH_PROGRESS`. That build still RUNS and still exits
        /// 0; only its progress is inaudible. Collapsing that into "no events"
        /// would leave a UI showing a stalled bar for a healthy build with no
        /// way to tell which it was — the substitution ARCH §18.3 forbids.
        #[tokio::test]
        async fn e2e_a_cli_that_speaks_only_banners_is_reported_as_silent() {
            let _guard = e2e_test_lock();
            let tmp = tempfile::tempdir().unwrap();
            let cli = write_fake_cli(
                tmp.path(),
                r#"
echo "=== enrich build — bk ==="
echo "─── [1/2] seed ───"
echo "=== build complete — bk ==="
exit 0
"#,
            );
            let (cb, collected) = event_collector();
            let outcome = run_enrich_build(
                "bk",
                EnrichBuildConfig {
                    cli_path: Some(cli),
                    extra_args: vec![],
                    cancel: None,
                },
                Some(cb),
            )
            .await
            .expect("an old CLI still runs");

            // The build itself is fine — the exit code is the truth.
            assert_eq!(outcome.exit_code, 0);
            assert_eq!(outcome.wire, ProgressWire::Silent);
            // Banners are prose, not faults.
            assert!(
                outcome.unrecognised_lines.is_empty(),
                "banners recorded as wire faults: {:?}",
                outcome.unrecognised_lines
            );
            // Exit 0 with no terminal event seen: nothing synthesised, because
            // a clean exit is not an abort.
            assert!(collected.lock().unwrap().is_empty());
        }
    }
}
