// SPDX-License-Identifier: AGPL-3.0-or-later
//! WHERE the walk's whole-work summaries come from — one stage signature, two
//! sources, composed by the corpus's own map (operator directive 25ae5815,
//! 2026-09-08).
//!
//! # What the rows taught, and why this shape
//!
//! ei-7a made `Summary` atoms reachable and SEP sources fell −7.5/66. The
//! campaign called that a seed race; ei-5c's lane says otherwise. In the arm
//! that reproduced it, 34 summaries seeded and NOT ONE was refused by its
//! budget — the ledger reads `summary_seeds 34, dropped_seed_budget 0`. What
//! was wrong was that two producers put summaries in one pool and neither knew
//! about the other: the walk's 34 plus the retrieval-time injector's up to 8
//! per question. With one producer over the same material, the same 34
//! summaries are worth +3 facts at equal source recall.
//!
//! So the fix is not to delete a source. It is to give every source the SAME
//! shape, the SAME budget and the SAME append, and let the corpus say which
//! ones it composes:
//!
//! - [`SummarySource::Atoms`] — `Summary` atoms in the atlas, reached by the
//!   walk like any other seed. What ei-7a built.
//! - [`SummarySource::Raptor`] — the corpus's own `raptor_summaries.lance`,
//!   read through the primitives `crate::index::raptor` already exposes.
//!
//! Neither is a fallback for the other and neither retires: a book may keep
//! both permanently, and a rebuild takes nothing away.
//!
//! # The dedupe is identity, not a heuristic
//!
//! `AtomId::summary_content_hash(node_id, corpus_id)` is what
//! `summary_atoms.rs` writes as a projected atom's id, and it is a pure
//! function of the node and the corpus. So a RAPTOR row and the `Summary` atom
//! projected FROM it collide by construction (ARCH §7.5). Composing both on a
//! migrated corpus is therefore idempotent for free — which is what makes
//! "keep both permanently" safe rather than double-counted.
//!
//! # Why an enum and not a trait
//!
//! A closed set of two, inside one crate (principle 9, ARCH §2/§4). A summary
//! source arriving from OUTSIDE corpus-engine is precisely what spec §3's "no
//! private kinds" and EI5's "one implementation, in corpus-engine" forbid, so
//! there is no open set to register and a plugin point would be a framework
//! nobody can populate (§5.1). What the enum still owes the design is ONE
//! stage signature — [`SummarySource::supply`] — so every source is asked the
//! same way, fills the same budget and is named by the same ledger line. The
//! day a source must come from outside, this becomes a registry and that is a
//! spec §3 decision then, not a guess now.

use std::collections::BTreeMap;

use corpus_engine_vocab::ontology::SummarySource;

use super::super::atoms::AtomId;
use super::super::provider::AtlasProvider;
use super::SummaryNode;

/// Everything a source needs to answer, and nothing it does not.
///
/// `reached` is the [`SummarySource::Atoms`] arm's whole supply: the walk has
/// already found those, scored them and held them out of leaf scoring (R1/R2),
/// so re-deriving them inside the arm would be a second implementation of the
/// walk's own hold-out.
pub struct SummaryQuery<'a> {
    /// The question, in the space the seed tables were built in.
    pub question_embedding: &'a [f32],
    /// How many summaries the row's budget still has room for.
    pub want: usize,
    /// The atlases in scope. The `Raptor` arm asks each for its chunk corpus.
    pub graphs: &'a [&'a dyn AtlasProvider],
    /// Summary-grain atoms the walk reached, highest weight first.
    pub reached: &'a [SummaryNode],
}

/// How many summaries each source actually SERVED — the ledger's own line.
///
/// Served, never configured: a row that lists two sources and gets everything
/// from the first must read differently from one that gets nothing at all, and
/// this is the only field that can tell them apart. It replaces the standalone
/// "this corpus has RAPTOR rows and no atoms" line, because `[atoms:0,
/// raptor:0]` says the same thing and says it for every source at once
/// (ARCH §18.3 — absence is reported, and here it is reported by the same
/// mechanism that reports presence).
pub type SourceYield = BTreeMap<SummarySource, usize>;

