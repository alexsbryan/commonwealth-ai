// SPDX-License-Identifier: AGPL-3.0-or-later
//! R8 — local synthesis: the draft, URL-constrained.
//!
//! The draft is produced through the port's constrained draft surface
//! (`ResearchPort::draft`) with the URL constraint enabled over the
//! window's source URLs — invented citations are structurally
//! impossible (the renderer then verifies every span anyway, the
//! always-on guarantee). The evidence is assembled into the prompt by
//! this code, never by the model.

use super::estate::{DraftLeg, ResearchPort};
use super::icd::{Draft, DraftCitation, EvidenceWindow, UrlConstraintPolicy};

/// Assemble the round's evidence text (chunk id → content) for the
/// prompt. Deterministic: chunks in window order, bounded by the
/// charter's window cap (the window was already capped at build).
pub fn evidence_block(window: &EvidenceWindow) -> String {
    let mut out = String::new();
    for chunk in &window.chunks {
        out.push_str(&format!(
            "[{}] {}",
            chunk.id,
            chunk.content.replace('\n', " ")
        ));
        out.push('\n');
    }
    out
}

/// The allowed citation set for the draft: the window's source URLs.
pub fn allowed_urls(window: &EvidenceWindow) -> Vec<String> {
    window.chunks.iter().map(|c| c.source_url.clone()).collect()
}

/// The draft's deterministic figure inventory (order deep-research-t1h,
/// H2 — pre-registered): per window chunk, its `figure_tokens` — the
/// ONE figure decider (mod.rs) — under a fixed header, so the model is
/// never left to volunteer the evidence's digits (the t1f residual:
/// keys whose figures sat in the window while the sub-questions did
/// not carry them; the t1g v1 flight: the window carried era figures
/// the draft's era-years restated). The inventory is code-enforced
/// into the PROMPT; the model's carrying of the figures into the
/// answer is measured by the battery, never assumed (§7.6). Empty
/// window → empty block (nothing to enumerate, nothing to invent).
pub fn figure_inventory(window: &EvidenceWindow) -> String {
    let mut out = String::new();
    let mut any = false;
    for chunk in &window.chunks {
        let tokens = super::figure_tokens(&chunk.content);
        if tokens.is_empty() {
            continue;
        }
        any = true;
        out.push_str(&format!("- [{}]: {}\n", chunk.id, tokens.join(", ")));
    }
    if !any {
        return String::new();
    }
    format!(
        "Figures present in the evidence (every evidence-supported figure must appear in the answer):\n{out}"
    )
}

/// The corruption-class marker set (order deep-research-t6c, REV-2,
/// pre-registered): inner-monologue and evidence-self-interrogation
/// shapes measured in the seed-07 r3 draft (flight record
/// dr-1787102765 — the rev-1 2->38 ledger blowout). Rules describe
/// SHAPES, never content: the marker class is the documented
/// corruption signature, and a clean draft with one occurrence cannot
/// trip the bar (>= 2 distinct or >= 3 total required).
pub(crate) const DEGENERATE_MARKERS: [&str; 10] = [
    "(Wait",
    "Let me re-",
    "Let me read",
    "Let me look",
    "I must ",
    "Actually,",
    "Note: Evidence",
    "the exact string",
    "in the snippet",
    "? no",
];

/// T6c REV-4 (pre-registered): the prompt-echo prefix — the corrupt
/// v1 draft-3 (flight dr-1787148073) opened with the prompt's own
/// framing line, and the splitter turned it into gap g19 (one of the
/// measured +3). Fires INDIVIDUALLY: the echo line is a single-
/// origin, structurally unpassable gap source of its own (the
/// battery-2-era echo flights each grew +1 per echoed draft).
fn draft_opens_with_prompt_echo(text: &str) -> bool {
    text.lines().next().is_some_and(|l| {
        l.trim_start()
            .starts_with("Based on the evidence provided, here is how")
    })
}

/// T6c REV-4 (pre-registered): the markdown-header swallow — a
/// `#`-header line whose next non-empty line starts with the header's
/// last word ("### Economic Inequality" + "Inequality widened
/// significantly…" — gap g20). Counts as ONE marker toward the
/// >= 2-distinct / >= 3-total bar, NEVER alone: the pinned clean
/// synthesis class (dr-1787104761 draft-3) carries the identical
/// pair ("### Gentrification" + "Gentrification has become…" —
/// amendment, §18.6). Parenthetical header words ("(1980–2024)")
/// and bullet continuations ("* **Acceleration:**…") are excluded.
fn count_header_swallows(text: &str) -> usize {
    let lines: Vec<&str> = text.lines().collect();
    let mut n = 0usize;
    for (i, line) in lines.iter().enumerate() {
        let hdr = line.trim_start();
        if !hdr.starts_with('#') {
            continue;
        }
        let Some(rest) = hdr.strip_prefix('#') else {
            continue;
        };
        let Some(last) = rest.trim().split_whitespace().next_back() else {
            continue;
        };
        let last = last.trim_matches(['*', ':', '.', ';', ',', '(', ')']);
        if last.is_empty() || last.contains('(') {
            continue;
        }
        let Some(next) = lines[i + 1..]
            .iter()
            .map(|l| l.trim_start())
            .find(|l| !l.is_empty())
        else {
            continue;
        };
        if next.starts_with(last) {
            n += 1;
        }
    }
    n
}

/// T6c REV-4 (pre-registered): the dependent-clause fragment bullet —
/// a bullet line (leading `*`, `-`, or a numbered marker) whose first
/// word opens with a subordinator ("* Although announced in March
/// 2025…" — seed-01's draft-3 bullet, flight dr-1787146175; the
/// splitter's fragment became gap g6, seed-01's +1). Fires
/// INDIVIDUALLY: the accepted false-positive class (seed-12's flat
/// flight, v1-mock's clean "Despite…/Since…" bullets — one extra
/// re-draft each, benign and bounded) is the price of catching the
/// seed-01 class; bold lead-ins ("* **Acceleration:**…") are never
/// fragments.
fn count_fragment_bullets(text: &str) -> usize {
    const FRAGMENT_OPENERS: [&str; 14] = [
        "although",
        "because",
        "while",
        "despite",
        "whereas",
        "since",
        "after",
        "before",
        "when",
        "though",
        "unless",
        "given",
        "showing",
        "including",
    ];
    let mut n = 0usize;
    for line in text.lines() {
        let l = line.trim_start();
        let after_marker = match l.chars().next() {
            Some('*') | Some('-') => &l[1..],
            Some(c) if c.is_ascii_digit() => {
                let word = l.split_whitespace().next().unwrap_or("");
                let rest = word.trim_end_matches(['.', ')', ':']);
                if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
                    continue; // not a numbered-list line
                }
                &l[word.len()..]
            }
            _ => continue,
        };
        let w = after_marker.trim_start();
        if w.starts_with('*') || w.is_empty() {
            continue; // bold lead-ins and empty bullets are not fragments
        }
        let first = w.split_whitespace().next().unwrap_or("");
        if FRAGMENT_OPENERS
            .iter()
            .any(|o| first.to_lowercase().starts_with(o))
        {
            n += 1;
        }
    }
    n
}

