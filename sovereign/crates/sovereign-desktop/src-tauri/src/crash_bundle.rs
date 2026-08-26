// SPDX-License-Identifier: AGPL-3.0-or-later
//! Crash-report bundler — the "Send report" affordance that follows
//! a supervisor-detected daemon crash (see `crate::supervisor`).
//!
//! Lifecycle:
//! 1. `Supervisor::persist_crash_log` writes a `daemon-<ts>.log` file
//!    under `<data_dir>/crash-logs/` whenever the child exits / fails
//!    heartbeat / spawn fails. Each file already contains the
//!    captured stderr ring buffer + exit reason header.
//! 2. The user clicks "Report problem" on the Reconnect banner. The
//!    Tauri command [`prepare_crash_report`] runs.
//! 3. We read the latest crash log, redact the active `SetupConfig`
//!    (model basenames only — no absolute paths, no extra fields),
//!    stitch into a single markdown file at
//!    `~/Desktop/sovereign-crash-<ts>.md`, and return both the file
//!    path AND the project's GitHub Issues URL the frontend can hand
//!    to `tauri-plugin-shell::open`.
//! 4. The user reads the file (transparency: every byte we'd ship
//!    is visible), opens a GitHub issue, and attaches it manually.
//!
//! Deliberately does NOT auto-upload: nothing leaves the machine
//! unless the user chooses to attach the file to an issue they open.
//! Reading the report first builds trust that the desktop isn't
//! shipping anything surprising. The trade-off is one extra "attach
//! this file" step; for an audit-first launch that's a worthwhile cost.
//!
//! v2 polish (deferred): opt-in HTTPS upload to a crash endpoint,
//! plus an in-app inbox view of crash status.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sovereign_core::setup_config::SetupConfig;

/// Maximum bytes of a crash log we'll embed verbatim into the
/// report. Daemon stderr ring buffers are capped at 500 lines today;
/// 256 KB is generous headroom and bounded enough that the report
/// stays attachable to a regular email.
const MAX_CRASH_LOG_BYTES: usize = 256 * 1024;

/// Returns the most recently modified `daemon-*.log` under
/// `crash_log_dir`, or `None` if the directory is empty / missing.
pub fn latest_crash_log(crash_log_dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(crash_log_dir).ok()?;
    let mut latest: Option<(SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path.file_name()?.to_string_lossy().into_owned();
        if !name.starts_with("daemon-") || !name.ends_with(".log") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        if latest.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {
            latest = Some((modified, path));
        }
    }
    latest.map(|(_, p)| p)
}

/// Why the user is filing this. A crash is one reason among several,
/// and it used to be the only one that could produce a report at all —
/// which meant the failures people actually hit (slow, wrong answer,
/// can't see anyone, import stuck) had no artifact behind them.
///
/// The reason is not cosmetic: it is the first thing triage reads, and
/// it tells the reader which section of the report to look at first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportReason {
    /// The app or the engine stopped.
    Crash,
    /// Answers arrive, too slowly to use.
    Slow,
    /// An answer was wrong, empty, or ignored the sources.
    WrongAnswer,
    /// Can't see peers, can't join, sharing not working.
    Mesh,
    /// A document or knowledge-base import failed or stalled.
    Import,
    /// Anything else.
    Other,
}

