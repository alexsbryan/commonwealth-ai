// SPDX-License-Identifier: AGPL-3.0-or-later
//! Named credentials — one record `{name, token, groups}`, one store, one CLI
//! (`svrn daemon key`), one resolution arm (ADDRESSED_TEXT §5.5 rule 2).
//!
//! A named credential resolves to [`Principal::Asserted`]`{ sub: name, groups
//! }`: the principal already meant "a subject an issuer asserted", and a
//! named client is one. Until 2026-10-08 this store held two record kinds — a
//! device token (`<label>.token`, minted by `svrn mesh token`, resolving to
//! `Principal::RemoteClient` with a label beside it) and an on-prem API key
//! (`<sub>.key`, written by `svrn daemon key`, resolving to `Asserted`) — with
//! two CLIs and two arms for one thing. Both forms on disk are read as the one
//! record; legacy `.token` files are rewritten as `.key` once `[daemon]
//! loopback` is declared ([`LoopbackPosture`]).
//!
//! [`Principal::Asserted`]: sovereign_contracts::principal::Principal::Asserted
//!
//! ## Where a credential lives, and why on disk at all
//!
//! `<data_dir>/client-tokens/<name>.key`, file `0600` and directory `0700` —
//! the file-per-name shape of the MCP secret store
//! (`svrn/crates/sovereign-tools-base/src/mcp/secret_store.rs`), reused as a
//! SHAPE and not as code: that store hardcodes `~/.svrnmesh/secrets/mcp` and
//! this one is told its directory, because the daemon under test must not
//! mint into the operator's. A credential that lives in `config.toml` rides
//! along with everything that file is copied, synced, screenshared and
//! attached to a bug report by.
//!
//! ## Why a fingerprint map, and why the token is still compared
//!
//! The in-memory map is keyed by [`crate::client_principal::fingerprint`], so
//! the map, its `Debug`, and every log line it feeds carry no token. It is a
//! BUCKET, not a proof: a hash lookup admitting on its own would make a
//! collision a credential. So the entry carries the token too and the
//! admission is a constant-time byte compare.
//!
//! ## Revocation is same-lifetime, which is the whole point
//!
//! [`ClientTokenStore::revoke`] mutates the map AND deletes the file, in that
//! order. Deleting only the file would revoke at the next daemon restart, and
//! a credential you can only withdraw by restarting the node is the gap this
//! module closes. `svrn daemon key` reaches a running daemon through its
//! owner-only routes, so a revoke from the CLI is live too.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use subtle::ConstantTimeEq;

use crate::client_principal::fingerprint;

pub mod keys;
mod posture;
pub use keys::KEY_ADMIN_GROUP;
pub use posture::{Loopback, LoopbackPosture, UnknownLoopback};

/// `<data_dir>/client-tokens` — the one spelling of where named credentials
/// live, read by the node seed, the boot's posture and `svrn daemon key`.
pub fn client_tokens_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("client-tokens")
}

/// Whether loopback grants nothing under `[daemon] loopback` = `declared` and
/// the store under `data_dir`: the answer the boot's readers that run before
/// the node seed take (the turn's owner resolver, the OCR pass's own
/// credential). The seed's decider, [`LoopbackPosture::resolve`], so the two
/// cannot disagree.
pub fn loopback_grants_nothing(
    declared: Option<&str>,
    data_dir: &Path,
) -> Result<bool, UnknownLoopback> {
    LoopbackPosture::resolve(declared, Some(&client_tokens_dir(data_dir)))
        .map(|p| p.loopback == Loopback::None)
}

/// What the client API accepts as the daemon-wide credential. **CLOSED SET**,
/// resolved once from `[daemon] client_tokens` and carried on the node part —
/// no request path reads config. Mirrors `[daemon] internal_auth` on the other
/// port (see [`crate::internal_gate::InternalAuth`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClientTokens {
    /// **The default.** The daemon-wide token admits, and so does any named
    /// credential. The posture every build before this one had, so a node
    /// that upgrades keeps serving the tools already pointed at it.
    #[default]
    Shared,
    /// Only a named credential admits. The daemon-wide token is refused with
    /// a sentence naming what to do instead. For a node whose shared token has
    /// been handed around enough that its holder set is no longer known.
    NamedOnly,
}

