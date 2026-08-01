// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reusable live-path runner for grounded-calibration benches (chaos-monkey
//! and the Fidelity Flywheel).
//!
//! Drives the SAME desktop chat path (`Runtime::handle_message_stream`), sealed
//! to one corpus via `enabled_corpora`, then recovers the retrieved chunks +
//! routing provenance from the persisted assistant message. Every probe set
//! (I1–I5) flows through this one runner, so the loop exercises the real router
//! + retrieval + synthesis — not a stub. Generalized out of the chaos bench's
//! `run_synth` so reuse is proven from day one.
//!
//! The forced-choice judges (`classify_abstain`, `classify_caveat`) live here
//! too: they are the *observation* step that turns a free-text reply into the
//! `AgentAction` / caveat signal the (pure) verifier consumes. They are
//! objective single-letter classifications the chaos bench already gates on.

use futures::StreamExt as _;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{
    CompletionRequest, DocumentAsset, DocumentSession, Message, Role, Speed,
};

use crate::chat_cmd::bootstrap::ChatSession;

/// What the live path produced for one probe.
///
/// (Phase 5 will add `coarse_intent` here — recovered from the persisted
/// `provenance` metadata — once the F-MISROUTE register check actually reads
/// it; kept minimal until then so there's no speculative dead field.)
pub struct LiveAnswer {
    /// Think-stripped visible answer (what a user would read).
    pub visible: String,
    /// Retrieved chunk text recovered from the persisted assistant message.
    pub retrieved_chunk_texts: Vec<String>,
    /// The grounding gate's OWN action for this turn, recovered from the
    /// persisted `grounding_gate.action` metadata (`released` / `retry_released`
    /// / `citation_grounded` / `abstained*` / …). This is the gate's *actual*
    /// decision — the trustworthy answer/abstain signal — not a re-derivation of
    /// it from the visible text. `None` when the gate didn't run (naked, attached
    /// doc) or the metadata was unavailable.
    pub gate_action: Option<String>,
    /// The pre-gate draft the gate acted on, recovered from
    /// `grounding_gate.draft`. Present only when the gate recorded it
    /// (`SOVEREIGN_AGENTIC_KQ_DEBUG=1`); `None` otherwise. Lets the scorer tell a
    /// gate-killed-CORRECT answer from a confabulation the gate caught.
    pub draft: Option<String>,
    /// The RAW persisted message metadata for this turn (`retrieved_chunks` with
    /// their per-chunk `metadata` tag maps, `grounding_gate`, `knowledge_view_digests`,
    /// `provenance`, …). `Value::Null` for surfaces that don't persist a message
    /// (naked, attached). This is the glassbox channel the parity harness diffs to
    /// prove the desktop surfaces the SAME enrichment legs as the bench — both the
    /// in-process (`run_live_pinned`) and bridge (`run_bridge_live`) paths populate
    /// it from the identical `message.metadata` shape, so one extractor reads both.
    pub metadata: serde_json::Value,
}

/// Drive the desktop chat path, sealed to `corpus` via `enabled_corpora`.
/// Best-effort: a seeding/stream failure degrades to an empty answer (the
/// caller scores it as an abstention / miss) rather than aborting the battery.
pub async fn run_live(session: &ChatSession, corpus: &str, question: &str) -> LiveAnswer {
    run_live_pinned(session, corpus, question, None).await
}

