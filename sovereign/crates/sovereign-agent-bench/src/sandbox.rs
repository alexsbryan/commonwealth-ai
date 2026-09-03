// SPDX-License-Identifier: AGPL-3.0-or-later
//! Workdir sandboxing.
//!
//! ARCH §7 (structural invariants):
//!   1. The agent workdir is always a fresh `tempfile::TempDir`.
//!   2. Held-out fixture directories live outside that workdir and
//!      are NEVER copied in during the run — the witness pipeline
//!      copies them in *after* the agent exits.
//!   3. Env scrub for the subprocess strips every model credential
//!      so the agent cannot authenticate against any model except
//!      the local daemon.
//!
//! Two enforcement paths matter:
//!   - construction: `Sandbox::new(...)` is the only way to bind a
//!     workdir to a fixture, and it never hands the fixture path to
//!     the runner.
//!   - env scrub: `Sandbox::scrubbed_env` returns the minimal env
//!     map passed to `tokio::process::Command::env_clear`'d children.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

/// Per-problem sandbox. Holds both the workdir (mutable; agent writes
/// here) and the fixture-source path (read-only; harness copies from
/// here only after the agent exits).
#[derive(Debug)]
pub(crate) struct Sandbox {
    workdir: TempDir,
    fixture_source: PathBuf,
}

impl Sandbox {
    /// Build a sandbox. `fixture_source` is the absolute path to the
    /// problem's `fixtures/` directory under `sovereign/bench/agent-coding/`.
    /// It is checked for existence here so an authoring mistake fails
    /// at sandbox construction rather than after the agent has burned
    /// budget.
    pub(crate) fn new(fixture_source: PathBuf) -> std::io::Result<Self> {
        if !fixture_source.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "fixture source not found or not a directory: {}",
                    fixture_source.display()
                ),
            ));
        }
        let workdir = tempfile::Builder::new()
            .prefix("sovereign-agent-bench-")
            .tempdir()?;
        Ok(Self {
            workdir,
            fixture_source,
        })
    }

    /// Copy a scaffold directory into the workdir before the agent
    /// runs. Used by the Scaffolded tier — the agent finds a working
    /// `Cargo.toml` + `src/lib.rs` stub in place and only needs to
    /// fill in the algorithm.
    ///
    /// Failure is loud: an authoring mistake (scaffold dir missing
    /// or unreadable) is surfaced before any model time is spent.
    pub(crate) fn install_scaffold(&self, scaffold_source: &Path) -> std::io::Result<()> {
        if !scaffold_source.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "scaffold source not found or not a directory: {}",
                    scaffold_source.display()
                ),
            ));
        }
        copy_dir_into(scaffold_source, self.workdir.path())
    }

    /// The path the agent will see as its cwd. Public because the
    /// runner needs it; the fixture path is intentionally NOT public.
    pub(crate) fn workdir(&self) -> &Path {
        self.workdir.path()
    }

    /// Take ownership of the workdir. Used when assembling the
    /// `AgentRunContext` — the context owns the TempDir, which is
    /// then handed back to the witness via the artifact.
    pub(crate) fn into_workdir(self) -> (TempDir, PathBuf) {
        (self.workdir, self.fixture_source)
    }

    /// Minimal env for a sandboxed subprocess. `PATH` and `HOME` are
    /// preserved so the agent can find common tools (`cargo`, `go`,
    /// `node`, `python`); model credentials are intentionally dropped
    /// so the agent can only talk to the local daemon.
    ///
    /// `extra` carries runner-specific keys (e.g. `PI_PROVIDER_URL`)
    /// that the harness needs to inject explicitly.
    pub(crate) fn scrubbed_env(extra: &[(&str, &str)]) -> HashMap<String, String> {
        let mut out = HashMap::new();
        for key in ["PATH", "HOME"].iter().copied() {
            if let Ok(v) = std::env::var(key) {
                out.insert(key.to_string(), v);
            }
        }
        out.insert("LANG".to_string(), "C.UTF-8".to_string());
        for (k, v) in extra {
            out.insert((*k).to_string(), (*v).to_string());
        }
        out
    }
}

fn copy_dir_into(src: &Path, dst: &Path) -> std::io::Result<()> {
    if !dst.exists() {
        std::fs::create_dir_all(dst)?;
    }
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if ft.is_dir() {
            copy_dir_into(&entry.path(), &target)?;
        } else if ft.is_file() {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_missing_fixture_source() {
        let err = Sandbox::new(PathBuf::from("/this/does/not/exist")).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn workdir_is_fresh_tempdir() {
        let fx = tempfile::tempdir().unwrap();
        let s = Sandbox::new(fx.path().to_path_buf()).unwrap();
        // Workdir exists and is different from the fixture source.
        assert!(s.workdir().is_dir());
        assert_ne!(s.workdir(), fx.path());
    }

    #[test]
    fn scrubbed_env_keeps_path_and_lang() {
        let env = Sandbox::scrubbed_env(&[("PI_PROVIDER_URL", "http://localhost:9741/v1")]);
        assert!(env.contains_key("PATH"));
        assert_eq!(env.get("LANG").map(String::as_str), Some("C.UTF-8"));
        assert_eq!(
            env.get("PI_PROVIDER_URL").map(String::as_str),
            Some("http://localhost:9741/v1")
        );
    }

    #[test]
    fn install_scaffold_populates_workdir() {
        let fx = tempfile::tempdir().unwrap();
        let scaffold = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(scaffold.path().join("src")).unwrap();
        std::fs::write(
            scaffold.path().join("Cargo.toml"),
            "[package]\nname=\"x\"\n",
        )
        .unwrap();
        std::fs::write(scaffold.path().join("src/lib.rs"), "pub fn solve(){}\n").unwrap();

        let sb = Sandbox::new(fx.path().to_path_buf()).unwrap();
        sb.install_scaffold(scaffold.path()).unwrap();
        assert!(sb.workdir().join("Cargo.toml").is_file());
        assert!(sb.workdir().join("src/lib.rs").is_file());
        let body = std::fs::read_to_string(sb.workdir().join("src/lib.rs")).unwrap();
        assert!(body.contains("solve"));
    }

    #[test]
    fn install_scaffold_rejects_missing_source() {
        let fx = tempfile::tempdir().unwrap();
        let sb = Sandbox::new(fx.path().to_path_buf()).unwrap();
        let err = sb
            .install_scaffold(std::path::Path::new("/does/not/exist"))
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn scrubbed_env_drops_oai_credentials() {
        // Set credentials in the parent env to confirm the scrubbed
        // map does NOT propagate them.
        std::env::set_var("OPENAI_API_KEY", "should-not-leak");
        std::env::set_var("ANTHROPIC_API_KEY", "should-not-leak");
        let env = Sandbox::scrubbed_env(&[]);
        assert!(!env.contains_key("OPENAI_API_KEY"));
        assert!(!env.contains_key("ANTHROPIC_API_KEY"));
        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("ANTHROPIC_API_KEY");
    }
}
