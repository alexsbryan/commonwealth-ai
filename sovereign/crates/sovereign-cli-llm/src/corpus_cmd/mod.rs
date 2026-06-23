// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign corpus` subcommand handlers — extracted from `mesh_cmd`
//! (§3.2). Corpus index management: list / install / remove / status /
//! stream-axes / diagnostics / dedupe / repair / pull / partition tooling
//! + parcel export. Dispatched as the `corpus` verb — which previously
//! lived in `mesh_cmd.rs` purely as a naming lie (the file served both
//! `mesh` and `corpus`).

// §3.2 sub-breakdown: the corpus surface is grouped into focused
// submodules. `fmt` is the shared-formatter leaf; `inventory` +
// `partitions` use it; `diagnostics` borrows the partition-discovery
// helpers. `run_corpus` below is the dispatcher.
mod diagnostics;
mod fmt;
mod ingest;
mod inventory;
mod partitions;
mod search;

use diagnostics::{
    cmd_corpus_dedupe, cmd_corpus_diag, cmd_corpus_export_parcels, cmd_corpus_repair,
    cmd_corpus_stream_axes,
};
use inventory::{cmd_corpus_install, cmd_corpus_list, cmd_corpus_remove, cmd_corpus_status};
use partitions::{
    cmd_corpus_merge_partitions, cmd_corpus_migrate_to_partition, cmd_corpus_pull,
    cmd_corpus_reconstruct_manifest,
};

pub async fn run_corpus(args: &[String]) -> i32 {
    if args.is_empty() {
        sovereign_cli_shared::help::print(&HELP_CORPUS);
        return 1;
    }
    if matches!(args[0].as_str(), "--help" | "-h" | "help") {
        sovereign_cli_shared::help::print(&HELP_CORPUS);
        return 0;
    }

    match args[0].as_str() {
        "list" => cmd_corpus_list().await,
        "ingest" => ingest::cmd_corpus_ingest(&args[1..]).await,
        "search" => search::cmd_corpus_search(&args[1..]).await,
        "install" => cmd_corpus_install(&args[1..]).await,
        "remove" => cmd_corpus_remove(&args[1..]).await,
        "status" => cmd_corpus_status().await,
        "diag" => cmd_corpus_diag(&args[1..]).await,
        "dedupe" => cmd_corpus_dedupe(&args[1..]).await,
        "repair" => cmd_corpus_repair(&args[1..]).await,
        "merge-partitions" => cmd_corpus_merge_partitions(&args[1..]).await,
        "pull" => cmd_corpus_pull(&args[1..]).await,
        "reconstruct-manifest" => cmd_corpus_reconstruct_manifest(&args[1..]).await,
        "migrate-to-partition" => cmd_corpus_migrate_to_partition(&args[1..]).await,
        "catalog" => crate::corpus_catalog_cmd::run_catalog(&args[1..]).await,
        "extract-entities" => {
            crate::corpus_extract_entities_cmd::run_extract_entities(&args[1..]).await
        }
        "scrub" => crate::corpus_scrub_cmd::run_scrub(&args[1..]).await,
        "snapshot" => crate::corpus_snapshot_cmd::run_snapshot(&args[1..]).await,
        // Watched-folder lifecycle subcommands. Implemented in
        // `corpus_watch_cmd` and proxied through the daemon's
        // `/internal/corpus/watch/*` HTTP routes.
        "watch" => crate::corpus_watch_cmd::run_register(&args[1..]).await,
        "watch-list" => crate::corpus_watch_cmd::run_list(&args[1..]).await,
        "watch-status" => crate::corpus_watch_cmd::run_status(&args[1..]).await,
        "watch-pause" => crate::corpus_watch_cmd::run_pause(&args[1..]).await,
        "watch-resume" => crate::corpus_watch_cmd::run_resume(&args[1..]).await,
        "watch-confirm-deletion" => crate::corpus_watch_cmd::run_confirm_deletion(&args[1..]).await,
        "watch-sync-now" => crate::corpus_watch_cmd::run_sync_now(&args[1..]).await,
        "watch-add-root" => crate::corpus_watch_cmd::run_add_root(&args[1..]).await,
        "watch-remove-root" => crate::corpus_watch_cmd::run_remove_root(&args[1..]).await,
        "watch-remove" => crate::corpus_watch_cmd::run_remove(&args[1..]).await,
        "stream-axes" => cmd_corpus_stream_axes(&args[1..]).await,
        "export-parcels" => cmd_corpus_export_parcels(&args[1..]).await,
        other => {
            eprintln!("Unknown corpus subcommand: {other}");
            sovereign_cli_shared::help::print(&HELP_CORPUS);
            1
        }
    }
}


