// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn atlas backfill-ann <corpus>...` — ATLAS_STORAGE_V2 step 3b.
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
use sovereign_core::atlas_context::build_persistent_ann_seed_table;

pub async fn run(args: &[String]) -> i32 {
    let (globals, rest) = match parse_globals(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("atlas backfill-ann: {e}");
            return 2;
        }
    };
    // Parse the filter-override flags out of `rest` (each is flag + value),
    // leaving plain positional tokens as corpus ids. The atlas-context filter
    // already supports these knobs (`AtlasLoadFilter`); we just surface them so
    // a CODE atlas — `structural` depth, short/empty summaries — can be indexed.
    // Without them the default prose profile (`extracted` depth, min 200 chars)
    // excludes every code atom and the backfill resolves 0 entries.
    let mut depth_override: Option<Vec<String>> = None;
    let mut min_chars_override: Option<usize> = None;
    let mut include_override: Option<(bool, bool, bool)> = None;
    let mut positionals: Vec<String> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let arg = rest[i].as_str();
        let take_value = |i: &mut usize, name: &str| -> Result<String, ()> {
            match rest.get(*i + 1) {
                Some(v) => {
                    *i += 2;
                    Ok(v.clone())
                }
                None => {
                    eprintln!("atlas backfill-ann: {name} needs a value");
                    Err(())
                }
            }
        };
        match arg {
            "--atlas-depth" => {
                let Ok(v) = take_value(&mut i, "--atlas-depth") else {
                    return 2;
                };
                depth_override = Some(csv(&v));
            }
            "--atlas-min-description-chars" => {
                let Ok(v) = take_value(&mut i, "--atlas-min-description-chars") else {
                    return 2;
                };
                match v.parse::<usize>() {
                    Ok(n) => min_chars_override = Some(n),
                    Err(_) => {
                        eprintln!(
                            "atlas backfill-ann: --atlas-min-description-chars not a number: {v}"
                        );
                        return 2;
                    }
                }
            }
            "--atlas-include" => {
                let Ok(v) = take_value(&mut i, "--atlas-include") else {
                    return 2;
                };
                let (mut c, mut t, mut g) = include_override.unwrap_or((false, false, false));
                for tok in csv(&v) {
                    match tok.as_str() {
                        "claim" | "claims" => c = true,
                        "tension" | "tensions" => t = true,
                        "config" | "configuration" | "configurations" => g = true,
                        other => {
                            eprintln!("atlas backfill-ann: unknown --atlas-include value: {other} (claim|tension|configuration)");
                            return 2;
                        }
                    }
                }
                include_override = Some((c, t, g));
            }
            flag if flag.starts_with('-') => {
                // Unknown flag parse_globals left behind — skip (don't treat as a corpus).
                i += 1;
            }
            _ => {
                positionals.push(rest[i].clone());
                i += 1;
            }
        }
    }
    // Positional corpus ids (comma- or space-separated).
    let corpora: Vec<String> = positionals.iter().flat_map(|a| csv(a)).collect();
    if corpora.is_empty() {
        eprintln!("usage: sovereign atlas backfill-ann <corpus-id>[,<corpus-id>...] \\");
        eprintln!("         [--atlas-depth <csv>] [--atlas-min-description-chars <n>] [--atlas-include <csv>]");
        eprintln!("  builds <corpus>/atlas/atoms_ann.lance (ATLAS_STORAGE_V2 3b ANN seed table)");
        eprintln!("  code atlases: --atlas-depth structural --atlas-min-description-chars 1");
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
    let (inc_claims, inc_tensions, inc_configs) = include_override.unwrap_or((
        prod.include_claims,
        prod.include_tensions,
        prod.include_configurations,
    ));
    let filter = AtlasLoadFilter {
        min_description_chars: min_chars_override.unwrap_or(prod.min_description_chars),
        depth_allowlist: depth_override
            .clone()
            .unwrap_or_else(|| prod.depth_allowlist.clone()),
        max_entries: prod.max_entries,
        include_claims: inc_claims,
        include_tensions: inc_tensions,
        include_configurations: inc_configs,
    };
    let overridden =
        depth_override.is_some() || min_chars_override.is_some() || include_override.is_some();
    eprintln!(
        "atlas backfill-ann: {} filter — min_chars={} depth={:?} claims={} tensions={} configs={}",
        if overridden {
            "overridden"
        } else {
            "production grounding"
        },
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
        match build_persistent_ann_seed_table(&atlas_dir, &ctx).await {
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

/// Split a comma- (or whitespace-) separated value into trimmed, non-empty
/// tokens. Used for `--atlas-depth`, `--atlas-include`, and comma-joined
/// corpus-id positionals.
fn csv(s: &str) -> Vec<String> {
    s.split([',', ' '])
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(String::from)
        .collect()
}