impl ClientTokens {
    /// Parse the configured value. Refuses an unknown one rather than falling
    /// back to a default (ARCH principle 6): an operator who typed
    /// `client_tokens = "named"` asked for the strict posture and must not
    /// silently get the permissive one.
    pub fn parse(raw: &str) -> Result<Self, UnknownClientTokens> {
        match raw.trim() {
            "shared" => Ok(Self::Shared),
            "named-only" => Ok(Self::NamedOnly),
            other => Err(UnknownClientTokens {
                value: other.to_string(),
            }),
        }
    }

    /// The configured spelling, for a trace that says which posture is live.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::NamedOnly => "named-only",
        }
    }

    /// Whether the daemon-wide token may admit a caller under this posture.
    /// THE one decider — [`crate::client_principal`] asks it and nothing else
    /// re-derives the rule.
    pub fn admits_shared_token(&self) -> bool {
        matches!(self, Self::Shared)
    }
}

/// `[daemon] client_tokens` named something that is not a posture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownClientTokens {
    /// What was configured, so the refusal can show it back.
    pub value: String,
}

impl std::fmt::Display for UnknownClientTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[daemon] client_tokens = '{}' is not a client-credential posture — \
             it is \"shared\" (the daemon-wide token admits, and so does any \
             named one) or \"named-only\" (only a credential minted with \
             `svrn daemon key --add <name>` admits)",
            self.value
        )
    }
}

impl std::error::Error for UnknownClientTokens {}

/// One named credential as the store holds it. Private: the token never
/// leaves the store except at mint, when the operator is the one who asked.
#[derive(Debug, Clone)]
pub(crate) struct Named {
    pub(crate) name: String,
    pub(crate) token: String,
    pub(crate) groups: Vec<String>,
}

/// A named credential as `list` reports it — the name, its groups and the
/// bucket key, never the secret. The fingerprint is what a log line carries,
/// so a row can be matched against a log line without either holding a
/// credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialRow {
    /// The name it was minted under: the asserted subject, what a call is
    /// logged by, and what it is revoked by.
    pub name: String,
    /// The groups it asserts. `admin` reaches ingest, the admin routes and
    /// minting.
    pub groups: Vec<String>,
    /// [`fingerprint`] of the secret.
    pub fingerprint: String,
}

/// A name that cannot be a file name, is already taken, or a mint this store
/// refuses. Carries the sentence the operator reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadLabel(pub String);

impl std::fmt::Display for BadLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BadLabel {}

/// The `sub` the daemon's own credential asserts. A name holds no `@`, so no
/// key file can claim it.
pub const SELF_SUB: &str = "@svrn";

/// This process's own credential: minted once per process and never written
/// anywhere, asserting [`SELF_SUB`] in the admin group. A store under
/// `loopback = "none"` admits it beside the records on disk; one under
/// `owner` does not. It is how the daemon's calls to its own client routes
/// (the OCR cleanup pass) are admitted by key, as every caller of such a
/// daemon is (phase-b-86), never by a loopback exemption. `None` when the OS
/// gave no entropy, named in the log; the caller then sends no key and the
/// refusal it gets says why.
pub fn self_credential() -> Option<&'static str> {
    static KEY: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    KEY.get_or_init(|| match crate::client_auth::generate_bearer_token() {
        Ok(token) => Some(token),
        Err(e) => {
            tracing::warn!("client_tokens: no self credential this process: {e}");
            None
        }
    })
    .as_deref()
}

/// The named credentials this node admits: the on-disk set, loaded once at
/// start, and mutated in place by the owner-only routes.
#[derive(Debug, Default)]
pub struct ClientTokenStore {
    /// `<data_dir>/client-tokens`. `None` on a daemon with no data directory
    /// (tests, in-process states) — minting then refuses rather than keeping
    /// a credential that would vanish with the process.
    dir: Option<PathBuf>,
    /// How loopback is treated, decided before the store was opened.
    posture: LoopbackPosture,
    by_fingerprint: Mutex<HashMap<String, Named>>,
}

