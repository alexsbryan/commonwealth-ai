// SPDX-License-Identifier: AGPL-3.0-or-later
//! `[media]` in cw-rails' rails.toml and the credentials declared for it: the
//! one place the media verbs read and write them, and the reload that has a
//! running cw-rails serve what they now say (commonwealth-rails
//! `origins::stand_media`). Until 2026-10-04 the verbs wrote svrn's `[iroh]`
//! and svrn's root and reloaded svrn, which has read neither since
//! pb-mesh-exit-transport, so an offer reached nobody (ring-room film leg).

use std::path::{Path, PathBuf};
use std::time::Duration;

use toml_edit::{DocumentMut, Item, Table};

/// The file cw-rails loads from its data dir; absent, it runs on defaults.
pub(super) fn rails_toml() -> PathBuf {
    commonwealth_media::rails_data_dir().join(commonwealth_media::RAILS_CONFIG_FILE)
}

/// Where cw-rails reads the credentials it adds to a member's media request.
pub(super) fn declared_dir() -> PathBuf {
    commonwealth_media::dir_under(&commonwealth_media::rails_data_dir())
}

/// rails.toml as an editable document; an absent file is an empty one.
pub(super) fn load() -> Result<(PathBuf, DocumentMut), String> {
    let path = rails_toml();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let doc = text
        .parse::<DocumentMut>()
        .map_err(|e| format!("parse {}: {e}", path.display()))?;
    Ok((path, doc))
}

/// Write `doc`, then load it with cw-rails' own reader. A file cw-rails would
/// refuse is put back as it was, so an edit never stops it from starting.
pub(super) fn write(path: &Path, doc: &DocumentMut) -> Result<(), String> {
    // Absent is a file to remove on refusal; unreadable is a refusal now,
    // or a refused edit would delete a file nobody could read back.
    let original = match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    std::fs::write(path, doc.to_string()).map_err(|e| format!("write {}: {e}", path.display()))?;
    if let Err(e) = commonwealth_rails::config::Config::load(dir, Some(path)) {
        let restored = match original {
            Some(text) => std::fs::write(path, text),
            None => std::fs::remove_file(path),
        };
        tracing::warn!(error = %e, restored = restored.is_ok(), path = %path.display(), "media: edit refused by cw-rails' reader");
        return Err(format!(
            "the edit produced a rails.toml cw-rails would refuse ({e}); it is unchanged"
        ));
    }
    Ok(())
}

/// `[media]`, created when absent.
pub(super) fn media_table(doc: &mut DocumentMut) -> Result<&mut Table, String> {
    doc.entry("media")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_mut()
        .ok_or_else(|| "`media` in rails.toml is not a table".to_string())
}

/// Ask this node's cw-rails to serve what rails.toml and the declared
/// credentials now say (`POST /v1/mesh/media/reload`). One that does not
/// answer, or refuses, is the verb's failure: the files are written and
/// serve from its next start, which is not "offered now".
pub(super) async fn reload(verb: &str) -> i32 {
    let base = match crate::mesh_cmd::rails_base() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("mesh media {verb}: {e}");
            return 1;
        }
    };
    let url = format!("{base}/v1/mesh/media/reload");
    let sent = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c.post(&url).send().await,
        Err(e) => {
            eprintln!("mesh media {verb}: {e}");
            return 1;
        }
    };
    match sent {
        Ok(r) if r.status().is_success() => {
            tracing::info!(%url, "media: cw-rails serves the media config as written");
            0
        }
        Ok(r) => {
            let status = r.status();
            let body = r
                .text()
                .await
                .unwrap_or_else(|e| format!("<the body could not be read: {e}>"));
            tracing::warn!(%url, %status, %body, "media: cw-rails refused the reload");
            eprintln!("mesh media {verb}: cw-rails refused the reload ({status}): {body}");
            1
        }
        Err(e) => {
            tracing::warn!(%url, error = %e, "media: cw-rails did not answer the reload");
            eprintln!(
                "mesh media {verb}: cw-rails is not answering at {base} ({e}). What was \
                 written serves from its next start."
            );
            1
        }
    }
}
