// SPDX-License-Identifier: AGPL-3.0-or-later
//! R4 + R5 — query forming (G, cheap) and triage as RANKER never
//! excluder.
//!
//! Query forming: every gap's `actionable_query` is the round's query
//! (formed deterministically at audit time — reproducible, zero model
//! tokens). `form_queries` materializes the ICD rows and records who
//! formed each query. The figure-hunting step (order deep-research-t1e)
//! is the generic "what measures and numbers does this question
//! imply?" — the question's OWN figure specifiers (its digits and its
//! measure-family words), folded into any sub-question or gap query
//! that carries none. SHAPE, never the test: the bank's named measures
//! (Gini, Case-Shiller, 80/20, ...) never enter this lexicon or any
//! prompt — the model names those from its own knowledge, under a
//! generic instruction.
//!
//! Triage (R5): rank the round's search hits by score; the code-set K
//! is the top-K (or all when fewer); an ε-quota of below-cut fetches is
//! admitted by rank — the cut is a rank boundary, never an exclusion.
//! Admission favors figure-bearing hits (order deep-research-t1e): a
//! hit whose title or snippet carries a figure token outranks a
//! same-scored figure-less hit, so the K-cut does not silently exclude
//! the evidence the figures live in (the t1d journal's cap: wiki-
//! inequality rank 5 and brookings rank 7 cut at K=4 on the v1 flight,
//! all scores tied at 0.9). Every skipped hit lands in the skip ledger
//! ICD (F25): the ledger is the answer to "what did the loop see and
//! not fetch, and why?".

use super::figure_tokens;
use super::icd::{FetchList, FormedQuery, Gap, SearchHit, SkipEntry, SkipLedger, TriageOutcome};

/// Materialize the round's queries from its gaps. Cheap and
/// deterministic: the gap's actionable query IS the query. `G` (the
/// provider) is deliberately not consulted — a reproducible thin loop
/// spends tokens on judgment, not on re-forming text it already wrote.
///
/// `preplanned` (t1d fix 2 — breadth): the plan's acquisition frontier,
/// appended AFTER the gap queries and formed_by "plan-subquestion". The
/// caller decides which rounds carry the frontier (the loop: round 1
/// only — the initial acquisition; rounds 2+ are gap-targeted
/// follow-ups).
/// The ONE query-validity decider (acquisition tune, 2026-08-24):
/// `None` when the text is dispatchable, `Some(reason)` when it is not.
///
/// WHY IT EXISTS. `actionable_query` is a string template over a claim
/// sentence (audit.rs `query_for` → mod.rs `template_query`), and nothing
/// between the claim and the search backend asked whether the result was
/// a query. The logged t7a flight dispatched `###` three times and the
/// empty string three times on task 90 round 2 alone — 21% of that task's
/// search allowance spent on markdown the draft happened to contain.
///
/// WHAT IT DOES NOT DO. It does not judge query QUALITY. The same flight
/// formed gap queries that are mangled draft prose ("They equipped
/// internal clocks allow them interpret changing patterns throughou") —
/// those clear this bar and should, because separating a clumsy query
/// from a good one is judgment, and a deterministic three-word count that
/// pretended to have it would be the worse failure (§7.6). This gate
/// answers exactly one question: is there a query here at all?
pub fn query_refusal(text: &str) -> Option<&'static str> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Some("empty");
    }
    if content_word_count(trimmed) < MIN_QUERY_CONTENT_WORDS {
        return Some("fewer-than-3-content-words");
    }
    None
}

/// A query needs this many content words to be worth a search call.
/// Three is the floor at which a web query names a subject and something
/// about it; below that the flight's own record is markdown scaffolding
/// and bare fragments.
pub const MIN_QUERY_CONTENT_WORDS: usize = 3;

/// A content word: at least two characters and at least one alphanumeric
/// once markup and punctuation are trimmed. `###`, `**`, `(3)` and `—`
/// are not content words; `MCP`, `1873` and `A2A` are.
fn content_word_count(text: &str) -> usize {
    text.split_whitespace()
        .filter(|w| {
            let core = w.trim_matches(|c: char| !c.is_alphanumeric());
            core.chars().count() >= 2 && core.chars().any(|c| c.is_alphanumeric())
        })
        .count()
}

/// Mint the round's queries.
///
/// `gap_queries` is the port's model-formed reformulation of the gaps
/// (AIQ planner rule 3 — see `ResearchPort::gap_queries`), positionally
/// matched to `gaps`. `None` means the port had no such surface or
/// refused, and the deterministic `actionable_query` template is used
/// instead — which is RECORDED on the query's `formed_by`, so the
/// artifact always says which shape produced it and a silent revert to
/// the template is impossible (§18.3).
pub fn form_queries(
    run_id: &str,
    charter_hash: &str,
    round: u32,
    gaps: &[Gap],
    preplanned: &[String],
    gap_queries: Option<&[String]>,
) -> FetchList {
    // The refusal ledger: a formed query the gate declined. Recorded in
    // formation order so the artifact reads as "what the round tried".
    let mut refused_queries: Vec<super::icd::RefusedQuery> = Vec::new();
    // Positional: the port's contract is one line per gap, in order, and
    // it refuses rather than return a different count.
    let model_formed = gap_queries.filter(|q| q.len() == gaps.len());
    let gap_source = if model_formed.is_some() {
        "gap-model"
    } else {
        "gap-template"
    };
    let text_for = |i: usize, g: &Gap| -> String {
        model_formed
            .and_then(|q| q.get(i).cloned())
            .unwrap_or_else(|| g.actionable_query.clone())
    };
    let mut queries: Vec<FormedQuery> = gaps
        .iter()
        .enumerate()
        .filter(|(i, g)| {
            let text = text_for(*i, g);
            if let Some(reason) = query_refusal(&text) {
                refused_queries.push(super::icd::RefusedQuery {
                    text,
                    reason: reason.to_string(),
                    from_gap_id: Some(g.id.clone()),
                    formed_by: gap_source.to_string(),
                });
                return false;
            }
            true
        })
        .map(|(i, g)| (i, g))
        .enumerate()
        .map(|(n, (i, g))| FormedQuery {
            id: format!("q{}", n + 1),
            // The port's model-formed query when there is one, else the
            // t6f rung 2 template (actionable_query — the search-shaped
            // form of the gap; MICRO-PROBE VALIDATED: gap.text sentence
            // form "Meridian Bridge completed" doesn't match keyword
            // tokens like "completion", the actionable form does).
            // `formed_by` records which, so the fetch list always names
            // the shape that produced it.
            text: text_for(i, g),
            from_gap_id: Some(g.id.clone()),
            formed_by: gap_source.to_string(),
            provider: if model_formed.is_some() {
                "port".to_string()
            } else {
                "deterministic".to_string()
            },
            // t1d fix 3: the floor's record rides the query into the
            // fetch list — the artifact is self-describing.
            corroboration: g.corroboration.clone(),
        })
        .collect();
    let mut next = queries.len() + 1;
    for q in preplanned {
        if let Some(reason) = query_refusal(q) {
            refused_queries.push(super::icd::RefusedQuery {
                text: q.clone(),
                reason: reason.to_string(),
                from_gap_id: None,
                formed_by: "plan-subquestion".to_string(),
            });
            continue;
        }
        queries.push(FormedQuery {
            id: format!("q{next}"),
            text: q.clone(),
            from_gap_id: None,
            formed_by: "plan-subquestion".to_string(),
            provider: "deterministic".to_string(),
            corroboration: None,
        });
        next += 1;
    }
    if !refused_queries.is_empty() {
        tracing::warn!(
            target: "deep_research",
            run_id,
            round,
            formed = queries.len() + refused_queries.len(),
            dispatched = queries.len(),
            refused = refused_queries.len(),
            reasons = ?refused_queries.iter().map(|r| r.reason.as_str()).collect::<Vec<_>>(),
            "acquisition: formed queries refused before dispatch (not a query)"
        );
    }
    FetchList {
        icd: "fetch_list".to_string(),
        version: super::icd::ICD_VERSION,
        run_id: run_id.to_string(),
        charter_hash: charter_hash.to_string(),
        round,
        queries,
        refused_queries,
        search_hits: Vec::new(),
        triage: TriageOutcome {
            code_set_k: Vec::new(),
            eps_admits: Vec::new(),
            below_cut: Vec::new(),
            threshold: 0.0,
            eps_quota: 0.0,
            admission_rule: ADMISSION_RULE_SCORE_THEN_FIGURE.to_string(),
        },
    }
}

