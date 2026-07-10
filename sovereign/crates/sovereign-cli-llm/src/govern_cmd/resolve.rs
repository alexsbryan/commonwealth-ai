// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn govern resolve` — adjudicate a tension by superseding one
//! of its two rules with the other, then marking the tension resolved.
//!
//! Writes a `Supersede` op (new = the kept rule, old = the other
//! endpoint) followed by a `ResolveTension` op linked to it. From then
//! on `derive_active` reports the superseded rule out of current law, and
//! `govern ask`'s active-set filter drops its evidence chunks — so the
//! answer can only be grounded in the rule that won (FR-9 RL-3).
//!
//! v1 supersedes via an *existing* rule (`--keep`); authoring a brand-new
//! superseding rule (`--draft`) is deferred — see the `--draft` arm.

use corpus_engine::enrichment::{GovernanceOp, GovernanceOpKind, GovernanceOplog};

use super::{atlas_dir, load_view, now_unix};

struct Parsed {
    corpus_id: String,
    tension_id: String,
    keep: Option<String>,
    draft: Option<String>,
    rationale: String,
}

fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut corpus_id = None;
    let mut tension_id = None;
    let mut keep = None;
    let mut draft = None;
    let mut rationale = String::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--keep" => {
                keep = Some(args.get(i + 1).ok_or("--keep needs a rule-id")?.clone());
                i += 2;
            }
            "--draft" => {
                draft = Some(args.get(i + 1).ok_or("--draft needs rule text")?.clone());
                i += 2;
            }
            "--rationale" => {
                rationale = args.get(i + 1).ok_or("--rationale needs text")?.clone();
                i += 2;
            }
            other if other.starts_with("--") => return Err(format!("unknown flag {other}")),
            other => {
                if corpus_id.is_none() {
                    corpus_id = Some(other.to_string());
                } else if tension_id.is_none() {
                    tension_id = Some(other.to_string());
                } else {
                    return Err(format!("unexpected argument {other}"));
                }
                i += 1;
            }
        }
    }
    Ok(Parsed {
        corpus_id: corpus_id.ok_or("missing <corpus-id>")?,
        tension_id: tension_id.ok_or("missing <tension-id>")?,
        keep,
        draft,
        rationale,
    })
}

pub fn cmd_resolve(args: &[String]) -> i32 {
    let parsed = match parse(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!("usage: sovereign govern resolve <corpus-id> <tension-id> --keep <rule-id> [--rationale <s>]");
            return 2;
        }
    };

    if parsed.draft.is_some() {
        eprintln!("error: `--draft` (author a brand-new superseding rule) is not wired in v1.");
        eprintln!("  A drafted rule isn't retrievable by `govern ask` without atom→retrieval");
        eprintln!("  injection. For now use `--keep <rule-id>` to designate which existing rule");
        eprintln!("  wins, or add the new rule to the source document and re-enrich so it becomes");
        eprintln!("  a citable chunk, then resolve against it.");
        return 2;
    }
    let Some(keep) = parsed.keep else {
        eprintln!("error: resolve needs `--keep <rule-id>` (which tensioned rule wins).");
        eprintln!(
            "  run `svrn govern tensions {}` to see the rule ids.",
            parsed.corpus_id
        );
        return 2;
    };

    let view = match load_view(&parsed.corpus_id) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let Some(tension) = view
        .tensions
        .iter()
        .find(|t| t.id.as_str() == parsed.tension_id)
    else {
        eprintln!(
            "error: no tension `{}` in `{}` — run `svrn govern tensions {}` to list them.",
            parsed.tension_id, parsed.corpus_id, parsed.corpus_id
        );
        return 1;
    };

    // `--keep` must name one of the two tensioned rules; the other is
    // the one we supersede.
    let (keep_id, old_id, keep_text, old_text) = if keep == tension.rule_a.as_str() {
        (
            &tension.rule_a,
            &tension.rule_b,
            &tension.text_a,
            &tension.text_b,
        )
    } else if keep == tension.rule_b.as_str() {
        (
            &tension.rule_b,
            &tension.rule_a,
            &tension.text_b,
            &tension.text_a,
        )
    } else {
        eprintln!(
            "error: --keep `{keep}` is not a rule in tension `{}` (its rules are {} and {}).",
            parsed.tension_id,
            tension.rule_a.as_str(),
            tension.rule_b.as_str()
        );
        return 2;
    };

    let oplog = GovernanceOplog::new(atlas_dir(&parsed.corpus_id));
    let ts = now_unix();
    // The Supersede is the substance; ResolveTension records that this
    // tension was adjudicated *via* that Supersede (so a later Revert of
    // the bundle is atomic — it names both).
    let supersede = GovernanceOp::new(
        GovernanceOpKind::Supersede {
            new_rule: keep_id.clone(),
            old_rules: vec![old_id.clone()],
            rationale: parsed.rationale.clone(),
        },
        ts,
        "human:cli",
    );
    let resolve = GovernanceOp::new(
        GovernanceOpKind::ResolveTension {
            tension: tension.id.clone(),
            via: supersede.id.clone(),
            // Record the endpoint rule pair so this adjudication survives
            // an atlas rebuild that renumbers the tension's edge id.
            endpoints: Some((keep_id.clone(), old_id.clone())),
            rationale: parsed.rationale.clone(),
        },
        ts,
        "human:cli",
    );
    if let Err(e) = oplog.append(&supersede) {
        eprintln!("error: appending Supersede: {e}");
        return 1;
    }
    if let Err(e) = oplog.append(&resolve) {
        eprintln!("error: appending ResolveTension: {e}");
        return 1;
    }

    println!(
        "✓ resolved tension {} in {}:",
        parsed.tension_id, parsed.corpus_id
    );
    println!("  KEEP [{}]: {}", keep_id.as_str(), keep_text);
    println!(
        "  DROP [{}]: {} (superseded — now out of current law)",
        old_id.as_str(),
        old_text
    );
    if !parsed.rationale.is_empty() {
        println!("  rationale: {}", parsed.rationale);
    }
    println!(
        "  (re-run `svrn govern tensions {}` to confirm it cleared.)",
        parsed.corpus_id
    );
    0
}
