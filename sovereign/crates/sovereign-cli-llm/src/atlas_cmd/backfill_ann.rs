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

use crate::chat_cmd::bootstrap::build_inference;
use crate::chat_cmd::config::parse_globals;
use crate::enrich_cmd::paths;
use sovereign_tools::atlas_context_manager::{backfill_ann, AtlasContextFilter, BackfillOutcome};

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
    // already supports these knobs (`AtlasContextFilter`); we just surface them so
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

    // An embedder and an atlas directory is the whole of what this verb
    // needs. It used to ask for a `ChatSession`, which also opens the state
    // store, builds a `CorpusEngine` and commissions the shared recipe — and
    // the recipe loads the wiki graph (51,280 articles, 7.3M edges) and the
    // meta-atlas (1.57M atoms) into THIS process, beside the resident daemon.
    // Two concurrent invocations OOM-killed the daemon on 2026-09-04
    // (11:52:08); the embeds were never the price, the bootstrap was.
    // `build_inference` is the first two steps of that same bootstrap — the
    // probe, the model-id resolution, the HTTP provider — and nothing after
    // them (ei-3b step 0). `no_session_bootstrap_in_this_verb` below keeps it
    // that way structurally rather than by memory (ARCH §7).
    let inference = match build_inference(&globals).await {
        Ok((inference, _base, _embed_model)) => inference,
        Err(e) => {
            eprintln!("atlas backfill-ann: reach the daemon: {e}");
            return 1;
        }
    };
    // The loader is corpus-engine's now and takes an `EmbedFn`; the QUERY-side
    // adapter keeps this table in the same vector space `atlas_navigate_ann`
    // queries it in (ei-5a-build-cut). Built once, not once per corpus — the
    // provider behind it is the same Arc either way.
    let embed = sovereign_core::embed_fn::inference_to_embed_query_fn(inference);

    // CRITICAL: build the ANN table over the SAME atom universe the daemon /
    // desktop ground with — i.e. the production grounding filter, which is the
    // `AtlasContextManager`'s `AtlasContextFilter::default()` (env-aware:
    // SOVEREIGN_ATLAS_MIN_DESCRIPTION_CHARS / INCLUDE_DEPTHS / INCLUDE_CLAIMS).
    // Its default depth allowlist is `["extracted"]`, NOT the eval's all-depths
    // default — backfilling at the eval default would index atoms the production
    // seed path never sees (and key the embed cache the manager can't read).
    // Single source of truth, so the table can never drift from grounding.
    let prod = AtlasContextFilter::default();
    let (inc_claims, inc_tensions, inc_configs) = include_override.unwrap_or((
        prod.include_claims,
        prod.include_tensions,
        prod.include_configurations,
    ));
    // Overrides on top of `prod`, rather than a field-by-field rebuild of it:
    // whatever the operator did not override stays whatever grounding uses,
    // including fields added later.
    let filter = AtlasContextFilter {
        min_description_chars: min_chars_override.unwrap_or(prod.min_description_chars),
        depth_allowlist: depth_override
            .clone()
            .unwrap_or_else(|| prod.depth_allowlist.clone()),
        include_claims: inc_claims,
        include_tensions: inc_tensions,
        include_configurations: inc_configs,
        ..prod.clone()
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
    // One writer for every surface that seeds a corpus — this verb, the
    // `enrich build` Backfill step, and the daemon's post-write hook all call
    // `sovereign_tools::atlas_context_manager::backfill_ann` (ontology-v1 P0).
    for corpus_id in &corpora {
        let atlas_dir = paths::index_root(corpus_id).join(ATLAS_DIRNAME);
        match backfill_ann(&embed, &atlas_dir, corpus_id, &filter).await {
            Ok(BackfillOutcome::Built(stats)) => {
                println!(
                    "backfill-ann {corpus_id}: wrote {} — {}/{} bag entries resolved to atom-ids",
                    atlas_dir.join(ANN_TABLE_DIRNAME).display(),
                    stats.resolved,
                    stats.total
                );
                built += 1;
            }
            Ok(BackfillOutcome::NoSeedableAtoms {
                min_description_chars,
            }) => {
                // The operator asked for a table and got none: a failure at
                // this surface, with the knobs that widen the filter named.
                eprintln!(
                    "backfill-ann {corpus_id}: filter excluded every atom \
                     (min_chars={min_description_chars}, depth={:?}) — nothing to index; \
                     see --atlas-min-description-chars / --atlas-depth / --atlas-include",
                    filter.depth_allowlist
                );
                failed += 1;
            }
            Err(e) => {
                eprintln!("backfill-ann {corpus_id}: {e}");
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    /// The needle, assembled at runtime and never written as a literal.
    /// THIS FILE IS ONE OF THE FILES BEING SCANNED, so a literal here would
    /// make the guard match its own source and fail for the wrong reason —
    /// the same trap `bench_cmd_is_the_only_module_naming_the_eval_harness`
    /// documents in `lib.rs`. Keep the token out of this file, test names and
    /// assertion text included.
    fn session_builder() -> String {
        ["build", "session"].join("_")
    }

    fn src(rel: &str) -> (PathBuf, String) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(rel);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        // A guard that scanned nothing is `never-ran`, not `passed` (ARCH
        // §18.1). If this file is ever moved or renamed, fail here — loudly —
        // rather than reporting green over an empty scan.
        assert!(
            text.len() > 500,
            "{} is {} bytes — the scan found no source to check",
            path.display(),
            text.len()
        );
        (path, text)
    }

    /// `svrn atlas backfill-ann` must reach the daemon through
    /// `build_inference` (probe + model ids + HTTP provider) and NOT through
    /// the `ChatSession` bootstrap, which additionally opens the state store,
    /// builds a `CorpusEngine`, and commissions the shared recipe — the recipe
    /// that loads the wiki graph (51,280 articles, 7.3M edges) and the
    /// meta-atlas (1.57M atoms) into this process. Two concurrent invocations
    /// of this verb OOM-killed the resident daemon on 2026-09-04 at 11:52:08.
    ///
    /// # What this proves, and what it does not
    ///
    /// It is a two-hop source check over the verb's actual call graph: this
    /// module, plus the body of the one bootstrap helper it calls. Those are
    /// the only two places the session builder could be named on the path from
    /// `run` to `backfill_ann`. It is NOT a whole-graph proof — a future edit
    /// that routes through some third helper would pass this and still boot a
    /// session, and the fix then is to add that helper here, not to weaken the
    /// check. `callees("run")` is the exact instrument; this is the one that
    /// runs in CI on every push with no index and no daemon.
    ///
    /// Failing input, so this is a gate and not a decoration: revert either
    /// half of ei-3b step 0 and hop 1 or hop 2 fails by name.
    #[test]
    fn no_session_bootstrap_in_this_verb() {
        let needle = session_builder();

        // Hop 1 — the verb's own module, PRODUCTION HALF ONLY. Truncated at
        // `#[cfg(test)]` because this test module is inside the file it
        // scans: its own assertion literals would otherwise satisfy the
        // positive control below even with the call deleted, and a prose
        // comment in here would trip the negative one (it did, on the first
        // cut). Scanning only the half that ships makes both honest.
        let (path, whole) = src("atlas_cmd/backfill_ann.rs");
        let verb = whole
            .split_once("#[cfg(test)]")
            .map(|(prod, _)| prod)
            .unwrap_or_else(|| panic!("{}: no test module to split on", path.display()));
        assert!(
            verb.len() > 500,
            "{}: production half is {} bytes — the scan found nothing to check",
            path.display(),
            verb.len()
        );
        assert!(
            !verb.contains(&needle),
            "{} names the session bootstrap. This verb needs an embedder and \
             an atlas dir; a session also loads the wiki graph and the \
             meta-atlas into this process and has OOM-killed the daemon. Use \
             `build_inference` (chat_cmd::bootstrap).",
            path.display()
        );
        // Positive control: hop 1 passes trivially if the verb stopped
        // reaching the daemon at all, so pin the exact call it DOES make.
        // The full call form, not the bare name: a rename to some
        // `build_inference<suffix>` would satisfy a substring check while
        // hop 2 went on inspecting the original function — the wrong one.
        assert!(
            verb.contains("build_inference(&globals)"),
            "{} no longer calls `build_inference(&globals)` — either the verb \
             changed shape or this guard is now checking nothing",
            path.display()
        );

        // Hop 2 — the body of the helper hop 1 pins. Scoped to that
        // function, because the session builder's `_with_skills` sibling
        // lives in the same file and is of course allowed to name itself.
        let (boot_path, boot) = src("chat_cmd/bootstrap.rs");
        let start = boot
            .find("pub async fn build_inference(")
            .unwrap_or_else(|| panic!("{}: no `build_inference`", boot_path.display()));
        let rest = &boot[start..];
        let end = rest
            .find("\n}\n")
            .unwrap_or_else(|| panic!("{}: `build_inference` has no end", boot_path.display()));
        let body = &rest[..end];
        assert!(
            !body.contains(&needle),
            "{}: `build_inference` names the session bootstrap, so the verb \
             reaches it transitively",
            boot_path.display()
        );
    }
}
