// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /oicp/v1/verify` — a unit of text judged claim by claim against the
//! stored texts the caller may read (OICP v0.6 §2.6), outside any turn.
//!
//! Built from what already runs (CITE_V1 §1.4, P4), never a new verifier:
//! - claims are the unit's sentences, and a claim is checkable when it
//!   carries a specific (`sovereign_core::runtime::sentence_specifics`);
//! - **fast** asks, with no model, whether each specific appears in any
//!   passage lexical search returns for the claim or for the specific itself
//!   (`value_present_in_chunks`, the in-turn value veto's deterministic half).
//!   A specific in no passage is `unsupported`; otherwise the tier
//!   `abstained`, because presence is not support;
//! - **checker** judges the claim against each passage in the gate's own
//!   per-claim register (`claim_chunk_support`), at the faithfulness lane's
//!   threshold. That register says support or not, so this tier never says
//!   `contradicted`.
//!
//! Candidates come from each corpus's lexical index as align's do, so no
//! embedding model is involved. A passage's span is located by finding its
//! chunk body in the stored text (`oicp_evidence::span_of`, the locator
//! search's hits share); a passage that is not an exact slice says so instead
//! of carrying a range. Every operating point is `uncalibrated` until the
//! bench has measured a cell.

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::Json;
use corpus_index::index::CorpusIndex;
use kernel_types::Sha256Hash;
use oicp_types::evidence::{CorpusTexts, Span, Unavailable};
use oicp_types::verify::{
    reasons, Checkability, JudgedPassage, VerifiedClaim, VerifyRequest, VerifyResponse, VerifyTier,
    VerifyVerdict, UNCALIBRATED,
};
use quote_align::code_point_slice;
use sovereign_contracts::principal::AttachedPrincipal;
use sovereign_contracts::types::SpecificsTally;
use sovereign_core::oicp::ShardingPrivacy;
use sovereign_core::runtime::{
    claim_chunk_support, sentence_ranges, sentence_specifics, value_present_in_chunks,
};
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::Speed;
use sovereign_tools::summary_verify::{EARLY_EXIT_SUPPORT, SUPPORTED_TAU};

use crate::oicp_evidence::{
    absence_reason, open_for_reading, readable_corpora, requested_corpora, span_of,
};
use crate::routes_internal::ErrorBody;
pub use crate::routes_oicp_verify_journal::VerifyJournal;
use crate::state::AppState;

/// Where the route is mounted, and what the manifest advertises.
pub const VERIFY_ENDPOINT: &str = "/oicp/v1/verify";
/// The tiers this host serves, as the manifest advertises them.
pub const TIERS: [VerifyTier; 2] = [VerifyTier::Fast, VerifyTier::Checker];
/// Lexical candidates read per query per corpus.
const CANDIDATES_K: usize = 8;
/// Passages the checker judges per claim, best lexical candidates first. The
/// judge reads them one at a time, so this is the tier's latency: at the
/// gate's `MEMBER_CAP` (12, so all 8 candidates) X1's draft took 4.0-8.5 s a
/// claim warm (n=4, 2026-10-12), over the S6 order's 5 s line; operator chose
/// 3 the same day. With several corpora the first corpus's candidates fill it.
pub const CHECKER_PASSAGES: usize = 3;
/// The rung names on the wire: a change to what a rung does changes its name.
const RUNG_CLASS: &str = "class/no-specifics";
const RUNG_FAST: &str = "fast/value-presence";
const RUNG_CHECKER: &str = "checker/chunk-support";

/// The model the checker judges with: layered onto the operator listener's
/// router only (`client_surface::operator_listener_router`). A listener
/// without it answers every `checker` claim `could_not_judge: no checker`;
/// `fast` needs no model and answers everywhere `verify` is mounted.
#[derive(Clone)]
pub struct VerifyJudge(pub Arc<dyn InferenceProvider>);

type Refusal = (StatusCode, Json<ErrorBody>);

fn refuse(status: StatusCode, error: impl Into<String>) -> Refusal {
    (
        status,
        Json(ErrorBody {
            error: error.into(),
        }),
    )
}

/// One passage lexical search returned.
struct Passage {
    corpus_id: String,
    chunk_id: Option<u64>,
    source_doc_id: Option<String>,
    title: Option<String>,
    content: String,
}

