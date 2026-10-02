// SPDX-License-Identifier: AGPL-3.0-or-later
//! On-prem API keys — the on-disk half (`<data_dir>/client-tokens/<sub>.key`).
//!
//! A key file is the secret on its first line and, optionally, the groups the
//! key asserts on a `groups = a, b` line:
//!
//! ```text
//! 3f9c…e1
//! groups = admin
//! ```
//!
//! The file name is the key's `sub`, under the same label rule a named token
//! has, because it is a file name and nothing else. This module reads, writes
//! and removes those files; [`super::ClientTokenStore`] answers who a
//! presented bearer is. One store, two credential kinds (principle 8): the
//! daemon never needed a second table to ask "who is this key".

use std::path::{Path, PathBuf};

use super::{check_label, harden_dir, harden_file, BadLabel};
use crate::client_principal::fingerprint;

/// The group whose keys reach the ingest and admin routes. Every other key is
/// a lawyer's: its own conversations and the read routes `crate::api_keys`
/// names.
pub const KEY_ADMIN_GROUP: &str = "admin";

/// One loaded key. Private to the store; the secret leaves only at mint.
#[derive(Debug, Clone)]
pub(super) struct ApiKey {
    pub(super) sub: String,
    pub(super) token: String,
    pub(super) groups: Vec<String>,
}

/// A key as `svrn daemon key --list` reports it: who, which groups, and the
/// bucket key a log line carries. Never the secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyRow {
    /// The subject the key asserts; its conversations are owned by it.
    pub sub: String,
    /// The groups the key asserts.
    pub groups: Vec<String>,
    /// [`fingerprint`] of the secret.
    pub fingerprint: String,
}

fn key_path(dir: &Path, sub: &str) -> PathBuf {
    dir.join(format!("{sub}.key"))
}

/// Parse a key file's text. `None` when it holds no secret.
fn parse_key(sub: &str, raw: &str) -> Option<ApiKey> {
    let mut lines = raw.lines().map(str::trim).filter(|l| !l.is_empty());
    let token = lines.next()?.to_string();
    let mut groups = Vec::new();
    for line in lines {
        if let Some(list) = line
            .strip_prefix("groups")
            .and_then(|r| r.trim_start().strip_prefix('='))
        {
            groups.extend(
                list.split(',')
                    .map(str::trim)
                    .filter(|g| !g.is_empty())
                    .map(str::to_string),
            );
        } else {
            tracing::warn!(
                sub,
                "client_tokens: key file line is not `groups = …` — ignored"
            );
        }
    }
    Some(ApiKey {
        sub: sub.to_string(),
        token,
        groups,
    })
}

/// Every `<sub>.key` under `dir`. A missing directory is no keys; an
/// unreadable or empty file is named and skipped, as a token file is.
pub(super) fn read_dir_keys(dir: &Path) -> Vec<ApiKey> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!(dir = ?dir, "client_tokens: key directory unreadable: {e}");
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(sub) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".key"))
        else {
            continue;
        };
        if check_label(sub).is_err() {
            tracing::warn!(
                ?path,
                "client_tokens: skipping a key whose name is not a label"
            );
            continue;
        }
        match std::fs::read_to_string(&path).map(|raw| parse_key(sub, &raw)) {
            Ok(Some(key)) => out.push(key),
            Ok(None) => tracing::warn!(?path, "client_tokens: key file is empty — skipping"),
            Err(e) => tracing::warn!(?path, "client_tokens: key file unreadable: {e}"),
        }
    }
    out.sort_by(|a, b| a.sub.cmp(&b.sub));
    out
}

