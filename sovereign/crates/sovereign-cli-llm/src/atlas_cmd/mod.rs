// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign atlas ...` — Atlas-style structural enrichment that
//! lives outside the existing 8-phase LLM-extraction pipeline.
//!
//! The `enrich` command runs the literary/philosophy atlas pipeline:
//! eight LLM-driven phases over chunk text. That trait is bound to
//! its phase shape and doesn't fit a structure-first pipeline that
//! deserialises pre-computed metadata into a graph with zero LLM
//! cycles. Rather than bend the trait, we register a sibling
//! command tree.
//!
//! Today `atlas` hosts one concrete pipeline:
//!
//! - `atlas wikipedia` — Layer 0 (link graph) of the Wikipedia
//!   atlas plan. Builds a SQLite-backed adjacency store from the
//!   `outgoing_links` / `section_path` / `pov_count` metadata
//!   that Wikipedia extractors already emit.
//!
//! Layer 1 (HDBSCAN clusters + bridge detection) and Layer 2
//! (targeted LLM enrichment on contested/bridge articles) will land
//! as additional sub-commands once Layer 0 has demonstrated bench
//! gains.

pub mod budget;
pub mod build_archive;
pub mod build_doc_index;
pub mod enable_incremental;
pub mod inspect;
pub mod migrate_ids;
pub mod stats;
pub mod status;
pub mod typed_extension;
pub mod wikipedia;

use sovereign_cli_shared::help::{self, Help, HelpSection};

const HELP: Help = Help {
    command: "sovereign atlas",
    summary: "Atlas-style structural enrichment of a corpus (Wikipedia today).",
    sections: &[
        HelpSection::Usage("sovereign atlas <subcommand> [args]"),
        HelpSection::Subcommands(&[
            (
                "wikipedia",
                "Layer 0: build the link graph from Wikipedia extractor metadata.",
            ),
            (
                "budget",
                "Show or set the per-corpus Tier-2 enrichment budget (top-N articles).",
            ),
            (
                "status",
                "Per-corpus atlas readiness — atom counts, Tier-2 progress, token spend.",
            ),
            (
                "list-corpora",
                "List installed corpora that have an atlas, with per-type atom counts.",
            ),
            (
                "list-atoms",
                "Browse atoms in one corpus, filterable by type and name substring.",
            ),
            (
                "show-atom",
                "Full inspector record for one atom — type body, evidence, related, cross-corpus.",
            ),
            (
                "migrate-ids",
                "Move 6 P0: rewrite sequential atom ids to content-hash. Idempotent.",
            ),
            (
                "build-doc-index",
                "Move 6 P1: derive doc_to_atoms.json sidecar from atoms.json.",
            ),
            (
                "build-archive",
                "Build the zero-copy atoms.rkyv archive off the query thread (Phase 1.5).",
            ),
            (
                "enable-incremental",
                "Move 6 P5: flip per-corpus atlas_incremental_enabled flag (pre-flight checks content-hash).",
            ),
            (
                "typed-extension",
                "Run the tiered typed-extension LLM pass over RAPTOR leaves + vault themes; writes atoms.json + atoms.meta.json.",
            ),
        ]),
        HelpSection::Notes(
            "Atlas commands operate against an already-installed corpus index. Install \
             the corpus first via `sovereign corpus install <id>` (or `sovereign \
             recipe run`). The graph DB lives alongside the LanceDB table at \
             `<data-dir>/indexes/<corpus-id>/wikipedia_graph.db` by default.",
        ),
    ],
};

pub async fn run_atlas(args: &[String]) -> i32 {
    if args.is_empty() {
        help::print(&HELP);
        return 2;
    }
    let first = args[0].as_str();
    if first == "--help" || first == "-h" || first == "help" {
        help::print(&HELP);
        return 0;
    }
    match first {
        "wikipedia" => wikipedia::run(&args[1..]).await,
        "budget" => budget::run(&args[1..]).await,
        "status" => status::run(&args[1..]).await,
        "list-corpora" => inspect::run_list_corpora(&args[1..]).await,
        "list-atoms" => inspect::run_list_atoms(&args[1..]).await,
        "show-atom" => inspect::run_show_atom(&args[1..]).await,
        "migrate-ids" => migrate_ids::run(&args[1..]).await,
        "build-doc-index" => build_doc_index::run(&args[1..]).await,
        "build-archive" => build_archive::run(&args[1..]).await,
        "enable-incremental" => enable_incremental::run(&args[1..]).await,
        "typed-extension" => typed_extension::run(&args[1..]).await,
        "stats" => stats::run(&args[1..]).await,
        other => {
            eprintln!("error: unknown atlas subcommand `{other}`");
            help::print(&HELP);
            2
        }
    }
}
