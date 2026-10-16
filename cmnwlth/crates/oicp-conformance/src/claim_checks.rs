// SPDX-License-Identifier: AGPL-3.0-or-later
//! v0.6's verify-by-meaning check (oicp-v0.6.md §2.15, §6):
//! `evidence.verify_sentences`. It installs its own shelf of figure
//! sentences and verifies units against it: each claim names its specifics
//! by kind, a citation's year and a locator's number are none, a figure in
//! any other parentheses is one; the fast tier's literal miss decides
//! nothing; a contradiction names the specific that differs, read back
//! through the text endpoint.

use std::collections::BTreeMap;

use oicp_types::verify::{VerifiedClaim, VerifyResponse, VerifyTier, VerifyVerdict};
use oicp_types::{features, ClaimSpecificKind, ProviderManifest};
use serde::Deserialize;
use serde_json::json;

use crate::checks::Host;
use crate::evidence_checks::read;
use crate::fixture::code_points;
use crate::report::{Check, Level};
use crate::shelf::Shelf;

/// The claims shelf: its library, and the units verified against it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimsShelf {
    /// The library.
    pub shelf: Shelf,
    /// The units, each one claim.
    pub claims: Vec<ClaimCase>,
}

/// A unit and what a host must say of it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimCase {
    /// The unit, one sentence.
    pub unit: String,
    /// Text a figure, date or quantity specific must cover (first
    /// occurrence in the unit).
    pub specifics: Vec<String>,
    /// Text no specific may overlap: a citation, a locator.
    #[serde(default)]
    pub none_within: Vec<String>,
    /// The verdict at `fast`, where the host serves it.
    #[serde(default)]
    pub fast: Option<Expect>,
    /// The verdict at `checker`, where the host serves it.
    #[serde(default)]
    pub checker: Option<Expect>,
}

/// A verdict a case expects of its claim.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    /// The verdict's kind.
    pub verdict: String,
    /// Whether the claim carries evidence; unasserted when absent.
    #[serde(default)]
    pub evidence: Option<bool>,
    /// Pairs (the claim's text, the source's text) some contradicted
    /// passage names as differing.
    #[serde(default)]
    pub differing: Vec<[String; 2]>,
}

/// The claims shelf, read from `fixture/claims.json`. The one reader: the
/// checks that need figure sentences a host's verify decides install this
/// shelf rather than carry a shelf of their own.
pub fn claims_fixture() -> Result<ClaimsShelf, String> {
    serde_json::from_str(include_str!("../fixture/claims.json"))
        .map_err(|e| format!("fixture/claims.json does not load: {e}"))
}

