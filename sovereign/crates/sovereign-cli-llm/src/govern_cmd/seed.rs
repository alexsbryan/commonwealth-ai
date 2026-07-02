// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn govern seed` — establish the governed rule baseline.
//!
//! A rule is "governed" iff some oplog op touched its id — the oplog, not
//! a claim-field heuristic, defines the rule set. Nothing auto-emits
//! these ops today (enrichment writes atoms/edges, not the oplog), so
//! `seed` is the explicit baseline step: one idempotent `AssertRule` per
//! Claim atom. Re-running skips rules already asserted, so it is safe to
//! re-seed after a re-enrich. (Future direction: emit `AssertRule` from
//! the enrich pipeline for governance recipes so this is automatic.)

use std::collections::HashSet;

use corpus_engine::enrichment::atlas::{read_atlas_atoms, AtomEnvelope, AtomId};
use corpus_engine::enrichment::{GovernanceOp, GovernanceOpKind, GovernanceOplog};

use super::{atlas_dir, now_unix};

pub fn cmd_seed(args: &[String]) -> i32 {
    let Some(corpus_id) = args.first() else {
        eprintln!("error: usage: sovereign govern seed <corpus-id>");
        return 2;
    };
    let dir = atlas_dir(corpus_id);
    let atoms = match read_atlas_atoms(&dir) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: reading atoms.json for `{corpus_id}`: {e}");
            eprintln!("  run `svrn enrich build {corpus_id} --full` first.");
            return 1;
        }
    };
    let oplog = GovernanceOplog::new(&dir);
    // Idempotency: never re-assert a rule the oplog already governs.
    let already: HashSet<AtomId> = match oplog.read_all() {
        Ok(ops) => ops
            .into_iter()
            .filter_map(|op| match op.kind {
                GovernanceOpKind::AssertRule { rule, .. } => Some(rule),
                _ => None,
            })
            .collect(),
        Err(e) => {
            eprintln!("error: reading governance oplog: {e}");
            return 1;
        }
    };
    let ts = now_unix();
    let mut seeded = 0usize;
    for env in &atoms.atoms {
        if let AtomEnvelope::Claim(c) = env {
            if already.contains(&c.id) {
                continue;
            }
            let op = GovernanceOp::new(
                GovernanceOpKind::AssertRule {
                    rule: c.id.clone(),
                    source_doc: None,
                },
                ts,
                "seed",
            );
            if let Err(e) = oplog.append(&op) {
                eprintln!("error: appending AssertRule for {}: {e}", c.id.as_str());
                return 1;
            }
            seeded += 1;
        }
    }
    println!(
        "✓ govern seed {corpus_id}: asserted {seeded} new rule(s); {} already governed.",
        already.len()
    );
    if seeded == 0 && already.is_empty() {
        println!("  (no Claim atoms found — is `{corpus_id}` an atlas/governance corpus?)");
    }
    0
}
