//! Extracted from judge.rs (2026-09-03, ARCH §3.1) — see the judge façade.
use super::*;
use crate::oicp::ShardingPrivacy;
use crate::runtime::grounding::call_census::gate_call;
use crate::runtime::grounding::config::dbg;
use crate::runtime::grounding::search::SealedEvidenceSearch;
use crate::slot_policy::Workload;
use crate::traits::InferenceProvider;
use crate::types::{CompletionRequest, Speed};
use sovereign_contracts::types::GateCallMechanism;
use std::sync::Arc;

/// real fabrications, not prune legitimately-grounded content. Returns the
/// offending specifics verbatim (answer wording), or an empty vec when every
/// specific checks out. `None` on inference error → caller fails open.
pub async fn scan_unsupported_specifics(
    inference: &Arc<dyn InferenceProvider>,
    question: &str,
    answer: &str,
    leaf_chunks: &[String],
    summary_chunks: &[String],
    max_items: usize,
    posture: ShardingPrivacy,
) -> Option<Vec<String>> {
    // D3 CANDIDATE A (order audit-economy): the scan JOINS the judges' prefix
    // family — the same system turn and the same leaf-window opening bytes as
    // `claim_violation_joint` / `claims_support_batched`, summaries appended
    // AFTER the declared boundary (exactly as thematic claim checks append
    // theirs). D0 measured this scan as the audit's largest single term
    // (9.7s median, 35% of the stage) precisely because its private system
    // turn put it in its own pin family; joining the family makes its
    // evidence prefill a restore of state a sibling already paid for, on
    // clean and rewrite turns alike. This IS a judge-input change: it is
    // priced replay-first against the 9 labeled scan items and the
    // scan-vs-main deltas before any live arm (the land-C caution does not
    // transfer — this register is generative, no forced-choice margin
    // exists here to compress — but the claim is measured, not argued).
    if leaf_chunks
        .iter()
        .chain(summary_chunks.iter())
        .all(|c| c.trim().is_empty())
    {
        return Some(Vec::new());
    }
    // Audit the CONTENT of honestly-labeled spans, not the label: the wrapper
    // words bias the judge against supported content (see
    // `unwrap_unverified_excerpts`).
    let answer = &unwrap_unverified_excerpts(answer);
    let family = EvidenceFamily::new(leaf_chunks);
    let (prompt, stable_prefix_len) =
        family.scan_prompt(summary_chunks, question, answer, max_items);
    let req = CompletionRequest {
        prompt,
        stable_prefix_len,
        system_message: Some(CHUNK_JUDGE_SYSTEM.into()),
        preferred_speed: Speed::Slow,
        // SLOT_POLICY §7: route the Critic through the privacy-gated OICP
        // path instead of pinning `model_id: "primary"` (see
        // `forced_choice_ab` for the full rationale).
        oicp: Some(Workload::Judge.requirements(posture)),
        max_tokens: Some((max_items * 40).max(160)),
        temperature: Some(0.0),
        think_budget: Some(0),
        enable_thinking: Some(false),
        ..Default::default()
    };
    match gate_call(&**inference, &req, GateCallMechanism::SpecificsScan).await {
        Ok(resp) => Some(scan_items_from_reply(&resp.text, answer, max_items)),
        Err(e) => {
            tracing::warn!(target: "grounding_gate", error = %e, "specifics scan failed");
            None
        }
    }
}

/// The specifics scan's reply → the flagged answer spans. Pure, so the
/// judge's raw output can be replayed in a test without an inference
/// provider — which is how the judge-prose defect below is pinned.
///
/// Line discipline first (bullet/number prefixes, the NONE sentinel, a
/// length floor), then [`anchor_scan_item`] decides, per line, whether
/// the judge quoted the ANSWER or wrote about it. Only the former survive:
/// a scan item is a claim the answer made, never the judge's commentary on
/// it.
pub(crate) fn scan_items_from_reply(reply: &str, answer: &str, max_items: usize) -> Vec<String> {
    let t = reply.trim();
    if t.is_empty() || t.to_uppercase().contains("NONE") {
        return Vec::new();
    }
    t.lines()
        .map(|l| l.trim().trim_start_matches(['-', '*', '•']).trim())
        .map(|l| {
            l.trim_start_matches(|c: char| c.is_ascii_digit())
                .trim_start_matches(['.', ')'])
                .trim()
                .to_string()
        })
        .filter(|l| l.len() > 8)
        .filter_map(|l| match anchor_scan_item(&l, answer) {
            Some(span) => Some(span),
            None => {
                // Reported, never defaulted: the line is named at the level
                // that reads it, so a judge drifting off the verbatim
                // contract is visible as a drop count rather than as
                // commentary appearing in someone's ledger.
                tracing::info!(
                    target: "grounding_gate",
                    event = "scan_item_dropped",
                    reason = "not a span of the answer",
                    line = %l.chars().take(120).collect::<String>(),
                    "specifics scan: judge wrote about the answer, not from it"
                );
                None
            }
        })
        .take(max_items)
        .collect()
}