/// `POST /oicp/v1/verify`.
pub async fn verify(
    State(state): State<AppState>,
    principal: Option<Extension<AttachedPrincipal>>,
    judge: Option<Extension<VerifyJudge>>,
    journal: Option<Extension<VerifyJournal>>,
    Json(req): Json<VerifyRequest>,
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
    let readable = readable_corpora(
        &state,
        principal.as_ref().map(|Extension(p)| &p.0),
        &installed,
    );
    let (targets, mut corpora_unavailable) = requested_corpora(&req.corpora, &readable, "verify");

    let mut corpora: Vec<CorpusTexts> = Vec::new();
    let mut open: Vec<(String, CorpusIndex)> = Vec::new();
    for corpus_id in targets {
        match open_with_digest(engine.as_ref(), &corpus_id).await {
            Ok((index, digest)) => {
                corpora.push(CorpusTexts {
                    corpus_id: corpus_id.clone(),
                    texts_digest: digest.to_hex(),
                });
                open.push((corpus_id, index));
            }
            Err(reason) => {
                tracing::info!(corpus = %corpus_id, %reason, "oicp verify: corpus unavailable");
                corpora_unavailable.push(Unavailable { corpus_id, reason });
            }
        }
    }

    let inference = judge.map(|Extension(VerifyJudge(i))| i);
    let verifier = verifier_identity(req.tier, inference.as_deref());
    let mut texts = TextCache::default();
    let context = req.effective_span_context() as usize;
    let mut claims = Vec::new();
    for range in sentence_ranges(&req.unit) {
        // `range` is one of `sentence_ranges(&req.unit)`, so it is in range.
        let exact = code_point_slice(&req.unit, range.clone())
            .unwrap_or_default()
            .to_string();
        let judged_at = std::time::Instant::now();
        let judged = judge_claim(&exact, req.tier, &open, inference.as_ref()).await;
        let judged_ms = judged_at.elapsed().as_millis() as u64;
        let refs: Vec<(String, Option<u64>)> = judged
            .evidence
            .iter()
            .map(|(p, _, _)| (p.corpus_id.clone(), p.chunk_id))
            .collect();
        let tally = judged.tally;
        let mut claim = judged
            .into_wire(range, exact, &open, &mut texts, context)
            .await;
        if let Some(Extension(VerifyJournal(dir))) = &journal {
            claim.episode_id = crate::routes_oicp_verify_journal::record_claim(
                dir, &claim, &refs, tally, judged_ms,
            );
        }
        tracing::debug!(
            claim = %claim.exact,
            verdict = ?claim.verdict,
            rung = %claim.rung,
            evidence = claim.evidence.len(),
            absent = ?claim.specifics_absent,
            "oicp verify: claim judged"
        );
        claims.push(claim);
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

/// Who judged: the route, its rung for the tier, and for the checker the
/// threshold, the passages it judges and the model its slot names
/// (`model_id_for(Slow)`, the slot the judge workload prefers; routing may
/// serve it elsewhere, which this does not see).
fn verifier_identity(tier: VerifyTier, inference: Option<&dyn InferenceProvider>) -> String {
    match tier {
        VerifyTier::Checker => format!(
            "verify/1 {RUNG_CHECKER} tau={SUPPORTED_TAU} passages={CHECKER_PASSAGES} model={}",
            inference.map_or_else(|| "none".to_string(), |i| i.model_id_for(Speed::Slow))
        ),
        _ => format!("verify/1 {RUNG_FAST}"),
    }
}

/// A corpus's index and the digest of its texts, or why it cannot be read.
async fn open_with_digest(
    engine: &dyn corpus_index::ingest_port::daemon::IngestPort,
    corpus_id: &str,
) -> Result<(CorpusIndex, Sha256Hash), String> {
    let index = open_for_reading(engine, corpus_id)
        .await
        .map_err(|e| format!("corpus unreadable: {e}"))?;
    match engine.texts_digest(&index).await {
        Ok(Ok(d)) => Ok((index, d)),
        Ok(Err(absence)) => Err(absence_reason(absence).into()),
        Err(e) => Err(format!("texts unreadable: {e}")),
    }
}

/// The lexical candidates for `query` across `open`, best first per corpus.
async fn candidates(
    open: &[(String, CorpusIndex)],
    query: &str,
    k: usize,
) -> Result<Vec<Passage>, String> {
    let mut out = Vec::new();
    for (corpus_id, index) in open {
        let hits = index
            .search(&[], query, k)
            .await
            .map_err(|e| format!("lexical search failed on {corpus_id}: {e}"))?;
        out.extend(hits.into_iter().map(|h| Passage {
            corpus_id: corpus_id.clone(),
            chunk_id: h.chunk_id,
            source_doc_id: h.source_doc_id,
            title: h.title,
            content: h.content,
        }));
    }
    Ok(out)
}

/// A claim's verdict before its passages are located.
struct Judged {
    class: Checkability,
    verdict: VerifyVerdict,
    rung: &'static str,
    score: Option<f32>,
    specifics_absent: Vec<String>,
    evidence: Vec<(Passage, VerifyVerdict, Option<f32>)>,
    /// The claim's specifics against the passages read, for the journal.
    tally: Option<SpecificsTally>,
}

impl Judged {
    fn could_not(class: Checkability, rung: &'static str, reason: impl Into<String>) -> Self {
        Judged {
            class,
            verdict: VerifyVerdict::CouldNotJudge {
                reason: reason.into(),
            },
            rung,
            score: None,
            specifics_absent: Vec::new(),
            evidence: Vec::new(),
            tally: None,
        }
    }
}

async fn judge_claim(
    claim: &str,
    tier: VerifyTier,
    open: &[(String, CorpusIndex)],
    inference: Option<&Arc<dyn InferenceProvider>>,
) -> Judged {
    let specifics = sentence_specifics(claim);
    if specifics.is_empty() {
        return Judged {
            class: Checkability::NotCheckable,
            verdict: VerifyVerdict::Abstained,
            rung: RUNG_CLASS,
            score: None,
            specifics_absent: Vec::new(),
            evidence: Vec::new(),
            tally: None,
        };
    }
    let checkable = Checkability::Checkable;
    if open.is_empty() {
        return Judged::could_not(checkable, RUNG_CLASS, reasons::NO_CORPUS_READ);
    }
    match tier {
        VerifyTier::Fast => {
            let shown = match candidates(open, claim, CANDIDATES_K).await {
                Ok(p) => p,
                Err(e) => return Judged::could_not(checkable, RUNG_FAST, e),
            };
            let mut read: Vec<String> = shown.iter().map(|p| p.content.clone()).collect();
            for s in &specifics {
                match candidates(open, &s.text, CANDIDATES_K).await {
                    Ok(p) => read.extend(p.into_iter().map(|p| p.content)),
                    Err(e) => return Judged::could_not(checkable, RUNG_FAST, e),
                }
            }
            let absent: Vec<String> = specifics
                .iter()
                .filter(|s| !value_present_in_chunks(&s.text, &read))
                .map(|s| s.text.clone())
                .collect();
            let score = 1.0 - absent.len() as f32 / specifics.len() as f32;
            let verdict = if absent.is_empty() {
                VerifyVerdict::Abstained
            } else {
                VerifyVerdict::Unsupported
            };
            tracing::debug!(
                %claim,
                specifics = specifics.len(),
                passages_read = read.len(),
                ?absent,
                "oicp verify: fast tier"
            );
            Judged {
                class: checkable,
                evidence: shown
                    .into_iter()
                    .map(|p| (p, verdict.clone(), None))
                    .collect(),
                verdict,
                rung: RUNG_FAST,
                score: Some(score),
                tally: Some(SpecificsTally {
                    checked: specifics.len() as u32,
                    absent: absent.len() as u32,
                }),
                specifics_absent: absent,
            }
        }
        VerifyTier::Checker => {
            let Some(inference) = inference else {
                return Judged::could_not(checkable, RUNG_CHECKER, reasons::NO_CHECKER);
            };
            let mut shown = match candidates(open, claim, CANDIDATES_K).await {
                Ok(p) => p,
                Err(e) => return Judged::could_not(checkable, RUNG_CHECKER, e),
            };
            shown.truncate(CHECKER_PASSAGES);
            let read: Vec<String> = shown.iter().map(|p| p.content.clone()).collect();
            let tally = SpecificsTally {
                checked: specifics.len() as u32,
                absent: specifics
                    .iter()
                    .filter(|s| !value_present_in_chunks(&s.text, &read))
                    .count() as u32,
            };
            let mut judged = judge_with_checker(claim, shown, inference).await;
            judged.tally = Some(tally);
            judged
        }
        VerifyTier::Judge => Judged::could_not(checkable, RUNG_CLASS, reasons::TIER_NOT_OFFERED),
    }
}

/// The checker over each passage in turn, stopping at a clear support.
async fn judge_with_checker(
    claim: &str,
    shown: Vec<Passage>,
    inference: &Arc<dyn InferenceProvider>,
) -> Judged {
    let mut evidence = Vec::new();
    let mut best: Option<f64> = None;
    let mut answered = 0usize;
    for p in shown {
        let support =
            claim_chunk_support(inference, &p.content, claim, ShardingPrivacy::LocalOnly).await;
        let verdict = match support {
            Some(s) if s >= SUPPORTED_TAU => VerifyVerdict::Supported,
            Some(_) => VerifyVerdict::Unsupported,
            None => VerifyVerdict::CouldNotJudge {
                reason: reasons::CHECKER_DID_NOT_ANSWER.into(),
            },
        };
        if let Some(s) = support {
            answered += 1;
            best = Some(best.map_or(s, |b| b.max(s)));
        }
        evidence.push((p, verdict, support.map(|s| s as f32)));
        if support.is_some_and(|s| s >= EARLY_EXIT_SUPPORT) {
            break;
        }
    }
    let verdict = match best {
        Some(b) if b >= SUPPORTED_TAU => VerifyVerdict::Supported,
        Some(_) => VerifyVerdict::Unsupported,
        None if evidence.is_empty() => VerifyVerdict::Unsupported,
        None => VerifyVerdict::CouldNotJudge {
            reason: reasons::CHECKER_DID_NOT_ANSWER.into(),
        },
    };
    tracing::debug!(
        %claim,
        judged = evidence.len(),
        answered,
        best,
        "oicp verify: checker tier"
    );
    Judged {
        class: Checkability::Checkable,
        verdict,
        rung: RUNG_CHECKER,
        score: best.map(|b| b as f32),
        specifics_absent: Vec::new(),
        evidence,
        tally: None,
    }
}

/// Text names and stored texts read once per request, each with the reason
/// it could not be read.
#[derive(Default)]
struct TextCache {
    by_chunk: HashMap<(String, u64), Result<Sha256Hash, String>>,
    texts: HashMap<(String, Sha256Hash), Result<Arc<corpus_index::index::StoredText>, String>>,
}

impl Judged {
    async fn into_wire(
        self,
        range: std::ops::Range<usize>,
        exact: String,
        open: &[(String, CorpusIndex)],
        texts: &mut TextCache,
        context: usize,
    ) -> VerifiedClaim {
        let mut evidence = Vec::with_capacity(self.evidence.len());
        for (p, verdict, score) in self.evidence {
            let (span, span_absent) = match locate(&p, open, texts, context).await {
                Ok(span) => (Some(span), None),
                Err(why) => (None, Some(why)),
            };
            evidence.push(JudgedPassage {
                corpus_id: p.corpus_id,
                span,
                span_absent,
                verdict,
                score,
            });
        }
        VerifiedClaim {
            start: range.start as u64,
            end: range.end as u64,
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

/// `p` as a span of its stored text, or why it is not one: the passage is
/// not an exact slice of its text, or names no text, or its text or record
/// could not be read, each by its own reason, never one standing for another.
async fn locate(
    p: &Passage,
    open: &[(String, CorpusIndex)],
    cache: &mut TextCache,
    context: usize,
) -> Result<Span, String> {
    let names_no_text = || reasons::PASSAGE_NAMES_NO_TEXT.to_string();
    let index = open
        .iter()
        .find(|(c, _)| *c == p.corpus_id)
        .map(|(_, i)| i)
        .ok_or_else(names_no_text)?;
    let chunk = p.chunk_id.ok_or_else(names_no_text)?;
    let key = (p.corpus_id.clone(), chunk);
    if !cache.by_chunk.contains_key(&key) {
        let name = match index.chunk_text_sha256s(&[chunk]).await {
            Ok(n) => n.get(&chunk).copied().ok_or_else(names_no_text),
            Err(e) => {
                tracing::warn!(corpus = %p.corpus_id, chunk, error = %e, "oicp verify: chunk text names unread");
                Err(format!("text names unread: {e}"))
            }
        };
        cache.by_chunk.insert(key.clone(), name);
    }
    let name = cache.by_chunk[&key].clone()?;
    let tkey = (p.corpus_id.clone(), name);
    if !cache.texts.contains_key(&tkey) {
        let stored = match index.text(&name).await {
            Ok(Ok(s)) => Ok(Arc::new(s)),
            Ok(Err(absence)) => {
                tracing::debug!(corpus = %p.corpus_id, %name, reason = absence_reason(absence), "oicp verify: a passage's text is not readable");
                Err(absence_reason(absence).to_string())
            }
            Err(e) => {
                tracing::warn!(corpus = %p.corpus_id, %name, error = %e, "oicp verify: text store unreadable");
                Err(format!("text store unreadable: {e}"))
            }
        };
        cache.texts.insert(tkey.clone(), stored);
    }
    let stored = cache.texts[&tkey].clone()?;
    span_of(
        &stored,
        &p.corpus_id,
        p.title.as_deref(),
        &p.content,
        p.source_doc_id.as_deref(),
        context,
    )
    .map_err(str::to_string)
}
