// SPDX-License-Identifier: AGPL-3.0-or-later
//! The fake's verify routes (v0.6 §2.6, §2.10). A child of `fake_host`, so
//! it reads the fake's private state.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use oicp_types::Span;
use serde_json::json;

use super::{refuse, Fake};
use crate::fixture::{code_points, sha256_hex};

/// The lawful verify: each sentence of the unit is a claim, `supported` by
/// the stored text it appears in verbatim (that occurrence is its passage),
/// `unsupported` otherwise; the judge tier is not served. Spans take
/// `exact_off_by_one` like align's.
pub(super) async fn verify(
    State(f): State<Arc<Fake>>,
    Json(req): Json<oicp_types::verify::VerifyRequest>,
) -> Response {
    use oicp_types::verify::{reasons as vr, VerifyTier, UNCALIBRATED};
    if req.tier == VerifyTier::Judge {
        return refuse(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"error": vr::TIER_NOT_OFFERED}),
        );
    }
    let unit: Vec<char> = req.unit.chars().collect();
    let mut claims = Vec::new();
    let mut start = 0usize;
    for i in 0..unit.len() {
        let stop = unit[i] == '.' && unit.get(i + 1).is_none_or(|c| c.is_whitespace());
        if !stop {
            continue;
        }
        let exact: String = unit[start..=i].iter().collect();
        let lead = exact.len() - exact.trim_start().len();
        let (a, b) = ((start + lead) as u64, (i + 1) as u64);
        let exact = exact.trim_start().to_string();
        start = i + 1;
        let found = f.of(&req.corpora).find_map(|s| {
            s.text
                .find(&exact)
                .map(|byte| (s, s.text[..byte].chars().count() as u64))
        });
        let shift = u64::from(f.breaks.exact_off_by_one);
        let (verdict, evidence) = match found {
            Some((s, at)) => {
                let end = at + exact.chars().count() as u64;
                let span = Span {
                    corpus_id: s.corpus.clone(),
                    document: f.record(s),
                    start: at,
                    end,
                    exact: code_points(&s.text, at + shift, end + shift).unwrap_or_default(),
                    prefix: String::new(),
                    suffix: String::new(),
                    locator: f.locator(s, at),
                };
                (
                    json!({"kind": "supported"}),
                    json!([{"corpus_id": s.corpus, "span": span, "verdict": {"kind": "supported"}}]),
                )
            }
            None => (json!({"kind": "unsupported"}), json!([])),
        };
        let verdict = if f.breaks.verify_unknown_verdict {
            json!({"kind": "plausible"})
        } else {
            verdict
        };
        claims.push(json!({
            "start": a, "end": b, "exact": exact, "class": "checkable",
            "verdict": verdict, "evidence": evidence, "rung": "fake/verbatim",
            "operating_point": UNCALIBRATED,
            "episode_id": format!("00000000-0000-4000-8000-{:012x}", claims.len()),
        }));
    }
    Json(json!({
        "claims": claims,
        "verifier": "fake-verify/1",
        "corpora": f.searched(&req.corpora).into_iter()
            .map(|c| json!({"corpus_id": c, "texts_digest": f.digest(&c)}))
            .collect::<Vec<_>>(),
    }))
    .into_response()
}

/// A UUID's shape: 8-4-4-4-12 hex digits.
fn uuid_shaped(s: &str) -> bool {
    let groups: Vec<&str> = s.split('-').collect();
    groups.iter().map(|g| g.len()).eq([8, 4, 4, 4, 12])
        && groups
            .iter()
            .all(|g| g.chars().all(|c| c.is_ascii_hexdigit()))
}

/// The lawful correction (v0.6 §2.10): a malformed episode is 400, and the
/// witness is read back against the fake's texts. `witness_unread` calls
/// every witness `verified`.
pub(super) async fn verify_correction(
    State(f): State<Arc<Fake>>,
    Json(req): Json<oicp_types::verify::VerifyCorrection>,
) -> Response {
    use oicp_types::verify::{reasons as vr, WitnessCheck};
    if !uuid_shaped(&req.episode_id) {
        return refuse(
            StatusCode::BAD_REQUEST,
            json!({"error": vr::NOT_AN_EPISODE}),
        );
    }
    let witness = match &req.witness {
        None => WitnessCheck::NotGiven,
        Some(_) if f.breaks.witness_unread => WitnessCheck::Verified,
        Some(w) => match f
            .texts
            .iter()
            .find(|s| sha256_hex(s.text.as_bytes()) == w.text_sha256)
        {
            None => WitnessCheck::NotHeld,
            Some(s) if w.start <= w.end && w.end <= s.text.chars().count() as u64 => {
                WitnessCheck::Verified
            }
            Some(_) => WitnessCheck::OutOfRange,
        },
    };
    Json(json!({"recorded": true, "witness": witness})).into_response()
}
