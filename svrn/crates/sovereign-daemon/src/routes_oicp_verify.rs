// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /oicp/v1/verify` — a unit of text judged claim by claim against the
//! stored texts the caller may read (OICP v0.6 §2.6), by meaning (§2.15,
//! `evidence:verify_sentences`), outside any turn.
//!
//! Built from what already runs (CITE_V1 §1.4, P4), never a new verifier.
//! The rungs, in order, each a tracing event:
//! - **opinion**: a claim holding a marker of the data file's rule
//!   (`routes_oicp_verify_opinion`, `data/opinion_markers.toml`) is offered as
//!   the writer's view, `not_checkable` before any search or model call;
//! - **class**: claims are the unit's sentences, and a claim is checkable when
//!   it carries a specific (`sovereign_core::runtime::sentence_specifics`:
//!   figures, dates, quantities and names, nothing inside a citation or a
//!   locator). Every checkable claim names them on the wire by kind;
//! - **candidates**: the claim's sentences from sentence search
//!   (`routes_knowledge_sentences::sentences`, the one retrieval path search
//!   and verify share), each a sentence with its span;
//! - **floor**: a claim no sentence scores `verify_floor` for (the data file,
//!   `data/sentence_search.toml`, named in `verifier`) is `unsupported` with
//!   no evidence at every tier: nothing in the library is about it;
//! - **fast** asks, with no model, whether each specific appears in a
//!   candidate (`value_present_in_chunks`, which finds a number by its
//!   value). A miss is a hint in `specifics_absent`; the tier `abstained`;
//! - **checker** judges the claim against each of the best candidates in the
//!   gate's own per-claim register (`claim_chunk_support`), at the
//!   faithfulness lane's threshold. It is shown the claim less its citations
//!   and locators (`without_citations`, the recogniser the specifics use):
//!   a source backs what a claim says, not whom it cites. A candidate it does not back that states
//!   one of the claim's figures, dates or quantities otherwise
//!   (`differing_specifics`) is `contradicted` and names the pair.
//!
//! Every operating point is `uncalibrated` until the bench has measured a
//! cell.
//!
//! A checker call the host's model queue sheds is backpressure, not a claim
//! the checker could not judge: the unit is answered `503` with `Retry-After`
//! and the shed body every route uses (`admission::local_queue_shed_response`),
//! judging stops at the first shed, and nothing of the unit is journaled.

use std::sync::Arc;

use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use kernel_types::Sha256Hash;
use oicp_types::evidence::{CorpusTexts, Span, Unavailable};
use oicp_types::verify::{
    reasons, Checkability, ClaimSpecific, ClaimSpecificKind, DifferingSpecific, JudgedPassage,
    VerifiedClaim, VerifyRequest, VerifyResponse, VerifyTier, VerifyVerdict, UNCALIBRATED,
};
use oicp_types::{Granularity, KnowledgeSearchRequest};
use sovereign_contracts::principal::{AttachedPrincipal, Principal};
use sovereign_contracts::types::SpecificsTally;
use sovereign_core::oicp::ShardingPrivacy;
use sovereign_core::runtime::{
    claim_chunk_support_checked, differing_specifics, sentence_ranges, sentence_specifics,
    value_present_in_chunks, without_citations, Specific, SpecificKind,
};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::Speed;
use sovereign_tools::summary_verify::{EARLY_EXIT_SUPPORT, SUPPORTED_TAU};

use crate::oicp_evidence::{absence_reason, open_for_reading, readable_corpora, requested_corpora};
use crate::routes_internal::ErrorBody;
pub use crate::routes_oicp_verify_journal::VerifyJournal;
use crate::sentence_index::SentenceSearchConfig;
use crate::state::AppState;

/// Where the route is mounted, and what the manifest advertises.
pub const VERIFY_ENDPOINT: &str = "/oicp/v1/verify";
/// The tiers this host serves, as the manifest advertises them.
pub const TIERS: [VerifyTier; 2] = [VerifyTier::Fast, VerifyTier::Checker];
/// Sentences asked of sentence search per claim; those at the floor or above
/// are the claim's candidates.
const CANDIDATES_K: usize = 8;
/// Candidates the checker judges per claim, best first. The judge reads
/// them one at a time, so this is the tier's latency: at the gate's
/// `MEMBER_CAP` (12, so all 8 candidates) X1's draft took 4.0-8.5 s a claim
/// warm (n=4, 2026-10-12), over the S6 order's 5 s line; operator chose 3
/// the same day.
pub const CHECKER_PASSAGES: usize = 3;
/// The rung names on the wire: a change to what a rung does changes its name.
const RUNG_OPINION: &str = "class/opinion";
const RUNG_CLASS: &str = "class/no-specifics";
const RUNG_SEARCH: &str = "sentences/search";
const RUNG_FLOOR: &str = "sentences/floor";
const RUNG_FAST: &str = "fast/value-presence";
const RUNG_CHECKER: &str = "checker/sentence-support";

