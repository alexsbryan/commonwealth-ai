// SPDX-License-Identifier: AGPL-3.0-or-later
//! Question-shape analysis and post-merge chunk shaping.
//!
//! Two families of helpers:
//!
//! 1. **Question parsing** — `extract_question_entities`,
//!    `extract_comparison_entities`, `comparison_axis`,
//!    `extract_commitment_phrase`, `MetalingualLocator` +
//!    `parse_metalingual_locator`. Pure string heuristics over the
//!    user's question that the retrieval planner consults to widen
//!    entity-anchored search, decompose comparisons, or pick a
//!    metalingual locator.
//!
//! 2. **Per-article / per-entity chunk shaping** — `cap_chunks_per_article`
//!    + the `section_from_url` helper it uses, and `reserve_chunks_per_entity`.
//!    Run on the merged top-K to enforce article diversity and to
//!    guarantee shelf space for both sides of a comparison query.

use corpus_engine::ScoredChunk;

/// Per-section chunk cap inside the per-article cap. 4 keeps a
/// fact-rich Wikipedia article from filling all its slots with one
/// or two sections — observed on
/// `contested_atomic_bombings_morality`, where the abstract +
/// "Air raids on Japan" sections claimed every article slot and
/// left zero room for the pro/con debate sections where the
/// actual arguments live.
const MAX_CHUNKS_PER_SECTION_AT_MERGE: usize = 4;