/// The degenerate-draft detector (pure, deterministic — no model, no
/// battery-learned thresholds). Degenerate iff the prompt-echo prefix
/// OR any dependent-clause fragment bullet is present (REV-4: each is
/// a single-origin, structurally unpassable gap source that fires
/// alone — the +3/+1 r3 growths), OR >= 2 DISTINCT markers OR >= 3
/// total occurrences OR >= 8 "**" per 1k chars. The header swallow
/// counts as ONE marker toward the bar (it never fires alone — the
/// pinned clean class carries the identical pair). Measured on the
/// flight records: the seed-07 corruption draft = 10 distinct /
/// 27 total / 12.8 per 1k; the clean synthesis class (v1 draft-2/3,
/// seed-02 draft-2) = 0 distinct / 0 total / <= 3.2 per 1k — a >= 2.5x
/// margin on the density bar.
pub(crate) fn draft_is_degenerate(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    if draft_opens_with_prompt_echo(text) || count_fragment_bullets(text) > 0 {
        return true;
    }
    let mut distinct = 0usize;
    let mut total = 0usize;
    for marker in DEGENERATE_MARKERS {
        let n = text.matches(marker).count();
        if n > 0 {
            distinct += 1;
            total += n;
        }
    }
    let swallows = count_header_swallows(text);
    if swallows > 0 {
        distinct += 1;
        total += swallows;
    }
    let bold_per_1k = text.matches("**").count() as f64 * 1000.0 / text.len() as f64;
    distinct >= 2 || total >= 3 || bold_per_1k >= 8.0
}

/// Produce the round's draft through the constrained surface. Round 1
/// drafts from the estate answer alone; later rounds draft from the
/// evidence + the still-open gaps. `strict_shape` (REV-2: the
/// degenerate-draft guard's re-draft) appends a plain-prose shape
/// constraint — the default prompt is byte-shaped exactly as before.

// ---------------------------------------------------------------------
// drb1-t5 — the composed report (AIQ writer contract, teardown §1.6/§6.3).
//
// `draft_round` produces ONE prose draft per round, and the render then
// rebuilt the deliverable out of atomised, individually-audited claim
// rows. Measured on the logged t7a flight, that shape cannot produce a
// research article: the nine deliverables averaged 2.16/10 against the
// reference's 9.32 on the benchmark's own criteria, with `## Findings`
// empty or near-empty on every one of them, because 127 of 137 claims
// landed could-not-judge and the page was the bookkeeping rather than
// the answer.
//
// The reference class this must reach is known and measured: the
// articles that score 40.46 run ~2,200 words across six to eight
// sections with sub-headings, each section answering one sub-question of
// the prompt and citing as it goes. That is what this composes.
//
// Every obligation below is AIQ §6.3 ported onto OUR evidence — with
// their soft `evidence_judgment` replaced by the window we actually
// verified, and their instructed honesty left to our gate, which runs
// over the composed text afterwards and is not weakened by this stage.
// ---------------------------------------------------------------------

/// Passage geometry for per-section retrieval.
const PASSAGE_CHARS: usize = 1400;
const PASSAGE_OVERLAP: usize = 200;
/// Passages handed to one section's writer.
const SECTION_PASSAGES: usize = 8;
/// At most this many passages from any ONE source per section, so a
/// single long page cannot crowd out the rest of the window.
const PER_SOURCE_CAP: usize = 3;

/// The section writer's obligations (AIQ §6.3, items 3-6). Stated once,
/// used by every section — one decider, one name (§10.6).
const WRITER_CONTRACT: &str = "\
Obligations for this section:\n\
- Retain the useful detail: specific numbers, dates, names, mechanisms, \
findings and caveats from the evidence must survive into the prose. Do NOT \
flatten them into generic themes.\n\
- Cross-synthesize ACROSS sources into higher-level conclusions rather than \
summarising one source at a time.\n\
- Do not merely report: evaluate. Say what the finding means, why it matters, \
how strong the support is, and what follows from it.\n\
- Where sources disagree, present the conflict and say which evidence is \
stronger or more recent.\n\
- Developed paragraphs, not bullet checklists. A short markdown table is \
welcome where the content is genuinely tabular.\n\
- Err on the side of more useful information rather than less.\n\
- Assert ONLY what the evidence supports. Never invent facts, numbers, names \
or dates. Cite EVERY material claim as [Source: ev-N], naming the evidence \
chunk the claim rests on — the same handle the evidence block labels it with.\n\
- If the evidence genuinely does not cover part of this sub-question, say so \
in ONE short sentence and move on.";

/// One retrieval passage: a span of a window chunk, tagged with the
/// chunk it came from so the citation maps to a real fetched source.
#[derive(Clone)]
struct Passage {
    chunk_id: String,
    url: String,
    text: String,
}

/// Split the window into overlapping passages. Retrieval granularity:
/// a whole chunk is too coarse to rank against one sub-question.
fn window_passages(window: &EvidenceWindow) -> Vec<Passage> {
    let mut out = Vec::new();
    for c in &window.chunks {
        let joined: String = super::scrub_control(&c.content)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let chars: Vec<char> = joined.chars().collect();
        let step = PASSAGE_CHARS.saturating_sub(PASSAGE_OVERLAP).max(1);
        let mut i = 0usize;
        while i < chars.len() {
            let end = (i + PASSAGE_CHARS).min(chars.len());
            let text: String = chars[i..end].iter().collect();
            if text.chars().count() >= 220 || out.is_empty() {
                out.push(Passage {
                    chunk_id: c.id.clone(),
                    url: c.source_url.clone(),
                    text,
                });
            }
            if end == chars.len() {
                break;
            }
            i += step;
        }
    }
    out
}

/// Sub-questions whose embeddings sit at or above this cosine are the
/// same question twice, and writing both produces the same section
/// twice. Pre-registered 2026-08-23 (`research/deep-research/adversarial/
/// pre-registration.md`, "Composed-report output quality (E)") as the
/// MIDPOINT of the observed gap: the duplicate pair that shipped on run
/// `dr-1787534265` measured 0.8591, and the tightest pair that composed
/// cleanly on `dr-1787535219` measured 0.7908.
///
/// The bias is deliberate. A false merge LOSES a section; a false keep
/// merely repeats one. So the floor sits above every observed safe pair
/// rather than hugging the duplicate.
///
/// n = 2 runs — a separation, not a calibration (§18.5). One const, one
/// name: a third observation re-derives it here.
pub const SUBQUESTION_DEDUP_FLOOR: f32 = 0.825;

/// Below this max question-to-passage cosine the evidence window does
/// not answer the question, and the honest deliverable is one line
/// saying so — not 2,381 words about adjacent topics, which is what run
/// `dr-1787534265` shipped for an auction question over an A2A/MCP
/// estate.
///
/// Pre-registered with the same evidence: that run's max measured
/// 0.3009; the run whose estate DID hold the answer measured 0.7885.
/// The floor sits well below the answerable case because a false refusal
/// on an answerable question is far worse than a verbose report.
pub const EVIDENCE_RELEVANCE_FLOOR: f32 = 0.45;