/// The model the checker judges with: layered onto the operator listener's
/// router only (`client_surface::operator_listener_router`). A listener
/// without it answers every `checker` claim above the floor `could_not_judge:
/// no checker`; `fast` needs no model and answers everywhere `verify` is
/// mounted.
#[derive(Clone)]
pub struct VerifyJudge(pub Arc<dyn InferenceProvider>);

type Refusal = Response;

fn refuse(status: StatusCode, error: impl Into<String>) -> Refusal {
    (
        status,
        Json(ErrorBody {
            error: error.into(),
        }),
    )
        .into_response()
}

/// A checker call the model queue refused before parking it
/// (`Error::QueueShed`), with the numbers the shed response carries.
struct Shed {
    position: u32,
    predicted_wait_ms: u64,
    retry_after_secs: u64,
}

/// One sentence sentence search returned for a claim, with its span.
struct Passage {
    corpus_id: String,
    span: Span,
    score: f32,
}

/// What every claim of a unit is read against: the corpora read, through
/// sentence search under the data file's choices.
struct Retrieval<'a> {
    state: &'a AppState,
    principal: Option<&'a Principal>,
    corpora: Vec<String>,
    cfg: SentenceSearchConfig,
    context: usize,
}

impl Retrieval<'_> {
    /// The claim's sentences, best first, or why sentence search refused.
    async fn sentences(&self, asked: &str) -> Result<Vec<Passage>, String> {
        let request = KnowledgeSearchRequest {
            query_embedding: Vec::new(),
            query_text: asked.to_string(),
            corpora: Some(self.corpora.clone()),
            limit: Some(CANDIDATES_K as u32),
            documents: Vec::new(),
            granularity: Granularity::Sentence,
            priority: None,
        };
        let reply = crate::routes_knowledge_sentences::sentences(
            self.state.clone(),
            self.principal,
            request,
            None,
            self.cfg.clone(),
            self.context,
        )
        .await
        .map_err(|r| r.reason())?;
        let (mut out, mut unspanned) = (Vec::new(), 0usize);
        for h in reply.results {
            match h.span {
                Some(span) => out.push(Passage {
                    corpus_id: h.corpus_id,
                    span,
                    score: h.score,
                }),
                None => unspanned += 1,
            }
        }
        if unspanned > 0 {
            tracing::warn!(
                unspanned,
                "oicp verify: sentence hits with no span, not judged"
            );
        }
        tracing::debug!(
            route = ?reply.route,
            pending = ?reply.sentences_pending,
            found = out.len(),
            "oicp verify: sentence candidates"
        );
        Ok(out)
    }
}

/// `POST /oicp/v1/verify`, run under the request's `priority` (v0.6 §2.17),
/// which nothing holds since the engine was deleted; the daemon does not
/// advertise `knowledge:priority`.
pub async fn verify(
    state: State<AppState>,
    principal: Option<Extension<AttachedPrincipal>>,
    judge: Option<Extension<VerifyJudge>>,
    journal: Option<Extension<VerifyJournal>>,
    Json(req): Json<VerifyRequest>,
) -> Result<Json<VerifyResponse>, Refusal> {
    let priority = req.priority;
    sovereign_contracts::request_priority::asked(
        priority,
        judge_unit(state, principal, judge, journal, req),
    )
    .await
}

