// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svrn ring checkpoint --verify <file>` — the four steps of
//! `docs/THE_LINK.md` §"The checkpoint, specified", run cold over a frozen
//! copy of a ring's record.
//!
//! Nothing in the document is trusted: the ops are re-parsed as ordinary
//! journal lines (step 1), every admission rule re-runs over them (step 2),
//! the stated digest is compared against `commonwealth_rail::digest`
//! recomputed over those very ops (step 3 — which IS the completeness claim:
//! the computed marks are contiguous from each actor's sealed floor by
//! construction, `commonwealth-rail-core/src/sync.rs`), and only then are the
//! marks, the admitted-act count and the remaining gaps rendered (step 4).
//! A document served by a hostile host is therefore worth exactly what its
//! signatures are worth.
//!
//! Refusal policy, from the bar (`quality/campaigns/the-link.toml`
//! `tl-checkpoint-verifies`): a gap that means an inauthentic or forked act —
//! bad signature, unknown signer, tampered id, sequence fork — REFUSES the
//! document; a gap that means incompleteness — a sequence hole, a correction
//! naming an op the room missed mid-cut — is named on screen but does not
//! refuse, because a checkpoint frozen mid-cut is supposed to be able to
//! carry one.

use commonwealth_rail::{admit, digest, Ed25519Verifier, Op, RailGap, Roster, SignedOp};

const STEP_PARSE: &str = "step 1 (parse)";
const STEP_ADMIT: &str = "step 2 (admit)";
const STEP_DIGEST: &str = "step 3 (digest)";

/// The four steps over one parsed document. `Ok(rendered)` is the exit-0
/// block: the marks, the admitted-act count, the gaps. `Err(sentence)` is
/// exit 1 with ONE sentence naming the failing step and, where one exists,
/// the actor.
pub(crate) fn verify_document(
    doc: &serde_json::Value,
    roster_override: Option<Roster>,
) -> Result<String, String> {
    // ── step 1: parse, verbatim ─────────────────────────────────
    let ns = doc
        .get("ns")
        .and_then(|v| v.as_str())
        .ok_or_else(|| refusal(STEP_PARSE, "the document does not name a `ns`"))?;
    let lines = doc
        .get("ops")
        .and_then(|v| v.as_array())
        .ok_or_else(|| refusal(STEP_PARSE, "the document carries no `ops` array"))?;
    let mut ops = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        let line = line.as_str().ok_or_else(|| {
            refusal(
                STEP_PARSE,
                &format!("ops line {} is not a journal line", i + 1),
            )
        })?;
        let op: Op<SignedOp> = serde_json::from_str(line).map_err(|e| {
            refusal(
                STEP_PARSE,
                &format!(
                    "ops line {} does not parse as an ordinary journal line: {e}",
                    i + 1
                ),
            )
        })?;
        ops.push(op);
    }
    let roster = match roster_override {
        Some(r) => r,
        None => serde_json::from_value::<Roster>(
            doc.get("roster")
                .cloned()
                .ok_or_else(|| refusal(STEP_PARSE, "the document carries no embedded `roster`"))?,
        )
        .map_err(|e| {
            refusal(
                STEP_PARSE,
                &format!("the embedded roster does not parse: {e}"),
            )
        })?,
    };
    let stated: commonwealth_rail::Digest = serde_json::from_value(
        doc.get("digest")
            .cloned()
            .ok_or_else(|| refusal(STEP_PARSE, "the document carries no `digest`"))?,
    )
    .map_err(|e| {
        refusal(
            STEP_PARSE,
            &format!("the stated digest does not parse: {e}"),
        )
    })?;

    // ── step 2: admit — signature, roster, sequence, fork, void ─
    let admission = admit(&ops, &[], &roster, ns, &Ed25519Verifier);
    for gap in &admission.gaps {
        if let Some(why) = authenticity_refusal(gap, &ops) {
            return Err(refusal(STEP_ADMIT, &why));
        }
    }

    // ── step 3: digest equality — the completeness claim ────────
    let computed = digest(&ops);
    let mut actors: std::collections::BTreeSet<&str> =
        computed.keys().map(String::as_str).collect();
    actors.extend(stated.keys().map(String::as_str));
    for actor in actors {
        let c = computed.get(actor);
        let s = stated.get(actor);
        if c != s {
            let said = |m: Option<&u64>| {
                m.map(|n| n.to_string())
                    .unwrap_or_else(|| "no mark".to_string())
            };
            return Err(refusal(
                STEP_DIGEST,
                &format!(
                    "actor {actor}'s marks disagree — the document states {}, its own ops compute {}",
                    said(s),
                    said(c)
                ),
            ));
        }
    }

    // ── step 4: render — marks, admitted-act count, gaps ────────
    let mut out = format!("{ns}: verified — {} admitted act(s)\n", admission.ops.len());
    for (actor, mark) in &computed {
        out.push_str(&format!("  mark {actor}: {mark}\n"));
    }
    if admission.gaps.is_empty() {
        out.push_str("no gaps\n");
    } else {
        out.push_str(&format!("{} gap(s):\n", admission.gaps.len()));
        for gap in &admission.gaps {
            out.push_str(&format!("  - {gap}\n"));
        }
    }
    Ok(out)
}