/// Comparison-aware entity extraction. Pulls the two contrasted
/// noun phrases from a comparison-shape question, including the
/// lowercase case ("special relativity vs general relativity")
/// that [`extract_question_entities`] misses by design — its
/// proper-noun heuristic skips lowercase tokens.
///
/// Patterns handled (in order):
/// - "between X and Y" — X and Y are the slots between
///   "between"/"and" and "and"/sentence boundary.
/// - "X and Y differ" / "X vs Y" — X/Y are parallel-length noun
///   phrases bracketing the comparison signal, with leading
///   wh-words / aux verbs stripped from X.
///
/// Falls back to the proper-noun extractor when no pattern matches
/// — questions like "Compare Marie Curie and Lise Meitner" already
/// work via the proper-noun path, so we only need this helper for
/// the cases that path misses.
pub(crate) fn extract_comparison_entities(text: &str) -> Vec<String> {
    const STOP_PREFIX: &[&str] = &[
        "how", "what", "when", "where", "why", "who", "which", "do", "did", "does", "is", "are",
        "was", "were", "compare", "contrast", "describe", "explain", "the", "a", "an",
    ];
    let trim_word = |w: &str| -> String {
        w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
            .trim_end_matches("'s")
            .trim_end_matches('\'')
            .to_string()
    };
    let strip_lead_stops = |words: &[&str]| -> Vec<String> {
        let mut out: Vec<String> = words
            .iter()
            .map(|w| trim_word(w))
            .filter(|w| !w.is_empty())
            .collect();
        while !out.is_empty() && STOP_PREFIX.iter().any(|s| out[0].eq_ignore_ascii_case(s)) {
            out.remove(0);
        }
        out
    };

    // ── Pattern A: "between X and Y" (covers "the difference between
    //    special relativity and general relativity?"). Both X and Y
    //    can be lowercase noun phrases — that's the whole point.
    let lower = text.to_lowercase();
    if let Some(b_start) = lower.find("between ") {
        // Only fire if "between" is preceded by whitespace or sentence
        // start — avoid grabbing inside a longer word.
        let preceded_ok = b_start == 0
            || lower
                .as_bytes()
                .get(b_start - 1)
                .map(|b| b.is_ascii_whitespace())
                .unwrap_or(false);
        if preceded_ok {
            let after = &text[b_start + "between ".len()..];
            // Find the first " and " in `after`.
            let after_lower = after.to_lowercase();
            if let Some(a_pos) = after_lower.find(" and ") {
                let x_part = &after[..a_pos];
                let after_and = &after[a_pos + " and ".len()..];
                // Y ends at the first sentence-terminator or
                // contrast-suffix ("?", ".", ",", " differ",
                // " in their"). Lowercase scan.
                let after_and_lower = after_and.to_lowercase();
                let mut y_end = after_and.len();
                for needle in &["?", ".", ",", " differ", " in their", " regarding"] {
                    if let Some(p) = after_and_lower.find(needle) {
                        y_end = y_end.min(p);
                    }
                }
                let y_part = &after_and[..y_end];
                let x: String =
                    strip_lead_stops(&x_part.split_whitespace().collect::<Vec<_>>()).join(" ");
                let y: String =
                    strip_lead_stops(&y_part.split_whitespace().collect::<Vec<_>>()).join(" ");
                if !x.is_empty() && !y.is_empty() {
                    let mut out = vec![x.clone(), y.clone()];
                    if x.eq_ignore_ascii_case(&y) {
                        out.pop();
                    }
                    return out;
                }
            }
        }
    }

    // ── Pattern B: "X and Y differ" / "X and Y differs" — X and Y
    //    bracket " and " with X to the left of " and " and Y between
    //    " and " and " differ". Use parallel-length extraction so
    //    "How do Einstein's and Newton's conceptions of gravity differ"
    //    produces ["Einstein", "Newton"] rather than dragging the
    //    "conceptions of gravity" tail into Y.
    if let Some(diff_pos) = lower.find(" differ") {
        let before_differ = &text[..diff_pos];
        let bd_lower = before_differ.to_lowercase();
        if let Some(a_pos) = bd_lower.rfind(" and ") {
            let before_and = &before_differ[..a_pos];
            let after_and = &before_differ[a_pos + " and ".len()..];
            let bef_words: Vec<&str> = before_and.split_whitespace().collect();
            let x_words = strip_lead_stops(&bef_words);
            // Take parallel length: |Y| = |X|, so we don't grab
            // post-modifying noun phrases.
            let aft_words: Vec<&str> = after_and.split_whitespace().collect();
            let take = x_words.len().min(aft_words.len()).max(1);
            let y_words: Vec<String> = aft_words
                .iter()
                .take(take)
                .map(|w| trim_word(w))
                .filter(|w| !w.is_empty())
                .collect();
            if !x_words.is_empty() && !y_words.is_empty() {
                return vec![x_words.join(" "), y_words.join(" ")];
            }
        }
    }

    // ── Pattern C: " vs " / " versus " — split on the separator and
    //    take the parallel-length noun phrase on each side, leading
    //    stop words stripped from the X side.
    for sep in [" vs ", " vs.", " versus "] {
        if let Some(pos) = lower.find(sep) {
            let x_part = &text[..pos];
            let y_part = &text[pos + sep.len()..];
            let x_words = strip_lead_stops(&x_part.split_whitespace().collect::<Vec<_>>());
            let aft_words: Vec<&str> = y_part.split_whitespace().collect();
            let take = x_words.len().min(aft_words.len()).max(1);
            let y_words: Vec<String> = aft_words
                .iter()
                .take(take)
                .map(|w| trim_word(w))
                .filter(|w| !w.is_empty())
                .collect();
            if !x_words.is_empty() && !y_words.is_empty() {
                return vec![x_words.join(" "), y_words.join(" ")];
            }
        }
    }

    // Fallback: proper-noun extractor handles "Compare X and Y" etc.
    extract_question_entities(text)
}