/// Write a new key for `sub` asserting `groups`, mode 0600 in a 0700
/// directory. Refuses a `sub` or group that is not a label, and a `sub` that
/// already has a key: replacing one is a revoke and an add, said aloud.
pub fn add_key(
    dir: &Path,
    sub: &str,
    groups: &[String],
    token: &str,
) -> Result<ApiKeyRow, BadLabel> {
    let sub = check_label(sub)?;
    for g in groups {
        check_label(g).map_err(|e| BadLabel(format!("group: {e}")))?;
    }
    let path = key_path(dir, sub);
    if path.exists() {
        return Err(BadLabel(format!(
            "'{sub}' already has a key — revoke it first (`svrn daemon key --revoke {sub}`) \
             if you meant to replace it"
        )));
    }
    let mut body = format!("{}\n", token.trim());
    if !groups.is_empty() {
        body.push_str(&format!("groups = {}\n", groups.join(", ")));
    }
    std::fs::create_dir_all(dir)
        .and_then(|()| {
            harden_dir(dir);
            std::fs::write(&path, body)
        })
        .map_err(|e| BadLabel(format!("could not write {}: {e}", path.display())))?;
    harden_file(&path);
    let fp = fingerprint(token.trim());
    tracing::info!(sub, groups = ?groups, fingerprint = %fp, "client_tokens: wrote an API key");
    Ok(ApiKeyRow {
        sub: sub.to_string(),
        groups: groups.to_vec(),
        fingerprint: fp,
    })
}

/// Remove `sub`'s key file. `Ok(false)` when there was none — reported, never
/// counted as a revocation.
pub fn revoke_key(dir: &Path, sub: &str) -> std::io::Result<bool> {
    match std::fs::remove_file(key_path(dir, sub.trim())) {
        Ok(()) => {
            tracing::info!(sub, "client_tokens: removed an API key");
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// The keys on disk now, as rows.
pub fn list_keys(dir: &Path) -> Vec<ApiKeyRow> {
    read_dir_keys(dir)
        .into_iter()
        .map(|k| ApiKeyRow {
            fingerprint: fingerprint(&k.token),
            sub: k.sub,
            groups: k.groups,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::ClientTokenStore;
    use super::*;

    #[test]
    fn a_key_asserts_its_sub_and_groups_and_makes_the_store_keyed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("client-tokens");
        assert!(!ClientTokenStore::load(Some(dir.clone())).is_keyed());
        add_key(&dir, "it", &[KEY_ADMIN_GROUP.into()], "tok-it").unwrap();
        add_key(&dir, "alice", &[], "tok-alice").unwrap();
        let store = ClientTokenStore::load(Some(dir.clone()));
        assert!(store.is_keyed());
        assert_eq!(
            store.asserted_for("tok-it"),
            Some(("it".into(), vec![KEY_ADMIN_GROUP.to_string()]))
        );
        assert_eq!(
            store.asserted_for("tok-alice"),
            Some(("alice".into(), vec![]))
        );
        assert_eq!(store.asserted_for("tok-nobody"), None);
        // A key is not a named device token: it never admits as one.
        assert_eq!(store.label_for("tok-alice"), None);
        assert!(
            add_key(&dir, "alice", &[], "tok-2").is_err(),
            "no silent rotation"
        );
        assert!(add_key(&dir, "../x", &[], "tok-3").is_err());
        assert!(revoke_key(&dir, "alice").unwrap());
        assert!(!revoke_key(&dir, "alice").unwrap());
        assert_eq!(
            list_keys(&dir)
                .into_iter()
                .map(|r| r.sub)
                .collect::<Vec<_>>(),
            vec!["it".to_string()]
        );
    }

    /// The daemon's own credential is admitted, as an admin, only by a store
    /// the disk made keyed, and no operator listing shows it. Failing input:
    /// insert it unconditionally and the unkeyed store admits it (and reads
    /// keyed).
    #[test]
    fn the_self_credential_admits_only_on_a_keyed_store() {
        let own = super::super::self_credential().expect("entropy");
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("client-tokens");
        let unkeyed = ClientTokenStore::load(Some(dir.clone()));
        assert!(!unkeyed.is_keyed());
        assert_eq!(unkeyed.asserted_for(own), None);
        add_key(&dir, "alice", &[], "tok-alice").unwrap();
        let keyed = ClientTokenStore::load(Some(dir.clone()));
        assert_eq!(
            keyed.asserted_for(own),
            Some((
                super::super::SELF_SUB.to_string(),
                vec![KEY_ADMIN_GROUP.to_string()]
            ))
        );
        assert_eq!(
            list_keys(&dir)
                .into_iter()
                .map(|r| r.sub)
                .collect::<Vec<_>>(),
            vec!["alice".to_string()]
        );
        assert!(check_label(super::super::SELF_SUB).is_err());
    }
}