async fn judge_unit(
    State(state): State<AppState>,
    principal: Option<Extension<AttachedPrincipal>>,
    judge: Option<Extension<VerifyJudge>>,
    journal: Option<Extension<VerifyJournal>>,
    req: VerifyRequest,
) -> Result<Json<VerifyResponse>, Refusal> {
    let started = std::time::Instant::now();
    if req.unit.trim().is_empty() {
        return Err(refuse(StatusCode::BAD_REQUEST, reasons::EMPTY_UNIT));
    }
    if !TIERS.contains(&req.tier) {
        tracing::info!(tier = req.tier.as_str(), "oicp verify: tier not offered");
        return Err(refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            reasons::TIER_NOT_OFFERED,
        ));
    }
    let Some(engine) = state.inner.node.corpus_engine.clone() else {
        return Err(refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            crate::hosted_ingest::NO_INGEST,
        ));
    };
    let installed = engine.installed_indexes().await.map_err(|e| {
        tracing::warn!(error = %e, "oicp verify: installed corpora unread");
        refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("installed corpora unread: {e}"),
        )
    })?;
    let principal = principal.as_ref().map(|Extension(p)| &p.0);
    let readable = readable_corpora(&state, principal, &installed);
    let (mut targets, mut corpora_unavailable) =
        requested_corpora(&req.corpora, &readable, "verify");
    // Unconstrained, the scope is what sentence search reaches unasked: the
    // corpora already sentence-indexed here, never an embedding run over
    // every corpus the caller may read.
    if req.corpora.is_empty() {
        let readable = targets.len();
        targets.retain(|id| {
            state
                .inner
                .node
                .sentences
                .is_indexed(engine.index_dir(), id)
        });
        tracing::debug!(
            readable,
            indexed = targets.len(),
            "oicp verify: unconstrained; the sentence-indexed corpora read"
        );
    }

    let mut corpora: Vec<CorpusTexts> = Vec::new();
    for corpus_id in targets {
        match texts_digest(engine.as_ref(), &corpus_id).await {
            Ok(digest) => corpora.push(CorpusTexts {
                corpus_id,
                texts_digest: digest.to_hex(),
            }),
            Err(reason) => {
                tracing::info!(corpus = %corpus_id, %reason, "oicp verify: corpus unavailable");
                corpora_unavailable.push(Unavailable { corpus_id, reason });
            }
        }
    }

    let inference = judge.map(|Extension(VerifyJudge(i))| i);
    let retrieval = state
        .inner
        .node
        .sentences
        .config()
        .map(|cfg| Retrieval {
            state: &state,
            principal,
            corpora: corpora.iter().map(|c| c.corpus_id.clone()).collect(),
            cfg,
            context: req.effective_span_context() as usize,
        })
        .map_err(|why| {
            tracing::warn!(%why, "oicp verify: sentence search is not offered; no claim can be read");
            format!("sentence search not offered: {why}")
        });
    let verifier = verifier_identity(
        req.tier,
        retrieval.as_ref().ok().map(|r| &r.cfg),
        inference.as_deref(),
    );
    let mut claims = Vec::new();
    let mut kept = Vec::new();
    for range in sentence_ranges(&req.unit) {
        // `range` is one of `sentence_ranges(&req.unit)`, so it is in range.
        let exact = quote_align::code_point_slice(&req.unit, range.clone())
            .unwrap_or_default()
            .to_string();
        let judged_at = std::time::Instant::now();
        let judged = match judge_claim(
            &exact,
            req.tier,
            &retrieval,
            !corpora.is_empty(),
            inference.as_ref(),
        )
        .await
        {
            Ok(judged) => judged,
            Err(shed) => {
                tracing::info!(
                    claims_judged = claims.len(),
                    queue_position = shed.position,
                    predicted_wait_ms = shed.predicted_wait_ms,
                    retry_after_secs = shed.retry_after_secs,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "oicp verify: checker shed, answered host busy"
                );
                return Err(crate::admission::local_queue_shed_response(
                    shed.position,
                    shed.predicted_wait_ms,
                    shed.retry_after_secs,
                ));
            }
        };
        let judged_ms = judged_at.elapsed().as_millis() as u64;
        // Sentences name no chunk: the journal counts them unresolved.
        let refs: Vec<(String, Option<u64>)> = judged
            .evidence
            .iter()
            .map(|e| (e.passage.corpus_id.clone(), None))
            .collect();
        let tally = judged.tally;
        let claim = judged.into_wire(range, exact);
        tracing::debug!(
            claim = %claim.exact,
            verdict = ?claim.verdict,
            rung = %claim.rung,
            evidence = claim.evidence.len(),
            specifics = claim.specifics.len(),
            absent = ?claim.specifics_absent,
            "oicp verify: claim judged"
        );
        claims.push(claim);
        kept.push((refs, tally, judged_ms));
    }
    // Journaled once every claim is judged: a unit answered `503` keeps nothing.
    if let Some(Extension(VerifyJournal(dir))) = &journal {
        for (claim, (refs, tally, judged_ms)) in claims.iter_mut().zip(kept) {
            claim.episode_id = crate::routes_oicp_verify_journal::record_claim(
                dir, claim, &refs, tally, judged_ms,
            );
        }
    }
    tracing::info!(
        tier = req.tier.as_str(),
        claims = claims.len(),
        corpora = corpora.len(),
        unavailable = corpora_unavailable.len(),
        checker = inference.is_some(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "oicp verify: answered"
    );
    Ok(Json(VerifyResponse {
        claims,
        verifier,
        corpora,
        corpora_unavailable,
    }))
}