/// For a comparison-shaped question with known entities, extract
/// the *axis* — the noun phrase the entities are being compared on.
/// Used by the heuristic decomposer to build per-entity sub-queries
/// like `["Buddhism compassion", "Christianity compassion"]`.
///
/// Strategy: locate a comparison-axis cue (`differ in`, `differ on`,
/// `regarding`, `concepts of`, `views on`, etc.) and lift the noun
/// phrase that follows. Strip stopwords and entity-name tokens so
/// the axis stays sharp. Returns `None` when no cue fires — the
/// caller then declines to decompose.
pub(crate) fn comparison_axis(text: &str, entities: &[String]) -> Option<String> {
    const CUES: &[&str] = &[
        " differ in their ",
        " differ on ",
        " differ in ",
        " differ regarding ",
        " regarding ",
        " in their conceptions of ",
        " in their concepts of ",
        " in their views on ",
        " in their treatment of ",
        " on their ",
        " about ",
        " concerning ",
        " in terms of ",
        " with respect to ",
    ];
    let lower = text.to_lowercase();
    let mut after_idx: Option<usize> = None;
    for cue in CUES {
        if let Some(p) = lower.find(cue) {
            let candidate = p + cue.len();
            if after_idx.map(|i| candidate < i).unwrap_or(true) {
                after_idx = Some(candidate);
            }
        }
    }
    let start = after_idx?;
    let tail = &text[start..];

    // Tail ends at the first terminal punctuation.
    let tail_end = tail.find(['?', '.', ',', ';', '!']).unwrap_or(tail.len());
    let span = &tail[..tail_end];

    // Strip lead-side filler words — "the X", "a Y", "their Z".
    const LEAD_DROP: &[&str] = &[
        "the",
        "a",
        "an",
        "their",
        "his",
        "her",
        "its",
        "concept",
        "concepts",
        "conception",
        "conceptions",
        "view",
        "views",
        "treatment",
        "treatments",
        "notion",
        "notions",
        "idea",
        "ideas",
        "of",
        "on",
        "about",
    ];
    let entity_lowers: Vec<String> = entities.iter().map(|e| e.to_lowercase()).collect();
    let words: Vec<String> = span
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
                .to_string()
        })
        .filter(|w| !w.is_empty())
        .collect();
    let mut filtered: Vec<String> = Vec::new();
    let mut leading = true;
    for w in &words {
        let wl = w.to_lowercase();
        // Drop entity tokens — the axis is what's compared, not the
        // entities themselves.
        if entity_lowers.iter().any(|e| e == &wl) {
            continue;
        }
        if leading && LEAD_DROP.iter().any(|d| *d == wl) {
            continue;
        }
        leading = false;
        filtered.push(w.clone());
    }
    // Cap to 4 words — anything longer is a verbose qualifier the
    // retrieval scorer doesn't benefit from.
    filtered.truncate(4);
    let axis = filtered.join(" ").trim().to_string();
    if axis.is_empty() {
        return None;
    }
    Some(axis)
}

/// Extract proper-noun entities from the question for entity-boost
/// retrieval.
///
/// Heuristics:
/// - Skips sentence-initial capitalised words (grammar, not entity).
/// - Skips a leading-token stop list of wh-words and verbs that often
///   appear capitalised at start (`How`, `What`, `Compare`, ...).
/// - Groups consecutive capitalised tokens into multi-word phrases
///   (`Industrial Revolution`, `Marie Curie`, `Yalta Conference`).
/// - Strips trailing possessives (`Einstein's` → `Einstein`).
/// - Dedupes while preserving order.
///
/// False positives are cheap (a search for `Allied` returns no
/// high-relevance hits and the noise floor drops them); false
/// negatives miss the entity-rich articles that question-named
/// entities almost always have. Tune toward catching too many.
pub(crate) fn extract_question_entities(text: &str) -> Vec<String> {
    const SKIP_LEAD: &[&str] = &[
        "How",
        "What",
        "When",
        "Where",
        "Why",
        "Who",
        "Which",
        "Compare",
        "Contrast",
        "Describe",
        "Explain",
        "Tell",
        "Give",
        "List",
        "Discuss",
        "Summarize",
        "Show",
        "Did",
        "Does",
        "Do",
        "Is",
        "Are",
        "Was",
        "Were",
    ];
    let mut out: Vec<String> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut at_sentence_start = true;
    for word in text.split_whitespace() {
        let trimmed = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'' && c != '-');
        let starts_upper = trimmed
            .chars()
            .next()
            .map(|c| c.is_uppercase())
            .unwrap_or(false);
        let is_skip = SKIP_LEAD.contains(&trimmed);
        if starts_upper && !at_sentence_start && !is_skip {
            let clean = trimmed
                .trim_end_matches("'s")
                .trim_end_matches('\'')
                .to_string();
            if !clean.is_empty() {
                current.push(clean);
            }
        } else {
            if !current.is_empty() {
                out.push(current.join(" "));
                current.clear();
            }
        }
        let last_char = word.chars().last();
        at_sentence_start = matches!(last_char, Some('.') | Some('!') | Some('?'));
    }
    if !current.is_empty() {
        out.push(current.join(" "));
    }
    let mut seen = std::collections::HashSet::new();
    out.into_iter().filter(|s| seen.insert(s.clone())).collect()
}

