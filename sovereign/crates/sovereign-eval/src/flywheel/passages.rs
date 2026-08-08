// SPDX-License-Identifier: AGPL-3.0-or-later
//! Resolving an atom's evidence reference to the **real passage text** it
//! points at.
//!
//! **The defect this closes.** A `Claim` atom's evidence carries a
//! `chunk_id` like `sec_0002` and a `passage_preview` — and the preview is a
//! ~25-character fragment, not a passage. The first calibration set shipped
//! those fragments *as if they were the retrieved chunks*
//! (`sovereign/bench/calibration/native_grounding_calibration.jsonl`, whose
//! own report records `answerable_witness_absent: 13` on 13 answerable
//! pairs — every "answerable" pool failed to contain its own answer). A
//! scorer measured on 25-char fragments is not measured on anything the
//! runtime will ever see, so H1's kill gate would have been reading noise.
//!
//! **Why the section id cannot be followed directly.** `CorpusIndex` ships
//! the purpose-built resolver for this — `resolve_sections_to_chunks`
//! (`corpus-engine/src/index/read.rs:137`) — and it keys on a `section_id`
//! field inside each chunk's `metadata` JSON. Measured on this host
//! (2026-08-08): across all 42 installed corpora that have a `chunks.lance`,
//! 146,596 chunks carry non-null metadata and **39 of them carry
//! `section_id`** — none in any corpus this initiative mines. So the
//! section id is, in practice, an unresolvable reference here.
//!
//! **What is resolvable: the anchor.** The same atom carries up to three
//! verbatim fragments of its supporting passage — `quotable_excerpt`,
//! `anchor`, and `evidence[].passage_preview`. Those are *copied out of the
//! source text*, so they can be found again in it. [`PassageStore::resolve`]
//! searches the document's real chunks for a chunk containing the anchor,
//! and the passage it returns is the one the anchor was cut from. The match
//! is a containment check, not a similarity score: either the chunk holds
//! the fragment or it does not.
//!
//! **A claim whose anchor does not resolve is DROPPED, and counted.** There
//! is no nearest-chunk fallback. Handing back a plausible-but-wrong passage
//! would poison the answerable label silently, which is the exact shape ARCH
//! §18.3 forbids — absence is reported, never defaulted. Measured drop rate
//! on the SEP substrate is ~42% (see `MineReport::claims_unresolved`).

use std::collections::HashMap;
use std::path::Path;

/// One real retrieval passage: the text a reranker would actually be handed.
#[derive(Debug, Clone)]
pub struct Passage {
    /// The chunk table's row id — the address this text lives at.
    pub chunk_id: u64,
    /// The chunk's full text.
    pub text: String,
    /// [`normalize`]d text, precomputed because every claim in the document
    /// searches every chunk.
    normalized: String,
}

/// Every passage of one document, in chunk-id order.
///
/// Loaded once per document and reused across that document's claims —
/// the scan is O(corpus) and must not be paid per claim.
#[derive(Debug, Clone)]
pub struct PassageStore {
    /// The chunk-store corpus these passages came from (which is NOT always
    /// the atlas corpus id — see [`PassageStore::load`]).
    pub corpus_id: String,
    /// The `source_doc_id` filter applied, if any.
    pub doc_filter: Option<String>,
    passages: Vec<Passage>,
}

/// Lowercase, strip everything that is not `[a-z0-9]` to single spaces.
///
/// This is what makes the anchor match survive the two differences that
/// otherwise sink it: curly vs. straight quotation marks (`’` is not
/// ascii-alphanumeric, so both sides collapse to a space), and the
/// case damage some enrichment passes leave in previews (a real one from
/// brothers-karamazov-book-1: `"to be likE Shakespeare's"`).
#[must_use]
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending_space = false;
    for ch in s.chars() {
        // `to_lowercase` per char, then keep only ascii alphanumerics.
        let mut kept = false;
        for lc in ch.to_lowercase() {
            if lc.is_ascii_alphanumeric() {
                if pending_space && !out.is_empty() {
                    out.push(' ');
                }
                pending_space = false;
                out.push(lc);
                kept = true;
            }
        }
        if !kept {
            pending_space = true;
        }
    }
    out
}

