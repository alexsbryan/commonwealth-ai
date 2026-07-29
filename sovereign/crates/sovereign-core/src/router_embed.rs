// SPDX-License-Identifier: AGPL-3.0-or-later
//! Embedding-based intent classifier — k-NN over a hand-authored
//! exemplar TOML, k=1 (max-similarity per intent).
//!
//! Replaces the string-match heuristic pre-checks in `router::LlmRouter`
//! for cases where the embedding has high margin. Falls through to
//! the existing heuristic + LLM cascade when ambiguous.
//!
//! ## Why this exists
//!
//! Pre-2026-05-15 the router had a stack of `looks_like_*` heuristics
//! (`looks_like_metalingual`, `looks_like_conation`, etc.) doing
//! substring matching on the user message and FORCING an intent
//! before the LLM classifier ever saw the message. The heuristics
//! were brittle — "where do" in `DEFINITIONAL_VERBS` matched "where
//! does the deepest rent sit?", routing a factual lookup to
//! MetalingualQuery and producing an empty answer.
//!
//! Semantic match beats string match on every axis except cost.
//! Embedding is ~50ms (one batch call to the local embed slot) vs
//! ~500-2000ms for the small-LLM classifier — fast enough to run
//! before either heuristic OR LLM.
//!
//! ## Iteration loop
//!
//! Exemplars live in a TOML file (path from `$SOVEREIGN_ROUTER_EXEMPLARS`
//! env var, or the default `sovereign/router/exemplars.toml` relative
//! to the cwd). Add a misroute to the TOML → next process picks it
//! up. No rebuild required.
//!
//! ## Confidence + margin gate
//!
//! Returns an intent only when:
//! - top similarity > `MIN_TOP_SIM` (default 0.55 — exemplar must
//!   actually match), AND
//! - margin between top and second intent > `MIN_MARGIN` (default
//!   0.10 — top must be decisively ahead).
//!
//! Ambiguous queries (low margin or low top) fall through. The LLM
//! classifier handles those with full-sentence context.
//!
//! The margin floor was 0.04 when the exemplar bank held only the
//! original taxonomy. Adding the specialized product intents
//! (`code_query`, `generative_query`) densified the embedding space:
//! their topical anchors (e.g. "retrieval pipeline") now sit close to
//! general queries that merely share a domain word, producing
//! "decisive"-looking k=1 wins between two WRONG intents. Raising the
//! floor to 0.10 makes those low-separation cases defer to the LLM,
//! which reads the full sentence and disambiguates correctly. The
//! value sits in an empirical gap measured across the routing banks:
//! the only correct embed decisions below 0.10 have margins <= 0.061,
//! and the lowest-margin correct decision retained is 0.110 — so a
//! 0.10 floor sheds the ambiguous collision (margin 0.099) plus two
//! barely-decisive cases (which the LLM re-confirms) without touching
//! any decision that was clearly separated.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::router_axis::{dot, normalize, AxisGate, AxisScore};
use crate::traits::InferenceProvider;
use crate::types::Intent;

/// Locator gate — floor on the best similarity to a tagged exemplar.
///
/// Deliberately BELOW the intent floor (0.55). The locator axis is
/// one-vs-rest, so its discriminating quantity is the margin over the
/// rest of the bank, not the absolute similarity; the floor only sheds
/// queries too far from anything to be trusted.
const DEFAULT_LOCATOR_MIN_SIM: f32 = 0.50;

/// Locator gate — margin the tagged group must hold over the best
/// UNTAGGED exemplar.
///
/// Calibrated 2026-07-26 against Qwen3-Embedding-0.6B over the whole
/// exemplar bank as the negative set (8 held-out positives, 14 held-out
/// negatives, query-instruction prefix applied). Every negative scored a
/// margin ≤ -0.030 — the closest being "Search the web for today's
/// launch coverage" — so 0.02 keeps a ~0.05 cushion. It is the
/// asymmetry that sets this value: a false positive HARD-COMMITS a
/// world question to conversation-only answering, while a false
/// negative merely falls through to the existing cascade. Positives
/// that abstain here ("what was my second question?", which sits
/// nearer the conation exemplar "Elaborate on the second point") lose
/// nothing they had before.
const DEFAULT_LOCATOR_MIN_MARGIN: f32 = 0.02;

// THIS FLOOR IS INERT. DO NOT TUNE IT — the margin below is the live term.
//
// Measured 2026-07-29 on the 40-case calibration bank: EVERY point on the
// axis's Pareto frontier is achieved at a floor of 0.000. The floor screens
// nothing the margin does not already screen, at any operating point. Two
// consequences, both counter-intuitive enough to be worth stating:
//
//   * Lowering it buys real coverage and costs nothing — (0.45, 0.100) scores
//     9 correct fires against the SAME hard-error count as (0.55, 0.100)'s 5.
//   * Raising the margin makes that moot: at the value shipped below, all
//     three firing cases sit at sim ≥ 0.571, so this floor excludes nothing.
//     It is left at 0.55 because moving an inert constant is pure risk.
//
// DO NOT take the gate `fit` reports as "best" (it has offered (0.356, 0.123)
// since the code_query re-filing). Separation collapses 0.018 → 0.007, 0.356
// sits at the encoder noise floor, and it is demonstrably overfit — the fitted
// floor slid 0.363 → 0.356 when the bank grew by three cases. More
// fundamentally, `fit`'s safe-recall objective refuses ANY mislabel while the
// shipped gate already carries one, so it judges candidates against a stricter
// standard than the status quo and cannot surface a gate that ties shipped
// errors while raising coverage. `--max-false-positives` does not relax that;
// it governs abstain-fires only.
const DEFAULT_MIN_TOP_SIM: f32 = 0.55;

