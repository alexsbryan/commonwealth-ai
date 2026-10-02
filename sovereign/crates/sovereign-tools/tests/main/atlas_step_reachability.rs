// SPDX-License-Identifier: AGPL-3.0-or-later
//! Does the atlas step's BODY reach a wiki-class store? (test 3 of the three
//! in `sovereign-core/src/runtime/retrieval_pipeline/atlas_step_reachability_tests.rs`,
//! which keeps the two step-LIST tests). The subject is svrn's step over
//! `AtlasContextManager`, so the store is the leaf's checked-in wiki fixture
//! (phase-b-48), not one ingest writes here. That ingest writes the same
//! store, Entity atoms of the wiki type, and the same seed table is proven in
//! corpus-engine's tests/main/atlas_store_fixtures.rs.
//!
//! A ledger is a VALUE the step returns, so no subscriber has to be in the
//! loop for the answer to be observable: `svrn eval run` emits no
//! `sovereign_core` tracing even at `RUST_LOG=info`.

use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;

use corpus_engine_atlas_reader::context::AtlasContextProvider;
use sovereign_core::error::{Error, Result};
use sovereign_core::registry::ToolRegistry;
use sovereign_core::runtime::Runtime;
use sovereign_core::skills::SkillRegistry;
use sovereign_core::traits::InferenceProvider;
use sovereign_core::types::{
    CompletionRequest, CompletionResponse, Depth, ProviderCapabilities, Speed,
};

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
    Runtime::new(sovereign_core::runtime::RuntimeParts::new(
        Arc::new(FixedEmbed),
        Box::new(sovereign_core::stubs::PassthroughRouter),
        Box::new(sovereign_core::stubs::NoOpPlanner),
        Arc::new(ToolRegistry::new()),
        Arc::new(sovereign_store::memory::InMemoryStateStore::new()),
        Arc::new(SkillRegistry::new()),
        Arc::new(sovereign_core::executor::AutoApprovalChannel),
        sovereign_core::types::InferenceConfig::default(),
        sovereign_core::runtime::lane::LaneSources::none(),
    ))
}

/// A wiki-class atlas under `<indexes>/<corpus>/atlas`: the leaf's checked-in
/// `articles.lance` + `edges.lance` (Alpha links to Beta) and NO atom store,
/// which is wikipedia's shape. Plus a seed table borrowed from the same
/// vectors the query is embedded with — the migrated-table shape, so the walk
/// has something to seed on (a wiki store carries no atom bag, so name-match
/// seeding is not available to it). The fixture's atom ids carry the corpus
/// id it was written under, so `corpus` is always `fixtures::WIKI_STORE`.
async fn write_wiki_atlas(indexes: &Path, corpus: &str) {
    use corpus_engine_atlas_reader::ann_store::{ann_table_dir, AnnSeedTable};
    use corpus_engine_atlas_reader::fixtures;

    assert_eq!(corpus, fixtures::WIKI_STORE);
    fixtures::copy_store_fixture(fixtures::WIKI_STORE, &indexes.join(corpus)).unwrap();

    // The seed table, through the table's one builder, one row per article.
    // Beta's id is read off the store: the target of Alpha's one link.
    let store = corpus_engine_atlas_reader::opener::open_walk_provider(indexes, corpus)
        .await
        .unwrap();
    let alpha = fixtures::WIKI_ALPHA_ATOM_ID.to_string();
    let beta = store.edges_from(&alpha)[0].target.to_string();
    let rows: Vec<(String, Vec<f32>)> = [(alpha, "Alpha"), (beta, "Beta")]
        .into_iter()
        .map(|(id, title)| (id, vec_for(title)))
        .collect();
    let dir = ann_table_dir(
        &indexes
            .join(corpus)
            .join(understanding_vocab::read::ATLAS_DIRNAME),
    );
    std::fs::create_dir_all(&dir).unwrap();
    AnnSeedTable::build(&dir, &rows).await.unwrap();
}

