// SPDX-License-Identifier: AGPL-3.0-or-later
//! Parser for `.sovereign/sovereign.toml` — per-project watcher configuration.
//!
//! The file is optional. [`SovereignConfig::load_or_default`] never fails —
//! it returns a zeroed default when the file is absent or unparseable (with a
//! warning log).
//!
//! ## Example file
//!
//! ```toml
//! [test_runner]
//! command = "scripts/sovereign-test.sh"
//! working_dir = "."
//! timeout_secs = 120
//! debounce_ms = 2000
//!
//! [lint_runner]
//! command = "scripts/sovereign-lint.sh"
//! working_dir = "."
//! timeout_secs = 60
//! debounce_ms = 800
//! ```

use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result};

// ─── Types ────────────────────────────────────────────────────────────────────

/// Top-level configuration parsed from `.sovereign/sovereign.toml`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct SovereignConfig {
    /// Test runner configuration. Absent means the test watcher is unconfigured.
    pub test_runner: Option<RunnerConfig>,

    /// Lint runner configuration. Absent means the lint watcher is unconfigured.
    pub lint_runner: Option<RunnerConfig>,

    /// Whether background watchers are wanted here at all.
    #[serde(default)]
    pub watchers: WatchersConfig,
}

/// Opt-out switch for the background lint/test watchers.
///
/// ## Why this exists (2026-07-28)
///
/// Watchers are OPTIONAL. Before this, "off" could only be expressed by
/// deleting or commenting out the `[lint_runner]`/`[test_runner]` sections —
/// which is indistinguishable from "someone forgot to configure them". So
/// every surface that could see the absence treated it as a defect: `doctor`
/// raised a warning advising you to restore the config, and the status tools'
/// hint told you to put it back. On a workspace where the watchers are off
/// deliberately (this one — disabled 2026-05-31 after the parallel cargo fan
/// OOM'd the daemon under a resident model) that is a permanent false alarm,
/// and a check that always warns is a check nobody reads.
///
/// `enabled = false` says "off on purpose" so the tooling can stop asking.
/// Leaving it unset preserves the old inference, so a workspace that DOES want
/// watchers still gets told when they are missing.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct WatchersConfig {
    /// `false` = deliberately off; `true` = wanted; absent = infer from
    /// whether a runner section is present.
    pub enabled: Option<bool>,
}

/// Configuration for a single subprocess-based watcher (test or lint runner).
#[derive(Debug, Clone, Deserialize)]
pub struct RunnerConfig {
    /// Shell command to execute. Run as `sh -c "<command>"`.
    pub command: String,

    /// Working directory for the command. Defaults to the project root when absent.
    pub working_dir: Option<String>,

    /// Maximum wall-clock seconds before the run is killed. Defaults to 120.
    pub timeout_secs: Option<u64>,

    /// Debounce window in milliseconds before re-running after a file change.
    /// Defaults to the coordinator's global debounce when absent.
    pub debounce_ms: Option<u64>,
}

impl RunnerConfig {
    /// Effective timeout in seconds (default 120).
    pub fn effective_timeout_secs(&self) -> u64 {
        self.timeout_secs.unwrap_or(120)
    }
}

// ─── Loader ───────────────────────────────────────────────────────────────────

impl SovereignConfig {
    /// Load from `{sovereign_dir}/sovereign.toml`. Returns an error if the
    /// file exists but cannot be parsed.
    pub fn load(sovereign_dir: &Path) -> Result<Self> {
        let path = sovereign_dir.join("sovereign.toml");
        let contents = std::fs::read_to_string(&path).map_err(|e| {
            Error::Io(std::io::Error::new(
                e.kind(),
                format!("sovereign.toml: {}: {e}", path.display()),
            ))
        })?;
        toml::from_str(&contents).map_err(|e| {
            Error::Io(std::io::Error::other(format!(
                "sovereign.toml parse error: {e}"
            )))
        })
    }

