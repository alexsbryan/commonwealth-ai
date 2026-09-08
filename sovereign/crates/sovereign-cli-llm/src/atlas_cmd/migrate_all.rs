// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn atlas migrate-all` — the reusable ATLAS_STORAGE_V2 full-port.
//!
//! One idempotent command that migrates EVERY atlas-bearing corpus on this
//! machine to the v2 system, so the port is repeatable on any dev machine with
//! a single invocation:
//!
//!   - **atom corpora** -> v2 store (`atoms.lance` + `edges.csr`, the direct-read
//!     reader) + `atoms_ann.lance` (ANN seeding) for those that already carry an
//!     embeddings cache + the per-corpus `atlas/.read_v2` flip (so the daemon /
//!     desktop read v2 instead of the rkyv archive).
//!   - **wiki-class corpora** (those with the columnar `articles.lance` +
//!     `edges.lance` link graph) are REPORTED and skipped. Since
//!     WIKIPEDIA_ATLAS_V2 W4 there is no SQLite for this command to convert
//!     from, and the only build path — `atlas wikipedia build-graph` — reads
//!     the chunk index, which this command never opens. A row that says "not
//!     this command" is the honest answer; silently counting it as migrated
//!     would not be (ARCH §18.3).
//!
//! Idempotent: skips a store/columnar already current vs its source, and an ANN
//! table that already exists for an unchanged store. Re-runnable and safe to
//! interrupt. The ANN step goes through the ONE seed-table writer,
//! `atlas_context_manager::backfill_ann` — the same call `svrn atlas
//! backfill-ann`, the `enrich build` Backfill step and the atlas writer make —
//! so the table's POPULATION is the corpus's own navigation map
//! (`seed_population`) and its marker is stamped with it. It only touches
//! corpora that already have an embeddings cache (it never bulk-embeds the
//! resident set). The `read_v2` flip is reversible (delete the marker) and
//! `load_from_disk` falls back to rkyv if a store is absent/unreadable, so a
//! flip can never strand an atlas.
//!
//! It did NOT go through that writer until ei-5c, and the consequence was
//! measured: this verb loaded the bag under `AtlasContextFilter::default()` and
//! called `build_persistent_ann_seed_table` itself, which is the retrieval
//! filter authoring the writer's table — the exact disagreement `seed_population`
//! exists to close (ARCH §10.6, one decider). It rebuilt the table from the
//! always-seeded pair and DROPPED all 2,004 `Summary` rows from the ei-7a
//! fixture, and because it never stamped a population marker the result read as
//! fresh. Two implementations of one question, one of them silent.

use std::path::Path;

use corpus_engine::enrichment::atlas::ann_store::ann_table_present;
use corpus_engine::enrichment::atlas::store::{build_and_write_store, store_needs_build};
use corpus_engine::enrichment::atlas::ATLAS_DIRNAME;
use corpus_engine::wikipedia_graph_present;

use crate::chat_cmd::bootstrap::{build_session, ChatSession};
use crate::chat_cmd::config::parse_globals;
use crate::eval_cmd::runner::AtlasContextFilter;
use sovereign_tools::atlas_context_manager::{backfill_ann, BackfillOutcome};

