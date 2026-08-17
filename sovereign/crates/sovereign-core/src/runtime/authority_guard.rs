// SPDX-License-Identifier: AGPL-3.0-or-later
//! Answer-exit numeric-provenance guard for authoritative corpora
//! (order authority-guard-at-exit, 2026-08-17; FINANCIAL_CORPORA.md §6).
//!
//! # The defect this closes
//!
//! The §6 numeric audit was bolted to ONE of the answer-producing
//! handlers (`handlers/complex_task.rs`), so whether an answer over an
//! authoritative corpus was audited depended on which handler a
//! classifier picked. Measured 2026-08-17 (`runs/f2-instrument-arming`):
//! "why did Mac net sales increase in fiscal 2025?" routed
//! `DeepQuery`, ran no tool, ran no audit, and on the 2026-08-16 run
//! volunteered `$33,708 million` / `$29,984 million` / `12%` from
//! filing prose. This module binds the guarantee to the ANSWER — any
//! covered exit seam, any handler — instead of to a route.
//!
//! # Arming — one decider, corpus granularity
//!
//! A turn is ARMED when the corpora its answer actually drew on (the
//! retrieved-chunk pool, [`crate::runtime::epistemic::pool_corpora`])
//! intersect the corpora some registered tool declares authority over
//! ([`crate::registry::ToolRegistry::authority_domains`]). That is the
//! SAME declaration index the router's question-level pre-check
//! (`router.rs` `authority_claims`) reads, at corpus granularity:
//! question-level matching is a ROUTING decision and deliberately
//! declines explanation-shaped questions (see the `"why"` carve-out in
//! `sec_facts::store_claims`, 2026-08-15) — exactly the questions this
//! guard exists to cover. THE DECIDER IS THE `[authority]` DECLARATION
//! INDEX; question-matching is routing narrowing that a provenance
//! guard must not inherit (seat ruling 2026-08-17, note 23bc5b91 — the
//! order's original "arm off `authority_claims(message)`" instruction
//! predates this ruling and cannot arm on its own negative control). Corpora that declare nothing produce an empty
//! intersection and the guard is structurally inert: no audit, no
//! metadata, no behaviour change (the blast-radius contract).
//!
//! Evidence-pool arming (NOT the conversation's enabled-corpora list —
//! seat ruling 2026-08-17): a turn that drew only on a parcel corpus
//! while an SEC corpus sat enabled-but-unused must not audit parcel
//! figures against the SEC store. The corollary: a turn that drew on
//! NOTHING (zero chunks, naked mode) is not "over" any corpus and does
//! not arm — retrieval-miss honesty is owned by the PR5 diversion and
//! the grounding gate, not here.
//!
//! # What the guard allows when it fires
//!
//! Bare-scope audit (`numeric_audit::uncited_numerics_including_bare`;
//! the corpus's `[authority]` declaration is the opt-in — §6.3 by
//! extension) with the allowed set the union of:
//! - figures a deterministic tool emitted this turn (`cited`,
//!   `raw_values`, declared `allowed_tokens`) — a computed value is
//!   provenanced by its computation (note 36ae0d12);
//! - every numeric token inside a VERIFIED verbatim quotation
//!   (`quote_verification::VerificationResult::verified_spans`) — the
//!   filing's own sentence stays legal (§6.2(5), the F4 exemption);
//!   demoted and sub-floor spans are never exempt, so quote-wrapping a
//!   fabricated figure earns nothing;
//! - every numeric token of the USER'S question — a numeral the user
//!   supplied is not a numeral the model originated, so echoing
//!   "in fiscal 2025" back is not fabrication (and this is not the
//!   §18.1 echo smell: that smell is a guard trusting a field its
//!   SUBJECT controls, and the subject here is the model, not the
//!   user).
//!
//! Anything else numeric blocks the answer: the prose is withheld and
//! replaced by a §6.2(4)-shaped notice naming every withheld numeral,
//! carrying the verified quotations (the only source-attributable
//! rendering a no-tool turn has), and naming the typed store that IS
//! authoritative.
//!
//! # Glassbox
//!
//! Every ARMED decision traces under target `authority_guard` — corpus,
//! tool, handler, verdict, and on a block every withheld numeral by
//! name. Unarmed turns that drew on ≥1 corpus trace at debug with the
//! reason (empty intersection / handler-delegated), because an absent
//! event is what kept this defect invisible for the life of the
//! feature.
//!
//! # Coverage
//!
//! The seam-by-seam coverage table is `guard_story` below — an
//! exhaustive match over [`Intent`], pinned by test, so adding an
//! intent FAILS COMPILATION until its guard story is decided. The
//! streaming-surface exits that are not intent-keyed are dispositioned
//! in `guard_story`'s doc table.