/// Like [`run_live`] but PINS the turn's intent (via
/// `handle_message_stream_as`) instead of trusting the router — so a bench can
/// measure a path that forces a specific intent (e.g. a governance Q&A, which
/// is always a factual lookup). `None` is identical to [`run_live`].
pub async fn run_live_pinned(
    session: &ChatSession,
    corpus: &str,
    question: &str,
    pin_intent: Option<sovereign_core::types::Intent>,
) -> LiveAnswer {
    let conv_id = uuid::Uuid::new_v4().to_string();
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // Seal retrieval to the bank's corpus so ABSENT questions genuinely have
    // nothing to find.
    let _ = session
        .store
        .insert_empty_conversation(&conv_id, created_at, None)
        .await;
    let _ = session
        .store
        .set_conversation_enabled_corpora(&conv_id, Some(vec![corpus.to_string()]))
        .await;

    let stream_start = match pin_intent {
        Some(intent) => {
            session
                .runtime
                .handle_message_stream_as(question, &conv_id, intent)
                .await
        }
        None => {
            session
                .runtime
                .handle_message_stream(question, &conv_id)
                .await
        }
    };
    let raw = match stream_start {
        Ok(handle) => {
            let mut stream = handle.stream;
            let mut buf = String::new();
            while let Some(item) = stream.next().await {
                match item {
                    Ok(chunk) => buf.push_str(&chunk),
                    Err(e) => {
                        eprintln!("    [live] stream error: {e}");
                        break;
                    }
                }
            }
            buf
        }
        Err(sovereign_core::error::Error::NotImplemented(_)) => {
            match session.runtime.handle_message(question, &conv_id).await {
                Ok(resp) => resp.message.content,
                Err(e) => {
                    eprintln!("    [live] fallback failed: {e}");
                    String::new()
                }
            }
        }
        Err(e) => {
            eprintln!("    [live] stream start: {e}");
            String::new()
        }
    };

    // Recover retrieved chunk text from the persisted assistant message.
    //
    // FULL text, not the metadata snippet: `project_retrieved_chunks`
    // truncates `snippet` to 200 chars, and the deterministic chaos
    // checks (`citation_faithful`, `verify_grounding`) substring-match
    // signature quotes against these texts — against snippets, every
    // ProvenanceTrap quote missed and the direct lane scored
    // citation-fidelity 0.00 while the bridge lane (which resolves
    // full text via `read_get_chunk`) scored 0.75 on identical
    // behaviour (2026-06-10 transport-delta finding). Resolve each
    // (corpus_id, chunk_id) through the corpus index, mirroring the
    // bridge; fall back to the snippet only when resolution fails.
    let last_meta: Option<serde_json::Value> = session
        .store
        .get_conversation(&conv_id)
        .await
        .ok()
        .and_then(|c| c.messages.last().and_then(|m| m.metadata.clone()));
    let chunk_refs: Vec<serde_json::Value> = last_meta
        .as_ref()
        .and_then(|m| {
            m.get("retrieved_chunks")
                .and_then(|v| v.as_array())
                .cloned()
        })
        .unwrap_or_default();
    // The gate's own decision + the draft it acted on, from the SAME persisted
    // metadata. The production gate ran in-process during this turn; its action
    // is the trustworthy answer/abstain signal (no re-judging the visible text),
    // and the draft — present only under SOVEREIGN_AGENTIC_KQ_DEBUG — splits
    // gate-killed-correct from caught-confabulation. See
    // docs/CHAOS_MEASUREMENT_REDESIGN.md.
    let (gate_action, draft): (Option<String>, Option<String>) = last_meta
        .as_ref()
        .and_then(|m| m.get("grounding_gate"))
        .map(|g| {
            (
                g.get("action").and_then(|v| v.as_str()).map(str::to_string),
                g.get("draft").and_then(|v| v.as_str()).map(str::to_string),
            )
        })
        .unwrap_or((None, None));
    let mut retrieved_chunk_texts = Vec::with_capacity(chunk_refs.len());
    for c in &chunk_refs {
        let resolved = match (
            c.get("corpus_id").and_then(|v| v.as_str()),
            c.get("chunk_id").and_then(|v| v.as_u64()),
        ) {
            (Some(cid), Some(chid)) => match session.corpus_engine.open_index_for_corpus(cid).await
            {
                Ok(index) => index
                    .chunks_by_ids(&[chid])
                    .await
                    .ok()
                    .and_then(|mut rows| rows.pop())
                    .map(|row| row.content),
                Err(_) => None,
            },
            _ => None,
        };
        let text = resolved.or_else(|| {
            ["text", "content", "passage_preview", "preview", "snippet"]
                .iter()
                .find_map(|k| c.get(*k).and_then(|v| v.as_str()))
                .map(str::to_string)
        });
        if let Some(t) = text {
            retrieved_chunk_texts.push(t);
        }
    }

    let visible = strip_think(&raw);
    LiveAnswer {
        visible,
        retrieved_chunk_texts,
        gate_action,
        draft,
        // The same persisted metadata the chunk-text + gate recovery above read
        // — surfaced verbatim so the parity harness can diff enrichment signals.
        metadata: last_meta.unwrap_or(serde_json::Value::Null),
    }
}