pub async fn run(args: &[String]) -> i32 {
    let (globals, rest) = match parse_globals(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("atlas migrate-all: {e}");
            return 2;
        }
    };
    let mut flip = true; // the migration flips read_v2 by default
    let mut only: Option<String> = None;
    for a in &rest {
        match a.as_str() {
            "--flip" => flip = true,
            "--no-flip" => flip = false,
            "-h" | "--help" => {
                print_help();
                return 0;
            }
            other if other.starts_with('-') => {
                eprintln!("unknown flag: {other}");
                return 2;
            }
            other => only = Some(other.to_string()),
        }
    }

    let indexes_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .unwrap_or_else(|_| sovereign_contracts::rebrand::svrnmesh_root())
        .join("indexes");

    let corpora: Vec<String> = if let Some(c) = only {
        vec![c]
    } else {
        match std::fs::read_dir(&indexes_dir) {
            Ok(rd) => {
                let mut v: Vec<String> = rd
                    .flatten()
                    .filter(|e| e.path().join(ATLAS_DIRNAME).join("atoms.json").exists())
                    .filter_map(|e| {
                        e.path()
                            .file_name()
                            .and_then(|n| n.to_str())
                            .map(String::from)
                    })
                    .filter(|n| !n.starts_with('.') && !n.starts_with('_'))
                    .collect();
                v.sort();
                v
            }
            Err(e) => {
                eprintln!("atlas migrate-all: read_dir {}: {e}", indexes_dir.display());
                return 1;
            }
        }
    };
    if corpora.is_empty() {
        eprintln!(
            "atlas migrate-all: no atlas-bearing corpora under {}",
            indexes_dir.display()
        );
        return 0;
    }
    eprintln!(
        "atlas migrate-all: {} atlas-bearing corpora; flip={flip}",
        corpora.len()
    );

    // The session is built ON FIRST NEED, not here (ei-3b step 0b).
    //
    // Only the ANN sub-step below uses it, and only for a corpus that already
    // carries an embed marker or an ANN table. The store build
    // (`build_and_write_store`) and the `read_v2` flip need no daemon, no
    // inference and no model at all — they are a local file transform. Building
    // a `ChatSession` up here made every invocation load the wiki graph (51,280
    // articles, 7.3M edges) and the meta-atlas (1.57M atoms) into this process
    // first, which is what stopped the ei-3b battery on its first batch: of the
    // 1,081 store-less `sep-*` atlases the driver calls this verb for, exactly
    // 34 carry an embed marker, so 1,047 of them paid a full bootstrap to
    // resolve `ann_state = "n/a"` and touch the session zero times.
    //
    // `Err` is remembered as well as `Ok`: without that, a box with no daemon
    // would re-probe once per corpus, 1,081 times.
    let mut session: Option<ChatSession> = None;
    let mut session_err: Option<String> = None;
    // The PRODUCTION grounding filter carries the quality knobs
    // (`min_description_chars`, the depth allowlist, the cap); the corpus's
    // navigation map carries the KINDS, and `backfill_ann` derives them. This
    // verb passes the same filter the atlas writer passes, and decides nothing
    // about the population itself.
    let filter = AtlasContextFilter::default();

    println!(
        "{:<46} {:>7} {:>8} {:>5}  track",
        "corpus", "store", "ann", "flip"
    );
    println!("{}", "-".repeat(82));
    let (mut stores, mut anns, mut flips, mut wikis, mut errs) =
        (0usize, 0usize, 0usize, 0usize, 0usize);

    for corpus_id in &corpora {
        let atlas_dir = indexes_dir.join(corpus_id).join(ATLAS_DIRNAME);

        // Wiki-class: a corpus with the columnar link graph is on the wiki
        // track, not the atom track. There is nothing for THIS command to
        // migrate any more — the SQLite it used to convert from is retired
        // (WIKIPEDIA_ATLAS_V2 W4), and the only build path is
        // `atlas wikipedia build-graph`, which needs the chunk index this
        // command never opens. So it reports and moves on; it does not
        // pretend to have done work (ARCH §18.3).
        if wikipedia_graph_present(&indexes_dir, corpus_id) {
            wikis += 1;
            println!(
                "{corpus_id:<46} {:>7} {:>8} {:>5}  wiki columnar (not this command)",
                "-", "-", "-"
            );
            continue;
        }

        // Atom track. 1) store (idempotent).
        let mut store_built = false;
        let store_state = if store_needs_build(&atlas_dir) {
            match build_and_write_store(&atlas_dir, corpus_id).await {
                Ok(_) => {
                    stores += 1;
                    store_built = true;
                    "built"
                }
                Err(e) => {
                    errs += 1;
                    println!("{corpus_id:<46}  ERROR store: {e}");
                    continue;
                }
            }
        } else {
            "current"
        };

        // 2) ANN — scope to embedding-bearing corpora (never bulk-embed the
        // structural set). `atoms.embeddings.bin` is no longer written
        // (ATLAS_STORAGE_V2 Phase B retired the embed cache), but the file
        // persists on disk as the legacy "this corpus was embedded" marker, and
        // an existing ANN table is the forward-looking equivalent — either signal
        // admits the corpus. Builds the stragglers (embedded but no table yet)
        // and leaves current tables as-is; fresh corpora get their table via
        // `svrn atlas backfill-ann`.
        let ann_state: &str =
            if !ann_table_present(&atlas_dir) && !atlas_dir.join("atoms.embeddings.bin").exists() {
                "n/a"
            } else if ann_table_present(&atlas_dir) && !store_built {
                "current"
            } else {
                // FIRST corpus that actually needs to embed pays for the session;
                // the rest of the run reuses it, and a failure is remembered so a
                // dead daemon costs one probe, not one per corpus.
                if session.is_none() && session_err.is_none() {
                    match build_session(&globals).await {
                        Ok(s) => session = Some(s),
                        Err(e) => session_err = Some(e.to_string()),
                    }
                }
                match session.as_ref() {
                    // Named, never defaulted (ARCH §18.3). This corpus asked for a
                    // table and did not get one, so the row reads `err`, not `n/a`
                    // — and control falls through, so the store stays built and the
                    // read_v2 flip below still happens, exactly as on any other ANN
                    // failure.
                    None => {
                        errs += 1;
                        eprintln!(
                            "  {corpus_id}: ann needs the daemon and it is not reachable: {}",
                            session_err.as_deref().unwrap_or("unknown")
                        );
                        "err"
                    }
                    Some(session) => {
                        // The ONE writer. It derives the population from this
                        // corpus's navigation map, seeds under it, and stamps the
                        // population marker in the same call — none of which this
                        // verb may decide for itself.
                        let embed = sovereign_core::embed_fn::inference_to_embed_query_fn(
                            session.inference.clone(),
                        );
                        match backfill_ann(&embed, &atlas_dir, corpus_id, &filter).await {
                            Ok(BackfillOutcome::Built(_)) => {
                                anns += 1;
                                "built"
                            }
                            // A corpus the filter empties carries only surfaces the
                            // grounding filter does not admit. Reported as `none`,
                            // never as a build (ARCH §18.3). The relaxed-floor retry
                            // this branch used to make is gone with the fork: one
                            // filter, the one the daemon seeds with — the same rule
                            // `backfill_ann`'s own doc records for `svrn atlas
                            // backfill-ann`.
                            Ok(BackfillOutcome::NoSeedableAtoms { .. }) => "none",
                            Err(e) => {
                                errs += 1;
                                eprintln!("  {corpus_id}: ann build: {e}");
                                "err"
                            }
                        }
                    }
                }
            };

        // 3) flip read_v2 (reversible; rkyv stays the fallback).
        let flip_state: &str = if !flip {
            "-"
        } else {
            let marker = atlas_dir.join(".read_v2");
            if marker.exists() {
                "on"
            } else {
                match std::fs::File::create(&marker) {
                    Ok(_) => {
                        flips += 1;
                        "set"
                    }
                    Err(e) => {
                        errs += 1;
                        eprintln!("  {corpus_id}: flip: {e}");
                        "err"
                    }
                }
            }
        };

        println!("{corpus_id:<46} {store_state:>7} {ann_state:>8} {flip_state:>5}  atom");
    }

    println!(
        "\nmigrate-all: {stores} stores built, {anns} ANN tables, {flips} flipped, \
         {wikis} wiki columnar, {errs} errors (over {} corpora)",
        corpora.len()
    );
    i32::from(errs > 0)
}