/// The generic measure-family lexicon — the CLOSED set of words that
/// name a measure or statistic (an index, a ratio, a share, a rate, a
/// count, a price, a median ...). Applied ONLY to the question's own
/// text: the figure-hunting step is shape ("what measures and numbers
/// does this question imply?"), never bank-derived. Direction words
/// (change, increase, decline) are deliberately absent — they describe
/// movement, not a measure. The bank's NAMED measures (Gini, Case-
/// Shiller, 80/20, white share, ...) never enter this list or any
/// prompt; naming those is the model's job under the generic
/// instruction (deep_research_cmd.rs plan_subquestions).
const MEASURE_WORDS: &[&str] = &[
    "index",
    "ratio",
    "share",
    "rate",
    "percent",
    "percentage",
    "median",
    "average",
    "mean",
    "count",
    "number",
    "price",
    "income",
    "earnings",
    "wage",
    "salary",
    "employment",
    "jobs",
    "population",
    "mobility",
    "cost",
    "rent",
    "poverty",
    "wealth",
    "proportion",
    "statistic",
    "metric",
    "estimate",
    "amount",
    "total",
    "level",
];

/// The question's OWN figure specifiers — the answer to "what measures
/// and numbers does this question imply?", read from the question's own
/// text: its figure tokens (digit runs, in text order) followed by its
/// measure-family words (MEASURE_WORDS ∩ question, in text order),
/// deduped. Deterministic C-class, zero model tokens, applied to the
/// question — never to any bank text. One decider, one name (§10.6):
/// every consumer (frontier fold-in, gap-query fold-in, the scorer's
/// presence measurement) reads THIS.
pub fn figure_specifiers(question: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for f in figure_tokens(question) {
        if !out.contains(&f) {
            out.push(f);
        }
    }
    let lower = question.to_ascii_lowercase();
    for w in lower.split(|c: char| !c.is_alphanumeric()) {
        if MEASURE_WORDS.contains(&w) && !out.iter().any(|s| s.to_ascii_lowercase() == w) {
            out.push(w.to_string());
        }
    }
    out
}

/// Does the text carry a figure specifier — a digit run or a
/// measure-family word (whole-word, case-insensitive)? The fold-in
/// rule's guard: a sub-question or query that already carries a
/// specifier stands as drafted; one that carries none gets the
/// question's specifiers folded in.
pub fn has_figure_specifier(text: &str) -> bool {
    if !figure_tokens(text).is_empty() {
        return true;
    }
    let lower = text.to_ascii_lowercase();
    lower
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| MEASURE_WORDS.contains(&w))
}

/// R1 fold-in (order deep-research-t1e): the acquisition frontier's
/// sub-questions are the round-1 queries; a sub-question that carries
/// NO figure specifier (no digit, no measure word) gets the question's
/// own specifiers folded in — the plan artifact's sub-questions carry
/// figure specifiers for a question whose own text implies figures,
/// structurally, whatever the draft returned. A sub-question that
/// already carries a specifier stands as drafted (the model's named
/// measures — Gini, ratio, Case-Shiller — are never overwritten).
pub fn figure_hunt_frontier(frontier: Vec<String>, question: &str) -> Vec<String> {
    let specs = figure_specifiers(question);
    if specs.is_empty() {
        return frontier;
    }
    frontier
        .into_iter()
        .map(|sub| {
            if has_figure_specifier(&sub) {
                sub
            } else {
                format!("{sub} ({})", specs.join(", "))
            }
        })
        .collect()
}

/// R4 fold-in: a gap query (the claim's prose template) that carries
/// no figure specifier gets the question's own specifiers appended —
/// a thematic claim's follow-up query still hunts the figures the
/// question implies, so the numbers never silently drop out of the
/// acquisition. The floor-capped FACT query already carries the
/// claim's figures and never passes through here.
pub fn figure_hunt_query(query: String, question_specifiers: &[String]) -> String {
    if has_figure_specifier(&query) || question_specifiers.is_empty() {
        query
    } else {
        format!("{query} ({})", question_specifiers.join(", "))
    }
}