// Raised 0.10 → 0.206 on 2026-07-29. This is a PRECISION repair, and it costs
// coverage on purpose.
//
// WHY. Growing the bank 27 → 40 revealed the shipped gate was committing wrong
// about as often as it committed right: 5 correct fires against 5 hard errors,
// **50% precision**. The old bank could not see this because ALL SEVEN of its
// abstain cases were 2-6 word ellipticals ("go on", "tell me more") — too
// short to reach the sim band where hard commits happen. Adding four
// content-rich but genuinely under-determined abstain cases turned up two
// false positives that the shipped gate had been making all along.
//
// This axis documents its own asymmetry: a false positive HARD-COMMITS the
// turn down a narrowed path, while a false negative merely falls through to
// the LLM cascade ~1.2s slower. At 50% precision that asymmetry was being paid
// in the expensive direction. 0.206 buys 100% precision (3 fires, 0 hard
// errors) for two correct fires — five hard commits removed at the cost of two
// extra cascade calls.
//
// WHY 0.206 SPECIFICALLY. The margins sort with a clean gap exactly here:
// every hard error is at ≤ 0.190, every remaining correct fire at ≥ 0.223.
// 0.206 is the midpoint, 0.016 clear on each side — comparable to the effort
// axis's 0.021. It is not set by any single case: dropping the most
// influential new one (`int_know_losalamos_arrival`, 0.190) moves the midpoint
// only to ~0.197.
//
// DO NOT try to recover the lost coverage by lowering the margin. Below 0.206
// good and bad interleave with no separating value — the two worst false
// positives fire at margins of 0.171 and 0.190, HIGHER than most correct
// fires. On this axis margin measures confidence, not correctness, because
// k=1 means TOPIC dominates ASK: `int_know_losalamos_arrival` (a bare date
// lookup) and `int_deep_losalamos_disillusion` (a causal question) resolve to
// the SAME nearest exemplar, and the wrong one wins with the bigger margin.
// Recovering coverage needs per-class thresholds or a topic-normalised score.
const DEFAULT_MIN_MARGIN: f32 = 0.206;

/// On-disk exemplar list. Each `[[example]]` row carries an intent
/// name (matches `Intent` debug-format, lowercased + snake_case) and
/// a query string.
#[derive(Debug, Clone, Deserialize)]
struct ExemplarFile {
    #[serde(default)]
    example: Vec<ExemplarRow>,
}

#[derive(Debug, Clone, Deserialize)]
struct ExemplarRow {
    intent: String,
    query: String,
    /// Optional scope axis ORTHOGONAL to intent. Forwarded through
    /// `EmbedClassification.scope` so downstream retrieval can bias
    /// corpus selection (e.g., `scope = "personal"` on a
    /// knowledge_query exemplar restricts atlas grounding to
    /// user-owned corpora — `mesh_sharing=false` in IndexInfo).
    /// `None` = no scope hint (current default behavior).
    #[serde(default)]
    scope: Option<String>,
    /// Optional locator axis, ALSO orthogonal to intent: which source
    /// the question points at. Today's only value is `"conversation"`
    /// — this thread, whose turns are already in the context. Scored
    /// one-vs-rest on its own gate (`locator_from_embedding`), NOT
    /// through the per-intent k=1 ranking, because the locator signal
    /// can be unambiguous on a query whose intent is not.
    #[serde(default)]
    locator: Option<String>,
}

/// One embedded exemplar.
#[derive(Debug, Clone)]
struct Exemplar {
    intent: Intent,
    /// L2-normalised embedding. Cosine-similarity reduces to dot
    /// product after normalisation.
    embedding: Vec<f32>,
    /// Kept for diagnostics — surfaced in router rationale.
    query: String,
    /// Optional scope tag from the source exemplar; propagates to
    /// `EmbedClassification.scope` when this exemplar is the
    /// nearest match.
    scope: Option<String>,
    /// Optional locator tag; consumed by `locator_from_embedding`,
    /// never by the intent ranking.
    locator: Option<String>,
}

/// Result of an embed-classify call.
#[derive(Debug, Clone)]
pub struct EmbedClassification {
    pub intent: Intent,
    /// Max cosine similarity against any exemplar of the chosen
    /// intent (in `[-1, 1]`; for L2-normalised vectors usually
    /// `[0, 1]`).
    pub top_sim: f32,
    /// `top_sim - second_intent_sim`. Larger = more confident.
    pub margin: f32,
    /// Nearest exemplar text — diagnostic surface for "why did this
    /// route here?". Truncated to 80 chars.
    pub nearest_exemplar: String,
    /// Scope tag from the nearest exemplar (when set). Orthogonal
    /// to intent; downstream retrieval consumes this to bias corpus
    /// selection. Values today: `Some("personal")` for
    /// conversation-history / journaling shapes; `None` for the
    /// general / external-knowledge default.
    pub scope: Option<String>,
}

/// Result of a locator-axis call. Orthogonal to [`EmbedClassification`]
/// — a query can carry a decisive locator with an ambiguous intent,
/// which is exactly the case this axis exists for.
#[derive(Debug, Clone)]
pub struct LocatorVerdict {
    /// The winning locator tag (today: `"conversation"`).
    pub locator: String,
    /// Max cosine similarity to any exemplar carrying that tag.
    pub top_sim: f32,
    /// `top_sim` minus the best similarity to any exemplar NOT
    /// carrying it — the one-vs-rest separation the gate turns on.
    pub margin: f32,
    /// Nearest tagged exemplar, truncated — the "why did this fire?"
    /// surface.
    pub nearest_exemplar: String,
}