/// True if `a`'s mtime is at least `b`'s — the cheap "already current" gate.
fn newer_than(a: &Path, b: &Path) -> bool {
    let mt = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    match (mt(a), mt(b)) {
        (Some(x), Some(y)) => x >= y,
        _ => false,
    }
}

fn print_help() {
    println!("svrn atlas migrate-all — reusable ATLAS_STORAGE_V2 full-port (idempotent)\n");
    println!("  sovereign atlas migrate-all                 migrate every atlas-bearing corpus + flip read_v2");
    println!(
        "  sovereign atlas migrate-all --no-flip       build v2 artifacts but do NOT flip read_v2"
    );
    println!("  sovereign atlas migrate-all <corpus_id>     migrate one corpus");
    println!(
        "\natom corpora -> atoms.lance + edges.csr (+ atoms_ann.lance if embedded) + .read_v2"
    );
    println!(
        "wiki-class   -> articles.lance + edges.lance, built by `atlas wikipedia build-graph`"
    );
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// Assembled at runtime, never a literal: this test module is INSIDE the
    /// file it scans, so a literal would match its own source. Same trap as
    /// `no_session_bootstrap_in_this_verb` and as `lib.rs`'s harness guard.
    fn session_builder() -> String {
        ["build", "session"].join("_")
    }

    /// The production half of this file — the same split
    /// `migrate_all_builds_no_session_before_it_needs_one` uses, so a doc
    /// comment or an assertion literal cannot answer for the code.
    fn production_source() -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("atlas_cmd/migrate_all.rs");
        let whole = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        whole
            .split_once("#[cfg(test)]")
            .map(|(p, _)| p.to_string())
            .unwrap_or_else(|| panic!("{}: no test module to split on", path.display()))
    }

    /// `migrate-all` must SEED THROUGH THE ONE WRITER, not build a seed table
    /// of its own.
    ///
    /// What it did until ei-5c: loaded the bag under
    /// `AtlasContextFilter::default()` and called
    /// `build_persistent_ann_seed_table` directly. That is the RETRIEVAL filter
    /// authoring the writer's table — Entity-only in production — so this verb
    /// rebuilt every table it touched from the always-seeded pair and wrote no
    /// population marker, which made the result read as fresh. Measured on the
    /// ei-7a SEP fixture: all 2,004 `Summary` rows dropped, exit 0, every count
    /// in the report looking right. Two implementations of one question (ARCH
    /// §10.6), and the silent one won.
    ///
    /// Positional, like its sibling, because the defect is a CALL SITE and not
    /// a value: the behavioural proof is in the commit body (rebuild the
    /// fixture's table with each binary and diff the per-kind census). Failing
    /// inputs, both directions (§18.6): name either forked symbol again (hop 1
    /// goes red), or stop naming the writer (the positive control goes red).
    #[test]
    fn the_ann_step_seeds_through_the_one_writer() {
        let prod = production_source();
        for forked in [
            ["build", "persistent", "ann", "seed", "table"].join("_"),
            ["load", "atlas", "context"].join("_"),
        ] {
            assert!(
                !prod.contains(&forked),
                "migrate_all.rs names `{forked}` — it is authoring a seed table \
                 instead of asking `backfill_ann` for one, so the population is \
                 this verb's filter rather than the corpus's navigation map"
            );
        }
        let writer = ["backfill", "ann"].join("_");
        assert!(
            prod.contains(&writer),
            "nothing in migrate_all.rs calls `{writer}`, so either the ANN \
             sub-step is gone or this guard is checking nothing"
        );
    }

    /// `svrn atlas migrate-all` must not build a `ChatSession` before it knows
    /// it needs one.
    ///
    /// The store build and the `read_v2` flip are a local file transform — no
    /// daemon, no inference, no model. Only the ANN sub-step embeds, and only
    /// for a corpus already carrying an embed marker or an ANN table. Building
    /// the session in the prologue made every invocation load the wiki graph
    /// (51,280 articles, 7.3M edges) and the meta-atlas (1.57M atoms) first,
    /// which is what stopped the ei-3b battery on its first batch: 1,081
    /// store-less `sep-*` atlases, of which 34 carry an embed marker, so 1,047
    /// paid a full bootstrap to touch the session zero times.
    ///
    /// So the invariant is positional, and this test is positional: the session
    /// builder may be named only AFTER the per-corpus loop opens. Before it is
    /// the prologue, and the prologue is the bug.
    ///
    /// # What this proves, and what it does not
    ///
    /// It pins where the call site sits, not what happens at runtime. The
    /// behavioural proof is the one recorded in the commit body and is the
    /// better instrument: with the daemon made unreachable
    /// (`--daemon http://127.0.0.1:1`) the old binary exits 1 having built
    /// nothing, and the new one builds the store and exits 0. Reproduce that
    /// rather than trusting this test if you change the shape here.
    ///
    /// Failing inputs, so this is a gate and not a decoration: move the call
    /// back into the prologue (hop 1 goes red), or delete it entirely (the
    /// positive control goes red).
    #[test]
    fn migrate_all_builds_no_session_before_it_needs_one() {
        let needle = session_builder();
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("atlas_cmd/migrate_all.rs");
        let whole = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        // Production half only — this module's own prose would otherwise trip
        // the scan, and its assertion literals would flatter the control.
        let prod = whole
            .split_once("#[cfg(test)]")
            .map(|(p, _)| p)
            .unwrap_or_else(|| panic!("{}: no test module to split on", path.display()));

        // The `use` line legitimately names the builder, so the scan starts at
        // the function, not at the top of the file.
        let run_at = prod
            .find("pub async fn run(")
            .unwrap_or_else(|| panic!("{}: no `run`", path.display()));
        let loop_at = prod
            .find("for corpus_id in &corpora")
            .unwrap_or_else(|| panic!("{}: no per-corpus loop to divide on", path.display()));
        assert!(
            loop_at > run_at,
            "{}: the loop precedes `run` — the scan is not oriented",
            path.display()
        );

        let prologue = &prod[run_at..loop_at];
        assert!(
            !prologue.contains(&needle),
            "{}: `run`'s prologue builds a session before it knows a corpus needs \
             one. The store build and the read_v2 flip need no daemon; 1,047 of \
             the 1,081 store-less sep-* atlases never reach the ANN branch. Build \
             it on first need inside the loop.",
            path.display()
        );

        // Positive control: the prologue is trivially clean if the call is gone
        // altogether, which would strand every embedding-bearing corpus.
        let body = &prod[loop_at..];
        assert!(
            body.contains(&needle),
            "{}: nothing after the loop builds a session, so the ANN sub-step \
             cannot embed — either the verb changed shape or this guard is now \
             checking nothing",
            path.display()
        );
    }
}