const HELP_CORPUS: sovereign_cli_shared::help::Help = sovereign_cli_shared::help::Help {
    command: "sovereign corpus",
    summary: "Manage knowledge corpora shared across the mesh (install / remove / inspect).",
    sections: &[
        sovereign_cli_shared::help::HelpSection::Usage("sovereign corpus <subcommand> [args]"),
        sovereign_cli_shared::help::HelpSection::Subcommands(&[
            ("list",                      "List installed and available corpora"),
            ("ingest <folder>",           "Build a corpus from a folder via the workflow runner (chunk→embed→store; --corpus <id>, --glob)"),
            ("search <id> <query>",       "Search a corpus (embeds the query via the daemon; --limit N)"),
            ("install <id>",              "Install a corpus (e.g. 'wikipedia')"),
            ("remove <id>",               "Remove canonical + partitions (or --canonical-only / --partitions-only)"),
            ("status",                    "Show shard status for all corpora"),
            ("diag <id>",                 "Audit an installed corpus: distinct-article count vs. recipe filter"),
            ("dedupe <id>",               "One-shot rescue: collapse duplicate-content rows from a resume-rewind ingest"),
            ("repair <id>",               "Reset a 'completed' partition with missing shards back to in-progress so resume picks it up"),
            ("merge-partitions <id>",     "Merge all <id>-partition-*/ dirs into canonical <id>/ (one-shot rescue when peer-merge handoff was lost)"),
            ("pull <id>",                 "Stream a peer's canonical index over the mesh (use when local is missing or smaller than peer's)"),
            ("reconstruct-manifest <id>", "Rebuild source-file manifest (required before collaborative ingestion)"),
            ("migrate-to-partition <id>", "Rename a legacy canonical index into a partition-of-self so collaborative ingest can resume it"),
            ("scrub",                     "Entity-candidate extraction + bench TOML sanitisation for local-only corpora"),
            ("snapshot <subcmd>",         "Publish or inspect prebuilt-index tarballs for cold-start onboarding"),
            ("watch <path>",              "Register a folder the daemon keeps in sync (adds/edits/deletes flow through every ~2 minutes)"),
            ("watch-list",                "List every registered watched-folder corpus"),
            ("watch-status <id>",         "Show the most recent reconciliation status for one watched corpus"),
            ("watch-pause <id>",          "Pause sweeps for a watched folder until `watch-resume`"),
            ("watch-resume <id>",         "Resume sweeps after a manual pause"),
            ("watch-confirm-deletion <id>", "Acknowledge a guard-tripped pause so the next sweep applies the pending deletes"),
            ("watch-sync-now <id>",       "Trigger a sweep on a Manual-mode watched folder (no-op for Continuous corpora)"),
            ("watch-add-root <id> <path>", "Layer an additional folder onto an existing watched corpus"),
            ("watch-remove-root <id> <idx>", "Detach an additional folder by 0-based index"),
            ("watch-remove <id>",         "Unregister a watched folder and remove its index (source folder untouched)"),
            ("export-parcels <id>",       "Export a corpus's deterministic parcel atoms to CSV (--corpus, --out) for independent verification in a spreadsheet"),
        ]),
        sovereign_cli_shared::help::HelpSection::Notes(
            "`reconstruct-manifest` accepts --source-dir <path> (default:\n\
             ~/.sovereign/indexes/_downloads/<id>) and --yes (skip confirmation).\n\
             `migrate-to-partition` accepts --dry-run to preview without touching disk.",
        ),
    ],
};
