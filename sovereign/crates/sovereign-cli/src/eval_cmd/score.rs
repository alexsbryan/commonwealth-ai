//! Deterministic, glassbox scoring of retrieval results.
//!
//! Two scorers, both pure (no LLM, no embeddings). They're crude on
//! purpose: scoring is supposed to be readable so a developer can tell
//! at a glance which fact matched and which didn't. Embedding-based
//! and LLM-judge scorers can layer on later behind opt-in flags
//! without changing the call sites.
//!
//! Score conventions:
//!   - `matched / expected` is the headline number (0..1).
//!   - `missing` is preserved verbatim so the report can list exactly
//!     what slipped through.
//!   - A question with zero expected items in a dimension is treated
//!     as N/A (not 0/0 = NaN, not 1.0). The runner skips that dimension.

use corpus_engine::ScoredChunk;

/// Result of comparing a question's `expected_sources` against the
/// titles of chunks that came back from retrieval.
#[derive(Debug, Clone)]
pub struct SourceScore {
    pub matched: Vec<String>,
    pub missing: Vec<String>,
    pub total_expected: usize,
}

impl SourceScore {
    pub fn ratio(&self) -> Option<f32> {
        if self.total_expected == 0 {
            None
        } else {
            Some(self.matched.len() as f32 / self.total_expected as f32)
        }
    }
}

/// Match `expected_sources` against the titles of `retrieved` chunks.
/// Title comparison goes through `corpus_engine::filters::normalize_title`,
/// which lowercases and folds underscores/whitespace — so `"Albert
/// Einstein"`, `"albert_einstein"`, and `"Albert  Einstein"` all match
/// the same expected entry.
pub fn score_sources(expected: &[String], retrieved: &[ScoredChunk]) -> SourceScore {
    let retrieved_titles: Vec<String> = retrieved
        .iter()
        .filter_map(|c| c.title.as_deref())
        .map(corpus_engine::filters::normalize_title)
        .collect();

    let mut matched = Vec::new();
    let mut missing = Vec::new();
    for want in expected {
        let want_norm = corpus_engine::filters::normalize_title(want);
        if retrieved_titles.iter().any(|t| t == &want_norm) {
            matched.push(want.clone());
        } else {
            missing.push(want.clone());
        }
    }
    SourceScore {
        matched,
        missing,
        total_expected: expected.len(),
    }
}

/// Result of a fact-coverage check across the retrieved bag-of-text.
#[derive(Debug, Clone)]
pub struct FactScore {
    pub matched: Vec<String>,
    pub missing: Vec<String>,
    pub total_expected: usize,
}

impl FactScore {
    pub fn ratio(&self) -> Option<f32> {
        if self.total_expected == 0 {
            None
        } else {
            Some(self.matched.len() as f32 / self.total_expected as f32)
        }
    }
}

/// Crude fact-coverage scorer: a fact is "matched" if every space-
/// separated keyword token in it appears (case-insensitive substring)
/// somewhere in the concatenated retrieved-chunk text. Tokens shorter
/// than 3 chars are dropped — they're stopword-y and produce noise.
///
/// This is glassbox by construction. The bank author can read each
/// expected_fact, see which keywords it'll match on, and tighten or
/// loosen accordingly. Fancier scorers (embedding cosine, LLM judge)
/// can layer on later; this one returns a number you can defend.
pub fn score_facts(expected: &[String], retrieved: &[ScoredChunk]) -> FactScore {
    let haystack = retrieved
        .iter()
        .map(|c| c.content.as_str())
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();

    let mut matched = Vec::new();
    let mut missing = Vec::new();
    for fact in expected {
        let tokens = keyword_tokens(fact);
        if tokens.is_empty() {
            // No content tokens — treat as a no-op rather than a miss
            // (a zero-token fact is a bank bug, not a retrieval miss).
            continue;
        }
        if tokens.iter().all(|t| haystack.contains(t)) {
            matched.push(fact.clone());
        } else {
            missing.push(fact.clone());
        }
    }
    FactScore {
        matched,
        missing,
        total_expected: expected.len(),
    }
}

