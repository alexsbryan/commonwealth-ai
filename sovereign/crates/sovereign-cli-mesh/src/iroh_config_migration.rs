// SPDX-License-Identifier: AGPL-3.0-or-later
//! svrn's `[iroh]` keys, handed to their owner (pb-mesh-exit-transport).
//!
//! The daemon bound the node's iroh endpoint until the flip and read its
//! relays and media origin from `[iroh]` in svrn's config.toml; cw-rails binds
//! it now and reads them from its own `rails.toml` (commonwealth-rails
//! config.rs `[relay]`, `[media]`). Run by `rail_migration::hand_over` on the
//! same terms as `migrate_work_offer`: only when no cw-rails answers, a key
//! rails.toml already holds is kept (the operator's newer word), the original
//! config is kept beside as `config.toml.iroh.bak`, and a write that fails
//! leaves svrn's keys where they are. Keys no program reads after the flip are
//! named and left alone, never silently dropped.

use std::path::Path;

/// `[iroh]` key → (`rails.toml` table, key) for every key cw-rails owns now.
const IROH_TO_RAILS: [(&str, &str, &str); 4] = [
    ("relay_urls", "relay", "urls"),
    ("discovery", "relay", "discovery"),
    ("media_origin", "media", "origin"),
    ("media_allow", "media", "allow"),
];

/// `[iroh]` keys the daemon alone read, whose readers the flip deleted.
/// `apps`, `app_allow`, `offer_origin` and `offer_allow` stay where they are:
/// svrn reads them and registers them with cw-rails
/// (`sovereign_daemon::published_origins`, phase-b-81 (3)).
const IROH_RETIRED: [&str; 2] = ["enabled", "transport"];

const TARGET: &str = "rail_migration";