/// Extract the commitment phrase from a commissive message — the
/// noun-verb clause following the marker. Best-effort: if no marker
/// is found, returns `None` and the caller falls back to the full
/// trimmed message.
pub(crate) fn extract_commitment_phrase(message: &str) -> Option<String> {
    let lower = message.to_lowercase();
    const MARKERS: &[&str] = &[
        "i'll ",
        "i will ",
        "i'm going to ",
        "i am going to ",
        "i'm gonna ",
        "i plan to ",
        "i'll be ",
        "remind me to ",
        "remind me about ",
        "remind me later to ",
        "remind me on ",
        "remind me in ",
    ];
    for marker in MARKERS {
        if let Some(pos) = lower.find(marker) {
            let after = &message[pos + marker.len()..];
            // Cap at sentence boundary to avoid dragging in unrelated trailing context.
            let end = after.find(['.', '!', '?', '\n']).unwrap_or(after.len());
            let phrase = after[..end].trim();
            if !phrase.is_empty() {
                return Some(phrase.to_string());
            }
        }
    }
    None
}

/// Metalingual locator — what kind of source-anchor the question
/// references. Drives which corpora the metalingual handler filters
/// retrieval to. Inferred heuristically from the message; the
/// `Ambient` and `Unknown` variants exist so the handler can degrade
/// gracefully when the parser can't pin down the locator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MetalingualLocator {
    /// "in this codebase / repo / project / sovereign" — internal
    /// system code.
    SystemCode,
    /// "earlier", "we mentioned", "you said" — internal conversation.
    Conversation,
    /// "according to <X>", "per <X>", "<X> defines" — captures the
    /// named source string for case-insensitive corpus_id / display
    /// name match downstream.
    NamedSource(String),
    /// "here" / "this" with definitional context — best handled by
    /// resolving from active conversation context (anchored doc,
    /// recently-discussed corpus).
    Ambient,
    /// Heuristic fired but no specific locator extracted — fall back
    /// to broadest internal-source set.
    Unknown,
}

/// Coarse label the router stamps when the LITERAL markers below
/// committed the route (Pre-check -3).
pub(crate) const COARSE_CONVERSATION_LOCATOR_DIRECT: &str = "CONVERSATION_LOCATOR_DIRECT";

/// Coarse label the router stamps when the SEMANTIC locator axis
/// committed the route (Pre-check -2.5, `EmbedRouter::locator_from_embedding`).
pub(crate) const COARSE_CONVERSATION_LOCATOR_EMBED: &str = "CONVERSATION_LOCATOR_EMBED";

/// Coarse label the router stamps when the ARCHIVE axis committed the
/// route (Pre-check -2.4,
/// `ConversationArchiveClassifier::classify_from_embedding`).
///
/// Deliberately NOT part of [`locator_hint_from_coarse`]: an archive
/// question is routed AWAY from the metalingual handler entirely, to
/// `KnowledgeQuery` over the user's personal corpora. Mapping it to a
/// [`MetalingualLocator`] would reintroduce the exact dead end this
/// axis exists to fix.
pub(crate) const COARSE_CONVERSATION_ARCHIVE_EMBED: &str = "CONVERSATION_ARCHIVE_EMBED";