/// Raw, UNGATED locator score — what [`LocatorVerdict`] is built from
/// once a gate admits it.
///
/// `score.sim_positive` is the best similarity to any exemplar
/// carrying the tag; `score.sim_negative` is the best similarity to
/// any exemplar NOT carrying it, so `score.margin()` is the
/// one-vs-rest separation the gate turns on.
#[derive(Debug, Clone)]
pub struct LocatorScore {
    pub locator: String,
    pub score: AxisScore,
    /// Untruncated — the caller truncates for display.
    pub nearest_exemplar: String,
    /// The UNTAGGED exemplar that set `score.sim_negative` — i.e. the
    /// row this query lost to when the margin came out negative.
    ///
    /// Without it a negative margin is an unattributable number: you
    /// know the tagged set was beaten but not by what, so "add more
    /// exemplars" is the only available move and it is a guess.
    /// `archive_examples.toml` records that guess being made and
    /// failing. `None` only when no untagged exemplar exists.
    pub rival_exemplar: Option<String>,
}

/// Raw, UNGATED intent score.
///
/// The intent axis is multi-class (k=1 nearest-neighbour per intent)
/// but its GATE is the same two-threshold rule every binary axis uses:
/// `sim_positive` is the winning intent's max cosine and
/// `sim_negative` is the runner-up intent's, so `margin()` is the
/// separation between the top two intents.
#[derive(Debug, Clone)]
pub struct IntentScore {
    pub top_intent: Intent,
    pub second_intent: Option<Intent>,
    pub score: AxisScore,
    /// Untruncated nearest exemplar text for the winning intent.
    pub nearest_exemplar: String,
    /// Scope tag carried by that nearest exemplar, if any.
    pub scope: Option<String>,
    /// Nearest exemplar of the RUNNER-UP intent — the row that set
    /// `score.sim_negative` and therefore capped the margin. `None`
    /// when only one intent has exemplars.
    pub rival_exemplar: Option<String>,
}

/// Hand-authored intent classifier. Pre-embeds every exemplar at
/// load time; classify-time cost is one embedding call plus a flat
/// loop over the exemplar set.
#[derive(Debug, Clone)]
pub struct EmbedRouter {
    exemplars: Vec<Exemplar>,
    min_top_sim: f32,
    min_margin: f32,
    locator_min_sim: f32,
    locator_min_margin: f32,
}

