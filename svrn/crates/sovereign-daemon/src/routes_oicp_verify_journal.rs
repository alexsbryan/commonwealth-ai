// SPDX-License-Identifier: AGPL-3.0-or-later
//! verify's journal (OICP v0.6 §2.10): every claim `POST /oicp/v1/verify`
//! decides is kept as a decision line in the grounding journal under surface
//! [`SURFACE`] (VERIFICATION_COMMONS.md §8 step 1, "keep the episode", for
//! this surface), and `POST /oicp/v1/verify/correction` keeps a client's
//! correction of one, joined by `episode_id` (§2, D0). The hard cases the
//! verifier gets wrong are what the platform learns from, so they are kept
//! where `svrn journal grounding` reads, bundles and switches them off.
//!
//! Lines carry ids, numbers and closed-set tokens only, as every grounding
//! line does: the claim's words stay with the client, which keys them by the
//! `episode_id` its reply carried; evidence is `(corpus, chunk)` handles;
//! a witness is a text's name and a code-point range.
//!
//! A claim the host did not decide writes nothing: a `not_checkable` claim,
//! and `fast`'s `abstained` (presence is not support), the way an ungated
//! turn writes no gate line.

use std::path::{Path, PathBuf};

use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::Json;
use oicp_types::verify::{
    reasons, VerifiedClaim, VerifyCorrection, VerifyCorrectionReply, VerifyVerdict, WitnessCheck,
};
use sovereign_contracts::principal::AttachedPrincipal;
use sovereign_contracts::types::{
    grounding_journal_append, EvidenceRef, GateJudgeVerdict, GroundingCorrectionLine,
    GroundingDecisionLine, GroundingLine, SpecificsTally, GROUNDING_STREAM,
};
use sovereign_tools::summary_verify::SUPPORTED_TAU;

use crate::oicp_evidence::{open_for_reading, readable_corpora};
use crate::routes_internal::ErrorBody;
use crate::state::AppState;

/// The grounding-journal surface verify's decisions are kept under.
pub const SURFACE: &str = "oicp_verify";
/// Where corrections are taken, and what the manifest advertises.
pub const VERIFY_CORRECTION_ENDPOINT: &str = "/oicp/v1/verify/correction";

/// The journal directory verify writes to, layered onto the operator
/// listener's router only (`client_surface::operator_listener_router`), as
/// the checker's judge is. A router without it keeps nothing and refuses
/// corrections with `no journal`, so no test can write into the operator's
/// journal by calling the route.
#[derive(Clone)]
pub struct VerifyJournal(pub PathBuf);

/// Keep `claim` as a decision line, if it was decided and the stream is on;
/// its `episode_id`, to hand back on the claim.
pub(crate) fn record_claim(
    dir: &Path,
    claim: &VerifiedClaim,
    refs: &[(String, Option<u64>)],
    tally: Option<SpecificsTally>,
    ms: u64,
) -> Option<String> {
    let verdict = match &claim.verdict {
        VerifyVerdict::Supported => GateJudgeVerdict::Supported,
        VerifyVerdict::Unsupported => GateJudgeVerdict::Unsupported,
        VerifyVerdict::CouldNotJudge { .. } => GateJudgeVerdict::CouldNotJudge,
        // Not a decision this journal can hold: `abstained` decided nothing,
        // and `contradicted` has no grounding verdict (no rung says it yet).
        VerifyVerdict::Abstained | VerifyVerdict::Contradicted => return None,
    };
    if !GROUNDING_STREAM.enabled(dir) {
        tracing::debug!("oicp verify journal: the stream is off; nothing kept");
        return None;
    }
    // The checker's cut is on support; the line's `tau` is on violation, as
    // the gate's is. `fast` has no threshold: any absent specific decides.
    let tau = if claim.rung.starts_with("checker") {
        1.0 - SUPPORTED_TAU
    } else {
        0.0
    };
    let mut d = GroundingDecisionLine::new(SURFACE, tau, ms);
    d.verdict = verdict;
    d.violation_prob = claim.score.map(|s| 1.0 - f64::from(s));
    d.claim_audited = true;
    d.action = Some(claim.rung.clone());
    d.chunks = refs.len();
    d.evidence = refs
        .iter()
        .filter_map(|(corpus, chunk)| {
            chunk.map(|chunk| EvidenceRef {
                corpus: corpus.clone(),
                chunk,
            })
        })
        .collect();
    d.evidence_unresolved = refs.len() - d.evidence.len();
    d.specifics = tally;
    let episode = d.episode_id.clone();
    let dir = dir.to_path_buf();
    let line = GroundingLine::Decision(d);
    drop(tokio::task::spawn_blocking(move || {
        if let Err(e) = grounding_journal_append(&dir, &line) {
            tracing::warn!(error = %e, "oicp verify journal: append failed");
        }
    }));
    tracing::debug!(%episode, rung = %claim.rung, ?verdict, "oicp verify journal: claim kept");
    Some(episode)
}