impl ReportReason {
    /// Human-readable, used in the report title.
    pub fn label(&self) -> &'static str {
        match self {
            ReportReason::Crash => "crash",
            ReportReason::Slow => "slowness",
            ReportReason::WrongAnswer => "a wrong answer",
            ReportReason::Mesh => "mesh trouble",
            ReportReason::Import => "an import problem",
            ReportReason::Other => "a problem",
        }
    }

    /// Filename-safe token. Also what a support conversation greps for
    /// across a folder of reports from twenty people.
    pub fn slug(&self) -> &'static str {
        match self {
            ReportReason::Crash => "crash",
            ReportReason::Slow => "slow",
            ReportReason::WrongAnswer => "answer",
            ReportReason::Mesh => "mesh",
            ReportReason::Import => "import",
            ReportReason::Other => "other",
        }
    }

    /// Parse the string the frontend sends. Unknown values degrade to
    /// [`ReportReason::Other`] rather than erroring — a user trying to
    /// report a problem must never be blocked by a bad enum.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "crash" => ReportReason::Crash,
            "slow" => ReportReason::Slow,
            "answer" | "wrong_answer" | "wronganswer" => ReportReason::WrongAnswer,
            "mesh" => ReportReason::Mesh,
            "import" => ReportReason::Import,
            _ => ReportReason::Other,
        }
    }
}

/// Everything the report renders from. A struct rather than nine
/// positional parameters because the list grows every time a new
/// failure class earns a section, and a caller that transposes two
/// `Option<&str>` arguments produces a plausible-looking wrong report.
pub struct ReportInputs<'a> {
    pub timestamp_unix: u64,
    pub app_version: &'a str,
    pub os_label: &'a str,
    pub reason: ReportReason,
    /// What the user typed. The single highest-value field in the
    /// document — it is the only part that says what *should* have
    /// happened, which no amount of machine state can supply.
    pub user_note: Option<&'a str>,
    pub config: Option<&'a SetupConfig>,
    pub health: Option<&'a crate::health::HealthReport>,
    /// The specific answer being reported, when the report was filed
    /// from a chat message rather than from Settings. Present only on
    /// the [`ReportReason::WrongAnswer`] path today; the section it
    /// renders is what makes "this answer was wrong" debuggable
    /// without asking the user to reproduce anything.
    pub turn: Option<&'a crate::turn_report::TurnSnapshot>,
    pub crash_log_path: Option<&'a Path>,
    pub crash_log: &'a str,
}