/// A name must be a file name and nothing else: the store's whole on-disk
/// addressing is `<name>.key`, so a name carrying a separator or a `..` would
/// write outside the directory it was told to use.
fn check_label(label: &str) -> Result<&str, BadLabel> {
    let l = label.trim();
    if l.is_empty() {
        return Err(BadLabel(
            "a credential needs a name — `--add claude-code`".into(),
        ));
    }
    if l.len() > 64 {
        return Err(BadLabel(format!("name '{l}' is longer than 64 characters")));
    }
    if !l
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(BadLabel(format!(
            "name '{l}' may hold only letters, digits, '-' and '_' — it is the \
             name of a file under <data_dir>/client-tokens"
        )));
    }
    Ok(l)
}

impl ClientTokenStore {
    /// Read every record under `dir` (`<name>.key`, and the legacy
    /// `<label>.token`), or start empty when there is no data directory.
    ///
    /// Under a DECLARED posture the legacy `.token` files are rewritten as
    /// `.key` first, so the disk converges on one form. An undeclared posture
    /// leaves them where they are: rewriting them would add `.key` files to a
    /// store whose posture is still inferred from them.
    ///
    /// An unreadable file is reported and skipped rather than failing the
    /// boot: one corrupt credential must not take the node down, and the
    /// operator learns which one from the log rather than from a device that
    /// mysteriously stopped being admitted (ARCH principle 6).
    pub fn load(dir: Option<PathBuf>, posture: LoopbackPosture) -> Self {
        if let (Some(d), true) = (dir.as_deref(), posture.declared) {
            keys::migrate_tokens(d);
        }
        let mut map = HashMap::new();
        for record in dir.as_deref().map(keys::read_records).unwrap_or_default() {
            map.insert(fingerprint(&record.token), record);
        }
        // The daemon's own credential joins only where loopback grants
        // nothing, so an owner-posture daemon never admits it.
        if posture.loopback == Loopback::None {
            if let Some(token) = self_credential() {
                map.insert(
                    fingerprint(token),
                    Named {
                        name: SELF_SUB.to_string(),
                        token: token.to_string(),
                        groups: vec![KEY_ADMIN_GROUP.to_string()],
                    },
                );
            }
        }
        tracing::debug!(
            dir = ?dir,
            credentials = map.len(),
            loopback = posture.loopback.as_str(),
            declared = posture.declared,
            "client_tokens: loaded the named credentials"
        );
        Self {
            dir,
            posture,
            by_fingerprint: Mutex::new(map),
        }
    }

    /// The posture this store was opened under.
    pub fn posture(&self) -> LoopbackPosture {
        self.posture
    }

    /// Whether loopback grants nothing on this daemon (`loopback = "none"`,
    /// declared or inferred). Fixed for the store's lifetime.
    pub fn is_keyed(&self) -> bool {
        self.posture.loopback == Loopback::None
    }

    /// The `(name, groups)` a presented bearer asserts, or `None` when it is
    /// not a live named credential. THE one resolution of a named credential:
    /// bucket by fingerprint, admit by constant-time compare.
    pub fn asserted_for(&self, presented: &str) -> Option<(String, Vec<String>)> {
        let guard = self
            .by_fingerprint
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let named = guard.get(&fingerprint(presented))?;
        bool::from(presented.as_bytes().ct_eq(named.token.as_bytes()))
            .then(|| (named.name.clone(), named.groups.clone()))
    }

