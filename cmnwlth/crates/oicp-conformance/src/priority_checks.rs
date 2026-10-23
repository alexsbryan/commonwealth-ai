// SPDX-License-Identifier: AGPL-3.0-or-later
//! v0.6's `knowledge.priority` (oicp-v0.6.md §2.17, §6) over the fixture
//! library: a priority changes when a request's model work starts, never what
//! it is answered, and a priority outside the set is refused. Which request
//! goes first is a timing property no single request shows; the reference
//! host's own test holds it against a fake of the stage it holds at.

use oicp_types::{features, KnowledgeResult, ProviderManifest, RequestPriority};
use serde_json::{json, Value};

use crate::checks::Host;
use crate::evidence_checks::fixture_corpus;
use crate::fixture::Library;
use crate::ingest_checks::FixtureState;
use crate::report::{Check, Level};
use crate::search_checks::{hits, search_body};

/// The three asks a check compares: none, then each priority.
const ASKS: [Option<RequestPriority>; 3] = [
    None,
    Some(RequestPriority::Interactive),
    Some(RequestPriority::Background),
];

fn named(p: Option<RequestPriority>) -> &'static str {
    p.map_or("none", RequestPriority::as_str)
}

fn with_priority(mut body: Value, p: Option<RequestPriority>) -> Value {
    if let Some(p) = p {
        body["priority"] = p.as_str().into();
    }
    body
}

/// What a hit list answers, the parts a priority must not change: each
/// hit's text and content, in order.
fn answered(hits: &[KnowledgeResult]) -> Vec<(Option<String>, String)> {
    hits.iter()
        .map(|h| {
            (
                h.document.as_ref().map(|d| d.text_sha256.clone()),
                h.content.clone(),
            )
        })
        .collect()
}

/// The verdicts of a verify reply, claim by claim; a reply with no claims,
/// or a claim with no verdict, judges nothing to compare.
fn verdicts(body: &Value) -> Result<Vec<String>, String> {
    let claims = body
        .get("claims")
        .and_then(Value::as_array)
        .filter(|c| !c.is_empty())
        .ok_or("a verify reply with no claims")?;
    claims
        .iter()
        .map(|c| {
            c.get("verdict")
                .map(Value::to_string)
                .ok_or_else(|| "a claim with no verdict".to_string())
        })
        .collect()
}

/// Whether a status refuses a request, as §2.17 allows for an unknown value.
fn refused(status: u16) -> bool {
    status == 400 || status == 422
}

pub async fn check_knowledge_priority(
    host: &Host,
    m: &ProviderManifest,
    lib: &Library,
    state: &FixtureState,
) -> Check {
    let id = "knowledge.priority";
    let fail = |why: String| Check::fail(id, Level::Feature, why);
    if !m.has_feature(features::KNOWLEDGE_PRIORITY) {
        return Check::skip(id, Level::Feature, "knowledge:priority not advertised");
    }
    let corpus = match fixture_corpus(id, state) {
        Ok(c) => c,
        Err(skip) => return skip,
    };
    let mut searched = 0usize;
    for signature in lib.signatures.values() {
        let body = json!({"query": signature, "corpora": [corpus], "limit": 5});
        let mut first: Option<Vec<(Option<String>, String)>> = None;
        for p in ASKS {
            let got = match hits(host, m, &with_priority(body.clone(), p)).await {
                Ok(h) => answered(&h),
                Err(e) => return fail(format!("priority {}: {e}", named(p))),
            };
            match &first {
                None => first = Some(got),
                Some(was) if *was != got => {
                    return fail(format!(
                        "the signature {signature:?} is answered differently with priority {} \
                         than with none",
                        named(p)
                    ))
                }
                Some(_) => {}
            }
        }
        searched += 1;
    }
    let bad = json!({"query": "q", "corpora": [corpus], "priority": "urgent"});
    match search_body(host, m, &bad).await {
        Ok(r) if refused(r.status) => {}
        Ok(r) => {
            return fail(format!(
                "a priority outside the set must be 400 or 422, got {}",
                r.brief()
            ))
        }
        Err(e) => return fail(e),
    }
    let verified = match verify_alike(host, m, lib, &corpus).await {
        Ok(v) => v,
        Err(why) => return fail(why),
    };
    Check::pass(
        id,
        Level::Feature,
        format!(
            "{searched} signature(s) answered alike with each priority and none{verified}; \
             an unknown priority is refused"
        ),
    )
}

/// With `evidence:verify`, the fixture unit verified at the first served
/// tier with each priority and none returns the same verdicts. The tail of
/// the pass line, or why it failed.
async fn verify_alike(
    host: &Host,
    m: &ProviderManifest,
    lib: &Library,
    corpus: &str,
) -> Result<String, String> {
    if !m.has_feature(features::EVIDENCE_VERIFY) {
        return Ok(String::new());
    }
    let ev = m.knowledge.as_ref().and_then(|k| k.evidence.as_ref());
    let (Some(endpoint), Some(tier)) = (
        ev.and_then(|e| e.verify_endpoint.clone()),
        ev.and_then(|e| e.verify_tiers.first().copied()),
    ) else {
        return Err("evidence:verify advertised with no endpoint or tier".into());
    };
    let (planted, _) = lib.planted_quote();
    let unit = format!("{} {planted}", lib.plant.sentence);
    let body = json!({"unit": unit, "corpora": [corpus], "tier": tier});
    let mut first: Option<Vec<String>> = None;
    for p in ASKS {
        let r = host
            .post(&endpoint, &with_priority(body.clone(), p))
            .await?;
        if r.status != 200 {
            return Err(format!("verify with priority {}: {}", named(p), r.brief()));
        }
        let got = verdicts(&r.body).map_err(|e| format!("priority {}: {e}", named(p)))?;
        match &first {
            None => first = Some(got),
            Some(was) if *was != got => {
                return Err(format!(
                    "the fixture unit is judged {got:?} with priority {}, {was:?} with none",
                    named(p)
                ))
            }
            Some(_) => {}
        }
    }
    Ok(format!(
        ", the fixture unit judged alike at {}",
        tier.as_str()
    ))
}