/// The triage admission preference (R5, order deep-research-t1e): a
/// hit is figure-bearing when its own title, snippet, or BODY carries
/// a figure token — the evidence's figures are on the hit, and the
/// K-cut must not silently exclude the hits the figures live in
/// (the t1d journal's v1 shape: wiki-inequality and brookings cut at
/// rank 5 and 7, all scores tied at 0.9, insertion order deciding).
/// The body joined the decider in t1h — the corpus leg's boundary: the
/// corpus surface's titles are digit-free document names and its
/// snippets are term-centered 600-char cuts, so the body is where the
/// digits live (the t1g v1 flight's chunk 65, the Gini-bearing
/// source-report chunk, skipped at rank 6 — skip-ledger-1.json).
/// Deterministic, reuses the one figure-token decider.
pub fn figure_bearing(hit: &SearchHit) -> bool {
    !figure_tokens(&hit.title).is_empty()
        || !figure_tokens(&hit.snippet).is_empty()
        || hit
            .content
            .as_deref()
            .is_some_and(|c| !figure_tokens(c).is_empty())
}

/// The one admission rule's name, recorded on the triage outcome
/// (glassbox — the artifact names the decider it ran).
pub const ADMISSION_RULE_SCORE_THEN_FIGURE: &str = "score-then-figure-bearing";

/// The admission thresholds' production defaults (drb1-t1 — one
/// decider, one name: the charter's `triage.code_set_k` /
/// `triage.eps_quota` and the CLI's flag defaults read THESE; the
/// replay harness and the red tests replay the same numbers). Tuned
/// on the logged t7a flight (order drb1-t1 item 4): at K=3 the
/// logged task 56 round 1 admitted four unfetchable PDF urls and no
/// second chance existed within the round; K=5 (+ the ε quota's one
/// rank) admits the fetchable exact-topic pages behind them. See the
/// T1 execution record for the measured before/after.
pub const DEFAULT_CODE_SET_K: usize = 5;
pub const DEFAULT_EPS_QUOTA: f64 = 0.1;

/// drb1-t1 — the web leg's admission score: the fraction of the
/// query's DISTINCT terms present in the hit's recorded surface
/// (title + snippet + url), in [0, 1]. Deterministic, zero model
/// tokens.
///
/// WHY this exists: the production web port recorded a constant
/// `score: 0.0` for every web hit (deep_research_cmd.rs — the t7a
/// flight's 843/843 logged rows at exactly 0.0, 775 of them skipped
/// "below-cut"), so triage ranked a fully-tied field and admission
/// fell to the figure-bearing tie-break plus backend insertion order
/// — task 56's exact-topic papers (brocku, kasberger, researchgate,
/// sciencedirect) all cut at 0.0 while four unfetchable PDF urls
/// took the code set. The mock leg's decider (gym.rs
/// `Deck::relevance` — distinct query terms present in the hit's
/// term set) is the reference shape (§10.6); this is that decider
/// over the web hit's own recorded surface, normalized by the
/// query's distinct-term count so hits from DIFFERENT queries in one
/// round are comparable. One scorer per leg, one triage: the corpus
/// leg keeps the index's own score; the mock keeps the deck's.
///
/// The URL joins the surface because web titles degenerate (a PDF's
/// `<title>` is its filename — "asymmetricfpa24.dvi" carries none of
/// "asymmetric first price auctions") while the URL often carries
/// the paper's slug
/// (`researchgate.net/.../Linear_Bid_in_Asymmetric_First_Price_Auctions`).
pub fn web_hit_relevance(query: &str, title: &str, snippet: &str, url: &str) -> f64 {
    let q = terms(query);
    if q.is_empty() {
        return 0.0;
    }
    let surface: std::collections::HashSet<String> = terms(&format!("{title}\n{snippet}\n{url}"))
        .into_iter()
        .collect();
    let covered = q.iter().filter(|t| surface.contains(*t)).count();
    covered as f64 / q.len() as f64
}

/// The one tokenizer (T1.9): lowercase, split on non-alphanumeric,
/// empty tokens dropped, deduped in first-appearance order. Applied
/// identically to queries and to every indexed surface — one decider
/// for both sides. A decimal figure splits at the point ("0.5469" →
/// "0", "5469") — the same split a punctuation-splitting analyzer
/// makes. Lives here (production's admission path) since drb1-t1;
/// the search gym imports it (it owned the shape from T1.9 — the
/// move is the fn verbatim, no behavior change).
pub fn terms(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in text
        .to_ascii_lowercase()
        .split(|c: char| !c.is_alphanumeric())
    {
        if t.is_empty() {
            continue;
        }
        if !out.iter().any(|o| o == t) {
            out.push(t.to_string());
        }
    }
    out
}

/// The triage result for one round's hits.
#[derive(Debug, Clone)]
pub struct TriageResult {
    /// The ranked hits, code-set K first (the fetch list's search_hits
    /// order is the rank order).
    pub ranked: Vec<SearchHit>,
    /// drb1-t2 (permissive triage): the FULL non-noise candidate list
    /// in rank order — the fetch leg's walk queue. Under
    /// fetch-then-judge the pre-fetch gate demotes noise only, so the
    /// queue extends past the K ∪ ε tiers and the round's fetch budget
    /// (plus fallbacks past failures) decides how deep it walks.
    pub candidates: Vec<SearchHit>,
    pub outcome: TriageOutcome,
    pub skip_ledger: SkipLedger,
}

/// The noise classes (drb1-t2, AIQ §1.3 ph.3's "demote obvious junk")
/// — a CLOSED set, classified from the URL alone (host + path). Jobs
/// boards, careers pages, and social surfaces are never research
/// evidence whatever the query; everything else stays a candidate and
/// the topicality decision happens on CONTENT after fetch. Measured on
/// the logged t7a flight: 70 of 843 rows carry one of these shapes
/// (65 social — 27 youtube, 23 facebook, 11 linkedin, 3 reddit, 1
/// instagram; 3 jobs boards; 2 careers hosts/paths), 9 of which the
/// T1 admission would have spent fetches on (a youtube stub admitted
/// on task 56 round 2 among them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoiseClass {
    /// Social/video surfaces (facebook, youtube, linkedin, reddit, …).
    Social,
    /// Dedicated jobs boards (indeed, glassdoor, amazon.jobs, …).
    JobsBoard,
    /// A careers/jobs subdomain or host prefix.
    CareersHost,
    /// A careers/jobs PATH segment on an otherwise general host.
    CareersPath,
}

impl NoiseClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Social => "social",
            Self::JobsBoard => "jobs-board",
            Self::CareersHost => "careers-host",
            Self::CareersPath => "careers-path",
        }
    }
}