/// Exit 1's one sentence.
fn refusal(step: &str, why: &str) -> String {
    format!("checkpoint verify: {step} refused: {why}")
}

/// Whether this gap means the document carries an inauthentic or forked act
/// (`Some`, the refusal's why) or merely an incomplete one (`None`, rendered).
fn authenticity_refusal(gap: &RailGap, ops: &[Op<SignedOp>]) -> Option<String> {
    match gap {
        RailGap::BadSignature { actor, .. } => {
            Some(format!("an op by actor {actor} carries a signature that does not verify"))
        }
        RailGap::UnknownSigner { actor, .. } => Some(format!(
            "an op signed by actor {actor} is not covered by the roster — nobody in it claims that key"
        )),
        RailGap::TamperedId { claimed, .. } => {
            let actor = ops.iter().find(|o| &o.id == claimed).map(|o| o.actor.clone());
            Some(match actor {
                Some(a) => format!("an op by actor {a} carries an id its content does not derive"),
                None => "a journal line carries an id its content does not derive".to_string(),
            })
        }
        RailGap::SequenceFork { actor, seq, .. } => Some(format!(
            "actor {actor} used one sequence number twice (#{seq}) — the document forks"
        )),
        _ => None,
    }
}

/// `svrn ring checkpoint --verify <file> [--roster <file>]` — verify a frozen
/// copy cold. Exit 0 prints the marks; exit 1 is one refusal sentence; exit 2
/// is a usage error.
pub(crate) fn run_verify(args: &[String]) -> i32 {
    let file = match args.first().filter(|a| !a.starts_with("--")) {
        Some(f) => f,
        None => {
            eprintln!(
                "ring checkpoint --verify: which file? `svrn ring checkpoint --verify <file> [--roster <file>]`"
            );
            return 2;
        }
    };
    let roster_path = match args.iter().position(|a| a == "--roster") {
        Some(i) => match args.get(i + 1) {
            Some(path) if !path.starts_with("--") => Some(path),
            _ => {
                eprintln!("ring checkpoint --verify: --roster needs a file path");
                return 2;
            }
        },
        None => None,
    };
    let roster_override = match roster_path {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(bytes) => match serde_json::from_str::<Roster>(&bytes) {
                Ok(r) => Some(r),
                Err(e) => {
                    eprintln!(
                        "{}",
                        refusal(
                            STEP_PARSE,
                            &format!("{path} does not parse as a ring roster: {e}")
                        )
                    );
                    return 1;
                }
            },
            Err(e) => {
                eprintln!(
                    "{}",
                    refusal(STEP_PARSE, &format!("cannot read {path}: {e}"))
                );
                return 1;
            }
        },
        None => None,
    };
    let bytes = match std::fs::read_to_string(file) {
        Ok(b) => b,
        Err(e) => {
            eprintln!(
                "{}",
                refusal(STEP_PARSE, &format!("cannot read {file}: {e}"))
            );
            return 1;
        }
    };
    let doc: serde_json::Value = match serde_json::from_str(&bytes) {
        Ok(d) => d,
        Err(e) => {
            eprintln!(
                "{}",
                refusal(STEP_PARSE, &format!("{file} does not parse as JSON: {e}"))
            );
            return 1;
        }
    };
    match verify_document(&doc, roster_override) {
        Ok(rendered) => {
            print!("{rendered}");
            0
        }
        Err(sentence) => {
            eprintln!("{sentence}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Path, State};
    use commonwealth_rail::{body_json, sign_ring_op, Payload, Person, RailAct, RingSigner};
    use sovereign_daemon::routes_internal::ring_checkpoint;
    use sovereign_daemon::state::{fabric::FabricSeed, test_app_state_with_seed};

    const NS: &str = "house-expenses";

    /// The one key the test ring signs with; its actor hex is what the roster
    /// and every refusal must name.
    fn key() -> commonwealth_rail::SigningKey {
        commonwealth_rail::SigningKey::from_bytes(&[7u8; 32])
    }

    fn roster_for_this_key() -> Roster {
        let mut members = std::collections::BTreeMap::new();
        members.insert(Person::from("Ada"), vec![key().actor()]);
        Roster::new(members)
    }

    /// A rail on a temp dir with `ns` rostered to `key`'s actor, holding
    /// `n` record acts — a live namespace the export route can freeze.
    async fn rail_with_acts(root: &std::path::Path, n: u64) -> commonwealth_rail::RingRail {
        let signer = std::sync::Arc::new(key());
        let rail = commonwealth_rail::RingRail::new(root, signer);
        rail.journal(NS)
            .unwrap()
            .set_roster(&roster_for_this_key())
            .unwrap();
        for i in 0..n {
            let journal = rail.journal(NS).unwrap();
            let roster = rail.roster(&journal).await.unwrap();
            journal
                .append(
                    RailAct::Record {
                        payload: Payload::new(serde_json::json!({
                            "kind": "expense", "amount": i + 1,
                        }))
                        .unwrap(),
                    },
                    rail.signer(),
                    &roster,
                    None,
                )
                .unwrap();
        }
        rail
    }

    /// The real export route's document for a live namespace — verify reads
    /// exactly what a node puts on the wire, never a hand-built stand-in.
    async fn export(rail: commonwealth_rail::RingRail) -> serde_json::Value {
        let state = test_app_state_with_seed(FabricSeed {
            ring_rail: Some(std::sync::Arc::new(rail)),
            ..Default::default()
        });
        let response = ring_checkpoint(State(state), Path(NS.to_string())).await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn an_export_of_a_live_namespace_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let doc = export(rail_with_acts(dir.path(), 3).await).await;

        let rendered = verify_document(&doc, None).expect("an honest export verifies");
        let actor = key().actor();
        assert!(
            rendered.contains(&actor),
            "the marks name the actor: {rendered}"
        );
        assert!(rendered.contains("mark"), "{rendered}");
        assert!(rendered.contains("3 admitted act(s)"), "{rendered}");
        assert!(rendered.contains("no gaps"), "{rendered}");
    }

    #[tokio::test]
    async fn a_flipped_signature_byte_refuses_naming_the_signature_step() {
        let dir = tempfile::tempdir().unwrap();
        let actor = key().actor();
        let mut doc = export(rail_with_acts(dir.path(), 2).await).await;

        // Flip one hex char of the first op's signature — the line still
        // parses, the id still derives, only the signature breaks.
        let mut op: serde_json::Value =
            serde_json::from_str(doc["ops"][0].as_str().unwrap()).unwrap();
        let sig = op["sig"].as_str().unwrap().to_string();
        let flipped = if sig.starts_with('0') { "1" } else { "0" };
        op["sig"] = serde_json::json!(format!("{flipped}{}", &sig[1..]));
        doc["ops"][0] = serde_json::json!(serde_json::to_string(&op).unwrap());

        let err = verify_document(&doc, None).unwrap_err();
        assert!(err.contains(STEP_ADMIT), "{err}");
        assert!(err.contains("signature"), "{err}");
        assert!(err.contains(&actor), "the actor is named: {err}");
    }

    #[tokio::test]
    async fn the_last_ops_line_removed_refuses_naming_the_disagreeing_actor() {
        let dir = tempfile::tempdir().unwrap();
        let actor = key().actor();
        let mut doc = export(rail_with_acts(dir.path(), 3).await).await;

        // Three acts, seqs 0..=2: the stated mark is 2, and without the last
        // line the ops compute 1 — the completeness claim fails by name.
        let last = doc["ops"].as_array().unwrap().len() - 1;
        doc["ops"].as_array_mut().unwrap().remove(last);

        let err = verify_document(&doc, None).unwrap_err();
        assert!(err.contains(STEP_DIGEST), "{err}");
        assert!(
            err.contains(&actor),
            "the disagreeing actor is named: {err}"
        );
        assert!(err.contains("states 2"), "{err}");
        assert!(err.contains("compute 1"), "{err}");
    }

    #[tokio::test]
    async fn a_planted_same_seq_pair_refuses_as_a_fork() {
        let dir = tempfile::tempdir().unwrap();
        let actor = key().actor();
        let mut doc = export(rail_with_acts(dir.path(), 3).await).await;

        // Plant a second, different op at an occupied seq, signed by the
        // same actor: valid signature, valid roster, different body — the
        // equivocation a fork is. Only the fork exclusion can refuse this:
        // the digest is set-semantics, so the planted pair still computes
        // to the stated marks.
        let existing: Op<SignedOp> = serde_json::from_str(doc["ops"][1].as_str().unwrap()).unwrap();
        let act = RailAct::Record {
            payload: Payload::new(serde_json::json!({
                "kind": "expense", "amount": 999,
            }))
            .unwrap(),
        };
        let body = body_json(&act, None);
        let sig = sign_ring_op(&key(), NS, existing.ts_unix, existing.kind.seq, &body);
        let planted = Op::new(
            SignedOp {
                seq: existing.kind.seq,
                sig,
                act,
                on_behalf_of: None,
            },
            existing.ts_unix,
            actor.clone(),
        );
        doc["ops"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!(serde_json::to_string(&planted).unwrap()));

        let err = verify_document(&doc, None).unwrap_err();
        assert!(err.contains(STEP_ADMIT), "{err}");
        assert!(err.contains("fork"), "{err}");
        assert!(err.contains(&actor), "the actor is named: {err}");
    }

    #[tokio::test]
    async fn a_second_node_s_roster_substitutes_and_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let doc = export(rail_with_acts(dir.path(), 2).await).await;

        // A second node's own roster — same members, built independently of
        // the document — substitutes and verifies.
        let substituted =
            verify_document(&doc, Some(roster_for_this_key())).expect("the substitute verifies");
        assert!(substituted.contains("verified"), "{substituted}");

        // And it is the substitute that ran: a roster missing the actor
        // refuses where the embedded one would have passed.
        let err = verify_document(&doc, Some(Roster::new(std::collections::BTreeMap::new())))
            .unwrap_err();
        assert!(err.contains(STEP_ADMIT), "{err}");
        assert!(err.contains(&key().actor()), "{err}");
    }

    /// The exit contract at the verb level: a frozen file exits 0 printing
    /// the marks; a forged copy of the same file exits 1 with one sentence.
    #[tokio::test]
    async fn verify_exits_zero_on_a_frozen_file_and_one_on_a_forged_one() {
        let dir = tempfile::tempdir().unwrap();
        let doc = export(rail_with_acts(dir.path(), 1).await).await;
        let file = dir.path().join("checkpoint.json");
        std::fs::write(&file, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
        assert_eq!(run_verify(&[file.display().to_string()]), 0);

        // Forge: strip the roster field — step 1 has nothing to weigh the
        // signatures against and refuses rather than passing by default.
        let mut forged = doc.clone();
        forged.as_object_mut().unwrap().remove("roster");
        let forged_file = dir.path().join("forged.json");
        std::fs::write(&forged_file, serde_json::to_string(&forged).unwrap()).unwrap();
        assert_eq!(run_verify(&[forged_file.display().to_string()]), 1);
    }
}