/// `evidence.verify_sentences` (§2.15).
pub async fn check_evidence_verify_sentences(host: &Host, m: &ProviderManifest) -> Check {
    let id = "evidence.verify_sentences";
    let fail = |why: String| Check::fail(id, Level::Feature, why);
    if !m.has_feature(features::EVIDENCE_VERIFY_SENTENCES) {
        return Check::skip(
            id,
            Level::Feature,
            "evidence:verify_sentences not advertised",
        );
    }
    let ev = m.knowledge.as_ref().and_then(|k| k.evidence.as_ref());
    let Some((ev, endpoint)) = ev.and_then(|e| Some((e, e.verify_endpoint.clone()?))) else {
        return fail("evidence:verify_sentences advertised without a verify_endpoint".into());
    };
    if !m.has_feature(features::INGEST_RECIPE) {
        return Check::skip(
            id,
            Level::Feature,
            "could not judge: the claims shelf is installed by ingest:recipe",
        );
    }
    let fx = match claims_fixture() {
        Ok(x) => x,
        Err(e) => return fail(e),
    };
    if let Err(e) = fx.shelf.install(host, m, &[]).await {
        return fail(e);
    }
    let corpus = &fx.shelf.corpus_id;
    let corpus_q = format!("?corpus={corpus}");
    let mut texts: BTreeMap<String, String> = BTreeMap::new();
    let (mut judged, mut unjudged) = (0usize, None);
    for case in &fx.claims {
        for tier in [VerifyTier::Fast, VerifyTier::Checker] {
            if !ev.verify_tiers.contains(&tier) {
                continue;
            }
            let body = json!({"unit": case.unit, "corpora": [corpus], "tier": tier});
            let r = match host.post(&endpoint, &body).await {
                Ok(r) if r.status == 200 => r,
                Ok(r) => return fail(format!("verify at {}: {}", tier.as_str(), r.brief())),
                Err(e) => return fail(e),
            };
            let reply: VerifyResponse = match serde_json::from_value(r.body) {
                Ok(x) => x,
                Err(e) => return fail(format!("the verify reply does not decode: {e}")),
            };
            let expect = match tier {
                VerifyTier::Fast => case.fast.as_ref(),
                _ => case.checker.as_ref(),
            };
            let bad = claim_reply_failures(case, tier, expect, &reply.claims);
            if !bad.is_empty() {
                return fail(bad.join("; "));
            }
            for p in reply.claims.iter().flat_map(|c| &c.evidence) {
                let (Some(span), false) = (&p.span, p.differing.is_empty()) else {
                    continue;
                };
                let name = &span.document.text_sha256;
                if !texts.contains_key(name) {
                    match read(host, &ev.text_endpoint, name, &corpus_q).await {
                        Ok(w) => texts.insert(name.clone(), w.text),
                        Err(e) => return fail(e),
                    };
                }
                for d in &p.differing {
                    let there = code_points(&texts[name], d.source.start, d.source.end);
                    if there.as_deref() != Some(d.source.exact.as_str()) {
                        return fail(format!(
                            "a differing source specific [{}, {}) says {:?}; the text there \
                             says {there:?}",
                            d.source.start, d.source.end, d.source.exact
                        ));
                    }
                }
            }
            let target = target(case, &reply.claims);
            if let Some(VerifyVerdict::CouldNotJudge { reason }) = target.map(|c| &c.verdict) {
                unjudged = Some(format!(
                    "{} answered could_not_judge: {reason}",
                    tier.as_str()
                ));
            }
            judged += 1;
        }
    }
    if let Some(why) = unjudged {
        return Check::skip(
            id,
            Level::Feature,
            format!("could not judge every verdict: {why}"),
        );
    }
    Check::pass(
        id,
        Level::Feature,
        format!(
            "{judged} verify reply(ies) over {} unit(s) name their specifics by kind, decide \
             nothing from the fast tier's miss, and name what differs",
            fx.claims.len()
        ),
    )
}

/// The code points of `needle`'s first occurrence in `hay`.
fn occurrence(hay: &str, needle: &str) -> Option<(u64, u64)> {
    let at = hay[..hay.find(needle)?].chars().count() as u64;
    Some((at, at + needle.chars().count() as u64))
}

/// The claim the case is about: the one holding its first specific.
fn target<'a>(case: &ClaimCase, claims: &'a [VerifiedClaim]) -> Option<&'a VerifiedClaim> {
    let (at, _) = occurrence(&case.unit, case.specifics.first()?)?;
    claims.iter().find(|c| c.start <= at && at < c.end)
}

fn kind(v: &VerifyVerdict) -> &'static str {
    match v {
        VerifyVerdict::Supported => "supported",
        VerifyVerdict::Contradicted => "contradicted",
        VerifyVerdict::Unsupported => "unsupported",
        VerifyVerdict::CouldNotJudge { .. } => "could_not_judge",
        VerifyVerdict::Abstained => "abstained",
    }
}

