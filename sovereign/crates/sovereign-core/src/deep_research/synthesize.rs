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
use super::icd::{Draft, DraftCitation, EvidenceWindow, ResearchNote, UrlConstraintPolicy};

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
    let bodies: Vec<(String, String)> = window
        .chunks
        .iter()
        .map(|c| (c.id.clone(), c.content.clone()))
        .collect();
    figure_inventory_of(&bodies)
}

/// The inventory over the bodies a prompt ACTUALLY carries.
///
/// The instruction attached to this list is "every evidence-supported figure
/// must appear in the answer". Listing a figure the prompt's evidence no
/// longer contains turns that into an instruction to produce a number the
/// model cannot see — a demand to invent, aimed at the one part of the output
/// the audit checks hardest. So the inventory is derived from the same
/// admitted text the model reads, never from the full window.
pub fn figure_inventory_of(bodies: &[(String, String)]) -> String {
    let mut out = String::new();
    let mut any = false;
    let mut seen: std::collections::BTreeMap<&str, Vec<String>> = Default::default();
    for (id, body) in bodies {
        let tokens = super::figure_tokens(body);
        if tokens.is_empty() {
            continue;
        }
        seen.entry(id.as_str()).or_default().extend(tokens);
    }
    for (id, mut tokens) in seen {
        tokens.dedup();
        any = true;
        out.push_str(&format!("- [{id}]: {}\n", tokens.join(", ")));
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
/// Chars per token, MEASURED rather than assumed: the web arm's own failure
/// tokenized 1,360,782 chars to 302,153 tokens — 4.50. We divide by 4, which
/// over-estimates the token cost of a char and therefore UNDER-fills the
/// budget. Wrong in the safe direction, deliberately.
pub(crate) const CHARS_PER_TOKEN: usize = 4;

/// The round draft's evidence budget, against this deployment's 65,532-token
/// window. 24k tokens leaves ample room for the system message, the figure
/// inventory, the open-gap list and the output — and costs roughly 3.6 min of
/// prefill at this host's measured ~110 tok/s, which is the real reason it is
/// not simply set to the window: breadth per call is bought in wall clock,
/// linearly.
pub(crate) const ROUND_EVIDENCE_TOKENS: usize = 24_000;

/// **The deliverable's length is a decision, not an accident of section
/// count.** Measured 2026-08-24 on the outline A/B, and it cost that
/// experiment its answer: the per-section budget was a fixed "300-380 words"
/// regardless of how many sections there were, so the 20-section control
/// wrote 9,084-9,354 words while the 7-section outline arm wrote 3,702-4,053
/// — and `overall = T/(T+R)` compares head to head against references
/// running 6,898-13,348 words. Control sat inside that band; the outline arm
/// sat at roughly half its floor. Structure and length moved together, so
/// the arm could not test structure.
///
/// The per-section budget is therefore DERIVED: target total / sections, so
/// a 7-section and a 20-section plan produce comparable deliverables and an
/// A/B over structure is an A/B over structure.
pub(crate) const TARGET_REPORT_WORDS: usize = 9_000;

/// The deliverable's target total, with `SOVEREIGN_DR_TARGET_WORDS` as an
/// A/B override. Why it exists: the first matched-length outline arm landed
/// 11,375-11,837 words against a 9,000 ask (a ~26% writer overshoot), and
/// the shorter pre-fix control at 9,219 words scored 47.52 against that
/// arm's 42.10 — a 5.42-point separation with every shorter run beating
/// every longer one. Testing that needs BOTH lengths under ONE binary in
/// one session; a const edit between arms reintroduces exactly the
/// cross-binary confound being measured. Unset, empty, unparseable or zero
/// all keep [`TARGET_REPORT_WORDS`] — a bad value is never a silent zero,
/// which would collapse every section to `SECTION_WORDS_MIN`.
fn target_report_words() -> usize {
    target_words_policy(std::env::var("SOVEREIGN_DR_TARGET_WORDS").ok().as_deref())
}

/// Pure policy for the target override so the precedence (unset > empty >
/// unparseable > zero > explicit) is unit-testable without touching process
/// env — the same split `memory_watch::hard_limit_policy` uses, and for the
/// same reason: an env-var read is not testable in a parallel suite, so the
/// DECISION is separated from the READ.
fn target_words_policy(raw: Option<&str>) -> usize {
    raw.and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(TARGET_REPORT_WORDS)
}
/// Band for one section. The floor keeps a many-sectioned plan from writing
/// stubs; the cap keeps a three-section plan from being asked for an essay
/// the evidence window cannot support.
pub(crate) const SECTION_WORDS_MIN: usize = 300;
pub(crate) const SECTION_WORDS_MAX: usize = 1_400;

/// Words to ask of one section, given how many the plan has.
pub(crate) fn section_word_budget(sections: usize) -> usize {
    (target_report_words() / sections.max(1)).clamp(SECTION_WORDS_MIN, SECTION_WORDS_MAX)
}

/// The outline's own evidence slice — enough to know what the evidence
/// COVERS, not to read it. The writer reads properly, section by section.
pub(crate) const OUTLINE_EVIDENCE_TOKENS: usize = 6_000;
/// Section-count band. A report with fewer than five sections cannot give
/// distinct subjects standalone treatment AND relate them; more than eight
/// returns to the fragmentation this replaces.
pub(crate) const OUTLINE_MIN: usize = 5;
pub(crate) const OUTLINE_MAX: usize = 8;
/// The cap when the report architecture is on. A question that names two
/// subjects needs a section for each BEFORE the sections that compare them,
/// and at 8 the second subject is what gets squeezed out — measured as our
/// single worst RACE criterion, `Breadth and Depth of MCP Protocol
/// Description`, in 25 of 25 task-69 draws.
pub(crate) const OUTLINE_MAX_ARCHITECTED: usize = 12;

/// ONE decider for how many sections the plan may hold, so the prompt that
/// ASKS for n and the parser that ADMITS n can never disagree (§10.6).
pub(crate) fn outline_max() -> usize {
    if super::report_architecture_enabled() {
        OUTLINE_MAX_ARCHITECTED
    } else {
        OUTLINE_MAX
    }
}
/// Below this a line is scaffolding, not a planned section.
const OUTLINE_MIN_CHARS: usize = 25;
const TITLE_MIN_CHARS: usize = 20;
const TITLE_MAX_CHARS: usize = 160;

const PASSAGE_CHARS: usize = 1400;
const PASSAGE_OVERLAP: usize = 200;
/// Passages handed to one section's writer.
///
/// 16, NOT 8 — THE KNEE OF A MEASURED FIVE-POINT CURVE, 2026-08-27. The
/// compose replay is a zero-noise instrument (both halves byte-identical
/// across a daemon restart, note 680940ce), so these are exact for the
/// task-69 bed rather than a sample of one draw. RACE overall, same judge
/// (`Qwen3.8-27B-UD-Q6_K_XL`), same bed `dr-1787807617`:
///
/// ```text
///   arm     overall   delta   words    min
///   8x3     45.9166   +0.00   10829   10.6   <- the shipped default until now
///   16x4    51.3347   +5.42   10707   11.2   <- this
///   28x5    50.9864   +5.07   10200   12.3   <- the _WIDE flag below
///   44x6    50.9510   +5.03   10178   14.1
///   60x8    51.9689   +6.05   10064   16.2
/// ```
///
/// The whole effect is the first step. 16/28/44 sit inside 0.4 of each other
/// across a 2.75x increase in evidence, and the middle is NOT monotone (44x6
/// is the lowest of those four) — that is a plateau with task-level jitter,
/// so reading 60x8's nominal +0.63 over 16x4 as a trend would be exactly the
/// single-run delta §18.5 forbids. What the curve supports is narrower and
/// firmer: evidence buys quality up to ~16 passages and nothing measurable
/// after.
///
/// 16x4 is therefore the knee on every axis at once — best measured score,
/// second-lowest wall-clock, second-smallest prompts. Prompt size matters for
/// more than cost: the writer's output buffer is
/// `n_vocab * prompt_tokens * 4` bytes and it strands unreclaimable host
/// memory past ~3,650 tokens on this device (see
/// `research/deep-research/arms/mem-forensics/PREREG-buffer-threshold.md`),
/// so the wider arms buy nothing and pay in GiB.
///
/// n=1 ACROSS TASKS. This is one bed, one question. The curve's SHAPE is what
/// is being relied on, not its third digit.
const SECTION_PASSAGES: usize = 16;
/// At most this many passages from any ONE source per section, so a
/// single long page cannot crowd out the rest of the window.
///
/// Moves with `SECTION_PASSAGES` (4 at 16). Widening `want` while leaving the
/// cap narrow fills the new room from new SOURCES only — the opposite of what
/// a section needing depth on one subject requires — which is why the curve
/// swept them together as `8:3, 16:4, 28:5, 44:6, 60:8` rather than
/// independently.
const PER_SOURCE_CAP: usize = 4;

/// drb1-r9: what one section's writer may see when
/// `SOVEREIGN_DR_REPORT_SECTION_EVIDENCE` is on.
///
/// MEASURED, on the task-69 control flight `dr-1787742429`. Its evidence
/// window held 46 chunks and **1,060,308 characters** — the material for a
/// proper MCP section is in 40 of those 46 chunks, and the primitives
/// (Tools/Resources/Prompts/Roots/Sampling) appear in 41. Acquisition is not
/// the constraint. But at the THEN-shipped `SECTION_PASSAGES = 8` × `PASSAGE_CHARS = 1400`, one
/// section's writer sees **11,200 chars — 1.06% of the window** — and eight
/// sections see 8.5% between them. The 11,345-word deliverable that came out
/// cites 22 distinct sources of the 46 available, and has no section
/// describing MCP at all.
///
/// So the writer is not failing to use the evidence; it is not being shown
/// it. This is a strictly larger lever than the deliverable's architecture
/// (drb1-r8) and is flagged separately so the two can be told apart.
///
/// It is also a REGRESSION rather than a limit anyone chose — see the port
/// provenance on the constants below.
///
/// THE VALUES ARE NOT A GUESS — THEY RESTORE THE CONFIGURATION THE COMPOSED
/// REPORT'S OWN QUALITY NUMBER WAS MEASURED AT. `compose_report` was ported to
/// Rust in `a50d2fdf3` (2026-08-23) from the Python prototype
/// `research/deep-research/arms/lab/compose2.py`, whose own commit message
/// records that "the 44.40 composite that stood in for its quality was measured
/// by `arms/lab/compose2.py`". That prototype chunks passages IDENTICALLY —
/// `passages(chunks, size=1400, overlap=200)` vs our `PASSAGE_CHARS`/
/// `PASSAGE_OVERLAP` — ranks by the same cosine, and applies the same
/// per-source cap. Its budget: `k=28, repeat_cap=5`, and it recorded the
/// consequence in its own manifest as `evidence_chars_per_section: k * 1400`
/// = 39,200.
///
/// The port shipped 8 and 3 — **11,200 chars, a 3.5× cut** — and no commit,
/// note or ledger row records that as a decision. `14ddccf49` later OBSERVED
/// the consequence ("ours showed each section eight passages by cosine, so on
/// the logged task-69 flight a 38-chunk window reached the writer eight chunks
/// at a time") and responded with the research-notes flag rather than by
/// restoring the number. So the shipped Rust path had never run at the
/// configuration whose measured quality justified building it.
///
/// SUPERSEDED IN PART, 2026-08-27 — THE SWEEP HAPPENED AND 28/5 LOST. The
/// argument above is that 28/5 should be restored because it is the only
/// configuration whose quality was ever measured. That premise expired the
/// moment the curve was flown: 28/5 scores 50.9864 and 16/4 scores 51.3347,
/// and 16/4 gets there in 11.2 min against 12.3 with smaller prompts. So the
/// default moved to 16/4 (see `SECTION_PASSAGES`), NOT to this flag's 28/5.
///
/// The flag is kept, unchanged, because it is still a real widening and the
/// curve is n=1 across tasks — but it is now a DOMINATED point, not a target
/// to restore. Anyone reaching for it should re-read the curve first.
const SECTION_PASSAGES_WIDE: usize = 28;
const PER_SOURCE_CAP_WIDE: usize = 5;

/// ONE decider for the section evidence budget, so the ranker that PICKS
/// passages and the per-source cap that shapes the pick can never be set from
/// different rules (§10.6).
pub(crate) fn section_evidence_budget() -> (usize, usize) {
    // NUMERIC OVERRIDES — the curve, not another decider. 28/5 is ONE POINT:
    // 28 × 1400 is still only ~2.8% of a ~995-passage window, so "wide" is a
    // restored measurement, not an argued optimum. Finding the optimum needs
    // the knobs swept, and a boolean flag cannot sweep. Both still resolve
    // HERE and nowhere else (§10.6) — the ranker that picks passages and the
    // cap that shapes the pick can still never be set from different rules.
    //
    // Precedence: explicit number > the wide flag > the shipped default. A
    // value that does not parse, or is zero, is IGNORED rather than treated
    // as a budget of nothing — a silent zero would collapse every section to
    // no evidence and still produce a plausible report, which is the failure
    // this file exists to prevent (§18.3). `cap` may be set alone, but
    // widening `want` while leaving the cap narrow fills the new room from
    // new SOURCES only, which is the opposite of what a section needing depth
    // on one subject requires — so raising `want` alone is a real
    // configuration and the caller owns that choice knowingly.
    let (mut want, mut cap) = if super::report_section_evidence_enabled() {
        (SECTION_PASSAGES_WIDE, PER_SOURCE_CAP_WIDE)
    } else {
        (SECTION_PASSAGES, PER_SOURCE_CAP)
    };
    if let Some(n) = positive_env("SOVEREIGN_DR_SECTION_PASSAGES") {
        want = n;
    }
    if let Some(n) = positive_env("SOVEREIGN_DR_SECTION_SOURCE_CAP") {
        cap = n;
    }
    (want, cap)
}

/// A positive integer from the environment, or `None`. Unset, empty,
/// unparseable and zero all read as "not set" — never as a budget of zero.
fn positive_env(key: &str) -> Option<usize> {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
}

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

/// The v2 obligations, appended to [`WRITER_CONTRACT`] when
/// `SOVEREIGN_DR_WRITER_CONTRACT_V2` is on. These are the items AIQ's writer
/// prompt (`deep_researcher/prompts/writer.j2`) carries that v1 does not —
/// checked line by line against that file, not recalled (§11.1). v1 already
/// holds cross-synthesis, evaluate-don't-report, conflict surfacing, developed
/// paragraphs, detail retention and err-toward-more; those are NOT repeated
/// here, because saying an obligation twice in one prompt is how a contract
/// starts contradicting itself.
///
/// Four additions, each aimed at a dimension we measurably lack (insight
/// -15.20, readability -11.93; note bdf94683):
///
/// 1. HOW TO USE THE GRADE. AIQ: "high-score/high-confidence notes are
///    synthesis anchors, medium notes are support or nuance, and low-score or
///    low-confidence notes are mainly for gaps, caveats, conflicts, or clearly
///    labeled weak evidence." Ours had no analog because the writer never saw
///    a grade.
/// 2. CONSENSUS AND COMPLEMENTARITY. AIQ asks the writer to find "repeated
///    findings that show consensus across notes or sources" and "complementary
///    findings that only become useful when combined". v1 asks for
///    cross-synthesis but never names these two shapes.
/// 3. LICENSED, MARKED INFERENCE. AIQ: "Distinguish cited facts from your
///    synthesis or inference. Inferences are allowed, but they must be
///    grounded in cited evidence and phrased with the right level of
///    confidence." v1 says only "Assert ONLY what the evidence supports",
///    which is stricter and may suppress the analytical move the Insight
///    dimension actually rewards. The honesty floor is unchanged: an inference
///    must still rest on cited evidence and must be visibly an inference.
/// 4. WHEN A TABLE EARNS ITS PLACE. AIQ names the trigger (comparable
///    entities, metrics, timelines, choices, categories), demands units and
///    dates, and rejects "shallow tables that merely restate prose". v1 says
///    only that a table is "welcome where the content is genuinely tabular",
///    which tells the writer nothing about when.
const WRITER_CONTRACT_V2_EXTRA: &str = "\
- The evidence above is GRADED. [ANCHOR] passages carry this section; build \
the argument on them. [SUPPORT] passages add nuance, qualification and \
detail. [WEAK] passages are for caveats, conflicts and naming what is thin — \
never build a load-bearing claim on one, and never silently drop one that \
contradicts an anchor.\n\
- Name CONSENSUS explicitly where two or more independent sources agree, and \
say so. Combine COMPLEMENTARY evidence: where two sources are each partial \
and only together answer the question, make that combination the point.\n\
- Inference is allowed and wanted. Mark it: an inference must rest on cited \
evidence, read visibly as your reasoning rather than as a sourced fact, and \
carry the confidence the evidence actually supports.\n\
- Use a markdown table when the evidence holds comparable entities, metrics, \
timelines, choices or categories, and carry units, dates and ranges into it. \
Do not build a table that merely restates the prose beside it.";

/// The section writer's obligations for this run — v1, or v1 plus the ported
/// AIQ additions. ONE decider, so a section cannot be written under one
/// contract while a test asserts the other (§10.6).
fn writer_contract() -> String {
    if super::writer_contract_v2_enabled() {
        format!("{WRITER_CONTRACT}\n{WRITER_CONTRACT_V2_EXTRA}")
    } else {
        WRITER_CONTRACT.to_string()
    }
}

/// Usefulness at or above this is an ANCHOR; below [`GRADE_WEAK_BELOW`] is
/// WEAK; between them is SUPPORT. The band straddles `DEFAULT_USEFULNESS`
/// (50) deliberately: a finding whose worker declined to score it lands in
/// SUPPORT, never in ANCHOR and never in WEAK — an absent score must not be
/// read as a judgement in either direction (§18.3).
const GRADE_ANCHOR_AT: u8 = 70;
const GRADE_WEAK_BELOW: u8 = 40;

/// Grade for a 0-100 usefulness score.
pub(crate) fn grade_for_usefulness(u: u8) -> &'static str {
    if u >= GRADE_ANCHOR_AT {
        "ANCHOR"
    } else if u < GRADE_WEAK_BELOW {
        "WEAK"
    } else {
        "SUPPORT"
    }
}

/// Grade for a passage at rank `i` of `n`, best-first.
///
/// `rank_passages` returns descending cosine, so position IS the grade and it
/// costs nothing to read. Top third anchors, bottom third weak, middle
/// supports. With fewer than 3 passages every one is an ANCHOR: a floor of
/// one-third would otherwise mark the only evidence a section has as WEAK and
/// instruct the writer not to build on it.
fn grade_for_rank(i: usize, n: usize) -> &'static str {
    if n < 3 {
        return "ANCHOR";
    }
    let third = n / 3;
    if i < third.max(1) {
        "ANCHOR"
    } else if i >= n - third.max(1) {
        "WEAK"
    } else {
        "SUPPORT"
    }
}