/// The minimum words a fragment must have before it is allowed to anchor a
/// passage. Three-word fragments collide across a philosophy corpus; four is
/// the floor that stopped producing cross-article false matches in the
/// 45-article resolution sample.
///
/// A fragment under the floor is **dropped**, not matched loosely — so an
/// elided anchor whose short side is too small resolves on its long side
/// alone (still a verbatim containment proof, of less text), and an anchor
/// with no surviving fragment resolves to nothing at all.
const MIN_FRAGMENT_WORDS: usize = 4;

/// Split an anchor into the verbatim fragments a passage must contain, in
/// order.
///
/// Anchors are often *elided* quotations — real examples from
/// brothers-karamazov-book-1: `"socialism is... the atheistic question"`,
/// `"it was the greatest need... to find some one or something holy"`. The
/// `...` stands for text the extractor cut out, so the two sides are
/// verbatim but not contiguous. Splitting on the ellipsis and requiring the
/// pieces IN ORDER keeps the match honest: it still proves the passage
/// contains the quoted words, in the quoted sequence.
#[must_use]
pub fn anchor_fragments(raw: &str) -> Vec<String> {
    raw.split("...")
        .flat_map(|p| p.split('…'))
        .map(normalize)
        .filter(|f| f.split(' ').filter(|w| !w.is_empty()).count() >= MIN_FRAGMENT_WORDS)
        .collect()
}

impl PassageStore {
    /// Load a document's passages from a corpus chunk store.
    ///
    /// `corpus_id` is the **chunk store**, which is not always the atlas
    /// corpus. Both shapes exist on this host and both are supported:
    ///
    ///   * *co-located* — `brothers-karamazov-book-1` has its own
    ///     `chunks.lance` beside its `atlas/`. Pass its id and no filter.
    ///   * *shared* — the 1,770 `sep-<slug>/` directories are atlases ONLY
    ///     (no `chunks.lance`, verified across all of them). Their passages
    ///     live together in one `sep` corpus of 187,967 chunks, keyed by
    ///     `source_doc_id` (1,770 distinct values, one per article). Pass
    ///     `corpus_id = "sep"` and the article's `source_doc_id` as
    ///     `doc_filter`.
    ///
    /// # Errors
    /// Refuses — never returns an empty store — when the corpus has no chunk
    /// table, or when the filter selects nothing. An empty passage store
    /// would resolve zero anchors and report every claim unresolvable, which
    /// reads as "this corpus is bad" rather than "you pointed at the wrong
    /// chunk store".
    pub async fn load(
        index_root: &Path,
        corpus_id: &str,
        doc_filter: Option<&str>,
    ) -> Result<Self, String> {
        let dir = index_root.join(corpus_id);
        if !dir.join("chunks.lance").exists() {
            return Err(format!(
                "`{corpus_id}` has no chunk store at {:?} — it is an atlas-only corpus, and its \
                 passages (if they exist at all) live in some other corpus. Point --chunks-corpus \
                 at the corpus that holds the text.",
                dir.join("chunks.lance")
            ));
        }
        let index = corpus_engine::CorpusIndex::open(&dir)
            .await
            .map_err(|e| format!("open chunk store `{corpus_id}` at {dir:?}: {e}"))?;

        // One read path for both shapes: resolve the set of `source_doc_id`s
        // wanted, then fetch their chunks. `chunks_by_source_doc_ids` is the
        // only API that returns content without also pulling the 1024-dim
        // embedding column (`all_chunks_with_embeddings` does, and on a
        // shared store that is gigabytes for text we already have).
        let doc_ids: Vec<String> = match doc_filter {
            Some(doc) => vec![doc.to_string()],
            None => {
                let mut ids: Vec<String> = index
                    .group_chunks_by_source_doc()
                    .await
                    .map_err(|e| format!("group `{corpus_id}` chunks by source doc: {e}"))?
                    .into_keys()
                    .collect();
                ids.sort();
                if ids.is_empty() {
                    return Err(format!(
                        "`{corpus_id}` has a chunk store but no chunk carries a `source_doc_id`, \
                         so its passages cannot be addressed"
                    ));
                }
                ids
            }
        };
        let rows = index
            .chunks_by_source_doc_ids(&doc_ids)
            .await
            .map_err(|e| format!("read `{corpus_id}` chunks for {} doc(s): {e}", doc_ids.len()))?;

        let mut passages: Vec<Passage> = rows
            .into_iter()
            .filter(|r| !r.content.trim().is_empty())
            .map(|r| Passage {
                chunk_id: r.id,
                normalized: normalize(&r.content),
                text: r.content,
            })
            .collect();
        // Chunk-id order is source order, and it is what makes the
        // distractor rotation reproducible.
        passages.sort_by_key(|p| p.chunk_id);

        if passages.is_empty() {
            return Err(match doc_filter {
                Some(doc) => format!(
                    "`{corpus_id}` has a chunk store but no non-empty chunk with source_doc_id \
                     `{doc}` — the filter matched nothing, so every claim would report as \
                     unresolvable for the wrong reason"
                ),
                None => format!("`{corpus_id}`'s chunk store is empty"),
            });
        }
        Ok(Self {
            corpus_id: corpus_id.to_string(),
            doc_filter: doc_filter.map(str::to_string),
            passages,
        })
    }