/// Drop sub-questions that repeat one already kept, by embedding cosine.
///
/// Returns the indices to KEEP, in order — the first member of each
/// near-duplicate cluster wins, so the plan's own ordering survives and
/// the choice does not depend on iteration order.
///
/// This runs before any section is written, which is the point: the
/// duplicate cost is a wasted draft call per repeat, and the duplicate
/// TEXT is what a reader sees. Uses the sub-question vectors
/// `compose_report` already embedded for ranking — no new embed call.
fn dedupe_subquestions(sub_vecs: &[Vec<f32>]) -> Vec<usize> {
    let mut keep: Vec<usize> = Vec::new();
    for (i, v) in sub_vecs.iter().enumerate() {
        let dup = keep
            .iter()
            .find(|&&k| super::cosine(v, &sub_vecs[k]) >= SUBQUESTION_DEDUP_FLOOR);
        match dup {
            Some(&k) => tracing::debug!(
                target: "deep_research",
                dropped = i, kept = k,
                cosine = super::cosine(v, &sub_vecs[k]),
                floor = SUBQUESTION_DEDUP_FLOOR,
                "compose_report: sub-question repeats an earlier one — one section, not two"
            ),
            None => keep.push(i),
        }
    }
    keep
}

/// The highest question-to-passage cosine in the window — how well the
/// evidence we actually hold answers what was actually asked.
fn peak_relevance(question_vec: &[f32], passage_vecs: &[Vec<f32>]) -> f32 {
    passage_vecs
        .iter()
        .map(|v| super::cosine(question_vec, v))
        .fold(f32::NEG_INFINITY, f32::max)
}

/// Top passages for one sub-question, source-diverse. Falls back to
/// document order when the embedding surface is unavailable — NAMED by
/// the caller, never silently scored (§18.3).
fn rank_passages(sub_vec: &[f32], passage_vecs: &[Vec<f32>], passages: &[Passage]) -> Vec<Passage> {
    let mut scored: Vec<(f32, usize)> = passage_vecs
        .iter()
        .enumerate()
        .map(|(i, v)| (super::cosine(sub_vec, v), i))
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut picked = Vec::new();
    let mut per_source: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (_, i) in scored {
        let p = &passages[i];
        let n = per_source.entry(p.url.as_str()).or_insert(0);
        if *n >= PER_SOURCE_CAP {
            continue;
        }
        *n += 1;
        picked.push(p.clone());
        if picked.len() >= SECTION_PASSAGES {
            break;
        }
    }
    picked
}

/// The honest deliverable when the evidence does not answer the question:
/// say so, in one line, and name the measurement that decided it.
///
/// This replaces the failure it is named for. Run `dr-1787534265` asked
/// about first-price auctions over an estate holding A2A/MCP material and
/// shipped 2,381 words about adjacent topics with 0 of 67 claims verified
/// — a report that looks like an answer and is not one. A reader is far
/// better served by one true sentence.
///
/// It is a REFUSAL, not a short report: it names the floor, the measured
/// value and the window size, so the operator can tell "we hold nothing
/// relevant" apart from "the composer broke".
fn unanswered_report(question: &str, peak: f32, chunks: usize) -> String {
    format!(
        "# {question}\n\n         ## No answer from this evidence\n\n         The evidence gathered for this run does not answer the question. The          closest passage in a {chunks}-passage window scored {peak:.3} against          the question, below the {EVIDENCE_RELEVANCE_FLOOR:.2} relevance floor          — near enough to unrelated that composing a report from it would          produce prose about adjacent topics rather than an answer.\n\n         No findings are reported because none were found. Re-run against a          corpus that holds material on this subject, or release the web leg          with `--consent public-web` so the run can go and look.\n"
    )
}

