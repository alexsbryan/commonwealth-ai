// SPDX-License-Identifier: AGPL-3.0-or-later
//! Structural "does the evidence anchor the question?" signals for the
//! agentic evidence loop (`super`): question keyword/stem extraction,
//! entity- and corpus-anchoring predicates, atlas-atom gazetteer matching,
//! and the grounding-gate predicates (`compute_entity_anchored`,
//! `retrieval_is_catalog_only`, `question_anchors_retrieved_title`,
//! `question_is_corpus_deictic`) that knowledge_query / streaming / the
//! handlers read via `crate::runtime::evidence_loop::*`.
//!
//! Split out of `evidence_loop.rs` (2026-07-13) for legibility and the
//! ARCH §3.1 file-size ceiling — a pure move, no behaviour change.

use corpus_engine_vocab::atoms::{AtomEnvelope, AtomsFile};
use std::collections::HashSet;

use super::dbg;

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
///
/// Parsed as the ONE `atoms.json` shape, [`AtomsFile`], since 2026-09-03 —
/// until then this was an untyped `serde_json::Value` walk that re-derived
/// the schema by key name (§10.6), tolerated a pre-v2 flat shape no writer
/// has produced since, and looked for a `statement` key no atom kind has
/// (`Claim` carries `content`), so it silently matched nothing there.
fn cached_atoms(corpus_id: &str) -> Option<std::sync::Arc<AtomsFile>> {
    use std::sync::{Arc, OnceLock, RwLock};
    use std::time::SystemTime;
    type Cache = RwLock<std::collections::HashMap<String, (SystemTime, Arc<AtomsFile>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(|| RwLock::new(std::collections::HashMap::new()));

    let path = sovereign_contracts::rebrand::data_dir()
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
    let value = Arc::new(serde_json::from_str::<AtomsFile>(&text).ok()?);
    if let Ok(mut map) = cache.write() {
        map.insert(corpus_id.to_string(), (mtime, Arc::clone(&value)));
    }
    Some(value)
}

/// Canonical entity names from a corpus's atlas atoms file
/// (`<data>/indexes/<corpus>/atlas/atoms.json`, via the mtime-keyed
/// [`cached_atoms`] cache). Best-effort: missing/garbled atlas → empty vec.
/// Used only as the gazetteer fallback when the embedded-context provider has no
/// entry for the corpus (see call site).
pub(crate) fn atlas_entity_names(corpus_id: &str) -> Vec<String> {
    let Some(file) = cached_atoms(corpus_id) else {
        return Vec::new();
    };
    // Borrow the shared cached file in place — no array clone (the pre-cache
    // version cloned the whole atoms array on every call). `canonical_name`
    // is an Entity (and Position) field; the untyped walk read it off any
    // atom's `data`, which for every other kind was absent.
    file.atoms
        .iter()
        .filter_map(|a| match a {
            AtomEnvelope::Entity(e) => Some(e.canonical_name.clone()),
            AtomEnvelope::Position(p) => Some(p.canonical_name.clone()),
            _ => None,
        })
        .collect()
}

/// Content words of the question: ≥4 chars, stop-filtered, lowercased,
/// first 6 distinct. The lexical view of the question that drives
/// atlas-atom matching.
pub(crate) fn question_keywords(message: &str) -> Vec<String> {
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
pub(crate) fn question_is_entity_anchored(keywords: &[String], corpus_ids: &[String]) -> bool {
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

/// Catalog-only retrieval: every retrieved chunk is a CATALOG metadata hit
/// (title/author/subject/year), with NO ingested full-text body behind it.
///
/// Such a turn can only draw on the metadata plus the model's parametric
/// memory — so a confident SPECIFIC ("the 1938 Washburn Ichabods went 0–7–0
/// under coach John J. Bowers") is exactly a GK fabrication the thin catalog
/// metadata cannot ground (observed gen-ceiling steps 373/519/535: an invented
/// coach, the wrong Ranji winner, non-existent 1946 Soviet opposition parties —
/// all shipped under an honest "based on general knowledge:" caveat). The
/// caveat is the same "fabrication in honest clothing" shape the gazetteer
/// entity-anchor check was built to close (`question_is_entity_anchored`), but
/// catalog corpora carry NO atlas, so that check returns false and the
/// exemption stays open. Treat catalog-only retrieval as entity-anchored so the
/// gate strips the caveat and verifies the specific — finding no support in the
/// metadata, it abstains with the ingest offer instead of shipping the guess.
///
/// Empty retrieval is deliberately NOT catalog-only: the zero-chunk structural
/// GK-caveat path owns that, and `gate_on` requires `documents_found > 0`
/// anyway. A MIXED turn (some full-text) is not catalog-only either — the body
/// chunks can ground a real answer, so the honest GK path stays open there.
pub(crate) fn retrieval_is_catalog_only(
    chunks: &[corpus_engine::ScoredChunk],
    kinds: &std::collections::HashMap<String, corpus_engine::CorpusKind>,
) -> bool {
    !chunks.is_empty()
        && chunks
            .iter()
            .all(|c| kinds.get(&c.corpus_id) == Some(&corpus_engine::CorpusKind::Catalog))
}

/// Retrieval-derived entity anchor: does the question name a SPECIFIC
/// entity that one retrieved chunk's TITLE identifies? A per-turn anchor
/// that complements the atlas gazetteer (`question_is_entity_anchored`)
/// and the catalog-only net (`retrieval_is_catalog_only`).
///
/// It fires on MIXED or full-text-miss retrieval — the gap the catalog-only
/// net leaves open. `retrieval_is_catalog_only` requires EVERY chunk be a
/// catalog hit, so a single tangential full-text chunk (a "Darlington"
/// geography article, a "by-election" explainer) riding along with the
/// catalog title disables it, and the atlas has no such obscure entity, so
/// `question_is_entity_anchored` is false too — both nets miss and a
/// GK-caveated confident specific ships (measured 2026-07-13, validation
/// step 179: "Who won the 1926 Darlington by-election?" retrieved the
/// catalog title "1926 Darlington by-election" yet answered "from general
/// knowledge: … Robert Gascoyne-Cecil … Conservative Party" — the real
/// winner was Labour). When the entity the question asks about lives in a
/// retrieved TITLE but no ingested body grounds the specific, treat the
/// question as entity-anchored so the gate strips the caveat, verifies the
/// specific, and abstains rather than launder the guess.
///
/// The match is deliberately TIGHT so it cannot over-gate a grounded
/// answer: a title anchors the question only when it carries ≥2 significant
/// words (`MIN_TITLE_SIG`) AND ≥70% of them appear (stemmed) in the
/// question (`TITLE_MATCH_FLOOR`). That rejects the generic-word coattail —
/// in step 179 twelve distractor election titles ("1866 New Brunswick
/// general election", …) share only "election" (≤1/4 of their words) and
/// correctly do NOT anchor, while "1926 Darlington by-election" (3/3 words
/// present in the question) does. Anchoring only changes behaviour when the
/// specific is UNgrounded: a grounded answer still passes `verify_grounding`
/// because the body supplies the support.
pub(crate) fn question_anchors_retrieved_title(
    message: &str,
    chunks: &[corpus_engine::ScoredChunk],
) -> bool {
    const MIN_TITLE_SIG: usize = 2;
    const TITLE_MATCH_FLOOR: f32 = 0.70;
    // Full stemmed content-word set of the question. Deliberately NOT the
    // capped-6 `question_keywords` — a specific title's words can fall past
    // that cap in a long question, and here we want maximal recall on the
    // question side, paced by the tight title-side floor below.
    let q_stems: HashSet<String> = message
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 4)
        .map(|t| stem(&t.to_lowercase()).to_string())
        .collect();
    if q_stems.is_empty() {
        return false;
    }
    for c in chunks {
        let Some(title) = c.title.as_deref() else {
            continue;
        };
        let title_sig: Vec<String> = title
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| t.len() >= 4)
            .map(|t| stem(&t.to_lowercase()).to_string())
            .collect();
        if title_sig.len() < MIN_TITLE_SIG {
            continue;
        }
        let present = title_sig.iter().filter(|s| q_stems.contains(*s)).count();
        let frac = present as f32 / title_sig.len() as f32;
        if frac >= TITLE_MATCH_FLOOR {
            dbg(&format!(
                "title_anchor: q_stems_n={} title={title:?} matched={present}/{} frac={frac:.2} HIT",
                q_stems.len(),
                title_sig.len()
            ));
            return true;
        }
    }
    false
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
pub(crate) fn question_is_corpus_anchored(keywords: &[String], corpus_ids: &[String]) -> bool {
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
    let Some(file) = cached_atoms(corpus_id) else {
        return Vec::new();
    };
    // The kinds that carry a prose `description`: Entity, Event,
    // Configuration. (The untyped walk also asked for `statement`; no kind
    // has one — see `cached_atoms`.) Previews come from the envelope's own
    // evidence accessor; an Entity has no evidence list, so its previews are
    // empty exactly as before.
    file.atoms
        .iter()
        .filter_map(|a| {
            let desc = match a {
                AtomEnvelope::Entity(e) => e.description.as_str(),
                AtomEnvelope::Event(e) => e.description.as_str(),
                AtomEnvelope::Configuration(c) => c.description.as_str(),
                _ => return None,
            };
            if desc.is_empty() {
                return None;
            }
            let previews: Vec<String> = a
                .evidence()
                .into_iter()
                .filter_map(|c| c.passage_preview.clone())
                .collect();
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
pub(crate) fn atlas_atom_matches(
    corpus_id: &str,
    keywords: &[String],
) -> Vec<(String, Vec<String>, usize)> {
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
pub(crate) fn merged_corpora(chunks: &[corpus_engine::ScoredChunk]) -> HashSet<String> {
    chunks.iter().map(|c| c.corpus_id.clone()).collect()
}