/// Render the markdown report. Pure function — no IO — so the test
/// suite can pin the exact wire format without touching the disk.
///
/// `crash_log` is the verbatim file content (may be empty if we
/// couldn't read it; the report stays useful as a config snapshot).
/// `config` is `None` for installations that haven't completed the
/// setup wizard yet.
pub fn render_report(inp: &ReportInputs<'_>) -> String {
    let ReportInputs {
        timestamp_unix,
        app_version,
        os_label,
        reason,
        user_note,
        config,
        health,
        turn,
        crash_log_path,
        crash_log,
    } = *inp;

    let mut out = String::new();
    out.push_str(&format!("# svrnmesh report — {}\n\n", reason.label()));
    if let Some(t) = turn {
        // Directly under the title, because it is the handle the whole
        // support conversation runs on: the user can say it out loud
        // before the file has been sent anywhere.
        out.push_str(&format!(
            "**Reference: {}**\n\n",
            crate::turn_report::reference_code(&t.message_id)
        ));
    }

    // The health summary goes above everything, including the version
    // block, because it is the line that decides whether anyone needs
    // to read the rest.
    if let Some(h) = health {
        out.push_str(&format!(
            "**{} {}**\n\n",
            h.overall().glyph(),
            h.summary_line()
        ));
    }

    out.push_str(&format!("- App version: `{app_version}`\n"));
    out.push_str(&format!("- OS: `{os_label}`\n"));
    out.push_str(&format!(
        "- Generated: `{timestamp_unix}` (unix seconds)\n\n"
    ));

    // Stated in the artifact itself, not just in a privacy policy the
    // user won't read. They are about to hand this to someone; the
    // document has to be able to tell them what they are handing over
    // — and therefore it has to change when the contents change. A
    // fixed disclaimer that stopped being true the day we added the
    // reported-answer section would be worse than none.
    out.push_str(&disclosure(turn));

    out.push_str("## What went wrong\n\n");
    match user_note.map(str::trim).filter(|s| !s.is_empty()) {
        Some(n) => {
            out.push_str(n);
            out.push_str("\n\n");
        }
        None => out.push_str("_(the reporter did not add a description)_\n\n"),
    }

    // Ahead of the health check: when someone reports an answer, the
    // answer is the subject and the machine state is the context.
    if let Some(t) = turn {
        out.push_str(&crate::turn_report::render_turn_section(t));
    }

    if let Some(h) = health {
        out.push_str("## Health check\n\n");
        for c in &h.checks {
            out.push_str(&format!(
                "- {} **{}** ({}) — {}\n",
                c.status.glyph(),
                c.label,
                c.id,
                c.detail
            ));
            if let Some(fix) = &c.fix_hint {
                out.push_str(&format!("  - _suggested:_ {fix}\n"));
            }
        }
        out.push('\n');
    }

    out.push_str("## Daemon config (redacted)\n\n");
    match config {
        Some(c) => {
            out.push_str(&format!(
                "- Primary model: `{}`\n",
                basename(&c.models.primary)
            ));
            // Show the explicit fast GGUF when one is configured;
            // otherwise tell triage that primary is doing double duty
            // so they don't go hunting for a fast-slot misconfig.
            if c.models.has_explicit_fast() {
                out.push_str(&format!(
                    "- Fast model: `{}`\n",
                    basename(c.models.fast_path())
                ));
            } else {
                out.push_str("- Fast model: <subsumed by primary>\n");
            }
            out.push_str(&format!("- Embed model: `{}`\n", basename(&c.models.embed)));
            if let Some(code) = &c.models.code {
                out.push_str(&format!("- Code model: `{}`\n", basename(code)));
            }
            out.push_str(&format!("- Client port: `{}`\n", c.daemon.client_port));
            out.push_str(&format!("- Internal port: `{}`\n", c.daemon.internal_port));
            out.push_str(&format!(
                "- Yield window (s): `{}`\n",
                c.daemon.yield_to_foreground_secs
            ));
        }
        None => out.push_str("- _(no SetupConfig on disk)_\n"),
    }
    out.push('\n');

    out.push_str("## Crash log\n\n");
    match crash_log_path {
        Some(p) => out.push_str(&format!("Source: `{}`\n\n", p.display())),
        None => out.push_str("Source: _(no crash log found)_\n\n"),
    }
    if crash_log.is_empty() {
        out.push_str("_(crash log unavailable)_\n");
    } else {
        out.push_str("```\n");
        out.push_str(crash_log);
        if !crash_log.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("```\n");
    }
    out
}

/// The "what's in this file" paragraph, derived from what is actually
/// in the file.
///
/// Three cases, because there are three genuinely different documents
/// here and telling a person the wrong one is a broken promise, not a
/// typo:
///
/// - no turn: machine state only — the always-on report from Settings;
/// - a turn: that one question and answer, by the reporter's choice;
/// - a turn with source text: the passages the answer was built from.
fn disclosure(turn: Option<&crate::turn_report::TurnSnapshot>) -> String {
    let base = "> **What's in this file.** Your app version and settings, a health check, \
                and whatever you typed below.";
    let tail = "Read it before you send it — nothing leaves this computer unless you send \
                it yourself.";
    match turn {
        None => format!(
            "{base} It does **not** contain your documents, your conversations, or your \
             answers. {tail}\n\n"
        ),
        Some(t) if t.include_source_text => format!(
            "{base} It also contains **the one question and answer you chose to report**, \
             and **the text of the passages that answer was built from** — you asked for \
             those to be included. Nothing from your other conversations or documents is \
             here. {tail}\n\n"
        ),
        Some(_) => format!(
            "{base} It also contains **the one question and answer you chose to report**, \
             and the *names* of the documents that answer used — not their contents. \
             Nothing from your other conversations is here. {tail}\n\n"
        ),
    }
}