/// Recover the locator the ROUTER committed on, from the coarse label
/// it stamped on the classification.
///
/// Exists because the metalingual handler used to re-derive the
/// locator by calling [`parse_metalingual_locator`] on the raw message
/// — which is fine when the string markers are what routed the turn,
/// and silently wrong when the semantic axis did: the router would
/// commit "this is about our conversation", then the handler would
/// re-parse, find no marker, fall through to `Ambient`, and go
/// searching corpora for an answer that was in the message list. The
/// routing decision is made once and travels; it is not re-guessed
/// downstream from the same evidence.
pub(crate) fn locator_hint_from_coarse(coarse: Option<&str>) -> Option<MetalingualLocator> {
    match coarse {
        Some(COARSE_CONVERSATION_LOCATOR_DIRECT) | Some(COARSE_CONVERSATION_LOCATOR_EMBED) => {
            Some(MetalingualLocator::Conversation)
        }
        _ => None,
    }
}

/// Parse the metalingual locator from a message. Mirrors the heuristic
/// in [`crate::router::LlmRouter::looks_like_metalingual`] — same families, but here
/// we record *which* family fired so the handler can resolve to the
/// right source set.
pub(crate) fn parse_metalingual_locator(message: &str) -> MetalingualLocator {
    let lower = message.to_lowercase();

    // 1. NamedSource — "according to <name>", "per <name>", "<name>
    //    defines / says / uses". Capture the name token(s) after the
    //    anchor preposition; cap at 3 words so we don't drag the rest
    //    of the sentence in.
    let named_anchors: &[&str] = &["according to ", " per "];
    for anchor in named_anchors {
        if let Some(pos) = lower.find(anchor) {
            // Use original-case `message` for the captured name so
            // proper-noun corpora (SEP, Wikipedia) survive lookup.
            let after = &message[pos + anchor.len()..];
            // Take up to 3 words, stop at common terminators.
            let mut name_words: Vec<&str> = Vec::new();
            for w in after.split_whitespace() {
                let cleaned =
                    w.trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_');
                if cleaned.is_empty() {
                    break;
                }
                let cleaned_lower = cleaned.to_lowercase();
                // Stop on filler / definitional verbs / clause boundaries.
                if matches!(
                    cleaned_lower.as_str(),
                    "what"
                        | "how"
                        | "the"
                        | "a"
                        | "an"
                        | "is"
                        | "are"
                        | "does"
                        | "do"
                        | "did"
                        | "mean"
                        | "means"
                        | "say"
                        | "says"
                        | "define"
                        | "defines"
                        | "use"
                        | "uses"
                ) {
                    break;
                }
                name_words.push(cleaned);
                if name_words.len() >= 3 {
                    break;
                }
            }
            if !name_words.is_empty() {
                return MetalingualLocator::NamedSource(name_words.join(" "));
            }
        }
    }

    // 2. SystemCode — explicit codebase/system locators.
    const SYSTEM_MARKERS: &[&str] = &[
        "in this codebase",
        "in this repo",
        "in this repository",
        "in this project",
        "in this code",
        "in our codebase",
        "in our repo",
        "in our system",
        "in the codebase",
        "in the repo",
        "in sovereign",
        "in the sovereign",
    ];
    if SYSTEM_MARKERS.iter().any(|m| lower.contains(m)) {
        return MetalingualLocator::SystemCode;
    }

    // 3. Conversation — internal thread references.
    const CONVERSATION_MARKERS: &[&str] = &[
        "in this conversation",
        "in our conversation",
        "earlier you said",
        "earlier i said",
        "we mentioned",
        "we discussed",
        "we talked about",
        "you mentioned",
        "you said",
    ];
    if CONVERSATION_MARKERS.iter().any(|m| lower.contains(m)) {
        return MetalingualLocator::Conversation;
    }

    // 4. Ambient ("here" / "this" + definitional) — handled at the
    //    heuristic level; if we got here, it's the residual case.
    if lower.contains(" here") || lower.contains(" this") {
        return MetalingualLocator::Ambient;
    }

    MetalingualLocator::Unknown
}