impl EmbedRouter {
    /// Load exemplars from the given TOML path; embed each one via
    /// `inference.embed_query`. Sequential because exemplar counts
    /// are small (~200) and the embed slot serialises anyway.
    pub async fn load(path: &Path, inference: Arc<dyn InferenceProvider>) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| Error::InvalidInput(format!("read exemplars {}: {e}", path.display())))?;
        Self::from_toml_str(&raw, inference).await
    }

    /// Build from in-memory TOML (the baked default in
    /// [`crate::router_bootstrap`], or any caller-supplied content). Identical
    /// parse + embed path to [`Self::load`] minus the file read, so a binary
    /// with no on-disk exemplars (a desktop `.app`) still gets the embed router
    /// — bench/desktop parity by construction.
    pub async fn from_toml_str(raw: &str, inference: Arc<dyn InferenceProvider>) -> Result<Self> {
        Self::from_toml_str_cached(raw, inference, None).await
    }

    /// [`Self::from_toml_str`] with an optional boot embed cache —
    /// exemplar embeddings are static per (text, model), and embedding
    /// ~175 of them sequentially at every boot is splash-screen time.
    pub async fn from_toml_str_cached(
        raw: &str,
        inference: Arc<dyn InferenceProvider>,
        mut cache: Option<&mut crate::router_embed_cache::BootEmbedCache>,
    ) -> Result<Self> {
        let parsed: ExemplarFile = toml::from_str(raw)
            .map_err(|e| Error::InvalidInput(format!("parse exemplars: {e}")))?;

        let mut exemplars = Vec::with_capacity(parsed.example.len());
        for row in parsed.example {
            let intent = parse_intent(&row.intent).map_err(|e| {
                Error::InvalidInput(format!("exemplar `{}`: {e}", truncate(&row.query, 60)))
            })?;
            let mut emb = match cache.as_deref_mut() {
                Some(c) => c.embed_query_cached(&*inference, &row.query).await?,
                None => inference.embed_query(&row.query).await?,
            };
            normalize(&mut emb);
            exemplars.push(Exemplar {
                intent,
                embedding: emb,
                query: row.query,
                scope: row.scope,
                locator: row.locator,
            });
        }

        tracing::info!(
            target: "router.embed",
            exemplar_count = exemplars.len(),
            "embed-router loaded"
        );

        Ok(Self {
            exemplars,
            min_top_sim: DEFAULT_MIN_TOP_SIM,
            min_margin: DEFAULT_MIN_MARGIN,
            locator_min_sim: DEFAULT_LOCATOR_MIN_SIM,
            locator_min_margin: DEFAULT_LOCATOR_MIN_MARGIN,
        })
    }

    /// Parse-only: the exemplar texts this router embeds, in file order,
    /// WITHOUT running inference. SSOT for the boot-cache freshness gate —
    /// shares the exact `ExemplarFile` parse + `query` field that
    /// `from_toml_str_cached` embeds above (`embed_query_cached`, the `q:`
    /// space), so the gate can never drift from what actually gets cached.
    pub fn exemplar_texts(raw: &str) -> Result<Vec<String>> {
        let parsed: ExemplarFile = toml::from_str(raw)
            .map_err(|e| Error::InvalidInput(format!("parse exemplars: {e}")))?;
        Ok(parsed.example.into_iter().map(|r| r.query).collect())
    }

    /// Override the default thresholds. Useful for tests + tuning.
    pub fn with_thresholds(mut self, min_top_sim: f32, min_margin: f32) -> Self {
        self.min_top_sim = min_top_sim;
        self.min_margin = min_margin;
        self
    }

    /// Override the locator-axis thresholds. Useful for tests + tuning.
    pub fn with_locator_thresholds(mut self, min_sim: f32, min_margin: f32) -> Self {
        self.locator_min_sim = min_sim;
        self.locator_min_margin = min_margin;
        self
    }

    /// How many exemplars carry a locator tag. Zero means the axis is
    /// inert — `locator_from_embedding` always abstains.
    pub fn locator_exemplar_count(&self) -> usize {
        self.exemplars.iter().filter(|e| e.locator.is_some()).count()
    }

    /// Embed + L2-normalise a query, without classifying.
    ///
    /// Exists so a caller can run the locator axis EARLY (before the
    /// intent pre-checks that would otherwise short-circuit) and then
    /// hand the same vector to [`Self::classify_from_embedding`] and
    /// the scope classifier. One embed serves all three; the axes stay
    /// independent.
    pub async fn embed_query_normalized(
        &self,
        query: &str,
        inference: &dyn InferenceProvider,
    ) -> Result<Vec<f32>> {
        let mut q = inference.embed_query(query).await?;
        normalize(&mut q);
        Ok(q)
    }

    pub fn exemplar_count(&self) -> usize {
        self.exemplars.len()
    }

    /// Classify `query` by max-similarity per intent. Returns `Some`
    /// only when both top-similarity and margin gates pass.
    pub async fn classify(
        &self,
        query: &str,
        inference: &dyn InferenceProvider,
    ) -> Result<Option<EmbedClassification>> {
        if self.exemplars.is_empty() {
            return Ok(None);
        }
        let mut q = inference.embed_query(query).await?;
        normalize(&mut q);
        Ok(self.classify_from_embedding(&q))
    }

    /// Same as `classify` but returns the L2-normalised query
    /// embedding alongside the verdict, so the caller can reuse the
    /// embedding for downstream classifiers (e.g. the binary
    /// personal-scope classifier) without paying a second embed.
    pub async fn classify_returning_embedding(
        &self,
        query: &str,
        inference: &dyn InferenceProvider,
    ) -> Result<(Option<EmbedClassification>, Vec<f32>)> {
        if self.exemplars.is_empty() {
            // Still embed so caller can run scope classifier; cheap
            // single embed and matches the non-empty path's contract.
            let mut q = inference.embed_query(query).await?;
            normalize(&mut q);
            return Ok((None, q));
        }
        let mut q = inference.embed_query(query).await?;
        normalize(&mut q);
        let intent = self.classify_from_embedding(&q);
        Ok((intent, q))
    }

    /// Score the LOCATOR axis against a pre-computed query embedding.
    ///
    /// One-vs-rest, deliberately not the per-intent k=1 ranking used
    /// for intent:
    ///
    /// * `top_sim` = best similarity to any exemplar carrying a given
    ///   locator tag;
    /// * `margin`  = that minus the best similarity to any exemplar
    ///   NOT carrying it — every other row in the bank is a negative,
    ///   including the personal-archive rows that ask about the user's
    ///   past conversations rather than this one.
    ///
    /// Why a separate axis at all: intent and locator are independent.
    /// "What was the first thing I asked?" is intent-ambiguous (it sits
    /// near conation and near archive recall) while being locator-clear.
    /// Reading the locator off the winning INTENT exemplar — the way
    /// `scope` used to work — loses exactly those cases, because k=1
    /// hands the tag to whichever intent won. See `scope_classifier.rs`
    /// for the same lesson learned the expensive way.
    ///
    /// Returns `None` when no tagged exemplar exists, when the floor
    /// isn't met, or when the margin is too thin — in every case the
    /// caller simply continues down its normal cascade.
    pub fn locator_from_embedding(&self, q_normalized: &[f32]) -> Option<LocatorVerdict> {
        let scored = self.score_locator_from_embedding(q_normalized)?;
        let gate = self.locator_gate();
        let decided = gate.admits(scored.score);

        // Glassbox on its own target so "why did/didn't the locator
        // fire?" is answerable from logs without enabling the whole
        // router.embed stream. Emitted for every evaluation, including
        // abstentions — the near-misses are the tuning signal, and
        // `cushion` is how far this one sat from flipping.
        tracing::info!(
            target: "router.locator",
            event = "classify",
            locator = %scored.locator,
            top_sim = scored.score.sim_positive,
            rest_best = scored.score.sim_negative,
            margin = scored.score.margin(),
            min_sim = gate.min_sim,
            min_margin = gate.min_margin,
            cushion = gate.cushion(scored.score),
            decided,
            nearest = %truncate(&scored.nearest_exemplar, 60),
            rival = %scored
                .rival_exemplar
                .as_deref()
                .map(|r| truncate(r, 60))
                .unwrap_or_else(|| "<none>".to_string()),
            "router.locator: one-vs-rest decision"
        );

        decided.then(|| LocatorVerdict {
            locator: scored.locator,
            top_sim: scored.score.sim_positive,
            margin: scored.score.margin(),
            nearest_exemplar: truncate(&scored.nearest_exemplar, 80),
        })
    }

    /// The gate currently applied to the locator axis.
    pub fn locator_gate(&self) -> AxisGate {
        AxisGate::new(self.locator_min_sim, self.locator_min_margin)
    }

    /// The gate currently applied to the intent axis.
    pub fn intent_gate(&self) -> AxisGate {
        AxisGate::new(self.min_top_sim, self.min_margin)
    }

    /// Raw, UNGATED locator score. `None` when no exemplar carries a
    /// locator tag (the axis is inert) or the query embedding is empty.
    ///
    /// Split out from [`Self::locator_from_embedding`] so
    /// [`crate::router_calibration`] can evaluate any candidate gate
    /// from a single embedding pass.
    pub fn score_locator_from_embedding(&self, q_normalized: &[f32]) -> Option<LocatorScore> {
        if q_normalized.is_empty() {
            return None;
        }
        // Best similarity per tag, and the best over untagged rows.
        let mut per_tag: HashMap<&str, (f32, &str)> = HashMap::new();
        let mut untagged_best = f32::MIN;
        let mut untagged_best_q: Option<&str> = None;
        for ex in &self.exemplars {
            if ex.embedding.len() != q_normalized.len() {
                continue;
            }
            let sim = dot(q_normalized, &ex.embedding);
            match ex.locator.as_deref() {
                Some(tag) => {
                    per_tag
                        .entry(tag)
                        .and_modify(|(best, best_q)| {
                            if sim > *best {
                                *best = sim;
                                *best_q = ex.query.as_str();
                            }
                        })
                        .or_insert((sim, ex.query.as_str()));
                }
                None => {
                    if sim > untagged_best {
                        untagged_best = sim;
                        untagged_best_q = Some(ex.query.as_str());
                    }
                }
            }
        }
        let (tag, (top_sim, nearest)) = per_tag
            .into_iter()
            .max_by(|a, b| a.1 .0.partial_cmp(&b.1 .0).unwrap_or(std::cmp::Ordering::Equal))?;

        // A second tag would also be a negative for the winner. With
        // one tag today this reduces to the untagged best; written so
        // adding a tag can't silently weaken the gate.
        let rest_best = if untagged_best == f32::MIN {
            0.0
        } else {
            untagged_best
        };
        Some(LocatorScore {
            locator: tag.to_string(),
            score: AxisScore::new(top_sim, rest_best),
            nearest_exemplar: nearest.to_string(),
            rival_exemplar: untagged_best_q.map(str::to_string),
        })
    }

    /// Classify against a pre-computed query embedding. Public for
    /// callers that already have one (the router could splice this
    /// into the existing search-embedding pipeline to skip a second
    /// embed call).
    pub fn classify_from_embedding(&self, q_normalized: &[f32]) -> Option<EmbedClassification> {
        let scored = self.score_intent_from_embedding(q_normalized)?;
        let gate = self.intent_gate();
        let decided = gate.admits(scored.score);

        // Glassbox: the routing decision is the *first level* of the
        // whole stack — if intent classification is wrong, every
        // downstream choice (retrieval, expansion, synthesis) is built
        // on sand. Emit per-query whether the embed router was confident
        // enough to OWN this route (`decided=true`, short-circuiting the
        // heuristic + LLM cascade) or fell through (`decided=false`), with
        // the similarity/margin vs thresholds that drove it. On the
        // `router.embed` target, which the default daemon/eval filter
        // does NOT enable — so this is opt-in (`router.embed=info`) and
        // free in normal operation. Pairs with the second-best intent so
        // near-miss misroutes (the margin-just-cleared case) are visible,
        // and with `cushion` — the signed distance to the boundary, which
        // is what a score-distribution drift report aggregates.
        tracing::info!(
            target: "router.embed",
            event = "classify",
            top_intent = ?scored.top_intent,
            top_sim = scored.score.sim_positive,
            second_intent = ?scored.second_intent.as_ref().map(|i| format!("{i:?}")),
            second_sim = scored.score.sim_negative,
            margin = scored.score.margin(),
            min_top_sim = gate.min_sim,
            min_margin = gate.min_margin,
            cushion = gate.cushion(scored.score),
            decided,
            "router.embed: classify decision"
        );

        if !decided {
            return None;
        }
        Some(EmbedClassification {
            intent: scored.top_intent,
            top_sim: scored.score.sim_positive,
            margin: scored.score.margin(),
            nearest_exemplar: truncate(&scored.nearest_exemplar, 80),
            scope: scored.scope,
        })
    }

    /// Raw, UNGATED intent score: the winning intent, the runner-up,
    /// and their similarities.
    ///
    /// Split out from [`Self::classify_from_embedding`] so
    /// [`crate::router_calibration`] can evaluate any candidate gate
    /// from a single embedding pass. On this axis the sweep answers the
    /// question accuracy cannot: how much of the bank could the embed
    /// router OWN — displacing a ~1.2s LLM classifier call — before its
    /// precision slips.
    pub fn score_intent_from_embedding(&self, q_normalized: &[f32]) -> Option<IntentScore> {
        if self.exemplars.is_empty() || q_normalized.is_empty() {
            return None;
        }

        // Max similarity per intent + remember the nearest exemplar
        // (text + scope) for the diagnostic surface and downstream
        // routing bias.
        let mut per_intent: HashMap<Intent, (f32, &str, Option<&str>)> = HashMap::new();
        for ex in &self.exemplars {
            if ex.embedding.len() != q_normalized.len() {
                // Dimension mismatch (exemplars embedded with a
                // different model). Skip rather than panic — caller
                // will see "no result" and fall through.
                continue;
            }
            let sim = dot(q_normalized, &ex.embedding);
            per_intent
                .entry(ex.intent.clone())
                .and_modify(|(best, best_q, best_scope)| {
                    if sim > *best {
                        *best = sim;
                        *best_q = ex.query.as_str();
                        *best_scope = ex.scope.as_deref();
                    }
                })
                .or_insert((sim, ex.query.as_str(), ex.scope.as_deref()));
        }
        if per_intent.is_empty() {
            return None;
        }

        let mut ranked: Vec<(Intent, f32, &str, Option<&str>)> = per_intent
            .into_iter()
            .map(|(i, (s, q, sc))| (i, s, q, sc))
            .collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let (top_intent, top_sim, nearest, top_scope) =
            (ranked[0].0.clone(), ranked[0].1, ranked[0].2, ranked[0].3);
        let second_sim = ranked.get(1).map(|(_, s, _, _)| *s).unwrap_or(0.0);
        let second_intent = ranked.get(1).map(|(i, _, _, _)| i.clone());

        Some(IntentScore {
            top_intent,
            second_intent,
            score: AxisScore::new(top_sim, second_sim),
            nearest_exemplar: nearest.to_string(),
            scope: top_scope.map(String::from),
            rival_exemplar: ranked.get(1).map(|(_, _, q, _)| q.to_string()),
        })
    }
}