/// Who judged: the route, the scorer and floor every rung reads candidates
/// by (the data file's), the rung for the tier, and for the checker the
/// threshold, the passages it judges and the model its slot names
/// (`model_id_for(Slow)`, the slot the judge workload prefers; routing may
/// serve it elsewhere, which this does not see).
fn verifier_identity(
    tier: VerifyTier,
    cfg: Option<&SentenceSearchConfig>,
    inference: Option<&dyn InferenceProvider>,
) -> String {
    let reading = cfg.map_or_else(
        || "sentences=none".to_string(),
        |c| format!("scorer={} floor={}", c.scorer.as_str(), c.verify_floor),
    );
    let door = crate::routes_oicp_verify_opinion::identity();
    match tier {
        VerifyTier::Checker => format!(
            "verify/3 {reading} {door} {RUNG_CHECKER} tau={SUPPORTED_TAU} passages={CHECKER_PASSAGES} model={}",
            inference.map_or_else(|| "none".to_string(), |i| i.model_id_for(Speed::Slow))
        ),
        _ => format!("verify/3 {reading} {door} {RUNG_FAST}"),
    }
}

/// The digest of a corpus's texts, or why it cannot be read.
async fn texts_digest(
    engine: &dyn corpus_index::ingest_port::daemon::IngestPort,
    corpus_id: &str,
) -> Result<Sha256Hash, String> {
    let index = open_for_reading(engine, corpus_id)
        .await
        .map_err(|e| format!("corpus unreadable: {e}"))?;
    match engine.texts_digest(&index).await {
        Ok(Ok(d)) => Ok(d),
        Ok(Err(absence)) => Err(absence_reason(absence).into()),
        Err(e) => Err(format!("texts unreadable: {e}")),
    }
}

/// One passage judged, with what it states otherwise where contradicted.
struct Judgement {
    passage: Passage,
    verdict: VerifyVerdict,
    score: Option<f32>,
    /// Pairs (the claim's, the sentence's), ranges in each's own text.
    differing: Vec<(Specific, Specific)>,
}

/// A claim's verdict before it is put on the wire.
struct Judged {
    class: Checkability,
    verdict: VerifyVerdict,
    rung: &'static str,
    score: Option<f32>,
    specifics: Vec<Specific>,
    specifics_absent: Vec<String>,
    evidence: Vec<Judgement>,
    /// The claim's specifics against the passages read, for the journal.
    tally: Option<SpecificsTally>,
}

impl Judged {
    fn new(class: Checkability, verdict: VerifyVerdict, rung: &'static str) -> Self {
        Judged {
            class,
            verdict,
            rung,
            score: None,
            specifics: Vec::new(),
            specifics_absent: Vec::new(),
            evidence: Vec::new(),
            tally: None,
        }
    }

    fn could_not(specifics: Vec<Specific>, rung: &'static str, reason: impl Into<String>) -> Self {
        let verdict = VerifyVerdict::CouldNotJudge {
            reason: reason.into(),
        };
        Judged {
            specifics,
            ..Judged::new(Checkability::Checkable, verdict, rung)
        }
    }
}