/// The social hosts — demoted whatever the query. Matched on the
/// registrable domain (last two labels), so `www.`/`m.`/`mobile.`
/// prefixes and subdomains all classify.
const SOCIAL_HOSTS: &[&str] = &[
    "facebook.com",
    "twitter.com",
    "x.com",
    "instagram.com",
    "linkedin.com",
    "tiktok.com",
    "reddit.com",
    "youtube.com",
    "pinterest.com",
    "threads.net",
];

/// Dedicated jobs-board hosts (registrable domains).
const JOBS_BOARD_HOSTS: &[&str] = &[
    "indeed.com",
    "glassdoor.com",
    "monster.com",
    "ziprecruiter.com",
    "amazon.jobs",
    "myworkdayjobs.com",
    "lever.co",
    "greenhouse.io",
];

/// The host's registrable domain (last two labels — the approximation
/// the closed sets above are calibrated for; no public-suffix list,
/// the lists are the decider).
fn registrable_host(host: &str) -> &str {
    let trimmed = host.trim_start_matches("www.");
    if let Some((_, last2)) = trimmed.rsplit_once('.') {
        if let Some((_, last1)) = last2.rsplit_once('.') {
            return last1;
        }
    }
    trimmed
}

/// Classify a URL as pre-fetch noise. One decider (the fetch leg, the
/// replay harness, and the skip-ledger writer all call THIS); returns
/// `None` for everything that stays a candidate. Host-based rules run
/// first (strongest signal); the careers/jobs PATH segment is the
/// weakest and last — a `.gov`/`.edu` statistics page whose path
/// happens to carry /jobs/ is not demoted (BLS's jobs report shape:
/// the host is not a board, the page is data).
pub fn noise_class(url: &str) -> Option<NoiseClass> {
    let lower = url.to_ascii_lowercase();
    let after_scheme = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .unwrap_or(&lower);
    let (host, path) = match after_scheme.split_once('/') {
        Some((h, p)) => (h, format!("/{p}")),
        None => (after_scheme, String::new()),
    };
    // Strip credentials/port noise; keep the host simple.
    let host = host.split('@').next_back().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);
    let domain = registrable_host(host);
    if SOCIAL_HOSTS.contains(&domain) {
        return Some(NoiseClass::Social);
    }
    if JOBS_BOARD_HOSTS.contains(&domain) {
        return Some(NoiseClass::JobsBoard);
    }
    if host.starts_with("careers.") || host.starts_with("jobs.") {
        return Some(NoiseClass::CareersHost);
    }
    let segs = || path.split('/').filter(|s| !s.is_empty());
    if segs().any(|s| s == "careers" || s == "jobs") {
        // Weakest rule: a careers/jobs path segment on a general host.
        // Government/education hosts are excluded — their /jobs/ paths
        // are data (labor statistics), not postings.
        let tld = domain.rsplit('.').next().unwrap_or("");
        if tld != "gov" && tld != "edu" {
            return Some(NoiseClass::CareersPath);
        }
    }
    None
}

/// The post-fetch content admission floors (drb1-t2). A fetched page
/// admits to the evidence window when its CONTENT covers at least
/// `DEFAULT_CONTENT_COVERAGE_FLOOR` of the query's distinct terms OR
/// carries a prose line of at least `DEFAULT_PROSE_LINE_FLOOR` chars.
/// Calibrated on the logged t7a flight's 45 recorded surviving chunks
/// (own measurement): coverage-only real pages floor at 0.38
/// (m-malinowski.github.io), rejected stubs top at 0.21 (sunmi news);
/// prose lines — rejects peak at 338 chars (atlan.com), admits start
/// at 561 (simutechgroup.com). 0.25 and 500 sit mid-gap; any pair in
/// (0.21, 0.31) × (338, 561) classifies the 45 identically.
pub const DEFAULT_CONTENT_COVERAGE_FLOOR: f64 = 0.25;
pub const DEFAULT_PROSE_LINE_FLOOR: usize = 500;

/// Serde default accessors (the charter's TriageConfig reads these —
/// one decider, one name: the const IS the default).
pub fn default_content_coverage_floor() -> f64 {
    DEFAULT_CONTENT_COVERAGE_FLOOR
}

pub fn default_prose_line_floor() -> usize {
    DEFAULT_PROSE_LINE_FLOOR
}

/// The content-admission rule's name (rides the window's refusal
/// records and the tracing event — glassbox).
pub const CONTENT_ADMISSION_RULE: &str = "coverage-floor-or-prose";

/// The longest line in the fetched content — the prose signal. A page
/// whose extraction delivered only site chrome (nav labels, menus,
/// disclaimers — the task-65 shape) has no long line; real body text
/// arrives as paragraphs. One accessor (fetch and the replay harness
/// both call THIS).
pub fn prose_line_length(content: &str) -> usize {
    content.lines().map(str::len).max().unwrap_or(0)
}

/// drb1-t2 — the post-fetch content admission verdict. REUSE finding
/// (one decider): the coverage score is the SAME admission scorer the
/// pre-fetch gate runs (`web_hit_relevance`'s term-coverage core, the
/// ONE tokenizer), applied to the content surface instead of the
/// metadata surface — one scorer, two surfaces, zero model tokens.
/// The witness/containment path cannot serve here (draft-shaped,
/// judge-bound, downgrade-only — see the T2 declaration); this is the
/// machinery that could.
#[derive(Debug, Clone, PartialEq)]
pub struct ContentVerdict {
    pub admits: bool,
    pub coverage: f64,
    pub prose_line: usize,
    /// The named refusal reason when `!admits` (empty string when
    /// admitted) — recorded on the window's `content_refused`, never
    /// a silent un-ledgering.
    pub reason: String,
}

/// Judge fetched content for window admission. `admits ⇔ coverage ≥
/// coverage_floor ∨ prose_line ≥ prose_floor`. Empty content is its
/// own named reason (the semanticscholar shape: the fetch succeeded
/// and delivered nothing).
pub fn judge_content(
    query: &str,
    title: &str,
    content: &str,
    url: &str,
    coverage_floor: f64,
    prose_floor: usize,
) -> ContentVerdict {
    let prose_line = prose_line_length(content);
    if content.trim().is_empty() {
        return ContentVerdict {
            admits: false,
            coverage: 0.0,
            prose_line,
            reason: "empty-content".to_string(),
        };
    }
    let coverage = web_hit_relevance(query, title, content, url);
    let admits = coverage >= coverage_floor || prose_line >= prose_floor;
    let reason = if admits {
        String::new()
    } else {
        format!(
            "content-below-threshold: coverage {coverage:.3} < {coverage_floor}, \
             longest line {prose_line} < {prose_floor} ({CONTENT_ADMISSION_RULE})"
        )
    };
    ContentVerdict {
        admits,
        coverage,
        prose_line,
        reason,
    }
}

