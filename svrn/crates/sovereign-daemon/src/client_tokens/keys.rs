// SPDX-License-Identifier: AGPL-3.0-or-later
//! Named credentials — the on-disk half (`<data_dir>/client-tokens/<name>.key`).
//!
//! A record is the secret on its first line and, optionally, the groups it
//! asserts on a `groups = a, b` line:
//!
//! ```text
//! svrn_3f9c…e1
//! groups = admin
//! ```
//!
//! The file name is the credential's name, under the label rule
//! [`super::check_label`] enforces, because it is a file name and nothing
//! else. A legacy device token (`<label>.token`, written by `svrn mesh token`
//! before 2026-10-08) is the same record with no groups line, read as one and
//! rewritten as `<label>.key` once the posture is declared
//! ([`migrate_tokens`]). This module reads, writes and removes the files;
//! [`super::ClientTokenStore`] answers who a presented bearer is.

use std::path::{Path, PathBuf};

use super::{check_label, harden_dir, harden_file, Named};

/// The group whose credentials reach ingest, the admin routes and minting.
/// Every other credential reaches what its posture gives a named client: on
/// `loopback = "none"`, its own conversations and the read routes
/// `crate::api_keys` names.
pub const KEY_ADMIN_GROUP: &str = "admin";

const KEY_EXT: &str = "key";
const LEGACY_TOKEN_EXT: &str = "token";

fn record_path(dir: &Path, name: &str, ext: &str) -> PathBuf {
    dir.join(format!("{name}.{ext}"))
}

/// Parse a record's text. `None` when it holds no secret.
fn parse_record(name: &str, raw: &str) -> Option<Named> {
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
                name,
                "client_tokens: record line is not `groups = …` — ignored"
            );
        }
    }
    Some(Named {
        name: name.to_string(),
        token,
        groups,
    })
}

/// The records' file names under `dir`: `(name, extension, path)` for every
/// `<name>.key` and `<name>.token` whose name is a label. A missing directory
/// is no records.
fn record_files(dir: &Path) -> Vec<(String, &'static str, PathBuf)> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!(dir = ?dir, "client_tokens: credential directory unreadable: {e}");
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let (name, ext) = match file.rsplit_once('.') {
            Some((name, KEY_EXT)) => (name, KEY_EXT),
            Some((name, LEGACY_TOKEN_EXT)) => (name, LEGACY_TOKEN_EXT),
            _ => continue,
        };
        if check_label(name).is_err() {
            tracing::warn!(
                ?path,
                "client_tokens: skipping a file whose name is not a credential name"
            );
            continue;
        }
        out.push((name.to_string(), ext, path));
    }
    out.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
    out
}

/// Whether `dir` holds a `<name>.key` file: the evidence an undeclared
/// posture is inferred from ([`super::LoopbackPosture::resolve`]).
pub(super) fn holds_a_key_file(dir: &Path) -> bool {
    record_files(dir).iter().any(|(_, ext, _)| *ext == KEY_EXT)
}

/// Every record under `dir`, both forms. When one name has both a `.key` and
/// a legacy `.token`, the `.key` is the record and the `.token` is named as
/// left unread: one name, one record (ARCH 8). An unreadable or empty file is
/// named and skipped.
pub(super) fn read_records(dir: &Path) -> Vec<Named> {
    let files = record_files(dir);
    let mut out: Vec<Named> = Vec::new();
    for (name, ext, path) in &files {
        if *ext == LEGACY_TOKEN_EXT && files.iter().any(|(n, e, _)| n == name && *e == KEY_EXT) {
            tracing::warn!(
                ?path,
                "client_tokens: a legacy token shares its name with a key — the key is the \
                 record, and this token is NOT admitted; revoke one of them by name"
            );
            continue;
        }
        match std::fs::read_to_string(path).map(|raw| parse_record(name, &raw)) {
            Ok(Some(record)) => out.push(record),
            Ok(None) => tracing::warn!(?path, "client_tokens: credential file is empty — skipping"),
            Err(e) => tracing::warn!(?path, "client_tokens: credential file unreadable: {e}"),
        }
    }
    out
}

/// Whether a record named `name` is on disk, in either form.
pub(super) fn record_exists(dir: &Path, name: &str) -> bool {
    record_path(dir, name, KEY_EXT).exists() || record_path(dir, name, LEGACY_TOKEN_EXT).exists()
}

/// Write `record` as `<name>.key`, mode 0600 in a 0700 directory, by
/// temp-and-rename so a crash never leaves a half-written secret. THE one
/// writer of a record file: [`super::ClientTokenStore::mint`] and
/// [`migrate_tokens`] both come through it.
pub(super) fn write_record(dir: &Path, record: &Named) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    harden_dir(dir);
    let mut body = format!("{}\n", record.token.trim());
    if !record.groups.is_empty() {
        body.push_str(&format!("groups = {}\n", record.groups.join(", ")));
    }
    let target = record_path(dir, &record.name, KEY_EXT);
    let tmp = dir.join(format!(".{}.key.tmp", record.name));
    std::fs::write(&tmp, body)?;
    harden_file(&tmp);
    std::fs::rename(&tmp, &target)
}

/// Remove `name`'s record in both forms. `Ok(false)` when there was none —
/// reported, never counted as a revocation.
pub(super) fn remove_record(dir: &Path, name: &str) -> std::io::Result<bool> {
    let mut removed = false;
    for ext in [KEY_EXT, LEGACY_TOKEN_EXT] {
        match std::fs::remove_file(record_path(dir, name, ext)) {
            Ok(()) => removed = true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(removed)
}

/// Rewrite every legacy `<label>.token` as `<label>.key` (no groups), then
/// remove the `.token`. Run only under a declared posture, where adding a
/// `.key` file cannot change what an undeclared daemon infers. A label that
/// already has a `.key` is left alone and named by [`read_records`].
pub(super) fn migrate_tokens(dir: &Path) {
    for (name, ext, path) in record_files(dir) {
        if ext != LEGACY_TOKEN_EXT || record_path(dir, &name, KEY_EXT).exists() {
            continue;
        }
        let record = match std::fs::read_to_string(&path).map(|raw| parse_record(&name, &raw)) {
            Ok(Some(r)) => r,
            Ok(None) | Err(_) => continue, // `read_records` names it.
        };
        match write_record(dir, &record).and_then(|()| std::fs::remove_file(&path)) {
            Ok(()) => tracing::info!(
                name = %name,
                "client_tokens: migrated a legacy device token into the one record form"
            ),
            Err(e) => tracing::warn!(
                name = %name,
                "client_tokens: could not migrate a legacy device token (it is still read): {e}"
            ),
        }
    }
}