async fn judge_claim(
    claim: &str,
    tier: VerifyTier,
    retrieval: &Result<Retrieval<'_>, String>,
    any_corpus: bool,
    inference: Option<&Arc<dyn InferenceProvider>>,
) -> Result<Judged, Shed> {
    // An unreadable rule holds nothing at the door; `verifier` says `opinion=none`.
    let held =
        crate::routes_oicp_verify_opinion::rule().map_or(Vec::new(), |r| r.markers_in(claim));
    if !held.is_empty() {
        tracing::debug!(%claim, markers = ?held, "oicp verify: offered as the writer's view; not checkable");
        let (class, verdict) = (Checkability::NotCheckable, VerifyVerdict::Abstained);
        return Ok(Judged::new(class, verdict, RUNG_OPINION));
    }
    let specifics = sentence_specifics(claim);
    if specifics.is_empty() {
        tracing::debug!(%claim, "oicp verify: no specifics; not checkable");
        return Ok(Judged::new(
            Checkability::NotCheckable,
            VerifyVerdict::Abstained,
            RUNG_CLASS,
        ));
    }
    if !any_corpus {
        tracing::debug!(%claim, "oicp verify: no corpus read");
        return Ok(Judged::could_not(
            specifics,
            RUNG_CLASS,
            reasons::NO_CORPUS_READ,
        ));
    }
    let retrieval = match retrieval {
        Ok(r) => r,
        Err(why) => return Ok(Judged::could_not(specifics, RUNG_SEARCH, why.clone())),
    };
    // One cut, read by the search and the checker alike; the specifics, the
    // contradiction check and the wire span stay on the unit's own text.
    let (asked, cut) = without_citations(claim);
    if cut > 0 {
        tracing::debug!(
            %claim,
            citations = cut,
            "oicp verify: citations and locators removed from what is searched and judged"
        );
    }
    let found = match retrieval.sentences(&asked).await {
        Ok(found) => found,
        Err(why) => {
            tracing::info!(%claim, %why, "oicp verify: sentence search refused; could not judge");
            return Ok(Judged::could_not(specifics, RUNG_SEARCH, why));
        }
    };
    let floor = retrieval.cfg.verify_floor;
    let best = found.first().map(|p| p.score);
    let mut candidates: Vec<Passage> = found.into_iter().filter(|p| p.score >= floor).collect();
    if candidates.is_empty() {
        tracing::debug!(%claim, ?best, floor, "oicp verify: no sentence reaches the floor; unsupported, no evidence");
        return Ok(Judged {
            specifics,
            ..Judged::new(
                Checkability::Checkable,
                VerifyVerdict::Unsupported,
                RUNG_FLOOR,
            )
        });
    }
    let read: Vec<String> = candidates.iter().map(|p| p.span.exact.clone()).collect();
    let absent: Vec<String> = specifics
        .iter()
        .filter(|s| !value_present_in_chunks(&s.text, &read))
        .map(|s| s.text.clone())
        .collect();
    let tally = SpecificsTally {
        checked: specifics.len() as u32,
        absent: absent.len() as u32,
    };
    Ok(match tier {
        VerifyTier::Fast => {
            // The literal miss is a hint the checker reads, never a verdict.
            tracing::debug!(
                %claim,
                specifics = specifics.len(),
                candidates = candidates.len(),
                ?absent,
                "oicp verify: fast tier abstains, its misses a hint"
            );
            Judged {
                score: Some(1.0 - absent.len() as f32 / specifics.len() as f32),
                specifics,
                specifics_absent: absent,
                evidence: candidates
                    .into_iter()
                    .map(|passage| Judgement {
                        passage,
                        verdict: VerifyVerdict::Abstained,
                        score: None,
                        differing: Vec::new(),
                    })
                    .collect(),
                tally: Some(tally),
                ..Judged::new(Checkability::Checkable, VerifyVerdict::Abstained, RUNG_FAST)
            }
        }
        VerifyTier::Checker => {
            let Some(inference) = inference else {
                tracing::debug!(%claim, "oicp verify: no checker on this listener");
                return Ok(Judged::could_not(
                    specifics,
                    RUNG_CHECKER,
                    reasons::NO_CHECKER,
                ));
            };
            candidates.truncate(CHECKER_PASSAGES);
            let mut judged = judge_with_checker(&asked, specifics, candidates, inference).await?;
            judged.tally = Some(tally);
            judged
        }
        VerifyTier::Judge => Judged::could_not(specifics, RUNG_CLASS, reasons::TIER_NOT_OFFERED),
    })
}

