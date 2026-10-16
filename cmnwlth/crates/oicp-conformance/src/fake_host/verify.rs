// SPDX-License-Identifier: AGPL-3.0-or-later
//! The fake's verify routes (v0.6 §2.6, §2.10, §2.15): verify by meaning,
//! over the same scored sentences sentence search answers from, and the
//! correction route. A child of `fake_host`, so it reads the fake's private
//! state.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use oicp_types::{ClaimSpecific, ClaimSpecificKind, DifferingSpecific, Span};
use serde_json::{json, Value};

use super::{refuse, sentences, Fake, Stored};
use crate::fixture::{code_points, sha256_hex};

/// The fake's relevance floor (v0.6 §2.15): a sentence scoring below it is
/// not about the claim. Its data file is this constant.
const FLOOR: f32 = 0.4;

/// The candidate sentences the fake's checker judges, best first.
const CHECKER_SENTENCES: usize = 3;

/// The lawful verify by meaning (§2.6, §2.15): each sentence of the unit is
/// a claim, its specifics named by kind ([`specifics_of`]) and its
/// candidates the scope's sentences at or above [`FLOOR`], by the scorer
/// sentence search uses. None: `unsupported` with no evidence, at either
/// tier. At `fast`, `abstained`, the specifics no candidate carries named
/// as a hint. At `checker`, each candidate is judged: `supported` when it
/// carries every specific's value (a claim with none: when it holds the
/// claim verbatim), `contradicted` when a specific of one kind differs,
/// else `unsupported`. The judge tier is not served.
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
        let lead = exact.chars().count() - exact.trim_start().chars().count();
        let (a, b) = ((start + lead) as u64, (i + 1) as u64);
        let exact = exact.trim_start().to_string();
        start = i + 1;
        let specifics = specifics_of(&exact, a, &f.breaks);
        let candidates: Vec<_> = sentences::scored(&f, &exact, &req.corpora, &[])
            .into_iter()
            .filter(|c| c.0 >= FLOOR)
            .take(CHECKER_SENTENCES)
            .collect();
        let (verdict, evidence, absent, rung) = if candidates.is_empty() {
            ("unsupported", Vec::new(), Vec::new(), "fake/below-floor")
        } else if req.tier == VerifyTier::Fast {
            let absent: Vec<String> = specifics
                .iter()
                .filter(|sp| {
                    !candidates.iter().any(|(_, s, x, y)| {
                        code_points(&s.text, *x, *y).is_some_and(|t| t.contains(&sp.exact))
                    })
                })
                .map(|sp| sp.exact.clone())
                .collect();
            let decides = f.breaks.fast_hint_decides && !absent.is_empty();
            let verdict = if decides { "unsupported" } else { "abstained" };
            (verdict, Vec::new(), absent, "fake/value-presence")
        } else {
            let judged: Vec<Value> = candidates
                .iter()
                .map(|(score, s, x, y)| judge(&f, &exact, &specifics, s, *x, *y, *score))
                .collect();
            let has = |k: &str| judged.iter().any(|p| p["verdict"]["kind"] == k);
            let verdict = if has("supported") {
                "supported"
            } else if has("contradicted") {
                "contradicted"
            } else {
                "unsupported"
            };
            (verdict, judged, Vec::new(), "fake/sentence-support")
        };
        let verdict = if f.breaks.verify_unknown_verdict {
            json!({"kind": "plausible"})
        } else {
            json!({ "kind": verdict })
        };
        claims.push(json!({
            "start": a, "end": b, "exact": exact, "class": "checkable",
            "verdict": verdict, "evidence": evidence, "rung": rung,
            "specifics": specifics, "specifics_absent": absent,
            "operating_point": UNCALIBRATED,
            "episode_id": format!("00000000-0000-4000-8000-{:012x}", claims.len()),
        }));
    }
    Json(json!({
        "claims": claims,
        "verifier": format!("fake-verify/2 floor={FLOOR} sentences={CHECKER_SENTENCES}"),
        "corpora": f.searched(&req.corpora).into_iter()
            .map(|c| json!({"corpus_id": c, "texts_digest": f.digest(&c)}))
            .collect::<Vec<_>>(),
    }))
    .into_response()
}