/// Strip the app's own honest `[unverified excerpt: X]` wrappers down to X.
/// The wrapper is presentation metadata from quote_verification.rs; fed back
/// into a judge it reads as an admission and biases the verdict against
/// SUPPORTED content (observed 2026-07-01: "As Samuelson (1954) noted…" —
/// verbatim in the evidence at offset 2410 — was flagged unsupported only when
/// wrapped, and the verification note then listed it as unverified while the
/// body cited it: a self-contradiction the re-judge scored confabulation).
/// Same principle as the offline rubric's clause: judge X's content, never the
/// wrapper.
pub(crate) fn unwrap_unverified_excerpts(s: &str) -> String {
    const OPEN: &str = "[unverified excerpt:";
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(OPEN) {
        out.push_str(&rest[..i]);
        let after = &rest[i + OPEN.len()..];
        match after.find(']') {
            Some(j) => {
                out.push_str(after[..j].trim());
                rest = &after[j + 1..];
            }
            None => {
                out.push_str(&rest[i..]);
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Reduce a scan line toward the ANSWER SPAN it flags. The prompt demands the
/// answer's exact wording, but the 35B routinely appends judgment chatter
/// ("… — The evidence does not mention this") or frames the item as commentary
/// ("The answer cites \"[Source: X]\" for …"). These lines flow into the
/// rewrite instructions AND the user-visible verification note — where the
/// chatter reads as the assistant indicting itself (observed live 2026-07-01:
/// a released answer footnoted "… is a fabricated specific not found in the
/// Deterministic jurisdiction filter: self-referential DECLINE statements —
/// negated capability/coverage claims whose subject is the system itself or
/// its evidence ("the system does not have access to…", "the provided
/// passages do not contain…", "there is no evidence in the sources…").
/// These are honesty meta-language, not world-claims: no passage can state
/// them, so auditing them prosecutes the answer's own honesty. Observed
/// 2026-07-10 (persona-QA): refined honest declines rejected at vp
/// 0.85–0.98 on exactly these sentences, reverting the web-search
/// refinement to the original. A decline asserts the ABSENCE of
/// information — it cannot launder a false world-claim — so exempting the
/// SHAPE is safe. Same family as the offline judge's decline-shape
/// override (calibration gate) and the `[Source:]` scan-jurisdiction rule.
/// T1 P1.4 claim-class decision. FACTUAL/SPECIFIC claims must be
/// supported by Leaf-class evidence; THEMATIC/STRUCTURAL claims (about
/// the text's themes, structure, or discourse rather than in-world
/// specifics) may additionally rest on Summary-class evidence.
///
/// Two layers, in order:
/// 1. Structural specificity — digits or quotations in the claim →
///    factual, deterministically. These are features of the claim's
///    FORM, reliable regardless of vocabulary.
/// 2. Semantic class — the centroid-of-embeddings classifier
///    (`claim_class_classifier`, same shape as the current-info and
///    scope routers). No marker lists: a substring heuristic here
///    would be the keyword-classifier failure the routers already
///    replaced twice, and this decision gates honesty.
///
/// DEFAULT-FACTUAL everywhere: low signal, thin margin, classifier
/// unavailable, embed failure — all keep the conservative bar.
pub(crate) async fn claim_is_factual_specific(
    inference: &Arc<dyn InferenceProvider>,
    claim: &str,
) -> bool {
    if claim_has_structural_specificity(claim) {
        return true;
    }
    match crate::claim_class_classifier::shared_claim_classifier(inference).await {
        Some(classifier) => matches!(
            classifier.classify(claim, inference).await,
            crate::claim_class_classifier::ClaimClass::Factual
        ),
        None => true,
    }
}

/// Layer-1 structural check: numbers, years, quantities, or quoted
/// spans make a claim factual/specific regardless of phrasing.
pub(super) fn claim_has_structural_specificity(claim: &str) -> bool {
    let has_digit = claim.chars().any(|c| c.is_ascii_digit());
    let has_quote = claim.contains('"') || claim.contains('\u{201c}') || claim.contains('\u{201d}');
    has_digit || has_quote
}

pub(crate) fn is_self_referential_decline(text: &str) -> bool {
    let t = normalize_meta(text);
    if !meta_subject(&t) {
        return false;
    }
    [
        "does not",
        "do not",
        "doesn't",
        "don't",
        "cannot",
        "can't",
        "no evidence",
        "no information",
        "lacks",
        "not include",
        "not contain",
        "not have",
    ]
    .iter()
    .any(|n| t.contains(n))
}

/// Strip markdown emphasis ("does **not** have" must match "does not"),
/// then leading list/quote decoration; lowercase. Shared normalization for
/// the meta-language predicates below.
fn normalize_meta(text: &str) -> String {
    text.replace('*', "")
        .trim()
        .trim_start_matches(['-', ' ', '"', '\u{201c}'])
        .to_lowercase()
}

/// Explicit system/evidence-artifact subjects — safe to treat as
/// meta-language even WITHOUT a negation (a positive description of the
/// evidence still isn't a world-claim).
const META_SUBJECTS_CORE: &[&str] = &[
    "the system",
    "the assistant",
    "the model",
    "the app",
    "this system",
    "the provided",
    "the retrieved",
    "the sources",
    "the passages",
    "the evidence",
    "the corpus",
    "the collection",
    "the knowledge base",
    "the local corpus",
    "the initial answer",
];

/// Looser subject prefixes ("I …", "It …", "There is no …", "As of …") that
/// read as meta ONLY when the negation requirement of
/// [`is_self_referential_decline`] constrains them — "It was sent in May" is
/// a world-claim with a pronoun subject and must never match the
/// negation-free arm.
const META_SUBJECTS_LOOSE: &[&str] = &["i ", "it ", "there is no", "as of "];

/// Subject test for [`is_self_referential_decline`] (negation-guarded →
/// loose prefixes allowed).
fn meta_subject(t: &str) -> bool {
    META_SUBJECTS_CORE
        .iter()
        .chain(META_SUBJECTS_LOOSE)
        .any(|s| t.starts_with(s))
}

/// Strict subject test for the negation-free rider arm of
/// [`decline_rider_exempt`]: explicit evidence/system nouns only.
fn meta_subject_strict(t: &str) -> bool {
    META_SUBJECTS_CORE.iter().any(|s| t.starts_with(s))
}

/// Short-path jurisdiction scalpel (2026-07-21): should the gate SKIP
/// auditing this extracted claim because it is a decline's meta-rider, not a
/// world-claim? True when either:
///
///  1. the claim itself is a negated self-referential decline — the exact
///     shape the longform gate already exempts (asserts ABSENCE, cannot
///     launder a value); or
///  2. the ANSWER's headline act is a deterministic decline
///     (`answer_declines`) AND the claim's subject is the evidence/system —
///     the rider case ("I don't have reliable information on this. The
///     provided passages are Rust source code snippets…"). Auditing such a
///     rider is category-confused — no passage states facts about the
///     passages — so it reliably fails, burning the per-passage sweep
///     (measured 16 × 0.8s, 2026-07-21 soak step 91) and then a doomed
///     second-synthesis retry (the documented 50-160s slow abstention).
///
/// A decline that smuggles a WORLD-claim rider ("…However, John sent the
/// memo on May 5") keeps its full audit: the claim extractor strips
/// source-attribution wrappers, so a world rider arrives with a world
/// subject and fails arm 2's subject test.
pub(super) fn decline_rider_exempt(answer: &str, claim: &str) -> bool {
    is_self_referential_decline(claim)
        || (crate::runtime::grounding::answer_declines(answer)
            && meta_subject_strict(&normalize_meta(claim)))
}

/// Anchor one specifics-scan line to the ANSWER, or reject it.
///
/// The scan is asked for verbatim answer wording ("Quote the answer's exact
/// wording"), and a well-behaved judge obliges. A judge that does not obliges
/// with commentary — a critique preamble, or a quoted span with its own
/// verdict appended — and that commentary used to pass through untouched.
/// Downstream, `longform_claims` turns every scan finding into a `GateClaim`
/// and the epistemic ledger renders it as a `failed_once` **holding**, so the
/// user read the judge's remarks as their own answer's failed claims. Measured
/// on `compound-killer-and-lugger` (see `testdata/README.md`): three of that
/// turn's five negative holdings were judge prose, and two of the three also
/// reached the user-visible verification note.
///
/// So this is a decision, not a cleanup: **an item that is not wording of the
/// answer is not a claim about the world, and gets no holding.** `None` is
/// that verdict, and the caller traces it — an item is dropped loudly, never
/// silently rewritten into something claim-shaped.
///
/// Deterministic ladder, first match wins:
/// 1. the longest QUOTED span that occurs in the answer → the span;
/// 2. a quoted span the judge ELIDED with a trailing ellipsis → its prefix,
///    when that prefix occurs in the answer and is substantial;
/// 3. the item is itself answer wording → the item;
/// 4. a prefix cut at a commentary dash that occurs in the answer → the prefix;
/// 5. otherwise `None` — the judge wrote ABOUT the answer, not FROM it.
///
/// Containment is judged by [`anchor_key`], which ignores emphasis markers:
/// the judge re-quotes `**Severin Quenholt**` as `Severin Quenholt`, and step 1
/// used to miss on exactly that difference and fall through to the old
/// pass-through arm.
pub(crate) fn anchor_scan_item(item: &str, answer: &str) -> Option<String> {
    /// A prefix recovered from an elided quote has to be long enough to still
    /// be a claim — `"Severin Quenholt... as harbormaster"` must not reduce to
    /// a bare name.
    const MIN_ELIDED_PREFIX: usize = 24;
    const MIN_SPAN: usize = 12;

    let item = &unwrap_unverified_excerpts(item);
    let ans = anchor_key(answer);
    let quoted: Vec<&str> = extract_quoted_spans(item);
    // 1. A quoted span the answer actually contains.
    if let Some(best) = quoted
        .iter()
        .filter(|s| s.chars().count() >= MIN_SPAN && ans.contains(&anchor_key(s)))
        .max_by_key(|s| s.chars().count())
    {
        return Some(best.trim().to_string());
    }
    // 2. A quoted span cut short with "…" — anchor on what precedes it.
    for span in &quoted {
        let head = span.trim_end().trim_end_matches(['"', '“', '”']).trim_end();
        for ellipsis in ["...", "…"] {
            if let Some(prefix) = head.strip_suffix(ellipsis) {
                let prefix = prefix.trim_end();
                if prefix.chars().count() >= MIN_ELIDED_PREFIX && ans.contains(&anchor_key(prefix))
                {
                    return Some(prefix.to_string());
                }
            }
        }
    }
    // 3. The whole item is answer wording (checked BEFORE the dash cut, so a
    //    legitimate interior dash in a present item is not treated as a seam).
    if ans.contains(&anchor_key(item)) {
        return Some(item.trim().trim_matches(['"', '“', '”']).trim().to_string());
    }
    // 4. Commentary appended after a dash. " - " is here because it is what the
    //    live judge emitted on the measured turn; the others predate it.
    for dash in [" — ", " – ", " -- ", " - "] {
        if let Some((head, _)) = item.split_once(dash) {
            let head = head.trim().trim_matches(['"', '“', '”']).trim();
            if head.chars().count() >= MIN_SPAN && ans.contains(&anchor_key(head)) {
                return Some(head.to_string());
            }
        }
    }
    None
}

/// Spans inside straight or curly double quotes, in order of appearance.
pub(crate) fn extract_quoted_spans(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = s;
    loop {
        let Some(open) = rest.find(['"', '“']) else {
            break;
        };
        let open_len = rest[open..].chars().next().map_or(1, char::len_utf8);
        let after = &rest[open + open_len..];
        let Some(close) = after.find(['"', '”']) else {
            break;
        };
        out.push(&after[..close]);
        let close_len = after[close..].chars().next().map_or(1, char::len_utf8);
        rest = &after[close + close_len..];
    }
    out
}

/// The one normal form for "does this text occur in the answer" —
/// lowercase, whitespace runs collapsed, and Markdown emphasis markers
/// dropped. Emphasis is presentation: the answer writes
/// `**Severin Quenholt**` and `*The Cold Lantern*`, and a judge quoting
/// either writes the plain words. Comparing raw made those spans read as
/// absent from the answer they came from.
///
/// Containment only. Never use it to build a value that is shown or stored —
/// [`anchor_scan_item`] returns slices of the ORIGINAL text.
fn anchor_key(s: &str) -> String {
    s.to_lowercase()
        .replace(['*', '_', '`'], "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// In-world attribution veto — the deterministic pre-check the yes-biased
/// joint judge needs. Measured (padghost replay 2026-07-02): "Betty Alexander
/// sent an email to Jeff Skilling on July 7, 2000" scored vp=0.010 — every
/// element of the claim is corpus-true EXCEPT the invented person (the real
/// sender is Rosalee; "Betty Alexander" appears nowhere in the evidence), and
/// a forced-choice judge shown a nearly-true claim answers "supports". The
/// same ghost shipped in three separate runs.
///
/// The veto is scoped to IN-WORLD attributions so correct general knowledge is
/// never shackled (the trust bar): it fires only when the claim is about a
/// corpus ARTIFACT (email/letter/document/passage/sent/wrote/…) AND carries a
/// person-name-shaped bigram (Capitalized-lowercase pair — acronyms like "HR"
/// don't match) absent from the ENTIRE evidence + labels. A name attributed to
/// a corpus artifact must exist in the corpus; a GK claim ("Noam Cohen wrote
/// in Wired…", no artifact noun) passes through to the judge untouched.
/// Returns the offending name for the glassbox.
/// Remove `[Source: …]` citation spans before any name/identifier sweep:
/// labels are pre-validated by the deterministic snap pass and are OUT OF
/// JURISDICTION here — sweeping them produced user-visible self-indictments
/// ("The answer references \"Source Psilocybin\", which does not appear in
/// the sources", persona-QA 2026-07-10: 4 of 9 answers ended that way).
/// Unclosed brackets strip to end-of-line (the bounded-bracket lesson).
pub(super) fn strip_citation_spans(claim: &str) -> String {
    let mut out = String::with_capacity(claim.len());
    let mut rest = claim;
    loop {
        let Some(i) = rest.to_lowercase().find("[source:") else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..i]);
        out.push(' ');
        let tail = &rest[i..];
        let end = tail
            .find(']')
            .map(|e| e + 1)
            .or_else(|| tail.find('\n'))
            .unwrap_or(tail.len());
        rest = &tail[end..];
    }
}

/// Capitalized FUNCTION/BOILERPLATE words are structurally never given
/// names — "From Retrieved" (a section header), "Source Federalist" (a
/// label fragment). Blocking them as bigram members costs a theoretical
/// missed fabrication and removes a measured class of self-indictments.
pub(crate) fn non_name_word(w: &str) -> bool {
    matches!(
        w.to_lowercase().as_str(),
        "from" | "the" | "this" | "these" | "those" | "your" | "their" | "our"
            | "its" | "based" | "initial" | "additional" | "retrieved"
            | "provided" | "source" | "sources" | "answer" | "web" | "search"
            | "note" | "summary" | "overview" | "key" | "corpus" | "evidence"
            | "passage" | "passages" | "section" | "document" | "knowledge"
            // Pronouns: "Webber He averaged…" flagged "Webber He" as a
            // fabricated name (persona-QA, the run after the label fix).
            | "he" | "she" | "they" | "we" | "his" | "her" | "him" | "them"
            | "who" | "which" | "when" | "where" | "while" | "after" | "before"
    )
}

/// Does `low` contain any of `words` as a WHOLE WORD?
///
/// Both deterministic vetoes below gate themselves on "is this claim even
/// about a corpus artifact?" and both used `low.contains(a)`, which is a
/// substring test. The consequences were not marginal — measured 2026-08-13,
/// the artifact gate opened on ordinary prose:
///
///   "designed"  contains "signed"     "presented" contains "sent"
///   "sentence"  contains "sent"       "absent"    contains "sent"
///   "consent"   contains "sent"       "represent" contains "sent"
///   "essential" contains "sent"       "classical" contains "class"
///   "denotes"   contains "notes"      "documented" contains "document"
///
/// So "Harry Frankfurt designed cases…" tripped the name veto — the gate
/// opened on "signed", and the bigram check then flagged "Harry Frankfurt"
/// because the corpus writes the surname alone. A gate meant to restrict these
/// vetoes to claims about emails, letters and source files was instead open on
/// most sentences an essay contains.
///
/// One helper for both call sites (ARCH §10.6): the two vetoes ask the same
/// question and must not answer it two ways.
pub(crate) fn mentions_artifact(low: &str, words: &[&str]) -> bool {
    words.iter().any(|w| {
        low.match_indices(w).any(|(i, _)| {
            let before_ok = i == 0
                || !low[..i]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_alphanumeric());
            let after = i + w.len();
            let after_ok = after >= low.len()
                || !low[after..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphanumeric());
            before_ok && after_ok
        })
    })
}