use crate::registry::ToolRegistry;
use crate::types::AuthorityClaim;

/// Non-empty proof that this turn's evidence pool intersects a declared
/// authority domain. Constructed only by [`armed_for_evidence`].
#[derive(Debug, Clone)]
pub(crate) struct ArmedAuthority {
    /// The intersecting claims, `(tool_id, corpus_id)`-sorted; the
    /// first is the one named in logs and block text (same tie rule as
    /// the router pre-check).
    pub claims: Vec<AuthorityClaim>,
}

impl ArmedAuthority {
    fn first(&self) -> &AuthorityClaim {
        // Invariant: `armed_for_evidence` never constructs an empty set.
        &self.claims[0]
    }
}

/// Arm iff `evidence_corpora` (the corpora the answer actually drew on)
/// intersects the registry's declared authority domains. `None` is the
/// structural no-op: nothing declared, or nothing drawn on.
pub(crate) fn armed_for_evidence(
    tools: &ToolRegistry,
    evidence_corpora: &[String],
    handler: &'static str,
) -> Option<ArmedAuthority> {
    if evidence_corpora.is_empty() {
        return None;
    }
    let claims: Vec<AuthorityClaim> = tools
        .authority_domains()
        .into_iter()
        .filter(|d| evidence_corpora.iter().any(|c| c == &d.corpus_id))
        .collect();
    if claims.is_empty() {
        tracing::debug!(
            target: "authority_guard",
            handler,
            evidence_corpora = ?evidence_corpora,
            "authority_guard: not armed — no evidence corpus declares authority"
        );
        return None;
    }
    tracing::info!(
        target: "authority_guard",
        handler,
        corpus = %claims[0].corpus_id,
        tool = %claims[0].tool_id,
        claimants = claims.len(),
        "authority_guard: ARMED — answer figures must trace or the answer refuses"
    );
    Some(ArmedAuthority { claims })
}

/// Scope-level arming for seams that must decide BEFORE retrieval runs
/// (the team-pipeline branch gates, which would otherwise route an
/// armed turn onto a live token stream no exit guard can hold).
/// Conservative by construction: `scoped_corpora` is the turn's
/// retrievable set (`context.installed_corpora`), a superset of what
/// the answer will draw on, so this can only steer MORE turns onto the
/// covered path, never fewer.
pub(crate) fn scope_is_armed(tools: &ToolRegistry, scoped_corpora: &[String]) -> bool {
    !scoped_corpora.is_empty()
        && tools
            .authority_domains()
            .iter()
            .any(|d| scoped_corpora.iter().any(|c| c == &d.corpus_id))
}

/// Everything the audit may treat as "not model-originated" at this
/// exit, beyond what the answer text itself carries.
pub(crate) struct GuardBasis<'a> {
    /// The user's question — its numerals are user-supplied.
    pub question: &'a str,
    /// Inner text of quotations that PASSED verbatim verification
    /// against this turn's evidence (`VerificationResult::verified_spans`).
    pub verified_spans: &'a [String],
    /// Figures a deterministic tool emitted this turn (empty on the
    /// no-tool paths this guard exists for; populated if a seam with a
    /// tool transcript ever adopts the exit guard).
    pub cited: &'a [String],
    pub raw_values: &'a [f64],
    pub allowed_tokens: &'a [String],
}

/// The exit verdict. `Released` answers pass through byte-identical;
/// `Blocked` answers are REPLACED by `replacement` (prose withheld).
#[derive(Debug)]
pub(crate) enum GuardVerdict {
    Released,
    Blocked {
        violations: Vec<String>,
        replacement: String,
    },
}