/// The composed deliverable: one section per sub-question plus a closing
/// synthesis, with a `## Sources` list whose numbers the section text
/// cites. Returns the markdown and the ordered source list.
pub async fn compose_report(
    port: &dyn ResearchPort,
    question: &str,
    window: &EvidenceWindow,
    subquestions: &[String],
) -> Result<String, String> {
    if window.chunks.is_empty() {
        return Err("compose_report: empty evidence window".to_string());
    }
    let passages = window_passages(window);
    let subs: Vec<String> = if subquestions.is_empty() {
        vec![question.to_string()]
    } else {
        subquestions.to_vec()
    };

    // One embed pass for the passages, one for the sub-questions — the
    // question rides along as the LAST row of the sub-question call, so
    // the relevance gate below costs no extra round-trip.
    let passage_texts: Vec<String> = passages.iter().map(|p| p.text.clone()).collect();
    let pv = port.embed(&passage_texts).await;
    let mut sub_inputs = subs.clone();
    sub_inputs.push(question.to_string());
    let sv = port.embed(&sub_inputs).await;
    let embedded = match (&pv, &sv) {
        (Ok(a), Ok(b)) if !a.iter().any(|v| v.is_empty()) && !b.iter().any(|v| v.is_empty()) => {
            true
        }
        _ => {
            tracing::warn!(
                target: "deep_research",
                "compose_report: no embedding surface — sections fall back to document order (DEGRADED, named)"
            );
            false
        }
    };

    // Two gates over the vectors just computed. Both are skipped when
    // the embedding surface is unavailable — a degraded run composes as
    // before rather than refusing on a measurement it could not take
    // (§18.3: could-not-judge is not the same verdict as failed).
    let mut kept: Vec<usize> = (0..subs.len()).collect();
    if embedded {
        let sv_ok = sv.as_ref().unwrap();
        let pv_ok = pv.as_ref().unwrap();

        // Does the evidence answer the question at all? Below the floor,
        // the honest deliverable is one line saying so.
        let question_vec = &sv_ok[subs.len()];
        let peak = peak_relevance(question_vec, pv_ok);
        if peak < EVIDENCE_RELEVANCE_FLOOR {
            tracing::info!(
                target: "deep_research",
                peak, floor = EVIDENCE_RELEVANCE_FLOOR, chunks = passages.len(),
                "compose_report: the evidence does not answer the question — refusing to compose"
            );
            return Ok(unanswered_report(question, peak, passages.len()));
        }
        tracing::debug!(
            target: "deep_research", peak, floor = EVIDENCE_RELEVANCE_FLOOR,
            "compose_report: evidence clears the relevance floor"
        );

        // Two sub-questions that mean the same thing make one section,
        // not two near-identical ones.
        kept = dedupe_subquestions(&sv_ok[..subs.len()]);
        if kept.len() < subs.len() {
            tracing::info!(
                target: "deep_research",
                planned = subs.len(), sections = kept.len(),
                "compose_report: near-duplicate sub-questions merged"
            );
        }
    }

    let allowed = allowed_urls(window);
    let system = "You are a local research synthesist writing one section of a \
                  report. Write from the evidence given and nothing else.";
    let mut sections: Vec<String> = Vec::new();

    for &si in kept.iter() {
        let sub = &subs[si];
        let picked = if embedded {
            rank_passages(&sv.as_ref().unwrap()[si], pv.as_ref().unwrap(), &passages)
        } else {
            // Degraded path: no embedding surface, so rank by nothing.
            // Rotate the window per section rather than handing every
            // section the SAME passages — identical inputs would make
            // identical sections and the report would say one thing
            // eight times.
            let start = (si * SECTION_PASSAGES) % passages.len().max(1);
            passages
                .iter()
                .cycle()
                .skip(start)
                .take(SECTION_PASSAGES.min(passages.len()))
                .cloned()
                .collect()
        };
        if picked.is_empty() {
            continue;
        }
        let mut ev = String::new();
        for p in picked.iter() {
            ev.push_str(&format!("[{}] ({})\n{}\n\n", p.chunk_id, p.url, p.text));
        }
        let prompt = format!(
            "You are writing ONE section of an analytical research report that answers:\n{question}\n\n\
             THIS SECTION: {sub}\n\nEVIDENCE:\n{ev}\n{WRITER_CONTRACT}\n\n\
             Write 300-380 words. Start with a '## ' heading that is a short noun phrase, \
             never the sub-question verbatim; use '### ' sub-headings where the material \
             has natural parts. No preamble and no commentary about the evidence itself."
        );
        let body = port
            .draft(DraftLeg::Section, &prompt, Some(system), &allowed)
            .await
            .map_err(|e| format!("section draft: {e}"))?;
        sections.push(body);
    }

    if sections.is_empty() {
        return Err("compose_report: no section produced".to_string());
    }

    // The closing synthesis (AIQ §6.3 item 3's "cross-synthesize into
    // higher-level conclusions"), the direct Insight-dimension lever:
    // Insight carries the highest mean dimension weight across the
    // DRB-I subset (0.351) and was our weakest dimension.
    let digest: String = sections
        .iter()
        .map(|s| s.chars().take(1500).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n\n");
    let synth_prompt = format!(
        "You are writing the closing synthesis of a research report answering:\n{question}\n\n\
         THE REPORT SO FAR:\n{digest}\n\n\
         Write a '## Synthesis and Assessment' section of 280-340 words that draws the \
         threads into 3-5 justified conclusions, each saying WHY it follows from what the \
         report established; weighs which rest on strong evidence and which are tentative; \
         names the genuine open questions and what would resolve them; and gives the \
         practical implication a demanding reader would want. Reuse the [Source: ev-N] \
         handles already used above where a claim needs one. Developed paragraphs, no \
         checklists, and no new facts beyond what the report states."
    );
    match port
        .draft(DraftLeg::Synthesis, &synth_prompt, Some(system), &allowed)
        .await
    {
        Ok(t) => sections.push(t),
        Err(e) => tracing::warn!(
            target: "deep_research", error = %e,
            "compose_report: synthesis section failed — the report lands without it, named"
        ),
    }

    // The composed text keeps its [Source: ev-N] handles: the gate's
    // ref-required step verifies the writer's OWN selection against the
    // window, and rewriting the handles into reader-facing numbers
    // before the audit would blind it. `number_citations` does that
    // rewrite at RENDER time, after the verdicts exist.
    Ok(format!("# {question}\n\n{}", sections.join("\n\n")))
}

/// Render-time rewrite: `[Source: ev-3]` → `[2]`, with the ordered
/// source list the numbers index. Runs AFTER the gate, never before.
pub fn number_citations(md: &str, window: &EvidenceWindow) -> (String, Vec<String>) {
    let url_of = |id: &str| -> Option<String> {
        window
            .chunks
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.source_url.clone())
    };
    let mut numbering: Vec<String> = Vec::new();
    let mut out = String::with_capacity(md.len());
    let mut rest = md;
    while let Some(open) = rest.find("[Source:") {
        out.push_str(&rest[..open]);
        let after = &rest[open..];
        match after.find(']') {
            Some(close) => {
                let inner = after[8..close].trim();
                match url_of(inner) {
                    Some(u) => {
                        let n = match numbering.iter().position(|x| x == &u) {
                            Some(i) => i + 1,
                            None => {
                                numbering.push(u);
                                numbering.len()
                            }
                        };
                        out.push_str(&format!("[{n}]"));
                    }
                    // A handle naming no window chunk is DROPPED from the
                    // reader's page; the verdict set still records the
                    // claim's refusal (ref-required), so the absence is
                    // on the record rather than hidden.
                    None => {}
                }
                rest = &after[close + 1..];
            }
            None => {
                out.push_str(after);
                return (out, numbering);
            }
        }
    }
    out.push_str(rest);
    if !numbering.is_empty() {
        out.push_str("\n\n## Sources\n\n");
        for (i, u) in numbering.iter().enumerate() {
            out.push_str(&format!("{}. {}\n", i + 1, u));
        }
    }
    (out, numbering)
}

