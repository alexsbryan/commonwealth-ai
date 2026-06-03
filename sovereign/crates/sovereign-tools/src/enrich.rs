//! Enrichment build orchestration for non-CLI frontends.
//!
//! Spawns `sovereign-cli enrich <subcommand>` as a subprocess,
//! parses its stdout banners into typed `EnrichProgress` events,
//! and forwards them to a caller-supplied callback. Lets the
//! desktop app drive the same flow the CLI exposes with a real-
//! time progress UI while keeping the orchestration one source of
//! truth.
//!
//! Why not link the CLI crate directly? The CLI is a binary-only
//! crate today; exposing its internals as a library would mean
//! splitting the crate, reordering visibility, and threading state
//! through. For Landing 3.C we keep the CLI untouched and sit one
//! parser away from it. The parsed event shape is the same type
//! the CLI emits internally, so a future library refactor is a
//! drop-in replacement on this side.
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

use corpus_engine::enrichment::pipeline::{BuildStep, EnrichProgress};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// Caller-supplied callback that receives each parsed event.
///
/// Boxed + `Send + Sync + 'static` so a Tauri command can clone it
/// into a spawned task and emit events on a channel without having
/// to thread a channel sender through every internal API.
pub type EnrichProgressFn =
    Arc<dyn Fn(EnrichProgress) + Send + Sync + 'static>;

/// Shared atomic flag a caller flips to request cancellation of
/// an in-flight build. `run_enrich_build` polls this between
/// stdout reads and kills the subprocess when the flag goes true.
///
/// `Arc<AtomicBool>` rather than a channel so a caller can scope
/// one flag to one job and drop it on completion without worrying
/// about outstanding senders. Cheap to clone, cheap to check.
pub type CancellationFlag = Arc<AtomicBool>;

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
    /// Non-banner lines the parser couldn't classify. Empty when
    /// the CLI's output was well-formed. Exposed for debugging —
    /// the UI surfaces these in the error panel when the exit
    /// code is non-zero.
    pub unrecognised_lines: Vec<String>,
    /// `true` when the subprocess was killed via the
    /// cancellation flag rather than exiting on its own. The
    /// desktop layer uses this to suppress a misleading
    /// `Aborted` event (cancellation already emits a dedicated
    /// terminal event).
    pub cancelled: bool,
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
    let path = std::env::var_os("PATH").ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "PATH unset")
    })?;
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
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    let mut child = cmd.spawn()?;
    let stdout = child
        .stdout
        .take()
        .expect("stdout piped above — take() should succeed");
    let mut lines = BufReader::new(stdout).lines();

    let mut state = ParserState::new(corpus_id);
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
            Some(line) => {
                // Forward the line to our stdout so a log tail still
                // shows the banner text — otherwise the subprocess'
                // output would be invisible to any operator reading
                // logs.
                println!("{line}");
                if let Some(evt) = state.ingest(&line) {
                    if let Some(cb) = progress.as_ref() {
                        cb(evt);
                    }
                }
            }
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

    Ok(EnrichBuildOutcome {
        corpus_id: corpus_id.to_string(),
        exit_code,
        unrecognised_lines: state.unrecognised,
        cancelled,
    })
}

/// Stateful parser for the CLI's build banners.
///
/// Separated from `run_enrich_build` so it can be unit-tested
/// without spawning a subprocess — feed lines via `ingest`, inspect
/// the event stream.
pub struct ParserState {
    corpus_id: String,
    /// Steps announced by `BuildStart`, captured so the parser can
    /// synthesize `ordinal` + `total` for steps whose banner line
    /// doesn't include them explicitly.
    planned_steps: Vec<BuildStep>,
    /// Tracks the step currently running. `None` at start and
    /// between steps. Used to attribute `ChapterProgress` /
    /// `ChapterFailed` / `Aborted` events to the right step.
    current_step: Option<BuildStep>,
    /// Set when a `Complete` event is emitted so the subprocess
    /// wrapper doesn't re-emit `Aborted` on a clean exit.
    complete_emitted: bool,
    /// Lines that didn't match any banner. Surfaced in the outcome
    /// for debugging.
    unrecognised: Vec<String>,
}

