// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded agentic evidence loop for KnowledgeQuery (prototype,
//! env-gated behind `SOVEREIGN_AGENTIC_KQ=1`).
//!
//! Today's KQ contract treats retrieval as preprocessing: one
//! embedding pass over the user's raw phrasing, then synthesis over
//! whatever came back. The model — the only component that knows what
//! evidence would *answer* the question — never gets a say. Measured
//! failure shape (2026-06-10): chaos-bench retrieval recall pinned at
//! 2/8 on critical evidence spans across five runs ("which anarchist
//! takes Winnie's money" retrieved zero Ossipon chunks); production
//! case "When was the last time the Knicks won a championship?"
//! returned a Phil Jackson article. Topical-overlap retrieval, no
//! agency.
//!
//! This module gives the model ONE bounded round of agency:
//!
//! 1. **Sufficiency check** — forced-choice logprob pass on the fast
//!    slot (~1 token decode, prefill-dominated): do the round-0
//!    passages contain the specific facts the question needs?
//! 2. **Query formulation** — when insufficient, the fast slot emits
//!    1–3 short targeted queries (grammar-constrained JSON), naming
//!    the entities/events the answer requires. The fast slot's
//!    formulation quality is already production-proven by the
//!    InformationRequest flow (`gap.rs`) — this moves the same
//!    capability PRE-synthesis where it can still change the evidence.
//! 3. **Round-2 retrieval** — each formulated query runs the SAME
//!    `kq_pipeline()` as round 0 (own embedding, FTS+vector, mesh,
//!    boosts); results are deduped against round 0 and appended. The
//!    existing downstream PPR rerank orders the merged set.
//!
//! One structural stage runs alongside the model's own formulations,
//! in code rather than in the model (structure-over-instruction):
//! atlas event/relation atoms whose pronoun-resolved statements
//! overlap the question's content words are injected directly as
//! evidence (`atlas_atom_matches`). The loop also classifies the
//! question as in-world (entity-anchored) or world-general, which
//! drives the caller's choice of insufficiency note.
//!
//! Hard bounds: one extra round, ≤6 formulated queries, ≤12 appended
//! chunks. Latency: +~2s (sufficiency) on every gated turn; the
//! formulation + retrieval cost (~4–10s) is paid only on turns that
//! are, by the sufficiency judge's own verdict, currently
//! unanswerable. Glassbox: every step traces under
//! `agentic_kq` with the verdict probability, the formulated
//! queries, and per-query yields.

use std::collections::HashSet;

use crate::runtime::retrieval_pipeline::{kq_pipeline, PipelineState};
use crate::types::{CompletionRequest, Intent, Speed};

use super::ConversationContext;
use super::Runtime;

