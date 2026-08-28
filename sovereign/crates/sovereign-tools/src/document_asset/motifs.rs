// SPDX-License-Identifier: AGPL-3.0-or-later
//! Motif detection and embedding-space segmentation — the statistical half of
//! skeleton building, kept apart from the model-driven half.

// One cooperating unit split for size (ARCH §3.2), not independent modules:
// the manager, its three phases and the skeleton free functions all name each
// other's types. The import surface stays in `mod.rs`.
use super::*;

/// Stoplist of the most common English function words. Used by motif
/// extraction to filter out conjunctions, prepositions, pronouns, and
/// other words that recur frequently in every English document and
/// therefore can't distinguish one document from another. Kept short
/// and curated (~110 entries) rather than exhaustive — the TF-IDF +
/// LLM classifier downstream catches anything this misses.
pub(super) const MOTIF_STOPLIST: &[&str] = &[
    "the", "and", "for", "are", "but", "not", "you", "all", "can", "had", "her", "was", "one",
    "our", "out", "day", "get", "has", "him", "his", "how", "man", "new", "now", "old", "see",
    "two", "way", "who", "boy", "did", "its", "let", "put", "say", "she", "too", "use", "any",
    "every", "from", "have", "into", "like", "more", "much", "must", "only", "over", "said",
    "some", "such", "than", "that", "them", "they", "this", "very", "want", "well", "were", "what",
    "when", "with", "your", "their", "there", "these", "those", "would", "could", "should",
    "about", "after", "again", "before", "being", "below", "doing", "going", "having", "still",
    "while", "where", "which", "whose", "until", "under", "above", "across", "almost", "another",
    "because", "between", "however", "without", "through", "though", "perhaps", "rather", "seemed",
    "though", "toward", "upon", "whom", "indeed", "least", "much", "often", "since", "thus", "yet",
    "even", "made", "make", "down", "back", "come", "came", "took", "look", "good", "great",
    "long", "last", "first", "right", "left", "thing", "things", "those", "time", "times", "year",
    "years", "place", "world",
];

