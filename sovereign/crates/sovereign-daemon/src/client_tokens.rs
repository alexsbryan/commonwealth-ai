// SPDX-License-Identifier: AGPL-3.0-or-later
//! Named client tokens — one credential per device, revocable alone.
//!
//! The client API (`:9741`) has had exactly one remote credential: the
//! daemon-wide bearer on [`NodePart::client_token`], fixed when the state is
//! built. Lending the API to a laptop therefore meant handing out the token
//! everything else already holds, and taking it back meant rotating it for
//! everyone — the desktop, the editor extension, the other machine, the
//! script on the NAS. `docs/THREAT_MODEL.md` §Known gaps entry 3.
//!
//! A named token is not a new kind of principal. It is a second spelling of
//! the credential [`crate::client_principal`] already resolves to
//! [`Principal::RemoteClient`], carrying a LABEL the operator chose, so the
//! fingerprint that principal keys on has a name an operator can read and
//! revoke by.
//!
//! [`Principal::RemoteClient`]: sovereign_serving_host::admission::Principal::RemoteClient
//! [`NodePart::client_token`]: crate::state::node::NodePart::client_token
//!
//! ## Where a token lives, and why on disk at all
//!
//! `<data_dir>/client-tokens/<label>.token`, file `0600` and directory
//! `0700` — the file-per-name shape of the MCP secret store
//! (`studio/.../mcp/secret_store.rs`), reused as a SHAPE and not as code: that
//! store hardcodes `~/.svrnmesh/secrets/mcp` and this one is told its
//! directory, because the daemon under test must not mint into the operator's.
//! The reason the shape is right is the same one: a credential that lives in
//! `config.toml` rides along with everything that file is copied, synced,
//! screenshared and attached to a bug report by.
//!
//! ## Why a fingerprint map, and why the token is still compared
//!
//! The in-memory map is keyed by [`crate::client_principal::fingerprint`] —
//! the same non-cryptographic bucket key the resolver already computes for
//! every bearer — so the map, its `Debug`, and every log line it feeds carry
//! no token. It is a BUCKET, not a proof: a hash lookup admitting on its own
//! would make a collision a credential. So the entry carries the token too and
//! the admission is still a constant-time byte compare, exactly as the shared
//! token's is. One extra secret in process memory, held the same way
//! [`NodePart::client_token`] already is.
//!
//! ## Revocation is same-lifetime, which is the whole point
//!
//! [`ClientTokenStore::revoke`] mutates the map AND deletes the file, in that
//! order. Deleting only the file would revoke at the next load — that is, at
//! the next daemon restart — and a credential you can only withdraw by
//! restarting the node is the gap this module closes rather than a smaller
//! version of it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use commonwealth_core::ct::constant_time_eq;

use crate::client_principal::fingerprint;

/// What the client API accepts as a remote credential. **CLOSED SET**,
/// resolved once from `[daemon] client_tokens` and carried on the node part —
/// no request path reads config. Mirrors `[daemon] internal_auth` on the other
/// port (see [`crate::internal_gate::InternalAuth`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClientTokens {
    /// **The default.** The daemon-wide token admits, and so does any named
    /// token. The posture every build before this one had, plus the new
    /// credentials — so a node that upgrades keeps serving the tools already
    /// pointed at it.
    #[default]
    Shared,
    /// Only a named token admits. The daemon-wide token is refused with a
    /// sentence naming what to do instead. For a node whose shared token has
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

    /// Whether the daemon-wide token may admit a remote caller under this
    /// posture. THE one decider — [`crate::client_auth`] asks it and nothing
    /// else re-derives the rule.
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
             named one) or \"named-only\" (only a token minted with \
             `svrn mesh token --new <label>` admits)",
            self.value
        )
    }
}

impl std::error::Error for UnknownClientTokens {}

/// One named token as the store holds it. Private: the token never leaves the
/// store except at mint, when the operator is the one who asked for it.
#[derive(Debug, Clone)]
struct Named {
    label: String,
    token: String,
}