pub async fn draft_round(
    port: &dyn ResearchPort,
    run_id: &str,
    charter_hash: &str,
    round: u32,
    question: &str,
    evidence: &EvidenceWindow,
    open_gaps: &[String],
    strict_shape: bool,
) -> Result<Draft, String> {
    let system = "You are a local research synthesist. Answer the question from the evidence provided. \
                  Cite EVERY factual claim with [Source: ev-<id>] where <id> is the evidence chunk id \
                  the claim rests on (each chunk is labelled [id] in the evidence block, and its \
                  figures are listed in the inventory). Use only chunk ids present in the evidence \
                  block. If the evidence cannot answer a part, say so explicitly rather than guessing."
        .to_string();
    let mut prompt = String::new();
    if round == 1 {
        prompt.push_str(&format!("Estate evidence:\n{}", evidence_block(evidence)));
    } else {
        prompt.push_str(&format!(
            "Evidence gathered so far:\n{}\n\nQuestion: {question}",
            evidence_block(evidence)
        ));
        if !open_gaps.is_empty() {
            prompt.push_str(
                "\n\nStill-open specifics to resolve (answer only if the evidence supports it):",
            );
            for gap in open_gaps {
                prompt.push_str(&format!("\n- {gap}"));
            }
        }
    }
    // The deterministic figure inventory (t1h — H2): the evidence's
    // figures are enumerated for the model, never left to the draft's
    // discretion. Both round shapes carry it — EXCEPT the resolve-only
    // rounds (REV-3, order deep-research-t6c, pre-registered): the
    // inventory is round-2's enumeration job; at round >= 3 the draft
    // resolves the still-open ledger and enumerates NO new facts (the
    // measured +2/+1 r3 growths are the draft's re-expressions of
    // evidence into NEW fact identities the fold correctly refuses and
    // the floor caps — the growth is killed at the source, and the
    // closing path is the loop's own verbatim re-audit of prior texts,
    // which needs no enumeration).
    let resolve_only = round >= 3;
    let inventory = if resolve_only {
        String::new()
    } else {
        figure_inventory(evidence)
    };
    if !inventory.is_empty() {
        prompt.push_str(&format!("\n\n{inventory}"));
    }
    if resolve_only {
        prompt.push_str(
            "\n\nResolution constraint: restate each still-open specific \
             above exactly as the evidence supports it and nothing beyond \
             — no new facts, no new figures, no claims not already listed \
             above.",
        );
    }
    if evidence.chunks.is_empty() {
        prompt.push_str("\n\n(No evidence was retrieved this round. Say so plainly.)");
    }
    // REV-2 (pre-registered): the re-draft's shape constraint — the
    // seed-07 corruption class violated every one of these shapes;
    // the constraint is appended LAST so the model sees it last.
    if strict_shape {
        prompt.push_str(
            "\n\nShape constraint (re-draft): plain prose only — complete \
             sentences, no markdown, no bold, no bullet lists, no \
             parenthetical asides, and no self-interrogation or asides \
             about the evidence text itself; state each fact at most once. \
             Spelled-out figures are forbidden in the re-draft: every \
             figure must appear as digits (e.g. \"20%\", \"58.1%\"), or \
             not at all.",
        );
    }
    let urls = allowed_urls(evidence);
    let text = port
        .draft(DraftLeg::Round, &prompt, Some(&system), &urls)
        .await
        .map_err(|e| format!("draft failed: {e}"))?;
    let citations: Vec<DraftCitation> = evidence
        .chunks
        .iter()
        .map(|c| DraftCitation {
            evidence_id: c.id.clone(),
            url: c.source_url.clone(),
            custody: Some(c.custody.clone()),
        })
        .collect();
    Ok(Draft {
        icd: "draft".to_string(),
        version: super::icd::ICD_VERSION,
        run_id: run_id.to_string(),
        charter_hash: charter_hash.to_string(),
        round,
        provider: "port:draft".to_string(),
        url_constraint: UrlConstraintPolicy {
            enabled: true,
            layer: "sovereign-inference:UrlAllowlistConstraint".to_string(),
        },
        text,
        citations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deep_research::estate::{EstateListing, PortHit};
    use crate::types::Custody;
    use std::sync::{Arc, Mutex};

    /// A recording fake port: captures the prompt it was asked to
    /// complete. Everything else is unreachable — the test drives
    /// draft_round directly.
    struct RecordingPort {
        prompt: Arc<Mutex<Option<String>>>,
    }

    impl RecordingPort {
        fn new() -> Self {
            RecordingPort {
                prompt: Arc::new(Mutex::new(None)),
            }
        }
        fn last_prompt(&self) -> String {
            self.prompt.lock().unwrap().clone().unwrap_or_default()
        }
    }

    #[async_trait::async_trait]
    impl ResearchPort for RecordingPort {
        async fn estate_listing(&self, _c: &[String]) -> Result<EstateListing, String> {
            unimplemented!("unreachable: draft_round calls only draft")
        }
        async fn estate_search(
            &self,
            _c: &[String],
            _q: &str,
            _l: usize,
        ) -> Result<Vec<PortHit>, String> {
            unimplemented!("unreachable")
        }
        async fn web_search(&self, _b: &str, _q: &str, _l: usize) -> Result<Vec<PortHit>, String> {
            unimplemented!("unreachable")
        }
        async fn web_fetch(&self, _u: &str) -> Result<String, String> {
            unimplemented!("unreachable")
        }
        async fn terminal_poll(&self) -> Result<(), String> {
            Ok(())
        }
        async fn draft(
            &self,
            _leg: DraftLeg,
            prompt: &str,
            _s: Option<&str>,
            _a: &[String],
        ) -> Result<String, String> {
            *self.prompt.lock().unwrap() = Some(prompt.to_string());
            Ok("draft".to_string())
        }
    }

    fn window() -> EvidenceWindow {
        EvidenceWindow {
            icd: "evidence_window".to_string(),
            version: 1,
            run_id: "r".to_string(),
            charter_hash: "h".to_string(),
            round: 1,
            chunks: vec![super::super::icd::WindowChunk {
                id: "ev-1".to_string(),
                locator: "https://example.com/a".to_string(),
                source_url: "https://example.com/a".to_string(),
                custody: Custody::PublicWeb.as_str().to_string(),
                provenance_class: "known".to_string(),
                content: "The Meridian Bridge was completed in 1873.".to_string(),
                ingested_into: None,
                tags: Vec::new(),
            }],
            fetch_failures: Vec::new(),
            dedup_refused: Vec::new(),
            content_refused: Vec::new(),
            derived_custody: Custody::PublicWeb.as_str().to_string(),
        }
    }

    #[test]
    fn evidence_block_is_deterministic() {
        let w = window();
        let block = evidence_block(&w);
        assert!(block.contains("[ev-1] The Meridian Bridge"));
        assert_eq!(evidence_block(&w), evidence_block(&w));
    }

    // ------------------------------------------------------------------
    // Composed-report output quality (E) — the pre-registered bars.
    // `research/deep-research/adversarial/pre-registration.md`,
    // "Composed-report output quality (E)", written 2026-08-23 BEFORE
    // this code. Both cases are MEASURED fixtures from live runs, not
    // invented vectors: the cosines below are what the loop's own
    // embedder produced on those runs' actual plans.
    // ------------------------------------------------------------------

    /// Two unit vectors at a chosen cosine, so a fixture can name the
    /// measured separation directly instead of shipping 1024 floats.
    fn pair_at(cosine: f32) -> (Vec<f32>, Vec<f32>) {
        let a = vec![1.0, 0.0];
        let b = vec![cosine, (1.0 - cosine * cosine).sqrt()];
        (a, b)
    }

    /// PRE-REGISTERED BAR, half 1: run `dr-1787534265`'s two
    /// sub-questions measured **0.8591** apart and shipped "Absence of
    /// Auction Theory in Evidence" and "Absence of Auction Theory
    /// Evidence" — the same paragraph twice. 2 must collapse to 1.
    #[test]
    fn the_duplicate_pair_that_shipped_twice_becomes_one_section() {
        let (a, b) = pair_at(0.8591);
        let keep = dedupe_subquestions(&[a, b]);
        assert_eq!(
            keep,
            vec![0],
            "0.8591 is the measured cosine of the pair that shipped as two \
             near-identical sections; it must merge"
        );
    }

    /// PRE-REGISTERED BAR, half 2: run `dr-1787535219`'s tightest pair
    /// measured **0.7908** and composed cleanly. 10 must stay 10 — and
    /// this is the half that fails if the floor is set too low, which is
    /// the error that LOSES a section.
    #[test]
    fn the_tightest_clean_pair_keeps_both_sections() {
        let (a, b) = pair_at(0.7908);
        let keep = dedupe_subquestions(&[a, b]);
        assert_eq!(
            keep,
            vec![0, 1],
            "0.7908 composed cleanly on a live run; merging it would lose a section"
        );
    }

    /// The first member of a cluster wins, so the plan's own ordering
    /// decides — not iteration order, and not a counter.
    #[test]
    fn dedup_keeps_the_first_of_each_cluster_and_is_order_stable() {
        let (a, b) = pair_at(0.90);
        let far = vec![0.0, 1.0];
        assert_eq!(
            dedupe_subquestions(&[a.clone(), b.clone(), far.clone()]),
            vec![0, 2]
        );
        // The distinct one moving to the front changes which indices
        // survive but never how many.
        assert_eq!(dedupe_subquestions(&[far, a, b]), vec![0, 1]);
    }

    /// PRE-REGISTERED BAR, the relevance floor: run `dr-1787534265`'s
    /// window peaked at **0.3009** against its question and shipped
    /// 2,381 words anyway; run `dr-1787535219`'s peaked at **0.7885**
    /// and composed a real report.
    #[test]
    fn the_relevance_floor_separates_the_two_measured_windows() {
        let q = vec![1.0, 0.0];
        let unanswerable: Vec<Vec<f32>> = vec![pair_at(0.3009).1, pair_at(0.21).1];
        let answerable: Vec<Vec<f32>> = vec![pair_at(0.7885).1, pair_at(0.30).1];
        assert!(
            peak_relevance(&q, &unanswerable) < EVIDENCE_RELEVANCE_FLOOR,
            "the window that could not answer must be refused"
        );
        assert!(
            peak_relevance(&q, &answerable) >= EVIDENCE_RELEVANCE_FLOOR,
            "the window that DID answer must compose — a false refusal is the worse error"
        );
    }

    /// The refusal is a refusal: it names the floor, the measurement and
    /// the window size, so "we hold nothing relevant" cannot be mistaken
    /// for "the composer broke" (§18.3, absence is reported).
    #[test]
    fn the_unanswered_report_names_what_it_measured() {
        let r = unanswered_report("Do we know about auctions?", 0.3009, 4);
        assert!(r.starts_with("# Do we know about auctions?"));
        assert!(r.contains("No answer from this evidence"));
        assert!(r.contains("0.301"), "the measured peak is named: {r}");
        assert!(r.contains("0.45"), "the floor is named: {r}");
        assert!(r.contains("4-passage"), "the window size is named: {r}");
        assert!(
            !r.contains("## Findings"),
            "a refusal must not ship an empty findings section"
        );
    }

    #[test]
    fn allowed_urls_is_the_window() {
        assert_eq!(
            allowed_urls(&window()),
            vec!["https://example.com/a".to_string()]
        );
    }

    /// RED (order deep-research-t1h, H2 — draft figure-completeness,
    /// pre-registered in adversarial/pre-registration.md): "a
    /// window-held figure the plan's sub-questions missed enters the
    /// draft". The drafting surface must carry a deterministic figure
    /// inventory — figure_tokens per window chunk, the one decider —
    /// so the model is never left to volunteer the evidence's digits.
    /// The t1f residual: keys whose figures sat in the window while
    /// the draft's sub-questions did not carry them (20 Class-A keys,
    /// t1h-failure-taxonomy.md). Watched red: fails at HEAD — the
    /// prompt carries the evidence block with no inventory.
    #[tokio::test]
    async fn draft_prompt_carries_the_window_figure_inventory() {
        let mut w = window();
        w.chunks[0].content =
            "Gini coefficients in the largest metro areas exceeded 0.5469 in 2019.".to_string();
        let port = RecordingPort::new();
        draft_round(&port, "r", "h", 1, "How did cities change?", &w, &[], false)
            .await
            .unwrap();
        let prompt = port.last_prompt();
        assert!(
            prompt.contains("Figures present in the evidence"),
            "the draft prompt must carry the figure inventory: {prompt}"
        );
        assert!(
            prompt.contains("0.5469"),
            "the window's figure must be enumerated in the inventory: {prompt}"
        );
    }

    // --- REV-3 (order deep-research-t6c, pre-registered): the
    // resolve-only rounds. The r3 draft resolves the still-open ledger
    // and enumerates NO new facts — the measured +2/+1 r3 growths are
    // the draft's re-expressions of evidence into NEW fact identities
    // the fold correctly refuses and the floor caps; suppression at
    // the source. The inventory is round-2's enumeration job.

    #[tokio::test]
    async fn resolve_only_rounds_suppress_the_inventory_and_carry_the_constraint() {
        let mut w = window();
        w.chunks[0].content =
            "Gini coefficients in the largest metro areas exceeded 0.5469 in 2019.".to_string();
        let port = RecordingPort::new();
        let gaps = vec!["Still-open: the 2019 baseline.".to_string()];
        draft_round(
            &port,
            "r",
            "h",
            2,
            "How did cities change?",
            &w,
            &gaps,
            false,
        )
        .await
        .unwrap();
        let round2 = port.last_prompt();
        assert!(
            round2.contains("Figures present in the evidence"),
            "round 2 keeps the inventory (its enumeration job): {round2}"
        );
        assert!(
            !round2.contains("Resolution constraint"),
            "round 2 is not resolve-only: {round2}"
        );

        draft_round(
            &port,
            "r",
            "h",
            3,
            "How did cities change?",
            &w,
            &gaps,
            false,
        )
        .await
        .unwrap();
        let round3 = port.last_prompt();
        assert!(
            round3.contains("Resolution constraint"),
            "round 3 must carry the resolution constraint: {round3}"
        );
        assert!(
            !round3.contains("Figures present in the evidence"),
            "round 3 suppresses the inventory: {round3}"
        );
    }

    // --- REV-2 (order deep-research-t6c, pre-registered): the
    // degenerate-draft guard. RED: these reference
    // `draft_is_degenerate` and the `strict_shape` param, which do not
    // exist at HEAD — the tests must fail to COMPILE before the fix
    // (watched red, then green).

    /// RED (a): the real seed-07 corruption window (flight record
    /// dr-1787102765/draft-3.json — the rev-1 2→38 ledger blowout) must
    /// be detected. The window trips both shape rules: 3 distinct
    /// inner-monologue markers (>= 2) and 29 "**" per 1500 chars
    /// (19.3/k >= 8/k).
    #[test]
    fn degenerate_corruption_draft_is_detected() {
        let text = r#"*announced** **?** per Bloomberg)`
Actually, I cannot see the date clearly in my internal simulation because the user pasted it as plain text. Let me re-read the *User* input carefully.

Input Text: `... The resolution was an oracle-led deal (**announced** **?** ...`
Wait, looking at the very bottom of the evidence block `[ev-1]`:
It ends with `... completed through **?**. `

Okay, let's look at the "Figures present" list again. It contains dates like `2045`, `2076`. This suggests the evidence might have typos or specific future-dated scenarios (perhaps a hypothetical scenario?). Or maybe I am misreading the year 2025 as something else?
No, the text explicitly says `signed by President Biden on **2024**-**04**-**24**`. And `Supreme Court ... on **2025**-**01**-**17**`. These are consistent with real-world recent events (though the Jan 18/19 blackout is fictional/hypothetical in this context compared to reality where it was delayed further).

Let's find the announcement date of the Oracle deal in [ev-1].
Text: `(announced **[DATE]** per Bloomberg)`
I will look for a date pattern. The figure list has `... , ?, ? , 12.5%, ? .`
Actually, I don't need to guess if the text isn't clear? No, I must use the evidence.
Let me re-read the specific sentence in [ev-1]:
"The resolution was an oracle-led deal (**announced** **?** per Bloomberg) giving Oracle about a **12.5%** stake..."

Is it possible the date is missing or obscured?
"Still-open specifics to resolve (answer only if the evide"#;
        assert!(
            draft_is_degenerate(text),
            "the seed-07 corruption class must be detected as degenerate"
        );
    }

    /// RED (b): the real clean synthesis class (v1 flight record
    /// dr-1787104761/draft-3.json) — markdown headings, bold labels,
    /// bullet lists, dense citations — must NOT be flagged. Zero
    /// markers, 6 "**" per 1900 chars (3.2/k < 8/k).
    #[test]
    fn clean_synthesis_draft_is_not_flagged() {
        let text = r#"American cities have undergone a fundamental transformation over the last four decades (1980–2024), characterized by accelerated gentrification, widening economic inequality, deteriorating housing affordability, and distinct demographic shifts.

### Gentrification
Gentrification has become significantly more prevalent since 2000, although it remains geographically concentrated in specific regions [Source: ev-2]. The term was first coined in 1963, but rates accelerated sharply as Americans pursued urban lifestyles; for the period following the 2000 Census, nearly 20% of lower-income neighborhoods experienced gentrification compared to only 9% during the 1990s [Source: ev-1] [Source: ev-2]. This represents a doubling of the rate from the previous decade [Source: ev-2].

*   **Geographic Concentration:** A select group of cities saw extensive changes. Portland, Oregon led with 58.1% of eligible tracts gentrifying (36 out of 142 total tracts) [Source: ev-1] [Source: ev-2]. Washington, D.C. followed at 51.9%, Minneapolis at 50.6%, and Seattle at 50% [Source: ev-1] [Source: ev-2]. In terms of raw numbers, New York City recorded the highest total with 128 gentrified tracts [Source: ev-1].
*   **Limited Reach:** Conversely, cities like Detroit (2.8%), Las Vegas (2%), El Paso (0%), and Arlington, Texas (0%) experienced little to no gentrification [Source: ev-1]. Nationally, only 8% of all neighborhoods reviewed experienced gentrification since the 2000 Census [Source: ev-1].

### Demographic Shifts in Gentrifying Areas
Gentrified neighborhoods typically saw increases in non-Hispanic white populations and declines in poverty rates, whereas lower-income areas that did not gentrify often saw population losses and rising minority concentrations [Source: ev-1]. Specifically, between 2009 and 2013 data points:
*   **Gentrifying Tracts (n=948):** Experienced a +6.5% population change"#;
        assert!(
            !draft_is_degenerate(text),
            "the clean synthesis class must not be flagged"
        );
    }

    /// RED (c): the density bar is a SHAPE rule, not "any bold": a
    /// heading-and-emphasis draft below 8 "**" per 1k chars stays
    /// clean even though it is heavily structured. The test validates
    /// its own precondition (density < 8/k) before asserting the guard
    /// lets it pass — the near-boundary behavior is pinned.
    #[test]
    fn markdown_heading_bold_does_not_trip_density_bar() {
        let mut text = String::new();
        for i in 0..20 {
            // One bold pair in 8 of the 20 sections: 16 "**" occurrences.
            let emphasis = if i % 5 == 0 {
                "The **headline figure** was 42.7%. "
            } else {
                ""
            };
            text.push_str(&format!(
                "### Section {i}\n{emphasis}The district reported a 42.7% change in the \
                 eligible population, the highest in the region, against the 2019 baseline.\n"
            ));
        }
        let per_1k = text.matches("**").count() as f64 * 1000.0 / text.len() as f64;
        assert!(
            per_1k < 8.0,
            "fixture precondition: {per_1k:.1} bold per 1k chars must sit under the 8/k bar"
        );
        assert!(
            !draft_is_degenerate(&text),
            "bold structure alone under the density bar must not trip the guard"
        );
    }

    /// RED (d): a single monologue marker in a long clean draft is NOT
    /// the corruption signature — the bar is >= 2 DISTINCT markers or
    /// >= 3 total. One "Actually," (a terse transition) must pass.
    #[test]
    fn single_monologue_word_does_not_trip_marker_bar() {
        let mut text = String::new();
        for i in 0..24 {
            text.push_str(&format!(
                "District {i} recorded a 12.4% change in the eligible population, \
                 the highest in the region. The figure reflects the 2019 baseline. "
            ));
        }
        text.push_str("Actually, the 2019 baseline appears twice in the evidence.");
        assert!(
            !draft_is_degenerate(&text),
            "one marker occurrence must not trip the >=2-distinct / >=3-total bar"
        );
    }

    // --- REV-4 (order deep-research-t6c, pre-registered): the three
    // battery-3 corruption classes. RED: `draft_opens_with_prompt_echo`
    // and `count_fragment_bullets` do not exist at HEAD — these tests
    // fail to COMPILE before the fix (watched red, then green). The
    // swallow shape is a bar-marker (amendment §18.6): the swallow-
    // alone fixture below is the pinned clean class and must NOT fire.

    /// RED (f): the prompt-echo prefix — the corrupt v1 draft-3's
    /// first line (flight record dr-1787148073; the split line became
    /// gap g19, one of the measured +3). Fires alone.
    #[test]
    fn prompt_echo_prefix_is_degenerate() {
        let text = r#"Based on the evidence provided, here is how American cities changed across four decades (1980–2024) regarding gentrification, inequality, affordability, and displacement.

### Gentrification
*   **Acceleration:** The rate of gentrification doubled after 2000 compared to the 1990s [Source: ev-1]."#;
        assert!(
            draft_is_degenerate(text),
            "the prompt-echo prefix must fire the guard"
        );
    }

    /// RED (g): the clean evidence framing is NOT the echo — the
    /// corrupt flight's OWN clean draft-2 opens "Based on the
    /// evidence provided, American cities have undergone…" (no
    /// "here is how").
    #[test]
    fn clean_evidence_framing_is_not_the_echo() {
        let text = r#"Based on the evidence provided, American cities have undergone a fundamental transformation across four decades (1980–2024), with accelerated gentrification and widening inequality.

### Gentrification Trends (1980–2024)
*   **Acceleration:** The rate of gentrification doubled after 2000 compared to the 1990s [Source: ev-1]."#;
        assert!(
            !draft_is_degenerate(text),
            "the clean framing must not be mistaken for the echo"
        );
    }

    /// RED (h): the swallow package — the corrupt draft-3's exact
    /// opening (echo line + swallowed header pair). The echo fires
    /// alone; the swallow adds a marker toward the bar.
    #[test]
    fn swallowed_header_package_is_degenerate() {
        let text = r#"Based on the evidence provided, here is how American cities changed across four decades (1980–2024).

### Economic Inequality
Inequality widened significantly during this period, with metropolitan areas showing steeper increases than national averages [Source: ev-1]."#;
        assert!(
            draft_is_degenerate(text),
            "the echo + swallowed-header package must fire the guard"
        );
    }

    /// RED (i): a swallow pair ALONE is the pinned clean shape — the
    /// clean synthesis fixture (dr-1787104761 draft-3) has exactly
    /// this pair ("### Gentrification" + "Gentrification has
    /// become…"). The swallow counts toward the >=2-distinct bar, it
    /// never fires alone (amendment §18.6).
    #[test]
    fn single_swallow_pair_does_not_fire_the_guard() {
        let text = r#"American cities have undergone a fundamental transformation over the last four decades (1980–2024).

### Gentrification
Gentrification has become significantly more prevalent since 2000, although it remains geographically concentrated in specific regions [Source: ev-2]."#;
        assert!(
            !draft_is_degenerate(text),
            "the clean header + topic sentence must not trip the guard"
        );
    }

    /// RED (j): the dependent-clause fragment bullet — seed-01's
    /// draft-3 bullet (flight record dr-1787146175; the splitter's
    /// fragment became gap g6, seed-01's +1). Fires alone.
    #[test]
    fn dependent_clause_bullet_is_degenerate() {
        let text = r#"*   Although announced in March 2025, the deal completed its regulatory and shareholder steps later, with completion reported in June [Source: ev-1].

Regulatory approval followed the announcement [Source: ev-1]."#;
        assert!(
            draft_is_degenerate(text),
            "the subordinator-opened bullet must fire the guard"
        );
    }

    /// RED (k): a complete-sentence bullet (capitalized, no
    /// subordinator) is NOT a fragment — the clean bullet class stays
    /// clean. (No bold in the fixture: the density bar is not this
    /// test's subject.)
    #[test]
    fn complete_sentence_bullet_is_not_a_fragment() {
        let text = r#"*   The rate of gentrification doubled after 2000 compared to the 1990s [Source: ev-1].
*   Gentrification remained rare nationally as a whole, affecting only 8 percent of all reviewed neighborhoods [Source: ev-1]."#;
        assert!(
            !draft_is_degenerate(text),
            "a complete-sentence bullet is not a fragment"
        );
    }

    /// RED (e): the shape-constrained re-draft prompt carries the
    /// plain-prose constraint ONLY when strict_shape is set; the
    /// default prompt is the pre-rev-2 shape (evidence block +
    /// inventory, no constraint).
    #[tokio::test]
    async fn shape_constraint_appears_only_on_retry_prompt() {
        let mut w = window();
        w.chunks[0].content =
            "Gini coefficients in the largest metro areas exceeded 0.5469 in 2019.".to_string();
        let port = RecordingPort::new();
        draft_round(&port, "r", "h", 1, "How did cities change?", &w, &[], false)
            .await
            .unwrap();
        let default_prompt = port.last_prompt();
        assert!(
            !default_prompt.contains("Shape constraint"),
            "the default prompt must stay byte-shaped as before: {default_prompt}"
        );
        assert!(
            default_prompt.contains("Figures present in the evidence"),
            "the default prompt must still carry the figure inventory"
        );
        assert!(
            !default_prompt.contains("Spelled-out figures"),
            "the default prompt must stay byte-shaped as before: {default_prompt}"
        );

        draft_round(&port, "r", "h", 1, "How did cities change?", &w, &[], true)
            .await
            .unwrap();
        let retry_prompt = port.last_prompt();
        assert!(
            retry_prompt.contains("Shape constraint"),
            "the retry prompt must carry the plain-prose constraint: {retry_prompt}"
        );
        assert!(
            retry_prompt.contains("Figures present in the evidence"),
            "the constraint APPENDS; the inventory must survive it"
        );
        // RED-first (order deep-research-t6d — the figures-as-digits
        // clause): the strict-shape re-draft spelled every figure as
        // words (battery #4's v1, 40/40 could-not-judge); the clause
        // forbids that shape in the re-draft.
        assert!(
            retry_prompt.contains("Spelled-out figures"),
            "the retry prompt must carry the figures-as-digits clause: {retry_prompt}"
        );
    }

    // ---- drb1-t5: the composed deliverable -------------------------

    fn two_source_window() -> EvidenceWindow {
        let mut w = window();
        w.chunks.push(super::super::icd::WindowChunk {
            id: "ev-2".to_string(),
            locator: "https://example.org/b".to_string(),
            source_url: "https://example.org/b".to_string(),
            custody: Custody::PublicWeb.as_str().to_string(),
            provenance_class: "known".to_string(),
            content: "A second source, on the same bridge, giving the span as 240 metres."
                .to_string(),
            ingested_into: None,
            tags: Vec::new(),
        });
        w
    }

    /// The reader-facing numbering happens AFTER the gate: the composed
    /// text keeps its [Source: ev-N] handles so ref-required can verify
    /// the writer's own selection.
    #[test]
    fn number_citations_maps_handles_in_first_use_order() {
        let md = "The bridge opened in 1873 [Source: ev-1]. Its span is 240 metres \
                  [Source: ev-2]. Opened 1873 again [Source: ev-1].";
        let (out, srcs) = number_citations(md, &two_source_window());
        assert!(
            out.contains("1873 [1]."),
            "first source numbers 1, got: {out}"
        );
        assert!(
            out.contains("240 metres [2]."),
            "second source numbers 2, got: {out}"
        );
        assert!(
            out.contains("again [1]."),
            "a repeat source keeps its number"
        );
        assert_eq!(
            srcs,
            vec![
                "https://example.com/a".to_string(),
                "https://example.org/b".to_string()
            ]
        );
        assert!(out.contains("## Sources"), "the page lists what it cited");
    }

    /// A handle naming no window chunk is dropped from the READER's
    /// page — it must never be renumbered onto some other source. The
    /// verdict set still records the claim's ref-required refusal, so
    /// the absence stays on the record (§18.3).
    #[test]
    fn number_citations_drops_a_handle_that_names_no_chunk() {
        let md = "A claim resting on nothing in the window [Source: ev-99].";
        let (out, srcs) = number_citations(md, &two_source_window());
        assert!(!out.contains("ev-99"), "the dangling handle is gone: {out}");
        assert!(
            !out.contains("[1]"),
            "it is NOT renumbered onto a real source: {out}"
        );
        assert!(srcs.is_empty(), "and it contributes no source row");
    }

    /// Retrieval granularity: a whole chunk is too coarse to rank
    /// against one sub-question, so the window is split with overlap.
    #[test]
    fn window_passages_split_long_chunks_with_overlap() {
        let mut w = window();
        w.chunks[0].content = "lorem ipsum dolor sit amet ".repeat(400);
        let ps = window_passages(&w);
        assert!(
            ps.len() > 1,
            "a long chunk yields several passages, got {}",
            ps.len()
        );
        assert!(
            ps.iter().all(|p| p.chunk_id == "ev-1"),
            "every passage remembers the chunk it came from"
        );
        assert!(
            ps.iter().all(|p| p.text.chars().count() <= PASSAGE_CHARS),
            "no passage exceeds the span budget"
        );
    }

    /// The writer contract is stated ONCE and carries the obligations
    /// the Insight dimension actually rewards (AIQ §6.3 items 3-6).
    #[test]
    fn writer_contract_carries_the_analysis_obligations() {
        for needle in [
            "Do NOT",
            "Cross-synthesize",
            "evaluate",
            "disagree",
            "Developed paragraphs",
            "[Source: ev-N]",
        ] {
            assert!(
                WRITER_CONTRACT.contains(needle),
                "the writer contract must carry {needle:?}"
            );
        }
    }
}
