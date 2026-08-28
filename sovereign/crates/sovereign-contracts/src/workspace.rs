// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which checkout this host's code tools operate on — the two configured
//! sources, read the same way by everyone who reads them.
//!
//! # The mirror that wasn't
//!
//! `sovereign-tools::code::read_notes::resolve_workspace_root` opens with
//! "mirroring the daemon's chain (`daemon_cmd/workspace.rs`)". It did not
//! mirror it, in two ways that both change which directory you get:
//!
//! | | daemon | `read_notes` |
//! |---|---|---|
//! | `SOVEREIGN_WORKSPACE_DIR` | trimmed, and must be a directory | non-empty only |
//! | the pin file | `svrnmesh_root()/workspace` | `$HOME/.svrnmesh/workspace` |
//!
//! So `SOVEREIGN_WORKSPACE_DIR=" "` was "unset" to the daemon and a workspace
//! at path `" "` to the notes tool; a path that does not exist was ignored
//! with a warning by one and returned by the other; and on a host with a
//! relocated root (`svrnmesh_root()` honours the rebrand override, a bare
//! `$HOME` join does not) the two read *different pin files*. A comment
//! asserting a mirror is exactly what ARCH §7.2 says an assertion in prose
//! cannot be.
//!
//! What is NOT here: each caller's own tail. The daemon auto-detects from its
//! install layout, `read_notes` ascends from the cwd looking for the repo
//! signature. Those are genuinely different questions — this module answers
//! only "what did the operator configure", and answers it once.

use std::path::PathBuf;

/// The operator-configured workspace directory, or `None` when neither
/// source names an existing one.
///
/// Order is load-bearing and is the daemon's: an explicit environment
/// override beats the pin file, because the env is how a launchd/systemd unit
/// or a container says which checkout it was given.
///
/// A configured-but-missing path yields `None` rather than the path. That is
/// the daemon's rule and it is the right one: a code tool handed a directory
/// that is not there produces empty results, which reads as "no notes" rather
/// than "wrong workspace" (ARCH §18.3 — absence is reported, never defaulted).
/// Callers that want to say so should log it; `explain` returns why.
pub fn configured_workspace_dir() -> Option<PathBuf> {
    explain().0
}

/// Why a source was or was not taken. Kept separate from
/// [`configured_workspace_dir`] so the daemon can keep logging its warning
/// without every caller paying for one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceSource {
    /// `SOVEREIGN_WORKSPACE_DIR` named an existing directory.
    Env,
    /// The pin file at `<svrnmesh_root>/workspace` named an existing one.
    PinFile,
    /// `SOVEREIGN_WORKSPACE_DIR` was set but is not a directory.
    EnvNotADirectory(PathBuf),
    /// The pin file was set but does not name a directory.
    PinFileNotADirectory(PathBuf),
    /// Neither source said anything.
    Unconfigured,
}

/// The resolution and the reason for it.
pub fn explain() -> (Option<PathBuf>, WorkspaceSource) {
    if let Ok(val) = std::env::var("SOVEREIGN_WORKSPACE_DIR") {
        match existing_dir(&val) {
            Ok(Some(p)) => return (Some(p), WorkspaceSource::Env),
            Ok(None) => {}
            Err(p) => return (None, WorkspaceSource::EnvNotADirectory(p)),
        }
    }
    let pin = crate::rebrand::svrnmesh_root().join("workspace");
    if let Ok(contents) = std::fs::read_to_string(&pin) {
        match existing_dir(&contents) {
            Ok(Some(p)) => return (Some(p), WorkspaceSource::PinFile),
            Ok(None) => {}
            Err(p) => return (None, WorkspaceSource::PinFileNotADirectory(p)),
        }
    }
    (None, WorkspaceSource::Unconfigured)
}

/// `Ok(Some)` = an existing directory, `Ok(None)` = the source said nothing,
/// `Err(path)` = it named something that is not a directory.
fn existing_dir(raw: &str) -> Result<Option<PathBuf>, PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let path = PathBuf::from(trimmed);
    if path.is_dir() {
        Ok(Some(path))
    } else {
        Err(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact input the two resolvers disagreed on.
    #[test]
    fn whitespace_is_not_a_workspace() {
        assert_eq!(existing_dir("   "), Ok(None));
        assert_eq!(existing_dir(""), Ok(None));
        assert_eq!(existing_dir("\n"), Ok(None));
    }

    /// The second disagreement: a configured path that is not there is a
    /// misconfiguration to report, not a directory to hand a code tool.
    #[test]
    fn a_path_that_is_not_a_directory_is_an_error_not_a_value() {
        let missing = "/definitely/not/a/real/directory/here";
        assert_eq!(existing_dir(missing), Err(PathBuf::from(missing)));
    }

    #[test]
    fn an_existing_directory_comes_back_trimmed() {
        let tmp = std::env::temp_dir();
        let padded = format!("  {}  ", tmp.display());
        assert_eq!(existing_dir(&padded), Ok(Some(tmp)));
    }
}