/// Cap chunks per `(corpus_id, title)` group to enforce article
/// diversity in the merged top-K.
///
/// Walks chunks in input order — callers pass score-sorted order so
/// the first `max_per_article` per group are the highest-scoring
/// within their article. Drops the rest. This runs before
/// `truncate(KQ_MERGED_LIMIT)` so a query that hits one article
/// densely (Wikipedia's main subject article filling 10/20 hybrid-
/// search slots, or an SEP entry on the question's exact philosophical
/// angle) doesn't crowd out the other articles that appeared further
/// down. The multi-source expander downstream tops top groups back to
/// `EXPANSION_MULTI_PER_SOURCE` (4) where depth actually matters.
///
/// Within the per-article cap, also enforces a per-section
/// (`MAX_CHUNKS_PER_SECTION_AT_MERGE`) sub-cap derived from the URL
/// fragment (the `#Section_name` anchor on a Wikipedia/SEP URL).
/// Without this, a question whose exact phrasing pattern-matches the
/// article's overview/abstract section can fill all 5 article slots
/// with chunks from one or two sections (we observed this on
/// `contested_atomic_bombings_morality`: 5 article slots all filled
/// with `#Abstract` + `#Air_raids_on_Japan` chunks, leaving zero room
/// for the `#Debate_over_bombings` and `#Soviet_entry` sections where
/// the actual pro/con arguments live). Section-aware capping forces
/// distribution across sections inside a fact-rich article.
pub(crate) fn cap_chunks_per_article(
    chunks: Vec<ScoredChunk>,
    max_per_article: usize,
) -> Vec<ScoredChunk> {
    use std::collections::HashMap;
    let mut article_counts: HashMap<(String, String), usize> = HashMap::new();
    let mut section_counts: HashMap<(String, String, String), usize> = HashMap::new();
    let mut out = Vec::with_capacity(chunks.len());
    for c in chunks {
        // Atom-directed and RAPTOR chunks bypass the anti-flood cap. The
        // cap exists to stop one *organically*-retrieved article from
        // dominating the merge; these two sets are intentional, upstream-
        // bounded selections directed by the structural layers, not organic
        // retrieval:
        //   - atom-enum: one-chunk-per-entity, atlas-directed, bounded by
        //     SOVEREIGN_ATOM_ENUM_TOPK.
        //   - raptor: top-M whole-document summaries, collapsed-tree-
        //     directed, bounded by SOVEREIGN_RAPTOR_TOP_M.
        // Capping either by (corpus_id, title) would silently drop exactly
        // the directed evidence — a RAPTOR summary carries title=<slug> and
        // corpus_id=<corpus>, so it collides with the article's own leaf
        // chunks (and the no-fragment section sub-cap) and, scoring lower
        // than a query-term-dense leaf, loses its slot to its own leaves.
        // That is the precise summary we injected it to surface. Tagged in
        // `enumerate_typed_atom_chunks` / `apply_raptor_grounding`.
        if c.metadata
            .get("source")
            .map(|s| s == "atom-enum" || s == "raptor")
            .unwrap_or(false)
        {
            out.push(c);
            continue;
        }
        let title = c.title.as_deref().unwrap_or("").to_string();
        let section = section_from_url(c.url.as_deref());
        let article_key = (c.corpus_id.clone(), title.clone());
        let section_key = (c.corpus_id.clone(), title, section);
        let article_n = *article_counts.get(&article_key).unwrap_or(&0);
        let section_n = *section_counts.get(&section_key).unwrap_or(&0);
        if article_n >= max_per_article || section_n >= MAX_CHUNKS_PER_SECTION_AT_MERGE {
            continue;
        }
        *article_counts.entry(article_key).or_insert(0) += 1;
        *section_counts.entry(section_key).or_insert(0) += 1;
        out.push(c);
    }
    out
}

/// Pull the section anchor out of a Wikipedia/SEP/etc. URL —
/// everything after the first `#`. Empty string when there's no
/// fragment (i.e. the article overview / no specific section).
/// Treating the no-fragment case as its own bucket means the
/// abstract chunks share the section sub-cap with each other but
/// don't compete with named sections, which is the intended behavior.
fn section_from_url(url: Option<&str>) -> String {
    url.and_then(|u| u.split_once('#').map(|(_, frag)| frag.to_string()))
        .unwrap_or_default()
}