/// Move the `[iroh]` keys cw-rails owns into `rails_dir`'s rails.toml.
pub fn migrate_iroh_keys(config_path: &Path, rails_dir: &Path) {
    let Ok(text) = std::fs::read_to_string(config_path) else {
        tracing::debug!(target: TARGET, config = %config_path.display(), "iroh migration: no config to read");
        return;
    };
    let mut doc = match text.parse::<toml_edit::DocumentMut>() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(target: TARGET, error = %e, config = %config_path.display(), "iroh migration: config.toml did not parse; `[iroh]` is left alone");
            return;
        }
    };
    let Some(iroh) = doc.get("iroh").and_then(|i| i.as_table_like()) else {
        tracing::debug!(target: TARGET, config = %config_path.display(), "iroh migration: no `[iroh]`");
        return;
    };
    let retired: Vec<&str> = IROH_RETIRED
        .into_iter()
        .filter(|k| iroh.contains_key(k))
        .collect();
    if !retired.is_empty() {
        tracing::warn!(target: TARGET, keys = ?retired, config = %config_path.display(),
                       "iroh migration: these `[iroh]` keys have no reader since cw-rails is the mesh endpoint; left in place");
    }
    let moving: Vec<(&str, &str, &str, toml_edit::Item)> = IROH_TO_RAILS
        .into_iter()
        .filter_map(|(k, t, rk)| iroh.get(k).map(|v| (k, t, rk, v.clone())))
        .collect();
    if moving.is_empty() {
        tracing::debug!(target: TARGET, config = %config_path.display(), "iroh migration: no `[iroh]` key cw-rails owns");
        return;
    }

    let rails_toml = rails_dir.join(commonwealth_media::RAILS_CONFIG_FILE);
    let rails_text = match std::fs::read_to_string(&rails_toml) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            tracing::warn!(target: TARGET, error = %e, to = %rails_toml.display(), "iroh migration: rails.toml could not be read; `[iroh]` stays in svrn's config");
            return;
        }
    };
    let mut rails = match rails_text.parse::<toml_edit::DocumentMut>() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(target: TARGET, error = %e, to = %rails_toml.display(), "iroh migration: rails.toml did not parse; `[iroh]` stays in svrn's config");
            return;
        }
    };
    for (key, table, rails_key, value) in &moving {
        let section = rails.entry(table).or_insert(toml_edit::table());
        let Some(section) = section.as_table_like_mut() else {
            tracing::warn!(target: TARGET, table, to = %rails_toml.display(), "iroh migration: rails.toml's entry is not a table; `[iroh]` stays in svrn's config");
            return;
        };
        if section.contains_key(rails_key) {
            tracing::warn!(target: TARGET, key, to = %format!("[{table}] {rails_key}"),
                           "iroh migration: rails.toml already holds it — kept; svrn's copy is removed");
        } else {
            section.insert(rails_key, value.clone());
        }
    }
    if let Err(e) = std::fs::create_dir_all(rails_dir)
        .and_then(|()| std::fs::write(&rails_toml, rails.to_string()))
    {
        tracing::error!(target: TARGET, error = %e, to = %rails_toml.display(), "iroh migration: rails.toml could not be written; `[iroh]` stays in svrn's config");
        return;
    }
    let backup = config_path.with_extension("toml.iroh.bak");
    if let Err(e) = std::fs::write(&backup, &text) {
        tracing::warn!(target: TARGET, error = %e, backup = %backup.display(), "iroh migration: the backup could not be written, so `[iroh]` stays in svrn's config (cw-rails reads its own copy)");
        return;
    }
    if let Some(iroh) = doc.get_mut("iroh").and_then(|i| i.as_table_like_mut()) {
        for (key, ..) in &moving {
            iroh.remove(key);
        }
    }
    if let Err(e) = std::fs::write(config_path, doc.to_string()) {
        tracing::warn!(target: TARGET, error = %e, config = %config_path.display(), "iroh migration: the keys are in rails.toml, but they could not be removed from svrn's config");
        return;
    }
    tracing::info!(target: TARGET, keys = ?moving.iter().map(|m| m.0).collect::<Vec<_>>(),
                   from = %config_path.display(), to = %rails_toml.display(), backup = %backup.display(),
                   "iroh migration: `[iroh]` keys moved to rails.toml");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Relays and the media origin move to rails.toml under cw-rails' names;
    /// a key rails.toml already holds is kept; a retired key stays in svrn's
    /// config; the original is kept as a backup. Failing input: skip the
    /// migration, and cw-rails binds with n0's relays and serves no media.
    #[test]
    fn iroh_keys_move_to_their_owner_and_nothing_is_lost() {
        let svrn = tempfile::tempdir().unwrap();
        let rails = tempfile::tempdir().unwrap();
        let config = svrn.path().join("config.toml");
        std::fs::write(
            &config,
            "[iroh]\nenabled = true\nrelay_urls = [\"https://relay.corp:443\"]\n\
             media_origin = \"127.0.0.1:8096\"\nmedia_allow = [\"LittleMac\"]\n",
        )
        .unwrap();
        std::fs::write(
            rails.path().join(commonwealth_media::RAILS_CONFIG_FILE),
            "[media]\nallow = [\"Kept\"]\n",
        )
        .unwrap();

        migrate_iroh_keys(&config, rails.path());

        let rails_doc: toml::Value = toml::from_str(
            &std::fs::read_to_string(rails.path().join(commonwealth_media::RAILS_CONFIG_FILE))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            rails_doc["relay"]["urls"][0].as_str(),
            Some("https://relay.corp:443")
        );
        assert_eq!(
            rails_doc["media"]["origin"].as_str(),
            Some("127.0.0.1:8096")
        );
        assert_eq!(
            rails_doc["media"]["allow"][0].as_str(),
            Some("Kept"),
            "rails' own word kept"
        );
        let svrn_doc: toml::Value =
            toml::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(
            svrn_doc["iroh"]["enabled"].as_bool(),
            Some(true),
            "a retired key stays"
        );
        assert!(svrn_doc["iroh"].get("relay_urls").is_none());
        assert!(svrn_doc["iroh"].get("media_origin").is_none());
        assert!(config.with_extension("toml.iroh.bak").exists());
    }
}