    /// Record `token` under `name` asserting `groups`, writing it to disk and
    /// admitting it from the next request on.
    ///
    /// The token is a parameter, not minted here — entropy is injected so the
    /// store is testable without an RNG. A name already in use is refused
    /// rather than replaced: "mint" and "rotate" are different asks, and
    /// quietly doing the second withdraws a live credential. An undeclared
    /// posture refuses too: "add a key" used to mean "enter keyed mode", and
    /// with one record kind that intent is ambiguous until the operator says
    /// which posture they meant.
    pub fn mint(
        &self,
        name: &str,
        groups: &[String],
        token: String,
    ) -> Result<CredentialRow, BadLabel> {
        let name = check_label(name)?.to_string();
        for g in groups {
            check_label(g).map_err(|e| BadLabel(format!("group: {e}")))?;
        }
        if !self.posture.declared {
            return Err(BadLabel(format!(
                "[daemon] loopback is not declared, so it is not clear whether this credential \
                 is a named client of a daemon whose local processes are the owner, or one key \
                 of a daemon where loopback grants nothing. Declare one under [daemon] in \
                 config.toml — `loopback = \"owner\"` or `loopback = \"none\"` (inferred today: \
                 \"{}\") — and add it again",
                self.posture.loopback.as_str()
            )));
        }
        let Some(dir) = self.dir.clone() else {
            return Err(BadLabel(
                "this daemon has no data directory, so a credential could not outlive the \
                 process — refusing to mint one"
                    .into(),
            ));
        };
        let mut guard = self
            .by_fingerprint
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if guard.values().any(|n| n.name == name) || keys::record_exists(&dir, &name) {
            return Err(BadLabel(format!(
                "a credential named '{name}' already exists — revoke it first \
                 (`svrn daemon key --revoke {name}`) if you meant to replace it"
            )));
        }
        let record = Named {
            name: name.clone(),
            token,
            groups: groups.to_vec(),
        };
        keys::write_record(&dir, &record)
            .map_err(|e| BadLabel(format!("could not write the credential file: {e}")))?;
        let fp = fingerprint(&record.token);
        guard.insert(fp.clone(), record);
        tracing::info!(
            name = %name,
            groups = ?groups,
            fingerprint = %fp,
            "client_tokens: minted a named credential"
        );
        Ok(CredentialRow {
            name,
            groups: groups.to_vec(),
            fingerprint: fp,
        })
    }

    /// Stop admitting the credential named `name`, in this daemon's lifetime.
    ///
    /// The map is mutated FIRST and the file deleted after, so there is no
    /// window in which the credential is gone from disk and still admitted.
    /// Returns false when no such name existed — "nothing to revoke" is
    /// reported rather than read as success, because an operator who mistyped
    /// a name must not walk away believing a live credential is dead.
    /// Revoking is never refused on an undeclared posture.
    pub fn revoke(&self, name: &str) -> bool {
        let name = name.trim();
        let mut guard = self
            .by_fingerprint
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let found = guard
            .iter()
            .find(|(_, n)| n.name == name && n.name != SELF_SUB)
            .map(|(fp, _)| fp.clone());
        if let Some(fp) = &found {
            guard.remove(fp);
        }
        let on_disk = match self.dir.as_deref() {
            Some(dir) => keys::remove_record(dir, name).unwrap_or_else(|e| {
                tracing::warn!(
                    name = %name,
                    "client_tokens: revoked in memory but the credential file would not \
                     delete: {e}"
                );
                false
            }),
            None => false,
        };
        let revoked = found.is_some() || on_disk;
        if revoked {
            tracing::info!(name = %name, "client_tokens: revoked a named credential");
        }
        revoked
    }

    /// Every named credential, name-sorted so the rendering is stable. The
    /// daemon's own credential is not one an operator minted and is never
    /// listed.
    pub fn list(&self) -> Vec<CredentialRow> {
        let guard = self
            .by_fingerprint
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<CredentialRow> = guard
            .iter()
            .filter(|(_, n)| n.name != SELF_SUB)
            .map(|(fp, n)| CredentialRow {
                name: n.name.clone(),
                groups: n.groups.clone(),
                fingerprint: fp.clone(),
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

#[cfg(unix)]
fn harden_file(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}
#[cfg(unix)]
fn harden_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
}
#[cfg(not(unix))]
fn harden_file(_path: &Path) {}
#[cfg(not(unix))]
fn harden_dir(_path: &Path) {}

#[cfg(test)]
mod tests;