/// Process-level parse cache for a corpus's `atlas/atoms.json`, keyed by corpus
/// id with the file mtime as the freshness token.
///
/// The evidence loop's gazetteer helpers (`atlas_entity_names`,
/// `atlas_atom_records`) are consulted on every gated turn — and
/// `atlas_atom_matches` calls BOTH, so without this it was up to FOUR full
/// read+serde-parses of atoms.json per turn. For the 724 MB / 1.67M-atom
/// wikipedia atlas that is 0.5–5 s **per turn** of pure parsing (the dominant
/// cost; the per-call iteration the helpers already did is comparatively cheap
/// and left unchanged). This caches the parsed `Value` once per (corpus, mtime):
/// the first gated turn pays the parse, subsequent turns are an `Arc` clone, and
/// a re-enriched corpus (newer mtime) reparses on its next turn. Returns `None`
/// (→ empty gazetteer, the existing best-effort contract) when the file is
/// missing or unparseable.
fn cached_atoms_json(corpus_id: &str) -> Option<std::sync::Arc<serde_json::Value>> {
    use std::sync::{Arc, OnceLock, RwLock};
    use std::time::SystemTime;
    type Cache = RwLock<std::collections::HashMap<String, (SystemTime, Arc<serde_json::Value>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(|| RwLock::new(std::collections::HashMap::new()));

    let base = std::env::var("SOVEREIGN_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".sovereign"));
    let path = base
        .join("indexes")
        .join(corpus_id)
        .join("atlas")
        .join("atoms.json");
    let mtime = std::fs::metadata(&path).ok()?.modified().ok()?;

    // Fast path: present and fresh (mtime unchanged since we parsed it).
    if let Ok(map) = cache.read() {
        if let Some((cached_mtime, value)) = map.get(corpus_id) {
            if *cached_mtime == mtime {
                return Some(Arc::clone(value));
            }
        }
    }
    // Slow path: (re)parse and cache under the current mtime.
    let text = std::fs::read_to_string(&path).ok()?;
    let value = Arc::new(serde_json::from_str::<serde_json::Value>(&text).ok()?);
    if let Ok(mut map) = cache.write() {
        map.insert(corpus_id.to_string(), (mtime, Arc::clone(&value)));
    }
    Some(value)
}

/// Canonical entity names from a corpus's atlas atoms file
/// (`<data>/indexes/<corpus>/atlas/atoms.json`, via the mtime-keyed
/// [`cached_atoms_json`] cache). Best-effort: missing/garbled atlas → empty vec.
/// Used only as the gazetteer fallback when the embedded-context provider has no
/// entry for the corpus (see call site).
fn atlas_entity_names(corpus_id: &str) -> Vec<String> {
    let Some(v) = cached_atoms_json(corpus_id) else {
        return Vec::new();
    };
    // Borrow the shared cached Value in place — no array clone (the pre-cache
    // version cloned the whole atoms array on every call).
    let atoms: &[serde_json::Value] = v
        .get("atoms")
        .and_then(|a| a.as_array())
        .or_else(|| v.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    atoms
        .iter()
        .filter_map(|a| {
            // v2 schema: {"atom_type": "Entity", "data": {"canonical_name": …}};
            // older flat shapes carry canonical_name at the top level.
            a.get("data")
                .and_then(|d| d.get("canonical_name"))
                .or_else(|| a.get("canonical_name"))
                .and_then(|n| n.as_str())
        })
        .map(str::to_string)
        .collect()
}

/// Content words of the question: ≥4 chars, stop-filtered, lowercased,
/// first 6 distinct. The lexical view of the question that drives
/// atlas-atom matching.
fn question_keywords(message: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "which",
        "who",
        "what",
        "does",
        "kind",
        "with",
        "near",
        "the",
        "end",
        "novel",
        "their",
        "from",
        "into",
        "takes",
        "that",
        "this",
        "her",
        "his",
        "and",
        "when",
        "where",
        "about",
        "according",
    ];
    let mut out: Vec<String> = Vec::new();
    for w in message.split(|c: char| !c.is_alphanumeric()) {
        if w.len() < 4 {
            continue;
        }
        let lw = w.to_lowercase();
        if STOP.contains(&lw.as_str()) || out.contains(&lw) {
            continue;
        }
        out.push(lw);
        if out.len() >= 6 {
            break;
        }
    }
    out
}

/// Does the question name an entity that lives inside the corpus's
/// own world (per the atlas gazetteer)? Decides whether "general
/// knowledge" is admissible for an unanswered question: the capital
/// of Australia is a world fact a model may caveat-and-answer, but a
/// character's unstated real name exists only inside the corpus —
/// outside knowledge structurally cannot supply it, and a
/// GK-caveated guess is a fabrication in honest clothing (measured
/// 2026-06-11: "from general knowledge: The Professor's real name is
/// Dr. Verloc" — pure confabulation wearing the caveat format, which
/// also exempts it from the bench critic's claim extractor).
fn question_is_entity_anchored(keywords: &[String], corpus_ids: &[String]) -> bool {
    let entity_toks: HashSet<String> = corpus_ids
        .iter()
        .flat_map(|cid| atlas_entity_names(cid))
        .flat_map(|n| {
            n.split(|c: char| !c.is_alphanumeric())
                .filter(|t| t.len() >= 4)
                .map(str::to_lowercase)
                .collect::<Vec<_>>()
        })
        .collect();
    let hit = keywords.iter().any(|k| entity_toks.contains(k));
    dbg(&format!(
        "entity_match: kw={keywords:?} cids={corpus_ids:?} entity_toks_n={} hit={hit}",
        entity_toks.len()
    ));
    hit
}

/// Deterministic entity-anchored verdict for the grounding gate — computed from
/// the question + the conversation's corpora alone, with NO model call and
/// independent of whether the (optional, fast-route-skipped) agentic evidence
/// loop runs. Mirrors the `lookup_ids` derivation inside `agentic_evidence_round`
/// so the gate's GK-caveat exemption closes on EVERY route, including the fast
/// streaming/desktop path. Without this, `gate_entity_anchored` defaulted false
/// off the agentic path and a "from general knowledge: …" fabrication about a
/// corpus entity was released unverified.
pub(crate) fn compute_entity_anchored(
    message: &str,
    enabled_corpora: Option<&[String]>,
    chunks: &[corpus_engine::ScoredChunk],
) -> bool {
    let lookup_ids: Vec<String> = match enabled_corpora {
        Some(ids) if !ids.is_empty() => ids.to_vec(),
        _ => merged_corpora(chunks).into_iter().collect(),
    };
    question_is_entity_anchored(&question_keywords(message), &lookup_ids)
}

/// Corpus-DEICTIC question: it refers to the corpus's own material by
/// deixis ("the story", "this document") rather than by entity name,
/// so the lexical gazetteer check misses it — yet outside knowledge
/// structurally cannot answer it any more than it can an
/// entity-anchored one. Closes the GK-caveat exemption for the gate:
/// measured 2026-06-11 (saltgrass-p3b), "In what year is the story
/// set?" drew a caveated retry fabrication ("by William Trevor,
/// published 1952" — no such author) that the caveat exempted from
/// claim extraction. World-general questions ("capital of Canada")
/// contain none of these phrasings and keep the honest GK path.
pub(crate) fn question_is_corpus_deictic(message: &str) -> bool {
    const DEICTIC: &[&str] = &[
        "the story",
        "the novel",
        "the book",
        "the text",
        "the document",
        "this document",
        "this book",
        "the narrative",
        "the plot",
        "the attached",
        "the report",
        "your sources",
        "the sources",
        "the corpus",
    ];
    let q = message.to_lowercase();
    DEICTIC.iter().any(|d| q.contains(d))
}

/// Broader companion to `question_is_entity_anchored`: does the
/// question share ANY content word (stemmed) with the corpus's atlas —
/// entity names or atom-description vocabulary? Drives the structural
/// general-knowledge caveat: when a question is topically FOREIGN to
/// every enabled corpus and two retrieval rounds found nothing, the
/// answer is coming from the model's parametric memory and must say
/// so. The caveat is committed via `assistant_prefix` in code because
/// prompt instructions to add it are followed ~60% of the time
/// (measured across the 2026-06-11 banks: 3/5 OOD caveat omissions on
/// one run was the difference between honesty 0.64 and 0.91).
fn question_is_corpus_anchored(keywords: &[String], corpus_ids: &[String]) -> bool {
    if keywords.is_empty() {
        // No content words to test — err on the side of "anchored"
        // (no caveat) rather than mislabeling a corpus answer as GK.
        return true;
    }
    let kw_stems: Vec<String> = keywords.iter().map(|k| stem(k).to_string()).collect();
    for cid in corpus_ids {
        for name in atlas_entity_names(cid) {
            let nl = name.to_lowercase();
            for t in nl.split(|c: char| !c.is_alphanumeric()) {
                if t.len() >= 4 && kw_stems.iter().any(|s| s == stem(t)) {
                    return true;
                }
            }
        }
        for (desc, _) in atlas_atom_records(cid) {
            let dl = desc.to_lowercase();
            for w in dl.split(|c: char| !c.is_alphanumeric()) {
                if w.len() >= 4 && kw_stems.iter().any(|s| s == stem(w)) {
                    return true;
                }
            }
        }
    }
    false
}

/// Minimal suffix-stripping stem so "abandons"/"abandoned"/"abandon"
/// compare equal. Deliberately crude — it only needs to make keyword
/// overlap robust to inflection, not be linguistically right.
fn stem(word: &str) -> &str {
    for suf in ["ing", "ed", "es", "s"] {
        if word.len() > suf.len() + 3 {
            if let Some(stripped) = word.strip_suffix(suf) {
                return stripped;
            }
        }
    }
    word
}

// REMOVED (v8b, 2026-06-11): structural per-candidate chunk
// enumeration — one pipeline query per atlas person-entity for
// WHICH-questions (v5c). The full-bank A/B showed it net-HURTS:
// per-candidate chunks arrive in atlas order (the protagonist, with
// the most text, always first), splice into the high-attention
// region, and exhaust the append budget before the right candidate —
// the synthesizer then crowns whichever wrong candidate dominates
// (measured: Wurmt over Vladimir, Sir Ethelred as the Assistant
// Commissioner, Michaelis as the bomb-maker, Verloc as the explosion
// victim; distractor-evasion 1.00 → 0.00). Text volume tracks
// character prominence, which for WHICH-questions is an anti-signal.
// Atom matching below replaces it at the semantic layer, where the
// right candidate's ACTION is what's indexed.

/// All atom (description, passage_previews) pairs from a corpus's
/// atlas file — the raw material for both the lexical and the
/// semantic matchers below.
fn atlas_atom_records(corpus_id: &str) -> Vec<(String, Vec<String>)> {
    let Some(v) = cached_atoms_json(corpus_id) else {
        return Vec::new();
    };
    let atoms: &[serde_json::Value] = v
        .get("atoms")
        .and_then(|a| a.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    atoms
        .iter()
        .filter_map(|a| {
            let d = a.get("data")?;
            let desc = d
                .get("description")
                .or_else(|| d.get("statement"))
                .and_then(|s| s.as_str())?;
            if desc.is_empty() {
                return None;
            }
            let previews: Vec<String> = d
                .get("evidence")
                .and_then(|e| e.as_array())
                .map(|evs| {
                    evs.iter()
                        .filter_map(|e| e.get("passage_preview").and_then(|p| p.as_str()))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            Some((desc.to_string(), previews))
        })
        .collect()
}

/// Atlas atom records matching the question's content words. The
/// enrichment pipeline already did the hard part at ingest time —
/// event/relation atoms carry pronoun-resolved, single-sentence
/// statements of who did what ("X abandons Y by jumping off the
/// train…"), each with a supporting source passage. For a question
/// whose answer is an action, the atom IS the evidence; no chunk-rank
/// lottery required. Matching is plain stemmed keyword overlap in
/// code over the (small) atom file — no model call, no embedding.
/// Returns `(description, passage_previews, keyword_hits)` for atoms
/// with ≥2 distinct keyword hits, best first, capped at 4.
fn atlas_atom_matches(corpus_id: &str, keywords: &[String]) -> Vec<(String, Vec<String>, usize)> {
    if keywords.len() < 2 {
        return Vec::new();
    }
    // Keywords that are tokens of entity canonical names are weak
    // evidence — the protagonists' names co-occur in half the atoms
    // of a narrative corpus, so name-only overlap matches household
    // scenery, not the asked-about action (measured on the v7a probe:
    // "Winnie…Verloc" matched 3 dinner-table atoms for a murder
    // question). Require at least one ACTION-word hit, and rank by
    // action hits first.
    let entity_toks: HashSet<String> = atlas_entity_names(corpus_id)
        .iter()
        .flat_map(|n| n.split(|c: char| !c.is_alphanumeric()))
        .filter(|t| t.len() >= 4)
        .map(str::to_lowercase)
        .collect();
    let mut scored: Vec<(String, Vec<String>, usize, usize)> = Vec::new();
    for (desc, previews) in atlas_atom_records(corpus_id) {
        let dl = desc.to_lowercase();
        let dwords: Vec<&str> = dl.split(|c: char| !c.is_alphanumeric()).collect();
        let mut hits = 0usize;
        let mut action_hits = 0usize;
        for k in keywords {
            let s = stem(k);
            if dwords.iter().any(|w| stem(w) == s) {
                hits += 1;
                if !entity_toks.contains(k) {
                    action_hits += 1;
                }
            }
        }
        if hits < 2 || action_hits < 1 {
            continue;
        }
        scored.push((desc, previews, hits, action_hits));
    }
    scored.sort_by(|a, b| (b.3, b.2).cmp(&(a.3, a.2)));
    scored.truncate(3);
    scored.into_iter().map(|(d, p, h, _)| (d, p, h)).collect()
}

/// Distinct corpus ids present in a chunk set — the implicit scope
/// when the conversation carries no explicit corpus seal.
fn merged_corpora(chunks: &[corpus_engine::ScoredChunk]) -> HashSet<String> {
    chunks.iter().map(|c| c.corpus_id.clone()).collect()
}

/// `SOVEREIGN_AGENTIC_KQ=1` (or `true`) turns the loop on. Default off
/// so benches A/B cleanly and nothing changes for existing surfaces.
pub(crate) fn agentic_kq_enabled() -> bool {
    std::env::var("SOVEREIGN_AGENTIC_KQ")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Insufficiency probability above which the formulation round fires.
/// Env-tunable (`SOVEREIGN_AGENTIC_KQ_THRESHOLD`) for sweeps.
fn insufficiency_threshold() -> f64 {
    std::env::var("SOVEREIGN_AGENTIC_KQ_THRESHOLD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.5)
}

/// `SOVEREIGN_AGENTIC_KQ_DEBUG=1` mirrors the loop's tracing onto
/// stderr. Needed because bench/CLI surfaces don't install a tracing
/// subscriber (their idiom is eprintln, e.g. `[router]`/`[gv]` lines)
/// — without this the loop is invisible exactly where it's being
/// evaluated.
fn dbg(msg: &str) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let on = *ON.get_or_init(|| {
        std::env::var("SOVEREIGN_AGENTIC_KQ_DEBUG")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    });
    if on {
        eprintln!("    [agentic_kq] {msg}");
        // Mirror to tracing too: a detached daemon discards stderr, so the loop
        // was invisible in daemon.err. Default target (`sovereign_core::…`)
        // matches the daemon's crate-scoped filter. (2026-06-18 glassbox fix.)
        tracing::info!("[agentic_kq] {msg}");
    }
}

const MAX_FORMULATED_QUERIES: usize = 6;
const MAX_APPENDED_CHUNKS: usize = 12;
/// Evidence excerpt budget for the sufficiency prompt. Was 6×600 chars to stay
/// prefill-cheap, but that truncated answers deep in a chunk: measured 2026-06-18,
/// the science answer ("the sacrosanct fetish of to-day is science") sat at char
/// 1902 of a 2023-char chunk ranked 7th, so the judge saw a 600-char slice of the
/// top 6 chunks, never saw the answer, declared the evidence insufficient, and the
/// abstain note fired — even though full synthesis answered it correctly. The
/// judge must see the evidence it is judging. Defaults raised to 12×2000 (the full
/// round-0 set, full chunks); env-tunable so the accuracy↔prefill-throughput trade
/// can be swept without a rebuild. A more accurate verdict also avoids firing
/// round-2 (formulation + extra retrieval) when round-0 was already sufficient, so
/// the larger prefill is partly self-funding.
fn sufficiency_chunks() -> usize {
    std::env::var("SOVEREIGN_SUFFICIENCY_CHUNKS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(12)
}
fn sufficiency_chars_per_chunk() -> usize {
    std::env::var("SOVEREIGN_SUFFICIENCY_CHARS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2000)
}

impl Runtime {
    /// Run the bounded agentic round over the round-0 evidence.
    /// Returns the (possibly augmented) chunk set; on any judge or
    /// formulation failure it degrades to the input unchanged — the
    /// loop can only ADD evidence, never lose or reorder round 0.
    /// Returns `(chunks, still_insufficient, entity_anchored, corpus_anchored)`.
    ///
    /// `still_insufficient` — the loop fired, did its round, and the
    /// merged evidence STILL fails the sufficiency judge (or round 2
    /// found nothing new). The caller surfaces this to the synthesis
    /// prompt: a model that knows the targeted search came back empty
    /// abstains; one that doesn't treats the near-miss pile as
    /// license to answer. Measured 2026-06-11 full bank: the loop
    /// converted 3 absent-question abstentions into confident
    /// fabrications precisely because synthesis never learned that
    /// round 2 failed.
    ///
    /// `entity_anchored` — the question names entities from the
    /// corpus's own world (atlas gazetteer match), so "general
    /// knowledge" structurally cannot answer it; see
    /// `question_is_entity_anchored`.
    pub(crate) async fn agentic_evidence_round(
        &self,
        message: &str,
        chunks: Vec<corpus_engine::ScoredChunk>,
        context: &ConversationContext,
        intent: &Intent,
        scope: Option<&str>,
    ) -> (Vec<corpus_engine::ScoredChunk>, bool, bool, bool) {
        dbg(&format!(
            "agentic loop ENTERED: round0_chunks={} (judge next)",
            chunks.len()
        ));
        // Empty round 0 is the strongest possible insufficiency signal
        // — skip the judge and go straight to formulation.
        let insufficiency = if chunks.is_empty() {
            1.0
        } else {
            match self.judge_evidence_sufficiency(message, &chunks).await {
                Some(p) => p,
                None => {
                    // Fail-FORWARD: if we cannot confirm sufficiency, treat the
                    // evidence as insufficient and run round-2 — which only ADDS
                    // deduped evidence, never removes. The old default returned
                    // round-0 unchanged, so ANY judge flakiness silently disabled
                    // recall. Recall-safe default: when unsure, retrieve more.
                    tracing::warn!(
                        target: "agentic_kq",
                        "sufficiency judge failed — treating as INSUFFICIENT (fail-forward to round-2)"
                    );
                    dbg(
                        "agentic loop: sufficiency judge FAILED (None) → fail-forward, run round-2",
                    );
                    1.0
                }
            }
        };
        let threshold = insufficiency_threshold();
        tracing::info!(
            target: "agentic_kq",
            insufficiency = format!("{insufficiency:.3}").as_str(),
            threshold,
            round0_chunks = chunks.len(),
            fires = insufficiency >= threshold,
            "agentic_kq: sufficiency verdict"
        );
        dbg(&format!(
            "insufficiency={insufficiency:.3} threshold={threshold} round0={} fires={}",
            chunks.len(),
            insufficiency >= threshold
        ));
        if insufficiency < threshold {
            return (chunks, false, false, true);
        }

        // Corpora the atom-matching stage may read atlases from, and
        // the question's lexical view — both also feed the in-world
        // (entity-anchored) verdict the caller uses to pick the right
        // insufficiency note.
        let lookup_ids: Vec<String> = match context.conversation.enabled_corpora.as_deref() {
            // Intersect the user's selection with the principal-scoped
            // installed set, so a forged `enabled_corpora` can't widen
            // retrieval into a forbidden (another principal's Private) corpus.
            Some(ids) if !ids.is_empty() => {
                let scoped: std::collections::HashSet<&str> = context
                    .installed_corpora
                    .iter()
                    .map(String::as_str)
                    .collect();
                ids.iter()
                    .filter(|id| scoped.contains(id.as_str()))
                    .cloned()
                    .collect()
            }
            _ => merged_corpora(&chunks).into_iter().collect(),
        };
        let kw = question_keywords(message);
        let entity_anchored = question_is_entity_anchored(&kw, &lookup_ids);
        let corpus_anchored = entity_anchored || question_is_corpus_anchored(&kw, &lookup_ids);
        // Lexical-only matching, deliberately. A v10 probe tried a
        // cosine-similarity fallback (floor 0.5) for synonymy gaps and
        // it matched WRONG atoms at 0.52–0.59 — short-text embedding
        // similarity is dominated by entity-name overlap, not answer
        // relevance — and any nonzero match defeats the zero-atom skip
        // below, re-arming the fabrication path it exists to close.
        // Precision over recall here; the grounding gate downstream is
        // the recall safety net.
        let atom_matches: Vec<(String, Vec<(String, Vec<String>, usize)>)> = lookup_ids
            .iter()
            .map(|cid| (cid.clone(), atlas_atom_matches(cid, &kw)))
            .collect();
        let atom_count: usize = atom_matches.iter().map(|(_, m)| m.len()).sum();
        dbg(&format!(
            "entity_anchored={entity_anchored} atom_matches={atom_count}"
        ));

        // In-world question, and the semantic index has nothing for it:
        // an entity-anchored question whose content words match NO atlas
        // atom is, with high probability, asking for a fact the corpus
        // never states (the atlas catalogued every event/relation at
        // enrichment time). Round-2 chunk retrieval can only return
        // near-miss passages about the named entity — and the measured
        // effect of appending those (2026-06-11 full banks ×2) is
        // flipping honest abstentions into confident fabrications: the
        // near-miss pile reads as license to answer, and no prompt note
        // (generic OR in-world-strengthened) stopped a 4B from guessing
        // over it. So the append round is structurally SKIPPED — the
        // fabrication fuel never reaches the prompt, round-0 stands as
        // retrieved, and the caller's in-world note still tells the
        // model the targeted search found nothing. Cheaper too: no
        // formulation call, no per-query retrieval.
        if entity_anchored && atom_count == 0 {
            tracing::info!(
                target: "agentic_kq",
                "agentic_kq: in-world question with zero atlas-atom support — append round skipped"
            );
            dbg("in-world + zero atoms: append round skipped (anti-fabrication)");
            return (chunks, true, true, true);
        }

        let queries = match self
            .formulate_evidence_queries(message, &chunks, context)
            .await
        {
            Some(q) if !q.is_empty() => q,
            _ => {
                tracing::warn!(
                    target: "agentic_kq",
                    "query formulation failed/empty — keeping round-0 evidence unchanged"
                );
                // The loop FIRED (round 0 judged insufficient) and
                // produced nothing — synthesis should know.
                return (chunks, true, entity_anchored, corpus_anchored);
            }
        };
        tracing::info!(
            target: "agentic_kq",
            queries = ?queries,
            "agentic_kq: formulated round-2 queries"
        );
        dbg(&format!("queries={queries:?}"));

        // Dedupe key: stable row id when present, else corpus + content
        // head (synthetic chunks have no row id).
        let key = |c: &corpus_engine::ScoredChunk| -> (String, String) {
            match c.chunk_id {
                Some(id) => (c.corpus_id.clone(), id.to_string()),
                None => (c.corpus_id.clone(), c.content.chars().take(120).collect()),
            }
        };
        let mut seen: HashSet<(String, String)> = chunks.iter().map(&key).collect();

        // Hard seal for round-2 results. The upstream "seal" proved
        // score-soft (2026-06-11 probe: a sealed-to-one-novel
        // conversation accumulated Winnie-the-Pooh wikipedia chunks
        // because the formulated query scored high there). Round-2
        // additions are structurally restricted to the conversation's
        // enabled corpora; when no explicit seal exists, to the
        // corpora round 0 actually drew from — agency must widen the
        // EVIDENCE, never the corpus scope.
        let allowed: HashSet<String> = match context.conversation.enabled_corpora.as_deref() {
            // Same principal-scope intersection as `lookup_ids` above: a
            // forged `enabled_corpora` cannot widen round-2 evidence into a
            // forbidden corpus.
            Some(ids) if !ids.is_empty() => {
                let scoped: HashSet<&str> = context
                    .installed_corpora
                    .iter()
                    .map(String::as_str)
                    .collect();
                ids.iter()
                    .filter(|id| scoped.contains(id.as_str()))
                    .cloned()
                    .collect()
            }
            _ => merged_corpora(&chunks),
        };

        let mut merged = chunks;
        let mut appended_chunks: Vec<corpus_engine::ScoredChunk> = Vec::new();
        let mut appended = 0usize;

        // Atlas atom matches first — they carry pronoun-resolved
        // event/relation statements extracted at enrichment time, so
        // when one matches the question's content words it is usually
        // the single most decisive piece of evidence available (and
        // costs nothing: an in-code keyword scan over a small file).
        // NOTE: a v6 probe tried pooling per-candidate BM25 hits by
        // raw score instead; cross-query FTS scores are dominated by
        // each query's rarest term (IDF favours the rarest CANDIDATE,
        // not the right one) — same composition trap `ScoredChunk`'s
        // own docs warn about for cross-corpus scores. Don't revive it.
        for (cid, matches) in &atom_matches {
            for (desc, previews, hits) in matches {
                if appended >= MAX_APPENDED_CHUNKS {
                    break;
                }
                let title = merged
                    .iter()
                    .find(|c| &c.corpus_id == cid)
                    .and_then(|c| c.title.clone());
                let mut content = format!("Knowledge-atlas record for this corpus: {desc}");
                if !previews.is_empty() {
                    content.push_str(&format!(
                        " [supporting passage: \"{}\"]",
                        previews.join("\" / \"")
                    ));
                }
                let chunk = corpus_engine::ScoredChunk {
                    content,
                    title,
                    url: None,
                    corpus_id: cid.clone(),
                    score: 1.0,
                    metadata: std::collections::HashMap::from([(
                        "atlas_atom".to_string(),
                        "true".to_string(),
                    )]),
                    chunk_id: None,
                    source_doc_id: None,
                    vector_distance: None,
                };
                if seen.insert(key(&chunk)) {
                    dbg(&format!("atlas atom hits={hits} desc={desc:?}"));
                    tracing::info!(
                        target: "agentic_kq",
                        corpus = %cid,
                        keyword_hits = hits,
                        "agentic_kq: atlas atom match injected"
                    );
                    appended_chunks.push(chunk);
                    appended += 1;
                }
            }
        }

        for q in queries.iter().take(MAX_FORMULATED_QUERIES) {
            if appended >= MAX_APPENDED_CHUNKS {
                break;
            }
            let embedding = self.inference.embed_query(q).await.unwrap_or_default();
            if embedding.is_empty() {
                tracing::warn!(target: "agentic_kq", query = %q, "embed failed — skipping query");
                continue;
            }
            let mut state = PipelineState::new(
                q,
                context,
                intent,
                scope,
                embedding,
                "KnowledgeQuery",
                "KnowledgeQuery".to_string(),
            );
            kq_pipeline().run(self, &mut state).await;
            let mut new_for_query = 0usize;
            for c in state.chunks {
                if appended >= MAX_APPENDED_CHUNKS {
                    break;
                }
                if !allowed.is_empty() && !allowed.contains(&c.corpus_id) {
                    continue;
                }
                if seen.insert(key(&c)) {
                    appended_chunks.push(c);
                    appended += 1;
                    new_for_query += 1;
                }
            }
            tracing::info!(
                target: "agentic_kq",
                query = %q,
                new_chunks = new_for_query,
                "agentic_kq: round-2 query yield"
            );
            dbg(&format!("query={q:?} new_chunks={new_for_query}"));
        }
        // Priority placement: round-2 evidence splices in right after
        // the top-3 round-0 chunks instead of appending at the tail.
        // The prompt formatter serves the front of the set first and
        // the fast model reads early context most reliably — v2's
        // tail-appended murder-scene chunk was RETRIEVED but the model
        // still answered from general knowledge (position ~21 of 21,
        // past the char budget / attention cliff). The loop only fires
        // when round 0 was judged insufficient, so promoting round-2
        // evidence over mid-tier round-0 chunks is exactly the bet the
        // sufficiency verdict already made.
        let keep_front = merged.len().min(3);
        let tail = merged.split_off(keep_front);
        merged.extend(appended_chunks);
        merged.extend(tail);
        // Post-loop verdict: did round 2 actually fix the gap? Re-run
        // the same forced-choice judge over the merged front (which now
        // contains the appended evidence). One extra ~1s call, paid
        // only on fired turns; the verdict drives the caller's
        // insufficiency note to the synthesis prompt.
        let still_insufficient = if appended == 0 {
            true
        } else {
            match self.judge_evidence_sufficiency(message, &merged).await {
                Some(p) => p >= threshold,
                None => false,
            }
        };
        tracing::info!(
            target: "agentic_kq",
            appended,
            total = merged.len(),
            still_insufficient,
            "agentic_kq: evidence round complete"
        );
        dbg(&format!(
            "complete: appended={appended} total={} still_insufficient={still_insufficient}",
            merged.len()
        ));
        (merged, still_insufficient, entity_anchored, corpus_anchored)
    }

    /// Forced-choice logprob pass: P(evidence is INSUFFICIENT). Uses
    /// the `x_forced_choice` sentinel (one decoded token, distribution
    /// read off the masked logits) — deterministic, no sampled verdict.
    async fn judge_evidence_sufficiency(
        &self,
        message: &str,
        chunks: &[corpus_engine::ScoredChunk],
    ) -> Option<f64> {
        let excerpts: Vec<String> = chunks
            .iter()
            .take(sufficiency_chunks())
            .map(|c| {
                c.content
                    .chars()
                    .take(sufficiency_chars_per_chunk())
                    .collect()
            })
            .collect();
        let prompt = format!(
            "PASSAGES retrieved for a question:\n\"\"\"\n{}\n\"\"\"\n\n\
             QUESTION: {}\n\n\
             Do these passages contain the specific facts needed to answer the \
             question — the actual names, events, dates, or details it asks for? \
             Passages that are merely on the same topic, without the asked-for \
             facts, do NOT count.\n\n\
             Answer with exactly one letter — A = yes, the needed facts are \
             present; B = no, the needed facts are missing.",
            excerpts.join("\n---\n"),
            message.chars().take(400).collect::<String>(),
        );
        let req = CompletionRequest {
            prompt,
            system_message: Some(
                "You are a careful evidence auditor. Answer with a single letter.".into(),
            ),
            // PRIMARY tier, not Fast. The `x_forced_choice` logit-distribution
            // sentinel is NOT honored on the fast slot — it returns a sampled
            // token ("\"B") instead of the {A,B} distribution, so the parse below
            // fails and the loop silently degrades to round-0. This is the
            // documented "recall pinned at 2/8" failure: the judge never worked.
            // The gate's `forced_choice_ab` runs on primary for the same reason
            // (the fast slot's support distributions are squashed). One
            // prefill-dominated forced-choice per KQ is affordable for recall.
            preferred_speed: Speed::Medium,
            model_id: Some("primary".into()),
            max_tokens: Some(1),
            temperature: Some(0.0),
            think_budget: Some(0),
            enable_thinking: Some(false),
            structured_output: Some(serde_json::json!({
                "type": "string", "enum": ["A", "B"], "x_forced_choice": true
            })),
            ..Default::default()
        };
        let resp = match self.inference.complete(&req).await {
            Ok(r) => r,
            Err(e) => {
                dbg(&format!("sufficiency judge: complete() error: {e}"));
                return None;
            }
        };
        let dist: std::collections::HashMap<String, f64> =
            match serde_json::from_str(resp.text.trim()) {
                Ok(d) => d,
                Err(e) => {
                    dbg(&format!(
                        "sufficiency judge: parse error ({e}) on resp={:?}",
                        resp.text.chars().take(80).collect::<String>()
                    ));
                    return None;
                }
            };
        let a = dist.get("A").copied().unwrap_or(0.0);
        let b = dist.get("B").copied().unwrap_or(0.0);
        let denom = a + b;
        if denom <= 0.0 {
            dbg("sufficiency judge: A+B logprob mass is 0 → None");
            return None;
        }
        Some(b / denom)
    }

    /// Grammar-constrained formulation: 1–3 short search queries naming
    /// the specific entities/events the answer needs. Same fast-slot
    /// capability the InformationRequest flow already exercises.
    ///
    /// The prompt is grounded in the WORLD being searched — corpus
    /// titles from the round-0 chunks (e.g. the novel's title) plus
    /// the conversation's enabled corpora. Without this grounding the
    /// v1 prototype free-associated entities from the wrong universe
    /// entirely ("Winnie the Pooh money thief", "Annie Wilkes murder
    /// weapon" for questions about Conrad's The Secret Agent) — the
    /// formulator cannot know which world a bare question concerns.
    async fn formulate_evidence_queries(
        &self,
        message: &str,
        round0: &[corpus_engine::ScoredChunk],
        context: &ConversationContext,
    ) -> Option<Vec<String>> {
        let mut titles: Vec<String> = Vec::new();
        for c in round0 {
            if let Some(t) = c.title.as_deref() {
                let t = t.trim();
                if !t.is_empty() && !titles.iter().any(|x| x == t) {
                    titles.push(t.to_string());
                }
            }
            if titles.len() >= 6 {
                break;
            }
        }
        let corpora: Vec<String> = context
            .conversation
            .enabled_corpora
            .clone()
            .unwrap_or_default();
        // Atlas gazetteer: the canonical entity inventory for the
        // corpora in play. The formulator cannot search for an entity
        // it cannot NAME — "which anarchist takes Winnie's money" is
        // unformulable without knowing Ossipon exists. The atlas (when
        // built) knows every canonical entity; handing the inventory
        // to the formulator lets it enumerate candidates by name. This
        // is the designed composition point of the two retrieval
        // levers: atlas = indexes, agency = queries.
        let lookup_ids: Vec<String> = if corpora.is_empty() {
            merged_corpora(round0).into_iter().collect()
        } else {
            corpora.clone()
        };
        let mut entities: Vec<String> = Vec::new();
        if let Some(provider) = self.atlas_context_provider.as_ref() {
            for cid in &lookup_ids {
                if let Some(ctx) = provider.get(cid) {
                    for e in &ctx.entries {
                        if entities.len() >= 24 {
                            break;
                        }
                        if !entities.contains(&e.canonical_name) {
                            entities.push(e.canonical_name.clone());
                        }
                    }
                }
            }
        }
        // Fallback: read canonical entity NAMES straight from the
        // atlas atoms file. The provider above serves pre-EMBEDDED
        // contexts and its description-length/depth filter excludes
        // enrich-built literary atoms entirely (they carry no
        // description field) — but the gazetteer needs only names.
        // Prototype-scoped file read; the durable fix is a name-only
        // surface on AtlasContextProvider.
        if entities.is_empty() {
            for cid in &lookup_ids {
                for name in atlas_entity_names(cid) {
                    if entities.len() >= 24 {
                        break;
                    }
                    if !entities.contains(&name) {
                        entities.push(name);
                    }
                }
            }
        }
        let mut world = String::new();
        if !corpora.is_empty() {
            world.push_str(&format!(
                "Knowledge base being searched: {}.\n",
                corpora.join(", ")
            ));
        }
        if !titles.is_empty() {
            world.push_str(&format!(
                "Source documents seen so far: {}.\n",
                titles.join("; ")
            ));
        }
        if !entities.is_empty() {
            world.push_str(&format!(
                "Known people and things in this knowledge base: {}.\n",
                entities.join(", ")
            ));
        }
        let prompt = format!(
            "{}\nA question is being answered FROM THIS KNOWLEDGE BASE ONLY:\n\n{}\n\n\
             The first search, using the question's own wording, did not surface \
             the needed facts. Write 1-3 alternative search queries that name the \
             specific people, places, things, or events FROM THIS KNOWLEDGE BASE \
             whose details would answer the question. Stay inside the world of the \
             source documents named above — never introduce names from other books, \
             films, or topics. If the question asks WHICH person or thing did \
             something, you MUST write one query for EVERY plausible candidate \
             from the known entities above (up to 6), each pairing one \
             candidate's name with the asked-about action — enumerate, do not \
             bet on a single guess. Each \
             query is a few plain words — names and concrete terms only. No \
             punctuation, no quotes, no braces, no explanations.",
            world,
            message.chars().take(400).collect::<String>(),
        );
        let req = CompletionRequest {
            prompt,
            system_message: Some("You write precise search queries.".into()),
            preferred_speed: Speed::Fast,
            max_tokens: Some(160),
            temperature: Some(0.0),
            think_budget: Some(0),
            enable_thinking: Some(false),
            structured_output: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "queries": {
                        "type": "array",
                        "items": { "type": "string", "maxLength": 80 },
                        "minItems": 1,
                        "maxItems": 6
                    }
                },
                "required": ["queries"],
                "additionalProperties": false
            })),
            ..Default::default()
        };
        let resp = self.inference.complete(&req).await.ok()?;
        #[derive(serde::Deserialize)]
        struct Q {
            queries: Vec<String>,
        }
        let text = resp.text.trim();
        // Grammar guarantees shape on the happy path; the defensive
        // find('{') covers template tails around the JSON.
        let parsed: Q = serde_json::from_str(text)
            .or_else(|_| {
                let start = text.find('{').unwrap_or(0);
                serde_json::from_str(&text[start..])
            })
            .ok()?;
        // Sanitize: with a long entity-rich prompt the fast model
        // sometimes nests JSON-looking text INSIDE the string items
        // (grammar constrains the envelope, not the string contents).
        // Strip structural characters; drop items that were mostly
        // meta-narration rather than query terms.
        let queries: Vec<String> = parsed
            .queries
            .into_iter()
            .map(|q| {
                q.chars()
                    .map(|c| if "{}\"\\:".contains(c) { ' ' } else { c })
                    .collect::<String>()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|q| {
                !q.is_empty() && q.len() <= 80 && !q.to_lowercase().contains("searches for")
            })
            .collect();
        Some(queries)
    }
}

#[cfg(test)]
mod tests {
    use super::question_is_corpus_deictic;

    #[test]
    fn corpus_deictic_catches_story_year_class() {
        assert!(question_is_corpus_deictic("In what year is the story set?"));
        assert!(question_is_corpus_deictic(
            "What year is this document from?"
        ));
        assert!(question_is_corpus_deictic("Who wrote the novel?"));
        assert!(!question_is_corpus_deictic(
            "What is the capital of Canada?"
        ));
        assert!(!question_is_corpus_deictic(
            "How do I reverse a linked list in Python?"
        ));
    }

    /// The env gate must default OFF — existing surfaces and benches
    /// change behaviour only by explicit opt-in.
    #[test]
    fn agentic_kq_default_off() {
        std::env::remove_var("SOVEREIGN_AGENTIC_KQ");
        assert!(!super::agentic_kq_enabled());
    }
}
