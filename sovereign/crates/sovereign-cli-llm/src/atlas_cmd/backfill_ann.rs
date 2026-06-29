// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sovereign atlas backfill-ann <corpus>...` — ATLAS_STORAGE_V2 step 3b.
//!
//! Build the persistent per-corpus ANN seed table (`atlas/atoms_ann.lance`) for
//! each named atlas corpus. This is a cheap one-time TRANSFORM of data that
//! already exists: it reads the atlas embedding bag (the same cache the eval and
//! daemon load), resolves each entry to its atom-id (the join the v1 cosine seed
//! ran per QUERY — done once here), and writes `(atom_id, embedding)` to a flat
//! Lance vector table. No LLM, no re-extraction, no re-embedding. Once built,
//! the daemon's `atlas_navigate_ann` seeds directly from atom-ids with no resolve.
//!
//! Filter note: the table is built with the PRODUCTION grounding filter — the
//! `AtlasContextManager`'s `AtlasContextFilter::default()` (env-aware; default
//! `min_description_chars=200`, depth `["extracted"]`, no claim/tension/config
//! includes) — so the ANN table covers exactly the atom universe the daemon /
//! desktop seed `atlas_navigate` from. NOT the eval's all-depths default. Verify
//! with `--atlas-depth extracted` (+ matching env) so the eval's ANN and cosine
//! arms see the same universe production does.

use corpus_engine::enrichment::atlas::ann_store::ANN_TABLE_DIRNAME;
use corpus_engine::enrichment::atlas::ATLAS_DIRNAME;

use crate::chat_cmd::bootstrap::build_session;
use crate::chat_cmd::config::parse_globals;
use crate::enrich_cmd::paths;
use crate::eval_cmd::runner::{self, AtlasLoadFilter};
use sovereign_core::atlas_context::{build_persistent_ann_seed_table, AtlasGraph};

pub async fn run(args: &[String]) -> i32 {
    let (globals, rest) = match parse_globals(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("atlas backfill-ann: {e}");
            return 2;
        }
    };
    // Positional corpus ids (comma- or space-separated), skipping any stray
    // flag tokens parse_globals left behind.
    let corpora: Vec<String> = rest
        .iter()
        .filter(|a| !a.starts_with('-'))
        .flat_map(|a| {
            a.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
        })
        .collect();
    if corpora.is_empty() {
        eprintln!("usage: sovereign atlas backfill-ann <corpus-id>[,<corpus-id>...]");
        eprintln!("  builds <corpus>/atlas/atoms_ann.lance (ATLAS_STORAGE_V2 3b ANN seed table)");
        return 2;
    }

    let session = match build_session(&globals).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("atlas backfill-ann: build session: {e}");
            return 1;
        }
    };

    // CRITICAL: build the ANN table over the SAME atom universe the daemon /
    // desktop ground with — i.e. the production grounding filter, which is the
    // `AtlasContextManager`'s `AtlasContextFilter::default()` (env-aware:
    // SOVEREIGN_ATLAS_MIN_DESCRIPTION_CHARS / INCLUDE_DEPTHS / INCLUDE_CLAIMS).
    // Its default depth allowlist is `["extracted"]`, NOT the eval's all-depths
    // default — backfilling at the eval default would index atoms the production
    // seed path never sees (and key the embed cache the manager can't read).
    // Single source of truth, so the table can never drift from grounding.
    let prod = sovereign_tools::atlas_context_manager::AtlasContextFilter::default();
    let filter = AtlasLoadFilter {
        min_description_chars: prod.min_description_chars,
        depth_allowlist: prod.depth_allowlist.clone(),
        max_entries: prod.max_entries,
        include_claims: prod.include_claims,
        include_tensions: prod.include_tensions,
        include_configurations: prod.include_configurations,
    };
    eprintln!(
        "atlas backfill-ann: production grounding filter — min_chars={} depth={:?} claims={} tensions={} configs={}",
        filter.min_description_chars,
        filter.depth_allowlist,
        filter.include_claims,
        filter.include_tensions,
        filter.include_configurations,
    );

    let mut built = 0usize;
    let mut failed = 0usize;
    for corpus_id in &corpora {
        let atlas_dir = paths::index_root(corpus_id).join(ATLAS_DIRNAME);
        let ctx = match runner::load_atlas_context(&session, corpus_id, 3, &filter).await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("backfill-ann {corpus_id}: load atlas context: {e}");
                failed += 1;
                continue;
            }
        };
        let graph = match AtlasGraph::load_from_disk(corpus_id, &atlas_dir) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("backfill-ann {corpus_id}: load graph: {e}");
                failed += 1;
                continue;
            }
        };
        match build_persistent_ann_seed_table(&atlas_dir, &ctx, &graph).await {
            Ok(stats) => {
                println!(
                    "backfill-ann {corpus_id}: wrote {} — {}/{} bag entries resolved to atom-ids",
                    atlas_dir.join(ANN_TABLE_DIRNAME).display(),
                    stats.resolved,
                    stats.total
                );
                built += 1;
            }
            Err(e) => {
                eprintln!("backfill-ann {corpus_id}: build ANN table: {e}");
                failed += 1;
            }
        }
    }
    println!("atlas backfill-ann: {built} built, {failed} failed");
    i32::from(failed > 0)
}