    /// Load EVERY document of a shared chunk store in one scan, partitioned
    /// by `source_doc_id`.
    ///
    /// This exists for the SEP substrate's shape: 1,770 atlases sharing one
    /// 187,967-chunk store. Calling [`Self::load`] per article would issue
    /// 1,770 filtered scans of the same table — LanceDB has no index on
    /// `source_doc_id`, so each is a full scan. One scan and a group-by is
    /// the same answer at 1/1770th the I/O.
    ///
    /// Documents whose chunks are all empty are omitted rather than
    /// returned as empty stores, so a lookup miss means the same thing here
    /// as it does in [`Self::load`]: there is nothing to resolve against.
    ///
    /// # Errors
    /// Propagates a chunk-store open/read failure, and refuses a store that
    /// yields no documents at all.
    pub async fn load_partitioned(
        index_root: &Path,
        corpus_id: &str,
    ) -> Result<HashMap<String, Self>, String> {
        let dir = index_root.join(corpus_id);
        if !dir.join("chunks.lance").exists() {
            return Err(format!(
                "`{corpus_id}` has no chunk store at {:?}",
                dir.join("chunks.lance")
            ));
        }
        let index = corpus_engine::CorpusIndex::open(&dir)
            .await
            .map_err(|e| format!("open chunk store `{corpus_id}` at {dir:?}: {e}"))?;
        let mut doc_ids: Vec<String> = index
            .group_chunks_by_source_doc()
            .await
            .map_err(|e| format!("group `{corpus_id}` chunks by source doc: {e}"))?
            .into_keys()
            .collect();
        doc_ids.sort();
        if doc_ids.is_empty() {
            return Err(format!(
                "`{corpus_id}` has a chunk store but no chunk carries a `source_doc_id`"
            ));
        }
        let rows = index
            .chunks_by_source_doc_ids(&doc_ids)
            .await
            .map_err(|e| format!("read `{corpus_id}` chunks for {} doc(s): {e}", doc_ids.len()))?;

        let mut by_doc: HashMap<String, Vec<Passage>> = HashMap::new();
        for r in rows {
            let Some(doc) = r.source_doc_id.clone() else {
                continue;
            };
            if r.content.trim().is_empty() {
                continue;
            }
            by_doc.entry(doc).or_default().push(Passage {
                chunk_id: r.id,
                normalized: normalize(&r.content),
                text: r.content,
            });
        }
        Ok(by_doc
            .into_iter()
            .map(|(doc, mut passages)| {
                passages.sort_by_key(|p| p.chunk_id);
                (
                    doc.clone(),
                    Self {
                        corpus_id: corpus_id.to_string(),
                        doc_filter: Some(doc),
                        passages,
                    },
                )
            })
            .collect())
    }

    /// Build a store directly from `(chunk_id, text)` rows. The seam tests
    /// use it; `load` is the production path.
    #[must_use]
    pub fn from_rows(corpus_id: &str, rows: Vec<(u64, String)>) -> Self {
        let mut passages: Vec<Passage> = rows
            .into_iter()
            .map(|(chunk_id, text)| Passage {
                chunk_id,
                normalized: normalize(&text),
                text,
            })
            .collect();
        passages.sort_by_key(|p| p.chunk_id);
        Self {
            corpus_id: corpus_id.to_string(),
            doc_filter: None,
            passages,
        }
    }

    /// The passages, in chunk-id order.
    #[must_use]
    pub fn passages(&self) -> &[Passage] {
        &self.passages
    }

    /// How many passages this document has.
    #[must_use]
    pub fn len(&self) -> usize {
        self.passages.len()
    }