/// The fixture's store, opened by the leaf's own reader: what the echo is
/// checked against, since it is not the subject.
async fn fixture_store(
    indexes: &Path,
    corpus: &str,
) -> Arc<dyn corpus_engine_atlas_reader::provider::AtlasProvider> {
    corpus_engine_atlas_reader::opener::open_walk_provider(indexes, corpus)
        .await
        .unwrap()
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
    fn get(&self, id: &str) -> Option<Arc<corpus_engine_atlas_reader::context::AtlasContext>> {
        self.0.get(id)
    }
    fn loaded_corpus_ids(&self) -> Vec<String> {
        self.0.loaded_corpus_ids()
    }
    fn graph(&self, id: &str) -> Option<Arc<sovereign_core::atlas_context::AtlasGraph>> {
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
    ) -> Option<Arc<dyn corpus_engine_atlas_reader::provider::AtlasProvider>> {
        self.0
            .graph(id)
            .map(|g| g as Arc<dyn corpus_engine_atlas_reader::provider::AtlasProvider>)
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
            Some(Arc::new(
                corpus_engine_atlas_reader::ports::double::AtlasPortDouble::new(),
            )),
        ),
    )
}

/// The step's two return values: the ledger it accounts with, and the
/// walk echo it carries out. Both, because a test that asserts on one and
/// not the other cannot tell "the walk reached nothing" from "the echo
/// lost it".
async fn ledger_over(
    rt: &Runtime,
    provider: Arc<dyn AtlasContextProvider>,
    corpus: &str,
) -> (
    sovereign_core::runtime::retrieval_ledger::StepLedger,
    Option<sovereign_core::runtime::AtlasWalkEcho>,
) {
    let lane = sovereign_core::runtime::Lane {
        atlas_context: Some(provider),
        ..sovereign_core::runtime::Lane::none()
    };
    let mut chunks: Vec<corpus_index::types::ScoredChunk> = Vec::new();
    // ei-7a: the walk hands its `Summary` rollups OUT here rather than
    // appending them, because rung 8 is before reweight and rerank. This
    // helper discards them — it asserts on the step's ledger, not on the
    // late append — but the sink has to exist for the walk to have
    // somewhere to put them.
    let mut summaries = Vec::new();
    let mut walk = None;
    let scope = [corpus.to_string()];
    let ledger = rt
        .apply_atlas_grounding(
            "what does alpha say about beta",
            &vec_for("what does alpha say about beta"),
            &mut chunks,
            &mut summaries,
            &mut walk,
            "test",
            None,
            Some(&scope),
            None,
            &lane,
        )
        .await;
    (ledger, walk)
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

    let (served, _) =
        ledger_over(&rt, mgr.clone() as Arc<dyn AtlasContextProvider>, "wikish").await;
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
    let (refused, _) = ledger_over(
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

/// THE ECHO CARRIES THE PATH, NOT JUST THE COUNTS.
///
/// `considered > 0` (the test above) says the walk produced candidates. It
/// says nothing about WHICH atoms it reached, and a study of ontology reach
/// is a join on atom ids — a walk whose echo carries an empty `nodes` is
/// indistinguishable from no walk at all on the only surface that can
/// measure it (`svrn eval run` emits no `sovereign_core` tracing, so the
/// `atlas-grounding: fetch ledger` event is dark there).
///
/// The fixture's fetch finds nothing — no corpus index behind it — so
/// `added` is 0 here on purpose. The path under test is upstream of the
/// fetch, which is exactly why it has to be its own assertion: build the
/// echo with `nodes: vec![]` and every count in it still reads correct.
#[tokio::test]
async fn atlas_walk_echo_carries_atom_ids_and_subtypes() {
    let tmp = tempfile::tempdir().unwrap();
    write_wiki_atlas(tmp.path(), "wikish").await;
    let mgr = manager(tmp.path());
    mgr.init_from_cache().await;
    let rt = runtime();

    let (served, walk) =
        ledger_over(&rt, mgr.clone() as Arc<dyn AtlasContextProvider>, "wikish").await;
    let considered = served.considered.expect("an injector reports `considered`");
    assert!(
        considered > 0,
        "the walk must have run for the echo to be worth asserting on; \
         considered {considered}"
    );

    let walk = walk.expect(
        "a walk that ran must carry an echo. `None` is reserved for the \
         walk that did NOT run — feature off, no provider, no embedding, \
         or no graph layer — and collapsing the two makes 'reached \
         nothing' unreadable.",
    );
    assert!(
        !walk.nodes.is_empty(),
        "the echo must carry the evidence PATH, not only its counts: a \
         walk that considered {considered} candidates reached the atoms \
         that produced them. counts were seeds={} edges={} reached={} \
         requests={}",
        walk.seeds,
        walk.edges_followed,
        walk.nodes_reached,
        walk.requests
    );
    // Against the FIXTURE's own atoms, not against the echo's self-report
    // (ARCH §5: assert on something the subject cannot author). `atom_id`
    // merely being non-empty is satisfied by any literal the echo builder
    // chooses — which is the shape that let the wrong-slot guard pass on
    // an SSE `model` field the client had supplied. These ids, titles and
    // types come out of the store `write_wiki_atlas` copied, read by the
    // leaf's own opener.
    let store = fixture_store(tmp.path(), "wikish").await;
    for n in &walk.nodes {
        let atom = store.atom(&n.atom_id).unwrap_or_else(|| {
            panic!(
                "the echoed atom id must be one the fixture's store holds — it \
                 is the join key a reach study is made of, and an id the echo \
                 authored joins to nothing. node {n:?}"
            )
        });
        assert_eq!(
            atom.name(),
            n.name,
            "the echoed name must be the stored atom's. node {n:?}"
        );
        assert!(
            ["Alpha", "Beta"].contains(&n.name.as_str()),
            "the fixture wrote exactly two articles; a walk reporting any \
             other name did not read that store. node {n:?}"
        );
        assert_eq!(
            n.kind, "entity",
            "the wiki store builds Entity atoms, so the echoed kind is \
             `AtomType::Entity.label()`. node {n:?}"
        );
        assert_eq!(
            n.subtype,
            atom.subtype(),
            "the echoed subtype must be the fixture's declared entity type \
             — the field a declared-ontology study groups by. node {n:?}"
        );
    }
}

/// A DEEPQUERY TURN THAT WAS WALKED SAYS SO.
///
/// `deep_pipeline` has carried `atlas_grounding` all along (test 1 above),
/// and until 2026-09-21 `prepare_knowledge_context` destructured the
/// pipeline state with `..` and let the echo fall through it. Every
/// DeepQuery and SimpleQuery message therefore persisted no `atlas_walk`
/// key, and the ei7 pod pilot read four walked whole-story turns as
/// unwalked (`research/ontology-retrieval/pod/20260921T035355Z/`).
///
/// Same fixture and same assertion target as the echo test above: the
/// names come out of the store `write_wiki_atlas` wrote, not out of the
/// echo's own report.
#[tokio::test]
async fn a_walked_deep_query_turn_carries_the_echo_out_of_retrieval() {
    let tmp = tempfile::tempdir().unwrap();
    write_wiki_atlas(tmp.path(), "wikish").await;
    let mgr = manager(tmp.path());
    mgr.init_from_cache().await;
    let rt = Runtime::new(sovereign_core::runtime::RuntimeParts::new(
        Arc::new(FixedEmbed),
        Box::new(sovereign_core::stubs::PassthroughRouter),
        Box::new(sovereign_core::stubs::NoOpPlanner),
        Arc::new(ToolRegistry::new()),
        Arc::new(sovereign_store::memory::InMemoryStateStore::new()),
        Arc::new(SkillRegistry::new()),
        Arc::new(sovereign_core::executor::AutoApprovalChannel),
        sovereign_core::types::InferenceConfig::default(),
        sovereign_core::runtime::lane::LaneSources {
            atlas_context: Some(mgr.clone() as Arc<dyn AtlasContextProvider>),
            ..sovereign_core::runtime::lane::LaneSources::none()
        },
    ));
    let context = sovereign_core::types::ConversationContext {
        conversation: sovereign_core::types::Conversation {
            id: "conv-deep-echo".to_string(),
            title: None,
            messages: Vec::new(),
            created_at: 0,
            updated_at: 0,
            version: 0,
            deleted_at: None,
            skill_id: None,
            enabled_corpora: Some(vec!["wikish".to_string()]),
            searched_sources: None,
        },
        memories: Vec::new(),
        working_memory: None,
        installed_corpora: vec![],
        corpus_ceiling: None,
        document_session: None,
        topic_context: None,
        knowledge_view_digests: None,
        temporal_tensions: Vec::new(),
        compacted_history: None,
        history_retrieval_hits: None,
        tool_dossier: None,
        intent_policy: None,
    };

    let kc = rt
        .prepare_knowledge_context(
            "what does alpha say about beta",
            &context,
            &sovereign_core::types::Intent::DeepQuery,
            None,
        )
        .await;

    let walk = kc.atlas_walk.expect(
        "the deep pipeline ran `atlas_grounding` over a store it can walk; \
         `None` here is the echo being dropped between the pipeline state \
         and the `KnowledgeContext`, not a walk that did not run",
    );
    assert!(
        !walk.nodes.is_empty()
            && walk
                .nodes
                .iter()
                .all(|n| ["Alpha", "Beta"].contains(&n.name.as_str())),
        "the carried echo must be the walk over the fixture's two articles. {walk:?}"
    );
}