/// Strip the directory portion. Treats both `/` and `\` as separators
/// so Windows paths round-trip cleanly. Empty input maps to empty —
/// callers handle the `_(unset)_` case at the rendering layer.
fn basename(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Convenience wrapper: read the latest crash log from
/// `<data_dir>/crash-logs/`, truncate to `MAX_CRASH_LOG_BYTES` if
/// needed, build the report, write it to `~/Desktop/sovereign-
/// crash-<unix-ts>.md`, and return both the on-disk path and the
/// project's GitHub Issues URL the frontend can open.
///
/// All failure modes are mapped to `Result<_, String>` for direct
/// surface to the Tauri command.
pub fn prepare_report(
    data_dir: &Path,
    config: Option<&SetupConfig>,
    app_version: &str,
) -> Result<PreparedReport, String> {
    prepare_report_with(&ReportRequest {
        data_dir,
        config,
        app_version,
        reason: ReportReason::Crash,
        user_note: None,
        health: None,
        turn: None,
    })
}

/// What to put in a report. A struct for the same reason
/// [`ReportInputs`] is one: the list grows with every failure class
/// that earns a section, and a caller who transposes two
/// `Option<&str>` arguments produces a plausible-looking wrong report
/// rather than a compile error.
pub struct ReportRequest<'a> {
    pub data_dir: &'a Path,
    pub config: Option<&'a SetupConfig>,
    pub app_version: &'a str,
    pub reason: ReportReason,
    pub user_note: Option<&'a str>,
    pub health: Option<&'a crate::health::HealthReport>,
    pub turn: Option<&'a crate::turn_report::TurnSnapshot>,
}

/// The general form: any [`ReportReason`], the user's own description,
/// a health check, and optionally the specific answer being reported.
///
/// Always attaches the latest crash log when one exists, even for a
/// non-crash reason — a user reporting "it's slow" who crashed twice
/// yesterday is describing one problem, not two, and they have no way
/// to know that.
pub fn prepare_report_with(req: &ReportRequest<'_>) -> Result<PreparedReport, String> {
    let &ReportRequest {
        data_dir,
        config,
        app_version,
        reason,
        user_note,
        health,
        turn,
    } = req;
    let crash_log_dir = data_dir.join("crash-logs");
    let crash_log_path = latest_crash_log(&crash_log_dir);
    let crash_log = match &crash_log_path {
        Some(p) => {
            read_truncated(p).unwrap_or_else(|e| format!("(failed to read {}: {e})", p.display()))
        }
        None => String::new(),
    };

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let os_label = os_label();
    let report = render_report(&ReportInputs {
        timestamp_unix: ts,
        app_version,
        os_label: &os_label,
        reason,
        user_note,
        config,
        health,
        turn,
        crash_log_path: crash_log_path.as_deref(),
        crash_log: &crash_log,
    });

    // A reported answer is named by its reference code, not by the
    // clock: the user is about to read that code to someone, and the
    // filename is the first place they will look for it. Re-reporting
    // the same answer overwrites rather than littering the Desktop
    // with near-identical files — the later report is the better one,
    // since it is the one with the second thought in the note.
    let reference_code = turn.map(|t| crate::turn_report::reference_code(&t.message_id));
    let filename = match &reference_code {
        Some(code) => format!("svrnmesh-answer-{code}.md"),
        None => format!("svrnmesh-{}-{ts}.md", reason.slug()),
    };
    let dest = desktop_dir()
        .ok_or_else(|| "could not resolve user Desktop directory".to_string())?
        .join(filename);
    std::fs::write(&dest, report).map_err(|e| format!("write {}: {e}", dest.display()))?;

    Ok(PreparedReport {
        issues_url: issues_url(),
        report_path: dest,
        reference_code,
    })
}

pub struct PreparedReport {
    pub report_path: PathBuf,
    pub issues_url: String,
    /// The speakable handle for the reported answer, when this report
    /// is about one. `None` for machine-state reports, which have
    /// nothing to correlate against.
    pub reference_code: Option<String>,
}