    /// Always `false` — [`Self::load`] refuses to build an empty store.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.passages.is_empty()
    }

    /// Find the passage an anchor was cut from.
    ///
    /// `anchors` are tried in order (best first) and the first one that
    /// resolves wins; a caller passes `quotable_excerpt`, then
    /// `passage_preview`, then `anchor`. Returns the index into
    /// [`Self::passages`], or `None` when no anchor is found in any passage
    /// — the drop signal.
    #[must_use]
    pub fn resolve(&self, anchors: &[String]) -> Option<usize> {
        for raw in anchors {
            let fragments = anchor_fragments(raw);
            if fragments.is_empty() {
                continue;
            }
            if let Some(i) = self
                .passages
                .iter()
                .position(|p| contains_in_order(&p.normalized, &fragments))
            {
                return Some(i);
            }
        }
        None
    }

    /// Does ANY passage in `idxs` contain any of these anchors?
    ///
    /// This is [`Self::resolve`]'s question asked of a subset, and it is the
    /// leak check for an absent pool: a pool that contains the claim's own
    /// quoted evidence is not an absent pool, whatever the label says. It is
    /// strictly stronger than the witness-term `gold_match` check it sits
    /// beside, because the witness is derived from the claim's (paraphrased)
    /// content and is frequently absent from the source prose even when the
    /// evidence IS there — measured on brothers-karamazov-book-1, 6 of 10
    /// answerable pools contain their evidence verbatim but not their
    /// witness terms. On those, the witness check cannot fire at all.
    #[must_use]
    pub fn anchors_present_in(&self, anchors: &[String], idxs: &[usize]) -> bool {
        for raw in anchors {
            let fragments = anchor_fragments(raw);
            if fragments.is_empty() {
                continue;
            }
            for &i in idxs {
                if self
                    .passages
                    .get(i)
                    .is_some_and(|p| contains_in_order(&p.normalized, &fragments))
                {
                    return true;
                }
            }
        }
        false
    }
}

/// True when every fragment appears in `haystack`, each after the previous
/// one ended.
fn contains_in_order(haystack: &str, fragments: &[String]) -> bool {
    let mut from = 0usize;
    for f in fragments {
        match haystack[from..].find(f.as_str()) {
            Some(i) => from = from + i + f.len(),
            None => return false,
        }
    }
    true
}

/// Map an atlas corpus id to the chunk store that holds its passages.
///
/// One name for the mapping so the CLI, the miner and the tests cannot
/// drift apart on it (ARCH §10.6). Open set, so it is data with an explicit
/// default rather than a `match` that grows an arm per corpus (ARCH §2.1,
/// §4): anything not listed resolves to itself, which is the co-located
/// case.
#[must_use]
pub fn chunk_store_for(atlas_corpus_id: &str) -> (String, Option<String>) {
    if let Some(slug) = atlas_corpus_id.strip_prefix("sep-") {
        // The SEP chunk store keys articles by their canonical SEP URL.
        return (
            "sep".to_string(),
            Some(format!("https://plato.stanford.edu/entries/{slug}/")),
        );
    }
    (atlas_corpus_id.to_string(), None)
}