/// One retrieval passage: a span of a window chunk, tagged with the
/// chunk it came from so the citation maps to a real fetched source.
#[derive(Clone)]
pub(crate) struct Passage {
    pub chunk_id: String,
    pub url: String,
    pub text: String,
}

/// Split the window into overlapping passages. Retrieval granularity:
/// a whole chunk is too coarse to rank against one sub-question.
pub(crate) fn window_passages(window: &EvidenceWindow) -> Vec<Passage> {
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

/// **The ONE bound on how much evidence enters a single prompt** (§10.6),
/// and it drops passages by RELEVANCE or by fair rotation — never by
/// position in a document.
///
/// Watched red in the field, not theorised: the 2026-08-24 web arm on DRB-I
/// task 69 pulled 50 chunks / 1,360,782 chars from Tavily in under two
/// minutes and died on the round draft with
///
/// ```text
/// Prompt too long: 302,153 tokens meets or exceeds the context window of 65,532
/// ```
///
/// No cap was missing in the sense anyone had checked. `evidence_window_
/// max_chunks` is 100 and the run held 50 — but that cap counts CHUNKS, and a
/// web chunk is fifty times fatter than an estate one (measured on that run:
/// median 26,766 chars against 521, with `fetch::CHUNK_CONTENT_CAP` allowing
/// 50,000). Acquisition breadth and prompt size were coupled with nothing in
/// between, so the first genuinely broad acquisition took the loop down.
///
/// **Why this works on passages instead of truncating chunks.** The obvious
/// bound — give each chunk a share of the budget and cut it there — loses the
/// TAIL of every long page, silently, and a page's most specific material is
/// as likely to sit at the bottom as the top. That is the artificial-cutoff
/// failure this codebase has been bitten by before: information disappears and
/// nothing downstream can tell you it ever existed. Passages avoid it: the
/// whole page is available as overlapping spans, and what loses is the span
/// that ranks worst, not the span that happens to be late.
///
/// Two fill orders, and the caller says which it got:
///
/// - **Ranked** (an embedder was available): best passage first, capped per
///   source so one large site cannot buy the whole budget.
/// - **Rotation** (no embedder): round-robin across sources, so every source
///   contributes its first passage before any source contributes its second.
///   Document order is never the selector.
///
/// What did not fit is COUNTED and returned. Evidence a run paid to fetch and
/// then never showed a model is reported, never silently absent (§18.3).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BoundedEvidence {
    pub text: String,
    /// The bodies actually admitted, `(chunk id, text)`, in block order.
    ///
    /// Anything ELSE derived from the evidence must be derived from THESE,
    /// never from the window. `figure_inventory` walks every chunk in full,
    /// so a bounded prompt would tell the model "every evidence-supported
    /// figure must appear in the answer" while listing figures from text the
    /// bound had removed — an instruction to cite what it cannot see, which
    /// is a request to invent. The bound did not create that; it exposed it.
    pub admitted: Vec<(String, String)>,
    pub passages_used: usize,
    pub passages_dropped: usize,
    pub sources_used: usize,
    pub chars_used: usize,
}