fn read_truncated(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    if bytes.len() > MAX_CRASH_LOG_BYTES {
        // Keep the TAIL of the file — crash context (panic, last
        // syscalls before SEGV) lives at the bottom. Prepend a
        // marker so the receiver knows the head was elided.
        let tail = &bytes[bytes.len() - MAX_CRASH_LOG_BYTES..];
        let mut s = String::from("_(crash log truncated; showing the trailing 256 KB)_\n\n");
        s.push_str(&String::from_utf8_lossy(tail));
        Ok(s)
    } else {
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

fn os_label() -> String {
    // Family is structurally available at compile time; specific OS
    // version requires a syscall and isn't worth the complexity for
    // v1. The user fills in details in the email if asked.
    format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH)
}

#[allow(clippy::disallowed_methods)] // real $HOME: fallback when the OS Desktop dir is unset
pub(crate) fn desktop_dir() -> Option<PathBuf> {
    // `dirs::desktop_dir()` honours the OS-specific path (XDG_DESKTOP_DIR
    // on Linux, ~/Desktop on macOS, the localized name on Windows).
    // Falls back to home if Desktop isn't configured.
    dirs::desktop_dir().or_else(dirs::home_dir)
}

/// The public repository's GitHub Issues page (new-issue form). The
/// crash flow hands this to the frontend instead of an email address:
/// the user opens an issue and attaches the locally-written report.
///
/// `alexsbryan` is the current code owner; update the URL if the repo moves.
const GITHUB_ISSUES_URL: &str = "https://github.com/alexsbryan/commonwealth-ai/issues/new";

pub(crate) fn issues_url() -> String {
    GITHUB_ISSUES_URL.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sovereign_core::setup_config::{DaemonSection, DataSection, ModelsSection, SetupConfig};
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    fn config_fixture() -> SetupConfig {
        SetupConfig {
            compute: Default::default(),
            search: Default::default(),
            models: ModelsSection {
                primary: "/home/alex/.sovereign/models/Darwin-36B.gguf".into(),
                fast: Some("/home/alex/.sovereign/models/Qwen3-2B.gguf".into()),
                embed: "/home/alex/.sovereign/models/embed.gguf".into(),
                code: None,
                context_size: None,
                fast_context_size: None,
                max_extras_memory_gb: None,
                extra: BTreeMap::new(),
                primary_pool: None,
                edit: None,
            },
            daemon: DaemonSection {
                client_port: 9741,
                internal_port: 9742,
                ..Default::default()
            },
            data: DataSection::default(),
            watched_folders: Default::default(),
            memory: Default::default(),
            iroh: Default::default(),
            shared_model: Default::default(),
            discovery: Default::default(),
            mcp_servers: Vec::new(),
        }
    }

    /// Minimal inputs; tests override the one field they exercise.
    fn inputs<'a>() -> ReportInputs<'a> {
        ReportInputs {
            timestamp_unix: 1234567890,
            app_version: "0.1.0",
            os_label: "linux",
            reason: ReportReason::Crash,
            user_note: None,
            config: None,
            health: None,
            turn: None,
            crash_log_path: None,
            crash_log: "",
        }
    }

    #[test]
    fn report_redacts_model_paths_to_basenames() {
        let cfg = config_fixture();
        let report = render_report(&ReportInputs {
            os_label: "linux (x86_64)",
            config: Some(&cfg),
            crash_log: "(no crash log)",
            ..inputs()
        });
        // The basename should be present; the absolute path should NOT.
        assert!(report.contains("Darwin-36B.gguf"));
        assert!(!report.contains("/home/alex/.sovereign/models/"));
    }

    #[test]
    fn report_handles_missing_config() {
        let report = render_report(&inputs());
        assert!(report.contains("_(no SetupConfig on disk)_"));
    }

    #[test]
    fn report_embeds_crash_log_in_fence() {
        let report = render_report(&ReportInputs {
            crash_log_path: Some(Path::new("/tmp/daemon-1.log")),
            crash_log: "panic at line 5\nstack frame\n",
            ..inputs()
        });
        assert!(report.contains("```\npanic at line 5\n"));
        assert!(report.contains("Source: `/tmp/daemon-1.log`"));
    }

    #[test]
    fn every_reason_titles_and_names_its_own_file() {
        // Two reports filed in the same second must not collide on
        // disk, and a folder of twenty people's reports has to be
        // sortable by problem class without opening any of them.
        let mut slugs = std::collections::BTreeSet::new();
        for r in [
            ReportReason::Crash,
            ReportReason::Slow,
            ReportReason::WrongAnswer,
            ReportReason::Mesh,
            ReportReason::Import,
            ReportReason::Other,
        ] {
            assert!(slugs.insert(r.slug()), "duplicate slug: {}", r.slug());
            let report = render_report(&ReportInputs {
                reason: r,
                ..inputs()
            });
            assert!(
                report.starts_with(&format!("# svrnmesh report — {}", r.label())),
                "reason {:?} not in the title",
                r
            );
        }
    }

    #[test]
    fn an_unknown_reason_never_blocks_a_report() {
        // A user trying to tell us something is broken must not be
        // stopped by an enum they can't see.
        assert_eq!(ReportReason::parse("nonsense"), ReportReason::Other);
        assert_eq!(ReportReason::parse(""), ReportReason::Other);
        assert_eq!(ReportReason::parse("  CRASH "), ReportReason::Crash);
        assert_eq!(
            ReportReason::parse("wrong_answer"),
            ReportReason::WrongAnswer
        );
    }

    #[test]
    fn the_users_own_words_survive_into_the_report() {
        let report = render_report(&ReportInputs {
            reason: ReportReason::Slow,
            user_note: Some("every answer takes about two minutes since Tuesday"),
            ..inputs()
        });
        assert!(report.contains("two minutes since Tuesday"));

        // And its absence is stated rather than rendering an empty
        // section that reads like the user said nothing on purpose.
        let blank = render_report(&ReportInputs {
            user_note: Some("   "),
            ..inputs()
        });
        assert!(blank.contains("did not add a description"));
    }

    #[test]
    fn health_summary_leads_the_report_and_carries_fix_hints() {
        use crate::health::{self, CorpusFacts, HealthFacts, MeshFacts};
        let h = health::evaluate(&HealthFacts {
            daemon_running: true,
            free_disk_gb: Some(1.0),
            mesh: Some(MeshFacts {
                joined: true,
                peers_visible: 0,
                peers_known: 4,
                ..Default::default()
            }),
            corpora: Some(CorpusFacts {
                total: 1,
                failed: 0,
                in_progress: 0,
            }),
            ..Default::default()
        });
        let report = render_report(&ReportInputs {
            reason: ReportReason::Mesh,
            health: Some(&h),
            ..inputs()
        });
        // The verdict is above the version block: whoever opens this
        // should know in one line whether to keep reading.
        let head = &report[..report.find("App version").unwrap()];
        assert!(
            head.contains("need attention"),
            "summary not in head: {head}"
        );
        assert!(report.contains("mesh_peers"));
        // Fix hints travel with the report, so a supporter can answer
        // by quoting the file back rather than re-deriving the advice.
        assert!(report.contains("_suggested:_"));
    }

    #[test]
    fn report_states_what_it_does_and_does_not_contain() {
        // The privacy posture is only real if the artifact itself
        // carries it — the user is about to hand this to someone.
        let report = render_report(&inputs());
        assert!(report.contains("does **not** contain your documents"));
        assert!(report.contains("nothing leaves"));
    }

    /// A reported turn with consent withheld — the default the dialog
    /// offers.
    fn turn_fixture() -> crate::turn_report::TurnSnapshot {
        serde_json::from_str(
            r#"{"conversation_id":"c-1","message_id":"m-1",
                "question":"who wrote it?","answer":"Hegel.",
                "retrieved":[{"title":"Hegel's Dialectics","snippet":"body text"}]}"#,
        )
        .unwrap()
    }

    #[test]
    fn the_disclosure_tracks_what_is_actually_in_the_file() {
        // Three documents, three honest descriptions. A fixed
        // disclaimer that stopped being true the day we added the
        // reported-answer section would be a broken promise, not a
        // stale comment — this is the test that keeps them in step.
        let state_only = render_report(&inputs());
        assert!(state_only.contains("does **not** contain your documents"));

        let t = turn_fixture();
        let with_turn = render_report(&ReportInputs {
            reason: ReportReason::WrongAnswer,
            turn: Some(&t),
            ..inputs()
        });
        assert!(with_turn.contains("the one question and answer you chose to report"));
        assert!(
            with_turn.contains("not their contents"),
            "must say the passages' text is withheld"
        );
        assert!(!with_turn.contains("does **not** contain your documents"));

        let mut consented = turn_fixture();
        consented.include_source_text = true;
        let with_text = render_report(&ReportInputs {
            reason: ReportReason::WrongAnswer,
            turn: Some(&consented),
            ..inputs()
        });
        assert!(with_text.contains("the text of the passages"));
        assert!(!with_text.contains("not their contents"));
    }

    #[test]
    fn a_reported_answer_leads_with_its_reference_code() {
        // The code has to be readable before the file goes anywhere:
        // people quote it in a chat message first and attach the file
        // second, if at all.
        let t = turn_fixture();
        let report = render_report(&ReportInputs {
            reason: ReportReason::WrongAnswer,
            turn: Some(&t),
            ..inputs()
        });
        let code = crate::turn_report::reference_code("m-1");
        let head = &report[..report.find("App version").unwrap()];
        assert!(head.contains(&code), "code not in the head: {head}");
        assert!(
            report.contains("Hegel."),
            "the reported answer must be present"
        );
    }

    #[test]
    fn a_state_only_report_carries_no_reference_code() {
        // Nothing to correlate against, so offering a code would
        // invite the user to quote a handle that means nothing.
        let report = render_report(&inputs());
        assert!(!report.contains("Reference:"));
    }

    #[test]
    fn latest_crash_log_picks_newest() {
        let dir = TempDir::new().unwrap();
        let p1 = dir.path().join("daemon-100.log");
        let p2 = dir.path().join("daemon-200.log");
        let p3 = dir.path().join("not-a-daemon-log.txt");
        std::fs::write(&p1, "old").unwrap();
        // Sleep briefly to ensure the mtime ordering is unambiguous
        // on filesystems with second-resolution timestamps.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&p2, "new").unwrap();
        std::fs::write(&p3, "unrelated").unwrap();
        let latest = latest_crash_log(dir.path()).unwrap();
        assert_eq!(latest, p2);
    }

    #[test]
    fn latest_crash_log_returns_none_for_empty_dir() {
        let dir = TempDir::new().unwrap();
        assert!(latest_crash_log(dir.path()).is_none());
    }

    #[test]
    fn read_truncated_keeps_tail_when_oversize() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("big.log");
        // 1 MB of "A" followed by a distinctive tail.
        let mut content = "A".repeat(MAX_CRASH_LOG_BYTES * 4);
        content.push_str("TAIL_MARKER\n");
        std::fs::write(&path, &content).unwrap();
        let read = read_truncated(&path).unwrap();
        assert!(read.contains("TAIL_MARKER"));
        assert!(read.starts_with("_(crash log truncated"));
    }

    #[test]
    fn issues_url_points_at_repo_issues() {
        let url = issues_url();
        assert!(url.starts_with("https://"));
        assert!(url.contains("/issues"));
        // No email address is shipped in the crash flow.
        assert!(!url.contains("mailto:"));
        assert!(!url.contains('@'));
    }
}
