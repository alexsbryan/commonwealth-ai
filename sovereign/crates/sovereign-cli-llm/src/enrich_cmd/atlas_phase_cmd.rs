// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn enrich cluster / name` — the CLI surface for
//! [`sovereign_enrichment_build::atlas_phase_cmd`].
//!
//! Help text, flag parsing and the `cmd_*` entry point stay here because they
//! are this host's user interface. The work — `Parsed*`, `run`, `render` —
//! moved down to the capability crate (ontology-v1 P0.5) and is re-exported
//! below, so `super::atlas_phase_cmd::…` keeps resolving for this crate's siblings.

use sovereign_cli_shared::help::{self, Help, HelpSection};

pub use sovereign_enrichment_build::atlas_phase_cmd::*;

const CLUSTER_HELP: Help = Help {
    command: "svrn enrich cluster-atlas",
    summary: "Phase 2 (atlas): cluster atlas sketches by facet.",
    sections: &[
        HelpSection::Usage("svrn enrich cluster-atlas <corpus-id>"),
        HelpSection::Notes(
            "Reads the Phase 1 cache (must carry section_extraction payloads; re-run \
             extract with literary_atlas if not) and writes atlas-clusters cache + run \
             file. Idempotent — re-running overwrites the cache in place.",
        ),
    ],
};
pub async fn cmd_cluster_atlas(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&CLUSTER_HELP);
        return 0;
    }
    let Some(corpus_id) = args.first().cloned() else {
        eprintln!("error: missing <corpus-id>");
        eprintln!();
        help::print(&CLUSTER_HELP);
        return 2;
    };

    let parsed = ParsedCluster { corpus_id };
    match run_cluster(&parsed).await {
        Ok(report) => {
            render_cluster(&parsed.corpus_id, &report);
            0
        }
        Err(msg) => {
            eprintln!("error: {msg}");
            1
        }
    }
}
const NAME_HELP: Help = Help {
    command: "svrn enrich name-atlas-clusters",
    summary:
        "Phase 3 (atlas): name each facet cluster with a position / trajectory / thread label.",
    sections: &[
        HelpSection::Usage("svrn enrich name-atlas-clusters <corpus-id>"),
        HelpSection::Notes(
            "Reads the Phase 2 (atlas) cache and calls the atlas pipeline's \
             compose_phase3_facet per cluster. Writes atlas-named-clusters cache + run \
             file. The pipeline must implement compose_phase3_facet (literary_atlas \
             does); pipelines returning None from the trait default get a clear \
             error here rather than silent empty output.",
        ),
    ],
};
pub async fn cmd_name_atlas_clusters(args: &[String]) -> i32 {
    if help::wants_help(args) {
        help::print(&NAME_HELP);
        return 0;
    }
    let Some(corpus_id) = args.first().cloned() else {
        eprintln!("error: missing <corpus-id>");
        eprintln!();
        help::print(&NAME_HELP);
        return 2;
    };

    match run_name(&ParsedName { corpus_id }).await {
        Ok(report) => {
            render_name(&report);
            0
        }
        Err(msg) => {
            eprintln!("error: {msg}");
            1
        }
    }
}
