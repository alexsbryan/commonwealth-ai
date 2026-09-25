// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn newsworthy <subcommand>` — operator inspection for the
//! `wikipedia-newsworthy` freshness daemon.
//!
//! v0 ships one subcommand:
//!
//! - `status` — print the tracked-article summary by lifecycle plus
//!   the most recent leader, last portal date ingested, and per-self
//!   ownership count.
//!
//! The watcher writes its state to the mesh store under three app_ids
//! (`wikipedia-newsworthy-tracked`, `-portal`, `-status` — colon-free because
//! a ring namespace is a directory name). This command reads cw-rails' mesh
//! store through `/v1/mesh/kv/*` (five-programs fp-87), after migrating the
//! legacy SQLite file (`--store-path`, default
//! `commonwealth-daemon`'s `~/.commonwealth/store.db`) into it once.

use std::collections::BTreeMap;
use std::path::PathBuf;

use corpus_engine::update::newsworthy_watcher::{
    Lifecycle, PortalMarker, TrackedArticle, APP_ID_PORTAL, APP_ID_STATUS, APP_ID_TRACKED,
};
use sovereign_contracts::peer::ReplicatedKv;

/// Each legacy app_id the migration reads and the one it is written under:
/// the current spellings, and fp-106's renames of the two colon forms.
const MIGRATED_APP_IDS: &[(&str, &str)] = &[
    (APP_ID_TRACKED, APP_ID_TRACKED),
    (APP_ID_PORTAL, APP_ID_PORTAL),
    (APP_ID_STATUS, APP_ID_STATUS),
    ("wikipedia-newsworthy:portal", APP_ID_PORTAL),
    ("wikipedia-newsworthy:status", APP_ID_STATUS),
];

pub async fn run(args: &[String]) -> i32 {
    let subcommand = args.first().map(String::as_str).unwrap_or("");
    match subcommand {
        "status" | "" => run_status(args.get(1..).unwrap_or(&[])).await,
        "help" | "--help" | "-h" => {
            print_help();
            0
        }
        other => {
            eprintln!("Unknown newsworthy subcommand: {other}");
            print_help();
            2
        }
    }
}

fn print_help() {
    println!(
        "Usage: svrn newsworthy <subcommand>\n\n\
         Subcommands:\n  \
           status [--store-path PATH]    Print tracked-set summary by lifecycle\n  \
           help                          This message\n\n\
         Reads cw-rails' mesh store. A legacy MeshStore SQLite file is \
         migrated into it once first: `~/.commonwealth/store.db` (the \
         commonwealth-daemon location), or the file `--store-path` names."
    );
}

async fn run_status(args: &[String]) -> i32 {
    let store_path = parse_store_path(args).unwrap_or_else(default_store_path);
    let store = crate::legacy_store::rails_kv();
    if let Err(e) = crate::legacy_store::migrate_if_needed(
        &store_path,
        MIGRATED_APP_IDS,
        &store,
        crate::legacy_store::export_via_cli_mesh,
    ) {
        eprintln!("newsworthy: {e}");
        return 1;
    }

    // ── Tracked articles by lifecycle ─────────────────────────────
    let tracked_entries = match store.scan(APP_ID_TRACKED, "tracked:") {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("newsworthy: scan tracked: {e}");
            return 1;
        }
    };
    let mut by_lifecycle: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut total_known = 0usize;
    let mut decode_errors = 0usize;
    for entry in &tracked_entries {
        match serde_json::from_slice::<TrackedArticle>(&entry.value) {
            Ok(article) => {
                total_known += 1;
                let key = lifecycle_key(article.lifecycle);
                *by_lifecycle.entry(key).or_default() += 1;
            }
            Err(_) => decode_errors += 1,
        }
    }

    // ── Most recent portal ingest ─────────────────────────────────
    let portal_entries = match store.scan(APP_ID_PORTAL, "portal:") {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("newsworthy: scan portal: {e}");
            return 1;
        }
    };
    let latest_portal = portal_entries
        .iter()
        .filter_map(|e| serde_json::from_slice::<PortalMarker>(&e.value).ok())
        .max_by_key(|m| m.fetched_at);

    println!("Wikipedia Newsworthy — operator status");
    println!("{}", "─".repeat(60));
    println!("  store: cw-rails' mesh store");
    println!(
        "  tracked entries: {} (decode errors: {decode_errors})",
        total_known
    );
    for (label, count) in &by_lifecycle {
        println!("    {label:>14}: {count}");
    }
    match latest_portal {
        Some(m) => println!(
            "  last portal:  {} (revid {}) fetched at {}",
            m.date_iso,
            m.last_fetched_revid,
            format_unix(m.fetched_at),
        ),
        None => println!("  last portal:  (none yet)"),
    }
    0
}

fn parse_store_path(args: &[String]) -> Option<PathBuf> {
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        if a == "--store-path" {
            if let Some(p) = iter.next() {
                return Some(PathBuf::from(p));
            }
        } else if let Some(rest) = a.strip_prefix("--store-path=") {
            return Some(PathBuf::from(rest));
        }
    }
    None
}

fn default_store_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".commonwealth").join("store.db")
}

fn lifecycle_key(l: Lifecycle) -> &'static str {
    match l {
        Lifecycle::PendingFetch => "PendingFetch",
        Lifecycle::Present => "Present",
        Lifecycle::Refreshing => "Refreshing",
        Lifecycle::Stale => "Stale",
        Lifecycle::Failed => "Failed",
    }
}

fn format_unix(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| ts.to_string())
}
