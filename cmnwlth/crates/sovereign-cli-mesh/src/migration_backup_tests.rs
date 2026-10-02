// SPDX-License-Identifier: AGPL-3.0-or-later
//! pb-distribution-f10: every handover move keeps its first original, a later
//! run never overwrites it, and RUNBOOK §9's rollback restores a main-era data
//! dir file for file. The failing input for each: a second run that writes
//! its backup again, which loses the original the first run kept.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use commonwealth_transport::identity::NODE_KEY_FILE;

use crate::identity_handover::{self, Handover, HANDED_OVER_KEY};
use crate::iroh_config_migration::migrate_iroh_keys;
use crate::rail_migration::{migrate_media_to_rails, migrate_work_offer};

const RAILS_TOML: &str = "[media]\nallow = [\"Kept\"]\n";

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

#[test]
fn a_second_viewer_move_keeps_the_first_config_backup() {
    let svrn = tempfile::tempdir().unwrap();
    let rails = tempfile::tempdir().unwrap();
    let config = svrn.path().join("config.toml");
    let first = "[iroh]\nmedia_viewer_user = \"viewer-1\"\n";
    fs::write(&config, first).unwrap();
    migrate_media_to_rails(svrn.path(), &config, rails.path());
    fs::write(&config, "[iroh]\nmedia_viewer_user = \"viewer-2\"\n").unwrap();
    migrate_media_to_rails(svrn.path(), &config, rails.path());

    assert_eq!(read(&svrn.path().join("config.toml.bak")), first);
    assert!(!read(&config).contains("media_viewer_user"));
}

#[test]
fn a_second_work_offer_move_keeps_the_first_backups() {
    let svrn = tempfile::tempdir().unwrap();
    let rails = tempfile::tempdir().unwrap();
    let config = svrn.path().join("config.toml");
    fs::write(rails.path().join("rails.toml"), RAILS_TOML).unwrap();
    let first = "[compute.work_offer]\nmax_concurrent = 1\n";
    fs::write(&config, first).unwrap();
    migrate_work_offer(&config, rails.path());
    fs::write(&config, "[compute.work_offer]\nmax_concurrent = 2\n").unwrap();
    migrate_work_offer(&config, rails.path());

    assert_eq!(read(&svrn.path().join("config.toml.bak")), first);
    assert_eq!(
        read(&rails.path().join("rails.toml.pre-handover")),
        RAILS_TOML
    );
    assert!(!read(&config).contains("work_offer"));
}

#[test]
fn a_second_iroh_move_keeps_the_first_backups() {
    let svrn = tempfile::tempdir().unwrap();
    let rails = tempfile::tempdir().unwrap();
    let config = svrn.path().join("config.toml");
    let first = "[iroh]\nrelay_urls = [\"https://a:443\"]\n";
    fs::write(&config, first).unwrap();
    migrate_iroh_keys(&config, rails.path());
    let moved_rails = read(&rails.path().join("rails.toml"));
    fs::write(&config, "[iroh]\nrelay_urls = [\"https://b:443\"]\n").unwrap();
    migrate_iroh_keys(&config, rails.path());

    assert_eq!(read(&svrn.path().join("config.toml.iroh.bak")), first);
    assert_eq!(
        read(&rails.path().join("rails.toml.pre-handover")),
        "",
        "rails.toml did not exist: the empty aside says so, and stays"
    );
    assert_eq!(read(&rails.path().join("rails.toml")), moved_rails);
}

/// A main-era binary run after the handover mints the daemon a fresh key, and
/// the next `svrn mesh up` hands that one over too. cw-rails' own first key
/// and the first daemon key handed over both survive it.
#[test]
fn a_second_identity_handover_keeps_both_first_keys() {
    let svrn = tempfile::tempdir().unwrap();
    let rails = tempfile::tempdir().unwrap();
    let (key, ..) = identity_handover::tests::daemon_store(svrn.path());
    fs::write(rails.path().join(NODE_KEY_FILE), [9u8; 32]).unwrap();
    identity_handover::hand_over(svrn.path(), rails.path(), false).unwrap();
    fs::write(svrn.path().join(NODE_KEY_FILE), [5u8; 32]).unwrap();
    assert!(matches!(
        identity_handover::hand_over(svrn.path(), rails.path(), false).unwrap(),
        Handover::Moved { .. }
    ));

    assert_eq!(
        fs::read(rails.path().join("node_key.pre-handover")).unwrap(),
        vec![9u8; 32]
    );
    assert_eq!(fs::read(svrn.path().join(HANDED_OVER_KEY)).unwrap(), key);
    assert!(!svrn.path().join(NODE_KEY_FILE).exists());
}

/// Every file under `root`, by relative path.
fn files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).unwrap().display().to_string();
                out.insert(rel, fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// RUNBOOK §9 step 1, the script an operator runs.
const ROLLBACK: &str = include_str!("../rollback-handover.sh");

/// A main-era node — the daemon's key, meshes, a ring journal, the media
/// house credential and a config holding the viewer id, the work offer and
/// `[iroh]` keys, beside a cw-rails holding its own key and rails.toml — is
/// handed over twice, then the rollback script runs: both dirs are back, file
/// for file, as the main-era layout left them.
#[test]
fn the_rollback_script_restores_the_main_era_dir() {
    let svrn = tempfile::tempdir().unwrap();
    let rails = tempfile::tempdir().unwrap();
    identity_handover::tests::daemon_store(svrn.path());
    let ns = svrn.path().join("rings/house");
    fs::create_dir_all(&ns).unwrap();
    fs::write(ns.join("oplog.jsonl"), "{\"seq\":0}").unwrap();
    commonwealth_media::write_declared_in(
        &commonwealth_media::house_dir_under(svrn.path()),
        "authorization",
        "house-key",
    )
    .unwrap();
    let config = svrn.path().join("config.toml");
    fs::write(
        &config,
        "# mine\n[iroh]\nrelay_urls = [\"https://relay.corp:443\"]\n\
         media_viewer_user = \"viewer-1\"\n\n[compute.work_offer]\nmax_concurrent = 2\n",
    )
    .unwrap();
    fs::write(rails.path().join(NODE_KEY_FILE), [9u8; 32]).unwrap();
    fs::write(rails.path().join("rails.toml"), RAILS_TOML).unwrap();
    let main_era = (files(svrn.path()), files(rails.path()));

    std::env::set_var("CW_RAILS_DIR", rails.path());
    for _ in 0..2 {
        identity_handover::hand_over(svrn.path(), rails.path(), false).unwrap();
        crate::rail_migration::hand_over(svrn.path(), &config, false);
    }
    assert_ne!(files(rails.path()), main_era.1, "the handover moved files");

    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(ROLLBACK)
        .env("SVRN", svrn.path())
        .env("CONFIG", &config)
        .env("RAILS", rails.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(files(svrn.path()), main_era.0);
    assert_eq!(files(rails.path()), main_era.1);
}
