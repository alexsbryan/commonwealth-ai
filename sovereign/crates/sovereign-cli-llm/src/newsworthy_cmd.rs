//! `sovereign newsworthy <subcommand>` — operator inspection for the
//! `wikipedia-newsworthy` freshness daemon.
//!
//! v0 ships one subcommand:
//!
//! - `status` — print the tracked-article summary by lifecycle plus
//!   the most recent leader, last portal date ingested, and per-self
//!   ownership count.
//!
//! The watcher writes its state to `MeshStore` under three app_ids
//! (`wikipedia-newsworthy:tracked`, `:portal`, `:job`). This command
//! reads the same store directly so the operator sees ground truth
//! without going via the running daemon's HTTP surface.
//!
//! **Caveat (v0).** `EmbeddedDaemon::start_daemon` currently
//! constructs an in-memory `MeshStore` (cf. sovereign-mesh's Cargo
//! comment). Disk-backed inspection only works against
//! `commonwealth-daemon`'s `~/.commonwealth/store.db`. When the
//! embedded daemon eventually persists MeshStore to disk, this
//! command's `--store-path` flag picks the right file automatically;
//! until then operators rely on `tracing::info!` events emitted by
//! the watcher (`newsworthy.tick`, `newsworthy.tracked_state_change`,
//! `newsworthy.portal_ingested`).

use std::collections::BTreeMap;
use std::path::PathBuf;

use commonwealth_state::MeshStore;
use corpus_engine::update::newsworthy_watcher::{
    Lifecycle, PortalMarker, TrackedArticle, APP_ID_PORTAL, APP_ID_TRACKED,
};

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
        "Usage: sovereign newsworthy <subcommand>\n\n\
         Subcommands:\n  \
           status [--store-path PATH]    Print tracked-set summary by lifecycle\n  \
           help                          This message\n\n\
         The default store path is `~/.commonwealth/store.db` (the \
         commonwealth-daemon location). Override with `--store-path` to \
         inspect a different MeshStore SQLite file."
    );
}

async fn run_status(args: &[String]) -> i32 {
    let store_path = parse_store_path(args).unwrap_or_else(default_store_path);
    if !store_path.exists() {
        eprintln!(
            "newsworthy: no MeshStore at {} — run a daemon first, or pass --store-path",
            store_path.display()
        );
        return 1;
    }
    let store = match MeshStore::open(&store_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "newsworthy: open {} failed: {e}",
                store_path.display()
            );
            return 1;
        }
    };

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
    let portal_entries = store.scan(APP_ID_PORTAL, "portal:").unwrap_or_default();
    let latest_portal = portal_entries
        .iter()
        .filter_map(|e| serde_json::from_slice::<PortalMarker>(&e.value).ok())
        .max_by_key(|m| m.fetched_at);

    println!("Wikipedia Newsworthy — operator status");
    println!("{}", "─".repeat(60));
    println!("  store: {}", store_path.display());
    println!("  tracked entries: {} (decode errors: {decode_errors})", total_known);
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