/// The checker over each candidate in turn, stopping at a clear support, or
/// at a shed: the candidates after it would only queue behind the refusal.
/// A candidate it does not back that states one of the claim's specifics
/// otherwise is `contradicted`; the claim is `supported` on any support, else
/// `contradicted` on any contradiction, else `unsupported` with its
/// candidates, nearest first. `asked` is the claim less its citations
/// and locators, cut once by the caller; the claim's specifics and wire span stay the unit's.
async fn judge_with_checker(
    asked: &str,
    specifics: Vec<Specific>,
    shown: Vec<Passage>,
    inference: &Arc<dyn InferenceProvider>,
) -> Result<Judged, Shed> {
    let mut evidence = Vec::new();
    let mut best: Option<f64> = None;
    let mut answered = 0usize;
    for p in shown {
        let support = match claim_chunk_support_checked(
            inference,
            &p.span.exact,
            asked,
            ShardingPrivacy::LocalOnly,
        )
        .await
        {
            Ok(support) => support,
            Err(sovereign_core::error::Error::QueueShed {
                position,
                predicted_wait_ms,
                retry_after_secs,
            }) => {
                return Err(Shed {
                    position,
                    predicted_wait_ms,
                    retry_after_secs,
                })
            }
            // Logged where it failed (`forced-choice pass failed`).
            Err(_) => None,
        };
        let (verdict, differing) = match support {
            Some(s) if s >= SUPPORTED_TAU => (VerifyVerdict::Supported, Vec::new()),
            Some(_) => {
                let source = sentence_specifics(&p.span.exact);
                let differing = differing_specifics(&specifics, &source, &p.span.exact);
                if differing.is_empty() {
                    (VerifyVerdict::Unsupported, differing)
                } else {
                    (VerifyVerdict::Contradicted, differing)
                }
            }
            None => (
                VerifyVerdict::CouldNotJudge {
                    reason: reasons::CHECKER_DID_NOT_ANSWER.into(),
                },
                Vec::new(),
            ),
        };
        tracing::debug!(
            claim = %asked,
            sentence = %p.span.exact,
            score = p.score,
            support,
            verdict = ?verdict,
            differing = differing.len(),
            "oicp verify: a candidate judged"
        );
        if let Some(s) = support {
            answered += 1;
            best = Some(best.map_or(s, |b| b.max(s)));
        }
        evidence.push(Judgement {
            passage: p,
            verdict,
            score: support.map(|s| s as f32),
            differing,
        });
        if support.is_some_and(|s| s >= EARLY_EXIT_SUPPORT) {
            break;
        }
    }
    let contradicted = evidence
        .iter()
        .any(|e| e.verdict == VerifyVerdict::Contradicted);
    let verdict = match best {
        Some(b) if b >= SUPPORTED_TAU => VerifyVerdict::Supported,
        _ if contradicted => VerifyVerdict::Contradicted,
        Some(_) => VerifyVerdict::Unsupported,
        None if evidence.is_empty() => VerifyVerdict::Unsupported,
        None => VerifyVerdict::CouldNotJudge {
            reason: reasons::CHECKER_DID_NOT_ANSWER.into(),
        },
    };
    tracing::debug!(
        claim = %asked,
        judged = evidence.len(),
        answered,
        best,
        contradicted,
        verdict = ?verdict,
        "oicp verify: checker tier"
    );
    Ok(Judged {
        score: best.map(|b| b as f32),
        specifics,
        evidence,
        ..Judged::new(Checkability::Checkable, verdict, RUNG_CHECKER)
    })
}

/// A specific on the wire: its kind, and its range offset into the text it
/// is read from (`at`, code points), its exact text that range's.
fn wire_specific(s: &Specific, text: &str, at: u64) -> ClaimSpecific {
    ClaimSpecific {
        kind: match s.kind {
            SpecificKind::Figure => ClaimSpecificKind::Figure,
            SpecificKind::Date => ClaimSpecificKind::Date,
            SpecificKind::Quantity => ClaimSpecificKind::Quantity,
            SpecificKind::Name => ClaimSpecificKind::Name,
        },
        start: at + s.range.start as u64,
        end: at + s.range.end as u64,
        exact: text
            .chars()
            .skip(s.range.start)
            .take(s.range.len())
            .collect(),
    }
}

impl Judged {
    fn into_wire(self, range: std::ops::Range<usize>, exact: String) -> VerifiedClaim {
        let at = range.start as u64;
        let evidence = self
            .evidence
            .into_iter()
            .map(|e| JudgedPassage {
                differing: e
                    .differing
                    .iter()
                    .map(|(c, s)| DifferingSpecific {
                        claim: wire_specific(c, &exact, at),
                        source: wire_specific(s, &e.passage.span.exact, e.passage.span.start),
                    })
                    .collect(),
                corpus_id: e.passage.corpus_id,
                span: Some(e.passage.span),
                span_absent: None,
                verdict: e.verdict,
                score: e.score,
            })
            .collect();
        VerifiedClaim {
            start: at,
            end: range.end as u64,
            specifics: self
                .specifics
                .iter()
                .map(|s| wire_specific(s, &exact, at))
                .collect(),
            exact,
            class: self.class,
            verdict: self.verdict,
            evidence,
            specifics_absent: self.specifics_absent,
            rung: self.rung.to_string(),
            score: self.score,
            operating_point: UNCALIBRATED.to_string(),
            episode_id: None,
        }
    }
}