/// Move up to `per_entity_reserve` chunks per entity to the front of
/// the score-sorted merge so they survive the downstream truncation.
/// Chunks are matched to an entity by case-insensitive title-contains.
///
/// Reserved chunks keep their relative score order (the highest-
/// scoring entity-titled chunks for entity X come before lower-scoring
/// ones for entity X), and the non-reserved tail stays in score order
/// behind them. The net effect: for ComparisonQuery, `KQ_MERGED_LIMIT`
/// truncate cannot drop a Newton-side chunk just because Einstein's
/// chunks ranked higher — both sides are guaranteed shelf space.
///
/// No-op when `entities` is empty.
pub(crate) fn reserve_chunks_per_entity(
    chunks: Vec<ScoredChunk>,
    entities: &[String],
    per_entity_reserve: usize,
) -> Vec<ScoredChunk> {
    if entities.is_empty() || per_entity_reserve == 0 {
        return chunks;
    }
    use std::collections::HashSet;
    let entity_lowers: Vec<String> = entities.iter().map(|e| e.to_lowercase()).collect();
    let mut reserved_idx: HashSet<usize> = HashSet::new();
    for entity_lower in &entity_lowers {
        let mut taken = 0usize;
        for (i, c) in chunks.iter().enumerate() {
            if reserved_idx.contains(&i) {
                continue;
            }
            let title_lower = c
                .title
                .as_deref()
                .map(str::to_lowercase)
                .unwrap_or_default();
            if title_lower.contains(entity_lower) {
                reserved_idx.insert(i);
                taken += 1;
                if taken >= per_entity_reserve {
                    break;
                }
            }
        }
    }
    let mut reserved: Vec<ScoredChunk> = Vec::with_capacity(reserved_idx.len());
    let mut rest: Vec<ScoredChunk> =
        Vec::with_capacity(chunks.len().saturating_sub(reserved_idx.len()));
    for (i, c) in chunks.into_iter().enumerate() {
        if reserved_idx.contains(&i) {
            reserved.push(c);
        } else {
            rest.push(c);
        }
    }
    let mut out = reserved;
    out.extend(rest);
    out
}

/// Pin atom-directed chunks to the front of the merge so the
/// `KQ_MERGED_LIMIT` truncate cannot drop them.
///
/// Atom-enum chunks are fetched by chunk-id / FTS (no query embedding,
/// see `enumerate_typed_atom_chunks` → `fetch_chunk_by_id`), so they
/// carry `vector_distance = None`. `cross_corpus_sort_cmp` sorts every
/// `None`-distance chunk *after* every cosine-scored base chunk
/// (`(Some, None) => Less`), so on any corpus that returns ≥
/// `KQ_MERGED_LIMIT` base hits the entire directed set lands past the
/// truncation cut — injected but never seen by synthesis (survival 0,
/// measured: exec_cast / counterparty enumeration both 0/16 survivors).
///
/// This is the same contract `reserve_chunks_per_entity` enforces for
/// ComparisonQuery + title-expand: an upstream step made an intentional
/// source selection the cross-corpus sort must not silently demote.
/// Here the selector is the atlas itself — "these are the entities the
/// question enumerates" — which is the foundational atlas-directs-
/// retrieval premise. The set is already bounded by
/// `SOVEREIGN_ATOM_ENUM_TOPK`, so all of it is pinned (no per-entity
/// quota); relative order is preserved (atom-enum injects in descending
/// atom prominence). No-op when nothing is tagged `source=atom-enum`.
pub(crate) fn reserve_atom_enum_chunks(chunks: Vec<ScoredChunk>) -> Vec<ScoredChunk> {
    let is_atom_enum = |c: &ScoredChunk| {
        c.metadata
            .get("source")
            .map(|s| s == "atom-enum")
            .unwrap_or(false)
    };
    if !chunks.iter().any(is_atom_enum) {
        return chunks;
    }
    let mut reserved: Vec<ScoredChunk> = Vec::new();
    let mut rest: Vec<ScoredChunk> = Vec::new();
    for c in chunks {
        if is_atom_enum(&c) {
            reserved.push(c);
        } else {
            rest.push(c);
        }
    }
    reserved.extend(rest);
    reserved
}