// ── Helpers ──────────────────────────────────────────────────────

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}

/// The wire label for an `Intent` — the inverse of [`parse_intent`]
/// over the labels an exemplar TOML can name.
///
/// Exists so calibration banks and routing reports can name intents in
/// the same snake_case vocabulary the exemplar TOML uses, without every
/// caller re-deriving it from `{:?}`. `intent_label_round_trips` keeps
/// the two in sync.
///
/// `SimpleAction` and `Continuation` are router-internal — they are
/// produced downstream (a web-search action, a thread continuation)
/// and no exemplar can be tagged with them, so `parse_intent` REJECTS
/// their labels. The mapping is deliberately one-way for those two:
/// a report still needs to name them.
pub fn intent_label(intent: &Intent) -> &'static str {
    match intent {
        Intent::SimpleQuery => "simple_query",
        Intent::KnowledgeQuery => "knowledge_query",
        Intent::DeepQuery => "deep_query",
        Intent::ComparisonQuery => "comparison_query",
        Intent::CodeQuery => "code_query",
        Intent::ComplexTask => "complex_task",
        Intent::MetalingualQuery => "metalingual_query",
        Intent::ConationQuery => "conation_query",
        Intent::CommissiveQuery => "commissive_query",
        Intent::ExpressiveQuery => "expressive_query",
        Intent::GenerativeQuery => "generative_query",
        Intent::SimpleAction { .. } => "simple_action",
        Intent::Continuation { .. } => "continuation",
    }
}