type Refusal = (StatusCode, Json<ErrorBody>);

fn refuse(status: StatusCode, error: &str) -> Refusal {
    (
        status,
        Json(ErrorBody {
            error: error.into(),
        }),
    )
}

/// `POST /oicp/v1/verify/correction` (v0.6 §2.10): a client's word that one
/// decision was wrong, kept with what reading its witness back found.
pub async fn verify_correction(
    State(state): State<AppState>,
    principal: Option<Extension<AttachedPrincipal>>,
    journal: Option<Extension<VerifyJournal>>,
    Json(req): Json<VerifyCorrection>,
) -> Result<Json<VerifyCorrectionReply>, Refusal> {
    let Some(Extension(VerifyJournal(dir))) = journal else {
        return Err(refuse(StatusCode::SERVICE_UNAVAILABLE, reasons::NO_JOURNAL));
    };
    if uuid::Uuid::parse_str(&req.episode_id).is_err() {
        return Err(refuse(StatusCode::BAD_REQUEST, reasons::NOT_AN_EPISODE));
    }
    let witness = match &req.witness {
        None => WitnessCheck::NotGiven,
        Some(w) => check_witness(&state, principal.as_ref().map(|Extension(p)| &p.0), w).await,
    };
    let line = GroundingLine::Correction(GroundingCorrectionLine::new(
        &req.episode_id,
        req.expected,
        req.witness.clone(),
        witness,
    ));
    let recorded = tokio::task::spawn_blocking(move || grounding_journal_append(&dir, &line))
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r.map_err(|e| e.to_string()));
    let recorded = match recorded {
        Ok(kept) => kept,
        Err(e) => {
            tracing::warn!(error = %e, "oicp verify correction: append failed");
            return Err(refuse(
                StatusCode::INTERNAL_SERVER_ERROR,
                "journal append failed",
            ));
        }
    };
    tracing::info!(
        episode = %req.episode_id,
        expected = ?req.expected,
        ?witness,
        recorded,
        "oicp verify correction: kept"
    );
    Ok(Json(VerifyCorrectionReply { recorded, witness }))
}

/// Read the witness back: held by a corpus the caller may read, and its
/// range inside the text. One answer for a corpus the caller may not read
/// and one the host does not hold, as everywhere in the evidence extension.
async fn check_witness(
    state: &AppState,
    principal: Option<&sovereign_contracts::principal::Principal>,
    w: &oicp_types::verify::WitnessRef,
) -> WitnessCheck {
    let Some(engine) = state.inner.node.corpus_engine.clone() else {
        return WitnessCheck::NotHeld;
    };
    let installed = match engine.installed_indexes().await {
        Ok(i) => i,
        Err(e) => {
            // The witness cannot be read back, so it is not held as far as
            // this correction can show; the cause is named here.
            tracing::warn!(error = %e, "oicp verify correction: installed corpora unread");
            return WitnessCheck::NotHeld;
        }
    };
    if !readable_corpora(state, principal, &installed).contains(&w.corpus) {
        return WitnessCheck::NotHeld;
    }
    let Some(name) = kernel_types::Sha256Hash::from_hex(&w.text_sha256) else {
        return WitnessCheck::NotHeld;
    };
    let index = match open_for_reading(engine.as_ref(), &w.corpus).await {
        Ok(i) => i,
        Err(e) => {
            tracing::info!(corpus = %w.corpus, error = %e, "oicp verify correction: witness corpus unopened");
            return WitnessCheck::NotHeld;
        }
    };
    match index.text(&name).await {
        Ok(Ok(stored)) => {
            let len = stored.text.chars().count() as u64;
            if w.start <= w.end && w.end <= len {
                WitnessCheck::Verified
            } else {
                WitnessCheck::OutOfRange
            }
        }
        Ok(Err(_)) | Err(_) => WitnessCheck::NotHeld,
    }
}