/// Reserve RAPTOR collapsed-tree summary chunks (metadata `source=raptor`)
/// ahead of the `KQ_MERGED_LIMIT` truncate — same rationale as
/// `reserve_atom_enum_chunks`: the grounding step deliberately selected the
/// top-M summaries by cosine; the cross-corpus sort must not silently demote
/// them below base chunks. No-op when nothing is tagged `source=raptor`.
pub(crate) fn reserve_raptor_chunks(chunks: Vec<ScoredChunk>) -> Vec<ScoredChunk> {
    let is_raptor = |c: &ScoredChunk| {
        c.metadata
            .get("source")
            .map(|s| s == "raptor")
            .unwrap_or(false)
    };
    if !chunks.iter().any(is_raptor) {
        return chunks;
    }
    let mut reserved: Vec<ScoredChunk> = Vec::new();
    let mut rest: Vec<ScoredChunk> = Vec::new();
    for c in chunks {
        if is_raptor(&c) {
            reserved.push(c);
        } else {
            rest.push(c);
        }
    }
    reserved.extend(rest);
    reserved
}

/// Project merged chunks into the `retrieved_chunks` JSON carried on the
/// assistant message metadata — the single shape BOTH the KnowledgeQuery
/// (`prepare_knowledge_query_plan`) and DeepQuery (`prepare_knowledge_context`)
/// paths emit, and that the desktop citation expander + the eval's
/// `RetrievedChunk` both read. Previously two divergent `json!` blocks: the KQ
/// copy carried `score` but not `metadata`, the DQ copy the reverse, and the
/// `source` provenance field (added for RAPTOR observability) had to be patched
/// into each by hand — a duplication that silently hid raptor chunks from the
/// eval until both copies were found. Unified to the superset so the next
/// chunk-level field is a one-place edit. `metadata` powers the desktop
/// "↗ surfaced via entity bridge" subtitle (frontend gates on
/// `metadata.ppr_mass_norm > 0.5`).
pub(crate) fn project_retrieved_chunks(chunks: &[ScoredChunk]) -> Vec<serde_json::Value> {
    chunks
        .iter()
        .map(|c| {
            let snippet = super::text_utils::truncate_with_ellipsis(&c.content, 200);
            serde_json::json!({
                "title": c.title.as_deref().unwrap_or(""),
                "corpus_id": c.corpus_id,
                "url": c.url,
                "snippet": snippet,
                "score": c.score,
                "source": c.metadata.get("source"),
                "provenance_tier": if c.url.is_some() { "web" } else { "corpus" },
                "chunk_id": c.chunk_id,
                "source_doc_id": c.source_doc_id,
                "metadata": c.metadata,
            })
        })
        .collect()
}

/// `SOVEREIGN_RAPTOR_LATE` (default ON — set `=0` to disable) — inject RAPTOR
/// summaries AFTER the leaf merge/rerank pipeline instead of before it, so they
/// cannot perturb leaf retrieval or ranking. This is the default mode because
/// on the SEP bench it makes raptor QA-NEUTRAL (sources 76→86, at/above the
/// no-raptor 85 baseline — the residual harm additive truncation couldn't
/// reach, since sparse-pool questions are displaced UPSTREAM of the truncate)
/// while keeping the summarization gain (+5 judge over no-raptor) and running
/// slightly FASTER than early injection (raptor no longer drags ~8 chunks
/// through reweight/sort/graph-expand). Early injection (LATE=0) lets summaries
/// participate in graph-expansion but costs source-coverage QA. Independent of
/// SOVEREIGN_RAPTOR_GROUNDING, which still gates raptor on/off overall.
pub(crate) fn raptor_late_inject_enabled() -> bool {
    std::env::var("SOVEREIGN_RAPTOR_LATE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(true)
}
