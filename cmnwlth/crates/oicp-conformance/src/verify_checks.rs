// SPDX-License-Identifier: AGPL-3.0-or-later
//! v0.6's verify check (oicp-v0.6.md §2.6, §6): a unit of the fixture
//! library's sentences judged at every tier the host advertises. The check
//! holds the shape, never the verdicts' quality: claims that are exact ranges
//! of the unit, verdicts from the closed set (the types refuse any other),
//! passages that dereference to their `exact`, and the named refusal of a
//! tier the host does not serve.

use oicp_types::verify::{reasons, VerifyResponse, VerifyTier};
use oicp_types::{features, ProviderManifest};
use serde_json::json;

use crate::checks::Host;
use crate::evidence_checks::{fixture_corpus, read};
use crate::fixture::{code_points, Library};
use crate::ingest_checks::FixtureState;
use crate::report::{Check, Level};

/// `evidence.verify` (§2.6).
pub async fn check_evidence_verify(
    host: &Host,
    m: &ProviderManifest,
    lib: &Library,
    state: &FixtureState,
) -> Check {
    let id = "evidence.verify";
    let fail = |why: String| Check::fail(id, Level::Feature, why);
    if !m.has_feature(features::EVIDENCE_VERIFY) {
        return Check::skip(id, Level::Feature, "evidence:verify not advertised");
    }
    let Some(ev) = m.knowledge.as_ref().and_then(|k| k.evidence.as_ref()) else {
        return fail("evidence:verify advertised but no knowledge.evidence".into());
    };
    let Some(endpoint) = ev.verify_endpoint.clone() else {
        return fail("evidence:verify advertised but no verify_endpoint".into());
    };
    if ev.verify_tiers.is_empty() {
        return fail("evidence:verify advertised with no verify_tiers".into());
    }
    let corpus = match fixture_corpus(id, state) {
        Ok(c) => c,
        Err(skip) => return skip,
    };
    let (planted, _) = lib.planted_quote();
    let unit = format!("{} {planted}", lib.plant.sentence);
    let corpus_q = format!("?corpus={corpus}");
    let mut judged = 0usize;
    let mut passages = 0usize;
    for tier in &ev.verify_tiers {
        let r = match host
            .post(
                &endpoint,
                &json!({"unit": unit, "corpora": [corpus], "tier": tier}),
            )
            .await
        {
            Ok(r) => r,
            Err(e) => return fail(e),
        };
        if r.status != 200 {
            return fail(format!("verify at {}: {}", tier.as_str(), r.brief()));
        }
        let reply: VerifyResponse = match serde_json::from_value(r.body.clone()) {
            Ok(x) => x,
            Err(e) => {
                return fail(format!(
                    "the {} reply does not decode (a verdict outside the five?): {e}",
                    tier.as_str()
                ))
            }
        };
        if reply.verifier.is_empty() {
            return fail("the reply names no verifier".into());
        }
        if reply.claims.is_empty() {
            return fail(format!(
                "{} returned no claims for a two-sentence unit",
                tier.as_str()
            ));
        }
        let mut last_end = 0;
        for c in &reply.claims {
            if c.start < last_end || c.end < c.start {
                return fail(format!(
                    "claims out of order or overlapping at [{}, {})",
                    c.start, c.end
                ));
            }
            last_end = c.end;
            if code_points(&unit, c.start, c.end).as_deref() != Some(c.exact.as_str()) {
                return fail(format!(
                    "claim [{}, {}) says {:?}; the unit there says {:?}",
                    c.start,
                    c.end,
                    c.exact,
                    code_points(&unit, c.start, c.end)
                ));
            }
            for p in &c.evidence {
                let Some(span) = &p.span else { continue };
                let whole = match read(
                    host,
                    &ev.text_endpoint,
                    &span.document.text_sha256,
                    &corpus_q,
                )
                .await
                {
                    Ok(s) => s,
                    Err(e) => return fail(e),
                };
                if code_points(&whole.text, span.start, span.end).as_deref()
                    != Some(span.exact.as_str())
                {
                    return fail(format!(
                        "a passage's span [{}, {}) says {:?}; the text there says {:?}",
                        span.start,
                        span.end,
                        span.exact,
                        code_points(&whole.text, span.start, span.end)
                    ));
                }
                passages += 1;
            }
            judged += 1;
        }
    }
    let unserved = [VerifyTier::Fast, VerifyTier::Checker, VerifyTier::Judge]
        .into_iter()
        .find(|t| !ev.verify_tiers.contains(t));
    if let Some(t) = unserved {
        match host
            .post(
                &endpoint,
                &json!({"unit": unit, "corpora": [corpus], "tier": t}),
            )
            .await
        {
            Ok(r) if r.status == 422 && r.error() == Some(reasons::TIER_NOT_OFFERED) => {}
            Ok(r) => {
                return fail(format!(
                    "an unserved tier ({}) must be 422 `{}`, got {}",
                    t.as_str(),
                    reasons::TIER_NOT_OFFERED,
                    r.brief()
                ))
            }
            Err(e) => return fail(e),
        }
    }
    Check::pass(
        id,
        Level::Feature,
        format!(
            "{judged} claim(s) over {} tier(s) are exact ranges of the unit; {passages} passage \
             span(s) dereference exactly{}",
            ev.verify_tiers.len(),
            if unserved.is_some() {
                "; the unserved tier is refused by name"
            } else {
                ""
            }
        ),
    )
}