/// Same matching rule as [`score_facts`] but against an arbitrary
/// haystack — used by the `--synth` path to score expected_facts
/// against the model's synthesised answer rather than the bag of
/// retrieved chunks. Keeping the rule identical means a fact that
/// would have scored against the chunks scores the same way against
/// the answer; the *only* thing that changes is the haystack.
pub fn score_facts_in_text(expected: &[String], text: &str) -> FactScore {
    let haystack = text.to_lowercase();
    let mut matched = Vec::new();
    let mut missing = Vec::new();
    for fact in expected {
        let tokens = keyword_tokens(fact);
        if tokens.is_empty() {
            continue;
        }
        if tokens.iter().all(|t| haystack.contains(t)) {
            matched.push(fact.clone());
        } else {
            missing.push(fact.clone());
        }
    }
    FactScore {
        matched,
        missing,
        total_expected: expected.len(),
    }
}

/// Source-match against pre-extracted titles. Used by the synth path,
/// which only has the metadata `retrieved_chunks` array (titles, no
/// `ScoredChunk`s) to work with. Normalisation matches `score_sources`.
pub fn score_sources_titles<S: AsRef<str>>(expected: &[String], titles: &[S]) -> SourceScore {
    let normalized: Vec<String> = titles
        .iter()
        .map(|t| corpus_engine::filters::normalize_title(t.as_ref()))
        .collect();

    let mut matched = Vec::new();
    let mut missing = Vec::new();
    for want in expected {
        let want_norm = corpus_engine::filters::normalize_title(want);
        if normalized.iter().any(|t| t == &want_norm) {
            matched.push(want.clone());
        } else {
            missing.push(want.clone());
        }
    }
    SourceScore {
        matched,
        missing,
        total_expected: expected.len(),
    }
}

fn keyword_tokens(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 3)
        .map(|t| t.to_lowercase())
        .collect()
}

// ─── Instructor-mode (LLM-as-judge) fact scorer ────────────────

/// One audit record per (expected_fact, judge call). Lets the report
/// show *why* the judge said yes or no without re-running the bench.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct JudgeFactDetail {
    pub fact: String,
    pub present: bool,
    /// Short verbatim quote from the answer, or `"(absent)"` if the
    /// judge said the concept isn't there. `(parse failed)` when the
    /// daemon returned malformed JSON (rare with the in-house grammar
    /// enforcer; logged separately to stderr).
    pub evidence: String,
}