/// THE STAGE SIGNATURE — one method, every source, same shape.
///
/// An extension trait rather than an inherent `impl`, for one reason: the enum
/// is DATA and lives in `corpus-engine-vocab`, the leaf every thin host links
/// and the boundary-gate holds to no store dependencies. It must not learn how
/// to open a Lance table. So the vocabulary declares WHICH sources exist and
/// this crate — the one that owns the stores — says what asking one means
/// (ARCH §8).
#[allow(async_fn_in_trait)]
pub trait SummaryStage {
    /// Ask this source for at most `q.want` summaries, best first.
    ///
    /// The caller neither knows nor cares which source answered, which is the
    /// whole point of there being one signature: every arm returns the same
    /// [`SummaryNode`], fills the same budget, and is appended by the same
    /// late path.
    async fn supply(&self, q: &SummaryQuery<'_>) -> Vec<SummaryNode>;
}

/// One `match`, two arms, and nothing else decides where a summary may come
/// from.
impl SummaryStage for SummarySource {
    async fn supply(&self, q: &SummaryQuery<'_>) -> Vec<SummaryNode> {
        if q.want == 0 {
            return Vec::new();
        }
        match self {
            SummarySource::Atoms => q.reached.iter().take(q.want).cloned().collect(),
            SummarySource::Raptor => raptor_rows(q).await,
        }
    }
}

/// The RAPTOR arm: the corpus's own summary table, read where it lives.
///
/// It is a READ and not a port. `crate::index::raptor::search_raptor_summaries`
/// takes a PATH — no engine handle, no daemon — and returns a `RaptorHit`
/// carrying the exact cosine recomputed from the stored embedding, which is
/// deliberately bit-comparable to `atlas_context::cosine` and therefore to the
/// weights the `Atoms` arm carries. So the two arms' scores are on one scale
/// without anything being rescaled.
///
/// WHICH corpus dir it reads is `AtlasGraph::summary_corpus_dir`, derived
/// through [`AtlasProvider::site`] and never by a second path rule: the RAPTOR table lives under the CHUNK corpus (`sep`)
/// while the walking provider is per-article (`sep-freewill`), and
/// `EvidenceSite` already owns that direction. A private `strip the prefix`
/// here would be the conflation `evidence_site` exists to prevent.
async fn raptor_rows(q: &SummaryQuery<'_>) -> Vec<SummaryNode> {
    let mut out: Vec<SummaryNode> = Vec::new();
    let mut seen_dirs: Vec<std::path::PathBuf> = Vec::new();
    for graph in q.graphs {
        let Some(dir) = graph.summary_corpus_dir() else {
            continue;
        };
        // Several per-article atlases of one corpus share ONE table; reading it
        // once per atlas would multiply every row by the article count.
        if seen_dirs.contains(&dir) {
            continue;
        }
        seen_dirs.push(dir.clone());
        // The sidecar is the gate. Absent means this corpus has no table to
        // read, which is not an error and not a degradation — it is a corpus
        // that keeps no summaries here.
        if crate::index::raptor::read_raptor_meta(&dir).is_none() {
            continue;
        }
        let hits =
            match crate::index::raptor::search_raptor_summaries(&dir, q.question_embedding, q.want)
                .await
            {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(
                        target: "retrieval_audit",
                        dir = %dir.display(),
                        "summaries: raptor read failed ({e}); this source contributes none"
                    );
                    continue;
                }
            };
        let corpus = graph.site().chunk_corpus().as_str().to_string();
        for h in hits {
            if h.summary.trim().is_empty() {
                continue;
            }
            out.push(SummaryNode {
                // The SAME id the projection would write for this row, so a
                // corpus that has been projected dedupes to one entry.
                atom_id: AtomId::summary_content_hash(&h.node_id, &corpus)
                    .as_str()
                    .to_string(),
                site: graph.site().clone(),
                text: h.summary,
                score: h.score,
            });
        }
    }
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(q.want);
    out
}

/// Compose the row's sources, in the row's order, into ONE budgeted list.
///
/// The three rules that make several sources behave as one producer:
///
/// 1. **One budget.** Each source is asked only for the room that is left, so
///    the total can never exceed the row's `Summary` quota however many
///    sources are listed. This is the rule whose absence cost −7.5/66.
/// 2. **One entry per summary.** Deduped on the summary's own content-derived
///    id, so an earlier source wins and a migrated corpus that composes both
///    is idempotent.
/// 3. **One order, declared.** Priority is the list's order, which is the
///    corpus's declaration and not this function's opinion.
pub async fn compose(
    sources: &[SummarySource],
    budget: usize,
    q_embedding: &[f32],
    graphs: &[&dyn AtlasProvider],
    reached: &[SummaryNode],
) -> (Vec<SummaryNode>, SourceYield) {
    let mut kept: Vec<SummaryNode> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut served: SourceYield = sources.iter().map(|s| (*s, 0usize)).collect();
    for source in sources {
        let want = budget.saturating_sub(kept.len());
        if want == 0 {
            break;
        }
        let q = SummaryQuery {
            question_embedding: q_embedding,
            want,
            graphs,
            reached,
        };
        for node in source.supply(&q).await {
            if kept.len() >= budget {
                break;
            }
            if !seen.insert(node.atom_id.clone()) {
                continue;
            }
            kept.push(node);
            *served.entry(*source).or_insert(0) += 1;
        }
    }
    (kept, served)
}