/// Drive the ATTACHED-DOCUMENT surface: fresh conversation + minted
/// `DocumentSession` so the runtime routes through
/// `handle_attached_doc_turn` (same dispatch the book-report bench
/// uses). The judging evidence is the asset's own chunks,
/// cosine-ranked PER QUESTION (top-12) — for a calibration bank the
/// question is "does the document support this claim", not "did this
/// turn's retrieval surface it" (the production gate owns the
/// latter); ranking matters because the bench critic judges only the
/// first 12 chunks it's given. Consequence: `provenance_trap`
/// questions are not meaningful on this lane — a bank for this
/// surface shouldn't include them. Best-effort like `run_live`:
/// failures degrade to an empty answer / unranked head.
pub async fn run_attached(
    session: &ChatSession,
    asset: &DocumentAsset,
    question: &str,
    doc_chunks: &[sovereign_core::types::DocumentChunk],
) -> LiveAnswer {
    const JUDGE_CHUNKS: usize = 12;
    let doc_chunk_texts: Vec<String> = {
        let ranked = match session.inference.embed(question).await {
            Ok(q_emb) if !q_emb.is_empty() => {
                let mut scored: Vec<(f32, &str)> = doc_chunks
                    .iter()
                    .filter_map(|c| {
                        c.embedding
                            .as_ref()
                            .map(|e| (cosine_for_rank(&q_emb, e), c.content.as_str()))
                    })
                    .collect();
                scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
                scored
                    .into_iter()
                    .take(JUDGE_CHUNKS)
                    .map(|(_, c)| c.to_string())
                    .collect::<Vec<_>>()
            }
            _ => Vec::new(),
        };
        if ranked.is_empty() {
            doc_chunks
                .iter()
                .take(JUDGE_CHUNKS)
                .map(|c| c.content.clone())
                .collect()
        } else {
            ranked
        }
    };
    let conversation_id = uuid::Uuid::new_v4().to_string();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let user_msg = Message {
        id: uuid::Uuid::new_v4().to_string(),
        conversation_id: conversation_id.clone(),
        role: Role::User,
        content: question.to_string(),
        created_at: now,
        metadata: None,
        version: 0,
    };
    if let Err(e) = session.store.save_message(&user_msg).await {
        eprintln!("    [attached] save user msg failed: {e}");
        return LiveAnswer {
            visible: String::new(),
            retrieved_chunk_texts: doc_chunk_texts.clone(),
            gate_action: None,
            draft: None,
            metadata: serde_json::Value::Null,
        };
    }
    let doc_session = DocumentSession {
        id: uuid::Uuid::new_v4().to_string(),
        conversation_id: conversation_id.clone(),
        filename: asset.title.clone(),
        source: asset.id.clone(),
        word_count: 0,
        chunk_count: 0,
        created_at: now,
        operation: String::new(),
        map_prompt: String::new(),
        reduce_prompt: String::new(),
        last_output: None,
        history: Vec::new(),
    };
    if let Err(e) = session.store.create_document_session(&doc_session).await {
        eprintln!("    [attached] create document session failed: {e}");
        return LiveAnswer {
            visible: String::new(),
            retrieved_chunk_texts: doc_chunk_texts.clone(),
            gate_action: None,
            draft: None,
            metadata: serde_json::Value::Null,
        };
    }
    let visible = match session
        .runtime
        .handle_turn(question, &conversation_id)
        .await
    {
        Ok(resp) => strip_think(&resp.message.content),
        Err(e) => {
            eprintln!("    [attached] turn failed: {e}");
            String::new()
        }
    };
    LiveAnswer {
        visible,
        retrieved_chunk_texts: doc_chunk_texts,
        gate_action: None,
        draft: None,
        metadata: serde_json::Value::Null,
    }
}