impl ParserState {
    pub fn new(corpus_id: &str) -> Self {
        Self {
            corpus_id: corpus_id.to_string(),
            planned_steps: Vec::new(),
            current_step: None,
            complete_emitted: false,
            unrecognised: Vec::new(),
        }
    }

    /// Consume one line of CLI stdout. Returns `Some(event)` when
    /// the line is recognised, `None` otherwise.
    pub fn ingest(&mut self, line: &str) -> Option<EnrichProgress> {
        let trimmed = line.trim();

        // === enrich build — <corpus> ===
        if let Some(corpus) = parse_build_start(trimmed) {
            self.corpus_id = corpus;
            return None; // wait for the planned-steps lines
        }

        // "  N step(s) planned" — total count
        if trimmed.ends_with("step(s) planned") {
            // Flush any BuildStart we'd accumulated — but we
            // don't have the step list yet, it arrives on
            // subsequent "  1. seed" lines. Mark that we're
            // capturing.
            self.planned_steps.clear();
            return None;
        }

        // "    1. seed" / "    2. extract" etc.
        if let Some(step) = parse_planned_step(trimmed) {
            self.planned_steps.push(step);
            // Emit BuildStart once we have at least one step —
            // the UI can update the total as more arrive, though
            // in practice all nine arrive before the first
            // StepStart.
            return Some(EnrichProgress::BuildStart {
                corpus_id: self.corpus_id.clone(),
                pipeline_id: String::new(), // CLI banner doesn't include pipeline id
                steps: self.planned_steps.clone(),
                auto_skipped: Vec::new(),
            });
        }

        // ─── [ord/total] <step> ───
        if let Some((step, ordinal, total)) = parse_step_banner(trimmed) {
            self.current_step = Some(step);
            return Some(EnrichProgress::StepStart {
                corpus_id: self.corpus_id.clone(),
                step,
                ordinal,
                total,
            });
        }

        // [i/total] <chapter_id>… <n> q   (success)
        if let Some((chapter_id, index, total, q_count)) =
            parse_chapter_done(trimmed)
        {
            return Some(EnrichProgress::ChapterProgress {
                corpus_id: self.corpus_id.clone(),
                chapter_id,
                index,
                total,
                question_count: Some(q_count),
            });
        }

        // [i/total] <chapter_id>… FAILED: <reason>
        if let Some((chapter_id, reason)) = parse_chapter_failed(trimmed) {
            return Some(EnrichProgress::ChapterFailed {
                corpus_id: self.corpus_id.clone(),
                chapter_id,
                failure_kind: classify_reason(&reason),
                reason,
            });
        }

        // === build complete — <corpus> ===
        if parse_build_complete(trimmed) {
            self.complete_emitted = true;
            return Some(EnrichProgress::Complete {
                corpus_id: self.corpus_id.clone(),
                steps_completed: self.planned_steps.len(),
            });
        }

        if !trimmed.is_empty() && !is_noise(trimmed) {
            self.unrecognised.push(trimmed.to_string());
        }
        None
    }
}

fn parse_build_start(line: &str) -> Option<String> {
    // `=== enrich build — <corpus> ===`
    let stripped = line.strip_prefix("=== enrich build — ")?;
    let corpus = stripped.strip_suffix(" ===")?;
    Some(corpus.to_string())
}

fn parse_build_complete(line: &str) -> bool {
    // `=== build complete — <corpus> ===`
    line.starts_with("=== build complete — ") && line.ends_with(" ===")
}