/// A named token as `list` reports it — the label and the bucket key, never
/// the secret. The fingerprint is what a `client_auth` admit line carries, so
/// a row here can be matched against a log line without either holding a
/// credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedTokenRow {
    /// The name the operator minted it under, and revokes it by.
    pub label: String,
    /// The bucket key [`crate::client_principal`] computes for this token —
    /// what an admit line carries in place of the credential.
    pub fingerprint: String,
}

/// A label that cannot be a file name, or is already taken. Carries the
/// sentence the operator reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadLabel(pub String);

impl std::fmt::Display for BadLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BadLabel {}

/// The named tokens this node admits: the on-disk set, loaded once at start,
/// and mutated in place by the routes.
#[derive(Debug, Default)]
pub struct ClientTokenStore {
    /// `<data_dir>/client-tokens`. `None` on a daemon with no data directory
    /// (tests, in-process states) — minting then refuses rather than keeping a
    /// credential that would vanish with the process.
    dir: Option<PathBuf>,
    by_fingerprint: Mutex<HashMap<String, Named>>,
}

/// A label must be a file name and nothing else: the store's whole on-disk
/// addressing is `<label>.token`, so a label carrying a separator or a `..`
/// would write outside the directory it was told to use.
fn check_label(label: &str) -> Result<&str, BadLabel> {
    let l = label.trim();
    if l.is_empty() {
        return Err(BadLabel("a token needs a label — `--new laptop`".into()));
    }
    if l.len() > 64 {
        return Err(BadLabel(format!(
            "label '{l}' is longer than 64 characters"
        )));
    }
    if !l
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(BadLabel(format!(
            "label '{l}' may hold only letters, digits, '-' and '_' — it is the \
             name of a file under <data_dir>/client-tokens"
        )));
    }
    Ok(l)
}

impl ClientTokenStore {
    /// Read every `<label>.token` under `dir`, or start empty when there is no
    /// data directory.
    ///
    /// An unreadable file is reported and skipped rather than failing the
    /// boot: one corrupt credential must not take the node down, and the
    /// operator learns which one from the log rather than from a device that
    /// mysteriously stopped being admitted (ARCH principle 6 — the absence is
    /// named, never defaulted away).
    pub fn load(dir: Option<PathBuf>) -> Self {
        let mut map = HashMap::new();
        if let Some(d) = dir.as_deref() {
            for (label, token) in read_dir_tokens(d) {
                map.insert(fingerprint(&token), Named { label, token });
            }
        }
        tracing::debug!(
            dir = ?dir,
            named_tokens = map.len(),
            "client_tokens: loaded the named client-token set"
        );
        Self {
            dir,
            by_fingerprint: Mutex::new(map),
        }
    }

    /// The label admitting `presented`, or `None`.
    ///
    /// The fingerprint picks the bucket; the byte compare is what admits. See
    /// the module docs for why it is both and not either.
    pub fn label_for(&self, presented: &str) -> Option<String> {
        let guard = self
            .by_fingerprint
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let named = guard.get(&fingerprint(presented))?;
        constant_time_eq(presented.as_bytes(), named.token.as_bytes()).then(|| named.label.clone())
    }

    /// Record `token` under `label`, writing it to disk and admitting it from
    /// the next request on.
    ///
    /// The token is a parameter, not minted here — entropy is injected so the
    /// store is testable without an RNG, the same choice
    /// [`sovereign_grants::GuestGrantStore::issue`] made. A label already in
    /// use is refused rather than replaced: "mint" and "rotate" are different
    /// asks, and quietly doing the second when asked for the first withdraws a
    /// live credential the operator did not mention.
    pub fn mint(&self, label: &str, token: String) -> Result<NamedTokenRow, BadLabel> {
        let label = check_label(label)?.to_string();
        let Some(dir) = self.dir.clone() else {
            return Err(BadLabel(
                "this daemon has no data directory, so a named token could not \
                 outlive the process — refusing to mint one"
                    .into(),
            ));
        };
        let mut guard = self
            .by_fingerprint
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if guard.values().any(|n| n.label == label) {
            return Err(BadLabel(format!(
                "a token named '{label}' already exists — revoke it first \
                 (`svrn mesh token --revoke {label}`) if you meant to replace it"
            )));
        }
        write_token(&dir, &label, &token)
            .map_err(|e| BadLabel(format!("could not write the token file: {e}")))?;
        let fp = fingerprint(&token);
        guard.insert(
            fp.clone(),
            Named {
                label: label.clone(),
                token,
            },
        );
        tracing::info!(
            label = %label,
            fingerprint = %fp,
            "client_tokens: minted a named client token"
        );
        Ok(NamedTokenRow {
            label,
            fingerprint: fp,
        })
    }

