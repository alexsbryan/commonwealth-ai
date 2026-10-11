// SPDX-License-Identifier: AGPL-3.0-or-later
//! `[daemon] loopback` — what a local process that presents no credential is
//! on this daemon. Declared, never inferred from what happens to be on disk.
//!
//! Until 2026-10-08 the posture was inferred: a daemon holding at least one
//! API key (`<sub>.key`) was keyed for its lifetime, and loopback granted
//! nothing. Named tokens and API keys are now one record, so "a key exists"
//! no longer says which posture an operator meant, and "add a key" no longer
//! says whether they meant to enter keyed mode. The posture is a declaration
//! (ADDRESSED_TEXT D5):
//!
//! - `owner` — a local process presenting nothing is the owner; named
//!   credentials are clients with names. A desktop's posture.
//! - `none` — loopback grants nothing; every caller presents a credential. An
//!   on-prem box behind nginx on the same host.
//!
//! An install that declares nothing keeps what it had: `none` when the store
//! holds a legacy API key file, `owner` otherwise, and the boot names the
//! inference and asks for the declaration. No daemon's posture flips on
//! upgrade. Minting refuses until the posture is declared
//! ([`super::ClientTokenStore::mint`]).

use std::path::Path;

/// `[daemon] loopback`. **CLOSED SET**, resolved once by
/// [`LoopbackPosture::resolve`] and carried on the node part.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Loopback {
    /// A local process presenting nothing is the owner.
    Owner,
    /// Loopback grants nothing: every caller presents a credential.
    None,
}

impl Loopback {
    /// Parse a configured value. An unknown one refuses rather than falling
    /// back (ARCH 6): an operator who typed `loopback = "nobody"` asked for
    /// something, and neither posture is safe to guess.
    pub fn parse(raw: &str) -> Result<Self, UnknownLoopback> {
        match raw.trim() {
            "owner" => Ok(Self::Owner),
            "none" => Ok(Self::None),
            other => Err(UnknownLoopback {
                value: other.to_string(),
            }),
        }
    }

    /// The configured spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::None => "none",
        }
    }
}

/// `[daemon] loopback` named something that is not a posture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownLoopback {
    /// What was configured, so the refusal can show it back.
    pub value: String,
}

impl std::fmt::Display for UnknownLoopback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[daemon] loopback = '{}' is not a loopback posture — it is \"owner\" (a local \
             process presenting nothing is the owner) or \"none\" (every caller presents a \
             credential)",
            self.value
        )
    }
}

impl std::error::Error for UnknownLoopback {}

/// The posture this daemon runs under, and whether it was declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopbackPosture {
    /// What loopback is.
    pub loopback: Loopback,
    /// `false` when `[daemon] loopback` is absent and `loopback` was inferred
    /// from the store, the pre-declaration rule.
    pub declared: bool,
}

impl Default for LoopbackPosture {
    /// A daemon with no data directory and no declaration: what an unkeyed
    /// install has always been.
    fn default() -> Self {
        Self {
            loopback: Loopback::Owner,
            declared: false,
        }
    }
}

impl LoopbackPosture {
    /// THE one decider. `declared` is `[daemon] loopback` as configured;
    /// `dir` is the credential store (`<data_dir>/client-tokens`). With no
    /// declaration, the posture is today's inference: `none` iff the store
    /// holds a legacy API key file (`<sub>.key`), `owner` otherwise.
    pub fn resolve(declared: Option<&str>, dir: Option<&Path>) -> Result<Self, UnknownLoopback> {
        if let Some(raw) = declared {
            return Ok(Self {
                loopback: Loopback::parse(raw)?,
                declared: true,
            });
        }
        let keyed = dir.is_some_and(super::keys::holds_a_key_file);
        Ok(Self {
            loopback: if keyed {
                Loopback::None
            } else {
                Loopback::Owner
            },
            declared: false,
        })
    }

    /// Whether a local process presenting nothing is the owner.
    pub fn loopback_is_owner(&self) -> bool {
        self.loopback == Loopback::Owner
    }

    /// Say, once at boot, how the posture was decided. An inference is a
    /// warning when it reads `none` (an install with keys that must declare
    /// it), and debug otherwise.
    pub fn log(&self) {
        match (self.declared, self.loopback) {
            (true, l) => tracing::info!(
                loopback = l.as_str(),
                "client_tokens: [daemon] loopback declared"
            ),
            (false, Loopback::None) => tracing::warn!(
                "client_tokens: [daemon] loopback is not declared and this daemon holds API keys, \
                 so loopback grants nothing (`none`), as before. Declare it: add \
                 `loopback = \"none\"` under [daemon]. Minting a credential refuses until it is \
                 declared."
            ),
            (false, Loopback::Owner) => tracing::info!(
                "client_tokens: [daemon] loopback is not declared and this daemon holds no API \
                 keys, so a local process is the owner (`owner`), as before. Declare it to mint \
                 a credential: add `loopback = \"owner\"` under [daemon]."
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Failing input: infer from any record rather than from a legacy key
    /// file, and a store holding only a named token reads `none`, which 401s
    /// that machine's own desktop.
    #[test]
    fn an_undeclared_posture_is_todays_inference_and_a_declared_one_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("client-tokens");
        let undeclared = |d: &Path| LoopbackPosture::resolve(None, Some(d)).unwrap();
        assert_eq!(undeclared(&dir).loopback, Loopback::Owner, "no store");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("laptop.token"), "tok").unwrap();
        assert_eq!(
            undeclared(&dir).loopback,
            Loopback::Owner,
            "a named token alone never made a daemon keyed"
        );
        std::fs::write(dir.join("firm.key"), "tok-firm\n").unwrap();
        let keyed = undeclared(&dir);
        assert_eq!(keyed.loopback, Loopback::None);
        assert!(!keyed.declared);
        let owner = LoopbackPosture::resolve(Some("owner"), Some(&dir)).unwrap();
        assert_eq!(
            owner.loopback,
            Loopback::Owner,
            "a declaration outranks the disk"
        );
        assert!(owner.declared);
        assert_eq!(
            LoopbackPosture::resolve(Some(" none "), None)
                .unwrap()
                .loopback,
            Loopback::None
        );
        let err = LoopbackPosture::resolve(Some("nobody"), None).unwrap_err();
        assert!(err.to_string().contains("\"owner\""), "{err}");
    }
}