fn parse_planned_step(line: &str) -> Option<BuildStep> {
    // "    1. seed" — loose match: digits, dot, space, step-id.
    let mut chars = line.chars().peekable();
    while matches!(chars.peek(), Some(c) if c.is_ascii_digit()) {
        chars.next();
    }
    if chars.peek() == Some(&'.') {
        chars.next();
        if chars.peek() == Some(&' ') {
            chars.next();
            let rest: String = chars.collect();
            return build_step_from_id(rest.trim());
        }
    }
    None
}

fn parse_step_banner(line: &str) -> Option<(BuildStep, usize, usize)> {
    // "─── [3/7] extract ───"
    let rest = line.strip_prefix("─── [")?;
    let rest = rest.strip_suffix(" ───")?;
    let (bracket, rest) = rest.split_once("] ")?;
    let (ord_s, total_s) = bracket.split_once('/')?;
    let ord: usize = ord_s.parse().ok()?;
    let total: usize = total_s.parse().ok()?;
    let step = build_step_from_id(rest)?;
    Some((step, ord, total))
}

fn parse_chapter_done(line: &str) -> Option<(String, usize, usize, usize)> {
    // "    [1/3] sec_0001… 2 q"
    let body = line.trim_start();
    let rest = body.strip_prefix('[')?;
    let (bracket, rest) = rest.split_once("] ")?;
    let (ix_s, total_s) = bracket.split_once('/')?;
    let index: usize = ix_s.parse().ok()?;
    let total: usize = total_s.parse().ok()?;
    let (chapter_id, tail) = rest.split_once("… ")?;
    // Must end with " q" — the success shape.
    let count = tail.strip_suffix(" q")?;
    let q_count: usize = count.trim().parse().ok()?;
    Some((chapter_id.to_string(), index, total, q_count))
}

fn parse_chapter_failed(line: &str) -> Option<(String, String)> {
    // "    [1/3] sec_0001… FAILED: parse error: …"
    let body = line.trim_start();
    let rest = body.strip_prefix('[')?;
    let (_bracket, rest) = rest.split_once("] ")?;
    let (chapter_id, tail) = rest.split_once("… FAILED: ")?;
    Some((chapter_id.to_string(), tail.to_string()))
}

fn build_step_from_id(id: &str) -> Option<BuildStep> {
    match id {
        "seed" => Some(BuildStep::Seed),
        "extract" => Some(BuildStep::Extract),
        "cluster" => Some(BuildStep::Cluster),
        "name" => Some(BuildStep::Name),
        "resolve" => Some(BuildStep::Resolve),
        "tensions" => Some(BuildStep::Tensions),
        "gaps" => Some(BuildStep::Gaps),
        "configure" => Some(BuildStep::Configure),
        "report" => Some(BuildStep::Report),
        _ => None,
    }
}

/// Best-effort classification of a Phase 1 failure reason into a
/// `PhaseFailureKind` snake_case id. The CLI already embeds the
/// failure_kind in the run file but its stdout reason line is
/// free-text — we keyword-match on known substrings.
fn classify_reason(reason: &str) -> String {
    let lower = reason.to_lowercase();
    if lower.contains("think truncated") || lower.contains("<think>") {
        "think_truncated".into()
    } else if lower.contains("parse error") || lower.contains("parse:") {
        "parse_drift".into()
    } else if lower.contains("chat error") || lower.contains("chat:") {
        "chat_error".into()
    } else if lower.contains("empty") {
        "empty_extraction".into()
    } else if lower.contains("skipped") {
        "skipped".into()
    } else {
        "other".into()
    }
}