/// Index the `source_doc_id`s a shared chunk store actually holds, so a
/// caller can tell "this article is not in the store" from "this article's
/// anchors did not resolve".
///
/// Returns `source_doc_id → chunk count`.
///
/// # Errors
/// Propagates a chunk-store open/read failure.
pub async fn shared_store_documents(
    index_root: &Path,
    corpus_id: &str,
) -> Result<HashMap<String, usize>, String> {
    let dir = index_root.join(corpus_id);
    let index = corpus_engine::CorpusIndex::open(&dir)
        .await
        .map_err(|e| format!("open chunk store `{corpus_id}` at {dir:?}: {e}"))?;
    let groups = index
        .group_chunks_by_source_doc()
        .await
        .map_err(|e| format!("group `{corpus_id}` chunks by source doc: {e}"))?;
    Ok(groups.into_iter().map(|(k, v)| (k, v.len())).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_collapses_the_two_differences_that_sink_a_naive_match() {
        // Curly apostrophe vs straight, and the case damage seen in real
        // previews (`"to be likE Shakespeare's"`, bk-book-1 claim-0002).
        assert_eq!(normalize("to be likE Shakespeare’s"), "to be like shakespeare s");
        assert_eq!(normalize("to be like Shakespeare's"), "to be like shakespeare s");
        assert_eq!(normalize("  a—b\n\nc  "), "a b c");
        assert_eq!(normalize("!!!"), "");
    }

    #[test]
    fn an_elided_anchor_becomes_ordered_fragments() {
        // Real anchor, bk-book-1 claim-0013.
        let f = anchor_fragments("it was the greatest need... to find some one or something holy");
        assert_eq!(
            f,
            vec![
                "it was the greatest need".to_string(),
                "to find some one or something holy".to_string()
            ]
        );
        // Fragments under the word floor are dropped, not matched loosely.
        assert_eq!(anchor_fragments("socialism is... x"), Vec::<String>::new());
        assert_eq!(anchor_fragments("a b c"), Vec::<String>::new());
    }

    #[test]
    fn an_elided_anchor_must_match_in_order() {
        // Both fragments clear MIN_FRAGMENT_WORDS, so both are load-bearing.
        let anchor = "it was the greatest need... to find some one or something holy".to_string();
        let ordered = PassageStore::from_rows(
            "fixture",
            vec![(
                1,
                "and it was the greatest need of all to find some one or something holy to bow \
                 down before"
                    .into(),
            )],
        );
        assert_eq!(ordered.resolve(&[anchor.clone()]), Some(0));

        let reversed = PassageStore::from_rows(
            "fixture",
            vec![(
                2,
                "to find some one or something holy, and only later it was the greatest need of all"
                    .into(),
            )],
        );
        assert_eq!(
            reversed.resolve(&[anchor]),
            None,
            "out-of-order fragments are not a match"
        );
    }

    #[test]
    fn a_fragment_under_the_word_floor_is_dropped_not_matched_loosely() {
        // Consequence worth stating out loud: when one side of an ellipsis
        // is too short to be evidence, it is DISCARDED and the anchor
        // resolves on its surviving side alone. That is still a verbatim
        // containment proof, just of less text — never a fuzzy match of the
        // short side.
        let store = PassageStore::from_rows(
            "fixture",
            vec![(1, "the atheistic question was posed in a wholly different order here".into())],
        );
        // "socialism is" (2 words) is dropped; "the atheistic question was
        // posed" (5 words) carries the match on its own.
        assert_eq!(
            anchor_fragments("socialism is... the atheistic question was posed"),
            vec!["the atheistic question was posed".to_string()]
        );
        assert_eq!(
            store.resolve(&["socialism is... the atheistic question was posed".into()]),
            Some(0)
        );
        // When EVERY fragment is under the floor there is nothing left to
        // prove containment with, and the anchor resolves to nothing.
        assert_eq!(store.resolve(&["socialism is... the question".into()]), None);
    }

    #[test]
    fn anchors_are_tried_best_first() {
        let store = PassageStore::from_rows(
            "fixture",
            vec![
                (1, "the first passage mentions a distant tidal authority".into()),
                (2, "the second passage governs the reckoning of the harbour".into()),
            ],
        );
        // Both resolve; the earlier anchor in the list decides.
        let hit = store.resolve(&[
            "governs the reckoning of the harbour".into(),
            "mentions a distant tidal authority".into(),
        ]);
        assert_eq!(hit, Some(1));
    }

    #[test]
    fn an_unresolvable_anchor_returns_none_rather_than_a_near_miss() {
        // This is the whole point: there is a topically obvious "best"
        // chunk here, and resolve must still refuse it. A nearest-chunk
        // fallback would silently mislabel the pair.
        let store = PassageStore::from_rows(
            "fixture",
            vec![(1, "faith and miracles are discussed at length in this passage".into())],
        );
        assert_eq!(
            store.resolve(&["faith does not spring from the miracle".into()]),
            None
        );
    }

    #[test]
    fn the_sep_mapping_names_the_shared_store_and_everything_else_is_colocated() {
        assert_eq!(
            chunk_store_for("sep-abduction"),
            (
                "sep".to_string(),
                Some("https://plato.stanford.edu/entries/abduction/".to_string())
            )
        );
        assert_eq!(
            chunk_store_for("brothers-karamazov-book-1"),
            ("brothers-karamazov-book-1".to_string(), None)
        );
    }

    #[tokio::test]
    async fn an_atlas_only_corpus_is_refused_by_name() {
        // The refusal that proved the SEP directories carry no passages.
        let dir = std::env::temp_dir().join("passages_atlas_only_fixture");
        std::fs::create_dir_all(dir.join("atlas")).unwrap();
        let err = PassageStore::load(&dir.parent().unwrap().to_path_buf(), "passages_atlas_only_fixture", None)
            .await
            .unwrap_err();
        assert!(err.contains("atlas-only"), "{err}");
    }
}