/// The checker's judgement of the sentence `[x, y)` of `s` on the claim
/// `exact`: a passage with its span (`exact_off_by_one` shifts it), its
/// verdict, and on `contradicted` the specifics that differ
/// (`contradicted_unnamed` names none).
fn judge(
    f: &Fake,
    exact: &str,
    claim: &[ClaimSpecific],
    s: &Stored,
    x: u64,
    y: u64,
    score: f32,
) -> Value {
    let sentence = code_points(&s.text, x, y).unwrap_or_default();
    let source = specifics_of(&sentence, x, &f.breaks);
    let counted: Vec<&ClaimSpecific> = claim
        .iter()
        .filter(|c| c.kind != ClaimSpecificKind::Name)
        .collect();
    let held = |c: &ClaimSpecific, among: &[ClaimSpecific]| {
        among.iter().any(|o| value(&o.exact) == value(&c.exact))
    };
    let backed = if counted.is_empty() {
        sentence.contains(exact)
    } else {
        counted.iter().all(|c| held(c, &source))
    };
    let differing: Vec<DifferingSpecific> = if backed {
        Vec::new()
    } else {
        counted
            .iter()
            .filter(|c| !held(c, &source))
            .filter_map(|c| {
                let other = source
                    .iter()
                    .find(|o| o.kind == c.kind && !held(o, claim))?;
                Some(DifferingSpecific {
                    claim: (*c).clone(),
                    source: other.clone(),
                })
            })
            .collect()
    };
    let verdict = match (backed, differing.is_empty()) {
        (true, _) => "supported",
        (false, false) => "contradicted",
        (false, true) => "unsupported",
    };
    let shift = u64::from(f.breaks.exact_off_by_one);
    let span = Span {
        corpus_id: s.corpus.clone(),
        document: f.record(s),
        start: x,
        end: y,
        exact: code_points(&s.text, x + shift, y + shift).unwrap_or_default(),
        prefix: String::new(),
        suffix: String::new(),
        locator: f.locator(s, x),
    };
    let differing = if f.breaks.contradicted_unnamed {
        Vec::new()
    } else {
        differing
    };
    json!({"corpus_id": s.corpus, "span": span, "verdict": {"kind": verdict},
           "score": score, "differing": differing})
}

/// A number's value, its separators and signs dropped: `4,120` and `4120`
/// are one value, `7.5%` and `7.5` another.
fn value(exact: &str) -> Option<f64> {
    let digits: String = exact
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    digits.trim_matches('.').parse().ok()
}

/// The fake's specifics finder (§2.15), the shape of the rule and no more:
/// every word carrying a digit, with a `%`, a currency sign or `percent`
/// after it a figure, a lone year a date, one before a lower-case word a
/// quantity, any other a figure. A number inside parentheses that are a
/// citation (a capitalised name and a year) or a locator (`page 14`,
/// `p. 3`, `§ 4`) is none, nor is a citekey (`@key`).
/// `citation_year_specific` reads those too; `parenthesis_dropped` reads no
/// number inside any parentheses. `offset` places the ranges.
fn specifics_of(text: &str, offset: u64, b: &super::Breaks) -> Vec<ClaimSpecific> {
    let chars: Vec<char> = text.chars().collect();
    let mut parens: Vec<(usize, usize, bool)> = Vec::new();
    let mut open: Vec<usize> = Vec::new();
    for (i, c) in chars.iter().enumerate() {
        match c {
            '(' => open.push(i),
            ')' => {
                if let Some(o) = open.pop() {
                    let inner: String = chars[o + 1..i].iter().collect();
                    parens.push((o, i, cites_or_locates(&inner)));
                }
            }
            _ => {}
        }
    }
    let mut words: Vec<(usize, usize)> = Vec::new();
    let mut at = None;
    for (i, c) in chars.iter().chain([&' ']).enumerate() {
        match (c.is_whitespace(), at) {
            (false, None) => at = Some(i),
            (true, Some(s)) => {
                words.push((s, i));
                at = None;
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for (n, &(ws, we)) in words.iter().enumerate() {
        let word: String = chars[ws..we].iter().collect();
        if !word.chars().any(|c| c.is_ascii_digit())
            || word.trim_start_matches('[').starts_with('@')
        {
            continue;
        }
        let lead = word.chars().take_while(|c| "([\"'".contains(*c)).count();
        let tail = word
            .chars()
            .rev()
            .take_while(|c| ")].,;:!?\"'".contains(*c))
            .count();
        let (s, e) = (ws + lead, we - tail);
        let inside = parens.iter().find(|(o, c, _)| *o < s && e <= *c);
        match inside {
            Some((_, _, true)) if !b.citation_year_specific => continue,
            Some(_) if b.parenthesis_dropped => continue,
            _ => {}
        }
        let exact: String = chars[s..e].iter().collect();
        let next: String = words
            .get(n + 1)
            .map(|&(a, z)| chars[a..z].iter().collect())
            .unwrap_or_default();
        let next = next.trim_end_matches(|c: char| !c.is_alphanumeric());
        let year = exact.len() == 4
            && exact.bytes().all(|c| c.is_ascii_digit())
            && (1000..3000).contains(&exact.parse::<u32>().unwrap_or(0));
        let kind = if exact.ends_with('%')
            || exact.starts_with(['$', '£', '€'])
            || ["percent", "per"].contains(&next)
        {
            ClaimSpecificKind::Figure
        } else if year {
            ClaimSpecificKind::Date
        } else if next.starts_with(|c: char| c.is_lowercase()) {
            ClaimSpecificKind::Quantity
        } else {
            ClaimSpecificKind::Figure
        };
        out.push(ClaimSpecific {
            kind,
            start: offset + s as u64,
            end: offset + e as u64,
            exact,
        });
    }
    out
}

/// Whether a parenthesis's text is a citation (a capitalised name, then a
/// year) or a locator (a page, pages, a section, a chapter).
fn cites_or_locates(inner: &str) -> bool {
    let t = inner.trim();
    let lower = t.to_lowercase();
    let locator = ["p.", "pp.", "page", "§", "section", "chapter", "ch."]
        .iter()
        .any(|w| lower.starts_with(w));
    let year = t.split(|c: char| !c.is_ascii_digit()).any(|w| w.len() == 4);
    let named = t.chars().next().is_some_and(char::is_uppercase);
    locator || (named && year)
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