    /// Load from `{sovereign_dir}/sovereign.toml`. Returns a default config
    /// (all fields `None`) if the file is absent or unparseable, with a
    /// diagnostic warning in the latter case.
    pub fn load_or_default(sovereign_dir: &Path) -> Self {
        let path = sovereign_dir.join("sovereign.toml");
        if !path.exists() {
            return Self::default();
        }
        match Self::load(sovereign_dir) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to parse sovereign.toml — using defaults"
                );
                Self::default()
            }
        }
    }

    /// True when this workspace has explicitly opted out of background
    /// watchers (`[watchers] enabled = false`).
    ///
    /// Callers use this to tell "off on purpose" from "not set up yet": the
    /// former is a supported posture and must not be reported as a fault.
    pub fn watchers_disabled(&self) -> bool {
        self.watchers.enabled == Some(false)
    }

    /// True when a runner is configured for either watcher.
    pub fn any_runner_configured(&self) -> bool {
        self.test_runner.is_some() || self.lint_runner.is_some()
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_toml(dir: &std::path::Path, content: &str) {
        let path = dir.join(".sovereign");
        std::fs::create_dir_all(&path).unwrap();
        let mut f = std::fs::File::create(path.join("sovereign.toml")).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    #[test]
    fn load_or_default_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = SovereignConfig::load_or_default(&dir.path().join(".sovereign"));
        assert!(cfg.test_runner.is_none());
        assert!(cfg.lint_runner.is_none());
    }

    #[test]
    fn watchers_disabled_only_when_explicitly_false() {
        let dir = tempfile::tempdir().unwrap();
        write_toml(dir.path(), "[watchers]\nenabled = false\n");
        let cfg = SovereignConfig::load_or_default(&dir.path().join(".sovereign"));
        assert!(cfg.watchers_disabled());
        assert!(!cfg.any_runner_configured());
    }

    #[test]
    fn watchers_absent_is_not_disabled() {
        // Back-compat: a workspace that never heard of [watchers] must keep
        // the old inference, so "you forgot to configure them" still warns.
        let dir = tempfile::tempdir().unwrap();
        write_toml(dir.path(), "[commonwealth]\nurl = \"http://localhost:9741\"\n");
        let cfg = SovereignConfig::load_or_default(&dir.path().join(".sovereign"));
        assert!(!cfg.watchers_disabled());
    }

    #[test]
    fn watchers_enabled_true_is_not_disabled() {
        let dir = tempfile::tempdir().unwrap();
        write_toml(dir.path(), "[watchers]\nenabled = true\n");
        let cfg = SovereignConfig::load_or_default(&dir.path().join(".sovereign"));
        assert!(!cfg.watchers_disabled());
    }

    #[test]
    fn watchers_key_coexists_with_runner_sections() {
        let dir = tempfile::tempdir().unwrap();
        write_toml(
            dir.path(),
            "[watchers]\nenabled = false\n\n[lint_runner]\ncommand = \"scripts/sovereign-lint.sh\"\n",
        );
        let cfg = SovereignConfig::load_or_default(&dir.path().join(".sovereign"));
        assert!(cfg.watchers_disabled());
        assert!(cfg.any_runner_configured());
    }

    #[test]
    fn load_test_runner() {
        let dir = tempfile::tempdir().unwrap();
        write_toml(
            dir.path(),
            r#"
[test_runner]
command = "cargo test"
timeout_secs = 90
"#,
        );
        let cfg = SovereignConfig::load_or_default(&dir.path().join(".sovereign"));
        let tr = cfg.test_runner.unwrap();
        assert_eq!(tr.command, "cargo test");
        assert_eq!(tr.effective_timeout_secs(), 90);
    }

    #[test]
    fn load_both_runners() {
        let dir = tempfile::tempdir().unwrap();
        write_toml(
            dir.path(),
            r#"
[test_runner]
command = "scripts/test.sh"

[lint_runner]
command = "scripts/lint.sh"
debounce_ms = 500
"#,
        );
        let cfg = SovereignConfig::load_or_default(&dir.path().join(".sovereign"));
        assert!(cfg.test_runner.is_some());
        let lr = cfg.lint_runner.unwrap();
        assert_eq!(lr.command, "scripts/lint.sh");
        assert_eq!(lr.debounce_ms, Some(500));
    }

    #[test]
    fn load_or_default_on_bad_toml() {
        let dir = tempfile::tempdir().unwrap();
        write_toml(dir.path(), "not valid toml ][[[");
        // Should not panic, should return default.
        let cfg = SovereignConfig::load_or_default(&dir.path().join(".sovereign"));
        assert!(cfg.test_runner.is_none());
    }
}