/// Noise lines — known banner decorations the parser doesn't turn
/// into events but shouldn't record as "unrecognised" either.
///
/// The caller passes the already-trimmed line, so prefix checks
/// run against the leading non-whitespace character.
fn is_noise(line: &str) -> bool {
    line.starts_with("pipeline `")
        || line.starts_with("· ")
        || line.starts_with("✓ ")
        || line.starts_with("! ")
        || line.starts_with("Next:")
        || line.starts_with("running phase ")
        || line.starts_with("loaded ")
        // Per-step detail the orchestration prints on success
        // (e.g. "12 entity atom(s)"). These are informational —
        // the StepDone event covers the structured signal.
        || (line.starts_with("✓") && line.contains("atom"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_build_start_and_complete_match_cli_banners() {
        assert_eq!(
            parse_build_start("=== enrich build — bk ==="),
            Some("bk".to_string())
        );
        assert!(parse_build_complete("=== build complete — bk ==="));
        assert!(!parse_build_complete("=== foo bar ==="));
    }

    #[test]
    fn parse_step_banner_extracts_step_and_position() {
        let got = parse_step_banner("─── [3/7] extract ───").unwrap();
        assert_eq!(got, (BuildStep::Extract, 3, 7));
        let got = parse_step_banner("─── [9/9] report ───").unwrap();
        assert_eq!(got, (BuildStep::Report, 9, 9));
        assert!(parse_step_banner("─── [a/b] seed ───").is_none());
    }

    #[test]
    fn parse_chapter_done_extracts_id_and_q_count() {
        let got = parse_chapter_done("    [2/5] sec_0007… 3 q").unwrap();
        assert_eq!(got, ("sec_0007".to_string(), 2, 5, 3));
    }

    #[test]
    fn parse_chapter_failed_extracts_id_and_reason() {
        let (id, reason) =
            parse_chapter_failed("    [1/3] sec_0001… FAILED: parse error: EOF")
                .unwrap();
        assert_eq!(id, "sec_0001");
        assert!(reason.starts_with("parse error"));
    }

    #[test]
    fn parser_end_to_end_stream() {
        // Drive the parser through a scripted sequence that
        // mirrors real CLI output. Locks the event order the
        // desktop relies on.
        let mut p = ParserState::new("bk");
        let script = [
            "=== enrich build — bk ===",
            "  9 step(s) planned",
            "    1. seed",
            "    2. extract",
            "    3. cluster",
            "    4. name",
            "    5. resolve",
            "    6. tensions",
            "    7. gaps",
            "    8. configure",
            "    9. report",
            "─── [1/9] seed ───",
            "  running phase 1a",
            "─── [2/9] extract ───",
            "    [1/3] sec_0001… 2 q",
            "    [2/3] sec_0002… FAILED: parse error: EOF",
            "    [3/3] sec_0003… 1 q",
            "=== build complete — bk ===",
        ];
        let mut events: Vec<EnrichProgress> = Vec::new();
        for line in &script {
            if let Some(e) = p.ingest(line) {
                events.push(e);
            }
        }
        // BuildStart gets re-emitted as each planned step is
        // appended; desktop listeners treat the last one as
        // canonical. The first StepStart is the seed step.
        assert!(matches!(events.first(), Some(EnrichProgress::BuildStart { .. })));
        let step_starts: Vec<&EnrichProgress> = events
            .iter()
            .filter(|e| matches!(e, EnrichProgress::StepStart { .. }))
            .collect();
        assert_eq!(step_starts.len(), 2);
        let chapter_events: Vec<&EnrichProgress> = events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    EnrichProgress::ChapterProgress { .. }
                        | EnrichProgress::ChapterFailed { .. }
                )
            })
            .collect();
        assert_eq!(chapter_events.len(), 3);
        // Final event is Complete.
        assert!(matches!(events.last(), Some(EnrichProgress::Complete { .. })));
        assert!(p.complete_emitted);
    }

    #[test]
    fn parser_records_unrecognised_lines_but_skips_noise() {
        let mut p = ParserState::new("bk");
        // `ingest` trims leading whitespace first — test both
        // pre-trimmed and raw forms so the noise filter is
        // robust regardless of the CLI's indentation.
        p.ingest("pipeline `literary_atlas` auto-skips: seed");
        p.ingest("  ✓ 12 entity atom(s)");
        p.ingest("  · promoted subset run → cache/questions.json");
        p.ingest("some weird line the parser doesn't know");
        // Banner decorations are noise; only the last line is
        // unrecognised.
        assert_eq!(
            p.unrecognised.len(),
            1,
            "unrecognised lines: {:?}",
            p.unrecognised
        );
        assert!(p.unrecognised[0].contains("weird line"));
    }

    #[test]
    fn classify_reason_picks_think_truncated_over_parse_drift() {
        // When a line mentions both, think_truncated is more
        // specific — the parse error is a downstream effect.
        assert_eq!(
            classify_reason("<think> truncated: parse error at EOF"),
            "think_truncated"
        );
        assert_eq!(classify_reason("parse error: missing field"), "parse_drift");
        assert_eq!(classify_reason("chat error: 502"), "chat_error");
    }

    // ── End-to-end subprocess tests ────────────────────────────
    //
    // The parser tests above feed scripted strings through
    // `ParserState::ingest` directly — fast, pure, no subprocess.
    // These tests spawn a real subprocess (a shell script stood up
    // in a tempdir) and exercise the full `run_enrich_build`
    // plumbing: tokio::process spawn, stdout streaming,
    // cancellation flag poll, child kill, exit-code propagation.
    //
    // They're slower (~50ms each on the happy path, ~200ms on
    // cancellation due to the sleep) but catch regressions that
    // parser-only tests can't: SIGKILL handling, non-zero exit
    // with no complete banner, ordering between the final
    // stdout flush and the wait() reap.
    //
    // *nix-only — the fixture is a `#!/bin/sh` script. Gated
    // behind `cfg(unix)` because Windows takes a different path
    // (and our target platforms are macOS + Linux).

    #[cfg(unix)]
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
            static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> =
                std::sync::OnceLock::new();
            LOCK.get_or_init(|| std::sync::Mutex::new(()))
                .lock()
                .unwrap_or_else(|p| p.into_inner())
        }

        /// Collect every EnrichProgress callback into a shared
        /// Vec. Returns the callback + the Vec handle.
        fn event_collector() -> (EnrichProgressFn, Arc<Mutex<Vec<EnrichProgress>>>) {
            let collected: Arc<Mutex<Vec<EnrichProgress>>> =
                Arc::new(Mutex::new(Vec::new()));
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
            // + planned step list + one step-start + the
            // complete banner. Exit 0.
            let tmp = tempfile::tempdir().unwrap();
            let cli = write_fake_cli(
                tmp.path(),
                r#"
echo "=== enrich build — bk ==="
echo "  2 step(s) planned"
echo "    1. seed"
echo "    2. report"
echo "─── [1/2] seed ───"
echo "─── [2/2] report ───"
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
            .expect("happy path should not fail to spawn");

            assert_eq!(outcome.exit_code, 0);
            assert!(!outcome.cancelled);
            assert!(
                outcome.unrecognised_lines.is_empty(),
                "unrecognised lines on happy path: {:?}",
                outcome.unrecognised_lines
            );
            let events = collected.lock().unwrap();
            let kinds = event_kinds(&events);
            // BuildStart gets re-emitted as each planned-step
            // line arrives (parser design); the last BuildStart
            // carries the full step list. StepStart should fire
            // twice; Complete once at the end.
            assert!(
                kinds.contains(&"build_start"),
                "missing build_start: {kinds:?}"
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
echo "=== enrich build — bk ==="
echo "  3 step(s) planned"
echo "    1. seed"
echo "    2. extract"
echo "    3. report"
echo "─── [1/3] seed ───"
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
echo "=== enrich build — bk ==="
echo "  2 step(s) planned"
echo "    1. seed"
echo "    2. report"
echo "─── [1/2] seed ───"
# Sleep long enough that the test has time to flip the
# cancellation flag. `sleep 5` is well past the test's
# expected runtime; if the kill doesn't land, the test hangs
# until tokio's default test timeout.
sleep 5
echo "=== build complete — bk ==="
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
    }
}
