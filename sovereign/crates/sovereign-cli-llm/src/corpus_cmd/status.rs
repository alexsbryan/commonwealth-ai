// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn corpus status` — the PRINTING half.
//!
//! Split out of `inventory.rs` on 2026-09-02 (that file had crossed ARCH
//! §3.1's 1200 ceiling). The status ROW, the readiness decider and their
//! rules moved DOWN into corpus-engine on 2026-09-08 (sv-surface rung 1):
//! the desktop walked the same directories through
//! `CorpusEngine::installed_indexes` with different rules, and the CLI's
//! private walk was the twin. One decider now —
//! [`corpus_engine::engine::status`] — with this file its printing
//! consumer and the daemon's `GET /internal/corpus/status` its wire
//! consumer. The rules' recorded history (the partition-name regression,
//! the `Unsearchable` reclassification) lives in the decider's module, not
//! here.

use corpus_engine::engine::status::{scan_corpus_rows, CorpusReadiness};

use super::fmt::format_count;

/// `svrn corpus status [<corpus>]`
///
/// With no argument, every corpus the indexes dir knows about. With a
/// corpus id, just that one — which is what makes the `state` column
/// assertable by a caller that cares about ONE corpus (the CLI-contract
/// `enrich-atlas` journey greps this output for `ready`; unfiltered, some
/// OTHER corpus being ready would satisfy it).
pub(super) async fn cmd_corpus_status(args: &[String]) -> i32 {
    let indexes_dir = sovereign_core::setup_config::SetupConfig::load()
        .map(|c| c.data.dir)
        .unwrap_or_else(|_| sovereign_contracts::rebrand::svrnmesh_root())
        .join("indexes");
    let filter: Option<&str> = args
        .iter()
        .map(|s| s.as_str())
        .find(|a| !a.starts_with('-'));
    let mut rows = match scan_corpus_rows(&indexes_dir) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: read {}: {e}", indexes_dir.display());
            return 1;
        }
    };
    if let Some(want) = filter {
        rows.retain(|r| r.corpus_id == want);
        if rows.is_empty() {
            // ABSENCE IS REPORTED, NOT DEFAULTED (§18.3). A filtered
            // status that matched nothing must say so in the state
            // vocabulary the caller is grepping for, and must not print
            // an empty table that a `stdout_non_empty` check would pass.
            println!("{:<32} {:>12}", want, CorpusReadiness::Absent.label());
            println!(
                "(no index for '{want}' under {} — `svrn corpus install {want} --wait`)",
                indexes_dir.display()
            );
            return 0;
        }
    }
    if rows.is_empty() {
        println!("(no corpora installed at {})", indexes_dir.display());
        return 0;
    }
    println!(
        "{:<32} {:>12} {:>14} {:>10} {:>10} {:>8} {:>10} {:>12}",
        "corpus", "state", "chunks", "atlas", "tier-2", "seeded", "embed-cache", "tier-2 toks"
    );
    println!("{}", "─".repeat(114));
    for r in rows {
        let chunks = r
            .chunk_count
            .map(|n| format_count(n as u64))
            .unwrap_or_else(|| "—".into());
        let atlas = r
            .atlas_entities
            .map(|n| format_count(n as u64))
            .unwrap_or_else(|| "—".into());
        let tier2 = r
            .atlas_extracted_entities
            .map(|n| format_count(n as u64))
            .unwrap_or_else(|| "—".into());
        // Seed-table coverage, read against the `atlas` column beside it. `—`
        // is NOT zero: it means there is no `atoms_ann.lance` at all and this
        // corpus answers from chunks alone (EPISTEMIC_INDEX section 1, Ideas
        // row -- the artifact is mandatory and its absence is reported).
        let seeded = r
            .atlas_embedded_atoms
            .map(format_count)
            .unwrap_or_else(|| "—".into());
        let cache: String = if r.atlas_embeddings_cached {
            "✓".into()
        } else {
            "—".into()
        };
        let tokens = r
            .tier2_total_tokens
            .map(format_count)
            .unwrap_or_else(|| "—".into());
        println!(
            "{:<32} {:>12} {:>14} {:>10} {:>10} {:>8} {:>10} {:>12}",
            r.corpus_id,
            r.state.label(),
            chunks,
            atlas,
            tier2,
            seeded,
            cache,
            tokens
        );
    }
    0
}