/// Candidate term for the motif index. Pure-Rust extraction pass —
/// no LLM. Returns up to `top_n` terms ranked by chunk-presence
/// breadth (terms appearing in 3+ chunks but not in every chunk are
/// the most likely motif candidates). Caller passes the result to
/// `classify_motifs` for the LLM motif-vs-noise judgment.
pub(super) fn extract_motif_candidates(chunks: &[TextChunk], top_n: usize) -> Vec<MotifCandidate> {
    use std::collections::HashMap;

    let stoplist: std::collections::HashSet<&str> = MOTIF_STOPLIST.iter().copied().collect();

    // term → (total_count, set_of_chunk_indices)
    let mut term_stats: HashMap<String, (u32, std::collections::BTreeSet<u32>)> = HashMap::new();

    for (idx, chunk) in chunks.iter().enumerate() {
        let mut seen_this_chunk: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for raw in chunk
            .content
            .split(|c: char| !c.is_alphabetic() && c != '\'')
        {
            let lower = raw.to_lowercase();
            // Length + stoplist filters. Drop possessives and contractions
            // by stripping a trailing 's or ' before the length check.
            let trimmed = lower
                .trim_end_matches("'s")
                .trim_end_matches('\'')
                .to_string();
            if trimmed.len() < 4 || trimmed.len() > 20 {
                continue;
            }
            if stoplist.contains(trimmed.as_str()) {
                continue;
            }
            if !trimmed.chars().any(|c| c.is_alphabetic()) {
                continue;
            }
            let entry = term_stats
                .entry(trimmed.clone())
                .or_insert_with(|| (0, std::collections::BTreeSet::new()));
            entry.0 += 1;
            if seen_this_chunk.insert(trimmed) {
                entry.1.insert(idx as u32);
            }
        }
    }

    let total_chunks = chunks.len().max(1) as f32;
    let mut candidates: Vec<MotifCandidate> = term_stats
        .into_iter()
        .filter_map(|(term, (count, chunk_set))| {
            let df = chunk_set.len();
            // Drop topical terms (>60% of doc — generic vocabulary).
            // Keep low-df hapax-and-near-hapax terms: a Conrad word
            // like "coruscations" (df=2) IS the load-bearing scene
            // marker, not noise. The LLM motif classifier downstream
            // separates real motifs from incidental rarities; we
            // only need to keep the candidate pool wide enough that
            // the rare-but-distinctive ones reach it.
            if df < 1 || df as f32 / total_chunks > 0.6 {
                return None;
            }
            let occurrences: Vec<u32> = chunk_set.into_iter().collect();
            // TF-IDF style score: higher when a term is moderately
            // frequent in absolute count but distributed across
            // relatively few chunks.
            let tf = count as f32;
            let idf = ((total_chunks + 1.0) / (df as f32 + 1.0)).ln();
            let score = tf * idf;
            Some(MotifCandidate {
                term,
                tf_idf_score: score,
                occurrence_chunk_ids: occurrences,
            })
        })
        .collect();

    candidates.sort_by(|a, b| {
        b.tf_idf_score
            .partial_cmp(&a.tf_idf_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    candidates.truncate(top_n);
    candidates
}

/// A pre-classification motif candidate. Identical shape to
/// `AssetMotif` minus `is_distinctive`, which the LLM classifier
/// fills in.
#[derive(Debug, Clone)]
pub(super) struct MotifCandidate {
    pub(super) term: String,
    pub(super) tf_idf_score: f32,
    pub(super) occurrence_chunk_ids: Vec<u32>,
}

/// Ask the model which of the candidate terms are genuine recurring
/// motifs vs incidental rare words. One Slow-slot call; grammar
/// forces a JSON array of motif terms drawn from the input set.
///
/// Returns a Vec<AssetMotif> with `is_distinctive` set per the
/// model's judgment. Falls back to "all distinctive" on LLM failure
/// — over-inclusive is safer than empty (the briefing has its own
/// budget cap).
pub(super) async fn classify_motifs(
    inference: &Arc<dyn InferenceProvider>,
    candidates: Vec<MotifCandidate>,
    doc_type: DocumentTypeTag,
) -> Vec<AssetMotif> {
    if candidates.is_empty() {
        return Vec::new();
    }

    let terms_csv = candidates
        .iter()
        .map(|c| c.term.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    let doc_cue = match doc_type {
        DocumentTypeTag::Narrative => {
            "recurring motifs are images, gestures, character tics, or refrains the author returns to"
        }
        DocumentTypeTag::Argument => {
            "recurring motifs are key concepts or terms-of-art the argument turns on"
        }
        DocumentTypeTag::Evidence => {
            "recurring motifs are central variables, methods, or claims the paper threads through"
        }
        DocumentTypeTag::Chronicle => {
            "recurring motifs are people, places, or patterns that recur across the timeline"
        }
        DocumentTypeTag::Technical => {
            "recurring motifs are protocols, components, or recurring procedures"
        }
        DocumentTypeTag::Journal => {
            "recurring motifs are people, feelings, or situations the entries keep returning to"
        }
        DocumentTypeTag::Unknown => {
            "recurring motifs are terms the document returns to deliberately, not incidentally"
        }
    };

    let prompt = format!(
        "You are picking out genuine recurring motifs from a {doc_type} document. \
         The candidates below were extracted by frequency; some are real motifs the \
         document returns to deliberately, others are incidental rare vocabulary. \
         For this document type, {doc_cue}.\n\n\
         CANDIDATES: {terms_csv}\n\n\
         Reply with a JSON array of just the motif terms — only terms from the \
         candidate list, lowercase, no explanation. Example: [\"incurious\", \"circles\"].",
        doc_type = doc_type.label(),
    );

    // SLOT_POLICY §3 ExtractDurable: recurring-motif classification written
    // to the durable skeleton; corruption outlives the session.
    let mut request = Workload::ExtractDurable
        .request(prompt)
        .with_output_budget(400);
    request.temperature = Some(0.1);
    // POLICY-DEBT(SLOT_POLICY §3 ExtractDurable): Some(0) preserved for P1
    // neutrality (bundle is None); P5 confirms.
    request.think_budget = Some(0);
    let resp = inference.complete(&request).await;

    let distinctive_set: std::collections::HashSet<String> = match resp {
        Ok(r) => parse_motif_classification(&r.text),
        Err(e) => {
            tracing::warn!(
                error = %e,
                "classify_motifs — LLM call failed; treating all candidates as distinctive"
            );
            candidates.iter().map(|c| c.term.clone()).collect()
        }
    };

    candidates
        .into_iter()
        .map(|c| AssetMotif {
            is_distinctive: distinctive_set.contains(&c.term),
            term: c.term,
            tf_idf_score: c.tf_idf_score,
            occurrence_chunk_ids: c.occurrence_chunk_ids,
        })
        .collect()
}

/// Parse the model's motif-classification response. Accepts JSON
/// arrays of strings, ignoring anything outside the first `[...]`
/// span. Returns the set of distinctive terms (lowercased). On
/// parse failure returns an empty set — caller decides the fallback.
pub(super) fn parse_motif_classification(text: &str) -> std::collections::HashSet<String> {
    let start = match text.find('[') {
        Some(i) => i,
        None => return std::collections::HashSet::new(),
    };
    let end = match text[start..].find(']') {
        Some(i) => start + i + 1,
        None => return std::collections::HashSet::new(),
    };
    let json_slice = &text[start..end];
    serde_json::from_str::<Vec<String>>(json_slice)
        .map(|v| v.into_iter().map(|s| s.to_lowercase()).collect())
        .unwrap_or_default()
}

/// TextTiling-style boundary detection on adjacent-chunk embedding
/// similarity. Returns a `Vec<bool>` of length `embeddings.len() - 1`
/// where `true` at index `i` means a segment break falls between
/// chunk `i` and chunk `i+1`.
///
/// Algorithm (Hearst 1997, modern embedding variant):
/// 1. Compute cosine similarity between each adjacent pair.
/// 2. For each gap `i`, compute a "depth score" — how far this
///    similarity dips below the maximum similarity in the
///    `window`-sized neighborhood on either side. A high depth
///    score means the gap is a deep valley between two coherent
///    regions.
/// 3. Threshold: `depth > mean(depth) + depth_k * std(depth)`.
///
/// Parameters:
/// - `window`: how many gaps to scan on each side when computing
///   left/right peaks. 3 works well for ~700-char chunks; smaller
///   for noisier signals, larger for sentence-level tiling.
/// - `depth_k`: standard-deviation multiplier for the threshold.
///   1.0 gives a "moderately confident" boundary. 0.5 is more
///   permissive; 1.5 is stricter. The bench will tune this.
///
/// Returns no breaks (all `false`) if `embeddings.len() < 2` or
/// if the depth signal has no variance (e.g. identical embeddings).
pub(super) fn detect_segment_boundaries(
    embeddings: &[Vec<f32>],
    window: usize,
    depth_k: f32,
) -> Vec<bool> {
    let n = embeddings.len();
    if n < 2 {
        return Vec::new();
    }

    // Cosine similarity for each gap (n-1 gaps for n chunks).
    let sims: Vec<f32> = (0..n - 1)
        .map(|i| cosine_similarity(&embeddings[i], &embeddings[i + 1]))
        .collect();

    // Depth score for each gap. The left/right peak is the max
    // similarity in the window-sized neighborhood; the depth is
    // how far the current similarity drops below the average of
    // the two peaks. Higher depth = stronger boundary candidate.
    let depths: Vec<f32> = (0..sims.len())
        .map(|i| {
            let left_start = i.saturating_sub(window);
            let right_end = (i + window + 1).min(sims.len());
            let left_peak = sims[left_start..=i]
                .iter()
                .copied()
                .fold(f32::NEG_INFINITY, f32::max);
            let right_peak = sims[i..right_end]
                .iter()
                .copied()
                .fold(f32::NEG_INFINITY, f32::max);
            ((left_peak - sims[i]) + (right_peak - sims[i])).max(0.0)
        })
        .collect();

    // Adaptive threshold: mean + depth_k * std. If std == 0 the
    // signal is flat and no boundaries should fire.
    let mean = depths.iter().sum::<f32>() / depths.len() as f32;
    let variance = depths.iter().map(|d| (d - mean).powi(2)).sum::<f32>() / depths.len() as f32;
    let std = variance.sqrt();
    if std < f32::EPSILON {
        return vec![false; depths.len()];
    }
    let threshold = mean + depth_k * std;

    depths.iter().map(|d| *d > threshold).collect()
}

/// Cosine similarity between two equal-length f32 vectors.
/// Returns 0.0 if either vector is empty or has zero magnitude.
pub(super) fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na <= f32::EPSILON || nb <= f32::EPSILON {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Parse the lean skeleton-extraction response (one line per chunk,
/// comma-separated entity names). Returns one SkeletonBatchEntry per
/// chunk in the batch.
///
/// Lines are taken in order — line N maps to batch_start+N. Empty
/// lines (chunks with no entities) produce an entry with an empty
/// entity list. If the model emits fewer lines than expected, the
/// missing tail is filled with empty entries so chunk_index alignment
/// is preserved.
///
/// The lark grammar wired alongside this parser should make
/// fewer-than-expected lines impossible in practice, but we defend
/// against it so a grammar-compile fallback (which silently drops
/// the constraint) doesn't desync the entity_index.
pub(super) fn parse_lean_skeleton_batch(
    response: &str,
    batch_start: usize,
    batch_len: usize,
) -> Vec<SkeletonBatchEntry> {
    let trimmed = response.trim();
    // Strip a stray "Answer:" prefix if the model echoes the cue.
    let cleaned = trimmed
        .strip_prefix("Answer:")
        .map(|s| s.trim())
        .unwrap_or(trimmed);

    let mut lines: Vec<&str> = cleaned.lines().collect();
    // Pad with empty lines if model emitted fewer than expected.
    while lines.len() < batch_len {
        lines.push("");
    }
    lines.truncate(batch_len);

    let mut entries = Vec::with_capacity(batch_len);
    for (i, line) in lines.iter().enumerate() {
        let entity_names_and_kinds: Vec<(String, EntityKind)> = line
            .split(',')
            .map(|n| n.trim())
            .filter(|n| !n.is_empty() && n.chars().any(|c| c.is_alphabetic()))
            .map(|n| (n.to_string(), EntityKind::Concept))
            .collect();
        entries.push(SkeletonBatchEntry {
            chunk_index: batch_start + i,
            // Per-chunk function is no longer carried in the lean
            // schema; segments carry function at segment scope which
            // is what downstream consumes. Default to Develops as
            // an unobtrusive placeholder.
            function: SectionFunction::Develops,
            entity_names_and_kinds,
            // structural_moments superseded by segments.
            moment_description: None,
        });
    }
    entries
}

/// Parse the WINDOW entity schema (2026-07-24): one comma-separated,
/// deduped list of canonical names for a 12-chunk window. Chunk-level
/// attribution is recovered here, deterministically: each name is
/// attributed to every window chunk whose text contains it
/// (case-insensitive — canonical names are distinctive enough that
/// case folding trades no precision for robustness to sentence-case
/// drift). A name the model extracted but no chunk contains verbatim
/// (paraphrase, e.g. an epithet) falls back to the window's first
/// chunk so the entity still exists in the index at window
/// granularity rather than vanishing.
pub(super) fn parse_window_skeleton_batch(
    response: &str,
    batch_start: usize,
    batch: &[TextChunk],
) -> Vec<SkeletonBatchEntry> {
    attribute_entity_names(parse_entity_name_list(response), batch_start, batch)
}

/// Split the LLM's one-line, comma-separated name list into names.
///
/// Separated from [`attribute_entity_names`] so the attribution half can
/// be driven by a non-LLM extractor (GLiNER) that already returns names
/// and never produces a string to parse.
pub(super) fn parse_entity_name_list(response: &str) -> Vec<String> {
    let trimmed = response.trim();
    let cleaned = trimmed
        .strip_prefix("Answer:")
        .map(|s| s.trim())
        .unwrap_or(trimmed);
    // Grammar forces a single line, but defend against a
    // grammar-compile fallback emitting several: fold them into one
    // name pool rather than desyncing.
    cleaned
        .lines()
        .flat_map(|l| l.split(','))
        .map(|n| n.trim())
        .filter(|n| !n.is_empty() && n.chars().any(|c| c.is_alphabetic()))
        .map(|n| n.to_string())
        .collect()
}

/// Attribute window-level entity names to individual chunks.
///
/// Each name is attributed to every window chunk whose text contains it
/// (case-insensitive — canonical names are distinctive enough that case
/// folding trades no precision for robustness to sentence-case drift).
/// A name no chunk contains verbatim (a paraphrase or epithet) falls
/// back to the window's first chunk so the entity still exists in the
/// index at window granularity rather than vanishing.
///
/// **Casing is taken from the document, not from the caller.** The name
/// a producer hands us may be cased arbitrarily — the `EntityExtractor`
/// contract specifies lower-cased output, and an LLM renders names
/// however it likes — but these strings end up in the briefing and in
/// the segment-naming prompt, where "stevie" instead of "Stevie" is a
/// visible quality loss. When a chunk contains the name we splice the
/// matched span back out of the original text, so the stored form is
/// whatever the document actually wrote.
pub(super) fn attribute_entity_names(
    names: Vec<String>,
    batch_start: usize,
    batch: &[TextChunk],
) -> Vec<SkeletonBatchEntry> {
    let lowered_chunks: Vec<String> = batch.iter().map(|c| c.content.to_lowercase()).collect();
    let mut per_chunk: Vec<Vec<(String, EntityKind)>> = vec![Vec::new(); batch.len()];
    for name in names {
        let needle = name.to_lowercase();
        let mut hit = false;
        for (i, chunk_lower) in lowered_chunks.iter().enumerate() {
            if let Some(at) = chunk_lower.find(&needle) {
                // `to_lowercase` can change byte length (e.g. 'İ'), so the
                // lowered offset is only safe to reuse when it maps back
                // to a char boundary in the original; otherwise keep the
                // name as given rather than risk a panic or a garbled slice.
                let cased = batch[i]
                    .content
                    .get(at..at + needle.len())
                    .filter(|s| s.to_lowercase() == needle)
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| name.clone());
                per_chunk[i].push((cased, EntityKind::Concept));
                hit = true;
            }
        }
        if !hit && !per_chunk.is_empty() {
            per_chunk[0].push((name, EntityKind::Concept));
        }
    }

    per_chunk
        .into_iter()
        .enumerate()
        .map(|(i, entity_names_and_kinds)| SkeletonBatchEntry {
            chunk_index: batch_start + i,
            function: SectionFunction::Develops,
            entity_names_and_kinds,
            moment_description: None,
        })
        .collect()
}

/// Parse the Pass-B segment-naming JSON response. Returns None for
/// unparseable responses; the caller falls back to a placeholder
/// title rather than failing the segment.
pub(super) fn parse_segment_naming(
    text: &str,
) -> Option<(String, String, SectionFunction, Vec<String>)> {
    let trimmed = text.trim();
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    if end <= start {
        return None;
    }
    let json_str = &trimmed[start..=end];
    let v: serde_json::Value = serde_json::from_str(json_str).ok()?;
    let obj = v.as_object()?;
    let title = obj.get("title").and_then(|x| x.as_str())?.to_string();
    let summary = obj
        .get("summary")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let function = match obj
        .get("function")
        .and_then(|x| x.as_str())
        .unwrap_or("Develops")
        .to_lowercase()
        .as_str()
    {
        "introduces" => SectionFunction::Introduces,
        "develops" => SectionFunction::Develops,
        "complicates" => SectionFunction::Complicates,
        "resolves" => SectionFunction::Resolves,
        "transitions" => SectionFunction::Transitions,
        "evidences" => SectionFunction::Evidences,
        _ => SectionFunction::Develops,
    };
    let key_entities = obj
        .get("key_entities")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    Some((title, summary, function, key_entities))
}
