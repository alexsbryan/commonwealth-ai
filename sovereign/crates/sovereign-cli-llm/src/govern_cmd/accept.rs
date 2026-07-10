// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn govern accept` — record a tension as known-and-tolerated.
//!
//! Some tensions are genuine but intentional (a general rule with a
//! deliberate exception). `accept` appends an `AcceptTension` op so the
//! tension stops surfacing as open, while the history of *why* it was
//! accepted is preserved on the op. Both rules stay in current law.

use corpus_engine::enrichment::{GovernanceOp, GovernanceOpKind, GovernanceOplog};

use super::{atlas_dir, load_view, now_unix};

pub fn cmd_accept(args: &[String]) -> i32 {
    let mut corpus_id = None;
    let mut tension_id = None;
    let mut rationale = String::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--rationale" => {
                let Some(v) = args.get(i + 1) else {
                    eprintln!("error: --rationale needs text");
                    return 2;
                };
                rationale = v.clone();
                i += 2;
            }
            other if other.starts_with("--") => {
                eprintln!("error: unknown flag {other}");
                return 2;
            }
            other => {
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                } else if tension_id.is_none() {
                    tension_id = Some(other.to_string());
                } else {
                    eprintln!("error: unexpected argument {other}");
                    return 2;
                }
                i += 1;
            }
        }
    }
    let (Some(corpus_id), Some(tension_id)) = (corpus_id, tension_id) else {
        eprintln!("error: usage: sovereign govern accept <corpus-id> <tension-id> --rationale <s>");
        return 2;
    };
    if rationale.trim().is_empty() {
        eprintln!(
            "error: accept requires `--rationale <s>` — record *why* the tension is tolerated."
        );
        return 2;
    }

    let view = match load_view(&corpus_id) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let Some(tension) = view.tensions.iter().find(|t| t.id.as_str() == tension_id) else {
        eprintln!(
            "error: no tension `{tension_id}` in `{corpus_id}` — run `svrn govern tensions {corpus_id}` to list them."
        );
        return 1;
    };

    let oplog = GovernanceOplog::new(atlas_dir(&corpus_id));
    let op = GovernanceOp::new(
        GovernanceOpKind::AcceptTension {
            tension: tension.id.clone(),
            rationale: rationale.clone(),
            // Record the endpoint rule pair so this adjudication survives
            // an atlas rebuild that renumbers the tension's edge id.
            endpoints: Some((tension.rule_a.clone(), tension.rule_b.clone())),
        },
        now_unix(),
        "human:cli",
    );
    if let Err(e) = oplog.append(&op) {
        eprintln!("error: appending AcceptTension: {e}");
        return 1;
    }
    println!("✓ accepted tension {tension_id} in {corpus_id} (both rules remain in force).");
    println!("  rationale: {rationale}");
    0
}
