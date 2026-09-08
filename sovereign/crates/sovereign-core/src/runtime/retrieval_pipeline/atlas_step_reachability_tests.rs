// SPDX-License-Identifier: AGPL-3.0-or-later
//! Does the atlas step reach a wiki-class store?
//!
//! Three tests, answering one question in the order its links can be checked.
//! They live together because a green on any one of them alone reads as an
//! answer and is not:
//!
//! 1. Is `atlas_grounding` in the pipelines that search a corpus? (the step
//!    LIST — no Runtime needed)
//! 2. Is the one pipeline without it the one without a corpus search? (the
//!    only legitimate omission, pinned so it stays the only one)
//! 3. Does the step's BODY reach a wiki-class store, and does it refuse when
//!    the provider cannot serve one? (the returned [`StepLedger`])
//!
//! (3) exists because the channel that would have shown it was dark: `svrn
//! eval run` emits no `sovereign_core` tracing even at `RUST_LOG=info`, and
//! reading that silence as "the code never runs" was wrong. A ledger is a
//! VALUE the step returns, so no subscriber has to be in the loop for the
//! answer to be observable.

use super::*;

/// THE ATLAS STEP IS IN EVERY PIPELINE THAT SEARCHES A CORPUS.
///
/// Written while chasing why a `--prod-pipeline` wikipedia eval showed no
/// atlas contribution. The hypothesis on the table was that a pipeline-level
/// predicate skips the step for a wiki-class corpus before
/// `apply_atlas_grounding` is ever called. This pins the half of that
/// question that can be answered without a Runtime: whether the step is in
/// the list at all. It is, for both pipelines `retrieve_evidence`
/// dispatches to (`kq_pipeline` for KnowledgeQuery / ComparisonQuery,
/// `deep_pipeline` for DeepQuery), and it is in `shared_core_steps` so
/// neither can drop it by drifting apart.
///
/// What this test does NOT establish, said plainly so nobody reads more
/// into a green: that the step's body runs, or that it reaches a store.
/// Those need the step's returned `StepLedger`, not its name — see
/// [`the_atlas_step_reaches_a_wiki_class_store`] below.
#[test]
fn atlas_grounding_is_in_every_corpus_searching_pipeline() {
    for (name, steps) in [
        ("kq", kq_pipeline().step_names()),
        ("deep", deep_pipeline(true).step_names()),
    ] {
        assert!(
            steps.contains(&"atlas_grounding"),
            "{name}: atlas_grounding must be in the pipeline — a corpus \
             search that cannot reach the atlas grounds nothing, and the \
             absence is invisible from outside"
        );
    }
}

/// The ONE legitimate omission, pinned so it stays the only one.
///
/// `deep_pipeline(false)` is the attached-doc turn: no corpus search, so no
/// pool to seed from and no query embedding computed. Dropping the step
/// there is deliberate (see the doc comment on `deep_pipeline`). Pinning it
/// means a future change that drops `atlas_grounding` from a SEARCHING
/// pipeline cannot hide behind "it was already conditional".
#[test]
fn the_only_pipeline_without_atlas_grounding_is_the_one_without_corpus_search() {
    let without = deep_pipeline(false).step_names();
    assert!(
        !without.contains(&"atlas_grounding"),
        "attached-doc turns intentionally drop atlas grounding"
    );
    // And it is dropped for the stated reason — the corpus head is gone
    // too, not just the grounding step. `raptor_grounding_early` was the
    // sibling this checked against until ei-5c retired it; `store_search` is
    // the head itself, and a step whose absence proves the same condition.
    assert!(
        !without.contains(&"store_search"),
        "the same no-corpus-search condition drops the corpus head; \
         if these two ever diverge the reason given here is stale"
    );
}

// ── (3) the step's body, over a real wiki-class store ───────────────────────

mod ledger {
    use std::path::Path;
    use std::pin::Pin;
    use std::sync::Arc;

    use async_trait::async_trait;
    use futures::Stream;

    use crate::error::{Error, Result};
    use crate::registry::ToolRegistry;
    use crate::runtime::Runtime;
    use crate::skills::SkillRegistry;
    use crate::traits::InferenceProvider;
    use crate::types::{CompletionRequest, CompletionResponse, Depth, ProviderCapabilities, Speed};
    use corpus_engine::enrichment::atlas::context::AtlasContextProvider;

    /// Four positive dims, so every pair clears the walk's cosine floor. The
    /// walk's ROUTE is what is under test, not its ranking.
    const DIM: usize = 4;

    fn vec_for(text: &str) -> Vec<f32> {
        let mut v = vec![1.0_f32; DIM];
        for (i, b) in text.bytes().enumerate() {
            v[i % DIM] += (b % 7) as f32 * 0.01;
        }
        v
    }

    /// Embeds deterministically and refuses to complete. The walk needs an
    /// embedder (for the navigation classifier's centroids) and nothing else.
    struct FixedEmbed;