/// LLM-judge fact scorer — "instructor mode."
///
/// Scores each `expected_fact` against `answer` by asking a fast-slot
/// model whether the concept is conveyed, regardless of whether the
/// answer uses the bank's exact wording. Pairs with [`score_facts_in_text`]
/// (the strict keyword-AND scorer) so the report can show both:
/// strict catches verbatim coverage, judge catches paraphrase.
///
/// Returns the boolean rollup as a [`FactScore`] AND a
/// per-fact `Vec<JudgeFactDetail>` carrying the judge's evidence
/// quote. The detail list is what makes the score auditable — a
/// reviewer reading the run JSON can verify each yes/no without
/// re-running.
///
/// Each fact gets one structured-output call constrained by the
/// in-house JSON enforcer (see `sovereign-inference::json_constraint`).
/// Schema: `{present: "yes" | "no", evidence: <quote>}`.
///
/// Prompt is generic — describes the *shape* of the judgment, never
/// the specific bank. See `feedback_no_teaching_to_test.md`.
pub async fn score_facts_judge(
    expected: &[String],
    answer: &str,
    inference: &dyn sovereign_core::traits::InferenceProvider,
) -> (FactScore, Vec<JudgeFactDetail>) {
    let mut matched = Vec::new();
    let mut missing = Vec::new();
    let mut details: Vec<JudgeFactDetail> = Vec::with_capacity(expected.len());

    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "present": {"type": "string", "enum": ["yes", "no"]},
            "evidence": {"type": "string"},
        },
        "required": ["present", "evidence"],
    });

    for fact in expected {
        // The prompt is intentionally generous. Earlier we observed
        // gemma-4-E4B reading "concept conveyed" too literally on
        // sub-points (e.g. marking "American Revolution" absent from a
        // French-Revolution answer that named it as a financial-cause
        // factor). The framing below makes it explicit: the answer is
        // a multi-point response and the concept is *one of many
        // relevant items*; mention-in-context counts.
        let prompt = format!(
            "You are evaluating an answer against a list of relevant concepts, \
             one at a time. The answer responds to a broader question, so a \
             concept counts as present if it appears anywhere in the answer in a \
             way that bears on the topic — direct mention, paraphrase, contextual \
             inference, or as one item in a larger list. Mark \"yes\" if a \
             reasonable instructor would credit a student for surfacing the concept. \
             Mark \"no\" only if the concept is genuinely absent from the answer, \
             or mentioned only in an unrelated way.\n\n\
             Quote a short verbatim span from the answer as evidence (≤30 words), \
             or write \"(absent)\" if the concept is not there.\n\n\
             Concept: {fact}\n\n\
             Answer:\n{answer}\n\n\
             Respond with JSON only."
        );

        let request = sovereign_core::types::CompletionRequest {
            prompt,
            system_message: Some(
                "You evaluate whether answers convey concepts. Be generous: \
                 mention-in-context counts. Respond with JSON only."
                    .to_string(),
            ),
            preferred_speed: sovereign_core::types::Speed::Fast,
            max_tokens: Some(200),
            temperature: Some(0.0),
            structured_output: Some(schema.clone()),
            think_budget: Some(0),
            top_k: None,
            top_p: None,
            oicp: None,
            tools: None,
            tool_choice: None,
            model_id: None,
            enable_thinking: None,
        };

        match inference.complete(&request).await {
            Ok(resp) => match parse_judge(&resp.text) {
                Some((present, evidence)) => {
                    if present {
                        matched.push(fact.clone());
                    } else {
                        missing.push(fact.clone());
                    }
                    details.push(JudgeFactDetail {
                        fact: fact.clone(),
                        present,
                        evidence,
                    });
                }
                None => {
                    eprintln!(
                        "  [judge] parse failed for fact={fact:?} raw={raw:?}",
                        raw = &resp.text[..resp.text.len().min(120)]
                    );
                    missing.push(fact.clone());
                    details.push(JudgeFactDetail {
                        fact: fact.clone(),
                        present: false,
                        evidence: "(parse failed)".into(),
                    });
                }
            },
            Err(e) => {
                eprintln!("  [judge] inference failed for fact={fact:?}: {e}");
                missing.push(fact.clone());
                details.push(JudgeFactDetail {
                    fact: fact.clone(),
                    present: false,
                    evidence: format!("(inference failed: {e})"),
                });
            }
        }
    }

    let score = FactScore {
        matched,
        missing,
        total_expected: expected.len(),
    };
    (score, details)
}

/// Pull `(present, evidence)` out of the judge's JSON response. The
/// in-house constraint guarantees a valid JSON object with the right
/// shape, so the only failure mode is a daemon-side fallthrough (rare
/// post-resolution of the alpha-blocker). Returns `None` on parse
/// failure so the caller can log + treat as a miss.
fn parse_judge(raw: &str) -> Option<(bool, String)> {
    let trimmed = raw
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let v: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let present = v.get("present")?.as_str()?;
    let evidence = v
        .get("evidence")
        .and_then(|e| e.as_str())
        .unwrap_or("")
        .to_string();
    match present.to_ascii_lowercase().as_str() {
        "yes" => Some((true, evidence)),
        "no" => Some((false, evidence)),
        _ => None,
    }
}

// ─── Loose source-credit judge (Option A) ──────────────────────

/// One audit record per missing expected_source the loose judge
/// considered. `loose_match=true` means the judge decided the topic
/// is materially covered by at least one retrieved chunk even though
/// no chunk's title matched the slug literally. Pairs with the
/// rigid [`SourceScore`] so a reviewer can see both numbers and
/// audit each loose-credit decision.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct JudgeSourceDetail {
    pub source: String,
    pub loose_match: bool,
    /// Short rationale from the judge (≤30 words).
    pub evidence: String,
}