/// Rank the hits, cut at K, admit an ε-quota of below-cut fetches by
/// rank. The threshold recorded is the score of the last code-set
/// member (a rank boundary, not a semantic bar). Ties break on
/// figure-bearing-ness first (t1e — the K-cut must not silently
/// exclude the hits the figures live in), then insertion order —
/// deterministic. The admission rule's name rides the outcome.
///
/// drb1-t2 (permissive triage): the RANKING and the K/ε tiers run
/// over ALL rows unchanged — the recorded tiers stay comparable to
/// every logged flight (the T1 parity gate replays recorded rounds
/// through this function; 8 noise urls sit inside the logged flight's
/// recorded admitted sets, so demoting them out of the ranking would
/// break the instrument). What changes is the FETCH side: the
/// `candidates` queue excludes noise rows (they never spend a fetch),
/// every NON-noise row is a queue member whatever its score (under
/// fetch-then-judge the pre-fetch gate demotes junk only and never
/// exclusively decides topicality), and every noise row gets a skip
/// ledger row with reason `noise-demoted:{class}` whether or not its
/// rank placed it in a tier — the ledger is the complete F25 record,
/// and a tier-ranked noise row that never fetched is exactly the
/// phantom shape the ledger exists to prevent.
pub fn triage_hits(
    run_id: &str,
    charter_hash: &str,
    round: u32,
    mut hits: Vec<SearchHit>,
    k: usize,
    eps_quota: f64,
) -> TriageResult {
    hits.sort_by(|a, b| {
        b.score.total_cmp(&a.score).then_with(|| {
            let fb_a = figure_bearing(a);
            let fb_b = figure_bearing(b);
            fb_b.cmp(&fb_a)
        })
    });
    // The permissive partition: noise rows stay in the ranked field
    // (parity — see the doc comment) but never enter the fetch queue.
    let candidates: Vec<SearchHit> = hits
        .iter()
        .filter(|h| noise_class(&h.url).is_none())
        .cloned()
        .collect();
    let k = k.min(hits.len());
    let code_set: Vec<SearchHit> = hits.iter().take(k).cloned().collect();
    let below_cut: Vec<SearchHit> = hits.iter().skip(k).cloned().collect();
    let threshold = code_set.last().map(|h| h.score).unwrap_or(0.0);

    let eps_budget = ((k as f64) * eps_quota).ceil() as usize;
    let eps_admits: Vec<SearchHit> = below_cut.iter().take(eps_budget).cloned().collect();

    // Skip ledger: every hit not in {code set ∪ ε admits} gets a row —
    // the ledger records the loop's judgment, not a silent drop.
    // Admission is POSITIONAL (drb1-t1): the ε admits are, by the
    // construction above, exactly the ranks in k..k+eps_budget. The
    // previous id-matched check (`eps_admits.iter().any(|a| a.id ==
    // hit.id)`) silently un-ledgered every OTHER hit sharing an
    // ε-admitted id — and the web port mints per-query counter ids
    // (`web-{i}`), so on the logged flight's task 56 round 1 the q2/q3
    // hits named web-3 vanished (ranks 13 and 20: never fetched, never
    // recorded). The positional form states the same fact without the
    // id, and drops the "beyond-eps-quota" reason branch it had left
    // unreachable (0 of 775 logged ledger rows ever carried it).
    //
    // drb1-t2: noise rows are ALWAYS ledgered (tier-ranked or not)
    // with their class as the reason; non-noise rows below the tier
    // boundary keep `below-cut` — a rank boundary, never an exclusion
    // (they remain queue members the round's budget did not reach).
    let mut entries = Vec::new();
    for (rank, hit) in hits.iter().enumerate() {
        if let Some(class) = noise_class(&hit.url) {
            entries.push(SkipEntry {
                url: hit.url.clone(),
                title: hit.title.clone(),
                score: hit.score,
                rank: rank + 1,
                reason: format!("noise-demoted:{}", class.as_str()),
                decision: "skip".to_string(),
                query_id: hit.query_id.clone(),
                snippet: hit.snippet.clone(),
            });
            continue;
        }
        if rank < k + eps_budget {
            continue;
        }
        entries.push(SkipEntry {
            url: hit.url.clone(),
            title: hit.title.clone(),
            score: hit.score,
            rank: rank + 1,
            reason: "below-cut".to_string(),
            decision: "skip".to_string(),
            // drb1-t1: the row's query and snippet ride the ledger so
            // the admission stage replays exactly from the record
            // (the logged flight could not — skipped rows carried
            // neither, which is why the replay scores them against
            // every round query on title+url alone).
            query_id: hit.query_id.clone(),
            snippet: hit.snippet.clone(),
        });
    }

    let admitted_ids: Vec<String> = code_set
        .iter()
        .map(|h| h.id.clone())
        .chain(eps_admits.iter().map(|h| h.id.clone()))
        .collect();

    // drb1-t1 glassbox: the admission decision is visible at debug —
    // before this the round's cut was reconstructable only from the
    // artifacts after the fact (§0/§9: a decision invisible at
    // tracing=debug is not finished).
    tracing::debug!(
        target: "deep_research",
        run_id,
        round,
        hits = hits.len(),
        noise_demoted = hits.len() - candidates.len(),
        candidates = candidates.len(),
        k,
        eps_quota,
        eps_budget,
        admitted = admitted_ids.len(),
        skipped = entries.len(),
        threshold,
        rule = ADMISSION_RULE_SCORE_THEN_FIGURE,
        "triage decided (drb1-t2: permissive — noise demoted, budget decides the walk)"
    );

    TriageResult {
        ranked: code_set
            .iter()
            .cloned()
            .chain(eps_admits.iter().cloned())
            .collect(),
        candidates,
        outcome: TriageOutcome {
            code_set_k: code_set.iter().map(|h| h.id.clone()).collect(),
            eps_admits: eps_admits.iter().map(|h| h.id.clone()).collect(),
            below_cut: below_cut.iter().map(|h| h.id.clone()).collect(),
            threshold,
            eps_quota,
            admission_rule: ADMISSION_RULE_SCORE_THEN_FIGURE.to_string(),
        },
        skip_ledger: SkipLedger {
            icd: "skip_ledger".to_string(),
            version: super::icd::ICD_VERSION,
            run_id: run_id.to_string(),
            charter_hash: charter_hash.to_string(),
            round,
            entries,
        },
    }
}