    #[async_trait]
    impl InferenceProvider for FixedEmbed {
        async fn complete(&self, _r: &CompletionRequest) -> Result<CompletionResponse> {
            Err(Error::NotImplemented("FixedEmbed: complete unused".into()))
        }
        async fn complete_stream(
            &self,
            _r: &CompletionRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<String>> + Send>>> {
            Err(Error::NotImplemented("FixedEmbed: stream unused".into()))
        }
        async fn embed(&self, text: &str) -> Result<Vec<f32>> {
            Ok(vec_for(text))
        }
        async fn embed_query(&self, q: &str) -> Result<Vec<f32>> {
            Ok(vec_for(q))
        }
        async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|t| vec_for(t)).collect())
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                max_context_tokens: 4096,
                supports_structured_output: false,
                relative_speed: Speed::Fast,
                relative_reasoning: Depth::Shallow,
            }
        }
    }

    fn runtime() -> Runtime {
        Runtime::new(crate::runtime::RuntimeParts::new(
            Arc::new(FixedEmbed),
            Box::new(crate::stubs::PassthroughRouter),
            Box::new(crate::stubs::NoOpPlanner),
            Arc::new(ToolRegistry::new()),
            Arc::new(sovereign_store::memory::InMemoryStateStore::new()),
            Arc::new(SkillRegistry::new()),
            Arc::new(crate::executor::AutoApprovalChannel),
            crate::types::InferenceConfig::default(),
            crate::runtime::lane::LaneSources::none(),
        ))
    }

    /// A wiki-class atlas under `<indexes>/<corpus>/atlas`: `articles.lance` +
    /// `edges.lance` and NO atom store, which is wikipedia's shape. Two
    /// articles, one link, and a seed table borrowed from the same vectors the
    /// query is embedded with — the migrated-table shape, so the walk has
    /// something to seed on (a wiki store carries no atom bag, so name-match
    /// seeding is not available to it).
    async fn write_wiki_atlas(indexes: &Path, corpus: &str) {
        use corpus_engine::enrichment::atlas::context::{
            build_persistent_ann_seed_table, AtlasContext, AtlasEntry,
        };
        use corpus_engine::enrichment::atlas::wiki_store::{
            build_wikipedia_columnar_store_from_chunks, wiki_atom_id,
        };
        use corpus_engine::extractors::wikipedia_types::{WikiLink, WikipediaChunkMetadata};
        use corpus_engine::index::StoredChunkWithMetadata;

        let atlas = indexes
            .join(corpus)
            .join(corpus_engine::enrichment::atlas::writer::ATLAS_DIRNAME);
        std::fs::create_dir_all(&atlas).unwrap();
        let meta = |links: Vec<(&str, &str)>| {
            serde_json::to_string(&WikipediaChunkMetadata {
                section_name: "Lead".into(),
                section_path: vec!["Lead".into()],
                section_depth: 0,
                section_type: "lead".into(),
                citation_needed_count: None,
                pov_count: None,
                clarification_needed_count: None,
                update_count: None,
                is_flagged_stable: None,
                outgoing_links: links
                    .into_iter()
                    .map(|(t, l)| WikiLink {
                        target_title: t.into(),
                        link_text: l.into(),
                    })
                    .collect(),
                revision_id: Some(1),
                wikidata_qid: None,
                page_id: None,
            })
            .unwrap()
        };
        let ch = |id: u64, title: &str, m: String| StoredChunkWithMetadata {
            id,
            title: Some(title.into()),
            url: None,
            metadata_raw: Some(m),
        };
        build_wikipedia_columnar_store_from_chunks(
            &atlas,
            corpus,
            vec![
                ch(1, "Alpha", meta(vec![("Beta", "beta")])),
                ch(2, "Beta", meta(vec![])),
            ],
        )
        .await
        .unwrap();

        // The seed table, through the ONE writer (ARCH §10.6) — no new arm on
        // `AtlasSeeding`, and the entries carry BORROWED vectors, which is the
        // shape the wikipedia migration writes.
        let entries: Vec<AtlasEntry> = ["Alpha", "Beta"]
            .into_iter()
            .map(|t| AtlasEntry {
                atom_id: wiki_atom_id(t, corpus),
                canonical_name: t.to_string(),
                embed_text: t.to_string(),
                embedding: vec_for(t),
            })
            .collect();
        build_persistent_ann_seed_table(
            &atlas,
            &AtlasContext {
                atlas_corpus_id: corpus.to_string(),
                entries,
                top_k: 12,
            },
        )
        .await
        .unwrap();
    }

    /// An `AtlasContextProvider` that can serve ONLY atom-class stores —
    /// exactly what every provider was before `walk_provider` existed, since
    /// the trait's default delegates to `graph()`.
    ///
    /// This is the failing input for the test below, kept as a type rather
    /// than described in prose: over the SAME wiki fixture it takes the
    /// bag-of-atoms branch, and the bag for a wiki corpus is empty.
    struct AtomClassOnly(Arc<sovereign_tools::atlas_context_manager::AtlasContextManager>);

    #[async_trait]
    impl AtlasContextProvider for AtomClassOnly {
        fn get(
            &self,
            id: &str,
        ) -> Option<Arc<corpus_engine::enrichment::atlas::context::AtlasContext>> {
            self.0.get(id)
        }
        fn loaded_corpus_ids(&self) -> Vec<String> {
            self.0.loaded_corpus_ids()
        }
        fn graph(&self, id: &str) -> Option<Arc<crate::atlas_context::AtlasGraph>> {
            self.0.graph(id)
        }
        /// The atom-class answer, and only it: `graph()` widened to the walk's
        /// trait. This is what every provider did before the wiki store
        /// existed, and since `walk_provider` stopped having a default
        /// (a class dispatch is a decision, not an omission) it has to be
        /// written out — which is better, because the failing input is now a
        /// body a reader can compare against the manager's.
        fn walk_provider(
            &self,
            id: &str,
        ) -> Option<Arc<dyn corpus_engine::enrichment::atlas::AtlasProvider>> {
            self.0
                .graph(id)
                .map(|g| g as Arc<dyn corpus_engine::enrichment::atlas::AtlasProvider>)
        }
        async fn ensure_loaded(&self, ids: &[String]) {
            self.0.ensure_loaded(ids).await;
        }
    }

    fn manager(indexes: &Path) -> Arc<sovereign_tools::atlas_context_manager::AtlasContextManager> {
        Arc::new(
            sovereign_tools::atlas_context_manager::AtlasContextManager::new(
                indexes.to_path_buf(),
                Arc::new(FixedEmbed),
                "test-embed".into(),
            ),
        )
    }

    async fn ledger_over(
        rt: &Runtime,
        provider: Arc<dyn AtlasContextProvider>,
        corpus: &str,
    ) -> crate::runtime::retrieval_ledger::StepLedger {
        let lane = crate::runtime::Lane {
            atlas_context: Some(provider),
            ..crate::runtime::Lane::none()
        };
        let mut chunks: Vec<corpus_engine::ScoredChunk> = Vec::new();
        // ei-7a: the walk hands its `Summary` rollups OUT here rather than
        // appending them, because rung 8 is before reweight and rerank. This
        // helper discards them — it asserts on the step's ledger, not on the
        // late append — but the sink has to exist for the walk to have
        // somewhere to put them.
        let mut summaries = Vec::new();
        let scope = [corpus.to_string()];
        rt.apply_atlas_grounding(
            "what does alpha say about beta",
            &vec_for("what does alpha say about beta"),
            &mut chunks,
            &mut summaries,
            "test",
            None,
            Some(&scope),
            None,
            &lane,
        )
        .await
    }

    /// THE STEP'S BODY REACHES A WIKI-CLASS STORE — and says so in a value.
    ///
    /// `apply_atlas_grounding` returns a `StepLedger`, so this asserts on what
    /// the step ACCOUNTED FOR rather than on a log line: `considered > 0` is
    /// the walk having produced evidence requests from a store with no atom
    /// bag, which only the graph branch can do (the bag branch's `considered`
    /// IS its injected count, and a wiki corpus has no bag to inject from).
    ///
    /// The fetch itself finds nothing — there is no corpus index behind the
    /// fixture — so every candidate lands in `accounted`. That is the point:
    /// the accounting identity holds either way, and the number under test is
    /// the one upstream of the fetch.
    #[tokio::test]
    async fn the_atlas_step_reaches_a_wiki_class_store() {
        let tmp = tempfile::tempdir().unwrap();
        write_wiki_atlas(tmp.path(), "wikish").await;
        let mgr = manager(tmp.path());
        mgr.init_from_cache().await;
        let rt = runtime();

        let served = ledger_over(&rt, mgr.clone() as Arc<dyn AtlasContextProvider>, "wikish").await;
        let considered = served.considered.expect("an injector reports `considered`");
        assert!(
            considered > 0,
            "the atlas step must reach the wiki-class store and generate \
             candidates; it considered {considered}. A zero here means the \
             walk never ran — the store was not resolved, or it seeded on \
             nothing."
        );
        assert_eq!(
            considered,
            served.total_accounted(),
            "an injector's identity: every candidate is either added or \
             accounted for by reason. Added is 0 here (no corpus index behind \
             the fixture), so all of them must be accounted: {:?}",
            served.accounted
        );

        // The failing input, run: the SAME fixture through a provider that can
        // only serve atom-class stores refuses, and refuses to zero.
        let refused = ledger_over(
            &rt,
            Arc::new(AtomClassOnly(mgr)) as Arc<dyn AtlasContextProvider>,
            "wikish",
        )
        .await;
        assert_eq!(
            refused.considered,
            Some(0),
            "an atom-class-only provider has no store for a wiki corpus, so \
             the step falls to bag-of-atoms over an empty bag. If this is \
             non-zero the test above is no longer discriminating."
        );
    }
}