    /// Stop admitting the token named `label`, in this daemon's lifetime.
    ///
    /// The map is mutated FIRST and the file deleted after, so there is no
    /// window in which the credential is gone from disk and still admitted.
    /// Returns false when no such label existed — "nothing to revoke" is
    /// reported rather than read as success, because an operator who mistyped
    /// a label must not walk away believing a live credential is dead.
    pub fn revoke(&self, label: &str) -> bool {
        let label = label.trim();
        let mut guard = self
            .by_fingerprint
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let Some(fp) = guard
            .iter()
            .find(|(_, n)| n.label == label)
            .map(|(fp, _)| fp.clone())
        else {
            return false;
        };
        guard.remove(&fp);
        if let Some(dir) = self.dir.as_deref() {
            if let Err(e) = delete_token(dir, label) {
                tracing::warn!(
                    label = %label,
                    "client_tokens: revoked in memory but the token file would \
                     not delete: {e}"
                );
            }
        }
        tracing::info!(
            label = %label,
            fingerprint = %fp,
            "client_tokens: revoked a named client token"
        );
        true
    }

    /// Every named token, label-sorted so the rendering is stable.
    pub fn list(&self) -> Vec<NamedTokenRow> {
        let guard = self
            .by_fingerprint
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<NamedTokenRow> = guard
            .iter()
            .map(|(fp, n)| NamedTokenRow {
                label: n.label.clone(),
                fingerprint: fp.clone(),
            })
            .collect();
        out.sort_by(|a, b| a.label.cmp(&b.label));
        out
    }
}

// ── the on-disk half: `<dir>/<label>.token`, 0600 in a 0700 directory ───────

fn token_path(dir: &Path, label: &str) -> PathBuf {
    dir.join(format!("{label}.token"))
}

fn read_dir_tokens(dir: &Path) -> Vec<(String, String)> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        // Not an error: a node that has never minted one has no directory.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!(dir = ?dir, "client_tokens: token directory unreadable: {e}");
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(label) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".token"))
        else {
            continue;
        };
        if check_label(label).is_err() {
            tracing::warn!(
                ?path,
                "client_tokens: skipping a file whose name is not a label"
            );
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(raw) if !raw.trim().is_empty() => {
                out.push((label.to_string(), raw.trim().to_string()))
            }
            Ok(_) => tracing::warn!(?path, "client_tokens: token file is empty — skipping"),
            Err(e) => tracing::warn!(?path, "client_tokens: token file unreadable: {e}"),
        }
    }
    out
}

fn write_token(dir: &Path, label: &str, token: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    harden_dir(dir);
    let path = token_path(dir, label);
    std::fs::write(&path, token.trim().as_bytes())?;
    harden_file(&path);
    Ok(())
}