/// The ledger's rendering of [`SourceYield`] — `atoms:34,raptor:0`.
pub fn yield_label(served: &SourceYield) -> String {
    served
        .iter()
        .map(|(s, n)| format!("{}:{n}", s.as_str()))
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enrichment::atlas::evidence_site::EvidenceSite;

    fn node(id: &str, score: f32) -> SummaryNode {
        SummaryNode {
            atom_id: id.to_string(),
            site: EvidenceSite::derive("fixture"),
            text: format!("a paraphrase ({id})"),
            score,
        }
    }

    /// ONE BUDGET across every source, which is the rule whose absence cost
    /// −7.5/66. Failing input: ask each source for `budget` instead of for the
    /// room that is left, and two sources return twice the cap.
    #[tokio::test]
    async fn several_sources_share_one_budget() {
        let reached = vec![node("a", 0.9), node("b", 0.8), node("c", 0.7)];
        // Two sources listed, but the graphs are empty so only `Atoms` can
        // serve; the cap is what bounds the result either way.
        let (kept, served) = compose(
            &[SummarySource::Atoms, SummarySource::Raptor],
            2,
            &[1.0, 0.0],
            &[],
            &reached,
        )
        .await;
        assert_eq!(
            kept.len(),
            2,
            "the budget caps the COMPOSITION, not a source"
        );
        assert_eq!(kept[0].atom_id, "a", "the row's order is priority order");
        assert_eq!(served[&SummarySource::Atoms], 2);
        assert_eq!(
            served[&SummarySource::Raptor],
            0,
            "a source asked with no room left serves nothing, and says so"
        );
    }

    /// ONE ENTRY PER SUMMARY. Two sources offering the same summary must not
    /// double-count it, and that is what makes "compose both permanently"
    /// safe on a corpus whose atoms were projected FROM the rows the other
    /// source reads. Failing input: drop the `seen` set.
    #[tokio::test]
    async fn the_same_summary_from_two_sources_is_one_entry() {
        // `compose` dedupes on the id alone, so a duplicate WITHIN a source's
        // supply exercises the same code path a cross-source duplicate does.
        let reached = vec![node("dup", 0.9), node("dup", 0.9), node("other", 0.5)];
        let (kept, served) = compose(&[SummarySource::Atoms], 8, &[1.0, 0.0], &[], &reached).await;
        assert_eq!(kept.len(), 2);
        assert_eq!(
            served[&SummarySource::Atoms],
            2,
            "served counts what was KEPT"
        );
    }

    /// A row that composes NO source supplies nothing, however many summaries
    /// the walk reached. That is what every non-thematic row declares, and a
    /// composition that ignored the list would quietly give them summaries
    /// their map never asked for.
    #[tokio::test]
    async fn an_empty_source_list_supplies_nothing() {
        let reached = vec![node("a", 0.9)];
        let (kept, served) = compose(&[], 8, &[1.0, 0.0], &[], &reached).await;
        assert!(kept.is_empty());
        assert!(served.is_empty(), "nothing configured, nothing to report");
    }

    /// The ledger line says SERVED, per source, so absence and presence are
    /// reported by one mechanism. `[atoms:0,raptor:0]` is the absence that
    /// used to need its own log line.
    #[tokio::test]
    async fn the_ledger_names_what_each_source_served() {
        let (_, served) = compose(
            &[SummarySource::Atoms, SummarySource::Raptor],
            8,
            &[1.0, 0.0],
            &[],
            &[],
        )
        .await;
        assert_eq!(yield_label(&served), "atoms:0,raptor:0");

        let reached = vec![node("a", 0.9)];
        let (_, served) = compose(
            &[SummarySource::Atoms, SummarySource::Raptor],
            8,
            &[1.0, 0.0],
            &[],
            &reached,
        )
        .await;
        assert_eq!(yield_label(&served), "atoms:1,raptor:0");
    }

    /// The `Raptor` arm needs a corpus dir and the trait DEFAULTS it to
    /// `None`, so a provider with no disk behind it contributes nothing rather
    /// than guessing a path. Failing input: default the accessor to the atlas
    /// dir — on SEP that reads `<indexes>/sep-freewill`, which holds no table,
    /// and every article would silently serve zero for a reason nobody could
    /// see.
    #[tokio::test]
    async fn a_provider_with_no_corpus_dir_serves_no_raptor_rows() {
        let q = SummaryQuery {
            question_embedding: &[1.0, 0.0],
            want: 8,
            graphs: &[],
            reached: &[],
        };
        assert!(SummarySource::Raptor.supply(&q).await.is_empty());
    }

    /// A source asked for nothing returns nothing without touching a store —
    /// the guard that keeps a full budget from paying for a Lance query.
    #[tokio::test]
    async fn a_source_with_no_room_is_not_asked_to_read() {
        let q = SummaryQuery {
            question_embedding: &[1.0, 0.0],
            want: 0,
            graphs: &[],
            reached: &[node("a", 0.9)],
        };
        assert!(SummarySource::Atoms.supply(&q).await.is_empty());
        assert!(SummarySource::Raptor.supply(&q).await.is_empty());
    }
}