/// Loose-judge source scorer — Option A in the SEP atlas-grounding
/// post-mortem. The rigid [`score_sources`] requires retrieved-chunk
/// titles to match `expected_sources` slugs exactly (after
/// `normalize_title` folding). That breaks when an `extraction_first`
/// atlas surfaces semantically-relevant content under a different
/// title (e.g. atlas entity `Knowledge Argument` covers the
/// `qualia-knowledge` topic; `Frank Jackson` is one of the canonical
/// authors, etc.). This function only judges the **missing**
/// expected_sources from the rigid pass — already-matched ones stay
/// credited verbatim — so the work is bounded and the loose score
/// is a strict superset of the rigid score.
///
/// One LLM call per question (multi-label classification over the
/// missing slug set) keeps cost bounded: 21-question bank ≈ 21 calls
/// ≈ <1 minute on Darwin-36B fast slot.
///
/// Schema:
///   `{"loose_credit": ["slug1", "slug2"], "rationale": "..."}`
///
/// Returns the loose [`SourceScore`] (rigid_matched ∪ loose_matched
/// vs total_expected) plus per-source audit details. Caller is
/// responsible for keeping the rigid score available alongside —
/// the loose pass is additive, not a replacement.
pub async fn score_sources_loose(
    question: &str,
    rigid: &SourceScore,
    retrieved: &[ScoredChunk],
    inference: &dyn sovereign_core::traits::InferenceProvider,
) -> (SourceScore, Vec<JudgeSourceDetail>) {
    // Nothing to judge — every expected_source already matched.
    if rigid.missing.is_empty() {
        let details = rigid
            .matched
            .iter()
            .map(|m| JudgeSourceDetail {
                source: m.clone(),
                loose_match: true,
                evidence: "(rigid match)".into(),
            })
            .collect();
        return (rigid.clone(), details);
    }

    // Build the chunk excerpt list. Cap each snippet to ~400 chars
    // so the prompt stays under ~5k tokens even with a large top-K.
    let chunks_block: String = retrieved
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let title = c.title.as_deref().unwrap_or("(untitled)");
            let snippet = truncate(&c.content.replace('\n', " "), 400);
            format!("  [{}] {title} — {snippet}", i + 1)
        })
        .collect::<Vec<_>>()
        .join("\n");

    let missing_block: String = rigid
        .missing
        .iter()
        .map(|s| format!("  - {s}"))
        .collect::<Vec<_>>()
        .join("\n");

    // The prompt is intentionally generic. The bank's
    // expected_sources are SEP slugs — which the model knows about
    // from pretraining well enough to judge topical coverage. We
    // describe the *shape* of the judgment (paraphrase / canonical
    // siblings / indirect coverage all count) without naming any
    // specific bank — see `feedback_no_teaching_to_test.md`.
    let prompt = format!(
        "You are evaluating whether a question's expected source articles are \
         materially covered by a list of retrieved passages — even when the passage \
         titles don't match the article slugs literally. A source counts as covered \
         if a reasonable instructor would say the retrieved passages contain \
         substantive content about that source's topic — direct mention, paraphrase, \
         a canonical sub-topic of it, or named figures / arguments / concepts that \
         are central to it.\n\n\
         Be generous on topical relevance, strict on substance: don't credit a \
         source if the passages only mention it in passing or as one item in a long \
         list of unrelated references.\n\n\
         Question:\n{question}\n\n\
         Sources whose exact titles did NOT appear in retrieval — judge each:\n{missing_block}\n\n\
         Retrieved passages (numbered):\n{chunks_block}\n\n\
         Reply with a JSON object listing only the sources that ARE materially \
         covered, plus a short rationale. Example: \
         {{\"loose_credit\": [\"slug-a\", \"slug-c\"], \"rationale\": \"...\"}}\n\n\
         Respond with JSON only."
    );

    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "loose_credit": {
                "type": "array",
                "items": {"type": "string"},
                "maxItems": 50
            },
            "rationale": {"type": "string", "maxLength": 800}
        },
        "required": ["loose_credit", "rationale"],
    });

    let request = sovereign_core::types::CompletionRequest {
        prompt,
        system_message: Some(
            "You evaluate whether source articles are topically covered by retrieved \
             passages. Be generous on paraphrase / canonical-sibling matches, strict \
             on substance. Respond with JSON only."
                .to_string(),
        ),
        preferred_speed: sovereign_core::types::Speed::Fast,
        max_tokens: Some(800),
        temperature: Some(0.0),
        structured_output: Some(schema),
        think_budget: Some(0),
        top_k: None,
        top_p: None,
        oicp: None,
        tools: None,
        tool_choice: None,
        model_id: None,
        enable_thinking: None,
    };

    let mut all_matched = rigid.matched.clone();
    let mut details: Vec<JudgeSourceDetail> = rigid
        .matched
        .iter()
        .map(|m| JudgeSourceDetail {
            source: m.clone(),
            loose_match: true,
            evidence: "(rigid match)".into(),
        })
        .collect();

    match inference.complete(&request).await {
        Ok(resp) => match parse_loose_credit(&resp.text) {
            Some((credited, rationale)) => {
                let credit_set: std::collections::HashSet<String> = credited
                    .iter()
                    .map(|s| s.to_lowercase())
                    .collect();
                let mut new_missing = Vec::new();
                for src in &rigid.missing {
                    let loose = credit_set.contains(&src.to_lowercase());
                    if loose {
                        all_matched.push(src.clone());
                    } else {
                        new_missing.push(src.clone());
                    }
                    details.push(JudgeSourceDetail {
                        source: src.clone(),
                        loose_match: loose,
                        evidence: rationale.clone(),
                    });
                }
                let loose_score = SourceScore {
                    matched: all_matched,
                    missing: new_missing,
                    total_expected: rigid.total_expected,
                };
                (loose_score, details)
            }
            None => {
                eprintln!(
                    "  [loose-judge] parse failed; raw={raw:?}",
                    raw = &resp.text[..resp.text.len().min(180)]
                );
                for src in &rigid.missing {
                    details.push(JudgeSourceDetail {
                        source: src.clone(),
                        loose_match: false,
                        evidence: "(parse failed)".into(),
                    });
                }
                (rigid.clone(), details)
            }
        },
        Err(e) => {
            eprintln!("  [loose-judge] inference failed: {e}");
            for src in &rigid.missing {
                details.push(JudgeSourceDetail {
                    source: src.clone(),
                    loose_match: false,
                    evidence: format!("(inference failed: {e})"),
                });
            }
            (rigid.clone(), details)
        }
    }
}