impl GuardVerdict {
    /// Glassbox record for message metadata — attached ONLY on armed
    /// turns (unarmed turns stay byte-identical to pre-guard bytes).
    pub(crate) fn metadata(
        &self,
        armed: &ArmedAuthority,
        handler: &'static str,
    ) -> serde_json::Value {
        let first = armed.first();
        match self {
            GuardVerdict::Released => serde_json::json!({
                "armed": true,
                "handler": handler,
                "corpus_id": first.corpus_id,
                "tool_id": first.tool_id,
                "action": "released",
            }),
            GuardVerdict::Blocked { violations, .. } => serde_json::json!({
                "armed": true,
                "handler": handler,
                "corpus_id": first.corpus_id,
                "tool_id": first.tool_id,
                "action": "blocked_6_2_4_provenance_guard",
                "withheld_numerals": violations,
            }),
        }
    }
}

/// Audit `answer` at the exit of an ARMED turn. Pure: no I/O, no
/// inference — the audit core is `numeric_audit`'s (the ONE
/// implementation, §10.6); this function only assembles the allowed
/// basis and renders the block notice.
pub(crate) fn guard_answer(
    armed: &ArmedAuthority,
    answer: &str,
    basis: &GuardBasis<'_>,
    handler: &'static str,
) -> GuardVerdict {
    // Allowed strings beyond the tool's own declaration: verified
    // verbatim quotations and the user's question. `numeric_tokens`
    // is the same extractor the audit runs over the answer, so
    // "allowed" is by construction "stated by the source or the user".
    let mut allowed: Vec<String> = basis.allowed_tokens.to_vec();
    for span in basis.verified_spans {
        allowed.extend(crate::runtime::numeric_audit::numeric_tokens(span));
    }
    allowed.extend(crate::runtime::numeric_audit::numeric_tokens(
        basis.question,
    ));

    // Bare scope, unconditionally: the corpus's [authority] declaration
    // is the opt-in (§6.3 by extension). Empty cited/raw does NOT
    // early-return at bare scope — on a turn that consulted no typed
    // store, an empty tool basis means every unattributable numeral
    // flags, which is the correct outcome (numeric_audit.rs:74-78).
    let violations = crate::runtime::numeric_audit::uncited_numerics_including_bare(
        answer,
        basis.cited,
        basis.raw_values,
        &allowed,
    );

    let first = armed.first();
    if violations.is_empty() {
        tracing::info!(
            target: "authority_guard",
            handler,
            corpus = %first.corpus_id,
            tool = %first.tool_id,
            verified_quotes = basis.verified_spans.len(),
            "authority_guard: released — every figure traces (tool, verified quote, or the question)"
        );
        return GuardVerdict::Released;
    }

    tracing::warn!(
        target: "authority_guard",
        handler,
        corpus = %first.corpus_id,
        tool = %first.tool_id,
        violations = ?violations,
        "authority_guard: BLOCKED — model-originated figure(s) over an authoritative corpus"
    );

    let mut replacement = format!(
        "**Provenance guard** — this answer was withheld because {} figure(s) in it \
         did not trace to `{}`'s typed store, to a verified quotation from the \
         source, or to the question itself: {}.\n\n",
        violations.len(),
        first.corpus_id,
        violations.join(", ")
    );
    if basis.verified_spans.is_empty() {
        replacement.push_str(
            "No verbatim source passage was verified for this answer, so none of its \
             figures can be attributed.\n\n",
        );
    } else {
        replacement.push_str("The source's own verified words:\n\n");
        for span in basis.verified_spans {
            replacement.push_str(&format!("> \"{span}\"\n\n"));
        }
    }
    replacement.push_str(&format!(
        "`{}` is declared authoritative for figures over this corpus, and this \
         answer consulted no typed fact. Ask for a specific figure — a concept and \
         period, e.g. \"revenue for FY2025\" — for a cited, deterministic answer, or \
         ask for the filing's own wording.",
        first.tool_id
    ));

    GuardVerdict::Blocked {
        violations,
        replacement,
    }
}