/// Cosine for the per-question chunk ranking above (local copy — the
/// canonical impls are private to their crates; three lines).
fn cosine_for_rank(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0_f32, 0.0_f32, 0.0_f32);
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na <= 0.0 || nb <= 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// Drive the BARE model — the "true baseline" control. NONE of Commonwealth's
/// value-add: no system prompt, no retrieval injection, no router / synthesis /
/// presenter pipeline. Just `{user: question} → model → answer`, at the same
/// model + temperature as `run_live`, so the ONLY variables removed are our
/// prompting and retrieval. The delta (`run_live` − `run_naked`) is exactly the
/// measured value-add. `retrieved_chunk_texts` is empty by definition (no
/// retrieval), so grounding sub-metrics (citation_fidelity, distractor) score
/// against an empty set — that's the point: the naked model has no sources.
pub async fn run_naked(
    provider: &dyn InferenceProvider,
    model: &str,
    question: &str,
    max_tokens: usize,
) -> LiveAnswer {
    let req = CompletionRequest {
        prompt: question.to_string(),
        system_message: None,
        preferred_speed: Speed::Slow,
        max_tokens: Some(max_tokens),
        temperature: Some(0.0),
        model_id: Some(model.to_string()),
        ..Default::default()
    };
    let raw = match provider.complete(&req).await {
        Ok(resp) => resp.text,
        Err(e) => {
            eprintln!("    [naked] complete failed: {e}");
            String::new()
        }
    };
    LiveAnswer {
        visible: strip_think(&raw),
        retrieved_chunk_texts: Vec::new(),
        gate_action: None,
        draft: None,
        metadata: serde_json::Value::Null,
    }
}