fn parse_loose_credit(raw: &str) -> Option<(Vec<String>, String)> {
    let trimmed = raw
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let v: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let credited: Vec<String> = v
        .get("loose_credit")?
        .as_array()?
        .iter()
        .filter_map(|s| s.as_str().map(str::to_string))
        .collect();
    let rationale = v
        .get("rationale")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    Some((credited, rationale))
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n).collect();
        t.push('…');
        t
    }
}

// ─── Essay-readiness judge (Option C) ──────────────────────────

/// Multi-axis 0–3 evaluation of whether the retrieved chunks
/// constitute enough material for a sophisticated essay answering
/// the question. Mirrors voice-bench's
/// [`sovereign_core::pipeline::judge::JudgeScore`] shape: per-axis
/// integers + a free-text rationale.
///
/// The four axes capture distinct kinds of substance an essay needs:
/// - **topical_coverage**: breadth across the question's territory.
/// - **position_attribution**: are the named thinkers / arguments /
///   positions actually represented (not just topic-adjacent)?
/// - **dialectical_breadth**: are multiple competing perspectives
///   present, or only one side?
/// - **argument_depth**: specific reasoning detail vs surface-level gloss.
///
/// Total 0–12. Different question categories naturally weight axes
/// differently (e.g. `contested` cares more about dialectical_breadth;
/// `argument_reconstruction` cares more about position_attribution +
/// argument_depth) — the eval keeps all four scores so consumers can
/// re-weight downstream.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EssayReadinessScore {
    pub topical_coverage: u8,
    pub position_attribution: u8,
    pub dialectical_breadth: u8,
    pub argument_depth: u8,
    /// Total = sum of the four axes. 0–12.
    pub total: u8,
    /// Short rationale (≤120 words) explaining the per-axis calls.
    pub rationale: String,
}