fn delete_token(dir: &Path, label: &str) -> std::io::Result<()> {
    match std::fs::remove_file(token_path(dir, label)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
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
mod tests {
    use super::*;

    #[test]
    fn the_posture_is_a_closed_set_and_an_unknown_spelling_refuses() {
        assert_eq!(ClientTokens::parse("shared"), Ok(ClientTokens::Shared));
        assert_eq!(
            ClientTokens::parse(" named-only "),
            Ok(ClientTokens::NamedOnly)
        );
        assert_eq!(ClientTokens::default(), ClientTokens::Shared);
        assert!(ClientTokens::Shared.admits_shared_token());
        assert!(!ClientTokens::NamedOnly.admits_shared_token());
        let err = ClientTokens::parse("named").unwrap_err();
        assert_eq!(err.value, "named");
        assert!(err.to_string().contains("named-only"));
    }

    #[test]
    fn a_minted_token_admits_under_its_label_and_a_wrong_one_does_not() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ClientTokenStore::load(Some(tmp.path().join("client-tokens")));
        store.mint("laptop", "tok-laptop".into()).unwrap();
        store.mint("tablet", "tok-tablet".into()).unwrap();
        assert_eq!(store.label_for("tok-laptop").as_deref(), Some("laptop"));
        assert_eq!(store.label_for("tok-tablet").as_deref(), Some("tablet"));
        assert_eq!(store.label_for("tok-nobody"), None);
        assert_eq!(
            store
                .list()
                .into_iter()
                .map(|r| r.label)
                .collect::<Vec<_>>(),
            vec!["laptop".to_string(), "tablet".to_string()]
        );
    }

    /// Clause (b) at the store: the revoked label stops admitting IN THIS
    /// STORE, with no reload, and its sibling is untouched.
    #[test]
    fn revoking_one_label_leaves_the_other_admitting_with_no_reload() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("client-tokens");
        let store = ClientTokenStore::load(Some(dir.clone()));
        store.mint("laptop", "tok-laptop".into()).unwrap();
        store.mint("tablet", "tok-tablet".into()).unwrap();

        assert!(store.revoke("laptop"));
        assert_eq!(
            store.label_for("tok-laptop"),
            None,
            "a revoked label must stop admitting without a reload"
        );
        assert_eq!(store.label_for("tok-tablet").as_deref(), Some("tablet"));
        assert!(!dir.join("laptop.token").exists());
        assert!(dir.join("tablet.token").exists());
        // Revoking what is not there is reported, not counted as success.
        assert!(!store.revoke("laptop"));
    }

    #[test]
    fn the_set_survives_a_reload_and_a_revoked_one_does_not_come_back() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("client-tokens");
        let store = ClientTokenStore::load(Some(dir.clone()));
        store.mint("laptop", "tok-laptop".into()).unwrap();
        store.mint("tablet", "tok-tablet".into()).unwrap();
        store.revoke("laptop");

        let reloaded = ClientTokenStore::load(Some(dir));
        assert_eq!(reloaded.label_for("tok-tablet").as_deref(), Some("tablet"));
        assert_eq!(reloaded.label_for("tok-laptop"), None);
    }

    #[test]
    fn a_label_that_is_not_a_file_name_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ClientTokenStore::load(Some(tmp.path().join("client-tokens")));
        for bad in ["", "  ", "../escape", "a/b", "sp ace", &"x".repeat(65)] {
            assert!(
                store.mint(bad, "tok".into()).is_err(),
                "label {bad:?} must be refused"
            );
        }
        store.mint("laptop", "tok-laptop".into()).unwrap();
        // Minting over a live label is refused rather than silently rotating.
        assert!(store.mint("laptop", "tok-other".into()).is_err());
        assert_eq!(store.label_for("tok-laptop").as_deref(), Some("laptop"));
    }

    #[cfg(unix)]
    #[test]
    fn the_token_file_is_0600_in_a_0700_directory() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("client-tokens");
        let store = ClientTokenStore::load(Some(dir.clone()));
        store.mint("laptop", "tok-laptop".into()).unwrap();
        let file = std::fs::metadata(dir.join("laptop.token")).unwrap();
        assert_eq!(file.permissions().mode() & 0o777, 0o600);
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    /// A store with nowhere to persist refuses to mint rather than handing
    /// back a credential that dies with the process.
    #[test]
    fn a_store_with_no_directory_refuses_to_mint() {
        let store = ClientTokenStore::load(None);
        assert!(store.mint("laptop", "tok".into()).is_err());
        assert!(store.list().is_empty());
    }
}