/// Passages from one source in a single prompt. A cap, not a budget: it
/// stops one large site from crowding out the rest even when its passages
/// legitimately rank best.
pub(crate) const PER_SOURCE_PROMPT_CAP: usize = 6;

pub(crate) fn bounded_evidence(
    passages: &[Passage],
    ranked: bool,
    budget_chars: usize,
) -> BoundedEvidence {
    let mut out = BoundedEvidence {
        text: String::new(),
        admitted: Vec::new(),
        passages_used: 0,
        passages_dropped: 0,
        sources_used: 0,
        chars_used: 0,
    };
    if passages.is_empty() || budget_chars == 0 {
        out.passages_dropped = passages.len();
        return out;
    }

    // The fill ORDER. `ranked` means the caller already sorted best-first;
    // otherwise rotate across sources so breadth, not position, decides.
    let order: Vec<usize> = if ranked {
        (0..passages.len()).collect()
    } else {
        let mut by_source: std::collections::BTreeMap<&str, Vec<usize>> = Default::default();
        for (i, p) in passages.iter().enumerate() {
            by_source.entry(p.url.as_str()).or_default().push(i);
        }
        let mut lanes: Vec<Vec<usize>> = by_source.into_values().collect();
        let deepest = lanes.iter().map(|l| l.len()).max().unwrap_or(0);
        let mut order = Vec::with_capacity(passages.len());
        for depth in 0..deepest {
            for lane in lanes.iter_mut() {
                if let Some(i) = lane.get(depth) {
                    order.push(*i);
                }
            }
        }
        order
    };

    let mut per_source: std::collections::HashMap<&str, usize> = Default::default();
    let mut taken = vec![false; passages.len()];
    for i in order {
        let p = &passages[i];
        let n = per_source.entry(p.url.as_str()).or_insert(0);
        if *n >= PER_SOURCE_PROMPT_CAP {
            continue;
        }
        let cost = p.text.chars().count();
        if out.chars_used + cost > budget_chars {
            continue;
        }
        *n += 1;
        taken[i] = true;
        out.chars_used += cost;
        out.passages_used += 1;
        out.text
            .push_str(&format!("[{}] {}\n\n", p.chunk_id, p.text));
        out.admitted.push((p.chunk_id.clone(), p.text.clone()));
    }
    out.passages_dropped = taken.iter().filter(|t| !**t).count();
    out.sources_used = out
        .admitted
        .iter()
        .map(|(id, _)| id.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    out
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
    let (want, per_source_cap) = section_evidence_budget();
    let mut picked = Vec::new();
    let mut per_source: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (_, i) in scored {
        let p = &passages[i];
        let n = per_source.entry(p.url.as_str()).or_insert(0);
        if *n >= per_source_cap {
            continue;
        }
        *n += 1;
        picked.push(p.clone());
        if picked.len() >= want {
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

/// **The report's outline is not the search frontier** (drb1-r5).
///
/// `compose_report` wrote one section per PLANNED SUB-QUESTION, and those
/// sub-questions come from the acquisition frontier — a list the planner
/// prompt deliberately tunes for retrieval, asking for "the specific measure
/// or statistic it implies — an index, a ratio, a share, a rate, a count".
/// Those make good search queries and bad section headings. The task-69 web
/// arm's actual section list included "Count of distinct error handling
/// states defined in the A2A message schema" and "Number of documented
/// failure modes unique to asynchronous communication channels".
///
/// The judge said so directly. Three of that arm's four largest weighted
/// losses were structural, not evidential — with 98 sources and 2.18M chars
/// in hand:
///
/// - *"Article 2 dedicates Section III to MCP, detailing its definition,
///   origins, core architecture, key primitives… Article 1 lacks a
///   comprehensive standalone explanation."* (ours 5.0 / ref 9.0)
/// - *"Article 2 has a dedicated Section VI ('Interplay and Relationship')."*
///   (ours 6.0 / ref 9.5)
/// - *"Article 2 explicitly maps problems to solutions in Section IX."*
///   (ours 6.5 / ref 9.5)
///
/// And a fourth says the fragmentation costs INSIGHT, our worst dimension:
/// *"Article 1 offers deep dives into technical metrics (latency bytes,
/// token counts)… risks being overly granular or speculative."*
///
/// It also explains a result we could not otherwise account for: widening
/// the frontier 8 → 20 made the deliverable MORE fragmented, and the
/// frontier-20 arms did not beat the frontier-8 ones.
///
/// So the frontier keeps its job — finding things — and the outline gets its
/// own: deciding what the report must establish. The prompt describes a
/// SHAPE and never the answer (no criterion vocabulary, no worked example
/// carrying content): give each distinct subject its own standing where it
/// needs explaining on its own terms, then relate them, then say what
/// follows. It is planned over the evidence actually gathered, so it cannot
/// promise sections nothing can support.
pub async fn plan_outline(
    port: &dyn ResearchPort,
    question: &str,
    window: &EvidenceWindow,
) -> Result<Vec<String>, String> {
    // A small slice: the outline needs to know what the evidence COVERS, not
    // to read it. The writer reads it properly, section by section.
    let bounded = bounded_evidence(
        &window_passages(window),
        false,
        OUTLINE_EVIDENCE_TOKENS * CHARS_PER_TOKEN,
    );
    let max = outline_max();
    let prompt = format!(
        "Plan the sections of a report that answers this question, using the evidence below.\n\n         Give each distinct subject the question names its own section wherever it needs \
         explaining on its own terms before it can be compared. Then the sections that relate \
         those subjects to each other. Then what follows from that for someone acting on it. \
         Plan only sections the evidence can support.\n\n         One section per line: a short noun-phrase title, then ' — ', then one sentence naming \
         what that section must establish. Between {OUTLINE_MIN} and {max} sections. \
         No numbering, no commentary.\n\n         Question: {question}\n\nEvidence:\n{}",
        bounded.text
    );
    let raw = port
        .draft(DraftLeg::Outline, &prompt, None, &[])
        .await
        .map_err(|e| format!("outline draft: {e}"))?;
    parse_outline(&raw, max)
}

/// The report's own title. The default deliverable's H1 is the user's raw
/// prompt sentence — which reads as a machine artifact, not a report, and is
/// scored as one under `Formatting, Layout, and Typographical Consistency`.
/// A refused or unusable title falls back to the question and SAYS so; it is
/// never silently substituted (§18.3).
pub async fn plan_title(
    port: &dyn ResearchPort,
    question: &str,
    sections: &[String],
) -> Result<String, String> {
    let plan = sections
        .iter()
        .map(|s| s.chars().take(160).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    let prompt = format!(
        "Give this report its title.\n\nQuestion it answers: {question}\n\n\
         Sections:\n{plan}\n\n\
         One line. A specific noun phrase naming the subjects and what the report \
         establishes about them — the title a published report would carry, not the \
         question restated and not a generic label. No quotes, no markdown, no commentary."
    );
    let raw = port
        .draft(DraftLeg::Outline, &prompt, None, &[])
        .await
        .map_err(|e| format!("title draft: {e}"))?;
    parse_title(&raw)
}

/// PURE, for the same reason `parse_outline` is: every rule that admits or
/// refuses a title is decidable with a failing input you can name (§18.1).
pub(crate) fn parse_title(raw: &str) -> Result<String, String> {
    let line = raw
        .lines()
        .map(|l| {
            l.trim()
                .trim_start_matches(['#', '-', '*', '•', ' '])
                .trim()
                .trim_matches(['"', '\'', '“', '”'])
                .trim()
        })
        .find(|l| !l.is_empty() && !l.starts_with('<'))
        .unwrap_or("");
    // A title that runs to a paragraph is the model answering instead of
    // naming; a two-word one names nothing. Both fall back, loudly.
    let n = line.chars().count();
    if !(TITLE_MIN_CHARS..=TITLE_MAX_CHARS).contains(&n) {
        return Err(format!(
            "title unusable — {n} chars, outside {TITLE_MIN_CHARS}-{TITLE_MAX_CHARS}"
        ));
    }
    Ok(line.to_string())
}

/// The outline parser — PURE, so every admission rule is decidable without a
/// model in the loop (§18.1: a check with a failing input you can name).
pub(crate) fn parse_outline(raw: &str, max: usize) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for line in raw.lines() {
        let line = line
            .trim()
            .trim_start_matches(['-', '*', '•', '#', ' '])
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .trim_start_matches(['.', ')', ' '])
            .trim();
        // A bare heading is a title, not a section plan — the brief after the
        // separator is what tells the writer what to establish, and a section
        // with no brief is exactly the frontier-shaped heading this replaces.
        if line.chars().count() < OUTLINE_MIN_CHARS || (!line.contains('—') && !line.contains(':'))
        {
            continue;
        }
        if out.iter().any(|e| e == line) {
            continue;
        }
        out.push(line.to_string());
        if out.len() >= max {
            break;
        }
    }
    if out.len() < 2 {
        // Refuse rather than compose from a one-line outline: the caller
        // falls back to the frontier and NAMES the fallback (§18.3).
        return Err(format!(
            "outline unusable — {} section(s) parsed from {} chars of draft",
            out.len(),
            raw.chars().count()
        ));
    }
    Ok(out)
}

/// The composed deliverable: one section per sub-question plus a closing
/// synthesis, with a `## Sources` list whose numbers the section text
/// cites. Returns the markdown and the ordered source list.
pub async fn compose_report(
    port: &dyn ResearchPort,
    question: &str,
    window: &EvidenceWindow,
    subquestions: &[String],
    notes: &[ResearchNote],
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
    // Glassbox (§9.1): the section evidence budget is a DECISION and an arm
    // that cannot prove its own lever fired is not a measurement (§18.1).
    // Emitted at info so a flight log carries it without RUST_LOG surgery.
    {
        let (want, cap) = section_evidence_budget();
        tracing::info!(
            target: "deep_research",
            passages_per_section = want,
            per_source_cap = cap,
            window_chunks = window.chunks.len(),
            wide = super::report_section_evidence_enabled(),
            "section evidence budget decided — this is how much of the window \
             one section's writer will see"
        );
    }
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

    tracing::info!(
        target: "deep_research",
        target_words = target_report_words(),
        default_words = TARGET_REPORT_WORDS,
        sections = kept.len(),
        section_words = section_word_budget(kept.len()),
        "compose_report: deliverable length is a decision — target total and \
         the per-section budget derived from it"
    );
    // Read the flag ONCE for the whole compose, not per section: a run must
    // not write section 1 under v1 and section 2 under v2 if the environment
    // changes mid-flight.
    let graded = super::writer_contract_v2_enabled();
    let contract = writer_contract();
    tracing::debug!(
        target: "deep_research",
        graded_evidence = graded,
        contract_chars = contract.len(),
        "compose_report: writer contract selected"
    );
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
            let (want, _) = section_evidence_budget();
            let start = (si * want) % passages.len().max(1);
            passages
                .iter()
                .cycle()
                .skip(start)
                .take(want.min(passages.len()))
                .cloned()
                .collect()
        };
        // drb1-r4: the writer reads FINDINGS when a researcher worker
        // distilled this sub-question, and passages when it did not. One
        // section, one input — never both, because a section handed both
        // would double-count its evidence and an ablation would measure
        // nothing. The choice is per sub-question and NAMED in the trace:
        // a note whose findings were ALL refused falls back to passages
        // rather than writing a section from nothing (§18.3 — the
        // substitution is reported, never silent).
        let note = notes.iter().find(|n| n.sub_question == *sub);
        let ev = match note.filter(|n| !n.findings.is_empty()) {
            Some(n) => {
                tracing::debug!(
                    target: "deep_research",
                    sub_question = %sub,
                    findings = n.findings.len(),
                    refused = n.refused.len(),
                    passages_seen = n.passages_seen,
                    "compose_report: section written from distilled findings"
                );
                if graded {
                    super::notes::findings_block_graded(n)
                } else {
                    super::notes::findings_block(n)
                }
            }
            None => {
                if picked.is_empty() {
                    continue;
                }
                if note.is_some() {
                    tracing::info!(
                        target: "deep_research",
                        sub_question = %sub,
                        "compose_report: worker admitted no finding — \
                         section falls back to passages"
                    );
                }
                let mut ev = String::new();
                let n = picked.len();
                for (i, p) in picked.iter().enumerate() {
                    // drb1-r7: rank_passages already returned these in
                    // descending cosine order and the block then flattened
                    // that away. Surface it — the grade is free and the
                    // writer was being asked to weigh an ordering nobody
                    // told it about.
                    if graded {
                        ev.push_str(&format!(
                            "[{}] ({}) [{}]\n{}\n\n",
                            p.chunk_id,
                            p.url,
                            grade_for_rank(i, n),
                            p.text
                        ));
                    } else {
                        ev.push_str(&format!("[{}] ({})\n{}\n\n", p.chunk_id, p.url, p.text));
                    }
                }
                ev
            }
        };
        if ev.trim().is_empty() {
            continue;
        }
        // Derived from the plan's own size so length does not ride on structure.
        let words = section_word_budget(kept.len());
        let (lo, hi) = (words * 9 / 10, words * 11 / 10);
        let prompt = format!(
            "You are writing ONE section of an analytical research report that answers:\n{question}\n\n\
             THIS SECTION: {sub}\n\nEVIDENCE:\n{ev}\n{contract}\n\n\
             Write {lo}-{hi} words. Start with a '## ' heading that is a short noun phrase, \
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
    let architected = super::report_architecture_enabled();
    // A report closes on a Conclusion. "Synthesis and Assessment" names the
    // pipeline's own step, and a deliverable that ends on its producer's
    // vocabulary reads as an internal artifact.
    let closing_heading = if architected {
        "## Conclusion"
    } else {
        "## Synthesis and Assessment"
    };
    let synth_prompt = format!(
        "You are writing the closing synthesis of a research report answering:\n{question}\n\n\
         THE REPORT SO FAR:\n{digest}\n\n\
         Write a '{closing_heading}' section of 280-340 words that draws the \
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
    if !architected {
        return Ok(format!("# {question}\n\n{}", sections.join("\n\n")));
    }

    // The report's own title. A refusal is NAMED and falls back to the
    // question — the deliverable still lands (§18.3).
    let title = match plan_title(port, question, &subs).await {
        Ok(t) => {
            tracing::info!(
                target: "deep_research", title = %t,
                "report title planned — the H1 is the report's, not the prompt's"
            );
            t
        }
        Err(e) => {
            tracing::warn!(
                target: "deep_research", error = %e,
                "title unavailable — the H1 falls back to the question (named, never silent)"
            );
            question.to_string()
        }
    };

    // The executive summary is written LAST and read FIRST: it can only
    // summarise a report that already exists, and a reader who stops after it
    // must still have the answer.
    let body = sections.join("\n\n");
    let digest_for_summary: String = sections
        .iter()
        .map(|s| s.chars().take(1800).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n\n");
    let summary_prompt = format!(
        "You are writing the executive summary of a research report answering:\n{question}\n\n\
         THE REPORT:\n{digest_for_summary}\n\n\
         Write a '## Executive Summary' section of 200-260 words that ANSWERS the question \
         directly in its first two sentences, then gives the findings that answer carries and \
         the one or two caveats a reader must hold. Reuse the [Source: ev-N] handles already \
         used below where a claim needs one. Developed paragraphs, no checklist, and nothing \
         the report does not already establish."
    );
    let head = match port
        .draft(DraftLeg::Synthesis, &summary_prompt, Some(system), &allowed)
        .await
    {
        Ok(t) => format!("{t}\n\n"),
        Err(e) => {
            tracing::warn!(
                target: "deep_research", error = %e,
                "executive summary failed — the report lands without it, named"
            );
            String::new()
        }
    };
    Ok(format!("# {title}\n\n{head}{body}"))
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
    // SCAN EVERY BRACKET, NOT JUST `[Source:`. The writer emits its handles in
    // more than one form and this only ever recognised one of them, so the
    // other two reached the READER. Measured 2026-08-27 on the shipped t69
    // flight reports — raw handles that survived render, per report:
    //
    //   t69-pinfix   45 bare [ev-N] + 31 [estate-N] + 1 [Source: ev-N]  (37 numbered)
    //   t69-trim     53 + 12 + 13                                       (33 numbered)
    //   t69-web      55 +  0 + 18                                       (143 numbered)
    //
    // pinfix and trim shipped MORE raw handles than numbered citations. This
    // is the deliverable a reader receives, and it is the Formatting criterion
    // the RACE judge marks us down on ("the density of citations
    // [Source: ev-xx] can be visually cluttering").
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after = &rest[open..];
        let Some(close) = after.find(']') else {
            out.push_str(after);
            return (out, numbering);
        };
        let raw = &after[1..close];
        let explicit = raw.trim_start().starts_with("Source:");
        let inner = raw
            .trim_start()
            .strip_prefix("Source:")
            .unwrap_or(raw)
            .trim();
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
            None if explicit => {}
            // NOT A HANDLE AT ALL — a markdown link's text, a citation we
            // already numbered, ordinary bracketed prose. Emitted VERBATIM.
            // Dropping these the way an unresolvable `[Source: x]` is dropped
            // would silently eat every `[text](url)` in the report, which is
            // why the bare form only ever REWRITES and never deletes.
            None => out.push_str(&after[..=close]),
        }
        rest = &after[close + 1..];
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
    // The evidence is BOUNDED before it becomes a prompt, and what loses is
    // the worst-ranked passage rather than the tail of a long page (see
    // `bounded_evidence`). No embedder on this leg, so the fill rotates
    // across sources: every source contributes before any source repeats.
    let bounded = bounded_evidence(
        &window_passages(evidence),
        false,
        ROUND_EVIDENCE_TOKENS * CHARS_PER_TOKEN,
    );
    if bounded.passages_dropped > 0 {
        tracing::info!(
            target: "deep_research",
            run_id, round,
            window_chunks = evidence.chunks.len(),
            passages_used = bounded.passages_used,
            passages_dropped = bounded.passages_dropped,
            sources_used = bounded.sources_used,
            chars_used = bounded.chars_used,
            budget_chars = ROUND_EVIDENCE_TOKENS * CHARS_PER_TOKEN,
            "round draft: evidence bounded — passages dropped are NAMED, never silently absent"
        );
    }
    let mut prompt = String::new();
    if round == 1 {
        prompt.push_str(&format!("Estate evidence:\n{}", bounded.text));
    } else {
        prompt.push_str(&format!(
            "Evidence gathered so far:\n{}\n\nQuestion: {question}",
            bounded.text
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
        figure_inventory_of(&bounded.admitted)
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

    #[test]
    fn number_citations_renders_the_bare_handle_form_too() {
        // The writer emits `[ev-N]` as well as `[Source: ev-N]`, and only the
        // second was ever recognised — so the first reached the READER. The
        // shipped t69 flight reports carried 37-55 bare handles each; pinfix
        // and trim shipped MORE raw handles than numbered citations.
        //
        // Watch-it-fail: restore the `rest.find("[Source:")` scan and the bare
        // handle survives into `out` verbatim.
        let md = "Bare [ev-1] and explicit [Source: ev-1] name the SAME source.";
        let (out, srcs) = number_citations(md, &two_source_window());
        assert!(
            !out.contains("ev-1"),
            "no raw handle reaches the reader: {out}"
        );
        assert_eq!(
            out.matches("[1]").count(),
            2,
            "both forms resolve to the same numbered source: {out}"
        );
        assert_eq!(srcs.len(), 1, "and they share one source row");
    }

    #[test]
    fn number_citations_leaves_brackets_that_are_not_handles_alone() {
        // The bare form must only ever REWRITE, never delete. An unresolvable
        // `[Source: x]` is dropped on purpose, but applying that rule to every
        // bracket would eat markdown links, already-numbered citations, and
        // ordinary bracketed prose — turning a citation fix into silent
        // corruption of the deliverable.
        let md = "See [the spec](https://example.com/a2a), footnote [1], and [TODO] later.";
        let (out, _) = number_citations(md, &two_source_window());
        assert!(
            out.contains("[the spec](https://example.com/a2a)"),
            "the markdown link survives whole: {out}"
        );
        assert!(
            out.contains("[1]"),
            "an existing numbered citation survives: {out}"
        );
        assert!(
            out.contains("[TODO]"),
            "ordinary bracketed prose survives: {out}"
        );
    }

    /// Retrieval granularity: a whole chunk is too coarse to rank
    /// against one sub-question, so the window is split with overlap.
    #[test]
    fn total_length_does_not_ride_on_section_count() {
        // The defect this replaces, in one assertion. With a FIXED per-section
        // budget the 20-section control wrote 9,084-9,354 words and the
        // 7-section outline arm wrote 3,702-4,053 against references of
        // 6,898-13,348 — so the outline A/B varied structure and length at
        // once and could not answer the question it was run to answer.
        //
        // Watch-it-fail: return a constant from `section_word_budget` and the
        // 7-vs-20 totals diverge by more than half.
        let seven = 7 * section_word_budget(7);
        let twenty = 20 * section_word_budget(20);
        let ratio = seven as f32 / twenty as f32;
        assert!(
            (0.75..=1.33).contains(&ratio),
            "a 7-section and a 20-section plan must target comparable totals: \
             {seven} vs {twenty} words (ratio {ratio:.2})"
        );
    }

    #[test]
    fn a_bad_target_override_is_never_a_silent_zero() {
        // Watch-it-fail: drop the `.filter(|&n| n > 0)` and "0" returns 0,
        // so section_word_budget clamps EVERY section to SECTION_WORDS_MIN
        // and the run ships a stub that still looks like a deliverable.
        assert_eq!(target_words_policy(None), TARGET_REPORT_WORDS, "unset");
        assert_eq!(target_words_policy(Some("")), TARGET_REPORT_WORDS, "empty");
        assert_eq!(
            target_words_policy(Some("   ")),
            TARGET_REPORT_WORDS,
            "blank"
        );
        assert_eq!(
            target_words_policy(Some("banana")),
            TARGET_REPORT_WORDS,
            "unparseable"
        );
        assert_eq!(target_words_policy(Some("0")), TARGET_REPORT_WORDS, "zero");
        assert_eq!(
            target_words_policy(Some("-1")),
            TARGET_REPORT_WORDS,
            "negative"
        );
        // An explicit positive value is honoured, whitespace and all.
        assert_eq!(target_words_policy(Some("7000")), 7_000);
        assert_eq!(target_words_policy(Some(" 7000 ")), 7_000);
    }

    #[test]
    fn the_section_budget_stays_inside_its_band() {
        assert_eq!(
            section_word_budget(1),
            SECTION_WORDS_MAX,
            "one section is capped"
        );
        assert_eq!(
            section_word_budget(100),
            SECTION_WORDS_MIN,
            "many sections keep a floor"
        );
        assert_eq!(
            section_word_budget(0),
            SECTION_WORDS_MAX,
            "zero must not divide by zero"
        );
    }

    #[test]
    fn the_outline_refuses_a_frontier_shaped_list() {
        // THE point of drb1-r5. The acquisition frontier is a list of search
        // queries — bare noun phrases with no brief — and feeding it to the
        // writer as a section plan is what produced sections titled "Count of
        // distinct error handling states defined in the A2A message schema".
        // A line with no brief after the separator is not a planned section.
        //
        // Watch-it-fail: drop the separator requirement and these parse as a
        // five-section outline.
        let frontier = "Number of major AI agent protocols released by Google in 2024\n\
             Count of distinct error handling states defined in the A2A message schema\n\
             Name and release date of the A2A protocol announced by Google\n\
             Number of documented failure modes unique to asynchronous channels\n\
             Percentage increase in developer adoption metrics for MCP\n";
        let got = parse_outline(frontier, OUTLINE_MAX);
        assert!(
            got.is_err(),
            "a frontier-shaped list must not pass as an outline: {got:?}"
        );
        assert!(got.unwrap_err().contains("unusable"));
    }

    #[test]
    fn a_real_outline_parses_and_keeps_its_briefs() {
        let raw = "Sections:\n\n\
            - The MCP Protocol — establish its architecture, primitives and transport.\n\
            - The A2A Protocol — establish its task lifecycle and agent discovery model.\n\
            3. Interplay and Overlap — relate the two and name where they compete.\n\
            * What Follows for Adopters — say what a team should do with the distinction.\n";
        let out = parse_outline(raw, OUTLINE_MAX).expect("a briefed outline parses");
        assert_eq!(out.len(), 4, "got {out:?}");
        assert!(out[0].starts_with("The MCP Protocol"), "{:?}", out[0]);
        assert!(
            out[0].contains("architecture"),
            "the brief survives — it is what tells the writer what to establish: {:?}",
            out[0]
        );
        assert!(
            !out.iter().any(|s| s.starts_with('-') || s.starts_with('3')),
            "list markers are stripped: {out:?}"
        );
    }

    #[test]
    fn the_outline_is_capped_and_deduped() {
        let mut raw = String::new();
        for _ in 0..3 {
            for i in 0..5 {
                raw.push_str(&format!(
                    "Section {i} — establish the thing numbered {i}.\n"
                ));
            }
        }
        let out = parse_outline(&raw, OUTLINE_MAX).expect("parses");
        assert!(
            out.len() <= OUTLINE_MAX,
            "capped at {OUTLINE_MAX}: {}",
            out.len()
        );
        let mut uniq = out.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), out.len(), "no repeated section: {out:?}");
    }

    /// Serialises the tests that move `SOVEREIGN_DR_REPORT_ARCHITECTURE`.
    /// Same reason as `audit.rs::budget_guard`: different tests set this var
    /// to DIFFERENT values, so under a threaded runner one test's `on` is
    /// another's `off` and the pair passes only under `--test-threads=1`.
    fn architecture_guard(value: &str) -> std::sync::MutexGuard<'static, ()> {
        static ARCH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let g = ARCH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("SOVEREIGN_DR_REPORT_ARCHITECTURE", value);
        g
    }

    #[test]
    fn the_section_cap_is_one_decider_for_the_prompt_and_the_parser() {
        // §10.6. The cap is asked for in `plan_outline`'s prompt and enforced
        // in `parse_outline`. Two literals would let the writer be asked for
        // 12 sections and admitted for 8 — a silent truncation that reads as
        // "the model planned 8".
        //
        // Watch-it-fail: hard-code OUTLINE_MAX back into parse_outline's break
        // and the architected case admits 8 where the prompt asked for 12.
        let mut raw = String::new();
        for i in 0..20 {
            raw.push_str(&format!(
                "Section {i} — establish the thing numbered {i}.\n"
            ));
        }
        {
            let _g = architecture_guard("0");
            assert_eq!(outline_max(), OUTLINE_MAX);
            assert_eq!(
                parse_outline(&raw, outline_max()).unwrap().len(),
                OUTLINE_MAX
            );
        }
        {
            let _g = architecture_guard("1");
            assert_eq!(outline_max(), OUTLINE_MAX_ARCHITECTED);
            assert_eq!(
                parse_outline(&raw, outline_max()).unwrap().len(),
                OUTLINE_MAX_ARCHITECTED
            );
        }
        let _g = architecture_guard("0");
        assert!(
            OUTLINE_MAX_ARCHITECTED > OUTLINE_MAX,
            "the architected cap must leave room for a section per named subject"
        );
    }

    fn section_evidence_guard(value: &str) -> std::sync::MutexGuard<'static, ()> {
        static SEC_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let g = SEC_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("SOVEREIGN_DR_REPORT_SECTION_EVIDENCE", value);
        g
    }

    #[test]
    fn the_section_evidence_budget_widens_both_knobs_together() {
        // §10.6, and the reason the per-source cap is IN the decider rather
        // than beside it: widening the passage count while holding the cap at
        // 3 fills the new room from new SOURCES only, which is the opposite of
        // what a section needing depth on one protocol wants — a spec page
        // carries its detail across consecutive passages.
        //
        // Watch-it-fail: return (SECTION_PASSAGES_WIDE, PER_SOURCE_CAP) and a
        // 24-passage section can still take only 3 from the one page that
        // actually documents the subject.
        {
            let _g = section_evidence_guard("0");
            assert_eq!(
                section_evidence_budget(),
                (SECTION_PASSAGES, PER_SOURCE_CAP)
            );
            // THE SHIPPED DEFAULT IS PINNED BECAUSE IT IS NOW MEASURED.
            // It was pinned at 8/3 as a KNOWN PORT REGRESSION (a50d2fdf3 wrote
            // 8/3 fresh where the prototype used 28/5), explicitly "until the
            // flag's arm reports". The arm reported on 2026-08-27: a five-point
            // curve on bed `dr-1787807617`, one judge, zero-noise replay —
            //
            //   8x3 45.9166 | 16x4 51.3347 | 28x5 50.9864 | 44x6 50.9510 | 60x8 51.9689
            //
            // — and 16/4 is the knee. So this pin no longer guards a regression
            // we tolerated; it guards a number we bought. The obligation is
            // unchanged and runs in BOTH directions: moving it again means
            // flying the arm again, not arguing from the shape of the curve.
            //
            // Watch-it-fail: set SECTION_PASSAGES back to 8 and this fails with
            // the delta the revert would cost.
            assert_eq!(
                (SECTION_PASSAGES, PER_SOURCE_CAP),
                (16, 4),
                "the shipped default moved. 16/4 is the MEASURED knee \
                 (+5.42 RACE overall over the old 8/3, bed dr-1787807617, \
                 2026-08-27) — re-fly the curve before changing it, and see \
                 `sovereign/DEFAULTS_LEDGER.md` \
                 §SOVEREIGN_DR_REPORT_SECTION_EVIDENCE."
            );
        }
        {
            let _g = section_evidence_guard("1");
            let (want, cap) = section_evidence_budget();
            assert_eq!((want, cap), (SECTION_PASSAGES_WIDE, PER_SOURCE_CAP_WIDE));
            assert!(want > SECTION_PASSAGES, "the budget must widen");
            assert!(
                cap > PER_SOURCE_CAP,
                "the per-source cap must widen with it"
            );
            assert!(
                want >= cap * 2,
                "no single source may fill the section on its own: {want} vs {cap}"
            );
            // The wide budget restores the Python prototype the Rust port was
            // written from (arms/lab/compose2.py: k=28, repeat_cap=5, same
            // 1400/200 passage chunking). It is pinned to the prototype so the
            // flag keeps testing a configuration a measurement stood behind.
            //
            // It is NO LONGER the target the default should converge on: the
            // 2026-08-27 curve scored 28/5 at 50.9864 against 16/4's 51.3347,
            // for +1.1 min and larger prompts. The default moved to 16/4; this
            // stayed 28/5 because it is a different question (how wide can a
            // section go), not because it won.
            assert_eq!((want, cap), (28, 5), "the prototype's measured budget");
            assert_eq!(
                want * PASSAGE_CHARS,
                39_200,
                "compose2.py recorded evidence_chars_per_section = k * 1400"
            );
        }
        let _g = section_evidence_guard("0");
    }

    #[test]
    fn the_title_parser_refuses_what_is_not_a_title() {
        // Named failing inputs (§18.1). A title that runs to a paragraph is
        // the model answering the question instead of naming the report; a
        // two-word one names nothing. Both must fall back to the question
        // rather than land as the H1.
        let paragraph = "a".repeat(TITLE_MAX_CHARS + 1);
        assert!(
            parse_title(&paragraph).is_err(),
            "a paragraph is not a title"
        );
        assert!(parse_title("Protocols").is_err(), "a stub is not a title");
        assert!(parse_title("   \n\n  ").is_err(), "empty is not a title");
        assert!(
            parse_title(&paragraph).unwrap_err().contains("unusable"),
            "the refusal names itself"
        );
    }

    #[test]
    fn the_title_parser_takes_the_first_real_line_and_strips_its_decoration() {
        let raw = "# \"A2A vs MCP: Differences, Connections, and What A2A Solves\"\n\n                   Some commentary the model added after.";
        assert_eq!(
            parse_title(raw).unwrap(),
            "A2A vs MCP: Differences, Connections, and What A2A Solves"
        );
        // A leading list marker is decoration too, and a think-tag opener is
        // not the title — the parser skips it rather than adopting it.
        assert_eq!(
            parse_title("<think>\n- The Elderly Consumption Outlook for Japan, 2020-2050").unwrap(),
            "The Elderly Consumption Outlook for Japan, 2020-2050"
        );
    }

    #[test]
    fn a_long_page_keeps_its_tail_when_the_budget_bites() {
        // THE anti-cutoff property. A budget spent by truncating each chunk
        // loses the END of every long page, silently — and a page's most
        // specific material is as likely to sit at the bottom as the top.
        // Here the only passage that answers the question is the last span of
        // a long document, and the budget admits barely two passages.
        //
        // Watch-it-fail: select by document order (drop `ranked`, feed the
        // passages unsorted) and the needle is never admitted.
        let mut w = window();
        let filler = "padding sentence with no bearing on the question. ".repeat(120);
        w.chunks[0].content = format!("{filler}THE-NEEDLE sits at the very end.");
        let passages = window_passages(&w);
        assert!(
            passages.len() > 3,
            "the page must split into several passages"
        );
        // Rank: the needle passage first, as an embedder would order it.
        let mut ordered = passages.clone();
        ordered.sort_by_key(|p| !p.text.contains("THE-NEEDLE"));
        let out = bounded_evidence(&ordered, true, 2 * PASSAGE_CHARS);
        assert!(
            out.text.contains("THE-NEEDLE"),
            "the tail of a long page must survive a tight budget:\n{}",
            out.text
        );
        assert!(
            out.passages_dropped > 0,
            "this budget must actually bite, or the test proves nothing"
        );
    }

    #[test]
    fn rotation_gives_every_source_a_turn_before_any_source_repeats() {
        // The no-embedder fill. Position in the window must never be the
        // selector: a run that fetched forty sources and showed the model the
        // first two is the information loss this bound exists to prevent.
        let mut w = window();
        w.chunks.clear();
        for i in 0..4 {
            let mut c = crate::deep_research::icd::WindowChunk {
                id: format!("ev-{i}"),
                locator: format!("https://s{i}.example/p"),
                source_url: format!("https://s{i}.example/p"),
                custody: "public-web".to_string(),
                provenance_class: "known".to_string(),
                content: String::new(),
                ingested_into: None,
                tags: Vec::new(),
            };
            c.content = format!("source {i} body. ").repeat(300);
            w.chunks.push(c);
        }
        let passages = window_passages(&w);
        let out = bounded_evidence(&passages, false, 4 * PASSAGE_CHARS);
        for i in 0..4 {
            assert!(
                out.text.contains(&format!("source {i} body")),
                "every source contributes before any repeats; source {i} missing:\n{}",
                out.text
            );
        }
    }

    #[test]
    fn the_budget_holds_and_what_it_dropped_is_counted() {
        let mut w = window();
        w.chunks[0].content = "long body sentence here. ".repeat(500);
        let passages = window_passages(&w);
        let budget = 3 * PASSAGE_CHARS;
        let out = bounded_evidence(&passages, false, budget);
        assert!(
            out.chars_used <= budget,
            "budget {budget} exceeded: {} chars",
            out.chars_used
        );
        assert_eq!(
            out.passages_used + out.passages_dropped,
            passages.len(),
            "every passage is either admitted or counted as dropped — never \
             unaccounted for"
        );
    }

    #[test]
    fn the_admitted_bodies_are_exactly_what_the_prompt_carries() {
        // The figure inventory is built from `admitted`. If admitted and text
        // could disagree, the inventory would name figures the model cannot
        // see — an instruction to invent, aimed at the numbers the audit
        // checks hardest.
        let mut w = window();
        w.chunks[0].content = "the value was 42 percent in 2024. ".repeat(200);
        let passages = window_passages(&w);
        let out = bounded_evidence(&passages, false, 2 * PASSAGE_CHARS);
        for (_, body) in out.admitted.iter() {
            assert!(
                out.text.contains(body.as_str()),
                "an admitted body must appear verbatim in the prompt text"
            );
        }
        let inv = figure_inventory_of(&out.admitted);
        if !inv.is_empty() {
            assert!(
                inv.contains("ev-1"),
                "the inventory names the chunk the admitted body came from"
            );
        }
    }

    #[test]
    fn an_empty_window_yields_an_empty_block_not_a_panic() {
        let out = bounded_evidence(&[], false, 10_000);
        assert_eq!(out.passages_used, 0);
        assert_eq!(out.chars_used, 0);
        assert!(out.text.is_empty());
    }

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

    /// drb1-r7: the ported AIQ additions are present, and are the ones v1
    /// does NOT already carry. A duplicate obligation is a contract arguing
    /// with itself, so this asserts both directions.
    #[test]
    fn the_v2_extra_ports_what_v1_lacks_and_does_not_repeat_it() {
        for needle in [
            "[ANCHOR]",
            "[SUPPORT]",
            "[WEAK]",
            "CONSENSUS",
            "COMPLEMENTARY",
            "Inference is allowed",
            "units, dates and ranges",
        ] {
            assert!(
                WRITER_CONTRACT_V2_EXTRA.contains(needle),
                "the ported contract must carry {needle:?}"
            );
        }
        // v1 already holds these; repeating them in v2 would double-state the
        // obligation in one prompt.
        for dup in ["Cross-synthesize", "Developed paragraphs", "Do NOT"] {
            assert!(
                !WRITER_CONTRACT_V2_EXTRA.contains(dup),
                "{dup:?} is already in v1 — v2 must not repeat it"
            );
        }
    }

    /// The grade bands straddle DEFAULT_USEFULNESS (50): a worker that
    /// declined to score its finding must land in SUPPORT, never be read as
    /// having judged the finding an anchor OR weak (§18.3 — an absent value
    /// is not a verdict).
    #[test]
    fn an_unscored_finding_is_support_never_anchor_or_weak() {
        assert_eq!(grade_for_usefulness(50), "SUPPORT");
        assert_eq!(grade_for_usefulness(70), "ANCHOR");
        assert_eq!(grade_for_usefulness(69), "SUPPORT");
        assert_eq!(grade_for_usefulness(40), "SUPPORT");
        assert_eq!(grade_for_usefulness(39), "WEAK");
        assert_eq!(grade_for_usefulness(0), "WEAK");
        assert_eq!(grade_for_usefulness(100), "ANCHOR");
    }

    /// A section with one or two passages must not have its only evidence
    /// marked WEAK — the contract tells the writer not to build on WEAK, so a
    /// naive top-third rule would instruct it to build on nothing.
    #[test]
    fn a_thin_section_never_grades_its_only_evidence_weak() {
        for n in 1..=2 {
            for i in 0..n {
                assert_eq!(
                    grade_for_rank(i, n),
                    "ANCHOR",
                    "with n={n} every passage must anchor"
                );
            }
        }
        // At the real section width every band is populated and best-first
        // order is respected.
        let g: Vec<&str> = (0..8).map(|i| grade_for_rank(i, 8)).collect();
        assert_eq!(g[0], "ANCHOR", "the top-ranked passage anchors");
        assert_eq!(g[7], "WEAK", "the worst-ranked passage is weak");
        assert!(
            g.contains(&"SUPPORT"),
            "the middle band is populated: {g:?}"
        );
        // No WEAK may outrank an ANCHOR.
        let first_weak = g.iter().position(|x| *x == "WEAK").unwrap();
        let last_anchor = g.iter().rposition(|x| *x == "ANCHOR").unwrap();
        assert!(
            last_anchor < first_weak,
            "grades must be monotone in rank: {g:?}"
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