/// Parse the snake_case intent label in the exemplar TOML into an
/// `Intent` enum. Accepts the same set the router's classifier
/// emits.
fn parse_intent(s: &str) -> std::result::Result<Intent, String> {
    match s.trim() {
        "simple_query" | "SimpleQuery" => Ok(Intent::SimpleQuery),
        "knowledge_query" | "KnowledgeQuery" => Ok(Intent::KnowledgeQuery),
        "deep_query" | "DeepQuery" => Ok(Intent::DeepQuery),
        "comparison_query" | "ComparisonQuery" => Ok(Intent::ComparisonQuery),
        "code_query" | "CodeQuery" => Ok(Intent::CodeQuery),
        "complex_task" | "ComplexTask" => Ok(Intent::ComplexTask),
        "metalingual_query" | "MetalingualQuery" => Ok(Intent::MetalingualQuery),
        "conation_query" | "ConationQuery" => Ok(Intent::ConationQuery),
        "commissive_query" | "CommissiveQuery" => Ok(Intent::CommissiveQuery),
        "expressive_query" | "ExpressiveQuery" => Ok(Intent::ExpressiveQuery),
        "generative_query" | "GenerativeQuery" => Ok(Intent::GenerativeQuery),
        other => Err(format!("unknown intent label: {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_unit_vector() {
        let mut v = vec![3.0, 4.0];
        normalize(&mut v);
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!((v[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn dot_normalized_is_cosine() {
        let a = vec![0.6, 0.8];
        let b = vec![0.6, 0.8];
        assert!((dot(&a, &b) - 1.0).abs() < 1e-6);
    }

    /// `intent_label` and `parse_intent` must stay inverse over every
    /// label an exemplar TOML can carry — otherwise a calibration bank
    /// could name an intent the router can never produce, and the
    /// mismatch would read as a routing failure rather than a typo.
    #[test]
    fn intent_label_round_trips() {
        let parseable = [
            Intent::SimpleQuery,
            Intent::KnowledgeQuery,
            Intent::DeepQuery,
            Intent::ComparisonQuery,
            Intent::CodeQuery,
            Intent::ComplexTask,
            Intent::MetalingualQuery,
            Intent::ConationQuery,
            Intent::CommissiveQuery,
            Intent::ExpressiveQuery,
            Intent::GenerativeQuery,
        ];
        for i in parseable {
            let label = intent_label(&i);
            assert_eq!(
                parse_intent(label).expect("label must parse back"),
                i,
                "round-trip failed for {label}"
            );
        }
    }

    /// The two router-internal variants are namable but NOT parseable —
    /// an exemplar tagged with them would be a bank authoring error.
    #[test]
    fn router_internal_intents_are_one_way() {
        assert!(parse_intent("simple_action").is_err());
        assert!(parse_intent("continuation").is_err());
    }

    #[test]
    fn parse_intent_snake_and_camel() {
        assert!(matches!(
            parse_intent("knowledge_query"),
            Ok(Intent::KnowledgeQuery)
        ));
        assert!(matches!(
            parse_intent("KnowledgeQuery"),
            Ok(Intent::KnowledgeQuery)
        ));
        assert!(parse_intent("nonsense").is_err());
    }

    fn make_exemplar(intent: Intent, query: &str, emb: Vec<f32>) -> Exemplar {
        tagged_exemplar(intent, query, emb, None)
    }

    fn tagged_exemplar(
        intent: Intent,
        query: &str,
        emb: Vec<f32>,
        locator: Option<&str>,
    ) -> Exemplar {
        let mut e = emb;
        normalize(&mut e);
        Exemplar {
            intent,
            embedding: e,
            query: query.into(),
            scope: None,
            locator: locator.map(String::from),
        }
    }

    fn router_with(exemplars: Vec<Exemplar>, min_top_sim: f32, min_margin: f32) -> EmbedRouter {
        EmbedRouter {
            exemplars,
            min_top_sim,
            min_margin,
            locator_min_sim: DEFAULT_LOCATOR_MIN_SIM,
            locator_min_margin: DEFAULT_LOCATOR_MIN_MARGIN,
        }
    }

    fn unit(v: Vec<f32>) -> Vec<f32> {
        let mut q = v;
        normalize(&mut q);
        q
    }

    #[test]
    fn classify_picks_max_similarity_intent_with_margin() {
        let r = router_with(
            vec![
                make_exemplar(Intent::KnowledgeQuery, "What is X?", vec![1.0, 0.0, 0.0]),
                make_exemplar(Intent::DeepQuery, "Why did X happen?", vec![0.0, 1.0, 0.0]),
                make_exemplar(
                    Intent::MetalingualQuery,
                    "What does X mean here?",
                    vec![0.0, 0.0, 1.0],
                ),
            ],
            0.5,
            0.1,
        );
        // Query close to the KnowledgeQuery exemplar
        let q = vec![0.95_f32, 0.10, 0.10];
        let mut qn = q.clone();
        normalize(&mut qn);
        let out = r.classify_from_embedding(&qn).unwrap();
        assert_eq!(out.intent, Intent::KnowledgeQuery);
        assert!(out.top_sim > 0.9);
        assert!(out.margin > 0.5);
    }

    #[test]
    fn classify_returns_none_below_min_top_sim() {
        let r = router_with(
            vec![make_exemplar(
                Intent::KnowledgeQuery,
                "x",
                vec![1.0, 0.0, 0.0],
            )],
            0.9,
            0.0,
        );
        // Orthogonal query → 0 similarity
        let mut q = vec![0.0_f32, 1.0, 0.0];
        normalize(&mut q);
        assert!(r.classify_from_embedding(&q).is_none());
    }

    #[test]
    fn classify_returns_none_below_min_margin() {
        let r = router_with(
            vec![
                make_exemplar(Intent::KnowledgeQuery, "x", vec![1.0, 0.0, 0.0]),
                make_exemplar(Intent::DeepQuery, "y", vec![0.9, 0.1, 0.0]),
            ],
            0.0,
            0.2, // tight
        );
        // Query close to both → margin too small to commit
        let mut q = vec![1.0_f32, 0.05, 0.0];
        normalize(&mut q);
        assert!(r.classify_from_embedding(&q).is_none());
    }

    #[test]
    fn classify_returns_none_when_exemplars_empty() {
        let r = router_with(vec![], 0.0, 0.0);
        assert!(r.classify_from_embedding(&[1.0, 0.0]).is_none());
    }

    // ── Locator axis ────────────────────────────────────────────
    //
    // Fixture geometry: the locator exemplar sits on x, the rest of
    // the bank on y/z. A query leaning toward x is locator-clear;
    // one sitting between x and y is not.

    fn locator_bank() -> Vec<Exemplar> {
        vec![
            tagged_exemplar(
                Intent::MetalingualQuery,
                "What did I ask you at the very start of this chat?",
                vec![1.0, 0.0, 0.0],
                Some("conversation"),
            ),
            make_exemplar(Intent::KnowledgeQuery, "What is X?", vec![0.0, 1.0, 0.0]),
            make_exemplar(Intent::ConationQuery, "Stop.", vec![0.0, 0.0, 1.0]),
        ]
    }

    #[test]
    fn locator_fires_on_a_clear_one_vs_rest_win() {
        let r = router_with(locator_bank(), 0.55, 0.10);
        let v = r
            .locator_from_embedding(&unit(vec![0.98, 0.15, 0.0]))
            .expect("clear locator win must fire");
        assert_eq!(v.locator, "conversation");
        assert!(v.margin > 0.5, "margin was {}", v.margin);
        assert!(v.nearest_exemplar.starts_with("What did I ask you"));
    }

    /// The axis is ORTHOGONAL to intent: a query the intent gate
    /// abstains on (two intents too close to separate) can still carry
    /// a decisive locator. This is the whole reason it is not read off
    /// the winning intent exemplar.
    #[test]
    fn locator_can_fire_where_the_intent_gate_abstains() {
        let mut bank = locator_bank();
        // Second intent placed right next to the first so the intent
        // margin collapses, while the locator exemplar stays clear.
        bank.push(make_exemplar(
            Intent::DeepQuery,
            "Why did X happen?",
            vec![0.0, 0.99, 0.1],
        ));
        let r = router_with(bank, 0.55, 0.10);
        let q = unit(vec![0.55, 0.83, 0.0]);
        assert!(
            r.classify_from_embedding(&q).is_none(),
            "fixture must be intent-ambiguous for this test to mean anything"
        );
        // Same query, locator axis: still under-separated here, so it
        // abstains too — the point is that the two gates are decided
        // independently, not that locator always wins.
        let loose = router_with(locator_bank(), 0.55, 0.10).with_locator_thresholds(0.4, 0.0);
        assert_eq!(
            loose
                .locator_from_embedding(&unit(vec![0.75, 0.66, 0.0]))
                .map(|v| v.locator),
            Some("conversation".to_string()),
        );
    }

    #[test]
    fn locator_abstains_below_its_margin() {
        // Query equidistant from the tagged exemplar and a plain one.
        let r = router_with(locator_bank(), 0.55, 0.10).with_locator_thresholds(0.4, 0.05);
        assert!(r
            .locator_from_embedding(&unit(vec![1.0, 1.0, 0.0]))
            .is_none());
    }

    /// Floor and margin are independent gates: a query can win
    /// one-vs-rest by a mile and still be too far from anything to
    /// commit on. The 4th dimension here is orthogonal to every
    /// exemplar, so it drains absolute similarity while leaving the
    /// margin wide.
    #[test]
    fn locator_abstains_below_its_floor_despite_a_wide_margin() {
        let bank = vec![
            tagged_exemplar(
                Intent::MetalingualQuery,
                "What did I ask you at the very start of this chat?",
                vec![1.0, 0.0, 0.0, 0.0],
                Some("conversation"),
            ),
            make_exemplar(Intent::KnowledgeQuery, "What is X?", vec![0.0, 1.0, 0.0, 0.0]),
        ];
        let q = unit(vec![0.6, 0.1, 0.0, 0.79]);
        let permissive = router_with(bank.clone(), 0.55, 0.10).with_locator_thresholds(0.4, 0.05);
        let v = permissive
            .locator_from_embedding(&q)
            .expect("margin alone is comfortably clear");
        assert!(v.margin > 0.4, "margin was {}", v.margin);
        assert!(v.top_sim < 0.7, "top_sim was {}", v.top_sim);

        let strict = router_with(bank, 0.55, 0.10).with_locator_thresholds(0.9, 0.0);
        assert!(
            strict.locator_from_embedding(&q).is_none(),
            "the floor must reject what the margin would have admitted"
        );
    }

    #[test]
    fn locator_abstains_when_no_exemplar_is_tagged() {
        let r = router_with(
            vec![make_exemplar(
                Intent::KnowledgeQuery,
                "What is X?",
                vec![1.0, 0.0, 0.0],
            )],
            0.55,
            0.10,
        );
        assert_eq!(r.locator_exemplar_count(), 0);
        assert!(r.locator_from_embedding(&unit(vec![1.0, 0.0, 0.0])).is_none());
    }

    /// A negative margin says the tagged set was beaten. Without the
    /// rival's identity that is an unattributable number, and the only
    /// available move is to guess more exemplars — the guess
    /// `archive_examples.toml` records failing.
    ///
    /// The rival must track the ACTUAL argmax over untagged rows, not
    /// merely the first one seen, so this leans the query at each
    /// untagged exemplar in turn and demands the answer follow.
    #[test]
    fn locator_score_names_the_untagged_rival_that_capped_the_margin() {
        let r = router_with(locator_bank(), 0.55, 0.10);

        // Leaning x→y: the KnowledgeQuery row on y is the rival.
        let s = r
            .score_locator_from_embedding(&unit(vec![0.7, 0.6, 0.0]))
            .expect("tagged row exists");
        assert_eq!(s.rival_exemplar.as_deref(), Some("What is X?"));
        assert!(s.nearest_exemplar.starts_with("What did I ask you"));

        // Leaning x→z: the ConationQuery row on z takes over. Same
        // tagged nearest, different rival — which is the whole point.
        let s = r
            .score_locator_from_embedding(&unit(vec![0.7, 0.0, 0.6]))
            .expect("tagged row exists");
        assert_eq!(s.rival_exemplar.as_deref(), Some("Stop."));
        assert!(s.nearest_exemplar.starts_with("What did I ask you"));
    }

    /// `sim_negative` falls back to 0.0 when nothing is untagged. The
    /// rival must then be `None` rather than a fabricated row.
    #[test]
    fn locator_rival_is_none_when_every_exemplar_is_tagged() {
        let r = router_with(
            vec![tagged_exemplar(
                Intent::MetalingualQuery,
                "What did we cover so far?",
                vec![1.0, 0.0, 0.0],
                Some("conversation"),
            )],
            0.55,
            0.10,
        );
        let s = r
            .score_locator_from_embedding(&unit(vec![1.0, 0.0, 0.0]))
            .expect("tagged row exists");
        assert_eq!(s.rival_exemplar, None);
        assert_eq!(s.score.sim_negative, 0.0);
    }

    /// The intent axis is multi-class, so its `sim_negative` is the
    /// RUNNER-UP intent's best row. Naming it answers "what did this
    /// query nearly route to instead?".
    #[test]
    fn intent_score_names_the_runner_up_exemplar() {
        let r = router_with(
            vec![
                make_exemplar(Intent::KnowledgeQuery, "What is X?", vec![1.0, 0.0, 0.0]),
                make_exemplar(Intent::DeepQuery, "Why did X happen?", vec![0.0, 1.0, 0.0]),
                make_exemplar(Intent::ConationQuery, "Stop.", vec![0.0, 0.0, 1.0]),
            ],
            0.55,
            0.10,
        );
        let s = r
            .score_intent_from_embedding(&unit(vec![0.8, 0.55, 0.0]))
            .expect("non-empty bank");
        assert_eq!(s.top_intent, Intent::KnowledgeQuery);
        assert_eq!(s.nearest_exemplar, "What is X?");
        assert_eq!(s.rival_exemplar.as_deref(), Some("Why did X happen?"));
    }

    /// The shipped bank must actually carry the axis — a rename or a
    /// dropped tag would silently disable the whole tier, and the
    /// symptom (conversation questions quietly routing to corpora)
    /// is the exact failure this was built to fix.
    #[test]
    fn shipped_exemplars_carry_conversation_locator_rows() {
        let parsed: ExemplarFile =
            toml::from_str(crate::router_bootstrap::BAKED_ROUTER_EXEMPLARS).expect("baked TOML");
        let tagged = parsed
            .example
            .iter()
            .filter(|r| r.locator.as_deref() == Some("conversation"))
            .count();
        assert!(
            tagged >= 6,
            "expected the conversation-locator exemplar set, found {tagged}"
        );
    }
}