/// How each dispatch route relates to the exit guard — the coverage
/// table the order requires, as code, pinned exhaustive so a new
/// [`Intent`] cannot ship without a decided story.
///
/// Streaming exits that are not intent-keyed (file:line at HEAD of this
/// change, `runtime/streaming.rs` unless noted):
///
/// | Exit | Disposition |
/// |---|---|
/// | oversize / degenerate rejections (`:3641`, `:3645`; `turn.rs:171`, `:179`) | NO-OP BY CONSTRUCTION — `Err`, static text, no answer |
/// | wellbeing crisis gate (`:3865`; `turn.rs:263`) | EXCLUDED BY DECISION (seat, 2026-08-17) — crisis-resource response on the Relational surface carries no financial figures |
/// | recipe/workflow-author workspace (`:4131`; `turn.rs:462`) | EXCLUDED BY DECISION (seat, 2026-08-17) — authoring loop over recipe drafts, not corpus Q&A |
/// | Ask move / retrieval-miss clarification (`:4148`, `:1246`; `turn.rs:477`) | NO-OP BY CONSTRUCTION — clarification placeholder, no synthesis over evidence |
/// | team pipeline (`:4238`; `turn.rs:652`) | COVERED STRUCTURALLY — an armed scope DIVERTS to the guarded legacy path, with a traced diversion; unarmed turns byte-identical |
/// | doc-attached prefix (`:4311` `Err`; `turn.rs:566`) and `document_session` (`turn.rs:600`) | EXCLUDED BY DECISION (seat, 2026-08-17) — the evidence universe is the user-attached document; the corpus's typed store is not the authority over it (reasoning also at the branch, `turn.rs:545`) |
/// | naked mode (`:907`) | NO-OP BY CONSTRUCTION — no retrieval exists on that path, so the evidence pool is empty and arming is structurally impossible |
///
/// Every row re-verified against the tree 2026-08-17 (second worker):
/// line numbers above are POST-change positions; the streaming rows'
/// original numbers predated this change's own insertions and were
/// corrected. The `_as` pinned-intent variant (`:871`) and the plain
/// wrapper (`:855`) both funnel into the one dispatcher these seams
/// live in, so caller-pinned intents are guarded at the same exits;
/// streaming `ComplexTask`/`Metalingual`/`Conation`/`Commissive` take
/// the single-chunk non-streaming fallback (`:4325`) into the same
/// guarded handlers.
pub(crate) enum GuardStory {
    /// The exit guard audits this route's answers when armed.
    Covered(&'static str),
    /// This route cannot draw on an authoritative corpus — the arming
    /// intersection is empty by construction. Say why.
    NoOpByConstruction(&'static str),
    /// This route could carry the problem; a named decision excluded it.
    /// No intent-keyed route uses this today — its rows are the
    /// non-intent-keyed exits in the doc table above (wellbeing,
    /// recipe-author, attached-doc), kept as a variant so a future
    /// intent-level exclusion must say why and who decided.
    #[allow(dead_code)]
    ExcludedByDecision(&'static str),
}

/// The story for every intent-keyed dispatch route, both surfaces.
pub(crate) fn guard_story(intent: &crate::types::Intent) -> GuardStory {
    use crate::types::Intent::*;
    match intent {
        // Streaming: stream_knowledge_query_turn (guard at the held-
        // answer seam, hold forced when armed). Non-streaming:
        // handlers/knowledge_query.rs (guard after its quote pass).
        KnowledgeQuery | ComparisonQuery => {
            GuardStory::Covered("kq stream seam + handlers/knowledge_query.rs")
        }
        // Streaming: stream_deep_query_turn — prose-explanation-mac's
        // route. Non-streaming: handle_simple (guard runs the shared
        // quote verifier itself on armed turns; that path had none).
        DeepQuery | SimpleQuery => {
            GuardStory::Covered("deep/simple stream seam + handlers/simple.rs")
        }
        // Source-anchored synthesis over corpus chunks; evidence never
        // leaves the handler, so the guard runs at its own exit.
        MetalingualQuery => GuardStory::Covered("handlers/metalingual.rs exit"),
        // The §6.2(4) audit already runs in-handler with the tool
        // transcript basis (cited/raw/allowed_tokens) — richer than
        // the exit basis. The exit seam DELEGATES: behaviour
        // byte-identical, story recorded here.
        ComplexTask => GuardStory::Covered("delegated to handlers/complex_task.rs §6.2(4)"),
        ConationQuery => GuardStory::NoOpByConstruction(
            "operates on the prior turn; does not retrieve (conation.rs:12)",
        ),
        CommissiveQuery => GuardStory::NoOpByConstruction(
            "persists commitments to the notes store; no corpus retrieval",
        ),
        ExpressiveQuery => GuardStory::NoOpByConstruction(
            "witness path over FTS memories/tensions, never corpus chunks",
        ),
        GenerativeQuery => GuardStory::NoOpByConstruction(
            "creative path: no retrieval, no grounding, by design (generative.rs:11)",
        ),
        CodeQuery => GuardStory::NoOpByConstruction(
            "retrieves only code corpora (is_code_corpus); an authority-declaring \
             corpus is knowledge-kind. If a code corpus ever declares authority, \
             this line is the decision to revisit",
        ),
        // Verified 2026-08-17: neither variant has its own branch on
        // either dispatch surface — turn.rs's `_ =>` arm sends both to
        // handle_simple, and the streaming dispatcher falls through to
        // stream_deep_query_turn (no SimpleAction/Continuation match in
        // either dispatcher; handle_simple never branches on the tool
        // payload). So they exit through the already-covered
        // simple/deep seams.
        SimpleAction { .. } | Continuation { .. } => {
            GuardStory::Covered("catch-all dispatch → simple/deep exit seams")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn armed() -> ArmedAuthority {
        ArmedAuthority {
            claims: vec![AuthorityClaim {
                tool_id: "sec_facts".into(),
                corpus_id: "sec-filings-aapl".into(),
                matched: "recipe [authority] declaration for entity 'Apple Inc.'".into(),
            }],
        }
    }

    fn no_tool_basis<'a>(question: &'a str, verified_spans: &'a [String]) -> GuardBasis<'a> {
        GuardBasis {
            question,
            verified_spans,
            cited: &[],
            raw_values: &[],
            allowed_tokens: &[],
        }
    }

    /// NEGATIVE CONTROL (order §controls 1) — the failing input, by
    /// name: the 2026-08-16 prose-explanation-mac figures, volunteered
    /// in model prose with no tool and no verified quote. Every one
    /// must be withheld and NAMED.
    #[test]
    fn model_originated_figures_over_an_armed_corpus_are_blocked() {
        let answer = "Mac net sales increased 12% to $33,708 million in fiscal 2025, \
                      up from $29,984 million.";
        let basis = no_tool_basis("why did Mac net sales increase in fiscal 2025?", &[]);
        match guard_answer(&armed(), answer, &basis, "test") {
            GuardVerdict::Blocked {
                violations,
                replacement,
            } => {
                assert!(violations.contains(&"12%".to_string()));
                assert!(violations.contains(&"$33,708 million".to_string()));
                assert!(violations.contains(&"$29,984 million".to_string()));
                // "fiscal 2025" is the user's own numeral — never a violation.
                assert!(!violations.iter().any(|v| v == "2025"));
                for v in &violations {
                    assert!(
                        replacement.contains(v.as_str()),
                        "every withheld numeral is named in the notice: {v}"
                    );
                }
                assert!(replacement.contains("sec-filings-aapl"));
                assert!(replacement.contains("sec_facts"));
            }
            v => panic!("must block, got {v:?}"),
        }
    }

    /// POSITIVE CONTROL (order §controls 2) — the F4 shape: the same
    /// explanation grounded in a verified verbatim quotation releases,
    /// including the figures the quotation itself states.
    #[test]
    fn figures_inside_verified_quotes_release() {
        let span = "Mac net sales increased during 2025 compared to 2024 due primarily \
                    to higher net sales of MacBook Air, which contributed $2,412 million."
            .to_string();
        let spans = vec![span.clone()];
        let answer = format!(
            "Higher MacBook Air sales drove the increase in fiscal 2025.\n\n\
             Grounded in the source:\n\"{span}\""
        );
        let basis = no_tool_basis("why did Mac net sales increase in fiscal 2025?", &spans);
        match guard_answer(&armed(), &answer, &basis, "test") {
            GuardVerdict::Released => {}
            v => panic!("verified-quote answer must release, got {v:?}"),
        }
    }

    /// The quote exemption covers ONLY what the verified span states: a
    /// figure in the model's own prose does not trace merely because a
    /// quotation is also present (§6.2(1): numbers in prose are not
    /// facts).
    #[test]
    fn a_prose_figure_beside_a_verified_quote_still_blocks() {
        let spans = vec![
            "Mac net sales increased during 2025 compared to 2024 due primarily to \
             higher net sales of MacBook Air."
                .to_string(),
        ];
        let answer = format!(
            "Mac revenue hit $33,708 million.\n\nGrounded in the source:\n\"{}\"",
            spans[0]
        );
        let basis = no_tool_basis("why did Mac net sales increase?", &spans);
        match guard_answer(&armed(), &answer, &basis, "test") {
            GuardVerdict::Blocked { violations, .. } => {
                assert_eq!(violations, vec!["$33,708 million".to_string()]);
            }
            v => panic!("must block the prose figure, got {v:?}"),
        }
    }

    /// Tool-emitted figures remain legal at the exit (seat correction
    /// 2, note 36ae0d12): a computed value is provenanced by its
    /// computation.
    #[test]
    fn tool_cited_figures_release_at_the_exit() {
        let cited = vec!["land_value_total = $172.62B [sf-assessor-roll]".to_string()];
        let raw = vec![1_400_000_000.0_f64];
        let basis = GuardBasis {
            question: "what would a land levy raise?",
            verified_spans: &[],
            cited: &cited,
            raw_values: &raw,
            allowed_tokens: &[],
        };
        let answer = "The $172.62B base supports a $1.40B replacement levy.";
        match guard_answer(&armed(), answer, &basis, "test") {
            GuardVerdict::Released => {}
            v => panic!("tool-cited figures must release, got {v:?}"),
        }
    }

    /// NO-OP CONTROL (order §controls 3) — arming: a pool with no
    /// declared-authority corpus never constructs an `ArmedAuthority`,
    /// and an empty pool never arms even when a domain is declared.
    #[test]
    fn arming_requires_a_declared_corpus_in_the_evidence_pool() {
        let mut reg = ToolRegistry::new();
        struct Declared;
        #[async_trait::async_trait]
        impl crate::traits::Tool for Declared {
            fn descriptor(&self) -> crate::types::ToolDescriptor {
                crate::types::ToolDescriptor {
                    id: "sec_facts".into(),
                    name: "t".into(),
                    description: String::new(),
                    parameters: serde_json::json!({}),
                    examples: vec![],
                    effect: crate::types::Effect::Read,
                    idempotency: crate::types::Idempotency::Idempotent,
                    latency: crate::types::Latency::Fast,
                    scope: crate::types::Scope::Persistent,
                    output_schema: None,
                }
            }
            fn required_permissions(&self) -> Vec<crate::types::Permission> {
                vec![]
            }
            async fn execute(
                &self,
                _p: &serde_json::Value,
                _c: &crate::types::ToolContext,
            ) -> crate::error::Result<crate::types::StepOutput> {
                Ok(crate::types::StepOutput::Text("ok".into()))
            }
            fn authority_domains(&self) -> Vec<AuthorityClaim> {
                vec![AuthorityClaim {
                    tool_id: "sec_facts".into(),
                    corpus_id: "sec-filings-aapl".into(),
                    matched: "declared".into(),
                }]
            }
        }
        reg.register(Box::new(Declared));

        // The failing input, by name: wikipedia declares nothing.
        assert!(armed_for_evidence(&reg, &["wikipedia".to_string()], "test").is_none());
        // Zero-evidence turns (naked mode, zero chunks) never arm.
        assert!(armed_for_evidence(&reg, &[], "test").is_none());
        // The declared corpus in the pool arms.
        let armed = armed_for_evidence(
            &reg,
            &["wikipedia".to_string(), "sec-filings-aapl".to_string()],
            "test",
        )
        .expect("declared corpus in pool must arm");
        assert_eq!(armed.claims.len(), 1);
        // Scope-level arming (team-pipeline steer) sees the same index.
        assert!(scope_is_armed(&reg, &["sec-filings-aapl".to_string()]));
        assert!(!scope_is_armed(&reg, &["wikipedia".to_string()]));
        assert!(!scope_is_armed(&reg, &[]));
    }

    /// The coverage pin: every intent has a decided story. Adding an
    /// `Intent` variant fails compilation in `guard_story` until its
    /// row is written — the same structural pattern as
    /// `epistemic::every_answer_surface_has_a_ledger_story`.
    #[test]
    fn every_intent_has_a_guard_story() {
        use crate::types::Intent::*;
        for intent in [
            SimpleQuery,
            DeepQuery,
            KnowledgeQuery,
            ComparisonQuery,
            MetalingualQuery,
            ConationQuery,
            CommissiveQuery,
            ExpressiveQuery,
            GenerativeQuery,
            CodeQuery,
            ComplexTask,
            SimpleAction {
                tool: "web_search".into(),
            },
            Continuation {
                task_id: "t1".into(),
            },
        ] {
            match guard_story(&intent) {
                GuardStory::Covered(s)
                | GuardStory::NoOpByConstruction(s)
                | GuardStory::ExcludedByDecision(s) => {
                    assert!(!s.is_empty(), "a story must say why: {intent:?}");
                }
            }
        }
    }
}