impl EssayReadinessScore {
    pub fn ratio(&self) -> f32 {
        self.total as f32 / 12.0
    }
}

/// LLM-judge essay-readiness scorer — Option C. Where
/// [`score_sources_loose`] answers "are the right articles in the
/// bag?" (multiple-choice recall), this answers "does the bag have
/// what an undergraduate philosophy student would need to write a
/// defensible nuanced essay?" (substance).
///
/// One LLM call per question (multi-axis structured output). At
/// ~2–3s per call on a fast 30B-class slot, a 21-question bank costs
/// under a minute. The output is intentionally axis-decomposed so
/// consumers can read where the retrieval is weak (e.g. high topical
/// coverage but low dialectical_breadth = "found the topic, missed
/// the debate"); a single scalar would hide that.
pub async fn score_essay_readiness(
    question: &str,
    category: &str,
    retrieved: &[ScoredChunk],
    // `atlas_navigation` is captured upstream for diagnostic /
    // audit purposes (the JSON output preserves it so a reviewer can
    // see which atlas surfaces matched the question). Deliberately
    // NOT injected into the judge prompt: a small model adding a
    // "navigation, not evidence" section to a 5–7k-char prompt pays a
    // distraction tax that exceeds atlas's contribution at this scope.
    // Atlas's job is to route attention to the right passages — it
    // does that via retrieval reordering (see runner.rs), not by
    // appearing as parallel content here.
    _atlas_navigation: &[ScoredChunk],
    inference: &dyn sovereign_core::traits::InferenceProvider,
) -> Option<EssayReadinessScore> {
    if retrieved.is_empty() {
        return Some(EssayReadinessScore {
            topical_coverage: 0,
            position_attribution: 0,
            dialectical_breadth: 0,
            argument_depth: 0,
            total: 0,
            rationale: "no retrieved chunks".into(),
        });
    }

    // Cap each snippet to ~500 chars; total prompt budget for a
    // top-10 retrieval set lands around 6k chars + question + rubric,
    // well under the fast slot's prompt budget.
    let chunks_block: String = retrieved
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let title = c.title.as_deref().unwrap_or("(untitled)");
            let snippet = truncate(&c.content.replace('\n', " "), 500);
            format!("  [{}] {title} — {snippet}", i + 1)
        })
        .collect::<Vec<_>>()
        .join("\n");

    // The rubric is generic — describes the kind of judgment, never
    // names a specific bank or expected answer. See
    // `feedback_no_teaching_to_test.md`. Category is passed as a hint
    // (`contested` should weight dialectical_breadth; `argument_reconstruction`
    // should weight position_attribution etc.) but the judge owns the
    // final per-axis call.
    let prompt = format!(
        "You are evaluating whether a set of retrieved passages contains enough \
         RAW MATERIAL for an undergraduate philosophy student to write a defensible \
         nuanced essay answering the question. The passages are inputs to writing, \
         not the essay itself — the student will synthesize across passages, restate \
         arguments in their own words, and connect quotes. Do not penalise the set \
         for failing to pre-assemble the essay; only penalise when the raw material \
         is missing or wrong.\n\n\
         Some passages begin with `[Atlas highlights]` followed by structured \
         analytic markers extracted from the article. Treat them as substantive \
         evidence, not metadata, when scoring the axes:\n\
         - `Argument: NAME [from <article>]` followed by `P1. … P2. … C. …` is a \
           named-argument reconstruction. The premise-conclusion structure IS \
           reconstruction-grade content for argument_depth (a student can cite the \
           premises directly), and IS substantive content for position_attribution \
           when the question names that argument.\n\
           Indented `Objections:` lines under an Argument are counter-positions: \
           each `- NAME: CONTENT` is a distinct objection with one-sentence \
           substance. Multiple such lines = strong dialectical_breadth raw \
           material; even bare `- NAME` lines count as registered counter-position \
           pointers (1-2 on dialectical_breadth).\n\
         - `[ATTRIBUTION (article) — contested]: \"...\"` is a Claim flagged as \
           epistemically contested in the corpus — its presence signals that this \
           position is itself counter-positioned elsewhere. Counts toward \
           dialectical_breadth.\n\
         - `Defining X (article): \"...\"` and `[X (article)]: \"...\"` are \
           verbatim defining-quotes / quotable excerpts. Count as substantive \
           content for position_attribution and argument_depth when the named \
           term/figure is question-relevant.\n\n\
         Score four axes from 0 (worst) to 3 (best):\n\n\
         1. topical_coverage — Do the passages span the topic the question raises, \
         not just one corner of it? 0 = off-topic, 3 = thorough breadth. Coverage \
         from multiple distinct articles in `[Atlas highlights]` blocks counts \
         toward breadth.\n\n\
         2. position_attribution — Are the named thinkers, arguments, positions, or \
         technical terms in the question REPRESENTED in the passages with enough \
         content for an undergraduate to write about them? 0 = absent or wrong; \
         1 = mentioned only by name; 2 = named with substantive content (key \
         claims/concepts attributed to them); 3 = named with substantive content \
         AND specific textual material (quotes, premises, examples) that an \
         undergraduate could cite. Award 3 when raw-material adequacy is achieved \
         even if the passages do not pre-assemble the argument step-by-step — \
         that's the student's job. An `Argument: NAME ... P1./P2./C.` block whose \
         NAME the question asks about is direct evidence for 3.\n\n\
         3. dialectical_breadth — Are multiple competing perspectives, objections, \
         or counter-positions present? An essay on a contested question needs more \
         than one side. 0 = one-sided; 1 = main position + brief mention of an \
         objection; 2 = main position + at least one substantive counter-position; \
         3 = multiple rival positions each with substantive content. \"Substantive\" \
         means content suitable for the student to engage with, not pre-written \
         dialectic. Indented `Objections: - NAME: CONTENT` lines are exactly this \
         kind of substantive counter-position; 1 such line ≈ 2 on dialectical, \
         2+ such lines or a mix with `[... — contested]` claims ≈ 3.\n\n\
         4. argument_depth — Do the passages contain enough specific reasoning \
         detail (premises, distinctions, examples, technical vocabulary) for an \
         undergraduate to reconstruct the argument? 0 = surface gloss only; \
         1 = some specific content but missing key pieces; 2 = solid raw material \
         (key concepts, core moves, examples present); 3 = rich detail (multiple \
         premises, technical vocabulary, examples — the student has everything \
         needed to write the reconstruction). An `Argument: NAME ... P1./P2./C.` \
         block whose NAME matches the question is direct 3-grade evidence; do NOT \
         require additional pre-format or extra paragraphs.\n\n\
         Question category: {category}\n\
         Question: {question}\n\n\
         Retrieved passages:\n{chunks_block}\n\n\
         Reply with a JSON object: \
         {{\"topical_coverage\": int 0-3, \"position_attribution\": int 0-3, \
         \"dialectical_breadth\": int 0-3, \"argument_depth\": int 0-3, \
         \"rationale\": \"≤120 word per-axis explanation\"}}\n\n\
         Respond with JSON only."
    );

    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "topical_coverage":     {"type": "integer", "minimum": 0, "maximum": 3},
            "position_attribution": {"type": "integer", "minimum": 0, "maximum": 3},
            "dialectical_breadth":  {"type": "integer", "minimum": 0, "maximum": 3},
            "argument_depth":       {"type": "integer", "minimum": 0, "maximum": 3},
            "rationale":            {"type": "string", "maxLength": 1200},
        },
        "required": ["topical_coverage", "position_attribution",
                     "dialectical_breadth", "argument_depth", "rationale"],
    });

    // Allow ablation experiments to swap the judge model via an env
    // var without plumbing a new CLI flag through every entry point.
    // When `SOVEREIGN_JUDGE_MODEL` is set, the request targets that
    // exact model id (and downgrades the speed gate to Slow so the
    // dispatcher doesn't second-guess our choice). Empty string or
    // unset = default Fast-slot model.
    let judge_model_override = std::env::var("SOVEREIGN_JUDGE_MODEL")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let preferred_speed = if judge_model_override.is_some() {
        sovereign_core::types::Speed::Slow
    } else {
        sovereign_core::types::Speed::Fast
    };
    let request = sovereign_core::types::CompletionRequest {
        prompt,
        system_message: Some(
            "You evaluate retrieval-set sufficiency for essay writing across four \
             axes. Be calibrated: 3 means genuinely strong, 2 is solid, 1 is thin, \
             0 is absent or wrong. Respond with JSON only."
                .to_string(),
        ),
        preferred_speed,
        max_tokens: Some(1200),
        temperature: Some(0.0),
        structured_output: Some(schema),
        think_budget: Some(0),
        top_k: None,
        top_p: None,
        oicp: None,
        tools: None,
        tool_choice: None,
        model_id: judge_model_override,
        enable_thinking: None,
    };

    match inference.complete(&request).await {
        Ok(resp) => parse_essay_readiness(&resp.text).or_else(|| {
            eprintln!(
                "  [essay-judge] parse failed; raw={raw:?}",
                raw = &resp.text[..resp.text.len().min(180)]
            );
            None
        }),
        Err(e) => {
            eprintln!("  [essay-judge] inference failed: {e}");
            None
        }
    }
}

