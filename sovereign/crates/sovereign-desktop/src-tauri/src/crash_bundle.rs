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

/// Render the markdown report. Pure function — no IO — so the test
/// suite can pin the exact wire format without touching the disk.
///
/// `crash_log` is the verbatim file content (may be empty if we
/// couldn't read it; the report stays useful as a config snapshot).
/// `config` is `None` for installations that haven't completed the
/// setup wizard yet.
pub fn render_report(
    timestamp_unix: u64,
    app_version: &str,
    os_label: &str,
    config: Option<&SetupConfig>,
    crash_log_path: Option<&Path>,
    crash_log: &str,
) -> String {
    let mut out = String::new();
    out.push_str("# svrnmesh crash report\n\n");
    out.push_str(&format!("- App version: `{app_version}`\n"));
    out.push_str(&format!("- OS: `{os_label}`\n"));
    out.push_str(&format!(
        "- Generated: `{timestamp_unix}` (unix seconds)\n\n"
    ));

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
    let report = render_report(
        ts,
        app_version,
        &os_label,
        config,
        crash_log_path.as_deref(),
        &crash_log,
    );

    let dest = desktop_dir()
        .ok_or_else(|| "could not resolve user Desktop directory".to_string())?
        .join(format!("sovereign-crash-{ts}.md"));
    std::fs::write(&dest, report).map_err(|e| format!("write {}: {e}", dest.display()))?;

    Ok(PreparedReport {
        issues_url: issues_url(),
        report_path: dest,
    })
}

pub struct PreparedReport {
    pub report_path: PathBuf,
    pub issues_url: String,
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
            models: ModelsSection {
                primary: "/home/alex/.sovereign/models/Darwin-36B.gguf".into(),
                fast: Some("/home/alex/.sovereign/models/Qwen3-2B.gguf".into()),
                embed: "/home/alex/.sovereign/models/embed.gguf".into(),
                code: None,
                context_size: None,
                max_extras_memory_gb: None,
                extra: BTreeMap::new(),
                primary_pool: None,
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

    #[test]
    fn report_redacts_model_paths_to_basenames() {
        let cfg = config_fixture();
        let report = render_report(
            1234567890,
            "0.1.0",
            "linux (x86_64)",
            Some(&cfg),
            None,
            "(no crash log)",
        );
        // The basename should be present; the absolute path should NOT.
        assert!(report.contains("Darwin-36B.gguf"));
        assert!(!report.contains("/home/alex/.sovereign/models/"));
    }

    #[test]
    fn report_handles_missing_config() {
        let report = render_report(1234567890, "0.1.0", "linux", None, None, "");
        assert!(report.contains("_(no SetupConfig on disk)_"));
    }

    #[test]
    fn report_embeds_crash_log_in_fence() {
        let report = render_report(
            1234567890,
            "0.1.0",
            "linux",
            None,
            Some(Path::new("/tmp/daemon-1.log")),
            "panic at line 5\nstack frame\n",
        );
        assert!(report.contains("```\npanic at line 5\n"));
        assert!(report.contains("Source: `/tmp/daemon-1.log`"));
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