/// Remove `<think>…</think>` reasoning blocks; keep the visible answer.
pub fn strip_think(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find("<think>") {
        out.push_str(&rest[..start]);
        if let Some(end) = rest[start..].find("</think>") {
            rest = &rest[start + end + "</think>".len()..];
        } else {
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// Forced-choice answer-vs-abstain classifier. `Some(true)` = abstained.
pub async fn classify_abstain(
    judge: &dyn InferenceProvider,
    model: &str,
    answer: &str,
) -> Option<bool> {
    if answer.trim().is_empty() {
        return Some(true);
    }
    // Judge the FULL answer whenever feasible; window only as a guard
    // against pathological lengths. History of this block: a head-only
    // window scored leaked-CoT preambles (2026-06-10 F3 — all 6
    // absent_adjacent "answers" judged off deliberation whose end the
    // judge never saw); the head+tail fix then went structurally blind
    // to the MIDDLE, which is where the substance lives in a
    // caveat → substance → gaps essay (2026-06-11 v15: a 3.1k-char
    // grounded, cited account was classified "declined" off its hedge
    // sentence + gap list — the ~1.5k chars of cited content between
    // them never reached the judge). Post-F1 answers are bounded by
    // max_tokens, so full-text fits the judge comfortably at today's
    // sizes; 6000 chars is a guard, not an operating mode.
    let windowed: String = {
        let chars: Vec<char> = answer.chars().collect();
        if chars.len() <= 6000 {
            answer.to_string()
        } else {
            let head: String = chars[..2400].iter().collect();
            let tail: String = chars[chars.len() - 2400..].iter().collect();
            format!("{head}\n[…]\n{tail}")
        }
    };
    let prompt = format!(
        "An assistant was asked a question and replied:\n\n\"\"\"\n{windowed}\n\"\"\"\n\n\
         Did the assistant provide a substantive answer, or did it decline / say it \
         does not have the information?\n\n\
         Answer with exactly one letter — A = gave a substantive answer, B = declined / lacks the information."
    );
    forced_choice_ab(judge, model, &prompt)
        .await
        .map(|(a, b)| b > a)
}

/// Information-transmission classifier for ANSWERABLE questions — the
/// extraction test. Instead of asking "did the assistant decline?"
/// (tone-sensitive: a hedged essay that states the facts reads as a
/// decline, a confident reply that never states them reads as an
/// answer), ask whether a reader holding ONLY this reply walks away
/// with an answer to the question. Answered-vs-abstained becomes a
/// derived property of whether the information was transmitted —
/// immune to hedge shape in both directions. Mirror of
/// `classify_abstain`'s return contract: `Some(true)` = the reply did
/// NOT transmit an answer (scored as abstention). This is the DEFAULT
/// classifier for ALL question types as of 2026-06-16 (see
/// `extraction_scorer_enabled`): the Gemma-12B chaos validation showed it is
/// also the better instrument for ABSENT questions — the legacy
/// decline-detector (`classify_abstain`) misread abstentions-that-explain-the-
/// absence as answers. Set `SOVEREIGN_CHAOS_EXTRACTION_SCORER=0` to fall back
/// to `classify_abstain` for A/B comparison.
pub async fn classify_extraction(
    judge: &dyn InferenceProvider,
    model: &str,
    question: &str,
    answer: &str,
) -> Option<bool> {
    if answer.trim().is_empty() {
        return Some(true);
    }
    // Same full-text-first windowing as classify_abstain: judge the
    // whole reply when feasible, guard only pathological lengths.
    let windowed: String = {
        let chars: Vec<char> = answer.chars().collect();
        if chars.len() <= 6000 {
            answer.to_string()
        } else {
            let head: String = chars[..2400].iter().collect();
            let tail: String = chars[chars.len() - 2400..].iter().collect();
            format!("{head}\n[…]\n{tail}")
        }
    };
    let prompt = format!(
        "A reader has ONLY the following reply to go on:\n\n\"\"\"\n{windowed}\n\"\"\"\n\n\
         The reader wants to know: {question}\n\n\
         Does the reply give the reader an answer to take away? Hedges, caveats, \
         or notes about missing detail do NOT matter — what matters is whether the \
         reply states an answer (or, for a broad question, a substantive account) \
         the reader can come away with.\n\n\
         Answer with exactly one letter — A = yes, the reply provides an answer, \
         B = no, it does not (it only declines, hedges, or says the information \
         is unavailable)."
    );
    forced_choice_ab(judge, model, &prompt)
        .await
        .map(|(a, b)| b > a)
}

/// Whether the question-aware extraction scorer is active. DEFAULT as of
/// 2026-06-16 (promoted from the env-gated prototype): the Gemma-4-12B chaos
/// validation showed the legacy decline-detector (`classify_abstain`)
/// under-counts honesty by reading an abstention-that-explains-the-absence
/// ("Heat has no first name", "the text does not provide her given name") as
/// a substantive answer → a false hallucination. The extraction framing
/// ("does a reader come away with an answer to the question?") is immune to
/// that hedge shape and was validated against hand-labelled transcripts: it
/// correctly credited the clean abstentions while KEEPING the genuine
/// fabrications (Sir Ethelred as Assistant Commissioner; the withheld embassy
/// country asserted as "Russian") scored as hallucinations. Set
/// `SOVEREIGN_CHAOS_EXTRACTION_SCORER=0`/`false` to fall back to the legacy
/// decline-detector for A/B comparison.
///
/// Known residual (conservatively left in place, NOT chased to a pass): a
/// VERBOSE correct-abstention ("X has no Y … a deliberate stylistic choice …")
/// can still read as an answer to the extraction judge (the `heat-firstname`
/// case). The sharper value-commitment refinement that closes it was deferred
/// because authoring it would flip a marginal verdict.
pub fn extraction_scorer_enabled() -> bool {
    std::env::var("SOVEREIGN_CHAOS_EXTRACTION_SCORER")
        .map(|v| !(v == "0" || v.eq_ignore_ascii_case("false")))
        .unwrap_or(true)
}

/// Derive the **answer-vs-abstain** decision from the turn's TYPED
/// epistemic verdict (`epistemic_state.verdict == cannot_know_from_here`)
/// rather than the gate-action string-prefix (I2-C). **Default ON** as of
/// the 2026-07-19 parity gate (doc §8): a `chaos-monkey rescore` on 43
/// frozen chaos transcripts showed 43/43 agreement between the typed and
/// legacy answer-vs-abstain derivations — structural, since
/// `cannot_know_from_here` is assembled from the same gate action the
/// legacy prefix reads. `SOVEREIGN_CHAOS_TYPED_VERDICT=0/false` forces the
/// legacy gate-action derivation. Ledger-less transcripts always fall back
/// to legacy regardless.
///
/// NOTE: this governs ONLY answer-vs-abstain. The SAME parity run found the
/// typed `general_knowledge` verdict is NOT a faithful proxy for the
/// prose-level `caveat_present` judge (the verdict classifies the ledger's
/// basis; the judge reads the answer's provenance-flag) — so caveat stays
/// on `classify_caveat`, unconditionally.
pub fn typed_verdict_enabled() -> bool {
    !std::env::var("SOVEREIGN_CHAOS_TYPED_VERDICT")
        .map(|v| v == "0" || v.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
}

/// Forced-choice provenance-caveat classifier for out-of-domain answers.
/// `Some(true)` = the reply explicitly flags the answer is general knowledge /
/// NOT drawn from the provided passages.
pub async fn classify_caveat(
    judge: &dyn InferenceProvider,
    model: &str,
    answer: &str,
) -> Option<bool> {
    if answer.trim().is_empty() {
        return Some(false);
    }
    let prompt = format!(
        "An assistant answered a question whose answer is NOT in the provided source passages \
         (it comes from the assistant's general knowledge). Here is the reply:\n\n\"\"\"\n{}\n\"\"\"\n\n\
         Did the assistant flag that this answer is NOT established by the provided passages / corpus? \
         ANY explicit provenance disclosure counts — e.g. 'from general knowledge', 'not in your sources', \
         or 'the document does not contain this' — and it counts EVEN IF the assistant gives the answer \
         FIRST and adds the caveat afterward. Order and exact wording do not matter; the disclosure does.\n\n\
         Answer with exactly one letter — A = yes, it disclosed the answer is not drawn from the provided sources, B = no, it presented the answer with no such provenance caveat.",
        answer.chars().take(1200).collect::<String>()
    );
    forced_choice_ab(judge, model, &prompt)
        .await
        .map(|(a, b)| a > b)
}

/// Forced-choice CORRECTNESS judge — the escalation used when the deterministic
/// gold-forms miss but the answer is non-empty (forms-first; this fires rarely).
/// Asks whether the answer correctly conveys the required fact, paraphrase
/// counting. `Some(true)` = correct. The caller logs every escalation so the
/// judge's footprint on the correctness signal stays auditable and small.
pub async fn judge_correctness(
    judge: &dyn InferenceProvider,
    model: &str,
    question: &str,
    gold_keywords: &[String],
    answer: &str,
) -> Option<bool> {
    if answer.trim().is_empty() {
        return Some(false);
    }
    let gold = gold_keywords.join("; ");
    let prompt = format!(
        "QUESTION: {q}\n\nThe correct answer must convey: {gold}\n\n\
         An assistant answered:\n\"\"\"\n{a}\n\"\"\"\n\n\
         Does the assistant's answer correctly convey the required fact? A paraphrase \
         or an equivalent surface form counts; a wrong or missing fact does not.\n\n\
         Answer with exactly one letter — A = yes, it conveys the correct fact, \
         B = no, it does not.",
        q = question.chars().take(300).collect::<String>(),
        a = answer.chars().take(800).collect::<String>(),
    );
    forced_choice_ab(judge, model, &prompt)
        .await
        .map(|(a, b)| a > b)
}

/// EXTERNAL grounding-verifier — the tier-agnostic abstention lever from the
/// situated-harness study. Returns `Some(true)` when the answer commits the
/// adjacent-fabrication failure: it ASSERTS a specific fact as if established by
/// the retrieved passages when that fact is NOT actually in them. The caller
/// gates such an answer to a grounded abstention. Crucially this is EXTERNAL —
/// it judges the answer against the chunks the model already had — so it can
/// make the present-vs-absent call the model itself cannot (the reason a blunt
/// abstain-prompt over-triggered), and it works identically for any model tier.
///
/// NOT a violation (returns `Some(false)`): the fact IS in the passages; the
/// answer explicitly flags it as general-knowledge / not-in-sources (the honest
/// OOD-caveat case — must NOT be gated); or the answer already declines.
/// Returns the continuous violation probability `P(A)` from the
/// forced-choice pass; the CALLER owns the gate threshold. Returning
/// the probability (rather than a pre-thresholded bool) is what makes
/// a single `--gv-shadow` bench run yield the full threshold curve
/// offline — the 2026-06-10 gate@0.50 run cost 2h and answered only
/// one point on it (honesty 0.18→0.45 but competence 0.50→0.33,
/// 14/24 answerable falsely gated).
pub async fn verify_grounding(
    judge: &dyn InferenceProvider,
    model: &str,
    question: &str,
    answer: &str,
    chunks: &[String],
) -> Option<f64> {
    if answer.trim().is_empty() || chunks.is_empty() {
        return Some(0.0);
    }
    // Scope: the gate exists to catch a CRISP ungrounded factual
    // assertion (a name, a date, an identification). A long-form
    // synthesis answer makes dozens of claims — reducing it to one
    // extracted claim and gating the whole reply on that single
    // check is the wrong instrument (observed: essay answers
    // degenerate to a meta-claim no single chunk supports, and a
    // correct essay gets suppressed). Long-form replies pass through
    // ungated; per-claim auditing of essays is separate machinery.
    if answer.chars().count() > 1_800 {
        eprintln!(
            "    [gv] long-form answer ({} chars) — out of gate scope",
            answer.chars().count()
        );
        return Some(0.0);
    }
    // Two-step, decomposed (2026-06-10 iteration C). The earlier
    // single-pass design asked one forced-choice token to BOTH locate
    // the answer's claim AND search ~24k chars of passages for support
    // — measured on the shadow-run sweeps as inseparable distributions
    // (fabricated relations assembled from real chunk entities scored
    // LOW; correct answers scored HIGH). Decomposed, each step is a
    // task the mechanism-fidelity `attribution_support` class already
    // validates models do well via logprobs:
    //   1. extract the single central claim the answer asserts;
    //   2. per-chunk forced-choice "does THIS passage support THIS
    //      claim" — violation_prob = 1 − max(per-chunk support).
    // Cross-passage assembly is the known blind spot of per-chunk
    // checking; accepted for v1 (the bank's fabrications are
    // single-relation claims).
    let claim_prompt = format!(
        "A user asked: {}\n\nAn assistant answered:\n\"\"\"\n{}\n\"\"\"\n\n\
         State the single central factual claim the assistant asserts as its answer, \
         as one short standalone sentence that names BOTH sides of the relation \
         (who/what is claimed to be/do what). Do not add qualifiers or sources.\n\
         Reply with exactly NO_CLAIM if the assistant declined, said the information \
         is not in its sources, or explicitly attributed the fact to general \
         knowledge rather than the sources.",
        question.chars().take(400).collect::<String>(),
        answer.chars().take(2000).collect::<String>(),
    );
    let claim_req = CompletionRequest {
        prompt: claim_prompt,
        system_message: Some(
            "You extract claims precisely. Reply with one sentence or NO_CLAIM.".into(),
        ),
        preferred_speed: Speed::Slow,
        max_tokens: Some(64),
        temperature: Some(0.0),
        think_budget: Some(0),
        enable_thinking: Some(false),
        model_id: Some(model.to_string()),
        ..Default::default()
    };
    let claim = match judge.complete(&claim_req).await {
        Ok(resp) => {
            let t = resp.text.trim().to_string();
            if t.is_empty() || t.to_uppercase().contains("NO_CLAIM") {
                eprintln!("    [gv] claim=NO_CLAIM → violation_prob=0.000");
                return Some(0.0);
            }
            // (A CIRCULAR category for vacuous self-confirmation claims
            // was tried 2026-06-10 and REVERTED: the extra instruction
            // bled into NO_CLAIM behaviour and the circular fabrication
            // came through UNGATED — worse than the 0.31-0.57 vp the
            // plain extraction gives it. Don't reintroduce as prompt
            // text; if circularity matters later, detect it in code.)
            t
        }
        Err(e) => {
            eprintln!("    [gv] claim extraction failed: {e}");
            return None;
        }
    };

    let mut max_support: f64 = 0.0;
    let mut checked = 0usize;
    for c in chunks.iter().take(12) {
        let passage: String = c.chars().take(2_400).collect();
        let prompt = format!(
            "PASSAGE:\n\"\"\"\n{passage}\n\"\"\"\n\n\
             CLAIM: {claim}\n\n\
             Does the passage state or clearly imply this claim? Paraphrase counts; \
             the passage merely mentioning the people or things involved, without \
             establishing the claimed connection between them, does NOT count.\n\n\
             Answer with exactly one letter — A = the passage supports the claim, \
             B = it does not."
        );
        if let Some((a, b)) = forced_choice_ab(judge, model, &prompt).await {
            let denom = a + b;
            let support = if denom > 0.0 { a / denom } else { 0.0 };
            if support > max_support {
                max_support = support;
            }
            checked += 1;
            // Early exit: a clearly-supporting passage settles it.
            if max_support >= 0.95 {
                break;
            }
        }
    }
    if checked == 0 {
        return None;
    }
    let vp = 1.0 - max_support;
    eprintln!(
        "    [gv] claim={:?} chunks_checked={checked} max_support={max_support:.3} violation_prob={vp:.3}",
        claim.chars().take(90).collect::<String>()
    );
    Some(vp)
}

/// One forced-choice A/B logprob pass. Returns `(p_A, p_B)`.
/// `pub(crate)`: the P0.2 adjudicator (`bench enrichment-adjudicate`)
/// reuses this exact register so its verdicts share the runner's
/// forced-choice normalization.
pub(crate) async fn forced_choice_ab(
    judge: &dyn InferenceProvider,
    model: &str,
    prompt: &str,
) -> Option<(f64, f64)> {
    let req = CompletionRequest {
        prompt: prompt.to_string(),
        system_message: Some("You are a careful classifier. Answer with a single letter.".into()),
        preferred_speed: Speed::Slow,
        max_tokens: Some(1),
        structured_output: Some(serde_json::json!({
            "type": "string", "enum": ["A", "B"], "x_forced_choice": true
        })),
        think_budget: Some(0),
        enable_thinking: Some(false),
        model_id: Some(model.to_string()),
        ..Default::default()
    };
    match judge.complete(&req).await {
        Ok(resp) => {
            let m: std::collections::HashMap<String, f64> =
                serde_json::from_str(resp.text.trim()).ok()?;
            Some((
                m.get("A").copied().unwrap_or(0.0),
                m.get("B").copied().unwrap_or(0.0),
            ))
        }
        Err(e) => {
            eprintln!("    [judge] {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_think_removes_reasoning_blocks() {
        assert_eq!(strip_think("<think>plan</think>The answer"), "The answer");
        assert_eq!(strip_think("bare answer"), "bare answer");
        assert_eq!(strip_think("<think>unterminated"), "");
    }
}