fn parse_essay_readiness(raw: &str) -> Option<EssayReadinessScore> {
    let trimmed = raw
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let v: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let topical_coverage = v.get("topical_coverage")?.as_u64()? as u8;
    let position_attribution = v.get("position_attribution")?.as_u64()? as u8;
    let dialectical_breadth = v.get("dialectical_breadth")?.as_u64()? as u8;
    let argument_depth = v.get("argument_depth")?.as_u64()? as u8;
    let rationale = v
        .get("rationale")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    let total = topical_coverage
        .saturating_add(position_attribution)
        .saturating_add(dialectical_breadth)
        .saturating_add(argument_depth);
    Some(EssayReadinessScore {
        topical_coverage,
        position_attribution,
        dialectical_breadth,
        argument_depth,
        total,
        rationale,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn chunk(title: &str, content: &str) -> ScoredChunk {
        ScoredChunk {
            content: content.into(),
            title: Some(title.into()),
            url: None,
            corpus_id: "wikipedia".into(),
            score: 1.0,
            metadata: HashMap::new(),
            chunk_id: None,
            source_doc_id: None,
            vector_distance: None,
        }
    }

    #[test]
    fn source_match_normalizes_titles() {
        let retrieved = vec![chunk("Albert_Einstein", "...")];
        let s = score_sources(&["Albert Einstein".into()], &retrieved);
        assert_eq!(s.matched, vec!["Albert Einstein".to_string()]);
        assert!(s.missing.is_empty());
    }

    #[test]
    fn source_match_reports_missing() {
        let retrieved = vec![chunk("Niels Bohr", "...")];
        let s = score_sources(
            &["Albert Einstein".into(), "Niels Bohr".into()],
            &retrieved,
        );
        assert_eq!(s.matched, vec!["Niels Bohr".to_string()]);
        assert_eq!(s.missing, vec!["Albert Einstein".to_string()]);
    }

    #[test]
    fn fact_match_requires_all_tokens() {
        let retrieved = vec![chunk("Einstein", "Einstein discovered photoelectric effect in 1905.")];
        let s = score_facts(&["photoelectric effect".into(), "Brownian motion".into()], &retrieved);
        assert_eq!(s.matched, vec!["photoelectric effect".to_string()]);
        assert_eq!(s.missing, vec!["Brownian motion".to_string()]);
    }

    #[test]
    fn fact_match_is_case_insensitive() {
        let retrieved = vec![chunk("X", "PHOTOELECTRIC EFFECT happened.")];
        let s = score_facts(&["photoelectric effect".into()], &retrieved);
        assert_eq!(s.matched.len(), 1);
    }

    #[test]
    fn empty_expected_yields_none_ratio() {
        let s = score_facts(&[], &[]);
        assert!(s.ratio().is_none());
    }

    #[test]
    fn keyword_tokens_drops_short_words() {
        let toks = keyword_tokens("a brief fact about Newton");
        assert_eq!(toks, vec!["brief", "fact", "about", "newton"]);
    }
}