/// What a verify reply says wrongly of `case` at `tier`, judged without
/// reading a text back.
pub fn claim_reply_failures(
    case: &ClaimCase,
    tier: VerifyTier,
    expect: Option<&Expect>,
    claims: &[VerifiedClaim],
) -> Vec<String> {
    let mut f = Vec::new();
    let unit = &case.unit;
    for c in claims {
        for s in &c.specifics {
            if code_points(unit, s.start, s.end).as_deref() != Some(s.exact.as_str()) {
                f.push(format!(
                    "specific `{}` is not the unit's [{}, {})",
                    s.exact, s.start, s.end
                ));
            }
            if s.start < c.start || s.end > c.end {
                f.push(format!("specific `{}` stands outside its claim", s.exact));
            }
        }
        let verdict = kind(&c.verdict);
        if tier == VerifyTier::Fast && ["supported", "contradicted"].contains(&verdict) {
            f.push(format!("the fast tier judged `{}` {verdict}", c.exact));
        }
        if tier == VerifyTier::Fast && verdict == "unsupported" && !c.evidence.is_empty() {
            f.push(format!(
                "at fast, `{}` is unsupported with evidence: the tier's literal miss decided it",
                c.exact
            ));
        }
        let named = c
            .evidence
            .iter()
            .any(|p| matches!(p.verdict, VerifyVerdict::Contradicted) && !p.differing.is_empty());
        if verdict == "contradicted" && !named {
            f.push(format!(
                "`{}` is contradicted and no passage names a differing specific",
                c.exact
            ));
        }
        for p in &c.evidence {
            if p.span.is_none() {
                f.push(format!("a passage judged on `{}` carries no span", c.exact));
            }
            if !p.differing.is_empty() && !matches!(p.verdict, VerifyVerdict::Contradicted) {
                f.push("a passage names differing specifics and is not contradicted".into());
            }
            for d in &p.differing {
                let same = d.claim.kind == d.source.kind && d.claim.kind != ClaimSpecificKind::Name;
                if !same || d.claim.exact == d.source.exact {
                    f.push(format!(
                        "`{}` and `{}` are named differing: two of one kind that differ, \
                         never a name",
                        d.claim.exact, d.source.exact
                    ));
                }
                if code_points(unit, d.claim.start, d.claim.end).as_deref()
                    != Some(d.claim.exact.as_str())
                {
                    f.push(format!(
                        "differing `{}` is not the unit's range",
                        d.claim.exact
                    ));
                }
            }
        }
    }
    let all: Vec<_> = claims.iter().flat_map(|c| &c.specifics).collect();
    for want in &case.specifics {
        let Some((a, b)) = occurrence(unit, want) else {
            f.push(format!("`{want}` is not in the unit"));
            continue;
        };
        let covered = all
            .iter()
            .any(|s| s.start <= a && b <= s.end && s.kind != ClaimSpecificKind::Name);
        if !covered {
            f.push(format!(
                "no figure, date or quantity specific covers `{want}` in `{unit}`"
            ));
        }
    }
    for not in &case.none_within {
        let Some((a, b)) = occurrence(unit, not) else {
            f.push(format!("`{not}` is not in the unit"));
            continue;
        };
        if let Some(s) = all.iter().find(|s| s.start < b && a < s.end) {
            f.push(format!(
                "`{}` is named a specific inside `{not}`, a citation or a locator",
                s.exact
            ));
        }
    }
    let Some(e) = expect else { return f };
    let Some(c) = target(case, claims) else {
        f.push(format!(
            "no claim of the reply holds `{unit}`'s first specific"
        ));
        return f;
    };
    let verdict = kind(&c.verdict);
    if verdict == "could_not_judge" {
        return f;
    }
    if verdict != e.verdict {
        f.push(format!(
            "at {}, `{}` is {verdict}; it should be {}",
            tier.as_str(),
            c.exact,
            e.verdict
        ));
    }
    if let Some(has) = e.evidence {
        if has == c.evidence.is_empty() {
            f.push(format!(
                "at {}, `{}` carries {} passage(s); it should carry {}",
                tier.as_str(),
                c.exact,
                c.evidence.len(),
                if has { "some" } else { "none" }
            ));
        }
    }
    for [claim, source] in &e.differing {
        let named = c.evidence.iter().flat_map(|p| &p.differing).any(|d| {
            d.claim.exact.contains(claim.as_str()) && d.source.exact.contains(source.as_str())
        });
        if !named {
            f.push(format!(
                "`{}` is not named differing from the source's `{source}` on `{claim}`",
                c.exact
            ));
        }
    }
    f
}