/// Attach the round's search hits to the fetch list (rank order).
pub fn attach_hits(fetch_list: &mut FetchList, hits: Vec<SearchHit>) {
    fetch_list.search_hits = hits;
}

/// Recover the admitted hit ids (the fetch list's triage outcome).
pub fn admitted_ids(fetch_list: &FetchList) -> Vec<String> {
    fetch_list
        .triage
        .code_set_k
        .iter()
        .chain(fetch_list.triage.eps_admits.iter())
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(id: &str, score: f64) -> SearchHit {
        SearchHit {
            id: id.to_string(),
            query_id: "q1".to_string(),
            url: format!("https://example.com/{id}"),
            // The title must carry NO digit: figure_bearing reads the
            // title, and the ids ("h1", "h2") leak digits — a digit
            // in the title would saturate the tie-break (every hit
            // figure-bearing, insertion order deciding again).
            title: format!(
                "title {}",
                id.chars()
                    .filter(|c| c.is_ascii_alphabetic())
                    .collect::<String>()
            ),
            snippet: String::new(),
            // The triage fixture carries no body by default — the
            // tests that exercise the body fill it explicitly.
            content: None,
            engine: "duckduckgo".to_string(),
            score,
            // The triage tests' fixture predates the t1g custody carry;
            // triage never reads the stamp.
            custody: String::new(),
        }
    }

    #[test]
    fn code_set_k_with_eps_quota() {
        let hits = vec![
            hit("h1", 0.9),
            hit("h2", 0.8),
            hit("h3", 0.7),
            hit("h4", 0.6),
        ];
        let r = triage_hits("run", "hash", 1, hits, 2, 0.5);
        // K = 2 → h1, h2; eps quota = ceil(2*0.5) = 1 → h3 admitted.
        assert_eq!(
            r.outcome.code_set_k,
            vec!["h1".to_string(), "h2".to_string()]
        );
        assert_eq!(r.outcome.eps_admits, vec!["h3".to_string()]);
        assert_eq!(r.ranked.len(), 3);
        assert_eq!(r.outcome.threshold, 0.8);
        // h4 skipped, ledger records it with a reason.
        assert_eq!(r.skip_ledger.entries.len(), 1);
        assert_eq!(r.skip_ledger.entries[0].url, "https://example.com/h4");
        assert_eq!(r.skip_ledger.entries[0].reason, "below-cut");
        assert_eq!(r.skip_ledger.entries[0].rank, 4);
    }

    /// The figure-specifier extractor reads the question's OWN text —
    /// its digit runs and its generic measure-family words — and adds
    /// nothing the question lacks. Shape, never the test: no bank
    /// vocabulary appears in the lexicon (the bank's NAMED measures —
    /// Gini, Case-Shiller, 80/20, white share — are not generic
    /// measure words and never enter this module).
    #[test]
    fn figure_specifiers_come_from_the_question_own_text() {
        let q = "How did income inequality and housing affordability evolve \
                 across US cities from 1980 to 2024?";
        assert_eq!(
            figure_specifiers(q),
            vec!["1980".to_string(), "2024".to_string(), "income".to_string()]
        );
        // A question with no figures implies none.
        assert!(figure_specifiers("What happened in American cities?").is_empty());
        // A question that carries measure words keeps them (the
        // question's own words — the lexicon never adds a measure the
        // question lacks).
        let s2 = figure_specifiers("What is the price-to-income ratio in California?");
        assert!(s2.iter().any(|s| s == "price"));
        assert!(s2.iter().any(|s| s == "income"));
        assert!(s2.iter().any(|s| s == "ratio"));
        // has_figure_specifier: a digit or a measure word.
        assert!(has_figure_specifier("What was the ratio in 1980?"));
        assert!(has_figure_specifier("How did the index evolve?"));
        assert!(!has_figure_specifier(
            "What were the primary drivers of the change?"
        ));
    }

    /// RED-first (order deep-research-t1e — R5 admission): the K-cut
    /// stops cutting figure-bearing hits — admission ranking favors
    /// them.
    ///
    /// The HEAD failure shape (measured in the t1d battery,
    /// dr-1786754967): every v1 hit scored 0.9 (the deck's default),
    /// so insertion order decided the code-set K, and the
    /// figure-bearing hits (wiki-income's Gini 0.485, brookings'
    /// 95/20 9.3) were cut at ranks 5 and 7 while same-scored
    /// figure-less ties were admitted. The fixed ranker breaks ties on
    /// figure-bearing-ness first: a hit whose title or snippet carries
    /// a figure token outranks a same-scored figure-less hit, so the
    /// K-cut cannot silently exclude the hits the figures live in.
    /// Watch-it-fail: on the pre-fix shape (score-only sort) the
    /// figure-bearing hit sits below the cut by insertion order and
    /// the admission assertion fails.
    #[test]
    fn triage_favors_figure_bearing_hits() {
        let figureless_hit = |id: &str| hit(id, 0.9);
        let figure_bearing_hit = |id: &str| SearchHit {
            snippet: "The Gini index reached 0.485 in 2018.".to_string(),
            ..hit(id, 0.9)
        };
        // All ties at 0.9 — the deck's default — with the figure-
        // bearing hit at insertion position 3, beyond K=2.
        let hits = vec![
            figureless_hit("h1"),
            figureless_hit("h2"),
            figure_bearing_hit("h3"),
            figureless_hit("h4"),
        ];
        let r = triage_hits("run", "hash", 1, hits, 2, 0.0);
        assert!(
            r.outcome.code_set_k.iter().any(|id| id == "h3"),
            "the figure-bearing hit must be admitted into the code-set K, \
             not cut by insertion order: {:?}",
            r.outcome.code_set_k
        );
        assert_eq!(
            r.outcome.admission_rule, ADMISSION_RULE_SCORE_THEN_FIGURE,
            "the outcome records the admission rule it ran"
        );
        // A lower-scored figure-bearing hit does NOT outrank a
        // higher-scored figure-less hit — the preference breaks ties,
        // it never overrides score.
        let hits = vec![
            figureless_hit("h1"), // 0.9, figure-less
            {
                let mut h = figure_bearing_hit("h2");
                h.score = 0.8;
                h
            },
        ];
        let r = triage_hits("run", "hash", 1, hits, 1, 0.0);
        assert_eq!(
            r.outcome.code_set_k,
            vec!["h1".to_string()],
            "score still decides first — the figure preference is a tie-break"
        );
    }

    /// RED (order deep-research-t1h, H1 — the corpus-leg triage
    /// boundary, pre-registered in adversarial/pre-registration.md):
    /// "a corpus hit whose BODY carries the figure-bearing digit but
    /// whose title does not is admitted by the triage ahead of
    /// figure-free hits". The corpus surface's titles are digit-free
    /// document names and its snippets are term-centered 600-char cuts
    /// (gym.rs estate_search) — the body is the only digit carrier,
    /// and inside LanceDB's quantized f32 top bucket the tie must not
    /// fall to insertion order. The t1g v1 flight's chunk 65 (the
    /// source-report chunk carrying Gini 0.5469) lost exactly this
    /// boundary at rank 6 (skip-ledger-1.json, below-cut).
    /// Watched red: fails at HEAD — figure_bearing reads title+snippet
    /// only, the body is invisible, insertion order decides.
    #[test]
    fn triage_admits_body_figure_over_figure_free_at_equal_score() {
        // The corpus-surface shape: digit-free title, term-centered
        // digit-free snippet cut, digit-bearing BODY.
        let mut body_figure = hit("c65", 0.03333333507180214);
        body_figure.snippet =
            "urban areas generate substantial wealth and attract educated".to_string();
        body_figure.content = Some(
            "Gini coefficients in the largest metro areas exceeded 0.5469 in 2019.".to_string(),
        );
        // The fully figure-free hit arrives FIRST — insertion order
        // must not decide inside the score tie.
        let figure_free = hit("c40", 0.03333333507180214);
        let r = triage_hits("run", "hash", 1, vec![figure_free, body_figure], 1, 0.0);
        assert_eq!(
            r.outcome.code_set_k,
            vec!["c65".to_string()],
            "the body-figure hit must win the tie over the figure-free hit"
        );
        assert_eq!(r.ranked[0].id, "c65");
    }

    #[test]
    fn no_hits_no_ledger() {
        let r = triage_hits("run", "hash", 1, vec![], 2, 0.5);
        assert!(r.ranked.is_empty());
        assert!(r.skip_ledger.entries.is_empty());
        assert_eq!(r.outcome.threshold, 0.0);
    }

    #[test]
    fn fewer_hits_than_k_takes_all() {
        let hits = vec![hit("h1", 0.5)];
        let r = triage_hits("run", "hash", 1, hits, 2, 0.5);
        assert_eq!(r.outcome.code_set_k, vec!["h1".to_string()]);
        assert!(r.skip_ledger.entries.is_empty());
    }

    /// RED-first (acquisition tune, 2026-08-24): a formed query that is
    /// not a query is never dispatched, and its refusal is RECORDED.
    ///
    /// THE MEASURED SHAPE. `actionable_query` is a string TEMPLATE over a
    /// claim sentence (audit.rs `query_for` → mod.rs `template_query`:
    /// strip citation spans, strip disallowed figures, take 140 chars).
    /// Nothing between the claim and the search backend asks whether the
    /// result is a query at all. On the logged t7a flight, task 90 round
    /// 2 dispatched `###` three times and the EMPTY STRING three times —
    /// six of that task's 28 queries (21% of its search allowance) spent
    /// on markdown scaffolding the draft happened to contain. Six of the
    /// flight's 132 queries overall.
    ///
    /// THE BAR IS DELIBERATELY LOW. Three content words. This gate
    /// refuses things that are not queries; it does NOT judge whether a
    /// query is a GOOD one — 49% of the flight's rounds-2+ gap queries
    /// are mangled draft prose that clears three words easily ("They
    /// equipped internal clocks allow them interpret changing patterns
    /// throughou"), and separating those from real queries needs
    /// judgment this function does not have and must not pretend to.
    /// Fixing that is a different change (a planner, not a template).
    ///
    /// REFUSED, NOT DROPPED (§18.3): the refusal rides the fetch list, so
    /// a query the loop declined to spend on is visible in the artifact
    /// rather than silently absent.
    /// The AIQ rule-3 port (acquisition tune, 2026-08-24): when the port
    /// reformulates the gaps, the round asks the MODEL-FORMED query and
    /// the artifact says so; when it does not, the round falls back to
    /// the deterministic template and the artifact says THAT.
    ///
    /// The recorded shape this replaces: `actionable_query` is a template
    /// over a claim sentence, so the gap rounds fired draft prose at a
    /// search engine ("They equipped internal clocks allow them interpret
    /// changing patterns throughou"). Round 1's queries were already
    /// model-formed via `plan_subquestions` and are the ones that
    /// retrieve well — this gives the gap rounds the same surface.
    ///
    /// A fallback is never silent (§18.3): `formed_by` and `provider`
    /// both carry which shape produced the query, so a run that quietly
    /// reverted to the template is visible in the fetch list.
    #[test]
    fn model_formed_gap_queries_are_used_and_the_fallback_is_recorded() {
        let gaps = vec![
            Gap {
                id: "g1".to_string(),
                text: "It was completed several years after the survey.".to_string(),
                actionable_query: "It was completed several years after the survey".to_string(),
                from_claim_id: None,
                corroboration: None,
            },
            Gap {
                id: "g2".to_string(),
                text: "The protocol handles capability discovery.".to_string(),
                actionable_query: "The protocol handles capability discovery".to_string(),
                from_claim_id: None,
                corroboration: None,
            },
        ];
        let reformed = vec![
            "Meridian Bridge completion date".to_string(),
            "Agent2Agent A2A protocol capability discovery mechanism".to_string(),
        ];

        let modelled = form_queries("run", "hash", 2, &gaps, &[], Some(&reformed));
        assert_eq!(
            modelled
                .queries
                .iter()
                .map(|q| q.text.as_str())
                .collect::<Vec<_>>(),
            vec![
                "Meridian Bridge completion date",
                "Agent2Agent A2A protocol capability discovery mechanism"
            ],
            "the round asks the reformulated query, not the template"
        );
        assert!(
            modelled
                .queries
                .iter()
                .all(|q| q.formed_by == "gap-model" && q.provider == "port"),
            "a model-formed query names its shape and its provider"
        );

        let templated = form_queries("run", "hash", 2, &gaps, &[], None);
        assert!(
            templated
                .queries
                .iter()
                .all(|q| q.formed_by == "gap-template" && q.provider == "deterministic"),
            "the fallback names ITSELF — a silent revert to the template \
             is what the record exists to prevent"
        );
        assert_eq!(
            templated.queries[0].text,
            "It was completed several years after the survey"
        );

        // A count mismatch is refused, not matched positionally: a query
        // aligned to the wrong gap is worse than the template.
        let short = vec!["only one line".to_string()];
        let mismatched = form_queries("run", "hash", 2, &gaps, &[], Some(&short));
        assert!(
            mismatched
                .queries
                .iter()
                .all(|q| q.formed_by == "gap-template"),
            "a short reformulation falls back rather than misaligning"
        );
    }

    #[test]
    fn a_query_that_is_not_a_query_is_refused_and_recorded() {
        let gaps = vec![
            Gap {
                id: "g1".to_string(),
                text: "heading".to_string(),
                actionable_query: "###".to_string(),
                from_claim_id: None,
                corroboration: None,
            },
            Gap {
                id: "g2".to_string(),
                text: "empty".to_string(),
                actionable_query: String::new(),
                from_claim_id: None,
                corroboration: None,
            },
            Gap {
                id: "g3".to_string(),
                text: "two words only".to_string(),
                actionable_query: "Meridian Bridge".to_string(),
                from_claim_id: None,
                corroboration: None,
            },
            Gap {
                id: "g4".to_string(),
                text: "a real one".to_string(),
                actionable_query: "Meridian Bridge completion date 1873".to_string(),
                from_claim_id: None,
                corroboration: None,
            },
        ];
        let fl = form_queries("run", "hash", 2, &gaps, &["**".to_string()], None);
        assert_eq!(
            fl.queries.len(),
            1,
            "only the real query is dispatchable, got {:?}",
            fl.queries.iter().map(|q| &q.text).collect::<Vec<_>>()
        );
        assert_eq!(fl.queries[0].text, "Meridian Bridge completion date 1873");
        let refused: Vec<&str> = fl.refused_queries.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(
            refused,
            vec!["###", "", "Meridian Bridge", "**"],
            "every refusal is recorded in the order it was formed"
        );
        assert!(
            fl.refused_queries.iter().all(|r| !r.reason.is_empty()),
            "a refusal names its reason"
        );
        // The surviving query keeps its identity: ids are assigned to
        // what is DISPATCHED, so the fetch list's query ids stay dense
        // and `SearchHit::query_id` still resolves.
        assert_eq!(fl.queries[0].id, "q1");
    }

    #[test]
    fn queries_come_from_gaps_deterministically() {
        let gaps = vec![Gap {
            id: "g1".to_string(),
            text: "The Meridian Bridge was completed in 1873.".to_string(),
            actionable_query: "Meridian Bridge completion date 1873".to_string(),
            from_claim_id: Some("c2".to_string()),
            corroboration: None,
        }];
        let fl = form_queries("run", "hash", 2, &gaps, &[], None);
        assert_eq!(fl.queries.len(), 1);
        // drb1-r2c: re-pinned to the committed form_queries (t6f rung 2)
        // — the query is the gap's actionable_query, the search-shaped
        // keyword form, not the declarative text. The sibling
        // gap_derived_queries_use_actionable_form pins the same shape.
        assert_eq!(fl.queries[0].text, "Meridian Bridge completion date 1873");
        assert_eq!(fl.queries[0].formed_by, "gap-template");
        assert_eq!(fl.queries[0].from_gap_id.as_deref(), Some("g1"));
    }

    /// t6f rung 2: gap-derived acquisition — round N+1's search queries
    /// use the actionable_query field (the search-shaped keyword form),
    /// not the declarative gap text. Micro-probe validated: gap.text sentence
    /// form doesn't match keyword tokens; actionable_query does.
    #[test]
    fn gap_derived_queries_use_actionable_form() {
        let gaps = vec![Gap {
            id: "g1".to_string(),
            text: "What year did the Meridian Bridge open?".to_string(),
            actionable_query: "Meridian Bridge completion year".to_string(),
            from_claim_id: Some("c2".to_string()),
            corroboration: None,
        }];
        let fl = form_queries("run", "hash", 2, &gaps, &[], None);
        assert_eq!(fl.queries.len(), 1);
        // Query uses actionable_query (search-shaped), not text (declarative)
        assert_eq!(fl.queries[0].text, "Meridian Bridge completion year");
        assert_eq!(fl.queries[0].formed_by, "gap-template");
        assert_eq!(fl.queries[0].from_gap_id.as_deref(), Some("g1"));
    }

    /// t1d fix 3 (second-origin): the floor's corroboration record
    /// rides the formed query into the fetch list — the artifact is
    /// self-describing (why this query: a capped claim's missing
    /// origin). Preplanned queries carry none.
    #[test]
    fn floor_record_rides_the_formed_query() {
        let record = crate::deep_research::icd::CorroborationRecord {
            origins: vec!["https://gym.example/one".to_string()],
            support_chunks: 1,
            floor: 2,
            passes_floor: false,
        };
        let gaps = vec![Gap {
            id: "g1".to_string(),
            text: "The Gini index rose to 0.55 by 2024.".to_string(),
            actionable_query: "0.55 Gini index rose 2024".to_string(),
            from_claim_id: Some("c1".to_string()),
            corroboration: Some(record.clone()),
        }];
        // Three content words minimum (`query_refusal`): the old fixture
        // here was the two-word "preplanned query", which the 2026-08-24
        // validity gate refuses. Widened, not weakened — what this test
        // pins is that the FLOOR RECORD rides the formed query, and a
        // realistic preplanned query pins it just as well. On the logged
        // t7a flight the bar refused nothing beyond the `###`/empty six,
        // so it costs no real query.
        let fl = form_queries(
            "run",
            "hash",
            2,
            &gaps,
            &["preplanned query about the index".to_string()],
            None,
        );
        assert_eq!(fl.queries.len(), 2);
        assert!(
            fl.refused_queries.is_empty(),
            "both fixtures clear the validity gate"
        );
        assert_eq!(fl.queries[0].corroboration, Some(record));
        assert_eq!(
            fl.queries[1].corroboration, None,
            "a preplanned query carries no floor record"
        );
        assert_eq!(fl.queries[1].formed_by, "plan-subquestion");
    }
}
